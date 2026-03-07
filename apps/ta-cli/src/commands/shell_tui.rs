// shell_tui.rs -- Full TUI shell for `ta shell` using ratatui + crossterm.
//
// Three-zone layout:
//   1. Scrolling output pane (top)    — command output + SSE events
//   2. Input line (middle)            — text input with history
//   3. Status bar (bottom)            — project info, agent count, daemon status
//
// Background tasks feed events through an mpsc channel into the TUI event loop.

use std::io::{self, Stdout};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use super::shell::{resolve_daemon_url, StatusInfo};

/// Messages sent from background tasks to the TUI event loop.
pub enum TuiMessage {
    /// Status update from periodic health check.
    StatusUpdate(StatusInfo),
    /// An SSE event rendered for display.
    SseEvent(String),
    /// Response from a command sent to the daemon.
    CommandResponse(String),
    /// Daemon went down.
    DaemonDown,
    /// Daemon came back.
    DaemonUp,
    /// An agent is asking a question (from SSE `agent_needs_input` event).
    AgentQuestion(PendingQuestion),
}

/// A pending question from an agent that needs a human response.
#[derive(Clone, Debug)]
pub struct PendingQuestion {
    pub interaction_id: String,
    #[allow(dead_code)]
    pub goal_id: String,
    pub question: String,
    pub context: Option<String>,
    #[allow(dead_code)]
    pub response_hint: String,
    pub choices: Vec<String>,
    pub turn: u32,
}

/// A line in the output pane, with optional styling.
#[derive(Clone)]
struct OutputLine {
    text: String,
    style: Style,
}

impl OutputLine {
    fn command(text: String) -> Self {
        Self {
            text,
            style: Style::default(),
        }
    }

    fn event(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::DarkGray),
        }
    }

    fn error(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Red),
        }
    }

    fn info(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Cyan),
        }
    }
}

/// The TUI application state.
struct App {
    /// Lines displayed in the output pane.
    output: Vec<OutputLine>,
    /// Current input text.
    input: String,
    /// Cursor position within input.
    cursor: usize,
    /// Command history.
    history: Vec<String>,
    /// Current position in history (None = editing new input).
    history_idx: Option<usize>,
    /// Saved input when browsing history.
    saved_input: String,
    /// Scroll offset (0 = bottom, positive = scrolled up).
    scroll_offset: u16,
    /// Daemon status info.
    status: StatusInfo,
    /// Whether the daemon is connected.
    daemon_connected: bool,
    /// Unread event count (cleared when scrolled to bottom).
    unread_events: usize,
    /// Whether the app is still running.
    running: bool,
    /// Base URL for daemon API.
    base_url: String,
    /// Active workflow prompt (stage name, if any).
    workflow_prompt: Option<String>,
    /// Completion words for tab-completion.
    completions: Vec<String>,
    /// Session ID (if attached).
    session_id: Option<String>,
    /// Pending agent question awaiting human response.
    pending_question: Option<PendingQuestion>,
}

impl App {
    fn new(base_url: String, session_id: Option<String>) -> Self {
        Self {
            output: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_idx: None,
            saved_input: String::new(),
            scroll_offset: 0,
            status: StatusInfo::default(),
            daemon_connected: false,
            unread_events: 0,
            running: true,
            base_url,
            workflow_prompt: None,
            completions: Vec::new(),
            session_id,
            pending_question: None,
        }
    }

    fn push_output(&mut self, line: OutputLine) {
        self.output.push(line);
        // If scrolled up, don't auto-scroll — increment unread.
        if self.scroll_offset > 0 {
            self.unread_events += 1;
        }
    }

    fn push_lines(&mut self, text: &str, style_fn: fn(String) -> OutputLine) {
        for line in text.lines() {
            self.push_output(style_fn(line.to_string()));
        }
    }

    fn prompt_str(&self) -> String {
        if let Some(ref q) = self.pending_question {
            format!("[agent Q{}] > ", q.turn)
        } else if self.workflow_prompt.is_some() {
            "workflow> ".to_string()
        } else {
            "ta> ".to_string()
        }
    }

