from fastmcp import Client

from agentwiki.mcp import mcp


async def test_mcp_resource_template_reuses_lifespan_service(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("AGENTWIKI_DOCUMENT_ROOT", str(tmp_path / "documents"))
    monkeypatch.setenv("AGENTWIKI_INDEX_PATH", str(tmp_path / "index.sqlite3"))

    async with Client(mcp) as client:
        templates = await client.list_resource_templates()
        assert any(template.uri_template == "wiki://{path*}" for template in templates)

        result = await client.call_tool(
            "write_note",
            {"title": "Today", "content": "Local Markdown content.", "path": "today.md"},
        )
        assert result.is_error is False

        contents = await client.read_resource("wiki://today.md")
        assert "title: Today" in contents[0].text
        assert contents[0].text.endswith("Local Markdown content.\n")
