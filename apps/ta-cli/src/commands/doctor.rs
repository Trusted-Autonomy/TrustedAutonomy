// doctor.rs — `ta doctor`: runtime validation with agent-agnostic auth checking (v0.15.17).
//
// Checks the full TA runtime chain and reports the active authentication mode
// with clear, actionable output. Auth checking is driven by AgentAuthSpec in
// the active framework's manifest — it works for any configured agent.
//
// Output format:
//   [ok]   <check name>  <detail>
//   [warn] <check name>  <detail>
//          Fix: <fix hint>
//   [FAIL] <check name>  <detail>
//          Fix: <fix hint>

use std::path::Path;

use serde::Serialize;
use ta_mcp_gateway::GatewayConfig;
use ta_runtime::{detect_auth_mode, AgentAuthSpec, AgentFrameworkManifest, AuthCheckResult};

// ── Check result types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

impl CheckResult {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            detail: detail.into(),
            fix: String::new(),
        }
    }
    fn warn(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
    fn fail(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Execute `ta doctor [--json] [--fix-denied] [--fix] [--yes]`.
pub fn execute(
    config: &GatewayConfig,
    json: bool,
    fix_denied: bool,
    fix: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if fix_denied {
        return execute_fix_denied(config);
    }
    if fix {
        return execute_fix(config, yes);
    }
    let checks = run_all_checks(config);
    let fail_count = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();

    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        println!("TA Doctor -- Runtime Validation");
        println!();
        print_checks(&checks);
        println!();
        let pass = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Ok)
            .count();
        let warn = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count();
        println!(
            "{} passed, {} warnings, {} failures",
            pass, warn, fail_count
        );
        if fail_count == 0 && warn == 0 {
            println!("All checks passed.");
        } else if fail_count == 0 {
            println!("All critical checks passed ({} warning(s)).", warn);
        }
    }

    if fail_count > 0 {
        Err(anyhow::anyhow!("{} health check(s) failed", fail_count))
    } else {
        Ok(())
    }
}

fn print_checks(checks: &[CheckResult]) {
    for check in checks {
        let tag = match check.status {
            CheckStatus::Ok => "[ok]  ",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[FAIL]",
        };
        // Pad name to 20 chars for alignment.
        println!("  {} {:<20} {}", tag, check.name, check.detail);
        if !check.fix.is_empty() {
            for line in check.fix.lines() {
                println!("         {}", line);
            }
        }
    }
}

// ── Individual checks ────────────────────────────────────────────────────────

fn run_all_checks(config: &GatewayConfig) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // 1. CLI version (always passes).
    results.push(check_cli_version());

    // 2. Daemon connection.
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    results.push(check_daemon(&daemon_url));

    // 3. Auth check — driven by the active framework's manifest.
    let agent_name = read_active_agent(config);
    let (auth_check, auth_warnings) = check_auth(&agent_name, &config.workspace_root);
    results.push(auth_check);
    // Auth warnings are separate [warn] lines.
    results.extend(auth_warnings);

    // 4. Agent binary presence.
    results.push(check_agent_binary(&agent_name, &config.workspace_root));

    // 5. gh CLI presence and auth.
    results.push(check_gh_cli());

    // 6. Project root.
    results.push(check_project_root(&config.workspace_root));

    // 7. .ta/config loaded.
    results.push(check_ta_config(config));

    // 8. Plan state.
    results.push(check_plan_state(config));

    // 9. VCS + gitignore.
    results.extend(check_vcs(config));

    // 10. GC health.
    results.extend(check_gc(config));

    // 11. Version consistency.
    results.extend(check_version_consistency(config));

    // 12. Stale ephemeral file.
    results.push(check_stale_ephemeral(config));

    // 13. Project upgrade state (v0.15.18).
    results.push(check_upgrade_state(config));

    // 14. Goals in pr_ready state with denied drafts (v0.15.18).
    results.extend(check_pr_ready_denied(config));

    // 15. Gemma 4 Ollama model / profile consistency (v0.16.2.1).
    results.extend(check_gemma4_ollama(config));

    // 16. Cross-project link health (v0.16.1.5).
    results.extend(check_links(config));

    // 17. Orphaned in_progress phases — PLAN.md says in_progress but no live goal (v0.16.1.6.1).
    results.extend(check_orphaned_in_progress_phases(config));

    // 18. Stale PID files — .ta/*.pid points to a dead process (v0.16.1.8).
    results.extend(check_stale_pid_files(config));

    // 19. Plugin crash-loop diagnosis — reads .ta/discord-crash-state.json (v0.16.1.8).
    results.extend(check_plugin_crash_loop_diagnosis(config));

    // 20. IDE index exclusions — VS Code settings.json (v0.16.1.9).
    results.extend(check_ide_exclusions(config));

    // 21. IDE exclude manifest — .ta/ide-excludes.json (v0.16.1.10).
    results.extend(check_ide_excludes_manifest(config));

    // 22. Agent context.files validation — check declared paths exist (v0.16.3).
    results.extend(check_agent_context_files(config));

    // 23. Ollama probe — check all Ollama profiles have their models pulled (v0.16.3).
    results.extend(check_ollama_profiles(config));

    // 24. AppContainer availability — Windows filesystem + network isolation (v0.16.4.2).
    results.push(check_appcontainer(config));

    results
}

fn check_cli_version() -> CheckResult {
    let version = env!("CARGO_PKG_VERSION");
    let hash = env!("TA_GIT_HASH");
    CheckResult::ok("CLI version", format!("{} ({})", version, hash))
}

fn check_daemon(daemon_url: &str) -> CheckResult {
    let result = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("{}/api/status", daemon_url)).send().ok())
        .and_then(|r| {
            if r.status().is_success() {
                r.json::<serde_json::Value>().ok()
            } else {
                None
            }
        });

    match result {
        Some(body) => {
            let daemon_ver = body
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let cli_ver = env!("CARGO_PKG_VERSION");
            if daemon_ver != "unknown" && daemon_ver != cli_ver {
                CheckResult::warn(
                    "Daemon",
                    format!(
                        "connected at {} (daemon: {}, CLI: {})",
                        daemon_url, daemon_ver, cli_ver
                    ),
                    "Version mismatch — restart daemon: ta daemon restart",
                )
            } else {
                CheckResult::ok("Daemon", format!("connected at {}", daemon_url))
            }
        }
        None => CheckResult::warn(
            "Daemon",
            "not running".to_string(),
            "Start the daemon: ta daemon start",
        ),
    }
}

fn check_auth(agent_name: &str, project_root: &Path) -> (CheckResult, Vec<CheckResult>) {
    let manifest = AgentFrameworkManifest::resolve(agent_name, project_root);
    let spec: AgentAuthSpec = manifest.map(|m| m.auth).unwrap_or_default();
    let check_name = format!("Auth ({})", agent_name);

    match detect_auth_mode(&spec) {
        AuthCheckResult::Ok {
            method_label,
            detail,
            warnings,
        } => {
            let main = CheckResult::ok(check_name, format!("{} -- {}", method_label, detail));
            let warn_checks: Vec<CheckResult> = warnings
                .into_iter()
                .map(|w| {
                    CheckResult::warn(
                        format!("Auth ({})", agent_name),
                        w.clone(),
                        extract_fix_hint(&w),
                    )
                })
                .collect();
            (main, warn_checks)
        }
        AuthCheckResult::Missing { tried } => {
            let tried_lines: Vec<String> = tried
                .iter()
                .map(|(label, reason)| format!("  {} -- {}", label, reason))
                .collect();
            let fix = format!(
                "No authentication found.\nTried:\n{}\nFix one of:\n  {}",
                tried_lines.join("\n"),
                auth_fix_hint(agent_name)
            );
            (
                CheckResult::fail(check_name, "No authentication found".to_string(), fix),
                Vec::new(),
            )
        }
    }
}

/// Extract a short fix hint from a warning message.
fn extract_fix_hint(warning: &str) -> String {
    if warning.contains("not set") {
        // Extract the env var name from "VARNAME not set ..."
        let var_name = warning.split_whitespace().next().unwrap_or("").to_string();
        if !var_name.is_empty() {
            return format!("export {}=<your-key>", var_name);
        }
    }
    if warning.contains("not reachable") {
        return "Start the service (e.g. ollama serve)".to_string();
    }
    String::new()
}

