# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI, ...) → Session → Agent → Node → SharedStore
```

- **Frontend**: Swappable. CLI for MVP.
- **Session**: Orchestrator. Owns SharedStore, manages user turns.
- **Agent**: Orchestrator. Drives CoT loop (think → act → observe).
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible).

## LLM

Generic config: `model`, `base_url`, `api_key`. No per-provider backends.
