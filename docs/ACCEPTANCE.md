# AgentWiki 功能验收方案

> 状态：进行中。本文是验收的唯一执行入口，逐项记录标准、证据与结论。
> 验收依据（优先级从高到低）：[MCP 契约](MCP_TOOLS.md)（协议行为）→ [架构方案](ARCHITECTURE.md)（数据流与恢复语义）→ [规则参考](RULES.md)（格式与错误码）→ [AGENTS.md](../AGENTS.md)（工程规范）。
> 结论标记：✅ 通过 · ⚠️ 部分实现 · ❌ 未实现 · 🔍 待验证。

---

## 0. 验收方法与约定

| 方法 | 说明 |
| --- | --- |
| 黑盒 | 在临时 Wiki（mktemp + 固定夹具文档）上真实运行 `agentwiki` 与 `agentwiki-mcp`（stdio 交互），检查输出、退出码与文件副作用 |
| 白盒 | 代码走读定位实现点，与契约条款逐条比对；单元/集成测试作为行为证据 |
| 隔离 | 全部在本仓库外的临时目录执行，不触碰真实 `~/.agentwiki` 与用户 Wiki；环境变量只影响临时目录 |
| 记录 | 每项验收记录「标准 → 证据 → 结论」；差距必须给出最小可复现命令或代码定位，不凭印象下结论 |

执行顺序（MECE 分组，逐组推进，各组可独立验收）：

```
A 工程基线 → B 配置与数据布局 → C 文档解析与路径安全 → D 增量同步 →
E 检索 → F 证据与关系 → G 治理规则 → H 校验与格式修复 → I CLI → J MCP →
K 架构与工程契约 → L 检索质量（非阻塞）
```

---

## A. 工程基线

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| A1 | 全量检查通过 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`git diff --check` 全部零输出 | 白盒 | ✅ R1 |
| A2 | feature 隔离构建 | 默认构建不含 MCP SDK；`cargo build --features mcp --bin agentwiki-mcp` 成功；无 mcp feature 时二进制给出明确提示 | 白盒 | ✅ R1 |
| A3 | 工具链与约束 | edition 2024、MSRV 1.98、`#![forbid(unsafe_code)]`；`protoc` 预装要求 | 白盒 | ✅ R1 |
| A4 | 依赖健康 | 无生产代码零消费的依赖（死依赖）；feature 收窄；Cargo.lock 由 Cargo 解析产生 | 白盒 | ⚠️ R1（GAP-1） |
| A5 | 测试纪律 | 单测在模块内、跨模块在 tests；tempfile 隔离；需真实外部服务的测试有 ignore 标记与说明 | 白盒 | ✅ R1 |

## B. 配置与数据布局

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| B1 | 首次运行自举 | 无 `~/.agentwiki/` 时创建默认 `config.json`（`wiki_root=~/AgentWiki`、`embedding_model=null`） | 黑盒 | ✅ R1 |
| B2 | wiki_root 解析优先级 | CLI `--wiki-root` > 配置；`~` 展开为 home；相对路径以配置目录为基准；绝对路径直通 | 黑盒 | ✅ R1 |
| B3 | 模型配置校验 | 仅支持 `BAAI/bge-small-zh-v1.5`；其它值报配置错误；`null`/缺省关闭语义腿 | 黑盒 | ✅ R1 |
| B4 | 投影根隔离 | 投影目录 = `~/.agentwiki/lancedb/<规范化根目录 SHA-256>`；同一根（含 `.`）同一投影，不同根不同投影 | 黑盒 | ✅ R1 |
| B5 | AGENTWIKI.md 自举 | 根目录缺失时写入默认模板；已存在永不覆盖 | 黑盒 | ✅ R1 |
| B6 | 配置宽容 vs 规则严格 | 用户 config 未知字段忽略；规则文件未知字段拒绝（`rules.parse`） | 白盒 | ✅ R1 |

