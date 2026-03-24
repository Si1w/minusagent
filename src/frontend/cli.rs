use std::io;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::*;
use tokio::sync::{Mutex, oneshot};
use tokio::time::Duration;

use crate::frontend::{Channel, UserMessage};

const BANNER: &str = "\
Welcome. Type a message to chat, or use / commands.\n\n";

struct TuiState {
    output: String,
    input: String,
    cursor: usize,
    scroll: u16,
    auto_scroll: bool,
    input_sender: Option<oneshot::Sender<String>>,
}

/// Interactive CLI frontend using ratatui TUI
pub struct Cli {
    state: Arc<Mutex<TuiState>>,
}

/// Restore terminal to normal mode
pub fn cleanup_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
}

impl Cli {
    /// Create a new CLI with TUI event loop
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(TuiState {
            output: BANNER.to_string(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            auto_scroll: true,
            input_sender: None,
        }));

        terminal::enable_raw_mode().expect("failed to enable raw mode");
        crossterm::execute!(io::stdout(), EnterAlternateScreen)
            .expect("failed to enter alternate screen");
        let backend = CrosstermBackend::new(io::stdout());
        let terminal =
            Terminal::new(backend).expect("failed to create terminal");

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            cleanup_terminal();
            original_hook(info);
        }));

        let state_clone = state.clone();
        tokio::spawn(run_event_loop(state_clone, terminal));

        Self { state }
    }
}

async fn run_event_loop(
    state: Arc<Mutex<TuiState>>,
    mut terminal: Terminal<CrosstermBackend<io::Stdout>>,
) {
    let mut reader = EventStream::new();

    loop {
        {
            let mut s = state.lock().await;

            for entry in crate::logger::TuiLogger::drain() {
                s.output.push_str(&format!("{entry}\n"));
                s.auto_scroll = true;
            }

            let _ = terminal.draw(|f| render(&mut s, f));
        }

        tokio::select! {
            event = reader.next() => {
                if let Some(Ok(Event::Key(key))) = event {
                    let mut s = state.lock().await;

                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        cleanup_terminal();
                        std::process::exit(0);
                    }

                    match key.code {
                        KeyCode::Char(c) => {
                            let pos = s.byte_cursor();
                            s.input.insert(pos, c);
                            s.cursor += 1;
                        }
                        KeyCode::Backspace => {
                            if s.cursor > 0 {
                                s.cursor -= 1;
                                let pos = s.byte_cursor();
                                s.input.remove(pos);
                            }
                        }
                        KeyCode::Left => {
                            s.cursor = s.cursor.saturating_sub(1);
                        }
                        KeyCode::Right => {
                            let len = s.input.chars().count();
                            if s.cursor < len {
                                s.cursor += 1;
                            }
                        }
                        KeyCode::Enter => {
                            if s.input_sender.is_some() {
                                let text = std::mem::take(&mut s.input);
                                s.cursor = 0;
                                s.output
                                    .push_str(&format!("> {text}\n"));
                                s.auto_scroll = true;
                                if let Some(tx) = s.input_sender.take() {
                                    let _ = tx.send(text);
                                }
                            }
                        }
                        KeyCode::Up => {
                            s.scroll = s.scroll.saturating_sub(1);
                            s.auto_scroll = false;
                        }
                        KeyCode::Down => {
                            s.scroll = s.scroll.saturating_add(1);
                        }
                        KeyCode::Home => s.cursor = 0,
                        KeyCode::End => {
                            s.cursor = s.input.chars().count();
                        }
                        _ => {}
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

impl TuiState {
    fn byte_cursor(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }
}

fn render(state: &mut TuiState, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());

    // Output area
    let title = Line::from(vec![
        Span::raw(" minus"),
        Span::styled("agent", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" "),
    ]);
    let hint = Line::from(vec![
        Span::styled(
            " /new /save /load /list /compact ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "/remember ",
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            "/<skill> ",
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            "/agents /switch /bind ",
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            "/help /discord /gateway /exit ",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
        .title_bottom(hint);
    let inner_width = block.inner(chunks[0]).width;
    let inner_height = block.inner(chunks[0]).height;

    let total_lines: u16 = state
        .output
        .lines()
        .map(|line| {
            let w = inner_width.max(1) as usize;
            1 + line.len().saturating_sub(1) / w
        })
        .sum::<usize>() as u16;
    let max_scroll = total_lines.saturating_sub(inner_height);

    let output =
        Paragraph::new(state.output.as_str()).wrap(Wrap { trim: false });

    if state.auto_scroll {
        state.scroll = max_scroll;
    } else {
        state.scroll = state.scroll.min(max_scroll);
    }

    let output = output.block(block).scroll((state.scroll, 0));
    frame.render_widget(output, chunks[0]);

    // Input area
    let waiting = state.input_sender.is_some();
    let prompt = if waiting { "> " } else { "  " };
    let input_text = format!("{prompt}{}", state.input);
    let border_style = if waiting {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input = Paragraph::new(input_text.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    frame.render_widget(input, chunks[1]);

    if waiting {
        use unicode_width::UnicodeWidthStr;
        let byte_pos = state.byte_cursor();
        let display_width =
            UnicodeWidthStr::width(&state.input[..byte_pos]) as u16;
        frame.set_cursor_position((
            chunks[1].x + 1 + prompt.len() as u16 + display_width,
            chunks[1].y + 1,
        ));
    }
}

#[async_trait::async_trait]
impl Channel for Cli {
    async fn receive(&self) -> Option<UserMessage> {
        let (tx, rx) = oneshot::channel();
        self.state.lock().await.input_sender = Some(tx);

        let text = rx.await.ok()?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }

        Some(UserMessage {
            text,
            sender_id: "cli-user".into(),
            channel: "cli".into(),
            account_id: String::new(),
            guild_id: String::new(),
        })
    }

    async fn send(&self, text: &str) {
        let mut state = self.state.lock().await;
        if !state.output.ends_with('\n') {
            state.output.push('\n');
        }
        if !text.is_empty() {
            state.output.push_str(text);
            state.output.push('\n');
        }
        state.output.push('\n');
        state.auto_scroll = true;
    }

    async fn confirm(&self, command: &str) -> bool {
        {
            let mut state = self.state.lock().await;
            state.output
                .push_str(&format!("Execute: `{command}` (y/n)\n"));
            state.auto_scroll = true;
        }

        let (tx, rx) = oneshot::channel();
        self.state.lock().await.input_sender = Some(tx);

        match rx.await.ok() {
            Some(reply) => reply.trim().eq_ignore_ascii_case("y"),
            None => false,
        }
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        let mut state = self.state.lock().await;
        state.output.push_str(chunk);
        state.auto_scroll = true;
    }

    async fn flush(&self) {
        let mut state = self.state.lock().await;
        if !state.output.ends_with('\n') {
            state.output.push('\n');
        }
        state.output.push('\n');
        state.auto_scroll = true;
    }
}
