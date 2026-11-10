//! Seatbelt v2 conformance suite (v0.17.12.7).
//!
//! Runs `ta-sandbox-probe` inside the generated profile and checks each
//! allow/deny against the policy. macOS only. A host that cannot nest or run
//! `sandbox-exec` (for example an already sandboxed agent) skips the suite
//! unless `TA_REQUIRE_SANDBOX_CONFORMANCE=1`, which CI sets so a skip is a failure.
//!
//! Reads are NOT confined on macOS by default (read-allow with targeted denies); the
//! tests assert only the denies the profile claims, and that is all the docs claim.
//! A chat-mode profile opts into confining the user's home (`read_confine`); its
//! tests assert exactly that and the stated limit outside the home.
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
        goal_read_files: vec![],
        unix_sockets: vec![sock.clone()],
        egress_proxy_port: Some(proxy.local_addr().unwrap().port()),
        egress_unix_socket: None,
        windows: Default::default(),
        violation_tag: Some(seatbelt::violation_tag(&uuid::Uuid::new_v4().to_string())),
        read_confine: Default::default(),
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

/// Start a tail for the world's goal tag and prove the live stream is
/// attached. `None` (after saying why) when this host's unified log never
/// delivers even a denial TA causes on purpose; `TA_REQUIRE_SANDBOX_CONFORMANCE=1`
/// turns that into a failure.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<ta_runtime::seatbelt_log::Violation>>>;

fn live_tail(
    w: &World,
) -> Option<(
    ta_runtime::seatbelt_log::ViolationTail,
    Seen,
    ta_runtime::seatbelt_log::Canary,
)> {
    use std::sync::{Arc, Mutex};
    use ta_runtime::seatbelt_log::ViolationTail;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let tag = w.policy.violation_tag.clone().unwrap();
    let Ok(tail) = ViolationTail::start(tag, Arc::new(move |v| sink.lock().unwrap().push(v)))
    else {
        eprintln!("SKIP: /usr/bin/log unavailable");
        return None;
    };
    let start = tail.probe(std::time::Duration::from_secs(30));
    if !start.delivered() {
        let msg = format!(
            "the unified log delivered no canary denial in 30s ({start:?}, {} raw lines): \
             sandbox denial capture is unavailable on this host",
            tail.lines_seen()
        );
        assert!(
            std::env::var("TA_REQUIRE_SANDBOX_CONFORMANCE").as_deref() != Ok("1"),
            "{msg}, but TA_REQUIRE_SANDBOX_CONFORMANCE=1 requires it"
        );
        eprintln!("SKIP: {msg}");
        return None;
    }
    Some((tail, seen, start))
}

fn ssh_denial_seen(seen: &std::sync::Mutex<Vec<ta_runtime::seatbelt_log::Violation>>) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|v| v.target.contains(".ssh") && v.operation.starts_with("file-read"))
        .count()
}

