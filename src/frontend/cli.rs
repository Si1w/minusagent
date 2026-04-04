use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::*;
use tokio::sync::{Mutex, oneshot};
use tokio::time::Duration;

use crate::config::tuning;
use crate::frontend::{Channel, UserMessage};

const BANNER: &str = "\
Welcome to minusagent. Type /help for commands.

";

struct TuiState {
    output: String,
    stream_buf: String,
    output_dirty: bool,
    cached_display: Option<Text<'static>>,
    input: String,
    cursor: usize,
    scroll: u16,
    auto_scroll: bool,
    input_sender: Option<oneshot::Sender<String>>,
}

impl TuiState {
    /// Trim output buffer from the front if it exceeds the max size
    fn trim_output(&mut self) {
        let max_bytes = tuning().cli_max_output_bytes;
        if self.output.len() > max_bytes {
            let cut = self.output.len() - max_bytes;
            // Find a char boundary after the cut point
            let safe = self.output.ceil_char_boundary(cut);
            self.output.drain(..safe);
            self.output_dirty = true;
        }
    }
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
            stream_buf: String::new(),
            output_dirty: true,
            cached_display: None,
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
                s.output.push_str(&format!("{entry}\n\n"));
                s.output_dirty = true;
                s.auto_scroll = true;
            }

            for msg in crate::scheduler::drain_bg_output() {
                s.output.push_str(&msg);
                s.output.push_str("\n\n");
                s.output_dirty = true;
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
                                    .push_str(&format!("❯ {text}\n\n"));
                                s.output_dirty = true;
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
                        KeyCode::Esc => {
                            if s.input_sender.is_some() {
                                s.input.clear();
                                s.cursor = 0;
                                if let Some(tx) = s.input_sender.take() {
                                    let _ = tx.send(String::new());
                                }
                            }
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
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    // ── Output area ──
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("minus", Style::default().fg(Color::White).bold()),
        Span::styled("agent", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" "),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Indexed(238)))
        .title(title);
    let inner_width = block.inner(chunks[0]).width;
    let inner_height = block.inner(chunks[0]).height;

    if state.output_dirty || state.cached_display.is_none() {
        let parsed = tui_markdown::from_str(&state.output);
        let owned_lines: Vec<Line<'static>> = parsed
            .lines
            .into_iter()
            .map(|line| {
                Line::from(
                    line.spans
                        .into_iter()
                        .map(|s| {
                            Span::styled(
                                Cow::<'static, str>::Owned(s.content.into_owned()),
                                s.style,
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        state.cached_display = Some(Text::from(owned_lines));
        state.output_dirty = false;
    }
    let mut display_text = state.cached_display.clone().unwrap();
    if !state.stream_buf.is_empty() {
        for line in state.stream_buf.split('\n') {
            display_text.push_line(Line::raw(line));
        }
    }
    use unicode_width::UnicodeWidthStr;
    let total_lines: u16 = display_text
        .lines
        .iter()
        .map(|line| {
            let w = inner_width.max(1) as usize;
            let line_width: usize = line
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            1 + line_width.saturating_sub(1) / w
        })
        .sum::<usize>() as u16;
    let max_scroll = total_lines.saturating_sub(inner_height);

    let output =
        Paragraph::new(display_text).wrap(Wrap { trim: false });

    if state.auto_scroll {
        state.scroll = max_scroll;
    } else {
        state.scroll = state.scroll.min(max_scroll);
    }

    let output = output.block(block).scroll((state.scroll, 0));
    frame.render_widget(output, chunks[0]);

    // ── Scrollbar ──
    if total_lines > inner_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(Color::Indexed(236)))
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(Color::Indexed(245)));
        let mut scrollbar_state = ScrollbarState::new(max_scroll as usize)
            .position(state.scroll as usize);
        frame.render_stateful_widget(
            scrollbar,
            chunks[0].inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }

    // ── Status bar ──
    let waiting = state.input_sender.is_some();
    let status_left = if waiting {
        Span::styled(
            " ● ready ",
            Style::default().fg(Color::Green),
        )
    } else {
        Span::styled(
            " ◌ thinking... ",
            Style::default().fg(Color::Yellow),
        )
    };
    let hints = Line::from(vec![
        Span::styled(
            "/new ",
            Style::default().fg(Color::Indexed(243)),
        ),
        Span::styled(
            "/save ",
            Style::default().fg(Color::Indexed(243)),
        ),
        Span::styled(
            "/compact ",
            Style::default().fg(Color::Indexed(243)),
        ),
        Span::styled(
            "/team ",
            Style::default().fg(Color::Indexed(243)),
        ),
        Span::styled(
            "/agents ",
            Style::default().fg(Color::Indexed(243)),
        ),
        Span::styled(
            "/help ",
            Style::default().fg(Color::Indexed(243)),
        ),
    ]);
    let scroll_pct = if max_scroll > 0 {
        format!(
            "{}% ",
            (state.scroll as u32 * 100) / max_scroll as u32,
        )
    } else {
        String::new()
    };
    let status_right = Span::styled(
        scroll_pct,
        Style::default().fg(Color::Indexed(243)),
    );

    let status_bar = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(status_left.width() as u16),
            Constraint::Min(1),
            Constraint::Length(status_right.width() as u16),
        ])
        .split(chunks[1]);
    frame.render_widget(
        Paragraph::new(status_left),
        status_bar[0],
    );
    frame.render_widget(
        Paragraph::new(hints).alignment(Alignment::Center),
        status_bar[1],
    );
    frame.render_widget(
        Paragraph::new(status_right).alignment(Alignment::Right),
        status_bar[2],
    );

    // ── Input area ──
    let prompt = if waiting { "❯ " } else { "  " };
    let input_text = format!("{prompt}{}", state.input);
    let border_color = if waiting {
        Color::Indexed(245)
    } else {
        Color::Indexed(238)
    };
    let input = Paragraph::new(input_text.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(input, chunks[2]);

    if waiting {
        use unicode_width::UnicodeWidthStr;
        let byte_pos = state.byte_cursor();
        let display_width =
            UnicodeWidthStr::width(&state.input[..byte_pos]) as u16;
        // "❯ " is 2 display columns (❯ = 1 wide + space)
        frame.set_cursor_position((
            chunks[2].x + 1 + 2 + display_width,
            chunks[2].y + 1,
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
        state.output_dirty = true;
        state.trim_output();
        state.auto_scroll = true;
    }

    async fn confirm(&self, command: &str) -> bool {
        {
            let mut state = self.state.lock().await;
            if !state.output.ends_with('\n') {
                state.output.push('\n');
            }
            state.output
                .push_str(&format!("Execute: `{command}` (y/n)\n"));
            state.output_dirty = true;
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
        state.stream_buf.push_str(chunk);
        state.auto_scroll = true;
    }

    async fn flush(&self) {
        let mut state = self.state.lock().await;
        if !state.output.ends_with('\n') {
            state.output.push('\n');
        }
        let stream = std::mem::take(&mut state.stream_buf);
        state.output.push_str(&stream);
        if !state.output.ends_with('\n') {
            state.output.push('\n');
        }
        state.output.push('\n');
        state.output_dirty = true;
        state.trim_output();
        state.auto_scroll = true;
    }
}

