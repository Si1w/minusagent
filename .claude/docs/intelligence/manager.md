---
description: Agent configuration, registry, and workspace discovery
---

# AgentManager

Registry of agent configurations. Located in `src/intelligence/manager.rs`.

## AgentConfig

Per-agent configuration:

- **id** — Normalized identifier (`[a-z0-9][a-z0-9_-]{0,63}`)
- **name** — Display name
- **personality** — Personality description for prompt generation
- **system_prompt** — Explicit system prompt (from `AGENT.md` body); if empty, generated from name + personality
- **model** — Model override; empty means use global default
- **dm_scope** — Session isolation scope: `main`, `per-peer`, `per-channel-peer`, `per-account-channel-peer`
- **workspace_dir** — Per-agent workspace directory; overrides global `WORKSPACE_DIR`

## Methods

- `register(config)` — Add/overwrite an agent, normalizing its ID.
- `get(agent_id)` — Look up by ID (normalized before lookup).
- `list()` — All registered agents.
- `effective_model(agent_id)` — Per-agent model if set, otherwise global default.
- `discover_workspace(base_dir)` — Scan subdirectories for `AGENT.md` files, auto-register agents.

## Agent Discovery

Each `workspace/<name>/AGENT.md` file registers an agent:

```markdown
---
name: Mandeven
dm_scope: per-peer
model: gpt-4
---

You are Mandeven, a general-purpose AI assistant.
```

- Directory name becomes the agent ID
- Frontmatter: `name`, `personality`, `model`, `dm_scope`
- Body: used as `system_prompt` (identity for Layer 1)

## normalize_agent_id

Cleans raw strings into valid agent IDs: lowercase, replace invalid chars with hyphens, truncate to 64 chars. Falls back to `"main"` if empty.