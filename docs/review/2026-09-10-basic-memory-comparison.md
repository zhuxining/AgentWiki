# AgentWiki vs Basic Memory 对比分析

- 参照项目：[basic-memory](https://github.com/basicmachines-co/basic-memory)（AGPL-3.0，commit `3452c821`）
- 被对比项目：AgentWiki（本仓库，`main` @ 本轮改动）
- 目的：找出 AgentWiki 在检索质量、可观测性、工程成熟度上的差距，以及哪些做法**不应**照搬

---

## 1. 定位与规模

| 维度 | Basic Memory | AgentWiki |
| --- | --- | --- |
| 定位 | 本地优先知识管理（Zettelkasten + 知识图谱），**双向读写** | 本地 Markdown **只读检索层**（读写交给 Agent 原生工具） |
| 许可 | AGPL-3.0-or-later | 未声明 |
| 源码规模 | 427 文件 / 124,302 行 | 29 文件 / 3,864 行 |
| 测试 | 539 个测试文件 / 211,896 行；`test-int/` 独立集成目录 | 14 文件 / 1,765 行；`integration` marker 已声明但零使用 |
| CI | 11 个 GitHub Actions 工作流 | **无** |
| Schema 演进 | alembic，**38 个迁移版本** | `PRAGMA user_version` + 删索引重建 |
| 存储后端 | SQLite、PostgreSQL、pgvector、Milvus、Redis 读缓存 | 仅 SQLite + sqlite-vec |
| 对外 | MCP（29 个工具）+ HTTP API + Web UI + 云同步 + CLI + hooks | MCP（3 个工具）+ CLI |
| 检索模式 | text / vector / hybrid（可配置默认） | 自动组合 exact/keyword/semantic/graph/recency |
| 嵌入模型 | 可选多语言（`multilingual-minilm` 等）+ **cross-encoder rerank** | 单模型 `bge-small-en-v1.5`，无 rerank |
| 相似度阈值 | `semantic_min_similarity = 0.55`（可配，0 关闭） | **无阈值**，无弃答 |
| 向量候选 | `semantic_vector_k = 100` | `candidate_limit * 4`（limit≤20 → ≤80） |

规模差异不是"谁更好"，而是产品边界不同：basic-memory 把**写入、同步、多端、云**都做进了产品，AgentWiki 明确把这些推给 Agent 原生工具。对比的价值在**同一问题上两种不同解法**。

---

## 2. 五个关键设计差异

### 2.1 中文/多语言检索：分词 vs 多语言 embedding

| | Basic Memory | AgentWiki |
| --- | --- | --- |
| 解法 | 提供**多语言 embedding 模型** + 相似度阈值；FTS 仍是 `tsvector`/FTS5 词法路径 | 自研 **CJK bigram 预分词**写入 FTS `search_text` 列 |
| 依据 | `docs/multilingual-embedding-benchmark.md`：17 篇笔记 / 23 条判定查询，覆盖中、日、韩、阿、俄、西、泰与混合语言 | 无多语言基准（原 smoke 集全英文） |
| 实测 | MiniLM：跨语言 recall@5 `0.7143 → 1.0000`，跨语言 MRR@10 `1.0`，负样本误报 `0.75 → 0.00` | bigram 让中文子串查询从 0 命中变为可命中 |
| 代价 | MiniLM 模型 RSS +671 MB、缓存 252 MB（vs BGE 221 MB / 67 MB） | 零额外依赖，但**只解决字面匹配**，无法处理同义改写与整句提问 |

**判断**：两者不冲突。bigram 解决"关键词路在中文下可用"（已落地、零成本）；多语言 embedding 解决"语义路在中文下可用"，是 AgentWiki 语义检索目前的真实短板——默认 `bge-small-en-v1.5` 对中文语义查询基本无效。

### 2.2 弃答（no-answer）：阈值门控 vs 无门控

- Basic Memory：`semantic_min_similarity: float = 0.55`（`config_models.py:417-422`），"结果低于阈值被过滤，0.0 关闭过滤"；基准同时报告 `empty-result rate` 与 `negative false-positive rate`。
- 距离换算（`sqlite_search_repository.py:737-743`）：单位向量下 `cos = 1 - L2²/2`。
- AgentWiki：`semantic_candidates` 用 `1/(1+distance)` 作为分数，**没有阈值**，且该分数只用于排序、不过滤。上一轮审查记录的 `no_answer_false_positive_rate` 在 hybrid 下不可靠，正源于此。

**这是 AgentWiki 明确缺失且值得照搬的能力**。注意 basic-memory 的实测也显示代价：MiniLM 在 0.55 下 `accepted empty`（判定为正例却返回空）升到 `0.2632`，他们的结论是"换模型必须显式做阈值决策 + 长文档验证，不能继承 0.55"。**照搬机制可以，照搬数值不行。**

### 2.3 混合融合：分数级融合 vs 排名级 RRF

- Basic Memory（`search_repository_base.py:2851-2856, 2998-3012`，`FUSION_BONUS = 0.3`）：

  ```
  fused = max(vec, fts) + 0.3 * min(vec, fts)
  ```

  两侧分数先归一化到 `[0,1]`：FTS 按批内最大值归一（显式处理 SQLite 负 bm25 与 Postgres 正 ts_rank 的符号差异），并设 `FTS_GATE_THRESHOLD` 把低分 FTS 归零；向量分**直接用**换算后的余弦相似度。

- AgentWiki（`services/retrieval.py`）：加权 RRF，`Σ w_source/(60+rank)`，`w={exact:1.2, keyword:1.0, semantic:1.0, graph:0.4, recency:0.3}`。

**差异**：RRF 只用排名，天然免疫"不同源分数不可比"的问题，实现更简单；但它丢掉了**分数强度**信息，也无法做阈值门控（没有可比分数就不能弃答）。basic-memory 的 `max + 0.3*min` 保留主导信号并奖励双源一致，为阈值门控提供了基础。

**判断**：AgentWiki 的 RRF 对当前规模是合理选择；若要做弃答，需要先有"可比相似度"——这意味着要引入 2.2 的阈值，而阈值依赖原始相似度，不能建立在 RRF 融合分上。

### 2.4 查询理解：宽松回退 vs 严格全词匹配

这是**最能直接解决 AgentWiki 已知缺陷**的一条。

- Basic Memory（`postgres_search_repository.py:1501-1539`）：严格 `tsquery`（AND 语义）返回 0 行或语法错误时，**自动用 OR 连接的宽松查询重试一次**，并记录 `relaxed_fallback_used`。代码注释直说动机：

  > questions rarely have every word in one document; without relaxation the FTS half of hybrid search contributes zero candidates. Fusion plus bm25 keep relaxed lexical candidates from dominating precision.

- AgentWiki：`indexing/text.py:matches_text` 要求**每个查询词元都在文档中逐字出现**（严格 AND）。实测：

  | 查询 | FTS 候选 | 严格保留 |
  | --- | ---: | ---: |
  | `更新流程` | 1 | 1 |
  | `如何更新配置` | 1 | **0** |
  | `变更管理` | 1 | **0** |
  | `备份 回滚 通知` | 1 | 1 |

  即"如何在文档里出现但查询多了「如何」"就整体归零——**正是 basic-memory 用宽松回退修掉的同一类问题**。

**这是 AgentWiki 目前最值得照搬的机制**。但要注意它的门槛：宽松后必须靠 fusion + bm25 压制精度损失。AgentWiki 若要复刻，需要同时给候选一个"覆盖率"信号用于排序，否则 OR 放宽会把大量弱相关块挤进融合。

### 2.5 可观测性：结构化 trace vs 字符串 degraded

- Basic Memory：`repository/search_trace.py` 是一等公民——记录 `effective_min_similarity`、`min_similarity_source`、`threshold_rejections`、`filter_rejections`、每个阶段的耗时（`fts_ms`/`vector_ms`/`fusion`）与行数收缩。基准输出把"被阈值拒绝"的候选也保留在 trace 里，使"返回"与"拒绝"可区分。
- AgentWiki：`ContextResult.degraded` 是 `tuple[str, ...]`，靠字符串拼接表达降级原因（如 `"semantic_unavailable: ..."`）。无法区分"没有候选"与"被过滤"，也无法回答"为什么这条没进结果"。

**判断**：AgentWiki 的 degraded 对 Agent 消费者够用（Agent 只需知道"语义不可用"），但对**调试检索质量**不够。basic-memory 的 trace 值钱之处在于它支撑了阈值调优与回归定位。

### 2.6 分页与计数：显式语义 vs 单布尔

- Basic Memory：`SearchResult` 带 `total` / `total_is_exact` / `has_more`（`schemas/search.py:309-317`），且源码注释明确警示分页陷阱（`"has_more = offset + len(results) < total 会给出误导"`）。
- AgentWiki：只有 `truncated: bool`，无 total、无 offset。

**判断**：AgentWiki 的 `limit ≤ 20`、无分页是有意的产品选择（避免 Agent 翻页），`truncated` 足够。basic-memory 的贡献是**把每个计数字段的语义写进字段描述**——这与本轮给 `rank_score` 补语义是同一思路。

### 2.7 缓存：可插拔契约 vs 单点指纹

- Basic Memory：`read_cache/` 是完整子系统——`contract.py` 定义 Protocol，`policy.py` 决定可缓存性，`invalidation.py` 管失效，`keys.py` 生成请求摘要，`redis.py` 是适配器，默认关闭（"best-effort semantic read caching"）。
- AgentWiki：本轮刚给 `get_wiki_rules` 加了"控制文件指纹"缓存（单点、进程内）。

**判断**：不需要照搬 Redis 与协议层。但 basic-memory 的**"缓存必须显式定义失效条件"**是可借鉴的原则——AgentWiki 后续若缓存检索结果或规则扫描，失效键应当是"文档集指纹 + 规则指纹"而不是时间。

---

## 3. AgentWiki 可直接借鉴（按性价比排序）

| # | 机制 | 落点 | 风险 |
| --- | --- | --- | --- |
| 1 | **相似度阈值 + 弃答** | `semantic_candidates` 用 `cos = 1 - L2²/2` 取代 `1/(1+d)`，加可配 `semantic_min_similarity`；阈值以下返回空并在 `degraded` 说明 | 低。需按 AgentWiki 自己的语料定值，不能继承 0.55 |
| 2 | **FTS 宽松回退（一次重试）** | `keyword_candidates`：严格全词未命中时，用 OR 语义重查并按词元覆盖率排序 | 中。需同时引入覆盖率排序，否则精度换召回得不偿失 |
| 3 | **向量候选量提升** | `candidate_limit * 4`（≤80）→ 固定下限（如 100） | 低。只是常数，需实测延迟 |
| 4 | **结构化降级/trace** | `degraded: tuple[str, ...]` 之外增加机器可读的阶段统计（候选数、被过滤数、耗时） | 低，但会改对外结构 |
| 5 | **多语言 embedding（可选）** | 当前 `bge-small-en-v1.5` 对中文语义无效；提供多语言模型选项并补中文语义基准 | 中。模型体积/RSS 显著上升，需用户可选 |
| 6 | **benchmark 记录负样本与空结果率** | 现有中文集已含 `no_answer`，但未单列 `empty-result rate` | 低 |

## 4. AgentWiki **不应**照搬

| 项 | 理由 |
| --- | --- |
| 写入类工具（`write_note`/`edit_note`/`move_note`/`delete_note`）与 `note_preparation` | 这正是 AgentWiki 的产品边界：已知路径的增删改交给 Agent 原生工具，AgentWiki 只做"从未知到已知"与事后校验。复制写入会引入格式一致性与并发写的整类问题 |
| 多存储后端（Postgres/pgvector/Milvus/Redis） | 12 万行项目需要它们，3 千行项目不需要。SQLite + sqlite-vec 已覆盖本地单机场景 |
| 云同步 / Web UI / HTTP API | AgentWiki 明确不承诺这些 |
| alembic 38 个迁移 | AgentWiki 的索引是**可删除派生物**，删库重建比维护迁移更符合"Markdown 是事实源"的定位 |
| 全量复刻 reranker | cross-encoder 会显著抬高延迟与内存；AgentWiki 当前规模用阈值 + 覆盖率排序收益更高 |

---

## 4.5 产品边界与 MCP 层（第三方深读补充）

> 这一节来自一次独立的专项审计，其中两条**纠正了我上面的判断**。

### 纠正：我们的 instructions 并不臃肿

AgentWiki 的 MCP instructions 是 **932 字符**，basic-memory 约 **2005 字符**——我们反而更短。`guide_content` 也**不在** instructions 里，它是 `get_wiki_rules` 的返回字段（`governance.py:184-187`）。此前"把所有指引塞进 instructions"的印象不成立。

真正的差距是**缺中间层**：basic-memory 用三层承载长文本——instructions（只放身份与开场动作，源码注释写明"新连接只有 instructions 是免费的"）、`memory://` resources（40 行 `ai_assistant_guide` + 22 页 man），以及一份 3330 行的扩展手册**根本不被 MCP 加载**。我们只有 `get_wiki_rules` 一个按需入口，且它把指南与规则查询耦合——不需要指南的调用也要付全文成本。

### 纠正：'progressive tool discovery' 并非分阶段暴露

basic-memory 实际是 `tag + hints + 一组 Visibility 开关 + memory://man 按页 fetch`。唯一真正的按需机制是 `POSIX_TOOLS_TAG` + `Visibility(enabled, tags={"posix"})`（`server.py:30-39,111-115`），由 config 决定是否注册整组工具。同版本依赖我们也有，**零成本可照搬**。

### AgentWiki 边界内但缺失的四项

| # | 缺失 | 说明 |
| --- | --- | --- |
| 1 | **无文件系统客户端读不到正文** | instructions 要求"用原生工具读关键原文"（`mcp.py:44-45`），但 Claude Desktop / 云端 Agent 拿到 `path` 也读不了。这是**最大的产品风险**：证据片段无法被回读，整个"片段是候选、原文为准"的设计在那类客户端上断链。建议 config-gated 的最小读取工具，而不是整套 POSIX |
| 2 | 运行时自描述入口 | 我们的 `docs/MCP_TOOLS.md` 在运行时不可见；basic-memory 有 `memory://ai_assistant_guide` 与 man 页 |
| 3 | 运行/索引状态自描述 | 对应 basic-memory 的 `basic_memory_diagnostics`；我们只有 `degraded` 字符串 |
| 4 | n 跳关联 | `related` 硬编码 1 跳 5 条（`services/retrieval.py:251`、`repository/search.py:766`） |

被排除的怀疑：'缺 recent_activity / build_context'不成立——空 query 等价 recent，`related` 等价一跳。

### 本次对比新查出的三个具体缺陷

1. **`GraphEdgeDraft.anchor` 是死代码**：`indexing/graph.py:19` 定义了 `anchor`，全仓无赋值点，`search.py:302` 只是把它写进 SQLite 的 `anchor` 列，该列恒 NULL。后果是 `[[doc#section]]` 的章节部分在 `graph.py:120` 被 `split("#")` 丢弃，**无法建立章节级边**。
2. **多进程并发写无保护**：索引写锁是进程内 `asyncio.Lock`（`repository/search.py:43`、`indexing/sync.py:22`），跨进程只有 `busy_timeout=5000` + WAL。`ARCHITECTURE.md` 里"单一写锁"的表述在多 MCP 进程下不成立。
3. **`limit` 越界直接抛异常**：`ContextQuery.limit` 是 `ge=1, le=20`，MCP 层（`mcp.py:87`）没有 clamp，越界返回的是 Pydantic 校验错误而不是可读提示。另：`get_wiki_rules` 返回**无上限**的完整 `known_tags`，大库下体积会膨胀。

顺带：`ARCHITECTURE.md:103` 此前写"项目根目录的 `.agentwiki/config.json`"，与 `config.py:9` 的家目录语义不一致——**本轮已修正**。

---

## 4.6 存储、同步与工程实践（第三方深读 + 本地复核）

### 已被我独立复核的四条

| 断言 | 复核结果 |
| --- | --- |
| 移动识别是"content_hash 唯一配对，≥2 候选放弃" | **成立**。`indexing/sync.py:56-65`：`move_sources` 长度必须恰为 1，否则 `moved_from=None`（退回按新文档处理）。少于此前的印象——它对"源已删、目标下一轮才落盘"的窗口无能为力，因为证据只活在单次 `ensure_fresh()` 的局部集合里 |
| `wiki_index_meta` 缺"完成过一次全量索引"的持久标记 | **成立**。该表只存 `vector_model` 与 `vector_dimensions` 两个配置值（`sqlite.py:109-119`、`search.py:1225-1240`）。`last_indexed_at_ns` 仅存在于每篇文档行（`sqlite.py:51`），没有库级就绪位 |
| 可用 `RETURNING` + 幂等 upsert 让移动识别原子化 | **成立**。本机 SQLite 3.53.4 实测 `INSERT ... ON CONFLICT DO UPDATE ... RETURNING k` 可用。这意味着"这次是新插入还是命中已有"可以由数据库一次判定，不必先 `SELECT` 再 `INSERT`（basic-memory 的 `note_file_vacate_repository.py:66-87` 正是这个模式） |
| 我们的 `limit` 越界与"零计数非就绪" | 见 4.5 与下表 |

### 存储与同步的关键差异

| 维度 | Basic Memory | AgentWiki |
| --- | --- | --- |
| schema 演进 | alembic **38 个版本**（含 2 个 merge revision、38/38 有 `downgrade`），启动即迁移 | `PRAGMA user_version = 2` + 删除 `.sqlite3/-wal/-shm` 重建 |
| 可 rebuild 的证据 | 有两张**不可从 Markdown 重建**的持久表（`note_content` 权威副本、accepted 变更 journal），因此**必须**迁移 | 索引完全可从 Markdown 重建，因此删库是**不变量推导出的结论**而非省事 |
| 移动识别 | checksum 配对 + **`note_file_vacate` 持久标记**（区分"移动残留源"与"逐字节相同副本"）+ frontmatter permalink 优先 | content_hash 唯一配对，证据只存活于单次同步 |
| 文件监听 | 一次 `awatch` 覆盖所有根 + **最深优先路由** + 项目级异常隔离 + 周期重启 cycle；状态落盘，保留最近 100 事件 | 21 行，无 ignore、无状态、无重启；**零测试**，唯一调用方 `watch-index` 也零测试 |
| 批量索引 | 三阶段各自独立并发上限（`Semaphore`），每路径错误隔离 | 完全串行；异常隔离做得好（`_DOCUMENT_ERRORS` + 继续），但无并发 |
| 并发写 | 委托 DB 仲裁 + 乐观 checksum + 加锁顺序不变量（`FOR UPDATE` 仅 Postgres）+ 每项目单飞合并 | 进程内 `_write_lock`/`_task_lock`/`asyncio.Lock`；**跨进程无保护**（只有 `busy_timeout=5000` + WAL） |
| 读缓存 | Redis 可选，项目级 generation token 失效（随机 token 防"被逐出的 key 复活旧值"），300s/1800s TTL，取消安全 | 无缓存层；`_DirectoryCache`（目录 mtime）与向量内容哈希复用是两个替代物 |
| 多项目 | 单全局 DB + `project_id` 外键 + 每查询集中过滤；`mcp/project_context.py` 2,039 行 + 测试 3,589 行 | 单根目录；`path_for` 一次 `relative_to` 换到**可证明的路径安全** |

**注意**：basic-memory 全仓唯一的 OS 文件锁是 `hooks/inbox.py:196` 的 `.flush.lock`，且它保护的是 hook inbox 而非 DB——**他们也没有跨进程 DB 锁**，而是把并发完全委托给数据库。这修正了"应该照搬跨进程文件锁"的直觉：正确方向是**让 DB 仲裁（约束 + upsert/CAS）**，而不是加锁。

### 工程实践：一条我认为应立即做的

**可执行的架构边界测试**。basic-memory 用 31 行 `ast` 检查断言"repository 层不得 import indexing"（`tests/test_architecture_boundaries.py`）。而 AgentWiki 的 `AGENTS.md` 写了一条**极其明确却零强制**的依赖方向：

```text
CLI/MCP composition roots → runtime context → services
                                    ↓
                      domain / service ports ↑ repository / indexing / markdown
```

这条不变量目前只靠人自觉——我在本轮就亲手违反过一次（`governance.py` 从 `markdown.library` 导入 `RESERVED_FILE`，越过了 `ports` 边界，靠人工发现才改到 `domain`）。**把它变成约 40 行测试，性价比最高。**

其余可借鉴（按性价比）：

1. **`last_indexed_at` 的教训**：basic-memory 的源码注释记录了一个真实 bug——"计数为零无法区分『没事可做』和『从没跑过』"。我们已经有这个问题：`sync_state='ready'` + 空索引与"尚未索引"不可区分。建议在 `wiki_index_meta` 增加一个"完成过至少一次全量索引"的标记。
2. **"移动"从一次性推断升级为持久断言**：basic-memory 的 `note_file_vacate` 思路，配 4.6 已验证的 `RETURNING` upsert，可把我们的"≥2 候选放弃"变成"结合 vacate 标记裁决"。
3. **并发/生命周期回归测试**：我们有 `asyncio.Lock`、后台向量任务、`user_version` 删库重建——全敏感，却零覆盖。至少要三条：并发 `ensure_fresh` 不产生重复投影；`close()` 正确取消向量任务且不留 `pending`；重建后索引与增量 sync 结果一致。
4. **显式覆盖豁免清单**：basic-memory 的 `pyproject.toml` 有 9 项 `omit`，**每项带一行理由**。我们既无覆盖率报告也无豁免纪律。

### 明确不应照搬

- **alembic**：我们的索引是可删除派生物；照搬要付幂等守卫、方言分支、双 head、迁移图 CI 的整套成本，换来的"保留索引行"对可重建数据毫无价值。**唯一反转条件**：一旦引入不可重建状态（跨进程队列、审计日志、用户级向量缓存）。
- **多项目/多 workspace 路由**：`project_context.py` 2,039 行 + 3,589 行测试 + 每表 `project_id` + 每查询过滤，为"本地单机单 Wiki"这个不存在的需求付出整个代码库最大的一块复杂度。
- **Redis 读缓存子系统**：805 行 + 624 行 plan + 15+ 处失效点。我们单用户单进程，重复读的瓶颈在目录扫描而非 DB 查询。唯一可带走的是原则：**缓存必须显式定义失效条件，并把失效条件写成可测断言**。

### 一条"不必照搬但该重新设计"的

**读路径无条件 `ensure_fresh()`**（`services/retrieval.py:102`）。basic-memory 的解法不是"更快的扫描"，而是**把扫描移出读路径**（watcher + 显式 index 命令 + 后台 scheduler）。我们的 `_DirectoryCache` 是在同一位置打补丁；更彻底的方向是"有 watcher 时信任 watcher，无 watcher 时按 mtime 粗粒度节流"。这属于设计取舍，不是照搬。

---

## 5. 结论

同一个问题（本地 Markdown + 中文/跨语言检索 + MCP 暴露），两个项目的解法分叉在三个点上：

1. **多语言能力放在哪一层**：basic-memory 放在 embedding（模型选型 + 阈值），AgentWiki 放在 FTS（bigram 分词）。前者解决语义，后者解决字面；AgentWiki 的语义路在中文下仍是空的。
2. **如何对待"查询词不全命中"**：basic-memory 明确做宽松回退并靠 fusion 压制精度损失；AgentWiki 严格 AND 导致整句提问归零。**这是最值得抄的一条。**
3. **分数是否可比**：basic-memory 保留可比分数以支撑阈值弃答；AgentWiki 用排名级 RRF，简单但与弃答不兼容。

工程成熟度上 basic-memory 领先一个量级（CI、539 个测试文件、结构化解耦），但那是 12 万行的必然要求，不是 AgentWiki 该追的指标。AgentWiki 该补的是**检索质量的可度量性**：阈值、覆盖率信号、结构化降级。

---

## 6. 基于对比落地的修复（本轮）

对比结束后，把实测确认的 6 个缺陷一并修掉。验收：`ruff` / `ty check` / `pytest`（97 passed）/ `git diff --check` 全通过；中文基准未退化（misses 与修复前一致）。

| # | 缺陷 | 修法 | 实测 |
| --- | --- | --- | --- |
| 1 | CJK run 内单字查不到，假名/谚文/泰文子串查不到 | `indexing/text.py` 改用 **23 段 script 区间表**覆盖 Han/假名/谚文/注音/泰老藏缅高棉；NFKC 归一 + 合并组合符 | `生`、`エンジン`、`ンジン`、`검색엔진`、`การค้นหา` 全部从 **0 命中 → 命中** |
| 2 | FTS 只有 bigram，无 unigram；且 `matches_text` 复检使 `LIMIT` 先于校验 | 改为**三列投影**（`search_chars` 单字 / `search_bigrams` 二元组 / `search_words` 拉丁词），查询侧每段渲染成「字符有序短语 **AND** 二元组有序短语」——精确性由 FTS5 短语语义保证，**删除 Python 复检** | 命中矩阵 19/19；假阳性检查 `令牌 部署指南` / `完全不存在` / `配置 不存在` 均为空 |
| 3 | `section`/`content` 作为已索引列写入原文 → 正文被索引两遍、bm25 对拉丁词双重计数 | 两列改 `UNINDEXED`，`_SCHEMA_VERSION` 2 → 3（重建索引） | 拉丁词只落在 `search_words` 一列 |
| 4 | 标签别名扩展**收紧**而非放宽召回（扩展到 `matches_text` 的 all-terms 检查里） | 停用 `_expand_tag_terms`（删除函数与调用）；别名仍通过 SQL 侧 `tag_aliases` 作用于标签**过滤** | `发布流程` 从「扩展后 0/原标题 1」变为稳定命中 |
| 5 | 多 run 查询的短语语义（走查中发现的连带问题） | 每个 script run 生成独立条件组，组间 AND；拉丁词元用 `search_words: "…"` | `配置 FTS5` 等混合查询正常 |
| 6 | `GraphEdgeDraft.anchor` 是死代码，`[[doc#section]]` 的 `#section` 被丢弃 | `graph.py` 捕获并归一 wikilink 锚点；`RelatedDocument` 增加 `anchor` 字段并在出入边查询中返回 | 新增测试：`[[target#回滚流程]]` → `RelatedDocument(anchor="回滚流程")` |

### 与 basic-memory 的机制对应

- 第 1、2 条对齐它的 `script_ngrams.py`：它用「29 段区间 + `(*run, *bigrams)` 单列 + 逐 run 排序短语」；本实现用「23 段区间 + 字符/二元组双列 + 列限定短语」。**双列是为了让单字查询与相邻性约束互不干扰**——单列混排时，二元组 token 会打断单字短语，使「令牌」这类查询在长 run 上失效（实测发现并修正）。
- 第 2 条同时消除了它的 `allow_relaxed` 想解决的问题的一半：短语语义把 AND 约束交给 FTS5，不再依赖 bm25 恰好把全词命中的文档排进窗口。
- 第 5 条是三列方案的必然结果，非照搬。

### 仍未做（下一步候选）

- 语义阈值 + 弃答（需先把 `knn.distance` 换算为 `cos = 1 - d²/2`）
- `allow_relaxed` 式的宽松回退（严格短语 0 命中时退化为 OR + 覆盖率排序）
- 架构边界测试（`ast` 检查约 40 行）
- 多进程并发写保护、`wiki_index_meta` 的「完成过一次全量索引」标记
- 读路径 `ensure_fresh` 的扫描短路

---

## 7. 第二批落地：语义阈值、快速路径、架构边界、并发

| # | 项 | 机制 | 配套测试 |
| --- | --- | --- | --- |
| 1 | **语义阈值 + 弃答** | 向量**L2 归一化**后用 `cos = 1 - d²/2`（`repository/embeddings.py`）；`Settings.min_similarity`（默认 0.55）经 `ContextQuery` 下传到向量腿过滤；`ContextResult.matched` 独立表达"没有达到门槛的匹配" | 归一化与零向量、阈值过滤 |
| 2 | **索引完成标记** | `wiki_index_meta` 新增 `index_completed` / `index_generation`；`SyncReport.indexed_once`。零行数不再等价于"已索引且为空" | `rebuild` 后为 True、初始为 False |
| 3 | **读路径快速路径** | `MarkdownLibrary.snapshot()` 返回「描述符 + 全树指纹（路径/大小/mtime）」；指纹与上次写入一致时，`ensure_fresh` **跳过 `fingerprints()` 全表扫描与 `vector_stale_paths()` 的 chunk 级反连接** | monkeypatch 计数证明指纹查询未被调用；编辑后快速路径失效 |
| 4 | **宽松回退** | 严格短语 0 命中时用 OR（单字 + 二元组 + 词元）重试一次，由 bm25 排序；FTS 加 `tokenchars 0x2F` + `prefix='1,2,3'` | 严格短语仍精确；宽松后部分匹配可召回 |
| 5 | **架构边界测试** | `tests/unit/test_layering.py` 用 AST 校验每层可导入的层；`agentwiki.config` 只允许组合根读取 | 6 条断言，立刻抓出 3 个真实违规 |
| 6 | **跨进程写保护** | `indexing/locking.py`：索引旁的 `.lock` 文件 + `flock`（Windows 走 `msvcrt`）；`ensure_fresh` / `rebuild` 全程持锁 | 真子进程持锁 → 父进程阻塞 → 子进程正常退出 |

### 这轮修掉的架构违规（由第 5 项抓出）

| 违规 | 处理 |
| --- | --- |
| `services/governance` 导入 `markdown.formatting` | 纯函数下沉为 `domain/formatting.py` |
| `services/ports` 导入 `indexing.graph` | `GraphEdgeDraft` 下沉为 `domain/graph.py` |
| `repository/search` 导入 `indexing.text` | 文本分析下沉为 `domain/text.py` |
| `agentwiki/data/**/__init__.py` 造成两个空模块 | 删除，模板改由 `files("agentwiki").joinpath("data/default/AGENTWIKI.md")` 读取 |

### 三处设计缺陷在实现中被发现并修正

1. **快速路径掩盖了模型切换**：模型变化只 drop 向量表，但 generation 未失效，下一轮 `ensure_fresh` 走快速路径导致向量永不重建。修法：`drop_vector_table` 一并清 `index_generation`。
2. **快速路径掩盖了遗留 pending**：`close()` 把 pending 改成 error，但 generation 未失效，下次启动不重试。修法：`close()` 与 `_recover_pending_vectors` 发现有待处理行时清 `index_generation`。
3. **宽松回退改变了既有契约**：原先"措辞不一致就零召回"变为"共享词元即可部分召回"。这是**召回换精确**的有意权衡，已更新 `benchmarks/README.md` 的边界描述与两条测试契约。

### 仍未做

- cross-encoder rerank（basic-memory 默认关闭；RRF 只有 rank 信号，纠正不了"两路都排第 3 但都不相关"）
- embedding identity 逐 chunk 持久化（模型变化仍整表重嵌）
- 带拒绝原因的检索阶段 trace 与慢查询阈值
- `ContextResult` 的分页字段（`total` / `has_offset`）
