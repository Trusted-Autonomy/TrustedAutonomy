//! Capturing a login token from a vendor command that runs in a terminal.
//!
//! `claude setup-token` is an interactive terminal program: it opens a browser,
//! waits for the person to sign in, then paints the long-lived token on the
//! screen. Nothing is written to a file and nothing goes to stdout in a form a
//! pipe can read, so TA runs the command in a pseudo-terminal (a real pty on
//! Unix, ConPTY on Windows), reads what it paints, and extracts the token.
//!
//! The terminal output is not text: words are placed with cursor-movement
//! sequences, frames are redrawn in place, colors wrap everything, and long
//! lines may be wrapped by the terminal width. [`Screen`] replays the bytes
//! onto a tiny virtual screen so the parser reads what a person would see.
//!
//! The token never leaves this module except as the [`Secret`] returned by
//! [`run_vendor_login`]; everything echoed for progress or in an error passes
//! through [`redact`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ta_credentials::agent_login::{Secret, LOGIN_TIMEOUT_ENV};

/// Columns of the pseudo-terminal. Wide enough that the vendor never wraps the
/// token or the sign-in address; the parser still copes if it does.
const PTY_COLS: u16 = 500;
const PTY_ROWS: u16 = 60;
/// A token shorter than this is not a token (a stray word after the header).
const MIN_TOKEN_LEN: usize = 24;
const MAX_TOKEN_LEN: usize = 4096;
/// How long the screen may stay silent after a token appeared before it is
/// taken as complete even though no closing line followed.
const QUIET_AFTER_TOKEN: Duration = Duration::from_secs(3);
/// How long a vendor that already printed its token may take to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);

// ── Virtual screen ─────────────────────────────────────────────────────────

/// The visible text of a stream of terminal output. Unlimited width, so a long
/// line stays one line.
#[derive(Default)]
pub struct Screen {
    rows: Vec<Vec<char>>,
    row: usize,
    col: usize,
    state: Esc,
    params: String,
    saved: (usize, usize),
    utf8: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
    /// Two-byte escape such as `ESC ( B`.
    Charset,
}

impl Screen {
    pub fn new() -> Self {
        Screen {
            rows: vec![Vec::new()],
            ..Default::default()
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    /// The screen as text, one line per row, trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = self
            .rows
            .iter()
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn byte(&mut self, b: u8) {
        match self.state {
            Esc::Text => match b {
                0x1b => {
                    self.flush_utf8();
                    self.state = Esc::Escape;
                }
                b'\n' => {
                    self.flush_utf8();
                    self.row += 1;
                    self.ensure_row();
                }
                b'\r' => {
                    self.flush_utf8();
                    self.col = 0;
                }
                0x08 => {
                    self.flush_utf8();
                    self.col = self.col.saturating_sub(1);
                }
                b'\t' => {
                    self.flush_utf8();
                    let next = (self.col / 8 + 1) * 8;
                    while self.col < next {
                        self.put(' ');
                    }
                }
                0x00..=0x1f | 0x7f => {}
                _ => {
                    self.utf8.push(b);
                    if let Ok(s) = std::str::from_utf8(&self.utf8) {
                        let chars: Vec<char> = s.chars().collect();
                        self.utf8.clear();
                        for c in chars {
                            self.put(c);
                        }
                    } else if self.utf8.len() >= 4 {
                        self.utf8.clear();
                    }
                }
            },
            Esc::Escape => {
                self.state = match b {
                    b'[' => {
                        self.params.clear();
                        Esc::Csi
                    }
                    b']' => Esc::Osc,
                    b'(' | b')' | b'*' | b'+' | b'#' => Esc::Charset,
                    b'7' => {
                        self.saved = (self.row, self.col);
                        Esc::Text
                    }
                    b'8' => {
                        (self.row, self.col) = self.saved;
                        self.ensure_row();
                        Esc::Text
                    }
                    _ => Esc::Text,
                };
            }
            Esc::Charset => self.state = Esc::Text,
            Esc::Osc => match b {
                0x07 => self.state = Esc::Text,
                0x1b => self.state = Esc::OscEscape,
                _ => {}
            },
            Esc::OscEscape => self.state = Esc::Text,
            Esc::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    self.csi(b as char);
                    self.state = Esc::Text;
                } else {
                    self.params.push(b as char);
                }
            }
        }
    }

    fn flush_utf8(&mut self) {
        self.utf8.clear();
    }

    fn ensure_row(&mut self) {
        while self.rows.len() <= self.row {
            self.rows.push(Vec::new());
        }
    }

