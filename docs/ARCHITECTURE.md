# AgentWiki 架构与迁移方案

## 1. 目标与当前状态

AgentWiki 是本地 Markdown Wiki 的检索与治理层，面向一万篇以内的个人或团队知识库。目标是少写、少维护通用代码，复用成熟组件，保持单 package 和嵌入式部署。

Markdown 正文和 YAML Frontmatter 是事实源；LanceDB 是唯一派生投影。Agent 原生工具负责已知路径读取、文档创建、编辑、移动与删除。AgentWiki 提供检索、规则、校验，以及显式请求的格式修复。

HTTP API、云同步、Web UI、查询 LLM、实体自动抽取和操作审计不在范围内。无需独立数据库或模型服务，允许依赖使用原生库。

**本轮已完成基础迁移，以下表格区分已落地能力与后续增量：**

| 部分 | 当前状态与差距 |
| --- | --- |
| CLI / 配置 | `src/cli.rs` 已有命令路由和配置加载；模型配置可驱动可选语义索引 |
| Markdown / 规则 / 校验 | 已接入 pulldown-cmark、text-splitter、globset、dprint；格式修复仍需显式 `--fix-format` |
| 同步 / LanceDB | 文件指纹、向量输入哈希和投影格式与检索行共同保存在 LanceDB |
| 检索 / 图谱 | LanceDB 统一承接文档与片段、结构化过滤、精确匹配、关系、FTS 和可选向量查询 |
| MCP | `src/mcp.rs` 已用官方 MCP SDK 提供三个 stdio 工具 |
| 语义 / 格式修复 | FastEmbed 已接入同步与 LanceDB 向量投影；默认关闭模型，格式修复提供 CLI 显式入口 |

LanceDB 的本地构建需要 `protoc`，CI 与开发环境应预装并通过 `PROTOC` 指定。

## 2. 组件与工程工具链

| 职责 | 目标组件 | 自有代码边界 |
| --- | --- | --- |
| 全文 / 向量 / 混合 | LanceDB | 字段映射、查询约束、证据组织与降级 |
| 投影账本 | LanceDB document 行 | 文件指纹、向量输入哈希和投影格式；与检索数据同一版本 |
| 本地 embedding | FastEmbed | 输入构造、模型配置、缓存与错误处理 |
| Markdown | pulldown-cmark | 标题、链接、源位置的领域映射 |
| 章节切分 | text-splitter `MarkdownSplitter` | 按标题归属分段，段内按 Markdown 语义边界切分 |
| YAML | serde_yaml + serde | 直接反序列化规则类型和 JSON 兼容元数据 |
| 文件遍历 / glob | 标准库 / globset | 安全路径与规则合并 |
| 格式化 | dprint-plugin-markdown | 检查、显式修复、安全写回 |
| CLI / MCP | clap / 官方 MCP SDK | 参数、协议、序列化；MCP SDK 由 mcp feature 隔离 |
| 异步 / 错误 | tokio / thiserror + anyhow | 生命周期与系统边界上下文 |
| 路径 / 指纹 / 锁 | camino / sha2 / fs2 | UTF-8 路径、变更确认、跨进程写协调 |

