# AgentWiki

AgentWiki 是面向多个 AI Agent 的本地优先文档层。它以本地 Markdown 文档库为事实源，并将文档索引到本地 SQLite，让 Agent 通过统一工具操作和搜索 Markdown 文档。

## 目标

AgentWiki 的核心能力是：

- 写文档
- 读文档
- 改文档
- 删除文档
- 移动文档
- 搜索文档：支持关键词、语义和混合查询

Markdown 正文保存文档内容，YAML Frontmatter 保存类型、状态、范围、项目、标签和来源等可查询元数据。SQLite 保存可重建的搜索索引：关键词索引使用 FTS5，语义索引使用可插拔的本地 embedding provider 和向量投影；索引同时记录内容哈希与向量来源状态，避免复用过期数据。

## 架构入口

```text
Agent / Script
     │
     ├── CLI ──┐
     └── MCP ──┴── Services / Domain ─── Markdown 文档库
```

- **CLI**：本地调试、初始化、批处理和自动化脚本。
- **MCP**：向 Agent 暴露共享文档操作工具。
- **Services / Domain**：承载文档操作、索引同步和搜索规则。
- **Repository / Indexing**：通过 `aiosqlite` 访问 SQLite 索引，并负责扫描、增量同步和索引重建。
- **SQLite Index**：保存从 Markdown 文档库派生的文档元数据、全文索引和可选向量索引。
- **Markdown 文档库**：Markdown 文件及其 YAML Frontmatter 的持久化边界。

HTTP API、云端同步、Postgres、多用户服务和 Web UI 不属于当前架构范围。

详细设计见 [架构文档](docs/ARCHITECTURE.md)。

## 工具能力

CLI 和 MCP 应共享以下应用能力；具体协议参数由各入口适配：

| 能力 | 作用 |
| --- | --- |
| `write_note` | 创建文档或写入文档内容与 Frontmatter |
| `read_note` | 按路径读取文档 |
| `edit_note` | 以追加、前置、查找替换或章节操作增量修改文档 |
| `update_note` | 更新文档内容或 Frontmatter |
| `delete_note` | 删除文档 |
| `move_note` | 在文档库内移动文档 |
| `search_notes` | 使用关键词、语义或混合模式搜索文档 |

工具参数参考 Basic Memory 的本地文档工具，但只保留本项目的基础范围：写入支持 `title`、`directory`、`tags`、`note_type`、`metadata` 和 `overwrite`，也接受正文自带的 YAML Frontmatter；读取支持 Frontmatter 和行范围；编辑支持增量操作；移动和删除支持路径、唯一标题和目录；搜索支持 `text/title/permalink/vector/hybrid`、分页、标签、文档类型和 Frontmatter 过滤。所有写入统一经过 `mdformat` 的 GFM 与 Frontmatter 扩展格式化。

不实现云端项目、内容审核、知识图谱、schema、Web UI 和非 Markdown 文件工具。

SQLite 索引是派生数据，不是文档事实源。首次使用或索引损坏时可以从 Markdown 文档库扫描重建；语义模型不可用时，关键词搜索仍可独立工作。

服务、CLI 和 MCP 的索引访问链路使用 `asyncio`/`aiosqlite`；Markdown 文件仍由文档存储边界统一写入，SQLite 只保存可删除、可重建的派生投影。

启用基于 `fastembed` 的本地语义模型：

```bash
AGENTWIKI_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5 uv run agentwiki rebuild-index
```

`fastembed` 已作为项目依赖安装，模型按首次语义索引或查询时惰性加载。不设置 `AGENTWIKI_EMBEDDING_MODEL` 时，项目只使用 FTS5 关键词索引，不会下载或加载 embedding 模型。

## 快速开始

项目使用 Python 3.14+ 和 `uv`：

```bash
uv sync
uv run agentwiki
# 启动 MCP（stdio）
uv run agentwiki-mcp
```

CLI 和 MCP 当前都已提供上述文档工具；CLI 适合本地脚本与调试，MCP 适合 Agent 调用。

## 开发

```bash
uv sync
uv run ruff check
uv run ty check
uv run pytest
```

监听文档库的直接修改并自动重建索引：

```bash
uv run agentwiki watch-index
```

提交前还应执行：

```bash
git diff --check
```

工程约定见 [AGENTS.md](AGENTS.md)。
