---
description: Tool system
---

# Tool

Executes actions for the agent. Registered as function calling tools in LLM API.

Built-in: `bash` — run shell commands, capture stdout/stderr.

As a Node:
- `prep`: read command/arguments from Context.
- `exec`: run the tool logic.
- `post`: write output to Context as observation (tool role).
