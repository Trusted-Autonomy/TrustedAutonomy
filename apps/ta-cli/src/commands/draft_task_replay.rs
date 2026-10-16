//! Apply-time replay of the draft-bundled Wayfinder task proposals
//! (`ta_propose_task_*`, see `ta_mcp_gateway::tools::wayfinder_task`).
//!
//! Per the CoS read-only chat-mode design
//! (`docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md`,
//! "Task mutation" and "Dispatched" item 4): no agent ever mutates a
//! Wayfinder task live. A worker's task proposals are captured as
//! `PendingAction`s in its own draft, reviewed together with the rest of the
//! draft, and only replayed here, after `ta draft apply` has actually
//! applied that draft.
//!
//! Safety properties:
//! - Only a draft whose status is `Applied` is replayed (the caller also
//!   skips dry runs). Actions the reviewer marked `Rejected` or `Discuss`
//!   are skipped.
//! - Idempotent: each successfully replayed `action_id` is appended to a
//!   local ledger (`.ta/wayfinder-task-replay.log`) and skipped on any later
//!   replay. Independently of the ledger, creates always carry an
//!   `external_id` (Wayfinder upserts on it), status/assignee updates are
//!   naturally idempotent, and a duplicate dependency edge (409) is treated
//!   as success, so even a lost ledger cannot double-create a task.
//! - One action failing never blocks the others; every failure is logged
//!   with structured fields (`action_id`, `tool_name`, `task_id`, `error`)
//!   and printed with a next step.
//! - A project with no `[plan] backend = "wayfinder"` is skipped with a log
//!   line, never an error: the rest of the draft has already applied.
//!
//! Goal / KPI proposals (`ta_propose_goal_create`, `ta_propose_kpi_*`) take
//! the same path with three additions:
//! - every create carries `external_id = "ta_draft:<draft_id>:<n>"`, `n`
//!   being the action's index in the draft, so a re-apply upserts;
//! - a proposal may name a goal or metric created earlier in the same draft
//!   as `ref:<n>`; replay runs in declared order and resolves it from the id
//!   the earlier action returned, which the ledger records so the reference
//!   still resolves on a re-apply after a partial failure;
//! - a step that fails (or was rejected by the reviewer) stops the steps that
//!   depend on it, and the message says which.
//!
//! Reviewed wiki writes (`ta_wiki_create` / `ta_wiki_update`, see
//! `ta_mcp_gateway::tools::wiki`) take this same path, with the same
//! gating, reviewer dispositions and ledger. They use the wiki write
//! credential, resolved here at apply time only.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use ta_changeset::draft_package::{ArtifactDisposition, DraftPackage, DraftStatus, PendingAction};
use ta_mcp_gateway::tools::wayfinder_task::{self as wt, ProposeKind};
use ta_mcp_gateway::tools::wiki;
use ta_mcp_gateway::GatewayConfig;
use ta_plan_wayfinder::{
    AddKpiDatapointRequest, CreateGoalRequest, CreateKpiMetricRequest, CreateOrgKpiRequest,
    CreateTaskRequest, WayfinderClient, STATUS_DONE, STATUS_ON_HOLD, STATUS_OPEN,
};

/// `created_by` sent with an org-level KPI.
const KPI_CREATED_BY: &str = "trusted-autonomy";

/// Ledger of replayed `action_id`s, one UUID per line, under `.ta/`.
pub(crate) const LEDGER_FILE: &str = "wayfinder-task-replay.log";

/// Outcome counts of one replay pass, printed and returned for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ReplaySummary {
    pub replayed: usize,
    pub already_replayed: usize,
    pub failed: usize,
    /// Goal / KPI steps not attempted because a step they refer to (via
    /// `ref:<n>`) failed, was rejected, or has no recorded id.
    pub blocked: usize,
    pub skipped_by_reviewer: usize,
    /// Set when no Wayfinder backend/config/client was available, so nothing
    /// was attempted.
    pub backend_unavailable: bool,
}

/// The hook `apply_package` calls after a successful, non-dry-run apply.
pub(crate) fn replay_task_proposals(config: &GatewayConfig, pkg: &DraftPackage) {
    if !is_replayable(&pkg.status) {
        tracing::debug!(
            draft_id = %pkg.package_id,
            "not replaying ta_propose_task_* actions: draft is not in Applied status"
        );
        return;
    }
    replay_actions(
        config,
        &pkg.package_id.to_string(),
        &pkg.changes.pending_actions,
    );
}

/// Only an actually-applied draft's proposals may reach Wayfinder.
pub(crate) fn is_replayable(status: &DraftStatus) -> bool {
    matches!(status, DraftStatus::Applied { .. })
}

/// Replays every `ta_propose_task_*` action in `actions`. Actions of other
/// tools are ignored (they are not this module's to replay).
///
/// `draft_id` and each action's index in `actions` form the idempotency key
/// of goal / KPI creates (`ta_draft:<draft_id>:<n>`), so `actions` must be
/// the draft's complete, ordered list.
pub(crate) fn replay_actions(
    config: &GatewayConfig,
    draft_id: &str,
    actions: &[PendingAction],
) -> ReplaySummary {
    let mut summary = replay_task_actions(config, draft_id, actions);
    replay_wiki_actions(config, actions, &mut summary);
    summary
}

