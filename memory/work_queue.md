---
name: work-queue
description: Tasks to execute only when no goal is in flight and apply.lock is absent
metadata:
  type: project
---

# Work Queue

Execute these only when `ta goal list` shows no running goal and `.ta/apply.lock` is absent.

---

## Ready to add to PLAN.md (v0.17.0.12.3 applied 2026-06-30)

### 1. Add plan phases v0.17.0.12.5, v0.17.0.12.6, v0.17.0.12.7 to PLAN.md

Insert after v0.17.0.12.4 and before v0.17.0.13. Full phase definitions below.

---

#### v0.17.0.12.5 — Studio Cleanup + Dashboard Immediacy

**Depends on**: v0.17.0.12.4

**Goal**: Fix three UX gaps with near-zero visual feedback: the 30s lag before dashboard shows a running goal, notifications with no timestamps, and disk-space notices with no temporal context. Fix the apply timeout and its misleading error message.

**Items**:

**Apply timeout (critical)**:
1. [ ] **Make apply async** (`crates/ta-daemon/src/web.rs` `apply_draft_endpoint`): Instead of blocking the HTTP response for up to 120s, spawn `ta draft apply` as a background task and return `{"status": "pending", "job_id": "<uuid>"}` immediately. Add `GET /api/apply-jobs/:job_id` returning `{"status": "running"|"done"|"failed", "output": "<last N lines>", "log_path": "<path>", "commit_sha": "..."}`. Write full stdout+stderr to `.ta/logs/apply-<draft-id>-<timestamp>.log` regardless of outcome.
2. [ ] **Studio: apply progress + log viewer**: Replace static "Applying draft…" with a polling progress indicator (every 2s via `GET /api/apply-jobs/:job_id`). On completion show `commit_sha`. On failure show a **"View Log"** button opening the log in a modal text viewer. Remove the `navigator.clipboard` fallback — clipboard is unreliable in non-HTTPS contexts and silently fails.
3. [ ] **Apply log directory**: Create `.ta/logs/` on daemon startup. Prune logs older than 30 days. Filename: `apply-<draft-short-id>-<timestamp>.log`.

**Dashboard immediacy (30s watchdog lag)**:
4. [ ] **Goal-started immediate event**: When `ta run` transitions a goal to `Running`, push a status-change event immediately (before the watchdog cycle). Studio picks it up on next tick; new goal appears in < 2s.

**Notification timestamps**:
5. [ ] **Timestamp on all notifications**: Add `timestamp: DateTime<Utc>` to the `Notification` struct in `crates/ta-events/src/notification.rs`. All creation sites stamp `Utc::now()`. Studio renders timestamp in notification list (e.g. "Jun 29 14:32").
6. [ ] **Disk space deduplication + timing**: `crates/ta-daemon/src/watchdog.rs` lines 302–326 fires a disk-low alert every 30s while disk is below 2 GB — producing 9+ identical notices with no "when." Fix: suppress re-fires for 10 minutes. Message includes: current free space, timestamp, threshold, and "first noticed at <time>".

#### Version: `0.17.0-alpha.12.5`

---

#### v0.17.0.12.6 — Studio Redesign + Smart Advisor

**Depends on**: v0.17.0.12.5

**Goal**: Redesign the Studio main page to surface the Advisor dialog and running goals without replacing the existing nav/health layout. Add element-level DOM updates so Studio never does a full page refresh. Rebuild the draft review panel with supervisor context, per-file selection, dependency warnings, and an inline Q&A dialog. Ship the Smart Advisor backend.

**Items**:

**Studio element updates (no full page refresh)**:
1. [ ] Replace full-page reloads with targeted DOM/component updates in Studio. On each status poll tick, only re-render fields that changed. Applies to all pages.

