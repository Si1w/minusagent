---
description: Tool permission policy and session control protocol
---

# Protocol

Session control and tool permission system. Located in `src/routing/protocol.rs`.

## ToolPolicy

Per-session tool permission policy with three modes:

| Mode | Behavior |
|------|----------|
| `Ask` (default) | Always `channel.confirm()` before execution |
| `Auto` | Auto-approve read-only tools, ask for write/exec |
| `Trust` | Auto-approve all |

Auto-approved tools in `Auto` mode: `read_file`, `glob`, `grep`, `todo`, `task_list`, `task_get`, `team_read_inbox`, `worktree_list`, `background_check`.

### Per-tool Overrides

`ToolPolicy.overrides: HashMap<String, bool>` allows per-tool allow/deny. Overrides take precedence over the mode.

`ToolPolicy::from_denied(denied)` creates a policy with specific tools denied. Used by per-agent `denied_tools` config in `AGENT.md` frontmatter.

## SessionControl

Control events sent to sessions via `ControlEvent`:
- Interrupt signals
- Session lifecycle management

## ProtocolChannel

Wraps a `Channel` with protocol-level concerns (permission checking, control event handling).
