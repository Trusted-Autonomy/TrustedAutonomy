//! Did the agent ever start its model session?
//!
//! A launch can "succeed" at the process level (exit code 0) while the agent
//! never reached its model: the sandbox denied exec of the agent's login
//! helper, the system-prompt file was unreadable, or the agent printed one fatal
//! line and quit. Such a launch did no work and must be recorded as FAILED, with
//! a precise reason, so the caller (the daemon's wake retry, `ta goal`) can act.
//!
//! The criteria use only evidence TA already has:
//! - the agent produced zero output lines (its stream-json transcript is empty);
//! - its stderr carries a known fatal start-up message ([`classify_stderr`]).

/// Why an agent never started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFailureKind {
    /// The agent has no usable login inside the sandbox.
    NoLogin,
    /// The agent could not read the system-prompt file TA wrote for it.
    PromptUnreadable,
}

/// A known fatal start-up message found in the agent's stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartFailure {
    pub kind: StartFailureKind,
    /// The text that matched.
    pub matched: &'static str,
}

/// stderr fragments that mean the agent died before its model session. Matched
/// case-insensitively.
const KNOWN_FATAL_STDERR: &[(&str, StartFailureKind)] = &[
    (
        "a login stored in the macos keychain is not available inside the box",
        StartFailureKind::NoLogin,
    ),
    ("please run /login", StartFailureKind::NoLogin),
    (
        "error reading append system prompt file",
        StartFailureKind::PromptUnreadable,
    ),
];

/// First known fatal start-up message in `stderr`, if any.
pub fn classify_stderr(stderr: &str) -> Option<StartFailure> {
    let lower = stderr.to_ascii_lowercase();
    KNOWN_FATAL_STDERR
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(needle, kind)| StartFailure {
            kind: *kind,
            matched: needle,
        })
}

/// What the owner must do when the sandboxed agent has no usable login. The two
/// options that exist in the code: `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`
/// or `CLAUDE_CODE_OAUTH_TOKEN` in the environment of the process that starts
/// the launch (for wake launches, where `ta-daemon` starts); the sandbox
/// forwards exactly those, and a login in the macOS Keychain is never reachable
/// from inside the box.
pub const NO_LOGIN_ADVICE: &str = "The sandboxed agent has no usable login: a login stored in \
the macOS Keychain (or ~/.claude) is not reachable from inside the sandbox. Run `ta agent-login \
login claude-code` once (TA stores the login and hands it to every sandboxed launch), or set \
ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN, or CLAUDE_CODE_OAUTH_TOKEN from `claude \
setup-token`) in the environment of the process that starts the agent (for wake launches: where \
`ta-daemon` is started, then `ta daemon restart`). See \"Logging your agents in\" and \"Agent \
login inside the sandbox\" in docs/USAGE.md.";

/// The failure reason for a launch whose agent never started its model session.
/// `output_lines` is how many lines the agent wrote to stdout, when the caller
/// knows; `stderr` is its stderr when the caller has it.
pub fn never_started_reason(output_lines: Option<usize>, stderr: Option<&str>) -> String {
    let cause = stderr.and_then(classify_stderr);
    let mut s = match output_lines {
        Some(n) => format!(
            "agent never started its model session (it wrote {n} output line(s), so it did \
             no work)"
        ),
        None => "agent never started its model session (it did no work)".to_string(),
    };
    match cause {
        Some(c) => {
            s.push_str(&format!("; its stderr says \"{}\". ", c.matched));
            match c.kind {
                StartFailureKind::NoLogin => s.push_str(NO_LOGIN_ADVICE),
                StartFailureKind::PromptUnreadable => s.push_str(
                    "The sandbox denied reading the goal's system-prompt file \
                     (.ta/logs/goals/<id>/system-prompt.txt). Check the launch's sandbox \
                     profile with `ta sandbox explain`.",
                ),
            }
        }
        None => s.push_str(
            ". Likely causes: the sandbox denied something the agent needs at start-up, or the \
             agent has no usable login. ",
        ),
    }
    if cause.is_none() {
        s.push_str(NO_LOGIN_ADVICE);
        s.push_str(" Read the agent's stderr in the launch log for the exact error.");
    }
    s
}

/// Whether a finished headless launch never started: it produced no output
/// lines, or its stderr carries a known fatal start-up message.
pub fn never_started(output_lines: usize, stderr: Option<&str>) -> bool {
    output_lines == 0 || stderr.and_then(classify_stderr).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_known_fatal_messages() {
        let keychain = "a login stored in the macOS Keychain is not available inside the box";
        assert_eq!(
            classify_stderr(keychain).map(|c| c.kind),
            Some(StartFailureKind::NoLogin)
        );
        let prompt =
            "Error reading append system prompt file: EPERM .ta/logs/goals/x/system-prompt.txt";
        assert_eq!(
            classify_stderr(prompt).map(|c| c.kind),
            Some(StartFailureKind::PromptUnreadable)
        );
        assert!(classify_stderr("compiling... done").is_none());
    }

    #[test]
    fn zero_lines_or_fatal_stderr_means_never_started() {
        assert!(never_started(0, None));
        assert!(never_started(0, Some("")));
        assert!(!never_started(3, Some("warning: something")));
        assert!(never_started(
            3,
            Some("Error reading append system prompt file: EPERM")
        ));
    }

    #[test]
    fn reason_names_the_cause_and_what_to_do() {
        let r = never_started_reason(
            Some(0),
            Some("a login stored in the macOS Keychain is not available inside the box"),
        );
        assert!(r.contains("never started"), "{r}");
        assert!(r.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{r}");
        assert!(r.contains("ta-daemon"), "{r}");
        let p = never_started_reason(
            Some(0),
            Some("Error reading append system prompt file: EPERM"),
        );
        assert!(p.contains("system-prompt.txt"), "{p}");
        let u = never_started_reason(Some(0), None);
        assert!(u.contains("ANTHROPIC_API_KEY"), "{u}");
    }
}
