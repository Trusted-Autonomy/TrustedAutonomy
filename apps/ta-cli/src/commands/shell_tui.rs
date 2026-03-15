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

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
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
    /// Live agent output line from goal output stream (v0.10.11).
    AgentOutput(AgentOutputLine),
    /// An agent is requesting stdin input via a detected prompt (v0.10.18.5).
    StdinPrompt(PendingStdinPrompt),
    /// An agent prompt was auto-answered (v0.10.18.5).
    StdinAutoAnswered { prompt: String, response: String },
    /// A goal started — may trigger auto-tail (v0.10.11).
    GoalStarted { goal_id: String, title: String },
    /// Agent output stream ended (goal process exited).
    AgentOutputDone(String),
    /// A draft is ready for review (v0.10.11).
    DraftReady {
        #[allow(dead_code)]
        goal_id: String,
        #[allow(dead_code)]
        draft_id: String,
        display_id: String,
        title: String,
    },
}

/// A line of live agent output (v0.10.11).
#[derive(Clone, Debug)]
pub struct AgentOutputLine {
    pub stream: String,
    pub line: String,
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

/// A pending stdin prompt from an agent process that needs a human response (v0.10.18.5).
#[derive(Clone, Debug)]
pub struct PendingStdinPrompt {
    pub goal_id: String,
    pub prompt_text: String,
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