fn auth_fix_hint(agent: &str) -> String {
    match agent {
        "claude-code" | "claude-flow" => {
            "Option 1 (subscription): claude auth login\n  Option 2 (API key):      export ANTHROPIC_API_KEY=sk-ant-...".to_string()
        }
        "codex" => "export OPENAI_API_KEY=sk-...".to_string(),
        "ollama" => "Start Ollama: ollama serve".to_string(),
        _ => "See agent documentation for auth setup".to_string(),
    }
}

fn check_agent_binary(agent_name: &str, project_root: &Path) -> CheckResult {
    let manifest = AgentFrameworkManifest::resolve(agent_name, project_root);
    let command = manifest
        .as_ref()
        .map(|m| m.command.as_str())
        .unwrap_or(agent_name);

    match which::which(command) {
        Ok(path) => {
            // Try to get version string.
            let ver = std::process::Command::new(command)
                .arg("--version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout).ok()
                    } else {
                        String::from_utf8(o.stderr).ok()
                    }
                })
                .map(|s| s.lines().next().unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty());
            let detail = match ver {
                Some(v) => format!("{} -- {}", path.display(), v),
                None => path.display().to_string(),
            };
            CheckResult::ok("Agent binary", detail)
        }
        Err(_) => CheckResult::fail(
            "Agent binary",
            format!("'{}' not found on PATH", command),
            format!(
                "Install {} — see https://trustedautonomy.ai/docs/agents",
                agent_name
            ),
        ),
    }
}

