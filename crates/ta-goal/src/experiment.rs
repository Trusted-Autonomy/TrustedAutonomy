//! Generic cost-experiment definitions (`.ta/experiments/<id>.toml`).
//!
//! An experiment assigns goal runs to named arms, each an opaque config-
//! override map. This module owns the storage format and the arm-assignment
//! logic (`assign_arm`); config-override *application* (actually interpreting
//! an arm's opaque JSON map) lives in the CLI/gateway layers that consume
//! this (see `apps/ta-cli/src/commands/experiment.rs`).

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
    /// The arm treated as the default/canonical baseline when assigning
    /// paired samples. When `None`, the first arm name in sorted order is
    /// used instead, since alphabetical order doesn't reliably match which
    /// arm is actually the intended default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_arm: Option<String>,
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
            path: path.display().to_string(),
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
            path: dir.display().to_string(),
            source,
        })?;
        let path = config_path(project_root, &self.id);
        let raw = toml::to_string_pretty(self)
            .map_err(|e| GoalError::ParseError(format!("{}: {e}", path.display())))?;
        std::fs::write(&path, raw).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })
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
            path: dir.display().to_string(),
            source,
        })? {
            let entry = entry.map_err(|source| GoalError::IoError {
                path: dir.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).map_err(|source| GoalError::IoError {
                path: path.display().to_string(),
                source,
            })?;
            let config: Self = toml::from_str(&raw)
                .map_err(|e| GoalError::ParseError(format!("{}: {e}", path.display())))?;
            out.push(config);
        }
        Ok(out)
    }
}

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
pub fn assign_arm(config: &ExperimentConfig, rng: &mut impl rand::Rng) -> ArmAssignment {
    let mut arm_names: Vec<&String> = config.arms.keys().collect();
    arm_names.sort(); // deterministic ordering for reproducible tests and stable pair selection
    if arm_names.len() < 2 {
        return ArmAssignment::None;
    }

    if rng.gen::<f64>() < config.paired_fraction {
        let canonical_arm = config
            .canonical_arm
            .clone()
            .unwrap_or_else(|| arm_names[0].clone());
        let shadow_arm = arm_names
            .iter()
            .find(|a| ***a != canonical_arm)
            .expect("at least two arms checked above")
            .to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn sample_config() -> ExperimentConfig {
        let mut arms = HashMap::new();
        arms.insert("variant-on".to_string(), serde_json::json!({}));
        arms.insert(
            "variant-off".to_string(),
            serde_json::json!({"feature.disabled": true}),
        );
        ExperimentConfig {
            id: "cost-test-1".to_string(),
            holdout_fraction: 0.2,
            paired_fraction: 0.1,
            maintenance_workflow: Some("experiment-maintenance".to_string()),
            arms,
            canonical_arm: None,
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let config = sample_config();
        config.save(dir.path()).unwrap();

        let loaded = ExperimentConfig::load(dir.path(), "cost-test-1").unwrap();
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
        other.id = "cost-test-2".to_string();
        other.save(dir.path()).unwrap();

        let mut ids: Vec<String> = ExperimentConfig::list(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["cost-test-1".to_string(), "cost-test-2".to_string()]
        );
    }

    #[test]
    fn load_malformed_toml_returns_err_not_panic() {
        let dir = tempdir().unwrap();
        let experiments_dir = experiments_dir(dir.path());
        std::fs::create_dir_all(&experiments_dir).unwrap();
        std::fs::write(
            experiments_dir.join("cost-test-bad.toml"),
            "this is not valid toml = = =",
        )
        .unwrap();

        let result = ExperimentConfig::load(dir.path(), "cost-test-bad");
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    #[test]
    fn list_with_malformed_toml_returns_err_not_panic() {
        let dir = tempdir().unwrap();
        sample_config().save(dir.path()).unwrap();
        let experiments_dir = experiments_dir(dir.path());
        std::fs::write(
            experiments_dir.join("cost-test-bad.toml"),
            "this is not valid toml = = =",
        )
        .unwrap();

        let result = ExperimentConfig::list(dir.path());
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    use rand::SeedableRng;

    fn two_arm_config(holdout: f64, paired: f64) -> ExperimentConfig {
        let mut arms = HashMap::new();
        arms.insert("variant-on".to_string(), serde_json::json!({}));
        arms.insert(
            "variant-off".to_string(),
            serde_json::json!({"feature.disabled": true}),
        );
        ExperimentConfig {
            id: "cost-test-arms".to_string(),
            holdout_fraction: holdout,
            paired_fraction: paired,
            maintenance_workflow: None,
            arms,
            canonical_arm: None,
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
                ArmAssignment::Paired {
                    canonical_arm,
                    shadow_arm,
                    pair_id,
                } => {
                    assert_ne!(canonical_arm, shadow_arm);
                    assert!(config.arms.contains_key(&canonical_arm));
                    assert!(config.arms.contains_key(&shadow_arm));
                    assert!(
                        seen_pair_ids.insert(pair_id),
                        "pair_id must be fresh each call"
                    );
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
                if arm == "variant-off" {
                    off_count += 1;
                }
            }
        }
        let observed_fraction = off_count as f64 / trials as f64;
        assert!(
            (observed_fraction - 0.15).abs() < 0.03,
            "expected roughly half of the 0.3 holdout fraction to land on variant-off, got {observed_fraction}"
        );
    }

    #[test]
    fn canonical_arm_preferred_when_set() {
        let mut config = two_arm_config(0.0, 1.0);
        config.canonical_arm = Some("variant-off".to_string());
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        for _ in 0..20 {
            match assign_arm(&config, &mut rng) {
                ArmAssignment::Paired {
                    canonical_arm,
                    shadow_arm,
                    ..
                } => {
                    assert_eq!(canonical_arm, "variant-off");
                    assert_eq!(shadow_arm, "variant-on");
                }
                other => panic!("expected Paired, got {other:?}"),
            }
        }
    }

    #[test]
    fn canonical_arm_defaults_to_first_sorted_name_when_unset() {
        let config = two_arm_config(0.0, 1.0);
        assert_eq!(config.canonical_arm, None);
        let mut rng = rand::rngs::StdRng::seed_from_u64(6);
        for _ in 0..20 {
            match assign_arm(&config, &mut rng) {
                ArmAssignment::Paired {
                    canonical_arm,
                    shadow_arm,
                    ..
                } => {
                    // "variant-off" < "variant-on" alphabetically.
                    assert_eq!(canonical_arm, "variant-off");
                    assert_eq!(shadow_arm, "variant-on");
                }
                other => panic!("expected Paired, got {other:?}"),
            }
        }
    }

    #[test]
    fn fewer_than_two_arms_never_assigns() {
        let mut config = two_arm_config(1.0, 1.0);
        config.arms.remove("variant-off");
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        for _ in 0..100 {
            assert_eq!(assign_arm(&config, &mut rng), ArmAssignment::None);
        }
    }
}
