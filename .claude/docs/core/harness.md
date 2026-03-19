---
description: Harness for terminal operations
---

# Harness

Executes shell commands and returns results. Harness ≠ Frontend.

As a Node:
- `prep`: read command from Context.
- `exec`: run in shell, capture stdout/stderr.
- `post`: write output to Context as observation.
