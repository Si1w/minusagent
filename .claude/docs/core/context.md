---
description: Context and SharedStore
---

# SharedStore

Top-level container for all shared state.

```
SharedStore
├── Context      — LLM-visible state
└── SystemState  — LLM-invisible (config, intelligence, managers)
```

- Node `prep` reads `&SharedStore`.
- Node `post` writes `&mut SharedStore`.

# Context

Everything the LLM can see:
- `system_prompt` — dynamically assembled by Intelligence (7-layer prompt), or fallback default
- `history` — Vec<Message> (role: User/Assistant/Tool, content, tool_calls, tool_call_id)

# SystemState

Internal data, LLM does not see this:
- `config` — `Config` containing `LLMConfig` (model, base_url, api_key, context_window)
- `intelligence` — `Option<Intelligence>` for dynamic prompt assembly
- `todo` — `TodoManager` per-session todo list
- `is_subagent` — whether this session is a one-shot subagent
- `agents` — `SharedAgents` read-only handle to the agent registry
- `tasks` — `Option<TaskManager>` persistent task graph (workspace-level)
- `background` — `BackgroundManager` background task runner with notification queue
- `team` — `Option<TeammateManager>` multi-agent collaboration
- `team_name` — `Option<String>` this agent's team name (`None` for lead)
- `worktrees` — `Option<WorktreeManager>` git worktree isolation
- `tool_policy` — `ToolPolicy` per-session tool permission policy
- `idle_requested` — `bool` set by the `idle` tool to break out of cot_loop
