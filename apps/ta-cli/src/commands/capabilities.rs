// capabilities.rs — `ta capabilities [--json]`: what this installation can safely do.
//
// An installer used to decide "is this `ta` safe to wire a Chief of Staff into?"
// by looking for `--chat-mode` / `--origin` in `ta run --help`, or by comparing
// version strings. Neither proves anything: a daemon can predate the flags it
// should be passing, and two builds with the same version can differ (PR #646).
//
// Every capability here is decided by running the code behind it on fixed inputs
// and checking the result, and every boolean is true only if that run succeeded.
// A flag merely existing never counts. Each answer carries an `evidence` line
// naming what was run (or why it is false).
//
// What the `ta` binary can run itself is probed in-process. What only the
// separately installed `ta-daemon` binary can vouch for (does it pass the
// restricted flags, mint role-bound outcome rights, enforce NATS credentials) is
// asked of that binary with `ta-daemon --print-capabilities`, which runs the
// daemon's own launch, token and NATS code. A daemon that is missing, too old to
// know the flag, or that fails is reported as such, with those capabilities false.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use ta_workspace::{
    CapabilitiesReport, DaemonProbe, DaemonProbeState, DaemonProbeStatus, Platform,
    SandboxProviderFact, CAPABILITIES_SCHEMA_VERSION, DAEMON_PROBE_SCHEMA_VERSION,
};

use super::chat_launch::{plan_chat_mode_launch, ChatModeInputs};

/// How long `ta-daemon --print-capabilities` may take. It starts no server and
/// only builds a few argument lists, so a healthy daemon answers in well under a second.
const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

const DAEMON_CAPABILITIES: [&str; 3] = [
    "daemon_passes_restricted_flags",
    "outcome_roles",
    "nats_credentials",
];

/// What came back from asking the daemon binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOutcome {
    pub status: DaemonProbeStatus,
    pub probe: Option<DaemonProbe>,
}

