// api/status.rs — Project status endpoint (`GET /api/status`).
//
// Returns the same data as `ta status` but as JSON — project name, version,
// current phase, active agents, pending drafts, and recent events.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use ta_events::store::{EventQueryFilter, EventStore, FsEventStore};
use ta_goal::{GoalRunState, GoalRunStore};

use crate::api::AppState;

#[derive(Debug, Serialize)]
pub struct ProjectStatus {
    pub project: String,
    pub version: String,
    /// Explicit daemon version field for version guard checks.
    /// Always matches `version` but provides a stable API contract.
    pub daemon_version: String,
    /// Build SHA (VCS revision at compile time) for same-version rebuild detection.
    /// The version guard compares this to detect rebuilds within the same semver.
    pub build_sha: String,
    /// The default agent binary for shell Q&A (e.g., "claude-code").
    pub default_agent: String,
    pub current_phase: Option<PhaseInfo>,
    pub active_agents: Vec<AgentInfo>,
    pub pending_drafts: usize,
    pub active_goals: usize,
    pub total_goals: usize,
    pub recent_events: Vec<serde_json::Value>,
    /// Maximum concurrent parallel sessions allowed (v0.11.5 item 16).
    pub max_parallel_sessions: usize,
    /// Idle timeout for parallel sessions in seconds (v0.11.5 item 16).
    pub parallel_idle_timeout_secs: u64,
    /// CSS color for the shell input cursor (v0.11.7 item 6).
    pub cursor_color: String,
    /// Cursor style: "block", "bar", or "underline" (v0.11.7 item 6).
    pub cursor_style: String,
    /// Seconds without heartbeat before showing no-heartbeat alert (v0.11.7 item 2).
    pub no_heartbeat_alert_secs: u32,
    /// Whether a power assertion (preventing idle sleep) is currently active (v0.13.1.1).
    /// True when there are active goals and `prevent_sleep_during_active_goals` is enabled.
    pub power_assertion_active: bool,
    /// Number of community resources with stale or missing caches (v0.14.7).
    /// A resource is "pending" if its cache is older than 30 days or has never been synced.
    pub community_pending_count: usize,
    /// Absolute path of the currently active project root (v0.14.18).
    /// None if no valid project root has been set (triggers Projects tab redirect in TA Studio).
    pub active_project_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PhaseInfo {
    pub id: String,
    pub title: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub agent_id: String,
    pub goal_id: String,
    /// Human-friendly goal tag (v0.11.2.3).
    pub tag: String,
    pub title: String,
    pub state: String,
    pub running_secs: i64,
    /// Whether the agent is considered actively running (updated within the idle threshold).
    pub active: bool,
    /// VCS review state (e.g., "open", "merged") if a PR exists (v0.11.2.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs_state: Option<String>,
    /// Agent process health: "alive", "dead", "unknown", or null for terminal states (v0.11.2.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_health: Option<String>,
    /// Agent process ID, if tracked (v0.11.2.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
}

/// `GET /api/status` — Project dashboard as JSON.
pub async fn project_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Determine active project root and whether it's valid.
    let (active_root, active_project_path) = {
        let root = state.active_project_root.read().unwrap().clone();
        let valid = root.join(".ta").exists();
        let path_str = if valid {
            Some(root.display().to_string())
        } else {
            None
        };
        (root, path_str)
    };

    let project_name = crate::api::project_browser::read_project_name(&active_root);

    let version = env!("CARGO_PKG_VERSION").to_string();

    // Current plan phase.
    let current_phase = {
        let plan_path = active_root.join("PLAN.md");
        if plan_path.exists() {
            std::fs::read_to_string(&plan_path)
                .ok()
                .and_then(|content| find_next_pending_phase(&content))
        } else {
            None
        }
    };

    // Goals and agents.
    let (active_agents, active_goals, total_goals) = {
        let goal_store = GoalRunStore::new(&state.goals_dir);
        match goal_store {
            Ok(store) => {
                let all = store.list().unwrap_or_default();
                // Idle threshold: an agent is "active" if updated within the last 10 minutes.
                let idle_threshold = chrono::Duration::minutes(10);
                let now = chrono::Utc::now();

                let active: Vec<AgentInfo> = all
                    .iter()
                    .filter(|g| {
                        matches!(
                            g.state,
                            GoalRunState::Running
                                | GoalRunState::Configured
                                | GoalRunState::PrReady
                        )
                    })
                    .map(|g| {
                        let elapsed = now.signed_duration_since(g.created_at).num_seconds();
                        let is_active = (now - g.updated_at) < idle_threshold;
                        let health = crate::watchdog::process_health_label(g);
                        let health_opt = if health == "—" {
                            None
                        } else {
                            Some(health.to_string())
                        };
                        AgentInfo {
                            agent_id: g.agent_id.clone(),
                            goal_id: g.goal_run_id.to_string(),
                            tag: g.display_tag(),
                            title: g.title.clone(),
                            state: g.state.to_string(),
                            running_secs: elapsed,
                            active: is_active,
                            vcs_state: None, // Populated by VCS check if configured
                            process_health: health_opt,
                            agent_pid: g.agent_pid,
                        }
                    })
                    .collect();
                let active_count = active.iter().filter(|a| a.active).count();
                let total = all.len();
                (active, active_count, total)
            }
            Err(_) => (vec![], 0, 0),
        }
    };

