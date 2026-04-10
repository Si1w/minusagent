//! # minusagent
//!
//! Rust agent framework where everything is a `Node` (`prep → exec → post`).
//!
//! ## Architecture
//!
//! ```text
//! Frontend (CLI TUI / Discord / WebSocket Gateway)
//!        ↓
//!     UserMessage
//!        ↓
//! BindingRouter (BindingTable → agent_id → build_session_key)
//!        ↓
//! Session (per session_key, built from AgentConfig)
//! ├── Persistence: JSONL + index
//! ├── Commands: /new /save /load /list /compact /prompt /remember /help /exit
//! ├── 3-layer compaction: L1 micro → L2 auto → L3 full
//! └── Agent CoT loop (wrapped by ResilienceRunner)
//!        ↓
//!   ResilienceRunner
//!        ├── Layer 1: Auth rotation (ProfileManager)
//!        ├── Layer 2: Overflow recovery (compact)
//!        └── Layer 3: Agent.run()
//!               ├── LLMCall (Node) — streaming OpenAI-compatible API
//!               └── dispatch_tool() — 23+ tools
//! ```
//!
//! ## Module Map
//!
//! - [`engine`] — `Node` abstraction, `Agent` `CoT` loop, LLM calls, `Session`, `SharedStore`
//! - [`intelligence`] — 7-layer prompt assembly, agent manager, skills, memory
//! - [`routing`] — 5-tier `BindingRouter`, delivery queue, control protocol
//! - [`scheduler`] — Heartbeat (per-session) + cron (global), lane lock
//! - [`frontend`] — CLI TUI, Discord, WebSocket gateway, REPL
//! - [`team`] — `TeammateManager`, message bus, tasks, todos, worktrees
//! - [`resilience`] — Auth rotation, overflow recovery, runner
//! - [`tool`] — 23+ tools, dispatch, schemas, permissions
//! - [`config`] — `AppConfig`, `LLMConfig`, `Tuning` (global `OnceLock`)
//! - [`runtime`] — Persisted service intent across restarts
//! - [`logger`] — TUI logger

pub mod config;
pub mod engine;
pub mod frontend;
pub mod intelligence;
pub mod logger;
pub mod resilience;
pub mod routing;
pub mod runtime;
pub mod scheduler;
pub mod team;
pub mod tool;
