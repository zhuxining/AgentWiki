# AgentWiki Architecture

> **Status**: active
>
> 本文描述 AgentWiki 的目标架构、稳定边界和阶段约束。目标架构不等同于当前已完成的实现；实现状态以代码和测试为准。

## 1. 系统目标

AgentWiki 是一个面向多个 AI Agent 的本地优先文档层。它使用本地 Markdown 文档库作为人和 Agent 都能访问的文档边界，为 Agent 提供统一的 Markdown 文档操作能力。

核心能力：

- 写文档：创建文档，写入正文和 YAML Frontmatter；
- 读文档：按路径读取文档及其元数据；
- 改文档：更新正文、Frontmatter 或文档属性；
- 删除文档：删除文档库内的指定文档；
- 移动文档：在文档库根目录内修改文档路径；
- 搜索文档：使用关键词、语义或混合模式查询文档。

AgentWiki 聚焦文档操作、索引和搜索，不扩展为内容工作流或项目管理系统。

## 2. 核心原则

### 2.1 Markdown 文档库是持久化边界

本地 Markdown 文档库是文档的事实来源。AgentWiki 不在入口层维护一套与文档库脱节的平行文档存储。

### 2.2 SQLite 是可重建的派生索引

SQLite 保存从 Markdown 文档库派生的文档元数据、关键词索引和可选语义向量。它用于加速搜索，不拥有文档内容的最终写入权，也不替代 Markdown 文件。

- Markdown 文件丢失时，索引记录必须能够被清理；
- SQLite 损坏或版本变化时，必须能够从 Markdown 文档库全量重建；
- 索引暂时不可用时，文档读写仍应以 Markdown 文档库为准，搜索能力可以降级或报告索引不可用。

### 2.3 一个文件对应一个文档

一条可独立读取、修改、移动和搜索的内容对应一个 Markdown 文件：

- Markdown 正文承载人类可读内容；
- YAML Frontmatter 承载类型、状态、范围、项目、标签、来源和时间等元数据；
- Frontmatter 是解析、过滤和搜索的共享契约。

### 2.4 入口层只做协议适配

CLI 和 MCP 可以有不同的参数格式和返回格式，但不得各自实现文档操作规则。两者必须调用同一套 services/domain 能力。

### 2.5 Composition root 负责装配

每个入口拥有自己的 composition root，负责读取配置、解析运行模式、创建容器和装配依赖。业务模块通过构造参数或端口接收依赖，不直接读取全局配置。

### 2.6 本地优先、可演进

文档操作规则不绑定具体入口或文件系统实现。未来若增加其他存储或入口，应通过新的 adapter 和 composition root 接入，而不是侵入领域层。

## 3. 入口与数据流

当前正式入口只有 CLI 和 MCP：

```text
Agent / Script
      │
      ├────────────── CLI composition root
      │                       │
      └────────────── MCP composition root
                              │
                              ▼
                    Services / use cases
                              │
                              ▼
                       Domain rules
                              │
                              ▼
                    Ports / repository contracts
                              │
                              ▼
                     Markdown document adapter
                              │
                              ▼
                      Markdown 文档库
                              │
                              ▼
                    Local SQLite index
```

典型流程：

```text
写入：CLI/MCP → write_note → 路径与内容校验 → Markdown adapter → Markdown 文件 → 更新索引
读取：CLI/MCP → read_note → 路径解析 → Markdown adapter → 文档结果
修改：CLI/MCP → update_note → 文档存在性与内容校验 → Markdown adapter → 更新索引
删除：CLI/MCP → delete_note → 路径边界校验 → Markdown adapter → 删除索引记录
移动：CLI/MCP → move_note → 源路径与目标路径校验 → Markdown adapter → 更新索引路径
搜索：CLI/MCP → search_notes → 查询条件解析 → SQLite FTS/向量索引 → 匹配结果
外部变更：文档库 watcher/启动扫描 → Markdown adapter → 增量索引或索引重建
```

HTTP API、云端同步和 Web UI 暂不属于当前架构承诺；新增入口时必须复用 services/domain 层。

## 4. 领域划分

### note

代表一份可持久化的 Markdown 文档，包含路径、正文和 Frontmatter。note 领域对象不直接暴露文件系统操作。

### notePath

代表文档库内的相对文档路径。它负责路径格式和文档库根目录边界，禁止路径穿越、绝对路径和越界软链接场景。

### Frontmatter

代表 Markdown 文件头部的 YAML 元数据，负责解析、序列化、字段访问和搜索条件匹配。

### SearchQuery / SearchResult

`SearchQuery` 表达路径、正文和 Frontmatter 的搜索条件，以及 `keyword`、`semantic` 或 `hybrid` 检索模式；`SearchResult` 表达匹配文档的路径、元数据、相关性分数和必要的内容摘要。搜索规则属于 services/domain，具体 FTS 和向量查询由 repository 实现。

### DocumentIndex

代表 SQLite 中的一条文档索引投影，至少包含文档库相对路径、文件标识或修改时间、Frontmatter 可查询字段、可搜索正文和索引状态。DocumentIndex 可以被删除和重建，不是新的文档事实源。

### MarkdownDocument

这是 Markdown 文件的持久化表示，不等同于领域模型：

- `MarkdownDocument` 描述路径、正文和原始元数据；
- services 层把持久化表示转换为领域对象或结果 DTO；
- markdown 模块负责 Markdown/YAML 的具体读写。

## 5. 模块与依赖边界

目标源码结构：

