// workflow_graph.rs — CLI-layer node implementations + `ta workflow
// graph-run` entry point for the workflow graph engine (v0.17.7.1).
//
// `GoalDispatchAction`, `AutoApproveAction`, and `RecommendAction` live here
// rather than in `ta-workflow::graph` because they need `ta-brain`
// (`route()`) and `ta-goal`/the real draft-apply path — `ta-brain` already
// depends on `ta-workflow` (for its template-matching signal), so
// `ta-workflow` cannot depend on `ta-brain` back without a cycle. `apps/ta-cli`
// already depends on every crate these nodes need, so it's the natural home
// for the "wiring" layer — see `ta_workflow::graph::registry`'s module doc
// comment for the full reasoning.

use std::collections::HashMap;

use ta_mcp_gateway::GatewayConfig;
use ta_workflow::graph::{
    ActionDef, ActionNode, ActionOutcome, Decision, GraphContext, GraphDefinition, GraphError,
    NodeDef, NodeRegistry, ReviewInput, TriggerPayload, TriggerSource, WorkItem, WorkResult,
    WorkerNode,
};

use crate::commands::draft::{self, DraftCommands};
use crate::commands::goal::{self, GoalCommands};

// ── GoalDispatchAction (WorkerNode) ──────────────────────────────────────

/// Wraps `ta goal start`, passing `verb`/`workload_hint` straight into
/// `ta-brain::route()`'s already data-defined `workload_type`
/// classification: `workload_hint` (if set) or else `verb` is used as an
/// explicit `workload_type_override`, so a new domain (art, docs, whatever)
/// is a `workflow.toml` `[workload_types.<type>]` binding, not new Rust code
/// per domain.
pub struct GoalDispatchAction {
    pub config: GatewayConfig,
}

impl GoalDispatchAction {
    pub fn new(config: GatewayConfig) -> Self {
        Self { config }
    }

    /// Resolve routing for `item` via `ta_brain::route()`. Exposed
    /// separately from `dispatch()` so callers (and tests) can inspect the
    /// `RoutingDecision` without also creating a goal — this is the piece
    /// PLAN.md v0.17.7.1 item 5 requires proving: two different `verb`s
    /// against the same `workflow.toml` fixture must produce two different
    /// routing decisions with zero new Rust per verb.
    pub fn resolve_routing(
        &self,
        item: &WorkItem,
        workspace_root: &std::path::Path,
    ) -> ta_brain::RoutingDecision {
        let workload_override = item
            .workload_hint
            .clone()
            .or_else(|| Some(item.verb.clone()));
        let request = ta_brain::ExplicitGoalRequest {
            goal_title: item.title.clone(),
            objective: item.objective.clone(),
            cli_agent: None,
            cli_persona: None,
            cli_team: None,
            cli_security: None,
            cli_priority: None,
            workflow_name_or_path: None,
            workload_type_override: workload_override,
        };
        ta_brain::route(
            &ta_brain::RoutingInput::ExplicitGoal(request),
            workspace_root,
        )
    }
}

impl WorkerNode for GoalDispatchAction {
    fn dispatch(&self, item: &WorkItem, ctx: &GraphContext) -> Result<WorkResult, GraphError> {
        let decision = self.resolve_routing(item, &ctx.workspace_root);
        // `ctx.vars["draft_id"]` — if already set (e.g. by an earlier
        // worker in this same graph run, or by a driver seeding the
        // originating goal per v0.17.7.2's `CorrectiveGoalAction`) — is
        // treated as "follow up on this goal" rather than starting fresh.
        // This keeps a corrective fix on the *same* PR/branch instead of
        // opening a new one, reusing `ta goal start --follow-up`'s existing
        // parent-staging-reuse behavior (`goal.rs::start_goal_extending_parent`)
        // rather than a second dispatch path.
        let follow_up_goal_id = ctx.vars.get("draft_id").cloned();
        tracing::info!(
            title = %item.title,
            verb = %item.verb,
            workload_type = %decision.workload_type,
            team = %decision.team,
            agent = %decision.agent,
            follow_up_goal_id = follow_up_goal_id.as_deref().unwrap_or("-"),
            "graph: GoalDispatchAction routed work item"
        );

        let cmd = GoalCommands::Start {
            title: item.title.clone(),
            source: None,
            objective: item.objective.clone(),
            agent: decision.agent.clone(),
            phase: item.phase_id.clone(),
            follow_up: follow_up_goal_id.map(Some),
            objective_file: None,
        };
        goal::execute(&cmd, &self.config).map_err(|e| GraphError::NodeExecution {
            node_id: "goal_dispatch".to_string(),
            message: format!("`ta goal start` failed: {e}"),
        })?;

        let store = ta_goal::GoalRunStore::new(&self.config.goals_dir).map_err(|e| {
            GraphError::NodeExecution {
                node_id: "goal_dispatch".to_string(),
                message: format!("failed to reopen goal store after dispatch: {e}"),
            }
        })?;
        let created = store
            .list()
            .map_err(|e| GraphError::NodeExecution {
                node_id: "goal_dispatch".to_string(),
                message: format!("failed to list goals after dispatch: {e}"),
            })?
            .into_iter()
            .filter(|g| g.title == item.title)
            .max_by_key(|g| g.created_at)
            .ok_or_else(|| GraphError::NodeExecution {
                node_id: "goal_dispatch".to_string(),
                message: format!(
                    "`ta goal start` reported success but no goal titled '{}' was found",
                    item.title
                ),
            })?;

        let mut metadata = HashMap::new();
        metadata.insert("team".to_string(), decision.team.as_str().to_string());
        metadata.insert("agent".to_string(), decision.agent.clone());
        metadata.insert("workload_type".to_string(), decision.workload_type.clone());
        Ok(WorkResult {
            draft_id: created.goal_run_id.to_string(),
            metadata,
        })
    }
}