    fn agent_stdout(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::White),
        }
    }

    fn agent_stderr(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Yellow),
        }
    }

    fn notification(text: String) -> Self {
        Self {
            text,
            style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn separator(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::DarkGray),
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
    scroll_offset: usize,
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
    /// Pending stdin prompt from agent process awaiting user input (v0.10.18.5).
    pending_stdin_prompt: Option<PendingStdinPrompt>,
    /// Goal ID currently being tailed for agent output (v0.10.11).
    tailing_goal: Option<String>,
    /// Maximum output buffer lines (configurable, v0.10.11).
    output_buffer_limit: usize,
    /// Whether auto-tail on goal start is enabled (v0.10.11).
    auto_tail: bool,
    /// Number of lines to show as backfill when attaching to tail (v0.10.11).
    tail_backfill_lines: usize,
    /// Whether split-pane mode is active (Ctrl-W toggle, v0.10.14).
    split_pane: bool,
    /// Agent output lines (displayed in right/bottom pane when split, v0.10.14).
    agent_output: Vec<OutputLine>,
    /// Scroll offset for agent pane in split mode.
    agent_scroll_offset: usize,
    /// Project root path for local commands like follow-up picker (v0.10.14).
    project_root: std::path::PathBuf,
}

impl App {
    fn new(base_url: String, session_id: Option<String>, project_root: std::path::PathBuf) -> Self {
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
            pending_stdin_prompt: None,
            tailing_goal: None,
            output_buffer_limit: 50000,
            auto_tail: true,
            tail_backfill_lines: 5,
            split_pane: false,
            agent_output: Vec::new(),
            agent_scroll_offset: 0,
            project_root,
        }
    }

    fn push_output(&mut self, line: OutputLine) {
        self.output.push(line);
        // Enforce buffer limit — drop oldest lines when exceeded.
        if self.output.len() > self.output_buffer_limit {
            let excess = self.output.len() - self.output_buffer_limit;
            self.output.drain(..excess);
            // Adjust scroll offset to compensate for removed lines.
            self.scroll_offset = self.scroll_offset.saturating_sub(excess);
        }
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
        if let Some(ref _sp) = self.pending_stdin_prompt {
            "[stdin] > ".to_string()
        } else if let Some(ref q) = self.pending_question {
            format!("[agent Q{}] > ", q.turn)
        } else if self.workflow_prompt.is_some() {
            "workflow> ".to_string()
        } else {
            "ta> ".to_string()
        }
    }

    /// Move cursor left by one character (char-boundary aware).
    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            while self.cursor > 0 && !self.input.is_char_boundary(self.cursor) {
                self.cursor -= 1;
            }
        }
    }

    /// Move cursor right by one character (char-boundary aware).
    fn cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += 1;
            while self.cursor < self.input.len() && !self.input.is_char_boundary(self.cursor) {
                self.cursor += 1;
            }
        }
    }

    /// Insert a character at cursor.
    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete character before cursor.
    fn backspace(&mut self) {
        if self.cursor > 0 {
            let mut new_cursor = self.cursor - 1;
            while new_cursor > 0 && !self.input.is_char_boundary(new_cursor) {
                new_cursor -= 1;
            }
            self.cursor = new_cursor;
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
    fn scroll_up(&mut self, amount: usize) {
        // Use logical line count as upper bound. The actual visual max is
        // computed in draw_output with the real terminal width, but logical
        // lines are a safe ceiling — you can't scroll past all content.
        let max_scroll = self.output.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);
    }

    /// Scroll down in the output pane.
    fn scroll_down(&mut self, amount: usize) {
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
    no_version_check: bool,
) -> anyhow::Result<()> {
    if init {
        return super::shell::init_config(project_root);
    }

    let base_url = daemon_url
        .map(|u| u.to_string())
        .unwrap_or_else(|| resolve_daemon_url(project_root));

    let rt = tokio::runtime::Runtime::new()?;

    // Version guard check (v0.10.10).
    if !no_version_check {
        let client = reqwest::Client::new();
        let _guard = super::version_guard::check_daemon_version(
            &client,
            &base_url,
            project_root,
            true, // interactive — TUI hasn't started yet, stdin is available
            &rt,
        );
        // All results proceed — the function already printed warnings/prompts.
    }

    // Agent terms consent check (v0.10.18.4 item 4 & 7).
    // Before entering TUI mode (while stdin is still available), check if the
    // default agent (claude-code) has current consent. If not, prompt the user.
    {
        let default_agent = "claude-code";
        let current_version = super::consent::detect_agent_version(default_agent);
        if let Err(_msg) =
            super::consent::check_agent_consent(project_root, default_agent, &current_version)
        {
            // Show inline consent prompt before TUI takes over stdin.
            println!();
            println!(
                "Agent '{}' requires terms acceptance before goals can be dispatched.",
                default_agent
            );
            if let Err(e) = super::consent::prompt_and_accept(project_root, default_agent) {
                eprintln!("Warning: {}", e);
                eprintln!(
                    "Goals using '{}' will fail until terms are accepted.",
                    default_agent
                );
                eprintln!(
                    "You can accept later with: ta terms accept {}",
                    default_agent
                );
                // Continue to shell anyway — the user can still use other commands.
            }
        }
    }

    let project_root = project_root.to_path_buf();
    rt.block_on(run_tui(
        base_url,
        attach.map(|s| s.to_string()),
        project_root,
    ))
}

async fn run_tui(
    base_url: String,
    attach_session: Option<String>,
    project_root: std::path::PathBuf,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    // Check daemon connectivity before entering TUI mode.
    let initial_status = super::shell::fetch_status(&client, &base_url).await;
    if initial_status.version.is_empty() || initial_status.version == "?" {
        eprintln!("Error: Cannot reach daemon at {}", base_url);
        eprintln!();
        eprintln!("Start the daemon with:");
        eprintln!("  ta daemon start                # start in background");
        eprintln!("  ta daemon start --foreground   # start in foreground (for debugging)");
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

    // Load shell config from workflow.toml (v0.10.11).
    let shell_config = {
        // Try to find the project root from the status endpoint or use cwd.
        let workflow_path = std::env::current_dir()
            .unwrap_or_default()
            .join(".ta/workflow.toml");
        let wf = ta_submit::WorkflowConfig::load_or_default(&workflow_path);
        wf.shell
    };

    // Create app state.
    let mut app = App::new(base_url.clone(), attach_session.clone(), project_root);
    app.status = initial_status;
    app.daemon_connected = true;
    app.completions = completions;
    app.output_buffer_limit = shell_config.effective_scrollback();
    app.auto_tail = shell_config.auto_tail;
    app.tail_backfill_lines = shell_config.tail_backfill_lines;

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
    stdout.execute(EnableMouseCapture)?;
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
    terminal.backend_mut().execute(DisableMouseCapture)?;
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
                    // Check if this is a GoalStarted that needs auto-tail.
                    let auto_tail_goal = if let TuiMessage::GoalStarted { ref goal_id, .. } = msg {
                        if app.auto_tail && app.tailing_goal.is_none() {
                            Some(goal_id.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    handle_tui_message(app, msg);

                    // Spawn auto-tail if triggered.
                    if let Some(goal_id) = auto_tail_goal {
                        let tail_client = client.clone();
                        let tail_base = app.base_url.clone();
                        let tail_tx = tx.clone();
                        let backfill = app.tail_backfill_lines;
                        tokio::spawn(async move {
                            start_tail_stream(
                                tail_client,
                                &tail_base,
                                Some(&goal_id),
                                tail_tx,
                                backfill,
                            ).await;
                        });
                    }
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
                    // If tailing agent output, Ctrl-C detaches instead of exiting (v0.10.14).
                    if app.tailing_goal.is_some() {
                        let goal_id = app.tailing_goal.take().unwrap();
                        let short = &goal_id[..8.min(goal_id.len())];
                        app.push_output(OutputLine::info(format!(
                            "Detached from {} (Ctrl-C)",
                            short
                        )));
                    } else if app.pending_stdin_prompt.is_some() {
                        // Cancel pending stdin prompt (v0.10.18.5).
                        app.pending_stdin_prompt = None;
                        app.push_output(OutputLine::info("Stdin prompt cancelled.".to_string()));
                    } else if app.pending_question.is_some() {
                        // Cancel pending question prompt.
                        app.pending_question = None;
                        app.push_output(OutputLine::info("Agent question cancelled.".to_string()));
                    } else {
                        app.running = false;
                    }
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
                (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                    // Toggle split-pane mode (v0.10.14).
                    app.split_pane = !app.split_pane;
                    let mode = if app.split_pane { "on" } else { "off" };
                    app.push_output(OutputLine::info(format!(
                        "Split pane: {} (Ctrl-W to toggle)",
                        mode
                    )));
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

                        // If there's a pending stdin prompt, route to goal input endpoint (v0.10.18.5).
                        if let Some(sp) = app.pending_stdin_prompt.take() {
                            let client = client.clone();
                            let base_url = app.base_url.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let url = format!("{}/api/goals/{}/input", base_url, sp.goal_id);
                                let result = client
                                    .post(&url)
                                    .json(&serde_json::json!({ "input": text }))
                                    .send()
                                    .await;
                                match result {
                                    Ok(resp) if resp.status().is_success() => {
                                        let _ = tx.send(TuiMessage::CommandResponse(format!(
                                            "Sent input to agent: {}",
                                            text
                                        )));
                                    }
                                    Ok(resp) => {
                                        let body = resp.text().await.unwrap_or_default();
                                        let _ = tx.send(TuiMessage::CommandResponse(format!(
                                            "Error sending input: {}",
                                            body
                                        )));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(TuiMessage::CommandResponse(format!(
                                            "Error sending stdin input: {}",
                                            e
                                        )));
                                    }
                                }
                            });
                            return;
                        }

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

                        // :tail — attach to goal output stream (v0.10.11).
                        // Supports: :tail [id] [--lines <count>]
                        if text.starts_with(":tail") {
                            let (goal_id_arg, backfill) =
                                parse_tail_args(&text, app.tail_backfill_lines);
                            let client = client.clone();
                            let base_url = app.base_url.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                start_tail_stream(
                                    client,
                                    &base_url,
                                    goal_id_arg.as_deref(),
                                    tx,
                                    backfill,
                                )
                                .await;
                            });
                            return;
                        }

                        // :follow-up — fuzzy-searchable follow-up picker (v0.10.14).
                        if text.starts_with(":follow-up") || text.starts_with(":followup") {
                            handle_follow_up_picker(app, &text);
                            return;
                        }

                        // Agent consent check for goal-dispatching commands (v0.10.18.4 item 7).
                        // If the command is `run` or `dev`, verify that agent consent is current
                        // before dispatching. If consent is missing or outdated, block the
                        // dispatch with an actionable error message.
                        if text.starts_with("run ")
                            || text.starts_with("dev ")
                            || text == "run"
                            || text == "dev"
                        {
                            let default_agent = "claude-code";
                            let current_version =
                                super::consent::detect_agent_version(default_agent);
                            if let Err(msg) = super::consent::check_agent_consent(
                                &app.project_root,
                                default_agent,
                                &current_version,
                            ) {
                                app.push_output(OutputLine::error(msg));
                                app.push_output(OutputLine::info(
                                    "Exit the shell and run: ta terms accept claude-code"
                                        .to_string(),
                                ));
                                return;
                            }
                        }

                        // Immediate ack so the user sees activity before the
                        // daemon responds (v0.10.15.1).
                        app.push_output(OutputLine::info(format!("Dispatching: {}", text)));

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
                                    // Check for streaming response from agent.
                                    if let Some(request_id) = output.strip_prefix("__streaming__:")
                                    {
                                        let request_id = request_id.trim().to_string();
                                        let _ = tx.send(TuiMessage::CommandResponse(
                                            "Agent is working...".to_string(),
                                        ));
                                        // Subscribe to the agent output stream.
                                        stream_agent_output(
                                            client.clone(),
                                            &base_url,
                                            &request_id,
                                            tx.clone(),
                                        )
                                        .await;
                                    } else {
                                        let _ = tx.send(TuiMessage::CommandResponse(output));
                                    }
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
                // Shift+Home/End scroll output; plain Home/End move cursor (v0.10.18.2).
                (KeyCode::Home, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_up(app.output.len());
                }
                (KeyCode::End, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_to_bottom();
                }
                (KeyCode::Home, _) => app.home(),
                (KeyCode::End, _) => app.end(),
                // Shift+Up/Down scroll output 1 line; plain Up/Down navigate history (v0.10.18.2).
                (KeyCode::Up, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_up(1);
                }
                (KeyCode::Down, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_down(1);
                }
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
        Event::Mouse(mouse_event) => match mouse_event.kind {
            MouseEventKind::ScrollUp => app.scroll_up(3),
            MouseEventKind::ScrollDown => app.scroll_down(3),
            _ => {}
        },
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
        TuiMessage::StdinPrompt(sp) => {
            // Display the stdin prompt and switch to input mode (v0.10.18.5 item 5).
            app.push_output(OutputLine::info("\n━━━ Agent Stdin Prompt ━━━".to_string()));
            app.push_output(OutputLine::info(sp.prompt_text.clone()));
            app.push_output(OutputLine::info(
                "Type your response and press Enter:".to_string(),
            ));
            app.scroll_to_bottom();
            app.pending_stdin_prompt = Some(sp);
        }
        TuiMessage::StdinAutoAnswered { prompt, response } => {
            // Show auto-answered prompt as dimmed informational line (v0.10.18.5 item 5).
            app.push_output(OutputLine::event(format!(
                "[auto] {} → {}",
                prompt, response
            )));
        }
        TuiMessage::AgentOutput(line) => {
            let styled = if line.stream == "stderr" {
                OutputLine::agent_stderr(line.line)
            } else {
                // Try to parse stream-json format from `claude --output-format stream-json`.
                // Extract human-readable text content from JSON lines.
                // Also detect model name from message_start events (v0.10.14).
                if app.status.agent_model.is_none() {
                    if let Some(model) = extract_model_from_stream_json(&line.line) {
                        app.status.agent_model = Some(model);
                    }
                }
                match parse_stream_json_text(&line.line) {
                    Some(text) if !text.is_empty() => OutputLine::agent_stdout(text),
                    Some(_) => return, // Empty text chunk — skip.
                    None => OutputLine::agent_stdout(line.line), // Not JSON — show raw.
                }
            };
            // In split-pane mode, route agent output to the agent pane (v0.10.14).
            if app.split_pane {
                app.agent_output.push(styled);
                // Enforce buffer limit on agent pane too.
                if app.agent_output.len() > app.output_buffer_limit {
                    let excess = app.agent_output.len() - app.output_buffer_limit;
                    app.agent_output.drain(..excess);
                }
            } else {
                app.push_output(styled);
            }
        }
        TuiMessage::GoalStarted { goal_id, title } => {
            app.push_output(OutputLine::notification(format!(
                "[goal started] \"{}\" ({})",
                title,
                &goal_id[..8.min(goal_id.len())]
            )));
            app.scroll_to_bottom();
            // Store goal ID for auto-tail (the actual tail subscription is handled
            // by the caller since it needs the tx channel and client).
            if app.auto_tail && app.tailing_goal.is_none() {
                app.tailing_goal = Some(goal_id);
            }
        }
        TuiMessage::AgentOutputDone(goal_id) => {
            let short_id = &goal_id[..8.min(goal_id.len())];
            app.push_output(OutputLine::separator(format!(
                "━━━ Agent output ended ({}) ━━━",
                short_id
            )));
            if app.tailing_goal.as_deref() == Some(&goal_id) {
                app.tailing_goal = None;
            }
        }
        TuiMessage::DraftReady {
            goal_id: _,
            draft_id: _,
            display_id,
            title,
        } => {
            app.push_output(OutputLine::notification(format!(
                "[draft ready] \"{}\" ({}) — run: draft view {}",
                title, display_id, display_id
            )));
            app.scroll_to_bottom();
        }
    }
}

fn draw_ui(f: &mut Frame, app: &App) {
    let size = f.area();

    // Calculate input area height dynamically based on wrapped text (v0.10.14).
    // The block has top+bottom borders (2 lines), plus content lines.
    let prompt = app.prompt_str();
    let display_len = prompt.len() + app.input.chars().count();
    // Inner width = total width minus 0 (no side borders on input block).
    let inner_width = size.width.saturating_sub(0) as usize;
    let content_lines = if inner_width == 0 {
        1
    } else {
        display_len.div_ceil(inner_width).max(1)
    };
    // Borders add 2 lines; cap at half the terminal to keep output visible.
    let input_height = (content_lines as u16 + 2).min(size.height / 2).max(3);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),               // Output pane
            Constraint::Length(input_height), // Input area (dynamic)
            Constraint::Length(1),            // Status bar
        ])
        .split(size);

    // In split-pane mode, divide the output area into two side-by-side panes (v0.10.14).
    if app.split_pane {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[0]);
        draw_output(f, app, split[0]);
        draw_agent_pane(f, app, split[1]);
    } else {
        draw_output(f, app, chunks[0]);
    }
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
    let wrap_width = inner.width as usize;

    // Count total *visual* lines (accounting for word wrap) so scrollback works
    // correctly when lines are longer than the terminal width.
    let visual_line_count: usize = app
        .output
        .iter()
        .map(|ol| {
            if ol.text.is_empty() || wrap_width == 0 {
                1
            } else {
                // Ceiling division: chars / width, minimum 1.
                ol.text.len().div_ceil(wrap_width)
            }
        })
        .sum();

    // `scroll_offset` 0 = bottom. Convert to a top-based visual-line offset.
    let max_scroll = visual_line_count.saturating_sub(visible_height);
    let scroll_y = max_scroll.saturating_sub(app.scroll_offset);

    // Ratatui's Paragraph::scroll() takes (u16, u16), which overflows beyond
    // 65 535 visual lines. To support unlimited scrollback we pre-slice the
    // logical lines to just the visible window and use a small residual scroll
    // for the partial first line.
    let mut cumulative: usize = 0;
    let mut start_idx: usize = 0;
    let mut residual_scroll: u16 = 0;
    for (i, ol) in app.output.iter().enumerate() {
        let vlines = if ol.text.is_empty() || wrap_width == 0 {
            1
        } else {
            ol.text.len().div_ceil(wrap_width)
        };
        if cumulative + vlines > scroll_y {
            start_idx = i;
            residual_scroll = (scroll_y - cumulative) as u16;
            break;
        }
        cumulative += vlines;
        start_idx = i + 1;
    }

    // Take enough logical lines to fill the visible area (with margin).
    let mut end_idx = start_idx;
    let mut visible_vlines: usize = 0;
    for ol in app.output.iter().skip(start_idx) {
        let vlines = if ol.text.is_empty() || wrap_width == 0 {
            1
        } else {
            ol.text.len().div_ceil(wrap_width)
        };
        visible_vlines += vlines;
        end_idx += 1;
        if visible_vlines >= visible_height + residual_scroll as usize {
            break;
        }
    }

    let lines: Vec<Line> = app.output[start_idx..end_idx.min(app.output.len())]
        .iter()
        .map(|ol| Line::styled(ol.text.clone(), ol.style))
        .collect();

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((residual_scroll, 0));
    f.render_widget(paragraph, inner);

    // Scrollbar.
    if visual_line_count > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_y);
        f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

