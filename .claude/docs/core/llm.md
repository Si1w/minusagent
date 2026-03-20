---
description: LLM integration
---

# LLM

Generic config, no per-provider backends:

```
model: string
base_url: string
api_key: string
```

## Response Format

Native function calling. No custom JSON.

- `content` — thought or final answer.
- `tool_calls` — action (Bash or Skill).

### Flow

- `tool_calls` present → action, `content` is thought.
- `content` only, no `tool_calls` → final answer.

### Observation

`tool` role with `tool_call_id`, set by the environment.

### Tools

- `bash` — execute shell command.
- `skill` — load and use a skill by name.
