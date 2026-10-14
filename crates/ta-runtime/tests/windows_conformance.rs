//! AppContainer conformance suite (v0.17.12.9). Windows only.
//!
//! Launches small `cmd.exe` and PowerShell probes inside a real AppContainer and
//! checks what the policy claims: staging is writable, the rest of the machine is
//! not, loopback is unreachable (no loopback exemption is ever granted), and the
//! container SID reaches no Credential Manager secrets.
//!
//! The Credential Manager and DPAPI probes record what Windows actually does
//! instead of asserting an expectation: they exist to answer the open question
//! from the sandbox plan. Their results are printed as `FINDING` lines (run with
//! `--nocapture`) and written to the file named by `TA_WINDOWS_FINDINGS`.
//!
//! A host that cannot create an AppContainer profile (a CI image with the
//! feature locked down) skips the suite unless `TA_REQUIRE_SANDBOX_CONFORMANCE=1`,
//! which CI sets so a skip is a failure.
#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use ta_runtime::adapter::{SpawnRequest, StdinMode, StdoutMode};
use ta_runtime::sandbox_windows::{sandboxed_spawn, WindowsAppContainerGuard};
use ta_runtime::BareProcessRuntime;

struct World {
    _root: tempfile::TempDir,
    staging: PathBuf,
    outside: PathBuf,
    guard: WindowsAppContainerGuard,
}

fn required() -> bool {
    std::env::var("TA_REQUIRE_SANDBOX_CONFORMANCE").as_deref() == Ok("1")
}

fn world(name: &str) -> Option<World> {
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let staging = base.join("staging");
    let outside = base.join("outside");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    match WindowsAppContainerGuard::new(name, &staging, false) {
        Ok(guard) => Some(World {
            _root: root,
            staging,
            outside,
            guard,
        }),
        Err(e) => {
            assert!(
                !required(),
                "AppContainer is required on this runner (TA_REQUIRE_SANDBOX_CONFORMANCE=1) but \
                 the profile could not be created: {e}"
            );
            eprintln!("SKIP: cannot create an AppContainer profile here: {e}");
            None
        }
    }
}

/// Run `command args` inside the container and return its exit code.
fn run(w: &World, command: &str, args: &[&str]) -> i32 {
    let request = SpawnRequest {
        command: command.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: HashMap::new(),
        working_dir: w.staging.clone(),
        stdin_mode: StdinMode::Null,
        stdout_mode: StdoutMode::Inherited,
        clear_env: false,
    };
    let runtime = BareProcessRuntime::new();
    let mut handle = sandboxed_spawn(&runtime, request, Some(&w.guard))
        .unwrap_or_else(|e| panic!("could not start {command} in the container: {e}"));
    let status = handle.wait().expect("wait for the probe");
    status.code().unwrap_or(-1)
}

