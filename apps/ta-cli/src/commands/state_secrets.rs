//! Control-plane secrets and the per-user TA state directory.
//!
//! * `ta show state` prints where everything lives, so installers and the
//!   people running them do not guess.
//! * `ta doctor` reports old-location secrets and bearer tokens left in the
//!   project tree.
//! * `ta doctor --fix` moves them: each copy is verified before the old one is
//!   removed, nothing is ever deleted unverified, a re-run is safe, and every
//!   outcome is written to the audit log (never the secret itself).

use std::path::{Path, PathBuf};

use ta_audit::{AuditAction, AuditEvent, AuditLog};
use ta_credentials::state::{
    self, CustodyOpts, LegacyFinding, MigrationReport, MigrationStatus, Os, Posture, ProjectState,
    StateLocator,
};
use ta_mcp_gateway::GatewayConfig;

/// What `ta doctor` needs to know about secrets in the project tree.
pub struct SecretsSummary {
    pub posture: Posture,
    pub legacy: Vec<LegacyFinding>,
    /// Files that still carry a live bearer token inside the project tree.
    pub token_files: Vec<PathBuf>,
}

impl SecretsSummary {
    pub fn is_clean(&self) -> bool {
        self.legacy.is_empty() && self.token_files.is_empty()
    }
}

pub fn summarize(project_root: &Path) -> SecretsSummary {
    let locator = StateLocator::from_env();
    SecretsSummary {
        posture: Posture::resolve(&locator),
        legacy: state::legacy_findings(project_root),
        token_files: token_bearing_files(project_root),
    }
}

fn has_live_token(value: &serde_json::Value) -> bool {
    value
        .pointer("/config/whiteboard_token")
        .is_some_and(|t| t.as_str().is_some_and(|s| !s.is_empty()))
}

/// Team-session `state.json` files that hold a `whiteboard_token`, and any
/// `whiteboard-session.json` copy (project or staging) from older launches.
pub fn token_bearing_files(project_root: &Path) -> Vec<PathBuf> {
    let ta = project_root.join(".ta");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(ta.join("team-sessions")) {
        for e in entries.flatten() {
            let state_json = e.path().join("state.json");
            if let Ok(text) = std::fs::read_to_string(&state_json) {
                if serde_json::from_str::<serde_json::Value>(&text)
                    .map(|v| has_live_token(&v))
                    .unwrap_or(false)
                {
                    out.push(state_json);
                }
            }
        }
    }
    let direct = ta.join("whiteboard-session.json");
    if direct.is_file() {
        out.push(direct);
    }
    if let Ok(entries) = std::fs::read_dir(ta.join("staging")) {
        for e in entries.flatten() {
            let f = e.path().join(".ta").join("whiteboard-session.json");
            if f.is_file() {
                out.push(f);
            }
        }
    }
    out.sort();
    out
}

/// Remove bearer tokens left in the project tree by older launches:
/// `state.json` files are rewritten without the token, `whiteboard-session.json`
/// copies are deleted. Returns one line per file for the report.
pub fn scrub_token_files(files: &[PathBuf]) -> Vec<(PathBuf, Result<(), String>)> {
    files
        .iter()
        .map(|f| {
            let result = if f.file_name().is_some_and(|n| n == "state.json") {
                scrub_state_json(f)
            } else {
                std::fs::remove_file(f).map_err(|e| {
                    format!("could not delete {}: {e}. Delete it yourself.", f.display())
                })
            };
            (f.clone(), result)
        })
        .collect()
}

fn scrub_state_json(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON ({e}); left untouched", path.display()))?;
    if let Some(cfg) = v.get_mut("config").and_then(|c| c.as_object_mut()) {
        cfg.remove("whiteboard_token");
        cfg.remove("whiteboard_token_expires_at");
        // The daemon mints a fresh per-role token at each launch instead.
        cfg.insert("whiteboard_enabled".into(), serde_json::Value::Bool(true));
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

fn status_word(s: &MigrationStatus) -> &'static str {
    match s {
        MigrationStatus::Migrated => "migrated",
        MigrationStatus::StaleCopyRemoved => "stale_copy_removed",
        MigrationStatus::NothingToMigrate => "nothing_to_migrate",
        MigrationStatus::Conflict(_) => "conflict",
        MigrationStatus::Failed(_) => "failed",
    }
}

/// Write one audit event per migrated, refused or failed item. Values are
/// never recorded, only names, locations and outcomes.
pub fn audit_migration(config: &GatewayConfig, project_id: &str, report: &MigrationReport) {
    let mut log = match AuditLog::open(&config.audit_log) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                path = %config.audit_log.display(),
                error = %e,
                "could not open the audit log to record the secret migration"
            );
            eprintln!(
                "[warn] The secret migration succeeded but could not be written to the audit log \
                 at {} ({e}). Check the file's permissions.",
                config.audit_log.display()
            );
            return;
        }
    };
    for item in &report.items {
        if item.status == MigrationStatus::NothingToMigrate {
            continue;
        }
        let detail = match &item.status {
            MigrationStatus::Conflict(m) | MigrationStatus::Failed(m) => m.clone(),
            _ => String::new(),
        };
        let mut event = AuditEvent::new("ta-doctor", AuditAction::SecretMigration)
            .with_target(format!("secret://{}", item.name))
            .with_metadata(serde_json::json!({
                "project_id": project_id,
                "from": item.from.display().to_string(),
                "to": item.to,
                "status": status_word(&item.status),
                "detail": detail,
            }));
        if let Err(e) = log.append(&mut event) {
            tracing::warn!(error = %e, item = %item.name, "could not append a secret-migration audit event");
        }
    }
}

