// human.rs: The human credential: proof that a person, not an agent, is acting.
//
// `ta login` mints a random secret and stores it OUTSIDE every project tree:
//
//   * the secret itself lives in the OS keychain (an item bound to the signed
//     `ta` binary) where one is available, otherwise in a mode-0600 file in the
//     per-user state directory;
//   * a SHA-256 digest of the secret is written next to it so the daemon can
//     verify a presented credential without ever touching the keychain.
//
// The daemon requires the secret (header `X-TA-Human-Credential`) on every
// mutating route, and `ta draft approve/deny/apply` require it when an agent
// context is detected.
//
// HONEST LIMITS. While agents run as the same OS user as the human, this is a
// speed bump, not a wall: an agent that reads the 0600 fallback file (or talks
// the user into a keychain prompt) can present the credential. The OS sandbox
// phases (v0.17.12.6 onward) are what turn it into a wall. Nothing here claims
// more than that.
//
// Tests never touch the real keychain: `default_use_keychain()` is false inside
// any cargo test binary and when `TA_NO_KEYCHAIN` is set, exactly like the
// vault (see `encryption::keychain_guard_active`).

use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Overrides the per-user state directory (tests, containers).
pub const STATE_DIR_ENV: &str = "TA_USER_STATE_DIR";
/// Environment variable that supplies the credential directly (CI, plugins).
pub const SECRET_ENV: &str = "TA_HUMAN_CREDENTIAL";
/// HTTP header carrying the human credential to the daemon.
pub const CREDENTIAL_HEADER: &str = "x-ta-human-credential";
/// HTTP header a client sends when it detected an agent context. ADVISORY: it
/// feeds the audit trail, it is not a security control (an agent can omit it).
pub const ACTOR_CONTEXT_HEADER: &str = "x-ta-actor-context";
/// Keychain service name for the human credential item.
const KEYRING_SERVICE: &str = "trusted-autonomy-human";
const KEYRING_USER: &str = "human-credential";
const SECRET_FILENAME: &str = "credential";
const DIGEST_FILENAME: &str = "credential.sha256";
const KEYS_FILENAME: &str = "assertion-keys.json";
const SECRET_PREFIX: &str = "tahc_";

/// Errors from human-credential handling. Every message says what was being
/// attempted and what to do next.
#[derive(Debug, thiserror::Error)]
pub enum HumanCredentialError {
    #[error(
        "could not write the human credential under {path}: {source}. \
         Check that the directory is writable, or set {STATE_DIR_ENV} to a directory you own."
    )]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the assertion key registry at {path} is unreadable or corrupt: {reason}. \
         Fix or delete the file, then re-register keys with `ta keys add`."
    )]
    Registry { path: PathBuf, reason: String },
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// Where the secret ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Custody {
    /// OS keychain item bound to the signed `ta` binary.
    Keychain,
    /// Mode-0600 file (user-only ACL note on Windows) at this path.
    File(PathBuf),
}

impl Custody {
    pub fn describe(&self) -> String {
        match self {
            Custody::Keychain => "the OS keychain (item bound to the signed `ta` binary)".into(),
            Custody::File(p) => format!("the file {} (owner-only permissions)", p.display()),
        }
    }
}

/// The per-user state directory for human credentials: never inside a
/// project tree. `$TA_USER_STATE_DIR` wins, otherwise `~/.config/ta/human`
/// (the literal `~/.config/ta` TA uses everywhere, also on macOS and Windows).
pub fn user_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(STATE_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("ta")
        .join("human")
}

/// Whether the OS keychain may be used: not when `TA_NO_KEYCHAIN` is set and
/// never inside a test binary.
pub fn default_use_keychain() -> bool {
    std::env::var_os(crate::encryption::NO_KEYCHAIN_ENV).is_none()
        && !crate::encryption::keychain_guard_active()
}

fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality for two equal-purpose hex digests.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn write_private(path: &Path, contents: &str) -> Result<(), HumanCredentialError> {
    let werr = |source| HumanCredentialError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(werr)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(werr)?;
        f.write_all(contents.as_bytes()).map_err(werr)?;
        // An existing file keeps its old mode through `open`, so enforce it.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(werr)?;
    }
    #[cfg(not(unix))]
    {
        // Windows: the per-user directory under the profile inherits a
        // user-only ACL; there is no mode bit to set.
        std::fs::write(path, contents).map_err(werr)?;
    }
    Ok(())
}

/// Result of `login`.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub custody: Custody,
    pub digest_path: PathBuf,
    /// Set when the keychain was wanted but unusable (the reason).
    pub keychain_fallback_reason: Option<String>,
}

