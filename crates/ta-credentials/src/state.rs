// state.rs: the daemon-only per-user state directory for control-plane secrets.
//
// TA's root key (signs capability tokens), audit key (signs the audit log),
// daemon API tokens and the NATS JetStream data used to live under
// `<project>/.ta/`, two folders from every agent's staging workspace. They now
// live OUTSIDE every project tree:
//
//   macOS    ~/Library/Application Support/TrustedAutonomy/projects/<project-id>/
//   Linux    $XDG_STATE_HOME/trusted-autonomy/projects/<project-id>/
//            (default ~/.local/state/trusted-autonomy/projects/<project-id>/)
//   Windows  %LOCALAPPDATA%\TrustedAutonomy\projects\<project-id>\
//
// `<project-id>` is a stable id minted once per project and recorded in
// `<project>/.ta/project-id` (non-secret). Two projects never share a state
// directory: a copied project (same id file, original still on disk) mints a
// fresh id, a moved project (original path gone) keeps its keys.
//
// Key custody reuses the CR-02 scheme from `human.rs`: the OS keychain where
// one is usable, otherwise a mode-0600 file in the state directory with a loud
// warning. `TA_NO_KEYCHAIN` and the cargo-test guard apply exactly as for the
// human credential and the vault.
//
// HONEST LIMITS. A same-user process that is not sandboxed can still read the
// state directory. What this phase buys is that the secrets are no longer
// reachable with `../../` from a staging workspace, and are absent from
// backups of the project tree. The OS sandbox phases (v0.17.12.6 onward) deny
// the state directory itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::human::{self, Custody};

/// Overrides the root that holds `projects/<project-id>/` (tests, containers,
/// installers that pre-seed a state directory).
pub const STATE_ROOT_ENV: &str = "TA_STATE_DIR";
/// Selects the posture (`strict` or `default`) without a file.
pub const POSTURE_ENV: &str = "TA_POSTURE";
/// File name (inside the state root) that selects the posture when the
/// environment variable is unset.
pub const POSTURE_FILENAME: &str = "posture";
/// File (inside `<project>/.ta/`) that holds the non-secret project id.
pub const PROJECT_ID_FILENAME: &str = "project-id";
/// Marker (inside the project state directory) naming the project path that owns it.
const PROJECT_PATH_MARKER: &str = "project-path";
use crate::keychain::CONTROL_PLANE_SERVICE as KEYRING_SERVICE;
const SECRETS_DIRNAME: &str = "secrets";

/// Errors from state-directory handling. Every message says what was being
/// attempted and what to do next.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(
        "could not use the TA state directory {path}: {source}. Check that the directory is \
         writable by your user, or set {STATE_ROOT_ENV} to a directory you own."
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "{what} is held in the OS keychain ({item}) but the keychain could not be read: \
         {reason}. TA will not create a replacement key over it. Unlock the keychain, \
         approve the TA access prompt (choose Always Allow), then run `ta daemon restart` \
         (or re-run your command). To use file custody instead, set TA_NO_KEYCHAIN=1."
    )]
    KeychainUnavailable {
        what: String,
        item: String,
        reason: String,
    },
    #[error(
        "the old-location secret {path} ({what}) is still inside the project tree and the \
         strict posture refuses it. A copy inside the project is reachable by any agent with \
         `../../`. Run `ta doctor --fix` to move it to {new_home} (the copy is verified before \
         the old one is removed), then retry."
    )]
    LegacyRefused {
        path: PathBuf,
        what: String,
        new_home: String,
    },
    #[error(
        "could not store {what} for project {project_id}: {reason}. Nothing was changed. Check \
         that {dir} is writable, or set {STATE_ROOT_ENV} to a directory you own."
    )]
    Store {
        what: String,
        project_id: String,
        dir: PathBuf,
        reason: String,
    },
    #[error(
        "{what} at {path} is unreadable or corrupt: {reason}. Restore it from a backup, or, if \
         it can be regenerated, delete it so a fresh one is created (existing signatures made \
         with the old value will no longer verify)."
    )]
    Corrupt {
        what: String,
        path: PathBuf,
        reason: String,
    },
    #[error(
        "secrets are not available from a staging workspace ({path}). Control-plane keys belong \
         to the real project; run this command from the project root instead."
    )]
    StagingWorkspace { path: PathBuf },
}

impl From<human::HumanCredentialError> for StateError {
    fn from(e: human::HumanCredentialError) -> Self {
        match e {
            human::HumanCredentialError::Write { path, source } => StateError::Io { path, source },
            other => StateError::Corrupt {
                what: "state file".into(),
                path: PathBuf::new(),
                reason: other.to_string(),
            },
        }
    }
}

// ── Locations ──────────────────────────────────────────────────────────────

/// Operating system families with distinct state-directory conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// Pure computation of the state root for an OS, given an environment lookup
/// and a home directory. Separated from the process environment so every OS
/// is testable on every host.
pub fn state_root_for(
    os: Os,
    env: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = env(STATE_ROOT_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = home.map(Path::to_path_buf).unwrap_or_else(|| ".".into());
    match os {
        Os::MacOs => home
            .join("Library")
            .join("Application Support")
            .join("TrustedAutonomy"),
        Os::Linux => env("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"))
            .join("trusted-autonomy"),
        Os::Windows => env("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("TrustedAutonomy"),
    }
}

/// Where state lives for the running process. Inside a cargo test binary with
/// no override this is a per-process directory under the system temp dir, so
/// a test can never write into the developer's real state directory.
#[derive(Debug, Clone)]
pub struct StateLocator {
    root: PathBuf,
}

impl StateLocator {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let explicit = env(STATE_ROOT_ENV).filter(|v| !v.is_empty());
        if explicit.is_none() && crate::encryption::keychain_guard_active() {
            return Self {
                root: std::env::temp_dir()
                    .join(format!("ta-test-state-{}", std::process::id()))
                    .join("TrustedAutonomy"),
            };
        }
        Self {
            root: state_root_for(Os::current(), &env, dirs::home_dir().as_deref()),
        }
    }

    /// A locator rooted at an explicit directory (tests, installers).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }
}

fn canonical_text(project_root: &Path) -> String {
    let canon = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    canon.to_string_lossy().replace('\\', "/")
}

fn is_staging_path(project_root: &Path) -> bool {
    let n = project_root.to_string_lossy().replace('\\', "/");
    n.contains("/.ta/staging/") || n.ends_with("/.ta/staging")
}