// ── AutoApproveAction (ActionNode) ───────────────────────────────────────

/// Calls the existing `ta draft apply` code path (same audit trail) when
/// `Decision.proceed` is true. Never applies otherwise — an unmet threshold
/// leaves the draft exactly where it was (pending review), it does not
/// deny/reject it (that's a human or a future `EscalateAction`'s call).
pub struct AutoApproveAction {
    pub config: GatewayConfig,
}

impl AutoApproveAction {
    pub fn new(config: GatewayConfig) -> Self {
        Self { config }
    }
}

impl ActionNode for AutoApproveAction {
    fn act(&self, decision: &Decision, ctx: &GraphContext) -> Result<ActionOutcome, GraphError> {
        if !decision.proceed {
            return Ok(ActionOutcome {
                kind: "auto_approve".to_string(),
                applied: false,
                message: format!(
                    "decision did not clear threshold (score={:.2}) — draft left pending review",
                    decision.score
                ),
                metadata: HashMap::new(),
            });
        }

        let draft_id = ctx.vars.get("draft_id").cloned();
        let cmd = DraftCommands::Apply {
            id: draft_id.clone(),
            target: None,
            submit: true,
            no_submit: false,
            review: true,
            no_review: false,
            dry_run: false,
            git_commit: false,
            git_push: false,
            skip_verify: false,
            conflict_resolution: "abort".to_string(),
            approve_patterns: vec!["all".to_string()],
            reject_patterns: vec![],
            discuss_patterns: vec![],
            phase: None,
            require_review: false,
            watch: false,
            chain: false,
            force_apply: false,
            validate_version: false,
            status: false,
            auto_repair: false,
            skip_plan_merge: false,
        };
        draft::execute(&cmd, &self.config).map_err(|e| GraphError::NodeExecution {
            node_id: "auto_approve".to_string(),
            message: format!("`ta draft apply` failed: {e}"),
        })?;

        let mut metadata = HashMap::new();
        if let Some(id) = draft_id {
            metadata.insert("draft_id".to_string(), id);
        }
        Ok(ActionOutcome {
            kind: "auto_approve".to_string(),
            applied: true,
            message: format!(
                "auto-approved and applied (score={:.2}, threshold cleared)",
                decision.score
            ),
            metadata,
        })
    }
}

// ── RecommendAction (ActionNode) ─────────────────────────────────────────

/// Surfaces the `Decision` to a human via Studio's existing Attention queue
/// — never applies. The Attention queue is already populated by polling
/// `/api/drafts` (pending-review drafts) and related endpoints
/// (`crates/ta-daemon/src/web.rs`), so a draft this action leaves in
/// `PendingReview` already shows up there — no new plumbing needed for
/// v0.17.7.1, per the design spec's reuse table (§5).
#[derive(Default)]
pub struct RecommendAction;

impl ActionNode for RecommendAction {
    fn act(&self, decision: &Decision, ctx: &GraphContext) -> Result<ActionOutcome, GraphError> {
        let draft_id = ctx.vars.get("draft_id").cloned();
        let message = format!(
            "recommendation surfaced for human review (score={:.2}, proceed={}) — draft{} remains in the Attention queue",
            decision.score,
            decision.proceed,
            draft_id
                .as_ref()
                .map(|id| format!(" {id}"))
                .unwrap_or_default()
        );
        tracing::info!(
            score = decision.score,
            proceed = decision.proceed,
            draft_id = draft_id.as_deref().unwrap_or("-"),
            "graph: RecommendAction surfaced decision to Attention queue"
        );
        let mut metadata = HashMap::new();
        if let Some(id) = draft_id {
            metadata.insert("draft_id".to_string(), id);
        }
        Ok(ActionOutcome {
            kind: "recommend".to_string(),
            applied: false,
            message,
            metadata,
        })
    }
}

// ── VcsTaskCompletionTrigger / CiFailureTrigger (TriggerSource, v0.17.7.2) ──
//
// Both triggers go through `SourceAdapter` only (constitution §16.4) — no
// `gh`/platform-specific code lives here. They poll `check_review()`
// (already used by `AutoApproveAction`'s siblings via `ta draft watch`) on a
// fixed interval up to `max_polls` times; `max_polls`/`poll_interval` are
// constructor params (not hardcoded) so tests can run with a zero interval
// and a small poll budget instead of a real sleep loop.

/// Fires once `SourceAdapter::check_review()` reports the review has reached
/// a terminal state (`merged`/`closed`) — VCS-agnostic per spec §2.1 ("via
/// `SourceAdapter::check_review()` polling ... not a `gh`-specific call").
pub struct VcsTaskCompletionTrigger {
    pub adapter: std::sync::Arc<dyn ta_submit::SourceAdapter>,
    pub review_id: String,
    pub poll_interval: std::time::Duration,
    pub max_polls: u32,
}

fn terminal_review_state(state: &str) -> bool {
    matches!(state, "merged" | "closed")
}

