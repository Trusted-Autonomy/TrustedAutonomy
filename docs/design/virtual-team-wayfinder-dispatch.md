# Virtual Team ↔ Wayfinder Dispatch — Design Options & Recommendation

> Design spike, 2026-08-25. Rev 2 (same day) folds in a Wayfinder-side review grounded in that repo's just-completed auth/rate-limit hardening work — see the changelog note at the end. Triggered by: "Wayfinder will definitely push tasks to the virtual team through the project manager or chief of staff... I expect there needs to be a push mechanism too... Red team come up with a plan, examining wayfinder and considering Studio. Do we need a central command and control Amplified Office dashboard?" Scope: how a private-repo virtual team receives Wayfinder-assigned work, executes it, reports back, and how humans stay in the loop. This is a planning document — no code in this repo implements it.

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
- **`/api/dispatch` and the ready-queue routes are not actually rate-limited**, despite sitting behind Wayfinder's now-hardened auth: the per-project router (`/api/projects/:project_id/*rest`) constructs a fresh inner router — and therefore fresh rate-limiter state — on every request, so any limiter mounted there is structurally inert. Nothing on Wayfinder's side backstops a misbehaving or runaway poller; interval and backoff discipline has to live entirely on the poller side (see §9.1, §9.3).
- **`member` is the correct service-account tier for the poller**, not higher. It's the minimum role that grants `EditTasksGoals` (task/goal read+write, including the `external_id` upsert path) plus dispatch access. `owner`-tier is deliberately reserved for the export endpoint and isn't needed anywhere in this design — worth stating explicitly (see §8) both as a security minimum and so the poller's credential blast radius is honest if it's ever compromised.

**"TA Studio" is not a separate app.** It's the daemon's own static HTML (`crates/ta-daemon/assets/index.html`, `shell.html`), served on the same port as the API, gated by the same auth just hardened in v0.17.11.4. There is no React/Next frontend anywhere in TA. Its "Team & Roles" tab reads `.ta/team.toml` — static persona/agent config, not live routing.

**TA's escalation primitive is dead code in practice.** `AgentAction::Escalate`/`RoleRef` (`ta-session/src/agent_action.rs`) is real in the type system, but `EscalatePrimitive` in `action_router.rs` only logs — it does not deliver, notify, or route to anyone. The mechanism that actually works today is the older, separate `ta_ask_human`/`ta_human_verify` file-polling path: an agent writes `.ta/interactions/pending/*.json`, the daemon exposes it over `/api/interactions/*`, and the dashboard (the same static HTML above) already renders and answers it. It works, but it's undifferentiated — "whoever is watching the dashboard," not routed to a specific role.

**`office.rs`/`OfficeConfig`/`ProjectRegistry`** is multi-*project* + external-channel (Discord/Slack/email) routing. It answers "which project does this message belong to," not "which team member should do this task." Reusing it here would be a category error.

**Wayfinder's own web UI is real and decent**: Board, Goals, Queue, Roster, Tasks, Time, Settings pages exist today (`web/app/(app)/[orgSlug]/[projectSlug]/...`). This is the one genuinely-built multi-page dashboard in the whole picture.

## 3. The core tension, named directly

You described "Wayfinder pushes tasks." Wayfinder cannot push anything today — it's pull-only, and adding real push (webhooks/SSE/queue) is new work on the Wayfinder side, in a repo that just spent a session getting its auth model hardened. Two ways to close that gap:

**Option A — Build real push into Wayfinder.** Webhook delivery on task-ready/assignee-changed, or an SSE stream. Correct long-term, but it's new Wayfinder-side infrastructure (delivery retries, signing, dead-lettering — the same class of problem TA's own webhook routes already solve once), and it makes the virtual team depend on Wayfinder's uptime for *every* new task, not just status sync.

