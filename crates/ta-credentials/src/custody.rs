// custody.rs: the ONE place that decides whether TA keeps secrets in the OS
// keychain or in owner-only files, plus the verified migration between the two.
//
// Why this exists. On macOS every keychain item carries a `partition_id` ACL.
// A binary signed by a self-signed local certificate has no Team ID, so macOS
// identifies it by its cdhash, and every rebuild is a new identity for every
// item: endless permission prompts. A per-process environment variable
// (`TA_NO_KEYCHAIN`) cannot cure that for installed binaries, because it does
// not reach launchd children, the daemon or the poller. A persistent per-user
// choice does.
//
// Precedence (first match wins):
//   1. `TA_NO_KEYCHAIN` set (presence, not value)      -> file
//   2. a cargo test binary (keychain guard)             -> file
//   3. `<user_state_dir>/custody` holding `file`/`keychain` -> that mode
//   4. default                                          -> keychain
//
// A malformed custody file is ignored with a warning naming it and the accepted
// values; it never silently turns the keychain off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::human;
use crate::keychain::{self, CONTROL_PLANE_SERVICE, HUMAN_ITEM, HUMAN_SERVICE};
use crate::state::{ControlSecret, ProjectState};

/// Name of the per-user custody file inside [`human::user_state_dir`].
pub const CUSTODY_FILENAME: &str = "custody";

/// Where secrets are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyMode {
    /// Owner-only (0600) files in the per-user / per-project state directory.
    File,
    /// The OS keychain, with an owner-only file fallback when it is unusable.
    Keychain,
}

impl CustodyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CustodyMode::File => "file",
            CustodyMode::Keychain => "keychain",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "file" => Some(CustodyMode::File),
            "keychain" => Some(CustodyMode::Keychain),
            _ => None,
        }
    }
}

/// Why the effective mode is what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyReason {
    /// `TA_NO_KEYCHAIN` is set in the environment.
    Env,
    /// Running inside a cargo test binary: the real keychain is unreachable.
    TestGuard,
    /// The per-user custody file chose the mode.
    CustodyFile(PathBuf),
    /// Nothing chose: the production default.
    Default,
}

impl CustodyReason {
    /// One line for `ta credentials custody status`.
    pub fn describe(&self) -> String {
        match self {
            CustodyReason::Env => format!(
                "environment: {} is set (overrides the custody file; unset it to use the file)",
                crate::encryption::NO_KEYCHAIN_ENV
            ),
            CustodyReason::TestGuard => {
                "test guard: this is a cargo test binary, which never touches the real keychain"
                    .to_string()
            }
            CustodyReason::CustodyFile(p) => {
                format!("custody file: {} (your explicit choice)", p.display())
            }
            CustodyReason::Default => {
                "default: no environment override and no custody file (production default)"
                    .to_string()
            }
        }
    }
}

/// What the custody file held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyFileState {
    Missing,
    Mode(CustodyMode),
    /// Present but not exactly `file` or `keychain`; the text is the warning.
    Invalid(String),
}

/// The effective custody decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub mode: CustodyMode,
    pub reason: CustodyReason,
    /// Set when the custody file existed but was ignored.
    pub warning: Option<String>,
}

/// Path of the per-user custody file.
pub fn custody_file_path() -> PathBuf {
    human::user_state_dir().join(CUSTODY_FILENAME)
}

/// Read and classify a custody file.
pub fn read_custody_file(path: &Path) -> CustodyFileState {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CustodyFileState::Missing,
        Err(e) => {
            return CustodyFileState::Invalid(format!(
                "could not read the custody file {}: {e}. It is ignored; the default (keychain) \
                 applies. Fix its permissions, or rewrite it with `ta credentials custody \
                 file` or `ta credentials custody keychain`.",
                path.display()
            ))
        }
    };
    match CustodyMode::parse(&text) {
        Some(mode) => CustodyFileState::Mode(mode),
        None => CustodyFileState::Invalid(format!(
            "the custody file {} holds {:?}, which is not an accepted value. It is ignored; \
             the default (keychain) applies. Accepted values are exactly `file` or `keychain`; \
             rewrite it with `ta credentials custody file` or `ta credentials custody keychain`.",
            path.display(),
            text.trim()
        )),
    }
}

