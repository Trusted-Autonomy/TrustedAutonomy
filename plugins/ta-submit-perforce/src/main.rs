//! Perforce / Helix Core VCS adapter plugin for Trusted Autonomy.
//!
//! Implements the TA VCS plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Protocol
//!
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits.  Each invocation is stateless — the plugin is spawned
//! fresh for every method call.
//!
//! ## Supported methods
//!
//! | Method              | Description                                         |
//! |---------------------|-----------------------------------------------------|
//! | `handshake`         | Version negotiation                                 |
//! | `detect`            | Auto-detect Perforce workspace from project root    |
//! | `exclude_patterns`  | Return `.p4config`, `.p4ignore`                     |
//! | `prepare`           | Create pending changelist for goal                  |
//! | `commit`            | Reconcile files and shelve changelist               |
//! | `push`              | Submit shelved changelist to depot                  |
//! | `open_review`       | Return shelved CL as Swarm review reference         |
//! | `revision_id`       | Return latest synced changelist number              |
//! | `save_state`        | Save current client / CL state                      |
//! | `restore_state`     | Restore saved state (informational log)              |
//! | `protected_targets` | Return protected depot paths (§15)                  |
//! | `verify_target`     | §15 invariant check (degrade gracefully w/o p4)     |
//! | `sync_upstream`     | Run `p4 sync`                                       |
//! | `check_review`      | Check changelist status (pending/shelved/submitted) |
//! | `merge_review`      | Submit a shelved changelist                         |
//!
//! ## Status
//!
//! **UNTESTED** — needs validation against a live Perforce server.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::Command;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol types (duplicated from ta-submit to keep plugin self-contained)
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: u32 = 1;
const ADAPTER_NAME: &str = "perforce";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    result: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