/// Mint a fresh credential, replacing any previous one. Returns where the
/// secret now lives. The secret itself is never returned or printed.
pub fn login(state_dir: &Path, use_keychain: bool) -> Result<LoginOutcome, HumanCredentialError> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let secret = format!(
        "{SECRET_PREFIX}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let secret_path = state_dir.join(SECRET_FILENAME);
    let digest_path = state_dir.join(DIGEST_FILENAME);

    let mut custody = None;
    let mut reason = None;
    if use_keychain {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .and_then(|e| e.set_password(&secret))
        {
            Ok(()) => {
                custody = Some(Custody::Keychain);
                // A stale fallback file must not outlive the keychain item.
                let _ = std::fs::remove_file(&secret_path);
            }
            Err(e) => reason = Some(e.to_string()),
        }
    }
    let custody = match custody {
        Some(c) => c,
        None => {
            write_private(&secret_path, &secret)?;
            Custody::File(secret_path)
        }
    };
    write_private(&digest_path, &hash_secret(&secret))?;
    Ok(LoginOutcome {
        custody,
        digest_path,
        keychain_fallback_reason: reason,
    })
}

/// Load the secret for presenting to the daemon: keychain first (when allowed),
/// then the fallback file. `None` when `ta login` has not been run.
pub fn load_secret(state_dir: &Path, use_keychain: bool) -> Option<String> {
    // Scripts and editor plugins that cannot reach the keychain may be handed the
    // secret in the environment.
    if let Some(v) = std::env::var(SECRET_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| v.starts_with(SECRET_PREFIX))
    {
        return Some(v);
    }
    if use_keychain {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if let Ok(s) = entry.get_password() {
                return Some(s);
            }
        }
    }
    std::fs::read_to_string(state_dir.join(SECRET_FILENAME))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Verify a presented secret against the stored digest (constant time).
pub fn verify_secret(state_dir: &Path, presented: &str) -> bool {
    let Ok(stored) = std::fs::read_to_string(state_dir.join(DIGEST_FILENAME)) else {
        return false;
    };
    let stored = stored.trim();
    if stored.is_empty() || !presented.starts_with(SECRET_PREFIX) {
        return false;
    }
    constant_time_eq(stored.as_bytes(), hash_secret(presented).as_bytes())
}

/// True when a credential has been minted (a digest exists) and the secret for
/// it can be loaded and verifies. This is what "a human credential is present"
/// means for `ta draft approve/deny/apply`.
pub fn credential_present(state_dir: &Path, use_keychain: bool) -> bool {
    load_secret(state_dir, use_keychain)
        .map(|s| verify_secret(state_dir, &s))
        .unwrap_or(false)
}

/// Remove the credential (digest, fallback file and keychain item).
/// Returns true if anything was removed.
pub fn logout(state_dir: &Path, use_keychain: bool) -> bool {
    let mut removed = false;
    for name in [SECRET_FILENAME, DIGEST_FILENAME] {
        removed |= std::fs::remove_file(state_dir.join(name)).is_ok();
    }
    if use_keychain {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            removed |= entry.delete_credential().is_ok();
        }
    }
    removed
}

// ── Agent-context detection (advisory) ─────────────────────────────────────

/// Why the current process looks like it is running under an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    pub signals: Vec<String>,
}

/// Env vars whose mere presence marks an agent launch. `TA_AGENT_CONTEXT` is
/// set by `ta run` for every agent it launches; the rest are markers other
/// tooling already sets. ADVISORY: an agent can unset them.
pub const AGENT_MARKER_VARS: &[&str] = &[
    "TA_AGENT_CONTEXT",
    "TA_IS_STAGING",
    "TA_AGENT_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
];

/// Pure detection over an environment lookup, a working directory and whether
/// a human terminal is attached. Separated from the process environment so it
/// is testable without mutating global state.
pub fn detect_agent_context_from(
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    interactive_terminal: bool,
) -> Option<AgentContext> {
    let mut signals = Vec::new();
    for var in AGENT_MARKER_VARS {
        if env(var).is_some_and(|v| !v.is_empty()) {
            signals.push(format!("{var} is set"));
        }
    }
    if let Some(mode) = env("TA_CALLER_MODE").filter(|m| !m.is_empty() && m != "normal") {
        signals.push(format!("TA_CALLER_MODE={mode}"));
    }
    if env("TA_CHAT_MODE").is_some_and(|v| !v.is_empty()) {
        signals.push("TA_CHAT_MODE is set".to_string());
    }
    let normalized = cwd.to_string_lossy().replace('\\', "/");
    if normalized.contains("/.ta/staging/") || normalized.ends_with("/.ta/staging") {
        signals.push("working directory is inside a .ta/staging workspace".to_string());
    }
    if !interactive_terminal {
        signals.push("no interactive terminal (headless launch)".to_string());
    }
    if signals.is_empty() {
        None
    } else {
        Some(AgentContext { signals })
    }
}

