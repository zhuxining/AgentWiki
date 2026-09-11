---
name: AgentWiki
purpose: 为 Agent 提供可检索的团队知识、技术决策和操作指南
default_type: note

# 必填字段完全由本文件决定，系统不内置任何必填字段。
# 增删这些字段即可调整约束；设为空列表（required_fields: []）表示不强制任何字段。
required_fields:
  - title
  - type
  - tags

# 同义标签归一：键是规范标签，值是别名列表。
# tag_aliases:
#   architecture:
#     - Architecture
#     - arch

# 目录级规则：path 相对 Wiki 根目录，支持 glob。
# sections:
#   - path: decisions
#     description: 已确认的技术与产品决策
#     types:
#       - decision
#     required_fields:
#       - status
#     filename_pattern: "*.md"
---

# Wiki 使用指南

本文件是 Wiki 的组织规则与 Agent 指导入口。Frontmatter 承载结构化规则，正文会作为
`guide_content` 返回给 Agent；本文件不参与普通文档索引。

> 本文件由 AgentWiki 在启动时自动生成（仅在缺失时写入，已存在则永不覆盖），可以按团队
> 需要自由修改。上面的 `required_fields` 一旦声明就会对全部文档生效：如果 Wiki 里已有
> 大量缺少这些字段的历史文档，`validate_wiki` 会一次性报出多条 `frontmatter.required`。
> 这不影响检索，按需要的粒度放宽或清空该字段即可。删除本文件会在下次启动时重新生成默认版本。
> 完整规则字段说明见 AgentWiki 仓库的 `docs/RULES.md`。

## 检索

任务涉及历史方案、已有决策、跨文档关系或近期变化时，先调用 `get_wiki_context`。
检索只返回候选证据片段，形成结论前必须用原生工具读取关键文档原文。

## 写入前取规则

新建、移动或首次修改陌生目录前，调用 `get_wiki_rules` 获取该范围适用的类型、必填字段和
文件名约束。同一范围的连续编辑可以复用一次结果。

## 修改后校验

用原生工具创建或编辑 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束
和内部链接。校验只报告问题，不会自动改写文件。

## 目录组织

下面只是建议，按实际需要增删；要让某个目录的规则生效，请在 Frontmatter 的 `sections` 中声明。

- `guides/`：操作指南和流程
- `decisions/`：已确认的技术或产品决策
- `projects/`：项目相关知识
- `notes/`：一般知识和记录

Markdown 文件是知识的事实源；SQLite 索引可删除、可重建，不是事实源。