    /// Move cursor left.
    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move cursor right.
    fn cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += 1;
        }
    }

    /// Insert a character at cursor.
    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Delete character before cursor.
    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    /// Delete character at cursor.
    fn delete(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
        }
    }

    /// Move to start of input.
    fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move to end of input.
    fn end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Navigate history up.
    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_idx {
            None => {
                self.saved_input = self.input.clone();
                self.history_idx = Some(self.history.len() - 1);
                self.input = self.history[self.history.len() - 1].clone();
            }
            Some(idx) if idx > 0 => {
                self.history_idx = Some(idx - 1);
                self.input = self.history[idx - 1].clone();
            }
            _ => {}
        }
        self.cursor = self.input.len();
    }

    /// Navigate history down.
    fn history_down(&mut self) {
        match self.history_idx {
            Some(idx) if idx + 1 < self.history.len() => {
                self.history_idx = Some(idx + 1);
                self.input = self.history[idx + 1].clone();
            }
            Some(_) => {
                self.history_idx = None;
                self.input = self.saved_input.clone();
            }
            None => {}
        }
        self.cursor = self.input.len();
    }

    /// Attempt tab completion.
    fn tab_complete(&mut self) {
        if self.input.is_empty() || self.completions.is_empty() {
            return;
        }
        // Find the word at cursor.
        let prefix = &self.input[..self.cursor];
        let word_start = prefix.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = &prefix[word_start..];
        if word.is_empty() {
            return;
        }

        let matches: Vec<&String> = self
            .completions
            .iter()
            .filter(|c| c.starts_with(word))
            .collect();

        match matches.len() {
            0 => {}
            1 => {
                let replacement = matches[0].clone();
                self.input = format!(
                    "{}{}{}",
                    &self.input[..word_start],
                    replacement,
                    &self.input[self.cursor..]
                );
                self.cursor = word_start + replacement.len();
            }
            _ => {
                // Show completions in output.
                let list = matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("  ");
                self.push_output(OutputLine::info(format!("  {}", list)));
            }
        }
    }

    /// Submit the current input as a command.
    fn submit(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        // Add to history (deduplicate last).
        if self.history.last().map(|h| h.as_str()) != Some(&text) {
            self.history.push(text.clone());
        }
        self.history_idx = None;
        self.saved_input.clear();
        self.input.clear();
        self.cursor = 0;
        Some(text)
    }

    /// Scroll up in the output pane.
    fn scroll_up(&mut self, amount: u16) {
        let max_scroll = self.output.len().saturating_sub(1) as u16;
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);
    }

    /// Scroll down in the output pane.
    fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.unread_events = 0;
        }
    }

    /// Scroll to bottom.
    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.unread_events = 0;
    }
}

/// Run the TUI shell.
pub fn run(
    project_root: &Path,
    attach: Option<&str>,
    daemon_url: Option<&str>,
    init: bool,
) -> anyhow::Result<()> {
    if init {
        return super::shell::init_config(project_root);
    }

    let base_url = daemon_url
        .map(|u| u.to_string())
        .unwrap_or_else(|| resolve_daemon_url(project_root));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_tui(base_url, attach.map(|s| s.to_string())))
}