```text
src/agentwiki/
├── cli/                 # CLI composition root、命令和输出适配
├── mcp/                 # MCP server、工具、资源和协议适配
├── domain/              # note、notePath、Frontmatter、SearchQuery、索引规则
├── services/            # 文档用例、搜索和索引同步业务流程
├── repository/          # SQLite、FTS5 和向量索引访问
├── indexing/            # 扫描、增量同步和索引重建
├── markdown/            # Markdown、Frontmatter、本地文档库文件系统
├── config.py            # 配置模型与配置读取
└── runtime/             # 文件监听、运行上下文和后台索引生命周期
```

依赖方向：

```text
cli/mcp composition roots
            ↓
        services
            ↓
     domain + contracts
            ↓
 repository / indexing
            ↓
      markdown / SQLite
```

边界规则：

- `domain` 不导入 Typer、FastMCP、Path 读写实现或具体配置管理器。
- `services` 只依赖领域类型和 repository/indexing 契约；它负责文档用例编排、操作顺序、索引同步和错误转换。
- `repository` 实现 SQLite、FTS5 和向量索引访问，不负责完整业务流程。
- `indexing` 实现 Markdown 文档扫描、增量同步和索引重建。
- `markdown` 实现 Markdown、Frontmatter 和本地文档库文件操作。
- `runtime` 只承载文件监听、运行上下文和后台索引生命周期；不承载文档业务规则。
- `cli`、`mcp` 不直接读写文档库，也不复制文档校验和搜索规则。
- 配置只在 composition root 读取一次，再显式传递给下游模块。

## 6. 工具契约

CLI 和 MCP 应共享以下六类应用能力。协议层可以调整参数命名和返回格式，但不能改变语义：

| 工具 | 语义 |
| --- | --- |
| `write_note` | 创建新文档；若产品规则允许，也可作为完整快照写入入口 |
| `read_note` | 按文档库相对路径读取单份文档 |
| `update_note` | 更新已有文档的正文、Frontmatter 或属性 |
| `delete_note` | 删除指定文档 |
| `move_note` | 将文档移动到文档库内的新相对路径 |
| `search_notes` | 使用关键词、语义或混合模式搜索并返回匹配文档 |

这些工具只负责文档操作、索引同步和搜索，不承载内容工作流或项目管理流程。

### 搜索实现

- **关键词查询**：使用 SQLite FTS5，对路径、标题、正文和可查询 Frontmatter 建立全文索引。
- **语义查询**：使用本地 embedding 模型生成查询和文档向量，并通过 SQLite 向量扩展进行近邻检索；语义依赖作为可选能力，不得阻塞基础文档操作。
- **混合查询**：分别取得关键词和语义候选，再由 services 层合并、去重和排序。
- **索引同步**：文档写入、修改、删除和移动成功后刷新对应索引；启动时支持全量扫描，外部 Markdown 变更支持增量同步或重新扫描。
- **降级策略**：没有可用 embedding 模型或向量扩展时，`search_notes` 仍支持关键词模式；索引不可用时返回明确错误，不伪造搜索结果。

## 7. 持久化与安全边界

- 所有文件路径必须解析并限制在文档库根目录内，拒绝路径穿越和越界软链接场景。
- 新建、更新、删除和移动操作必须经过统一 services 层。
- Markdown 文件写入成功后再更新 SQLite 派生索引；索引更新失败必须留下可重建状态，不能回滚或覆盖 Markdown 事实源。
- SQLite 使用本地文件和 WAL 等适合本地并发的配置；不引入 Postgres、远程数据库或云端索引服务。
- 删除是明确的文档操作，不应被入口层静默改写成其他状态转换。
- API key、password、token 等敏感字段禁止写入文档正文或 Frontmatter。
- Frontmatter 缺失或格式错误的文档仍是文件系统中的文档；读取和搜索流程应返回可定位的解析错误或降级结果。
- 文件系统错误、解析错误、路径错误和查询错误应保持可区分，入口层再转换为适合 CLI/MCP 的错误表示。

## 8. 阶段演进

### 当前基础阶段

- 稳定包名、配置入口和 CLI/MCP composition root；
- 建立 note、notePath、Frontmatter 和 SearchQuery 边界；
- 实现 `write_note`、`read_note`、`update_note`、`delete_note`、`move_note` 和 `search_notes`；
- 建立本地 SQLite 索引，支持 FTS5 关键词搜索、全量重建和基础增量同步；
- 为路径安全、Markdown/YAML 解析、文件写入、索引同步和关键词搜索建立测试。

### 后续增强阶段

- 增加本地 embedding 和 sqlite-vec 语义搜索，以及关键词/语义混合排序；
- 优化大型 Markdown 文档库的搜索索引、变更监测和结果分页；
- 增加文档冲突检测、并发写入保护和变更审计；
- 增加更多 Markdown 文档组织和批量操作能力。

新增能力时，应先更新文档领域边界和端口，再实现入口适配；不要直接把新规则添加到 CLI command 或 MCP tool 中。

## 9. 测试策略

- `domain`：测试路径值对象、Frontmatter 规则、搜索条件和错误边界。
- `services`：使用 fake contracts 测试六类文档用例的编排、依赖注入和失败传播。
- `repository`、`indexing`、`markdown`：测试 Markdown/YAML 解析、文档库路径边界、文件创建、更新、删除、移动、SQLite 索引、FTS 查询和索引重建。
- `semantic` 集成测试：在本地 embedding 和 sqlite-vec 可用时测试向量生成、语义检索和混合搜索；基础测试不得依赖模型下载或外部 API。
- `cli`、`mcp`：测试参数转换、调用正确用例和结果序列化，不重复测试领域规则。
- 真实外部服务测试使用 `integration` marker；纯单元测试保持确定、快速。

所有实现改动至少应通过：

```bash
uv run ruff check
uv run ty check
uv run pytest
git diff --check
```