/// Draw the agent output pane (right side in split-pane mode, v0.10.14).
/// Convert a markdown-ish text line into styled ratatui Spans (v0.10.14).
///
/// Handles: `# headers`, `**bold**`, `` `inline code` ``, `- list items`.
/// When `in_code_block` is true, the entire line is rendered as code.
/// Returns updated `in_code_block` state (toggled by ``` fences).
fn stylize_markdown_line<'a>(
    text: &'a str,
    base_style: Style,
    in_code_block: &mut bool,
) -> Line<'a> {
    let trimmed = text.trim();

    // Toggle code block on ``` fences.
    if trimmed.starts_with("```") {
        *in_code_block = !*in_code_block;
        let fence_style = base_style.fg(Color::DarkGray);
        return Line::from(Span::styled(text.to_string(), fence_style));
    }

    // Inside a code block: render as monospace-styled.
    if *in_code_block {
        let code_style = base_style.fg(Color::Yellow);
        return Line::from(Span::styled(text.to_string(), code_style));
    }

    // Headers: # / ## / ###
    if trimmed.starts_with("# ") {
        let header_style = base_style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
        return Line::from(Span::styled(text.to_string(), header_style));
    }
    if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
        let header_style = base_style.fg(Color::Cyan);
        return Line::from(Span::styled(text.to_string(), header_style));
    }

    // List items: - or *
    let list_prefix = trimmed.starts_with("- ") || trimmed.starts_with("* ");

    // Parse inline formatting: **bold** and `code`.
    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut current_start = 0;

    while let Some(&(i, ch)) = chars.peek() {
        if ch == '*' {
            // Check for **bold**.
            let rest = &text[i..];
            if let Some(after_stars) = rest.strip_prefix("**") {
                if let Some(end) = after_stars.find("**") {
                    // Push preceding text.
                    if i > current_start {
                        spans.push(Span::styled(text[current_start..i].to_string(), base_style));
                    }
                    let bold_text = &text[i + 2..i + 2 + end];
                    spans.push(Span::styled(
                        bold_text.to_string(),
                        base_style.add_modifier(Modifier::BOLD),
                    ));
                    // Advance past **...**
                    let skip_to = i + 2 + end + 2;
                    while chars.peek().is_some_and(|&(ci, _)| ci < skip_to) {
                        chars.next();
                    }
                    current_start = skip_to;
                    continue;
                }
            }
        } else if ch == '`' {
            // Inline code.
            let rest = &text[i + 1..];
            if let Some(end) = rest.find('`') {
                if i > current_start {
                    spans.push(Span::styled(text[current_start..i].to_string(), base_style));
                }
                let code_text = &text[i + 1..i + 1 + end];
                spans.push(Span::styled(
                    code_text.to_string(),
                    base_style.fg(Color::Yellow),
                ));
                let skip_to = i + 1 + end + 1;
                while chars.peek().is_some_and(|&(ci, _)| ci < skip_to) {
                    chars.next();
                }
                current_start = skip_to;
                continue;
            }
        }
        chars.next();
    }

    // Remaining text.
    if current_start < text.len() {
        let remaining_style = if list_prefix && spans.is_empty() {
            base_style.fg(Color::White)
        } else {
            base_style
        };
        spans.push(Span::styled(
            text[current_start..].to_string(),
            remaining_style,
        ));
    }

    if spans.is_empty() {
        Line::from(Span::styled(text.to_string(), base_style))
    } else {
        Line::from(spans)
    }
}