impl TriggerSource for VcsTaskCompletionTrigger {
    fn wait(&self, _ctx: &GraphContext) -> Result<TriggerPayload, GraphError> {
        for attempt in 0..self.max_polls {
            let status = self.adapter.check_review(&self.review_id).map_err(|e| {
                GraphError::NodeExecution {
                    node_id: "vcs_task_completion".to_string(),
                    message: format!("check_review failed for review {}: {e}", self.review_id),
                }
            })?;
            if let Some(status) = status {
                if terminal_review_state(&status.state) {
                    let mut data = std::collections::HashMap::new();
                    data.insert("review_id".to_string(), self.review_id.clone());
                    data.insert("state".to_string(), status.state.clone());
                    data.insert(
                        "checks_passing".to_string(),
                        status
                            .checks_passing
                            .map(|b| b.to_string())
                            .unwrap_or_default(),
                    );
                    return Ok(TriggerPayload {
                        kind: "vcs_task_completion".to_string(),
                        data,
                    });
                }
            }
            if attempt + 1 < self.max_polls && !self.poll_interval.is_zero() {
                std::thread::sleep(self.poll_interval);
            }
        }
        Err(GraphError::NodeExecution {
            node_id: "vcs_task_completion".to_string(),
            message: format!(
                "review {} did not reach a terminal state within {} poll(s) \
                 (interval {:?}) — it may still be open, or CI is still running",
                self.review_id, self.max_polls, self.poll_interval
            ),
        })
    }
}

/// Fires specifically when `check_review()`'s `checks_passing` transitions
/// to `Some(false)` (not merely "is false" — avoids re-firing every poll
/// while CI stays red). On firing, also calls `check_failures()` for detail;
/// per spec §4, an adapter with no failure-log support (Perforce/SVN/"none",
/// or Git with no `gh` CLI) degrades to an explicit "investigate manually"
/// hint rather than an empty/confusing payload.
pub struct CiFailureTrigger {
    pub adapter: std::sync::Arc<dyn ta_submit::SourceAdapter>,
    pub review_id: String,
    pub poll_interval: std::time::Duration,
    pub max_polls: u32,
}

const CI_DETAIL_UNAVAILABLE_MESSAGE: &str =
    "CI failure detail unavailable for this VCS adapter, investigate manually.";

impl TriggerSource for CiFailureTrigger {
    fn wait(&self, _ctx: &GraphContext) -> Result<TriggerPayload, GraphError> {
        let mut previously_passing: Option<bool> = None;
        for attempt in 0..self.max_polls {
            let status = self.adapter.check_review(&self.review_id).map_err(|e| {
                GraphError::NodeExecution {
                    node_id: "ci_failure".to_string(),
                    message: format!("check_review failed for review {}: {e}", self.review_id),
                }
            })?;
            if let Some(status) = status {
                let checks_passing = status.checks_passing;
                if checks_passing == Some(false) && previously_passing != Some(false) {
                    let failures = self
                        .adapter
                        .check_failures(&self.review_id)
                        .unwrap_or_default();

                    let mut data = std::collections::HashMap::new();
                    data.insert("review_id".to_string(), self.review_id.clone());
                    data.insert("state".to_string(), status.state.clone());
                    if let Some(first) = failures.first() {
                        data.insert("check_name".to_string(), first.check_name.clone());
                        data.insert("log_excerpt".to_string(), first.log_excerpt.clone());
                    } else {
                        data.insert("check_name".to_string(), "unknown".to_string());
                        data.insert(
                            "log_excerpt".to_string(),
                            CI_DETAIL_UNAVAILABLE_MESSAGE.to_string(),
                        );
                    }
                    if failures.len() > 1 {
                        data.insert("check_count".to_string(), failures.len().to_string());
                    }
                    return Ok(TriggerPayload {
                        kind: "ci_failure".to_string(),
                        data,
                    });
                }
                if terminal_review_state(&status.state) {
                    return Err(GraphError::NodeExecution {
                        node_id: "ci_failure".to_string(),
                        message: format!(
                            "review {} reached terminal state '{}' without a new CI \
                             failure — nothing to correct",
                            self.review_id, status.state
                        ),
                    });
                }
                previously_passing = checks_passing;
            }
            if attempt + 1 < self.max_polls && !self.poll_interval.is_zero() {
                std::thread::sleep(self.poll_interval);
            }
        }
        Err(GraphError::NodeExecution {
            node_id: "ci_failure".to_string(),
            message: format!(
                "no CI failure detected for review {} within {} poll(s) (interval {:?})",
                self.review_id, self.max_polls, self.poll_interval
            ),
        })
    }
}

// ── CorrectiveGoalAction (ActionNode, v0.17.7.2) ─────────────────────────

/// On a `CiFailureTrigger`'s payload, dispatches a follow-up fix goal via
/// the same `GoalDispatchAction` machinery `WorkerNode`s use — "one dispatch
/// path, not two" per the design spec. Reuses v0.17.0.12.31's
/// `decide_gate_failure_action`/`GateFailureMode::AutoFix` retry-cap/escalate
/// logic rather than a second auto-fix mechanism.
///
/// `decision` is accepted only for `ActionNode` trait conformance — this
/// action's real gate is the retry cap (`ctx.vars["ci_fix_attempt"]` vs.
/// `retry_cap`), not `Decision.proceed`: a CI failure needing a fix isn't a
/// panel vote, per spec §3's `ci-failure-response.toml` ("no reviewers
/// needed"). The caller (`run_ci_failure_watch`) is responsible for
/// populating `ctx.vars` from the trigger payload before calling `act()`.
pub struct CorrectiveGoalAction {
    pub dispatcher: GoalDispatchAction,
    pub retry_cap: u32,
}

impl CorrectiveGoalAction {
    pub fn new(config: GatewayConfig, retry_cap: u32) -> Self {
        Self {
            dispatcher: GoalDispatchAction::new(config),
            retry_cap,
        }
    }
}

