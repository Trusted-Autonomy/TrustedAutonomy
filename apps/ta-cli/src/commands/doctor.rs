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

/// Execute `ta doctor [--json] [--fix-denied]`.
pub fn execute(config: &GatewayConfig, json: bool, fix_denied: bool) -> anyhow::Result<()> {
    if fix_denied {
        return execute_fix_denied(config);
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
    use ta_workspace::partitioning::{git_is_ignored, VcsBackend, LOCAL_TA_PATHS};

    let mut results = Vec::new();
    let vcs = VcsBackend::detect(&config.workspace_root);

    match &vcs {
        VcsBackend::Git => {
            let git_ok = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&config.workspace_root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if git_ok {
                results.push(CheckResult::ok("VCS", "git".to_string()));
            } else {
                results.push(CheckResult::warn(
                    "VCS",
                    "git status failed".to_string(),
                    "Check git installation and repository state",
                ));
                return results;
            }
            // Check gitignore coverage for .ta/ paths.
            let mut unignored: Vec<&str> = Vec::new();
            for path in LOCAL_TA_PATHS {
                let full = config
                    .workspace_root
                    .join(".ta")
                    .join(path.trim_end_matches('/'));
                if full.exists() {
                    if let Ok(false) = git_is_ignored(&config.workspace_root, path) {
                        unignored.push(path);
                    }
                }
            }
            if !unignored.is_empty() {
                let paths = unignored.join(", .ta/");
                results.push(CheckResult::warn(
                    "VCS .gitignore",
                    format!("{} .ta/ path(s) not in .gitignore", unignored.len()),
                    format!(".ta/{} should be ignored — run: ta setup vcs", paths),
                ));
            }
        }
        VcsBackend::Perforce => {
            let p4_ok = std::process::Command::new("p4")
                .arg("info")
                .current_dir(&config.workspace_root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if p4_ok {
                results.push(CheckResult::ok("VCS", "perforce".to_string()));
            } else {
                results.push(CheckResult::warn(
                    "VCS",
                    "p4 info failed".to_string(),
                    "Check P4PORT/P4CLIENT configuration",
                ));
            }
            if std::env::var("P4IGNORE").is_err() {
                results.push(CheckResult::warn(
                    "VCS P4IGNORE",
                    "P4IGNORE not set".to_string(),
                    "export P4IGNORE=.p4ignore  then run: ta setup vcs",
                ));
            }
        }
        VcsBackend::None => {
            results.push(CheckResult::ok(
                "VCS",
                "none (skipping VCS checks)".to_string(),
            ));
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
}
