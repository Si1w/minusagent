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

`thought` + `action` + `input`, all strings.

```json
{ "thought": "...", "action": "Bash", "input": "ls -la" }
{ "thought": "...", "action": "UseSkill", "input": "search" }
{ "thought": "...", "action": "Answer", "input": "the answer" }
```

### Action

- `Bash` — execute shell command, `input` is the command.
- `UseSkill` — load a skill, `input` is the skill name.
- `Answer` — final response to user, `input` is the answer.

### Observation

Set by the environment after action execution, not by LLM.
