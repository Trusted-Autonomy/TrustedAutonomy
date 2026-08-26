# Virtual Team ↔ Wayfinder Dispatch — Design Options & Recommendation

> Design spike, 2026-08-25. Rev 2 folds in a Wayfinder-side review grounded in that repo's just-completed auth/rate-limit hardening work; Rev 3 replaces explicit recipient-addressing with topic-based, registration-driven consumption; Rev 4 turns §10 into a phased development plan and answers the split-sequencing question — see the changelog notes at the end. Triggered by: "Wayfinder will definitely push tasks to the virtual team through the project manager or chief of staff... I expect there needs to be a push mechanism too... Red team come up with a plan, examining wayfinder and considering Studio. Do we need a central command and control Amplified Office dashboard?" Scope: how a private-repo virtual team receives Wayfinder-assigned work, executes it, reports back, and how humans stay in the loop. This is a planning document — no code in this repo implements it.

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
- **Exact route verbs and shapes, confirmed 2026-08-25 by reading `wayfinder-api/src/routes/{dispatch,tasks,ready_queue}.rs` and `project_router.rs` directly**: every inner handler is flat (`/api/dispatch`, `/api/tasks/:id/status`, `/api/ready-queue`, ...) and `project_router.rs`'s `dispatch_to_project` rewrites `/api/projects/:project_id/*rest` → `/api/{rest}` before handing off — so the *external*, client-facing contract genuinely is `/api/projects/:project_id/...` (this is the same rewrite layer behind the inert-rate-limiter fact above), but `dispatch` is registered `post(dispatch)`, not `get` — it mutates (`DispatchResultDto` rows), so a GET would 405. And there is no bare `.../tasks/:id` PATCH — task status is a separate route, `.../tasks/:id/status`, with assignee (`.../tasks/:id/assignee`) as its own distinct PATCH. §8.5's Contract 1 table is corrected to match (Rev 5).

**"TA Studio" is not a separate app.** It's the daemon's own static HTML (`crates/ta-daemon/assets/index.html`, `shell.html`), served on the same port as the API, gated by the same auth just hardened in v0.17.11.4. There is no React/Next frontend anywhere in TA. Its "Team & Roles" tab reads `.ta/team.toml` — static persona/agent config, not live routing.

**TA's escalation primitive is dead code in practice.** `AgentAction::Escalate`/`RoleRef` (`ta-session/src/agent_action.rs`) is real in the type system, but `EscalatePrimitive` in `action_router.rs` only logs — it does not deliver, notify, or route to anyone. The mechanism that actually works today is the older, separate `ta_ask_human`/`ta_human_verify` file-polling path: an agent writes `.ta/interactions/pending/*.json`, the daemon exposes it over `/api/interactions/*`, and the dashboard (the same static HTML above) already renders and answers it. It works, but it's undifferentiated — "whoever is watching the dashboard," not routed to a specific role.

**`office.rs`/`OfficeConfig`/`ProjectRegistry`** is multi-*project* + external-channel (Discord/Slack/email) routing. It answers "which project does this message belong to," not "which team member should do this task." Reusing it here would be a category error.

**Wayfinder's own web UI is real and decent**: Board, Goals, Queue, Roster, Tasks, Time, Settings pages exist today (`web/app/(app)/[orgSlug]/[projectSlug]/...`). This is the one genuinely-built multi-page dashboard in the whole picture.

## 3. The core tension, named directly

You described "Wayfinder pushes tasks." Wayfinder cannot push anything today — it's pull-only, and adding real push (webhooks/SSE/queue) is new work on the Wayfinder side, in a repo that just spent a session getting its auth model hardened. Two ways to close that gap:

**Option A — Build real push into Wayfinder.** Webhook delivery on task-ready/assignee-changed, or an SSE stream. Correct long-term, but it's new Wayfinder-side infrastructure (delivery retries, signing, dead-lettering — the same class of problem TA's own webhook routes already solve once), and it makes the virtual team depend on Wayfinder's uptime for *every* new task, not just status sync.

**Option B — A thin polling adapter, on the TA/virtual-team side, that *feels* like push to the team.** The adapter polls Wayfinder's existing `/api/dispatch` + ready-queue endpoints (same bearer/service-account auth pattern `ta-plan-wayfinder` already uses) on a short interval. The moment it sees new or reassigned work, **it does not call the chief-of-staff persona directly — no RPC, no direct call, and as of Rev 3 (§4) not even an explicit named recipient.** It publishes an ordinary, *topic-tagged* message onto a durable stream built on top of `ta-agent-whiteboard`'s already-built transport primitives (JetStream-backed, durable, the same substrate `handoff.rs` already proves out — exactly built for "get this to the right peer even if they're not listening right now"); the chief-of-staff persona consumes it because it has separately **registered itself** as the consumer for that topic, not because the poller named it. From the team member's point of view further downstream, it *is* a push — they receive a handoff message, same as they would from a sibling agent today. Reporting back (completion, blockers, new work discovered) goes the other way through the same adapter, via plain REST calls to Wayfinder (`PATCH` task status, `POST` new tasks with `external_id` for idempotent upsert — that endpoint already exists per the wayfinder work merged this session).

