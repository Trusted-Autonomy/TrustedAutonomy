# TA Constitution

> The canonical behavioral contract for Trusted Autonomy.
> Every command, subsystem, and integration must adhere to these rules.
> Pre-release reviews validate conformance against this document.

**Last updated**: v0.17.0.12.13-alpha (added §1.8 Workflow-First: Prefer Workflow Over Code)
**Status**: Living document — update when behavior changes.

---

## 1. Core Principles

### 1.1 Agent Invisibility
TA is invisible to the agent. The agent works in a staging copy using native tools (editor, compiler, test runner). It sees a normal project — not a sandboxed environment. TA mediates through injection (CLAUDE.md, settings) and observation (diffs), never by intercepting agent commands.

### 1.2 Default-Deny
All agent actions are denied unless explicitly granted. The policy engine evaluates every request against a capability manifest. No manifest = no access. Expired manifest = no access. Unknown verb = deny.

### 1.3 Human-in-the-Loop
Irreversible side effects always require human approval. The verbs `apply`, `commit`, `send`, and `post` are hardcoded as approval-required regardless of grants. The agent may propose; only the human (or a constitutional auto-approval policy) may execute.

### 1.4 Observable & Actionable
Every outcome must be observable (logged with details) and actionable (user knows what to do next). No silent failures. No bare "Error" messages. Every error path includes: what happened, what was being attempted, and what the user can do about it.

### 1.5 Append-Only Audit
All significant actions are recorded in an append-only, hash-chained audit log. Each event links to the previous via `previous_hash`. The chain is verifiable via `ta audit verify`. No event may be deleted or modified after write.