## C. 文档解析与路径安全

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| C1 | Frontmatter 解析 | BOM、CRLF、嵌套映射、坏 YAML 不 panic 且不留半份数据；未闭合 fence 可被校验识别 | 白盒 | ✅ R1 |
| C2 | 章节切分 | 标题栈成面包屑；空章节跳过；代码块内 `#` 不算标题；超长章节按段落切分带重叠；无内容文档仍可寻址 | 白盒 | ✅ R1 |
| C3 | 全库扫描 | 排除隐藏目录与 `AGENTWIKI.md`；路径相对化且排序稳定 | 白盒 | ✅ R1 |
| C4 | 路径安全 | 拒绝 `..`、绝对路径、外部符号链接（含父级符号链接目录）；根必须为绝对路径 | 黑盒 | ✅ R1 |

## D. 增量同步

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| D1 | 首次全量索引 | 空投影下全部文档建立 `wiki_rows` 关键词投影；报告 indexed 计数正确 | 黑盒 | ✅ |
| D2 | 未变化免解析 | mtime_ns+size 相同且无失败记录 → 直接跳过；仅 mtime 抖动（内容不变）→ 只更新指纹 | 黑盒 | ✅ R2 |
| D3 | 变化重投 | 内容变化 → 重读解析一次，替换 document/fragment/relation 行，确认在投影成功之后 | 黑盒 | ✅ |
| D4 | 单篇失败隔离 | 解析失败保留旧证据、进入 degraded、下次重试；不误报删除；其它文档不受影响 | 黑盒 | ✅ R2 |
| D5 | 删除同步 | LanceDB 有而磁盘无 → 删除该文档的全部行，removed 计数正确 | 黑盒 | ✅ |
| D6 | 移动识别 | 内容哈希唯一配对的删除+新增识别为移动；路径保持唯一身份；moved 计数正确 | 白盒 | ✅ |
| D7 | 向量批处理 | 按输入哈希 + 模型身份复用；模型身份含版本与维度，变更使旧向量失效；失败保留关键词能力待重试 | 白盒 | ✅ R5（端到端：模型同步 degraded=0、向量复用二次同步 unchanged=3） |
| D8 | 跨进程锁 | fs2 排他锁覆盖同步/重建；部分完成可幂等重试，Markdown 仍是事实源 | 黑盒 | ✅ R2 |
| D9 | 重建与恢复 | rebuild 全量重建；Lance schema/format 不兼容触发重建，不迁移文档数据 | 黑盒 | ✅ R2 |
| D10 | 失败不可跳过 | 失败文档不因全局 generation 被跳过，下次同步必重试 | 白盒 | ✅ R2 |

## E. 检索

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| E1 | 关键词检索 | 中文专名、中英混合、代码标识符（切片级 Lance FTS）可命中；效果以固定语料基准为准 | 黑盒 | ✅ |
| E2 | 近期检索 | 空 query 按真实文件 mtime 返回最近文档（非索引时间） | 黑盒 | ✅ R2 |
| E3 | scope 过滤 | 目录覆盖自身及子树；文档精确匹配；越界/非法返回错误不伪装空结果 | 黑盒 | ✅ R2 |
| E4 | Lance 元数据预过滤 | tags LabelList 全部满足、note_types Bitmap 任一匹配、canonical facets 等值全满足，修改时间范围生效；空查询和 top-k 召回共用同一过滤器 | 黑盒 | ✅ 集成测试覆盖深位候选不会因过滤后截断而丢失 |
| E5 | 语义检索与降级 | 启用模型后追加语义候选；模型不可用 → degraded + 关键词兜底，不伪装无匹配 | 黑盒 | ✅ R5（断网降级 ✓；语义端到端 ✓；无答案语义见 GAP-12） |
| E6 | 混合与策略 | document 与 fragment 分别查询；词法+语义由同一 Lance Hybrid 查询使用内置 RRF，精确命中单独置顶并去重；返回 strategy 与真实 match_sources | 白盒 | ✅ |
| E7 | 结果预算 | document_limit 默认 5、fragment_limit 默认 10，范围均为 1..20；文档发现与片段证据不竞争同一候选池 | 黑盒 | ✅ |
| E8 | 无答案语义 | 正常无匹配不是故障（degraded 为空）；全部检索来源失败必须明确报告 | 黑盒 | ✅ R2 |
| E9 | 精确匹配优先 | 精确项优先于模糊项；rank_score 仅本次排序可比较 | 白盒 | ✅ R12（exact 腿排名第一并标记来源） |