pub fn execute(json: bool) -> anyhow::Result<()> {
    let daemon = probe_daemon(
        super::version_guard::find_daemon_binary,
        DAEMON_PROBE_TIMEOUT,
    );
    let report = build_report(daemon);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

// ── Daemon-side facts ──────────────────────────────────────────────────

/// Ask the daemon binary for its facts. `locate` finds the binary (a seam for
/// tests); a missing binary, an old binary and a failing binary are three
/// different states with different fixes.
pub fn probe_daemon(
    locate: impl FnOnce() -> anyhow::Result<PathBuf>,
    timeout: Duration,
) -> DaemonOutcome {
    let binary = match locate() {
        Ok(b) => b,
        Err(e) => {
            return failed(
                DaemonProbeState::BinaryNotFound,
                None,
                format!(
                    "no ta-daemon binary was found next to `ta` or on PATH ({e}). The daemon-side \
                     capabilities are reported false. Install TA so `ta` and `ta-daemon` sit in \
                     the same directory, then run `ta capabilities` again."
                ),
            )
        }
    };
    let shown = binary.display().to_string();
    let mut child = match Command::new(&binary)
        .arg("--print-capabilities")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return failed(
                DaemonProbeState::ProbeFailed,
                Some(shown.clone()),
                format!(
                    "could not run `{shown} --print-capabilities`: {e}. Check that the file is \
                     executable and built for this machine."
                ),
            )
        }
    };
    // Drain both pipes on threads so a chatty daemon cannot block on a full pipe.
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "`{shown} --print-capabilities` did not answer within {}s and was stopped. \
                     Run it by hand to see why.",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                break Err(format!(
                    "waiting for `{shown} --print-capabilities` failed: {e}"
                ));
            }
        }
    };
    // On a timeout the killed child's own children can still hold the pipes
    // open; return without joining the reader threads so the timeout holds.
    let status = match exit {
        Ok(s) => s,
        Err(detail) => return failed(DaemonProbeState::ProbeFailed, Some(shown), detail),
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    if !status.success() {
        // clap exits 2 on an argument it does not know: a daemon that predates
        // the probe. Anything else is a real failure.
        let first_line = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if status.code() == Some(2) && stdout.trim().is_empty() {
            return failed(
                DaemonProbeState::ProbeUnsupported,
                Some(shown.clone()),
                format!(
                    "{shown} does not understand --print-capabilities, so it predates the \
                     capability probe and nothing proves what it passes to launched roles. \
                     Its capabilities are reported false. Update TA (reinstall `ta` and \
                     `ta-daemon` together) and run `ta capabilities` again. ({first_line})"
                ),
            );
        }
        return failed(
            DaemonProbeState::ProbeFailed,
            Some(shown.clone()),
            format!(
                "`{shown} --print-capabilities` exited with {} ({}). Run it by hand to see the \
                 error.",
                status
                    .code()
                    .map(|c| format!("code {c}"))
                    .unwrap_or_else(|| "a signal".to_string()),
                if first_line.is_empty() {
                    "no error output"
                } else {
                    first_line
                }
            ),
        );
    }

    let probe: DaemonProbe = match serde_json::from_str(stdout.trim()) {
        Ok(p) => p,
        Err(e) => {
            return failed(
                DaemonProbeState::ProbeFailed,
                Some(shown.clone()),
                format!(
                    "`{shown} --print-capabilities` printed something that is not a capability \
                     document ({e}). Reinstall `ta` and `ta-daemon` together."
                ),
            )
        }
    };
    if probe.probe_schema != DAEMON_PROBE_SCHEMA_VERSION {
        return failed(
            DaemonProbeState::ProbeFailed,
            Some(shown.clone()),
            format!(
                "{shown} answered with capability probe schema {} but this `ta` understands \
                 schema {}. Reinstall `ta` and `ta-daemon` together.",
                probe.probe_schema, DAEMON_PROBE_SCHEMA_VERSION
            ),
        );
    }
    DaemonOutcome {
        status: DaemonProbeStatus {
            state: DaemonProbeState::Ok,
            binary: Some(shown),
            detail: String::new(),
        },
        probe: Some(probe),
    }
}

fn failed(state: DaemonProbeState, binary: Option<String>, detail: String) -> DaemonOutcome {
    DaemonOutcome {
        status: DaemonProbeStatus {
            state,
            binary,
            detail,
        },
        probe: None,
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut p) = pipe {
            let mut bytes = Vec::new();
            let _ = p.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        text
    })
}

// ── In-process probes ──────────────────────────────────────────────────

fn parse_cli(args: &[&str]) -> Result<crate::Cli, String> {
    let argv = std::iter::once("ta").chain(args.iter().copied());
    crate::Cli::try_parse_from(argv).map_err(|e| {
        e.to_string()
            .lines()
            .next()
            .unwrap_or("parse error")
            .to_string()
    })
}

/// `ta run --chat-mode` parses to a chat-mode run AND the launch planner turns it
/// into a read-only launch for the built-in claude-code agent.
fn probe_chat_mode() -> Result<String, String> {
    let cli = parse_cli(&["run", "capability probe", "--chat-mode"])?;
    if !matches!(
        cli.command,
        Some(crate::Commands::Run {
            chat_mode: true,
            ..
        })
    ) {
        return Err("`ta run --chat-mode` does not parse to a chat-mode run".to_string());
    }
    let (framework, injects_settings) = super::run::builtin_claude_code_chat_facts();
    let plan = plan_chat_mode_launch(&ChatModeInputs {
        cli_flag: true,
        persona_name: None,
        persona_chat_mode: false,
        persona_allowed_tools: &[],
        agent: "claude-code",
        agent_framework_name: framework.as_deref(),
        injects_settings,
        macro_goal: false,
        uses_pty: false,
    })
    .map_err(|e| format!("planning a `--chat-mode` launch for claude-code failed: {e}"))?
    .ok_or_else(|| "`--chat-mode` did not switch chat mode on".to_string())?;
    if plan.source != "--chat-mode" || plan.allowed_tools.is_empty() {
        return Err(format!(
            "the chat-mode plan is not usable (source {:?}, {} tools)",
            plan.source,
            plan.allowed_tools.len()
        ));
    }
    Ok(format!(
        "parsed `ta run --chat-mode` and planned a read-only claude-code launch with {} chat-safe tools",
        plan.allowed_tools.len()
    ))
}