impl ActionNode for CorrectiveGoalAction {
    fn act(&self, _decision: &Decision, ctx: &GraphContext) -> Result<ActionOutcome, GraphError> {
        let check_name = ctx
            .vars
            .get("check_name")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let log_excerpt = ctx
            .vars
            .get("log_excerpt")
            .cloned()
            .unwrap_or_else(|| CI_DETAIL_UNAVAILABLE_MESSAGE.to_string());
        let review_id = ctx.vars.get("review_id").cloned().unwrap_or_default();
        let attempt: u32 = ctx
            .vars
            .get("ci_fix_attempt")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        match ta_workflow::decide_gate_failure_action(
            ta_workflow::GateFailureMode::AutoFix,
            attempt,
            self.retry_cap,
        ) {
            ta_workflow::GateFailureAction::EscalateToHuman => {
                let message = format!(
                    "check '{check_name}' failed on review {review_id} (attempt {attempt}/{}) \
                     — auto-fix retry cap exhausted, escalating to human.\n{log_excerpt}",
                    self.retry_cap
                );
                tracing::warn!(
                    review_id = %review_id,
                    check_name = %check_name,
                    attempt,
                    retry_cap = self.retry_cap,
                    "graph: CorrectiveGoalAction escalating to human"
                );
                let mut metadata = HashMap::new();
                metadata.insert("review_id".to_string(), review_id);
                metadata.insert("escalated".to_string(), "true".to_string());
                Ok(ActionOutcome {
                    kind: "corrective_goal".to_string(),
                    applied: false,
                    message,
                    metadata,
                })
            }
            ta_workflow::GateFailureAction::LaunchFollowUpFix => {
                let objective = format!(
                    "# Fix CI failure\n\n\
                     Check `{check_name}` failed on review {review_id} (auto-fix attempt \
                     {attempt}/{}).\n\nLog excerpt:\n```\n{log_excerpt}\n```\n\n\
                     Fix the issue so this check passes.",
                    self.retry_cap
                );
                let item = WorkItem {
                    title: format!("Fix CI failure: {check_name}"),
                    objective,
                    phase_id: None,
                    verb: "fix".to_string(),
                    workload_hint: None,
                };
                let result = self.dispatcher.dispatch(&item, ctx)?;
                tracing::info!(
                    review_id = %review_id,
                    check_name = %check_name,
                    attempt,
                    retry_cap = self.retry_cap,
                    goal_id = %result.draft_id,
                    "graph: CorrectiveGoalAction launched follow-up fix goal"
                );
                let mut metadata = result.metadata;
                metadata.insert("review_id".to_string(), review_id.clone());
                metadata.insert("goal_id".to_string(), result.draft_id.clone());
                Ok(ActionOutcome {
                    kind: "corrective_goal".to_string(),
                    applied: true,
                    message: format!(
                        "launched follow-up fix goal {} for check '{check_name}' on review \
                         {review_id} (attempt {attempt}/{})",
                        result.draft_id, self.retry_cap
                    ),
                    metadata,
                })
            }
        }
    }
}

/// Drives `CiFailureTrigger` → `CorrectiveGoalAction` end-to-end for one
/// review: waits for a CI failure, dispatches (or escalates) a fix, then
/// loops back to watch for the *next* failure (e.g. after the fix goal's own
/// push re-triggers CI) until either the review reaches a terminal state or
/// the retry cap escalates. This is the "future caller" `graph::engine`'s
/// module doc anticipates for wiring `TriggerSource::wait()` into an
/// event-driven flow — scoped here to the single CI-failure-response use
/// case rather than the full daemon-level graph scheduler (later phases).
///
/// Attempt count is tracked in `ctx.vars["ci_fix_attempt"]` for the lifetime
/// of this call only (in-process, not persisted across restarts) — adequate
/// for a single `ta workflow watch-ci` invocation; daemon-level persistence
/// is out of scope for this phase.
///
/// `origin_goal_id`, when known, seeds `ctx.vars["draft_id"]` so
/// `GoalDispatchAction::dispatch`'s follow-up support keeps the fix on the
/// *same* branch/PR (see `GoalDispatchAction::dispatch`'s doc comment).
/// Standalone callers like `ta workflow watch-ci <review-id>` don't have a
/// goal ID for a bare review ID (no reverse PR->goal lookup exists yet), so
/// they pass `None` and get a fresh (non-follow-up) fix goal per failure —
/// still correct, just not landing on the original branch. A future caller
/// that already knows the originating goal (e.g. a chained
/// `phase-review-panel` -> `ci-failure-response` graph run, v0.17.7.4) can
/// supply it here.
pub fn run_ci_failure_watch(
    adapter: std::sync::Arc<dyn ta_submit::SourceAdapter>,
    review_id: &str,
    origin_goal_id: Option<&str>,
    retry_cap: u32,
    poll_interval: std::time::Duration,
    max_polls_per_wait: u32,
    config: &GatewayConfig,
) -> anyhow::Result<Vec<ActionOutcome>> {
    let trigger = CiFailureTrigger {
        adapter,
        review_id: review_id.to_string(),
        poll_interval,
        max_polls: max_polls_per_wait,
    };
    let action = CorrectiveGoalAction::new(config.clone(), retry_cap);
    let mut ctx = GraphContext::new(&config.workspace_root, uuid::Uuid::new_v4().to_string());
    if let Some(goal_id) = origin_goal_id {
        ctx.vars.insert("draft_id".to_string(), goal_id.to_string());
    }

    let mut outcomes = Vec::new();
    let mut attempt: u32 = 0;
    // terminal state or poll budget exhausted — stop watching.
    while let Ok(payload) = trigger.wait(&ctx) {
        attempt += 1;
        for (k, v) in payload.data {
            ctx.vars.insert(k, v);
        }
        ctx.vars
            .insert("ci_fix_attempt".to_string(), attempt.to_string());

        let decision = Decision {
            score: 1.0,
            proceed: true,
            algorithm_used: ta_workflow::consensus::ConsensusAlgorithm::Weighted,
            scores_by_role: HashMap::new(),
            findings_by_role: HashMap::new(),
            timed_out_roles: vec![],
            override_active: false,
            summary: "CI failure — no panel vote needed".to_string(),
        };
        let outcome = action.act(&decision, &ctx)?;
        let escalated = outcome.metadata.get("escalated").map(String::as_str) == Some("true");
        outcomes.push(outcome);
        if escalated {
            break;
        }
    }
    Ok(outcomes)
}

