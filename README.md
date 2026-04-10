# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Quick Start

### Prerequisites

- Rust (edition 2024)
- An OpenAI-compatible LLM API endpoint

### Configuration

On first run, a `config.toml` template is created. Configure LLM profiles via CLI commands:

```
/llm add              # interactive — prompts for model, base_url, api_key, context_window
/llm                  # list all profiles
/llm primary <model>  # set primary profile
/llm rm <model>       # remove a profile
```

The first profile added becomes the primary. Additional profiles are used for auth rotation by the resilience layer.

API keys support `$ENV_VAR` syntax — the value is resolved from your shell environment at runtime, so secrets stay out of the config file.

| Section | Required | Description |
|---------|----------|-------------|
| `[llm]` | Yes | LLM provider profiles + `fallback_models` for resilience |
| `[workspace]` | No | Workspace root for agent discovery (default: `./workspace`) |
| `[frontend.startup]` | No | Default frontend mode (`repl` or `stdio`) |
| `[frontend.discord]` | No | Discord bot token for `/discord` gateway (or `$ENV_VAR`) |
| `[frontend.websocket]` | No | WebSocket gateway host and port |
| `[services.*]` | No | Per-service autostart policy (`cron`, `delivery`, `discord`, `websocket`) |
| `[tuning.*]` | No | Runtime-tunable parameters (all have sensible defaults) |

### Build & Run

```bash
cargo build --release
cargo run
```

This launches the TUI (ratatui-based terminal interface). Type messages to chat with the agent.

## Workspace Setup

Agents are discovered from `WORKSPACE_DIR/.agents/`. Each subdirectory with an `AGENT.md` becomes a registered agent.

```
workspace/
├── routes.json                   # persisted route bindings
└── .agents/
    └── my-agent/
        ├── AGENT.md              # agent identity (system prompt)
        ├── TOOLS.md              # tool usage guidelines (optional)
        ├── HEARTBEAT.md          # enables periodic heartbeat (optional)
        ├── memory/               # progressive memory files
        │   └── *.md
        └── skills/               # discoverable skills
            └── my-skill/
                └── SKILL.md
```