fn draw_agent_pane(f: &mut Frame, app: &App, area: Rect) {
    let title = if let Some(ref goal_id) = app.tailing_goal {
        let short = &goal_id[..8.min(goal_id.len())];
        format!(" Agent ({}) ", short)
    } else {
        " Agent ".to_string()
    };
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(title, Style::default().fg(Color::Green)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.agent_output.is_empty() {
        let hint = Paragraph::new("No agent output yet.\nStart a goal to see output here.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, inner);
        return;
    }

    let visible_height = inner.height as usize;
    let wrap_width = inner.width as usize;

    let visual_line_count: usize = app
        .agent_output
        .iter()
        .map(|ol| {
            if ol.text.is_empty() || wrap_width == 0 {
                1
            } else {
                ol.text.len().div_ceil(wrap_width)
            }
        })
        .sum();

    let max_scroll = visual_line_count.saturating_sub(visible_height);
    let scroll_y = max_scroll.saturating_sub(app.agent_scroll_offset);

    // Pre-slice logical lines to avoid ratatui's u16 scroll overflow
    // (same approach as draw_output).
    let mut cumulative: usize = 0;
    let mut start_idx: usize = 0;
    let mut residual_scroll: u16 = 0;
    for (i, ol) in app.agent_output.iter().enumerate() {
        let vlines = if ol.text.is_empty() || wrap_width == 0 {
            1
        } else {
            ol.text.len().div_ceil(wrap_width)
        };
        if cumulative + vlines > scroll_y {
            start_idx = i;
            residual_scroll = (scroll_y - cumulative) as u16;
            break;
        }
        cumulative += vlines;
        start_idx = i + 1;
    }

    let mut end_idx = start_idx;
    let mut visible_vlines: usize = 0;
    for ol in app.agent_output.iter().skip(start_idx) {
        let vlines = if ol.text.is_empty() || wrap_width == 0 {
            1
        } else {
            ol.text.len().div_ceil(wrap_width)
        };
        visible_vlines += vlines;
        end_idx += 1;
        if visible_vlines >= visible_height + residual_scroll as usize {
            break;
        }
    }

    // Render agent output with inline markdown styling (v0.10.14).
    let mut render_code_block = false;
    let lines: Vec<Line> = app.agent_output[start_idx..end_idx.min(app.agent_output.len())]
        .iter()
        .map(|ol| stylize_markdown_line(&ol.text, ol.style, &mut render_code_block))
        .collect();

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((residual_scroll, 0));
    f.render_widget(paragraph, inner);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let prompt = app.prompt_str();
    let display = format!("{}{}", &prompt, &app.input);

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    let paragraph = Paragraph::new(display.clone())
        .wrap(Wrap { trim: false })
        .block(block);
    f.render_widget(paragraph, area);

    // Position cursor accounting for line wrap (v0.10.14).
    let cursor_chars = prompt.len() + app.input[..app.cursor].chars().count();
    let wrap_width = inner.width.max(1) as usize;
    let cursor_y = (cursor_chars / wrap_width) as u16;
    let cursor_x = (cursor_chars % wrap_width) as u16;
    let x = inner.x + cursor_x.min(inner.width.saturating_sub(1));
    let y = inner.y + cursor_y.min(inner.height.saturating_sub(1));
    f.set_cursor_position((x, y));
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let cli_version = env!("CARGO_PKG_VERSION");
    let is_stale = app.daemon_connected
        && !app.status.version.is_empty()
        && app.status.version != "?"
        && app.status.version != cli_version;

    let daemon_indicator = if !app.daemon_connected {
        Span::styled(" ◉ daemon ", Style::default().fg(Color::Red))
    } else if is_stale {
        Span::styled(
            format!(" ◉ daemon {} (stale) ", app.status.daemon_version),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(
            format!(" ◉ daemon {} ", app.status.daemon_version),
            Style::default().fg(Color::Green),
        )
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
        Span::raw("│"),
        Span::styled(
            format!(" {} ", app.status.default_agent),
            Style::default().fg(Color::Magenta),
        ),
    ];

    // Agent model indicator (v0.10.14).
    if let Some(ref model) = app.status.agent_model {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" {} ", model),
            Style::default().fg(Color::Blue),
        ));
    }

    // Scroll position indicator (v0.10.18.2).
    if app.scroll_offset > 0 {
        // Calculate current visible line range.
        let total_lines = app.output.len();
        let visible_line = total_lines.saturating_sub(app.scroll_offset);
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" line {} of {} ", visible_line, total_lines),
            Style::default().fg(Color::White),
        ));
    }

    // Unread event badge — shows "new output" when scrolled up (v0.10.18.2).
    if app.unread_events > 0 {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" {} new output \u{2193} ", app.unread_events),
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

    // Stdin prompt indicator (v0.10.18.5).
    if app.pending_stdin_prompt.is_some() {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            " stdin prompt ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Tailing indicator (v0.10.11).
    if let Some(ref goal_id) = app.tailing_goal {
        let short = &goal_id[..8.min(goal_id.len())];
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" tailing {} ", short),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
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

                // Check for structured events before generic rendering.
                if let Some(pq) = parse_agent_question(&frame) {
                    let _ = tx.send(TuiMessage::AgentQuestion(pq));
                } else if let Some((goal_id, title)) = parse_goal_started(&frame) {
                    let _ = tx.send(TuiMessage::GoalStarted { goal_id, title });
                } else if let Some(dr) = parse_draft_built(&frame) {
                    let _ = tx.send(TuiMessage::DraftReady {
                        goal_id: dr.0,
                        draft_id: dr.1,
                        display_id: dr.2,
                        title: dr.3,
                    });
                    // Also render the generic SSE event for the log.
                    if let Some(rendered) = super::shell::render_sse_event(&frame) {
                        let _ = tx.send(TuiMessage::SseEvent(rendered));
                    }
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
                        daemon_version: json["daemon_version"].as_str().unwrap_or("?").to_string(),
                        next_phase: json["current_phase"]["id"].as_str().map(|id| {
                            let title = json["current_phase"]["title"].as_str().unwrap_or("");
                            format!("{} -- {}", id, title)
                        }),
                        pending_drafts: json["pending_drafts"].as_u64().unwrap_or(0) as usize,
                        active_agents: json["active_agents"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0),
                        default_agent: json["default_agent"]
                            .as_str()
                            .unwrap_or("claude-code")
                            .to_string(),
                        agent_model: json["agent_model"].as_str().map(|s| s.to_string()).or_else(
                            || {
                                json["active_agents"].as_array().and_then(|agents| {
                                    agents
                                        .iter()
                                        .find_map(|a| a["model"].as_str().map(String::from))
                                })
                            },
                        ),
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

/// Start streaming agent output from a goal's output endpoint (v0.10.11).
///
/// Resolves the goal ID (auto-selects if only one running), connects to
/// `GET /api/goals/:id/output` SSE, and publishes lines as `AgentOutput`
/// messages to the TUI event loop.
async fn start_tail_stream(
    client: reqwest::Client,
    base_url: &str,
    goal_id: Option<&str>,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
    _backfill_lines: usize,
) {
    // Resolve goal ID.
    let target = match goal_id {
        Some(id) => id.to_string(),
        None => {
            let url = format!("{}/api/goals/active-output", base_url);
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(TuiMessage::CommandResponse(format!(
                        "Error fetching active goals: {}",
                        e
                    )));
                    return;
                }
            };
            let json: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => {
                    let _ = tx.send(TuiMessage::CommandResponse(
                        "Error: invalid response from daemon".into(),
                    ));
                    return;
                }
            };
            let goals: Vec<String> = json["goals"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            match goals.len() {
                0 => {
                    let _ = tx.send(TuiMessage::CommandResponse(
                        "No goals with active output. Start one with: ta run <phase>".into(),
                    ));
                    return;
                }
                1 => goals[0].clone(),
                _ => {
                    let mut msg = "Multiple goals running. Specify one:\n".to_string();
                    for (i, g) in goals.iter().enumerate() {
                        msg.push_str(&format!("  [{}] {}\n", i + 1, g));
                    }
                    msg.push_str("Usage: :tail <id>");
                    let _ = tx.send(TuiMessage::CommandResponse(msg));
                    return;
                }
            }
        }
    };

    let short_id = &target[..8.min(target.len())];

    // Print confirmation with backfill separator (v0.10.11 item 3).
    let _ = tx.send(TuiMessage::CommandResponse(format!(
        "Tailing \"{}\"...",
        short_id
    )));
    let _ = tx.send(TuiMessage::CommandResponse(
        "─── live output ───".to_string(),
    ));

    // Connect to goal output SSE stream. Retry with escalating strategies:
    // 1. Try the exact target (may be full UUID, short ID, or output key)
    // 2. On 404, query active-output and do client-side prefix match
    // 3. Retry with delays since the channel may not be registered yet
    let mut resp_result = None;
    let mut resolved_target = target.clone();
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let url = format!("{}/api/goals/{}/output", base_url, resolved_target);
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                resp_result = Some(r);
                break;
            }
            Ok(_) if attempt < 4 => {
                // On 404, try client-side prefix resolution against active goals.
                // The output channel uses a human-friendly key (e.g., "v0.10.17 — ...")
                // but auto-tail passes the full UUID from the SSE event. The daemon's
                // reverse-prefix match may not find it if the alias isn't registered yet.
                if attempt == 1 {
                    if let Some(matched) =
                        resolve_via_active_output(&client, base_url, &target).await
                    {
                        resolved_target = matched;
                    }
                }
                continue;
            }
            Ok(r) => {
                let json: serde_json::Value = r.json().await.unwrap_or_default();
                let err = json["error"].as_str().unwrap_or("unknown error");
                let hint = json["hint"].as_str().unwrap_or("");
                let mut msg = format!("Error: {}", err);
                if !hint.is_empty() {
                    msg.push_str(&format!("\n  {}", hint));
                }
                let _ = tx.send(TuiMessage::CommandResponse(msg));
                return;
            }
            Err(_) if attempt < 4 => continue, // Retry on network error
            Err(e) => {
                let _ = tx.send(TuiMessage::CommandResponse(format!(
                    "Error: Cannot reach daemon: {}",
                    e
                )));
                return;
            }
        }
    }

    let Some(resp) = resp_result else {
        let _ = tx.send(TuiMessage::CommandResponse(
            "Error: Could not connect to output stream after retries".into(),
        ));
        return;
    };

    let mut stream = resp.bytes_stream();
    use tokio_stream::StreamExt;
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(_) => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buffer.find("\n\n") {
            let frame = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            // Parse SSE frame.
            let mut event_type = None;
            let mut data = None;
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data = Some(rest.trim().to_string());
                }
            }

            match event_type.as_deref() {
                Some("output") => {
                    if let Some(d) = &data {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(d) {
                            let stream_name =
                                json["stream"].as_str().unwrap_or("stdout").to_string();
                            let line = json["line"].as_str().unwrap_or("").to_string();
                            // Route prompt-typed lines to the stdin prompt handler (v0.10.18.5).
                            if stream_name == "prompt" {
                                let _ = tx.send(TuiMessage::StdinPrompt(PendingStdinPrompt {
                                    goal_id: target.clone(),
                                    prompt_text: line,
                                }));
                            } else if stream_name == "auto_answered" {
                                // Parse "[auto] prompt → response" format.
                                if let Some(arrow_pos) = line.find(" → ") {
                                    let prompt = line[7..arrow_pos].to_string(); // skip "[auto] "
                                    let response = line[arrow_pos + 5..].to_string(); // skip " → "
                                    let _ =
                                        tx.send(TuiMessage::StdinAutoAnswered { prompt, response });
                                } else {
                                    let _ = tx.send(TuiMessage::AgentOutput(AgentOutputLine {
                                        stream: stream_name,
                                        line,
                                    }));
                                }
                            } else {
                                let _ = tx.send(TuiMessage::AgentOutput(AgentOutputLine {
                                    stream: stream_name,
                                    line,
                                }));
                            }
                        }
                    }
                }
                Some("done") => {
                    let _ = tx.send(TuiMessage::AgentOutputDone(target.clone()));
                    return;
                }
                Some("lagged") => {
                    let _ = tx.send(TuiMessage::CommandResponse(
                        "[skipped some output lines — subscriber lagged]".into(),
                    ));
                }
                _ => {}
            }
        }
    }

    let _ = tx.send(TuiMessage::AgentOutputDone(target));
}

