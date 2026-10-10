//! The guard must also protect dependents' test binaries, which are built
//! without `cfg(test)` for this crate. This integration test binary behaves
//! like one: it lives in `target/<profile>/deps/`, which is what the guard
//! keys on.

use std::sync::Mutex;

use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};

/// Both tests read or write `TA_NO_KEYCHAIN`; serialize them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn keychain_use_in_a_dependent_test_binary_panics_with_actionable_message() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TA_NO_KEYCHAIN");
    let dir = tempfile::tempdir().unwrap();
    let config = CredentialsConfig {
        vault_path: dir.path().join("credentials.json"),
        use_keychain: true,
    };
    let result = std::panic::catch_unwind(|| FileVault::open(&config).map(|_| ()));
    let payload = result.expect_err("opening a vault with use_keychain=true must panic in tests");
    let msg = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("real OS keychain"),
        "unexpected message: {msg}"
    );
    assert!(
        msg.contains("keychain_use_in_a_dependent_test_binary"),
        "message must name the offending test: {msg}"
    );
    assert!(msg.contains("use_keychain: false"), "no fix hint: {msg}");
}

#[test]
fn ta_no_keychain_env_wins_over_use_keychain_true() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("TA_NO_KEYCHAIN", "1");
    let dir = tempfile::tempdir().unwrap();
    let config = CredentialsConfig {
        vault_path: dir.path().join("credentials.json"),
        use_keychain: true,
    };
    let vault = FileVault::open(&config).expect("file custody must work with TA_NO_KEYCHAIN set");
    assert!(vault.list().unwrap().is_empty());
    assert!(
        dir.path().join("credentials.key").exists(),
        "key must be a file next to the vault, not in the keychain"
    );
    std::env::remove_var("TA_NO_KEYCHAIN");
}