async fn run_tui(base_url: String, attach_session: Option<String>) -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    // Check daemon connectivity before entering TUI mode.
    let initial_status = super::shell::fetch_status(&client, &base_url).await;
    if initial_status.version.is_empty() || initial_status.version == "?" {
        eprintln!("Error: Cannot reach daemon at {}", base_url);
        eprintln!();
        eprintln!("Start the daemon with:");
        eprintln!("  ./scripts/ta-shell.sh          # builds + starts daemon + opens shell");
        eprintln!("  ta-daemon --api --project-root .");
        return Err(anyhow::anyhow!("daemon not reachable at {}", base_url));
    }

    // Version mismatch warning.
    let cli_version = env!("CARGO_PKG_VERSION");
    if initial_status.version != cli_version {
        eprintln!(
            "Warning: daemon is v{} but this CLI is v{}",
            initial_status.version, cli_version
        );
        eprintln!("  Restart with matching version: pkill -f 'ta-daemon' && ta-daemon --api --project-root .");
    }

    // Fetch completions.
    let completions = super::shell::fetch_completions(&client, &base_url).await;

    // Create app state.
    let mut app = App::new(base_url.clone(), attach_session.clone());
    app.status = initial_status;
    app.daemon_connected = true;
    app.completions = completions;

    // Welcome message.
    app.push_output(OutputLine::info(format!(
        "Connected to {} v{} at {}",
        app.status.project, app.status.version, base_url
    )));
    if let Some(ref sid) = attach_session {
        app.push_output(OutputLine::info(format!(
            "Attached to agent session: {}",
            sid
        )));
    }
    app.push_output(OutputLine::info(
        "Type 'help' for commands, Ctrl-C or 'exit' to quit.".to_string(),
    ));

    // Set up message channel for background tasks.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiMessage>();

    let running = Arc::new(AtomicBool::new(true));

    // Start background SSE listener.
    let sse_tx = tx.clone();
    let sse_running = running.clone();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let sse_url = format!("{}/api/events?since={}", base_url, now);
    let sse_client = client.clone();
    tokio::spawn(async move {
        background_sse(sse_client, &sse_url, sse_running, sse_tx).await;
    });

    // Start periodic health check.
    let health_tx = tx.clone();
    let health_running = running.clone();
    let health_client = client.clone();
    let health_url = base_url.clone();
    tokio::spawn(async move {
        background_health(health_client, &health_url, health_running, health_tx).await;
    });

    // Enter TUI mode.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Load history.
    let history_path = super::shell::dirs_history_path();
    if let Some(ref p) = history_path {
        if let Ok(content) = std::fs::read_to_string(p) {
            for line in content.lines() {
                if !line.is_empty() {
                    app.history.push(line.to_string());
                }
            }
        }
    }

    let result = tui_event_loop(&mut terminal, &mut app, &mut rx, &client, tx.clone()).await;

    // Cleanup.
    running.store(false, Ordering::Relaxed);
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    // Save history.
    if let Some(ref p) = history_path {
        let history_text: String = app.history.iter().map(|h| format!("{}\n", h)).collect();
        let _ = std::fs::write(p, history_text);
    }

    println!("Goodbye.");
    result
}

async fn tui_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiMessage>,
    client: &reqwest::Client,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) -> anyhow::Result<()> {
    loop {
        // Draw UI.
        terminal.draw(|f| draw_ui(f, app))?;

        if !app.running {
            break;
        }

        // Poll for crossterm events with a short timeout so we can also check messages.
        tokio::select! {
            // Keyboard / terminal events.
            _ = tokio::task::spawn_blocking(|| event::poll(std::time::Duration::from_millis(50))) => {
                // Read all available events.
                while event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                    if let Ok(ev) = event::read() {
                        handle_terminal_event(app, ev, client, &tx).await;
                    }
                }
            }
            // Background messages.
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    handle_tui_message(app, msg);
                }
            }
        }
    }
    Ok(())
}