// ── Registry wiring + `ta workflow graph-run` entry point ───────────────

/// Build a `NodeRegistry` with `ta-workflow`'s own built-ins
/// (`policy`/`advisor_confidence`/`weighted`) plus the three CLI-layer kinds
/// this module implements (`goal_dispatch`/`auto_approve`/`recommend`).
pub fn build_registry(config: GatewayConfig) -> NodeRegistry {
    let mut registry = NodeRegistry::with_builtins();

    let dispatch_config = config.clone();
    registry.register_worker("goal_dispatch", move |_def| {
        Ok(Box::new(GoalDispatchAction::new(dispatch_config.clone())) as Box<dyn WorkerNode>)
    });

    let apply_config = config.clone();
    registry.register_action("auto_approve", move |_def: &ActionDef| {
        Ok(Box::new(AutoApproveAction::new(apply_config.clone())) as Box<dyn ActionNode>)
    });

    registry.register_action("recommend", |_def: &ActionDef| {
        Ok(Box::new(RecommendAction) as Box<dyn ActionNode>)
    });

    let corrective_config = config.clone();
    registry.register_action("corrective_goal", move |def: &ActionDef| {
        let retry_cap = def
            .params
            .get("retry_cap")
            .and_then(|v| v.as_integer())
            .unwrap_or(1) as u32;
        Ok(Box::new(CorrectiveGoalAction::new(
            corrective_config.clone(),
            retry_cap,
        )) as Box<dyn ActionNode>)
    });

    let vcs_trigger_config = config.clone();
    registry.register_trigger("vcs_task_completion", move |def| {
        Ok(Box::new(VcsTaskCompletionTrigger {
            adapter: source_adapter_for_project(&vcs_trigger_config),
            review_id: def.param_str("review_id").unwrap_or_default().to_string(),
            poll_interval: poll_interval_param(def),
            max_polls: max_polls_param(def),
        }) as Box<dyn TriggerSource>)
    });

    let ci_trigger_config = config.clone();
    registry.register_trigger("ci_failure", move |def| {
        Ok(Box::new(CiFailureTrigger {
            adapter: source_adapter_for_project(&ci_trigger_config),
            review_id: def.param_str("review_id").unwrap_or_default().to_string(),
            poll_interval: poll_interval_param(def),
            max_polls: max_polls_param(def),
        }) as Box<dyn TriggerSource>)
    });

    registry
}

/// Resolve this project's configured `SourceAdapter` the same way
/// `load_excludes_with_adapter`/`draft.rs`'s apply path do (`.ta/workflow.toml`
/// -> `select_adapter`), shared to `Arc` so multiple trigger instances (and
/// the daemon-style watch loop) can hold it concurrently without re-reading
/// config per poll.
pub fn source_adapter_for_project(
    config: &GatewayConfig,
) -> std::sync::Arc<dyn ta_submit::SourceAdapter> {
    let wf_path = config.workspace_root.join(".ta").join("workflow.toml");
    let wf_config = ta_submit::WorkflowConfig::load_or_default(&wf_path);
    std::sync::Arc::from(ta_submit::select_adapter(
        &config.workspace_root,
        &wf_config.submit,
    ))
}

const DEFAULT_TRIGGER_POLL_INTERVAL_SECS: i64 = 30;
const DEFAULT_TRIGGER_MAX_POLLS: i64 = 120;

