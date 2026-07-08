// verb_alias_cli.rs — Black-box proof of the v0.17.0.12.16 CLI Verb-Set
// Consolidation: the new primary `ta <verb> <noun>` surface and the legacy
// `ta <noun> <action>` surface produce identical stdout for a read-only
// command, and the deprecation notice appears exactly once (on stderr) for
// the legacy form only — never for the new form.
//
// Runs the actual compiled `ta` binary as a subprocess against an empty
// temp project, rather than calling internal functions directly, so this
// test can't be fooled by a refactor that keeps the internal plumbing
// correct but breaks the real CLI surface.

use std::process::Command;
use std::sync::Mutex;

use tempfile::TempDir;

// Each test spawns the real `ta` binary as a subprocess. Observed flaky on
// Windows CI when all 4 tests' subprocesses run concurrently (cargo test's
// default per-test-thread parallelism) -- the same exact invocation
// (`ta goal list`) succeeded in one test's run and crashed with a stack
// overflow in another's, pointing at resource contention on a constrained
// CI runner rather than a deterministic bug in any single invocation.
// Serialize this file's tests so at most one real subprocess runs at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn ta_cmd(project_root: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ta"))
        .arg("--project-root")
        .arg(project_root)
        .arg("--no-version-check")
        .args(args)
        .output()
        .expect("failed to run ta binary")
}

#[test]
fn new_and_legacy_goal_list_produce_identical_stdout() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let project = TempDir::new().unwrap();

    let legacy = ta_cmd(project.path(), &["goal", "list"]);
    let via_verb = ta_cmd(project.path(), &["list", "goal"]);

    assert_eq!(
        String::from_utf8_lossy(&legacy.stdout),
        String::from_utf8_lossy(&via_verb.stdout),
        "legacy `ta goal list` and new `ta list goal` must produce identical stdout"
    );
    assert_eq!(legacy.status.code(), via_verb.status.code());
}

#[test]
fn legacy_invocation_prints_deprecation_notice_exactly_once() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let project = TempDir::new().unwrap();

    let legacy = ta_cmd(project.path(), &["goal", "list"]);
    let stderr = String::from_utf8_lossy(&legacy.stderr);

    let notice_count = stderr.matches("[deprecated-cli]").count();
    assert_eq!(
        notice_count, 1,
        "expected exactly one deprecation notice, got {notice_count} in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("ta list goal"),
        "notice should name the new form:\n{stderr}"
    );
}

#[test]
fn new_verb_form_never_prints_a_deprecation_notice() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let project = TempDir::new().unwrap();

    let via_verb = ta_cmd(project.path(), &["list", "goal"]);
    let stderr = String::from_utf8_lossy(&via_verb.stderr);

    assert!(
        !stderr.contains("[deprecated-cli]"),
        "new verb+noun form must never print a deprecation notice, got:\n{stderr}"
    );
}

#[test]
fn unmapped_new_verb_noun_pair_is_a_clean_error() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let project = TempDir::new().unwrap();

    // "team" has no mapped "create" verb (see commands::verb::NOUN_TABLE) —
    // must be a clear, actionable error, not a panic/crash.
    let out = ta_cmd(project.path(), &["create", "team"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("isn't mapped"),
        "expected a clear 'not mapped yet' error, got:\n{stderr}"
    );
}