/// The pure core of the resolver: every input explicit, so the precedence
/// table is testable without touching process-wide state.
pub fn resolve_with(env_opt_out: bool, test_guard: bool, custody_file: &Path) -> Resolution {
    if env_opt_out {
        return Resolution {
            mode: CustodyMode::File,
            reason: CustodyReason::Env,
            warning: None,
        };
    }
    if test_guard {
        return Resolution {
            mode: CustodyMode::File,
            reason: CustodyReason::TestGuard,
            warning: None,
        };
    }
    match read_custody_file(custody_file) {
        CustodyFileState::Mode(mode) => Resolution {
            mode,
            reason: CustodyReason::CustodyFile(custody_file.to_path_buf()),
            warning: None,
        },
        CustodyFileState::Invalid(warning) => Resolution {
            mode: CustodyMode::Keychain,
            reason: CustodyReason::Default,
            warning: Some(warning),
        },
        CustodyFileState::Missing => Resolution {
            mode: CustodyMode::Keychain,
            reason: CustodyReason::Default,
            warning: None,
        },
    }
}

/// The effective custody decision for the running process. Warns (once per
/// process) when the custody file is present but unusable.
pub fn resolve() -> Resolution {
    let resolution = resolve_with(
        std::env::var_os(crate::encryption::NO_KEYCHAIN_ENV).is_some(),
        crate::encryption::keychain_guard_active(),
        &custody_file_path(),
    );
    if let Some(warning) = &resolution.warning {
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::SeqCst) {
            tracing::warn!(file = %custody_file_path().display(), "{warning}");
        }
    }
    resolution
}

/// Whether the OS keychain is the default custody for this process. Every
/// site that used to read `TA_NO_KEYCHAIN` itself goes through here.
pub fn use_keychain() -> bool {
    resolve().mode == CustodyMode::Keychain
}

/// A hard opt-out for code that is handed `use_keychain = true` by its caller:
/// the environment variable or an explicit `file` in the custody file. Unlike
/// [`use_keychain`] it ignores the test guard on purpose (tests rely on the
/// guard panicking when they reach for the real keychain) and does not read
/// the custody file inside a test binary.
pub(crate) fn opted_out_of_keychain() -> bool {
    if std::env::var_os(crate::encryption::NO_KEYCHAIN_ENV).is_some() {
        return true;
    }
    if crate::encryption::keychain_guard_active() {
        return false;
    }
    matches!(
        read_custody_file(&custody_file_path()),
        CustodyFileState::Mode(CustodyMode::File)
    )
}

/// Write the custody file (directory 0700, file 0600).
pub fn write_custody_file(path: &Path, mode: CustodyMode) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Only tighten a directory this call is likely to have created:
            // never loosen, and leave a pre-existing directory's mode alone.
            if std::fs::read_dir(parent)?.next().is_none() {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    human::write_private(path, &format!("{}\n", mode.as_str()))
        .map_err(|e| std::io::Error::other(e.to_string()))
}

// ── Migration ──────────────────────────────────────────────────────────────

/// Outcome for one secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemStatus {
    /// Copied to the destination and read back identical.
    Copied,
    /// The destination already held an identical copy.
    AlreadyThere,
    /// Nothing at the source.
    NothingToCopy,
    /// The keychain was deliberately not touched (`TA_NO_KEYCHAIN` is set), so
    /// this item was not compared or copied. Existing keychain secrets, if
    /// any, were NOT migrated.
    Skipped(String),
    /// The destination holds a DIFFERENT value. Nothing was overwritten.
    Conflict(String),
    /// The source or destination could not be read or written. Nothing changed.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ItemReport {
    pub name: String,
    pub source: String,
    pub destination: String,
    pub status: ItemStatus,
}

/// Result of [`migrate`].
#[derive(Debug, Clone)]
pub struct MigrationOutcome {
    pub target: CustodyMode,
    pub items: Vec<ItemReport>,
    /// Exact shell commands that remove the now-unused source entries. TA never
    /// runs these itself.
    pub cleanup_commands: Vec<String>,
}

impl MigrationOutcome {
    /// True when every item is copied, already there, or had nothing to copy.
    pub fn is_clean(&self) -> bool {
        self.items.iter().all(|i| {
            matches!(
                i.status,
                ItemStatus::Copied
                    | ItemStatus::AlreadyThere
                    | ItemStatus::NothingToCopy
                    | ItemStatus::Skipped(_)
            )
        })
    }