/// `ta doctor --fix` for control-plane secrets. Returns (fixed, skipped).
pub fn fix(config: &GatewayConfig, yes: bool) -> anyhow::Result<(usize, usize)> {
    use std::io::{self, Write};
    let summary = summarize(&config.workspace_root);
    if summary.is_clean() {
        return Ok((0, 0));
    }
    println!("[warn] Control-plane secrets are inside the project tree:");
    for f in &summary.legacy {
        println!("  {} ({})", f.path.display(), f.what);
    }
    for f in &summary.token_files {
        println!("  {} (a live bearer token)", f.display());
    }
    println!("  Fix: move them to the TA state directory (each copy is verified before the old one is removed)");
    let go = if yes {
        println!("  -> Applying (--yes)");
        true
    } else {
        print!("  Apply fix? [y/N]: ");
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        input.trim().eq_ignore_ascii_case("y")
    };
    if !go {
        println!("  Skipped. The strict posture will keep refusing these until they are moved.");
        return Ok((0, 1));
    }

    let locator = StateLocator::from_env();
    let ps = ProjectState::open(&config.workspace_root, &locator)?;
    let opts = CustodyOpts::from_env(&locator);
    let report = state::migrate_project(&ps, opts);
    print_report(&report);
    audit_migration(config, &ps.project_id, &report);

    let tokens = token_bearing_files(&config.workspace_root);
    let mut token_problems = 0usize;
    for (path, res) in scrub_token_files(&tokens) {
        match res {
            Ok(()) => println!("  ok  removed the bearer token from {}", path.display()),
            Err(e) => {
                token_problems += 1;
                println!("  FAILED {e}");
            }
        }
    }
    let problems = report.problems().len() + token_problems;
    if problems > 0 {
        println!(
            "  {problems} item(s) need attention (nothing was deleted that could not be verified). \
             Resolve them, then re-run `ta doctor --fix`."
        );
        return Ok((0, 1));
    }
    println!("  State directory: {}", ps.dir.display());
    println!(
        "  Restart the daemon so it picks up the new locations: `ta daemon restart` (stop NATS \
         first if it was using the old nats-jetstream directory)."
    );
    Ok((1, 0))
}

fn print_report(report: &MigrationReport) {
    for item in &report.items {
        match &item.status {
            MigrationStatus::NothingToMigrate => {}
            MigrationStatus::Migrated => {
                println!(
                    "  ok  {} moved: {} -> {}",
                    item.name,
                    item.from.display(),
                    item.to
                )
            }
            MigrationStatus::StaleCopyRemoved => println!(
                "  ok  {} already present at {}; removed the stale copy {}",
                item.name,
                item.to,
                item.from.display()
            ),
            MigrationStatus::Conflict(m) => println!("  CONFLICT {}: {m}", item.name),
            MigrationStatus::Failed(m) => println!("  FAILED {}: {m}", item.name),
        }
    }
}

/// Pin the persona and project agent-config files a launch is about to use
/// (red-team CR-12 groundwork). The first sighting records each file's SHA-256
/// in the state directory, outside anything a project file can edit; a later
/// launch whose file differs prints a loud drift warning (once, then the new
/// content is pinned). This detects a changed persona or agent override; it
/// does not yet refuse the launch.
pub fn pin_launch_configs(project_root: &Path, persona: Option<&str>, agent: &str) {
    let mut rels: Vec<String> = Vec::new();
    if let Some(p) = persona {
        rels.push(format!(".ta/personas/{p}.toml"));
    }
    rels.push(format!(".ta/agents/{agent}.yaml"));
    let locator = StateLocator::from_env();
    let Ok(ps) = ProjectState::open(project_root, &locator) else {
        return;
    };
    for rel in rels {
        let Ok(bytes) = std::fs::read(project_root.join(&rel)) else {
            continue; // not present for this launch
        };
        match state::check_config_hash(&ps, &rel, &bytes) {
            state::HashCheck::Match => {}
            state::HashCheck::Unrecorded => {
                if let Err(e) = state::record_config_hash(&ps, &rel, &bytes) {
                    tracing::warn!(file = %rel, error = %e, "could not pin the launch config hash");
                }
            }
            state::HashCheck::Drift { recorded, actual } => {
                eprintln!(
                    "[warn] {rel} changed since the last launch (pinned sha256 {}..., now {}...). \
                     If you did not edit it, treat the project as tampered with: a changed persona \
                     or agent override can widen what an agent may do. The new content is now pinned.",
                    &recorded[..12.min(recorded.len())],
                    &actual[..12.min(actual.len())]
                );
                tracing::warn!(file = %rel, recorded = %recorded, actual = %actual, "launch config drift");
                if let Err(e) = state::record_config_hash(&ps, &rel, &bytes) {
                    tracing::warn!(file = %rel, error = %e, "could not re-pin the launch config hash");
                }
            }
        }
    }
}

