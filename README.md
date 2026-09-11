# AgentWiki

面向多个 AI Agent 的本地优先 Markdown 知识检索层。Markdown 是事实源，检索索引和元数据存储都是可删除、可重建的派生数据。

> **迁移状态**：本仓库已确定 LanceDB 目标方案，Rust 代码尚未整体迁移。当前检索引擎和 MCP 入口仍为占位接线；下文目标能力不代表当前已经可用。具体差距见 [架构与迁移说明](docs/ARCHITECTURE.md)。

## 目标能力

Agent 原生文件工具负责已知路径读取和文档增删改；AgentWiki 负责发现未知位置的知识，返回可回读的路径、章节和证据。

- 检索：关键词、语义、混合、精确匹配和近期搜查，支持范围与元数据过滤。
- 证据：章节片段、命中来源和最多一跳的显式文档关系。
- 治理：获取 Wiki 规则，校验 Frontmatter、目录约束、内部链接与 Markdown 格式。
- 格式修复：默认只检查，显式请求后用内置格式化组件修复格式并重新校验。

不提供 HTTP API、云同步、Web UI、查询 LLM 或独立数据库服务。面向一万篇以内的本地 Wiki，允许依赖使用原生库。

## 目标方案

```text
Markdown 文档库 + AGENTWIKI.md 规则
                ↓ 增量同步、解析、章节切分
       ┌────────┴──────────┐
       │ LanceDB           │ SQLite
       │ 全文 / 向量 / RRF │ 账本 / 文档信息 / 关系
       └────────┬──────────┘
                ↑ FastEmbed 本地向量生成
                ↓
         章节证据与降级诊断
                ↓
          CLI / MCP 三个工具
```

单 Rust package，两个二进制入口；代码按 `document`、`retrieval`、`governance` 聚合。复用现成解析、切分、检索、推理和格式化组件，Runtime 统一持有资源。向量同步批处理，不维护后台队列或 watcher。

## 当前入口

以下为现有命令形态；查询仍不能提供实际检索命中，MCP 命令仅输出接线提示：

```bash
cargo build
cargo run --bin agentwiki show-config
cargo run --bin agentwiki sync-index
cargo run --bin agentwiki rebuild-index
cargo run --bin agentwiki validate-wiki --path decisions/auth.md
cargo run --bin agentwiki validate-wiki
cargo run --bin agentwiki query "认证方案"
cargo run --bin agentwiki query ""
cargo run --bin agentwiki-mcp --features mcp
```

**迁移后新增，当前不可用**：

```bash
cargo run --bin agentwiki validate-wiki --path decisions/auth.md --fix-format
cargo run --bin agentwiki validate-wiki --full --fix-format
```

默认校验不写文件。单文件修复必须指定 `--path`，全库修复必须指定 `--full`；两种范围互斥。格式修复不修正标签、链接或业务内容。

## 配置与数据

配置保留在 `~/.agentwiki/config.json`，缺失时创建默认配置，不读取业务环境变量配置：

```json
{
  "wiki_root": "~/AgentWiki",
  "embedding_model": null
}
```

- `wiki_root`：Wiki 根目录；`~` 展开为主目录，相对路径以配置目录为基准。`--wiki-root` 优先于配置文件。
- `embedding_model`：`null` 或缺省关闭语义检索。目标首个支持值为 `BAAI/bge-small-zh-v1.5`，由 FastEmbed 适配到相应模型资源；模型准备完成后支持离线使用。当前字段尚未接入推理。
- 派生数据仍位于 `~/.agentwiki/`。当前共用一组投影；目标按规范化 Wiki 根目录隔离投影，模型缓存单独存放，无需新增配置项。

Wiki 根目录的 `AGENTWIKI.md` 是唯一规则与 Agent 指导入口，不进入普通文档索引。Runtime 缺失时写入默认模板，已存在时不覆盖；当前 CLI 已调用 Runtime，MCP 尚未接入。必填字段由规则声明，系统不内置必填字段。

## 开发与文档

Python 原型及其基准归档在 `legacy/python/`，不参与 Rust 开发。后续整体迁移包括源码目录、Cargo 依赖、源码内默认模板和测试，本轮只更新文档。

| 文档 | 职责 |
| --- | --- |
| [架构](docs/ARCHITECTURE.md) | 组件、目标目录、数据流、迁移映射及验收 |
| [MCP 契约](docs/MCP_TOOLS.md) | 三个工具的目标参数、结果与副作用 |
| [规则参考](docs/RULES.md) | Frontmatter、目录匹配、标签与格式规范 |
| [规则示例](docs/AGENTWIKI.md) | 可复制的完整规则与 Agent 指引 |
| [工程规范](AGENTS.md) | 开发边界、依赖、测试和提交要求 |
