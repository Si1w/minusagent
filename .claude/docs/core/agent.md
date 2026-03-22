---
description: Stateless CoT agent
---

# Agent

Stateless chain-of-thought loop. Does not own SharedStore or Channel — receives both per call.

`Agent::run(store, channel)`:

1. Call LLMCall (Node) → get streaming response.
2. If `tool_calls` present → dispatch each tool via `dispatch_tool()` → loop to 1.
3. If no `tool_calls` → `channel.send("")` to flush → break.

Returns `Option<usize>` — `total_tokens` from the last LLM call.
Session uses this to decide whether to compact history.
