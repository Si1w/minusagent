---
description: Session orchestrator and JSONL persistence
---

# Session

Orchestrator between Frontend and Agent. Owns SharedStore and Agent.

## Lane Lock

Session holds a per-session `LaneLock` (`Arc<tokio::sync::Mutex<()>>`). `turn()` acquires it (blocking) for the entire duration, so background tasks (heartbeat) yield via `try_lock()`.

## turn(input, channel)

- Acquires lane lock (blocking — user always wins).
- Routes `/commands` to `handle_command()`.
- If Intelligence is configured, rebuilds `system_prompt` via `Intelligence::build_prompt()` before each turn.
- Appends user message to history, runs `Agent::run()`.
- After agent completes, checks if `total_tokens > 80% context_window` → auto-compact.

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new <label>` | Create new session |
| | `/save` | Save session to JSONL |
| | `/load <id>` | Load by label or prefix |
| | `/list` | List all sessions |
| | `/compact` | Compact history |
| Intelligence | `/prompt` | Show current system prompt |
| | `/remember <name> <txt>` | Save memory (MemoryWrite Node) |
| | `/<skill> [args]` | Invoke discovered skill |
| Scheduler | `/heartbeat` | Heartbeat status |
| | `/trigger` | Manual heartbeat trigger |

Note: `/agents`, `/switch`, `/bindings`, `/route`, `/discord`, `/gateway`, `/exit`, `/help` are handled in `repl.rs`, not in Session.

## Persistence (SessionStore)

- `sessions/` directory, one `.jsonl` file per session.
- `sessions/sessions.json` — index with metadata (label, created_at, last_active, message_count).
- Prefix matching: `/load abc` matches any session label starting with `abc`.
- `SessionStore::new(base_dir)` accepts custom directory (tests use tempdir).

## Compaction

- Keeps the most recent 20% of messages (min 4).
- Summarizes the first 50% into a single user/assistant pair via LLM (using SilentChannel).
