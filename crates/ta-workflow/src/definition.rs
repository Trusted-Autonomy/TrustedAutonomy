// definition.rs — Declarative workflow structure used by all engines.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ta_changeset::ArtifactType;

use crate::interaction::AwaitHumanConfig;

/// Catalog of built-in workflow names and descriptions shipped with TA.
///
/// Used by `ta workflow list --builtin` and `ta run --workflow` to enumerate
/// known workflows and validate user-provided names.
pub struct WorkflowCatalog;

impl WorkflowCatalog {
    /// Returns all built-in workflow names and descriptions.
    pub fn list() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "single-agent",
                "Default: one agent in one staging directory (backwards-compatible)",
            ),
            (
                "serial-phases",
                "Chain phases serially: each phase as follow-up in same staging, one PR at end",
            ),
            (
                "swarm",
                "Parallel sub-goals with integration agent (v0.13.7.2+)",
            ),
            (
                "approval-chain",
                "Sequential human approval steps (v0.13.7.3+)",
            ),
        ]
    }

    /// Returns true if the given name is a known built-in workflow.
    pub fn is_known(name: &str) -> bool {
        Self::list().iter().any(|(n, _)| *n == name)
    }
}

/// A complete workflow definition that engines parse and execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    /// Human-readable workflow name.
    pub name: String,
    /// Ordered list of stages in the workflow.
    pub stages: Vec<StageDefinition>,
    /// Role definitions keyed by role name.
    #[serde(default)]
    pub roles: HashMap<String, RoleDefinition>,
    /// Verdict scoring configuration.
    #[serde(default)]
    pub verdict: Option<VerdictConfig>,
    /// Default agent framework for all roles in this workflow (v0.13.8).
    ///
    /// Overrides the project `[agent].default_framework` in daemon.toml.
    /// Individual roles can override this via their `framework` field.
    ///
    /// Example in workflow YAML:
    ///   agent_framework: codex
    #[serde(default)]
    pub agent_framework: Option<String>,
}

/// A single stage in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageDefinition {
    /// Stage name (unique within the workflow).
    pub name: String,
    /// Stages that must complete before this one starts.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Roles that execute in parallel within this stage.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Roles that execute sequentially after the parallel roles complete.
    #[serde(default)]
    pub then: Vec<String>,
    /// Optional review configuration for this stage.
    #[serde(default)]
    pub review: Option<StageReview>,
    /// Routing on failure.
    #[serde(default)]
    pub on_fail: Option<FailureRouting>,
    /// When to pause for human input.
    #[serde(default)]
    pub await_human: AwaitHumanConfig,
    /// Artifact types this stage consumes. The WorkflowEngine uses these to
    /// resolve implicit DAG edges — any stage whose `outputs` intersect with
    /// this stage's `inputs` becomes an implicit dependency (v0.14.10).
    #[serde(default)]
    pub inputs: Vec<ArtifactType>,
    /// Artifact types this stage produces. Written to the session artifact
    /// store (ta memory) under `<run-id>/<stage-name>/<ArtifactType>` (v0.14.10).
    #[serde(default)]
    pub outputs: Vec<ArtifactType>,
}

/// Review configuration for a stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageReview {
    /// Roles that perform the review.
    #[serde(default)]
    pub reviewers: Vec<String>,
    /// Whether all reviewers must pass (true) or any one (false).
    #[serde(default = "default_true")]
    pub require_all: bool,
}

fn default_true() -> bool {
    true
}

/// Where to route when a stage fails its review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureRouting {
    /// Stage to route back to.
    pub route_to: String,
    /// Maximum retry count before failing the workflow.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_max_retries() -> u32 {
    3
}

/// A role definition — describes an agent's configuration for a stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDefinition {
    /// Agent config name (e.g., "claude-code", "codex").
    pub agent: String,
    /// Optional constitution YAML path.
    #[serde(default)]
    pub constitution: Option<String>,
    /// System prompt for this role.
    #[serde(default)]
    pub prompt: String,
    /// Override framework for this role.
    #[serde(default)]
    pub framework: Option<String>,
}

/// Verdict scoring configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictConfig {
    /// Scorer configuration.
    #[serde(default)]
    pub scorer: Option<ScorerConfig>,
    /// Minimum aggregate score to pass (0.0-1.0).
    #[serde(default = "default_pass_threshold")]
    pub pass_threshold: f64,
    /// Roles whose pass verdict is required regardless of aggregate score.
    #[serde(default)]
    pub required_pass: Vec<String>,
}

fn default_pass_threshold() -> f64 {
    0.7
}

/// Configuration for the feedback scoring agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScorerConfig {
    /// Agent to use for scoring.
    pub agent: String,
    /// System prompt for the scorer.
    #[serde(default)]
    pub prompt: String,
}

