// multi_phase.rs — Advisor Natural-Language Multi-Phase Entry Point
// (v0.17.7.4).
//
// Resolves a natural-language phase range ("build phases v0.17.3 through
// v0.17.8", parsed by `ta_workflow::intent::extract_phase_range`) against
// PLAN.md's phase graph, partitions the resolved phases into dependency
// waves (reusing `plan::candidate_waves`, v0.17.0.12.34), and drives each
// phase through: dispatch (fire-and-forget goal start, same semantics
// `goal_dispatch` worker nodes use elsewhere in the graph engine) -> wait
// for its draft to appear -> the named `phase-review-panel`-shaped graph
// instance (v0.17.7.1/.3, reviewers -> weighted decision -> action, no
// worker section — reviewing an already-built draft, same calling
// convention as `run_apply_gate`) -> `run_ci_failure_watch` (v0.17.7.2) for
// any CI failures on the resulting PR -> a `VcsTaskCompletionTrigger` wait
// for the PR to reach a terminal state. Waves run sequentially; phases
// within a wave dispatch concurrently via `ta_workflow::run_concurrently`
// (item 4).
//
// "Wait for its draft to appear" is deliberately NOT the graph engine
// actually implementing the phase — nothing in this codebase synchronously
// drives a real coding agent to completion from inside a library call (see
// `GoalDispatchAction::dispatch`'s doc comment: it only starts a goal).
// That step is a bounded poll with an `implement_hook` extension point: by
// default (`None`, everything public callers use) an external
// process — a human, or an agent session working through the dispatched
// goal's staging directory — is expected to actually implement the phase
// and run `ta draft build`; the poll budget just decides how long this call
// waits before pausing (an `escalated` result, not an error) rather than
// blocking forever. A future daemon integration that can synchronously
// launch and wait on a real coding agent would supply a hook here instead.
//
// A phase whose panel review never applied (advisory `recommend` mode, or
// the panel score didn't clear threshold), whose CI corrective-goal retries
// are exhausted, or whose merge-wait poll budget is exhausted, escalates
// and halts the remaining range — mirroring the user's own standing
// instruction: pause rather than silently skip ahead when the outcome
// isn't certain (item 6).

use std::sync::Arc;
use std::time::Duration;

use ta_mcp_gateway::GatewayConfig;
use ta_workflow::graph::{
    ActionOutcome, Decision, GraphContext, GraphDefinition, GraphError, ReviewInput, TriggerSource,
    WorkItem, WorkerNode,
};
use ta_workflow::intent::PhaseRangeIntent;

use crate::commands::draft;
use crate::commands::plan::{self, PlanPhase, PlanStatus};
use crate::commands::workflow_graph;

// ── Phase-range resolution (item 1) ──────────────────────────────────────

/// Why a requested phase range couldn't be resolved unambiguously — surfaced
/// as a clarifying question rather than a guess (PLAN.md v0.17.7.4 item 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseRangeError {
    PhaseNotFound {
        requested: String,
        boundary: &'static str,
    },
    RangeReversed {
        start: String,
        end: String,
    },
    UnresolvedDependency {
        phase_id: String,
        dep_id: String,
    },
}

impl std::fmt::Display for PhaseRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseRangeError::PhaseNotFound {
                requested,
                boundary,
            } => write!(
                f,
                "Couldn't find a plan phase matching \"{requested}\" (the {boundary} of the \
                 range). Check PLAN.md for the exact phase ID, or run `ta plan status` to list \
                 phases."
            ),
            PhaseRangeError::RangeReversed { start, end } => write!(
                f,
                "\"{start}\" comes after \"{end}\" in PLAN.md — did you mean \"build phases \
                 {end} through {start}\"?"
            ),
            PhaseRangeError::UnresolvedDependency { phase_id, dep_id } => write!(
                f,
                "Phase {phase_id} in this range depends on {dep_id}, which is outside the range \
                 and not yet done. Building phases out of dependency order risks a broken \
                 intermediate state — widen the range to include {dep_id}, or narrow it to start \
                 after it."
            ),
        }
    }
}

/// Resolve a `start..=end` phase range against PLAN.md's parsed phase list
/// (document order — the same order `load_plan` returns). Ambiguous input
/// (an unknown boundary, a reversed range, or a range that crosses a
/// dependency on a not-yet-done phase outside the range) is an error rather
/// than a guess, per item 1's "ask a clarifying follow-up ... rather than
/// guessing" requirement.
pub fn resolve_phase_range(
    phases: &[PlanPhase],
    start_ref: &str,
    end_ref: &str,
) -> Result<Vec<PlanPhase>, PhaseRangeError> {
    let start_idx = phases
        .iter()
        .position(|p| plan::phase_ids_match(&p.id, start_ref))
        .ok_or_else(|| PhaseRangeError::PhaseNotFound {
            requested: start_ref.to_string(),
            boundary: "start",
        })?;
    let end_idx = phases
        .iter()
        .position(|p| plan::phase_ids_match(&p.id, end_ref))
        .ok_or_else(|| PhaseRangeError::PhaseNotFound {
            requested: end_ref.to_string(),
            boundary: "end",
        })?;

    if start_idx > end_idx {
        return Err(PhaseRangeError::RangeReversed {
            start: start_ref.to_string(),
            end: end_ref.to_string(),
        });
    }

    let range: Vec<PlanPhase> = phases[start_idx..=end_idx].to_vec();
    let range_ids: std::collections::HashSet<&str> = range.iter().map(|p| p.id.as_str()).collect();

    for phase in &range {
        for dep in &phase.depends_on {
            let in_range = range_ids.contains(dep.as_str())
                || range_ids.iter().any(|id| plan::phase_ids_match(id, dep));
            if in_range {
                continue;
            }
            let dep_done = phases
                .iter()
                .find(|p| plan::phase_ids_match(&p.id, dep))
                .map(|p| p.status == PlanStatus::Done)
                .unwrap_or(false);
            if !dep_done {
                return Err(PhaseRangeError::UnresolvedDependency {
                    phase_id: phase.id.clone(),
                    dep_id: dep.clone(),
                });
            }
        }
    }

    Ok(range)
}

