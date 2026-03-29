// artifact_store.rs — Memory-backed workflow artifact store (v0.14.10).
//
// Each step writes its output artifacts to `ta memory` under the key:
//   <workflow-run-id>/<step-name>/<ArtifactType>
//
// Reading inputs: retrieve by producing step name and type.
//
// Resume: `ta workflow resume <run-id>` reads the store, checks which steps
// already have stored outputs, and skips them.
//
// The store is filesystem-based (FsMemoryStore) by default so it survives
// process restarts. This is the "memory IS the session artifact store" design
// from v0.14.10.

use std::path::Path;

use serde::{Deserialize, Serialize};
use ta_changeset::ArtifactType;

use crate::WorkflowError;

/// Key prefix used in the memory store for workflow artifacts.
///
/// Full key structure: `workflow/<run-id>/<stage-name>/<ArtifactType>`
const KEY_PREFIX: &str = "workflow";

/// A stored artifact from a workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredArtifact {
    /// Which workflow run this artifact belongs to.
    pub run_id: String,
    /// Which stage produced this artifact.
    pub stage: String,
    /// The artifact type.
    pub artifact_type: ArtifactType,
    /// The artifact payload (type-specific JSON).
    pub payload: serde_json::Value,
    /// ISO timestamp when the artifact was written.
    pub stored_at: String,
}

/// Compute the memory key for a specific artifact.
pub fn artifact_key(run_id: &str, stage: &str, artifact_type: &ArtifactType) -> String {
    format!("{}/{}/{}/{}", KEY_PREFIX, run_id, stage, artifact_type)
}

/// Compute the prefix used to enumerate all artifacts for a run.
pub fn run_prefix(run_id: &str) -> String {
    format!("{}/{}/", KEY_PREFIX, run_id)
}

/// Compute the prefix used to enumerate all artifacts for a stage in a run.
pub fn stage_prefix(run_id: &str, stage: &str) -> String {
    format!("{}/{}/{}/", KEY_PREFIX, run_id, stage)
}

/// Thin wrapper that handles artifact storage on top of any directory-based
/// memory store (the FsMemoryStore layout).
///
/// This struct is intentionally lightweight — it delegates all I/O to the
/// underlying store. In the CLI, callers create the store with
/// `ta_memory::memory_store_from_config()`.
pub struct ArtifactStore {
    /// Root of the memory store directory (`.ta/memory/` by default).
    store_dir: std::path::PathBuf,
}

impl ArtifactStore {
    /// Create an artifact store rooted at `store_dir` (e.g. `.ta/memory/`).
    pub fn new(store_dir: &Path) -> Self {
        Self {
            store_dir: store_dir.to_path_buf(),
        }
    }

    /// Store an artifact for a step output.
    ///
    /// Writes a JSON file at `<store_dir>/<key>.json`.
    pub fn store(
        &self,
        run_id: &str,
        stage: &str,
        artifact_type: &ArtifactType,
        payload: serde_json::Value,
    ) -> Result<(), WorkflowError> {
        let key = artifact_key(run_id, stage, artifact_type);
        let artifact = StoredArtifact {
            run_id: run_id.to_string(),
            stage: stage.to_string(),
            artifact_type: artifact_type.clone(),
            payload,
            stored_at: chrono::Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_string_pretty(&artifact)
            .map_err(|e| WorkflowError::Other(format!("artifact serialization error: {}", e)))?;

        // Sanitize key for filesystem: replace / with OS separator for subdirs.
        let rel_path = key.replace('/', std::path::MAIN_SEPARATOR_STR);
        let file_path = self.store_dir.join(format!("{}.json", rel_path));

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WorkflowError::IoError {
                path: parent.display().to_string(),
                source: e,
            })?;
        }

        std::fs::write(&file_path, json).map_err(|e| WorkflowError::IoError {
            path: file_path.display().to_string(),
            source: e,
        })?;

