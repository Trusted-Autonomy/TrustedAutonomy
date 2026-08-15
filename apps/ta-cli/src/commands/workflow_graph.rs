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
    NodeRegistry, ReviewInput, WorkItem, WorkResult, WorkerNode,
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
        tracing::info!(
            title = %item.title,
            verb = %item.verb,
            workload_type = %decision.workload_type,
            team = %decision.team,
            agent = %decision.agent,
            "graph: GoalDispatchAction routed work item"
        );

        let cmd = GoalCommands::Start {
            title: item.title.clone(),
            source: None,
            objective: item.objective.clone(),
            agent: decision.agent.clone(),
            phase: item.phase_id.clone(),
            follow_up: None,
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

    registry
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
}