**Option B — A thin polling adapter, on the TA/virtual-team side, that *feels* like push to the team.** The adapter polls Wayfinder's existing `/api/dispatch` + ready-queue endpoints (same bearer/service-account auth pattern `ta-plan-wayfinder` already uses) on a short interval. The moment it sees new or reassigned work, **it does not call the chief-of-staff persona directly — no RPC, no direct call.** It publishes an ordinary message onto the already-built, already-durable `ta-agent-whiteboard` handoff channel (JetStream-backed, durable, exactly built for "get this to the right peer even if they're not listening right now"); the chief-of-staff persona consumes it the same way it would consume any other whiteboard message, with no special-cased path. From the team member's point of view further downstream, it *is* a push — they receive a handoff message, same as they would from a sibling agent today. Reporting back (completion, blockers, new work discovered) goes the other way through the same adapter, via plain REST calls to Wayfinder (`PATCH` task status, `POST` new tasks with `external_id` for idempotent upsert — that endpoint already exists per the wayfinder work merged this session).

Keeping the poller and the persona mutually unaware of each other — connected only by the message on the whiteboard, never a direct call — is the actual independence property this design is after: either side can be swapped, scaled, or duplicated without the other needing to change. (An earlier pass of this document blurred that line; §4/§5 below are now consistent with this framing throughout.)

**Recommendation: B.** It needs zero new Wayfinder-side infrastructure — Wayfinder stays exactly what it is today, a REST API with real auth and a real UI. It reuses two already-solid, already-tested TA primitives (`ta-plan-wayfinder`'s client/auth pattern, `ta-agent-whiteboard`'s handoff) instead of building a third delivery mechanism. And it fails safe: if the poller is down, nothing breaks except *new* task pickup — in-flight work and existing status keep working, unlike Option A where a Wayfinder outage would break live delivery. Revisit Option A only if polling latency (whatever interval you pick — seconds-to-low-minutes is the realistic range) turns out to be a real product problem, not before.

## 4. The "single orchestration role" — has to be invented, not found

You framed this as "the project manager or chief of staff or whatever the single orchestration role is" — red-teaming that: **it doesn't exist yet, on either side.** Wayfinder's `TeamRole` has no manager/gatekeeper concept; TA's `.ta/team.toml` is flat persona config. This needs to be designed, not wired up.

Split it into two layers, because they have different failure/judgment characteristics, and give the seam between them a real type instead of prose:

1. **The poller (Option B above)** — deterministic, no LLM, lives in the new private repo. Its only job: notice Wayfinder state changes, translate a Wayfinder `Task` into a handoff-message candidate, and translate handoff outcomes back into Wayfinder REST calls. This is the "simple" part — a REST client and a JetStream publisher, maybe a few hundred lines. It should not make judgment calls.

   The Wayfinder-client half and the whiteboard-publisher half of the poller should only communicate through one small internal type — a `TaskCandidate` struct, not the raw Wayfinder DTO passed straight through. This is an anti-corruption layer: if Wayfinder's `Task` shape changes, or the wire schema published onto the whiteboard changes (§8.5), only the construction of `TaskCandidate` moves, not the whole poller.

   The poller also owns its own **audit trail**: every Wayfinder write it makes gets correlated back to the `candidate_id`/persona decision that caused it (the `candidate`/`outcome` schema in §8.5 carries this by construction). With three hops — Wayfinder ↔ poller ↔ whiteboard ↔ persona — a wrong status update needs to be attributable to "poller bug" vs. "bad persona judgment" vs. "Wayfinder API behavior" without guesswork after the fact.

   `external_id` derivation for the "new work found" path has to be **stable across retries**, not freshly generated per attempt (e.g. `ta-goal:<goal_id>`, never a fresh UUID each time the persona re-emits the same logical outcome). Wayfinder's upsert is idempotent on this field; a wobbly id defeats that silently, with no error to notice — this is the same bug shape the upsert fix on the Wayfinder side was originally closing.