// ── Per-phase graph instantiation (item 2) ───────────────────────────────

/// The shipped v0.17.7.3 reference graph, embedded so a project that hasn't
/// authored its own `.ta/workflows/graphs/<name>.toml` still gets a working
/// (advisory, per constitution §16.2) review panel — same fallback pattern
/// as `run_apply_gate`'s `default_apply_gate_graph_def`.
const DEFAULT_PHASE_REVIEW_PANEL_GRAPH: &str =
    include_str!("../../../../templates/workflows/graphs/phase-review-panel.toml");

/// Default graph name every resolved phase instantiates from unless the
/// caller overrides it — "overridable via a project-level default-graph
/// config" per item 2 means authoring `.ta/workflows/graphs/<name>.toml`
/// under this same name.
pub const DEFAULT_REVIEW_GRAPH_NAME: &str = "phase-review-panel";

fn load_phase_graph_definition(
    config: &GatewayConfig,
    name: &str,
) -> anyhow::Result<GraphDefinition> {
    match GraphDefinition::load_named(&config.workspace_root, name) {
        Ok(def) => Ok(def),
        Err(GraphError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            GraphDefinition::from_toml_str(DEFAULT_PHASE_REVIEW_PANEL_GRAPH).map_err(|e| {
                anyhow::anyhow!(
                    "built-in {DEFAULT_REVIEW_GRAPH_NAME} template failed to parse: {e}"
                )
            })
        }
        Err(e) => anyhow::bail!(
            "failed to load .ta/workflows/graphs/{name}.toml: {e}\n\
             Fix the graph definition, or delete the file to use the built-in default."
        ),
    }
}

/// What one phase's review-panel graph run produced, given an
/// already-built draft to review.
#[derive(Debug, Clone)]
pub struct PhaseGraphOutcome {
    pub decision: Option<Decision>,
    pub action_outcome: Option<ActionOutcome>,
}

/// Run the named review-panel graph (reviewers -> weighted decision ->
/// action — same shape as the shipped `phase-review-panel.toml`, no worker
/// section) against an already-built draft. One call = one resolved
/// phase's graph instance, per item 2.
pub fn run_phase_graph(
    phase: &PlanPhase,
    config: &GatewayConfig,
    review_graph_name: &str,
    draft_id: &str,
    run_id: &str,
) -> anyhow::Result<PhaseGraphOutcome> {
    let def = load_phase_graph_definition(config, review_graph_name)?;

    let registry = workflow_graph::build_registry(config.clone());
    let mut ctx = GraphContext::new(&config.workspace_root, run_id.to_string());
    ctx.vars
        .insert("draft_id".to_string(), draft_id.to_string());

    let review_input = ReviewInput {
        draft_id: Some(draft_id.to_string()),
        agent_id: "claude-code".to_string(),
        plan_phase: Some(phase.id.clone()),
        ..Default::default()
    };

    let outcome = ta_workflow::graph::run_graph(
        &def,
        &registry,
        &WorkItem::default(),
        &review_input,
        &mut ctx,
    )
    .map_err(|e| anyhow::anyhow!("phase '{}' review graph run failed: {e}", phase.id))?;

    Ok(PhaseGraphOutcome {
        decision: outcome.decision,
        action_outcome: outcome.action_outcome,
    })
}

// ── Dispatch + draft wait ─────────────────────────────────────────────────

/// Dispatch (start-only, fire-and-forget) a phase's implementation goal via
/// the same `GoalDispatchAction` machinery every other `WorkerNode` uses.
/// Returns the goal id — actually completing the implementation and
/// building a draft happens outside this call.
fn dispatch_phase(phase: &PlanPhase, config: &GatewayConfig) -> anyhow::Result<String> {
    let dispatcher = workflow_graph::GoalDispatchAction::new(config.clone());
    let work_item = WorkItem {
        title: format!("Implement {}: {}", phase.id, phase.title),
        objective: format!(
            "Implement plan phase {} — {}. See PLAN.md for the full item list.",
            phase.id, phase.title
        ),
        phase_id: Some(phase.id.clone()),
        verb: "implement".to_string(),
        workload_hint: None,
    };
    let ctx = GraphContext::new(&config.workspace_root, uuid::Uuid::new_v4().to_string());
    let result = dispatcher
        .dispatch(&work_item, &ctx)
        .map_err(|e| anyhow::anyhow!("phase '{}' dispatch failed: {e}", phase.id))?;
    Ok(result.draft_id)
}

/// Poll for a draft package to appear for `goal_id`. `None` means the poll
/// budget was exhausted first — not an error, just "still in progress."
fn wait_for_phase_draft(
    goal_id: &str,
    config: &GatewayConfig,
    poll_interval: Duration,
    max_polls: u32,
) -> anyhow::Result<Option<String>> {
    let max_polls = max_polls.max(1);
    for attempt in 0..max_polls {
        let packages = draft::load_all_packages(config)?;
        if let Some(pkg) = packages.into_iter().find(|p| p.goal.goal_id == goal_id) {
            return Ok(Some(pkg.package_id.to_string()));
        }
        if attempt + 1 < max_polls && !poll_interval.is_zero() {
            std::thread::sleep(poll_interval);
        }
    }
    Ok(None)
}

/// Extension point invoked synchronously right after a phase's goal is
/// dispatched, before `wait_for_phase_draft` starts polling — "how to
/// actually get this phase implemented." A plain `fn` pointer (not a
/// captured closure) so it's trivially `Send + Sync + 'static` and can be
/// copied into each wave's per-phase thread (`run_concurrently`). Public
/// callers (`handle_phase_range`) pass `None`; tests use it to fabricate a
/// draft synchronously instead of racing a real agent against a poll loop.
pub type ImplementPhaseHook = fn(&PlanPhase, &str, &GatewayConfig) -> anyhow::Result<()>;

