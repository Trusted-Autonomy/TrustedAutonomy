//! The `branches` group against real local git repositories. PR state is faked through
//! `GroundTruth::branch_pr`, the same seam the CLI and daemon fill from the VCS adapter.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Run git, failing the test with its stderr on a non-zero exit.
fn g(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn branch_exists(dir: &Path, name: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap()
        .status
        .success()
}

/// A work repo cloned from a local bare origin, with one commit on `main` pushed.
struct Repo {
    dir: tempfile::TempDir,
    work: PathBuf,
    paths: Paths,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        g(&origin, &["init", "--bare", "-b", "main"]);
        g(&work, &["init", "-b", "main"]);
        g(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        std::fs::write(work.join("README.md"), "base\n").unwrap();
        // The engine's own state lives inside the repo; keep it out of `git status`.
        std::fs::write(work.join(".git/info/exclude"), ".ta/\n").unwrap();
        g(&work, &["add", "README.md"]);
        g(&work, &["commit", "-m", "base"]);
        g(&work, &["push", "-u", "origin", "main"]);
        g(&work, &["remote", "set-head", "origin", "main"]);
        let paths = Paths::for_project(&work);
        std::fs::create_dir_all(paths.cleanup_dir()).unwrap();
        Repo { dir, work, paths }
    }

    /// New branch off `main` with `n` commits, then back to `main`. Returns its tip.
    fn branch_with_commits(&self, name: &str, n: usize) -> String {
        g(&self.work, &["checkout", "-b", name, "main"]);
        for i in 0..n {
            self.commit_file(&format!("{}-{i}.txt", name.replace('/', "_")));
        }
        let tip = g(&self.work, &["rev-parse", "HEAD"]);
        g(&self.work, &["checkout", "main"]);
        tip
    }

    fn commit_file(&self, file: &str) {
        std::fs::write(self.work.join(file), format!("{file}\n")).unwrap();
        g(&self.work, &["add", file]);
        g(&self.work, &["commit", "-m", file]);
    }

    /// Land `branch` on main as ONE new commit, the way GitHub squash-merges. The branch tip
    /// is not an ancestor of main afterwards and `git cherry` cannot match it.
    fn squash_merge(&self, branch: &str) {
        g(&self.work, &["checkout", "main"]);
        g(&self.work, &["merge", "--squash", branch]);
        g(&self.work, &["commit", "-m", &format!("squash {branch}")]);
        g(&self.work, &["push", "origin", "main"]);
    }

    fn engine<'a>(&self, truth: &'a Truth) -> Engine<'a> {
        Engine {
            paths: self.paths.clone(),
            settings: Settings::default(),
            truth,
            closer: &FileCloser,
            assistant: None,
        }
    }
}

fn only_branches() -> RunOptions {
    RunOptions {
        only: Some(vec![Category::Branches]),
        ..RunOptions::default()
    }
}

fn pr(id: &str, state: ReviewState, head: Option<&str>) -> BranchPr {
    BranchPr::Prs(vec![BranchReview {
        review_id: id.into(),
        state,
        head_sha: head.map(str::to_string),
    }])
}

fn log_lines(p: &Paths) -> Vec<serde_json::Value> {
    std::fs::read_to_string(crate::branches::log_path(p))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn merged_squash_branch_is_deleted_and_logged_with_its_tip() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/done", 2);
    repo.squash_merge("feature/done");
    // The premise: git alone cannot see this branch as merged.
    let contained = Command::new("git")
        .args(["merge-base", "--is-ancestor", "feature/done", "main"])
        .current_dir(&repo.work)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .unwrap();
    assert!(!contained.success(), "squash merge must rewrite history");

    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/done".into(),
        pr("12", ReviewState::Merged, Some(&tip)),
    );
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();

    assert!(!branch_exists(&repo.work, "feature/done"));
    assert!(branch_exists(&repo.work, "main"));
    assert_eq!(r.done.len(), 1, "{:?}", r.done);
    assert!(r.done[0].summary.contains(&tip), "{}", r.done[0].summary);
    let log = log_lines(&repo.paths);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["branch"], "feature/done");
    assert_eq!(log[0]["tip_sha"], tip.as_str());
    // The logged recovery command works.
    let recover = log[0]["recover"].as_str().unwrap().to_string();
    let parts: Vec<&str> = recover.split(' ').skip(1).collect();
    g(&repo.work, &parts);
    assert!(branch_exists(&repo.work, "feature/done"));
}

#[test]
fn open_pr_branch_is_kept() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/wip", 1);
    let mut t = Truth::default();
    t.branch_prs
        .insert("feature/wip".into(), pr("7", ReviewState::Open, Some(&tip)));
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();
    assert!(branch_exists(&repo.work, "feature/wip"));
    assert!(r.done.is_empty());
    let f = find(&r, "branch:feature/wip");
    assert_eq!(f.verdict, Verdict::Skip);
    assert!(f.reason.contains("open"), "{}", f.reason);
}

