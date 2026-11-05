// keychain.rs -- the one gateway to the OS keychain for control-plane secrets
// and the credential vault key (v0.17.12.16).
//
// Why this exists: reading a keychain item can block on a system dialog that
// needs a person's click ("Always Allow"). A daemon that did that read inside
// a request handler stopped answering HTTP and logging until someone clicked,
// and nothing said why. So every read and write goes through here, and the
// daemon switches this process into "request mode" once its startup preload
// is done:
//
//   * Direct mode (the CLI, and the daemon while it is starting): calls go to
//     the backend.
//   * Request mode (the daemon after startup): reads are answered from memory
//     loaded at start and NEVER reach the backend; writes and deletes reach
//     the backend only on a helper thread with a short timeout, so a locked
//     or revoked keychain can fail an operation but can never freeze a
//     caller.
//
// The backend is a trait so tests inject a blocked, slow or panicking
// keychain. The real OS keychain is never used by a test: see
// `encryption::keychain_guard_active`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Once, OnceLock, RwLock};
use std::time::Duration;

/// Longest a write or delete may take in request mode before it is abandoned
/// with an error (the helper thread is left to finish on its own).
pub const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Keychain service of the control-plane secrets (broker root key, audit key).
pub const CONTROL_PLANE_SERVICE: &str = "trusted-autonomy-control-plane";
/// Keychain service and item of the credential vault's age identity.
pub const VAULT_SERVICE: &str = "trusted-autonomy-vault";
pub const VAULT_ITEM: &str = "credential-vault-age-identity";

/// Keychain service and item of the human credential (`ta login`).
pub const HUMAN_SERVICE: &str = "trusted-autonomy-human";
pub const HUMAN_ITEM: &str = "human-credential";

/// Seconds a keychain call may take before it is abandoned, unless configured.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Environment variable that sets the wait in whole seconds. It wins over the
/// project setting.
pub const TIMEOUT_ENV: &str = "TA_KEYCHAIN_TIMEOUT_SECS";
/// The project setting for the wait. The daemon's per-item wait at startup is
/// the same value.
pub const TIMEOUT_SETTING: &str = "[daemon] keychain_approval_timeout_secs in .ta/daemon.toml";

/// The configured wait in milliseconds; 0 when none was configured.
static CONFIGURED_TIMEOUT_MS: AtomicU64 = AtomicU64::new(0);

/// The wait for a project setting of `configured_secs`: [`TIMEOUT_ENV`] when
/// it holds a positive number of seconds, else the setting, else
/// [`DEFAULT_TIMEOUT_SECS`]. A setting of 0 means "not set".
pub fn resolve_timeout(configured_secs: u64) -> Duration {
    resolve_ms(configured_secs.saturating_mul(1000))
}

fn resolve_ms(configured_ms: u64) -> Duration {
    if let Ok(raw) = std::env::var(TIMEOUT_ENV) {
        match raw.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => return Duration::from_secs(secs),
            Ok(_) => {}
            Err(_) => {
                static WARNED: Once = Once::new();
                WARNED.call_once(|| {
                    tracing::warn!(
                        value = %raw,
                        "{TIMEOUT_ENV} is not a whole number of seconds, so it is ignored. Set it \
                         to a positive number such as 120"
                    )
                });
            }
        }
    }
    if configured_ms > 0 {
        Duration::from_millis(configured_ms)
    } else {
        Duration::from_secs(DEFAULT_TIMEOUT_SECS)
    }
}

/// Make [`timeout`] follow a project setting. The daemon and the CLI call
/// this once at start with `[daemon] keychain_approval_timeout_secs`; zero
/// clears it.
pub fn configure_timeout(configured: Duration) {
    let ms = u64::try_from(configured.as_millis()).unwrap_or(u64::MAX);
    CONFIGURED_TIMEOUT_MS.store(ms, Ordering::SeqCst);
}

/// How long one keychain call may take in this process before it is
/// abandoned with an error that says what to do.
pub fn timeout() -> Duration {
    resolve_ms(CONFIGURED_TIMEOUT_MS.load(Ordering::SeqCst))
}

