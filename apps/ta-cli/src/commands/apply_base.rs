//! Guards around `ta draft apply` that keep an interrupted or ill-based apply
//! recoverable and observable (v0.17.12.25 item 14):
//!
//! * a stale `.ta/apply.lock` (the applying process died) is reported with what
//!   was found and the exact commands to continue or discard;
//! * an apply on a branch that is behind `origin/<default>` is refused before
//!   anything is written, naming the commit it expected, because the PR it
//!   would open shows everyone else's newer work as deletions;
//! * the pre-submit verification announces how long the last run took, so a
//!   caller with a shorter timeout knows it will cut the apply short.
//!
//! The git checks read local refs only. They never fetch: `git fetch` is the
//! user's action, and the refusal says so.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

thread_local! {
    static ALLOW_STALE_BASE: Cell<bool> = const { Cell::new(false) };
}

/// Scoped carrier for `ta draft apply --allow-stale-base`; see `KeepBuildOutputGuard`.
pub struct AllowStaleBaseGuard {
    previous: bool,
}

impl AllowStaleBaseGuard {
    pub fn set(allowed: bool) -> Self {
        let previous = ALLOW_STALE_BASE.with(|c| c.replace(allowed));
        Self { previous }
    }
}

impl Drop for AllowStaleBaseGuard {
    fn drop(&mut self) {
        ALLOW_STALE_BASE.with(|c| c.set(self.previous));
    }
}

fn stale_base_allowed() -> bool {
    ALLOW_STALE_BASE.with(|c| c.get())
}

/// Run `git -C root args...`; `Some(stdout)` only when git succeeded.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A branch that does not contain the latest `origin/<default>` commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBase {
    pub branch: String,
    /// `origin/main`, `origin/master`, or whatever `origin/HEAD` names.
    pub base_ref: String,
    /// Commits on `base_ref` that the branch does not contain.
    pub behind: usize,
    /// Short hash and subject of the newest commit on `base_ref`.
    pub expected_sha: String,
    pub expected_subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseCheck {
    /// Nothing to compare with (not a git repo, no `origin`, no default ref).
    NotApplicable(String),
    /// The branch contains everything on `base_ref`.
    Current {
        base_ref: String,
    },
    Stale(StaleBase),
}

/// The remote default branch as a local ref, from `origin/HEAD` or the usual names.
fn default_base_ref(root: &Path) -> Option<String> {
    if let Some(head) = git(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        if git(root, &["rev-parse", "--verify", "--quiet", &head]).is_some() {
            return Some(head);
        }
    }
    ["origin/main", "origin/master"].into_iter().find_map(|r| {
        git(root, &["rev-parse", "--verify", "--quiet", r])
            .is_some()
            .then(|| r.to_string())
    })
}

/// Compare the checked-out branch with the local `origin/<default>` ref.
pub fn check_base(root: &Path) -> BaseCheck {
    if git(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return BaseCheck::NotApplicable("not a git working tree".into());
    }
    let Some(base_ref) = default_base_ref(root) else {
        return BaseCheck::NotApplicable("no origin/main or origin/master ref".into());
    };
    let Some(behind) = git(root, &["rev-list", "--count", &format!("HEAD..{base_ref}")])
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return BaseCheck::NotApplicable("HEAD has no commits to compare".into());
    };
    if behind == 0 {
        return BaseCheck::Current { base_ref };
    }
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "HEAD".into());
    let latest = git(root, &["log", "-1", "--format=%h%x09%s", &base_ref]).unwrap_or_default();
    let (sha, subject) = latest.split_once('\t').unwrap_or((latest.as_str(), ""));
    BaseCheck::Stale(StaleBase {
        branch,
        base_ref,
        behind,
        expected_sha: sha.to_string(),
        expected_subject: subject.to_string(),
    })
}

/// The refusal printed when a stale base stops an apply.
pub fn stale_base_message(s: &StaleBase, draft_ref: &str) -> String {
    format!(
        "Refusing to apply: branch '{branch}' is {behind} commit(s) behind {base} \
         (as of your last `git fetch`). It does not contain {base}'s latest commit \
         {sha} \"{subject}\", so the branch this apply creates would show everyone else's \
         newer work as deletions in its pull request.\n\
         Nothing was changed.\n\
         To fix: `git fetch origin && git merge --ff-only {base}` (on a feature branch: \
         `git rebase {base}`), then re-run `ta draft apply {draft_ref}`.\n\
         To apply on this base anyway (for example a stacked branch): \
         `ta draft apply {draft_ref} --allow-stale-base`.",
        branch = s.branch,
        behind = s.behind,
        base = s.base_ref,
        sha = s.expected_sha,
        subject = s.expected_subject,
    )
}

