//! Control-plane secrets and the per-user TA state directory.
//!
//! * `ta show state` prints where everything lives, so installers and the
//!   people running them do not guess.
//! * `ta doctor` reports old-location secrets (including the credential
//!   vault's `credentials.key`), plaintext `*.cred` files and bearer tokens
//!   left in the project tree.
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
    /// Plaintext `*.cred` credential files inside the project tree (for
    /// example the `.ta/wayfinder-service-account.cred` a pairing script
    /// writes). TA cannot move these for the script that reads them, so they
    /// are reported with a manual fix.
    pub cred_files: Vec<PathBuf>,
}

impl SecretsSummary {
    pub fn is_clean(&self) -> bool {
        self.legacy.is_empty() && self.token_files.is_empty() && self.cred_files.is_empty()
    }
    /// Items `ta doctor --fix` can move or scrub on its own.
    pub fn fixable_count(&self) -> usize {
        self.legacy.len() + self.token_files.len()
    }
    pub fn item_count(&self) -> usize {
        self.fixable_count() + self.cred_files.len()
    }
}

pub fn summarize(project_root: &Path) -> SecretsSummary {
    let locator = StateLocator::from_env();
    SecretsSummary {
        posture: Posture::resolve(&locator),
        legacy: state::legacy_findings(project_root),
        token_files: token_bearing_files(project_root),
        cred_files: plaintext_cred_files(project_root),
    }
}

/// Plaintext `*.cred` files in the project root, in `.ta/`, and one level
/// below `.ta/` (never inside `.ta/staging`, which holds whole project
/// copies). Sorted for stable output.
pub fn plaintext_cred_files(project_root: &Path) -> Vec<PathBuf> {
    fn creds_in(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let is_cred = p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("cred"));
            if is_cred && e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let ta = project_root.join(".ta");
    let mut out = Vec::new();
    creds_in(project_root, &mut out);
    creds_in(&ta, &mut out);
    if let Ok(entries) = std::fs::read_dir(&ta) {
        for e in entries.flatten() {
            let p = e.path();
            let skip = p.file_name().is_some_and(|n| n == "staging");
            if !skip && e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                creds_in(&p, &mut out);
            }
        }
    }
    out.sort();
    out
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

/// The lines `ta doctor` shows for the secrets finding: every item that
/// `ta doctor --fix` would move or scrub, then the files it cannot fix itself.
pub fn describe_items(summary: &SecretsSummary) -> Vec<String> {
    let mut out: Vec<String> = summary
        .legacy
        .iter()
        .map(|f| format!("{} ({})", f.path.display(), f.what))
        .collect();
    out.extend(
        summary
            .token_files
            .iter()
            .map(|f| format!("{} (a live bearer token)", f.display())),
    );
    out
}

/// Result of applying the secrets fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretsFixOutcome {
    /// Items moved, verified and removed from the project tree.
    pub moved: usize,
    /// Items that still need attention (nothing unverified was deleted).
    pub problems: usize,
}

