# TA Concepts & Architecture: Current State, Right Abstractions, and Refactor Path

**Status**: Living design document review, first written 2026-07-04
**Purpose**: A single, honest inventory of every core concept TA has, what abstraction each one *should* use, whether it currently does, and a sequenced plan to close the gaps. Written in response to a direct architecture review request after the 0.17.0.12.x cleanup arc and the sage-lore/sage/graphify research (see [`sage-lore-review.md`](sage-lore-review.md)).

**How to use this doc**: it's the reference for (a) what to refactor and when, (b) why Studio's UI is overly complex, (c) why the CLI command surface is overly complex, and (d) what "smallest clean verb set" TA should converge on. Update it as concepts change — don't let it go stale like the old `docs/design/sage-lore-review.md` reference did before this session existed.

**Read this first if you want the plain-language version**: [`docs/guides/what-is-ta.md`](../guides/what-is-ta.md) — no jargon, meant as a sanity check on what TA actually is and does before committing to the refactor work this document lays out.

**Read this for the clean, canonical action/flow reference**: [`ta-action-reference.md`](ta-action-reference.md) — the full instruction set distilled out of §1–12's narrative findings into one sage-lore-style spec (Execution Model → grouped Actions → composed Flows → a built-vs-gap table).

---

## 1. Executive Summary

TA's core differentiator — staged review (goal → draft → constitution check → apply) — is solid and doesn't need to change. The problems are all *around* it:

1. **The same "hardcoded when it should be data" mistake has been made at least three times**: `phase_id_to_semver()`'s closed match arms (fixed 2026-07-03), `TeamRole` as a closed Rust enum (not yet fixed), and `EXTERNAL_TOOLS` as a hardcoded array (not yet fixed). This is a pattern, not a one-off — worth naming explicitly so it stops recurring.
2. **TA has 12+ distinct "let something be swapped/extended" mechanisms** where it needs roughly 4. The word "Adapter" alone is used for two *opposite* trust models (in-process-core-only vs. intended-community-contributable) in different parts of the codebase.
3. **The goal → workflow-type → team/role/persona → security-rule mapping the supervisor/advisor was meant to provide does not exist.** Everything is explicit, bottom-up, user-supplied every time (`--team`, `--persona`, `--agent`). There is no default resolution based on what kind of work a goal actually is. These should be definable in the configuration, e.g. in a workflow definition, but overridable. The supervisor/advisor will offer recommendations which can be set to supervisor automated selection where appropriate and desired.
4. **Studio's 15-tab sprawl and the CLI's ~250-action surface are the same root cause wearing two costumes**: both surface every backend struct as its own top-level destination, with no information-architecture layer reconciling overlapping concepts. Fix the concept model first; both surfaces collapse substantially on their own.
5. **Two capabilities you want kept in the plan for later are confirmed genuinely absent** (not just unbuilt-but-planned): data-defined (non-enum) roles, and any knowledge-graph/persona-hierarchy mechanism. Consensus-based multi-agent review, however, is **already built and shipped** (v0.15.15/v0.15.15.1) — a corrected finding from earlier in this review; it's the right foundation to extend rather than something to build from scratch.

---

## 2. Concept Catalog

For each concept: what it is, current abstraction, right abstraction, when to use it, and status.

### 2.1 Goal / Draft / Apply (the core thesis — keep as-is)

**What it is**: an agent works inside a staged overlay; the result becomes a reviewable `DraftPackage` (diff + AI summary + supervisor verdict); a human or policy approves; `apply` materializes it.

**Status**: built, mature, don't touch the shape of this. The bugs found this session (silent version-bump failures, draft missing an artifact, `ta goal delete` resetting a `done` phase) are implementation bugs in this pipeline, not evidence the model itself is wrong. Tracked separately as PLAN.md v0.17.0.12.11.

**Gap found 2026-07-04 — no composable threshold-based auto-approval for this pipeline.** `AdvisorSecurity::Auto` (`crates/ta-session`) exists but is a pure unconditional bypass (`if ctx.security == AdvisorSecurity::Auto { allow }`) — no threshold logic. `SupervisorReview` has a `verdict` (Pass/Warn/Block) and `DraftPackage` has a `risk_score: u32` field, but `risk_score` is hardcoded to `0` (or a static test value) everywhere it's constructed in the real CLI command paths (`run.rs`, `draft.rs`, `plan.rs`, `constitution.rs`) — a real field, not a live computed signal. No `confidence` field exists on `SupervisorReview` at all.

The exact composable pattern (verdict/risk/confidence gating an auto-action) **already exists and works, twice — just not here**: `email_manager.rs` and `social_adapter.rs` both have a real, tested `min_confidence: f64` (default **0.80**) gating auto-send/auto-post (`if reply.confidence < supervisor.min_confidence { require human }`). This is the exact "confidence > 80%" pattern, scoped to two narrow side-channels instead of the core draft pipeline.

**This is the concrete missing piece behind why every phase in today's session needed manual `ta draft apply`** — not just the orchestrator poll bug (§2.6/§7 item 3), but the deeper fact that there is no way to say "auto-approve *unless* the signals look risky," only "always bypass for this role" or "never." Per §1.7 (Reuse Before Reinventing), the fix is to generalize the proven email/social `min_confidence` pattern rather than invent a new one: add a real `confidence` field to `SupervisorReview`, make `risk_score` an actually-computed value, and let a workflow/team definition declare a composable rule (e.g. `verdict == pass && risk_score < N && confidence > 0.8`) per role — with `AdvisorSecurity::Auto` as the switch that activates the threshold check, not an unconditional bypass. This belongs alongside the escalation-stub/cost-governor/self-sourced-backlog gaps already tracked as blocking genuine unattended autonomy.

### 2.2 Extensibility (Plugins / Adapters / Connectors / Backends)

**Current state** (12 distinct patterns found, verified against code):