/// Detection against the real process environment and terminal.
pub fn detect_agent_context() -> Option<AgentContext> {
    use std::io::IsTerminal;
    let cwd = std::env::current_dir().unwrap_or_default();
    // Headless means NEITHER end is a terminal: an orchestrator that pipes a
    // child's output still inherits the person's terminal on stdin.
    let interactive = std::io::stdin().is_terminal() || std::io::stdout().is_terminal();
    detect_agent_context_from(&|k| std::env::var(k).ok(), &cwd, interactive)
}

/// The one-line message shown when an approve/deny/apply is refused.
pub fn refusal_message(action: &str, ctx: &AgentContext) -> String {
    format!(
        "refusing to {action}: this looks like an agent context ({}) and no human credential \
         is present. A person must run `ta login` once in their own terminal, then run the \
         command themselves (inside a coding assistant: `! ta draft {action} <id>`). \
         These checks are advisory while agents run as your OS user; the OS sandbox phases \
         make them enforceable.",
        ctx.signals.join("; ")
    )
}

// ── Agent delegation (interim development mode) ───────────────────────────
//
// While a developer has run `ta login --file` and lets a coding assistant
// approve and apply drafts, the credential is present, so the gate accepts
// agent-context approvals. That is a delegation, not a direct human action, and
// it is recorded as one.
//
// Labeling convention for `actor_context`:
//   * `agent_delegated`: accepted from a detected agent context because the
//     credential holder's credential was present. Always recorded, with the
//     detection reasons (`delegated_signals`) and the holder as the delegating owner.
//   * absent (CLI records): a direct human action from a plain terminal. The CLI
//     deliberately does NOT write `human_direct`, so old and new records read the
//     same way for direct actions; `ACTOR_HUMAN_DIRECT` exists for display only.
//   * `direct` (daemon records): the daemon, which sees every request, records
//     `direct` for approve, deny and apply that arrived without an agent header.

/// `actor_context` value for an action accepted from a detected agent context.
pub const ACTOR_AGENT_DELEGATED: &str = "agent_delegated";
/// Display-only name for a direct human action (never persisted; see above).
pub const ACTOR_HUMAN_DIRECT: &str = "human_direct";

const AGENT_SEEN_FILENAME: &str = "agent-context-seen";
/// How long after the last sighting of an agent context delegation counts as active.
pub const DELEGATION_ACTIVE_WINDOW_HOURS: i64 = 24;

/// The credential holder: the OS user who ran `ta login`. Named as the
/// delegating owner on delegated actions.
pub fn credential_holder() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown-user".to_string())
}

/// When an agent context was last seen at a gated action, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSighting {
    pub at: chrono::DateTime<chrono::Utc>,
    pub signals: Vec<String>,
}

/// Record that an agent context was seen at a gated action (accepted or
/// refused). Stored in the per-user state directory, mode 0600.
pub fn record_agent_context_seen(
    state_dir: &Path,
    ctx: &AgentContext,
) -> Result<(), HumanCredentialError> {
    record_agent_context_seen_at(state_dir, ctx, chrono::Utc::now())
}

/// As [`record_agent_context_seen`] with a supplied clock.
pub fn record_agent_context_seen_at(
    state_dir: &Path,
    ctx: &AgentContext,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), HumanCredentialError> {
    let mut body = now.to_rfc3339();
    for s in &ctx.signals {
        body.push('\n');
        body.push_str(&s.replace('\n', " "));
    }
    write_private(&state_dir.join(AGENT_SEEN_FILENAME), &body)
}

/// The last agent-context sighting, if one was recorded and readable.
pub fn last_agent_context_seen(state_dir: &Path) -> Option<AgentSighting> {
    let text = std::fs::read_to_string(state_dir.join(AGENT_SEEN_FILENAME)).ok()?;
    let mut lines = text.lines();
    let at = chrono::DateTime::parse_from_rfc3339(lines.next()?.trim())
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(AgentSighting {
        at,
        signals: lines
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    })
}

/// The sighting, only when it happened within `window` of `now`.
pub fn agent_context_seen_within(
    state_dir: &Path,
    window: chrono::Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<AgentSighting> {
    last_agent_context_seen(state_dir).filter(|s| now - s.at <= window && s.at <= now + window)
}

fn humanize_age(age: chrono::Duration) -> String {
    let mins = age.num_minutes().max(0);
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins} minute(s) ago")
    } else {
        format!("{} hour(s) ago", age.num_hours())
    }
}

/// The 'agent delegation active' line for `ta login status` and `ta status`:
/// `Some` only when a credential exists and an agent context was seen within
/// the last 24 hours. Pure: the caller supplies presence, sighting and clock.
pub fn delegation_status_line(
    credential_present: bool,
    sighting: Option<&AgentSighting>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let sighting = sighting?;
    if !credential_present
        || now - sighting.at > chrono::Duration::hours(DELEGATION_ACTIVE_WINDOW_HOURS)
    {
        return None;
    }
    let reasons = if sighting.signals.is_empty() {
        "no reasons recorded".to_string()
    } else {
        sighting.signals.join("; ")
    };
    Some(format!(
        "agent delegation active: an agent context was seen {} ({reasons}), so approve, deny and \
         apply from it are accepted on your credential and recorded as agent_delegated. \
         Run `ta login logout` to switch back to Wayfinder-only approvals.",
        humanize_age(now - sighting.at)
    ))
}

