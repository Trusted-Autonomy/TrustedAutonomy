# sage-lore & sage Design Review

**Status:** Decision document for PLAN.md phase [v0.17.3.1](../../PLAN.md) — sage-lore Design Review
**Date:** 2026-07-03
**Recommendation:** Complement, not integrate. See [Decision](#decision) below.

## Sources

This review covers two distinct, unaffiliated-but-similarly-named projects, both surfaced during today's plan review:

- **[kwg/sage-lore](https://github.com/kwg/sage-lore)** — an LLM orchestration engine (the actual subject of PLAN.md's original v0.17.3.1 phase text).
- **[gendigitalinc/sage](https://github.com/gendigitalinc/sage)** — an unrelated project, also named "Sage," that is a real-time Agent Detection & Response (ADR) layer for AI coding assistants. Relevant here specifically for its tool-call-guarding model and its **community threat-rule contribution pipeline**, which is a useful design reference for TA's own constitution/policy system.

The original phase text referenced "sage-lore's 20 Scroll primitives" and a "scan-once security model" without ever linking to a source. Neither could be found by web search alone (three separate targeted searches turned up nothing) — the correct source was supplied directly by the project owner after the fact. Corrected: sage-lore ships **21** primitives as of `v1.0.0-beta.1.2`, not 20.

---

## 1. Capability Audit — sage-lore vs. TA Concepts

sage-lore is a **deterministic workflow orchestration engine for LLM tasks**. Its central idea: keep LLMs doing only targeted, single-task generation, and let a typed, statically-checked workflow language ("Scroll Assembly") own all the control flow, data wiring, and non-generative work around them.

| sage-lore concept | TA concept | Overlap / gap |
|---|---|---|
| **Scroll** (a `.scroll` file — static, typed workflow definition, the sole trust/execution boundary) | `workflow.toml` step types (`agent_review`, `pr_monitor`, `plan_check`, `sync_build`), `ta plan build --autonomous --workflow serial-phases/swarm` | TA's workflow steps are a small fixed Rust enum, not a general-purpose typed DSL. sage-lore's Scroll Assembly is a real language (formal `.pest` grammar, LSP, VS Code/Vim/Kate/Neovim editor support) for composing *arbitrary* multi-step LLM+deterministic pipelines. |
| **21 primitives** (elaborate/distill/split/merge/validate/convert; fs/vcs/test/platform/run; invoke/parallel/consensus/concurrent; branch/loop/aggregate/set; secure; string) | TA has no equivalent primitive library — goal execution is "spawn one Claude Code agent, let it use its own tools freely inside a staged sandbox" | This is the biggest real gap. TA governs the *boundary* (staging, diff, policy, draft review) but has no structured way to compose *what happens inside* a goal beyond a single freeform agent session. sage-lore's `parallel`/`consensus`/`concurrent` primitives are a more disciplined version of what today's session hand-rolled all day via ad-hoc `Agent`/fork calls. |
| **Scan-once security model**: a scroll is content-hash cached after its first security scan; once scanned, trusted for subsequent runs. "Lock the doors, not the walls" — the scroll boundary *is* the security perimeter, not each primitive call. | TA's staging overlay + draft/apply gate + `constitution.toml` `[[rules.warn/block]]` — every *action* inside a goal is unconstrained until the resulting diff is reviewed at draft time. | **Genuinely different layer, not redundant.** sage-lore constrains what workflow logic *can even be expressed* before it runs (input-side). TA constrains what a *result* is allowed to contain before it's applied (output-side). See §2. |
| **Three-tier config hierarchy** (Corp → User → Project, most-specific-wins) | TA has an analogous project-vs-user split (`.ta/` vs `~/.config/ta/`) but no explicit "Corp" tier | Minor — a real but small gap for enterprise/SA deployments (see PLAN.md v0.18.0 SA credential store track). |
| **Adapter scrolls** (`chunk-from-forgejo.scroll`, platform-specific, swappable against a `requires:`/`provides:` contract) | TA's own plugin-adapter model (VCS/memory/messaging/social/channel plugins, JSON-over-stdio, `plugin.toml` manifests) — see today's separate v0.17.1/v0.17.4 adapter-unification research | Same idea, different implementation. Worth cross-referencing rather than reinventing if sage-lore's contract pattern (`requires:`/`provides:` typed I/O) is cleaner than TA's current ad-hoc plugin method signatures. |

## 2. Security Model Comparison

**Not redundant — additive, at different points in the pipeline:**

- sage-lore's scan-once model asks: *"is this workflow definition itself safe to execute at all?"* — evaluated once, before any run, against the static Scroll file (secret-pattern detection, structural validation). It's an **input-constraint** check.
- TA's staging + draft + constitution model asks: *"is the concrete set of file changes this run actually produced safe to apply?"* — evaluated after execution, against the real diff. It's an **output-constraint** check.

A workflow could pass sage-lore's scan (well-formed, no embedded secrets, uses only declared primitives) and still produce a dangerous diff (e.g., an `invoke` primitive whose LLM-generated output happens to `DROP TABLE`) — TA's constitution rules are exactly the layer that catches that, which sage-lore does not claim to do. Conversely, TA has no equivalent of "verify the workflow logic itself, before running it, doesn't contain smuggled instructions" — that's closer to what **sage** (gendigitalinc) does in real time.

**sage (gendigitalinc)'s model is a third, complementary layer again**: it intercepts individual tool calls (`Bash`, `Write`, `Edit`, `WebFetch`) *during* execution — not before (like sage-lore's scan) and not after (like TA's draft review), but at the moment of the call itself. Its [decision pipeline](https://github.com/gendigitalinc/sage/blob/main/docs/decision-pipeline.md) merges signals from several independent sources — YAML heuristic rules, URL reputation, npm/PyPI supply-chain checks, a fine-tuned prompt-injection ML model, Windows AMSI — into a single `(confidence, category)` per signal, then applies a **fail-open**, confidence-threshold policy (`allow`/`ask`/`deny`) tunable by a `paranoid`/`balanced`/`relaxed` sensitivity setting. All three layers (sage's per-call interception, sage-lore's per-workflow scan, TA's per-draft review) can coexist without conflict — they gate different moments in the same pipeline.

## 3. Community Additions — the part directly relevant to TA's constitution model

This is the strongest, most concrete design input from this review, and the reason [gendigitalinc/sage](https://github.com/gendigitalinc/sage) is included here even though it's an unrelated project from sage-lore.

Sage's threat detection rules live as individual YAML files under `threats/*.yaml`, and the project has a genuinely working **community rule contribution pipeline** (see [CONTRIBUTING.md](https://github.com/gendigitalinc/sage/blob/main/CONTRIBUTING.md)):

- `main` is the release/distribution branch users actually install from. **No external PRs target it directly.**
- `pre-release` is the contribution branch. All external threat-rule PRs land here first.
- Contributed rules go through a **security review cycle** before being synced back to `main` in controlled releases.
- Each rule requires an `author` field for attribution, and is licensed separately (Detection Rule License 1.1) from the core source (Apache 2.0) — an explicit, deliberate split between "code contributions" and "policy/rule contributions."
- The [decision pipeline doc](https://github.com/gendigitalinc/sage/blob/main/docs/decision-pipeline.md) documents an explicit, versioned contract for what a signal source must emit (`confidence`, `category`, `severity`, `source`, `reason`, `artifact`) — so a community-contributed rule has a well-defined interface to the policy engine, not an ad-hoc one.

**This maps directly onto a gap TA doesn't currently have a design for: community-contributed constitution rules.** Today, `constitution.toml`'s `[[rules.warn]]`/`[[rules.block]]` entries are authored per-project by whoever owns the repo — there's no equivalent of "install a community-maintained rule pack" the way `.ta/db-adapters.toml` or `.ta/community-resources.toml` let a project pull in community adapters/resources. Sage's model is a ready-made template for what that could look like:

- A `threats/`-equivalent directory of individually-authored, individually-licensed constitution rule files.
- A staged contribution branch + security review step before a community rule reaches a project's live `constitution.toml`.
- A typed signal/confidence contract so a community rule author knows exactly what interface to implement, rather than TA needing bespoke code for every new rule category.

This is worth a dedicated follow-on design (see [Decision](#decision)) rather than folding into the existing DB-adapter/release-adapter unification work already proposed today — constitution rules are policy, not an executable adapter, and the review/trust model (sage's `pre-release` → security-audit → `main` sync) is meaningfully different from "install and run a third-party binary."

## 4. Integration Options

**(a) sage-lore as an orchestrator that drives `ta run` goals via CLI** — rejected. Would mean TA becomes a primitive *inside* someone else's orchestration loop rather than the other way around; inverts TA's own "agent works inside TA's staging, TA is invisible to the agent" thesis.

**(b) Scroll DSL as a workflow definition language inside `.ta/workflows/`** — the interesting option, but premature. TA's own `workflow.toml` step-type system (`agent_review`/`pr_monitor`/`plan_check`/`sync_build`, serial-phases/swarm) already covers today's actual needs reasonably well; adopting a full external DSL + parser + LSP dependency for marginal expressiveness gains isn't justified without a concrete use case that TA's existing step types can't express. Worth revisiting once/if TA's own multi-agent parallelism (currently deferred, see today's gap analysis) matures far enough that composing primitives like sage-lore's `parallel`/`consensus`/`concurrent` becomes a real, felt need rather than a hypothetical one.

**(c) No integration of sage-lore itself; adopt sage's community-rule-contribution pattern as a design template for TA's constitution system** — **recommended**, see Decision.

## Decision

**Complement, don't integrate sage-lore.** Its Scroll DSL and primitive library solve a real problem (composable, typed, deterministic multi-step LLM orchestration) that TA does not currently solve — but TA's own `workflow.toml` step-type system covers today's needs adequately, and adopting an external DSL is a large dependency for a gap that isn't acutely felt yet. Revisit if/when TA's multi-agent parallelism work matures and a concrete composition need that TA's own primitives can't express actually shows up.

**Do pursue a follow-on design** for community-contributed constitution rules, directly modeled on [gendigitalinc/sage](https://github.com/gendigitalinc/sage)'s `threats/*.yaml` + `pre-release`-branch-review pipeline: a `.ta/constitution-rules/` (or similar) directory of individually-authored, individually-attributed rule files, a defined signal contract (confidence/category/severity/source/reason), and a staged review step before a community rule reaches a project's live `constitution.toml`. This is the concrete "constitution amendment via community additions" capability referenced when this review was requested — not covered by today's separate DB-adapter/release-adapter protocol unification work, since constitution rules are policy definitions, not executable third-party binaries, and need their own trust/review model.

**Follow-on plan phase**: a new phase (not yet numbered) for "Community Constitution Rules" — scoped to: rule file format + signal contract, `.ta/constitution-rules/` discovery (VCS-shared, same pattern as `.ta/community-resources.toml`), and a review/trust step before a contributed rule activates. Should be sequenced after the adapter-protocol-unification phase from today's other research (v0.17.1/v0.17.4), since both share the "community contribution + review before trust" shape and should likely reuse the same underlying mechanism rather than building two parallel review pipelines.