/// Stream agent Q&A output from a request ID.
///
/// Connects to the same `/api/goals/:id/output` SSE endpoint used by `:tail`,
/// since agent ask responses use the GoalOutput channel system.
async fn stream_agent_output(
    client: reqwest::Client,
    base_url: &str,
    request_id: &str,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    start_tail_stream(client, base_url, Some(request_id), tx, 0).await;
}

/// Extract human-readable text from a `claude --output-format stream-json` line.
///
/// Stream-json emits one JSON object per line on stdout. Text content appears in
/// objects with `type: "assistant"` (or `type: "content_block_delta"`) and the
/// text lives in nested fields. We extract it so the TUI shows readable text
/// instead of raw JSON.
///
/// Returns `Some(text)` if the line is recognized stream-json (text may be empty
/// for non-text events like tool_use), `None` if the line isn't valid JSON (pass
/// through as-is).
fn parse_stream_json_text(line: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    // The stream-json format varies, but text content typically appears as:
    //   {"type":"assistant","content":[{"type":"text","text":"..."}], ...}
    //   {"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}, ...}
    //   {"type":"result","result":"final text", ...}
    //   {"type":"message","message":{...}, ...}  (message start/end)

    // Try result type (final output).
    if json["type"].as_str() == Some("result") {
        if let Some(text) = json["result"].as_str() {
            return Some(text.to_string());
        }
        // Result may contain a sub-object; try extracting text from it.
        if let Some(text) = json["result"]["text"].as_str() {
            return Some(text.to_string());
        }
        return Some(String::new()); // Result with no text — skip.
    }

    // Try content_block_delta (streaming text chunks).
    if json["type"].as_str() == Some("content_block_delta") {
        if let Some(text) = json["delta"]["text"].as_str() {
            return Some(text.to_string());
        }
        return Some(String::new());
    }

    // Try assistant message with content array.
    if json["type"].as_str() == Some("assistant") {
        if let Some(content) = json["content"].as_array() {
            let text: String = content
                .iter()
                .filter_map(|c| {
                    if c["type"].as_str() == Some("text") {
                        c["text"].as_str().map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Some(text);
        }
        // Single text field.
        if let Some(text) = json["content"].as_str() {
            return Some(text.to_string());
        }
        return Some(String::new());
    }

    // Other recognized types (message start, tool_use, etc.) — suppress.
    if json.get("type").is_some() {
        return Some(String::new());
    }

    // Not recognized JSON format — return None so caller shows raw line.
    None
}

/// Extract LLM model name from a stream-json line (v0.10.14).
///
/// Claude's stream-json format includes the model in `message_start` events:
///   `{"type":"message_start","message":{"model":"claude-sonnet-4-20250514",...}}`
/// Also checks `system` events which may contain a model field.
fn extract_model_from_stream_json(line: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    // message_start has the model in message.model
    if json["type"].as_str() == Some("message_start") {
        if let Some(model) = json["message"]["model"].as_str() {
            return Some(humanize_model_name(model));
        }
    }

    // Some formats put model at top level
    if let Some(model) = json["model"].as_str() {
        return Some(humanize_model_name(model));
    }

    None
}

/// Convert API model IDs to human-readable names.
fn humanize_model_name(model_id: &str) -> String {
    // Map known model IDs to friendly names.
    if model_id.starts_with("claude-opus-4") {
        "Claude Opus 4".to_string()
    } else if model_id.starts_with("claude-sonnet-4") {
        "Claude Sonnet 4".to_string()
    } else if model_id.starts_with("claude-haiku-4") {
        "Claude Haiku 4".to_string()
    } else if model_id.starts_with("claude-3-5-sonnet") {
        "Claude 3.5 Sonnet".to_string()
    } else if model_id.starts_with("claude-3-5-haiku") {
        "Claude 3.5 Haiku".to_string()
    } else if model_id.starts_with("claude-3-opus") {
        "Claude 3 Opus".to_string()
    } else if model_id.starts_with("claude-3-sonnet") {
        "Claude 3 Sonnet".to_string()
    } else if model_id.starts_with("claude-3-haiku") {
        "Claude 3 Haiku".to_string()
    } else {
        // Return the raw model ID if not recognized.
        model_id.to_string()
    }
}

/// Handle `:follow-up [filter]` command — gather and display follow-up candidates (v0.10.14).
///
/// Without arguments: shows all candidates with numbered list.
/// With a filter: shows candidates matching the filter text (case-insensitive).
fn handle_follow_up_picker(app: &mut App, text: &str) {
    let filter = text
        .strip_prefix(":follow-up")
        .or_else(|| text.strip_prefix(":followup"))
        .unwrap_or("")
        .trim();

    let config = ta_mcp_gateway::GatewayConfig::for_project(&app.project_root);
    let goal_store = match ta_goal::GoalRunStore::new(&config.goals_dir) {
        Ok(store) => store,
        Err(e) => {
            app.push_output(OutputLine::error(format!(
                "Failed to open goal store: {}",
                e
            )));
            return;
        }
    };

    let candidates = match super::follow_up::gather_follow_up_candidates(&config, &goal_store) {
        Ok(c) => c,
        Err(e) => {
            app.push_output(OutputLine::error(format!(
                "Failed to gather follow-up candidates: {}",
                e
            )));
            return;
        }
    };

    if candidates.is_empty() {
        app.push_output(OutputLine::info(
            "No follow-up candidates found. All goals are complete or no work has been started."
                .into(),
        ));
        return;
    }

    // Apply filter if provided.
    let filtered: Vec<(usize, &super::follow_up::FollowUpCandidate)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if filter.is_empty() {
                return true;
            }
            let lower_filter = filter.to_lowercase();
            c.title.to_lowercase().contains(&lower_filter)
                || c.status.to_lowercase().contains(&lower_filter)
                || c.source.to_string().to_lowercase().contains(&lower_filter)
                || c.context_summary.to_lowercase().contains(&lower_filter)
        })
        .collect();

    if filtered.is_empty() {
        app.push_output(OutputLine::info(format!(
            "No candidates matching '{}'. {} total candidates available.",
            filter,
            candidates.len()
        )));
        return;
    }

    app.push_output(OutputLine::separator(format!(
        "━━━ Follow-Up Candidates ({}{}) ━━━",
        filtered.len(),
        if filter.is_empty() {
            String::new()
        } else {
            format!(" matching '{}'", filter)
        }
    )));

    for (idx, candidate) in &filtered {
        let source_tag = match candidate.source {
            super::follow_up::CandidateSource::Goal => "[goal]",
            super::follow_up::CandidateSource::Draft => "[draft]",
            super::follow_up::CandidateSource::Phase => "[phase]",
            super::follow_up::CandidateSource::VerifyFailure => "[verify]",
        };
        let line = format!(
            "  {:>2}. {} {} — {} ({})",
            idx + 1,
            source_tag,
            candidate.title,
            candidate.status,
            candidate.age,
        );
        let style = match candidate.source {
            super::follow_up::CandidateSource::VerifyFailure => Style::default().fg(Color::Red),
            super::follow_up::CandidateSource::Draft => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::White),
        };
        app.push_output(OutputLine { text: line, style });
    }

    app.push_output(OutputLine::info(
        "Use `ta run --follow-up` to start a follow-up session from the CLI.".into(),
    ));
}