fn poll_interval_param(def: &NodeDef) -> std::time::Duration {
    std::time::Duration::from_secs(
        def.params
            .get("poll_interval_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(DEFAULT_TRIGGER_POLL_INTERVAL_SECS)
            .max(0) as u64,
    )
}

fn max_polls_param(def: &NodeDef) -> u32 {
    def.params
        .get("max_polls")
        .and_then(|v| v.as_integer())
        .unwrap_or(DEFAULT_TRIGGER_MAX_POLLS)
        .max(1) as u32
}

/// `ta workflow graph-run <name>` — load `.ta/workflows/graphs/<name>.toml`
/// and execute it end-to-end, for testing/debugging a graph before it's
/// wired into an event-driven flow (that wiring is v0.17.7.2/.3). Named
/// `graph-run` rather than the PLAN.md-literal `graph run` because
/// `WorkflowCommands::Graph { path, dot }` already means "print a YAML
/// workflow's artifact-type DAG" — reusing that name for a different
/// engine's execution entry point would collide.
pub fn run_named_graph(
    name: &str,
    goal_title: &str,
    objective: &str,
    verb: &str,
    workload_hint: Option<&str>,
    phase: Option<&str>,
    config: &GatewayConfig,
) -> anyhow::Result<()> {
    let def = GraphDefinition::load_named(&config.workspace_root, name)
        .map_err(|e| anyhow::anyhow!("failed to load graph '{name}': {e}"))?;
    let registry = build_registry(config.clone());

    let run_id = uuid::Uuid::new_v4().to_string();
    let mut ctx = GraphContext::new(&config.workspace_root, run_id);

    let work_item = WorkItem {
        title: goal_title.to_string(),
        objective: objective.to_string(),
        phase_id: phase.map(|p| p.to_string()),
        verb: verb.to_string(),
        workload_hint: workload_hint.map(|w| w.to_string()),
    };
    let review_input = ReviewInput {
        agent_id: "claude-code".to_string(),
        plan_phase: phase.map(|p| p.to_string()),
        ..Default::default()
    };

    let outcome =
        ta_workflow::graph::run_graph(&def, &registry, &work_item, &review_input, &mut ctx)
            .map_err(|e| anyhow::anyhow!("graph '{name}' run failed: {e}"))?;

    println!("[graph] run '{name}' complete (run_id={})", ctx.run_id);
    for work_result in &outcome.work_results {
        println!("  worker -> draft_id={}", work_result.draft_id);
    }
    for vote in &outcome.votes {
        println!(
            "  reviewer '{}': score={:.2} timed_out={}",
            vote.role, vote.score, vote.timed_out
        );
    }
    if let Some(decision) = &outcome.decision {
        println!(
            "  decision: score={:.2} proceed={} algorithm={}",
            decision.score, decision.proceed, decision.algorithm_used
        );
    }
    if let Some(action) = &outcome.action_outcome {
        println!(
            "  action '{}': applied={} — {}",
            action.kind, action.applied, action.message
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn passing_decision() -> Decision {
        Decision {
            score: 1.0,
            proceed: true,
            algorithm_used: ta_workflow::consensus::ConsensusAlgorithm::Weighted,
            scores_by_role: HashMap::new(),
            findings_by_role: HashMap::new(),
            timed_out_roles: vec![],
            override_active: false,
            summary: "test decision".to_string(),
        }
    }

    fn failing_decision() -> Decision {
        Decision {
            proceed: false,
            score: 0.1,
            ..passing_decision()
        }
    }

    /// Build a real, approved draft package the same way `draft::execute`'s
    /// own tests do (`Start` -> mutate staging -> `Build` -> `Approve`), so
    /// `AutoApproveAction`/`RecommendAction` tests exercise the exact same
    /// public dispatch path `ta draft apply`/`ta draft approve` use — not a
    /// stand-in fixture.
    fn build_approved_draft(project: &std::path::Path) -> (GatewayConfig, String) {
        std::fs::write(project.join("README.md"), "# Original\n").unwrap();

        let config = GatewayConfig::for_project(project);
        goal::execute(
            &GoalCommands::Start {
                title: "graph-node-test".to_string(),
                source: Some(project.to_path_buf()),
                objective: "test AutoApproveAction/RecommendAction".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let goal = goal_store.list().unwrap().into_iter().next().unwrap();
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();

        draft::execute(
            &DraftCommands::Build {
                goal_id: goal_id.clone(),
                summary: "graph node test changes".to_string(),
                latest: false,
                apply_context_file: None,
            },
            &config,
        )
        .unwrap();

        let packages = draft::load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        draft::execute(
            &DraftCommands::Approve {
                id: Some(pkg_id.clone()),
                reviewer: "graph-test".to_string(),
                reviewer_as: None,
                force_override: false,
            },
            &config,
        )
        .unwrap();

        (config, pkg_id)
    }

    #[test]
    fn auto_approve_action_calls_the_real_ta_draft_apply_path() {
        let project = TempDir::new().unwrap();
        let (config, draft_id) = build_approved_draft(project.path());

        let mut ctx = GraphContext::new(project.path(), "run-auto-approve-test");
        ctx.vars.insert("draft_id".to_string(), draft_id);

        let action = AutoApproveAction::new(config);
        let outcome = action.act(&passing_decision(), &ctx).unwrap();

        assert!(
            outcome.applied,
            "AutoApproveAction must apply on proceed=true"
        );
        assert_eq!(outcome.kind, "auto_approve");
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(
            readme, "# Updated\n",
            "AutoApproveAction must actually call ta draft apply's code path, \
             proven by the source file changing on disk — not a stub"
        );
    }

    #[test]
    fn auto_approve_action_skips_apply_when_decision_did_not_proceed() {
        let project = TempDir::new().unwrap();
        let (config, draft_id) = build_approved_draft(project.path());

        let mut ctx = GraphContext::new(project.path(), "run-auto-approve-skip-test");
        ctx.vars.insert("draft_id".to_string(), draft_id);

        let action = AutoApproveAction::new(config);
        let outcome = action.act(&failing_decision(), &ctx).unwrap();

        assert!(!outcome.applied);
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(
            readme, "# Original\n",
            "must not apply when decision.proceed is false"
        );
    }

    #[test]
    fn recommend_action_never_applies_the_same_decision_auto_approve_would() {
        // Same graph, same Decision, only the ActionNode kind differs —
        // proving constitution §16.2's "same decision, different wiring".
        let project = TempDir::new().unwrap();
        let (_config, draft_id) = build_approved_draft(project.path());

        let mut ctx = GraphContext::new(project.path(), "run-recommend-test");
        ctx.vars.insert("draft_id".to_string(), draft_id);

        let action = RecommendAction;
        let outcome = action.act(&passing_decision(), &ctx).unwrap();

        assert!(!outcome.applied, "RecommendAction must never apply");
        assert_eq!(outcome.kind, "recommend");
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(
            readme, "# Original\n",
            "RecommendAction must leave source untouched even with an identical proceed=true decision"
        );
    }

    #[test]
    fn goal_dispatch_action_verb_alone_changes_the_routing_decision() {
        // No new Rust per verb — a `[workload_types.<verb>]` binding in
        // `.ta/workflow.toml` is enough, per PLAN.md v0.17.7.1 item 5's
        // "data-only extensibility" requirement.
        let project = TempDir::new().unwrap();
        let ta_dir = project.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            r#"
[workload_types.implement]
team = "implementer"
persona = "careful-reviewer"

[workload_types.create]
team = "reviewer"
persona = "creative-generator"
"#,
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());
        let action = GoalDispatchAction::new(config);

        let implement_item = WorkItem {
            title: "Phase X".to_string(),
            objective: "do the phase".to_string(),
            phase_id: None,
            verb: "implement".to_string(),
            workload_hint: None,
        };
        let create_item = WorkItem {
            verb: "create".to_string(),
            ..implement_item.clone()
        };

        let implement_decision = action.resolve_routing(&implement_item, project.path());
        let create_decision = action.resolve_routing(&create_item, project.path());

        assert_eq!(implement_decision.workload_type, "implement");
        assert_eq!(create_decision.workload_type, "create");
        assert_ne!(
            implement_decision.team, create_decision.team,
            "verb alone must change the resolved team via workflow.toml data, no new Rust"
        );
        assert_ne!(implement_decision.persona, create_decision.persona);
    }

    // ── VcsTaskCompletionTrigger / CiFailureTrigger / CorrectiveGoalAction
    //    (v0.17.7.2) ──────────────────────────────────────────────────────

    /// A `SourceAdapter` whose `check_review()` replays a fixed script of
    /// responses (one per call, clamped to the last entry once exhausted) —
    /// following this codebase's established `MockAdapter` convention
    /// (`crates/ta-submit/src/adapter.rs` tests: implement only what the
    /// test exercises, `unimplemented!()` for the rest, real defaults for
    /// everything else). `check_failures()` is fixed (not scripted) since
    /// no test needs it to vary call-to-call.
    struct ScriptedAdapter {
        statuses: Vec<Option<ta_submit::ReviewStatus>>,
        call_count: std::sync::Mutex<usize>,
        failures: Vec<ta_submit::CheckFailure>,
    }

    impl ScriptedAdapter {
        fn new(
            statuses: Vec<Option<ta_submit::ReviewStatus>>,
            failures: Vec<ta_submit::CheckFailure>,
        ) -> Self {
            Self {
                statuses,
                call_count: std::sync::Mutex::new(0),
                failures,
            }
        }
    }

    impl ta_submit::SourceAdapter for ScriptedAdapter {
        fn prepare(
            &self,
            _ctx: &ta_goal::CommitContext,
            _config: &ta_submit::SubmitConfig,
        ) -> ta_submit::adapter::Result<()> {
            unimplemented!()
        }
        fn commit(
            &self,
            _ctx: &ta_goal::CommitContext,
            _pr: &ta_changeset::DraftPackage,
            _message: &str,
        ) -> ta_submit::adapter::Result<ta_submit::CommitResult> {
            unimplemented!()
        }
        fn push(
            &self,
            _ctx: &ta_goal::CommitContext,
        ) -> ta_submit::adapter::Result<ta_submit::PushResult> {
            unimplemented!()
        }
        fn open_review(
            &self,
            _ctx: &ta_goal::CommitContext,
            _pr: &ta_changeset::DraftPackage,
        ) -> ta_submit::adapter::Result<ta_submit::ReviewResult> {
            unimplemented!()
        }
        fn name(&self) -> &str {
            "scripted"
        }
        fn check_review(
            &self,
            _review_id: &str,
        ) -> ta_submit::adapter::Result<Option<ta_submit::ReviewStatus>> {
            let mut count = self.call_count.lock().unwrap();
            let idx = (*count).min(self.statuses.len().saturating_sub(1));
            *count += 1;
            Ok(self.statuses.get(idx).cloned().flatten())
        }
        fn check_failures(
            &self,
            _review_id: &str,
        ) -> ta_submit::adapter::Result<Vec<ta_submit::CheckFailure>> {
            Ok(self.failures.clone())
        }
    }

    fn status(state: &str, checks_passing: Option<bool>) -> Option<ta_submit::ReviewStatus> {
        Some(ta_submit::ReviewStatus {
            state: state.to_string(),
            checks_passing,
        })
    }

    #[test]
    fn ci_failure_trigger_fires_only_on_transition_to_false() {
        // pass -> pass -> fail: must not fire on the first two polls, only
        // once checks_passing actually transitions to Some(false).
        let adapter = ScriptedAdapter::new(
            vec![
                status("open", Some(true)),
                status("open", Some(true)),
                status("open", Some(false)),
            ],
            vec![ta_submit::CheckFailure {
                check_name: "build".to_string(),
                log_excerpt: "error: something broke".to_string(),
            }],
        );
        let trigger = CiFailureTrigger {
            adapter: std::sync::Arc::new(adapter),
            review_id: "42".to_string(),
            poll_interval: std::time::Duration::ZERO,
            max_polls: 5,
        };
        let project = TempDir::new().unwrap();
        let ctx = GraphContext::new(project.path(), "test-run");

        let payload = trigger.wait(&ctx).unwrap();

        assert_eq!(payload.kind, "ci_failure");
        assert_eq!(payload.data.get("check_name").unwrap(), "build");
        assert!(payload
            .data
            .get("log_excerpt")
            .unwrap()
            .contains("something broke"));
    }

    #[test]
    fn ci_failure_trigger_degrades_gracefully_when_check_failures_is_empty() {
        // Simulates a non-Git adapter (Perforce/SVN/"none") staying on the
        // check_failures() default (empty vec) — per constitution §16.4 this
        // must degrade to an explicit "investigate manually" hint, not an
        // empty/confusing payload.
        let adapter = ScriptedAdapter::new(vec![status("open", Some(false))], vec![]);
        let trigger = CiFailureTrigger {
            adapter: std::sync::Arc::new(adapter),
            review_id: "42".to_string(),
            poll_interval: std::time::Duration::ZERO,
            max_polls: 3,
        };
        let project = TempDir::new().unwrap();
        let ctx = GraphContext::new(project.path(), "test-run");

        let payload = trigger.wait(&ctx).unwrap();

        assert_eq!(payload.data.get("check_name").unwrap(), "unknown");
        assert_eq!(
            payload.data.get("log_excerpt").unwrap(),
            CI_DETAIL_UNAVAILABLE_MESSAGE
        );
    }

    #[test]
    fn vcs_task_completion_trigger_fires_on_terminal_state() {
        let adapter = ScriptedAdapter::new(
            vec![status("open", None), status("merged", Some(true))],
            vec![],
        );
        let trigger = VcsTaskCompletionTrigger {
            adapter: std::sync::Arc::new(adapter),
            review_id: "7".to_string(),
            poll_interval: std::time::Duration::ZERO,
            max_polls: 5,
        };
        let project = TempDir::new().unwrap();
        let ctx = GraphContext::new(project.path(), "test-run");

        let payload = trigger.wait(&ctx).unwrap();

        assert_eq!(payload.kind, "vcs_task_completion");
        assert_eq!(payload.data.get("state").unwrap(), "merged");
    }

    #[test]
    fn vcs_task_completion_trigger_errors_when_poll_budget_exhausted() {
        let adapter = ScriptedAdapter::new(vec![status("open", Some(true))], vec![]);
        let trigger = VcsTaskCompletionTrigger {
            adapter: std::sync::Arc::new(adapter),
            review_id: "7".to_string(),
            poll_interval: std::time::Duration::ZERO,
            max_polls: 3,
        };
        let project = TempDir::new().unwrap();
        let ctx = GraphContext::new(project.path(), "test-run");

        assert!(trigger.wait(&ctx).is_err());
    }

    #[test]
    fn ci_failure_trigger_drives_corrective_goal_action_end_to_end() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        let config = GatewayConfig::for_project(project.path());

        let adapter = ScriptedAdapter::new(
            vec![status("open", Some(false))],
            vec![ta_submit::CheckFailure {
                check_name: "build".to_string(),
                log_excerpt: "error: something broke".to_string(),
            }],
        );
        let trigger = CiFailureTrigger {
            adapter: std::sync::Arc::new(adapter),
            review_id: "42".to_string(),
            poll_interval: std::time::Duration::ZERO,
            max_polls: 3,
        };
        let mut ctx = GraphContext::new(project.path(), "test-run");

        let payload = trigger.wait(&ctx).unwrap();
        for (k, v) in payload.data {
            ctx.vars.insert(k, v);
        }
        ctx.vars
            .insert("ci_fix_attempt".to_string(), "1".to_string());

        let action = CorrectiveGoalAction::new(config.clone(), 2);
        let outcome = action.act(&passing_decision(), &ctx).unwrap();

        assert!(outcome.applied, "must launch a follow-up fix goal");
        assert_eq!(outcome.kind, "corrective_goal");
        assert!(outcome.message.contains("build"));

        let store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = store.list().unwrap();
        assert!(
            goals
                .iter()
                .any(|g| g.title.contains("Fix CI failure: build")),
            "CorrectiveGoalAction must actually dispatch a goal via GoalDispatchAction \
             machinery, not a stub — proven by a real goal existing in the store"
        );
    }

    #[test]
    fn corrective_goal_action_escalates_to_human_after_retry_cap_exhausted() {
        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());
        let mut ctx = GraphContext::new(project.path(), "test-run");
        ctx.vars
            .insert("check_name".to_string(), "build".to_string());
        ctx.vars
            .insert("log_excerpt".to_string(), "still broken".to_string());
        ctx.vars.insert("review_id".to_string(), "42".to_string());
        // attempt (2) exceeds retry_cap (1) — must escalate, not retry again.
        ctx.vars
            .insert("ci_fix_attempt".to_string(), "2".to_string());

        let action = CorrectiveGoalAction::new(config, 1);
        let outcome = action.act(&passing_decision(), &ctx).unwrap();

        assert!(
            !outcome.applied,
            "must not launch another follow-up fix goal past the retry cap"
        );
        assert_eq!(
            outcome.metadata.get("escalated").map(String::as_str),
            Some("true")
        );
        assert!(outcome.message.contains("escalating"));
    }

    #[test]
    fn run_ci_failure_watch_launches_fix_then_escalates_after_repeat_failure() {
        // Three consecutive CI failures with retry_cap=1: attempt 1 launches
        // a follow-up fix, attempt 2 escalates — reusing v0.17.0.12.31's
        // decide_gate_failure_action, not a second retry mechanism. The
        // third scripted failure must never be reached (the loop stops at
        // escalation).
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        let config = GatewayConfig::for_project(project.path());

        let failing = ta_submit::CheckFailure {
            check_name: "build".to_string(),
            log_excerpt: "still broken".to_string(),
        };
        let adapter = ScriptedAdapter::new(
            vec![
                status("open", Some(false)),
                status("open", Some(true)),
                status("open", Some(false)),
                status("open", Some(true)),
                status("open", Some(false)),
            ],
            vec![failing],
        );

        let outcomes = run_ci_failure_watch(
            std::sync::Arc::new(adapter),
            "42",
            None,
            1,
            std::time::Duration::ZERO,
            10,
            &config,
        )
        .unwrap();

        assert_eq!(
            outcomes.len(),
            2,
            "must stop after escalation, not retry a third time"
        );
        assert!(outcomes[0].applied, "attempt 1 launches a follow-up fix");
        assert!(
            !outcomes[1].applied,
            "attempt 2 escalates instead of retrying again"
        );
        assert_eq!(
            outcomes[1].metadata.get("escalated").map(String::as_str),
            Some("true")
        );
    }
}
