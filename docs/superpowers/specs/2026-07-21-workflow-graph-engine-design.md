# Workflow Graph Engine — Design Spec

**Status**: Draft, red-teamed 2026-07-21 (PM, head-of-engineering, non-technical-user passes), pending user review
**Target phases**: v0.17.7.1 – v0.17.7.4
**Author context**: Written during the v0.17.0 overhaul session, after discovering three disconnected auto-approval mechanisms (`ta_policy::auto_approve`, `ta_session::advisor_agent::check_advisor_auto_approve`, and `ta-workflow::consensus` via the governed-workflow engine) and a PR-merge continuation (`execute_pr_merged_continuation`, v0.17.0.12.31) that is Git/GitHub-specific and has no CI-failure-inspection loop.

**Disclosed limitation up front** (raised by the non-technical-user red-team pass): the only piece of this feature directly usable by a non-engineer in v0.17.7 is §6's natural-language advisor entry point ("build phases X through Y"). Everything else — authoring or editing a graph — means hand-editing a TOML file until the visual builder ships in v0.18+. §6 is sequenced last (7.7.4) not because it's deprioritized, but because it's the one piece that depends on the other three existing first; it is still the intended primary user-facing payoff of this whole spec, not an afterthought.

## 1. Problem

This session ran an autonomous multi-phase build loop entirely by hand: launch a goal, wait, review the draft, approve, apply, watch the resulting PR's CI, diagnose and fix failures, wait for merge, pull, rebuild, install, restart the daemon, launch the next phase. Every step that could be automated already has *a* mechanism somewhere in the codebase — but each one is a separate, hardcoded, non-composable path:

- **Approval scoring** exists three different ways (policy rules, advisor-confidence, consensus panel), none aware of the others, none shareable.
- **Multi-phase continuation** exists (`execute_pr_merged_continuation`) but only reacts to a webhook claiming a PR already merged — it has no CI-watch, no failure diagnosis, and it's wired directly to `gh`/GitHub semantics rather than the project's own `SourceAdapter` abstraction (which already supports Perforce/SVN/custom adapters per §2.3 of the constitution).
- **Multi-role review panels** (PM, security, engineering, sales, etc.) are describable today only inside the separate "governed-workflow YAML" engine, with the consensus engine's threshold/algorithm/weights hardcoded as literals — not configurable per use.

The user's explicit direction: stop hardcoding these as separate processes. Build them as **modular, data-wired components** — a small set of node types that fan out, collect with weights, and decide — where "the CLI's current approval gate" is just one particular wiring of the same pieces that could just as easily produce a human-facing recommendation instead of an auto-approval. Visual editing of these graphs is explicitly deferred (bundled into v0.18 alongside the Studio Next.js migration and the plan-DAG visualization) — v0.17.7 builds the engine and a **data-file** (TOML) wiring surface only.

## 2. Node Model

A workflow graph is a set of typed nodes connected by typed edges, executed by a new `ta-workflow::graph` engine. Every node type implements one of four traits:

```rust
trait TriggerSource {
    /// Blocks/polls until the event fires; returns a typed payload.
    fn wait(&self, ctx: &GraphContext) -> Result<TriggerPayload>;
}

trait ReviewerNode {
    /// Produces one scored vote. Wraps today's three mechanisms as
    /// interchangeable implementations of the SAME trait.
    fn review(&self, input: &ReviewInput, ctx: &GraphContext) -> Result<ReviewerVote>;
}

trait DecisionNode {
    /// Fans in N ReviewerVotes, applies weights/threshold/algorithm,
    /// emits a typed Decision (score + verdict). Does NOT act.
    fn decide(&self, votes: &[ReviewerVote], ctx: &GraphContext) -> Result<Decision>;
}

trait ActionNode {
    /// Consumes a Decision and performs an effect.
    fn act(&self, decision: &Decision, ctx: &GraphContext) -> Result<ActionOutcome>;
}
```

