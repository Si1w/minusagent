---
description: Agent configuration and registry
---

# AgentManager

Registry of agent configurations. Each agent has its own identity, personality, model, and session scope.

## AgentConfig

Per-agent configuration:

- **id** — Normalized identifier (`[a-z0-9][a-z0-9_-]{0,63}`)
- **name** — Display name
- **personality** — Personality description for prompt generation
- **system_prompt** — Explicit system prompt; if empty, generated from name + personality
- **model** — Model override; empty means use global default
- **dm_scope** — Session isolation scope: `main`, `per-peer`, `per-channel-peer`, `per-account-channel-peer`

`effective_system_prompt()` returns the explicit prompt if set, otherwise generates from name + personality.

## AgentManager

- `register(config)` — Add/overwrite an agent, normalizing its ID.
- `get(agent_id)` — Look up by ID (normalized before lookup).
- `list()` — All registered agents.
- `effective_model(agent_id)` — Per-agent model if set, otherwise global default.

## normalize_agent_id

Cleans raw strings into valid agent IDs: lowercase, replace invalid chars with hyphens, truncate to 64 chars. Falls back to `"main"` if empty.