fn valid_project_id(id: &str) -> bool {
    id.len() == 34
        && id.starts_with("p_")
        && id[2..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mint_project_id(canonical: &str) -> String {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    h.update([0u8]);
    h.update(salt);
    format!("p_{}", &hex(&h.finalize())[..32])
}

/// A project's daemon-only state directory.
#[derive(Debug, Clone)]
pub struct ProjectState {
    pub project_root: PathBuf,
    pub project_id: String,
    pub dir: PathBuf,
}

enum IdDecision {
    /// Keep this id; `write_marker` says whether the marker needs (re)writing.
    Use { id: String, write_marker: bool },
    /// Mint a new id.
    Mint,
}

impl ProjectState {
    /// Resolve the project's state directory, creating the id and directory
    /// on first use. Refuses from a staging workspace.
    pub fn open(project_root: &Path, locator: &StateLocator) -> Result<Self, StateError> {
        if is_staging_path(project_root) {
            return Err(StateError::StagingWorkspace {
                path: project_root.to_path_buf(),
            });
        }
        Self::resolve(project_root, locator, true).map(|o| o.expect("create=true yields Some"))
    }

    /// Resolve without creating anything: `None` when the project has no
    /// state directory yet (nothing was ever stored for it).
    pub fn locate(project_root: &Path, locator: &StateLocator) -> Result<Option<Self>, StateError> {
        Self::resolve(project_root, locator, false)
    }

    fn resolve(
        project_root: &Path,
        locator: &StateLocator,
        create: bool,
    ) -> Result<Option<Self>, StateError> {
        let canonical = canonical_text(project_root);
        let id_file = project_root.join(".ta").join(PROJECT_ID_FILENAME);
        let recorded = std::fs::read_to_string(&id_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| valid_project_id(s));

        let decision = match recorded {
            None => IdDecision::Mint,
            Some(id) => {
                let marker = locator.projects_dir().join(&id).join(PROJECT_PATH_MARKER);
                match std::fs::read_to_string(&marker) {
                    // Id file present but this machine has no record of it
                    // (fresh clone, wiped state dir): claim it.
                    Err(_) => IdDecision::Use {
                        id,
                        write_marker: true,
                    },
                    Ok(owner) => {
                        let owner = owner.trim();
                        if owner == canonical {
                            IdDecision::Use {
                                id,
                                write_marker: false,
                            }
                        } else if Path::new(owner).exists() {
                            // The recorded owner is still on disk: this tree
                            // is a COPY. It must not share keys with it.
                            IdDecision::Mint
                        } else {
                            // The recorded owner is gone: the project moved.
                            IdDecision::Use {
                                id,
                                write_marker: true,
                            }
                        }
                    }
                }
            }
        };

        let (id, write_marker, minted) = match decision {
            IdDecision::Use { id, write_marker } => (id, write_marker, false),
            IdDecision::Mint => {
                if !create {
                    return Ok(None);
                }
                (mint_project_id(&canonical), true, true)
            }
        };
        let dir = locator.projects_dir().join(&id);
        if !create {
            return Ok(dir.is_dir().then(|| Self {
                project_root: project_root.to_path_buf(),
                project_id: id,
                dir,
            }));
        }
        std::fs::create_dir_all(&dir).map_err(|source| StateError::Io {
            path: dir.clone(),
            source,
        })?;
        if write_marker {
            human_write(&dir.join(PROJECT_PATH_MARKER), &canonical)?;
        }
        if minted {
            let id_dir = project_root.join(".ta");
            std::fs::create_dir_all(&id_dir).map_err(|source| StateError::Io {
                path: id_dir.clone(),
                source,
            })?;
            std::fs::write(&id_file, format!("{id}\n")).map_err(|source| StateError::Io {
                path: id_file.clone(),
                source,
            })?;
            tracing::info!(
                project_id = %id,
                state_dir = %dir.display(),
                "created the daemon-only state directory for this project"
            );
        }
        Ok(Some(Self {
            project_root: project_root.to_path_buf(),
            project_id: id,
            dir,
        }))
    }

    pub fn secrets_dir(&self) -> PathBuf {
        self.dir.join(SECRETS_DIRNAME)
    }
    pub fn daemon_tokens_path(&self) -> PathBuf {
        self.dir.join("daemon-tokens.json")
    }
    pub fn tokens_dir(&self) -> PathBuf {
        self.dir.join("tokens")
    }
    pub fn nats_data_dir(&self) -> PathBuf {
        self.dir.join("nats-jetstream")
    }
    pub fn checkpoints_path(&self) -> PathBuf {
        self.dir.join("audit-checkpoints.jsonl")
    }
    pub fn hashes_path(&self) -> PathBuf {
        self.dir.join("config-hashes.json")
    }
    /// Durable action queue (v0.17.12.3). Daemon-only: outside the project
    /// tree and every staging workspace, so an agent cannot write or forge it.
    pub fn action_queue_dir(&self) -> PathBuf {
        self.dir.join("action-queue")
    }
    /// Two-phase apply journals (v0.17.12.3), same custody as the queue.
    pub fn apply_journal_dir(&self) -> PathBuf {
        self.dir.join("apply-journal")
    }
    /// Rules trust anchor and signed snapshots (v0.17.12.4): the owner's
    /// public key, the last signed copy of the rules file, and the log of
    /// auto-applied (untrusted) content. Outside the project tree and every
    /// staging workspace, so an agent cannot replace the key it is checked by.
    pub fn rules_dir(&self) -> PathBuf {
        self.dir.join("rules")
    }
    /// Trusted sandbox policy (v0.17.12.6): the owner's `policy.toml` that
    /// the launcher compiles the sandbox profile from. Outside the project
    /// tree and every staging workspace, so an agent cannot edit the box it
    /// is launched into; project config may only tighten it.
    pub fn sandbox_dir(&self) -> PathBuf {
        self.dir.join("sandbox")
    }
    /// Per-goal agent homes (v0.17.12.6): `<dir>/<goal-id>/` is the only
    /// state-dir path a sandboxed agent may write, and nothing in it
    /// outlives the goal.
    pub fn agent_homes_dir(&self) -> PathBuf {
        self.dir.join("agent-homes")
    }
}

fn human_write(path: &Path, contents: &str) -> Result<(), StateError> {
    human::write_private(path, contents).map_err(StateError::from)
}

// ── Posture ────────────────────────────────────────────────────────────────

/// How hard TA is on old-location secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Posture {
    /// Keep reading an old-location secret, with a loud warning.
    #[default]
    Default,
    /// Refuse any old-location secret.
    Strict,
}

impl Posture {
    pub fn parse(text: &str) -> Posture {
        if text.trim().eq_ignore_ascii_case("strict") {
            Posture::Strict
        } else {
            Posture::Default
        }
    }

    /// `TA_POSTURE`, then the `posture` file in the state root (which lives
    /// outside every project, so an agent cannot loosen it from staging).
    /// The owner rules file (a later phase) supersedes this interim switch.
    pub fn resolve(locator: &StateLocator) -> Posture {
        if let Ok(v) = std::env::var(POSTURE_ENV) {
            if !v.trim().is_empty() {
                return Posture::parse(&v);
            }
        }
        std::fs::read_to_string(locator.root().join(POSTURE_FILENAME))
            .map(|s| Posture::parse(&s))
            .unwrap_or_default()
    }
}

// ── Named secrets ──────────────────────────────────────────────────────────

/// The control-plane secrets held as single values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSecret {
    /// Biscuit root key that signs every capability token.
    BrokerRootKey,
    /// HMAC key that signs the audit log and its checkpoints.
    AuditKey,
    /// The credential vault's own age identity (`.ta/credentials.key` before
    /// it moved). Whoever holds it can decrypt `.ta/credentials.json`, so it
    /// must not sit next to the vault in the project tree. The vault tries
    /// the OS keychain itself (service `trusted-autonomy-vault`); this entry
    /// is its owner-only file fallback, always a file, never the control-plane
    /// keychain service.
    VaultKey,
    /// Ed25519 seed (base64) of the project owner's rules-signing key
    /// (v0.17.12.4). It is NOT in [`ControlSecret::ALL`]: it never lived in the
    /// project tree, only `ta rules sign` ever reads it, and the daemon verifies
    /// with the public half in the state directory instead.
    RulesOwnerKey,
}