/// `ta run --origin` parses, validates names, stamps the origin on a new goal,
/// and treats `cos`/`chat` as never auto-approved.
fn probe_origin() -> Result<String, String> {
    use ta_goal::origin::{origin_blocks_auto_approve, origin_to_stamp, validate_origin};
    let cli = parse_cli(&["run", "capability probe", "--origin", "cos"])?;
    match &cli.command {
        Some(crate::Commands::Run {
            origin: Some(o), ..
        }) if o == "cos" => {}
        _ => return Err("`ta run --origin cos` does not carry the origin".to_string()),
    }
    if validate_origin("cos").is_err() {
        return Err("origin `cos` is rejected by validation".to_string());
    }
    if validate_origin("Not A Valid Origin").is_ok() {
        return Err("an invalid origin name was accepted".to_string());
    }
    if !origin_blocks_auto_approve(Some("cos")) || origin_blocks_auto_approve(Some("worker")) {
        return Err("`cos` is not the only kind of origin that blocks auto-approve".to_string());
    }
    if origin_to_stamp(None, Some("cos")).as_deref() != Some("cos")
        || origin_to_stamp(Some("chat"), Some("cos")).is_some()
    {
        return Err("a goal's origin is not stamped once and then kept".to_string());
    }
    Ok(
        "parsed `ta run --origin cos`, validated names, stamped a new goal once and \
        kept it, and `cos` blocks auto-approve"
            .to_string(),
    )
}

/// A shipped posture compiles into a rules table that denies an unattended
/// action nothing allows.
fn probe_rules_engine() -> Result<String, String> {
    use ta_actions::rules::{Request, RulesLoader, When};
    let scratch =
        tempfile::tempdir().map_err(|e| format!("cannot create a scratch project: {e}"))?;
    let loader = RulesLoader::with_dirs(scratch.path(), None, None);
    let table = loader
        .compile_text("version = 1\nposture = \"strict\"\n", None)
        .map_err(|problems| {
            format!(
                "the strict posture did not compile: {}",
                problems.join("; ")
            )
        })?;
    let unlisted = table.decide(&Request::new("capability_probe_unlisted", When::Unattended));
    if !unlisted.is_deny() {
        return Err(format!(
            "an unattended action no rule allows was decided {:?}, not deny",
            unlisted.effect
        ));
    }
    Ok(format!(
        "compiled the strict posture and decided an unattended unlisted action: deny ({})",
        unlisted.rule
    ))
}

/// Apply journal: prior values are saved, and rolling back restores an
/// overwritten file and removes a newly created one.
fn probe_atomic_apply() -> Result<String, String> {
    let scratch =
        tempfile::tempdir().map_err(|e| format!("cannot create a scratch project: {e}"))?;
    let target = scratch.path().join("project");
    let journals = scratch.path().join("journals");
    let staged = scratch.path().join("staged.txt");
    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    std::fs::write(target.join("a.txt"), "before").map_err(|e| e.to_string())?;
    std::fs::write(&staged, "after").map_err(|e| e.to_string())?;

    let mut journal =
        ta_actions::ApplyJournal::begin(&journals, "probe", &target, Vec::new(), None)
            .map_err(|e| format!("could not open an apply journal: {e}"))?;
    journal
        .install("a.txt", &staged)
        .and_then(|_| journal.install("b.txt", &staged))
        .map_err(|e| format!("could not install through the journal: {e}"))?;
    let installed = std::fs::read_to_string(target.join("a.txt")).map_err(|e| e.to_string())?;
    if installed != "after" {
        return Err("the journal did not install the new file".to_string());
    }
    journal.abandon_after_rollback();
    let restored = std::fs::read_to_string(target.join("a.txt")).map_err(|e| e.to_string())?;
    if restored != "before" {
        return Err("rolling back did not restore the overwritten file".to_string());
    }
    if target.join("b.txt").exists() {
        return Err("rolling back did not remove the file the apply created".to_string());
    }
    if journals.join("probe").exists() {
        return Err("rolling back left the journal behind".to_string());
    }
    Ok(
        "journaled an overwrite and a creation, rolled back, and found the original file \
        restored, the new file gone and the journal closed"
            .to_string(),
    )
}

