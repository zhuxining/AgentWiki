# AgentWiki 真实 Wiki 基准测试

本目录提供一套离线、可重复的 Markdown Wiki 检索基准。基准通过 AgentWiki 的真实
`create_runtime`、`IndexSynchronizer` 和 `RetrievalService` 执行，不直接调用 SQLite 查询，
也不会修改 Wiki 原文；runner 只在临时目录创建可删除的 SQLite 派生索引。

## 1. 使用的公开 Wiki

首选 corpus 是 Mozilla 官方 MDN Web Docs 内容仓库：

- 仓库：[mdn/content](https://github.com/mdn/content)
- 用途：HTML、CSS、JavaScript、HTTP 和 Web API 技术文档；仓库 README 说明其为 MDN 文档
  的官方内容源，包含 14,000+ 篇文档；
- 格式：Markdown 文件，带 YAML Frontmatter、标题层级、代码块、列表和较长的章节结构，
  与 AgentWiki 的切块和检索边界匹配；
- 授权：MDN 文档默认按 [CC-BY-SA 2.5 或更高版本](https://github.com/mdn/content/blob/main/files/en-us/mdn/writing_guidelines/attrib_copyright_license/index.md)
  提供。若分发 corpus、截图或派生数据，必须保留 Mozilla Contributors 署名、原文链接和
  ShareAlike 许可说明；
- 选择理由：这是一个真实的公共技术知识库，而不是只包含项目 README 的代码仓库；它有
  足够的规模、主题相似文档、深层标题和多种 Markdown 形态，适合测试从关键词到章节证据
  的检索链路。

MDN 不是 GitHub 的 `*.wiki.git` 页面仓库，而是“以 Git 管理的公共文档 Wiki”。如果只需要
小型、低成本的 Markdown 对照库，可以改用 [tldr-pages/tldr](https://github.com/tldr-pages/tldr)，
但它主要是命令示例页，主题和章节复杂度低于 MDN。

## 2. 获取固定版本

不要直接用持续变化的 `main` 作为长期 benchmark 输入。当前已验证的版本是：

```text
repository: https://github.com/mdn/content.git
commit: ab710051a7a7cd8d74123b659a57483c3e8a5948
subset: files/en-us
```

浅克隆并只检出英文文档：

```bash
mkdir -p /private/tmp/agentwiki-public-corpus
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/mdn/content.git \
  /private/tmp/agentwiki-public-corpus/mdn-content
git -C /private/tmp/agentwiki-public-corpus/mdn-content \
  fetch --depth 1 origin ab710051a7a7cd8d74123b659a57483c3e8a5948
git -C /private/tmp/agentwiki-public-corpus/mdn-content \
  checkout --detach ab710051a7a7cd8d74123b659a57483c3e8a5948
git -C /private/tmp/agentwiki-public-corpus/mdn-content \
  sparse-checkout set files/en-us
```

运行时 corpus 根目录不是仓库根，而是：

```text
/private/tmp/agentwiki-public-corpus/mdn-content/files/en-us
```

本次 checkout 统计为 14,621 篇 Markdown，约 201 MB。`/private/tmp` 是临时位置，可能被
系统清理；需要长期复现时，应将 corpus 放在稳定的本地数据目录，并记录完整 commit 和
文件 manifest。不要把整个 MDN corpus 提交到 AgentWiki 仓库。

## 3. 查询集和标注方法

查询集使用 JSONL，一行一个 `BenchmarkQuery`。格式定义见
[`benchmarks/schema.py`](schema.py)，示例查询见：

- [`mdn-smoke.jsonl`](queries/mdn-smoke.jsonl)：全量英文 MDN smoke 集；
- [`mdn-global-objects-smoke.jsonl`](queries/mdn-global-objects-smoke.jsonl)：JavaScript
  Global Objects 子集，用于快速比较 keyword 和 hybrid；
- [`zh-team-wiki.jsonl`](queries/zh-team-wiki.jsonl)：中文团队 Wiki 集。**该集带 `section`
  级标注**，覆盖 `keyword`、`cross_document`、`recent`、`filter`、`long_document` 和
  `no_answer`，用于验证中文检索链路。语料已随仓库固化在
  [`benchmarks/corpora/zh-team-wiki/`](corpora/zh-team-wiki/)，其定位与归因结果见第 6.4 节。

### 中文查询集的能力边界

中文关键词检索按 CJK 二元组（bigram）召回，并要求**查询的每个词元都在文档中被逐字找到**。
因此中文查询应写成**短的、与文档措辞一致的主题词**（如「配置文件位置」、「候选池 去重
召回率」），而不是整句自然语言（如「配置文件放在哪里」）。

已覆盖的字符系统：Han（含常用扩展区）、假名、谚文、注音、泰/老/藏/缅/高棉；单字与多字
子串（如「生」「令牌」「エンジン」）均可命中。**仍未覆盖**的是语义等价：

- **整句提问**（如「如何更新配置」）会因「如何」不在文档中而整体不命中；
- **近似措辞**（如文档写「变更」，查询写「变更管理」）也会不命中——关键词路不做词干化、
  同义词扩展或停用词过滤。

短语语义只能证明「字面出现」，无法在没有分词与查询改写的情况下推断语义等价。正式质量集
若要覆盖自然语言提问，需要先引入中文分词或查询改写，届时应在本段更新边界描述并补相应 qrels。

每个查询应包含：

```json
{
  "id": "mdn-array-001",
  "query": "JavaScript Array methods",
  "scope": "",
  "limit": 10,
  "category": "keyword",
  "difficulty": "easy",
  "relevance": [
    {
      "path": "web/javascript/reference/global_objects/array/index.md",
      "section": "",
      "grade": 3
    }
  ]
}
```

`grade` 采用文档/章节级分级：

- `0`：无关；
- `1`：主题相关，但不能直接支持任务；
- `2`：可作为辅助证据；
- `3`：可以直接支持任务结论。

`section` 为空时按文档路径匹配；填写章节时按“路径 + AgentWiki 实际返回的完整标题层级”
匹配，例如 `Authentication / Tokens`。允许 AgentWiki 返回同一文档的两个片段，因此评分器
会避免同一个文档级标注在 nDCG 中重复计分。

**qrels 必须至少包含 section 级标注**：只有 path 级标注时 `recall@k` 与
`document_recall@k` 恒等，无法验证「章节级证据」这一核心设计。英文 smoke 集目前仍是
path-only，属于已知欠缺；`zh-team-wiki.jsonl` 已按 section 级标注。

无答案查询必须显式设置：

```json
{
  "expected_no_answer": true,
  "relevance": []
}
```

正式质量集建议至少准备 100–200 条人工复核查询，覆盖精确标题、关键词改写、语义表达、
跨文档关系、近期查询、scope/tags/type 过滤、长文档深层章节、相似主题干扰和无答案场景。
当前仓库内的 MDN 查询文件只有 smoke 用途，不能替代正式 qrels。

## 4. 运行基准

先安装依赖：

```bash
uv sync
```

### 4.1 全量 keyword 基线

```bash
uv run python -m benchmarks.runner \
  --corpus /private/tmp/agentwiki-public-corpus/mdn-content/files/en-us \
  --queries benchmarks/queries/mdn-smoke.jsonl \
  --mode keyword \
  --output /private/tmp/agentwiki-public-corpus/mdn-keyword.json
```

### 4.2 同一子库的 keyword 对照

```bash
uv run python -m benchmarks.runner \
  --corpus /private/tmp/agentwiki-public-corpus/mdn-content/files/en-us/web/javascript/reference/global_objects \
  --queries benchmarks/queries/mdn-global-objects-smoke.jsonl \
  --mode keyword \
  --output /private/tmp/agentwiki-public-corpus/mdn-global-keyword.json
```

### 4.3 同一子库的 hybrid 基线

```bash
uv run python -m benchmarks.runner \
  --corpus /private/tmp/agentwiki-public-corpus/mdn-content/files/en-us/web/javascript/reference/global_objects \
  --queries benchmarks/queries/mdn-global-objects-smoke.jsonl \
  --mode hybrid \
  --embedding-model BAAI/bge-small-en-v1.5 \
  --repeats 3 \
  --output /private/tmp/agentwiki-public-corpus/mdn-global-hybrid.json
```

`hybrid` 首次运行会下载/加载本地 embedding 模型。模型下载、全量向量化和查询 embedding
耗时应与普通查询延迟分开解读。`--repeats 3` 是同一进程内的重复查询平均值；要测冷启动，
应分别启动多个 runner 进程。

runner 会自动生成两个文件：

- `*.json`：完整机器报告，包含 corpus 指纹、AgentWiki Git revision、索引规模、每条查询
  的原始 `ContextResult`、指标和错误；
- `*.md`：可读摘要，路径默认为 JSON 路径的同名 `.md`，也可用 `--markdown-output` 指定。

## 5. 指标解释

质量主指标：

- `Recall@k`：是否召回 grade ≥ 2 的正确章节；
- `recall_strict@k`：是否召回 grade = 3 的高度相关章节；
- `document_recall@k`：忽略章节，只看文档是否命中；
- `mrr@k`：第一个相关证据的倒数排名；
- `ndcg@k`：使用 0–3 分级相关性评估排序；
- `section_precision@k`：返回片段中相关章节的比例；
- `no_answer_false_positive_rate`：无答案查询却返回结果的比例。

`no_answer` 会从总体 Recall/MRR/nDCG 分母中排除，单独报告误报率。keyword 模式的
`semantic_unavailable_rate=1.0` 是因为该基线主动关闭 embedding，不表示查询失败；应结合
`query_failure_rate` 和 hybrid 的降级率判断运行可靠性。

性能指标：

- rebuild 文档数、chunk 数、向量数和耗时；
- 查询 p50/p95/p99、最小值和最大值；
- query degraded rate、semantic unavailable rate、query failure rate。

## 6. 本次实际结果

### 全量英文 MDN keyword smoke

本次 corpus：14,621 篇文档、136,917 个 chunk；rebuild 约 299.9 秒。

| 指标         |     结果 |
| ------------ | -------: |
| 查询 p50     | 1,446 ms |
| 查询 p95     | 1,726 ms |
| Recall@5     |    0.600 |
| MRR@5        |    0.240 |
| nDCG@5       |   0.3297 |
| 无答案误报率 |        0 |

这组只有 5 条可回答 smoke 查询，Recall 仅用于验证 runner 和标注链路，不作为产品质量
门槛。当前运行报告位于临时目录中的 `mdn-keyword.json` 和 `mdn-keyword.md`。

### 同一 JavaScript Global Objects 子库

子库为 1,014 篇文档、10,463 个 chunk；hybrid 使用 `BAAI/bge-small-en-v1.5`，生成
10,463 个向量。

| 模式    | rebuild | 查询 p50 | 查询 p95 | Recall@5 | MRR@5 | nDCG@5 | 无答案误报率 |
| ------- | ------: | -------: | -------: | -------: | ----: | -----: | -----------: |
| keyword |   3.1 s |   115 ms |   205 ms |    0.667 | 0.400 | 0.4623 |            0 |
| hybrid  | 653.1 s | 1,215 ms | 1,375 ms |    1.000 | 0.833 | 0.8770 |          1.0 |

hybrid 的 653.1 秒包含模型下载/加载和首次向量化，不能与 keyword 的 3.1 秒直接当作同一
阶段比较。这个 smoke 结果显示 hybrid 在 3 条可回答查询上排序更好，但无答案查询仍会
返回最近似结果，说明当前检索没有 semantic similarity threshold；这是后续产品策略和
性能优化的候选问题，不应在 benchmark 文档中当作已解决能力。

### 中文团队 Wiki（section 级标注，fixture 语料）

**语料现状**：本节早期引用的 5 篇自建中文 Wiki 从未提交进仓库，只存在于当时的临时目录，
现已丢失，因此当时那版数字不可复现。语料现已固化在
[`benchmarks/corpora/zh-team-wiki/`](corpora/zh-team-wiki/)，共 5 篇文档。

**它的定位是回归 fixture，不是效果证明。** 该语料按 qrels 里已经写死的 path 和 section
反向构造，存在向评测集拟合的风险，因此只能用于**回归**和**失败归因**；要回答"效果好不好"，
必须在真实团队 Wiki 上重复同样的流程。

keyword 模式结果：

| 指标 | 结果 |
| --- | ---: |
| 文档 / chunk | 5 / 20 |
| rebuild | 20 ms |
| 查询 p50 / p95 | 1.6 ms / 2.0 ms |
| Recall@1 / @3 / @5 | 0.714 / 0.786 / 0.786 |
| recall_strict@5 | 0.643 |
| MRR@5 | 0.750 |
| nDCG@5 | 0.744 |
| section_precision@5 | 0.614 |
| 无答案误报率 | 0 |

**recent 查询必须固定 mtime。** `recent` 策略按文件修改时间排序，而 git 不保留 mtime：
一次 clone、checkout 或编辑就会重排结果。实测编辑 `reference/configuration.md` 之后，
`zh-recent-empty-query` 的首位结果发生变化，`recall@1` 从 0.714 掉到 0.643。fixture 因此用
[`MTIMES.json`](corpora/zh-team-wiki/MTIMES.json) 声明自己的时间线，runner 在索引前应用它；
只有声明了该文件的语料才会被改动。固定之后质量指标与逐条检索结果**完全可复现**，只有
延迟和 rebuild 耗时仍会抖动。

### 检索损失归因

`recall@5 = 0.786` 只说明"有东西没返回"，不说明是哪一层丢的。`benchmarks/attribution.py`
把每一条 gold 标注定位到具体管线层：

```bash
uv run python -m benchmarks.attribution \
  --corpus benchmarks/corpora/zh-team-wiki \
  --queries benchmarks/queries/zh-team-wiki.jsonl \
  --mode keyword --k 5 \
  --output benchmarks/baselines/zh-team-wiki.attribution.keyword.json
```

17 条标注的归因结果：

| 层 | 数量 | 含义 |
| --- | ---: | --- |
| `retrieved` | 12 | 精确命中且排在 top-5 |
| `granularity_only` | 2 | 返回了 gold 章节的**子章节** |
| `wrong_section` | 3 | 文档命中，但章节不对 |
| 其余层 | 0 | 无 qrels 过期、无未索引、无切块缺失、无未召回 |

两个上限比率：

- **index ceiling = 1.0**：所有 gold 章节都已切块并进入索引，**切块层零损失**；
- **document recall = 1.0**：所有 gold 文档都进入了返回结果，**召回层零损失**。

**结论：5 条未命中全部是标注口径问题，没有一条是检索链路缺陷。** 具体分两类：

1. `granularity_only`（2 条）：qrels 标在 `配置说明 / 配置字段`，检索返回
   `配置说明 / 配置字段 / 索引路径`——内容正是所要的，但 `metrics.relevance_grade` 用
   section 字符串**精确相等**比对，把它判成 grade 0。这是**评分器的粒度缺陷**，不是检索失败。
2. `wrong_section`（3 条）：
   - `zh-cross-document` 未命中的是 grade 2 辅助证据，主证据（grade 3）已经排在第 1 位，
     辅助证据缺席属正常；
   - `zh-scope-filter` 是空查询 + `recent` 策略，返回的是文档级 chunk，而 qrels 标到了 H2。
     空查询下 recent 路没有相关性信号，返回哪个 chunk 是任意的，**用 section 级标注评
     recent 查询本身不成立**——同一集合里的 `zh-config-changelog` 与
     `zh-recent-empty-query` 用 path 级标注才是正确做法。

由此得到三条待办，**都不在检索算法里**：`relevance_grade` 应把「gold 章节的子章节」视为
命中并单列该口径；`zh-scope-filter` 的标注应降为 path 级；`zh-cross-document` 的 grade 2
辅助证据不应计入损失。

**归因过程同时暴露了两个真实缺陷：**

- `summarize_quality` 的**总体**指标正确排除了 `expected_no_answer`，但 category /
  difficulty **分组**没有排除，no_answer 以 0 分拉低它所属的分组。fixture 上
  `difficulty.medium.recall@1` 曾报 `6/9 = 0.667`，正确值是 `6/8 = 0.75`。已修复，并由
  `test_quality_summary_excludes_no_answer_cases_from_group_metrics` 守住。
- **no_answer 用例的前提是语料里确实不存在该主题。** 重建 fixture 时「备份」一词曾意外
  出现在 `reference/configuration.md`，使 `zh-no-answer`（"Kubernetes etcd 备份"）的误报率
  虚高到 1.0。`attribution.py` 的 `no_answer_contamination` 字段现在会自动报出这类污染。

**延迟口径**：`latency_ms` 包住 `get_wiki_context`，其中包含查询前的增量确认
（`ensure_fresh`）。小语料下增量确认已由目录级缓存短路，因此该值接近纯检索耗时；
在大语料上应把它理解为「增量确认 + 检索」，不要单独归因于检索。

## 7. 下一次复现清单

1. 固定 corpus 路径、上游 commit、英文子目录和 SHA-256 manifest；
1. 若查询集含 `recent` 用例，确认语料带有 `MTIMES.json`，否则结果会随文件 mtime 漂移；
1. 检查查询集是否仍引用存在的相对路径和完整 section 名称；
1. 在同一个 corpus、同一份 qrels 上分别运行 keyword 与 hybrid；
1. 质量集和性能集分开记录，首次模型下载不纳入查询 p50/p95；
1. 正式报告至少重复查询延迟 3 次，并保存 JSON 原始结果；
1. 对新增、修改、删除、解析失败和 embedding 失败场景执行 `mutations.jsonl` 对应检查；
1. 更新 benchmark 结果时记录机器、Python、AgentWiki Git revision 和 embedding 模型版本。
