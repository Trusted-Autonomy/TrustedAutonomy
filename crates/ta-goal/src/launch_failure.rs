//! Why a launch failed, as one of six stable codes with a plain explanation.
//!
//! A goal that never starts used to leave nothing behind but a log line, so the
//! task that asked for it stayed "in progress" with nobody working on it. This
//! module turns the evidence TA already has (exit status, stderr, whether the
//! agent wrote anything, whether the launch was killed by its timeout, and
//! whether a login is reachable) into a [`LaunchFailure`]: a code the Wayfinder
//! poller acts on, a `detail` saying what happened and a `fix` saying what the
//! person should do.
//!
//! The codes are the wire vocabulary of the `launch_failed` outcome the daemon
//! publishes (see `docs/USAGE.md`, "When an agent cannot start"):
//!
//! | code | meaning | retry helps? |
//! |------|---------|--------------|
//! | `no_login` | no usable credential for the agent framework | no, a person logs in |
//! | `login_invalid` | a credential exists but the vendor rejected it | no, a person logs in again |
//! | `agent_missing` | the agent binary is not installed or not on `PATH` | no |
//! | `sandbox_denied` | the sandbox refused to start, or denied the agent something it needs to start | no |
//! | `crashed` | the launch exited non-zero before doing any work | maybe |
//! | `timed_out` | the launch was killed after its timeout without finishing | maybe |
//!
//! Secrets: nothing here ever contains a credential. `detail` is built from
//! fixed phrases plus the evidence line the caller passes in, which the caller
//! has already passed through secret redaction; the classifier itself only
//! matches substrings and never copies stderr into its output.

use serde::{Deserialize, Serialize};

use crate::agent_start::{classify_stderr, StartFailureKind};

/// The line `ta run` prints on stderr when its launch failed for a classified
/// reason, followed by the code: `ta-launch-failure: code=no_login`. The daemon
/// reads it so it never has to guess from free text. It carries the code only:
/// the explanation is rebuilt by the reader from its own evidence, so a line an
/// agent prints to its own stderr can at worst pick a code, never inject text.
pub const LAUNCH_FAILURE_MARKER: &str = "ta-launch-failure:";

/// Longest `detail` or `fix` produced, in characters.
pub const MAX_TEXT_CHARS: usize = 400;

/// Why a launch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LaunchFailureCode {
    NoLogin,
    LoginInvalid,
    AgentMissing,
    SandboxDenied,
    Crashed,
    TimedOut,
}

impl LaunchFailureCode {
    /// Every code, in wire order.
    pub const ALL: [LaunchFailureCode; 6] = [
        LaunchFailureCode::NoLogin,
        LaunchFailureCode::LoginInvalid,
        LaunchFailureCode::AgentMissing,
        LaunchFailureCode::SandboxDenied,
        LaunchFailureCode::Crashed,
        LaunchFailureCode::TimedOut,
    ];

    /// The wire string (`no_login`, `login_invalid`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchFailureCode::NoLogin => "no_login",
            LaunchFailureCode::LoginInvalid => "login_invalid",
            LaunchFailureCode::AgentMissing => "agent_missing",
            LaunchFailureCode::SandboxDenied => "sandbox_denied",
            LaunchFailureCode::Crashed => "crashed",
            LaunchFailureCode::TimedOut => "timed_out",
        }
    }

    /// Parse the wire string; `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// True when running the same launch again cannot succeed until a person
    /// changes something (logs in, installs the agent, fixes the sandbox). The
    /// daemon stops retrying such a message at once instead of burning its
    /// retry budget, and the poller puts the task on hold instead of reopening it.
    pub fn is_permanent(self) -> bool {
        !matches!(
            self,
            LaunchFailureCode::Crashed | LaunchFailureCode::TimedOut
        )
    }
}

impl std::fmt::Display for LaunchFailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a usable login for an agent framework is reachable. Answered by
/// `ta_runtime::login_probe`; defined here so the classifier does not depend on
/// the runtime crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// A credential is reachable (in the environment or in TA's custody).
    Present,
    /// No credential is reachable.
    Missing,
    /// A credential is stored but the vendor rejected it when last verified.
    Invalid,
    /// Not known (cannot be checked for this agent, or the check failed).
    Unknown,
}

/// A classified launch failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchFailure {
    pub code: LaunchFailureCode,
    /// What happened. One line, no trailing period, no secrets.
    pub detail: String,
    /// What the person should do. One line, no trailing period.
    pub fix: String,
}

