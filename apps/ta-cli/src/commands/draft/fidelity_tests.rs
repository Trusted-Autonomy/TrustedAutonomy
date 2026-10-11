// Apply-fidelity tests (v0.17.11.29): the plan merge touches only the target phase, a
// dry run is a dry run, and apply never writes onto a protected branch.
//
// Every fixture lives in `tempfile::tempdir()`; nothing touches the real Keychain.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .output()
        .expect("run git");
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

const PLAN_V1: &str = "\
# Plan

## Human Tasks
- [ ] Code-signing cert review before stable release
- [ ] ARK contract sign-off

### v0.1.0 - Target phase
<!-- status: in_progress -->

1. [ ] First item
2. [ ] Second item

```text
- [ ] example checkbox inside a fence


- [ ] another one after two blank lines
```

### v0.1.1 - Another phase
<!-- status: pending -->

1. [ ] Unrelated item
2. [ ] Another unrelated item

#### Human Review
1. [ ] Maintainer signs off
";

fn new_project() -> TempDir {
    let project = TempDir::new().unwrap();
    git(project.path(), &["init", "-q"]);
    git(project.path(), &["config", "user.email", "test@test.com"]);
    git(project.path(), &["config", "user.name", "Test"]);
    std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
    std::fs::write(project.path().join("src.txt"), "old\n").unwrap();
    std::fs::write(project.path().join("PLAN.md"), PLAN_V1).unwrap();
    git(project.path(), &["add", "-A"]);
    git(project.path(), &["commit", "-q", "-m", "initial"]);
    project
}

