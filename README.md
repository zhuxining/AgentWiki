# AgentWiki

面向多个 AI Agent 的本地优先 Markdown 知识检索层。Markdown 是事实源，LanceDB 是唯一可删除、可重建的派生投影。

> **迁移状态**：Rust 主链路已切换到 LanceDB，MCP 已接入官方 SDK。默认配置使用关键词检索；配置 embedding model 后启用 FastEmbed 向量投影。具体边界见 [架构与迁移说明](docs/ARCHITECTURE.md)。

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
       │ LanceDB Catalog                 │
       │ 文档 / 切片 / 关系 / 指纹        │
       │ 全文 / 向量 / 过滤 / RRF         │
       └────────┬──────────┘
                ↑ FastEmbed 本地向量生成
                ↓
         章节证据与降级诊断
                ↓
          CLI / MCP 三个工具
```

单 Rust package，两个二进制入口；代码按 `document`、`projection`、`retrieval`、`governance` 聚合。复用现成解析、切分、检索、推理和格式化组件，AgentWiki 异步门面统一持有资源。向量同步批处理，不维护后台队列或 watcher。

## 当前入口

以下为现有命令形态：

```bash
cargo build
cargo run --bin agentwiki show-config
cargo run --bin agentwiki sync-index
cargo run --bin agentwiki rebuild-index
cargo run --bin agentwiki optimize-index
cargo run --bin agentwiki validate-wiki --path decisions/auth.md
cargo run --bin agentwiki validate-wiki
cargo run --bin agentwiki query "认证方案"
cargo run --bin agentwiki query ""
cargo run --bin agentwiki validate-wiki --path decisions/auth.md --fix-format
cargo run --bin agentwiki validate-wiki --full --fix-format
cargo run --bin agentwiki-mcp
```

默认校验不写文件。单文件修复必须指定 `--path`，全库修复必须指定 `--full`；两种范围互斥。格式修复不修正标签、链接或业务内容。

`optimize-index` 只把新增行并入既有索引，是显式维护动作；查询路径不会隐式 optimize，也不会因此少返回结果。

## 配置与数据

配置保留在 `~/.agentwiki/config.json`，缺失时创建默认配置，不读取业务环境变量配置：

```json
{
  "wiki_root": "~/AgentWiki",
  "embedding_model": null
}
```

- `wiki_root`：Wiki 根目录；`~` 展开为主目录，相对路径以配置目录为基准。`--wiki-root` 优先于配置文件。
- `embedding_model`：`null` 或缺省关闭语义检索。支持值为 `BAAI/bge-small-zh-v1.5`，由 FastEmbed 适配并在同步时写入 LanceDB 向量投影；模型准备完成后支持离线使用。
- 派生数据位于 `~/.agentwiki/`，按规范化 Wiki 根目录隔离投影，模型缓存单独存放，无需新增配置项。

### 关键词检索

关键词检索使用 LanceDB FTS；当前采用 Lance 默认 tokenizer，不需要额外下载分词词典。中文专名、混合文本和代码标识符的召回效果取决于实际语料，应通过固定语料基准验证，不将 tokenizer 的存在等同于质量保证。

Wiki 根目录的 `AGENTWIKI.md` 是唯一规则与 Agent 指导入口，不进入普通文档索引。AgentWiki 门面在文件缺失时写入默认模板，已存在时不覆盖；CLI 与 MCP 共用同一异步用例层。`type`、`tags`、`summary` 是系统内置必填字段；其他必填字段由规则声明。

## 开发与文档

Rust 是唯一维护实现，主链路已迁移到目标目录和 Cargo 依赖，后续只做行为与协议的增量收敛。

| 文档 | 职责 |
| --- | --- |
| [产品与 MVP](docs/PRODUCT.md) | 产品定位、用户闭环、范围、需求与验收标准 |
| [架构](docs/ARCHITECTURE.md) | 组件、目标目录、数据流、迁移映射及验收 |
| [MCP 契约](docs/MCP_TOOLS.md) | 三个工具的目标参数、结果与副作用 |
| [规则参考](docs/RULES.md) | Frontmatter、目录匹配、标签与格式规范 |
| [规则示例](docs/AGENTWIKI.md) | 可复制的完整规则与 Agent 指引 |
| [工程规范](AGENTS.md) | 开发边界、依赖、测试和提交要求 |
