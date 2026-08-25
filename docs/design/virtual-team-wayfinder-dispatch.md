# Virtual Team ↔ Wayfinder Dispatch — Design Options & Recommendation

> Design spike, 2026-08-25. Triggered by: "Wayfinder will definitely push tasks to the virtual team through the project manager or chief of staff... I expect there needs to be a push mechanism too... Red team come up with a plan, examining wayfinder and considering Studio. Do we need a central command and control Amplified Office dashboard?" Scope: how a private-repo virtual team receives Wayfinder-assigned work, executes it, reports back, and how humans stay in the loop. This is a planning document — no code in this repo implements it.

---

## 1. Problem statement

v0.17 is done. `ta-plan-wayfinder` (v0.17.11.3) and `ta-agent-whiteboard` (v0.17.11.2) both merged and give TA two solid, narrow primitives:

- **`ta-plan-wayfinder`**: TA's local PLAN.md is the structural source of truth; Wayfinder is a **status mirror** humans watch. One direction only (local → Wayfinder), and it only carries *TA's own* plan-phase/goal state.
- **`ta-agent-whiteboard`**: presence, discovery, task-claim, and durable handoff **among TA agents already running concurrently on the same codebase**. It has no concept of Wayfinder, dispatch, or external task sources.

Neither solves what's being asked now: **Wayfinder holding a backlog of tasks that a virtual team should pick up and execute, with results and new work flowing back.** That's a third, distinct integration direction — "Wayfinder as a work *source*," not "Wayfinder as a status *sink*." It should not be bolted onto either existing crate; both were deliberately scoped narrower than this, and stretching them would recreate the exact "candidate 2, rejected" shape from the original Sub-project 3 design doc (Wayfinder-as-primary, touching `goal_run.rs` execution semantics directly) — for the same reasons it was rejected then: no offline mode, and it collapses two independently-useful systems into one that only works when both are up.

## 2. Ground truth (from direct source review, 2026-08-25)

**Wayfinder's dispatch (`wayfinder-orchestration`)**:
- `dispatch_ready_queue()` and `find_role_for_verb()` (`store.rs:141-166`) do flat, first-match-by-verb assignment. `TeamRole { handles_verbs: Vec<String>, is_human, active }` is a static roster — there is no hierarchy, no "manager" concept, no role that receives-and-redistributes.
- **Zero outbound push exists.** `wayfinder-api/src/routes/dispatch.rs` and `ready_queue.rs` are plain REST endpoints — something has to call them. No webhooks, no SSE, no queue consumer.
- `Task.assignee_id` is inert metadata (`wayfinder-core/src/models.rs:106`) — setting it fires no event.
- When no role matches a verb, Wayfinder's own dispatcher already records `Decision::Escalate` — this is a *different* escalation than "agent needs human input"; it means "nobody on the roster can do this verb at all."

**"TA Studio" is not a separate app.** It's the daemon's own static HTML (`crates/ta-daemon/assets/index.html`, `shell.html`), served on the same port as the API, gated by the same auth just hardened in v0.17.11.4. There is no React/Next frontend anywhere in TA. Its "Team & Roles" tab reads `.ta/team.toml` — static persona/agent config, not live routing.

**TA's escalation primitive is dead code in practice.** `AgentAction::Escalate`/`RoleRef` (`ta-session/src/agent_action.rs`) is real in the type system, but `EscalatePrimitive` in `action_router.rs` only logs — it does not deliver, notify, or route to anyone. The mechanism that actually works today is the older, separate `ta_ask_human`/`ta_human_verify` file-polling path: an agent writes `.ta/interactions/pending/*.json`, the daemon exposes it over `/api/interactions/*`, and the dashboard (the same static HTML above) already renders and answers it. It works, but it's undifferentiated — "whoever is watching the dashboard," not routed to a specific role.

