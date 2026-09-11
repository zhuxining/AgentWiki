# AGENTS.md

本文件是 AgentWiki 全仓库的工程规范，只用于指导开发；AgentWiki 是面向多个 AI Agent 的本地优先 Markdown 知识检索层，详细架构与产品设计见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 项目概览

AgentWiki 使用本地 Markdown 文档库作为文档事实源，以 Markdown 正文承载内容、YAML Frontmatter 承载元数据，并将文档片段投影到本地检索引擎与元数据存储。核心目标是为 Agent 提供精确、关键词、语义、混合和近期搜查能力；已知路径读取和文档增删改由 Agent 原生工具完成。

当前正式入口是：

- **CLI**（二进制 `agentwiki`）：本地检索、索引同步、重建和治理维护入口。
- **MCP**（二进制 `agentwiki-mcp`）：为 Agent 暴露任务上下文检索、Wiki 规则和规范校验；不重复暴露 Agent 原生文件工具（SDK 接线进行中）。

HTTP API、云端同步和 Web UI 不属于当前架构承诺。Python 原型已归档至 `legacy/python/`（含基准脚本与语料），不再维护；开发只针对 Rust 侧。

技术基础：

- Rust（edition `2024`，`MSRV` 为 1.98，见 `Cargo.toml` 的 `package.rust-version`）
- `tokio` 异步运行时；所有二进制入口都在 tokio 之上
- 单一 package + `src/lib.rs` 库，CLI/MCP 作为独立的 `[[bin]]` 共享库
- 检索引擎为 `tantivy`（+ `lindera` 中文分词）；`rusqlite` 仅作元数据存储（同步账本 / 图谱 / 状态），**不执行全文或向量检索**
- 全文/向量检索必须经由 `tantivy_svc` 单独封装；`model` 保持第三方 I/O 无关
- 通过 `#[forbid(unsafe_code)]` 禁止 unsafe（见 `src/lib.rs`）；新增 crate 必须先确认无 unsafe
- MCP SDK 放在 feature `mcp` 之后，核心库保持 SDK-free

## 目录结构

当前实现按领域和职责方向组织。未实现的模块应随对应功能落地，不要提前创建空壳模块（`lib.rs` 声明的模块必须存在对应文件）。

```text
src/
├── lib.rs               # 库根；声明模块、顶层 re-export，`forbid(unsafe_code)`
├── main.rs              # CLI composition root（二进制 agentwiki）
├── mcp.rs               # MCP composition root（二进制 agentwiki-mcp，feature `mcp`；SDK 接线进行中）
├── error.rs             # 统一库错误类型（thiserror）
├── model.rs             # 纯领域类型（依赖无关的 value objects / query structs）
├── markdown.rs          # Markdown 只读解析、Frontmatter、路径安全、标题感知切块
├── graph.rs             # 一跳文档关系抽取（显式声明的关系，不自动抽取实体）
├── storage.rs           # rusqlite 元数据存储：同步账本、图谱、状态
├── sync.rs              # Markdown → 索引/元数据的增量投影与 rebuild
├── tantivy_svc.rs       # 检索引擎的唯一边界封装（写索引 / BM25 查询 / 可选向量腿；全文接线进行中）
├── runtime.rs           # 显式资源装配、同步锁、可选 watcher、索引生命周期
├── search.rs            # 检索编排、排名融合、scope 过滤与降级
└── validate.rs          # 格式、结构、Frontmatter 与内部链接校验
```

当前依赖方向必须保持为：

```text
CLI / MCP composition roots (main.rs / mcp.rs)
            ↓
        runtime context
            ↓
      search / validate / sync（编排）
            ↓
   model（纯领域） ← tantivy_svc / storage / markdown（适配）
```

- `model` 是依赖无关的纯领域层：不导入 Tantivy、rusqlite、MCP、CLAP，也不触碰文件系统；作为 `sync` / `search` / `validate` 之间及 MCP 序列化的共享契约。
- `tantivy_svc` 是**唯一**触碰检索引擎的地方：`sync` 和 `search` 只依赖它的小 API，从不直接依赖 Tantivy 类型。
- `storage` 只做元数据（账本 / 图谱 / 状态），**不执行** FTS5 或向量查询；全文/向量检索全部交给 Tantivy。
- `markdown` 只读，从不写 Markdown；文档增删改由 Agent 原生工具完成。
- `main.rs` / `mcp.rs` 只负责协议适配、参数解析、用例调用和序列化；只有 composition root 读取全局配置，其余模块通过构造参数接收依赖。
- 越层导入（如业务层直接依赖 Tantivy 或 rusqlite 连接）属于架构破坏。

