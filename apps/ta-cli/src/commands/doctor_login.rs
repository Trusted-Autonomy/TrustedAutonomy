// doctor_login.rs — `ta doctor`: a running `ta-daemon` that was started without
// any agent login in its environment.
//
// A sandboxed agent cannot use a login stored in the macOS Keychain or in
// `~/.claude`: the only logins it can use are the ones the sandbox forwards from
// the environment of the process that starts the launch (`ANTHROPIC_API_KEY`,
// `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`). For goals the daemon starts
// on its own (wake launches) that process is `ta-daemon`, so a daemon started
// from a shell without one of them fails every such goal with "The sandboxed
// agent has no usable login". This check reads the daemon's environment and says
// so before a wave is dispatched.
//
// Only the NAMES of the login variables are ever examined or printed; their
// values are not kept, logged or shown.

use ta_mcp_gateway::GatewayConfig;

use super::doctor::CheckResult;

const CHECK_NAME: &str = "Daemon agent login";

/// The environment variables the sandbox forwards to a Claude Code agent as its login.
pub const LOGIN_VARS: [&str; 3] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

/// Variables every normal login environment has. Seeing one of them proves the
/// environment text was really the process environment, so "no login variable in
/// it" can be trusted instead of meaning "the read returned nothing".
const BASELINE_VARS: [&str; 7] = ["PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG"];

/// What was found in one process's environment. Holds names only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvScan {
    /// Login variables that are set to a non-empty value.
    pub logins: Vec<&'static str>,
    /// The text looked like a real process environment (a baseline variable was in it).
    pub saw_environment: bool,
}

/// Scan environment text for login variables. Entries are separated by NUL
/// (`/proc/<pid>/environ`) or whitespace (`ps -E` output, which appends the
/// environment to the command line); a value containing spaces only splits the
/// value, never the `NAME=` that starts an entry.
pub fn scan_environment_text(text: &str) -> EnvScan {
    let mut scan = EnvScan::default();
    for token in text.split(|c: char| c == '\0' || c.is_whitespace()) {
        let Some((name, value)) = token.split_once('=') else {
            continue;
        };
        if BASELINE_VARS.contains(&name) {
            scan.saw_environment = true;
        }
        if let Some(var) = LOGIN_VARS.iter().find(|v| **v == name) {
            if !value.is_empty() && !scan.logins.contains(var) {
                scan.logins.push(var);
            }
        }
    }
    scan
}

/// Read the environment of process `pid` and scan it. `Err` is why it could not
/// be read (another user's process, an OS without a way to read it, a failed
/// helper); it never contains environment text.
pub fn read_process_environment(pid: u32) -> Result<EnvScan, String> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/proc/{pid}/environ");
        let bytes = std::fs::read(&path).map_err(|e| format!("could not read {path}: {e}"))?;
        Ok(scan_environment_text(&String::from_utf8_lossy(&bytes)))
    }
    #[cfg(target_os = "macos")]
    {
        // `ps -E` shows the environment of the user's own processes; for any
        // other process it prints the command line only (so no baseline variable
        // is seen and the caller reports "cannot check").
        let out = std::process::Command::new("/bin/ps")
            .args(["-E", "-ww", "-p", &pid.to_string(), "-o", "command="])
            .output()
            .map_err(|e| format!("could not run /bin/ps -E -p {pid}: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "/bin/ps -E -p {pid} failed ({}): {}",
                out.status,
                stderr.lines().next().unwrap_or("no message")
            ));
        }
        Ok(scan_environment_text(&String::from_utf8_lossy(&out.stdout)))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(format!(
            "reading another process's environment (pid {pid}) is not supported on this OS"
        ))
    }
}

/// The doctor line for the running daemon's agent login, or `None` when the
/// question does not apply (no running daemon, a non-Anthropic agent, no
/// sandbox).
pub fn check(config: &GatewayConfig, agent_name: &str) -> Option<CheckResult> {
    let pid = super::daemon::read_pid(&config.workspace_root);
    daemon_login_result(
        agent_name,
        || super::sandbox_launch::chat_launch_is_sandboxed(&config.workspace_root),
        pid,
        &super::doctor::is_pid_alive,
        &read_process_environment,
    )
}