Keeping the poller and the persona mutually unaware of each other — connected only by a topic both sides independently agree on, never a direct call or a hardcoded recipient — is the actual independence property this design is after: either side can be swapped, scaled, or duplicated without the other needing to change, and a topic can gain or lose its consumer without the publisher ever being touched. (An earlier pass of this document blurred that line, and a later pass still addressed messages by an explicit `requested_role` field; §4/§5/§8.5 below are now consistent with topic-based, registration-driven consumption throughout.)

**Recommendation: B.** It needs zero new Wayfinder-side infrastructure — Wayfinder stays exactly what it is today, a REST API with real auth and a real UI. It reuses two already-solid, already-tested TA primitives (`ta-plan-wayfinder`'s client/auth pattern, `ta-agent-whiteboard`'s handoff) instead of building a third delivery mechanism. And it fails safe: if the poller is down, nothing breaks except *new* task pickup — in-flight work and existing status keep working, unlike Option A where a Wayfinder outage would break live delivery. Revisit Option A only if polling latency (whatever interval you pick — seconds-to-low-minutes is the realistic range) turns out to be a real product problem, not before.

## 4. The "single orchestration role" — has to be invented, not found

You framed this as "the project manager or chief of staff or whatever the single orchestration role is" — red-teaming that: **it doesn't exist yet, on either side.** Wayfinder's `TeamRole` has no manager/gatekeeper concept; TA's `.ta/team.toml` is flat persona config. This needs to be designed, not wired up.

Split it into two layers, because they have different failure/judgment characteristics, and give the seam between them a real type instead of prose:

