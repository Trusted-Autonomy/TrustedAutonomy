// api/spend.rs: spend by origin class (v0.17.12.15).
//
//   GET /api/spend?group_by=origin_class,project&since=&until=
//
// Read-only: it changes nothing and returns no secrets, so it needs no human
// credential (a GET is `RouteClass::Safe` in `human_gate`).
//
// The numbers come from the GOAL RECORDS (`.ta/goals/`), not from the velocity
// file: a project's velocity file is a plain file in the project tree that an
// agent can edit, while a goal record is written by `ta run`. Totals per group
// carry the count of exact (runtime-reported), estimated (price table) and
// unpriced goals so a reader knows how far to trust the dollars.
//
// `origin_class` is accounting data: this endpoint groups by it and nothing
// else reads it.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use ta_goal::spend::{aggregate, parse_group_by, parse_time_bound};
use ta_goal::GoalRunStore;

use super::AppState;

#[derive(Debug, Deserialize)]
pub struct SpendQuery {
    /// Comma-separated: origin_class, project, origin, model. Empty: one group.
    pub group_by: Option<String>,
    /// Goals created at or after this (RFC 3339 or YYYY-MM-DD).
    pub since: Option<String>,
    /// Goals created before this (RFC 3339 or YYYY-MM-DD).
    pub until: Option<String>,
}

fn bad_request(what: &str, detail: String) -> Response {
    tracing::warn!(endpoint = "GET /api/spend", parameter = what, error = %detail, "rejected spend query");
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": format!("GET /api/spend: bad {what}: {detail}"),
            "usage": "GET /api/spend?group_by=origin_class,project&since=2026-10-01&until=2026-11-01",
        })),
    )
        .into_response()
}

/// (project name, goals directory) for every project this daemon serves.
fn goal_dirs(state: &AppState) -> Vec<(String, PathBuf)> {
    if state.project_registry.list().is_empty() {
        let name = state
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        return vec![(name, state.goals_dir.clone())];
    }
    crate::attention::project_roots(state)
        .into_iter()
        .map(|(name, root)| (name, root.join(".ta").join("goals")))
        .collect()
}