/// Everything known about one failed launch.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchEvidence<'a> {
    /// The agent id the launch was for (`claude-code`, `codex`, ...).
    pub agent: &'a str,
    /// The agent command that was to be run, when known.
    pub command: Option<&'a str>,
    pub exit_code: Option<i32>,
    /// The launch was killed because it ran past its timeout.
    pub timed_out: bool,
    /// The timeout that applied, in seconds, when `timed_out`.
    pub timeout_secs: Option<u64>,
    /// The launch's stderr (or the error text `ta run` returned). Only
    /// searched, never copied into the result.
    pub stderr: &'a str,
    /// Lines the agent wrote to stdout, when known. `Some(0)` means it never
    /// started its model session.
    pub output_lines: Option<usize>,
    /// Whether a login is reachable for `agent`.
    pub login: Option<LoginState>,
    /// Where the full launch output is saved, when it is.
    pub log_path: Option<&'a str>,
    /// One already-redacted line of evidence (typically the last stderr line)
    /// to quote in `detail`.
    pub evidence_line: Option<&'a str>,
    /// What the agent's own stream-json output reported, when it reported a failure.
    pub stream_failure: Option<StreamFailureKind>,
    /// The launch belongs to a Wayfinder-managed project (its `.ta/workflow.toml`
    /// has a `[plan.wayfinder]` project, which is what `vt-install` writes). A login
    /// failure's fix then points at the Wayfinder Settings flow first and at the CLI
    /// second; elsewhere the CLI line comes first and Wayfinder is not mentioned.
    pub wayfinder_managed: bool,
}

/// What an agent's own stream-json output said went wrong. The agent can exit
/// with a plain failure after printing these, with TA's own markers absent, so
/// the launcher reads them from the stream instead of trusting the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFailureKind {
    /// The agent said it is not logged in (`Not logged in - Please run /login`).
    NotLoggedIn,
    /// The vendor refused the credential (`authentication_failed`, a 401 or 403,
    /// an expired or invalid key).
    AuthRejected,
    /// The session ended with an API error result and spent no tokens and no
    /// money, so the model never did any work.
    ErrorWithoutWork,
}

fn assistant_text(v: &serde_json::Value) -> String {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn auth_kind(lower_text: &str, status: Option<u64>) -> StreamFailureKind {
    if status.is_some_and(|s| s == 401 || s == 403)
        || contains_any(lower_text, INVALID_LOGIN_NEEDLES)
    {
        StreamFailureKind::AuthRejected
    } else if lower_text.contains("not logged in") || lower_text.contains("please run /login") {
        StreamFailureKind::NotLoggedIn
    } else {
        StreamFailureKind::AuthRejected
    }
}

/// What one line of an agent's stream-json stdout says about its own failure.
/// Only structured fields count (an `authentication_failed` error on an
/// assistant message; an `is_error` result), never free text an agent printed
/// as part of its work, so a task that merely talks about logins is not a
/// failure. Lines that are not JSON are ignored.
pub fn stream_failure(line: &str) -> Option<StreamFailureKind> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let error_field = v.get("error").and_then(|e| e.as_str()).unwrap_or_default();
    let auth_error = error_field == "authentication_failed";
    match v.get("type").and_then(|t| t.as_str())? {
        "assistant" if auth_error => {
            Some(auth_kind(&assistant_text(&v).to_ascii_lowercase(), None))
        }
        "result" if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) => {
            let text = v
                .get("result")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let status = v.get("api_error_status").and_then(|s| s.as_u64());
            let text_is_auth = contains_any(&text, INVALID_LOGIN_NEEDLES)
                || contains_any(&text, NO_LOGIN_NEEDLES)
                || text.contains("please run /login");
            if auth_error || text_is_auth || status.is_some_and(|s| s == 401 || s == 403) {
                return Some(auth_kind(&text, status));
            }
            let tokens: u64 = v
                .get("usage")
                .and_then(|u| u.as_object())
                .map(|u| u.values().filter_map(|n| n.as_u64()).sum())
                .unwrap_or(0);
            let cost = v
                .get("total_cost_usd")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            (tokens == 0 && cost == 0.0).then_some(StreamFailureKind::ErrorWithoutWork)
        }
        _ => None,
    }
}