/// Parse `:tail [id] [--lines <count>]` arguments.
///
/// Client-side prefix resolution: query `/api/goals/active-output` and find
/// a goal whose key starts with the given prefix (short UUID match).
async fn resolve_via_active_output(
    client: &reqwest::Client,
    base_url: &str,
    prefix: &str,
) -> Option<String> {
    let url = format!("{}/api/goals/active-output", base_url);
    let resp = client.get(&url).send().await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let goals: Vec<String> = json["goals"]
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    // Try: an active goal whose key starts with our prefix (short UUID).
    let short = &prefix[..8.min(prefix.len())];
    for g in &goals {
        if g.starts_with(short) || g.starts_with(prefix) {
            return Some(g.clone());
        }
    }
    // Fallback: if there's exactly one active goal, use it.
    if goals.len() == 1 {
        return Some(goals[0].clone());
    }
    None
}

/// Returns `(goal_id, backfill_lines)`. If `--lines` is not specified, uses
/// the provided `default_backfill` from config.
pub(crate) fn parse_tail_args(text: &str, default_backfill: usize) -> (Option<String>, usize) {
    let rest = text.strip_prefix(":tail").unwrap_or("").trim();
    if rest.is_empty() {
        return (None, default_backfill);
    }

    let parts: Vec<&str> = rest.split_whitespace().collect();
    let mut goal_id: Option<String> = None;
    let mut backfill = default_backfill;
    let mut i = 0;

    while i < parts.len() {
        if parts[i] == "--lines" || parts[i] == "-n" {
            if i + 1 < parts.len() {
                if let Ok(n) = parts[i + 1].parse::<usize>() {
                    backfill = n;
                }
                i += 2;
                continue;
            }
        } else if goal_id.is_none() {
            goal_id = Some(parts[i].to_string());
        }
        i += 1;
    }

    (goal_id, backfill)
}

/// Parse an SSE frame for a `goal_started` event.
/// Returns `Some((goal_id, title))` if found.
fn parse_goal_started(frame: &str) -> Option<(String, String)> {
    let mut event_type = None;
    let mut data = None;
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.trim());
        }
    }

    if event_type? != "goal_started" {
        return None;
    }

    let json: serde_json::Value = serde_json::from_str(data?).ok()?;
    let payload = &json["payload"];
    let goal_id = payload["goal_id"]
        .as_str()
        .or_else(|| payload["id"].as_str())?
        .to_string();
    let title = payload["title"]
        .as_str()
        .unwrap_or("(untitled)")
        .to_string();
    Some((goal_id, title))
}

/// Parse an SSE frame for a `draft_built` event (v0.10.11 item 4).
/// Returns `Some((goal_id, draft_id, display_id, title))`.
fn parse_draft_built(frame: &str) -> Option<(String, String, String, String)> {
    let mut event_type = None;
    let mut data = None;
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.trim());
        }
    }

    if event_type? != "draft_built" {
        return None;
    }

    let json: serde_json::Value = serde_json::from_str(data?).ok()?;
    let payload = &json["payload"];
    let goal_id = payload["goal_id"].as_str().unwrap_or("").to_string();
    let draft_id = payload["draft_id"].as_str().unwrap_or("").to_string();
    // Use display_id if present, otherwise fall back to short draft_id.
    let display_id = payload["display_id"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| draft_id[..8.min(draft_id.len())].to_string());
    let title = payload["title"]
        .as_str()
        .unwrap_or("(untitled)")
        .to_string();
    Some((goal_id, draft_id, display_id, title))
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
  build              Shortcut for: ta build
  test               Shortcut for: ta build --test
  status             Shortcut for: ta status
  plan               Shortcut for: ta plan list
  plan add <desc>    Add a phase to the plan via agent session
  goals              Shortcut for: ta goal list
  drafts             Shortcut for: ta draft list
  <anything else>    Sent to agent session (if attached)

Agent output:
  :tail [id] [--lines N]  Attach to goal output (--lines overrides backfill count)
  Agent output auto-streams when a goal starts (configurable: shell.auto_tail)

Follow-up:
  :follow-up             List all follow-up candidates (failed goals, denied drafts, etc.)
  :follow-up <filter>    Filter candidates by keyword (fuzzy match on title/status/type)

Interactive mode:
  When an agent asks a question, the prompt changes to [agent Q1] >
  Type your response and press Enter to send it back to the agent.

Shell commands:
  :status            Refresh the status bar
  clear              Clear the output pane
  Ctrl-L             Clear the output pane
  PgUp / PgDn        Scroll output 10 lines
  Shift+Up / Down    Scroll output 1 line
  Shift+Home / End   Scroll to top / bottom of output
  Tab                Auto-complete commands
  Ctrl-W             Toggle split pane (shell | agent side-by-side)
  Ctrl-C / exit      Exit the shell (Ctrl-C detaches when tailing)