fn show(d: Duration) -> String {
    if d.is_zero() || d.as_secs() > 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// A keychain implementation. `Ok(None)` from `get` means "no such item".
pub trait KeychainBackend: Send + Sync {
    fn get(&self, service: &str, user: &str) -> Result<Option<String>, String>;
    fn set(&self, service: &str, user: &str, value: &str) -> Result<(), String>;
    /// Returns whether an item was removed.
    fn delete(&self, service: &str, user: &str) -> Result<bool, String>;
    /// True only for the real OS keychain, which tests must never reach.
    fn is_real(&self) -> bool {
        false
    }
}

/// The real OS keychain (macOS Keychain, Windows Credential Manager, Secret
/// Service on Linux).
pub struct OsKeychain;

impl KeychainBackend for OsKeychain {
    fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(service, user)
            .map_err(|e| format!("keychain entry {service}/{user} could not be created: {e}"))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, service: &str, user: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(service, user)
            .map_err(|e| format!("keychain entry {service}/{user} could not be created: {e}"))?
            .set_password(value)
            .map_err(|e| e.to_string())
    }
    fn delete(&self, service: &str, user: &str) -> Result<bool, String> {
        let entry = keyring::Entry::new(service, user)
            .map_err(|e| format!("keychain entry {service}/{user} could not be created: {e}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
    fn is_real(&self) -> bool {
        true
    }
}

/// What the startup preload found for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cached {
    Value(String),
    Absent,
    Unavailable(String),
}

/// A keychain call that timed out and whose helper thread has not returned.
struct Stall {
    call: String,
    timeout: Duration,
    done: Arc<AtomicBool>,
}

struct Global {
    backend: Arc<dyn KeychainBackend>,
    request_mode: bool,
    cache: HashMap<(String, String), Cached>,
    /// While set and not `done`, direct-mode calls fail at once instead of
    /// queueing another wait behind the stuck one.
    stall: Option<Stall>,
}

fn global() -> &'static RwLock<Global> {
    static G: OnceLock<RwLock<Global>> = OnceLock::new();
    G.get_or_init(|| {
        RwLock::new(Global {
            backend: Arc::new(OsKeychain),
            request_mode: false,
            cache: HashMap::new(),
            stall: None,
        })
    })
}

fn read<R>(f: impl FnOnce(&Global) -> R) -> R {
    f(&global().read().unwrap_or_else(|e| e.into_inner()))
}

fn write<R>(f: impl FnOnce(&mut Global) -> R) -> R {
    f(&mut global().write().unwrap_or_else(|e| e.into_inner()))
}

/// Replace the backend (tests and embedders). Clears nothing else.
pub fn install_backend(backend: Arc<dyn KeychainBackend>) {
    write(|g| {
        g.backend = backend;
        g.stall = None;
    });
}

/// Back to the real OS keychain, direct mode, empty cache, default timeout.
pub fn reset() {
    CONFIGURED_TIMEOUT_MS.store(0, Ordering::SeqCst);
    write(|g| {
        g.backend = Arc::new(OsKeychain);
        g.request_mode = false;
        g.cache.clear();
        g.stall = None;
    });
}

/// True when the installed backend is the real OS keychain.
pub fn is_real_backend() -> bool {
    read(|g| g.backend.is_real())
}

/// Switch to request mode: from now on reads are served from the preloaded
/// cache and never reach the backend.
pub fn enter_request_mode() {
    write(|g| g.request_mode = true);
}

pub fn request_mode() -> bool {
    read(|g| g.request_mode)
}

/// An item as messages name it: `service/user` and what it is for.
pub fn describe_item(service: &str, user: &str) -> String {
    let purpose = match (service, user) {
        (VAULT_SERVICE, VAULT_ITEM) => Some("credential vault encryption key"),
        (HUMAN_SERVICE, HUMAN_ITEM) => Some("human credential from `ta login`"),
        (CONTROL_PLANE_SERVICE, u) if u.ends_with("/broker-root-key") => {
            Some("capability-token root key")
        }
        (CONTROL_PLANE_SERVICE, u) if u.ends_with("/audit-key") => Some("audit-log signing key"),
        (CONTROL_PLANE_SERVICE, u) if u.ends_with("/vault-key") => Some("credential vault key"),
        (CONTROL_PLANE_SERVICE, u) if u.ends_with("/rules-owner-key") => Some("rules-owner key"),
        _ => None,
    };
    match purpose {
        Some(p) => format!("{service}/{user} ({p})"),
        None => format!("{service}/{user}"),
    }
}

fn miss_error(service: &str, user: &str) -> String {
    format!(
        "keychain item {} was not loaded when the daemon started, and the daemon \
         never reads the keychain while serving requests (a locked keychain or an approval \
         dialog would freeze it). Run `ta daemon restart` so startup loads it, or run \
         `ta credentials custody file` (or set TA_NO_KEYCHAIN=1) and restart",
        describe_item(service, user)
    )
}

/// What a person can do when the keychain stops answering.
fn how_to_fix(limit: Duration) -> String {
    format!(
        "Fix: approve the dialog at the machine's console (choose Always Allow) and run the \
         command again; or stop using the keychain with `ta credentials custody file` (or set \
         TA_NO_KEYCHAIN=1), so nothing prompts. To change how long TA waits, set \
         {TIMEOUT_SETTING} or the {TIMEOUT_ENV} environment variable (now {}, default \
         {DEFAULT_TIMEOUT_SECS}s)",
        show(limit)
    )
}

#[derive(Clone, Copy)]
enum Op {
    Read,
    Write,
    Remove,
}

impl Op {
    fn call(self, service: &str, user: &str) -> String {
        let verb = match self {
            Op::Read => "reading",
            Op::Write => "writing",
            Op::Remove => "removing",
        };
        format!("{verb} keychain item {}", describe_item(service, user))
    }
}

/// Run one keychain call on a helper thread and wait at most [`timeout`] for
/// it. A keychain that is waiting on an approval dialog nobody can see never
/// answers, so an unbounded wait here is a hung process. On timeout the helper
/// is left to finish by itself and the call is recorded as stalled: later
/// direct calls fail at once, with the same advice, until it returns.
fn direct<T: Send + 'static>(
    op: Op,
    service: &str,
    user: &str,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let limit = timeout();
    let call = op.call(service, user);
    let stalled = read(|g| {
        g.stall
            .as_ref()
            .filter(|s| !s.done.load(Ordering::SeqCst))
            .map(|s| (s.call.clone(), s.timeout))
    });
    if let Some((earlier, waited)) = stalled {
        return Err(format!(
            "the OS keychain is still not answering: an earlier call ({earlier}) timed out after \
             {} and has not returned, so {call} was not attempted. {}",
            show(waited),
            how_to_fix(limit)
        ));
    }
    let done = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let flag = done.clone();
    std::thread::Builder::new()
        .name("keychain-call".into())
        .spawn(move || {
            let result = f();
            flag.store(true, Ordering::SeqCst);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("could not start a thread for {call}: {e}"))?;
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            write(|g| {
                g.stall = Some(Stall {
                    call: call.clone(),
                    timeout: limit,
                    done,
                })
            });
            tracing::error!(
                call = %call,
                timeout_ms = limit.as_millis() as u64,
                "the OS keychain did not answer in time; the call was abandoned"
            );
            Err(format!(
                "the OS keychain did not answer within {} while {call}. It is most likely \
                 showing an approval dialog nobody can click (an SSH or headless session, a \
                 locked screen, or a Mac with nobody logged in at the console), so the call was \
                 abandoned instead of waiting for it. {}",
                show(limit),
                how_to_fix(limit)
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!(
            "the keychain call panicked while {call}. Run `ta credentials custody file` (or set \
             TA_NO_KEYCHAIN=1) to stop using the keychain"
        )),
    }
}

/// Read an item. Direct mode asks the backend, giving up after [`timeout`];
/// request mode answers from the preloaded cache only (a miss is an error,
/// never a backend call).
pub fn get(service: &str, user: &str) -> Result<Option<String>, String> {
    let key = (service.to_string(), user.to_string());
    let (backend, request_mode, cached) = read(|g| {
        (
            g.backend.clone(),
            g.request_mode,
            g.cache.get(&key).cloned(),
        )
    });
    if request_mode {
        return match cached {
            Some(Cached::Value(v)) => Ok(Some(v)),
            Some(Cached::Absent) => Ok(None),
            Some(Cached::Unavailable(reason)) => Err(format!(
                "keychain item {} could not be read at daemon start ({reason}). Approve the \
                 keychain prompt (Always Allow) and run `ta daemon restart`, or run \
                 `ta credentials custody file` (or set TA_NO_KEYCHAIN=1) and restart",
                describe_item(service, user)
            )),
            None => Err(miss_error(service, user)),
        };
    }
    direct(Op::Read, service, user, move || {
        let (s, u) = key;
        backend.get(&s, &u)
    })
}

/// Read one item through the backend regardless of mode and remember the
/// result in the cache. This is what the startup preload calls, on a blocking
/// thread. It also completes a read that finished after the startup timeout:
/// the late result fills the cache, so a delayed approval still helps.
pub fn preload(service: &str, user: &str) -> Cached {
    let backend = read(|g| g.backend.clone());
    let found = match backend.get(service, user) {
        Ok(Some(v)) => Cached::Value(v),
        Ok(None) => Cached::Absent,
        Err(e) => Cached::Unavailable(e),
    };
    write(|g| {
        g.cache
            .insert((service.to_string(), user.to_string()), found.clone());
    });
    found
}

fn bounded<T: Send + 'static>(
    what: String,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(REQUEST_WRITE_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(format!(
            "{what} did not finish within {}s (the keychain may be locked or waiting for an \
             approval dialog); the operation was abandoned so the daemon keeps serving. Approve \
             the dialog at the console, or run `ta credentials custody file` and \
             `ta daemon restart` to stop using the keychain",
            REQUEST_WRITE_TIMEOUT.as_secs()
        )),
    }
}