各模块的详细职责、数据流、实现状态与设计取舍见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) 第 2、3 节；上文是必须保持的开发约束，不是设计描述。

## 常用命令

```bash
# 构建 / 调试
cargo build
cargo run --bin agentwiki           # 运行 CLI
cargo run --bin agentwiki-mcp --features mcp   # 启动 MCP（stdio，需 mcp feature）

# 测试
cargo test --workspace
cargo test -- --ignored             # 只跑被 #[ignore] 的集成/网络测试
cargo test <模块>::<测试名>          # 单个测试
cargo test --features mcp             # 含 feature 需求的部分测试按需启用

# 代码检查（提交前必须全绿）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 基准
cargo bench

# 已知配置默认值 / 依赖审查
cargo metadata --no-deps            # 查看解析后的包/feature
cargo tree                          # 依赖树
cargo audit                         # 依赖漏洞（不能替代依赖审查）
```

MCP 相关目标默认被 `mcp` feature 隐藏：`cargo build --all-features` 或 `--features mcp` 时才编译 SDK 依赖。核心库的单元测试不应依赖 `mcp` feature 即可通过。

## 工程规范

### 工具链与依赖

- 版本与依赖由 `Cargo.toml`（+ 锁文件 `Cargo.lock`）统一管理；不要手动编辑 `Cargo.lock`。
- 新增依赖前先检查现有依赖是否已提供等价能力，避免引入同类替代品。
- 依赖尽量收窄 feature；`rusqlite` 使用 `bundled` 避免宿主机 sqlite 依赖。
- 修改 `Cargo.toml` 后让 Cargo 重新解析并同步 `Cargo.lock`（`cargo build` 或 `cargo update -w`）。
- 工具链跟随最新 stable；使用新语法、标准库 API 或依赖版本前，确认不会无意提高 MSRV（`package.rust-version` 当前为 `1.98`）。
- 配置模型使用 `serde` 从用户配置目录 `~/.agentwiki/config.json` 读取并校验（`validator` 校验规则）；不读取环境变量，业务模块只接收已解析的构造参数。首次运行缺少配置文件时创建默认配置。
- 错误处理：库错误用 `thiserror`（`error.rs`），应用入口（`main.rs` / `mcp.rs`）转成 `anyhow::Error` 收敛。

### 需求与架构

- 复杂改动先确认目标、边界和验收标准，再实现。
- 新能力先确定模块归属、文件命名和依赖方向，再添加代码。
- Agent 原生工具负责已知路径读取和文档增删改；MCP 承载任务检索、规则和规范校验。
- Markdown 文件是文档存储的事实边界；不要在入口层复制一套平行存储模型。
- 检索索引与 SQLite 都是可删除、可重建的派生投影，不是文档事实源；索引损坏或过期时必须支持从文档库重建。
- 全文/向量检索必须走 `tantivy_svc`；禁止在 `sync`、`search`、CLI 或 MCP 中直接构造 Tantivy 查询。SQLite 只是元数据镜像，不做 FTS5/向量检索。
- 关键词搜索使用 Tantivy（BM25 + `lindera` 中文分词）；语义搜索使用可选 embedding 与向量腿；语义依赖不可用时关键词搜索仍必须可用。
- 跨文档库根目录的路径必须拒绝；敏感信息不得写入文档文件。
- 查询前以路径、真实 `mtime_ns` 和大小增量确认外部 Markdown 变化（具体见 `markdown.rs` / `sync.rs`）；索引更新失败不得覆盖或回滚 Markdown，必须保留可重建状态。
- 跨进程写锁使用文件锁（`fs2`），因为进程内锁不能阻止另一个 MCP 进程同时写入同一索引。
- Wiki 根目录的 `AGENTWIKI.md` 是唯一的组织规则与 Agent 指导入口：Frontmatter 承载结构化规则，正文作为 `guide_content` 返回，该文件不作为普通文档索引。运行时装配会在启动时写入随包分发的默认模板，已存在的文件永不覆盖。必填字段完全由规则文件中的 `required_fields` 决定，系统不内置任何必填字段；规则可用可选 `tag_aliases` 归一同义标签；优先复用动态 `known_tags`，新标签仅告警、不阻断，其他字段允许扩展。
- 原生工具写入或编辑后应执行格式、结构、Frontmatter 和内部链接校验；校验只报告，不自动改写 Markdown。
- 外部 Markdown 变更优先走增量同步；只有显式 rebuild 或索引恢复场景才清空并全量重建投影。
- 索引扫描遇到单个文档的 Markdown/YAML 解析错误时，不得静默丢弃；至少记录相对路径和错误原因，并继续处理其他文档（错误进 `sync_error` / 降级诊断，不中止整轮）。

