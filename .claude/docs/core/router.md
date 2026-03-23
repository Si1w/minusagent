---
description: Five-tier routing and session key builder
---

# Router

Determines which agent handles a message and what session context to use.

## BindingTable

Five-tier binding table. Bindings are sorted by (tier ASC, priority DESC); first match wins.

| Tier | match_key  | Example match_value       |
|------|------------|---------------------------|
| 1    | peer_id    | `discord:admin-001`       |
| 2    | guild_id   | `guild-42`                |
| 3    | account_id | `bot-prod`                |
| 4    | channel    | `telegram`                |
| 5    | default    | `*`                       |

- `add(binding)` — Insert and re-sort.
- `remove(agent_id, match_key, match_value)` — Remove by exact match.
- `resolve_msg(channel, account_id, guild_id, peer_id)` — Walk tiers 1-5, return first match.

## build_session_key

Builds a session key from agent ID, channel metadata, and `dm_scope`:

- `main` → `agent:{id}:main`
- `per-peer` → `agent:{id}:direct:{peer}`
- `per-channel-peer` → `agent:{id}:{ch}:direct:{peer}`
- `per-account-channel-peer` → `agent:{id}:{ch}:{acc}:direct:{peer}`

## Router trait

```rust
fn resolve(&self, msg: &UserMessage) -> RouteResult { agent_id, session_key }
```

Two implementations:

- **DefaultRouter** — Single agent, `channel:sender_id` keys. Backwards-compatible.
- **BindingRouter** — Uses BindingTable + AgentManager for full routing.

## Main loop routing

`main.rs` uses shared state (`AppState`) directly instead of the Router trait. `resolve_route()` reads BindingTable and AgentManager to determine agent_id and session_key for each inbound message.