fn replay_task_actions(
    config: &GatewayConfig,
    draft_id: &str,
    actions: &[PendingAction],
) -> ReplaySummary {
    let mut summary = ReplaySummary::default();
    let mut ours: Vec<(usize, ProposeKind, &PendingAction)> = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        let Some(kind) = ProposeKind::from_tool_name(&action.tool_name) else {
            continue;
        };
        if matches!(
            action.disposition,
            ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
        ) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                disposition = %action.disposition,
                "not replaying task proposal: reviewer did not approve this action"
            );
            summary.skipped_by_reviewer += 1;
            continue;
        }
        ours.push((index, kind, action));
    }
    if ours.is_empty() {
        return summary;
    }

    let Some(client) = load_client(config, ours.len()) else {
        summary.backend_unavailable = true;
        return summary;
    };

    let ledger_path = config.workspace_root.join(".ta").join(LEDGER_FILE);
    // action_id -> id Wayfinder returned (empty for kinds that return none).
    let mut done = read_ledger_ids(&ledger_path);
    // Index -> why that step cannot supply an id in this pass.
    let mut unavailable: std::collections::HashMap<usize, &'static str> =
        std::collections::HashMap::new();
    for (index, action) in actions.iter().enumerate() {
        if ProposeKind::from_tool_name(&action.tool_name).is_some()
            && matches!(
                action.disposition,
                ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
            )
        {
            unavailable.insert(index, "was not approved by the reviewer");
        }
    }

    for (index, kind, action) in ours {
        let task_id = action
            .parameters
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if done.contains_key(&action.action_id.to_string()) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                task_id = %task_id,
                "skipping task proposal: already replayed against Wayfinder (ledger hit)"
            );
            summary.already_replayed += 1;
            continue;
        }
        let params = match resolve_refs(
            kind,
            index,
            &action.parameters,
            actions,
            &done,
            &unavailable,
        ) {
            Ok(p) => p,
            Err(why) => {
                summary.blocked += 1;
                unavailable.insert(
                    index,
                    "was not applied because a step it depends on was not",
                );
                tracing::warn!(
                    action_id = %action.action_id,
                    tool_name = %action.tool_name,
                    action_index = index,
                    error = %why,
                    "not replaying proposal: a step it refers to did not produce an id"
                );
                eprintln!(
                    "  [blocked] Wayfinder {} (action #{index}, {}): {why}\n           \
                     Fix the step it depends on, then re-run `ta draft apply` for this draft \
                     (steps already applied are remembered and not repeated).",
                    action.tool_name, action.action_id,
                );
                continue;
            }
        };
        match replay_one(&client, kind, &params, &planning_key(draft_id, index)) {
            Ok((what, returned_id)) => {
                println!("  [applied] Wayfinder: {what}");
                summary.replayed += 1;
                if let Err(e) = append_ledger_entry(
                    &ledger_path,
                    &action.action_id.to_string(),
                    returned_id.as_deref(),
                ) {
                    tracing::warn!(
                        action_id = %action.action_id,
                        tool_name = %action.tool_name,
                        path = %ledger_path.display(),
                        error = %e,
                        "replayed task proposal but could not record it in the replay ledger; \
                         a later re-apply may repeat it (safe: creates upsert by external_id)"
                    );
                }
                done.insert(
                    action.action_id.to_string(),
                    returned_id.unwrap_or_default(),
                );
            }
            Err(e) => {
                summary.failed += 1;
                unavailable.insert(index, "failed to apply");
                tracing::warn!(
                    action_id = %action.action_id,
                    tool_name = %action.tool_name,
                    task_id = %task_id,
                    error = %e,
                    "failed to replay approved task proposal against Wayfinder"
                );
                eprintln!(
                    "  [failed] Wayfinder {} for {} (action #{index}, {}): {e}\n           \
                     The rest of the draft is applied. Fix the cause above, then re-run \
                     `ta draft apply` for this draft, or make the change in Wayfinder by hand.",
                    action.tool_name,
                    if !task_id.is_empty() {
                        format!("task {task_id}")
                    } else if kind.is_planning() {
                        "a goal/KPI proposal".to_string()
                    } else {
                        "task <new>".to_string()
                    },
                    action.action_id,
                );
            }
        }
    }

    if summary.replayed + summary.failed + summary.blocked > 0 {
        println!(
            "  Wayfinder task proposals: {} applied, {} failed, {} blocked, {} already applied earlier",
            summary.replayed, summary.failed, summary.blocked, summary.already_replayed
        );
    }
    summary
}

/// Replays every approved `ta_wiki_create` / `ta_wiki_update` action in
/// `actions`, adding to `summary`. Uses the same ledger as task proposals.
fn replay_wiki_actions(
    config: &GatewayConfig,
    actions: &[PendingAction],
    summary: &mut ReplaySummary,
) {
    let ledger_path = config.workspace_root.join(".ta").join(LEDGER_FILE);
    let mut done = read_ledger(&ledger_path);
    let (mut replayed, mut failed, mut already) = (0usize, 0usize, 0usize);

    for action in actions
        .iter()
        .filter(|a| wiki::is_wiki_write_tool(&a.tool_name))
    {
        if matches!(
            action.disposition,
            ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
        ) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                disposition = %action.disposition,
                "not applying wiki write: reviewer did not approve this action"
            );
            summary.skipped_by_reviewer += 1;
            continue;
        }
        if done.contains(&action.action_id.to_string()) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                "skipping wiki write: already applied (ledger hit)"
            );
            already += 1;
            continue;
        }
        match wiki::apply_wiki_action(
            &config.workspace_root,
            config.credential_vault_use_keychain,
            &action.tool_name,
            &action.parameters,
        ) {
            Ok(what) => {
                println!("  [applied] Wayfinder wiki: {what}");
                replayed += 1;
                if let Err(e) = append_ledger(&ledger_path, &action.action_id.to_string()) {
                    tracing::warn!(
                        action_id = %action.action_id,
                        tool_name = %action.tool_name,
                        path = %ledger_path.display(),
                        error = %e,
                        "applied wiki write but could not record it in the replay ledger; \
                         a later re-apply may repeat it (a create would add a second page)"
                    );
                }
                done.insert(action.action_id.to_string());
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    action_id = %action.action_id,
                    tool_name = %action.tool_name,
                    error = %e,
                    "failed to apply approved wiki write against Wayfinder"
                );
                eprintln!(
                    "  [failed] Wayfinder {} (action {}): {e}\n           \
                     The rest of the draft is applied. Fix the cause above (a rejected `if_sha` \
                     means the page changed since it was read: re-read it and propose again), \
                     then re-run `ta draft apply` for this draft, or make the change in Wayfinder \
                     by hand.",
                    action.tool_name, action.action_id,
                );
            }
        }
    }

    summary.replayed += replayed;
    summary.failed += failed;
    summary.already_replayed += already;
    if replayed + failed > 0 {
        println!(
            "  Wayfinder wiki writes: {replayed} applied, {failed} failed, {already} already applied earlier"
        );
    }
}

