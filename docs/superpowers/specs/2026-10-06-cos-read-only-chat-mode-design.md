# Chief-of-Staff: Read-Only Chat-Mode Role Design

**Status:** drafted for user review, then Wayfinder-side review (`agentic-pm-ba`), before implementation. Not yet ratified.

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
- Nothing else. No `ta_fs_write`, no git, no email, no `ta_wiki_create`/`ta_wiki_update`, no apply/approve-capable tool, no task-mutation tool (see below: none exists for any agent yet, and CoS specifically should never get one).

## Two real gaps found while resolving this, and how they resolve

### Wiki updates

`ta_wiki_create`/`ta_wiki_update` (`crates/ta-mcp-gateway/src/tools/wiki.rs`) are a thin client (`WikiMcpClient`) proxying directly to Wayfinder's own remote wiki MCP endpoint: an immediate write to Wayfinder's backend, not TA-local content, and **not policy-gated on the TA side at all** (only `require_declared_scope`, a config check, not a capability-manifest check).

Since there is no TA-side gate to lean on regardless of caller, and wiki content isn't TA-local (no git history to delegate into), the only available safeguard is tool-surface exclusion: **CoS never holds `ta_wiki_create`/`ta_wiki_update`.** A wiki edit gets classified and dispatched as a hardened `ta run` goal to a librarian-class persona, which holds those tools. The safeguard is Wayfinder's own revision history on its side (its `if_sha` optimistic-concurrency parameter implies it already has one), not a new TA-side mechanism.

### Task mutation (create, update, assign, mark complete)

Confirmed directly against `ta-virtual-team`'s real code (via `agentic-pm-ba`): `WayfinderClient`'s `create_task`/`update_task_status` (real REST calls: `POST /api/dispatch`, `GET /api/tasks/:id`, `PATCH /api/tasks/:id/status`, `POST /api/tasks`) are called exclusively from `poller.rs`'s own deterministic outcome-reporting cycle, after a dispatched goal finishes (done/blocked/new-work). No agent, CoS or worker, has ever had a live tool to mutate a task directly. This is a clean gap, not a retrofit.

**Correction from an earlier draft of this doc:** it first treated "a fresh message becoming a task" as needing no new mechanism, reasoning that today's existing unconditional per-message `TaskCandidate` creation (`poller.rs`) already covers it and CoS just needs to gate whether that pipeline proceeds. That's wrong: today's mechanism is a dumb 1:1 pass-through with no judgment in it, and CoS's actual triage job, deciding how many tasks a input implies, whether it updates an existing task instead of creating a new one, who it's assigned to, is real judgment that the existing mechanism doesn't express at all. CoS (or a delegated agent such as librarian) genuinely needs to create and update tasks based on chat, triage, forum feedback, and meeting notes, not just gate an existing dumb pipeline.

**Resolution: there is exactly one mechanism for creating or updating a task, and it covers every trigger.** Whether the trigger is CoS/librarian triaging fresh input (chat, forum, meeting notes) or a worker goal reporting its own completion outcome, the actual Wayfinder mutation happens the same way: CoS (or librarian) makes the judgment call of what should exist or change (a read-only decision: how many tasks, create-vs-update, assignment, status), then that decision is realized by dispatching a hardened, auto-verified-then-auto-approved `ta run` goal, often trivial and fast when the "work" is just recording an intake decision, whose *reported outcome* performs the actual create/update/assign/complete against Wayfinder, through an extension of the existing deterministic, service-account-authed outcome-reporting mechanism (today's done/blocked/new-work, extended to cover `create`, `update`, `reassign`, `needs-revision`, `complete-by-review`, and whatever else the triage/review job needs). No agent, CoS, librarian, or any worker, ever holds a live tool that mutates a task directly. The mutation is always the post-hoc consequence of a reviewed goal's outcome, matching today's existing risk shape instead of adding a new one, for every trigger that creates or changes a task, not only the ones that happen to follow a larger piece of real work.

Net effect: **zero new agent-facing mutating tools, for wiki or for any task operation, including creation.** CoS (and librarian, and every other persona) stays genuinely zero-tool for mutation in every case; only the outcome-reporting surface itself (service-account-authed, not agent-invoked) grows.

**Non-negotiable: the result has to land in Wayfinder's own Task API, not just in TA-local bookkeeping.** `ta-plan-wayfinder` already wraps a local `FilePlanStore` as the structural source of truth for PLAN.md phases, with Wayfinder as a synced status mirror; it's plausible the outcome-reporting extension reuses that existing sync path rather than calling `WayfinderClient` directly. Whether a goal's outcome reaches Wayfinder via a PLAN.md-phase-triggered sync or a direct call is an implementation choice for the `ta-virtual-team`/Wayfinder-side scoping pass, not resolved here, but landing in Wayfinder itself is the requirement either way.

## Delegation safety: the risks that remain even with zero write tools

CoS holding no mutating tool does not make delegation itself risk-free, since CoS is the single component most exposed to untrusted external input (chat, forum posts, meeting notes). Two concrete safeguards:

1. **Constrain routing by task type, not open agent selection.** CoS classifies a task's type and that classification maps to an allowed set of worker personas (a wiki-edit task can only route to a librarian-class persona, a code task only to a developer-class persona), never an open "pick any agent" choice. This bounds what a prompt-injected routing decision could reach: at worst, a wiki-edit-shaped task gets routed to a librarian persona, not an arbitrarily privileged one.
2. **No auto-approve shortcut for CoS-originated work.** A goal dispatched because CoS classified and routed it gets the same (or stricter) review posture as any other goal. CoS being the most exposed-to-untrusted-input component is exactly why its delegated work must not get a "trusted because CoS asked" fast lane.

## Open items before this is implementable

- Exact shape of the outcome-reporting extension (new outcome types, their service-account auth, how a goal's "mark complete" intent gets threaded through to its outcome report, and whether it reuses `ta-plan-wayfinder`'s existing PLAN.md-to-Wayfinder sync or calls `WayfinderClient` directly) is `ta-virtual-team`/Wayfinder-side work, to be scoped with `agentic-pm-ba` once this doc is approved. Landing in Wayfinder's own Task API is the requirement regardless of which path is chosen.
- The librarian-class persona's own tool/capability profile (what it can touch beyond `ta_wiki_create`/`update`) is not specified here: out of scope for this doc, needed before wiki delegation is implementable.
- What "things like analytics" (the user's own carve-out from "all outbound interfaces thread through CoS") concretely includes needs to be enumerated, not left as a loose exception.
- `security-hypotheses.md` should gain new entries once this ships: CoS's declared toolset contains no mutating tool (a manifest/tool-availability check, not just a `PolicyEngine::evaluate()` check, since several of the tools involved, wiki, whiteboard, the new outcome-reporting surface, aren't policy-gated at all); a prompt-injected routing decision cannot reach an agent persona outside the task type's allowed set; no goal receives an auto-approve shortcut solely because CoS originated it.

## Self-review

- Placeholder scan: none. Every resolved question states the concrete mechanism; every open item names exactly what's missing and who owns resolving it.
- Internal consistency: the "zero mutating tools, ever" rule for CoS and the "no new agent-facing mutation tool for anyone" resolution for tasks are consistent. Neither CoS nor a worker persona gets a live task-mutation tool; mutation is always goal-outcome-driven.
- Scope: this doc defines the role model and resolves the wiki/task gaps found this session. It does not specify the librarian persona's profile or the outcome-reporting extension's exact API shape; both are named as open items for the next round with `agentic-pm-ba`, not silently assumed.
