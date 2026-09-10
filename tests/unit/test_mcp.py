import json

from fastmcp import Client

from agentwiki.mcp import mcp


async def test_mcp_exposes_retrieval_rules_and_validation(tmp_path, monkeypatch) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "evidence.md").write_text("SQLite evidence\n", encoding="utf-8")
    config_dir = tmp_path / ".agentwiki"
    config_dir.mkdir()
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "document_root": "documents",
                "index_path": ".agentwiki/index.sqlite3",
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.chdir(tmp_path)
    async with Client(mcp) as client:
        tools = await client.list_tools()
        resources = await client.list_resources()
        result = await client.call_tool("get_wiki_context", {"query": "SQLite"})
    assert {tool.name for tool in tools} == {
        "get_wiki_context",
        "get_wiki_rules",
        "validate_wiki",
    }
    assert {resource.uri for resource in resources} == {
        "agentwiki://rules",
        "agentwiki://guide",
    }
    context_tool = next(tool for tool in tools if tool.name == "get_wiki_context")
    assert "mode" not in context_tool.input_schema.get("properties", {})
    assert context_tool.annotations is not None
    assert context_tool.annotations.read_only_hint is True
    assert result.data["results"][0]["path"] == "evidence.md"
