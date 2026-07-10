# TA CLI: Verb Reference & Surface-Area Split

**Status**: design reference, 2026-07-08. Current numbers verified against `apps/ta-cli/src/main.rs` and `apps/ta-cli/src/commands/verb.rs` on the v0.17.0.12.16 branch (not yet merged).

**The honest starting point**: 12.16 added a 10-verb layer *alongside* the existing surface — it did not remove anything. Total top-level commands grew from 59 to 70. Only 18 of ~59 legacy nouns got a working verb+noun mapping. This doc lays out what "actually simple" requires beyond that first step.

---

## 1. The 10 verbs

`create · list · show · update · remove · approve · deny · apply · check · sync`, plus `run` (already verb-shaped) and `status` (the no-argument default). Each takes a noun: `ta <verb> <noun> [id] [flags]`.

| Verb | Does | Why it exists |
|---|---|---|
| `create` | Provision a new resource | Replaces `new`/`init`/`add`/`install` naming inconsistency |
| `list` | Show all resources of a kind | Replaces 15+ independent `list` implementations |
| `show` | Single-item detail | Replaces `view`/`status`/`inspect` inconsistency |
| `update` | Modify a resource | Replaces `set`/`assign`/`reload` |
| `remove` | Delete a resource | Replaces `delete`/`remove`/`revoke`/`uninstall` |
| `approve` / `deny` | Decision-gate outcomes | Same shape as `draft approve`/`deny`, generalized |
| `apply` | Fire the Commit stage | TA's defining action — materializes an approved draft |
| `check` | Correctness/validation | Replaces `validate`/`verify`/`audit` inconsistency |
| `sync` | Reconcile with remote/registry | Replaces `gc`/`prune`/`migrate` |
| `run` | Launch an agent goal | Already verb-shaped, unchanged |

These 5 (`approve`/`deny`/`apply`/`check`/`run`) map directly onto the Write→Review→Decision→Commit/Reject graph (§9 of `ta-concepts-and-architecture.md`). The rest are generic CRUD, not TA-specific.

---

## 2. Current coverage (18 of ~42 noun-areas)

**Mapped today** (via `NOUN_TABLE` in `verb.rs`): `goal, draft, plan-phase, team, persona, workflow, plugin, template, session, credential, event, token, office, daemon, connector, community-resource, context, agent`.

**Not yet mapped** (23 more noun-areas that have real subcommands but only work via their legacy form): `runbook, operations, audit, setup, init, new, advisor, style, constitution, memory, adapter, release, intake, stats, meridian, tools, manifest, link, policy, config, analysis, compression, webhook`.

**Standalone commands with no noun at all** (16): `status, run, shell, gc, doctor, upgrade, onboard, install, dev, advise, publish, serve, build, sync, verify, conversation`.

**Already hidden from `--help`** (5, pre-existing): `pr` (deprecated alias for `draft`), `accept-terms, view-terms, terms-status, terms`.

**Completing #2** means adding NOUN_TABLE entries for the remaining 23 — mechanical, same pattern as the existing 18, no new design needed. Scoping this as a fast-follow phase.

---

## 3. What "simple" actually requires — three moves, not one

1. **Finish the mapping** (above) — so the new surface is complete, not partial.
2. **Hide the legacy 59 from `--help`** (`#[command(hide = true)]`, same mechanism already used for `pr`/`terms`). A new user sees only the 10 verbs; existing scripts/muscle-memory using legacy forms keep working silently. Zero breaking change.
3. **Actually remove the legacy forms** — a deliberate future breaking-change phase, once the alias window has run its course. Not decided yet; deferred on purpose.

(1) and (2) together get to "looks simple" fast, without breaking anything. (3) is a real product decision for later.

---

## 4. Surface area: human-facing vs. agent/automation-exposed

This is the actual design principle, not just a cleanup: **the full 70+ command surface should keep existing — it's the substrate workflows and the Advisor compose against.** What shrinks is what a *person* needs to type day-to-day.

- **Human daily-use surface (target: ~5-6 verbs)**: `run`, `status`, `approve`/`deny`/`apply` (the review decision), `list`/`show` for checking in. Everything else — creating personas, wiring plugins, tuning policy — is either a one-time setup step (better done via `ta onboard`/Studio than memorized commands) or something the Advisor does *for* the user in response to a plain-language request.
- **Agent/workflow-authoring surface (full ~70+ commands)**: this is what a workflow definition, the Advisor's own tool-calling, or a future visual workflow builder (v0.18+) compose against. An LLM predicting "what should happen next in this workflow" needs the *full* verb+noun+flag vocabulary, not the trimmed human one — restricting automation to the same small surface a person sees would make workflows less capable, not safer.

So the reduction target is specifically the **`--help` output and documentation a new user reads first**, not the actual command count. See `docs/design/ta-user-personas.md` for what this looks like from each persona's side — what they'd want to do, what they'd actually type, and what happens invisibly.
