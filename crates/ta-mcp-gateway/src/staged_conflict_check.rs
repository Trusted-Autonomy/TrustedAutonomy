//! Filesystem-backed [`DraftLookup`] for `ta-agent-whiteboard`'s
//! staged-conflict detection (v0.17.11.7) — reads real `DraftPackage` JSON
//! files out of `.ta/pr_packages/`, the same directory
//! `ta_changeset::count_pending_drafts` already reads for the "N drafts
//! awaiting review" notification.
//!
//! Lives here (not in `ta-agent-whiteboard`) because this crate already
//! depends on both `ta-agent-whiteboard` and `ta-changeset` —
//! `ta-agent-whiteboard` itself stays free of a `ta-changeset` dependency
//! by design (see `docs/design/staged-resource-conflict-detection.md` §4).
//!
//! Same fail-open philosophy as `whiteboard_check.rs`: an unreadable
//! directory or a malformed individual draft file is treated as "no
//! conflict information available for that entry," never surfaced as an
//! error to the caller — this is advisory information, and a broken
//! advisory check must never block or crash the thing it's advising.

use std::path::{Path, PathBuf};

use ta_agent_whiteboard::staged_conflicts::{
    staged_conflicts_for, DraftLookup, PendingDraftResources, StagedConflict,
};
use ta_agent_whiteboard::Result;
use ta_changeset::{DraftPackage, DraftStatus};

/// Reads `.ta/pr_packages/*.json` under a given directory on each call —
/// no caching, since this is meant for the same kind of infrequent,
/// pre-launch advisory check `whiteboard_check.rs` already does, not a hot
/// path.
pub struct FsDraftLookup {
    pr_packages_dir: PathBuf,
}

impl FsDraftLookup {
    pub fn new(pr_packages_dir: PathBuf) -> Self {
        Self { pr_packages_dir }
    }

    /// Convenience constructor matching the `<project_root>/.ta/pr_packages`
    /// convention used everywhere else this directory is derived from a
    /// project root (`ta-daemon`'s `project_context.rs`, `watchdog.rs`,
    /// this crate's own `config.rs`).
    pub fn for_project_root(project_root: &Path) -> Self {
        Self::new(project_root.join(".ta").join("pr_packages"))
    }
}

impl DraftLookup for FsDraftLookup {
    fn pending_draft_resources(&self) -> Result<Vec<PendingDraftResources>> {
        let Ok(entries) = std::fs::read_dir(&self.pr_packages_dir) else {
            // No drafts directory yet is a completely normal state for a
            // project with nothing staged — not an error.
            return Ok(Vec::new());
        };

        let mut result = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(pkg) = serde_json::from_str::<DraftPackage>(&content) else {
                continue;
            };
            if !is_still_staged(&pkg.status) {
                continue;
            }
            let resource_uris = pkg
                .changes
                .artifacts
                .iter()
                .map(|a| a.resource_uri.clone())
                .collect();
            result.push(PendingDraftResources {
                draft_id: pkg.package_id.to_string(),
                goal_run_id: pkg.goal.goal_id.clone(),
                resource_uris,
            });
        }
        Ok(result)
    }
}

/// A draft still represents a real conflict risk unless it's already
/// `Applied` (the change is now real, this is TA core-record-keeping, not
/// staged-but-pending) or has been ruled irrelevant (`Denied`,
/// `Superseded`, `Closed`). `Approved`-but-not-yet-`ta draft apply`'d
/// drafts are deliberately still included — approval means reviewed, not
/// landed.
fn is_still_staged(status: &DraftStatus) -> bool {
    !matches!(
        status,
        DraftStatus::Applied { .. }
            | DraftStatus::Denied { .. }
            | DraftStatus::Superseded { .. }
            | DraftStatus::Closed { .. }
    )
}

/// Human-readable descriptions of staged drafts that already touch any of
/// `resource_uris`, for the same kind of pre-launch advisory surfacing
/// `whiteboard_check.rs::other_active_agents_on` already does for live
/// presence. Never errors — a broken drafts directory degrades to "no
/// conflict information," matching this whole subsystem's fail-open
/// philosophy.
pub fn staged_conflicts_on(project_root: &Path, resource_uris: &[String]) -> Vec<String> {
    let lookup = FsDraftLookup::for_project_root(project_root);
    match staged_conflicts_for(&lookup, resource_uris) {
        Ok(conflicts) => conflicts.iter().map(describe).collect(),
        Err(e) => {
            tracing::debug!(error = %e, "staged-conflict check: query failed");
            Vec::new()
        }
    }
}

