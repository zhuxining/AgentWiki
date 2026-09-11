# AGENTS.md

本文件规定 AgentWiki 的工程边界。产品、目录与数据流以 [架构与迁移方案](docs/ARCHITECTURE.md) 为准；协议以 [MCP 契约](docs/MCP_TOOLS.md) 为准。

## 开发目标与迁移状态

- 始终使用简体中文沟通；优先给出结论、依据和可验证结果，避免重复说明。
- 少写通用代码，优先标准库和已采用组件；新增包必须替代明确的自写职责，不为预留能力引入依赖。
- 源码已按 LanceDB 架构完成基础目录迁移；修改前仍需读取实际 Cargo.toml、源码和测试，不能仅凭设计文档推断行为。
- 目录迁移与依赖切换已经落地，后续改动保持现有功能边界，不重新引入旧的扁平模块。
- Rust 为唯一维护实现，legacy/python 为历史归档，不参与开发或 Rust 验收。
- 复杂改动先明确目标、边界与验收；存在影响契约的歧义时说明取舍，不擅自扩大范围。

## 工具链与依赖

- 保持单 package、共享库和 CLI/MCP 两个二进制；edition 2024，当前 MSRV 1.98。工具链、版本和 feature 以 Cargo.toml 为准。
- 目标检索使用 LanceDB，推理使用 FastEmbed，元数据使用 rusqlite bundled。SQLite 不执行全文或向量查询。
- 文档解析使用 pulldown-cmark、serde_yaml，长章节切分使用 text-splitter，遍历使用 walkdir，模式匹配使用 globset，格式化使用 dprint-plugin-markdown。
- CLI 使用 clap，MCP 使用官方 rmcp 3.3 并由 mcp feature 隔离；核心库不依赖 MCP SDK。移除无消费者的旧 SDK、watcher、配置校验与测试依赖，不建立替代框架。
- 复用 tokio、serde、camino、tracing、thiserror 和 anyhow；少量配置条件直接校验。检查现有依赖是否已提供能力，再决定新增依赖。
- 收窄依赖 feature；更改清单后由 Cargo 解析锁文件，不手改 Cargo.lock。新增 API、语法或依赖不得无意提高 MSRV。
- 审查依赖维护情况、安全接口、许可证和原生构建成本；允许依赖内部使用 unsafe 或原生库，不要求传递依赖树无 unsafe。cargo audit 不能替代依赖审查。
- LanceDB 当前构建链需要 `protoc`；开发机和 CI 必须预装与平台匹配的 Protocol Buffers 编译器，并在构建前确认 `protoc --version`。不把生成的二进制提交到仓库。

## 结构与资源所有权

目标结构按功能聚合，完整目录和当前模块映射见架构文档：

```text
src/bin/         CLI / MCP 参数、协议、输出
src/document/    安全路径、读取、解析、章节切分
src/retrieval/   查询契约、策略、LanceDB、FastEmbed
src/governance/  规则、校验、格式修复
src/runtime.rs  唯一资源所有者与用例入口
src/sync.rs     增量投影、重建与重试编排
src/storage.rs  SQLite 账本、文档信息、关系
src/graph.rs    显式一跳文档关系
```

- 目录随功能迁移创建，不声明空模块，不新增 workspace、通用 Repository 或无消费者的 trait。
- Runtime 统一持有一个 SyncContext 资源束，sync、检索和治理通过 Runtime 的用例入口访问，不重复装配索引或 SQLite。
- LanceDB/Arrow 类型仅在 retrieval/index，FastEmbed 类型仅在 embedding，SQLite 连接仅在 storage；禁止越过适配边界操作底层资源。
- model 保留跨 document、retrieval、governance 的纯契约类型；不触碰文件系统，不依赖引擎、数据库或协议 SDK。数据库行结构留在 storage 内部。
- lib 仅导出调用者需要的公共 API；入口只负责配置、参数、协议和序列化，不复制业务实现。
- document 统一生成解析结果，graph 和 validate 复用；不得分别实现标题、链接扫描或重复读取变化文档。

## 数据与行为约束

- Markdown 是唯一文档事实源，LanceDB 和 SQLite 可删除、可重建；索引失败不得覆盖或回滚原文。
- 配置只由入口读取 ~/.agentwiki/config.json；业务接收已解析参数，不读取业务环境变量配置。缺失时创建默认配置，保留 CLI 根目录覆盖优先级。
- 目标投影按规范化 Wiki 根目录隔离，模型缓存独立。旧投影重新建立，不自动删除未知数据目录。
- 查询前按路径、真实 mtime_ns 和大小筛选变化，再用内容哈希确认；变化文件只读取解析一次，失败记录不能被全局 generation 跳过。
- 单文档读取、解析或索引失败必须记录相对路径和原因并继续处理其他文档；不得静默丢弃或把读取失败当作删除。
- 向量同步批处理，按输入哈希与模型身份复用；失败保留关键词能力并允许下次重试。不维护后台队列、watcher 或独立向量 manifest。
- 跨进程写使用 fs2 文件锁；LanceDB 与 SQLite 没有跨库事务，部分完成必须可重试，账本不得提前确认成功。
- 只有显式 rebuild、版本不兼容或损坏恢复才全量重建。最近活动使用真实文件修改时间，不使用索引时间。
- 正常无匹配和主动关闭语义不记为系统故障；故障进入 degraded，命中来源必须有实际证据。
- 路径、scope 和链接目标必须留在 Wiki 根目录；拒绝外部符号链接。限制输入大小、数量和并发，禁止拼接未经验证的检索表达式。
- 图谱只表示 Markdown 链接和 relations 明确声明的一跳文档关系，不自动抽取实体。

