---
description: Channel trait and frontends
---

# Channel

Async trait for frontend communication. All frontends implement this.

```rust
async fn receive() -> Option<UserMessage>   // get user input
async fn send(text)                         // send output
async fn confirm(command) -> bool           // bash confirmation (y/n)
async fn on_stream_chunk(chunk)             // streaming LLM output
```

## CLI (ratatui TUI)

- Output area (top): scrollable, shows streaming output and log entries.
- Input area (bottom): fixed, cyan border when waiting, gray when processing.
- CJK-aware cursor positioning via `unicode-width`.
- `cleanup_terminal()` restores terminal on exit/panic.

## Discord

- Gateway WebSocket connection with heartbeat.
- Buffers streaming chunks, flushes on `send()`.
- Real y/n confirmation via `PendingConfirms` (shared oneshot map between gateway and DiscordReply).
- Chunks messages at 2000 chars.

## SilentChannel

No-op implementation used internally for compaction LLM calls.

## Routing (main.rs)

- CLI always starts. `/discord` spawns the gateway at runtime.
- Messages routed by `session_key` (e.g. `cli:cli-user`, `discord:{user_id}`).
- Each session key gets a dedicated tokio task (concurrent across sessions).
- CLI waits for session completion via oneshot before showing next prompt.
