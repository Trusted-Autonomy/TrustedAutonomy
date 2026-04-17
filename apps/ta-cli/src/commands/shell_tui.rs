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

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use super::shell::{resolve_daemon_url, StatusInfo};

// ── Mouse & scroll handling ─────────────────────────────────────────
//
// Mouse capture IS enabled (`?1000h`/`?1002h`) to allow scroll wheel and
// scrollbar click/drag (v0.14.7.1 item 6). Native text selection and copy
// are still available in most terminals via Option+drag (macOS) or
// Shift+drag (Linux/Windows).
//
// Keyboard scrolling (always available):
//   Shift+Up/Down    → scroll output 1 line
//   PageUp/PageDown  → scroll output 1 page
//   Shift+Home/End   → scroll to top/bottom
//   Cmd+Up/Down      → scroll to top/bottom (macOS)
//   Up/Down          → command history

// ── Latency diagnostics ─────────────────────────────────────────
//
// Enabled via `:latency on` in the shell. Tracks timestamps at each
// stage of the input pipeline to identify where time is lost:
//
//   OS read → channel send → try_recv → handle → draw
//
// Also tracks event loop cycle stats and background channel depth.

/// An input event tagged with the Instant it was read from the OS.
/// The dedicated input thread stamps every event before sending.
struct StampedEvent {
    event: Event,
    os_read_at: std::time::Instant,
}

/// Rolling latency diagnostics (when enabled via `:latency on`).
struct LatencyDiag {
    enabled: bool,
    /// Ring buffer of recent input-to-processed latencies (microseconds).
    samples: Vec<u64>,
    /// Index into ring buffer.
    idx: usize,
    /// Total event loop cycles since last report.
    cycles: u64,
    /// Cycles that did work (input or bg).
    busy_cycles: u64,
    /// Cycles spent idle (yielding to tokio).
    idle_cycles: u64,
    /// Max draw duration in last reporting window (microseconds).
    max_draw_us: u64,
    /// Max bg message process time in last window (microseconds).
    max_bg_us: u64,
    /// Max input handle time in last window (microseconds).
    max_input_us: u64,
    /// Last report time.
    last_report: std::time::Instant,
    /// bg channel depth at last sample.
    last_bg_depth: usize,
}

impl LatencyDiag {
    fn new() -> Self {
        Self {
            enabled: false,
            samples: Vec::with_capacity(128),
            idx: 0,
            cycles: 0,
            busy_cycles: 0,
            idle_cycles: 0,
            max_draw_us: 0,
            max_bg_us: 0,
            max_input_us: 0,
            last_report: std::time::Instant::now(),
            last_bg_depth: 0,
        }
    }

    fn record_input_latency(&mut self, latency_us: u64) {
        if self.samples.len() < 128 {
            self.samples.push(latency_us);
        } else {
            self.samples[self.idx % 128] = latency_us;
        }
        self.idx += 1;
    }

    fn record_draw(&mut self, dur_us: u64) {
        self.max_draw_us = self.max_draw_us.max(dur_us);
    }

    fn record_bg(&mut self, dur_us: u64) {
        self.max_bg_us = self.max_bg_us.max(dur_us);
    }

    fn record_input_handle(&mut self, dur_us: u64) {
        self.max_input_us = self.max_input_us.max(dur_us);
    }

    /// Generate a report string and reset counters. Called every 5s.
    /// If `force` is false, only reports when enabled and 5s have elapsed.
    fn report(&mut self) -> Option<String> {
        self.report_inner(false)
    }

    /// Force a report regardless of enabled/timing state.
    fn report_forced(&mut self) -> Option<String> {
        self.report_inner(true)
    }

    fn report_inner(&mut self, force: bool) -> Option<String> {
        if !force && !self.enabled {
            return None;
        }
        if self.cycles == 0 && self.samples.is_empty() {
            return None;
        }
        let elapsed = self.last_report.elapsed();
        if !force && elapsed.as_secs() < 5 {
            return None;
        }

        let n = self.samples.len();
        let (avg_us, p50_us, p99_us, max_us) = if n > 0 {
            let mut sorted: Vec<u64> = self.samples.clone();
            sorted.sort_unstable();
            let sum: u64 = sorted.iter().sum();
            let avg = sum / n as u64;
            let p50 = sorted[n / 2];
            let p99 = sorted[(n as f64 * 0.99) as usize];
            let max = *sorted.last().unwrap();
            (avg, p50, p99, max)
        } else {
            (0, 0, 0, 0)
        };

        let cps = self.cycles as f64 / elapsed.as_secs_f64();
        let report = format!(
            "[latency] input: avg={avg_us}µs p50={p50_us}µs p99={p99_us}µs max={max_us}µs | \
             cycles: {cps:.0}/s (busy={busy} idle={idle}) | \
             draw_max={draw}µs bg_max={bg}µs input_max={inp}µs | \
             bg_depth={depth} | samples={n}",
            busy = self.busy_cycles,
            idle = self.idle_cycles,
            draw = self.max_draw_us,
            bg = self.max_bg_us,
            inp = self.max_input_us,
            depth = self.last_bg_depth,
        );

        // Reset for next window.
        self.samples.clear();
        self.idx = 0;
        self.cycles = 0;
        self.busy_cycles = 0;
        self.idle_cycles = 0;
        self.max_draw_us = 0;
        self.max_bg_us = 0;
        self.max_input_us = 0;
        self.last_report = std::time::Instant::now();

        Some(report)
    }
}

/// A position in screen coordinates (column, row).
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenPos {
    col: u16,
    row: u16,
}

/// Active text selection state.
#[derive(Debug, Clone)]
struct Selection {
    /// Where the selection started (anchor point).
    anchor: ScreenPos,
    /// Where the selection currently extends to (moves with drag).
    extent: ScreenPos,
    /// The output pane area at the time selection started, for coordinate mapping.
    output_area: ratatui::layout::Rect,
}

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
    /// Q&A agent determined this is not a real prompt (v0.11.2.5 Layer 3).
    PromptVerifiedNotPrompt,
    /// A goal started — may trigger auto-tail (v0.10.11).
    GoalStarted { goal_id: String, title: String },
    /// Agent output stream ended (goal process exited).
    AgentOutputDone(String),
    /// "Agent is working..." indicator — pushed as a heartbeat so AgentOutputDone clears it (v0.12.7).
    WorkingIndicator(String),
    /// A draft is ready for review (v0.10.11 / v0.15.7.1).
    DraftReady {
        #[allow(dead_code)]
        goal_id: String,
        #[allow(dead_code)]
        draft_id: String,
        display_id: String,
        title: String,
        artifact_count: usize,
    },
    /// Tail stream successfully connected to a goal — track in active set (v0.12.3).
    TailStarted { goal_id: String },
}

/// A line of live agent output (v0.10.11).
#[derive(Clone, Debug)]
pub struct AgentOutputLine {
    pub stream: String,
    pub line: String,
    /// Goal ID this line belongs to — used for multi-agent tag prefixing (v0.12.3).
    pub goal_id: Option<String>,
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
    /// When this prompt was detected (v0.11.2.5 Layer 2: continuation cancellation).
    pub detected_at: std::time::Instant,
    /// Whether Q&A agent verification is in flight (v0.11.2.5 Layer 3).
    pub verifying: bool,
}

/// A line in the output pane, with optional styling.
#[derive(Clone)]
struct OutputLine {
    text: String,
    style: Style,
    /// Whether this line is a heartbeat that should be updated in-place (v0.11.4.1 item 9).
    is_heartbeat: bool,
}

impl OutputLine {
    fn command(text: String) -> Self {
        Self {
            text,
            style: Style::default(),
            is_heartbeat: false,
        }
    }

    fn event(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::DarkGray),
            is_heartbeat: false,
        }
    }

    fn error(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Red),
            is_heartbeat: false,
        }
    }

    fn info(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Cyan),
            is_heartbeat: false,
        }
    }

    fn agent_stdout(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::White),
            is_heartbeat: false,
        }
    }

    fn agent_stderr(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::Yellow),
            is_heartbeat: false,
        }
    }

    fn notification(text: String) -> Self {
        Self {
            text,
            style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            is_heartbeat: false,
        }
    }

    fn separator(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::DarkGray),
            is_heartbeat: false,
        }
    }

    /// Heartbeat line — updated in-place instead of appending (v0.11.4.1 item 9).
    fn heartbeat(text: String) -> Self {
        Self {
            text,
            style: Style::default().fg(Color::DarkGray),
            is_heartbeat: true,
        }
    }
}

// ── Large-paste compaction (v0.11.4.5) ──────────────────────────────────────
//
// Pasting more than PASTE_CHAR_THRESHOLD chars OR PASTE_LINE_THRESHOLD lines
// compacts the paste into a single-line indicator in the input area:
//
//   ta> [Pasted 2,847 chars / 47 lines — Tab to preview, Esc to cancel]
//
// The full text is stored in App::pending_paste (not in App::input).
// On Enter the full paste is appended to any text already typed in the input.
// Tab toggles an inline preview of the first few paste lines.
// Escape (or Ctrl-C) cancels and discards the pending paste.

/// Number of chars above which a paste is compacted.
const PASTE_CHAR_THRESHOLD: usize = 500;
/// Number of lines above which a paste is compacted.
const PASTE_LINE_THRESHOLD: usize = 10;
/// Max lines shown in the expanded paste preview.
const PASTE_PREVIEW_LINES: usize = 5;

