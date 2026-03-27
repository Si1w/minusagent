pub mod cron;
pub mod heartbeat;
pub mod lane;

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;

use crate::core::agent::Agent;
use crate::core::store::{
    Config, Context, LLMConfig, Message, Role, SharedStore, SystemState,
};
use crate::frontend::SilentChannel;

/// Per-session lane name shared by user turns and heartbeat
pub const LANE_SESSION: &str = "session";

/// Per-session command queue for lane-based coordination
pub type LaneLock = Arc<lane::CommandQueue>;

static BG_OUTPUT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

/// Initialize the background output buffer (call once at startup)
pub fn init_bg_output() {
    BG_OUTPUT.get_or_init(|| Mutex::new(Vec::new()));
}

/// Push a message to the background output buffer
pub fn push_bg_output(msg: String) {
    if let Some(buf) = BG_OUTPUT.get() {
        if let Ok(mut v) = buf.lock() {
            v.push(msg);
        }
    }
}

/// Current time as fractional seconds since UNIX epoch
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Drain all buffered background messages
pub fn drain_bg_output() -> Vec<String> {
    BG_OUTPUT
        .get()
        .and_then(|buf| buf.lock().ok())
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

/// Run a single agent turn in isolation
///
/// Creates a throwaway SharedStore, pushes one user message, runs the CoT
/// loop, and returns the final assistant response text.
///
/// # Arguments
///
/// * `system_prompt` - System prompt for this turn
/// * `user_message` - The instruction to send as a user message
/// * `llm_config` - LLM provider configuration
///
/// # Returns
///
/// The assistant's response text, or empty string if no response.
///
/// # Errors
///
/// Returns error if the LLM call fails.
pub async fn run_single_turn(
    system_prompt: &str,
    user_message: &str,
    llm_config: &LLMConfig,
) -> Result<String> {
    let mut store = SharedStore {
        context: Context {
            system_prompt: system_prompt.to_string(),
            history: vec![Message {
                role: Role::User,
                content: Some(user_message.to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
        },
        state: SystemState {
            config: Config {
                llm: LLMConfig {
                    model: llm_config.model.clone(),
                    base_url: llm_config.base_url.clone(),
                    api_key: llm_config.api_key.clone(),
                    context_window: llm_config.context_window,
                },
            },
            intelligence: None,
        },
    };

    let channel: Arc<dyn crate::frontend::Channel> =
        Arc::new(SilentChannel);
    let http = reqwest::Client::new();
    let agent = Agent;
    agent.run(&mut store, &channel, &http).await?;

    let response = store
        .context
        .history
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    Ok(response)
}