async fn handle_terminal_event(
    app: &mut App,
    ev: Event,
    client: &reqwest::Client,
    tx: &tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    match ev {
        Event::Key(KeyEvent {
            code, modifiers, ..
        }) => {
            match (code, modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    app.running = false;
                }
                (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    if app.input.is_empty() {
                        app.running = false;
                    }
                }
                (KeyCode::Char('a'), KeyModifiers::CONTROL) => app.home(),
                (KeyCode::Char('e'), KeyModifiers::CONTROL) => app.end(),
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    app.input.drain(..app.cursor);
                    app.cursor = 0;
                }
                (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                    app.input.truncate(app.cursor);
                }
                (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                    app.output.clear();
                    app.scroll_offset = 0;
                    app.unread_events = 0;
                }
                (KeyCode::Enter, _) => {
                    if let Some(text) = app.submit() {
                        // Echo the command.
                        let prompt = app.prompt_str();
                        app.push_output(OutputLine::command(format!("{}{}", prompt, text)));
                        app.scroll_to_bottom();

                        // If there's a pending agent question, route to interaction endpoint.
                        if let Some(pq) = app.pending_question.take() {
                            let client = client.clone();
                            let base_url = app.base_url.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let result = send_interaction_response(
                                    &client,
                                    &base_url,
                                    &pq.interaction_id,
                                    &text,
                                )
                                .await;
                                match result {
                                    Ok(msg) => {
                                        let _ = tx.send(TuiMessage::CommandResponse(msg));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(TuiMessage::CommandResponse(format!(
                                            "Error responding to agent: {}",
                                            e
                                        )));
                                    }
                                }
                            });
                            return;
                        }

                        // Handle built-in commands.
                        match text.as_str() {
                            "exit" | "quit" | ":q" => {
                                app.running = false;
                                return;
                            }
                            "help" | ":help" | "?" => {
                                app.push_lines(HELP_TEXT, OutputLine::info);
                                return;
                            }
                            ":status" => {
                                let s = super::shell::fetch_status(client, &app.base_url).await;
                                app.status = s;
                                app.push_output(OutputLine::info("Status refreshed.".into()));
                                return;
                            }
                            "clear" => {
                                app.output.clear();
                                app.scroll_offset = 0;
                                app.unread_events = 0;
                                return;
                            }
                            _ => {}
                        }

                        // :tail is handled synchronously in classic mode but we
                        // can't block the TUI loop. Show a hint instead.
                        if text.starts_with(":tail") {
                            app.push_output(OutputLine::info(
                                "Tip: Agent output appears inline via SSE events. Use PgUp/PgDn to scroll.".into(),
                            ));
                            return;
                        }

                        // Send to daemon asynchronously.
                        let client = client.clone();
                        let base_url = app.base_url.clone();
                        let session_id = app.session_id.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let result = super::shell::send_input(
                                &client,
                                &base_url,
                                &text,
                                session_id.as_deref(),
                            )
                            .await;
                            match result {
                                Ok(output) => {
                                    let _ = tx.send(TuiMessage::CommandResponse(output));
                                }
                                Err(e) => {
                                    let msg = e.to_string();
                                    if msg.contains("Cannot reach daemon") {
                                        let _ = tx.send(TuiMessage::DaemonDown);
                                    }
                                    let _ = tx
                                        .send(TuiMessage::CommandResponse(format!("Error: {}", e)));
                                }
                            }
                        });
                    }
                }
                (KeyCode::Backspace, _) => app.backspace(),
                (KeyCode::Delete, _) => app.delete(),
                (KeyCode::Left, _) => app.cursor_left(),
                (KeyCode::Right, _) => app.cursor_right(),
                (KeyCode::Home, _) => app.home(),
                (KeyCode::End, _) => app.end(),
                (KeyCode::Up, _) => app.history_up(),
                (KeyCode::Down, _) => app.history_down(),
                (KeyCode::Tab, _) => app.tab_complete(),
                (KeyCode::PageUp, _) => app.scroll_up(10),
                (KeyCode::PageDown, _) => app.scroll_down(10),
                (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                    app.insert_char(c);
                }
                _ => {}
            }
        }
        Event::Resize(_, _) => {
            // Terminal will re-draw on next loop iteration.
        }
        _ => {}
    }
}

fn handle_tui_message(app: &mut App, msg: TuiMessage) {
    match msg {
        TuiMessage::StatusUpdate(status) => {
            app.status = status;
        }
        TuiMessage::SseEvent(text) => {
            // Check for workflow prompts.
            if text.contains("workflow paused") {
                // Extract stage name if possible.
                if let Some(start) = text.find('\'') {
                    if let Some(end) = text[start + 1..].find('\'') {
                        app.workflow_prompt = Some(text[start + 1..start + 1 + end].to_string());
                    }
                }
            }
            app.push_lines(&text, OutputLine::event);
        }
        TuiMessage::CommandResponse(text) => {
            // Check if we got a response that clears workflow prompt.
            if text.contains("workflow resumed") || text.contains("workflow response accepted") {
                app.workflow_prompt = None;
            }
            app.push_lines(&text, OutputLine::command);
        }
        TuiMessage::DaemonDown => {
            if app.daemon_connected {
                app.daemon_connected = false;
                app.push_output(OutputLine::error(
                    "[disconnected] Daemon unreachable. Will auto-reconnect.".into(),
                ));
            }
        }
        TuiMessage::DaemonUp => {
            if !app.daemon_connected {
                app.daemon_connected = true;
                app.push_output(OutputLine::info(
                    "[reconnected] Daemon is back online.".into(),
                ));
            }
        }
        TuiMessage::AgentQuestion(pq) => {
            app.push_output(OutputLine::info(format!(
                "\n━━━ Agent Question (turn {}) ━━━",
                pq.turn
            )));
            app.push_output(OutputLine::info(pq.question.clone()));
            if let Some(ref ctx) = pq.context {
                app.push_output(OutputLine::event(format!("  Context: {}", ctx)));
            }
            if !pq.choices.is_empty() {
                for (i, choice) in pq.choices.iter().enumerate() {
                    app.push_output(OutputLine::info(format!("  [{}] {}", i + 1, choice)));
                }
            }
            app.push_output(OutputLine::info(
                "Type your response and press Enter:".to_string(),
            ));
            app.scroll_to_bottom();
            app.pending_question = Some(pq);
        }
    }
}

