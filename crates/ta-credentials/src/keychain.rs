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
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

/// Longest a write or delete may take in request mode before it is abandoned
/// with an error (the helper thread is left to finish on its own).
pub const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Keychain service of the control-plane secrets (broker root key, audit key).
pub const CONTROL_PLANE_SERVICE: &str = "trusted-autonomy-control-plane";
/// Keychain service and item of the credential vault's age identity.
pub const VAULT_SERVICE: &str = "trusted-autonomy-vault";
pub const VAULT_ITEM: &str = "credential-vault-age-identity";

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

struct Global {
    backend: Arc<dyn KeychainBackend>,
    request_mode: bool,
    cache: HashMap<(String, String), Cached>,
}

fn global() -> &'static RwLock<Global> {
    static G: OnceLock<RwLock<Global>> = OnceLock::new();
    G.get_or_init(|| {
        RwLock::new(Global {
            backend: Arc::new(OsKeychain),
            request_mode: false,
            cache: HashMap::new(),
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
    write(|g| g.backend = backend);
}

/// Back to the real OS keychain, direct mode, empty cache.
pub fn reset() {
    write(|g| {
        g.backend = Arc::new(OsKeychain);
        g.request_mode = false;
        g.cache.clear();
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

fn miss_error(service: &str, user: &str) -> String {
    format!(
        "keychain item {service}/{user} was not loaded when the daemon started, and the daemon \
         never reads the keychain while serving requests (a locked keychain or an approval \
         dialog would freeze it). Run `ta daemon restart` so startup loads it"
    )
}

/// Read an item. Direct mode asks the backend; request mode answers from the
/// preloaded cache only (a miss is an error, never a backend call).
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
                "keychain item {service}/{user} could not be read at daemon start ({reason}). \
                 Approve the keychain prompt and run `ta daemon restart`"
            )),
            None => Err(miss_error(service, user)),
        };
    }
    backend.get(service, user)
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
             approval dialog); the operation was abandoned so the daemon keeps serving",
            REQUEST_WRITE_TIMEOUT.as_secs()
        )),
    }
}

/// Write an item. In request mode the write runs on a helper thread with
/// `REQUEST_WRITE_TIMEOUT`, and the cache is updated on success.
pub fn set(service: &str, user: &str, value: &str) -> Result<(), String> {
    let (backend, request_mode) = read(|g| (g.backend.clone(), g.request_mode));
    if !request_mode {
        backend.set(service, user, value)?;
        write(|g| {
            g.cache.insert(
                (service.to_string(), user.to_string()),
                Cached::Value(value.to_string()),
            );
        });
        return Ok(());
    }
    let (s, u, v) = (service.to_string(), user.to_string(), value.to_string());
    bounded(
        format!("writing keychain item {service}/{user}"),
        move || backend.set(&s, &u, &v),
    )?;
    write(|g| {
        g.cache.insert(
            (service.to_string(), user.to_string()),
            Cached::Value(value.to_string()),
        );
    });
    Ok(())
}

/// Remove an item (same bounded behavior as [`set`] in request mode).
pub fn delete(service: &str, user: &str) -> Result<bool, String> {
    let (backend, request_mode) = read(|g| (g.backend.clone(), g.request_mode));
    let removed = if request_mode {
        let (s, u) = (service.to_string(), user.to_string());
        bounded(
            format!("removing keychain item {service}/{user}"),
            move || backend.delete(&s, &u),
        )?
    } else {
        backend.delete(service, user)?
    };
    write(|g| {
        g.cache
            .insert((service.to_string(), user.to_string()), Cached::Absent);
    });
    Ok(removed)
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
/// Empty when the keychain is not in use (`TA_NO_KEYCHAIN`, a test binary).
pub fn startup_items(project_root: &Path) -> Result<Vec<KeychainItem>, String> {
    if !crate::human::default_use_keychain() {
        return Ok(Vec::new());
    }
    items_for_project(project_root)
}

/// [`startup_items`] without the "is the keychain in use" gate, so tests can
/// ask what would be read.
pub fn items_for_project(project_root: &Path) -> Result<Vec<KeychainItem>, String> {
    use crate::state::{ControlSecret, ProjectState, StateLocator};
    let locator = StateLocator::from_env();
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
}