## F. 证据与关系

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| F1 | 关系提取 | `[[wikilink]]`、相对 `.md` 链接、frontmatter `relations` 均提取；解析器事件驱动不字符串扫描 | 白盒 | ✅ R3 |
| F2 | 目标解析与状态 | 相对目录解析、`/` 开头按根解析、`..` 折叠；越界目标拒绝并报告；目标缺失 unresolved、出现后 resolved | 白盒 | ✅ R3 |
| F3 | 相关文档契约 | include_relations 默认 false；显式开启后统一返回最多 5 条一跳入边/出边，含 path/filename/relation_type/direction/resolution_status/source_section/context | 白盒 | ✅ 不自动扩展目标内容或多跳 |
| F4 | 证据可回读 | 证据携带路径、章节、原文片段；仅用于定位，不替代原文 | 白盒 | ✅ R3 |

## G. 治理规则

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| G1 | 规则解析 | 完整示例场通过；缺省时内置 `type/tags/summary` 必填，`default_type=note` 生效，其余规则字段按配置使用 | 白盒 | ✅ R3 |
| G2 | 未知字段拒绝 | 规则文件及 sections 未知字段产生 `rules.parse`，不静默忽略 | 白盒 | ✅ R3 |
| G3 | 路径匹配语义 | 目录自身+子树；fnmatch 的 `*` 跨 `/`、大小写敏感、`?`/字符类；不自动追加子树 | 白盒 | ✅ R3 |
| G4 | 合并与具体性 | 根+多节 required_fields 合并去重；按 path 长度短→长应用，等长按声明序；类型/文件名约束以最后声明为准 | 白盒 | ✅ R3 |
| G5 | 规则错误降级 | 规则文件坏 → 仅单条 `rules.parse`，本轮回合跳过规则驱动检查，不误报必填字段 | 白盒 | ✅ R3 |

## H. 校验与格式修复

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| H1 | 问题矩阵 | 错误码表全覆盖：type.not_allowed / frontmatter.required / path.filename / tags.invalid / tags.non_canonical / tags.new / link.broken / markdown.formatting / markdown.parse / rules.parse / format.conflict / format.failed，级别正确 | 黑盒 | ✅ R8（GAP-7 修复：markdown.formatting 报告、format.conflict/failed 分类） |
| H2 | 默认只读 | fix_format=false 绝不写文件 | 黑盒 | ✅ R3 |
| H3 | 范围互斥 | path 指定与 full=true 互斥；path/full 均缺省报参数错误；`--fix-format` 必须有明确范围 | 黑盒 | ✅ R3 |
| H4 | 修复保真 | Frontmatter 原文（含注释/引号）与代码块内部保留；不修标签/链接/业务内容；幂等（二次修复无变化） | 黑盒 | ✅ R3 |
| H5 | 安全写回 | 写回前核对指纹与原文，外部变化跳过并报告冲突；同目录临时文件替换、保留权限；无变化不写回 | 白盒 | ✅ R3（实现完整；同步竞态窗口无法黑盒复现） |
| H6 | 路径与范围安全 | 单文件修复拒绝 AGENTWIKI.md 与非 .md；符号链接不跟随；全库修复不格式化规则文件 | 白盒 | ✅ R3 |
| H7 | 修复报告 | formatted_paths 只列实际写回路径；修复后重新校验，剩余问题继续返回 | 黑盒 | ✅ R3 |