#[test]
fn branch_with_commits_beyond_the_pr_head_is_asked_not_deleted() {
    let repo = Repo::new();
    let head = repo.branch_with_commits("feature/more", 1);
    repo.squash_merge("feature/more");
    g(&repo.work, &["checkout", "feature/more"]);
    repo.commit_file("after-merge.txt");
    g(&repo.work, &["checkout", "main"]);

    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/more".into(),
        pr("3", ReviewState::Merged, Some(&head)),
    );
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();
    assert!(branch_exists(&repo.work, "feature/more"));
    assert!(r.done.is_empty());
    let f = find(&r, "branch:feature/more");
    assert_eq!(f.verdict, Verdict::Ask);
    assert!(f.reason.contains("beyond the PR head"), "{}", f.reason);
    assert!(r.questions_open.contains(&f.id));
    assert!(f.options.iter().any(|o| o.key == "keep"));
    assert!(log_lines(&repo.paths).is_empty());
}

#[test]
fn dirty_worktree_is_asked_with_the_file_list() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/wt", 1);
    repo.squash_merge("feature/wt");
    let wt = repo.dir.path().join("wt-dirty");
    g(
        &repo.work,
        &["worktree", "add", wt.to_str().unwrap(), "feature/wt"],
    );
    std::fs::write(wt.join("scratch.txt"), "unsaved\n").unwrap();

    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/wt".into(),
        pr("4", ReviewState::Merged, Some(&tip)),
    );
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();

    assert!(branch_exists(&repo.work, "feature/wt"));
    assert!(
        wt.join("scratch.txt").exists(),
        "worktree must be untouched"
    );
    assert!(r.done.is_empty());
    let f = find(&r, "branch:feature/wt");
    assert_eq!(f.verdict, Verdict::Ask);
    let facts: String = f.evidence.iter().map(|e| e.fact.clone()).collect();
    assert!(facts.contains("scratch.txt"), "{facts}");
    assert!(
        f.reason.contains("uncommitted or untracked"),
        "{}",
        f.reason
    );
}

#[test]
fn clean_worktree_of_a_done_branch_is_removed_with_the_branch() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/clean", 1);
    repo.squash_merge("feature/clean");
    let wt = repo.dir.path().join("wt-clean");
    g(
        &repo.work,
        &["worktree", "add", wt.to_str().unwrap(), "feature/clean"],
    );

    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/clean".into(),
        pr("5", ReviewState::Merged, Some(&tip)),
    );
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();

    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert!(!branch_exists(&repo.work, "feature/clean"));
    assert!(!wt.exists(), "worktree directory should be gone");
    assert!(!g(&repo.work, &["worktree", "list"]).contains("wt-clean"));
    assert_eq!(log_lines(&repo.paths)[0]["tip_sha"], tip.as_str());
}

#[test]
fn current_and_default_branches_are_never_touched() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/current", 1);
    repo.squash_merge("feature/current");
    // Even a merged PR must not remove the branch the checkout is on.
    g(&repo.work, &["checkout", "feature/current"]);
    let main_tip = g(&repo.work, &["rev-parse", "main"]);

    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/current".into(),
        pr("6", ReviewState::Merged, Some(&tip)),
    );
    t.branch_prs
        .insert("main".into(), pr("1", ReviewState::Merged, Some(&main_tip)));
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();

    assert!(branch_exists(&repo.work, "feature/current"));
    assert!(branch_exists(&repo.work, "main"));
    assert!(r.done.is_empty());
    assert_eq!(find(&r, "branch:feature/current").verdict, Verdict::Skip);
    assert_eq!(find(&r, "branch:main").verdict, Verdict::Skip);
}

#[test]
fn branch_with_no_pr_and_unique_commits_is_asked() {
    let repo = Repo::new();
    repo.branch_with_commits("feature/nopr", 1);
    let mut t = Truth::default();
    t.branch_prs.insert("feature/nopr".into(), BranchPr::NoPr);
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();
    assert!(branch_exists(&repo.work, "feature/nopr"));
    assert_eq!(find(&r, "branch:feature/nopr").verdict, Verdict::Ask);
}

#[test]
fn unknown_pr_state_is_never_read_as_no_pr_or_merged() {
    let repo = Repo::new();
    repo.branch_with_commits("feature/unk", 1);
    // No entry: the fake adapter answers Unknown.
    let r = run(&repo.engine(&Truth::default()), &only_branches(), None).unwrap();
    assert!(branch_exists(&repo.work, "feature/unk"));
    let f = find(&r, "branch:feature/unk");
    assert_eq!(f.verdict, Verdict::Ask);
    assert!(f.reason.contains("could not be determined"), "{}", f.reason);
}

