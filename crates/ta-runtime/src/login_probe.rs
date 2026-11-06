//! Is a login reachable for an agent framework? A cheap check used to explain a
//! launch that never started (`ta_goal::launch_failure`), never to block one.
//!
//! Callers use [`default_probe`] only. It reads the environment and the logins
//! stored with `ta agent-login` (custody records, never a secret). The answer is a status, never a
//! secret: nothing here returns, logs or stores a credential value.

use ta_credentials::agent_login::{Catalog, LoginStore, State};
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

/// Checks the environment first (an environment variable always wins at
/// launch), then the logins stored with `ta agent-login` in TA's credential
/// custody. It reads only the secret-free state records, never a secret and never
/// the OS keychain, so it is safe in the daemon and never prompts.
///
/// A stored login that the vendor rejected on its last check is `Invalid`.
/// With nothing stored the answer is the environment probe's: `Missing` where an
/// absent variable proves there is no login, `Unknown` for `claude-code`, whose
/// own session (macOS Keychain, `~/.claude`) this probe cannot see.
pub struct CustodyLoginProbe {
    env: EnvLoginProbe,
    store: LoginStore,
    catalog: Catalog,
}

impl CustodyLoginProbe {
    /// An explicit environment probe, store and catalog (tests, embedding).
    pub fn with_parts(env: EnvLoginProbe, store: LoginStore, catalog: Catalog) -> Self {
        Self {
            env,
            store,
            catalog,
        }
    }
}

impl LoginProbe for CustodyLoginProbe {
    fn status(&self, framework_id: &str) -> LoginStatus {
        let from_env = self.env.status(framework_id);
        if from_env == LoginStatus::Present {
            return LoginStatus::Present;
        }
        // `claude` is an accepted alias of the `claude-code` agent.
        let id = if framework_id == "claude" {
            "claude-code"
        } else {
            framework_id
        };
        let mut frameworks = self.catalog.serving(id);
        if let Ok(fw) = self.catalog.get(id) {
            if !frameworks.iter().any(|f| f.id == fw.id) {
                frameworks.push(fw);
            }
        }
        let mut any_invalid = false;
        for fw in frameworks {
            if !self.store.is_stored(fw) {
                continue;
            }
            match self.store.record(&fw.id).map(|r| r.state) {
                Some(State::Invalid) => any_invalid = true,
                _ => return LoginStatus::Present,
            }
        }
        if any_invalid {
            LoginStatus::Invalid
        } else {
            from_env
        }
    }
}

/// The probe the daemon and `ta run` use: the environment, then TA's stored
/// agent logins. If the catalog cannot be read (a broken override file), it
/// answers from the environment alone rather than failing a launch report.
pub fn default_probe() -> Box<dyn LoginProbe> {
    match Catalog::load(None) {
        Ok(catalog) => Box::new(CustodyLoginProbe::with_parts(
            EnvLoginProbe::new(),
            LoginStore::open(),
            catalog,
        )),
        Err(e) => {
            tracing::warn!(error = %e, "agent framework catalog unreadable; login probe uses the environment only");
            Box::new(EnvLoginProbe::new())
        }
    }
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

#[cfg(test)]
mod custody_tests {
    use super::*;
    use ta_credentials::agent_login::{Catalog, Framework, LoginStore, Secret, State};

    fn fw<'a>(catalog: &'a Catalog, id: &str) -> &'a Framework {
        catalog.get(id).unwrap()
    }

    fn probe(
        dir: &std::path::Path,
        vars: &'static [(&'static str, &'static str)],
    ) -> CustodyLoginProbe {
        CustodyLoginProbe::with_parts(
            EnvLoginProbe::with_lookup(move |n| {
                vars.iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| v.to_string())
            }),
            LoginStore::with(dir, false),
            Catalog::shipped(),
        )
    }

    #[test]
    fn nothing_stored_answers_like_the_environment_probe() {
        let dir = tempfile::tempdir().unwrap();
        let p = probe(dir.path(), &[]);
        assert_eq!(p.status("claude-code"), LoginStatus::Unknown);
        assert_eq!(p.status("openai"), LoginStatus::Missing);
        assert_eq!(p.status("codex"), LoginStatus::Missing);
    }

    #[test]
    fn a_stored_login_is_present_for_its_agent_and_its_framework_id() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::shipped();
        let store = LoginStore::with(dir.path(), false);
        store
            .store(
                fw(&catalog, "claude-code"),
                &Secret::new("sk-ant-oat01-SENTINEL"),
                100,
            )
            .unwrap();
        store
            .store(
                fw(&catalog, "openai"),
                &Secret::new("sk-SENTINEL-openai"),
                100,
            )
            .unwrap();
        let p = probe(dir.path(), &[]);
        for id in ["claude-code", "claude", "openai", "codex"] {
            assert_eq!(p.status(id), LoginStatus::Present, "{id}");
        }
        // A framework with nothing stored is still missing.
        assert_eq!(p.status("xai"), LoginStatus::Missing);
        assert_eq!(p.status("grok"), LoginStatus::Missing);
    }

    #[test]
    fn a_stored_login_the_vendor_rejected_is_invalid_and_a_set_variable_wins() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::shipped();
        let store = LoginStore::with(dir.path(), false);
        let openai = fw(&catalog, "openai");
        store
            .store(openai, &Secret::new("sk-SENTINEL-openai"), 100)
            .unwrap();
        store
            .set_state("openai", State::Invalid, "HTTP 401", None, 101)
            .unwrap();
        assert_eq!(probe(dir.path(), &[]).status("codex"), LoginStatus::Invalid);
        assert_eq!(
            probe(dir.path(), &[("OPENAI_API_KEY", "from-env")]).status("codex"),
            LoginStatus::Present
        );
    }

    #[test]
    fn the_answer_never_contains_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::shipped();
        LoginStore::with(dir.path(), false)
            .store(
                fw(&catalog, "xai"),
                &Secret::new("xai-SENTINEL-secret"),
                100,
            )
            .unwrap();
        let shown = format!("{:?}", probe(dir.path(), &[]).status("grok"));
        assert!(!shown.contains("SENTINEL"), "{shown}");
    }
}
