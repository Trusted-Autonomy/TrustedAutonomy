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
pub const LAUNCH_FAILURE_MARKER: &str = "ta-launch-failure: code=";

/// Longest `detail` or `fix` produced, in characters.
pub const MAX_TEXT_CHARS: usize = 400;

/// Why a launch failed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
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
    text.lines()
        .rev()
        .filter_map(|l| l.trim().strip_prefix(LAUNCH_FAILURE_MARKER))
        .find_map(|rest| LaunchFailureCode::parse(rest.trim()))
}

/// The marker line for `code`.
pub fn marker_line(code: LaunchFailureCode) -> String {
    format!("{LAUNCH_FAILURE_MARKER}{}", code.as_str())
}

/// Decide the code from the evidence. Order, first match wins:
/// the `ta run` marker; the sandbox refusing or denying; the agent missing; the
/// vendor rejecting the credential; a fatal "no login" message; the launch
/// being killed by its timeout; the login probe saying `Invalid` or `Missing`
/// for an agent that wrote nothing; otherwise `crashed`.
pub fn classify(ev: &LaunchEvidence<'_>) -> LaunchFailureCode {
    if let Some(code) = parse_marker(ev.stderr) {
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

/// Build the explanation for `code` from the evidence.
pub fn describe(code: LaunchFailureCode, ev: &LaunchEvidence<'_>) -> LaunchFailure {
    let agent = if ev.agent.is_empty() {
        "the agent"
    } else {
        ev.agent
    };
    let framework = framework_for_agent(ev.agent);
    let quoted = ev
        .evidence_line
        .map(one_line)
        .filter(|l| !l.is_empty())
        .map(|l| format!(" (it printed: {l})"))
        .unwrap_or_default();
    let (detail, fix) = match code {
        LaunchFailureCode::NoLogin => (
            format!("{agent} has no usable login, so it could not start"),
            format!(
                "Log in from Wayfinder Settings > Agent login, or run: ta agent-login login {framework}"
            ),
        ),
        LaunchFailureCode::LoginInvalid => (
            format!("the login for {agent} was rejected, so it could not start{quoted}"),
            format!(
                "Log in again from Wayfinder Settings > Agent login, or run: ta agent-login login {framework}"
            ),
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
        LaunchFailureCode::SandboxDenied => (
            format!("the sandbox refused to start {agent} or denied it something it needs to start{quoted}"),
            "Run: ta sandbox explain to see what was blocked, then adjust the sandbox policy (see Sandbox in docs/USAGE.md)".to_string(),
        ),
        LaunchFailureCode::Crashed => {
            let exit = ev
                .exit_code
                .map(|c| format!("exited with status {c}"))
                .unwrap_or_else(|| "was stopped by a signal".to_string());
            let log = ev
                .log_path
                .map(|p| format!("Read the launch log {p}, fix the cause, then retry"))
                .unwrap_or_else(|| {
                    "Read the launch log under .ta/logs/wake-launches/, fix the cause, then retry"
                        .to_string()
                });
            (
                format!("{agent} {exit} before doing any work{quoted}"),
                log,
            )
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

/// Classify and describe in one step.
pub fn classify_and_describe(ev: &LaunchEvidence<'_>) -> LaunchFailure {
    describe(classify(ev), ev)
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
            ["no_login", "login_invalid", "agent_missing", "sandbox_denied"]
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
        let text = format!("noise\n{}\nmore", marker_line(LaunchFailureCode::SandboxDenied));
        assert_eq!(classify(&ev(&text)), LaunchFailureCode::SandboxDenied);
        assert_eq!(parse_marker("ta-launch-failure: code=nonsense"), None);
        // The marker carries the code only: detail and fix are always rebuilt.
        let f = classify_and_describe(&ev(&text));
        assert!(!f.detail.contains("noise"), "{f:?}");
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
            assert!(f.headline().starts_with(&format!("Agent could not start ({c}): ")));
        }
        let nl = describe(
            LaunchFailureCode::NoLogin,
            &LaunchEvidence {
                agent: "claude-code",
                ..Default::default()
            },
        );
        assert!(
            nl.fix
                .contains("Wayfinder Settings > Agent login, or run: ta agent-login login claude-code"),
            "{nl:?}"
        );
    }

    #[test]
    fn codex_maps_to_the_openai_framework() {
        assert_eq!(framework_for_agent("codex"), "openai");
        assert_eq!(framework_for_agent("claude-code"), "claude-code");
    }
}