/// `ta show state`: every location an installer or operator needs.
pub fn show_state(project_root: &Path) -> anyhow::Result<()> {
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(project_root, &locator)?;
    let opts = CustodyOpts::from_env(&locator);
    let os = match Os::current() {
        Os::MacOs => "macOS",
        Os::Linux => "Linux",
        Os::Windows => "Windows",
    };
    println!("TA state directory ({os})");
    println!("  State root:       {}", locator.root().display());
    println!(
        "  Project id:       {}   (recorded in {})",
        ps.project_id,
        project_root
            .join(".ta")
            .join(state::PROJECT_ID_FILENAME)
            .display()
    );
    println!("  Project state:    {}", ps.dir.display());
    println!("  Secrets (files):  {}", ps.secrets_dir().display());
    println!("  Daemon tokens:    {}", ps.daemon_tokens_path().display());
    println!("  Approval tokens:  {}", ps.tokens_dir().display());
    println!("  NATS JetStream:   {}", ps.nats_data_dir().display());
    println!("  Audit checkpoints:{}", ps.checkpoints_path().display());
    println!("  Config hashes:    {}", ps.hashes_path().display());
    println!(
        "  Key custody:      {}",
        if opts.use_keychain {
            "OS keychain first (service trusted-autonomy-control-plane), owner-only file fallback"
        } else {
            "owner-only files (OS keychain disabled: TA_NO_KEYCHAIN is set)"
        }
    );
    println!(
        "  Posture:          {}",
        match opts.posture {
            Posture::Strict => "strict (old-location secrets are refused)",
            Posture::Default => "default (old-location secrets are read with a warning)",
        }
    );
    println!(
        "  Daemon pid/port:  {}   (unchanged; stays in the project)",
        project_root.join(".ta").join("daemon.pid").display()
    );
    let summary = summarize(project_root);
    if summary.is_clean() {
        println!("  Old locations:    none (nothing secret remains in the project tree)");
    } else {
        println!(
            "  Old locations:    {} item(s) still in the project tree; run `ta doctor --fix`",
            summary.legacy.len() + summary.token_files.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_files_are_found_and_scrubbed_but_state_json_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let ta = dir.path().join(".ta");
        let sess = ta.join("team-sessions/s1");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("state.json"),
            r#"{"id":"s1","config":{"name":"s1","whiteboard_token":"TOKEN-SENTINEL","whiteboard_token_expires_at":"2030-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        let staged = ta.join("staging/g1/.ta");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(
            staged.join("whiteboard-session.json"),
            r#"{"token":"TOKEN-SENTINEL"}"#,
        )
        .unwrap();

        let found = token_bearing_files(dir.path());
        assert_eq!(found.len(), 2, "{found:?}");
        for (_, r) in scrub_token_files(&found) {
            r.unwrap();
        }
        assert!(token_bearing_files(dir.path()).is_empty());
        let after = std::fs::read_to_string(sess.join("state.json")).unwrap();
        assert!(!after.contains("TOKEN-SENTINEL"));
        assert!(after.contains("\"id\": \"s1\""));
        assert!(!staged.join("whiteboard-session.json").exists());
    }

    #[test]
    fn launch_configs_are_pinned_in_the_state_dir_and_drift_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let persona = dir.path().join(".ta/personas/cos.toml");
        std::fs::create_dir_all(persona.parent().unwrap()).unwrap();
        std::fs::write(&persona, "v1").unwrap();
        pin_launch_configs(dir.path(), Some("cos"), "claude-code");
        let ps = ProjectState::open(dir.path(), &StateLocator::from_env()).unwrap();
        assert_eq!(
            state::check_config_hash(&ps, ".ta/personas/cos.toml", b"v1"),
            state::HashCheck::Match
        );
        assert!(!dir.path().join(".ta/config-hashes.json").exists());
        std::fs::write(&persona, "v2 widened").unwrap();
        pin_launch_configs(dir.path(), Some("cos"), "claude-code");
        assert_eq!(
            state::check_config_hash(&ps, ".ta/personas/cos.toml", b"v2 widened"),
            state::HashCheck::Match,
            "after the warning the new content is pinned"
        );
    }

    #[test]
    fn summarize_is_clean_for_an_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(summarize(dir.path()).is_clean());
    }
}