### 测试

- 单元测试放在被测模块内；较大测试集可拆为子模块。跨模块的公共行为放在 `tests/` 集成测试，公开文档示例优先使用 doctest。
- 优先为领域模型、搜索封装、同步与图谱边界编写精确测试。
- 使用 `tempfile` 构造隔离 `tmp` 文件系统的测试可保持为单元测试；涉及真实外部服务、跨进程资源或非隔离文件系统的测试用 `#[ignore]` 并注明原因和运行方式。
- Bug 修复必须先有最小失败测试，再修复根因并执行相关回归测试。
- 测试验证行为契约，不依赖特定操作系统的文件事件顺序。

### 代码质量

- 保持模块和函数单一职责，避免把配置、协议适配、业务编排和文件读写混在一起。
- 遵循 `src/lib.rs` 的 `#![forbid(unsafe_code)]`；如需 unsafe 必须先在 Cargo.toml 层讨论并评估，不在模块内局部放水。
- 代码应自解释，注释只保留关键信息（不变量、边界语义、非显而易见的取舍），不要逐行逐函数堆注释。
- 需要暴露契约的公开函数写简洁 rustdoc，按适用情况记录 `# Errors` / `# Panics`；测试 fixture 与测试辅助函数不要求注释。
- 用类型系统表达约束：enum 表达互斥状态、newtype 封装不变量，避免用无约束 `Any`/宽类型掩盖接口问题。
- 不要为消除统一错误类型而拼字符串丢失错误链；跨层失败用 `thiserror` 变体保留 source。
- AI 生成的代码同样必须遵守本文件和项目架构文档。

### Git 与提交

Commit 遵循 Conventional Commits：`feat`、`fix`、`refactor`、`docs`、`test`、`chore`。

不要手动修改自动生成文件（`Cargo.lock` 同步依赖、`target/` 绝不提交）；与当前任务无关的用户改动必须保留。

## 编码规范

编写**安全、惯用、可维护**的 Rust 代码。优先使用类型系统和所有权表达约束，避免不必要的分配、复制和抽象。

### 项目约束

- 修改前检查 `Cargo.toml`、edition、`rust-version`、feature 和 CI 配置
- 遵循项目已有的错误类型、异步运行时、日志、测试和依赖约定
- 使用新语法、标准库 API 或依赖版本前，确认不会无意提高 MSRV
- 注意 `no_std`、目标平台和 workspace resolver，不假设所有 feature 可以同时启用（本项目 `mcp` 为可选 feature，核心库不应依赖它）

### 类型设计

- 使用 enum 表达互斥状态，避免用多个布尔值组合出非法状态
- 使用 newtype 区分容易混淆的值，或封装表示和不变量；不要机械包装每个基础类型
- 仅在 API 使用顺序属于重要且稳定的不变量时使用 typestate
- 静态分发使用泛型或 `impl Trait`；需要运行时多态或异构集合时使用 `dyn Trait`
- 为公开类型实现语义成立且调用者需要的常用 trait，不要盲目派生 `Clone`、`PartialEq`、`Hash` 或 `Default`
- 优先使用标准转换 trait，如 `From`、`TryFrom`、`AsRef` 和 `AsMut`
- API 应使非法状态难以表达，但类型复杂度应与误用风险相称

