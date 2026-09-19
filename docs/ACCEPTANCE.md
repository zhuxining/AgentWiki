# AgentWiki 验收清单

> 本文只保留当前实现的验收入口，不记录已经解决的历史 GAP 或逐轮开发日志。
> 契约优先级： [MCP 契约](MCP_TOOLS.md) → [架构](ARCHITECTURE.md) → [规则参考](RULES.md) → [工程规范](../AGENTS.md)。

## 1. 验收范围

验收以当前 Rust 实现为准，覆盖以下四组行为：

| 组别 | 核心检查 | 主要证据 |
| --- | --- | --- |
| 工程与配置 | edition、feature、配置、自举、投影隔离、路径安全 | `Cargo.toml`、`src/config.rs`、`src/document/path.rs` |
| 同步与检索 | 增改删移、失败重试、关键词/语义/混合检索、过滤、近期查询 | `src/projection/`、`src/retrieval/`、`tests/` |
| 规则与写回 | 规则合并、Frontmatter、链接、格式检查、冲突保护 | `src/governance/`、`docs/RULES.md` |
| 入口与协议 | CLI/MCP 参数、结果结构、错误和异步边界 | `src/cli.rs`、`src/mcp.rs`、`docs/MCP_TOOLS.md` |

## 2. 必须满足的行为

### 工程、配置与安全

- [ ] `cargo fmt`、Clippy、默认 feature 测试和 `git diff --check` 通过。
- [ ] `cargo build --bin agentwiki-mcp` 通过；缺少 `protoc` 时给出明确环境错误。
- [ ] 首次运行创建默认配置和 Wiki 规则；已有 `AGENTWIKI.md` 不被覆盖。
- [ ] CLI 的 `--wiki-root` 优先于配置；相对路径按配置目录解析；投影按规范化 Wiki 根目录隔离。
- [ ] 拒绝越界路径、绝对相对路径和外部符号链接；规则文件未知字段报 `rules.parse`。

### 同步与检索

- [ ] Markdown 是唯一事实源；索引可删除重建，索引失败不覆盖原文。
- [ ] 未变化文件跳过解析；内容哈希相同只刷新指纹且不解析；内容变化只解析一次；删除、唯一内容哈希移动和失败重试语义正确。
- [ ] 同内容只改 mtime 时，文档与片段的时间一致，时间过滤与 recent 查询反映真实 mtime。
- [ ] 向量按模型身份、维度和嵌入输入哈希复用，只对变化切片推理；指纹写入失败或中断后下次同步可重试。
- [ ] 投影版本不匹配只重建 `wiki_rows`；中断的建索引在下次打开或同步后补齐。新增行未并入索引时仍可召回。
- [ ] 关键词检索使用 LanceDB FTS；启用模型时可追加语义检索，失败保留关键词能力并报告 `degraded`。
- [ ] 空查询按真实文件 mtime 返回近期文档；scope、标签、类型、元数据和时间过滤在召回前生效。
- [ ] 每个单元的 `search_text` 不超过 512 token 预算；中文长章节被切分为多个片段且无内容丢失；超长摘要截断后仍能被文档级检索命中；`MarkdownSplitter` 不切开能装下的代码块和段落。
- [ ] scope 含 `_`、`%` 时按字面路径匹配；YAML 整数与 JSON 整数元数据过滤一致。
- [ ] `keywords` 的 any/all 是硬约束，query 命中或语义候选不能绕过；`order=modified_desc` 返回全部匹配项中最新的结果。
- [ ] match_sources 反映每条结果实际参与的检索腿，不由启用配置推断。
- [ ] document 与 fragment 分开返回；精确路径/文件名优先；关系默认关闭，显式开启时最多返回一跳关系。
- [ ] 正常无匹配返回空结果，不伪装为系统故障；`rank_score` 不被解释为概率或置信度。

### 规则、校验与写回

- [ ] `type`、`tags`、`summary` 为内置必填字段；其他必填字段由规则声明并按匹配范围合并。
- [ ] 默认校验只报告；`fix_format=true` 且范围明确时才写回。
- [ ] `path` 与 `full` 互斥，缺少范围时报错；规则文件不进入普通文档格式修复。
- [ ] 格式修复保留 Frontmatter 和代码块，写回前检查冲突，修复后重新校验。
- [ ] 问题类型与级别符合 [规则参考](RULES.md)，包括 `markdown.formatting`、`format.conflict` 和 `format.failed`。

### CLI 与 MCP

- [ ] CLI 与 MCP 共用 `AgentWiki` 用例层，查询、校验和默认值语义一致；`optimize-index` 是唯一的显式索引并入入口，查询路径不隐式 optimize。
- [ ] MCP 提供 `get_wiki_context`、`get_wiki_rules`、`validate_wiki` 三个工具。
- [ ] `get_wiki_context` 返回 query、scope、strategy、degraded、documents、fragments、relations、truncated。
- [ ] `get_wiki_rules` 返回有效规则、动态 `known_tags`、Wiki 根目录、指引正文和规则文件指纹。
- [ ] `validate_wiki` 在只读和修复模式返回同一结构；路径输出可直接交给原生文件工具。

## 3. 本地验证命令

在仓库根目录执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --bin agentwiki-mcp
git diff --check
```

黑盒验证应使用仓库外的临时 Wiki，覆盖：首次自举、增改删移、解析失败重试、中文/中英混合/代码标识符查询、过滤、无答案、规则错误、默认只读、格式冲突和 MCP stdio 生命周期。

## 4. 当前非阻塞事项

- 固定语料的正式检索基准仍需独立维护，不能用小样本结果代表长期质量。
- 模型缓存目录的具体管理方式以 FastEmbed 实际行为为准；文档不把未显式创建的目录当作功能保证。
- 标题只作为章节路径时的部分匹配效果需要在固定语料中验证；精确路径、文件名和 alias 仍走精确匹配。