fn describe(conflict: &StagedConflict) -> String {
    format!(
        "{} (draft {}, goal {})",
        conflict.resource_uri, conflict.draft_id, conflict.goal_run_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    /// Builds a minimal-but-real `DraftPackage` JSON document and writes it
    /// to `dir`. Built as raw JSON rather than a `DraftPackage` struct
    /// literal deliberately — the struct has many fields, and
    /// `ta_changeset::draft_package::make_test_pkg` (the crate's own
    /// canonical test fixture) is `#[cfg(test)]`-gated so it isn't visible
    /// across the crate boundary from here. Every field below is either
    /// required or explicitly exercised; everything else relies on the
    /// real type's own `#[serde(default)]` coverage (confirmed against
    /// `draft_package.rs`'s own `artifact_without_new_fields_deserializes_with_defaults`
    /// test before relying on it).
    ///
    /// `status_json` is the *value* of the `status` field — `DraftStatus`
    /// is internally tagged (`#[serde(tag = "status")]`), so nested under
    /// `DraftPackage.status` it serializes as `{"status": {"status": "..."}}`
    /// (double-nested "status" key) — easy to get wrong by hand, so callers
    /// pass the inner value via the small helpers below rather than
    /// reconstructing the shape at each call site.
    fn write_draft(
        dir: &Path,
        goal_id: &str,
        status_json: serde_json::Value,
        resource_uris: &[&str],
    ) -> Uuid {
        let package_id = Uuid::new_v4();
        let artifacts: Vec<serde_json::Value> = resource_uris
            .iter()
            .map(|uri| {
                json!({
                    "resource_uri": uri,
                    "change_type": "modify",
                    "diff_ref": "changeset:0"
                })
            })
            .collect();
        let pkg = json!({
            "package_version": "1.0.0",
            "package_id": package_id,
            "created_at": chrono::Utc::now(),
            "goal": {
                "goal_id": goal_id,
                "title": "Test goal",
                "objective": "test",
                "success_criteria": [],
                "constraints": [],
                "parent_goal_title": null
            },
            "iteration": {
                "iteration_id": "iter-1",
                "sequence": 1,
                "workspace_ref": {
                    "type": "staging_dir",
                    "ref": "staging/test",
                    "base_ref": null
                }
            },
            "agent_identity": {
                "agent_id": "test-agent",
                "agent_type": "test",
                "constitution_id": "default",
                "capability_manifest_hash": "abc",
                "orchestrator_run_id": null
            },
            "summary": {
                "what_changed": "test",
                "why": "test",
                "impact": "none",
                "rollback_plan": "none",
                "open_questions": [],
                "alternatives_considered": []
            },
            "plan": {
                "completed_steps": [],
                "next_steps": [],
                "decision_log": []
            },
            "changes": {
                "artifacts": artifacts,
                "patch_sets": [],
                "pending_actions": []
            },
            "risk": {
                "risk_score": 0,
                "findings": [],
                "policy_decisions": []
            },
            "provenance": {
                "inputs": [],
                "tool_trace_hash": "test"
            },
            "review_requests": {
                "requested_actions": [],
                "reviewers": [],
                "required_approvals": 1,
                "notes_to_reviewer": null
            },
            "signatures": {
                "package_hash": "test",
                "agent_signature": "test",
                "gateway_attestation": null
            },
            "status": status_json,
        });
        std::fs::write(
            dir.join(format!("{package_id}.json")),
            serde_json::to_string(&pkg).unwrap(),
        )
        .unwrap();
        // Fail fast in the test itself if the fixture doesn't even
        // round-trip as a real DraftPackage — better than a confusing
        // "0 conflicts found" failure two calls downstream.
        let _: DraftPackage = serde_json::from_str(&serde_json::to_string(&pkg).unwrap())
            .expect("test fixture must deserialize as a real DraftPackage");
        package_id
    }

    fn pending_review_status() -> serde_json::Value {
        json!({"status": "pending_review"})
    }

    fn approved_status() -> serde_json::Value {
        json!({"status": "approved", "approved_by": "michael", "approved_at": chrono::Utc::now()})
    }

    fn applied_status() -> serde_json::Value {
        json!({"status": "applied", "applied_at": chrono::Utc::now(), "applied_via": {"via": "manual"}})
    }

    #[test]
    fn detects_conflict_from_a_real_staged_draft_file() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_packages = tmp.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr_packages).unwrap();
        write_draft(
            &pr_packages,
            "goal-1",
            pending_review_status(),
            &["fs://workspace/src/auth.rs"],
        );

        let conflicts = staged_conflicts_on(tmp.path(), &["fs://workspace/src/**".to_string()]);

        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("fs://workspace/src/auth.rs"));
        assert!(conflicts[0].contains("goal-1"));
    }

    #[test]
    fn applied_drafts_are_not_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_packages = tmp.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr_packages).unwrap();
        write_draft(
            &pr_packages,
            "goal-1",
            applied_status(),
            &["fs://workspace/src/auth.rs"],
        );

        let conflicts = staged_conflicts_on(tmp.path(), &["fs://workspace/src/**".to_string()]);

        assert!(conflicts.is_empty());
    }

    #[test]
    fn approved_but_not_yet_applied_still_counts_as_staged() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_packages = tmp.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr_packages).unwrap();
        write_draft(
            &pr_packages,
            "goal-1",
            approved_status(),
            &["fs://workspace/src/auth.rs"],
        );

        let conflicts = staged_conflicts_on(tmp.path(), &["fs://workspace/src/**".to_string()]);

        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn missing_pr_packages_dir_returns_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();

        let conflicts = staged_conflicts_on(tmp.path(), &["fs://workspace/src/**".to_string()]);

        assert!(conflicts.is_empty());
    }

    #[test]
    fn malformed_draft_file_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_packages = tmp.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr_packages).unwrap();
        std::fs::write(pr_packages.join("garbage.json"), "{not valid json").unwrap();
        write_draft(
            &pr_packages,
            "goal-2",
            json!({"status": "draft"}),
            &["fs://workspace/src/lib.rs"],
        );

        let conflicts = staged_conflicts_on(tmp.path(), &["fs://workspace/src/**".to_string()]);

        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("goal-2"));
    }
}
