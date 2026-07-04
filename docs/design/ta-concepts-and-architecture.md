# TA Concepts & Architecture: Current State, Right Abstractions, and Refactor Path

**Status**: Living design document review, first written 2026-07-04
**Purpose**: A single, honest inventory of every core concept TA has, what abstraction each one *should* use, whether it currently does, and a sequenced plan to close the gaps. Written in response to a direct architecture review request after the 0.17.0.12.x cleanup arc and the sage-lore/sage/graphify research (see [`sage-lore-review.md`](sage-lore-review.md)).

**How to use this doc**: it's the reference for (a) what to refactor and when, (b) why Studio's UI is overly complex, (c) why the CLI command surface is overly complex, and (d) what "smallest clean verb set" TA should converge on. Update it as concepts change — don't let it go stale like the old `docs/design/sage-lore-review.md` reference did before this session existed.

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

Given "ship 0.17 soon" and everything confirmed above, in order:

1. **Now / low-risk** — convert `TeamRole` from a closed enum to a data-defined role type. Confirmed small blast radius (52 usages, 6 files), and it's the precondition for everything else in this list. Also rename the "Adapter" naming collision (§2.2) while touching related code.
2. **Now / already in motion** — unify the plugin/adapter/connector patterns into the 4-category model (§2.2), migrating `EXTERNAL_TOOLS`, `DbProxyPlugin`, and the planned `ReleaseAdapter` onto the "Plugin" category before any of them get more implementation baked in on the wrong foundation. This aligns with and should absorb yesterday's DB-proxy-redesign and adapter-unification findings rather than running as a separate effort.
3. **0.17.x, new phase** — build modes 2 and 3 of the goal → workflow-type → team/role/persona → security mapping tree (§3): workflow-defined defaults (extending `workflow.toml`, not a new registry) and supervisor-recommended/auto-select resolution (extending the existing `AdvisorSecurity` tri-state and `ta-advisor`'s intent classifier). This is the concrete, highest-leverage piece for "autonomous workflows to first class" — extend existing mechanisms (workflow.toml, AdvisorSecurity, the consensus engine from §2.6) rather than inventing new ones.
4. **0.17.x or immediate fast-follow** — CLI verb-set consolidation (§5), shipped with a deprecation/alias window, not a hard cutover.
5. **After #1–3 land** — Studio IA redesign around the now-clean concept set. Building new UI against a still-fragmented backend just recreates the sprawl; sequence the backend fixes first.
6. **0.18+, kept explicitly in the plan** — true concurrent sub-goal execution (already tracked at v0.13.16, no change needed, just don't lose it) and the knowledge-hierarchy/persona capability (§4) — needs a model decision (graphify-style clustering vs. leveled-context vs. novel ancestry design) before design work starts, but the plug-in point in `PersonaConfig` is already known and ready whenever that happens.

This document should be the reference the next PLAN.md phase additions for items 1–5 cite back to, rather than re-deriving the rationale each time.
