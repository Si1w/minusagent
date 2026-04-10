//! Core agent runtime.
//!
//! Everything here is built around the [`node::Node`] abstraction
//! (`prep → exec → post`):
//!
//! - [`agent`] — `Agent` runs the chain-of-thought loop, dispatching tools.
//! - [`llm`] — `LLMCall` is the streaming OpenAI-compatible client wrapped as a node.
//! - [`session`] — `Session` orchestrates one user-facing conversation
//!   (persistence, commands, compaction).
//! - [`store`] — `SharedStore` (LLM-visible `Context` + LLM-invisible `SystemState`).
//! - [`node`] — The universal `Node` trait used by everything above.

pub mod agent;
pub mod llm;
pub mod node;
pub mod session;
pub mod store;
