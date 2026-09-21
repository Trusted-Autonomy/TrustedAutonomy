# Generic Cost-Experiment Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give TA core a generic, product-agnostic way to run a controlled cost experiment (e.g. "does having
the wiki available save net tokens") across real goal runs: assign goals to named arms, merge each arm's config
overrides into that goal's session, record the arm on the resulting velocity entry, and report the delta.

**Architecture:** New optional fields on `GoalRun` and `VelocityEntry` (`ta-goal` crate) carry experiment identity
through a goal's whole lifecycle, mirroring how `plan_phase` already does this today. A new `.ta/experiments/<id>.toml`
config format defines an experiment's arms as opaque config-override maps. A single hook at goal-creation time (the
same code path every goal launch already goes through, whether started by a human, `wake_listener.rs`, or
`poller_daemon`, since all of them ultimately invoke `ta run`) rolls the arm assignment. A new `ta experiment`
CLI verb group starts/stops experiments and reports deltas by reading the existing `.ta/velocity-history.jsonl`.
No new data store; no change to any goal that isn't participating in an experiment.

**Tech Stack:** Rust, `ta-goal` crate (`crates/ta-goal`), `ta-cli` app (`apps/ta-cli`), existing `serde`/`toml`/
`uuid`/`chrono` dependencies already used throughout these crates.

## Global Constraints

- Never use em dashes (`—`) in any code comment, doc comment, CLI output string, or commit message. Use a period,
  comma, colon, or parentheses instead.
- All four verification gates must pass before every commit: `./dev cargo build --workspace`,
  `./dev cargo test --workspace`, `./dev cargo clippy --workspace --all-targets -- -D warnings`,
  `./dev cargo fmt --all -- --check`.
- Work happens on a feature branch (`feature/generic-cost-experiment-framework`), never directly on `main`.
- `tempfile::tempdir()` for all test fixtures needing filesystem access.
- TA core must never reference "wiki" or any downstream product name; every field, struct, and CLI string in this
  plan is generic.
- Commit in logical, working units; run the full test suite after every code change, before every commit.

---

### Task 1: `GoalRun` and `VelocityEntry` experiment fields

**Files:**
- Modify: `crates/ta-goal/src/goal_run.rs` (add fields to `GoalRun` struct and its `new()` constructor)
- Modify: `crates/ta-goal/src/velocity.rs` (add fields to `VelocityEntry`, add builder methods, thread through
  `from_goal()`)
- Test: `crates/ta-goal/src/velocity.rs` (inline `#[cfg(test)]` module, following the file's existing convention)

**Interfaces:**
- Produces: `GoalRun.experiment_id: Option<String>`, `GoalRun.experiment_arm: Option<String>`,
  `GoalRun.experiment_pair_id: Option<Uuid>`, `GoalRun.experiment_overrides: Option<serde_json::Value>`,
  `GoalRun.workflow: Option<String>` (a new generic classification field, separate from `VelocityEntry.workflow`
  which stays a plain `String` for backward-compatible serialization)
- Produces: `VelocityEntry.experiment_id: Option<String>`, `VelocityEntry.experiment_arm: Option<String>`,
  `VelocityEntry.experiment_pair_id: Option<Uuid>`
- Produces: `VelocityEntry::with_experiment(mut self, id: impl Into<String>, arm: impl Into<String>, pair_id:
  Option<Uuid>) -> Self`
- Consumes: nothing new (this is the foundation task)

- [ ] **Step 1: Write the failing test for `GoalRun`'s new fields defaulting to `None`**

Add to `crates/ta-goal/src/goal_run.rs`'s existing test module (search for `mod tests` in that file):

```rust
#[test]
fn new_goal_run_defaults_experiment_fields_to_none() {
    let goal = GoalRun::new(
        "title",
        "objective",
        "agent",
        PathBuf::from("/tmp/ws"),
        PathBuf::from("/tmp/store"),
    );
    assert_eq!(goal.experiment_id, None);
    assert_eq!(goal.experiment_arm, None);
    assert_eq!(goal.experiment_pair_id, None);
    assert_eq!(goal.experiment_overrides, None);
    assert_eq!(goal.workflow, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `./dev cargo test -p ta-goal new_goal_run_defaults_experiment_fields_to_none -- --nocapture`
Expected: FAIL with "no field `experiment_id` on type `GoalRun`" (compile error, not a runtime assertion failure)

- [ ] **Step 3: Add the fields to `GoalRun`**

In `crates/ta-goal/src/goal_run.rs`, find the `pub struct GoalRun` definition and add these fields immediately
after the existing `plan_phase: Option<String>` field:

```rust
    /// Cost-experiment id this goal was assigned to at launch (e.g. "wiki-brain").
    /// `None` for goals not participating in any experiment. Generic: TA core
    /// never interprets this string, only stores and threads it through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// The arm this goal was assigned within `experiment_id` (e.g. "brain_on").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_arm: Option<String>,

    /// Links this goal to its paired counterpart's goal_run_id when the
    /// experiment's paired-shadow-sampling mode assigned it. `None` for an
    /// unpaired-holdout assignment or a non-participating goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_pair_id: Option<Uuid>,

    /// The resolved config-override map for this goal's assigned arm, merged
    /// into the staging copy's effective configuration at launch time.
    /// Opaque to TA core: keys and values are entirely the experiment
    /// definition's own concern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_overrides: Option<serde_json::Value>,

    /// Generic cost-category classification for this goal (e.g.
    /// "feature-work", "brain-maintenance"), set by whatever launched it
    /// (a wake-on-demand listener's persona config, a CLI flag). `None` when
    /// unclassified. Distinct from `VelocityEntry.workflow`, which is the
    /// plain-`String` form this field is copied into at completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
```

Then in `GoalRun::new()`'s struct literal, add the five new fields with `None` values, next to the existing
`plan_phase: None,` line:

```rust
            plan_phase: None,
            experiment_id: None,
            experiment_arm: None,
            experiment_pair_id: None,
            experiment_overrides: None,
            workflow: None,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `./dev cargo test -p ta-goal new_goal_run_defaults_experiment_fields_to_none -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write the failing test for `VelocityEntry`'s new fields and `with_experiment`**

Add to `crates/ta-goal/src/velocity.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn with_experiment_sets_all_three_fields() {
    let goal = test_goal_run(); // existing test helper in this file, constructs a minimal GoalRun
    let pair_id = Uuid::new_v4();
    let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied)
        .with_experiment("wiki-brain", "brain_off", Some(pair_id));
    assert_eq!(entry.experiment_id.as_deref(), Some("wiki-brain"));
    assert_eq!(entry.experiment_arm.as_deref(), Some("brain_off"));
    assert_eq!(entry.experiment_pair_id, Some(pair_id));
}

#[test]
fn from_goal_without_experiment_leaves_fields_none() {
    let goal = test_goal_run();
    let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
    assert_eq!(entry.experiment_id, None);
    assert_eq!(entry.experiment_arm, None);
    assert_eq!(entry.experiment_pair_id, None);
    assert_eq!(entry.workflow, "");
}

#[test]
fn from_goal_copies_goal_workflow_into_entry_workflow() {
    let mut goal = test_goal_run();
    goal.workflow = Some("brain-maintenance".to_string());
    let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
    assert_eq!(entry.workflow, "brain-maintenance");
}
```

(If this file has no `test_goal_run()` helper yet, check the existing tests around line 960-1030 for whatever
helper they already use to build a minimal `GoalRun` for a `VelocityEntry::from_goal` call, and reuse that name
instead of inventing a new one.)

- [ ] **Step 6: Run tests to verify they fail**

Run: `./dev cargo test -p ta-goal with_experiment_sets_all_three_fields from_goal_without_experiment_leaves_fields_none from_goal_copies_goal_workflow_into_entry_workflow -- --nocapture`
Expected: FAIL (compile error: no field `experiment_id` on `VelocityEntry`, no method `with_experiment`)

- [ ] **Step 7: Add the fields, builder method, and `from_goal` wiring to `VelocityEntry`**

In `crates/ta-goal/src/velocity.rs`, add to the `VelocityEntry` struct, after the existing `derived_title` field:

```rust
    /// Cost-experiment id, copied from the goal at completion time. See
    /// `GoalRun::experiment_id` for the generic, product-agnostic contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// The arm this goal ran under within `experiment_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_arm: Option<String>,

    /// Links this entry to its paired counterpart's entry when the goal was
    /// assigned via paired-shadow-sampling. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_pair_id: Option<Uuid>,
