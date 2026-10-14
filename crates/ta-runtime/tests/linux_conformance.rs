//! Linux hardened sandbox conformance suite (v0.17.12.8).
//!
//! Runs `ta-sandbox-probe` inside the real bubblewrap + Landlock + seccomp
//! sandbox (and inside the Landlock-only fallback) and checks each allow and
//! deny against the policy. Linux only.
//!
//! A host that cannot run a layer (no bubblewrap, userns disabled, a container
//! without the needed privileges, a kernel without Landlock) skips the matching
//! tests with a stated reason, unless `TA_REQUIRE_SANDBOX_CONFORMANCE=1`, which
//! the Linux CI jobs set so a skip is a failure. The `TA_EXPECT_NO_*` variables
//! let the degraded CI jobs assert the opposite: that the layer really is
//! missing and TA reported it instead of pretending.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use ta_runtime::adapter::{SpawnRequest, StdinMode, StdoutMode};
use ta_runtime::linux_bwrap::{wrap_bwrap, wrap_landlock_only};
use ta_runtime::linux_confine::landlock_status;
use ta_runtime::linux_probe::{self, BwrapStatus, HelperCommand};
use ta_runtime::linux_relay::HostRelay;
use ta_runtime::sandbox::{SandboxPolicy, SandboxProvider};

const PROBE: &str = env!("CARGO_BIN_EXE_ta-sandbox-probe");
const HELPER: &str = env!("CARGO_BIN_EXE_ta-sandbox-exec");

fn require() -> bool {
    std::env::var("TA_REQUIRE_SANDBOX_CONFORMANCE").as_deref() == Ok("1")
}

fn expects(var: &str) -> bool {
    std::env::var(var).as_deref() == Ok("1")
}

fn skip_or_fail(why: &str) {
    // A degraded CI job that declared the layer missing expects the skip.
    let expected = (why.starts_with("bubblewrap") && expects("TA_EXPECT_NO_BWRAP"))
        || (why.starts_with("Landlock") && expects("TA_EXPECT_NO_LANDLOCK"));
    assert!(
        expected || !require(),
        "{why}, but TA_REQUIRE_SANDBOX_CONFORMANCE=1 requires this suite to run"
    );
    eprintln!("SKIP: {why}");
}

fn helper() -> HelperCommand {
    HelperCommand {
        program: PathBuf::from(HELPER),
        prefix_args: Vec::new(),
    }
}

struct World {
    _root: tempfile::TempDir,
    staging: PathBuf,
    agent_home: PathBuf,
    ta_secret: PathBuf,
    outside: PathBuf,
    outside_file: PathBuf,
    daemon: TcpListener,
    proxy_port: u16,
    _relay: HostRelay,
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
    let outside = mk("outside");
    mk("state");
    let ta_dir = mk("proj/.ta");
    let ta_secret = staging.join("private").join("secret");
    std::fs::create_dir_all(staging.join("private")).unwrap();
    std::fs::write(&ta_secret, "secret").unwrap();
    std::fs::write(ta_dir.join("workflow.toml"), "secret").unwrap();
    let outside_file = outside.join("data.txt");
    std::fs::write(&outside_file, "outside").unwrap();
    std::fs::write(staging.join("work.txt"), "ok").unwrap();

    // A host service the sandbox must not reach, and the "egress proxy" it may.
    let daemon = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in proxy.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4];
                if c.read_exact(&mut b).is_ok() {
                    let _ = c.write_all(&b);
                }
            });
        }
    });
    let relay = HostRelay::start(&base.join("relay"), proxy_port).unwrap();

    let probe_dir = Path::new(PROBE).parent().unwrap().to_path_buf();
    let policy = SandboxPolicy {
        enabled: true,
        provider: SandboxProvider::LinuxBwrap,
        allow_read: vec![probe_dir],
        allow_write: Vec::new(),
        allow_network: vec!["api.anthropic.com".into()],
        deny_paths: vec![ta_dir, base.join("state"), staging.join("private")],
        goal_paths: vec![agent_home.clone()],
        unix_sockets: Vec::new(),
        egress_proxy_port: Some(proxy_port),
        egress_unix_socket: Some(relay.socket_path().to_path_buf()),
        violation_tag: None,
    };
    World {
        _root: root,
        staging,
        agent_home,
        ta_secret,
        outside,
        outside_file,
        daemon,
        proxy_port,
        _relay: relay,
        policy,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Bwrap,
    LandlockOnly,
}

fn bwrap_path() -> Option<PathBuf> {
    match linux_probe::probe().bwrap {
        BwrapStatus::Usable { path, .. } => Some(path),
        _ => None,
    }
}

fn request(w: &World, args: &[&str]) -> SpawnRequest {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    SpawnRequest {
        command: PROBE.to_string(),
        args: a,
        env: HashMap::new(),
        working_dir: w.staging.clone(),
        stdin_mode: StdinMode::Null,
        stdout_mode: StdoutMode::Inherited,
        clear_env: false,
    }
}

