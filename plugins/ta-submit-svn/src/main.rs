//! Subversion (SVN) VCS adapter plugin for Trusted Autonomy.
//!
//! Implements the TA VCS plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Key SVN characteristics
//!
//! - SVN commit is immediately remote — there are no local-only commits.
//! - `push()` is a no-op since `commit()` already sends to the server.
//! - SVN has no built-in code review workflow.
//! - §15 guard blocks commits to `/trunk` until SVN branching support is added.
//!
//! ## Status
//!
//! **UNTESTED** — needs validation against a live SVN server.

use std::io::{self, BufRead, Write};
use std::process::Command;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: u32 = 1;
const ADAPTER_NAME: &str = "svn";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

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
// SVN helpers
// ---------------------------------------------------------------------------

struct Svn {
    work_dir: String,
}

impl Svn {
    fn new(work_dir: &str) -> Self {
        Self {
            work_dir: work_dir.to_string(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("svn")
            .args(args)
            .current_dir(&self.work_dir)
            .output()
            .map_err(|e| {
                format!(
                    "Failed to spawn svn: {}. Ensure svn is installed and on PATH.",
                    e
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "svn {} failed: {}",
                args.join(" "),
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
            "commit", "push", "sync",
            "save_state",
            "protected_targets"
        ]
    }))
}

fn handle_detect(params: &serde_json::Value) -> Response {
    let project_root = match params.get("project_root").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return Response::err("detect: missing 'project_root' param"),
    };

    let detected = std::path::Path::new(project_root)
        .join(".svn")
        .exists();

    Response::ok(serde_json::json!({ "detected": detected }))
}

fn handle_exclude_patterns(_params: &serde_json::Value) -> Response {
    Response::ok(serde_json::json!({
        "patterns": [".svn/"]
    }))
}

fn handle_prepare(_params: &serde_json::Value, _svn: &Svn) -> Response {
    // SVN doesn't use branches the same way as Git.
    // No-op: the working copy is already pointing at the correct URL.
    Response::ok(serde_json::json!({}))
}

fn handle_commit(params: &serde_json::Value, svn: &Svn) -> Response {
    let goal_id = params
        .get("goal_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("TA commit");

    // Add any new (unversioned) files.
    let _ = svn.run(&["add", "--force", "."]);

    let commit_msg = format!("{}\n\nGoal-ID: {}", message, goal_id);
    match svn.run(&["commit", "-m", &commit_msg]) {
        Ok(output) => {
            let rev = output
                .lines()
                .find(|l| l.contains("Committed revision"))
                .and_then(|l| {
                    l.split_whitespace()
                        .find(|w| w.chars().any(|c| c.is_ascii_digit()))
                        .map(|w| w.trim_end_matches('.').to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());

            Response::ok(serde_json::json!({
                "commit_id": format!("r{}", rev),
                "message": format!("Committed revision {}", rev),
                "metadata": { "revision": rev }
            }))
        }
        Err(e) => Response::err(format!("commit: svn commit failed: {}", e)),
    }
}

fn handle_push(_params: &serde_json::Value, _svn: &Svn) -> Response {
    // SVN commit is already remote — no separate push step.
    Response::ok(serde_json::json!({
        "remote_ref": "svn://committed",
        "message": "SVN commit is already remote — no push needed",
        "metadata": {}
    }))
}

fn handle_open_review(_params: &serde_json::Value, _svn: &Svn) -> Response {
    Response::ok(serde_json::json!({
        "review_url": "svn://no-review",
        "review_id": "none",
        "message": "SVN has no built-in review workflow. Consider using a code review tool \
                    like Crucible or ReviewBoard.",
        "metadata": {}
    }))
}

fn handle_revision_id(_params: &serde_json::Value, svn: &Svn) -> Response {
    match svn.run(&["info"]) {
        Ok(info) => {
            let rev = info
                .lines()
                .find(|l| l.starts_with("Revision:"))
                .and_then(|l| l.split(':').nth(1))
                .map(|r| r.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            Response::ok(serde_json::json!({ "revision_id": format!("r{}", rev) }))
        }
        Err(e) => Response::err(format!("revision_id: {}", e)),
    }
}

fn handle_save_state(_params: &serde_json::Value, svn: &Svn) -> Response {
    let url = svn.run(&["info", "--show-item", "url"]).ok();
    let rev = svn.run(&["info", "--show-item", "revision"]).ok();

    Response::ok(serde_json::json!({
        "state": {
            "url": url,
            "revision": rev
        }
    }))
}

fn handle_restore_state(params: &serde_json::Value, _svn: &Svn) -> Response {
    let state = &params["state"];
    eprintln!(
        "[ta-submit-svn] restore_state: url={}, revision={}",
        state
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
        state
            .get("revision")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
    );
    Response::ok(serde_json::json!({}))
}

fn handle_protected_targets(_params: &serde_json::Value) -> Response {
    // /trunk is the conventional SVN integration line.
    Response::ok(serde_json::json!({
        "targets": ["/trunk"]
    }))
}

fn handle_verify_target(_params: &serde_json::Value, svn: &Svn) -> Response {
    // Check working copy URL — block if pointed at /trunk.
    match svn.run(&["info", "--show-item", "url"]) {
        Ok(url) => {
            if url.contains("/trunk") {
                Response::err(format!(
                    "§15 violation: working copy URL '{}' contains protected path '/trunk'. \
                     SVN branching is not yet supported in this plugin — create a branch/copy \
                     before applying changes to a protected path.",
                    url
                ))
            } else {
                Response::ok(serde_json::json!({}))
            }
        }
        Err(_) => {
            // svn not installed or not an SVN working copy — allow (svn commit
            // would also fail in this case, providing its own error).
            eprintln!(
                "[ta-submit-svn] verify_target: could not run svn info — skipping §15 check"
            );
            Response::ok(serde_json::json!({}))
        }
    }
}

fn handle_sync_upstream(_params: &serde_json::Value, svn: &Svn) -> Response {
    match svn.run(&["update"]) {
        Ok(output) => {
            let conflicts: Vec<String> = output
                .lines()
                .filter(|l| l.starts_with("C ") || l.starts_with("C\t"))
                .map(|l| l[2..].trim().to_string())
                .collect();

            let updated_count = output
                .lines()
                .filter(|l| {
                    l.starts_with("U ") || l.starts_with("A ") || l.starts_with("D ")
                })
                .count();

            Response::ok(serde_json::json!({
                "updated": updated_count > 0 || !conflicts.is_empty(),
                "conflicts": conflicts,
                "new_commits": updated_count,
                "message": format!("svn update completed. {}", output.lines().last().unwrap_or("")),
                "metadata": {}
            }))
        }
        Err(e) => Response::err(format!("sync_upstream: svn update failed: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
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

    let work_dir = request
        .params
        .get("workspace_path")
        .and_then(|v| v.as_str())
        .or_else(|| request.params.get("project_root").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| ".".to_string());

    let svn = Svn::new(&work_dir);

    let response = match request.method.as_str() {
        "handshake" => handle_handshake(&request.params),
        "detect" => handle_detect(&request.params),
        "exclude_patterns" => handle_exclude_patterns(&request.params),
        "prepare" => handle_prepare(&request.params, &svn),
        "commit" => handle_commit(&request.params, &svn),
        "push" => handle_push(&request.params, &svn),
        "open_review" => handle_open_review(&request.params, &svn),
        "revision_id" => handle_revision_id(&request.params, &svn),
        "save_state" => handle_save_state(&request.params, &svn),
        "restore_state" => handle_restore_state(&request.params, &svn),
        "protected_targets" => handle_protected_targets(&request.params),
        "verify_target" => handle_verify_target(&request.params, &svn),
        "sync_upstream" => handle_sync_upstream(&request.params, &svn),
        unknown => Response::err(format!(
            "Unknown method '{}'. Supported methods: handshake, detect, exclude_patterns, \
             prepare, commit, push, open_review, revision_id, save_state, restore_state, \
             protected_targets, verify_target, sync_upstream.",
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
    fn handshake_returns_svn_adapter() {
        let resp = handle_handshake(&serde_json::json!({}));
        assert!(resp.ok);
        assert_eq!(
            resp.result.get("adapter_name").and_then(|v| v.as_str()),
            Some("svn")
        );
        assert_eq!(
            resp.result.get("protocol_version").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn handshake_includes_protected_targets() {
        let resp = handle_handshake(&serde_json::json!({}));
        let caps = resp.result["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(
            caps.contains(&"protected_targets"),
            "expected protected_targets capability"
        );
    }

    #[test]
    fn detect_with_svn_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".svn")).unwrap();

        let resp = handle_detect(&serde_json::json!({
            "project_root": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        assert_eq!(resp.result["detected"], true);
    }

    #[test]
    fn detect_without_svn_dir() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_detect(&serde_json::json!({
            "project_root": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        assert_eq!(resp.result["detected"], false);
    }

    #[test]
    fn exclude_patterns_contains_svn() {
        let resp = handle_exclude_patterns(&serde_json::json!({}));
        assert!(resp.ok);
        let patterns: Vec<String> =
            serde_json::from_value(resp.result["patterns"].clone()).unwrap();
        assert!(patterns.contains(&".svn/".to_string()));
    }

    #[test]
    fn push_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let svn = Svn::new(dir.path().to_str().unwrap());
        let resp = handle_push(&serde_json::json!({}), &svn);
        assert!(resp.ok);
        assert_eq!(
            resp.result["remote_ref"].as_str(),
            Some("svn://committed")
        );
    }

    #[test]
    fn protected_targets_contains_trunk() {
        let resp = handle_protected_targets(&serde_json::json!({}));
        assert!(resp.ok);
        let targets: Vec<String> =
            serde_json::from_value(resp.result["targets"].clone()).unwrap();
        assert!(targets.contains(&"/trunk".to_string()));
    }

    #[test]
    fn verify_target_degrades_without_svn_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        let svn = Svn::new(dir.path().to_str().unwrap());
        // Not an SVN working copy — svn info fails → should return ok
        let resp = handle_verify_target(&serde_json::json!({}), &svn);
        assert!(resp.ok);
    }

    #[test]
    fn prepare_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let svn = Svn::new(dir.path().to_str().unwrap());
        let resp = handle_prepare(&serde_json::json!({}), &svn);
        assert!(resp.ok);
    }
}
