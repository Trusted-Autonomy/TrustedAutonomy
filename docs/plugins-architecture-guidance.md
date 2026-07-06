# Trusted Autonomy — Plugin Architecture & Event Model

> **Note:** For the current, unified Plugin-category manifest/discovery/protocol reference (VCS/messaging/social/agent/tool/db/release), see `docs/USAGE.md` → "Authoring a Plugin". This document covers general design principles and the event model, not superseded by that guide.

Trusted Autonomy is designed to be **extendible by nature**.
Plugins add intelligence and observability, not control.

---

## 1. Plugin Design Principles

- Plugins are optional.
- Plugins cannot bypass policy.
- Plugins observe and advise.
- Plugins operate via stable events.

---

## 2. Plugin Types

- Advisors (pre-execution)
- Reviewers (PR-time)
- Auditors (post-execution)
- Optimizers (longitudinal)

---

## 3. Core Event Hooks

Plugins may subscribe to:

### Goal Lifecycle
- `on_goal_created`
- `on_goal_configured`
- `on_goal_started`
- `on_goal_completed`
- `on_goal_failed`

### Access & Policy
- `on_access_proposed`
- `on_access_approved`
- `on_access_denied`
- `on_boundary_hit`

### Execution
- `on_changeset_created`
- `on_pr_generated`
- `on_pr_approved`
- `on_pr_denied`

### Audit
- `on_policy_violation`
- `on_sandbox_escape_attempt`
- `on_anomaly_detected`

---

## 4. Denial & Uncertainty Handling

When access is denied or insufficient:
- Plugin may emit:
  - `ClarificationRequest`
  - `PermissionEscalationSuggestion`
- UX must surface:
  - why access was denied
  - what additional input is needed
  - risks of escalation

---

## 5. Rust Plugin Interface (Conceptual)

- Plugins implement trait-based handlers.
- Loaded via registry at startup.
- Cannot mutate core state directly.

---

## 6. UX Implications

Plugins produce:
- annotations
- summaries
- warnings
- suggestions

UX decides presentation.