#[test]
fn tip_already_in_default_branch_is_deleted_without_a_pr_record() {
    let repo = Repo::new();
    g(&repo.work, &["branch", "feature/old"]);
    repo.commit_file("moved-on.txt");
    g(&repo.work, &["push", "origin", "main"]);
    let r = run(&repo.engine(&Truth::default()), &only_branches(), None).unwrap();
    assert!(!branch_exists(&repo.work, "feature/old"));
    assert_eq!(r.done.len(), 1, "{:?}", r.done);
}

#[test]
fn fresh_branch_on_the_default_tip_without_a_merged_pr_is_not_deleted() {
    let repo = Repo::new();
    g(&repo.work, &["branch", "feature/fresh"]);
    let mut t = Truth::default();
    t.branch_prs.insert("feature/fresh".into(), BranchPr::NoPr);
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();
    assert!(branch_exists(&repo.work, "feature/fresh"));
    assert_eq!(find(&r, "branch:feature/fresh").verdict, Verdict::Ask);
}

#[test]
fn dry_run_shows_reasons_and_changes_nothing() {
    let repo = Repo::new();
    let tip = repo.branch_with_commits("feature/dry", 1);
    repo.squash_merge("feature/dry");
    let mut t = Truth::default();
    t.branch_prs.insert(
        "feature/dry".into(),
        pr("8", ReviewState::Merged, Some(&tip)),
    );
    let r = run(
        &repo.engine(&t),
        &RunOptions {
            dry_run: true,
            ..only_branches()
        },
        None,
    )
    .unwrap();
    assert!(branch_exists(&repo.work, "feature/dry"));
    assert!(r.done.is_empty());
    let text = render_report(&r, true);
    assert!(text.contains("feature/dry"), "{text}");
    assert!(text.contains("PR is merged"), "{text}");
    assert!(log_lines(&repo.paths).is_empty());
}

#[test]
fn remote_branch_with_a_closed_unmerged_pr_is_reported_and_never_deleted() {
    let repo = Repo::new();
    repo.branch_with_commits("feature/abandoned", 1);
    g(&repo.work, &["push", "origin", "feature/abandoned"]);
    let t = Truth {
        closed_unmerged: vec!["feature/abandoned".into()],
        ..Truth::default()
    };
    let r = run(&repo.engine(&t), &only_branches(), None).unwrap();
    let f = find(&r, "remote-branch:feature/abandoned");
    assert_eq!(f.kind, Kind::RemoteBranch);
    assert!(f.reason.contains("never deletes remote branches"));
    let remote = g(&repo.work, &["ls-remote", "--heads", "origin"]);
    assert!(remote.contains("refs/heads/feature/abandoned"), "{remote}");
    assert!(r.done.is_empty());
}

#[test]
fn branches_group_can_be_turned_off_but_still_runs_when_named() {
    let repo = Repo::new();
    g(&repo.work, &["branch", "feature/old"]);
    repo.commit_file("moved-on.txt");
    g(&repo.work, &["push", "origin", "main"]);
    let t = Truth::default();
    let mut e = repo.engine(&t);
    e.settings.branches = false;

    let r = run(&e, &RunOptions::default(), None).unwrap();
    assert!(r.findings.iter().all(|f| f.kind != Kind::Branch));
    assert!(branch_exists(&repo.work, "feature/old"));

    let r = run(&e, &only_branches(), None).unwrap();
    assert_eq!(r.done.len(), 1);
    assert!(!branch_exists(&repo.work, "feature/old"));
}

#[test]
fn a_branch_that_moved_after_the_scan_is_not_deleted() {
    let repo = Repo::new();
    let old_tip = repo.branch_with_commits("feature/moved", 1);
    g(&repo.work, &["checkout", "feature/moved"]);
    repo.commit_file("late.txt");
    g(&repo.work, &["checkout", "main"]);
    let t = Truth::default();
    let e = repo.engine(&t);
    let err = crate::act::execute(
        &e,
        &Action::PruneBranch {
            branch: "feature/moved".into(),
            tip_sha: old_tip,
            worktree: None,
            force: false,
            reason: "test".into(),
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("moved from"), "{err}");
    assert!(branch_exists(&repo.work, "feature/moved"));
}

#[test]
fn a_project_that_is_not_a_git_checkout_has_no_branch_findings() {
    let fx = Fx::new();
    let t = Truth::default();
    let e = fx.engine(&t, Settings::default());
    let r = run(&e, &only_branches(), None).unwrap();
    assert!(r.findings.is_empty());
}
