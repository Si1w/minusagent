# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI TUI / Discord / WebSocket Gateway)
       ↓
    UserMessage
       ↓
main.rs: routing layer (AppState + BindingTable → agent_id + session_key)
       ↓
Session (per session_key, built from AgentConfig)
├── Persistence: JSONL + index
├── Commands: /new, /save, /load, /list, /compact, /remember, /discord, /gateway, /exit
├── Skill invocation: /<skill> routes to discovered SKILL.md body
└── Agent CoT loop
       ↓
  Agent.run()
       ├── LLMCall (Node) — streaming OpenAI-compatible API
       └── dispatch_tool()
              ├── BashExec (Node)
              ├── ReadFile (Node)
              ├── WriteFile (Node)
              └── EditFile (Node)

  /remember → MemoryWrite (Node) — LLM generates TLDR, writes .md, hot-updates index
```

- **Frontend**: CLI (ratatui TUI) + Discord + WebSocket Gateway. Swappable via `Channel` trait.
- **Router**: BindingTable (5-tier: peer > guild > account > channel > default) resolves agent_id + session_key.
- **AgentManager**: Registry of AgentConfig (personality, model override, dm_scope).
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel, http)` calls LLM → dispatch tools → repeat.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible + optional Intelligence).
- **Intelligence**: Dynamic 8-layer system prompt assembly. Enabled by `WORKSPACE_DIR` env var.

## Intelligence

When `WORKSPACE_DIR` is set, system prompt is rebuilt each turn from 8 layers:

1. Identity (`prompts/system.md` fallback, or workspace `IDENTITY.md`)
2. Personality (workspace `SOUL.md`)
3. Tool guidelines (workspace `TOOLS.md`)
4. Skills (`prompts/skills.md` template + discovered `SKILL.md` files)
5. Memory (`prompts/memory.md` template + TLDR index from `memory/*.md`)
6. Bootstrap context (workspace `HEARTBEAT.md`, `BOOTSTRAP.md`, `AGENTS.md`, `USER.md`)
7. Runtime context (agent_id, model, channel, time)
8. Channel hints

Memory uses progressive loading: only TLDRs at startup, LLM uses `read_file` for full content.

## LLM

Global provider config: `base_url`, `api_key`, `context_window`. Per-agent override: `model`. No per-provider backends.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.

## Shared State

`AppState` (AgentManager + BindingTable + session set) wrapped in `Arc<RwLock>`. Shared between main loop and Gateway. Gateway can register agents and modify bindings at runtime via JSON-RPC.