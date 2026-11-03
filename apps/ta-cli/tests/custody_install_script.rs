//! Runs `scripts/test-custody-decision.sh`, the shell test for the dev
//! installer's key-custody decision (no Team ID -> write `file`; never
//! overwrite an existing choice). The script uses a fake `codesign` on PATH and
//! a tempdir HOME: it never touches the Keychain, `ta`, or the real home.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn install_local_custody_decision_script_passes() {
    let root: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let out = Command::new("bash")
        .arg(root.join("scripts/test-custody-decision.sh"))
        .env("TA_TEST_NO_EXTERNAL_AGENTS", "1")
        .output()
        .expect("bash must be available to run scripts/test-custody-decision.sh");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "scripts/test-custody-decision.sh failed (re-run it by hand: bash scripts/test-custody-decision.sh):\n{text}"
    );
    assert!(text.contains("ok   - no Team ID writes 'file'"), "{text}");
}
