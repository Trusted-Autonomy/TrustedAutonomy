# ta-plan-wayfinder Feature-Gate Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop compiling Wayfinder-specific networking/credential code (`ta-plan-wayfinder`) into every default TA build, while keeping the `PlanStore` trait as the single stable integration seam that both a vanilla TA install and the future `ta-virtual-team`/Wayfinder-pairing work (Stage 4) plug into.

**Architecture:** The `PlanStore` trait (`crates/ta-plan/src/store.rs`) already **is** the plan abstraction — it's a dyn-dispatched (`Box<dyn PlanStore>`), backend-agnostic interface with `FilePlanStore` as one implementation and `WayfinderPlanStore` (in `ta-plan-wayfinder`) as another. No new trait design is needed. The actual coupling is structural: `ta-daemon` and `ta-mcp-gateway` both carry an unconditional path-dependency on `ta-plan-wayfinder` and call `ta_plan_wayfinder::select_plan_store(...)` directly, so every default `cargo build --workspace` compiles in `ta-plan-wayfinder`'s `reqwest`/`url`/`ta-credentials` transitively, whether or not anyone ever sets `[plan] backend = "wayfinder"`.

This plan makes that dependency **optional and feature-gated** (`wayfinder-plan` Cargo feature, default off) in both consuming crates, rather than physically relocating `ta-plan-wayfinder`'s source into the private `ta-virtual-team` repo. Reasoning: Wayfinder plan-sync is a Wayfinder-account feature, not a virtual-team-paid-add-on feature — a Wayfinder user who never buys the virtual-team add-on should still be able to build TA with `--features wayfinder-plan` and sync PLAN.md status. Moving the crate's source into the private `ta-virtual-team` repo would make that impossible (it would become paid-add-on-only) and would also break the crate's public/open-source licensing story (`license = "Apache-2.0"`, public `repository` field). Keeping the crate in the TA repo but off-by-default in the two binaries that use it removes 100% of the unwanted build weight for users who don't need it, while leaving it available to anyone who opts in — including, later, `ta-virtual-team`'s own build, which can enable the feature.

**"The hook" for Stage 4 Wayfinder pairing:** `ta-virtual-team`'s future Wayfinder-pairing work (topics/registration, REST poller, etc.) does not need its own plan-sync mechanism — it consumes the exact same `Box<dyn PlanStore>` returned by `select_plan_store`, either by (a) depending on TA built with `--features wayfinder-plan` if it links `ta-daemon`/`ta-mcp-gateway` directly, or (b) depending on `ta-plan-wayfinder` itself directly (it's a normal public crate, nothing about this plan prevents that) if virtual-team needs the `WayfinderPlanStore` type without going through TA's HTTP API. Either path terminates at the same `PlanStore` trait object — one integration seam, not two.

**Tech Stack:** Rust, Cargo workspace features (`optional = true` deps + `[features]` tables), existing `ta-plan`/`ta-submit`/`ta-plan-wayfinder` crates. No new libraries.

## Global Constraints