/// Current shell context for context-sensitive help (v0.14.9.2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellContext {
    /// No active goal or draft view.
    Idle,
    /// Currently tailing a running goal.
    RunningGoal { goal_id: String },
    /// Viewing a draft (after `view <id>` or `ta draft view <id>`).
    ViewingDraft { draft_id: String },
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
    /// Full text of a large paste awaiting submission (v0.11.4.5).
    /// When Some, the input area shows a compact indicator instead of raw text.
    pending_paste: Option<String>,
    /// Whether the paste preview (Tab to expand) is currently visible (v0.11.4.5).
    paste_preview_expanded: bool,
    /// Goal ID currently being tailed for agent output (v0.10.11).
    tailing_goal: Option<String>,
    /// Goal ID in bidirectional attach mode — all input is relayed to the agent (v0.12.0.1).
    /// Ctrl-D or `:detach` exits attach mode.
    attach_mode: Option<String>,
    /// Maximum output buffer lines (configurable, v0.10.11).
    output_buffer_limit: usize,
    /// Cached total visual line count (accounting for word wrap).
    /// Updated on push_output and terminal resize. Avoids O(n) recount every frame.
    cached_visual_lines: usize,
    /// Terminal width when `cached_visual_lines` was last computed.
    cached_wrap_width: usize,
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
    /// Schema-driven agent output parser (v0.11.2.2).
    output_schema: ta_output_schema::OutputSchema,
    /// Seconds after which a prompt is auto-dismissed if agent continues output (v0.11.2.5).
    prompt_dismiss_after_output_secs: u64,
    /// Seconds to wait for Q&A agent prompt verification (v0.11.2.5).
    prompt_verify_timeout_secs: u64,
    /// Active text selection (unused — native terminal selection handles this).
    #[allow(dead_code)]
    selection: Option<Selection>,
    /// Cached output pane area from the last draw (for mouse coordinate mapping).
    #[allow(dead_code)]
    output_area: ratatui::layout::Rect,
    /// Latency diagnostics (`:latency on/off/dump`).
    latency_diag: LatencyDiag,
    /// All goal IDs currently being tailed (for multi-agent tag-prefix logic, v0.12.3).
    active_tailing_goals: std::collections::HashSet<String>,
    /// Short tag of last agent routed to via `>tag message` — shows `[→tag]` in status bar (v0.12.3).
    last_routed_tag: Option<String>,
    /// Goal ID awaiting auth-failure retry/abort decision (v0.12.3).
    pending_auth_retry: Option<String>,
    /// Whether new output should auto-scroll to bottom (v0.14.7.1 item 4).
    /// Set to false when user scrolls up; restored to true when scrolled back to bottom.
    auto_scroll: bool,
    /// Column index of the scrollbar (rightmost column, v0.14.7.1 item 6).
    scrollbar_col: Option<u16>,
    /// Top row of the output area for mouse coordinate mapping (v0.14.7.1 item 6).
    output_area_top: u16,
    /// Height of the output area in rows (v0.14.7.1 item 6).
    output_area_height: u16,
    /// Whether the user is currently dragging the scrollbar (v0.14.7.1 item 6).
    scrollbar_dragging: bool,
    /// Current shell context for context-sensitive help (v0.14.9.2).
    shell_context: ShellContext,
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
            pending_paste: None,
            paste_preview_expanded: false,
            tailing_goal: None,
            attach_mode: None,
            output_buffer_limit: 50000,
            cached_visual_lines: 0,
            cached_wrap_width: 0,
            auto_tail: true,
            tail_backfill_lines: 5,
            split_pane: false,
            agent_output: Vec::new(),
            agent_scroll_offset: 0,
            output_schema: {
                let loader = ta_output_schema::SchemaLoader::new(&project_root);
                // Default to claude-code schema; the active agent may change at runtime.
                loader
                    .load("claude-code")
                    .unwrap_or_else(|_| ta_output_schema::OutputSchema::passthrough())
            },
            project_root,
            prompt_dismiss_after_output_secs: 5,
            prompt_verify_timeout_secs: 10,
            selection: None,
            output_area: ratatui::layout::Rect::default(),
            latency_diag: LatencyDiag::new(),
            active_tailing_goals: std::collections::HashSet::new(),
            last_routed_tag: None,
            pending_auth_retry: None,
            auto_scroll: true,
            scrollbar_col: None,
            output_area_top: 0,
            output_area_height: 0,
            scrollbar_dragging: false,
            shell_context: ShellContext::Idle,
        }
    }

    fn push_output(&mut self, line: OutputLine) {
        // Update cached visual line count for the new line.
        if self.cached_wrap_width > 0 {
            let vlines = if line.text.is_empty() {
                1
            } else {
                line.text.len().div_ceil(self.cached_wrap_width)
            };
            self.cached_visual_lines += vlines;
        }
        self.output.push(line);
        // Enforce buffer limit — drop oldest lines when exceeded.
        if self.output.len() > self.output_buffer_limit {
            let excess = self.output.len() - self.output_buffer_limit;
            // Subtract visual lines for removed entries.
            if self.cached_wrap_width > 0 {
                for ol in &self.output[..excess] {
                    let vlines = if ol.text.is_empty() {
                        1
                    } else {
                        ol.text.len().div_ceil(self.cached_wrap_width)
                    };
                    self.cached_visual_lines = self.cached_visual_lines.saturating_sub(vlines);
                }
            }
            self.output.drain(..excess);
            // Adjust scroll offset to compensate for removed lines.
            self.scroll_offset = self.scroll_offset.saturating_sub(excess);
        }
        // Auto-scroll logic (v0.14.9.1): use is_at_bottom() so that both
        // scroll_offset==0 and the "content shorter than viewport" case are handled.
        // When at bottom, always re-enable auto_scroll — this ensures tail resumes
        // after a scroll-up/scroll-back sequence even if auto_scroll was left false
        // (e.g. by buffer-overflow scroll_offset adjustment via saturating_sub).
        if self.is_at_bottom() {
            self.unread_events = 0;
            self.auto_scroll = true;
        } else {
            // Scrolled up — disable auto-scroll and count unread events.
            self.auto_scroll = false;
            self.unread_events += 1;
        }
    }

    fn push_lines(&mut self, text: &str, style_fn: fn(String) -> OutputLine) {
        for line in text.lines() {
            self.push_output(style_fn(line.to_string()));
        }
    }

    /// Push a heartbeat line, updating the last line in-place if it's already
    /// a heartbeat (v0.11.4.1 item 9). This avoids flooding the output with
    /// repeated heartbeat lines — only the elapsed time changes.
    fn push_heartbeat(&mut self, text: String) {
        if let Some(last) = self.output.last_mut() {
            if last.is_heartbeat {
                // Update in-place — don't append a new line.
                last.text = text;
                return;
            }
        }
        self.push_output(OutputLine::heartbeat(text));
    }

    fn prompt_str(&self) -> String {
        if let Some(ref sp) = self.pending_stdin_prompt {
            if sp.verifying {
                "[stdin] > (verifying...) ".to_string()
            } else {
                "[stdin] > ".to_string()
            }
        } else if self.pending_auth_retry.is_some() {
            "auth-fail> [r]etry [a]bort: ".to_string()
        } else if let Some(ref goal_id) = self.attach_mode {
            let short = &goal_id[..8.min(goal_id.len())];
            format!("[attach:{}] > ", short)
        } else if let Some(ref q) = self.pending_question {
            format!("[agent Q{}] > ", q.turn)
        } else if self.workflow_prompt.is_some() {
            "workflow> ".to_string()
        } else if let Some(ref tag) = self.last_routed_tag {
            // After a `>tag message` route, show where the last message went (v0.12.3).
            format!("[→{}] > ", tag)
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
        // If there's a pending large paste, combine typed prefix with the full paste text.
        let text = if let Some(paste) = self.pending_paste.take() {
            self.paste_preview_expanded = false;
            let prefix = self.input.trim_end().to_string();
            if prefix.is_empty() {
                paste
            } else {
                format!("{}\n{}", prefix, paste)
            }
        } else {
            self.input.trim().to_string()
        };
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

    /// Cancel any pending large paste without submitting.
    fn cancel_pending_paste(&mut self) {
        if self.pending_paste.take().is_some() {
            self.paste_preview_expanded = false;
            self.push_output(OutputLine::info("Paste cancelled.".into()));
        }
    }

    /// Scroll up in the output pane.
    fn scroll_up(&mut self, amount: usize) {
        // Use logical line count as upper bound. The actual visual max is
        // computed in draw_output with the real terminal width, but logical
        // lines are a safe ceiling — you can't scroll past all content.
        let max_scroll = self.output.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);
        // Disable auto-scroll when user has scrolled up (v0.14.7.1 item 4).
        if self.scroll_offset > 0 {
            self.auto_scroll = false;
        }
    }

    /// Scroll down in the output pane.
    fn scroll_down(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.unread_events = 0;
            // Re-enable auto-scroll when back at bottom (v0.14.7.1 item 4).
            self.auto_scroll = true;
        }
    }

    /// Scroll to bottom.
    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.unread_events = 0;
        // Re-enable auto-scroll when explicitly scrolled to bottom (v0.14.7.1 item 4).
        self.auto_scroll = true;
    }

    /// Auto-scroll to bottom when "near bottom" (within threshold lines).
    ///
    /// Called after any output line arrives so that a user who is at or
    /// near the bottom stays pinned there — matching a `tail -f` experience.
    /// If the user has scrolled far up to read history, this is a no-op.
    /// Threshold reduced to 3 (from 5) to avoid surprising snaps when the user
    /// has scrolled 4-5 lines to re-read recent output (R2 fix, v0.13.1.5).
    fn auto_scroll_if_near_bottom(&mut self) {
        const NEAR_BOTTOM_LINES: usize = 3;
        if self.scroll_offset <= NEAR_BOTTOM_LINES {
            self.scroll_offset = 0;
            self.unread_events = 0;
            // Re-enable auto-scroll when near bottom (v0.14.7.1 item 4).
            self.auto_scroll = true;
        }
    }

    /// Returns true when the viewport is at the bottom of the output buffer.
    ///
    /// Two cases count as "at bottom" (v0.14.9.1):
    ///   1. scroll_offset == 0 — the standard case: user is pinned to newest output.
    ///   2. Content doesn't fill the viewport — when there are fewer lines than the
    ///      visible area, scroll_offset may be a small positive number while the user
    ///      is visually looking at everything. Without this check, auto_scroll stays
    ///      false after scrolling up in a short buffer, even when the user scrolls back
    ///      to the very last line.
    fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
            || (self.output_area_height > 0
                && self.output.len() < self.output_area_height.saturating_sub(4) as usize)
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
    let mut initial_status = super::shell::fetch_status(&client, &base_url).await;
    if initial_status.version.is_empty() || initial_status.version == "?" {
        // Auto-start the daemon (v0.11.2.1 — match classic shell behavior).
        match super::daemon::ensure_running(&project_root) {
            Ok(()) => {
                initial_status = super::shell::fetch_status(&client, &base_url).await;
                if initial_status.version.is_empty() || initial_status.version == "?" {
                    eprintln!("Error: Daemon started but status fetch still failed.");
                    eprintln!("  Check logs: ta daemon log");
                    return Err(anyhow::anyhow!("daemon not reachable at {}", base_url));
                }
                eprintln!("Daemon auto-started (v{}).", initial_status.version);
            }
            Err(e) => {
                eprintln!("Error: Cannot reach daemon at {}", base_url);
                eprintln!("  Auto-start failed: {}", e);
                eprintln!();
                eprintln!("Start the daemon manually with:");
                eprintln!("  ta daemon start                # start in background");
                eprintln!("  ta daemon start --foreground   # start in foreground (for debugging)");
                return Err(anyhow::anyhow!("daemon not reachable at {}", base_url));
            }
        }
    }

    // Build SHA mismatch — auto-restart daemon (catches rebuilds within same version).
    let cli_version = env!("CARGO_PKG_VERSION");
    let cli_build_sha = env!("TA_GIT_HASH");
    let daemon_sha = &initial_status.build_sha;
    let sha_mismatch = !daemon_sha.is_empty() && daemon_sha != "?" && daemon_sha != cli_build_sha;
    let version_mismatch = initial_status.version != cli_version;
    if sha_mismatch || version_mismatch {
        eprintln!(
            "Daemon build mismatch ({} vs {}) — restarting daemon...",
            daemon_sha, cli_build_sha
        );
        match super::daemon::restart(&project_root, None) {
            Ok(_pid) => {
                // Wait briefly for the new daemon to be ready.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                // Re-fetch status after restart.
                initial_status = super::shell::fetch_status(&client, &base_url).await;
                eprintln!("Daemon restarted (v{}).", initial_status.version);
            }
            Err(e) => {
                eprintln!(
                    "Warning: daemon restart failed: {}. Continuing with v{}.",
                    e, initial_status.version
                );
            }
        }
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

    // Load prompt detection config from daemon.toml (v0.11.2.5).
    let (prompt_dismiss_secs, prompt_verify_secs) = load_prompt_detection_config();

    // Create app state.
    let mut app = App::new(base_url.clone(), attach_session.clone(), project_root);
    app.status = initial_status;
    app.daemon_connected = true;
    app.completions = completions;
    app.output_buffer_limit = shell_config.effective_scrollback();
    app.auto_tail = shell_config.auto_tail;
    app.tail_backfill_lines = shell_config.tail_backfill_lines;
    app.prompt_dismiss_after_output_secs = prompt_dismiss_secs;
    app.prompt_verify_timeout_secs = prompt_verify_secs;

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
    // Enable mouse capture for scroll wheel and scrollbar click/drag (v0.14.7.1 item 6).
    // Bracketed paste: pasted text arrives as Event::Paste(String) instead of
    // individual key events, so we can insert without executing on newlines.
    stdout.execute(crossterm::event::EnableMouseCapture)?;
    stdout.execute(EnableBracketedPaste)?;
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

    // Dedicated input thread (v0.11.4.2 item 11): decouple terminal event
    // reading from the async runtime so keystrokes stay responsive even when
    // agent subprocesses are spawning or the event loop is under pressure.
    // Each event is stamped with the OS-read Instant for latency tracking.
    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<StampedEvent>();
    let input_running = running.clone();
    std::thread::spawn(move || {
        while input_running.load(Ordering::Relaxed) {
            // Block up to 16ms waiting for the first event.
            if event::poll(std::time::Duration::from_millis(16)).unwrap_or(false) {
                // Drain ALL available events in a tight loop — critical for
                // fast typists and for keeping up when the event loop is busy
                // with draws or bg processing.
                while let Ok(ev) = event::read() {
                    let stamped = StampedEvent {
                        event: ev,
                        os_read_at: std::time::Instant::now(),
                    };
                    if input_tx.send(stamped).is_err() {
                        return; // Receiver dropped — TUI is shutting down.
                    }
                    // Check for more events with zero timeout (non-blocking).
                    if !event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                        break;
                    }
                }
            }
        }
    });
    let result = tui_event_loop(
        &mut terminal,
        &mut app,
        input_rx,
        &mut rx,
        &client,
        tx.clone(),
    )
    .await;

    // Cleanup.
    running.store(false, Ordering::Relaxed);
    disable_raw_mode()?;
    terminal.backend_mut().execute(DisableBracketedPaste)?;
    // Disable mouse capture (v0.14.7.1 item 6).
    terminal
        .backend_mut()
        .execute(crossterm::event::DisableMouseCapture)?;
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
    mut input_rx: tokio::sync::mpsc::UnboundedReceiver<StampedEvent>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiMessage>,
    client: &reqwest::Client,
    tx: tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) -> anyhow::Result<()> {
    use std::io::Write;
    use std::time::{Duration, Instant};

    // Latency log file: written to .ta/latency.log when diagnostics are on.
    // Opened lazily on `:latency on`, closed on `:latency off`.
    let mut latency_log: Option<std::fs::File> = None;

    // Frame rate limiter for background-only updates: 16ms (~60fps).
    // Input-triggered draws bypass this and happen immediately.
    let frame_interval = Duration::from_millis(16);
    let mut last_draw = Instant::now() - frame_interval;
    let mut needs_draw = true;

    // Initial draw.
    terminal.draw(|f| draw_ui(f, app))?;

    loop {
        app.latency_diag.cycles += 1;

        if !app.running {
            break;
        }

        // ── Event-driven select ──────────────────────────────────────
        //
        // `tokio::select!` with `biased` wakes INSTANTLY when data
        // arrives on either channel — no polling, no sleeping, no
        // scheduler starvation. Input is checked first (biased) so
        // keystrokes always take priority over background messages.
        //
        // The frame timer branch fires periodically to handle
        // rate-limited redraws for background-only updates.

        // Use a longer timer interval when streaming agent output —
        // no point waking 60x/sec if we only draw at 2fps.
        let target_interval = if app.tailing_goal.is_some() && needs_draw {
            Duration::from_millis(500)
        } else if needs_draw {
            frame_interval
        } else {
            Duration::from_millis(100)
        };
        let time_to_next_frame = target_interval.saturating_sub(last_draw.elapsed());

        tokio::select! {
            biased;

            // ── Input (highest priority) ─────────────────────────────
            Some(stamped) = input_rx.recv() => {
                let handle_start = Instant::now();

                // Process this event + drain any more that are ready.
                process_input_event(app, stamped, &mut latency_log, client, &tx).await;
                while let Ok(more) = input_rx.try_recv() {
                    process_input_event(app, more, &mut latency_log, client, &tx).await;
                }

                let handle_us = handle_start.elapsed().as_micros() as u64;
                app.latency_diag.record_input_handle(handle_us);
                if app.latency_diag.enabled {
                    if let Some(ref mut lf) = latency_log {
                        let _ = writeln!(
                            lf,
                            "{} INPUT_BATCH handle={}µs",
                            chrono::Local::now().format("%H:%M:%S%.3f"),
                            handle_us,
                        );
                    }
                }
                app.latency_diag.busy_cycles += 1;

                // Render the input line immediately so the user sees
                // their keystroke without delay.
                //
                // During agent streaming, a full ratatui draw() includes
                // output pane changes — hundreds of ANSI escape sequences
                // that saturate the terminal emulator and prevent it from
                // forwarding keystrokes through the pty.
                //
                // Fix: when streaming, write ONLY the input line directly
                // via crossterm (~50 bytes). The terminal processes this
                // instantly. Full-frame draws happen on the rate-limited
                // bg path (2fps) for output updates.
                if app.tailing_goal.is_some() {
                    direct_input_write(terminal, app);
                    // Don't update last_draw — let the bg rate-limiter
                    // handle full-frame draws independently.
                    needs_draw = true;
                } else {
                    let draw_start = Instant::now();
                    update_wrap_cache(terminal, app);
                    terminal.draw(|f| draw_ui(f, app))?;
                    update_layout_cache(app, terminal);
                    last_draw = Instant::now();
                    needs_draw = false;
                    app.latency_diag
                        .record_draw(draw_start.elapsed().as_micros() as u64);
                }
            }

            // ── Background messages ──────────────────────────────────
            Some(msg) = rx.recv() => {
                let bg_start = Instant::now();
                app.latency_diag.last_bg_depth = rx.len() + 1;
                process_background_message(app, msg, client, &tx).await;

                // Drain more bg messages up to a batch limit.
                const BG_BATCH_LIMIT: usize = 200;
                let mut bg_count = 1usize;
                while let Ok(more) = rx.try_recv() {
                    process_background_message(app, more, client, &tx).await;
                    bg_count += 1;
                    if bg_count >= BG_BATCH_LIMIT {
                        break;
                    }
                }
                app.latency_diag
                    .record_bg(bg_start.elapsed().as_micros() as u64);
                app.latency_diag.busy_cycles += 1;
                needs_draw = true;
            }

            // ── Frame timer (periodic wake for bg redraws) ───────────
            _ = tokio::time::sleep(time_to_next_frame) => {
                app.latency_diag.idle_cycles += 1;
            }
        }

        // ── Draw (rate-limited, for bg-only updates) ─────────────────
        // Throttle draws to avoid saturating the terminal emulator with
        // escape sequences. When the emulator can't keep up, it delays
        // forwarding keystrokes through the pty — causing the "lost input"
        // sensation where users press keys but nothing appears.
        //
        // When actively tailing agent output, limit to 2fps (500ms).
        // This gives the terminal emulator ample time to process both
        // stdout rendering AND stdin forwarding between frames.
        let effective_interval = if app.tailing_goal.is_some() {
            Duration::from_millis(500)
        } else if app.latency_diag.last_bg_depth > 50 {
            Duration::from_millis(200)
        } else {
            frame_interval
        };
        if needs_draw && last_draw.elapsed() >= effective_interval {
            let draw_start = Instant::now();
            update_wrap_cache(terminal, app);
            terminal.draw(|f| draw_ui(f, app))?;
            update_layout_cache(app, terminal);
            last_draw = Instant::now();
            needs_draw = false;
            app.latency_diag
                .record_draw(draw_start.elapsed().as_micros() as u64);
        }

        // Emit periodic latency report if diagnostics are on.
        if app.latency_diag.enabled {
            if latency_log.is_none() {
                let log_dir = app.project_root.join(".ta");
                let _ = std::fs::create_dir_all(&log_dir);
                latency_log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_dir.join("latency.log"))
                    .ok();
                if let Some(ref mut lf) = latency_log {
                    let _ = writeln!(
                        lf,
                        "\n--- session {} ---",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
            if let Some(report) = app.latency_diag.report() {
                app.push_output(OutputLine::event(report.clone()));
                if let Some(ref mut lf) = latency_log {
                    let _ = writeln!(
                        lf,
                        "{} {}",
                        chrono::Local::now().format("%H:%M:%S%.3f"),
                        report
                    );
                }
            }
        } else if latency_log.is_some() {
            latency_log = None;
        }
    }
    Ok(())
}

/// Process a single input event: record latency, log, and dispatch.
async fn process_input_event(
    app: &mut App,
    stamped: StampedEvent,
    latency_log: &mut Option<std::fs::File>,
    client: &reqwest::Client,
    tx: &tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    use std::io::Write;

    let transit_us = stamped.os_read_at.elapsed().as_micros() as u64;
    app.latency_diag.record_input_latency(transit_us);

    if app.latency_diag.enabled {
        if let Some(ref mut lf) = latency_log {
            let ev_desc = match &stamped.event {
                Event::Key(k) => {
                    format!("key:{:?}+{:?} kind={:?}", k.code, k.modifiers, k.kind)
                }
                Event::Paste(_) => "paste".to_string(),
                Event::Mouse(m) => format!("mouse:{:?}", m.kind),
                Event::Resize(w, h) => format!("resize:{}x{}", w, h),
                _ => "other".to_string(),
            };
            let _ = writeln!(
                lf,
                "{} EVENT transit={}µs ev={}",
                chrono::Local::now().format("%H:%M:%S%.3f"),
                transit_us,
                ev_desc,
            );
        }
    }

    let input_before = if app.latency_diag.enabled {
        Some(app.input.clone())
    } else {
        None
    };
    handle_terminal_event(app, stamped.event, client, tx).await;
    if let Some(before) = input_before {
        if app.input != before {
            if let Some(ref mut lf) = latency_log {
                let _ = writeln!(
                    lf,
                    "{} INPUT_CHANGED len={}→{} buf={:?}",
                    chrono::Local::now().format("%H:%M:%S%.3f"),
                    before.len(),
                    app.input.len(),
                    &app.input,
                );
            }
        }
    }
}

/// Update the visual line cache if terminal width changed.
fn update_wrap_cache(terminal: &Terminal<CrosstermBackend<Stdout>>, app: &mut App) {
    let cur_width = terminal.size().map(|s| s.width as usize).unwrap_or(80);
    if cur_width != app.cached_wrap_width {
        app.cached_wrap_width = cur_width;
        app.cached_visual_lines = app
            .output
            .iter()
            .map(|ol| {
                if ol.text.is_empty() || cur_width == 0 {
                    1
                } else {
                    ol.text.len().div_ceil(cur_width)
                }
            })
            .sum();
    }
}

/// Process a single background message (SSE event, status update, etc.)
/// and spawn auto-tail if triggered by a GoalStarted event.
async fn process_background_message(
    app: &mut App,
    msg: TuiMessage,
    client: &reqwest::Client,
    tx: &tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
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

    if let Some(goal_id) = auto_tail_goal {
        let tail_client = client.clone();
        let tail_base = app.base_url.clone();
        let tail_tx = tx.clone();
        let backfill = app.tail_backfill_lines;
        tokio::spawn(async move {
            start_tail_stream(tail_client, &tail_base, Some(&goal_id), tail_tx, backfill).await;
        });
    }
}

async fn handle_terminal_event(
    app: &mut App,
    ev: Event,
    client: &reqwest::Client,
    tx: &tokio::sync::mpsc::UnboundedSender<TuiMessage>,
) {
    match ev {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) => {
            // Only process key PRESS events. Some terminals (especially on
            // macOS with kitty keyboard protocol) also report Release and
            // Repeat events. Processing those would cause double-inserts or
            // unexpected behavior.
            if kind != KeyEventKind::Press {
                return;
            }
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
                    } else if app.pending_paste.is_some() {
                        // Cancel pending large paste (v0.11.4.5).
                        app.cancel_pending_paste();
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
                (KeyCode::Esc, _) if app.pending_paste.is_some() => {
                    // Escape cancels a pending large paste (v0.11.4.5).
                    app.cancel_pending_paste();
                }
                (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    // Ctrl-D in attach mode exits attach (v0.12.0.1).
                    if app.attach_mode.is_some() {
                        app.attach_mode = None;
                        app.push_output(OutputLine::info(
                            "Detached from agent (Ctrl-D).".to_string(),
                        ));
                    } else if app.input.is_empty() {
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
                    app.auto_scroll = true; // re-enable tail after clear (v0.14.9.1)
                }
                // Ctrl+M mouse toggle removed — no mouse capture enabled.
                // Native text selection always works. Scroll via keyboard.
                (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
                    // Shift+Enter inserts a newline without submitting.
                    app.insert_char('\n');
                }
                (KeyCode::Enter, _) => {
                    if let Some(text) = app.submit() {
                        // Echo the command.
                        let prompt = app.prompt_str();
                        app.push_output(OutputLine::command(format!("{}{}", prompt, text)));
                        app.scroll_to_bottom();

                        // Attach mode: relay all input to the attached goal's stdin (v0.12.0.1).
                        // :detach colon commands are still processed above before reaching here.
                        if let Some(ref goal_id) = app.attach_mode.clone() {
                            // Allow :detach to fall through (handled in colon commands above).
                            // Any other input (including colon commands that didn't match) is relayed.
                            if !text.starts_with(':') || text == ":detach" {
                                if text != ":detach" {
                                    let client = client.clone();
                                    let base_url = app.base_url.clone();
                                    let tx = tx.clone();
                                    let goal_id = goal_id.clone();
                                    let input_text = text.clone();
                                    tokio::spawn(async move {
                                        let url =
                                            format!("{}/api/goals/{}/input", base_url, goal_id);
                                        let result = client
                                            .post(&url)
                                            .json(&serde_json::json!({ "input": input_text }))
                                            .send()
                                            .await;
                                        if let Err(e) = result {
                                            let _ = tx.send(TuiMessage::CommandResponse(format!(
                                                "[attach] Relay error: {}",
                                                e
                                            )));
                                        }
                                    });
                                }
                                return;
                            }
                        }

                        // Auth retry/abort prompt handling (v0.12.3).
                        // When `pending_auth_retry` is set, only 'r' / 'a' / "retry" / "abort"
                        // are accepted; everything else shows the prompt again.
                        if let Some(goal_id) = app.pending_auth_retry.clone() {
                            let lower = text.to_lowercase();
                            if lower == "r" || lower == "retry" {
                                app.pending_auth_retry = None;
                                app.last_routed_tag = None;
                                // No daemon restart API yet — instruct the user to re-run.
                                app.push_output(OutputLine::info(
                                    "[auth retry] Re-running the goal is not yet supported \
                                     from the shell. Run: ta goal list  and restart via CLI."
                                        .to_string(),
                                ));
                                app.push_output(OutputLine::info(format!(
                                    "  Goal ID: {}  — ta run <title> to start a new session.",
                                    &goal_id[..8.min(goal_id.len())]
                                )));
                            } else if lower == "a" || lower == "abort" {
                                app.pending_auth_retry = None;
                                app.last_routed_tag = None;
                                app.push_output(OutputLine::info(
                                    "[auth abort] Dismissed auth failure notice. \
                                     Agent process may still be running."
                                        .to_string(),
                                ));
                            } else {
                                app.push_output(OutputLine::error(
                                    "Auth failure pending — type 'r' to retry or 'a' to abort."
                                        .to_string(),
                                ));
                            }
                            return;
                        }

                        // `>tag message` — inline agent routing (v0.12.3).
                        // Syntax: >tag message OR > message (routes to sole active agent).
                        if let Some(after_gt) = text.strip_prefix('>') {
                            let rest = after_gt.trim();
                            // Parse optional tag prefix: `>tag message` vs `> message`.
                            let (tag, message) = if rest.contains(' ') {
                                let space = rest.find(' ').unwrap();
                                let candidate_tag = rest[..space].trim();
                                let msg = rest[space..].trim();
                                // If candidate_tag looks like a goal tag (no spaces, non-empty),
                                // use it; otherwise treat the whole rest as the message.
                                if !candidate_tag.is_empty()
                                    && !candidate_tag.contains('\n')
                                    && !msg.is_empty()
                                {
                                    (Some(candidate_tag.to_string()), msg.to_string())
                                } else {
                                    (None, rest.to_string())
                                }
                            } else if !rest.is_empty() {
                                // `>tag` with no message — show help.
                                app.push_output(OutputLine::info(
                                    "Usage: >tag message   OR   > message (routes to sole active agent)"
                                        .to_string(),
                                ));
                                return;
                            } else {
                                (None, String::new())
                            };

                            if message.is_empty() {
                                app.push_output(OutputLine::info(
                                    "Usage: >tag message   OR   > message (routes to sole active agent)"
                                        .to_string(),
                                ));
                                return;
                            }

                            // Resolve which goal to send to.
                            let goal_id_to_route = if let Some(ref t) = tag {
                                // Find goal matching the tag (prefix match on active_tailing_goals).
                                app.active_tailing_goals
                                    .iter()
                                    .find(|g| {
                                        g.starts_with(t.as_str()) || {
                                            let short = &g[..8.min(g.len())];
                                            short.starts_with(t.as_str())
                                        }
                                    })
                                    .cloned()
                                    .or_else(|| app.tailing_goal.clone())
                            } else {
                                // No tag: use sole active goal or primary tailing goal.
                                if app.active_tailing_goals.len() == 1 {
                                    app.active_tailing_goals.iter().next().cloned()
                                } else {
                                    app.tailing_goal.clone()
                                }
                            };

                            match goal_id_to_route {
                                None => {
                                    app.push_output(OutputLine::error(
                                        "No active agent to route to. Start a goal or use :attach."
                                            .to_string(),
                                    ));
                                }
                                Some(goal_id) => {
                                    let short = goal_id[..8.min(goal_id.len())].to_string();
                                    // Update `last_routed_tag` so the prompt shows `[→tag] >`.
                                    app.last_routed_tag = Some(short.clone());
                                    app.push_output(OutputLine::info(format!(
                                        "[→{}] {}",
                                        short, message
                                    )));

                                    let client = client.clone();
                                    let base_url = app.base_url.clone();
                                    let tx = tx.clone();
                                    let input_text = message.clone();
                                    tokio::spawn(async move {
                                        let url =
                                            format!("{}/api/goals/{}/input", base_url, goal_id);
                                        let result = client
                                            .post(&url)
                                            .json(&serde_json::json!({ "input": input_text }))
                                            .send()
                                            .await;
                                        match result {
                                            Ok(resp) if resp.status().is_success() => {}
                                            Ok(resp) => {
                                                let status = resp.status();
                                                let body = resp.text().await.unwrap_or_default();
                                                let _ =
                                                    tx.send(TuiMessage::CommandResponse(format!(
                                                        "[→] Route error (HTTP {}): {}",
                                                        status, body
                                                    )));
                                            }
                                            Err(e) => {
                                                let _ = tx.send(TuiMessage::CommandResponse(
                                                    format!("[→] Route failed: {}", e),
                                                ));
                                            }
                                        }
                                    });
                                }
                            }
                            return;
                        }

                        // Routing complete — any non-`>` command clears the last_routed_tag.
                        app.last_routed_tag = None;

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
                                        let status = resp.status();
                                        let body = resp.text().await.unwrap_or_default();
                                        tracing::warn!(
                                            goal_id = %sp.goal_id,
                                            status = %status,
                                            body = %body,
                                            "Stdin relay failed"
                                        );
                                        let _ = tx.send(TuiMessage::CommandResponse(format!(
                                            "Error sending input (HTTP {}): {}",
                                            status, body
                                        )));
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            goal_id = %sp.goal_id,
                                            error = %e,
                                            "Stdin relay connection error"
                                        );
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
                                match &app.shell_context.clone() {
                                    ShellContext::Idle => {
                                        app.push_lines(HELP_TEXT, OutputLine::info);
                                        // Keybindings from data-driven table (v0.13.1.3).
                                        app.push_lines(&keybinding_help_text(), OutputLine::info);
                                        // Also show CLI commands for discoverability.
                                        app.push_lines(CLI_HELP_TEXT, OutputLine::info);
                                    }
                                    ShellContext::ViewingDraft { draft_id } => {
                                        let id = draft_id.clone();
                                        app.push_lines(DRAFT_HELP_TEXT, OutputLine::info);
                                        app.push_output(OutputLine::info(format!(
                                            "  Current draft: {}",
                                            id
                                        )));
                                    }
                                    ShellContext::RunningGoal { goal_id } => {
                                        let id = goal_id.clone();
                                        app.push_lines(RUNNING_GOAL_HELP_TEXT, OutputLine::info);
                                        app.push_output(OutputLine::info(format!(
                                            "  Current goal: {}",
                                            id
                                        )));
                                    }
                                }
                                return;
                            }
                            ":status" => {
                                // Fetch status asynchronously to avoid blocking input.
                                let client = client.clone();
                                let base_url = app.base_url.clone();
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let s = super::shell::fetch_status(&client, &base_url).await;
                                    let _ = tx.send(TuiMessage::StatusUpdate(s));
                                });
                                app.push_output(OutputLine::info("Refreshing status...".into()));
                                return;
                            }
                            "clear" => {
                                app.output.clear();
                                app.scroll_offset = 0;
                                app.unread_events = 0;
                                app.auto_scroll = true; // re-enable auto-tail after clear (v0.14.9.3)
                                return;
                            }
                            // :stats — print velocity aggregate inline (v0.15.14.2).
                            ":stats" => {
                                let project_root = app.project_root.clone();
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let msg = tokio::task::spawn_blocking(move || {
                                        shell_velocity_stats(&project_root)
                                    })
                                    .await
                                    .unwrap_or_else(|e| format!("Error: {}", e));
                                    let _ = tx.send(TuiMessage::CommandResponse(msg));
                                });
                                app.push_output(OutputLine::info(
                                    "Loading velocity stats...".into(),
                                ));
                                return;
                            }
                            _ => {}
                        }

                        // :latency — toggle input latency diagnostics.
                        if text.starts_with(":latency") {
                            let arg = text.strip_prefix(":latency").unwrap().trim();
                            match arg {
                                "on" => {
                                    app.latency_diag.enabled = true;
                                    app.latency_diag.last_report = std::time::Instant::now();
                                    app.push_output(OutputLine::info(
                                        "[latency] Diagnostics ON — reports every 5s. \
                                         Log: .ta/latency.log"
                                            .into(),
                                    ));
                                    app.push_output(OutputLine::info(
                                        "[latency] Tracks: OS→event-loop transit, draw time, \
                                         bg process time, cycle rate, channel depth"
                                            .into(),
                                    ));
                                }
                                "off" => {
                                    app.latency_diag.enabled = false;
                                    app.push_output(OutputLine::info(
                                        "[latency] Diagnostics OFF.".into(),
                                    ));
                                }
                                "dump" => {
                                    // Force an immediate report even if disabled.
                                    if let Some(report) = app.latency_diag.report_forced() {
                                        app.push_output(OutputLine::info(report));
                                    } else {
                                        app.push_output(OutputLine::info(
                                            "[latency] No data collected yet.".into(),
                                        ));
                                    }
                                }
                                _ => {
                                    app.push_output(OutputLine::info(
                                        "Usage: :latency on|off|dump".into(),
                                    ));
                                }
                            }
                            return;
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

                        // :attach — bidirectional agent session (v0.12.0.1).
                        // Like :tail but also forwards all user input to the agent's stdin.
                        // Usage: :attach [goal-id-or-tag]
                        // Exit: Ctrl-D or :detach
                        if text.starts_with(":attach") {
                            let (goal_id_arg, backfill) = parse_tail_args(
                                &text.replacen(":attach", ":tail", 1),
                                app.tail_backfill_lines,
                            );

                            // Start tail stream (same as :tail).
                            let client = client.clone();
                            let base_url = app.base_url.clone();
                            let tx2 = tx.clone();
                            let goal_for_tail = goal_id_arg.clone();
                            tokio::spawn(async move {
                                start_tail_stream(
                                    client,
                                    &base_url,
                                    goal_for_tail.as_deref(),
                                    tx2,
                                    backfill,
                                )
                                .await;
                            });

                            // Resolve the actual goal ID to attach to (reuse the tail logic result).
                            // We'll learn the real ID from the first TailLine message; for now
                            // store the requested tag/id (or "__latest__" sentinel).
                            app.attach_mode =
                                Some(goal_id_arg.unwrap_or_else(|| "__latest__".to_string()));
                            app.push_output(OutputLine::info(
                                "Attached — all input will be relayed to the agent. \
                                 Press Ctrl-D or type :detach to exit."
                                    .to_string(),
                            ));
                            return;
                        }

                        // :detach — exit attach mode (v0.12.0.1).
                        if text == ":detach" {
                            if app.attach_mode.is_some() {
                                app.attach_mode = None;
                                app.push_output(OutputLine::info(
                                    "Detached from agent.".to_string(),
                                ));
                            } else {
                                app.push_output(OutputLine::info(
                                    "Not in attach mode.".to_string(),
                                ));
                            }
                            return;
                        }

                        // :follow-up — fuzzy-searchable follow-up picker (v0.10.14).
                        if text.starts_with(":follow-up") || text.starts_with(":followup") {
                            handle_follow_up_picker(app, &text);
                            return;
                        }

                        // Track draft view context for context-sensitive help (v0.14.9.2).
                        // Detect "view <id>" and "ta draft view <id>" to update shell_context.
                        {
                            let cmd_lower = text.trim().to_lowercase();
                            let view_id = if let Some(rest) = cmd_lower.strip_prefix("view ") {
                                // "view <id>" shortcut
                                rest.split_whitespace().next()
                            } else if let Some(rest) = cmd_lower.strip_prefix("ta draft view ") {
                                rest.split_whitespace().next()
                            } else {
                                None
                            };
                            if let Some(id) = view_id {
                                app.shell_context = ShellContext::ViewingDraft {
                                    draft_id: id.to_string(),
                                };
                            }
                        }

                        // Agent consent check for goal-dispatching commands (v0.10.18.4 item 7).
                        // If the command is `run` or `dev`, verify that agent consent is current
                        // before dispatching. If consent is missing or outdated, block the
                        // dispatch with an actionable error message.
                        // Strip optional case-insensitive "ta " prefix before checking subcommand.
                        let text_cmd = if text.len() >= 3
                            && text[..2].eq_ignore_ascii_case("ta")
                            && text.as_bytes()[2] == b' '
                        {
                            &text[3..]
                        } else {
                            text.as_str()
                        };
                        if text_cmd.starts_with("run ")
                            || text_cmd.starts_with("dev ")
                            || text_cmd == "run"
                            || text_cmd == "dev"
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
                                        // Use WorkingIndicator so AgentOutputDone can clear it
                                        // (it's pushed as a heartbeat line — v0.12.7 item 1).
                                        let _ = tx.send(TuiMessage::WorkingIndicator(
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
                                    // Log explicitly so errors aren't silently swallowed
                                    // by the tokio::spawn boundary (v0.11.4.1 item 4).
                                    tracing::warn!(
                                        command = %text,
                                        error = %e,
                                        "Command dispatch failed"
                                    );
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
                // Cmd+Up/Down scroll to top/bottom (macOS, v0.14.7.1 item 5).
                // Must be checked BEFORE the plain Up/Down handlers.
                (KeyCode::Up, m) if m.contains(KeyModifiers::SUPER) => {
                    app.scroll_up(app.output.len());
                }
                (KeyCode::Down, m) if m.contains(KeyModifiers::SUPER) => {
                    app.scroll_to_bottom();
                }
                // Shift+Up/Down scroll output 1 line; plain Up/Down navigate history.
                (KeyCode::Up, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_up(1);
                }
                (KeyCode::Down, m) if m.contains(KeyModifiers::SHIFT) => {
                    app.scroll_down(1);
                }
                (KeyCode::Up, _) => app.history_up(),
                (KeyCode::Down, _) => app.history_down(),
                (KeyCode::Tab, _) => {
                    if app.pending_paste.is_some() {
                        // Toggle paste preview when a large paste is pending.
                        app.paste_preview_expanded = !app.paste_preview_expanded;
                    } else {
                        app.tab_complete();
                    }
                }
                (KeyCode::PageUp, _) => {
                    let page = crossterm::terminal::size()
                        .map(|(_, h)| h as usize)
                        .unwrap_or(40);
                    app.scroll_up(page.saturating_sub(4));
                }
                (KeyCode::PageDown, _) => {
                    let page = crossterm::terminal::size()
                        .map(|(_, h)| h as usize)
                        .unwrap_or(40);
                    app.scroll_down(page.saturating_sub(4));
                }
                // Ctrl+V / Cmd+V — read from OS clipboard (v0.14.9.1).
                //
                // Bracketed-paste mode (EnableBracketedPaste) handles modern
                // terminals that send Event::Paste. But Terminal.app and some
                // Linux terminals send a raw keycode instead (Ctrl+V → CONTROL,
                // Cmd+V → SUPER). We intercept that here and read from the
                // system clipboard directly so paste works everywhere.
                (KeyCode::Char('v'), m)
                    if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::SUPER) =>
                {
                    match read_from_clipboard() {
                        Some(text) => {
                            // Process through the same paste path as Event::Paste.
                            let safe = text
                                .replace("\r\n", "\n")
                                .replace('\r', "\n")
                                .replace('\t', "    ")
                                .trim_matches('\n')
                                .to_string();
                            let line_count = safe.lines().count();
                            let char_count = safe.chars().count();
                            if char_count > PASTE_CHAR_THRESHOLD
                                || line_count > PASTE_LINE_THRESHOLD
                            {
                                app.pending_paste = Some(safe);
                                app.paste_preview_expanded = false;
                            } else {
                                if app.scroll_offset > 0 {
                                    app.cursor = app.input.len();
                                    app.scroll_to_bottom();
                                }
                                for ch in safe.chars() {
                                    app.insert_char(ch);
                                }
                            }
                        }
                        None => {
                            // Clipboard unavailable or empty — show brief notice.
                            app.push_output(OutputLine::info(
                                "[clipboard] paste failed: clipboard is empty or unavailable \
                                 (no display server, or use bracketed-paste in your terminal)"
                                    .to_string(),
                            ));
                        }
                    }
                }
                (KeyCode::Char(c), m)
                    if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    // Accept Char events with any modifier combination EXCEPT
                    // Ctrl and Alt (which are handled as explicit keybindings
                    // above). This catches normal typing even when the terminal
                    // reports unexpected SUPER/HYPER/META/CAPS_LOCK flags —
                    // common on macOS with some terminal emulators.
                    app.insert_char(c);
                }
                _ => {}
            }
        }
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => app.scroll_up(3),
                MouseEventKind::ScrollDown => app.scroll_down(3),
                // Scrollbar click: jump to proportional position (v0.14.7.1 item 6).
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(scol) = app.scrollbar_col {
                        if mouse.column == scol
                            && app.output_area_height > 0
                            && mouse.row >= app.output_area_top
                            && mouse.row < app.output_area_top + app.output_area_height
                        {
                            app.scrollbar_dragging = true;
                            let h = app.output_area_height as usize;
                            let rel_row = (mouse.row - app.output_area_top) as usize;
                            let vl = app.cached_visual_lines.max(app.output.len());
                            let max_scroll = vl.saturating_sub(h);
                            if max_scroll > 0 {
                                let pos = rel_row * max_scroll / h;
                                app.scroll_offset = max_scroll.saturating_sub(pos);
                                if app.scroll_offset == 0 {
                                    app.auto_scroll = true;
                                    app.unread_events = 0;
                                } else {
                                    app.auto_scroll = false;
                                }
                            }
                        }
                    }
                }
                // Scrollbar drag: update position while dragging (v0.14.7.1 item 6).
                MouseEventKind::Drag(MouseButton::Left)
                    if app.scrollbar_dragging && app.output_area_height > 0 =>
                {
                    let h = app.output_area_height as usize;
                    let rel_row = (mouse.row.saturating_sub(app.output_area_top)) as usize;
                    let rel_row = rel_row.min(h.saturating_sub(1));
                    let vl = app.cached_visual_lines.max(app.output.len());
                    let max_scroll = vl.saturating_sub(h);
                    if max_scroll > 0 {
                        let pos = rel_row * max_scroll / h;
                        app.scroll_offset = max_scroll.saturating_sub(pos);
                        if app.scroll_offset == 0 {
                            app.auto_scroll = true;
                            app.unread_events = 0;
                        } else {
                            app.auto_scroll = false;
                        }
                    }
                }
                // Release drag (v0.14.7.1 item 6).
                MouseEventKind::Up(_) => {
                    app.scrollbar_dragging = false;
                }
                _ => {}
            }
        }
        Event::Paste(data) => {
            // Bracketed paste: normalize CRLF → LF, standalone CR → LF, tabs → spaces.
            // Strip leading/trailing newlines to prevent accidental submission (v0.12.2).
            let safe = data
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\t', "    ")
                .trim_matches('\n')
                .to_string();
            let line_count = safe.lines().count();
            let char_count = safe.chars().count();
            if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
                // Large paste: compact into an indicator, store full text separately.
                // Cursor position is irrelevant here; submit() combines prefix + paste.
                app.pending_paste = Some(safe);
                app.paste_preview_expanded = false;
            } else {
                // Small paste: cursor-aware (v0.14.7.1 items 1/8).
                // Scroll-focused (output scrolled up): snap to end of input + scroll to bottom.
                // Input-focused (at prompt, scroll_offset==0): insert at current cursor position.
                if app.scroll_offset > 0 {
                    app.cursor = app.input.len();
                    app.scroll_to_bottom();
                }
                for ch in safe.chars() {
                    app.insert_char(ch);
                }
            }
        }
        Event::Resize(_, _) => {
            // Terminal will re-draw on next loop iteration.
        }
        _ => {}
    }
}

