# AGENTS.md

This file provides guidance to Code Agents (claude.ai/code、codex、pi、opencode) when working with code in this repository.

## Project Overview

`AgentWiki` is an Obsidian-based shared memory layer for agents, providing governed context retrieval, memory proposals, episodic summaries, and project state across Claude, OpenClaw, Pi, Codex, and other AI tools.

- Python 3.14, managed with **uv**
- Src layout: source in `src/AgentWiki/` (built as wheel package via `uv_build`)
- Entry point: `src/AgentWiki/cli.py` → `cli()` (registered as `agw` command via `project.scripts`)

## Commands

```bash
# Install dependencies
uv sync

# Run the CLI
uv run meb

# Lint & format (auto-fix enabled)
uv run ruff check --fix

# Type check
uv run ty check --fix

# Run all tests
uv run pytest

```


## 通用约定

### 代码质量

- AI/Agent 生成的代码同样遵守本文件全部规范：提交前必须过 `vpr check`，且不得修改不可修改目录与自动生成文件。
- 使用 zod、React 等依赖时禁止使用已废弃（deprecated）的 API、方法或类，必须采用当前版本推荐用法；升级大版本时同步迁移旧写法，避免产生废弃警告。

### Git 与提交

- Commit 遵循 Conventional Commits：`feat` / `fix` / `refactor` / `docs` / `test` / `chore`。

## 编码原则

### 需求对齐

- 动手前先确认目标与边界，识别隐含约束与假设；信息不足先提问，不臆测需求。
- 复杂改动先产出方案再写代码。

### 架构演进

- 架构非永恒，过去的抽象未必适用当下。
- 某功能频繁出问题时，优先演进架构，而非继续打补丁。
- 架构演进是例外手段，不是默认动作。

### 代码结构

- 日常改动融入既有架构与代码约定，与周边保持一致性。
- 保持小而明确的单一职责，避免无关逻辑混杂。
- 结构是架构的语言：从包/模块边界、目录、文件，到类型、成员（方法、字段、属性、常量）与函数，命名与归属都应表达领域模型；编码中发现结构不再表达领域时，主动调整（移动、拆分、重命名），保持内聚、层次与依赖方向清晰，而不是继续往里塞。

### 问题解决

- 回归第一性原理：先定位问题本质，再分析根本原因，结合整体约束选方案，避免局部补丁。
- 方案不止一种，优先简洁可靠的（遵循KISS：简单、可理解、可维护）。

### Bug 修复

- 先问"为何测试没覆盖"，写最小测试稳定复现、确认失败；修复根因；按改动范围回归验证。
- 无法自动化复现时，保留可重复的最小验证步骤。
- 测试有维护成本，围绕风险与边界保持必要且精简。