/// Write an item. Direct mode gives up after [`timeout`]; request mode runs
/// the write on a helper thread with `REQUEST_WRITE_TIMEOUT`. The cache is
/// updated on success.
pub fn set(service: &str, user: &str, value: &str) -> Result<(), String> {
    let (backend, request_mode) = read(|g| (g.backend.clone(), g.request_mode));
    let (s, u, v) = (service.to_string(), user.to_string(), value.to_string());
    if request_mode {
        bounded(Op::Write.call(service, user), move || {
            backend.set(&s, &u, &v)
        })?;
    } else {
        direct(Op::Write, service, user, move || backend.set(&s, &u, &v))?;
    }
    write(|g| {
        g.cache.insert(
            (service.to_string(), user.to_string()),
            Cached::Value(value.to_string()),
        );
    });
    Ok(())
}

/// Remove an item (same bounds as [`set`]).
pub fn delete(service: &str, user: &str) -> Result<bool, String> {
    let (backend, request_mode) = read(|g| (g.backend.clone(), g.request_mode));
    let (s, u) = (service.to_string(), user.to_string());
    let removed = if request_mode {
        bounded(Op::Remove.call(service, user), move || {
            backend.delete(&s, &u)
        })?
    } else {
        direct(Op::Remove, service, user, move || backend.delete(&s, &u))?
    };
    write(|g| {
        g.cache
            .insert((service.to_string(), user.to_string()), Cached::Absent);
    });
    Ok(removed)
}

