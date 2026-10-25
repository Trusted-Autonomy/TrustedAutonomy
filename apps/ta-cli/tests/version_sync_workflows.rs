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

#[test]
fn sync_runs_only_when_plan_or_cargo_toml_change_and_validates_the_version() {
    let wf = read(".github/workflows/version-sync.yml");
    assert!(
        wf.contains("    paths: [PLAN.md, Cargo.toml]\n"),
        "the push trigger must be limited to the files the expected version depends on"
    );
    assert!(
        wf.contains("workflow_dispatch"),
        "manual runs must stay available"
    );
    assert!(
        wf.contains(r"SEMVER_RE='^[0-9]+\.[0-9]+\.[0-9]+(-alpha(\.[0-9]+)?)?$'"),
        "the expected version must be checked against a strict semver"
    );
    assert!(wf.contains("::error title=Expected version is not a semver::"));
}

#[test]
fn dev_shell_banner_goes_to_stderr_so_command_stdout_stays_clean() {
    let flake = read("flake.nix");
    let hook_start = flake
        .find("shellHook = ''")
        .expect("flake.nix has a shellHook");
    let hook = &flake[hook_start..];
    let hook = &hook[..hook.find("'';").expect("shellHook is terminated")];
    assert!(
        hook.contains("} >&2"),
        "the shellHook banner must be redirected to stderr: nix develop --command \
         output is captured by CI (version-sync.yml)"
    );
}

/// `scripts/bump-version.sh` writes its argument into Cargo.toml, so it must
/// reject non-semver text (e.g. a captured shell banner) before touching a
/// file. Runs a copy of the script in a scratch tree so a regression cannot
/// damage the real checkout.
#[cfg(unix)]
#[test]
fn bump_version_refuses_a_non_semver_argument_without_touching_files() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("scripts")).unwrap();
    std::fs::copy(
        root.join("scripts/bump-version.sh"),
        tmp.path().join("scripts/bump-version.sh"),
    )
    .unwrap();
    let manifest = "[workspace.package]\nversion = \"0.1.0-alpha\"\n";
    std::fs::write(tmp.path().join("Cargo.toml"), manifest).unwrap();

    for bad in [
        "TrustedAutonomydevenvironmentloadedRust:rustc1.97.0",
        "Trusted Autonomy dev environment loaded",
        "0.17",
        "v0.17.12",
        "0.17.12-",
        "0.17.12\n0.17.13",
    ] {
        let out = std::process::Command::new("bash")
            .arg(tmp.path().join("scripts/bump-version.sh"))
            .arg(bad)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{bad:?} must be refused");
        assert!(
            stderr.contains("is not a valid version"),
            "{bad:?}: {stderr}"
        );
        assert!(stderr.contains("nothing was changed"), "{bad:?}: {stderr}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap(),
            manifest,
            "{bad:?} must leave Cargo.toml untouched"
        );
    }
}
