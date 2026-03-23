---
description: WebSocket JSON-RPC 2.0 gateway
---

# Gateway

WebSocket server on `ws://localhost:8765`. Allows external programs to interact with the agent system via JSON-RPC 2.0.

Started at runtime via `/gateway` CLI command.

## JSON-RPC Methods

- **send** — Send a message to an agent, get reply. Params: `text` (required), `channel`, `peer_id`, `agent_id` (optional, overrides routing).
- **bindings.set** — Add a routing binding. Params: `agent_id`, `tier`, `match_key`, `match_value`, `priority`.
- **bindings.remove** — Remove a binding. Params: `agent_id`, `match_key`, `match_value`.
- **bindings.list** — List all bindings.
- **agents.list** — List all registered agents.
- **agents.register** — Register a new agent. Params: `id`, `name`, `personality`, `system_prompt`, `model`, `dm_scope`.
- **sessions.list** — List active session keys.
- **status** — Health check: uptime, agent count, binding count, session count.

## GatewayReply

Channel implementation that buffers all output (streaming chunks + send) and returns it as the JSON-RPC response. Auto-approves bash confirmations.

## Shared State

Gateway shares `AppState` (AgentManager + BindingTable + session set) with the main loop via `Arc<RwLock<AppState>>`. Routes messages through the same mpsc channel as CLI and Discord.