2. **The chief-of-staff persona** — a real `TeamRole` entry in `.ta/team.toml` (matching the existing convention), backed by an ordinary TA agent invocation (`ta run`, same machinery every other goal already uses — nothing new to build here). It receives the poller's candidates and does the part that actually needs judgment: which team member's remit this matches, whether it overlaps in-flight work, how to phrase the goal brief handed to whoever executes it. It emits its routing decision as a normal whiteboard handoff to the chosen team member.

   **Model tier is resolved, not left open**: the chief-of-staff persona runs on the **highest reasoning-effort model tier the user has selected** for the session/deployment — it's the one role in this design making judgment calls that affect routing, priority, and what becomes a new Wayfinder task, so it gets the top tier by default, not a fixed model name pinned in config. Every other team-member persona runs on the **best lower-cost model that fits that specific role's task class** — chosen per role (a docs-fix worker doesn't need the same tier as a design-judgment worker), not a single uniform "cheap tier for everyone" setting. Concretely: `.ta/team.toml` gives the chief-of-staff role a `model_tier: highest` (or equivalent alias that resolves to whatever top-tier model the user has configured) while other roles set an explicit, role-appropriate tier.

This keeps the *mechanical* surface (the new component) small and boring by construction, while the *judgment* surface reuses infrastructure that already exists (goal execution) instead of inventing a new "orchestrator agent runtime."

## 5. Reconciling push-down and push-up into one flow

```
Wayfinder (task backlog, REST, no push)
    │  poll: ready-queue / dispatch
    ▼
Poller  ──────────────────────────────► Wayfinder Task → TaskCandidate (internal type)
    │
    ▼  publish `candidate` message (§8.5 schema) — no direct call
Whiteboard handoff (durable, JetStream)
    │  consumed independently, same as any other message
    ▼
Chief-of-staff persona (ta run, highest reasoning tier)
    │  routes + writes brief
    ▼
Whiteboard handoff  ──► Team member (ordinary ta run goal, role-appropriate lower-cost tier)
    │                                              │
    │        ◄── completion / blocked / new-work ──┘  (handoff reply, peer→peer)
    ▼
Chief-of-staff persona reacts, publishes `outcome` message (§8.5 schema):
    - done          → poller PATCHes Wayfinder task status
    - blocked        → poller PATCHes status + raises escalation; if it needs a human,
                       falls into §6 below
    - new work found → poller POSTs a new Wayfinder task (external_id = stable
                       `ta-goal:<goal_id>`, upsert-safe — endpoint already exists)
```

One state machine, one owner (the chief-of-staff persona) for "what does this Wayfinder task's lifecycle mean right now" — the poller never makes that decision, it only carries bytes in both directions, correlated by `candidate_id`. This avoids the classic bidirectional-sync bug class (both sides think they own a field): Wayfinder task status is *only ever written* by the poller acting on the chief-of-staff's instruction, never inferred independently on both ends. Same field-ownership discipline `ta-plan-wayfinder`'s design doc already established for the status-mirror direction — reapplied here for the new direction.

**Open question this flow surfaces**: whether chief-of-staff invocations for *different* candidates can run concurrently. The design is careful about single-writer discipline for Wayfinder status and about the poller never judging — but if two concurrent persona invocations touch the same Wayfinder task or the same whiteboard thread, the dual-writer problem this flow otherwise avoids comes back in through the side door. Tracked as an open decision in §9.5; not resolved here because it doesn't block building the mechanical layer first.

## 6. Human feedback and escalation — where it actually surfaces

Two escalation classes exist and stay separate, because they mean different things:

- **"Nobody can do this verb"** (Wayfinder's own `Decision::Escalate`) — a roster/capability gap.
- **"An agent needs a human to decide something"** — TA's job, via the *working* `ta_ask_human`/`ta_human_verify` mechanism, rendered in the daemon dashboard.

**v1 (ship first, zero new Wayfinder infrastructure)**: keep answering where it already works (TA's dashboard). Close the "who even knows to look" gap cheaply — when the chief-of-staff persona raises a `ta_ask_human` interaction for Wayfinder-sourced work, the poller attaches a short comment + status flag (`blocked: needs-human`) to the originating Wayfinder task, linking back to the TA daemon's interaction URL. This was deliberately the cheapest possible option, chosen specifically because it costs nothing on the Wayfinder side — that was the whole point of Option B. Don't resurrect `AgentAction::Escalate`/`RoleRef` for this; nothing here requires it to be real yet.

**v2 (explicitly costed — the first thing in this whole design that isn't free on the Wayfinder side): a real Notification primitive.** It's a genuine upgrade over a task comment, worth naming as its own line item in §8 rather than folding into "poller + persona" as if it came for free:

1. A task comment conflates "this task is blocked" with "a human needs an out-of-band decision" — not always the same thing (an ambiguous routing call, or "should new-work-found even become a task" isn't tied to any single existing Wayfinder task). A Notification entity isn't forced to hang off a task record the way a comment is.
2. It's the only way to actually carry priority — a task comment has no room for it.
3. It fits Wayfinder's own role from §7 directly: "the director's view — what work exists, its priority, what's blocked." A notification inbox *is* that view's job. It doesn't compete with TA's dashboard, which stays where the interaction actually gets *answered* — §7's ownership split is unchanged by this.
4. It can absorb Wayfinder's other escalation class too: today `Decision::Escalate` (roster-capability gap) is only visible if you're already looking at Queue/Board. Give both escalation kinds — roster-gap and TA-human-needed — one shared priority taxonomy and one inbox, discriminated by a `source` field (`roster_capability_gap` vs. `ta_human_escalation`), with only the TA-sourced ones carrying a link out to the daemon. Strict improvement on the "two places, cross-linked" fallback in §7 — it doesn't let a human *resolve* a TA interaction from inside Wayfinder, only surface-and-link, so §7's ownership table still holds.

**The raise/clear lifecycle has to ship as one unit, not half.** v1's `blocked: needs-human` flag already has a gap: nothing clears it once a human answers via TA's dashboard. A real Notification sharpens this into an explicit `resolved_at`/dismissed state — but the fix doesn't need a new component. The same poller that already publishes candidates and reads back completions should, in the same reconciliation loop, poll interaction-resolution status and `PATCH` the notification to resolved. Don't ship the raise half without the clear half in the same change, whichever version (v1 or v2) is built.

**Priority is one shared enum, defined once — on the Wayfinder side**, since Wayfinder owns the sorting/surfacing UI — and referenced by value in the `candidate`/`outcome` schema (§8.5), never redefined independently on the TA side. Two independently-maintained severity scales drift the first time someone adds a level to one and not the other.

If a specific team member (not "whoever's watching") should be the one to answer, that's a natural, low-cost extension once `RoleRef` is worth making real — still not required up front.

## 7. Do we need a new "Amplified Office" C2 dashboard?

**Recommendation: no, not yet.** Two real, working UIs already exist and already cover different, non-overlapping concerns:

| Surface | What it's actually good at | What it should own here |
|---|---|---|
| **Wayfinder web UI** | Board/Queue/Roster/Tasks, plus a Notification inbox if §6 v2 gets built | The director's view: what work exists, its priority, who it's assigned to, what's blocked (roster-level and human-escalation-level alike) |
| **TA daemon dashboard ("Studio")** | `/api/interactions` (working), whiteboard presence (live), draft review/apply (the actual code-change gate) | The operator's view: what's happening right now, what needs a human decision, reviewing the actual output, and where interactions get *answered* |

Building a third dashboard would (a) duplicate real estate both already have well, (b) create a third "what's the current state" source of truth to keep consistent, (c) directly contradict the "keep this simple" instruction. The honest cost of *not* building one: a human overseeing the whole system checks two places instead of one. The mitigation is cheap — cross-link both directions (Wayfinder task/notification ↔ TA goal/interaction, both ways, both already have stable IDs to link on) rather than build a third page that re-renders what the other two already render live. A Notification primitive (§6 v2), if built, strengthens this split rather than undermining it — it's still Wayfinder's job to surface, TA's job to answer.

This matches a pattern already used twice in this codebase (`task-graph` OSS extraction, `ta-agent-whiteboard`'s in-tree-first landing): **prove the two-surface-plus-links shape in real use first; only build a unifying dashboard if that turns out to be insufficient in practice**, not speculatively. If it does prove insufficient, the fallback isn't a ground-up build — it's a thin aggregation view (read-only, no new state) inside whichever surface the pain shows up in first.

## 8. Component boundary — what's new, what's reused

| Component | Status | Lives where |
|---|---|---|
| `ta-plan-wayfinder` | done, v0.17.11.3 | TA core (this repo) — local plan → Wayfinder status mirror. Unrelated direction; don't conflate. |
| `ta-agent-whiteboard` | done, v0.17.11.2 | TA core (this repo) — peer presence/handoff among TA agents. Reused as-is, unmodified. |
| **Wayfinder dispatch poller** | new | **Private virtual-team repo** — thin REST client (dispatch/ready-queue poll, task PATCH/POST), publishes/consumes whiteboard handoffs via the §8.5 schema. No LLM, no judgment logic. Authenticates as a Wayfinder service account at **`member` role**, never `owner`. Owns its own poll-interval/backoff discipline — Wayfinder's rate limiter on these routes is structurally inert (§2), so nothing backstops the poller from the other side. |
| **Chief-of-staff persona** | new (config + prompt, not new runtime) | Private repo's `.ta/team.toml` + a goal brief template — executed via TA's existing `ta run`/goal machinery, `model_tier: highest`. No new agent-execution code needed. |
| Human escalation glue (v1) | new, small | Poller adds a comment/status write on `ta_ask_human` events — a few dozen lines, not a subsystem. |
| **Wayfinder Notification primitive (v2)** | new, **costed Wayfinder-side work** — not free, not bundled into "poller + persona" | Wayfinder repo — new entity + endpoints (§8.5), surfaced in Wayfinder's own UI. Track as its own line, decide in §9.6 whether/when to build it. |
| Amplified Office dashboard | **not built** | Deferred per §7; revisit only on evidence. |

The new private repo's actual surface area, by this design, is genuinely narrow: one REST client against Wayfinder's existing API, wired to one existing durable transport (whiteboard), plus config for one new team role. It composes on top of two already-hardened TA-core primitives rather than reimplementing either — which is the "simple but not constrained" property you asked for: nothing here blocks the private repo from later adding more roles, more Wayfinder projects, or swapping the poll interval/transport, because none of that touches TA core.

### 8.5 The API surface — two independent contracts

Two separate contracts exist here and should stay independently documented, so the poller is a translator between two documented interfaces rather than the only thing that understands both systems' internals. Without this, the only spec for the TA-facing side would be "speak whiteboard's internal JetStream message format" — which means only a TA-native client could ever hand the chief-of-staff work, cutting against the "so Wayfinder (or anything else) can integrate, and vice versa" goal directly.

**Contract 1 — Wayfinder's outbound REST contract.** Already the right shape, needs no rework: REST + JSON, service-account bearer auth (`member` role), versioned by path. Inherently swappable — anything that speaks HTTP can integrate, not just this poller. Add the notification endpoints here if/when §6 v2 gets built:

```
GET    /api/projects/:project_id/dispatch          (existing)
GET    /api/projects/:project_id/ready-queue        (existing)
PATCH  /api/projects/:project_id/tasks/:id           (existing)
POST   /api/projects/:project_id/tasks               (existing, external_id upsert)
POST   /api/projects/:project_id/notifications        (new, v2)
PATCH  /api/projects/:project_id/notifications/:id     (new, v2)
GET    /api/projects/:project_id/notifications         (new, v2)
```

**Contract 2 — the chief-of-staff handoff contract.** This is the one that doesn't exist yet as a contract, and needs to be defined independently of whiteboard-as-transport (whiteboard/JetStream is today's delivery mechanism for it — swappable later without either side changing, same anti-corruption reasoning as the poller's internal `TaskCandidate` type in §4, just moved to the actual system boundary):

```
candidate  { candidate_id,                       // stable, correlates outcome + audit trail
             source: "wayfinder",
             source_ref: { org_id, project_id, task_id },
             title, description,
             priority,                            // shared enum, owned/defined by Wayfinder (§6)
             requested_role: "chief-of-staff" }

outcome    { candidate_id,                        // correlates back to the candidate
             outcome: "done" | "blocked" | "new_work",
             wayfinder_task_id,                    // stable external_id when outcome = new_work
             detail }
```

This also answers the "vice versa" half directly: anything else that wants to hand the chief-of-staff work — not just this Wayfinder poller — only needs to speak this schema, not know whiteboard exists.

## 9. Open decisions (yours, not mine to assume)

1. **Poll interval** — a few seconds feels "live enough" for a small team; every poll is a real API call against Wayfinder's (now-hardened, but not rate-limited on these routes — §2) auth path. Pick a default, make it configurable, and own the backoff discipline on the poller side since Wayfinder won't backstop it.
2. ~~Chief-of-staff / persona model tier~~ **Resolved**: chief-of-staff runs at the highest reasoning-effort tier the user has selected; every other team-member persona runs at the best lower-cost tier that fits that role's specific task class, set per role in `.ta/team.toml` (§4).
3. **Multi-Wayfinder-project scope** — one poller instance per Wayfinder org/project, or one poller fanning out across several? Affects the private repo's config shape from day one, cheap to decide now, expensive to retrofit. The inert-rate-limiter fact in §2 applies here too: fan-out design shouldn't assume Wayfinder will ever throttle it.
4. **Whether to formally name and scaffold the private repo now** — this doc assumes it exists; nothing here has created it.
5. **Chief-of-staff concurrency model** — can persona invocations for different candidates run concurrently? If yes, the single-writer discipline in §5 needs to extend to that case explicitly (two concurrent invocations must never touch the same Wayfinder task or whiteboard thread) or the dual-writer problem this design otherwise avoids reappears.
6. **Whether/when to build the Wayfinder Notification primitive (§6 v2, §8.5 Contract 1 additions)** — real upgrade, but the first genuinely costed Wayfinder-side piece of this whole design. Ship v1 (comment + flag) first; decide on v2 with evidence from real use, not speculatively — same standard §7 already applies to the dashboard question.

## 10. Suggested build order (once you confirm direction)

1. Scaffold the private repo, thin Wayfinder REST client (reuse `ta-plan-wayfinder`'s auth/HTTP pattern by reference, not by cross-repo dependency — light duplication across the public/private boundary is the right call here, not a shared crate, matching this project's own "prove in-tree first" precedent). Authenticate as `member`-role service account (§2, §8).
2. Poller: read-only first (log `TaskCandidate`s, no handoff yet) — cheap to verify against a real Wayfinder project before anything acts on it. Define the `candidate`/`outcome` schema (§8.5) at this step, even though nothing publishes it yet.
3. Wire the whiteboard handoff leg (poller publishes `candidate` → chief-of-staff persona, highest-tier model, consumes independently → team member, role-tier model), still no Wayfinder writes. Nail down stable `external_id` derivation now (§4), before anything depends on upsert idempotency.
4. Wire the report-back leg (`outcome` → status PATCH, new-task POST) behind an explicit dry-run flag until trusted. Add the poller's audit trail (§4) in this step, not after.
5. Human-escalation glue, v1 (§6) — comment + flag, raise and clear together.
6. Cross-links (§7) — cheap, easy to defer without blocking anything.
7. Wayfinder Notification primitive, v2 (§6, §9.6) — only after evidence from steps 1-6 that v1's comment-flag approach is actually insufficient in practice.

Each step is independently testable against a real (or sandboxed) Wayfinder project before the next one starts writing.

---

**Changelog**: Rev 2 (2026-08-25) folds in a Wayfinder-side review of Rev 1, grounded in that session's actual work on Wayfinder's auth and rate-limiting. Changes: fixed a §3/§4/§5 flow inconsistency (poller never calls the persona directly — whiteboard-message-only, now stated explicitly and consistently); added the `TaskCandidate` internal type, stable `external_id` derivation, poller audit trail, and the chief-of-staff concurrency question (§4, §9.5); added two facts grounded in Wayfinder's current source — the structurally inert rate limiter on `/api/projects/*` dispatch/ready-queue routes, and `member` as the correct (not `owner`) service-account tier (§2, §8); expanded §6 into an explicit v1 (free) / v2 (costed Wayfinder Notification primitive) split with a shared-priority-enum rule and a raise/clear lifecycle requirement; added §8.5 defining two independent contracts — Wayfinder's REST DTOs and a versioned `candidate`/`outcome` handoff schema decoupled from whiteboard-as-transport; and resolved the model-tier open decision (chief-of-staff = highest reasoning tier the user selects; other personas = best lower-cost tier fitting each role).