/// `ta terms-status --json` is a real command and this build embeds a versioned,
/// hashed terms document.
fn probe_terms_status() -> Result<String, String> {
    let cli = parse_cli(&["terms-status", "--json"])?;
    if !matches!(
        cli.command,
        Some(crate::Commands::TermsStatus { json: true })
    ) {
        return Err("`ta terms-status --json` does not parse".to_string());
    }
    let (version, sha256) = super::terms::document_facts();
    if version.is_empty()
        || sha256.len() != 64
        || !sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(format!(
            "the terms document has no usable version/hash (version {version:?}, sha256 {sha256:?})"
        ));
    }
    Ok(format!(
        "`ta terms-status --json` parses; the embedded terms document is version {version}, sha256 {sha256}"
    ))
}

/// The sandbox provider this machine has, and what it enforces for a worker role.
fn sandbox_providers() -> Vec<SandboxProviderFact> {
    use ta_runtime::sandbox_launch::{
        provider_capabilities, provider_name, representative_profile, RoleClass, SandboxPosture,
    };
    let provider = ta_runtime::SandboxPolicy::detect_provider();
    if provider == ta_runtime::SandboxProvider::None {
        return Vec::new();
    }
    let profile = representative_profile(RoleClass::Worker, SandboxPosture::Balanced);
    vec![SandboxProviderFact {
        name: provider_name(&provider).to_string(),
        enforces: provider_capabilities(&provider, &profile)
            .into_iter()
            .map(|c| c.name().to_string())
            .collect(),
    }]
}

// ── The report ─────────────────────────────────────────────────────────

/// Run every in-process probe and combine with what the daemon said.
pub fn build_report(daemon: DaemonOutcome) -> CapabilitiesReport {
    let mut evidence: BTreeMap<String, String> = BTreeMap::new();
    let mut verdict = |name: &str, result: Result<String, String>| -> bool {
        match result {
            Ok(what) => {
                evidence.insert(name.to_string(), what);
                true
            }
            Err(why) => {
                evidence.insert(name.to_string(), format!("not wired: {why}"));
                false
            }
        }
    };
    let supports_chat_mode = verdict("supports_chat_mode", probe_chat_mode());
    let supports_origin = verdict("supports_origin", probe_origin());
    let rules_engine = verdict("rules_engine", probe_rules_engine());
    let atomic_apply = verdict("atomic_apply", probe_atomic_apply());
    let terms_status = verdict("terms_status", probe_terms_status());
    let sandbox_providers = sandbox_providers();
    evidence.insert(
        "sandbox_providers".to_string(),
        match sandbox_providers.first() {
            Some(p) => format!(
                "detected {} enforcing {} capabilities for a worker role",
                p.name,
                p.enforces.len()
            ),
            None => "no sandbox provider is usable on this machine".to_string(),
        },
    );

    let (restricted, outcome_roles, nats) = match &daemon.probe {
        Some(p) => {
            for name in DAEMON_CAPABILITIES {
                if let Some(line) = p.evidence.get(name) {
                    evidence.insert(name.to_string(), format!("ta-daemon: {line}"));
                }
            }
            (
                p.passes_restricted_flags,
                p.outcome_roles,
                p.nats_credentials,
            )
        }
        None => {
            for name in DAEMON_CAPABILITIES {
                evidence.insert(
                    name.to_string(),
                    format!("unknown, reported false: {}", daemon.status.detail),
                );
            }
            (false, false, false)
        }
    };
    // `ta run` must also act on the flags, or a daemon that passes them is moot.
    let daemon_passes_restricted_flags = restricted && supports_chat_mode && supports_origin;
    if restricted && !daemon_passes_restricted_flags {
        evidence.insert(
            "daemon_passes_restricted_flags".to_string(),
            "the daemon passes the flags but this `ta run` does not act on them (see \
             supports_chat_mode and supports_origin)"
                .to_string(),
        );
    }

    CapabilitiesReport {
        schema_version: CAPABILITIES_SCHEMA_VERSION,
        capabilities_schema: CAPABILITIES_SCHEMA_VERSION,
        ta_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: Platform::current(),
        daemon_passes_restricted_flags,
        supports_chat_mode,
        supports_origin,
        rules_engine,
        outcome_roles,
        nats_credentials: nats,
        terms_status,
        atomic_apply,
        sandbox_providers,
        daemon_probe: daemon.status,
        evidence,
    }
}