**`office.rs`/`OfficeConfig`/`ProjectRegistry`** is multi-*project* + external-channel (Discord/Slack/email) routing. It answers "which project does this message belong to," not "which team member should do this task." Reusing it here would be a category error.

**Wayfinder's own web UI is real and decent**: Board, Goals, Queue, Roster, Tasks, Time, Settings pages exist today (`web/app/(app)/[orgSlug]/[projectSlug]/...`). This is the one genuinely-built multi-page dashboard in the whole picture.

## 3. The core tension, named directly

You described "Wayfinder pushes tasks." Wayfinder cannot push anything today — it's pull-only, and adding real push (webhooks/SSE/queue) is new work on the Wayfinder side, in a repo that just spent a session getting its auth model hardened. Two ways to close that gap:

**Option A — Build real push into Wayfinder.** Webhook delivery on task-ready/assignee-changed, or an SSE stream. Correct long-term, but it's new Wayfinder-side infrastructure (delivery retries, signing, dead-lettering — the same class of problem TA's own webhook routes already solve once), and it makes the virtual team depend on Wayfinder's uptime for *every* new task, not just status sync.

**Option B — A thin polling adapter, on the TA/virtual-team side, that *feels* like push to the team.** The adapter polls Wayfinder's existing `/api/dispatch` + ready-queue endpoints (same bearer/service-account auth pattern `ta-plan-wayfinder` already uses) on a short interval. The moment it sees new or reassigned work, it **does not** hand it to a team member directly — it drops it onto the already-built, already-durable `ta-agent-whiteboard` handoff channel (JetStream-backed, durable, exactly built for "get this to the right peer even if they're not listening right now"). From the team member's point of view, it *is* a push — they receive a handoff message, same as they would from a sibling agent today. Reporting back (completion, blockers, new work discovered) goes the other way through the same adapter, via plain REST calls to Wayfinder (`PATCH` task status, `POST` new tasks with `external_id` for idempotent upsert — that endpoint already exists per the wayfinder work merged this session).

**Recommendation: B.** It needs zero new Wayfinder-side infrastructure — Wayfinder stays exactly what it is today, a REST API with real auth and a real UI. It reuses two already-solid, already-tested TA primitives (`ta-plan-wayfinder`'s client/auth pattern, `ta-agent-whiteboard`'s handoff) instead of building a third delivery mechanism. And it fails safe: if the poller is down, nothing breaks except *new* task pickup — in-flight work and existing status keep working, unlike Option A where a Wayfinder outage would break live delivery. Revisit Option A only if polling latency (whatever interval you pick — seconds-to-low-minutes is the realistic range) turns out to be a real product problem, not before.

## 4. The "single orchestration role" — has to be invented, not found

You framed this as "the project manager or chief of staff or whatever the single orchestration role is" — red-teaming that: **it doesn't exist yet, on either side.** Wayfinder's `TeamRole` has no manager/gatekeeper concept; TA's `.ta/team.toml` is flat persona config. This needs to be designed, not wired up.

Split it into two layers, because they have different failure/judgment characteristics:

1. **The poller (Option B above)** — deterministic, no LLM, lives in the new private repo. Its only job: notice Wayfinder state changes, translate a Wayfinder `Task` into a handoff-message candidate, and translate handoff outcomes back into Wayfinder REST calls. This is the "simple" part — a REST client and a JetStream publisher, maybe a few hundred lines. It should not make judgment calls.
2. **The chief-of-staff persona** — a real `TeamRole` entry in `.ta/team.toml` (matching the existing convention), backed by an ordinary TA agent invocation (`ta run`, same machinery every other goal already uses — nothing new to build here). It receives the poller's candidates and does the part that actually needs judgment: which team member's remit this matches, whether it overlaps in-flight work, how to phrase the goal brief handed to whoever executes it. It emits its routing decision as a normal whiteboard handoff to the chosen team member.

