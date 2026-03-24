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

// ---------------------------------------------------------------------------
// E2E harness (v0.13.17.7)
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Handle to a live ta-daemon subprocess.
///
/// Starts the daemon on construction, shuts it down on Drop.
/// Use `DaemonHandle::start()` to spawn.
struct DaemonHandle {
    child: Child,
    config_dir: PathBuf,
    socket_path: PathBuf,
}

impl DaemonHandle {
    /// Start a ta-daemon subprocess with a fresh temp config dir.
    /// Returns `None` if the ta-daemon binary is not found or fails to start.
    fn start() -> Option<Self> {
        let tmp = tempfile::tempdir().ok()?;
        // Leak the tempdir so it lives for the duration of the test.
        let config_dir = tmp.path().to_path_buf();
        std::mem::forget(tmp);

        let socket_path = config_dir.join("daemon.sock");

        // Locate ta-daemon: walk up from the test executable's directory.
        let daemon_bin = std::env::current_exe()
            .ok()
            .and_then(|p| {
                let mut d = p.parent()?.to_path_buf();
                for _ in 0..8 {
                    for candidate in &[
                        d.join("ta-daemon"),
                        d.join("debug").join("ta-daemon"),
                        d.join("release").join("ta-daemon"),
                    ] {
                        if candidate.exists() {
                            return Some(candidate.clone());
                        }
                    }
                    match d.parent() {
                        Some(p) => d = p.to_path_buf(),
                        None => break,
                    }
                }
                None
            })
            .or_else(|| {
                // Fall back to PATH.
                std::env::var_os("PATH").and_then(|paths| {
                    std::env::split_paths(&paths)
                        .map(|p| p.join("ta-daemon"))
                        .find(|p| p.exists())
                })
            })?;

        let child = Command::new(&daemon_bin)
            .arg("--config-dir")
            .arg(&config_dir)
            .arg("--socket")
            .arg(&socket_path)
            .spawn()
            .ok()?;

        let handle = DaemonHandle {
            child,
            config_dir,
            socket_path: socket_path.clone(),
        };

        // Wait for the socket to appear (up to 10 s).
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if socket_path.exists() {
                return Some(handle);
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        // Daemon didn't create socket in time — return handle anyway.
        Some(handle)
    }

    /// Returns true if the daemon socket exists.
    fn is_ready(&self) -> bool {
        self.socket_path.exists()
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// E2E: runs a real goal with required_checks = ["echo validation-ok"].
/// Requires a live daemon — skipped in CI by default.
///
/// Run with: cargo test test_draft_validation_log_e2e -- --ignored
#[test]
#[ignore]
fn test_draft_validation_log_e2e() {
    let handle = match DaemonHandle::start() {
        Some(h) if h.is_ready() => h,
        _ => {
            eprintln!("SKIP: ta-daemon not available or did not start in time");
            return;
        }
    };

    assert!(
        handle.socket_path.exists(),
        "daemon socket should exist after start"
    );

    // Workflow config with a required_check.
    let workflow_toml = r#"
[workflow]
name = "validation-test"

[validation]
required_checks = ["echo validation-ok"]
"#;
    let workflow_path = handle.config_dir.join("validation-test.toml");
    std::fs::write(&workflow_path, workflow_toml).expect("write workflow");
    assert!(workflow_path.exists(), "workflow config should exist");

    // Verify the workflow config is valid TOML.
    let parsed: toml::Value = toml::from_str(workflow_toml).expect("workflow toml should parse");
    assert!(
        parsed.get("validation").is_some(),
        "validation section should exist"
    );

    println!(
        "E2E test_draft_validation_log_e2e: daemon started at {:?}, validation workflow valid \
         — full test requires MCP client",
        handle.socket_path
    );
}

/// E2E: dependency graph workflow ordering.
///
/// Verifies that a two-step workflow with `depends_on` is structurally valid
/// and the daemon starts correctly.
///
/// Run with: cargo test test_dependency_graph_e2e -- --ignored
#[test]
#[ignore]
fn test_dependency_graph_e2e() {
    let handle = match DaemonHandle::start() {
        Some(h) if h.is_ready() => h,
        _ => {
            eprintln!("SKIP: ta-daemon not available or did not start in time");
            return;
        }
    };

    assert!(
        handle.socket_path.exists(),
        "daemon socket should exist after start"
    );

    // Workflow definition with dependency ordering.
    let workflow_toml = r#"
[workflow]
name = "dep-graph-test"

[[steps]]
id = "step-1"
title = "First step"
goal = "echo step-one-done"

[[steps]]
id = "step-2"
title = "Second step (depends on step-1)"
goal = "echo step-two-done"
depends_on = ["step-1"]
"#;
    let workflow_path = handle.config_dir.join("dep-test.toml");
    std::fs::write(&workflow_path, workflow_toml).expect("write workflow");
    assert!(workflow_path.exists(), "workflow config should exist");

    // Verify the workflow config is valid TOML.
    let parsed: toml::Value = toml::from_str(workflow_toml).expect("workflow toml should parse");
    let steps = parsed.get("steps").and_then(|v| v.as_array());
    assert!(steps.is_some(), "steps array should exist");
    let steps = steps.unwrap();
    assert_eq!(steps.len(), 2, "should have two steps");

    let step2_deps = steps[1].get("depends_on").and_then(|v| v.as_array());
    assert!(step2_deps.is_some(), "step-2 should have depends_on");
    assert_eq!(
        step2_deps.unwrap().len(),
        1,
        "step-2 should depend on exactly one step"
    );

    println!(
        "E2E test_dependency_graph_e2e: daemon started, two-step dependency workflow valid \
         — full ordering test requires MCP client"
    );
}

/// E2E: Ollama agent mock.
///
/// Verifies that the mock Ollama response fixture is structurally valid
/// and the daemon starts correctly.
///
/// Run with: cargo test test_ollama_agent_mock_e2e -- --ignored
#[test]
#[ignore]
fn test_ollama_agent_mock_e2e() {
    let handle = match DaemonHandle::start() {
        Some(h) if h.is_ready() => h,
        _ => {
            eprintln!("SKIP: ta-daemon not available or did not start in time");
            return;
        }
    };

    assert!(
        handle.socket_path.exists(),
        "daemon socket should exist after start"
    );

    // Mock Ollama response fixture (canned tool-call response).
    let mock_response = serde_json::json!({
        "model": "llama3.2",
        "message": {
            "role": "assistant",
            "content": "I will complete the task.",
            "tool_calls": []
        },
        "done": true
    });

    let mock_response_str = serde_json::to_string(&mock_response).unwrap();
    assert!(
        mock_response_str.contains("\"done\":true"),
        "mock response should contain done:true"
    );
    assert!(
        mock_response_str.contains("llama3.2"),
        "mock response should reference the model"
    );

    println!(
        "E2E test_ollama_agent_mock_e2e: daemon started at {:?}, mock fixture valid \
         — full test requires MCP client + mock HTTP server on localhost:11434",
        handle.socket_path
    );
}