**Main page layout redesign**:
2. [ ] Top nav bar stays unchanged (Dashboard, Plan, Review Drafts, Settings, etc.).
3. [ ] Project health section stays (8 stat boxes). No layout change.
4. [ ] **Advisor dialog**: New section below the 8 health boxes on the main dashboard page. Shows ongoing conversation with the Smart Advisor. Input field always visible. Scrollable log of past exchanges. Dialog persisted per-project under `.ta/advisor-history.jsonl`.
5. [ ] **Active tab**: New top-nav entry "Active". Lists all `Running`/`Configured` goals. Each row is expandable — shows: title, elapsed time, last event, and a free-text input ("Send info / ask this agent") that posts to the running agent via `ta_ask_human` MCP tool or `POST /api/goals/:id/message`.
6. [ ] **Stats page**: New top-nav "Stats" page. Shows: total goals, completion rate, average duration, goals by phase, plan velocity. Integrates Meridian KPI data if `meridian.toml` is configured — shows per-category KPI alignment scores inline.

**Draft review panel redesign**:
7. [ ] **Supervisor review section**: Below the draft summary, add a collapsible "Supervisor Review" section (default open). Shows the initial supervisor review output from the goal's audit trail. Formatted with: risk level, issues list, recommendation.
8. [ ] **Summary "why" section**: Draft summary rendered with a distinct "Why" subsection (pulled from goal title and phase description) above the list of changes.
9. [ ] **Changes section with per-file selection**: Rename "Changed files" to "Changes." Each file entry has a checkbox (default: checked = selected for apply). User can deselect files to exclude them from the apply.
10. [ ] **Dependency warnings**: Detect inter-file dependencies. If file A is selected but a dependency of A is not selected, surface a "Warnings" section above the Approve/Apply buttons. On Approve/Apply with active warnings, show an "Are you sure?" confirmation modal.
11. [ ] **Per-draft Q&A dialog**: Each draft review panel has a chat window (styled like the Claude CLI sidebar) for questions to the advisor/supervisor about that draft. Stored in `.ta/drafts/<id>-dialog.jsonl`. Allows asking: "Is this safe to apply?", "What does change X do?", "Can I apply just the UI changes?"
12. [ ] **Advisor actions from draft dialog**: The per-draft dialog also accepts commands: "add item X to the plan", "create a --follow-up goal to fix Y", "amend this draft to also include Z". Routed through the Smart Advisor.

**Smart Advisor backend**:
13. [ ] **Intent classification** (`crates/ta-advisor/src/classify.rs`): Classify free-text input as: `queue_goal`, `info_request`, `draft_action` (amend/follow-up), or `ambiguous`. Reuse `ta advisor ask` classifier.
14. [ ] **Unambiguous path**: For `queue_goal`, show confirmation card (title, phase, estimated duration) with Approve / Edit / Cancel. On Approve, call `ta goal start`.
15. [ ] **Ambiguous path**: Show numbered clarification options. Re-classify (max 2 rounds), then surface "I need more info."
16. [ ] **Info request**: Answer from daemon state (`/api/status`, goal list, plan phase) without spawning a goal.
17. [ ] USAGE.md: "Studio Smart Advisor" section — what it can do, how to queue work, draft Q&A, --follow-up from dialog.

#### Version: `0.17.0-alpha.12.6`

---

#### v0.17.0.12.7 — Merge Shared Files for Parallel Work

**Depends on**: v0.17.0.12.6

