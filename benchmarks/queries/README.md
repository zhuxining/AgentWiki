# Benchmark query sets

把真实 Wiki 的查询集放在这里时，使用 `retrieval.jsonl`，每行符合
`benchmarks.schema.BenchmarkQuery`。查询集应提交脱敏后的查询和人工标注，不要提交真实敏感
文本。

变更场景可以另存为 `mutations.jsonl`，建议使用以下字段：

```json
{"id":"update-auth-001","operation":"update","path":"decisions/auth.md","expected":{"old_term_absent":true,"new_term_present":true}}
```

`mutations.jsonl` 是同步/恢复行为的执行清单，不会被普通检索 runner 当作查询集读取。
