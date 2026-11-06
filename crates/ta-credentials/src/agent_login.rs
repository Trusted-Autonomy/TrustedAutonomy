// agent_login.rs: the logins a person gives their agent frameworks (Claude,
// OpenAI, Gemini, Grok, ...) on this computer, and where TA keeps them.
//
// Three parts:
//
//   * the CATALOG: which frameworks exist, how each one logs in, which
//     environment variables carry its login to an agent, how to verify it. It
//     is data (`agent-frameworks.default.toml`, overridden per user and per
//     project); nothing here branches on a framework id.
//   * the STORE: the secret itself, in the same custody as the human
//     credential (`human.rs`): the OS keychain where the person's custody
//     choice allows it, otherwise a mode-0600 file under the per-user state
//     directory (`$TA_USER_STATE_DIR`, else `~/.config/ta/human`, the directory
//     that also holds the `custody` file). Beside each secret sits a small
//     non-secret record (state, when it was checked, where it is held) so that
//     `ta agent-login list`, `ta doctor` and the daemon can answer "is there a
//     login" without ever reading the secret.
//   * LAUNCH INJECTION: for a sandboxed agent launch, the stored login for the
//     launching agent is turned into at most one `NAME=value` for that child,
//     unless the environment already has a login for it.
//
// A secret is only ever returned as a [`Secret`] (redacted `Debug`), is never
// part of an error message, and is never logged here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::human::{self, Custody};
use crate::keychain::{self, AGENT_LOGIN_SERVICE};

/// Shipped catalog.
const SHIPPED_CATALOG: &str = include_str!("agent-frameworks.default.toml");
/// File name of the user (`~/.config/ta/`) and project (`.ta/`) override files.
pub const OVERRIDE_FILE_NAME: &str = "agent-frameworks.toml";
/// Sub-directory of the per-user state directory that holds the logins.
const DIRNAME: &str = "agent-logins";
/// Default time a vendor login command may take.
pub const DEFAULT_LOGIN_TIMEOUT_SECS: u64 = 300;
/// Environment variable that overrides `timeout_secs` of every oauth_cli login.
pub const LOGIN_TIMEOUT_ENV: &str = "TA_AGENT_LOGIN_TIMEOUT_SECS";

// ── Errors ─────────────────────────────────────────────────────────────────

/// Every message says what was attempted, what happened and what to do. None
/// ever contains a secret.
#[derive(Debug, thiserror::Error)]
pub enum AgentLoginError {
    #[error("the agent framework catalog {origin} is not usable: {detail}. Fix or delete that file (the shipped catalog applies without it); see the comments in agent-frameworks.default.toml for the fields.")]
    Catalog { origin: String, detail: String },
    #[error("unknown agent framework '{id}'. Known frameworks: {known}. Add your own under [[framework]] in ~/.config/ta/agent-frameworks.toml.")]
    UnknownFramework { id: String, known: String },
    #[error("could not store the {id} login under {path}: {reason}. Check that the directory is writable, or set TA_USER_STATE_DIR to a directory you own.")]
    Store {
        id: String,
        path: PathBuf,
        reason: String,
    },
    #[error("the {id} login is held in the OS keychain but it could not be read: {reason}")]
    Keychain { id: String, reason: String },
}

// ── Catalog ────────────────────────────────────────────────────────────────

/// What a framework's login is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A login minted by a vendor command.
    Token,
    /// A key the person copies from a vendor console.
    ApiKey,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Token => "token",
            Kind::ApiKey => "api_key",
        }
    }
}

/// How the login is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    /// TA runs the vendor command in a pseudo-terminal and captures its token.
    OauthCli,
    /// The caller asks the person for the secret and sends it on stdin.
    Prompt,
}

impl LoginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginMethod::OauthCli => "oauth_cli",
            LoginMethod::Prompt => "prompt",
        }
    }
}

