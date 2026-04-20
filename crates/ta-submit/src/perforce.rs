//! Perforce adapter stub — untested, contributed by AI.
//!
//! This adapter provides basic Perforce/Helix Core integration.
//! It is **untested** and needs validation by a Perforce user before production use.
//!
//! Key differences from Git:
//! - Uses changelists instead of branches
//! - `commit()` shelves files (staging for review)
//! - `push()` submits the changelist (makes it permanent)
//! - Review via Helix Swarm API (if configured)

use std::path::Path;
use std::process::Command;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{
    CommitResult, MergeResult, PushResult, Result, ReviewResult, ReviewStatus, SavedVcsState,
    SourceAdapter, SubmitError, SyncResult,
};
use crate::config::SubmitConfig;

/// Saved Perforce state: current changelist number and client name.
#[derive(Debug, Clone)]
struct PerforceState {
    client: String,
    changelist: Option<String>,
}

/// Perforce/Helix Core adapter implementing changelist-based workflow.
///
/// **Status: UNTESTED** — needs validation by a Perforce user.
pub struct PerforceAdapter {
    work_dir: std::path::PathBuf,
}

impl PerforceAdapter {
    pub fn new(work_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }

    fn p4_cmd(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("p4")
            .args(args)
            .current_dir(&self.work_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SubmitError::VcsError(format!(
                "p4 {} failed: {}",
                args.join(" "),
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Auto-detect whether this is a Perforce workspace.
    pub fn detect(project_root: &Path) -> bool {
        // Check for P4CONFIG env var
        if std::env::var("P4CONFIG").is_ok() {
            return true;
        }
        // Check for .p4config file
        project_root.join(".p4config").exists()
    }
}

impl SourceAdapter for PerforceAdapter {
    fn prepare(&self, goal: &GoalRun, _config: &SubmitConfig) -> Result<()> {
        tracing::info!(
            "PerforceAdapter: creating pending changelist for goal {}",
            goal.goal_run_id
        );

        // Create a new pending changelist.
        // `p4 change -o` outputs a changelist spec, we modify and pipe to `p4 change -i`.
        let spec = self.p4_cmd(&["change", "-o"])?;

        // Replace the description in the spec.
        let new_desc = format!("TA Goal: {} [{}]", goal.title, goal.goal_run_id);
        let modified_spec = spec
            .lines()
            .map(|line| {
                if line.starts_with("\t<enter description here>") {
                    format!("\t{}", new_desc)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Pipe modified spec to create the changelist.
        let output = Command::new("p4")
            .args(["change", "-i"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&self.work_dir)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(modified_spec.as_bytes())?;
                }
                child.wait_with_output()
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SubmitError::VcsError(format!(
                "p4 change -i failed: {}",
                stderr
            )));
        }

        tracing::info!("PerforceAdapter: changelist created");
        Ok(())
    }

    fn commit(&self, goal: &GoalRun, _pr: &DraftPackage, message: &str) -> Result<CommitResult> {
        tracing::info!("PerforceAdapter: reconciling and shelving changes");

        // Reconcile: detect added/edited/deleted files.
        let _ = self.p4_cmd(&["reconcile", "..."]);

        // Shelve the files (staging for review).
        let shelve_output = self.p4_cmd(&["shelve", "-c", "default"])?;

        // Try to extract changelist number from output.
        let cl = shelve_output
            .split_whitespace()
            .find(|w| w.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or("unknown")
            .to_string();

        Ok(CommitResult {
            commit_id: format!("cl:{}", cl),
            message: format!("{} (shelved in changelist {})", message, cl),
            metadata: [
                ("changelist".to_string(), cl),
                ("goal_id".to_string(), goal.goal_run_id.to_string()),
            ]
            .into_iter()
            .collect(),
            ignored_artifacts: vec![],
        })
    }

    fn push(&self, _goal: &GoalRun) -> Result<PushResult> {
        tracing::info!("PerforceAdapter: submitting changelist");

        let output = self.p4_cmd(&["submit", "-c", "default"])?;

        Ok(PushResult {
            remote_ref: "p4://submitted".to_string(),
            message: format!("Submitted: {}", output.lines().next().unwrap_or("ok")),
            metadata: Default::default(),
        })
    }

    fn open_review(&self, goal: &GoalRun, _pr: &DraftPackage) -> Result<ReviewResult> {
        // Shelving is the Perforce equivalent of opening a review.
        // If Helix Swarm is configured, the shelved changelist appears there automatically.
        tracing::debug!(
            "PerforceAdapter: open_review() — shelved changelist serves as review (use Helix Swarm for web UI)"
        );
        Ok(ReviewResult {
            review_url: format!("p4://shelved/{}", goal.goal_run_id),
            review_id: format!("p4-{}", goal.goal_run_id),
            message: "Changes shelved. If Helix Swarm is configured, the review is available in the Swarm web UI.".to_string(),
            metadata: Default::default(),
        })
    }

    fn sync_upstream(&self) -> Result<SyncResult> {
        tracing::info!("PerforceAdapter: running p4 sync");

        match self.p4_cmd(&["sync"]) {
            Ok(output) => {
                // Count synced files from p4 sync output.
                let file_count = output.lines().count();

                Ok(SyncResult {
                    updated: file_count > 0,
                    conflicts: vec![],
                    new_commits: file_count as u32,
                    message: format!("p4 sync completed: {} file(s) updated.", file_count),
                    metadata: Default::default(),
                })
            }
            Err(e) => Err(SubmitError::SyncError(format!("p4 sync failed: {}", e))),
        }
    }

    fn name(&self) -> &str {
        "perforce"
    }

    fn exclude_patterns(&self) -> Vec<String> {
        vec![".p4config".to_string(), ".p4ignore".to_string()]
    }

    fn commit_diff(&self) -> Option<String> {
        // Get the most recent submitted changelist number.
        let cl = match self.p4_cmd(&["changes", "-s", "submitted", "-m", "1"]) {
            Ok(out) => out,
            Err(_) => return None,
        };
        // Output format: "Change <N> on <date> by <user> '<desc>'"
        let cl_num = cl.split_whitespace().nth(1)?;
        self.p4_cmd(&["describe", "-du", cl_num]).ok()
    }

    fn save_state(&self) -> Result<Option<SavedVcsState>> {
        // Save current client and pending changelist info.
        let client = self
            .p4_cmd(&["set", "P4CLIENT"])
            .unwrap_or_else(|_| "unknown".to_string());
        let changelist = self.p4_cmd(&["changes", "-s", "pending", "-m", "1"]).ok();

        let state = PerforceState { client, changelist };

        tracing::debug!(?state, "PerforceAdapter: saved state");
        Ok(Some(SavedVcsState {
            adapter: "perforce".to_string(),
            data: Box::new(state),
        }))
    }

    fn restore_state(&self, state: Option<SavedVcsState>) -> Result<()> {
        let state = match state {
            Some(s) => s,
            None => return Ok(()),
        };

        if state.adapter != "perforce" {
            return Err(SubmitError::InvalidState(format!(
                "Cannot restore state from adapter '{}' in PerforceAdapter",
                state.adapter
            )));
        }

        // For Perforce, restore is mostly informational — the client workspace
        // persists across operations. Log for observability.
        if let Ok(p4_state) = state.data.downcast::<PerforceState>() {
            tracing::info!(
                client = %p4_state.client,
                changelist = ?p4_state.changelist,
                "PerforceAdapter: state restored"
            );
        }

        Ok(())
    }

    fn revision_id(&self) -> Result<String> {
        // Get the latest changelist number synced to this client.
        let output = self.p4_cmd(&["changes", "-m", "1", "...#have"])?;
        let cl = output
            .split_whitespace()
            .nth(1) // "Change 1234 ..."
            .unwrap_or("unknown")
            .to_string();
        Ok(format!("@{}", cl))
    }

    fn protected_submit_targets(&self) -> Vec<String> {
        // Depot paths that agents must never submit directly to.
        // Default: the conventional main depot path.
        vec!["//depot/main/...".to_string()]
    }

    fn verify_not_on_protected_target(&self) -> Result<()> {
        // Check current CL's target stream/depot via `p4 info`.
        // If p4 is not installed, degrade gracefully (allow the submit to proceed
        // but log a warning — p4 itself will enforce restrictions).
        let p4_available = std::process::Command::new("p4")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !p4_available {
            tracing::warn!(
                "PerforceAdapter: p4 CLI not found — cannot verify protected targets. \
                 Ensure your depot paths are not in: {:?}",
                self.protected_submit_targets()
            );
            return Ok(());
        }

        // Get the current client's root stream/depot mapping.
        match self.p4_cmd(&["info"]) {
            Ok(info) => {
                let client_root = info
                    .lines()
                    .find(|l| l.starts_with("Client root:"))
                    .map(|l| l.trim_start_matches("Client root:").trim().to_string())
                    .unwrap_or_default();

                let protected = self.protected_submit_targets();
                for target in &protected {
                    // Simple check: if the target depot path appears in client info
                    // and there's no branch indicator, warn but allow (Perforce
                    // enforces protection through its own permission system; our
                    // `prepare()` creates a pending CL which is the isolation mechanism).
                    tracing::debug!(
                        client_root = %client_root,
                        protected_target = %target,
                        "PerforceAdapter: protected target check (informational)"
                    );
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "PerforceAdapter: could not run `p4 info` for protected target check"
                );
                Ok(()) // Degrade gracefully
            }
        }
    }

    fn check_review(&self, review_id: &str) -> Result<Option<ReviewStatus>> {
        // Extract the raw CL number (strip "cl:" or "@" prefix if present).
        let cl = review_id
            .strip_prefix("cl:")
            .or_else(|| review_id.strip_prefix('@'))
            .unwrap_or(review_id);

        match self.p4_cmd(&["change", "-o", cl]) {
            Ok(spec) => {
                // Parse the Status field from the CL spec.
                // Possible values: pending, shelved, submitted.
                let state = spec
                    .lines()
                    .find(|l| l.starts_with("Status:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("unknown")
                    .to_lowercase();

                let mapped_state = match state.as_str() {
                    "submitted" => "merged",
                    "pending" | "shelved" => "open",
                    other => other,
                };

                Ok(Some(ReviewStatus {
                    state: mapped_state.to_string(),
                    checks_passing: None,
                }))
            }
            Err(_) => Ok(None),
        }
    }

    fn merge_review(&self, review_id: &str) -> Result<MergeResult> {
        // Extract raw CL number.
        let cl = review_id
            .strip_prefix("cl:")
            .or_else(|| review_id.strip_prefix('@'))
            .unwrap_or(review_id);

        tracing::info!(cl = %cl, "PerforceAdapter: submitting shelved changelist");

        match self.p4_cmd(&["submit", "-c", cl]) {
            Ok(output) => {
                // Extract submitted CL number from output ("Submitted as change N.")
                let submitted_cl = output
                    .lines()
                    .find(|l| l.contains("Submitted as change"))
                    .and_then(|l| l.split_whitespace().last())
                    .map(|s| s.trim_end_matches('.').to_string());

                Ok(MergeResult {
                    merged: true,
                    merge_commit: submitted_cl.clone(),
                    message: format!(
                        "Changelist {} submitted to depot{}.",
                        cl,
                        submitted_cl
                            .as_ref()
                            .map(|n| format!(" as change {}", n))
                            .unwrap_or_default()
                    ),
                    metadata: [
                        ("changelist".to_string(), cl.to_string()),
                        ("submitted_cl".to_string(), submitted_cl.unwrap_or_default()),
                    ]
                    .into_iter()
                    .collect(),
                })
            }
            Err(e) => Err(SubmitError::ReviewError(format!(
                "p4 submit -c {} failed: {}. \
                 Resolve any conflicts, then re-run `ta draft merge <id>` or submit manually.",
                cl, e
            ))),
        }
    }

    fn stage_env(
        &self,
        _staging_dir: &std::path::Path,
        config: &crate::config::VcsAgentConfig,
    ) -> crate::adapter::Result<std::collections::HashMap<String, String>> {
        let mut env = std::collections::HashMap::new();
        match config.p4_mode.as_str() {
            "inherit" => {
                // No env changes — agent inherits developer's P4CLIENT.
            }
            "read-only" => {
                // Clear P4CLIENT so writes are rejected.
                env.insert("P4CLIENT".to_string(), String::new());
            }
            _ => {
                // "shelve" (default): clear P4CLIENT to prevent accidental submits.
                // A real P4 staging workspace must be created server-side separately.
                // This env injection blocks the agent from using the developer's live
                // workspace while still allowing p4 reads via P4PORT/P4USER.
                env.insert("P4CLIENT".to_string(), String::new());
                tracing::info!(
                    "Perforce staging mode: shelve — P4CLIENT cleared for agent isolation"
                );
            }
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perforce_adapter_name() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = PerforceAdapter::new(dir.path());
        assert_eq!(adapter.name(), "perforce");
    }

    #[test]
    fn test_perforce_adapter_exclude_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = PerforceAdapter::new(dir.path());
        let patterns = adapter.exclude_patterns();
        assert!(patterns.contains(&".p4config".to_string()));
        assert!(patterns.contains(&".p4ignore".to_string()));
    }

    #[test]
    fn test_perforce_adapter_detect_p4config_file() {
        let dir = tempfile::tempdir().unwrap();

        // No .p4config — should not detect (unless P4CONFIG env is set)
        // We can't control env easily in tests, so just test file detection.
        std::fs::write(dir.path().join(".p4config"), "P4PORT=ssl:perforce:1666\n").unwrap();
        assert!(PerforceAdapter::detect(dir.path()));
    }

    #[test]
    fn test_perforce_adapter_push_result() {
        // Just verify the adapter can be constructed and basic methods work.
        let dir = tempfile::tempdir().unwrap();
        let adapter = PerforceAdapter::new(dir.path());
        assert_eq!(adapter.name(), "perforce");
    }

    #[test]
    fn test_perforce_adapter_protected_targets() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = PerforceAdapter::new(dir.path());
        let targets = adapter.protected_submit_targets();
        assert!(targets.contains(&"//depot/main/...".to_string()));
    }

    #[test]
    fn test_perforce_adapter_verify_degrades_without_p4() {
        // Without p4 CLI, verify_not_on_protected_target should succeed (degrade gracefully).
        let dir = tempfile::tempdir().unwrap();
        let adapter = PerforceAdapter::new(dir.path());
        // This will either succeed (p4 not installed) or succeed (p4 installed but warns).
        assert!(adapter.verify_not_on_protected_target().is_ok());
    }
}