## 规则、校验与写入

- Wiki 根目录 AGENTWIKI.md 是唯一规则与指引入口，不作为普通文档索引；缺失时写默认模板，已有文件不覆盖。
- 必填字段完全由规则 required_fields 决定。标签别名用于归一建议，优先复用动态 known_tags，新标签只警告，其他 Frontmatter 字段允许扩展。
- 规则缓存使用规则文件指纹，known_tags 随文档变化更新，不能仅按规则指纹缓存。
- Agent 原生工具负责已知路径读取和文档增删改。应用写 Markdown 仅限缺失规则自举及显式格式修复。
- validate_wiki 默认只报告；fix_format=true 才修复格式并重新校验。单文件指定 path，全库显式 full=true，范围互斥。
- 格式修复使用 dprint，保留 Frontmatter 原文和代码块内部，不修复标签、链接和业务内容。规则文件不进入普通文档格式修复范围。
- 写回前核对文件变化，冲突跳过并报告；同目录临时文件替换并保留权限，无变化不写回，不宣称能锁住外部编辑器。

## Rust 实现要求

- 保持源码 #![forbid(unsafe_code)]，不得局部放开。使用安全 API、所有权和 RAII 管理资源。
- 用 enum 表达互斥状态，必要时用私有字段和 newtype 维护不变量；不要机械包装基础类型或为消除 clone 引入复杂生命周期。
- 只读参数优先 &str、切片、camino::Utf8Path；所有权、跨线程传递与存储需要时再按值接收。
- 库错误使用 thiserror 保留 source，入口收敛为 anyhow；使用 ? 传播错误，不将底层错误压成字符串丢失错误链，不静默忽略错误。
- unwrap/expect 限于测试或已证明的不变量。公开 API 按需写简洁 rustdoc，说明错误和有意的 panic。
- 目标入口运行于 Tokio；文件 I/O、SQLite 和模型推理等阻塞工作使用有界 spawn_blocking，不阻塞 executor，不跨 await 持同步锁 guard。
- 缩小锁范围，不在锁内执行无关工作；投影一致性需要的协调锁说明保护范围。任务必须有所有者负责等待、取消和错误处理。
- 先测量再优化；不手写已有组件提供的查询、分词、切分、模式匹配、格式化或通用融合算法。
- 注释保留不变量和非显然取舍，不逐行解释代码；避免敏感值进入 Debug、错误与日志。

## 测试与检查

- 单元测试放模块内，公共跨模块行为放 tests，固定小型文档放 tests/fixtures；文档示例适合运行时使用 doctest。
- Bug 修复先添加最小失败测试，再修复根因。测试验证行为，不镜像实现，不依赖特定文件事件顺序。
- 使用 tempfile 隔离文件系统；真实外部服务、跨进程或非隔离测试按项目约定标记 ignore，注明原因和运行方式，不掩盖关键失败。
- 覆盖检索、过滤、无答案、同步失败重试、规则、路径安全、格式化幂等和冲突；完整迁移场景见架构文档。
- 基准使用固定语料、相同查询与标注比较关键词和混合检索，记录设备、模型、文档/片段数、资源与召回；不沿用旧模型阈值。

代码变更提交前执行并通过：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
```

MCP 相关改动另运行：

```bash
cargo build --features mcp --bin agentwiki-mcp
cargo clippy --workspace --all-targets --features mcp -- -D warnings
cargo test --workspace --features mcp
```

纯文档变更检查链接、契约一致、当前/目标状态和 git diff --check，无需运行无关 Rust 测试。CI 与本地检查保持一致；当前仓库没有 CI 配置，不声称已接入。

常用只读检查：cargo metadata --no-deps、cargo tree、cargo audit。cargo clippy --fix 会修改源码并隐含 all-targets，仅在明确需要自动修复时执行。依赖是否允许同时启用以实际 feature 为准。

## Git 与提交

- 使用 Conventional Commits：feat、fix、refactor、docs、test、chore。
- 仅修改和暂存当前任务文件，保留用户无关改动及暂存边界。
- 不手改 Cargo.lock，不提交 target 或派生索引；未获请求不自动提交或推送。