/// True when a credential has been minted (its digest exists). Unlike
/// [`credential_present`] it never loads the secret, so the daemon can use it
/// without touching the keychain.
pub fn credential_digest_present(state_dir: &Path) -> bool {
    std::fs::read_to_string(state_dir.join(DIGEST_FILENAME))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// The `agent_delegation` object in `GET /api/status`: whether delegation is
/// active (credential present and an agent context seen in the last 24 hours).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationStatus {
    pub active: bool,
    /// RFC 3339 time of the last recorded agent-context approval or sighting.
    pub last_seen: Option<String>,
    pub credential_present: bool,
    /// Why that context was judged to be an agent.
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// Compute [`DelegationStatus`] from a credential flag and the last sighting.
pub fn delegation_status(
    credential_present: bool,
    sighting: Option<&AgentSighting>,
    now: chrono::DateTime<chrono::Utc>,
) -> DelegationStatus {
    let recent =
        sighting.filter(|s| now - s.at <= chrono::Duration::hours(DELEGATION_ACTIVE_WINDOW_HOURS));
    DelegationStatus {
        active: credential_present && recent.is_some(),
        last_seen: sighting.map(|s| s.at.to_rfc3339()),
        credential_present,
        reasons: sighting.map(|s| s.signals.clone()).unwrap_or_default(),
    }
}

impl DelegationStatus {
    /// The 'agent delegation active' line, or `None` when not active. Used by
    /// the CLI when it got this object from the daemon.
    pub fn line(&self, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
        let at = chrono::DateTime::parse_from_rfc3339(self.last_seen.as_deref()?)
            .ok()?
            .with_timezone(&chrono::Utc);
        let sighting = AgentSighting {
            at,
            signals: self.reasons.clone(),
        };
        delegation_status_line(self.credential_present && self.active, Some(&sighting), now)
    }
}

// ── Actor-context header (advisory) ──────────────────────────────────────
//
// Approvals through the daemon cannot see the client's environment, so clients
// that detect an agent context say so in `X-TA-Actor-Context`:
//
//   agent_delegated; reasons=CLAUDECODE%20is%20set,no%20interactive%20terminal
//
// Reasons are percent-encoded (URL-safe), at most `MAX_ACTOR_REASONS` of at
// most `MAX_ACTOR_REASON_CHARS` characters, and the whole header stays under
// `MAX_ACTOR_HEADER_BYTES`. Header absent means a direct human action. The
// daemon ignores a malformed or oversize header (debug log), never errors.

/// Set (to `1`) by the daemon on a `ta draft apply` it spawns for a request it
/// already labeled, so the headless child does not label or log a sighting for
/// an action a person took in Studio. Labels are advisory; this does not touch
/// the approve/deny/apply gate.
pub const DAEMON_RECORDED_ENV: &str = "TA_ACTOR_CONTEXT_RECORDED_BY_DAEMON";

/// Value token of the header for a delegated action.
pub const ACTOR_HEADER_VALUE: &str = ACTOR_AGENT_DELEGATED;
/// Most reasons carried in the header.
pub const MAX_ACTOR_REASONS: usize = 5;
/// Longest single reason, in characters, before encoding.
pub const MAX_ACTOR_REASON_CHARS: usize = 60;
/// Longest accepted header value, in bytes.
pub const MAX_ACTOR_HEADER_BYTES: usize = 512;

fn percent_encode_reason(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'=') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode_reason(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The `X-TA-Actor-Context` value for a detected agent context.
pub fn actor_context_header_value(ctx: &AgentContext) -> String {
    let mut value = ACTOR_HEADER_VALUE.to_string();
    let mut first = true;
    for signal in ctx.signals.iter().take(MAX_ACTOR_REASONS) {
        let reason: String = signal.chars().take(MAX_ACTOR_REASON_CHARS).collect();
        let piece = percent_encode_reason(&reason);
        let prefix = if first { "; reasons=" } else { "," };
        if value.len() + prefix.len() + piece.len() > MAX_ACTOR_HEADER_BYTES {
            break;
        }
        value.push_str(prefix);
        value.push_str(&piece);
        first = false;
    }
    value
}

/// Parse an `X-TA-Actor-Context` value into its reasons. `Err` carries why the
/// value was rejected (for a debug log); callers ignore such a header.
pub fn parse_actor_context_header(value: &str) -> Result<Vec<String>, String> {
    if value.len() > MAX_ACTOR_HEADER_BYTES {
        return Err(format!(
            "value is {} bytes, over the {MAX_ACTOR_HEADER_BYTES} byte limit",
            value.len()
        ));
    }
    let value = value.trim();
    let (kind, rest) = match value.split_once(';') {
        Some((k, r)) => (k.trim(), Some(r.trim())),
        None => (value, None),
    };
    if kind != ACTOR_HEADER_VALUE {
        return Err(format!("unknown actor context '{kind}'"));
    }
    let Some(rest) = rest.filter(|r| !r.is_empty()) else {
        return Ok(Vec::new());
    };
    let Some(list) = rest.strip_prefix("reasons=") else {
        return Err("expected 'reasons=<list>' after the actor context".to_string());
    };
    let mut reasons = Vec::new();
    for raw in list.split(',').filter(|r| !r.is_empty()) {
        let decoded = percent_decode_reason(raw)
            .ok_or_else(|| "a reason is not valid percent-encoded UTF-8".to_string())?;
        if decoded.chars().any(char::is_control) {
            return Err("a reason contains a control character".to_string());
        }
        if reasons.len() < MAX_ACTOR_REASONS {
            reasons.push(decoded.chars().take(MAX_ACTOR_REASON_CHARS).collect());
        }
    }
    Ok(reasons)
}

/// The same agent-marker detection the plugins use: environment only (no
/// terminal or working-directory signals). Returns the reasons.
pub fn agent_markers_from_env(env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    detect_agent_context_from(env, Path::new(""), true)
        .map(|c| c.signals)
        .unwrap_or_default()
}

// ── Install identity ───────────────────────────────────────────────────────

const INSTALL_ID_FILENAME: &str = "install-id";

/// This install's id: the `aud` every human assertion must name. Generated on
/// first use and stored in the per-user state directory (never in a project),
/// so one id covers every project the daemon serves for this user.
pub fn install_id(state_dir: &Path) -> Result<String, HumanCredentialError> {
    let path = state_dir.join(INSTALL_ID_FILENAME);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let id = format!(
        "ta_install_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    write_private(&path, &id)?;
    Ok(id)
}

// ── Assertion key registry (user level, never in a project) ────────────────

/// Roles that may act on an `any_maintainer` item unless a key says otherwise.
pub const DEFAULT_MAINTAINER_ROLES: &[&str] = &["admin", "owner"];
/// Role ranks, lowest first, unless a key says otherwise.
pub const DEFAULT_ROLE_ORDER: &[&str] = &["member", "admin", "owner"];

fn default_maintainer_roles() -> Vec<String> {
    DEFAULT_MAINTAINER_ROLES
        .iter()
        .map(|r| r.to_string())
        .collect()
}

fn default_role_order() -> Vec<String> {
    DEFAULT_ROLE_ORDER.iter().map(|r| r.to_string()).collect()
}

/// One registered public key allowed to sign human assertions. There is one
/// `kid` per organization (the signing system's assertion key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssertionKey {
    /// Key id, the `kid` in the assertion header.
    pub kid: String,
    /// Base64 (standard, unpadded or padded) 32-byte Ed25519 public key.
    pub public_key: String,
    /// Roles that satisfy an `any_maintainer` item.
    #[serde(default = "default_maintainer_roles")]
    pub maintainer_roles: Vec<String>,
    /// Role ranks, lowest first. A claimed role satisfies an item that names a
    /// role of equal or lower rank.
    #[serde(default = "default_role_order")]
    pub role_order: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    pub registered_at: chrono::DateTime<chrono::Utc>,
}

impl AssertionKey {
    /// Rank of `role` in this key's order (higher outranks lower).
    pub fn rank(&self, role: &str) -> Option<usize> {
        self.role_order.iter().position(|r| r == role)
    }

    /// Does a claimed role satisfy an item that names `required`? Equal always
    /// does; otherwise the claim must rank at least as high (both known).
    pub fn role_satisfies(&self, claimed: &str, required: &str) -> bool {
        if claimed == required {
            return true;
        }
        match (self.rank(claimed), self.rank(required)) {
            (Some(c), Some(r)) => c >= r,
            _ => false,
        }
    }

    pub fn is_maintainer(&self, claimed: &str) -> bool {
        self.maintainer_roles.iter().any(|r| r == claimed)
    }

    pub fn fingerprint(&self) -> String {
        public_key_fingerprint(&self.public_key)
    }
}

/// `sha256:<hex>` of the raw public key bytes: safe to publish, never key material.
pub fn public_key_fingerprint(public_key_b64: &str) -> String {
    let raw = base64_decode(public_key_b64.trim()).unwrap_or_default();
    let digest = Sha256::digest(&raw);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyRegistryFile {
    #[serde(default)]
    keys: Vec<AssertionKey>,
}

pub fn assertion_keys_path(state_dir: &Path) -> PathBuf {
    state_dir.join(KEYS_FILENAME)
}

pub fn load_assertion_keys(state_dir: &Path) -> Result<Vec<AssertionKey>, HumanCredentialError> {
    let path = assertion_keys_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| HumanCredentialError::Registry {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    serde_json::from_str::<KeyRegistryFile>(&text)
        .map(|f| f.keys)
        .map_err(|e| HumanCredentialError::Registry {
            path,
            reason: e.to_string(),
        })
}

fn save_assertion_keys(
    state_dir: &Path,
    keys: Vec<AssertionKey>,
) -> Result<(), HumanCredentialError> {
    let json = serde_json::to_string_pretty(&KeyRegistryFile { keys })
        .map_err(|e| HumanCredentialError::Invalid(e.to_string()))?;
    write_private(&assertion_keys_path(state_dir), &json)
}

fn clean_roles(what: &str, roles: &[String]) -> Result<Vec<String>, HumanCredentialError> {
    let cleaned: Vec<String> = roles
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    if cleaned.is_empty() || cleaned.iter().any(|r| r.len() > 64) {
        return Err(HumanCredentialError::Invalid(format!(
            "{what} must list one or more role names of at most 64 characters, e.g. owner,admin"
        )));
    }
    Ok(cleaned)
}

/// Register (or replace, by `kid`) an assertion key. `maintainer_roles` and
/// `role_order` default to [`DEFAULT_MAINTAINER_ROLES`] and [`DEFAULT_ROLE_ORDER`].
pub fn add_assertion_key(
    state_dir: &Path,
    kid: &str,
    public_key_b64: &str,
    maintainer_roles: Option<&[String]>,
    role_order: Option<&[String]>,
    label: Option<&str>,
) -> Result<AssertionKey, HumanCredentialError> {
    let kid = kid.trim();
    if kid.is_empty()
        || kid.len() > 128
        || !kid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(HumanCredentialError::Invalid(
            "kid must be 1-128 characters from A-Z a-z 0-9 - _ . :".into(),
        ));
    }
    let decoded = base64_decode(public_key_b64.trim()).ok_or_else(|| {
        HumanCredentialError::Invalid(
            "public key must be base64 (standard or URL-safe alphabet) of a 32-byte Ed25519 key"
                .into(),
        )
    })?;
    if decoded.len() != 32 {
        return Err(HumanCredentialError::Invalid(format!(
            "public key decodes to {} bytes; an Ed25519 public key is 32 bytes",
            decoded.len()
        )));
    }
    let maintainer_roles = match maintainer_roles {
        Some(r) => clean_roles("--maintainer-roles", r)?,
        None => default_maintainer_roles(),
    };
    let role_order = match role_order {
        Some(r) => clean_roles("--role-order", r)?,
        None => default_role_order(),
    };
    let mut keys = load_assertion_keys(state_dir)?;
    keys.retain(|k| k.kid != kid);
    let key = AssertionKey {
        kid: kid.to_string(),
        public_key: public_key_b64.trim().to_string(),
        maintainer_roles,
        role_order,
        label: label.map(str::to_string),
        registered_at: chrono::Utc::now(),
    };
    keys.push(key.clone());
    save_assertion_keys(state_dir, keys)?;
    Ok(key)
}

/// Remove the key with this `kid`. Returns whether one was removed.
pub fn remove_assertion_key(state_dir: &Path, kid: &str) -> Result<bool, HumanCredentialError> {
    let mut keys = load_assertion_keys(state_dir)?;
    let before = keys.len();
    keys.retain(|k| k.kid != kid);
    let removed = before != keys.len();
    if removed {
        save_assertion_keys(state_dir, keys)?;
    }
    Ok(removed)
}

/// Minimal base64 decoder (standard or URL-safe alphabet, padding optional), so this crate
/// needs no extra dependency for one 32-byte key.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn login_file_custody_then_verify_and_logout() {
        let dir = tempfile::tempdir().unwrap();
        let out = login(dir.path(), false).unwrap();
        assert!(matches!(out.custody, Custody::File(_)));
        let secret = load_secret(dir.path(), false).expect("secret stored");
        assert!(secret.starts_with("tahc_"));
        assert!(verify_secret(dir.path(), &secret));
        assert!(!verify_secret(dir.path(), "tahc_wrong"));
        assert!(!verify_secret(dir.path(), ""));
        assert!(credential_present(dir.path(), false));
        assert!(logout(dir.path(), false));
        assert!(!credential_present(dir.path(), false));
        assert!(!verify_secret(dir.path(), &secret));
    }

    #[test]
    fn relogin_invalidates_the_previous_secret() {
        let dir = tempfile::tempdir().unwrap();
        login(dir.path(), false).unwrap();
        let first = load_secret(dir.path(), false).unwrap();
        login(dir.path(), false).unwrap();
        assert!(!verify_secret(dir.path(), &first));
    }

    #[cfg(unix)]
    #[test]
    fn secret_and_digest_files_are_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        login(dir.path(), false).unwrap();
        for name in [SECRET_FILENAME, DIGEST_FILENAME] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name} must be owner-only");
        }
    }

    #[test]
    fn keychain_is_never_the_default_under_test() {
        assert!(!default_use_keychain());
    }

    #[test]
    fn no_credential_means_not_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!credential_present(dir.path(), false));
        assert!(!verify_secret(dir.path(), "tahc_anything"));
    }

    #[test]
    fn detects_marker_env_vars_and_staging_cwd() {
        let env = |k: &str| (k == "CLAUDECODE").then(|| "1".to_string());
        let ctx = detect_agent_context_from(&env, Path::new("/home/u/proj"), true).unwrap();
        assert_eq!(ctx.signals, vec!["CLAUDECODE is set".to_string()]);

        let ctx = detect_agent_context_from(
            &no_env,
            Path::new("/home/u/proj/.ta/staging/abc/crates"),
            true,
        )
        .unwrap();
        assert!(ctx.signals[0].contains(".ta/staging"));

        // Windows separators are normalized.
        let ctx =
            detect_agent_context_from(&no_env, Path::new("C:\\proj\\.ta\\staging\\abc\\src"), true);
        assert!(ctx.is_some());
    }

    #[test]
    fn caller_mode_normal_is_not_a_signal_but_orchestrator_is() {
        let normal = |k: &str| (k == "TA_CALLER_MODE").then(|| "normal".to_string());
        assert!(detect_agent_context_from(&normal, Path::new("/p"), true).is_none());
        let orch = |k: &str| (k == "TA_CALLER_MODE").then(|| "orchestrator".to_string());
        assert!(detect_agent_context_from(&orch, Path::new("/p"), true).is_some());
    }

    #[test]
    fn headless_launch_is_a_signal_and_a_terminal_session_is_clean() {
        assert!(detect_agent_context_from(&no_env, Path::new("/p"), false).is_some());
        assert!(detect_agent_context_from(&no_env, Path::new("/p"), true).is_none());
    }

    #[test]
    fn refusal_message_names_login_and_the_human_path() {
        let ctx = AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        };
        let msg = refusal_message("approve", &ctx);
        assert!(msg.contains("ta login"));
        assert!(msg.contains("! ta draft approve <id>"));
        assert!(msg.contains("advisory"));
        assert!(!msg.contains('\u{2014}'));
    }

    #[test]
    fn assertion_key_registry_round_trip_with_defaults_and_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let key = "A".repeat(43); // 32 bytes of zero in unpadded base64
        let k = add_assertion_key(dir.path(), "wf-1", &key, None, None, None).unwrap();
        assert_eq!(k.kid, "wf-1");
        assert_eq!(k.maintainer_roles, vec!["admin", "owner"]);
        assert_eq!(k.role_order, vec!["member", "admin", "owner"]);
        assert!(k.fingerprint().starts_with("sha256:"));
        assert!(!k.fingerprint().contains(&key));
        assert_eq!(load_assertion_keys(dir.path()).unwrap().len(), 1);
        // Re-adding the same kid replaces it.
        let roles = vec!["owner".to_string()];
        add_assertion_key(dir.path(), "wf-1", &key, Some(&roles), None, None).unwrap();
        let keys = load_assertion_keys(dir.path()).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].maintainer_roles, vec!["owner"]);
        assert!(add_assertion_key(dir.path(), "wf-1", "not base64!", None, None, None).is_err());
        assert!(add_assertion_key(dir.path(), "wf-1", "AAAA", None, None, None).is_err());
        assert!(add_assertion_key(dir.path(), "bad kid", &key, None, None, None).is_err());
        assert!(remove_assertion_key(dir.path(), "wf-1").unwrap());
        assert!(!remove_assertion_key(dir.path(), "wf-1").unwrap());
        assert!(load_assertion_keys(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn role_rank_is_at_least_and_unknown_roles_need_equality() {
        let dir = tempfile::tempdir().unwrap();
        let key = "A".repeat(43);
        let k = add_assertion_key(dir.path(), "k", &key, None, None, None).unwrap();
        assert!(k.role_satisfies("owner", "admin"));
        assert!(k.role_satisfies("admin", "admin"));
        assert!(k.role_satisfies("owner", "member"));
        assert!(!k.role_satisfies("member", "admin"));
        assert!(!k.role_satisfies("superuser", "admin"));
        assert!(k.role_satisfies("custom", "custom"));
        assert!(!k.role_satisfies("owner", "custom"));
        assert!(k.is_maintainer("admin") && k.is_maintainer("owner") && !k.is_maintainer("member"));
    }

    #[test]
    fn install_id_is_generated_once_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let a = install_id(dir.path()).unwrap();
        assert!(a.starts_with("ta_install_") && a.len() > 20);
        assert_eq!(install_id(dir.path()).unwrap(), a);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(install_id(other.path()).unwrap(), a);
    }

    #[test]
    fn corrupt_registry_is_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(assertion_keys_path(dir.path()), "{not json").unwrap();
        let err = load_assertion_keys(dir.path()).unwrap_err().to_string();
        assert!(err.contains("ta keys add"));
    }

    #[test]
    fn base64_decode_matches_known_vectors() {
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
        assert!(base64_decode("TW*u").is_none());
    }

    // ── agent delegation ──

    fn agent() -> AgentContext {
        AgentContext {
            signals: vec!["CLAUDECODE is set".into(), "TA_AGENT_ID is set".into()],
        }
    }

    #[test]
    fn agent_sighting_round_trips_with_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        assert!(last_agent_context_seen(dir.path()).is_none());
        record_agent_context_seen_at(dir.path(), &agent(), now).unwrap();
        let s = last_agent_context_seen(dir.path()).unwrap();
        assert_eq!(s.signals, agent().signals);
        assert!((s.at - now).num_seconds().abs() <= 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("agent-context-seen"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn seen_within_respects_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        record_agent_context_seen_at(dir.path(), &agent(), now - chrono::Duration::hours(1))
            .unwrap();
        let day = chrono::Duration::hours(24);
        assert!(agent_context_seen_within(dir.path(), day, now).is_some());
        record_agent_context_seen_at(dir.path(), &agent(), now - chrono::Duration::hours(25))
            .unwrap();
        assert!(agent_context_seen_within(dir.path(), day, now).is_none());
    }

    #[test]
    fn delegation_line_needs_credential_and_a_recent_sighting() {
        let now = chrono::Utc::now();
        let recent = AgentSighting {
            at: now - chrono::Duration::hours(1),
            signals: agent().signals,
        };
        let old = AgentSighting {
            at: now - chrono::Duration::hours(25),
            signals: vec![],
        };
        let line = delegation_status_line(true, Some(&recent), now).unwrap();
        assert!(line.starts_with("agent delegation active"), "{line}");
        assert!(line.contains("1 hour(s) ago"), "{line}");
        assert!(line.contains("CLAUDECODE is set"), "{line}");
        assert!(line.contains("ta login logout"), "{line}");
        assert!(line.contains("Wayfinder-only"), "{line}");
        assert!(delegation_status_line(false, Some(&recent), now).is_none());
        assert!(delegation_status_line(true, Some(&old), now).is_none());
        assert!(delegation_status_line(true, None, now).is_none());
    }

    #[test]
    fn credential_holder_is_never_empty() {
        assert!(!credential_holder().is_empty());
    }

    #[test]
    fn actor_header_round_trips_and_is_url_safe() {
        let ctx = AgentContext {
            signals: vec![
                "CLAUDECODE is set".into(),
                "no interactive terminal (headless launch)".into(),
            ],
        };
        let v = actor_context_header_value(&ctx);
        assert!(v.starts_with("agent_delegated; reasons="), "{v}");
        assert!(!v["agent_delegated; ".len()..].contains(' '), "{v}");
        let back = parse_actor_context_header(&v).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], "CLAUDECODE is set");
    }

    #[test]
    fn actor_header_is_capped_on_both_ends() {
        let ctx = AgentContext {
            signals: (0..20).map(|i| format!("{i}{}", "x".repeat(200))).collect(),
        };
        let v = actor_context_header_value(&ctx);
        assert!(v.len() <= MAX_ACTOR_HEADER_BYTES, "{}", v.len());
        let back = parse_actor_context_header(&v).unwrap();
        assert!(back.len() <= MAX_ACTOR_REASONS);
        assert!(back
            .iter()
            .all(|r| r.chars().count() <= MAX_ACTOR_REASON_CHARS));
    }

    #[test]
    fn malformed_actor_headers_are_rejected_with_a_reason() {
        for bad in [
            "human_direct",
            "agent_delegated; nope=1",
            "agent_delegated; reasons=%ZZ",
            "agent_delegated; reasons=%FF%FE",
            "agent_delegated; reasons=a%0Ab",
            "",
        ] {
            assert!(parse_actor_context_header(bad).is_err(), "{bad}");
        }
        let big = format!("agent_delegated; reasons={}", "a".repeat(600));
        assert!(parse_actor_context_header(&big)
            .unwrap_err()
            .contains("over the"));
        assert_eq!(parse_actor_context_header("agent_delegated"), Ok(vec![]));
    }

    #[test]
    fn env_markers_match_the_cli_detection() {
        let env = |k: &str| (k == "CLAUDECODE").then(|| "1".to_string());
        assert_eq!(agent_markers_from_env(&env), vec!["CLAUDECODE is set"]);
        assert!(agent_markers_from_env(&|_| None).is_empty());
    }
}
