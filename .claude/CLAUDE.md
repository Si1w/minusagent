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
├── routing/        router (Router trait, BindingRouter, BindingTable)
├── frontend/       cli, discord, gateway
└── main.rs         entry point, session spawn
```

- **Frontend**: CLI (ratatui TUI) + Discord + WebSocket Gateway. Swappable via `Channel` trait.
- **Router**: `Router` trait (`resolve`, `resolve_explicit`). `BindingRouter` implements it with a 5-tier BindingTable (peer > guild > account > channel > default) + AgentManager. Default agent: `mandeven`.
- **AgentManager**: Registry of AgentConfig. Auto-discovers agents from `WORKSPACE_DIR/.agents/` via `AGENT.md`.
- **Session**: Orchestrator. Owns SharedStore. `turn(input)` routes commands or runs Agent.
- **Agent**: Stateless CoT loop. `run(store, channel, http)` calls LLM → dispatch tools → repeat.
- **Node**: Universal building block. `prep(store) → exec() → post(store)`.
- **SharedStore**: Context (LLM-visible) + SystemState (LLM-invisible + optional Intelligence).
- **Intelligence**: Dynamic 7-layer system prompt assembly. Enabled per-agent via `workspace_dir`.

## Intelligence

System prompt is rebuilt each turn from 7 layers:

1. Identity (`AGENT.md` — plain markdown, no frontmatter)
2. Tool guidelines (workspace `TOOLS.md`)
3. Skills (name + description summary from discovered `SKILL.md` files)
4. Memory (TLDR index from `memory/*.md`)
5. Bootstrap context (workspace `HEARTBEAT.md`, `BOOTSTRAP.md`, `AGENTS.md`, `USER.md`)
6. Runtime context (agent_id, model, channel, time)
7. Channel hints

Progressive loading:
- **Skills**: only name + description at startup; full body loaded from file on activation (`/<skill>`)
- **Memory**: only TLDRs at startup; LLM uses `read_file` for full content

## Agent Discovery

`WORKSPACE_DIR/.agents/` subdirectories with `AGENT.md` are auto-registered at startup.
Directory name = agent ID. Entire file content = identity (system prompt).

```
WORKSPACE_DIR/
└── .agents/
    └── mandeven/
        ├── AGENT.md      plain markdown identity (no frontmatter)
        ├── TOOLS.md      tool usage guidelines (optional)
        ├── memory/       progressive memory files
        └── skills/       discovered SKILL.md directories
```

## Routing

`BindingRouter` wraps `BindingTable` + `AgentManager`. Routes via 5-tier binding resolution, falls back to default agent (`mandeven`).

`routes.json` persists bindings. Managed via `/route` command or Gateway JSON-RPC.

## CLI Commands

| Category | Command | Description |
|----------|---------|-------------|
| Sessions | `/new <label>` | New session |
| | `/save` | Save session |
| | `/load <id>` | Load session |
| | `/list` | List sessions |
| | `/compact` | Compact history |
| Intelligence | `/prompt` | Show system prompt |
| | `/remember <name> <txt>` | Save memory (LLM generates TLDR) |
| | `/<skill> [args]` | Invoke discovered skill |
| Agents | `/agents` | List registered agents |
| | `/switch <agent>` | Route to specific agent |
| | `/switch off` | Restore default routing |
| | `/route` | List bindings |
| | `/route <ch> <agent>` | Bind channel to agent |
| | `/route rm <ch>` | Remove binding |
| Gateways | `/discord` | Start Discord bot |
| | `/gateway` | Start WebSocket gateway |
| | `/help` | Show commands |
| | `/exit` | Exit |

## LLM

Global provider config: `base_url`, `api_key`, `context_window`. Per-agent override: `model` (via runtime config). No per-provider backends.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.

## Shared State

`AppState` (`BindingRouter` + session set) wrapped in `Arc<RwLock>`. Shared between main loop and Gateway. Gateway can register agents and modify bindings at runtime via JSON-RPC.