### 1.6 Data-Defined Extensibility
System entities that represent a category of pluggable or extensible behavior — roles, personas, teams, workflow step kinds, plugin/adapter kinds, ID-to-version derivation rules, and similar — MUST be defined as data (TOML/YAML/JSON, loaded at runtime) rather than as closed Rust enums or hardcoded match arms, unless the set of variants is genuinely fixed and finite by the domain itself (e.g., HTTP methods, a wire protocol's opcode set).

When introducing a new entity of this kind, the implementer MUST justify, in the PR or draft description:
1. Why it should be enum/code-defined rather than data-defined, if that choice is made.
2. What its extensibility limit is — how many variants exist today, and what happens when a caller needs one that doesn't exist yet.

A silent catch-all (`_ => None`, `_ => unreachable!()`, or equivalent) that drops an unhandled case without producing a loud, actionable error is never acceptable, regardless of whether the entity is enum- or data-defined. Unhandled cases must fail visibly.

**Why this rule exists**: `phase_id_to_semver()`'s closed match arms (3-part and 4-part phase IDs only) silently skipped TA's own version-bump for every 5-segment phase ID, leaving `ta --version` stale through six merged releases before anyone noticed (fixed in v0.17.0.12.10). `TeamRole` was found to be a closed enum with the same latent risk. `EXTERNAL_TOOLS` was found hardcoded as a Rust array requiring a core PR to extend. This principle exists to stop this specific class of bug from recurring a fourth time. See `docs/design/ta-concepts-and-architecture.md` for the full audit that surfaced this pattern.

### 1.7 Reuse Before Reinventing
Before introducing a new protocol, trait, or enum for a category of pluggable/extensible behavior (an integration point, a plugin kind, a communication pattern), the implementer MUST check `docs/design/ta-concepts-and-architecture.md`'s concept catalog for an existing abstraction that already covers the need, and either extend/reuse it or justify in the PR/draft description why a new one is required.

**Why this rule exists**: VCS, messaging, social, and agent-runtime plugins were each built as four fully independent Rust type definitions for the same "external process, call/response" idea, with no shared base trait or crate between them — despite VCS plugins already existing when the other three were written. This rule exists to stop a fifth independent reimplementation of the same pattern.

### 1.8 Workflow-First: Prefer Workflow Over Code
Before implementing a new capability as compiled Rust code (a new CLI command, a new internal function, a new crate), the implementer MUST first ask whether it can be expressed as a **workflow** — an agent-orchestration composition (parallel/pipeline agent calls, a consensus template, a multi-persona review script) using TA's existing agent-runtime and consensus infrastructure — instead of new code.

Workflow is strongly preferred because it is data/script-defined (composable, community-shareable, inspectable, changeable without a Rust recompile) rather than compiled and closed.

When code is chosen over workflow, the implementer MUST justify why, in the PR or draft description. **"Performance" is not by itself an acceptable justification** — if performance is the stated reason, the implementer must first measure the actual workflow-based approach's latency/cost and identify the specific bottleneck, rather than assume a workflow would be too slow. A verified, named bottleneck (e.g., "each agent invocation costs 30s+ of model latency, and this path runs on every keystroke") is a valid justification; an unverified assumption is not.

**Why this rule exists**: raised 2026-07-06 as a durable operating principle after observing that a genuinely code-first instinct (build a new CLI command, a new Rust struct) is the default even when an orchestration script would do the same job more flexibly and without a recompile — e.g., "red team review" of a completed goal/draft is fundamentally a multi-agent adversarial-review composition, not a new subsystem, and should be built as a reusable workflow module rather than new Rust code. See `docs/design/ta-concepts-and-architecture.md` §14 for the worked example and the audit of which already-planned v0.17.0.12.x phases are genuinely code (core platform mechanics the workflows run on top of) vs. which should be reconsidered as workflows.

---

## 2. VCS & Branch Management

### 2.1 Feature Branch Isolation
All TA-generated changes land on feature branches, never directly on the user's current branch or `main`. Branch naming convention: `ta/<sanitized-goal-title>` (truncated to 50 characters).

### 2.2 Branch Restoration Guarantee
`ta draft apply` MUST restore the user's original branch after completion, regardless of success or failure. The sequence is:
1. Save current branch via `adapter.save_state()`
2. Create feature branch, commit, push, open review
3. Restore original branch via `adapter.restore_state()`
4. Step 3 executes even if step 2 fails partially

Violation of this rule leaves the user on an unexpected branch with no indication of how to recover.

### 2.3 Submit Abstraction
VCS operations use three abstract stages, not git-specific terminology:
- **Stage**: Prepare changes for submission (git: branch + commit; p4: changelist; svn: implicit)
- **Submit**: Send to remote (git: push; p4: shelve/submit; svn: commit)
- **Review**: Request human review (git: PR; p4: review; svn: email/external)

CLI flags use `--submit`/`--no-submit` and `--review`/`--no-review`. Legacy `--git-commit`/`--git-push` are backward-compat aliases.

### 2.4 Default Submit Behavior
When a VCS adapter is configured (`[submit].adapter != "none"`), `ta draft apply` defaults to running the full submit workflow (stage + submit + review). The user must explicitly pass `--no-submit` to skip. Plain `ta draft apply <id>` does the right thing.

### 2.5 Commit Message Safety
Goal titles, draft summaries, and other user/AI-generated text MUST be sanitized before use in VCS commit messages or branch names. No shell interpolation — use direct argument passing. Special characters (backticks, single quotes, newlines) must be escaped or removed.

---

## 3. Staging & Overlay

### 3.1 Full Copy Model (V1)
Staging creates a complete copy of the source project in `.ta/staging/<goal-id>/`. The agent works in this copy. Diffs are computed by comparing staging to source.

### 3.2 Infrastructure Exclusion
The following directories are ALWAYS excluded from staging copies and diffs, regardless of `.taignore` configuration:
- `.ta/` — TA state and configuration
- `.claude-flow/` — Agent framework state
- `.hive-mind/` — Agent framework state
- `.swarm/` — Agent framework state

This prevents TA internal state from leaking into agent workspaces or draft artifacts.

### 3.3 Binary Detection
Files with null bytes in the first 8KB are classified as binary. Binary files appear in diffs with size summaries, not content. Both `overlay.rs` and `draft.rs` use this heuristic consistently.

### 3.4 Staging Cleanup
Staging directories for completed/applied goals should be cleaned up. `ta goal gc --include-staging` removes stale staging. Applied goals should auto-clean staging on successful apply (configurable, default: on).

---

## 4. CLAUDE.md Injection & Cleanup

### 4.1 Injection Content
`ta run` injects the following into the staging copy before launching the agent:
- `CLAUDE.md` — plan context, memory context, goal objective, interactive mode sections
- `.claude/settings.local.json` — TA-specific tool permissions
- `.mcp.json` — MCP server routing for TA tools

### 4.2 Backup Before Injection
Before modifying any file, the original content is saved as a backup. If the file did not exist, this is recorded so it can be deleted during cleanup.

### 4.3 Cleanup Guarantee
ALL injected content MUST be removed before:
- Computing diffs (`ta draft build`)
- Any early return or error exit from `ta run`
- Follow-up re-injection (restore original first, then inject fresh)

**Invariant**: No injected content appears in diffs, draft artifacts, or commits. The agent's changes are the only things captured.

### 4.4 Follow-Up Re-Injection
When a follow-up goal reuses the parent's staging, CLAUDE.md must be restored from backup before re-injecting. This prevents stale or nested injection content.

---

## 5. Goal Lifecycle

### 5.1 State Machine
Valid states and transitions:

```
Created → Configured → Running → PrReady → UnderReview → Approved → Applied → Completed

Running ↔ AwaitingInput  (interactive mode)
UnderReview → Running    (denied draft, retry)
PrReady → Running        (macro goal inner loop)
Any state → Failed       (always valid)
```

### 5.2 No State Skipping
Transitions that skip intermediate states are rejected. `Created → Running` is invalid (must go through `Configured`). The `transition()` method validates all state changes.

### 5.3 Failure Always Allowed
Any state may transition to `Failed`. This ensures crashed agents, timeouts, and user cancellations can always be recorded.

### 5.4 Goal Process Liveness
A goal in `Running` state must have a live agent process. If the process exits without updating state, the daemon should detect this and transition to `Completed` (exit 0) or `Failed` (non-zero).

### 5.5 Zombie Prevention
Goals stuck in `Running` with no live process are zombies. `ta goal gc` should detect and offer to transition them. Goals dispatched via daemon should have configurable timeouts.

### 5.6 Goal Traceability Invariant
Every goal that was ever started MUST be completely traceable through TA tooling, regardless of how it ended.

1. **`ta goal list --all` is the authoritative ledger.** Every goal run, past or present, must appear here. No goal may be silently dropped.
2. **`Failed` goals with staging directories are NOT truly terminal.** A goal killed by watchdog or system crash may have recoverable work. Such goals MUST surface in the default `ta goal list` output — they must not be hidden the way `Applied`/`Completed` goals are.
3. **Recovery path always visible.** When a failed goal has an existing staging directory, `ta goal list` output MUST surface the recovery hint (`ta goal recover <id>`) inline — not buried inside the goal's JSON.
4. **Watchdog transitions are audited.** When the daemon watchdog transitions a goal to `Failed`, it MUST write an audit record including: detected PID exit, goal ID, detection timestamp, and recovery command.

**Corollary**: Staging is preserved until explicitly GC'd. TA must surface recoverable failed goals rather than hiding them from default views.

### 5.7 Goal Lifecycle Cleanup
`ta goal gc` is the designated cleanup command:
- `ta goal gc` — detects and transitions zombie `Running` goals (dead PID) to `Failed`; prints summary.
- `ta goal gc --include-staging` — additionally removes staging directories for `Applied` and `Completed` goals.
- `ta goal gc --dry-run` — previews all actions without making changes.
- Goals in `Closed`, `Denied`, or `Applied` states with no open draft require explicit purge; GC does not auto-delete them.

`ta goal list` should show a GC hint footer when zombie or stale goals are detected.

---

## 6. Policy Engine

### 6.1 Evaluation Order
1. Agent has capability manifest? → No → **Deny**
2. Manifest expired? → Yes → **Deny**
3. Path traversal in resource URI? → Yes → **Deny**
4. Verb in approval-required list? → **RequireApproval** (even with matching grant)
5. Matching grant exists? → **Allow**
6. No match → **Deny**

### 6.2 Approval-Required Verbs
`apply`, `commit`, `send`, `post` — these represent irreversible side effects and ALWAYS require human approval regardless of grants.

### 6.3 Path Traversal Guard
Resource URIs containing `..` or absolute paths outside the workspace are rejected. Agents must not escape the staging directory.

### 6.4 Supervised Mode
When security level is `Supervised`, only read verbs (`read`, `list`, `diff`, `status`, `search`) are allowed without approval. All other verbs require approval.

### 6.5 Fail Closed
Invalid glob patterns in grants never match. The system fails closed (deny) rather than open (allow).

---

## 7. Audit & Compliance

### 7.1 Append-Only Writes
The audit log is opened in append mode. Writes are flushed after each event to ensure OS-level durability.

### 7.2 Hash Chain Integrity
Each `AuditEvent` includes a `previous_hash` field linking to the prior event. This forms a tamper-evident chain. `ta audit verify` validates chain integrity.

### 7.3 Tracked Actions
All of the following produce audit events:
- `ToolCall` — MCP tool invocation
- `PolicyDecision` — policy engine evaluation
- `Approval` — human approval action
- `Apply` — changes applied to target
- `Error` — error during processing
- `AutoApproval` — draft auto-approved by policy

### 7.4 Terminal Transition Auditing
Every path that ends a goal's lifecycle MUST write an audit record: apply, deny, close, delete, gc, timeout, agent crash. No goal data should be removed without a trace.

---

## 8. Drift Detection

### 8.1 Behavioral Baseline
Drift is measured against the agent's historical behavior across five signals:
- **ResourceScope** — URIs outside historical pattern
- **EscalationFrequency** — change in policy escalation rate
- **RejectionRate** — change in draft denial rate
- **ChangeVolume** — unexpectedly large/small diffs
- **DependencyPattern** — unusual external dependency additions
- **ConstitutionViolation** — undeclared access per access constitution

### 8.2 Severity Levels
- **Normal** — within historical variance
- **Warning** — notable deviation (20% rate delta, 2x volume factor)
- **Alert** — significant deviation (50% rate delta, 3x volume factor)

Constitution violations are always Warning or higher.

---

## 9. Shell & Daemon Trust Model

### 9.1 Shell as Thin Client
`ta shell` is a stateless REPL and renderer. It has no direct file access, no policy enforcement, no business logic. All authority lives in the daemon.

### 9.2 Daemon Mediates All Writes
The agent (and shell) propose actions. The daemon evaluates policy, records audit events, and mediates execution. The agent never writes directly to the source project — all changes flow through staging → diff → draft → apply.

### 9.3 Daemon Auto-Start
If `ta shell` cannot reach the daemon, it MUST auto-start via `daemon::ensure_running()`. The user should never have to manually start the daemon to use the shell. If the daemon is still unreachable after auto-start, shell fails with a clear error.

### 9.4 Daemon Version Guard
If the running daemon version does not match the CLI version, the shell MUST auto-restart the daemon to ensure version parity. The CLI and daemon are tightly coupled — running mismatched versions leads to silent failures, missing features, and protocol incompatibilities. The restart happens automatically before entering the shell; the user sees the version transition in startup output.

### 9.5 Agent Read-Only Inspection
The agent can read daemon state (goal status, draft details, plan progress, logs) through MCP tools or daemon API. It cannot mutate state without daemon mediation and policy evaluation.

---

## 10. Draft Lifecycle

### 10.1 Draft States
```
Draft → PendingReview → Approved { approved_by, approved_at }
                      → Denied { reason, denied_by }

Approved → Applied { applied_at }
         → Superseded { superseded_by }

Any non-terminal → Closed
```

### 10.2 Supersession Rules
- **Same staging follow-up**: New draft auto-supersedes parent draft (same workspace, cumulative changes)
- **Different staging follow-up**: Drafts are independent — no auto-supersession
- Superseded drafts cannot be applied or re-reviewed

### 10.3 Apply Idempotence
`ta draft apply` copies artifacts from the draft package to the source project. If the source has diverged, conflict detection identifies phantom artifacts (changed in source since staging snapshot). The user must resolve conflicts before apply proceeds.

### 10.4 Draft Amend (planned)
A lightweight follow-up that works with an existing feature branch rather than creating new staging. Amends the draft with additional changes without full staging copy overhead.

---

## 11. Plugin Architecture

### 11.1 Plugin Types
- **Channel plugins**: Deliver agent questions to external systems (Discord, Slack, email). JSON-over-stdio protocol.
- **Submit plugins**: VCS adapters for non-built-in systems. Named `ta-submit-<name>`.
- **Data write plugins**: Audit storage backends (database, cloud storage).

### 11.2 Plugin Discovery
Plugins are executables in `~/.ta/plugins/` or project-local `.ta/plugins/`. Named by convention: `ta-<type>-<name>`.

### 11.3 Plugin Isolation
Plugins run as separate processes. They communicate via stdio (channel plugins) or CLI protocol (submit plugins). A misbehaving plugin cannot corrupt TA state.

### 11.4 macOS Code Signing
On macOS, plugin binaries must be re-signed with `codesign --force --sign -` after copying to prevent AppleSystemPolicy from blocking execution.

---

## 12. Build & Test Environment

### 12.1 Nix Toolchain
All cargo commands run inside the Nix devShell. Use `./dev "command"` or `nix develop --command bash -c "command"`.

### 12.2 Pre-Commit Verification
Four checks must pass before every commit:
1. `cargo build --workspace`
2. `cargo test --workspace`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo fmt --all -- --check`

### 12.3 Test Fixtures
All tests requiring filesystem access use `tempfile::tempdir()`. No hardcoded paths. No test pollution across runs.

### 12.4 Platform Parity
Tests must pass on macOS, Linux, and Windows CI. Platform-specific tests use `#[cfg(unix)]` / `#[cfg(windows)]` with appropriate implementations for each.

---

## 13. Error Handling

### 13.1 Structured Errors
Error messages include:
- **What happened**: The specific failure
- **What was being attempted**: The operation context
- **What to do**: Next steps for the user

### 13.2 Timeout Reporting
Timeout errors state: which operation, the timeout duration, and how to configure it.

### 13.3 CLI Confirmation
Commands confirm what they did, not just succeed silently. Include counts, paths, IDs, and durations where relevant.

### 13.4 Logging
Use `tracing::warn`/`tracing::error` for operational issues. Include structured fields (command, duration, path), not just string messages.

---

## 14. Autonomous Operations & Self-Healing

### 14.1 Detection Without Mutation
The daemon watchdog may detect issues (dead processes, low disk, crashed plugins) continuously and without human consent. Detection is read-only observation — no state is changed.

### 14.2 Corrective Action Approval
All corrective actions are proposals. The daemon presents the issue, diagnosis, and proposed fix. The user approves or denies. No corrective mutation happens without consent, unless covered by auto-heal policy.

### 14.3 Auto-Heal Policy Scope
Auto-heal is opt-in and explicitly scoped. Only actions listed in `[operations.auto_heal].allowed` may execute without approval. The allowed list must be conservative — only low-risk, reversible actions qualify (restart plugin, mark zombie failed, clean applied staging). High-risk actions (delete goal, kill process, gc drafts) always require approval.

### 14.4 Diagnostic Goals Are Read-Only
Diagnostic goals spawned by the daemon for issue investigation have read-only access. They produce reports, not changes. The policy engine enforces this via read-only capability manifests with no write/apply grants.

### 14.5 Corrective Action Audit
Every corrective action — whether auto-healed or human-approved — produces an audit event with: what was detected, what was proposed, who/what approved (human or auto-heal policy), and the outcome. The audit trail must be as complete for automated operations as for human-initiated ones.

### 14.6 Escalation Path
If a corrective action fails, or if the daemon detects an issue it cannot diagnose, it escalates to the user via all configured channels. Auto-heal never retries a failed corrective action — it escalates instead.

### 14.7 Runbook Transparency
Operational runbooks execute step-by-step with each step visible to the user. The user can interrupt, modify, or cancel at any step. Runbooks do not execute as opaque batches.

---

## 15. VCS Submit Invariant

### 15.1 Mandatory Branch Isolation
All VCS adapters MUST route agent-produced changes through an isolation mechanism (branch, shelved CL, patch queue) before any commit. `prepare()` is the mandatory enforcement point — failure is always a hard abort. After `prepare()`, the adapter MUST NOT be positioned to commit directly to a protected target.

### 15.2 Protected Target Declaration
Adapters MUST declare protected targets via `protected_submit_targets()`. The default implementation returns an empty list (safe for adapters where `prepare()` guarantees isolation without an explicit target check). Built-in adapters MUST override:
- **Git**: `["main", "master", "trunk", "dev"]` by default; overridable via `[submit.git].protected_branches` in `workflow.toml`.
- **Perforce**: `["//depot/main/..."]` by default; `prepare()` creates a pending CL as isolation.
- **SVN**: `["/trunk"]` by default; blocks direct trunk commits until branch/copy support is added.

### 15.3 Verification After Prepare
`verify_not_on_protected_target()` MUST be called immediately after `prepare()` succeeds and before any commit or push. Hard failure on any match — the apply workflow aborts. This check replaces all hardcoded adapter-name checks in the apply path (no more `if adapter.name() == "git"`).

### 15.4 Plugin Compliance
When extracting adapters to external plugins, implementations MUST expose `protected_targets` and `verify_target` capabilities via the JSON-over-stdio protocol. The plugin registry SHOULD emit `tracing::warn!` if a plugin declares protected targets but `verify_target` is a no-op.

### 15.5 This Invariant Applies to All Adapters
The VCS Submit Invariant applies to all current built-in adapters (git, svn, perforce) and to all future plugin-supplied adapters. No adapter may bypass it through special-casing or by returning an always-Ok `verify_not_on_protected_target()` when `protected_submit_targets()` is non-empty.

---

## 16. Workflow Graph & Modular Decision Nodes

> **DRAFT — pending user approval, not yet in the Appendix checklist.** Added 2026-07-21 alongside `docs/superpowers/specs/2026-07-21-workflow-graph-engine-design.md`, red-teamed by three adversarial passes (PM, head of engineering, non-technical user) same day. Not yet enforced. This banner and the missing checklist rows are the graduation gate: remove the banner and add §16 rows to the Appendix table only once v0.17.7.1 ships and this section has been through normal review — don't let enforcement start silently just because the code exists.

**Why this section exists**: found 2026-07-20, during this session's autonomous multi-phase run — `ta_policy::auto_approve::should_auto_approve_draft`, `ta_session::advisor_agent::check_advisor_auto_approve`, and the governed-workflow consensus engine (`stage_consensus`/`stage_apply_draft`) each independently decide whether to auto-approve a draft apply, with no shared gate and no awareness of one another; a consensus-approved draft today bypasses the other two entirely. Separately, `execute_pr_merged_continuation` (v0.17.0.12.31) is wired directly to Git/GitHub semantics rather than the project's own `SourceAdapter` abstraction, and has no way to inspect *why* a CI check failed. This section exists to stop a fourth disconnected approval mechanism and a second platform-specific trigger path from being built, the same way §1.6/§1.7/§1.8 exist to stop their own named incidents from recurring.

### 16.1 New Approval/Gating Logic Goes Through the Graph
Any new code that produces a `Decision{verdict}` consumed by an `AutoApproveAction`, `RecommendAction`, or `EscalateAction` (i.e., decides whether a draft apply, merge, or similar TA-mediated action proceeds, is surfaced to a human, or is blocked) MUST be built as a `ReviewerNode`/`DecisionNode` pair wired into the graph engine (§2 of the design spec), not as a new standalone function with its own independent approve/deny return value. This rule targets **approval-gating logic specifically** — it does not require rewriting unrelated checks (policy verb allowlists at §6.2, path-traversal guards at §6.3, disk-space auto-heal thresholds at §14.3, etc.) into graph nodes; those are not in scope unless they start being used to gate an apply/merge decision. A new mechanism that bypasses the graph entirely for an apply/merge decision is a constitution violation unless justified in the PR/draft description (why the graph model doesn't fit, per the §1.6/§1.8 justification pattern). Adding a new `kind` value to an existing node trait (a new `ReviewerNode` implementation, a new persona role string) is ordinary extension, not a new mechanism, and needs no such justification.

### 16.2 Recommendation and Auto-Approval Are the Same Decision, Different Wiring
A `DecisionNode`'s output MUST NOT encode, in its own type or logic, whether it will be acted on autonomously or surfaced to a human — that distinction belongs entirely to which `ActionNode` the graph wires the decision to (`AutoApproveAction` vs. `RecommendAction` vs. `EscalateAction`). Concretely: in plain terms, this means a rule like "auto-approve safe changes, always ask a human about risky ones" is one graph with two different `ActionNode`s at the end, not two separate approval systems. **New graphs default to `RecommendAction`.** A graph may only be wired to `AutoApproveAction` once a human has explicitly reviewed and upgraded it — mirroring §1.3's Human-in-the-Loop default and giving a non-technical user a plain guarantee: nothing starts auto-approving on your behalf without you turning it on.

### 16.3 One Approval Gate: A Named Call-Site Invariant
`ta draft apply`'s approval check MUST call exactly one graph instance. No other code path may call `should_auto_approve_draft`, `check_advisor_auto_approve`, or `run_consensus` directly to gate an apply/merge decision — each becomes a `ReviewerNode` implementation feeding that one graph's `DecisionNode` instead. This is a structural call-site rule, not just an aspiration: a PR that adds a second direct caller of any of those three functions (or their v0.17.7+ successors) for the purpose of gating an apply/merge is a constitution violation, full stop, no justification accepted — this is the one thing §16 exists to prevent.

### 16.4 Triggers Are VCS-Adapter-Mediated, Never Platform-Specific
A `TriggerSource` that reacts to VCS/CI state (a review reaching a terminal state, a CI check failing) MUST obtain that state through `SourceAdapter` (§2.3's Stage/Submit/Review abstraction), never by calling a specific platform's CLI or API directly from trigger/graph code. Where the current `SourceAdapter` trait doesn't expose enough detail (e.g., per-check failure logs), extend the trait with a defaulted, adapter-optional method (e.g. `check_failures()`), following the same pattern as `check_review()`/`merge_review()`. **A permanent empty-vec/`None` return from an adapter whose platform has no native equivalent (e.g., Perforce has no concept of a "CI check") is fully compliant, not a gap to close** — the trait contract only requires that non-supporting adapters degrade visibly (§1.4 Observable & Actionable: "CI failure detail unavailable for this VCS adapter" is an acceptable, actionable message), not that every adapter eventually grows the same capability.

### 16.5 Graphs Are Inspectable and Reproducible
A workflow graph's definition (nodes, edges, weights, thresholds) MUST be a readable data file, not embedded in compiled logic — so a human (or a future visual editor, deferred per the roadmap) can read exactly what will happen before it happens, and so the same graph can be replayed identically. This extends §1.6 (Data-Defined Extensibility) to decision composition specifically. Note the boundary: the *set of decision algorithms* (e.g., Raft/Paxos/Weighted) may remain a closed Rust enum under §1.6's own "genuinely fixed and finite" carve-out — it's the *wiring* (which algorithm, what threshold, what weights, which nodes) that must be data, not necessarily the algorithm implementations themselves.

**Known limitation, stated plainly**: as of v0.17.7, authoring or editing a graph means hand-editing a TOML file — there is no visual editor yet (deferred to v0.18+, bundled with the Studio frontend migration and the plan-DAG visualization). The one part of this system usable directly by a non-technical user in v0.17.7 is the advisor's natural-language entry point (design spec §6, e.g. "build phases v0.17.3 through v0.17.8") — everything else in this section requires a developer to hand-author or modify the underlying TOML until the visual builder ships.

---

## Appendix: Constitution Compliance Checklist

For pre-release review, verify each command against these rules:

| Command | Key Rules |
|---------|-----------|
| `ta run` | 4.1-4.4 (injection/cleanup), 5.1-5.2 (state machine) |
| `ta draft build` | 4.3 (cleanup before diff), 3.2 (infrastructure exclusion) |
| `ta draft apply` | 2.1-2.2 (branch isolation + restoration), 2.4 (default submit), 7.3 (audit) |
| `ta draft deny` | 7.4 (terminal audit), 10.1 (state transition) |
| `ta goal start` | 3.1 (staging copy), 5.1 (Created → Configured) |
| `ta goal list` | 5.6 (traceability — failed+staging goals visible by default) |
| `ta goal recover` | 5.6 (recovery path always visible), 5.3 (failure always allowed) |
| `ta goal delete` | 7.4 (terminal audit) |
| `ta goal gc` | 5.5 (zombie detection), 5.7 (lifecycle cleanup), 7.4 (terminal audit) |
| `ta shell` | 9.1-9.5 (thin client, daemon mediates, auto-start, version guard) |
| `ta plan *` | 9.4 (read-only agent inspection) |
| `ta audit verify` | 7.2 (hash chain validation) |
| Plugins | 11.2-11.4 (discovery, isolation, signing) |
| Watchdog | 14.1 (detection without mutation), 14.5 (audit) |
| Auto-heal | 14.2-14.3 (approval, scoped policy), 14.6 (escalation) |
| Diagnostic goals | 14.4 (read-only), 6.1-6.5 (policy enforcement) |
| Runbooks | 14.7 (step-by-step transparency), 14.2 (approval per step) |
| `ta status` | 13.3 (confirmation), 14.1 (surfaces watchdog findings) |
