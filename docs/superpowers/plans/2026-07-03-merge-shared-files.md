# Merge Shared Files for Parallel Work (v0.17.0.12.7) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `ta draft apply` from silently discarding direct edits to shared files (PLAN.md, CLAUDE.md, Cargo.toml, memory/*.md) that were made on `main` while a goal's staging copy was also editing them. Do this with a real 3-way merge whose base is the actual goal-start content (not git HEAD, which can be stale or absent), and a patch queue so Advisor-triggered direct writes never race a running goal's apply.

**Architecture:** `ta goal start` captures a `SharedFileBase` (real byte content, not just hashes) for the shared-file set into `.ta/staging/<goal-id>/apply-base.json`. `ta draft apply` loads it onto the `OverlayWorkspace` and, when a shared file has a true conflict, always attempts a 3-way merge using that base (existing `Diff3MergeTool`/`git merge-file` machinery, extended to accept an explicit base instead of only reconstructing from git HEAD). Clean merges apply transparently; unresolved conflicts are written to a sidecar under `.ta/goals/<goal-id>/conflicts/` and surfaced read-only in Studio (interactive diff-editor resolution is deferred to v0.18 per this phase's own scope note) — the goal transitions to `GoalRunState::Custom("conflict_resolution")` and apply stops for that file only. Advisor direct writes to shared files (`ta-daemon/src/api/plan.rs`, `dashboard_advisor.rs`) are queued to `.ta/advisor-patches/*.patch` (JSON: path + old/new content) instead of hitting disk immediately whenever another goal is active; `ta draft apply` replays queued patches after its own merge step using the same 3-way-merge machinery.

**Tech Stack:** Rust (existing `ta-workspace`, `ta-goal`, `ta-cli`, `ta-daemon` crates), no new external crates (`base64 = "0.22"` is already a workspace dependency, just needs adding to `ta-workspace/Cargo.toml`).

## Global Constraints

- Never disable or skip tests. Run `./dev cargo test --workspace` after every change.
- `./dev cargo clippy --workspace --all-targets -- -D warnings` and `./dev cargo fmt --all -- --check` must pass before considering any task done.
- `GoalRunState` is `#[non_exhaustive]`; use `GoalRunState::Custom("conflict_resolution".into())` rather than adding a first-class variant (existing doc comment on the enum mandates this pattern).
- Use `tempfile::tempdir()` for all test fixtures needing filesystem access.
- Do not add `---` horizontal rules inside PLAN.md phase bodies.
- Mark PLAN.md items `[x]` immediately once their code is written and compiles, per this phase's own instructions.

---

### Task 1: `SharedFileBase` — real-content snapshot of shared files

**Files:**
- Create: `crates/ta-workspace/src/shared_files.rs`
- Modify: `crates/ta-workspace/src/lib.rs` (add `pub mod shared_files;` and re-export `SharedFileBase`, `is_shared_file`)
- Modify: `crates/ta-workspace/Cargo.toml` (add `base64 = { workspace = true }`)

**Interfaces:**
- Produces: `pub fn is_shared_file(rel_path: &str) -> bool`
- Produces: `pub struct SharedFileBase { pub created_at: u64, pub files: HashMap<String, String> }` (files: rel path -> base64 content)
- Produces: `impl SharedFileBase { pub fn capture(root: &Path) -> Self; pub fn save(&self, path: &Path) -> std::io::Result<()>; pub fn load(path: &Path) -> std::io::Result<Self>; pub fn get(&self, rel_path: &str) -> Option<Vec<u8>>; }`
- Consumed by: Task 2 (`OverlayWorkspace`), Task 3 (`ta goal start`), Task 4 (`ta draft apply`).

- [x] Step 1: Add `base64 = { workspace = true }` to `crates/ta-workspace/Cargo.toml` `[dependencies]`.
- [x] Step 2: Write `crates/ta-workspace/src/shared_files.rs` with the struct/functions above. Shared file set: exact paths `PLAN.md`, `CLAUDE.md`, `Cargo.toml`, `docs/USAGE.md`, plus any `memory/*.md`.
- [x] Step 3: Write tests in the same file: `is_shared_file_matches_exact_names`, `is_shared_file_matches_memory_md_glob`, `is_shared_file_rejects_unrelated_paths`, `shared_file_base_captures_existing_files`, `shared_file_base_skips_missing_files`, `shared_file_base_round_trips_through_save_load`, `shared_file_base_get_returns_none_for_untracked_path`.
- [x] Step 4: Run `./dev cargo test -p ta-workspace shared_files`. Expect all new tests pass.
- [x] Step 5: Add `pub mod shared_files;` to `crates/ta-workspace/src/lib.rs` and re-export the two public items alongside the crate's existing re-exports.
- [x] Step 6: Commit.

---

### Task 2: Fix `three_way_merge` to use an explicit base instead of only git HEAD

**Files:**
- Modify: `crates/ta-workspace/src/overlay.rs:1044-1146` (`three_way_merge` function body and its one call site at `~925`)
- Modify: `crates/ta-workspace/src/overlay.rs:2628` (existing test `three_way_merge_non_overlapping_succeeds` — update call signature)

**Interfaces:**
- Produces: `pub fn three_way_merge(explicit_base: Option<&[u8]>, staging_path: &Path, source_path: &Path) -> Result<MergeResult, Box<dyn std::error::Error>>` (drops the unused `_base_hash`/`_snap`/`_staging_dir` params, replacing with one real one).
- Consumes: nothing new.

- [x] Step 1: Change the signature to `pub fn three_way_merge(explicit_base: Option<&[u8]>, staging_path: &Path, source_path: &Path) -> Result<MergeResult, Box<dyn std::error::Error>>`.
- [x] Step 2: Inside, when `explicit_base.is_some()`, use those bytes directly as `base_bytes` (skip the git-root-walk / `git show HEAD:<path>` block entirely). When `None`, keep the existing git-HEAD reconstruction logic verbatim as the fallback (still returns the same `"Cannot reconstruct base content..."` error when git lookup fails).
- [x] Step 3: Update the one production call site (`apply_with_conflict_check`, ~line 925) — for now pass `None` (Task 5 wires the real value through). Update the test at `~2628` to pass `None` too and confirm it still exercises the git-HEAD fallback path.
- [x] Step 4: Add a new test `three_way_merge_uses_explicit_base_over_git_head`: create a temp dir, write a file, capture its bytes as `explicit_base`, then mutate the on-disk file (simulating git HEAD having moved on) and confirm the merge still treats the ORIGINAL captured bytes as base (i.e. a conflicting edit in `ours` merges cleanly against `explicit_base` even though `source_path`'s current content no longer matches, and would fail if git-HEAD reconstruction were used because the repo may not be a git repo at all in the test).
- [x] Step 5: Run `./dev cargo test -p ta-workspace overlay::tests`. Expect all pass, including the new test and the untouched `three_way_merge_non_overlapping_succeeds`.
- [x] Step 6: Commit.

---

### Task 3: Capture `apply-base.json` at `ta goal start`

**Files:**
- Modify: `apps/ta-cli/src/commands/goal.rs:639` (insert after the existing PLAN.md snapshot block, before `// Transition: Created → Configured → Running.`)

**Interfaces:**
- Consumes: `ta_workspace::shared_files::SharedFileBase::capture`/`save` (Task 1).
- Produces: a file at `overlay.staging_dir().join("apply-base.json")` — consumed by Task 5.

- [x] Step 1: After the existing PLAN.md-snapshot block (ends ~line 639), add:
  ```rust
  // v0.17.0.12.7: Snapshot real content of shared files (PLAN.md, CLAUDE.md,
  // Cargo.toml, docs/USAGE.md, memory/*.md) as the 3-way merge base for
  // apply-time conflict resolution — independent of git history so it still
  // works when a file was edited/created on main without a commit.
  let apply_base = ta_workspace::shared_files::SharedFileBase::capture(&source_dir);
  let apply_base_path = overlay.staging_dir().join("apply-base.json");
  if let Err(e) = apply_base.save(&apply_base_path) {
      tracing::warn!("Could not write apply-base.json: {}", e);
  }
  ```
- [x] Step 2: Write an integration test in `apps/ta-cli/src/commands/goal.rs`'s existing test module (follow the `TempDir` + `goal::execute(&GoalCommands::Start{...})` pattern already used there) named `goal_start_writes_apply_base_json_for_shared_files`: create a fake project with `PLAN.md` and `memory/notes.md` content, run `ta goal start`, assert `apply-base.json` exists in the staging dir and, when loaded via `SharedFileBase::load`, `get("PLAN.md")` and `get("memory/notes.md")` return the original bytes.
- [x] Step 3: Run `./dev cargo test -p ta-cli goal_start_writes_apply_base_json_for_shared_files`. Expect PASS.
- [x] Step 4: Commit.

---

### Task 4: `WorkspaceError::SharedFileConflicts` + always-merge behavior for shared files

**Files:**
- Modify: `crates/ta-workspace/src/error.rs` (add variant)
- Modify: `crates/ta-workspace/src/overlay.rs` (struct field, setter, `apply_with_conflict_check` shared-file branch)

**Interfaces:**
- Produces: `pub struct SharedFileConflict { pub path: String, pub conflicted_content: Vec<u8> }` (new, in `overlay.rs` or `conflict.rs`)
- Produces: `WorkspaceError::SharedFileConflicts { conflicts: Vec<SharedFileConflict> }`
- Produces: `impl OverlayWorkspace { pub fn set_apply_base(&mut self, base: ta_workspace::shared_files::SharedFileBase) }`
- Consumes: `three_way_merge` (Task 2), `is_shared_file` (Task 1).
- Consumed by: Task 6 (`draft.rs` apply flow).

- [x] Step 1: Add `apply_base: Option<crate::shared_files::SharedFileBase>` field to `OverlayWorkspace` (default `None` in all constructors), and:
  ```rust
  pub fn set_apply_base(&mut self, base: crate::shared_files::SharedFileBase) {
      self.apply_base = Some(base);
  }
  ```
- [x] Step 2: Add to `error.rs`:
  ```rust
  #[derive(Debug, Clone)]
  pub struct SharedFileConflict {
      pub path: String,
      pub conflicted_content: Vec<u8>,
  }
  ```
  and a `WorkspaceError` variant `SharedFileConflicts { conflicts: Vec<SharedFileConflict> }` with a `Display` message like `"{n} shared file(s) have unresolved merge conflicts: {paths}"`.
- [x] Step 3: In `apply_with_conflict_check`'s `true_conflicts` handling (currently branches purely on `resolution`), split `true_conflicts` into `shared` (path passes `crate::shared_files::is_shared_file`) and `other` BEFORE the `match resolution` block. For `shared`, unconditionally attempt the merge (same loop body as the existing `ConflictResolution::Merge` arm) using `explicit_base = self.apply_base.as_ref().and_then(|b| b.get(&path))` passed into the updated `three_way_merge`. Collect any still-conflicted shared files into `Vec<SharedFileConflict>` (capturing the conflict-marked `content` bytes from `MergeResult::Conflicted`). For `other`, keep the exact existing `match resolution { Abort | ForceOverwrite | Merge }` behavior unchanged.
- [x] Step 4: After processing both groups: if `shared_conflicts` (the still-unresolved ones) is non-empty, return `Err(WorkspaceError::SharedFileConflicts { conflicts: shared_conflicts })` — checked before falling through to the `other`-group's own abort/force logic, so shared-file conflicts are always reported distinctly even if `other` group resolves fine.
- [x] Step 5: Add tests in `overlay.rs`'s test module: `apply_with_conflict_check_auto_merges_clean_shared_file_conflict` (PLAN.md changed on both sides in non-overlapping sections — merges cleanly, applies without needing `--conflict-resolution merge`), `apply_with_conflict_check_reports_shared_file_conflicts_distinctly` (same line changed on both sides — expect `Err(WorkspaceError::SharedFileConflicts{..})`, not `ConflictDetected`), `apply_with_conflict_check_ignores_resolution_flag_for_shared_files` (pass `ConflictResolution::Abort` explicitly, confirm shared-file merge is still attempted rather than immediately erroring).
- [x] Step 6: Run `./dev cargo test -p ta-workspace`. Expect all pass.
- [x] Step 7: Commit.

---

### Task 5: GoalRunState conflict-resolution transitions

**Files:**
- Modify: `crates/ta-goal/src/goal_run.rs:150-198` (`can_transition_to`)

**Interfaces:**
- Consumes: `GoalRunState::Custom(String)` (existing).
- Produces: transition rules consumed by Task 6.

- [x] Step 1: In `can_transition_to`, add (inside the existing `matches!`, as new alternatives):
  ```rust
  | (GoalRunState::Approved { .. }, GoalRunState::Custom(tag)) if tag == "conflict_resolution"
  | (GoalRunState::PrReady, GoalRunState::Custom(tag)) if tag == "conflict_resolution"
  | (GoalRunState::UnderReview, GoalRunState::Custom(tag)) if tag == "conflict_resolution"
  ```
  Note: `matches!` doesn't allow a per-arm `if` inside a `|`-chain the way written above — express instead as an `||` on the whole macro call, or precompute a helper. Use this shape instead:
  ```rust
  pub fn can_transition_to(&self, next: &GoalRunState) -> bool {
      if matches!(next, GoalRunState::Failed { .. }) {
          return true;
      }
      if let GoalRunState::Custom(tag) = next {
          if tag == "conflict_resolution" {
              return matches!(
                  self,
                  GoalRunState::Approved { .. } | GoalRunState::PrReady | GoalRunState::UnderReview
              );
          }
      }
      if let GoalRunState::Custom(tag) = self {
          if tag == "conflict_resolution" {
              return matches!(next, GoalRunState::Applied | GoalRunState::PrReady);
          }
      }
      matches!( /* existing big tuple list, unchanged */ )
  }
  ```
- [x] Step 2: Add tests in `goal_run.rs`'s test module: `can_transition_from_approved_to_conflict_resolution`, `can_transition_from_conflict_resolution_to_applied`, `cannot_transition_from_created_to_conflict_resolution`, `can_still_transition_to_failed_from_conflict_resolution` (any-state-to-Failed guard still applies).
- [x] Step 3: Run `./dev cargo test -p ta-goal`. Expect all pass.
- [x] Step 4: Commit.

---

### Task 6: Wire it into `ta draft apply` (load apply-base, handle shared conflicts, write sidecars)

**Files:**
- Modify: `apps/ta-cli/src/commands/draft.rs` (near `6364-6421`, the `OverlayWorkspace::open` + snapshot-restore block, and around `6923` where `apply_with_conflict_check` is called)

**Interfaces:**
- Consumes: `SharedFileBase::load` (Task 1), `set_apply_base` (Task 4), `WorkspaceError::SharedFileConflicts` (Task 4), `GoalRunState::Custom` (Task 5).
- Produces: conflict sidecars at `config.goals_dir.join(&goal_id).join("conflicts").join(<sanitized path>.conflict)` — consumed by Task 8 (daemon API) and Task 9 (Studio UI).

- [x] Step 1: Right after `OverlayWorkspace::open(...)` (~line 6364-6369), load the base if present:
  ```rust
  let apply_base_path = overlay.staging_dir().join("apply-base.json");
  if apply_base_path.exists() {
      match ta_workspace::shared_files::SharedFileBase::load(&apply_base_path) {
          Ok(base) => overlay.set_apply_base(base),
          Err(e) => tracing::warn!("Could not load apply-base.json: {}", e),
      }
  }
  ```
- [x] Step 2: Where `apply_with_conflict_check` is called (~6923), match its result. On `Err(ta_workspace::WorkspaceError::SharedFileConflicts { conflicts })`:
  - For each conflict, sanitize `path` (replace `/` with `__`) and write `conflicted_content` to `config.goals_dir.join(&goal_id).join("conflicts").join(format!("{}.conflict", sanitized))`, creating parent dirs as needed.
  - Transition the goal to `GoalRunState::Custom("conflict_resolution".to_string())` via the store (mirror how other state transitions are already saved in this function).
  - Print (per Observability Mandate — what happened, what was attempted, what to do next):
    ```
    ⚠️  {n} shared file(s) have unresolved merge conflicts: PLAN.md, memory/x.md
        Conflict markers written to .ta/goals/<goal_id>/conflicts/
        Resolve manually, then re-run `ta draft apply <id>` (or resolve via Studio).
    ```
  - Return `Err(anyhow::anyhow!(...))` with the same information, so the CLI exit code reflects the unresolved state (existing callers already expect `apply_package` to return `anyhow::Result<()>`).
- [x] Step 3: Write a test `apply_writes_conflict_sidecar_and_transitions_state_on_shared_file_conflict` following the `apply_rollback_on_verification_failure` pattern (`TempDir` fake git project): create a goal, edit `PLAN.md` in staging on one line, edit the SAME line differently in the source project after goal start (simulating concurrent main edits), run `build_package`/`approve_package`/`apply_package`, assert the call errors, assert a `*.conflict` file exists under `.ta/goals/<goal_id>/conflicts/`, and assert the goal's state (reload via store) is `Custom("conflict_resolution")`.
- [x] Step 4: Write a second test `apply_auto_merges_shared_file_when_changes_are_non_overlapping`: same setup but staging and source touch different lines/sections of PLAN.md — assert `apply_package` succeeds and the resulting PLAN.md on disk contains both edits.
- [x] Step 5: Run `./dev cargo test -p ta-cli draft::tests`. Expect all pass (including pre-existing tests, unaffected).
- [x] Step 6: Commit.

---

### Task 7: Advisor patch queue — direct-write interception

**Files:**
- Create: `crates/ta-daemon/src/advisor_patch.rs`
- Modify: `crates/ta-daemon/src/lib.rs` (or wherever daemon submodules are declared — add `pub mod advisor_patch;`)
- Modify: `crates/ta-daemon/src/api/plan.rs:461` (`add_plan_phase`'s `std::fs::write(&plan_path, ...)`) and the `in_progress` marker write in `claim_phase` (~line 651)
- Modify: `crates/ta-daemon/src/api/dashboard_advisor.rs:195` area (the PLAN.md write inside `post_dialog`)

**Interfaces:**
- Produces: `pub fn queue_or_write(project_root: &Path, rel_path: &str, new_content: &[u8], description: &str, is_goal_active: impl Fn() -> bool) -> std::io::Result<()>` — writes directly when `is_goal_active()` is false; otherwise reads the current on-disk content as "old", and writes a JSON patch file to `.ta/advisor-patches/<unix_ts>-<slug(description)>.patch` with `{path, old_content_b64, new_content_b64, description, queued_at}`.
- Produces: `pub fn has_active_goal(store: &ta_goal::Store) -> bool` (or reuse whatever active-goal check `web.rs`/`status.rs` already does — check `crates/ta-daemon/src/api/status.rs` or `active.rs` for an existing "list active goals" helper before writing a new one).
- Consumed by: Task 8 (`ta draft apply` patch replay).

- [x] Step 1: Before writing `advisor_patch.rs`, grep `crates/ta-daemon/src/api/status.rs` and `active.rs` for an existing function that lists goals in `Running`/`Configured`/`PrReady`/`UnderReview`/`Approved` state — reuse it for `is_goal_active` rather than duplicating goal-store iteration logic.
- [x] Step 2: Write `advisor_patch.rs` with the `queue_or_write` function above (JSON patch format, base64 via the same `base64` crate — add `base64 = { workspace = true }` to `ta-daemon/Cargo.toml` if not already present).
- [x] Step 3: Write tests: `queue_or_write_writes_directly_when_no_active_goal`, `queue_or_write_queues_patch_when_goal_active`, `queue_or_write_patch_file_is_valid_json_with_expected_fields`.
- [x] Step 4: Run `./dev cargo test -p ta-daemon advisor_patch`. Expect PASS.
- [x] Step 5: In `plan.rs`'s `add_plan_phase` and the in-progress-marker write in `claim_phase`, replace the direct `std::fs::write(&plan_path, ...)` calls with `advisor_patch::queue_or_write(&project_root, "PLAN.md", new_content.as_bytes(), "<description of the action>", || advisor_patch::has_active_goal(&store))`. Same for `dashboard_advisor.rs`'s PLAN.md write in `post_dialog`.
- [x] Step 6: Run `./dev cargo test -p ta-daemon`. Expect all pass (existing `plan.rs`/`dashboard_advisor.rs` tests still pass — direct-write behavior is unchanged when no goal is active, which is what those tests exercise).
- [x] Step 7: Commit.

---

### Task 8: Replay advisor patches at apply time

**Files:**
- Modify: `apps/ta-cli/src/commands/draft.rs` (apply flow, after the shared-file merge step from Task 6)

**Interfaces:**
- Consumes: patch files written by Task 7, `three_way_merge`/`Diff3MergeTool` (Task 2 / existing).
- Produces: none further downstream — this is the terminal consumer.

- [x] Step 1: After the shared-file conflict handling in Task 6 completes successfully (i.e. `apply_with_conflict_check` returned `Ok`), scan `source_dir.join(".ta/advisor-patches")` for `*.patch` files (skip if dir absent).
- [x] Step 2: For each patch (parse the JSON shape from Task 7): read current on-disk content at `source_dir.join(&patch.path)`. If it equals `old_content` (nobody else touched it since the patch was queued): write `new_content` directly, delete the patch file. Else: run `three_way_merge(Some(&old_content), &tmp_file_containing_current_content, &tmp_file_containing_new_content)` — on `Clean`, write merged content and delete the patch file; on `Conflicted`, write the conflict sidecar (reuse the same `.ta/goals/<goal_id>/conflicts/` mechanism from Task 6) and leave the patch file in place for retry, printing an observable warning naming the file and next steps.
- [x] Step 3: Write a test `apply_replays_advisor_patch_after_merge` (TempDir fake project): queue a patch file by hand (matching Task 7's JSON shape) targeting `PLAN.md`, run `apply_package` for an unrelated goal, assert the patch's `new_content` ends up in `PLAN.md` on disk and the patch file is deleted.
- [x] Step 4: Write a test `apply_leaves_advisor_patch_and_writes_conflict_when_unmergeable`: pre-seed `PLAN.md`'s current content to differ from the patch's `old_content` in a conflicting way, run apply, assert the patch file still exists and a conflict sidecar was written.
- [x] Step 5: Run `./dev cargo test -p ta-cli draft::tests`. Expect all pass.
- [x] Step 6: Commit.

---

### Task 9: Studio — read-only conflict + advisor-patch surfacing

**Files:**
- Modify: `crates/ta-daemon/src/api/draft_dialog.rs` or wherever the draft JSON payload is assembled for `renderDraftDetail` (find via grep for the handler backing `/api/drafts` list/detail) — add `conflicts: Vec<{path, content}>` and `pending_advisor_patches: Vec<{path, description, queued_at}>` fields when the goal is `Custom("conflict_resolution")` or has patch files queued.
- Modify: `crates/ta-daemon/assets/index.html` (`renderDraftDetail`, ~2374-2514) — add a new `<details class="section">` block, positioned after "Warnings" (2451-2455) and before "Constitution Check" (2457), titled "⚠️ Merge Conflicts" that lists each conflicted file with its conflict-marked content in a read-only `<pre>` block and the instruction: "Resolve manually in the project files, then re-run `ta draft apply <id>`." Add a smaller "Queued Advisor Changes" line item if `pending_advisor_patches` is non-empty.

**Interfaces:**
- Consumes: sidecar files from Task 6/8, patch files from Task 7.

- [x] Step 1: Find the API handler that serializes a draft package for the frontend (grep `renderDraftDetail` call site in `index.html` for the fetch URL, then find the matching daemon handler).
- [x] Step 2: Add the two new fields, populated by reading `.ta/goals/<goal_id>/conflicts/*.conflict` and `.ta/advisor-patches/*.patch` from the project root (empty arrays when absent — no behavior change for the common case).
- [x] Step 3: Add the Studio section per the description above, using the existing `esc()` helper for the conflict content.
- [x] Step 4: Manually verify by running the daemon locally (`./dev cargo run -p ta-daemon` or the project's existing dev-run script) and hitting a draft with a synthetically-created conflict sidecar file — confirm the section renders. If no easy local run path exists in this environment, note that in the change summary instead of claiming manual verification.
- [x] Step 5: Commit.

---

### Task 10: Documentation

**Files:**
- Modify: `docs/USAGE.md` — add a "Parallel Work and Shared Files" section explaining: which files are treated as shared (PLAN.md, CLAUDE.md, Cargo.toml, docs/USAGE.md, memory/*.md), that `ta draft apply` now auto-merges non-overlapping concurrent edits to these files using the goal-start snapshot as the merge base, what happens on a real conflict (sidecar file + Studio surfacing + `Custom("conflict_resolution")` state), and how Advisor-triggered direct edits are queued as patches while a goal is running and replayed at apply time.
- Modify: `PLAN.md` — mark v0.17.0.12.7 items `[x]` with brief implementation notes; note that item 3's Studio surfacing is read-only in this phase (interactive diff-editor resolution deferred to v0.18, consistent with the phase's own scope note).

- [x] Step 1: Write the USAGE.md section.
- [x] Step 2: Update PLAN.md checkboxes/notes for v0.17.0.12.7 (do not touch the `<!-- status: ... -->` marker).
- [x] Step 3: Commit.

---

### Task 11: Full workspace verification

- [x] Step 1: `./dev cargo build --workspace`
- [x] Step 2: `./dev cargo test --workspace`
- [x] Step 3: `./dev cargo clippy --workspace --all-targets -- -D warnings`
- [x] Step 4: `./dev cargo fmt --all -- --check`
- [x] Step 5: Write `.ta/change_summary.json` and `.ta-decisions.json` per the goal's TA instructions, and final `.ta/ta-progress.json` checkpoint.