    fn put(&mut self, c: char) {
        self.ensure_row();
        let line = &mut self.rows[self.row];
        while line.len() < self.col {
            line.push(' ');
        }
        if self.col < line.len() {
            line[self.col] = c;
        } else {
            line.push(c);
        }
        self.col += 1;
    }

    fn numbers(&self) -> Vec<usize> {
        self.params
            .trim_start_matches(['?', '>', '=', '!'])
            .split(';')
            .map(|p| p.parse::<usize>().unwrap_or(0))
            .collect()
    }

    fn csi(&mut self, fin: char) {
        let n = self.numbers();
        let first = n.first().copied().unwrap_or(0);
        let one = first.max(1);
        match fin {
            'A' => self.row = self.row.saturating_sub(one),
            'B' | 'e' => {
                self.row += one;
                self.ensure_row();
            }
            'C' | 'a' => self.col += one,
            'D' => self.col = self.col.saturating_sub(one),
            'E' => {
                self.row += one;
                self.col = 0;
                self.ensure_row();
            }
            'F' => {
                self.row = self.row.saturating_sub(one);
                self.col = 0;
            }
            'G' | '`' => self.col = one - 1,
            'd' => {
                self.row = one - 1;
                self.ensure_row();
            }
            'H' | 'f' => {
                self.row = one - 1;
                self.col = n.get(1).copied().unwrap_or(0).max(1) - 1;
                self.ensure_row();
            }
            'K' => {
                self.ensure_row();
                let col = self.col;
                let line = &mut self.rows[self.row];
                match first {
                    0 => line.truncate(col),
                    1 => {
                        for c in line.iter_mut().take(col + 1) {
                            *c = ' ';
                        }
                    }
                    _ => line.clear(),
                }
            }
            'J' => {
                self.ensure_row();
                match first {
                    0 => {
                        let col = self.col;
                        self.rows[self.row].truncate(col);
                        self.rows.truncate(self.row + 1);
                    }
                    1 => {
                        for r in self.rows.iter_mut().take(self.row) {
                            r.clear();
                        }
                    }
                    _ => {
                        for r in self.rows.iter_mut() {
                            r.clear();
                        }
                    }
                }
            }
            'X' => {
                self.ensure_row();
                let (col, line) = (self.col, &mut self.rows[self.row]);
                for c in line.iter_mut().skip(col).take(one) {
                    *c = ' ';
                }
            }
            's' => self.saved = (self.row, self.col),
            'u' => {
                (self.row, self.col) = self.saved;
                self.ensure_row();
            }
            _ => {}
        }
    }
}

// ── Parsing what the vendor printed ────────────────────────────────────────

fn squash(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_token_line(line: &str) -> bool {
    !line.is_empty()
        && line.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-')
        })
}

/// A token found on the screen.
#[derive(Debug, PartialEq, Eq)]
pub struct FoundToken {
    pub token: String,
    /// Text followed the token, so the vendor finished painting it.
    pub complete: bool,
}

/// Find the login token on `text` (the [`Screen::text`] of the vendor output).
///
/// The vendor prints a header ("Your OAuth token (valid for 1 year):"), then the
/// token, then advice ("Store this token securely..."). The token is every
/// token-looking line after the LAST header (frames are repainted), joined, so a
/// token wrapped across lines is reassembled. If no header is present, a value
/// starting `sk-ant-` on one line is taken instead (never joined with the next
/// line: without the header there is no telling a wrapped token from a word).
pub fn find_token(text: &str) -> Option<FoundToken> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    for (i, line) in lines.iter().enumerate().rev() {
        if !squash(line).starts_with("youroauthtoken") {
            continue;
        }
        let mut token = String::new();
        let mut end = i + 1;
        for (j, l) in lines.iter().enumerate().skip(i + 1) {
            if l.is_empty() {
                if token.is_empty() {
                    continue;
                }
                end = j;
                break;
            }
            if is_token_line(l) {
                token.push_str(l);
                end = j + 1;
            } else {
                end = j;
                break;
            }
        }
        if plausible(&token) {
            let complete = lines.iter().skip(end).any(|l| !l.is_empty());
            return Some(FoundToken { token, complete });
        }
    }
    // No header: a value that looks like an Anthropic token.
    for (i, line) in lines.iter().enumerate().rev() {
        if let Some(pos) = line.find("sk-ant-") {
            let token: String = line[pos..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            let end = i + 1;
            if plausible(&token) {
                let complete = lines.iter().skip(end).any(|l| !l.is_empty());
                return Some(FoundToken { token, complete });
            }
        }
    }
    None
}

fn plausible(token: &str) -> bool {
    (MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&token.len()) && is_token_line(token)
}

