---
description: Per-session heartbeat with lane priority and background output
---

# Heartbeat

Background scheduler that periodically runs an isolated agent turn using HEARTBEAT.md as instruction. Located in `src/scheduler/heartbeat.rs`.

## Lane Priority

User turns always win. Heartbeat yields via non-blocking `try_lock()`:

- `Session::turn()` → `lane_lock.lock().await` (blocking, user always enters)
- `HeartbeatRunner::execute()` → `lane_lock.try_lock()` (non-blocking, skips if busy)

Lane lock is `Arc<tokio::sync::Mutex<()>>`, created per-session in `Gateway::dispatch()`.

## HeartbeatRunner (internal, per-session)

Polls every 1s. Runs when all 4 preconditions pass:

1. `HEARTBEAT.md` exists and is non-empty
2. Interval elapsed (default 1800s)
3. Within active hours (default 9:00-22:00)
4. Not already running

Builds prompt from agent identity + memory TLDRs + current time. Parses response: `HEARTBEAT_OK` → suppress; duplicate → suppress; meaningful → push to bg_output.

## HeartbeatHandle (public, clonable)

Communication via `mpsc` command channel:

- `trigger()` → manual heartbeat, bypassing interval check
- `status()` → returns `HeartbeatStatus` snapshot
- `stop()` → terminate the heartbeat task

## Lifecycle

Spawned in `Gateway::dispatch()` when creating a new session whose workspace has `HEARTBEAT.md`. Handle stored in `Gateway.heartbeat_handles`.

## CLI Commands (session.rs)

- `/heartbeat` — show status for all active heartbeat runners
- `/trigger` — manually trigger all heartbeat runners