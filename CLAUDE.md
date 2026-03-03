# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** Implement v0.5.7 — Semantic Memory Queries & Memory Dashboard
**Goal ID:** 4bf6cb1b-1ced-4850-bded-8cdfba2444f2

## Plan Context

Plan progress:
- [x] Phase 0 — Repo Layout & Core Data Model
- [x] Phase 1 — Kernel: Audit, Policy, Changeset, Workspace
- [x] Phase 2 — MCP Gateway, Goal Lifecycle, CLI
- [x] Phase 3 — Transparent Overlay Mediation
- [x] Phase 4a — Agent Prompt Enhancement
- [x] Phase 4a.1 — Plan Tracking & Lifecycle
- [x] Phase 4b — Per-Artifact Review Model
- [x] Phase 4c — Selective Review CLI
- [ ] Phase v0.1 — Public Preview & Call for Feedback
- [ ] Phase v0.1.1 — Release Automation & Binary Distribution
- [x] Phase v0.1.2 — Follow-Up Goals & Iterative Review
- [x] Phase v0.2.0 — SubmitAdapter Trait & Git Implementation
- [x] Phase v0.2.1 — Concurrent Session Conflict Detection
- [x] Phase v0.2.2 — External Diff Routing
- [x] Phase v0.2.3 — Tiered Diff Explanations & Output Adapters
- [x] Phase v0.2.4 — Terminology & Positioning Pass
- [x] Phase v0.3.0 — Review Sessions
- [x] Phase v0.3.0.1 — Consolidate `pr.rs` into `draft.rs`
- [x] Phase v0.3.1 — Plan Lifecycle Automation
- [x] Phase v0.3.1.1 — Configurable Plan Format Parsing
- [x] Phase v0.3.1.2 — Interactive Session Orchestration
- [x] Phase v0.3.2 — Configurable Release Pipeline (`ta release`
- [x] Phase v0.3.3 — Decision Observability & Reasoning Capture
- [x] Phase v0.3.4 — Draft Amendment & Targeted Re-Work
- [x] Phase v0.3.5 — Release Pipeline Fixes
- [x] Phase v0.3.6 — Draft Lifecycle Hygiene
- [x] Phase v0.4.0 — Intent-to-Access Planner & Agent Alignment Profiles
- [x] Phase v0.4.1 — Macro Goals & Inner-Loop Iteration
- [x] Phase v0.4.1.1 — Runtime Channel Architecture & Macro Session Loop
- [x] Phase v0.4.1.2 — Follow-Up Draft Continuity
- [x] Phase v0.4.2 — Behavioral Drift Detection
- [x] Phase v0.4.3 — Access Constitutions
- [x] Phase v0.4.4 — Interactive Session Completion
- [x] Phase v0.4.5 — CLI UX Polish
- [x] Phase v0.5.0 — Credential Broker & Identity Abstraction
- [x] Phase v0.5.1 — MCP Tool Call Interception
- [x] Phase v0.5.2 — Minimal Web Review UI
- [x] Phase v0.5.3 — Additional ReviewChannel Adapters
- [x] Phase v0.5.4 — Context Memory Store (ruvector integration
- [x] Phase v0.5.5 — RuVector Memory Backend
- [x] Phase v0.5.6 — Framework-Agnostic Agent State
- [ ] Phase v0.5.7 — Semantic Memory Queries & Memory Dashboard
- [ ] Phase v0.6.0 — Supervisor Agent & Constitutional Workflows
- [ ] Phase v0.6.1 — Cost Tracking & Budget Limits
- [ ] Phase v0.7.0 — Agent-Guided Setup (`ta setup`
- [ ] Phase v0.7.1 — Domain Workflow Templates
- [ ] Phase v0.8.0 — Event System & Orchestration API
- [ ] Phase v0.8.1 — Community Memory
- [ ] Phase v0.9.0 — Distribution & Packaging
- [ ] Phase v0.9.1 — Native Windows Support
- [ ] Phase v0.9.2 — Sandbox Runner (optional hardening
- [ ] Phase v1.0.0 — Virtual Office Runtime

## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** 4bf6cb1b-1ced-4850-bded-8cdfba2444f2

## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** Build the v0.5 features
**Goal ID:** 7676fe31-1581-49ad-bc16-2f281916a344

## Plan Context

Plan progress:
- [x] Phase 0 — Repo Layout & Core Data Model
- [x] Phase 1 — Kernel: Audit, Policy, Changeset, Workspace
- [x] Phase 2 — MCP Gateway, Goal Lifecycle, CLI
- [x] Phase 3 — Transparent Overlay Mediation
- [x] Phase 4a — Agent Prompt Enhancement
- [x] Phase 4a.1 — Plan Tracking & Lifecycle
- [x] Phase 4b — Per-Artifact Review Model
- [x] Phase 4c — Selective Review CLI
- [ ] Phase v0.1 — Public Preview & Call for Feedback
- [ ] Phase v0.1.1 — Release Automation & Binary Distribution
- [x] Phase v0.1.2 — Follow-Up Goals & Iterative Review
- [x] Phase v0.2.0 — SubmitAdapter Trait & Git Implementation
- [x] Phase v0.2.1 — Concurrent Session Conflict Detection
- [x] Phase v0.2.2 — External Diff Routing
- [x] Phase v0.2.3 — Tiered Diff Explanations & Output Adapters
- [x] Phase v0.2.4 — Terminology & Positioning Pass
- [x] Phase v0.3.0 — Review Sessions
- [x] Phase v0.3.0.1 — Consolidate `pr.rs` into `draft.rs`
- [x] Phase v0.3.1 — Plan Lifecycle Automation
- [x] Phase v0.3.1.1 — Configurable Plan Format Parsing
- [x] Phase v0.3.1.2 — Interactive Session Orchestration
- [x] Phase v0.3.2 — Configurable Release Pipeline (`ta release`
- [x] Phase v0.3.3 — Decision Observability & Reasoning Capture
- [x] Phase v0.3.4 — Draft Amendment & Targeted Re-Work
- [x] Phase v0.3.5 — Release Pipeline Fixes
- [x] Phase v0.3.6 — Draft Lifecycle Hygiene
- [x] Phase v0.4.0 — Intent-to-Access Planner & Agent Alignment Profiles
- [x] Phase v0.4.1 — Macro Goals & Inner-Loop Iteration
- [x] Phase v0.4.1.1 — Runtime Channel Architecture & Macro Session Loop
- [x] Phase v0.4.1.2 — Follow-Up Draft Continuity
- [x] Phase v0.4.2 — Behavioral Drift Detection
- [x] Phase v0.4.3 — Access Constitutions
- [x] Phase v0.4.4 — Interactive Session Completion
- [x] Phase v0.4.5 — CLI UX Polish
- [ ] Phase v0.5.0 — Credential Broker & Identity Abstraction
- [ ] Phase v0.5.1 — MCP Tool Call Interception
- [ ] Phase v0.5.2 — Minimal Web Review UI
- [ ] Phase v0.5.3 — Additional ReviewChannel Adapters
- [ ] Phase v0.5.4 — Context Memory Store (ruvector integration
- [ ] Phase v0.6.0 — Supervisor Agent & Constitutional Workflows
- [ ] Phase v0.6.1 — Cost Tracking & Budget Limits
- [ ] Phase v0.7.0 — Agent-Guided Setup (`ta setup`
- [ ] Phase v0.7.1 — Domain Workflow Templates
- [ ] Phase v0.8.0 — Event System & Orchestration API
- [ ] Phase v0.8.1 — Community Memory
- [ ] Phase v0.9.0 — Distribution & Packaging
- [ ] Phase v0.9.1 — Native Windows Support
- [ ] Phase v0.9.2 — Sandbox Runner (optional hardening
- [ ] Phase v1.0.0 — Virtual Office Runtime

## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** 7676fe31-1581-49ad-bc16-2f281916a344

## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

# Claude Code Project Instructions

## Build Environment

Nix provides the Rust toolchain. **Always prefix cargo/just commands with the nix wrapper:**

```bash
export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
nix develop --command bash -c "COMMAND_HERE"
```

Or use the helper script for one-liners:
```bash
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev just verify
```

## Verification Before Every Commit

Run these four checks (all must pass):
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

## Git Workflow — Feature Branches + Pull Requests

All work MUST happen on feature branches. Never commit directly to `main`.

1. **Create a feature branch** before starting work:
   ```bash
   git checkout -b feature/<short-description>
   ```
   Use prefixes: `feature/`, `fix/`, `refactor/`, `docs/` as appropriate.

2. **Commit to the feature branch** in logical working units as you go.

3. **When the goal is complete**, push and open a pull request:
   ```bash
   git push -u origin feature/<short-description>
   gh pr create --title "Short description" --body "## Summary\n- what changed and why\n\n## Test plan\n- verification steps"
   ```

4. **The PR is reviewed and merged** into `main` (squash or merge commit).

This applies to both manual work and TA-mediated goals. When `ta pr apply --git-commit` runs, the commit should land on a feature branch, not `main`.

## Rules

- Never commit directly to `main` — always use a feature branch + PR
- Never disable or skip tests
- Run tests after every code change, before committing
- Commit in logical working units
- All work stays within ~/development/TrustedAutonomy/
- Use `tempfile::tempdir()` for all test fixtures that need filesystem access

## Current State

- **Current version**: `0.5.7-alpha`
- See **PLAN.md** for the canonical development roadmap with per-phase status
- `ta plan list` / `ta plan status` show current progress
- Goals can link to plan phases: `ta run "title" --source . --phase 4b`
- `ta draft apply` auto-updates PLAN.md when a phase completes

## Version Management

When completing a phase, you MUST update versions as part of the work:

1. **`apps/ta-cli/Cargo.toml`**: Update `version` to the phase's target version (e.g., `"0.2.0-alpha"`)
2. **This file (`CLAUDE.md`)**: Update "Current version" above to match
3. **`PLAN.md`**: Mark the phase `<!-- status: done -->` (done automatically by `ta draft apply --phase`)
4. **`docs/USAGE.md`**: Update with any new commands, flags, config options, or workflow changes

Version format: `MAJOR.MINOR.PATCH-alpha` (semver). See `PLAN.md` "Versioning & Release Policy" for the full mapping of phases to versions. Sub-phases use pre-release dot notation: `v0.4.1.2` → `0.4.1-alpha.2`.

### How It Works (Overlay Flow)
1. `ta goal start "title" --source . --phase 4b` → copies project to `.ta/staging/`
2. `ta run "title" --source . --phase 4b` → creates goal + injects CLAUDE.md (with plan context) + launches agent + builds draft on exit
3. Agent works normally in staging copy — TA is invisible to the agent
4. `ta draft build --latest` → diffs staging vs source → creates draft package with artifacts
5. `ta draft view/approve/deny <id>` → review workflow
6. `ta draft apply <id> --git-commit` → copies changes back to source + updates PLAN.md + optional git commit

### Key Types
- **Artifact.resource_uri**: `"fs://workspace/<path>"` — URI-based identity for all changes
- **PatchSet.target_uri**: Same URI scheme for external resources (gmail://, drive://, etc.)
- **DraftStatus**: Draft → PendingReview → Approved/Denied → Applied/Superseded/Closed
- **GoalRunState**: Created → Configured → Running → PrReady → UnderReview → Approved → Applied → Completed
- **GoalRun.plan_phase**: Optional link to a PLAN.md phase (e.g., "4b")
- **CLAUDE.md injection**: `ta run` prepends TA context + plan progress, saves backup, restores before diff