/// Record why an item could not be loaded, unless a read already filled the
/// cache. A read that finishes later still replaces it.
pub fn mark_unavailable_if_absent(service: &str, user: &str, reason: &str) {
    write(|g| {
        g.cache
            .entry((service.to_string(), user.to_string()))
            .or_insert_with(|| Cached::Unavailable(reason.to_string()));
    });
}

/// One keychain item the daemon needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainItem {
    pub service: String,
    pub user: String,
    /// What the item is for, for the "waiting for approval" log line.
    pub purpose: &'static str,
}

impl KeychainItem {
    pub fn label(&self) -> String {
        format!("{} ({}/{})", self.purpose, self.service, self.user)
    }
}

/// Every keychain item the daemon will read for `project_root`: the broker
/// root key, the audit signing key and the credential vault's age identity.
/// Empty when the keychain is not in use (file custody, `TA_NO_KEYCHAIN`, a test binary).
pub fn startup_items(project_root: &Path) -> Result<Vec<KeychainItem>, String> {
    if !crate::custody::use_keychain() {
        return Ok(Vec::new());
    }
    items_for_project(project_root)
}

/// [`startup_items`] without the "is the keychain in use" gate, so tests can
/// ask what would be read.
pub fn items_for_project(project_root: &Path) -> Result<Vec<KeychainItem>, String> {
    use crate::state::{ControlSecret, ProjectState, StateLocator};
    let locator = StateLocator::from_env().map_err(|e| e.to_string())?;
    let ps = ProjectState::open(project_root, &locator).map_err(|e| e.to_string())?;
    let mut items = Vec::new();
    for (which, purpose) in [
        (ControlSecret::BrokerRootKey, "capability-token root key"),
        (ControlSecret::AuditKey, "audit-log signing key"),
    ] {
        items.push(KeychainItem {
            service: CONTROL_PLANE_SERVICE.to_string(),
            user: format!("{}/{}", ps.project_id, which.name()),
            purpose,
        });
    }
    items.push(KeychainItem {
        service: VAULT_SERVICE.to_string(),
        user: VAULT_ITEM.to_string(),
        purpose: "credential vault encryption key",
    });
    Ok(items)
}