/// Start a goal for `phase`, edit staging like an agent (and like a hostile one: every
/// checkbox checked, blank lines removed), and build the draft.
fn build_draft(project: &Path, config: &GatewayConfig, hostile_plan: bool) -> (String, GoalRun) {
    super::super::goal::execute(
        &super::super::goal::GoalCommands::Start {
            title: "Fidelity test".to_string(),
            source: Some(project.to_path_buf()),
            objective: "Exercise apply fidelity".to_string(),
            agent: "test-agent".to_string(),
            phase: Some("v0.1.0".to_string()),
            follow_up: None,
            objective_file: None,
        },
        config,
    )
    .unwrap();
    let goal = GoalRunStore::new(&config.goals_dir)
        .unwrap()
        .list()
        .unwrap()
        .remove(0);
    std::fs::write(goal.workspace_path.join("src.txt"), "new\n").unwrap();
    let staged_plan = std::fs::read_to_string(goal.workspace_path.join("PLAN.md")).unwrap();
    let edited = if hostile_plan {
        // Check every box (human gates too) and drop every blank line.
        staged_plan
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.replace("[ ]", "[x]"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    } else {
        staged_plan
            .replacen("1. [ ] First item", "1. [x] First item", 1)
            .replacen("2. [ ] Second item", "2. [x] Second item", 1)
    };
    std::fs::write(goal.workspace_path.join("PLAN.md"), edited).unwrap();
    build_package(
        config,
        &goal.goal_run_id.to_string(),
        "Fidelity test",
        false,
    )
    .unwrap();
    let pkg_id = load_all_packages(config).unwrap()[0].package_id.to_string();
    (pkg_id, goal)
}

fn apply(
    config: &GatewayConfig,
    pkg_id: &str,
    git_commit: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    apply_package(
        config,
        pkg_id,
        None,
        git_commit,
        false,
        false,
        true, // skip_verify
        dry_run,
        ta_workspace::ConflictResolution::Abort,
        SelectiveReviewPatterns::default(),
        Some("v0.1.0"),
        false,
        false,
        false,
        false,
    )
}

/// Every file under `root` except `.git`, keyed by relative path with `/` separators.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[derive(Debug, PartialEq, Eq)]
struct VcsState {
    head: String,
    branch: String,
    branches: String,
    status: String,
}

fn vcs_state(dir: &Path) -> VcsState {
    VcsState {
        head: git(dir, &["rev-parse", "HEAD"]),
        branch: git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branches: git(
            dir,
            &[
                "branch",
                "--list",
                "--format=%(refname:short) %(objectname)",
            ],
        ),
        status: git(dir, &["status", "--porcelain=v1", "--untracked-files=all"]),
    }
}

fn goal_and_draft_state(config: &GatewayConfig) -> (String, String) {
    let goal = GoalRunStore::new(&config.goals_dir)
        .unwrap()
        .list()
        .unwrap()
        .remove(0);
    let pkg = load_all_packages(config).unwrap().remove(0);
    (format!("{}", goal.state), format!("{:?}", pkg.status))
}

/// The expected final PLAN.md: the original with ONLY the target phase's marker and its two
/// own items changed.
fn expected_plan_after_apply() -> String {
    PLAN_V1
        .replacen("<!-- status: in_progress -->", "<!-- status: done -->", 1)
        .replacen("1. [ ] First item", "1. [x] First item", 1)
        .replacen("2. [ ] Second item", "2. [x] Second item", 1)
}

#[test]
fn dry_run_changes_nothing_and_a_real_apply_then_matches_the_preview() {
    let project = new_project();
    let config = GatewayConfig::for_project(project.path());
    let (pkg_id, goal) = build_draft(project.path(), &config, true);

    let tree_before = snapshot_tree(project.path());
    let vcs_before = vcs_state(project.path());
    let states_before = goal_and_draft_state(&config);
    assert!(goal.workspace_path.exists());

    apply(&config, &pkg_id, true, true).expect("dry run must succeed");

    assert_eq!(
        vcs_state(project.path()),
        vcs_before,
        "dry run changed HEAD, branches or the working tree status"
    );
    assert_eq!(
        goal_and_draft_state(&config),
        states_before,
        "dry run changed goal or draft state"
    );
    assert!(goal.workspace_path.exists(), "dry run removed staging");
    let tree_after = snapshot_tree(project.path());
    let changed: Vec<&String> = tree_before
        .keys()
        .chain(tree_after.keys())
        .filter(|k| tree_before.get(*k) != tree_after.get(*k))
        .collect();
    assert!(changed.is_empty(), "dry run changed files: {changed:?}");
    assert!(
        !project.path().join(".ta/apply.lock").exists(),
        "dry run must not leave a lock behind"
    );
    assert_eq!(
        git(project.path(), &["branch", "--list", "ta/*"]),
        "",
        "dry run created a feature branch"
    );

    // The same dry run, with --no-submit semantics, must be equally inert.
    apply(&config, &pkg_id, false, true).expect("dry run (no-submit) must succeed");
    assert_eq!(vcs_state(project.path()), vcs_before);
    assert_eq!(snapshot_tree(project.path()), tree_before);

    // And nothing was consumed: a real apply afterwards still works, on a feature branch.
    apply(&config, &pkg_id, true, false).expect("real apply after a dry run");
    let feature = git(
        project.path(),
        &["branch", "--list", "ta/*", "--format=%(refname:short)"],
    );
    assert!(
        !feature.is_empty(),
        "real apply should have created a ta/ branch"
    );
    let on_branch = git(project.path(), &["show", &format!("{feature}:PLAN.md")]);
    assert_eq!(
        on_branch.trim_end(),
        expected_plan_after_apply().trim_end(),
        "PLAN.md on the feature branch must differ from the original only in the target \
         phase's marker and its own items, even though the draft checked every box and \
         dropped every blank line"
    );
    assert_eq!(
        git(project.path(), &["show", &format!("{feature}:src.txt")]),
        "new"
    );
    // The protected branch itself was not written to.
    assert_eq!(
        git(project.path(), &["show", "HEAD:PLAN.md"]).trim_end(),
        PLAN_V1.trim_end()
    );
}

#[test]
fn apply_without_submit_on_a_protected_branch_never_writes_onto_it() {
    let project = new_project();
    let config = GatewayConfig::for_project(project.path());
    let (pkg_id, _goal) = build_draft(project.path(), &config, false);

    let protected = git(project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(
        protected == "main" || protected == "master",
        "started on {protected}"
    );
    let protected_tip = git(project.path(), &["rev-parse", &protected]);

    apply(&config, &pkg_id, false, false).expect("--no-submit apply");

    let now_on = git(project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_ne!(
        now_on, protected,
        "apply left the working tree on the protected branch"
    );
    assert!(
        now_on.starts_with("ta/"),
        "expected a ta/ feature branch, got {now_on}"
    );
    assert_eq!(
        git(project.path(), &["rev-parse", &protected]),
        protected_tip,
        "the protected branch moved"
    );
    assert_eq!(
        git(project.path(), &["show", &format!("{protected}:src.txt")]),
        "old",
        "the change leaked onto the protected branch"
    );
    // The change sits uncommitted on the feature branch.
    assert_eq!(
        std::fs::read_to_string(project.path().join("src.txt")).unwrap(),
        "new\n"
    );
    assert!(git(project.path(), &["status", "--porcelain"]).contains("src.txt"));
    assert_eq!(
        std::fs::read_to_string(project.path().join("PLAN.md"))
            .unwrap()
            .trim_end(),
        expected_plan_after_apply().trim_end()
    );
}

#[test]
fn apply_refuses_before_writing_when_the_feature_branch_cannot_be_created() {
    let project = new_project();
    let config = GatewayConfig::for_project(project.path());
    let (pkg_id, _goal) = build_draft(project.path(), &config, false);
    let protected = git(project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);

    // A stale index lock makes every branch switch fail, for real.
    let lock = project.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let before = snapshot_tree(project.path());

    let err = apply(&config, &pkg_id, false, false).expect_err("apply must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("could not create a feature branch"), "{msg}");
    assert!(
        msg.contains("no changes made"),
        "says nothing was written: {msg}"
    );
    assert!(msg.contains("protected branch"), "{msg}");
    assert!(
        msg.contains("ta draft apply"),
        "says what to do next: {msg}"
    );

    std::fs::remove_file(&lock).unwrap();
    assert_eq!(
        git(project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        protected
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("src.txt")).unwrap(),
        "old\n",
        "refused apply must not have written the change"
    );
    let after = snapshot_tree(project.path());
    let changed: Vec<&String> = before
        .keys()
        .filter(|k| before.get(*k) != after.get(*k) && !k.starts_with(".ta/"))
        .collect();
    assert!(
        changed.is_empty(),
        "refused apply changed files: {changed:?}"
    );
}

/// A minimal adapter: sits on a protected branch and fails to prepare.
struct StubAdapter {
    prepare_ok: bool,
    branch_after_prepare: &'static str,
    prepared: std::sync::atomic::AtomicBool,
}

impl ta_submit::SourceAdapter for StubAdapter {
    fn prepare(
        &self,
        _: &CommitContext,
        _: &ta_submit::SubmitConfig,
    ) -> ta_submit::adapter::Result<()> {
        if self.prepare_ok {
            self.prepared
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        } else {
            Err(ta_submit::adapter::SubmitError::InvalidState(
                "checkout refused".to_string(),
            ))
        }
    }
    fn commit(
        &self,
        _: &CommitContext,
        _: &DraftPackage,
        _: &str,
    ) -> ta_submit::adapter::Result<ta_submit::CommitResult> {
        unreachable!()
    }
    fn push(&self, _: &CommitContext) -> ta_submit::adapter::Result<ta_submit::PushResult> {
        unreachable!()
    }
    fn open_review(
        &self,
        _: &CommitContext,
        _: &DraftPackage,
    ) -> ta_submit::adapter::Result<ta_submit::ReviewResult> {
        unreachable!()
    }
    fn name(&self) -> &str {
        "stub"
    }
    fn current_branch(&self) -> ta_submit::adapter::Result<String> {
        if self.prepared.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(self.branch_after_prepare.to_string())
        } else {
            Ok("main".to_string())
        }
    }
    fn protected_submit_targets(&self) -> Vec<String> {
        vec!["main".to_string()]
    }
    fn verify_not_on_protected_target(&self) -> ta_submit::adapter::Result<()> {
        if self.current_branch()? == "main" {
            Err(ta_submit::adapter::SubmitError::InvalidState(
                "still on main".to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

fn stub_ctx() -> (CommitContext, ta_submit::SubmitConfig) {
    let project = new_project();
    let config = GatewayConfig::for_project(project.path());
    let (_id, goal) = build_draft(project.path(), &config, false);
    (
        CommitContext::from(&goal),
        ta_submit::SubmitConfig::default(),
    )
}

#[test]
fn preflight_refuses_with_an_actionable_message_when_prepare_fails() {
    let (ctx, cfg) = stub_ctx();
    let adapter = StubAdapter {
        prepare_ok: false,
        branch_after_prepare: "main",
        prepared: Default::default(),
    };
    let msg = ensure_off_protected_branch(&adapter, &ctx, &cfg, false, "abcd1234")
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("could not create a feature branch"),
        "what happened: {msg}"
    );
    assert!(msg.contains("'main'"), "which branch: {msg}");
    assert!(msg.contains("checkout refused"), "the VCS error: {msg}");
    assert!(msg.contains("ta draft apply abcd1234"), "next step: {msg}");
}

#[test]
fn preflight_refuses_when_prepare_leaves_the_tree_on_the_protected_branch() {
    let (ctx, cfg) = stub_ctx();
    let adapter = StubAdapter {
        prepare_ok: true,
        branch_after_prepare: "main",
        prepared: Default::default(),
    };
    let msg = ensure_off_protected_branch(&adapter, &ctx, &cfg, true, "abcd1234")
        .unwrap_err()
        .to_string();
    assert!(msg.contains("still on a protected branch"), "{msg}");
    assert!(msg.contains("ta draft apply abcd1234"), "{msg}");
}

#[test]
fn preflight_switches_off_the_protected_branch_for_no_submit() {
    let (ctx, cfg) = stub_ctx();
    let adapter = StubAdapter {
        prepare_ok: true,
        branch_after_prepare: "ta/feature",
        prepared: Default::default(),
    };
    let pre = ensure_off_protected_branch(&adapter, &ctx, &cfg, false, "abcd1234").unwrap();
    assert_eq!(pre.original_branch.as_deref(), Some("main"));
    assert_eq!(pre.working_branch.as_deref(), Some("ta/feature"));
}

#[test]
fn preflight_is_a_no_op_for_the_none_adapter() {
    struct NoVcs;
    impl ta_submit::SourceAdapter for NoVcs {
        fn prepare(
            &self,
            _: &CommitContext,
            _: &ta_submit::SubmitConfig,
        ) -> ta_submit::adapter::Result<()> {
            unreachable!("none adapter must not prepare")
        }
        fn commit(
            &self,
            _: &CommitContext,
            _: &DraftPackage,
            _: &str,
        ) -> ta_submit::adapter::Result<ta_submit::CommitResult> {
            unreachable!()
        }
        fn push(&self, _: &CommitContext) -> ta_submit::adapter::Result<ta_submit::PushResult> {
            unreachable!()
        }
        fn open_review(
            &self,
            _: &CommitContext,
            _: &DraftPackage,
        ) -> ta_submit::adapter::Result<ta_submit::ReviewResult> {
            unreachable!()
        }
        fn name(&self) -> &str {
            "none"
        }
    }
    let (ctx, cfg) = stub_ctx();
    assert_eq!(
        ensure_off_protected_branch(&NoVcs, &ctx, &cfg, false, "x").unwrap(),
        BranchPreflight::default()
    );
}

/// Source scan (CRLF-normalized): the apply paths must not call the whole-document
/// auto-correct or the horizontal-rule normalizer on PLAN.md any more.
#[test]
fn apply_source_no_longer_rewrites_plan_md_wholesale() {
    let src = include_str!("../draft.rs").replace("\r\n", "\n");
    let apply_start = src.find("\nfn apply_package(").expect("apply_package");
    let apply_end = src[apply_start..]
        .find("\nfn watch_package")
        .map(|i| apply_start + i)
        .unwrap_or(src.len());
    let body = &src[apply_start..apply_end];
    for forbidden in [
        "auto_correct_done_phase_items(",
        "normalize_plan_horizontal_rules(",
        "auto_check_covered_items(",
    ] {
        assert!(
            !body.contains(forbidden),
            "apply_package must not call {forbidden} (it rewrites PLAN.md outside the target phase)"
        );
    }
}

/// Goals must build in their own target directory, never the source tree's `target/`
/// (artifacts compiled in staging bake the staging path into test binaries).
#[test]
fn goal_builds_use_their_own_target_directory() {
    let src = include_str!("../run.rs").replace("\r\n", "\n");
    assert!(
        !src.contains("src_dir.join(\"target\")"),
        "run.rs must not point CARGO_TARGET_DIR at the source project's target/"
    );
    assert!(
        src.contains("super::verify::goal_target_dir(&staging_path)"),
        "run.rs must give each goal its own CARGO_TARGET_DIR inside staging"
    );
    let staging = std::path::Path::new("/tmp/.ta/staging/abc");
    assert_eq!(
        super::super::verify::goal_target_dir(staging),
        staging.join("target")
    );
}
