# Why Use a Product Constitution (and When You Don't Need One)

> **TL;DR**: TA protects you by default. A constitution is how you teach it what "right" looks like for your specific work.

---

## What the Supervisor Does Without a Constitution

TA's supervisor is on by default, and it already does a lot without any configuration:

- Every action the agent proposes is staged, not applied — you review before anything changes
- Irreversible operations (commit, send, post, delete) always require human approval
- A complete audit trail records what happened and why
- The agent can't see anything outside the staging copy of your project

These protections exist regardless of whether you have a constitution. If you're new to TA, you don't need to write one to benefit from the supervisor.

---

## What a Constitution Adds

A constitution is a behavioral contract between you and the agent. It tells the agent — and TA's policy engine — what *you* consider acceptable for your specific context.

Think of it like the difference between traffic law (applies to everyone by default) and a driving school's rulebook (rules specific to what they teach, on top of traffic law). TA's defaults are the traffic law. Your constitution is the driving school.

A constitution can specify:

- **What the agent should and shouldn't touch** — "never modify billing code without a second reviewer"
- **How the agent should communicate** — tone, format, what to ask before assuming
- **Decision rules for ambiguous situations** — "when uncertain between two approaches, prefer the more conservative one"
- **Quality standards** — "all user-facing strings must be localized before being marked done"
- **Escalation rules** — "if a task touches PII fields, pause and notify before proceeding"

---

## When You Need One

Write a constitution when the agent keeps making decisions that are technically correct but wrong for your context.

**Concrete signals:**

| You see this... | A constitution can help by... |
|----------------|-------------------------------|
| Agent picks the right approach but in the wrong file | Scoping which parts of the project the agent should prefer |
| Agent's output is correct but the tone/style is off | Defining communication standards |
| Agent asks good questions but asks too many of them | Setting a threshold for when to ask vs. infer |
| Reviews keep catching the same class of mistake | Adding a standing rule that catches it before you do |
| Two agents give inconsistent outputs on similar tasks | Establishing a shared behavioral baseline |

---

## When You Don't Need One

- **Early in a project** — defaults are fine. Write a constitution after you see patterns, not before.
- **One-off tasks** — the overhead of a constitution isn't worth it for a single goal.
- **When you're still learning** — the supervisor's review workflow will surface what the agent gets wrong. Watch the pattern before encoding it.
- **When the task has high human-in-the-loop already** — if you're approving every draft anyway, a constitution doesn't change much.

---

## The Supervisor's Relationship to the Constitution

The supervisor doesn't just execute the constitution — it interprets it. When the agent's behavior approaches a rule boundary, the supervisor:

1. Flags the potential conflict in the draft summary
2. Highlights which rule applies and what the agent did
3. Asks for human judgment if the rule is ambiguous

This means the constitution doesn't need to be exhaustive. Write the rules that matter, let the supervisor flag edge cases, and refine over time.

---

## Getting Started

TA ships with a starter constitution template. Run:

```bash
ta constitution init
```

This creates a `ta-constitution.toml` (or extends `workflow.toml` with a `[constitution]` section) with commented-out rules you can enable one at a time.

You can also reference the built-in [TA-CONSTITUTION.md](../TA-CONSTITUTION.md) — that's the behavioral spec TA's own development follows. It's a real-world example of what a constitution looks like at scale.

---

## The Key Insight

**The supervisor is how TA helps you verify the result. The constitution is how TA learns what the right result looks like for you.**

You can use TA without a constitution and get substantial value. But the more specific your work, the more a constitution pays off — it turns the supervisor from a safety net into a collaborator that actually knows your standards.

---

*Related: [USAGE.md — Supervisor configuration](../USAGE.md) · [TA-CONSTITUTION.md — TA's own behavioral spec](../TA-CONSTITUTION.md)*
