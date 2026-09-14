# Sub-Project 2: PLAN.md Phases ↔ Wayfinder KPI/Wave Sync

**Status**: proposed, 2026-09-14. TA-side design only (`ta-plan`, `ta-plan-wayfinder`, `ta-cli`) — no Wayfinder-repo changes; that side (wave HTTP surfacing) is already done, see `docs/superpowers/specs/2026-09-13-eight-role-roster-and-program-roadmap-design.md` sub-project 2 scoping and the merged `wayfinder` PR #161.

## Grounding: what exists today, verified by direct source read (2026-09-14)

- **`ta-plan-wayfinder`** (merged, PR #604) already pushes `PLAN.md` phases into Wayfinder as `Task`s via `ensure_phase_gate_task` (`store.rs`), with dependency edges (`add_dependency`) and bidirectional best-effort status sync. Goal-runs are pushed too (`push_goal`), with `description: Some(goal.objective.clone())` — goals already carry real content.
- **Phase-gate tasks do not**: `ensure_phase_gate_task`'s `CreateTaskRequest` sets `description: None`. Wayfinder's `POST /api/tasks` (`wayfinder-api/src/routes/tasks.rs`'s `create_task`) auto-classifies every new task against the project's configured KPIs and links them (`classify_against_kpis` → `link_kpis_to_task`) — already fully automatic, no TA-side work needed for the linking mechanism itself, but a task with no description gives that classifier only a bare phase title to score against.
- **`PlanPhase`** (`ta-plan/src/schema.rs`) has no field carrying a phase's `**Goal**:` prose line — only `id`, `title`, `status`, `depends_on`, `human_review_items`, `api_impact`. `depends_on` is already parsed from a `**Depends on**: ...` prose line (`find_depends_on_in_lookahead`) — the same parsing pattern this needs, just a different label.
- **Wayfinder's `Wave`/dispatch API** (now live, `wayfinder` PR #161, merged 2026-09-14): `POST /api/dispatch` returns `{wave_id: Option<String>, results: [...]}`; `GET /api/waves`, `GET /api/waves/:id` list/fetch wave membership. `ta-plan-wayfinder`'s `client.rs` calls none of these today — it only calls `list_tasks`, `upsert_task`, `update_task_status`, `add_dependency`, `export`.
- **TA already has its own, separate "wave" concept**: `ta_plan::query::candidate_waves` (`ta-plan/src/query.rs`) computes a purely local, dependency-graph-derived grouping of phases via `ta_workflow::WaveNode` — "which phases could theoretically run in parallel," based only on `depends_on`/`api_impact`, no network call, no Wayfinder involvement. This is a **different question** from Wayfinder's `Wave` ("what got dispatched together in one real dispatch call, given the live ready-queue"). This design keeps them separate — reconciling/unifying the two concepts is explicitly out of scope (see below), not a gap to silently paper over.
- **No CLI wiring exists** for anything Wayfinder-specific in `ta-plan` today (confirmed: no `wayfinder` string anywhere in `apps/ta-cli/src/commands/plan.rs`) — `select_plan_store`'s Wayfinder backend is reachable only by whatever already calls the generic `PlanStore` trait via `.ta/workflow.toml`'s `[plan] backend = "wayfinder"`. Dispatch/wave data has no home on that generic trait and shouldn't get one (see Deliverable 3's rationale).

## Non-negotiable safety boundary

**Dispatch output is advisory only. Nothing in this sub-project auto-launches `ta run` or any goal.** Wayfinder's dispatch decisions (who Wayfinder's own algorithm thinks should work on what, and when) are surfaced to a human (or the chief-of-staff persona, who is itself an LLM a human is supervising) to read and act on manually — never wired to automatically start execution. This follows directly from this project's standing rule: never launch `ta run` autonomously, always require explicit human confirmation before starting a goal. A "waves of execution" feature that silently starts executing would violate that rule outright, not bend it.

## Deliverables

