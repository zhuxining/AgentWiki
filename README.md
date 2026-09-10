# AgentWiki

AgentWiki 是面向多个 AI Agent 的本地优先 Markdown 搜查层。Markdown 文件是事实源，
SQLite FTS5 和可选的本地向量索引是可删除、可重建的派生投影。

AgentWiki 重点解决 Agent 原生文件工具不擅长的“从未知到已知”：发现历史、已有方案、
跨文档关系和近期变化，并返回可继续读取的证据片段与路径。已知路径的读取以及文档增删改
仍由 Agent 原生工具完成。

## MCP 能力

| 能力 | 作用 |
| --- | --- |
| `get_wiki_context` | 自动组合精确、关键词、语义和近期检索，为当前任务组装证据 |
| `get_wiki_rules` | 获取目标目录或文档适用的组织与 Frontmatter 规则 |
| `validate_wiki` | 在原生文件修改后检查格式、结构、Frontmatter 和内部链接 |
| `agentwiki://rules` | 根级机器规则 |
| `agentwiki://guide` | Wiki 使用指南 |

`get_wiki_context` 不要求 Agent 选择搜索模式。配置 embedding 时自动执行片段级混合检索；
不可用时保留关键词和近期查询并明确报告降级。空查询返回最近修改文档，带近期意图的主题
查询同时考虑相关性与真实文件修改时间。

## 使用

```bash
uv sync

# 搜查任务上下文；省略查询可查看最近修改
uv run agentwiki query "认证方案"
uv run agentwiki query

# 索引与治理维护
uv run agentwiki sync-index
uv run agentwiki rebuild-index
uv run agentwiki rules guides
uv run agentwiki validate-wiki --full

# 启动 MCP（stdio）
uv run agentwiki-mcp
```

配置统一放在项目根目录的 `.agentwiki/config.json`，相对路径以项目根目录为基准：

```json
{
  "document_root": "documents",
  "index_path": ".agentwiki/index.sqlite3",
  "embedding_model": "BAAI/bge-small-en-v1.5"
}
```

进程环境变量不会覆盖该文件；`embedding_model` 可设为 `null` 以禁用语义检索。

查询前会比较 Markdown 路径、`mtime_ns` 和大小，只同步发生变化的文档，因此原生工具修改后
的下一次检索无需等待 watcher。

详细设计见 [架构文档](docs/ARCHITECTURE.md)，协议见
[MCP 能力说明](docs/MCP_TOOLS.md)，规则字段见
[Wiki Rules 配置参考](docs/RULES.md)。
