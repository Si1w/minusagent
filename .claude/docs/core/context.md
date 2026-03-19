---
description: Context and SharedStore
---

# SharedStore

Top-level container for all shared state.

```
SharedStore
├── Context      — LLM-visible state
└── SystemState  — LLM-invisible (config, inter-Node data)
```

- Node `prep` reads `&SharedStore`.
- Node `post` writes `&mut SharedStore`.

# Context

Everything the LLM can see:
- System prompt
- Conversation history (user, assistant, observation)

# SystemState

Internal data, LLM does not see this:
- Config (model, base_url, api_key)
- Inter-Node communication data