LanceDB 承接 FTS、向量查询、过滤与原生 RRF；当前 FTS 使用 Lance 默认 tokenizer，不在应用层自建分词器或通用融合算法。中文专名、混合文本和代码标识符的效果必须用固定语料实测，不能由组件支持本身推断召回质量。[FTS 配置](https://docs.rs/lancedb/latest/lancedb/index/scalar/struct.FtsIndexBuilder.html)、[RRF](https://docs.rs/lancedb/latest/lancedb/rerankers/rrf/struct.RRFReranker.html)

FastEmbed 首个支持 `BAAI/bge-small-zh-v1.5`，适配其模型枚举和资源，不自行实现模型分词、池化或推理。关闭模型时不初始化推理资源；启用后按需准备本地缓存，准备完成后离线运行。[FastEmbed](https://docs.rs/fastembed/latest/fastembed/)

Markdown 标题、标准链接和 Wiki 链接使用解析器事件，不自行用字符串扫描替代解析。超长章节交给切分器；保留章节面包屑、源位置及原文证据，不手写滑动窗口。[解析选项](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.Options.html)、[切分器](https://docs.rs/text-splitter/latest/text_splitter/)

配置字段少时直接校验；移除 validator、watcher 和其他没有实际消费者的预留依赖。保留 Cargo、rustfmt、Clippy、Rust 测试和 tempfile；不为未落地的测试或基准能力预留依赖。不新增 ORM、任务编排框架、通用 Repository 或插件系统。

edition 保持 2024，依赖与 feature 以 Cargo.toml 为准；本轮不宣称新组件已通过本项目编译或性能验证。

LanceDB 的 Rust 依赖链会编译 Protocol Buffers schema，开发机和 CI 需要预装 `protoc`（按平台安装并确认 `protoc` 可执行）。仓库不携带或提交该工具二进制；缺失时构建应给出明确环境错误。

## 3. 目标目录与依赖

目录随对应功能迁移创建，不提前声明空模块：

```text
src/
├── lib.rs                 # 公共 API 与模块声明
├── app.rs                 # AgentWiki 异步门面、资源和并发控制
├── config.rs              # 配置加载、默认值、路径解析
├── error.rs               # 统一错误类型
├── cli.rs                 # agentwiki 二进制入口
├── mcp.rs                 # agentwiki-mcp，mcp feature
├── document/
│   ├── mod.rs
│   ├── types.rs           # 文档、路径、指纹、切片和关系
│   ├── path.rs            # 受约束相对路径、安全检查、扫描
│   ├── parse.rs           # Frontmatter、标题、读取
│   ├── chunk.rs           # 章节归属、切分器接线
│   └── relation.rs        # 显式一跳关系提取
├── projection/
│   ├── mod.rs             # 可重建投影资源束
│   ├── types.rs           # 同步报告
│   ├── sync.rs            # 增量同步、重建、重试
│   ├── lance.rs           # LanceDB 唯一边界
│   └── embedding.rs       # FastEmbed 唯一边界
├── retrieval/
│   ├── mod.rs
│   ├── types.rs           # 查询与结果契约
│   └── search.rs          # 融合策略与证据组织
├── governance/
│   ├── mod.rs
│   ├── types.rs           # 规则与校验契约
│   ├── rules.rs           # 解析、匹配、合并
│   ├── validate.rs        # 问题类型与确定性检查
│   └── format.rs          # dprint 接线与显式格式写回

tests/
├── retrieval.rs
├── sync.rs
├── governance.rs
└── fixtures/
```

单元测试留在模块内，公共跨模块行为放在 tests，固定小型 Markdown 文档放在 fixtures。文档继续使用现有 README、AGENTS 和 docs 文件，不新增平行设计入口。

```text
CLI / MCP → AgentWiki
              ├─ projection → document / LanceDB / FastEmbed
├─ retrieval → projection，所有读取由 LanceDB 完成
              └─ governance → document / rules / format
```

- AgentWiki 统一持有 Wiki 根目录和一个 Projection 资源束；Projection 只在 AgentWiki 装配时创建。
- 用例接口为 async；LanceDB 使用原生异步 API，FastEmbed、格式化和同步解析通过阻塞任务隔离。
- LanceDB/Arrow 和 FastEmbed 类型分别限制在 projection/lance、embedding。
- 类型按使用域组织，不建立中央 model 模块；领域类型不依赖协议 SDK。
- lib 只导出实际调用者需要的公共 API，不全量公开内部模块或数据库行结构。
- relation 复用解析出的链接和章节，validate 复用同一文档表示，不重复扫描或解析。
- config 只在入口加载，业务模块接收已解析参数；同步文件 I/O 和模型推理不阻塞 Tokio executor，阻塞任务需限制并发并等待完成。

## 4. 数据与运行行为

### 4.1 规则与配置

Wiki 根目录的 `AGENTWIKI.md` 是唯一组织规则入口：Frontmatter 是结构化规则，正文作为 guide_content 返回，不作为普通文档索引。AgentWiki 创建缺失模板，已存在文件不覆盖。模板自举与显式格式修复是应用写 Markdown 的两个限定场景。

配置继续使用 `~/.agentwiki/config.json`，仅保留 wiki_root 和 embedding_model。默认根目录 `~/AgentWiki`，模型默认 null；CLI 显式根目录优先于配置，配置相对路径以配置目录为基准。业务不读取环境变量配置，第三方缓存和模型路径尽量通过构造参数传递。

目标数据布局：

```text
~/.agentwiki/
├── config.json
├── models/                       # 本地模型缓存
└── lancedb/<wiki-root-hash>/      # 规范化绝对 Wiki 根目录的 SHA-256
    └── sync.lock
```

先创建并规范化根目录，再确定隔离键。现有共享投影不直接复用，迁移后按根目录重新建立；旧投影清理由用户显式执行，不自动删除未知目录。Markdown 和规则内容无需迁移。

### 4.1.1 LanceDB 存储结构

当前投影只有一张业务表 `wiki_rows`，不再维护 retrieval、vector、relation 三张表。表中的每一行由 `chunk_id` 唯一标识，`unit_kind` 区分三种行：

| 行类型 | `unit_kind` | 作用 | 主要字段 |
| --- | --- | --- | --- |
| 文档 | `document` | 文档级发现、最近修改、Frontmatter 返回和同步指纹 | `path`、`type`、`tags`、`facets`、`lookup_keys`、`frontmatter_json`、`content_hash`、`source_size`、`modified_at_ns` |
| 切片 | `fragment` | 章节级证据召回 | `path`、`chunk_id`、`section`、`content`、`tags`、`facets`、`ordinal` |
| 关系 | `relation` | Markdown 显式的一跳关系 | `path`、`target_path`、`relation_type`、`source_section` |

所有行共享以下投影字段：

| 字段 | 类型/约束 | 设计意图 |
| --- | --- | --- |
| `path` | Utf8 | 文档和切片为来源文档；关系为来源文档，保证按文档整组替换 |
| `chunk_id` | Utf8 | 文档、切片和关系的稳定 merge key；由路径、类型、序号或关系内容确定性生成 |
| `type` | Utf8 | Frontmatter 的文档分类，如 `profile`、`events`、`skills`、`cases` |
| `tags` | `List<Utf8>` | 归一化标签，支持全部满足的集合过滤 |
| `facets` | `List<Utf8>` | 其他 Frontmatter 等值条件，编码为 `key=canonical-json`，避免为动态字段扩展 Schema |
| `lookup_keys` | `List<Utf8>` | 规范化相对路径、文件名和 aliases，支持精确匹配 |
| `section` / `content` | Utf8 | 结果展示和全文检索输入；文档标题由 Frontmatter `title`（若有）或文件名推导，不单独存列 |
| `frontmatter_json` | Utf8 | 仅 document 行保存完整 Frontmatter，供结果水合；aliases 等原始字段不拆列 |
| `modified_at_ns` | Int64 | 真实文件 mtime，用于 recent 和显式时间过滤 |
| `ordinal` | Int32 | 切片在来源文档中的顺序 |
| `content_hash` / `source_size` | Utf8 / Int64 | document 行的原文同步指纹 |
| `vector_input_hash` | Utf8 | embedding 模型身份与实际输入的哈希；空值表示向量尚未成功生成 |
| `vector` | nullable `FixedSizeList<Float32, 512>` | document/fragment 的可选语义向量；relation 行为空 |

`search_text` 是当前 Lance FTS 的统一词法输入，由路径、文件名、可选 title、aliases、摘要、标签、大纲以及切片章节正文构成；它不是 Markdown 的事实字段。关系行的文本字段为空，只参与结构化关系查询，不进入文档和切片检索。title、aliases 保留在 document 行的 `frontmatter_json` 中，归一化后同时写入 `search_text` 和 `lookup_keys`，不再建立独立列。

索引布局如下：

| 字段 | LanceDB 索引 | 查询场景 |
| --- | --- | --- |
| `path`、`chunk_id`、`target_path`、`modified_at_ns` | BTree | 路径、关系目标和时间 |
| `unit_kind`、`type`、`relation_type` | Bitmap | 文档/切片/关系及分类过滤 |
| `tags`、`facets`、`lookup_keys` | LabelList | 集合过滤、Frontmatter 等值过滤、精确文件名/路径/alias |
| `search_text` | FTS | 关键词、显式 keywords 和 Hybrid 的词法腿 |
| `vector` | Lance 向量查询 | 可选语义和 Hybrid 查询 |

`source_size` 只参与同步指纹比较，不做等值过滤，因此不建索引。表 schema 携带 `agentwiki_projection_version` metadata；打开时版本或 schema 不匹配只丢弃并重建已知的 `wiki_rows` 派生表，不触碰其他表、不迁移文档数据。

这种结构将“返回文档所需的数据”“检索所需的数据”和“增量同步所需的指纹”放在同一版本的同一张表中；Markdown 仍然是唯一事实源，表可随时删除并从 Markdown 重建。

### 4.2 增量同步与恢复

1. 查询前收集相对路径、真实 mtime_ns 和大小。stat 未变化直接跳过，不读取也不解析；变化或向量缺失的文档进入后续步骤。
2. 读取一次原始字节并计算内容哈希。哈希与已存指纹相同则不解析、不重新生成向量，只把新的 mtime 和大小写回该路径的全部检索行，使文档与片段的时间视图一致。
3. 变化内容解析一次，供片段、元数据、关系和校验复用。解析失败保留该文档已有有效投影，记录路径和错误；不得将读取失败当成文件删除。读取前后核对 mtime 和大小，确认拿到一致快照，并限制单文档大小。
4. 唯一内容哈希配对的删除与新增识别为移动，保留文档身份；有歧义则按增删处理。
5. 更新关键词、文档信息和关系；向量按 `vector_input_hash` 逐切片复用，只对输入真正变化的切片按固定批次同步推理。哈希包含模型身份、维度和实际嵌入输入，任一变化都使旧向量失效。
6. 每个文档的 document、fragment、relation 行通过一次 `merge_insert` 提交；语义失败仍提交词法字段、记录 `vector_input_hash` 并将向量留空，下次同步重新构造该文档投影并重试向量。

读写规则查询也确认文档投影新鲜度，但不触发 embedding 推理：规则只需要最新的标签与文档元数据，向量留给下一次检索补偿。模型在首次实际需要推理时才加载，未启用或未使用的进程不准备推理资源。

LanceDB 统一保存 document/fragment、结构化过滤字段、显式关系和同步指纹；单文档通过 `merge_insert` 一次提交，查询统一针对当前 `wiki_rows` 表。写操作由 fs2 跨进程锁协调，部分完成操作必须幂等重试。

不维护后台向量队列、watcher、pending 恢复或独立向量 manifest。首次和变更查询允许等待同步向量批处理；计算失败报告降级，进程退出后通过哈希与失败记录再次同步。

外部修改走增量路径，rebuild 和索引恢复才全量重建。投影格式不兼容时在锁内重建 `wiki_rows`，不进行文档数据迁移。索引在进程打开投影时核对并补齐，中断的建索引可重试；新增行不自动进入既有索引，查询仍会合并已索引和未索引数据，因此结果不会因未 optimize 而缺失。把新增行并入索引是显式维护动作：`rebuild-index` 在重建后执行，或由 `optimize-index` 单独触发，查询路径不隐式 optimize。未并入索引的数据仍须可查，不能为速度隐式返回过期结果。[索引更新机制](https://docs.lancedb.com/search/full-text-search)

### 4.3 检索与证据

检索内核不解释 `profile`、`events`、`skills` 等文档类型的业务语义；场景选择、查询组装、权限与范围判断由 Agent 按 `AGENTWIKI.md` 完成。内核只忠实执行显式查询：路径范围、类型、标签、扩展 Frontmatter 等值条件、修改时间、关键词模式、排序方式和关系开关。

每篇文档固定生成一个 document 检索单元；正文按 Markdown 标题生成零到多个 fragment 单元，超长章节再用 `MarkdownSplitter` 按段落、列表项或代码块等语义边界切分。类型不参与切片策略。document 的检索文本由路径、标题、别名、摘要、标签和标题大纲组成；fragment 额外包含章节与正文。两类单元分别召回、融合和限额，避免文档发现与局部证据竞争同一候选池。

普通查询先把 `query` 或显式 `keywords` 编译为 Lance FTS 查询；有向量时在同一次 Lance 查询中组合 FTS 与向量，并交给内置 `RRFReranker` 排序。精确路径、标题和 alias 通过 `lookup_keys` 单独过滤并置顶，再与 Hybrid 结果去重。显式 `keywords` 是全局词法硬约束：any 要求同一检索单元命中任一关键词，all 要求命中全部，精确匹配和语义候选都不能绕过；`query` 只用于在同一约束内召回和排序。内核不从自然语言查询推导关键词，不自动扩展标签、类型或关系，也不根据“最近”等措辞猜测排序意图。

`order=modified_desc` 时结果集由词法匹配定义，先按真实文件 mtime 排序再取限额，因此返回的是全部匹配项中最新的若干条，而不是相关候选内部的最新；该模式不加入未经阈值标定的纯语义候选。顺序敏感查询需要扫描全部匹配行，代价随匹配规模增长，语料扩大后按基准决定是否需要专门的执行路径。

scope、tags、note_types、metadata_filters 和修改时间在 LanceDB 的 FTS/向量候选生成前施加；空 query、精确 path/filename/alias、最近修改、known_tags 和 Frontmatter 水合也查询 LanceDB。tags、facets 和 `lookup_keys` 使用 `List<Utf8>` 与 LabelList 索引，类型和单元类型使用 Bitmap，路径与时间使用 BTree。scope 按字面路径前缀匹配，`_`、`%` 等字符不作通配符；过滤值必须转义，不能拼接未经验证的检索表达式。

document_limit 默认 5，fragment_limit 默认 10，范围均为 1..20。关系默认关闭；显式开启后只返回最多五条一跳边及声明上下文，不自动读取目标文档或扩展多跳。具体接口见 MCP 契约。

关系仅从标准内部 Markdown 链接、Wiki 链接和 Frontmatter relations 派生，不自动抽取实体。保留方向、来源章节、原文上下文；目标缺失保留 unresolved，目标出现或删除时重新解析。越界目标拒绝并报告。

正常无匹配、主动关闭语义不是故障。启用模型但不可用、投影失败、关系声明非法等进入 degraded；实际命中来源进入 match_sources：来源由每条结果实际参与的检索腿决定，不按启用配置推断，Hybrid 复用 Lance 原生 RRF 排序，只把两路成员关系作为证据附加，不自行实现融合算法。rank_score 是本次查询的原生排序分数，不代表概率或跨查询可比较的置信度。路径非法或请求不合法返回错误，不伪装为空结果。

语义阈值按模型和实际嵌入文本在固定语料中标定，包含标题、标签、章节和正文。当前实现不对语义距离做阈值判断，语义腿只参与排序和证据来源；因此关键词召回保持可用，无答案查询由词法匹配决定。需要按相似度过滤时必须先用固定语料标定，不得沿用未经当前实现验证的历史阈值，也不宣称单阈值能可靠分离主题相邻的无答案查询。

切片预算按模型 token 上限反推，不按字符经验值设定。`bge-small-zh-v1.5` 的 tokenizer 由 FastEmbed 固定装载 512 token 截断，超限部分从尾部静默丢弃；该模型的 WordPiece 词表保证每个 token 至少覆盖一个输入字符，因此字符数是 token 数的上界，可用字符预算代替 token 计数。切分参数如下：

| 参数 | 值 | 依据 |
| --- | --- | --- |
| 模型上限 | 512 token | FastEmbed 默认 `max_length`，超出即截断 |
| 安全余量 | 16 字符 | 特殊 token 与估算余量 |
| 正文下限 / 上限 | 96 / 448 字符 | 下限保证病态前缀下仍有正文；上限避免短前缀产生超大单元 |
| 非正文预算 | 512 − 16 − 96 − 1 = 399 字符 | path、标题、章节路径、摘要、标签的总和 |
| 单字段上限 | 240 字符 | 防止单个膨胀字段挤占其他字段 |
| overlap | 预算的 1/8 | 恒定小于容量，无需额外校验 |

任何单元的 `search_text` 因此满足「非正文 ≤ 399 且正文 ≤ 96..448」，总字符数不超过 496，即不超过 512 token。超长摘要按字符边界截断保留起始部分而不是整段丢弃，使文档级检索仍然命中；完整摘要仍保存在 `frontmatter_json` 并原样返回给 Agent，丢失的只是检索索引内的尾部。文档单元的标题大纲逐条累加标题，只丢弃放不下的尾部标题，不会把标题截成半截。

切片器使用 `MarkdownSplitter`，切点按 字符 → 字素 → 词 → 句子 → 软换行 → 行内元素 → 块元素（段落、代码块、列表项）→ 标题 的语义层级由细到粗优先选择，因此只在单个段落自身超过预算时才会落进段落内部；能装下的代码块和列表项保持完整。这一层不依赖模型：切片是文档层职责，不在其中引入 embedding 依赖，因此开启或关闭模型不会改变单元内容与 `chunk_id`。

若未来更换为字节级 BPE 词表模型，一个字符可能对应多个 token，字符上界不再成立，必须改用 tokenizer 作为 sizer 并重新标定以上参数。

### 4.4 校验与显式格式修复

规则合并、标签建议和格式定义以 RULES 为准，协议以 MCP_TOOLS 为准。`type`、`tags`、`summary` 是内置必填字段，其他必填字段由规则声明。默认只报告；fix_format=true 才允许改写请求范围内的格式，并在写回后重新校验。

采用内置 dprint，保留 Frontmatter 原文和代码块内部，不修正标签、链接、标题语义或业务内容。无格式变化不写回，不触发无意义的修改时间变化。

修复必须安全解析路径，拒绝跨根目录和外部符号链接；写回前核对原内容和文件指纹，变化则跳过并报告冲突。使用同目录临时文件替换，保留权限。此方式检测已观察到的并发修改，不宣称能锁住不配合的外部编辑器。

单文件修复指定 path；全库修复显式 full=true，两者互斥。默认全库不格式化规则文件，防止自动改变组织指引；规则解析错误仍报告。格式修复后使用文件新状态，下一次查询按增量同步更新投影。

## 5. 迁移映射与验收

| 当前实现 | 结构说明 |
| --- | --- |
| cli.rs、mcp.rs | 两个异步二进制入口；参数、协议和输出接线 |
| document/* | 文档类型、解析、安全路径、切分与关系提取 |
| projection/* | 同步编排、LanceDB 与 FastEmbed 边界 |
| retrieval/* | 查询契约、候选融合与证据组织 |
| governance/* | 规则契约、globset、dprint、校验与格式修复 |
| app.rs | AgentWiki 单次装配 Projection，并提供异步用例接口 |

功能域目录与异步资源边界已经落地；后续只针对模型缓存策略和检索质量做增量演进。规则示例与代码内精简模板用途不同，默认模板不直接替换成完整示例。

CLI 维护入口：`sync-index` 增量同步，`rebuild-index` 全量重建并刷新索引，`optimize-index` 只把新增行并入既有索引。查询路径不执行 optimize。

验收场景：

- 中文专名、中英混合、代码标识符、精确路径、语义改写、过滤、近期及无答案查询；同时报告文档与章节召回，比较关键词和混合基线。
- scope 含 `_`、`%` 等字符时只匹配字面路径；`keywords` 的 any/all 约束不被 query 或语义候选绕过；`modified_desc` 返回全部匹配项中最新的结果；match_sources 反映每条结果实际参与的检索腿。
- 增改删移、重复哈希移动歧义、mtime 抖动、未变化免解析、同内容只改 mtime 后文档与片段时间一致、单篇解析失败、部分投影失败重试、模型切换与不可用、向量按输入哈希复用、跨进程同步、中断的建索引重试和投影版本不兼容重建。
- 默认校验不写文件；格式化幂等；Frontmatter、代码块和链接语义保持；冲突不覆盖，路径越界拒绝，修复后重新校验。
- CLI/MCP 使用相同默认值和业务入口；核心库无需 mcp feature，MCP 入口单独编译验证。
- 在固定语料记录构建、启动、增量同步、查询延迟、内存与召回。明确设备、模型、文档数和片段数，不承诺未经测量的性能。
- 用真实 tokenizer 校验切片预算：既有语料生成的每个 `search_text` 都不超过 512 token，且长摘要在截断后仍能被文档级查询命中。

本轮文档验收只检查内容一致、链接、当前/目标标识及 git diff --check，不修改源码或依赖。代码迁移完成后执行 AGENTS 中的 Cargo 检查及相应 feature 测试。