1. **The poller (Option B above)** — deterministic, no LLM, lives in the new private repo. Its only job: notice Wayfinder state changes, translate a Wayfinder `Task` into a handoff-message candidate, and translate handoff outcomes back into Wayfinder REST calls. This is the "simple" part — a REST client and a JetStream publisher, maybe a few hundred lines. It should not make judgment calls.

   The Wayfinder-client half and the whiteboard-publisher half of the poller should only communicate through one small internal type — a `TaskCandidate` struct, not the raw Wayfinder DTO passed straight through. This is an anti-corruption layer: if Wayfinder's `Task` shape changes, or the wire schema published onto the whiteboard changes (§8.5), only the construction of `TaskCandidate` moves, not the whole poller.

   **Addressing is by topic, not by naming a recipient — and there are two independent tag vocabularies, not one.** The poller does not know "chief-of-staff" as an identity, doesn't hardcode a `RoleRef`, and doesn't look anyone up before publishing. All Wayfinder-sourced missives — regardless of which of Wayfinder's own tags they carry — land on **one well-known external-intake stream**, and the chief-of-staff persona is the **sole registered consumer of it**. This is deliberate, not an oversight: it's the direct implementation of "the single orchestration role" from the original ask — every external missive passes through one gate before anything local happens, so there is exactly one place that ever has to reconcile "what is Wayfinder asking for right now," not N independently-addressed consumers each seeing a partial view.

   - **Wayfinder's tag vocabulary** (`task-delegation`, `research-request`, `status-report`, `escalation`, ... — a known, published set, owned by Wayfinder, same ownership rule as the priority enum in §6) travels *inside* the message payload as classification metadata, not as separate stream addresses. It tells the chief-of-staff persona what kind of external missive this is so it can triage appropriately, but it never determines "who receives this" — that's always the chief-of-staff, unconditionally, for anything Wayfinder-sourced.
   - **The project's own tag vocabulary** — locally defined per virtual team, in `.ta/team.toml`, mirroring the same `handles_verbs`-style convention Wayfinder's own `TeamRole` already uses — is what the chief-of-staff persona consults *after* triage, to decide which local team member actually does the work. This is an internal decision, not an external contract, so it reuses the *existing* `handoff.rs` RoleRef-addressed mechanism exactly as already built (point-to-point, no new primitive needed) — the chief-of-staff persona is simply another caller of `send_handoff`, addressing a specific `RoleRef` it has picked using its own judgment plus the project's local tag config.

     **Correction (Rev 5): this field doesn't exist yet, and it's TA-core work, not private-repo config.** `TeamMember` (`ta-session/src/team.rs`/`agent_action.rs`) has exactly four fields today — `role`, `agent_id`, `security`, `persona` — no tag list. Consulting a "local tag vocabulary" requires adding one, e.g. `handles_tags: Vec<String>` with `#[serde(default)]` for backward compatibility with existing `team.toml` files, plus whatever small resolution logic reads it. That's a small, additive `ta-session` change on a feature branch through TA's own PR workflow — not a config-only addition the private repo can make unilaterally. See §8, §9.2, §9.8.

   The intake side (Wayfinder tags → the single external-intake stream) is built entirely on `ta-agent-whiteboard`'s existing public `WhiteboardTransport` trait (`stream_append`/`stream_read_next`/`stream_ack` for the stream itself, `kv_put`/`kv_list` with TTL for the registration record — the exact pattern `presence.rs` already establishes for liveness) — **no change to `ta-agent-whiteboard` is needed**; a small `topics.rs` + `registration.rs` pair lives in the private repo, built purely as a consumer of the crate's already-public surface. Because there's exactly one intended consumer, registration here isn't doing multi-consumer routing — it exists so (a) the persona formally binds to the stream without the poller ever hardcoding its identity, and (b) it *would become* observable — "the external-intake stream has messages piling up and nobody is currently registered to drain it" (chief-of-staff crashed, misconfigured, or never started) — **if something is actually built to check stream depth against registration presence.** Nothing in this design's current scope specifies that checker or where its output surfaces; it's the same raise-without-a-check shape §6 already flags for the human-escalation flag. Scoped explicitly as a Phase 3 exit-criterion in §10 rather than left as an implicit claim (Rev 5 correction; renumbered to Phase 3 in Rev 6's four-stage restructure).

   **Future flexibility, not default architecture**: nothing about the wire format forces every Wayfinder tag through chief-of-staff forever. If a project later wants, say, `status-report` missives drained by a different, dedicated role without passing through chief-of-staff, that's a deliberate re-registration (a different `RoleRef` registers for that one Wayfinder tag specifically) — the tag is already on the wire, so splitting the intake later doesn't require touching the schema, only the registration config. Don't build that split up front; the single-gate design is the right default for a small team.

   The poller also owns its own **audit trail**: every Wayfinder write it makes gets correlated back to the `candidate_id`/persona decision that caused it (the `candidate`/`outcome` schema in §8.5 carries this by construction). With three hops — Wayfinder ↔ poller ↔ whiteboard ↔ persona — a wrong status update needs to be attributable to "poller bug" vs. "bad persona judgment" vs. "Wayfinder API behavior" without guesswork after the fact.

   `external_id` derivation for the "new work found" path has to be **stable across retries**, not freshly generated per attempt (e.g. `ta-goal:<goal_id>`, never a fresh UUID each time the persona re-emits the same logical outcome). Wayfinder's upsert is idempotent on this field; a wobbly id defeats that silently, with no error to notice — this is the same bug shape the upsert fix on the Wayfinder side was originally closing.

2. **The chief-of-staff persona** — a real `TeamRole` entry in `.ta/team.toml` (matching the existing convention), backed by an ordinary TA agent invocation (`ta run`, same machinery every other goal already uses — nothing new to build here). It receives the poller's candidates and does the part that actually needs judgment: which team member's remit this matches, whether it overlaps in-flight work, how to phrase the goal brief handed to whoever executes it. It emits its routing decision as a normal whiteboard handoff to the chosen team member.

   **Model tier policy is resolved, not left open; the schema to express it is not built yet.** The chief-of-staff persona should run on the **highest reasoning-effort model tier the user has selected** for the session/deployment — it's the one role in this design making judgment calls that affect routing, priority, and what becomes a new Wayfinder task, so it gets the top tier by default, not a fixed model name pinned in config. Every other team-member persona should run on the **best lower-cost model that fits that specific role's task class** — chosen per role (a docs-fix worker doesn't need the same tier as a design-judgment worker), not a single uniform "cheap tier for everyone" setting.

   **Correction (Rev 5): `.ta/team.toml`'s real schema has no field for this today.** `TeamMember` is exactly `{ role, agent_id, security, persona }` (confirmed by direct source read of `ta-session/src/team.rs`) — there is no `model_tier` key to fill in. Expressing the policy above concretely requires adding one (`model_tier: Option<String>` or a small enum, `#[serde(default)]`), plus tier→model resolution logic somewhere in the execution path. This is small, additive, backward-compatible — the same shape as `persona`'s own optional field — but it is a real `ta-session` change, not zero-code config-filling. Track it honestly as its own line (§8), the same discipline this doc already applies to the Wayfinder Notification primitive in §6.

This keeps the *mechanical* surface (the new component) small and boring by construction, while the *judgment* surface reuses infrastructure that already exists (goal execution) instead of inventing a new "orchestrator agent runtime."

## 5. Reconciling push-down and push-up into one flow

```
Wayfinder (task backlog, REST, no push)
    │  poll: ready-queue / dispatch
    ▼
Poller  ──────────────────────────────► Wayfinder Task → TaskCandidate (internal type)
    │                                    tagged with Wayfinder's classification vocabulary
    │                                    (task-delegation / research-request / status-report / ...)
    ▼  publish `candidate` message (§8.5 schema) — no direct call, no named recipient
External-intake stream (single, durable — topics.rs on top of WhiteboardTransport)
    │  drained by whoever is registered — by design, chief-of-staff, always
    ▼
Chief-of-staff persona (ta run, highest reasoning tier)
    │  triages using Wayfinder's tag, then routes using the PROJECT's own
    │  local tag vocabulary (.ta/team.toml, mirrors Wayfinder's handles_verbs)
    │  writes the goal brief
    ▼
Whiteboard handoff, RoleRef-addressed (existing handoff.rs, unchanged)
    │──► Team member (ordinary ta run goal, role-appropriate lower-cost tier)
    │                                              │
    │        ◄── completion / blocked / new-work ──┘  (handoff reply, peer→peer)
    ▼
Chief-of-staff persona reacts, publishes `outcome` message (§8.5 schema) back onto
the same external-intake channel (or a paired outcome stream — implementation detail):
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
| **Wayfinder dispatch poller** | new | **Private virtual-team repo** — thin REST client (dispatch/ready-queue poll, task PATCH/POST), publishes candidates onto the external-intake stream via the §8.5 schema, tagged with Wayfinder's classification vocabulary. No LLM, no judgment logic, no knowledge of who consumes the stream. Authenticates as a Wayfinder service account at **`member` role**, never `owner`. Owns its own poll-interval/backoff discipline — Wayfinder's rate limiter on these routes is structurally inert (§2), so nothing backstops the poller from the other side. |
| **`topics.rs` + `registration.rs`** | new, small | **Private virtual-team repo** — the external-intake stream and the (single-consumer-by-design) registration record, built entirely on `ta-agent-whiteboard`'s existing public `WhiteboardTransport` trait (§4). No `ta-agent-whiteboard` changes needed; a candidate to upstream later if a second use case wants the same pattern. |
| **Chief-of-staff persona** | new (config + prompt; *execution* is unchanged, existing `ta run`/goal machinery) | Private repo's `.ta/team.toml` + a goal brief template. Sole registered consumer of the external-intake stream; routes onward to team members via the existing `handoff.rs` (unchanged). Runs at the highest reasoning-effort tier once §8's `TeamMember` schema row lands; other personas at role-appropriate lower-cost tiers. |
| **`TeamMember` schema extension (`model_tier`, `handles_tags`)** | new, small — **corrected in Rev 5**: this is TA-core work, not private-repo config | **TA core (this repo), `ta-session/src/team.rs`/`agent_action.rs`** — `TeamMember` is exactly `{ role, agent_id, security, persona }` today (confirmed by direct source read); no field exists for model tier or a local tag vocabulary. Requires two small, additive, `#[serde(default)]` fields plus resolution logic, through TA's own feature-branch+PR workflow. Blocks §4's model-tier policy and the project-local tag vocabulary from being expressible at all until it lands — sequence it in Phase 0 (§10). |
| Human escalation glue (v1) | new, small | Poller adds a comment/status write on `ta_ask_human` events — a few dozen lines, not a subsystem. |
| **Wayfinder Notification primitive (v2)** | new, **costed Wayfinder-side work** — not free, not bundled into "poller + persona" | Wayfinder repo — new entity + endpoints (§8.5), surfaced in Wayfinder's own UI. Track as its own line, decide in §9.6 whether/when to build it. |
| Amplified Office dashboard | **not built** | Deferred per §7; revisit only on evidence. |

The new private repo's actual surface area, by this design, is genuinely narrow: one REST client against Wayfinder's existing API, wired to one existing durable transport (whiteboard), plus config for one new team role. It composes on top of two already-hardened TA-core primitives rather than reimplementing either — which is the "simple but not constrained" property you asked for: nothing here blocks the private repo from later adding more roles, more Wayfinder projects, or swapping the poll interval/transport, because none of that touches TA core.

### 8.5 The API surface — two independent contracts

Two separate contracts exist here and should stay independently documented, so the poller is a translator between two documented interfaces rather than the only thing that understands both systems' internals. Without this, the only spec for the TA-facing side would be "speak whiteboard's internal JetStream message format" — which means only a TA-native client could ever hand the chief-of-staff work, cutting against the "so Wayfinder (or anything else) can integrate, and vice versa" goal directly.

**Contract 1 — Wayfinder's outbound REST contract.** Already the right shape, needs no rework: REST + JSON, service-account bearer auth (`member` role), versioned by path. Inherently swappable — anything that speaks HTTP can integrate, not just this poller. Add the notification endpoints here if/when §6 v2 gets built:

```
POST   /api/projects/:project_id/dispatch                (existing — mutates, not GET)
GET    /api/projects/:project_id/ready-queue              (existing)
PATCH  /api/projects/:project_id/tasks/:id/status          (existing — no bare .../tasks/:id PATCH)
POST   /api/projects/:project_id/tasks                     (existing, external_id upsert)
POST   /api/projects/:project_id/notifications              (new, v2)
PATCH  /api/projects/:project_id/notifications/:id           (new, v2)
GET    /api/projects/:project_id/notifications                (new, v2)
```

(`.../tasks/:id/assignee` also exists as its own separate PATCH — not used by this design since `Task.assignee_id` is inert metadata per §2, noted here only so nobody assumes status and assignee share a route.)

**Contract 2 — the chief-of-staff handoff contract.** This is the one that doesn't exist yet as a contract, and needs to be defined independently of whiteboard-as-transport (whiteboard/JetStream is today's delivery mechanism for it — swappable later without either side changing, same anti-corruption reasoning as the poller's internal `TaskCandidate` type in §4, just moved to the actual system boundary):

```
candidate  { candidate_id,                       // stable, correlates outcome + audit trail
             source: "wayfinder",
             source_ref: { org_id, project_id, task_id },
             title, description,
             priority,                            // shared enum, owned/defined by Wayfinder (§6)
             tag: "task-delegation"                // Wayfinder's own published classification
                  | "research-request"             // vocabulary — informs triage, does NOT
                  | "status-report"                 // address a specific recipient (§4)
                  | "escalation" | ... }

outcome    { candidate_id,                        // correlates back to the candidate
             outcome: "done" | "blocked" | "new_work",
             wayfinder_task_id,                    // stable external_id when outcome = new_work
             detail }
```

Note there is no `requested_role`/recipient field at all — addressing is implicit: every `candidate` lands on the single external-intake stream regardless of `tag`, and whoever is registered to drain that stream (chief-of-staff, by design — §4) is the recipient. This also answers the "vice versa" half directly: anything else that wants to hand the chief-of-staff work — not just this Wayfinder poller — only needs to speak this schema and know the intake stream's well-known name, not know whiteboard's internals or the persona's identity.

`tag`'s vocabulary is **published and versioned by Wayfinder** (parity with `priority`, §6) — it's the classification Wayfinder itself assigns to what it's asking for, and it's what the chief-of-staff persona's own triage logic switches on. It is a **separate vocabulary from the project's local team-role tags** in `.ta/team.toml` (§4), which the chief-of-staff persona uses only for its own downstream `handoff.rs` routing decision and never appears on the wire to Wayfinder at all — the two vocabularies serve different audiences and must not be conflated into one enum.

## 9. Open decisions (yours, not mine to assume)

1. **Poll interval** — a few seconds feels "live enough" for a small team; every poll is a real API call against Wayfinder's (now-hardened, but not rate-limited on these routes — §2) auth path. Pick a default, make it configurable, and own the backoff discipline on the poller side since Wayfinder won't backstop it.
2. ~~Chief-of-staff / persona model tier~~ **Policy resolved, schema not yet built**: chief-of-staff should run at the highest reasoning-effort tier the user has selected; every other team-member persona should run at the best lower-cost tier that fits that role's specific task class. Requires the `TeamMember` schema extension (§4, §8) to land first — `.ta/team.toml` has no field for this today.
3. **Multi-Wayfinder-project scope** — one poller instance per Wayfinder org/project, or one poller fanning out across several? Affects the private repo's config shape from day one, cheap to decide now, expensive to retrofit. The inert-rate-limiter fact in §2 applies here too: fan-out design shouldn't assume Wayfinder will ever throttle it.
4. ~~Repo name~~ **Resolved**: `ta-virtual-team` (2026-08-26). Org/visibility (which GitHub org, private) still need confirming at repo-creation time — the name alone doesn't fully unblock Phase 0.
5. **Chief-of-staff concurrency model** — can persona invocations for different candidates run concurrently? If yes, the single-writer discipline in §5 needs to extend to that case explicitly (two concurrent invocations must never touch the same Wayfinder task or whiteboard thread) or the dual-writer problem this design otherwise avoids reappears.
6. **Whether/when to build the Wayfinder Notification primitive (§6 v2, §8.5 Contract 1 additions)** — real upgrade, but the first genuinely costed Wayfinder-side piece of this whole design. Ship v1 (comment + flag) first; decide on v2 with evidence from real use, not speculatively — same standard §7 already applies to the dashboard question.
7. **The exact `tag` vocabulary Wayfinder publishes, and where it's documented/versioned** — `task-delegation`/`research-request`/`status-report`/`escalation` are a plausible starting set (§4, §8.5), not a ratified list. Whoever owns Wayfinder's API contract should publish this the same way `priority` needs publishing — a small, explicit enum, not something the poller and the persona each guess at independently.
8. **The project-local team-role tag vocabulary in `.ta/team.toml`** is per-project by design (§4) — but the field itself doesn't exist yet (§4, §8: `TeamMember` is `{ role, agent_id, security, persona }` today). Confirm the schema/field name for `handles_tags` as part of the same `ta-session` change that adds `model_tier`, before the first private-repo team gets configured, so every project doesn't invent its own shape.

## 10. Development plan

**Four stages, in this order: split, test, finalize, then the Wayfinder module — and the Wayfinder module is optional, off by default, the whole way through.** Splitting the virtual team out and pairing it with Wayfinder are two different deliverables, not one continuous build. The virtual team has to work, and be worth using, entirely on its own — a human should be able to run one with zero Wayfinder configuration. Wayfinder-pairing is an add-on bolted on top once that base is proven, gated behind an explicit opt-in the same way `ta-agent-whiteboard` already gates coordination behind `[whiteboard] enabled = true` (default `false`, confirmed in `ta-agent-whiteboard/src/config.rs`) — this design should add a matching `[wayfinder] enabled = true` gate, default `false`, rather than assuming every virtual team wants Wayfinder wired in.

**Dependency mechanism** (settled, not open): the private repo depends on `ta-agent-whiteboard`, `ta-session`, `ta-goal`, etc. as **git dependencies** against `Trusted-Autonomy/TrustedAutonomy`, the same pattern TA's own `ta-mcp-gateway/Cargo.toml` already uses today for `ta-decision` (a *different* repo, `decision-gate`, consumed via `{ git = "...", tag = "v0.1.0" }`). This is a proven mechanism already in production in this exact codebase, not a new one to validate. **Pin to a released tag, not `main`** — TA core changes daily; tracking `main` would make the private repo's build break on unrelated TA-core work. Bump the pin deliberately, the same way any dependency upgrade would be reviewed.

**Tooling parity**: carry over TA's own conventions into the new repo's `CLAUDE.md` — feature-branch + PR workflow, the four-gate verify (`build`/`test`/`clippy -D warnings`/`fmt --check`), Nix-provided toolchain. No reason to invent a second convention for a repo that's going to depend on TA core directly.

**Existing engine, not a blank slate**: TA core's multi-role executor (`ta-daemon/src/team_session.rs` — fires one `ta run` goal per role, sequentially) already carries ~20 unit tests today (signal handling, session-context building, `run_one_cycle`). Stage 2 below is exercising that engine as a real product end to end for the first time, not proving it from zero.

---

### Stage 1 — Split

**Phase 0 — Repo scaffold + `TeamMember` schema PR**
- **TA-core sub-step, on its own feature branch + PR against `Trusted-Autonomy/TrustedAutonomy`, done first**: extend `TeamMember` (`ta-session/src/team.rs`/`agent_action.rs`) with two small, additive, `#[serde(default)]` fields — `model_tier` and `handles_tags` — plus whatever minimal resolution logic reads them (§4, §8, §9.2, §9.8). This benefits the virtual team on its own merits (§4's model-tier policy is not Wayfinder-specific), so it belongs here, not deferred to Stage 4. Land and tag-release it before the private repo's `.ta/team.toml` can express either field.
- Create the private repo — named `ta-virtual-team` (§9.4); org/visibility still to confirm at creation time. Wire the git-dependency pin described above, against the tag that includes the `TeamMember` change.
- **Exit criteria**: the repo exists, builds against the pinned TA-core tag, and `.ta/team.toml` accepts `model_tier`/`handles_tags` without error.

### Stage 2 — Test virtual teams, standalone (no Wayfinder anywhere)

**Phase 1 — Real roster, real work**
- Replace any placeholder roster with a real one: chief-of-staff plus however many worker roles the first real use case needs, each with an appropriate `model_tier` and `security` level.
- Run real goals (not toys) through `team_session.rs`'s existing sequential execution. Confirm `model_tier` actually changes which model runs a given role — this is the first real exercise of the field added in Phase 0.
- Exercise the whiteboard substrate for real: presence while two roles are concurrently active, a `handoff.rs` message between two team members outside of any Wayfinder context, `ta_ask_human`/`ta_human_verify` firing from inside a team-run goal and getting answered.
- Run the full draft review → apply cycle on team-produced output, end to end.
- **Exit criteria**: a human can point TA at this repo, run a real multi-role goal, review its draft, and apply it — with no Wayfinder involvement anywhere. This is the actual bar for "the split succeeded," not the toy smoke test alone.

### Stage 3 — Finalize base integration

**Phase 2 — Productionize**
- CI on the new repo mirroring TA's own four-gate verify.
- Prove the git-dependency pin/bump workflow at least once for real (bump the pin to a newer TA-core tag, confirm nothing silently breaks).
- Docs: a README/USAGE-equivalent, example `team.toml` configs, the repo's own versioning scheme.
- **Exit criteria**: the repo is in a state you'd hand to another person to use, still with zero Wayfinder configuration anywhere.

### Stage 4 — Optional Wayfinder-pairing module (gated behind `[wayfinder] enabled = true`)

Everything below is additive and off by default — a team that never sets `[wayfinder] enabled = true` never runs any of it.

**Phase 3 — Topic + registration primitive**
- Build `topics.rs` + `registration.rs` on top of `ta-agent-whiteboard`'s existing public `WhiteboardTransport` trait (§4, §8) — the external-intake stream and its single-consumer-by-design registration record. No `ta-agent-whiteboard` changes required (confirmed by direct source read — its KV/stream primitives are already public).
- Test against `InMemoryTransport` (already shipped, no NATS server needed for this phase) — publish a message, register a fake consumer, confirm it drains; confirm an unregistered topic still lands durably.
- **Scope the stream-depth-vs-registration check here, explicitly** (§4 Rev 5 correction): a small function comparing pending-message count on the intake stream against whether a live (non-expired) registration exists, logged as a warning when they diverge. Doesn't need a UI yet — a `tracing::warn!` is enough to make the earlier "detectable condition" claim actually true rather than aspirational.
- **Exit criteria**: a message published before any consumer registers is still delivered once one does; registration liveness (§4) is queryable; the depth-vs-registration check fires in a test that stops registering and keeps publishing.

**Phase 4 — Wayfinder REST client + poller, read-only**
- Thin client for `POST /api/dispatch`, `GET` ready-queue, `PATCH .../tasks/:id/status`, `POST /api/tasks` (§8.5 Contract 1 — verbs corrected Rev 5), authenticating as a `member`-role service account (§2).
- Define the `candidate`/`outcome` schema (§8.5) now, even though nothing publishes yet.
- Poller runs read-only: logs `TaskCandidate`s (the internal anti-corruption type, §4) tagged with Wayfinder's classification vocabulary, publishes nothing. Only runs at all when `[wayfinder] enabled = true`.
- **Exit criteria**: verified against a real (or sandboxed) Wayfinder project — candidates are logged correctly, tagged correctly, with no writes anywhere yet.

**Phase 5 — Wire intake end to end**
- Poller publishes `candidate` onto the external-intake stream (Phase 3's primitive). Chief-of-staff persona (highest reasoning-effort tier, §4) is the registered consumer; it triages by Wayfinder's tag, then routes via the *existing, unmodified* `handoff.rs` to a team member, using the project's own local tag vocabulary (§9.8). Team member executes at its own role-appropriate lower-cost tier.
- Still no Wayfinder writes. Nail down stable `external_id` derivation now (§4) — `ta-goal:<goal_id>`, never freshly generated — before anything downstream depends on upsert idempotency.
- **Exit criteria**: a real Wayfinder task flows all the way to a team member executing a goal, purely through internal TA/whiteboard plumbing.

**Phase 6 — Report-back leg**
- `outcome` messages drive `PATCH` task status / `POST` new tasks, behind an explicit dry-run flag until trusted. Add the poller's audit trail (§4) here, not after — every write should already be correlatable to its originating `candidate_id`.
- **Exit criteria**: dry-run output reviewed and judged correct on real Wayfinder tasks before the flag is removed.

**Phase 7 — Human escalation, v1**
- Comment + status flag glue (§6 v1) — raise and clear shipped together, not the raise half alone.
- **Exit criteria**: a `ta_ask_human` interaction on Wayfinder-sourced work is visible and answerable from Wayfinder's own UI via the linked flag, and clears itself once answered.

**Phase 8 — Cross-links** (§7)
- Cheap, cosmetic relative to the rest; safe to defer past Phase 7 without blocking anything downstream.

**Phase 9 — Wayfinder Notification primitive, v2** (§6, §9.6) — **evidence-gated, not scheduled**
- Only after real use of Phases 3-8 shows v1's comment-flag approach is actually insufficient. This is the one phase with real Wayfinder-side cost (§8); don't pull it forward speculatively.

---

Each phase is independently testable before the next one starts writing (Stage 4's phases against a real or sandboxed Wayfinder project; Stage 1-3's against the private repo alone). Once Phase 0 lands, **migrate this phase list into the new repo's own `PLAN.md`**, using TA's existing phase-tracking convention (`<!-- status: pending/in_progress/done -->`, `ta plan status`) — this document's job was to get the repo to a point where TA's own planning tooling can take over; it isn't meant to be the plan of record forever.

---

**Changelog**: Rev 2 (2026-08-25) folds in a Wayfinder-side review of Rev 1, grounded in that session's actual work on Wayfinder's auth and rate-limiting. Changes: fixed a §3/§4/§5 flow inconsistency (poller never calls the persona directly — whiteboard-message-only, now stated explicitly and consistently); added the `TaskCandidate` internal type, stable `external_id` derivation, poller audit trail, and the chief-of-staff concurrency question (§4, §9.5); added two facts grounded in Wayfinder's current source — the structurally inert rate limiter on `/api/projects/*` dispatch/ready-queue routes, and `member` as the correct (not `owner`) service-account tier (§2, §8); expanded §6 into an explicit v1 (free) / v2 (costed Wayfinder Notification primitive) split with a shared-priority-enum rule and a raise/clear lifecycle requirement; added §8.5 defining two independent contracts — Wayfinder's REST DTOs and a versioned `candidate`/`outcome` handoff schema decoupled from whiteboard-as-transport; and resolved the model-tier open decision (chief-of-staff = highest reasoning tier the user selects; other personas = best lower-cost tier fitting each role).

**Rev 3** (2026-08-25, same day) replaces explicit recipient-addressing with topic-based, registration-driven consumption (§3, §4, §5, §8, §8.5): the poller no longer names "chief-of-staff" anywhere — it tags each candidate with Wayfinder's own published classification vocabulary (`task-delegation`/`research-request`/`status-report`/`escalation`, ...) and appends it to a single external-intake stream; the chief-of-staff persona binds to that stream by registering itself, the poller never looks the registration up. Confirmed as a deliberate single-gate design, not one-topic-per-consumer: every Wayfinder-sourced missive, regardless of its Wayfinder tag, goes to the same sole registered consumer (chief-of-staff) — preserving "one orchestration role" from the original ask rather than fragmenting external intake across multiple independently-addressed roles. A second, separate tag vocabulary — defined per project in `.ta/team.toml`, mirroring Wayfinder's own `handles_verbs` convention — is what the chief-of-staff persona then uses for its own downstream routing to a specific team member, over the existing unmodified `handoff.rs` RoleRef mechanism; this vocabulary is local and never appears on the wire to Wayfinder. Both the topic stream and the registration record are built entirely on `ta-agent-whiteboard`'s already-public `WhiteboardTransport` trait — no changes to that crate are needed, though the pattern is a plausible future upstream candidate if a second use case wants it. New open decisions added (§9.7, §9.8): who publishes/versions Wayfinder's tag vocabulary, and confirming the `.ta/team.toml` local-tag schema before the first private-repo team is configured.

**Rev 4** (2026-08-25, same day) turns §10 from a flat build-order list into a phased development plan and directly answers "before or after the virtual-team split": before, but as a fast Phase 0 inside this one plan rather than a separate initiative to finish first, since TA core already ships the multi-role execution engine (`team_session.rs`) and the whiteboard substrate the split mostly just needs to be pointed at. Adds a settled dependency mechanism (git dependency against `Trusted-Autonomy/TrustedAutonomy`, pinned to a released tag — the same pattern already proven by `ta-decision`/`decision-gate` in TA's own `ta-mcp-gateway/Cargo.toml`) and tooling-parity guidance (carry over CLAUDE.md conventions, four-gate verify). Restructures the prior 8-step list into 8 named phases (0-7) with explicit exit criteria each, and closes with instructions to migrate this plan into the new repo's own `PLAN.md` via TA's existing phase-tracking convention once Phase 0 lands, rather than treating this document as the permanent plan of record.

**Rev 5** (2026-08-25, same day) corrects three issues surfaced by an independent Wayfinder-side agent review, verified directly against both codebases (`wayfinder-api/src/routes/{dispatch,tasks,ready_queue}.rs`, `project_router.rs`, `ta-session/src/team.rs`) rather than taken on trust — the review's own methodology (checking concrete claims against source, not the doc's prose) is repeated here, and its whiteboard-primitive and `EscalatePrimitive` findings were confirmed accurate and left unchanged. **Two real route errors in §8.5 Contract 1**: `dispatch` is `post(dispatch)`, not GET — it mutates and would 405 as GET; there is no bare `.../tasks/:id` PATCH, only `.../tasks/:id/status` (task status) and the separate `.../tasks/:id/assignee` — both fixed in the table, with the external `/api/projects/:project_id/...` prefix itself confirmed correct via `project_router.rs`'s `dispatch_to_project` rewrite (§2). **One systematic mis-costing**: `model_tier` and the project-local tag vocabulary were written as if `.ta/team.toml` already had fields for them; `TeamMember` is actually exactly `{ role, agent_id, security, persona }` (confirmed by direct source read of `ta-session/src/team.rs`) — realizing either requires a small, additive, backward-compatible `ta-session` schema change (new §8 row, new Phase 0 sub-step in §10), not free config, correcting §8's prior "no new agent-execution code needed" framing for the chief-of-staff persona specifically (the persona's *execution* path is still unchanged; its *config schema* needs a small TA-core PR first). **One softened claim**: §4's assertion that an unregistered intake stream is "a real, detectable condition" is corrected to note nothing in scope actually builds that check yet — scoped explicitly as a Phase 1 exit criterion in §10 instead of stated as already true.

**Rev 6** (2026-08-25, same day) restructures §10 into four explicit stages — Split, Test (standalone, no Wayfinder), Finalize base integration, then the Wayfinder-pairing module — answering a direct sequencing question and correcting an implicit assumption the prior revs never stated outright: that the Wayfinder module is **optional and off by default**, not something baked into the core virtual-team repo. Adds a `[wayfinder] enabled = true` config gate, mirroring `ta-agent-whiteboard`'s own `[whiteboard] enabled = true` (default `false`, confirmed in `ta-agent-whiteboard/src/config.rs`) — a team should work fully with zero Wayfinder configuration. Phases renumbered 0-9 across the four stages (0: repo + `TeamMember` schema PR; 1: real roster + real goals through `team_session.rs`, no Wayfinder anywhere, exit criterion is draft-review-and-apply working end to end standalone; 2: CI/docs/dependency-bump-workflow productionization; 3-9: the prior Wayfinder-module phases, unchanged in content, gated behind the new config flag). Notes that TA core's multi-role executor (`ta-daemon/src/team_session.rs`) already carries ~20 unit tests today — Stage 2 is the engine's first real end-to-end product exercise, not proving it from zero.
