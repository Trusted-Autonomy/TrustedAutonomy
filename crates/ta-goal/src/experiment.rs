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
}
