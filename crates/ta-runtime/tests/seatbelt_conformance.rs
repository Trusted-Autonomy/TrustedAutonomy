//! Seatbelt v2 conformance suite (v0.17.12.7).
//!
//! Runs `ta-sandbox-probe` inside the generated profile and checks each
//! allow/deny against the policy. macOS only. A host that cannot nest or run
//! `sandbox-exec` (for example an already sandboxed agent) skips the suite
//! unless `TA_REQUIRE_SANDBOX_CONFORMANCE=1`, which CI sets so a skip is a failure.
//!
//! Reads are NOT confined on macOS (read-allow with targeted denies); the
//! tests assert only the denies the profile claims, and that is all the docs claim.
#![cfg(target_os = "macos")]

use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ta_runtime::sandbox::{SandboxPolicy, SandboxProvider};
use ta_runtime::seatbelt;

const PROBE: &str = env!("CARGO_BIN_EXE_ta-sandbox-probe");

struct World {
    _root: tempfile::TempDir,
    staging: PathBuf,
    agent_home: PathBuf,
    ssh_key: PathBuf,
    npmrc: PathBuf,
    state_secret: PathBuf,
    outside: PathBuf,
    daemon: TcpListener,
    proxy: TcpListener,
    sock: PathBuf,
    _sock_listener: UnixListener,
    policy: SandboxPolicy,
}

fn world() -> World {
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let mk = |rel: &str| {
        let p = base.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    };
    let staging = mk("proj/.ta/staging/g1");
    let agent_home = mk("state/agent-homes/g1");
    let home = mk("home");
    let ssh = mk("home/.ssh");
    let outside = mk("outside");
    let state = mk("state");
    let ssh_key = ssh.join("id_ed25519");
    std::fs::write(&ssh_key, "secret").unwrap();
    let npmrc = home.join(".npmrc");
    std::fs::write(&npmrc, "//registry:_authToken=secret").unwrap();
    let state_secret = state.join("policy.toml");
    std::fs::write(&state_secret, "secret").unwrap();

    let daemon = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = staging.join("mcp.sock");
    let sock_listener = UnixListener::bind(&sock).unwrap();

    let policy = SandboxPolicy {
        enabled: true,
        provider: SandboxProvider::MacosSandboxExec,
        allow_read: Vec::new(),
        allow_write: Vec::new(),
        allow_network: vec!["api.anthropic.com".into()],
        deny_paths: vec![
            base.join("proj/.ta"),
            state.clone(),
            ssh.clone(),
            npmrc.clone(),
        ],
        goal_paths: vec![agent_home.clone(), staging.clone()],
        unix_sockets: vec![sock.clone()],
        egress_proxy_port: Some(proxy.local_addr().unwrap().port()),
        egress_unix_socket: None,
        violation_tag: Some(seatbelt::violation_tag("conformance-goal")),
    };
    World {
        _root: root,
        staging,
        agent_home,
        ssh_key,
        npmrc,
        state_secret,
        outside,
        daemon,
        proxy,
        sock,
        _sock_listener: sock_listener,
        policy,
    }
}

/// Run the probe inside the profile. `true` means the action succeeded.
fn sandboxed(w: &World, args: &[&str]) -> bool {
    let profile = seatbelt::generate(&w.policy, &w.staging);
    Command::new("/usr/bin/sandbox-exec")
        .args(["-p", &profile, "--", PROBE])
        .args(args)
        .current_dir(&w.staging)
        .env("TMPDIR", &w.agent_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map(|o| {
            if !o.status.success() {
                eprintln!(
                    "[probe {args:?}] {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                );
            }
            o.status.success()
        })
        .unwrap_or(false)
}

fn unsandboxed(args: &[&str]) -> bool {
    Command::new(PROBE)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// False (after saying why) when this host cannot run the suite.
fn can_run(w: &World) -> bool {
    if sandboxed(w, &["exec", "/usr/bin/true"]) {
        return true;
    }
    let msg = "sandbox-exec cannot run here (nested sandbox or restricted host): \
               macOS conformance suite not executed";
    assert!(
        std::env::var("TA_REQUIRE_SANDBOX_CONFORMANCE").as_deref() != Ok("1"),
        "{msg}, but TA_REQUIRE_SANDBOX_CONFORMANCE=1 requires it"
    );
    eprintln!("SKIP: {msg}");
    false
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn credential_and_ta_state_reads_and_writes_are_denied() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    // Control: the unsandboxed probe can read the same files.
    assert!(unsandboxed(&["read", s(&w.ssh_key)]));
    assert!(
        !sandboxed(&w, &["read", s(&w.ssh_key)]),
        ".ssh must be unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&w.npmrc)]),
        ".npmrc must be unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&w.state_secret)]),
        "state dir must be unreadable"
    );
    assert!(
        !sandboxed(&w, &["write", s(&w.ssh_key)]),
        ".ssh must be unwritable"
    );
    // The project `.ta` tree is denied, but the goal's own staging inside it works.
    let ta_file = w
        .staging
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("workflow.toml");
    std::fs::write(&ta_file, "x").unwrap();
    assert!(
        !sandboxed(&w, &["read", s(&ta_file)]),
        "project .ta content must be unreadable"
    );
}

#[test]
fn denies_hold_for_a_different_case_spelling() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let upper = w.ssh_key.to_string_lossy().replace("/.ssh/", "/.SSH/");
    if !Path::new(&upper).exists() {
        eprintln!("SKIP: case-sensitive volume, nothing to prove");
        return;
    }
    assert!(
        !sandboxed(&w, &["read", &upper]),
        "case-folded path must be denied too"
    );
}

#[test]
fn writes_are_confined_to_staging_and_the_agent_home() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let ok_staging = w.staging.join("out.txt");
    let ok_home = w.agent_home.join("scratch.txt");
    let bad_outside = w.outside.join("nope.txt");
    assert!(sandboxed(&w, &["write", s(&ok_staging)]));
    assert!(sandboxed(&w, &["write", s(&ok_home)]));
    assert!(!sandboxed(&w, &["write", s(&bad_outside)]));
    assert!(!bad_outside.exists());
    assert!(!sandboxed(
        &w,
        &["write", "/private/tmp/ta-conformance-should-not-exist"]
    ));
    assert!(!Path::new("/private/tmp/ta-conformance-should-not-exist").exists());
    assert!(!sandboxed(
        &w,
        &["write", "/Library/ta-conformance-should-not-exist"]
    ));
}

#[test]
fn network_reaches_only_the_proxy_and_the_mediation_socket() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let daemon = w.daemon.local_addr().unwrap().to_string();
    let proxy = w.proxy.local_addr().unwrap().to_string();
    assert!(
        unsandboxed(&["connect", &daemon]),
        "control: daemon port is reachable unsandboxed"
    );
    assert!(
        !sandboxed(&w, &["connect", &daemon]),
        "the daemon port must be unreachable"
    );
    assert!(
        sandboxed(&w, &["connect", &proxy]),
        "the proxy port must be reachable"
    );
    assert!(
        sandboxed(&w, &["unix", s(&w.sock)]),
        "the mediation socket must be reachable"
    );
    // Any other unix socket is refused.
    let other = w.outside.join("other.sock");
    let _l = UnixListener::bind(&other).unwrap();
    assert!(!sandboxed(&w, &["unix", s(&other)]));
}