fn wrapped(w: &World, mode: Mode, args: &[&str]) -> Command {
    let req = request(w, args);
    let mut p = w.policy.clone();
    let req = match mode {
        Mode::Bwrap => {
            p.provider = SandboxProvider::LinuxBwrap;
            wrap_bwrap(req, &p, &bwrap_path().expect("bwrap"), &helper())
        }
        Mode::LandlockOnly => {
            p.provider = SandboxProvider::LinuxLandlock;
            wrap_landlock_only(req, &p, &helper())
        }
    };
    let mut c = Command::new(&req.command);
    c.args(&req.args)
        .current_dir(&w.staging)
        .env("TMPDIR", &w.agent_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn run(w: &World, mode: Mode, args: &[&str]) -> Output {
    wrapped(w, mode, args).output().unwrap()
}

fn ok(w: &World, mode: Mode, args: &[&str]) -> bool {
    let o = run(w, mode, args);
    if !o.status.success() {
        eprintln!(
            "[{mode:?} {args:?}] exit {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    o.status.success()
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

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// The modes this host can actually run, after a smoke test. Skips loudly.
fn modes(w: &World) -> Vec<Mode> {
    let mut out = Vec::new();
    if bwrap_path().is_some() && ok(w, Mode::Bwrap, &["read", s(&w.staging.join("work.txt"))]) {
        out.push(Mode::Bwrap);
    } else {
        skip_or_fail("bubblewrap cannot run here (not installed, userns disabled or restricted)");
    }
    if landlock_status().abi().is_some()
        && ok(
            w,
            Mode::LandlockOnly,
            &["read", s(&w.staging.join("work.txt"))],
        )
    {
        out.push(Mode::LandlockOnly);
    } else {
        skip_or_fail("Landlock is not available here");
    }
    out
}

#[test]
fn filesystem_view_is_confined_and_excludes_ta() {
    let w = world();
    for mode in modes(&w) {
        assert!(unsandboxed(&["read", s(&w.ta_secret)]), "control");
        assert!(
            ok(&w, mode, &["read", s(&w.staging.join("work.txt"))]),
            "{mode:?}: workspace readable"
        );
        // Landlock cannot hide a child of a directory it grants whole (see
        // `workspace_inner_denies`), so only the mount namespace hides it.
        if matches!(mode, Mode::Bwrap) {
            assert!(
                !ok(&w, mode, &["read", s(&w.ta_secret)]),
                "{mode:?}: denied path inside the workspace must be hidden"
            );
        }
        assert!(
            !ok(
                &w,
                mode,
                &[
                    "read",
                    s(&w.staging
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("workflow.toml"))
                ]
            ),
            "{mode:?}: project .ta unreachable"
        );
        assert!(
            !ok(&w, mode, &["read", s(&w.outside_file)]),
            "{mode:?}: unlisted dirs unreadable"
        );
        assert!(
            !ok(&w, mode, &["read", "/etc/shadow"]),
            "{mode:?}: /etc/shadow unreadable"
        );
        assert!(
            ok(&w, mode, &["write", s(&w.staging.join("out.txt"))]),
            "{mode:?}: staging writable"
        );
        assert!(
            ok(&w, mode, &["write", s(&w.agent_home.join("scratch"))]),
            "{mode:?}: agent home writable"
        );
        assert!(
            !ok(&w, mode, &["write", s(&w.outside.join("new.txt"))]),
            "{mode:?}: outside not writable"
        );
        assert!(!w.outside.join("new.txt").exists());
        assert!(
            !ok(&w, mode, &["write", "/usr/ta-conformance"]),
            "{mode:?}: /usr read-only"
        );
        assert!(
            !ok(&w, mode, &["write", "/etc/ta-conformance"]),
            "{mode:?}: /etc read-only"
        );
    }
}

#[test]
fn seccomp_denies_the_named_syscalls() {
    let w = world();
    for mode in modes(&w) {
        for name in ["bpf", "keyctl", "io_uring_setup", "process_vm_readv"] {
            assert!(
                !ok(&w, mode, &["syscall", name]),
                "{mode:?}: {name} must be EPERM"
            );
        }
        assert!(!ok(&w, mode, &["ptrace"]), "{mode:?}: ptrace must be EPERM");
        assert!(
            !ok(&w, mode, &["unshare-user"]),
            "{mode:?}: nested user namespace denied"
        );
        assert!(!ok(&w, mode, &["tiocsti"]), "{mode:?}: TIOCSTI denied");
        assert!(
            ok(&w, mode, &["clone3-enosys"]),
            "{mode:?}: clone3 must fall back via ENOSYS"
        );
        // Ordinary work still functions.
        assert!(
            ok(&w, mode, &["exec", "/bin/true"]),
            "{mode:?}: exec of ordinary tools works"
        );
    }
}

#[test]
fn namespace_sandbox_hides_processes_and_network_devices() {
    let w = world();
    if !modes(&w).contains(&Mode::Bwrap) {
        return;
    }
    let pids = run(&w, Mode::Bwrap, &["countpids"]);
    let n: usize = String::from_utf8_lossy(&pids.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!(
        n <= 12,
        "private /proc should show only the sandbox's own processes, saw {n}"
    );
    let host_n = Command::new(PROBE).arg("countpids").output().unwrap();
    let host_n: usize = String::from_utf8_lossy(&host_n.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!(
        n < host_n || host_n <= 12,
        "sandbox sees {n} pids, host {host_n}"
    );
    let devs = run(&w, Mode::Bwrap, &["netdevs"]);
    assert_eq!(
        String::from_utf8_lossy(&devs.stdout).trim(),
        "lo",
        "only loopback inside the netns"
    );
}

#[test]
fn bwrap_cannot_reach_host_loopback_but_reaches_the_proxy_through_the_relay() {
    let w = world();
    if !modes(&w).contains(&Mode::Bwrap) {
        return;
    }
    let daemon = w.daemon.local_addr().unwrap().to_string();
    assert!(
        unsandboxed(&["connect", &daemon]),
        "control: daemon reachable unsandboxed"
    );
    assert!(
        !ok(&w, Mode::Bwrap, &["connect", &daemon]),
        "daemon port must be unreachable"
    );
    // The helper serves the proxy port inside the namespace and relays it out.
    let addr = format!("127.0.0.1:{}", w.proxy_port);
    assert!(
        ok(&w, Mode::Bwrap, &["echo-proxy", &addr]),
        "proxy must be reachable through the bind-mounted socket"
    );
}

#[test]
fn landlock_only_limits_tcp_to_the_proxy_port_when_the_kernel_can() {
    let w = world();
    if !modes(&w).contains(&Mode::LandlockOnly) {
        return;
    }
    let abi = landlock_status().abi().unwrap();
    let daemon = w.daemon.local_addr().unwrap().to_string();
    let proxy = format!("127.0.0.1:{}", w.proxy_port);
    if abi >= 4 {
        assert!(
            !ok(&w, Mode::LandlockOnly, &["connect", &daemon]),
            "daemon port must be refused"
        );
        assert!(
            ok(&w, Mode::LandlockOnly, &["echo-proxy", &proxy]),
            "proxy port must be allowed"
        );
    } else {
        // No kernel support: TA must say the network is not confined (capability
        // report), and the connect is not blocked. Documented, not hidden.
        eprintln!("Landlock ABI {abi} < 4: TCP not confined; capability net.loopback_blocked is not claimed");
        assert!(ok(&w, Mode::LandlockOnly, &["connect", &daemon]));
    }
}

#[test]
fn missing_landlock_is_reported_never_silent() {
    let w = world();
    if landlock_status().abi().is_some() {
        if std::env::var("TA_EXPECT_NO_LANDLOCK").as_deref() == Ok("1") {
            panic!("TA_EXPECT_NO_LANDLOCK=1 but this kernel reports Landlock");
        }
        return;
    }
    // Landlock-only must refuse to start the agent.
    let o = {
        let mut c = wrapped(&w, Mode::LandlockOnly, &["exec", "/bin/true"]);
        c.output().unwrap()
    };
    assert_eq!(
        o.status.code(),
        Some(125),
        "must refuse, not run unconfined"
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("Landlock is required"), "stderr: {err}");
    // The launcher must not have picked a Landlock provider.
    assert!(linux_probe::probe().landlock.abi().is_none());
    // Under bwrap the missing layer is announced on stderr.
    if bwrap_path().is_some() {
        let o = run(&w, Mode::Bwrap, &["exec", "/bin/true"]);
        if o.status.success() {
            assert!(
                String::from_utf8_lossy(&o.stderr).contains("landlock NOT enforced"),
                "a skipped Landlock layer must be announced: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
    }
}

#[test]
fn helper_without_a_command_exits_2_with_usage() {
    let o = Command::new(HELPER)
        .arg("--ro")
        .arg("/usr")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("usage"));
}

#[test]
fn helper_status_line_reports_the_enforced_layers() {
    let w = world();
    for mode in modes(&w) {
        let o = run(&w, mode, &["exec", "/bin/true"]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("[sandbox] linux:"), "{mode:?}: {err}");
        assert!(err.contains("seccomp enforced"), "{mode:?}: {err}");
        assert!(err.contains("landlock ABI"), "{mode:?}: {err}");
    }
}

#[test]
fn degraded_environment_is_explained_by_the_probe() {
    // Run in the degraded CI jobs: the probe must say what is wrong and what to do.
    let p = linux_probe::probe();
    if std::env::var("TA_EXPECT_NO_BWRAP").as_deref() == Ok("1") {
        assert!(
            !matches!(p.bwrap, BwrapStatus::Usable { .. }),
            "TA_EXPECT_NO_BWRAP=1 but bubblewrap works here"
        );
        let findings = p.findings().join(" ");
        assert!(
            findings.contains("bubblewrap") || findings.contains("bwrap"),
            "findings must name the problem: {findings}"
        );
        assert!(
            findings.len() > 40,
            "findings must say what to do: {findings}"
        );
    }
}
