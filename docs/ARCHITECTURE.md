# AgentWiki 架构与迁移方案

## 1. 目标与当前状态

AgentWiki 是本地 Markdown Wiki 的检索与治理层，面向一万篇以内的个人或团队知识库。目标是少写、少维护通用代码，复用成熟组件，保持单 package 和嵌入式部署。

Markdown 正文和 YAML Frontmatter 是事实源；LanceDB 与 SQLite 都是派生投影。Agent 原生工具负责已知路径读取、文档创建、编辑、移动与删除。AgentWiki 提供检索、规则、校验，以及显式请求的格式修复。

HTTP API、云同步、Web UI、查询 LLM、实体自动抽取和操作审计不在范围内。无需独立数据库或模型服务，允许依赖使用原生库。

**本轮仅更新文档，以下组件、目录和行为是待迁移目标。** 当前实现按源码核对如下，不应以旧注释或接口名称推断功能完整：

| 部分 | 当前状态与差距 |
| --- | --- |
| CLI / 配置 | `src/cli.rs` 已有命令路由和配置加载；入口目前是同步函数，模型配置尚未用于推理 |
| Markdown / 规则 / 校验 | 已有手写扫描、切分、规则合并和部分校验；未接入目标解析、切分、格式化组件 |
| 同步 / SQLite / 图谱 | 已有同步流程和元数据表；扫描会读取全部文档，变化文档重复读取；移动计数恒为零，解析错误隔离、重试与关系解析仍需补齐 |
| 检索 | `src/tantivy_svc.rs` 写入和查询为占位；`src/search.rs` 未实现完整排名融合和近期检索 |
| MCP | `src/mcp.rs` 为提示信息占位，尚无可调用的三个工具 |
| 语义 / 格式修复 | 未实现 |

当前 Cargo 仍声明 Tantivy、lindera-tantivy、rust-mcp-sdk 等旧依赖。当前 Tantivy 0.26 的字段类型不包含旧设计假设的 `VecField`，后续不再按该假设接线。旧 Python 实现保留在 `legacy/python/`，不恢复维护。

## 2. 组件与工程工具链

| 职责 | 目标组件 | 自有代码边界 |
| --- | --- | --- |
| 全文 / 向量 / 混合 | LanceDB | 字段映射、查询约束、证据组织与降级 |
| 元数据 | rusqlite，bundled | 同步账本、文档信息、关系、失败记录 |
| 本地 embedding | FastEmbed | 输入构造、模型配置、缓存与错误处理 |
| Markdown | pulldown-cmark | 标题、链接、源位置的领域映射 |
| 章节切分 | text-splitter | 在标题章节内切分，保留章节路径 |
| YAML | serde-saphyr + serde | 直接反序列化规则类型和 JSON 兼容元数据 |
| 文件遍历 / glob | walkdir / globset | 安全路径与规则合并 |
| 格式化 | dprint-plugin-markdown | 检查、显式修复、安全写回 |
| CLI / MCP | clap / 官方 rmcp | 参数、协议、序列化；MCP SDK 由 mcp feature 隔离 |
| 异步 / 日志 / 错误 | tokio / tracing / thiserror + anyhow | 生命周期与系统边界上下文 |
| 路径 / 指纹 / 锁 | camino / sha2 / fs2 | UTF-8 路径、变更确认、跨进程写协调 |

