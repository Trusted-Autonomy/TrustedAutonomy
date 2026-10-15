//! Executes the action of a finding.

use std::path::Path;

use chrono::Utc;
use ta_changeset::draft_package::DraftStatus;
use ta_goal::{GoalRunState, GoalRunStore};
use uuid::Uuid;

use crate::model::*;
use crate::scan::dir_size;

/// File-level closer: marks the draft Closed and mirrors the goal. The CLI
/// replaces it with one that also handles the audit ledger and daemon claims.
pub struct FileCloser;

impl DraftCloser for FileCloser {
    fn close(
        &self,
        paths: &Paths,
        draft_id: Uuid,
        reason: &str,
        applied_externally_ref: Option<&str>,
    ) -> anyhow::Result<()> {
        let path = paths.pr_packages_dir.join(format!("{draft_id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!("cannot read draft {draft_id} at {}: {e}", path.display())
        })?;
        let mut pkg: ta_changeset::draft_package::DraftPackage = serde_json::from_str(&text)?;
        if !matches!(
            pkg.status,
            DraftStatus::Approved { .. } | DraftStatus::PendingReview | DraftStatus::Draft
        ) {
            anyhow::bail!(
                "draft {draft_id} is already {}; nothing to close",
                pkg.status
            );
        }
        pkg.status = DraftStatus::Closed {
            closed_at: Utc::now(),
            reason: Some(reason.to_string()),
            closed_by: "ta-cleanup".to_string(),
            applied_externally_ref: applied_externally_ref.map(str::to_string),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&pkg)?)?;
        if paths.goals_dir.exists() {
            let store = GoalRunStore::new(&paths.goals_dir)?;
            for g in store.list()? {
                if g.pr_package_id == Some(draft_id)
                    && matches!(
                        g.state,
                        GoalRunState::PrReady
                            | GoalRunState::UnderReview
                            | GoalRunState::Approved { .. }
                    )
                {
                    store.transition(
                        g.goal_run_id,
                        GoalRunState::Closed {
                            reason: Some(reason.to_string()),
                            applied_externally_ref: applied_externally_ref.map(str::to_string),
                        },
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Run one action. Returns a summary and the bytes it freed.
pub fn execute(e: &Engine, action: &Action) -> anyhow::Result<(String, u64)> {
    match action {
        Action::None => Ok(("kept as is".into(), 0)),
        Action::CloseDraft {
            draft_id,
            reason,
            applied_externally_ref,
        } => {
            e.closer.close(
                &e.paths,
                *draft_id,
                reason,
                applied_externally_ref.as_deref(),
            )?;
            Ok((
                match applied_externally_ref {
                    Some(r) => format!(
                        "closed draft {} as applied externally ({r})",
                        &draft_id.to_string()[..8]
                    ),
                    None => format!("closed draft {} ({reason})", &draft_id.to_string()[..8]),
                },
                0,
            ))
        }
        Action::RefreshVcsLabel {
            draft_id,
            new_state,
            review_ref,
        } => {
            let path = e.paths.pr_packages_dir.join(format!("{draft_id}.json"));
            let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            let vcs = v
                .get_mut("vcs_status")
                .and_then(|x| x.as_object_mut())
                .ok_or_else(|| anyhow::anyhow!("draft {draft_id} has no VCS status to refresh"))?;
            vcs.insert("review_state".into(), new_state.clone().into());
            vcs.insert("last_checked".into(), Utc::now().to_rfc3339().into());
            std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
            Ok((
                format!("label for review {review_ref} set to {new_state}"),
                0,
            ))
        }
        Action::SetGoalState {
            goal_id,
            state,
            reason,
            applied_externally_ref,
        } => {
            let store = GoalRunStore::new(&e.paths.goals_dir)?;
            let new = match state.as_str() {
                "applied" => GoalRunState::Applied,
                _ => GoalRunState::Closed {
                    reason: Some(reason.clone()),
                    applied_externally_ref: applied_externally_ref.clone(),
                },
            };
            store.transition(*goal_id, new)?;
            Ok((
                format!("goal {} set to {state}", &goal_id.to_string()[..8]),
                0,
            ))
        }
        Action::RemoveDir { path } => remove(path, true),
        Action::RemoveFile { path } => remove(path, false),
        Action::ReleasePhaseClaim { phase } => {
            e.truth.release_phase_claim(phase)?;
            Ok((format!("released daemon claim on phase {phase}"), 0))
        }
        Action::ArchiveRecords { goals, drafts } => {
            let n = crate::archive::archive_records(&e.paths, goals, drafts)?;
            Ok((format!("archived {n} record(s) under .ta/archive/"), 0))
        }
    }
}

fn remove(path: &Path, dir: bool) -> anyhow::Result<(String, u64)> {
    let bytes = dir_size(path);
    let r = if dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match r {
        Ok(()) => Ok((
            format!("removed {} ({} bytes)", path.display(), bytes),
            bytes,
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok((format!("{} was already gone", path.display()), 0))
        }
        Err(err) => Err(anyhow::anyhow!(
            "could not remove {}: {err}. Check permissions or remove it by hand.",
            path.display()
        )),
    }
}
