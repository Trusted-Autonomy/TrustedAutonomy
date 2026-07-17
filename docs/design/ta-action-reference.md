# TA Action Reference

**Status**: canonical reference, distilled from [`ta-concepts-and-architecture.md`](ta-concepts-and-architecture.md) §1–12, 2026-07-04.
**Purpose**: the complete instruction set TA operates in — every action, grouped, and how real flows compose them. Deliberately mirrors [kwg/sage-lore](https://github.com/kwg/sage-lore)'s own README shape (Execution Model → grouped Primitives → composed Flows) since that presentation is exactly what was asked for — without adopting sage-lore itself (see [`sage-lore-review.md`](sage-lore-review.md): complement, don't depend on it).
**Read first if you want the plain version**: [`docs/guides/what-is-ta.md`](../guides/what-is-ta.md).
**Current-state architecture reference**: [`ta-architecture-reference.md`](../architecture/ta-architecture-reference.md) — the maintainer-level "how it is set up" doc; this reference is scoped to actions, that one is scoped to the tier/crate/data-boundary architecture those actions run inside.
**Data shapes for these actions**: [`ta-data-format-spec.md`](ta-data-format-spec.md) — the versioned JSON Schema for `Goal`, `Draft`/`Artifact`, `TriggerEvent`, `RoutingDecision`, and `Persona`, the data these actions actually operate on.

---

## Execution Model

A **goal** is the trust boundary. Everything an agent does happens inside that goal's private staging copy; nothing touches a real target until it passes through **Commit** — the one privileged, TA-mediated action every application (VCS, DB, Social, Email, Release, …) implements. Agents write freely inside the boundary. Only TA crosses it.

---

## Actions, by Category

### Core Flow — output direction (proposing changes to the world)
- **Write** — agent proposes a change in staging.
- **Review** — evaluates a Write, produces a verdict + confidence + risk.
- **Decision** — routes a Review's output to Commit, Reject, Rework, or Escalate.
- **Commit** — the privileged, TA-mediated action that materializes a Write. Per-application (VCS commit+push, DB mutation-apply, social publish, email send, release publish) — same invocation everywhere, different function body.
- **Reject** — discards a Write, audited.

### Core Flow — input direction (admitting information into a goal)
- **Fetch** — a connector retrieves external data (email, DB row, web page, issue).
- **Scan** — validates fetched content before it reaches agent context.
- **Admit** — content crosses into agent context.
- **Block** — content withheld, audited.

*(Decision is the same action in both directions — one node type, fed different signal sources depending on which side it's gating.)*

### Escalation & Aggregation — reused mechanisms, not separate primitives
- **HumanGate** — where a Decision routes when it can't confidently resolve on its own. A human's answer becomes a fresh Decision input.
- **Consensus** — one way to implement Review: aggregate N parallel reviews into a single verdict (Raft / Paxos / Weighted — already built and shipped).

### Agent Operations
- **Invoke** — run a specific agent (framework + model + persona) against a goal or role.
- **Switch** — change which agent/model/framework backs a role, without redefining the whole persona. **Confirmed product requirement (2026-07-04): must be low-friction** — today this exists only as a `--agent` flag / static `[agent_profiles.*]` config, not a first-class, easy-to-change action.
- **Parallel** — run multiple Invokes concurrently. Used today inside Consensus's 4-reviewer template; true independent concurrent *sub-goal* execution is a separate, already-tracked gap (deferred to v0.13.16).

### Flow Control
- **Rework** — a Reject's (or a denied Decision's) loop back to Write, same staging, retry with feedback.
- **Route** — Decision's branch selection, resolved by one of three modes: explicit override, workflow-defined default, or supervisor-recommended/auto-select.

### Telemetry & Audit
- **Audit** — every significant action appended to the hash-chained, tamper-evident log. Built (`ta audit verify`) — this is the compliance/security trail.
- **Meter** — cost, tokens, duration, confidence, and risk recorded *per action*, not just in aggregate. **Confirmed product requirement (2026-07-04): "good telemetry."** Today only one aggregate cost stat exists project-wide; there is no per-goal or per-action breakdown, and no turn-level activity stream. Distinct from Audit — Audit is the tamper-evident compliance record, Meter is the observability/metrics stream a dashboard or cost report would read from.

### Configuration — CRUD, not graph actions
- **create / list / show / update / remove / sync** — manage the entities the graph is built from: goals, drafts, teams, personas, workflows, plugins, agent-framework bindings.

---

## Composed Flows

How the action set actually gets used:

- **Standard implementation**: `Invoke(implementer)` → `Write` → `Review` → `Decision` → `Commit` (or `Reject` → `Rework`).
- **Consensus review**: `Invoke` ×N (distinct personas, `Parallel`) → `Review` ×N → `Consensus` → `Decision` → `Commit`/`Reject`. Already shipped (`code-review-consensus.toml`).
- **Content/social**: `Invoke` → `Write` (platform draft) → `Review` (confidence gate) → `Decision` → `Commit` (publish — policy-gated, not yet implemented) or held for a human.
- **DB migration**: `Invoke` → `Write` (mutation log) → *(Review/Decision missing — gap)* → `Commit` (apply mutation) or `Reject`.
- **Context/input**: `Fetch` → `Scan` → `Decision` → `Admit` (feeds a Write's context) or `Block`.
- **Escalation**: `Decision` → `HumanGate` → human answers → fresh `Decision`.
- **Autonomous multi-phase loop** (the self-reinforcing case): `Invoke` → `Write` → `Review` → `Decision` (auto or human) → `Commit` → next phase's `Invoke`, repeating without a human in the loop once trust thresholds are met. This is the literal target state of "autonomous workflows to first class," and the reason the auto-approval work matters more than any single other item on the list.

---

## Built vs. Gap (2026-07-04)

| Action | Status |
|---|---|
| Write, Commit, Reject (VCS) | ✅ built |
| Review, Decision (VCS) | ⚠️ verdict only — no confidence/risk threshold |
| Consensus | ✅ built (Raft/Paxos/Weighted, 37 tests) — ⚠️ not generalized beyond one template |
| HumanGate | ✅ built |
| Review, Decision (Social) | ✅ built (`social_supervisor_check`) |
| Commit (Social, i.e. `publish`) | ❌ gap — policy default, not architectural, per §9 |
| Write, Commit (DB) | ⚠️ trait methods exist |
| Review, Decision (DB) | ❌ gap — don't exist at all |
| Fetch, Scan, Decision, Admit/Block | ⚠️ path-traversal only; secret-scan-on-fetch and PI-scan are gaps |
| Invoke | ✅ built |
| Switch (low-friction agent/model swap) | ⚠️ flag-level only, not first-class |
| Parallel | ✅ built (Consensus template) — true concurrent sub-goals deferred (v0.13.16) |
| Audit | ✅ built |
| Meter (per-action telemetry) | ❌ gap — aggregate-only today |
| create/list/show/update/remove/sync | ✅ built — consolidation into a 10-verb CLI surface pending |

This table is the actual punch list. The reasoning behind every row lives in `ta-concepts-and-architecture.md` §1–12.