/// The human summary.
pub fn render_text(r: &CapabilitiesReport) -> String {
    let mut out = format!(
        "TA capabilities (schema {}) — ta {} on {}/{}\n\n",
        r.capabilities_schema, r.ta_version, r.platform.os, r.platform.arch
    );
    let rows: [(&str, bool); 8] = [
        (
            "daemon_passes_restricted_flags",
            r.daemon_passes_restricted_flags,
        ),
        ("supports_chat_mode", r.supports_chat_mode),
        ("supports_origin", r.supports_origin),
        ("rules_engine", r.rules_engine),
        ("outcome_roles", r.outcome_roles),
        ("nats_credentials", r.nats_credentials),
        ("terms_status", r.terms_status),
        ("atomic_apply", r.atomic_apply),
    ];
    for (name, on) in rows {
        out.push_str(&format!(
            "  {} {:<32} {}\n",
            if on { "yes" } else { "NO " },
            name,
            r.evidence.get(name).map(String::as_str).unwrap_or("")
        ));
    }
    out.push_str(&format!(
        "\nSandbox providers: {}\n",
        if r.sandbox_providers.is_empty() {
            "none usable on this machine".to_string()
        } else {
            r.sandbox_providers
                .iter()
                .map(|p| format!("{} (enforces {})", p.name, p.enforces.join(", ")))
                .collect::<Vec<_>>()
                .join("; ")
        }
    ));
    match r.daemon_probe.state {
        DaemonProbeState::Ok => out.push_str(&format!(
            "Daemon probe: ok ({})\n",
            r.daemon_probe.binary.as_deref().unwrap_or("?")
        )),
        _ => out.push_str(&format!(
            "Daemon probe: {:?} — {}\n",
            r.daemon_probe.state, r.daemon_probe.detail
        )),
    }
    out.push_str("\nFor scripts: `ta capabilities --json`.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::path::Path;

    /// `Commands` is large; clap builds it on the stack, so every test that
    /// parses runs on a big-stack thread (see `verb_dispatch_tests` in main.rs).
    fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(f)
            .expect("spawn probe thread")
            .join()
            .expect("probe thread panicked")
    }

    fn probe(restricted: bool, outcome: bool, nats: bool) -> DaemonProbe {
        DaemonProbe {
            probe_schema: DAEMON_PROBE_SCHEMA_VERSION,
            daemon_version: "9.9.9".to_string(),
            passes_restricted_flags: restricted,
            outcome_roles: outcome,
            nats_credentials: nats,
            evidence: DAEMON_CAPABILITIES
                .iter()
                .map(|n| (n.to_string(), format!("ran {n}")))
                .collect(),
        }
    }

    fn answered(p: DaemonProbe) -> DaemonOutcome {
        DaemonOutcome {
            status: DaemonProbeStatus {
                state: DaemonProbeState::Ok,
                binary: Some("/fake/ta-daemon".to_string()),
                detail: String::new(),
            },
            probe: Some(p),
        }
    }

    fn report(daemon: DaemonOutcome) -> CapabilitiesReport {
        on_big_stack(move || build_report(daemon))
    }

    #[test]
    fn this_build_passes_every_in_process_probe() {
        let r = report(answered(probe(true, true, true)));
        for (name, on) in [
            ("supports_chat_mode", r.supports_chat_mode),
            ("supports_origin", r.supports_origin),
            ("rules_engine", r.rules_engine),
            ("atomic_apply", r.atomic_apply),
            ("terms_status", r.terms_status),
        ] {
            assert!(on, "{name}: {:?}", r.evidence.get(name));
        }
        assert_eq!(r.capabilities_schema, CAPABILITIES_SCHEMA_VERSION);
        assert_eq!(r.schema_version, r.capabilities_schema);
        assert_eq!(r.ta_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(r.platform, Platform::current());
        assert!(r.daemon_passes_restricted_flags && r.outcome_roles && r.nats_credentials);
    }

    /// The PR #646 failure: nothing proves the daemon passes the flags, so the
    /// capability is false however new the `ta` binary is.
    #[test]
    fn daemon_side_capabilities_are_false_whenever_the_daemon_cannot_vouch() {
        for state in [
            DaemonProbeState::BinaryNotFound,
            DaemonProbeState::ProbeUnsupported,
            DaemonProbeState::ProbeFailed,
        ] {
            let r = report(failed(state, None, "the reason".to_string()));
            assert!(!r.daemon_passes_restricted_flags, "{state:?}");
            assert!(!r.outcome_roles, "{state:?}");
            assert!(!r.nats_credentials, "{state:?}");
            // The `ta` side is still reported on its own merits.
            assert!(r.supports_chat_mode && r.supports_origin, "{state:?}");
            assert_eq!(r.daemon_probe.state, state);
            for name in DAEMON_CAPABILITIES {
                let line = &r.evidence[name];
                assert!(
                    line.contains("reported false") && line.contains("the reason"),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn each_daemon_capability_follows_the_daemon_answer_independently() {
        let r = report(answered(probe(true, false, true)));
        assert!(r.daemon_passes_restricted_flags);
        assert!(!r.outcome_roles);
        assert!(r.nats_credentials);
        let r = report(answered(probe(false, true, false)));
        assert!(!r.daemon_passes_restricted_flags);
        assert!(r.outcome_roles);
        assert!(!r.nats_credentials);
        assert_eq!(r.evidence["outcome_roles"], "ta-daemon: ran outcome_roles");
    }

    #[test]
    fn json_document_has_the_documented_fields_and_round_trips() {
        let r = report(answered(probe(true, true, true)));
        let text = serde_json::to_string_pretty(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for key in [
            "schema_version",
            "capabilities_schema",
            "daemon_passes_restricted_flags",
            "supports_chat_mode",
            "supports_origin",
            "rules_engine",
            "outcome_roles",
            "nats_credentials",
            "terms_status",
            "atomic_apply",
            "sandbox_providers",
            "platform",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert!(v["platform"]["os"].is_string() && v["platform"]["arch"].is_string());
        assert!(v["supports_chat_mode"].is_boolean());
        let back: CapabilitiesReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn text_summary_says_no_and_what_to_do_when_the_daemon_is_missing() {
        let r = report(failed(
            DaemonProbeState::BinaryNotFound,
            None,
            "no ta-daemon binary was found; install TA".to_string(),
        ));
        let text = render_text(&r);
        assert!(
            text.contains("NO  daemon_passes_restricted_flags"),
            "{text}"
        );
        assert!(text.contains("yes supports_chat_mode"), "{text}");
        assert!(
            text.contains("BinaryNotFound") && text.contains("install TA"),
            "{text}"
        );
        assert!(text.contains("ta capabilities --json"));
    }

    #[test]
    fn a_missing_binary_is_reported_as_not_found_with_a_fix() {
        let out = probe_daemon(
            || Err(anyhow::anyhow!("not on PATH")),
            Duration::from_secs(1),
        );
        assert_eq!(out.status.state, DaemonProbeState::BinaryNotFound);
        assert!(out.probe.is_none());
        assert!(out.status.detail.contains("not on PATH"));
        assert!(out.status.detail.contains("Install TA"));
    }

    #[test]
    fn a_binary_that_cannot_be_run_is_a_failed_probe() {
        let dir = tempfile::tempdir().unwrap();
        let not_executable = dir.path().join("ta-daemon");
        std::fs::write(&not_executable, "not a program").unwrap();
        let out = probe_daemon(|| Ok(not_executable), Duration::from_secs(1));
        assert_eq!(out.status.state, DaemonProbeState::ProbeFailed);
        assert!(
            out.status.detail.contains("could not run"),
            "{}",
            out.status.detail
        );
    }

    #[cfg(unix)]
    fn fake_daemon(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ta-daemon");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn run_fake(body: &str, timeout: Duration) -> DaemonOutcome {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_daemon(dir.path(), body);
        probe_daemon(|| Ok(bin), timeout)
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_answers_is_ok_and_its_facts_are_used() {
        let json = serde_json::to_string(&probe(true, true, false)).unwrap();
        let out = run_fake(
            &format!("cat <<'EOF'\n{json}\nEOF"),
            Duration::from_secs(10),
        );
        assert_eq!(out.status.state, DaemonProbeState::Ok, "{:?}", out.status);
        let p = out.probe.unwrap();
        assert!(p.passes_restricted_flags && p.outcome_roles && !p.nats_credentials);
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_rejects_the_flag_predates_the_probe() {
        let out = run_fake(
            "echo \"error: unexpected argument '--print-capabilities' found\" >&2; exit 2",
            Duration::from_secs(10),
        );
        assert_eq!(out.status.state, DaemonProbeState::ProbeUnsupported);
        assert!(out.probe.is_none());
        assert!(
            out.status.detail.contains("Update TA"),
            "{}",
            out.status.detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn crashes_garbage_and_unknown_schemas_are_failed_probes() {
        let out = run_fake("echo boom >&2; exit 1", Duration::from_secs(10));
        assert_eq!(out.status.state, DaemonProbeState::ProbeFailed);
        assert!(out.status.detail.contains("code 1") && out.status.detail.contains("boom"));

        let out = run_fake("echo 'hello'", Duration::from_secs(10));
        assert_eq!(out.status.state, DaemonProbeState::ProbeFailed);
        assert!(out.status.detail.contains("not a capability document"));

        let mut future = probe(true, true, true);
        future.probe_schema = DAEMON_PROBE_SCHEMA_VERSION + 1;
        let json = serde_json::to_string(&future).unwrap();
        let out = run_fake(
            &format!("cat <<'EOF'\n{json}\nEOF"),
            Duration::from_secs(10),
        );
        assert_eq!(out.status.state, DaemonProbeState::ProbeFailed);
        assert!(
            out.status.detail.contains("schema"),
            "{}",
            out.status.detail
        );
        assert!(out.probe.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_hangs_is_stopped_at_the_timeout() {
        let started = Instant::now();
        let out = run_fake("sleep 30", Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(out.status.state, DaemonProbeState::ProbeFailed);
        assert!(
            out.status.detail.contains("did not answer"),
            "{}",
            out.status.detail
        );
    }
}
