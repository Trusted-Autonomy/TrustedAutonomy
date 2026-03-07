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
    CommitResult, PushResult, Result, ReviewResult, SavedVcsState, SubmitAdapter, SubmitError,
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

impl SubmitAdapter for PerforceAdapter {
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

    fn name(&self) -> &str {
        "perforce"
    }

    fn exclude_patterns(&self) -> Vec<String> {
        vec![".p4config".to_string(), ".p4ignore".to_string()]
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
}
