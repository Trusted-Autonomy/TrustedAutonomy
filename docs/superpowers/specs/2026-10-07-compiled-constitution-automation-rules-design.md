# Compiled Constitution: Automation Rules and Security Postures

**Status:** proposal for the next TA + VT release. Not built. Written from the user's direction on 2026-10-07.

## The problem in one paragraph

Today the rules about what an agent's work may *do in the world* (send email, write to a database, post to Slack) are scattered: `.ta/constitution.toml` block/warn rules, `[actions.<type>]` policies in `.ta/workflow.toml`, hard-coded guards (email is never auto), and, new in PR #642, a blanket "automated applies never send". A user cannot answer "what will happen if nobody is watching?" without reading code. And the blanket rule is too blunt: the real wish is "never send emails, never touch the production database, but dev and test are fine."

## What the user wants (their words, condensed)

1. A **constitution** a person writes in plain terms: "never send emails. never update the production db, but dev and test environments are fine."
2. TA **compiles** it into strict, deterministic automation rules. No model judgment at run time. Strict adherence.
3. **Default rule sets per security posture** (for example strict, balanced, open) shipped with TA and VT, so a new install is safe without writing anything.
4. **One verb.** `ta draft apply` does everything. No separate "send" or "replay" commands.

## Vocabulary (kept small on purpose)

| Word | Meaning |
|---|---|
| **apply** | Make an approved draft real: copy the files, then run its approved external actions. Safe to run again; it only does what is still undone. |
| **action** | Something a draft does outside the project: send an email, run a database query, post a message. |
| **constitution** | The plain-language rules you own. |
| **rules** | What the constitution compiles to. A table. You can print it: `ta constitution show`. |
| **posture** | A named starting constitution: `strict`, `balanced`, `open`. |

Nothing else is new. "Replay", "ledger", "bounded channel" are internal and do not appear in user-facing output.

## Design

### 1. Targets carry an environment

Every external target declares what it is, once, in config:

```toml
[targets.main-db]        # a db_query target
type = "database"
environment = "production"   # production | staging | dev | test

[targets.scratch-db]
type = "database"
environment = "dev"
```

An action with a target that has no declared environment is treated as **production**. Unknown means strictest.

### 2. The constitution (human-written)

```toml
posture = "strict"            # starting point; the lines below adjust it

[[allow]]
action = "db_query"
environment = ["dev", "test"]
who = "automation"            # automation = no human present; human = a person ran apply

[[deny]]
action = "email"
who = "automation"
reason = "Email always needs a person."

[[deny]]
action = "db_query"
environment = ["production"]
```

`who` is the key new axis: the same action can be fine when a person runs `apply` and denied when an automated workflow does.

### 3. Compile to a rule table

`ta constitution compile` (also run automatically on apply, cached by file hash) turns posture + constitution into one flat table: `(action type, environment, who) -> allow | deny | ask`, plus limits (rate, recipients, domains). Properties:

- **Default deny.** Anything not matched is denied for `automation` and asked for `human`.
- **Deny beats allow.** Most specific deny wins; no ordering tricks.
- **Fail closed.** A constitution that does not parse, a missing environment on a target, or an unknown action type: nothing runs, and the output says exactly which line to fix.
- **Deterministic.** The table is plain data. A test can enumerate every cell. No LLM is consulted.
- **Visible.** `ta constitution show` prints the table; `ta draft apply` prints the single rule that allowed or refused each action.

### 4. Where it is enforced

One function, called by every path that can cause an external effect: `rules.decide(action, target, who) -> Allow | Deny(rule, reason) | Ask`. Callers: `ta draft apply` (human and automated), governed workflows, workflow-graph auto-approve, the VT poller when it launches work, and the whiteboard outcome handler. PR #642 already isolates the automated-apply decision in one function (`automated_apply_may_run`), which this replaces.

### 5. Postures (shipped defaults)

| Posture | Email / social | Production DB | Dev/test DB | Local files | Intended for |
|---|---|---|---|---|---|
| **strict** | deny for automation, ask for human | deny | deny for automation, ask for human | allow | default for any install |
| **balanced** | deny for automation, ask for human | deny | allow | allow | a team working in dev/test daily |
| **open** | ask for human, allow for automation up to rate limits | ask | allow | allow | sandboxes and demos only |

TA ships these as data (`postures/*.toml`), and VT's installer asks which posture to start from and writes it into the project.

### 6. Audit

Every decision (allow, deny, ask) is appended to the audit log with the rule that decided it. "Why did this send?" and "why did this not send?" are always answerable.

## How this relates to what exists

- Extends `.ta/constitution.toml` (block/warn rules in `ta-actions/src/constitution_rules.rs`); the old file keeps working and is read as extra deny rules.
- Subsumes `[actions.<type>]` allow-lists in `workflow.toml` for the same purpose.
- Replaces the PR #642 blanket "automated applies never send".
- Uses the existing `GoalRun.origin` (PR #641) as one input to `who`: origin `cos` or `chat` is always `automation`-class and can never be elevated by the goal itself.

## Open questions

1. Where does the environment of a target come from for third-party connectors? Proposal: the connector manifest must declare it; a connector that cannot is treated as production.
2. Does `ask` in an automated context (nobody to ask) become `deny`? Proposal: yes, always.
3. Posture upgrades: if TA ships a new default table, does an existing project get it? Proposal: never silently; `ta constitution diff` shows what would change.
4. Per-persona overrides (the CoS always strict regardless of project posture): proposal yes, a persona can only tighten, never loosen.

## Suggested phasing

1. Environment-tagged targets + the single `decide()` function + `strict` posture, wired to apply (replaces #642's blanket rule).
2. `ta constitution show|compile|diff`, the other postures, audit entries.
3. VT installer posture choice, per-persona tightening, red-team review of the compiler.
