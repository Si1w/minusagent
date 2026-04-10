//! Five-tier routing, delivery queue, and control protocol.
//!
//! - [`router`] — `BindingRouter` resolves an incoming `UserMessage` to a
//!   `(agent_id, session_key)` pair through five priority tiers:
//!   peer > guild > account > channel > default.
//! - [`delivery`] — Persistent outbound queue. Background tasks (heartbeat,
//!   cron, team) enqueue messages here so the gateway can deliver them
//!   reliably across restarts.
//! - [`protocol`] — Control protocol shared between protocol-aware frontends
//!   (stdio, SDK, JSON-RPC). Defines `ControlMessage`, `ControlEvent`,
//!   `PermissionMode`, `ToolPolicy`, and the `ProtocolChannel` adapter.

pub mod delivery;
pub mod protocol;
pub mod router;
