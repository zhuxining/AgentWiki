# AgentWiki MCP 能力

AgentWiki MCP 只提供任务检索、组织规则和确定性校验。已知路径读取与 Markdown 增删改由
Agent 原生文件工具完成。

运行配置只读取当前项目的 `.agentwiki/config.json`，不读取环境变量。相对的文档库和索引
路径以配置文件所在项目根目录为基准。

## `get_wiki_context`

为当前任务返回有界证据集合：

```text
query: str = ""
scope: str = ""
limit: int = 10              # 1..20
tags: list[str] | null
note_types: list[str] | null
metadata_filters: object | null
```

- 空查询：最近修改文档；
- 普通查询：精确匹配、FTS5 和可选语义混合检索；
- 近期主题查询：相关性与新近度融合；
- `scope`：Wiki 相对目录或文档范围；
- 不暴露 keyword/semantic/hybrid 模式选择和分页。

返回：

```json
{
  "query": "最近认证方案有什么变化",
  "scope": "",
  "strategy": "recent_hybrid",
  "degraded": [],
  "results": [
    {
      "path": "decisions/auth.md",
      "title": "认证方案",
      "section": "刷新令牌",
      "snippet": "与查询相关的证据片段",
      "score": 0.032522,
      "match_sources": ["keyword", "semantic", "recency"],
      "modified_at": "2026-09-10T08:00:00Z",
      "frontmatter": {}
    }
  ],
  "truncated": false
}
```

`strategy` 为 `recent`、`keyword`、`hybrid` 或 `recent_hybrid`。`degraded` 会说明语义不可用
或具体文档索引失败。结果是候选证据；形成结论前应使用原生工具读取关键原文。

## `get_wiki_rules`

参数 `scope: str = ""`。返回根规则与匹配目录规则合并后的 `WikiRules`，包括默认类型、
`required_fields`、可选 `tag_aliases`、动态 `known_tags`、目录规则和 guide 内容。标题、类型、
必填字段由 `AGENTWIKI.md` 中的 `required_fields` 配置决定。新标签允许使用但会被标记为 warning；别名、大小写变体和
层级标签会按规范标签归一。

新建、移动或首次修改陌生范围前调用；同一范围的连续编辑可复用结果。
完整 YAML 字段见 [Wiki Rules 配置参考](RULES.md)。

## `validate_wiki`

```text
path: str | null
full: bool = false
```

检查 Markdown 格式、目录和类型约束、Frontmatter 以及内部链接。所有问题只返回给 Agent
自行修复，不存在阻断配置，也不会改写文件；
`markdown.formatting` 表示内容与项目 mdformat 规范不一致。仅完整验收时使用 `full=true`。

## 自动指引

MCP instructions 和工具描述会要求 Agent：

1. 历史、约定、已有方案、关联关系、近期变化或未知位置需要检索；
2. 命中后读取关键原文；
3. 新实体、证据不足或矛盾时重新检索；
4. 陌生范围写前取规则；
5. 原生工具修改后执行校验；
6. 已知准确路径的简单操作跳过检索。
