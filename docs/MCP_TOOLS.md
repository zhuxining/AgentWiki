# AgentWiki MCP 契约

> **目标契约，待整体迁移实现。** 当前 src/mcp.rs 仅输出接线提示，尚未提供可调用工具；本文的新参数、返回结构及行为均不能作为当前运行结果。目标采用官方 rmcp，stdio 入口由 mcp feature 隔离。

MCP 保留三个工具：任务检索、规则获取和规范校验。已知路径读取和文档增删改由 Agent 原生工具完成；显式格式修复是校验工具的有限写入能力，不提供通用文件 CRUD。

配置只由入口读取 `~/.agentwiki/config.json`，不引入业务环境变量配置。配置和数据生命周期见 [架构](ARCHITECTURE.md)。

## get_wiki_context

为当前任务返回有界证据：

```text
query: str = ""
scope: str = ""
limit: int = 10              # 1..20
tags: list[str] | null
note_types: list[str] | null
metadata_filters: object | null
```

- 空 query 返回最近修改文档；普通 query 使用精确匹配、LanceDB BM25 和可选语义混合检索；明确近期主题查询加入新近度。
- scope 是 Wiki 相对目录或文档，空值表示全库；越界和非法参数返回错误。
- tags 按规范化标签全部满足过滤，note_types 匹配任一指定类型；空列表不限制。metadata_filters 为字段等值条件，多个字段全部满足，不接受查询代码。
- 不暴露 keyword/semantic/hybrid 模式选择或分页；同一文档最多两个片段，limit 限制返回片段总数。

目标返回示例：

```json
{
  "query": "认证方案",
  "scope": "",
  "strategy": "hybrid",
  "degraded": [],
  "results": [
    {
      "path": "decisions/auth.md",
      "title": "认证方案",
      "section": "刷新令牌",
      "snippet": "与查询相关的证据片段",
      "rank_score": 0.0325,
      "match_sources": ["keyword", "semantic"],
      "modified_at": "2026-09-10T08:00:00Z",
      "frontmatter": {},
      "related": [
        {
          "path": "architecture/retrieval.md",
          "title": "检索架构",
          "relation_type": "depends_on",
          "direction": "outgoing",
          "resolution_status": "resolved",
          "source_section": "架构",
          "context": "该文档依赖检索架构。"
        }
      ]
    }
  ],
  "truncated": false
}
```

strategy 为 recent、keyword、hybrid 或 recent_hybrid，表示实际采用的策略。rank_score 仅解释本次排序，不是相似度或置信度，不跨查询比较，也不承诺固定公式近似值。match_sources 使用实际参与的 exact、keyword、semantic、recency 来源；不得从开启的配置推断命中来源。truncated 表示因输出或候选预算而未返回全部可用证据。

related 最多五条一跳入边或出边，包含来源章节与原文上下文；目标不存在仍保留 unresolved 和目标路径，未知标题为空字符串。不要自动扩展到多跳或抽取实体。

查询前执行增量同步。首次或变化后的查询允许等待同步向量批处理，不存在后台任务完成通知。向量按实际输入哈希与模型身份复用，模型变化使旧向量失效并重新计算；失败记录留待后续同步重试。

启用模型但不可用、文档同步失败和非法 relations 等进入 degraded。主动关闭模型、正常无匹配不记作故障；无匹配正常返回空 results。语义故障保留关键词能力，全部检索来源失败必须明确报告，不能伪装成正常无匹配。保留旧投影的失败文档需标明路径和过期原因。

证据只用于定位；Agent 形成结论前应回读关键原文。

## get_wiki_rules

```text
scope: str = ""
```

返回根规则、匹配范围的目录规则、default_type、required_fields、tag_aliases、动态 known_tags、wiki_root 和 guide_content。结构化规则见 [规则参考](RULES.md)，guide_content 来自 Wiki 根目录 AGENTWIKI.md 正文。

返回 source_modified_at_ns 和 source_size 表示规则文件指纹，只用于判断规则内容是否变化。known_tags 随文档投影变化更新，不能仅按规则文件指纹缓存。调用时先确认文档投影新鲜度；为读取规则不启动无关 embedding 计算。

新建、移动或首次修改陌生范围前调用；同一范围的连续编辑可复用规则，但不得将规则文件未变理解为标签集合未变。系统没有内置必填字段，新标签只警告，允许扩展 Frontmatter。

## validate_wiki

```text
path: str | null
full: bool = false
fix_format: bool = false
```

范围与写入约束：

| 参数 | 行为 |
| --- | --- |
| path 指定、full=false | 校验指定普通 Markdown 文档 |
| path 缺省、full=true | 全库校验普通文档，同时报告规则解析问题 |
| path 缺省、full=false | 参数错误，要求明确范围 |
| path 指定、full=true | 参数错误，范围互斥 |
| fix_format=false | 只报告，不写文件 |
| fix_format=true | 在上述明确范围内修复格式，写回后重新校验 |

path 必须为 Wiki 内的普通 Markdown 文件；AGENTWIKI.md 不进入普通文档格式修复范围。规则文件错误通过规则校验报告，由原生工具修正。

检查 Markdown 结构、Frontmatter、目录/类型约束、标签、内部链接与内置 dprint 格式。修复只修改格式，不修复标签、关系、链接或业务内容；保留 Frontmatter 原文和代码块内部。格式规范及错误码见 [规则参考](RULES.md)。

格式写回前核对文件指纹与原文，外部变化则跳过并报告冲突；同目录临时文件替换并保留权限。无变化不写回，单文件失败不回滚其他文件已完成的修复。不得用根目录检查代替对具体目标和符号链接的检查。

返回结构在只读与修复模式中一致：

```json
{
  "issues": [
    {
      "path": "decisions/auth.md",
      "kind": "frontmatter.required",
      "message": "缺少规则要求的 owner 字段",
      "severity": "error"
    }
  ],
  "formatted_paths": ["decisions/auth.md"]
}
```

issues 是检查或修复后的问题集合，severity 为 error 或 warning；formatted_paths 仅列出本次成功写回的路径，只读或无变化时为空。格式修复成功不意味着业务校验全部通过。

CLI 对应使用 `validate-wiki --path <path> --fix-format` 或 `validate-wiki --full --fix-format`，均为迁移后新增行为。保留现有 CLI 不带 path 的全库只读校验，但不得据此隐式允许全库格式写回。

## Agent 工作流

1. 依赖历史知识、未知位置或近期变化时检索；已知准确路径的简单操作直接使用原生工具。
2. 命中后回读原文；证据不足、有矛盾或出现新实体时细化查询。
3. 陌生范围写前取规则，使用原生工具修改文档。
4. 修改后按 path 校验；完整验收才使用 full=true。
5. 明确需要格式修复时设置 fix_format=true，并检查返回的剩余问题；不默认开启写入。
