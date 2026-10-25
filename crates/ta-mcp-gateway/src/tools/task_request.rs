//! `ta_request_task` / `ta_request_status`: the task intake funnel, TA side.
//!
//! A goal agent that discovers it needs a task created, or a KPI datapoint
//! recorded, in Wayfinder cannot act on Wayfinder itself. It asks through
//! `ta_request_task`; the request is appended to the project's intake queue
//! (`.ta/task-intake/queue.jsonl`, see the `ta-task-intake` crate) and a later
//! stage (the project-manager worker) turns it into a draft a human reviews.
//! **Nothing in this module reaches Wayfinder.**
//!
//! # Identity comes from the gateway, never from arguments
//!
//! The tools take no goal, role or project argument, and
//! `deny_unknown_fields` makes a caller who supplies one fail loudly instead
//! of being silently ignored. The requester is derived from where the gateway
//! is running:
//!
//! * the goal id and project root from the staging workspace path
//!   `<project>/.ta/staging/<goal-uuid>` the gateway was started in;
//! * the goal record is read from `<project>/.ta/goals` and must be active;
//! * the role from `TA_WHITEBOARD_TEAM_ROLE` (set by the launcher), then the
//!   goal's `role`, then its `agent_id`;
//! * the Wayfinder project id from `<project>/.ta/workflow.toml`
//!   `[plan.wayfinder] project_id` (the project root's copy, not the staging
//!   copy). No `[plan.wayfinder]` table, or an empty id, refuses the request:
//!   the funnel fails closed rather than guessing a project.
//!
//! A gateway that is not in a staging workspace (an operator's own session)
//! or that is chat-locked (the read-only Chief-of-Staff surface) refuses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use ta_goal::{GoalRunState, GoalRunStore};
use ta_task_intake::{
    EnqueueOutcome, FileIntakeStore, IntakeError, IntakeSettings, IntakeStore, RequestBody,
    RequestKind, Submission, MAX_BODY_BYTES,
};

use super::wayfinder_task::{
    invalid, validate_id, validate_number, validate_opt_description, validate_slug, validate_text,
    validate_title, validate_verb, ToolLabel, DEFAULT_DATAPOINT_SOURCE, MAX_ID_LEN, MAX_SLUG_LEN,
    REF_PREFIX,
};
use crate::server::GatewayState;

/// MCP name of the request tool.
pub const REQUEST_TOOL: &str = "ta_request_task";
/// MCP name of the status tool.
pub const STATUS_TOOL: &str = "ta_request_status";

/// Environment variable the launcher sets to the requester's team role.
pub const TEAM_ROLE_ENV: &str = "TA_WHITEBOARD_TEAM_ROLE";

/// Longest `why_now`, in characters.
pub const MAX_WHY_NOW_CHARS: usize = 300;

#[derive(Debug, Clone, Copy)]
struct RequestLabel(&'static str);

impl ToolLabel for RequestLabel {
    fn label(self) -> &'static str {
        self.0
    }
}

const REQUEST: RequestLabel = RequestLabel(REQUEST_TOOL);
const STATUS: RequestLabel = RequestLabel(STATUS_TOOL);

// ── Parameters ──────────────────────────────────────────────────

/// Arguments of `ta_request_task`. One flat object (tool schemas cannot use a
/// top-level `oneOf`); the fields that apply depend on `kind`, and a field
/// that belongs to the other kind is refused.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestTaskParams {
    /// What to request: `create-task` (needs `title`, `verb`) or
    /// `record-datapoint` (needs `metric_id`, `value`).
    pub kind: String,
    /// One sentence on why this is needed now (single line, at most 300
    /// characters). Shown to the reviewer as untrusted text.
    pub why_now: String,
    /// True if this goal cannot finish until the request is handled.
    #[serde(default)]
    pub blocks_goal: bool,
    /// `create-task`: title of the task (required).
    #[serde(default)]
    pub title: Option<String>,
    /// `create-task`: Wayfinder task verb, for example "implement" (required).
    #[serde(default)]
    pub verb: Option<String>,
    /// `create-task`: optional longer description.
    #[serde(default)]
    pub description: Option<String>,
    /// `record-datapoint`: the real Wayfinder KPI metric id (required).
    #[serde(default)]
    pub metric_id: Option<String>,
    /// `record-datapoint`: the number to record (required, finite).
    #[serde(default)]
    pub value: Option<f64>,
    /// `record-datapoint`: provenance label, a lowercase slug (default
    /// "manual").
    #[serde(default)]
    pub source: Option<String>,
}

/// Arguments of `ta_request_status`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestStatusParams {
    /// The `request_id` returned by `ta_request_task`.
    pub request_id: String,
}