// ── Chaining, CI-failure handling, escalation (items 3–6) ────────────────

/// Look up the VCS review (PR) id associated with a goal, via the draft
/// package `AutoApproveAction`'s `ta draft apply --submit` produces —
/// `None` until the goal's draft has actually been applied+submitted.
pub fn review_id_for_goal(goal_id: &str, config: &GatewayConfig) -> anyhow::Result<Option<String>> {
    let packages = draft::load_all_packages(config)?;
    Ok(packages
        .into_iter()
        .find(|p| p.goal.goal_id == goal_id)
        .and_then(|p| p.vcs_status)
        .and_then(|v| v.review_id))
}

/// Tunables for a multi-phase run — same defaults as `ta workflow watch-ci`
/// (`DEFAULT_TRIGGER_POLL_INTERVAL_SECS`/`DEFAULT_TRIGGER_MAX_POLLS`/retry
/// cap 1) so behavior is consistent whether CI-failure handling is driven
/// standalone or as part of a phase range.
#[derive(Debug, Clone)]
pub struct PhaseRangeConfig {
    pub review_graph_name: String,
    pub retry_cap: u32,
    pub poll_interval: Duration,
    pub max_polls: u32,
}

impl Default for PhaseRangeConfig {
    fn default() -> Self {
        Self {
            review_graph_name: DEFAULT_REVIEW_GRAPH_NAME.to_string(),
            retry_cap: 1,
            poll_interval: Duration::from_secs(30),
            max_polls: 120,
        }
    }
}

/// Outcome of driving one phase through dispatch -> draft-wait -> graph-run
/// -> CI-failure-watch -> merge-wait.
#[derive(Debug, Clone)]
pub struct PhaseRunResult {
    pub phase_id: String,
    pub goal_id: String,
    pub review_id: Option<String>,
    pub decision: Option<Decision>,
    pub action_outcome: Option<ActionOutcome>,
    pub ci_outcomes: Vec<ActionOutcome>,
    pub merged: bool,
    pub escalated: bool,
    pub escalation_reason: Option<String>,
}

fn escalated_result(mut result: PhaseRunResult, reason: impl Into<String>) -> PhaseRunResult {
    result.escalated = true;
    result.escalation_reason = Some(reason.into());
    result
}

/// Drive one phase end-to-end. See the module doc for the full
/// dispatch -> draft-wait -> review -> CI-watch -> merge-wait pipeline and
/// why each pause point is a soft `escalated` result, not a hard error.
fn run_single_phase(
    phase: &PlanPhase,
    config: &GatewayConfig,
    run_cfg: &PhaseRangeConfig,
    run_id: &str,
    implement_hook: Option<ImplementPhaseHook>,
    adapter_override: Option<Arc<dyn ta_submit::SourceAdapter>>,
    apply_lock: &std::sync::Mutex<()>,
) -> anyhow::Result<PhaseRunResult> {
    let goal_id = dispatch_phase(phase, config)?;

    let mut result = PhaseRunResult {
        phase_id: phase.id.clone(),
        goal_id: goal_id.clone(),
        review_id: None,
        decision: None,
        action_outcome: None,
        ci_outcomes: Vec::new(),
        merged: false,
        escalated: false,
        escalation_reason: None,
    };

    if let Some(hook) = implement_hook {
        hook(phase, &goal_id, config)?;
    }

    let draft_id =
        match wait_for_phase_draft(&goal_id, config, run_cfg.poll_interval, run_cfg.max_polls)? {
            Some(id) => id,
            None => {
                return Ok(escalated_result(
                    result,
                    format!(
                    "phase {} dispatched as goal {goal_id} — no draft has been built yet within \
                     the poll budget ({} poll(s) @ {:?}). Implement the phase and run `ta draft \
                     build`, then re-run this phase range to continue.",
                    phase.id, run_cfg.max_polls, run_cfg.poll_interval
                ),
                ));
            }
        };

    // The review-panel graph's action stage may call `ta draft apply`
    // (`AutoApproveAction`), which takes a project-wide file lock
    // (`.ta/apply.lock`) that rejects — rather than waits for — a
    // concurrent apply against the same working tree. Independent phases in
    // the same wave dispatch and wait for their drafts fully concurrently
    // (the slow, genuinely parallelizable part); only this brief
    // review+apply moment is serialized, so two phases never race to apply
    // at once.
    let graph_outcome = {
        let _guard = apply_lock.lock().unwrap_or_else(|e| e.into_inner());
        run_phase_graph(phase, config, &run_cfg.review_graph_name, &draft_id, run_id)?
    };
    result.decision = graph_outcome.decision.clone();
    result.action_outcome = graph_outcome.action_outcome.clone();

    let applied = graph_outcome
        .action_outcome
        .as_ref()
        .map(|a| a.applied)
        .unwrap_or(false);
    if !applied {
        let reason = match &graph_outcome.action_outcome {
            Some(a) if a.kind == "escalate" => a.message.clone(),
            Some(a) => format!(
                "phase {} panel decision did not apply (action '{}': {}) — needs human review",
                phase.id, a.kind, a.message
            ),
            None => format!(
                "phase {} graph run produced no [decision]/[action] — check the graph definition",
                phase.id
            ),
        };
        return Ok(escalated_result(result, reason));
    }

    let review_id = match review_id_for_goal(&goal_id, config)? {
        Some(id) => id,
        None => {
            return Ok(escalated_result(
                result,
                format!(
                    "phase {} was applied but no PR/review was opened for goal {goal_id} — cannot \
                     confirm merge or chain to the next phase",
                    phase.id
                ),
            ));
        }
    };
    result.review_id = Some(review_id.clone());

    let adapter =
        adapter_override.unwrap_or_else(|| workflow_graph::source_adapter_for_project(config));

    let ci_outcomes = workflow_graph::run_ci_failure_watch(
        adapter.clone(),
        &review_id,
        Some(&goal_id),
        run_cfg.retry_cap,
        run_cfg.poll_interval,
        run_cfg.max_polls,
        config,
    )?;
    let ci_escalated = ci_outcomes
        .last()
        .map(|o| o.metadata.get("escalated").map(String::as_str) == Some("true"))
        .unwrap_or(false);
    result.ci_outcomes = ci_outcomes;
    if ci_escalated {
        return Ok(escalated_result(
            result,
            format!(
                "phase {} — CI corrective-goal retry cap exhausted, escalating",
                phase.id
            ),
        ));
    }

    let merge_ctx = GraphContext::new(&config.workspace_root, format!("{run_id}-merge-wait"));
    let merge_trigger = workflow_graph::VcsTaskCompletionTrigger {
        adapter,
        review_id: review_id.clone(),
        poll_interval: run_cfg.poll_interval,
        max_polls: run_cfg.max_polls,
    };
    match merge_trigger.wait(&merge_ctx) {
        Ok(_) => result.merged = true,
        Err(e) => {
            return Ok(escalated_result(
                result,
                format!(
                    "phase {} — review {review_id} did not reach a terminal state within the \
                     poll budget: {e}",
                    phase.id
                ),
            ));
        }
    }

    Ok(result)
}

