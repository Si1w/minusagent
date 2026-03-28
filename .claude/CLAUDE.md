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
├── core/           node, agent, llm, session, store, tool, team, worktree
├── intelligence/   manager, bootstrap, skills, memory, prompt, utils
├── routing/        router (Router trait, BindingRouter, BindingTable)
├── scheduler/      heartbeat, cron (background tasks with lane priority)
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
- **Scheduler**: Heartbeat (per-session, lane-priority yield) + Cron (global, 3 schedule types). Background output via global buffer, drained in TUI event loop.
- **Team**: `TeammateManager` + `MessageBus` for multi-agent collaboration. Persistent teammates with JSONL inboxes, wake-on-message, and lifecycle management (working → idle → working → shutdown).

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
| Team | `/team` | Show team roster and status |
| | `/inbox` | Check lead inbox |
| | `/tasks` | Show task board with owners |
| | `/worktrees` | List worktrees |
| | `/events` | Worktree event log |
| Scheduler | `/heartbeat` | Heartbeat status |
| | `/trigger` | Manual heartbeat |
| | `/cron` | List cron jobs |
| | `/cron trigger <id>` | Trigger a cron job |
| | `/cron reload` | Reload CRON.json |
| Gateways | `/discord` | Start Discord bot |
| | `/gateway` | Start WebSocket gateway |
| | `/help` | Show commands |
| | `/exit` | Exit |

## LLM

Global provider config: `base_url`, `api_key`, `context_window`. Per-agent override: `model` (via runtime config). No per-provider backends.

## Scheduler

Per-session lane lock (`Arc<tokio::sync::Mutex<()>>`) ensures user turns always win:
- `Session::turn()` → `lock().await` (blocking)
- `HeartbeatRunner::execute()` → `try_lock()` (non-blocking, yields if user active)

**Heartbeat**: Per-session. Spawned in `Gateway::dispatch()` when `HEARTBEAT.md` exists. Polls every 1s, runs when interval (1800s), active hours (9-22), and preconditions met. `HEARTBEAT_OK` suppresses output.

**Cron**: Global. Spawned in `Gateway::new()` from `CRON.json`. Three schedule types: `at`, `every`, `cron`. Auto-disables after 5 consecutive errors. Run log: `cron-runs.jsonl`.

**Output**: Background results pushed to global buffer (`scheduler::push_bg_output`), drained by TUI event loop every 50ms frame.

## Agent Teams

Persistent multi-agent collaboration via JSONL inboxes. Located in `WORKSPACE_DIR/.team/`.

```
.team/
  config.json           <- team roster + statuses
  inbox/
    alice.jsonl         <- append-only, drain-on-read
    bob.jsonl
    lead.jsonl
```

**TeammateManager**: Manages roster (`config.json`), spawns teammate tokio tasks, handles wake-on-message.

**MessageBus**: JSONL file per agent. `send()` appends; `read_inbox()` reads all + truncates.

**Lifecycle**: `spawn → WORKING → IDLE → WORKING → ... → SHUTDOWN`. Idle teammates wake when they receive a message.

**Inbox drain**: In `cot_loop`, before each LLM call, the agent's inbox is auto-drained and injected into history (like background notifications). Lead agent name defaults to "lead".

**Tools**: `team_spawn` (lead only), `team_send` (all), `team_read_inbox` (all). Teammates reuse agent identities from `SharedAgents` registry — same agent discovery as subagents.

**Protocols**: Structured request-response over inbox with shared FSM (`pending → approved | rejected`):
- **Shutdown**: `shutdown_request` (lead) → `shutdown_response` (teammate). Approval drops wake channel → teammate exits after current cycle.
- **Plan approval**: `plan_submit` (teammate) → `plan_response` (lead). Approval/rejection wakes teammate with decision.
- `InboxMessage.extra` carries protocol metadata (`request_id`, `approve`).

**Autonomy**: Teammates self-organize via idle polling. After cot_loop finishes, they poll every 5s (up to 60s) for: (1) inbox messages, (2) unclaimed tasks on the task board (pending, no owner, not blocked). Auto-claims first available task. 60s timeout → shutdown. The `idle` tool lets teammates explicitly enter idle polling.

**Identity re-injection**: When teammate history ≤ 3 messages (post-compression), identity block is re-inserted at the start.

**Worktree isolation**: `WorktreeManager` creates per-task git worktrees (`git worktree add -b wt/{name}`). Tracked in `.worktrees/index.json`, lifecycle events in `events.jsonl`. Tools: `worktree_create` (with optional task binding), `worktree_remove` (with optional task completion), `worktree_keep`, `worktree_list`, `worktree_exec`. Task gets `worktree` field; `bind_worktree`/`unbind_worktree` link both sides.

**vs Subagent**: Subagents (`task` tool) are one-shot with isolated context. Teammates are persistent with inbox communication, lifecycle management, and autonomous task claiming.

## Concurrency

Each session key gets a dedicated tokio task with its own mpsc channel.
Same session: serial. Different sessions: concurrent.
CLI waits for session completion via oneshot before showing next prompt.

## Shared State

`AppState` (`BindingRouter` + session set) wrapped in `Arc<RwLock>`. Shared between main loop and Gateway. Gateway can register agents and modify bindings at runtime via JSON-RPC.