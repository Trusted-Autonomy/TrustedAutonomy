# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** v0.12.2.2 — Draft Apply: Transactional Rollback on Validation Failure
**Goal ID:** eb531b1a-063f-4ba3-b617-fb602b3eaee5

## Plan Context

Plan progress:
- [x] Phase v0.9.0 — Distribution & Packaging
- [x] Phase v0.9.1 — Native Windows Support
- [x] Phase v0.9.2 — Sandbox Runner (optional hardening, Layer 2
- [x] Phase v0.9.3 — Dev Loop Access Hardening
- [x] Phase v0.9.4 — Orchestrator Event Wiring & Gateway Refactor
- [x] Phase v0.9.4.1 — Event Emission Plumbing Fix
- [x] Phase v0.9.5 — Enhanced Draft View Output
- [x] Phase v0.9.5.1 — Goal Lifecycle Hygiene & Orchestrator Fixes
- [x] Phase v0.9.6 — Orchestrator API & Goal-Scoped Agent Tracking
- [x] Phase v0.9.7 — Daemon API Expansion
- [x] Phase v0.9.8 — Interactive TA Shell (`ta shell`
- [x] Phase v0.9.8.1 — Auto-Approval, Lifecycle Hygiene & Operational Polish
- [x] Phase v0.9.8.1.1 — Unified Allow/Deny List Pattern
- [x] Phase v0.9.8.2 — Pluggable Workflow Engine & Framework Integration
- [x] Phase v0.9.8.3 — Full TUI Shell (`ratatui`
- [x] Phase v0.9.8.4 — VCS Adapter Abstraction & Plugin Architecture
- [-] Phase v0.9.9 — Conversational Project Bootstrapping (`ta new`) *(design only *(deferred)*
- [x] Phase v0.9.9.1 — Interactive Mode Core Plumbing
- [x] Phase v0.9.9.2 — Shell TUI Interactive Mode
- [x] Phase v0.9.9.3 — `ta plan from <doc>` Wrapper
- [x] Phase v0.9.9.4 — External Channel Delivery
- [x] Phase v0.9.9.5 — Workflow & Agent Authoring Tooling
- [x] Phase v0.9.10 — Multi-Project Daemon & Office Configuration
- [x] Phase v0.10.0 — Gateway Channel Wiring & Multi-Channel Routing
- [x] Phase v0.10.1 — Native Discord Channel
- [x] Phase v0.10.2 — Channel Plugin Loading (Multi-Language
- [x] Phase v0.10.2.1 — Refactor Discord Channel to External Plugin
- [x] Phase v0.10.2.2 — `ta plugin build` Command
- [x] Phase v0.10.3 — Slack Channel Plugin
- [x] Phase v0.10.4 — Email Channel Plugin
- [x] Phase v0.10.5 — External Workflow & Agent Definitions
- [x] Phase v0.10.6 — Release Process Hardening & Interactive Release Flow
- [x] Phase v0.10.7 — Documentation Review & Consolidation
- [x] Phase v0.10.8 — Pre-Draft Verification Gate
- [x] Phase v0.10.9 — Smart Follow-Up UX
- [x] Phase v0.10.10 — Daemon Version Guard
- [x] Phase v0.10.11 — Shell TUI UX Overhaul
- [x] Phase v0.10.12 — Streaming Agent Q&A & Status Bar Enhancements
- [x] Phase v0.10.13 — `ta plan add` Command (Agent-Powered Plan Updates
- [x] Phase v0.10.14 — Deferred Items: Shell & Agent UX
- [x] Phase v0.10.15 — Deferred Items: Observability & Audit
- [x] Phase v0.10.15.1 — TUI Output & Responsiveness Fixes
- [x] Phase v0.10.16 — Deferred Items: Platform & Channel Hardening
- [x] Phase v0.10.17 — `ta new` — Conversational Project Bootstrapping
- [x] Phase v0.10.17.1 — Shell Reliability & Command Timeout Fixes
- [x] Phase v0.10.18 — Deferred Items: Workflow & Multi-Project
- [x] Phase v0.10.18.1 — Developer Loop: Verification Timing, Notifications & Shell Fixes
- [x] Phase v0.10.18.2 — Shell TUI: Scrollback & Command Output Visibility
- [x] Phase v0.10.18.3 — Verification Streaming, Heartbeat & Configurable Timeout
- [x] Phase v0.10.18.4 — Live Agent Output in Shell & Terms Consent
- [x] Phase v0.10.18.5 — Agent Stdin Relay & Interactive Prompt Handling
- [x] Phase v0.10.18.6 — `ta daemon` Subcommand
- [x] Phase v0.10.18.7 — Per-Platform Icon Packaging
- [x] Phase v0.11.0 — Event-Driven Agent Routing
- [x] Phase v0.11.0.1 — Draft Apply Defaults & CLI Flag Cleanup
- [x] Phase v0.11.1 — `SourceAdapter` Unification & `ta sync`
- [x] Phase v0.11.2 — `BuildAdapter` & `ta build`
- [x] Phase v0.11.2.1 — Shell Agent Routing, TUI Mouse Fix & Agent Output Diagnostics
- [x] Phase v0.11.2.2 — Agent Output Schema Engine
- [x] Phase v0.11.2.3 — Goal & Draft Unified UX
- [x] Phase v0.11.2.4 — Daemon Watchdog & Process Liveness
- [x] Phase v0.11.2.5 — Prompt Detection Hardening & Version Housekeeping
- [x] Phase v0.11.3 — Self-Service Operations, Draft Amend & Plan Intelligence
- [x] Phase v0.11.3.1 — Shell Scroll & Help
- [x] Phase v0.11.4 — Plugin Registry & Project Manifest
- [x] Phase v0.11.4.1 — Shell Reliability: Command Output, Text Selection & Heartbeat
- [x] Phase v0.11.4.2 — Shell Mouse & Agent Session Fix
- [x] Phase v0.11.4.3 — Smart Input Routing & Intent Disambiguation
- [x] Phase v0.11.4.4 — Constitution Compliance Remediation
- [x] Phase v0.11.4.5 — Shell Large-Paste Compaction
- [x] Phase v0.11.5 — Web Shell UX, Agent Transparency & Parallel Sessions
- [x] Phase v0.11.6 — Constitution Audit Completion (§5–§14
- [x] Phase v0.11.7 — Web Shell Stream UX Polish
- [x] Phase v0.12.0 — Template Projects & Bootstrap Flow
- [x] Phase v0.12.0.1 — PR Merge & Main Sync Completion
- [x] Phase v0.12.0.2 — VCS Adapter Externalization
- [x] Phase v0.12.1 — Discord Channel Polish
- [x] Phase v0.12.2 — Shell Paste-at-End UX
- [x] Phase v0.12.2.1 — Draft Compositing: Parent + Child Chain Merge
- [ ] Phase v0.12.2.2 — Draft Apply: Transactional Rollback on Validation Failure
- [ ] Phase v0.12.3 — Shell Multi-Agent UX & Resilience
- [ ] Phase v0.12.4 — Plugin Template Publication & Registry Bootstrap
- [ ] Phase v0.13.0 — Reflink/COW Overlay Optimization
- [ ] Phase v0.13.1 — Autonomous Operations & Self-Healing Daemon
- [ ] Phase v0.13.2 — MCP Transport Abstraction (TCP/Unix Socket
- [ ] Phase v0.13.3 — Runtime Adapter Trait
- [ ] Phase v0.13.4 — External Action Governance Framework
- [ ] Phase v0.13.5 — Database Proxy Plugins
- [ ] Phase v0.13.6 — Community Knowledge Hub Plugin (Context Hub Integration
- [ ] Phase v0.13.7 — Goal Workflows: Serial Chains, Parallel Swarms & Office Routing
- [ ] Phase v0.13.8 — Local Model Support: Plan Phase
- [ ] Phase v0.13.9 — Compliance-Ready Audit Ledger
- [ ] Phase v0.14.0 — Agent Sandboxing & Process Isolation
- [ ] Phase v0.14.1 — Hardware Attestation & Verifiable Audit Trails
- [ ] Phase v0.14.2 — Multi-Party Approval & Threshold Governance
- [ ] Phase v0.14.3 — Product Constitution Framework

## Prior Context (from TA memory)

The following knowledge was captured from previous sessions across all agent frameworks.

### Architecture

- **[architecture] arch:module-map:goal-3fd8b562**: {"file_count":2,"modules":["ta-submit"],"summary":"Fix two bugs that caused full_lifecycle_detect_save_commit_restore to panic on Linux CI: (1) watchdog thread join blocked for full timeout_ms on every plugin call, making the test take 80+ seconds and risk CI timeout; (2) shell script used sed BRE backreferences to parse JSON method name, which behaves differently on Linux dash+sed vs macOS bash+sed."}
- **[architecture] arch:module-map:goal-465f4842**: {"file_count":6,"modules":["ta-changeset","ta-cli"],"summary":"Fix RenderContext build errors: revert file_filters (Vec<String>) back to file_filter (Option<String>) across mod.rs, all output adapters, and draft.rs. The prior goal introduced file_filters but all call sites still expected file_filter, causing two E0560 compile errors."}
- **[architecture] arch:module-map:goal-f705c889**: {"file_count":18,"modules":["ta-submit","ta-cli","ta-daemon"],"summary":"Implement v0.11.7 — Web Shell Stream UX Polish, plus fix two compile errors in draft.rs and pr.rs caught during draft apply (field rename file_filter→file_filters and removal of stale `files` field)."}
- **[architecture] arch:module-map:goal-e0abdd06**: {"file_count":8,"modules":["ta-submit","ta-cli"],"summary":"v0.10.18.3 — Verification Streaming, Heartbeat & Configurable Timeout. Replaces silent fire-and-forget verification with real-time streaming output, progress heartbeats, per-command configurable timeouts, and enhanced timeout error messages. Also adds mouse wheel/touchpad scroll to the shell TUI."}
- **[architecture] arch:module-map:goal-5f2b6f18**: {"file_count":17,"modules":["ta-cli","ta-mcp-gateway","ta-goal","ta-workflow","ta-daemon"],"summary":"Fix pre-commit verification failure on ta draft apply: add --skip-verify flag, restore VCS state on failure, and provide actionable recovery guidance. Also includes all v0.10.18 deferred items (goal chaining, process engine I/O, scoring agent, multi-project refactor, thread tracking, config hot-reload, pre-release sync/build wiring)."}
- **[architecture] arch:module-map:goal-0046f885**: {"file_count":14,"modules":["ta-mcp-gateway","ta-goal","ta-workflow","ta-daemon","ta-cli"],"summary":"v0.10.18 — Deferred Items: Workflow & Multi-Project. Implements all 7 deferred items from workflow engine (v0.9.8.2) and multi-project (v0.9.10) phases: goal chaining context propagation, full async process engine I/O, live scoring agent integration, GatewayState multi-project refactor, thread context tracking, config hot-reload, and pre-release sync/build wiring."}
- **[architecture] arch:module-map:goal-7a207eff**: {"file_count":10,"modules":["ta-cli","ta-daemon"],"summary":"Implement v0.10.17 — `ta new` — Conversational Project Bootstrapping. Adds the `ta new` CLI command for interactive project creation through a planner agent, with language-specific scaffold generation, template integration, version schema selection, daemon API for remote bootstrapping, and post-creation handoff. Also adds `--version-schema` to `ta plan create`."}
- **[architecture] arch:module-map:goal-d01f0930**: {"file_count":13,"modules":["ta-daemon","ta-changeset","ta-cli"],"summary":"Implement v0.10.16 — Deferred Items: Platform & Channel Hardening. Adds cross-platform signal handling with graceful shutdown and PID file, sandbox configuration section, Unix domain socket config field, auto-start daemon from ta shell, channel access control (denied_roles/denied_users), agent tool access control (allow/deny per agent), plugin version checking and upgrade management. 16 new tests across 2 files. 8 remaining items (MSI, Slack/Discord modals, IMAP, webhooks, marketplace) deferred as they require external service integration."}
- **[architecture] arch:module-map:goal-22253ef1**: {"file_count":11,"modules":["ta-audit","ta-mcp-gateway","ta-events","ta-cli"],"summary":"Implement v0.10.15 — Deferred Items: Observability & Audit. Adds per-tool-call MCP audit logging with agent identity and caller mode, event store pruning CLI and trait method, auto-approve verification gate (require_tests_pass/require_clean_clippy), auto-apply for approved drafts, --require-review flag to bypass auto-approve, and auto_approval audit trail entries. 9 new tests across 3 crates."}
- **[architecture] arch:module-map:goal-492fac59**: {"file_count":7,"modules":["ta-cli"],"summary":"Implement v0.10.13 — `ta plan add` Command (Agent-Powered Plan Updates). Adds a new `ta plan add <description>` CLI command that uses a planner agent to intelligently modify an existing PLAN.md through interactive dialog or non-interactive auto mode. Includes full plan context awareness, placement hints, version number continuity, and diff-based output through standard draft review. 13 new tests."}


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
- Always run `cargo fmt --all -- --check` before every `git push`
- Commit in logical working units
- All work stays within ~/development/TrustedAutonomy/
- Use `tempfile::tempdir()` for all test fixtures that need filesystem access

## Deferred Items Policy

Completed phases must not contain open "Remaining (deferred)" lists. When finishing a phase:

1. **Review every planned item** — verify each is done, partially done, or not started.
2. **If an item is not done**, discuss it with the user before closing the phase:
   - Is it still needed?
   - Which future phase should own it?
   - Should it be dropped?
3. **Move items to their target phase** with a one-line note (e.g., `→ v0.11.4`).
4. **Replace the "Remaining" section** with "Deferred items moved/resolved" showing where each item went or that it was completed in another phase.
5. **Never leave unchecked `[ ]` items in a `done` phase.** Every item is either checked `[x]`, moved to a named future phase, or explicitly dropped with rationale.

## Observability Mandate

All outcomes must be **observable** (with details and logging) and **actionable** (user knows what to do next). This applies to every error path, timeout, status message, and user-facing output.

- **Error messages**: Always include what happened, what was being attempted, and what the user can do about it. Never return bare "Error" or "failed" without context.
- **Timeouts**: State which operation timed out, what the timeout duration was, and how to configure it.
- **CLI output**: Commands should confirm what they did, not just succeed silently. Include counts, paths, and IDs where relevant.
- **Daemon/API errors**: Include the command or endpoint, relevant IDs, and suggest next steps.
- **Logging**: Use `tracing::warn`/`tracing::error` for operational issues. Include structured fields (command, duration, path) not just messages.
- **Scripts**: Print what step is running, what binary is being used, and on failure, print the exact command that can be re-run manually.

## Current State

- **Current version**: `0.12.2-alpha.2`
- See **PLAN.md** for the canonical development roadmap with per-phase status
- `ta plan list` / `ta plan status` show current progress
- Goals can link to plan phases: `ta run "title" --phase 4b`
- `ta draft apply` auto-updates PLAN.md when a phase completes

## Version Management

When completing a phase, you MUST update versions as part of the work:

1. **`apps/ta-cli/Cargo.toml`**: Update `version` to the phase's target version (e.g., `"0.2.0-alpha"`)
2. **This file (`CLAUDE.md`)**: Update "Current version" above to match
3. **`PLAN.md`**: Mark the phase `<!-- status: done -->` (done automatically by `ta draft apply --phase`)
4. **`docs/USAGE.md`**: Update with any new commands, flags, config options, or workflow changes. USAGE.md is the user onboarding guide — write feature documentation as "how to" sections, not version-annotated changelogs. Keep version references out of feature descriptions (use the Roadmap section for version tracking). When adding a new workflow or command, add it to the appropriate section with a clear code example.

Version format: `MAJOR.MINOR.PATCH-alpha` (semver). See `PLAN.md` "Versioning & Release Policy" for the full mapping of phases to versions. Sub-phases use pre-release dot notation: `v0.4.1.2` → `0.4.1-alpha.2`.

### How It Works (Overlay Flow)
1. `ta goal start "title" --phase 4b` → copies project to `.ta/staging/`
2. `ta run "title" --phase 4b` → creates goal + injects CLAUDE.md (with plan context) + launches agent + builds draft on exit
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

