# Red-Team Review: Is TA Too Monolithic? (2026-07 revisit)

**Status**: Red-team analysis, not a decision — for the user's evaluation.
**Prior art this builds on** (read in full before writing this): `docs/ADR-modular-decomposition.md` (the actual prior decision: extraction *considered and explicitly deferred* at v0.5.6/v0.5.7, "monorepo is manageable at current scale"), `docs/ADR-product-concept-model.md` (the Five-Layer concern-based model: L1 Resource Mediation → L2 Supervision/Policy → L3 Session/Review → L4 Agent Integration → L5 IO/Delivery, with a Crate Map already assigning every crate to a layer).
**Trigger**: the user's own framing — "a layer for abstracted execution to do work in a virtual space, another layer for workflow orchestration, another for planning, another for automated validation... We already have Autoreward and Meridian split out."

This is not a fresh question. It's the third framing of the same underlying tension, and the first thing worth establishing is that **three different layering models now exist in this project's history, and none of them have been reconciled with each other.**

---

## 1. The three models, side by side

| # | Model | Axis | Status | Source |
|---|---|---|---|---|
| A | Core / Agent-Infra / Applications | *Who owns it / where does it ship* (this repo vs. a standalone crate vs. a separate product) | Considered, explicitly deferred ~v0.5.6 | `ADR-modular-decomposition.md` |
| B | L1 Mediation → L2 Policy → L3 Session → L4 Agent-Integration → L5 IO/Delivery | *Concern* (what problem does this code solve) | Accepted (per its own header), but written pre-0.17.x and never revisited against the current crate map | `ADR-product-concept-model.md` |
| C | Trigger → Brain → Back-office (3-tier) | *Lifecycle stage of a request* (intake → routing decision → execution) | Actively being built this session (`ta-intake`/`ta-brain`/12.x work) | This session's PLAN.md phases |
| D (new, user's framing) | Execution / Orchestration / Planning / Validation | *Functional capability* (what kind of work happens here) | Just proposed | This document |

**First finding, before evaluating D on its own merits: nobody has ever drawn the mapping between A, B, and C.** They coexist as separate mental models different sessions reached for, each locally coherent, never cross-checked. Adding D without first reconciling A/B/C risks a fourth incompatible taxonomy rather than a clarification. Section 3 does the mapping exercise the project has been skipping.

---

## 2. What actually happened since the 2026-05-era deferral — modularity by accretion, not decision

`ADR-modular-decomposition.md`'s crate list (v0.10.7) had ~12 crates. The current workspace (post-0.17.x) has grown to roughly 30+, and critically: **the 0.17.x overhaul already extracted several genuinely separate-concern crates without ever revisiting the ADR that deferred exactly this** — `ta-brain` (routing/classification), `ta-intake` (trigger layer), `ta-decision` (the generic Commit/Reject/Escalate gate), `ta-plugin` (the shared external-subprocess transport), `ta-data-spec` (schema generation) all shipped as separate crates this year, each with a single clear responsibility and clean dependency edges — exactly the shape the deferred ADR argued for, arrived at organically through feature work rather than a deliberate extraction decision.

Separately, **Meridian is a genuinely separate product** (its own MCP server, own binary, own release cycle — connected to TA only via MCP tool calls) — real evidence that the "Applications" layer of Model A works when actually executed, not just proposed. **"Autoreward" is not yet separate** — it's `v0.17.0.12.27`, still pending, still scoped to land inside `ta-mcp-gateway`/`ta-decision` in this repo. The user's framing ("we already have Autoreward... split out") is aspirational for Autoreward specifically — worth flagging directly, since a design built on "this is already proven" needs that premise checked.

**So the honest current state is: TA is meaningfully less monolithic than either ADR assumed, via crate-level extraction within one workspace/repo — but zero applications have been extracted to genuinely separate repos except Meridian**, and nobody updated `ADR-modular-decomposition.md`'s "Decision Needed" checklist to reflect any of this.

---

## 3. Mapping the user's proposed model (D) onto what exists

| Proposed layer | Maps to (Model B / L-number) | Maps to (Model C / tier) | Actual crates today |
|---|---|---|---|
| **Abstracted execution** (work in a virtual space) | L1 Resource Mediation, L3 Session/Review | Back-office (tier 3) | `ta-workspace`, `ta-mediation`, `ta-sandbox`, `ta-session` |
| **Workflow orchestration** | L3/L4 boundary | Back-office, partially Brain | `ta-workflow` (serial-phases, YAML definitions), `ta-goal` |
| **Planning** | Not modeled at all in B | Brain (tier 2) resolves *routing*, but "planning" here means backlog/goal-decomposition, which is genuinely absent | `ta-brain` (routing, not planning), PLAN.md itself (a document, not a service) |
| **Automated validation** | L2 Supervision/Policy | Back-office's Decision/Commit/Reject graph | `ta-decision`, `ta-policy`, the `ta_human_verify`/Autoreward pipeline |

**Finding**: three of the user's four proposed layers already map cleanly onto existing crates and concerns — this isn't a new architecture, it's a **correct, sharper restatement of boundaries that already exist**, just never named this way. The fourth — **Planning** — is the one genuinely missing concern: there is no service, crate, or even a clearly-named responsibility for "given goals/KPIs/backlog, decide what to build next," as distinct from `ta-brain`'s job (given a request, decide *how* to route it). This is the same gap identified independently in `USE-CASE-product-team.md` §6 (the PM's backlog-management loop) — the same missing piece surfacing from two different directions in the same review session is a stronger signal than either alone.

---

## 4. Pros and cons of pushing further toward explicit layering

**Pros (matching `ADR-modular-decomposition.md`'s original reasoning, re-verified against current scale):**
1. **Thesis clarity scales worse now, not better** — at ~30+ crates, "what is TA" is a harder question to answer from the crate list alone than it was at 12. The original ADR's "onboarding is simpler" argument is *more* true today than when it was written and deferred.
2. **A named Planning layer would surface a real gap** rather than leaving it implicitly absent — see §3. Naming a layer that has zero crates in it is itself useful; it turns "we forgot this" into "this layer is empty, on purpose or not."
3. **The Validation layer is about to get real content** (12.27's Autoreward) and would benefit from a stable name/boundary before that lands, rather than retrofitting one after.

**Cons (the original ADR's risks section, re-verified, plus new ones specific to now):**
1. **The original "premature extraction" risk is more true today, not less** — TA's actual interfaces (routing decisions, decision-gate contracts, plugin transport) are *still actively changing* this session (the whole 12.x wave). Locking a layer boundary into separate repos/crates now would freeze interfaces that are demonstrably still moving.
2. **The 3-tier model (C) is mid-construction** — `ta-intake`/`ta-brain` themselves are only partially built (per this session's own findings: "tier-1 triggers essentially absent, tier-2 brain confirmed missing" as of the design-review a few days before this session, now partially filled in). Introducing a fourth taxonomy (D) while a third (C) is still being built risks the team optimizing the *map* instead of finishing the *territory*.
3. **Coordination overhead was already the stated risk in 2026-05 and nothing has reduced it** — if anything, the amplified-office project (a real, separate repo already depending on "TA Core v0.17.x+" primitives that don't exist yet, per `USE-CASE-product-team.md` §9) is a live demonstration of exactly this risk: a downstream consumer now blocked on TA shipping primitives it hasn't built, with version-pinning coordination already a live, unsolved problem between the two repos.
4. **No new evidence has appeared that in-workspace crate separation (what actually happened via 0.17.x) is insufficient.** The wins attributed to "extraction" in the original ADR (thesis clarity, independent evolution, CI scaling) were achieved by `ta-brain`/`ta-intake`/`ta-decision` *without* leaving the workspace. This is the strongest single argument against forcing the next step (separate repos) rather than continuing the pattern that's already working.

---

## 5. Recommendation

**Don't re-litigate the deferral; extend it with better bookkeeping.** The original 2026-05 decision to defer full extraction was right then and the reasoning holds now — the strongest evidence is that the *benefits* extraction was supposed to deliver already arrived via ordinary in-workspace crate boundaries (`ta-brain`, `ta-intake`, `ta-decision`, `ta-plugin`, `ta-data-spec`), without paying separate-repo coordination costs. Model A's "Applications" tier is validated (Meridian proves it works when the boundary is real); Model A's "Agent Infrastructure" tier (`agent-memory`, `agent-credentials`) remains theoretical and un-urgent for the same reason it was deferred before — nothing outside TA needs `ta-memory` or `ta-credentials` yet.

Three concrete, low-risk actions instead of a re-architecture:

1. **Update `ADR-modular-decomposition.md`'s "Decision Needed" checklist** to record that `ta-brain`/`ta-intake`/`ta-decision`/`ta-plugin`/`ta-data-spec` extraction happened, unprompted by the ADR, validating its thesis without its mechanism — this is a five-minute doc fix that closes a real gap (a "deferred" decision with no record of what happened since).
2. **Name the Planning layer explicitly**, even with zero crates in it today, in whichever doc becomes the living architecture reference (`docs/architecture/ta-architecture-reference.md`, added this session in `v0.17.0.12.28`) — so the gap identified in both this doc and `USE-CASE-product-team.md` independently is visible rather than implicit, and the next phase that fills it (something like a `ta-plan`/backlog-service crate) has an obvious home to land in.
3. **Do not extract anything to a separate repo before the 3-tier model (C) and the Autoreward/validation work (12.27) both stabilize** — re-evaluate the separate-repos question after those ship, not before, using amplified-office's actual experience as a live case study of what coordination costs really look like in practice (it's already paying them right now, waiting on `v0.17.5.1/5.2/5.3`).