/// Full outcome of a multi-phase run: every phase actually attempted, plus
/// (when a phase escalated) which phases were never started.
#[derive(Debug, Clone)]
pub struct MultiPhaseRunOutcome {
    pub waves: Vec<Vec<String>>,
    pub results: Vec<PhaseRunResult>,
    pub halted_at: Option<String>,
    pub remaining: Vec<String>,
}

/// Drive a resolved phase range wave-by-wave: phases within a wave dispatch
/// concurrently (`ta_workflow::run_concurrently`, item 4); waves run
/// sequentially. A phase that escalates halts the *next* wave from starting
/// — phases already in flight within the same wave still run to completion,
/// since by construction they're independent of the escalated phase.
pub fn run_phase_range(
    phases: &[PlanPhase],
    waves: &[Vec<String>],
    config: &GatewayConfig,
    run_cfg: &PhaseRangeConfig,
) -> anyhow::Result<MultiPhaseRunOutcome> {
    run_phase_range_with_hook(phases, waves, config, run_cfg, None, None)
}

/// One wave's per-phase closures, indexed for `run_concurrently`'s
/// original-order result tagging.
type PhaseTasks = Vec<(
    usize,
    Box<dyn FnOnce() -> anyhow::Result<PhaseRunResult> + Send>,
)>;

fn run_phase_range_with_hook(
    phases: &[PlanPhase],
    waves: &[Vec<String>],
    config: &GatewayConfig,
    run_cfg: &PhaseRangeConfig,
    implement_hook: Option<ImplementPhaseHook>,
    adapter_override: Option<Arc<dyn ta_submit::SourceAdapter>>,
) -> anyhow::Result<MultiPhaseRunOutcome> {
    let phase_by_id: std::collections::HashMap<&str, &PlanPhase> =
        phases.iter().map(|p| (p.id.as_str(), p)).collect();

    let mut results: Vec<PhaseRunResult> = Vec::new();
    let mut halted_at: Option<String> = None;
    // Shared across every phase in the range (not per-wave) — serializes the
    // brief review+apply moment so concurrently-dispatched phases never
    // collide on `.ta/apply.lock`. See `run_single_phase`'s comment at its
    // `apply_lock.lock()` call site for why this doesn't reduce the
    // dispatch/draft-wait parallelism item 4 asks for.
    let apply_lock = Arc::new(std::sync::Mutex::new(()));

    'waves: for wave in waves {
        let tasks: PhaseTasks = wave
            .iter()
            .enumerate()
            .map(|(i, phase_id)| {
                let phase = (*phase_by_id
                    .get(phase_id.as_str())
                    .expect("wave id must be present in the resolved phase set"))
                .clone();
                let config = config.clone();
                let run_cfg = run_cfg.clone();
                let run_id = format!("multi-phase-{phase_id}");
                let adapter_override = adapter_override.clone();
                let apply_lock = Arc::clone(&apply_lock);
                let task: Box<dyn FnOnce() -> anyhow::Result<PhaseRunResult> + Send> =
                    Box::new(move || {
                        run_single_phase(
                            &phase,
                            &config,
                            &run_cfg,
                            &run_id,
                            implement_hook,
                            adapter_override,
                            &apply_lock,
                        )
                    });
                (i, task)
            })
            .collect();

        let mut outcomes = ta_workflow::run_concurrently(tasks);
        outcomes.sort_by_key(|(i, _)| *i);

        for (_, outcome) in outcomes {
            let result = outcome?;
            let escalated = result.escalated;
            let phase_id = result.phase_id.clone();
            results.push(result);
            if escalated {
                halted_at = Some(phase_id);
            }
        }

        if halted_at.is_some() {
            break 'waves;
        }
    }

    let attempted: std::collections::HashSet<&str> =
        results.iter().map(|r| r.phase_id.as_str()).collect();
    let remaining: Vec<String> = phases
        .iter()
        .map(|p| p.id.clone())
        .filter(|id| !attempted.contains(id.as_str()))
        .collect();

    Ok(MultiPhaseRunOutcome {
        waves: waves.to_vec(),
        results,
        halted_at,
        remaining,
    })
}

// ── Advisor entry point (item 1 + CLI wiring) ────────────────────────────

