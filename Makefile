# AgentWiki developer tasks. Every gate that CI runs is a target here, so a local run
# and a pipeline run cannot drift apart.
.DEFAULT_GOAL := help
.PHONY: help install sync check lint type test test-unit test-quiet \
        watch rules validate rebuild-index benchmark attribution clean dist

UV ?= uv
PYTEST_FLAGS ?=

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

install: ## Create the environment and install all dependencies
	$(UV) sync

sync: ## Refresh the lockfile and environment
	$(UV) sync --upgrade

check: lint type test-unit ## Run every gate (what CI runs)
	@echo "--- integration suite (separate process: loads a real model) ---"
	$(UV) run pytest -q --no-cov -m "integration" $(PYTEST_FLAGS)

lint: ## Lint without rewriting files (pass --fix to apply fixes)
	$(UV) run ruff check

type: ## Type-check
	$(UV) run ty check

test: ## Run the full test suite
	$(UV) run pytest $(PYTEST_FLAGS)

test-quiet: ## Run the test suite with minimal output
	$(UV) run pytest -q $(PYTEST_FLAGS)

test-unit: ## Run only tests that need no external services
	$(UV) run pytest -m "not integration" $(PYTEST_FLAGS)

benchmark: ## Run the Chinese retrieval benchmark against a corpus (CORPUS=..., QUERIES=...)
	@test -n "$(CORPUS)" || (echo "set CORPUS=/path/to/wiki" && exit 1)
	$(UV) run python -m benchmarks.runner \
		--corpus "$(CORPUS)" \
		--queries "$(or $(QUERIES),benchmarks/queries/zh-team-wiki.jsonl)" \
		--mode "$(or $(MODE),keyword)" \
		--output "$(or $(OUTPUT),/tmp/agentwiki-benchmark.json)"

attribution: ## Attribute retrieval loss to a pipeline layer (CORPUS=..., QUERIES=...)
	@test -n "$(CORPUS)" || (echo "set CORPUS=/path/to/wiki" && exit 1)
	$(UV) run python -m benchmarks.attribution \
		--corpus "$(CORPUS)" \
		--queries "$(or $(QUERIES),benchmarks/queries/zh-team-wiki.jsonl)" \
		--mode "$(or $(MODE),keyword)" \
		--k "$(or $(K),5)" \
		--output "$(or $(OUTPUT),/tmp/agentwiki-attribution.json)"

watch: ## Watch Markdown changes and keep the index fresh
	$(UV) run agentwiki watch-index

rules: ## Print the effective Wiki rules
	$(UV) run agentwiki rules $(SCOPE)

validate: ## Validate the whole Wiki
	$(UV) run agentwiki validate-wiki --full

rebuild-index: ## Drop and rebuild the derived index
	$(UV) run agentwiki rebuild-index

dist: ## Build the wheel and sdist
	$(UV) build

clean: ## Remove caches and build output
	rm -rf .pytest_cache .ruff_cache dist .build
	find . -name __pycache__ -type d -prune -exec rm -rf {} +
