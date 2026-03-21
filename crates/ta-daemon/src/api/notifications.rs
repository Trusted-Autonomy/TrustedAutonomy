// api/notifications.rs — Proactive notification push endpoint (v0.13.1.6).
//
// `GET /api/notifications` scans current project state and returns actionable
// push notifications that clients (shell, channels, CLI) can display.
//
// Notification types:
//   - goal_stuck        — Running goal with no heartbeat > threshold
//   - goal_failed       — Goal transitioned to Failed state
//   - draft_ready       — Draft in PendingReview awaiting approval
//   - corrective_action — Watchdog-detected issue with proposed fix
//   - disk_warning      — Available disk < threshold (from status endpoint)
//
// Clients should poll this endpoint periodically (e.g., every 60s) and
// surface notifications through their channel (shell badge, Discord DM, etc.).

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;

use ta_goal::operations::{ActionSeverity, ActionStatus, OperationsLog};
use ta_goal::{GoalRunState, GoalRunStore};

use crate::api::AppState;

/// Severity of a notification.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warning,
    Critical,
}

/// A single actionable notification.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    /// Stable identifier for deduplication (callers can track seen IDs).
    pub id: String,
    /// Machine-readable type for routing/filtering.
    pub notification_type: String,
    /// Severity level.
    pub severity: NotificationSeverity,
    /// One-line summary.
    pub summary: String,
    /// Optional detail message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Suggested action the user can take.
    pub suggested_action: String,
    /// When this notification was generated.
    pub generated_at: DateTime<Utc>,
    /// Related entity ID (goal ID, draft ID, etc.) if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
}

impl Notification {
    fn new(
        id: impl Into<String>,
        notification_type: impl Into<String>,
        severity: NotificationSeverity,
        summary: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            notification_type: notification_type.into(),
            severity,
            summary: summary.into(),
            detail: None,
            suggested_action: suggested_action.into(),
            generated_at: Utc::now(),
            entity_id: None,
        }
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn with_entity(mut self, entity_id: impl Into<String>) -> Self {
        self.entity_id = Some(entity_id.into());
        self
    }
}

/// Response payload for `GET /api/notifications`.
#[derive(Debug, Serialize)]
pub struct NotificationsResponse {
    pub notifications: Vec<Notification>,
    pub total: usize,
    pub urgent: usize,
}

