# Wiki Rules 配置参考

Wiki 规则使用文档库根目录下的 `_agentwiki/context.yaml`。它是 YAML 文件，只描述文档
组织和校验规则；运行路径与 embedding 模型仍由项目根目录的 `.agentwiki/config.json`
配置。

## 完整示例

```yaml
version: 1
name: Team Wiki
purpose: 团队知识、技术决策和操作指南

# 文档没有 type 时使用的默认类型
default_type: note

# 系统始终要求 title、type、tags、created_at、updated_at。
# 这里声明 Wiki 自己追加的根级必填项。
required_fields:
  - owner

# 可选。键是规范标签，值是应归并到该标签的旧写法或同义词。
tag_aliases:
  architecture:
    - Architecture
    - arch
  decision:
    - decisions

# 目录级规则可以细化用途、类型、必填字段和文件名
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

  - path: projects/*
    description: 各项目自己的知识文档
    types:
      - project
      - note
    required_fields:
      - project
```

## 字段说明

| 字段 | 必填 | 行为 |
| --- | --- | --- |
| `version` | 否 | 配置版本，默认 `1`，必须大于等于 1 |
| `name` | 否 | Wiki 名称，默认 `AgentWiki` |
| `purpose` | 否 | Wiki 用途，返回给 Agent 作为组织背景 |
| `default_type` | 否 | 文档未声明 `type` 时采用的类型，默认 `note` |
| `required_fields` | 否 | 在五个系统必填字段之外追加的根级必填字段 |
| `tag_aliases` | 否 | 规范标签到别名列表的映射，用于提示归一和检索匹配 |
| `sections` | 否 | 目录或路径模式对应的细化规则 |
| `sections[].path` | 是 | 相对路径或 `fnmatch` 模式，如 `guides`、`projects/*` |
| `sections[].description` | 否 | 该范围的用途说明 |
| `sections[].types` | 否 | 该范围允许的文档类型 |
| `sections[].required_fields` | 否 | 为匹配范围继续追加必填字段 |
| `sections[].filename_pattern` | 否 | 文件名 glob，不匹配时返回 `path.filename` |

## 合并和校验语义

- `title`、`type`、`tags`、`created_at`、`updated_at` 始终必填，配置不能移除。
- 根级及 `sections[].required_fields` 只会追加必填字段，不会覆盖已有字段。
- Frontmatter 的其他字段允许自由扩展。
- 标签采用小写规范形式，可用 `/` 表达层级，例如 `engineering/backend`。
- `tag_aliases` 只提供归一建议，不限制新标签；别名和大小写变体会产生 warning。
- `get_wiki_rules` 会从当前文档动态返回 `known_tags`（规范标签、使用次数和已见别名）。
- 首次出现且未配置别名的新标签产生 warning，但仍允许保存；重复出现后进入稳定目录。
- 文档中的 `tags` 必须是非空字符串列表，标签各段只允许 Unicode 字母数字和连字符。
- 按父标签筛选时包含所有子标签；别名在筛选时也会归一，例如 `eng` 可匹配 `engineering/backend`。
- 多条目录规则匹配时按 `path` 长度从短到长应用，更具体的规则最后生效。
- 所有问题都由 `validate_wiki` 返回；规则不包含阻断配置，也不会自动改写文件。
- 未声明字段会被拒绝为配置错误，避免拼写错误或无效规则被静默忽略。

主要错误码：

| 错误码 | 级别 | 含义 |
| --- | --- | --- |
| `type.not_allowed` | error | 文档类型不在目录允许范围内 |
| `frontmatter.required` | error | 缺少必填 Frontmatter 字段 |
| `tags.invalid` | error | `tags` 不是非空字符串列表，或标签语法无效 |
| `tags.non_canonical` | warning | 标签是别名或大小写不规范，并给出规范标签建议 |
| `tags.duplicate` | warning | 同一文档内的标签归一后重复 |
| `tags.new` | warning | 标签首次出现且尚未配置为规范标签 |
| `path.filename` | error | 文件名不符合目录规则 |
| `link.broken` | warning | 内部 Markdown 链接目标不存在 |
| `markdown.formatting` | warning | 文档不符合 mdformat 规范 |
| `markdown.parse` | error | Markdown 或 YAML Frontmatter 无法解析 |
