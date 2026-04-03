---
description: Node abstraction
---

# Node

Smallest building block. Every operation is a Node: `prep → exec → post`.

## Prep

- Read from `&SharedStore`, return `PrepRes`.

## Exec

- Pure computation. No SharedStore access.
- Receives `PrepRes`, returns `ExecRes`.
- Examples: LLM API calls, shell commands, file I/O.

## Post

- Write results to `&mut SharedStore`.
- Receives both `PrepRes` and `ExecRes`.

## Implementations

- `LLMCall` — streaming LLM request.
- `BashExec` — shell command execution.
- `ReadFile`, `WriteFile`, `EditFile` — file operations.
- `MemoryWrite` — LLM-generated TLDR, writes memory file and hot-updates index.
