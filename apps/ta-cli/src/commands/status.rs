// commands/status.rs — Project-wide status dashboard (v0.13.1.6).
//
// Unified, prioritized view that replaces the need to run:
//   ta goal list, ta draft list, ta plan status, ta daemon health, ta doctor
//
// Output order: Urgent (stuck/failed goals, pending approvals, health issues)
//               → Active work → Recent completions → Suggested next actions.

use std::collections::HashSet;

use ta_goal::{GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;

pub fn execute(config: &GatewayConfig, deep: bool) -> anyhow::Result<()> {
    let project_name = config
        .workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let version = env!("CARGO_PKG_VERSION");

    println!("╭─ {} (ta v{})", project_name, version);

    // Current plan phase.
    let next_phase = {
        let plan_path = config.workspace_root.join("PLAN.md");
        if plan_path.exists() {
            std::fs::read_to_string(&plan_path)
                .ok()
                .and_then(|content| find_next_pending_phase(&content))
        } else {
            None
        }
    };

    if let Some(ref phase) = next_phase {
        println!("│  Next phase: {}", phase);
    }

    println!("│");

    // Load all goals once.
    let goal_store = GoalRunStore::new(&config.goals_dir);
    let all_goals = match &goal_store {
        Ok(store) => store.list().unwrap_or_default(),
        Err(_) => vec![],
    };

    let now = chrono::Utc::now();

    // Classify goals.
    let urgent_goals: Vec<_> = all_goals
        .iter()
        .filter(|g| {
            // Stuck: running but updated >2h ago.
            let stale = (now - g.updated_at).num_hours() >= 2;
            matches!(g.state, GoalRunState::Running | GoalRunState::Configured) && stale
        })
        .collect();

    // v0.15.7.1: Exclude reviewer goals whose parent draft is terminal from URGENT.
    // v0.15.14.0: Exclude goals whose plan_phase is now done in PLAN.md — if a later
    // run completed the phase, the old failure is expected and not user-actionable.
    let done_phases = {
        let plan_path = config.workspace_root.join("PLAN.md");
        if plan_path.exists() {
            std::fs::read_to_string(&plan_path)
                .map(|c| collect_done_phase_ids(&c))
                .unwrap_or_default()
        } else {
            std::collections::HashSet::new()
        }
    };
    let failed_goals: Vec<_> = all_goals
        .iter()
        .filter(|g| matches!(g.state, GoalRunState::Failed { .. }))
        .filter(|g| !is_terminal_reviewer_goal(g))
        .filter(|g| {
            // Suppress if the goal's plan phase is now done — a later run completed it.
            if let Some(ref phase) = g.plan_phase {
                !done_phases.contains(phase.as_str())
            } else {
                true
            }
        })
        .take(5)
        .collect();

    let active_goals: Vec<_> = all_goals
        .iter()
        .filter(|g| {
            matches!(
                g.state,
                GoalRunState::Running | GoalRunState::Configured | GoalRunState::PrReady
            )
        })
        .collect();

    let pending_drafts = count_pending_drafts(&config.pr_packages_dir);
    let pending_draft_ids = list_pending_draft_ids(&config.pr_packages_dir);

    let recent_completions: Vec<_> = all_goals
        .iter()
        .filter(|g| matches!(g.state, GoalRunState::Applied | GoalRunState::Completed))
        .filter(|g| (now - g.updated_at).num_hours() < 24)
        .take(3)
        .collect();

    // Read operations log for pending corrective actions.
    let ops_log = ta_goal::OperationsLog::for_project(&config.workspace_root);
    let pending_ops: Vec<_> = ops_log
        .read(Some(10))
        .unwrap_or_default()
        .into_iter()
        .filter(|a| matches!(a.status, ta_goal::ActionStatus::Proposed))
        .take(5)
        .collect();

    let has_urgent = !urgent_goals.is_empty()
        || !failed_goals.is_empty()
        || pending_drafts > 0
        || !pending_ops.is_empty();

    // ── URGENT ───────────────────────────────────────────────────────────
    if has_urgent {
        println!("│  ⚠ URGENT");

        for g in &urgent_goals {
            let hours = (now - g.updated_at).num_hours();
            println!(
                "│    Stuck goal: \"{}\" [{}h stale, ID: {}]",
                g.title,
                hours,
                &g.goal_run_id.to_string()[..8]
            );
            println!("│    → Run `ta goal list` to inspect or `ta gc` to clean up");
        }

        for g in &failed_goals {
            println!(
                "│    Failed goal: \"{}\" [ID: {}]",
                g.title,
                &g.goal_run_id.to_string()[..8]
            );
            println!(
                "│    → Run `ta run --follow-up {}` to retry",
                &g.goal_run_id.to_string()[..8]
            );
        }

        if pending_drafts > 0 {
            println!("│    {} draft(s) awaiting your review", pending_drafts);
            for id in &pending_draft_ids {
                println!("│    → `ta draft view {}` to review", id);
            }
        }

        // Deduplicate disk-space CRIT entries: multiple paths at low space
        // produce one consolidated message rather than one line per threshold.
        let (disk_crits, other_ops): (Vec<_>, Vec<_>) = pending_ops.iter().partition(|op| {
            op.severity == ta_goal::ActionSeverity::Critical
                && op.issue.to_lowercase().contains("disk")
        });
        if !disk_crits.is_empty() {
            if disk_crits.len() == 1 {
                println!("│    [CRIT] {}", disk_crits[0].issue);
                println!("│    → {}", disk_crits[0].proposed_action);
            } else {
                println!(
                    "│    [CRIT] Low disk space on {} paths. Run `ta gc` to reclaim space, \
                     or free disk manually.",
                    disk_crits.len()
                );
            }
        }
        for op in &other_ops {
            let sev = match op.severity {
                ta_goal::ActionSeverity::Critical => "CRIT",
                ta_goal::ActionSeverity::Warning => "WARN",
                ta_goal::ActionSeverity::Info => "INFO",
            };
            println!("│    [{}] {}", sev, op.issue);
            if op.auto_healable {
                println!("│    → Auto-healable: {}", op.proposed_action);
            } else {
                println!("│    → {}", op.proposed_action);
            }
        }

        println!("│");
    }

    // ── ACTIVE WORK ───────────────────────────────────────────────────────
    if active_goals.is_empty() {
        println!("│  Active agents: none");
    } else {
        println!("│  Active agents: {}", active_goals.len());
        for g in &active_goals {
            let elapsed = (now - g.created_at).num_minutes();
            let state_label = match &g.state {
                GoalRunState::Running => "running",
                GoalRunState::Configured => "starting",
                GoalRunState::PrReady => "draft ready",
                _ => "active",
            };
            println!(
                "│    [{}m] {} — \"{}\"  [{}]",
                elapsed,
                state_label,
                g.title,
                &g.goal_run_id.to_string()[..8]
            );
        }
    }

    // ── RECENT COMPLETIONS ────────────────────────────────────────────────
    if !recent_completions.is_empty() {
        println!("│");
        println!("│  Recent (last 24h):");
        for g in &recent_completions {
            let hours = (now - g.updated_at).num_hours();
            let label = if hours == 0 {
                format!("{}m ago", (now - g.updated_at).num_minutes())
            } else {
                format!("{}h ago", hours)
            };
            println!(
                "│    ✓ {} — \"{}\"  [{}]",
                label,
                g.title,
                &g.goal_run_id.to_string()[..8]
            );
        }
    }

    // ── SUMMARY ROW ───────────────────────────────────────────────────────
    println!("│");
    println!(
        "│  Goals: {} active  {} pending drafts  {} total",
        active_goals.len(),
        pending_drafts,
        all_goals.len()
    );

    if deep {
        println!("│");
        deep_status(config)?;
    }

    // ── SUGGESTED NEXT ACTIONS ────────────────────────────────────────────
    let suggestions = suggest_next_actions(
        &active_goals,
        pending_drafts,
        &pending_draft_ids,
        &recent_completions,
        next_phase.as_deref(),
        &pending_ops,
        deep,
    );

    if !suggestions.is_empty() {
        println!("│");
        println!("│  Suggested next:");
        for s in &suggestions {
            println!("│    {}", s);
        }
    }

    println!("╰─");

    Ok(())
}

/// Build a prioritized list of suggested next actions based on current state.
fn suggest_next_actions(
    active_goals: &[&ta_goal::GoalRun],
    pending_drafts: usize,
    pending_draft_ids: &[String],
    recent_completions: &[&ta_goal::GoalRun],
    next_phase: Option<&str>,
    pending_ops: &[ta_goal::CorrectiveAction],
    deep: bool,
) -> Vec<String> {
    let mut suggestions = Vec::new();

    if pending_drafts > 0 {
        if let Some(id) = pending_draft_ids.first() {
            suggestions.push(format!("`ta draft view {}` — review pending draft", id));
        }
    }

    // If a goal just completed (within 10 minutes), suggest review.
    for completion in recent_completions.iter().take(1) {
        let mins = chrono::Utc::now()
            .signed_duration_since(completion.updated_at)
            .num_minutes();
        if mins < 10 {
            suggestions.push(format!(
                "`ta draft list` — \"{}\" just completed",
                completion.title
            ));
        }
    }

    if active_goals.is_empty() && pending_drafts == 0 {
        if let Some(phase) = next_phase {
            // Extract just the phase ID for the run command.
            let phase_id = phase.split_whitespace().next().unwrap_or(phase);
            suggestions.push(format!("`ta run {}` — start next phase", phase_id));
        }
    }

    if !pending_ops.is_empty() {
        suggestions.push("`ta operations log` — review corrective actions".to_string());
    }

    if !deep && active_goals.is_empty() && pending_drafts == 0 {
        suggestions.push("`ta status --deep` — full health check".to_string());
    }

    suggestions
}

fn deep_status(config: &GatewayConfig) -> anyhow::Result<()> {
    // Platform info
    println!(
        "│  Platform: {}",
        ta_changeset::registry_client::detect_platform()
    );
    println!("│");

    // Plugin requirements
    if ta_changeset::project_manifest::ProjectManifest::exists(&config.workspace_root) {
        println!("│  Plugin requirements:");
        match ta_changeset::project_manifest::ProjectManifest::load(&config.workspace_root) {
            Ok(manifest) => {
                let issues = ta_changeset::plugin_resolver::check_requirements(
                    &manifest,
                    &config.workspace_root,
                );
                if issues.is_empty() {
                    println!("│    All required plugins satisfied");
                } else {
                    for (name, issue) in &issues {
                        println!("│    [!] {}: {}", name, issue);
                    }
                }
            }
            Err(e) => {
                println!("│    Error loading project.toml: {}", e);
            }
        }
        println!("│");
    }

    // Daemon health
    println!("│  Daemon:");
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
            let power_active = json["power_assertion_active"].as_bool().unwrap_or(false);
            println!("│    Status:  healthy");
            println!("│    URL:     {}", daemon_url);
            println!("│    Version: {}", version);
            if let Some(p) = pid {
                println!("│    PID:     {}", p);
            }
            if power_active {
                println!("│    Power:   sleep prevented (active goal in progress)");
            } else {
                println!("│    Power:   no assertion (no active goals)");
            }
        }
        _ => {
            println!("│    Status: not running");
            println!("│    → Start with: ta daemon start");
        }
    }

    // Disk usage
    println!("│");
    println!("│  Disk usage:");
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
            "│    Staging: {} director{} ({})",
            count,
            if count == 1 { "y" } else { "ies" },
            format_staging_size(total_size)
        );
        if count > 10 {
            println!("│    → Run `ta gc --compact` to compact old staging directories");
        }
    } else {
        println!("│    Staging: none");
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
        println!("│    Drafts:  {} package file(s)", count);
    }

    // Pending questions
    println!("│");
    println!("│  Pending questions:");
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
            println!("│    none");
        } else {
            println!("│    {} pending (answer via ta shell or channel)", pending);
        }
    } else {
        println!("│    none");
    }

    // Recent events
    println!("│");
    println!("│  Recent events:");
    let events_file = config.workspace_root.join(".ta/events/events.jsonl");
    if events_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&events_file) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(5);
            if lines.is_empty() {
                println!("│    (no events)");
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
                        println!("│    [{}] {} (goal: {})", time, event_type, goal);
                    }
                }
            }
        }
    } else {
        println!("│    (no events)");
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

/// Find the next phase the user should work on.
///
/// Uses a watermark approach: find the last `done` phase in document order,
/// then return the first `pending` phase after that position. This prevents
/// out-of-order completions from surfacing an already-completed phase as "next".
///
/// Pending phases before the watermark (skipped or deferred) are not returned
/// by this function — they are surfaced separately if needed.
fn find_next_pending_phase(plan_content: &str) -> Option<String> {
    let lines: Vec<&str> = plan_content.lines().collect();

    // Collect all phase positions with their status.
    let mut done_watermark: Option<usize> = None;
    let mut pending_phases: Vec<(usize, String)> = Vec::new();

    let mut i = 0;
    while i < lines.len().saturating_sub(1) {
        if lines[i].starts_with("### ") {
            let title = lines[i].trim_start_matches('#').trim().to_string();
            if lines[i + 1].contains("<!-- status: done -->") {
                done_watermark = Some(i);
            } else if lines[i + 1].contains("<!-- status: pending -->") {
                pending_phases.push((i, title));
            }
        }
        i += 1;
    }

    // Return the first pending phase after the highest-numbered done phase.
    let watermark = done_watermark.unwrap_or(0);
    pending_phases
        .into_iter()
        .find(|(pos, _)| *pos > watermark)
        .map(|(_, title)| title)
}

/// Collect the phase IDs (e.g. "v0.15.14") of all phases currently marked `done`
/// in PLAN.md. Used to suppress failed-goal URGENT alerts when a later run completed
/// the phase successfully (v0.15.14.0).
fn collect_done_phase_ids(plan_content: &str) -> HashSet<String> {
    let lines: Vec<&str> = plan_content.lines().collect();
    let mut done = HashSet::new();
    let mut i = 0;
    while i < lines.len().saturating_sub(1) {
        if lines[i].starts_with("### ") && lines[i + 1].contains("<!-- status: done -->") {
            // Extract the phase ID — the first whitespace-delimited token after "### ".
            // e.g. "### v0.15.14 — Hierarchical Workflows" → "v0.15.14"
            let title = lines[i].trim_start_matches('#').trim();
            if let Some(id) = title.split_whitespace().next() {
                done.insert(id.to_string());
            }
        }
        i += 1;
    }
    done
}

/// Returns true if this is a system reviewer goal spawned by a governed workflow
/// and the review is no longer actionable by the user (v0.15.7.1).
///
/// Pattern: title matches "Review draft <short-id> for governed workflow".
/// These goals are internal orchestration artifacts — showing them as URGENT
/// is noise rather than signal. The user cannot do anything about a reviewer
/// that failed after the governed workflow already applied or denied the draft.
fn is_terminal_reviewer_goal(goal: &ta_goal::GoalRun) -> bool {
    let t = goal.title.as_str();
    t.starts_with("Review draft ") && t.ends_with(" for governed workflow")
}

fn count_pending_drafts(pr_packages_dir: &std::path::Path) -> usize {
    list_pending_draft_ids(pr_packages_dir).len()
}

fn list_pending_draft_ids(pr_packages_dir: &std::path::Path) -> Vec<String> {
    if !pr_packages_dir.exists() {
        return vec![];
    }
    std::fs::read_dir(pr_packages_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .filter_map(|e| {
                    let content = std::fs::read_to_string(e.path()).ok()?;
                    // Parse the JSON to check the status field directly.
                    // This avoids false positives from string matching on other fields and
                    // correctly excludes Applied drafts (root cause of item #9: Applied drafts
                    // appearing in the pending review count because the old string check
                    // didn't exclude Applied state).
                    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
                    let status = v["status"].as_str()?;
                    if status != "pending_review" {
                        return None;
                    }
                    // Extract ID from filename (strip .json extension).
                    e.path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
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

    // v0.15.7.1 — reviewer goal URGENT filter tests.
    #[test]
    fn is_terminal_reviewer_goal_matches_pattern() {
        use chrono::Utc;
        let make_goal = |title: &str| ta_goal::GoalRun {
            goal_run_id: uuid::Uuid::new_v4(),
            tag: None,
            title: title.to_string(),
            objective: String::new(),
            agent_id: String::new(),
            state: ta_goal::GoalRunState::Failed {
                reason: "test".to_string(),
            },
            manifest_id: uuid::Uuid::nil(),
            workspace_path: std::path::PathBuf::new(),
            store_path: std::path::PathBuf::new(),
            source_dir: None,
            plan_phase: None,
            parent_goal_id: None,
            source_snapshot: None,
            is_macro: false,
            parent_macro_id: None,
            sub_goal_ids: vec![],
            workflow_id: None,
            stage: None,
            role: None,
            context_from: vec![],
            thread_id: None,
            project_name: None,
            agent_pid: None,
            heartbeat_required: false,
            pr_url: None,
            pr_package_id: None,
            progress_note: None,
            vcs_isolation: None,
            initiated_by: None,
            memory_entries_created: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // Should match: this is a system reviewer goal.
        assert!(is_terminal_reviewer_goal(&make_goal(
            "Review draft f3eb3516 for governed workflow"
        )));
        // Should not match: regular failed goal.
        assert!(!is_terminal_reviewer_goal(&make_goal(
            "Fix the authentication bug"
        )));
        // Should not match: partial prefix.
        assert!(!is_terminal_reviewer_goal(&make_goal("Review draft")));
        // Should not match: different suffix.
        assert!(!is_terminal_reviewer_goal(&make_goal(
            "Review draft abc123 for some other workflow"
        )));
    }

    #[test]
    fn deep_status_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(dir.path());
        // Should not panic even with empty project.
        let _ = deep_status(&config);
    }

    #[test]
    fn list_pending_draft_ids_missing_dir() {
        let ids = list_pending_draft_ids(std::path::Path::new("/nonexistent/path"));
        assert!(ids.is_empty());
    }

    #[test]
    fn suggest_actions_pending_draft() {
        let ids = vec!["abc123".to_string()];
        let suggestions = suggest_next_actions(&[], 1, &ids, &[], None, &[], false);
        assert!(suggestions.iter().any(|s| s.contains("draft view")));
    }

    #[test]
    fn suggest_actions_no_work_next_phase() {
        let suggestions = suggest_next_actions(
            &[],
            0,
            &[],
            &[],
            Some("v0.13.2 — MCP Transport"),
            &[],
            false,
        );
        assert!(suggestions.iter().any(|s| s.contains("ta run")));
    }

    // v0.15.14.0 — watermark-based next phase logic.
    #[test]
    fn find_next_pending_phase_watermark_skips_deferred_pending_before_done() {
        // v0.9.4 is pending (deferred/skipped), v0.9.5 is done, v0.9.6 is pending.
        // The watermark is at v0.9.5. v0.9.4 is before the watermark — it should NOT
        // be returned as "next". v0.9.6 is after the watermark — it should be returned.
        let plan = r#"
### v0.9.4 — Deferred Phase
<!-- status: pending -->

### v0.9.5 — Completed Phase
<!-- status: done -->

### v0.9.6 — Next Phase
<!-- status: pending -->
"#;
        let result = find_next_pending_phase(plan);
        assert_eq!(result, Some("v0.9.6 — Next Phase".to_string()));
    }

    #[test]
    fn find_next_pending_phase_no_done_phase_returns_first_pending() {
        // No done phases at all: return the first pending phase.
        let plan = r#"
### v0.9.5 — First Phase
<!-- status: pending -->

### v0.9.6 — Second Phase
<!-- status: pending -->
"#;
        let result = find_next_pending_phase(plan);
        assert_eq!(result, Some("v0.9.5 — First Phase".to_string()));
    }

    // v0.15.14.0 — collect_done_phase_ids.
    #[test]
    fn collect_done_phase_ids_extracts_ids() {
        let plan = r#"
### v0.9.5 — Done Phase
<!-- status: done -->

### v0.9.6 — Pending Phase
<!-- status: pending -->

### v0.9.7 — Another Done
<!-- status: done -->
"#;
        let ids = collect_done_phase_ids(plan);
        assert!(ids.contains("v0.9.5"));
        assert!(ids.contains("v0.9.7"));
        assert!(!ids.contains("v0.9.6"));
    }

    // v0.15.14.0 — list_pending_draft_ids excludes Applied drafts.
    #[test]
    fn list_pending_draft_ids_excludes_applied() {
        let dir = tempfile::tempdir().unwrap();
        // Write a pending_review draft.
        std::fs::write(
            dir.path().join("pkg-pending.json"),
            r#"{"status":"pending_review","package_id":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .unwrap();
        // Write an applied draft — must NOT appear in results.
        std::fs::write(
            dir.path().join("pkg-applied.json"),
            r#"{"status":"applied","applied_at":"2026-04-13T00:00:00Z","package_id":"00000000-0000-0000-0000-000000000002"}"#,
        )
        .unwrap();
        // Write a denied draft — must NOT appear.
        std::fs::write(
            dir.path().join("pkg-denied.json"),
            r#"{"status":"denied","reason":"bad","denied_by":"tester","package_id":"00000000-0000-0000-0000-000000000003"}"#,
        )
        .unwrap();

        let ids = list_pending_draft_ids(dir.path());
        assert_eq!(ids.len(), 1);
        assert!(ids[0].contains("pkg-pending"));
    }
}
