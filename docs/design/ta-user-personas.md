# TA User Personas: What They Want, What They Type, What's Invisible

**Status**: design reference, 2026-07-08. Companion to `ta-cli-verb-reference.md`. Read `docs/guides/what-is-ta.md` first for the plain-language model this doc assumes.

**Structure per persona**: what they want → what they explicitly type (the small human-facing verb surface) → what happens implicitly (handled by the Advisor or chained by a workflow definition, never typed by hand).

---

## 1. The Engineer

**Wants**: implement a feature, fix a bug, keep the codebase moving, without babysitting every step.

**Explicit**:
```
ta run "fix the auth token refresh bug"
ta status
ta show draft <id>
ta approve draft <id>
```

**Implicit** (Advisor/workflow, never typed): which agent/model runs the goal (resolved through the persona/workload/team tiers — v0.17.0.12.13's `Switch` resolution, or `auto` handing it to the supervisor); whether the draft needs a human at all (auto-approved if confidence/risk clears the configured threshold, v0.17.0.12.15); which team member's persona applies; the deprecation/consolidation bookkeeping.

**What they never think about**: plugin manifests, policy grants, team.toml syntax. If asked, they'd say "I told it what to build and it handled the rest."

---

## 2. The PM

**Wants**: know what's in flight, what's blocked, what shipped this week — without reading code.

**Explicit**:
```
ta status
ta list goal
ta show goal <id>
ta advisor ask "what shipped this week?"
```

**Implicit**: the phase summary (decisions made, diffs, risk flags) is compiled by the Advisor, not assembled by hand; escalations route to the PM only when a Decision genuinely can't resolve on its own (`HumanGate`), not on every action.

**What they never think about**: the underlying verb/noun structure at all — `ta advisor ask` in plain language is the only surface a PM should ever need.

---

## 3. The Artist / Creative

**Wants**: run a render job, iterate on a look, without learning a CLI at all.

**Explicit** (ideally via Studio, not terminal):
```
ta run "render the turntable with the new lighting preset"
ta show draft <id>       # to see the rendered output, not a code diff
```

**Implicit**: which render backend/connector handles the job (Unity/Unreal/ComfyUI — resolved from project config, never chosen by hand); asset staging and cleanup; whether output needs review before it's "real" (same Draft/apply model as code, just with image/video artifacts instead of file diffs).

**Gap noted**: Draft review today is file-path-only for image/video artifacts — no visual preview. This persona is the direct motivator for that fix (already tracked, v0.17.0.12.17 Studio IA).

---

## 4. The Exec Sponsor (approving TA for team-wide use)

**Wants**: a yes/no on whether this is safe to roll out to employees, in about five minutes, without reading code or CLI docs.

**Explicit**: none — this persona should never need to type a command. Their entire interaction is reading `docs/guides/what-is-ta.md` (or a Studio dashboard summary) and asking the Advisor plain-language questions: *"what happens if an agent tries something dangerous?"* *"who approves what actually goes live?"*

**Implicit**: everything — the staged-review model, the audit trail, the policy engine's default-deny posture are the actual answer to their question, but they should never need to know the mechanism names to trust the answer.

**This is the real test of "simple and clear"**: if this persona ever needs to see a verb+noun command to make their decision, the CLI-reduction work hasn't succeeded for its actual target audience.

---

## The pattern across all four

Every persona's *explicit* surface is a handful of commands (`run`, `status`, `show`, `approve`/`deny`, and increasingly just natural language to the Advisor). Everything else — agent selection, policy checks, plugin dispatch, team/persona resolution — is implicit, chained by a workflow definition or decided by the Advisor. The full ~70-command surface exists for that implicit layer to compose against, not for a person to memorize. See §4 of `ta-cli-verb-reference.md` for why that's a deliberate split, not a shortfall.
