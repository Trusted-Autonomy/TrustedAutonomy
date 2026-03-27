//! Integration test: GitAdapter::open_review() uses workflow.toml config.
//!
//! Verifies that:
//! 1. `target_branch` from SubmitConfig (loaded from workflow.toml) is passed as
//!    `--base` to `gh pr create`, not a hardcoded default.
//! 2. The head branch (derived from goal title + branch_prefix) is passed as `--head`.
//! 3. Idempotency: when an open PR already exists for the head branch, a second
//!    `open_review()` call returns the existing PR URL without calling `gh pr create`.
//!
//! Uses a `gh` stub shell script placed at the front of PATH to intercept CLI
//! calls without requiring a live GitHub account or network access.
//!
//! Unix-only: the stub is a shell script that requires Unix executable permissions.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

use ta_changeset::draft_package::{
    AgentIdentity, Artifact, ChangeType, Changes, Goal, Iteration, Plan, Provenance,
    RequestedAction, ReviewRequests, Risk, Signatures, Summary, WorkspaceRef,
};
use ta_changeset::{DraftPackage, DraftStatus};
use ta_goal::GoalRun;
use ta_submit::{GitAdapter, GitConfig, SourceAdapter, SubmitConfig};
use tempfile::tempdir;

/// Serialize tests that manipulate the process PATH so they don't interfere
/// with each other when run in parallel.
static PATH_MUTEX: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Initialize a minimal git repository in `dir` with an initial commit.
fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .expect("git command failed");
    };
    run(&["init"]);
    run(&["config", "user.name", "Test User"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(dir.join("README.md"), "# test\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "initial"]);
}

/// Build a minimal `GoalRun` suitable for `open_review()` tests.
fn make_goal(title: &str, dir: &Path) -> GoalRun {
    GoalRun::new(
        title,
        "Test objective",
        "test-agent",
        dir.to_path_buf(),
        dir.join("store"),
    )
}

/// Build a minimal `DraftPackage` sufficient for `open_review()` / `build_pr_body()`.
fn make_draft_package() -> DraftPackage {
    DraftPackage {
        package_version: "1.0.0".to_string(),
        package_id: uuid::Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        goal: Goal {
            goal_id: "goal-test".to_string(),
            title: "Test Goal".to_string(),
            objective: "Test the system".to_string(),
            success_criteria: vec![],
            constraints: vec![],
            parent_goal_title: None,
        },
        iteration: Iteration {
            iteration_id: "iter-1".to_string(),
            sequence: 1,
            workspace_ref: WorkspaceRef {
                ref_type: "staging_dir".to_string(),
                ref_name: "staging/goal-test/1".to_string(),
                base_ref: None,
            },
        },
        agent_identity: AgentIdentity {
            agent_id: "agent-1".to_string(),
            agent_type: "coder".to_string(),
            constitution_id: "default".to_string(),
            capability_manifest_hash: "abc123".to_string(),
            orchestrator_run_id: None,
        },
        summary: Summary {
            what_changed: "Added test file".to_string(),
            why: "Integration test".to_string(),
            impact: "Test only".to_string(),
            rollback_plan: "Delete test file".to_string(),
            open_questions: vec![],
            alternatives_considered: vec![],
        },
        plan: Plan {
            completed_steps: vec![],
            next_steps: vec![],
            decision_log: vec![],
        },
        changes: Changes {
            artifacts: vec![Artifact {
                resource_uri: "fs://workspace/test.txt".to_string(),
                change_type: ChangeType::Add,
                diff_ref: "diff-001".to_string(),
                tests_run: vec![],
                disposition: Default::default(),
                rationale: None,
                dependencies: vec![],
                explanation_tiers: None,
                comments: None,
                amendment: None,
            }],
            patch_sets: vec![],
            pending_actions: vec![],
        },
        risk: Risk {
            risk_score: 5,
            findings: vec![],
            policy_decisions: vec![],
        },
        provenance: Provenance {
            inputs: vec![],
            tool_trace_hash: "trace-hash".to_string(),
        },
        review_requests: ReviewRequests {
            requested_actions: vec![RequestedAction {
                action: "merge".to_string(),
                targets: vec!["fs://workspace/test.txt".to_string()],
            }],
            reviewers: vec!["reviewer".to_string()],
            required_approvals: 1,
            notes_to_reviewer: None,
        },
        signatures: Signatures {
            package_hash: "pkg-hash".to_string(),
            agent_signature: "sig".to_string(),
            gateway_attestation: None,
        },
        status: DraftStatus::Draft,
        verification_warnings: vec![],
        validation_log: vec![],
        display_id: None,
        tag: None,
        vcs_status: None,
        parent_draft_id: None,
        pending_approvals: vec![],
        supervisor_review: None,
        ignored_artifacts: vec![],
        baseline_artifacts: vec![],
        agent_decision_log: vec![],
        goal_shortref: None,
        draft_seq: 0,
    }
}

/// Write a `gh` stub script to `stub_dir/gh`.
///
/// The stub:
/// - `gh --version` → exits 0 (signals gh is available)
/// - `gh pr list ...` → outputs the JSON in `pr_list_response_file`
/// - `gh pr create ...` → appends space-separated args to `capture_file`,
///   then outputs the mock PR URL `https://github.com/test/repo/pull/42`
/// - `gh pr merge ...` → exits 0 silently
fn write_gh_stub(
    stub_dir: &Path,
    capture_file: &Path,
    pr_list_response_file: &Path,
) -> std::path::PathBuf {
    let stub_path = stub_dir.join("gh");
    let capture = capture_file.to_str().unwrap();
    let pr_list_file = pr_list_response_file.to_str().unwrap();

    // Use absolute file paths in the script to avoid any cwd dependency.
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
           --version) echo 'gh version 2.0.0 (test-stub)'; exit 0 ;;\n\
           pr)\n\
             case \"$2\" in\n\
               list)  cat '{pr_list_file}'; exit 0 ;;\n\
               create)\n\
                 printf '%s\\n' \"$*\" >> '{capture}'\n\
                 echo 'https://github.com/test/repo/pull/42'\n\
                 exit 0 ;;\n\
               merge) exit 0 ;;\n\
               *) exit 1 ;;\n\
             esac ;;\n\
           *) exit 1 ;;\n\
         esac\n",
        pr_list_file = pr_list_file,
        capture = capture,
    );

    {
        let mut f = std::fs::File::create(&stub_path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub_path, perms).unwrap();
    // Force overlayfs copy-up to complete before exec.
    let _ = std::fs::read(&stub_path).unwrap();
    stub_path
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `open_review()` must use `self.config.git.target_branch` (from workflow.toml)
/// as the `--base` argument and the derived head branch as `--head`.
///
/// Before PR #279 this bug existed: `open_review` called `SubmitConfig::default()`
/// (adapter="none", target_branch="main") instead of `self.config`, silently
/// ignoring `target_branch = "staging"` set in workflow.toml.
#[test]
fn test_open_review_uses_workflow_config() {
    let _guard = PATH_MUTEX.lock().unwrap();

    let repo_dir = tempdir().unwrap();
    let stub_dir = tempdir().unwrap();
    let pr_list_file = tempdir().unwrap();
    let capture_file = tempdir().unwrap();

    let pr_list_path = pr_list_file.path().join("pr_list.json");
    let capture_path = capture_file.path().join("capture.txt");

    init_git_repo(repo_dir.path());

    // gh pr list returns an empty array → no existing PR, so gh pr create will be called.
    std::fs::write(&pr_list_path, "[]").unwrap();

    write_gh_stub(stub_dir.path(), &capture_path, &pr_list_path);

    // Build a SubmitConfig with target_branch = "staging".
    let config = SubmitConfig {
        git: GitConfig {
            target_branch: "staging".to_string(),
            branch_prefix: "ta/".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };

    let adapter = GitAdapter::with_config(repo_dir.path(), config);
    let goal = make_goal("my feature", repo_dir.path());
    let pkg = make_draft_package();

    // Prepend stub_dir to PATH so our `gh` stub is found first.
    let original_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", stub_dir.path().display(), original_path),
    );

    let result = adapter.open_review(&goal, &pkg);

    // Restore PATH before any assertions so a test panic doesn't leave PATH broken.
    std::env::set_var("PATH", &original_path);

    let review = result.expect("open_review should succeed");
    assert_eq!(
        review.review_url, "https://github.com/test/repo/pull/42",
        "returned URL should be the stub mock URL"
    );

    // Read captured args to verify --base staging and --head ta/my-feature.
    let captured = std::fs::read_to_string(&capture_path)
        .expect("capture file should exist after gh pr create was called");

    assert!(
        captured.contains("--base staging"),
        "gh pr create must include --base staging (from workflow.toml target_branch); \
         got: {captured}"
    );
    // v0.14.7.3: branch name now includes goal shortref prefix: ta/<shortref>-my-feature
    // The shortref is dynamic (first 8 chars of a random UUID), so check for the stable parts.
    assert!(
        captured.contains("--head ta/") && captured.contains("-my-feature"),
        "gh pr create must include --head ta/<shortref>-my-feature (shortref + goal title slug + prefix); \
         got: {captured}"
    );
}

/// Second `open_review()` call for the same branch returns the existing PR URL
/// without calling `gh pr create` (idempotency).
///
/// This covers the case where `ta draft apply --submit` runs twice (e.g., daemon
/// restart between push and PR creation): the second run must not fail with
/// "a pull request for branch ... already exists".
#[test]
fn test_open_review_idempotency_returns_existing_pr() {
    let _guard = PATH_MUTEX.lock().unwrap();

    let repo_dir = tempdir().unwrap();
    let stub_dir = tempdir().unwrap();
    let pr_list_file = tempdir().unwrap();
    let capture_file = tempdir().unwrap();

    let pr_list_path = pr_list_file.path().join("pr_list.json");
    let capture_path = capture_file.path().join("capture.txt");

    init_git_repo(repo_dir.path());

    // gh pr list returns an existing open PR — idempotency path.
    std::fs::write(
        &pr_list_path,
        r#"[{"url":"https://github.com/test/repo/pull/99","number":99}]"#,
    )
    .unwrap();

    write_gh_stub(stub_dir.path(), &capture_path, &pr_list_path);

    let config = SubmitConfig {
        git: GitConfig {
            target_branch: "main".to_string(),
            branch_prefix: "ta/".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };

    let adapter = GitAdapter::with_config(repo_dir.path(), config);
    let goal = make_goal("idempotency test", repo_dir.path());
    let pkg = make_draft_package();

    let original_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", stub_dir.path().display(), original_path),
    );

    let result = adapter.open_review(&goal, &pkg);

    std::env::set_var("PATH", &original_path);

    let review = result.expect("open_review should succeed on second call (idempotent)");

    // Should return the existing PR, not a newly created one.
    assert_eq!(
        review.review_url, "https://github.com/test/repo/pull/99",
        "idempotent call should return the existing PR URL"
    );
    assert!(
        review.message.contains("already open"),
        "message should indicate the PR was reused, not newly created; got: {}",
        review.message
    );

    // gh pr create must NOT have been called (capture file should be absent or empty).
    let create_was_called = capture_path
        .exists()
        .then(|| std::fs::read_to_string(&capture_path).unwrap_or_default())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    assert!(
        !create_was_called,
        "gh pr create must not be called when an existing open PR is found"
    );
}