/// The most telling failure in an agent's stream-json output (a login problem
/// beats a plain error result), or `None` when no line reported one.
pub fn stream_failure_in(text: &str) -> Option<StreamFailureKind> {
    let rank = |k: &StreamFailureKind| match k {
        StreamFailureKind::NotLoggedIn => 0,
        StreamFailureKind::AuthRejected => 1,
        StreamFailureKind::ErrorWithoutWork => 2,
    };
    text.lines().filter_map(stream_failure).min_by_key(rank)
}

impl StreamFailureKind {
    /// A fixed, secret-free sentence for logs and the goal's failure reason.
    pub fn describe(self) -> &'static str {
        match self {
            StreamFailureKind::NotLoggedIn => {
                "the agent reported it is not logged in (authentication_failed)"
            }
            StreamFailureKind::AuthRejected => {
                "the agent reported that its login was rejected (authentication_failed or HTTP 401/403)"
            }
            StreamFailureKind::ErrorWithoutWork => {
                "the agent's session ended with an API error and spent no tokens and no money"
            }
        }
    }

    /// True when the failure is about the login rather than a plain API error.
    pub fn is_login(self) -> bool {
        !matches!(self, StreamFailureKind::ErrorWithoutWork)
    }
}

/// The framework id `ta agent-login login <id>` takes for an agent id.
pub fn framework_for_agent(agent: &str) -> &str {
    match agent {
        "codex" => "openai",
        "grok" => "xai",
        other => other,
    }
}

const SANDBOX_NEEDLES: &[&str] = &[
    "launch refused by the sandbox policy",
    "sandbox setup failed",
    "sandbox-exec:",
    "bwrap:",
    // What an OS sandbox prints when it blocks a write the agent (or its
    // wrapper script) tried: macOS sandbox-exec, bubblewrap, a read-only mount.
    "operation not permitted",
    "read-only file system",
    "deny file-write",
    "sandbox: deny",
];

const MISSING_NEEDLES: &[&str] = &["command not found", "spawn failed:", "program not found"];

const INVALID_LOGIN_NEEDLES: &[&str] = &[
    "invalid api key",
    "invalid_api_key",
    "incorrect api key",
    "invalid x-api-key",
    "authentication_error",
    "authentication failed",
    "oauth token has expired",
    "token has expired",
    "invalid bearer token",
    "401 unauthorized",
    "api error: 401",
    "credentials were rejected",
];

const NO_LOGIN_NEEDLES: &[&str] = &[
    "not logged in",
    "no api key found",
    "missing api key",
    "api key is not set",
    "api key not found",
];

fn contains_any(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(n))
}

/// The code a `ta run` marker line in `text` names, if any. The last valid
/// marker line wins.
pub fn parse_marker(text: &str) -> Option<LaunchFailureCode> {
    parse_marker_full(text).map(|m| m.code)
}

/// A parsed `ta run` marker line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub code: LaunchFailureCode,
    /// The agent id `ta run` resolved, when it printed one that is a plain identifier.
    pub agent: Option<String>,
    /// The agent program `ta run` tried to run, when it printed a plain one.
    pub command: Option<String>,
    /// The goal id `ta run` created for this launch.
    pub goal: Option<String>,
}

fn plain_identifier(v: &str) -> Option<String> {
    let ok = !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then(|| v.to_string())
}

/// Parse the last valid marker line of `text`.
pub fn parse_marker_full(text: &str) -> Option<Marker> {
    text.lines().rev().find_map(|l| {
        let rest = l.trim().strip_prefix(LAUNCH_FAILURE_MARKER)?;
        let mut code = None;
        let (mut agent, mut command, mut goal) = (None, None, None);
        for token in rest.split_whitespace() {
            match token.split_once('=') {
                Some(("code", v)) => code = LaunchFailureCode::parse(v),
                Some(("agent", v)) => agent = plain_identifier(v),
                Some(("command", v)) => command = plain_identifier(v),
                Some(("goal", v)) => goal = plain_identifier(v),
                _ => {}
            }
        }
        Some(Marker {
            code: code?,
            agent,
            command,
            goal,
        })
    })
}

/// The marker line for `code`:
/// `ta-launch-failure: code=no_login agent=claude-code command=claude goal=<uuid>`.
/// An identifier that is not plain is left out.
pub fn marker_line(
    code: LaunchFailureCode,
    agent: Option<&str>,
    command: Option<&str>,
    goal: Option<&str>,
) -> String {
    let mut line = format!("{LAUNCH_FAILURE_MARKER} code={}", code.as_str());
    for (key, value) in [("agent", agent), ("command", command), ("goal", goal)] {
        if let Some(v) = value.and_then(plain_identifier) {
            line.push_str(&format!(" {key}={v}"));
        }
    }
    line
}

