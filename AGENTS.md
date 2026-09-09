# AGENTS.md

本文件是 AgentWiki 全仓库的工程规范。AgentWiki 是面向多个 AI Agent 的本地优先 Markdown 文档层；详细架构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 项目概览

AgentWiki 使用本地 Markdown 文档库作为文档事实源，以 Markdown 正文承载内容、YAML Frontmatter 承载元数据，并将文档索引到本地 SQLite。核心目标是让 Agent 通过统一工具读写、移动、删除和搜索文档，支持关键词和语义查询。

当前正式入口是：

- **CLI**：本地调试、初始化、批处理和自动化脚本入口。
- **MCP**：为 Agent 暴露与 CLI 共享的文档操作工具。

HTTP API、云端同步和 Web UI 不属于当前架构承诺。

技术基础：

- Python 3.14+
- `uv` 负责环境与依赖管理
- `src/` layout，包目录为 `src/agentwiki/`
- 构建后端为 `uv_build`
- CLI 入口为 `agentwiki:main`

## 目录结构

当前实现按领域和依赖方向组织。未实现的目录或模块应随对应功能落地，不要提前创建空壳模块。

```text
src/agentwiki/
├── cli.py               # CLI composition root 与命令适配
├── mcp.py               # MCP server、工具与资源适配
├── domain/              # 领域模型、值对象、规则和领域错误
├── services/            # 文档操作、搜索和索引同步的业务流程
├── repository/          # SQLite、FTS5 和向量索引访问
├── indexing/            # 文件扫描、增量索引和索引重建
├── markdown/            # Markdown、Frontmatter、本地文档库文件系统适配
├── config.py            # 配置模型与配置读取
└── runtime/             # 文件监听、运行上下文和后台索引生命周期
```

当前依赖方向必须保持为：

```text
CLI/MCP composition roots
            ↓
        services
            ↓
        domain
            ↓
 repository / indexing
            ↓
      markdown / SQLite
```

- `domain` 不依赖 Typer、FastMCP、文件系统或具体配置实现。
- `services` 编排文档的读、写、改、删、移动、索引和搜索；需要替换实现或隔离测试时，再为稳定边界引入 Protocol 契约。
- `repository` 负责 SQLite、FTS5 和向量索引的持久化访问，不负责完整业务流程。
- `indexing` 负责从 Markdown 文档库扫描、增量更新和重建索引。
- `markdown` 负责 Markdown、Frontmatter 和本地文档库文件操作。
- `runtime` 只承载运行上下文、文件监听和后台索引生命周期；没有这些需求时不强行扩展它。
- `cli`、`mcp` 只负责协议适配、参数转换、用例调用和结果序列化。
- 只有 composition root 可以读取全局配置；其他模块通过构造参数接收配置和依赖。

## 常用命令

```bash
# 安装或同步依赖
uv sync

# 运行 CLI
uv run agentwiki

# 启动 MCP（stdio）
uv run agentwiki-mcp

# 代码检查
uv run ruff check
uv run ty check

# 运行全部测试
uv run pytest

# Markdown 写入和编辑由服务层统一经过 mdformat；新增路径不得绕过格式化边界。

# 监听 Markdown 变更并同步索引
uv run agentwiki watch-index

# 只运行非外部服务测试
uv run pytest -m "not integration"

# 运行单个测试文件
uv run pytest tests/path/to/test_file.py
```

提交前必须执行 `uv run ruff check`、`uv run ty check`、`uv run pytest` 和 `git diff --check`。
## 工程规范

### 工具链与依赖

- 统一使用 `uv` 管理依赖、锁文件和虚拟环境。
- 新增依赖前先检查现有依赖是否已提供等价能力，避免引入同类替代品。
- 运行脚本优先使用 `uv run`，不要绕过项目环境直接调用全局 Python 包。
- 修改 `pyproject.toml` 后同步检查 `uv.lock` 是否需要更新。
- 配置模型使用 `pydantic-settings` 从环境变量读取并校验；不要在业务模块中直接读取环境变量。

### 需求与架构

- 复杂改动先确认目标、边界和验收标准，再实现。
- 新能力先确定领域归属、文件命名和依赖方向，再添加代码。
- 入口层不承载文档操作规则；CLI 和 MCP 必须复用 services 层。
- Markdown 文件是文档存储的事实边界；不要在入口层复制一套平行存储模型。
- SQLite 是可删除、可重建的派生索引，不是文档事实源；索引损坏或过期时必须支持从文档库重建。
- 关键词搜索使用 SQLite FTS5；语义搜索使用可选的本地 embedding provider 和向量投影，语义依赖不可用时关键词搜索仍必须可用。
- 跨文档库根目录的路径必须拒绝；敏感信息不得写入文档文件。
- Markdown 写入成功后才更新 SQLite；索引更新失败不得覆盖或回滚 Markdown，必须保留可重建状态。
- 索引扫描遇到单个文档的 Markdown/YAML 解析错误时，不得静默丢弃；至少记录相对路径和错误原因，并继续处理其他文档。

