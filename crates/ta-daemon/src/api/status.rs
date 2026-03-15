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
    let project_name = state
        .project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let version = env!("CARGO_PKG_VERSION").to_string();

    // Current plan phase.
    let current_phase = {
        let plan_path = state.project_root.join("PLAN.md");
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
    })
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