// ── Validation ──────────────────────────────────────────────────

/// A request after validation: the typed body plus the common fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedRequest {
    /// Typed payload.
    pub body: RequestBody,
    /// Trimmed `why_now`.
    pub why_now: String,
    /// Whether the goal is blocked on the request.
    pub blocks_goal: bool,
}

fn require<'a>(field: &str, kind: RequestKind, v: &'a Option<String>) -> Result<&'a str, McpError> {
    v.as_deref().ok_or_else(|| {
        invalid(
            REQUEST,
            format!("{field} is required for kind {}", kind.as_str()),
        )
    })
}

fn refuse_foreign(
    kind: RequestKind,
    foreign: &[(&str, bool)],
    belongs_to: RequestKind,
) -> Result<(), McpError> {
    if let Some((name, _)) = foreign.iter().find(|(_, present)| *present) {
        return Err(invalid(
            REQUEST,
            format!(
                "{name} applies to kind {} only, but kind is {}; remove it",
                belongs_to.as_str(),
                kind.as_str()
            ),
        ));
    }
    Ok(())
}

/// Validate `params` with the same field validators the `ta_propose_*` tools
/// use, then enforce the 2 KB body cap.
pub fn validate_request(params: &RequestTaskParams) -> Result<ValidatedRequest, McpError> {
    let kind = RequestKind::parse(params.kind.trim()).ok_or_else(|| {
        invalid(
            REQUEST,
            format!(
                "kind {:?} is not supported; use one of: {}",
                params.kind,
                RequestKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;

    let why_now = params.why_now.trim().to_string();
    validate_text(REQUEST, "why_now", &why_now, MAX_WHY_NOW_CHARS, true, true)?;

    let body = match kind {
        RequestKind::CreateTask => {
            refuse_foreign(
                kind,
                &[
                    ("metric_id", params.metric_id.is_some()),
                    ("value", params.value.is_some()),
                    ("source", params.source.is_some()),
                ],
                RequestKind::RecordDatapoint,
            )?;
            let title = require("title", kind, &params.title)?;
            let verb = require("verb", kind, &params.verb)?;
            validate_title(REQUEST, "title", title)?;
            validate_verb(REQUEST, "verb", verb)?;
            validate_opt_description(REQUEST, "description", params.description.as_deref())?;
            RequestBody::CreateTask {
                title: title.to_string(),
                verb: verb.to_string(),
                description: params.description.clone(),
            }
        }
        RequestKind::RecordDatapoint => {
            refuse_foreign(
                kind,
                &[
                    ("title", params.title.is_some()),
                    ("verb", params.verb.is_some()),
                    ("description", params.description.is_some()),
                ],
                RequestKind::CreateTask,
            )?;
            let metric_id = require("metric_id", kind, &params.metric_id)?;
            validate_id(REQUEST, "metric_id", metric_id, MAX_ID_LEN)?;
            if metric_id.starts_with(REF_PREFIX) {
                return Err(invalid(
                    REQUEST,
                    "metric_id must be a real Wayfinder metric id; `ref:<n>` only works inside \
                     a draft, and a request has no draft to refer to",
                ));
            }
            let value = params
                .value
                .ok_or_else(|| invalid(REQUEST, "value is required for kind record-datapoint"))?;
            validate_number(REQUEST, "value", value, false, false)?;
            let source = params.source.as_deref().unwrap_or(DEFAULT_DATAPOINT_SOURCE);
            validate_slug(REQUEST, "source", source, MAX_SLUG_LEN)?;
            RequestBody::RecordDatapoint {
                metric_id: metric_id.to_string(),
                value,
                source: source.to_string(),
            }
        }
    };

    let encoded = serde_json::to_vec(&body)
        .map_err(|e| McpError::internal_error(format!("{REQUEST_TOOL}: {e}"), None))?;
    let size = encoded.len() + why_now.len();
    if size > MAX_BODY_BYTES {
        return Err(invalid(
            REQUEST,
            format!(
                "request is {size} bytes (fields plus why_now) but the cap is {MAX_BODY_BYTES} \
                 bytes. Shorten the description or why_now, or split it into two requests"
            ),
        ));
    }
    Ok(ValidatedRequest {
        body,
        why_now,
        blocks_goal: params.blocks_goal,
    })
}

// ── Requester binding ───────────────────────────────────────────

/// What the gateway knows about its own session, snapshotted so binding can
/// be resolved (and tested) without the gateway state.
#[derive(Debug, Clone)]
pub struct SessionFacts {
    /// The gateway's workspace root (the staging directory for a goal agent).
    pub workspace_root: PathBuf,
    /// The gateway is locked to a chat-mode session.
    pub chat_locked: bool,
    /// Role forwarded by the launcher, if any.
    pub team_role: Option<String>,
    /// Wayfinder project the launcher pinned the session to, if any.
    pub pinned_project_id: Option<String>,
}

/// The authenticated requester: everything the store needs, none of it from
/// tool arguments.
#[derive(Debug, Clone)]
pub struct Requester {
    /// Real project root (parent of `.ta`).
    pub project_root: PathBuf,
    /// The requesting goal.
    pub goal_id: Uuid,
    /// Requester role.
    pub role: String,
    /// The single Wayfinder project this request is scoped to.
    pub project_id: String,
    /// Caps and expiry from `[task_intake]`.
    pub settings: IntakeSettings,
}

impl Requester {
    fn store(&self) -> FileIntakeStore {
        FileIntakeStore::open(
            &self.project_root,
            self.settings,
            Some(self.project_root.join(".ta").join("audit.jsonl")),
        )
    }
}

fn denied(tool: &str, msg: impl std::fmt::Display) -> McpError {
    McpError::invalid_request(format!("{tool}: {msg}"), None)
}

/// Split `<project>/.ta/staging/<goal-uuid>` into the project root and goal id.
pub fn parse_staging_workspace(workspace_root: &Path) -> Option<(PathBuf, Uuid)> {
    let goal_id = Uuid::parse_str(workspace_root.file_name()?.to_str()?).ok()?;
    let staging = workspace_root.parent()?;
    if staging.file_name()? != "staging" {
        return None;
    }
    let ta_dir = staging.parent()?;
    if ta_dir.file_name()? != ".ta" {
        return None;
    }
    Some((ta_dir.parent()?.to_path_buf(), goal_id))
}

/// A role name usable as a persona file stem (no separators, bounded).
fn is_safe_role_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Resolve who is asking. `tool` names the tool in error text.
pub fn resolve_requester(tool: &str, facts: &SessionFacts) -> Result<Requester, McpError> {
    if facts.chat_locked {
        return Err(denied(
            tool,
            "not available in a read-only chat-mode session. Requests come from goal agents; \
             ask the project manager through the normal channel instead",
        ));
    }
    let (project_root, goal_id) =
        parse_staging_workspace(&facts.workspace_root).ok_or_else(|| {
            denied(
                tool,
                format!(
                    "only available to a goal agent running in its staging workspace \
                 (<project>/.ta/staging/<goal-id>), but this gateway runs in {}. Start the agent \
                 with `ta run` so it gets a goal identity",
                    facts.workspace_root.display()
                ),
            )
        })?;

    let goals_dir = project_root.join(".ta").join("goals");
    let store = GoalRunStore::new(&goals_dir).map_err(|e| {
        McpError::internal_error(
            format!(
                "{tool}: could not open the goal store {}: {e}",
                goals_dir.display()
            ),
            None,
        )
    })?;
    let goal = store
        .get(goal_id)
        .map_err(|e| {
            McpError::internal_error(
                format!(
                    "{tool}: could not read goal {goal_id} from {}: {e}",
                    goals_dir.display()
                ),
                None,
            )
        })?
        .ok_or_else(|| {
            denied(
                tool,
                format!(
                    "no goal record for {goal_id} in {}; the staging workspace does not belong \
                     to a known goal",
                    goals_dir.display()
                ),
            )
        })?;
    if !matches!(goal.state, GoalRunState::Configured | GoalRunState::Running) {
        return Err(denied(
            tool,
            format!(
                "goal {goal_id} is `{}`, not active; only a running goal may queue requests",
                goal.state
            ),
        ));
    }
    if !goal.workspace_path.as_os_str().is_empty()
        && !same_dir(&goal.workspace_path, &facts.workspace_root)
    {
        return Err(denied(
            tool,
            format!(
                "goal {goal_id} is bound to workspace {} but this gateway runs in {}; refusing \
                 to act for a goal from a different workspace",
                goal.workspace_path.display(),
                facts.workspace_root.display()
            ),
        ));
    }

    let candidates: Vec<&str> = facts
        .team_role
        .as_deref()
        .into_iter()
        .chain(goal.role.as_deref())
        .chain(std::iter::once(goal.agent_id.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for name in &candidates {
        if !is_safe_role_name(name) {
            continue;
        }
        if let Ok(persona) = ta_goal::persona::PersonaConfig::load(&project_root, name) {
            if persona.capabilities.read_only {
                return Err(denied(
                    tool,
                    format!(
                        "role `{name}` is a read-only persona and may not queue requests \
                         (requests lead to proposed changes)"
                    ),
                ));
            }
        }
    }
    let role = candidates
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let workflow_toml = project_root.join(".ta").join("workflow.toml");
    if !workflow_toml.is_file() {
        return Err(denied(
            tool,
            format!(
                "cannot tell which Wayfinder project to file this under: {} does not exist. \
                 Add a [plan.wayfinder] table with a project_id to it",
                workflow_toml.display()
            ),
        ));
    }
    let workflow = ta_submit::WorkflowConfig::load(&workflow_toml).map_err(|e| {
        denied(
            tool,
            format!(
                "could not parse {}: {e}. Fix the TOML and retry",
                workflow_toml.display()
            ),
        )
    })?;
    let project_id = workflow
        .plan
        .wayfinder
        .as_ref()
        .map(|w| w.project_id.trim().to_string())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            denied(
                tool,
                format!(
                    "{} has no [plan.wayfinder] project_id, so the request cannot be scoped to \
                     exactly one Wayfinder project. Set project_id there",
                    workflow_toml.display()
                ),
            )
        })?;
    if let Some(pin) = facts.pinned_project_id.as_deref() {
        if pin != project_id {
            return Err(denied(
                tool,
                format!(
                    "this session is pinned to Wayfinder project {pin} but {} maps to \
                     {project_id}; refusing to cross projects",
                    workflow_toml.display()
                ),
            ));
        }
    }

    Ok(Requester {
        project_root,
        goal_id,
        role,
        project_id,
        settings: workflow.task_intake.settings(),
    })
}

fn session_facts(state: &Arc<Mutex<GatewayState>>, tool: &str) -> Result<SessionFacts, McpError> {
    let locked = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("{tool}: lock poisoned: {e}"), None))?;
    Ok(SessionFacts {
        workspace_root: locked.config.workspace_root.clone(),
        chat_locked: locked.chat_lock.is_some(),
        team_role: std::env::var(TEAM_ROLE_ENV)
            .ok()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty()),
        pinned_project_id: locked.chat_wayfinder_project_id.clone(),
    })
}

fn map_store_error(tool: &str, e: IntakeError) -> McpError {
    match e {
        IntakeError::CapExceeded { .. } => McpError::invalid_request(format!("{tool}: {e}"), None),
        other => McpError::internal_error(format!("{tool}: {other}"), None),
    }
}

fn json_result(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&value)
            .map_err(|e| McpError::internal_error(format!("serialisation failed: {e}"), None))?,
    )]))
}