`ReviewerVote` and `Decision` are the existing types from `ta-workflow::consensus` (`ReviewerVote{role, score, findings, timed_out}`, threshold/algorithm-driven verdict) — reused, not reinvented, per constitution §1.7.

### 2.1 v1 node catalog

| Node | Trait | Implementation notes |
|---|---|---|
| `VcsTaskCompletionTrigger` | `TriggerSource` | Fires on a VCS review reaching a terminal or CI state, via `SourceAdapter::check_review()` polling (or a webhook where the adapter supports one) — **not** a `gh`-specific call. Payload includes `ReviewStatus` (state, `checks_passing`). |
| `CiFailureTrigger` | `TriggerSource` | Fires specifically when `checks_passing` transitions to `Some(false)`. Requires a new `SourceAdapter` method (§4) to fetch which check(s) failed and their logs, since today's `ReviewStatus.checks_passing` is a single opaque bool with no per-check detail. |
| `PolicyReviewer` | `ReviewerNode` | Wraps existing `ta_policy::auto_approve::should_auto_approve_draft` rule evaluation as one scored vote. |
| `AdvisorConfidenceReviewer` | `ReviewerNode` | Wraps existing `ta-decision::gate::decide()` confidence score as one scored vote. |
| `AgentPanelReviewer` | `ReviewerNode` | Spawns a persona agent (e.g. "head of security") to score a draft/decision; role is a plain string per constitution §1.6 (`TeamRole` is already data-defined, confirmed v0.17.0.12.12 — new roles like `head_of_sales` need no core change). |
| `WeightedDecisionNode` | `DecisionNode` | Thin wrapper over `ta-workflow::consensus::run_consensus` — the difference from today is the algorithm/threshold/weights come from the graph's TOML config, not hardcoded literals (fixes the gap found in `governed_workflow.rs`'s `stage_consensus`). |
| `AutoApproveAction` | `ActionNode` | Calls `ta draft apply` (existing apply path, existing audit trail) when `Decision.verdict == Pass`. |
| `RecommendAction` | `ActionNode` | Surfaces the `Decision` to a human via Studio's existing Attention queue — no apply call. |
| `EscalateAction` | `ActionNode` | Notifies via the existing notification system (`ta-events::notification`) and halts the graph at this node. |
| `CorrectiveGoalAction` | `ActionNode` | On a `CiFailureTrigger`'s payload, launches a follow-up goal (`ta run --follow-up`) with the failure detail injected as the goal's objective — generalizes what I did manually this session (read failing job log, diagnose, fix, push) into a graph node. |

**Key property**: `AutoApproveAction` and `RecommendAction` consume the exact same `Decision` type. Whether a given `WeightedDecisionNode`'s output becomes binding or advisory is which `ActionNode` the graph wires it to — not a difference in how the decision was computed. This directly satisfies "recommendation vs. auto-approval is just data wiring."

## 3. Graph Definition (data-defined, per constitution §1.6)

```toml
# .ta/workflows/graphs/phase-review-panel.toml
[[trigger]]
id = "draft_ready"
kind = "vcs_task_completion"

[[reviewer]]
id = "policy_check"
kind = "policy"

[[reviewer]]
id = "pm_score"
kind = "agent_panel"
role = "pm"

[[reviewer]]
id = "security_score"
kind = "agent_panel"
role = "head_of_security"

[[reviewer]]
id = "engineering_score"
kind = "agent_panel"
role = "head_of_engineering"

[decision]
id = "panel_verdict"
kind = "weighted"
algorithm = "weighted"       # was hardcoded; now configurable
threshold = 0.75
inputs = ["policy_check", "pm_score", "security_score", "engineering_score"]
weights = { policy_check = 1.0, pm_score = 1.0, security_score = 1.5, engineering_score = 1.0 }

[action]
id = "outcome"
kind = "auto_approve"        # swap to "recommend" for advisory-only, same graph otherwise
decision = "panel_verdict"
```

