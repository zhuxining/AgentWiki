---
name: AgentWiki 示例知识库
purpose: 为 Agent 提供可检索的团队知识、操作指南和项目资料

required_fields:
  - title
  - type
  - tags
  - created_at
  - updated_at
  - owner
default_type: note

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
    filename_pattern: "*.md"
    required_fields:
      - project

  - path: notes
    description: 一般知识与记录
    types:
      - note
      - guide
    filename_pattern: "*.md"

tag_aliases:
  guide:
    - guides
  decision:
    - decisions
  architecture:
    - arch
---

# AgentWiki 使用指南

## 检索知识

当任务涉及历史方案、团队约定、已有决策、跨文档关系或近期变化时，先调用 `get_wiki_context`。检索命中后，使用 Agent 原生文件工具读取关键文档原文，再形成结论。

## 编写文档

新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。每篇 Markdown 文档都应包含 `required_fields` 中声明的 Frontmatter 字段。

## 修改后校验

使用原生工具创建或编辑 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。发现问题时修复后再次校验。

## 文档组织

- `guides/`：操作指南和流程
- `decisions/`：已确认的技术或产品决策
- `projects/`：项目相关知识
- `notes/`：一般知识和记录

SQLite 仅是可重建的检索索引，Markdown 文件才是知识事实源。
