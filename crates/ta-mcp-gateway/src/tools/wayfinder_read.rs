//! `ta_wayfinder_*`: the six read-only Wayfinder tools of the Chief-of-Staff
//! chat mode (`task_list`, `task_get`, `goal_list`, `kpi_metric_list`,
//! `kpi_summary`, `kpi_list`).
//!
//! Owner decision (Wayfinder DEC-9): the CoS stays READ-ONLY. This module has
//! no write path at all: every call is a single `GET`. Work requests (add a
//! task, goal, KPI or datapoint) go out through the `delegate` outcome to a
//! project-manager worker, never through a tool here.
//!
//! ## Project pin (security-load-bearing)
//!
//! The project a call reads is fixed by the server, never by the model:
//!
//! * the pin is `[plan.wayfinder].project_id` in the gateway workspace's
//!   `.ta/workflow.toml` (the same place `ta-plan-wayfinder` and the draft
//!   replay get the base URL, org and credential from);
//! * if the chat wake context also names a project
//!   (`TA_CHAT_WAYFINDER_PROJECT_ID`) it must AGREE with that config, or every
//!   call is refused naming both ids (fail closed, no guessing which wins);
//! * a tool call may carry a `project_id` argument only so a wrong one can be
//!   rejected: if it differs from the pin the call fails with an error naming
//!   both ids, and no request is sent.
//!
//! Two projects served by two gateways therefore cannot read each other's data.
//!
//! The service-account token is resolved from the credential vault and held in
//! a redacting type; it is only ever placed in the `Authorization` header and is
//! never logged or echoed in an error.
//!
//! ## Response shaping
//!
//! Compact JSON with only the fields the CoS needs. Every list is capped at
//! [`MAX_LIST_ITEMS`] and carries `total`, `returned` and `truncated`, so a
//! cut is never silent. Free text is length-capped.

use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use ta_plan_wayfinder::{WayfinderClient, WayfinderPlanConfig, WayfinderReadError};

use crate::server::GatewayState;

/// Hard cap on items in any list response.
pub const MAX_LIST_ITEMS: usize = 100;
/// Items returned when the caller passes no `limit`.
pub const DEFAULT_LIST_LIMIT: usize = 50;
/// Default and maximum for `kpi_summary`'s `limit` (Wayfinder's own bounds).
const DEFAULT_SUMMARY_LIMIT: usize = 25;
const MAX_ID_LEN: usize = 128;
const MAX_FILTER_LEN: usize = 100;
const DESCRIPTION_CAP: usize = 4000;
const SHORT_TEXT_CAP: usize = 500;

pub const TASK_LIST: &str = "ta_wayfinder_task_list";
pub const TASK_GET: &str = "ta_wayfinder_task_get";
pub const GOAL_LIST: &str = "ta_wayfinder_goal_list";
pub const KPI_METRIC_LIST: &str = "ta_wayfinder_kpi_metric_list";
pub const KPI_SUMMARY: &str = "ta_wayfinder_kpi_summary";
pub const KPI_LIST: &str = "ta_wayfinder_kpi_list";

// ── Params ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TaskListParams {
    /// Do NOT set. The project is fixed by this chat session; a value that
    /// differs from it is rejected.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Optional status filter: open, in_progress, done, cancelled or on_hold.
    #[serde(default)]
    pub status: Option<String>,
    /// Optional case-insensitive substring of the task title.
    #[serde(default)]
    pub title_contains: Option<String>,
    /// Max items, 1 to 100 (default 50). Newest-updated first.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TaskGetParams {
    /// Do NOT set. The project is fixed by this chat session.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The Wayfinder task id (from `ta_wayfinder_task_list`).
    pub task_id: String,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct GoalListParams {
    /// Do NOT set. The project is fixed by this chat session.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Optional status filter: active, completed or archived.
    #[serde(default)]
    pub status: Option<String>,
    /// Optional case-insensitive substring of the goal name.
    #[serde(default)]
    pub name_contains: Option<String>,
    /// Max items, 1 to 100 (default 50). Newest-updated first.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct KpiMetricListParams {
    /// Do NOT set. The project is fixed by this chat session.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The goal whose KPI metrics to list (from `ta_wayfinder_goal_list`).
    /// Wayfinder's REST API lists metrics per goal; for every metric in the
    /// project at once use `ta_wayfinder_kpi_summary`.
    pub goal_id: String,
    /// Max items, 1 to 100 (default 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct KpiSummaryParams {
    /// Do NOT set. The project is fixed by this chat session.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Max metrics, 1 to 100 (default 25). Worst pace first.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct KpiListParams {
    /// Do NOT set. The project is fixed by this chat session.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Max items, 1 to 100 (default 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

// ── Pin and context ─────────────────────────────────────────────────────

/// The project a call is pinned to: the configured project, which the wake
/// context (when it names one) must agree with.
fn resolve_pin(tool: &str, configured: &str, wake: Option<&str>) -> Result<String, McpError> {
    match wake {
        Some(w) if w != configured => Err(McpError::invalid_request(
            format!(
                "{tool}: the chat wake context pins this session to Wayfinder project '{w}' \
                 (TA_CHAT_WAYFINDER_PROJECT_ID) but .ta/workflow.toml [plan.wayfinder] \
                 project_id is '{configured}'. Refusing to read either: the two must agree. \
                 Fix the config or re-launch the chat session for the right project."
            ),
            None,
        )),
        _ => Ok(configured.to_string()),
    }
}

/// Rejects a caller-supplied `project_id` that differs from the pin.
fn check_requested(tool: &str, pin: &str, requested: Option<&str>) -> Result<(), McpError> {
    match requested {
        Some(r) if r != pin => Err(McpError::invalid_params(
            format!(
                "{tool}: project_id '{r}' does not match the project this chat session is \
                 pinned to ('{pin}'). A chat session reads exactly one Wayfinder project and \
                 the project is not a tool argument: omit project_id (or pass '{pin}')."
            ),
            None,
        )),
        _ => Ok(()),
    }
}

struct ReadContext {
    config: WayfinderPlanConfig,
    project_id: String,
}

impl ReadContext {
    /// Builds the blocking client, runs `f` and drops the client, all on a
    /// dedicated thread: the blocking HTTP client owns a runtime that panics
    /// if created or dropped on a tokio worker, and tool handlers run on one.
    /// Wayfinder errors are mapped to MCP errors there too.
    fn read<T: Send + 'static>(
        self,
        tool: &'static str,
        f: impl FnOnce(&WayfinderClient) -> Result<T, WayfinderReadError> + Send + 'static,
    ) -> Result<T, McpError> {
        let ReadContext { config, project_id } = self;
        std::thread::spawn(move || {
            let client = WayfinderClient::new(&config).map_err(|e| {
                McpError::internal_error(
                    format!("{tool}: could not build the HTTP client: {e:#}"),
                    None,
                )
            })?;
            f(&client).map_err(|e| map_read_error(tool, &project_id, e))
        })
        .join()
        .map_err(|_| McpError::internal_error(format!("{tool}: worker thread panicked"), None))?
    }
}

