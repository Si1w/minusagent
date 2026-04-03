---
description: Session orchestrator, JSONL persistence, and 3-layer compaction
---

# Session

Orchestrator between Frontend and Agent. Owns SharedStore and Agent.

## Lane Lock

Session holds a per-session `LaneLock` (`Arc<tokio::sync::Mutex<()>>`). `turn()` acquires it (blocking) for the entire duration, so background tasks (heartbeat) yield via `try_lock()`.

## turn(input, channel)

- Acquires lane lock (blocking — user always wins).
- Routes `/commands` to `handle_command()`.
- If Intelligence is configured, rebuilds `system_prompt` via `Intelligence::build_prompt()` before each turn.
- Appends user message to history, runs agent via `ResilienceRunner::run()`.
- After agent completes, runs `turn_end()` for compaction checks.

## 3-Layer Compaction Cascade

After each agent turn, `turn_end()` checks `total_tokens > compact_threshold * context_window` and runs a 3-layer cascade:

### L1: MicroCompact (free, no API call)

Replaces old tool-result content with a placeholder marker (`CLEARED_TOOL_MARKER`). Keeps the most recent 20% of history (min 4 messages) intact.

### L2: AutoCompact (LLM summarization)

Summarizes the oldest 50% of history into a single user/assistant pair. Budget controlled by `compact_summary_ratio`. Circuit breaker: after `compact_max_failures` consecutive failures, L2 is skipped.

### L3: Full Compact (LLM summarization + re-injection)

Summarizes the entire history, then re-injects:
- Recently read file paths (from `read_file_state`)
- Active todo items

Budget controlled by `full_compact_summary_ratio`.

### Cascade flow

```
tokens > threshold?
  → L1 micro_compact (clear old tool results)
  → re-estimate tokens
  → still over? → L2 auto_compact (summarize older half)
  → re-estimate tokens
  → still over? → L3 full_compact (summarize all + re-inject context)
```

The `/compact` command runs L1 + L3 directly (skipping L2).

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new <label>` | Create new session |
| | `/save` | Save session to JSONL |
| | `/load <id>` | Load by label or prefix |
| | `/list` | List all sessions |
| | `/compact` | Compact history (L1 + L3) |
| Intelligence | `/prompt` | Show current system prompt |
| | `/remember <name> <txt>` | Save memory (MemoryWrite Node) |
| | `/<skill> [args]` | Invoke discovered skill |
| Team | `/team` | Show team roster and status |
| | `/inbox` | Check lead inbox |
| | `/tasks` | Show task board with owners |
| | `/worktrees` | List worktrees |
| | `/events` | Worktree event log |
| Scheduler | `/heartbeat` | Heartbeat status |
| | `/trigger` | Manual heartbeat trigger |
| | `/cron` | List cron jobs |
| | `/cron trigger <id>` | Trigger a cron job |
| | `/cron reload` | Reload CRON.json |

Note: `/agents`, `/switch`, `/bindings`, `/route`, `/discord`, `/gateway`, `/exit`, `/help` are handled in `repl.rs`, not in Session.

## Persistence (SessionStore)

- `sessions/` directory, one `.jsonl` file per session.
- `sessions/sessions.json` — index with metadata (label, created_at, last_active, message_count).
- Prefix matching: `/load abc` matches any session label starting with `abc`.
- `SessionStore::new(base_dir)` accepts custom directory (tests use tempdir).
