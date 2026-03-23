//! Integration test: Perforce VCS plugin via JSON-over-stdio.
//!
//! Uses a mock Perforce plugin that speaks the TA VCS plugin protocol to verify
//! the full handshake → save_state → restore_state flow without requiring a
//! live Perforce installation.
//!
//! Unix-only: the mock plugin is a shell script and relies on Unix executable
//! permissions. Windows CI skips this test file entirely.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ta_submit::external_vcs_adapter::ExternalVcsAdapter;
use ta_submit::vcs_plugin_manifest::VcsPluginManifest;
use ta_submit::SourceAdapter;

// ---------------------------------------------------------------------------
// Mock Perforce plugin — speaks the TA VCS plugin protocol with canned p4-like
// responses, no real p4 CLI required.
// ---------------------------------------------------------------------------

const MOCK_PERFORCE_PLUGIN: &str = r#"#!/bin/sh
# Mock Perforce VCS plugin for TA integration testing.
# Reads one JSON line, dispatches by method, returns hardcoded success responses.

read -r line
method=$(echo "$line" | awk -F'"' '{print $4}')

case "$method" in
  handshake)
    echo '{"ok":true,"result":{"plugin_version":"0.1.0","protocol_version":1,"adapter_name":"perforce","capabilities":["status","diff","submit","shelve","save_state","restore_state","revision_id","protected_targets","verify_target","sync_upstream","check_review","merge_review","commit","push","open_review"]}}'
    ;;
  detect)
    echo '{"ok":true,"result":{"detected":true}}'
    ;;
  exclude_patterns)
    echo '{"ok":true,"result":{"patterns":[".p4config",".p4ignore"]}}'
    ;;
  prepare)
    echo '{"ok":true,"result":{}}'
    ;;
  status)
    echo '{"ok":true,"result":{"changed_files":["//depot/foo.rs#1 - reconcile to add","//depot/bar.rs#2 - reconcile to edit"],"raw":"//depot/foo.rs#1 - reconcile to add\n//depot/bar.rs#2 - reconcile to edit\n"}}'
    ;;
  save_state)
    echo '{"ok":true,"result":{"state":{"changelist":"12344"}}}'
    ;;
  restore_state)
    echo '{"ok":true,"result":{}}'
    ;;
  commit)
    echo '{"ok":true,"result":{"commit_id":"12345","message":"Change 12345 shelved.","metadata":{}}}'
    ;;
  push)
    echo '{"ok":true,"result":{"remote_ref":"perforce://depot","message":"Change 12345 submitted.","metadata":{}}}'
    ;;
  open_review)
    echo '{"ok":true,"result":{"review_url":"perforce://changelist/12345","review_id":"12345","message":"Change 12345 shelved.","metadata":{}}}'
    ;;
  revision_id)
    echo '{"ok":true,"result":{"revision_id":"12345"}}'
    ;;
  protected_targets)
    echo '{"ok":true,"result":{"targets":[]}}'
    ;;
  verify_target)
    echo '{"ok":true,"result":{}}'
    ;;
  sync_upstream)
    echo '{"ok":true,"result":{"updated":true,"conflicts":[],"new_commits":0,"message":"Synced","metadata":{}}}'
    ;;
  check_review)
    echo '{"ok":true,"result":{"found":false,"state":"unknown","checks_passing":false}}'
    ;;
  merge_review)
    echo '{"ok":true,"result":{"merged":false,"merge_commit":"","message":"Not implemented","metadata":{}}}'
    ;;
  *)
    echo '{"ok":false,"error":"Unknown method"}'
    ;;
esac
"#;

/// Returns the path to the shared mock Perforce plugin binary.
/// Written exactly once per test process using `OnceLock` to avoid ETXTBSY.
fn mock_plugin_path() -> &'static PathBuf {
    static PLUGIN: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &PLUGIN
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = write_plugin_binary(dir.path());
            (dir, path)
        })
        .1
}

fn write_plugin_binary(dir: &Path) -> PathBuf {
    let path = dir.join("ta-submit-mock-perforce");
    {
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(MOCK_PERFORCE_PLUGIN.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    // Read back to force overlayfs copy-up to complete (avoids ETXTBSY on Nix CI).
    let _ = std::fs::read(&path).unwrap();
    path
}

fn mock_manifest() -> VcsPluginManifest {
    VcsPluginManifest {
        name: "perforce".to_string(),
        version: "0.1.0".to_string(),
        plugin_type: "vcs".to_string(),
        command: mock_plugin_path().display().to_string(),
        args: vec![],
        capabilities: vec![
            "status".to_string(),
            "diff".to_string(),
            "submit".to_string(),
            "shelve".to_string(),
            "protected_targets".to_string(),
        ],
        description: Some("Mock Perforce plugin for testing".to_string()),
        timeout_secs: 10,
        min_daemon_version: None,
        source_url: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn perforce_plugin_handshake_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ExternalVcsAdapter::new(&mock_manifest(), dir.path(), "0.13.17-alpha")
        .expect("handshake should succeed");
    assert_eq!(adapter.name(), "perforce");
}

#[test]
fn perforce_plugin_exclude_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ExternalVcsAdapter::new(&mock_manifest(), dir.path(), "0.13.17-alpha").unwrap();
    let patterns = adapter.exclude_patterns();
    assert!(
        patterns.contains(&".p4config".to_string()),
        "perforce plugin must exclude .p4config"
    );
}

#[test]
fn perforce_plugin_save_restore_state_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ExternalVcsAdapter::new(&mock_manifest(), dir.path(), "0.13.17-alpha").unwrap();

    // save_state should succeed.
    let state = adapter.save_state().expect("save_state should succeed");
    assert!(state.is_some(), "expected Some(SavedVcsState)");

    // restore_state should accept the state.
    adapter
        .restore_state(state)
        .expect("restore_state should succeed");
}

#[test]
fn perforce_plugin_protected_targets() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ExternalVcsAdapter::new(&mock_manifest(), dir.path(), "0.13.17-alpha").unwrap();
    // Mock returns an empty list — verify this doesn't panic.
    let targets = adapter.protected_submit_targets();
    let _ = targets; // just verify no error
}

#[test]
fn perforce_plugin_verify_not_on_protected_target() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ExternalVcsAdapter::new(&mock_manifest(), dir.path(), "0.13.17-alpha").unwrap();
    adapter
        .verify_not_on_protected_target()
        .expect("verify_target should succeed for perforce mock");
}
