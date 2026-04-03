---
description: WebSocket JSON-RPC 2.0 gateway and central dispatcher
---

# Gateway

Central message dispatcher. Also runs a WebSocket server on `ws://localhost:8765` for JSON-RPC 2.0 access.

## ProviderConfig

Global LLM provider configuration loaded from env vars:
- `LLM_MODEL`, `LLM_BASE_URL`, `LLM_API_KEY`, `LLM_CONTEXT_WINDOW`, `WORKSPACE_DIR`
- `build_store()` creates a SharedStore with LLM config and optional Intelligence.

## Gateway::dispatch()

Core dispatch logic:
1. Resolve agent via BindingRouter (or explicit override).
2. Track session key in active sessions set.
3. If new session: build Intelligence + SharedStore, spawn dedicated tokio task.
4. Send message through session's mpsc channel.
5. Return DispatchResult with completion receiver.

## JSON-RPC Methods

- **send** — Dispatch message, get reply. Params: `text` (required), `channel`, `peer_id`, `account_id`, `guild_id`, `agent_id` (optional override).
- **bindings.set** — Add a routing binding. Params: `agent_id`, `tier`, `match_key`, `match_value`, `priority`.
- **bindings.remove** — Remove a binding. Params: `agent_id`, `match_key`, `match_value`.
- **bindings.list** — List all bindings.
- **agents.list** — List all registered agents (id, name, model, dm_scope).
- **agents.register** — Register a new agent. Params: `id` (required), `name` (required), `system_prompt`, `model`, `dm_scope`, `workspace_dir`.
- **sessions.list** — List active session keys.
- **status** — Health check: uptime, agent count, binding count, session count.

## AppState

Shared between REPL and Gateway via `Arc<RwLock<AppState>>`:
- `router: BindingRouter` — routes messages to agents
- `sessions: HashSet<String>` — active session keys
- `start_time: Instant` — startup timestamp

## GatewayReply

Channel implementation that buffers all output and returns it as JSON-RPC response. Auto-approves bash confirmations.