fn default_timeout() -> u64 {
    DEFAULT_LOGIN_TIMEOUT_SECS
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginSpec {
    pub method: LoginMethod,
    /// The vendor command and its arguments (oauth_cli only).
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMethod {
    HttpGet,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyAuth {
    Bearer,
    Header,
}

fn default_invalid_status() -> Vec<u16> {
    vec![401]
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    pub method: VerifyMethod,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_auth")]
    pub auth: VerifyAuth,
    /// Header that carries the secret when `auth = "header"`.
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_invalid_status")]
    pub invalid_status: Vec<u16>,
}

fn default_auth() -> VerifyAuth {
    VerifyAuth::Bearer
}

impl Default for VerifySpec {
    fn default() -> Self {
        VerifySpec {
            method: VerifyMethod::None,
            url: String::new(),
            auth: VerifyAuth::Bearer,
            header: String::new(),
            headers: BTreeMap::new(),
            invalid_status: default_invalid_status(),
        }
    }
}

/// One agent framework a person can log in to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Framework {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub how: String,
    /// `ta run --agent` names this login serves.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Variables the agent reads its login from; the first receives the secret.
    pub env: Vec<String>,
    pub login: LoginSpec,
    #[serde(default)]
    pub verify: VerifySpec,
}

impl Framework {
    /// The variable the stored secret is injected as.
    pub fn primary_env(&self) -> &str {
        &self.env[0]
    }

    /// Seconds the vendor login command may run: `$TA_AGENT_LOGIN_TIMEOUT_SECS`
    /// when it holds a positive number, else the catalog's `timeout_secs`.
    pub fn login_timeout_secs(&self) -> u64 {
        std::env::var(LOGIN_TIMEOUT_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(self.login.timeout_secs)
    }

    fn validate(&self, origin: &str) -> Result<(), AgentLoginError> {
        let bad = |detail: String| AgentLoginError::Catalog {
            origin: origin.to_string(),
            detail: format!("framework '{}': {detail}", self.id),
        };
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 48
            && self
                .id
                .starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !id_ok {
            return Err(bad(
                "id must be 1-48 characters of a-z, 0-9 and '-' (it names a stored file)"
                    .to_string(),
            ));
        }
        if self.label.trim().is_empty() {
            return Err(bad("label is empty".to_string()));
        }
        if self.env.is_empty() {
            return Err(bad(
                "env lists no variable, so there is nothing to inject".to_string()
            ));
        }
        for var in &self.env {
            let ok = !var.is_empty()
                && var.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok || var.starts_with("TA_") {
                return Err(bad(format!(
                    "env variable {var:?} is not a plain variable name (letters, digits, '_'; \
                     TA_* is reserved for TA's own control channel)"
                )));
            }
        }
        if self.login.method == LoginMethod::OauthCli
            && self.login.command.first().is_none_or(|c| c.is_empty())
        {
            return Err(bad(
                "login.method = \"oauth_cli\" needs login.command (the vendor command)".to_string(),
            ));
        }
        if self.login.timeout_secs == 0 {
            return Err(bad("login.timeout_secs must be positive".to_string()));
        }
        if self.verify.method == VerifyMethod::HttpGet {
            if self.verify.url.is_empty() {
                return Err(bad(
                    "verify.method = \"http_get\" needs verify.url".to_string()
                ));
            }
            if self.verify.auth == VerifyAuth::Header && self.verify.header.is_empty() {
                return Err(bad(
                    "verify.auth = \"header\" needs verify.header (the header's name)".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    #[serde(default)]
    framework: Vec<Framework>,
}

impl CatalogFile {
    fn parse(text: &str, origin: &str) -> Result<Self, AgentLoginError> {
        let file: CatalogFile = toml::from_str(text).map_err(|e| AgentLoginError::Catalog {
            origin: origin.to_string(),
            detail: e.to_string(),
        })?;
        let mut seen = std::collections::BTreeSet::new();
        for fw in &file.framework {
            fw.validate(origin)?;
            if !seen.insert(fw.id.clone()) {
                return Err(AgentLoginError::Catalog {
                    origin: origin.to_string(),
                    detail: format!("framework '{}' is listed twice", fw.id),
                });
            }
        }
        Ok(file)
    }

    fn load(path: &Path) -> Result<Option<Self>, AgentLoginError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AgentLoginError::Catalog {
                origin: path.display().to_string(),
                detail: format!("could not read it: {e}"),
            }),
        }
    }
}

/// The frameworks visible to this process: shipped, then user, then project.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub frameworks: Vec<Framework>,
    /// Override files that were applied, for `ta agent-login list --json` diagnostics.
    pub applied_overrides: Vec<PathBuf>,
}

impl Catalog {
    /// Only the shipped catalog.
    pub fn shipped() -> Self {
        let file = CatalogFile::parse(SHIPPED_CATALOG, "shipped agent-frameworks.default.toml")
            .expect("the shipped agent framework catalog must parse (covered by tests)");
        Catalog {
            frameworks: file.framework,
            applied_overrides: Vec::new(),
        }
    }

    /// Shipped + `~/.config/ta/agent-frameworks.toml` + `<project>/.ta/agent-frameworks.toml`.
    pub fn load(project_root: Option<&Path>) -> Result<Self, AgentLoginError> {
        let user = dirs::home_dir().map(|h| h.join(".config").join("ta").join(OVERRIDE_FILE_NAME));
        let project = project_root.map(|p| p.join(".ta").join(OVERRIDE_FILE_NAME));
        Self::load_from(user.as_deref(), project.as_deref())
    }

    /// [`load`](Self::load) with explicit paths (tests, embedding).
    pub fn load_from(user: Option<&Path>, project: Option<&Path>) -> Result<Self, AgentLoginError> {
        let mut catalog = Self::shipped();
        for path in [user, project].into_iter().flatten() {
            if let Some(file) = CatalogFile::load(path)? {
                for fw in file.framework {
                    match catalog.frameworks.iter_mut().find(|f| f.id == fw.id) {
                        Some(slot) => *slot = fw,
                        None => catalog.frameworks.push(fw),
                    }
                }
                catalog.applied_overrides.push(path.to_path_buf());
            }
        }
        Ok(catalog)
    }

    pub fn get(&self, id: &str) -> Result<&Framework, AgentLoginError> {
        self.frameworks.iter().find(|f| f.id == id).ok_or_else(|| {
            AgentLoginError::UnknownFramework {
                id: id.to_string(),
                known: self
                    .frameworks
                    .iter()
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        })
    }

    /// Frameworks whose login serves `agent` (`ta run --agent <agent>`), catalog order.
    pub fn serving(&self, agent: &str) -> Vec<&Framework> {
        self.frameworks
            .iter()
            .filter(|f| f.agents.iter().any(|a| a == agent))
            .collect()
    }
}

// ── State ──────────────────────────────────────────────────────────────────

/// The state of one framework's login, as the poller and Wayfinder show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Missing,
    Invalid,
    Unknown,
    LoggingIn,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Missing => "missing",
            State::Invalid => "invalid",
            State::Unknown => "unknown",
            State::LoggingIn => "logging_in",
        }
    }
}

/// A secret value. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The non-secret record kept beside a stored login (`<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// A secret is held (in `custody`).
    pub stored: bool,
    /// `keychain` or `file`, when stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody: Option<String>,
    pub state: State,
    /// Unix seconds of the last check or state change.
    pub checked_at: i64,
    #[serde(default)]
    pub detail: String,
    /// What the last verify call checked: `stored` or `env:<VAR>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Where a login was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// This environment variable is set (its value is never read into a report).
    Env(String),
    /// TA's custody; the string says where.
    Stored(String),
    None,
}

/// What `ta agent-login list` reports for one framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkStatus {
    pub id: String,
    pub state: State,
    pub checked_at: i64,
    pub detail: String,
    pub source: Source,
}