/// Printed by `ta run` / `ta draft build` when the agent process finished and
/// the goal produced no changes: the goal was completed with no draft. It is
/// NOT a launch failure.
pub const NO_CHANGES_MARKER: &str = "ta-run-finished: result=no_changes";

/// `ta-run-finished: result=no_changes goal=<uuid>`.
pub fn no_changes_marker_line(goal: &str) -> String {
    match plain_identifier(goal) {
        Some(g) => format!("{NO_CHANGES_MARKER} goal={g}"),
        None => NO_CHANGES_MARKER.to_string(),
    }
}

/// The goal id (empty when the line carried none) of the last
/// `no_changes` marker line in `text`; `None` when there is no such line.
pub fn parse_no_changes_marker(text: &str) -> Option<String> {
    text.lines().rev().find_map(|l| {
        let rest = l.trim().strip_prefix(NO_CHANGES_MARKER)?;
        Some(
            rest.split_whitespace()
                .find_map(|t| t.strip_prefix("goal=").and_then(plain_identifier))
                .unwrap_or_default(),
        )
    })
}

/// The first path a sandbox denial in `stderr` names (a token starting with
/// `/` or a drive path on the same line as a denial phrase), if any.
pub fn denied_path(stderr: &str) -> Option<String> {
    const DENIAL: &[&str] = &[
        "operation not permitted",
        "read-only file system",
        "deny file-write",
        "sandbox: deny",
    ];
    stderr
        .lines()
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            DENIAL.iter().any(|n| lower.contains(n))
        })
        .find_map(|l| {
            l.split_whitespace()
                .filter_map(|tok| {
                    let t = tok.trim_matches(|c: char| {
                        matches!(c, ':' | ',' | ';' | '\'' | '"' | '(' | ')')
                    });
                    let abs = t.starts_with('/')
                        || (t.len() > 2
                            && t.as_bytes()[1] == b':'
                            && t.as_bytes()[0].is_ascii_alphabetic());
                    (abs && t.len() > 1).then(|| one_line(t))
                })
                .next_back()
        })
}

/// True when `stderr` carries a sandbox refusal or denial.
pub fn sandbox_denial(stderr: &str) -> bool {
    contains_any(&stderr.to_ascii_lowercase(), SANDBOX_NEEDLES)
}

/// Decide the code from the evidence. Order, first match wins:
/// a `ta run` marker naming anything but `crashed` (`ta run` knows its own
/// refusals; `crashed` is its "unclassified", which the evidence may refine);
/// the sandbox refusing or denying; the agent missing; the
/// vendor rejecting the credential; a fatal "no login" message; the launch
/// being killed by its timeout; the login probe saying `Invalid` or `Missing`
/// for an agent that wrote nothing; otherwise `crashed`.
pub fn classify(ev: &LaunchEvidence<'_>) -> LaunchFailureCode {
    if let Some(code) = parse_marker(ev.stderr).filter(|c| *c != LaunchFailureCode::Crashed) {
        return code;
    }
    let lower = ev.stderr.to_ascii_lowercase();
    if contains_any(&lower, SANDBOX_NEEDLES)
        || classify_stderr(ev.stderr).map(|c| c.kind) == Some(StartFailureKind::PromptUnreadable)
    {
        return LaunchFailureCode::SandboxDenied;
    }
    // "command not found" is also what a shell prints when the agent's own tool
    // call names a missing program, so it only counts for an agent that wrote nothing.
    let did_work = ev.output_lines.is_some_and(|n| n > 0);
    if !did_work && contains_any(&lower, MISSING_NEEDLES) {
        return LaunchFailureCode::AgentMissing;
    }
    // The agent's own stream said it could not authenticate: a login problem
    // whatever the exit status or the rest of the output was.
    match ev.stream_failure {
        Some(StreamFailureKind::NotLoggedIn) => return LaunchFailureCode::NoLogin,
        Some(StreamFailureKind::AuthRejected) => {
            return if ev.login == Some(LoginState::Missing) {
                LaunchFailureCode::NoLogin
            } else {
                LaunchFailureCode::LoginInvalid
            };
        }
        _ => {}
    }
    if contains_any(&lower, INVALID_LOGIN_NEEDLES) {
        return LaunchFailureCode::LoginInvalid;
    }
    if classify_stderr(ev.stderr).map(|c| c.kind) == Some(StartFailureKind::NoLogin)
        || contains_any(&lower, NO_LOGIN_NEEDLES)
    {
        return LaunchFailureCode::NoLogin;
    }
    if ev.timed_out {
        return LaunchFailureCode::TimedOut;
    }
    let wrote_nothing = ev.output_lines == Some(0) || ev.output_lines.is_none();
    if wrote_nothing {
        match ev.login {
            Some(LoginState::Invalid) => return LaunchFailureCode::LoginInvalid,
            Some(LoginState::Missing) => return LaunchFailureCode::NoLogin,
            _ => {}
        }
    }
    LaunchFailureCode::Crashed
}

