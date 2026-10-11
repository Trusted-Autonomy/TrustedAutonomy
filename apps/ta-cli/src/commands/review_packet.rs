//! `ta run --provenance-file` / `--title`: stamping the review packet onto a goal.
//!
//! Carried into `run::execute` by a scoped guard (the same pattern as
//! `--intake-file`, see `intake_prompt::CliIntakeFileGuard`) so the flags stay
//! out of `execute`'s long parameter list.
//!
//! The provenance record is DATA for the human reviewer. It is parsed by code,
//! stored on the goal, copied to the draft and shown by `ta show draft`; it is
//! never read by prompt-building code, so nothing in it can reach an agent as
//! instructions.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use ta_goal::provenance::{self, RequestProvenance};
use ta_goal::GoalRun;

#[derive(Clone, Default)]
struct Extras {
    provenance_file: Option<PathBuf>,
    title: Option<String>,
}

thread_local! {
    static CLI_EXTRAS: RefCell<Extras> = RefCell::new(Extras::default());
}

/// Holds `--provenance-file` and `--title` for the duration of one `execute`.
pub struct CliReviewPacketGuard {
    previous: Extras,
}

impl CliReviewPacketGuard {
    pub fn set(provenance_file: Option<PathBuf>, title: Option<String>) -> Self {
        let previous = CLI_EXTRAS.with(|c| {
            c.replace(Extras {
                provenance_file,
                title,
            })
        });
        Self { previous }
    }
}

impl Drop for CliReviewPacketGuard {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        CLI_EXTRAS.with(|c| *c.borrow_mut() = previous);
    }
}

/// Parse `--provenance-file` up front so a bad record fails the launch before
/// any staging workspace exists.
pub(crate) fn validate_provenance_file(path: &Path) -> anyhow::Result<RequestProvenance> {
    provenance::load_provenance_file(path).map_err(anyhow::Error::msg)
}

/// A caller-supplied title: sanitized to one printable line, at most 200 chars.
pub(crate) fn clean_title(raw: &str) -> Option<String> {
    let one_line: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let t: String = one_line.chars().take(200).collect();
    (!t.is_empty()).then_some(t)
}

/// Apply the guard's provenance and title to a goal record. Provenance, like a
/// goal's origin, is fixed at creation: a goal that already has some keeps it.
pub(crate) fn stamp_goal(goal: &mut GoalRun, quiet: bool) -> anyhow::Result<()> {
    let extras = CLI_EXTRAS.with(|c| c.borrow().clone());
    stamp_goal_with(
        goal,
        extras.provenance_file.as_deref(),
        extras.title.as_deref(),
        quiet,
    )
}

pub(crate) fn stamp_goal_with(
    goal: &mut GoalRun,
    provenance_file: Option<&Path>,
    title: Option<&str>,
    quiet: bool,
) -> anyhow::Result<()> {
    if let Some(path) = provenance_file {
        if goal.request_provenance.is_some() {
            tracing::warn!(
                goal_id = %goal.goal_run_id,
                file = %path.display(),
                "ignoring --provenance-file: the goal already has provenance and it is fixed when the goal is created"
            );
            if !quiet {
                eprintln!(
                    "[warn] Goal {} keeps its recorded provenance; --provenance-file {} was \
                     ignored (provenance is fixed when the goal is created).",
                    goal.goal_run_id,
                    path.display()
                );
            }
        } else {
            let mut p = validate_provenance_file(path)?;
            p.ids.worker_goal_id = Some(goal.goal_run_id.to_string());
            tracing::info!(
                goal_id = %goal.goal_run_id,
                requester = %p.requester.id,
                channel = %p.requester.channel,
                "review-packet provenance recorded"
            );
            goal.request_provenance = Some(p);
        }
    }
    if let Some(t) = title.and_then(clean_title) {
        goal.title = t;
    } else if let Some(obj) = goal
        .request_provenance
        .as_ref()
        .and_then(|p| p.objective.as_deref())
    {
        // No caller title: replace a machine-made one with a readable title
        // derived from the objective.
        if provenance::looks_synthetic(&goal.title) {
            if let Some(t) = provenance::title_from_objective(obj) {
                goal.title = t;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> GoalRun {
        GoalRun::new(
            "wake:msg-1234567890abcdef",
            "obj",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        )
    }

    fn write_prov(dir: &Path) -> PathBuf {
        let path = dir.join("prov.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "requester": {"id": "u-1", "display_name": "Alice", "channel": "slack"},
                "message_excerpt": "please fix login",
                "objective": "Fix the login redirect loop. Add a test."
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    #[test]
    fn stamps_provenance_worker_id_and_readable_title() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = goal();
        stamp_goal_with(&mut g, Some(&write_prov(dir.path())), None, true).unwrap();
        let p = g.request_provenance.as_ref().unwrap();
        assert_eq!(p.requester.channel, "slack");
        assert_eq!(
            p.ids.worker_goal_id.as_deref(),
            Some(g.goal_run_id.to_string().as_str())
        );
        assert_eq!(g.title, "Fix the login redirect loop");
    }

    #[test]
    fn caller_title_wins_and_is_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = goal();
        stamp_goal_with(
            &mut g,
            Some(&write_prov(dir.path())),
            Some("  Fix\tlogin\u{1b}[2J bug \n"),
            true,
        )
        .unwrap();
        assert_eq!(g.title, "Fix login [2J bug");
    }

    #[test]
    fn existing_provenance_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = goal();
        stamp_goal_with(&mut g, Some(&write_prov(dir.path())), None, true).unwrap();
        let other = dir.path().join("other.json");
        std::fs::write(&other, r#"{"requester":{"id":"mallory","channel":"x"}}"#).unwrap();
        stamp_goal_with(&mut g, Some(&other), None, true).unwrap();
        assert_eq!(g.request_provenance.unwrap().requester.id, "u-1");
    }

    #[test]
    fn a_bad_record_fails_with_the_file_named() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, r#"{"requester":{"id":"u","channel":"c"},"evil":1}"#).unwrap();
        let err = stamp_goal_with(&mut goal(), Some(&bad), None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("bad.json") && err.contains("unknown field"),
            "{err}"
        );
    }

    #[test]
    fn no_flags_changes_nothing() {
        let mut g = goal();
        let before = g.title.clone();
        stamp_goal_with(&mut g, None, None, true).unwrap();
        assert_eq!(g.title, before);
        assert!(g.request_provenance.is_none());
    }

    #[test]
    fn guard_scopes_and_restores() {
        {
            let _g = CliReviewPacketGuard::set(None, Some("T".into()));
            CLI_EXTRAS.with(|c| assert_eq!(c.borrow().title.as_deref(), Some("T")));
        }
        CLI_EXTRAS.with(|c| assert!(c.borrow().title.is_none()));
    }
}
