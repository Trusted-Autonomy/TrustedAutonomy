//! Once a daemon has finished starting, no secret may be read from the OS
//! keychain on the request path: a locked or revoked keychain, or an approval
//! dialog nobody clicked, must never freeze HTTP. These tests drive the real
//! broker and credential vault through an injected keychain (never the real
//! one) that panics if it is read after startup.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ta_credential_broker::CredentialBroker;
use ta_credentials::keychain::{self, KeychainBackend, CONTROL_PLANE_SERVICE};
use ta_credentials::state::{ControlSecret, CustodyOpts, Posture, ProjectState, StateLocator};
use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};

/// The keychain state is process-wide; serialize the tests in this binary.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct PanicAfterStartup {
    items: Mutex<HashMap<(String, String), String>>,
    armed: AtomicBool,
    gets_before_arm: AtomicUsize,
    sets: AtomicUsize,
}

impl KeychainBackend for PanicAfterStartup {
    fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
        assert!(
            !self.armed.load(Ordering::SeqCst),
            "keychain READ on the request path: {service}/{user}"
        );
        self.gets_before_arm.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .items
            .lock()
            .unwrap()
            .get(&(service.into(), user.into()))
            .cloned())
    }
    fn set(&self, service: &str, user: &str, value: &str) -> Result<(), String> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.items
            .lock()
            .unwrap()
            .insert((service.into(), user.into()), value.into());
        Ok(())
    }
    fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
        Ok(false)
    }
}

fn keychain_opts() -> CustodyOpts {
    CustodyOpts {
        use_keychain: true,
        posture: Posture::Default,
    }
}

#[test]
fn broker_and_vault_never_read_the_keychain_after_startup() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    keychain::reset();
    let fake = Arc::new(PanicAfterStartup::default());
    keychain::install_backend(fake.clone());

    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ta_dir = project.path().join(".ta");
    let locator = StateLocator::at(state.path());
    let ps = ProjectState::open(project.path(), &locator).unwrap();

    // Startup: the broker creates its root key in the (fake) keychain, then the
    // daemon preloads every item it will need.
    CredentialBroker::open_with_state(&ta_dir, &ps, keychain_opts()).unwrap();
    let root_key_item = format!("{}/{}", ps.project_id, ControlSecret::BrokerRootKey.name());
    let preloaded = keychain::preload(CONTROL_PLANE_SERVICE, &root_key_item);
    assert!(matches!(preloaded, keychain::Cached::Value(_)));
    keychain::preload(keychain::VAULT_SERVICE, keychain::VAULT_ITEM);
    assert!(fake.gets_before_arm.load(Ordering::SeqCst) >= 2);

    // Startup is over: any keychain read from here on panics the fake.
    fake.armed.store(true, Ordering::SeqCst);
    keychain::enter_request_mode();

    // Request path 1: open the broker repeatedly and mint + verify a grant.
    for _ in 0..3 {
        let broker = CredentialBroker::open_with_state(&ta_dir, &ps, keychain_opts()).unwrap();
        let grant = broker
            .grant(
                uuid::Uuid::new_v4(),
                "agent",
                vec!["whiteboard:presence".into()],
                60,
            )
            .unwrap();
        broker.verify(&grant.token).unwrap();
    }

    // Request path 2: open the credential vault and use it.
    let cfg = CredentialsConfig {
        vault_path: project.path().join("credentials.json"),
        use_keychain: true,
    };
    let mut vault = FileVault::open(&cfg).unwrap();
    vault
        .add("nats-credentials", "nats", "s3cret", vec![])
        .unwrap();
    assert_eq!(FileVault::open(&cfg).unwrap().list().unwrap().len(), 1);

    keychain::reset();
}

#[test]
fn an_unreadable_keychain_refuses_instead_of_minting_a_replacement_root_key() {
    struct Locked {
        sets: AtomicUsize,
    }
    impl KeychainBackend for Locked {
        fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
            Err("keychain is locked".into())
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            self.sets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
            Ok(false)
        }
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    keychain::reset();
    let locked = Arc::new(Locked {
        sets: AtomicUsize::new(0),
    });
    keychain::install_backend(locked.clone());

    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let locator = StateLocator::at(state.path());
    let ps = ProjectState::open(project.path(), &locator).unwrap();

    let result =
        CredentialBroker::open_with_state(&project.path().join(".ta"), &ps, keychain_opts());
    let err = match result {
        Ok(_) => panic!("a locked keychain must not yield a broker"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("keychain could not be read"), "{err}");
    assert!(err.contains("ta daemon restart"), "no next step: {err}");
    assert_eq!(
        locked.sets.load(Ordering::SeqCst),
        0,
        "a replacement key must never be written over an unreadable one"
    );
    keychain::reset();
}