- **AGENT.md** — Plain markdown. Entire content becomes the agent's system prompt.
- **TOOLS.md** — Guidelines for how the agent should use tools.
- **HEARTBEAT.md** — Presence of this file enables per-session heartbeat scheduling.
- **memory/** — Each `.md` file is a memory entry. Only TLDRs are loaded at startup; full content is read on demand via `read_file`.
- **skills/** — Each subdirectory containing a `SKILL.md` is a discoverable skill. Invoked via `/<skill-name>` in chat.

## CLI Commands

| Category | Command | Description |
|----------|---------|-------------|
| **Sessions** | `/new <label>` | New session |
| | `/save` | Save session |
| | `/load <label>` | Load session |
| | `/list` | List sessions |
| | `/compact` | Compact history (L1 micro + L3 full) |
| **Intelligence** | `/prompt` | Show system prompt |
| | `/remember <name> <txt>` | Save memory (LLM generates TLDR) |
| | `/<skill> [args]` | Invoke discovered skill |
| **Team** | `/team` | Show team roster |
| | `/inbox` | Check lead inbox |
| | `/tasks` | Show task board |
| | `/worktrees` | List worktrees |
| | `/events` | Worktree event log |
| **Agents & Routing** | `/agents` | List registered agents |
| | `/switch <agent>` | Switch to a specific agent |
| | `/switch off` | Restore default routing |
| | `/bindings` | List route bindings |
| | `/route <ch> <peer>` | Test route resolution |
| **Resilience** | `/profiles` | Show API key profiles |
| | `/lanes` | Show lane stats |
| **Scheduler** | `/heartbeat` | Heartbeat status |
| | `/trigger` | Manual heartbeat |
| | `/cron` | List cron jobs |
| | `/cron trigger <id>` | Trigger a cron job |
| | `/cron reload` | Reload CRON.json |
| | `/cron stop` | Stop cron service |
| | `/delivery` | Delivery queue stats |
| | `/delivery stop` | Stop delivery runner |
| **Config** | `/llm` | List LLM profiles |
| | `/llm add` | Add profile (interactive) |
| | `/llm rm <model>` | Remove profile |
| | `/llm primary <model>` | Set primary |
| **Gateways** | `/discord` | Start Discord bot |
| | `/gateway` | Start WebSocket API gateway |
| | `/help` | Show commands |
| | `/exit` | Exit |

## Gateways

Beyond the CLI TUI, two additional frontends can be started at runtime:

- **Discord** (`/discord`) — Requires `[frontend.discord]` token in `config.toml`.
- **WebSocket** (`/gateway`) — JSON-RPC interface for programmatic access. Supports agent registration, binding management, and message dispatch.

All frontends implement the `Channel` trait and can run concurrently.

## Compaction

Session history is compacted automatically via a 3-layer cascade when token usage exceeds `compact_threshold` (default 87%):

1. **L1 MicroCompact** — Clears old tool-result content in-place. No API call, instant.
2. **L2 AutoCompact** — Summarizes the oldest 50% of messages via LLM. Circuit breaker trips after `compact_max_failures` (default 3) consecutive failures.
3. **L3 FullCompact** — Summarizes the entire history and re-injects recently read file paths and active todo items.

Manual `/compact` runs L1 + L3 directly.

## Resilience

When multiple LLM profiles are configured in the `llm` array, the resilience layer provides:

- **Profile rotation** — On auth, billing, or rate-limit failures, automatically rotates to the next available profile with category-specific cooldowns.
- **Overflow recovery** — On context-window overflow, compacts message history and retries.
- **Fallback models** — If `fallback_models` is set, tries alternative models when the primary fails.

## SDK Usage

minusagent can be used as a Rust library. Add it as a dependency:

```toml
[dependencies]
minusagent = { path = "../minusagent" }
```

### Gateway (high-level)

The `Gateway` handles routing, session lifecycle, and concurrency. Use `ProtocolChannel` to receive structured events:

```rust
use std::sync::Arc;

use minusagent::frontend::UserMessage;
use minusagent::routing::protocol::{ControlEvent, ProtocolChannel};

// Assuming `gateway` is already initialized (see main.rs for setup)
let (channel, mut rx) = ProtocolChannel::new();

let msg = UserMessage {
    text: "Hello".into(),
    sender_id: "user-1".into(),
    channel: "sdk".into(),
    account_id: String::new(),
    guild_id: String::new(),
};

let result = gateway.dispatch(msg, Arc::new(channel), None).await?;

// Consume the event stream
while let Some(event) = rx.recv().await {
    match event {
        ControlEvent::StreamDelta { text } => print!("{text}"),
        ControlEvent::ToolRequest { request_id, tool, args } => {
            // Approve or deny tool execution
        }
        ControlEvent::TurnComplete { .. } => break,
        _ => {}
    }
}
```

### Session (mid-level)

Bypass routing and drive a session directly:

```rust
use minusagent::engine::session::Session;

let mut session = Session::new(store, lane_lock, heartbeat, profiles, fallback, interrupted)?;
session.turn("user input", &channel).await?;
```

### Agent (low-level)

Run a single CoT loop with no session management:

```rust
use minusagent::engine::agent::Agent;

let agent = Agent;
agent.run(&mut store, &channel, &http, None).await?;
```

### Key Types

| Type | Module | Role |
|------|--------|------|
| `Gateway` | `frontend::gateway` | High-level dispatcher with routing and session pool |
| `ProtocolChannel` | `routing::protocol` | Structured event stream for SDK consumers |
| `ControlEvent` | `routing::protocol` | Server → client events (stream, tool request, completion) |
| `Session` | `engine::session` | Per-conversation orchestrator |
| `Agent` | `engine::agent` | Stateless CoT loop |
| `SharedStore` | `engine::store` | Context (LLM-visible) + SystemState (LLM-invisible) |
| `Channel` | `frontend` | Trait for custom frontends |
| `AgentManager` | `intelligence::manager` | Agent registry and workspace discovery |
| `BindingRouter` | `routing::router` | 5-tier message routing |
