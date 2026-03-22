use std::io::{self, Write};
use std::sync::Arc;

use rustyline::DefaultEditor;

use crate::frontend::{Channel, UserMessage};

const TEAL: &str = "\x1b[38;2;70;120;142m";
const TEAL_LIGHT: &str = "\x1b[38;2;120;183;201m";
const YELLOW: &str = "\x1b[38;2;246;224;147m";
const CORAL: &str = "\x1b[38;2;229;139;123m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Interactive CLI frontend using rustyline
pub struct Cli {
    editor: Arc<std::sync::Mutex<DefaultEditor>>,
}

impl Cli {
    /// Create a new CLI instance with a readline editor
    pub fn new() -> Self {
        println!(
            "\n{TEAL}{BOLD}minusagent{RESET}\n\n\
             {TEAL_LIGHT}  /new <label>    {RESET}Start a new session\n\
             {TEAL_LIGHT}  /save           {RESET}Save current session\n\
             {TEAL_LIGHT}  /load <id>      {RESET}Load a session by ID or prefix\n\
             {TEAL_LIGHT}  /list           {RESET}List all sessions\n\
             {TEAL_LIGHT}  /compact        {RESET}Compact conversation history\n\
             {TEAL_LIGHT}  /discord        {RESET}Start Discord gateway\n\
             {TEAL_LIGHT}  /exit           {RESET}Exit\n"
        );
        Self {
            editor: Arc::new(std::sync::Mutex::new(
                DefaultEditor::new().expect("failed to create editor"),
            )),
        }
    }
}

#[async_trait::async_trait]
impl Channel for Cli {
    async fn receive(&self) -> Option<UserMessage> {
        let editor = self.editor.clone();
        let result = tokio::task::spawn_blocking(move || {
            let prompt = format!("{TEAL}{BOLD}> {RESET}");
            let line = editor.lock().ok()?.readline(&prompt).ok()?;
            let text = line.trim().to_string();
            if text.is_empty() { None } else { Some(text) }
        })
        .await
        .ok()??;

        Some(UserMessage {
            text: result,
            sender_id: "cli-user".into(),
            channel: "cli".into(),
        })
    }

    async fn send(&self, text: &str) {
        if text.is_empty() {
            println!("\n");
        } else {
            println!("\n{CORAL}{text}{RESET}\n");
        }
    }

    async fn confirm(&self, command: &str) -> bool {
        let editor = self.editor.clone();
        let prompt = format!(
            "\n{YELLOW}{BOLD}Execute:{RESET} {TEAL_LIGHT}`{command}`{RESET} \
             {YELLOW}(y/n) > {RESET}"
        );
        let result = tokio::task::spawn_blocking(move || {
            editor
                .lock()
                .ok()
                .and_then(|mut e| e.readline(&prompt).ok())
        })
        .await
        .ok()
        .flatten();

        result
            .map(|l| l.trim().eq_ignore_ascii_case("y"))
            .unwrap_or(false)
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        print!("{CORAL}{chunk}{RESET}");
        io::stdout().flush().ok();
    }
}
