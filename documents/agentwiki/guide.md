---
title: AgentWiki 使用指南
type: guide
tags:
  - agentwiki
  - guide
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
---

# AgentWiki 使用指南

## 检索知识

当任务涉及历史方案、团队约定、已有决策、跨文档关系或近期变化时，先调用 `get_wiki_context`。
检索命中后，使用 Agent 原生文件工具读取关键文档原文，再形成结论。

## 编写文档

新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。每篇 Markdown 文档都应包含完整 Frontmatter：`title`、`type`、`tags`、`created_at` 和 `updated_at`。

## 修改后校验

使用原生工具创建或编辑 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。校验发现问题时，由 Agent 修复后再次校验。

## 文档组织

- `guides/`：操作指南和流程
- `decisions/`：已确认的技术或产品决策
- `projects/`：项目相关知识
- 其他内容放在根目录或合适的主题目录中

SQLite 仅是可重建的检索索引，Markdown 文件才是知识事实源。
