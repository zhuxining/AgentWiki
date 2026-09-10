"""Executable checks for the dependency direction documented in AGENTS.md.

The layering is:

    cli / mcp (composition roots)
        -> runtime
        -> services
        -> domain / ports
        <- repository / indexing / markdown (adapters)

AGENTS.md states this rule but nothing enforced it, so a violation could only be
caught by review. These tests turn the rule into a build failure.
"""

import ast
from pathlib import Path
import sys
import unittest

PACKAGE_ROOT = Path(__file__).resolve().parents[2] / "src" / "agentwiki"

# Every module that defines a layer barrier maps to the layer it belongs to.
_LAYER_PREFIXES: tuple[tuple[str, str], ...] = (
    ("agentwiki.data", "package"),
    ("agentwiki.domain", "domain"),
    ("agentwiki.services.ports", "ports"),
    ("agentwiki.services", "services"),
    ("agentwiki.repository", "repository"),
    ("agentwiki.indexing", "indexing"),
    ("agentwiki.markdown", "markdown"),
    ("agentwiki.runtime", "runtime"),
    ("agentwiki.cli", "composition"),
    ("agentwiki.mcp", "composition"),
    ("agentwiki.config", "config"),
    # The package root re-exports the CLI entry point, so it belongs with composition.
    ("agentwiki", "composition"),
)

# Layers a given layer may import from. Anything unlisted is a violation; stdlib and
# third-party modules are always allowed.
# The adapters (repository / indexing / markdown) are peers that cooperate during a
# sync, so each may import the others; only the inward layers are off limits.
_ADAPTERS = frozenset({"domain", "ports", "repository", "indexing", "markdown"})

_ALLOWED: dict[str, frozenset[str]] = {
    "package": frozenset({"package"}),
    "config": frozenset({"domain", "package", "config"}),
    "domain": frozenset({"domain", "package"}),
    "ports": frozenset({"domain", "package", "ports"}),
    "services": frozenset({"domain", "package", "ports", "services"}),
    "repository": _ADAPTERS | {"package", "repository", "config"},
    "indexing": _ADAPTERS | {"package", "indexing", "config"},
    "markdown": _ADAPTERS | {"package", "markdown", "config"},
    "runtime": _ADAPTERS | {"package", "services", "runtime", "config"},
    "composition": _ADAPTERS | {"package", "services", "runtime", "composition", "config"},
}

# Only composition roots may read global configuration.
_CONFIG_ALLOWED = frozenset({"agentwiki.cli", "agentwiki.mcp", "agentwiki.runtime.context"})


def _layer_of(module: str) -> str | None:
    # Exact matches win, so the package root can differ from its submodules.
    for prefix, layer in _LAYER_PREFIXES:
        if module == prefix:
            return layer
    for prefix, layer in _LAYER_PREFIXES:
        if module.startswith(f"{prefix}."):
            return layer
    return None


def _module_name(path: Path) -> str:
    relative = path.relative_to(PACKAGE_ROOT.parent).with_suffix("")
    parts = list(relative.parts)
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def _imported_modules(tree: ast.AST) -> set[str]:
    """Return the absolute module names imported by one parsed file."""
    modules: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            modules.add(node.module)
    return modules


def _agentwiki_files() -> list[Path]:
    return sorted(PACKAGE_ROOT.rglob("*.py"))


def _violations(module: str, imported: set[str]) -> list[str]:
    layer = _layer_of(module)
    if layer is None:
        return []
    allowed = _ALLOWED[layer]
    found: list[str] = []
    for target in sorted(imported):
        target_layer = _layer_of(target)
        if target_layer is not None and target_layer not in allowed:
            found.append(f"{module} ({layer}) must not import {target} ({target_layer})")
        if target == "agentwiki.config" and module not in _CONFIG_ALLOWED:
            found.append(f"{module} must not read configuration directly")
    return found


class TestLayering(unittest.TestCase):
    def test_package_root_exists(self) -> None:
        self.assertTrue(PACKAGE_ROOT.is_dir(), f"missing package root: {PACKAGE_ROOT}")

    def test_every_module_maps_to_a_known_layer(self) -> None:
        unmapped = [
            _module_name(path)
            for path in _agentwiki_files()
            if _layer_of(_module_name(path)) is None
        ]
        self.assertEqual(unmapped, [], f"add these modules to _LAYER_PREFIXES: {unmapped}")

    def test_dependency_direction_is_respected(self) -> None:
        problems: list[str] = []
        for path in _agentwiki_files():
            module = _module_name(path)
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            problems.extend(_violations(module, _imported_modules(tree)))
        self.assertEqual(problems, [], "dependency direction violated:\n" + "\n".join(problems))

    def test_domain_stays_free_of_frameworks_and_io(self) -> None:
        banned = {"typer", "fastmcp", "aiosqlite", "sqlite3", "sqlalchemy"}
        problems: list[str] = []
        for path in sorted((PACKAGE_ROOT / "domain").rglob("*.py")):
            imported = _imported_modules(ast.parse(path.read_text(encoding="utf-8")))
            for module in sorted(imported & banned):
                problems.append(f"{_module_name(path)} must not import {module}")
        self.assertEqual(problems, [], "\n".join(problems))

    def test_config_is_read_only_by_composition_roots(self) -> None:
        readers: list[str] = []
        for path in _agentwiki_files():
            module = _module_name(path)
            imported = _imported_modules(ast.parse(path.read_text(encoding="utf-8")))
            if "agentwiki.config" in imported and module not in _CONFIG_ALLOWED:
                readers.append(module)
        self.assertEqual(readers, [], f"unexpected config readers: {readers}")

    def test_runtime_does_not_depend_on_composition_roots(self) -> None:
        problems: list[str] = []
        for path in sorted((PACKAGE_ROOT / "runtime").rglob("*.py")):
            imported = _imported_modules(ast.parse(path.read_text(encoding="utf-8")))
            for module in sorted(imported):
                if _layer_of(module) == "composition":
                    problems.append(f"{_module_name(path)} must not import {module}")
        self.assertEqual(problems, [], "\n".join(problems))


def _self_check() -> int:
    """`python -m tests.unit.test_layering` reports violations as a checklist."""
    suite = unittest.TestLoader().loadTestsFromTestCase(TestLayering)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(_self_check())