// ── Handlers ────────────────────────────────────────────────────

/// `ta_request_task`.
pub fn handle_request_task(
    state: &Arc<Mutex<GatewayState>>,
    params: RequestTaskParams,
) -> Result<CallToolResult, McpError> {
    request_task_at(&session_facts(state, REQUEST_TOOL)?, params, Utc::now())
}

/// `ta_request_task` with the session snapshot and clock supplied.
pub fn request_task_at(
    facts: &SessionFacts,
    params: RequestTaskParams,
    now: DateTime<Utc>,
) -> Result<CallToolResult, McpError> {
    let requester = resolve_requester(REQUEST_TOOL, facts)?;
    let request = validate_request(&params)?;
    let outcome = requester
        .store()
        .enqueue(
            Submission {
                project_id: requester.project_id.clone(),
                goal_id: requester.goal_id,
                role: requester.role.clone(),
                body: request.body,
                why_now: request.why_now,
                blocks_goal: request.blocks_goal,
            },
            now,
        )
        .map_err(|e| map_store_error(REQUEST_TOOL, e))?;
    let (request_id, status, deduplicated) = match outcome {
        EnqueueOutcome::Queued { request_id } => (request_id, "queued", false),
        EnqueueOutcome::Duplicate { request_id, state } => {
            (request_id, state.requester_status(), true)
        }
    };
    json_result(serde_json::json!({
        "request_id": request_id,
        "status": status,
        "deduplicated": deduplicated,
        "note": "Queued for the project manager to review. It has NOT been executed; a human \
                 approves any resulting change. Check progress with ta_request_status.",
    }))
}

