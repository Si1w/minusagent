---
description: Agent configuration, registry, and workspace discovery
---

# AgentManager

Registry of agent configurations. Located in `src/intelligence/manager.rs`.

## AgentConfig

Per-agent configuration:

- **id** — Normalized identifier (`[a-z0-9][a-z0-9_-]{0,63}`)
- **name** — Display name
- **system_prompt** — Identity text (from `AGENT.md` body)
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

Each subdirectory under `.agents/` containing `AGENT.md` is auto-registered:

- Directory name becomes agent ID (normalized)
- If `AGENT.md` has frontmatter, body (after `---`) is used as `system_prompt`
- If no frontmatter, entire file content is used as `system_prompt`
- `dm_scope` defaults to `per-peer` for discovered agents

## normalize_agent_id

Cleans raw strings into valid agent IDs: lowercase, replace invalid chars with hyphens, truncate to 64 chars. Falls back to `"mandeven"` if empty.