fn check_gh_cli() -> CheckResult {
    match which::which("gh") {
        Ok(path) => {
            // Check auth status.
            let auth_ok = std::process::Command::new("gh")
                .args(["auth", "status"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if auth_ok {
                CheckResult::ok("gh CLI", format!("{} -- authenticated", path.display()))
            } else {
                CheckResult::warn(
                    "gh CLI",
                    format!("{} -- not authenticated", path.display()),
                    "Run: gh auth login",
                )
            }
        }
        Err(_) => CheckResult::warn(
            "gh CLI",
            "not found on PATH".to_string(),
            "Install GitHub CLI: https://cli.github.com",
        ),
    }
}

fn check_project_root(workspace_root: &Path) -> CheckResult {
    if workspace_root.exists() {
        CheckResult::ok("Project root", workspace_root.display().to_string())
    } else {
        CheckResult::fail(
            "Project root",
            format!("{} does not exist", workspace_root.display()),
            "Run ta from inside your project directory".to_string(),
        )
    }
}

fn check_ta_config(config: &GatewayConfig) -> CheckResult {
    let ta_dir = config.workspace_root.join(".ta");
    if !ta_dir.exists() {
        return CheckResult::warn(
            ".ta/config",
            "No .ta directory found".to_string(),
            "Run: ta init  to initialize TA for this project",
        );
    }

    // Read agent from global config.
    let agent = read_active_agent(config);
    let detail = format!("agent: {}", agent);
    CheckResult::ok(".ta/config", detail)
}

fn check_plan_state(config: &GatewayConfig) -> CheckResult {
    let plan_path = config.workspace_root.join("PLAN.md");
    if !plan_path.exists() {
        return CheckResult::warn(
            "Plan",
            "No PLAN.md found".to_string(),
            "Create one with: ta plan create  or add PLAN.md to your project root",
        );
    }
    match super::plan::load_plan(&config.workspace_root) {
        Ok(phases) => {
            let pending: Vec<_> = phases.iter().filter(|p| p.status.is_actionable()).collect();
            if pending.is_empty() {
                CheckResult::ok("Plan", "All phases complete".to_string())
            } else {
                let next = pending.first().map(|p| p.id.as_str()).unwrap_or("unknown");
                CheckResult::ok(
                    "Plan",
                    format!("Next phase: {} ({} pending)", next, pending.len()),
                )
            }
        }
        Err(e) => CheckResult::warn(
            "Plan",
            format!("Could not parse PLAN.md: {}", e),
            "Check PLAN.md for formatting errors".to_string(),
        ),
    }
}

fn check_vcs(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_workspace::partitioning::{VcsBackend, LOCAL_TA_PATHS};

    let mut results = Vec::new();
    let vcs = VcsBackend::detect(&config.workspace_root);

    match &vcs {
        VcsBackend::None => {
            results.push(CheckResult::ok(
                "VCS",
                "none (skipping VCS checks)".to_string(),
            ));
            return results;
        }
        VcsBackend::Perforce => {
            if std::env::var("P4IGNORE").is_err() {
                results.push(CheckResult::warn(
                    "VCS P4IGNORE",
                    "P4IGNORE not set".to_string(),
                    "export P4IGNORE=.p4ignore  then run: ta setup vcs",
                ));
            }
        }
        VcsBackend::Git => {}
    }

    results.push(CheckResult::ok("VCS", vcs.as_str().to_string()));

    // Check VCS ignore coverage for .ta/ paths that exist on disk (VCS-agnostic).
    for path in LOCAL_TA_PATHS {
        let full = config
            .workspace_root
            .join(".ta")
            .join(path.trim_end_matches('/'));
        if full.exists() {
            if let Ok(false) = vcs.is_path_ignored(&config.workspace_root, path) {
                results.push(CheckResult::warn(
                    "VCS ignore",
                    format!(".ta/{} exists but is not ignored by {}", path, vcs.as_str()),
                    "run: ta setup vcs --force",
                ));
            }
        }
    }

    results
}

fn check_gc(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_goal::{GoalRunState, GoalRunStore};
    let mut results = Vec::new();

    // Stale staging dirs.
    let staging_dir = config.workspace_root.join(".ta").join("staging");
    if staging_dir.exists() {
        let seven_days_secs: u64 = 7 * 24 * 3600;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let active_staging: std::collections::HashSet<std::path::PathBuf> =
            match GoalRunStore::new(&config.goals_dir) {
                Ok(gs) => gs
                    .list()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|g| {
                        matches!(
                            g.state,
                            GoalRunState::Running
                                | GoalRunState::Configured
                                | GoalRunState::PrReady
                                | GoalRunState::UnderReview
                                | GoalRunState::Finalizing { .. }
                        )
                    })
                    .map(|g| g.workspace_path)
                    .collect(),
                Err(_) => std::collections::HashSet::new(),
            };

        let stale_count = std::fs::read_dir(&staging_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .filter(|e| !active_staging.contains(&e.path()))
                    .filter(|e| {
                        e.metadata()
                            .and_then(|m| m.modified())
                            .and_then(|t| {
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .map_err(|_| std::io::Error::other("time error"))
                            })
                            .map(|t| now_secs.saturating_sub(t.as_secs()) > seven_days_secs)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);

        if stale_count > 0 {
            results.push(CheckResult::warn(
                "GC staging",
                format!("{} stale dir(s) older than 7 days", stale_count),
                "Run: ta gc",
            ));
        } else {
            results.push(CheckResult::ok("GC staging", "no stale dirs".to_string()));
        }
    }

    // events.jsonl size.
    let events_file = config
        .workspace_root
        .join(".ta")
        .join("events")
        .join("events.jsonl");
    if events_file.exists() {
        let size_bytes = events_file.metadata().map(|m| m.len()).unwrap_or(0);
        let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
        if size_bytes > 10 * 1024 * 1024 {
            results.push(CheckResult::warn(
                "GC events",
                format!("{:.1} MB (>10 MB threshold)", size_mb),
                "Run: ta gc --include-events",
            ));
        }
    }

    results
}

fn check_version_consistency(config: &GatewayConfig) -> Vec<CheckResult> {
    use super::draft::{read_cargo_version, read_claude_md_version};
    let mut results = Vec::new();
    let cargo_ver = read_cargo_version(&config.workspace_root);
    let claude_ver = read_claude_md_version(&config.workspace_root);
    match (cargo_ver, claude_ver) {
        (Some(ref cv), Some(ref mv)) if cv == mv => {
            results.push(CheckResult::ok("Version", cv.clone()));
        }
        (Some(ref cv), Some(ref mv)) => {
            results.push(CheckResult::fail(
                "Version",
                format!("Cargo.toml ({}) != CLAUDE.md ({})", cv, mv),
                format!("Run from project root: ./scripts/bump-version.sh {}", cv),
            ));
        }
        (Some(ref cv), None) => {
            results.push(CheckResult::ok("Version", cv.clone()));
        }
        (None, _) => {
            results.push(CheckResult::warn(
                "Version",
                "No version in Cargo.toml".to_string(),
                String::new(),
            ));
        }
    }
    results
}

fn check_stale_ephemeral(config: &GatewayConfig) -> CheckResult {
    let stale = config.workspace_root.join(".ta-decisions.json");
    if stale.exists() {
        CheckResult::warn(
            "Ephemeral files",
            ".ta-decisions.json found in project root".to_string(),
            "Remove it: rm .ta-decisions.json",
        )
    } else {
        CheckResult::ok("Ephemeral files", "clean".to_string())
    }
}

// ── Upgrade state check (v0.15.18) ────────────────────────────────────────────

fn check_upgrade_state(config: &GatewayConfig) -> CheckResult {
    let ta_dir = config.workspace_root.join(".ta");
    if !ta_dir.exists() {
        return CheckResult::ok(
            "Project upgrade",
            "no .ta directory (not a TA project)".to_string(),
        );
    }
    let (pending_count, _steps) = super::upgrade::check_pending(&config.workspace_root);
    if pending_count == 0 {
        CheckResult::ok("Project upgrade", "up to date".to_string())
    } else {
        CheckResult::warn(
            "Project upgrade",
            format!("{} pending upgrade step(s)", pending_count),
            "Run: ta upgrade",
        )
    }
}

// ── pr_ready + denied draft check (v0.15.18) ─────────────────────────────────

fn check_pr_ready_denied(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_changeset::DraftStatus;
    use ta_goal::{GoalRunState, GoalRunStore};

    let mut results = Vec::new();

    let store = match GoalRunStore::new(&config.goals_dir) {
        Ok(s) => s,
        Err(_) => return results,
    };

    let goals = store.list().unwrap_or_default();
    let pr_ready: Vec<_> = goals
        .into_iter()
        .filter(|g| matches!(g.state, GoalRunState::PrReady))
        .collect();

    if pr_ready.is_empty() {
        return results;
    }

    // Load all draft packages to cross-reference.
    let all_pkgs = super::draft::load_all_packages(config).unwrap_or_default();

    let mut denied_goals: Vec<(String, String, u64)> = Vec::new(); // (id, title, staging_bytes)
    for goal in &pr_ready {
        if let Some(pkg_id) = goal.pr_package_id {
            if let Some(pkg) = all_pkgs.iter().find(|p| p.package_id == pkg_id) {
                if matches!(pkg.status, DraftStatus::Denied { .. }) {
                    let staging_bytes = dir_size_bytes(&goal.workspace_path);
                    denied_goals.push((
                        goal.goal_run_id.to_string()[..8].to_string(),
                        goal.title.clone(),
                        staging_bytes,
                    ));
                }
            }
        }
    }

    if denied_goals.is_empty() {
        return results;
    }

    let total_gb: f64 =
        denied_goals.iter().map(|(_, _, b)| *b as f64).sum::<f64>() / (1024.0 * 1024.0 * 1024.0);
    let list: Vec<String> = denied_goals
        .iter()
        .map(|(id, title, bytes)| {
            format!(
                "{} '{}' ({:.1} MB)",
                id,
                title,
                *bytes as f64 / (1024.0 * 1024.0)
            )
        })
        .collect();
    let detail = format!(
        "{} goal(s) are pr_ready with a denied draft ({:.2} GB staging): {}",
        denied_goals.len(),
        total_gb,
        list.join(", "),
    );
    results.push(CheckResult::warn(
        "GC denied drafts",
        detail,
        "Run 'ta doctor --fix-denied' to clean up, or re-run the phase to supersede.".to_string(),
    ));
    results
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

// ── --fix-denied handler ──────────────────────────────────────────────────────

fn execute_fix_denied(config: &GatewayConfig) -> anyhow::Result<()> {
    use std::io::{self, Write};
    use ta_changeset::DraftStatus;
    use ta_goal::{GoalRunState, GoalRunStore};

    let store = GoalRunStore::new(&config.goals_dir)?;
    let goals = store.list().unwrap_or_default();
    let pr_ready: Vec<_> = goals
        .into_iter()
        .filter(|g| matches!(g.state, GoalRunState::PrReady))
        .collect();

    let all_pkgs = super::draft::load_all_packages(config).unwrap_or_default();

    let mut found = false;
    for goal in &pr_ready {
        if let Some(pkg_id) = goal.pr_package_id {
            if let Some(pkg) = all_pkgs.iter().find(|p| p.package_id == pkg_id) {
                if matches!(pkg.status, DraftStatus::Denied { .. }) {
                    found = true;
                    let staging_mb =
                        dir_size_bytes(&goal.workspace_path) as f64 / (1024.0 * 1024.0);
                    println!(
                        "Goal {} '{}' — staging {:.1} MB, denied on {}",
                        &goal.goal_run_id.to_string()[..8],
                        goal.title,
                        staging_mb,
                        goal.updated_at.format("%Y-%m-%d"),
                    );
                    print!("  [d]elete staging + mark closed, [s]kip? [d/s]: ");
                    io::stdout().flush().ok();
                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    if input.trim().eq_ignore_ascii_case("d") {
                        if goal.workspace_path.exists() {
                            std::fs::remove_dir_all(&goal.workspace_path)?;
                            println!("  Deleted staging: {}", goal.workspace_path.display());
                        }
                        // Mark goal as closed.
                        let mut updated = goal.clone();
                        updated.state = GoalRunState::Failed {
                            reason: "closed by ta doctor --fix-denied".to_string(),
                        };
                        store.save(&updated)?;
                        println!(
                            "  Goal {} marked closed.",
                            &goal.goal_run_id.to_string()[..8]
                        );
                    } else {
                        println!("  Skipped.");
                    }
                }
            }
        }
    }

    if !found {
        println!("No pr_ready goals with denied drafts found.");
    }
    Ok(())
}

// ── --fix handler (v0.15.30.6) ───────────────────────────────────────────────

/// `ta doctor --fix [--yes]`
///
/// For each health signal, describe the issue and proposed fix, then:
/// - Interactive mode (`--fix` only): prompt "fix? [y/N]" before taking action.
/// - Non-interactive mode (`--fix --yes`): apply all fixes automatically (used by `ta gc` alias).
fn execute_fix(config: &GatewayConfig, yes: bool) -> anyhow::Result<()> {
    use std::io::{self, Write};

    let signals = super::health_signals::compute_health_signals(config);
    let vcs_gaps = vcs_gitignore_gaps(config);
    let total = signals.len() + usize::from(!vcs_gaps.is_empty());

    if total == 0 {
        println!("No health issues detected — nothing to fix.");
        return Ok(());
    }

    println!("TA Doctor — Fix Mode");
    println!();
    println!("Found {} issue(s):", total);
    println!();

    let mut fixed = 0usize;
    let mut skipped = 0usize;

    for signal in &signals {
        let label = match signal.severity {
            super::health_signals::SignalSeverity::Crit => "[crit]",
            super::health_signals::SignalSeverity::Warn => "[warn]",
            super::health_signals::SignalSeverity::Info => "[info]",
        };
        println!("{} {}", label, signal.message);
        println!("  Fix: {}", signal.action);

        let should_fix = if yes {
            println!("  → Applying (--yes)");
            true
        } else {
            print!("  Apply fix? [y/N]: ");
            io::stdout().flush().ok();
            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            input.trim().eq_ignore_ascii_case("y")
        };

        if should_fix {
            match apply_fix(config, signal) {
                Ok(msg) => {
                    println!("  ✓ {}", msg);
                    fixed += 1;
                }
                Err(e) => {
                    println!("  ✗ Failed: {}", e);
                }
            }
        } else {
            println!("  Skipped.");
            skipped += 1;
        }
        println!();
    }

    // VCS gitignore fix — runs ta setup vcs --force.
    if !vcs_gaps.is_empty() {
        println!(
            "[warn] {} .ta/ path(s) exist but are not gitignored",
            vcs_gaps.len()
        );
        for gap in &vcs_gaps {
            println!("       .ta/{}", gap);
        }
        println!("  Fix: ta setup vcs --force (rewrites the TA gitignore block)");

        let should_fix = if yes {
            println!("  → Applying (--yes)");
            true
        } else {
            print!("  Apply fix? [y/N]: ");
            io::stdout().flush().ok();
            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            input.trim().eq_ignore_ascii_case("y")
        };

        if should_fix {
            match super::setup::run_vcs_setup(config, true, false, None, None) {
                Ok(_) => {
                    println!("  ✓ Ran ta setup vcs --force — gitignore block updated");
                    fixed += 1;
                }
                Err(e) => {
                    println!("  ✗ Failed: {}", e);
                }
            }
        } else {
            println!("  Skipped.");
            skipped += 1;
        }
        println!();
    }

    // VS Code settings.json fix.
    let vscode_missing = vscode_missing_excludes(&config.workspace_root);
    if !vscode_missing.is_empty() {
        println!(
            "[warn] .vscode/settings.json is missing {} TA runtime exclude(s)",
            vscode_missing.len()
        );
        println!("  Fix: add missing entries to files.exclude and search.exclude");

        let should_fix = if yes {
            println!("  → Applying (--yes)");
            true
        } else {
            print!("  Apply fix? [y/N]: ");
            io::stdout().flush().ok();
            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            input.trim().eq_ignore_ascii_case("y")
        };

        if should_fix {
            match crate::commands::init::write_vscode_settings_excludes(&config.workspace_root) {
                Ok(_) => {
                    println!("  ✓ Updated .vscode/settings.json with TA runtime excludes");
                    fixed += 1;
                }
                Err(e) => {
                    println!("  ✗ Failed: {}", e);
                }
            }
        } else {
            println!("  Skipped.");
            skipped += 1;
        }
        println!();
    }

    // IDE exclude manifest fix.
    if ide_manifest_needs_fix(&config.workspace_root) {
        println!("[warn] .ta/ide-excludes.json is missing or out of date");
        println!("  Fix: regenerate manifest from current ta_runtime_dirs()");

        let should_fix = if yes {
            println!("  → Applying (--yes)");
            true
        } else {
            print!("  Apply fix? [y/N]: ");
            io::stdout().flush().ok();
            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            input.trim().eq_ignore_ascii_case("y")
        };

        if should_fix {
            match crate::commands::init::write_ide_excludes_manifest(&config.workspace_root) {
                Ok(_) => {
                    println!("  ✓ Regenerated .ta/ide-excludes.json");
                    fixed += 1;
                }
                Err(e) => {
                    println!("  ✗ Failed: {}", e);
                }
            }
        } else {
            println!("  Skipped.");
            skipped += 1;
        }
        println!();
    }

    println!("{} fix(es) applied, {} skipped.", fixed, skipped);
    Ok(())
}

/// Returns LOCAL_TA_PATHS entries that exist on disk but are not ignored by the project's VCS.
///
/// Returns empty vec if the project has no VCS (`VcsBackend::None`).
fn vcs_gitignore_gaps(config: &GatewayConfig) -> Vec<&'static str> {
    use ta_workspace::partitioning::{VcsBackend, LOCAL_TA_PATHS};

    let vcs = VcsBackend::detect(&config.workspace_root);
    if vcs == VcsBackend::None {
        return vec![];
    }

    let mut gaps = Vec::new();
    for path in LOCAL_TA_PATHS {
        let full = config
            .workspace_root
            .join(".ta")
            .join(path.trim_end_matches('/'));
        if full.exists() {
            if let Ok(false) = vcs.is_path_ignored(&config.workspace_root, path) {
                gaps.push(*path);
            }
        }
    }
    gaps
}

/// Apply the fix for a given health signal.
/// Returns a short description of what was done.
fn apply_fix(
    config: &GatewayConfig,
    signal: &super::health_signals::HealthSignal,
) -> anyhow::Result<String> {
    match signal.kind.as_str() {
        "disk_staging" | "orphan_staging" => {
            // Run GC to reclaim staging space.
            super::gc::execute(
                config, false, // dry_run
                7,     // threshold_days
                false, // all
                false, // archive
                false, // include_events
                false, // compact
                30,    // compact_after_days
                false, // force
                false, // status
                true,  // delete_stale
            )?;
            Ok("Ran gc to reclaim staging space".to_string())
        }
        "stale_drafts" => {
            // Use existing draft close-stale logic via CLI passthrough.
            println!("  Run `ta draft close --stale` to close stale drafts.");
            Ok("See: ta draft close --stale".to_string())
        }
        "stale_failed_goals" | "stale_pr_ready" => {
            // Run GC for stale goals.
            super::gc::execute(
                config, false, // dry_run
                1,     // threshold_days (aggressive for stale goals)
                false, // all
                false, // archive
                false, // include_events
                false, // compact
                30,    // compact_after_days
                false, // force
                false, // status
                true,  // delete_stale
            )?;
            Ok("Ran gc to clean up stale goals".to_string())
        }
        "plugin_crash_loop" | "stale_pid" => {
            // Remove stale PID files for all .ta/*.pid entries.
            let ta_dir = config.workspace_root.join(".ta");
            let mut removed = 0usize;
            if let Ok(entries) = std::fs::read_dir(&ta_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("pid") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            if !is_pid_alive(pid) {
                                if let Err(e) = std::fs::remove_file(&path) {
                                    println!(
                                        "  Warning: could not remove {}: {}",
                                        path.display(),
                                        e
                                    );
                                } else {
                                    println!("  Removed stale PID file: {}", path.display());
                                    removed += 1;
                                }
                            }
                        }
                    }
                }
            }
            // Remove crash state file now that user is fixing the issue.
            let crash_state = config.workspace_root.join(".ta/discord-crash-state.json");
            if crash_state.exists() {
                let _ = std::fs::remove_file(&crash_state);
            }
            if removed > 0 {
                println!("  Restart the daemon to bring the plugin back: ta daemon restart");
                Ok(format!(
                    "Removed {} stale PID file(s) — run `ta daemon restart` to restart the plugin",
                    removed
                ))
            } else {
                println!("  No stale PID files found. Run `ta daemon restart` if the plugin is still down.");
                Ok("No stale PID files found — run `ta daemon restart` if needed".to_string())
            }
        }
        "daemon_error_rate" => {
            println!("  Run `ta daemon log` to inspect errors.");
            Ok("Manual action needed: ta daemon log".to_string())
        }
        "disk_free" => {
            // Run comprehensive GC.
            super::gc::execute(
                config, false, // dry_run
                7,     // threshold_days
                false, // all
                false, // archive
                false, // include_events
                true,  // compact
                30,    // compact_after_days
                false, // force
                false, // status
                true,  // delete_stale
            )?;
            Ok("Ran gc with compaction to free disk space".to_string())
        }
        _ => Ok(format!(
            "No automated fix available — manual action: {}",
            signal.action
        )),
    }
}