- No behavior change for any existing deployment: default `cargo build --workspace` (no `--features` flag) must produce identical runtime behavior to today for every caller that has `[plan] backend` unset or `= "file"`.
- `[plan] backend = "wayfinder"` on a build compiled *without* `wayfinder-plan` must fail with a clear, actionable error (per this project's Observability Mandate) — not a silent fallback to the file backend.
- Do not touch `ta-agent-whiteboard` or its call sites — out of scope for this plan (confirmed correctly-scoped core infra in a separate investigation).
- Do not modify the `PlanStore` trait, `FilePlanStore`, or anything inside `crates/ta-plan-wayfinder/src/` — this plan only changes how the two *consumer* crates depend on and call into `ta-plan-wayfinder`.
- Follow this project's four-check verification (`cargo build`, `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`) via `./dev`, for **both** the default feature set and `--features wayfinder-plan`, before any commit lands.
- This is a `feature/` branch + PR per this repo's Git Workflow (code change, not docs-only).

---

### Task 1: Register the phase in PLAN.md

**Files:**
- Modify: `PLAN.md` (add a new phase entry after the last `v0.18.4` entry, around line 10822+)

**Interfaces:**
- Produces: phase ID `v0.18.5`, which later tasks' commit messages reference.

- [ ] **Step 1: Add the phase entry**

Insert after the end of the existing `v0.18.4` section in `PLAN.md`:

```markdown
### v0.18.5 — Feature-Gate ta-plan-wayfinder (Optional PlanStore Backend)
<!-- status: pending -->
**Depends on**: v0.17.11.7

Stop compiling `ta-plan-wayfinder` (Wayfinder HTTP client, `ta-credentials`,
`url`, `reqwest`) into every default TA build. Add an off-by-default
`wayfinder-plan` Cargo feature to `ta-daemon` and `ta-mcp-gateway`; gate
their two `ta_plan_wayfinder::select_plan_store` call sites behind it with a
clear runtime error when `[plan] backend = "wayfinder"` is requested on a
build that lacks the feature. No change to the `PlanStore` trait itself —
it already is the stable integration seam. Full design and task breakdown:
`docs/superpowers/plans/2026-09-09-ta-plan-wayfinder-extraction.md`.
```

- [ ] **Step 2: Commit**

```bash
git add PLAN.md
git commit -m "docs: register v0.18.5 (feature-gate ta-plan-wayfinder)"
```

---

### Task 2: Make `ta-plan-wayfinder` an optional dependency of `ta-daemon`

**Files:**
- Modify: `crates/ta-daemon/Cargo.toml`

**Interfaces:**
- Produces: a `wayfinder-plan` feature on the `ta-daemon` crate, enabling `dep:ta-plan-wayfinder`.
- Produces: `ta-daemon` gains a new dependency on `ta-submit` (previously absent — needed by Task 4's fallback path to read `[plan] backend` from `.ta/workflow.toml`; `ta-submit` itself has no HTTP/credentials dependencies, so this doesn't reintroduce the coupling being removed).

- [ ] **Step 1: Locate the current dependency line**

`crates/ta-daemon/Cargo.toml` currently has (confirm the exact line before editing, it may have moved):

```toml
ta-plan-wayfinder = { path = "../ta-plan-wayfinder", version = "0.17.11-alpha.7" }
```

- [ ] **Step 2: Make it optional and add `ta-submit`**

Replace that line with:

```toml
ta-plan-wayfinder = { path = "../ta-plan-wayfinder", version = "0.17.11-alpha.7", optional = true }
ta-submit = { path = "../ta-submit", version = "0.17.11-alpha.7" }
```

- [ ] **Step 3: Add the feature table**

Add (or extend, if a `[features]` table already exists by the time this runs) at the end of `crates/ta-daemon/Cargo.toml`:

```toml
[features]
wayfinder-plan = ["dep:ta-plan-wayfinder"]
```

- [ ] **Step 4: Verify it still compiles with default features**

Run: `./dev cargo check -p ta-daemon`
Expected: compiles (this will show an unresolved-import error at `crates/ta-daemon/src/api/plan.rs`'s `ta_plan_wayfinder::select_plan_store` call — that's expected and fixed in Task 4; if the error is anything else, stop and investigate).

- [ ] **Step 5: Commit**

```bash
git add crates/ta-daemon/Cargo.toml
git commit -m "feat: make ta-plan-wayfinder an optional dependency of ta-daemon"
```

---

### Task 3: Make `ta-plan-wayfinder` an optional dependency of `ta-mcp-gateway`

**Files:**
- Modify: `crates/ta-mcp-gateway/Cargo.toml`

**Interfaces:**
- Produces: a `wayfinder-plan` feature on the `ta-mcp-gateway` crate, enabling `dep:ta-plan-wayfinder`. `ta-mcp-gateway` already depends on `ta-submit` (confirmed: `crates/ta-mcp-gateway/Cargo.toml:43`) — no new dependency needed here.

- [ ] **Step 1: Locate and modify the dependency line**

`crates/ta-mcp-gateway/Cargo.toml` currently has:

```toml
ta-plan-wayfinder = { path = "../ta-plan-wayfinder", version = "0.17.11-alpha.7" }
```

Replace with:

```toml
ta-plan-wayfinder = { path = "../ta-plan-wayfinder", version = "0.17.11-alpha.7", optional = true }
```

- [ ] **Step 2: Add the feature table**

Add at the end of `crates/ta-mcp-gateway/Cargo.toml`:

```toml
[features]
wayfinder-plan = ["dep:ta-plan-wayfinder"]
```

- [ ] **Step 3: Verify it still compiles with default features**

Run: `./dev cargo check -p ta-mcp-gateway`
Expected: compiles except for the same expected unresolved-import error at `crates/ta-mcp-gateway/src/tools/plan.rs`, fixed in Task 5.

- [ ] **Step 4: Commit**

```bash
git add crates/ta-mcp-gateway/Cargo.toml
git commit -m "feat: make ta-plan-wayfinder an optional dependency of ta-mcp-gateway"
```

---

### Task 4: Gate the `ta-daemon` call site

**Files:**
- Modify: `crates/ta-daemon/src/api/plan.rs:830-848` (the `select_plan_store` call inside the phase-claim handler)
- Test: `crates/ta-daemon/src/api/plan.rs` (add the two unit tests below in the file's existing `#[cfg(test)] mod tests` block)

**Interfaces:**
- Consumes: `ta_submit::WorkflowConfig::load_or_default` (existing, public — `crates/ta-submit/src/config.rs`), `ta_submit::WorkflowConfig.plan.backend: String` (existing public field, `crates/ta-submit/src/config.rs:713`), `ta_plan::FilePlanStore::new(project_root, goals_dir) -> anyhow::Result<FilePlanStore>` (existing), `ta_plan_wayfinder::select_plan_store(project_root, goals_dir) -> anyhow::Result<Box<dyn PlanStore>>` (existing, only reachable behind the feature).
- Produces: a private `open_plan_store` helper function other tasks do not depend on (it's call-site-local).

- [ ] **Step 1: Write the failing tests**

Add to the existing test module at the bottom of `crates/ta-daemon/src/api/plan.rs`:

```rust
#[cfg(not(feature = "wayfinder-plan"))]
#[test]
fn open_plan_store_defaults_to_file_backend_without_the_feature() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("PLAN.md"),
        "### v0.1.0 — First phase\n<!-- status: pending -->\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".ta/goals")).unwrap();

    let store = open_plan_store(dir.path(), dir.path().join(".ta/goals")).unwrap();
    assert_eq!(store.backend_name(), "file");
}

#[cfg(not(feature = "wayfinder-plan"))]
#[test]
fn open_plan_store_rejects_wayfinder_backend_without_the_feature() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("PLAN.md"),
        "### v0.1.0 — First phase\n<!-- status: pending -->\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".ta/goals")).unwrap();
    std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
    std::fs::write(
        dir.path().join(".ta").join("workflow.toml"),
        "[plan]\nbackend = \"wayfinder\"\n",
    )
    .unwrap();

    let err = open_plan_store(dir.path(), dir.path().join(".ta/goals"))
        .err()
        .unwrap();
    assert!(err.to_string().contains("wayfinder-plan"));
}
```

- [ ] **Step 2: Run the tests to verify they fail (helper doesn't exist yet)**

Run: `./dev cargo test -p ta-daemon open_plan_store`
Expected: FAIL with "cannot find function `open_plan_store`"

- [ ] **Step 3: Replace the call site with the cfg-gated helper**

In `crates/ta-daemon/src/api/plan.rs`, find:

```rust
        let store: Box<dyn ta_plan::PlanStore> =
            match ta_plan_wayfinder::select_plan_store(&state.project_root, &state.goals_dir) {
```

Replace the whole `let store: Box<dyn ta_plan::PlanStore> = match ... { Ok(s) => s, Err(e) => { ... } };` block's call with:

```rust
        let store: Box<dyn ta_plan::PlanStore> =
            match open_plan_store(&state.project_root, &state.goals_dir) {
```

(everything else in that `match` arm — the `Ok(s) => s` and error-response `Err(e) => { ... }` arms — stays exactly as-is; only the function being called changes.)

Then add the helper function above the `impl` block or handler function that contains this call site (module-level, same file):

```rust
/// Opens the configured `PlanStore` backend. Behind `--features
/// wayfinder-plan`, delegates to `ta_plan_wayfinder::select_plan_store`,
/// which itself reads `.ta/workflow.toml`'s `[plan] backend`. Without the
/// feature, only the file backend is available — requesting `"wayfinder"`
/// is a clear config error rather than a silent fallback, since a synced
/// Wayfinder status mirror silently *not* syncing would be far more
/// confusing to debug than a build-time-flavored error at startup.
#[cfg(feature = "wayfinder-plan")]
fn open_plan_store(
    project_root: impl AsRef<std::path::Path>,
    goals_dir: impl AsRef<std::path::Path>,
) -> anyhow::Result<Box<dyn ta_plan::PlanStore>> {
    ta_plan_wayfinder::select_plan_store(project_root, goals_dir)
}

#[cfg(not(feature = "wayfinder-plan"))]
fn open_plan_store(
    project_root: impl AsRef<std::path::Path>,
    goals_dir: impl AsRef<std::path::Path>,
) -> anyhow::Result<Box<dyn ta_plan::PlanStore>> {
    let project_root = project_root.as_ref();
    let workflow_toml = project_root.join(".ta").join("workflow.toml");
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

    if workflow_config.plan.backend == "wayfinder" {
        anyhow::bail!(
            "[plan] backend = \"wayfinder\" requires a ta-daemon binary built with the \
             `wayfinder-plan` Cargo feature — this one was not. Rebuild with \
             `cargo build -p ta-daemon --features wayfinder-plan`, or set \
             [plan] backend = \"file\" in .ta/workflow.toml."
        );
    }

    Ok(Box::new(ta_plan::FilePlanStore::new(project_root, goals_dir)?))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `./dev cargo test -p ta-daemon open_plan_store`
Expected: PASS (both tests)

- [ ] **Step 5: Run full default-feature build+test for this crate**

Run: `./dev cargo test -p ta-daemon`
Expected: PASS, no regressions

- [ ] **Step 6: Commit**

```bash
git add crates/ta-daemon/src/api/plan.rs
git commit -m "feat: gate ta-daemon's wayfinder PlanStore call site behind wayfinder-plan feature"
```

---

### Task 5: Gate the `ta-mcp-gateway` call site

**Files:**
- Modify: `crates/ta-mcp-gateway/src/tools/plan.rs:273-279`
- Test: same file's `#[cfg(test)] mod tests` block

**Interfaces:**
- Consumes: same as Task 4 (`ta_submit::WorkflowConfig`, `ta_plan::FilePlanStore`, `ta_plan_wayfinder::select_plan_store`).
- Produces: a private `open_plan_store` helper local to this file (separate from `ta-daemon`'s — two call sites, kept independent per this plan's Global Constraints not touching shared trait/crate code; duplicating ~20 lines across 2 files is not worth a shared crate for 2 call sites, per this project's YAGNI convention).

- [ ] **Step 1: Write the failing tests**

Add to this file's test module:

```rust
#[cfg(not(feature = "wayfinder-plan"))]
#[test]
fn open_plan_store_defaults_to_file_backend_without_the_feature() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("PLAN.md"),
        "### v0.1.0 — First phase\n<!-- status: pending -->\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".ta/goals")).unwrap();

    let store = open_plan_store(dir.path(), dir.path().join(".ta/goals")).unwrap();
    assert_eq!(store.backend_name(), "file");
}

#[cfg(not(feature = "wayfinder-plan"))]
#[test]
fn open_plan_store_rejects_wayfinder_backend_without_the_feature() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("PLAN.md"),
        "### v0.1.0 — First phase\n<!-- status: pending -->\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
    std::fs::create_dir_all(dir.path().join(".ta/goals")).unwrap();
    std::fs::write(
        dir.path().join(".ta").join("workflow.toml"),
        "[plan]\nbackend = \"wayfinder\"\n",
    )
    .unwrap();

    let err = open_plan_store(dir.path(), dir.path().join(".ta/goals"))
        .err()
        .unwrap();
    assert!(err.to_string().contains("wayfinder-plan"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `./dev cargo test -p ta-mcp-gateway open_plan_store`
Expected: FAIL with "cannot find function `open_plan_store`"

- [ ] **Step 3: Replace the call site**

In `crates/ta-mcp-gateway/src/tools/plan.rs`, find:

```rust
    let store =
        ta_plan_wayfinder::select_plan_store(&state.config.workspace_root, &state.config.goals_dir)
            .map_err(|e| {
                McpError::internal_error(format!("failed to open PlanStore: {}", e), None)
            })?;
```

Replace with:

```rust
    let store = open_plan_store(&state.config.workspace_root, &state.config.goals_dir)
        .map_err(|e| McpError::internal_error(format!("failed to open PlanStore: {}", e), None))?;
```

Add the helper function at module level in the same file:

```rust
/// See `ta-daemon`'s `open_plan_store` (`crates/ta-daemon/src/api/plan.rs`)
/// for the identical rationale — kept as a separate small function here
/// rather than a shared crate for 2 call sites.
#[cfg(feature = "wayfinder-plan")]
fn open_plan_store(
    project_root: impl AsRef<std::path::Path>,
    goals_dir: impl AsRef<std::path::Path>,
) -> anyhow::Result<Box<dyn ta_plan::PlanStore>> {
    ta_plan_wayfinder::select_plan_store(project_root, goals_dir)
}

#[cfg(not(feature = "wayfinder-plan"))]
fn open_plan_store(
    project_root: impl AsRef<std::path::Path>,
    goals_dir: impl AsRef<std::path::Path>,
) -> anyhow::Result<Box<dyn ta_plan::PlanStore>> {
    let project_root = project_root.as_ref();
    let workflow_toml = project_root.join(".ta").join("workflow.toml");
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

    if workflow_config.plan.backend == "wayfinder" {
        anyhow::bail!(
            "[plan] backend = \"wayfinder\" requires a ta-mcp-gateway binary built with the \
             `wayfinder-plan` Cargo feature — this one was not. Rebuild with \
             `cargo build -p ta-mcp-gateway --features wayfinder-plan`, or set \
             [plan] backend = \"file\" in .ta/workflow.toml."
        );
    }

    Ok(Box::new(ta_plan::FilePlanStore::new(project_root, goals_dir)?))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `./dev cargo test -p ta-mcp-gateway open_plan_store`
Expected: PASS (both tests)

- [ ] **Step 5: Run full default-feature build+test for this crate**

Run: `./dev cargo test -p ta-mcp-gateway`
Expected: PASS, no regressions

- [ ] **Step 6: Commit**

```bash
git add crates/ta-mcp-gateway/src/tools/plan.rs
git commit -m "feat: gate ta-mcp-gateway's wayfinder PlanStore call site behind wayfinder-plan feature"
```

---

### Task 6: Verify the default build actually drops the weight, and the feature build still works

**Files:**
- None modified — verification only.

**Interfaces:**
- Consumes: `cargo tree` (cargo's own dependency-graph inspector).

- [ ] **Step 1: Confirm `ta-plan-wayfinder` is absent from the default dependency graph**

Run:
```bash
./dev cargo tree --workspace -e normal | grep -c ta-plan-wayfinder
```
Expected: `0` (previously this would have shown non-zero, via `ta-daemon` and `ta-mcp-gateway`)

- [ ] **Step 2: Confirm the full workspace still builds and tests clean by default**

Run: `./dev cargo build --workspace`
Expected: success

Run: `./dev cargo test --workspace`
Expected: all tests pass

- [ ] **Step 3: Confirm the feature-enabled build works too**

Run:
```bash
./dev cargo build -p ta-daemon -p ta-mcp-gateway --features ta-daemon/wayfinder-plan,ta-mcp-gateway/wayfinder-plan
```
Expected: success, and:
```bash
./dev cargo tree --workspace -e normal -p ta-daemon --features wayfinder-plan | grep -c ta-plan-wayfinder
```
Expected: `1` (present when the feature is on)

- [ ] **Step 4: Run clippy and fmt on both configurations**

Run: `./dev cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean

Run: `./dev cargo clippy -p ta-daemon -p ta-mcp-gateway --all-targets --features ta-daemon/wayfinder-plan,ta-mcp-gateway/wayfinder-plan -- -D warnings`
Expected: clean

Run: `./dev cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 5: No commit** — this task is verification only; if any step fails, fix the offending task above and re-run before continuing.

---

### Task 7: Update docs to describe the new architecture

**Files:**
- Modify: `CLAUDE.md` (Current State section — add one line noting the feature gate)
- Modify: `docs/USAGE.md` (per this project's convention: new build/config options get a "how to" section here)

**Interfaces:**
- None (docs only).

- [ ] **Step 1: Add a line to CLAUDE.md's Current State section**

In `CLAUDE.md`, under "Current State", add:

```markdown
- **Wayfinder plan-sync (`ta-plan-wayfinder`) is opt-in at build time**, not just at config time — `ta-daemon` and `ta-mcp-gateway` only compile it in with `--features wayfinder-plan`. Default builds use the `file` `PlanStore` backend exclusively. See `docs/superpowers/plans/2026-09-09-ta-plan-wayfinder-extraction.md` for the design.
```

- [ ] **Step 2: Add a "how to" section to docs/USAGE.md**

Find `docs/USAGE.md`'s section on plan/PLAN.md configuration (search for `[plan]` or `backend`), and add a subsection:

```markdown
### Syncing PLAN.md status to Wayfinder

By default, TA tracks phase and goal status entirely in the local PLAN.md
file. If you have a Wayfinder account and want phase/goal status mirrored
there too, two things are required:

1. Build `ta-daemon` and `ta-mcp-gateway` with the `wayfinder-plan` Cargo
   feature enabled:
   ```bash
   cargo build -p ta-daemon -p ta-mcp-gateway \
     --features ta-daemon/wayfinder-plan,ta-mcp-gateway/wayfinder-plan
   ```
   (Pre-built TA release binaries do not enable this feature by default.)
2. Add a `[plan]` section to `.ta/workflow.toml`:
   ```toml
   [plan]
   backend = "wayfinder"

   [plan.wayfinder]
   base_url = "https://your-wayfinder-instance.example.com"
   org_id = "..."
   project_id = "..."
   credential_name = "..."
   ```

Local PLAN.md remains the structural source of truth (phase list, dependency
graph); Wayfinder becomes a synced, human-visible status mirror on top of
it. Running `[plan] backend = "wayfinder"` against a binary built without
the `wayfinder-plan` feature fails fast with an actionable error at
startup rather than silently falling back to the file backend.
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/USAGE.md
git commit -m "docs: document the wayfinder-plan build-time feature gate"
```

---

### Task 8: Mark the phase done and open the PR

**Files:**
- Modify: `PLAN.md` (flip `v0.18.5`'s status marker)

- [ ] **Step 1: Mark the phase done**

In `PLAN.md`, change the `v0.18.5` entry added in Task 1 from:
```markdown
<!-- status: pending -->
```
to:
```markdown
<!-- status: done -->
```

- [ ] **Step 2: Final full verification**

Run all four required checks per this project's CLAUDE.md:
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```
Expected: all pass.

- [ ] **Step 3: Commit and push**

```bash
git add PLAN.md
git commit -m "docs: mark v0.18.5 done"
git push -u origin feature/wayfinder-plan-feature-gate
```

- [ ] **Step 4: Open the PR**

```bash
gh pr create --title "Feature-gate ta-plan-wayfinder (v0.18.5)" --body "$(cat <<'EOF'
## Summary
- ta-plan-wayfinder is no longer compiled into ta-daemon/ta-mcp-gateway by default; it's now behind an off-by-default `wayfinder-plan` Cargo feature on both crates
- [plan] backend = "wayfinder" on a build without the feature now fails with a clear, actionable error instead of silently working or breaking obscurely
- No behavior change for any existing deployment (file backend remains the default everywhere)
- This is the seam ta-virtual-team's future Wayfinder-pairing (Stage 4) work plugs into — no new abstraction needed, the PlanStore trait already was one

## Test plan
- [x] cargo tree confirms ta-plan-wayfinder is absent from the default workspace dependency graph
- [x] cargo build/test/clippy/fmt pass on default features
- [x] cargo build/test/clippy pass with --features wayfinder-plan on both crates
- [x] New unit tests cover both the file-backend fallback and the clear-error-when-feature-off cases
EOF
)"
```

---

## Self-Review

**Spec coverage:**
1. Read actual `PlanStore` trait + call sites and confirmed it's already generic — done (investigation above; no task needed to "generalize" it since it's already dyn-dispatched with no Wayfinder-specific assumptions).
2. Design the actual relocation with a concrete, non-optional decision (feature flag, not physical relocation) — done, justified in Architecture section against the "Wayfinder users without virtual-team" tension the user raised.
3. Plan how this becomes "the hook" for Stage 4 — done, in Architecture section's last paragraph.
4. Task for updating docs in both repos — Task 7 covers the TA repo (CLAUDE.md + USAGE.md per this project's own documentation convention). This plan does **not** include a `ta-virtual-team` repo doc task: at plan-writing time, Stage 4 (the only place `ta-virtual-team` would reference this mechanism) has zero code, so there is nothing there yet to update — that documentation belongs inside the eventual Stage 4 plan, not this one, per this project's "no placeholders" rule (a task with nothing concrete to write is a placeholder).
5. Task for verifying no regression in vanilla TA — Task 6.

**Placeholder scan:** no TBD/TODO, no "add appropriate error handling," no "similar to Task N" without inline code — every step has literal file paths, literal code, and literal expected command output.

**Type consistency:** `open_plan_store` signature (`impl AsRef<std::path::Path>, impl AsRef<std::path::Path>) -> anyhow::Result<Box<dyn ta_plan::PlanStore>>`) is identical across both `#[cfg]` arms in both Task 4 and Task 5, matching `ta_plan_wayfinder::select_plan_store`'s existing real signature (`impl AsRef<Path>, impl AsRef<Path>) -> anyhow::Result<Box<dyn PlanStore>>`, confirmed by reading `crates/ta-plan-wayfinder/src/select.rs:25-28`) and `ta_plan::FilePlanStore::new`'s real signature (confirmed by reading `crates/ta-plan/src/store.rs:166-174`).

---

Plan complete and saved to `docs/superpowers/plans/2026-09-09-ta-plan-wayfinder-extraction.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
