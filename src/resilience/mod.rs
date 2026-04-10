//! Three-layer resilience runner that wraps the agent loop.
//!
//! Layers, applied outer-to-inner:
//!
//! 1. **Auth rotation** — [`profile::ProfileManager`] rotates API keys/profiles
//!    when an upstream returns auth errors.
//! 2. **Overflow recovery** — On context-window overflow, compact the session
//!    and retry the same request.
//! 3. **Agent loop** — [`runner::ResilienceRunner`] hands control to
//!    [`crate::engine::agent::Agent::run`] under the layers above.
//!
//! See [`classify`] for the error taxonomy that decides which layer handles
//! a given failure.

pub mod classify;
pub mod profile;
pub mod runner;