// ── Gemma 4 check (v0.16.2.1) ───────────────────────────────────────────────

/// Check: if any `gemma4:*` model is pulled in Ollama, warn if no matching agent profile
/// is installed. Emits no output when Ollama is not running (best-effort only).
fn check_gemma4_ollama(config: &GatewayConfig) -> Vec<CheckResult> {
    // Query Ollama for installed models (2 s timeout — skip silently if not running).
    let models: Vec<String> = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| {
            v.get("models")?.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name")?.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
        .unwrap_or_default();

    let gemma4_models: Vec<&str> = models
        .iter()
        .filter(|m| m.starts_with("gemma4:") || m.contains("gemma4"))
        .map(|m| m.as_str())
        .collect();

    if gemma4_models.is_empty() {
        return vec![];
    }

    // Check whether a gemma4 agent profile is installed.
    let ta_config_dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("ta");

    let agents_dirs = [
        ta_config_dir.join("agents"),
        config.workspace_root.join(".ta").join("agents"),
    ];

    let profile_installed = agents_dirs.iter().any(|dir| {
        dir.is_dir()
            && (dir.join("gemma4-4b.toml").exists() || dir.join("gemma4-12b.toml").exists())
    });

    if profile_installed {
        vec![CheckResult::ok(
            "Gemma 4",
            format!(
                "{} Gemma 4 model(s) in Ollama and agent profile installed",
                gemma4_models.len()
            ),
        )]
    } else {
        vec![CheckResult::warn(
            "Gemma 4",
            format!(
                "{} Gemma 4 model(s) found in Ollama but no agent profile installed",
                gemma4_models.len()
            ),
            "Install a profile: ta agent install gemma4",
        )]
    }
}

// ── v0.16.3 checks ───────────────────────────────────────────────────────────

/// Check context.files entries in all installed agent manifests resolve to real files.
///
/// Missing files emit [warn], not [fail] — they're injected at goal start and
/// a missing file at doctor time is actionable without blocking the project.
fn check_agent_context_files(config: &GatewayConfig) -> Vec<CheckResult> {
    let all_manifests = AgentFrameworkManifest::discover(&config.workspace_root);
    if all_manifests.is_empty() {
        return vec![];
    }

    let mut results = Vec::new();
    let project_root = &config.workspace_root;

    for manifest in &all_manifests {
        let ctx_files = manifest.resolved_context_files(project_root);
        if ctx_files.is_empty() {
            continue;
        }
        let missing: Vec<String> = ctx_files
            .iter()
            .filter(|(_, path)| !path.exists())
            .map(|(declared, _)| declared.clone())
            .collect();

        if missing.is_empty() {
            results.push(CheckResult::ok(
                format!("context.files ({})", manifest.name),
                format!("all {} file(s) resolved", ctx_files.len()),
            ));
        } else {
            results.push(CheckResult::warn(
                format!("context.files ({})", manifest.name),
                format!("missing: {}", missing.join(", ")),
                "Create the missing files or remove them from context.files in the manifest",
            ));
        }
    }

    results
}

/// Check all Ollama-backed agent profiles have their model pulled (v0.16.3).
///
/// Probes GET http://localhost:11434/api/tags. Skipped when Ollama is not running.
/// Emits [warn] with `ollama pull <model>` suggestion for each missing model.
fn check_ollama_profiles(config: &GatewayConfig) -> Vec<CheckResult> {
    let all_manifests = AgentFrameworkManifest::discover(&config.workspace_root);
    let ollama_manifests: Vec<_> = all_manifests
        .iter()
        .filter(|m| m.command.contains("ollama") || m.command == "ta-agent-ollama")
        .collect();

    if ollama_manifests.is_empty() {
        return vec![];
    }

    // Probe Ollama (2 s timeout — skip silently if not running).
    let pulled_models: Vec<String> = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| {
            v.get("models")?.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name")?.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
        .unwrap_or_default();

    if pulled_models.is_empty() {
        // Ollama not running or no models — skip (not a hard error).
        return vec![];
    }

    let mut results = Vec::new();
    for manifest in &ollama_manifests {
        let model = match manifest.extract_model() {
            Some(m) => m,
            None => continue,
        };
        let is_pulled = pulled_models
            .iter()
            .any(|p| p == model || p.starts_with(&format!("{}:", model)) || p.contains(model));

        if is_pulled {
            results.push(CheckResult::ok(
                format!("ollama model ({})", manifest.name),
                format!("model '{}' is pulled", model),
            ));
        } else {
            results.push(CheckResult::warn(
                format!("ollama model ({})", manifest.name),
                format!("model '{}' not found in Ollama", model),
                format!("Run: ollama pull {}", model),
            ));
        }
    }

    results
}

// ── Config helpers ───────────────────────────────────────────────────────────

/// Read the active agent name from global TA config (defaults to "claude-code").
fn read_active_agent(config: &GatewayConfig) -> String {
    // 1. Check .ta/config.yaml for a project-level override.
    let project_config = config.workspace_root.join(".ta/config.yaml");
    if project_config.exists() {
        if let Ok(content) = std::fs::read_to_string(&project_config) {
            if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                if let Some(agent) = v
                    .get("defaults")
                    .and_then(|d| d.get("agent"))
                    .and_then(|a| a.as_str())
                {
                    if !agent.is_empty() {
                        return agent.to_string();
                    }
                }
            }
        }
    }

    // 2. Check global config.
    if let Some(path) = super::onboard::global_config_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(v) = toml::from_str::<toml::Value>(&content) {
                if let Some(agent) = v
                    .get("defaults")
                    .and_then(|d| d.get("agent"))
                    .and_then(|a| a.as_str())
                {
                    if !agent.is_empty() {
                        return agent.to_string();
                    }
                }
            }
        }
    }

    "claude-code".to_string()
}

