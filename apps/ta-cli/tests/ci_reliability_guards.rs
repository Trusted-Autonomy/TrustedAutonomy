//! Guards for the CI reliability setup: the leftover-process check on every
//! required job, the standalone plugin test step, and step timeouts.
//!
//! These scan workflow YAML and scripts as text. CRLF is normalized first so
//! the checks hold on Windows checkouts with `core.autocrlf`.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    std::fs::read_to_string(root.join(rel))
        .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
        .replace("\r\n", "\n")
}

#[test]
fn every_required_job_checks_for_leftover_processes_even_after_failure() {
    let ci = read(".github/workflows/ci.yml");
    let step = "Check for leftover gh, ta-daemon and ta processes";
    assert_eq!(
        ci.matches(step).count(),
        2,
        "the unix matrix job and the Windows job each need the leftover-process step"
    );
    for part in ci.split(&format!("- name: {step}\n")).skip(1) {
        let head: String = part.lines().take(6).collect::<Vec<_>>().join("\n");
        assert!(
            head.contains("if: always() && env.CODE == 'true'"),
            "leftover step must run on failure and keep the docs-only fast path:\n{head}"
        );
        assert!(head.contains("timeout-minutes"), "step needs a timeout");
    }
    assert!(ci.contains("scripts/ci-leftover-processes.sh"));
    assert!(ci.contains("scripts/ci-leftover-processes.ps1"));
}

#[test]
fn standalone_plugins_are_tested_with_their_own_cache_key() {
    let ci = read(".github/workflows/ci.yml");
    for plugin in ["discord", "slack", "email"] {
        assert!(
            ci.contains(&format!("plugins/ta-channel-{plugin}")),
            "ta-channel-{plugin} is not built or tested by CI"
        );
    }
    assert!(ci.contains("cargo test --manifest-path plugins/$plugin/Cargo.toml"));
    assert!(ci.contains("key: standalone-plugins"));
    assert!(
        ci.contains("matrix.os == 'ubuntu-latest'"),
        "plugin tests run on the ubuntu leg only"
    );
}

#[test]
fn every_job_and_step_with_a_command_is_time_bounded() {
    let ci = read(".github/workflows/ci.yml");
    assert_eq!(
        ci.matches("\n    timeout-minutes:").count(),
        4,
        "each of the four jobs needs a job-level timeout"
    );
    // Every `run:`/`uses:` step carries timeout-minutes within its block.
    let mut blocks: Vec<&str> = ci.split("\n      - ").skip(1).collect();
    blocks.retain(|b| b.contains("run:") || b.contains("uses:"));
    for block in blocks {
        assert!(
            block.contains("timeout-minutes:"),
            "step without timeout-minutes:\n{block}"
        );
    }
}

#[test]
fn leftover_scripts_enforce_the_documented_limits() {
    let sh = read("scripts/ci-leftover-processes.sh");
    assert!(sh.contains("MAX_GH=\"${MAX_GH:-5}\""));
    assert!(sh.contains("ta-daemon"));
    let ps = read("scripts/ci-leftover-processes.ps1");
    assert!(ps.contains("[int]$MaxGh = 5"));
    assert!(ps.contains("ta-daemon"));
}

#[cfg(unix)]
#[test]
fn leftover_shell_script_self_test_passes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let out = std::process::Command::new("bash")
        .arg(root.join("scripts/ci-leftover-processes.sh"))
        .arg("--self-test")
        .output()
        .expect("run ci-leftover-processes.sh --self-test");
    assert!(
        out.status.success(),
        "self-test failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
