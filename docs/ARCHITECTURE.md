# AgentWiki 架构

## 1. 产品边界

AgentWiki 是本地 Markdown Wiki 的搜查与治理层。它负责：

- 为 Agent 检索历史、约定、已有方案、关联内容和近期变化；
- 使用 Tantivy 关键词、可选本地 embedding 和排名融合返回章节级证据；
- 提供目录组织规则，并在原生文件修改后执行确定性校验；
- 保持 SQLite 投影可删除、可增量同步和可全量重建。

Agent 原生工具负责已知路径读取、创建、编辑、移动和删除。HTTP API、云同步、Web UI、
查询 LLM 和操作审计不属于当前范围。图谱仅表示 Markdown 中明确声明的文档关系。

## 2. 数据与检索

Markdown 正文和 YAML Frontmatter 是事实源。Wiki 根目录的 `AGENTWIKI.md` 是唯一的保留治理
文件：Frontmatter 承载结构化规则，正文作为 `guide_content` 返回，该文件不进入普通索引。
除此之外所有 `*.md` 都是普通文档。

运行时装配（`runtime` 模块的 Runtime 构造器）在启动时检查该文件：缺失则写入随包分发的默认模板，
已存在则永不覆盖。因此新建 Wiki 立刻拥有可用规则，
而手写规则不会在任何一次启动中被回退。该初始化是启动期唯一的写操作，不参与索引事务。

索引保存：

- 文档路径、标题、Frontmatter、真实 `mtime_ns` 和大小；
- 文档稳定身份、内容 checksum、同步状态和失败原因；
- 按 Markdown 标题层级切分的有界片段；
- 片段级 Tantivy 全文投影和可选的向量腿；
- 从内部链接和 `relations` Frontmatter 派生的一跳文档关系；
- 用于变化检测的文件指纹。

图谱不自动抽取实体。支持 `[[doc]]`、相对 Markdown 链接以及：

```yaml
relations:
  - type: depends_on
    target: architecture/retrieval.md
```

目标暂不存在的边保留为 `unresolved`，目标出现后在同步时解析；关联证据会保留关系来源章节
和原文上下文，便于 Agent 回读。图谱是 SQLite 派生投影，不会成为 Markdown 正文的事实源；
检索证据最多附带一跳关联文档。非法 `relations` 声明不阻断其他文档索引，但会进入 `degraded`
诊断。

语义索引是可选的向量腿，按 chunk embedding hash、模型和维度校验。embedding hash 包含标题、
标签、章节和正文：文档修改时，未改变语义输入的 chunk 直接复用旧向量，只有新增或改变的
chunk 重新 embedding。模型变化会清理旧模型投影，并在下一次查询前重建；向量腿或 embedding
不可用时保留关键词检索，并在检索结果中报告降级原因。

文档的 Tantivy 和图谱投影先提交，向量 manifest 随后以 `pending` 状态提交，向量计算在后台
完成后以当前文档 content hash 做栅栏，再原子替换为 `ready`；计算失败保留 `error` 状态，
不会撤销 Markdown 或关键词索引。首次创建向量表时检索会等待这一轮初始任务，后续文档变更
保持异步，语义候选只读取模型、hash 和状态均匹配的 ready 向量。进程关闭或异常退出遗留的
`pending` 会在下次 runtime 启动时恢复为可重试状态，避免后台任务丢失后永久阻塞同步。

查询前执行增量确认，只解析变化文档并清理删除投影。mtime/size 用于快速筛选，读取后计算
content hash；唯一 hash 配对的删除+新增会保留文档稳定身份并识别为移动。解析失败时保留
已有有效投影，新文档则只报告失败。rebuild 不得用执行时间覆盖文件修改时间。“最近活动”
因此表示当前仍存在文档的真实修改时间，不是索引时间或审计历史。

SQLite 仅是 Markdown 的镜像索引，不执行旧 schema 的逐列迁移。索引文件使用
`PRAGMA user_version` 标记当前 schema；发现已有索引版本不匹配时，直接删除 SQLite、WAL 和
SHM 文件并创建新 schema，随后由增量同步从 Markdown 重新生成，原始文档不受影响。

普通查询并发取得路径/标题、关键词和语义候选，再使用排名融合；同一文档最多返回两个片段。
语义依赖不可用时降级为关键词查询，并在结果中说明原因。带近期意图的主题查询额外加入
新近度排名，普通查询不受时间偏置。