/// Refuse (with the message above) when the apply's base is behind
/// `origin/<default>`, unless `--allow-stale-base` was given.
pub fn enforce_current_base(root: &Path, draft_ref: &str) -> anyhow::Result<()> {
    match check_base(root) {
        BaseCheck::Current { base_ref } => {
            tracing::debug!(base = %base_ref, "apply base contains the latest {base_ref}");
            Ok(())
        }
        BaseCheck::NotApplicable(why) => {
            tracing::debug!(reason = %why, "apply base check skipped");
            Ok(())
        }
        BaseCheck::Stale(s) if stale_base_allowed() => {
            eprintln!(
                "[apply] Warning: branch '{}' is {} commit(s) behind {} (latest {} \"{}\"); \
                 continuing because --allow-stale-base was given.",
                s.branch, s.behind, s.base_ref, s.expected_sha, s.expected_subject
            );
            tracing::warn!(
                branch = %s.branch,
                base = %s.base_ref,
                behind = s.behind,
                "applying on a base behind origin (--allow-stale-base)"
            );
            Ok(())
        }
        BaseCheck::Stale(s) => {
            tracing::warn!(
                branch = %s.branch,
                base = %s.base_ref,
                behind = s.behind,
                expected = %s.expected_sha,
                "apply refused: the branch is behind origin"
            );
            anyhow::bail!("{}", stale_base_message(&s, draft_ref))
        }
    }
}

/// What was found in a lock whose process is gone.
#[derive(Debug, Clone)]
pub struct StaleLock {
    pub pid: u32,
    pub draft_id: String,
    pub started_at: String,
    pub lock_path: PathBuf,
}

/// Paths under `root` with uncommitted changes, ignoring `.ta/` bookkeeping.
/// `None` when git cannot say.
pub fn dirty_path_count(root: &Path) -> Option<usize> {
    let porcelain = git(root, &["status", "--porcelain"])?;
    Some(
        porcelain
            .lines()
            .filter(|l| l.len() > 3 && !l[3..].trim_start_matches('"').starts_with(".ta/"))
            .count(),
    )
}

/// The report printed when an apply finds a lock left by a dead process.
///
/// `recovered` is how many interrupted-apply journals startup recovery handled
/// just before the lock was inspected; `dirty` is [`dirty_path_count`];
/// `this_draft` says the new apply is for the draft that was interrupted.
pub fn stale_lock_report(
    lock: &StaleLock,
    this_draft: bool,
    recovered: usize,
    dirty: Option<usize>,
) -> String {
    let tree = if recovered > 0 {
        format!(
            "Startup recovery handled {recovered} interrupted apply journal(s) just now \
             (the [recover] lines above say what it restored or finished)."
        )
    } else {
        match dirty {
            Some(0) => "No apply journal was left and the working tree is clean: nothing was left \
                 half-applied."
                .to_string(),
            Some(n) => format!(
                "No apply journal was left to roll back, and the working tree has {n} \
                 uncommitted change(s) that may be what the interrupted apply wrote. \
                 Inspect them with `git status`; keep them, or discard them with \
                 `git restore .` (and `git clean -n` to preview untracked files) before \
                 applying again."
            ),
            None => "No apply journal was left to roll back. Git could not report the working \
                     tree; check `git status` for half-applied files."
                .to_string(),
        }
    };
    let next = if this_draft {
        format!(
            "This apply runs draft {} again from the start.",
            lock.draft_id
        )
    } else {
        format!(
            "To apply that draft later: `ta draft apply {id}`. To discard it: `ta draft close {id}`.",
            id = lock.draft_id
        )
    };
    format!(
        "[apply] A previous `ta draft apply` for draft {draft} died: PID {pid} is no longer \
         running (started {started}) and left {lock}. Removed the stale lock.\n\
         [apply]   {tree}\n[apply]   {next}",
        draft = lock.draft_id,
        pid = lock.pid,
        started = lock.started_at,
        lock = lock.lock_path.display(),
    )
}

