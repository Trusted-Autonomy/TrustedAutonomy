//! `ta_propose_task_*`: the draft-bundled Wayfinder task outcome tools.
//!
//! Lets a virtual-team worker propose a change to Wayfinder task state
//! (content update, create, reassign, needs-revision, on-hold, complete),
//! bundled into the SAME draft a human reviews -- not a separate automated
//! outcome report fired after the fact. Per the CoS read-only chat-mode
//! design (TrustedAutonomy
//! docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md,
//! "Task mutation" and "Dispatched" item 4): every proposal is captured as
//! a `PendingAction` in the goal's draft package (the same mechanism
//! `ta_external_action`'s `policy = "review"` branch uses -- see
//! `tools/action.rs`), surfaced in `ta draft view` alongside the rest of
//! the draft, and replayed against the real Wayfinder API only when the
//! draft containing it is approved and applied -- see
//! `apps/ta-cli/src/commands/draft_task_replay.rs`.
//!
//! **No tool in this module performs a live mutation.** Each handler only
//! validates its input and appends a `PendingAction` to in-memory gateway
//! state; there is no HTTP client anywhere in this file.
//!
//! Four more proposals cover the project-manager worker's planning surface:
//! `ta_propose_goal_create`, `ta_propose_kpi_metric_create`,
//! `ta_propose_kpi_datapoint_add` and `ta_propose_kpi_create` (org-level
//! KPI). They ride in the same draft and replay the same way. A proposal may
//! refer to a goal or metric proposed EARLIER IN THE SAME DRAFT with the
//! symbolic id `ref:<n>`, where `n` is the earlier action's index in the
//! draft (echoed as `ref` in that tool's result); replay resolves it from
//! the ids the earlier action returned.
//!
//! All ten tools share one dispatcher, [`ProposeKind`], so the gateway side
//! (tool name, capture) and the replay side (`tool_name` -> what to call)
//! agree on a single closed set rather than independent string constants.
//!
//! Deliberately simpler than `ta_external_action`: no action-type
//! registry, no policy table, no connector/budget plumbing. A task
//! proposal always goes through review -- there is no `auto`/`block`
//! mode -- since the whole point is that a reviewer sees the proposed
//! change alongside the work it describes, not that it ever bypasses that
//! review.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

use ta_changeset::draft_package::{ActionKind, ArtifactDisposition, PendingAction};

use crate::server::GatewayState;

/// The closed set of draft-bundled task outcome kinds. The single source of
/// truth for each tool's MCP name, shared with the apply-time replay step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProposeKind {
    /// Title/description revision (`PATCH tasks/:id/content`).
    Update,
    /// New task (`POST tasks`, idempotent upsert by `external_id`).
    Create,
    /// Assignee change (`PATCH tasks/:id/assignee`).
    Reassign,
    /// Delivered work is wrong; back to the queue (`status = open`).
    NeedsRevision,
    /// Task is blocked (`status = on_hold` + `hold_reason`, optional
    /// precursor task + dependency edge).
    OnHold,
    /// Task done by review (`status = done`).
    Complete,
    /// New goal (`POST goals`, idempotent by `external_id`).
    GoalCreate,
    /// New countable KPI metric under a goal (`POST goals/:id/kpi-metrics`).
    KpiMetricCreate,
    /// One datapoint on an `external` KPI metric
    /// (`POST kpi-metrics/:id/datapoints`).
    KpiDatapointAdd,
    /// New org-level KPI (`POST /api/orgs/:org/kpis`).
    KpiCreate,
}

impl ProposeKind {
    pub const ALL: [ProposeKind; 10] = [
        ProposeKind::Update,
        ProposeKind::Create,
        ProposeKind::Reassign,
        ProposeKind::NeedsRevision,
        ProposeKind::OnHold,
        ProposeKind::Complete,
        ProposeKind::GoalCreate,
        ProposeKind::KpiMetricCreate,
        ProposeKind::KpiDatapointAdd,
        ProposeKind::KpiCreate,
    ];

    /// The MCP tool name, and the `tool_name` every `PendingAction` of this
    /// kind carries.
    pub const fn tool_name(self) -> &'static str {
        match self {
            ProposeKind::Update => "ta_propose_task_update",
            ProposeKind::Create => "ta_propose_task_create",
            ProposeKind::Reassign => "ta_propose_task_reassign",
            ProposeKind::NeedsRevision => "ta_propose_task_needs_revision",
            ProposeKind::OnHold => "ta_propose_task_on_hold",
            ProposeKind::Complete => "ta_propose_task_complete",
            ProposeKind::GoalCreate => "ta_propose_goal_create",
            ProposeKind::KpiMetricCreate => "ta_propose_kpi_metric_create",
            ProposeKind::KpiDatapointAdd => "ta_propose_kpi_datapoint_add",
            ProposeKind::KpiCreate => "ta_propose_kpi_create",
        }
    }

    /// True for the goal / KPI / datapoint proposals (as opposed to task
    /// outcome proposals). These get the structured draft-view rendering
    /// and the `ta_draft:<draft_id>:<n>` idempotency key at replay.
    pub const fn is_planning(self) -> bool {
        matches!(
            self,
            ProposeKind::GoalCreate
                | ProposeKind::KpiMetricCreate
                | ProposeKind::KpiDatapointAdd
                | ProposeKind::KpiCreate
        )
    }

    /// Whether a later proposal in the same draft may refer to this one's
    /// result with `ref:<n>` (a created goal or metric).
    pub const fn is_referenceable(self) -> bool {
        matches!(self, ProposeKind::GoalCreate | ProposeKind::KpiMetricCreate)
    }

    pub fn from_tool_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.tool_name() == name)
    }
}

/// Kept for compatibility with existing callers: the `ta_propose_task_update`
/// tool name.
pub const TOOL_NAME: &str = ProposeKind::Update.tool_name();

// ── Validation limits ───────────────────────────────────────────

pub const MAX_ID_LEN: usize = 128;
pub const MAX_EXTERNAL_ID_LEN: usize = 256;
pub const MAX_VERB_LEN: usize = 64;
pub const MAX_TITLE_LEN: usize = 500;
pub const MAX_DESCRIPTION_LEN: usize = 10_000;
pub const MAX_REASON_LEN: usize = 2_000;
pub const MAX_LABEL_LEN: usize = 200;
pub const MAX_UNIT_LEN: usize = 32;
pub const MAX_SLUG_LEN: usize = 32;
/// Numbers (targets, weights, datapoint values) must be finite and within
/// plus or minus this bound, matching Wayfinder's own datapoint limit.
pub const MAX_ABS_VALUE: f64 = 1e12;

/// Prefix of a symbolic reference to an earlier proposal in the same draft.
pub const REF_PREFIX: &str = "ref:";

/// Closed value lists, mirroring `wayfinder-core` (`MetricCadence`,
/// `MetricKind`, `MetricAggregation`).
pub const METRIC_CADENCES: [&str; 4] = ["none", "daily", "weekly", "monthly"];
pub const METRIC_KINDS: [&str; 2] = ["task_count", "external"];
pub const METRIC_AGGREGATIONS: [&str; 2] = ["sum", "latest"];
/// Goal kind used when a proposal names none (Wayfinder's default).
pub const DEFAULT_GOAL_KIND: &str = "milestone";
/// Datapoint source used when a proposal names none.
pub const DEFAULT_DATAPOINT_SOURCE: &str = "manual";

/// Prefix of the `external_id` derived when a create (or an on-hold
/// precursor) proposal carries none. Deterministic in `goal_run_id` +
/// `action_id`, so re-applying the same draft upserts the same task rather
/// than creating a duplicate.
pub const DERIVED_EXTERNAL_ID_PREFIX: &str = "ta-proposal";

