//! `ta sandbox`: see, check and pin the box agents run in (v0.17.12.10).
//!
//! * `explain [goal-id | role]` prints the recorded sandbox decision and any
//!   denials for a goal, or the profile a role class gets on this machine.
//! * `test [--role <role>]` launches a harmless probe inside the real provider
//!   and checks that it can write where it should and nowhere else.
//! * `pin-persona <name>` records the owner's approval of a persona's
//!   `[sandbox]` table. It needs a human credential, like `ta rules sign`.
//!
//! Read-only commands never change anything. Pinning is the one write, and it
//! lands in the trusted state directory the agent cannot reach.

use std::path::Path;

use anyhow::{bail, Context};
use clap::Subcommand;
use ta_events::store::EventQueryFilter;
use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
use ta_mcp_gateway::GatewayConfig;
use ta_runtime::sandbox_launch::{
    degraded_label, readiness, representative_profile, Outcome, RoleClass, SandboxPosture,
};
use ta_runtime::SandboxPolicy;
#[cfg(unix)]
use ta_runtime::SandboxProvider;

#[derive(Subcommand, Debug)]
pub enum SandboxCommands {
    /// Explain a goal's sandbox decision, or what a role class gets here.
    ///
    /// Pass a goal id to read the recorded decision and denials, or a role
    /// (chat, reviewer, worker, librarian) to preview its profile and which
    /// postures this machine can satisfy.
    Explain {
        /// Goal id or role class. Omit for the capability table of every role.
        target: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run a harmless probe inside the sandbox and report what it could reach.
    Test {
        /// Role class to test (default: chat, the strictest).
        #[arg(long, default_value = "chat")]
        role: String,
    },
    /// Approve a persona's `[sandbox]` additions by pinning the file's hash.
    PinPersona {
        /// Persona name (.ta/personas/<name>.toml).
        name: String,
    },
}

pub fn execute(cmd: &SandboxCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let root = &config.workspace_root;
    match cmd {
        SandboxCommands::Explain { target, json } => explain(root, target.as_deref(), *json),
        SandboxCommands::Test { role } => test(role),
        SandboxCommands::PinPersona { name } => pin_persona(root, name),
    }
}

fn events_dir(root: &Path) -> std::path::PathBuf {
    root.join(".ta").join("events")
}

fn explain(root: &Path, target: Option<&str>, json: bool) -> anyhow::Result<()> {
    if let Some(t) = target {
        if let Ok(goal_id) = uuid::Uuid::parse_str(t) {
            return explain_goal(root, goal_id, json);
        }
        let Some(role) = RoleClass::parse(t) else {
            bail!(
                "\"{t}\" is neither a goal id nor a role class. Use a goal id from `ta goal list`, \
                 or one of: chat, reviewer, worker, librarian."
            );
        };
        return explain_role(role, json);
    }
    print_matrix(json)
}

fn print_matrix(json: bool) -> anyhow::Result<()> {
    let provider = SandboxPolicy::detect_provider();
    let rows = readiness(&provider);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "provider": ta_runtime::sandbox_launch::provider_name(&provider),
                "roles": rows,
            }))?
        );
        return Ok(());
    }
    println!(
        "Sandbox provider: {}",
        ta_runtime::sandbox_launch::provider_name(&provider)
    );
    println!("{}", format_matrix(&rows));
    Ok(())
}

/// The role-by-posture table as text.
pub fn format_matrix(rows: &[ta_runtime::sandbox_launch::RoleReadiness]) -> String {
    let mut out = String::new();
    out.push_str("  role        strict      balanced    open        missing capabilities\n");
    for r in rows {
        let cell = |p: SandboxPosture| {
            r.by_posture
                .iter()
                .find(|(q, _)| *q == p)
                .map(|(_, o)| o.name())
                .unwrap_or("?")
        };
        let missing = if r.missing.is_empty() {
            "none".to_string()
        } else {
            r.missing
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "  {:<11} {:<11} {:<11} {:<11} {}\n",
            r.role.name(),
            cell(SandboxPosture::Strict),
            cell(SandboxPosture::Balanced),
            cell(SandboxPosture::Open),
            missing
        ));
    }
    out
}

