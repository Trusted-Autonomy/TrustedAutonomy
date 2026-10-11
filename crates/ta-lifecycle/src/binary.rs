//! Sibling-binary lookup and installed-build identity (`<bin> --version`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::version::BuildIdentity;

/// `ta-daemon` on Unix, `ta-daemon.exe` on Windows.
pub fn binary_file_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `<dir of exe>/<name>` when it exists.
pub fn sibling_of(exe: &Path, name: &str) -> Option<PathBuf> {
    let candidate = exe.parent()?.join(binary_file_name(name));
    candidate.is_file().then_some(candidate)
}

/// First `<dir>/<name>` found in the `path_var` directories.
pub fn find_on_path(name: &str, path_var: Option<OsString>) -> Option<PathBuf> {
    let file = binary_file_name(name);
    std::env::split_paths(&path_var?)
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

/// Locate `name`: sibling of `exe` first (zip/tar installs keep both binaries
/// together), then `path_var`. The error says where it looked.
pub fn locate_binary_from(
    exe: Option<&Path>,
    path_var: Option<OsString>,
    name: &str,
) -> Result<PathBuf, String> {
    if let Some(p) = exe.and_then(|e| sibling_of(e, name)) {
        return Ok(p);
    }
    find_on_path(name, path_var).ok_or_else(|| {
        format!(
            "Cannot find '{}' binary. Ensure it is in the same directory as '{}' or on your PATH.",
            binary_file_name(name),
            exe.map(|e| e.display().to_string())
                .unwrap_or_else(|| "the running executable".into())
        )
    })
}

/// [`locate_binary_from`] using the current executable and the real `PATH`.
pub fn locate_sibling_binary(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().ok();
    locate_binary_from(exe.as_deref(), std::env::var_os("PATH"), name)
}

/// Run `<bin> --version` with a timeout and return its stdout.
pub fn run_version_command(bin: &Path, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run `{} --version`: {e}", bin.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    use std::io::Read;
                    let _ = so.read_to_string(&mut out);
                }
                return if status.success() {
                    Ok(out)
                } else {
                    Err(format!(
                        "`{} --version` exited with {status}; the installed binary may be broken",
                        bin.display()
                    ))
                };
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{} --version` did not finish within {}s",
                    bin.display(),
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("waiting for `{} --version`: {e}", bin.display())),
        }
    }
}

/// Identity of the installed binary, via an injectable version runner.
pub fn read_installed_identity_with(
    bin: &Path,
    run: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<BuildIdentity, String> {
    let out = run(bin)?;
    BuildIdentity::parse_version_output(&out).ok_or_else(|| {
        format!(
            "`{} --version` printed output with no version number: {:?}",
            bin.display(),
            out.trim()
        )
    })
}

/// Identity of the installed binary by running `<bin> --version` (5s timeout).
pub fn read_installed_identity(bin: &Path) -> Result<BuildIdentity, String> {
    read_installed_identity_with(bin, &|b| run_version_command(b, Duration::from_secs(5)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_is_preferred_over_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sib = a.path().join(binary_file_name("ta-daemon"));
        std::fs::write(&sib, b"x").unwrap();
        std::fs::write(b.path().join(binary_file_name("ta-daemon")), b"x").unwrap();
        let exe = a.path().join(binary_file_name("ta"));
        let path = std::env::join_paths([b.path()]).unwrap();
        assert_eq!(
            locate_binary_from(Some(&exe), Some(path), "ta-daemon").unwrap(),
            sib
        );
    }

    #[test]
    fn falls_back_to_path_and_errors_with_guidance() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let on_path = b.path().join(binary_file_name("ta-daemon"));
        std::fs::write(&on_path, b"x").unwrap();
        let exe = a.path().join(binary_file_name("ta"));
        let path = std::env::join_paths([b.path()]).unwrap();
        assert_eq!(
            locate_binary_from(Some(&exe), Some(path), "ta-daemon").unwrap(),
            on_path
        );
        let err = locate_binary_from(Some(&exe), None, "ta-daemon").unwrap_err();
        assert!(
            err.contains("same directory") && err.contains("PATH"),
            "{err}"
        );
    }

    #[test]
    fn installed_identity_uses_the_injected_runner() {
        let id = read_installed_identity_with(Path::new("/x/ta-daemon"), &|_| {
            Ok("ta-daemon 1.2.3 (abc1234)\n".into())
        })
        .unwrap();
        assert_eq!(id, BuildIdentity::new("1.2.3", Some("abc1234")));
        let err = read_installed_identity_with(Path::new("/x/ta-daemon"), &|_| Ok("???".into()))
            .unwrap_err();
        assert!(err.contains("no version number"), "{err}");
        let err = read_installed_identity_with(Path::new("/x/ta-daemon"), &|_| Err("boom".into()))
            .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[cfg(unix)]
    #[test]
    fn real_version_command_runs_a_shell_script() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("fake-daemon");
        std::fs::write(&bin, "#!/bin/sh\necho 'fake-daemon 9.9.9 (feedbee)'\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let id = read_installed_identity(&bin).unwrap();
        assert_eq!(id, BuildIdentity::new("9.9.9", Some("feedbee")));
        std::fs::write(&bin, "#!/bin/sh\nexit 3\n").unwrap();
        assert!(read_installed_identity(&bin)
            .unwrap_err()
            .contains("exited"));
    }
}
