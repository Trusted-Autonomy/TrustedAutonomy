//! Where a goal's commit goes: the open release branch, main (hotfix), or the
//! configured target branch.
//!
//! Precedence, highest first:
//!
//! 1. **Hotfix** (`--hotfix` with a reason): the goal opts out of the release. It
//!    targets the configured `[submit.git] target_branch` (main by default) with a
//!    per-goal PR, exactly as without a release, and is recorded as a hotfix.
//! 2. **Open release** (`.ta/release-set.json` has an open release): the goal commits
//!    onto `release/<name>`. No per-goal PR: the push is deferred to
//!    `ta release-set cut`.
//! 3. **Configured target** (`[submit.git] target_branch`, default `main`): today's behaviour.
//!
//! Only the git adapter has branches to route; for other adapters the config is
//! returned unchanged.

use std::path::Path;

use ta_goal::release_set::ReleaseSetStore;

use crate::adapter::{Result, SubmitError};
use crate::config::SubmitConfig;

/// Which rule decided the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteKind {
    /// An open release: commit onto its branch, defer the push to `cut`.
    Release {
        release_id: String,
        release_name: String,
        branch: String,
    },
    /// An explicit opt-out: target the configured branch with its own PR.
    Hotfix {
        reason: String,
        /// The open release this goal bypasses, when one is open.
        bypassed_release: Option<String>,
    },
    /// No release, no hotfix: the configured (or default) target branch.
    Configured,
}

/// The submit config to use for one goal, and why.
#[derive(Debug, Clone)]
pub struct SubmitRoute {
    pub submit: SubmitConfig,
    pub kind: RouteKind,
}

impl SubmitRoute {
    /// The branch the goal's work ends up on (the release branch, or the PR target).
    pub fn target_branch(&self) -> &str {
        match &self.kind {
            RouteKind::Release { branch, .. } => branch,
            _ => &self.submit.git.target_branch,
        }
    }

    /// True when the push and the PR are left to `ta release-set cut`.
    pub fn defers_push(&self) -> bool {
        matches!(self.kind, RouteKind::Release { .. })
    }

    /// One line saying where this goal goes and why, for the apply output.
    pub fn describe(&self) -> String {
        match &self.kind {
            RouteKind::Release {
                release_name,
                branch,
                ..
            } => format!(
                "release '{release_name}' is open: committing onto {branch}; no per-goal PR \
                 (the push and the one PR to {} happen at `ta release-set cut`)",
                self.submit.git.target_branch
            ),
            RouteKind::Hotfix {
                reason,
                bypassed_release,
            } => format!(
                "hotfix ({reason}): targeting {} with its own PR{}",
                self.submit.git.target_branch,
                bypassed_release
                    .as_ref()
                    .map(|r| format!(", bypassing the open release '{r}'"))
                    .unwrap_or_default()
            ),
            RouteKind::Configured => format!(
                "no release open: targeting {} (per-goal PR)",
                self.submit.git.target_branch
            ),
        }
    }
}

