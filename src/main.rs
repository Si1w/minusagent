use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

mod core;
mod frontend;
mod logger;

use crate::core::session::Session;
use crate::core::store::{Config, Context, LLMConfig, SharedStore, SystemState};
use crate::frontend::Channel;
use crate::frontend::cli::Cli;

/// A message routed from a channel to the main loop
pub struct RoutedMessage {
    text: String,
    session_key: String,
    channel: Arc<dyn Channel>,
    done: Option<oneshot::Sender<()>>,
}

fn build_store() -> SharedStore {
    let system_prompt = std::fs::read_to_string("prompts/system.md")
        .expect("Failed to read prompts/system.md");

    SharedStore {
        context: Context {
            system_prompt,
            history: Vec::new(),
        },
        state: SystemState {
            config: Config {
                llm: LLMConfig {
                    model: std::env::var("LLM_MODEL")
                        .expect("LLM_MODEL not set"),
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
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    logger::TuiLogger::init();

    let (tx, mut rx) = mpsc::channel::<RoutedMessage>(32);

    // CLI always starts; /discord spawns the gateway at runtime
    let cli: Arc<dyn Channel> = Arc::new(Cli::new());
    let cli_clone = cli.clone();
    let cli_tx = tx.clone();
    let dc_tx = tx.clone();
    drop(tx);

    tokio::spawn(async move {
        let mut discord_started = false;
        loop {
            let msg = match cli_clone.receive().await {
                Some(msg) => msg,
                None => continue,
            };

            if msg.text == "/exit" {
                frontend::cli::cleanup_terminal();
                std::process::exit(0);
            }

            if msg.text == "/discord" {
                if discord_started {
                    cli_clone.send("Discord gateway already running").await;
                    continue;
                }
                match std::env::var("DISCORD_BOT_TOKEN") {
                    Ok(token) if !token.is_empty() => {
                        let gateway_tx = dc_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                frontend::discord::start_gateway(
                                    token, gateway_tx,
                                )
                                .await
                            {
                                log::error!("Discord gateway error: {e}");
                            }
                        });
                        discord_started = true;
                        cli_clone.send("Discord gateway started").await;
                    }
                    _ => {
                        cli_clone.send("DISCORD_BOT_TOKEN not set").await;
                    }
                }
                continue;
            }

            let (done_tx, done_rx) = oneshot::channel();
            let _ = cli_tx
                .send(RoutedMessage {
                    text: msg.text,
                    session_key: format!(
                        "{}:{}",
                        msg.channel, msg.sender_id
                    ),
                    channel: cli_clone.clone(),
                    done: Some(done_tx),
                })
                .await;
            let _ = done_rx.await;
        }
    });

    // Main loop: route messages to per-session tasks
    let mut session_txs =
        HashMap::<String, mpsc::Sender<RoutedMessage>>::new();

    while let Some(routed) = rx.recv().await {
        let session_tx = session_txs
            .entry(routed.session_key.clone())
            .or_insert_with(|| {
                let (stx, mut srx) = mpsc::channel::<RoutedMessage>(8);
                tokio::spawn(async move {
                    let mut session = Session::new(build_store())
                        .expect("Failed to create session");
                    while let Some(msg) = srx.recv().await {
                        if let Err(e) =
                            session.turn(&msg.text, &msg.channel).await
                        {
                            msg.channel
                                .send(&format!("Error: {e}"))
                                .await;
                        }
                        if let Some(done) = msg.done {
                            let _ = done.send(());
                        }
                    }
                });
                stx
            });

        if session_tx.send(routed).await.is_err() {
            log::error!("Session task unexpectedly closed");
        }
    }
}