fn explain_role(role: RoleClass, json: bool) -> anyhow::Result<()> {
    let provider = SandboxPolicy::detect_provider();
    let profile = representative_profile(role, SandboxPosture::Balanced);
    let row = readiness(&provider)
        .into_iter()
        .find(|r| r.role == role)
        .context("role missing from the readiness table")?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "provider": ta_runtime::sandbox_launch::provider_name(&provider),
                "readiness": row,
                "profile": profile,
            }))?
        );
        return Ok(());
    }
    let ceiling = role.ceiling();
    println!("Role class: {}", role.name());
    println!(
        "Provider:   {}",
        ta_runtime::sandbox_launch::provider_name(&provider)
    );
    println!(
        "Required:   {}",
        row.required
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "Missing:    {}",
        if row.missing.is_empty() {
            "none".to_string()
        } else {
            row.missing
                .iter()
                .map(|c| format!("{} ({})", c.name(), c.fix_hint()))
                .collect::<Vec<_>>()
                .join("; ")
        }
    );
    println!(
        "Ceiling:    extra read {}, extra write {}, network grants {}",
        ceiling.extra_read, ceiling.extra_write, ceiling.extra_network
    );
    println!("{}", format_matrix(std::slice::from_ref(&row)));
    println!("Preview (balanced, egress proxy running, no network grant):");
    println!("  writable:  {}", join_paths(&profile.allow_write));
    println!("  readable:  {} entries", profile.allow_read.len());
    println!("  network:   {}", profile.allow_network.join(", "));
    for w in &profile.warnings {
        println!("  note: {w}");
    }
    Ok(())
}

fn join_paths(paths: &[std::path::PathBuf]) -> String {
    if paths.is_empty() {
        return "(none)".to_string();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Latest recorded decision and every violation for a goal.
pub fn goal_records(
    events_dir: &Path,
    goal_id: uuid::Uuid,
) -> anyhow::Result<(Option<EventEnvelope>, Vec<EventEnvelope>)> {
    let store = FsEventStore::new(events_dir);
    let filter = EventQueryFilter {
        goal_id: Some(goal_id),
        event_types: vec!["sandbox_decision".into(), "sandbox_violation".into()],
        ..Default::default()
    };
    let all = store.query(&filter).with_context(|| {
        format!(
            "could not read events from {}. Check the directory exists and is readable.",
            events_dir.display()
        )
    })?;
    let mut decision = None;
    let mut violations = Vec::new();
    for e in all {
        match e.payload {
            SessionEvent::SandboxDecision { .. } => decision = Some(e),
            SessionEvent::SandboxViolation { .. } => violations.push(e),
            _ => {}
        }
    }
    Ok((decision, violations))
}

/// The macOS denial-capture check recorded for a goal, when there is one.
pub fn goal_capture(events_dir: &Path, goal_id: uuid::Uuid) -> Option<EventEnvelope> {
    let filter = EventQueryFilter {
        goal_id: Some(goal_id),
        event_types: vec!["sandbox_capture".into()],
        ..Default::default()
    };
    FsEventStore::new(events_dir)
        .query(&filter)
        .ok()?
        .into_iter()
        .rfind(|e| matches!(e.payload, SessionEvent::SandboxCapture { .. }))
}

fn explain_goal(root: &Path, goal_id: uuid::Uuid, json: bool) -> anyhow::Result<()> {
    let dir = events_dir(root);
    let (decision, violations) = goal_records(&dir, goal_id)?;
    let capture = goal_capture(&dir, goal_id);
    let Some(decision) = decision else {
        bail!(
            "no sandbox decision is recorded for goal {goal_id} in {}. Goals started before \
             sandbox recording, or events pruned by `ta events prune`, have none. Run \
             `ta sandbox explain <role>` to see what a role gets today.",
            dir.display()
        );
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "decision": decision.payload,
                "violations": violations.iter().map(|v| &v.payload).collect::<Vec<_>>(),
                "capture": capture.as_ref().map(|c| &c.payload),
            }))?
        );
        return Ok(());
    }
    let SessionEvent::SandboxDecision {
        posture,
        role_class,
        provider,
        outcome,
        required,
        enforced,
        missing,
        profile_hash,
        gateway,
        detail,
        ..
    } = &decision.payload
    else {
        unreachable!("goal_records only stores SandboxDecision here");
    };
    println!("Goal:       {goal_id}");
    println!("Outcome:    {outcome}");
    println!("Posture:    {posture}");
    println!("Role class: {role_class}");
    println!("Provider:   {provider}");
    println!("Gateway:    {gateway}");
    println!("Profile:    sha256 {profile_hash}");
    println!("Required:   {}", required.join(", "));
    println!("Enforced:   {}", enforced.join(", "));
    println!(
        "Missing:    {}",
        if missing.is_empty() {
            "none".to_string()
        } else {
            missing.join(", ")
        }
    );
    println!("Detail:     {detail}");
    if !missing.is_empty() && matches!(outcome.as_str(), "degraded" | "unsandboxed") {
        println!(
            "Review:     drafts from this goal are labelled \"built without sandbox capability {}\"",
            missing.join(", ")
        );
    }
    println!();
    println!(
        "{}",
        format_violations(provider, &violations, capture.as_ref())
    );
    Ok(())
}

