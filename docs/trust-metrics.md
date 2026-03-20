# Trusted Autonomy — Trust Metrics Alignment

This document maps TA's architectural choices to the 15 trust variables proposed in *"Suggested Metrics for Trusted Autonomy"* (Dr. Robert Finkelstein, Robotic Technology Inc., submitted as public comment to NIST docket NIST-2023-0009-0002, January 2024).

The paper proposes measurable variables for establishing trust in autonomous systems. Though framed around military robotics, the variables apply directly to software AI agent autonomy platforms. This mapping documents how TA satisfies each variable today and where work is planned.

---

## Mapping: TA Architecture → 15 Trust Variables

| # | Variable | TA Implementation | Status |
|---|---|---|---|
| 1 | Risk and Risk Mitigation | Staging model: agent output cannot affect production without approval — reduces consequence term in `risk = probability × consequence` | ✅ Core architecture |
| 2 | Uncertainty | Not yet surfaced to user in review UI | 🔲 Future: draft review could surface agent confidence signals |
| 3 | Reliability | Tamper-evident audit ledger: deviations from spec are traceable, attributable, and repairable | ✅ Audit ledger (v0.14.1) |
| 4 | Accuracy and Precision | Diff-level review: user sees exact file changes before approval — no summary approximations | ✅ Draft diff view |
| 5 | Learning and Adaptation | Not implemented | 🔲 Future: learn from approval patterns to suggest policy rule updates |
| 6 | Redundancy | Staging isolation: a misbehaving agent cannot cascade failures into production | ✅ Overlay staging |
| 7 | Predictability | Staging model makes agent actions predictable: operator always knows what *will* happen before it does | ✅ Draft review workflow |
| 8 | Consistency | Constitution framework: project-level invariants enforced at draft-build time (v0.13.9) | 🔲 Planned (v0.13.9) |
| 9 | Transparency | Every agent action produces a reviewable diff; per-artifact `explanation_tiers` document the reasoning; `ta draft view` surfaces full context | ✅ Draft system |
| 10 | Robustness and Resilience | Sandbox isolation (v0.14.0): even if agent misbehaves, blast radius is limited to staging | 🔲 Planned (v0.14.0) |
| 11 | Security | Sandboxed execution (v0.14.0): agent process isolated from filesystem, network, and syscall surface beyond declared scope | 🔲 Planned (v0.14.0) |
| 12 | Situational Awareness | URI-based artifact identity (`fs://workspace/<path>`, `gmail://`, `drive://`): gives the platform a structured situational model for applying policy | ✅ URI artifact system |
| 13 | Value Judgment | Policy engine: URI-scoped globs, tiered approval tiers, action-class constraints. Constitution (`constitution.toml`) extends this per-project (v0.13.9) | ✅ Auto-approval policy; 🔲 Constitution (v0.13.9) |
| 14 | Context Cognition | Plan-linked goals (`--phase`): goals inherit phase context; CLAUDE.md injection gives agent project conventions | ✅ Plan linking + CLAUDE.md injection |
| 15 | Self-Reflexive Meta Control | Draft approval workflow: user sees full context before granting trust. Tamper-evident ledger is the "complete accounting." Draft `explanation_tiers` are agent-generated self-explanation of *why* each change was made | ✅ Draft system + ledger; 🔲 Policy rationale display (future) |

---

## High-Priority Gap Analysis

### Gap 1 — Policy rationale in approval UI (§13 Value Judgment)
When a draft requires approval because of a policy rule, the UI should explain *which rule* triggered it and *why*. Currently the user sees "requires approval" without context.

**Maps to**: Observability Mandate in `CLAUDE.md` — actionable messages must include what happened, what was attempted, and what to do next.

**Suggested fix**: When presenting a draft artifact for approval, show the matching policy rule key alongside the artifact. Example:
```
✋ Requires approval — matched policy rule: protect["Config/DefaultEngine.ini"]
```

### Gap 2 — Agent self-explanation in drafts (§15 Self-Reflexive Meta Control)
The `explanation_tiers` field exists and is populated per-artifact. But the top-level `summary_why` is often a placeholder. Bug C (v0.13.1.2 item 11) partially addresses this by reading the plan phase `**Goal**:` line. A more complete fix would prompt the agent to write a structured self-assessment.

### Gap 3 — Uncertainty surfacing (§2 Uncertainty)
The agent produces diffs but does not signal when it is uncertain about a decision. The draft review UI could surface a confidence indicator or flag artifacts where the agent hedged (e.g., added a `TODO` comment, or made a choice with multiple valid alternatives).

### Gap 4 — Learning from approval patterns (§5 Learning and Adaptation)
TA could observe which artifact types the user routinely approves vs. amends vs. denies, and surface suggestions for policy rules that would automate low-risk approvals. This turns the approval history into a policy recommendation engine.

---

## Version History

| Date | Change |
|---|---|
| 2026-03-20 | Initial document — created from analysis of NIST-2023-0009-0002 |

---

## Reference

**Document**: *Suggested Metrics for Trusted Autonomy*
**Author**: Dr. Robert Finkelstein, Robotic Technology Inc.
**Filed**: January 3, 2024, as public comment to NIST docket NIST-2023-0009 (AI Risk Management Framework)
**Source**: https://downloads.regulations.gov/NIST-2023-0009-0002/attachment_1.pdf
**Note**: This is an external public comment, not a NIST publication or binding standard. It is used here as a theoretical reference for articulating TA's design rationale.