/// `ta doctor --fix` for control-plane secrets: move every old-location
/// secret (including `credentials.key`) to the state directory with a
/// verified copy, and scrub bearer tokens. Never prompts; the caller has
/// already decided. Safe to re-run.
pub fn apply_fix(config: &GatewayConfig) -> anyhow::Result<SecretsFixOutcome> {
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(&config.workspace_root, &locator)?;
    let opts = CustodyOpts::from_env(&locator);
    let report = state::migrate_project(&ps, opts);
    print_report(&report);
    audit_migration(config, &ps.project_id, &report);
    let moved = report
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.status,
                MigrationStatus::Migrated | MigrationStatus::StaleCopyRemoved
            )
        })
        .count();

    let tokens = token_bearing_files(&config.workspace_root);
    let mut token_problems = 0usize;
    let mut scrubbed = 0usize;
    for (path, res) in scrub_token_files(&tokens) {
        match res {
            Ok(()) => {
                scrubbed += 1;
                println!("  ok  removed the bearer token from {}", path.display());
            }
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
    } else {
        println!("  State directory: {}", ps.dir.display());
        println!(
            "  Restart the daemon so it picks up the new locations: `ta daemon restart` (stop \
             NATS first if it was using the old nats-jetstream directory)."
        );
    }
    Ok(SecretsFixOutcome {
        moved: moved + scrubbed,
        problems,
    })
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
///
/// Strictly read-only: it never mints a project id, never creates the state
/// directory and never touches the project tree. A project that has not stored
/// a secret yet reports "no project id yet" and where state will go.
pub fn show_state(project_root: &Path) -> anyhow::Result<()> {
    show_state_with(project_root, &StateLocator::from_env())
}

pub(crate) fn show_state_with(project_root: &Path, locator: &StateLocator) -> anyhow::Result<()> {
    let opts = CustodyOpts::from_env(locator);
    let os = match Os::current() {
        Os::MacOs => "macOS",
        Os::Linux => "Linux",
        Os::Windows => "Windows",
    };
    let id_file = project_root.join(".ta").join(state::PROJECT_ID_FILENAME);
    println!("TA state directory ({os})");
    println!("  State root:       {}", locator.root().display());
    match ProjectState::locate(project_root, locator)? {
        Some(ps) => {
            println!(
                "  Project id:       {}   (recorded in {})",
                ps.project_id,
                id_file.display()
            );
            println!("  Project state:    {}", ps.dir.display());
            println!("  Secrets (files):  {}", ps.secrets_dir().display());
            println!("  Daemon tokens:    {}", ps.daemon_tokens_path().display());
            println!("  Approval tokens:  {}", ps.tokens_dir().display());
            println!("  NATS JetStream:   {}", ps.nats_data_dir().display());
            println!("  Audit checkpoints:{}", ps.checkpoints_path().display());
            println!("  Config hashes:    {}", ps.hashes_path().display());
        }
        None => {
            println!(
                "  Project id:       no project id yet (nothing has been stored for this \
                 project; {} is created the first time a command stores a secret)",
                id_file.display()
            );
            println!(
                "  Project state:    {}/<project-id>/ (not created yet)",
                locator.projects_dir().display()
            );
        }
    }
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
            summary.item_count()
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

    fn snapshot(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.insert(rel, std::fs::read(&p).unwrap_or_default());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    #[test]
    fn show_state_is_read_only_and_leaves_the_tree_byte_identical() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        std::fs::write(project.path().join(".ta/workflow.toml"), "x = 1\n").unwrap();
        std::fs::write(project.path().join("PLAN.md"), "# plan\n").unwrap();
        let state = tempfile::tempdir().unwrap();
        let locator = StateLocator::at(state.path());
        let (before_tree, before_state) = (snapshot(project.path()), snapshot(state.path()));
        let dirs_before = std::fs::read_dir(state.path()).unwrap().count();

        show_state_with(project.path(), &locator).unwrap();

        assert_eq!(snapshot(project.path()), before_tree);
        assert_eq!(snapshot(state.path()), before_state);
        assert_eq!(
            std::fs::read_dir(state.path()).unwrap().count(),
            dirs_before
        );
        assert!(
            !project.path().join(".ta/project-id").exists(),
            "a read-only command must not mint a project id"
        );
    }

    #[test]
    fn show_state_reports_the_recorded_id_when_one_exists_without_writing() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let locator = StateLocator::at(state.path());
        // A real write path (what a command that stores a secret does).
        ProjectState::open(project.path(), &locator).unwrap();
        let (before_tree, before_state) = (snapshot(project.path()), snapshot(state.path()));
        show_state_with(project.path(), &locator).unwrap();
        assert_eq!(snapshot(project.path()), before_tree);
        assert_eq!(snapshot(state.path()), before_state);
    }

    #[test]
    fn plaintext_cred_files_are_found_in_the_root_and_under_dot_ta_but_not_in_staging() {
        let dir = tempfile::tempdir().unwrap();
        let ta = dir.path().join(".ta");
        std::fs::create_dir_all(ta.join("sub")).unwrap();
        std::fs::create_dir_all(ta.join("staging/g1/.ta")).unwrap();
        std::fs::write(ta.join("wayfinder-service-account.cred"), "s").unwrap();
        std::fs::write(ta.join("sub/other.CRED"), "s").unwrap();
        std::fs::write(dir.path().join("root.cred"), "s").unwrap();
        std::fs::write(ta.join("staging/g1/.ta/copy.cred"), "s").unwrap();
        std::fs::write(ta.join("notes.txt"), "s").unwrap();
        let found: Vec<String> = plaintext_cred_files(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.contains(&"wayfinder-service-account.cred".to_string()));
        assert!(!found.contains(&"copy.cred".to_string()));
        let summary = summarize(dir.path());
        assert!(!summary.is_clean());
        assert_eq!(summary.fixable_count(), 0);
        assert_eq!(summary.item_count(), 3);
    }

    #[test]
    fn the_vault_key_in_the_tree_is_a_secrets_finding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/credentials.key"), "AGE-SECRET-KEY-1X").unwrap();
        let summary = summarize(dir.path());
        assert_eq!(summary.fixable_count(), 1);
        assert!(describe_items(&summary)[0].contains("credentials.key"));
        assert!(describe_items(&summary)[0].contains("vault"));
    }
}