1. **`PlanPhase.goal: Option<String>`** (`ta-plan/src/schema.rs` + parser): parsed from a phase's `**Goal**: ...` prose line, mirroring `depends_on`'s existing `find_depends_on_in_lookahead` pattern exactly (same lookahead-scan shape, different label and no comma-splitting). `None` when a phase has no such line (some don't). Additive, `#[serde(default)]`, no breaking change to existing parsed plans.

2. **`ensure_phase_gate_task` passes real content** (`ta-plan-wayfinder/src/store.rs`): `description: phase.goal.clone()` instead of `description: None`. This is the entire fix needed for KPI classification quality — Wayfinder's classifier already runs automatically on task creation; it just needs something to read.

3. **`WayfinderClient` dispatch/wave methods** (`ta-plan-wayfinder/src/client.rs`), following the existing method style (`upsert_task`, `update_task_status`) exactly — same error handling, same test pattern (wiremock-based, see existing `client.rs` tests):
   - `trigger_dispatch() -> Result<DispatchOutcome, WayfinderClientError>` — `POST /api/dispatch`, parses `{wave_id, results}`.
   - `get_wave(wave_id: &str) -> Result<WaveDto, WayfinderClientError>` — `GET /api/waves/:id`.
   - `list_waves() -> Result<Vec<WaveSummaryDto>, WayfinderClientError>` — `GET /api/waves`.

4. **`ta plan wayfinder dispatch` CLI subcommand** (new, `apps/ta-cli/src/commands/plan.rs` or a new `plan/wayfinder.rs` submodule — follow whatever this file's existing subcommand-grouping convention is). Deliberately **not** a `PlanStore` trait method — dispatch/wave is a Wayfinder-specific concept with no `FilePlanStore` equivalent, and forcing it onto the generic trait would mean either a leaky no-op default impl or an `Option`-typed trait method every other backend has to think about. Instead this subcommand:
   - Requires `[plan] backend = "wayfinder"` in `.ta/workflow.toml` — clear error naming the missing config if not set, not a silent no-op.
   - Pushes any locally-dirty phase statuses first (reuses `WayfinderPlanStore`'s existing best-effort push path — a dispatch call is only useful against up-to-date task state).
   - Calls `trigger_dispatch()`.
   - Prints the resulting wave's decisions human-readably: which phase-gate/goal tasks got which decision (assign/hold/escalate, whatever `DispatchResult`'s decision variants are — mirror `wayfinder-api`'s `DispatchResultDto` shape for the CLI's output fields), and the wave id for later reference via `ta plan wayfinder wave <id>` (a small companion read-only subcommand, same PR).
   - Prints "no new wave (nothing changed)" plainly when `wave_id` is `None` — an honest, actionable message per the Observability Mandate, not silence.

## Explicitly out of scope

- **Reconciling `candidate_waves` (TA's local dependency-derived grouping) with Wayfinder's `Wave` (dispatch-time cohort)** — different questions, deliberately kept separate per Grounding above. Revisit only if real use shows the two groupings actively confuse users side by side, not speculatively now.
- **Configuring KPIs themselves** in Wayfinder's registry (org/project KPI definitions, weights) — a Wayfinder-admin action through Wayfinder's own UI/API, not something this sub-project's CLI needs to wrap.
- **Any automatic goal/`ta run` launch from dispatch output** — see Non-negotiable safety boundary above. Not a phased-in feature to add later either, without a separate, explicit design conversation about the safety implications.
- **Wayfinder-repo changes of any kind** — already done (PR #161).
- **A `ta plan wayfinder bootstrap` CLI command** for initial project-to-Wayfinder linking — a separate, already-identified gap (see project memory), not this sub-project's concern; this design assumes a project is already bootstrapped/configured against Wayfinder.

## Tests

- `ta-plan`: parser test that a `**Goal**: ...` line populates `PlanPhase.goal`, and that its absence leaves `goal: None` (not an error) — mirror the existing `depends_on` parser tests' style exactly.
- `ta-plan-wayfinder/store.rs`: `ensure_phase_gate_task` sends the phase's `goal` text as `description` in the `CreateTaskRequest` it builds (assert on the constructed request, same style as existing store tests).
- `ta-plan-wayfinder/client.rs`: wiremock tests for `trigger_dispatch`/`get_wave`/`list_waves` mirroring the file's existing `upsert_task`/`update_task_status` test pattern — success path, a `wave_id: None` no-op-replay response parses correctly, auth/error-surfacing consistent with existing methods (including the existing "bearer secret never appears in error messages" discipline).
- CLI: an integration-style test (wherever this repo's existing `ta plan` subcommand tests live) that `ta plan wayfinder dispatch` without `[plan] backend = "wayfinder"` configured fails with a clear, actionable error rather than silently doing nothing.