#[test]
fn star_network_grant_still_cannot_reach_loopback() {
    let mut w = world();
    w.policy.allow_network = vec!["*".into()];
    w.policy.egress_proxy_port = None;
    if !can_run(&w) {
        return;
    }
    let daemon = w.daemon.local_addr().unwrap().to_string();
    assert!(!sandboxed(&w, &["connect", &daemon]));
}

#[test]
fn security_and_ta_daemon_cannot_be_executed() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    assert!(sandboxed(&w, &["exec", "/usr/bin/true"]));
    assert!(!sandboxed(&w, &["exec", "/usr/bin/security", "help"]));
    let fake = w.staging.join("ta-daemon");
    std::fs::copy("/usr/bin/true", &fake).unwrap();
    assert!(
        unsandboxed(&["exec", s(&fake)]),
        "control: the copy runs unsandboxed"
    );
    assert!(
        !sandboxed(&w, &["exec", s(&fake)]),
        "ta-daemon must not be executable"
    );
}

#[test]
fn keychain_and_launch_services_are_unreachable_but_basics_work() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    for svc in seatbelt::DENIED_MACH_SERVICES {
        if unsandboxed(&["mach", svc]) {
            assert!(
                !sandboxed(&w, &["mach", svc]),
                "mach service {svc} must be denied"
            );
        } else {
            eprintln!("SKIP {svc}: not registered on this host");
        }
    }
    for svc in seatbelt::ALLOWED_MACH_SERVICES {
        if unsandboxed(&["mach", svc]) {
            assert!(
                sandboxed(&w, &["mach", svc]),
                "mach service {svc} must stay reachable"
            );
        }
    }
}

#[test]
fn other_processes_cannot_be_signalled_or_inspected() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let mut victim = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let pid = victim.id().to_string();
    assert!(unsandboxed(&["signal", &pid]));
    assert!(
        !sandboxed(&w, &["signal", &pid]),
        "signalling another process must be denied"
    );
    assert!(unsandboxed(&["pidinfo", &pid]));
    assert!(
        !sandboxed(&w, &["pidinfo", &pid]),
        "inspecting another process must be denied"
    );
    let _ = victim.kill();
    let _ = victim.wait();
    assert!(
        sandboxed(&w, &["signal-child"]),
        "the agent may signal its own children"
    );
}

#[test]
fn rosetta_translated_binaries_still_run() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    if !unsandboxed(&["rosetta"]) {
        eprintln!("SKIP: Rosetta is not installed on this host");
        return;
    }
    assert!(
        sandboxed(&w, &["rosetta"]),
        "x86_64 translation must work inside the profile"
    );
}

#[test]
fn denials_carry_the_goal_tag_in_the_unified_log() {
    use std::sync::{Arc, Mutex};
    use ta_runtime::seatbelt_log::ViolationTail;
    let w = world();
    if !can_run(&w) {
        return;
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let tag = w.policy.violation_tag.clone().unwrap();
    let Ok(tail) = ViolationTail::start(tag, Arc::new(move |v| sink.lock().unwrap().push(v)))
    else {
        eprintln!("SKIP: /usr/bin/log unavailable");
        return;
    };
    std::thread::sleep(std::time::Duration::from_millis(4000));
    // Enforcement is the hard assertion: the read must be refused.
    assert!(!sandboxed(&w, &["read", s(&w.ssh_key)]));
    for _ in 0..40 {
        if seen
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.target.contains(".ssh") && v.operation.starts_with("file-read"))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // Delivery of the denial to `log stream` is NOT asserted. Measured on macOS:
    // with the profile fixed (tags on explicit denies only), roughly 1 run in 6
    // to 1 in 4 delivers no tagged line for the whole test, even over repeated
    // refused reads, while the refusal itself is always enforced. The cause is
    // unexplained, so a required check must not depend on it. The profile shape
    // and the parser are covered by the unit tests; this prints what was seen.
    let got = seen.lock().unwrap().clone();
    eprintln!(
        "WARN: unified log did not deliver a tagged denial ({} raw Sandbox events seen, \
         tagged={got:?}); sandbox violation events can be missing for a goal",
        tail.lines_seen()
    );
}
