---
description: Tool system (dispatch_tool and built-in tools)
---

# Tool

Executes actions for the agent. Registered as OpenAI-compatible function calling tools in LLM API.
Each tool is a Node (`prep → exec → post`).

## Tool Categories

### File & Shell

| Tool | Parameters | Description |
|------|-----------|-------------|
| `bash` | `command` | Run shell command. Blocks dangerous patterns. Requires `channel.confirm()`. |
| `read_file` | `path` | Read file with line numbers. `safe_path()` validated. |
| `write_file` | `path`, `content` | Write file, auto-creates parent dirs. `safe_path()` validated. |
| `edit_file` | `path`, `old_string`, `new_string` | Replace unique string in file. Fails if match count != 1. |

### Search

| Tool | Parameters | Description |
|------|-----------|-------------|
| `glob` | `pattern`, `directory?` | Find files by glob. Auto-prefixes `**/` for bare patterns. Results sorted by mtime (newest first). |
| `grep` | `pattern`, `path?`, `include?` | Regex search via ripgrep (fallback: regex crate). Returns `file:line: content`. |

### Web

| Tool | Parameters | Description |
|------|-----------|-------------|
| `web_fetch` | `url`, `max_length?` | Fetch URL content. Truncates at `tuning.web_fetch_max_body` (default 50k chars). |
| `web_search` | `query` | Search via DuckDuckGo HTML. Returns up to 10 results with title, URL, snippet. |

### Planning

| Tool | Parameters | Description |
|------|-----------|-------------|
| `plan_mode` | `active` | Toggle plan mode. When active, agent should only research and plan, not execute changes. |

### Task Management (Session-level)

| Tool | Parameters | Description |
|------|-----------|-------------|
| `todo` | `items[]` | Replace full todo list. Only one item can be `in_progress` at a time. |

### Task Graph (Workspace-level, conditional: `has_tasks`)

| Tool | Parameters | Description |
|------|-----------|-------------|
| `task_create` | `subject`, `description?` | Create persistent task. |
| `task_update` | `task_id`, `status?`, `blocked_by?`, `blocks?` | Update status/deps. Completing auto-unblocks dependents. |
| `task_list` | _(none)_ | List all tasks with status and dependencies. |
| `task_get` | `task_id` | Get single task details. |
| `claim_task` | `task_id` | Claim unclaimed task. Sets owner + `in_progress`. |

### Background & Subagent

| Tool | Parameters | Description |
|------|-----------|-------------|
| `background_run` | `command` | Run shell command in background. Returns task ID. Results auto-delivered before next LLM turn. |
| `background_check` | `task_id?` | Check background task status. Omit ID to list all. |
| `task` | `prompt`, `agent` | Spawn one-shot subagent with isolated context. Only final summary returned. Disabled for subagents. |

### Team Collaboration (conditional: `has_team`)

| Tool | Parameters | Scope | Description |
|------|-----------|-------|-------------|
| `team_spawn` | `name`, `role`, `prompt`, `agent?` | lead only | Spawn persistent teammate with inbox. |
| `team_send` | `to`, `content` | all | Send message to teammate or `lead`. Wakes idle recipients. |
| `team_read_inbox` | `name?` | all | Read and drain inbox. Defaults to own inbox. |
| `shutdown_request` | `teammate` | lead only | Request graceful shutdown. Teammate can approve/reject. |
| `shutdown_response` | `request_id`, `approve`, `reason?` | teammate | Respond to shutdown request. |
| `plan_submit` | `plan` | teammate | Submit plan for lead review. Block until response. |
| `plan_response` | `request_id`, `approve`, `feedback?` | lead only | Approve or reject submitted plan. |
| `idle` | _(none)_ | teammate | Enter idle polling. Polls for inbox messages or unclaimed tasks. 60s timeout → shutdown. |

### Worktree Isolation (conditional: `has_worktrees`)

| Tool | Parameters | Description |
|------|-----------|-------------|
| `worktree_create` | `name`, `task_id?` | Create git worktree (`git worktree add -b wt/{name}`). Optional task binding (auto `in_progress`). |
| `worktree_remove` | `name`, `force?`, `complete_task?` | Remove worktree. `complete_task` also marks bound task completed. |
| `worktree_keep` | `name` | Mark worktree as persistent (skip auto-cleanup). |
| `worktree_list` | _(none)_ | List all worktrees with status and task bindings. |
| `worktree_exec` | `name`, `command` | Run shell command inside worktree directory. |

### Cron (conditional: `has_cron`)

| Tool | Parameters | Description |
|------|-----------|-------------|
| `cron_list` | _(none)_ | List all cron jobs with status, schedule kind, errors, next run. |
| `cron_create` | `id`, `name`, `schedule_kind`, `message`, `at?`, `every_seconds?`, `cron_expr?`, `payload_kind?`, `channel?`, `delete_after_run?` | Create a cron job. Persists to CRON.json. Schedule kinds: `at` (one-shot), `every` (interval), `cron` (expression). |
| `cron_delete` | `job_id` | Delete a cron job by ID. Persists to CRON.json. |

## Conditional Registration

`all_tools(is_subagent, has_tasks, has_team, has_worktrees, has_cron)` builds the tool list:

| Flag | Effect |
|------|--------|
| `is_subagent` | Disables `task`, `team_spawn`, `shutdown_request`, `plan_response`. Enables `idle`. |
| `has_tasks` | Enables task graph tools (`task_create/update/list/get`, `claim_task`). |
| `has_team` | Enables team tools. Lead vs teammate scope controlled by `is_subagent`. |
| `has_worktrees` | Enables worktree tools. |
| `has_cron` | Enables cron tools (`cron_list/create/delete`). |

Core tools (bash, read/write/edit_file, glob, grep, todo, background_run/check, web_fetch, web_search, plan_mode) are always available.

## Permission Policy

`ToolPolicy` in `protocol.rs`. Three modes:

| Mode | Behavior |
|------|----------|
| `Ask` (default) | Always `channel.confirm()` before execution |
| `Auto` | Auto-approve read-only tools, ask for write/exec |
| `Trust` | Auto-approve all |

Auto-approved tools: `read_file`, `glob`, `grep`, `todo`, `task_list`, `task_get`, `team_read_inbox`, `worktree_list`, `background_check`.

## Path Safety

`safe_path()` canonicalizes paths and rejects directory traversal outside the working directory. Applied to `read_file`, `write_file`, `edit_file`.

## dispatch_tool()

Routes tool calls by name via match statement. For each call:

1. Parse JSON arguments from `&str`
2. Create Node implementation
3. `node.run(store)` → `prep → exec → post`
4. `push_tool_result()` appends `Message { role: Tool, tool_call_id }` to history

Returns `Ok(true)` if tool recognized, `Ok(false)` for unknown tools. Errors caught and pushed as tool results.

## Schema

Tool definitions in `schema.rs`. Each returns `ToolDefinition { type: "function", function: ToolFunction { name, description, parameters } }`. Parameters follow JSON Schema format for OpenAI function calling.