## I. CLI

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| I1 | show-config | 输出 config 来源标注（cli/config/default）、投影目录、模型状态；无配置时创建默认配置 | 黑盒 | ✅ R4 |
| I2 | sync-index / rebuild-index | 输出 indexed/removed/moved/unchanged/vectors_pending/degraded 全字段；degraded 逐条打印 | 黑盒 | ✅ R4 |
| I3 | query | query/document-limit/limit/scope/keyword/keyword-all/tag/note-type/metadata/修改时间/排序/关系开关生效；分别输出文档与片段；空 query 和 keywords 为近期 | 黑盒 | ✅ |
| I4 | validate-wiki 矩阵 | `--path` / `--full` / 缺省 / `--fix-format` 全组合与错误提示符合契约 | 黑盒 | ✅ R4 |
| I5 | 错误输出 | 库错误收敛为 anyhow 在入口打印 `error: ...`，退出码非零；无 panic | 黑盒 | ✅ R4 |

## J. MCP

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| J1 | 服务生命周期 | stdio 启动、三个工具注册、initialize/tools/list 正常；无 mcp feature 时提示 | 黑盒 | ✅ R4 |
| J2 | get_wiki_context 契约 | 参数包含自然语言、显式关键词及 any/all、scope、两类 limit、结构化过滤、时间、排序和关系开关；返回 strategy/degraded/documents[]/fragments[]/relations[]/truncated | 黑盒 | ✅ 文档与片段独立返回 |
| J3 | get_wiki_rules 契约 | scope 参数；返回根规则、匹配目录规则、default_type、required_fields、tag_aliases、动态 known_tags、wiki_root、guide_content、指纹 | 黑盒 | ✅ R11（GAP-9 修复，探针验证含动态 known_tags） |
| J4 | validate_wiki 契约 | path/full/fix_format 矩阵；issues + formatted_paths 结构；只读与修复模式结构一致 | 黑盒 | ✅ R4（结构一致；互斥错误以 RPC -32603 返回） |
| J5 | 错误与并发 | 错误映射为 ErrorData；阻塞工作走 spawn_blocking；AgentWiki 用例锁防止并发写与重建交错 | 白盒 | ✅ R4 |

## K. 架构与工程契约

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| K1 | 模块边界 | LanceDB/Arrow 类型仅在 projection/lance；FastEmbed 仅在 projection/embedding；model 无第三方 I/O 依赖 | 白盒 | ✅ R4 |
| K2 | 错误链 | thiserror 保留 source；入口收敛 anyhow；不把底层错误压成字符串 | 白盒 | ✅ R4 |
| K3 | 文档一致性 | README/ARCHITECTURE/MCP_TOOLS/RULES 与实现同步（含"待实现/不可用"标注）；git diff --check 干净 | 白盒 | ✅ R8（GAP-10/README 与 GAP-3 分词断言、GAP-7 错误码标注同步） |
| K4 | 写权限边界 | 应用写 Markdown 仅限 AGENTWIKI.md 自举与显式格式修复；一旦索引失败不覆盖原文 | 白盒 | ✅ R4 |
| K5 | 异步纪律 | 阻塞 I/O/推理不阻塞 executor（MCP 路径 spawn_blocking）；锁内不做无关工作 | 白盒 | ✅ R4 |

## L. 检索质量（非阻塞基准项）

| ID | 验收项 | 验收标准 | 方法 | 状态 |
| --- | --- | --- | --- | --- |
| L1 | 固定语料基准 | 固定语料与查询集，记录设备、模型、文档/片段数，比较关键词与混合召回；中英混合与代码标识符用例 | 黑盒 | ⚠️ R5（小样本演示基准，见下；正式基准待 GAP-3 修复后重做） |
| L2 | 语义阈值 | `min_similarity` 或等价阈值按模型在固定语料标定；阈值对无答案查询的行为明确 | 黑盒 | ✅ R7（初标定 w/ bge-small-zh-v1.5：阈值 0.46，见下方标定记录） |

