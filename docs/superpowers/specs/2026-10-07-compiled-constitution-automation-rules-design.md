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

---

# Revision 2 (2026-10-07, after owner direction and three independent reviews)

Revision 2 replaces the placement, vocabulary and trust rules above where they differ. It folds in the owner's direction and a security review, an audience review and a commercial review.

## R2.1 Where the rules live

**One engine and one owner file in TA. Role and team presets in VT, as proposals the owner adopts.**

- **TA owns the engine, the floor and the owner's file.** The engine (`decide()`, the compiled table, `show`, `diff`, the audit entries) is open source and lives in TA. The owner's rules file is one file, per project, and nothing below it can loosen it.
- **The owner's file is signed and held out of agents' reach.** The security review's top finding: if the file is a plain file an agent can edit, the rule table is only advice. So: the file is committed with the project so it travels with the team, it is signed by the owner's key, and that key lives outside the project tree (Keychain, DPAPI or the platform keyring). The daemon verifies the signature on every compile and refuses a file whose signature does not match. A draft that touches the rules file, a posture, a target binding or a persona file always needs a human, even under the `open` posture.
- **VT supplies presets, not rules.** A preset (for example "research role: may apply notes under `docs/research/**`, no outside actions") is a proposal shipped in a team pack. It can **tighten** freely. It can **loosen** only when the owner adopts it, which rewrites the owner's file, shows the exact table cells that change, and re-signs. The same pattern as Wayfinder's verb adoption: the team proposes, the owner adopts.
- **Org floor (optional).** An organization may supply a floor file that no project file can loosen. Projects can only tighten beneath it.

Precedence, printed by `show` for every cell: **org floor, then owner file, then a preset that can only tighten, then the posture default.** The old `.ta/constitution.toml` rules and the `[actions]` tables in `workflow.toml` are folded into the new file by one migration, so users meet one file and not three.

## R2.2 Trust inputs the rules depend on

The review found the whole design rested on three inputs that were not yet trustworthy. These must exist before `decide()` is trusted:

1. **"Attended" must be proven, not claimed.** Whether a person is present comes only from a verified human credential presented in the same session. Anything unproven is unattended. It never comes from an environment variable or the goal's origin tag. This requires the CR-02 fix (agents must not reach approve or apply) first.
2. **A target's environment is bound to its real connection identity**, not to a label. A target called `scratch-db` that points at the production connection string is production. The binding lives in an owner-only file, and any edit to a target through a draft is always a human decision. An unknown target is production.
3. **The rules file itself is tamper-evident** (R2.1).

## R2.3 File auto-apply is part of the table

The first draft covered outside actions only (email, database). The research-agent case needs file apply in the same table. Rules gain an `apply` action with literal path prefixes:

```toml
[[allow]]
action = "apply"
paths  = ["docs/research/**"]
when   = "unattended"
```

Safeguards from the security review:

- Allowed paths are literal prefixes after normalization (case folded, Unicode NFC, no `..`), and can never reach `.ta/`, `.git/`, `.claude/` or `.mcp.json`.
- Links of any kind (symlinks, hard links) are refused.
- The rule is matched against the draft's own list of changed files, and no build, hook or script runs on an unattended apply.
- **Second-order injection.** Auto-applied content is marked untrusted in its metadata. Never allow unattended apply into paths that agent prompts are built from (for example the wiki a CoS reads). Reading untrusted content never raises any agent's permissions.
- A preset's allowed paths are checked as a subset of the owner file at compile time.

## R2.4 Vocabulary (budget: four new words at most)

| Keep | Meaning |
|---|---|
| **apply**, **action** | Already exist. |
| **posture** | `strict`, `balanced`, `open`. |
| **environment** | `production`, `staging`, `dev`, `test`. |
| **rules** | The one word for the constitution and its compiled table. `ta rules show` prints "your rules, printed". |

Cut from anything a user sees: "constitution" (use "rules"), "compile" (happens automatically, cached), "who", "automation", "ask". The rule field becomes `when = "unattended" | "attended"` ("without me" and "with me" in plain English). Refusals name the rule and give the one-line fix, for example: `Blocked: unattended email is not allowed by your rules (strict). To allow it for dev only: ta rules allow email --env dev --unattended`.

**Authoring.** Chat or a short form is the normal way to write rules. The assistant drafts the rules file, the owner reviews the table diff like any other change, and the owner's key signs it. A model never decides anything at run time. The file is the audit artifact, not the place people have to write.

**Machine-readable effective policy.** `ta rules effective --role <name> --json` prints the resolved table for a role. VT's poller uses it to refuse launching a role whose effective policy exceeds the ceiling, and Wayfinder's adoption screen uses it to show what a preset or verb would newly allow.

## R2.5 What is open and what is paid

- **Free and open (TA):** the engine, the owner file format, the postures, `show`, `diff`, `effective`, and the audit entries. Enforcement is never a paid feature; security reviewers will not trust enforcement they cannot read.
- **In the bundle (VT, Untollable):** maintained role and team presets, the installer's posture picker, plain-English drafting of rules, and a stream of preset updates delivered as diffs that are never applied silently.
- **Higher tier (SA Enterprise):** central policy across projects, signed organization floors, compliance evidence reports that map rules to controls, verified industry packs, and long audit retention.
- **Marketplace rule:** packs can only tighten by default. "Verified" means a pack passes the published rule-table test suite, not that it is a safety guarantee.

## R2.6 Sequencing and honesty

This design depends on three fixes the red-team report lists as open: agents reaching approve and apply (CR-02), secrets inside the project tree (CR-03), and agents impersonating roles (CR-04). Until they land, the rules engine would enforce on inputs an agent can influence. The commercial review also notes that marketing "enforced governance" before those are fixed is a risk to the enterprise story. Ship order: CR-02, CR-03 (secrets move), CR-04, then the engine, then presets.
