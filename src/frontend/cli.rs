use std::borrow::Cow;
use std::fmt::Write;
use std::io;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use tokio::sync::{Mutex, oneshot};
use tokio::time::Duration;
use unicode_width::UnicodeWidthStr;

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
        let max_bytes = tuning().frontend.cli_max_output_bytes;
        if self.output.len() > max_bytes {
            let cut = self.output.len() - max_bytes;
            // Find a char boundary after the cut point
            let safe = self.output.ceil_char_boundary(cut);
            self.output.drain(..safe);
            self.output_dirty = true;
        }
    }

    fn mark_output_changed(&mut self) {
        self.output_dirty = true;
        self.auto_scroll = true;
        self.trim_output();
    }

    fn ensure_output_break(&mut self) {
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn append_paragraph(&mut self, text: &str) {
        self.ensure_output_break();
        self.output.push_str(text);
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output.push('\n');
        self.mark_output_changed();
    }

    fn append_user_input(&mut self, text: &str) {
        self.ensure_output_break();
        self.output.push_str("❯ ");
        self.output.push_str(text);
        self.output.push_str("\n\n");
        self.mark_output_changed();
    }

    fn append_confirm_prompt(&mut self, command: &str) {
        self.ensure_output_break();
        let _ = writeln!(self.output, "Execute: `{command}` (y/n)");
        self.mark_output_changed();
    }
}

/// Interactive CLI frontend using ratatui TUI
pub struct Cli {
    state: Arc<Mutex<TuiState>>,
}

impl Default for Cli {
    fn default() -> Self {
        Self::new()
    }
}

/// Restore terminal to normal mode
pub fn cleanup_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
}

impl Cli {
    /// Create a new CLI with TUI event loop
    ///
    /// # Panics
    ///
    /// Panics if raw mode, alternate screen, or terminal backend initialization fails.
    #[must_use]
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
        let terminal = Terminal::new(backend).expect("failed to create terminal");

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
            drain_runtime_output(&mut s);

            let _ = terminal.draw(|f| render(&mut s, f));
        }

        tokio::select! {
            event = reader.next() => {
                if let Some(Ok(Event::Key(key))) = event {
                    let mut s = state.lock().await;
                    if handle_key_event(key, &mut s) {
                        cleanup_terminal();
                        std::process::exit(0);
                    }
                }
            }
            () = tokio::time::sleep(Duration::from_millis(tuning().frontend.cli_refresh_interval_ms)) => {}
        }
    }
}

impl TuiState {
    fn byte_cursor(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map_or(self.input.len(), |(i, _)| i)
    }
}

struct OutputMetrics {
    max_scroll: u16,
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

    let output_metrics = render_output_area(state, frame, chunks[0]);
    render_status_bar(state, frame, chunks[1], output_metrics.max_scroll);
    render_input_area(state, frame, chunks[2]);
}

fn render_output_area(state: &mut TuiState, frame: &mut Frame, area: Rect) -> OutputMetrics {
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
    let inner = block.inner(area);
    let inner_width = inner.width;
    let inner_height = inner.height;

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
    let mut display_text = state.cached_display.clone().unwrap_or_default();
    if !state.stream_buf.is_empty() {
        for line in state.stream_buf.split('\n') {
            display_text.push_line(Line::raw(line));
        }
    }
    let total_lines = total_display_lines(&display_text, inner_width);
    let max_scroll = total_lines.saturating_sub(inner_height);

    let output = Paragraph::new(display_text).wrap(Wrap { trim: false });

    if state.auto_scroll {
        state.scroll = max_scroll;
    } else {
        state.scroll = state.scroll.min(max_scroll);
    }

    let output = output.block(block).scroll((state.scroll, 0));
    frame.render_widget(output, area);

    if total_lines > inner_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(Color::Indexed(236)))
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(Color::Indexed(245)));
        let mut scrollbar_state =
            ScrollbarState::new(usize::from(max_scroll)).position(usize::from(state.scroll));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }

    OutputMetrics { max_scroll }
}

