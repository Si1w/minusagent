---
description: LLM integration (OpenAI-compatible streaming)
---

# LLM

Generic config, no per-provider backends:

```
model, base_url, api_key, context_window
```

## LLMCall (Node)

- `prep`: builds request from system_prompt + history + tool definitions.
- `exec`: streams SSE response, aggregates content and tool_calls.
- `post`: appends assistant message to history.

## Streaming

Content chunks are sent to `channel.on_stream_chunk()` in real-time.
Only non-empty content is streamed.

## Response

- `content` — LLM text output.
- `tool_calls` — function calls (bash, read_file, write_file, edit_file).
- `usage` — prompt_tokens, completion_tokens, total_tokens.

## Flow

- `tool_calls` present → agent dispatches tools, loops back.
- No `tool_calls` → final answer, agent breaks.
