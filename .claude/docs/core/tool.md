---
description: Tool system (dispatch_tool and built-in tools)
---

# Tool

Executes actions for the agent. Registered as function calling tools in LLM API.
Each tool is a Node (`prep → exec → post`).

## Built-in Tools

### File & Shell

- **bash** — run shell commands, capture stdout/stderr. Blocks dangerous patterns. Requires `channel.confirm()` before execution.
- **read_file** — read file contents with line numbers. Validates path safety via `safe_path()`.
- **write_file** — write content to file, creates parent directories. Validates path safety.
- **edit_file** — replace a unique string in a file. Fails if match count != 1. Validates path safety.
- **glob** — find files by glob pattern.
- **grep** — search file contents by regex.

### Task Management

- **todo** — manage a per-session todo list.
- **task_create** — create a task in the persistent task graph.
- **task_update** — update task status, owner, or fields.
- **task_list** — list tasks with optional filters.
- **task_get** — get a single task by ID.
- **claim_task** — claim an unowned pending task (used by teammates during idle polling).

### Background & Subagent

- **background_run** — run a shell command in the background with notification on completion.
- **background_check** — check status of a background task.
- **task** — spawn a one-shot subagent with isolated context.

### Team Collaboration

- **team_spawn** — spawn a teammate (lead only).
- **team_send** — send a message to another agent's inbox.
- **team_read_inbox** — read and drain own inbox.
- **shutdown_request** / **shutdown_response** — structured shutdown protocol.
- **plan_submit** / **plan_response** — structured plan approval protocol.
- **idle** — enter idle polling mode (breaks out of cot_loop).

### Worktree Isolation

- **worktree_create** — create a git worktree, optionally bound to a task.
- **worktree_remove** — remove a worktree, optionally completing the bound task.
- **worktree_keep** — mark a worktree as persistent (skip auto-cleanup).
- **worktree_list** — list all tracked worktrees.
- **worktree_exec** — execute a command inside a worktree directory.

## Path Safety

`safe_path()` canonicalizes paths and prevents directory traversal outside the working directory.

## dispatch_tool()

Routes tool calls by name, parses JSON arguments, runs the corresponding Node.
Returns `bool` indicating whether the tool was recognized.