/// `ta advisor create "build phases X through Y"` lands here instead of the
/// single-goal pipeline (`ta_advisor::run_pipeline_with_security`) once
/// `ta_workflow::intent::extract_phase_range` matches two version refs with
/// a range connector between them.
pub fn handle_phase_range(
    config: &GatewayConfig,
    range: &PhaseRangeIntent,
    json_output: bool,
) -> anyhow::Result<()> {
    let phases = plan::load_plan(&config.workspace_root)?;
    let resolved = match resolve_phase_range(&phases, &range.start, &range.end) {
        Ok(r) => r,
        Err(e) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({"clarifying_question": e.to_string()})
                );
            } else {
                println!("[advisor] {e}");
            }
            return Ok(());
        }
    };

    let phase_ids: Vec<&str> = resolved.iter().map(|p| p.id.as_str()).collect();
    println!(
        "[advisor] resolved {} phase(s) for \"{}\" through \"{}\": {}",
        resolved.len(),
        range.start,
        range.end,
        phase_ids.join(", ")
    );

    let waves = plan::candidate_waves(&resolved).map_err(|e| {
        anyhow::anyhow!("failed to compute dependency waves for the requested range: {e}")
    })?;

    let run_cfg = PhaseRangeConfig::default();
    let outcome = run_phase_range(&resolved, &waves, config, &run_cfg)?;

    if json_output {
        print_json_outcome(&outcome)?;
    } else {
        print_outcome(&outcome);
    }
    Ok(())
}

fn print_outcome(outcome: &MultiPhaseRunOutcome) {
    for wave in &outcome.waves {
        println!("[advisor] wave: {}", wave.join(", "));
    }
    for result in &outcome.results {
        let status = if result.escalated {
            "ESCALATED"
        } else if result.merged {
            "MERGED"
        } else {
            "IN PROGRESS"
        };
        let score = result
            .decision
            .as_ref()
            .map(|d| format!("{:.2}", d.score))
            .unwrap_or_else(|| "-".to_string());
        let action_kind = result
            .action_outcome
            .as_ref()
            .map(|a| a.kind.as_str())
            .unwrap_or("-");
        println!(
            "[advisor] phase {} ({status}): goal={} review={} panel_score={score} action={action_kind}{}",
            result.phase_id,
            result.goal_id,
            result.review_id.as_deref().unwrap_or("-"),
            result
                .escalation_reason
                .as_ref()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default()
        );
    }
    match &outcome.halted_at {
        Some(halted) => println!(
            "[advisor] halted at phase {halted} — {} phase(s) paused for human review: {}",
            outcome.remaining.len(),
            outcome.remaining.join(", ")
        ),
        None => println!(
            "[advisor] range complete — all {} phase(s) merged.",
            outcome.results.len()
        ),
    }
}