/// `ta_request_status`.
pub fn handle_request_status(
    state: &Arc<Mutex<GatewayState>>,
    params: RequestStatusParams,
) -> Result<CallToolResult, McpError> {
    request_status_at(&session_facts(state, STATUS_TOOL)?, params)
}

/// `ta_request_status` with the session snapshot supplied.
pub fn request_status_at(
    facts: &SessionFacts,
    params: RequestStatusParams,
) -> Result<CallToolResult, McpError> {
    let requester = resolve_requester(STATUS_TOOL, facts)?;
    validate_id(STATUS, "request_id", &params.request_id, MAX_ID_LEN)?;
    let found = requester
        .store()
        .status_for_requester(&requester.project_id, &params.request_id, requester.goal_id)
        .map_err(|e| map_store_error(STATUS_TOOL, e))?;
    match found {
        Some(state) => json_result(serde_json::json!({
            "request_id": params.request_id,
            "status": state.requester_status(),
        })),
        None => Err(invalid(
            STATUS,
            format!(
                "no request {} from this goal. Only the goal that made a request can read its \
                 status; check the id returned by ta_request_task",
                params.request_id
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::GoalRun;

    fn text(r: &CallToolResult) -> serde_json::Value {
        let raw = serde_json::to_value(&r.content).unwrap();
        let s = raw[0]["text"].as_str().expect("text content").to_string();
        serde_json::from_str(&s).expect("json body")
    }

    fn err_text(e: McpError) -> String {
        e.message.to_string()
    }

    struct Project {
        dir: tempfile::TempDir,
    }

    impl Project {
        fn new(workflow: Option<&str>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
            if let Some(w) = workflow {
                std::fs::write(dir.path().join(".ta/workflow.toml"), w).unwrap();
            }
            Project { dir }
        }

        fn with_project(project_id: &str, extra: &str) -> Self {
            Self::new(Some(&format!(
                "[plan.wayfinder]\nbase_url = \"http://localhost:1\"\norg_id = \"org\"\n\
                 project_id = \"{project_id}\"\ncredential_name = \"c\"\n{extra}"
            )))
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn persona(&self, name: &str, read_only: bool) {
            let dir = self.root().join(".ta/personas");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{name}.toml")),
                format!("[persona]\nname = \"{name}\"\n[capabilities]\nread_only = {read_only}\n"),
            )
            .unwrap();
        }

        /// Add a goal record plus its staging dir and return the session a
        /// gateway running in that staging dir would see.
        fn goal(&self, agent: &str, role: Option<&str>, state: GoalRunState) -> SessionFacts {
            let mut goal = GoalRun::new(
                "t",
                "o",
                agent,
                PathBuf::new(),
                self.root().join(".ta/store"),
            );
            let staging = self
                .root()
                .join(".ta/staging")
                .join(goal.goal_run_id.to_string());
            std::fs::create_dir_all(&staging).unwrap();
            goal.workspace_path = staging.clone();
            goal.state = state;
            goal.role = role.map(str::to_string);
            GoalRunStore::new(self.root().join(".ta/goals"))
                .unwrap()
                .save(&goal)
                .unwrap();
            SessionFacts {
                workspace_root: staging,
                chat_locked: false,
                team_role: None,
                pinned_project_id: None,
            }
        }

        fn running(&self) -> SessionFacts {
            self.goal("agent-a", None, GoalRunState::Running)
        }
    }

    fn create_params(title: &str) -> RequestTaskParams {
        serde_json::from_value(serde_json::json!({
            "kind": "create-task",
            "why_now": "blocked on a missing build task",
            "blocks_goal": true,
            "title": title,
            "verb": "implement",
        }))
        .unwrap()
    }

    fn datapoint_params(metric: &str, value: f64) -> RequestTaskParams {
        serde_json::from_value(serde_json::json!({
            "kind": "record-datapoint",
            "why_now": "measured after the run",
            "metric_id": metric,
            "value": value,
        }))
        .unwrap()
    }

    fn status(facts: &SessionFacts, id: &str) -> Result<serde_json::Value, String> {
        request_status_at(
            facts,
            RequestStatusParams {
                request_id: id.into(),
            },
        )
        .map(|r| text(&r))
        .map_err(err_text)
    }

    // ── identity never comes from arguments ──

    #[test]
    fn identity_arguments_are_rejected_not_ignored() {
        for field in ["goal_run_id", "goal_id", "role", "project_id", "agent_id"] {
            let mut v = serde_json::json!({
                "kind": "create-task", "why_now": "x", "title": "t", "verb": "build",
            });
            v[field] = serde_json::json!("spoofed");
            let err = serde_json::from_value::<RequestTaskParams>(v)
                .expect_err(&format!("{field} must be refused"));
            assert!(err.to_string().contains(field), "{err}");
        }
        assert!(serde_json::from_value::<RequestStatusParams>(
            serde_json::json!({"request_id": "x", "goal_run_id": "y"})
        )
        .is_err());
    }

    #[test]
    fn staging_workspace_binding_parses_goal_and_root() {
        let id = Uuid::new_v4();
        let ws = PathBuf::from(format!("/proj/.ta/staging/{id}"));
        assert_eq!(
            parse_staging_workspace(&ws),
            Some((PathBuf::from("/proj"), id))
        );
        for bad in [
            "/proj".to_string(),
            "/proj/.ta/staging/not-a-uuid".to_string(),
            format!("/proj/staging/{id}"),
            format!("/proj/.ta/other/{id}"),
        ] {
            assert_eq!(parse_staging_workspace(Path::new(&bad)), None, "{bad}");
        }
    }

    // ── happy path and dedupe ──

    #[test]
    fn request_is_queued_with_gateway_identity_and_status_is_coarse() {
        let p = Project::with_project("proj-1", "");
        let facts = p.running();
        let r = request_task_at(&facts, create_params("Add retry"), Utc::now()).unwrap();
        let body = text(&r);
        assert_eq!(body["status"], "queued");
        assert_eq!(body["deduplicated"], false);
        let id = body["request_id"].as_str().unwrap().to_string();

        let queue = std::fs::read_to_string(p.root().join(".ta/task-intake/queue.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(queue.lines().next().unwrap()).unwrap();
        let goal_id = parse_staging_workspace(&facts.workspace_root).unwrap().1;
        assert_eq!(line["project_id"], "proj-1");
        assert_eq!(line["goal_id"], goal_id.to_string());
        assert_eq!(line["role"], "agent-a", "role falls back to the agent id");

        let s = status(&facts, &id).unwrap();
        assert_eq!(s["status"], "queued");
        assert!(s.get("state").is_none() && s.get("draft_id").is_none());
    }

    #[test]
    fn role_prefers_team_role_then_goal_role_then_agent() {
        let p = Project::with_project("proj-1", "");
        let mut facts = p.goal("agent-a", Some("goal-role"), GoalRunState::Running);
        request_task_at(&facts, create_params("one"), Utc::now()).unwrap();
        facts.team_role = Some("team-role".into());
        request_task_at(&facts, create_params("two"), Utc::now()).unwrap();
        let queue = std::fs::read_to_string(p.root().join(".ta/task-intake/queue.jsonl")).unwrap();
        let roles: Vec<String> = queue
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["role"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(roles, ["goal-role", "team-role"]);
    }

    #[test]
    fn identical_request_returns_existing_id_and_is_flagged() {
        let p = Project::with_project("proj-1", "");
        let facts = p.running();
        let a = text(&request_task_at(&facts, create_params("Same   title"), Utc::now()).unwrap());
        let b = text(&request_task_at(&facts, create_params("same title"), Utc::now()).unwrap());
        assert_eq!(a["request_id"], b["request_id"]);
        assert_eq!(b["deduplicated"], true);
        let queue = std::fs::read_to_string(p.root().join(".ta/task-intake/queue.jsonl")).unwrap();
        assert_eq!(queue.lines().count(), 1);
    }

    // ── caps ──

    #[test]
    fn cap_refusal_is_loud_and_names_the_cap_and_its_config_key() {
        let p = Project::with_project("proj-1", "\n[task_intake]\nper_goal_max = 2\n");
        let facts = p.running();
        request_task_at(&facts, create_params("a"), Utc::now()).unwrap();
        request_task_at(&facts, create_params("b"), Utc::now()).unwrap();
        let e = err_text(request_task_at(&facts, create_params("c"), Utc::now()).unwrap_err());
        assert!(e.contains("queue full, retry or ask_human"), "{e}");
        assert!(e.contains("per_goal_max"), "{e}");
        // A duplicate of an existing request is not charged to the cap.
        assert!(request_task_at(&facts, create_params("a"), Utc::now()).is_ok());
    }

    // ── validation ──

    #[test]
    fn oversized_request_is_refused_with_the_cap() {
        let p = Project::with_project("proj-1", "");
        let facts = p.running();
        let mut params = create_params("t");
        params.description = Some("x".repeat(3000));
        let e = err_text(request_task_at(&facts, params, Utc::now()).unwrap_err());
        assert!(e.contains("2048"), "{e}");
        assert!(!p.root().join(".ta/task-intake/queue.jsonl").exists());
    }

    #[test]
    fn request_at_the_byte_boundary_is_accepted_and_one_over_is_refused() {
        let mut params = create_params("t");
        let base = validate_request(&params).unwrap();
        let used = serde_json::to_vec(&base.body).unwrap().len() + base.why_now.len();
        params.description =
            Some("d".repeat(MAX_BODY_BYTES - used - ",\"description\":\"\"".len()));
        let ok = validate_request(&params).unwrap();
        let size = serde_json::to_vec(&ok.body).unwrap().len() + ok.why_now.len();
        assert_eq!(size, MAX_BODY_BYTES);
        params.description = Some(format!("{}d", params.description.unwrap()));
        assert!(validate_request(&params).is_err());
    }

    #[test]
    fn typed_schema_rejects_bad_fields_by_kind() {
        let bad = |v: serde_json::Value| {
            let p: RequestTaskParams = serde_json::from_value(v).unwrap();
            err_text(validate_request(&p).unwrap_err())
        };
        assert!(bad(serde_json::json!({"kind":"close-goal","why_now":"x"}))
            .contains("create-task, record-datapoint"));
        assert!(
            bad(serde_json::json!({"kind":"create-task","why_now":"x","verb":"b"}))
                .contains("title is required")
        );
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"x","title":"t","verb":"b","metric_id":"m"
        }))
        .contains("metric_id applies to kind record-datapoint"));
        assert!(bad(serde_json::json!({
            "kind":"record-datapoint","why_now":"x","metric_id":"m","value":1.0,"title":"t"
        }))
        .contains("title applies to kind create-task"));
        assert!(
            bad(serde_json::json!({"kind":"record-datapoint","why_now":"x","value":1.0}))
                .contains("metric_id is required")
        );
        assert!(bad(serde_json::json!({
            "kind":"record-datapoint","why_now":"x","metric_id":"ref:0","value":1.0
        }))
        .contains("real Wayfinder metric id"));
        assert!(bad(serde_json::json!({
            "kind":"record-datapoint","why_now":"x","metric_id":"m/../x","value":1.0
        }))
        .contains("invalid character"));
        assert!(bad(serde_json::json!({
            "kind":"record-datapoint","why_now":"x","metric_id":"m","value":1e30
        }))
        .contains("finite"));
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"line1\nline2","title":"t","verb":"b"
        }))
        .contains("control character"));
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"x","title":"t\u{1b}[2J","verb":"b"
        }))
        .contains("control character"));
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"x","title":"t","verb":"has space"
        }))
        .contains("verb"));
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"  ","title":"t","verb":"b"
        }))
        .contains("why_now must be non-empty"));
        assert!(bad(serde_json::json!({
            "kind":"create-task","why_now":"x".repeat(MAX_WHY_NOW_CHARS + 1),"title":"t","verb":"b"
        }))
        .contains("why_now is longer"));
    }

    #[test]
    fn datapoint_defaults_source_and_is_queued() {
        let p = Project::with_project("proj-1", "");
        let facts = p.running();
        let r = request_task_at(&facts, datapoint_params("metric-9", 4.5), Utc::now()).unwrap();
        assert_eq!(text(&r)["status"], "queued");
        let queue = std::fs::read_to_string(p.root().join(".ta/task-intake/queue.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(queue.lines().next().unwrap()).unwrap();
        assert_eq!(line["body"]["source"], "manual");
        assert_eq!(line["body"]["value"], 4.5);
    }

    // ── project binding fails closed ──

    #[test]
    fn missing_or_incomplete_wayfinder_config_fails_closed() {
        let no_file = Project::new(None);
        let e = err_text(
            request_task_at(&no_file.running(), create_params("t"), Utc::now()).unwrap_err(),
        );
        assert!(
            e.contains("workflow.toml") && e.contains("[plan.wayfinder]"),
            "{e}"
        );

        let no_table = Project::new(Some("[plan]\nbackend = \"file\"\n"));
        let e = err_text(
            request_task_at(&no_table.running(), create_params("t"), Utc::now()).unwrap_err(),
        );
        assert!(e.contains("project_id"), "{e}");

        let blank = Project::with_project("  ", "");
        let e = err_text(
            request_task_at(&blank.running(), create_params("t"), Utc::now()).unwrap_err(),
        );
        assert!(e.contains("project_id"), "{e}");

        let bad_toml = Project::new(Some("[plan.wayfinder\n"));
        let e = err_text(
            request_task_at(&bad_toml.running(), create_params("t"), Utc::now()).unwrap_err(),
        );
        assert!(e.contains("could not parse"), "{e}");
        for p in [&no_file, &no_table, &blank, &bad_toml] {
            assert!(!p.root().join(".ta/task-intake").exists());
        }
    }

    #[test]
    fn pinned_project_must_agree_with_config() {
        let p = Project::with_project("proj-1", "");
        let mut facts = p.running();
        facts.pinned_project_id = Some("proj-2".into());
        let e = err_text(request_task_at(&facts, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("proj-1") && e.contains("proj-2"), "{e}");
        facts.pinned_project_id = Some("proj-1".into());
        assert!(request_task_at(&facts, create_params("t"), Utc::now()).is_ok());
    }

    // ── who may ask ──

    #[test]
    fn chat_locked_and_non_staging_sessions_are_refused() {
        let p = Project::with_project("proj-1", "");
        let mut facts = p.running();
        facts.chat_locked = true;
        let e = err_text(request_task_at(&facts, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("chat-mode"), "{e}");

        let operator = SessionFacts {
            workspace_root: p.root().to_path_buf(),
            chat_locked: false,
            team_role: None,
            pinned_project_id: None,
        };
        let e = err_text(request_task_at(&operator, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("staging workspace"), "{e}");
        let e = err_text(
            request_status_at(
                &operator,
                RequestStatusParams {
                    request_id: "x".into(),
                },
            )
            .unwrap_err(),
        );
        assert!(e.contains("staging workspace"), "{e}");
    }

    #[test]
    fn inactive_or_unknown_goals_are_refused() {
        let p = Project::with_project("proj-1", "");
        for state in [GoalRunState::PrReady, GoalRunState::Applied] {
            let facts = p.goal("agent-a", None, state);
            let e = err_text(request_task_at(&facts, create_params("t"), Utc::now()).unwrap_err());
            assert!(e.contains("not active"), "{e}");
        }
        let ghost = SessionFacts {
            workspace_root: p
                .root()
                .join(".ta/staging")
                .join(Uuid::new_v4().to_string()),
            chat_locked: false,
            team_role: None,
            pinned_project_id: None,
        };
        let e = err_text(request_task_at(&ghost, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("no goal record"), "{e}");
    }

    #[test]
    fn goal_bound_to_another_workspace_is_refused() {
        let p = Project::with_project("proj-1", "");
        let mut facts = p.running();
        let id = parse_staging_workspace(&facts.workspace_root).unwrap().1;
        let mut goal = GoalRunStore::new(p.root().join(".ta/goals"))
            .unwrap()
            .get(id)
            .unwrap()
            .unwrap();
        goal.workspace_path = p.root().join("elsewhere");
        GoalRunStore::new(p.root().join(".ta/goals"))
            .unwrap()
            .save(&goal)
            .unwrap();
        facts.chat_locked = false;
        let e = err_text(request_task_at(&facts, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("different workspace"), "{e}");
    }

    #[test]
    fn read_only_roles_are_refused_whichever_field_names_them() {
        let p = Project::with_project("proj-1", "");
        p.persona("chief-of-staff", true);
        p.persona("builder", false);

        let mut by_team_role = p.running();
        by_team_role.team_role = Some("chief-of-staff".into());
        let e =
            err_text(request_task_at(&by_team_role, create_params("t"), Utc::now()).unwrap_err());
        assert!(e.contains("read-only persona"), "{e}");

        let by_goal_role = p.goal("agent-a", Some("chief-of-staff"), GoalRunState::Running);
        assert!(request_task_at(&by_goal_role, create_params("t"), Utc::now()).is_err());

        let by_agent = p.goal("chief-of-staff", None, GoalRunState::Running);
        assert!(request_task_at(&by_agent, create_params("t"), Utc::now()).is_err());
        // Status is refused for the same sessions too.
        assert!(request_status_at(
            &by_agent,
            RequestStatusParams {
                request_id: "x".into()
            }
        )
        .is_err());

        let writer = p.goal("builder", None, GoalRunState::Running);
        assert!(request_task_at(&writer, create_params("t"), Utc::now()).is_ok());
        assert!(
            !p.root()
                .join(".ta/task-intake")
                .join("queue.jsonl")
                .exists()
                || std::fs::read_to_string(p.root().join(".ta/task-intake/queue.jsonl"))
                    .unwrap()
                    .lines()
                    .count()
                    == 1,
            "only the writable role's request may be queued"
        );
    }

    // ── status scoping ──

    #[test]
    fn status_is_only_visible_to_the_requesting_goal() {
        let p = Project::with_project("proj-1", "");
        let owner = p.running();
        let other = p.running();
        let id = text(&request_task_at(&owner, create_params("secret plan"), Utc::now()).unwrap())
            ["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(status(&owner, &id).unwrap()["status"], "queued");
        let e = status(&other, &id).unwrap_err();
        assert!(e.contains("no request"), "{e}");
        let unknown = status(&other, "req-does-not-exist").unwrap_err();
        // Foreign and unknown ids are indistinguishable apart from the echoed id.
        assert_eq!(
            e.replace(&id, "ID"),
            unknown.replace("req-does-not-exist", "ID")
        );
    }

    #[test]
    fn status_does_not_cross_projects_for_the_same_goal() {
        let p = Project::with_project("proj-1", "");
        let owner = p.running();
        let id = text(&request_task_at(&owner, create_params("t"), Utc::now()).unwrap())
            ["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        // The project mapping changes under the same goal: the request now
        // belongs to a project this goal is no longer scoped to.
        std::fs::write(
            p.root().join(".ta/workflow.toml"),
            "[plan.wayfinder]\nbase_url = \"http://localhost:1\"\norg_id = \"org\"\n\
             project_id = \"proj-2\"\ncredential_name = \"c\"\n",
        )
        .unwrap();
        assert!(status(&owner, &id).unwrap_err().contains("no request"));
    }
}
