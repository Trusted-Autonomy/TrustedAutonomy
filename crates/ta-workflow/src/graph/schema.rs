// graph/schema.rs — TOML graph-definition schema + loader (v0.17.7.1).
//
// `.ta/workflows/graphs/<name>.toml` -> `GraphDefinition`. See
// docs/superpowers/specs/2026-07-21-workflow-graph-engine-design.md §3 for
// the canonical example this schema mirrors.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::types::GraphError;

/// A generic `id` + `kind` + free-form params node entry, used for
/// `[[trigger]]`, `[[worker]]`, and `[[reviewer]]` sections — each node
/// `kind` may need different extra fields (e.g. `agent_panel`'s `role`,
/// `goal_dispatch`'s `verb`/`workload_hint`), so those live in `params`
/// rather than being hardcoded per section, mirroring `StepAction`'s
/// `action_type` + `params` shape (`crate::step_action::StepAction`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeDef {
    pub id: String,
    pub kind: String,
    #[serde(flatten)]
    pub params: toml::value::Table,
}

impl NodeDef {
    /// Read a string param, if present.
    pub fn param_str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }
}

/// `[decision]` — fan-in from N reviewer ids to one `Decision`. Config
/// (algorithm/threshold/weights) is data, not hardcoded literals — the gap
/// `WeightedDecisionNode` fixes in `governed_workflow.rs`'s `stage_consensus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionDef {
    pub id: String,
    #[serde(default = "default_decision_kind")]
    pub kind: String,
    /// "raft" | "paxos" | "weighted". Defaults to "weighted" (§16.5: the
    /// algorithm set may stay a closed enum, only the wiring must be data).
    #[serde(default)]
    pub algorithm: Option<String>,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Reviewer ids that fan into this decision.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub weights: HashMap<String, f64>,
    #[serde(default)]
    pub require_all: bool,
}

fn default_decision_kind() -> String {
    "weighted".to_string()
}

fn default_threshold() -> f64 {
    0.75
}

/// `[action]` — the terminal node. `decision` names the `[decision].id`
/// whose output feeds this action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionDef {
    pub id: String,
    /// "auto_approve" | "recommend" | ... — swapping only this string
    /// changes whether the same `Decision` becomes binding or advisory
    /// (constitution §16.2).
    pub kind: String,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(flatten)]
    pub params: toml::value::Table,
}

/// In-memory form of a `.ta/workflows/graphs/<name>.toml` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphDefinition {
    #[serde(default, rename = "trigger")]
    pub triggers: Vec<NodeDef>,
    #[serde(default, rename = "worker")]
    pub workers: Vec<NodeDef>,
    #[serde(default, rename = "reviewer")]
    pub reviewers: Vec<NodeDef>,
    pub decision: Option<DecisionDef>,
    pub action: Option<ActionDef>,
}

impl GraphDefinition {
    /// Parse a graph definition from a TOML string.
    pub fn from_toml_str(content: &str) -> Result<Self, GraphError> {
        let def: GraphDefinition =
            toml::from_str(content).map_err(|e| GraphError::Parse(e.to_string()))?;
        def.validate()?;
        Ok(def)
    }

