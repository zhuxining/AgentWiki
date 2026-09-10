# AgentWiki 架构

## 1. 产品边界

AgentWiki 是本地 Markdown Wiki 的搜查与治理层。它负责：

- 为 Agent 检索历史、约定、已有方案、关联内容和近期变化；
- 使用 FTS5、可选本地 embedding 和排名融合返回章节级证据；
- 提供目录组织规则，并在原生文件修改后执行确定性校验；
- 保持 SQLite 投影可删除、可增量同步和可全量重建。

Agent 原生工具负责已知路径读取、创建、编辑、移动和删除。HTTP API、云同步、Web UI、
知识图谱、查询 LLM 和操作审计不属于当前范围。

## 2. 数据与检索

Markdown 正文和 YAML Frontmatter 是事实源。`_agentwiki/context.yaml` 与
`_agentwiki/guide.md` 是保留治理文件，不进入普通索引。

索引保存：

- 文档路径、标题、Frontmatter、真实 `mtime_ns` 和大小；
- 按 Markdown 标题层级切分的有界片段；
- 片段级 FTS5 投影和可选向量；
- 用于变化检测的文件指纹。

查询前执行增量确认，只解析变化文档并清理删除投影。rebuild 不得用执行时间覆盖文件修改
时间。“最近活动”因此表示当前仍存在文档的真实修改时间，不是索引时间或审计历史。

普通查询并发取得路径/标题、关键词和语义候选，再使用排名融合；同一文档最多返回两个片段。
语义依赖不可用时降级为关键词查询，并在结果中说明原因。带近期意图的主题查询额外加入
新近度排名，普通查询不受时间偏置。

## 3. 模块与依赖

```text
CLI / MCP composition roots
            ↓
       runtime context
            ↓
 services/retrieval + services/governance
            ↓
 service ports + domain values
            ↑
 indexing / repository / markdown adapters
```

- `domain`：文档、检索结果、规则与校验的纯模型；
- `services`：任务检索策略、排名融合、规则合并和校验；
- `indexing`：标题感知切块、文件指纹、增量同步和 rebuild；
- `repository`：SQLite 生命周期、片段投影、FTS5 和向量候选；
- `markdown`：路径安全、只读扫描、Frontmatter 解析和格式比较；
- `runtime`：显式资源装配、同步锁和可选 watcher；
- `cli`、`mcp`：读取配置、协议适配和结果序列化。

services 通过 Protocol 使用稳定边界，不直接创建 SQLite 或读取配置。composition root
统一读取项目根目录的 `.agentwiki/config.json`；runtime factory 只接受已经解析的构造参数。

## 4. Agent 工作流

1. 任务依赖 Wiki 知识、近期变化或未知位置时调用 `get_wiki_context`；
2. 用 Agent 原生工具读取关键命中文档，不能只根据摘要下结论；
3. 出现新实体、证据不足或矛盾时细化查询并再次检索；
4. 新建、移动或首次修改陌生范围前调用 `get_wiki_rules`；
5. 使用原生工具修改 Markdown；
6. 调用 `validate_wiki(path=...)`，全库验收才使用 `full=true`。

已知准确路径且不依赖其他 Wiki 知识时，直接使用原生文件工具，不做无效检索。

## 5. 可靠性

- 文档变更不会因索引或 embedding 失败而被回滚；
- 单篇解析失败不会阻断其他文档，失败路径进入检索降级信息；
- embedding 失败不会破坏关键词投影；
- 路径和 scope 必须留在 Wiki 根目录；外部符号链接不进入索引；
- 校验只报告问题，不自动改写原生工具产生的 Markdown；
- Frontmatter 始终要求 `title`、`type`、`tags`、`created_at`、`updated_at`；规则可追加必填字段，并通过可选 `tag_aliases` 归一同义标签；动态 `known_tags` 用于复用提示，新标签仅告警、不阻断，其他字段允许扩展；
- SQLite 连接由 async runtime 显式初始化并关闭。