/// Sign-in addresses the vendor printed, reassembled when wrapped.
pub fn find_urls(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut urls: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(pos) = line.find("https://") else {
            continue;
        };
        let mut url: String = line[pos..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect();
        if url.len() == line[pos..].len() {
            for l in lines.iter().skip(i + 1).take(8) {
                if !l.is_empty() && !l.contains(char::is_whitespace) && !l.starts_with("http") {
                    url.push_str(l);
                } else {
                    break;
                }
            }
        }
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

/// Replace anything that looks like a secret in `text` before it is shown.
pub fn redact(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|word| {
            let core = word.trim_end();
            let tail = &word[core.len()..];
            let looks_secret = core.len() >= 20
                && is_token_line(core)
                && (core.starts_with("sk-")
                    || core.starts_with("AIza")
                    || core.starts_with("xai-")
                    || core.len() >= 40);
            if looks_secret {
                format!("[redacted]{tail}")
            } else {
                word.to_string()
            }
        })
        .collect()
}

/// The last few non-empty lines of the screen, redacted, for an error message.
fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let from = lines.len().saturating_sub(6);
    redact(&lines[from..].join(" | "))
}

// ── Running the vendor command ─────────────────────────────────────────────

/// Why a vendor login did not produce a token.
#[derive(Debug)]
pub enum VendorError {
    /// The command is not installed (exit code 4).
    NotFound {
        command: String,
        searched: String,
    },
    /// Refused because tests must not reach a paid agent.
    Blocked(String),
    Spawn {
        command: String,
        reason: String,
    },
    Timeout {
        command: String,
        secs: u64,
    },
    /// The command ended without printing a token (closed, cancelled, failed).
    NoToken {
        command: String,
        code: Option<u32>,
        tail: String,
    },
}

impl std::fmt::Display for VendorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VendorError::NotFound { command, searched } => write!(
                f,
                "`{command}` was not found, so the login could not be started. It is searched \
                 on PATH and in the usual user install folders ({searched}). Install it and \
                 retry, or log in with an existing token: \
                 printf %s \"$TOKEN\" | ta agent-login login <framework> --secret-stdin."
            ),
            VendorError::Blocked(message) => f.write_str(message),
            VendorError::Spawn { command, reason } => write!(
                f,
                "`{command}` could not be started ({reason}). Run it by hand to see why, then \
                 retry, or log in with a token: \
                 printf %s \"$TOKEN\" | ta agent-login login <framework> --secret-stdin."
            ),
            VendorError::Timeout { command, secs } => write!(
                f,
                "`{command}` did not finish signing in within {secs} s, so it was stopped and \
                 nothing was stored. Finish the sign-in in the browser sooner, or raise the \
                 limit with {LOGIN_TIMEOUT_ENV} (seconds) or `timeout_secs` in \
                 agent-frameworks.toml, then retry."
            ),
            VendorError::NoToken {
                command,
                code,
                tail,
            } => {
                let code = code
                    .map(|c| format!("exit code {c}"))
                    .unwrap_or_else(|| "no exit code".to_string());
                write!(
                    f,
                    "`{command}` ended ({code}) without printing a login token, so nothing was \
                     stored. The sign-in may have been cancelled or failed; retry \
                     `ta agent-login login <framework>`. Last output: {tail}"
                )
            }
        }
    }
}

impl std::error::Error for VendorError {}

impl VendorError {
    /// The `ta agent-login` exit code for this failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            VendorError::NotFound { .. } => 4,
            _ => 3,
        }
    }
}

/// Folders searched in addition to PATH: a login item or daemon starts with a
/// minimal PATH that lacks where the vendor installs itself.
pub fn extra_search_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        for rel in [
            ".local/bin",
            ".claude/local",
            ".npm-global/bin",
            ".bun/bin",
            ".cargo/bin",
        ] {
            dirs.push(home.join(rel));
        }
        #[cfg(windows)]
        dirs.push(home.join("AppData").join("Roaming").join("npm"));
    }
    #[cfg(unix)]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs
}

/// PATH followed by [`extra_search_dirs`].
pub fn search_path(path: Option<std::ffi::OsString>, home: Option<&Path>) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = path
        .as_deref()
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    for d in extra_search_dirs(home) {
        if !parts.contains(&d) {
            parts.push(d);
        }
    }
    std::env::join_paths(parts).unwrap_or_default()
}

/// Locate `program` (a bare name or a path) on `search`.
pub fn find_binary(program: &str, search: &std::ffi::OsStr) -> Option<PathBuf> {
    if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
        let p = PathBuf::from(program);
        return p.is_file().then_some(p);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    which::which_in(program, Some(search), cwd).ok()
}

