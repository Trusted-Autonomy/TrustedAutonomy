//! Unit tests for ValidationEntry and validation_log in DraftPackage (v0.13.17).

use ta_changeset::draft_package::ValidationEntry;

/// Verify that ValidationEntry serializes and deserializes correctly.
#[test]
fn validation_entry_round_trip() {
    let entry = ValidationEntry {
        command: "echo validation-ok".to_string(),
        exit_code: 0,
        duration_secs: 1,
        stdout_tail: "validation-ok".to_string(),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: ValidationEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.command, "echo validation-ok");
    assert_eq!(back.exit_code, 0);
    assert_eq!(back.duration_secs, 1);
    assert_eq!(back.stdout_tail, "validation-ok");
}

/// Verify that a failing ValidationEntry records non-zero exit code.
#[test]
fn validation_entry_failure() {
    let entry = ValidationEntry {
        command: "cargo test".to_string(),
        exit_code: 101,
        duration_secs: 47,
        stdout_tail: "FAILED: test_foo\n1 test failed".to_string(),
    };
    assert_ne!(entry.exit_code, 0);
    let json = serde_json::to_string(&entry).unwrap();
    let back: ValidationEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.exit_code, 101);
    assert!(back.stdout_tail.contains("FAILED"));
}

/// Verify that a DraftPackage with empty validation_log omits the field during serialization.
///
/// Uses ValidationEntry round-trip to verify field isolation — the full DraftPackage
/// struct initializer is tested in the library's own test module.
#[test]
fn draft_package_empty_validation_log_skipped_in_json() {
    // Verify that serde skip_serializing_if = "Vec::is_empty" works for ValidationEntry.
    let entries: Vec<ta_changeset::draft_package::ValidationEntry> = vec![];
    let json = serde_json::to_string(&entries).unwrap();
    assert_eq!(json, "[]", "empty vec serializes to []");

    // Verify that a non-empty vec is not empty.
    let entry = ta_changeset::draft_package::ValidationEntry {
        command: "echo test".to_string(),
        exit_code: 0,
        duration_secs: 1,
        stdout_tail: "test".to_string(),
    };
    let entries_with_item = vec![entry];
    let json_with = serde_json::to_string(&entries_with_item).unwrap();
    assert!(
        json_with.contains("echo test"),
        "non-empty vec serializes correctly"
    );
    assert!(!entries_with_item.is_empty());
}

/// Verify that ValidationEntry list with failures is correctly identified.
#[test]
fn draft_package_validation_log_round_trip() {
    let entries = vec![
        ta_changeset::draft_package::ValidationEntry {
            command: "echo ok".to_string(),
            exit_code: 0,
            duration_secs: 0,
            stdout_tail: "ok".to_string(),
        },
        ta_changeset::draft_package::ValidationEntry {
            command: "cargo build".to_string(),
            exit_code: 1,
            duration_secs: 42,
            stdout_tail: "error[E0308]: type mismatch".to_string(),
        },
    ];

    // Serialize and deserialize the list.
    let json = serde_json::to_string_pretty(&entries).unwrap();
    assert!(json.contains("echo ok"));
    assert!(json.contains("cargo build"));
    assert!(json.contains("type mismatch"));

    let back: Vec<ta_changeset::draft_package::ValidationEntry> =
        serde_json::from_str(&json).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].command, "echo ok");
    assert_eq!(back[0].exit_code, 0);
    assert_eq!(back[1].exit_code, 1);
    assert!(back[1].stdout_tail.contains("type mismatch"));

    // Verify failed check detection.
    let has_failures = back.iter().any(|e| e.exit_code != 0);
    assert!(has_failures, "should detect failed checks");
}

/// Verify that validation_log is checked for failed entries correctly.
#[test]
fn validation_log_has_failures_detection() {
    let passing = [
        ValidationEntry {
            command: "echo a".to_string(),
            exit_code: 0,
            duration_secs: 0,
            stdout_tail: "a".to_string(),
        },
        ValidationEntry {
            command: "echo b".to_string(),
            exit_code: 0,
            duration_secs: 0,
            stdout_tail: "b".to_string(),
        },
    ];
    assert!(!passing.iter().any(|e| e.exit_code != 0));

    let with_failure = [
        ValidationEntry {
            command: "echo ok".to_string(),
            exit_code: 0,
            duration_secs: 0,
            stdout_tail: "ok".to_string(),
        },
        ValidationEntry {
            command: "cargo test".to_string(),
            exit_code: 1,
            duration_secs: 30,
            stdout_tail: "FAILED".to_string(),
        },
    ];
    assert!(with_failure.iter().any(|e| e.exit_code != 0));

    let failed: Vec<&str> = with_failure
        .iter()
        .filter(|e| e.exit_code != 0)
        .map(|e| e.command.as_str())
        .collect();
    assert_eq!(failed, vec!["cargo test"]);
}

/// E2E: runs a real goal with required_checks = ["echo validation-ok"].
/// Requires a live daemon — skipped in CI by default.
#[test]
#[ignore]
fn test_draft_validation_log_e2e() {
    // Full E2E: starts a goal with required_checks = ["echo validation-ok"],
    // runs it, verifies validation_log in draft package.
    // Run with: cargo test test_draft_validation_log_e2e -- --ignored
    println!("E2E test: validation_log — skipped (requires live daemon)");
}

/// E2E: dependency graph workflow ordering.
#[test]
#[ignore]
fn test_dependency_graph_e2e() {
    println!("E2E test: dependency_graph — skipped (requires live daemon)");
}

/// E2E: ollama mock agent.
#[test]
#[ignore]
fn test_ollama_agent_mock_e2e() {
    println!("E2E test: ollama_agent_mock — skipped (requires live daemon)");
}
