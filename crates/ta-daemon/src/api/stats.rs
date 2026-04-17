// api/stats.rs — Velocity stats HTTP API (v0.15.14.2).
//
// GET /api/stats/velocity       — aggregate stats (merge local + committed)
// GET /api/stats/velocity-detail — per-goal list
//
// Query parameters shared by both:
//   ?since=YYYY-MM-DD    — filter entries from this date
//   ?phase_prefix=0.15   — filter to v0.15.x phases

use std::cmp::Reverse;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use ta_goal::{
    aggregate_by_contributor, detect_phase_conflicts, filter_by_phase_prefix,
    merge_velocity_entries, VelocityAggregate, VelocityHistoryStore, VelocityStore,
};

use super::AppState;

#[derive(Debug, Deserialize)]
pub struct VelocityQuery {
    /// Filter to entries from this date (YYYY-MM-DD).
    pub since: Option<String>,
    /// Filter to goals whose title starts with v<prefix>.
    pub phase_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VelocityDetailQuery {
    /// Filter to entries from this date (YYYY-MM-DD).
    pub since: Option<String>,
    /// Filter to goals whose title starts with v<prefix>.
    pub phase_prefix: Option<String>,
    /// Maximum entries to return (default 50).
    pub limit: Option<usize>,
}

/// `GET /api/stats/velocity` — aggregate velocity stats.
pub async fn velocity_aggregate(
    State(state): State<Arc<AppState>>,
    Query(params): Query<VelocityQuery>,
) -> impl IntoResponse {
    let project_root = state.project_root.clone();
    let result = tokio::task::spawn_blocking(move || {
        let local_store = VelocityStore::for_project(&project_root);
        let history_store = VelocityHistoryStore::for_project(&project_root);

        let local = local_store.load_all().unwrap_or_default();
        let committed = history_store.load_all().unwrap_or_default();
        let (merged, committed_ids) = merge_velocity_entries(local, committed);

        let mut entries = if let Some(since_str) = &params.since {
            if let Ok(dt) = parse_date(since_str) {
                merged.into_iter().filter(|e| e.started_at >= dt).collect()
            } else {
                return Err(format!(
                    "Invalid date '{}' — expected YYYY-MM-DD",
                    since_str
                ));
            }
        } else {
            merged
        };

        if let Some(prefix) = &params.phase_prefix {
            entries = filter_by_phase_prefix(entries, prefix);
        }

        let agg = VelocityAggregate::from_entries(&entries);
        let committed_entries: Vec<_> = entries
            .iter()
            .filter(|e| committed_ids.contains(&e.goal_id))
            .cloned()
            .collect();
        let by_contributor = aggregate_by_contributor(&committed_entries);
        let phase_conflicts = detect_phase_conflicts(&committed_entries);

        let local_only_count = entries
            .iter()
            .filter(|e| !committed_ids.contains(&e.goal_id))
            .count();

        Ok(serde_json::json!({
            "aggregate": agg,
            "by_contributor": by_contributor,
            "phase_conflicts": phase_conflicts,
            "local_only_count": local_only_count,
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => Json(json).into_response(),
        Ok(Err(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Internal error: {}", e),
        )
            .into_response(),
    }
}

/// `GET /api/stats/velocity-detail` — per-goal velocity breakdown.
pub async fn velocity_detail(
    State(state): State<Arc<AppState>>,
    Query(params): Query<VelocityDetailQuery>,
) -> impl IntoResponse {
    let project_root = state.project_root.clone();
    let result = tokio::task::spawn_blocking(move || {
        let local_store = VelocityStore::for_project(&project_root);
        let history_store = VelocityHistoryStore::for_project(&project_root);

        let local = local_store.load_all().unwrap_or_default();
        let committed = history_store.load_all().unwrap_or_default();
        let (merged, _) = merge_velocity_entries(local, committed);

        let mut entries = if let Some(since_str) = &params.since {
            if let Ok(dt) = parse_date(since_str) {
                merged.into_iter().filter(|e| e.started_at >= dt).collect()
            } else {
                return Err(format!(
                    "Invalid date '{}' — expected YYYY-MM-DD",
                    since_str
                ));
            }
        } else {
            merged
        };

        if let Some(prefix) = &params.phase_prefix {
            entries = filter_by_phase_prefix(entries, prefix);
        }

        // Newest first.
        entries.sort_by_key(|e| Reverse(e.started_at));
        let limit = params.limit.unwrap_or(50);
        entries.truncate(limit);

        Ok(entries)
    })
    .await;

    match result {
        Ok(Ok(entries)) => Json(entries).into_response(),
        Ok(Err(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Internal error: {}", e),
        )
            .into_response(),
    }
}

fn parse_date(s: &str) -> Result<chrono::DateTime<chrono::Utc>, ()> {
    use chrono::TimeZone;
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|d| chrono::Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()))
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_valid() {
        let dt = parse_date("2026-01-15").unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2026-01-15");
    }

    #[test]
    fn parse_date_invalid() {
        assert!(parse_date("not-a-date").is_err());
        assert!(parse_date("2026/01/15").is_err());
    }
}
