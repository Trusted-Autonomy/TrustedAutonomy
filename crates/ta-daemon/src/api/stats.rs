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

// ── Studio Stats tab (v0.17.0.12.6 item 6) ────────────────────────────────────

/// `GET /api/stats/summary` — aggregate goal stats + plan velocity for the
/// Studio "Stats" tab, with Meridian KPI alignment scores folded in when
/// `meridian.toml` is configured at the project root.
pub async fn summary(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.project_root.clone();
    let goal_stats = tokio::task::spawn_blocking(move || {
        let local_store = VelocityStore::for_project(&project_root);
        let history_store = VelocityHistoryStore::for_project(&project_root);
        let local = local_store.load_all().unwrap_or_default();
        let committed = history_store.load_all().unwrap_or_default();
        let (merged, _) = merge_velocity_entries(local, committed);

        let agg = VelocityAggregate::from_entries(&merged);
        let completion_rate = if agg.total_goals > 0 {
            agg.applied as f64 / agg.total_goals as f64
        } else {
            0.0
        };

        let mut goals_by_phase: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for entry in &merged {
            let phase = entry
                .plan_phase
                .clone()
                .unwrap_or_else(|| "unassigned".to_string());
            *goals_by_phase.entry(phase).or_insert(0) += 1;
        }

        (agg, completion_rate, goals_by_phase)
    })
    .await;

    let (aggregate, completion_rate, goals_by_phase) = match goal_stats {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to compute velocity stats: {}", e)
                })),
            )
                .into_response();
        }
    };

    let meridian_kpis =
        meridian_kpis_if_configured(&state.project_root, &state.daemon_config).await;

    Json(serde_json::json!({
        "total_goals": aggregate.total_goals,
        "completion_rate": completion_rate,
        "avg_duration_secs": aggregate.avg_build_seconds,
        "goals_by_phase": goals_by_phase,
        "velocity": aggregate,
        "meridian_kpis": meridian_kpis,
    }))
    .into_response()
}

/// Shell out to `meridian analyze --source ta --path <root> --format json`
/// when `meridian.toml` exists at the project root, returning its parsed
/// output as an opaque JSON value. Returns `None` (not an error) when
/// Meridian isn't configured, isn't installed, or the call fails/times out —
/// this is purely an informational enhancement to the Stats tab, never a
/// blocking dependency.
async fn meridian_kpis_if_configured(
    project_root: &std::path::Path,
    daemon_config: &crate::config::DaemonConfig,
) -> Option<serde_json::Value> {
    if !project_root.join("meridian.toml").exists() {
        return None;
    }

    let binary = resolve_meridian_binary(daemon_config);
    let project_root = project_root.to_path_buf();

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(&binary)
                .args([
                    "analyze",
                    "--source",
                    "ta",
                    "--path",
                    &project_root.to_string_lossy(),
                    "--format",
                    "json",
                ])
                .output()
        }),
    )
    .await;

    match output {
        Ok(Ok(Ok(out))) if out.status.success() => {
            serde_json::from_slice::<serde_json::Value>(&out.stdout)
                .ok()
                .or_else(|| {
                    tracing::warn!("meridian analyze --format json returned non-JSON output");
                    None
                })
        }
        Ok(Ok(Ok(out))) => {
            tracing::warn!(
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr),
                "meridian analyze exited non-zero; omitting KPI data from Stats tab"
            );
            None
        }
        Ok(Ok(Err(e))) => {
            tracing::warn!(err = %e, "Failed to spawn meridian binary for Stats tab KPI data");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "meridian analyze task panicked");
            None
        }
        Err(_) => {
            tracing::warn!(
                "meridian analyze timed out after 10s; omitting KPI data from Stats tab"
            );
            None
        }
    }
}

/// Resolve the `meridian` binary path: `TA_MERIDIAN_BINARY` env var, then
/// `.ta/daemon.toml` `[meridian] binary`, then bare `meridian` (PATH lookup).
fn resolve_meridian_binary(daemon_config: &crate::config::DaemonConfig) -> String {
    if let Ok(path) = std::env::var("TA_MERIDIAN_BINARY") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Some(path) = &daemon_config.meridian.binary {
        return path.to_string_lossy().into_owned();
    }
    "meridian".to_string()
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