| Pattern | Shape | Community today? |
|---|---|---|
| VCS plugins | External process, `method:String` dispatch | Yes |
| Messaging plugins | External process, independent Rust enum dispatch | Yes (own reimplementation) |
| Social plugins | External process, independent Rust enum dispatch | Yes (own reimplementation) |
| Agent runtime plugins | External process, `method:String`-style, own type | Designed for it, not yet community-facing |
| Channel/listener plugins (Discord/Slack/Email) | External **long-running daemon**, no manifest | Structurally possible |
| Connectors (fs/web/comfyui/unreal/unity) | **In-process** trait | No — core PR required |
| `EXTERNAL_TOOLS` | Hardcoded Rust array | No |
| `.ta/community-resources.toml` | Declarative registry, no executable contract | Yes (passive resources only) |
| `DbProxyPlugin` | In-process trait (as spec'd) | No |
| `ReleaseAdapter` (planned) | In-process trait (as spec'd) | No |
| `BuildAdapter` | In-process trait | No, shouldn't be |
| Output format renderers | In-process closed enum | No, and correctly so — rendering isn't an integration point |

**The naming collision**: "Adapter" is used for both `BuildAdapter`/output-adapters (in-process, core-only, correctly closed) and `ReleaseAdapter` (intended to be community-contributable). Same word, opposite trust model. This causes real confusion when reading the codebase and needs a rename as part of the fix, not just a protocol unification.

**Right abstraction — four categories, not twelve:**

1. **Plugin** (external process, call/response, community-contributable): unify VCS/messaging/social/runtime plugins onto *one* shared protocol crate and *one* `plugin.toml` manifest schema, discovered at `.ta/plugins/<kind>/<name>/`. *Use when the integration is optional, should be swappable without a TA release, and a community member should be able to author one.* `EXTERNAL_TOOLS`, `DbProxyPlugin`, and `ReleaseAdapter` should all migrate here.
2. **Channel/Listener** (external, long-running, supervised): keep Discord/Slack/Email as their own category — genuinely different lifecycle (persistent connection, not call/response). *Use when the integration needs to maintain a session, not just answer discrete calls.*
3. **Backend** (in-process Rust trait, core-only): connectors, `BuildAdapter`, output renderers. Rename away from "Adapter" to avoid the collision. *Use when it's first-party, needs zero IPC overhead, or has no reason to be community-extensible.*
4. **Resource list** (declarative registry, VCS-shared): `.ta/community-resources.toml` and its future siblings (`.ta/db-adapters.toml` from yesterday's DB proxy review, a future `.ta/constitution-rules/` — see §2.5). *Use when there's nothing to execute, just configuration or a pointer to something in category 1 or 3.*

### 2.3 Teams & Roles

**Current state**: `TeamRole` (`crates/ta-session/src/agent_action.rs:17-24`) is a closed enum: `Implementer, Reviewer, QA, Architect, ReleaseManager, Human(String)`. Already `#[serde(rename_all = "snake_case")]`, so it already round-trips as a plain string in TOML — the enum-ness is a Rust-side constraint, not a serialization one. Confirmed only **52 usages across 6 files**, and the actual hardcoding chokepoint is a single fixed string→enum parser in `apps/ta-cli/src/commands/team.rs`. The `Human(String)` variant is already precedent that an open string works fine here.

**Right abstraction**: a data-defined role — effectively what `Human(String)` already proves works, generalized to all roles. **This is a small, contained refactor, not a sprawling one** — the 6-file/52-usage footprint means it's safe to do soon rather than needing to be deferred as "too risky to touch."

**Status**: confirmed genuinely closed/hardcoded, low-risk to fix, not yet fixed.

### 2.4 Personas

**Current state**: `PersonaConfig` (`crates/ta-goal/src/persona.rs:34-53`) is *already* data-defined — `.ta/personas/<name>.toml` with `system_prompt`, `constitution` (extension path), `capabilities` (allowed/forbidden tools), `style` (output format/length). Applied via `ta run --persona <name>`, injected into CLAUDE.md via `to_claude_md_section()`, called from `run.rs:1809-1811`. This part of the vision is genuinely done.

**What's missing**: no knowledge-graph/hierarchy field anywhere in `PersonaInner`. The exact plug-in point for a future capability is known: add a field to `PersonaInner`, thread it through `PersonaConfig::load()`, append it inside `to_claude_md_section()`. See §3 for what that field's actual model should be.

### 2.5 Constitution / Security

**Current state**: two genuinely distinct mechanisms, easy to conflate:
- **`AccessConstitution`** (`crates/ta-policy/src/constitution.rs`) — per-*goal*, a pre-declared URI-access-intent contract stored at `.ta/constitutions/goal-<id>.yaml`. A drift-detection mechanism (did the goal touch what it said it would), not a rule-enforcement one.
- **`ApprovalRule`** (`crates/ta-policy/src/approval_rules.rs`) — the actual `[[rules.warn/block]]` engine, project-wide, path-pattern scoped. **No workflow-type field exists** — the same rule set applies regardless of whether a goal is a routine doc fix or a schema-destroying migration.

**Right abstraction for community rule contribution** (per the sage-lore/sage review): a `threats/*.yaml`-equivalent directory of individually-authored, individually-attributed rule files, staged in a review branch before activating — modeled directly on [gendigitalinc/sage](https://github.com/gendigitalinc/sage)'s contribution pipeline. Not yet designed as its own phase; flagged in the sage-lore review doc as a follow-on, should reuse category-1's discovery/review mechanism (§2.2) rather than building a second parallel pipeline.

**What's missing for workflow-aware scoping**: nothing today varies `ApprovalRule` by workflow type. This is the piece the mapping tree (§3) needs to supply.

### 2.6 Workflows

**Current state, corrected from an earlier pass in this review**:
- `WorkflowStepKind` has five step kinds: `AgentReview`, `PrMonitor`, `PlanCheck`, `SyncBuild`, `HumanGate`. No generic "Parallel" or "Concurrent" step kind exists as a reusable primitive.
- **Consensus is already built and shipped** (PLAN.md v0.15.15 / v0.15.15.1, both `done`): `crates/ta-workflow/src/consensus/mod.rs` has a real `ConsensusAlgorithm` enum (Raft/Paxos/Weighted), a `run_consensus()` dispatcher, 37 tests, and a shipped `code-review-consensus.toml` template — four parallel specialist reviewers (architect/security/principal/PM) each scoring independently, aggregated via configurable consensus. Raft degrades to Weighted with only one active reviewer. **This is real prior art for any further parallel/consensus work — extend it, don't rebuild it.**
- **True concurrent sub-goal scheduling is confirmed still deferred**, explicitly, in PLAN.md at the item marked `[-]` (not just pending — deliberately deferred): *dependency-graph ordering for `--workflow swarm --sub-goals`, moved to v0.13.16 ("local model + advanced swarm phase")*. `swarm` runs sub-goals sequentially today.
- **Serial** already exists implicitly (the default execution mode, and `--workflow serial-phases`).

**Right abstraction**: promote "parallel" and "consensus" from being embedded inside one specific workflow *template* (`code-review-consensus.toml`) to being a generic, reusable `WorkflowStepKind` primitive any workflow definition can use — not just the one built-in template that happens to use them today.

**Status**: serial ✅ built, consensus ✅ built (needs generalizing beyond one template), parallel/concurrent (true, independent sub-goal execution) ⏳ deferred to v0.13.16 per existing plan — correctly already tracked, just needs to stay visible.

---

## 3. The Missing Mapping Tree

This is the part confirmed **genuinely absent** — not partially built, not built-under-a-different-name. Traced the actual call paths: `--team <path>` only pulls the `reviewer` role for `agent_review` steps specifically; persona injection only fires on explicit `--persona <name>`. Zero automatic inference from goal/phase content to role, team, or constitution-rule-subset exists anywhere in `ta-advisor` or the supervisor.

**What it should look like**, synthesizing everything above and refined 2026-07-04: the Workflow Type node should be *a `workflow.toml` definition itself* (or a section within one), not a new parallel registry — reusing the config surface that already exists rather than adding a second one. Resolution has three modes, not a single default:

```
Goal (title + phase context + objective)
  │
  ▼
Workflow Type  ← a workflow.toml definition; selected explicitly,
  │              or inferred from goal/phase content by extending
  │              the existing ta-advisor intent classifier
  │              (classify.rs, §2.6) rather than a new classifier
  │
  ├──▶ Default Team  (which Roles this workflow type needs, defined
  │        │          IN the workflow definition, overridable)
  │        ▼
  │     Role → { Agent, Persona, Security level }
  │              (Persona = system_prompt + capabilities + style
  │               + constitution-extension + [future: KG ref, §4])
  │
  └──▶ Default ApprovalRule subset  (which constitution rules apply
                                     to THIS workflow type)

  Three resolution modes per node, not a single default:
    1. Explicit override — user passes --team/--persona/--agent,
       exactly as today.
    2. Workflow-defined default — the workflow.toml definition's
       own binding.
    3. Supervisor/advisor recommendation, with an auto-select tier
       reusing the EXISTING AdvisorSecurity tri-state
       (read_only/suggest/auto) rather than a new trust-level
       concept — "auto" already means "the advisor may act without
       waiting for a human" for draft actions; extending that same
       meaning to "the advisor may also pick the team/persona" is a
       generalization of a mechanism that already exists.
```

Today, only mode 1 exists — every arrow in this tree requires an explicit flag from the user. Building modes 2 and 3 — extending `workflow.toml` for defaults and `AdvisorSecurity` + `ta-advisor`'s classifier for the recommend/auto-select tier — is the concrete meaning of "the supervisor/advisor was intended to map teams/roles and workflows onto goals." It's also the single piece of work that most directly serves 0.17's stated goal of getting autonomous workflows to first-class: without it, "autonomous" still means "a human chooses the team/persona/security for every goal by hand."

---

## 4. Knowledge Hierarchy ("Agent Brain") — flagged for later, kept in the plan

Two real external reference points were researched, not guessed:

- **[graphify](https://github.com/safishamsi/graphify)** (`safishamsi/graphify`, aka Graphify Labs): real, highly reliable (77K+ stars, YC S26-backed, 30+ contributors, real CI, cross-platform, actively maintained — pushed the same day this doc was written). Ingests a project (code/docs/PDFs/images/video) via Tree-sitter + NetworkX into a queryable knowledge graph, with Leiden clustering to detect **communities** of related nodes and betweenness-centrality to identify **god-nodes**. This is a *community-clustering* model, not a strict ancestry/inheritance tree.
- **[ai-context-hierarchy](https://github.com/CreatmanCEO/ai-context-hierarchy)** (smaller, related, cited in graphify's own v5.0 roadmap): a three-level *leveled context* model — Level 0 (global map, ~2KB, always loaded) → Level 1 (per-project context) → Level 2 (source files, on-demand). Closer to "composite knowledge by level" but still not a strict per-node ancestry-composition mechanism.

**Neither exactly matches** "composite knowledge = union of a node's ancestors up a tree" as originally described. This needs an explicit decision before design work starts: is the target model (a) graphify's community/centrality clustering, (b) the leveled-context model, or (c) a genuinely new ancestry-tree design TA would author itself, possibly informed by but not copying either. Given graphify's reliability (unlike sage-lore), it's a legitimate integration candidate if option (a) is chosen — unlike sage-lore, this doesn't need to be "concepts only."

**Status**: confirmed absent from PLAN.md entirely — no phase, anywhere, references this. Correctly deferred (0.18+ is fine per the request that started this review), but now explicitly on record so it doesn't get lost again. The concrete plug-in point when it's ready: `PersonaInner`'s missing knowledge field (§2.4).

---

## 5. CLI Verb Set

**Current state**: 59 top-level commands, ~250+ distinct invocable actions total (verified from source, not just `--help` text). The sprawl is overwhelmingly the same handful of verbs reimplemented once per noun:
- `list` independently implemented 15+ times
- "read one item" spelled 4 different ways with no consistent rule (`View`, `Show`, `Status`, `Inspect`)
- "delete one item" spelled 2 ways (`Delete` vs `Remove`) — `goal delete` is the outlier, and it's the one with the known phase-reset side-effect bug
- "correctness check" spelled 3 ways (`Validate`, `Verify`, `Check`)
- `install` reimplemented 7 times
- `plan` alone has three ways to create something (`New`/`Create`/`CreatePhase`) and three ways to close something out (`Complete`/`Defer`/`MarkDone`) — the single largest source of internal inconsistency

**Proposed minimal verb set** — ten orthogonal verbs, nouns as subjects (`ta <verb> <noun> [id] [flags]`):

| Verb | Replaces |
|---|---|
| `create` | New / Init / Add / Install (provisioning) |
| `list` | all 15+ independent List implementations |
| `show` | View / Status / Inspect (single-item detail) |
| `update` | Set / Assign / MoveItem / AddItem / Reload |
| `remove` | Delete / Remove / Revoke / Uninstall (unify, and fix the goal-delete phase-reset bug as part of unifying, not carry it forward) |
| `run` | Start(-a-goal) / Resume / Restart |
| `approve` / `deny` | kept first-class — core to the staged-review thesis, not CRUD |
| `apply` | kept first-class — TA's defining action |
| `check` | Validate / Verify / Check / Audit |
| `sync` | Gc / Prune / Migrate / reconcile-with-remote |

Nouns: `goal`, `draft`, `plan-phase`, `team`, `persona`, `workflow`, `plugin`, `template`, `session`, `credential`, `event`, `token`, `office`, `daemon`, `connector`, `community-resource`, `context`.

**Net effect**: `ta draft apply <id>` and `ta goal list` already fit this shape and need no change. Concrete collapse example: `ta agent`'s 15 subcommands (New/Validate/List/Add/Remove/Frameworks/Info/FrameworkValidate/FrameworkNew/Test/Doctor/Install/Publish/InstallQwen/Migrate) become roughly 6 invocations (`create agent`, `list agent [--frameworks]`, `show agent <name>`, `check agent <name>`, `remove agent <name>`, `sync agent`).

**This is a breaking change to the command surface** and should ship with an aliasing/deprecation window (old subcommands print a deprecation notice and forward to the new verb form for some transition period), not a hard cutover — see sequencing in §7.

---

## 6. Studio's 15-Tab Sprawl — same root cause, confirmed

Mapped every tab to its backend concept:

| Tab | Backend concept |
|---|---|
| Dashboard | aggregates 6 different endpoints |
| Active | `/api/active/goals` |
| Plan | `/api/plan/phases` |
| Review Drafts | `/api/drafts` |
| Agent Questions | `/api/interactions/pending` |
| Memory | auto-memory KV store |
| Projects | multi-project registry |
| Workflows | raw `workflow.toml` text editor — no structured step-type UI |
| Release | legacy `ta release dispatch` pipeline (not the new `ReleaseAdapter` model — that has no UI yet) |
| Agents | `[agent_profiles.*]` (model/framework choice only — no team/role content) |
| Personas | `.ta/personas/*.toml`, raw system-prompt textarea |
| Advisor | its own separate conversation history |
| Stats / Health / Settings | mostly legitimate, Health is a real drill-in from Dashboard |

**Confirmed: no UI exists for team/role assignment (`ta team assign`) at all** — zero mentions of team/role/reviewer/implementer anywhere in Studio's render code. Confirmed no structured UI for workflow step types (parallel/consensus/concurrent/serial) — Workflows tab is a raw TOML textarea.

**Synthesis, with direct evidence**: Personas and Agents are two separate tabs *only* because `PersonaConfig` and `[agent_profiles.*]` are two unrelated backend structs — a user thinks of both as "who does this work," not two different concepts. Same story for Active vs. Dashboard's running-goals section, and Agent Questions vs. Dashboard's own questions card (the same endpoint fetched twice for two different UI locations). **Team-role assignment — arguably the single most important "who does this work" concept — has no tab at all**, while three lower-stakes concepts each got one. This is "one tab per struct that happened to get an `/api/` endpoint," not workflow-driven information architecture. Once the concept model is fixed (§2–3), most of these 15 tabs are really 3–4 real destinations (Attention, Activity, Configuration — folding in team/persona/workflow-type together, Advisor) wearing backend-struct-shaped costumes.

---

## 7. Refactor Recommendation & Sequencing

Given "ship 0.17 soon" and everything confirmed above, in order. Items 1–3b are now understood as facets of ONE unifying model (§9) rather than four separate efforts — sequence them together, not independently:

1. **Now / low-risk** — convert `TeamRole` from a closed enum to a data-defined role type. Confirmed small blast radius (52 usages, 6 files), and it's the precondition for everything else in this list. Also rename the "Adapter" naming collision (§2.2) while touching related code.
1b. **Same phase — easy agent/model switching (`Switch`, confirmed product requirement 2026-07-04)**: once roles/personas are data-defined (#1), make which agent/framework/model backs a role a first-class, low-friction thing to change — not a `--agent` flag buried in a command's help text. This is the natural extension of the same refactor, not separate work.
2. **Now / already in motion** — unify the plugin/adapter/connector patterns into the 4-category model (§2.2), migrating `EXTERNAL_TOOLS`, `DbProxyPlugin`, and the planned `ReleaseAdapter` onto the "Plugin" category before any of them get more implementation baked in on the wrong foundation. Do this in terms of the §9 Commit contract, not independently — a new Plugin-category integration IS a Commit implementation for some application, so building the unified contract first means the unification happens automatically as each one lands, rather than needing a second pass.
3. **0.17.x, new phase — build the §9 graph itself**: extract the generic Write → Review → Decision → Commit/Reject shape from its three existing independent instantiations (`DraftStatus`, `social_supervisor_check`, email's `supervisor_check`) into one model, fix the hardcoded `APPROVAL_REQUIRED_VERBS` array as part of it (§9), and build modes 2 and 3 of the goal → workflow-type → team/role/persona → security mapping tree (§3) on top of it: workflow-defined defaults (extending `workflow.toml`, not a new registry) and supervisor-recommended/auto-select resolution (extending `AdvisorSecurity` and `ta-advisor`'s intent classifier). The composable threshold-based auto-approval work (formerly listed as a separate 3b) is not separate — it IS the Decision node's core logic, generalized from `social_supervisor_check()`. This is the single highest-leverage phase for "autonomous workflows to first class" and for closing why today's entire session needed manual `ta draft apply` on every phase.
3c. **Same phase — per-action telemetry (`Meter`, confirmed product requirement 2026-07-04, "good telemetry")**: today cost/tokens exist only as one project-wide aggregate stat — no per-goal or per-action breakdown, no turn-level activity stream. Build this alongside #3, not after — Decision's confidence/risk thresholds are exactly the kind of signal a real telemetry stream should be recording and exposing, so building them together avoids instrumenting the same code path twice. Distinct from Audit (§7 of TA-CONSTITUTION.md's compliance trail) — Meter is the observability stream a cost dashboard or per-goal report reads from.
4. **0.17.x or immediate fast-follow** — CLI verb-set consolidation (§5), shipped with a deprecation/alias window, not a hard cutover.
5. **After #1–3 land** — Studio IA redesign around the now-clean concept set. Building new UI against a still-fragmented backend just recreates the sprawl; sequence the backend fixes first.
6. **0.18+, kept explicitly in the plan** — true concurrent sub-goal execution (already tracked at v0.13.16, no change needed, just don't lose it) and the knowledge-hierarchy/persona capability (§4) — needs a model decision (graphify-style clustering vs. leveled-context vs. novel ancestry design) before design work starts, but the plug-in point in `PersonaConfig` is already known and ready whenever that happens.
7. **Later, kept explicitly in the plan** — the community contribution security review workflow (§8). Build after the Plugin category (§2.2) and community constitution rules (§2.5) exist, since it governs both.

This document should be the reference the next PLAN.md phase additions for items 1–5 cite back to, rather than re-deriving the rationale each time.

---

## 8. Community Contribution Security Review Workflow — flagged for later, kept in the plan

Not designed yet — noted 2026-07-04 so it isn't lost, same treatment as §4. Governs any community-contributed Plugin (§2.2) or constitution rule (§2.5) once those categories exist.

**The shape of the workflow, as specified:**

1. **Initial security review before trust.** A community contribution (a Plugin per §2.2's category 1, or a constitution rule per §2.5's `gendigitalinc/sage`-modeled pipeline) goes through a security review before it's activated/trusted in a project. Findings are reported visibly — not a silent pass/fail (per Constitution §1.4, Observable & Actionable).
2. **Re-review on every update, not just at submission.** An update to an already-trusted contribution re-triggers the review cycle. Trust is not permanent once granted.
3. **Andon cord** (the manufacturing/lean concept — any worker can stop the line the moment they spot a problem, rather than waiting for it to reach the end): any community member can report a concern about a contribution at any time, not just maintainers or during a formal review window.
4. **Report → admin yank.** A community report triggers an admin-initiated "yank" — suspending the contribution from active/trusted use pending investigation. This is a pull/quarantine action, not an immediate permanent removal; the contribution comes back if the report doesn't hold up.
5. **Supervisor + security team review.** The actual investigation of a yanked contribution is done jointly by TA's own supervisor agent and a "security team" role, who produce advisory findings — reusing the existing `supervisor_review.rs` pipeline (per §1.7, Reuse Before Reinventing) rather than a new parallel review system. "Security team" as a role is exactly the kind of thing §2.3's TeamRole-as-data-defined fix (rather than a fixed 6-value enum with no security-team slot) needs to exist for before this can be assigned cleanly.

**Where this plugs into what's already documented**: reuses the Plugin category (§2.2), the community constitution rules design modeled on `gendigitalinc/sage`'s `threats/*.yaml` + `pre-release`-branch pipeline (§2.5), the supervisor review pipeline (§2.1, §7 item 3b), and data-defined roles (§2.3/§1.6) for the "security team" assignment. No new mechanism needs inventing except the andon-cord report/yank state machine itself.

---

## 9. The Write → Review → Decision → Commit/Reject Graph — Unifying Model

Answers a direct question: is TA's core workflow a node-graph, where a Review connects to a Decision that routes to a Commit or Reject path, with VCS/DB/Social/etc. as pluggable Commit implementations? **Yes — and TA has already independently built this same four-stage shape at least three times, which is itself a live §1.7 violation worth fixing as part of adopting the unified model, not a hypothetical one.**

**Evidence, verified against code**:
- `DraftStatus` (`crates/ta-changeset/src/draft_package.rs`) — `Draft → PendingReview → Approved{..}/Denied{..} → Applied{..}/Superseded{..}` — the richest instantiation, for files/VCS.
- `social_supervisor_check()` / `SocialSupervisorResult` (`crates/ta-submit/src/social_adapter.rs`) — confidence + flag-substring gate for social posts.
- `supervisor_check()` / `SupervisorResult` (`apps/ta-cli/src/commands/email_manager.rs`) — structurally near-identical confidence gate for email replies, independently reimplemented.
- DB proxy (`DbProxyPlugin`) has no equivalent gate at all yet (consistent with yesterday's finding it needs redesign). Release adapters aren't built yet either.

**Five node roles** (not six — Escalate is not a new node, see below):

| Node | Role | Reuse instead of reinventing |
|---|---|---|
| **Write** | Agent proposes a change, staged only, never touches the live target | Universal already (staging overlay) |
| **Review** | Evaluates a Write, produces verdict + confidence + risk | `SupervisorVerdict` + the new `confidence` field (§2.1 gap). A Review MAY be a **Consensus** of parallel sub-reviews — reuse the already-built Raft/Paxos/Weighted engine (§2.6) as one Review implementation, not a new concept. |
| **Decision** | Takes Review output(s), applies a threshold policy, routes to an outgoing edge (Commit / Reject / Rework-back-to-Write / Escalate) | `social_supervisor_check()` is already a working, application-agnostic template for this node's core logic — lift it out rather than writing it a fourth time. |
| **Commit** | The one privileged, TA-mediated action that materializes a Write | `SourceAdapter::commit()`+`push()` (VCS, `crates/ta-submit/src/adapter.rs`) is the most complete existing example — generalize its shape, don't invent a new one. |
| **Reject** | Discards the Write, audited | §7.4 Terminal Transition Auditing already covers this. |

Escalate is not a sixth node — it's what Decision routes to when it can't confidently resolve, landing on the already-existing `HumanGate` workflow step (§2.6). Once a human resolves it, that's a fresh Decision input feeding back into the graph.

**The per-Application ("adapter") contract**, generalizing `SourceAdapter`'s already-existing `prepare()`/`commit()`/`push()` shape past VCS to every application:
- `write(proposed_change) -> WriteHandle` — stage only.
- `describe_for_review(WriteHandle) -> ReviewableRepresentation` — unified diff / row-level before-after / post preview / message preview, per domain.
- `commit(WriteHandle) -> CommitResult` — callable ONLY from TA's daemon-mediated pathway, never the agent process directly. This generalizes an *existing* constitutional invariant (§9.2 Daemon Mediates All Writes, currently stated in VCS-flavored terms) to every application, not a new rule.
- `reject(WriteHandle)` — discard, no live effect ever occurred.

**Correction (2026-07-04) — Social is not a special case, don't fork the contract for it.** `ExternalSocialAdapter` currently has no `publish` method — "TA never publishes social media posts on behalf of the user" is an explicit comment in the code. The first pass at this doc read that as a permanent architectural exception needing its own `CommitCapability::Delegated` vs `TaMediated` distinction. That's wrong and over-engineered. `publish` **is** `Commit()` for the social endpoint, invoked exactly as generically as `ta draft apply` already invokes VCS's `commit()`+`push()` or DB's mutation-apply — same pathway, same shape, every endpoint. What's endpoint-specific is only the *function body* (git commit+push vs. DB mutation-apply vs. social publish vs. email send), never the invocation mechanism.

The reason today's social plugin has no `publish()` is a **policy default, not an architectural fork**: `post` is already in `APPROVAL_REQUIRED_VERBS` (§9 above) alongside `apply`/`commit`/`send` — social posting is (and should stay) conservative-by-default, gated behind human approval or a very deliberately-configured `AdvisorSecurity::Auto`. The fix is to add a real `commit()`/`publish()` method to the social adapter contract, gated by the same policy layer every other endpoint already uses — not to invent a second class of "endpoints that don't really commit." One uniform contract, one uniform invocation path, policy (not architecture) decides how cautious each endpoint is by default.

**A concrete bug this generalization fixes**: `APPROVAL_REQUIRED_VERBS: &[&str] = &["apply", "commit", "send", "post"]` (`crates/ta-policy/src/engine.rs:83`) is a hardcoded array — the exact §1.6 pattern, in the single most security-critical location in the system. Every new application's commit-equivalent verb currently requires manually appending a string here. Once Commit is a first-class contract (per §1.6, itself data/trait-defined, not a closed enum), any application implementing it is automatically approval-required.

**How this relates to the 4 extensibility categories (§2.2) — orthogonal, not competing**: the 4 categories answer *how* an implementation is built (external process / in-process / long-running / declarative config). This graph answers *what role* a node plays in the workflow. They compose: "VCS Commit" is a Backend today (built-in git) with Plugin variants (svn/perforce, external) — same graph role, different build mechanism. One correction surfaced while vetting: **Channel-Listener plugins (Discord/Slack/email-delivery) are not Commit implementations at all** — they deliver questions to humans (feed the Escalate/HumanGate path), an unrelated concern that happens to share the plugin-daemon infrastructure. Don't conflate the two.

**Composability answer**: the simplest system is a DAG (not a fully general graph language — sage-lore's Scroll Assembly-level generality isn't justified yet, per the earlier sage-lore review) with explicit labeled back-edges for rework, defined inside `workflow.toml` (§3's mapping tree — Workflow Type already lives there), using the existing `WorkflowStepKind` as the node-kind vocabulary, extended with generic Write/Review/Decision/Commit/Reject roles and made data-defined per §1.6 rather than a further-closed Rust enum.

---

## 10. Worked Examples: Common Workflows in TA's Action Terms

Every workflow below is the same graph — Goal → **Write** → **Review** → **Decision** → **Commit** / **Reject** / (rework loop back to Write) — with different components filling each role. Grouped to show the full spread: the most mature endpoint (VCS), the least mature (DB), one with a real policy gate but no publish yet (Social), and Review-as-Consensus already shipped (multi-agent code review).

### 10.1 Standard code-implementation goal (canonical, most mature)
e.g. `ta run "implement v0.17.0.12.11" --phase v0.17.0.12.11`

| Graph role | Concrete translation | Real code today |
|---|---|---|
| Write | Agent edits files inside `.ta/staging/<goal-id>/`, diffed against source | staging overlay, `ta draft build` |
| Review | Supervisor AI review produces a verdict (optionally a Consensus of parallel reviewers, see 10.4) | `crates/ta-changeset/src/supervisor_review.rs` |
| Decision | Verdict alone today (Pass/Warn/Block) — no confidence/risk threshold yet (§2.1 gap) | `SupervisorVerdict` |
| Commit | `SourceAdapter::commit()` + `push()` — feature branch, opens a PR | `crates/ta-submit/src/adapter.rs` |
| Reject | `ta draft deny` — reason recorded | `DraftStatus::Denied` |
| Rework | `DraftStatus::UnderReview → Running` — same staging, agent retries | goal state machine |

CLI verbs in play: `ta run` (Write) → `ta draft view` (inspect Review) → `ta draft apply` (fire Commit) / `ta draft deny` (Reject).

### 10.2 DB migration/mutation goal (least mature endpoint — Review/Decision don't exist yet)
e.g. agent proposes a schema change or bulk data update

| Graph role | Concrete translation | Real code today |
|---|---|---|
| Write | Agent's mutations captured as a log in the DB proxy's staging, never touching the live DB | `DbProxyPlugin` trait (`crates/ta-db-proxy/src/plugin.rs`) — needs migration to the Plugin category per yesterday's redesign |
| Review | **Missing.** No confidence/risk gate exists for DB mutations at all, unlike social/email | gap |
| Decision | **Missing.** Would reuse the generalized Decision logic lifted from `social_supervisor_check()` (§9), gated on row-count/schema-drop thresholds already spec'd (`[[rules.warn]]`/`[[rules.block]]`) but never implemented | PLAN.md v0.17.1 spec, not built |
| Commit | `apply_mutation()` — replays the captured log against the real DB | `DbProxyPlugin::apply_mutation` (trait signature exists) |
| Reject | Discard mutation log, drop replication slot | spec'd, not fully implemented |

**This is the clearest illustration that DB is genuinely the least mature endpoint** — Write and Commit exist as trait methods, but the Review/Decision gating that VCS gets "for free" via supervisor review, and that social/email get via `min_confidence`, doesn't exist here at all.

### 10.3 Social content posting goal (real policy gate, no `Commit` implementation yet)
e.g. a content-creator goal produces a video + caption for posting

| Graph role | Concrete translation | Real code today |
|---|---|---|
| Write | `ExternalSocialAdapter::create_draft()` / `create_scheduled()` — stages on the platform's own draft/schedule mechanism | `crates/ta-submit/src/social_adapter.rs` |
| Review | `social_supervisor_check()` — confidence + flagged-substring + blocked-client-name checks | same file, already built |
| Decision | `confidence >= min_confidence` (default 0.80) and no flags → pass; else → human review queue | `SocialSupervisorConfig` |
| Commit | **`publish()` — is Commit() for this endpoint** (per the correction above), invoked exactly as generically as VCS's commit+push once it exists. Doesn't exist as a method yet; `post` sitting in `APPROVAL_REQUIRED_VERBS` means a human does the actual posting today, by policy, not by architectural necessity | gap: add `publish()`, gated the same way every other endpoint is |
| Reject | Draft discarded on the platform, never scheduled/posted | implicit today |

### 10.4 Multi-agent consensus code review (already built, real prior art for Review-as-Consensus)
e.g. `code-review-consensus.toml` — 4 parallel specialist reviewers (architect/security/principal/PM)

| Graph role | Concrete translation | Real code today |
|---|---|---|
| Write | Same as 10.1 | staging overlay |
| Review | 4 parallel reviews (architect/security/principal/PM personas), each scoring independently | `code-review-consensus.toml` template |
| Decision | `ConsensusAlgorithm` (Raft default / Paxos / Weighted) aggregates the 4 scores into one readiness verdict | `crates/ta-workflow/src/consensus/mod.rs` (v0.15.15, done, 37 tests) |
| Commit / Reject | Same as 10.1 once consensus resolves | VCS |

Proof that "Review can be a Consensus" (§9) isn't hypothetical — it's already shipped, just not yet generalized so any workflow's Review node can optionally be a Consensus rather than only this one built-in template.

---

## 11. The Reduced Instruction Set

Two layers, meant to be the same vocabulary at different altitudes — the CLI verbs are the user-facing names for the underlying graph actions, not a separate language.

**Internal graph semantics (§9)** — 5 actions, plus 2 reused (not new) mechanisms:
- **Write, Review, Decision, Commit, Reject**
- Reused: **Consensus** (one Review implementation, already built, §2.6/10.4) — **HumanGate** (a Decision's escalation target, already built, §2.6)

**User-facing CLI verbs (§5)** — 10 verbs, nouns as subjects:
- create, list, show, update, remove, run, approve, deny, apply, check, sync

**The mapping between them:**

| CLI verb | Graph action it triggers |
|---|---|
| `ta run <goal>` | Starts a Write |
| `ta check <draft>` | Invokes/inspects a Review |
| `ta approve` / `ta deny` | Supplies a human Decision output (HumanGate resolution) |
| `ta apply` | Fires Commit |
| *(implicit, on deny + retry)* | Reject → Rework loop back to Write |
| `ta create/list/show/update/remove/sync <noun>` | CRUD on the entities the graph is built from (goals, drafts, teams, personas, workflows, plugins) — configuration, not graph actions |

**The reduced instruction set, stated plainly**: 5 internal graph actions. 5 of the 10 CLI verbs (`run`/`check`/`approve`/`deny`/`apply`) are their direct user-facing expression. The other 5 (`create`/`list`/`show`/`update`/`remove`/`sync`) are generic CRUD any config-driven system needs regardless of domain — not TA-specific at all. That's the whole surface: **one small graph vocabulary for what happens to a goal, one small CRUD vocabulary for managing the configuration that shapes it.**

---

## 12. Input Connectors — the Mirror Direction, Mostly Unbuilt

§9-11 modeled the *outbound* direction (agent proposes → world). Input connectors (Gmail, Slack, DB reads, web fetch, GitHub/Forgejo issues) are *inbound*, and the graph mirrors cleanly — same shape, opposite direction, mostly not built yet:

**Fetch → Scan → Decision → Admit / Block**

| Role | What it does | Mirrors | Real code today |
|---|---|---|---|
| **Fetch** | Connector retrieves external data | Write | `ta-connectors/*` traits exist but are oriented around writing patches, not a distinct "fetch" verb — not cleanly named yet |
| **Scan** | Validate fetched content before it crosses into agent context | Review | **Only path-traversal is real** (`ta-policy/src/engine.rs`, step 1 of 6 in the evaluation order). Secret-scan exists only on the output side (`ta-changeset`) — nothing scans fetched content. Prompt-injection scanning doesn't exist in TA at all — `gendigitalinc/sage` has a real, working two-tier (heuristic + ML) implementation, `PreToolUse`-scoped to `WebFetch`, worth using as the model rather than inventing one |
| **Decision** | Same node type as §9, reused | Decision | doesn't exist for this purpose yet |
| **Admit** | Content crosses the trust boundary into agent context | Commit | implicit today (nothing blocks it) |
| **Block** | Content withheld, agent never sees it, audited | Reject | doesn't exist |

**Security is a signal source feeding Scan (or Review), not a separate node** — matching `gendigitalinc/sage`'s real decision pipeline exactly: independent sources (heuristic rules, URL reputation, package checks, AMSI, PI-check) each emit `(confidence, category)`, merged into one verdict, rather than each being its own graph stage. Output-side signal sources today: `ApprovalRule` pattern checks, secret-scan on draft artifacts, the §1.6/§1.7 lint checks. Input-side: only the path-traversal guard is real; secret-scan-on-fetch and PI-scan are gaps.

**One more level, distinct from both**: §8's community contribution review (andon cord) reviews the *connector/adapter code itself*, once, at contribution/update time — not a per-instance runtime gate. Scan/Review run on every goal; §8 runs on every plugin update. Related, not the same mechanism.

**How auto-approval extends to inputs**: the same threshold-gate mechanism from §2.1/§9 (lifted from `social_supervisor_check`), fed input-side signals instead of output-side ones — auto-Admit if Scan is clean **and** the fetch matches something already declared. The one genuinely new wrinkle: `AccessConstitution`'s `access: Vec<ConstitutionEntry>` (URI pattern + intent, `crates/ta-policy/src/constitution.rs`) already exists, but only as a **post-hoc drift check at `ta draft build` time** — it doesn't gate anything live today. Using it as a *live* confidence signal at fetch-time (matches declared pattern → strong "expected" signal → auto-admit candidate) is a natural extension, not something already wired that way. A fetch to an undeclared URI should never auto-admit regardless of how clean the content scan looks — Default-Deny (§1.2) wins over a clean scan.

---

## 13. The Three-Tier Request Model — Triggers / Routing Brain / Back Office — flagged for later, kept in the plan

Raised 2026-07-05: is TA a 3-tier system — (1) incoming request triggers, (2) a "brain" that routes/prioritizes and picks the workload + privilege tier, (3) a supervised back-office execution layer with auto-approve and escalation? Answer, mapped onto what's built:

- **Tier 1 (triggers/intake) — essentially absent.** Every goal today starts from an explicit call (`ta run`, MCP `ta_goal_start`). No first-class trigger abstraction exists (webhook → goal, schedule → goal, inbound email → goal). §12's Fetch/Scan/Admit/Block governs content entering an *already-running* goal, not goal creation itself. This is the one tier where commercial automation platforms (Zapier/Make/n8n) are strong and TA is at zero.
- **Tier 2 (the "brain": workload classification, prioritization, agent + privilege selection) — confirmed missing, same gap as §3's mapping tree.** `--team`/`--persona`/`--agent` are always explicit today; nothing classifies an incoming request's workload type and derives both the right team/persona *and* the right security tier from it. The agent-switching work (v0.17.0.12.12/12.13) is only the agent-selection facet of this tier, not full workload classification + prioritization + privilege determination.
- **Tier 3 (back office: supervised execution, auto-approve, escalation) — TA's strongest tier by far.** This is §9's Write/Review/Decision/Commit/Reject graph, `AdvisorSecurity` auto-approve (once wired to real thresholds, v0.17.0.12.15), `HumanGate` escalation, hash-chained Audit. Commercial automation tools are nearly inverted here — actions fire directly, with at best one optional manual-approval node, no systemic staged-review/audit model.
- **"Team coordinator" is likely the same missing component as the tier-2 brain, not a fourth one.** Today TA has *Supervisor* (a per-draft quality gate, not persistent) and *Advisor* (a human-facing chat assistant) — neither watches an incoming-request queue, classifies/prioritizes it, and assigns team/persona/security tier across concurrent goals the way a team lead would. Building the tier-2 brain and building "a coordinator teammate that operates outside the core Write/Review/Decision loop" are very likely the same design problem.

**External comparison** (verified via web search, not assumed):
- **AgenC** (agenc.tech) — real, but a different problem space: a Solana-based agent marketplace toolkit (escrowed gig work between agents, on-chain identity/capability bitmasks). Structurally validates TA's core thesis, though — its "preview-first execution, policy-gated mutation tools, human approval before settlement" is a genuine parallel to TA's staging→Commit gate, just applied to payments/escrow instead of internal engineering goals.
- **Alembic** — NOT an agent orchestration platform; it's a marketing-intelligence/causal-AI product for CMOs. Not a comparable system; flagged so this doesn't get treated as prior art by mistake in a future session.
- **Zapier / Make / n8n** — inverse of TA's current strengths/gaps: deep tier-1 (trigger/connector catalogs) and lightweight deterministic tier-2 (if/then branching, not AI-driven workload classification), essentially absent tier-3 (no systemic staged-review/audit model — approval, if present at all, is one optional manual node).

**Status**: not scoped into PLAN.md phases yet — user chose to finish the already-queued v0.17.0.12.11–12.17 overhaul first (2026-07-05) rather than design this in parallel. Next step when picked back up: scope a tier-1 trigger abstraction, extend the mapping-tree work (§3, v0.17.0.12.13) to cover workload classification + privilege derivation (not just agent selection), and decide whether the "team coordinator" is a new persistent role or a capability of the existing Advisor.
