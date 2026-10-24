//! `ta-daemon --print-capabilities`: facts only this binary can vouch for.
//!
//! `ta capabilities` runs in the `ta` binary, which has no way to know what a
//! separately installed `ta-daemon` does. Before this probe an installer could
//! only look for `--chat-mode` / `--origin` in `ta run --help`, which passes for
//! a daemon that never sends them. Here each fact is decided by running the
//! daemon's own code on a scratch project, so the answer is true only when the
//! path is wired:
//!
//! - `passes_restricted_flags`: [`build_ta_run_args`], the function every role
//!   launch uses, is asked to launch a Chief-of-Staff role and must emit both
//!   `--chat-mode` and `--origin cos`. A plain worker launched the same way must
//!   emit neither (so the flags are not simply always on).
//! - `outcome_roles`: [`role_token_scopes`], which `role_launch_env` uses to
//!   mint role tokens, must grant the outcome right to the Chief of Staff and
//!   withhold it from a worker.
//! - `nats_credentials`: [`ta_agent_whiteboard::nats_auth::decide`], the policy
//!   behind the NATS transport, must refuse a non-loopback server with no
//!   credentials, accept it with credentials, and accept loopback without.
//!
//! The probe starts no server, opens no socket and touches no real state: the
//! scratch project is a temp dir that is removed afterwards.

use std::collections::BTreeMap;
use std::path::Path;

use ta_session::agent_action::TeamRole;
use ta_session::team::TeamConfig;
use ta_workspace::{DaemonProbe, DAEMON_PROBE_SCHEMA_VERSION};
use uuid::Uuid;

use crate::team_session::{
    build_ta_run_args, role_token_scopes, TeamSessionConfig, TeamSessionStageConfig,
    TeamSessionState,
};

/// The role a CoS persona is attached to in the scratch project.
const COS_ROLE: &str = "chief-of-staff";
/// A role that must never get the outcome right or the restricted flags.
const WORKER_ROLE: &str = "implementer";

/// Run every check and report. Never fails: a check that cannot pass reports
/// `false` with the reason in `evidence`.
pub fn run_probe() -> DaemonProbe {
    let mut evidence = BTreeMap::new();
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
    let passes_restricted_flags =
        verdict("daemon_passes_restricted_flags", check_restricted_flags());
    let outcome_roles = verdict("outcome_roles", check_outcome_roles());
    let nats_credentials = verdict("nats_credentials", check_nats_credentials());
    DaemonProbe {
        probe_schema: DAEMON_PROBE_SCHEMA_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        passes_restricted_flags,
        outcome_roles,
        nats_credentials,
        evidence,
    }
}

fn write_persona(project: &Path, name: &str, capabilities_toml: &str) -> Result<(), String> {
    let dir = project.join(".ta").join("personas");
    std::fs::create_dir_all(&dir).map_err(|e| format!("scratch project: {e}"))?;
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("[persona]\nname = \"{name}\"\n\n[capabilities]\n{capabilities_toml}\n"),
    )
    .map_err(|e| format!("scratch project: {e}"))
}

