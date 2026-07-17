# Red-Team Autoreward: Adversarial Validation of Auto-Confirmed Verifications

Phase: v0.17.0.12.27. Depends on v0.17.0.12.26 (`ta_human_verify`).

Note on process: this phase ran as a headless, non-interactive TA-mediated
goal (no user available mid-session to answer clarifying questions). This
document was authored by the agent from the PLAN.md phase text, the prior
phase's implementation, and one explicit CLI-naming deviation called out
below, rather than through live back-and-forth. It is written up front, as
brainstorming would produce, so the reasoning is on the record for review.

## Problem

`ta_human_verify` (12.26) auto-confirms (`Commit`) when an opinion pass and
an independent validator pass agree. The validator only checks whether the
opinion's *reasoning is internally sound* — it never asks whether the pair
is exploitable, biased, or simply wrong in a way that still reads as
well-argued. Three concrete gaps follow, each with a fix:

1. **Correlated blind spots** — opinion and validator are both LLM passes
   sharing the same failure modes. Fix: a red-team pass framed
   adversarially ("assume this is wrong — find the failure"), never as a
   second soundness check.
2. **Silent drift** — nothing today shows whether a workload's auto-confirm
   rate is healthy or the thresholds have quietly gone slack. Fix: per
   `workload_type` metrics over time.
3. **No feedback loop** — a human-discovered wrong `Commit` doesn't change
   anything. Fix: (a) confirmed misses become few-shot context in future
   opinion/validator prompts for that workload; (b) misses clustering above
   a configurable rate auto-*propose* (never auto-apply) a threshold
   tightening for human approval.

"Autoreward" is explicitly *not* model training — there is no fine-tuning
step in TA's headless-CLI-agent architecture. It means closing the loop
procedurally with primitives TA already has: audit logs, threshold config,
prompt context.

## Design decision: CLI surface (deviates from PLAN.md's literal text)

PLAN.md item 1 suggests `ta verify audit [--sample N] [--workload <type>]`.
That collides with the existing hidden `Commands::Verify { goal_id }`
(`apps/ta-cli/src/commands/verify.rs`) — an unrelated pre-draft build/test
gate ("run cargo test/clippy/etc. in a staging dir"), already wired as a
flat positional-arg variant, documented at `docs/USAGE.md:2060` as
`ta verify <goal-id-prefix>`. Restructuring that variant to accept a nested
`audit` subcommand alongside its existing positional `goal_id: Option<String>`
is ambiguous for clap (is `audit` a goal ID prefix or a subcommand?) and
risks breaking the one existing documented invocation for an unrelated
feature.

Decision: add the new surface under the existing `ta audit` subcommand
group instead (`apps/ta-cli/src/commands/audit.rs`), which already
aggregates log-inspection subcommands (`tail`, `show`, `export`, `drift`,
`messaging`, `social`, `telemetry`, `ledger`). This is a same-shape, lower-risk
fit:

```
ta audit human-verify sample [--sample N] [--workload <type>]
ta audit human-verify metrics [--workload <type>]
ta audit human-verify proposals
```

This satisfies PLAN.md item 1's flags verbatim and item 5's "`ta stats` or
equivalent" (explicitly flexible wording) without touching the unrelated
`ta verify` command.

## Data model

New files under `.ta/` (workspace root):

| File | Committed? | Written by | Purpose |
|---|---|---|---|
| `.ta/human-verify-audit.jsonl` | No (gitignored, pre-existing from 12.26) | `ta_human_verify` Commit path | Detailed per-auto-confirm record. **Gains one new field: `id: Uuid`**, so a review pass can reference which entry it reviewed. Existing 12.26 tests only assert substring contains — unaffected. |
| `.ta/human-verify-invocations.jsonl` | No (gitignored, new) | `ta_human_verify`, right after `decide()` | One line per *rendered* gate decision (`{timestamp, workload_type, decision}`), for every branch (Commit/Reject/Rework/Escalate) — this is the metrics denominator 12.26 never recorded. Deliberately scoped to only the branch where `decide()` actually ran (not the security-tier-skip or pipeline-failure escalate paths, which never entered the confidence-gated population at all). |
| `.ta/verify-audit-reviewed.jsonl` | No (gitignored, new) | `ta audit human-verify sample` | `{id, reviewed_at}` per audit entry the red-team pass has looked at — a cursor, not a log to prune, but not durable data either. |
| `.ta/verify-failures.jsonl` | **Yes, committed** (new) | `ta audit human-verify sample`, only on confirmed-miss | The durable calibration dataset per PLAN.md item 3: `{id, audit_entry_id, workload_type, question, context, opinion, validator, red_team_explanation, timestamp}`. |
| `.ta/verify-threshold-proposals.jsonl` | No (gitignored, new) | `ta audit human-verify sample`, when miss rate clusters above threshold | Proposals only — nothing ever reads this to mutate `.ta/workflow.toml` automatically. A human reviews `ta audit human-verify proposals` output and edits `workflow.toml` by hand. |

Config: `.ta/workflow.toml` gains `[verify_redteam.<workload_type>]` /
`[verify_redteam.default]` tables (same override-layering pattern as
`[human_verify.*]` in 12.26):

```toml
[verify_redteam.default]
miss_rate_threshold = 0.25    # propose tightening above this miss rate
min_sample_size = 5           # never propose from a tiny reviewed sample
tighten_min_confidence_step = 0.05
tighten_max_risk_step = 5     # subtracted from max_risk_score
```

## New library module: `crates/ta-mcp-gateway/src/verify_audit.rs`