fn load_context(
    state: &Arc<Mutex<GatewayState>>,
    tool: &str,
    requested: Option<&str>,
) -> Result<ReadContext, McpError> {
    let (workspace_root, use_keychain, wake_pin) = {
        let locked = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("{tool}: lock poisoned: {e}"), None))?;
        (
            locked.config.workspace_root.clone(),
            locked.config.credential_vault_use_keychain,
            locked.chat_wayfinder_project_id.clone(),
        )
    };

    let workflow_toml = workspace_root.join(".ta").join("workflow.toml");
    if !workflow_toml.is_file() {
        return Err(McpError::invalid_request(
            format!(
                "{tool}: no Wayfinder configuration: {} does not exist. Add a [plan.wayfinder] \
                 table (base_url, org_id, project_id, credential_name) to it.",
                workflow_toml.display()
            ),
            None,
        ));
    }
    let workflow = ta_submit::WorkflowConfig::load(&workflow_toml).map_err(|e| {
        McpError::invalid_request(
            format!(
                "{tool}: could not parse {}: {e}. Fix the TOML and retry.",
                workflow_toml.display()
            ),
            None,
        )
    })?;
    let raw = workflow.plan.wayfinder.as_ref().ok_or_else(|| {
        McpError::invalid_request(
            format!(
                "{tool}: {} has no [plan.wayfinder] table. Add base_url, org_id, project_id and \
                 credential_name to read Wayfinder.",
                workflow_toml.display()
            ),
            None,
        )
    })?;

    let pin = resolve_pin(tool, &raw.project_id, wake_pin.as_deref())?;
    check_requested(tool, &pin, requested)?;

    let mut cred_config = ta_credentials::CredentialsConfig::for_project(&workspace_root);
    cred_config.use_keychain = use_keychain;
    let mut config = WayfinderPlanConfig::load_with_credentials_config(raw, &cred_config)
        .map_err(|e| McpError::invalid_request(format!("{tool}: {e:#}"), None))?;
    config.project_id = pin.clone();
    Ok(ReadContext {
        config,
        project_id: pin,
    })
}

// ── Plumbing ────────────────────────────────────────────────────────────

fn map_read_error(tool: &str, project_id: &str, err: WayfinderReadError) -> McpError {
    tracing::warn!(tool, project_id, error = %err, "Wayfinder read failed");
    let message = format!("{tool}: {err}");
    match err {
        WayfinderReadError::NotFound { .. } => McpError::invalid_params(message, None),
        _ => McpError::internal_error(message, None),
    }
}