fn draw_ui(f: &mut Frame, app: &App) {
    let size = f.area();

    // Layout: output (flexible), input (3 lines), status bar (1 line).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Output pane
            Constraint::Length(3), // Input area
            Constraint::Length(1), // Status bar
        ])
        .split(size);

    draw_output(f, app, chunks[0]);
    draw_input(f, app, chunks[1]);
    draw_status_bar(f, app, chunks[2]);
}

fn draw_output(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::NONE);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.output.is_empty() {
        return;
    }

    let visible_height = inner.height as usize;
    let total_lines = app.output.len();

    // Calculate which lines to show based on scroll offset.
    let end = total_lines.saturating_sub(app.scroll_offset as usize);
    let start = end.saturating_sub(visible_height);

    let visible_lines = &app.output[start..end];

    let lines: Vec<Line> = visible_lines
        .iter()
        .map(|ol| Line::styled(ol.text.clone(), ol.style))
        .collect();

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);

    // Scrollbar (only if content exceeds visible area).
    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let mut scrollbar_state =
            ScrollbarState::new(total_lines.saturating_sub(visible_height)).position(start);
        f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let prompt = app.prompt_str();
    let display = format!("{}{}", &prompt, &app.input);

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    let paragraph = Paragraph::new(display.clone()).block(block);
    f.render_widget(paragraph, area);

    // Position cursor.
    let cursor_x = (prompt.len() + app.cursor) as u16;
    // Clamp to prevent overflow on very narrow terminals.
    let x = inner.x + cursor_x.min(inner.width.saturating_sub(1));
    f.set_cursor_position((x, inner.y));
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let daemon_indicator = if app.daemon_connected {
        Span::styled(" ◉ daemon ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" ◉ daemon ", Style::default().fg(Color::Red))
    };

    let phase_str = app.status.next_phase.as_deref().unwrap_or("(none)");

    let mut spans = vec![
        Span::styled(
            format!(" {} v{} ", app.status.project, app.status.version),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("│"),
        Span::styled(
            format!(" {} agents ", app.status.active_agents),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("│"),
        Span::styled(
            format!(" {} drafts ", app.status.pending_drafts),
            if app.status.pending_drafts > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::raw("│"),
        daemon_indicator,
    ];

    // Unread event badge.
    if app.unread_events > 0 {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" {} unread ", app.unread_events),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Agent question indicator.
    if let Some(ref pq) = app.pending_question {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" Q{} pending ", pq.turn),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Workflow stage indicator.
    if let Some(ref stage) = app.workflow_prompt {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" workflow: {} ", stage),
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ));
    }

    // Phase info on the right side.
    let right_text = format!(" {} ", phase_str);
    let left_line = Line::from(spans);
    let left_width: u16 = left_line.width() as u16;
    let right_width = right_text.len() as u16;

    // Render left-aligned status.
    let bar =
        Paragraph::new(left_line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(bar, area);

    // Render right-aligned phase info (if there's room).
    if area.width > left_width + right_width {
        let right_area = Rect {
            x: area.x + area.width - right_width,
            y: area.y,
            width: right_width,
            height: 1,
        };
        let right = Paragraph::new(right_text)
            .style(Style::default().bg(Color::DarkGray).fg(Color::DarkGray));
        f.render_widget(right, right_area);
    }
}

// -- Background tasks --------------------------------------------------------

async fn background_sse(
    client: reqwest::Client,
    url: &str,
    running: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    while running.load(Ordering::Relaxed) {
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        use tokio_stream::StreamExt;
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buffer.find("\n\n") {
                let frame = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();
                // Check if this is an agent_needs_input event before generic rendering.
                if let Some(pq) = parse_agent_question(&frame) {
                    let _ = tx.send(TuiMessage::AgentQuestion(pq));
                } else if let Some(rendered) = super::shell::render_sse_event(&frame) {
                    let _ = tx.send(TuiMessage::SseEvent(rendered));
                }
            }
        }

        if running.load(Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}

async fn background_health(
    client: reqwest::Client,
    base_url: &str,
    running: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    let url = format!("{}/api/status", base_url);
    let mut was_down = false;

    while running.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        if !running.load(Ordering::Relaxed) {
            return;
        }

        match client
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => {
                if was_down {
                    was_down = false;
                    let _ = tx.send(TuiMessage::DaemonUp);
                }
                // Try to parse status for live updates.
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let status = StatusInfo {
                        project: json["project"].as_str().unwrap_or("unknown").to_string(),
                        version: json["version"].as_str().unwrap_or("?").to_string(),
                        next_phase: json["current_phase"]["id"].as_str().map(|id| {
                            let title = json["current_phase"]["title"].as_str().unwrap_or("");
                            format!("{} -- {}", id, title)
                        }),
                        pending_drafts: json["pending_drafts"].as_u64().unwrap_or(0) as usize,
                        active_agents: json["active_agents"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0),
                    };
                    let _ = tx.send(TuiMessage::StatusUpdate(status));
                }
            }
            Err(_) => {
                if !was_down {
                    was_down = true;
                    let _ = tx.send(TuiMessage::DaemonDown);
                }
            }
        }
    }
}

