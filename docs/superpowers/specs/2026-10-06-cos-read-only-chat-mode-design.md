# Chief-of-Staff: Read-Only Chat-Mode Role Design

**Status:** ratified by the user and by Wayfinder-side review (`agentic-pm-ba`). Implementation dispatched: TA-core-side items are `agentic-pm-ba`'s and this session's follow-up work; `ta-virtual-team`/`wayfinder-api`-side items (persona rename and scope, new task-update endpoint) are dispatched to `agentic-pm-ba`, out of this session's own working-directory scope per its own CLAUDE.md ("all work stays within `~/development/TrustedAutonomy/`").

## Context

This session shipped the building blocks for a lightweight, policy-enforced chat session (`ta-ask`'s classifier, `ta-policy::chat_manifest`, `GatewayState::start_chat_session()`, PRs #632-635, all merged). None of it is wired into `ta-virtual-team` yet. Before wiring it in, the user raised a sharper question than "restrict CoS's tools for the chat path": should Chief-of-Staff (CoS) ever hold a tool capable of mutating anything, in any mode?

This doc answers that question and resolves two concrete follow-on gaps found while answering it (wiki updates, task mutation).

## The role model

CoS's actual job, per the user: answer questions, triage incoming work (chat, forum feedback, meeting notes), classify which agent should handle a task and dispatch it, and review other agents' results for accuracy. CoS is never itself the target of a `ta_goal_start`: it never writes code, wiki content, or task state directly.

**Resolution: CoS is always read-only chat mode. It holds no tool, in any mode, that mutates anything.** Every real-world effect CoS's work produces, whether a code change, a wiki edit, or a task status change, happens as a hardened `ta run` goal dispatched to the correct worker persona, auto-verified and (where appropriate) auto-approved, never as a tool CoS calls directly.

This single rule replaces what would otherwise be a growing list of "CoS can do X but not Y" exceptions, and gives the system exactly one hardened ingress point for untrusted external content (chat, forum posts, meeting notes all funnel through CoS), rather than N ingestion paths each needing its own review.

### CoS's real toolset

- Read: `ta_fs_read`, `ta_fs_diff`, wiki read (`ta_wiki_search`/`ta_wiki_get`), whiteboard read/presence.
- Reply: chat response back to the requester.
- Classify-and-dispatch: decide what task(s) a chat/forum/meeting-note input implies (create new, update existing, assignment, status), then trigger a hardened `ta run` goal to realize that decision (see "Task mutation" below): this is a decision, not a tool that mutates state.
- Request a follow-up goal when review finds a gap: if CoS reviews a completed goal's draft and finds the work incomplete or wrong, it requests a continuation using TA's existing `ta run --follow-up-goal <id>` mechanism (`apps/ta-cli/src/commands/run.rs:469`, confirmed real, already used to resume an existing goal's own staging context rather than starting a fresh one, per the comment at `run.rs:2103`: "rather than building a second one"). The follow-up runs in the *same* worktree/staging space as the original work, not a disconnected new task, so it continues from exactly where the gap was found.
- Nothing else. No `ta_fs_write`, no git, no email, no `ta_wiki_create`/`ta_wiki_update`, no apply/approve-capable tool, no task-mutation tool (see below: none exists for any agent yet, and CoS specifically should never get one).

## Two real gaps found while resolving this, and how they resolve

### Wiki updates

`ta_wiki_create`/`ta_wiki_update` (`crates/ta-mcp-gateway/src/tools/wiki.rs`) are a thin client (`WikiMcpClient`) proxying directly to Wayfinder's own remote wiki MCP endpoint: an immediate write to Wayfinder's backend, not TA-local content, and **not policy-gated on the TA side at all** (only `require_declared_scope`, a config check, not a capability-manifest check).

Since there is no TA-side gate to lean on regardless of caller, and wiki content isn't TA-local (no git history to delegate into), the only available safeguard is tool-surface exclusion: **CoS never holds `ta_wiki_create`/`ta_wiki_update`.** A wiki edit gets classified and dispatched as a hardened `ta run` goal to the Curator persona (see "Persona naming and scope" below), which holds those tools. The safeguard is Wayfinder's own revision history on its side (its `if_sha` optimistic-concurrency parameter implies it already has one), not a new TA-side mechanism.

### Task mutation (create, update, assign, mark complete)

Confirmed directly against `ta-virtual-team`'s real code (via `agentic-pm-ba`): `WayfinderClient`'s `create_task`/`update_task_status` (real REST calls: `POST /api/dispatch`, `GET /api/tasks/:id`, `PATCH /api/tasks/:id/status`, `POST /api/tasks`) are called exclusively from `poller.rs`'s own deterministic outcome-reporting cycle, after a dispatched goal finishes (done/blocked/new-work). No agent, CoS or worker, has ever had a live tool to mutate a task directly. This is a clean gap, not a retrofit.

**Correction from an earlier draft of this doc:** it first treated "a fresh message becoming a task" as needing no new mechanism, reasoning that today's existing unconditional per-message `TaskCandidate` creation (`poller.rs`) already covers it and CoS just needs to gate whether that pipeline proceeds. That's wrong: today's mechanism is a dumb 1:1 pass-through with no judgment in it, and CoS's actual triage job, deciding how many tasks a input implies, whether it updates an existing task instead of creating a new one, who it's assigned to, is real judgment that the existing mechanism doesn't express at all. CoS (or a delegated agent such as librarian) genuinely needs to create and update tasks based on chat, triage, forum feedback, and meeting notes, not just gate an existing dumb pipeline.

**Resolution: there is exactly one mechanism for creating or updating a task, and it covers every trigger.** Whether the trigger is CoS/Curator triaging fresh input (chat, forum, meeting notes) or a worker completing real delegated work, the actual Wayfinder mutation happens the same way: CoS (or Curator) makes the judgment call of what should exist or change (a read-only decision: how many tasks, create-vs-update, assignment, status), then that decision is realized by dispatching a hardened `ta run` goal, often trivial and fast when the "work" is just recording an intake decision.

**Bundled, not separated, per explicit user direction.** A worker's task-update and wiki-update proposals are part of its own goal's deliverable, drafted and reviewed alongside the code/content change, in the same draft, not reported automatically after the fact through a side-channel the reviewer never sees. Concretely: a worker proposes its intended task-status change and wiki-content change as part of its own staged output, the human (or auto-approve rule) reviews the whole thing together, code and bookkeeping as one unit, and only on approval does the real outcome-reporting mechanism below fire, carrying the *approved* proposal, not a bare completion code. This means review actually covers what happens to task/wiki state, not just the code diff, closing a real gap where bookkeeping could otherwise drift from what was actually reviewed. No agent, CoS, Curator, or any worker, ever holds a live tool that mutates a task directly: the mutation is always the consequence of a reviewed goal's approved outcome, matching today's existing risk shape instead of adding a new one, for every trigger that creates or changes a task, not only the ones that happen to follow a larger piece of real work.

**"Needs-revision" and "on-hold" are semantically distinct outcomes, not interchangeable, and must stay that way for anyone reading a task's status:**

- **Needs-revision**: the delivered work is incorrect and must be redone. The task re-enters the active work queue immediately. Maps to Wayfinder's real `TaskStatus::Open` (confirmed by `agentic-pm-ba`: the real enum is `Open/InProgress/Done/Cancelled/OnHold`), not `OnHold`, since the task isn't blocked, it's simply back in the queue for another pass.
- **On-hold**: the task itself is blocked by a business reason or an internal dependency, independent of whether the work done so far is correct. Maps to `OnHold` + the existing `hold_reason` string, and ideally the blocking reason becomes a real precursor task with a dependency edge in Wayfinder's task graph (confirmed real dependency routes exist in `tasks.rs`), so the hold is structural, not just a status label, and the task auto-resumes (or is at least visibly unblockable) once its dependency closes.
- **Reassign**: confirmed, `PATCH /api/tasks/:id/assignee` already exists as a real Wayfinder endpoint; the outcome-reporting extension needs a new `WayfinderClient` method wrapping it, not new Wayfinder API.
- **Content update** (revising a task's title/description based on new chat/triage input, not just its status or owner): **confirmed as genuinely needed, and no endpoint exists for it today.** This is new Wayfinder API surface, dispatched to `agentic-pm-ba` below.

Net effect: **zero new agent-facing mutating tools, for wiki or for any task operation, including creation.** CoS (and Curator, and every other persona) stays genuinely zero-tool for mutation in every case; only the outcome-reporting surface itself (service-account-authed, not agent-invoked) grows.

**Non-negotiable: the result has to land in Wayfinder's own Task API, not just in TA-local bookkeeping.** `ta-plan-wayfinder` already wraps a local `FilePlanStore` as the structural source of truth for PLAN.md phases, with Wayfinder as a synced status mirror; it's plausible the outcome-reporting extension reuses that existing sync path rather than calling `WayfinderClient` directly. Whether a goal's outcome reaches Wayfinder via a PLAN.md-phase-triggered sync or a direct call is an implementation choice for the `ta-virtual-team`/Wayfinder-side scoping pass, not resolved here, but landing in Wayfinder itself is the requirement either way.

### Persona naming and scope

`agentic-pm-ba` found the real `librarian.toml` persona in `ta-virtual-team` (PR #17, merged) and two real problems with reusing it as-is:

1. Its own system prompt describes its job as maintenance (tagging stale/duplicate wiki pages `needs-review`) and deep-research escalation, explicitly stating new content is "reviewed and written by chief-of-staff." That directly contradicts this design's "CoS never writes" principle, so someone's scope has to change.
2. Independent of this design: `allowed_tools = []` in the real file today. It doesn't actually hold `ta_wiki_update` despite its prompt narratively describing using it. Pre-existing bug, not introduced by this doc.

**Resolution: expand this persona's scope to include content authoring (drafting new wiki content from source material, not only maintenance), rename it to Curator, rewrite its system prompt to drop the "written by chief-of-staff" line, and fix `allowed_tools` to actually include `ta_wiki_search`/`ta_wiki_get`/`ta_wiki_update`.** "Curator" is the chosen name because it's a modern term that covers all three real responsibilities, retrieving/organizing existing knowledge, maintaining it, and authoring/synthesizing new content, where librarian/archivist/historian all skew toward passive preservation and undersell the authoring half of the job. This reuses an existing persona rather than adding a second one for no structural benefit.

Also flagged by `agentic-pm-ba`, not resolved here: TA's `handles_tags` (`team.toml`) and Wayfinder's own `team_role_verb`/`handles_verbs` (a separate SQL-backed routing table in `wayfinder-orchestration`) look like two independent role-routing mechanisms that need reconciling for "task type maps to an allowed persona set" to actually work end-to-end. Dispatched to `agentic-pm-ba` for scoping.

## Delegation safety: the risks that remain even with zero write tools

CoS holding no mutating tool does not make delegation itself risk-free, since CoS is the single component most exposed to untrusted external input (chat, forum posts, meeting notes). Two concrete safeguards:

1. **Constrain routing by task type, not open agent selection.** CoS classifies a task's type and that classification maps to an allowed set of worker personas (a wiki-edit task can only route to the Curator persona, a code task only to a developer-class persona), never an open "pick any agent" choice. This bounds what a prompt-injected routing decision could reach: at worst, a wiki-edit-shaped task gets routed to Curator, not an arbitrarily privileged one.
2. **No auto-approve shortcut for CoS-originated work.** A goal dispatched because CoS classified and routed it gets the same (or stricter) review posture as any other goal. CoS being the most exposed-to-untrusted-input component is exactly why its delegated work must not get a "trusted because CoS asked" fast lane.

## Confirmed clean on Wayfinder-side review

`agentic-pm-ba` checked the "all outbound interfaces thread through CoS except analytics" carve-out against the real `kpi_scores.rs` routes: every KPI route is GET-only, a pure reporting surface with no mutation path. Correctly excluded; nothing there to gate in the first place.

## Dispatched for implementation (outside this session's own working-directory scope)

Sent to `agentic-pm-ba` for `ta-virtual-team`/`wayfinder-api`-side implementation:

1. Rename and expand the `librarian` persona to `Curator`: scope grows to include content authoring, system prompt rewritten to drop the "written by chief-of-staff" line, `allowed_tools` fixed to actually include `ta_wiki_search`/`ta_wiki_get`/`ta_wiki_update`.
2. New Wayfinder API endpoint for task content updates (title/description), since none exists today.
3. A new `WayfinderClient` method wrapping the already-existing `PATCH /api/tasks/:id/assignee` endpoint, for the reassign outcome type.
4. The outcome-reporting extension itself: new outcome types (`create`, `update` via the new content-update endpoint, `reassign`, `needs-revision` mapping to `Open`, `on-hold` mapping to `OnHold` + `hold_reason` + a dependency-task-creation pattern, `complete-by-review`), their service-account auth, and whether it reuses `ta-plan-wayfinder`'s existing PLAN.md-to-Wayfinder sync or calls `WayfinderClient` directly. Landing in Wayfinder's own Task API is the requirement regardless of which path is chosen. **Per the user's explicit direction, this mechanism fires from a worker's own approved goal draft (its task/wiki proposal bundled into the same review as its code/content work), not from a bare automated completion code separate from what got reviewed.** The draft-build/draft-apply pipeline needs to recognize and carry these proposed-update artifacts alongside the usual diff, and apply them as part of the same approval.
5. Reconciling TA's `handles_tags` (`team.toml`) with Wayfinder's own `team_role_verb`/`handles_verbs` routing table so "task type maps to an allowed persona set" works end-to-end.

## Open items remaining after dispatch

- `security-hypotheses.md` should gain new entries once this ships: CoS's declared toolset contains no mutating tool (a manifest/tool-availability check, not just a `PolicyEngine::evaluate()` check, since several of the tools involved, wiki, whiteboard, the new outcome-reporting surface, aren't policy-gated at all); a prompt-injected routing decision cannot reach an agent persona outside the task type's allowed set; no goal receives an auto-approve shortcut solely because CoS originated it.

## Self-review

- Placeholder scan: none. Every resolved question states the concrete mechanism; every dispatched item names exactly what's needed and who owns building it.
- Internal consistency: the "zero mutating tools, ever" rule for CoS and the "no new agent-facing mutation tool for anyone" resolution for tasks are consistent. Neither CoS nor a worker persona gets a live task-mutation tool; mutation is always goal-outcome-driven, and now explicitly a *reviewed* outcome, since task/wiki proposals are bundled into the worker's own draft rather than reported separately. "Needs-revision" and "on-hold" are kept as distinct Wayfinder states (`Open` vs `OnHold`), not collapsed. CoS's review-gap path reuses TA's existing `--follow-up-goal` mechanism rather than inventing a new one.
- Scope: this doc defines the role model and resolves the wiki/task gaps found this session, verified against real code on both the TA-core side and, via `agentic-pm-ba`, the `ta-virtual-team`/Wayfinder side. The exact outcome-reporting API shape and the Curator persona's final configuration are implementation work dispatched to `agentic-pm-ba`, not specified here.
