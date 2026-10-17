# Supervisor review slot and auto-approval constitution

Status: design (2026-10-16). Nothing in sections 3 to 7 is built. Templates: `templates/constitution/`.

## 1. What exists today (reuse, do not rebuild)

- `ta-changeset::supervisor_review`: `invoke_supervisor_agent(objective, changed_files, constitution_text, config)` returns a `SupervisorReview` (verdict pass/warn/block, `scope_ok`, findings, summary, confidence). The agent is configurable (`builtin`, `claude-code`, `codex`, `ollama`, or a manifest agent). Any failure degrades to `warn`; it never blocks a draft build.
- `ta-policy::approval_rules`: `[[approval_rules]]` in `.ta/constitution.toml`, path globs, first match wins within the file, most restrictive action across items wins, unmatched goes to `review`. Amendment goes through a draft (`ta constitution amend`).
- `ta-policy::auto_approve`: gate order enabled, size, paths, phase, agent level. H9: goals whose origin is in `NO_AUTO_APPROVE_ORIGINS` (`cos`, `chat`) are never auto-approved, checked before any `approve` rule.
- `ta-decision::decide` (decision-gate crate): combines supervisor confidence and risk score.
- A call-site guard test (`constitution_16_3_call_site.rs`) restricts who may call `should_auto_approve_draft`. New logic must go through the existing entry point.

## 2. Principle

The conversation-aware reviewer is a supervisor agent profile, not new machinery. It plugs into the existing slot and its output flows through the existing gate. Rule extensions reuse `[[approval_rules]]` (TA-CONSTITUTION 1.7: reuse before reinventing; 1.6: rule kinds are data, not closed enums).

## 3. Supervisor review slot

New supervisor agent profile `conversation-reviewer`.

Inputs added to today's (objective, changed files, constitution):
- The conversation context pack for the draft's source: until the per-project RAG-KG exists, the last N turns of the originating conversation plus wiki retrieval for the project. When the RAG-KG lands, it replaces the retrieval step with no interface change.
- For funnel drafts: the raw queued requests beside the project manager's decision for each item (the project manager's paraphrase is never the only evidence).
- The merged constitution (section 5).

Outputs: the existing `SupervisorReview`, plus `intent_match` (does the draft do what the conversation asked, with a one-line reason) and per-item `decisions[]` (item id, pass/warn/block, reason) so the human and the gate see item-level results.

Constraints (the risk choices agreed 2026-10-16):
- Read-only tool profile. It cannot approve or write; it emits a verdict and the gate acts.
- Independence: a different session and persona from the author, ideally a different model tier. It never reviews a draft its own session produced.
- Conversation text, queued requests and draft bodies are untrusted, delivered inside a nonce-delimited fence. Reviewer instructions come only from TA and the constitution.
- It can only tighten. A deterministic rule decides the candidate action; the reviewer can downgrade approve to review or block, never the reverse.
- Output is shown to the human with its reasons, and only flagged items need attention. A reviewer that approves everything is measured (section 6) and surfaced.

## 4. Evaluation order

1. Hard stops: origin (H9 unchanged), kinds the project does not allow, protected paths, secret-shaped content.
2. Deterministic rule match per item (fs, wiki, task) gives a candidate action. Layering in section 5.
3. Supervisor: needs verdict `pass` and confidence at or above the rule's `min_confidence`, else the item goes to `review`.
4. Mode: `shadow` records "would have approved" and still routes to the human. `enforce` approves.
5. Everything is written to the audit log with rule id, layer, verdict, confidence and mode.

Origin note: project-manager drafts reach TA through CoS delegation. H9 keeps `cos` and `chat` origins out of auto-approval, correctly. A project-manager draft is auto-approvable only if it carries a distinct origin (proposed `project-manager`) that a rule lists in `origins = [...]`. That origin is stamped by the gateway from the launching role, never by the agent.

## 5. Constitution format and layering

Extends `[[approval_rules]]`; every sample is in `templates/constitution/`.

New optional fields per rule: `id` (unique, stable, required for non-fs kinds), `kind` (`fs` default, `wiki`, `task`), `match` (kind-specific table), `mode` (`shadow` or `enforce`), `min_confidence`, `origins`. File-level: `schema_version = 2`, `layer` (`org` or `project`), `default_mode`.

Layering (org file plus project file):
- Rules from both layers apply. For each item, the most restrictive action across layers wins.
- A project can add rules and tighten. It cannot remove, disable or loosen an org rule. A project `approve` has no effect on an item an org rule marks `review` or `block`.
- Mode: org sets the default per rule. A project may move a rule toward `shadow`. It cannot move an org `shadow` rule to `enforce`. (Decision to confirm: this is the cautious reading of "project and org override".)
- Org file source: the org system of record is Wayfinder. Proposed: Wayfinder stores and serves the org constitution; TA caches it read-only with a version hash and records the hash on every decision. Amendments go through draft review, never silent edits. Until Wayfinder exposes it, the org file is a checked-in file the project pulls from.
- Unknown `kind` is a loud error, not a silent skip.

## 6. Shadow mode

- Default for every sample. Setting at three levels: file `default_mode`, per rule `mode`, project override toward shadow.
- Each decision in shadow appends to `.ta/audit/auto-approve-shadow.jsonl`: draft id, item id, rule id, would-be action, supervisor verdict and confidence, the human's actual action, agree or disagree.
- `ta constitution shadow report` shows per rule: decisions, agreement rate, disagreements with links. Suggested promotion bar (configurable): at least 50 decisions and at most 2 percent disagreement, and zero cases where the machine would have approved something the human rejected. Meeting the bar never switches a rule automatically. TA shows a recommendation (in the report, in `ta constitution shadow report`, and in Wayfinder) with a link to the setting, or raises an ask-human to switch. Only a human action changes `mode` from `shadow` to `enforce`, and the change itself goes through draft review.
- Wayfinder is asked to expose the mode setting and show the shadow report (message 013).

## 7. Phases

- v0.17.12.26: conversation-reviewer profile, per-item decisions, constitution v2 parsing for `wiki` and `task` kinds, org+project layering, gateway-stamped origin.
- v0.17.12.27: shadow mode, audit log, `ta constitution shadow report`, mode settings and promotion bar.
- v0.17.12.28 (after alpha): typed-ask refinement for close phase-title matches in `ta plan resolve`.
- Task intake funnel build (DESIGN-task-intake-funnel.md revision 2) lands behind these: its project-manager drafts are the first consumer.

## 8. Open decisions

1. (Decided 2026-10-16) Project may only move a rule toward shadow.
2. (Decided 2026-10-16) 50 decisions at 2 percent is the recommendation threshold only; switching is always a human action.
3. Distinct origin name for project-manager drafts.
4. Whether Wayfinder or TA owns the canonical org file once Wayfinder serves it (proposed: Wayfinder).
