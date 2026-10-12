// package_effects.rs: the real-world `ApplyEffects` for recovery (v0.17.12.3).
//
// Recovery asks two questions of the outside world: is the draft's DURABLE
// status Applied (the commit point), and can the local commit be undone. This
// implementation answers both from the draft package file (`status.status ==
// "applied"`, the same shape `ta_changeset::DraftStatus` serializes to) and from
// git. It is shared by `ta draft apply` and the daemon so both recover the
// same way.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::apply_journal::ApplyEffects;

pub struct PackageStatusEffects {
    pub pr_packages_dir: PathBuf,
    /// Project root whose `.ta/apply.lock` marks a live apply.
    pub lock_root: PathBuf,
    /// Whether a process id is alive (the caller owns the platform code).
    pub pid_alive: fn(u32) -> bool,
}

impl ApplyEffects for PackageStatusEffects {
    fn is_applied(&self, draft_id: &str) -> Result<bool, String> {
        let path = self.pr_packages_dir.join(format!("{draft_id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "could not read the draft package {}: {e}. If the draft was deleted on purpose, \
                 remove its journal directory from the daemon state directory",
                path.display()
            )
        })?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            format!(
                "the draft package {} is not valid JSON: {e}. Restore it from a backup",
                path.display()
            )
        })?;
        Ok(v.get("status")
            .and_then(|s| s.get("status"))
            .and_then(|s| s.as_str())
            == Some("applied"))
    }

    /// Reads `<project>/.ta/apply.lock` (written by `ta draft apply`) and
    /// reports whether its process is still running. Fails closed: a lock that
    /// exists but cannot be read or parsed is treated as "apply in progress",
    /// because guessing "not running" could roll back a live apply.
    fn apply_in_progress(&self, _target_root: &Path) -> bool {
        let lock = self.lock_root.join(".ta").join("apply.lock");
        let text = match std::fs::read_to_string(&lock) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
            Err(e) => {
                tracing::warn!(lock = %lock.display(), error = %e,
                    "apply lock exists but cannot be read; treating it as an apply in progress. \
                     Verify no `ta draft apply` is running, then remove the lock file");
                return true;
            }
        };
        let pid = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
            .and_then(|p| u32::try_from(p).ok());
        match pid {
            Some(pid) => (self.pid_alive)(pid),
            None => {
                tracing::warn!(lock = %lock.display(),
                    "apply lock is malformed (no valid pid); treating it as an apply in progress. \
                     Verify no `ta draft apply` is running, then remove the lock file");
                true
            }
        }
    }

    /// Resets the INDEX to the pre-apply head with `reset --mixed`, only when
    /// HEAD is still the commit the apply made. The working tree is restored
    /// separately from the journal's priors, so unrelated uncommitted work is
    /// never discarded.
    fn undo_local_commit(
        &self,
        target_root: &Path,
        commit: &str,
        prior_head: Option<&str>,
    ) -> Result<(), String> {
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(target_root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_COMMON_DIR")
                .output()
        };
        let head = git(&["rev-parse", "HEAD"]).map_err(|e| {
            format!(
                "could not run git in {}: {e}. Is git installed",
                target_root.display()
            )
        })?;
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        if head != commit {
            tracing::warn!(head = %head, commit = %commit,
                "HEAD is no longer the apply's local commit; leaving the history alone");
            return Ok(());
        }
        let Some(prior) = prior_head else {
            return Err(format!(
                "the apply made commit {commit} but the journal has no earlier head to return to. \
                 Run `git reset --mixed HEAD~1` in {} by hand",
                target_root.display()
            ));
        };
        let out = git(&["reset", "--mixed", prior])
            .map_err(|e| format!("could not run `git reset --mixed {prior}`: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "`git reset --mixed {prior}` failed in {}: {}",
                target_root.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_read_from_the_package_file() {
        let dir = tempfile::tempdir().unwrap();
        let fx = PackageStatusEffects {
            pr_packages_dir: dir.path().to_path_buf(),
            lock_root: dir.path().to_path_buf(),
            pid_alive: |_| false,
        };
        std::fs::write(
            dir.path().join("a.json"),
            r#"{"status":{"status":"applied","applied_at":"2026-10-12T00:00:00Z"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            r#"{"status":{"status":"approved","approved_by":"x","approved_at":"2026-10-12T00:00:00Z"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("c.json"), "{ torn").unwrap();
        assert!(fx.is_applied("a").unwrap());
        assert!(!fx.is_applied("b").unwrap());
        let e = fx.is_applied("c").unwrap_err();
        assert!(e.contains("not valid JSON") && e.contains("Restore"), "{e}");
        let e = fx.is_applied("missing").unwrap_err();
        assert!(e.contains("remove its journal"), "{e}");
    }

    #[test]
    fn apply_lock_with_a_live_pid_means_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let fx = PackageStatusEffects {
            pr_packages_dir: dir.path().to_path_buf(),
            lock_root: dir.path().to_path_buf(),
            pid_alive: |pid| pid == 4242,
        };
        assert!(!fx.apply_in_progress(dir.path()), "no lock file");
        let lock = dir.path().join(".ta").join("apply.lock");
        std::fs::write(&lock, r#"{"draft_id":"d","pid":4242}"#).unwrap();
        assert!(fx.apply_in_progress(dir.path()));
        std::fs::write(&lock, r#"{"draft_id":"d","pid":1}"#).unwrap();
        assert!(!fx.apply_in_progress(dir.path()), "stale lock");
    }

    #[test]
    fn garbage_or_unreadable_apply_lock_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let fx = PackageStatusEffects {
            pr_packages_dir: dir.path().to_path_buf(),
            lock_root: dir.path().to_path_buf(),
            pid_alive: |_| false,
        };
        let lock = dir.path().join(".ta").join("apply.lock");
        std::fs::write(&lock, "garbage").unwrap();
        assert!(fx.apply_in_progress(dir.path()), "garbage content");
        std::fs::write(&lock, r#"{"draft_id":"d"}"#).unwrap();
        assert!(fx.apply_in_progress(dir.path()), "no pid field");
        std::fs::remove_file(&lock).unwrap();
        // A directory in the lock's place exists but cannot be read as a file.
        std::fs::create_dir(&lock).unwrap();
        assert!(fx.apply_in_progress(dir.path()), "unreadable lock");
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn undo_resets_index_but_keeps_unrelated_working_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "1").unwrap();
        std::fs::write(root.join("other.txt"), "mine").unwrap();
        git(root, &["add", "a.txt", "other.txt"]);
        git(root, &["commit", "-q", "-m", "base"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("a.txt"), "2").unwrap();
        git(root, &["commit", "-q", "-am", "apply"]);
        let applied = git(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("other.txt"), "unrelated edit").unwrap();

        let fx = PackageStatusEffects {
            pr_packages_dir: root.to_path_buf(),
            lock_root: root.to_path_buf(),
            pid_alive: |_| false,
        };
        fx.undo_local_commit(root, &applied, Some(&base)).unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), base);
        // The unrelated edit survives; a.txt is still "2" until the journal restores it.
        assert_eq!(
            std::fs::read_to_string(root.join("other.txt")).unwrap(),
            "unrelated edit"
        );
        // A moved HEAD is left alone.
        fx.undo_local_commit(root, &applied, Some(&base)).unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), base);
    }

    #[test]
    fn undo_without_a_prior_head_explains_the_manual_step() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "base"]);
        let head = git(root, &["rev-parse", "HEAD"]);
        let fx = PackageStatusEffects {
            pr_packages_dir: root.to_path_buf(),
            lock_root: root.to_path_buf(),
            pid_alive: |_| false,
        };
        let e = fx.undo_local_commit(root, &head, None).unwrap_err();
        assert!(e.contains("git reset --mixed HEAD~1"), "{e}");
    }
}