/// Denials as text. On macOS the list comes from the unified log, which can
/// lose entries, so the text says how far this goal's capture was verified:
/// a `SandboxCapture` record with both canary denials delivered and the
/// persisted log reconciled makes an empty list strong evidence; without one
/// (or with a failed check) an empty list is not proof of anything.
pub fn format_violations(
    provider: &str,
    violations: &[EventEnvelope],
    capture: Option<&EventEnvelope>,
) -> String {
    let macos = provider == "macos_sandbox_exec";
    let verdict = if macos {
        Some(match capture.map(|c| &c.payload) {
            Some(SessionEvent::SandboxCapture {
                verified: true,
                live_denials,
                recovered_denials,
                ..
            }) => CaptureVerdict::Verified {
                live: *live_denials,
                recovered: *recovered_denials,
            },
            Some(SessionEvent::SandboxCapture { detail, .. }) => {
                CaptureVerdict::Unverified(detail.clone())
            }
            _ => CaptureVerdict::NotChecked,
        })
    } else {
        None
    };
    let mut out = String::new();
    if violations.is_empty() {
        out.push_str("Denials:    none recorded\n");
        match &verdict {
            Some(CaptureVerdict::Verified { recovered, .. }) => out.push_str(&format!(
                "            Capture was verified for this goal: canary denials came back through \
                 the unified log before the agent started and after it exited, and the persisted \
                 log was read ({recovered} denial(s) recovered from it). macOS can still omit a \
                 single denial under heavy load, so this is strong evidence, not proof. The \
                 profile is what confines the agent.\n"
            )),
            Some(CaptureVerdict::Unverified(why)) => out.push_str(&format!(
                "            This is not proof that nothing was denied. Capture was NOT verified \
                 for this goal: {why} The profile, not this list, is what confines the agent.\n"
            )),
            Some(CaptureVerdict::NotChecked) => out.push_str(
                "            This is not proof that nothing was denied. macOS reports Seatbelt \
                 denials through the unified log, which can drop entries under load or when \
                 log access is unavailable, and no capture check was recorded for this goal \
                 (it ran before capture checks existed, or the log tool could not start). The \
                 profile, not this list, is what confines the agent.\n",
            ),
            None => {}
        }
        return out;
    }
    out.push_str(&format!("Denials:    {} recorded\n", violations.len()));
    for v in violations {
        if let SessionEvent::SandboxViolation {
            source,
            operation,
            target,
            count,
            detail,
            ..
        } = &v.payload
        {
            out.push_str(&format!(
                "  {source:<12} {operation:<18} {target} (x{count})\n    {detail}\n"
            ));
        }
    }
    match verdict {
        Some(CaptureVerdict::Verified { live, recovered }) => out.push_str(&format!(
            "  (Capture verified for this goal: {live} live, {recovered} recovered from the \
             persisted log. An individual denial can still be missing under heavy load.)\n"
        )),
        Some(CaptureVerdict::Unverified(why)) => out.push_str(&format!(
            "  (Capture was NOT verified for this goal, so this list may be incomplete: {why})\n"
        )),
        Some(CaptureVerdict::NotChecked) => out.push_str(
            "  (macOS capture is best-effort and no capture check was recorded for this goal; \
             this list may be incomplete.)\n",
        ),
        None => {}
    }
    out
}

enum CaptureVerdict {
    Verified { live: u64, recovered: u64 },
    Unverified(String),
    NotChecked,
}

/// One labelled check in `ta sandbox test` (live probes run on Unix only).
#[cfg(unix)]
pub struct Check {
    pub name: &'static str,
    pub expect_ok: bool,
    pub got_ok: bool,
}

#[cfg(unix)]
impl Check {
    pub fn passed(&self) -> bool {
        self.expect_ok == self.got_ok
    }
}

