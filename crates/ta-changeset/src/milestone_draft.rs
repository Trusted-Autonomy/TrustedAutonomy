// milestone_draft.rs — MilestoneDraft: aggregated multi-phase draft (v0.15.14).
//
// A MilestoneDraft collects draft IDs from multiple phase runs into a single
// review unit. Stored as `.ta/milestones/<milestone-id>.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A summary of one phase's draft contribution to a milestone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    /// The draft ID produced by the phase run.
    pub draft_id: String,
    /// Optional plan phase ID (e.g. "v0.15.14").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// Optional human-readable phase title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<String>,
    /// Number of artifacts in this draft.
    pub artifact_count: usize,
}

/// An aggregated milestone draft collecting multiple per-phase drafts.
///
/// Stored at `.ta/milestones/<milestone_id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestoneDraft {
    /// Unique milestone identifier (UUID).
    pub milestone_id: String,
    /// Human-readable title for the milestone.
    pub milestone_title: String,
    /// Ordered list of draft IDs included in this milestone.
    pub source_drafts: Vec<String>,
    /// Optional branch name for milestone-branch application mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_branch: Option<String>,
    /// Per-phase summaries, one per source draft.
    pub phase_summaries: Vec<PhaseSummary>,
    /// When this milestone was created.
    pub created_at: DateTime<Utc>,
}

impl MilestoneDraft {
    /// Persist the milestone to `.ta/milestones/<milestone_id>.json`.
    pub fn save(&self, workspace_root: &std::path::Path) -> anyhow::Result<()> {
        let milestones_dir = workspace_root.join(".ta").join("milestones");
        std::fs::create_dir_all(&milestones_dir)?;
        let path = milestones_dir.join(format!("{}.json", self.milestone_id));
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content).map_err(|e| {
            anyhow::anyhow!(
                "Failed to write milestone draft to {}: {}",
                path.display(),
                e
            )
        })
    }

    /// Load a milestone from `.ta/milestones/<milestone_id>.json`.
    pub fn load(workspace_root: &std::path::Path, milestone_id: &str) -> anyhow::Result<Self> {
        let path = workspace_root
            .join(".ta")
            .join("milestones")
            .join(format!("{}.json", milestone_id));
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "Milestone draft not found at {}: {}\n\
                 List milestones with: ls .ta/milestones/",
                path.display(),
                e
            )
        })?;
        serde_json::from_str(&content).map_err(|e| {
            anyhow::anyhow!("Failed to parse milestone draft {}: {}", path.display(), e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_milestone(id: &str, title: &str) -> MilestoneDraft {
        MilestoneDraft {
            milestone_id: id.to_string(),
            milestone_title: title.to_string(),
            source_drafts: vec!["draft-aaa".to_string(), "draft-bbb".to_string()],
            milestone_branch: Some("feature/milestone-1".to_string()),
            phase_summaries: vec![
                PhaseSummary {
                    draft_id: "draft-aaa".to_string(),
                    phase_id: Some("v0.15.14".to_string()),
                    phase_title: Some("Phase title A".to_string()),
                    artifact_count: 3,
                },
                PhaseSummary {
                    draft_id: "draft-bbb".to_string(),
                    phase_id: Some("v0.15.15".to_string()),
                    phase_title: Some("Phase title B".to_string()),
                    artifact_count: 2,
                },
            ],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn milestone_draft_save_load() {
        let dir = tempdir().unwrap();
        let milestone = make_milestone("test-milestone-id", "Test Milestone");
        milestone.save(dir.path()).unwrap();

        let loaded = MilestoneDraft::load(dir.path(), "test-milestone-id").unwrap();
        assert_eq!(loaded.milestone_id, "test-milestone-id");
        assert_eq!(loaded.milestone_title, "Test Milestone");
        assert_eq!(loaded.source_drafts.len(), 2);
        assert_eq!(loaded.phase_summaries.len(), 2);
        assert_eq!(loaded.phase_summaries[0].artifact_count, 3);
        assert_eq!(
            loaded.milestone_branch.as_deref(),
            Some("feature/milestone-1")
        );
    }

    #[test]
    fn milestone_draft_roundtrip_json() {
        let milestone = make_milestone("roundtrip-id", "Roundtrip Milestone");
        let json = serde_json::to_string_pretty(&milestone).unwrap();
        let restored: MilestoneDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.milestone_id, "roundtrip-id");
        assert_eq!(restored.source_drafts, milestone.source_drafts);
        assert_eq!(restored.phase_summaries[1].draft_id, "draft-bbb");
    }

    #[test]
    fn milestone_draft_load_missing_returns_error() {
        let dir = tempdir().unwrap();
        let err = MilestoneDraft::load(dir.path(), "nonexistent-id").unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("No such file"),
            "Expected not-found error, got: {}",
            err
        );
    }
}
