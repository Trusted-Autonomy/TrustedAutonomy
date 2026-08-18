// gh_shim.rs — `gh` PATH-shadow wrapper for broker-mediated GitHub
// credentials (v0.17.6.7, PLAN item 2).
//
// `gh` has no pluggable "credential helper" hook the way git does, so
// closing this shell/CLI leak path for the GitHub CLI needs a different
// mechanism: `ta run` stages a copy of this binary at
// `<project_root>/.ta/bin/gh` and prepends that directory to the agent's
// `PATH`, but only when a broker-mediated connector declares
// `hosts = ["github.com", ...]` in `.ta/connectors.toml` (see
// `run.rs::install_credential_shims`). The agent's own shell then resolves
// bare `gh` invocations to this wrapper before the real CLI further down
// `PATH`.
//
// Each invocation: resolve the real secret via
// `ta_credential_broker::resolve_for_host` (the same broker/vault lookup
// `ta credential-helper` uses for git), find the *real* `gh` binary by
// searching `PATH` with this wrapper's own directory excluded (otherwise a
// naive search just finds this same binary again and recurses forever), and
// exec it with `GH_TOKEN` set only on that one child process — never on
// this wrapper's own environment, and never touching the agent's own
// long-lived shell environment (`std::env::set_var` is never called here).
//
// Fails open: no matching connector, no session token, or no real `gh`
// found on `PATH` all fall through to "run the real `gh` with no token
// injected" wherever a real `gh` can still be located — the same reduced-
// security fallback behavior as if this shim had never been installed —
// rather than blocking the agent's workflow over a broker misconfiguration.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const GH_HOST: &str = "github.com";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let self_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let real_gh = match find_real_gh(&path_var, self_dir.as_deref(), &cwd) {
        Some(p) => p,
        None => {
            eprintln!(
                "ta gh-shim: no other 'gh' binary found on PATH besides this wrapper — \
                 install the GitHub CLI, or remove .ta/bin from PATH to bypass the shim"
            );
            std::process::exit(127);
        }
    };

    let project_root = std::env::var("TA_PROJECT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| cwd.clone());

    let secret = ta_credential_broker::resolve_for_host(&project_root, GH_HOST, true)
        .map(|resolution| resolution.secret)
        .ok();

    let mut cmd = std::process::Command::new(&real_gh);
    cmd.args(&args);
    if let Some(secret) = &secret {
        cmd.env("GH_TOKEN", secret);
    }

    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "ta gh-shim: failed to launch real gh at {}: {e}",
                real_gh.display()
            );
            std::process::exit(127);
        }
    };
    std::process::exit(status.code().unwrap_or(1));
}

/// Build a `PATH`-shaped value with `exclude_dir` removed — pure, so it's
/// directly testable without touching the real process environment.
fn filtered_path(path_var: &str, exclude_dir: Option<&Path>) -> OsString {
    let dirs: Vec<PathBuf> = std::env::split_paths(path_var)
        .filter(|d| Some(d.as_path()) != exclude_dir)
        .collect();
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Locate the real `gh` binary on `path_var`, skipping `exclude_dir` (this
/// wrapper's own directory). Delegates to `which::which_in`, which already
/// handles `PATHEXT`/executable-bit resolution correctly per platform —
/// only the exclusion + PATH plumbing around it is this shim's own logic.
fn find_real_gh(path_var: &str, exclude_dir: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    let candidate_path = filtered_path(path_var, exclude_dir);
    which::which_in("gh", Some(candidate_path), cwd).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use tempfile::TempDir;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, "#!/bin/sh\necho fake-gh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn filtered_path_removes_only_the_excluded_dir() {
        let path_var = if cfg!(windows) {
            r"C:\wrapper;C:\real;C:\other"
        } else {
            "/wrapper:/real:/other"
        };
        let exclude = if cfg!(windows) {
            PathBuf::from(r"C:\wrapper")
        } else {
            PathBuf::from("/wrapper")
        };
        let filtered = filtered_path(path_var, Some(&exclude));
        let filtered_str = filtered.to_string_lossy();
        assert!(!filtered_str.contains("wrapper"));
        assert!(filtered_str.contains("real"));
        assert!(filtered_str.contains("other"));
    }

    #[cfg(unix)]
    #[test]
    fn find_real_gh_skips_own_directory_and_finds_the_real_binary() {
        let dir = TempDir::new().unwrap();
        let wrapper_dir = dir.path().join("wrapper");
        let real_dir = dir.path().join("real");
        fs::create_dir_all(&wrapper_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        // A same-named file in the wrapper's own dir must never be returned
        // — that would just be this wrapper re-invoking itself forever.
        make_executable(&wrapper_dir.join("gh"));
        make_executable(&real_dir.join("gh"));

        let path_var = format!("{}:{}", wrapper_dir.display(), real_dir.display());
        let found = find_real_gh(&path_var, Some(&wrapper_dir), dir.path()).unwrap();
        assert_eq!(found, real_dir.join("gh"));
    }

    #[cfg(unix)]
    #[test]
    fn find_real_gh_returns_none_when_only_the_wrapper_is_on_path() {
        let dir = TempDir::new().unwrap();
        let wrapper_dir = dir.path().join("wrapper");
        fs::create_dir_all(&wrapper_dir).unwrap();
        make_executable(&wrapper_dir.join("gh"));

        let path_var = wrapper_dir.display().to_string();
        assert!(find_real_gh(&path_var, Some(&wrapper_dir), dir.path()).is_none());
    }
}