Not an MCP tool (no `#[tool]` annotation) — a plain library module, since
`ta audit human-verify` is invoked from the CLI process, not through the
gateway. `apps/ta-cli` already depends on `ta-mcp-gateway` (used today for
`GatewayConfig` in `audit.rs`/`verify.rs`), so no new crate dependency.

Contents:
- `HumanVerifyAuditRecord` — owned struct matching the audit JSONL shape
  (including new `id`), for reading.
- `RedTeamVerdict{ConfirmedCorrect, ConfirmedMiss}`, `RedTeamResult{verdict, explanation}`.
- `RedTeamPipeline` trait (`fn review(&self, entry: &HumanVerifyAuditRecord) -> Result<RedTeamResult, String>`)
  mirroring `human_verify.rs`'s `SyntheticPipeline` abstraction — same reason:
  gating/sampling logic must be unit-testable without spawning subprocesses.
- `HeadlessRedTeamPipeline` — production impl, spawns `ta run --headless`
  with an explicitly adversarial prompt ("assume this is wrong — find the
  failure the opinion+validator pair missed," never phrased as a
  soundness re-check), reusing `human_verify.rs`'s `spawn_headless_and_capture`/
  `extract_marker_json`/`write_context_file` helpers (promoted to
  `pub(crate)`, not duplicated).
- `VerifyFailureRecord`, `append_verify_failure`, `load_recent_misses`.
- `read_audit_entries`, `load_reviewed_ids`, `mark_reviewed`, `select_unreviewed`
  (pure sampling: filter by workload + already-reviewed, cap at `--sample N`).
- `record_invocation` (writes to the new invocations log).
- `WorkloadMetrics`, `compute_metrics` — auto-confirm rate (from invocations
  log), red-team-catch rate and false-auto-confirm rate (from
  verify-failures vs. reviewed/Commit counts, both explicitly documented as
  *sampled* estimates bounded by how much has been reviewed so far, not a
  full-population guarantee).
- `ThresholdProposal`, `maybe_propose_threshold_tightening`,
  `append_threshold_proposal` — pure decision function taking a miss rate +
  sample size + config, returning `Option<ThresholdProposal>`; never writes
  `.ta/workflow.toml`.
- `run_redteam_review(...)` — the orchestration entry point the CLI calls:
  select unreviewed → pipeline.review() each → append misses → mark
  reviewed → compute miss rate for touched workloads → maybe propose
  tightening → return a summary for CLI output.

## Changes to `human_verify.rs` (additive, 12.26's existing tests untouched)

1. Add `id: Uuid::new_v4()` to each written `HumanVerifyAuditEntry`.
2. After `decide()` runs (both Commit and non-Commit branches, before the
   `is_auto_approvable()` check), call `record_invocation(workspace_root,
   &workload_type, decision)`.
3. Few-shot injection (item 4a): `handle_human_verify` resolves
   `workload_type` once (cheap, idempotent — `handle_human_verify_with_pipeline`
   already re-resolves it independently for gating, exactly as it does
   today), loads `load_recent_misses(..., workload_type, N)`, and passes it
   into `HeadlessSyntheticPipeline::new(workspace_root, ta_bin, recent_misses)`
   (new third constructor argument). `build_opinion_context`/
   `build_validator_context` gain a `recent_misses: &[VerifyFailureRecord]`
   parameter and append a "Known past mistakes for this workload" section
   when non-empty. The `SyntheticPipeline` **trait signature is unchanged**
   — only the production struct's construction changes — so existing test
   doubles (`FakePipeline`, `MustNotBeCalledPipeline`, `FailingOpinionPipeline`)
   are unaffected.

## CLI: `apps/ta-cli/src/commands/audit.rs`

New `AuditCommands::HumanVerify { command: HumanVerifyAuditCommands }`
variant (nested subcommand, same shape as the existing `Ledger { command }`
variant) with `Sample`, `Metrics`, `Proposals` subcommands, thin handlers
delegating to `ta_mcp_gateway::verify_audit`.

## Testing plan (covers PLAN.md item 6 explicitly)

- `select_unreviewed`: filters already-reviewed + workload, respects sample cap.
- `run_redteam_review` with a fake `RedTeamPipeline`: confirmed-miss appends
  to verify-failures.jsonl and marks reviewed; confirmed-correct only marks
  reviewed.
- Seeded-miss few-shot test: seed `.ta/verify-failures.jsonl`, call
  `load_recent_misses` + `build_opinion_context`, assert the built prompt
  text contains the seeded explanation.
- `maybe_propose_threshold_tightening`: fires only when miss_rate exceeds
  configured threshold *and* sample size meets the minimum; never touches
  `.ta/workflow.toml` (no function in the module ever opens that path for
  writing).
- `compute_metrics`: mixed sample of hits/misses across two workload types
  aggregates rates correctly.
- CLI smoke test for `ta audit human-verify metrics` on an empty project
  (no crash, clear "nothing recorded yet" message — Observability Mandate).

## Docs

`docs/USAGE.md` gains a section documenting: the red-team loop end-to-end,
`verify-failures.jsonl`'s role as a durable, committed calibration dataset
(not a log to prune), and how to review/approve a threshold-tightening
proposal (human edits `.ta/workflow.toml` after reading `ta audit
human-verify proposals`). Since the Explore step of this session found that
12.26's own USAGE.md section (item 7, marked `[x]`) was never actually
written, this session's docs addition covers both the 12.26 pipeline and
the 12.27 red-team loop together in one coherent section, rather than
leaving 12.26 permanently undocumented.