A second graph, `ci-failure-response.toml`, wires `CiFailureTrigger` → (no reviewers needed) → `CorrectiveGoalAction` directly, since a build failure doesn't need a panel vote — it needs a fix.

## 4. VCS Adapter Extension (generalizing off Git)

Today's `SourceAdapter::check_review()` returns `Option<ReviewStatus>` with only `checks_passing: Option<bool>` — no per-check name or failure detail (`crates/ta-submit/src/adapter.rs:456-461`). This is insufficient for `CiFailureTrigger`/`CorrectiveGoalAction` to inspect *what* failed.

**Addition**: a new adapter method, following the trait's existing default-no-op pattern so non-CI adapters (SVN, "none") aren't forced to implement it:

```rust
/// Fetch per-check failure detail for a review, if the adapter's platform
/// exposes it. Git/GitHub: shells `gh run view --log-failed` for the PR's
/// failing checks. Default: empty (adapter/platform doesn't expose this).
fn check_failures(&self, _review_id: &str) -> Result<Vec<CheckFailure>> {
    Ok(vec![])
}

pub struct CheckFailure {
    pub check_name: String,
    pub log_excerpt: String,   // adapter-specific: last N lines, or a fetch-log command hint
}
```

This keeps the VCS-agnostic contract intact (constitution §2.3: Stage/Submit/Review, never Git-specific terminology in the trait) while letting `CiFailureTrigger` work identically regardless of which `SourceAdapter` is configured — a Perforce or custom adapter that can't expose failure logs just returns an empty vec, and `CorrectiveGoalAction` degrades to "CI failed, no detail available, investigate manually."

## 5. Reuse Inventory (constitution §1.7 compliance)

| Existing capability | Reused as |
|---|---|
| `ta-workflow::consensus` (Raft/Paxos/Weighted, `ReviewerVote`) | `WeightedDecisionNode`'s engine, config surface unlocked |
| `ta_policy::auto_approve::should_auto_approve_draft` | `PolicyReviewer` implementation |
| `ta-decision::gate::decide()` confidence score | `AdvisorConfidenceReviewer` implementation |
| `TeamRole` (data-defined since 12.12) | Role strings for `AgentPanelReviewer`, no core change for new roles |
| 12.31's auto-fix-retry mechanism (`decide_gate_failure_action`/`GateFailureMode::AutoFix`) | Retry policy inside `CorrectiveGoalAction`, capped, escalates to human on repeat failure — **not** a second auto-fix mechanism |
| 12.34's dependency-wave planner + `run_concurrently` | Dispatch mechanism when a graph triggers multiple independent phases at once (parallel-safe wave) |
| Studio's existing Attention queue / draft approve-apply UI | `RecommendAction`'s and `EscalateAction`'s human-facing surface — no new UI needed for v0.17.7 |

**Explicitly not built in v0.17.7**: any visual graph editor/canvas (deferred to v0.18+ per user direction), a fourth approval mechanism, a second CI-retry mechanism, Git-specific trigger code (must go through `SourceAdapter`).

## 6. Advisor NL Entry Point ("Build phases X through Y")

`ta-brain::route()` is single-goal only today (`crates/ta-brain/src/route.rs:184`) and `serial-phases` needs an explicit comma list (`crates/ta-workflow/src/serial_phases.rs`). New advisor parsing: resolve a natural-language phase range against PLAN.md's existing dependency/ordering data (already available via 12.30/12.34), then construct one `phase-review-panel`-style graph instance per phase, chained by a `VcsTaskCompletionTrigger` on the previous phase's merge — using dependency-wave planning to run independent phases in parallel where safe, falling back to sequential otherwise. This is the layer that finally replaces "launch, wait, review, apply, watch PR, fix, pull, build, repeat" with one command.

## 7. Phased Implementation Plan

