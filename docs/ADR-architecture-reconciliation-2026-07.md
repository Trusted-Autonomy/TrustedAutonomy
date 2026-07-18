# Architecture Reconciliation: One Crate Map, Three Axes — Plus Meridian's Real Integration Point

**Status**: Proposed reconciliation, supersedes the "pick one model" ambiguity flagged in `ADR-modularity-revisit-2026-07.md` §1.
**Reads together with**: `ADR-modular-decomposition.md` (axis A), `ADR-product-concept-model.md` (axis B), this session's `ta-intake`/`ta-brain` work (axis C), `USE-CASE-product-team.md` (the Planning-layer gap, found independently), `ADR-modularity-revisit-2026-07.md` (where this gap was first named).

---

## 1. The insight: these are three orthogonal axes, not three competing hierarchies

The prior finding was that TA has three layering models that were never reconciled. The mistake in trying to reconcile them by picking a winner is that **they answer three different questions about the same crate, not the same question three different ways**:

| Axis | Question it answers | Values |
|---|---|---|
| **A — Ownership** (`ADR-modular-decomposition.md`) | Where does this code live, who releases it, what's the coordination boundary? | Core (this repo) / Infra (standalone-publishable) / Application (separate product) |
| **B — Concern** (`ADR-product-concept-model.md`) | What class of architectural problem does this solve? | L0 Planning *(new)* / L1 Mediation / L2 Policy / L3 Session-Review / L4 Agent-Integration / L5 IO-Delivery |
| **C — Lifecycle stage** (this session's 3-tier work) | At what point does this code run as a single request flows through the system? | Tier 0 Planning *(new)* / Tier 1 Trigger / Tier 2 Brain / Tier 3 Back-office |

A single crate has a coordinate on **all three axes simultaneously**, and there's no contradiction in that — `ta-brain` is Core (A) + L2-adjacent routing logic (B) + Tier 2 (C), all correctly true at once. The reconciliation isn't "which model wins" — it's publishing one table with all three coordinates so nobody has to guess which lens a given doc was using.

---

## 2. The reconciled crate map

Extends `ADR-product-concept-model.md`'s existing L1-L5 crate table with axis A, axis C, and the 0.17.x crates it predates.

| Crate | A — Ownership | B — Concern | C — Stage | Notes |
|---|---|---|---|---|
| `ta-policy` | Core | L2 Policy | Tier 3 | |
| `ta-audit` | Core | L2 Policy | Tier 3 | |
| `ta-workspace` | Core | L1 Mediation | Tier 3 | |
| `ta-changeset` | Core | L3 Session-Review | Tier 3 | |
| `ta-goal` | Core | L3 Session-Review | Tier 3 | |
| `ta-submit` | Core | L1 Mediation | Tier 3 | |
| `ta-memory` | Infra *(extraction deferred)* | L4 Agent-Integration | Tier 3 | |
| `ta-credentials` | Infra *(extraction deferred)* | L4 Agent-Integration | Tier 3 | |
| `ta-mcp-gateway` | Core | L4 Agent-Integration | Tier 3 | |
| `ta-connectors/*` | Core | L1 Mediation | Tier 3 | |
| `ta-daemon` | Core | L5 IO-Delivery | Tier 3 | |
| `ta-sandbox` | Core | L2 Policy | Tier 3 | |
| `ta-cli` | Core | L5 IO-Delivery | Tier 3 | |
| `ta-mediation` | Core | L1 Mediation | Tier 3 | |
| `ta-session` | Core | L3 Session-Review | Tier 3 | |
| `ta-events` | Core | L3 Session-Review | Tier 3 | |
| `ta-workflow` | Core | L3 Session-Review | Tier 3 | |
| Channel plugins (Discord/Slack/Email) | **Application** (out-of-process) | L5 IO-Delivery | Tier 3 | proves the A=Application boundary already works |
| `ta-decision` | Core | L2 Policy | Tier 3 | new, 0.17.x — generic Commit/Reject/Escalate gate |
| `ta-plugin` | Core | L1 Mediation | Tier 3 | new, 0.17.x — shared external-subprocess transport |
| `ta-data-spec` | Core | L4 Agent-Integration | Tier 3 (build-time) | new, 0.17.x |
| `ta-advisor` | Core | L3 Session-Review | Tier 2/3 boundary | new, 0.17.x — conversational entry point |
| `ta-brain` | Core | **L2 Policy (routing sub-concern)** | **Tier 2** | new, 0.17.x — this is the crate that made axis-C legible; it didn't fit cleanly on axis B alone, which is exactly the symptom that motivated this reconciliation |
| `ta-intake` | Core | **L1 Mediation (of *requests*, not resources)** | **Tier 1** | new, 0.17.x |
| Meridian | **Application** (separate repo/binary/MCP server, proven pattern) | **L0 Planning *(new slot)*** | **Tier 0 *(new slot)*** | see §3 — this is where it actually belongs, not bolted onto `ta plan status` |
| *(unbuilt)* Planning service | Core or Application — **undecided, see §4** | **L0 Planning *(new)*** | **Tier 0 *(new)*** | the gap named independently in `ADR-modularity-revisit-2026-07.md` §3 and `USE-CASE-product-team.md` §6 |

**Two new slots this reconciliation adds, not invents from nothing**: L0/Tier-0 "Planning" was already identified twice independently this session (once reasoning from the user's execution/orchestration/planning/validation framing, once reasoning from the product-team PM's backlog loop) — this table is the first place it gets an actual coordinate rather than being a footnote.

---

## 3. Where Meridian actually belongs, and how to integrate it for real

**The problem with what 12.13 shipped**: it bolted a fake local KPI scorer onto `ta plan status`/`ta run`, because the plan text asked for "in-process, no subprocess" — a constraint that's incompatible with actually calling Meridian's real engine (which only exists as an external binary/MCP server, never vendored as a Rust crate). That was the right call *given that specific, narrow requirement*, but it isn't a real Meridian integration — it's a placeholder that happens to share the word "KPI."

**The right integration, per your framing**: Meridian doesn't need to be inside TA at all. It needs to be the concrete adapter the *new Tier-0 Planning stage* calls — the same relationship the trigger layer already has with its own sources (email, schedule, webhook), just one stage earlier:

```
Tier 0 — Planning (new)              Tier 1 — Trigger (exists)         Tier 2 — Brain (exists)      Tier 3 — Back-office (exists)
┌─────────────────────────┐         ┌──────────────────┐             ┌─────────────────┐          ┌──────────────────────┐
│ Planning service reads:  │         │ ta-intake         │             │ ta-brain          │          │ staged goal execution │
│ - velocity-history.jsonl │  emits  │ (existing trigger │   routes    │ (existing routing │  routes  │ (unchanged, exists)   │
│ - goal-audit.jsonl       │ ──────► │ manifest system,  │ ──────────► │ + priority logic) │ ───────► │                        │
│ - plan_history.jsonl     │ synthetic│ NOT changed)      │             │                   │          │                        │
│ - PLAN.md phase status   │  trigger └──────────────────┘             └─────────────────┘          └──────────────────────┘
│         │                │  events
│         ▼                │
│   calls Meridian's real  │
│   MCP tools:              │
│   meridian_report         │  ◄── real KPI-alignment scoring, actually Meridian's engine, not TA's approximation
│   meridian_suggest        │  ◄── real low-alignment category×KPI pairs
│         │                │
│         ▼                │
│  emits a synthetic        │
│  TriggerEvent ("KPI       │
│  analysis suggests phase  │
│  X next") into the SAME   │
│  event store ta-intake    │
│  already polls            │
└─────────────────────────┘
```

**Why this design and not another**: it makes Meridian's integration point identical in shape to `v0.17.0.12.31`'s new PR-merged webhook trigger, which was just built and merged this session — both are "an external signal becomes a `TriggerEvent` in the same store `ta-intake` already polls." No new execution machinery, no new event bus, no second parallel trigger system. The Planning stage's only job is producing the *input* to Tier 1, not replacing anything downstream of it.

**Concrete shape**:
1. A new lightweight service/crate (working name `ta-plan-service`, but see §4 on where it lives) periodically (or on `on_pr_applied`/`on_phase_completed` events, matching the old `VISION-virtual-office.md`'s `on_pr_applied` trigger pattern) gathers TA's real execution metrics.
2. It calls Meridian's `meridian_report` (expert-panel KPI alignment for the session/project) and `meridian_suggest` (low-alignment category×KPI pairs) — Meridian's *actual* engine, not a reimplementation.
3. It translates Meridian's suggestions into candidate next-goal descriptions and writes them as `TriggerEvent`s ta-intake already knows how to consume — no new consumer needed.
4. `12.13`'s local scorer stays exactly as-is, as the honest degraded-mode fallback for `ta plan status`/`ta run`'s inline hint when Meridian isn't installed/reachable — it should be relabeled in its own doc as "offline approximation," not left implying it's a real Meridian call.
5. For the product-team use case (`USE-CASE-product-team.md` §6), the PM agent becomes the consumer of these Meridian-sourced trigger events instead of (or in addition to) its own knowledge-graph-driven backlog scoring — the same Planning-stage output feeds both TA's own plan-phase sequencing *and* Amplified Office's product backlog, which is exactly the "one Planning layer, two consumers" outcome the reconciliation in §2 was aiming for.

---

## 4. Open question this reconciliation surfaces but doesn't resolve

Axis A (ownership) for the new Planning stage is genuinely undecided, and shouldn't be resolved by default:
- **Core** (lives in this repo): argument — Tier 0/1/2/3 becomes one coherent pipeline maintained together, matches how `ta-brain`/`ta-intake` were just built as in-repo crates rather than extracted.
- **Application** (separate repo, like Meridian itself): argument — Planning is inherently product-specific (TA's own phase-sequencing needs differ from Amplified Office's investment/product backlog needs), and `ADR-modularity-revisit-2026-07.md` §5's recommendation was explicitly "don't extract before the 3-tier model stabilizes" — Planning is the newest, least-proven tier, so premature extraction risk is highest here of anywhere in the map.

Recommendation: build the first version **in TA core**, thin, generic (a "candidate next-goal source" trait, with Meridian as the first real implementation and the local scorer as the fallback), and revisit extraction only once Amplified Office's own product-team use case (`USE-CASE-product-team.md`) is far enough along to prove whether TA's phase-sequencing needs and a product backlog's needs actually want the same engine or diverge. Don't decide the ownership question before there's a second real consumer to design against.
