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

目标目录按领域和依赖方向组织；目录尚未实现的部分应随对应功能落地，不要提前创建空壳模块。

```text
src/agentwiki/
├── cli/                 # CLI composition root 与命令适配
├── mcp/                 # MCP server、工具与资源适配
├── domain/              # 领域模型、值对象、规则和领域错误
├── services/            # 文档操作、搜索和索引同步的业务流程
├── repository/          # SQLite、FTS5 和向量索引访问
├── indexing/            # 文件扫描、增量索引和索引重建
├── markdown/            # Markdown、Frontmatter、本地文档库文件系统适配
├── config.py            # 配置模型与配置读取
└── runtime/             # 文件监听、运行上下文和后台索引生命周期
```

依赖方向必须保持为：

```text
CLI/MCP composition roots
            ↓
        services
            ↓
     domain + contracts
            ↓
 repository / indexing
            ↓
      markdown / SQLite
```

- `domain` 不依赖 Typer、FastMCP、文件系统或具体配置实现。
- `services` 通过 repository 和 indexing 契约编排文档的读、写、改、删、移动、索引和搜索。
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

# 代码检查
uv run ruff check
uv run ty check

# 运行全部测试
uv run pytest

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

### 需求与架构

- 复杂改动先确认目标、边界和验收标准，再实现。
- 新能力先确定领域归属、文件命名和依赖方向，再添加代码。
- 入口层不承载文档操作规则；CLI 和 MCP 必须复用 services 层。
- Markdown 文件是文档存储的事实边界；不要在入口层复制一套平行存储模型。
- SQLite 是可删除、可重建的派生索引，不是文档事实源；索引损坏或过期时必须支持从文档库重建。
- 关键词搜索使用 SQLite FTS5；语义搜索使用本地 embedding 和向量索引，语义依赖不可用时关键词搜索仍必须可用。
- 跨文档库根目录的路径必须拒绝；敏感信息不得写入文档文件。

### 测试

- 测试目录镜像源码领域结构，优先为领域规则、服务流程、仓储和索引边界编写精确测试。
- 涉及外部服务或真实文件系统的测试使用 `integration` marker；纯单元测试保持快速、确定。
- Bug 修复必须先有最小失败测试，再修复根因并执行相关回归测试。
- 测试验证行为契约，不依赖特定操作系统的文件事件顺序。

### 代码质量

- 保持模块和函数单一职责，避免把配置、协议适配、业务编排和文件读写混在一起。
- 使用当前版本推荐的 Python、Typer、Pydantic 和 FastMCP API，禁止引入已废弃用法。
- 保持类型标注完整；不要用无约束的 `Any` 掩盖跨层接口设计问题。
- AI 生成的代码同样必须遵守本文件和项目架构文档。

### Git 与提交

Commit 遵循 Conventional Commits：`feat`、`fix`、`refactor`、`docs`、`test`、`chore`。

不要手动修改自动生成文件；与当前任务无关的用户改动必须保留。
