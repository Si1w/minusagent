# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI TUI / Discord)
       ↓
main.rs: routing layer (mpsc channel, per-session tasks)
       ↓
Session (per user/channel)
├── Persistence: JSONL + index
├── Commands: /new, /save, /load, /list, /compact, /discord, /exit
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

- **Frontend**: CLI (ratatui TUI) + Discord (gateway WebSocket). Swappable via `Channel` trait.
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel)` calls LLM → dispatch tools → repeat.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible).

## LLM

Generic config: `model`, `base_url`, `api_key`, `context_window`. No per-provider backends.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.