    /// Load `.ta/workflows/graphs/<name>.toml` (or any path) from disk.
    pub fn load_file(path: &Path) -> Result<Self, GraphError> {
        let content = std::fs::read_to_string(path).map_err(|e| GraphError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_toml_str(&content)
    }

    /// Resolve `<name>` to `<workspace_root>/.ta/workflows/graphs/<name>.toml`
    /// and load it. Accepts either a bare name ("phase-review-panel") or a
    /// path ending in `.toml`.
    pub fn load_named(workspace_root: &Path, name: &str) -> Result<Self, GraphError> {
        let path = if name.ends_with(".toml") {
            Path::new(name).to_path_buf()
        } else {
            workspace_root
                .join(".ta")
                .join("workflows")
                .join("graphs")
                .join(format!("{name}.toml"))
        };
        Self::load_file(&path)
    }

    /// Structural checks beyond what serde enforces: every id referenced by
    /// `[decision].inputs` and `[action].decision` must exist, and ids must
    /// be unique within the graph.
    fn validate(&self) -> Result<(), GraphError> {
        let mut seen = std::collections::HashSet::new();
        for id in self
            .triggers
            .iter()
            .map(|n| &n.id)
            .chain(self.workers.iter().map(|n| &n.id))
            .chain(self.reviewers.iter().map(|n| &n.id))
            .chain(self.decision.iter().map(|d| &d.id))
            .chain(self.action.iter().map(|a| &a.id))
        {
            if !seen.insert(id.as_str()) {
                return Err(GraphError::InvalidDefinition(format!(
                    "duplicate node id '{id}'"
                )));
            }
        }

        if let Some(decision) = &self.decision {
            let reviewer_ids: std::collections::HashSet<&str> =
                self.reviewers.iter().map(|r| r.id.as_str()).collect();
            for input in &decision.inputs {
                if !reviewer_ids.contains(input.as_str()) {
                    return Err(GraphError::InvalidDefinition(format!(
                        "decision '{}' references unknown reviewer input '{input}'",
                        decision.id
                    )));
                }
            }
        }

        if let Some(action) = &self.action {
            if let Some(decision_ref) = &action.decision {
                let matches = self
                    .decision
                    .as_ref()
                    .map(|d| &d.id == decision_ref)
                    .unwrap_or(false);
                if !matches {
                    return Err(GraphError::InvalidDefinition(format!(
                        "action '{}' references unknown decision '{decision_ref}'",
                        action.id
                    )));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE_GRAPH: &str = r#"
[[trigger]]
id = "draft_ready"
kind = "vcs_task_completion"

[[reviewer]]
id = "policy_check"
kind = "policy"

[[reviewer]]
id = "pm_score"
kind = "agent_panel"
role = "pm"

[decision]
id = "panel_verdict"
kind = "weighted"
algorithm = "weighted"
threshold = 0.75
inputs = ["policy_check", "pm_score"]
weights = { policy_check = 1.0, pm_score = 1.0 }

[action]
id = "outcome"
kind = "recommend"
decision = "panel_verdict"
"#;

    #[test]
    fn parses_reference_graph() {
        let def = GraphDefinition::from_toml_str(REFERENCE_GRAPH).unwrap();
        assert_eq!(def.triggers.len(), 1);
        assert_eq!(def.triggers[0].kind, "vcs_task_completion");
        assert_eq!(def.reviewers.len(), 2);
        assert_eq!(def.reviewers[1].param_str("role"), Some("pm"));
        let decision = def.decision.unwrap();
        assert_eq!(decision.threshold, 0.75);
        assert_eq!(decision.inputs, vec!["policy_check", "pm_score"]);
        assert_eq!(decision.weights.get("pm_score"), Some(&1.0));
        let action = def.action.unwrap();
        assert_eq!(action.kind, "recommend");
        assert_eq!(action.decision.as_deref(), Some("panel_verdict"));
    }

    #[test]
    fn swapping_action_kind_is_the_only_diff_between_recommend_and_auto_approve() {
        let recommend = GraphDefinition::from_toml_str(REFERENCE_GRAPH).unwrap();
        let auto_approve_toml = REFERENCE_GRAPH.replace(
            "kind = \"recommend\"\ndecision = \"panel_verdict\"",
            "kind = \"auto_approve\"\ndecision = \"panel_verdict\"",
        );
        let auto_approve = GraphDefinition::from_toml_str(&auto_approve_toml).unwrap();
        assert_eq!(
            recommend.decision.unwrap().id,
            auto_approve.decision.unwrap().id
        );
        assert_ne!(
            recommend.action.unwrap().kind,
            auto_approve.action.unwrap().kind
        );
    }

    #[test]
    fn rejects_decision_input_referencing_unknown_reviewer() {
        let bad = r#"
[[reviewer]]
id = "policy_check"
kind = "policy"

[decision]
id = "panel_verdict"
inputs = ["nonexistent"]

[action]
id = "outcome"
kind = "recommend"
decision = "panel_verdict"
"#;
        let err = GraphDefinition::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, GraphError::InvalidDefinition(_)));
    }

    #[test]
    fn rejects_action_referencing_unknown_decision() {
        let bad = r#"
[action]
id = "outcome"
kind = "recommend"
decision = "does_not_exist"
"#;
        let err = GraphDefinition::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, GraphError::InvalidDefinition(_)));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let bad = r#"
[[reviewer]]
id = "dup"
kind = "policy"

[decision]
id = "dup"
"#;
        let err = GraphDefinition::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, GraphError::InvalidDefinition(_)));
    }

    #[test]
    fn defaults_apply_when_fields_omitted() {
        let minimal = r#"
[decision]
id = "d1"
"#;
        let def = GraphDefinition::from_toml_str(minimal).unwrap();
        let decision = def.decision.unwrap();
        assert_eq!(decision.kind, "weighted");
        assert_eq!(decision.threshold, 0.75);
        assert!(decision.algorithm.is_none());
    }

    #[test]
    fn load_file_reads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.toml");
        std::fs::write(&path, REFERENCE_GRAPH).unwrap();
        let def = GraphDefinition::load_file(&path).unwrap();
        assert_eq!(def.reviewers.len(), 2);
    }

    #[test]
    fn load_named_resolves_under_ta_workflows_graphs() {
        let dir = tempfile::tempdir().unwrap();
        let graphs_dir = dir.path().join(".ta").join("workflows").join("graphs");
        std::fs::create_dir_all(&graphs_dir).unwrap();
        std::fs::write(graphs_dir.join("phase-review-panel.toml"), REFERENCE_GRAPH).unwrap();
        let def = GraphDefinition::load_named(dir.path(), "phase-review-panel").unwrap();
        assert_eq!(def.reviewers.len(), 2);
    }
}
