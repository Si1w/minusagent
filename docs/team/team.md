---
description: Multi-agent team collaboration, inbox messaging, and lifecycle
---

# Team

Persistent multi-agent collaboration via JSONL inboxes. Located in `src/team/manager.rs`.

## File Layout

```
WORKSPACE_DIR/.team/
  config.json           <- team roster + statuses
  inbox/
    alice.jsonl         <- append-only, drain-on-read
    bob.jsonl
    lead.jsonl
```

## TeammateManager

Manages roster (`config.json`), spawns teammate tokio tasks, handles wake-on-message.

## MessageBus

JSONL file per agent. `send()` appends; `read_inbox()` reads all + truncates.

## Lifecycle

`spawn → WORKING → IDLE → WORKING → ... → SHUTDOWN`

Idle teammates wake when they receive a message.

## Inbox Drain

In `cot_loop`, before each LLM call, the agent's inbox is auto-drained and injected into history (like background notifications). Lead agent name defaults to "lead".

## Prefix Cache Optimization

`build_teammate_identity()` returns the base agent identity only (no teammate-specific context). All teammates sharing the same `agent_id` produce a byte-identical system prompt, enabling LLM KV cache prefix reuse.

Teammate context (name, role) is injected into the initial user message via `build_fork_message()` using `FORK_PREFIX` as shared prefix.

## Protocols

Structured request-response over inbox with shared FSM (`pending → approved | rejected`):

- **Shutdown**: `shutdown_request` (lead) → `shutdown_response` (teammate)
- **Plan approval**: `plan_submit` (teammate) → `plan_response` (lead)
- `InboxMessage.extra` carries protocol metadata (`request_id`, `approve`)

## Autonomy

Teammates self-organize via idle polling. After cot_loop finishes, they poll every 5s (up to 60s) for:
1. Inbox messages
2. Unclaimed tasks on the task board (pending, no owner, not blocked)

Auto-claims first available task. 60s timeout → shutdown.

## Identity Re-injection

When teammate history ≤ 3 messages (post-compression), identity block is re-inserted at the start.

## Worktree Isolation

`WorktreeManager` creates per-task git worktrees (`git worktree add -b wt/{name}`). Tracked in `.worktrees/index.json`, lifecycle events in `events.jsonl`.

Tools: `worktree_create`, `worktree_remove`, `worktree_keep`, `worktree_list`, `worktree_exec`.

## Task Graph

`TaskManager` in `src/team/task.rs`. Persistent task graph with dependencies. `BackgroundManager` runs shell commands in background with notification queue.

## Todo

`TodoManager` in `src/team/todo.rs`. Per-session todo list. `TodoWrite` is a Node that manages todo items via tool calls. Statuses: `Pending`, `InProgress`, `Completed`.

## vs Subagent

Subagents (`task` tool) are one-shot with isolated context. Teammates are persistent with inbox communication, lifecycle management, and autonomous task claiming.