fn one_line(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut t: String = flat.chars().take(MAX_TEXT_CHARS).collect();
    while t.ends_with(['.', ' ']) {
        t.pop();
    }
    t
}

/// The sentence a person acts on for a login failure. In a Wayfinder-managed project
/// the login is a Wayfinder flow (Settings, Agent login, "Log in on this computer": the
/// poller opens the sign-in window here and stores the login in TA custody), so that
/// comes first and the CLI line is the second sentence for people running TA by hand.
/// Outside such a project the CLI line is the fix.
fn login_fix(framework: &str, again: bool, wayfinder_managed: bool) -> String {
    if wayfinder_managed {
        format!(
            "Open Wayfinder Settings, Agent login, and log in{} on this computer. If you run TA by hand, run: ta agent-login login {framework}",
            if again { " again" } else { "" }
        )
    } else {
        if again {
            format!("Run again: ta agent-login login {framework} (the stored login was rejected)")
        } else {
            format!("Run: ta agent-login login {framework}")
        }
    }
}

/// Build the explanation for `code` from the evidence.
pub fn describe(code: LaunchFailureCode, ev: &LaunchEvidence<'_>) -> LaunchFailure {
    let agent = if ev.agent.is_empty() {
        "the agent"
    } else {
        ev.agent
    };
    // TA's default agent when none is named.
    let framework = if ev.agent.is_empty() {
        "claude-code"
    } else {
        framework_for_agent(ev.agent)
    };
    let quoted = ev
        .evidence_line
        .map(one_line)
        .filter(|l| !l.is_empty())
        .map(|l| format!(" (it printed: {l})"))
        .unwrap_or_default();
    let (detail, fix) = match code {
        LaunchFailureCode::NoLogin => (
            format!("{agent} has no usable login, so it could not start"),
            login_fix(framework, false, ev.wayfinder_managed),
        ),
        LaunchFailureCode::LoginInvalid => (
            format!("the login for {agent} was rejected, so it could not start{quoted}"),
            login_fix(framework, true, ev.wayfinder_managed),
        ),
        LaunchFailureCode::AgentMissing => {
            let cmd = ev.command.filter(|c| !c.is_empty()).unwrap_or(agent);
            (
                format!("the agent program '{cmd}' was not found on this computer"),
                format!(
                    "Install {cmd} and make sure it is on the PATH of the process that starts ta-daemon, then run: ta daemon restart"
                ),
            )
        }
        LaunchFailureCode::SandboxDenied => {
            let denied = denied_path(ev.stderr)
                .map(|p| format!(" (denied path: {p})"))
                .unwrap_or_default();
            (
                format!("the sandbox refused to start {agent} or denied it something it needs to start{denied}{quoted}"),
                "Run: ta sandbox explain to see what was blocked, then adjust the sandbox policy (see Sandbox in docs/USAGE.md)".to_string(),
            )
        }
        LaunchFailureCode::Crashed => {
            let exit = ev
                .exit_code
                .map(|c| format!("exited with status {c}"))
                .unwrap_or_else(|| "exited abnormally (no exit status)".to_string());
            let log = ev
                .log_path
                .map(|p| format!("Read the launch log {p}, fix the cause, then retry"))
                .unwrap_or_else(|| {
                    "Read the launch log under .ta/logs/wake-launches/, fix the cause, then retry"
                        .to_string()
                });
            (format!("{agent} {exit} before doing any work{quoted}"), log)
        }
        LaunchFailureCode::TimedOut => {
            let after = ev
                .timeout_secs
                .map(|s| format!(" after {s}s"))
                .unwrap_or_default();
            (
                format!("{agent} was stopped{after} without finishing"),
                "Raise wake_launch_timeout_secs in [whiteboard] of .ta/workflow.toml if launches legitimately take this long, then retry".to_string(),
            )
        }
    };
    LaunchFailure {
        code,
        detail: one_line(&detail),
        fix: one_line(&fix),
    }
}