/// The lines `ta draft apply --status` prints under a stale lock.
pub fn stale_lock_status_lines(lock: &StaleLock) -> Vec<String> {
    vec![
        format!(
            "  Lock is stale: PID {} died while applying draft {}.",
            lock.pid, lock.draft_id
        ),
        format!(
            "  Run `ta draft apply {}` to recover and apply it again (an interrupted apply is \
             rolled back first), or `ta draft close {}` to discard the draft.",
            lock.draft_id, lock.draft_id
        ),
        format!(
            "  The lock itself can be removed with: rm {}",
            lock.lock_path.display()
        ),
    ]
}

const VERIFY_TIMING_FILE: &str = "verify-timing.json";
fn timing_path(root: &Path) -> PathBuf {
    // `.ta/store/` is gitignored: timing is machine-specific and must never
    // ride along in the apply commit.
    root.join(".ta").join("store").join(VERIFY_TIMING_FILE)
}

/// How long the last pre-submit verification took, if one was recorded.
pub fn last_verify_duration(root: &Path) -> Option<Duration> {
    let raw = std::fs::read_to_string(timing_path(root)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["secs"].as_u64().map(Duration::from_secs)
}

/// Record how long a pre-submit verification took. A failure to record is
/// logged, never fatal: it only costs the next run its up-front estimate.
pub fn record_verify_duration(root: &Path, took: Duration, commands: usize, passed: bool) {
    let body = serde_json::json!({
        "secs": took.as_secs(),
        "commands": commands,
        "passed": passed,
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });
    let path = timing_path(root);
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, body.to_string()));
    if let Err(e) = written {
        tracing::warn!(
            path = %timing_path(root).display(),
            error = %e,
            "could not record the verification duration"
        );
    }
}

fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// The line printed before the pre-submit verification starts.
pub fn verify_announcement(commands: usize, last: Option<Duration>, draft_ref: &str) -> String {
    let head = format!("Running pre-submit verification ({commands} check(s))");
    match last {
        Some(d) => format!(
            "{head}. The last run took {}: a caller timeout shorter than that will interrupt \
             this apply (an interrupted apply is rolled back by the next `ta draft apply`). To \
             run it detached: `nohup ta draft apply {draft_ref} > apply.log 2>&1 &`, then watch \
             `ta draft apply --status`.",
            human_duration(d)
        ),
        None => format!(
            "{head}. No earlier run is recorded, so its duration is unknown; a cold build can \
             take many minutes. To run it detached: `nohup ta draft apply {draft_ref} > \
             apply.log 2>&1 &`, then watch `ta draft apply --status`."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(dir: &Path, file: &str, msg: &str) {
        std::fs::write(dir.join(file), msg).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", msg]);
    }

    struct Repos {
        _tmp: tempfile::TempDir,
        work: PathBuf,
        other: PathBuf,
    }

    /// A bare `origin`, a working clone and a second clone that plays the
    /// teammate who pushes newer work.
    fn repos() -> Repos {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let work = tmp.path().join("work");
        let other = tmp.path().join("other");
        run(
            tmp.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        run(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        run(&work, &["checkout", "-q", "-b", "main"]);
        commit(&work, "a.txt", "first");
        run(&work, &["push", "-q", "-u", "origin", "main"]);
        run(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        Repos {
            _tmp: tmp,
            work,
            other,
        }
    }

    fn teammate_pushes(r: &Repos, msg: &str) {
        commit(&r.other, &format!("{}.txt", msg.replace(' ', "-")), msg);
        run(&r.other, &["push", "-q", "origin", "main"]);
        run(&r.work, &["fetch", "-q", "origin"]);
    }

    /// The incident: the local `main` was cut before origin's newer commits
    /// were fast-forwarded in, so the apply's branch lacked them.
    #[test]
    fn branch_behind_origin_is_stale_and_names_the_expected_commit() {
        let r = repos();
        teammate_pushes(&r, "CR-10 fixes");
        match check_base(&r.work) {
            BaseCheck::Stale(s) => {
                assert_eq!(s.base_ref, "origin/main");
                assert_eq!(s.behind, 1);
                assert_eq!(s.branch, "main");
                assert_eq!(s.expected_subject, "CR-10 fixes");
                assert!(!s.expected_sha.is_empty());
                let msg = stale_base_message(&s, "abc123");
                assert!(msg.contains(&s.expected_sha), "{msg}");
                assert!(msg.contains("CR-10 fixes"), "{msg}");
                assert!(msg.contains("git merge --ff-only origin/main"), "{msg}");
                assert!(msg.contains("--allow-stale-base"), "{msg}");
                assert!(msg.contains("Nothing was changed"), "{msg}");
            }
            other => panic!("expected a stale base, got {other:?}"),
        }
    }

    #[test]
    fn apply_is_refused_on_a_stale_base_and_allowed_with_the_flag() {
        let r = repos();
        teammate_pushes(&r, "newer work");
        let err = enforce_current_base(&r.work, "abc123").unwrap_err();
        assert!(err.to_string().contains("Refusing to apply"), "{err}");
        assert!(err.to_string().contains("newer work"), "{err}");

        let _g = AllowStaleBaseGuard::set(true);
        enforce_current_base(&r.work, "abc123").unwrap();
    }

    #[test]
    fn fast_forwarding_clears_the_refusal() {
        let r = repos();
        teammate_pushes(&r, "newer work");
        run(&r.work, &["merge", "-q", "--ff-only", "origin/main"]);
        assert!(matches!(check_base(&r.work), BaseCheck::Current { .. }));
        enforce_current_base(&r.work, "abc123").unwrap();
    }

    #[test]
    fn local_commits_ahead_of_origin_are_a_current_base() {
        let r = repos();
        commit(&r.work, "local.txt", "local only");
        assert!(matches!(check_base(&r.work), BaseCheck::Current { .. }));
    }

    #[test]
    fn diverged_branch_is_stale() {
        let r = repos();
        commit(&r.work, "local.txt", "local only");
        teammate_pushes(&r, "newer work");
        assert!(matches!(check_base(&r.work), BaseCheck::Stale(_)));
    }

    #[test]
    fn no_remote_and_no_repo_are_not_applicable() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_base(tmp.path()),
            BaseCheck::NotApplicable(_)
        ));
        run(tmp.path(), &["init", "-q"]);
        commit(tmp.path(), "f.txt", "x");
        assert!(matches!(
            check_base(tmp.path()),
            BaseCheck::NotApplicable(_)
        ));
        enforce_current_base(tmp.path(), "abc123").unwrap();
    }

    fn lock() -> StaleLock {
        StaleLock {
            pid: 4242,
            draft_id: "d1f2a3b4".into(),
            started_at: "2026-10-16T09:00:00Z".into(),
            lock_path: PathBuf::from("/p/.ta/apply.lock"),
        }
    }

    #[test]
    fn stale_lock_report_names_the_draft_pid_and_the_next_command() {
        let msg = stale_lock_report(&lock(), false, 0, Some(0));
        assert!(msg.contains("d1f2a3b4"), "{msg}");
        assert!(msg.contains("PID 4242"), "{msg}");
        assert!(msg.contains("nothing was left half-applied"), "{msg}");
        assert!(msg.contains("ta draft apply d1f2a3b4"), "{msg}");
        assert!(msg.contains("ta draft close d1f2a3b4"), "{msg}");
    }

    #[test]
    fn stale_lock_report_warns_about_a_dirty_tree_without_a_journal() {
        let msg = stale_lock_report(&lock(), true, 0, Some(3));
        assert!(msg.contains("3 uncommitted change(s)"), "{msg}");
        assert!(msg.contains("git status"), "{msg}");
        assert!(msg.contains("again from the start"), "{msg}");
    }

    #[test]
    fn stale_lock_report_defers_to_recovery_when_a_journal_was_handled() {
        let msg = stale_lock_report(&lock(), true, 1, Some(0));
        assert!(msg.contains("recovery handled 1"), "{msg}");
    }

    #[test]
    fn dirty_count_ignores_ta_bookkeeping() {
        let r = repos();
        assert_eq!(dirty_path_count(&r.work), Some(0));
        std::fs::create_dir_all(r.work.join(".ta")).unwrap();
        std::fs::write(r.work.join(".ta/state.json"), "{}").unwrap();
        assert_eq!(dirty_path_count(&r.work), Some(0));
        std::fs::write(r.work.join("half.txt"), "half-applied").unwrap();
        assert_eq!(dirty_path_count(&r.work), Some(1));
    }

    #[test]
    fn verify_duration_round_trips_and_shapes_the_announcement() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta")).unwrap();
        assert_eq!(last_verify_duration(tmp.path()), None);
        record_verify_duration(tmp.path(), Duration::from_secs(372), 4, true);
        let last = last_verify_duration(tmp.path());
        assert_eq!(last, Some(Duration::from_secs(372)));
        let with = verify_announcement(4, last, "abc");
        assert!(with.contains("6m 12s"), "{with}");
        assert!(with.contains("--status"), "{with}");
        let without = verify_announcement(4, None, "abc");
        assert!(without.contains("unknown"), "{without}");
    }
}