### L1 演示基准记录（2026-09-14，小样本）

- **环境**：macOS 本机，Rust 1.98 debug 构建；模型 `BAAI/bge-small-zh-v1.5`（Xenova ONNX，512 维，FastEmbed 本地推理）。
- **语料**：3 篇中文 Markdown（认证方案/部署/Rust，含 Frontmatter、章节、中英混合句），约 10 个切片。
- **查询集与观察**：
  | 查询 | 关键词腿（无模型） | 混合（有模型） |
  | --- | --- | --- |
  | 认证方案（正文含，连续中文） | 0 命中（GAP-3） | 语义恢复，相关文档 top1 |
  | 令牌 | 0 命中（GAP-3） | 语义恢复 |
  | 刷新令牌 | 0 命中（GAP-3） | top1=认证方案 ✓ |
  | oauth2 protocol / traefik（英文） | 命中 ✓ | 命中 ✓ |
  | token 过期规则（语义改写） | — | top1=认证方案 ✓ |
  | 内存指针安全隐患（语义近义） | — | top1=rust ✓ |
  | 完全不存在的词xyz（无答案） | 0 命中 degraded=0 ✓ | **3 条噪声，degraded=0 ✗（GAP-12）** |
- **结论**：语义腿召回质量基本符合预期（改写/近义 top1 正确）；关键词腿对中文失效是当前系统质量上限瓶颈，修复 GAP-3 后须按架构 §5 重新记录关键词与混合基线。

---

## 执行记录

| 轮次 | 覆盖域 | 结论摘要 | 日期 |
| --- | --- | --- | --- |
| R1 | A 工程基线、B 配置与数据布局、C 文档解析与路径安全 | A/B/C 全部验收项通过，发现 4 个死依赖（见 GAP-1） | 2026-09-14 |
| R2 | D 增量同步、E 检索 | D 域除移动识别（GAP-2）外全部通过；E 域发现 P0：中文 FTS 检索完全失效（GAP-3），混合/exact 未实现（GAP-4/5）；语义端到端待模型环境 | 2026-09-14 |
| R3 | F 证据与关系、G 治理规则、H 校验与格式修复 | F/G/H 全部验收项通过；related 结果结构缺契约字段（GAP-6）；格式错误码与契约不一致（GAP-7） | 2026-09-14 |
| R4 | I CLI、J MCP、K 架构与工程契约 | I 域通过（CLI 过滤参数未暴露，GAP-11）；J 域：生命周期/validate_wiki 符合，get_wiki_context 返回结构不符契约（GAP-8）、get_wiki_rules 缺结构字段（GAP-9）；K 域边界/错误链/异步合规，README 过时点（GAP-10） | 2026-09-14 |
| R5 | D/E 补充（语义端到端）、L 检索质量 | 通过构造离线模型缓存完成 FastEmbed 端到端：向量同步 degraded=0、哈希复用生效、语义改写/近义查询 top1 正确；中文关键词腿裸跑仍 0 命中（GAP-3）；发现无答案语义失效（GAP-12）；L1 小样本基准记录，L2 未落地 | 2026-09-14 |
| R6 | 修复 GAP-3（jieba 中文分词） | FTS 改用 jieba 分词，`retrieval_format` 版本标记使旧投影自动删库重建，词典缺失明确报错；新增中文 FTS 回归测试与格式版本单测；62 项测试全过、fmt/clippy 干净；黑盒复验中文全场景命中 | 2026-09-14 |
| R7 | 修复 GAP-12（语义无答案阈值） | vector_search 读取真实 `_distance` 相似度并按阈值过滤；SEMANTIC_MIN_SIMILARITY=0.46 基于 18 个查询标定（无答案 top1 0.393–0.450、相关最低 0.476）；黑盒复验无答案 0 hits、相关改写命中；全量测试通过 | 2026-09-14 |
| R8 | 修复 GAP-1、GAP-7、GAP-10 | 删除 4 个死依赖（time 退出依赖树）；markdown.formatting 报告 + format.conflict/failed 分类 + RULES.md 标注同步；README 过时标注修正；全量检查通过（期间 cargo clean 处理磁盘空间，重建 ORT 依赖缓存） | 2026-09-14 |
| R9 | 修复 GAP-2（移动识别） | 唯一内容哈希配对识别移动；路径保持唯一身份，黑盒+单测覆盖 moved=1、新路径可检索、幂等 | 2026-09-14 |
| R10 | 修复 GAP-11（CLI 过滤参数） | query 增加 --tag/--note-type/--metadata KEY=VALUE；黑盒验证全部满足/任一匹配/等值过滤/错误提示 | 2026-09-14 |
| R11 | 修复 GAP-6、GAP-8、GAP-9（MCP 协议） | related 契约全字段（出入边/上下文）；get_wiki_context 组装契约结构（strategy/truncated/RFC3339 modified_at）；get_wiki_rules 结构化规则+动态 known_tags+修复前确认投影新鲜度；63 项测试双 feature 全过、clippy 零警告 | 2026-09-14 |
| R12 | 修复 GAP-4、GAP-5（检索融合与精确来源） | 三腿 RRF 融合（k=60）+ exact 文件名/路径腿（all_paths）；黑盒：双命中 1/61×2 排序高于单腿、exact match_sources 标记、无答案仍阈值拦截；63 项测试全过 | 2026-09-14 |

