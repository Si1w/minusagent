---
description: Cron scheduler with 3 schedule types and auto-disable
---

# Cron

Background job scheduler that loads from `CRON.json` in the workspace directory. Located in `src/scheduler/cron.rs`.

## Schedule Types

- **at** — one-time execution at an ISO 8601 timestamp
- **every** — fixed interval in seconds, aligned to an anchor time
- **cron** — 7-field cron expression (sec min hour day month weekday year)

## CronJob

```json
{
  "id": "daily-check",
  "name": "Daily Check",
  "enabled": true,
  "schedule": {"kind": "cron", "expr": "0 0 9 * * * *"},
  "payload": {"kind": "agent_turn", "message": "Generate a daily summary."},
  "delete_after_run": false
}
```

## Payload Types

- **agent_turn** — runs `run_single_turn()` with the message as user input
- **system_event** — pushes static text to output queue

## Auto-Disable

5 consecutive errors → `enabled = false`. Errors and status logged to `cron-runs.jsonl`.

## CronHandle (public, clonable)

Communication via `mpsc` command channel:

- `trigger_job(id)` — manually run a specific job
- `list_jobs()` → returns `Vec<CronJobStatus>`
- `reload()` — re-read CRON.json
- `stop()` — terminate the cron task

## Lifecycle

Spawned once in `Gateway::new()` if `CRON.json` exists in workspace. Handle stored in `Gateway.cron_handle`.

## CLI Commands (repl.rs)

- `/cron` — list all jobs with status
- `/cron trigger <id>` — manually trigger a job
- `/cron reload` — reload CRON.json
