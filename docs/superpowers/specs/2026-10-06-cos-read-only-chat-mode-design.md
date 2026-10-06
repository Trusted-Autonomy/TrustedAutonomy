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
- Classify-and-dispatch: decide a task's type and which persona should run it, then trigger the existing hardened goal-dispatch path (see "New-task routing" below): this is a decision, not a tool that mutates state.
- Nothing else. No `ta_fs_write`, no git, no email, no `ta_wiki_create`/`ta_wiki_update`, no apply/approve-capable tool, no task-mutation tool (see below: none exists for any agent yet, and CoS specifically should never get one).

## Two real gaps found while resolving this, and how they resolve

### Wiki updates

`ta_wiki_create`/`ta_wiki_update` (`crates/ta-mcp-gateway/src/tools/wiki.rs`) are a thin client (`WikiMcpClient`) proxying directly to Wayfinder's own remote wiki MCP endpoint: an immediate write to Wayfinder's backend, not TA-local content, and **not policy-gated on the TA side at all** (only `require_declared_scope`, a config check, not a capability-manifest check).

Since there is no TA-side gate to lean on regardless of caller, and wiki content isn't TA-local (no git history to delegate into), the only available safeguard is tool-surface exclusion: **CoS never holds `ta_wiki_create`/`ta_wiki_update`.** A wiki edit gets classified and dispatched as a hardened `ta run` goal to a librarian-class persona, which holds those tools. The safeguard is Wayfinder's own revision history on its side (its `if_sha` optimistic-concurrency parameter implies it already has one), not a new TA-side mechanism.

### Task mutation (insert, update, assign, mark complete)

Confirmed directly against `ta-virtual-team`'s real code (via `agentic-pm-ba`): `WayfinderClient`'s `create_task`/`update_task_status` (real REST calls: `POST /api/dispatch`, `GET /api/tasks/:id`, `PATCH /api/tasks/:id/status`, `POST /api/tasks`) are called exclusively from `poller.rs`'s own deterministic outcome-reporting cycle, after a dispatched goal finishes (done/blocked/new-work). No agent, CoS or worker, has ever had a live tool to mutate a task directly. This is a clean gap, not a retrofit.

This splits into two different things:

**New-task routing** (an incoming chat/forum/meeting-note message becoming a task) needs no new tool. Today, every incoming message unconditionally becomes a `TaskCandidate` (`poller.rs`, confirmed this session). The fix is to gate that existing step with the classifier: `answer_directly` short-circuits into a chat reply via `start_chat_session()`, `real_work` falls through into the exact same `TaskCandidate` to dispatch flow that exists today, unchanged. CoS never "creates a task" via a tool call: it decides whether the existing pipeline proceeds.

**Mid-lifecycle mutation** (assign, mark complete, flag for revision, reassign) is the genuine gap, and needs new Wayfinder MCP surface. Per the user's explicit preference, and to avoid introducing a new live, agent-invokable mutation tool with its own fresh attack surface, **do not build a `ta_task_update`-style tool any agent calls directly.** Instead, extend the existing deterministic, service-account-authed outcome-reporting mechanism (what `poller.rs` already does post-hoc after a goal completes) to cover more outcome types than today's done/blocked/new-work: `reassign`, `needs-revision`, `complete-by-review`. CoS's "mark this complete" becomes: dispatch a trivial, fast, auto-verified-then-auto-approved `ta run` goal (the same hardened pipeline as every other change), whose *reported outcome*, not a live tool call, is what updates Wayfinder's task record. No agent, ever, holds a tool that mutates a task directly; the mutation is always the post-hoc consequence of a reviewed goal's outcome, matching today's existing risk shape instead of adding a new one.

Net effect: **zero new agent-facing mutating tools, for either wiki or tasks.** CoS stays genuinely zero-tool for mutation in every case.

## Delegation safety: the risks that remain even with zero write tools

CoS holding no mutating tool does not make delegation itself risk-free, since CoS is the single component most exposed to untrusted external input (chat, forum posts, meeting notes). Two concrete safeguards:

1. **Constrain routing by task type, not open agent selection.** CoS classifies a task's type and that classification maps to an allowed set of worker personas (a wiki-edit task can only route to a librarian-class persona, a code task only to a developer-class persona), never an open "pick any agent" choice. This bounds what a prompt-injected routing decision could reach: at worst, a wiki-edit-shaped task gets routed to a librarian persona, not an arbitrarily privileged one.
2. **No auto-approve shortcut for CoS-originated work.** A goal dispatched because CoS classified and routed it gets the same (or stricter) review posture as any other goal. CoS being the most exposed-to-untrusted-input component is exactly why its delegated work must not get a "trusted because CoS asked" fast lane.

## Open items before this is implementable

- Exact shape of the outcome-reporting extension (new outcome types, their service-account auth, how a goal's "mark complete" intent gets threaded through to its outcome report) is `ta-virtual-team`/Wayfinder-side work, to be scoped with `agentic-pm-ba` once this doc is approved.
- The librarian-class persona's own tool/capability profile (what it can touch beyond `ta_wiki_create`/`update`) is not specified here: out of scope for this doc, needed before wiki delegation is implementable.
- What "things like analytics" (the user's own carve-out from "all outbound interfaces thread through CoS") concretely includes needs to be enumerated, not left as a loose exception.
- `security-hypotheses.md` should gain new entries once this ships: CoS's declared toolset contains no mutating tool (a manifest/tool-availability check, not just a `PolicyEngine::evaluate()` check, since several of the tools involved, wiki, whiteboard, the new outcome-reporting surface, aren't policy-gated at all); a prompt-injected routing decision cannot reach an agent persona outside the task type's allowed set; no goal receives an auto-approve shortcut solely because CoS originated it.

## Self-review

- Placeholder scan: none. Every resolved question states the concrete mechanism; every open item names exactly what's missing and who owns resolving it.
- Internal consistency: the "zero mutating tools, ever" rule for CoS and the "no new agent-facing mutation tool for anyone" resolution for tasks are consistent. Neither CoS nor a worker persona gets a live task-mutation tool; mutation is always goal-outcome-driven.
- Scope: this doc defines the role model and resolves the wiki/task gaps found this session. It does not specify the librarian persona's profile or the outcome-reporting extension's exact API shape; both are named as open items for the next round with `agentic-pm-ba`, not silently assumed.
