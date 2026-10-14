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

/// Re-entry marker set on the child this shim launches. A shim started with
/// this already set was launched (directly or through a chain) by another
/// shim, which means self-resolution slipped past the path comparison in
/// `find_real_gh`. Refusing to run ends the chain at depth one instead of
/// forking processes until the machine is exhausted.
const REENTRY_ENV: &str = "TA_GH_SHIM_ACTIVE";

fn main() {
    if std::env::var_os(REENTRY_ENV).is_some() {
        eprintln!(
            "ta gh-shim: refusing to run: this wrapper was launched by another ta gh-shim \
             ({REENTRY_ENV} is set), so 'gh' on PATH resolves back to a wrapper. \
             Remove the wrapper directory (.ta/bin, target/debug) from PATH or install \
             the real GitHub CLI (https://cli.github.com)"
        );
        std::process::exit(126);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let self_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let self_exe = std::env::current_exe().ok();
    let real_gh = match find_real_gh(&path_var, self_dir.as_deref(), self_exe.as_deref(), &cwd) {
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

    // Inside a sandbox `.ta/` is unreadable: ask the out-of-sandbox gateway.
    #[cfg(unix)]
    let via_gateway = match (
        std::env::var_os("TA_CREDENTIAL_ENDPOINT"),
        std::env::var("TA_MCP_CONNECTION_TOKEN").ok(),
    ) {
        (Some(endpoint), Some(token)) => Some(
            ta_credential_broker::resolve_via_endpoint(Path::new(&endpoint), &token, GH_HOST)
                .map(|r| r.map(|r| r.secret))
                .unwrap_or_else(|e| {
                    eprintln!("ta gh-shim: {e}");
                    None
                }),
        ),
        _ => None,
    };
    #[cfg(not(unix))]
    let via_gateway: Option<Option<String>> = None;
    let secret = match via_gateway {
        Some(secret) => secret,
        None => ta_credential_broker::resolve_for_host(&project_root, GH_HOST, true)
            .map(|resolution| resolution.secret)
            .ok(),
    };

    let mut cmd = std::process::Command::new(&real_gh);
    cmd.args(&args);
    cmd.env(REENTRY_ENV, "1");
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

/// Canonical form of `p` for identity comparison. On Windows `current_exe`
/// and `PATH` entries can differ in case, 8.3 short names or a `\\?\` verbatim
/// prefix, so a plain string comparison misses a match; `canonicalize`
/// resolves all of those. Falls back to the input when the path cannot be
/// resolved (the comparison is then no worse than before).
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Build a `PATH`-shaped value with `exclude_dir` removed. Directories are
/// compared by canonical path, so the same directory spelled differently is
/// still excluded. Pure apart from reading the filesystem to canonicalize.
fn filtered_path(path_var: &str, exclude_dir: Option<&Path>) -> OsString {
    let exclude = exclude_dir.map(canonical);
    let dirs: Vec<PathBuf> = std::env::split_paths(path_var)
        .filter(|d| match &exclude {
            Some(ex) => canonical(d) != *ex,
            None => true,
        })
        .collect();
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Locate the real `gh` binary on `path_var`, skipping `exclude_dir` (this
/// wrapper's own directory) and any candidate that is this very executable
/// (`self_exe`), compared by canonical path. Delegates to
/// `which::which_all`, which already handles `PATHEXT`/executable-bit
/// resolution per platform (`gh.exe` on Windows); only the exclusion and
/// PATH plumbing around it is this shim's own logic.
fn find_real_gh(
    path_var: &str,
    exclude_dir: Option<&Path>,
    self_exe: Option<&Path>,
    cwd: &Path,
) -> Option<PathBuf> {
    let candidate_path = filtered_path(path_var, exclude_dir);
    let self_exe = self_exe.map(canonical);
    which::which_in_all("gh", Some(candidate_path), cwd)
        .ok()?
        .find(|c| match &self_exe {
            Some(me) => canonical(c) != *me,
            None => true,
        })
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
        let found = find_real_gh(&path_var, Some(&wrapper_dir), None, dir.path()).unwrap();
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
        assert!(find_real_gh(&path_var, Some(&wrapper_dir), None, dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn find_real_gh_skips_own_executable_even_when_dir_exclusion_misses() {
        let dir = TempDir::new().unwrap();
        let wrapper_dir = dir.path().join("wrapper");
        let real_dir = dir.path().join("real");
        fs::create_dir_all(&wrapper_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        make_executable(&wrapper_dir.join("gh"));
        make_executable(&real_dir.join("gh"));

        // No dir exclusion at all (as when current_exe and PATH disagree on
        // spelling): the wrapper is first on PATH but must still be skipped
        // because it is this executable.
        let path_var = format!("{}:{}", wrapper_dir.display(), real_dir.display());
        let me = wrapper_dir.join("gh");
        let found = find_real_gh(&path_var, None, Some(&me), dir.path()).unwrap();
        assert_eq!(found, real_dir.join("gh"));
    }

    #[cfg(unix)]
    #[test]
    fn find_real_gh_matches_own_dir_through_a_symlinked_spelling() {
        let dir = TempDir::new().unwrap();
        let wrapper_dir = dir.path().join("wrapper");
        let alias = dir.path().join("alias");
        let real_dir = dir.path().join("real");
        fs::create_dir_all(&wrapper_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        std::os::unix::fs::symlink(&wrapper_dir, &alias).unwrap();
        make_executable(&wrapper_dir.join("gh"));
        make_executable(&real_dir.join("gh"));

        // PATH names the wrapper dir through the alias; exclusion is given
        // the direct spelling. Canonical comparison must still exclude it.
        let path_var = format!("{}:{}", alias.display(), real_dir.display());
        let found = find_real_gh(&path_var, Some(&wrapper_dir), None, dir.path()).unwrap();
        assert_eq!(found, real_dir.join("gh"));
    }
}