/// Loads the project's Wayfinder client, or logs why it can't and returns
/// `None`.
fn load_client(config: &GatewayConfig, count: usize) -> Option<WayfinderClient> {
    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);
    if workflow_config.plan.backend != "wayfinder" {
        tracing::warn!(
            count,
            path = %workflow_toml.display(),
            "skipping ta_propose_task_* pending action(s): this project has no \
             [plan] backend = \"wayfinder\" configured, so there is no Wayfinder task to update"
        );
        return None;
    }
    let Some(raw) = workflow_config.plan.wayfinder.as_ref() else {
        tracing::warn!(
            count,
            path = %workflow_toml.display(),
            "skipping ta_propose_task_* pending action(s): [plan] backend = \"wayfinder\" \
             but no [plan.wayfinder] table was found; add it, then re-run `ta draft apply`"
        );
        return None;
    };
    let mut cred_config = ta_credentials::CredentialsConfig::for_project(&config.workspace_root);
    // Respect this gateway's own keychain setting (see `tools/wiki.rs`).
    cred_config.use_keychain = config.credential_vault_use_keychain;
    let wf_config = match ta_plan_wayfinder::WayfinderPlanConfig::load_with_credentials_config(
        raw,
        &cred_config,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(count, error = %e, "skipping ta_propose_task_* pending action(s): failed to load Wayfinder config");
            eprintln!(
                "  [skipped] {count} Wayfinder task proposal(s): could not load Wayfinder config: {e}"
            );
            return None;
        }
    };
    match WayfinderClient::new(&wf_config) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(count, error = %e, "skipping ta_propose_task_* pending action(s): failed to build Wayfinder client");
            None
        }
    }
}

fn str_param<'a>(params: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

fn required<'a>(params: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    str_param(params, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("pending action is missing required parameter `{key}`"))
}

/// The per-kind dispatcher: one match over [`ProposeKind`], each arm the
/// exact Wayfinder call(s) for that outcome. Returns a one-line summary of
/// what changed, plus the id Wayfinder returned for goal / KPI creates (the
/// ledger keeps it so later `ref:<n>` steps resolve on a re-apply).
/// `planning_key` is the `ta_draft:<draft_id>:<n>` idempotency key, used by
/// the goal / KPI arms.
fn replay_one(
    client: &WayfinderClient,
    kind: ProposeKind,
    p: &serde_json::Value,
    planning_key: &str,
) -> anyhow::Result<(String, Option<String>)> {
    let planning = |what: String, id: String| Ok((what, Some(id)));
    match kind {
        ProposeKind::Update => {
            let task_id = required(p, "task_id")?;
            client.update_task_content(
                task_id,
                str_param(p, "title"),
                str_param(p, "description"),
            )?;
            Ok((format!("task {task_id} content updated"), None))
        }
        ProposeKind::Create => {
            let external_id = required(p, "external_id")?;
            let task = client.upsert_task(&CreateTaskRequest {
                title: required(p, "title")?.to_string(),
                description: str_param(p, "description").map(str::to_string),
                verb: required(p, "verb")?.to_string(),
                external_id: Some(external_id.to_string()),
                assignee_id: str_param(p, "assignee_id").map(str::to_string),
            })?;
            Ok((
                format!(
                    "task {} created/upserted (external_id {external_id})",
                    task.id
                ),
                None,
            ))
        }
        ProposeKind::Reassign => {
            let task_id = required(p, "task_id")?;
            // A missing key is a corrupt action, not a request to clear the
            // assignee: only an explicit `null` clears.
            if p.get("assignee_id").is_none() {
                anyhow::bail!(
                    "pending action is missing the `assignee_id` key (use an explicit null to clear the assignee)"
                );
            }
            let assignee = str_param(p, "assignee_id");
            client.update_task_assignee(task_id, assignee)?;
            Ok((
                match assignee {
                    Some(a) => format!("task {task_id} reassigned to {a}"),
                    None => format!("task {task_id} assignee cleared"),
                },
                None,
            ))
        }
        ProposeKind::NeedsRevision => {
            let task_id = required(p, "task_id")?;
            client.update_task_status(task_id, STATUS_OPEN, None)?;
            Ok((format!("task {task_id} marked needs-revision (open)"), None))
        }
        ProposeKind::OnHold => {
            let task_id = required(p, "task_id")?;
            let reason = required(p, "hold_reason")?;
            let mut what = format!("task {task_id} put on hold");
            if let Some(b) = p.get("blocking_task").filter(|b| !b.is_null()) {
                let precursor = client.upsert_task(&CreateTaskRequest {
                    title: required(b, "title")?.to_string(),
                    description: str_param(b, "description").map(str::to_string),
                    verb: required(b, "verb")?.to_string(),
                    external_id: Some(required(b, "external_id")?.to_string()),
                    assignee_id: None,
                })?;
                client.add_dependency(task_id, &precursor.id).map_err(|e| {
                    anyhow::anyhow!(
                        "created precursor task {} but could not record that task {task_id} \
                             depends on it (Wayfinder rejects dependency cycles): {e}",
                        precursor.id
                    )
                })?;
                what.push_str(&format!(", blocked by precursor task {}", precursor.id));
            }
            client.update_task_status(task_id, STATUS_ON_HOLD, Some(reason))?;
            Ok((what, None))
        }
        ProposeKind::Complete => {
            let task_id = required(p, "task_id")?;
            client.update_task_status(task_id, STATUS_DONE, None)?;
            Ok((format!("task {task_id} marked done"), None))
        }
        ProposeKind::GoalCreate => {
            let name = required(p, "name")?;
            let created = client.create_goal(&CreateGoalRequest {
                name: name.to_string(),
                description: str_param(p, "description").map(str::to_string),
                kind: str_param(p, "kind")
                    .unwrap_or(wt::DEFAULT_GOAL_KIND)
                    .to_string(),
                parent_goal_id: str_param(p, "parent_goal_id").map(str::to_string),
                target_date: str_param(p, "target_date").map(str::to_string),
                external_id: Some(planning_key.to_string()),
            })?;
            planning(
                format!(
                    "goal \"{name}\" created/upserted as {} ({planning_key})",
                    created.id
                ),
                created.id,
            )
        }
        ProposeKind::KpiMetricCreate => {
            let goal_id = required(p, "goal_id")?;
            let label = required(p, "label")?;
            let created = client.create_kpi_metric(
                goal_id,
                &CreateKpiMetricRequest {
                    label: label.to_string(),
                    unit: required(p, "unit")?.to_string(),
                    target: number(p, "target")?,
                    cadence: required(p, "cadence")?.to_string(),
                    kind: required(p, "kind")?.to_string(),
                    aggregation: str_param(p, "aggregation").unwrap_or("sum").to_string(),
                    require_gate: p
                        .get("require_gate")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    weight: p.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0),
                    external_id: Some(planning_key.to_string()),
                },
            )?;
            planning(
                format!(
                    "KPI metric \"{label}\" created/upserted as {} on goal {goal_id} ({planning_key})",
                    created.id
                ),
                created.id,
            )
        }
        ProposeKind::KpiDatapointAdd => {
            let metric_id = required(p, "metric_id")?;
            let value = number(p, "value")?;
            let created = client.add_kpi_datapoint(
                metric_id,
                &AddKpiDatapointRequest {
                    value,
                    source: str_param(p, "source")
                        .unwrap_or(wt::DEFAULT_DATAPOINT_SOURCE)
                        .to_string(),
                    external_id: Some(planning_key.to_string()),
                },
            )?;
            planning(
                format!(
                    "datapoint {value} recorded on metric {metric_id} as {} ({planning_key})",
                    created.id
                ),
                created.id,
            )
        }
        ProposeKind::KpiCreate => {
            let label = required(p, "label")?;
            let org_wide = p.get("org_wide").and_then(|v| v.as_bool()).unwrap_or(false);
            let created = client.create_org_kpi(&CreateOrgKpiRequest {
                project_id: (!org_wide).then(|| client.project_id().to_string()),
                label: label.to_string(),
                description: str_param(p, "description").unwrap_or("").to_string(),
                weight: p.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0),
                created_by: KPI_CREATED_BY.to_string(),
                source_ref: str_param(p, "source_ref").map(str::to_string),
                external_id: Some(planning_key.to_string()),
            })?;
            planning(
                format!(
                    "KPI \"{label}\" created/upserted as {} ({planning_key})",
                    created.id
                ),
                created.id,
            )
        }
    }
}

