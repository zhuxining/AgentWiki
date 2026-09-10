# AGENTWIKI.md 规则配置参考

AgentWiki 使用 Wiki 根目录下的 `AGENTWIKI.md` 作为规则和指引文件。该文件是 Markdown：

- YAML Frontmatter：结构化规则配置；
- Frontmatter 后的 Markdown 正文：提供给 Agent 和团队成员阅读的使用指引。

例如，Wiki 根目录为 `~/AgentWiki` 时，配置文件为：

```text
~/AgentWiki/AGENTWIKI.md
```

`AGENTWIKI.md` 不会作为普通文档参与索引。索引是可删除、可重建的派生数据，Markdown 文档
仍然是知识库的事实源。

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

`path` 按 Wiki 根目录下的相对路径匹配。配置 `path: decisions` 时，匹配 `decisions` 目录及其所有后代路径（如 `decisions/example.md`、`decisions/archive/old.md`），但不匹配 `projects/decisions/example.md` 或 `decisions-old/example.md`。`path` 支持 glob；例如 `*decisions*` 会匹配路径字符串中包含 `decisions` 的路径，`projects/*/decisions` 会匹配项目目录下的 decisions 子目录。glob 应谨慎使用，避免范围过宽。

| 字段 | 必填 | 行为 |
| --- | --- | --- |
| `path` | 是 | 相对路径或 `fnmatch` 模式，如 `guides`、`projects/*` |
| `description` | 否 | 该范围的用途说明 |
| `types` | 否 | 该范围允许的文档类型 |
| `required_fields` | 否 | 为匹配范围追加必填字段 |
| `filename_pattern` | 否 | 文件名 glob，不匹配时返回 `path.filename` 错误 |

## 合并和校验语义

- 必填字段完全由根级及匹配的 `sections[].required_fields` 配置决定。
- 系统不再内置 `title`、`type`、`tags`、`created_at`、`updated_at` 等必填字段。
- 根级和目录级必填字段会合并，重复字段只保留一次。
- `Frontmatter` 的其他字段允许自由扩展。
- 多条目录规则匹配时按 `path` 长度从短到长应用，更具体的规则最后生效。
- `filename_pattern` 只校验匹配目录下文档的文件名。
- 标签建议使用小写规范形式，可用 `/` 表达层级，例如 `engineering/backend`。
- `tag_aliases` 只提供归一建议，不限制新标签；别名和大小写变体会产生 warning。重复标签会在检索归一时视为同一标签，不单独提示。
- `get_wiki_rules` 会返回当前配置、指引、动态 `known_tags` 和 `wiki_root`。
- 首次出现且未配置别名的新标签产生 warning，但仍允许保存。
- 所有问题都由 `validate_wiki` 返回；校验不会自动改写 Markdown。
- 配置中的未知字段会被拒绝，避免拼写错误或无效规则被静默忽略。

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
| `markdown.formatting` | warning | 文档不符合 mdformat 规范 |
| `markdown.parse` | error | Markdown 或 YAML Frontmatter 无法解析 |
