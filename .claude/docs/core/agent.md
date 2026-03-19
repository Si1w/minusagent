---
description: Agent orchestrator
---

# Agent

Orchestrator, not a Node. Holds `&mut SharedStore`, drives the CoT loop:

1. Run CoT step (Node) → get thought + action + input.
2. If action == Answer → return result, end.
3. If action == Bash/UseSkill → dispatch to executor.
4. Executor writes observation to Context.
5. Go to 1.

CoT step only reasons. Action execution is external.