    // Pending drafts.
    let pending_drafts = count_pending_drafts(&state.pr_packages_dir);

    // Recent events (last 10).
    let recent_events = {
        let store = FsEventStore::new(&state.events_dir);
        store
            .query(&EventQueryFilter {
                limit: Some(10),
                ..Default::default()
            })
            .unwrap_or_default()
            .into_iter()
            .rev() // Most recent first.
            .map(|e| serde_json::to_value(&e).unwrap_or_default())
            .collect()
    };

    let power_assertion_active =
        active_goals > 0 && state.daemon_config.power.prevent_sleep_during_active_goals;

    // Community cache staleness check (v0.14.7).
    // Count resources whose cache is missing or older than 30 days.
    let community_pending_count = count_stale_community_resources(&active_root);

    Json(ProjectStatus {
        project: project_name,
        version: version.clone(),
        daemon_version: version,
        build_sha: env!("TA_GIT_HASH").to_string(),
        default_agent: state.daemon_config.agent.default_agent.clone(),
        current_phase,
        active_agents,
        pending_drafts,
        active_goals,
        total_goals,
        recent_events,
        max_parallel_sessions: state.daemon_config.agent.max_parallel_sessions,
        parallel_idle_timeout_secs: state.daemon_config.agent.parallel_idle_timeout_secs,
        cursor_color: state.daemon_config.shell.ui.cursor_color.clone(),
        cursor_style: state.daemon_config.shell.ui.cursor_style.clone(),
        no_heartbeat_alert_secs: state.daemon_config.shell.ui.no_heartbeat_alert_secs,
        power_assertion_active,
        community_pending_count,
        active_project_path,
    })
}

/// Count community resources with stale or missing caches.
///
/// Reads `<project_root>/.ta/community-resources.toml` for resource names,
/// then checks each resource's `_meta.json` cache file. Resources with no cache
/// or caches older than 30 days are counted as "pending".
fn count_stale_community_resources(project_root: &std::path::Path) -> usize {
    let resources_path = project_root.join(".ta").join("community-resources.toml");
    let cache_root = project_root.join(".ta").join("community-cache");

    if !resources_path.exists() {
        return 0;
    }

    let content = match std::fs::read_to_string(&resources_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let stale_threshold = chrono::Duration::days(30);
    let now = chrono::Utc::now();
    let mut stale_count = 0;

    // Extract resource names from TOML: lines like `name = "..."` inside [[resource]] blocks.
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name = ") {
            let name = rest.trim().trim_matches('"');
            if name.is_empty() {
                continue;
            }
            let meta_path = cache_root.join(name).join("_meta.json");
            if !meta_path.exists() {
                stale_count += 1;
                continue;
            }
            // Try to read synced_at from the meta file.
            if let Ok(meta_content) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_content) {
                    if let Some(synced_at_str) = meta["synced_at"].as_str() {
                        if let Ok(synced_at) = chrono::DateTime::parse_from_rfc3339(synced_at_str) {
                            let age =
                                now.signed_duration_since(synced_at.with_timezone(&chrono::Utc));
                            if age > stale_threshold {
                                stale_count += 1;
                            }
                            continue;
                        }
                    }
                }
            }
            // If we can't parse the meta, count as stale.
            stale_count += 1;
        }
    }

    stale_count
}

fn find_next_pending_phase(plan_content: &str) -> Option<PhaseInfo> {
    let lines: Vec<&str> = plan_content.lines().collect();
    for i in 0..lines.len().saturating_sub(1) {
        if lines[i].starts_with("### ") && lines[i + 1].contains("<!-- status: pending -->") {
            let raw = lines[i].trim_start_matches('#').trim();
            // Parse "vX.Y.Z — Title" format.
            let (id, title) = if let Some(sep_idx) = raw.find(" — ") {
                (
                    raw[..sep_idx].trim().to_string(),
                    raw[sep_idx + " — ".len()..].trim().to_string(),
                )
            } else {
                (raw.to_string(), raw.to_string())
            };
            return Some(PhaseInfo {
                id,
                title,
                status: "pending".to_string(),
            });
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
    fn find_phase_from_plan() {
        let plan = r#"
### v0.9.6 — Orchestrator API
<!-- status: done -->

### v0.9.7 — Daemon API Expansion
<!-- status: pending -->

### v0.9.8 — Interactive Shell
<!-- status: pending -->
"#;
        let phase = find_next_pending_phase(plan).unwrap();
        assert_eq!(phase.id, "v0.9.7");
        assert_eq!(phase.title, "Daemon API Expansion");
        assert_eq!(phase.status, "pending");
    }

    #[test]
    fn no_pending_phase() {
        let plan = "### Done\n<!-- status: done -->";
        assert!(find_next_pending_phase(plan).is_none());
    }
}