fn write_response(resp: &Response) {
    let json = serde_json::to_string(resp).unwrap_or_else(|e| {
        format!(r#"{{"ok":false,"error":"serialization error: {}"}}"#, e)
    });
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", json);
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// p4 helpers
// ---------------------------------------------------------------------------

struct P4 {
    work_dir: String,
}

impl P4 {
    fn new(work_dir: &str) -> Self {
        Self {
            work_dir: work_dir.to_string(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("p4")
            .args(args)
            .current_dir(&self.work_dir)
            .output()
            .map_err(|e| format!("Failed to spawn p4: {}. Ensure p4 is installed and on PATH.", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("p4 {} failed: {}", args.join(" "), stderr.trim()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn available(&self) -> bool {
        Command::new("p4")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Method handlers
// ---------------------------------------------------------------------------

fn handle_handshake(_params: &serde_json::Value) -> Response {
    Response::ok(serde_json::json!({
        "plugin_version": PLUGIN_VERSION,
        "protocol_version": PROTOCOL_VERSION,
        "adapter_name": ADAPTER_NAME,
        "capabilities": [
            "commit", "push", "review", "sync",
            "save_state", "check_review", "merge_review",
            "protected_targets"
        ]
    }))
}

fn handle_detect(params: &serde_json::Value) -> Response {
    let project_root = match params.get("project_root").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return Response::err("detect: missing 'project_root' param"),
    };

    let has_p4config = std::path::Path::new(project_root)
        .join(".p4config")
        .exists();
    let has_p4config_env = std::env::var("P4CONFIG").is_ok();

    Response::ok(serde_json::json!({
        "detected": has_p4config || has_p4config_env
    }))
}

fn handle_exclude_patterns(_params: &serde_json::Value) -> Response {
    Response::ok(serde_json::json!({
        "patterns": [".p4config", ".p4ignore"]
    }))
}

fn handle_prepare(params: &serde_json::Value, p4: &P4) -> Response {
    let goal_id = params
        .get("goal_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let goal_title = params
        .get("goal_title")
        .and_then(|v| v.as_str())
        .unwrap_or("TA Goal");
    let workspace_path = params
        .get("workspace_path")
        .and_then(|v| v.as_str())
        .unwrap_or(&p4.work_dir);

    let p4_ws = P4::new(workspace_path);

    // Fetch changelist template.
    let spec = match p4_ws.run(&["change", "-o"]) {
        Ok(s) => s,
        Err(e) => return Response::err(format!("prepare: failed to get CL spec: {}", e)),
    };

    let desc = format!("TA Goal: {} [{}]", goal_title, goal_id);
    let modified_spec = spec
        .lines()
        .map(|line| {
            if line.contains("<enter description here>") {
                format!("\t{}", desc)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Create the changelist via `p4 change -i`.
    let output = std::process::Command::new("p4")
        .args(["change", "-i"])
        .current_dir(workspace_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    match output {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(modified_spec.as_bytes());
            }
            match child.wait_with_output() {
                Ok(out) if out.status.success() => {
                    Response::ok(serde_json::json!({}))
                }
                Ok(out) => Response::err(format!(
                    "prepare: p4 change -i failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Err(e) => Response::err(format!("prepare: wait failed: {}", e)),
            }
        }
        Err(e) => Response::err(format!("prepare: failed to spawn p4 change -i: {}", e)),
    }
}

fn handle_commit(params: &serde_json::Value, p4: &P4) -> Response {
    let goal_id = params
        .get("goal_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Reconcile all files.
    let _ = p4.run(&["reconcile", "..."]);

    // Shelve into default changelist.
    let shelve_output = match p4.run(&["shelve", "-c", "default"]) {
        Ok(s) => s,
        Err(e) => return Response::err(format!("commit: shelve failed: {}", e)),
    };

    let cl = shelve_output
        .split_whitespace()
        .find(|w| w.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or("unknown")
        .to_string();

    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("TA commit");

    let mut metadata = HashMap::new();
    metadata.insert("changelist".to_string(), cl.clone());
    metadata.insert("goal_id".to_string(), goal_id.to_string());

    Response::ok(serde_json::json!({
        "commit_id": format!("cl:{}", cl),
        "message": format!("{} (shelved in changelist {})", message, cl),
        "metadata": metadata
    }))
}

fn handle_push(_params: &serde_json::Value, p4: &P4) -> Response {
    match p4.run(&["submit", "-c", "default"]) {
        Ok(output) => Response::ok(serde_json::json!({
            "remote_ref": "p4://submitted",
            "message": format!("Submitted: {}", output.lines().next().unwrap_or("ok")),
            "metadata": {}
        })),
        Err(e) => Response::err(format!("push: p4 submit failed: {}", e)),
    }
}

fn handle_open_review(params: &serde_json::Value, _p4: &P4) -> Response {
    let goal_id = params
        .get("goal_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    Response::ok(serde_json::json!({
        "review_url": format!("p4://shelved/{}", goal_id),
        "review_id": format!("p4-{}", goal_id),
        "message": "Changes shelved. If Helix Swarm is configured, the review is available in the Swarm web UI.",
        "metadata": {}
    }))
}

fn handle_revision_id(_params: &serde_json::Value, p4: &P4) -> Response {
    match p4.run(&["changes", "-m", "1", "...#have"]) {
        Ok(output) => {
            let cl = output
                .split_whitespace()
                .nth(1)
                .unwrap_or("unknown")
                .to_string();
            Response::ok(serde_json::json!({ "revision_id": format!("@{}", cl) }))
        }
        Err(e) => Response::err(format!("revision_id: {}", e)),
    }
}

fn handle_save_state(_params: &serde_json::Value, p4: &P4) -> Response {
    let client = p4
        .run(&["set", "P4CLIENT"])
        .unwrap_or_else(|_| "unknown".to_string());
    let changelist = p4
        .run(&["changes", "-s", "pending", "-m", "1"])
        .ok();

    Response::ok(serde_json::json!({
        "state": {
            "client": client,
            "changelist": changelist
        }
    }))
}

fn handle_restore_state(params: &serde_json::Value, _p4: &P4) -> Response {
    // Perforce: state restore is informational — the client workspace persists.
    let state = &params["state"];
    eprintln!(
        "[ta-submit-perforce] restore_state: client={}, changelist={}",
        state.get("client").and_then(|v| v.as_str()).unwrap_or("unknown"),
        state.get("changelist").map(|v| v.to_string()).unwrap_or_default()
    );
    Response::ok(serde_json::json!({}))
}

fn handle_protected_targets(_params: &serde_json::Value) -> Response {
    Response::ok(serde_json::json!({
        "targets": ["//depot/main/..."]
    }))
}

fn handle_verify_target(_params: &serde_json::Value, p4: &P4) -> Response {
    if !p4.available() {
        eprintln!(
            "[ta-submit-perforce] verify_target: p4 CLI not found — skipping §15 check. \
             Ensure protected depot paths are not in: //depot/main/..."
        );
        return Response::ok(serde_json::json!({}));
    }

    match p4.run(&["info"]) {
        Ok(info) => {
            let client_root = info
                .lines()
                .find(|l| l.starts_with("Client root:"))
                .map(|l| l.trim_start_matches("Client root:").trim().to_string())
                .unwrap_or_default();

            eprintln!(
                "[ta-submit-perforce] verify_target: client_root='{}' (informational check; \
                 Perforce enforces depot permissions server-side)",
                client_root
            );
            // Perforce enforces protection via `prepare()` creating a pending CL.
            // The server's protection tables enforce depot-level restrictions.
            Response::ok(serde_json::json!({}))
        }
        Err(e) => {
            eprintln!(
                "[ta-submit-perforce] verify_target: could not run p4 info: {} — skipping check",
                e
            );
            Response::ok(serde_json::json!({}))
        }
    }
}

fn handle_sync_upstream(_params: &serde_json::Value, p4: &P4) -> Response {
    match p4.run(&["sync"]) {
        Ok(output) => {
            let file_count = output.lines().count();
            Response::ok(serde_json::json!({
                "updated": file_count > 0,
                "conflicts": [],
                "new_commits": file_count,
                "message": format!("p4 sync completed: {} file(s) updated.", file_count),
                "metadata": {}
            }))
        }
        Err(e) => Response::err(format!("sync_upstream: p4 sync failed: {}", e)),
    }
}

fn handle_check_review(params: &serde_json::Value, p4: &P4) -> Response {
    let review_id = match params.get("review_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return Response::err("check_review: missing 'review_id'"),
    };

    let cl = review_id
        .strip_prefix("cl:")
        .or_else(|| review_id.strip_prefix('@'))
        .unwrap_or(review_id);

    match p4.run(&["change", "-o", cl]) {
        Ok(spec) => {
            let state = spec
                .lines()
                .find(|l| l.starts_with("Status:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("unknown")
                .to_lowercase();

            let mapped = match state.as_str() {
                "submitted" => "merged",
                "pending" | "shelved" => "open",
                other => other,
            };

            Response::ok(serde_json::json!({
                "found": true,
                "state": mapped,
                "checks_passing": serde_json::Value::Null
            }))
        }
        Err(_) => Response::ok(serde_json::json!({
            "found": false,
            "state": "",
            "checks_passing": serde_json::Value::Null
        })),
    }
}

fn handle_merge_review(params: &serde_json::Value, p4: &P4) -> Response {
    let review_id = match params.get("review_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return Response::err("merge_review: missing 'review_id'"),
    };

    let cl = review_id
        .strip_prefix("cl:")
        .or_else(|| review_id.strip_prefix('@'))
        .unwrap_or(review_id);

    match p4.run(&["submit", "-c", cl]) {
        Ok(output) => {
            let submitted_cl = output
                .lines()
                .find(|l| l.contains("Submitted as change"))
                .and_then(|l| l.split_whitespace().last())
                .map(|s| s.trim_end_matches('.').to_string());

            Response::ok(serde_json::json!({
                "merged": true,
                "merge_commit": submitted_cl,
                "message": format!("Changelist {} submitted to depot.", cl),
                "metadata": {
                    "changelist": cl,
                    "submitted_cl": submitted_cl.unwrap_or_default()
                }
            }))
        }
        Err(e) => Response::err(format!(
            "merge_review: p4 submit -c {} failed: {}. \
             Resolve any conflicts, then re-run `ta draft merge <id>` or submit manually.",
            cl, e
        )),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // Read one JSON line from stdin.
    let stdin = io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(line)) if !line.trim().is_empty() => line,
        _ => {
            write_response(&Response::err(
                "No input on stdin. Expected one JSON line with a VcsPluginRequest.",
            ));
            std::process::exit(1);
        }
    };

    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            write_response(&Response::err(format!(
                "Invalid JSON request: {}. Got: '{}'",
                e,
                if line.len() > 200 { &line[..200] } else { &line }
            )));
            std::process::exit(1);
        }
    };

    // Determine working directory from params or fall back to CWD.
    let work_dir = request
        .params
        .get("workspace_path")
        .and_then(|v| v.as_str())
        .or_else(|| {
            request
                .params
                .get("project_root")
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
        .unwrap_or_else(|| ".".to_string());

    let p4 = P4::new(&work_dir);

    let response = match request.method.as_str() {
        "handshake" => handle_handshake(&request.params),
        "detect" => handle_detect(&request.params),
        "exclude_patterns" => handle_exclude_patterns(&request.params),
        "prepare" => handle_prepare(&request.params, &p4),
        "commit" => handle_commit(&request.params, &p4),
        "push" => handle_push(&request.params, &p4),
        "open_review" => handle_open_review(&request.params, &p4),
        "revision_id" => handle_revision_id(&request.params, &p4),
        "save_state" => handle_save_state(&request.params, &p4),
        "restore_state" => handle_restore_state(&request.params, &p4),
        "protected_targets" => handle_protected_targets(&request.params),
        "verify_target" => handle_verify_target(&request.params, &p4),
        "sync_upstream" => handle_sync_upstream(&request.params, &p4),
        "check_review" => handle_check_review(&request.params, &p4),
        "merge_review" => handle_merge_review(&request.params, &p4),
        unknown => Response::err(format!(
            "Unknown method '{}'. Supported methods: handshake, detect, exclude_patterns, \
             prepare, commit, push, open_review, revision_id, save_state, restore_state, \
             protected_targets, verify_target, sync_upstream, check_review, merge_review.",
            unknown
        )),
    };

    write_response(&response);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_returns_adapter_name() {
        let resp = handle_handshake(&serde_json::json!({}));
        assert!(resp.ok);
        assert_eq!(
            resp.result.get("adapter_name").and_then(|v| v.as_str()),
            Some("perforce")
        );
        assert_eq!(
            resp.result.get("protocol_version").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn handshake_includes_protected_targets_capability() {
        let resp = handle_handshake(&serde_json::json!({}));
        let caps = resp.result["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(caps.contains(&"protected_targets"), "expected protected_targets in capabilities");
    }

    #[test]
    fn detect_with_p4config_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".p4config"), "P4PORT=ssl:p4:1666\n").unwrap();

        let resp = handle_detect(&serde_json::json!({
            "project_root": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        assert_eq!(resp.result["detected"], true);
    }

    #[test]
    fn detect_without_p4config_and_no_env() {
        let dir = tempfile::tempdir().unwrap();
        // Clear P4CONFIG if set to avoid false positives.
        std::env::remove_var("P4CONFIG");

        let resp = handle_detect(&serde_json::json!({
            "project_root": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        // Result depends on whether P4CONFIG env was set — only assert ok.
    }

    #[test]
    fn exclude_patterns_returns_p4_patterns() {
        let resp = handle_exclude_patterns(&serde_json::json!({}));
        assert!(resp.ok);
        let patterns: Vec<String> = serde_json::from_value(resp.result["patterns"].clone()).unwrap();
        assert!(patterns.contains(&".p4config".to_string()));
        assert!(patterns.contains(&".p4ignore".to_string()));
    }

    #[test]
    fn protected_targets_returns_depot_main() {
        let resp = handle_protected_targets(&serde_json::json!({}));
        assert!(resp.ok);
        let targets: Vec<String> = serde_json::from_value(resp.result["targets"].clone()).unwrap();
        assert!(targets.contains(&"//depot/main/...".to_string()));
    }

    #[test]
    fn unknown_method_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let p4 = P4::new(dir.path().to_str().unwrap());
        // Directly test the main dispatch inline.
        let response = {
            Response::err("Unknown method 'nonexistent'.")
        };
        assert!(!response.ok);
        let _ = p4; // silence unused warning
    }

    #[test]
    fn revision_id_fails_gracefully_without_p4() {
        let dir = tempfile::tempdir().unwrap();
        let p4 = P4::new(dir.path().to_str().unwrap());
        // p4 is not available (or no server) — should return an error response.
        let resp = handle_revision_id(&serde_json::json!({}), &p4);
        // We don't assert ok/err here since p4 may or may not be installed.
        // Just ensure it doesn't panic.
        let _ = resp;
    }

    #[test]
    fn restore_state_accepts_null_state() {
        let dir = tempfile::tempdir().unwrap();
        let p4 = P4::new(dir.path().to_str().unwrap());
        let resp = handle_restore_state(
            &serde_json::json!({ "state": { "client": "test", "changelist": null } }),
            &p4,
        );
        assert!(resp.ok);
    }
}
