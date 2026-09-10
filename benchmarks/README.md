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
  Global Objects 子集，用于快速比较 keyword 和 hybrid。

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

## 7. 下一次复现清单

1. 固定 corpus 路径、上游 commit、英文子目录和 SHA-256 manifest；
1. 检查查询集是否仍引用存在的相对路径和完整 section 名称；
1. 在同一个 corpus、同一份 qrels 上分别运行 keyword 与 hybrid；
1. 质量集和性能集分开记录，首次模型下载不纳入查询 p50/p95；
1. 正式报告至少重复查询延迟 3 次，并保存 JSON 原始结果；
1. 对新增、修改、删除、解析失败和 embedding 失败场景执行 `mutations.jsonl` 对应检查；
1. 更新 benchmark 结果时记录机器、Python、AgentWiki Git revision 和 embedding 模型版本。