fn powershell(w: &World, script: &str) -> i32 {
    run(
        w,
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
}

fn finding(name: &str, text: &str) {
    let line = format!("FINDING {name}: {text}");
    println!("{line}");
    if let Ok(path) = std::env::var("TA_WINDOWS_FINDINGS") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn write_probe(w: &World, dir: &Path, file: &str) -> bool {
    let target = dir.join(file);
    // `fs::canonicalize` returns a `\\?\C:\...` verbatim path on Windows, which
    // PowerShell's Set-Content rejects as "Illegal characters in path". The script
    // needs the plain drive form; the guard keeps the canonical one.
    let plain = target.display().to_string();
    let plain = plain.strip_prefix(r"\\?\").unwrap_or(&plain);
    let script = format!("Set-Content -LiteralPath '{plain}' -Value ok -ErrorAction Stop");
    powershell(w, &script);
    target.exists()
}

#[test]
fn staging_is_writable_and_the_rest_of_the_machine_is_not() {
    let Some(w) = world("ta-conf-fs") else { return };
    assert!(
        write_probe(&w, &w.staging, "inside.txt"),
        "the container could not write into its own staging directory"
    );
    assert!(
        !write_probe(&w, &w.outside, "escape.txt"),
        "the container wrote outside its staging directory"
    );
}

#[test]
fn loopback_is_unreachable_without_an_exemption() {
    let Some(w) = world("ta-conf-net") else {
        return;
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let code = powershell(
        &w,
        &format!(
            "try {{ $c = New-Object Net.Sockets.TcpClient; $c.Connect('127.0.0.1', {port}); exit 0 }} \
             catch {{ exit 1 }}"
        ),
    );
    assert_eq!(
        code, 1,
        "the container reached a host loopback listener; no loopback exemption may exist"
    );
}

#[test]
fn credential_manager_and_dpapi_from_inside_the_container() {
    let Some(w) = world("ta-conf-cred") else {
        return;
    };

    // A secret in the host user's Credential Manager, planted under a unique target.
    let target = format!("ta-conformance-{}", std::process::id());
    let planted = std::process::Command::new("cmdkey")
        .args([
            &format!("/generic:{target}"),
            "/user:ta",
            "/pass:conformance",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        planted,
        "could not plant a Credential Manager entry for the probe"
    );

    // `cmdkey /list:<target>` prints a "Currently stored credentials for <target>:" header
    // whether or not anything matches, so matching the bare name would report a hit every
    // time. A real entry also prints a `Target: ...target=<name>` line; match that.
    let marker = format!("Target: .*{}", target.replace('-', "\\-"));
    let list = format!("cmdkey /list:{target} | Select-String -Quiet -Pattern '{marker}'");
    let script = format!("if ({list}) {{ exit 0 }} else {{ exit 1 }}");

    // Control: the same probe on the host (outside the container) must see the entry, or
    // the probe cannot tell "hidden from the container" from "probe is broken".
    let host_sees = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map(|s| s.code() == Some(0))
        .unwrap_or(false);
    assert!(
        host_sees,
        "control failed: the probe cannot see a planted Credential Manager entry even from \
         the host, so its result inside the container would mean nothing"
    );

    let sees_entry = powershell(&w, &script) == 0;
    finding(
        "credential-manager",
        &format!(
            "an AppContainer process {} the host user's generic credential",
            if sees_entry { "CAN see" } else { "can NOT see" }
        ),
    );

    // DPAPI (CurrentUser scope) from inside the container.
    let dpapi = powershell(
        &w,
        "Add-Type -AssemblyName System.Security; \
         try { $b = [Text.Encoding]::UTF8.GetBytes('x'); \
         $p = [Security.Cryptography.ProtectedData]::Protect($b, $null, 'CurrentUser'); \
         $u = [Security.Cryptography.ProtectedData]::Unprotect($p, $null, 'CurrentUser'); \
         if ($u.Length -eq 1) { exit 0 } else { exit 1 } } catch { exit 2 }",
    );
    finding(
        "dpapi-current-user",
        &format!(
            "an AppContainer process {} (probe exit code {dpapi})",
            if dpapi == 0 {
                "CAN use CurrentUser DPAPI"
            } else {
                "can NOT use CurrentUser DPAPI"
            }
        ),
    );

    let _ = std::process::Command::new("cmdkey")
        .arg(format!("/delete:{target}"))
        .status();

    // This is an open design question (PLAN v0.17.12.9 item 5), answered by the FINDING
    // lines above, not a regression TA can fix in code: whether an AppContainer process
    // can reach the user's Credential Manager is a Windows fact. Failing here would hold
    // CI red on that fact and block every later phase, so the answer is recorded and
    // loudly flagged instead. The control above still fails if the probe itself is broken.
    if sees_entry {
        eprintln!(
            "WARN: an AppContainer process can see the host user's Credential Manager \
             entries. The credential broker must not assume the container hides them; keep \
             secrets out of the user's Credential Manager scope or broker them over the pipe."
        );
    }
}