/// Parse an SSE frame looking for an `agent_needs_input` event.
/// Returns `Some(PendingQuestion)` if the frame contains one, `None` otherwise.
fn parse_agent_question(frame: &str) -> Option<PendingQuestion> {
    let mut event_type = None;
    let mut data = None;
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.trim());
        }
    }

    if event_type? != "agent_needs_input" {
        return None;
    }

    let json: serde_json::Value = serde_json::from_str(data?).ok()?;
    let payload = &json["payload"];

    Some(PendingQuestion {
        interaction_id: payload["interaction_id"].as_str().unwrap_or("").to_string(),
        goal_id: payload["goal_id"].as_str().unwrap_or("").to_string(),
        question: payload["question"].as_str().unwrap_or("").to_string(),
        context: payload["context"].as_str().map(|s| s.to_string()),
        response_hint: payload["response_hint"]
            .as_str()
            .unwrap_or("freeform")
            .to_string(),
        choices: payload["choices"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        turn: payload["turn"].as_u64().unwrap_or(1) as u32,
    })
}

/// Send a human response to a pending agent question via the daemon API.
async fn send_interaction_response(
    client: &reqwest::Client,
    base_url: &str,
    interaction_id: &str,
    answer: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/api/interactions/{}/respond", base_url, interaction_id);
    let body = serde_json::json!({ "answer": answer });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot reach daemon at {}: {}", base_url, e))?;

    let status_code = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));

    if !status_code.is_success() {
        let err = json["error"]
            .as_str()
            .unwrap_or("unknown error")
            .to_string();
        return Err(anyhow::anyhow!(
            "Failed to deliver response (HTTP {}): {}",
            status_code,
            err
        ));
    }

    Ok(format!(
        "Response delivered to agent (interaction: {})",
        interaction_id
    ))
}

const HELP_TEXT: &str = "\
TA Shell -- Interactive terminal for Trusted Autonomy

Commands:
  ta <cmd>           Run any ta CLI command (e.g., ta draft list)
  git <cmd>          Run git commands
  !<cmd>             Shell escape (e.g., !ls -la)
  approve <id>       Shortcut for: ta draft approve <id>
  deny <id>          Shortcut for: ta draft deny <id>
  view <id>          Shortcut for: ta draft view <id>
  apply <id>         Shortcut for: ta draft apply <id>
  status             Shortcut for: ta status
  plan               Shortcut for: ta plan list
  goals              Shortcut for: ta goal list
  drafts             Shortcut for: ta draft list
  <anything else>    Sent to agent session (if attached)