/// Classify and describe in one step. The agent and command a `ta run` marker
/// names (`ta run` resolved them, the caller may only be guessing) are used in
/// the explanation.
pub fn classify_and_describe(ev: &LaunchEvidence<'_>) -> LaunchFailure {
    let marker = parse_marker_full(ev.stderr);
    let mut ev = *ev;
    if let Some(m) = &marker {
        if let Some(a) = m.agent.as_deref() {
            ev.agent = a;
        }
        if let Some(c) = m.command.as_deref() {
            ev.command = Some(c);
        }
    }
    describe(classify(&ev), &ev)
}

impl LaunchFailure {
    /// `Agent could not start (<code>): <detail>. <fix>`, the text the poller
    /// puts on a held task. Printed by `ta` so a person at a terminal sees the
    /// same words Wayfinder shows.
    pub fn headline(&self) -> String {
        format!(
            "Agent could not start ({}): {}. {}",
            self.code, self.detail, self.fix
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev<'a>(stderr: &'a str) -> LaunchEvidence<'a> {
        LaunchEvidence {
            agent: "claude-code",
            stderr,
            exit_code: Some(1),
            ..Default::default()
        }
    }

    #[test]
    fn codes_round_trip_and_know_which_are_permanent() {
        for c in LaunchFailureCode::ALL {
            assert_eq!(LaunchFailureCode::parse(c.as_str()), Some(c));
        }
        assert_eq!(LaunchFailureCode::parse("bogus"), None);
        let permanent: Vec<_> = LaunchFailureCode::ALL
            .into_iter()
            .filter(|c| c.is_permanent())
            .map(|c| c.as_str())
            .collect();
        assert_eq!(
            permanent,
            [
                "no_login",
                "login_invalid",
                "agent_missing",
                "sandbox_denied"
            ]
        );
    }

    #[test]
    fn one_example_per_code() {
        let cases = [
            (
                "a login stored in the macOS Keychain is not available inside the box",
                LaunchFailureCode::NoLogin,
            ),
            (
                "API Error: 401 {\"type\":\"authentication_error\"}",
                LaunchFailureCode::LoginInvalid,
            ),
            (
                "'claude' command not found. To use manually:",
                LaunchFailureCode::AgentMissing,
            ),
            (
                "Launch refused by the sandbox policy (goal x): missing filesystem",
                LaunchFailureCode::SandboxDenied,
            ),
            ("segfault", LaunchFailureCode::Crashed),
        ];
        for (stderr, want) in cases {
            assert_eq!(classify(&ev(stderr)), want, "{stderr}");
        }
        let t = LaunchEvidence {
            timed_out: true,
            ..ev("")
        };
        assert_eq!(classify(&t), LaunchFailureCode::TimedOut);
    }

    #[test]
    fn an_unreadable_prompt_file_is_the_sandbox() {
        let e = ev("Error reading append system prompt file: EPERM");
        assert_eq!(classify(&e), LaunchFailureCode::SandboxDenied);
    }

    #[test]
    fn the_login_probe_decides_a_silent_launch() {
        let silent = LaunchEvidence {
            output_lines: Some(0),
            login: Some(LoginState::Missing),
            ..ev("")
        };
        assert_eq!(classify(&silent), LaunchFailureCode::NoLogin);
        let rejected = LaunchEvidence {
            login: Some(LoginState::Invalid),
            ..silent
        };
        assert_eq!(classify(&rejected), LaunchFailureCode::LoginInvalid);
        // An agent that did work and then died is not blamed on the login.
        let worked = LaunchEvidence {
            output_lines: Some(12),
            login: Some(LoginState::Missing),
            ..ev("boom")
        };
        assert_eq!(classify(&worked), LaunchFailureCode::Crashed);
        let present = LaunchEvidence {
            login: Some(LoginState::Present),
            ..silent
        };
        assert_eq!(classify(&present), LaunchFailureCode::Crashed);
    }