/// Decide where a goal's work goes. See the module docs for the precedence.
///
/// `hotfix_reason` is `Some` only when the goal was marked `--hotfix`; the reason is
/// mandatory at the CLI, and an empty one is refused here as well.
///
/// A release record that cannot be read is an error, never "assume no release":
/// that would send every goal to main as its own PR, the cost the release exists
/// to avoid.
pub fn resolve_route(
    project_root: &Path,
    submit: &SubmitConfig,
    hotfix_reason: Option<&str>,
) -> Result<SubmitRoute> {
    let open = ReleaseSetStore::new(project_root)
        .open_release()
        .map_err(|e| SubmitError::ConfigError(e.to_string()))?;
    let mut cfg = submit.clone();
    cfg.git.release_branch = None;

    if let Some(reason) = hotfix_reason {
        if reason.trim().is_empty() {
            return Err(SubmitError::ConfigError(
                "--hotfix needs a reason: pass --hotfix-reason \"<why this cannot wait for the \
                 release>\". It is recorded in the release state."
                    .to_string(),
            ));
        }
        return Ok(SubmitRoute {
            submit: cfg,
            kind: RouteKind::Hotfix {
                reason: reason.trim().to_string(),
                bypassed_release: open.map(|r| r.name),
            },
        });
    }

    if let Some(rs) = open {
        if submit.adapter != "git" && submit.adapter != "none" {
            return Err(SubmitError::ConfigError(format!(
                "Release '{}' is open on branch {}, but this project's submit adapter is '{}'. \
                 Releases route git branches only. Cut or abandon the release, or mark this goal \
                 `--hotfix --hotfix-reason \"...\"`.",
                rs.name, rs.branch, submit.adapter
            )));
        }
        cfg.git.release_branch = Some(rs.branch.clone());
        return Ok(SubmitRoute {
            submit: cfg,
            kind: RouteKind::Release {
                release_id: rs.id,
                release_name: rs.name,
                branch: rs.branch,
            },
        });
    }

    Ok(SubmitRoute {
        submit: cfg,
        kind: RouteKind::Configured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::release_set::NewRelease;

    fn open_release(root: &Path) {
        ReleaseSetStore::new(root)
            .start(NewRelease {
                name: "0.18".into(),
                objective: "Ship billing".into(),
                wayfinder_goal_id: None,
                plan_phases: vec![],
                base_branch: "main".into(),
                base_commit: "abc".into(),
            })
            .unwrap();
    }

    fn submit_with_target(target: &str) -> SubmitConfig {
        let mut s = SubmitConfig::default();
        s.git.target_branch = target.into();
        s
    }

    #[test]
    fn no_release_uses_the_configured_target() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_route(dir.path(), &submit_with_target("develop"), None).unwrap();
        assert_eq!(r.kind, RouteKind::Configured);
        assert_eq!(r.target_branch(), "develop");
        assert!(!r.defers_push());
        assert!(r.submit.git.release_branch.is_none());
    }

    #[test]
    fn open_release_beats_the_configured_target() {
        let dir = tempfile::tempdir().unwrap();
        open_release(dir.path());
        let r = resolve_route(dir.path(), &submit_with_target("develop"), None).unwrap();
        assert!(matches!(r.kind, RouteKind::Release { .. }));
        assert_eq!(r.target_branch(), "release/0.18");
        assert_eq!(r.submit.git.release_branch.as_deref(), Some("release/0.18"));
        assert!(r.defers_push());
        assert!(r.describe().contains("no per-goal PR"));
        assert!(r.describe().contains("develop"), "names the cut's PR base");
    }

    #[test]
    fn hotfix_beats_the_open_release_and_targets_the_configured_branch() {
        let dir = tempfile::tempdir().unwrap();
        open_release(dir.path());
        let r = resolve_route(
            dir.path(),
            &submit_with_target("main"),
            Some("prod is down"),
        )
        .unwrap();
        assert_eq!(
            r.kind,
            RouteKind::Hotfix {
                reason: "prod is down".into(),
                bypassed_release: Some("0.18".into())
            }
        );
        assert_eq!(r.target_branch(), "main");
        assert!(!r.defers_push());
        assert!(r.submit.git.release_branch.is_none());
    }

    #[test]
    fn hotfix_without_a_release_is_still_a_hotfix() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_route(dir.path(), &SubmitConfig::default(), Some("x")).unwrap();
        assert!(matches!(
            r.kind,
            RouteKind::Hotfix {
                bypassed_release: None,
                ..
            }
        ));
    }

    #[test]
    fn empty_hotfix_reason_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_route(dir.path(), &SubmitConfig::default(), Some("  "))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--hotfix-reason"), "{err}");
    }

    #[test]
    fn a_cut_release_no_longer_routes() {
        let dir = tempfile::tempdir().unwrap();
        open_release(dir.path());
        ReleaseSetStore::new(dir.path())
            .update(|rs| rs.mark_cut(None))
            .unwrap();
        let r = resolve_route(dir.path(), &SubmitConfig::default(), None).unwrap();
        assert_eq!(r.kind, RouteKind::Configured);
    }

    #[test]
    fn unreadable_release_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/release-set.json"), "garbage").unwrap();
        assert!(resolve_route(dir.path(), &SubmitConfig::default(), None).is_err());
    }

    #[test]
    fn non_git_adapter_with_an_open_release_is_refused_with_the_way_out() {
        let dir = tempfile::tempdir().unwrap();
        open_release(dir.path());
        let s = SubmitConfig {
            adapter: "svn".into(),
            ..SubmitConfig::default()
        };
        let err = resolve_route(dir.path(), &s, None).unwrap_err().to_string();
        assert!(err.contains("--hotfix") && err.contains("svn"), "{err}");
    }
}
