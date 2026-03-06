// commands/status.rs — Project-wide status dashboard (v0.9.6).

use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;

pub fn execute(config: &GatewayConfig) -> anyhow::Result<()> {
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

    Ok(())
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
}