    pub fn count(&self, pred: impl Fn(&ItemStatus) -> bool) -> usize {
        self.items.iter().filter(|i| pred(&i.status)).count()
    }
}

/// One secret and the two places it can live.
struct Slot {
    name: &'static str,
    keychain: (String, String),
    file: PathBuf,
}

fn slots(ps: &ProjectState, human_dir: &Path) -> Vec<Slot> {
    let mut v = Vec::new();
    for which in [ControlSecret::BrokerRootKey, ControlSecret::AuditKey] {
        v.push(Slot {
            name: which.describe(),
            keychain: (
                CONTROL_PLANE_SERVICE.to_string(),
                format!("{}/{}", ps.project_id, which.name()),
            ),
            file: ps.secrets_dir().join(which.name()),
        });
    }
    v.push(Slot {
        name: "credential vault age identity",
        keychain: (
            keychain::VAULT_SERVICE.to_string(),
            keychain::VAULT_ITEM.to_string(),
        ),
        file: ps.secrets_dir().join(ControlSecret::VaultKey.name()),
    });
    v.push(Slot {
        name: "human credential",
        keychain: (HUMAN_SERVICE.to_string(), HUMAN_ITEM.to_string()),
        file: human_dir.join(human::SECRET_FILENAME),
    });
    v
}

