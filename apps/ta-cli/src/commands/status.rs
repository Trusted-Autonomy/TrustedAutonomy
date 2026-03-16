// commands/status.rs — Project-wide status dashboard (v0.9.6).

use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;

pub fn execute(config: &GatewayConfig, deep: bool) -> anyhow::Result<()> {
    let project_name = config
        .workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let version = env!("CARGO_PKG_VERSION");

    println!("Project: {} (v{})", project_name, version);

    // Current plan phase.
    let plan_path = config.workspace_root.join("PLAN.md");
    if plan_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&plan_path) {
            if let Some(phase) = find_next_pending_phase(&content) {
                println!("Next phase: {}", phase);
            }
        }
    }

    println!();

    // Active goals.
    let goal_store = GoalRunStore::new(&config.goals_dir);
    match goal_store {
        Ok(store) => {
            let all_goals = store.list().unwrap_or_default();
            let active: Vec<_> = all_goals
                .iter()
                .filter(|g| {
                    matches!(
                        g.state,
                        ta_goal::GoalRunState::Running
                            | ta_goal::GoalRunState::Configured
                            | ta_goal::GoalRunState::PrReady
                    )
                })
                .collect();

            if active.is_empty() {
                println!("Active agents: none");
            } else {
                println!("Active agents:");
                for g in &active {
                    let elapsed = chrono::Utc::now()
                        .signed_duration_since(g.created_at)
                        .num_minutes();
                    println!(
                        "  {} ({}) → goal {} \"{}\" [{} {}m]",
                        g.agent_id,
                        g.agent_id,
                        &g.goal_run_id.to_string()[..8],
                        g.title,
                        g.state,
                        elapsed
                    );
                }
            }

            // Pending drafts.
            let pending_drafts = count_pending_drafts(&config.pr_packages_dir);
            println!();
            println!("Pending drafts: {}", pending_drafts);
            println!("Active goals:   {}", active.len());
            println!("Total goals:    {}", all_goals.len());
        }
        Err(_) => {
            println!("Active agents: (no goal store found)");
            println!("Pending drafts: 0");
            println!("Active goals:   0");
        }
    }

    if deep {
        println!();
        deep_status(config)?;
    }

    Ok(())
}

fn deep_status(config: &GatewayConfig) -> anyhow::Result<()> {
    // Platform info
    println!(
        "Platform: {}",
        ta_changeset::registry_client::detect_platform()
    );
    println!();

    // Plugin requirements
    if ta_changeset::project_manifest::ProjectManifest::exists(&config.workspace_root) {
        println!("Plugin requirements:");
        match ta_changeset::project_manifest::ProjectManifest::load(&config.workspace_root) {
            Ok(manifest) => {
                let issues = ta_changeset::plugin_resolver::check_requirements(
                    &manifest,
                    &config.workspace_root,
                );
                if issues.is_empty() {
                    println!("  All required plugins satisfied");
                } else {
                    for (name, issue) in &issues {
                        println!("  [!] {}: {}", name, issue);
                    }
                }
            }
            Err(e) => {
                println!("  Error loading project.toml: {}", e);
            }
        }
        println!();
    }

    // Daemon health
    println!("Daemon:");
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let status_url = format!("{}/api/status", daemon_url);
    match client.get(&status_url).send() {
        Ok(resp) if resp.status().is_success() => {
            let json: serde_json::Value = resp.json()?;
            let version = json["version"].as_str().unwrap_or("?");
            let pid = super::daemon::read_pid(&config.workspace_root);
            println!("  Status:  healthy");
            println!("  URL:     {}", daemon_url);
            println!("  Version: {}", version);
            if let Some(p) = pid {
                println!("  PID:     {}", p);
            }
        }
        _ => {
            println!("  Status: not running (start with: ta daemon start)");
        }
    }

    // Disk usage
    println!();
    println!("Disk usage:");
    let staging_dir = config.workspace_root.join(".ta/staging");
    if staging_dir.exists() {
        let mut total_size = 0u64;
        let mut count = 0u32;
        if let Ok(entries) = std::fs::read_dir(&staging_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    total_size += walkdir_size(&entry.path());
                    count += 1;
                }
            }
        }
        println!(
            "  Staging: {} director{} ({})",
            count,
            if count == 1 { "y" } else { "ies" },
            format_staging_size(total_size)
        );
    } else {
        println!("  Staging: none");
    }

    let pr_dir = &config.pr_packages_dir;
    if pr_dir.exists() {
        let count = std::fs::read_dir(pr_dir)
            .map(|e| {
                e.flatten()
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                    .count()
            })
            .unwrap_or(0);
        println!("  Drafts:  {} package file(s)", count);
    }

    // Pending questions
    println!();
    println!("Pending questions:");
    let interactions_dir = config.workspace_root.join(".ta/interactions");
    if interactions_dir.exists() {
        let mut pending = 0u32;
        if let Ok(entries) = std::fs::read_dir(&interactions_dir) {
            for entry in entries.flatten() {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    if content.contains("\"pending\"") {
                        pending += 1;
                    }
                }
            }
        }
        if pending == 0 {
            println!("  none");
        } else {
            println!("  {} pending (answer via ta shell or channel)", pending);
        }
    } else {
        println!("  none");
    }

    // Recent events
    println!();
    println!("Recent events:");
    let events_file = config.workspace_root.join(".ta/events/events.jsonl");
    if events_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&events_file) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(5);
            if lines.is_empty() {
                println!("  (no events)");
            } else {
                for line in &lines[start..] {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        let event_type = v["event_type"].as_str().unwrap_or("?");
                        let ts = v["timestamp"].as_str().unwrap_or("?");
                        let time = if ts.len() > 11 { &ts[11..19] } else { ts };
                        let goal = v["goal_id"]
                            .as_str()
                            .map(|id| &id[..8.min(id.len())])
                            .unwrap_or("-");
                        println!("  [{}] {} (goal: {})", time, event_type, goal);
                    }
                }
            }
        }
    } else {
        println!("  (no events)");
    }

    Ok(())
}