/// `GET /api/notifications` — Proactive push notifications.
///
/// Returns actionable items that require human attention, ordered by
/// severity (critical first). Clients may cache and deduplicate by `id`.
pub async fn get_notifications(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut notifications = Vec::new();

    // ── Stuck goals ──────────────────────────────────────────────────────
    if let Ok(store) = GoalRunStore::new(&state.goals_dir) {
        let now = Utc::now();
        let stuck_threshold_hours = 2i64;

        if let Ok(all_goals) = store.list() {
            for goal in &all_goals {
                // Stuck: running but not updated in > threshold.
                if matches!(goal.state, GoalRunState::Running | GoalRunState::Configured) {
                    let stale_hours = (now - goal.updated_at).num_hours();
                    if stale_hours >= stuck_threshold_hours {
                        let id = format!("goal_stuck:{}", &goal.goal_run_id.to_string()[..8]);
                        let notif = Notification::new(
                            id,
                            "goal_stuck",
                            NotificationSeverity::Warning,
                            format!(
                                "Goal \"{}\" appears stuck ({}h without update)",
                                goal.title, stale_hours
                            ),
                            format!(
                                "ta goal list  →  ta gc to clean up or ta run --follow-up {} to retry",
                                &goal.goal_run_id.to_string()[..8]
                            ),
                        )
                        .with_entity(goal.goal_run_id.to_string());
                        notifications.push(notif);
                    }
                }

                // Failed goals (last 24h).
                if matches!(goal.state, GoalRunState::Failed { .. }) {
                    let hours = (now - goal.updated_at).num_hours();
                    if hours < 24 {
                        let id = format!("goal_failed:{}", &goal.goal_run_id.to_string()[..8]);
                        let notif = Notification::new(
                            id,
                            "goal_failed",
                            NotificationSeverity::Warning,
                            format!("Goal \"{}\" failed {}h ago", goal.title, hours),
                            format!(
                                "ta run --follow-up {} to retry",
                                &goal.goal_run_id.to_string()[..8]
                            ),
                        )
                        .with_entity(goal.goal_run_id.to_string());
                        notifications.push(notif);
                    }
                }

                // Drafts ready for review (PrReady state).
                if matches!(goal.state, GoalRunState::PrReady) {
                    let id = format!("draft_ready:{}", &goal.goal_run_id.to_string()[..8]);
                    let notif = Notification::new(
                        id,
                        "draft_ready",
                        NotificationSeverity::Info,
                        format!("Draft ready for \"{}\"", goal.title),
                        "ta draft list  →  ta draft view <id>  →  ta draft approve <id>"
                            .to_string(),
                    )
                    .with_entity(goal.goal_run_id.to_string());
                    notifications.push(notif);
                }
            }
        }
    }

    // ── Pending drafts (from draft packages dir) ─────────────────────────
    let draft_count = count_pending_drafts(&state.pr_packages_dir);
    if draft_count > 0 {
        let id = format!("pending_drafts:{}", draft_count);
        let notif = Notification::new(
            id,
            "draft_ready",
            NotificationSeverity::Info,
            format!("{} draft(s) awaiting your review", draft_count),
            "ta draft list  →  ta draft view <id> to review each draft".to_string(),
        );
        notifications.push(notif);
    }

    // ── Corrective actions ────────────────────────────────────────────────
    let ops_log = OperationsLog::for_project(&state.project_root);
    if let Ok(actions) = ops_log.read(Some(20)) {
        for action in &actions {
            if !matches!(action.status, ActionStatus::Proposed) {
                continue;
            }
            let sev = match action.severity {
                ActionSeverity::Critical => NotificationSeverity::Critical,
                ActionSeverity::Warning => NotificationSeverity::Warning,
                ActionSeverity::Info => NotificationSeverity::Info,
            };
            let id = format!("corrective:{}", &action.id.to_string()[..8]);
            let mut notif = Notification::new(
                id,
                "corrective_action",
                sev,
                action.issue.clone(),
                format!("ta operations log  →  action key: {}", action.action_key),
            )
            .with_detail(format!("{} — {}", action.diagnosis, action.proposed_action));

            if let Some(goal_id) = action.goal_id {
                notif = notif.with_entity(goal_id.to_string());
            }

            notifications.push(notif);
        }
    }

    // ── Sort: critical first, then warning, then info ─────────────────────
    notifications.sort_by_key(|n| match n.severity {
        NotificationSeverity::Critical => 0u8,
        NotificationSeverity::Warning => 1,
        NotificationSeverity::Info => 2,
    });

    let urgent = notifications
        .iter()
        .filter(|n| {
            matches!(
                n.severity,
                NotificationSeverity::Critical | NotificationSeverity::Warning
            )
        })
        .count();

    let total = notifications.len();

    Json(NotificationsResponse {
        notifications,
        total,
        urgent,
    })
    .into_response()
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
                        .map(|c| c.contains("PendingReview"))
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
    fn notification_builder() {
        let n = Notification::new(
            "test:id",
            "test_type",
            NotificationSeverity::Warning,
            "Something happened",
            "Do something about it",
        )
        .with_detail("More details here")
        .with_entity("abc123");

        assert_eq!(n.id, "test:id");
        assert_eq!(n.notification_type, "test_type");
        assert!(matches!(n.severity, NotificationSeverity::Warning));
        assert_eq!(n.detail, Some("More details here".to_string()));
        assert_eq!(n.entity_id, Some("abc123".to_string()));
    }

    #[test]
    fn notification_serializes() {
        let n = Notification::new(
            "id1",
            "goal_stuck",
            NotificationSeverity::Critical,
            "Goal is stuck",
            "ta gc",
        );
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("goal_stuck"));
        assert!(json.contains("critical"));
    }

    #[test]
    fn count_pending_drafts_missing_dir() {
        let count = count_pending_drafts(std::path::Path::new("/nonexistent/path"));
        assert_eq!(count, 0);
    }
}