fn read_file_value(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let s = s.trim().to_string();
            Ok(if s.is_empty() { None } else { Some(s) })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

fn keychain_text(slot: &Slot) -> String {
    format!(
        "the OS keychain (service {}, item {})",
        slot.keychain.0, slot.keychain.1
    )
}

fn file_text(slot: &Slot) -> String {
    slot.file.display().to_string()
}

/// Copy every control-plane secret, the vault age identity and the human
/// credential of `ps` to the store `target` names, verifying each copy by
/// reading it back from the destination.
///
/// With `keychain_allowed == false` (the process has `TA_NO_KEYCHAIN` set) the
/// keychain is never read or written; every item is reported `Skipped`.
///
/// Never overwrites a destination that holds a different value, never creates
/// a replacement key over an unreadable one, and never deletes a source entry:
/// the returned `cleanup_commands` are for the person to run.
pub fn migrate(
    ps: &ProjectState,
    human_dir: &Path,
    target: CustodyMode,
    keychain_allowed: bool,
) -> MigrationOutcome {
    let mut items = Vec::new();
    let mut cleanup = Vec::new();
    for slot in slots(ps, human_dir) {
        let (source_text, dest_text) = match target {
            CustodyMode::File => (keychain_text(&slot), file_text(&slot)),
            CustodyMode::Keychain => (file_text(&slot), keychain_text(&slot)),
        };
        let status = if !keychain_allowed {
            ItemStatus::Skipped(format!(
                "{} is set, so the keychain was not read or written for {}. Unset it and re-run \
                 to migrate existing keychain secrets",
                crate::encryption::NO_KEYCHAIN_ENV,
                slot.name
            ))
        } else {
            match target {
                CustodyMode::File => keychain_to_file(&slot, &source_text),
                CustodyMode::Keychain => file_to_keychain(&slot, &dest_text),
            }
        };
        if matches!(status, ItemStatus::Copied | ItemStatus::AlreadyThere) {
            cleanup.push(match target {
                CustodyMode::File => format!(
                    "security delete-generic-password -s '{}' -a '{}'",
                    slot.keychain.0, slot.keychain.1
                ),
                CustodyMode::Keychain => format!("rm '{}'", slot.file.display()),
            });
        }
        items.push(ItemReport {
            name: slot.name.to_string(),
            source: source_text,
            destination: dest_text,
            status,
        });
    }
    MigrationOutcome {
        target,
        items,
        cleanup_commands: cleanup,
    }
}

/// Result of [`switch`].
#[derive(Debug)]
pub struct SwitchOutcome {
    pub migration: MigrationOutcome,
    /// `Ok(true)` when the custody file was written, `Ok(false)` when it was
    /// left alone because the migration was not clean.
    pub custody_file: Result<bool, std::io::Error>,
}

/// Move this project's secrets to `target`'s store (verified copy) and, only if
/// every item is safely there, record the choice in `custody_path`.
///
/// The custody file is written AFTER the migration, not before: a file-custody
/// choice recorded while a secret is still only in the keychain would make the
/// next process create a replacement key. A failed or conflicting migration
/// therefore leaves the previous mode in force.
pub fn switch(
    ps: &ProjectState,
    human_dir: &Path,
    custody_path: &Path,
    target: CustodyMode,
    keychain_allowed: bool,
) -> SwitchOutcome {
    let migration = migrate(ps, human_dir, target, keychain_allowed);
    let custody_file = if migration.is_clean() {
        write_custody_file(custody_path, target).map(|()| true)
    } else {
        Ok(false)
    };
    SwitchOutcome {
        migration,
        custody_file,
    }
}

fn keychain_to_file(slot: &Slot, source_text: &str) -> ItemStatus {
    let value = match keychain::get(&slot.keychain.0, &slot.keychain.1) {
        Ok(Some(v)) => v,
        Ok(None) => return ItemStatus::NothingToCopy,
        Err(e) => {
            return ItemStatus::Failed(format!(
                "tried to read {source_text} and could not: {e}. Nothing was changed and no \
                 replacement was created. Unlock the keychain, approve the TA access prompt \
                 (choose Always Allow), then re-run the command."
            ))
        }
    };
    match read_file_value(&slot.file) {
        Err(e) => ItemStatus::Failed(format!(
            "{e}. The existing file was not touched. Fix its permissions, then re-run."
        )),
        Ok(Some(existing)) if existing == value => ItemStatus::AlreadyThere,
        Ok(Some(_)) => ItemStatus::Conflict(format!(
            "{} already exists with a DIFFERENT value than {source_text}. Neither was changed. \
             Decide which is correct, delete the other, then re-run.",
            slot.file.display()
        )),
        Ok(None) => {
            if let Err(e) = human::write_private(&slot.file, &value) {
                return ItemStatus::Failed(format!(
                    "tried to write {} and could not: {e}. The keychain entry was left alone.",
                    slot.file.display()
                ));
            }
            match read_file_value(&slot.file) {
                Ok(Some(copy)) if copy == value => ItemStatus::Copied,
                _ => ItemStatus::Failed(format!(
                    "wrote {} but reading it back did not match the keychain value. The keychain \
                     entry was left alone; check the directory's permissions and re-run.",
                    slot.file.display()
                )),
            }
        }
    }
}

fn file_to_keychain(slot: &Slot, dest_text: &str) -> ItemStatus {
    let value = match read_file_value(&slot.file) {
        Ok(Some(v)) => v,
        Ok(None) => return ItemStatus::NothingToCopy,
        Err(e) => {
            return ItemStatus::Failed(format!(
                "{e}. Nothing was changed. Fix its permissions, then re-run."
            ))
        }
    };
    match keychain::get(&slot.keychain.0, &slot.keychain.1) {
        Err(e) => ItemStatus::Failed(format!(
            "tried to read {dest_text} before writing and could not: {e}. TA will not write a \
             key over an entry it cannot read. Unlock the keychain, approve the prompt (choose \
             Always Allow), then re-run."
        )),
        Ok(Some(existing)) if existing == value => ItemStatus::AlreadyThere,
        Ok(Some(_)) => ItemStatus::Conflict(format!(
            "{dest_text} already holds a DIFFERENT value than {}. Neither was changed. Decide \
             which is correct, remove the other, then re-run.",
            slot.file.display()
        )),
        Ok(None) => {
            if let Err(e) = keychain::set(&slot.keychain.0, &slot.keychain.1, &value) {
                return ItemStatus::Failed(format!(
                    "tried to write {dest_text} and could not: {e}. The file {} was left alone.",
                    slot.file.display()
                ));
            }
            match keychain::get(&slot.keychain.0, &slot.keychain.1) {
                Ok(Some(copy)) if copy == value => ItemStatus::Copied,
                other => ItemStatus::Failed(format!(
                    "wrote {dest_text} but reading it back gave {} instead of the original. The \
                     file {} was left alone; re-run once the keychain is readable.",
                    match other {
                        Ok(Some(_)) => "a different value".to_string(),
                        Ok(None) => "nothing".to_string(),
                        Err(e) => format!("an error ({e})"),
                    },
                    slot.file.display()
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::testing::{FakeKeychain, LOCK};
    use crate::state::StateLocator;
    use std::sync::Arc;

    fn file_with(dir: &Path, text: &str) -> PathBuf {
        let p = dir.join(CUSTODY_FILENAME);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn env_beats_file_and_file_beats_default() {
        let dir = tempfile::tempdir().unwrap();
        let keychain_file = file_with(dir.path(), "keychain\n");
        let r = resolve_with(true, false, &keychain_file);
        assert_eq!((r.mode, r.reason), (CustodyMode::File, CustodyReason::Env));

        let file_file = file_with(dir.path(), "  file \n");
        let r = resolve_with(false, false, &file_file);
        assert_eq!(r.mode, CustodyMode::File);
        assert_eq!(r.reason, CustodyReason::CustodyFile(file_file.clone()));

        let r = resolve_with(false, false, &keychain_file_again(dir.path()));
        assert_eq!(r.mode, CustodyMode::Keychain);
        assert!(matches!(r.reason, CustodyReason::CustodyFile(_)));

        let missing = dir.path().join("nope");
        let r = resolve_with(false, false, &missing);
        assert_eq!(
            (r.mode, r.reason, r.warning),
            (CustodyMode::Keychain, CustodyReason::Default, None)
        );
    }

    fn keychain_file_again(dir: &Path) -> PathBuf {
        file_with(dir, "keychain")
    }

    #[test]
    fn test_guard_beats_everything() {
        let dir = tempfile::tempdir().unwrap();
        let p = file_with(dir.path(), "keychain");
        let r = resolve_with(false, true, &p);
        assert_eq!(
            (r.mode, r.reason),
            (CustodyMode::File, CustodyReason::TestGuard)
        );
        let r = resolve_with(true, true, &p);
        assert_eq!(r.mode, CustodyMode::File);
    }

    #[test]
    fn garbage_file_is_ignored_with_a_warning_naming_file_and_values() {
        let dir = tempfile::tempdir().unwrap();
        for garbage in ["", "files", "FILE", "file keychain", "yes"] {
            let p = file_with(dir.path(), garbage);
            let r = resolve_with(false, false, &p);
            assert_eq!(r.mode, CustodyMode::Keychain, "{garbage:?}");
            assert_eq!(r.reason, CustodyReason::Default);
            let w = r.warning.expect("a warning");
            assert!(w.contains(&p.display().to_string()), "{w}");
            assert!(w.contains("`file`") && w.contains("`keychain`"), "{w}");
        }
    }

    #[test]
    fn reason_strings_say_why() {
        let p = PathBuf::from("/x/custody");
        assert!(CustodyReason::Env.describe().contains("TA_NO_KEYCHAIN"));
        assert!(CustodyReason::TestGuard.describe().contains("test guard"));
        assert!(CustodyReason::CustodyFile(p)
            .describe()
            .contains("/x/custody"));
        assert!(CustodyReason::Default.describe().contains("default"));
    }

    #[test]
    fn write_custody_file_is_private_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fresh").join(CUSTODY_FILENAME);
        write_custody_file(&p, CustodyMode::File).unwrap();
        assert_eq!(
            read_custody_file(&p),
            CustodyFileState::Mode(CustodyMode::File)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(p.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        write_custody_file(&p, CustodyMode::Keychain).unwrap();
        assert_eq!(
            read_custody_file(&p),
            CustodyFileState::Mode(CustodyMode::Keychain)
        );
    }

    struct Fixture {
        _root: tempfile::TempDir,
        _state: tempfile::TempDir,
        human_dir: tempfile::TempDir,
        ps: ProjectState,
        fake: Arc<FakeKeychain>,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let ps = ProjectState::open(root.path(), &StateLocator::at(state.path())).unwrap();
        let fake = Arc::new(FakeKeychain::default());
        keychain::reset();
        keychain::install_backend(fake.clone());
        Fixture {
            _root: root,
            _state: state,
            human_dir: tempfile::tempdir().unwrap(),
            ps,
            fake,
        }
    }

    fn seed_keychain(f: &Fixture) {
        use crate::keychain::KeychainBackend;
        let pid = &f.ps.project_id;
        f.fake
            .set(
                CONTROL_PLANE_SERVICE,
                &format!("{pid}/broker-root-key"),
                "ROOT",
            )
            .unwrap();
        f.fake
            .set(CONTROL_PLANE_SERVICE, &format!("{pid}/audit-key"), "AUDIT")
            .unwrap();
        f.fake
            .set(
                keychain::VAULT_SERVICE,
                keychain::VAULT_ITEM,
                "AGE-SECRET-KEY-X",
            )
            .unwrap();
        f.fake.set(HUMAN_SERVICE, HUMAN_ITEM, "tahc_abc").unwrap();
    }

    #[test]
    fn migrating_to_file_copies_all_four_secrets_verified_and_keeps_the_keychain() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        seed_keychain(&f);
        let out = migrate(&f.ps, f.human_dir.path(), CustodyMode::File, true);
        keychain::reset();

        assert!(out.is_clean(), "{:?}", out.items);
        assert_eq!(out.count(|s| *s == ItemStatus::Copied), 4);
        let secrets = f.ps.secrets_dir();
        assert_eq!(
            std::fs::read_to_string(secrets.join("broker-root-key")).unwrap(),
            "ROOT"
        );
        assert_eq!(
            std::fs::read_to_string(secrets.join("audit-key")).unwrap(),
            "AUDIT"
        );
        assert_eq!(
            std::fs::read_to_string(secrets.join("vault-key")).unwrap(),
            "AGE-SECRET-KEY-X"
        );
        assert_eq!(
            std::fs::read_to_string(f.human_dir.path().join("credential")).unwrap(),
            "tahc_abc"
        );
        // Source entries stay; exact removal commands are returned.
        assert_eq!(f.fake.items.lock().unwrap().len(), 4);
        assert_eq!(out.cleanup_commands.len(), 4);
        assert!(out
            .cleanup_commands
            .iter()
            .all(|c| c.starts_with("security delete-generic-password -s '")));
        assert!(out
            .cleanup_commands
            .iter()
            .any(|c| c.contains("trusted-autonomy-vault")
                && c.contains("credential-vault-age-identity")));
    }

    #[test]
    fn migrating_is_idempotent_and_refuses_to_overwrite_a_different_file() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        seed_keychain(&f);
        assert!(migrate(&f.ps, f.human_dir.path(), CustodyMode::File, true).is_clean());
        let again = migrate(&f.ps, f.human_dir.path(), CustodyMode::File, true);
        assert_eq!(again.count(|s| *s == ItemStatus::AlreadyThere), 4);

        std::fs::write(f.ps.secrets_dir().join("audit-key"), "DIFFERENT").unwrap();
        let conflict = migrate(&f.ps, f.human_dir.path(), CustodyMode::File, true);
        keychain::reset();
        assert!(!conflict.is_clean());
        assert_eq!(conflict.count(|s| matches!(s, ItemStatus::Conflict(_))), 1);
        assert_eq!(
            std::fs::read_to_string(f.ps.secrets_dir().join("audit-key")).unwrap(),
            "DIFFERENT",
            "an existing different value must never be overwritten"
        );
    }

    #[test]
    fn an_unreadable_keychain_fails_without_creating_anything() {
        struct Denied;
        impl keychain::KeychainBackend for Denied {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                Err("user denied access".into())
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
                Err("denied".into())
            }
            fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
                Err("denied".into())
            }
        }
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        keychain::install_backend(Arc::new(Denied));
        let out = migrate(&f.ps, f.human_dir.path(), CustodyMode::File, true);
        keychain::reset();
        assert!(!out.is_clean());
        assert_eq!(out.count(|s| matches!(s, ItemStatus::Failed(_))), 4);
        for item in &out.items {
            let ItemStatus::Failed(msg) = &item.status else {
                panic!()
            };
            assert!(
                msg.contains("user denied access") && msg.contains("Unlock"),
                "{msg}"
            );
        }
        assert!(out.cleanup_commands.is_empty());
        assert!(!f.ps.secrets_dir().join("audit-key").exists());
        assert!(!f.human_dir.path().join("credential").exists());
    }

    #[test]
    fn migrating_to_keychain_verifies_and_never_writes_over_an_unreadable_entry() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        std::fs::create_dir_all(f.ps.secrets_dir()).unwrap();
        std::fs::write(f.ps.secrets_dir().join("broker-root-key"), "ROOT\n").unwrap();
        std::fs::write(f.human_dir.path().join("credential"), "tahc_zzz").unwrap();
        let out = migrate(&f.ps, f.human_dir.path(), CustodyMode::Keychain, true);
        assert!(out.is_clean(), "{:?}", out.items);
        assert_eq!(out.count(|s| *s == ItemStatus::Copied), 2);
        assert_eq!(out.count(|s| *s == ItemStatus::NothingToCopy), 2);
        {
            use crate::keychain::KeychainBackend;
            let pid = &f.ps.project_id;
            assert_eq!(
                f.fake
                    .get(CONTROL_PLANE_SERVICE, &format!("{pid}/broker-root-key"))
                    .unwrap()
                    .as_deref(),
                Some("ROOT")
            );
        }
        assert!(
            f.ps.secrets_dir().join("broker-root-key").exists(),
            "source file kept"
        );
        assert!(out.cleanup_commands.iter().all(|c| c.starts_with("rm '")));

        // A corrupt/unreadable destination keychain: fail, do not write.
        struct Denied;
        impl keychain::KeychainBackend for Denied {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                Err("locked".into())
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
                panic!("must not write over an entry it cannot read")
            }
            fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        keychain::install_backend(Arc::new(Denied));
        let refused = migrate(&f.ps, f.human_dir.path(), CustodyMode::Keychain, true);
        keychain::reset();
        assert_eq!(refused.count(|s| matches!(s, ItemStatus::Failed(_))), 2);
    }

    #[test]
    fn switch_records_the_choice_only_after_a_clean_migration() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        seed_keychain(&f);
        let custody = f.human_dir.path().join(CUSTODY_FILENAME);

        // A conflicting destination blocks the switch: no custody file.
        std::fs::create_dir_all(f.ps.secrets_dir()).unwrap();
        std::fs::write(f.ps.secrets_dir().join("audit-key"), "OTHER").unwrap();
        let blocked = switch(&f.ps, f.human_dir.path(), &custody, CustodyMode::File, true);
        assert!(!blocked.migration.is_clean());
        assert!(matches!(blocked.custody_file, Ok(false)));
        assert_eq!(read_custody_file(&custody), CustodyFileState::Missing);

        std::fs::remove_file(f.ps.secrets_dir().join("audit-key")).unwrap();
        let done = switch(&f.ps, f.human_dir.path(), &custody, CustodyMode::File, true);
        keychain::reset();
        assert!(done.migration.is_clean());
        assert!(matches!(done.custody_file, Ok(true)));
        assert_eq!(
            read_custody_file(&custody),
            CustodyFileState::Mode(CustodyMode::File)
        );
    }

    #[test]
    fn a_corrupt_source_file_is_refused_not_replaced() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        std::fs::create_dir_all(f.ps.secrets_dir()).unwrap();
        // Not valid UTF-8: unreadable as a secret.
        std::fs::write(f.ps.secrets_dir().join("audit-key"), [0xff, 0xfe, 0xfd]).unwrap();
        let out = migrate(&f.ps, f.human_dir.path(), CustodyMode::Keychain, true);
        keychain::reset();
        assert_eq!(
            out.count(|s| matches!(s, ItemStatus::Failed(_))),
            1,
            "{:?}",
            out.items
        );
        assert!(
            f.fake.items.lock().unwrap().is_empty(),
            "nothing was written"
        );
    }

    #[test]
    fn a_process_with_the_env_opt_out_never_touches_the_keychain() {
        struct Panics;
        impl keychain::KeychainBackend for Panics {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                panic!("keychain read despite keychain_allowed=false")
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
                panic!("keychain write despite keychain_allowed=false")
            }
            fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
                panic!("keychain delete despite keychain_allowed=false")
            }
        }
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture();
        keychain::install_backend(Arc::new(Panics));
        for target in [CustodyMode::File, CustodyMode::Keychain] {
            let out = migrate(&f.ps, f.human_dir.path(), target, false);
            assert!(out.is_clean());
            assert_eq!(out.count(|s| matches!(s, ItemStatus::Skipped(_))), 4);
            assert!(out.cleanup_commands.is_empty());
        }
        keychain::reset();
    }
}
