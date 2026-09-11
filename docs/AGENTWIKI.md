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

> **本文档定位**：以下使用指引采用待迁移的目标契约，当前 MCP 和格式修复尚未接线。这是规则文件 `AGENTWIKI.md` 的**完整参考示例**，演示 `sections`、
> `tag_aliases` 等进阶能力。新建 Wiki 时实际写入的是代码内置的**精简默认模板**
> （`src/markdown.rs` 的 `DEFAULT_AGENTWIKI`，仅 `name` / `purpose` / `required_fields` /
> `default_type`）；规则文件已存在时永远不被覆盖。把本文件复制到 Wiki 根目录即启用完整规则。

# AgentWiki 使用指南

## 检索知识

当任务涉及历史方案、团队约定、已有决策、跨文档关系或近期变化时，先调用 `get_wiki_context`。检索命中后，使用 Agent 原生文件工具读取关键文档原文，再形成结论。

## 编写文档

新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。每篇 Markdown 文档都应包含 `required_fields` 中声明的 Frontmatter 字段。

目录规则中的 `path` 是相对于 Wiki 根目录的路径。`path: decisions` 会匹配 `decisions/` 及其所有子目录中的文件，不会匹配 `projects/decisions/` 或 `decisions-old/`。`path` 也支持 glob，例如 `*decisions*` 匹配路径字符串中包含 `decisions` 的路径，`projects/*/decisions/*` 匹配项目目录下 decisions 范围内的文档。模式整体匹配路径，`*` 可跨 `/`，不自动追加子树；完整语义见仓库的规则参考，glob 应谨慎使用。

## 修改后校验

使用原生工具创建或编辑 Markdown 后，按 `path` 调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。完整验收才使用 `full=true`；path 与 full 互斥。

默认只报告，不写文件。明确需要格式修复时设置 `fix_format=true`，由内置格式化组件修复请求范围的格式并重新校验。检查 `formatted_paths` 和剩余问题；格式修复不会修改 Frontmatter 原文、代码块内部或修正标签、链接和业务内容。规则文件不进入普通文档格式修复范围。

外部编辑冲突时跳过写回，重新读取再处理。其他问题使用原生工具修正后再次校验。上述 MCP 与格式修复能力需在整体迁移完成后使用。

## 文档组织

- `guides/`：操作指南和流程
- `decisions/`：已确认的技术或产品决策
- `projects/`：项目相关知识
- `notes/`：一般知识和记录

目标检索索引（LanceDB）与元数据存储（SQLite）都是可重建的派生数据，Markdown 文件才是知识事实源。