fn number(params: &serde_json::Value, key: &str) -> anyhow::Result<f64> {
    params
        .get(key)
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite())
        .ok_or_else(|| anyhow::anyhow!("pending action is missing numeric parameter `{key}`"))
}

/// `ta_draft:<draft_id>:<n>`: the idempotency key of the planning proposal at
/// index `n` in the draft.
fn planning_key(draft_id: &str, index: usize) -> String {
    wt::planning_external_id(draft_id, index)
}

/// Parameter names that may hold a `ref:<n>` to an earlier proposal, and the
/// kind of proposal each must point at.
const REF_FIELDS: [(ProposeKind, &str, ProposeKind); 3] = [
    (
        ProposeKind::GoalCreate,
        "parent_goal_id",
        ProposeKind::GoalCreate,
    ),
    (
        ProposeKind::KpiMetricCreate,
        "goal_id",
        ProposeKind::GoalCreate,
    ),
    (
        ProposeKind::KpiDatapointAdd,
        "metric_id",
        ProposeKind::KpiMetricCreate,
    ),
];

/// Returns `params` with any `ref:<n>` replaced by the id the earlier action
/// returned (from `done`, the ledger of ids). `Err` names the step it
/// depends on and why that step has no id.
fn resolve_refs(
    kind: ProposeKind,
    index: usize,
    params: &serde_json::Value,
    actions: &[PendingAction],
    done: &std::collections::HashMap<String, String>,
    unavailable: &std::collections::HashMap<usize, &'static str>,
) -> Result<serde_json::Value, String> {
    let mut resolved = params.clone();
    for (field_kind, field, expect) in REF_FIELDS {
        if field_kind != kind {
            continue;
        }
        let Some(value) = params.get(field).and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(n) = value.strip_prefix(wt::REF_PREFIX) else {
            continue;
        };
        let target = n
            .parse::<usize>()
            .ok()
            .filter(|t| n.chars().all(|c| c.is_ascii_digit()) && *t < index)
            .and_then(|t| actions.get(t).map(|a| (t, a)));
        let Some((t, target)) = target else {
            return Err(format!(
                "`{field}` is {value:?}, which does not name an earlier action in this draft \
                 (this is action #{index}; a reference must point at a lower index)"
            ));
        };
        if target.tool_name != expect.tool_name() {
            return Err(format!(
                "`{field}` {value:?} points at action #{t}, a {}, but a {} is required",
                target.tool_name,
                expect.tool_name()
            ));
        }
        match done
            .get(&target.action_id.to_string())
            .filter(|id| !id.is_empty())
        {
            Some(id) => {
                resolved[field] = serde_json::Value::String(id.clone());
            }
            None => {
                let why = unavailable
                    .get(&t)
                    .copied()
                    .unwrap_or("has no recorded id (it was never applied)");
                return Err(format!(
                    "it refers to action #{t} ({}) via `{field}` = {value:?}, and that action {why}",
                    target.tool_name
                ));
            }
        }
    }
    Ok(resolved)
}