#[cfg(test)]
pub(crate) mod testing {
    //! Serializes tests that touch the process-wide keychain state.
    use super::*;
    use std::sync::Mutex;

    pub static LOCK: Mutex<()> = Mutex::new(());

    /// In-memory keychain that counts calls.
    #[derive(Default)]
    pub struct FakeKeychain {
        pub items: Mutex<HashMap<(String, String), String>>,
        pub gets: std::sync::atomic::AtomicUsize,
    }

    impl KeychainBackend for FakeKeychain {
        fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.to_string(), user.to_string()))
                .cloned())
        }
        fn set(&self, service: &str, user: &str, value: &str) -> Result<(), String> {
            self.items
                .lock()
                .unwrap()
                .insert((service.to_string(), user.to_string()), value.to_string());
            Ok(())
        }
        fn delete(&self, service: &str, user: &str) -> Result<bool, String> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .remove(&(service.to_string(), user.to_string()))
                .is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn request_mode_serves_from_the_cache_and_never_calls_the_backend() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let fake = Arc::new(FakeKeychain::default());
        fake.set("svc", "item", "secret").unwrap();
        install_backend(fake.clone());

        assert_eq!(preload("svc", "item"), Cached::Value("secret".into()));
        assert_eq!(preload("svc", "gone"), Cached::Absent);
        let after_preload = fake.gets.load(std::sync::atomic::Ordering::SeqCst);
        enter_request_mode();

        assert_eq!(get("svc", "item").unwrap().as_deref(), Some("secret"));
        assert_eq!(get("svc", "gone").unwrap(), None);
        let err = get("svc", "never-loaded").unwrap_err();
        assert!(err.contains("ta daemon restart"), "no next step: {err}");
        assert_eq!(
            fake.gets.load(std::sync::atomic::Ordering::SeqCst),
            after_preload,
            "request mode must not read the backend"
        );
        reset();
    }

    #[test]
    fn direct_mode_reads_the_backend() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let fake = Arc::new(FakeKeychain::default());
        fake.set("svc", "item", "v1").unwrap();
        install_backend(fake.clone());
        assert_eq!(get("svc", "item").unwrap().as_deref(), Some("v1"));
        assert_eq!(fake.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        reset();
    }

    #[test]
    fn an_unreadable_item_is_an_actionable_error_in_request_mode() {
        struct Denied;
        impl KeychainBackend for Denied {
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
        reset();
        install_backend(Arc::new(Denied));
        assert!(matches!(preload("svc", "x"), Cached::Unavailable(_)));
        enter_request_mode();
        let err = get("svc", "x").unwrap_err();
        assert!(err.contains("user denied access") && err.contains("ta daemon restart"));
        reset();
    }

    #[test]
    fn a_hung_write_in_request_mode_times_out_instead_of_blocking() {
        struct Hangs;
        impl KeychainBackend for Hangs {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
                std::thread::sleep(REQUEST_WRITE_TIMEOUT + Duration::from_secs(2));
                Ok(())
            }
            fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        install_backend(Arc::new(Hangs));
        enter_request_mode();
        let started = std::time::Instant::now();
        let err = set("svc", "x", "v").unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
        assert!(started.elapsed() < REQUEST_WRITE_TIMEOUT + Duration::from_secs(1));
        reset();
    }

    #[test]
    fn items_for_project_names_the_control_secrets_and_the_vault_key() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::env::set_var(crate::state::STATE_ROOT_ENV, state.path());
        let items = items_for_project(project.path()).unwrap();
        std::env::remove_var(crate::state::STATE_ROOT_ENV);
        let users: Vec<_> = items.iter().map(|i| i.user.clone()).collect();
        assert_eq!(items.len(), 3, "{users:?}");
        assert!(users.iter().any(|u| u.ends_with("/broker-root-key")));
        assert!(users.iter().any(|u| u.ends_with("/audit-key")));
        assert!(items.iter().any(|i| i.service == VAULT_SERVICE));
    }

    /// A backend whose every call waits for `release`, like a keychain
    /// that is showing an approval dialog nobody clicks.
    struct Blocks {
        release: Arc<AtomicBool>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Blocks {
        fn new() -> (
            Arc<Self>,
            Arc<AtomicBool>,
            Arc<std::sync::atomic::AtomicUsize>,
        ) {
            let release = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let me = Arc::new(Blocks {
                release: release.clone(),
                calls: calls.clone(),
            });
            (me, release, calls)
        }
        fn wait(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let started = std::time::Instant::now();
            while !self.release.load(Ordering::SeqCst)
                && started.elapsed() < Duration::from_secs(20)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl KeychainBackend for Blocks {
        fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
            self.wait();
            Ok(None)
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            self.wait();
            Ok(())
        }
        fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
            self.wait();
            Ok(false)
        }
    }

    fn assert_actionable(err: &str, item: &str, waited: &str) {
        for needle in [
            item,
            waited,
            "did not answer",
            "Always Allow",
            "ta credentials custody file",
            "TA_NO_KEYCHAIN=1",
            "keychain_approval_timeout_secs",
            TIMEOUT_ENV,
        ] {
            assert!(err.contains(needle), "missing `{needle}` in: {err}");
        }
    }

    #[test]
    fn a_direct_read_that_never_answers_times_out_naming_the_item_and_the_fix() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let (blocks, release, _) = Blocks::new();
        install_backend(blocks);
        configure_timeout(Duration::from_millis(150));
        let started = std::time::Instant::now();
        let err = get(VAULT_SERVICE, VAULT_ITEM).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "did not return");
        assert_actionable(&err, "credential vault encryption key", "150ms");
        assert!(
            err.contains(VAULT_SERVICE) && err.contains(VAULT_ITEM),
            "{err}"
        );
        assert!(err.contains("reading keychain item"), "{err}");
        release.store(true, Ordering::SeqCst);
        reset();
    }

    #[test]
    fn a_direct_write_and_delete_are_bounded_too() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for write_not_delete in [true, false] {
            reset();
            let (blocks, release, _) = Blocks::new();
            install_backend(blocks);
            configure_timeout(Duration::from_millis(150));
            let started = std::time::Instant::now();
            let err = if write_not_delete {
                set("svc", "item", "v").unwrap_err()
            } else {
                delete("svc", "item").unwrap_err()
            };
            assert!(started.elapsed() < Duration::from_secs(5), "did not return");
            assert_actionable(&err, "svc/item", "150ms");
            assert!(
                err.contains(if write_not_delete {
                    "writing"
                } else {
                    "removing"
                }),
                "{err}"
            );
            release.store(true, Ordering::SeqCst);
        }
        reset();
    }

    #[test]
    fn after_a_timeout_later_calls_fail_at_once_until_the_stuck_call_returns() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let (blocks, release, calls) = Blocks::new();
        install_backend(blocks);
        configure_timeout(Duration::from_millis(150));
        get("svc", "first").unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let started = std::time::Instant::now();
        let err = get("svc", "second").unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "waited again"
        );
        assert!(err.contains("still not answering"), "{err}");
        assert!(
            err.contains("svc/first") && err.contains("svc/second"),
            "{err}"
        );
        assert!(err.contains("ta credentials custody file"), "{err}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a second call reached the backend"
        );

        // The dialog is finally approved: the stuck call returns and the
        // keychain works again.
        release.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match get("svc", "third") {
                Ok(v) => {
                    assert_eq!(v, None);
                    break;
                }
                Err(e) => {
                    assert!(std::time::Instant::now() < deadline, "never recovered: {e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        reset();
    }

    #[test]
    fn the_wait_is_env_then_setting_then_sixty_seconds() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let saved = std::env::var(TIMEOUT_ENV).ok();
        std::env::remove_var(TIMEOUT_ENV);
        assert_eq!(resolve_timeout(0), Duration::from_secs(60), "default");
        assert_eq!(timeout(), Duration::from_secs(60));
        assert_eq!(resolve_timeout(15), Duration::from_secs(15), "setting");
        configure_timeout(Duration::from_secs(20));
        assert_eq!(timeout(), Duration::from_secs(20), "configured");
        std::env::set_var(TIMEOUT_ENV, "7");
        assert_eq!(resolve_timeout(15), Duration::from_secs(7), "env wins");
        assert_eq!(
            timeout(),
            Duration::from_secs(7),
            "env wins over configured"
        );
        for ignored in ["abc", "0", "-4", ""] {
            std::env::set_var(TIMEOUT_ENV, ignored);
            assert_eq!(resolve_timeout(15), Duration::from_secs(15), "`{ignored}`");
        }
        match saved {
            Some(v) => std::env::set_var(TIMEOUT_ENV, v),
            None => std::env::remove_var(TIMEOUT_ENV),
        }
        reset();
    }

    #[test]
    fn items_are_described_by_what_they_are_for() {
        assert_eq!(
            describe_item(VAULT_SERVICE, VAULT_ITEM),
            format!("{VAULT_SERVICE}/{VAULT_ITEM} (credential vault encryption key)")
        );
        let broker = describe_item(CONTROL_PLANE_SERVICE, "abc123/broker-root-key");
        assert!(broker.contains("capability-token root key"), "{broker}");
        let audit = describe_item(CONTROL_PLANE_SERVICE, "abc123/audit-key");
        assert!(audit.contains("audit-log signing key"), "{audit}");
        assert_eq!(describe_item("svc", "x"), "svc/x");
    }

    #[test]
    fn items_the_reader_never_reached_fail_with_the_startup_reason() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let fake = Arc::new(FakeKeychain::default());
        fake.set("svc", "loaded", "secret").unwrap();
        install_backend(fake);
        assert_eq!(preload("svc", "loaded"), Cached::Value("secret".into()));
        mark_unavailable_if_absent("svc", "loaded", "timed out");
        mark_unavailable_if_absent("svc", "stuck", "timed out after 60s");
        enter_request_mode();
        assert_eq!(get("svc", "loaded").unwrap().as_deref(), Some("secret"));
        let err = get("svc", "stuck").unwrap_err();
        assert!(err.contains("timed out after 60s"), "{err}");
        assert!(err.contains("ta credentials custody file"), "{err}");
        reset();
    }
}
