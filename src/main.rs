use std::sync::Arc;

mod core;
mod frontend;

use crate::core::agent::Agent;
use crate::core::store::{Config, Context, LlmConfig, SharedStore, SystemState};
use crate::frontend::Channel;
use crate::frontend::cli::Cli;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let store = SharedStore {
        context: Context {
            system_prompt: "You are a helpful assistant.".into(),
            history: Vec::new(),
        },
        state: SystemState {
            config: Config {
                llm: LlmConfig {
                    model: std::env::var("LLM_MODEL").expect("LLM_MODEL not set"),
                    base_url: std::env::var("LLM_BASE_URL").expect("LLM_BASE_URL not set"),
                    api_key: std::env::var("LLM_API_KEY").expect("LLM_API_KEY not set"),
                },
            },
        },
    };

    let channel: Arc<dyn Channel> = Arc::new(Cli::new());
    let mut agent = Agent::new(store, channel.clone());

    loop {
        let msg = match channel.receive().await {
            Some(msg) => msg,
            None => break,
        };

        if msg.text == "quit" || msg.text == "exit" {
            break;
        }

        if let Err(e) = agent.turn(&msg.text).await {
            channel.send(&format!("Error: {e}")).await;
        }
    }
}