/// Ledger of replayed actions: `action_id` per line, optionally followed by a
/// tab and the id Wayfinder returned for it. Older lines have no id.
fn read_ledger_ids(path: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| match l.split_once('\t') {
                    Some((a, id)) => (a.trim().to_string(), id.trim().to_string()),
                    None => (l.to_string(), String::new()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn append_ledger_entry(
    path: &Path,
    action_id: &str,
    returned_id: Option<&str>,
) -> std::io::Result<()> {
    match returned_id {
        Some(id) => append_ledger(path, &format!("{action_id}\t{id}")),
        None => append_ledger(path, action_id),
    }
}

fn read_ledger(path: &Path) -> HashSet<String> {
    read_ledger_ids(path).into_keys().collect()
}

fn append_ledger(path: &Path, action_id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{action_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ta_changeset::draft_package::{ActionKind, ApplyProvenance};
    use uuid::Uuid;

    const TEST_DRAFT: &str = "draft-1";

    /// A `wiremock::MockServer` usable from synchronous tests:
    /// `WayfinderClient` is `reqwest::blocking`, which panics inside an
    /// existing tokio runtime, so the mock server gets its own runtime.
    struct BlockingMockServer {
        runtime: tokio::runtime::Runtime,
        server: wiremock::MockServer,
    }

    impl BlockingMockServer {
        fn start() -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let server = runtime.block_on(wiremock::MockServer::start());
            Self { runtime, server }
        }

        /// Answers every request with a TaskDto whose id is `task_id`.
        fn respond_all_with_task(&self, task_id: &str) {
            self.runtime.block_on(
                wiremock::Mock::given(wiremock::matchers::any())
                    .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({
                            "id": task_id,
                            "status": "open",
                            "hold_reason": null,
                            "external_id": null,
                            "updated_at": "1000000000"
                        }),
                    ))
                    .mount(&self.server),
            );
        }

        /// Answers `POST <route>` with `status` and `body`.
        fn mount_post(&self, route: &str, status: u16, body: serde_json::Value) {
            self.runtime.block_on(
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .and(wiremock::matchers::path(route))
                    .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
                    .mount(&self.server),
            );
        }

        /// Removes every mounted response and the recorded requests.
        fn reset_mocks(&self) {
            self.runtime.block_on(self.server.reset());
        }

        /// (method, path, body) of every request received, in order.
        fn requests(&self) -> Vec<(String, String, serde_json::Value)> {
            self.runtime
                .block_on(self.server.received_requests())
                .unwrap()
                .into_iter()
                .map(|r| {
                    let body = if r.body.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_slice(&r.body).unwrap()
                    };
                    (r.method.to_string(), r.url.path().to_string(), body)
                })
                .collect()
        }
    }

    fn project() -> (tempfile::TempDir, GatewayConfig) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        (dir, config)
    }

    fn write_wayfinder_plan_config(config: &GatewayConfig, base_url: &str) {
        std::fs::create_dir_all(config.workspace_root.join(".ta")).unwrap();
        std::fs::write(
            config.workspace_root.join(".ta/workflow.toml"),
            format!(
                "[plan]\nbackend = \"wayfinder\"\n\n[plan.wayfinder]\n\
                 base_url = \"{base_url}\"\norg_id = \"org-1\"\nproject_id = \"proj-1\"\n\
                 credential_name = \"wayfinder-service-account\"\n"
            ),
        )
        .unwrap();
        let mut cred_config =
            ta_credentials::CredentialsConfig::for_project(&config.workspace_root);
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        use ta_credentials::CredentialVault;
        vault
            .add(
                "wayfinder-service-account",
                "wayfinder",
                "wfsa-test-secret",
                vec![],
            )
            .unwrap();
    }

    fn action(kind: ProposeKind, parameters: serde_json::Value) -> PendingAction {
        PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: kind.tool_name().to_string(),
            parameters,
            kind: ActionKind::StateChanging,
            intercepted_at: Utc::now(),
            description: "test proposal".to_string(),
            target_uri: None,
            disposition: ArtifactDisposition::Pending,
        }
    }

    /// Runs `actions` against a fresh mock and returns what it received.
    fn replay_against_mock(
        response_task_id: &str,
        actions: &[PendingAction],
    ) -> (ReplaySummary, Vec<(String, String, serde_json::Value)>) {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task(response_task_id);
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let summary = replay_actions(&config, TEST_DRAFT, actions);
        (summary, mock.requests())
    }

    fn req(
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (String, String, serde_json::Value) {
        (method.to_string(), path.to_string(), body)
    }

    #[test]
    fn update_replays_as_patch_content() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[action(
                ProposeKind::Update,
                serde_json::json!({"task_id": "wf-1", "title": "Revised", "description": null}),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "PATCH",
                "/api/projects/proj-1/tasks/wf-1/content",
                serde_json::json!({"title": "Revised"})
            )]
        );
    }

    #[test]
    fn create_replays_as_post_tasks_with_external_id_and_verb() {
        let (s, reqs) = replay_against_mock(
            "wf-new",
            &[action(
                ProposeKind::Create,
                serde_json::json!({
                    "title": "Write docs",
                    "verb": "implement",
                    "description": "details",
                    "external_id": "ta-proposal:g:a",
                    "assignee_id": "role-dev",
                }),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "POST",
                "/api/projects/proj-1/tasks",
                serde_json::json!({
                    "title": "Write docs",
                    "description": "details",
                    "verb": "implement",
                    "external_id": "ta-proposal:g:a",
                    "assignee_id": "role-dev",
                })
            )]
        );
    }

    #[test]
    fn reassign_replays_as_patch_assignee_with_assignee_id_or_null() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[
                action(
                    ProposeKind::Reassign,
                    serde_json::json!({"task_id": "wf-1", "assignee_id": "role-qa"}),
                ),
                action(
                    ProposeKind::Reassign,
                    serde_json::json!({"task_id": "wf-2", "assignee_id": null}),
                ),
            ],
        );
        assert_eq!(s.replayed, 2);
        assert_eq!(
            reqs,
            vec![
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-1/assignee",
                    serde_json::json!({"assignee_id": "role-qa"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-2/assignee",
                    serde_json::json!({"assignee_id": null})
                ),
            ]
        );
    }

    #[test]
    fn reassign_with_missing_assignee_id_key_fails_instead_of_clearing() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[action(
                ProposeKind::Reassign,
                serde_json::json!({"task_id": "wf-1"}),
            )],
        );
        assert_eq!(s.replayed, 0);
        assert_eq!(s.failed, 1);
        assert!(reqs.is_empty(), "no Wayfinder call may be made: {reqs:?}");
    }

    #[test]
    fn needs_revision_maps_to_open_and_on_hold_maps_to_on_hold_distinctly() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[
                action(
                    ProposeKind::NeedsRevision,
                    serde_json::json!({"task_id": "wf-1", "reason": "tests missing"}),
                ),
                action(
                    ProposeKind::OnHold,
                    serde_json::json!({
                        "task_id": "wf-2",
                        "hold_reason": "waiting on legal",
                        "blocking_task": null,
                    }),
                ),
            ],
        );
        assert_eq!(s.replayed, 2);
        assert_eq!(
            reqs,
            vec![
                // needs-revision: plain open, no hold_reason, reason not sent.
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-1/status",
                    serde_json::json!({"status": "open"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-2/status",
                    serde_json::json!({"status": "on_hold", "hold_reason": "waiting on legal"})
                ),
            ]
        );
        assert_ne!(reqs[0].2["status"], reqs[1].2["status"]);
    }

    #[test]
    fn on_hold_with_blocking_task_creates_precursor_then_dependency_then_status() {
        let (s, reqs) = replay_against_mock(
            "precursor-1",
            &[action(
                ProposeKind::OnHold,
                serde_json::json!({
                    "task_id": "held-1",
                    "hold_reason": "needs sign-off",
                    "blocking_task": {
                        "title": "Get sign-off",
                        "verb": "review",
                        "description": null,
                        "external_id": "ta-proposal:g:a:precursor",
                    },
                }),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![
                req(
                    "POST",
                    "/api/projects/proj-1/tasks",
                    serde_json::json!({
                        "title": "Get sign-off",
                        "description": null,
                        "verb": "review",
                        "external_id": "ta-proposal:g:a:precursor",
                    })
                ),
                req(
                    "POST",
                    "/api/projects/proj-1/tasks/held-1/dependencies",
                    serde_json::json!({"depends_on_id": "precursor-1"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/held-1/status",
                    serde_json::json!({"status": "on_hold", "hold_reason": "needs sign-off"})
                ),
            ]
        );
    }

    #[test]
    fn complete_replays_as_status_done() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "PATCH",
                "/api/projects/proj-1/tasks/wf-1/status",
                serde_json::json!({"status": "done"})
            )]
        );
    }

    #[test]
    fn re_replaying_the_same_actions_makes_no_further_calls() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task("wf-new");
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let actions = vec![
            action(
                ProposeKind::Create,
                serde_json::json!({"title": "T", "verb": "implement", "external_id": "x:1"}),
            ),
            action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            ),
        ];

        let first = replay_actions(&config, TEST_DRAFT, &actions);
        assert_eq!(first.replayed, 2);
        let second = replay_actions(&config, TEST_DRAFT, &actions);
        assert_eq!(second.replayed, 0);
        assert_eq!(second.already_replayed, 2);
        assert_eq!(
            mock.requests().len(),
            2,
            "second replay must not call Wayfinder"
        );
    }

    #[test]
    fn one_failing_action_does_not_block_the_others_and_is_not_ledgered() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::path(
                "/api/projects/proj-1/tasks/bad/status",
            ))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&mock.server),
        );
        mock.respond_all_with_task("good");
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let failing = action(ProposeKind::Complete, serde_json::json!({"task_id": "bad"}));
        let ok = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "good"}),
        );

        let s = replay_actions(&config, TEST_DRAFT, &[failing.clone(), ok.clone()]);
        assert_eq!(s.failed, 1);
        assert_eq!(s.replayed, 1);
        let ledger = read_ledger(&config.workspace_root.join(".ta").join(LEDGER_FILE));
        assert!(ledger.contains(&ok.action_id.to_string()));
        assert!(!ledger.contains(&failing.action_id.to_string()));
    }

    #[test]
    fn reviewer_rejected_actions_and_other_tools_are_not_replayed() {
        let mut rejected = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "wf-1"}),
        );
        rejected.disposition = ArtifactDisposition::Rejected;
        let mut other = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "wf-2"}),
        );
        other.tool_name = "ta_external_action".to_string();
        let (s, reqs) = replay_against_mock("wf-1", &[rejected, other]);
        assert_eq!(s.skipped_by_reviewer, 1);
        assert_eq!(s.replayed, 0);
        assert!(reqs.is_empty());
    }

    #[test]
    fn a_project_without_a_wayfinder_backend_is_skipped_without_error() {
        // No .ta/workflow.toml at all: the common case.
        let (_dir, config) = project();
        let s = replay_actions(
            &config,
            TEST_DRAFT,
            &[action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            )],
        );
        assert!(s.backend_unavailable);
        assert_eq!(s.replayed + s.failed, 0);
    }

    #[test]
    fn no_task_proposals_means_no_backend_lookup_or_calls() {
        let (_dir, config) = project();
        let s = replay_actions(&config, TEST_DRAFT, &[]);
        assert_eq!(s, ReplaySummary::default());
    }

    #[test]
    fn only_applied_drafts_are_replayable() {
        assert!(is_replayable(&DraftStatus::Applied {
            applied_at: Utc::now(),
            applied_via: ApplyProvenance::Manual,
        }));
        assert!(!is_replayable(&DraftStatus::Draft));
        assert!(!is_replayable(&DraftStatus::PendingReview));
    }

    /// End to end with the real gateway handlers: what each tool captures is
    /// exactly what replay knows how to send.
    #[test]
    fn gateway_captured_actions_replay_cleanly() {
        use std::sync::{Arc, Mutex};
        use ta_mcp_gateway::tools::wayfinder_task as wt;

        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task("wf-x");
        write_wayfinder_plan_config(&config, &mock.server.uri());

        let state = Arc::new(Mutex::new(
            ta_mcp_gateway::server::GatewayState::new(config.clone()).unwrap(),
        ));
        let goal = Uuid::new_v4();
        let g = goal.to_string();
        wt::handle_propose_task_create(
            &state,
            wt::ProposeTaskCreateParams {
                title: "New".into(),
                verb: "implement".into(),
                description: None,
                external_id: None,
                assignee_id: None,
                goal_run_id: g.clone(),
            },
        )
        .unwrap();
        wt::handle_propose_task_on_hold(
            &state,
            wt::ProposeTaskOnHoldParams {
                task_id: "wf-x".into(),
                hold_reason: "blocked".into(),
                blocking_task: Some(wt::BlockingTaskSpec {
                    title: "Pre".into(),
                    verb: "implement".into(),
                    description: None,
                }),
                goal_run_id: g,
            },
        )
        .unwrap();
        // Nothing reached Wayfinder at capture time.
        assert!(mock.requests().is_empty());

        let actions = state.lock().unwrap().pending_actions[&goal].clone();
        let s = replay_actions(&config, TEST_DRAFT, &actions);
        assert_eq!(s.replayed, 2);
        assert_eq!(s.failed, 0);
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 4);
        assert!(reqs[0].2["external_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("ta-proposal:{goal}:")));
    }

    fn wiki_action(tool: &str, disposition: ArtifactDisposition) -> PendingAction {
        PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: tool.to_string(),
            parameters: serde_json::json!({
                "scope": "project", "id": "proj-1", "title": "T", "body": "b",
                "type": null, "tags": [],
            }),
            kind: ActionKind::StateChanging,
            intercepted_at: Utc::now(),
            description: "test wiki write".to_string(),
            target_uri: None,
            disposition,
        }
    }

    #[test]
    fn reviewer_rejected_wiki_writes_are_not_applied() {
        let (_dir, config) = project();
        let actions = [
            wiki_action("ta_wiki_create", ArtifactDisposition::Rejected),
            wiki_action("ta_wiki_update", ArtifactDisposition::Discuss),
        ];
        let s = replay_actions(&config, TEST_DRAFT, &actions);
        assert_eq!(s.skipped_by_reviewer, 2);
        assert_eq!((s.replayed, s.failed), (0, 0));
    }

    #[test]
    fn a_failed_wiki_write_is_counted_and_not_ledgered() {
        // No `.ta/wiki-resources.toml`: the write cannot be sent, and a
        // later re-apply must be able to retry it.
        let (_dir, config) = project();
        let a = wiki_action("ta_wiki_create", ArtifactDisposition::Approved);
        let s = replay_actions(&config, TEST_DRAFT, std::slice::from_ref(&a));
        assert_eq!(s.failed, 1);
        let ledger = read_ledger(&config.workspace_root.join(".ta").join(LEDGER_FILE));
        assert!(!ledger.contains(&a.action_id.to_string()));
    }

    #[test]
    fn a_ledgered_wiki_write_is_not_applied_again() {
        let (_dir, config) = project();
        let a = wiki_action("ta_wiki_update", ArtifactDisposition::Approved);
        append_ledger(
            &config.workspace_root.join(".ta").join(LEDGER_FILE),
            &a.action_id.to_string(),
        )
        .unwrap();
        let s = replay_actions(&config, TEST_DRAFT, std::slice::from_ref(&a));
        assert_eq!(s.already_replayed, 1);
        assert_eq!((s.replayed, s.failed), (0, 0));
    }

    // ---- goals, KPI metrics, datapoints and org KPIs ----

    const GOALS: &str = "/api/projects/proj-1/goals";

    fn goal_action(name: &str) -> PendingAction {
        action(
            ProposeKind::GoalCreate,
            serde_json::json!({"name": name, "description": null, "kind": "milestone",
                "parent_goal_id": null, "target_date": null}),
        )
    }

    fn metric_action(goal_ref: &str, kind: &str) -> PendingAction {
        action(
            ProposeKind::KpiMetricCreate,
            serde_json::json!({"goal_id": goal_ref, "label": "Signups", "unit": "users",
                "target": 100.0, "cadence": "weekly", "kind": kind,
                "aggregation": "sum", "require_gate": false, "weight": 1.0}),
        )
    }

    fn datapoint_action(metric_ref: &str, value: f64) -> PendingAction {
        action(
            ProposeKind::KpiDatapointAdd,
            serde_json::json!({"metric_id": metric_ref, "value": value, "source": "manual"}),
        )
    }

    fn planning_mock(config: &GatewayConfig) -> BlockingMockServer {
        let mock = BlockingMockServer::start();
        mock.mount_post(GOALS, 200, serde_json::json!({"id": "goal-9"}));
        mock.mount_post(
            "/api/projects/proj-1/goals/goal-9/kpi-metrics",
            200,
            serde_json::json!({"id": "metric-9"}),
        );
        mock.mount_post(
            "/api/projects/proj-1/kpi-metrics/metric-9/datapoints",
            200,
            serde_json::json!({"id": "dp-9"}),
        );
        mock.mount_post(
            "/api/orgs/org-1/kpis",
            200,
            serde_json::json!({"id": "kpi-9"}),
        );
        write_wayfinder_plan_config(config, &mock.server.uri());
        mock
    }

    fn ledger(config: &GatewayConfig) -> String {
        std::fs::read_to_string(config.workspace_root.join(".ta").join(LEDGER_FILE))
            .unwrap_or_default()
    }

    #[test]
    fn goal_metric_and_datapoint_replay_in_order_with_refs_and_external_ids() {
        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let actions = [
            goal_action("Launch"),
            metric_action("ref:0", "external"),
            datapoint_action("ref:1", 42.5),
        ];
        let s = replay_actions(&config, "d-7", &actions);
        assert_eq!((s.replayed, s.failed, s.blocked), (3, 0, 0));
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].1, GOALS);
        assert_eq!(
            reqs[0].2,
            serde_json::json!({"name": "Launch", "description": null, "kind": "milestone",
                "parent_goal_id": null, "target_date": null, "external_id": "ta_draft:d-7:0"})
        );
        assert_eq!(reqs[1].1, "/api/projects/proj-1/goals/goal-9/kpi-metrics");
        assert_eq!(
            reqs[1].2,
            serde_json::json!({"label": "Signups", "unit": "users", "target": 100.0,
                "cadence": "weekly", "kind": "external", "aggregation": "sum",
                "require_gate": false, "weight": 1.0, "external_id": "ta_draft:d-7:1"})
        );
        assert_eq!(
            reqs[2].1,
            "/api/projects/proj-1/kpi-metrics/metric-9/datapoints"
        );
        assert_eq!(
            reqs[2].2,
            serde_json::json!({"value": 42.5, "source": "manual",
                "external_id": "ta_draft:d-7:2"})
        );
    }

    #[test]
    fn org_kpi_replays_to_the_org_route_pinned_to_the_project_unless_org_wide() {
        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let pinned = action(
            ProposeKind::KpiCreate,
            serde_json::json!({"label": "NPS", "description": "Promoters", "weight": 2.0,
                "source_ref": null, "org_wide": false}),
        );
        let wide = action(
            ProposeKind::KpiCreate,
            serde_json::json!({"label": "Revenue", "org_wide": true}),
        );
        let s = replay_actions(&config, "d-1", &[pinned, wide]);
        assert_eq!(s.replayed, 2);
        let reqs = mock.requests();
        assert_eq!(reqs[0].1, "/api/orgs/org-1/kpis");
        assert_eq!(
            reqs[0].2,
            serde_json::json!({"project_id": "proj-1", "label": "NPS",
                "description": "Promoters", "weight": 2.0,
                "created_by": KPI_CREATED_BY, "source_ref": null,
                "external_id": "ta_draft:d-1:0"})
        );
        assert!(reqs[1].2["project_id"].is_null());
        assert_eq!(reqs[1].2["external_id"], "ta_draft:d-1:1");
    }

    #[test]
    fn re_apply_of_a_fully_applied_draft_makes_no_further_calls() {
        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let actions = [goal_action("Launch"), metric_action("ref:0", "external")];
        replay_actions(&config, "d-1", &actions);
        assert_eq!(mock.requests().len(), 2);
        let s = replay_actions(&config, "d-1", &actions);
        assert_eq!((s.replayed, s.already_replayed), (0, 2));
        assert_eq!(mock.requests().len(), 2);
        assert!(ledger(&config).contains("\tgoal-9"));
    }

    #[test]
    fn partial_failure_blocks_dependants_then_re_apply_resolves_refs_from_the_ledger() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        write_wayfinder_plan_config(&config, &mock.server.uri());
        mock.mount_post(GOALS, 200, serde_json::json!({"id": "goal-9"}));
        mock.mount_post(
            "/api/projects/proj-1/goals/goal-9/kpi-metrics",
            500,
            serde_json::json!({"error": "boom"}),
        );
        let actions = [
            goal_action("Launch"),
            metric_action("ref:0", "external"),
            datapoint_action("ref:1", 1.0),
        ];
        let s = replay_actions(&config, "d-2", &actions);
        assert_eq!((s.replayed, s.failed, s.blocked), (1, 1, 1));
        // The datapoint never reached Wayfinder.
        assert_eq!(mock.requests().len(), 2);

        // Fix the server, re-apply (the mock's request log restarts): the goal is not repeated, its id still
        // resolves for the metric, and the datapoint follows.
        mock.reset_mocks();
        mock.mount_post(
            "/api/projects/proj-1/goals/goal-9/kpi-metrics",
            200,
            serde_json::json!({"id": "metric-9"}),
        );
        mock.mount_post(
            "/api/projects/proj-1/kpi-metrics/metric-9/datapoints",
            200,
            serde_json::json!({"id": "dp-9"}),
        );
        let s = replay_actions(&config, "d-2", &actions);
        assert_eq!((s.replayed, s.already_replayed, s.failed), (2, 1, 0));
        let paths: Vec<String> = mock.requests().into_iter().map(|r| r.1).collect();
        assert_eq!(
            paths,
            vec![
                "/api/projects/proj-1/goals/goal-9/kpi-metrics",
                "/api/projects/proj-1/kpi-metrics/metric-9/datapoints",
            ]
        );
    }

    #[test]
    fn a_failed_parent_blocks_every_dependant_and_nothing_else_is_sent() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        write_wayfinder_plan_config(&config, &mock.server.uri());
        mock.mount_post(GOALS, 403, serde_json::json!({"error": "no"}));
        let actions = [
            goal_action("Launch"),
            metric_action("ref:0", "external"),
            datapoint_action("ref:1", 1.0),
        ];
        let s = replay_actions(&config, "d-3", &actions);
        assert_eq!((s.replayed, s.failed, s.blocked), (0, 1, 2));
        assert_eq!(mock.requests().len(), 1);
        assert!(ledger(&config).is_empty());
    }

    #[test]
    fn a_reviewer_rejected_parent_blocks_its_dependants() {
        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let mut parent = goal_action("Launch");
        parent.disposition = ArtifactDisposition::Rejected;
        let actions = [parent, metric_action("ref:0", "external")];
        let s = replay_actions(&config, "d-4", &actions);
        assert_eq!((s.replayed, s.blocked, s.skipped_by_reviewer), (0, 1, 1));
        assert!(mock.requests().is_empty());
    }

    #[test]
    fn refs_must_point_at_an_earlier_action_of_the_right_kind() {
        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let actions = [
            goal_action("Launch"),
            // ref to itself / forward, and a datapoint ref that names a goal.
            metric_action("ref:5", "external"),
            datapoint_action("ref:0", 1.0),
        ];
        let s = replay_actions(&config, "d-5", &actions);
        assert_eq!((s.replayed, s.blocked), (1, 2));
        assert_eq!(mock.requests().len(), 1);
    }

    #[test]
    fn forbidden_replies_name_the_missing_right() {
        for (route, act, right) in [
            (GOALS, goal_action("G"), "tasks:write"),
            (
                "/api/orgs/org-1/kpis",
                action(ProposeKind::KpiCreate, serde_json::json!({"label": "K"})),
                "kpis:manage",
            ),
        ] {
            let (_dir, config) = project();
            let mock = BlockingMockServer::start();
            write_wayfinder_plan_config(&config, &mock.server.uri());
            mock.mount_post(route, 403, serde_json::json!({"error": "forbidden"}));
            let client = load_client(&config, 1).unwrap();
            let e = replay_one(
                &client,
                ProposeKind::from_tool_name(&act.tool_name).unwrap(),
                &act.parameters,
                "ta_draft:d:0",
            )
            .unwrap_err()
            .to_string();
            assert!(e.contains(right), "{e}");
            assert!(e.contains(route), "{e}");
        }
    }

    #[test]
    fn a_datapoint_on_a_task_count_metric_surfaces_the_server_message() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        write_wayfinder_plan_config(&config, &mock.server.uri());
        mock.mount_post(
            "/api/projects/proj-1/kpi-metrics/m-1/datapoints",
            409,
            serde_json::json!({"error": "task_count metrics are computed from tasks"}),
        );
        let client = load_client(&config, 1).unwrap();
        let a = datapoint_action("m-1", 3.0);
        let e = replay_one(
            &client,
            ProposeKind::KpiDatapointAdd,
            &a.parameters,
            "ta_draft:d:0",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("conflict"), "{e}");
        assert!(
            e.contains("task_count metrics are computed from tasks"),
            "{e}"
        );
    }

    #[test]
    fn gateway_captured_planning_proposals_replay_with_matching_refs() {
        use std::sync::{Arc, Mutex};
        use ta_mcp_gateway::tools::wayfinder_task as wt;

        let (_dir, config) = project();
        let mock = planning_mock(&config);
        let state = Arc::new(Mutex::new(
            ta_mcp_gateway::server::GatewayState::new(config.clone()).unwrap(),
        ));
        let goal = Uuid::new_v4();
        let g = goal.to_string();
        let r = wt::handle_propose_goal_create(
            &state,
            wt::ProposeGoalCreateParams {
                name: "Launch".into(),
                description: None,
                kind: None,
                parent_goal_id: None,
                target_date: None,
                goal_run_id: g.clone(),
            },
        )
        .unwrap();
        let goal_ref = serde_json::to_value(&r).unwrap().to_string();
        assert!(goal_ref.contains("ref:0"), "{goal_ref}");
        wt::handle_propose_kpi_metric_create(
            &state,
            wt::ProposeKpiMetricCreateParams {
                goal_id: "ref:0".into(),
                label: "Signups".into(),
                unit: "users".into(),
                target: 10.0,
                cadence: "weekly".into(),
                kind: "external".into(),
                aggregation: None,
                require_gate: None,
                weight: None,
                goal_run_id: g,
            },
        )
        .unwrap();
        assert!(mock.requests().is_empty());
        let actions = state.lock().unwrap().pending_actions[&goal].clone();
        let s = replay_actions(&config, "d-gw", &actions);
        assert_eq!((s.replayed, s.failed, s.blocked), (2, 0, 0));
        let reqs = mock.requests();
        assert_eq!(reqs[1].1, "/api/projects/proj-1/goals/goal-9/kpi-metrics");
        assert_eq!(reqs[1].2["external_id"], "ta_draft:d-gw:1");
    }
}
