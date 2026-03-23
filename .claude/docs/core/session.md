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

- `/new <label>` — create a new session.
- `/save` — save current session to JSONL.
- `/load <id>` — load session by ID or unique prefix.
- `/list` — list all sessions sorted by last active.
- `/compact` — manually compact history.

## Persistence (SessionStore)

- `sessions/` directory, one `.jsonl` file per session.
- `sessions/sessions.json` — index with metadata (label, created_at, last_active, message_count).
- Prefix matching: `/load abc` matches any session ID starting with `abc`.

## Compaction

- Keeps the most recent 20% of messages (min 4).
- Summarizes the first 50% into a single user/assistant pair via LLM (using SilentChannel).