/// `GET /api/spend`: grouped spend totals.
pub async fn spend_report(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SpendQuery>,
) -> Response {
    let group_by = match parse_group_by(q.group_by.as_deref().unwrap_or("")) {
        Ok(g) => g,
        Err(e) => return bad_request("group_by", e),
    };
    let since = match q
        .since
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(parse_time_bound)
    {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => return bad_request("since", e),
        None => None,
    };
    let until = match q
        .until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(parse_time_bound)
    {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => return bad_request("until", e),
        None => None,
    };
    if let (Some(s), Some(u)) = (since, until) {
        if s >= u {
            return bad_request(
                "since/until",
                format!("since ({s}) must be before until ({u})"),
            );
        }
    }

    let dirs = goal_dirs(&state);
    let result = tokio::task::spawn_blocking(move || {
        let mut goals = Vec::new();
        let mut projects = Vec::new();
        for (name, dir) in dirs {
            let loaded = if dir.is_dir() {
                GoalRunStore::new(&dir).and_then(|s| s.list())
            } else {
                Ok(Vec::new())
            };
            match loaded {
                Ok(list) => {
                    projects.push(serde_json::json!({
                        "project": name, "goals_dir": dir.display().to_string(),
                        "goals_read": list.len(),
                    }));
                    // <root>/.ta/goals -> <root> for that project's price table.
                    let table = dir
                        .parent()
                        .and_then(|p| p.parent())
                        .map(ta_goal::PriceTable::load)
                        .unwrap_or_else(ta_goal::PriceTable::builtin);
                    goals.extend(list.into_iter().map(|mut g| {
                        ta_goal::spend::estimate_legacy_goal_cost(&mut g, &table);
                        (name.clone(), g)
                    }));
                }
                Err(e) => {
                    tracing::warn!(project = %name, dir = %dir.display(), error = %e, "could not read goal records for GET /api/spend");
                    projects.push(serde_json::json!({
                        "project": name, "goals_dir": dir.display().to_string(),
                        "error": format!("could not read goal records: {e}. Check the directory exists and is readable."),
                    }));
                }
            }
        }
        let report = aggregate(&goals, &group_by, since, until);
        let mut value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("projects".to_string(), serde_json::Value::Array(projects));
        }
        value
    })
    .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            tracing::error!(endpoint = "GET /api/spend", error = %e, "spend aggregation task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("GET /api/spend: the aggregation task failed: {e}. Retry; if it persists check the daemon log."),
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{build_api_router, AppState};
    use crate::config::DaemonConfig;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use chrono::{DateTime, Utc};
    use ta_goal::{CostSource, GoalRun, OriginClass};
    use tower::ServiceExt;

    struct Rig {
        project: tempfile::TempDir,
        app: axum::Router,
    }

    fn rig() -> Rig {
        let project = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new(
            project.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        Rig {
            app: build_api_router(state),
            project,
        }
    }

    fn save_goal(r: &Rig, class: Option<OriginClass>, usd: f64, source: CostSource, created: &str) {
        let dir = r.project.path().join(".ta").join("goals");
        let store = GoalRunStore::new(&dir).unwrap();
        let mut g = GoalRun::new(
            "t",
            "o",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        );
        g.origin_class = class;
        // A goal with no cost source and no model is genuinely unpriced: nothing
        // to price it from. (With a model and tokens it would be a legacy goal
        // priced at report time; see `a_goal_from_before_cost_tracking_...`.)
        if source != CostSource::None {
            g.agent_model = "claude-sonnet-5-5".into();
        }
        g.input_tokens = 10;
        g.output_tokens = 20;
        g.cache_read_input_tokens = 1_000;
        g.cache_creation_input_tokens = 100;
        g.cost_usd = usd;
        g.cost_source = source;
        g.cost_estimated = source == CostSource::Table;
        g.created_at = DateTime::parse_from_rfc3339(created)
            .unwrap()
            .with_timezone(&Utc);
        store.save(&g).unwrap();
    }

    async fn get(r: &Rig, uri: &str) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let resp = r.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn seed(r: &Rig) {
        save_goal(
            r,
            Some(OriginClass::Asked),
            2.0,
            CostSource::Runtime,
            "2026-10-01T00:00:00Z",
        );
        save_goal(
            r,
            Some(OriginClass::Asked),
            3.0,
            CostSource::Table,
            "2026-10-05T00:00:00Z",
        );
        save_goal(
            r,
            Some(OriginClass::Unasked),
            1.5,
            CostSource::Runtime,
            "2026-10-10T00:00:00Z",
        );
        save_goal(r, None, 0.0, CostSource::None, "2026-10-11T00:00:00Z");
    }

    #[tokio::test]
    async fn groups_by_origin_class_and_project_with_exact_and_estimated_counts() {
        let r = rig();
        seed(&r);
        let (st, v) = get(&r, "/api/spend?group_by=origin_class,project").await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(
            v["group_by"],
            serde_json::json!(["origin_class", "project"])
        );
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "asked and unasked, in one project: {v}");
        let asked = groups
            .iter()
            .find(|g| g["group"]["origin_class"] == "asked")
            .unwrap();
        assert!(asked["group"]["project"].is_string());
        assert_eq!(asked["goals"], 2);
        assert_eq!(asked["exact_cost_goals"], 1);
        assert_eq!(asked["estimated_cost_goals"], 1);
        assert_eq!(asked["usd"], 5.0);
        assert_eq!(asked["cache_read_tokens"], 2_000);
        let unasked = groups
            .iter()
            .find(|g| g["group"]["origin_class"] == "unasked")
            .unwrap();
        assert_eq!(
            unasked["goals"], 2,
            "a goal with no class counts as unasked"
        );
        assert_eq!(unasked["unpriced_goals"], 1);
        assert_eq!(v["totals"]["goals"], 4);
        assert_eq!(v["totals"]["estimated_cost_goals"], 1);
        assert_eq!(v["totals"]["exact_cost_goals"], 2);
        assert_eq!(v["projects"][0]["goals_read"], 4);
    }

    #[tokio::test]
    async fn a_goal_from_before_cost_tracking_is_priced_as_an_estimate_in_the_total() {
        let r = rig();
        let dir = r.project.path().join(".ta").join("goals");
        let store = GoalRunStore::new(&dir).unwrap();
        let mut g = GoalRun::new(
            "old",
            "o",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        );
        // Recorded before cost tracking: model and tokens, no cost source.
        g.agent_model = "claude-sonnet-5-5".into();
        g.input_tokens = 1_000_000;
        g.output_tokens = 100_000;
        store.save(&g).unwrap();
        let (st, v) = get(&r, "/api/spend").await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["totals"]["unpriced_goals"], 0, "{v}");
        assert_eq!(v["totals"]["estimated_cost_goals"], 1, "{v}");
        assert!(v["totals"]["usd"].as_f64().unwrap() > 0.0, "{v}");
    }

    #[tokio::test]
    async fn since_and_until_filter_by_goal_creation_time() {
        let r = rig();
        seed(&r);
        let (_, v) = get(&r, "/api/spend?since=2026-10-05&until=2026-10-11").await;
        assert_eq!(v["totals"]["goals"], 2, "the 5th and the 10th: {v}");
        let (_, v) = get(&r, "/api/spend?since=2026-10-11T00:00:00Z").await;
        assert_eq!(v["totals"]["goals"], 1);
        let (_, v) = get(&r, "/api/spend?until=2026-10-01").await;
        assert_eq!(v["totals"]["goals"], 0);
        assert_eq!(v["groups"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn no_group_by_is_one_total() {
        let r = rig();
        seed(&r);
        let (st, v) = get(&r, "/api/spend").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["groups"].as_array().unwrap().len(), 1);
        assert_eq!(v["totals"]["usd"], 6.5);
    }

    #[tokio::test]
    async fn bad_parameters_are_400_with_a_usable_message() {
        let r = rig();
        for (uri, needle) in [
            ("/api/spend?group_by=color", "group_by"),
            ("/api/spend?since=yesterday", "since"),
            ("/api/spend?until=nope", "until"),
            ("/api/spend?since=2026-10-10&until=2026-10-01", "before"),
        ] {
            let (st, v) = get(&r, uri).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{uri}");
            let msg = v["error"].as_str().unwrap();
            assert!(
                msg.contains("GET /api/spend") && msg.contains(needle),
                "{uri}: {msg}"
            );
            assert!(v["usage"].is_string(), "{uri}");
        }
    }

    #[tokio::test]
    async fn totals_come_from_goal_records_not_the_agent_writable_velocity_file() {
        let r = rig();
        seed(&r);
        // An agent writes a huge fake cost into the project's velocity file.
        std::fs::write(
            r.project.path().join(".ta/velocity-stats.json"),
            r#"{"cost_usd": 999999.0, "output_tokens": 999999999}"#,
        )
        .unwrap();
        std::fs::write(
            r.project.path().join(".ta/velocity-stats.jsonl"),
            "{\"cost_usd\": 999999.0}\n",
        )
        .unwrap();
        let (_, v) = get(&r, "/api/spend").await;
        assert_eq!(v["totals"]["usd"], 6.5);
        assert!(v["notes"][0].as_str().unwrap().contains("goal records"));
    }

    #[tokio::test]
    async fn a_read_needs_no_credential_and_the_response_has_no_secrets() {
        let r = rig();
        seed(&r);
        let (st, v) = get(&r, "/api/spend?group_by=model").await;
        assert_eq!(
            st,
            StatusCode::OK,
            "a GET is a read: no human credential needed"
        );
        let text = v.to_string().to_lowercase();
        for word in [
            "token\":\"",
            "secret",
            "password",
            "credential",
            "api_key",
            "bearer",
        ] {
            assert!(!text.contains(word), "response mentions {word}: {text}");
        }
        // Only aggregates: no objective, title or staging path.
        for word in ["objective", "/tmp/w", "/tmp/s"] {
            assert!(!text.contains(word), "response leaks {word}");
        }
    }

    #[test]
    fn the_route_is_a_read_and_is_documented() {
        use crate::human_gate::{classify, RouteClass};
        assert_eq!(
            classify(&axum::http::Method::GET, "/api/spend"),
            RouteClass::Safe
        );
        assert_ne!(
            classify(&axum::http::Method::POST, "/api/spend"),
            RouteClass::Safe
        );
        let usage = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/USAGE.md"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        assert!(
            usage.contains("GET /api/spend"),
            "docs/USAGE.md must document GET /api/spend"
        );
    }

    /// Nothing an agent can call accepts an origin class: the only daemon file
    /// that may mention it is this read-only endpoint (and the goal-record
    /// types it reads). Scans CRLF-normalized sources.
    #[test]
    fn no_other_daemon_source_reads_or_accepts_origin_class() {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let offenders: Vec<String> = files
            .into_iter()
            .filter(|p| !p.ends_with("api/spend.rs"))
            .filter(|p| {
                std::fs::read_to_string(p)
                    .unwrap()
                    .replace("\r\n", "\n")
                    .to_ascii_lowercase()
                    .contains("origin_class")
            })
            .map(|p| p.display().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "origin_class is accounting data set by the launcher; these daemon files mention it: {offenders:?}"
        );
    }
}