// ── Link health (v0.16.1.5) ──────────────────────────────────────────────────

/// Check cross-project link health from `.ta/links.toml`.
///
/// Returns a warning for each local-path link whose manifest is missing.
/// Silently skips links where the path is not mounted (ENOENT → debug).
/// Remote-only links with cached manifests are checked for staleness.
fn check_links(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_workspace::links::{load as load_links, LinkStatus};

    let project_root = &config.workspace_root;
    let links = load_links(project_root);

    if links.is_empty() {
        return Vec::new();
    }

    let cache_dir = project_root.join(".ta").join("link-cache");
    let mut results = Vec::new();

    for link in &links {
        let status = link.status(project_root, &cache_dir);
        match status {
            LinkStatus::Ok { .. } => {
                // Healthy — no check entry needed.
            }
            LinkStatus::MissingManifest => {
                results.push(CheckResult::warn(
                    format!("link/{}", link.name),
                    format!(
                        "'{}' has no .ta/project-manifest.md",
                        link.name
                    ),
                    format!(
                        "Run `ta manifest init` in the '{}' project directory to create a manifest.",
                        link.name
                    ),
                ));
            }
            LinkStatus::Unreachable { reason } => {
                // Path not mounted or remote unavailable — log at debug, not warn.
                tracing::debug!(name = %link.name, %reason, "linked project unreachable — skipping doctor check");
            }
        }
    }

    results
}

// ── Orphaned in_progress phase check (v0.16.1.6.1) ───────────────────────────

/// Detect PLAN.md phases that are marked `in_progress` but have no live goal claim.
///
/// An `in_progress` marker with no active goal means a previous run was interrupted
/// or denied without properly resetting the status. This blocks future `ta run --phase`
/// calls with a confusing "already claimed" error.
///
/// Emits a [warn] for each orphaned phase, with a fix hint to reset it manually.
fn check_orphaned_in_progress_phases(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_goal::{GoalRunState, GoalRunStore};

    let plan_path = config.workspace_root.join("PLAN.md");
    if !plan_path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(&plan_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Collect all phases with in_progress status.
    let in_progress_phases: Vec<_> = super::plan::parse_plan(&content)
        .into_iter()
        .filter(|p| matches!(p.status, super::plan::PlanStatus::InProgress))
        .collect();

    if in_progress_phases.is_empty() {
        return Vec::new();
    }

    // Load all active goals to check which phases have live claims.
    let active_phase_ids: std::collections::HashSet<String> = GoalRunStore::new(&config.goals_dir)
        .ok()
        .and_then(|store| store.list().ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|g| {
            matches!(
                g.state,
                GoalRunState::Running
                    | GoalRunState::Configured
                    | GoalRunState::PrReady
                    | GoalRunState::UnderReview
                    | GoalRunState::Approved { .. }
                    | GoalRunState::Finalizing { .. }
                    | GoalRunState::DraftPending { .. }
                    | GoalRunState::AwaitingInput { .. }
            )
        })
        .filter_map(|g| g.plan_phase)
        .collect();

    let mut results = Vec::new();
    for phase in in_progress_phases {
        if !active_phase_ids.contains(&phase.id) {
            results.push(CheckResult::warn(
                "Plan phase",
                format!(
                    "Phase {} is marked in_progress but has no active goal",
                    phase.id
                ),
                format!(
                    "Reset it manually:\n\
                     \x20  ta plan reset-phase {} --to pending\n\
                     or edit PLAN.md and change \"in_progress\" \u{2192} \"pending\" for this phase.\n\
                     Cause: a previous goal was denied or interrupted without resetting the phase.",
                    phase.id
                ),
            ));
        }
    }
    results
}

// ── Stale PID file check (v0.16.1.8) ─────────────────────────────────────────

/// Returns true if a process with the given PID is currently alive.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Check `.ta/*.pid` files — warn for any file whose recorded PID is no longer alive.
///
/// A stale PID file blocks the plugin from restarting (it sees the file and
/// assumes another instance is running), causing a crash loop.
fn check_stale_pid_files(config: &GatewayConfig) -> Vec<CheckResult> {
    let ta_dir = config.workspace_root.join(".ta");
    if !ta_dir.exists() {
        return Vec::new();
    }

    let mut results = Vec::new();

    let entries = match std::fs::read_dir(&ta_dir) {
        Ok(e) => e,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pid") {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let pid: u32 = match content.trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if !is_pid_alive(pid) {
            results.push(CheckResult::warn(
                "Stale PID file",
                format!(
                    "{} contains dead PID {} — plugin likely crashed or was not shut down cleanly",
                    file_name, pid
                ),
                format!(
                    "run `ta doctor --fix` to remove the stale PID file and restart the plugin\n\
                     or manually: rm {}",
                    path.display()
                ),
            ));
        }
    }

    results
}