impl ControlSecret {
    pub const ALL: [ControlSecret; 3] = [
        ControlSecret::BrokerRootKey,
        ControlSecret::AuditKey,
        ControlSecret::VaultKey,
    ];

    /// Custody options this secret really uses. The vault key is always held
    /// in an owner-only file here: the keychain tier for it is the vault's
    /// own, in `encryption.rs`.
    pub fn custody(self, opts: CustodyOpts) -> CustodyOpts {
        match self {
            ControlSecret::VaultKey => CustodyOpts {
                use_keychain: false,
                ..opts
            },
            _ => opts,
        }
    }

    /// Name of the keychain user and of the fallback file in `secrets/`.
    pub fn name(self) -> &'static str {
        match self {
            ControlSecret::BrokerRootKey => "broker-root-key",
            ControlSecret::AuditKey => "audit-key",
            ControlSecret::VaultKey => "vault-key",
            ControlSecret::RulesOwnerKey => "rules-owner-key",
        }
    }
    /// File name the secret had inside `<project>/.ta/`.
    pub fn legacy_filename(self) -> &'static str {
        match self {
            ControlSecret::BrokerRootKey => "broker_root.key",
            ControlSecret::AuditKey => "audit.key",
            ControlSecret::VaultKey => "credentials.key",
            ControlSecret::RulesOwnerKey => "rules_owner.key",
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            ControlSecret::BrokerRootKey => "the capability-token root key",
            ControlSecret::AuditKey => "the audit-log signing key",
            ControlSecret::VaultKey => "the credential vault's encryption key",
            ControlSecret::RulesOwnerKey => "the project owner's rules-signing key",
        }
    }
}

/// Custody options: whether the keychain may be used and how strict to be.
#[derive(Debug, Clone, Copy)]
pub struct CustodyOpts {
    pub use_keychain: bool,
    pub posture: Posture,
}

impl CustodyOpts {
    pub fn from_env(locator: &StateLocator) -> Self {
        Self {
            use_keychain: human::default_use_keychain(),
            posture: Posture::resolve(locator),
        }
    }
    /// File custody and the default posture: what tests use.
    pub fn file_only() -> Self {
        Self {
            use_keychain: false,
            posture: Posture::Default,
        }
    }
    pub fn strict(mut self) -> Self {
        self.posture = Posture::Strict;
        self
    }
}

/// Where a loaded secret came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOrigin {
    Keychain,
    StateFile(PathBuf),
    /// The old in-project location (default posture only).
    Legacy(PathBuf),
}