**Goal**: When Advisor-triggered plan changes and in-flight goal drafts both modify the same files (PLAN.md, CLAUDE.md, Cargo.toml, memory/*.md), `ta draft apply` currently overwrites main with the staging snapshot — discarding any direct edits made while the goal was running. This file was deleted by PR #519 because work_queue.md was created on main after the goal's staging copy was made. Fix with a 3-way merge and advisor patch queue.

**Problem**: `ta draft apply` diffs staging vs source at the time the goal *started*, then copies changed files. If a file was also changed on main between goal-start and apply, the main changes are lost. Victims: PLAN.md, CLAUDE.md, Cargo.toml, memory/*.md.

**Items**:
1. [ ] **Apply-time 3-way merge for shared files**: When `ta draft apply` encounters a file changed in both staging and main since goal-start, perform a 3-way merge (base = pre-goal snapshot, ours = main, theirs = staging). Use `diffy` or `similar` crate. On clean merge, apply result. On conflict, transition to `ConflictResolution` state and surface to Studio.
2. [ ] **Pre-apply snapshot**: At `ta goal start`, snapshot content hash of every file staged. Store in `.ta/staging/<goal-id>/apply-base.json`. Used as merge base at apply time.
3. [ ] **Conflict resolution in Studio**: When apply encounters a merge conflict, show a diff editor for conflicted files in the draft review panel. User resolves, then applies.
4. [ ] **Advisor patch queue**: When the Advisor makes a direct change to a shared file while a goal is running, write the change as a patch to `.ta/advisor-patches/<timestamp>-<description>.patch` instead of directly to disk. Apply advisor patches after the 3-way merge at apply time.
5. [ ] USAGE.md: "Parallel Work and Shared Files" section.

**Scope note**: Full 3-way merge + conflict UI may move to v0.18. Item 4 (advisor patch queue) can ship in v0.17.0.12.7 as a partial fix — prevents silent overwrites of advisor changes before the merge is built.

#### Version: `0.17.0-alpha.12.7`

---

## VCS-ignore bugs surfaced 2026-07-01

Two bugs found when `demo.cast` (asciinema recording, gitignored) ended up in a draft and blocked apply.

### Bug 1: Source snapshot does not respect .gitignore

**Symptom**: `ta goal start` copies the full source directory into staging, including files matched by `.gitignore`. When a gitignored file changes on disk between goal-start and apply (e.g. a recording file growing from 45 → 16,442 lines), the rebase fires a destructive-change safety check and blocks apply.

**Root cause**: The source snapshot at `ta goal start` (or the rebase comparison at apply time) does not filter out VCS-ignored paths.

**Fix**: At snapshot time AND at rebase/apply time, check each file path against the active ignore rules (`.gitignore`, `.taignore`, `ide-excludes.json`). Exclude gitignored files from: source snapshot hashes, artifact candidacy, and rebase conflict detection. This is VCS-agnostic — for git: `git check-ignore -q <path>` or use the `ignore` crate to evaluate `.gitignore` patterns without shelling out.

**Scope**: `crates/ta-changeset/src/` (snapshot and rebase logic), `crates/ta-daemon/src/api/plan.rs` (apply path).

### Bug 2: Agent modifying a gitignored file should surface a warning

**Symptom**: An agent can write to a gitignored file in the staging workspace and TA includes it as a draft artifact without any indication that it is gitignored in the source repo.

**Fix**: At `ta draft build` time, check each artifact path against VCS ignore rules. If the file is gitignored:
- **Exclude it from draft artifacts** (the main fix — gitignored files cannot be applied to the real repo via `git commit` anyway)
- **Log a one-line warning**: `[warn] Artifact '<path>' matches .gitignore and was excluded from this draft.`

This prevents agents from accidentally modifying `.cast`, `.log`, `target/`, `node_modules/` etc. and having those appear as draft changes.

**Note**: If the agent *intentionally* needs to create a gitignored file (e.g. a build artifact, local config), the user should first remove the pattern from `.gitignore` or add the file to `.taignore` explicitly.

**Suggested phase**: Add to v0.17.0.12.7 (merge/apply safety) or as v0.17.0.12.8 if 12.7 is already scoped.

---

## Orphaned goals cleaned (2026-06-30)

- 77aa2585 (v0.17.0.2) — deleted; plan phase reset to pending
- ce28f48d (v0.16.1.6.1) — deleted; plan phase reset to pending
- 0641bea5 (v0.17.0.4.5) — deleted; plan phase reset to pending; staging dir removed
