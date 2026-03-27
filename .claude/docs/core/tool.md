---
description: Tool system (4 built-in tools)
---

# Tool

Executes actions for the agent. Registered as function calling tools in LLM API.
Each tool is a Node (`prep → exec → post`).

## Built-in Tools

- **bash** — run shell commands, capture stdout/stderr. Blocks dangerous patterns (rm -rf /, sudo, shutdown, > /dev/). Requires `channel.confirm()` before execution.
- **read_file** — read file contents with line numbers. Validates path safety via `safe_path()`.
- **write_file** — write content to file, creates parent directories. Validates path safety.
- **edit_file** — replace a unique string in a file. Fails if match count != 1. Validates path safety.

## Path Safety

`safe_path()` canonicalizes paths and prevents directory traversal outside the working directory.

## dispatch_tool()

Routes tool calls by name, parses JSON arguments, runs the corresponding Node.
Returns `bool` indicating whether the tool was recognized.
