# AGENTWIKI.md 规则配置参考

> **目标契约，待整体迁移。** 当前已有规则解析与部分校验；globset、内置 dprint 和显式格式修复尚未接入。本文描述迁移后的统一行为，当前实现状态见 [架构](ARCHITECTURE.md)。

AgentWiki 使用 Wiki 根目录下的 `AGENTWIKI.md` 作为规则和指引文件。该文件是 Markdown：

- YAML Frontmatter：结构化规则配置；
- Frontmatter 后的 Markdown 正文：提供给 Agent 和团队成员阅读的使用指引。

例如，Wiki 根目录为 `~/AgentWiki` 时，配置文件为：

```text
~/AgentWiki/AGENTWIKI.md
```

`AGENTWIKI.md` 不会作为普通文档参与索引。索引是可删除、可重建的派生数据，Markdown 文档
仍然是知识库的事实源。

这是唯一的规则入口。**Runtime 启动时会自动初始化**：目标 CLI 与 MCP 共用装配入口（当前仅 CLI 已接入），检查 Wiki 根目录，
若缺少 `AGENTWIKI.md` 就写入随包分发的默认模板。已存在的文件**永远不会被覆盖**，手改内容在
后续每次启动都保留。

默认模板声明 `required_fields: [title, type, tags]` 与 `default_type: note`，`sections` 与
`tag_aliases` 以注释形式给出示例。对已有 Wiki，首次升级后这些必填字段才会开始生效，因此
可能一次性出现多条 `frontmatter.required` 问题；按需修改该文件即可调整约束。

若确实不需要任何规则，把 `required_fields` 设为空列表；删除文件会在下次启动时重新生成。

## 完整示例

```markdown
---
name: Team Wiki
purpose: 团队知识、技术决策和操作指南
default_type: note

# 必填字段完全由配置决定。没有系统内置必填字段。
required_fields:
  - title
  - type
  - tags
  - created_at
  - updated_at
  - owner

tag_aliases:
  architecture:
    - Architecture
    - arch
  decision:
    - decisions

sections:
  - path: decisions
    description: 已确认的技术与产品决策
    types:
      - decision
    required_fields:
      - status
      - decided_at
    filename_pattern: "*.md"

  - path: guides
    description: 面向 Agent 和团队成员的操作指南
    types:
      - guide
    required_fields:
      - status
    filename_pattern: "*.md"

  - path: projects/*
    description: 各项目自己的知识文档
    types:
      - project
      - note
    required_fields:
      - project
    filename_pattern: "*.md"
---

# Wiki 使用指南

当任务涉及历史方案、已有决策、跨文档关系或近期变化时，先调用 `get_wiki_context`。

新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。

使用原生工具修改 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。
```

## Frontmatter 字段

| 字段 | 必填 | 行为 |
| --- | --- | --- |
| `version` | 否 | 配置版本，默认 `1`，必须大于等于 `1` |
| `name` | 否 | Wiki 名称，默认 `AgentWiki` |
| `purpose` | 否 | Wiki 用途，返回给 Agent 作为组织背景 |
| `default_type` | 否 | 文档未声明 `type` 时使用的默认类型，默认 `note` |
| `required_fields` | 否 | 根级必填字段；不配置时不强制要求任何字段 |
| `tag_aliases` | 否 | 规范标签到别名列表的映射，用于标签归一和检索匹配 |
| `sections` | 否 | 目录或路径模式对应的细化规则 |

### `sections[]` 字段

path 使用 Wiki 根目录下的相对路径，统一使用 `/`：

- 不含模式字符的目录名匹配自身及子树：`decisions` 覆盖 `decisions/a.md`，不覆盖 `decisions-old/a.md` 或 `projects/decisions/a.md`。
- 模式使用 globset，显式设置 `literal_separator(false)`、`backslash_escape(false)`，保持 `*` 可跨 `/`、大小写敏感的整体路径匹配；规则加载时编译，非法模式作为规则错误报告。
- `projects/*` 覆盖该目录下的普通文档，包括嵌套路径；`*decisions*` 匹配路径中包含 decisions 的文档。
- 模式匹配不自动追加子树：`projects/*/decisions` 只匹配以 decisions 结尾的路径，要匹配其中的文档应写 `projects/*/decisions/*`。
- filename_pattern 只匹配文件名，不匹配完整路径。