```

In `VelocityEntry::from_goal()`'s struct literal, change:

```rust
            workflow: String::new(),
```

to:

```rust
            workflow: goal.workflow.clone().unwrap_or_default(),
```

and add, alongside the other `None` fields in that same struct literal:

```rust
            experiment_id: goal.experiment_id.clone(),
            experiment_arm: goal.experiment_arm.clone(),
            experiment_pair_id: goal.experiment_pair_id,
```

Then add a new builder method, next to the existing `with_workflow`:

```rust
    /// Tag this entry with the cost experiment it ran under. Overrides
    /// whatever `from_goal` already copied from the `GoalRun`, so callers
    /// that assign experiments after the fact (rather than at launch) can
    /// still use this instead of constructing the fields by hand.
    pub fn with_experiment(
        mut self,
        experiment_id: impl Into<String>,
        arm: impl Into<String>,
        pair_id: Option<Uuid>,
    ) -> Self {
        self.experiment_id = Some(experiment_id.into());
        self.experiment_arm = Some(arm.into());
        self.experiment_pair_id = pair_id;
        self
    }
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `./dev cargo test -p ta-goal with_experiment_sets_all_three_fields from_goal_without_experiment_leaves_fields_none from_goal_copies_goal_workflow_into_entry_workflow new_goal_run_defaults_experiment_fields_to_none -- --nocapture`
Expected: PASS (all four)

- [ ] **Step 9: Run the full ta-goal suite to check nothing else broke**

Run: `./dev cargo test -p ta-goal`
Expected: PASS. Existing tests around lines 1186-1187 (`assert_eq!(entry.input_tokens, 100_000)`) must still pass
unmodified since the new fields are all `Option`/additive with `#[serde(default)]`.

- [ ] **Step 10: Commit**

```bash
git add crates/ta-goal/src/goal_run.rs crates/ta-goal/src/velocity.rs
git commit -m "feat: add generic cost-experiment fields to GoalRun and VelocityEntry"
```

---

### Task 2: Experiment config format and loader

**Files:**
- Create: `crates/ta-goal/src/experiment.rs`
- Modify: `crates/ta-goal/src/lib.rs` (add `pub mod experiment;` and re-export the new public types)
- Test: `crates/ta-goal/src/experiment.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing from Task 1 directly (this is a pure config/data module)
- Produces: `ExperimentConfig { id: String, holdout_fraction: f64, paired_fraction: f64,
  maintenance_workflow: Option<String>, arms: HashMap<String, serde_json::Value> }`,
  `ExperimentConfig::load(project_root: &Path, id: &str) -> Result<Option<Self>, GoalError>`,
  `ExperimentConfig::save(&self, project_root: &Path) -> Result<(), GoalError>`,
  `ExperimentConfig::list(project_root: &Path) -> Result<Vec<Self>, GoalError>`,
  `experiments_dir(project_root: &Path) -> PathBuf` returning `.ta/experiments/`

- [ ] **Step 1: Write the failing test for round-trip save/load**

Create `crates/ta-goal/src/experiment.rs` with just this test at the bottom (module skeleton comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn sample_config() -> ExperimentConfig {
        let mut arms = HashMap::new();
        arms.insert("brain_on".to_string(), serde_json::json!({}));
        arms.insert("brain_off".to_string(), serde_json::json!({"wiki.disabled": true}));
        ExperimentConfig {
            id: "wiki-brain".to_string(),
            holdout_fraction: 0.2,
            paired_fraction: 0.1,
            maintenance_workflow: Some("brain-maintenance".to_string()),
            arms,
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let config = sample_config();
        config.save(dir.path()).unwrap();

        let loaded = ExperimentConfig::load(dir.path(), "wiki-brain").unwrap();
        assert_eq!(loaded, Some(config));
    }

    #[test]
    fn load_missing_experiment_returns_none() {
        let dir = tempdir().unwrap();
        let loaded = ExperimentConfig::load(dir.path(), "does-not-exist").unwrap();
        assert_eq!(loaded, None);
    }

    #[test]
    fn list_returns_all_saved_experiments() {
        let dir = tempdir().unwrap();
        sample_config().save(dir.path()).unwrap();
        let mut other = sample_config();
        other.id = "shorter-prompt".to_string();
        other.save(dir.path()).unwrap();

        let mut ids: Vec<String> = ExperimentConfig::list(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["shorter-prompt".to_string(), "wiki-brain".to_string()]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `./dev cargo test -p ta-goal --lib experiment:: -- --nocapture`
Expected: FAIL with compile errors (module doesn't exist / no `ExperimentConfig` type yet)

- [ ] **Step 3: Implement `ExperimentConfig`**

At the top of `crates/ta-goal/src/experiment.rs`, above the test module from Step 1:

```rust
//! Generic cost-experiment definitions (`.ta/experiments/<id>.toml`).
//!
//! An experiment assigns goal runs to named arms, each an opaque config-
//! override map. This module owns only the storage format; arm assignment
//! and config-override application live in the CLI/gateway layers that
//! consume this (see `apps/ta-cli/src/commands/experiment.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::GoalError;

/// One named cost experiment: a set of arms, each an opaque config-override
/// map, plus the fractions controlling how goals get assigned to them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentConfig {
    pub id: String,
    /// Fraction of eligible goals assigned an arm via the cheap, continuous
    /// unpaired-holdout mode. 0.0-1.0.
    pub holdout_fraction: f64,
    /// Fraction of eligible goals additionally run as a paired canonical
    /// plus shadow sample. 0.0-1.0. Independent of `holdout_fraction`.
    #[serde(default)]
    pub paired_fraction: f64,
    /// If set, the `VelocityEntry.workflow` value this experiment's report
    /// should treat as maintenance cost to net out of the savings figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance_workflow: Option<String>,
    /// Arm name to its config-override map. Values are opaque JSON; this
    /// crate never interprets a key.
    pub arms: HashMap<String, serde_json::Value>,
}

/// `.ta/experiments/` relative to `project_root`.
pub fn experiments_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("experiments")
}

fn config_path(project_root: &Path, id: &str) -> PathBuf {
    experiments_dir(project_root).join(format!("{id}.toml"))
}