impl SecretOrigin {
    pub fn describe(&self) -> String {
        match self {
            SecretOrigin::Keychain => "the OS keychain".to_string(),
            SecretOrigin::StateFile(p) => p.display().to_string(),
            SecretOrigin::Legacy(p) => format!("{} (old in-project location)", p.display()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSecret {
    pub value: String,
    pub origin: SecretOrigin,
}

/// Keychain item name (the "user" half of service/user) for a secret.
fn keychain_user(ps: &ProjectState, which: ControlSecret) -> String {
    format!("{}/{}", ps.project_id, which.name())
}

fn secret_file(ps: &ProjectState, which: ControlSecret) -> PathBuf {
    ps.secrets_dir().join(which.name())
}

fn legacy_secret_file(ps: &ProjectState, which: ControlSecret) -> PathBuf {
    ps.project_root.join(".ta").join(which.legacy_filename())
}

/// Emit a legacy-location warning once per process per path: on stderr (so a
/// CLI user sees it without a log subscriber) and as a structured log.
fn warn_legacy_once(path: &Path, what: &str, new_home: &str) {
    static SEEN: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = seen
        .lock()
        .map(|mut s| s.insert(path.to_path_buf()))
        .unwrap_or(true);
    if !first {
        return;
    }
    tracing::warn!(
        path = %path.display(),
        secret = what,
        new_home,
        "control-plane secret is still in the project tree; run `ta doctor --fix` to move it"
    );
    eprintln!(
        "[warn] {what} is still stored inside the project at {}. Any agent that can run `cat ../../` \
         can read it. It now belongs in {new_home}. Run `ta doctor --fix` to move it safely \
         (the copy is verified before the old one is removed).",
        path.display()
    );
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read a secret from its NEW home only (keychain, then state file).
///
/// A keychain that could not be read (as opposed to one that holds no such
/// item) is reported through `keychain_error`, so the caller never mistakes
/// "locked or denied" for "does not exist yet" and generates a replacement
/// key over the real one.
fn read_new_home(
    ps: &ProjectState,
    which: ControlSecret,
    opts: CustodyOpts,
) -> (Option<LoadedSecret>, Option<String>) {
    let opts = which.custody(opts);
    let mut keychain_error = None;
    if opts.use_keychain {
        match crate::keychain::get(KEYRING_SERVICE, &keychain_user(ps, which)) {
            Ok(Some(v)) => {
                return (
                    Some(LoadedSecret {
                        value: v,
                        origin: SecretOrigin::Keychain,
                    }),
                    None,
                )
            }
            Ok(None) => {}
            Err(e) => keychain_error = Some(e),
        }
    }
    let path = secret_file(ps, which);
    let found = read_trimmed(&path).map(|value| LoadedSecret {
        value,
        origin: SecretOrigin::StateFile(path),
    });
    (found, keychain_error)
}

/// Read a secret from its NEW home only (keychain, then state file).
pub fn load_secret_new(
    ps: &ProjectState,
    which: ControlSecret,
    opts: CustodyOpts,
) -> Option<LoadedSecret> {
    read_new_home(ps, which, opts).0
}

fn new_home_text(ps: &ProjectState, which: ControlSecret, opts: CustodyOpts) -> String {
    if opts.use_keychain {
        format!(
            "the OS keychain (service {KEYRING_SERVICE}, item {}/{})",
            ps.project_id,
            which.name()
        )
    } else {
        format!("{}", secret_file(ps, which).display())
    }
}

/// Load a secret: new home first, then (default posture only) the old
/// in-project file with a loud warning. The strict posture refuses an
/// old-location copy even when a new one exists, since the leftover is still
/// reachable by an agent.
pub fn load_secret(
    ps: &ProjectState,
    which: ControlSecret,
    opts: CustodyOpts,
) -> Result<Option<LoadedSecret>, StateError> {
    let opts = which.custody(opts);
    let legacy_path = legacy_secret_file(ps, which);
    let legacy_exists = legacy_path.is_file();
    if legacy_exists && opts.posture == Posture::Strict {
        return Err(StateError::LegacyRefused {
            path: legacy_path,
            what: which.describe().to_string(),
            new_home: new_home_text(ps, which, opts),
        });
    }
    let (found, keychain_error) = read_new_home(ps, which, opts);
    if let Some(found) = found {
        if legacy_exists {
            warn_legacy_once(
                &legacy_path,
                &format!("a stale copy of {}", which.describe()),
                &new_home_text(ps, which, opts),
            );
        }
        return Ok(Some(found));
    }
    if let Some(reason) = keychain_error {
        if !legacy_exists {
            return Err(StateError::KeychainUnavailable {
                what: which.describe().to_string(),
                item: format!("{KEYRING_SERVICE}/{}", keychain_user(ps, which)),
                reason,
            });
        }
    }
    if legacy_exists {
        warn_legacy_once(
            &legacy_path,
            which.describe(),
            &new_home_text(ps, which, opts),
        );
        return Ok(read_trimmed(&legacy_path).map(|value| LoadedSecret {
            value,
            origin: SecretOrigin::Legacy(legacy_path),
        }));
    }
    Ok(None)
}

/// Where a stored secret landed, plus the keychain failure reason when the
/// keychain was wanted but unusable.
#[derive(Debug, Clone)]
pub struct StoreOutcome {
    pub custody: Custody,
    pub keychain_fallback_reason: Option<String>,
}

/// Store a secret in its new home. Keychain first when allowed, otherwise a
/// mode-0600 file in the state directory, with a loud warning.
pub fn store_secret(
    ps: &ProjectState,
    which: ControlSecret,
    value: &str,
    opts: CustodyOpts,
) -> Result<StoreOutcome, StateError> {
    let opts = which.custody(opts);
    let file = secret_file(ps, which);
    let mut reason = None;
    if opts.use_keychain {
        match crate::keychain::set(KEYRING_SERVICE, &keychain_user(ps, which), value) {
            Ok(()) => {
                // A stale fallback file must not outlive the keychain item.
                let _ = std::fs::remove_file(&file);
                return Ok(StoreOutcome {
                    custody: Custody::Keychain,
                    keychain_fallback_reason: None,
                });
            }
            Err(e) => reason = Some(e),
        }
    }
    human::write_private(&file, value).map_err(|e| StateError::Store {
        what: which.describe().to_string(),
        project_id: ps.project_id.clone(),
        dir: ps.secrets_dir(),
        reason: e.to_string(),
    })?;
    if opts.use_keychain {
        tracing::warn!(
            path = %file.display(),
            secret = which.name(),
            reason = reason.as_deref().unwrap_or("unknown"),
            "OS keychain unusable; control-plane secret stored in an owner-only file instead"
        );
        eprintln!(
            "[warn] The OS keychain was not usable ({}); {} is stored in {} (owner-only \
             permissions). Set {} to silence this on machines without a keychain.",
            reason.as_deref().unwrap_or("unknown reason"),
            which.describe(),
            file.display(),
            crate::encryption::NO_KEYCHAIN_ENV
        );
    }
    Ok(StoreOutcome {
        custody: Custody::File(file),
        keychain_fallback_reason: reason,
    })
}

/// Load a secret or create it with `generate` and store it in the new home.
/// Returns the secret and whether it was just created.
pub fn load_or_create_secret(
    ps: &ProjectState,
    which: ControlSecret,
    opts: CustodyOpts,
    generate: impl FnOnce() -> String,
) -> Result<(LoadedSecret, bool), StateError> {
    if let Some(found) = load_secret(ps, which, opts)? {
        return Ok((found, false));
    }
    let value = generate();
    let outcome = store_secret(ps, which, &value, opts)?;
    let origin = match outcome.custody {
        Custody::Keychain => SecretOrigin::Keychain,
        Custody::File(p) => SecretOrigin::StateFile(p),
    };
    Ok((LoadedSecret { value, origin }, true))
}

/// Remove a secret from its new home (keychain item and fallback file).
pub fn remove_secret_new(ps: &ProjectState, which: ControlSecret, opts: CustodyOpts) -> bool {
    let opts = which.custody(opts);
    let mut removed = std::fs::remove_file(secret_file(ps, which)).is_ok();
    if opts.use_keychain {
        removed |=
            crate::keychain::delete(KEYRING_SERVICE, &keychain_user(ps, which)).unwrap_or(false);
    }
    removed
}

// ── Moved files and directories ────────────────────────────────────────────

/// The control-plane files and directories that move to the state directory
/// as-is (they are not single secret values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedItem {
    DaemonTokens,
    Tokens,
    NatsData,
}

impl MovedItem {
    pub const ALL: [MovedItem; 3] = [
        MovedItem::DaemonTokens,
        MovedItem::Tokens,
        MovedItem::NatsData,
    ];
    pub fn legacy_name(self) -> &'static str {
        match self {
            MovedItem::DaemonTokens => "daemon-tokens.json",
            MovedItem::Tokens => "tokens",
            MovedItem::NatsData => "nats-jetstream",
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            MovedItem::DaemonTokens => "the daemon API tokens",
            MovedItem::Tokens => "the issued API tokens",
            MovedItem::NatsData => "the NATS JetStream data",
        }
    }
    pub fn new_path(self, ps: &ProjectState) -> PathBuf {
        match self {
            MovedItem::DaemonTokens => ps.daemon_tokens_path(),
            MovedItem::Tokens => ps.tokens_dir(),
            MovedItem::NatsData => ps.nats_data_dir(),
        }
    }
    pub fn legacy_path(self, ps: &ProjectState) -> PathBuf {
        ps.project_root.join(".ta").join(self.legacy_name())
    }
    pub fn is_dir(self) -> bool {
        !matches!(self, MovedItem::DaemonTokens)
    }
}

/// Resolve where to read or write a moved item right now. The new home when
/// it exists or nothing old exists; the old location (with a warning) only in
/// the default posture and only when the new home has nothing.
pub fn resolve_moved_path(
    ps: &ProjectState,
    item: MovedItem,
    posture: Posture,
) -> Result<PathBuf, StateError> {
    let new = item.new_path(ps);
    let legacy = item.legacy_path(ps);
    let legacy_exists = legacy.exists();
    if legacy_exists && posture == Posture::Strict {
        return Err(StateError::LegacyRefused {
            path: legacy,
            what: item.describe().to_string(),
            new_home: new.display().to_string(),
        });
    }
    if new.exists() || !legacy_exists {
        return Ok(new);
    }
    warn_legacy_once(&legacy, item.describe(), &new.display().to_string());
    Ok(legacy)
}

// ── Startup posture check ──────────────────────────────────────────────────

/// One control-plane secret still sitting inside the project tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyFinding {
    pub what: String,
    pub path: PathBuf,
}

/// Every old-location control-plane secret that still exists for the project.
pub fn legacy_findings(project_root: &Path) -> Vec<LegacyFinding> {
    let ta = project_root.join(".ta");
    let mut out = Vec::new();
    for which in ControlSecret::ALL {
        let p = ta.join(which.legacy_filename());
        if p.is_file() {
            out.push(LegacyFinding {
                what: which.describe().to_string(),
                path: p,
            });
        }
    }
    for item in MovedItem::ALL {
        let p = ta.join(item.legacy_name());
        if p.exists() {
            out.push(LegacyFinding {
                what: item.describe().to_string(),
                path: p,
            });
        }
    }
    out
}

/// Daemon startup check. In the default posture each old-location secret is
/// warned about; under `strict` the daemon must refuse to start, and this
/// returns the refusal text (what, where, and the fix).
pub fn startup_check(project_root: &Path, posture: Posture) -> Result<(), String> {
    let findings = legacy_findings(project_root);
    if findings.is_empty() {
        return Ok(());
    }
    let list = findings
        .iter()
        .map(|f| format!("{} ({})", f.path.display(), f.what))
        .collect::<Vec<_>>()
        .join("; ");
    match posture {
        Posture::Strict => Err(format!(
            "the strict posture refuses control-plane secrets inside the project tree: {list}. \
             Any agent can read these with `../../`. Run `ta doctor --fix` to move them to the TA \
             state directory (copies are verified before the old ones are removed), then start \
             the daemon again."
        )),
        Posture::Default => {
            tracing::warn!(
                project = %project_root.display(),
                "control-plane secrets are still inside the project tree: {list}. Run `ta doctor --fix` to move them out"
            );
            Ok(())
        }
    }
}

// ── Migration ──────────────────────────────────────────────────────────────

/// What happened to one item during migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStatus {
    /// Copied to the new home, verified, old copy removed.
    Migrated,
    /// The new home already held an identical copy; the stale old copy was removed.
    StaleCopyRemoved,
    /// Nothing at the old location (already migrated, or never existed).
    NothingToMigrate,
    /// The new home holds a DIFFERENT value. Both were kept.
    Conflict(String),
    /// Something failed. The old copy was left untouched.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct MigrationItem {
    pub name: String,
    pub from: PathBuf,
    pub to: String,
    pub status: MigrationStatus,
}

#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    pub items: Vec<MigrationItem>,
}