/// What a stored secret ended up in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAt {
    pub custody: Custody,
    /// Set when the keychain was wanted but unusable (the reason).
    pub keychain_fallback_reason: Option<String>,
}

/// The per-user store of agent logins.
#[derive(Debug, Clone)]
pub struct LoginStore {
    dir: PathBuf,
    use_keychain: bool,
}

impl LoginStore {
    /// The store of this process's user: the per-user state directory and the
    /// effective custody decision (`TA_NO_KEYCHAIN`, the `custody` file).
    pub fn open() -> Self {
        LoginStore {
            dir: human::user_state_dir(),
            use_keychain: crate::custody::use_keychain(),
        }
    }

    /// Explicit directory and custody (tests, embedding).
    pub fn with(dir: impl Into<PathBuf>, use_keychain: bool) -> Self {
        LoginStore {
            dir: dir.into(),
            use_keychain,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.join(DIRNAME)
    }
    fn secret_path(&self, id: &str) -> PathBuf {
        self.root().join(format!("{id}.secret"))
    }
    fn record_path(&self, id: &str) -> PathBuf {
        self.root().join(format!("{id}.json"))
    }

    /// Directory holding the logins (for messages).
    pub fn describe_location(&self, fw: &Framework) -> String {
        if self.use_keychain {
            format!(
                "the OS keychain (service {AGENT_LOGIN_SERVICE}, item {}), falling back to {}",
                fw.id,
                self.secret_path(&fw.id).display()
            )
        } else {
            format!(
                "{} (owner-only permissions)",
                self.secret_path(&fw.id).display()
            )
        }
    }

    pub fn uses_keychain(&self) -> bool {
        self.use_keychain
    }

    /// The non-secret record, `None` when absent or unreadable.
    pub fn record(&self, id: &str) -> Option<Record> {
        let text = std::fs::read_to_string(self.record_path(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write_record(&self, id: &str, record: &Record) -> Result<(), AgentLoginError> {
        let path = self.record_path(id);
        let err = |reason: String| AgentLoginError::Store {
            id: id.to_string(),
            path: path.clone(),
            reason,
        };
        if let Some(parent) = path.parent() {
            human::create_private_dirs(parent).map_err(|e| err(e.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| err(e.to_string()))?;
        crate::state_dir::write_atomic_private(&path, &bytes).map_err(|e| err(e.to_string()))
    }

    /// Record a state with no stored-ness change (login in progress, verify result).
    pub fn set_state(
        &self,
        id: &str,
        state: State,
        detail: &str,
        source: Option<String>,
        now: i64,
    ) -> Result<(), AgentLoginError> {
        let prior = self.record(id);
        self.write_record(
            id,
            &Record {
                stored: prior.as_ref().is_some_and(|r| r.stored),
                custody: prior.and_then(|r| r.custody),
                state,
                checked_at: now,
                detail: detail.to_string(),
                source,
            },
        )
    }

    /// Whether a login is held for `fw`. For file custody the file is checked; for
    /// keychain custody the record is trusted (checking would read the keychain,
    /// and `list`, `doctor` and the daemon must never prompt).
    pub fn is_stored(&self, fw: &Framework) -> bool {
        if self.secret_path(&fw.id).is_file() {
            return true;
        }
        self.record(&fw.id)
            .is_some_and(|r| r.stored && r.custody.as_deref() == Some("keychain"))
    }

    /// Store `secret` for `fw`, replacing any earlier one, and record it as
    /// `unknown` (not yet verified).
    pub fn store(
        &self,
        fw: &Framework,
        secret: &Secret,
        now: i64,
    ) -> Result<StoredAt, AgentLoginError> {
        let path = self.secret_path(&fw.id);
        let mut custody = None;
        let mut reason = None;
        if self.use_keychain {
            match keychain::set(AGENT_LOGIN_SERVICE, &fw.id, secret.expose()) {
                Ok(()) => {
                    custody = Some(Custody::Keychain);
                    // A stale fallback file must not outlive the keychain item.
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => reason = Some(e),
            }
        }
        let custody = match custody {
            Some(c) => c,
            None => {
                crate::state_dir::write_atomic_private(&path, secret.expose().as_bytes()).map_err(
                    |e| AgentLoginError::Store {
                        id: fw.id.clone(),
                        path: path.clone(),
                        reason: e.to_string(),
                    },
                )?;
                Custody::File(path)
            }
        };
        self.write_record(
            &fw.id,
            &Record {
                stored: true,
                custody: Some(
                    match custody {
                        Custody::Keychain => "keychain",
                        Custody::File(_) => "file",
                    }
                    .to_string(),
                ),
                state: State::Unknown,
                checked_at: now,
                detail: "stored; not verified yet".to_string(),
                source: Some("stored".to_string()),
            },
        )?;
        Ok(StoredAt {
            custody,
            keychain_fallback_reason: reason,
        })
    }

    /// Read the stored secret. `Ok(None)`: nothing is stored (or it is held in a
    /// keychain this process is not allowed to use).
    pub fn load(&self, fw: &Framework) -> Result<Option<Secret>, AgentLoginError> {
        let mut keychain_error = None;
        if self.use_keychain {
            match keychain::get(AGENT_LOGIN_SERVICE, &fw.id) {
                Ok(Some(v)) if !v.trim().is_empty() => {
                    return Ok(Some(Secret(v.trim().to_string())))
                }
                Ok(_) => {}
                Err(e) => keychain_error = Some(e),
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.secret_path(&fw.id)) {
            let text = text.trim();
            if !text.is_empty() {
                return Ok(Some(Secret(text.to_string())));
            }
        }
        match keychain_error {
            Some(reason) if self.is_stored(fw) => Err(AgentLoginError::Keychain {
                id: fw.id.clone(),
                reason,
            }),
            _ => Ok(None),
        }
    }

    /// Remove the stored secret (keychain item and file) and record `missing`.
    /// Returns whether anything was removed.
    pub fn remove(&self, fw: &Framework, now: i64) -> Result<bool, AgentLoginError> {
        let mut removed = std::fs::remove_file(self.secret_path(&fw.id)).is_ok();
        let mut keychain_error = None;
        let held_in_keychain = self
            .record(&fw.id)
            .is_some_and(|r| r.custody.as_deref() == Some("keychain"));
        if self.use_keychain || held_in_keychain {
            match keychain::delete(AGENT_LOGIN_SERVICE, &fw.id) {
                Ok(r) => removed |= r,
                Err(e) => keychain_error = Some(e),
            }
        }
        if let Some(reason) = keychain_error {
            // Keep the record: the keychain item may still exist, and claiming
            // `missing` would be a lie.
            return Err(AgentLoginError::Keychain {
                id: fw.id.clone(),
                reason: format!(
                    "{reason}. The login was NOT fully removed; fix the keychain access and run \
                     `ta agent-login logout {}` again",
                    fw.id
                ),
            });
        }
        self.write_record(
            &fw.id,
            &Record {
                stored: false,
                custody: None,
                state: State::Missing,
                checked_at: now,
                detail: if removed {
                    "logged out on this computer".to_string()
                } else {
                    "no login stored".to_string()
                },
                source: None,
            },
        )?;
        Ok(removed)
    }

    /// The login `ta run` would use for `fw`: the environment variable when one is
    /// set, else the stored secret. Returns where it came from and the secret.
    pub fn effective(
        &self,
        fw: &Framework,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<(Source, Secret)>, AgentLoginError> {
        for var in &fw.env {
            if let Some(v) = env(var)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Ok(Some((Source::Env(var.clone()), Secret(v))));
            }
        }
        Ok(self.load(fw)?.map(|s| {
            (
                Source::Stored(
                    match self.record(&fw.id).and_then(|r| r.custody).as_deref() {
                        Some("keychain") => "keychain".to_string(),
                        _ => "file".to_string(),
                    },
                ),
                s,
            )
        }))
    }

    /// The cheap, secret-free state of `fw` for `ta agent-login list`: reads the
    /// record, the environment and custody presence; no network, no secret read.
    pub fn status(
        &self,
        fw: &Framework,
        env: &dyn Fn(&str) -> Option<String>,
        now: i64,
    ) -> FrameworkStatus {
        let rec = self.record(&fw.id);
        let mk = |state, checked_at, detail: String, source| FrameworkStatus {
            id: fw.id.clone(),
            state,
            checked_at,
            detail,
            source,
        };
        // A login in progress counts for as long as the vendor command may run.
        if let Some(r) = rec.as_ref().filter(|r| r.state == State::LoggingIn) {
            let window = fw.login_timeout_secs() as i64 + 30;
            if now - r.checked_at <= window {
                return mk(
                    State::LoggingIn,
                    r.checked_at,
                    "a login prompt is open on this computer".to_string(),
                    Source::None,
                );
            }
        }
        // An environment variable wins over the stored login, as it does at launch.
        let env_var = fw
            .env
            .iter()
            .find(|v| env(v).is_some_and(|x| !x.trim().is_empty()));
        if let Some(var) = env_var {
            let checked = rec
                .as_ref()
                .filter(|r| {
                    r.source.as_deref() == Some(&format!("env:{var}"))
                        && matches!(r.state, State::Ok | State::Invalid)
                })
                .cloned();
            return match checked {
                Some(r) => mk(r.state, r.checked_at, r.detail, Source::Env(var.clone())),
                None => mk(
                    State::Unknown,
                    rec.as_ref().map_or(now, |r| r.checked_at),
                    format!(
                        "{var} is set in the environment (value not read); run \
                         `ta agent-login verify {}` to check it",
                        fw.id
                    ),
                    Source::Env(var.clone()),
                ),
            };
        }
        if self.is_stored(fw) {
            let custody = match rec.as_ref().and_then(|r| r.custody.as_deref()) {
                Some("keychain") => "the OS keychain".to_string(),
                _ => format!("{}", self.secret_path(&fw.id).display()),
            };
            let (state, checked_at, detail) = match rec {
                Some(r) if matches!(r.state, State::Ok | State::Invalid | State::Unknown) => {
                    let d = if r.detail.is_empty() {
                        format!("stored in {custody}")
                    } else {
                        format!("{}; stored in {custody}", r.detail)
                    };
                    (r.state, r.checked_at, d)
                }
                _ => (
                    State::Unknown,
                    now,
                    format!("stored in {custody}; not verified yet"),
                ),
            };
            return mk(state, checked_at, detail, Source::Stored(custody));
        }
        mk(
            State::Missing,
            rec.as_ref().map_or(now, |r| r.checked_at),
            format!(
                "no login on this computer; run `ta agent-login login {}`",
                fw.id
            ),
            Source::None,
        )
    }
}

// ── Launch ─────────────────────────────────────────────────────────────────

/// One variable to put in an agent child's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchLogin {
    pub framework_id: String,
    pub var: String,
    pub secret: Secret,
    /// Where the secret was held, for the launch's log line.
    pub held_in: String,
}

/// What a launch for an agent should do about its login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The environment already has a login for the agent; nothing is injected.
    EnvAlreadySet(String),
    /// Put this variable in the child's environment.
    Inject(LaunchLogin),
    /// No framework serves the agent, or none has a stored login.
    NothingStored,
    /// A login is stored but could not be read; the text says why and what to do.
    Unreadable(String),
}

/// The stored login to inject into a launch of `agent`, if any. An environment
/// variable that already carries a login for the agent wins and nothing is
/// injected. At most one variable is returned: the first framework (catalog
/// order) that serves `agent` and has a stored login.
pub fn launch_login(
    store: &LoginStore,
    catalog: &Catalog,
    agent: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> LaunchOutcome {
    let serving = catalog.serving(agent);
    for fw in &serving {
        if let Some(var) = fw
            .env
            .iter()
            .find(|v| env(v).is_some_and(|x| !x.trim().is_empty()))
        {
            return LaunchOutcome::EnvAlreadySet(var.clone());
        }
    }
    let mut unreadable = None;
    for fw in serving {
        if !store.is_stored(fw) {
            continue;
        }
        match store.load(fw) {
            Ok(Some(secret)) => {
                return LaunchOutcome::Inject(LaunchLogin {
                    framework_id: fw.id.clone(),
                    var: fw.primary_env().to_string(),
                    secret,
                    held_in: match store.record(&fw.id).and_then(|r| r.custody).as_deref() {
                        Some("keychain") => "the OS keychain".to_string(),
                        _ => store.secret_path(&fw.id).display().to_string(),
                    },
                })
            }
            Ok(None) => {
                unreadable.get_or_insert(format!(
                    "the {} login is recorded as stored but this process cannot read it \
                     (it was stored with {} custody and this process uses {}). Run \
                     `ta agent-login login {}` again, or `ta credentials custody status`",
                    fw.id,
                    store
                        .record(&fw.id)
                        .and_then(|r| r.custody)
                        .unwrap_or_else(|| "other".into()),
                    if store.use_keychain {
                        "keychain"
                    } else {
                        "file"
                    },
                    fw.id
                ));
            }
            Err(e) => {
                unreadable.get_or_insert(e.to_string());
            }
        }
    }
    match unreadable {
        Some(why) => LaunchOutcome::Unreadable(why),
        None => LaunchOutcome::NothingStored,
    }
}

/// How usable the login for an agent is, without reading any secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLogin {
    /// An environment variable or a stored login that has not been rejected.
    Present,
    Missing,
    /// Stored, but the last verify call rejected it.
    Invalid,
}

/// Whether a launch of `agent` has a login to use (secret-free; safe in the daemon).
pub fn status_for_agent(
    store: &LoginStore,
    catalog: &Catalog,
    agent: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> AgentLogin {
    let serving = catalog.serving(agent);
    if serving.iter().any(|fw| {
        fw.env
            .iter()
            .any(|v| env(v).is_some_and(|x| !x.trim().is_empty()))
    }) {
        return AgentLogin::Present;
    }
    let mut any_invalid = false;
    for fw in serving {
        if store.is_stored(fw) {
            match store.record(&fw.id).map(|r| r.state) {
                Some(State::Invalid) => any_invalid = true,
                _ => return AgentLogin::Present,
            }
        }
    }
    if any_invalid {
        AgentLogin::Invalid
    } else {
        AgentLogin::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SENTINEL: &str = concat!("sk-ant-", "oat01-SENTINEL-SECRET-VALUE-1234567890");

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn store(dir: &Path) -> LoginStore {
        LoginStore::with(dir, false)
    }

    #[test]
    fn the_shipped_catalog_parses_and_is_complete() {
        let c = Catalog::shipped();
        let ids: Vec<_> = c.frameworks.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-code",
                "anthropic-api-key",
                "openai",
                "gemini",
                "xai"
            ]
        );
        let claude = c.get("claude-code").unwrap();
        assert_eq!(claude.kind, Kind::Token);
        assert_eq!(claude.login.method, LoginMethod::OauthCli);
        assert_eq!(claude.login.command, ["claude", "setup-token"]);
        assert_eq!(claude.primary_env(), "CLAUDE_CODE_OAUTH_TOKEN");
        let gemini = c.get("gemini").unwrap();
        assert_eq!(gemini.env, ["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
        assert!(c
            .frameworks
            .iter()
            .filter(|f| f.kind == Kind::ApiKey)
            .all(|f| f.login.method == LoginMethod::Prompt));
        assert!(c
            .get("nope")
            .unwrap_err()
            .to_string()
            .contains("Known frameworks: claude-code"));
    }

    #[test]
    fn overrides_replace_by_id_and_add_new_ones_project_last() {
        let t = tempfile::tempdir().unwrap();
        let user = t.path().join("user.toml");
        let project = t.path().join("project.toml");
        std::fs::write(
            &user,
            r#"
[[framework]]
id = "openai"
label = "OpenAI (company)"
kind = "api_key"
how = "x"
env = ["OPENAI_API_KEY", "COMPANY_KEY"]
[framework.login]
method = "prompt"

[[framework]]
id = "mistral"
label = "Mistral"
kind = "api_key"
how = "y"
agents = ["vibe"]
env = ["MISTRAL_API_KEY"]
[framework.login]
method = "prompt"
"#,
        )
        .unwrap();
        std::fs::write(
            &project,
            r#"
[[framework]]
id = "mistral"
label = "Mistral (project)"
kind = "api_key"
how = "z"
env = ["MISTRAL_API_KEY"]
[framework.login]
method = "prompt"
"#,
        )
        .unwrap();
        let c = Catalog::load_from(Some(&user), Some(&project)).unwrap();
        assert_eq!(c.get("openai").unwrap().label, "OpenAI (company)");
        assert_eq!(c.get("mistral").unwrap().label, "Mistral (project)");
        assert_eq!(c.frameworks.len(), 6);
        assert_eq!(c.applied_overrides.len(), 2);
        // a missing override file is fine
        let none = Catalog::load_from(Some(&t.path().join("absent.toml")), None).unwrap();
        assert_eq!(none.frameworks.len(), 5);
    }

    #[test]
    fn a_bad_override_names_the_file_and_the_problem() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("agent-frameworks.toml");
        for (text, needle) in [
            ("[[framework]]\nid = \"Bad Id\"\nlabel=\"x\"\nkind=\"api_key\"\nhow=\"h\"\nenv=[\"A\"]\n[framework.login]\nmethod=\"prompt\"\n", "id must be"),
            ("[[framework]]\nid = \"x\"\nlabel=\"x\"\nkind=\"token\"\nhow=\"h\"\nenv=[\"A\"]\n[framework.login]\nmethod=\"oauth_cli\"\n", "needs login.command"),
            ("[[framework]]\nid = \"x\"\nlabel=\"x\"\nkind=\"api_key\"\nhow=\"h\"\nenv=[\"TA_SECRET\"]\n[framework.login]\nmethod=\"prompt\"\n", "reserved"),
            ("[[framework]]\nid = \"x\"\nlabel=\"x\"\nkind=\"api_key\"\nhow=\"h\"\nenv=[\"A\"]\nbogus=1\n[framework.login]\nmethod=\"prompt\"\n", "unknown field"),
            ("not toml at all [", "TOML"),
        ] {
            std::fs::write(&p, text).unwrap();
            let e = Catalog::load_from(Some(&p), None).unwrap_err().to_string();
            assert!(e.contains(&p.display().to_string()), "{e}");
            assert!(e.contains(needle), "{needle}: {e}");
            assert!(e.contains("shipped catalog applies without it"), "{e}");
        }
    }

    #[test]
    fn store_load_remove_round_trip_with_owner_only_file() {
        let t = tempfile::tempdir().unwrap();
        let s = store(t.path());
        let c = Catalog::shipped();
        let fw = c.get("openai").unwrap();
        assert!(!s.is_stored(fw));
        let at = s.store(fw, &Secret::new(SENTINEL), 100).unwrap();
        assert!(matches!(at.custody, Custody::File(_)));
        assert!(s.is_stored(fw));
        assert_eq!(s.load(fw).unwrap().unwrap().expose(), SENTINEL);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(s.secret_path("openai"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // the record never holds the secret
        let rec = std::fs::read_to_string(s.record_path("openai")).unwrap();
        assert!(!rec.contains("SENTINEL"), "{rec}");
        assert!(s.remove(fw, 200).unwrap());
        assert!(!s.is_stored(fw));
        assert!(s.load(fw).unwrap().is_none());
        let rec = s.record("openai").unwrap();
        assert_eq!(rec.state, State::Missing);
        assert_eq!(rec.checked_at, 200);
        // removing again is not an error and reports nothing removed
        assert!(!s.remove(fw, 300).unwrap());
    }

    #[test]
    fn status_transitions_missing_unknown_ok_invalid_logging_in_missing() {
        let t = tempfile::tempdir().unwrap();
        let s = store(t.path());
        let c = Catalog::shipped();
        let fw = c.get("openai").unwrap();
        let none = env_of(&[]);
        let st = s.status(fw, &none, 10);
        assert_eq!(st.state, State::Missing);
        assert!(st.detail.contains("ta agent-login login openai"));
        s.store(fw, &Secret::new(SENTINEL), 20).unwrap();
        let st = s.status(fw, &none, 25);
        assert_eq!((st.state, st.checked_at), (State::Unknown, 20));
        s.set_state("openai", State::Ok, "verified", Some("stored".into()), 30)
            .unwrap();
        let st = s.status(fw, &none, 31);
        assert_eq!((st.state, st.checked_at), (State::Ok, 30));
        assert!(st.detail.contains("verified"), "{}", st.detail);
        s.set_state("openai", State::Invalid, "rejected (401)", None, 40)
            .unwrap();
        assert_eq!(s.status(fw, &none, 41).state, State::Invalid);
        s.set_state("openai", State::LoggingIn, "", None, 50)
            .unwrap();
        assert_eq!(s.status(fw, &none, 60).state, State::LoggingIn);
        // a stale logging_in (the prompt died) falls back to the stored state
        let late = 50 + fw.login_timeout_secs() as i64 + 31;
        assert_ne!(s.status(fw, &none, late).state, State::LoggingIn);
        s.remove(fw, 70).unwrap();
        let st = s.status(fw, &none, 71);
        assert_eq!((st.state, st.checked_at), (State::Missing, 70));
    }

    #[test]
    fn an_environment_variable_wins_over_the_stored_login_in_status() {
        let t = tempfile::tempdir().unwrap();
        let s = store(t.path());
        let c = Catalog::shipped();
        let fw = c.get("gemini").unwrap();
        s.store(fw, &Secret::new("stored-value"), 1).unwrap();
        let env = env_of(&[("GOOGLE_API_KEY", "from-env")]);
        let st = s.status(fw, &env, 5);
        assert_eq!(st.source, Source::Env("GOOGLE_API_KEY".into()));
        assert_eq!(st.state, State::Unknown);
        assert!(!st.detail.contains("from-env"));
        let (src, secret) = s.effective(fw, &env).unwrap().unwrap();
        assert_eq!(src, Source::Env("GOOGLE_API_KEY".into()));
        assert_eq!(secret.expose(), "from-env");
        let (src, secret) = s.effective(fw, &env_of(&[])).unwrap().unwrap();
        assert!(matches!(src, Source::Stored(_)));
        assert_eq!(secret.expose(), "stored-value");
    }

    #[test]
    fn launch_injects_only_the_launching_agents_variable_and_env_wins() {
        let t = tempfile::tempdir().unwrap();
        let s = store(t.path());
        let c = Catalog::shipped();
        s.store(c.get("claude-code").unwrap(), &Secret::new(SENTINEL), 1)
            .unwrap();
        s.store(c.get("openai").unwrap(), &Secret::new("openai-secret"), 1)
            .unwrap();
        // claude-code gets CLAUDE_CODE_OAUTH_TOKEN and nothing else
        match launch_login(&s, &c, "claude-code", &env_of(&[])) {
            LaunchOutcome::Inject(l) => {
                assert_eq!(l.var, "CLAUDE_CODE_OAUTH_TOKEN");
                assert_eq!(l.secret.expose(), SENTINEL);
                assert_eq!(l.framework_id, "claude-code");
                assert!(!format!("{l:?}").contains("SENTINEL"), "{l:?}");
            }
            other => panic!("{other:?}"),
        }
        // codex gets OPENAI_API_KEY only
        match launch_login(&s, &c, "codex", &env_of(&[])) {
            LaunchOutcome::Inject(l) => assert_eq!(l.var, "OPENAI_API_KEY"),
            other => panic!("{other:?}"),
        }
        // any login variable already set for the agent wins, including an alias
        for var in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ] {
            assert_eq!(
                launch_login(&s, &c, "claude-code", &env_of(&[(var, "x")])),
                LaunchOutcome::EnvAlreadySet(var.to_string())
            );
        }
        // an empty variable is not a login
        assert!(matches!(
            launch_login(
                &s,
                &c,
                "claude-code",
                &env_of(&[("ANTHROPIC_API_KEY", " ")])
            ),
            LaunchOutcome::Inject(_)
        ));
        // an agent no framework serves, or with nothing stored
        assert_eq!(
            launch_login(&s, &c, "ollama", &env_of(&[])),
            LaunchOutcome::NothingStored
        );
        assert_eq!(
            launch_login(&s, &c, "grok", &env_of(&[])),
            LaunchOutcome::NothingStored
        );
    }

    #[test]
    fn status_for_agent_follows_env_then_stored_then_recorded_verify() {
        let t = tempfile::tempdir().unwrap();
        let s = store(t.path());
        let c = Catalog::shipped();
        let none = env_of(&[]);
        assert_eq!(
            status_for_agent(&s, &c, "claude-code", &none),
            AgentLogin::Missing
        );
        assert_eq!(
            status_for_agent(
                &s,
                &c,
                "claude-code",
                &env_of(&[("ANTHROPIC_AUTH_TOKEN", "x")])
            ),
            AgentLogin::Present
        );
        let fw = c.get("claude-code").unwrap();
        s.store(fw, &Secret::new(SENTINEL), 1).unwrap();
        assert_eq!(
            status_for_agent(&s, &c, "claude-code", &none),
            AgentLogin::Present
        );
        s.set_state("claude-code", State::Invalid, "rejected", None, 2)
            .unwrap();
        assert_eq!(
            status_for_agent(&s, &c, "claude-code", &none),
            AgentLogin::Invalid
        );
        // a second, unrejected login for the same agent makes it usable again
        s.store(c.get("anthropic-api-key").unwrap(), &Secret::new("k"), 3)
            .unwrap();
        assert_eq!(
            status_for_agent(&s, &c, "claude-code", &none),
            AgentLogin::Present
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        assert!(!format!("{:?}", Secret::new(SENTINEL)).contains("SENTINEL"));
    }

    #[test]
    fn errors_never_contain_the_secret() {
        let t = tempfile::tempdir().unwrap();
        // make the target a file so the store fails
        let blocker = t.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let s = store(&blocker);
        let c = Catalog::shipped();
        let e = s
            .store(c.get("openai").unwrap(), &Secret::new(SENTINEL), 1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("could not store the openai login"), "{e}");
        assert!(!e.contains("SENTINEL"), "{e}");
    }
}