/// The macOS unified log is lossy (see `ViolationTail`): a single denial can
/// be dropped by the live stream or never logged by the OS at all. Delivery is
/// therefore made deterministic by retrying the denial until the tail has it,
/// the same way the pre-spawn canary does. 50 independent denials must all be
/// delivered; the attempt histogram is printed so a regression in the pipeline
/// (more retries per delivery) is visible in the test output.
#[test]
fn fifty_denials_are_all_delivered_with_bounded_retries() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let Some((tail, seen, _start)) = live_tail(&w) else {
        return;
    };
    let mut histogram = std::collections::BTreeMap::<u32, u32>::new();
    for run in 1..=50 {
        let before = ssh_denial_seen(&seen);
        let mut delivered_after = None;
        for attempt in 1..=10u32 {
            // Enforcement is the hard assertion: every read must be refused.
            assert!(
                !sandboxed(&w, &["read", s(&w.ssh_key)]),
                "run {run}: the read must be denied"
            );
            let wait_until = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            while std::time::Instant::now() < wait_until {
                if ssh_denial_seen(&seen) > before {
                    delivered_after = Some(attempt);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            if delivered_after.is_some() {
                break;
            }
        }
        let attempts = delivered_after.unwrap_or_else(|| {
            panic!(
                "run {run}: no tagged denial reached the tail in 10 attempts ({} raw Sandbox \
                 lines seen); delivery is not dependable on this host",
                tail.lines_seen()
            )
        });
        *histogram.entry(attempts).or_default() += 1;
    }
    eprintln!("denial delivery, attempts needed -> runs: {histogram:?}");
}

/// A whole capture: probe, one denial with NO retry, finish. What the OS kept
/// is recovered from the persisted log, and the report says whether capture
/// was verified; it must never claim health it did not check.
#[test]
fn finish_reconciles_with_the_persisted_log_and_reports_capture_health() {
    let w = world();
    if !can_run(&w) {
        return;
    }
    let Some((tail, seen, start)) = live_tail(&w) else {
        return;
    };
    assert!(!sandboxed(&w, &["read", s(&w.ssh_key)]));
    let report = tail.finish(start, std::time::Duration::from_secs(30));
    eprintln!("{}", report.summary());
    // A denial is never counted twice: the live and persisted copies are one.
    let tagged = ssh_denial_seen(&seen) as u64;
    assert_eq!(tagged, report.live_denials + report.recovered_denials);
    assert!(tagged <= 1, "one refused read, {tagged} reported");
    if report.healthy() {
        assert!(report.reconciled && report.end.delivered() && report.start.delivered());
    } else {
        assert!(
            !report.problems.is_empty() && report.summary().contains("NOT verified"),
            "{report:?}"
        );
    }
}

/// Refused-read occurrences of the `.ssh` key that `log show` holds for `tag`
/// since `since`: the ground truth for what the OS logged.
fn persisted_occurrences(tag: &str, since: chrono::DateTime<chrono::Local>) -> u64 {
    let out = Command::new("/usr/bin/log")
        .args([
            "show",
            "--style",
            "ndjson",
            "--start",
            &since.format("%Y-%m-%d %H:%M:%S").to_string(),
            "--predicate",
            &format!(r#"sender == "Sandbox" AND eventMessage CONTAINS "{tag}""#),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("/usr/bin/log show must run");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| ta_runtime::seatbelt_log::parse_ndjson_line(l, tag))
        .filter(|v| v.target.contains(".ssh") && v.operation.starts_with("file-read"))
        .map(|v| u64::from(v.count))
        .sum()
}

/// Delivery under a burst with NO per-denial retry: the case the live stream
/// alone gets wrong. Each round makes 100 refused reads back to back, then
/// finishes the capture. `report.live_denials` is what the live stream alone
/// delivered (the behaviour before the persisted-log reconciliation existed);
/// `live + recovered` is what `finish` delivers now. Both numbers are printed
/// so a regression is visible in the test output. `persisted` is the ground
/// truth read straight from `log show`: delivery must cover all of it. A burst
/// can still fall short of 100 when the OS itself never logged some denials;
/// that shortfall shows as `persisted < 100` and cannot be recovered.
#[test]
fn burst_of_hundred_denials_is_delivered_after_reconciliation() {
    const ROUNDS: usize = 3;
    const BURST: usize = 100;
    let mut live_only = Vec::new();
    let mut delivered = Vec::new();
    let mut persisted_all = Vec::new();
    for round in 1..=ROUNDS {
        let since = chrono::Local::now() - chrono::Duration::seconds(2);
        let w = world();
        if !can_run(&w) {
            return;
        }
        let Some((tail, seen, start)) = live_tail(&w) else {
            return;
        };
        for i in 1..=BURST {
            assert!(
                !sandboxed(&w, &["read", s(&w.ssh_key)]),
                "round {round} read {i}: the read must be denied"
            );
        }
        let report = tail.finish(start, std::time::Duration::from_secs(60));
        // Ground truth, read independently of TA's tail: what `log show` holds now
        // for this goal's tag. It tells a denial the OS never logged apart from
        // one the tail failed to deliver.
        let persisted = persisted_occurrences(w.policy.violation_tag.as_deref().unwrap(), since);
        eprintln!("round {round}: the persisted log holds {persisted} of {BURST} denials");
        persisted_all.push(persisted);
        eprintln!("round {round}: {}", report.summary());
        // Repeats the kernel collapsed ("N duplicate reports") stand for several
        // refusals, so compare occurrences, not report lines.
        let live = report.live_occurrences;
        let total = report.live_occurrences + report.recovered_occurrences;
        // No denial is delivered twice: the sink saw exactly what the report counts.
        assert_eq!(
            ssh_denial_seen(&seen) as u64,
            report.live_denials + report.recovered_denials
        );
        assert!(
            total <= BURST as u64,
            "round {round}: {total} occurrences for {BURST} refused reads"
        );
        assert!(
            total >= persisted,
            "round {round}: the persisted log holds {persisted} occurrences but only {total} \
             were delivered: a denial the OS logged was lost by TA's capture"
        );
        live_only.push(live);
        delivered.push(total);
    }
    eprintln!(
        "burst of {BURST} x {ROUNDS} rounds, occurrences delivered: live stream alone \
         {live_only:?}; after reconciliation {delivered:?}; persisted log holds {persisted_all:?}"
    );
}

/// Where the chat world's files live.
struct ChatPaths {
    project: PathBuf,
    /// A second project outside the user's home, allowed by name.
    other: PathBuf,
    /// `<home>/notes`, allowed by `[chat.read] allow`.
    notes: PathBuf,
    /// `<home>/notes/private`, hidden by `[chat.read] deny`.
    private: PathBuf,
    /// `<home>/unlisted`: inside the home, named nowhere.
    unlisted: PathBuf,
    /// `<home>/.local/bin`: a toolchain location re-allowed inside the confined home.
    toolchain: PathBuf,
    /// Not under the confined home and not listed.
    outside_home: PathBuf,
}

/// The profile a chat-mode Chief of Staff gets for the REAL project root (no copy):
/// the policy is compiled by the SAME `ta_policy::read_policy` engine the gateway and
/// the biscuit grant use (`allow` = the notes folder and another project, `deny` = a
/// subtree of the notes folder), and the profile by the same code `ta run` uses,
/// including the macOS home read confinement.
fn chat_world() -> (World, ChatPaths) {
    use ta_policy::read_policy::{ReadEnv, ReadPolicy};
    use ta_runtime::sandbox_launch::{
        chat_read_confinement, LaunchContext, ProjectRequest, RoleClass, SandboxPosture,
        SandboxProfile,
    };
    let mut w = world();
    let project = w.staging.ancestors().nth(3).unwrap().to_path_buf();
    let base = project.parent().unwrap().to_path_buf();
    let home = base.join("home");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::write(project.join("README.md"), "real project file").unwrap();
    std::fs::write(project.join("src/lib.rs"), "fn main() {}").unwrap();
    std::fs::write(project.join(".git/config"), "git internals").unwrap();
    let other = base.join("other-project");
    std::fs::create_dir_all(other.join(".ta")).unwrap();
    std::fs::write(other.join("notes.md"), "other project file").unwrap();
    std::fs::write(other.join(".ta/workflow.toml"), "other state").unwrap();
    let notes = home.join("notes");
    let private = notes.join("private");
    let unlisted = home.join("unlisted");
    let toolchain = home.join(".local/bin");
    let outside_home = base.join("outside");
    for d in [&private, &unlisted, &toolchain] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(notes.join("todo.md"), "buy milk").unwrap();
    std::fs::write(private.join("diary.md"), "private diary").unwrap();
    std::fs::write(unlisted.join("x.txt"), "unlisted file").unwrap();
    std::fs::write(toolchain.join("agent-runtime"), "toolchain file").unwrap();
    std::fs::write(outside_home.join("y.txt"), "outside file").unwrap();

    let policy = ReadPolicy::from_config(
        &ReadEnv {
            project_root: project.clone(),
            home: Some(home.clone()),
            state_dir: Some(base.join("state")),
            office_yaml: base.join("no-office.yaml"),
        },
        &[other.display().to_string(), notes.display().to_string()],
        &[private.display().to_string()],
    )
    .unwrap();
    let view = policy.sandbox_view();

    let ctx = LaunchContext {
        goal_id: "chat-g1".into(),
        role_class: RoleClass::Chat,
        team_role: false,
        project_root: project.clone(),
        staging: w.staging.clone(),
        state_dir: Some(base.join("state")),
        agent_home: w.agent_home.clone(),
        user_home: Some(home.clone()),
        mediation_socket: None,
        egress_proxy: false,
        persona: None,
        prompt_file: None,
    };
    let request = ProjectRequest {
        posture: Some(SandboxPosture::Balanced),
        ..Default::default()
    };
    let (roots, allow) = chat_read_confinement(Some(&home));
    assert_eq!(roots, vec![home.clone()], "macOS confines the home");
    let profile =
        SandboxProfile::compile(None, &request, &ctx).with_read_policy(&view, &roots, &allow);
    assert!(
        profile.read_roots.len() == 3,
        "project, other project and notes must be accepted: {:?}",
        profile.warnings
    );
    let mut policy = profile.to_policy(SandboxProvider::MacosSandboxExec);
    policy.egress_proxy_port = w.policy.egress_proxy_port;
    w.policy = policy;
    (
        w,
        ChatPaths {
            project,
            other,
            notes,
            private,
            unlisted,
            toolchain,
            outside_home,
        },
    )
}

#[test]
fn chat_session_reads_the_real_project_but_cannot_write_or_see_ta_state() {
    let (w, paths) = chat_world();
    let ChatPaths { project, other, .. } = paths;
    if !can_run(&w) {
        return;
    }
    // Reads: project files and the readable project's files work (and the probe proves
    // the control: the same files are readable unsandboxed).
    for f in [
        project.join("README.md"),
        project.join("src/lib.rs"),
        other.join("notes.md"),
    ] {
        assert!(unsandboxed(&["read", s(&f)]), "control {}", f.display());
        assert!(
            sandboxed(&w, &["read", s(&f)]),
            "{} must be readable",
            f.display()
        );
    }
    // Writes: no write to an existing project file, no new file, nothing in the other
    // project, and the files are unchanged.
    for f in [
        project.join("README.md"),
        project.join("src/lib.rs"),
        project.join("new-file.txt"),
        other.join("notes.md"),
        other.join("new-file.txt"),
    ] {
        assert!(
            !sandboxed(&w, &["write", s(&f)]),
            "{} must not be writable",
            f.display()
        );
    }
    assert!(!project.join("new-file.txt").exists());
    assert!(!other.join("new-file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("README.md")).unwrap(),
        "real project file"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("notes.md")).unwrap(),
        "other project file"
    );
    // The session's own directory and agent home are the only writable places.
    assert!(sandboxed(&w, &["write", s(&w.staging.join("scratch.txt"))]));
    assert!(sandboxed(
        &w,
        &["write", s(&w.agent_home.join("scratch.txt"))]
    ));
    // The deny list still wins inside the readable roots and at the home directory.
    let ta_file = project.join(".ta/workflow.toml");
    std::fs::write(&ta_file, "[plan.wayfinder]").unwrap();
    assert!(unsandboxed(&["read", s(&ta_file)]));
    assert!(
        !sandboxed(&w, &["read", s(&ta_file)]),
        "the project's .ta must stay unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&other.join(".ta/workflow.toml"))]),
        "a readable project's .ta must stay unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&project.join(".git/config"))]),
        "the project's .git must stay unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&w.ssh_key)]),
        "~/.ssh must stay unreadable"
    );
    assert!(
        !sandboxed(&w, &["read", s(&w.state_secret)]),
        "the TA state dir must stay unreadable"
    );
}

/// The read allow/deny lists through the real OS sandbox, on the profile `ta run`
/// builds: an allowed path outside the project root is readable, a denied subtree of
/// it is not, an unlisted path inside the confined home is not, the built-in
/// protected locations are not, a toolchain location inside the home still is, and
/// nothing is writable anywhere a read was granted.
#[test]
fn chat_read_allow_and_deny_lists_hold_under_the_real_sandbox() {
    let (w, p) = chat_world();
    if !can_run(&w) {
        return;
    }
    let readable = |f: &Path| sandboxed(&w, &["read", s(f)]);
    // Allowed outside the project root: control, then the sandbox.
    let todo = p.notes.join("todo.md");
    assert!(unsandboxed(&["read", s(&todo)]), "control");
    assert!(
        readable(&todo),
        "an allowed folder outside the project must read"
    );
    assert!(
        readable(&p.project.join("README.md")),
        "the project root reads"
    );
    // Deny beats allow: the subtree of an allowed folder is hidden, its siblings not.
    let diary = p.private.join("diary.md");
    assert!(unsandboxed(&["read", s(&diary)]), "control");
    assert!(!readable(&diary), "a denied subtree must not read");
    // Unlisted: a file in the confined home that no entry names.
    let unlisted = p.unlisted.join("x.txt");
    assert!(unsandboxed(&["read", s(&unlisted)]), "control");
    assert!(
        !readable(&unlisted),
        "an unlisted path in the home must not read"
    );
    // Built-in protected locations stay unreadable whatever the lists say.
    assert!(!readable(&w.ssh_key), "~/.ssh");
    assert!(!readable(&w.npmrc), "~/.npmrc");
    assert!(!readable(&w.state_secret), "the TA state dir");
    assert!(
        !readable(&p.project.join(".git/config")),
        "the project .git"
    );
    // The toolchain locations of the confined home still read, so the agent starts.
    assert!(
        readable(&p.toolchain.join("agent-runtime")),
        "toolchain dir"
    );
    assert!(sandboxed(&w, &["exec", "/bin/sh", "-c", "exit 0"]));
    assert!(sandboxed(&w, &["exec", "/usr/bin/git", "--version"]));
    // Known limit, asserted so the docs cannot drift from the behaviour: outside the
    // confined home reads stay allow-all with targeted denies (the OS profile is the
    // second layer; the gateway tools and the token are the first).
    assert!(
        readable(&p.outside_home.join("y.txt")),
        "outside the confined home macOS reads are not confined"
    );
    // Writes are refused everywhere a read was granted (and in the denied and
    // unlisted places), and nothing changed on disk.
    for f in [
        p.notes.join("todo.md"),
        p.notes.join("new.txt"),
        diary.clone(),
        unlisted.clone(),
        p.project.join("README.md"),
        p.project.join("new.txt"),
        p.other.join("notes.md"),
        p.other.join("new.txt"),
        p.toolchain.join("agent-runtime"),
        p.outside_home.join("new.txt"),
    ] {
        assert!(
            !sandboxed(&w, &["write", s(&f)]),
            "{} must not be writable",
            f.display()
        );
    }
    assert!(!p.notes.join("new.txt").exists());
    assert!(!p.project.join("new.txt").exists());
    assert!(!p.other.join("new.txt").exists());
    assert_eq!(std::fs::read_to_string(&todo).unwrap(), "buy milk");
    // The session's own directory is still its workspace.
    assert!(sandboxed(&w, &["write", s(&w.staging.join("scratch.txt"))]));
}
