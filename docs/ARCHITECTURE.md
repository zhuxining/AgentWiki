# AgentWiki 架构

## 1. 产品边界

AgentWiki 是本地 Markdown Wiki 的搜查与治理层。它负责：

- 为 Agent 检索历史、约定、已有方案、关联内容和近期变化；
- 使用 FTS5、可选本地 embedding 和排名融合返回章节级证据；
- 提供目录组织规则，并在原生文件修改后执行确定性校验；
- 保持 SQLite 投影可删除、可增量同步和可全量重建。

Agent 原生工具负责已知路径读取、创建、编辑、移动和删除。HTTP API、云同步、Web UI、
查询 LLM 和操作审计不属于当前范围。图谱仅表示 Markdown 中明确声明的文档关系。

## 2. 数据与检索

Markdown 正文和 YAML Frontmatter 是事实源。Wiki 根目录的 `AGENTWIKI.md` 是唯一的保留治理
文件：Frontmatter 承载结构化规则，正文作为 `guide_content` 返回，该文件不进入普通索引。
除此之外所有 `*.md` 都是普通文档。

运行时装配（`create_runtime`）在启动时检查该文件：缺失则写入随包分发的默认模板
（`agentwiki/data/default/AGENTWIKI.md`），已存在则永不覆盖。因此新建 Wiki 立刻拥有可用规则，
而手写规则不会在任何一次启动中被回退。该初始化是启动期唯一的写操作，不参与索引事务。

索引保存：

- 文档路径、标题、Frontmatter、真实 `mtime_ns` 和大小；
- 文档稳定身份、内容 checksum、同步状态和失败原因；
- 按 Markdown 标题层级切分的有界片段；
- 片段级 FTS5 投影和可选的 sqlite-vec 向量 manifest；
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

语义索引由 `wiki_vector_manifest` 和 sqlite-vec 物理表组成，按 chunk embedding hash、模型和
维度校验。embedding hash 包含标题、标签、章节和正文：文档修改时，未改变语义输入的
chunk 直接复用旧向量，只有新增或改变的 chunk 重新 embedding。模型变化会清理旧模型的
物理表和 manifest，并在下一次查询前重建；sqlite-vec 或 embedding 不可用时保留 FTS5，
并在检索结果中报告降级原因。

文档的 FTS5 和图谱投影先提交，向量 manifest 随后以 `pending` 状态提交，向量计算在后台
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

关键词投影对"不以空格分词"的书写系统（Han、假名、谚文、注音、泰/老/藏/缅/高棉）建立
三条索引列（`indexing/text.py`）：`search_chars` 存每个字，`search_bigrams` 存重叠二元组，
`search_words` 存拉丁词元。原因是 SQLite FTS5 的 `unicode61` 会把一整段连续中文当作单个
token，任何中文子串查询都无法命中。

查询侧把每个书写段渲染成一条 FTS5 条件组：`search_chars` 的**有序短语**钉住字符与顺序，
`search_bigrams` 的**有序短语**钉住段内相邻性，两列合取即等价于精确子串匹配——因此不需要
在 Python 侧再对候选做一次复检（复检会让 `LIMIT` 先于校验生效，使真正的命中被挤出候选窗口）。
段与拉丁词元之间用 AND 连接。

任何单个检索源失败都只降低策略等级（`degraded` 记录原因），不会中止整次检索。

候选查询在 SQL 层完成 scope 与 `type`/`tags` 过滤、按文档去重并施加 `LIMIT`：否则一篇章节
很多的文档会占满整个候选池，使其他匹配文档无法进入融合阶段。

## 3. 模块与依赖

```text
CLI / MCP composition roots
            ↓
       runtime context
            ↓
 services/retrieval + services/governance
            ↓
 service ports + domain values
            ↑
 indexing / repository / markdown adapters
```

- `domain`：文档、检索结果、规则与校验的纯模型；
- `services`：任务检索策略、排名融合、规则合并和校验；
- `indexing`：标题感知切块、文件指纹、增量同步和 rebuild；
- `repository`：SQLite 生命周期、片段投影、FTS5 和向量候选；
- `markdown`：路径安全、只读扫描、Frontmatter 解析和格式比较；
- `runtime`：显式资源装配、同步锁和可选 watcher；
- `cli`、`mcp`：读取配置、协议适配和结果序列化。

services 通过 Protocol 使用稳定边界，不直接创建 SQLite 或读取配置。composition root
统一读取用户配置目录的 `~/.agentwiki/config.json`（相对路径以该目录为基准，首次运行缺少
文件时创建默认配置并初始化 Wiki 的 `AGENTWIKI.md`）；runtime factory 只接受已经解析的
构造参数。

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
- SQLite 连接由 async runtime 显式初始化并关闭；
- 单文档索引失败（含 SQLite 约束错误）会被记录到 `sync_error` 并继续处理其余文档，不会中止整轮同步；
- 同一连接上的写操作串行化在一把写锁之下：正确性依赖锁纪律，而不是驱动层的事务隔离。因此所有写路径必须持有该锁，读路径不与之并发交叉。
