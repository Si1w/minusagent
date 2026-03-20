# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI, ...) → Agent → Node → SharedStore
```

- **Frontend**: Swappable. CLI for MVP. Holds Agent, drives REPL loop.
- **Agent**: Orchestrator. Owns SharedStore + Channel. `turn(input)` runs CoT loop.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible).

## LLM

Generic config: `model`, `base_url`, `api_key`. No per-provider backends.