### 所有权与借用

- 只读访问通常使用借用；需要存储、转移、跨线程发送或消费值时按值接收
- 小型 `Copy` 类型通常按值传递
- 借用 owned 容器时使用底层视图：`&str`、`&[T]`、`&Path`（本项目路径统一用 `camino::Utf8Path` / `Utf8PathBuf`），除非接口确实需要具体容器能力
- 避免不必要的 `.clone()`，但不要仅为消除 clone 引入复杂生命周期
- 显式生命周期只用于消除歧义或表达输入输出关系，不为标注而标注
- 仅在多数路径可借用、少数路径需要拥有且确有收益时使用 `Cow`
- 使用 RAII 管理资源，确保提前返回、错误和 panic 路径也能释放资源

### 惯用写法

- 使用模式匹配表达结构和分支；单一模式使用 `if let`，提前退出可使用 `let-else`
- 使用 `?` 传播错误，避免只为拆包编写重复的 `match`
- 循环和迭代器都属于惯用写法，选择更清晰且不产生无意义中间集合的实现
- 使用迭代器适配器表达转换流程，复杂控制流使用普通循环
- 使用 `format!` 构造新字符串，向已有缓冲区写入时使用 `write!`
- 使用解构减少重复字段访问，但不要牺牲可读性
- 使用 `Default` 表达有明确语义的默认值，不为所有类型强行实现默认状态

### 错误处理

- 使用 `Option` 表达值可能不存在，使用 `Result` 表达操作可能失败
- 使用 `?` 保留错误传播路径，并在跨越有意义的系统边界时补充上下文
- 库的公开错误应便于调用者检查和处理；应用边界可在无需按变体恢复时使用类型擦除错误（本项目：库用 `thiserror`，入口收敛为 `anyhow`）
- `thiserror` 和 `anyhow` 是项目既有选择，已在 `Cargo.toml` 中
- 保留底层错误 source，不要仅用字符串丢失错误链和可检查的类别
- `unwrap` 和 `expect` 仅用于已证明不可能失败、测试代码或 panic 明确属于程序契约的场景
- `expect` 信息应说明该状态为何不可能发生，而不是重复底层错误
- 不要静默忽略错误；有意忽略时必须让原因在代码或注释中清晰可见

### 公开 API

- 公开 API 按需用简洁 rustdoc 说明用途和契约，按适用情况记录 `# Errors`、`# Panics` 和 `# Safety`；不为注释而注释
- 文档示例适合编译运行时优先使用 doctest
- 谨慎暴露依赖 crate 的具体类型（如不把 Tantivy 类型泄漏到 `search` / 业务 API），避免将内部依赖变成公共 API
- 评估 SemVer：公开 enum 增加 variant、公开 struct 增加字段和改变 trait 实现都可能影响下游代码
- 需要保留扩展空间时使用私有字段、构造器、sealed trait 或 `#[non_exhaustive]`

### 并发

- 根据所有权、吞吐、背压和一致性要求选择消息传递或共享状态
- 缩小锁保护的数据和持锁范围
- 不要在持锁时执行 I/O、长计算、回调或获取顺序不明确的其他锁
- 共享所有权使用 `Arc`；内部同步原语根据读写模式和运行时选择，不默认使用 `RwLock`
- 仅在竞争分析或基准表明确有需要时使用分片容器或原子类型
- 原子代码必须说明内存序和依赖的不变量；`DashMap` 是分片锁容器，不是无锁容器
- 不要手动实现 `Send` 或 `Sync`，除非能完整证明线程安全契约

### 异步代码

- 本项目使用 tokio；不要在 executor 工作线程中执行阻塞 I/O 或长时间 CPU 计算
- 优先使用异步 API；必须卸载阻塞工作时使用 tokio 的 `spawn_blocking`，并限制并发和关闭行为
- 不要跨 `.await` 持有同步锁 guard；异步锁也不要覆盖无关 I/O 或长计算
- 使用 `select!`、timeout 或取消时，确认 future 的 cancellation safety
- 后台任务必须有明确所有者负责取消、等待、处理错误和关闭
- 原生 trait `async fn` 适合静态分发；需要 `dyn Trait` 时显式返回 boxed Future，或在接受其成本时使用 `async-trait`
- 仅当 Future 被要求为 `Send` 时，跨 `.await` 持有 `!Send` 值才会报错；local task 可以使用 `!Send` 值