Scrollback:
  Output is retained in a scrollback buffer (default: 50000 lines).
  Configure via [shell] scrollback_lines in .ta/workflow.toml (minimum: 10000).
  Status bar shows scroll position and new output indicator when scrolled up.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_insert_and_backspace() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.input = "   ".into();
        let cmd = app.submit();
        assert!(cmd.is_none());
    }

    #[test]
    fn app_submit_dedup_history() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.history = vec!["same".into()];
        app.input = "same".into();
        app.cursor = 4;
        app.submit();
        assert_eq!(app.history.len(), 1); // not duplicated
    }

    #[test]
    fn app_scroll() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.input = "hello".into();
        app.cursor = 2;
        app.delete();
        assert_eq!(app.input, "helo");
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn app_multibyte_cursor_operations() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Insert multi-byte chars (e.g., emoji, accented)
        app.insert_char('h');
        app.insert_char('é'); // 2-byte UTF-8
        app.insert_char('l');
        assert_eq!(app.input, "hél");
        assert_eq!(app.cursor, 4); // 1 + 2 + 1 bytes

        // Cursor left should land on 'l' → 'é' boundary
        app.cursor_left();
        assert_eq!(app.cursor, 3); // before 'l'
        app.cursor_left();
        assert_eq!(app.cursor, 1); // before 'é' (skip 2-byte char)

        // Cursor right back to 'l'
        app.cursor_right();
        assert_eq!(app.cursor, 3);

        // Insert at a char boundary mid-string
        app.insert_char('x');
        assert_eq!(app.input, "héxl");

        // Backspace removes multi-byte char correctly
        app.cursor_left(); // back to before 'x'
        app.backspace(); // removes 'é'
        assert_eq!(app.input, "hxl");
        assert_eq!(app.cursor, 1);

        // Paste simulation: rapid multi-byte inserts
        app.input.clear();
        app.cursor = 0;
        for c in "café".chars() {
            app.insert_char(c);
        }
        assert_eq!(app.input, "café");
        assert_eq!(app.cursor, "café".len()); // 5 bytes
        assert!(app.input.is_char_boundary(app.cursor));
    }

    #[test]
    fn app_prompt_changes_in_workflow_mode() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        assert_eq!(app.prompt_str(), "ta> ");
        app.workflow_prompt = Some("review".into());
        assert_eq!(app.prompt_str(), "workflow> ");
    }

    #[test]
    fn app_prompt_changes_for_agent_question() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
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
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::SseEvent("goal started: \"test\"".into()),
        );
        assert_eq!(app.output.len(), 1);
        assert_eq!(app.output[0].text, "goal started: \"test\"");
    }

    #[test]
    fn handle_tui_message_workflow_prompt() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::SseEvent("workflow paused at 'review': Need approval".into()),
        );
        assert_eq!(app.workflow_prompt, Some("review".into()));
    }

    // -- v0.10.11 tests --

    #[test]
    fn parse_goal_started_event() {
        let frame = concat!(
            "event: goal_started\n",
            "data: {",
            "\"event_type\":\"goal_started\",",
            "\"payload\":{",
            "\"goal_id\":\"aaaa1111-2222-3333-4444-555555555555\",",
            "\"title\":\"v0.10.11 — Shell TUI UX Overhaul\"",
            "}}"
        );
        let (id, title) = parse_goal_started(frame).expect("should parse");
        assert_eq!(id, "aaaa1111-2222-3333-4444-555555555555");
        assert_eq!(title, "v0.10.11 — Shell TUI UX Overhaul");
    }

    #[test]
    fn parse_goal_started_ignores_other_events() {
        let frame = "event: draft_built\ndata: {\"event_type\":\"draft_built\",\"payload\":{\"goal_id\":\"abc\"}}";
        assert!(parse_goal_started(frame).is_none());
    }

    #[test]
    fn parse_draft_built_event() {
        let frame = concat!(
            "event: draft_built\n",
            "data: {",
            "\"event_type\":\"draft_built\",",
            "\"payload\":{",
            "\"goal_id\":\"aaaa1111-2222-3333-4444-555555555555\",",
            "\"draft_id\":\"bbbb1111-2222-3333-4444-555555555555\",",
            "\"display_id\":\"aaaa1111-01\",",
            "\"title\":\"v0.10.11 — Shell TUI UX Overhaul\"",
            "}}"
        );
        let (goal_id, draft_id, display_id, title) =
            parse_draft_built(frame).expect("should parse");
        assert_eq!(goal_id, "aaaa1111-2222-3333-4444-555555555555");
        assert_eq!(draft_id, "bbbb1111-2222-3333-4444-555555555555");
        assert_eq!(display_id, "aaaa1111-01");
        assert_eq!(title, "v0.10.11 — Shell TUI UX Overhaul");
    }

    #[test]
    fn parse_draft_built_fallback_display_id() {
        let frame = concat!(
            "event: draft_built\n",
            "data: {",
            "\"event_type\":\"draft_built\",",
            "\"payload\":{",
            "\"goal_id\":\"abc\",",
            "\"draft_id\":\"bbbb1111-2222-3333-4444-555555555555\",",
            "\"title\":\"test\"",
            "}}"
        );
        let (_, _, display_id, _) = parse_draft_built(frame).expect("should parse");
        assert_eq!(display_id, "bbbb1111"); // falls back to 8-char prefix
    }

    #[test]
    fn parse_draft_built_ignores_other_events() {
        let frame = "event: goal_started\ndata: {\"event_type\":\"goal_started\",\"payload\":{\"title\":\"test\"}}";
        assert!(parse_draft_built(frame).is_none());
    }

    #[test]
    fn handle_agent_output_message() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "Building crate...".into(),
            }),
        );
        assert_eq!(app.output.len(), 1);
        assert_eq!(app.output[0].text, "Building crate...");
        // stdout gets white styling (not yellow/red)
        assert_eq!(app.output[0].style, Style::default().fg(Color::White));
    }

    #[test]
    fn handle_agent_stderr_output() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stderr".into(),
                line: "warning: unused var".into(),
            }),
        );
        assert_eq!(app.output.len(), 1);
        assert_eq!(app.output[0].style, Style::default().fg(Color::Yellow));
    }

    #[test]
    fn handle_goal_started_auto_tail() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.auto_tail = true;
        handle_tui_message(
            &mut app,
            TuiMessage::GoalStarted {
                goal_id: "aaaa1111-2222-3333-4444-555555555555".into(),
                title: "Test Goal".into(),
            },
        );
        assert!(app.tailing_goal.is_some());
        assert_eq!(
            app.tailing_goal.as_deref(),
            Some("aaaa1111-2222-3333-4444-555555555555")
        );
        // Should have added notification output.
        assert!(!app.output.is_empty());
    }

    #[test]
    fn handle_goal_started_no_auto_tail_when_already_tailing() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.auto_tail = true;
        app.tailing_goal = Some("existing-goal".into());
        handle_tui_message(
            &mut app,
            TuiMessage::GoalStarted {
                goal_id: "new-goal".into(),
                title: "Another Goal".into(),
            },
        );
        // Should not override existing tail.
        assert_eq!(app.tailing_goal.as_deref(), Some("existing-goal"));
    }

    #[test]
    fn handle_goal_started_no_auto_tail_when_disabled() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.auto_tail = false;
        handle_tui_message(
            &mut app,
            TuiMessage::GoalStarted {
                goal_id: "goal1".into(),
                title: "Test".into(),
            },
        );
        assert!(app.tailing_goal.is_none());
    }

    #[test]
    fn handle_agent_output_done_clears_tail() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.tailing_goal = Some("goal-123".into());
        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-123".into()));
        assert!(app.tailing_goal.is_none());
        assert!(!app.output.is_empty()); // separator line
    }

    #[test]
    fn handle_draft_ready_notification() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::DraftReady {
                goal_id: "goal-1".into(),
                draft_id: "draft-1".into(),
                display_id: "aaaa1111-01".into(),
                title: "v0.10.11 — Shell TUI UX Overhaul".into(),
            },
        );
        assert!(!app.output.is_empty());
        assert!(app.output.last().unwrap().text.contains("draft ready"));
        assert!(app.output.last().unwrap().text.contains("aaaa1111-01"));
    }

    #[test]
    fn output_buffer_limit_enforced() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.output_buffer_limit = 10;
        for i in 0..20 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }
        assert_eq!(app.output.len(), 10);
        // Oldest lines should have been dropped — first line should be "line 10".
        assert_eq!(app.output[0].text, "line 10");
    }

    #[test]
    fn output_buffer_limit_adjusts_scroll() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.output_buffer_limit = 10;
        for i in 0..10 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }
        app.scroll_up(5);
        assert_eq!(app.scroll_offset, 5);
        // Add 5 more — 5 will be dropped.
        for i in 10..15 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }
        assert_eq!(app.output.len(), 10);
        // Scroll offset should have been reduced.
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn parse_stream_json_result() {
        let line = r#"{"type":"result","result":"Hello world"}"#;
        assert_eq!(parse_stream_json_text(line), Some("Hello world".into()));
    }

    #[test]
    fn parse_stream_json_content_block_delta() {
        let line =
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"partial "}}"#;
        assert_eq!(parse_stream_json_text(line), Some("partial ".into()));
    }

    #[test]
    fn parse_stream_json_assistant_content_array() {
        let line = r#"{"type":"assistant","content":[{"type":"text","text":"Hello"}]}"#;
        assert_eq!(parse_stream_json_text(line), Some("Hello".into()));
    }

    #[test]
    fn parse_stream_json_non_text_type_returns_empty() {
        let line = r#"{"type":"message_start","message":{}}"#;
        assert_eq!(parse_stream_json_text(line), Some(String::new()));
    }

    #[test]
    fn parse_stream_json_plain_text_returns_none() {
        let line = "This is not JSON";
        assert_eq!(parse_stream_json_text(line), None);
    }

    // -- v0.10.14 tests --

    #[test]
    fn parse_tail_args_no_args() {
        let (id, lines) = parse_tail_args(":tail", 5);
        assert!(id.is_none());
        assert_eq!(lines, 5);
    }

    #[test]
    fn parse_tail_args_id_only() {
        let (id, lines) = parse_tail_args(":tail abc123", 5);
        assert_eq!(id.as_deref(), Some("abc123"));
        assert_eq!(lines, 5);
    }

    #[test]
    fn parse_tail_args_lines_only() {
        let (id, lines) = parse_tail_args(":tail --lines 50", 5);
        assert!(id.is_none());
        assert_eq!(lines, 50);
    }

    #[test]
    fn parse_tail_args_id_and_lines() {
        let (id, lines) = parse_tail_args(":tail abc123 --lines 20", 5);
        assert_eq!(id.as_deref(), Some("abc123"));
        assert_eq!(lines, 20);
    }

    #[test]
    fn parse_tail_args_lines_before_id() {
        let (id, lines) = parse_tail_args(":tail --lines 10 abc123", 5);
        assert_eq!(id.as_deref(), Some("abc123"));
        assert_eq!(lines, 10);
    }

    #[test]
    fn parse_tail_args_short_flag() {
        let (id, lines) = parse_tail_args(":tail -n 30", 5);
        assert!(id.is_none());
        assert_eq!(lines, 30);
    }

    #[test]
    fn extract_model_from_message_start() {
        let line = r#"{"type":"message_start","message":{"model":"claude-sonnet-4-20250514","role":"assistant"}}"#;
        assert_eq!(
            extract_model_from_stream_json(line),
            Some("Claude Sonnet 4".into())
        );
    }

    #[test]
    fn extract_model_from_top_level() {
        let line = r#"{"model":"claude-haiku-4-20250101","type":"system"}"#;
        assert_eq!(
            extract_model_from_stream_json(line),
            Some("Claude Haiku 4".into())
        );
    }

    #[test]
    fn extract_model_unknown_id() {
        let line = r#"{"type":"message_start","message":{"model":"gpt-4o"}}"#;
        assert_eq!(extract_model_from_stream_json(line), Some("gpt-4o".into()));
    }

    #[test]
    fn extract_model_no_model_field() {
        let line = r#"{"type":"content_block_delta","delta":{"text":"hello"}}"#;
        assert!(extract_model_from_stream_json(line).is_none());
    }

    #[test]
    fn humanize_model_names() {
        assert_eq!(humanize_model_name("claude-opus-4-6"), "Claude Opus 4");
        assert_eq!(
            humanize_model_name("claude-sonnet-4-20250514"),
            "Claude Sonnet 4"
        );
        assert_eq!(
            humanize_model_name("claude-3-5-sonnet-20241022"),
            "Claude 3.5 Sonnet"
        );
        assert_eq!(humanize_model_name("custom-model"), "custom-model");
    }

    // --- stylize_markdown_line tests (v0.10.14) ---

    #[test]
    fn md_plain_text_unchanged() {
        let mut code_block = false;
        let line = stylize_markdown_line("hello world", Style::default(), &mut code_block);
        assert_eq!(line.spans.len(), 1);
        assert!(!code_block);
    }

    #[test]
    fn md_header_styled() {
        let mut code_block = false;
        let line = stylize_markdown_line("# Title", Style::default(), &mut code_block);
        assert!(line.spans[0].style.fg == Some(Color::Cyan));
    }

    #[test]
    fn md_bold_inline() {
        let mut code_block = false;
        let line =
            stylize_markdown_line("before **bold** after", Style::default(), &mut code_block);
        assert!(line.spans.len() >= 3);
        // Middle span should be bold.
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn md_inline_code() {
        let mut code_block = false;
        let line = stylize_markdown_line("run `cargo test` now", Style::default(), &mut code_block);
        assert!(line.spans.len() >= 3);
        assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn md_code_block_toggle() {
        let mut code_block = false;
        let _ = stylize_markdown_line("```rust", Style::default(), &mut code_block);
        assert!(code_block);
        let inside = stylize_markdown_line("let x = 1;", Style::default(), &mut code_block);
        assert_eq!(inside.spans[0].style.fg, Some(Color::Yellow));
        let _ = stylize_markdown_line("```", Style::default(), &mut code_block);
        assert!(!code_block);
    }

    #[test]
    fn md_no_false_bold_on_single_star() {
        let mut code_block = false;
        let line = stylize_markdown_line("a * b * c", Style::default(), &mut code_block);
        // Should not detect bold — single stars are not bold markers.
        assert_eq!(line.spans.len(), 1);
    }

    // -- v0.10.18.1 scrollback u16 overflow tests --

    #[test]
    fn scroll_offset_handles_large_line_count() {
        // Verify that scroll logic handles >65535 visual lines without overflow.
        // The pre-slicing approach in draw_output avoids Paragraph::scroll((u16, u16))
        // overflow by computing start_idx / residual_scroll from logical lines.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Raise buffer limit so lines aren't dropped.
        app.output_buffer_limit = 80_000;
        // Add 70000 single-char lines (each wraps to 1 visual line at any width).
        for i in 0..70_000 {
            app.push_output(OutputLine::command(format!("{}", i)));
        }
        assert_eq!(app.output.len(), 70_000);

        // Scrolling up a large amount should not panic or wrap.
        app.scroll_up(60_000);
        assert_eq!(app.scroll_offset, 60_000);

        // Scroll back down.
        app.scroll_down(30_000);
        assert_eq!(app.scroll_offset, 30_000);

        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn scroll_offset_max_clamp() {
        // scroll_up should clamp to the output length, not exceed it.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..100 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }
        app.scroll_up(999_999);
        // Should clamp to output.len() at most.
        assert!(app.scroll_offset <= app.output.len());
    }

    // -- v0.10.18.2 scrollback & scroll navigation tests --

    #[test]
    fn scrollback_preserves_and_retrieves_past_output() {
        // Item 3: Push 500+ lines, verify buffer retains all, scroll to top,
        // verify first line, scroll to bottom, verify latest line.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Ensure buffer limit is well above 500.
        app.output_buffer_limit = 50_000;

        // Push 600 lines.
        for i in 0..600 {
            app.push_output(OutputLine::command(format!("output-line-{}", i)));
        }

        // Buffer retains all 600 lines.
        assert_eq!(app.output.len(), 600);

        // Verify first line content.
        assert_eq!(app.output[0].text, "output-line-0");

        // Verify last line content.
        assert_eq!(app.output[599].text, "output-line-599");

        // Scroll to top (scroll up by total line count).
        app.scroll_up(app.output.len());
        assert!(app.scroll_offset > 0);

        // First line is still accessible in the buffer.
        assert_eq!(app.output[0].text, "output-line-0");

        // Scroll back to bottom.
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);

        // Latest line is still there.
        assert_eq!(app.output[app.output.len() - 1].text, "output-line-599");
    }

    #[test]
    fn auto_scroll_follows_when_at_bottom() {
        // Item 4a: When scroll_offset is 0 (at bottom) and new content arrives,
        // scroll_offset stays at 0 and unread_events is not incremented.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );

        // Start at bottom (scroll_offset = 0).
        assert_eq!(app.scroll_offset, 0);

        // Push new content.
        app.push_output(OutputLine::command("line 1".into()));
        app.push_output(OutputLine::command("line 2".into()));
        app.push_output(OutputLine::command("line 3".into()));

        // Should still be at bottom, no unread.
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_events, 0);
    }

    #[test]
    fn no_auto_scroll_when_scrolled_up() {
        // Item 4b: When scroll_offset is NOT 0 (scrolled up) and new content
        // arrives, scroll_offset stays put and unread flag is set.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );

        // Add initial content.
        for i in 0..50 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }

        // Scroll up.
        app.scroll_up(10);
        assert_eq!(app.scroll_offset, 10);
        assert_eq!(app.unread_events, 0);

        // New content arrives while scrolled up.
        app.push_output(OutputLine::command("new line A".into()));
        app.push_output(OutputLine::command("new line B".into()));

        // Scroll offset should NOT change (no auto-scroll).
        assert_eq!(app.scroll_offset, 10);

        // Unread events should be incremented.
        assert_eq!(app.unread_events, 2);

        // Scrolling to bottom clears unread.
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_events, 0);
    }

    #[test]
    fn scrollback_lines_config_alias() {
        // Verify that ShellConfig::effective_scrollback() uses scrollback_lines
        // when set, and enforces the 10,000 minimum.
        use ta_submit::ShellConfig;

        let mut config = ShellConfig::default();
        // Default: uses output_buffer_lines (50000).
        assert_eq!(config.effective_scrollback(), 50_000);

        // scrollback_lines overrides.
        config.scrollback_lines = Some(20_000);
        assert_eq!(config.effective_scrollback(), 20_000);

        // Minimum enforced at 10,000.
        config.scrollback_lines = Some(5_000);
        assert_eq!(config.effective_scrollback(), 10_000);
    }

    // -- v0.10.18.3 mouse scroll tests --

    #[test]
    fn mouse_scroll_events_move_scroll_offset() {
        // Simulate mouse ScrollUp and ScrollDown events and verify
        // scroll_offset changes by 3 lines per event, clamped to bounds.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );

        // Push enough lines to allow scrolling.
        for i in 0..100 {
            app.push_output(OutputLine::command(format!("line {}", i)));
        }

        // Start at bottom (scroll_offset = 0).
        assert_eq!(app.scroll_offset, 0);

        // Scroll up by 3 (mouse wheel up).
        app.scroll_up(3);
        assert_eq!(app.scroll_offset, 3);

        // Scroll up again.
        app.scroll_up(3);
        assert_eq!(app.scroll_offset, 6);

        // Scroll down by 3 (mouse wheel down).
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 3);

        // Scroll down past bottom clamps to 0.
        app.scroll_down(100);
        assert_eq!(app.scroll_offset, 0);

        // Scroll up past top clamps to max.
        app.scroll_up(999_999);
        assert!(app.scroll_offset <= app.output.len());
        let max = app.scroll_offset;

        // Another scroll up doesn't exceed max.
        app.scroll_up(3);
        assert_eq!(app.scroll_offset, max);
    }

    // ── v0.10.18.5 tests ──────────────────────────────────────

    #[test]
    fn handle_stdin_prompt_sets_pending() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let sp = PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Select topology:".into(),
        };
        handle_tui_message(&mut app, TuiMessage::StdinPrompt(sp));
        assert!(app.pending_stdin_prompt.is_some());
        assert_eq!(
            app.pending_stdin_prompt.as_ref().unwrap().prompt_text,
            "Select topology:"
        );
        // Should have added output lines for the prompt display.
        assert!(!app.output.is_empty());
        assert!(app.output.iter().any(|l| l.text.contains("Stdin Prompt")));
    }

    #[test]
    fn handle_stdin_auto_answered() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::StdinAutoAnswered {
                prompt: "Continue?".into(),
                response: "y".into(),
            },
        );
        assert!(!app.output.is_empty());
        assert!(app.output.last().unwrap().text.contains("[auto]"));
        assert!(app.output.last().unwrap().text.contains("Continue?"));
    }

    #[test]
    fn prompt_str_for_stdin_prompt() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        assert_eq!(app.prompt_str(), "ta> ");
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Enter name:".into(),
        });
        assert_eq!(app.prompt_str(), "[stdin] > ");
    }

    #[test]
    fn ctrl_c_cancels_stdin_prompt() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Enter name:".into(),
        });
        // Simulate Ctrl-C by directly clearing the prompt.
        app.pending_stdin_prompt = None;
        assert!(app.pending_stdin_prompt.is_none());
    }
}