Interactive mode:
  When an agent asks a question, the prompt changes to [agent Q1] >
  Type your response and press Enter to send it back to the agent.

Shell commands:
  :status            Refresh the status bar
  clear              Clear the output pane
  Ctrl-L             Clear the output pane
  PgUp / PgDn        Scroll output
  Tab                Auto-complete commands
  Ctrl-C / exit      Exit the shell";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_insert_and_backspace() {
        let mut app = App::new("http://localhost".into(), None);
        app.insert_char('h');
        app.insert_char('i');
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor, 2);
        app.backspace();
        assert_eq!(app.input, "h");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn app_cursor_movement() {
        let mut app = App::new("http://localhost".into(), None);
        app.input = "hello".into();
        app.cursor = 5;
        app.cursor_left();
        assert_eq!(app.cursor, 4);
        app.home();
        assert_eq!(app.cursor, 0);
        app.cursor_left(); // should not go below 0
        assert_eq!(app.cursor, 0);
        app.end();
        assert_eq!(app.cursor, 5);
        app.cursor_right(); // should not go past len
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn app_history_navigation() {
        let mut app = App::new("http://localhost".into(), None);
        app.history = vec!["first".into(), "second".into()];
        app.input = "current".into();
        app.cursor = 7;

        app.history_up();
        assert_eq!(app.input, "second");
        assert_eq!(app.history_idx, Some(1));

        app.history_up();
        assert_eq!(app.input, "first");
        assert_eq!(app.history_idx, Some(0));

        app.history_up(); // at top, should stay
        assert_eq!(app.input, "first");

        app.history_down();
        assert_eq!(app.input, "second");

        app.history_down();
        assert_eq!(app.input, "current");
        assert_eq!(app.history_idx, None);
    }

    #[test]
    fn app_submit_adds_to_history() {
        let mut app = App::new("http://localhost".into(), None);
        app.input = "test command".into();
        app.cursor = 12;
        let cmd = app.submit();
        assert_eq!(cmd, Some("test command".into()));
        assert_eq!(app.history, vec!["test command".to_string()]);
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn app_submit_empty_returns_none() {
        let mut app = App::new("http://localhost".into(), None);
        app.input = "   ".into();
        let cmd = app.submit();
        assert!(cmd.is_none());
    }

    #[test]
    fn app_submit_dedup_history() {
        let mut app = App::new("http://localhost".into(), None);
        app.history = vec!["same".into()];
        app.input = "same".into();
        app.cursor = 4;
        app.submit();
        assert_eq!(app.history.len(), 1); // not duplicated
    }

    #[test]
    fn app_scroll() {
        let mut app = App::new("http://localhost".into(), None);
        for i in 0..50 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }
        assert_eq!(app.scroll_offset, 0);

        app.scroll_up(10);
        assert_eq!(app.scroll_offset, 10);
        assert_eq!(app.unread_events, 0); // unread only increments on new events while scrolled

        // New event while scrolled up.
        app.push_output(OutputLine::event("new event".into()));
        assert_eq!(app.unread_events, 1);

        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_events, 0);
    }

    #[test]
    fn app_tab_complete() {
        let mut app = App::new("http://localhost".into(), None);
        app.completions = vec!["approve".into(), "apply".into(), "deny".into()];
        app.input = "app".into();
        app.cursor = 3;

        // Multiple matches: should show list in output, not change input.
        app.tab_complete();
        assert_eq!(app.input, "app");
        assert!(!app.output.is_empty());

        // Single match.
        app.input = "den".into();
        app.cursor = 3;
        app.tab_complete();
        assert_eq!(app.input, "deny");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn app_delete_at_cursor() {
        let mut app = App::new("http://localhost".into(), None);
        app.input = "hello".into();
        app.cursor = 2;
        app.delete();
        assert_eq!(app.input, "helo");
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn app_prompt_changes_in_workflow_mode() {
        let mut app = App::new("http://localhost".into(), None);
        assert_eq!(app.prompt_str(), "ta> ");
        app.workflow_prompt = Some("review".into());
        assert_eq!(app.prompt_str(), "workflow> ");
    }

    #[test]
    fn app_prompt_changes_for_agent_question() {
        let mut app = App::new("http://localhost".into(), None);
        assert_eq!(app.prompt_str(), "ta> ");
        app.pending_question = Some(PendingQuestion {
            interaction_id: "abc123".into(),
            goal_id: "goal1".into(),
            question: "Which DB?".into(),
            context: None,
            response_hint: "choice".into(),
            choices: vec!["Postgres".into(), "SQLite".into()],
            turn: 3,
        });
        assert_eq!(app.prompt_str(), "[agent Q3] > ");
    }

    #[test]
    fn parse_agent_question_from_sse_frame() {
        let frame = concat!(
            "event: agent_needs_input\n",
            "data: {",
            "\"event_type\":\"agent_needs_input\",",
            "\"payload\":{",
            "\"goal_id\":\"00000000-0000-0000-0000-000000000001\",",
            "\"interaction_id\":\"00000000-0000-0000-0000-000000000002\",",
            "\"question\":\"Which database?\",",
            "\"context\":\"Setting up storage.\",",
            "\"response_hint\":\"choice\",",
            "\"choices\":[\"PostgreSQL\",\"SQLite\"],",
            "\"turn\":1",
            "}}"
        );
        let pq = parse_agent_question(frame).expect("should parse");
        assert_eq!(pq.question, "Which database?");
        assert_eq!(pq.turn, 1);
        assert_eq!(pq.choices, vec!["PostgreSQL", "SQLite"]);
        assert_eq!(pq.context.as_deref(), Some("Setting up storage."));
        assert_eq!(pq.interaction_id, "00000000-0000-0000-0000-000000000002");
    }

    #[test]
    fn parse_agent_question_ignores_other_events() {
        let frame = "event: goal_started\ndata: {\"event_type\":\"goal_started\",\"payload\":{\"title\":\"test\",\"agent_id\":\"claude\"}}";
        assert!(parse_agent_question(frame).is_none());
    }

    #[test]
    fn handle_tui_message_agent_question() {
        let mut app = App::new("http://localhost".into(), None);
        let pq = PendingQuestion {
            interaction_id: "abc".into(),
            goal_id: "goal1".into(),
            question: "Proceed?".into(),
            context: None,
            response_hint: "yes_no".into(),
            choices: vec![],
            turn: 1,
        };
        handle_tui_message(&mut app, TuiMessage::AgentQuestion(pq));
        assert!(app.pending_question.is_some());
        assert_eq!(app.pending_question.as_ref().unwrap().question, "Proceed?");
        // Should have added output lines for the question display.
        assert!(!app.output.is_empty());
    }

    #[test]
    fn handle_tui_message_daemon_down_up() {
        let mut app = App::new("http://localhost".into(), None);
        app.daemon_connected = true;

        handle_tui_message(&mut app, TuiMessage::DaemonDown);
        assert!(!app.daemon_connected);
        assert!(!app.output.is_empty());

        // Second DaemonDown should not add another message.
        let count = app.output.len();
        handle_tui_message(&mut app, TuiMessage::DaemonDown);
        assert_eq!(app.output.len(), count);

        handle_tui_message(&mut app, TuiMessage::DaemonUp);
        assert!(app.daemon_connected);
    }

    #[test]
    fn handle_tui_message_sse_event() {
        let mut app = App::new("http://localhost".into(), None);
        handle_tui_message(
            &mut app,
            TuiMessage::SseEvent("goal started: \"test\"".into()),
        );
        assert_eq!(app.output.len(), 1);
        assert_eq!(app.output[0].text, "goal started: \"test\"");
    }

    #[test]
    fn handle_tui_message_workflow_prompt() {
        let mut app = App::new("http://localhost".into(), None);
        handle_tui_message(
            &mut app,
            TuiMessage::SseEvent("workflow paused at 'review': Need approval".into()),
        );
        assert_eq!(app.workflow_prompt, Some("review".into()));
    }
}