This keeps the *mechanical* surface (the new component) small and boring by construction, while the *judgment* surface reuses infrastructure that already exists (goal execution) instead of inventing a new "orchestrator agent runtime."

## 5. Reconciling push-down and push-up into one flow

```
Wayfinder (task backlog, REST, no push)
    │  poll: ready-queue / dispatch
    ▼
Poller  ──────────────────────────────► translates Task → candidate
    │
    ▼
Chief-of-staff persona (ta run, ordinary goal)
    │  routes + writes brief
    ▼
Whiteboard handoff (durable, JetStream)  ──► Team member (ordinary ta run goal)
    │                                              │
    │        ◄── completion / blocked / new-work ──┘  (handoff reply, peer→peer)
    ▼
Chief-of-staff persona reacts:
    - done        → poller PATCHes Wayfinder task status
    - blocked      → poller PATCHes status + comment; if it needs a human,
                     falls into §6 below
    - new work found → poller POSTs a new Wayfinder task (external_id = TA goal id,
                     upsert-safe — this endpoint already exists)
```

One state machine, one owner (the chief-of-staff persona) for "what does this Wayfinder task's lifecycle mean right now" — the poller never makes that decision, it only carries bytes in both directions. This avoids the classic bidirectional-sync bug class (both sides think they own a field): Wayfinder task status is *only ever written* by the poller acting on the chief-of-staff's instruction, never inferred independently on both ends. Same field-ownership discipline `ta-plan-wayfinder`'s design doc already established for the status-mirror direction — reapplied here for the new direction.

## 6. Human feedback and escalation — where it actually surfaces

Two escalation classes exist and stay separate, because they mean different things:

- **"Nobody can do this verb"** (Wayfinder's own `Decision::Escalate`) — this is a roster/capability gap, and it's already visible wherever Wayfinder's own UI shows dispatch decisions (Queue/Board). No new work needed; it's Wayfinder's job to surface, and it already does.
- **"An agent needs a human to decide something"** — this is TA's job, and TA already has a *working* mechanism for it: `ta_ask_human`/`ta_human_verify`, rendered in the daemon dashboard. Don't build a second one, and don't try to resurrect `AgentAction::Escalate`/`RoleRef` for this purpose without first deciding whether it's worth making that primitive real (it currently isn't, and nothing here requires it to be — the file-polling path already threads through role-less "whoever's watching" fine for a single small team).

Recommendation: **keep answering where it already works** (TA's dashboard), but close the "who even knows to look" gap cheaply — when the chief-of-staff persona raises a `ta_ask_human` interaction for Wayfinder-sourced work, have the poller attach a short comment + status flag (`blocked: needs-human`) to the originating Wayfinder task, linking back to the TA daemon's interaction URL. The human watching Wayfinder's board (which is where they're naturally looking, since that's "the director's" view per your own framing) sees the flag and a working link; they don't have to context-switch to *discover* something's stuck, only to *answer* it. This is a few lines of glue, not a new UI.

If a specific team member should be the one to answer (not just "whoever's watching"), that's a natural, low-cost extension once `RoleRef` is worth making real — but nothing in this design requires solving that up front.

## 7. Do we need a new "Amplified Office" C2 dashboard?

**Recommendation: no, not yet.** Two real, working UIs already exist and already cover different, non-overlapping concerns:

| Surface | What it's actually good at | What it should own here |
|---|---|---|
| **Wayfinder web UI** | Board/Queue/Roster/Tasks — this is what it's *for* | The director's view: what work exists, its priority, who it's assigned to, what's blocked at the roster-capability level |
| **TA daemon dashboard ("Studio")** | `/api/interactions` (working), whiteboard presence (live), draft review/apply (the actual code-change gate) | The operator's view: what's happening right now, what needs a human decision, reviewing the actual output |

Building a third dashboard would (a) duplicate real estate both already have well, (b) create a third "what's the current state" source of truth to keep consistent, (c) directly contradict the "keep this simple" instruction. The honest cost of *not* building one: a human overseeing the whole system checks two places instead of one. The mitigation is cheap — cross-link both directions (Wayfinder task ↔ TA goal/interaction, both ways, both already have stable IDs to link on) rather than build a third page that re-renders what the other two already render live.

This matches a pattern already used twice in this codebase (`task-graph` OSS extraction, `ta-agent-whiteboard`'s in-tree-first landing): **prove the two-surface-plus-links shape in real use first; only build a unifying dashboard if that turns out to be insufficient in practice**, not speculatively. If it does prove insufficient, the fallback isn't a ground-up build — it's a thin aggregation view (read-only, no new state) inside whichever surface the pain shows up in first.

## 8. Component boundary — what's new, what's reused

| Component | Status | Lives where |
|---|---|---|
| `ta-plan-wayfinder` | done, v0.17.11.3 | TA core (this repo) — local plan → Wayfinder status mirror. Unrelated direction; don't conflate. |
| `ta-agent-whiteboard` | done, v0.17.11.2 | TA core (this repo) — peer presence/handoff among TA agents. Reused as-is, unmodified. |
| **Wayfinder dispatch poller** | new | **Private virtual-team repo** — thin REST client (dispatch/ready-queue poll, task PATCH/POST), publishes/consumes whiteboard handoffs. No LLM, no judgment logic. |
| **Chief-of-staff persona** | new (config + prompt, not new runtime) | Private repo's `.ta/team.toml` + a goal brief template — executed via TA's existing `ta run`/goal machinery. No new agent-execution code needed. |
| Human escalation glue | new, small | Poller adds a comment/status write on `ta_ask_human` events — a few dozen lines, not a subsystem. |
| Amplified Office dashboard | **not built** | Deferred per §7; revisit only on evidence. |

The new private repo's actual surface area, by this design, is genuinely narrow: one REST client against Wayfinder's existing API, wired to one existing durable transport (whiteboard), plus config for one new team role. It composes on top of two already-hardened TA-core primitives rather than reimplementing either — which is the "simple but not constrained" property you asked for: nothing here blocks the private repo from later adding more roles, more Wayfinder projects, or swapping the poll interval/transport, because none of that touches TA core.

## 9. Open decisions (yours, not mine to assume)

1. **Poll interval** — a few seconds feels "live enough" for a small team; every poll is a real API call against Wayfinder's (now-hardened) auth path. Pick a default, make it configurable.
2. **Where the chief-of-staff persona's *judgment* comes from** — a fixed prompt/rubric, or should it eventually be a more capable/expensive model tier than execution-worker roles? Not required to decide before building the mechanical layer.
3. **Multi-Wayfinder-project scope** — one poller instance per Wayfinder org/project, or one poller fanning out across several? Affects the private repo's config shape from day one, cheap to decide now, expensive to retrofit.
4. **Whether to formally name and scaffold the private repo now** — this doc assumes it exists; nothing here has created it.

## 10. Suggested build order (once you confirm direction)

1. Scaffold the private repo, thin Wayfinder REST client (reuse `ta-plan-wayfinder`'s auth/HTTP pattern by reference, not by cross-repo dependency — light duplication across the public/private boundary is the right call here, not a shared crate, matching this project's own "prove in-tree first" precedent).
2. Poller: read-only first (log candidates, no handoff yet) — cheap to verify against a real Wayfinder project before anything acts on it.
3. Wire the whiteboard handoff leg (poller → chief-of-staff → team member), still no Wayfinder writes.
4. Wire the report-back leg (status PATCH, new-task POST) behind an explicit dry-run flag until trusted.
5. Human-escalation glue (§6).
6. Cross-links (§7) last — cosmetic relative to the rest, easy to defer without blocking anything.

Each step is independently testable against a real (or sandboxed) Wayfinder project before the next one starts writing.
