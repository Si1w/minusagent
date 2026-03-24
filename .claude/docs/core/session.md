---
description: Session orchestrator and JSONL persistence
---

# Session

Orchestrator between Frontend and Agent. Owns SharedStore and Agent.

## turn(input, channel)

- Routes `/commands` to `handle_command()`.
- If Intelligence is configured, rebuilds `system_prompt` via `Intelligence::build_prompt()` before each turn.
- Appends user message to history, runs `Agent::run()`.
- After agent completes, checks if `total_tokens > 80% context_window` → auto-compact.

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new <label>` | Create new session |
| | `/save` | Save session to JSONL |
| | `/load <id>` | Load by ID or prefix |
| | `/list` | List all sessions |
| | `/compact` | Compact history |
| Intelligence | `/remember <name> <txt>` | Save memory (MemoryWrite Node) |
| | `/<skill> [args]` | Invoke discovered skill |

Note: `/agents`, `/switch`, `/bind`, `/discord`, `/gateway`, `/exit` are handled in `main.rs` CLI task, not in Session.

## Persistence (SessionStore)

- `sessions/` directory, one `.jsonl` file per session.
- `sessions/sessions.json` — index with metadata (label, created_at, last_active, message_count).
- Prefix matching: `/load abc` matches any session ID starting with `abc`.
- `SessionStore::new(base_dir)` accepts custom directory (tests use tempdir).

## Compaction

- Keeps the most recent 20% of messages (min 4).
- Summarizes the first 50% into a single user/assistant pair via LLM (using SilentChannel).