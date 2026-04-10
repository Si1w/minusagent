//! User-facing frontends.
//!
//! Each frontend turns external input (terminal keystrokes, Discord messages,
//! WebSocket frames) into a [`UserMessage`] and feeds it through the routing
//! and session layers via the [`Channel`] trait.
//!
//! - [`cli`] — Ratatui terminal TUI.
//! - [`discord`] — Discord gateway client.
//! - [`gateway`] — JSON-RPC 2.0 WebSocket gateway with managed services.
//! - [`repl`] — Readline-style REPL backed by the same router.
//! - [`stdio`] — Line-oriented JSON-over-stdio protocol channel.
//! - [`utils`] — Shared text helpers used by frontends.

pub mod cli;
pub mod discord;
pub mod gateway;
pub(crate) mod launch;
pub mod repl;
pub mod stdio;
pub mod utils;

/// A message received from the user
pub struct UserMessage {
    pub text: String,
    pub sender_id: String,
    pub channel: String,
    pub account_id: String,
    pub guild_id: String,
}

/// Frontend communication interface
///
/// Abstracts user interaction so the agent can work with different
/// frontends (CLI, web, etc.).
#[async_trait::async_trait]
pub trait Channel: Send + Sync {
    /// Receive user input, returns `None` on EOF or empty input
    async fn receive(&self) -> Option<UserMessage>;

    /// Send text output to the user
    async fn send(&self, text: &str);

    /// Ask user to confirm a command before execution
    async fn confirm(&self, command: &str) -> bool;

    /// Ask user to approve a specific tool invocation
    ///
    /// Default delegates to `confirm()`. Protocol-aware channels
    /// override this to send structured `ToolRequest` events.
    async fn can_use_tool(&self, tool: &str, args: &serde_json::Value) -> bool {
        self.confirm(&format!("{tool}: {args}")).await
    }

    /// Stream a chunk of LLM response to the user
    async fn on_stream_chunk(&self, chunk: &str);

    /// Flush buffered stream content after LLM finishes responding
    async fn flush(&self);
}

/// No-op channel for internal LLM calls (e.g. context compaction)
pub struct SilentChannel;

#[async_trait::async_trait]
impl Channel for SilentChannel {
    async fn receive(&self) -> Option<UserMessage> {
        None
    }

    async fn send(&self, _text: &str) {}

    async fn confirm(&self, _command: &str) -> bool {
        true
    }

    async fn on_stream_chunk(&self, _chunk: &str) {}

    async fn flush(&self) {}
}