关键词投影使用 Tantivy，中文等"不以空格分词"的书写系统经 `lindera` 分词后建立词元；这替代
了 SQLite FTS5 的 `unicode61`（其会把一整段连续中文当作单个 token，任何中文子串查询都无法
命中）。相关文本分析下沉在 `tantivy_svc` 与 `markdown`，不扩散到业务层。

任何单个检索源失败都只降低策略等级（`degraded` 记录原因），不会中止整次检索。

候选查询在检索层完成 scope 与 `type`/`tags` 过滤、按文档去重并施加 `LIMIT`：否则一篇章节
很多的文档会占满整个候选池，使其他匹配文档无法进入融合阶段。

## 3. 模块与依赖

```text
CLI / MCP composition roots (src/main.rs, src/mcp.rs)
            ↓
        runtime context (src/runtime.rs)
            ↓
 search / validate / sync 编排
            ↓
 model（纯领域） ← tantivy_svc / storage / markdown / graph（适配）
```

- `model`：文档、检索结果、规则与校验的纯领域类型，**依赖无关**（不导入 Tantivy、rusqlite、
  CLI/MCP SDK，也不触碰文件系统）；作为 `sync` / `search` / `validate` 之间及 MCP
  序列化的共享契约；
- `sync`：Markdown → 索引/元数据的增量投影与 rebuild；
- `search`：任务检索策略、排名融合和 scope 过滤；
- `validate`：格式、Frontmatter 与内部链接校验（只报告，不改写）；
- `tantivy_svc`：检索引擎的**唯一边界**封装（写索引 / BM25 查询 / 可选向量腿），`sync` 与
  `search` 只依赖它的小 API，从不直接依赖 Tantivy 类型；
- `storage`：`rusqlite` 元数据存储（同步账本 / 图谱 / 状态），**不执行**全文或向量检索；
- `markdown`：路径安全、只读扫描、Frontmatter 解析和标题感知切块；
- `graph`：一跳文档关系抽取；
- `runtime`：显式资源装配、同步锁、可选 watcher 和索引生命周期；
- `main.rs`、`mcp.rs`：读取配置、协议适配、参数解析和结果序列化。

检索适配层经由明确边界使用，不直接创建 SQLite 连接或读取全局配置。composition root 统一
读取用户配置目录的 `~/.agentwiki/config.json`（相对路径以该文件所在目录为基准，首次运行缺少
文件时创建默认配置并初始化 Wiki 的 `AGENTWIKI.md`）；runtime factory 只接受已经解析的
构造参数。运行配置不读取环境变量。

## 4. Agent 工作流

1. 任务依赖 Wiki 知识、近期变化或未知位置时调用 `get_wiki_context`；
2. 用 Agent 原生工具读取关键命中文档，不能只根据摘要下结论；
3. 出现新实体、证据不足或矛盾时细化查询并再次检索；
4. 新建、移动或首次修改陌生范围前调用 `get_wiki_rules`；
5. 使用原生工具修改 Markdown；
6. 调用 `validate_wiki(path=...)`，全库验收才使用 `full=true`。

已知准确路径且不依赖其他 Wiki 知识时，直接使用原生文件工具，不做无效检索。

## 5. 可靠性

- 文档变更不会因索引或 embedding 失败而被回滚；
- 单篇解析失败不会阻断其他文档，失败路径进入检索降级信息；
- embedding 失败不会破坏关键词投影；
- 路径和 scope 必须留在 Wiki 根目录；外部符号链接不进入索引；
- 校验只报告问题，不自动改写原生工具产生的 Markdown；
- 必填字段完全由规则文件 `AGENTWIKI.md` 的 `required_fields`（含匹配的 `sections[].required_fields`）决定，系统不内置任何必填字段；规则可用可选 `tag_aliases` 归一同义标签，动态 `known_tags` 用于复用提示，新标签仅告警、不阻断，其他字段允许扩展；
- `rusqlite` 元数据连接由 runtime 显式初始化并关闭；
- 单文档索引失败（含元数据存储约束错误）会被记录到 `storage` 的 `sync_error` 并继续处理其余文档，不会中止整轮同步；
- 同一连接上的写操作串行化在一把写锁之下：正确性依赖锁纪律，而不是驱动层的事务隔离。因此所有写路径必须持有该锁，读路径不与之并发交叉。

## 6. 检索设计思路与取舍

本节记录检索方案背后的第一性原理与选型决策，是第 2 节的"设计意图"说明。

### 6.1 匹配信号是正交的四个维度

检索的本质是"以某种信号度量 Query 与片段的相似度"。信号可拆成四个正交维度，分别对待：