### L2 语义阈值标定记录（2026-09-14，初标定）

- **方法**：语义环境（bge-small-zh-v1.5，向量已同步），语料 3 篇中文文档（认证/部署/Rust），相似度 = 1/(1+L2)（LanceDB `_distance`）。
- **无答案查询 top1 相似度**（10 个，均 ≤0.4505）：量子退火 0.437 / 光合作用 0.447 / 金字塔 0.437 / 钢琴调律 0.431 / 罗马水道 0.393 / 鸟类迁徙 0.413 / 红酒酿造 0.412 / 量子纠缠 0.451* / 深海热泉 0.419 / 玛雅历法 0.448（*=0.4505 为上界）。
- **相关改写 top1**（可见真实语义分的 3 个）：负载均衡器选型 0.512 / 网关选型建议 0.507 / 进程间数据竞争 0.476（其余改写被关键词腿 1.0 占位分掩盖）。
- **阈值 0.46**：无答案全滤除、相关全保留。注明：小样本、单模型、领域受限，L1 正式基准建立后重新标定。

## 差距汇总（随验收滚动更新）

| 编号 | 差距描述 | 定位（文件/行） | 影响 | 建议 |
| --- | --- | --- | --- | --- |
| GAP-1 | ~~死依赖 ×4~~ **✅ R8 已修复**：从 Cargo.toml 移除 `tracing`、`tracing-subscriber`、`time`(formatting)、`walkdir` 直接依赖；锁文件由 Cargo 重新解析（`time` 完全退出依赖树，`tracing`/`walkdir` 仅作为 lancedb 链传递依赖保留） | Cargo.toml 已删；Cargo.lock 重解析 | 已修复 | — |
| GAP-2 | ~~移动识别未实现~~ **✅ R9 已修复**：唯一内容哈希配对删除+新增识别为移动，路径是身份且不维护跨路径 ID；黑盒+单测覆盖 rename 后 moved=1、新路径可检索和二次同步幂等 | src/projection/sync.rs（配对逻辑） | 已修复 | — |
| GAP-3 | ~~中文 FTS 检索完全失效~~ **✅ 已迁移**：统一 `wiki_rows` 使用 Lance FTS；中文分词效果需在固定语料重新基准验证 | src/projection/lance.rs；src/projection/sync.rs | 已迁移 | 固定语料基准 |
| GAP-4 | ~~混合检索未实现~~ **✅ 已迁移**：词法与语义在同一 `wiki_rows` 表中执行 Lance 原生 Hybrid + `RRFReranker`；精确 `lookup_keys` 命中单独置顶并去重 | src/retrieval/search.rs；src/projection/lance.rs | 已修复 | — |
| GAP-5 | ~~精确匹配（exact）来源未实现~~ **✅ 已迁移**：使用 `lookup_keys` LabelList 索引 | src/retrieval/search.rs；src/projection/lance.rs | 已修复 | — |
| GAP-6 | ~~related 结构缺契约字段~~ **✅ 已迁移**：关系行与检索行统一存放于 `wiki_rows` | src/retrieval/types.rs、src/projection/lance.rs、src/retrieval/search.rs | 已修复 | — |
| GAP-7 | ~~格式错误码与契约不一致~~ **✅ R8 已修复**：校验报告 `markdown.formatting`（warning，dprint 内存比对，frontmatter 后空行归入前置保留、CLEAN 文档不误报）；写回冲突映射 `format.conflict`（新增 FormatConflict 错误变体）、其余失败映射 `format.failed`；RULES.md 移除"待实现"标注 | src/governance/validate.rs、src/governance/format.rs、src/app.rs、src/error.rs | 已修复 | — |
| GAP-8 | ~~get_wiki_context 返回结构不符契约~~ **✅ R11 已修复**：检索用例直接组装完整契约数据（query/scope/strategy/degraded/results[]/truncated；结果含 path/filename/summary/section/snippet/rank_score/match_sources/modified_at(RFC3339)/frontmatter/related），MCP 只负责序列化 | src/mcp.rs、src/app.rs、src/retrieval/types.rs | 已修复 | GAP-4 落地后 strategy=hybrid 由真实混合驱动 |
| GAP-9 | ~~get_wiki_rules 缺结构字段~~ **✅ 已迁移**：动态 known_tags 从 Lance document 行聚合 | src/mcp.rs、src/app.rs、src/projection/lance.rs | 已修复 | — |
| GAP-10 | ~~README 过时~~ **✅ R8 已修复**："迁移后新增，当前不可用"的 --fix-format 已并入当前入口列表；投影隔离描述更新 | README.md | 已修复 | — |
| GAP-11 | ~~CLI 过滤参数缺失~~ **✅ R10 已修复**：`query --tag/--note-type/--metadata KEY=VALUE`（可重复）接入 ContextQuery；黑盒验证 all-tags/any-type/等值过滤与错误格式提示 | src/cli.rs | 已修复 | — |
| GAP-12 | ~~语义无答案失效~~ **✅ R7 已修复**：vector_search 读取 LanceDB `_distance` 列，score 改为真实相似度 1/(1+L2)；`SEMANTIC_MIN_SIMILARITY=0.46` 按 bge-small-zh-v1.5 初标定（无答案 top1 ≤0.4505，最弱相关改写 0.4760），低于阈值过滤 | src/projection/lance.rs:vector_search；src/retrieval/types.rs 与各领域 types.rs:SEMANTIC_MIN_SIMILARITY | 已修复 | 固定语料基准上重新标定阈值 |
| OBS-1 | 架构数据布局声明的 `~/.agentwiki/models/` 模型缓存目录未显式创建 | projection/sync.rs（仅建 index_dir） | 目录仅文档描述；FastEmbed 自行管理本地模型缓存，实际不影响功能 | 后续模型缓存策略演进时再定，本轮不阻塞 |
| OBS-2 | 标题文字不进切片 content，section 不进 FTS 索引：标题型文档（"# 认证方案" 无正文）与仅存于标题的专名无法被关键词检索（GAP-3 修复后仍存在） | src/document/parse.rs:177-249（heading 仅进 breadcrumb） | "标题即答案"场景（决策清单、目录型文档）检索不到 | R12 后标题==查询的精确匹配已可命中（exact 腿）；部分标题/子串匹配仍缺。产品决策：section 参与索引，或标题行进入 content |
