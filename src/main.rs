use std::sync::Arc;

mod core;
mod frontend;

use crate::core::session::Session;
use crate::core::store::{Config, Context, LLMConfig, SharedStore, SystemState};
use crate::frontend::Channel;
use crate::frontend::cli::Cli;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let system_prompt = std::fs::read_to_string("prompts/system.md")
        .expect("Failed to read prompts/system.md");

    let store = SharedStore {
        context: Context {
            system_prompt,
            history: Vec::new(),
        },
        state: SystemState {
            config: Config {
                llm: LLMConfig {
                    model: std::env::var("LLM_MODEL").expect("LLM_MODEL not set"),
                    base_url: std::env::var("LLM_BASE_URL")
                        .expect("LLM_BASE_URL not set"),
                    api_key: std::env::var("LLM_API_KEY")
                        .expect("LLM_API_KEY not set"),
                    context_window: std::env::var("LLM_CONTEXT_WINDOW")
                        .expect("LLM_CONTEXT_WINDOW not set")
                        .parse()
                        .expect("LLM_CONTEXT_WINDOW must be a number"),
                },
            },
        },
    };

    let channel: Arc<dyn Channel> = Arc::new(Cli::new());
    let mut session = Session::new(store, channel.clone())
        .expect("Failed to initialize session");

    loop {
        let msg = match channel.receive().await {
            Some(msg) => msg,
            None => continue,
        };

        if msg.text == "quit" || msg.text == "exit" {
            break;
        }

        if let Err(e) = session.turn(&msg.text).await {
            channel.send(&format!("Error: {e}")).await;
        }
    }
}
