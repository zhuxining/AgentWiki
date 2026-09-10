# AgentWiki

AgentWiki 是面向多个 AI Agent 的本地优先 Markdown 搜查层。Markdown 文件是事实源，
SQLite FTS5 和可选的本地向量索引是可删除、可重建的派生投影。

AgentWiki 重点解决 Agent 原生文件工具不擅长的“从未知到已知”：发现历史、已有方案、
跨文档关系和近期变化，并返回可继续读取的证据片段与路径。已知路径的读取以及文档增删改
仍由 Agent 原生工具完成。

## MCP 能力

| 能力               | 作用                                                     |
| ------------------ | -------------------------------------------------------- |
| `get_wiki_context` | 自动组合精确、关键词、语义和近期检索，为当前任务组装证据 |
| `get_wiki_rules`   | 获取目标目录或文档适用的组织与 Frontmatter 规则          |
| `validate_wiki`    | 在原生文件修改后检查格式、结构、Frontmatter 和内部链接   |

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

配置统一放在用户配置目录 `~/.agentwiki/config.json`，相对路径以该文件所在目录为基准解析；
配置文件不存在时会在首次运行时创建一个默认配置：

```json
{
  "document_root": "~/AgentWiki",
  "index_path": "~/.agentwiki/agentwiki.sqlite3",
  "embedding_model": "BAAI/bge-small-zh-v1.5",
  "min_similarity": 0.44
}
```

进程环境变量不会覆盖该文件。默认文档根目录是 `~/AgentWiki`。

`embedding_model` 默认是 `BAAI/bge-small-zh-v1.5`（512 维，约 90 MB，首次使用会下载）：它
面向中文检索，中文改写的召回与旧的多语言模型持平，且模型体积更小。代价是**英文**——英文
查询对中文文档的相似度会明显抬高，弃答因此更依赖阈值。若 Wiki 的查询经常中英混排，可以换回
`sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`（384 维，约 220 MB）；设为
`null` 则完全禁用语义检索，关键词检索始终可用。

`min_similarity` 是语义命中的余弦下限，它**与模型绑定**：默认值 0.44 是在上述中文模型上用
`tests/unit/test_semantic_retrieval.py` 的语料标定的（最弱真实改写 0.4470，真正无关的查询
峰值 0.4281）。这个可用窗口**只有约 0.02**，远窄于旧多语言模型的约 0.20（改写 0.34–0.58 /
无关 0.10），所以阈值更脆弱：更换模型后必须重新标定，否则会静默地全部命中或全部拒绝。

标定必须用**索引实际嵌入的文本格式**（`title\ntags\nsection\ncontent`），而不是 Markdown
原文——两者算出的相似度相差约 0.01–0.02，在这个窗口宽度下足以得出相反结论。

还有一类查询是单阈值**分不开**的：主题相邻但并非所需。例如「如何用 Kubernetes 部署微服务」
对一篇发布手册打 0.4965，高于最弱真实改写。纯中文 embedding 加单阈值无法区分"相邻"与
"相关"，这类假阳性需要调用方结合 `match_sources`、`scope` 或读取原文来收敛。

该阈值只有一处定义（`domain/retrieval.py` 的 `DEFAULT_MIN_SIMILARITY`），配置层直接复用，
因为 `ContextQuery` 把它作为字段默认值：**直接构造查询的调用方（基准、测试）必须和 CLI /
MCP 入口跑同一个阈值**。此前两处各有一个默认值并发生漂移，导致基准测的阈值和产品实际
使用的不是同一个。

首次启动若索引 schema 变更，会删除并重建派生索引（Markdown 不受影响）；重建会在日志中
说明开始与原因，若上次重建中断则下次启动会报告。

查询前会比较 Markdown 路径、`mtime_ns` 和大小，只同步发生变化的文档，因此原生工具修改后
的下一次检索无需等待 watcher。

SQLite 只是镜像索引，不做旧 schema 迁移；检测到索引版本不兼容时会直接删除索引文件并从
Markdown 重建，原始文档不会被改动。

Wiki 根目录的 `AGENTWIKI.md` 是唯一的规则与 Agent 指导入口。首次启动（CLI 或 MCP）若发现它
不存在，会写入随包分发的默认模板；已存在的文件永远不会被覆盖。

索引还会从 `[[内部链接]]`、相对 Markdown 链接和 Frontmatter `relations` 派生一跳文档关系，
并将关系类型、来源章节、原文上下文和关联路径附加到检索证据中；目标不存在的关系会保留为
`unresolved`。非法关系声明只产生诊断，不阻断其他文档索引。语义索引使用本地 sqlite-vec，
按 chunk 的标题/标签/章节/正文 hash 复用未变化向量；模型切换会自动重建，首次建立等待初始
向量同步，后续更新异步进行。扩展或 embedding 不可用时，关键词检索仍然可用。
进程退出时未完成的向量任务会在下一次启动时恢复重试。

详细设计见 [架构文档](docs/ARCHITECTURE.md)，协议见
[MCP 能力说明](docs/MCP_TOOLS.md)，规则字段见
[Wiki Rules 配置参考](docs/RULES.md)。真实 Wiki 基准测试见
[benchmarks/README.md](benchmarks/README.md)。
