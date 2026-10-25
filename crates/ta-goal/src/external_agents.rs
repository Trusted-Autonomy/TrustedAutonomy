//! Guard that keeps automated test runs away from paid external agents.
//!
//! A test run must never reach a real `claude`, `codex`, `ollama`, `meridian`
//! or a real `ta run` goal launch: each of those spends money (or, for
//! `meridian summarize-title`, spawns a real paid `claude --print`). Before
//! this guard existed, `cargo test -p ta-cli` ran `ta draft apply` in-process
//! dozens of times, and every run shelled out to the installed `meridian`,
//! which spawned a real model call -- thousands of paid "Test git commit"
//! sessions.
//!
//! ## The two switches
//!
//! * [`NO_EXTERNAL_AGENTS_ENV`] (`TA_TEST_NO_EXTERNAL_AGENTS`): set to `1` for
//!   every `cargo test` / `cargo run` in this workspace by `.cargo/config.toml`
//!   `[env]`. Child processes (a test that spawns the `ta` binary, or a
//!   gateway test that would shell out to `ta run`) inherit it. Set it to `0`
//!   in your shell to opt out (e.g. when deliberately exercising a real
//!   agent): the config entry is `force = false`, so the shell value wins.
//! * `cfg!(test)`: the [`external_agents_blocked!`] macro also reports blocked
//!   whenever the *calling* crate is compiled with `cfg(test)`, so unit tests
//!   are safe even if the environment variable is somehow absent.
//!
//! Production behaviour is unchanged: neither switch is set for an installed
//! `ta`, `ta-daemon`, or a normal `cargo build`.
//!
//! ## Using it
//!
//! At every site that would spawn a paid external agent, check
//! `ta_goal::external_agents_blocked!()` and refuse with
//! [`blocked_message`]. Never gate on a mock: the guard exists so the real
//! spawn is unreachable from tests.

/// Environment variable that blocks every paid external-agent spawn.
pub const NO_EXTERNAL_AGENTS_ENV: &str = "TA_TEST_NO_EXTERNAL_AGENTS";

/// Whether [`NO_EXTERNAL_AGENTS_ENV`] is set to an "on" value. Unset, empty,
/// `0` and `false` (any case) mean "not blocked".
pub fn env_blocks_external_agents() -> bool {
    match std::env::var(NO_EXTERNAL_AGENTS_ENV) {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
        }
        Err(_) => false,
    }
}

/// True when paid external agents must not be spawned: the environment guard
/// is on, or the calling crate is compiled for its own unit tests.
#[macro_export]
macro_rules! external_agents_blocked {
    () => {
        (cfg!(test) || $crate::external_agents::env_blocks_external_agents())
    };
}

/// Operator-facing explanation for a spawn that was refused by the guard.
pub fn blocked_message(what: &str) -> String {
    format!(
        "{what} was not started: external agents are blocked ({NO_EXTERNAL_AGENTS_ENV} is set, \
         or this is a test build) so automated test runs cannot spend money. \
         To run it for real, use an installed `ta` (not `cargo test`/`cargo run`), or set \
         {NO_EXTERNAL_AGENTS_ENV}=0 in your shell."
    )
}

/// Whether `command` (a bare name or a path) is a paid agent or paid
/// agent-fronting tool: Claude Code, Codex, Ollama, claude-flow or Meridian.
pub fn is_paid_agent_command(command: &str) -> bool {
    let base = std::path::Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "claude"
            | "claude-code"
            | "claude-flow"
            | "codex"
            | "ollama"
            | "ta-agent-ollama"
            | "meridian"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paid_agent_commands_are_recognised_by_name_and_by_path() {
        for c in [
            "claude",
            "/Users/x/.local/bin/claude",
            "codex",
            "ollama",
            "claude-flow",
            "meridian",
            "/opt/meridian/bin/meridian",
            "CLAUDE.exe",
        ] {
            assert!(is_paid_agent_command(c), "{c} must be treated as paid");
        }
        for c in [
            "echo",
            "sh",
            "/bin/cat",
            "ta",
            "gh",
            "git",
            "my-claude-wrapper",
        ] {
            assert!(!is_paid_agent_command(c), "{c} must not be treated as paid");
        }
    }

    #[test]
    fn macro_reports_blocked_in_unit_tests_regardless_of_environment() {
        // cfg!(test) is true for this crate's own tests, so the macro is
        // blocked even if the env var were absent.
        assert!(external_agents_blocked!());
    }

    #[test]
    fn blocked_message_names_the_guard_and_how_to_opt_out() {
        let m = blocked_message("`meridian summarize-title`");
        assert!(m.contains(NO_EXTERNAL_AGENTS_ENV));
        assert!(m.contains("=0"));
    }
}