fn success_json(value: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::json(value)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn bad_param(tool: &str, msg: String) -> McpError {
    McpError::invalid_params(format!("{tool}: {msg}"), None)
}

fn validate_id(tool: &str, field: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() || value.chars().count() > MAX_ID_LEN {
        return Err(bad_param(
            tool,
            format!("{field} must be 1 to {MAX_ID_LEN} characters. List first to get a valid id."),
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(bad_param(
            tool,
            format!("{field} contains a control character."),
        ));
    }
    Ok(())
}

fn validate_filter(tool: &str, field: &str, value: Option<&str>) -> Result<(), McpError> {
    if let Some(v) = value {
        if v.chars().count() > MAX_FILTER_LEN || v.chars().any(|c| c.is_control()) {
            return Err(bad_param(
                tool,
                format!(
                    "{field} must be at most {MAX_FILTER_LEN} characters with no control \
                     characters."
                ),
            ));
        }
    }
    Ok(())
}

fn effective_limit(tool: &str, limit: Option<usize>, default: usize) -> Result<usize, McpError> {
    match limit {
        None => Ok(default),
        Some(n) if (1..=MAX_LIST_ITEMS).contains(&n) => Ok(n),
        Some(n) => Err(bad_param(
            tool,
            format!(
                "limit {n} is out of range: use 1 to {MAX_LIST_ITEMS}, or omit it for {default}."
            ),
        )),
    }
}

fn as_array<'a>(tool: &str, route: &str, v: &'a Value) -> Result<&'a Vec<Value>, McpError> {
    v.as_array().ok_or_else(|| {
        McpError::internal_error(
            format!(
                "{tool}: Wayfinder's response for {route} was not a JSON array as expected. The \
                 Wayfinder server may be a different version than this TA build expects."
            ),
            None,
        )
    })
}

/// Copies the listed keys that are present, truncating long strings.
fn pick(v: &Value, keys: &[&str], text_cap: usize) -> Value {
    let mut out = Map::new();
    for key in keys {
        if let Some(val) = v.get(*key) {
            out.insert((*key).to_string(), cap_text(val, text_cap));
        }
    }
    Value::Object(out)
}

fn cap_text(v: &Value, cap: usize) -> Value {
    match v.as_str() {
        Some(s) if s.chars().count() > cap => {
            let mut t: String = s.chars().take(cap).collect();
            t.push_str("...[truncated]");
            Value::String(t)
        }
        _ => v.clone(),
    }
}

/// `updated_at` is an epoch-seconds decimal string; newest first, ties by id.
fn updated_key(v: &Value) -> (std::cmp::Reverse<u64>, String) {
    let ts = v
        .get("updated_at")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    (std::cmp::Reverse(ts), id)
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Wraps a capped list: `{project_id, total, returned, truncated, <key>: [...]}`.
fn list_envelope(project_id: &str, key: &str, mut items: Vec<Value>, limit: usize) -> Value {
    let total = items.len();
    items.truncate(limit);
    let returned = items.len();
    let mut out = Map::new();
    out.insert("project_id".into(), json!(project_id));
    out.insert("total".into(), json!(total));
    out.insert("returned".into(), json!(returned));
    out.insert("truncated".into(), json!(total > returned));
    out.insert(key.into(), Value::Array(items));
    Value::Object(out)
}

const TASK_LIST_FIELDS: &[&str] = &[
    "id",
    "title",
    "verb",
    "status",
    "assignee_id",
    "target_date",
    "hold_reason",
    "external_id",
    "updated_at",
];
const TASK_GET_FIELDS: &[&str] = &[
    "id",
    "title",
    "description",
    "verb",
    "status",
    "assignee_id",
    "requested_by",
    "target_date",
    "hold_reason",
    "completed_at",
    "external_id",
    "created_at",
    "updated_at",
];
const TASK_REF_FIELDS: &[&str] = &["id", "title", "status"];

// ── ta_wayfinder_task_list ──────────────────────────────────────────────

pub fn handle_task_list(
    state: &Arc<Mutex<GatewayState>>,
    params: TaskListParams,
) -> Result<CallToolResult, McpError> {
    validate_filter(TASK_LIST, "status", params.status.as_deref())?;
    validate_filter(
        TASK_LIST,
        "title_contains",
        params.title_contains.as_deref(),
    )?;
    let limit = effective_limit(TASK_LIST, params.limit, DEFAULT_LIST_LIMIT)?;
    let ctx = load_context(state, TASK_LIST, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();

    let raw = ctx.read(TASK_LIST, move |c| c.get_project_json(&["tasks"], &[]))?;
    let route = format!("/api/projects/{project_id}/tasks");

    let mut tasks: Vec<&Value> = as_array(TASK_LIST, &route, &raw)?
        .iter()
        .filter(|t| {
            params
                .status
                .as_deref()
                .is_none_or(|s| str_field(t, "status") == s)
                && params
                    .title_contains
                    .as_deref()
                    .is_none_or(|q| contains_ci(str_field(t, "title"), q))
        })
        .collect();
    tasks.sort_by_key(|t| updated_key(t));
    let items = tasks
        .into_iter()
        .map(|t| pick(t, TASK_LIST_FIELDS, SHORT_TEXT_CAP))
        .collect();
    success_json(list_envelope(&project_id, "tasks", items, limit))
}

// ── ta_wayfinder_task_get ───────────────────────────────────────────────

pub fn handle_task_get(
    state: &Arc<Mutex<GatewayState>>,
    params: TaskGetParams,
) -> Result<CallToolResult, McpError> {
    validate_id(TASK_GET, "task_id", &params.task_id)?;
    let ctx = load_context(state, TASK_GET, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();
    let task_id = params.task_id.clone();

    let (task, deps) = ctx.read(TASK_GET, move |c| {
        let task = c.get_project_json(&["tasks", &task_id], &[])?;
        let deps = c.get_project_json(&["tasks", &task_id, "dependencies"], &[])?;
        Ok((task, deps))
    })?;

    let refs = |key: &str| -> Value {
        let all = deps.get(key).and_then(|v| v.as_array());
        let all = all.map(|a| a.as_slice()).unwrap_or(&[]);
        let items: Vec<Value> = all
            .iter()
            .take(MAX_LIST_ITEMS)
            .map(|t| pick(t, TASK_REF_FIELDS, SHORT_TEXT_CAP))
            .collect();
        json!({ "total": all.len(), "truncated": all.len() > items.len(), "items": items })
    };
    success_json(json!({
        "project_id": project_id,
        "task": pick(&task, TASK_GET_FIELDS, DESCRIPTION_CAP),
        "depends_on": refs("depends_on"),
        "dependents": refs("dependents"),
    }))
}

// ── ta_wayfinder_goal_list ──────────────────────────────────────────────

/// Flattens Wayfinder's nested goal tree (`{goal, tasks, children}`) into
/// compact goal rows; also accepts a flat list of goals.
fn flatten_goals(nodes: &[Value], out: &mut Vec<Value>) {
    for node in nodes {
        let goal = node.get("goal").unwrap_or(node);
        let mut row = pick(
            goal,
            &[
                "id",
                "name",
                "description",
                "kind",
                "parent_goal_id",
                "target_date",
                "status",
                "updated_at",
            ],
            SHORT_TEXT_CAP,
        );
        if let (Some(obj), Some(tasks)) = (
            row.as_object_mut(),
            node.get("tasks").and_then(|t| t.as_array()),
        ) {
            obj.insert("task_count".into(), json!(tasks.len()));
        }
        out.push(row);
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            flatten_goals(children, out);
        }
    }
}

pub fn handle_goal_list(
    state: &Arc<Mutex<GatewayState>>,
    params: GoalListParams,
) -> Result<CallToolResult, McpError> {
    validate_filter(GOAL_LIST, "status", params.status.as_deref())?;
    validate_filter(GOAL_LIST, "name_contains", params.name_contains.as_deref())?;
    let limit = effective_limit(GOAL_LIST, params.limit, DEFAULT_LIST_LIMIT)?;
    let ctx = load_context(state, GOAL_LIST, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();

    let raw = ctx.read(GOAL_LIST, move |c| c.get_project_json(&["goals"], &[]))?;
    let route = format!("/api/projects/{project_id}/goals");

    let mut flat = Vec::new();
    flatten_goals(as_array(GOAL_LIST, &route, &raw)?, &mut flat);
    let mut goals: Vec<Value> = flat
        .into_iter()
        .filter(|g| {
            params
                .status
                .as_deref()
                .is_none_or(|s| str_field(g, "status") == s)
                && params
                    .name_contains
                    .as_deref()
                    .is_none_or(|q| contains_ci(str_field(g, "name"), q))
        })
        .collect();
    goals.sort_by_key(updated_key);
    success_json(list_envelope(&project_id, "goals", goals, limit))
}

// ── ta_wayfinder_kpi_metric_list ────────────────────────────────────────

pub fn handle_kpi_metric_list(
    state: &Arc<Mutex<GatewayState>>,
    params: KpiMetricListParams,
) -> Result<CallToolResult, McpError> {
    validate_id(KPI_METRIC_LIST, "goal_id", &params.goal_id)?;
    let limit = effective_limit(KPI_METRIC_LIST, params.limit, DEFAULT_LIST_LIMIT)?;
    let ctx = load_context(state, KPI_METRIC_LIST, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();
    let goal_id = params.goal_id.clone();

    let raw = ctx.read(KPI_METRIC_LIST, move |c| {
        c.get_project_json(&["goals", &goal_id, "kpi-metrics"], &[])
    })?;
    let route = format!(
        "/api/projects/{project_id}/goals/{}/kpi-metrics",
        params.goal_id
    );

    let items = as_array(KPI_METRIC_LIST, &route, &raw)?
        .iter()
        .map(|m| {
            pick(
                m,
                &[
                    "id",
                    "goal_id",
                    "label",
                    "unit",
                    "target",
                    "cadence",
                    "kind",
                    "aggregation",
                    "actual_so_far",
                    "expected_so_far",
                    "pace_ratio",
                    "tone",
                ],
                SHORT_TEXT_CAP,
            )
        })
        .collect();
    let mut out = list_envelope(&project_id, "metrics", items, limit);
    out["goal_id"] = json!(params.goal_id);
    success_json(out)
}

// ── ta_wayfinder_kpi_summary ────────────────────────────────────────────

pub fn handle_kpi_summary(
    state: &Arc<Mutex<GatewayState>>,
    params: KpiSummaryParams,
) -> Result<CallToolResult, McpError> {
    let limit = effective_limit(KPI_SUMMARY, params.limit, DEFAULT_SUMMARY_LIMIT)?;
    let ctx = load_context(state, KPI_SUMMARY, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();
    let limit_str = limit.to_string();

    let raw = ctx.read(KPI_SUMMARY, move |c| {
        c.get_project_json(&["kpi-summary"], &[("limit", limit_str.as_str())])
    })?;

    let metrics: Vec<Value> = raw
        .get("metrics")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .take(MAX_LIST_ITEMS)
                .map(|m| {
                    pick(
                        m,
                        &[
                            "metric_id",
                            "goal_id",
                            "goal_name",
                            "label",
                            "unit",
                            "target",
                            "cadence",
                            "kind",
                            "actual_so_far",
                            "expected_so_far",
                            "pace_ratio",
                            "tone",
                        ],
                        SHORT_TEXT_CAP,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let metric_count = raw
        .get("metric_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(metrics.len() as u64);
    let returned = metrics.len() as u64;
    success_json(json!({
        "project_id": project_id,
        "as_of": raw.get("as_of").cloned().unwrap_or(Value::Null),
        "metric_count": metric_count,
        "by_tone": raw.get("by_tone").cloned().unwrap_or(Value::Null),
        "returned": returned,
        // Wayfinder reports its own truncation; ours (the 100 cap) adds to it.
        "truncated": raw.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false)
            || metric_count > returned,
        "metrics": metrics,
    }))
}

// ── ta_wayfinder_kpi_list ───────────────────────────────────────────────

pub fn handle_kpi_list(
    state: &Arc<Mutex<GatewayState>>,
    params: KpiListParams,
) -> Result<CallToolResult, McpError> {
    let limit = effective_limit(KPI_LIST, params.limit, DEFAULT_LIST_LIMIT)?;
    let ctx = load_context(state, KPI_LIST, params.project_id.as_deref())?;
    let project_id = ctx.project_id.clone();
    let pin = project_id.clone();

    // The KPI catalogue has no project-scoped route; the org route takes a
    // `project_id` scope filter, which is the pin.
    let raw = ctx.read(KPI_LIST, move |c| {
        c.get_org_json(&["kpis"], &[("project_id", pin.as_str())])
    })?;
    let route = "/api/orgs/<org_id>/kpis";

    // Defence in depth: whatever the server returned, keep only org-wide KPIs
    // and this project's own, never another project's.
    let items = as_array(KPI_LIST, route, &raw)?
        .iter()
        .filter(|k| match k.get("project_id").and_then(|p| p.as_str()) {
            None => true,
            Some(p) => p == project_id,
        })
        .map(|k| {
            let mut row = pick(
                k,
                &["id", "label", "description", "weight", "created_at"],
                SHORT_TEXT_CAP,
            );
            row["scope"] = json!(if k.get("project_id").is_some_and(|p| p.is_string()) {
                "project"
            } else {
                "org"
            });
            row
        })
        .collect();
    success_json(list_envelope(&project_id, "kpis", items, limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::IntoResponse;
    use axum::Router;

    use crate::GatewayConfig;

    const TOKEN: &str = "wfsa_test_token_do_not_log";

    type RouteTable = HashMap<String, (u16, Value, u64)>;

    #[derive(Clone, Default)]
    struct Mock {
        /// path -> (status, body, delay_ms)
        routes: Arc<Mutex<RouteTable>>,
        /// every request seen: (path-and-query, authorization header)
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Mock {
        fn route(&self, path: &str, status: u16, body: Value) {
            self.routes
                .lock()
                .unwrap()
                .insert(path.to_string(), (status, body, 0));
        }
        fn paths(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.0.clone())
                .collect()
        }
    }

    async fn handler(State(mock): State<Mock>, uri: Uri, headers: HeaderMap) -> impl IntoResponse {
        let auth = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        let full = match uri.query() {
            Some(q) => format!("{}?{}", uri.path(), q),
            None => uri.path().to_string(),
        };
        mock.seen.lock().unwrap().push((full, auth));
        let hit = mock.routes.lock().unwrap().get(uri.path()).cloned();
        match hit {
            Some((status, body, delay)) => {
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    struct Fixture {
        state: Arc<Mutex<GatewayState>>,
        mock: Mock,
        dir: tempfile::TempDir,
    }

    /// A project dir whose `.ta/workflow.toml` points at a live in-process
    /// Wayfinder stand-in, with the token in a file vault (no keychain).
    async fn fixture_for(project_id: &str, wake_pin: Option<&str>) -> Fixture {
        use ta_credentials::CredentialVault;

        let mock = Mock::default();
        let app = Router::new().fallback(handler).with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            format!(
                "[plan]\nbackend = \"wayfinder\"\n\n[plan.wayfinder]\nbase_url = \"{url}\"\n\
                 org_id = \"org-1\"\nproject_id = \"{project_id}\"\n\
                 credential_name = \"wayfinder-cos\"\n"
            ),
        )
        .unwrap();
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-cos", "wayfinder", TOKEN, vec![])
            .unwrap();

        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        let mut gateway_state = GatewayState::new(config).unwrap();
        gateway_state.chat_wayfinder_project_id = wake_pin.map(str::to_string);
        Fixture {
            state: Arc::new(Mutex::new(gateway_state)),
            mock,
            dir,
        }
    }

    fn text_of(result: Result<CallToolResult, McpError>) -> String {
        match result {
            Ok(r) => r.content[0].raw.as_text().unwrap().text.clone(),
            Err(e) => panic!("expected success, got error: {}", e.message),
        }
    }

    fn json_of(result: Result<CallToolResult, McpError>) -> Value {
        serde_json::from_str(&text_of(result)).unwrap()
    }

    fn err_of(result: Result<CallToolResult, McpError>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.message.to_string(),
        }
    }

    fn task(id: &str, title: &str, status: &str, updated: u64) -> Value {
        json!({
            "id": id, "title": title, "description": "long body", "verb": "implement",
            "status": status, "assignee_id": null, "requested_by": "u1", "workload_hint": "x",
            "source": "mcp", "fs_ref": null, "target_date": null, "hold_reason": null,
            "completed_at": null, "external_id": null,
            "created_at": "1", "updated_at": updated.to_string(),
        })
    }

    // ── pure logic ──────────────────────────────────────────────────────

    #[test]
    fn pin_rejects_a_caller_project_that_differs_and_names_both_ids() {
        let err = check_requested("t", "proj-a", Some("proj-b")).unwrap_err();
        assert!(err.message.contains("proj-a") && err.message.contains("proj-b"));
        assert!(check_requested("t", "proj-a", Some("proj-a")).is_ok());
        assert!(check_requested("t", "proj-a", None).is_ok());
    }

    #[test]
    fn wake_pin_must_agree_with_config_and_the_error_names_both() {
        let err = resolve_pin("t", "proj-cfg", Some("proj-wake")).unwrap_err();
        assert!(err.message.contains("proj-cfg") && err.message.contains("proj-wake"));
        assert_eq!(resolve_pin("t", "p", Some("p")).unwrap(), "p");
        assert_eq!(resolve_pin("t", "p", None).unwrap(), "p");
    }

    #[test]
    fn limit_is_bounded_and_ids_are_validated() {
        assert_eq!(effective_limit("t", None, 50).unwrap(), 50);
        assert_eq!(effective_limit("t", Some(100), 50).unwrap(), 100);
        assert!(effective_limit("t", Some(0), 50).is_err());
        assert!(effective_limit("t", Some(101), 50).is_err());
        assert!(validate_id("t", "task_id", "").is_err());
        assert!(validate_id("t", "task_id", "a\nb").is_err());
        assert!(validate_id("t", "task_id", &"x".repeat(129)).is_err());
        assert!(validate_id("t", "task_id", "task-1").is_ok());
    }

    #[test]
    fn long_text_is_capped_with_a_marker() {
        let v = json!({"description": "x".repeat(600)});
        let out = pick(&v, &["description"], 500);
        let s = out["description"].as_str().unwrap();
        assert!(s.ends_with("...[truncated]"));
        assert_eq!(s.chars().count(), 500 + "...[truncated]".len());
    }

    // ── per-tool route, shaping, filtering ──────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn task_list_hits_the_project_route_with_bearer_auth_and_shapes_compactly() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route(
            "/api/projects/proj-1/tasks",
            200,
            json!([
                task("t1", "Fix login", "open", 100),
                task("t2", "Write docs", "done", 300),
                task("t3", "Fix logout", "open", 200),
            ]),
        );
        let out = json_of(handle_task_list(
            &f.state,
            TaskListParams {
                status: Some("open".into()),
                title_contains: Some("FIX".into()),
                ..Default::default()
            },
        ));
        assert_eq!(f.mock.paths(), vec!["/api/projects/proj-1/tasks"]);
        assert_eq!(f.mock.seen.lock().unwrap()[0].1, format!("Bearer {TOKEN}"));
        assert_eq!(out["project_id"], "proj-1");
        assert_eq!(out["total"], 2);
        assert_eq!(out["truncated"], false);
        // Newest updated first, and only compact fields.
        assert_eq!(out["tasks"][0]["id"], "t3");
        assert_eq!(out["tasks"][1]["id"], "t1");
        assert!(out["tasks"][0].get("description").is_none());
        assert!(out["tasks"][0].get("workload_hint").is_none());
        drop(f.dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn task_get_fetches_the_task_and_its_dependencies() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route(
            "/api/projects/proj-1/tasks/t1",
            200,
            task("t1", "A", "open", 5),
        );
        f.mock.route(
            "/api/projects/proj-1/tasks/t1/dependencies",
            200,
            json!({
                "depends_on": [task("t0", "Before", "done", 1)],
                "dependents": [task("t2", "After", "open", 9)],
            }),
        );
        let out = json_of(handle_task_get(
            &f.state,
            TaskGetParams {
                task_id: "t1".into(),
                ..Default::default()
            },
        ));
        assert_eq!(
            f.mock.paths(),
            vec![
                "/api/projects/proj-1/tasks/t1",
                "/api/projects/proj-1/tasks/t1/dependencies"
            ]
        );
        assert_eq!(out["task"]["title"], "A");
        assert_eq!(out["task"]["description"], "long body");
        assert_eq!(out["depends_on"]["items"][0]["id"], "t0");
        assert_eq!(out["dependents"]["items"][0]["status"], "open");
        assert!(out["task"].get("workload_hint").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn goal_list_flattens_the_tree_and_filters() {
        let f = fixture_for("proj-1", None).await;
        let goal = |id: &str, name: &str, status: &str, up: u64| {
            json!({"id": id, "name": name, "description": null, "kind": "milestone",
                   "parent_goal_id": null, "target_date": null, "status": status,
                   "external_id": null, "created_at": "1", "updated_at": up.to_string()})
        };
        f.mock.route(
            "/api/projects/proj-1/goals",
            200,
            json!([{
                "goal": goal("g1", "Launch", "active", 10),
                "tasks": [task("t1", "x", "open", 1), task("t2", "y", "open", 1)],
                "children": [{"goal": goal("g2", "Launch beta", "completed", 20),
                              "tasks": [], "children": []}]
            }]),
        );
        let all = json_of(handle_goal_list(&f.state, GoalListParams::default()));
        assert_eq!(f.mock.paths(), vec!["/api/projects/proj-1/goals"]);
        assert_eq!(all["total"], 2);
        assert_eq!(all["goals"][0]["id"], "g2");
        let g1 = all["goals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == "g1")
            .unwrap();
        assert_eq!(g1["task_count"], 2);

        let active = json_of(handle_goal_list(
            &f.state,
            GoalListParams {
                status: Some("active".into()),
                name_contains: Some("launch".into()),
                ..Default::default()
            },
        ));
        assert_eq!(active["total"], 1);
        assert_eq!(active["goals"][0]["id"], "g1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kpi_metric_list_uses_the_goal_route_and_keeps_progress_fields() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route(
            "/api/projects/proj-1/goals/g1/kpi-metrics",
            200,
            json!([{"id": "m1", "goal_id": "g1", "label": "Signups", "unit": "users",
                    "target": 100.0, "cadence": "weekly", "kind": "external",
                    "aggregation": "sum", "require_gate": false, "weight": 1.0,
                    "created_at": "1", "actual_so_far": 40.0, "expected_so_far": 50.0,
                    "pace_ratio": 0.8, "tone": "warn"}]),
        );
        let out = json_of(handle_kpi_metric_list(
            &f.state,
            KpiMetricListParams {
                goal_id: "g1".into(),
                ..Default::default()
            },
        ));
        assert_eq!(
            f.mock.paths(),
            vec!["/api/projects/proj-1/goals/g1/kpi-metrics"]
        );
        assert_eq!(out["goal_id"], "g1");
        assert_eq!(out["metrics"][0]["tone"], "warn");
        assert_eq!(out["metrics"][0]["pace_ratio"], 0.8);
        assert!(out["metrics"][0].get("require_gate").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kpi_summary_passes_limit_and_reports_truncation() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route(
            "/api/projects/proj-1/kpi-summary",
            200,
            json!({"project_id": "proj-1", "as_of": "99", "metric_count": 5,
                   "by_tone": {"good": 3, "warn": 1, "crit": 1}, "returned": 1,
                   "truncated": true,
                   "metrics": [{"metric_id": "m1", "goal_id": "g1", "goal_name": "G",
                                "label": "L", "unit": "u", "target": 1.0, "cadence": "none",
                                "kind": "external", "actual_so_far": 0.0,
                                "expected_so_far": 1.0, "pace_ratio": 0.0, "tone": "crit"}]}),
        );
        let out = json_of(handle_kpi_summary(
            &f.state,
            KpiSummaryParams {
                limit: Some(1),
                ..Default::default()
            },
        ));
        assert_eq!(
            f.mock.paths(),
            vec!["/api/projects/proj-1/kpi-summary?limit=1"]
        );
        assert_eq!(out["metric_count"], 5);
        assert_eq!(out["truncated"], true);
        assert_eq!(out["by_tone"]["crit"], 1);
        assert_eq!(out["metrics"][0]["tone"], "crit");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kpi_list_uses_the_org_route_scoped_and_drops_other_projects() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route(
            "/api/orgs/org-1/kpis",
            200,
            json!([
                {"id": "k1", "org_id": "org-1", "project_id": null, "label": "Org KPI",
                 "description": "d", "weight": 1.0, "created_by": "human", "created_at": "1"},
                {"id": "k2", "org_id": "org-1", "project_id": "proj-1", "label": "Mine",
                 "description": "d", "weight": 2.0, "created_by": "human", "created_at": "2"},
                {"id": "k3", "org_id": "org-1", "project_id": "proj-OTHER", "label": "Leak",
                 "description": "d", "weight": 1.0, "created_by": "human", "created_at": "3"},
            ]),
        );
        let out = json_of(handle_kpi_list(&f.state, KpiListParams::default()));
        assert_eq!(
            f.mock.paths(),
            vec!["/api/orgs/org-1/kpis?project_id=proj-1"]
        );
        assert_eq!(out["total"], 2);
        let ids: Vec<&str> = out["kpis"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["k1", "k2"]);
        assert_eq!(out["kpis"][0]["scope"], "org");
        assert_eq!(out["kpis"][1]["scope"], "project");
        assert!(!text_of_value(&out).contains("proj-OTHER"));
    }

    fn text_of_value(v: &Value) -> String {
        v.to_string()
    }

    // ── truncation ──────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_are_capped_at_100_with_a_truncated_flag() {
        let f = fixture_for("proj-1", None).await;
        let many: Vec<Value> = (0..150)
            .map(|i| task(&format!("t{i}"), "T", "open", i))
            .collect();
        f.mock.route("/api/projects/proj-1/tasks", 200, json!(many));

        let capped = json_of(handle_task_list(
            &f.state,
            TaskListParams {
                limit: Some(100),
                ..Default::default()
            },
        ));
        assert_eq!(capped["total"], 150);
        assert_eq!(capped["returned"], 100);
        assert_eq!(capped["truncated"], true);
        assert_eq!(capped["tasks"].as_array().unwrap().len(), 100);

        let over = err_of(handle_task_list(
            &f.state,
            TaskListParams {
                limit: Some(101),
                ..Default::default()
            },
        ));
        assert!(over.contains("out of range"), "{over}");

        let default = json_of(handle_task_list(&f.state, TaskListParams::default()));
        assert_eq!(default["returned"], 50);
        assert_eq!(default["truncated"], true);
    }

    // ── project pin ─────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mismatched_project_id_is_rejected_before_any_request_is_sent() {
        let f = fixture_for("proj-1", None).await;
        let err = err_of(handle_task_list(
            &f.state,
            TaskListParams {
                project_id: Some("proj-2".into()),
                ..Default::default()
            },
        ));
        assert!(err.contains("proj-1") && err.contains("proj-2"), "{err}");
        assert!(f.mock.paths().is_empty(), "no request may be sent");

        // The same check guards every tool.
        for r in [
            handle_task_get(
                &f.state,
                TaskGetParams {
                    project_id: Some("proj-2".into()),
                    task_id: "t".into(),
                },
            ),
            handle_goal_list(
                &f.state,
                GoalListParams {
                    project_id: Some("proj-2".into()),
                    ..Default::default()
                },
            ),
            handle_kpi_metric_list(
                &f.state,
                KpiMetricListParams {
                    project_id: Some("proj-2".into()),
                    goal_id: "g".into(),
                    limit: None,
                },
            ),
            handle_kpi_summary(
                &f.state,
                KpiSummaryParams {
                    project_id: Some("proj-2".into()),
                    limit: None,
                },
            ),
            handle_kpi_list(
                &f.state,
                KpiListParams {
                    project_id: Some("proj-2".into()),
                    limit: None,
                },
            ),
        ] {
            let e = err_of(r);
            assert!(e.contains("proj-1") && e.contains("proj-2"), "{e}");
        }
        assert!(f.mock.paths().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_matching_project_id_argument_is_accepted() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route("/api/projects/proj-1/tasks", 200, json!([]));
        let out = json_of(handle_task_list(
            &f.state,
            TaskListParams {
                project_id: Some("proj-1".into()),
                ..Default::default()
            },
        ));
        assert_eq!(out["total"], 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wake_pin_that_disagrees_with_the_config_refuses_every_read() {
        let f = fixture_for("proj-cfg", Some("proj-wake")).await;
        let err = err_of(handle_task_list(&f.state, TaskListParams::default()));
        assert!(
            err.contains("proj-cfg") && err.contains("proj-wake"),
            "{err}"
        );
        assert!(f.mock.paths().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_gateways_for_two_projects_never_read_each_others_data() {
        let a = fixture_for("proj-a", Some("proj-a")).await;
        let b = fixture_for("proj-b", Some("proj-b")).await;
        a.mock.route(
            "/api/projects/proj-a/tasks",
            200,
            json!([task("ta", "A", "open", 1)]),
        );
        b.mock.route(
            "/api/projects/proj-b/tasks",
            200,
            json!([task("tb", "B", "open", 1)]),
        );
        let ra = json_of(handle_task_list(&a.state, TaskListParams::default()));
        let rb = json_of(handle_task_list(&b.state, TaskListParams::default()));
        assert_eq!(ra["tasks"][0]["id"], "ta");
        assert_eq!(rb["tasks"][0]["id"], "tb");
        assert_eq!(a.mock.paths(), vec!["/api/projects/proj-a/tasks"]);
        assert_eq!(b.mock.paths(), vec!["/api/projects/proj-b/tasks"]);
        // Asking gateway A for B is refused outright.
        let err = err_of(handle_task_list(
            &a.state,
            TaskListParams {
                project_id: Some("proj-b".into()),
                ..Default::default()
            },
        ));
        assert!(err.contains("proj-a") && err.contains("proj-b"));
        assert_eq!(a.mock.paths().len(), 1);
    }

    // ── errors ──────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn errors_keep_401_403_404_and_server_faults_distinct_and_never_leak_the_token() {
        let f = fixture_for("proj-1", None).await;
        f.mock.route("/api/projects/proj-1/tasks", 401, json!({}));
        f.mock.route("/api/projects/proj-1/goals", 403, json!({}));
        f.mock
            .route("/api/projects/proj-1/tasks/nope", 404, json!({}));
        f.mock
            .route("/api/projects/proj-1/kpi-summary", 500, json!({}));

        let e401 = err_of(handle_task_list(&f.state, TaskListParams::default()));
        assert!(
            e401.contains("401") && e401.contains("/api/projects/proj-1/tasks"),
            "{e401}"
        );
        assert!(e401.contains("ta credential add"), "{e401}");

        let e403 = err_of(handle_goal_list(&f.state, GoalListParams::default()));
        assert!(
            e403.contains("403") && e403.contains("project:read"),
            "{e403}"
        );

        let e404 = err_of(handle_task_get(
            &f.state,
            TaskGetParams {
                task_id: "nope".into(),
                ..Default::default()
            },
        ));
        assert!(
            e404.contains("404") && e404.contains("/tasks/nope"),
            "{e404}"
        );
        assert!(e404.contains("List first"), "{e404}");

        let e500 = err_of(handle_kpi_summary(&f.state, KpiSummaryParams::default()));
        assert!(e500.contains("500"), "{e500}");

        for e in [&e401, &e403, &e404, &e500] {
            assert!(!e.contains(TOKEN), "token leaked: {e}");
        }
        assert!(e401 != e403 && e403 != e404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_wayfinder_surfaces_as_a_timeout_not_a_hang() {
        // Drive the real client with a short timeout against a delayed route
        // (the 20s production timeout is covered by the client's own test).
        let f = fixture_for("proj-1", None).await;
        f.mock
            .routes
            .lock()
            .unwrap()
            .insert("/api/projects/proj-1/tasks".into(), (200, json!([]), 1500));
        let raw_cfg = {
            let toml = std::fs::read_to_string(f.dir.path().join(".ta/workflow.toml")).unwrap();
            let wf: ta_submit::WorkflowConfig = toml::from_str(&toml).unwrap();
            wf.plan.wayfinder.unwrap()
        };
        let mut cred = ta_credentials::CredentialsConfig::for_project(f.dir.path());
        cred.use_keychain = false;
        let cfg = WayfinderPlanConfig::load_with_credentials_config(&raw_cfg, &cred).unwrap();
        let err = std::thread::spawn(move || {
            let client =
                WayfinderClient::new_with_timeout(&cfg, std::time::Duration::from_millis(200))
                    .unwrap();
            client.get_project_json(&["tasks"], &[]).unwrap_err()
        })
        .join()
        .unwrap();
        let mapped = map_read_error("ta_wayfinder_task_list", "proj-1", err);
        assert!(mapped.message.contains("timed out"), "{}", mapped.message);
        assert!(mapped.message.contains("/api/projects/proj-1/tasks"));
    }

    #[test]
    fn missing_configuration_is_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        let state = Arc::new(Mutex::new(GatewayState::new(config).unwrap()));
        let err = err_of(handle_task_list(&state, TaskListParams::default()));
        assert!(
            err.contains("workflow.toml") && err.contains("[plan.wayfinder]"),
            "{err}"
        );

        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[plan]\nbackend = \"file\"\n",
        )
        .unwrap();
        let err = err_of(handle_task_list(&state, TaskListParams::default()));
        assert!(err.contains("no [plan.wayfinder] table"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_credential_names_the_fix() {
        let f = fixture_for("proj-1", None).await;
        // Point the config at a credential that was never stored.
        let p = f.dir.path().join(".ta/workflow.toml");
        let t = std::fs::read_to_string(&p)
            .unwrap()
            .replace("wayfinder-cos", "nope");
        std::fs::write(&p, t).unwrap();
        let err = err_of(handle_task_list(&f.state, TaskListParams::default()));
        assert!(err.contains("no credential named 'nope'"), "{err}");
        assert!(err.contains("ta credential add"), "{err}");
    }
}
