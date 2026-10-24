// no_external_agents.rs — Proof that a test run cannot reach a paid external
// agent.
//
// Background: `cargo test -p ta-cli` once ran `ta draft apply` in-process
// dozens of times; each run shelled out to the installed `meridian`
// (`meridian summarize-title`), which spawns a real paid `claude --print`.
// The guard is `TA_TEST_NO_EXTERNAL_AGENTS` (see `ta_goal::external_agents`),
// set for every cargo run by `.cargo/config.toml` `[env]` and inherited by the
// child processes tests spawn.
//
// These tests use the real compiled `ta` binary (which is NOT built with
// `cfg(test)`, so only the environment guard protects it) and a runnable
// stand-in `meridian` that records any execution.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::TempDir;

/// If this fails, `.cargo/config.toml` no longer exports the guard and every
/// test that spawns a child `ta` (or any other non-`cfg(test)` code path) can
/// again reach a paid agent.
#[test]
fn cargo_test_environment_exports_the_external_agent_guard() {
    let value = std::env::var("TA_TEST_NO_EXTERNAL_AGENTS").unwrap_or_default();
    assert!(
        !value.is_empty() && value != "0",
        "TA_TEST_NO_EXTERNAL_AGENTS must be on during `cargo test` (set by .cargo/config.toml \
         [env]); got {value:?}. Do not run tests with it set to 0: they would reach paid agents."
    );
}

/// A project that explicitly configures a runnable `meridian` must still not
/// have it executed by the real `ta` binary while the guard is on.
#[test]
fn ta_binary_refuses_to_execute_a_configured_meridian_under_the_guard() {
    let project = TempDir::new().unwrap();
    let marker = project.path().join("meridian-was-invoked");
    let stub = project.path().join("meridian-stub");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(project.path().join(".ta")).unwrap();
    std::fs::write(
        project.path().join(".ta/daemon.toml"),
        format!("[meridian]\nbinary = \"{}\"\n", stub.display()),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ta"))
        .arg("--project-root")
        .arg(project.path())
        .arg("--no-version-check")
        .args(["meridian", "tools"])
        .output()
        .expect("failed to run ta binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !marker.exists(),
        "the real `ta` binary executed meridian during a test run.\nstderr:\n{stderr}"
    );
    assert!(
        !out.status.success(),
        "`ta meridian tools` must fail while external agents are blocked"
    );
    assert!(
        stderr.contains("TA_TEST_NO_EXTERNAL_AGENTS"),
        "the refusal must name the guard so the operator knows what to do.\nstderr:\n{stderr}"
    );
}