impl MigrationReport {
    pub fn changed(&self) -> bool {
        self.items.iter().any(|i| {
            matches!(
                i.status,
                MigrationStatus::Migrated | MigrationStatus::StaleCopyRemoved
            )
        })
    }
    pub fn problems(&self) -> Vec<&MigrationItem> {
        self.items
            .iter()
            .filter(|i| {
                matches!(
                    i.status,
                    MigrationStatus::Conflict(_) | MigrationStatus::Failed(_)
                )
            })
            .collect()
    }
}

/// Move every old-location secret, file and directory to the state
/// directory. A copy is verified (re-read from the NEW home and compared)
/// before the old one is removed; anything that cannot be verified is left in
/// place. Safe to re-run: a second run reports `NothingToMigrate`.
pub fn migrate_project(ps: &ProjectState, opts: CustodyOpts) -> MigrationReport {
    let mut report = MigrationReport::default();
    for which in ControlSecret::ALL {
        report.items.push(migrate_secret(ps, which, opts));
    }
    for item in MovedItem::ALL {
        report.items.push(migrate_moved(ps, item));
    }
    report
}

fn migrate_secret(ps: &ProjectState, which: ControlSecret, opts: CustodyOpts) -> MigrationItem {
    let opts = which.custody(opts);
    let from = legacy_secret_file(ps, which);
    let to = new_home_text(ps, which, opts);
    let mut item = MigrationItem {
        name: which.name().to_string(),
        from: from.clone(),
        to,
        status: MigrationStatus::NothingToMigrate,
    };
    if !from.is_file() {
        return item;
    }
    let Some(old_value) = read_trimmed(&from) else {
        item.status = MigrationStatus::Failed(format!(
            "{} is empty or unreadable, so there is nothing safe to copy. Left in place. Inspect \
             it, or delete it if it is a leftover and let TA create a fresh key.",
            from.display()
        ));
        return item;
    };
    if let Some(existing) = load_secret_new(ps, which, opts) {
        item.status = if existing.value == old_value {
            match std::fs::remove_file(&from) {
                Ok(()) => MigrationStatus::StaleCopyRemoved,
                Err(e) => MigrationStatus::Failed(format!(
                    "the new copy matches but the old file {} could not be removed: {e}. Remove \
                     it yourself.",
                    from.display()
                )),
            }
        } else {
            MigrationStatus::Conflict(format!(
                "the new home already holds a DIFFERENT {}. Both were kept. Decide which is \
                 correct, delete the other, then re-run `ta doctor --fix`.",
                which.describe()
            ))
        };
        return item;
    }
    if let Err(e) = store_secret(ps, which, &old_value, opts) {
        item.status = MigrationStatus::Failed(format!("{e}"));
        return item;
    }
    // Verify by re-reading from the new home only.
    match load_secret_new(ps, which, opts) {
        Some(copy) if copy.value == old_value => match std::fs::remove_file(&from) {
            Ok(()) => item.status = MigrationStatus::Migrated,
            Err(e) => {
                item.status = MigrationStatus::Failed(format!(
                    "copied and verified, but the old file {} could not be removed: {e}. Remove \
                     it yourself; re-running `ta doctor --fix` will finish the job.",
                    from.display()
                ))
            }
        },
        _ => {
            item.status = MigrationStatus::Failed(
                "the copy could not be read back from the new home, so the old file was NOT \
                 removed. Check the keychain / state directory permissions and re-run \
                 `ta doctor --fix`."
                    .to_string(),
            )
        }
    }
    item
}

