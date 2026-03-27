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
async fn flush()                            // finalize buffered stream content
```

## UserMessage

Inbound message metadata used for both display and routing:

- **text** — Message content
- **sender_id** — User identifier
- **channel** — Channel type (e.g. "cli", "discord")
- **account_id** — Bot account identifier (empty for CLI)
- **guild_id** — Server/guild identifier (empty for non-guild channels)

## CLI (ratatui TUI)

- Output area (top): scrollable, shows streaming output and log entries.
- Input area (bottom): fixed, cyan border when waiting, gray when processing.
- CJK-aware cursor positioning via `unicode-width`.
- `cleanup_terminal()` restores terminal on exit/panic.

## Discord

- Gateway WebSocket connection with heartbeat, resume, and reconnection.
- Buffers streaming chunks, flushes on `flush()`.
- Real y/n confirmation via `PendingConfirms` (shared oneshot map between gateway and DiscordReply).
- Chunks messages at 2000 chars.

## WebSocket Gateway

- JSON-RPC 2.0 server on `ws://localhost:8765`.
- GatewayReply buffers all output, returns as JSON-RPC response.
- Auto-approves bash confirmations.
- See `frontend/gateway.md` for method details.

## SilentChannel

No-op implementation used internally for compaction LLM calls.

## REPL (repl.rs)

- CLI always starts. `/discord` and `/gateway` spawn at runtime.
- Messages dispatched through Gateway (BindingRouter + AgentManager).
- Session key built from agent's `dm_scope` (e.g. `agent:luna:direct:user1`).
- Each session key gets a dedicated tokio task (concurrent across sessions).
- CLI waits for session completion via oneshot before showing next prompt.
- Routing-level commands (`/agents`, `/switch`, `/bindings`, `/route`, `/discord`, `/gateway`, `/exit`, `/help`) handled in REPL, not Session.
