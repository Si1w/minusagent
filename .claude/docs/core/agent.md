---
description: Agent orchestrator
---

# Agent

Orchestrator. Owns SharedStore and Channel. Exposes `turn(input)` to frontend.

1. Write user input to Context.
2. Run LLMCall (Node) → get response.
3. If answer (no tool_calls) → channel.send(), end.
4. If tool_calls → channel.confirm().
   - Approved → execute tool → write observation to Context → go to 2.
   - Denied → write denial to Context → go to 2.
