# AgentWiki MCP Tools

AgentWiki 通过 stdio 暴露本地 Markdown 文档工具。启动方式：

```bash
uv run agentwiki-mcp
```

服务器同时暴露只读的 `wiki://{path*}` Resource Template。客户端可以通过
`resources/read` 读取 Markdown 原文，例如 `wiki://guides/search.md`；文档写入、修改、
删除和移动仍必须使用下方 tools。

MCP 服务通过 FastMCP lifespan 创建一次 `NoteService` 和 aiosqlite 索引连接，并在同一
MCP 会话内复用；会话结束时统一关闭连接。直接调用 Python 函数进行测试时没有 lifespan
context，则自动回退为单次调用服务。

文档库和索引位置由环境变量配置：

```bash
AGENTWIKI_DOCUMENT_ROOT=./documents \
AGENTWIKI_INDEX_PATH=./.agentwiki/index.sqlite3 \
uv run agentwiki-mcp
```

## 共同约定

- 文档事实源是文档库内的 `.md` 文件；SQLite 只保存可重建的派生索引。
- 所有路径都必须是相对于文档库根目录的路径，不允许路径穿越。
- 文档可以用相对路径、唯一标题或 `wiki://` 标识定位。
- `wiki://` 后面可以是相对路径或标题，例如 `wiki://guides/search`、`wiki://Search Guide`。
- 目录操作使用相对目录路径；空字符串或 `/` 表示文档库根目录。
- 写入和修改会同步更新 SQLite 索引；外部文件变化由 `watch-index` 或 `rebuild_index`/增量同步处理。
- 工具不执行内容审核，不维护云端项目、知识图谱或关系图谱。

## 工具总览

| 工具 | 用途 | 是否修改文件 |
| --- | --- | --- |
| `write_note` | 创建 Markdown 文档并建立索引 | 是 |
| `read_note` | 读取文档正文、Frontmatter 或行范围 | 否 |
| `update_note` | 整体更新已有文档正文或 Frontmatter | 是 |
| `edit_note` | 对文档执行增量编辑 | 是 |
| `delete_note` | 删除文档或目录 | 是 |
| `move_note` | 移动文档或目录 | 是 |
| `search_notes` | 关键词、语义或混合搜索 | 否 |
| `list_directory` | 浏览 Markdown 文件和目录 | 否 |
| `rebuild_index` | 从 Markdown 全量重建 SQLite 索引 | 修改索引 |

## Resource 与 Context

### `wiki://{path*}` Resource Template

- 使用 `wiki://` scheme，`path*` 支持文档库内的多级相对路径。
- 返回 `text/markdown`，与 `read_note` 共用路径安全和文档读取规则。
- Resource 是只读入口；所有修改必须通过 tools，确保 Markdown 与 SQLite 派生索引保持一致。

### Context 生命周期与进度

- tools/resources 使用 FastMCP `Context` 获取 lifespan 中的共享 `NoteService`。
- `rebuild_index` 通过 `Context.report_progress()` 报告已处理文档数，并通过 `Context.info()` 记录完成信息。
- 这些属于 MCP 运行时适配能力，不改变 domain/service 的文档规则。

## 工具说明

### `write_note`

创建文档。`path` 不传时，根据 `title` 和 `directory` 生成 `<directory>/<slug>.md`。

参数：

- `title: str`：文档标题；显式传入 `path` 时仍建议提供，用于默认 Frontmatter 和返回信息。
- `content: str`：Markdown 正文，也可以包含 YAML Frontmatter。
- `directory: str = ""`：目标目录。
- `tags: list[str] | None`：写入 Frontmatter 的标签。
- `note_type: str = "note"`：写入 Frontmatter 的 `type`。
- `metadata: object | None`：额外 Frontmatter；正文中的 Frontmatter 字段优先级更高。
- `overwrite: bool = false`：是否覆盖已有文件。
- `path: str | None`：显式 Markdown 相对路径。

返回：`{"path": "...", "title": "..."}`。

### `read_note`

按路径、唯一标题或 `wiki://` 标识读取文档。

参数：

- `identifier: str`：文档路径、标题或 `wiki://` 标识。
- `include_frontmatter: bool = false`：是否在 `content` 中返回原始 YAML Frontmatter。
- `start_line: int | None`、`end_line: int | None`：1-based、闭区间行范围。

