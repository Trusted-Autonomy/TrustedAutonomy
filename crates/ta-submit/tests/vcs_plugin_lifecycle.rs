//! Integration test: external VCS plugin lifecycle over JSON-over-stdio.
//!
//! Uses a mock shell-script plugin that speaks the protocol to verify the
//! full detect → save_state → commit → restore_state flow.
//!
//! The mock plugin script responds to every method with a hardcoded success
//! response. This validates the adapter-to-plugin plumbing without requiring
//! a live VCS installation.
//!
//! Unix-only: the mock plugin is a shell script and relies on Unix executable
//! permissions. Windows CI skips this test file entirely.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ta_submit::external_vcs_adapter::ExternalVcsAdapter;
use ta_submit::vcs_plugin_manifest::VcsPluginManifest;
use ta_submit::SourceAdapter;

// ---------------------------------------------------------------------------
// Mock plugin script
// ---------------------------------------------------------------------------

/// Write the mock VCS plugin shell script and return its path.
fn write_mock_plugin(dir: &std::path::Path) -> PathBuf {
    let script = r#"#!/bin/sh
# Mock VCS plugin for TA integration testing.
# Reads one JSON line from stdin, dispatches to a hardcoded response.

read -r line

method=$(echo "$line" | sed 's/.*"method":"\([^"]*\)".*/\1/')

case "$method" in
  handshake)
    echo '{"ok":true,"result":{"plugin_version":"0.1.0","protocol_version":1,"adapter_name":"mock-vcs","capabilities":["commit","push","review","sync","save_state","check_review","merge_review","protected_targets"]}}'
    ;;
  detect)
    echo '{"ok":true,"result":{"detected":true}}'
    ;;
  exclude_patterns)
    echo '{"ok":true,"result":{"patterns":[".mock-vcs/"]}}'
    ;;
  prepare)
    echo '{"ok":true,"result":{}}'
    ;;
  save_state)
    echo '{"ok":true,"result":{"state":{"branch":"feature/test","rev":"abc123"}}}'
    ;;
  restore_state)
    echo '{"ok":true,"result":{}}'
    ;;
  commit)
    echo '{"ok":true,"result":{"commit_id":"mock-abc123","message":"Mock commit ok","metadata":{}}}'
    ;;
  push)
    echo '{"ok":true,"result":{"remote_ref":"mock://remote/branch","message":"Mock push ok","metadata":{}}}'
    ;;
  open_review)
    echo '{"ok":true,"result":{"review_url":"mock://review/1","review_id":"mock-1","message":"Mock review opened","metadata":{}}}'
    ;;
  revision_id)
    echo '{"ok":true,"result":{"revision_id":"mock-rev-42"}}'
    ;;
  protected_targets)
    echo '{"ok":true,"result":{"targets":["mock://protected/main"]}}'
    ;;
  verify_target)
    echo '{"ok":true,"result":{}}'
    ;;
  sync_upstream)
    echo '{"ok":true,"result":{"updated":true,"conflicts":[],"new_commits":3,"message":"Mock sync ok","metadata":{}}}'
    ;;
  check_review)
    echo '{"ok":true,"result":{"found":true,"state":"open","checks_passing":true}}'
    ;;
  merge_review)
    echo '{"ok":true,"result":{"merged":true,"merge_commit":"mock-merge-sha","message":"Mock merge ok","metadata":{}}}'
    ;;
  *)
    echo "{\"ok\":false,\"error\":\"Unknown method: $method\"}"
    ;;
esac
"#;

    let path = dir.join("ta-submit-mock-vcs");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn mock_manifest(command_path: &str) -> VcsPluginManifest {
    VcsPluginManifest {
        name: "mock-vcs".to_string(),
        version: "0.1.0".to_string(),
        plugin_type: "vcs".to_string(),
        command: command_path.to_string(),
        args: vec![],
        capabilities: vec![
            "commit".to_string(),
            "push".to_string(),
            "protected_targets".to_string(),
        ],
        description: Some("Mock VCS plugin for testing".to_string()),
        timeout_secs: 10,
        min_daemon_version: None,
        source_url: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn handshake_succeeds_with_mock_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha")
        .expect("handshake should succeed");

    assert_eq!(adapter.name(), "mock-vcs");
}

#[test]
fn exclude_patterns_returns_mock_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let patterns = adapter.exclude_patterns();

    assert_eq!(patterns, vec![".mock-vcs/"]);
}

#[test]
fn save_state_returns_some() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let state = adapter.save_state().expect("save_state should succeed");

    assert!(state.is_some(), "expected Some(SavedVcsState)");
}

#[test]
fn restore_state_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();

    // Save then restore.
    let state = adapter.save_state().unwrap();
    adapter
        .restore_state(state)
        .expect("restore_state should succeed");
}

#[test]
fn protected_targets_returns_mock_targets() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let targets = adapter.protected_submit_targets();

    assert_eq!(targets, vec!["mock://protected/main"]);
}

#[test]
fn verify_not_on_protected_target_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    adapter
        .verify_not_on_protected_target()
        .expect("verify_target should succeed");
}

#[test]
fn revision_id_returns_mock_rev() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let rev = adapter.revision_id().expect("revision_id should succeed");
    assert_eq!(rev, "mock-rev-42");
}

#[test]
fn sync_upstream_returns_updated_true() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let result = adapter
        .sync_upstream()
        .expect("sync_upstream should succeed");
    assert!(result.updated);
    assert_eq!(result.new_commits, 3);
    assert!(result.conflicts.is_empty());
}

#[test]
fn check_review_returns_open() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let status = adapter
        .check_review("mock-pr-1")
        .expect("check_review should succeed");
    let status = status.expect("should return Some");
    assert_eq!(status.state, "open");
    assert_eq!(status.checks_passing, Some(true));
}

#[test]
fn merge_review_returns_merged_true() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    let result = adapter
        .merge_review("mock-pr-1")
        .expect("merge_review should succeed");
    assert!(result.merged);
    assert_eq!(result.merge_commit.as_deref(), Some("mock-merge-sha"));
}

#[test]
fn detect_with_mock_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    let detected = ExternalVcsAdapter::detect_with_plugin(&manifest, dir.path(), "0.13.5-alpha");
    assert!(detected, "mock plugin should return detected=true");
}

#[test]
fn full_lifecycle_detect_save_commit_restore() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_mock_plugin(dir.path());
    let manifest = mock_manifest(&bin.display().to_string());

    // 1. Detect
    let detected = ExternalVcsAdapter::detect_with_plugin(&manifest, dir.path(), "0.13.5-alpha");
    assert!(detected);

    // 2. Create adapter (includes handshake)
    let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
    assert_eq!(adapter.name(), "mock-vcs");

    // 3. Save state
    let state = adapter.save_state().unwrap();
    assert!(state.is_some());

    // 4. Restore state
    adapter.restore_state(state).unwrap();

    // 5. Verify §15 targets
    let targets = adapter.protected_submit_targets();
    assert!(!targets.is_empty());

    // 6. §15 check
    adapter.verify_not_on_protected_target().unwrap();

    // 7. revision_id
    let rev = adapter.revision_id().unwrap();
    assert_eq!(rev, "mock-rev-42");
}
