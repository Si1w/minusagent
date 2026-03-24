# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Architecture

```
Frontend (CLI TUI / Discord / WebSocket Gateway)
       ↓
    UserMessage
       ↓
main.rs: routing layer (AppState + BindingTable → agent_id + session_key)
       ↓
Session (per session_key, built from AgentConfig)
├── Persistence: JSONL + index
├── Commands: /new /save /load /list /compact /remember /help /exit
├── Skill invocation: /<skill> routes to discovered SKILL.md body
└── Agent CoT loop
       ↓
  Agent.run()
       ├── LLMCall (Node) — streaming OpenAI-compatible API
       └── dispatch_tool()
              ├── BashExec (Node)
              ├── ReadFile (Node)
              ├── WriteFile (Node)
              └── EditFile (Node)

  /remember → MemoryWrite (Node) — LLM generates TLDR, writes .md, hot-updates index
```

## Module Structure

```
src/
├── core/           node, agent, llm, session, store, tool
├── intelligence/   manager, bootstrap, skills, memory, prompt, utils
├── routing/        router (BindingTable, build_session_key)
├── frontend/       cli, discord, gateway
└── main.rs         entry point, routing, session spawn
```

- **Frontend**: CLI (ratatui TUI) + Discord + WebSocket Gateway. Swappable via `Channel` trait.
- **Router**: BindingTable (5-tier: peer > guild > account > channel > default) resolves agent_id + session_key. Persisted via `bindings.json`.
- **AgentManager**: Registry of AgentConfig. Auto-discovers agents from `workspace/` subdirectories via `AGENT.md`.
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel, http)` calls LLM → dispatch tools → repeat.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible + optional Intelligence).
- **Intelligence**: Dynamic 8-layer system prompt assembly. Enabled per-agent via `workspace_dir`.

## Intelligence

System prompt is rebuilt each turn from 8 layers:

1. Identity (`AGENT.md` body, or `prompts/system.md` fallback)
2. Personality (workspace `SOUL.md`)
3. Tool guidelines (workspace `TOOLS.md`)
4. Skills (`prompts/skills.md` template + discovered `SKILL.md` files)
5. Memory (`prompts/memory.md` template + TLDR index from `memory/*.md`)
6. Bootstrap context (workspace `HEARTBEAT.md`, `BOOTSTRAP.md`, `AGENTS.md`, `USER.md`)
7. Runtime context (agent_id, model, channel, time)
8. Channel hints

Memory uses progressive loading: only TLDRs at startup, LLM uses `read_file` for full content.

## Agent Discovery

`workspace/` subdirectories with `AGENT.md` are auto-registered at startup:

```
workspace/
├── mandeven/
│   ├── AGENT.md      frontmatter (name, dm_scope, model) + body (identity)
│   ├── SOUL.md       personality
│   └── memory/       progressive memory files
```

## Routing

`bindings.json` maps channels/peers to agents. Managed via `/bind` command or Gateway JSON-RPC.

## CLI Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new <label>` | New session |
| | `/save` | Save session |
| | `/load <id>` | Load session |
| | `/list` | List sessions |
| | `/compact` | Compact history |
| Intelligence | `/remember <name> <txt>` | Save memory (LLM generates TLDR) |
| | `/<skill> [args]` | Invoke discovered skill |
| Agents | `/agents` | List registered agents |
| | `/switch <agent>` | Route to specific agent |
| | `/switch off` | Restore default routing |
| | `/bind` | List bindings |
| | `/bind <ch> <agent>` | Bind channel to agent |
| | `/bind rm <ch>` | Remove binding |
| Gateways | `/discord` | Start Discord bot |
| | `/gateway` | Start WebSocket gateway |
| | `/help` | Show commands |
| | `/exit` | Exit |

## LLM

Global provider config: `base_url`, `api_key`, `context_window`. Per-agent override: `model`. No per-provider backends.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.

## Shared State

`AppState` (AgentManager + BindingTable + session set) wrapped in `Arc<RwLock>`. Shared between main loop and Gateway. Gateway can register agents and modify bindings at runtime via JSON-RPC.