LanceDB 承接 BM25、向量查询、过滤与 RRF；默认 ICU 分词，不自建中文分词适配器或通用融合算法。专业词和代码标识符效果通过固定语料验证，不能由组件支持推断召回质量。[FTS 配置](https://docs.rs/lancedb/latest/lancedb/index/scalar/struct.FtsIndexBuilder.html)、[RRF](https://docs.rs/lancedb/latest/lancedb/rerankers/rrf/struct.RRFReranker.html)

FastEmbed 首个支持 `BAAI/bge-small-zh-v1.5`，适配其模型枚举和资源，不自行实现模型分词、池化或推理。关闭模型时不初始化推理资源；启用后按需准备本地缓存，准备完成后离线运行。[FastEmbed](https://docs.rs/fastembed/latest/fastembed/)

Markdown 标题、标准链接和 Wiki 链接使用解析器事件，不自行用字符串扫描替代解析。超长章节交给切分器；保留章节面包屑、源位置及原文证据，不手写滑动窗口。[解析选项](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.Options.html)、[切分器](https://docs.rs/text-splitter/latest/text_splitter/)

配置字段少时直接校验；移除 validator、watcher 和其他没有实际消费者的预留依赖。保留 Cargo、rustfmt、Clippy、Rust 测试和 tempfile；rstest、insta、criterion 仅在实际测试或基准需要时保留。不新增 ORM、任务编排框架、通用 Repository 或插件系统。

edition 保持 2024，MSRV 保持 1.98。具体依赖版本及 feature 在源码迁移时按兼容性解析，交由 Cargo 更新锁文件；本轮不宣称新组件已通过本项目编译或性能验证。

## 3. 目标目录与依赖

目录随对应功能迁移创建，不提前声明空模块：

```text
src/
├── lib.rs                 # 公共 API 与模块声明
├── config.rs              # 配置加载、默认值、路径解析
├── runtime.rs             # 唯一资源所有者与公共用例入口
├── error.rs               # 统一错误类型
├── bin/
│   ├── agentwiki.rs       # CLI
│   └── agentwiki-mcp.rs   # MCP，mcp feature
├── document/
│   ├── mod.rs             # Document、Chunk、读取入口
│   ├── path.rs            # 受约束相对路径、安全检查、扫描
│   ├── parse.rs           # Frontmatter、标题、链接、源位置
│   └── chunk.rs           # 章节归属、切分器接线
├── retrieval/
│   ├── mod.rs             # 查询/结果类型、策略、证据组织
│   ├── index.rs           # LanceDB 唯一边界
│   └── embedding.rs       # FastEmbed 唯一边界
├── governance/
│   ├── mod.rs             # 规则获取、校验与修复入口
│   ├── rules.rs           # 规则类型、解析、匹配、合并
│   ├── validate.rs        # 问题类型与确定性检查
│   └── format.rs          # dprint 接线与显式格式写回
├── sync.rs                # 增量同步、重建、重试
├── storage.rs             # SQLite 元数据
└── graph.rs               # 一跳文档关系

tests/
├── retrieval.rs
├── sync.rs
├── governance.rs
└── fixtures/
```

单元测试留在模块内，公共跨模块行为放在 tests，固定小型 Markdown 文档放在 fixtures。文档继续使用现有 README、AGENTS 和 docs 文件，不新增平行设计入口。

```text
CLI / MCP → Runtime
              ├─ sync → document / graph / retrieval / storage
              ├─ retrieval → LanceDB / FastEmbed，附加 SQLite 关系
              └─ governance → document / rules / format
```

- Runtime 统一持有 Wiki 根目录、索引、元数据连接、可选模型和同步协调资源；移除 SyncContext 的重复装配。
- sync 接收明确资源引用；检索与治理不接收整个 Runtime，也不依赖同步上下文。
- LanceDB/Arrow 类型只出现在 retrieval/index，FastEmbed 类型只出现在 embedding，SQLite 连接只出现在 storage。
- 删除全局 model；Document、Chunk 属于 document，查询/结果属于 retrieval，规则/问题属于 governance，SyncReport 属于 sync。领域类型不依赖第三方 I/O 或协议 SDK。
- lib 只导出实际调用者需要的公共 API，不全量公开内部模块或数据库行结构。
- graph 复用解析出的链接和章节，validate 复用同一文档表示，不重复扫描或解析。
- config 只在入口加载，业务模块接收已解析参数；同步文件 I/O、SQLite 和模型推理不阻塞 Tokio executor，阻塞任务需限制并发并等待完成。

## 4. 数据与运行行为

### 4.1 规则与配置

Wiki 根目录的 `AGENTWIKI.md` 是唯一组织规则入口：Frontmatter 是结构化规则，正文作为 guide_content 返回，不作为普通文档索引。Runtime 创建缺失模板，已存在文件不覆盖。模板自举与显式格式修复是应用写 Markdown 的两个限定场景。

配置继续使用 `~/.agentwiki/config.json`，仅保留 wiki_root 和 embedding_model。默认根目录 `~/AgentWiki`，模型默认 null；CLI 显式根目录优先于配置，配置相对路径以配置目录为基准。业务不读取环境变量配置，第三方缓存和模型路径尽量通过构造参数传递。

目标数据布局：

```text
~/.agentwiki/
├── config.json
├── models/                       # 本地模型缓存
└── indexes/<wiki-root-hash>/      # 规范化绝对 Wiki 根目录的 SHA-256
    ├── lancedb/
    ├── agentwiki.sqlite3
    └── sync.lock
```

先创建并规范化根目录，再确定隔离键。现有共享投影不直接复用，迁移后按根目录重新建立；旧投影清理由用户显式执行，不自动删除未知目录。Markdown 和规则内容无需迁移。

### 4.2 增量同步与恢复

1. 查询前收集相对路径、真实 mtime_ns 和大小，与账本筛选变化；失败文档也必须进入重试判断，不能被全局 generation 跳过。
2. 疑似变化文档读取一次并计算内容哈希；内容相同只更新文件指纹，不重复解析和生成向量。
3. 变化内容解析一次，供片段、元数据、关系和校验复用。解析失败保留该文档已有有效投影，记录路径和错误；不得将读取失败当成文件删除。
4. 唯一内容哈希配对的删除与新增识别为移动，保留文档身份；有歧义则按增删处理。
5. 更新关键词、文档信息和关系；向量按模型身份和实际输入哈希复用或同步批量计算。模型身份包含适配后的版本和维度，变更时旧向量失效。
6. 元数据账本只有在相应投影成功后才确认。语义失败不撤销关键词投影，保留独立失败依据，下次同步重试。

SQLite 不做全文或向量检索；LanceDB 与 SQLite 没有跨库事务。写操作由 fs2 跨进程锁协调，部分完成操作必须幂等重试。全局 generation 不能掩盖部分失败。查询不消费未确认的新旧混合状态；失败保留的旧证据必须附带诊断。

不维护后台向量队列、watcher、pending 恢复或独立向量 manifest。首次和变更查询允许等待同步向量批处理；计算失败报告降级，进程退出后通过哈希与失败记录再次同步。

外部修改走增量路径，rebuild 和索引恢复才全量重建。投影格式不兼容时关闭资源、在锁内重建，不进行文档数据迁移。LanceDB 索引整理随批量同步或显式维护调用完成；未并入索引的数据仍须可查，不能为速度隐式返回过期结果。[索引更新机制](https://docs.lancedb.com/search/full-text-search)

### 4.3 检索与证据

普通查询使用精确匹配、BM25 和可选语义检索；词法与语义混合交给 LanceDB RRF。应用只保留精确项优先、近期意图、文档片段限额及证据组织等产品策略，不建立通用排名框架。

候选查询尽早施加 scope、tags、note_types 和 metadata_filters，过滤数据不能直接拼接未经验证的查询表达式。限制每篇最多两个片段，并保证章节很多的单篇文档不会耗尽整个候选池；必要时有界补取候选。

空查询按真实文件修改时间返回近期文档；明确近期主题查询加入新近度，普通主题查询不施加时间偏置。limit 默认 10、范围 1..20，单条证据最多五条一跳关系，具体接口见 MCP 契约。

关系仅从标准内部 Markdown 链接、Wiki 链接和 Frontmatter relations 派生，不自动抽取实体。保留方向、来源章节、原文上下文；目标缺失保留 unresolved，目标出现或删除时重新解析。越界目标拒绝并报告。

正常无匹配、主动关闭语义不是故障。启用模型但不可用、投影失败、关系声明非法等进入 degraded；实际命中来源进入 match_sources，rank_score 不代表概率或跨查询可比较的置信度。路径非法或请求不合法返回错误，不伪装为空结果。

语义阈值按模型和实际嵌入文本在固定语料中标定，包含标题、标签、章节和正文。不得沿用旧 Python 实验阈值，也不宣称单阈值能可靠分离主题相邻的无答案查询。

### 4.4 校验与显式格式修复

规则合并、标签建议和格式定义以 RULES 为准，协议以 MCP_TOOLS 为准。默认只报告；fix_format=true 才允许改写请求范围内的格式，并在写回后重新校验。

采用内置 dprint，保留 Frontmatter 原文和代码块内部，不修正标签、链接、标题语义或业务内容。无格式变化不写回，不触发无意义的修改时间变化。

修复必须安全解析路径，拒绝跨根目录和外部符号链接；写回前核对原内容和文件指纹，变化则跳过并报告冲突。使用同目录临时文件替换，保留权限。此方式检测已观察到的并发修改，不宣称能锁住不配合的外部编辑器。

单文件修复指定 path；全库修复显式 full=true，两者互斥。默认全库不格式化规则文件，防止自动改变组织指引；规则解析错误仍报告。格式修复后使用文件新状态，下一次查询按增量同步更新投影。

## 5. 迁移映射与验收

| 当前实现 | 目标迁移 |
| --- | --- |
| src/cli.rs、src/mcp.rs | bin 两个入口；Tokio 与官方 rmcp 接线 |
| markdown.rs | document；现成解析、扫描、切分；统一读取结果 |
| search.rs、tantivy_svc.rs | retrieval；LanceDB 替代旧检索占位，新增 FastEmbed 适配 |
| rules.rs、validate.rs | governance；globset、dprint 与格式修复 |
| model.rs | 类型按功能归属分散，删除集中模型文件 |
| runtime.rs、sync.rs | 单一资源所有权、借用资源的同步编排 |
| storage.rs、graph.rs | 保留 SQLite 与关系职责，删除独立向量 manifest，补齐失败和关系解析 |

后续整体迁移还需更新 Cargo.toml/Cargo.lock、源码 rustdoc、源码内 DEFAULT_AGENTWIKI、CLI 帮助和测试；不能仅移动文件后保留旧算法与旧承诺。规则示例与代码内精简模板用途不同，默认模板不直接替换成完整示例。

验收场景：

- 中文专名、中英混合、代码标识符、精确路径、语义改写、过滤、近期及无答案查询；同时报告文档与章节召回，比较关键词和混合基线。
- 增改删移、重复哈希移动歧义、mtime 抖动、未变化免解析、单篇解析失败、部分投影失败重试、模型切换与不可用、跨进程同步和损坏重建。
- 默认校验不写文件；格式化幂等；Frontmatter、代码块和链接语义保持；冲突不覆盖，路径越界拒绝，修复后重新校验。
- CLI/MCP 使用相同默认值和业务入口；核心库无需 mcp feature，MCP 入口单独编译验证。
- 在固定语料记录构建、启动、增量同步、查询延迟、内存与召回。明确设备、模型、文档数和片段数，不承诺未经测量的性能。

本轮文档验收只检查内容一致、链接、当前/目标标识及 git diff --check，不修改源码或依赖。代码迁移完成后执行 AGENTS 中的 Cargo 检查及相应 feature 测试。