/// [`check`] with every outside fact injected, so tests use fake readers and
/// never touch a real process.
///
/// `sandboxed` is called only after a live daemon is found.
pub(crate) fn daemon_login_result(
    agent_name: &str,
    sandboxed: impl FnOnce() -> Result<bool, String>,
    daemon_pid: Option<u32>,
    pid_alive: &dyn Fn(u32) -> bool,
    read_env: &dyn Fn(u32) -> Result<EnvScan, String>,
) -> Option<CheckResult> {
    // Only the Anthropic agent takes these variables as its login.
    if agent_name != "claude-code" {
        return None;
    }
    let pid = daemon_pid.filter(|p| pid_alive(*p))?;
    match sandboxed() {
        Ok(true) => {}
        Ok(false) => return None,
        Err(why) => {
            return Some(CheckResult::ok(
                CHECK_NAME,
                format!(
                    "cannot check: could not tell whether agents in this project run in the \
                     sandbox ({why}); fix that first (see `ta sandbox explain`)"
                ),
            ))
        }
    }
    let scan = match read_env(pid) {
        Ok(scan) => scan,
        Err(why) => {
            return Some(CheckResult::ok(
                CHECK_NAME,
                format!(
                    "cannot check: the environment of ta-daemon (pid {pid}) is not readable: {why}"
                ),
            ))
        }
    };
    if !scan.logins.is_empty() {
        return Some(CheckResult::ok(
            CHECK_NAME,
            format!(
                "ta-daemon (pid {pid}) has {} in its environment (value not read)",
                scan.logins.join(", ")
            ),
        ));
    }
    if !scan.saw_environment {
        return Some(CheckResult::ok(
            CHECK_NAME,
            format!(
                "cannot check: the environment of ta-daemon (pid {pid}) came back empty \
                 (it may belong to another user)"
            ),
        ));
    }
    Some(CheckResult::warn(
        CHECK_NAME,
        format!(
            "ta-daemon (pid {pid}) was started without an agent login: none of {} is set in its \
             environment, and agents in this project run in the sandbox, which cannot use a login \
             stored in the macOS Keychain or ~/.claude. Goals the daemon starts (wake launches) \
             will fail with \"The sandboxed agent has no usable login\".",
            LOGIN_VARS.join(", ")
        ),
        "Set ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN, or CLAUDE_CODE_OAUTH_TOKEN from \
         `claude setup-token`) in the environment of whatever starts ta-daemon (the shell you run \
         `ta daemon start` or `ta daemon restart` from, or the service definition), then run \
         `ta daemon restart`. See \"Agent login inside the sandbox\" in docs/USAGE.md.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::CheckStatus;

    fn scan(logins: &[&'static str], saw: bool) -> Result<EnvScan, String> {
        Ok(EnvScan {
            logins: logins.to_vec(),
            saw_environment: saw,
        })
    }

    fn run(
        agent: &str,
        sandboxed: Result<bool, String>,
        pid: Option<u32>,
        alive: bool,
        env: Result<EnvScan, String>,
    ) -> Option<CheckResult> {
        daemon_login_result(agent, || sandboxed, pid, &|_| alive, &|_| env.clone())
    }

    #[test]
    fn scan_finds_login_names_in_nul_separated_environ() {
        let text = "PATH=/usr/bin\0HOME=/Users/x\0CLAUDE_CODE_OAUTH_TOKEN=sk-ant-secret\0";
        let s = scan_environment_text(text);
        assert_eq!(s.logins, vec!["CLAUDE_CODE_OAUTH_TOKEN"]);
        assert!(s.saw_environment);
    }

    #[test]
    fn scan_finds_login_names_in_ps_style_text() {
        let text = "/usr/local/bin/ta-daemon --project-root /p TERM=xterm PATH=/usr/bin ANTHROPIC_API_KEY=abc HOME=/h";
        let s = scan_environment_text(text);
        assert_eq!(s.logins, vec!["ANTHROPIC_API_KEY"]);
        assert!(s.saw_environment);
    }

    #[test]
    fn scan_ignores_empty_values_and_lookalike_names() {
        let text =
            "PATH=/bin ANTHROPIC_API_KEY= MY_ANTHROPIC_API_KEY=x ANTHROPIC_AUTH_TOKEN_FILE=y";
        let s = scan_environment_text(text);
        assert!(s.logins.is_empty(), "{s:?}");
        assert!(s.saw_environment);
    }

    #[test]
    fn scan_without_a_baseline_variable_is_not_a_real_environment() {
        let s = scan_environment_text("/usr/local/bin/ta-daemon --project-root /p");
        assert!(!s.saw_environment);
        assert!(s.logins.is_empty());
    }

    #[test]
    fn scan_deduplicates_and_keeps_several_logins() {
        let text = "HOME=/h ANTHROPIC_API_KEY=a ANTHROPIC_API_KEY=a CLAUDE_CODE_OAUTH_TOKEN=b";
        let s = scan_environment_text(text);
        assert_eq!(
            s.logins,
            vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
        );
    }

    #[test]
    fn warns_when_a_sandboxed_daemon_has_no_login() {
        let r = run("claude-code", Ok(true), Some(4242), true, scan(&[], true)).unwrap();
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.detail.contains("pid 4242"), "{}", r.detail);
        assert!(r.detail.contains("no usable login"), "{}", r.detail);
        for v in LOGIN_VARS {
            assert!(r.detail.contains(v), "{}", r.detail);
        }
        assert!(r.fix.contains("ta daemon restart"), "{}", r.fix);
        assert!(r.fix.contains("claude setup-token"), "{}", r.fix);
        assert!(
            r.fix.contains("Agent login inside the sandbox"),
            "{}",
            r.fix
        );
    }

    #[test]
    fn ok_and_names_only_when_the_daemon_has_a_login() {
        let r = run(
            "claude-code",
            Ok(true),
            Some(4242),
            true,
            scan(&["ANTHROPIC_API_KEY"], true),
        )
        .unwrap();
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.detail.contains("ANTHROPIC_API_KEY"), "{}", r.detail);
    }

    #[test]
    fn never_prints_a_value_even_when_the_text_has_one() {
        let s = scan_environment_text("PATH=/bin CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-SECRET");
        let r = run("claude-code", Ok(true), Some(1), true, Ok(s)).unwrap();
        let shown = format!("{}{}", r.detail, r.fix);
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(!shown.contains("sk-ant"), "{shown}");
    }

    #[test]
    fn cannot_check_is_not_a_warning_when_the_environment_is_unreadable() {
        let r = run(
            "claude-code",
            Ok(true),
            Some(7),
            true,
            Err("permission denied".to_string()),
        )
        .unwrap();
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.detail.starts_with("cannot check"), "{}", r.detail);
        assert!(r.detail.contains("permission denied"), "{}", r.detail);
    }

    #[test]
    fn cannot_check_when_the_read_returned_no_environment() {
        let r = run("claude-code", Ok(true), Some(7), true, scan(&[], false)).unwrap();
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.detail.starts_with("cannot check"), "{}", r.detail);
    }

    #[test]
    fn cannot_check_when_the_sandbox_question_has_no_answer() {
        let r = run(
            "claude-code",
            Err("policy.toml is unreadable".to_string()),
            Some(7),
            true,
            scan(&[], true),
        )
        .unwrap();
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(
            r.detail.contains("policy.toml is unreadable"),
            "{}",
            r.detail
        );
    }

    #[test]
    fn silent_when_there_is_nothing_to_check() {
        let none = scan(&[], true);
        // No pid file, a dead pid, no sandbox, another agent: no line at all.
        assert!(run("claude-code", Ok(true), None, true, none.clone()).is_none());
        assert!(run("claude-code", Ok(true), Some(9), false, none.clone()).is_none());
        assert!(run("claude-code", Ok(false), Some(9), true, none.clone()).is_none());
        assert!(run("codex", Ok(true), Some(9), true, none).is_none());
    }

    #[test]
    fn the_environment_is_not_read_without_a_live_daemon_or_sandbox() {
        let read = std::cell::Cell::new(0);
        let count = |_: u32| {
            read.set(read.get() + 1);
            scan(&[], true)
        };
        let _ = daemon_login_result("claude-code", || Ok(true), Some(1), &|_| false, &count);
        let _ = daemon_login_result("claude-code", || Ok(false), Some(1), &|_| true, &count);
        assert_eq!(read.get(), 0);
    }

    #[test]
    fn reads_this_process_environment_names_only() {
        // The real reader, against our own process: it must either report the
        // environment or say it cannot; it must never panic.
        match read_process_environment(std::process::id()) {
            Ok(s) => assert!(s.saw_environment, "own environment has PATH or HOME"),
            Err(why) => assert!(!why.is_empty()),
        }
    }
}