impl WorkflowDefinition {
    /// Parse a workflow definition from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, crate::WorkflowError> {
        serde_yaml::from_str(yaml).map_err(|e| crate::WorkflowError::ParseError {
            reason: e.to_string(),
        })
    }

    /// Parse a workflow definition from a YAML file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, crate::WorkflowError> {
        let content = std::fs::read_to_string(path).map_err(|e| crate::WorkflowError::IoError {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_yaml(&content)
    }

    /// Get the topologically sorted stage order.
    /// Returns an error if there are cycles.
    pub fn stage_order(&self) -> Result<Vec<String>, crate::WorkflowError> {
        let stage_names: Vec<&str> = self.stages.iter().map(|s| s.name.as_str()).collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

        for name in &stage_names {
            in_degree.entry(name).or_insert(0);
            adjacency.entry(name).or_default();
        }

        for stage in &self.stages {
            for dep in &stage.depends_on {
                adjacency
                    .entry(dep.as_str())
                    .or_default()
                    .push(stage.name.as_str());
                *in_degree.entry(stage.name.as_str()).or_insert(0) += 1;
            }
        }

        let mut queue: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&name, _)| name)
            .collect();
        queue.sort(); // deterministic order

        let mut result = Vec::new();
        while let Some(node) = queue.pop() {
            result.push(node.to_string());
            if let Some(neighbors) = adjacency.get(node) {
                for &neighbor in neighbors {
                    let deg = in_degree.get_mut(neighbor).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(neighbor);
                        queue.sort();
                    }
                }
            }
        }

        if result.len() != stage_names.len() {
            let remaining: Vec<String> = stage_names
                .iter()
                .filter(|n| !result.contains(&n.to_string()))
                .map(|n| n.to_string())
                .collect();
            return Err(crate::WorkflowError::CycleDetected {
                id: self.name.clone(),
                stage: remaining.first().cloned().unwrap_or_default(),
            });
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_workflow_yaml() {
        let yaml = r#"
name: simple-review
stages:
  - name: build
    roles: [engineer]
  - name: review
    depends_on: [build]
    roles: [reviewer]
roles:
  engineer:
    agent: claude-code
    prompt: "Build the feature"
  reviewer:
    agent: claude-code
    prompt: "Review the code"
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        assert_eq!(def.name, "simple-review");
        assert_eq!(def.stages.len(), 2);
        assert_eq!(def.roles.len(), 2);
    }

    #[test]
    fn topological_sort_simple() {
        let def = WorkflowDefinition {
            name: "test".to_string(),
            stages: vec![
                StageDefinition {
                    name: "review".to_string(),
                    depends_on: vec!["build".to_string()],
                    roles: vec![],
                    then: vec![],
                    review: None,
                    on_fail: None,
                    await_human: Default::default(),
                    inputs: vec![],
                    outputs: vec![],
                },
                StageDefinition {
                    name: "build".to_string(),
                    depends_on: vec![],
                    roles: vec![],
                    then: vec![],
                    review: None,
                    on_fail: None,
                    await_human: Default::default(),
                    inputs: vec![],
                    outputs: vec![],
                },
            ],
            roles: Default::default(),
            verdict: None,
            agent_framework: None,
        };
        let order = def.stage_order().unwrap();
        assert_eq!(order, vec!["build", "review"]);
    }

    #[test]
    fn topological_sort_cycle_detected() {
        let def = WorkflowDefinition {
            name: "test".to_string(),
            stages: vec![
                StageDefinition {
                    name: "a".to_string(),
                    depends_on: vec!["b".to_string()],
                    roles: vec![],
                    then: vec![],
                    review: None,
                    on_fail: None,
                    await_human: Default::default(),
                    inputs: vec![],
                    outputs: vec![],
                },
                StageDefinition {
                    name: "b".to_string(),
                    depends_on: vec!["a".to_string()],
                    roles: vec![],
                    then: vec![],
                    review: None,
                    on_fail: None,
                    await_human: Default::default(),
                    inputs: vec![],
                    outputs: vec![],
                },
            ],
            roles: Default::default(),
            verdict: None,
            agent_framework: None,
        };
        let result = def.stage_order();
        assert!(matches!(
            result,
            Err(crate::WorkflowError::CycleDetected { .. })
        ));
    }

    #[test]
    fn stage_review_defaults() {
        let yaml = r#"
name: test
stages:
  - name: build
    review:
      reviewers: [security]
roles: {}
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        let review = def.stages[0].review.as_ref().unwrap();
        assert!(review.require_all); // default true
    }

    #[test]
    fn failure_routing_default_max_retries() {
        let yaml = r#"
name: test
stages:
  - name: build
    on_fail:
      route_to: planning
roles: {}
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        let routing = def.stages[0].on_fail.as_ref().unwrap();
        assert_eq!(routing.max_retries, 3); // default
    }

    #[test]
    fn await_human_defaults_to_never() {
        let yaml = r#"
name: test
stages:
  - name: build
roles: {}
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        assert_eq!(def.stages[0].await_human, AwaitHumanConfig::Never);
    }

    #[test]
    fn verdict_config_parsing() {
        let yaml = r#"
name: test
stages:
  - name: build
roles: {}
verdict:
  pass_threshold: 0.8
  required_pass: [security-reviewer]
  scorer:
    agent: claude-code
    prompt: "You are a metacritic reviewer."
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        let verdict = def.verdict.as_ref().unwrap();
        assert_eq!(verdict.pass_threshold, 0.8);
        assert_eq!(verdict.required_pass, vec!["security-reviewer"]);
        assert!(verdict.scorer.is_some());
    }

    #[test]
    fn workflow_catalog_lists_known_workflows() {
        let catalog = WorkflowCatalog::list();
        let names: Vec<&str> = catalog.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"single-agent"));
        assert!(names.contains(&"serial-phases"));
        assert!(names.contains(&"swarm"));
        assert!(names.contains(&"approval-chain"));
        assert_eq!(catalog.len(), 4);
    }

    #[test]
    fn workflow_catalog_is_known() {
        assert!(WorkflowCatalog::is_known("single-agent"));
        assert!(WorkflowCatalog::is_known("serial-phases"));
        assert!(WorkflowCatalog::is_known("swarm"));
        assert!(WorkflowCatalog::is_known("approval-chain"));
        assert!(!WorkflowCatalog::is_known("unknown-workflow"));
        assert!(!WorkflowCatalog::is_known(""));
    }

    #[test]
    fn workflow_catalog_descriptions_nonempty() {
        for (name, desc) in WorkflowCatalog::list() {
            assert!(!name.is_empty(), "workflow name should not be empty");
            assert!(
                !desc.is_empty(),
                "description for '{}' should not be empty",
                name
            );
        }
    }
}
