---
description: Context and SharedStore
---

# SharedStore

Top-level container for all shared state.

```
SharedStore
├── Context      — LLM-visible state
└── SystemState  — LLM-invisible (config, intelligence)
```

- Node `prep` reads `&SharedStore`.
- Node `post` writes `&mut SharedStore`.

# Context

Everything the LLM can see:
- `system_prompt` — dynamically assembled by Intelligence (7-layer prompt), or fallback default
- `history` — Vec<Message> (role: User/Assistant/Tool, content, tool_calls, tool_call_id)

# SystemState

Internal data, LLM does not see this:
- `Config` containing `LLMConfig` (model, base_url, api_key, context_window)
- `Option<Intelligence>` — when `WORKSPACE_DIR` is set, enables dynamic prompt assembly with bootstrap files, skills, and memory
