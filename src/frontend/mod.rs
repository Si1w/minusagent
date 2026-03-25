pub mod cli;
pub mod discord;
pub mod gateway;
pub mod repl;
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