### Unsafe 与 FFI

- 本项目在 `src/lib.rs` 使用 `#![forbid(unsafe_code)]`；新增代码默认禁止 unsafe，如确需放开必须先在架构层评估并统一收口
- 每个 `unsafe` 块必须写 `// SAFETY:`，说明调用前置条件以及当前代码如何满足条件
- `unsafe fn` 和 `unsafe trait` 必须在 `# Safety` 中记录调用者或实现者的责任
- 将 unsafe 封装在尽可能小的私有模块中，通过安全 API 保护不变量
- 检查有效性、对齐、别名、生命周期、初始化状态以及 panic/unwind 路径
- 除非能完整证明布局和有效值等不变量，否则不要使用 `transmute`；优先使用标准转换 API
- FFI 边界必须验证指针、长度、所有权、ABI 和外部数据有效性，不允许 panic 穿越不支持 unwind 的边界

### 性能与安全

- 先使用 profile、benchmark 或内存数据定位瓶颈，再优化（项目已引入 `criterion` 做基准）
- 已知最终大小时使用 `with_capacity` 等方式预分配集合
- 避免无意义的中间集合和热路径分配，但以代码清晰为前提
- 仅因结构体较大不要使用 `Box`；装箱用于间接寻址、稳定地址、递归类型、trait object 或控制外层类型大小
- 外部输入（路径、Frontmatter、查询参数、Markdown 内容）必须验证，并限制大小、深度、数量、时间和并发
- 避免敏感值进入 `Debug`、错误和日志；根据威胁模型决定是否使用 `secrecy` 或 `zeroize`
- 使用项目约定的工具检查依赖漏洞和供应链风险；`cargo audit` 不能替代依赖审查

### 测试

- 单元测试放在被测模块内；较大测试集可以拆为子模块
- 跨 crate 的公共行为放在 `tests/`，公开文档示例优先使用 doctest
- 测试围绕一个可描述的场景组织，可以验证该场景下多个相关结果
- 覆盖成功路径、失败路径和关键边界；修复 bug 时添加回归测试
- 对返回 `Result` 的 API 优先断言错误值；仅当 panic 属于契约时使用 `#[should_panic]`
- 存在可表达不变量和较大输入空间时使用属性测试或 fuzz
- unsafe 或并发代码按风险使用 Miri、sanitizer 或 `loom`
- `#[ignore]` 必须注明原因和运行方式，不得长期掩盖关键回归失败

## 提交前检查

提交前必须执行以下命令并全部通过：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

并按需验证 `cargo build --features mcp --bin agentwiki-mcp`（MCP 入口可编译）与
`cargo audit`（依赖漏洞），确认 `git diff --check` 干净。CI 与本地共用同一组验证，
避免两边漂移。

`cargo clippy --fix` 会修改源码并隐含 `--all-targets`，仅在明确需要应用并审查自动修复时运行。当前 `mcp` 是可独立编译的 feature、并非互斥，可用 `--features mcp` 验证；若未来出现互斥 feature，则不要盲目 `--all-features`。

## 权威来源（不确定时参考）

| 主题 | 官方参考 |
|------|---------|
| 语言、标准库与 Unsafe | [Rust Documentation](https://doc.rust-lang.org/) |
| 语言规则与 dyn compatibility | [The Rust Reference](https://doc.rust-lang.org/reference/) |
| Cargo、MSRV、feature 与 SemVer | [The Cargo Book](https://doc.rust-lang.org/cargo/) |
| 公开 API 设计 | [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) |
| Edition 迁移 | [Edition Guide](https://doc.rust-lang.org/edition-guide/) |
| 版本变更 | [Rust Release Notes](https://doc.rust-lang.org/releases.html) |
| Tokio 行为 | [Tokio Documentation](https://docs.rs/tokio/latest/tokio/) |
| 检索引擎 API | [Tantivy Documentation](https://docs.rs/tantivy/latest/tantivy/) |
| SQLite 元数据存储 | [rusqlite Documentation](https://docs.rs/rusqlite/latest/rusqlite/) |