迁移时删除自写 fnmatch，不模拟其全部边缘行为。现有常用 `*`、`?`、字符类规则保留对应匹配意图；非法模式由宽松处理改为规则错误，转义和字符类的边缘行为以 globset 为准。迁移测试覆盖现有规则样例及上述差异，不自动改写用户规则。[globset 配置](https://docs.rs/globset/latest/globset/struct.GlobBuilder.html)

| 字段 | 必填 | 行为 |
| --- | --- | --- |
| `path` | 是 | 相对目录或 globset 模式，如 `guides`、`projects/*` |
| `description` | 否 | 该范围的用途说明 |
| `types` | 否 | 该范围允许的文档类型 |
| `required_fields` | 否 | 为匹配范围追加必填字段 |
| `filename_pattern` | 否 | 文件名 glob，不匹配时返回 `path.filename` 错误 |

## 合并和校验语义

- 必填字段完全由根级及匹配的 `sections[].required_fields` 配置决定。
- 系统不再内置 `title`、`type`、`tags`、`created_at`、`updated_at` 等必填字段。
- 根级和目录级必填字段会合并，重复字段只保留一次。
- `Frontmatter` 的其他字段允许自由扩展。
- 多条目录规则匹配时按 `path` 长度从短到长应用，等长保持声明顺序；required_fields 合并去重，类型与文件名约束按最后一条声明该约束的匹配规则生效。这里的具体性只是长度约定，不推断模式集合包含关系。
- `filename_pattern` 只校验匹配目录下文档的文件名。
- 标签建议使用小写规范形式，可用 `/` 表达层级，例如 `engineering/backend`。
- `tag_aliases` 只提供归一建议，不限制新标签；别名和大小写变体会产生 warning。重复标签会在检索归一时视为同一标签，不单独提示。
- `get_wiki_rules` 返回有效规则、指引、动态 known_tags 和 wiki_root。规则缓存使用规则文件指纹，known_tags 随文档投影变化更新。
- 首次出现且未配置别名的新标签产生 warning，但仍允许保存。
- 所有问题都由 `validate_wiki` 返回；默认不写文件，只有显式 fix_format=true 才修复格式，不修复规则或元数据问题。
- 配置中的未知字段会被拒绝，避免拼写错误或无效规则被静默忽略。

## 格式检查与修复（待实现）

使用内置 dprint-plugin-markdown，取消旧 mdformat 等价承诺；不需要额外安装 Python、Node 或格式化命令。统一配置使用 LF、80 列目标宽度、保持原有段落换行（TextWrap::Maintain），其他 Markdown 风格使用锁定组件版本的默认值，不新增用户格式配置字段。[配置接口](https://docs.rs/dprint-plugin-markdown/latest/dprint_plugin_markdown/configuration/struct.ConfigurationBuilder.html)

- 检查时在内存中格式化并比较原文，存在差异报告 markdown.formatting。
- fix_format 默认 false；单文件修复指定 path，全库修复显式 full=true，两者互斥。未指定范围不能触发写回，参数详见 [MCP 契约](MCP_TOOLS.md)。
- 修复保留 Frontmatter 原文和代码块内部，不修复标签、链接目标、关系或业务内容；规则文件不进入普通文档格式修复范围。
- 原文无法安全解析时报告错误并跳过修复。写回前检查文件是否变化，冲突则跳过；同目录临时文件替换并保留权限，无变化不写回。
- 修复后重新校验；formatted_paths 只列实际成功写回的路径，剩余规则问题继续返回，不以格式修复成功代替校验通过。
- 原生工具仍负责内容修正，修正后再次校验；格式修复是唯一新增的文档编辑例外。

## 主要错误码

| 错误码 | 级别 | 含义 |
| --- | --- | --- |
| `type.not_allowed` | error | 文档类型不在目录允许范围内 |
| `frontmatter.required` | error | 缺少配置声明的必填 Frontmatter 字段 |
| `path.filename` | error | 文件名不符合目录的 `filename_pattern` |
| `tags.invalid` | error | `tags` 不是非空字符串列表，或标签语法无效 |
| `tags.non_canonical` | warning | 标签是别名或大小写不规范，并给出规范标签建议 |
| `tags.new` | warning | 标签首次出现且尚未配置为规范标签 |
| `link.broken` | warning | 内部 Markdown 链接目标不存在 |
| `markdown.formatting` | warning | 文档与内置 dprint 格式结果不同（待实现） |
| `markdown.parse` | error | Markdown 或 YAML Frontmatter 无法解析 |
| `rules.parse` | error | 规则配置或模式非法，无法应用规则 |
| `format.conflict` | error | 格式写回前发现外部修改，已跳过该文件（待实现） |
| `format.failed` | error | 格式化或安全写回失败（待实现） |

## 开发与验证

本文只描述规则配置契约。构建、测试与提交前检查命令见仓库根目录的 [AGENTS.md](../AGENTS.md)。