/// Run `command` (program and arguments) in a pty and capture the login token
/// it prints. `progress` receives plain, secret-free lines for the person.
pub fn run_vendor_login(
    command: &[String],
    timeout: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<Secret, VendorError> {
    let shown = command.join(" ");
    let program = command.first().map(String::as_str).unwrap_or_default();
    if ta_goal::external_agents_blocked!()
        && ta_goal::external_agents::is_paid_agent_command(program)
    {
        return Err(VendorError::Blocked(
            ta_goal::external_agents::blocked_message(&format!("the login command `{shown}`")),
        ));
    }
    let home = dirs::home_dir();
    let search = search_path(std::env::var_os("PATH"), home.as_deref());
    let Some(binary) = find_binary(program, &search) else {
        return Err(VendorError::NotFound {
            command: program.to_string(),
            searched: std::env::split_paths(&search)
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        });
    };

    let spawn_err = |reason: String| VendorError::Spawn {
        command: shown.clone(),
        reason,
    };
    let pty = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: PTY_ROWS,
            cols: PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| spawn_err(format!("no pseudo-terminal could be opened: {e}")))?;
    let mut builder = portable_pty::CommandBuilder::new(&binary);
    builder.args(&command[1..]);
    builder.env("PATH", &search);
    builder.env("TERM", "xterm-256color");
    if let Some(home) = &home {
        builder.cwd(home);
    }
    let mut child = pty
        .slave
        .spawn_command(builder)
        .map_err(|e| spawn_err(e.to_string()))?;
    // The slave end must be closed here or the reader never sees the end.
    drop(pty.slave);
    let mut reader = pty
        .master
        .try_clone_reader()
        .map_err(|e| spawn_err(format!("the pseudo-terminal could not be read: {e}")))?;
    // Held, not written: dropping a pty writer sends end-of-file to the child.
    let writer = pty.master.take_writer().ok();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    progress(&format!(
        "Starting `{shown}`. Sign in in the browser it opens (waiting up to {} s; \
         limit: {LOGIN_TIMEOUT_ENV}).",
        timeout.as_secs()
    ));

    let deadline = Instant::now() + timeout;
    let mut screen = Screen::new();
    let mut shown_urls: Vec<String> = Vec::new();
    let mut last_output = Instant::now();
    let mut result: Option<Secret> = None;
    let mut token_seen_at: Option<Instant> = None;
    let mut exited: Option<Option<u32>> = None;
    let mut timed_out = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                screen.feed(&chunk);
                last_output = Instant::now();
                let text = screen.text();
                for url in find_urls(&text) {
                    if !shown_urls.contains(&url) {
                        progress(&format!(
                            "If the browser did not open, open this address to sign in: {url}"
                        ));
                        shown_urls.push(url);
                    }
                }
                if let Some(found) = find_token(&text) {
                    token_seen_at.get_or_insert_with(Instant::now);
                    if found.complete {
                        result = Some(Secret::new(found.token));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The vendor closed the terminal: take what was printed.
                let code = child.wait().ok().map(|s| s.exit_code());
                exited = Some(code);
            }
        }
        if result.is_some() {
            break;
        }
        if let Some(found) = find_token(&screen.text()) {
            if token_seen_at.is_some() && last_output.elapsed() >= QUIET_AFTER_TOKEN {
                result = Some(Secret::new(found.token));
                break;
            }
        }
        if exited.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                // Drain what the reader thread still holds.
                while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(300)) {
                    screen.feed(&chunk);
                }
                exited = Some(Some(status.exit_code()));
            }
        }
        if exited.is_some() {
            if let Some(found) = find_token(&screen.text()) {
                result = Some(Secret::new(found.token));
            }
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
    }

    // Stop the vendor (it normally exits by itself right after printing).
    if exited.is_none() {
        let grace = Instant::now() + EXIT_GRACE;
        while Instant::now() < grace && matches!(child.try_wait(), Ok(None)) {
            if timed_out {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    drop(writer);

    if let Some(secret) = result {
        return Ok(secret);
    }
    if timed_out {
        return Err(VendorError::Timeout {
            command: shown,
            secs: timeout.as_secs(),
        });
    }
    Err(VendorError::NoToken {
        command: shown,
        code: exited.flatten(),
        tail: tail_of(&screen.text()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = concat!(
        "sk-ant-",
        "oat01-SENTINELabcdefghijklmnopqrstuvwxyz0123456789_-AbCdEfGhIjKlMnOpQrStUvWxYz"
    );

    #[test]
    fn screen_places_words_with_cursor_columns_and_strips_colors() {
        let mut s = Screen::new();
        s.feed(b"\x1b[1m\x1b[38;5;208mWelcome\x1b[0m\x1b[9Gto\x1b[12GClaude\r\r\nnext\r\n");
        assert_eq!(s.text(), "Welcome to Claude\nnext");
    }

    #[test]
    fn screen_applies_in_place_repaints() {
        let mut s = Screen::new();
        s.feed(b"spinner one\r\nline two\r\n");
        // Ink repaint: up two lines, erase each, write the new frame.
        s.feed(b"\x1b[2A\x1b[2K\rspinner two\r\n\x1b[2Kline two done\r\n");
        assert_eq!(s.text(), "spinner two\nline two done");
    }

    #[test]
    fn finds_a_token_painted_on_one_line() {
        let text = format!(
            "Welcome\n✓ Long-lived authentication token created successfully!\n\n\
             Your OAuth token (valid for 1 year):\n\n{TOKEN}\n\n\
             Store this token securely. You won't be able to see it again."
        );
        let found = find_token(&text).unwrap();
        assert_eq!(found.token, TOKEN);
        assert!(found.complete);
    }

    #[test]
    fn reassembles_a_token_wrapped_across_lines() {
        let (a, rest) = TOKEN.split_at(30);
        let (b, c) = rest.split_at(25);
        let text = format!(
            "Your OAuth token (valid for 1 year):\n{a}\n{b}\n{c}\n\nStore this token securely."
        );
        assert_eq!(find_token(&text).unwrap().token, TOKEN);
    }

    #[test]
    fn a_token_without_closing_text_is_not_complete_yet() {
        let text = format!("Your OAuth token (valid for 1 year):\n{TOKEN}");
        let found = find_token(&text).unwrap();
        assert_eq!(found.token, TOKEN);
        assert!(!found.complete);
    }

    #[test]
    fn the_last_header_wins_when_frames_were_repainted() {
        let text = format!(
            "Your OAuth token (valid for 1 year):\n{}OLDOLDOLDOLDOLDOLDOLDOLDOLD\n\
             Store this token securely.\nYour OAuth token (valid for 1 year):\n{TOKEN}\nStore this",
            concat!("sk-ant-", "oat01-")
        );
        assert_eq!(find_token(&text).unwrap().token, TOKEN);
    }

    #[test]
    fn falls_back_to_an_anthropic_shaped_value_without_a_header() {
        let text = format!("something changed\nhere it is: {TOKEN}\nbye");
        assert_eq!(find_token(&text).unwrap().token, TOKEN);
    }

    #[test]
    fn prose_and_short_words_are_not_tokens() {
        assert!(
            find_token("Your OAuth token (valid for 1 year):\nStore this token securely.")
                .is_none()
        );
        assert!(find_token("Your OAuth token:\nshort\n").is_none());
        assert!(find_token("nothing here").is_none());
    }

    #[test]
    fn urls_are_reassembled_when_wrapped() {
        let text = "Browser didn't open? Use the url below to sign in (c to copy)\n\
                    https://claude.com/cai/oauth/authorize?code=true&client_id=abc&redirect\n\
                    _uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=xyz\n\
                    \n\
                    Paste code here if prompted >";
        let urls = find_urls(text);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].ends_with("callback&state=xyz"), "{urls:?}");
    }

    #[test]
    fn redact_hides_token_shaped_words_only() {
        let shown = redact(&format!("error near {TOKEN} and a normal sentence"));
        assert!(!shown.contains("SENTINEL"));
        assert!(shown.contains("[redacted]"));
        assert!(shown.contains("normal sentence"));
    }

    #[test]
    fn search_path_adds_the_user_install_folders_after_path() {
        let home = Path::new("/home/someone");
        let joined = search_path(Some("/usr/bin".into()), Some(home));
        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parts[0], PathBuf::from("/usr/bin"));
        assert!(parts.contains(&home.join(".local/bin")));
    }

    #[test]
    fn a_missing_command_is_reported_as_not_found_with_exit_code_4() {
        let mut lines = Vec::new();
        let err = run_vendor_login(
            &["ta-no-such-vendor-login-command".to_string()],
            Duration::from_secs(5),
            &mut |l| lines.push(l.to_string()),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert!(matches!(err, VendorError::NotFound { .. }));
        assert!(err.to_string().contains("--secret-stdin"));
    }

    #[test]
    fn the_test_guard_refuses_a_command_named_like_a_paid_agent() {
        let err = run_vendor_login(
            &["claude".to_string(), "setup-token".to_string()],
            Duration::from_secs(5),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, VendorError::Blocked(_)), "{err}");
        assert_eq!(err.exit_code(), 3);
    }
}
