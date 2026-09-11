# AgentWiki

面向多个 AI Agent 的本地优先 Markdown 知识检索层（Rust 实现）。Markdown 文件是事实源，
全文索引与元数据存储都是可删除、可重建的派生投影。

## 项目价值

Agent 的原生文件工具擅长**已知路径**的读取与文档增删改，却不擅长"从未知到已知"：当任务
涉及历史方案、团队约定、已有决策、跨文档关系或近期变化时，Agent 不知道去哪里找、找什么。

AgentWiki 补上这一环：为 Agent 提供精确、关键词、语义、混合与近期检索能力，返回**可继续
读取的证据片段与路径**，让 Agent 用原生工具回读原文后形成结论。它只做"检索 + 治理"，不做
文档读写、不做查询 LLM、不依赖云端服务——本地优先，离线可用。

## 基础方案

```
Markdown 文档库（事实源，含 YAML Frontmatter）
        │  增量同步（mtime/size 快速筛选 + 内容哈希确认）
        ▼
   ┌─────────────────────────────┐
   │ Tantivy 检索引擎             │  BM25 关键词（lindera 中文分词）
   │ + 可选本地向量腿（按里程碑接入）│  语义检索（模型变化自动重建）
   └──────────────┬──────────────┘
                  │ + SQLite 元数据：同步账本 / 图谱 / 状态（不做检索）
                  ▼
       排名融合 → 章节级证据 + 一跳文档关系 + 降级诊断
                  ▼
        CLI（agentwiki） / MCP（agentwiki-mcp）
```

- **事实源**：Markdown 正文 + YAML Frontmatter；Wiki 根目录的 `AGENTWIKI.md` 是唯一的规则
  与 Agent 指导入口（首次启动自动写入默认模板，已有文件永不覆盖）。
- **投影**：Tantivy 提供关键词全文检索（BM25 + `lindera` 中文分词）与可选的向量腿；SQLite
  仅作元数据镜像（同步账本 / 文档图谱 / 同步状态），**不执行**全文或向量检索。两者都可随时
  从 Markdown 全量重建。
- **检索**：关键词、语义与近期信号在候选集上做排名融合；同一文档最多返回两个片段；单个
  检索源失败只降级（`degraded` 报告原因），不中断整次检索；从内部链接与 Frontmatter
  `relations` 派生一跳文档关系。
- **治理**：`get_wiki_rules` 返回目录适用的必填字段、类型与标签规则（完全由 `AGENTWIKI.md`
  配置决定，系统不内置必填字段）；`validate_wiki` 只报告格式、Frontmatter 与内部链接问题，
  绝不自动改写 Markdown。
- **入口**：
  - CLI（二进制 `agentwiki`）：本地检索、索引同步、重建、校验与治理维护；
  - MCP（二进制 `agentwiki-mcp`）：为 Agent 暴露 `get_wiki_context`（任务上下文检索）、
    `get_wiki_rules`（规则获取）、`validate_wiki`（规范校验）。已知路径读取与文档增删改
    仍由 Agent 原生工具完成，MCP 不重复暴露。

## 快速开始

```bash
# 构建
cargo build

# 搜查任务上下文（空查询列出最近修改的文档）
cargo run --bin agentwiki query "认证方案"
cargo run --bin agentwiki query

# 索引与治理维护
cargo run --bin agentwiki sync-index
cargo run --bin agentwiki rebuild-index
cargo run --bin agentwiki validate-wiki            # 全库校验
cargo run --bin agentwiki validate-wiki --path decisions/auth.md

# 查看解析后的配置
cargo run --bin agentwiki show-config

# 启动 MCP（stdio；SDK 接线完成前为占位实现）
cargo run --bin agentwiki-mcp --features mcp
```

配置统一放在用户配置目录 `~/.agentwiki/config.json`，首次运行缺少文件时自动创建默认配置；
进程环境变量不参与配置。CLI 可用 `--no-config` 关闭配置文件，改用 `--wiki-root` 与
`--index-dir` 显式指定（默认文档根目录是 `~/AgentWiki`，索引目录是 `~/.agentwiki`）：

```json
{
  "document_root": "~/AgentWiki",
  "index_path": "~/.agentwiki/index.sqlite3"
}
```

## 实现状态

Rust 重写进行中（Python 原型已归档于 `legacy/python/`，基准脚本与语料在
`legacy/python/benchmarks/`，不再维护）。

- 已实现：CLI 子命令与配置加载、Markdown 扫描与切块、增量同步与 rebuild、SQLite 元数据
  （账本/图谱/状态）、文档图谱、格式与规则校验；
- 进行中（下一里程碑）：Tantivy 全文接线与检索融合、MCP SDK 三个工具、可选语义向量腿。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 详细架构设计：数据与检索机制、模块依赖、实现状态、设计取舍 |
| [docs/MCP_TOOLS.md](docs/MCP_TOOLS.md) | MCP 协议契约：三个工具的参数与返回结构 |
| [docs/RULES.md](docs/RULES.md) | `AGENTWIKI.md` 规则配置参考：字段、合并语义、错误码 |
| [docs/AGENTWIKI.md](docs/AGENTWIKI.md) | 规则文件的完整参考示例（含 sections / tag_aliases） |
| [AGENTS.md](AGENTS.md) | 工程开发规范：构建、测试、代码与提交要求 |