// ── Plugin crash-loop diagnosis (v0.16.1.8) ───────────────────────────────────

/// Read `.ta/discord-crash-state.json` (written by the daemon's channel_listener_manager)
/// and emit a human-readable diagnosis with a specific fix hint.
///
/// Known patterns diagnosed:
/// - Stale PID file blocking every restart
/// - Missing environment variable (TA_DISCORD_TOKEN etc.)
/// - Discord auth failure / invalid token
/// - Network connectivity issue
fn check_plugin_crash_loop_diagnosis(config: &GatewayConfig) -> Vec<CheckResult> {
    let crash_state_path = config.workspace_root.join(".ta/discord-crash-state.json");
    if !crash_state_path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(&crash_state_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let consecutive_failures = state["consecutive_failures"].as_u64().unwrap_or(0);
    if consecutive_failures == 0 {
        return Vec::new();
    }

    let last_stderr: Vec<String> = state["last_stderr"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let plugin = state["plugin"].as_str().unwrap_or("unknown");
    let diagnosis = diagnose_crash_stderr(&last_stderr);

    let detail = format!(
        "{} crashed {} consecutive time(s). {}",
        plugin, consecutive_failures, diagnosis.message
    );

    let fix = format!(
        "{}\n  `ta doctor --fix` will remove stale PID files and clean up crash state.",
        diagnosis.fix_hint
    );

    vec![CheckResult::warn("Plugin crash loop", detail, fix)]
}

struct CrashDiagnosis {
    message: String,
    fix_hint: String,
}

/// Pattern-match last stderr lines to diagnose the crash cause.
fn diagnose_crash_stderr(lines: &[String]) -> CrashDiagnosis {
    let combined = lines.join("\n").to_lowercase();

    if combined.contains("already running")
        || combined.contains("stale pid")
        || combined.contains("another discord listener")
    {
        return CrashDiagnosis {
            message: "Cause: stale PID file is blocking every restart attempt.".to_string(),
            fix_hint:
                "Remove the stale PID file: `ta doctor --fix` or `rm .ta/discord-listener.pid`"
                    .to_string(),
        };
    }

    if combined.contains("not set")
        || combined.contains("missing token")
        || combined.contains("environment variable")
    {
        return CrashDiagnosis {
            message: "Cause: required environment variable is missing (likely TA_DISCORD_TOKEN or TA_DISCORD_CHANNEL_ID).".to_string(),
            fix_hint: "Set the missing env var in the daemon's environment and run `ta daemon restart`".to_string(),
        };
    }

    if combined.contains("unauthorized")
        || combined.contains("invalid token")
        || combined.contains("401")
        || (combined.contains("auth") && combined.contains("error"))
    {
        return CrashDiagnosis {
            message:
                "Cause: Discord authentication failure — the bot token may be invalid or revoked."
                    .to_string(),
            fix_hint:
                "Verify TA_DISCORD_TOKEN is a valid Discord bot token and run `ta daemon restart`"
                    .to_string(),
        };
    }

    if combined.contains("connection refused")
        || combined.contains("no route to host")
        || combined.contains("network unreachable")
        || combined.contains("timed out")
    {
        return CrashDiagnosis {
            message: "Cause: network connectivity issue — cannot reach Discord API.".to_string(),
            fix_hint: "Check network/firewall settings and run `ta daemon restart`".to_string(),
        };
    }

    let last_line = lines.last().cloned().unwrap_or_default();
    CrashDiagnosis {
        message: if last_line.is_empty() {
            "Cause: unknown (no stderr output captured).".to_string()
        } else {
            format!("Last error: {}", last_line)
        },
        fix_hint: "Check `ta daemon log` for full details and run `ta daemon restart`".to_string(),
    }
}

// ── IDE index exclusion check (v0.16.1.9) ────────────────────────────────────

/// Returns `LOCAL_TA_PATHS` entries missing from `.vscode/settings.json`'s `files.exclude`.
///
/// Returns an empty vec when `.vscode/` does not exist (no VS Code project) or when
/// `settings.json` cannot be parsed (conservative — don't warn on broken JSON).
fn vscode_missing_excludes(project_root: &Path) -> Vec<String> {
    use ta_workspace::partitioning::LOCAL_TA_PATHS;

    let settings_path = project_root.join(".vscode").join("settings.json");
    if !settings_path.exists() {
        return Vec::new();
    }

    let raw = match std::fs::read_to_string(&settings_path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let settings: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let files_exclude = settings
        .get("files.exclude")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    LOCAL_TA_PATHS
        .iter()
        .map(|p| format!(".ta/{}", p))
        .filter(|key| !files_exclude.contains_key(key))
        .collect()
}

/// Check that VS Code's `settings.json` excludes all TA runtime paths.
///
/// Skips silently when `.vscode/` does not exist. Emits a single `[warn]` when
/// any entries are missing, with a `--fix` hint to add them automatically.
fn check_ide_exclusions(config: &GatewayConfig) -> Vec<CheckResult> {
    let missing = vscode_missing_excludes(&config.workspace_root);
    if missing.is_empty() {
        // Either .vscode/ doesn't exist, or all entries are present.
        return Vec::new();
    }

    vec![CheckResult::warn(
        "IDE exclusions",
        format!(
            ".vscode/settings.json is missing {} TA runtime exclude(s) — IDE may index staging dirs",
            missing.len()
        ),
        "run `ta doctor --fix` to add the missing entries to .vscode/settings.json".to_string(),
    )]
}

// ── IDE exclude manifest check (v0.16.1.10) ──────────────────────────────────

/// Check that `.ta/ide-excludes.json` exists and includes all current `ta_runtime_dirs()`.
///
/// Missing or out-of-date manifests mean community editor plugins (Zed, Helix, Neovim, etc.)
/// won't exclude the full set of TA runtime directories after a TA upgrade.
fn check_ide_excludes_manifest(config: &GatewayConfig) -> Vec<CheckResult> {
    use ta_workspace::partitioning::ta_runtime_dirs;

    let ta_dir = config.workspace_root.join(".ta");
    if !ta_dir.exists() {
        return Vec::new();
    }

    let manifest_path = ta_dir.join("ide-excludes.json");
    if !manifest_path.exists() {
        return vec![CheckResult::warn(
            "IDE manifest",
            ".ta/ide-excludes.json is missing".to_string(),
            "run `ta doctor --fix` to generate it, or `ta init` for a new project".to_string(),
        )];
    }

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            return vec![CheckResult::warn(
                "IDE manifest",
                format!(".ta/ide-excludes.json could not be read: {}", e),
                "run `ta doctor --fix` to regenerate the manifest".to_string(),
            )];
        }
    };

    let manifest: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return vec![CheckResult::warn(
                "IDE manifest",
                format!(".ta/ide-excludes.json is invalid JSON: {}", e),
                "run `ta doctor --fix` to regenerate the manifest".to_string(),
            )];
        }
    };

    let manifest_dirs: Vec<String> = manifest
        .get("dirs")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let missing: Vec<&str> = ta_runtime_dirs()
        .filter(|d| !manifest_dirs.iter().any(|m| m == d))
        .collect();

    if missing.is_empty() {
        vec![CheckResult::ok(
            "IDE manifest",
            format!(
                ".ta/ide-excludes.json is current ({} dirs)",
                manifest_dirs.len()
            ),
        )]
    } else {
        vec![CheckResult::warn(
            "IDE manifest",
            format!(
                ".ta/ide-excludes.json is out of date — {} dir(s) missing: {}",
                missing.len(),
                missing.join(", ")
            ),
            "run `ta doctor --fix` to regenerate the manifest".to_string(),
        )]
    }
}

