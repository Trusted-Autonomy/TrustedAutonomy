//! Guards for the automatic version sync workflows (nightly stamp, sync PR job).
//!
//! These scan workflow YAML as text. CRLF is normalized first so the checks
//! hold on Windows checkouts with `core.autocrlf`.

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
fn required_ci_check_names_are_unchanged() {
    let ci = read(".github/workflows/ci.yml");
    assert!(
        ci.contains("    name: Lint and Test\n"),
        "Lint and Test job renamed"
    );
    assert!(
        ci.contains("os: [ubuntu-latest, macos-latest]"),
        "Lint and Test matrix changed"
    );
    assert!(
        ci.contains("name: Windows Build"),
        "Windows Build job renamed"
    );
}

#[test]
fn sync_job_uses_dedicated_token_and_fails_visibly_without_it() {
    let wf = read(".github/workflows/version-sync.yml");
    assert!(wf.contains("secrets.VERSION_SYNC_TOKEN"));
    assert!(wf.contains("::error title=VERSION_SYNC_TOKEN is not set::"));
    assert!(wf.contains("Allow auto-merge"));
    // Must never fall back to the default token for PR creation.
    assert!(!wf.contains("secrets.GITHUB_TOKEN"));
    assert!(!wf.contains("github.token"));
}

#[test]
fn sync_job_never_pushes_main_or_creates_tags_and_releases() {
    let wf = read(".github/workflows/version-sync.yml");
    for forbidden in [
        "git tag",
        "gh release",
        "push origin main",
        "git push origin HEAD:main",
    ] {
        assert!(
            !wf.contains(forbidden),
            "version-sync.yml must not contain `{forbidden}`"
        );
    }
    assert!(wf.contains("HEAD:refs/heads/$SYNC_BRANCH"));
}

#[test]
fn sync_job_skips_forks_and_is_serialized() {
    let wf = read(".github/workflows/version-sync.yml");
    assert!(wf.contains("github.event.repository.fork == false"));
    assert!(wf.contains("group: version-sync"));
    assert!(wf.contains("cancel-in-progress: false"));
}

#[test]
fn sync_job_calls_shared_code_and_reports_blocker() {
    let wf = read(".github/workflows/version-sync.yml");
    assert!(wf.contains("scripts/plan-version.sh --check"));
    assert!(wf.contains("./scripts/bump-version.sh"));
    assert!(wf.contains("Version is held"));
    assert!(wf.contains("--auto --squash"));
}

#[test]
fn nightly_stamps_version_and_fails_when_cargo_lags() {
    let wf = read(".github/workflows/nightly.yml");
    assert!(wf.contains("+nightly.$(date -u +%Y%m%d).${HEAD_SHA_SHORT}"));
    assert!(wf.contains("scripts/plan-version.sh --check"));
    assert!(wf.contains("Cargo.toml lags PLAN.md"));
    assert!(
        !wf.contains("--clobber"),
        "nightly must not rewrite published release assets"
    );
}

#[test]
fn plan_version_script_documents_exit_codes() {
    let sh = read("scripts/plan-version.sh");
    for code in ["0 ", "1 ", "2 ", "3 ", "4 "] {
        assert!(
            sh.contains(&format!("#   {code}")),
            "exit code {code}not documented"
        );
    }
    assert!(sh.contains("plan expected-version"));
}