- **v0.17.7.1 — Node trait model + graph config format + engine core.** Define the four traits, the TOML schema, a `ta-workflow::graph` module that loads a graph definition and executes it node-by-node with typed data passed along edges. Ship `PolicyReviewer`, `AdvisorConfidenceReviewer`, `WeightedDecisionNode` (config-driven, no hardcoded threshold), `AutoApproveAction`, `RecommendAction` — enough to express today's existing single-reviewer approval flow as a graph, proving the abstraction before adding panels.
- **v0.17.7.2 — VCS-adapter CI-status generalization.** Add `check_failures()` to `SourceAdapter` (default no-op), implement it for the Git adapter, add `VcsTaskCompletionTrigger` and `CiFailureTrigger`, add `CorrectiveGoalAction` reusing 12.31's retry-cap/escalate logic.
- **v0.17.7.3 — Multi-role panels + approval-gate unification.** Add `AgentPanelReviewer`, wire real weight/threshold/algorithm config through from the TOML graph into `ta-workflow::consensus` (removing the `governed_workflow.rs` hardcoded literals), and migrate `ta draft apply`'s real gate to consult the graph engine instead of the three disconnected mechanisms directly — this is the unification the user flagged as a real risk if left unaddressed.
- **v0.17.7.4 — Advisor NL phase-range entry point.** "Build phases v0.17.3 through v0.17.8" parsing, per-phase graph instantiation, PR-merge chaining via `VcsTaskCompletionTrigger`, dependency-wave dispatch for parallel-safe phases.

Each sub-phase depends on the previous (`Depends on` field per PLAN.md convention); 7.2 could in principle run parallel to 7.1 (different files) but is kept sequential here since 7.3's gate migration needs both.

## 8. Constitution Change

A new constitution principle is required — see `docs/TA-CONSTITUTION.md` §16 (added alongside this spec, red-teamed by three adversarial passes 2026-07-21 — PM, head-of-engineering, non-technical-user). Summary of the final §16, after revisions from that review:

- **16.1** — new approval/gating logic (specifically anything producing a `Decision{verdict}` consumed by an Action node) goes through the graph; scoped narrowly to apply/merge/escalate decisions, not every internal validation check.
- **16.2** — recommendation vs. auto-approval is purely which `ActionNode` a graph is wired to, never encoded in the `DecisionNode` itself; **new graphs default to `RecommendAction`** until a human explicitly upgrades one to `AutoApproveAction` (mirrors §1.3 Human-in-the-Loop).
- **16.3** — a named, structural call-site invariant: `ta draft apply` calls exactly one graph instance; no other code path may call `should_auto_approve_draft`/`check_advisor_auto_approve`/`run_consensus` directly for gating, no exceptions.
- **16.4** — triggers go through `SourceAdapter`, never a specific platform's API directly; a permanent no-op for platforms lacking the concept (e.g., Perforce has no "CI check") is explicitly compliant, not a gap.
- **16.5** — graphs are data files, not compiled logic; the decision-*algorithm* set (Raft/Paxos/Weighted) may stay a closed enum under §1.6's own finite-set carve-out — it's the wiring that must be data.

The red-team review's other findings (tightening scope language, naming the enforcement mechanism concretely, disclosing the TOML-only/no-visual-editor gap plainly) are folded directly into the sections above rather than listed as unresolved.

## 9. Open Questions / Deferred

- Visual graph authoring/editing UI — deferred to v0.18+ (bundled with Studio's Next.js migration and the plan-DAG visualization).
- Full drag-and-drop workflow-authoring canvas (editing `depends_on`/`api_impact` visually) — deferred further, 0.19+.
- Whether non-Git `SourceAdapter` implementations (Perforce/SVN) get real `check_failures()` implementations, or stay at the empty-vec default indefinitely — not scoped here; the trait contract makes this an additive, non-blocking follow-up per adapter.
