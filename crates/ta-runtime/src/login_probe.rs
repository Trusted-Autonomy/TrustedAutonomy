//! Is a login reachable for an agent framework? A cheap check used to explain a
//! launch that never started (`ta_goal::launch_failure`), never to block one.
//!
//! Callers use [`default_probe`] only. It reads the environment today; the
//! agent-login custody work replaces the body of `default_probe` with a probe
//! that also consults TA's credential custody. The answer is a status, never a
//! secret: nothing here returns, logs or stores a credential value.

pub use ta_goal::launch_failure::LoginState as LoginStatus;

/// Answers whether a login is reachable for an agent or framework id.
pub trait LoginProbe: Send + Sync {
    /// `framework_id` is an agent id (`claude-code`, `codex`, `gemini`, `grok`)
    /// or an agent-login catalog id (`anthropic-api-key`, `openai`, `gemini`,
    /// `xai`).
    fn status(&self, framework_id: &str) -> LoginStatus;
}

/// The environment variables that carry a credential for each framework, and
/// whether an absent variable proves there is no login. `claude-code` can also
/// be logged in through its own stored OAuth session (macOS Keychain,
/// `~/.claude`), which an environment check cannot see, so for it an absent
/// variable is `Unknown`, not `Missing`.
fn env_names(framework_id: &str) -> Option<(&'static [&'static str], bool)> {
    match framework_id {
        "claude-code" | "claude" => Some((
            &[
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ],
            false,
        )),
        "anthropic-api-key" => Some((&["ANTHROPIC_API_KEY"], true)),
        "codex" | "openai" => Some((&["OPENAI_API_KEY"], true)),
        "gemini" => Some((&["GEMINI_API_KEY", "GOOGLE_API_KEY"], true)),
        "grok" | "xai" => Some((&["XAI_API_KEY"], true)),
        _ => None,
    }
}

type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Reads the process environment (or any lookup, for tests).
pub struct EnvLoginProbe {
    lookup: EnvLookup,
}

impl EnvLoginProbe {
    /// Reads `std::env`.
    pub fn new() -> Self {
        Self::with_lookup(|name| std::env::var(name).ok())
    }

    /// Reads through `lookup` instead of the process environment.
    pub fn with_lookup(lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self {
            lookup: Box::new(lookup),
        }
    }
}

impl Default for EnvLoginProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginProbe for EnvLoginProbe {
    fn status(&self, framework_id: &str) -> LoginStatus {
        let Some((names, absent_is_missing)) = env_names(framework_id) else {
            return LoginStatus::Unknown;
        };
        let present = names
            .iter()
            .any(|n| (self.lookup)(n).is_some_and(|v| !v.trim().is_empty()));
        match (present, absent_is_missing) {
            (true, _) => LoginStatus::Present,
            (false, true) => LoginStatus::Missing,
            (false, false) => LoginStatus::Unknown,
        }
    }
}

/// The probe the daemon and `ta run` use.
pub fn default_probe() -> Box<dyn LoginProbe> {
    Box::new(EnvLoginProbe::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(vars: &'static [(&'static str, &'static str)]) -> EnvLoginProbe {
        EnvLoginProbe::with_lookup(move |n| {
            vars.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn a_set_variable_is_present_and_never_echoed() {
        let p = probe(&[("OPENAI_API_KEY", "sk-secret")]);
        assert_eq!(p.status("codex"), LoginStatus::Present);
        assert_eq!(p.status("openai"), LoginStatus::Present);
        assert_eq!(format!("{:?}", p.status("codex")), "Present");
    }

    #[test]
    fn api_key_frameworks_are_missing_without_their_variable() {
        let p = probe(&[]);
        for id in [
            "codex",
            "openai",
            "gemini",
            "grok",
            "xai",
            "anthropic-api-key",
        ] {
            assert_eq!(p.status(id), LoginStatus::Missing, "{id}");
        }
        assert_eq!(
            probe(&[("GOOGLE_API_KEY", "g")]).status("gemini"),
            LoginStatus::Present
        );
        assert_eq!(
            probe(&[("XAI_API_KEY", "  ")]).status("grok"),
            LoginStatus::Missing
        );
    }

    #[test]
    fn claude_without_a_variable_is_unknown_because_its_session_may_live_elsewhere() {
        assert_eq!(probe(&[]).status("claude-code"), LoginStatus::Unknown);
        assert_eq!(
            probe(&[("CLAUDE_CODE_OAUTH_TOKEN", "t")]).status("claude-code"),
            LoginStatus::Present
        );
        assert_eq!(probe(&[]).status("some-other-agent"), LoginStatus::Unknown);
    }
}
