# AgentWiki

`AgentWiki` is an Obsidian-based shared memory/knowledge layer for agents, providing governed context retrieval, memory proposals, episodic summaries, and project state across Claude, OpenClaw, Pi, Codex, and other AI tools.

## Quick start

```bash
python -m pip install -e .
AgentWiki --source notes --target memory
```

## Development

```bash
python -m pip install -e ".[dev]"
pytest
```
