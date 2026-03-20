pub mod cli;

pub struct InboundMessage {
    pub text: String,
    pub sender_id: String,
    pub channel: String,
}

#[async_trait::async_trait]
pub trait Channel: Send + Sync {
    async fn receive(&self) -> Option<InboundMessage>;
    async fn send(&self, text: &str);
    async fn confirm(&self, command: &str) -> bool;
    async fn on_stream_chunk(&self, chunk: &str);
}
