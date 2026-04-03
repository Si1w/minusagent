# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI TUI / Discord / WebSocket Gateway)
       ↓
    UserMessage
       ↓
BindingRouter (BindingTable → agent_id → build_session_key)
       ↓
Session (per session_key, built from AgentConfig)
├── Persistence: JSONL + index
├── Commands: /new /save /load /list /compact /prompt /remember /help /exit
├── Skill activation: /<skill> loads SKILL.md body on demand
├── 3-layer compaction: L1 micro → L2 auto → L3 full
└── Agent CoT loop (wrapped by ResilienceRunner)
       ↓
  ResilienceRunner
       ├── Layer 1: Auth rotation (ProfileManager)
       ├── Layer 2: Overflow recovery (compact)
       └── Layer 3: Agent.run()
              ├── LLMCall (Node) — streaming OpenAI-compatible API
              └── dispatch_tool() — 23+ tools
```

## Module Structure

```
src/
├── core/           node, agent, llm, session, store
├── intelligence/   manager, bootstrap, skills, memory, prompt, utils
├── routing/        router, protocol, delivery
├── scheduler/      heartbeat, cron, lane
├── frontend/       cli, repl, stdio, discord, gateway, utils
├── team/           manager, task, todo, worktree
├── resilience/     classify, profile, runner
├── tool/           mod (dispatch), schema, exec, search, web
├── config.rs       AppConfig, LLMConfig, Tuning (global OnceLock)
├── logger.rs       TUI logger
└── main.rs         entry point
```

## Documentation

Detailed per-module documentation lives in `docs/`. **When implementing features, fixing bugs, or refactoring any module, read the relevant doc first:**

```
docs/
├── core/
│   ├── agent.md          CoT loop, subagent runner
│   ├── context.md        SharedStore, Context, SystemState
│   ├── llm.md            LLM streaming, request/response
│   ├── logger.md         TUI logger
│   ├── node.md           Node abstraction (prep → exec → post)
│   ├── session.md        Session orchestrator, 3-layer compaction
│   └── tool.md           All 23+ tools, dispatch, schemas, permissions
├── frontend/
│   ├── channel.md        Channel trait, CLI, Discord, WebSocket
│   └── gateway.md        JSON-RPC 2.0 gateway, AppState
├── intelligence/
│   ├── intelligence.md   7-layer prompt assembly, progressive loading
│   └── manager.md        AgentConfig, AgentManager, workspace discovery
├── routing/
│   ├── protocol.md       ToolPolicy, PermissionMode, per-tool overrides
│   └── router.md         5-tier routing, binding persistence, session keys
├── scheduler/
│   ├── cron.md           Cron scheduler, 3 schedule types, auto-disable
│   └── heartbeat.md      Per-session heartbeat, lane priority
├── team/
│   └── team.md           TeammateManager, MessageBus, protocols, worktrees, tasks
├── resilience/
│   └── resilience.md     3-layer resilience runner, auth rotation, failover
└── config/
    └── tuning.md         All tunable parameters with defaults
```

## Quick Reference

- **Node**: `prep(store) → exec() → post(store)`. Universal building block.
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel, http)` → LLM → dispatch tools → repeat.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible).
- **Intelligence**: Dynamic 7-layer system prompt assembly. Enabled per-agent via `workspace_dir`.
- **Resilience**: Auth rotation → overflow recovery → agent loop. ProfileManager + fallback models.
- **Team**: Persistent teammates with JSONL inboxes, idle polling, autonomous task claiming.
- **Scheduler**: Heartbeat (per-session) + Cron (global). Lane lock ensures user priority.
- **Routing**: 5-tier BindingTable (peer > guild > account > channel > default). Default agent: `mandeven`.

## CLI Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new` `/save` `/load` `/list` `/compact` | Session management |
| Intelligence | `/prompt` `/remember` `/<skill>` | Prompt, memory, skills |
| Agents | `/agents` `/switch` `/route` | Agent routing |
| Team | `/team` `/inbox` `/tasks` `/worktrees` `/events` | Multi-agent |
| Scheduler | `/heartbeat` `/trigger` `/cron` | Background tasks |
| Gateways | `/discord` `/gateway` | Start services |

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
Lane lock ensures user turns always win over heartbeat.
