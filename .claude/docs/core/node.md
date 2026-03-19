---
description: Node abstraction
---

# Node

Smallest building block. Every operation is a Node: `prep → exec → post`.

## Prep

- Read from `SharedStore`, return `prep_res`.
- `prep_res` is passed to both `exec()` and `post()`.

## Exec

- Pure computation. No `SharedStore` access.
- Examples: LLM calls, shell commands, HTTP requests.
- Supports retry. Must be idempotent if retries enabled.
- Return `exec_res`, passed to `post()`.

## Post

- Write results to `SharedStore`.
