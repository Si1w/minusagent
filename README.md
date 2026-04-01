# minusagent

Rust agent framework. Everything is a Node (`prep → exec → post`).

## Quick Start

### Prerequisites

- Rust (edition 2024)
- An OpenAI-compatible LLM API endpoint

### Configuration

On first run, a `config.json` template is created. Edit it with your LLM provider details:

```jsonc
{
  "llm": [
    {
      "model": "gpt-4o",
      "base_url": "https://api.openai.com/v1/",
      "api_key": "$OPENAI_API_KEY",
      "context_window": 128000
    }
  ],
  "workspace_dir": "./workspace"
}
```

String values starting with `$` are resolved as environment variables, so secrets can live in your shell profile instead of the config file. Alternatively, put the literal value directly — `config.json` is gitignored.

| Field | Required | Description |
|-------|----------|-------------|
| `llm` | Yes | Array of LLM profiles. First = primary, rest = auth rotation. |
| `llm[].model` | Yes | Model name |
| `llm[].base_url` | Yes | OpenAI-compatible API base URL |
| `llm[].api_key` | Yes | API key (or `$ENV_VAR` reference) |
| `llm[].context_window` | Yes | Max context window size (tokens) |
| `workspace_dir` | No | Workspace root for agent discovery (default: `./workspace`) |
| `discord_token` | No | Discord bot token for `/discord` gateway (or `$ENV_VAR`) |
| `fallback_models` | No | Array of fallback model names for resilience |
| `tuning` | No | Runtime-tunable parameters (all have sensible defaults) |

Manage LLM profiles at runtime with `/llm`, `/llm add`, `/llm rm <model>`, `/llm primary <model>`.

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

- **Discord** (`/discord`) — Requires `discord_token` in config.json.
- **WebSocket** (`/gateway`) — JSON-RPC interface for programmatic access. Supports agent registration, binding management, and message dispatch.

All frontends implement the `Channel` trait and can run concurrently.

## Resilience

When multiple LLM profiles are configured in the `llm` array, the resilience layer provides:

- **Profile rotation** — On auth, billing, or rate-limit failures, automatically rotates to the next available profile with category-specific cooldowns.
- **Overflow recovery** — On context-window overflow, compacts message history and retries.
- **Fallback models** — If `fallback_models` is set, tries alternative models when the primary fails.