impl ExperimentConfig {
    /// Load one experiment by id. Returns `Ok(None)` (not an error) when no
    /// config file exists for that id, since "not currently defined" is an
    /// expected, common state.
    pub fn load(project_root: &Path, id: &str) -> Result<Option<Self>, GoalError> {
        let path = config_path(project_root, id);
        if !path.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| GoalError::IoError {
            path: path.clone(),
            source,
        })?;
        let config: Self = toml::from_str(&raw)
            .map_err(|e| GoalError::ParseError(format!("{}: {e}", path.display())))?;
        Ok(Some(config))
    }

    /// Write this experiment's config to `.ta/experiments/<id>.toml`,
    /// creating the directory if needed.
    pub fn save(&self, project_root: &Path) -> Result<(), GoalError> {
        let dir = experiments_dir(project_root);
        std::fs::create_dir_all(&dir).map_err(|source| GoalError::IoError {
            path: dir.clone(),
            source,
        })?;
        let path = config_path(project_root, &self.id);
        let raw = toml::to_string_pretty(self)
            .map_err(|e| GoalError::ParseError(format!("{}: {e}", path.display())))?;
        std::fs::write(&path, raw).map_err(|source| GoalError::IoError { path, source })
    }

    /// All experiments currently defined under `.ta/experiments/`. Returns
    /// an empty vec (not an error) when the directory doesn't exist yet.
    pub fn list(project_root: &Path) -> Result<Vec<Self>, GoalError> {
        let dir = experiments_dir(project_root);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|source| GoalError::IoError {
            path: dir.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| GoalError::IoError {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).map_err(|source| GoalError::IoError {
                path: path.clone(),
                source,
            })?;
            let config: Self = toml::from_str(&raw)
                .map_err(|e| GoalError::ParseError(format!("{}: {e}", path.display())))?;
            out.push(config);
        }
        Ok(out)
    }
}
```

Check `crates/ta-goal/src/error.rs` for `GoalError`'s exact existing variants before this step: if `IoError { path,
source }` or `ParseError(String)` aren't already there under those exact names, either reuse whatever equivalent
variants already exist (most likely candidates given this crate's other file-backed stores like
`VelocityHistoryStore`) or add them following that file's existing `thiserror` pattern. Do not invent a second
error type for this module.

Add `toml = "0.8"` to `crates/ta-goal/Cargo.toml`'s `[dependencies]` if it isn't already a dependency of this crate
(check first: `grep toml crates/ta-goal/Cargo.toml`). Several other crates in this workspace already depend on
`toml`, so it should already be pinned to a compatible version in the workspace root `Cargo.toml`; use
`toml = { workspace = true }` if this workspace uses a `[workspace.dependencies]` table (check
`grep -A2 "^toml" Cargo.toml` at the repo root first).

Add to `crates/ta-goal/src/lib.rs`:

```rust
pub mod experiment;
pub use experiment::{experiments_dir, ExperimentConfig};
```

(Match this crate's existing `pub use` style: check the lines immediately around the existing `pub mod velocity;`
to mirror how that module's types are re-exported, rather than assuming this exact form.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev cargo test -p ta-goal --lib experiment:: -- --nocapture`
Expected: PASS (all three tests)

- [ ] **Step 5: Run the full ta-goal suite**