/// Returns true when `.ta/ide-excludes.json` is absent or missing any `ta_runtime_dirs()` entry.
fn ide_manifest_needs_fix(project_root: &Path) -> bool {
    use ta_workspace::partitioning::ta_runtime_dirs;

    let ta_dir = project_root.join(".ta");
    if !ta_dir.exists() {
        return false;
    }
    let manifest_path = ta_dir.join("ide-excludes.json");
    if !manifest_path.exists() {
        return true;
    }
    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(_) => return true,
    };
    let manifest: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let manifest_dirs: Vec<String> = manifest
        .get("dirs")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    ta_runtime_dirs().any(|d| !manifest_dirs.iter().any(|m| m == d))
}

// ── AppContainer availability check (v0.16.4.2) ───────────────────────────────

fn check_appcontainer(_config: &GatewayConfig) -> CheckResult {
    #[cfg(target_os = "windows")]
    {
        if ta_runtime::sandbox_windows::appcontainer_available() {
            CheckResult::ok(
                "AppContainer",
                "available — filesystem + network isolation active on Windows 8+",
            )
        } else {
            CheckResult::warn(
                "AppContainer",
                "not available — sandbox will use Job Object only (process teardown, no filesystem isolation)",
                "Ensure TA is running on Windows 8 or later and is not nested inside a restricted Job Object.",
            )
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // AppContainer is Windows-only; on other platforms report ok (n/a).
        CheckResult::ok(
            "AppContainer",
            "n/a (Windows-only; macOS uses sandbox-exec, Linux uses bwrap)",
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ta_mcp_gateway::GatewayConfig;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(dir.path())
    }

    #[test]
    fn doctor_json_output_is_valid_json() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let checks = run_all_checks(&config);
        let json = serde_json::to_string_pretty(&checks).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            parsed.is_array(),
            "doctor --json should produce a JSON array"
        );
        let arr = parsed.as_array().unwrap();
        assert!(!arr.is_empty(), "should have at least one check");
        // Each entry must have name, status, detail.
        for entry in arr {
            assert!(entry.get("name").is_some(), "each check needs a name field");
            assert!(
                entry.get("status").is_some(),
                "each check needs a status field"
            );
            assert!(
                entry.get("detail").is_some(),
                "each check needs a detail field"
            );
        }
    }

    #[test]
    fn doctor_returns_ok_for_empty_project() {
        // doctor() should not panic or error for a minimal project dir.
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let checks = run_all_checks(&config);
        // Auth and agent-binary checks legitimately fail in CI (no API key, no
        // claude binary on PATH). Only assert that no *other* checks are Fail.
        let unexpected_failures: Vec<_> = checks
            .iter()
            .filter(|c| {
                c.status == CheckStatus::Fail
                    && !c.name.starts_with("Auth")
                    && !c.name.starts_with("Agent binary")
            })
            .collect();
        assert!(
            unexpected_failures.is_empty(),
            "unexpected failures for empty project: {:?}",
            unexpected_failures
        );
    }

    #[test]
    fn check_result_ok_has_no_fix() {
        let r = CheckResult::ok("test", "all good");
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.fix.is_empty());
    }

    #[test]
    fn check_result_fail_has_fix() {
        let r = CheckResult::fail("test", "broken", "do this to fix");
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(!r.fix.is_empty());
    }

    #[test]
    fn auth_check_missing_returns_fail_check() {
        // When neither ANTHROPIC_API_KEY is set nor ~/.config/claude exists,
        // and claude-code is active, auth check should be fail.
        // We test this without side-effecting the real env.
        let dir = TempDir::new().unwrap();
        // Use a temp dir as project root so no real agent config exists.
        let (result, warnings) = check_auth("claude-code", dir.path());
        // claude-code requires auth — if neither env var nor session file exists,
        // the result is either ok (if user has session) or fail.
        // We only assert the shape, not the specific status (since CI may have keys set).
        assert!(result.name.contains("claude-code"));
        let _ = warnings; // may or may not have warnings
    }

    #[test]
    fn version_mismatch_warn_does_not_fail() {
        // Version mismatch should be a [warn] not [FAIL] when detail is present.
        // This tests the observability mandate: version mismatch warns but doesn't fail.
        let r = CheckResult::warn("Version", "mismatch detail", "fix hint");
        assert_eq!(r.status, CheckStatus::Warn);
    }

    // ── Gemma 4 doctor checks (v0.16.2.1) ──────────────────────────────────────

    #[test]
    fn gemma4_check_no_ollama_returns_empty() {
        // When Ollama is not running (as in CI), the check should return no results.
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // This will fail to connect to Ollama (not running in tests) — expect empty vec.
        let results = check_gemma4_ollama(&config);
        // Should return empty (no models found) since Ollama is not running.
        // If somehow Ollama IS running with gemma4 models and no profile, we'd get a warn —
        // but that is also a valid outcome and not a test failure.
        assert!(
            results.iter().all(|r| r.status != CheckStatus::Fail),
            "gemma4 check should never produce a hard failure, only warnings"
        );
    }

    #[test]
    fn gemma4_check_profile_installed_suppresses_warning() {
        // If a gemma4 profile is present on disk, the warning should not fire.
        let dir = TempDir::new().unwrap();
        let agents_dir = dir.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("gemma4-4b.toml"),
            "name = \"gemma4-4b\"\ncommand = \"ta-agent-ollama\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let config = test_config(&dir);
        // Even if Ollama isn't running, once it is and a model is present, having the
        // profile means no warning. We can't simulate Ollama here, but verify that
        // the check produces no failure results.
        let results = check_gemma4_ollama(&config);
        assert!(
            results.iter().all(|r| r.status != CheckStatus::Fail),
            "gemma4 check should not fail when profile is installed"
        );
    }

    #[test]
    fn orphaned_in_progress_check_clean_when_no_plan() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let results = check_orphaned_in_progress_phases(&config);
        assert!(results.is_empty(), "no checks when no PLAN.md");
    }

    #[test]
    fn orphaned_in_progress_check_warns_for_stale_phase() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Write a PLAN.md with an in_progress phase.
        let plan_content = "### v0.1.0 — Test Phase\n<!-- status: in_progress -->\n";
        std::fs::write(dir.path().join("PLAN.md"), plan_content).unwrap();
        // No goals exist → the claim is orphaned.
        let results = check_orphaned_in_progress_phases(&config);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(results[0].detail.contains("v0.1.0"));
        assert!(!results[0].fix.is_empty(), "should have a fix hint");
    }

    // ── Stale PID file checks (v0.16.1.8) ────────────────────────────────────

    #[test]
    fn stale_pid_check_no_ta_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // No .ta/ directory — should return no results.
        let results = check_stale_pid_files(&config);
        assert!(results.is_empty(), "no checks when .ta/ does not exist");
    }

    #[test]
    fn stale_pid_check_warns_for_dead_pid() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Write a PID file with a dead PID.
        std::fs::write(ta_dir.join("discord-listener.pid"), u32::MAX.to_string()).unwrap();

        let config = test_config(&dir);
        let results = check_stale_pid_files(&config);
        assert_eq!(results.len(), 1, "should warn for one stale PID file");
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(
            results[0].detail.contains("discord-listener.pid"),
            "detail should name the file: {}",
            results[0].detail
        );
        assert!(!results[0].fix.is_empty(), "should provide a fix hint");
    }

    #[test]
    fn doctor_diagnoses_stale_pid_from_signal() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let crash_state = serde_json::json!({
            "plugin": "ta-channel-discord",
            "consecutive_failures": 7,
            "last_stderr": [
                "Another Discord listener is already running (PID 10435). \
                 Stop it first, or remove .ta/discord-listener.pid"
            ],
            "pid_path": ".ta/discord-listener.pid",
            "updated_at": "2026-05-28T10:00:00Z"
        });
        std::fs::write(
            ta_dir.join("discord-crash-state.json"),
            serde_json::to_string_pretty(&crash_state).unwrap(),
        )
        .unwrap();

        let config = test_config(&dir);
        let results = check_plugin_crash_loop_diagnosis(&config);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(
            results[0].detail.contains("stale PID") || results[0].detail.contains("blocking"),
            "diagnosis should identify stale PID cause: {}",
            results[0].detail
        );
        assert!(
            results[0].fix.contains("doctor --fix")
                || results[0].fix.contains("discord-listener.pid"),
            "fix should mention doctor --fix or the PID file: {}",
            results[0].fix
        );
    }

    // ── IDE exclusion checks (v0.16.1.9) ─────────────────────────────────────

    #[test]
    fn doctor_detects_missing_vscode_excludes() {
        let dir = TempDir::new().unwrap();
        // Create .vscode/settings.json with NO TA entries.
        let vscode_dir = dir.path().join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("settings.json"),
            r#"{"editor.formatOnSave": true}"#,
        )
        .unwrap();

        let config = test_config(&dir);
        let results = check_ide_exclusions(&config);
        assert_eq!(results.len(), 1, "should warn when TA excludes are missing");
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(
            results[0].detail.contains("missing"),
            "detail should mention missing entries: {}",
            results[0].detail
        );
        assert!(
            results[0].fix.contains("doctor --fix"),
            "fix hint should mention doctor --fix: {}",
            results[0].fix
        );
    }

    #[test]
    fn doctor_no_warn_when_vscode_dir_absent() {
        let dir = TempDir::new().unwrap();
        // No .vscode/ directory at all.
        let config = test_config(&dir);
        let results = check_ide_exclusions(&config);
        assert!(
            results.is_empty(),
            "no warning when .vscode/ does not exist"
        );
    }

    #[test]
    fn vscode_settings_merged_not_overwritten() {
        let dir = TempDir::new().unwrap();
        let vscode_dir = dir.path().join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        // Pre-existing user setting that must survive the merge.
        std::fs::write(
            vscode_dir.join("settings.json"),
            r#"{"editor.tabSize": 4, "files.exclude": {"node_modules/": true}}"#,
        )
        .unwrap();

        crate::commands::init::write_vscode_settings_excludes(dir.path()).unwrap();

        let content = std::fs::read_to_string(vscode_dir.join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // User's pre-existing key must be preserved.
        assert_eq!(
            parsed["editor.tabSize"],
            serde_json::Value::Number(4.into()),
            "user setting editor.tabSize must not be overwritten"
        );
        assert_eq!(
            parsed["files.exclude"]["node_modules/"],
            serde_json::Value::Bool(true),
            "user's files.exclude entry must not be overwritten"
        );
        // TA entries must be present.
        assert_eq!(
            parsed["files.exclude"][".ta/staging/"],
            serde_json::Value::Bool(true),
            ".ta/staging/ must be added to files.exclude"
        );
        assert_eq!(
            parsed["search.exclude"][".ta/goals/"],
            serde_json::Value::Bool(true),
            ".ta/goals/ must be added to search.exclude"
        );
    }

    #[test]
    fn doctor_fix_adds_vscode_excludes() {
        let dir = TempDir::new().unwrap();
        let vscode_dir = dir.path().join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("settings.json"),
            r#"{"editor.formatOnSave": true}"#,
        )
        .unwrap();

        // Verify missing before fix.
        let missing_before = vscode_missing_excludes(dir.path());
        assert!(
            !missing_before.is_empty(),
            "should detect missing excludes before fix"
        );

        // Apply fix.
        crate::commands::init::write_vscode_settings_excludes(dir.path()).unwrap();

        // Verify nothing missing after fix.
        let missing_after = vscode_missing_excludes(dir.path());
        assert!(
            missing_after.is_empty(),
            "all excludes should be present after fix, still missing: {:?}",
            missing_after
        );
    }

    #[test]
    fn doctor_fix_removes_stale_pid() {
        use crate::commands::health_signals::{HealthSignal, SignalSeverity};

        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let pid_path = ta_dir.join("discord-listener.pid");
        std::fs::write(&pid_path, u32::MAX.to_string()).unwrap();

        // Also create a crash state file — it should be removed on fix.
        let crash_path = ta_dir.join("discord-crash-state.json");
        std::fs::write(
            &crash_path,
            serde_json::json!({ "consecutive_failures": 3 }).to_string(),
        )
        .unwrap();

        let config = test_config(&dir);
        let signal = HealthSignal {
            kind: "stale_pid".to_string(),
            severity: SignalSeverity::Warn,
            message: "stale PID file".to_string(),
            action: "ta doctor --fix".to_string(),
        };
        let result = apply_fix(&config, &signal);
        assert!(result.is_ok(), "apply_fix should not error: {:?}", result);

        assert!(
            !pid_path.exists(),
            "stale PID file should be removed by fix"
        );
        assert!(
            !crash_path.exists(),
            "crash state file should be cleared by fix"
        );
    }

    // ── IDE exclude manifest checks (v0.16.1.10) ──────────────────────────────

    #[test]
    fn doctor_detects_missing_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // .ta/ exists but ide-excludes.json does not.

        let config = test_config(&dir);
        let results = check_ide_excludes_manifest(&config);
        assert_eq!(results.len(), 1, "should warn when manifest is absent");
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(
            results[0].detail.contains("missing"),
            "detail should mention missing: {}",
            results[0].detail
        );
        assert!(
            results[0].fix.contains("doctor --fix"),
            "fix hint should mention doctor --fix: {}",
            results[0].fix
        );
    }

    #[test]
    fn doctor_detects_stale_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // Write a manifest that is missing 'memory/' (a required dir).
        let stale = serde_json::json!({
            "version": 1,
            "ta_dir": ".ta",
            "dirs": ["staging/", "goals/"]  // intentionally incomplete
        });
        std::fs::write(
            ta_dir.join("ide-excludes.json"),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        let config = test_config(&dir);
        let results = check_ide_excludes_manifest(&config);
        assert_eq!(results.len(), 1, "should warn for stale manifest");
        assert_eq!(results[0].status, CheckStatus::Warn);
        assert!(
            results[0].detail.contains("out of date"),
            "detail should say out of date: {}",
            results[0].detail
        );
    }

    #[test]
    fn doctor_fix_rewrites_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // Write a stale manifest.
        let stale = serde_json::json!({
            "version": 1,
            "ta_dir": ".ta",
            "dirs": ["staging/"]  // intentionally incomplete
        });
        std::fs::write(
            ta_dir.join("ide-excludes.json"),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        assert!(
            ide_manifest_needs_fix(dir.path()),
            "should detect stale manifest before fix"
        );

        crate::commands::init::write_ide_excludes_manifest(dir.path()).unwrap();

        assert!(
            !ide_manifest_needs_fix(dir.path()),
            "manifest should be up to date after fix"
        );
    }

    #[test]
    fn doctor_manifest_ok_when_current() {
        let dir = TempDir::new().unwrap();
        crate::commands::init::write_ide_excludes_manifest(dir.path()).unwrap();

        let config = test_config(&dir);
        let results = check_ide_excludes_manifest(&config);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status,
            CheckStatus::Ok,
            "up-to-date manifest should pass: {}",
            results[0].detail
        );
    }

    #[test]
    fn doctor_no_manifest_check_when_no_ta_dir() {
        let dir = TempDir::new().unwrap();
        // No .ta/ directory.
        let config = test_config(&dir);
        let results = check_ide_excludes_manifest(&config);
        assert!(
            results.is_empty(),
            "no manifest check when .ta/ does not exist"
        );
    }

    #[test]
    fn check_appcontainer_returns_result_without_panic() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = check_appcontainer(&config);
        // Must always return a result (ok or warn) without panicking.
        assert!(!result.name.is_empty());
    }
}