返回：`path`、`content` 和解析后的 `frontmatter`。

### `update_note`

更新已有文档。未传入的字段保持不变；传入 `frontmatter` 时会整体替换 Frontmatter。

参数：`path`、可选的 `content`、可选的 `frontmatter`。

返回：`{"path": "...", "title": "..."}`。

### `edit_note`

执行局部修改。支持的 `operation`：

- `append`：追加到正文末尾；目标不存在时按 identifier 创建。
- `prepend`：插入到正文开头；目标不存在时按 identifier 创建。
- `find_replace`：将 `find_text` 替换为 `content`，默认要求恰好替换一次。
- `replace_section`：替换指定章节，可通过 `replace_subsections` 控制是否包含子章节。
- `insert_before_section`：在章节标题前插入。
- `insert_after_section`：在章节结束后插入。

章节通过 `section` 指定，例如 `Root/Details` 或 `Root/Details[1]`。

返回：`{"path": "...", "title": "..."}`。

### `delete_note`

删除单个文档或目录。

- `identifier`：文件路径、唯一标题、`wiki://` 标识，或目录路径。
- `is_directory: bool = false`：为 `true` 时删除目录及其中的 Markdown 文件。

不允许删除文档库根目录。

返回：`{"path": "...", "status": "deleted"}`。

### `move_note`

在同一文档库内移动文档或目录。

- `identifier`：源文档标识或目录路径。
- `destination_path`：目标相对路径；与 `destination_folder` 互斥。
- `destination_folder`：仅移动单个文档时使用，保留原文件名。
- `is_directory: bool = false`：是否按目录移动。

不允许移动到文档库根目录内部的源目录中，也不允许覆盖已有目标。

返回：`{"source": "...", "target": "...", "status": "moved"}`。

### `search_notes`

在 SQLite 派生索引中搜索文档。

- `query`：搜索文本；可使用 `tag:xxx` 过滤标签。
- `mode`：`keyword`、`title`、`permalink`、`semantic` 或 `hybrid`。
- `search_type`：`mode` 的兼容别名；传入时优先于 `mode`。
- `limit`：每页数量，默认 20，最大 100。
- `page`：从 1 开始的页码。
- `tags`、`note_types`：标签和文档类型过滤。
- `metadata_filters`：支持嵌套字段和 `$in`、`$gt`、`$gte`、`$lt`、`$lte`、`$between` 基础比较。

返回结果包含 `path`、`title`、`score`、`frontmatter` 和 `snippet`。

语义搜索需要设置 `AGENTWIKI_EMBEDDING_MODEL`。模型使用 `fastembed`，未配置时应使用 `keyword`、`title` 或 `permalink` 模式。

### `list_directory`

浏览文档库中的 Markdown 文件和目录。

- `directory: str = ""`：相对目录路径；空字符串表示根目录。
- `depth: int = 1`：递归深度，范围 1-10。
- `file_name_glob`：文件名过滤，例如 `*.md`、`*meeting*`；目录仍会保留用于导航。
- `page`、`page_size`：分页参数，`page_size` 最大 200。

返回：

```json
{
  "directory": "guides",
  "entries": [
    {
      "path": "guides/search.md",
      "name": "search.md",
      "kind": "file",
      "size": 1024,
      "modified_at": 1788960000.0
    }
  ],
  "page": 1,
  "page_size": 20,
  "total": 1,
  "has_more": false
}
```

### `rebuild_index`

扫描文档库中的 Markdown 文件，清空并重建 SQLite FTS5、元数据和可选向量投影。

适用场景：首次初始化、索引损坏、索引版本升级或需要显式恢复一致性时。普通外部文件变化应优先使用 `watch-index` 的增量同步。

返回：`{"indexed": 42}`。

## 推荐调用流程

不确定文档位置时：

1. `list_directory` 浏览目录。
2. `search_notes` 查找候选文档。
3. `read_note` 确认正文和 Frontmatter。
4. 使用 `edit_note`、`update_note`、`move_note` 或 `delete_note` 执行变更。

创建文档时直接调用 `write_note`；不需要先调用审核或知识图谱工具。