fn render_status_bar(state: &TuiState, frame: &mut Frame, area: Rect, max_scroll: u16) {
    let waiting = state.input_sender.is_some();
    let status_left = if waiting {
        Span::styled(" ● ready ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" ◌ thinking... ", Style::default().fg(Color::Yellow))
    };
    let hints = Line::from(vec![
        Span::styled("/new ", Style::default().fg(Color::Indexed(243))),
        Span::styled("/save ", Style::default().fg(Color::Indexed(243))),
        Span::styled("/compact ", Style::default().fg(Color::Indexed(243))),
        Span::styled("/team ", Style::default().fg(Color::Indexed(243))),
        Span::styled("/agents ", Style::default().fg(Color::Indexed(243))),
        Span::styled("/help ", Style::default().fg(Color::Indexed(243))),
    ]);
    let scroll_pct = if max_scroll > 0 {
        format!(
            "{}% ",
            (u32::from(state.scroll) * 100) / u32::from(max_scroll),
        )
    } else {
        String::new()
    };
    let status_right = Span::styled(scroll_pct, Style::default().fg(Color::Indexed(243)));

    let status_bar = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(usize_to_u16_saturating(status_left.width())),
            Constraint::Min(1),
            Constraint::Length(usize_to_u16_saturating(status_right.width())),
        ])
        .split(area);
    frame.render_widget(Paragraph::new(status_left), status_bar[0]);
    frame.render_widget(
        Paragraph::new(hints).alignment(Alignment::Center),
        status_bar[1],
    );
    frame.render_widget(
        Paragraph::new(status_right).alignment(Alignment::Right),
        status_bar[2],
    );
}

fn render_input_area(state: &TuiState, frame: &mut Frame, area: Rect) {
    let waiting = state.input_sender.is_some();
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
    frame.render_widget(input, area);

    if waiting {
        let byte_pos = state.byte_cursor();
        let display_width =
            usize_to_u16_saturating(UnicodeWidthStr::width(&state.input[..byte_pos]));
        // "❯ " is 2 display columns (❯ = 1 wide + space)
        frame.set_cursor_position((area.x + 1 + 2 + display_width, area.y + 1));
    }
}

fn total_display_lines(display_text: &Text<'_>, inner_width: u16) -> u16 {
    let width = usize::from(inner_width.max(1));
    let total = display_text
        .lines
        .iter()
        .map(|line| {
            let line_width: usize = line
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            1 + line_width.saturating_sub(1) / width
        })
        .sum::<usize>();
    usize_to_u16_saturating(total)
}

fn usize_to_u16_saturating(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn drain_runtime_output(state: &mut TuiState) {
    for entry in crate::logger::TuiLogger::drain() {
        state.append_paragraph(&entry.to_string());
    }

    for msg in crate::scheduler::drain_bg_output() {
        state.append_paragraph(&msg);
    }
}

fn handle_key_event(key: crossterm::event::KeyEvent, state: &mut TuiState) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }

    match key.code {
        KeyCode::Char(c) => {
            let pos = state.byte_cursor();
            state.input.insert(pos, c);
            state.cursor += 1;
        }
        KeyCode::Backspace => {
            if state.cursor > 0 {
                state.cursor -= 1;
                let pos = state.byte_cursor();
                state.input.remove(pos);
            }
        }
        KeyCode::Left => {
            state.cursor = state.cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            let len = state.input.chars().count();
            if state.cursor < len {
                state.cursor += 1;
            }
        }
        KeyCode::Enter => {
            if state.input_sender.is_some() {
                let text = std::mem::take(&mut state.input);
                state.cursor = 0;
                state.append_user_input(&text);
                if let Some(tx) = state.input_sender.take() {
                    let _ = tx.send(text);
                }
            }
        }
        KeyCode::Up => {
            state.scroll = state.scroll.saturating_sub(1);
            state.auto_scroll = false;
        }
        KeyCode::Down => {
            state.scroll = state.scroll.saturating_add(1);
        }
        KeyCode::Esc => {
            if state.input_sender.is_some() {
                state.input.clear();
                state.cursor = 0;
                if let Some(tx) = state.input_sender.take() {
                    let _ = tx.send(String::new());
                }
            }
        }
        KeyCode::Home => state.cursor = 0,
        KeyCode::End => {
            state.cursor = state.input.chars().count();
        }
        _ => {}
    }

    false
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
        state.append_paragraph(text);
    }

    async fn confirm(&self, command: &str) -> bool {
        {
            let mut state = self.state.lock().await;
            state.append_confirm_prompt(command);
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
        state.ensure_output_break();
        let stream = std::mem::take(&mut state.stream_buf);
        state.output.push_str(&stream);
        if !state.output.ends_with('\n') {
            state.output.push('\n');
        }
        state.output.push('\n');
        state.mark_output_changed();
    }
}