fn test(role_name: &str) -> anyhow::Result<()> {
    let Some(role) = RoleClass::parse(role_name) else {
        bail!("unknown role \"{role_name}\". Use one of: chat, reviewer, worker, librarian.");
    };
    let provider = SandboxPolicy::detect_provider();
    let pname = ta_runtime::sandbox_launch::provider_name(&provider);
    println!("Role class: {}", role.name());
    println!("Provider:   {pname}");

    let row = readiness(&provider)
        .into_iter()
        .find(|r| r.role == role)
        .context("role missing from the readiness table")?;
    let balanced = row
        .by_posture
        .iter()
        .find(|(p, _)| *p == SandboxPosture::Balanced)
        .map(|(_, o)| *o);
    println!(
        "Decision:   {} under balanced (missing: {})",
        balanced.map(|o| o.name()).unwrap_or("?"),
        if row.missing.is_empty() {
            "none".to_string()
        } else {
            row.missing
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    #[cfg(unix)]
    {
        if provider == SandboxProvider::None {
            bail!(
                "no sandbox provider is available here, so there is nothing to probe. Run \
                 `ta doctor` for the capability table and the fix for your platform."
            );
        }
        let checks = live_checks(role, provider)?;
        let mut failed = 0;
        for c in &checks {
            let ok = c.passed();
            if !ok {
                failed += 1;
            }
            println!(
                "  [{}] {} (expected {}, got {})",
                if ok { "ok" } else { "FAIL" },
                c.name,
                if c.expect_ok { "allowed" } else { "denied" },
                if c.got_ok { "allowed" } else { "denied" },
            );
        }
        if failed > 0 {
            bail!(
                "{failed} of {} sandbox probes did not behave as the profile says. The box is not \
                 confining as described: run `ta doctor` and fix the missing capabilities, or \
                 file the output above.",
                checks.len()
            );
        }
        println!("All {} probes behaved as the profile says.", checks.len());
        println!(
            "Note: reads outside the project are limited by a deny list on macOS, not an \
             allowlist, so this test only checks the denies the profile claims."
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = provider;
        println!(
            "Live probes run on macOS and Linux. On Windows, run the conformance suite: \
             `cargo test -p ta-runtime --test windows_conformance`."
        );
        Ok(())
    }
}

#[cfg(unix)]
fn live_checks(role: RoleClass, provider: SandboxProvider) -> anyhow::Result<Vec<Check>> {
    use ta_runtime::adapter::{SpawnRequest, StdinMode, StdoutMode};
    use ta_runtime::sandbox_launch::{LaunchContext, ProjectRequest, SandboxProfile};

    let tmp = tempfile::tempdir().context("could not create a scratch directory")?;
    let base = std::fs::canonicalize(tmp.path())?;
    let mk = |rel: &str| -> anyhow::Result<std::path::PathBuf> {
        let p = base.join(rel);
        std::fs::create_dir_all(&p)?;
        Ok(p)
    };
    let project = mk("project")?;
    let staging = mk("project/.ta/staging/probe")?;
    let state = mk("state")?;
    let agent_home = mk("state/agent-homes/probe")?;
    let outside = mk("outside")?;
    let secret = state.join("policy.toml");
    std::fs::write(&secret, "secret")?;

    let ctx = LaunchContext {
        goal_id: "sandbox-test".into(),
        role_class: role,
        team_role: true,
        project_root: project,
        staging: staging.clone(),
        state_dir: Some(state),
        agent_home,
        user_home: None,
        mediation_socket: None,
        egress_proxy: false,
        persona: None,
    };
    let mut profile = SandboxProfile::compile(None, &ProjectRequest::default(), &ctx);
    profile.requested = true;
    let policy = profile.to_policy(provider);

    let run = |script: String| -> bool {
        let req = SpawnRequest {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            env: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
            working_dir: staging.clone(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Piped,
            clear_env: true,
        };
        let req = match policy.try_apply(req) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[sandbox test] could not wrap the probe: {e}");
                return false;
            }
        };
        std::process::Command::new(&req.command)
            .args(&req.args)
            .env_clear()
            .envs(&req.env)
            .current_dir(&req.working_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    let inside = staging.join("probe-write");
    let outside_file = outside.join("probe-write");
    let checks = vec![
        Check {
            name: "write inside the staging copy",
            expect_ok: true,
            got_ok: run(format!("echo x > '{}'", inside.display())),
        },
        Check {
            name: "write outside the staging copy",
            expect_ok: false,
            got_ok: run(format!("echo x > '{}'", outside_file.display())),
        },
        Check {
            name: "read the trusted policy file",
            expect_ok: false,
            got_ok: run(format!("cat '{}' > /dev/null", secret.display())),
        },
    ];
    Ok(checks)
}

fn pin_persona(root: &Path, name: &str) -> anyhow::Result<()> {
    super::human_auth::require_human_for("sandbox")?;
    let Some(dir) = super::sandbox_launch::project_state_dir(root) else {
        bail!(
            "this project has no TA state directory yet, so there is nowhere trusted to record \
             the pin. Run `ta rules init` or `ta setup` first."
        );
    };
    let sandbox_dir = dir.join("sandbox");
    std::fs::create_dir_all(&sandbox_dir)
        .with_context(|| format!("could not create {}", sandbox_dir.display()))?;
    let (cfg, hash) = ta_goal::PersonaConfig::load_with_hash(root, name)
        .map_err(|e| anyhow::anyhow!("could not read persona \"{name}\": {e}"))?;
    let path = ta_runtime::sandbox_launch::save_persona_pin(&sandbox_dir, name, &hash)
        .map_err(anyhow::Error::msg)?;
    println!(
        "Pinned persona \"{name}\" (sha256 {hash}) in {}.",
        path.display()
    );
    println!(
        "  allow_hosts: {}",
        if cfg.sandbox.allow_hosts.is_empty() {
            "(none)".to_string()
        } else {
            cfg.sandbox.allow_hosts.join(", ")
        }
    );
    println!(
        "  allow_read:  {}",
        if cfg.sandbox.allow_read.is_empty() {
            "(none)".to_string()
        } else {
            cfg.sandbox.allow_read.join(", ")
        }
    );
    println!(
        "Editing the persona file changes its hash and turns these additions off until you run \
         `ta sandbox pin-persona {name}` again."
    );
    Ok(())
}

/// The one-line sandbox badge for a goal (`ta status`, Studio), from its
/// recorded decision. `None` when no decision was recorded.
pub fn badge_for_goal(root: &Path, goal_id: uuid::Uuid) -> Option<String> {
    let (decision, _) = goal_records(&events_dir(root), goal_id).ok()?;
    let SessionEvent::SandboxDecision {
        outcome,
        posture,
        role_class,
        missing,
        ..
    } = decision?.payload
    else {
        return None;
    };
    Some(ta_runtime::sandbox_launch::badge_from_parts(
        &outcome,
        &posture,
        &role_class,
        &missing,
    ))
}

/// The draft review label for a recorded decision, for `ta draft view`.
pub fn draft_label_for_goal(root: &Path, goal_id: uuid::Uuid) -> Option<String> {
    let (decision, _) = goal_records(&events_dir(root), goal_id).ok()?;
    let SessionEvent::SandboxDecision {
        outcome, missing, ..
    } = decision?.payload
    else {
        return None;
    };
    let outcome = match outcome.as_str() {
        "degraded" => Outcome::Degraded,
        "unsandboxed" => Outcome::Unsandboxed,
        _ => return None,
    };
    // Reuse the one label format.
    let d = ta_runtime::sandbox_launch::SandboxDecision {
        posture: SandboxPosture::Balanced,
        role_class: RoleClass::Worker,
        provider: String::new(),
        outcome,
        required: vec![],
        enforced: vec![],
        missing,
        profile_hash: String::new(),
        gateway: ta_runtime::sandbox_launch::GatewayPlacement::StdioUnsandboxed,
        detail: String::new(),
    };
    degraded_label(&d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_runtime::SandboxProvider;

    fn store_with(dir: &Path, goal: uuid::Uuid, outcome: &str, missing: &[&str]) {
        let store = FsEventStore::new(dir);
        store
            .append(&EventEnvelope::new(SessionEvent::SandboxDecision {
                goal_id: goal,
                posture: "balanced".into(),
                role_class: "chat".into(),
                provider: "macos_sandbox_exec".into(),
                outcome: outcome.into(),
                required: vec!["fs_write_confined".into()],
                enforced: vec![],
                missing: missing.iter().map(|s| s.to_string()).collect(),
                profile_hash: "abc".into(),
                gateway: "out_of_sandbox_socket".into(),
                detail: "d".into(),
            }))
            .unwrap();
    }

    #[test]
    fn records_split_decision_and_violations() {
        let tmp = tempfile::tempdir().unwrap();
        let goal = uuid::Uuid::new_v4();
        store_with(tmp.path(), goal, "enforced", &[]);
        FsEventStore::new(tmp.path())
            .append(&EventEnvelope::new(SessionEvent::SandboxViolation {
                goal_id: goal,
                source: "seatbelt".into(),
                operation: "file-read-data".into(),
                target: "/x".into(),
                process: None,
                pid: None,
                count: 2,
                detail: "denied".into(),
            }))
            .unwrap();
        let (d, v) = goal_records(tmp.path(), goal).unwrap();
        assert!(d.is_some());
        assert_eq!(v.len(), 1);
    }

    fn capture_event(goal: uuid::Uuid, verified: bool, detail: &str) -> EventEnvelope {
        EventEnvelope::new(SessionEvent::SandboxCapture {
            goal_id: goal,
            verified,
            start_canary: true,
            end_canary: verified,
            reconciled: verified,
            live_denials: 2,
            recovered_denials: 1,
            detail: detail.into(),
        })
    }

    #[test]
    fn empty_macos_violations_are_not_proof_without_a_verified_capture() {
        let text = format_violations("macos_sandbox_exec", &[], None);
        assert!(text.contains("none recorded"));
        assert!(text.contains("not proof"));
        assert!(text.contains("no capture check was recorded"), "{text}");
        let linux = format_violations("linux_bwrap", &[], None);
        assert!(!linux.contains("not proof"));
        assert!(!linux.contains("Capture"));
    }

    #[test]
    fn a_failed_capture_check_is_named_and_keeps_the_caveat() {
        let goal = uuid::Uuid::new_v4();
        let failed = capture_event(
            goal,
            false,
            "the live log stream did not deliver a canary denial after the agent exited.",
        );
        let text = format_violations("macos_sandbox_exec", &[], Some(&failed));
        assert!(
            text.contains("not proof") && text.contains("NOT verified"),
            "{text}"
        );
        assert!(text.contains("did not deliver a canary denial after the agent exited"));
    }

    #[test]
    fn a_verified_capture_replaces_the_bare_caveat_with_what_was_checked() {
        let goal = uuid::Uuid::new_v4();
        let ok = capture_event(goal, true, "");
        let text = format_violations("macos_sandbox_exec", &[], Some(&ok));
        assert!(text.contains("none recorded"));
        assert!(!text.contains("not proof that nothing"), "{text}");
        assert!(
            text.contains("verified") && text.contains("1 denial(s) recovered"),
            "{text}"
        );
        assert!(text.contains("strong evidence, not proof"));
    }

    #[test]
    fn goal_capture_returns_the_latest_record_for_that_goal_only() {
        let tmp = tempfile::tempdir().unwrap();
        let goal = uuid::Uuid::new_v4();
        let store = FsEventStore::new(tmp.path());
        store.append(&capture_event(goal, false, "first")).unwrap();
        store.append(&capture_event(goal, true, "")).unwrap();
        store
            .append(&capture_event(uuid::Uuid::new_v4(), false, "other goal"))
            .unwrap();
        let got = goal_capture(tmp.path(), goal).expect("a capture record");
        assert!(matches!(
            got.payload,
            SessionEvent::SandboxCapture { verified: true, .. }
        ));
        assert!(goal_capture(tmp.path(), uuid::Uuid::new_v4()).is_none());
    }

    #[test]
    fn draft_label_only_for_degraded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let events = events_dir(root);
        let degraded = uuid::Uuid::new_v4();
        let enforced = uuid::Uuid::new_v4();
        store_with(&events, degraded, "degraded", &["net_egress_filtered"]);
        store_with(&events, enforced, "enforced", &[]);
        assert_eq!(
            draft_label_for_goal(root, degraded).as_deref(),
            Some("built without sandbox capability net_egress_filtered")
        );
        assert_eq!(draft_label_for_goal(root, enforced), None);
        assert_eq!(draft_label_for_goal(root, uuid::Uuid::new_v4()), None);
    }

    #[test]
    fn matrix_lists_every_role() {
        let rows = readiness(&SandboxProvider::None);
        let text = format_matrix(&rows);
        for r in RoleClass::ALL {
            assert!(text.contains(r.name()), "{} missing from {text}", r.name());
        }
    }

    #[test]
    fn unknown_target_is_actionable() {
        let tmp = tempfile::tempdir().unwrap();
        let err = explain(tmp.path(), Some("wizard"), false).unwrap_err();
        assert!(err.to_string().contains("goal id"));
    }
}
