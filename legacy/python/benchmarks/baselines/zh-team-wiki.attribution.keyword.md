# Retrieval loss attribution 20260910T171219Z

- Corpus: `/Users/zhuxining/Code/AgentWiki/benchmarks/corpora/zh-team-wiki` (version `c70f6b33905854d823ce96aa7b64fcb0fcb407f2ed3a0bc83a87e06606999ee2`)
- Mode: `keyword`, k = 5
- Judgments: 17

## Loss layers

| Layer | Count |
| --- | ---: |
| `retrieved` | 12 |
| `granularity_only` | 2 |
| `wrong_section` | 3 |

## Rates

- Index ceiling (judged section exists in the index): 1.0000
- Document recall (judged file appears in results): 1.0000
- Retrieved (exact section within k): 0.7059
- Granularity loss (labeling, not retrieval): 0.1176