fn file_digest(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| hex(&Sha256::digest(&b)))
}

fn collect_files(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files(&path, base, out)?;
        } else if ft.is_file() {
            out.push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
        }
        // Symlinks are not followed or moved: a link inside a secret
        // directory must not drag foreign files into the state directory.
    }
    Ok(())
}

fn migrate_moved(ps: &ProjectState, item: MovedItem) -> MigrationItem {
    let from = item.legacy_path(ps);
    let to_path = item.new_path(ps);
    let mut out = MigrationItem {
        name: item.legacy_name().to_string(),
        from: from.clone(),
        to: to_path.display().to_string(),
        status: MigrationStatus::NothingToMigrate,
    };
    if !from.exists() {
        return out;
    }
    let rels: Vec<PathBuf> = if item.is_dir() {
        let mut v = Vec::new();
        if let Err(e) = collect_files(&from, &from, &mut v) {
            out.status = MigrationStatus::Failed(format!(
                "could not list {}: {e}. Left in place; fix the permissions and re-run \
                 `ta doctor --fix`.",
                from.display()
            ));
            return out;
        }
        v
    } else {
        vec![PathBuf::new()]
    };
    let src_of = |rel: &Path| {
        if rel.as_os_str().is_empty() {
            from.clone()
        } else {
            from.join(rel)
        }
    };
    let dst_of = |rel: &Path| {
        if rel.as_os_str().is_empty() {
            to_path.clone()
        } else {
            to_path.join(rel)
        }
    };
    // Phase 1: copy, never overwriting a different file.
    let mut all_identical_already = true;
    for rel in &rels {
        let (src, dst) = (src_of(rel), dst_of(rel));
        let Some(src_digest) = file_digest(&src) else {
            out.status = MigrationStatus::Failed(format!(
                "could not read {}. Left in place; check permissions and re-run.",
                src.display()
            ));
            return out;
        };
        if dst.exists() {
            if file_digest(&dst).as_deref() != Some(src_digest.as_str()) {
                out.status = MigrationStatus::Conflict(format!(
                    "{} already exists in the state directory with different content. Both were \
                     kept. Decide which is correct, delete the other, then re-run \
                     `ta doctor --fix`.",
                    dst.display()
                ));
                return out;
            }
            continue;
        }
        all_identical_already = false;
        let bytes = match std::fs::read(&src) {
            Ok(b) => b,
            Err(e) => {
                out.status =
                    MigrationStatus::Failed(format!("could not read {}: {e}", src.display()));
                return out;
            }
        };
        if let Err(e) = write_bytes_private(&dst, &bytes) {
            out.status = MigrationStatus::Failed(format!(
                "could not write {}: {e}. The old copy was left in place.",
                dst.display()
            ));
            return out;
        }
    }
    // Phase 2: verify every destination by re-reading it.
    for rel in &rels {
        if file_digest(&src_of(rel)) != file_digest(&dst_of(rel)) {
            out.status = MigrationStatus::Failed(format!(
                "verification failed for {}: the copy does not match. The old copy was NOT \
                 removed. Re-run `ta doctor --fix`.",
                dst_of(rel).display()
            ));
            return out;
        }
    }
    // Phase 3: remove the old copy, only now that every file is verified.
    let removed = if item.is_dir() {
        std::fs::remove_dir_all(&from)
    } else {
        std::fs::remove_file(&from)
    };
    out.status = match removed {
        Ok(()) if all_identical_already => MigrationStatus::StaleCopyRemoved,
        Ok(()) => MigrationStatus::Migrated,
        Err(e) => MigrationStatus::Failed(format!(
            "copied and verified, but the old copy {} could not be removed: {e}. Remove it \
             yourself; re-running `ta doctor --fix` will finish the job.",
            from.display()
        )),
    };
    out
}

fn write_bytes_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// Convenience: write a private (0600) text file inside the state directory.
pub fn write_state_file(path: &Path, contents: &str) -> Result<(), StateError> {
    human_write(path, contents)
}

// ── Config hashes (persona and agent config pinning, CR-12 groundwork) ─────

/// Record the SHA-256 of a project config file (persona TOML, agent YAML) in
/// the state directory, keyed by its path relative to the project. Returns
/// the digest. Stored OUTSIDE the project so a poisoned project file cannot
/// carry its own "expected" hash.
pub fn record_config_hash(
    ps: &ProjectState,
    rel: &str,
    bytes: &[u8],
) -> Result<String, StateError> {
    let digest = hex(&Sha256::digest(bytes));
    let path = ps.hashes_path();
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    map.insert(
        rel.replace('\\', "/"),
        serde_json::json!({
            "sha256": digest,
            "recorded_at": chrono::Utc::now().to_rfc3339(),
        }),
    );
    let text = serde_json::to_string_pretty(&map).map_err(|e| StateError::Corrupt {
        what: "config hash registry".into(),
        path: path.clone(),
        reason: e.to_string(),
    })?;
    human_write(&path, &text)?;
    Ok(digest)
}

/// Result of checking a config file against its recorded hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashCheck {
    /// Matches the recorded hash.
    Match,
    /// Differs from the recorded hash.
    Drift { recorded: String, actual: String },
    /// No hash was ever recorded for this file.
    Unrecorded,
}

