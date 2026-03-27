# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Quick Start

### Prerequisites

- Rust (edition 2024)
- An OpenAI-compatible LLM API endpoint

### Configuration

Create a `.env` file in the project root:

```env
LLM_MODEL=gpt-4o
LLM_BASE_URL=https://api.openai.com/v1/
LLM_API_KEY=sk-...
LLM_CONTEXT_WINDOW=128000
WORKSPACE_DIR=./workspace
```

| Variable | Required | Description |
|----------|----------|-------------|
| `LLM_MODEL` | Yes | Default model name |
| `LLM_BASE_URL` | Yes | OpenAI-compatible API base URL |
| `LLM_API_KEY` | Yes | API key |
| `LLM_CONTEXT_WINDOW` | Yes | Max context window size (tokens) |
| `WORKSPACE_DIR` | No | Workspace root for agent discovery (default: `./workspace`) |
| `DISCORD_BOT_TOKEN` | No | Discord bot token for `/discord` gateway |
| `LLM_API_KEY_1`, `LLM_BASE_URL_1`, ... | No | Additional API profiles for resilience failover |
| `LLM_FALLBACK_MODELS` | No | Comma-separated fallback model names |
| `RUST_LOG` | No | Log level (`debug`, `info`, `warn`, `error`) |

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
| | `/compact` | Compact history |
| **Intelligence** | `/prompt` | Show system prompt |
| | `/remember <name> <txt>` | Save memory (LLM generates TLDR) |
| | `/<skill> [args]` | Invoke discovered skill |
| **Agents & Routing** | `/agents` | List registered agents |
| | `/switch <agent>` | Switch to a specific agent |
| | `/switch off` | Restore default routing |
| | `/bindings` | List route bindings |
| | `/route <ch> <peer>` | Test route resolution |
| **Scheduler** | `/heartbeat` | Heartbeat status |
| | `/heartbeat stop` | Stop heartbeat |
| | `/trigger` | Manual heartbeat |
| | `/cron` | List cron jobs |
| | `/cron stop` | Stop cron service |
| | `/delivery` | Delivery queue stats |
| | `/delivery stop` | Stop delivery runner |
| **Gateways** | `/discord` | Start Discord bot |
| | `/gateway` | Start WebSocket API gateway |
| | `/help` | Show commands |
| | `/exit` | Exit |

## Gateways

Beyond the CLI TUI, two additional frontends can be started at runtime:

- **Discord** (`/discord`) — Requires `DISCORD_BOT_TOKEN` env var.
- **WebSocket** (`/gateway`) — JSON-RPC interface for programmatic access. Supports agent registration, binding management, and message dispatch.

All frontends implement the `Channel` trait and can run concurrently.

## Resilience

When multiple API keys are configured (`LLM_API_KEY_1`, `LLM_API_KEY_2`, ...), the resilience layer provides:

- **Profile rotation** — On auth, billing, or rate-limit failures, automatically rotates to the next available API key with category-specific cooldowns.
- **Overflow recovery** — On context-window overflow, compacts message history and retries.
- **Fallback models** — If `LLM_FALLBACK_MODELS` is set, tries alternative models when the primary fails.
