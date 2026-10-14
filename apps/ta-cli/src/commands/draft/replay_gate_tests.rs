// CR-07 tests: external actions are never replayed without a human, and `--chain
// --dry-run` changes nothing. Fixtures live in `tempfile::tempdir()`.

use tempfile::TempDir;

use super::*;

#[test]
fn replay_decision_table() {
    // (explicit_on, explicit_off, env_off, interactive) -> replay
    let cases = [
        (false, false, false, true, true), // a terminal replays by default
        (false, false, false, false, false), // non-TTY does not replay without the flag
        (true, false, false, false, true), // explicit flag replays without a TTY
        (true, false, true, true, false),  // env override beats the flag
        (false, true, false, true, false), // --no-replay-actions beats a terminal
        (true, true, false, true, false),  // off wins if both are somehow set
        (false, false, true, true, false), // env disables even on a terminal
    ];
    for (on, off, env, tty, want) in cases {
        let d = ReplayDecision::decide(on, off, env, tty);
        assert_eq!(
            d.replay, want,
            "on={on} off={off} env_off={env} interactive={tty}: {}",
            d.reason
        );
    }
}

#[test]
fn chain_dry_run_changes_nothing_and_replays_nothing() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
    let config = GatewayConfig::for_project(project.path());
    super::super::goal::execute(
        &super::super::goal::GoalCommands::Start {
            title: "Chain dry run".to_string(),
            source: Some(project.path().to_path_buf()),
            objective: "test".to_string(),
            agent: "test-agent".to_string(),
            phase: None,
            follow_up: None,
            objective_file: None,
        },
        &config,
    )
    .unwrap();
    let goal = GoalRunStore::new(&config.goals_dir)
        .unwrap()
        .list()
        .unwrap()
        .remove(0);
    std::fs::write(goal.workspace_path.join("README.md"), "# Changed\n").unwrap();
    build_package(
        &config,
        &goal.goal_run_id.to_string(),
        "Chain dry run",
        false,
    )
    .unwrap();
    let id = load_all_packages(&config).unwrap()[0]
        .package_id
        .to_string();
    approve_package(&config, &id, "tester", false).unwrap();

    let replay = ReplayDecision::decide(true, false, false, true);
    apply_chain(&config, &id, None, true, true, replay).unwrap();

    assert_eq!(
        std::fs::read_to_string(project.path().join("README.md")).unwrap(),
        "# Original\n",
        "--chain --dry-run must not copy files into the project"
    );
    let pkg = load_package(&config, uuid::Uuid::parse_str(&id).unwrap()).unwrap();
    assert!(
        !matches!(pkg.status, DraftStatus::Applied { .. }),
        "--chain --dry-run must not mark the draft applied"
    );
    assert!(
        !project
            .path()
            .join(".ta")
            .join(super::super::draft_task_replay::LEDGER_FILE)
            .exists(),
        "--chain --dry-run must not replay external actions"
    );
}

/// Every place the CLI or daemon spawns `ta draft apply` must say how replay is handled,
/// so a new automated spawn cannot silently skip the gate. Automated callers pass
/// `--no-replay-actions`; the daemon's web approval is a human and passes `--replay-actions`;
/// `publish.rs` runs in the user's own terminal.
#[test]
fn every_draft_apply_spawn_site_declares_replay_handling() {
    // Built with concat! so this test's own source never matches the scan.
    let needle = concat!("\"draft\",", "\"apply\"");
    let needle_chain = concat!(".arg(\"draft\")", ".arg(\"apply\")");
    let interactive_allowlist = ["commands/publish.rs"];

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        manifest.join("src"),
        manifest.join("../../crates/ta-daemon/src"),
    ];
    let mut sites = 0;
    let mut offenders = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = roots.to_vec();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // Production code only: drop everything from the first test module on.
            let prod = text.split("#[cfg(test)]").next().unwrap();
            let squashed: String = prod.chars().filter(|c| !c.is_whitespace()).collect();
            for n in [needle, needle_chain] {
                for (at, _) in squashed.match_indices(n) {
                    sites += 1;
                    let window = &squashed[at..(at + 400).min(squashed.len())];
                    let declared = window.contains("no-replay-actions")
                        || window.contains("\"--replay-actions\"");
                    let rel = path.to_string_lossy().replace('\\', "/");
                    if !declared && !interactive_allowlist.iter().any(|a| rel.ends_with(a)) {
                        offenders.push(rel);
                    }
                }
            }
        }
    }
    assert!(
        sites >= 6,
        "spawn-site scan found only {sites} sites; is it broken?"
    );
    assert!(
        offenders.is_empty(),
        "these `ta draft apply` spawn sites do not pass --no-replay-actions or --replay-actions: {offenders:?}"
    );
}