### 测试

- 测试目录尽量镜像源码领域结构，优先为领域规则、服务流程、仓储和索引边界编写精确测试。
- 涉及外部服务、跨进程资源或非隔离真实文件系统的测试使用 `integration` marker；使用 `tmp_path` 的隔离文件系统测试可保持为单元测试。
- Bug 修复必须先有最小失败测试，再修复根因并执行相关回归测试。
- 测试验证行为契约，不依赖特定操作系统的文件事件顺序。

### 代码质量

- 保持模块和函数单一职责，避免把配置、协议适配、业务编排和文件读写混在一起。
- 使用当前版本推荐的 Python 3.14、Typer、Pydantic V2 和 FastMCP，禁止引入已废弃用法。
- 项目源码中的公开函数、业务函数和边界适配函数必须完整标注参数与返回值；测试 fixture、第三方回调和框架要求的特殊函数可保留合理例外。
- 不要用无约束的 `Any` 掩盖跨层接口设计问题；类型确实未知时，优先使用 `object`、Protocol 或具体的 TypeAlias。
- AI 生成的代码同样必须遵守本文件和项目架构文档。

### Git 与提交

Commit 遵循 Conventional Commits：`feat`、`fix`、`refactor`、`docs`、`test`、`chore`。

不要手动修改自动生成文件；与当前任务无关的用户改动必须保留。


## 编码规范

编写**类型安全、可读性强、可维护**的 Python 代码。以显式意图优先，避免不必要的技巧。

### 类型注解

- 使用 `X | Y` 代替 `Union[X, Y]`，使用 `X | None` 代替 `Optional[X]`
- 使用 `list[T]`、`dict[K, V]`、`tuple[T, ...]` 而非 `List`、`Dict`、`Tuple`
- 对于复杂类型，使用 `TypeAlias` 或 `type` 语句定义别名（Python 3.12+）
- Python 3.14 中注解默认懒求值，无需 `from __future__ import annotations`


### 现代 Python 语法

- 根据可读性选择 `match`、`if/elif` 和其他控制流表达方式
- 使用 Pydantic `BaseModel` 定义需要校验、转换或序列化的数据结构
- 优先使用 `pathlib.Path` 而非 `os.path`
- 使用 f-string 进行字符串格式化；在 Python 3.14 中可用 t-string（PEP 750）进行安全模板化
- 仅在能明显提升可读性并避免重复计算时使用海象运算符 `:=`
- 用 `enumerate()` 替代手动索引，用 `zip()` 并行迭代


### 不可变性与常量

- 对不会修改的集合使用 `tuple` 而非 `list`
- 用 `Final` 标注模块级常量
- 对领域值对象和结果模型使用 Pydantic 的 `frozen` 配置；对简单的内部常量可使用 `NamedTuple`
- 避免全局可变状态

### 异常处理

- 捕获具体异常类型，而非裸 `except:` 或 `except Exception:`
- 用 `raise ... from err` 保留异常链
- 不要捕获异常后直接 `pass` 或无意义地重新抛出
- 优先用早返回（guard clause）减少嵌套


### 函数与模块设计

- 保持函数职责单一，认知复杂度低
- 对可选参数较多或语义容易混淆的函数，使用关键字参数提升调用处可读性（`def func(*, key: str)`）
- 对外部使用的模块或包，按需要用 `__all__` 明确声明公开 API
- 避免在模块顶层执行有副作用的代码
- 优先使用纯函数；将 I/O 限制在明确的适配器、仓储和运行时边界

### 异步代码

- 使用 `async/await`，避免直接调用 `asyncio.get_event_loop()`
- 用 `asyncio.TaskGroup`（Python 3.11+）并发管理任务，替代裸 `asyncio.gather`
- 不要在 async 函数中执行阻塞 I/O，使用 `asyncio.to_thread()` 卸载
- 用 `async with` 和 `async for` 管理异步资源

### 安全

- 不要用 `eval()` 或 `exec()` 执行动态代码
- 使用参数化查询，避免 SQL 字符串拼接
- 不要将密钥、密码硬编码在源码中，使用环境变量或 secrets 管理
- 对用户输入进行验证和清理；结构化输入和配置优先使用 `pydantic` 与 `pydantic-settings`
- 使用 `secrets` 模块生成安全随机数，而非 `random`

### 性能

- 优先使用生成器表达式而非列表推导式（当不需要随机访问时）
- 避免在循环内进行重复的属性查找，提前绑定到局部变量
- 对 Pydantic 模型使用其默认的 slots 优化；其他高频、轻量对象再考虑 `__slots__`
- 使用 `collections.deque` 代替列表实现队列
- 避免频繁的小字符串拼接，用 `"".join(parts)` 或 f-string