Run: `./dev cargo test -p ta-goal`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ta-goal/src/experiment.rs crates/ta-goal/src/lib.rs crates/ta-goal/Cargo.toml
git commit -m "feat: add ExperimentConfig storage format for cost experiments"
```

---

### Task 3: Arm-assignment logic (unpaired holdout and paired shadow sampling)

**Files:**
- Modify: `crates/ta-goal/src/experiment.rs` (add assignment functions and their tests)

**Interfaces:**
- Consumes: `ExperimentConfig` (Task 2), `GoalRun` fields (Task 1)
- Produces: `pub enum ArmAssignment { None, Unpaired { arm: String }, Paired { canonical_arm: String,
  shadow_arm: String, pair_id: Uuid } }`, `pub fn assign_arm(config: &ExperimentConfig, rng: &mut impl
  rand::Rng) -> ArmAssignment`

- [ ] **Step 1: Write the failing tests for assignment distribution and shape**

Append to `crates/ta-goal/src/experiment.rs`'s test module:

```rust
    use rand::SeedableRng;

    fn two_arm_config(holdout: f64, paired: f64) -> ExperimentConfig {
        let mut arms = HashMap::new();
        arms.insert("brain_on".to_string(), serde_json::json!({}));
        arms.insert("brain_off".to_string(), serde_json::json!({"wiki.disabled": true}));
        ExperimentConfig {
            id: "wiki-brain".to_string(),
            holdout_fraction: holdout,
            paired_fraction: paired,
            maintenance_workflow: None,
            arms,
        }
    }

    #[test]
    fn zero_fractions_never_assigns() {
        let config = two_arm_config(0.0, 0.0);
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        for _ in 0..100 {
            assert_eq!(assign_arm(&config, &mut rng), ArmAssignment::None);
        }
    }

    #[test]
    fn holdout_only_never_produces_paired() {
        let config = two_arm_config(1.0, 0.0);
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        for _ in 0..100 {
            match assign_arm(&config, &mut rng) {
                ArmAssignment::Unpaired { .. } => {}
                other => panic!("expected Unpaired, got {other:?}"),
            }
        }
    }

    #[test]
    fn paired_assignment_uses_both_arm_names_and_a_fresh_pair_id() {
        let config = two_arm_config(0.0, 1.0);
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let mut seen_pair_ids = std::collections::HashSet::new();
        for _ in 0..20 {
            match assign_arm(&config, &mut rng) {
                ArmAssignment::Paired { canonical_arm, shadow_arm, pair_id } => {
                    assert_ne!(canonical_arm, shadow_arm);
                    assert!(config.arms.contains_key(&canonical_arm));
                    assert!(config.arms.contains_key(&shadow_arm));
                    assert!(seen_pair_ids.insert(pair_id), "pair_id must be fresh each call");
                }
                other => panic!("expected Paired, got {other:?}"),
            }
        }
    }

    #[test]
    fn holdout_fraction_converges_over_many_trials() {
        let config = two_arm_config(0.3, 0.0);
        let mut rng = rand::rngs::StdRng::seed_from_u64(4);
        let mut off_count = 0;
        let trials = 10_000;
        for _ in 0..trials {
            if let ArmAssignment::Unpaired { arm } = assign_arm(&config, &mut rng) {
                if arm == "brain_off" {
                    off_count += 1;
                }
            }
        }
        let observed_fraction = off_count as f64 / trials as f64;
        assert!(
            (observed_fraction - 0.15).abs() < 0.03,
            "expected roughly half of the 0.3 holdout fraction to land on brain_off, got {observed_fraction}"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./dev cargo test -p ta-goal --lib experiment:: -- --nocapture`
Expected: FAIL (compile error: no `ArmAssignment` type, no `assign_arm` function)

- [ ] **Step 3: Implement `ArmAssignment` and `assign_arm`**

Add `rand = { workspace = true }` (or `rand = "0.8"` if this crate has no workspace-level pin yet; check
`grep "^rand" Cargo.toml` at the repo root first, since other crates almost certainly already depend on `rand`) to
`crates/ta-goal/Cargo.toml`.

Add to `crates/ta-goal/src/experiment.rs`, above the test module:

```rust
use rand::Rng;

/// The outcome of rolling an experiment's arm assignment for one goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmAssignment {
    /// This goal is not part of the experiment (outside both fractions).
    None,
    /// Assigned a single arm via the cheap, continuous holdout mode.
    Unpaired { arm: String },
    /// Assigned to run as a paired canonical-plus-shadow sample. The caller
    /// is responsible for actually launching two goal runs and linking them
    /// with `pair_id`.
    Paired {
        canonical_arm: String,
        shadow_arm: String,
        pair_id: uuid::Uuid,
    },
}

/// Roll one goal's assignment for `config`. `paired_fraction` is checked
/// first (mutually exclusive with the unpaired holdout for that goal): a
/// goal is never both paired and separately holdout-assigned. Requires at
/// least two arms to produce anything but `ArmAssignment::None`; an
/// experiment with fewer than two arms is a configuration error the caller
/// should reject at `ta experiment start` time, not something this function
/// silently works around.
pub fn assign_arm(config: &ExperimentConfig, rng: &mut impl Rng) -> ArmAssignment {
    let mut arm_names: Vec<&String> = config.arms.keys().collect();
    arm_names.sort(); // deterministic ordering for reproducible tests and stable pair selection
    if arm_names.len() < 2 {
        return ArmAssignment::None;
    }

    if rng.gen::<f64>() < config.paired_fraction {
        let canonical_arm = arm_names[0].clone();
        let shadow_arm = arm_names[1].clone();
        return ArmAssignment::Paired {
            canonical_arm,
            shadow_arm,
            pair_id: uuid::Uuid::new_v4(),
        };
    }

    if rng.gen::<f64>() < config.holdout_fraction {
        let arm = arm_names[rng.gen_range(0..arm_names.len())].clone();
        return ArmAssignment::Unpaired { arm };
    }

    ArmAssignment::None
}
```

Note on `arm_names[0]`/`arm_names[1]` as "canonical"/"shadow": this assumes exactly two arms and a stable sort
order where the first sorted name is canonical. `"brain_off" < "brain_on"` alphabetically is false (`brain_off` <
`brain_on`), so with the `wiki-brain` config `brain_on` sorts second, not first. Since the design calls for
`brain_on` (the real default) to be canonical, either name the arms so the default sorts first, or make this
explicit rather than order-dependent: add an optional `canonical_arm: Option<String>` field to `ExperimentConfig`
in Task 2 (defaulting to `None`, meaning "first alphabetically") and use it here if set. Do the simpler,
explicit version: add that field to `ExperimentConfig` now (in Task 2's struct, with `#[serde(default)]`), and
change this function to prefer it:

```rust
    let canonical_arm = config
        .canonical_arm
        .clone()
        .unwrap_or_else(|| arm_names[0].clone());
    let shadow_arm = arm_names
        .iter()
        .find(|a| ***a != canonical_arm)
        .expect("at least two arms checked above")
        .to_string();
```

Use this version instead of the plain `arm_names[0]`/`arm_names[1]` version above, and update Task 2's
`ExperimentConfig` struct and its two constructor sites in this task's tests (`two_arm_config`, `sample_config`)
to include `canonical_arm: None` (defaults are fine for tests that don't care which arm is canonical).

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev cargo test -p ta-goal --lib experiment:: -- --nocapture`
Expected: PASS (all seven tests in this module now)

- [ ] **Step 5: Run the full ta-goal suite**

Run: `./dev cargo test -p ta-goal`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ta-goal/src/experiment.rs crates/ta-goal/Cargo.toml
git commit -m "feat: add arm-assignment logic for unpaired holdout and paired shadow sampling"
```

---

### Task 4: Wire arm assignment into goal creation (`ta run`)

**Files:**
- Modify: `apps/ta-cli/src/commands/run.rs`
- Test: `apps/ta-cli/src/commands/run.rs` (inline, following this file's existing test module conventions)

**Interfaces:**
- Consumes: `ta_goal::experiment::{ArmAssignment, ExperimentConfig, assign_arm}` (Tasks 2-3), `GoalRun`'s new
  fields (Task 1)
- Produces: goals created by `ta run` (and therefore by `wake_listener.rs` and any other caller shelling out to
  the `ta` binary) now carry `experiment_id`/`experiment_arm`/`experiment_pair_id`/`experiment_overrides` when an
  active experiment's roll selects them

- [ ] **Step 1: Locate the exact goal-creation point**

Run: `grep -n "GoalRun::new(\|plan_phase = Some" apps/ta-cli/src/commands/run.rs`

This should show the primary (non-follow-up, non-macro-sub-goal) construction site where a fresh `GoalRun` is
built for a top-level `ta run` invocation, and the nearby line where `--phase` gets copied into `plan_phase`. Read
50 lines around that site before writing this task's code, since the exact surrounding variable names
(`project_root`, `mut goal`, etc.) determine how the snippet below should be spliced in.

- [ ] **Step 2: Write the failing integration test**

Add a test in this file's existing test module (search for `mod tests` near the bottom of `run.rs`) that exercises
the real flow: write an `ExperimentConfig` with `holdout_fraction: 1.0` to a tempdir's `.ta/experiments/`, run
whatever this file's existing tests use to invoke goal creation (mirror an existing test in this file that
constructs a `GoalRun` through the real command path, rather than calling `GoalRun::new` directly, since the
point is to test the actual wiring, not re-test Task 3's already-tested `assign_arm`), and assert the resulting
`GoalRun.experiment_id == Some("wiki-brain".to_string())` and `experiment_arm` is one of the configured arm names.

```rust
#[test]
fn goal_creation_assigns_experiment_arm_when_holdout_fraction_is_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut arms = std::collections::HashMap::new();
    arms.insert("brain_on".to_string(), serde_json::json!({}));
    arms.insert("brain_off".to_string(), serde_json::json!({"wiki.disabled": true}));
    ta_goal::ExperimentConfig {
        id: "wiki-brain".to_string(),
        holdout_fraction: 1.0,
        paired_fraction: 0.0,
        maintenance_workflow: None,
        canonical_arm: None,
        arms,
    }
    .save(dir.path())
    .unwrap();

    // Use this file's existing goal-creation test helper here instead of a
    // made-up function name -- check what the nearest existing "create a
    // goal through the real command path" test in this module calls, and
    // use that exact helper and its exact signature.
    let goal = create_goal_for_test(dir.path(), "title", "objective");

    assert_eq!(goal.experiment_id.as_deref(), Some("wiki-brain"));
    assert!(["brain_on", "brain_off"].contains(&goal.experiment_arm.as_deref().unwrap()));
}
```

This step deliberately does not give a fully mechanical helper name (`create_goal_for_test`) because it must match
whatever this 10,000+ line file already uses for its own goal-creation tests. Find that helper first
(`grep -n "fn.*-> GoalRun\|fn test_goal\|fn create_test_goal" apps/ta-cli/src/commands/run.rs`) and use its real
name and signature.

- [ ] **Step 3: Run test to verify it fails**

Run: `./dev cargo test -p ta-cli goal_creation_assigns_experiment_arm_when_holdout_fraction_is_one -- --nocapture`
Expected: FAIL (either a compile error if the wiring doesn't exist yet, or an assertion failure showing
`experiment_id: None`)

- [ ] **Step 4: Implement the wiring**

At the goal-creation site found in Step 1, immediately after the point where `plan_phase` is set on the new
`GoalRun` (before the goal is persisted via `GoalRunStore::save`/`save_with_tag`), add:

```rust
    // Cost-experiment arm assignment: check every defined experiment, apply
    // the first one whose roll selects this goal. Multiple simultaneously
    // active experiments on the same goal are not supported in this pass;
    // `ta experiment start` should refuse to start a second experiment
    // while one is already active (enforced in Task 5).
    if let Ok(experiments) = ta_goal::ExperimentConfig::list(&project_root) {
        let mut rng = rand::thread_rng();
        for config in experiments {
            match ta_goal::experiment::assign_arm(&config, &mut rng) {
                ta_goal::experiment::ArmAssignment::None => continue,
                ta_goal::experiment::ArmAssignment::Unpaired { arm } => {
                    goal.experiment_id = Some(config.id.clone());
                    goal.experiment_overrides = config.arms.get(&arm).cloned();
                    goal.experiment_arm = Some(arm);
                    break;
                }
                ta_goal::experiment::ArmAssignment::Paired {
                    canonical_arm,
                    shadow_arm: _,
                    pair_id: _,
                } => {
                    // Paired mode requires launching a second goal run from
                    // the same source snapshot, which this single-goal
                    // creation path cannot do alone. Deferred to Task 6
                    // (poller_daemon / wake_listener launch integration),
                    // which owns spawning both runs. For now, a paired roll
                    // here degrades to an unpaired assignment on the
                    // canonical arm rather than silently dropping the
                    // experiment membership.
                    goal.experiment_id = Some(config.id.clone());
                    goal.experiment_overrides = config.arms.get(&canonical_arm).cloned();
                    goal.experiment_arm = Some(canonical_arm);
                    break;
                }
            }
        }
    }
```

Confirm `project_root` is already an in-scope variable at this point in the function (it should be, since staging
copy creation needs it too); if the in-scope name differs, use that name instead.

- [ ] **Step 5: Run test to verify it passes**

Run: `./dev cargo test -p ta-cli goal_creation_assigns_experiment_arm_when_holdout_fraction_is_one -- --nocapture`
Expected: PASS

- [ ] **Step 6: Run the full ta-cli suite**

Run: `./dev cargo test -p ta-cli`
Expected: PASS. This file is large; if unrelated pre-existing failures surface, confirm via `git stash` that they
predate this change before investigating further, rather than assuming this task caused them.

- [ ] **Step 7: Commit**

```bash
git add apps/ta-cli/src/commands/run.rs
git commit -m "feat: assign cost-experiment arms at goal creation"
```

---

### Task 5: `ta experiment start/stop/report` CLI commands

**Files:**
- Create: `apps/ta-cli/src/commands/experiment.rs`
- Modify: `apps/ta-cli/src/commands/mod.rs` (add `pub mod experiment;`)
- Modify: `apps/ta-cli/src/main.rs` (add the `Experiment` subcommand variant and its dispatch arm, mirroring the
  existing `Stats { command: commands::stats::StatsCommands }` pattern shown by
  `grep -n "Stats {" apps/ta-cli/src/main.rs`)
- Test: `apps/ta-cli/src/commands/experiment.rs` (inline)

**Interfaces:**
- Consumes: `ta_goal::ExperimentConfig` (Task 2), `ta_goal::VelocityHistoryStore` (already exists)
- Produces: `ta experiment start <id> --holdout-fraction <f> [--paired-fraction <f>]
  [--maintenance-workflow <tag>] --arm <name>=<json-overrides> [--arm <name>=<json-overrides> ...]`,
  `ta experiment stop <id>`, `ta experiment report <id>`

- [ ] **Step 1: Write the failing test for the report's aggregation math**

Create `apps/ta-cli/src/commands/experiment.rs` with this test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::{GoalOutcome, VelocityEntry, VelocityHistoryStore};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn entry(arm: &str, cost_usd: f64, workflow: &str) -> VelocityEntry {
        let goal_id = Uuid::new_v4();
        VelocityEntry {
            goal_id,
            title: "t".to_string(),
            workflow: workflow.to_string(),
            agent: "claude".to_string(),
            plan_phase: None,
            outcome: GoalOutcome::Applied,
            started_at: chrono::Utc::now(),
            pr_ready_at: None,
            completed_at: Some(chrono::Utc::now()),
            build_seconds: 60,
            review_seconds: 0,
            total_seconds: 60,
            amended: false,
            follow_up_count: 0,
            rework_seconds: 0,
            denial_reason: None,
            cancel_reason: None,
            machine_id: String::new(),
            committer: None,
            input_tokens: 1000,
            output_tokens: 500,
            cost_usd,
            model: "claude-sonnet-5".to_string(),
            cost_estimated: false,
            tokens_input: Some(1000),
            tokens_output: Some(500),
            derived_title: None,
            experiment_id: Some("wiki-brain".to_string()),
            experiment_arm: Some(arm.to_string()),
            experiment_pair_id: None,
        }
    }

    #[test]
    fn report_computes_group_means_and_nets_maintenance_cost() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history.append(&entry("brain_on", 0.10, "feature-work")).unwrap();
        history.append(&entry("brain_on", 0.12, "feature-work")).unwrap();
        history.append(&entry("brain_off", 0.20, "feature-work")).unwrap();
        history.append(&entry("brain_off", 0.18, "feature-work")).unwrap();
        let mut maintenance = entry("brain_on", 0.05, "brain-maintenance");
        maintenance.experiment_arm = None;
        history.append(&maintenance).unwrap();

        let report = build_report(dir.path(), "wiki-brain", Some("brain-maintenance")).unwrap();

        assert_eq!(report.unpaired.get("brain_on").unwrap().n, 2);
        assert!((report.unpaired.get("brain_on").unwrap().mean_cost_usd - 0.11).abs() < 1e-9);
        assert_eq!(report.unpaired.get("brain_off").unwrap().n, 2);
        assert!((report.unpaired.get("brain_off").unwrap().mean_cost_usd - 0.19).abs() < 1e-9);
        assert!((report.maintenance_cost_usd - 0.05).abs() < 1e-9);
        // net savings: (brain_off mean - brain_on mean) per goal, minus total maintenance cost
        assert!((report.net_savings_usd - (0.19 - 0.11) * 2.0 + 0.05).abs() < 1e-9);
    }

    #[test]
    fn report_with_no_matching_entries_returns_empty_groups() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history.append(&entry("brain_on", 0.10, "feature-work")).unwrap(); // wrong experiment below

        let report = build_report(dir.path(), "some-other-experiment", None).unwrap();
        assert!(report.unpaired.is_empty());
        assert_eq!(report.maintenance_cost_usd, 0.0);
    }
}
```

Double-check `VelocityHistoryStore::for_project` and `VelocityEntry`'s exact field list against
`crates/ta-goal/src/velocity.rs` before finalizing this test; the field list above should match what Task 1 left
in place, but confirm no field was missed or misnamed (this crate derives `Serialize`/`Deserialize`, so a missing
field is a compile error here, not a silent bug).

- [ ] **Step 2: Run test to verify it fails**

Run: `./dev cargo test -p ta-cli --lib experiment:: -- --nocapture`
Expected: FAIL (compile error: no `build_report` function, no `ExperimentReport` type)

- [ ] **Step 3: Implement the report aggregation**

Add to `apps/ta-cli/src/commands/experiment.rs`, above the test module:

```rust
//! `ta experiment` command group: define, start/stop, and report on generic
//! cost experiments. See `ta_goal::experiment` for the underlying config
//! format and arm-assignment logic this command group manages.