        tracing::debug!(
            run_id = %run_id,
            stage = %stage,
            artifact_type = %artifact_type,
            path = %file_path.display(),
            "artifact stored"
        );
        Ok(())
    }

    /// Retrieve an artifact by run, stage, and type.
    ///
    /// Returns `None` if the artifact doesn't exist (step not yet completed).
    pub fn retrieve(
        &self,
        run_id: &str,
        stage: &str,
        artifact_type: &ArtifactType,
    ) -> Result<Option<StoredArtifact>, WorkflowError> {
        let key = artifact_key(run_id, stage, artifact_type);
        let rel_path = key.replace('/', std::path::MAIN_SEPARATOR_STR);
        let file_path = self.store_dir.join(format!("{}.json", rel_path));

        if !file_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&file_path).map_err(|e| WorkflowError::IoError {
            path: file_path.display().to_string(),
            source: e,
        })?;

        let artifact: StoredArtifact = serde_json::from_str(&content).map_err(|e| {
            WorkflowError::Other(format!(
                "artifact deserialization error at {}: {}",
                file_path.display(),
                e
            ))
        })?;

        Ok(Some(artifact))
    }

    /// Check whether ALL declared outputs for a stage are present in the store.
    ///
    /// Used by resume logic: if all outputs exist, the stage is considered
    /// complete and can be skipped.
    pub fn stage_complete(
        &self,
        run_id: &str,
        stage: &str,
        declared_outputs: &[ArtifactType],
    ) -> Result<bool, WorkflowError> {
        if declared_outputs.is_empty() {
            // A stage with no declared outputs cannot be confirmed complete via
            // the artifact store — conservatively return false so it re-runs.
            return Ok(false);
        }
        for output_type in declared_outputs {
            if self.retrieve(run_id, stage, output_type)?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// List all artifact keys stored for a specific run.
    pub fn list_run_artifacts(&self, run_id: &str) -> Result<Vec<StoredArtifact>, WorkflowError> {
        let prefix_path = self.store_dir.join(KEY_PREFIX).join(run_id);

        if !prefix_path.exists() {
            return Ok(vec![]);
        }

        let mut artifacts = Vec::new();
        for entry in walkdir_json(&prefix_path)? {
            let content = std::fs::read_to_string(&entry).map_err(|e| WorkflowError::IoError {
                path: entry.display().to_string(),
                source: e,
            })?;
            match serde_json::from_str::<StoredArtifact>(&content) {
                Ok(a) => artifacts.push(a),
                Err(e) => {
                    tracing::warn!(path = %entry.display(), error = %e, "skipping malformed artifact file");
                }
            }
        }
        Ok(artifacts)
    }

    /// Given a list of stages (in execution order) and their declared outputs,
    /// return the names of stages that have already completed (all outputs present).
    /// These can be skipped on resume.
    pub fn completed_stages(
        &self,
        run_id: &str,
        stages: &[(&str, &[ArtifactType])],
    ) -> Result<Vec<String>, WorkflowError> {
        let mut completed = Vec::new();
        for (stage_name, declared_outputs) in stages {
            if self.stage_complete(run_id, stage_name, declared_outputs)? {
                completed.push(stage_name.to_string());
            }
        }
        Ok(completed)
    }
}

/// Recursively walk a directory and return paths to all `.json` files.
fn walkdir_json(dir: &Path) -> Result<Vec<std::path::PathBuf>, WorkflowError> {
    let mut results = Vec::new();
    if !dir.is_dir() {
        return Ok(results);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| WorkflowError::IoError {
        path: dir.display().to_string(),
        source: e,
    })? {
        let entry = entry.map_err(|e| WorkflowError::IoError {
            path: dir.display().to_string(),
            source: e,
        })?;
        let path = entry.path();
        if path.is_dir() {
            results.extend(walkdir_json(&path)?);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            results.push(path);
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::ArtifactType;
    use tempfile::tempdir;

    #[test]
    fn artifact_key_format() {
        let key = artifact_key("run-123", "generate-plan", &ArtifactType::PlanDocument);
        assert_eq!(key, "workflow/run-123/generate-plan/PlanDocument");
    }

    #[test]
    fn store_and_retrieve_roundtrip() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());

        let payload = serde_json::json!({"items": ["do X", "do Y"]});
        store
            .store(
                "run-abc",
                "generate-plan",
                &ArtifactType::PlanDocument,
                payload.clone(),
            )
            .unwrap();

        let retrieved = store
            .retrieve("run-abc", "generate-plan", &ArtifactType::PlanDocument)
            .unwrap()
            .expect("artifact should exist");

        assert_eq!(retrieved.run_id, "run-abc");
        assert_eq!(retrieved.stage, "generate-plan");
        assert_eq!(retrieved.artifact_type, ArtifactType::PlanDocument);
        assert_eq!(retrieved.payload, payload);
    }

    #[test]
    fn retrieve_missing_returns_none() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        let result = store
            .retrieve("run-xyz", "nonexistent", &ArtifactType::DraftPackage)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn stage_complete_all_outputs_present() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());

        store
            .store(
                "run-1",
                "plan",
                &ArtifactType::PlanDocument,
                serde_json::json!({}),
            )
            .unwrap();

        assert!(store
            .stage_complete("run-1", "plan", &[ArtifactType::PlanDocument])
            .unwrap());
    }

    #[test]
    fn stage_complete_partial_outputs_false() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());

        store
            .store(
                "run-1",
                "plan",
                &ArtifactType::PlanDocument,
                serde_json::json!({}),
            )
            .unwrap();

        // DraftPackage not yet stored.
        assert!(!store
            .stage_complete(
                "run-1",
                "plan",
                &[ArtifactType::PlanDocument, ArtifactType::DraftPackage]
            )
            .unwrap());
    }

    #[test]
    fn stage_complete_no_outputs_always_false() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        // A stage with no declared outputs can't be confirmed complete.
        assert!(!store.stage_complete("run-1", "plan", &[]).unwrap());
    }

    #[test]
    fn completed_stages_returns_done_only() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());

        // stage1 complete, stage2 not.
        store
            .store(
                "run-2",
                "stage1",
                &ArtifactType::PlanDocument,
                serde_json::json!({}),
            )
            .unwrap();

        let stages: Vec<(&str, &[ArtifactType])> = vec![
            ("stage1", &[ArtifactType::PlanDocument]),
            ("stage2", &[ArtifactType::DraftPackage]),
        ];
        let completed = store.completed_stages("run-2", &stages).unwrap();
        assert_eq!(completed, vec!["stage1"]);
    }

    #[test]
    fn list_run_artifacts_finds_all() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());

        store
            .store(
                "run-3",
                "s1",
                &ArtifactType::PlanDocument,
                serde_json::json!({}),
            )
            .unwrap();
        store
            .store(
                "run-3",
                "s2",
                &ArtifactType::DraftPackage,
                serde_json::json!({}),
            )
            .unwrap();

        let artifacts = store.list_run_artifacts("run-3").unwrap();
        assert_eq!(artifacts.len(), 2);
    }

    #[test]
    fn custom_artifact_type_roundtrip() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        let custom = ArtifactType::Custom("x-my-artifact".to_string());

        store
            .store(
                "run-4",
                "custom-stage",
                &custom,
                serde_json::json!({"data": 42}),
            )
            .unwrap();

        let retrieved = store
            .retrieve("run-4", "custom-stage", &custom)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.artifact_type, custom);
        assert_eq!(retrieved.payload["data"], 42);
    }
}