/// The arguments the daemon's launch code builds for `role` bound to a persona
/// with the given `[capabilities]` text, in the scratch project `project`.
fn launch_args(project: &Path, role: &str, capabilities_toml: &str) -> Result<Vec<String>, String> {
    write_persona(project, role, capabilities_toml)?;
    let state = TeamSessionState::new(
        "capability-probe".to_string(),
        TeamSessionConfig {
            name: "capability-probe".to_string(),
            workflow_path: String::new(),
            team_toml_path: ".ta/team.toml".to_string(),
            objective: "capability probe".to_string(),
            budget: None,
            role_prompts: std::collections::HashMap::new(),
            whiteboard_enabled: false,
            whiteboard_token: None,
            whiteboard_token_expires_at: None,
        },
        vec![TeamSessionStageConfig {
            name: "probe".to_string(),
            roles: vec![role.to_string()],
        }],
    );
    let mut team = TeamConfig::default();
    team.assign(
        TeamRole::new(role),
        "probe-model".to_string(),
        ta_session::workflow_session::AdvisorSecurity::Auto,
        Some(role.to_string()),
    );
    build_ta_run_args(
        project,
        &state,
        "probe",
        role,
        Uuid::nil(),
        &team,
        &project.join("context.md"),
        None,
    )
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn check_restricted_flags() -> Result<String, String> {
    let scratch =
        tempfile::tempdir().map_err(|e| format!("cannot create a scratch project: {e}"))?;
    let cos = launch_args(
        scratch.path(),
        COS_ROLE,
        "chat_mode = true\norigin = \"cos\"",
    )
    .map_err(|e| format!("the Chief-of-Staff launch could not be built: {e}"))?;
    if !cos.iter().any(|a| a == "--chat-mode") {
        return Err(format!(
            "a Chief-of-Staff launch carries no --chat-mode (args: {cos:?})"
        ));
    }
    if flag_value(&cos, "--origin") != Some("cos") {
        return Err(format!(
            "a Chief-of-Staff launch carries no `--origin cos` (args: {cos:?})"
        ));
    }
    let worker = launch_args(scratch.path(), WORKER_ROLE, "")
        .map_err(|e| format!("the worker launch could not be built: {e}"))?;
    if worker.iter().any(|a| a == "--chat-mode" || a == "--origin") {
        return Err(format!(
            "a plain worker launch also carries restricted flags (args: {worker:?}), so the \
             flags do not distinguish the Chief of Staff"
        ));
    }
    Ok(format!(
        "team_session::build_ta_run_args for a chat-mode origin=cos persona emitted \
         `--chat-mode --origin cos`; a plain worker emitted neither (daemon {})",
        env!("CARGO_PKG_VERSION")
    ))
}

fn check_outcome_roles() -> Result<String, String> {
    let defaults = ta_agent_whiteboard::WhiteboardConfig::default().outcome_roles;
    let cos = role_token_scopes("capability-probe", COS_ROLE, &defaults);
    if !ta_agent_whiteboard::scopes::may_send_outcome(&cos) {
        return Err(format!(
            "a {COS_ROLE} role token does not carry the outcome right (outcome_roles = {defaults:?})"
        ));
    }
    let worker = role_token_scopes("capability-probe", WORKER_ROLE, &defaults);
    if ta_agent_whiteboard::scopes::may_send_outcome(&worker) {
        return Err(format!(
            "a {WORKER_ROLE} role token carries the outcome right although only {defaults:?} \
             are configured for it"
        ));
    }
    Ok(format!(
        "team_session::role_token_scopes granted the outcome right to {COS_ROLE} and withheld it \
         from {WORKER_ROLE} (default outcome_roles {defaults:?})"
    ))
}

fn check_nats_credentials() -> Result<String, String> {
    use ta_agent_whiteboard::nats_auth::{decide, NatsAuthError, DEFAULT_CREDENTIALS_ENV};
    let env = DEFAULT_CREDENTIALS_ENV;
    let remote = "nats://203.0.113.7:4222";
    match decide(remote, env, None, None) {
        Err(NatsAuthError::UnauthenticatedRemote { .. }) => {}
        other => {
            return Err(format!(
                "a non-loopback NATS server with no credentials was not refused (got {:?})",
                other.map(|(auth, _)| auth.kind())
            ))
        }
    }
    match decide(remote, env, Some("probe-user:probe-pass"), None) {
        Ok((auth, _)) if !auth.is_none() => {}
        other => {
            return Err(format!(
                "a non-loopback NATS server with credentials was not accepted (got {:?})",
                other.map(|(auth, _)| auth.kind())
            ))
        }
    }
    if let Err(e) = decide("nats://127.0.0.1:4222", env, None, None) {
        return Err(format!(
            "loopback NATS without credentials was refused ({e}), which would break local setups"
        ));
    }
    Ok(
        "nats_auth::decide refused a non-loopback server without credentials, accepted it with \
         credentials, and accepted loopback without"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_every_wired_capability_true_with_evidence() {
        let probe = run_probe();
        assert_eq!(probe.probe_schema, DAEMON_PROBE_SCHEMA_VERSION);
        assert_eq!(probe.daemon_version, env!("CARGO_PKG_VERSION"));
        assert!(probe.passes_restricted_flags, "{:?}", probe.evidence);
        assert!(probe.outcome_roles, "{:?}", probe.evidence);
        assert!(probe.nats_credentials, "{:?}", probe.evidence);
        for key in [
            "daemon_passes_restricted_flags",
            "outcome_roles",
            "nats_credentials",
        ] {
            let line = probe
                .evidence
                .get(key)
                .unwrap_or_else(|| panic!("no evidence for {key}"));
            assert!(!line.starts_with("not wired"), "{key}: {line}");
        }
    }

    /// The probe must be able to say no: a launch that drops the flags fails the check.
    #[test]
    fn restricted_flag_check_fails_when_the_persona_asks_for_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        // A "chief-of-staff" whose persona declares no chat mode and no origin
        // is built by the real launch code without the flags.
        let args = launch_args(scratch.path(), COS_ROLE, "").unwrap();
        assert!(!args.iter().any(|a| a == "--chat-mode"));
        assert_eq!(flag_value(&args, "--origin"), None);
    }

    #[test]
    fn launch_args_for_a_cos_persona_carry_both_flags() {
        let scratch = tempfile::tempdir().unwrap();
        let args = launch_args(
            scratch.path(),
            COS_ROLE,
            "chat_mode = true\norigin = \"cos\"",
        )
        .unwrap();
        assert!(args.iter().any(|a| a == "--chat-mode"));
        assert_eq!(flag_value(&args, "--origin"), Some("cos"));
    }
}