pub fn check_config_hash(ps: &ProjectState, rel: &str, bytes: &[u8]) -> HashCheck {
    let actual = hex(&Sha256::digest(bytes));
    let recorded = std::fs::read_to_string(ps.hashes_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get(rel.replace('\\', "/"))
                .and_then(|e| e.get("sha256"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        });
    match recorded {
        None => HashCheck::Unrecorded,
        Some(r) if r == actual => HashCheck::Match,
        Some(r) => HashCheck::Drift {
            recorded: r,
            actual,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, StateLocator) {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let loc = StateLocator::at(state.path());
        (project, state, loc)
    }

    #[test]
    fn per_os_locations_are_exact() {
        let home = Path::new("/home/u");
        assert_eq!(
            state_root_for(Os::MacOs, &env_of(&[]), Some(home)),
            Path::new("/home/u/Library/Application Support/TrustedAutonomy")
        );
        assert_eq!(
            state_root_for(Os::Linux, &env_of(&[]), Some(home)),
            Path::new("/home/u/.local/state/trusted-autonomy")
        );
        assert_eq!(
            state_root_for(
                Os::Linux,
                &env_of(&[("XDG_STATE_HOME", "/x/state")]),
                Some(home)
            ),
            Path::new("/x/state/trusted-autonomy")
        );
        assert_eq!(
            state_root_for(
                Os::Windows,
                &env_of(&[("LOCALAPPDATA", "C:/Users/u/AppData/Local")]),
                Some(home)
            ),
            Path::new("C:/Users/u/AppData/Local/TrustedAutonomy")
        );
        assert_eq!(
            state_root_for(Os::Windows, &env_of(&[]), Some(home)),
            Path::new("/home/u/AppData/Local/TrustedAutonomy")
        );
        // The override wins on every OS.
        assert_eq!(
            state_root_for(Os::MacOs, &env_of(&[(STATE_ROOT_ENV, "/o")]), Some(home)),
            Path::new("/o")
        );
    }

    #[test]
    fn project_id_is_stable_and_two_projects_never_share() {
        let (p1, _s, loc) = fixture();
        let p2 = tempfile::tempdir().unwrap();
        let a = ProjectState::open(p1.path(), &loc).unwrap();
        let again = ProjectState::open(p1.path(), &loc).unwrap();
        let b = ProjectState::open(p2.path(), &loc).unwrap();
        assert_eq!(a.project_id, again.project_id);
        assert_ne!(a.project_id, b.project_id);
        assert_ne!(a.dir, b.dir);
        assert!(valid_project_id(&a.project_id));
        // The id file is the only thing recorded in the project, and is not a secret.
        let recorded = std::fs::read_to_string(p1.path().join(".ta/project-id")).unwrap();
        assert_eq!(recorded.trim(), a.project_id);
    }

    #[test]
    fn copied_project_gets_its_own_id_but_moved_project_keeps_it() {
        let (p1, _s, loc) = fixture();
        let a = ProjectState::open(p1.path(), &loc).unwrap();
        // A copy: the id file is duplicated while the original still exists.
        let copy = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(copy.path().join(".ta")).unwrap();
        std::fs::copy(
            p1.path().join(".ta/project-id"),
            copy.path().join(".ta/project-id"),
        )
        .unwrap();
        let c = ProjectState::open(copy.path(), &loc).unwrap();
        assert_ne!(c.project_id, a.project_id, "a copy must not share keys");
        // A move: the recorded owner no longer exists.
        let marker = loc
            .projects_dir()
            .join(&a.project_id)
            .join(PROJECT_PATH_MARKER);
        std::fs::write(&marker, "/definitely/not/here/anymore").unwrap();
        let moved = ProjectState::open(p1.path(), &loc).unwrap();
        assert_eq!(
            moved.project_id, a.project_id,
            "a moved project keeps its keys"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            canonical_text(p1.path())
        );
    }

    #[test]
    fn staging_workspace_cannot_open_a_state_directory() {
        let (_p, _s, loc) = fixture();
        let err = ProjectState::open(Path::new("/proj/.ta/staging/abc"), &loc).unwrap_err();
        assert!(err.to_string().contains("staging workspace"));
    }

    #[test]
    fn locate_creates_nothing() {
        let (p, s, loc) = fixture();
        assert!(ProjectState::locate(p.path(), &loc).unwrap().is_none());
        assert!(!p.path().join(".ta").exists());
        assert!(std::fs::read_dir(s.path()).unwrap().next().is_none());
    }

    #[test]
    fn secret_round_trip_in_state_file_outside_the_project() {
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        let opts = CustodyOpts::file_only();
        assert!(load_secret(&ps, ControlSecret::AuditKey, opts)
            .unwrap()
            .is_none());
        let (made, created) =
            load_or_create_secret(&ps, ControlSecret::AuditKey, opts, || "ab".repeat(32)).unwrap();
        assert!(created);
        assert!(matches!(made.origin, SecretOrigin::StateFile(_)));
        let (again, created) =
            load_or_create_secret(&ps, ControlSecret::AuditKey, opts, || "zz".into()).unwrap();
        assert!(!created);
        assert_eq!(again.value, "ab".repeat(32));
        assert!(!p.path().join(".ta/audit.key").exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_secret_files_are_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        store_secret(
            &ps,
            ControlSecret::BrokerRootKey,
            "k",
            CustodyOpts::file_only(),
        )
        .unwrap();
        let mode = std::fs::metadata(secret_file(&ps, ControlSecret::BrokerRootKey))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn default_posture_reads_legacy_key_and_strict_refuses_it() {
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        std::fs::write(p.path().join(".ta/audit.key"), "legacyvalue\n").unwrap();
        let got = load_secret(&ps, ControlSecret::AuditKey, CustodyOpts::file_only())
            .unwrap()
            .unwrap();
        assert_eq!(got.value, "legacyvalue");
        assert!(matches!(got.origin, SecretOrigin::Legacy(_)));
        let err = load_secret(
            &ps,
            ControlSecret::AuditKey,
            CustodyOpts::file_only().strict(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ta doctor --fix"), "{err}");
        assert!(err.contains("strict"), "{err}");
    }

    #[test]
    fn strict_also_refuses_a_stale_legacy_copy_next_to_a_new_key() {
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        store_secret(&ps, ControlSecret::AuditKey, "v", CustodyOpts::file_only()).unwrap();
        std::fs::write(p.path().join(".ta/audit.key"), "v").unwrap();
        assert!(load_secret(
            &ps,
            ControlSecret::AuditKey,
            CustodyOpts::file_only().strict()
        )
        .is_err());
        // Default posture prefers the new copy.
        let got = load_secret(&ps, ControlSecret::AuditKey, CustodyOpts::file_only())
            .unwrap()
            .unwrap();
        assert!(matches!(got.origin, SecretOrigin::StateFile(_)));
    }

    /// Old-layout fixture: every control-plane secret in the project tree.
    fn old_layout(project: &Path) {
        let ta = project.join(".ta");
        std::fs::create_dir_all(ta.join("tokens")).unwrap();
        std::fs::create_dir_all(ta.join("nats-jetstream/jetstream/sub")).unwrap();
        std::fs::write(ta.join("broker_root.key"), "root-key-sentinel\n").unwrap();
        std::fs::write(ta.join("audit.key"), "audit-key-sentinel\n").unwrap();
        std::fs::write(
            ta.join("daemon-tokens.json"),
            "{\"t\":\"daemon-token-sentinel\"}",
        )
        .unwrap();
        std::fs::write(ta.join("tokens/a.json"), "token-a-sentinel").unwrap();
        std::fs::write(ta.join("tokens/b.json"), "token-b-sentinel").unwrap();
        std::fs::write(ta.join("nats-jetstream/jetstream/sub/blk.dat"), "nats-data").unwrap();
    }

    #[test]
    fn migration_verifies_copies_removes_and_is_idempotent() {
        let (p, _s, loc) = fixture();
        old_layout(p.path());
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        let opts = CustodyOpts::file_only();

        let report = migrate_project(&ps, opts);
        assert!(report.problems().is_empty(), "{:?}", report.problems());
        assert_eq!(
            report
                .items
                .iter()
                .filter(|i| i.status == MigrationStatus::Migrated)
                .count(),
            5
        );
        // Old locations are gone, new ones hold the values.
        let ta = p.path().join(".ta");
        for gone in [
            "broker_root.key",
            "audit.key",
            "daemon-tokens.json",
            "tokens",
            "nats-jetstream",
        ] {
            assert!(!ta.join(gone).exists(), "{gone} must be removed");
        }
        assert_eq!(
            load_secret(&ps, ControlSecret::BrokerRootKey, opts)
                .unwrap()
                .unwrap()
                .value,
            "root-key-sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(ps.tokens_dir().join("b.json")).unwrap(),
            "token-b-sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(ps.nats_data_dir().join("jetstream/sub/blk.dat")).unwrap(),
            "nats-data"
        );
        // Re-run: nothing to do, nothing lost.
        let again = migrate_project(&ps, opts);
        assert!(!again.changed());
        assert!(again
            .items
            .iter()
            .all(|i| i.status == MigrationStatus::NothingToMigrate));
        assert_eq!(
            load_secret(&ps, ControlSecret::AuditKey, opts)
                .unwrap()
                .unwrap()
                .value,
            "audit-key-sentinel"
        );
        // Strict now has nothing to refuse.
        assert!(load_secret(&ps, ControlSecret::AuditKey, opts.strict())
            .unwrap()
            .is_some());
    }

    #[test]
    fn migration_never_deletes_when_the_new_value_differs() {
        let (p, _s, loc) = fixture();
        old_layout(p.path());
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        let opts = CustodyOpts::file_only();
        store_secret(&ps, ControlSecret::AuditKey, "a-different-key", opts).unwrap();
        std::fs::create_dir_all(ps.tokens_dir()).unwrap();
        std::fs::write(ps.tokens_dir().join("a.json"), "DIFFERENT").unwrap();

        let report = migrate_project(&ps, opts);
        let conflicts: Vec<_> = report
            .items
            .iter()
            .filter(|i| matches!(i.status, MigrationStatus::Conflict(_)))
            .map(|i| i.name.as_str())
            .collect();
        assert!(
            conflicts.contains(&"audit-key") && conflicts.contains(&"tokens"),
            "{conflicts:?}"
        );
        let ta = p.path().join(".ta");
        assert!(ta.join("audit.key").exists(), "old audit key must be kept");
        assert!(ta.join("tokens/a.json").exists(), "old tokens must be kept");
        // The unrelated items still moved.
        assert!(!ta.join("broker_root.key").exists());
    }

    #[test]
    fn migration_does_not_remove_the_old_key_when_verification_fails() {
        let (p, _s, loc) = fixture();
        std::fs::create_dir_all(p.path().join(".ta")).unwrap();
        std::fs::write(p.path().join(".ta/audit.key"), "audit-key-sentinel").unwrap();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        // Make the state-file destination unwritable by occupying it with a directory.
        std::fs::create_dir_all(secret_file(&ps, ControlSecret::AuditKey)).unwrap();
        let report = migrate_project(&ps, CustodyOpts::file_only());
        let audit = report.items.iter().find(|i| i.name == "audit-key").unwrap();
        assert!(
            matches!(audit.status, MigrationStatus::Failed(_)),
            "{:?}",
            audit.status
        );
        assert!(p.path().join(".ta/audit.key").exists());
    }

    #[test]
    fn startup_check_warns_by_default_and_refuses_under_strict() {
        let (p, _s, _loc) = fixture();
        assert!(
            startup_check(p.path(), Posture::Strict).is_ok(),
            "clean tree"
        );
        old_layout(p.path());
        assert!(startup_check(p.path(), Posture::Default).is_ok());
        let err = startup_check(p.path(), Posture::Strict).unwrap_err();
        assert!(
            err.contains("broker_root.key") && err.contains("ta doctor --fix"),
            "{err}"
        );
        assert_eq!(legacy_findings(p.path()).len(), 5);
    }

    #[test]
    fn strict_startup_refuses_the_vault_key_in_the_project_tree() {
        let (p, _s, _loc) = fixture();
        std::fs::create_dir_all(p.path().join(".ta")).unwrap();
        std::fs::write(p.path().join(".ta/credentials.key"), "AGE-SECRET-KEY-1X").unwrap();
        assert!(startup_check(p.path(), Posture::Default).is_ok());
        let err = startup_check(p.path(), Posture::Strict).unwrap_err();
        assert!(
            err.contains("credentials.key") && err.contains("ta doctor --fix"),
            "{err}"
        );
        assert_eq!(legacy_findings(p.path()).len(), 1);
    }

    #[test]
    fn config_hashes_live_in_the_state_dir_and_detect_drift() {
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        assert_eq!(
            check_config_hash(&ps, ".ta/personas/cos.toml", b"v1"),
            HashCheck::Unrecorded
        );
        record_config_hash(&ps, ".ta/personas/cos.toml", b"v1").unwrap();
        assert_eq!(
            check_config_hash(&ps, ".ta/personas/cos.toml", b"v1"),
            HashCheck::Match
        );
        assert!(matches!(
            check_config_hash(&ps, ".ta/personas/cos.toml", b"v2"),
            HashCheck::Drift { .. }
        ));
        assert!(ps.hashes_path().starts_with(loc.root()));
    }

    #[test]
    fn action_queue_and_apply_journal_live_outside_the_project_tree() {
        let (p, _s, loc) = fixture();
        let ps = ProjectState::open(p.path(), &loc).unwrap();
        for dir in [ps.action_queue_dir(), ps.apply_journal_dir()] {
            assert!(
                dir.starts_with(loc.root()),
                "{dir:?} must be in the state root"
            );
            assert!(
                !dir.starts_with(p.path()),
                "{dir:?} must not be in the project tree"
            );
        }
        assert_ne!(ps.action_queue_dir(), ps.apply_journal_dir());
        // An agent in a staging workspace cannot even resolve the state directory.
        let staging = p.path().join(".ta").join("staging").join("goal-1");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(matches!(
            ProjectState::open(&staging, &loc),
            Err(StateError::StagingWorkspace { .. })
        ));
    }

    #[test]
    fn locator_is_isolated_under_test_and_keychain_is_off() {
        let loc = StateLocator::from_env();
        assert!(
            loc.root().starts_with(std::env::temp_dir()),
            "{:?}",
            loc.root()
        );
        assert!(!CustodyOpts::from_env(&loc).use_keychain);
    }

    #[test]
    fn posture_parses_and_resolves_from_the_state_root_file() {
        assert_eq!(Posture::parse(" Strict\n"), Posture::Strict);
        assert_eq!(Posture::parse("balanced"), Posture::Default);
        let (_p, s, loc) = fixture();
        assert_eq!(Posture::resolve(&loc), Posture::Default);
        std::fs::write(s.path().join(POSTURE_FILENAME), "strict").unwrap();
        assert_eq!(Posture::resolve(&loc), Posture::Strict);
    }
}
