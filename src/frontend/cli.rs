use std::io::{self, Write};
use std::sync::Mutex;

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
    editor: Mutex<DefaultEditor>,
}

impl Cli {
    /// Create a new CLI instance with a readline editor
    pub fn new() -> Self {
        Self {
            editor: Mutex::new(DefaultEditor::new().expect("failed to create editor")),
        }
    }
}

#[async_trait::async_trait]
impl Channel for Cli {
    async fn receive(&self) -> Option<UserMessage> {
        let prompt = format!("{TEAL}{BOLD}> {RESET}");
        let line = self.editor.lock().ok()?.readline(&prompt).ok()?;

        let text = line.trim().to_string();
        if text.is_empty() {
            return None;
        }

        Some(UserMessage {
            text,
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
        let prompt = format!(
            "\n{YELLOW}{BOLD}Execute:{RESET} {TEAL_LIGHT}`{command}`{RESET} {YELLOW}(y/n) > {RESET}"
        );
        let line = match self.editor.lock().ok().and_then(|mut e| e.readline(&prompt).ok()) {
            Some(l) => l,
            None => return false,
        };
        line.trim().eq_ignore_ascii_case("y")
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        print!("{CORAL}{chunk}{RESET}");
        io::stdout().flush().ok();
    }
}