// ── Params ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskUpdateParams {
    /// The real Wayfinder task id (not an `external_id`) this update
    /// applies to -- the same task this goal was dispatched to work on.
    pub task_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// The UUID of the goal run this proposal is part of. Required (unlike
    /// `ta_external_action`'s optional `goal_run_id`, which silently drops
    /// the action when absent): a proposal with no goal to attach to has
    /// nowhere to surface for review and would be silently lost.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskCreateParams {
    /// Title of the new task (required, non-empty).
    pub title: String,
    /// Wayfinder task verb (required by Wayfinder's create endpoint), e.g.
    /// "implement", "review", "research".
    pub verb: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Optional idempotency key. When omitted, one is derived from
    /// goal_run_id + the proposal's action id so re-applying the draft can
    /// never create a duplicate task.
    #[serde(default)]
    pub external_id: Option<String>,
    /// Optional Wayfinder roster/team-role id to assign (not a name).
    #[serde(default)]
    pub assignee_id: Option<String>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskReassignParams {
    /// The real Wayfinder task id to reassign.
    pub task_id: String,
    /// Required. Wayfinder roster/team-role id (not a display name), or
    /// JSON `null` to clear the assignee. Omitting the field is rejected so
    /// a clear can never happen by accident.
    #[serde(default, deserialize_with = "present_nullable")]
    #[schemars(with = "Option<String>")]
    pub assignee_id: Option<Option<String>>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskNeedsRevisionParams {
    /// The real Wayfinder task id whose delivered work must be redone.
    pub task_id: String,
    /// Optional explanation for the reviewer of what is wrong. Shown in the
    /// draft; not sent to Wayfinder (needs-revision is a plain `open`).
    #[serde(default)]
    pub reason: Option<String>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BlockingTaskSpec {
    /// Title of the precursor task that blocks the held task.
    pub title: String,
    /// Wayfinder task verb for the precursor (required by Wayfinder).
    pub verb: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskOnHoldParams {
    /// The real Wayfinder task id to put on hold.
    pub task_id: String,
    /// Required, non-empty: why the task is blocked.
    pub hold_reason: String,
    /// Optional precursor task. When set, apply creates it and records that
    /// `task_id` depends on it, so the hold is structural.
    #[serde(default)]
    pub blocking_task: Option<BlockingTaskSpec>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskCompleteParams {
    /// The real Wayfinder task id to mark done.
    pub task_id: String,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeGoalCreateParams {
    /// Goal name (required, one line).
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Goal kind slug (lowercase letters, digits, '_' or '-'). Defaults to
    /// "milestone".
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional parent goal: a real Wayfinder goal id, or `ref:<n>` for a
    /// goal proposed earlier in this same draft.
    #[serde(default)]
    pub parent_goal_id: Option<String>,
    /// Optional target date, `YYYY-MM-DD`.
    #[serde(default)]
    pub target_date: Option<String>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeKpiMetricCreateParams {
    /// The goal this countable target belongs to: a real Wayfinder goal id,
    /// or `ref:<n>` for a goal proposed earlier in this same draft.
    pub goal_id: String,
    /// Metric label (required, one line, at most 200 characters).
    pub label: String,
    /// Unit, e.g. "tasks", "USD", "signups" (required, one line, at most 32
    /// characters).
    pub unit: String,
    /// Target value: finite, greater than zero, within +-1e12.
    pub target: f64,
    /// One of: none, daily, weekly, monthly.
    pub cadence: String,
    /// One of: task_count (counted from the goal's tasks), external (you
    /// report numbers with ta_propose_kpi_datapoint_add).
    pub kind: String,
    /// One of: sum (default), latest.
    #[serde(default)]
    pub aggregation: Option<String>,
    /// Whether the metric requires the goal gate. Defaults to false.
    #[serde(default)]
    pub require_gate: Option<bool>,
    /// Weight, finite, zero or more, within 1e12. Defaults to 1.0.
    #[serde(default)]
    pub weight: Option<f64>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeKpiDatapointAddParams {
    /// The metric to record against: a real Wayfinder metric id, or
    /// `ref:<n>` for a metric proposed earlier in this same draft. Wayfinder
    /// refuses datapoints on a task_count metric.
    pub metric_id: String,
    /// The value: finite, within +-1e12.
    pub value: f64,
    /// Where the number came from (slug). Defaults to "manual".
    #[serde(default)]
    pub source: Option<String>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeKpiCreateParams {
    /// KPI label (required, one line, at most 200 characters).
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Weight, finite, zero or more, within 1e12. Defaults to 1.0.
    #[serde(default)]
    pub weight: Option<f64>,
    /// Optional external reference for the KPI's origin.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// When true the KPI is org-wide. By default it is tied to this
    /// project (the project pinned in `[plan.wayfinder]`).
    #[serde(default)]
    pub org_wide: Option<bool>,
    /// The UUID of the goal run this proposal is part of.
    pub goal_run_id: String,
}

/// Distinguishes a missing field (`None`) from an explicit `null`
/// (`Some(None)`). Used with `#[serde(default)]`.
fn present_nullable<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
}

// ── Validation helpers ──────────────────────────────────────────

fn invalid(kind: ProposeKind, msg: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(format!("{}: {msg}", kind.tool_name()), None)
}

/// Ids (task ids, roster ids, external ids): non-empty, bounded, and
/// restricted to `[A-Za-z0-9._:-]`, so they can't smuggle whitespace,
/// control characters, or path separators.
fn validate_id(kind: ProposeKind, field: &str, value: &str, max: usize) -> Result<(), McpError> {
    if value.is_empty() {
        return Err(invalid(kind, format!("{field} must be non-empty")));
    }
    if value.chars().count() > max {
        return Err(invalid(
            kind,
            format!("{field} is longer than {max} characters"),
        ));
    }
    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')))
    {
        return Err(invalid(
            kind,
            format!(
                "{field} contains an invalid character {bad:?}; only ASCII letters, digits, \
                 '.', '_', ':' and '-' are allowed (pass the real Wayfinder id, e.g. from the \
                 task this goal was dispatched for)"
            ),
        ));
    }
    Ok(())
}

fn validate_verb(kind: ProposeKind, field: &str, value: &str) -> Result<(), McpError> {
    if value.is_empty() {
        return Err(invalid(
            kind,
            format!("{field} must be non-empty (Wayfinder requires a task verb)"),
        ));
    }
    if value.len() > MAX_VERB_LEN
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(invalid(
            kind,
            format!(
                "{field} must be at most {MAX_VERB_LEN} ASCII letters, digits, '_' or '-' \
                 (e.g. \"implement\")"
            ),
        ));
    }
    Ok(())
}

/// Free text. `single_line` text (titles) rejects every control character;
/// multi-line text (descriptions, reasons) allows `\n`, `\r` and `\t` only.
/// `required` text must contain a non-whitespace character.
fn validate_text(
    kind: ProposeKind,
    field: &str,
    value: &str,
    max: usize,
    single_line: bool,
    required: bool,
) -> Result<(), McpError> {
    if required && value.trim().is_empty() {
        return Err(invalid(kind, format!("{field} must be non-empty")));
    }
    if value.chars().count() > max {
        return Err(invalid(
            kind,
            format!("{field} is longer than {max} characters"),
        ));
    }
    let bad = value
        .chars()
        .find(|c| c.is_control() && (single_line || !matches!(c, '\n' | '\r' | '\t')));
    if let Some(c) = bad {
        return Err(invalid(
            kind,
            format!(
                "{field} contains a disallowed control character (U+{:04X})",
                c as u32
            ),
        ));
    }
    Ok(())
}

fn validate_title(kind: ProposeKind, field: &str, value: &str) -> Result<(), McpError> {
    validate_text(kind, field, value, MAX_TITLE_LEN, true, true)
}

fn validate_opt_description(
    kind: ProposeKind,
    field: &str,
    value: Option<&str>,
) -> Result<(), McpError> {
    match value {
        Some(v) => validate_text(kind, field, v, MAX_DESCRIPTION_LEN, false, false),
        None => Ok(()),
    }
}

/// A lowercase slug: 1..=`max` chars of `[a-z0-9_-]`, starting with a letter
/// or digit (Wayfinder's goal kind and datapoint source shape).
fn validate_slug(kind: ProposeKind, field: &str, value: &str, max: usize) -> Result<(), McpError> {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !first_ok || !rest_ok || value.chars().count() > max {
        return Err(invalid(
            kind,
            format!(
                "{field} must be 1 to {max} lowercase characters from a-z, 0-9, '_' and '-', \
                 starting with a letter or digit (got {value:?})"
            ),
        ));
    }
    Ok(())
}

fn validate_enum(
    kind: ProposeKind,
    field: &str,
    value: &str,
    allowed: &[&str],
) -> Result<(), McpError> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(invalid(
        kind,
        format!(
            "{field} {value:?} is not allowed; use one of: {}",
            allowed.join(", ")
        ),
    ))
}

/// A finite number within +-[`MAX_ABS_VALUE`]; `min_exclusive_zero` also
/// requires it to be greater than zero, `non_negative` to be zero or more.
fn validate_number(
    kind: ProposeKind,
    field: &str,
    value: f64,
    positive: bool,
    non_negative: bool,
) -> Result<(), McpError> {
    if !value.is_finite() || value.abs() > MAX_ABS_VALUE {
        return Err(invalid(
            kind,
            format!("{field} must be a finite number within +-{MAX_ABS_VALUE:e} (got {value})"),
        ));
    }
    if positive && value <= 0.0 {
        return Err(invalid(
            kind,
            format!("{field} must be greater than zero (got {value})"),
        ));
    }
    if non_negative && value < 0.0 {
        return Err(invalid(
            kind,
            format!("{field} must not be negative (got {value})"),
        ));
    }
    Ok(())
}

fn validate_date(kind: ProposeKind, field: &str, value: &str) -> Result<(), McpError> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| {
            invalid(
                kind,
                format!("{field} must be a YYYY-MM-DD date (got {value:?})"),
            )
        })
}

/// Index named by a `ref:<n>` value, or `None` if `value` is not a ref.
/// A malformed ref (`ref:`, `ref:x`, `ref:-1`) is an error, never an id.
fn parse_ref(kind: ProposeKind, field: &str, value: &str) -> Result<Option<usize>, McpError> {
    let Some(n) = value.strip_prefix(REF_PREFIX) else {
        return Ok(None);
    };
    if n.is_empty() || n.len() > 6 || !n.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid(
            kind,
            format!(
                "{field} {value:?} is not a valid reference; use `ref:<n>` with the `ref` an \
                 earlier proposal in this goal returned, or a real Wayfinder id"
            ),
        ));
    }
    Ok(Some(n.parse().expect("1..=6 ASCII digits")))
}

/// An id field that may be a real Wayfinder id or `ref:<n>`. A ref must name
/// an already captured proposal of kind `expect` under this goal.
fn validate_id_or_ref(
    state: &Arc<Mutex<GatewayState>>,
    kind: ProposeKind,
    field: &str,
    value: &str,
    goal_id: Uuid,
    expect: ProposeKind,
) -> Result<(), McpError> {
    let Some(idx) = parse_ref(kind, field, value)? else {
        return validate_id(kind, field, value, MAX_ID_LEN);
    };
    let guard = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {e}"), None))?;
    let captured = guard.pending_actions.get(&goal_id).map_or(0, Vec::len);
    match guard.pending_actions.get(&goal_id).and_then(|a| a.get(idx)) {
        None => Err(invalid(
            kind,
            format!(
                "{field} {value:?} refers to action {idx}, but this goal run has only {captured} \
                 captured proposal(s) so far. Propose the {} first and use the `ref` its \
                 result returned",
                expect.tool_name()
            ),
        )),
        Some(a) if a.tool_name != expect.tool_name() => Err(invalid(
            kind,
            format!(
                "{field} {value:?} refers to action {idx}, which is a {}, not a {}. Use the \
                 `ref` returned by {}",
                a.tool_name,
                expect.tool_name(),
                expect.tool_name()
            ),
        )),
        Some(_) => Ok(()),
    }
}

fn parse_goal_id(kind: ProposeKind, raw: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(raw).map_err(|e| invalid(kind, format!("goal_run_id is not a valid UUID: {e}")))
}

/// Deterministic external id for a proposal-created task.
pub fn derived_external_id(goal_id: Uuid, action_id: Uuid, suffix: Option<&str>) -> String {
    match suffix {
        Some(s) => format!("{DERIVED_EXTERNAL_ID_PREFIX}:{goal_id}:{action_id}:{s}"),
        None => format!("{DERIVED_EXTERNAL_ID_PREFIX}:{goal_id}:{action_id}"),
    }
}

// ── Shared capture ──────────────────────────────────────────────

/// What a handler contributes to the captured `PendingAction`, computed
/// once the goal id and action id are known (so derived external ids can
/// be baked into the parameters the reviewer sees).
struct Proposal {
    parameters: serde_json::Value,
    description: String,
    target_uri: String,
    /// Echoed back to the caller in the tool result.
    result: serde_json::Value,
}

/// The single capture path for every `ta_propose_task_*` tool: stores a
/// `StateChanging`, `Pending` `PendingAction` under the goal. Never calls
/// out to Wayfinder.
fn capture(
    state: &Arc<Mutex<GatewayState>>,
    kind: ProposeKind,
    goal_run_id: &str,
    build: impl FnOnce(Uuid, Uuid) -> Proposal,
) -> Result<CallToolResult, McpError> {
    let goal_id = parse_goal_id(kind, goal_run_id)?;
    let action_id = Uuid::new_v4();
    let proposal = build(goal_id, action_id);

    let pending = PendingAction {
        action_id,
        tool_name: kind.tool_name().to_string(),
        parameters: proposal.parameters,
        kind: ActionKind::StateChanging,
        intercepted_at: Utc::now(),
        description: proposal.description,
        target_uri: Some(proposal.target_uri),
        disposition: ArtifactDisposition::Pending,
    };

    let index;
    {
        let mut state = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {e}"), None))?;
        let actions = state.pending_actions.entry(goal_id).or_default();
        index = actions.len();
        actions.push(pending);
    }

    let mut result = serde_json::json!({
        "status": "captured_for_review",
        "tool": kind.tool_name(),
        "action_id": action_id.to_string(),
        "note": "Not executed. Applied to Wayfinder only if this goal's draft is approved and applied.",
    });
    if kind.is_referenceable() {
        result["ref"] = serde_json::json!(format!("{REF_PREFIX}{index}"));
        result["ref_note"] = serde_json::json!(
            "Use this `ref` as goal_id / metric_id / parent_goal_id in a later proposal in the \
             same draft; it resolves to the real Wayfinder id when the draft is applied."
        );
    }
    if let (Some(obj), serde_json::Value::Object(extra)) = (result.as_object_mut(), proposal.result)
    {
        obj.extend(extra);
    }
    Ok(CallToolResult::success(vec![Content::json(result)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn task_uri(task_id: &str) -> String {
    format!("wayfinder://tasks/{task_id}")
}

// ── Handlers ────────────────────────────────────────────────────

pub fn handle_propose_task_update(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskUpdateParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::Update;
    if params.title.is_none() && params.description.is_none() {
        return Err(invalid(
            kind,
            "at least one of title/description must be set -- an empty proposal has nothing \
             to review or apply",
        ));
    }
    validate_id(kind, "task_id", &params.task_id, MAX_ID_LEN)?;
    if let Some(t) = &params.title {
        validate_title(kind, "title", t)?;
    }
    validate_opt_description(kind, "description", params.description.as_deref())?;

    let description = match (&params.title, &params.description) {
        (Some(t), Some(_)) => format!(
            "Propose updating task {}: title -> \"{}\", description updated",
            params.task_id, t
        ),
        (Some(t), None) => format!(
            "Propose updating task {}: title -> \"{}\"",
            params.task_id, t
        ),
        _ => format!(
            "Propose updating task {}: description updated",
            params.task_id
        ),
    };
    capture(state, kind, &params.goal_run_id, |_, _| Proposal {
        parameters: serde_json::json!({
            "task_id": params.task_id,
            "title": params.title,
            "description": params.description,
        }),
        description,
        target_uri: task_uri(&params.task_id),
        result: serde_json::json!({ "task_id": params.task_id }),
    })
}

pub fn handle_propose_task_create(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskCreateParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::Create;
    validate_title(kind, "title", &params.title)?;
    validate_verb(kind, "verb", &params.verb)?;
    validate_opt_description(kind, "description", params.description.as_deref())?;
    if let Some(ext) = &params.external_id {
        validate_id(kind, "external_id", ext, MAX_EXTERNAL_ID_LEN)?;
    }
    if let Some(a) = &params.assignee_id {
        validate_id(kind, "assignee_id", a, MAX_ID_LEN)?;
    }

    capture(state, kind, &params.goal_run_id, |goal_id, action_id| {
        let external_id = params
            .external_id
            .clone()
            .unwrap_or_else(|| derived_external_id(goal_id, action_id, None));
        let mut description = format!(
            "Propose creating task \"{}\" (verb {})",
            params.title, params.verb
        );
        if let Some(a) = &params.assignee_id {
            description.push_str(&format!(", assigned to {a}"));
        }
        Proposal {
            parameters: serde_json::json!({
                "title": params.title,
                "verb": params.verb,
                "description": params.description,
                "external_id": external_id,
                "assignee_id": params.assignee_id,
            }),
            description,
            target_uri: format!("wayfinder://tasks/external/{external_id}"),
            result: serde_json::json!({ "external_id": external_id }),
        }
    })
}

pub fn handle_propose_task_reassign(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskReassignParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::Reassign;
    validate_id(kind, "task_id", &params.task_id, MAX_ID_LEN)?;
    let Some(assignee) = params.assignee_id.clone() else {
        return Err(invalid(
            kind,
            "assignee_id is required: pass a Wayfinder roster/team-role id, or null to clear \
             the assignee",
        ));
    };
    if let Some(a) = &assignee {
        validate_id(kind, "assignee_id", a, MAX_ID_LEN)?;
    }
    let description = match &assignee {
        Some(a) => format!("Propose reassigning task {} to {a}", params.task_id),
        None => format!("Propose clearing the assignee of task {}", params.task_id),
    };
    capture(state, kind, &params.goal_run_id, |_, _| Proposal {
        parameters: serde_json::json!({
            "task_id": params.task_id,
            "assignee_id": assignee,
        }),
        description,
        target_uri: task_uri(&params.task_id),
        result: serde_json::json!({ "task_id": params.task_id }),
    })
}

pub fn handle_propose_task_needs_revision(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskNeedsRevisionParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::NeedsRevision;
    validate_id(kind, "task_id", &params.task_id, MAX_ID_LEN)?;
    if let Some(r) = &params.reason {
        validate_text(kind, "reason", r, MAX_REASON_LEN, false, false)?;
    }
    let mut description = format!(
        "Propose marking task {} as needs-revision (back to open)",
        params.task_id
    );
    if let Some(r) = params.reason.as_deref().filter(|r| !r.trim().is_empty()) {
        description.push_str(&format!(": {r}"));
    }
    capture(state, kind, &params.goal_run_id, |_, _| Proposal {
        parameters: serde_json::json!({
            "task_id": params.task_id,
            "reason": params.reason,
        }),
        description,
        target_uri: task_uri(&params.task_id),
        result: serde_json::json!({ "task_id": params.task_id }),
    })
}

pub fn handle_propose_task_on_hold(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskOnHoldParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::OnHold;
    validate_id(kind, "task_id", &params.task_id, MAX_ID_LEN)?;
    validate_text(
        kind,
        "hold_reason",
        &params.hold_reason,
        MAX_REASON_LEN,
        false,
        true,
    )?;
    if let Some(b) = &params.blocking_task {
        validate_title(kind, "blocking_task.title", &b.title)?;
        validate_verb(kind, "blocking_task.verb", &b.verb)?;
        validate_opt_description(kind, "blocking_task.description", b.description.as_deref())?;
    }

    capture(state, kind, &params.goal_run_id, |goal_id, action_id| {
        let blocking = params.blocking_task.as_ref().map(|b| {
            serde_json::json!({
                "title": b.title,
                "verb": b.verb,
                "description": b.description,
                "external_id": derived_external_id(goal_id, action_id, Some("precursor")),
            })
        });
        let mut description = format!(
            "Propose putting task {} on hold: {}",
            params.task_id, params.hold_reason
        );
        if let Some(b) = &params.blocking_task {
            description.push_str(&format!(" (blocked by new precursor task \"{}\")", b.title));
        }
        Proposal {
            parameters: serde_json::json!({
                "task_id": params.task_id,
                "hold_reason": params.hold_reason,
                "blocking_task": blocking,
            }),
            description,
            target_uri: task_uri(&params.task_id),
            result: serde_json::json!({ "task_id": params.task_id }),
        }
    })
}

pub fn handle_propose_task_complete(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskCompleteParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::Complete;
    validate_id(kind, "task_id", &params.task_id, MAX_ID_LEN)?;
    let description = format!("Propose marking task {} as done", params.task_id);
    capture(state, kind, &params.goal_run_id, |_, _| Proposal {
        parameters: serde_json::json!({ "task_id": params.task_id }),
        description,
        target_uri: task_uri(&params.task_id),
        result: serde_json::json!({ "task_id": params.task_id }),
    })
}

// ── Goal / KPI / datapoint proposals ────────────────────────────

fn goal_uri(tag: &str) -> String {
    format!("wayfinder://goals/{tag}")
}

pub fn handle_propose_goal_create(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeGoalCreateParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::GoalCreate;
    let goal_id = parse_goal_id(kind, &params.goal_run_id)?;
    validate_title(kind, "name", &params.name)?;
    validate_opt_description(kind, "description", params.description.as_deref())?;
    let goal_kind = params.kind.as_deref().unwrap_or(DEFAULT_GOAL_KIND);
    validate_slug(kind, "kind", goal_kind, MAX_SLUG_LEN)?;
    if let Some(p) = &params.parent_goal_id {
        validate_id_or_ref(
            state,
            kind,
            "parent_goal_id",
            p,
            goal_id,
            ProposeKind::GoalCreate,
        )?;
    }
    if let Some(d) = &params.target_date {
        validate_date(kind, "target_date", d)?;
    }

    let mut description = format!(
        "Propose creating goal \"{}\" (kind {goal_kind})",
        params.name
    );
    if let Some(p) = &params.parent_goal_id {
        description.push_str(&format!(", under goal {p}"));
    }
    if let Some(d) = &params.target_date {
        description.push_str(&format!(", target {d}"));
    }
    capture(state, kind, &params.goal_run_id, |_, action_id| Proposal {
        parameters: serde_json::json!({
            "name": params.name,
            "description": params.description,
            "kind": goal_kind,
            "parent_goal_id": params.parent_goal_id,
            "target_date": params.target_date,
        }),
        description,
        target_uri: goal_uri(&format!("proposed/{action_id}")),
        result: serde_json::json!({}),
    })
}

pub fn handle_propose_kpi_metric_create(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeKpiMetricCreateParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::KpiMetricCreate;
    let goal_id = parse_goal_id(kind, &params.goal_run_id)?;
    validate_id_or_ref(
        state,
        kind,
        "goal_id",
        &params.goal_id,
        goal_id,
        ProposeKind::GoalCreate,
    )?;
    validate_text(kind, "label", &params.label, MAX_LABEL_LEN, true, true)?;
    validate_text(kind, "unit", &params.unit, MAX_UNIT_LEN, true, true)?;
    validate_number(kind, "target", params.target, true, false)?;
    validate_enum(kind, "cadence", &params.cadence, &METRIC_CADENCES)?;
    validate_enum(kind, "kind", &params.kind, &METRIC_KINDS)?;
    let aggregation = params.aggregation.as_deref().unwrap_or("sum");
    validate_enum(kind, "aggregation", aggregation, &METRIC_AGGREGATIONS)?;
    let weight = params.weight.unwrap_or(1.0);
    validate_number(kind, "weight", weight, false, true)?;
    let require_gate = params.require_gate.unwrap_or(false);

    let description = format!(
        "Propose KPI metric \"{}\": target {} {} {} ({}, {}) on goal {}",
        params.label,
        params.target,
        params.unit,
        match params.cadence.as_str() {
            "daily" => "per day",
            "weekly" => "per week",
            "monthly" => "per month",
            _ => "one-time",
        },
        params.kind,
        aggregation,
        params.goal_id
    );
    capture(state, kind, &params.goal_run_id, |_, action_id| Proposal {
        parameters: serde_json::json!({
            "goal_id": params.goal_id,
            "label": params.label,
            "unit": params.unit,
            "target": params.target,
            "cadence": params.cadence,
            "kind": params.kind,
            "aggregation": aggregation,
            "require_gate": require_gate,
            "weight": weight,
        }),
        description,
        target_uri: format!("wayfinder://kpi-metrics/proposed/{action_id}"),
        result: serde_json::json!({}),
    })
}

pub fn handle_propose_kpi_datapoint_add(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeKpiDatapointAddParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::KpiDatapointAdd;
    let goal_id = parse_goal_id(kind, &params.goal_run_id)?;
    validate_id_or_ref(
        state,
        kind,
        "metric_id",
        &params.metric_id,
        goal_id,
        ProposeKind::KpiMetricCreate,
    )?;
    validate_number(kind, "value", params.value, false, false)?;
    let source = params.source.as_deref().unwrap_or(DEFAULT_DATAPOINT_SOURCE);
    validate_slug(kind, "source", source, MAX_SLUG_LEN)?;

    let description = format!(
        "Propose recording datapoint {} on KPI metric {} (source {source})",
        params.value, params.metric_id
    );
    capture(state, kind, &params.goal_run_id, |_, _| Proposal {
        parameters: serde_json::json!({
            "metric_id": params.metric_id,
            "value": params.value,
            "source": source,
        }),
        description,
        target_uri: format!("wayfinder://kpi-metrics/{}/datapoints", params.metric_id),
        result: serde_json::json!({}),
    })
}

pub fn handle_propose_kpi_create(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeKpiCreateParams,
) -> Result<CallToolResult, McpError> {
    let kind = ProposeKind::KpiCreate;
    validate_text(kind, "label", &params.label, MAX_LABEL_LEN, true, true)?;
    validate_opt_description(kind, "description", params.description.as_deref())?;
    let weight = params.weight.unwrap_or(1.0);
    validate_number(kind, "weight", weight, false, true)?;
    if let Some(r) = &params.source_ref {
        validate_text(kind, "source_ref", r, MAX_ID_LEN * 2, true, true)?;
    }
    let org_wide = params.org_wide.unwrap_or(false);

    let description = format!(
        "Propose creating {} KPI \"{}\" (weight {weight})",
        if org_wide { "org-wide" } else { "project" },
        params.label
    );
    capture(state, kind, &params.goal_run_id, |_, action_id| Proposal {
        parameters: serde_json::json!({
            "label": params.label,
            "description": params.description.clone().unwrap_or_default(),
            "weight": weight,
            "source_ref": params.source_ref,
            "org_wide": org_wide,
        }),
        description,
        target_uri: format!("wayfinder://kpis/proposed/{action_id}"),
        result: serde_json::json!({}),
    })
}

/// The reviewer-facing lines for one captured goal / KPI / datapoint
/// proposal in `ta draft view`. `index` is the action's position in the
/// draft (its `n` in `ref:<n>` and in the `ta_draft:<draft_id>:<n>`
/// idempotency key); `draft_id` is the draft's id. Empty for other tools.
pub fn review_lines(
    tool_name: &str,
    params: &serde_json::Value,
    index: usize,
    draft_id: &str,
) -> Vec<String> {
    let Some(kind) = ProposeKind::from_tool_name(tool_name).filter(|k| k.is_planning()) else {
        return Vec::new();
    };
    let s = |k: &str| params.get(k).and_then(|v| v.as_str());
    let n = |k: &str| params.get(k).and_then(|v| v.as_f64());
    let mut lines = Vec::new();
    match kind {
        ProposeKind::GoalCreate => {
            lines.push(format!("Goal:    {}", s("name").unwrap_or("?")));
            lines.push(format!(
                "Kind:    {}",
                s("kind").unwrap_or(DEFAULT_GOAL_KIND)
            ));
            if let Some(p) = s("parent_goal_id") {
                lines.push(format!("Parent:  {p}"));
            }
            if let Some(d) = s("target_date") {
                lines.push(format!("Target:  {d}"));
            }
            if let Some(d) = s("description").filter(|d| !d.trim().is_empty()) {
                lines.push(format!("About:   {}", d.trim()));
            }
        }
        ProposeKind::KpiMetricCreate => {
            lines.push(format!("On goal: {}", s("goal_id").unwrap_or("?")));
            lines.push(format!("Metric:  {}", s("label").unwrap_or("?")));
            lines.push(format!(
                "Target:  {} {} ({}, {}, {})",
                n("target").unwrap_or(f64::NAN),
                s("unit").unwrap_or("?"),
                s("cadence").unwrap_or("?"),
                s("kind").unwrap_or("?"),
                s("aggregation").unwrap_or("sum"),
            ));
            lines.push(format!(
                "Weight:  {}{}",
                n("weight").unwrap_or(1.0),
                if params.get("require_gate").and_then(|v| v.as_bool()) == Some(true) {
                    ", requires the goal gate"
                } else {
                    ""
                }
            ));
            if s("kind") == Some("task_count") {
                lines.push(
                    "Note:    task_count metrics are counted from tasks; Wayfinder refuses \
                     datapoints on them."
                        .to_string(),
                );
            }
        }
        ProposeKind::KpiDatapointAdd => {
            lines.push(format!("Metric:  {}", s("metric_id").unwrap_or("?")));
            lines.push(format!(
                "Value:   {} (source {})",
                n("value").unwrap_or(f64::NAN),
                s("source").unwrap_or(DEFAULT_DATAPOINT_SOURCE)
            ));
        }
        ProposeKind::KpiCreate => {
            lines.push(format!("KPI:     {}", s("label").unwrap_or("?")));
            lines.push(format!(
                "Scope:   {}",
                if params.get("org_wide").and_then(|v| v.as_bool()) == Some(true) {
                    "org-wide"
                } else {
                    "this project"
                }
            ));
            lines.push(format!("Weight:  {}", n("weight").unwrap_or(1.0)));
            if let Some(d) = s("description").filter(|d| !d.trim().is_empty()) {
                lines.push(format!("About:   {}", d.trim()));
            }
        }
        _ => {}
    }
    if kind.is_referenceable() {
        lines.push(format!(
            "Ref:     ref:{index} (later proposals in this draft may point at this)"
        ));
    }
    lines.push(format!(
        "Key:     {}",
        planning_external_id(draft_id, index)
    ));
    lines
}

/// The idempotency key replay sends for the planning proposal at `index` in
/// draft `draft_id`. One definition, shared by replay and the draft view.
pub fn planning_external_id(draft_id: &str, index: usize) -> String {
    format!("ta_draft:{draft_id}:{index}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GatewayConfig;

    fn make_state(root: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let mut config = GatewayConfig::for_project(root);
        config.credential_vault_use_keychain = false;
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    fn only_pending(state: &Arc<Mutex<GatewayState>>, goal_id: Uuid) -> PendingAction {
        let guard = state.lock().unwrap();
        let pending = guard
            .pending_actions
            .get(&goal_id)
            .expect("pending action should be stored under this goal");
        assert_eq!(pending.len(), 1);
        let p = pending[0].clone();
        assert_eq!(p.kind, ActionKind::StateChanging);
        assert_eq!(p.disposition, ArtifactDisposition::Pending);
        p
    }

    fn update(task_id: &str, title: Option<&str>, goal: &str) -> ProposeTaskUpdateParams {
        ProposeTaskUpdateParams {
            task_id: task_id.into(),
            title: title.map(Into::into),
            description: None,
            goal_run_id: goal.into(),
        }
    }

    fn create(title: &str, verb: &str, goal: Uuid) -> ProposeTaskCreateParams {
        ProposeTaskCreateParams {
            title: title.into(),
            verb: verb.into(),
            description: None,
            external_id: None,
            assignee_id: None,
            goal_run_id: goal.to_string(),
        }
    }

    fn on_hold(task_id: &str, reason: &str, goal: Uuid) -> ProposeTaskOnHoldParams {
        ProposeTaskOnHoldParams {
            task_id: task_id.into(),
            hold_reason: reason.into(),
            blocking_task: None,
            goal_run_id: goal.to_string(),
        }
    }

    // ── ProposeKind ──

    #[test]
    fn propose_kind_tool_names_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in ProposeKind::ALL {
            assert!(seen.insert(k.tool_name()), "duplicate {}", k.tool_name());
            assert!(k.tool_name().starts_with("ta_propose_"));
            assert_eq!(ProposeKind::from_tool_name(k.tool_name()), Some(k));
        }
        assert_eq!(ProposeKind::from_tool_name("ta_external_action"), None);
        assert_eq!(TOOL_NAME, "ta_propose_task_update");
    }

    // ── update ──

    #[test]
    fn update_captures_a_pending_action_keyed_on_the_goal() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_id = Uuid::new_v4();

        let result = handle_propose_task_update(
            &state,
            update("t1", Some("Revised title"), &goal_id.to_string()),
        )
        .unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let p = only_pending(&state, goal_id);
        assert_eq!(p.tool_name, TOOL_NAME);
        assert_eq!(p.target_uri.as_deref(), Some("wayfinder://tasks/t1"));
        assert_eq!(p.parameters["task_id"], "t1");
        assert_eq!(p.parameters["title"], "Revised title");
        assert!(p.description.contains("Revised title"));
    }

    #[test]
    fn update_rejects_a_proposal_with_neither_title_nor_description() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let err =
            handle_propose_task_update(&state, update("t1", None, &Uuid::new_v4().to_string()))
                .unwrap_err();
        assert!(err.message.contains("at least one of title/description"));
    }

    #[test]
    fn update_rejects_a_malformed_goal_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let err = handle_propose_task_update(&state, update("t1", Some("New"), "not-a-uuid"))
            .unwrap_err();
        assert!(err.message.contains("not a valid UUID"));
    }

    #[test]
    fn update_rejects_bad_task_ids_and_titles() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let g = Uuid::new_v4().to_string();
        for bad in ["", "a/b", "a b", "t\n1", &"x".repeat(MAX_ID_LEN + 1)] {
            let err = handle_propose_task_update(&state, update(bad, Some("ok"), &g)).unwrap_err();
            assert!(err.message.contains("task_id"), "{bad:?}: {}", err.message);
        }
        for bad in ["", "   ", "line1\nline2", "bell\u{7}"] {
            let err = handle_propose_task_update(&state, update("t1", Some(bad), &g)).unwrap_err();
            assert!(err.message.contains("title"), "{bad:?}: {}", err.message);
        }
        assert!(state.lock().unwrap().pending_actions.is_empty());
    }

    #[test]
    fn two_proposals_for_different_goals_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_a = Uuid::new_v4();
        let goal_b = Uuid::new_v4();
        handle_propose_task_update(&state, update("t1", Some("A"), &goal_a.to_string())).unwrap();
        handle_propose_task_update(&state, update("t2", Some("B"), &goal_b.to_string())).unwrap();
        let guard = state.lock().unwrap();
        assert_eq!(guard.pending_actions.get(&goal_a).unwrap().len(), 1);
        assert_eq!(guard.pending_actions.get(&goal_b).unwrap().len(), 1);
    }

    // ── create ──

    #[test]
    fn create_derives_a_deterministic_external_id_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_id = Uuid::new_v4();
        handle_propose_task_create(&state, create("Write docs", "implement", goal_id)).unwrap();

        let p = only_pending(&state, goal_id);
        assert_eq!(p.tool_name, "ta_propose_task_create");
        assert_eq!(
            p.parameters["external_id"],
            derived_external_id(goal_id, p.action_id, None)
        );
        assert_eq!(p.parameters["title"], "Write docs");
        assert_eq!(p.parameters["verb"], "implement");
        assert!(p.parameters["assignee_id"].is_null());
        assert!(p.description.contains("Write docs"));
    }

    #[test]
    fn create_keeps_a_caller_supplied_external_id_and_assignee() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_id = Uuid::new_v4();
        let mut params = create("Write docs", "implement", goal_id);
        params.external_id = Some("intake:msg-42".into());
        params.assignee_id = Some("role-dev".into());
        handle_propose_task_create(&state, params).unwrap();
        let p = only_pending(&state, goal_id);
        assert_eq!(p.parameters["external_id"], "intake:msg-42");
        assert_eq!(p.parameters["assignee_id"], "role-dev");
        assert_eq!(
            p.target_uri.as_deref(),
            Some("wayfinder://tasks/external/intake:msg-42")
        );
    }

    #[test]
    fn create_rejects_empty_title_missing_verb_and_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let g = Uuid::new_v4();
        let err = handle_propose_task_create(&state, create(" ", "implement", g)).unwrap_err();
        assert!(err.message.contains("title must be non-empty"));
        let err = handle_propose_task_create(&state, create("T", "", g)).unwrap_err();
        assert!(err.message.contains("verb must be non-empty"));
        let err = handle_propose_task_create(&state, create("T", "do it", g)).unwrap_err();
        assert!(err.message.contains("verb"));
        let mut p = create("T", "implement", g);
        p.external_id = Some("has space".into());
        assert!(handle_propose_task_create(&state, p)
            .unwrap_err()
            .message
            .contains("external_id"));
        let mut p = create("T", "implement", g);
        p.assignee_id = Some("".into());
        assert!(handle_propose_task_create(&state, p)
            .unwrap_err()
            .message
            .contains("assignee_id"));
        let mut p = create("T", "implement", g);
        p.description = Some("x".repeat(MAX_DESCRIPTION_LEN + 1));
        assert!(handle_propose_task_create(&state, p)
            .unwrap_err()
            .message
            .contains("description"));
        assert!(state.lock().unwrap().pending_actions.is_empty());
    }

    // ── reassign ──

    #[test]
    fn reassign_distinguishes_missing_from_null_assignee() {
        let goal = Uuid::new_v4();
        let missing: ProposeTaskReassignParams = serde_json::from_value(serde_json::json!({
            "task_id": "t1", "goal_run_id": goal.to_string()
        }))
        .unwrap();
        assert_eq!(missing.assignee_id, None);
        let null: ProposeTaskReassignParams = serde_json::from_value(serde_json::json!({
            "task_id": "t1", "assignee_id": null, "goal_run_id": goal.to_string()
        }))
        .unwrap();
        assert_eq!(null.assignee_id, Some(None));

        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let err = handle_propose_task_reassign(&state, missing).unwrap_err();
        assert!(err.message.contains("assignee_id is required"));

        handle_propose_task_reassign(&state, null).unwrap();
        let p = only_pending(&state, goal);
        assert_eq!(p.tool_name, "ta_propose_task_reassign");
        assert!(p.parameters["assignee_id"].is_null());
        assert!(p.description.contains("clearing the assignee"));
    }

    #[test]
    fn reassign_captures_the_roster_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        handle_propose_task_reassign(
            &state,
            ProposeTaskReassignParams {
                task_id: "t1".into(),
                assignee_id: Some(Some("role-qa".into())),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap();
        let p = only_pending(&state, goal);
        assert_eq!(p.parameters["task_id"], "t1");
        assert_eq!(p.parameters["assignee_id"], "role-qa");

        let err = handle_propose_task_reassign(
            &state,
            ProposeTaskReassignParams {
                task_id: "t1".into(),
                assignee_id: Some(Some("Jane Doe".into())),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap_err();
        assert!(err.message.contains("assignee_id"));
    }

    // ── needs-revision ──

    #[test]
    fn needs_revision_captures_task_and_optional_reason() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        handle_propose_task_needs_revision(
            &state,
            ProposeTaskNeedsRevisionParams {
                task_id: "t1".into(),
                reason: Some("tests missing".into()),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap();
        let p = only_pending(&state, goal);
        assert_eq!(p.tool_name, "ta_propose_task_needs_revision");
        assert_eq!(p.parameters["task_id"], "t1");
        assert!(p.description.contains("needs-revision"));
        assert!(p.description.contains("tests missing"));

        let err = handle_propose_task_needs_revision(
            &state,
            ProposeTaskNeedsRevisionParams {
                task_id: "t1".into(),
                reason: Some("bad\u{1b}[31m".into()),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap_err();
        assert!(err.message.contains("control character"));
    }

    // ── on-hold ──

    #[test]
    fn on_hold_requires_a_non_empty_hold_reason() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let g = Uuid::new_v4();
        for bad in ["", "  \n "] {
            let err = handle_propose_task_on_hold(&state, on_hold("t1", bad, g)).unwrap_err();
            assert!(err.message.contains("hold_reason must be non-empty"));
        }
        assert!(state.lock().unwrap().pending_actions.is_empty());
    }

    #[test]
    fn on_hold_with_blocking_task_derives_a_precursor_external_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        let mut params = on_hold("t1", "waiting on legal", goal);
        params.blocking_task = Some(BlockingTaskSpec {
            title: "Get legal sign-off".into(),
            verb: "review".into(),
            description: None,
        });
        handle_propose_task_on_hold(&state, params).unwrap();
        let p = only_pending(&state, goal);
        assert_eq!(p.tool_name, "ta_propose_task_on_hold");
        assert_eq!(p.parameters["hold_reason"], "waiting on legal");
        assert_eq!(
            p.parameters["blocking_task"]["external_id"],
            derived_external_id(goal, p.action_id, Some("precursor"))
        );
        assert!(p.description.contains("Get legal sign-off"));

        let mut bad = on_hold("t1", "x", goal);
        bad.blocking_task = Some(BlockingTaskSpec {
            title: "T".into(),
            verb: "".into(),
            description: None,
        });
        assert!(handle_propose_task_on_hold(&state, bad)
            .unwrap_err()
            .message
            .contains("blocking_task.verb"));
    }

    // ── complete ──

    #[test]
    fn complete_captures_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        handle_propose_task_complete(
            &state,
            ProposeTaskCompleteParams {
                task_id: "wf-9".into(),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap();
        let p = only_pending(&state, goal);
        assert_eq!(p.tool_name, "ta_propose_task_complete");
        assert_eq!(p.parameters["task_id"], "wf-9");
        let err = handle_propose_task_complete(
            &state,
            ProposeTaskCompleteParams {
                task_id: "".into(),
                goal_run_id: goal.to_string(),
            },
        )
        .unwrap_err();
        assert!(err.message.contains("task_id must be non-empty"));
    }

    // ── nothing executes live ──

    /// Points a real `[plan] backend = "wayfinder"` config at a local TCP
    /// listener and calls every tool: the listener must see zero
    /// connections, proving capture never reaches Wayfinder.
    #[test]
    fn no_tool_performs_a_live_wayfinder_call() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            format!(
                "[plan]\nbackend = \"wayfinder\"\n\n[plan.wayfinder]\n\
                 base_url = \"http://{addr}\"\norg_id = \"org-1\"\nproject_id = \"proj-1\"\n\
                 credential_name = \"wayfinder-service-account\"\n"
            ),
        )
        .unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        let g = goal.to_string();

        handle_propose_task_update(&state, update("t1", Some("T"), &g)).unwrap();
        handle_propose_task_create(&state, create("New", "implement", goal)).unwrap();
        handle_propose_task_reassign(
            &state,
            ProposeTaskReassignParams {
                task_id: "t1".into(),
                assignee_id: Some(Some("r1".into())),
                goal_run_id: g.clone(),
            },
        )
        .unwrap();
        handle_propose_task_needs_revision(
            &state,
            ProposeTaskNeedsRevisionParams {
                task_id: "t1".into(),
                reason: None,
                goal_run_id: g.clone(),
            },
        )
        .unwrap();
        let mut hold = on_hold("t1", "blocked", goal);
        hold.blocking_task = Some(BlockingTaskSpec {
            title: "Precursor".into(),
            verb: "implement".into(),
            description: None,
        });
        handle_propose_task_on_hold(&state, hold).unwrap();
        handle_propose_task_complete(
            &state,
            ProposeTaskCompleteParams {
                task_id: "t1".into(),
                goal_run_id: g,
            },
        )
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a ta_propose_task_* tool connected to Wayfinder at capture time"
        );
        let guard = state.lock().unwrap();
        let pending = guard.pending_actions.get(&goal).unwrap();
        assert_eq!(pending.len(), 6);
        let names: std::collections::HashSet<_> =
            pending.iter().map(|p| p.tool_name.as_str()).collect();
        assert_eq!(names.len(), 6);
        assert!(pending
            .iter()
            .all(|p| p.disposition == ArtifactDisposition::Pending));
    }

    // ── goals, KPI metrics, datapoints, org KPIs ──

    fn goal_p(name: &str, goal: Uuid) -> ProposeGoalCreateParams {
        ProposeGoalCreateParams {
            name: name.into(),
            description: None,
            kind: None,
            parent_goal_id: None,
            target_date: None,
            goal_run_id: goal.to_string(),
        }
    }

    fn metric_p(goal_id: &str, goal: Uuid) -> ProposeKpiMetricCreateParams {
        ProposeKpiMetricCreateParams {
            goal_id: goal_id.into(),
            label: "Signups".into(),
            unit: "users".into(),
            target: 100.0,
            cadence: "weekly".into(),
            kind: "external".into(),
            aggregation: None,
            require_gate: None,
            weight: None,
            goal_run_id: goal.to_string(),
        }
    }

    fn datapoint_p(metric_id: &str, value: f64, goal: Uuid) -> ProposeKpiDatapointAddParams {
        ProposeKpiDatapointAddParams {
            metric_id: metric_id.into(),
            value,
            source: None,
            goal_run_id: goal.to_string(),
        }
    }

    fn kpi_p(label: &str, goal: Uuid) -> ProposeKpiCreateParams {
        ProposeKpiCreateParams {
            label: label.into(),
            description: None,
            weight: None,
            source_ref: None,
            org_wide: None,
            goal_run_id: goal.to_string(),
        }
    }

    fn captured(state: &Arc<Mutex<GatewayState>>, goal: Uuid) -> usize {
        state
            .lock()
            .unwrap()
            .pending_actions
            .get(&goal)
            .map_or(0, Vec::len)
    }

    fn result_text(r: &CallToolResult) -> String {
        serde_json::to_string(r).unwrap()
    }

    #[test]
    fn planning_tools_capture_pending_actions_and_goal_and_metric_return_a_ref() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();

        let r = handle_propose_goal_create(&state, goal_p("Launch", goal)).unwrap();
        assert!(result_text(&r).contains("ref:0"), "{}", result_text(&r));
        let r = handle_propose_kpi_metric_create(&state, metric_p("ref:0", goal)).unwrap();
        assert!(result_text(&r).contains("ref:1"), "{}", result_text(&r));
        let r = handle_propose_kpi_datapoint_add(&state, datapoint_p("ref:1", 3.5, goal)).unwrap();
        assert!(
            !result_text(&r).contains("\\\"ref\\\""),
            "datapoints are not referenceable"
        );
        handle_propose_kpi_create(&state, kpi_p("NPS", goal)).unwrap();

        let g = state.lock().unwrap();
        let acts = &g.pending_actions[&goal];
        let names: Vec<&str> = acts.iter().map(|a| a.tool_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ta_propose_goal_create",
                "ta_propose_kpi_metric_create",
                "ta_propose_kpi_datapoint_add",
                "ta_propose_kpi_create"
            ]
        );
        for a in acts {
            assert_eq!(a.kind, ActionKind::StateChanging);
            assert_eq!(a.disposition, ArtifactDisposition::Pending);
            // The draft id is unknown at capture, so no key is stored yet.
            assert!(a.parameters.get("external_id").is_none());
        }
        assert_eq!(acts[1].parameters["goal_id"], "ref:0");
    }

    #[test]
    fn invalid_planning_input_is_refused_and_captures_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        let long = "x".repeat(MAX_LABEL_LEN + 1);

        let mut bad: Vec<Result<CallToolResult, McpError>> = Vec::new();
        bad.push(handle_propose_goal_create(&state, goal_p("", goal)));
        bad.push(handle_propose_goal_create(
            &state,
            goal_p(&"x".repeat(MAX_TITLE_LEN + 1), goal),
        ));
        let mut p = goal_p("G", goal);
        p.kind = Some("Not A Slug".into());
        bad.push(handle_propose_goal_create(&state, p));
        let mut p = goal_p("G", goal);
        p.target_date = Some("2026-13-40".into());
        bad.push(handle_propose_goal_create(&state, p));
        let mut p = goal_p("G", goal);
        p.target_date = Some("next week".into());
        bad.push(handle_propose_goal_create(&state, p));

        for v in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e12 + 1.0] {
            let mut p = metric_p("goal-1", goal);
            p.target = v;
            bad.push(handle_propose_kpi_metric_create(&state, p));
        }
        for (field, value) in [
            ("cadence", "hourly"),
            ("kind", "magic"),
            ("aggregation", "avg"),
        ] {
            let mut p = metric_p("goal-1", goal);
            match field {
                "cadence" => p.cadence = value.into(),
                "kind" => p.kind = value.into(),
                _ => p.aggregation = Some(value.into()),
            }
            bad.push(handle_propose_kpi_metric_create(&state, p));
        }
        let mut p = metric_p("goal-1", goal);
        p.unit = "u".repeat(MAX_UNIT_LEN + 1);
        bad.push(handle_propose_kpi_metric_create(&state, p));
        let mut p = metric_p("goal-1", goal);
        p.weight = Some(f64::NAN);
        bad.push(handle_propose_kpi_metric_create(&state, p));

        for v in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e12 + 1.0,
            -1e12 - 1.0,
        ] {
            bad.push(handle_propose_kpi_datapoint_add(
                &state,
                datapoint_p("m-1", v, goal),
            ));
        }
        bad.push(handle_propose_kpi_create(&state, kpi_p("", goal)));
        bad.push(handle_propose_kpi_create(&state, kpi_p(&long, goal)));

        // Bad goal_run_id.
        let mut p = goal_p("G", goal);
        p.goal_run_id = "not-a-uuid".into();
        bad.push(handle_propose_goal_create(&state, p));

        for (i, r) in bad.iter().enumerate() {
            assert!(r.is_err(), "case {i} should have been refused");
        }
        assert_eq!(captured(&state, goal), 0);
    }

    #[test]
    fn extreme_but_in_range_values_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        handle_propose_kpi_datapoint_add(&state, datapoint_p("m-1", 1e12, goal)).unwrap();
        handle_propose_kpi_datapoint_add(&state, datapoint_p("m-1", -1e12, goal)).unwrap();
        handle_propose_kpi_datapoint_add(&state, datapoint_p("m-1", 0.0, goal)).unwrap();
        assert_eq!(captured(&state, goal), 3);
    }

    #[test]
    fn refs_must_name_an_earlier_action_of_the_right_kind_in_this_goal_run() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();

        // Nothing captured yet.
        assert!(handle_propose_kpi_metric_create(&state, metric_p("ref:0", goal)).is_err());
        handle_propose_goal_create(&state, goal_p("Launch", goal)).unwrap();
        // Wrong kind: a datapoint needs a metric, ref:0 is a goal.
        let e = handle_propose_kpi_datapoint_add(&state, datapoint_p("ref:0", 1.0, goal))
            .unwrap_err()
            .message
            .to_string();
        assert!(e.contains("ta_propose_kpi_metric_create"), "{e}");
        // Malformed.
        for bad in ["ref:", "ref:x", "ref:-1", "ref:1.5", "ref:9999999"] {
            assert!(
                handle_propose_kpi_metric_create(&state, metric_p(bad, goal)).is_err(),
                "{bad}"
            );
        }
        // Another goal run's actions are not visible.
        let other = Uuid::new_v4();
        assert!(handle_propose_kpi_metric_create(&state, metric_p("ref:0", other)).is_err());
        // The valid reference works, and a parent_goal_id ref too.
        handle_propose_kpi_metric_create(&state, metric_p("ref:0", goal)).unwrap();
        let mut p = goal_p("Child", goal);
        p.parent_goal_id = Some("ref:0".into());
        handle_propose_goal_create(&state, p).unwrap();
        assert_eq!(captured(&state, goal), 3);
    }

    #[test]
    fn planning_capture_makes_no_backend_call() {
        // No workflow.toml, no vault entry: capture must not need either.
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal = Uuid::new_v4();
        handle_propose_goal_create(&state, goal_p("G", goal)).unwrap();
        handle_propose_kpi_create(&state, kpi_p("K", goal)).unwrap();
        assert!(!dir.path().join(".ta/workflow.toml").exists());
    }

    #[test]
    fn planning_kinds_are_classified() {
        for k in [
            ProposeKind::GoalCreate,
            ProposeKind::KpiMetricCreate,
            ProposeKind::KpiDatapointAdd,
            ProposeKind::KpiCreate,
        ] {
            assert!(k.is_planning());
        }
        assert!(ProposeKind::GoalCreate.is_referenceable());
        assert!(ProposeKind::KpiMetricCreate.is_referenceable());
        assert!(!ProposeKind::KpiDatapointAdd.is_referenceable());
        assert!(!ProposeKind::KpiCreate.is_referenceable());
        assert!(!ProposeKind::Create.is_planning());
    }

    #[test]
    fn review_lines_render_each_planning_proposal_readably() {
        let goal = review_lines(
            "ta_propose_goal_create",
            &serde_json::json!({"name": "Launch", "kind": "milestone", "target_date": "2026-12-01"}),
            0,
            "d-1",
        )
        .join("\n");
        assert!(goal.contains("Launch") && goal.contains("2026-12-01"));
        assert!(goal.contains("ref:0") && goal.contains("ta_draft:d-1:0"));

        let metric = review_lines(
            "ta_propose_kpi_metric_create",
            &serde_json::json!({"goal_id": "ref:0", "label": "Signups", "unit": "users",
                "target": 100.0, "cadence": "weekly", "kind": "task_count"}),
            1,
            "d-1",
        )
        .join("\n");
        assert!(metric.contains("ref:0") && metric.contains("Signups") && metric.contains("100"));
        assert!(metric.contains("refuses datapoints"), "{metric}");

        let dp = review_lines(
            "ta_propose_kpi_datapoint_add",
            &serde_json::json!({"metric_id": "ref:1", "value": 7.5}),
            2,
            "d-1",
        )
        .join("\n");
        assert!(dp.contains("ref:1") && dp.contains("7.5") && dp.contains("ta_draft:d-1:2"));
        assert!(!dp.contains("Ref:"));

        let kpi = review_lines(
            "ta_propose_kpi_create",
            &serde_json::json!({"label": "NPS", "org_wide": true}),
            3,
            "d-1",
        )
        .join("\n");
        assert!(kpi.contains("NPS") && kpi.contains("org-wide"));

        assert!(review_lines("ta_propose_task_update", &serde_json::json!({}), 0, "d").is_empty());
    }
}
