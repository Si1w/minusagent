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
├── Commands: /new, /save, /load, /list, /compact, /discord, /gateway, /exit
└── Agent CoT loop
       ↓
  Agent.run()
       ├── LLMCall (Node) — streaming OpenAI-compatible API
       └── dispatch_tool()
              ├── BashExec (Node)
              ├── ReadFile (Node)
              ├── WriteFile (Node)
              └── EditFile (Node)
```

- **Frontend**: CLI (ratatui TUI) + Discord + WebSocket Gateway. Swappable via `Channel` trait.
- **Router**: BindingTable (5-tier: peer > guild > account > channel > default) resolves agent_id + session_key.
- **AgentManager**: Registry of AgentConfig (personality, model override, dm_scope).
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel, http)` calls LLM → dispatch tools → repeat.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible).

## LLM

Global provider config: `base_url`, `api_key`, `context_window`. Per-agent override: `model`. No per-provider backends.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.

## Shared State

`AppState` (AgentManager + BindingTable + session set) wrapped in `Arc<RwLock>`. Shared between main loop and Gateway. Gateway can register agents and modify bindings at runtime via JSON-RPC.