fn print_json_outcome(outcome: &MultiPhaseRunOutcome) -> anyhow::Result<()> {
    let results: Vec<serde_json::Value> = outcome
        .results
        .iter()
        .map(|r| {
            serde_json::json!({
                "phase_id": r.phase_id,
                "goal_id": r.goal_id,
                "review_id": r.review_id,
                "panel_score": r.decision.as_ref().map(|d| d.score),
                "action_kind": r.action_outcome.as_ref().map(|a| a.kind.clone()),
                "merged": r.merged,
                "escalated": r.escalated,
                "escalation_reason": r.escalation_reason,
            })
        })
        .collect();
    let json = serde_json::json!({
        "waves": outcome.waves,
        "results": results,
        "halted_at": outcome.halted_at,
        "remaining": outcome.remaining,
    });
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::draft::DraftCommands;
    use std::path::Path;
    use tempfile::TempDir;

    fn phase(id: &str, status: PlanStatus, depends_on: &[&str]) -> PlanPhase {
        PlanPhase {
            id: id.to_string(),
            title: format!("Phase {id}"),
            status,
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            human_review_items: vec![],
            api_impact: vec![],
        }
    }

    // ── resolve_phase_range ──────────────────────────────────────────────

    #[test]
    fn resolve_phase_range_returns_inclusive_document_order_slice() {
        let phases = vec![
            phase("v0.17.1", PlanStatus::Done, &[]),
            phase("v0.17.2", PlanStatus::Pending, &["v0.17.1"]),
            phase("v0.17.3", PlanStatus::Pending, &["v0.17.2"]),
            phase("v0.17.4", PlanStatus::Pending, &["v0.17.3"]),
            phase("v0.17.5", PlanStatus::Pending, &["v0.17.4"]),
        ];
        let resolved = resolve_phase_range(&phases, "v0.17.2", "v0.17.4").unwrap();
        let ids: Vec<&str> = resolved.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["v0.17.2", "v0.17.3", "v0.17.4"]);
    }

    #[test]
    fn resolve_phase_range_errors_on_unknown_boundary() {
        let phases = vec![phase("v0.17.1", PlanStatus::Pending, &[])];
        let err = resolve_phase_range(&phases, "v0.17.1", "v0.99.9").unwrap_err();
        assert!(matches!(
            err,
            PhaseRangeError::PhaseNotFound {
                boundary: "end",
                ..
            }
        ));
    }

    #[test]
    fn resolve_phase_range_errors_on_reversed_range() {
        let phases = vec![
            phase("v0.17.1", PlanStatus::Pending, &[]),
            phase("v0.17.2", PlanStatus::Pending, &[]),
        ];
        let err = resolve_phase_range(&phases, "v0.17.2", "v0.17.1").unwrap_err();
        assert!(matches!(err, PhaseRangeError::RangeReversed { .. }));
    }

    #[test]
    fn resolve_phase_range_errors_when_crossing_unresolved_dependency() {
        // v0.17.5 depends on v0.17.1, which is outside the requested range
        // and not yet Done — must not silently guess this is safe.
        let phases = vec![
            phase("v0.17.1", PlanStatus::Pending, &[]),
            phase("v0.17.2", PlanStatus::Pending, &[]),
            phase("v0.17.3", PlanStatus::Pending, &[]),
            phase("v0.17.4", PlanStatus::Pending, &[]),
            phase("v0.17.5", PlanStatus::Pending, &["v0.17.1"]),
        ];
        let err = resolve_phase_range(&phases, "v0.17.3", "v0.17.5").unwrap_err();
        assert_eq!(
            err,
            PhaseRangeError::UnresolvedDependency {
                phase_id: "v0.17.5".to_string(),
                dep_id: "v0.17.1".to_string(),
            }
        );
    }

    #[test]
    fn resolve_phase_range_allows_dependency_on_an_already_done_phase_outside_range() {
        let phases = vec![
            phase("v0.17.1", PlanStatus::Done, &[]),
            phase("v0.17.2", PlanStatus::Pending, &[]),
            phase("v0.17.3", PlanStatus::Pending, &["v0.17.1"]),
        ];
        let resolved = resolve_phase_range(&phases, "v0.17.2", "v0.17.3").unwrap();
        assert_eq!(resolved.len(), 2);
    }

    // ── run_phase_graph / run_single_phase / run_phase_range ────────────

    /// Project fixture wiring `implement` to `test-agent` (fast, deterministic,
    /// no real subprocess) and, when `auto_approve` is true, an
    /// `.ta/workflows/graphs/phase-review-panel.toml` override whose
    /// `[action]` is binding — same shape as the shipped template but with
    /// `kind = "auto_approve"` instead of the advisory default, so a
    /// panel-cleared phase actually applies+opens a PR (per constitution
    /// §16.2, the shipped default is `recommend` and never applies on its
    /// own — chaining needs an explicit opt-in). `[submit] adapter = "none"`
    /// so `AutoApproveAction`'s `ta draft apply --submit` doesn't need a
    /// real git remote/gh CLI — the `none` adapter's `open_review` degrades
    /// gracefully and `check_review`'s synthetic status is what
    /// `VcsTaskCompletionTrigger` polls in these tests. Deliberately does
    /// NOT `git init` the project: `select_adapter` auto-detects "git" over
    /// a configured "none" the moment a `.git` directory exists, which
    /// would make these tests attempt a real `git push` with no remote.
    fn setup_project(project: &Path, auto_approve: bool) -> GatewayConfig {
        let ta_dir = project.join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            r#"
[workload_types.implement]
team = "implementer"
agent = "test-agent"

[submit]
adapter = "none"
"#,
        )
        .unwrap();

        if auto_approve {
            let graphs_dir = ta_dir.join("workflows").join("graphs");
            std::fs::create_dir_all(&graphs_dir).unwrap();
            let toml = DEFAULT_PHASE_REVIEW_PANEL_GRAPH.replace(
                "kind = \"recommend\"       # swap to \"auto_approve\" once a human upgrades this graph",
                "kind = \"auto_approve\"",
            );
            assert!(
                toml.contains("kind = \"auto_approve\""),
                "template replace must actually match — template text drifted"
            );
            std::fs::write(graphs_dir.join("phase-review-panel.toml"), toml).unwrap();
        }

        std::fs::write(project.join("README.md"), "# Original\n").unwrap();
        GatewayConfig::for_project(project)
    }

    fn seed_panel_verdicts(config: &GatewayConfig, run_id: &str, score: f64) {
        for role in ["pm", "head_of_security", "head_of_engineering"] {
            let verdict_dir = config
                .workspace_root
                .join(".ta")
                .join("workflow-runs")
                .join(run_id)
                .join("graph")
                .join("reviewers")
                .join(role);
            std::fs::create_dir_all(&verdict_dir).unwrap();
            std::fs::write(
                verdict_dir.join("verdict.json"),
                serde_json::json!({"score": score, "findings": []}).to_string(),
            )
            .unwrap();
        }
    }

    /// Simulates "an agent implemented the phase" between dispatch and the
    /// draft-wait poll — mutates the dispatched goal's workspace and builds
    /// a real draft via the same `ta draft build` code path production
    /// uses, so `run_single_phase`'s downstream review/apply stages exercise
    /// the real thing, not a stand-in fixture. A plain `fn` (no captures) so
    /// it coerces to `ImplementPhaseHook` and survives being copied into
    /// each wave's per-phase thread.
    fn build_fake_draft(
        phase: &PlanPhase,
        goal_id: &str,
        config: &GatewayConfig,
    ) -> anyhow::Result<()> {
        let store = ta_goal::GoalRunStore::new(&config.goals_dir)?;
        let uuid = uuid::Uuid::parse_str(goal_id)?;
        let goal = store
            .get(uuid)?
            .ok_or_else(|| anyhow::anyhow!("dispatched goal {goal_id} not found in store"))?;
        // A file unique to this phase (not a shared README.md) — multiple
        // phases run against the same project directory across waves, and
        // an earlier wave's applied change would otherwise make a later
        // phase's identical-content write look like "no changes" once its
        // staging workspace is copied from the already-updated source.
        std::fs::write(
            goal.workspace_path.join(format!("phase-{}.txt", phase.id)),
            format!("implemented {}\n", phase.id),
        )?;
        draft::execute(
            &DraftCommands::Build {
                goal_id: goal_id.to_string(),
                summary: "test phase implementation".to_string(),
                latest: false,
                apply_context_file: None,
            },
            config,
        )?;
        Ok(())
    }

    #[test]
    fn run_phase_graph_reviews_an_already_built_draft_and_returns_its_decision() {
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), false);
        let phase = phase("v0.17.7.4", PlanStatus::Pending, &[]);

        let goal_id = dispatch_phase(&phase, &config).unwrap();
        build_fake_draft(&phase, &goal_id, &config).unwrap();
        let draft_id = wait_for_phase_draft(&goal_id, &config, Duration::ZERO, 1)
            .unwrap()
            .expect("draft must exist immediately after build_fake_draft");

        seed_panel_verdicts(&config, "test-run-graph", 1.0);

        let outcome = run_phase_graph(
            &phase,
            &config,
            DEFAULT_REVIEW_GRAPH_NAME,
            &draft_id,
            "test-run-graph",
        )
        .expect("phase graph run must succeed");

        let decision = outcome.decision.unwrap();
        assert!(decision.proceed, "panel votes of 1.0 must clear threshold");
        // Shipped template defaults to `recommend` — never applies on its own.
        assert!(!outcome.action_outcome.unwrap().applied);
    }

    #[test]
    fn run_single_phase_pauses_when_no_draft_appears_within_the_poll_budget() {
        // No `implement_hook` and a tiny poll budget — the phase's
        // implementation is (correctly) still "in progress" from this
        // call's point of view. Must pause (escalated=true), not hang or
        // error out.
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), false);
        let phase = phase("v0.40.1", PlanStatus::Pending, &[]);
        let run_cfg = PhaseRangeConfig {
            poll_interval: Duration::ZERO,
            max_polls: 1,
            ..PhaseRangeConfig::default()
        };

        let result = run_single_phase(
            &phase,
            &config,
            &run_cfg,
            "test-run-pause",
            None,
            None,
            &std::sync::Mutex::new(()),
        )
        .unwrap();

        assert!(result.escalated);
        assert!(!result.goal_id.is_empty());
        assert!(result
            .escalation_reason
            .as_ref()
            .unwrap()
            .contains("no draft has been built yet"));

        let store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        assert!(
            store
                .list()
                .unwrap()
                .iter()
                .any(|g| g.title.contains("v0.40.1")),
            "must actually dispatch the phase's implementation goal even though it then pauses"
        );
    }

    /// The `none` adapter's `check_review` always returns `Ok(None)` (no
    /// review concept), so it can never simulate a merge — this stand-in
    /// reports every review as already `merged` on the first poll, letting
    /// the "full chain" test exercise `VcsTaskCompletionTrigger`'s success
    /// path deterministically, following the same minimal-trait-impl
    /// convention as `workflow_graph.rs`'s own `ScriptedAdapter` test double
    /// (`unimplemented!()` for methods this test never exercises).
    struct AutoMergeAdapter;

    impl ta_submit::SourceAdapter for AutoMergeAdapter {
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
            "auto-merge-test"
        }
        fn check_review(
            &self,
            _review_id: &str,
        ) -> ta_submit::adapter::Result<Option<ta_submit::ReviewStatus>> {
            Ok(Some(ta_submit::ReviewStatus {
                state: "merged".to_string(),
                checks_passing: Some(true),
            }))
        }
    }

    #[test]
    fn multi_phase_run_chains_two_independent_phases_through_merge() {
        // Two phases with no dependency between them land in the same wave
        // and must both run (and merge) without any manual "watch/pull/
        // install/launch next phase" step — item 3/4's payoff.
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), true);
        let phases = vec![
            phase("v0.20.1", PlanStatus::Pending, &[]),
            phase("v0.20.2", PlanStatus::Pending, &[]),
        ];
        let waves = plan::candidate_waves(&phases).unwrap();
        assert_eq!(
            waves,
            vec![vec!["v0.20.1".to_string(), "v0.20.2".to_string()]]
        );

        seed_panel_verdicts(&config, "multi-phase-v0.20.1", 1.0);
        seed_panel_verdicts(&config, "multi-phase-v0.20.2", 1.0);

        let run_cfg = PhaseRangeConfig {
            poll_interval: Duration::ZERO,
            max_polls: 5,
            ..PhaseRangeConfig::default()
        };
        let outcome = run_phase_range_with_hook(
            &phases,
            &waves,
            &config,
            &run_cfg,
            Some(build_fake_draft),
            Some(Arc::new(AutoMergeAdapter)),
        )
        .unwrap();

        assert_eq!(outcome.results.len(), 2, "outcomes: {:?}", outcome.results);
        assert!(
            outcome.halted_at.is_none(),
            "expected no escalation: {:?}",
            outcome.results
        );
        assert!(outcome.remaining.is_empty());
        assert!(outcome.results.iter().all(|r| r.merged));
    }

    #[test]
    fn multi_phase_run_escalates_and_halts_remaining_phases_on_low_panel_score() {
        // v0.30.1 -> v0.30.2 sequential (dependency). v0.30.1's panel score
        // is low (0.1, below the shipped 0.75 threshold) and the graph is
        // advisory (`recommend`, the constitution §16.2 default) — the
        // action never applies, so the range must halt before v0.30.2 is
        // ever attempted, per item 6.
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), false);
        let phases = vec![
            phase("v0.30.1", PlanStatus::Pending, &[]),
            phase("v0.30.2", PlanStatus::Pending, &["v0.30.1"]),
        ];
        let waves = plan::candidate_waves(&phases).unwrap();
        assert_eq!(
            waves,
            vec![vec!["v0.30.1".to_string()], vec!["v0.30.2".to_string()]]
        );

        seed_panel_verdicts(&config, "multi-phase-v0.30.1", 0.1);

        let run_cfg = PhaseRangeConfig {
            poll_interval: Duration::ZERO,
            max_polls: 3,
            ..PhaseRangeConfig::default()
        };
        let outcome = run_phase_range_with_hook(
            &phases,
            &waves,
            &config,
            &run_cfg,
            Some(build_fake_draft),
            None,
        )
        .unwrap();

        assert_eq!(outcome.results.len(), 1, "must not attempt v0.30.2");
        assert!(outcome.results[0].escalated);
        assert_eq!(outcome.halted_at.as_deref(), Some("v0.30.1"));
        assert_eq!(outcome.remaining, vec!["v0.30.2".to_string()]);
    }

    #[test]
    fn multi_phase_run_resolves_three_phase_range_into_correct_waves_and_order() {
        // v0.60.1, v0.60.2 independent; v0.60.3 depends on v0.60.1 — item
        // 7's "mocked 3-phase range (2 independent, 1 dependent) resolves
        // into the correct wave structure and executes in the right order."
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), true);
        let phases = vec![
            phase("v0.60.1", PlanStatus::Pending, &[]),
            phase("v0.60.2", PlanStatus::Pending, &[]),
            phase("v0.60.3", PlanStatus::Pending, &["v0.60.1"]),
        ];
        let waves = plan::candidate_waves(&phases).unwrap();
        assert_eq!(
            waves,
            vec![
                vec!["v0.60.1".to_string(), "v0.60.2".to_string()],
                vec!["v0.60.3".to_string()],
            ]
        );

        seed_panel_verdicts(&config, "multi-phase-v0.60.1", 1.0);
        seed_panel_verdicts(&config, "multi-phase-v0.60.2", 1.0);
        seed_panel_verdicts(&config, "multi-phase-v0.60.3", 1.0);

        let run_cfg = PhaseRangeConfig {
            poll_interval: Duration::ZERO,
            max_polls: 5,
            ..PhaseRangeConfig::default()
        };
        let outcome = run_phase_range_with_hook(
            &phases,
            &waves,
            &config,
            &run_cfg,
            Some(build_fake_draft),
            Some(Arc::new(AutoMergeAdapter)),
        )
        .unwrap();

        assert_eq!(outcome.results.len(), 3, "outcomes: {:?}", outcome.results);
        assert!(
            outcome.halted_at.is_none(),
            "expected no escalation: {:?}",
            outcome.results
        );
        assert!(outcome.results.iter().all(|r| r.merged));

        // Wave order: v0.60.3 (wave 2) must land after both wave-1 phases
        // in the results — proves waves ran sequentially, not all three
        // dispatched at once.
        let pos = |id: &str| {
            outcome
                .results
                .iter()
                .position(|r| r.phase_id == id)
                .unwrap()
        };
        assert!(pos("v0.60.3") > pos("v0.60.1"));
        assert!(pos("v0.60.3") > pos("v0.60.2"));
    }

    /// Scripted adapter for the "CI fails once mid-phase, then clears" test:
    /// the first `check_review()` call reports a CI failure (fires
    /// `CiFailureTrigger`), every call after reports `merged` — same
    /// scripted-sequence convention as `workflow_graph.rs`'s own
    /// `ScriptedAdapter` test double.
    struct CiFailureThenMergeAdapter {
        call_count: std::sync::Mutex<usize>,
    }

    impl ta_submit::SourceAdapter for CiFailureThenMergeAdapter {
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
            "ci-failure-then-merge-test"
        }
        fn check_review(
            &self,
            _review_id: &str,
        ) -> ta_submit::adapter::Result<Option<ta_submit::ReviewStatus>> {
            let mut count = self.call_count.lock().unwrap();
            let n = *count;
            *count += 1;
            if n == 0 {
                Ok(Some(ta_submit::ReviewStatus {
                    state: "open".to_string(),
                    checks_passing: Some(false),
                }))
            } else {
                Ok(Some(ta_submit::ReviewStatus {
                    state: "merged".to_string(),
                    checks_passing: Some(true),
                }))
            }
        }
        fn check_failures(
            &self,
            _review_id: &str,
        ) -> ta_submit::adapter::Result<Vec<ta_submit::CheckFailure>> {
            Ok(vec![ta_submit::CheckFailure {
                check_name: "build".to_string(),
                log_excerpt: "flaky failure".to_string(),
            }])
        }
    }

    #[test]
    fn run_single_phase_resumes_after_a_ci_failure_clears_mid_phase() {
        // item 7: "an injected CI failure mid-range triggers a corrective
        // goal and the range resumes after it clears." One phase is enough
        // to exercise the mechanism `run_phase_range` reuses for every
        // phase in a range: dispatch -> review -> apply -> CI fails once ->
        // corrective goal launched -> CI clears -> merge-wait succeeds.
        let project = TempDir::new().unwrap();
        let config = setup_project(project.path(), true);
        let phase = phase("v0.70.1", PlanStatus::Pending, &[]);

        seed_panel_verdicts(&config, "multi-phase-v0.70.1", 1.0);

        let run_cfg = PhaseRangeConfig {
            poll_interval: Duration::ZERO,
            max_polls: 5,
            ..PhaseRangeConfig::default()
        };
        let adapter: Arc<dyn ta_submit::SourceAdapter> = Arc::new(CiFailureThenMergeAdapter {
            call_count: std::sync::Mutex::new(0),
        });
        let result = run_single_phase(
            &phase,
            &config,
            &run_cfg,
            "multi-phase-v0.70.1",
            Some(build_fake_draft),
            Some(adapter),
            &std::sync::Mutex::new(()),
        )
        .unwrap();

        assert!(
            !result.escalated,
            "expected the phase to resume after CI clears: {:?}",
            result
        );
        assert!(result.merged);
        assert_eq!(
            result.ci_outcomes.len(),
            1,
            "expected exactly one corrective-goal dispatch"
        );
        assert!(
            result.ci_outcomes[0].applied,
            "the one CI failure must launch a follow-up fix, not escalate"
        );
    }

    #[test]
    fn handle_phase_range_prints_clarifying_question_instead_of_running() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        std::fs::write(
            project.path().join("PLAN.md"),
            "### v0.1.0 — Only Phase\n<!-- status: pending -->\n",
        )
        .unwrap();
        let config = GatewayConfig::for_project(project.path());

        let range = PhaseRangeIntent {
            start: "v0.1.0".to_string(),
            end: "v9.9.9".to_string(),
        };
        // Must return Ok (a clarifying question is a normal outcome, not an
        // error) without dispatching anything.
        handle_phase_range(&config, &range, false).unwrap();
        let store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