| 维度 | 度量方式 | 优点 | 短板 |
|---|---|---|---|
| **词法** | 倒排 + BM25 | 精确、可解释、对代码符/专名/ID 召回准确，是可靠的地基 | 同义改写搜不到；无空格中文需分词 |
| **向量（语义）** | embedding 余弦/内积 | 泛化到"词不同但意思同" | 对精确 ID/代码符不敏感；需 ANN 索引与本地模型 |
| **元数据过滤** | term/range（scope/tags/FM） | 精确、零模型、缩小候选集 | 只是过滤器，不是匹配器 |
| **重排** | cross-encoder 精排 | 提升 precision | 需要额外模型推理，属可选锦上添花 |

### 6.2 为什么是"分层式混合"而不是单一信号

对"给 Agent 提供 Markdown 上下文"这一目标，务实做法是**按需分层而非一步到位**：

1. **词法腿（必做地基）**：Tantivy BM25 + `lindera` 中文分词 + scope/tags 过滤。零模型依赖、离线、快、可解释，覆盖"关键词/专名/精确路径"这类 Agent 查 Wiki 的主要诉求。
2. **向量腿（可选叠加）**：弥补"词不同但意思同"的召回。**必须与词法混用**，不能单独用（免得精确项被漏掉）。
3. **重排（不引入）**：本项目不含查询/生成 LLM，生成式 RAG 不在边界内；cross-encoder 重排暂不作为必选项。
4. **融合算法**：RRF（倒数排名）或加权分数；候选**先按文档去重并施加 `LIMIT`**，防止单篇章节过多的文档占满融合候选池。

顺序上先做词法、向量 feature 化后置，既把"纯 Rust、零 C 依赖、秒级冷启动"这个重写的主要收益立住，又把最大不确定性（本地 embedding 推理链）往后推。

### 6.3 为什么选 Tantivy 而不是沿用 SQLite FTS5 + sqlite-vec

（结合 Rust 重写且不做一对一对数迁移的前提）Tantivy 的优势是本项目的决定性理由：

- **纯 Rust、零 C 依赖**：绕开 sqlite-vec 在 Rust 侧 `load_extension` 的编译链风险；
- **自带 BM25 与查询语法**：不再需要手工构造"三列投影 + 有序短语"这类 SQL 表达式；
- **mmap 冷启动快**：适合本地 Wiki 的索引常驻；
- **向量能力内嵌**（VecField），中小规模够用，不必另引专业向量库；
- 版本 0.20+ 支持 schemaless JSON 字段，Frontmatter（tags/note_types）可直接作为过滤字段。

由此，SQLite 在重写中**退化为纯元数据仓库**（同步账本 / 图谱 / 状态），不再执行全文或向量检索；检索全部收敛到 `tantivy_svc` 这一个边界。

### 6.4 中文处理：从"三列投影"演进到"lindera 分词"

Python 时代因 SQLite FTS5 `unicode61` 会把连续中文当作单个 token，采用`search_chars`（逐字）/`search_bigrams`（重叠二元组）/`search_words`（拉丁词元）三列 + 有序短语的破解方案。Rust 版改用 Tantivy + `lindera` **真分词**，替代那套手工工程：

- 纯词元召回对代码符/专名稍弱；若 Wiki 多英文标识符，可启用"词元 + 子串 n-gram"双字段作为可选增强。
- 中文文本分析下沉在 `tantivy_svc` / `markdown`，不扩散到业务层。

### 6.5 向量 / embedding 选型（可选 feature，默认后置）

- **离线本地**是硬约束，只能选有开源权重、能在本地推理的模型。中文场景首选 `BAAI/bge-zh` 系列（如 `bge-small-zh-v1.5`，512 维，~90MB）；中英混合再评估多语言模型（如 `bge-m3` / `paraphrase-multilingual`）。
- **推理链**：优先 `candle`（Hugging Face 官方 Rust 推理，纯 Rust、无 onnxruntime C 依赖）+ `tokenizers`；备选 `fastembed-rs`（若支持 bge 系列）。输出向量需 L2 归一化（单位向量上 `cos = 1 - L2²/2`）。
- **向量存储**：用 Tantivy `VecField`（中小规模 flat + mmap 够用），不另引专业向量库；规模上万再评估 LanceDB。
- **一致性**：向量按 `embedding hash + 模型 + 维度`校验；模型/维度变化时旧向量失效并重建；带"近期意图"的查询额外加入新近度排名，普通查询不受时间偏置。
- **降级**：embedding 或向量腿不可用时，检索降级为关键词查询并在结果中报告 `degraded` 原因，绝不中止。
