# TA Architecture Reference (Current State)

**Status**: stable maintainer reference, first published 2026-07-16 as the v0.17.0.12.11–12.27 overhaul lands.
**Audience**: maintainers and contributors who need "how is this actually set up," not the plain-language product description.
**Purpose**: the current-state architecture — what's built, where the boundaries are, why the repo is organized the way it is. This is a snapshot of *what exists now*, kept accurate going forward; it formalizes the working notes in [`docs/design/ta-concepts-and-architecture.md`](../design/ta-concepts-and-architecture.md) into a stable reference. That design doc remains valuable as the historical record of *how* these decisions were reached (the gap analysis, the alternatives considered, the sequencing rationale) — read it if you want the "why," read this doc if you want the "what, today."
**Plain-language companion**: [`docs/guides/what-is-ta.md`](../guides/what-is-ta.md) — read that first if you want the no-jargon version this document assumes.
**Companion references**: [`ta-action-reference.md`](../design/ta-action-reference.md) (the Write/Review/Decision/Commit/Reject action vocabulary this doc's Tier 3 implements), [`ta-data-format-spec.md`](../design/ta-data-format-spec.md) (the schemas backing §3 below), [`ta-cli-verb-reference.md`](../design/ta-cli-verb-reference.md) and [`ta-user-personas.md`](../design/ta-user-personas.md) (the CLI surface built on top of everything here).

---

## 1. The Three-Tier Request Model, As Built

Every unit of work TA does — however it originates — flows through the same three tiers:

```
Tier 1: Triggers (ta-intake)   — how work gets fed in
        │
        ▼
Tier 2: Routing Brain (ta-brain) — who does it, how autonomously, how urgently
        │
        ▼
Tier 3: Back Office (staged review) — Write → Review → Decision → Commit/Reject/Escalate
```

A fourth, orthogonal concern — **Tier 0, substrate maintenance** (`ta doctor`, `ta gc`) — keeps the tiers above healthy but isn't part of any one goal's flow. See §1.4.

### 1.1 Tier 1 — Triggers (`ta-intake`)

`ta-intake` is a **library crate with no CLI or daemon glue** — its only job is "normalize an external event into one `TriggerEvent` shape." Everything downstream (dispatch to a goal, append to a queue, route through `ta-brain`) is a thin, swappable consumer.

Trigger types are **data, not code** — the same pattern used for personas and plugins. Each type is one TOML file at `.ta/triggers/<type>.toml`:

```toml
type = "schedule"
enabled = true
dispatch = "direct"       # or "queue"
[settings]
interval_secs = 3600
goal_title = "Nightly health check"
```

Two shipped, real (not stub) `TriggerSource` implementations: `schedule` (fires on an elapsed interval) and `inbound-email` (polls a messaging plugin for new messages since a watermark). A community-authored trigger type needs only a config file plus, for a genuinely new kind of source, a small `TriggerSource` implementation — no `ta-intake` code change.

`dispatch` is per-type, not hardcoded: `"direct"` creates a goal immediately (`ta run --headless`); `"queue"` appends to `.ta/intake-queue.jsonl` for batch/coordinator processing. Each trigger type tracks its own watermark independently, so repeated fires only act on genuinely new events.

### 1.2 Tier 2 — Routing Brain (`ta-brain`)

Every goal request — an explicit `ta run` invocation, a fired trigger event, or a free-text `ta advisor create` prompt — resolves through **one shared pure function**, `ta_brain::route()`. This is the load-bearing design discipline of this tier: one decision function, called identically regardless of how the request arrived, so an explicit CLI call and an automated trigger can never be resolved by two different systems that quietly disagree.

`route()` answers five questions:

| Question | Values |
|---|---|
| **team** | which team role (`.ta/team.toml`) owns the work |
| **persona** | which persona (`.ta/personas/<name>.toml`), if any |
| **agent** | which agent/model/framework runs it |
| **security_tier** | `read_only` / `suggest` / `auto` — how autonomously it may proceed |
| **priority** | `low` / `normal` / `high` / `urgent` |

Each is resolved through the same tiered lookup (most-specific wins):
1. Explicit flag (`--team`/`--persona`/`--agent`/`--security`/`--priority` on `ta run`)
2. Per-workload-type binding (`.ta/workflow.toml`'s `[workload_types.<type>]`)
3. Workflow-level default (`.ta/team.toml` per-role binding, or `.ta/workflow.toml`'s top-level `[team]`/`[security]`/`[priority]`)
4. Built-in heuristic fallback

Before resolving those tiers, `route()` classifies the request's **workload type** (`bugfix`, `docs`, `feature`, `refactor`, `test`, `release`, `security`, `chore`, `other`) from the title/payload — a simple, auditable keyword heuristic, not a model, always carrying a confidence score. Low-confidence classification is handled conservatively: `security_tier = "auto"` automatically downgrades to `"suggest"` below 65% confidence, so an uncertain guess never grants full autonomy on its own. Every routing decision (including the downgrade and its reason) is logged to `.ta/routing-decisions.jsonl`.

**Two entry points sit in front of `route()`, not beside it:**
- **`ta intake coordinate [--dispatch]`** — the "team coordinator" is a capability of the existing Advisor (its `AdvisorSecurity` trust tri-state extended, not a new persistent role), triaging `.ta/intake-queue.jsonl` into `auto-eligible` / `needs review` / `needs clarification`.
- **`ta advisor create "<free text>"`** — parses a raw sentence into title/objective/hints via the same advisor-agent headless-conversation mechanism used for draft-review dialogue, then feeds the result into `route()` exactly as a structured request. High confidence routes immediately; low confidence asks exactly one clarifying question (via the same `ta_ask_human`-backed mechanism, see §1.3) and re-routes once. This folds `ta-workflow::intent::resolve_intent`'s workflow-template matching in as one signal `route()` consults, not a second parallel intent system.

### 1.3 Tier 3 — Back Office (Staged Review)

The most mature tier, and the one that doesn't change shape as part of this overhaul: an agent works inside a staged overlay (`.ta/staging/<goal-id>/`); the result becomes a reviewable `DraftPackage` (diff + AI summary + supervisor verdict); a human or a trusted policy approves; `apply` materializes it. See [`ta-action-reference.md`](../design/ta-action-reference.md) for the full Write/Review/Decision/Commit/Reject/Escalate vocabulary this tier implements, and [`ta-data-format-spec.md`](../design/ta-data-format-spec.md) for the wire shapes (`Goal`, `Draft`/`Artifact`).

**What's new in this tier as of 12.26/12.27 — confidence-gated verification, closing the loop it opens:**

`ta_human_verify` replaces `ta_ask_human`'s unconditional block-and-wait (kept registered as a deprecated alias) with a two-stage synthetic pre-check before ever escalating to a real human:
1. **Opinion pass** — a headless-agent call answers the question the way a careful human reviewer would, with explicit reasoning and self-reported confidence.
2. **Validator pass** — an independent second headless-agent call, sharing no prompt/context with the opinion pass, critiquing the opinion's reasoning rather than trusting its confidence, producing a `DecisionInput` (verdict/risk/confidence).

The pair is scored through the same generic `ta_decision::gate::decide()` used elsewhere in the graph. `Commit` auto-confirms (writing the full opinion + validator reasoning to `.ta/human-verify-audit.jsonl`, gitignored); `Reject`/`Rework`/`Escalate` fall through to a real blocking human question, with the synthetic reasoning attached as context. A `security_tier != "auto"` workload always escalates straight through, skipping the synthetic stage entirely — per-`workload_type` thresholds live in `.ta/workflow.toml`'s `[human_verify.<type>]`.

**Red-team autoreward (12.27) closes the remaining gap**: the validator only checks whether the opinion's reasoning is internally *sound* — it can't catch a mistake both LLM passes are blind to in the same way. `ta audit human-verify sample` runs a distinctly-framed adversarial pass ("assume this is wrong; find the failure the opinion+validator pair missed," never a second soundness check) over a sample of already-auto-confirmed entries. Confirmed misses are appended to `.ta/verify-failures.jsonl` — **committed, not gitignored**, a durable calibration dataset — and feed back into the system two ways: (a) folded into future opinion/validator prompts for that `workload_type` as few-shot context, and (b) if misses cluster above a configurable rate, a threshold-tightening *proposal* is appended to `.ta/verify-threshold-proposals.jsonl` for a human to approve — never applied automatically, since thresholds are a trust boundary. `ta audit human-verify metrics` surfaces auto-confirm rate, catch rate, and false-confirm rate per `workload_type` over time, so drift is visible instead of discovered after an incident.

### 1.4 Tier 0 — Substrate Maintenance (`ta doctor`, `ta gc`)

Not a Trigger, not the Brain, not part of the Write/Review/Decision/Commit/Reject graph. `ta doctor`'s checks (daemon health, disk pressure, stale goals/staging dirs/drafts, version/plan drift, log size) are the health of the substrate the three tiers above run on — orthogonal to any specific goal or routing decision, the same way office facility maintenance has nothing to do with any one contractor's job. `ta gc` is the same Tier-0 backend, exposed as the non-interactive alias (`doctor --fix --yes`) for cron/unattended use.

---

## 2. Library-Crate Boundaries

The three tiers are organized as **library crates, decoupled from any one binary**, so the Brain is genuinely reusable rather than reimplemented per entry point:

| Crate | Owns | Consumed by |
|---|---|---|
| `ta-intake` | `TriggerEvent` normalization, per-type `TriggerSource` trait, watermarking | `ta intake fire`/`list`/`queue`/`coordinate` (thin CLI glue), `ta-brain` |
| `ta-brain` | `route()` — the pure decision function; workload classification | `ta run`, `ta advisor create`, `ta intake coordinate --dispatch`, any future trigger-fired entry point |
| Back office (`ta-changeset`, `ta-policy`, `ta-goal`, `ta-submit`) | Staging, `DraftPackage`, `ApprovalRule`/`AccessConstitution`, per-application `commit()`, supervisor review | `ta draft`/`ta run`/`ta apply` command paths |
| `ta-mcp-gateway` | `ta_human_verify` (+ deprecated `ta_ask_human` alias), other MCP tool surfaces | Agents calling back into TA mid-goal; the human-verify audit/red-team loop (§1.3) |
| `ta-data-spec` | The five versioned JSON Schema wire types (§3) | `ta-daemon`'s API layer, Studio, community trigger-configs/plugins |

**The discipline that makes this work**: `ta-brain::route()` is a single function called identically whether the request is an explicit `ta run` or a normalized `TriggerEvent` — there is no second, parallel routing path for automated work. The same applies to the Advisor's clarifying-question mechanism (§1.2, §1.3): `ta advisor create`'s low-confidence path and `ta intake coordinate --dispatch`'s `needs_clarification` outcome both reuse the identical `ta_ask_human`-backed headless-agent mechanism, not two separate conversational loops.

---

## 3. Data-Format Specs — The Real Interface Boundary

TA stays a **single Cargo workspace**, not a split of per-tier repos — a split would add cross-repo schema drift and version-pinning friction without a real payoff for a project with a single release train. Instead, the boundary between TA's core and everything that needs to interoperate with it (Studio, community trigger-configs, community plugins) is enforced at the **data** level.

`ta-data-spec` (published v0.17.0.12.21) generates versioned JSON Schema directly from the real, already-`serde`-annotated Rust types via [`schemars`](https://docs.rs/schemars) — not a hand-maintained mirror that can drift from what's actually serialized on the wire:

| Spec | Rust type | Crate |
|---|---|---|
| `Goal` | `GoalRun` | `ta-goal` |
| `Draft` / `Artifact` | `DraftPackage` / `Artifact` | `ta-changeset` |
| `TriggerEvent` | `TriggerEvent` | `ta-intake` |
| `RoutingDecision` | `RoutingDecision` | `ta-brain` |
| `Persona` | `PersonaConfig` | `ta-goal` |

Each schema carries a stable `$id` and an explicit `x-ta-schema-version`, independent of the workspace semver. A schema-sync test fails CI if a checked-in schema drifts from what the current Rust types would generate; a round-trip test fails CI if a type change breaks deserialization of a frozen example — the concrete guarantee behind "a schema change that breaks an existing serialized example fails CI."

**The Studio boundary rule, and how it's enforced**: Studio is a separately-deployable add-on against the daemon's HTTP/SSE API — it may never special-case internal Rust types, only the versioned spec above. Since Studio is JS, the rule is enforced one layer down, at `ta-daemon`'s own API response types: prefer a purpose-built response DTO over serializing an internal type directly; a response may embed a spec type directly only alongside an explicit `schema_version` field. `ta-data-spec`'s `studio_boundary.rs` test statically scans `ta-daemon`'s API response definitions for a spec type embedded without that sibling field and fails CI if it finds one.

Full detail: [`ta-data-format-spec.md`](../design/ta-data-format-spec.md).

---

## 4. Why This Stays One Monorepo

Multi-repo only pays off when pieces need independent release cadence or separate team ownership — neither applies here (single release train). A split would *add* the exact friction it would be trying to solve: cross-repo schema drift, version-pinning overhead, more install/setup steps. TA is already workspace-organized (~30 crates); the fix for coupling concerns is tighter internal boundaries — the library-crate split in §2 and the data-format contract in §3 — not repo boundaries. Studio remains what it already is: a separately-deployable add-on against the daemon's API, governed by the boundary rule in §3, living in the same workspace for now because nothing about it requires an independent release cycle.

---

## 5. Where to Go Next

- **The action/graph vocabulary** (Write/Review/Decision/Commit/Reject, Consensus, HumanGate, Invoke/Switch/Parallel, Audit/Meter): [`ta-action-reference.md`](../design/ta-action-reference.md).
- **The wire-format schemas**: [`ta-data-format-spec.md`](../design/ta-data-format-spec.md).
- **The CLI surface built on top of all of this** (10-verb human-facing layer vs. full automation-facing surface): [`ta-cli-verb-reference.md`](../design/ta-cli-verb-reference.md) and, for how each persona actually uses it, [`ta-user-personas.md`](../design/ta-user-personas.md).
- **The design history** — gap analysis, alternatives considered, and the sequencing rationale behind everything in this doc: [`ta-concepts-and-architecture.md`](../design/ta-concepts-and-architecture.md). Its §4 (knowledge hierarchy), §8 (community contribution security review), and §13 (this three-tier model's original proposal) sections cover work still ahead, not yet reflected here because it isn't built.
- **User-facing behavior docs**: [`docs/USAGE.md`](../USAGE.md)'s "Trigger Layer", "Routing Brain", and "Confidence-Gated Verification" sections document the same systems from an operator's how-do-I-configure-this angle.