    #[test]
    fn the_marker_wins_and_forged_text_cannot_inject_an_explanation() {
        let text = format!(
            "noise\n{}\nmore",
            marker_line(
                LaunchFailureCode::SandboxDenied,
                Some("codex"),
                Some("codex"),
                Some("0b6f3c5e-1111-2222-3333-444455556666")
            )
        );
        assert_eq!(classify(&ev(&text)), LaunchFailureCode::SandboxDenied);
        assert_eq!(parse_marker("ta-launch-failure: code=nonsense"), None);
        // The marker carries the code and two identifiers only: detail and fix are always rebuilt.
        let f = classify_and_describe(&ev(&text));
        assert!(!f.detail.contains("noise"), "{f:?}");
        assert!(f.detail.contains("codex"), "{f:?}");
        let forged = "ta-launch-failure: code=no_login agent=evil;rm -rf command=$(x)";
        let m = parse_marker_full(forged).unwrap();
        assert_eq!((m.agent, m.command), (None, None));
    }

    #[test]
    fn every_code_has_a_detail_and_a_fix_with_no_trailing_period() {
        for c in LaunchFailureCode::ALL {
            let f = describe(
                c,
                &LaunchEvidence {
                    agent: "claude-code",
                    command: Some("claude"),
                    exit_code: Some(2),
                    timeout_secs: Some(3600),
                    evidence_line: Some("something happened."),
                    ..Default::default()
                },
            );
            assert!(!f.detail.is_empty() && !f.fix.is_empty(), "{f:?}");
            assert!(!f.detail.ends_with('.') && !f.fix.ends_with('.'), "{f:?}");
            assert!(!f.detail.contains('\n') && !f.fix.contains('\n'), "{f:?}");
            assert!(f
                .headline()
                .starts_with(&format!("Agent could not start ({c}): ")));
        }
        let nl = describe(
            LaunchFailureCode::NoLogin,
            &LaunchEvidence {
                agent: "claude-code",
                ..Default::default()
            },
        );
        assert_eq!(nl.fix, "Run: ta agent-login login claude-code", "{nl:?}");
    }

    #[test]
    fn a_login_failure_in_a_wayfinder_project_leads_with_wayfinder_then_the_cli_line() {
        for (code, again) in [
            (LaunchFailureCode::NoLogin, ""),
            (LaunchFailureCode::LoginInvalid, " again"),
        ] {
            let f = describe(
                code,
                &LaunchEvidence {
                    agent: "claude-code",
                    wayfinder_managed: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                f.fix,
                format!(
                    "Open Wayfinder Settings, Agent login, and log in{again} on this computer. If you run TA by hand, run: ta agent-login login claude-code"
                )
            );
            // Wayfinder first, CLI second.
            assert!(
                f.fix.find("Wayfinder Settings").unwrap()
                    < f.fix.find("ta agent-login login").unwrap(),
                "{f:?}"
            );
            assert!(
                f.headline()
                    .contains("Open Wayfinder Settings, Agent login"),
                "{}",
                f.headline()
            );
            // The reason code is unchanged by where the project lives.
            let plain = describe(
                code,
                &LaunchEvidence {
                    agent: "claude-code",
                    ..Default::default()
                },
            );
            assert_eq!(plain.code, f.code);
            assert!(!plain.fix.contains("Wayfinder"), "{plain:?}");
            assert!(plain.fix.contains("ta agent-login login claude-code"));
        }
    }

    #[test]
    fn non_login_failures_do_not_change_in_a_wayfinder_project() {
        let ev = |managed| LaunchEvidence {
            agent: "claude-code",
            command: Some("claude"),
            wayfinder_managed: managed,
            ..Default::default()
        };
        for code in [
            LaunchFailureCode::AgentMissing,
            LaunchFailureCode::SandboxDenied,
            LaunchFailureCode::Crashed,
            LaunchFailureCode::TimedOut,
        ] {
            assert_eq!(
                describe(code, &ev(true)),
                describe(code, &ev(false)),
                "{code:?}"
            );
        }
    }

    #[test]
    fn codex_maps_to_the_openai_framework() {
        assert_eq!(framework_for_agent("codex"), "openai");
        assert_eq!(framework_for_agent("claude-code"), "claude-code");
    }

    #[test]
    fn the_no_changes_marker_round_trips_and_is_not_a_failure_marker() {
        let line = no_changes_marker_line("0b6a2f1e-0000-4000-8000-000000000001");
        let text = format!("noise\n{line}\nError: No changes detected\n");
        assert_eq!(
            parse_no_changes_marker(&text).as_deref(),
            Some("0b6a2f1e-0000-4000-8000-000000000001")
        );
        assert_eq!(parse_marker(&text), None, "not a launch-failure marker");
        assert_eq!(parse_no_changes_marker("nothing here"), None);
        assert_eq!(
            parse_no_changes_marker(&no_changes_marker_line("bad id; rm -rf")).as_deref(),
            Some("")
        );
    }

    #[test]
    fn a_sandbox_write_denial_is_sandbox_denied_and_names_the_path() {
        let stderr = "/bin/sh: /Users/someone/outside/notes.txt: Operation not permitted\n";
        let f = classify_and_describe(&ev(stderr));
        assert_eq!(f.code, LaunchFailureCode::SandboxDenied);
        assert!(
            f.detail.contains("/Users/someone/outside/notes.txt"),
            "{f:?}"
        );
        assert_eq!(
            denied_path("Sandbox: deny(1) file-write-create /tmp/x/y").as_deref(),
            Some("/tmp/x/y")
        );
        assert_eq!(denied_path("all fine"), None);
        assert!(sandbox_denial(
            "bwrap: Can't mkdir /x: Read-only file system"
        ));
        assert!(!sandbox_denial("agent said hello"));
    }

    /// The exact shape the Claude CLI prints when it has no login.
    const NOT_LOGGED_IN_ASSISTANT: &str = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"error":"authentication_failed"}"#;
    const NOT_LOGGED_IN_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"result":"Not logged in · Please run /login","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#;

