# AgentWiki MCP 能力

AgentWiki MCP 只提供任务检索、组织规则和确定性校验。已知路径读取与 Markdown 增删改由
Agent 原生文件工具完成。

运行配置只读取用户配置目录 `~/.agentwiki/config.json`，不读取环境变量。相对的文档库和索引
路径以该配置文件所在目录（即 `~/.agentwiki`）为基准解析；配置文件缺失时首次运行会创建默认
配置，默认文档根目录是 `~/AgentWiki`。

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
      "rank_score": 0.032522,
      "match_sources": ["keyword", "semantic", "recency"],
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

`rank_score` 是排名融合分，只用于解释排序：它约等于「命中该证据的来源数 / 61」，不是
相似度，也不能跨查询比较。判断证据可信度请看 `match_sources`（命中来源越多越强）。
`strategy` 为 `recent`、`keyword`、`hybrid` 或 `recent_hybrid`。`related` 是最多 5 条的一跳
入边或出边关联，包含关系来源的 `source_section` 和 `context`；目标不存在时仍会返回
`unresolved` 关系及其目标路径。`degraded` 会说明语义不可用、具体文档索引失败或非法
Frontmatter 关系声明。结果是候选证据；形成结论前应使用原生工具读取关键原文。

语义向量按 chunk 维护 `pending`、`ready`、`error` 状态。首次建立向量索引时工具会等待初始
同步，后续 Markdown 变更先返回关键词/图谱结果，向量在后台更新；模型切换会自动丢弃旧模型
向量并重建。若上一个进程在向量任务完成前退出，下一次 runtime 启动会恢复遗留的 pending
任务并重新排队。

## `get_wiki_rules`

参数 `scope: str = ""`。返回根规则与匹配目录规则合并后的 `WikiRules`，包括默认类型、
`required_fields`、可选 `tag_aliases`、动态 `known_tags`、目录规则和 guide 内容。标题、类型、
必填字段由 `AGENTWIKI.md` 中的 `required_fields` 配置决定。新标签允许使用但会被标记为 warning；别名、大小写变体和
层级标签会按规范标签归一。

新建、移动或首次修改陌生范围前调用；同一范围的连续编辑可复用结果。
完整 YAML 字段见 [Wiki Rules 配置参考](RULES.md)。

返回值还带 `source_modified_at_ns` 与 `source_size`——规则文件自身的大小与修改时间。复用上一次
结果前先比对这两个值：发生变化即说明规则已更新，需要重新调用。服务端按同一指纹缓存
`known_tags`，规则文件未变化时不会重新读取整个文档库。

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
