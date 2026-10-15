// cleanup_pass.rs: the daemon's periodic `ta cleanup`, AutoSafe findings only.
//
// Runs from the periodic gc loop when `[cleanup] auto = "safe"` (default
// "off"). It never proposes and never asks: anything that is not unambiguous
// from ground truth is left for `ta cleanup` or `ta doctor` to put to a person.

use std::path::Path;

use ta_cleanup::{act::FileCloser, Engine, GroundTruth, Paths, ReviewState, RunOptions, Settings};
use ta_submit::{SourceAdapter, WorkflowConfig};

struct DaemonTruth {
    root: std::path::PathBuf,
    adapter: Option<Box<dyn SourceAdapter>>,
}

impl GroundTruth for DaemonTruth {
    fn review_state(&self, review_ref: &str) -> ReviewState {
        let Some(a) = &self.adapter else {
            return ReviewState::Unknown("no VCS adapter is configured".into());
        };
        match a.check_review(review_ref) {
            Ok(Some(rs)) => match rs.state.to_ascii_lowercase().as_str() {
                "merged" => ReviewState::Merged,
                "closed" => ReviewState::Closed,
                "open" | "draft" => ReviewState::Open,
                other => ReviewState::Unknown(format!("unrecognised review state '{other}'")),
            },
            Ok(None) => ReviewState::Unknown("the VCS adapter does not report review state".into()),
            Err(e) => ReviewState::Unknown(format!("VCS lookup failed: {e}")),
        }
    }

    fn phase_done(&self, phase: &str) -> bool {
        let Ok(text) = std::fs::read_to_string(self.root.join("PLAN.md")) else {
            return false;
        };
        let want = phase.trim().trim_start_matches('v');
        ta_plan::parse_plan(&text).iter().any(|p| {
            p.id.trim().trim_start_matches('v') == want
                && matches!(p.status, ta_plan::PlanStatus::Done)
        })
    }
}

/// Returns the number of actions taken (0 when disabled).
pub fn run_if_enabled(root: &Path) -> usize {
    let wf = WorkflowConfig::load_or_default(&root.join(".ta/workflow.toml"));
    if !wf.cleanup.auto_safe() {
        return 0;
    }
    let adapter = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ta_submit::select_adapter(root, &wf.submit)
    }))
    .ok();
    let truth = DaemonTruth {
        root: root.to_path_buf(),
        adapter,
    };
    let engine = Engine {
        paths: Paths::for_project(root),
        settings: Settings {
            stale_days: wf.gc.stale_threshold_days,
            retention_days: wf.cleanup.retention_days,
            archive: wf.cleanup.archive,
            model_assist: false,
        },
        truth: &truth,
        closer: &FileCloser,
        assistant: None,
    };
    let opts = RunOptions {
        auto_safe_only: true,
        ..Default::default()
    };
    match ta_cleanup::run(&engine, &opts, None) {
        Ok(r) => {
            if !r.done.is_empty() || !r.failed.is_empty() {
                tracing::info!(
                    done = r.done.len(),
                    failed = r.failed.len(),
                    bytes_reclaimed = r.bytes_reclaimed,
                    "Periodic cleanup (auto = \"safe\"): {} action(s), {} failed. Details in .ta/audit.jsonl",
                    r.done.len(),
                    r.failed.len(),
                );
            }
            r.done.len()
        }
        Err(e) => {
            tracing::warn!(
                path = %root.display(),
                error = %e,
                "Periodic cleanup failed; run `ta cleanup --dry-run` to see the findings"
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(auto: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        std::fs::write(
            d.path().join(".ta/workflow.toml"),
            format!("[cleanup]\nauto = \"{auto}\"\n"),
        )
        .unwrap();
        std::fs::write(
            d.path().join(".ta/apply.lock"),
            r#"{"pid": 2000000000, "draft_id": "x"}"#,
        )
        .unwrap();
        d
    }

    #[test]
    fn off_by_default_changes_nothing() {
        let d = project("off");
        assert_eq!(run_if_enabled(d.path()), 0);
        assert!(d.path().join(".ta/apply.lock").exists());
    }

    #[test]
    fn safe_mode_removes_dead_pid_lock() {
        let d = project("safe");
        assert!(run_if_enabled(d.path()) >= 1);
        assert!(!d.path().join(".ta/apply.lock").exists());
    }
}