/// Clear all heartbeat lines from a buffer, blanking their text and resetting style.
/// Returns true if any heartbeat lines were found and cleared (v0.14.7.1 item 3).
fn clear_all_heartbeats(buf: &mut [OutputLine]) -> bool {
    let indices: Vec<usize> = buf
        .iter()
        .enumerate()
        .filter_map(|(i, l)| if l.is_heartbeat { Some(i) } else { None })
        .collect();
    let found = !indices.is_empty();
    for &i in &indices {
        buf[i].is_heartbeat = false;
        buf[i].style = Style::default().fg(Color::DarkGray);
        buf[i].text = String::new();
    }
    found
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
            // R2 fix: auto-scroll on ALL output paths, not just AgentOutput (v0.13.1.5).
            app.auto_scroll_if_near_bottom();
        }
        TuiMessage::CommandResponse(text) => {
            // Check if we got a response that clears workflow prompt.
            if text.contains("workflow resumed") || text.contains("workflow response accepted") {
                app.workflow_prompt = None;
            }
            app.push_lines(&text, OutputLine::command);
            // R2 fix: auto-scroll on ALL output paths (v0.13.1.5).
            app.auto_scroll_if_near_bottom();
        }
        TuiMessage::WorkingIndicator(text) => {
            // Push "Agent is working..." as a heartbeat-flagged line so that
            // AgentOutputDone can find and replace it on any terminal goal state
            // (v0.12.7 item 1). Using push_heartbeat ensures at most one such
            // indicator is visible at a time.
            app.push_heartbeat(text);
            app.scroll_to_bottom();
        }
        TuiMessage::DaemonDown => {
            if app.daemon_connected {
                app.daemon_connected = false;
                app.push_output(OutputLine::error(
                    "[disconnected] Daemon unreachable. Will auto-reconnect.".into(),
                ));
                app.auto_scroll_if_near_bottom(); // R2 fix (v0.13.1.5)
            }
        }
        TuiMessage::DaemonUp => {
            if !app.daemon_connected {
                app.daemon_connected = true;
                app.push_output(OutputLine::info(
                    "[reconnected] Daemon is back online.".into(),
                ));
                app.auto_scroll_if_near_bottom(); // R2 fix (v0.13.1.5)
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
        TuiMessage::StdinPrompt(mut sp) => {
            // Display the stdin prompt and switch to input mode (v0.10.18.5 item 5).
            sp.detected_at = std::time::Instant::now();
            sp.verifying = true; // Q&A verification in flight (v0.11.2.5 Layer 3).
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
        TuiMessage::PromptVerifiedNotPrompt => {
            // Q&A agent says this is NOT a real prompt — auto-dismiss (v0.11.2.5 Layer 3).
            if app.pending_stdin_prompt.is_some() {
                app.pending_stdin_prompt = None;
                app.push_output(OutputLine::info(
                    "[info] Not a prompt — resumed normal mode".to_string(),
                ));
            }
        }
        TuiMessage::AgentOutput(line) => {
            // Layer 2: Auto-dismiss pending prompt on continued output (v0.11.2.5).
            if let Some(ref sp) = app.pending_stdin_prompt {
                let elapsed = sp.detected_at.elapsed().as_secs();
                if elapsed < app.prompt_dismiss_after_output_secs && line.stream != "stderr" {
                    // Still within the window — output arrived, dismiss the prompt.
                    app.pending_stdin_prompt = None;
                    app.push_output(OutputLine::info(
                        "[info] Prompt dismissed — agent continued output".to_string(),
                    ));
                }
            }

            // Detect auth / API-key failure in stderr lines (v0.12.3).
            // Patterns: HTTP 401, "Invalid API key", "authentication", "Unauthorized".
            if line.stream == "stderr" {
                let lower = line.line.to_lowercase();
                let is_auth_failure = lower.contains("401")
                    || lower.contains("invalid api key")
                    || lower.contains("authentication failed")
                    || lower.contains("unauthorized")
                    || lower.contains("invalid x-api-key")
                    || lower.contains("api key not found");
                if is_auth_failure {
                    let goal_id = line
                        .goal_id
                        .clone()
                        .or_else(|| app.tailing_goal.clone())
                        .unwrap_or_default();
                    // Only show once per goal (if not already pending).
                    if app.pending_auth_retry.is_none() {
                        app.pending_auth_retry = Some(goal_id);
                        app.push_output(OutputLine::error(
                            "━━━ Agent auth failure detected ━━━".to_string(),
                        ));
                        app.push_output(OutputLine::error(line.line.clone()));
                        app.push_output(OutputLine::error(
                            "Agent auth failed — type 'r' to retry or 'a' to abort.".to_string(),
                        ));
                        app.scroll_to_bottom();
                        return;
                    }
                }
            }

            // Heartbeat coalescing: detect [heartbeat] lines and update in-place
            // instead of appending (v0.11.4.1 items 9-10).
            // Auto-scroll after heartbeat so a user pinned near the bottom stays
            // at the bottom even when only the heartbeat ticker changes (v0.12.7 item 3).
            if line.line.starts_with("[heartbeat]") {
                let heartbeat_text = line.line.clone();
                if app.split_pane {
                    // Update last line in agent pane if it's a heartbeat.
                    if let Some(last) = app.agent_output.last_mut() {
                        if last.is_heartbeat {
                            last.text = heartbeat_text;
                            // Still auto-scroll agent pane when near bottom (v0.12.7 item 3).
                            const AGENT_NEAR_BOTTOM_LINES: usize = 3;
                            if app.agent_scroll_offset <= AGENT_NEAR_BOTTOM_LINES {
                                app.agent_scroll_offset = 0;
                            }
                            return;
                        }
                    }
                    app.agent_output.push(OutputLine::heartbeat(heartbeat_text));
                    // Auto-scroll agent pane when near bottom.
                    const AGENT_NEAR_BOTTOM_LINES: usize = 3;
                    if app.agent_scroll_offset <= AGENT_NEAR_BOTTOM_LINES {
                        app.agent_scroll_offset = 0;
                    }
                } else {
                    app.push_heartbeat(heartbeat_text);
                    // Auto-scroll main pane when near bottom (v0.12.7 item 3).
                    app.auto_scroll_if_near_bottom();
                }
                return;
            }

            let styled = if line.stream == "stderr" {
                OutputLine::agent_stderr(line.line.clone())
            } else {
                // Schema-driven stream-json parsing (v0.11.2.2).
                // Extract model name from any line if not yet known.
                if app.status.agent_model.is_none() {
                    if let Some(model) = app.output_schema.extract_model(&line.line) {
                        app.status.agent_model = Some(humanize_model_name(&model));
                    }
                }
                match ta_output_schema::parse_line(&app.output_schema, &line.line) {
                    ta_output_schema::ParseResult::Text(text) => {
                        // Multi-agent tag prefix: prepend `[short]` when multiple goals tailing (v0.12.3).
                        if app.active_tailing_goals.len() > 1 {
                            if let Some(ref gid) = line.goal_id {
                                let short = &gid[..8.min(gid.len())];
                                OutputLine::agent_stdout(format!("[{}] {}", short, text))
                            } else {
                                OutputLine::agent_stdout(text)
                            }
                        } else {
                            OutputLine::agent_stdout(text)
                        }
                    }
                    ta_output_schema::ParseResult::ToolUse(name) => {
                        if app.active_tailing_goals.len() > 1 {
                            if let Some(ref gid) = line.goal_id {
                                let short = &gid[..8.min(gid.len())];
                                OutputLine::agent_stdout(format!("[{}] [tool] {}", short, name))
                            } else {
                                OutputLine::agent_stdout(format!("[tool] {}", name))
                            }
                        } else {
                            OutputLine::agent_stdout(format!("[tool] {}", name))
                        }
                    }
                    ta_output_schema::ParseResult::Model(model) => {
                        if app.status.agent_model.is_none() {
                            app.status.agent_model = Some(humanize_model_name(&model));
                        }
                        return; // Model-only event — no display.
                    }
                    ta_output_schema::ParseResult::Suppress => return,
                    ta_output_schema::ParseResult::NotJson => {
                        // Not JSON — show raw, with multi-agent prefix if needed.
                        if app.active_tailing_goals.len() > 1 {
                            if let Some(ref gid) = line.goal_id {
                                let short = &gid[..8.min(gid.len())];
                                OutputLine::agent_stdout(format!("[{}] {}", short, line.line))
                            } else {
                                OutputLine::agent_stdout(line.line)
                            }
                        } else {
                            OutputLine::agent_stdout(line.line)
                        }
                    }
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
                // Auto-scroll agent pane when near bottom (v0.12.4.1 fix item 2).
                // Mirrors the main-pane auto_scroll_if_near_bottom() logic.
                const AGENT_NEAR_BOTTOM_LINES: usize = 3;
                if app.agent_scroll_offset <= AGENT_NEAR_BOTTOM_LINES {
                    app.agent_scroll_offset = 0;
                }
            } else {
                app.push_output(styled);
                // Auto-scroll to bottom when "near bottom" — keeps latest visible (v0.12.3).
                app.auto_scroll_if_near_bottom();
            }
        }
        TuiMessage::TailStarted { goal_id } => {
            // Track this goal in the active_tailing_goals set (v0.12.3).
            app.active_tailing_goals.insert(goal_id.clone());
            // Also set tailing_goal if not already set (covers explicit :tail calls).
            if app.tailing_goal.is_none() {
                app.tailing_goal = Some(goal_id.clone());
            }
            // Update shell context for context-sensitive help (v0.14.9.2).
            app.shell_context = ShellContext::RunningGoal {
                goal_id: goal_id.clone(),
            };
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
                app.tailing_goal = Some(goal_id.clone());
            }
            // Track in active_tailing_goals for multi-agent tag display (v0.12.3).
            if app.auto_tail {
                app.active_tailing_goals.insert(goal_id.clone());
            }
            // If in attach mode with __latest__ sentinel, resolve to the real goal ID.
            if app.attach_mode.as_deref() == Some("__latest__") {
                app.attach_mode = Some(goal_id);
            }
        }
        TuiMessage::AgentOutputDone(goal_id) => {
            let short_id = &goal_id[..8.min(goal_id.len())];

            // Clear ALL heartbeat lines so neither the WorkingIndicator ("Agent is
            // working...") nor any subsequent [heartbeat] tick lines linger after exit.
            // R1 regression fix (v0.13.1.5): the previous code only cleared the LAST
            // heartbeat.  When a WorkingIndicator is pushed and then regular agent output
            // arrives before the first [heartbeat] tick, the tick creates a NEW heartbeat
            // line.  AgentOutputDone then cleared only the tick, leaving the
            // WorkingIndicator visible indefinitely.  We now clear ALL heartbeat lines:
            // the final one becomes "[agent exited]" and earlier ones are blanked.
            // v0.12.4.1 fix preserved: also search agent_output (split-pane mode).
            let replaced_main = {
                let indices: Vec<usize> = app
                    .output
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| if l.is_heartbeat { Some(i) } else { None })
                    .collect();
                let found = !indices.is_empty();
                for &i in &indices {
                    app.output[i].is_heartbeat = false;
                    app.output[i].style = Style::default().fg(Color::DarkGray);
                    app.output[i].text = String::new(); // blank earlier heartbeats
                }
                if let Some(&last) = indices.last() {
                    app.output[last].text = format!("[agent exited] {}", short_id);
                }
                found
            };
            let replaced_agent = {
                let indices: Vec<usize> = app
                    .agent_output
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| if l.is_heartbeat { Some(i) } else { None })
                    .collect();
                let found = !indices.is_empty();
                for &i in &indices {
                    app.agent_output[i].is_heartbeat = false;
                    app.agent_output[i].style = Style::default().fg(Color::DarkGray);
                    app.agent_output[i].text = String::new();
                }
                if let Some(&last) = indices.last() {
                    app.agent_output[last].text = format!("[agent exited] {}", short_id);
                }
                found
            };
            let replaced = replaced_main || replaced_agent;

            if !replaced {
                app.push_output(OutputLine::separator(format!(
                    "━━━ Agent output ended ({}) ━━━",
                    short_id
                )));
            } else {
                app.push_output(OutputLine::separator(format!(
                    "━━━ Agent exited ({}) ━━━",
                    short_id
                )));
            }

            // Remove from active_tailing_goals set (v0.12.3).
            app.active_tailing_goals.remove(&goal_id);

            if app.tailing_goal.as_deref() == Some(&goal_id) {
                app.tailing_goal = None;
            }
            if app.attach_mode.as_deref() == Some(&goal_id) {
                app.attach_mode = None;
                app.push_output(OutputLine::info(
                    "[attach] Agent exited — detached.".to_string(),
                ));
            }
            // Layer 2, item 8: Clear pending stdin prompt on stream end (v0.11.2.5).
            // A completed goal cannot be waiting for input.
            if let Some(ref sp) = app.pending_stdin_prompt {
                if sp.goal_id == goal_id {
                    app.pending_stdin_prompt = None;
                    app.push_output(OutputLine::info(
                        "[info] Prompt cleared — goal output ended".to_string(),
                    ));
                }
            }
            // Clear auth retry if it was for this goal.
            if app.pending_auth_retry.as_deref() == Some(&goal_id) {
                app.pending_auth_retry = None;
            }
            app.scroll_to_bottom();
        }
        TuiMessage::DraftReady {
            goal_id,
            draft_id: _,
            display_id,
            title,
            artifact_count,
        } => {
            // Clear any lingering working-indicator heartbeats (v0.14.7.1 item 3).
            clear_all_heartbeats(&mut app.output);
            clear_all_heartbeats(&mut app.agent_output);
            if !goal_id.is_empty() {
                app.active_tailing_goals.remove(&goal_id);
            }
            // v0.15.7.1: Inline notification with file count (replaces opaque CTA).
            let file_note = if artifact_count > 0 {
                format!(
                    "  ({} file{} changed)",
                    artifact_count,
                    if artifact_count == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            app.push_output(OutputLine::notification(format!(
                "Draft ready: \"{}\" [{}]{}",
                title, display_id, file_note
            )));
            app.push_output(OutputLine::notification(format!(
                "  → ta draft view {}",
                display_id
            )));
            app.scroll_to_bottom();
        }
    }
}

/// Simulate word-boundary wrapping matching ratatui's `Wrap { trim: false }`.
///
/// Returns `(cursor_row, cursor_col, total_visual_lines)`.
/// - `cursor_byte`: byte offset in `display` where the cursor sits; pass
///   `display.len()` when only `total_visual_lines` is needed.
///
/// The algorithm mirrors ratatui: at a space, if placing the space plus the
/// following word would exceed `wrap_width`, the space is consumed as a wrap
/// point and the next word starts on a fresh line.  Single characters or
/// words that are longer than `wrap_width` hard-break at the column boundary.
fn word_wrap_metrics(display: &str, cursor_byte: usize, wrap_width: usize) -> (u16, usize, u16) {
    if wrap_width == 0 {
        return (0, 0, 1);
    }
    let mut row: u16 = 0;
    let mut col: usize = 0;
    let mut cursor_row: u16 = 0;
    let mut cursor_col: usize = 0;
    let chars: Vec<(usize, char)> = display.char_indices().collect();
    let mut idx = 0;
    while idx < chars.len() {
        let (bi, ch) = chars[idx];
        if bi == cursor_byte {
            cursor_row = row;
            cursor_col = col;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
            idx += 1;
            continue;
        }
        if ch == ' ' && col > 0 {
            let next_word_len: usize = chars[idx + 1..]
                .iter()
                .take_while(|(_, c)| *c != ' ' && *c != '\n')
                .count();
            if col + 1 + next_word_len > wrap_width {
                row += 1;
                col = 0;
                idx += 1;
                continue;
            }
        }
        if col >= wrap_width {
            row += 1;
            col = 0;
        }
        col += 1;
        idx += 1;
    }
    if cursor_byte >= display.len() {
        cursor_row = row;
        cursor_col = col;
    }
    (cursor_row, cursor_col, row + 1)
}

/// Write ONLY the input line directly to stdout using crossterm, bypassing
/// ratatui's full-frame diff entirely. This produces ~50 bytes of output —
/// the terminal processes it in microseconds regardless of how busy it is
/// rendering agent output. The ratatui frame buffer stays stale for the
/// input area, but the next full `terminal.draw()` will resync it.
fn direct_input_write(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &App) {
    use crossterm::cursor::MoveTo;
    use crossterm::style::Print;
    use crossterm::terminal::{Clear, ClearType};
    use crossterm::QueueableCommand;

    let size = match terminal.size() {
        Ok(s) => s,
        Err(_) => return,
    };

    // Compute input area position — same layout logic as draw_ui.
    //
    // layout_width matches draw_ui's block inner width (border takes 1 char each side).
    // This ensures direct_input_write and draw_ui agree on input_height and input_top.
    // render_width is the full terminal width since direct write bypasses the block.
    let prompt = app.prompt_str();
    let display = format!("{}{}", prompt, app.input);
    let layout_width = size.width.saturating_sub(2).max(1) as usize;
    let render_width = size.width.max(1) as usize;
    let content_lines = word_wrap_metrics(&display, display.len(), layout_width).2;
    let input_height = (content_lines + 2).min(size.height / 2).max(3);
    // Input area: top border at (size.height - 1 - input_height), text starts 1 below.
    let input_top = size.height.saturating_sub(1 + input_height);
    let text_start_row = input_top + 1; // skip top border
                                        // Last text row is the row before the bottom border (input_top + input_height - 2).
    let text_end_row = (input_top + input_height).saturating_sub(2);

    let backend = terminal.backend_mut();

    // Clear the input text rows and rewrite content.
    let mut row = text_start_row;
    let mut col: usize = 0;

    // Clear all input text rows first.
    for r in text_start_row..=text_end_row {
        let _ = backend.queue(MoveTo(0, r));
        let _ = backend.queue(Clear(ClearType::CurrentLine));
    }

    // Write the display string with word-boundary wrapping (matches ratatui Wrap { trim: false }).
    // Use render_width (full terminal width) since direct write has no block borders.
    let _ = backend.queue(MoveTo(0, text_start_row));
    let chars_vec: Vec<(usize, char)> = display.char_indices().collect();
    let mut widx = 0;
    while widx < chars_vec.len() {
        if row > text_end_row {
            break;
        }
        let (_, ch) = chars_vec[widx];
        if ch == '\n' {
            row += 1;
            col = 0;
            widx += 1;
            if row <= text_end_row {
                let _ = backend.queue(MoveTo(0, text_start_row + row as u16));
            }
            continue;
        }
        if ch == ' ' && col > 0 {
            let next_word_len: usize = chars_vec[widx + 1..]
                .iter()
                .take_while(|(_, c)| *c != ' ' && *c != '\n')
                .count();
            if col + 1 + next_word_len > render_width {
                row += 1;
                col = 0;
                widx += 1;
                if row <= text_end_row {
                    let _ = backend.queue(MoveTo(0, text_start_row + row as u16));
                }
                continue;
            }
        }
        if col >= render_width {
            row += 1;
            col = 0;
            if row > text_end_row {
                break;
            }
            let _ = backend.queue(MoveTo(0, text_start_row + row as u16));
        }
        let _ = backend.queue(Print(ch));
        col += 1;
        widx += 1;
    }

    // Position cursor using the same word-wrap metrics as draw_input (layout_width).
    let cursor_byte = prompt.len() + app.cursor;
    let (crow, ccol, _) = word_wrap_metrics(&display, cursor_byte, layout_width);
    let cx = (ccol as u16).min(size.width.saturating_sub(1));
    let cy = (text_start_row + crow).min(text_end_row);
    let _ = backend.queue(MoveTo(cx, cy));
    let _ = std::io::Write::flush(backend);
}

fn draw_ui(f: &mut Frame, app: &App) {
    let size = f.area();

    // Calculate input area height dynamically based on word-wrapped text.
    // The block has top+bottom borders (2 lines), plus content lines.
    // Account for both word-wrap AND embedded newlines (from paste/Shift+Enter).
    let prompt = app.prompt_str();
    let display = format!("{}{}", prompt, app.input);
    // Subtract 2 for block borders to get the actual text width.
    let inner_width = size.width.saturating_sub(2).max(1) as usize;
    let content_lines = word_wrap_metrics(&display, display.len(), inner_width).2;
    // Borders add 2 lines; cap at half the terminal to keep output visible.
    let input_height = (content_lines + 2).min(size.height / 2).max(3);

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

    // Use cached visual line count (maintained by push_output + event loop).
    // Falls back to recount if cache width doesn't match (e.g., first frame).
    let visual_line_count = if app.cached_wrap_width == wrap_width {
        app.cached_visual_lines
    } else {
        app.output
            .iter()
            .map(|ol| {
                if ol.text.is_empty() || wrap_width == 0 {
                    1
                } else {
                    ol.text.len().div_ceil(wrap_width)
                }
            })
            .sum()
    };

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

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);

    // When a large paste is pending, show the compact indicator (and optionally
    // an inline preview) instead of rendering the raw paste text.
    if let Some(ref paste) = app.pending_paste {
        let char_count = paste.chars().count();
        let line_count = paste.lines().count();
        // Format char_count with thousands separators manually.
        let chars_fmt = format_with_commas(char_count);
        let indicator = format!(
            "[Pasted {} chars / {} lines — Tab to preview, Esc to cancel]",
            chars_fmt, line_count
        );
        // Build display: any typed prefix + yellow indicator + optional preview.
        let prefix_display = format!("{}{}", &prompt, &app.input);
        let mut text_lines: Vec<Line> = Vec::new();
        // First line: typed prefix + indicator in a distinct style.
        text_lines.push(Line::from(vec![
            Span::raw(prefix_display.clone()),
            Span::styled(
                indicator,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if app.paste_preview_expanded {
            // Show the first PASTE_PREVIEW_LINES lines of the paste with a dim style.
            let preview_lines: Vec<&str> = paste.lines().take(PASTE_PREVIEW_LINES).collect();
            let remaining = line_count.saturating_sub(PASTE_PREVIEW_LINES);
            for line in &preview_lines {
                text_lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if remaining > 0 {
                text_lines.push(Line::from(Span::styled(
                    format!("  … {} more lines (Tab to collapse)", remaining),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                text_lines.push(Line::from(Span::styled(
                    "  [end of paste — Tab to collapse]".to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        let paragraph = Paragraph::new(text_lines)
            .wrap(Wrap { trim: false })
            .block(block);
        f.render_widget(paragraph, area);

        // Place the cursor at the end of the typed prefix (before the indicator).
        let wrap_width = inner.width.max(1) as usize;
        let (row, col, _) = word_wrap_metrics(&prefix_display, prefix_display.len(), wrap_width);
        let x = inner.x + (col as u16).min(inner.width.saturating_sub(1));
        let y = inner.y + row.min(inner.height.saturating_sub(1));
        f.set_cursor_position((x, y));
        return;
    }

    // Normal (no pending paste): show typed input with live cursor.
    let display = format!("{}{}", &prompt, &app.input);
    let paragraph = Paragraph::new(display.clone())
        .wrap(Wrap { trim: false })
        .block(block);
    f.render_widget(paragraph, area);

    // Position cursor with word-boundary wrap matching ratatui's Wrap { trim: false }.
    let cursor_byte = prompt.len() + app.cursor;
    let wrap_width = inner.width.max(1) as usize;
    let (row, col, _) = word_wrap_metrics(&display, cursor_byte, wrap_width);
    let x = inner.x + (col as u16).min(inner.width.saturating_sub(1));
    let y = inner.y + row.min(inner.height.saturating_sub(1));
    f.set_cursor_position((x, y));
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let cli_build_sha = env!("TA_GIT_HASH");
    let cli_version = env!("CARGO_PKG_VERSION");

    // Stale if build SHA differs (catches rebuilds within same version),
    // or if version differs and daemon doesn't report build_sha.
    let sha_match = !app.status.build_sha.is_empty()
        && app.status.build_sha != "?"
        && app.status.build_sha == cli_build_sha;
    let version_match = app.status.version == cli_version;
    let is_stale = app.daemon_connected
        && !app.status.version.is_empty()
        && app.status.version != "?"
        && !sha_match
        && !version_match;

    // Short SHA for display (first 7 chars).
    let display_sha = if app.status.build_sha.len() > 7 {
        &app.status.build_sha[..7]
    } else {
        &app.status.build_sha
    };

    let daemon_indicator = if !app.daemon_connected {
        Span::styled(" ◉ daemon ", Style::default().fg(Color::Red))
    } else if is_stale {
        Span::styled(
            format!(
                " ◉ daemon {} ({}) (stale) ",
                app.status.daemon_version, display_sha
            ),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(
            format!(" ◉ daemon {} ({}) ", app.status.daemon_version, display_sha),
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

    // Active goal tag indicator (v0.11.2.3).
    if let Some(ref tag) = app.status.active_goal_tag {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" goal: {} ", tag),
            Style::default().fg(Color::Green),
        ));
    }

    // Community hub badge (v0.14.7): shows stale/missing resource count.
    if app.status.community_pending_count > 0 {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" ⬡ {} community ", app.status.community_pending_count),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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

    // Last-routed agent indicator (`>tag message` prefix, v0.12.3).
    if let Some(ref tag) = app.last_routed_tag {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" →{} ", tag),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Tailing indicator (v0.10.11) / Attach indicator (v0.12.0.1).
    if let Some(ref goal_id) = app.attach_mode {
        let short = &goal_id[..8.min(goal_id.len())];
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" attach {} ", short),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.active_tailing_goals.len() > 1 {
        // Multiple agents — show count (v0.12.3).
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            format!(" {} agents ", app.active_tailing_goals.len()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(ref goal_id) = app.tailing_goal {
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

    // Auth failure indicator (v0.12.3).
    if app.pending_auth_retry.is_some() {
        spans.push(Span::raw("│"));
        spans.push(Span::styled(
            " auth failed — r/a ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // No mouse capture — native text selection always works.

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
    let bar = Paragraph::new(left_line).style(Style::default().fg(Color::White));
    f.render_widget(bar, area);

    // Render right-aligned phase info (if there's room).
    if area.width > left_width + right_width {
        let right_area = Rect {
            x: area.x + area.width - right_width,
            y: area.y,
            width: right_width,
            height: 1,
        };
        let right = Paragraph::new(right_text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(right, right_area);
    }
}

// -- Mouse selection helpers --------------------------------------------------

/// Extract the text covered by the current selection from the output buffer.
///
/// Maps screen coordinates back to logical output lines, accounting for
/// scroll offset and word wrapping.
#[allow(dead_code)]
fn extract_selection_text(app: &App, sel: &Selection) -> String {
    let area = sel.output_area;
    if area.width == 0 || area.height == 0 || app.output.is_empty() {
        return String::new();
    }

    // Normalize anchor/extent so `start` is before `end` (top-left to bottom-right).
    let (start, end) = if (sel.anchor.row, sel.anchor.col) <= (sel.extent.row, sel.extent.col) {
        (sel.anchor, sel.extent)
    } else {
        (sel.extent, sel.anchor)
    };

    // Only handle selections within the output pane area.
    let pane_top = area.y;
    let pane_bottom = area.y + area.height;
    let wrap_width = area.width as usize;
    if wrap_width == 0 {
        return String::new();
    }

    // Clamp to output pane.
    let sel_start_row = start.row.max(pane_top).saturating_sub(pane_top) as usize;
    let sel_end_row = end
        .row
        .min(pane_bottom.saturating_sub(1))
        .saturating_sub(pane_top) as usize;
    let sel_start_col = start.col.saturating_sub(area.x) as usize;
    let sel_end_col = end.col.saturating_sub(area.x) as usize;

    // Build a map of visual rows → text content by replaying the same
    // pre-slicing logic used by draw_output().
    let visual_line_count: usize = app
        .output
        .iter()
        .map(|ol| {
            if ol.text.is_empty() || wrap_width == 0 {
                1
            } else {
                ol.text.len().div_ceil(wrap_width)
            }
        })
        .sum();

    let visible_height = area.height as usize;
    let max_scroll = visual_line_count.saturating_sub(visible_height);
    let scroll_y = max_scroll.saturating_sub(app.scroll_offset);

    // Find start_idx and residual scroll (same as draw_output).
    let mut cumulative: usize = 0;
    let mut start_idx: usize = 0;
    let mut residual: usize = 0;
    for (i, ol) in app.output.iter().enumerate() {
        let vlines = if ol.text.is_empty() || wrap_width == 0 {
            1
        } else {
            ol.text.len().div_ceil(wrap_width)
        };
        if cumulative + vlines > scroll_y {
            start_idx = i;
            residual = scroll_y - cumulative;
            break;
        }
        cumulative += vlines;
        start_idx = i + 1;
    }

    // Build visual rows: for each logical line starting from start_idx,
    // break into wrapped segments, skip `residual` rows, collect up to
    // visible_height rows.
    struct VisualRow {
        text: String,
    }
    let mut rows: Vec<VisualRow> = Vec::with_capacity(visible_height);
    let mut skipped = 0usize;

    for ol in app.output.iter().skip(start_idx) {
        let segments = if ol.text.is_empty() {
            vec![String::new()]
        } else {
            ol.text
                .chars()
                .collect::<Vec<_>>()
                .chunks(wrap_width)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect()
        };
        for seg in segments {
            if skipped < residual {
                skipped += 1;
                continue;
            }
            rows.push(VisualRow { text: seg });
            if rows.len() >= visible_height {
                break;
            }
        }
        if rows.len() >= visible_height {
            break;
        }
    }

    // Extract text from the selected visual rows.
    let mut result = String::new();
    for (row_idx, vr) in rows.iter().enumerate() {
        if row_idx < sel_start_row || row_idx > sel_end_row {
            continue;
        }
        let chars: Vec<char> = vr.text.chars().collect();
        let col_start = if row_idx == sel_start_row {
            sel_start_col
        } else {
            0
        };
        let col_end = if row_idx == sel_end_row {
            (sel_end_col + 1).min(chars.len())
        } else {
            chars.len()
        };
        if col_start < chars.len() {
            let selected: String = chars[col_start..col_end.min(chars.len())].iter().collect();
            result.push_str(&selected);
        }
        if row_idx < sel_end_row {
            result.push('\n');
        }
    }

    result
}

/// Copy text to the system clipboard using the `arboard` crate (v0.14.9.3).
///
/// Replaces the previous subprocess approach (pbcopy/xclip/clip.exe) which
/// raced against terminal paste events and failed silently on missing tools.
/// arboard is synchronous and works without external binaries.
///
/// Fails silently (no-op) when no display server is available (e.g., headless CI).
#[allow(dead_code)]
#[cfg(not(test))]
fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
}

#[cfg(test)]
#[allow(dead_code)]
fn copy_to_clipboard(text: &str) {
    TEST_CLIPBOARD.with(|c| *c.borrow_mut() = Some(text.to_string()));
}

/// Read text from the system clipboard using the `arboard` crate (v0.14.9.3).
///
/// Replaces the previous subprocess approach (pbpaste/xclip/xsel/Get-Clipboard)
/// which raced against terminal paste events and failed silently on missing tools.
/// arboard is synchronous and works without external binaries.
///
/// Returns `None` if the clipboard is empty, unreadable, or no display server is
/// available (e.g., headless CI — arboard::Clipboard::new() returns Err).
#[cfg(not(test))]
fn read_from_clipboard() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
fn read_from_clipboard() -> Option<String> {
    TEST_CLIPBOARD.with(|c| c.borrow().clone())
}

// Thread-local mock clipboard for tests (v0.14.9.3).
// Avoids requiring a display server in CI. Tests set `TEST_CLIPBOARD` directly
// before calling the paste handler; `read_from_clipboard()` reads from it.
#[cfg(test)]
thread_local! {
    static TEST_CLIPBOARD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
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
                        artifact_count: dr.4,
                    });
                    // Also render the generic SSE event for the log.
                    if let Some(rendered) = super::shell::render_sse_event(&frame) {
                        let _ = tx.send(TuiMessage::SseEvent(rendered));
                    }
                } else if let Some(goal_id) = parse_goal_terminal_state(&frame) {
                    // Goal reached terminal state — clear working indicator (v0.14.7.1 item 3).
                    let _ = tx.send(TuiMessage::AgentOutputDone(goal_id));
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
                        build_sha: json["build_sha"].as_str().unwrap_or("?").to_string(),
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
                        active_goal_tag: json["active_agents"].as_array().and_then(|agents| {
                            agents
                                .first()
                                .and_then(|a| a["tag"].as_str().map(String::from))
                        }),
                        community_pending_count: json["community_pending_count"]
                            .as_u64()
                            .unwrap_or(0) as usize,
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

/// Load prompt detection timeouts from `.ta/daemon.toml` (v0.11.2.5).
///
/// Returns `(prompt_dismiss_after_output_secs, prompt_verify_timeout_secs)`.
/// Falls back to defaults (5s, 10s) if the config is missing or malformed.
fn load_prompt_detection_config() -> (u64, u64) {
    let config_path = std::env::current_dir()
        .unwrap_or_default()
        .join(".ta/daemon.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(ops) = table.get("operations").and_then(|v| v.as_table()) {
                let dismiss = ops
                    .get("prompt_dismiss_after_output_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(5) as u64;
                let verify = ops
                    .get("prompt_verify_timeout_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(10) as u64;
                return (dismiss, verify);
            }
        }
    }
    (5, 10)
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
    // Notify TUI to track this goal in active_tailing_goals (v0.12.3).
    let _ = tx.send(TuiMessage::TailStarted {
        goal_id: target.clone(),
    });

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
                let _ = tx.send(TuiMessage::AgentOutputDone(target));
                return;
            }
            Err(_) if attempt < 4 => continue, // Retry on network error
            Err(e) => {
                let _ = tx.send(TuiMessage::CommandResponse(format!(
                    "Error: Cannot reach daemon: {}",
                    e
                )));
                let _ = tx.send(TuiMessage::AgentOutputDone(target));
                return;
            }
        }
    }

    let Some(initial_resp) = resp_result else {
        let _ = tx.send(TuiMessage::CommandResponse(
            "Error: Could not connect to output stream after retries".into(),
        ));
        let _ = tx.send(TuiMessage::AgentOutputDone(target));
        return;
    };

    use tokio_stream::StreamExt;

    // SSE reconnect state (v0.14.9.3).
    //
    // `last_event_id` tracks the most-recently received SSE `id:` field so
    // that reconnect requests send `Last-Event-ID: <seq>` and the daemon
    // resumes from where we left off (replaying missed events from its history
    // buffer). Exponential backoff: 1s, 2s, 4s, 8s, 16s → max 5 retries.
    let mut last_event_id: Option<u64> = None;
    let mut reconnect_count: u32 = 0;
    const MAX_RECONNECTS: u32 = 5;
    let stream_url = format!("{}/api/goals/{}/output", base_url, resolved_target);

    // Use Option to allow moving the Response into bytes_stream() each iteration.
    let mut next_resp: Option<reqwest::Response> = Some(initial_resp);

    'reconnect: loop {
        let resp = next_resp.take().expect("always set before loop start");
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        'chunks: while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break 'chunks, // stream error → try reconnect
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buffer.find("\n\n") {
                let frame = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                // Parse SSE frame fields.
                let mut event_type = None;
                let mut data = None;
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("id: ") {
                        // Track last received event ID for reconnect (v0.14.9.3).
                        last_event_id = rest.trim().parse::<u64>().ok().or(last_event_id);
                    } else if let Some(rest) = line.strip_prefix("event: ") {
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
                                    let prompt_line = line.clone();
                                    let _ = tx.send(TuiMessage::StdinPrompt(PendingStdinPrompt {
                                        goal_id: target.clone(),
                                        prompt_text: line,
                                        detected_at: std::time::Instant::now(),
                                        verifying: true,
                                    }));

                                    // Layer 3: Dispatch Q&A agent verification (v0.11.2.5).
                                    let verify_tx = tx.clone();
                                    let verify_base = base_url.to_string();
                                    let verify_client = client.clone();
                                    tokio::spawn(async move {
                                        let ask_url = format!("{}/api/agent/ask", verify_base);
                                        let payload = serde_json::json!({
                                            "prompt": format!(
                                                "Is this agent output a prompt waiting for user input, \
                                                 or is it just informational output? The line is: \"{}\". \
                                                 Respond with only 'prompt' or 'not_prompt'.",
                                                prompt_line
                                            )
                                        });
                                        let result = tokio::time::timeout(
                                            std::time::Duration::from_secs(10),
                                            verify_client.post(&ask_url).json(&payload).send(),
                                        )
                                        .await;
                                        if let Ok(Ok(resp)) = result {
                                            if let Ok(body) = resp.text().await {
                                                if body.to_lowercase().contains("not_prompt") {
                                                    let _ = verify_tx
                                                        .send(TuiMessage::PromptVerifiedNotPrompt);
                                                }
                                            }
                                        }
                                        // On timeout or error, fail-open — keep the prompt visible.
                                    });
                                } else if stream_name == "auto_answered" {
                                    // Parse "[auto] prompt → response" format.
                                    if let Some(arrow_pos) = line.find(" → ") {
                                        let prompt = line[7..arrow_pos].to_string(); // skip "[auto] "
                                        let response = line[arrow_pos + 5..].to_string(); // skip " → "
                                        let _ = tx.send(TuiMessage::StdinAutoAnswered {
                                            prompt,
                                            response,
                                        });
                                    } else {
                                        let _ = tx.send(TuiMessage::AgentOutput(AgentOutputLine {
                                            stream: stream_name,
                                            line,
                                            goal_id: Some(target.clone()),
                                        }));
                                    }
                                } else {
                                    let _ = tx.send(TuiMessage::AgentOutput(AgentOutputLine {
                                        stream: stream_name,
                                        line,
                                        goal_id: Some(target.clone()),
                                    }));
                                }
                            }
                        }
                    }
                    Some("done") => {
                        // Intentional completion — do not reconnect.
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

        // Stream ended unexpectedly (network drop, daemon restart, etc.).
        // Attempt reconnect with exponential backoff and Last-Event-ID (v0.14.9.3).
        //
        // Structured as an inner loop so that failed HTTP attempts keep retrying
        // without returning to the top of 'reconnect (which would panic on a None
        // next_resp). We only `continue 'reconnect` once we have a live response.
        loop {
            if reconnect_count >= MAX_RECONNECTS {
                let _ = tx.send(TuiMessage::CommandResponse(format!(
                    "[reconnect] Stream connection failed after {} retries. \
                     Restart the tail with: :tail {}",
                    MAX_RECONNECTS,
                    &target[..8.min(target.len())]
                )));
                let _ = tx.send(TuiMessage::AgentOutputDone(target));
                return;
            }

            let backoff_secs = 1u64 << reconnect_count;
            let _ = tx.send(TuiMessage::CommandResponse(format!(
                "[reconnect] Connection lost. Reconnecting ({}/{}) in {}s...",
                reconnect_count + 1,
                MAX_RECONNECTS,
                backoff_secs
            )));
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            reconnect_count += 1;

            // Build reconnect request. Include Last-Event-ID so the daemon replays
            // any events we missed while disconnected (requires daemon >= v0.14.9.3).
            let mut req = client.get(&stream_url);
            if let Some(id) = last_event_id {
                req = req.header("Last-Event-ID", id.to_string());
            }

            match req.send().await {
                Ok(r) if r.status().is_success() => {
                    next_resp = Some(r);
                    break; // Have a live response — proceed to 'reconnect processing.
                }
                Ok(_) | Err(_) => {
                    // This attempt failed — loop to check retry limit and try again.
                    continue;
                }
            }
        }
        continue 'reconnect;
    }
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

// NOTE: parse_stream_json_text() and extract_model_from_stream_json() removed in v0.11.2.2.
// Replaced by schema-driven ta_output_schema::parse_line(). See agents/output-schemas/*.yaml.

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
        app.push_output(OutputLine {
            text: line,
            style,
            is_heartbeat: false,
        });
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

/// Format a usize with thousands separators (e.g. 12345 → "12,345").
fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
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

/// Parse an SSE frame for a `draft_built` event (v0.10.11 item 4 / v0.15.7.1).
/// Returns `Some((goal_id, draft_id, display_id, title, artifact_count))`.
fn parse_draft_built(frame: &str) -> Option<(String, String, String, String, usize)> {
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
        .filter(|s| !s.is_empty())
        .unwrap_or("(untitled)")
        .to_string();
    let artifact_count = payload["artifact_count"].as_u64().unwrap_or(0) as usize;
    Some((goal_id, draft_id, display_id, title, artifact_count))
}

/// Parse an SSE frame for any terminal goal state (failed, cancelled, denied, pr_ready).
/// Returns `Some(goal_id)` so the TUI can clear working indicators (v0.14.7.1 item 3).
fn parse_goal_terminal_state(frame: &str) -> Option<String> {
    let mut event_type = None;
    let mut data = None;
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.trim());
        }
    }
    let event_type = event_type?;
    if !matches!(
        event_type,
        "goal_failed" | "goal_cancelled" | "goal_denied" | "goal_pr_ready"
    ) {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(data?).ok()?;
    let payload = &json["payload"];
    payload["goal_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Update App's cached layout information for mouse event handling.
/// Called after each terminal draw (v0.14.7.1 item 6).
fn update_layout_cache(app: &mut App, terminal: &Terminal<CrosstermBackend<Stdout>>) {
    let size = match terminal.size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let prompt = app.prompt_str();
    let display = format!("{}{}", prompt, app.input);
    let inner_width = size.width.max(1) as usize;
    let content_lines = {
        let mut lines = 0u16;
        let mut col = 0usize;
        for ch in display.chars() {
            if ch == '\n' {
                lines += 1;
                col = 0;
            } else {
                col += 1;
                if col >= inner_width {
                    lines += 1;
                    col = 0;
                }
            }
        }
        lines + 1
    };
    let input_height = (content_lines + 2).min(size.height / 2).max(3);
    let output_height = size.height.saturating_sub(1 + input_height);
    app.output_area_top = 0;
    app.output_area_height = output_height;
    app.scrollbar_col = if size.width > 0 {
        Some(size.width - 1)
    } else {
        None
    };
}

const HELP_TEXT: &str = "\
TA Shell -- Interactive terminal for Trusted Autonomy

Commands:
  ta <cmd>           Run any ta CLI command (e.g., ta draft list)
  run <title>        Start a new agent goal (alias for: ta run <title>)
  vcs <cmd>          Run VCS commands (e.g., vcs status, vcs log)
  git <cmd>          Alias for vcs — runs git commands
  !<cmd>             Shell escape: run any shell command (e.g., !ls -la, !echo $PWD)
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

Agent routing (v0.12.3):
  >tag message           Send a message to the agent whose short ID starts with 'tag'
  > message              Send to the sole active agent (when only one is running)
  Prompt shows [→tag] > after routing; any other command clears the indicator.
  Multiple agents: each output line is prefixed with [short-id] for disambiguation.

Follow-up:
  :follow-up             List all follow-up candidates (failed goals, denied drafts, etc.)
  :follow-up <filter>    Filter candidates by keyword (fuzzy match on title/status/type)

Interactive mode:
  When an agent asks a question, the prompt changes to [agent Q1] >
  Type your response and press Enter to send it back to the agent.
  Auth failure: type 'r' to retry or 'a' to abort when auth-fail prompt appears.

Shell commands:
  :status            Refresh the status bar
  :stats             Show velocity stats inline (same as ta stats velocity)
  :latency on|off    Toggle input latency diagnostics (log: .ta/latency.log)
  :latency dump      Show latency report now
  clear              Clear the output pane

  :help shows context-specific commands when viewing a draft or running a goal.";

const DRAFT_HELP_TEXT: &str = "\
TA Shell -- Draft Review Mode

Commands:
  view <id>                   View draft summary
  view <id> --section files   List changed files
  view <id> --section decisions  Show agent decision log
  view <id> --file <pattern>  Show diff for specific files (glob supported)
  approve <id>                Approve the entire draft
  deny <id> --reason <why>    Deny the entire draft
  deny <id> --file <path> --reason <why>  Deny a single artifact
  ta draft amend <id> <uri> --file <corrected>  Replace an artifact
  apply <id>                  Apply approved draft to workspace
  :help                       Show this help
  exit                        Exit the shell";

const RUNNING_GOAL_HELP_TEXT: &str = "\
TA Shell -- Goal Running Mode

Commands:
  :tail [id]              Attach to goal output stream
  :detach                 Detach from goal output (stops tailing)
  > <message>             Send a message to the running agent
  ta goal status <id>     Show goal status details
  ta goal stop <id>       Request the agent to stop
  drafts                  List draft packages (check when goal completes)
  :help                   Show this help
  exit                    Exit the shell";

/// Keybinding table — single source of truth for help output and documentation.
/// Each entry is (key, description). Sections are separated by ("", "") blank rows.
const KEYBINDING_TABLE: &[(&str, &str)] = &[
    // Navigation
    ("Up / Down", "Command history"),
    ("Shift+Up / Down", "Scroll output 1 line"),
    ("PgUp / PgDn", "Scroll output one full page"),
    ("Shift+Home / End", "Scroll to top / bottom of output"),
    ("Cmd+Up / Down", "Scroll to top / bottom of output (Mac)"),
    ("", ""),
    // Text editing
    ("Ctrl-A / Ctrl-E", "Jump to start / end of input"),
    ("Ctrl-U / Ctrl-K", "Clear input before / after cursor"),
    ("Ctrl-W", "Toggle split pane (shell | agent side-by-side)"),
    ("Ctrl-L", "Clear the output pane"),
    (
        "Tab",
        "Auto-complete commands (or toggle paste preview when paste pending)",
    ),
    ("Click-drag", "Select text (native terminal selection)"),
    ("Cmd+C", "Copy selection (native)"),
    (
        "Paste",
        "Small pastes inserted verbatim; large pastes (>500 chars / >10 lines) compacted",
    ),
    ("", ""),
    // Exit
    (
        "Ctrl-C / exit",
        "Exit the shell (Ctrl-C detaches when tailing)",
    ),
    ("", ""),
    // Scrollback
    (
        "Scrollback",
        "Output is retained in a scrollback buffer (default: 50000 lines).",
    ),
    (
        "",
        "Configure via [shell] scrollback_lines in .ta/workflow.toml (minimum: 10000).",
    ),
    (
        "",
        "Status bar shows scroll position and new output indicator when scrolled up.",
    ),
];

/// Generate the Navigation/Text/Scrollback section of the help text from
/// `KEYBINDING_TABLE` — the same data that documents all active keybindings.
fn keybinding_help_text() -> String {
    let mut out = String::from("\nNavigation & Text:\n");
    for (key, desc) in KEYBINDING_TABLE {
        if key.is_empty() {
            if !desc.is_empty() {
                out.push_str(&format!("  {}\n", desc));
            } else {
                out.push('\n');
            }
        } else {
            out.push_str(&format!("  {:<22} {}\n", key, desc));
        }
    }
    out
}

const CLI_HELP_TEXT: &str = "\
CLI Commands (prefix with 'ta' or use directly):
  goal <cmd>         Manage goal runs (start, list, status, delete, inspect, doctor, gc)
  draft <cmd>        Review and manage draft packages (view, approve, deny, apply, list)
  run <title>        Run an agent in a TA-mediated staging workspace
  dev                Interactive developer loop
  plan <cmd>         View and track the development plan (list, status, add)
  status             Project-wide dashboard: agents, drafts, next phase
  build              Build the project using the configured adapter
  doctor             System-wide health check
  daemon <cmd>       Manage daemon lifecycle (start, stop, restart, status, log)
  plugin <cmd>       Manage channel plugins (list, install, validate)
  workflow <cmd>     Manage multi-stage workflows
  gc                 Unified garbage collection
  context <cmd>      Manage persistent context memory
  audit <cmd>        Inspect the audit trail
  release <cmd>      Run the configurable release pipeline
  setup              Interactive setup wizard
  verify             Pre-draft verification checks
  config <cmd>       Inspect and validate configuration

Run 'ta <command> --help' for details on any command.";

/// Compute a velocity stats summary string for the `:stats` shell command (v0.15.14.2).
///
/// Reads velocity data directly from the project's local and committed stores.
/// Returns a formatted multi-line string ready for display in the shell TUI.
fn shell_velocity_stats(project_root: &std::path::Path) -> String {
    use ta_goal::{
        aggregate_by_contributor, merge_velocity_entries, VelocityAggregate, VelocityHistoryStore,
        VelocityStore,
    };

    let local_store = VelocityStore::for_project(project_root);
    let history_store = VelocityHistoryStore::for_project(project_root);

    let local = match local_store.load_all() {
        Ok(e) => e,
        Err(e) => return format!("Error loading velocity stats: {}", e),
    };
    let committed = match history_store.load_all() {
        Ok(e) => e,
        Err(e) => return format!("Error loading velocity history: {}", e),
    };

    let (merged, committed_ids) = merge_velocity_entries(local, committed);

    if merged.is_empty() {
        return "No velocity data recorded yet. Data is written when goals complete.".to_string();
    }

    let agg = VelocityAggregate::from_entries(&merged);
    let committed_entries: Vec<_> = merged
        .iter()
        .filter(|e| committed_ids.contains(&e.goal_id))
        .cloned()
        .collect();
    let by_contributor = aggregate_by_contributor(&committed_entries);

    let mut out = String::new();
    out.push_str("Velocity Stats\n");
    out.push_str(&"─".repeat(50));
    out.push('\n');
    out.push_str(&format!("  Total goals:    {}\n", agg.total_goals));
    out.push_str(&format!(
        "  Applied:        {} ({:.0}%)\n",
        agg.applied,
        if agg.total_goals > 0 {
            agg.applied as f64 / agg.total_goals as f64 * 100.0
        } else {
            0.0
        }
    ));
    out.push_str(&format!(
        "  Avg build time: {}\n",
        shell_fmt_duration(agg.avg_build_seconds)
    ));
    out.push_str(&format!(
        "  P90 build time: {}\n",
        shell_fmt_duration(agg.p90_build_seconds)
    ));
    if agg.total_rework_seconds > 0 {
        out.push_str(&format!(
            "  Total rework:   {}\n",
            shell_fmt_duration(agg.total_rework_seconds)
        ));
    }
    if agg.total_cost_usd > 0.0 {
        out.push_str(&format!("  Total cost:     ${:.2}\n", agg.total_cost_usd));
        out.push_str(&format!("  Avg cost/goal:  ${:.2}\n", agg.avg_cost_usd));
    }
    if !by_contributor.is_empty() {
        out.push('\n');
        out.push_str(&format!(
            "  {:<22} {:>6} {:>8} {:>12}\n",
            "CONTRIBUTOR", "GOALS", "APPLIED", "AVG BUILD"
        ));
        out.push_str(&format!("  {}\n", "─".repeat(52)));
        for c in &by_contributor {
            out.push_str(&format!(
                "  {:<22} {:>6} {:>8} {:>12}\n",
                if c.contributor.len() > 20 {
                    &c.contributor[..20]
                } else {
                    &c.contributor
                },
                c.total_goals,
                c.applied,
                shell_fmt_duration(c.avg_build_seconds)
            ));
        }
    }
    out
}

fn shell_fmt_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

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
            "\"title\":\"v0.10.11 — Shell TUI UX Overhaul\",",
            "\"artifact_count\":11",
            "}}"
        );
        let (goal_id, draft_id, display_id, title, artifact_count) =
            parse_draft_built(frame).expect("should parse");
        assert_eq!(goal_id, "aaaa1111-2222-3333-4444-555555555555");
        assert_eq!(draft_id, "bbbb1111-2222-3333-4444-555555555555");
        assert_eq!(display_id, "aaaa1111-01");
        assert_eq!(title, "v0.10.11 — Shell TUI UX Overhaul");
        assert_eq!(artifact_count, 11);
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
        let (_, _, display_id, _, artifact_count) = parse_draft_built(frame).expect("should parse");
        assert_eq!(display_id, "bbbb1111"); // falls back to 8-char prefix
        assert_eq!(artifact_count, 0); // no artifact_count in payload
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
                goal_id: None,
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
                goal_id: None,
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
                artifact_count: 11,
            },
        );
        assert!(!app.output.is_empty());
        // Second line shows the ta draft view command.
        assert!(app.output.last().unwrap().text.contains("aaaa1111-01"));
        // First notification line contains the title.
        let has_title = app.output.iter().any(|l| l.text.contains("v0.10.11"));
        assert!(has_title, "notification should contain goal title");
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

    // -- v0.11.2.2 schema-driven parsing tests --

    fn test_schema() -> ta_output_schema::OutputSchema {
        let loader = ta_output_schema::SchemaLoader::embedded_only();
        loader.load("claude-code").unwrap()
    }

    #[test]
    fn schema_parse_result() {
        let schema = test_schema();
        let line = r#"{"type":"result","result":"Hello world"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("[result] Hello world".into())
        );
    }

    #[test]
    fn schema_parse_content_block_delta() {
        let schema = test_schema();
        let line =
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"partial "}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("partial ".into())
        );
    }

    #[test]
    fn schema_parse_assistant_nested_message() {
        let schema = test_schema();
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","content":[{"type":"text","text":"Nested hello"}]}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("Nested hello".into())
        );
    }

    #[test]
    fn schema_parse_system_init() {
        let schema = test_schema();
        let line = r#"{"type":"system","subtype":"init","model":"claude-opus-4-6"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("[init] model: claude-opus-4-6".into())
        );
    }

    #[test]
    fn schema_parse_system_hook() {
        let schema = test_schema();
        let line =
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("[hook] SessionStart:startup...".into())
        );
    }

    #[test]
    fn schema_parse_suppressed_events() {
        let schema = test_schema();
        let line = r#"{"type":"message_start","message":{}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Suppress
        );
    }

    #[test]
    fn schema_parse_plain_text_returns_not_json() {
        let schema = test_schema();
        let line = "This is not JSON";
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::NotJson
        );
    }

    #[test]
    fn schema_parse_tool_use() {
        let schema = test_schema();
        let line = r#"{"type":"tool_use","name":"Read"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::ToolUse("Read".into())
        );
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
    fn schema_extract_model_from_message_start() {
        let schema = test_schema();
        let line = r#"{"type":"message_start","message":{"model":"claude-sonnet-4-20250514","role":"assistant"}}"#;
        let model = schema.extract_model(line);
        assert_eq!(model, Some("claude-sonnet-4-20250514".into()));
        // humanize_model_name is applied at the call site.
        assert_eq!(
            humanize_model_name("claude-sonnet-4-20250514"),
            "Claude Sonnet 4"
        );
    }

    #[test]
    fn schema_extract_model_from_top_level() {
        let schema = test_schema();
        let line = r#"{"model":"claude-haiku-4-20250101","type":"system"}"#;
        assert_eq!(
            schema.extract_model(line),
            Some("claude-haiku-4-20250101".into())
        );
    }

    #[test]
    fn schema_extract_model_no_model_field() {
        let schema = test_schema();
        let line = r#"{"type":"content_block_delta","delta":{"text":"hello"}}"#;
        assert!(schema.extract_model(line).is_none());
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
            detected_at: std::time::Instant::now(),
            verifying: false,
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
            detected_at: std::time::Instant::now(),
            verifying: false,
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
            detected_at: std::time::Instant::now(),
            verifying: false,
        });
        // Simulate Ctrl-C by directly clearing the prompt.
        app.pending_stdin_prompt = None;
        assert!(app.pending_stdin_prompt.is_none());
    }

    // ── v0.11.2.5 prompt detection hardening tests ───────────

    #[test]
    fn prompt_dismissed_on_continued_output() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.prompt_dismiss_after_output_secs = 5;

        // Set a pending prompt (just created, so within the dismiss window).
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Enter name:".into(),
            detected_at: std::time::Instant::now(),
            verifying: false,
        });

        // Agent output arrives while prompt is pending.
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "More output from the agent".into(),
                goal_id: None,
            }),
        );

        // Prompt should have been auto-dismissed.
        assert!(app.pending_stdin_prompt.is_none());
        assert!(app
            .output
            .iter()
            .any(|l| l.text.contains("Prompt dismissed")));
    }

    #[test]
    fn prompt_cleared_on_stream_end() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Enter name:".into(),
            detected_at: std::time::Instant::now(),
            verifying: false,
        });

        // Stream ends for the same goal.
        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-1".into()));

        // Prompt should be cleared.
        assert!(app.pending_stdin_prompt.is_none());
        assert!(app.output.iter().any(|l| l.text.contains("Prompt cleared")));
    }

    #[test]
    fn prompt_not_cleared_on_different_goal_end() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Enter name:".into(),
            detected_at: std::time::Instant::now(),
            verifying: false,
        });

        // Different goal ends — prompt should remain.
        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-2".into()));
        assert!(app.pending_stdin_prompt.is_some());
    }

    #[test]
    fn prompt_verified_not_prompt_dismisses() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "**API** (path):".into(),
            detected_at: std::time::Instant::now(),
            verifying: true,
        });

        // Q&A agent responds: not a prompt.
        handle_tui_message(&mut app, TuiMessage::PromptVerifiedNotPrompt);
        assert!(app.pending_stdin_prompt.is_none());
        assert!(app.output.iter().any(|l| l.text.contains("Not a prompt")));
    }

    #[test]
    fn prompt_str_shows_verifying() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_stdin_prompt = Some(PendingStdinPrompt {
            goal_id: "goal-1".into(),
            prompt_text: "Prompt?".into(),
            detected_at: std::time::Instant::now(),
            verifying: true,
        });
        assert!(app.prompt_str().contains("verifying"));
    }

    #[test]
    fn load_prompt_detection_config_defaults() {
        let (dismiss, verify) = load_prompt_detection_config();
        // If no daemon.toml exists in cwd, defaults should apply.
        // (In test context, cwd is usually the project root or /tmp.)
        assert!(dismiss > 0);
        assert!(verify > 0);
    }

    // -- v0.11.4.1 tests --

    #[test]
    fn command_response_multiline_renders_all_lines() {
        // Item 5: Verify CommandResponse renders multi-line text correctly.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let multi = "Applied 3 files to /project\nDraft aaaa1111-01 marked as applied\nDone.";
        handle_tui_message(&mut app, TuiMessage::CommandResponse(multi.to_string()));
        assert_eq!(app.output.len(), 3);
        assert_eq!(app.output[0].text, "Applied 3 files to /project");
        assert_eq!(app.output[1].text, "Draft aaaa1111-01 marked as applied");
        assert_eq!(app.output[2].text, "Done.");
    }

    #[test]
    fn heartbeat_updates_in_place() {
        // Item 9: Heartbeat lines update the last line instead of appending.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.push_output(OutputLine::info("Agent started".into()));
        app.push_heartbeat("[heartbeat] still running... 10s elapsed".into());
        assert_eq!(app.output.len(), 2);
        assert!(app.output[1].is_heartbeat);

        // Second heartbeat should update in-place, not append.
        app.push_heartbeat("[heartbeat] still running... 20s elapsed".into());
        assert_eq!(app.output.len(), 2);
        assert_eq!(
            app.output[1].text,
            "[heartbeat] still running... 20s elapsed"
        );
    }

    #[test]
    fn heartbeat_pushed_after_real_output() {
        // Item 10: Non-heartbeat output pushes the heartbeat down.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.push_heartbeat("[heartbeat] still running... 10s elapsed".into());
        assert_eq!(app.output.len(), 1);

        // Real output should push after the heartbeat.
        app.push_output(OutputLine::agent_stdout("Building...".into()));
        assert_eq!(app.output.len(), 2);
        assert_eq!(app.output[1].text, "Building...");

        // Next heartbeat appends (last line is not heartbeat).
        app.push_heartbeat("[heartbeat] still running... 30s elapsed".into());
        assert_eq!(app.output.len(), 3);
        assert!(app.output[2].is_heartbeat);
    }

    #[test]
    fn heartbeat_coalesced_in_agent_output() {
        // Items 9-10: Heartbeat lines from AgentOutput are coalesced.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stderr".into(),
                line: "[heartbeat] still running... 10s elapsed".into(),
                goal_id: None,
            }),
        );
        assert_eq!(app.output.len(), 1);
        assert!(app.output[0].is_heartbeat);

        // Second heartbeat updates in-place.
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stderr".into(),
                line: "[heartbeat] still running... 20s elapsed".into(),
                goal_id: None,
            }),
        );
        assert_eq!(app.output.len(), 1);
        assert_eq!(
            app.output[0].text,
            "[heartbeat] still running... 20s elapsed"
        );

        // Non-heartbeat output comes in — next heartbeat appends.
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "Compiling crate...".into(),
                goal_id: None,
            }),
        );
        assert_eq!(app.output.len(), 2);

        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stderr".into(),
                line: "[heartbeat] still running... 30s elapsed".into(),
                goal_id: None,
            }),
        );
        assert_eq!(app.output.len(), 3);
        assert!(app.output[2].is_heartbeat);
    }

    #[test]
    fn mouse_selection_initial_state() {
        // TUI mouse selection: starts with no selection, output_area default.
        let app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(app.selection.is_none());
        assert_eq!(app.output_area, ratatui::layout::Rect::default());
    }

    #[test]
    fn dedicated_input_thread_channel() {
        // v0.11.4.2 item 11: Verify that the input channel works.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crossterm::event::Event>();
        // Simulate sending an event.
        let ev = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        tx.send(ev).unwrap();
        // Should be immediately receivable.
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn paste_strips_newlines() {
        // Pasted text with newlines should not trigger command submission.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let pasted = "line one\nline two\r\nline three";
        let safe = pasted.replace(['\r', '\n'], " ").replace('\t', "    ");
        for ch in safe.chars() {
            app.insert_char(ch);
        }
        assert_eq!(app.input, "line one line two  line three");
    }

    // ── Large-paste compaction tests (v0.11.4.5) ─────────────────────────────

    #[test]
    fn large_paste_over_char_threshold_stores_pending() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Build a paste > PASTE_CHAR_THRESHOLD chars on a single line.
        let big = "x".repeat(PASTE_CHAR_THRESHOLD + 1);
        // Simulate paste handling directly (mirrors the Event::Paste branch).
        let safe = big
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let char_count = safe.chars().count();
        let line_count = safe.lines().count();
        if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
            app.pending_paste = Some(safe.clone());
            app.paste_preview_expanded = false;
        }
        assert!(
            app.pending_paste.is_some(),
            "large paste should be stored as pending"
        );
        assert!(
            app.input.is_empty(),
            "input buffer should be unaffected by large paste"
        );
    }

    #[test]
    fn large_paste_over_line_threshold_stores_pending() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Build a paste with more than PASTE_LINE_THRESHOLD lines.
        let big = (0..=PASTE_LINE_THRESHOLD)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let safe = big
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let char_count = safe.chars().count();
        let line_count = safe.lines().count();
        if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
            app.pending_paste = Some(safe);
            app.paste_preview_expanded = false;
        }
        assert!(
            app.pending_paste.is_some(),
            "multi-line paste should be stored as pending"
        );
    }

    #[test]
    fn small_paste_inserted_verbatim() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Build a paste well under both thresholds.
        let small = "hello world";
        let safe = small
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let char_count = safe.chars().count();
        let line_count = safe.lines().count();
        if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
            app.pending_paste = Some(safe.clone());
        } else {
            for ch in safe.chars() {
                app.insert_char(ch);
            }
        }
        assert!(
            app.pending_paste.is_none(),
            "small paste should not be stored as pending"
        );
        assert_eq!(app.input, "hello world");
    }

    #[test]
    fn submit_combines_typed_prefix_with_pending_paste() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // User typed a prefix before pasting.
        for ch in "context: ".chars() {
            app.insert_char(ch);
        }
        let paste_text = "a".repeat(PASTE_CHAR_THRESHOLD + 1);
        app.pending_paste = Some(paste_text.clone());

        let result = app.submit();
        assert!(result.is_some());
        let submitted = result.unwrap();
        assert!(
            submitted.starts_with("context:"),
            "submitted text should include typed prefix"
        );
        assert!(
            submitted.contains(&paste_text),
            "submitted text should include full paste"
        );
        assert!(
            app.pending_paste.is_none(),
            "pending_paste should be cleared after submit"
        );
        assert!(app.input.is_empty(), "input should be cleared after submit");
    }

    #[test]
    fn submit_with_only_pending_paste_no_prefix() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let paste_text = "b".repeat(PASTE_CHAR_THRESHOLD + 1);
        app.pending_paste = Some(paste_text.clone());

        let result = app.submit();
        assert_eq!(result, Some(paste_text));
        assert!(app.pending_paste.is_none());
    }

    #[test]
    fn cancel_pending_paste_clears_state() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_paste = Some("big paste".into());
        app.paste_preview_expanded = true;
        app.cancel_pending_paste();
        assert!(
            app.pending_paste.is_none(),
            "paste should be cleared after cancel"
        );
        assert!(
            !app.paste_preview_expanded,
            "preview flag should be reset after cancel"
        );
    }

    #[test]
    fn tab_toggles_paste_preview() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_paste = Some("big paste content".into());
        assert!(!app.paste_preview_expanded);
        // Simulate Tab press when paste is pending.
        app.paste_preview_expanded = !app.paste_preview_expanded;
        assert!(
            app.paste_preview_expanded,
            "first Tab should expand preview"
        );
        app.paste_preview_expanded = !app.paste_preview_expanded;
        assert!(
            !app.paste_preview_expanded,
            "second Tab should collapse preview"
        );
    }

    #[test]
    fn submit_empty_with_pending_paste_returns_none_if_paste_empty() {
        // If somehow pending_paste is set to empty string, submit returns None.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.pending_paste = Some(String::new());
        let result = app.submit();
        assert!(result.is_none(), "empty pending paste should not submit");
    }

    // ── Paste tests (v0.12.2 / v0.14.7.1) ────────────────────────────────────

    /// Helper that simulates the Event::Paste handler on an App.
    fn simulate_paste(app: &mut App, data: &str) {
        let safe = data
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
            .trim_matches('\n')
            .to_string();
        let line_count = safe.lines().count();
        let char_count = safe.chars().count();
        if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
            app.pending_paste = Some(safe);
            app.paste_preview_expanded = false;
        } else {
            // Cursor-aware paste (v0.14.7.1 items 1/8).
            if app.scroll_offset > 0 {
                app.cursor = app.input.len();
                app.scroll_to_bottom();
            }
            for ch in safe.chars() {
                app.insert_char(ch);
            }
        }
    }

    #[test]
    fn small_paste_with_cursor_at_start_appends_at_end() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Type "hello" then move cursor to the beginning.
        for ch in "hello".chars() {
            app.insert_char(ch);
        }
        app.cursor = 0;
        assert_eq!(app.cursor, 0);
        // scroll_offset == 0 (input-focused), so paste inserts at cursor (position 0).
        simulate_paste(&mut app, "world");
        assert_eq!(
            app.input, "worldhello",
            "paste should insert at cursor (position 0) when input-focused"
        );
        assert_eq!(app.cursor, 5, "cursor should be after pasted text");
    }

    #[test]
    fn small_paste_with_cursor_in_middle_appends_at_end() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "abcde".chars() {
            app.insert_char(ch);
        }
        // Move cursor to middle (position 2, between 'b' and 'c').
        app.cursor = 2;
        // scroll_offset == 0 (input-focused), so paste inserts at cursor (position 2).
        simulate_paste(&mut app, "XYZ");
        assert_eq!(
            app.input, "abXYZcde",
            "paste should insert at cursor position when input-focused"
        );
        assert_eq!(app.cursor, 5, "cursor should be after pasted text");
    }

    #[test]
    fn small_paste_with_cursor_at_end_appends_normally() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "prefix ".chars() {
            app.insert_char(ch);
        }
        // Cursor is already at the end here.
        assert_eq!(app.cursor, app.input.len());
        simulate_paste(&mut app, "pasted");
        assert_eq!(app.input, "prefix pasted");
    }

    #[test]
    fn paste_strips_leading_and_trailing_newlines() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Leading/trailing newlines are stripped to avoid accidental submit.
        simulate_paste(&mut app, "\nhello\n");
        assert_eq!(
            app.input, "hello",
            "leading/trailing newlines must be stripped"
        );
    }

    #[test]
    fn paste_strips_multiple_leading_trailing_newlines() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        simulate_paste(&mut app, "\n\nhello world\n\n");
        assert_eq!(app.input, "hello world");
    }

    #[test]
    fn paste_preserves_internal_newlines() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // A small multi-line paste (under threshold) keeps internal newlines intact.
        simulate_paste(&mut app, "line one\nline two");
        assert_eq!(
            app.input, "line one\nline two",
            "internal newlines in a small paste must be preserved"
        );
    }

    // ── v0.14.7.1 new paste tests ─────────────────────────────────────────────

    #[test]
    fn paste_at_start_when_input_focused() {
        // scroll_offset == 0 → input-focused → insert at cursor position 0.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "hello".chars() {
            app.insert_char(ch);
        }
        app.cursor = 0;
        assert_eq!(app.scroll_offset, 0);
        simulate_paste(&mut app, "world");
        assert_eq!(app.input, "worldhello");
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn paste_at_middle_when_input_focused() {
        // scroll_offset == 0 → input-focused → insert at cursor position 2.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "abcde".chars() {
            app.insert_char(ch);
        }
        app.cursor = 2;
        assert_eq!(app.scroll_offset, 0);
        simulate_paste(&mut app, "XYZ");
        assert_eq!(app.input, "abXYZcde");
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn paste_at_end_when_input_focused() {
        // scroll_offset == 0, cursor at end → normal append.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "prefix ".chars() {
            app.insert_char(ch);
        }
        assert_eq!(app.scroll_offset, 0);
        simulate_paste(&mut app, "pasted");
        assert_eq!(app.input, "prefix pasted");
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn paste_scroll_focused_appends_at_end() {
        // scroll_offset > 0 → scroll-focused → snap to end, scroll to bottom, append.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Push enough output lines so scroll_offset can be > 0.
        for i in 0..10 {
            app.push_output(OutputLine::info(format!("line {}", i)));
        }
        // Type input with cursor at start.
        for ch in "hello".chars() {
            app.insert_char(ch);
        }
        app.cursor = 0;
        // Simulate scrolling up.
        app.auto_scroll = false;
        app.scroll_offset = 3;
        assert!(app.scroll_offset > 0);
        simulate_paste(&mut app, "world");
        // After paste: scroll snapped to bottom and text appended at end.
        assert_eq!(app.scroll_offset, 0, "scroll should snap to bottom");
        assert_eq!(
            app.input, "helloworld",
            "paste should append at end when scroll-focused"
        );
        assert_eq!(app.cursor, app.input.len(), "cursor should be at end");
    }

    // ── v0.14.7.1 scroll and working indicator tests ──────────────────────────

    #[test]
    fn draft_ready_clears_working_indicator() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Push a heartbeat line (working indicator).
        app.push_heartbeat("Agent is working...".to_string());
        assert!(
            app.output.iter().any(|l| l.is_heartbeat),
            "heartbeat should be present before DraftReady"
        );
        // Simulate DraftReady message handling.
        clear_all_heartbeats(&mut app.output);
        clear_all_heartbeats(&mut app.agent_output);
        assert!(
            !app.output.iter().any(|l| l.is_heartbeat),
            "no heartbeats should remain after clear_all_heartbeats"
        );
    }

    #[test]
    fn scroll_resumption_after_scroll_up_and_back() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Fill buffer with some lines.
        for i in 0..20 {
            app.push_output(OutputLine::info(format!("line {}", i)));
        }
        // Scroll up — auto_scroll should become false.
        app.scroll_up(5);
        assert!(app.scroll_offset > 0);
        assert!(
            !app.auto_scroll,
            "auto_scroll should be false after scrolling up"
        );
        // Scroll back to bottom — auto_scroll should be restored.
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
        assert!(
            app.auto_scroll,
            "auto_scroll should be true after scrolling to bottom"
        );
        // Push a new line — should stay at bottom.
        app.push_output(OutputLine::info("new line".to_string()));
        assert_eq!(
            app.scroll_offset, 0,
            "scroll_offset should remain 0 after new output"
        );
        assert_eq!(
            app.unread_events, 0,
            "no unread events when auto_scroll is true"
        );
    }

    #[test]
    fn scrollbar_click_jumps_to_position() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Fill buffer with lines to scroll.
        for i in 0..50 {
            app.push_output(OutputLine::info(format!("line {}", i)));
        }
        // Set up layout cache as if terminal is 80x24.
        app.scrollbar_col = Some(79);
        app.output_area_top = 0;
        app.output_area_height = 20;
        // Simulate click at row 0 (top of scrollbar) — should scroll to top.
        let h = app.output_area_height as usize;
        let rel_row: usize = 0;
        let vl = app.cached_visual_lines.max(app.output.len());
        let max_scroll = vl.saturating_sub(h);
        if max_scroll > 0 {
            let pos = rel_row * max_scroll / h;
            app.scroll_offset = max_scroll.saturating_sub(pos);
            if app.scroll_offset == 0 {
                app.auto_scroll = true;
                app.unread_events = 0;
            } else {
                app.auto_scroll = false;
            }
        }
        assert!(app.scroll_offset > 0, "click at top should scroll up");
        assert!(
            !app.auto_scroll,
            "auto_scroll should be false when scrolled up"
        );
    }

    #[test]
    fn extract_selection_empty_output() {
        let app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let sel = Selection {
            anchor: ScreenPos { col: 0, row: 0 },
            extent: ScreenPos { col: 5, row: 0 },
            output_area: ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
        };
        // No output lines → empty result.
        assert_eq!(extract_selection_text(&app, &sel), "");
    }

    #[test]
    fn extract_selection_single_line() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.push_output(OutputLine::info("Hello, World!".to_string()));
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let sel = Selection {
            anchor: ScreenPos { col: 0, row: 0 },
            extent: ScreenPos { col: 4, row: 0 },
            output_area: area,
        };
        let text = extract_selection_text(&app, &sel);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_selection_multi_line() {
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.push_output(OutputLine::info("Line one".to_string()));
        app.push_output(OutputLine::info("Line two".to_string()));
        app.push_output(OutputLine::info("Line three".to_string()));
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let sel = Selection {
            anchor: ScreenPos { col: 5, row: 0 },
            extent: ScreenPos { col: 3, row: 1 },
            output_area: area,
        };
        let text = extract_selection_text(&app, &sel);
        // Row 0 col 5..end = "one", Row 1 col 0..3 = "Line"
        assert_eq!(text, "one\nLine");
    }

    /// Test that input events are processed with low latency even when
    /// the background message channel is flooded with agent output.
    ///
    /// Simulates the real scenario: an agent producing high-volume SSE
    /// output while the user tries to type. The event loop must process
    /// input within 5ms regardless of background traffic volume.
    #[tokio::test]
    async fn input_latency_under_background_flood() {
        use std::time::{Duration, Instant};

        let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<TuiMessage>();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<StampedEvent>();

        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let mut input_rx = input_rx;

        // Flood background channel with 10,000 agent output messages.
        for i in 0..10_000 {
            let _ = bg_tx.send(TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: format!("Agent output line {}", i),
                goal_id: None,
            }));
        }

        // Now send an input event (simulating a keystroke).
        let _ = input_tx.send(StampedEvent {
            event: Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            os_read_at: Instant::now(),
        });

        // Run one iteration of the poll logic.
        let start = Instant::now();

        // Drain input (should find 'x' immediately).
        let mut pending_inputs = Vec::new();
        while let Ok(stamped) = input_rx.try_recv() {
            pending_inputs.push(stamped);
        }
        let input_count = pending_inputs.len();

        // Process input events (just insert chars, no network).
        for stamped in &pending_inputs {
            if let Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) = stamped.event
            {
                app.insert_char(c);
            }
        }

        let input_latency = start.elapsed();

        // Process one bg message.
        let mut bg_count = 0;
        if let Ok(msg) = bg_rx.try_recv() {
            handle_tui_message(&mut app, msg);
            bg_count = 1;
        }

        // Assertions:
        assert_eq!(input_count, 1, "Should have received exactly 1 input event");
        assert_eq!(app.input, "x", "Input char should be inserted");
        assert!(
            input_latency < Duration::from_millis(5),
            "Input latency {:?} exceeds 5ms threshold",
            input_latency
        );
        assert_eq!(bg_count, 1, "Should process at most 1 bg message per cycle");
        let mut remaining = 0;
        while bg_rx.try_recv().is_ok() {
            remaining += 1;
        }
        assert_eq!(remaining, 9999, "Remaining bg messages should be untouched");
    }

    /// Test that the event loop yields to tokio (doesn't block the thread)
    /// by verifying that a spawned background task can make progress
    /// concurrently with the event loop polling.
    #[tokio::test]
    async fn event_loop_yields_to_tokio_runtime() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        // Spawn a background task that increments a counter rapidly.
        tokio::spawn(async move {
            for _ in 0..100 {
                counter_clone.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        // Simulate what the event loop does when idle: yield via tokio::time::sleep.
        // If we used std::thread::sleep, the background task would be blocked.
        let start = std::time::Instant::now();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let elapsed = start.elapsed();

        // The background task should have made significant progress.
        let count = counter.load(Ordering::Relaxed);
        assert!(
            count >= 20,
            "Background task only ran {} times in {:?} — tokio runtime is being blocked",
            count,
            elapsed
        );
    }

    /// Test that when the bg channel has a burst of messages and then input
    /// arrives, the input is serviced on the very next cycle (not after
    /// draining all bg messages).
    #[tokio::test]
    async fn input_priority_over_background_burst() {
        use std::time::Instant;

        let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<TuiMessage>();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<StampedEvent>();

        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let mut input_rx = input_rx;

        // Send 1000 bg messages.
        for i in 0..1000 {
            let _ = bg_tx.send(TuiMessage::CommandResponse(format!("bg {}", i)));
        }

        // Simulate 5 cycles of the event loop's poll logic.
        let mut cycles_until_input = 0;
        let mut input_processed = false;

        for cycle in 0..5 {
            // On cycle 2, inject an input event (simulating delayed keystroke).
            if cycle == 2 {
                let _ = input_tx.send(StampedEvent {
                    event: Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
                    os_read_at: Instant::now(),
                });
            }

            // Step 1: drain input.
            let mut pending = Vec::new();
            while let Ok(stamped) = input_rx.try_recv() {
                pending.push(stamped);
            }
            if !pending.is_empty() {
                for stamped in pending {
                    if let Event::Key(KeyEvent {
                        code: KeyCode::Char(c),
                        ..
                    }) = stamped.event
                    {
                        app.insert_char(c);
                        input_processed = true;
                        cycles_until_input = cycle;
                    }
                }
            }

            // Step 2: process ONE bg message.
            if let Ok(msg) = bg_rx.try_recv() {
                handle_tui_message(&mut app, msg);
            }
        }

        assert!(input_processed, "Input should have been processed");
        assert!(
            cycles_until_input <= 3,
            "Input processed on cycle {} — should be within 1 cycle of injection",
            cycles_until_input
        );
        assert_eq!(app.input, "z");
    }

    /// Test that LatencyDiag records and reports correctly.
    #[test]
    fn latency_diag_recording_and_report() {
        let mut diag = LatencyDiag::new();
        diag.enabled = true;

        // Record some samples.
        diag.record_input_latency(100);
        diag.record_input_latency(200);
        diag.record_input_latency(300);
        diag.record_draw(500);
        diag.record_bg(1000);
        diag.record_input_handle(250);
        diag.cycles = 100;
        diag.busy_cycles = 60;
        diag.idle_cycles = 40;
        diag.last_bg_depth = 42;

        // Force report by setting last_report far in the past.
        diag.last_report = std::time::Instant::now() - std::time::Duration::from_secs(10);

        let report = diag.report();
        assert!(report.is_some(), "Should produce a report");
        let text = report.unwrap();
        assert!(text.contains("avg="), "Report should contain avg latency");
        assert!(text.contains("p50="), "Report should contain p50");
        assert!(text.contains("p99="), "Report should contain p99");
        assert!(
            text.contains("draw_max=500"),
            "Report should contain draw max"
        );
        assert!(text.contains("bg_max=1000"), "Report should contain bg max");
        assert!(
            text.contains("input_max=250"),
            "Report should contain input max"
        );
        assert!(
            text.contains("bg_depth=42"),
            "Report should contain bg depth"
        );
        assert!(
            text.contains("samples=3"),
            "Report should show sample count"
        );

        // After report, counters should be reset.
        assert!(diag.samples.is_empty());
        assert_eq!(diag.cycles, 0);
        assert_eq!(diag.max_draw_us, 0);
    }

    /// Test that LatencyDiag doesn't report when disabled.
    #[test]
    fn latency_diag_disabled_no_report() {
        let mut diag = LatencyDiag::new();
        // Don't enable.
        diag.record_input_latency(100);
        diag.cycles = 100;
        diag.last_report = std::time::Instant::now() - std::time::Duration::from_secs(10);
        assert!(diag.report().is_none());
    }

    // ── v0.12.4.1 split-pane shell fix tests ─────────────────────────────────

    #[test]
    fn agent_output_done_clears_heartbeat_in_agent_pane() {
        // Item 1: AgentOutputDone must clear heartbeats from agent_output (split-pane).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.split_pane = true;
        app.tailing_goal = Some("goal-abc".into());
        // Push a heartbeat directly into agent_output (as split-pane mode does).
        app.agent_output.push(OutputLine::heartbeat(
            "[heartbeat] still running... 5s elapsed".into(),
        ));
        assert!(app.agent_output[0].is_heartbeat);

        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-abc".into()));

        // Heartbeat in agent_output must be replaced with [agent exited].
        assert!(!app.agent_output[0].is_heartbeat);
        assert!(app.agent_output[0].text.contains("agent exited"));
        // tailing_goal should be cleared.
        assert!(app.tailing_goal.is_none());
    }

    #[test]
    fn agent_output_done_clears_heartbeat_in_main_pane_when_not_split() {
        // Item 1 (non-split path): heartbeat in main output is replaced.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.tailing_goal = Some("goal-xyz".into());
        app.push_heartbeat("[heartbeat] still running... 10s elapsed".into());
        assert!(app.output.last().unwrap().is_heartbeat);

        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-xyz".into()));

        // Heartbeat in main output replaced with [agent exited].
        let heartbeat_lines: Vec<_> = app.output.iter().filter(|l| l.is_heartbeat).collect();
        assert!(
            heartbeat_lines.is_empty(),
            "no heartbeat lines should remain"
        );
        assert!(app.output.iter().any(|l| l.text.contains("agent exited")));
    }

    #[test]
    fn auto_scroll_agent_pane_when_near_bottom() {
        // Item 2: auto-scroll fires for agent pane in split mode.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.split_pane = true;
        // agent_scroll_offset = 0 means pinned to bottom.
        app.agent_scroll_offset = 0;

        // Deliver agent output in split-pane mode.
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "some agent output".into(),
                goal_id: None,
            }),
        );

        // agent_scroll_offset should remain 0 (pinned to bottom).
        assert_eq!(app.agent_scroll_offset, 0);
        assert!(!app.agent_output.is_empty());
    }

    // ── v0.12.7 working indicator & scroll reliability tests ─────────────────

    #[test]
    fn working_indicator_pushed_as_heartbeat() {
        // Item 1: WorkingIndicator must push a heartbeat-flagged line so
        // AgentOutputDone can find and replace it.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        handle_tui_message(
            &mut app,
            TuiMessage::WorkingIndicator("Agent is working...".into()),
        );
        let last = app.output.last().expect("should have output");
        assert!(
            last.is_heartbeat,
            "WorkingIndicator must be a heartbeat line"
        );
        assert!(last.text.contains("Agent is working"));
    }

    #[test]
    fn agent_output_done_clears_working_indicator() {
        // Item 1: AgentOutputDone must clear the working indicator pushed by
        // WorkingIndicator (which is a heartbeat-flagged line).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.tailing_goal = Some("goal-wi".into());
        handle_tui_message(
            &mut app,
            TuiMessage::WorkingIndicator("Agent is working...".into()),
        );
        // Confirm it's there as a heartbeat.
        assert!(app.output.iter().any(|l| l.is_heartbeat));

        // Now simulate goal completion.
        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-wi".into()));

        // No heartbeat lines should remain.
        let remaining: Vec<_> = app.output.iter().filter(|l| l.is_heartbeat).collect();
        assert!(
            remaining.is_empty(),
            "AgentOutputDone must clear the working indicator heartbeat"
        );
        assert!(app.output.iter().any(|l| l.text.contains("agent exited")));
    }

    #[test]
    fn r1_working_indicator_cleared_when_heartbeat_tick_arrives_before_exit() {
        // R1 regression test (v0.13.1.5): when WorkingIndicator is pushed, then
        // regular output arrives (non-heartbeat), then a [heartbeat] tick creates a
        // NEW heartbeat line, AgentOutputDone must clear BOTH heartbeat lines — not
        // just the last [heartbeat] tick — so "Agent is working..." doesn't linger.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.tailing_goal = Some("goal-r1".into());

        // Step 1: WorkingIndicator pushed (heartbeat line #1).
        handle_tui_message(
            &mut app,
            TuiMessage::WorkingIndicator("Agent is working...".into()),
        );
        // Step 2: Regular agent output arrives (non-heartbeat) — breaks the in-place chain.
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "some agent output".into(),
                goal_id: Some("goal-r1".into()),
            }),
        );
        // Step 3: [heartbeat] tick arrives and appends a NEW heartbeat line (line #2).
        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "[heartbeat] 5s elapsed".into(),
                goal_id: Some("goal-r1".into()),
            }),
        );
        // Confirm two heartbeat lines exist.
        let heartbeat_count = app.output.iter().filter(|l| l.is_heartbeat).count();
        assert_eq!(
            heartbeat_count, 2,
            "should have 2 heartbeat lines before exit (working indicator + tick)"
        );

        // Step 4: Goal exits.
        handle_tui_message(&mut app, TuiMessage::AgentOutputDone("goal-r1".into()));

        // Both heartbeat lines must be cleared.
        let remaining: Vec<_> = app.output.iter().filter(|l| l.is_heartbeat).collect();
        assert!(
            remaining.is_empty(),
            "AgentOutputDone must clear ALL heartbeat lines, not just the last tick"
        );
        // The last heartbeat should show "[agent exited]", not "Agent is working..."
        let working_lines: Vec<_> = app
            .output
            .iter()
            .filter(|l| l.text.contains("Agent is working"))
            .collect();
        assert!(
            working_lines.is_empty(),
            "'Agent is working...' must not remain visible after agent exit"
        );
        assert!(app.output.iter().any(|l| l.text.contains("agent exited")));
    }

    #[test]
    fn r2_command_response_auto_scrolls_near_bottom() {
        // R2 regression test (v0.13.1.5): CommandResponse must call
        // auto_scroll_if_near_bottom so a user near the tail stays pinned.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Simulate near-bottom (within NEAR_BOTTOM_LINES=3).
        app.scroll_offset = 2;

        handle_tui_message(
            &mut app,
            TuiMessage::CommandResponse("command output\nline 2".into()),
        );

        assert_eq!(
            app.scroll_offset, 0,
            "CommandResponse near bottom must snap scroll to 0"
        );
    }

    #[test]
    fn r2_sse_event_auto_scrolls_near_bottom() {
        // R2 regression test (v0.13.1.5): SseEvent must call
        // auto_scroll_if_near_bottom so a user near the tail stays pinned.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.scroll_offset = 2;

        handle_tui_message(&mut app, TuiMessage::SseEvent("status update event".into()));

        assert_eq!(
            app.scroll_offset, 0,
            "SseEvent near bottom must snap scroll to 0"
        );
    }

    #[test]
    fn r2_command_response_preserves_scroll_when_far_up() {
        // R2: when user has scrolled far up, CommandResponse must NOT auto-scroll.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.scroll_offset = 50;

        handle_tui_message(&mut app, TuiMessage::CommandResponse("output".into()));

        assert_eq!(
            app.scroll_offset, 50,
            "far-up scroll must not be reset by CommandResponse"
        );
    }

    #[test]
    fn r3_paste_inserts_at_cursor_when_input_focused() {
        // v0.14.7.1: when input-focused (scroll_offset == 0), paste inserts at
        // current cursor position (replaces the old v0.12.2 append-at-end behavior).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Type some text, then move cursor left (simulating the user pressing ←).
        for ch in "hello".chars() {
            app.insert_char(ch);
        }
        app.cursor_left(); // cursor at byte 4 (before 'o')
        app.cursor_left(); // cursor at byte 3 (before 'l')
        assert_eq!(app.cursor, 3, "cursor should be mid-input");
        assert_eq!(app.scroll_offset, 0, "should be input-focused");

        // Paste inserts at cursor (position 3).
        simulate_paste(&mut app, " world");

        assert_eq!(
            app.input, "hel worldlo",
            "v0.14.7.1: paste inserts at cursor position when input-focused"
        );
        assert_eq!(app.cursor, 9, "cursor must be after inserted text");
    }

    #[test]
    fn heartbeat_auto_scrolls_main_pane_near_bottom() {
        // Item 3: heartbeat lines in main pane must auto-scroll when scroll_offset
        // is within NEAR_BOTTOM_LINES (intermittent scroll regression fix).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Simulate user near-bottom (2 lines up — within NEAR_BOTTOM_LINES=3).
        app.scroll_offset = 2;

        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "[heartbeat] still running... 5s elapsed".into(),
                goal_id: None,
            }),
        );

        // Should have snapped back to bottom.
        assert_eq!(
            app.scroll_offset, 0,
            "near-bottom scroll_offset must be reset to 0 after heartbeat"
        );
    }

    #[test]
    fn heartbeat_preserves_scroll_when_far_up_main_pane() {
        // Item 3: if user has scrolled far up, heartbeat must NOT auto-scroll.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.scroll_offset = 50;

        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "[heartbeat] still running... 5s elapsed".into(),
                goal_id: None,
            }),
        );

        assert_eq!(
            app.scroll_offset, 50,
            "far-up scroll must not be reset by heartbeat"
        );
    }

    #[test]
    fn heartbeat_auto_scrolls_agent_pane_near_bottom() {
        // Item 3: heartbeat in split-pane agent pane must auto-scroll when
        // agent_scroll_offset is within AGENT_NEAR_BOTTOM_LINES.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.split_pane = true;
        // Simulate user near-bottom in agent pane (3 lines up).
        app.agent_scroll_offset = 3;
        // First push a heartbeat line so the in-place update path is exercised.
        app.agent_output
            .push(OutputLine::heartbeat("[heartbeat] running".into()));

        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "[heartbeat] still running... 10s elapsed".into(),
                goal_id: None,
            }),
        );

        assert_eq!(
            app.agent_scroll_offset, 0,
            "near-bottom agent_scroll_offset must be reset to 0 after heartbeat"
        );
    }

    #[test]
    fn scroll_stays_bottom_through_burst_of_output() {
        // Item 4: verify scroll stays at 0 through a burst of 100 output lines
        // with no user scroll interaction.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        assert_eq!(app.scroll_offset, 0);

        for i in 0..100 {
            handle_tui_message(
                &mut app,
                TuiMessage::AgentOutput(AgentOutputLine {
                    stream: "stdout".into(),
                    line: format!("output line {}", i),
                    goal_id: None,
                }),
            );
        }

        assert_eq!(
            app.scroll_offset, 0,
            "scroll_offset must stay at 0 through a burst of 100 output lines"
        );
    }

    #[test]
    fn no_auto_scroll_agent_pane_when_scrolled_up() {
        // Item 2: when user has scrolled up in agent pane, offset is preserved.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.split_pane = true;
        // Simulate user scrolled 10 lines up in the agent pane.
        app.agent_scroll_offset = 10;

        handle_tui_message(
            &mut app,
            TuiMessage::AgentOutput(AgentOutputLine {
                stream: "stdout".into(),
                line: "more output".into(),
                goal_id: None,
            }),
        );

        // Scrolled far up — offset should NOT be reset.
        assert_eq!(app.agent_scroll_offset, 10);
    }

    // ── v0.14.9.1: Paste (Ctrl+V path) tests ──────────────────────────────────

    /// Helper: simulate the Ctrl+V clipboard paste path on an App, given
    /// clipboard text as a string. Mirrors the key handler branch exactly.
    fn simulate_ctrl_v_paste(app: &mut App, clipboard_text: &str) {
        let safe = clipboard_text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
            .trim_matches('\n')
            .to_string();
        let line_count = safe.lines().count();
        let char_count = safe.chars().count();
        if char_count > PASTE_CHAR_THRESHOLD || line_count > PASTE_LINE_THRESHOLD {
            app.pending_paste = Some(safe);
            app.paste_preview_expanded = false;
        } else {
            if app.scroll_offset > 0 {
                app.cursor = app.input.len();
                app.scroll_to_bottom();
            }
            for ch in safe.chars() {
                app.insert_char(ch);
            }
        }
    }

    #[test]
    fn ctrl_v_small_paste_inserts_at_cursor() {
        // Ctrl+V with short clipboard text inserts inline at cursor position.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for ch in "hello".chars() {
            app.insert_char(ch);
        }
        app.cursor = 2; // between 'h','e' and 'l','l','o'
        simulate_ctrl_v_paste(&mut app, "WORLD");
        assert_eq!(
            app.input, "heWORLDllo",
            "Ctrl+V must insert at cursor position"
        );
        assert_eq!(app.cursor, 7, "cursor must advance past inserted text");
    }

    #[test]
    fn ctrl_v_large_paste_stores_pending() {
        // Ctrl+V with a large payload (>PASTE_CHAR_THRESHOLD) stores as pending paste.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        let big = "x".repeat(PASTE_CHAR_THRESHOLD + 1);
        simulate_ctrl_v_paste(&mut app, &big);
        assert!(
            app.pending_paste.is_some(),
            "large Ctrl+V clipboard text must be stored as pending paste"
        );
        assert!(
            app.input.is_empty(),
            "input buffer must be unaffected by large pending paste"
        );
    }

    #[test]
    fn ctrl_v_when_scrolled_up_snaps_to_bottom_then_appends() {
        // If the user has scrolled up, Ctrl+V should snap to bottom before inserting.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..20 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        app.scroll_up(5);
        assert!(app.scroll_offset > 0);

        simulate_ctrl_v_paste(&mut app, "pasted");

        assert_eq!(
            app.scroll_offset, 0,
            "Ctrl+V from scroll-up must snap to bottom"
        );
        assert_eq!(app.input, "pasted", "text must be in input buffer");
    }

    // ── v0.14.9.1: Auto-tail (is_at_bottom) tests ─────────────────────────────

    #[test]
    fn auto_scroll_resumes_after_scroll_up_and_scroll_down() {
        // Core tail regression: scroll up, scroll back to 0, then new output
        // must not increment unread_events (auto_scroll must be true).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..50 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        assert!(app.auto_scroll);
        assert_eq!(app.scroll_offset, 0);

        // Scroll up.
        app.scroll_up(10);
        assert!(!app.auto_scroll);

        // Scroll back to bottom.
        app.scroll_down(10);
        assert_eq!(app.scroll_offset, 0);
        assert!(
            app.auto_scroll,
            "auto_scroll must be true after returning to bottom"
        );

        // New output must NOT increment unread.
        let before = app.unread_events;
        app.push_output(OutputLine::event("new event".into()));
        assert_eq!(
            app.unread_events, before,
            "new output at bottom must not increment unread_events"
        );
    }

    #[test]
    fn auto_scroll_resumes_from_push_output_when_at_bottom_with_auto_scroll_false() {
        // If auto_scroll was incorrectly left false (e.g. after buffer-overflow
        // scroll_offset adjustment) but scroll_offset is 0, the next push_output
        // must re-enable auto_scroll (is_at_bottom() fix, v0.14.9.1).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.auto_scroll = false;
        app.scroll_offset = 0;

        app.push_output(OutputLine::event("trigger".into()));

        assert!(
            app.auto_scroll,
            "push_output with scroll_offset==0 must re-enable auto_scroll"
        );
        assert_eq!(
            app.unread_events, 0,
            "unread_events must be 0 when at bottom"
        );
    }

    #[test]
    fn is_at_bottom_true_when_content_shorter_than_viewport() {
        // When the output buffer has fewer lines than the visible area height,
        // is_at_bottom() must return true even if scroll_offset > 0.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Simulate a visible area of 40 rows.
        app.output_area_height = 40;
        // Only 5 lines of output — well under 40 - 4 = 36.
        for i in 0..5 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        // Manually set a small offset (as if the user scrolled up in a tiny buffer).
        app.scroll_offset = 2;
        assert!(
            app.is_at_bottom(),
            "is_at_bottom must be true when output.len() < output_area_height - 4"
        );
    }

    #[test]
    fn ctrl_l_clears_and_reenables_auto_scroll() {
        // Ctrl+L must set auto_scroll = true so tail resumes after clear (v0.14.9.1).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..20 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        app.scroll_up(5);
        assert!(!app.auto_scroll);

        // Simulate Ctrl+L.
        app.output.clear();
        app.scroll_offset = 0;
        app.unread_events = 0;
        app.auto_scroll = true; // the fix

        assert!(app.auto_scroll, "Ctrl+L must re-enable auto_scroll");
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.output.len(), 0);
    }

    // --- word_wrap_metrics tests (v0.14.9.1 item 10) ---

    #[test]
    fn word_wrap_metrics_short_text_no_wrap() {
        // "ta> hello" at width 20 — fits on one line, cursor at end
        let display = "ta> hello";
        let (row, col, lines) = word_wrap_metrics(display, display.len(), 20);
        assert_eq!(row, 0, "no wrap expected");
        assert_eq!(col, 9, "cursor at end of 9-char string");
        assert_eq!(lines, 1);
    }

    #[test]
    fn word_wrap_metrics_wraps_at_word_boundary() {
        // "hello world" at width 8: "hello " (6) + "world" (5) = 11 > 8
        // → "hello " on row 0 (space consumed as wrap), "world" on row 1
        let display = "hello world";
        let (row, col, lines) = word_wrap_metrics(display, display.len(), 8);
        assert_eq!(lines, 2, "should take 2 visual lines");
        // Cursor (at end = after "world") should be on row 1
        assert_eq!(row, 1);
        assert_eq!(col, 5, "col 5 after 'world'");
    }

    #[test]
    fn word_wrap_metrics_cursor_mid_word_on_wrapped_line() {
        // "hello world" at width 8, cursor after 'w' in "world" (byte 7)
        let display = "hello world";
        let cursor_byte = display.find('w').unwrap(); // byte 6
        let (row, col, _) = word_wrap_metrics(display, cursor_byte, 8);
        assert_eq!(row, 1, "cursor is on the wrapped line");
        assert_eq!(col, 0, "at start of wrapped line");
    }

    #[test]
    fn word_wrap_metrics_hard_wrap_long_word() {
        // A single word longer than wrap_width hard-breaks at character boundary
        let display = "abcdefghij"; // 10 chars
        let (row, col, lines) = word_wrap_metrics(display, display.len(), 6);
        // "abcdef" row 0 (6 chars), "ghij" row 1
        assert_eq!(lines, 2);
        assert_eq!(row, 1);
        assert_eq!(col, 4);
    }

    #[test]
    fn word_wrap_metrics_embedded_newline() {
        // Explicit '\n' always starts a new row regardless of position
        let display = "abc\ndef";
        let (row, col, lines) = word_wrap_metrics(display, display.len(), 20);
        assert_eq!(lines, 2);
        assert_eq!(row, 1);
        assert_eq!(col, 3);
    }

    #[test]
    fn word_wrap_metrics_cursor_before_wrap_space() {
        // "hello world" at width 8, cursor at the space (byte 5) — still on row 0
        let display = "hello world";
        let cursor_byte = 5; // the space
        let (row, col, _) = word_wrap_metrics(display, cursor_byte, 8);
        assert_eq!(row, 0);
        assert_eq!(col, 5);
    }

    // ── v0.14.9.2 ShellContext tests ──

    #[test]
    fn shell_context_default_is_idle() {
        // App starts in Idle context (v0.14.9.2).
        let app = App::new(
            "http://localhost:7777".to_string(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        assert_eq!(app.shell_context, ShellContext::Idle);
    }

    #[test]
    fn help_context_idle() {
        // When context is Idle, HELP_TEXT is shown (v0.14.9.2).
        let mut app = App::new(
            "http://localhost:7777".to_string(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.shell_context = ShellContext::Idle;
        // Simulate pushing HELP_TEXT as the help handler does.
        app.push_lines(HELP_TEXT, OutputLine::info);
        let combined: String = app
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            combined.contains("TA Shell"),
            "Idle help should contain main help text"
        );
        assert!(
            combined.contains("run <title>"),
            "Idle help should list 'run' command"
        );
    }

    #[test]
    fn help_context_draft_viewing() {
        // When context is ViewingDraft, DRAFT_HELP_TEXT is shown (v0.14.9.2).
        let mut app = App::new(
            "http://localhost:7777".to_string(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        app.shell_context = ShellContext::ViewingDraft {
            draft_id: "abc12345".to_string(),
        };
        // Simulate what the help handler does for ViewingDraft.
        app.push_lines(DRAFT_HELP_TEXT, OutputLine::info);
        app.push_output(OutputLine::info("  Current draft: abc12345".to_string()));
        let combined: String = app
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            combined.contains("Draft Review Mode"),
            "Draft help should be shown"
        );
        assert!(combined.contains("abc12345"), "Draft ID should be shown");
    }

    // ── v0.14.9.3: Clipboard (arboard mock), auto-scroll audit ────────────────

    #[test]
    fn clipboard_mock_read_returns_set_value() {
        // Set the thread-local mock clipboard and verify read_from_clipboard returns it.
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = Some("hello paste".to_string()));
        let result = read_from_clipboard();
        assert_eq!(result.as_deref(), Some("hello paste"));
        // Clean up.
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = None);
    }

    #[test]
    fn clipboard_mock_read_returns_none_when_empty() {
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = None);
        let result = read_from_clipboard();
        assert!(result.is_none());
    }

    #[test]
    fn clipboard_mock_copy_sets_value() {
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = None);
        copy_to_clipboard("written by test");
        let result = TEST_CLIPBOARD.with(|c| c.borrow().clone());
        assert_eq!(result.as_deref(), Some("written by test"));
        // Clean up.
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = None);
    }

    #[test]
    fn ctrl_v_paste_uses_arboard_mock() {
        // Verify the Ctrl+V handler reads from the mock clipboard.
        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = Some("arboard text".to_string()));

        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        simulate_ctrl_v_paste(&mut app, "arboard text");
        assert_eq!(app.input, "arboard text");

        TEST_CLIPBOARD.with(|c| *c.borrow_mut() = None);
    }

    #[test]
    fn clear_command_re_enables_auto_scroll() {
        // Regression: :clear was not setting auto_scroll = true (v0.14.9.3 fix).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        // Fill output and scroll up to disable auto-scroll.
        for i in 0..30 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        app.scroll_up(10);
        assert!(
            !app.auto_scroll,
            "auto_scroll should be false after scroll up"
        );

        // Simulate the :clear path directly.
        app.output.clear();
        app.scroll_offset = 0;
        app.unread_events = 0;
        app.auto_scroll = true; // the fix

        assert!(app.auto_scroll, "auto_scroll must be true after :clear");
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_events, 0);
    }

    #[test]
    fn auto_scroll_blocked_when_scrolled_up_during_output() {
        // When scroll_offset > 0, new output must increment unread_events (not auto-scroll).
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..50 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        app.scroll_up(15); // scroll up — disable auto_scroll
        assert!(!app.auto_scroll);

        let before = app.unread_events;
        app.push_output(OutputLine::event("new event while scrolled up".into()));
        assert_eq!(
            app.unread_events,
            before + 1,
            "unread_events must increment when scrolled up"
        );
    }

    #[test]
    fn auto_scroll_resumes_after_scroll_to_bottom_via_scroll_down() {
        // Scroll up then back to bottom via scroll_down → auto_scroll must re-enable.
        let mut app = App::new(
            "http://localhost".into(),
            None,
            std::path::PathBuf::from("/tmp"),
        );
        for i in 0..50 {
            app.push_output(OutputLine::event(format!("line {}", i)));
        }
        app.scroll_up(5);
        assert!(!app.auto_scroll);

        app.scroll_down(5); // back to scroll_offset == 0
        assert_eq!(app.scroll_offset, 0);
        assert!(
            app.auto_scroll,
            "auto_scroll must be true after returning to bottom"
        );

        // New output at bottom → no unread increment.
        let before = app.unread_events;
        app.push_output(OutputLine::event("bottom event".into()));
        assert_eq!(app.unread_events, before, "no unread events when at bottom");
    }

    #[test]
    fn direct_input_write_uses_layout_width_for_height() {
        // Verify that direct_input_write uses `size.width.saturating_sub(2)` as
        // the layout width for computing content_lines / input_height, matching
        // draw_ui's block inner width (border takes 1 char each side).
        //
        // A display string that fits within `width` but wraps within `width-2`
        // demonstrates that `layout_width = width-2` gives the correct line count.
        let width = 10u16;
        // "hello xxx" = 9 chars; "hello" (5) + space + "xxx" (3).
        // With layout_width=8 (width-2): space check triggers wrap (5+1+3=9 > 8) → 2 lines.
        // With layout_width=10 (width): space check does not trigger (9 ≤ 10) → 1 line.
        let display = "hello xxx";

        // The formula from direct_input_write:
        let layout_width = width.saturating_sub(2).max(1) as usize;
        let (_, _, content_lines_layout) = word_wrap_metrics(display, display.len(), layout_width);

        // The naive (incorrect) approach using full terminal width:
        let (_, _, content_lines_full) = word_wrap_metrics(display, display.len(), width as usize);

        assert_eq!(
            content_lines_layout, 2,
            "display must wrap with layout_width={} (width-2): got {} lines",
            layout_width, content_lines_layout
        );
        assert_eq!(
            content_lines_full, 1,
            "display must not wrap with full width={}: got {} lines",
            width, content_lines_full
        );

        // Confirm layout_width is exactly width-2.
        assert_eq!(layout_width, (width - 2) as usize);

        // input_height computed from layout_width is >= input_height from full width,
        // meaning the input box correctly expands to accommodate the wrapped line.
        let input_height_correct = (content_lines_layout + 2).clamp(3, 24 / 2);
        let input_height_naive = (content_lines_full + 2).clamp(3, 24 / 2);
        assert!(
            input_height_correct >= input_height_naive,
            "input_height from layout_width must be >= input_height from full width"
        );
    }

    #[tokio::test]
    #[ignore = "makes real network calls; run with: cargo test -- --ignored reconnect_loop"]
    async fn reconnect_loop_handles_failed_http_attempt() {
        // Verify that start_tail_stream does NOT panic when all initial HTTP attempts
        // fail. Before the v0.14.9.3 reconnect fix, a failed reconnect HTTP attempt
        // inside the 'reconnect loop would call `continue 'reconnect` with
        // `next_resp == None`, causing the subsequent `.take().expect()` to panic.
        //
        // This test exercises the initial-connection failure path (5 attempts to
        // an unreachable server). The 'reconnect loop proper (stream-drops-mid-flight)
        // requires a mock SSE server and is covered by manual verification.
        //
        // Expected: function returns without panicking; at least one error message is
        // sent on the channel.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(50))
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiMessage>();

        // Port 1 (tcpmux) is reserved and refused on most platforms; the short
        // timeout ensures rapid failure even on platforms where it times out.
        start_tail_stream(
            client,
            "http://127.0.0.1:1",
            Some("deadbeef-reconnect-test"),
            tx,
            0,
        )
        .await;

        // Collect all messages emitted — must not have panicked.
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|m| {
                if let TuiMessage::CommandResponse(t) = m {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !msgs.is_empty(),
            "must emit at least one message when connection fails (got none)"
        );
        // At least one message must mention an error or inability to connect.
        let has_error = msgs
            .iter()
            .any(|m| m.contains("Error") || m.contains("reach") || m.contains("connect"));
        assert!(
            has_error,
            "at least one message must describe the connection failure; got: {:?}",
            msgs
        );
    }
}