use std::collections::HashMap;
use std::path::Path;

use ta_goal::{ExperimentConfig, GoalError, VelocityHistoryStore};

#[derive(Debug, Clone, PartialEq)]
pub struct ArmStats {
    pub n: usize,
    pub mean_cost_usd: f64,
    pub mean_input_tokens: f64,
    pub mean_output_tokens: f64,
    pub stddev_cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExperimentReport {
    pub experiment_id: String,
    pub unpaired: HashMap<String, ArmStats>,
    pub maintenance_cost_usd: f64,
    /// (arm_a_mean - arm_b_mean) x n, minus maintenance cost. Only
    /// meaningful with exactly two arms present; `0.0` otherwise, which
    /// callers should treat as "not computable," not "no savings."
    pub net_savings_usd: f64,
}

fn mean_and_stddev(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if values.len() < 2 {
        return (mean, 0.0);
    }
    let variance =
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    (mean, variance.sqrt())
}

/// Read `.ta/velocity-history.jsonl` under `project_root`, filter to
/// `experiment_id`, and compute per-arm statistics plus the maintenance-
/// netted savings figure.
pub fn build_report(
    project_root: &Path,
    experiment_id: &str,
    maintenance_workflow: Option<&str>,
) -> Result<ExperimentReport, GoalError> {
    let history = VelocityHistoryStore::for_project(project_root);
    let entries = history.read_all()?; // confirm this method name exists on VelocityHistoryStore;
                                        // if it's named differently (e.g. `load_all`, `all`), use that name

    let mut by_arm: HashMap<String, Vec<f64>> = HashMap::new();
    let mut maintenance_cost_usd = 0.0;

    for e in entries {
        if let Some(tag) = maintenance_workflow {
            if e.workflow == tag {
                maintenance_cost_usd += e.cost_usd;
                continue;
            }
        }
        if e.experiment_id.as_deref() != Some(experiment_id) {
            continue;
        }
        let Some(arm) = e.experiment_arm.clone() else {
            continue;
        };
        by_arm.entry(arm).or_default().push(e.cost_usd);
    }

    let unpaired: HashMap<String, ArmStats> = by_arm
        .iter()
        .map(|(arm, costs)| {
            let (mean, stddev) = mean_and_stddev(costs);
            (
                arm.clone(),
                ArmStats {
                    n: costs.len(),
                    mean_cost_usd: mean,
                    mean_input_tokens: 0.0, // populated in a follow-up pass if needed; cost is the primary signal
                    mean_output_tokens: 0.0,
                    stddev_cost_usd: stddev,
                },
            )
        })
        .collect();

    let net_savings_usd = if unpaired.len() == 2 {
        let mut arms: Vec<&ArmStats> = unpaired.values().collect();
        arms.sort_by(|a, b| a.mean_cost_usd.partial_cmp(&b.mean_cost_usd).unwrap());
        let cheaper = arms[0];
        let pricier = arms[1];
        (pricier.mean_cost_usd - cheaper.mean_cost_usd) * pricier.n as f64 - maintenance_cost_usd
    } else {
        0.0
    };

    Ok(ExperimentReport {
        experiment_id: experiment_id.to_string(),
        unpaired,
        maintenance_cost_usd,
        net_savings_usd,
    })
}
```

Before finalizing, check `crates/ta-goal/src/velocity.rs`'s actual `VelocityHistoryStore` API (`grep -n "pub fn"
crates/ta-goal/src/velocity.rs` around its `impl VelocityHistoryStore` block) for the real name of the
"read every entry" method; the code above assumes `read_all()` but the real name may differ, and `for_project`
was confirmed to exist in the earlier grep (`Self::new(project_root.as_ref().join(".ta/velocity-history.jsonl"))`)
but double check its exact signature (does it take `impl AsRef<Path>` or `&Path`?) before using it.

Add the `start`/`stop` command handlers and CLI struct wiring (a smaller, mechanical piece, following this
directory's existing command-handler pattern, e.g. `apps/ta-cli/src/commands/stats.rs`'s top-level `pub fn
execute(command: StatsCommands, config: ...)` shape):

```rust
#[derive(Debug, clap::Subcommand)]
pub enum ExperimentCommands {
    /// Define and activate a cost experiment.
    Start {
        id: String,
        #[arg(long)]
        holdout_fraction: f64,
        #[arg(long, default_value_t = 0.0)]
        paired_fraction: f64,
        #[arg(long)]
        maintenance_workflow: Option<String>,
        #[arg(long)]
        canonical_arm: Option<String>,
        /// Repeatable: `--arm name=<json-object>`, e.g. `--arm brain_off='{"wiki.disabled":true}'`
        #[arg(long = "arm", value_parser = parse_arm)]
        arms: Vec<(String, serde_json::Value)>,
    },
    /// Deactivate a cost experiment (its config file is removed; historical
    /// velocity-history.jsonl entries already tagged with it are untouched).
    Stop { id: String },
    /// Print the delta report for an experiment.
    Report { id: String },
}

fn parse_arm(s: &str) -> Result<(String, serde_json::Value), String> {
    let (name, json) = s
        .split_once('=')
        .ok_or_else(|| "expected name=<json-object>".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid JSON for arm {name}: {e}"))?;
    Ok((name.to_string(), value))
}

pub fn execute(command: ExperimentCommands, project_root: &Path) -> anyhow::Result<()> {
    match command {
        ExperimentCommands::Start {
            id,
            holdout_fraction,
            paired_fraction,
            maintenance_workflow,
            canonical_arm,
            arms,
        } => {
            if ExperimentConfig::list(project_root)?
                .iter()
                .any(|c| c.id != id)
                && !ExperimentConfig::list(project_root)?.is_empty()
            {
                anyhow::bail!(
                    "an experiment is already active; only one active experiment is supported in this release. \
                     Run `ta experiment stop <id>` first."
                );
            }
            let config = ExperimentConfig {
                id: id.clone(),
                holdout_fraction,
                paired_fraction,
                maintenance_workflow,
                canonical_arm,
                arms: arms.into_iter().collect(),
            };
            config.save(project_root)?;
            println!("Experiment '{id}' started: holdout_fraction={holdout_fraction}, paired_fraction={paired_fraction}");
        }
        ExperimentCommands::Stop { id } => {
            let path = ta_goal::experiment::experiments_dir(project_root).join(format!("{id}.toml"));
            if path.is_file() {
                std::fs::remove_file(&path)?;
                println!("Experiment '{id}' stopped.");
            } else {
                println!("No active experiment named '{id}'.");
            }
        }
        ExperimentCommands::Report { id } => {
            let config = ExperimentConfig::load(project_root, &id)?;
            let maintenance_tag = config.as_ref().and_then(|c| c.maintenance_workflow.clone());
            let report = build_report(project_root, &id, maintenance_tag.as_deref())?;
            println!("Experiment: {}", report.experiment_id);
            for (arm, stats) in &report.unpaired {
                println!(
                    "  {arm}: n={}, mean_cost_usd={:.4}, stddev_cost_usd={:.4}",
                    stats.n, stats.mean_cost_usd, stats.stddev_cost_usd
                );
            }
            if report.maintenance_cost_usd > 0.0 {
                println!("  maintenance_cost_usd={:.4}", report.maintenance_cost_usd);
            }
            println!("  net_savings_usd={:.4}", report.net_savings_usd);
        }
    }
    Ok(())
}
```

Register in `apps/ta-cli/src/commands/mod.rs`: add `pub mod experiment;` alphabetically alongside the existing
`pub mod events;` line. Register in `apps/ta-cli/src/main.rs`: add an `Experiment { #[command(subcommand)]
command: commands::experiment::ExperimentCommands }` variant to the `Commands` enum (mirroring the existing
`Stats { ... }` variant found by the earlier grep) and its `Commands::Experiment { command } =>
commands::experiment::execute(command, &config.project_root)` (or whatever the equivalent project-root variable
is named at that dispatch site; check the `Commands::Stats` dispatch arm for the exact call shape) arm in the
match statement that dispatches `Commands::Stats`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev cargo test -p ta-cli --lib experiment:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the full ta-cli suite and clippy**

Run: `./dev cargo test -p ta-cli && ./dev cargo clippy -p ta-cli --all-targets -- -D warnings`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add apps/ta-cli/src/commands/experiment.rs apps/ta-cli/src/commands/mod.rs apps/ta-cli/src/main.rs
git commit -m "feat: add ta experiment start/stop/report CLI commands"
```

---

### Task 6: Gateway experiment-marker consultation and wiki-handler disable checks

**Files:**
- Modify: `crates/ta-mcp-gateway/src/server.rs`
- Test: `crates/ta-mcp-gateway/src/server.rs` (inline, following the existing `audit_tool_call_writes_to_log`-style
  test pattern found near line 2182)

**Interfaces:**
- Consumes: `GoalRun.experiment_overrides` (Task 1), `GatewayState.goal_store`/`active_agents` (already exist)
- Produces: `GatewayState::resolve_current_goal_run_id(&self) -> Option<Uuid>`,
  `GatewayState::experiment_override(&self, key: &str) -> Option<serde_json::Value>`

- [ ] **Step 1: Write the failing test for goal-id resolution via the active-agent map**

Add near the existing `audit_tool_call_writes_to_log` test in `crates/ta-mcp-gateway/src/server.rs`:

```rust
#[test]
fn resolve_current_goal_run_id_uses_active_agents_map() {
    let (mut state, _dir) = test_state(); // reuse this file's existing test-state helper; check its real name
                                           // near audit_tool_call_writes_to_log if this guess is wrong
    let goal_run_id = Uuid::new_v4();
    state.start_agent_session("agent-1", "claude", goal_run_id);
    std::env::set_var("TA_AGENT_ID", "agent-1");

    assert_eq!(state.resolve_current_goal_run_id(), Some(goal_run_id));

    std::env::remove_var("TA_AGENT_ID");
}

#[test]
fn resolve_current_goal_run_id_is_none_when_no_agent_matches() {
    let (state, _dir) = test_state();
    std::env::remove_var("TA_AGENT_ID");
    assert_eq!(state.resolve_current_goal_run_id(), None);
}
```

Check `start_agent_session`'s real signature (`grep -n "fn start_agent_session" crates/ta-mcp-gateway/src/server.rs`)
before finalizing this test; adjust the call to match its actual parameters.

- [ ] **Step 2: Run test to verify it fails**

Run: `./dev cargo test -p ta-mcp-gateway resolve_current_goal_run_id -- --nocapture`
Expected: FAIL (no method `resolve_current_goal_run_id`)

- [ ] **Step 3: Implement `resolve_current_goal_run_id` and `experiment_override`**

Add to `GatewayState`'s `impl` block, next to the existing `resolve_agent_id`:

```rust
    /// Resolve the current goal run via `TA_AGENT_ID` and `active_agents`,
    /// the same generic resolution `resolve_agent_id` already provides one
    /// step of. Returns `None` when there's no matching active agent
    /// session (a dev/manual call, or a caller mode with no goal context).
    pub fn resolve_current_goal_run_id(&self) -> Option<Uuid> {
        let agent_id = self.resolve_agent_id();
        self.active_agents.get(&agent_id).map(|s| s.goal_run_id)
    }

    /// Look up the current goal's resolved experiment-override value for
    /// `key`, if any. Generic: this method (and every caller of it) never
    /// hardcodes what a key means, only that some tool handler decided to
    /// check one.
    pub fn experiment_override(&self, key: &str) -> Option<serde_json::Value> {
        let goal_run_id = self.resolve_current_goal_run_id()?;
        let goal = self.goal_store.get(goal_run_id).ok()??;
        let overrides = goal.experiment_overrides?;
        overrides.get(key).cloned()
    }
```

Confirm `AgentSession` actually has a `goal_run_id: Uuid` field (used already above in `end_agent_session`'s
`session.goal_run_id`), so `.map(|s| s.goal_run_id)` is valid as written.

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev cargo test -p ta-mcp-gateway resolve_current_goal_run_id -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write the failing test for the wiki-handler disable check**

Add a test exercising `ta_wiki_search`'s actual handler with an experiment override set:

```rust
#[test]
fn ta_wiki_search_returns_disabled_stub_when_wiki_disabled_override_is_set() {
    let (mut state, _dir) = test_state();
    let goal_run_id = Uuid::new_v4();
    let mut goal = ta_goal::GoalRun::new(
        "t", "o", "agent-1", PathBuf::from("/tmp/ws"), PathBuf::from("/tmp/store"),
    );
    goal.goal_run_id = goal_run_id;
    goal.experiment_overrides = Some(serde_json::json!({"wiki.disabled": true}));
    state.goal_store.save(&goal).unwrap();
    state.start_agent_session("agent-1", "claude", goal_run_id);
    std::env::set_var("TA_AGENT_ID", "agent-1");

    let server = TaGatewayServer::with_state(state);
    let result = server
        .ta_wiki_search(Parameters(tools::wiki::WikiSearchParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            query: "anything".to_string(),
        }))
        .unwrap();

    let text = result_text(&result); // reuse this file's existing helper for extracting text from a
                                       // CallToolResult; check nearby tests for its real name
    assert!(text.contains("disabled"));

    std::env::remove_var("TA_AGENT_ID");
}
```

Check `WikiSearchParams`'s real field names (`grep -n "struct WikiSearchParams" -A6
crates/ta-mcp-gateway/src/tools/wiki.rs`) before finalizing this test, since the fields above are a best guess
based on the tool's description string, not a confirmed struct definition.

- [ ] **Step 6: Run test to verify it fails**

Run: `./dev cargo test -p ta-mcp-gateway ta_wiki_search_returns_disabled_stub -- --nocapture`
Expected: FAIL (real Wayfinder call attempted or missing credential error, not the disabled stub)

- [ ] **Step 7: Add the disable check to all five `ta_wiki_*` handlers**

In each of `ta_wiki_search`, `ta_wiki_get`, `ta_wiki_types`, `ta_wiki_create`, `ta_wiki_update`
(`crates/ta-mcp-gateway/src/server.rs`), immediately after the existing `self.audit(...)` line, add:

```rust
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
```

Confirm `CallToolResult::success` and `Content::text` are already imported/used elsewhere in this file (they
should be, since every other handler constructs a `CallToolResult`); use whichever exact construction pattern
this file's other handlers already use if it differs from this snippet.

- [ ] **Step 8: Run tests to verify they pass**

Run: `./dev cargo test -p ta-mcp-gateway ta_wiki_search_returns_disabled_stub resolve_current_goal_run_id -- --nocapture`
Expected: PASS

- [ ] **Step 9: Run the full ta-mcp-gateway suite**

Run: `./dev cargo test -p ta-mcp-gateway`
Expected: PASS. This confirms the four other wiki handlers' pre-existing tests (if any exist for them) still pass
with the new check added.

- [ ] **Step 10: Commit**

```bash
git add crates/ta-mcp-gateway/src/server.rs
git commit -m "feat: gateway wiki handlers respect per-goal cost-experiment overrides"
```

---

### Task 7: Generic `workflow` classification flag on `ta run` and wake listeners

**Files:**
- Modify: `apps/ta-cli/src/commands/run.rs` (new `--workflow <tag>` CLI flag, sets `GoalRun.workflow`)
- Modify: `crates/ta-daemon/src/wake_listener.rs` (`WakeListenerConfig` gains `workflow_tag: Option<String>`)
- Modify: `crates/ta-daemon/src/team_session.rs` (`build_ta_run_args` accepts and passes the tag through)
- Test: `crates/ta-daemon/src/team_session.rs` and `crates/ta-daemon/src/wake_listener.rs` (inline)

**Interfaces:**
- Consumes: `GoalRun.workflow` (Task 1)
- Produces: `ta run <title> --workflow <tag>` sets `goal.workflow = Some(tag)`;
  `WakeListenerConfig { role, keys, workflow_tag: Option<String> }`; `build_ta_run_args(..., workflow_tag:
  Option<&str>)` appends `--workflow <tag>` to the built args when set

This is the mechanism a downstream product (e.g. `ta-virtual-team`) uses to classify goals launched by a
particular wake-on-demand-registered role (e.g. "librarian" launches are "brain-maintenance") without TA core
ever hardcoding a role or product name: the tag is opaque, product-supplied config.

- [ ] **Step 1: Write the failing test for `build_ta_run_args` passing the tag through**

Add to `crates/ta-daemon/src/team_session.rs`'s existing test module, near any existing `build_ta_run_args` test
(search `grep -n "fn.*build_ta_run_args" crates/ta-daemon/src/team_session.rs` for the nearest existing test to
place this beside):

```rust
#[test]
fn build_ta_run_args_appends_workflow_flag_when_listener_has_a_tag() {
    let state = test_team_session_state(); // reuse this file's existing helper; check its real name near
                                            // any other build_ta_run_args test
    let team_config = TeamConfig::default();
    let args = build_ta_run_args(
        &state,
        "label",
        "librarian",
        &team_config,
        Path::new("/tmp/ctx.md"),
        Some("brain-maintenance"),
    );
    let flag_pos = args.iter().position(|a| a == "--workflow").expect("--workflow flag present");
    assert_eq!(args[flag_pos + 1], "brain-maintenance");
}

#[test]
fn build_ta_run_args_omits_workflow_flag_when_no_tag() {
    let state = test_team_session_state();
    let team_config = TeamConfig::default();
    let args = build_ta_run_args(&state, "label", "researcher", &team_config, Path::new("/tmp/ctx.md"), None);
    assert!(!args.contains(&"--workflow".to_string()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./dev cargo test -p ta-daemon build_ta_run_args_appends_workflow_flag build_ta_run_args_omits_workflow_flag -- --nocapture`
Expected: FAIL (compile error: `build_ta_run_args` takes 5 args, not 6)

- [ ] **Step 3: Add the parameter to `build_ta_run_args` and update its one call site**

Change the signature in `crates/ta-daemon/src/team_session.rs`:

```rust
pub fn build_ta_run_args(
    state: &TeamSessionState,
    label: &str,
    role: &str,
    team_config: &TeamConfig,
    context_path: &Path,
    workflow_tag: Option<&str>,
) -> Vec<String> {
```

and, right before the final `args` return (after the existing `member.persona`/`model_tier` block), add:

```rust
    if let Some(tag) = workflow_tag {
        args.push("--workflow".to_string());
        args.push(tag.to_string());
    }
```

Find this function's one existing call site (`grep -rn "build_ta_run_args(" crates/ta-daemon/src/`) and update it
to pass `None` for now; Step 5 below updates `wake_listener.rs`'s own call site to pass the real tag instead.

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev cargo test -p ta-daemon build_ta_run_args_appends_workflow_flag build_ta_run_args_omits_workflow_flag -- --nocapture`
Expected: PASS

- [ ] **Step 5: Add `workflow_tag` to `WakeListenerConfig` and thread it through `run_listener_loop`**

In `crates/ta-daemon/src/wake_listener.rs`, add to `WakeListenerConfig`:

```rust
pub struct WakeListenerConfig {
    pub role: String,
    pub keys: Vec<String>,
    /// Opaque classification tag applied to every goal this listener
    /// launches (via `ta run --workflow <tag>`), e.g. "brain-maintenance".
    /// `None` for a listener whose launches shouldn't be classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tag: Option<String>,
}
```

Update `WakeListenerConfig::new` to take a third parameter (`workflow_tag: Option<String>`) and set it, and
update the one call inside `run_listener_loop` (find it via `grep -n "build_ta_run_args(" crates/ta-daemon/src/wake_listener.rs`)
to pass `listener.workflow_tag.as_deref()` as the new final argument.

- [ ] **Step 5b: Expose `workflow_tag` through `ta team-session start`'s `--wake-on-demand` registration**

`WakeListenerConfig` (daemon-side) is populated from a second, independently-defined DTO struct,
`WakeOnDemandListenerConfig`, in `apps/ta-cli/src/commands/team_session.rs:42` (private, not imported from
`wake_listener.rs`; the two are kept in sync by matching JSON shape when the CLI writes `state.json`, confirmed
by reading both definitions). Add the field there too:

```rust
struct WakeOnDemandListenerConfig {
    role: String,
    keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_tag: Option<String>,
}
```

Add a new repeatable CLI flag alongside the existing `--wake-on-demand` (in the same `#[derive(clap::Args)]` or
subcommand struct that declares it, found at `apps/ta-cli/src/commands/team_session.rs:160`):

```rust
    /// Classify every goal a --wake-on-demand role launches with this tag
    /// (e.g. "brain-maintenance"). Form: <role>=<tag>. Repeatable.
    #[arg(long = "wake-on-demand-workflow")]
    wake_on_demand_workflow: Vec<String>,
```

In `start()` (`apps/ta-cli/src/commands/team_session.rs:212`), after the existing `wake_on_demand_listeners`
`Vec<WakeOnDemandListenerConfig>` is built (around line 250), parse and apply the new flag:

```rust
    let mut wake_on_demand_listeners = wake_on_demand_listeners; // make mutable
    for entry in wake_on_demand_workflow {
        let (role, tag) = entry.split_once('=').with_context(|| {
            format!("--wake-on-demand-workflow '{entry}' is not in the form <role>=<tag> (missing '=')")
        })?;
        let listener = wake_on_demand_listeners
            .iter_mut()
            .find(|l| l.role == role)
            .with_context(|| {
                format!(
                    "--wake-on-demand-workflow references role '{role}', but no --wake-on-demand \
                     entry registers that role"
                )
            })?;
        listener.workflow_tag = Some(tag.to_string());
    }
```

Add a matching test near the existing `start_parses_wake_on_demand_flag_into_state_json` test
(`apps/ta-cli/src/commands/team_session.rs:618`) asserting a `--wake-on-demand-workflow librarian=brain-maintenance`
flag, paired with a `--wake-on-demand librarian:some-key` entry, produces a `state.json` listener with
`workflow_tag == Some("brain-maintenance")`, and that referencing an unregistered role is a clear error, not a
silent no-op.

- [ ] **Step 6: Write the failing test for `--workflow` actually setting `GoalRun.workflow`**

In `apps/ta-cli/src/commands/run.rs`'s test module, add (reusing whichever goal-creation test helper Task 4 used):

```rust
#[test]
fn workflow_flag_sets_goal_workflow_field() {
    let dir = tempfile::tempdir().unwrap();
    let goal = create_goal_for_test_with_workflow(dir.path(), "title", "objective", Some("brain-maintenance"));
    assert_eq!(goal.workflow.as_deref(), Some("brain-maintenance"));
}
```

- [ ] **Step 7: Run test to verify it fails**

Run: `./dev cargo test -p ta-cli workflow_flag_sets_goal_workflow_field -- --nocapture`
Expected: FAIL (no `--workflow` flag recognized, or `goal.workflow` stays `None`)

- [ ] **Step 8: Add the `--workflow` flag to `ta run`'s argument parser**

Find `ta run`'s clap arg struct (`grep -n "struct RunArgs\|#\[arg(long = \"phase\"" apps/ta-cli/src/commands/run.rs`
or `apps/ta-cli/src/main.rs`, wherever `--phase` is actually declared) and add a sibling field:

```rust
    /// Generic cost-classification tag for this goal (e.g. "brain-maintenance").
    /// Opaque to TA core; downstream products define what tags mean.
    #[arg(long)]
    workflow: Option<String>,
```

Then, at the same point Task 4 already touches (where `plan_phase` gets copied onto the new `GoalRun`), add:

```rust
    goal.workflow = workflow_arg.clone(); // use whichever local variable name holds the parsed --workflow value
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `./dev cargo test -p ta-cli workflow_flag_sets_goal_workflow_field -- --nocapture`
Expected: PASS

- [ ] **Step 10: Run the full ta-daemon and ta-cli suites**

Run: `./dev cargo test -p ta-daemon -p ta-cli`
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs crates/ta-daemon/src/wake_listener.rs apps/ta-cli/src/commands/run.rs
git commit -m "feat: add generic --workflow classification flag and wake-listener workflow_tag"
```

---

### Task 8: Full workspace verification and PR

**Files:** none (verification only)

- [ ] **Step 1: Run all four gates**

```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

Expected: all four PASS.

- [ ] **Step 2: Self-audit for em dashes**

```bash
git diff main --stat
git diff main | grep "^+" | grep -c "—"
```

Expected: `0`. Fix any that appear before proceeding.

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin feature/generic-cost-experiment-framework
gh pr create --title "feat: generic cost-experiment framework" --body "$(cat <<'EOF'
## Summary
- New generic cost-experiment fields on GoalRun/VelocityEntry (experiment_id/arm/pair_id, repurposed workflow)
- ExperimentConfig storage format and unpaired-holdout / paired-shadow-sampling arm assignment
- ta experiment start/stop/report CLI commands
- Gateway wiki-handler disable check via per-goal experiment overrides

## Test plan
- Unit tests for arm assignment (distribution convergence, paired-mode shape)
- Unit tests for report aggregation (group means, maintenance netting)
- Gateway test confirming the disabled stub response when wiki.disabled=true
- Full workspace build/test/clippy/fmt all green
EOF
)"
```

- [ ] **Step 4: Wait for review/merge before starting the `ta-virtual-team` plan**

The `ta-virtual-team` plan (wiki-brain experiment definition, `docs/superpowers/plans/2026-09-20-wiki-brain-experiment-and-poller-sync.md`)
depends on this PR's `ta-credentials`/`ta-goal`/`ta-mcp-gateway` changes being available at a pinned tag, the same
way every other `ta-virtual-team` dependency bump works. Do not start that plan's tasks until this one is merged
and a new tag exists to pin to.
