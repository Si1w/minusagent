---
description: Five-tier routing, binding persistence, and session key builder
---

# Router

Determines which agent handles a message and what session context to use. Located in `src/routing/router.rs`.

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
- `remove(agent_id, match_key, match_value)` — Remove by exact triple match.
- `resolve_msg(channel, account_id, guild_id, peer_id)` — Walk tiers 1-5, return first match.
- `load_file(path)` — Load bindings from a JSON array file.
- `list()` — Return all bindings in match order.

## Binding Persistence

`routes.json` at project root. Loaded at startup via `load_file()`.

```json
[
  { "agent_id": "mandeven", "tier": 4, "match_key": "channel", "match_value": "discord", "priority": 0 }
]
```

## build_session_key

Builds a session key from agent ID, channel metadata, and `dm_scope`:

- `main` → `agent:{id}:main`
- `per-peer` → `agent:{id}:direct:{peer}`
- `per-channel-peer` → `agent:{id}:{ch}:direct:{peer}`
- `per-account-channel-peer` → `agent:{id}:{ch}:{acc}:direct:{peer}`

If `peer_id` is empty, always falls back to `agent:{id}:main`.

## BindingRouter

Wraps `BindingTable` + `AgentManager`. Implements `Router` trait:
- `resolve(msg)` — Look up binding, fallback to default agent (`mandeven`), build result.
- `resolve_explicit(agent_id, msg)` — Override with explicit agent ID, normalize, build result.

## CLI commands (handled in repl.rs)

- `/bindings` — List all bindings.
- `/route <ch> <peer> [acc] [guild]` — Test route resolution.
- `/switch <agent>` — Override routing for current CLI session.
- `/switch off` — Restore default routing.