fn walkdir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += walkdir_size(&entry.path());
                }
            }
        }
    }
    total
}

fn format_staging_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn find_next_pending_phase(plan_content: &str) -> Option<String> {
    // Look for the first phase with `<!-- status: pending -->`.
    for line in plan_content.lines() {
        if line.contains("<!-- status: pending -->") {
            // The phase title is typically on the line above or this line.
            // Common format: "### vX.Y.Z — Title\n<!-- status: pending -->"
            // But the marker is often on the same line or the line after the heading.
            // Try to extract from this line or find the preceding heading.
            continue;
        }
        // Check if this is a heading followed by pending status.
        if line.starts_with("### ") {
            // Peek: the plan format has the status marker on the next line.
            // We'll use a different approach below.
        }
    }

    // Two-line scan: heading + status marker.
    let lines: Vec<&str> = plan_content.lines().collect();
    for i in 0..lines.len().saturating_sub(1) {
        if lines[i].starts_with("### ") && lines[i + 1].contains("<!-- status: pending -->") {
            let title = lines[i].trim_start_matches('#').trim();
            return Some(title.to_string());
        }
    }

    None
}

fn count_pending_drafts(pr_packages_dir: &std::path::Path) -> usize {
    if !pr_packages_dir.exists() {
        return 0;
    }
    std::fs::read_dir(pr_packages_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .filter(|e| {
                    // Quick check: read file and see if status is PendingReview.
                    std::fs::read_to_string(e.path())
                        .map(|content| content.contains("PendingReview"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_next_pending_phase_works() {
        let plan = r#"
### v0.9.5 — Enhanced Draft View Output
<!-- status: done -->

### v0.9.6 — Orchestrator API & Goal-Scoped Agent Tracking
<!-- status: pending -->

### v0.9.7 — Daemon API Expansion
<!-- status: pending -->
"#;
        let result = find_next_pending_phase(plan);
        assert_eq!(
            result,
            Some("v0.9.6 — Orchestrator API & Goal-Scoped Agent Tracking".to_string())
        );
    }

    #[test]
    fn find_next_pending_phase_none_when_all_done() {
        let plan = r#"
### v0.9.5 — Done
<!-- status: done -->
"#;
        assert_eq!(find_next_pending_phase(plan), None);
    }

    #[test]
    fn count_pending_drafts_missing_dir() {
        let count = count_pending_drafts(std::path::Path::new("/nonexistent/path"));
        assert_eq!(count, 0);
    }

    #[test]
    fn deep_status_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(dir.path());
        // Should not panic even with empty project.
        let _ = deep_status(&config);
    }
}