    #[test]
    fn an_authentication_failed_stream_is_a_login_failure() {
        assert_eq!(
            stream_failure(NOT_LOGGED_IN_ASSISTANT),
            Some(StreamFailureKind::NotLoggedIn)
        );
        assert_eq!(
            stream_failure(NOT_LOGGED_IN_RESULT),
            Some(StreamFailureKind::NotLoggedIn)
        );
        let rejected = r#"{"type":"result","is_error":true,"api_error_status":401,"result":"Invalid API key · Please run /login","usage":{"input_tokens":0}}"#;
        assert_eq!(
            stream_failure(rejected),
            Some(StreamFailureKind::AuthRejected)
        );
        let text =
            format!("{{\"type\":\"system\"}}\n{NOT_LOGGED_IN_ASSISTANT}\n{NOT_LOGGED_IN_RESULT}\n");
        assert_eq!(
            stream_failure_in(&text),
            Some(StreamFailureKind::NotLoggedIn)
        );
    }

    #[test]
    fn talking_about_logins_or_failing_after_working_is_not_a_launch_failure() {
        // Free text and ordinary assistant messages never count.
        assert_eq!(stream_failure("Not logged in · Please run /login"), None);
        let talk = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"the user is not logged in, so I fixed the banner"}]}}"#;
        assert_eq!(stream_failure(talk), None);
        // An API error after the agent spent tokens is a failed run, not a launch.
        let after_work = r#"{"type":"result","is_error":true,"api_error_status":529,"result":"Overloaded","total_cost_usd":0.42,"usage":{"input_tokens":900,"output_tokens":40}}"#;
        assert_eq!(stream_failure(after_work), None);
        let ok = r#"{"type":"result","is_error":false,"result":"done","usage":{"input_tokens":5}}"#;
        assert_eq!(stream_failure(ok), None);
    }

    #[test]
    fn an_error_result_that_spent_nothing_is_a_failed_launch() {
        let overloaded = r#"{"type":"result","is_error":true,"api_error_status":529,"result":"Overloaded","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#;
        assert_eq!(
            stream_failure(overloaded),
            Some(StreamFailureKind::ErrorWithoutWork)
        );
        let mut e = ev("");
        e.stream_failure = Some(StreamFailureKind::ErrorWithoutWork);
        e.output_lines = Some(2);
        assert_eq!(classify(&e), LaunchFailureCode::Crashed);
    }

    #[test]
    fn a_stream_login_failure_classifies_as_no_login_with_the_fix() {
        let mut e = ev("");
        e.exit_code = Some(1);
        e.output_lines = Some(3);
        e.stream_failure = Some(StreamFailureKind::NotLoggedIn);
        let f = classify_and_describe(&e);
        assert_eq!(f.code, LaunchFailureCode::NoLogin);
        assert!(f.fix.contains("ta agent-login login claude-code"), "{f:?}");
        // A rejected credential is `login_invalid`, or `no_login` when none is stored.
        e.stream_failure = Some(StreamFailureKind::AuthRejected);
        assert_eq!(classify(&e), LaunchFailureCode::LoginInvalid);
        e.login = Some(LoginState::Missing);
        assert_eq!(classify(&e), LaunchFailureCode::NoLogin);
    }
}
