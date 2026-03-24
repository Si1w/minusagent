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
- `remove_by_key(match_key, match_value)` — Remove all bindings matching key-value (any agent).
- `resolve_msg(channel, account_id, guild_id, peer_id)` — Walk tiers 1-5, return first match.
- `load_file(path)` — Load bindings from a JSON array file.
- `save_file(path)` — Persist all bindings to a JSON file.
- `list()` — Return all bindings in match order.

## Binding Persistence

`routes.json` at project root. Loaded at startup, saved on every `/route` change.

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

## CLI commands

- `/route` — List all bindings.
- `/route <channel> <agent>` — Add tier-4 channel binding (persisted).
- `/route rm <channel>` — Remove channel binding (persisted).
- `/switch <agent>` — Override routing for current CLI session.
- `/switch off` — Restore default routing.

## Main loop routing

`main.rs` uses `AppState.resolve_route()` to determine agent_id and session_key. CLI `/switch` override takes precedence over BindingTable when set.