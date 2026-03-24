// overlay_flow.rs — End-to-end integration test for the transparent overlay flow.
//
// This test proves the Phase 3 architecture: TA is invisible to the agent.
// The agent works on a staging copy using its native tools, and TA diffs
// the result to create a PR package for review.
//
// Flow:
//   1. Create source project (simulates real codebase)
//   2. ta goal start → creates overlay workspace (full copy)
//   3. Agent modifies/creates/deletes files in staging (native FS ops)
//   4. ta pr build → diffs staging vs source → PR package
//   5. ta pr approve → marks package approved
//   6. ta pr apply → copies changes back to source + git commit
//
// This proves: agent writes normally → TA captures as PR → human reviews → apply

use std::fs;

use ta_changeset::pr_package::ChangeType;
use ta_goal::{GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{ExcludePatterns, OverlayWorkspace};
use tempfile::TempDir;

/// Full overlay flow integration test — from goal start to applied changes.
#[test]
fn overlay_flow_goal_to_apply() {
    // =========================================================
    // 1. Create source project (simulates real codebase)
    // =========================================================

    let project = TempDir::new().unwrap();

    // Initialize git repo for the project.
    // Clear TA agent VCS isolation env vars (set by v0.13.17.3) so git
    // operates on the test's temp directory, not the staging repo.
    let git = |args: &[&str], dir: &std::path::Path| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .unwrap()
    };
    git(&["init"], project.path());
    git(&["config", "user.email", "test@test.com"], project.path());
    git(&["config", "user.name", "Test"], project.path());

    // Create project files.
    fs::write(
        project.path().join("README.md"),
        "# My Project\n\nA test project.\n",
    )
    .unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    )
    .unwrap();

    // Create .ta/ directory to verify it's excluded from overlay.
    fs::create_dir_all(project.path().join(".ta/goals")).unwrap();
    fs::write(project.path().join(".ta/config.toml"), "internal").unwrap();

    // Initial git commit.
    git(&["add", "-A"], project.path());
    git(&["commit", "-m", "initial commit"], project.path());

    let config = GatewayConfig::for_project(project.path());

    // =========================================================
    // 2. ta goal start → creates overlay workspace
    // =========================================================

    let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

    // Create goal with overlay.
    let source_dir = project.path().canonicalize().unwrap();
    let mut goal = ta_goal::GoalRun::new(
        "Fix greeting",
        "Update greeting to say hi instead of hello",
        "claude-code",
        std::path::PathBuf::new(),
        config.store_dir.join("placeholder"),
    );
    let goal_id = goal.goal_run_id.to_string();

    let excludes = ExcludePatterns::load(&source_dir);
    let overlay =
        OverlayWorkspace::create(&goal_id, &source_dir, &config.staging_dir, excludes).unwrap();

    goal.workspace_path = overlay.staging_dir().to_path_buf();
    goal.store_path = config.store_dir.join(&goal_id);
    goal.source_dir = Some(source_dir.clone());
    goal.transition(GoalRunState::Configured).unwrap();
    goal.transition(GoalRunState::Running).unwrap();
    goal_store.save(&goal).unwrap();

    // Verify: staging has all project files.
    assert!(overlay.staging_dir().join("README.md").exists());
    assert!(overlay.staging_dir().join("src/main.rs").exists());
    assert!(overlay.staging_dir().join("src/lib.rs").exists());
    assert!(overlay.staging_dir().join(".git").exists()); // .git is copied
    assert!(!overlay.staging_dir().join(".ta").exists()); // .ta is excluded

    // =========================================================
    // 3. Agent modifies/creates/deletes files in staging
    // =========================================================

    // Simulate agent work — using native fs ops (just like Claude Code would).

    // Modify: update the greeting.
    fs::write(
        overlay.staging_dir().join("src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"hi there!\"\n}\n",
    )
    .unwrap();

    // Modify: update main to use new greeting.
    fs::write(
        overlay.staging_dir().join("src/main.rs"),
        "use my_project::greet;\n\nfn main() {\n    println!(\"{}\", greet());\n}\n",
    )
    .unwrap();

    // Create: add a test file.
    fs::create_dir_all(overlay.staging_dir().join("tests")).unwrap();
    fs::write(
        overlay.staging_dir().join("tests/greeting_test.rs"),
        "#[test]\nfn test_greet() {\n    assert_eq!(my_project::greet(), \"hi there!\");\n}\n",
    )
    .unwrap();

    // Delete: remove old README (agent replaced it).
    fs::remove_file(overlay.staging_dir().join("README.md")).unwrap();

    // Create: add new README.
    fs::write(
        overlay.staging_dir().join("README.md"),
        "# My Project\n\nA project that says hi.\n\n## Usage\n\n```\ncargo run\n```\n",
    )
    .unwrap();

    // =========================================================
    // 4. Verify diff detection
    // =========================================================

    let changes = overlay.diff_all().unwrap();
    // Expected: README.md modified, src/lib.rs modified, src/main.rs modified,
    // tests/greeting_test.rs created.
    // (README.md was deleted and recreated, so it shows as modified since it exists in both)
    assert!(
        changes.len() >= 3,
        "expected at least 3 changes, got {}",
        changes.len()
    );

    let change_paths: Vec<String> = changes
        .iter()
        .map(|c| match c {
            ta_workspace::overlay::OverlayChange::Modified { path, .. }
            | ta_workspace::overlay::OverlayChange::Created { path, .. }
            | ta_workspace::overlay::OverlayChange::Deleted { path } => path.clone(),
        })
        .collect();

    assert!(change_paths.contains(&"src/lib.rs".to_string()));
    assert!(change_paths.contains(&"src/main.rs".to_string()));
    assert!(change_paths.contains(&"tests/greeting_test.rs".to_string()));

    // =========================================================
    // 5. ta pr build → creates PR package from diff
    // =========================================================

    // Build PR package using the same logic as `ta pr build`.
    let mut artifacts = Vec::new();
    for change in &changes {
        match change {
            ta_workspace::overlay::OverlayChange::Modified { path, .. } => {
                artifacts.push((path.clone(), ChangeType::Modify));
            }
            ta_workspace::overlay::OverlayChange::Created { path, .. } => {
                artifacts.push((path.clone(), ChangeType::Add));
            }
            ta_workspace::overlay::OverlayChange::Deleted { path } => {
                artifacts.push((path.clone(), ChangeType::Delete));
            }
        }
    }

    // Verify artifact types.
    let modify_count = artifacts
        .iter()
        .filter(|(_, t)| *t == ChangeType::Modify)
        .count();
    let add_count = artifacts
        .iter()
        .filter(|(_, t)| *t == ChangeType::Add)
        .count();

    assert!(modify_count >= 2, "should have at least 2 modified files");
    assert!(add_count >= 1, "should have at least 1 created file");

    // =========================================================
    // 6. ta pr apply → copies changes back to source
    // =========================================================

    let applied = overlay
        .apply_to(&source_dir)
        .map_err(|e| format!("{}", e))
        .unwrap();

    assert!(applied.len() >= 3, "should apply at least 3 files");

    // Verify source files were updated.
    let lib_content = fs::read_to_string(source_dir.join("src/lib.rs")).unwrap();
    assert!(lib_content.contains("hi there!"));

    let main_content = fs::read_to_string(source_dir.join("src/main.rs")).unwrap();
    assert!(main_content.contains("greet()"));

    let test_file = fs::read_to_string(source_dir.join("tests/greeting_test.rs")).unwrap();
    assert!(test_file.contains("test_greet"));

    // =========================================================
    // 7. Git commit (simulates --git-commit)
    // =========================================================

    let add_result = git(&["add", "-A"], &source_dir);
    assert!(add_result.status.success());

    let commit_result = git(
        &[
            "commit",
            "-m",
            "Fix greeting\n\nApplied via Trusted Autonomy",
        ],
        &source_dir,
    );
    assert!(
        commit_result.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit_result.stderr)
    );

    // Verify git log.
    let log = git(&["log", "--oneline"], &source_dir);
    let log_output = String::from_utf8_lossy(&log.stdout);
    assert!(log_output.contains("Fix greeting"));
    assert!(log_output.contains("initial commit"));

    // =========================================================
    // SUCCESS: The transparent overlay flow works end-to-end!
    // =========================================================
    //
    // We demonstrated:
    // - TA creates an overlay workspace (full copy, .ta excluded)
    // - Agent works on the copy using native FS tools (invisible mediation)
    // - TA diffs the staging copy against source to find changes
    // - Changes can be applied back to source with git commit
    // - The agent never knew it was working in a staged environment
}
