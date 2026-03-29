// validate.rs — Workflow definition validation (v0.9.9.5 / v0.14.10).
//
// Provides structural, reference, and dependency validation for workflow
// YAML definitions and agent config YAML files.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::artifact_dag;
use crate::definition::WorkflowDefinition;

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct ValidationFinding {
    /// Severity: error, warning.
    pub severity: ValidationSeverity,
    /// Which field or element triggered this finding.
    pub location: String,
    /// Human-readable description.
    pub message: String,
    /// Suggested fix.
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    Error,
    Warning,
}

impl std::fmt::Display for ValidationSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationSeverity::Error => write!(f, "error"),
            ValidationSeverity::Warning => write!(f, "warning"),
        }
    }
}

/// Result of validating a workflow definition.
#[derive(Debug)]
pub struct ValidationResult {
    pub findings: Vec<ValidationFinding>,
}

impl ValidationResult {
    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity == ValidationSeverity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Warning)
            .count()
    }
}

/// Validate a workflow definition comprehensively.
///
/// Checks:
/// 1. Schema: required fields, non-empty name, at least one stage
/// 2. References: every role in a stage exists in `roles:`
/// 3. Dependencies: no cycles, no references to undefined stages
/// 4. Agent configs: every `roles.*.agent` has a matching config file (optional)
pub fn validate_workflow(
    def: &WorkflowDefinition,
    project_root: Option<&Path>,
) -> ValidationResult {
    let mut findings = Vec::new();

    // 1. Schema validation
    if def.name.trim().is_empty() {
        findings.push(ValidationFinding {
            severity: ValidationSeverity::Error,
            location: "name".to_string(),
            message: "Workflow name is empty.".to_string(),
            suggestion: Some("Add a descriptive name for this workflow.".to_string()),
        });
    }

    if def.stages.is_empty() {
        findings.push(ValidationFinding {
            severity: ValidationSeverity::Error,
            location: "stages".to_string(),
            message: "Workflow has no stages defined.".to_string(),
            suggestion: Some("Add at least one stage to the workflow.".to_string()),
        });
    }

    // Check for duplicate stage names.
    let mut seen_stages: HashSet<&str> = HashSet::new();
    for stage in &def.stages {
        if !seen_stages.insert(stage.name.as_str()) {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: format!("stages.{}", stage.name),
                message: format!("Duplicate stage name: '{}'.", stage.name),
                suggestion: Some("Each stage must have a unique name.".to_string()),
            });
        }
    }

    // Check for stages with empty names.
    for (i, stage) in def.stages.iter().enumerate() {
        if stage.name.trim().is_empty() {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: format!("stages[{}].name", i),
                message: "Stage has an empty name.".to_string(),
                suggestion: Some("Give this stage a descriptive name.".to_string()),
            });
        }

        // Stages should reference at least one role.
        if stage.roles.is_empty() && stage.then.is_empty() {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Warning,
                location: format!("stages.{}", stage.name),
                message: format!("Stage '{}' has no roles assigned.", stage.name),
                suggestion: Some(
                    "Add roles to execute in this stage, or use 'then' for sequential roles."
                        .to_string(),
                ),
            });
        }
    }

    // 2. Reference validation: every role used in a stage must be defined.
    let defined_roles: HashSet<&str> = def.roles.keys().map(|k| k.as_str()).collect();
    let stage_names: HashSet<&str> = def.stages.iter().map(|s| s.name.as_str()).collect();

    for stage in &def.stages {
        for role in stage.roles.iter().chain(stage.then.iter()) {
            if !defined_roles.contains(role.as_str()) {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    location: format!("stages.{}.roles", stage.name),
                    message: format!(
                        "Stage '{}' references undefined role '{}'.",
                        stage.name, role
                    ),
                    suggestion: Some(format!(
                        "Add '{}' to the 'roles:' section or remove it from this stage.",
                        role
                    )),
                });
            }
        }

        // Check review.reviewers references.
        if let Some(review) = &stage.review {
            for reviewer in &review.reviewers {
                if !defined_roles.contains(reviewer.as_str()) {
                    findings.push(ValidationFinding {
                        severity: ValidationSeverity::Error,
                        location: format!("stages.{}.review.reviewers", stage.name),
                        message: format!(
                            "Stage '{}' review references undefined role '{}'.",
                            stage.name, reviewer
                        ),
                        suggestion: Some(format!("Add '{}' to the 'roles:' section.", reviewer)),
                    });
                }
            }
        }

        // Check on_fail.route_to references a valid stage.
        if let Some(on_fail) = &stage.on_fail {
            if !stage_names.contains(on_fail.route_to.as_str()) {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    location: format!("stages.{}.on_fail.route_to", stage.name),
                    message: format!(
                        "Stage '{}' routes failures to undefined stage '{}'.",
                        stage.name, on_fail.route_to
                    ),
                    suggestion: Some(format!(
                        "Change route_to to one of: {}",
                        stage_names.iter().copied().collect::<Vec<_>>().join(", ")
                    )),
                });
            }
        }

        // Check depends_on references.
        for dep in &stage.depends_on {
            if !stage_names.contains(dep.as_str()) {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    location: format!("stages.{}.depends_on", stage.name),
                    message: format!(
                        "Stage '{}' depends on undefined stage '{}'.",
                        stage.name, dep
                    ),
                    suggestion: Some(format!(
                        "Available stages: {}",
                        stage_names.iter().copied().collect::<Vec<_>>().join(", ")
                    )),
                });
            }
        }
    }

    // Check for unused roles (warning only).
    let mut used_roles: HashSet<&str> = HashSet::new();
    for stage in &def.stages {
        for role in stage.roles.iter().chain(stage.then.iter()) {
            used_roles.insert(role.as_str());
        }
        if let Some(review) = &stage.review {
            for reviewer in &review.reviewers {
                used_roles.insert(reviewer.as_str());
            }
        }
    }
    if let Some(verdict) = &def.verdict {
        for rp in &verdict.required_pass {
            used_roles.insert(rp.as_str());
        }
    }
    for role_name in defined_roles.difference(&used_roles) {
        findings.push(ValidationFinding {
            severity: ValidationSeverity::Warning,
            location: format!("roles.{}", role_name),
            message: format!(
                "Role '{}' is defined but never used in any stage.",
                role_name
            ),
            suggestion: Some("Remove unused roles or add them to a stage.".to_string()),
        });
    }

    // 3. Dependency validation: check for cycles.
    match def.stage_order() {
        Ok(_) => {}
        Err(crate::WorkflowError::CycleDetected { stage, .. }) => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "stages".to_string(),
                message: format!("Dependency cycle detected involving stage '{}'.", stage),
                suggestion: Some(
                    "Remove circular depends_on references between stages.".to_string(),
                ),
            });
        }
        Err(e) => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "stages".to_string(),
                message: format!("Stage ordering error: {}", e),
                suggestion: None,
            });
        }
    }

    // 4. Agent config validation (when project_root is available).
    if let Some(root) = project_root {
        let agents_dir = root.join(".ta").join("agents");
        for (role_name, role_def) in &def.roles {
            let agent_config = agents_dir.join(format!("{}.yaml", role_def.agent));
            if agents_dir.exists() && !agent_config.exists() {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Warning,
                    location: format!("roles.{}.agent", role_name),
                    message: format!(
                        "Role '{}' uses agent '{}' but no config found at {}.",
                        role_name,
                        role_def.agent,
                        agent_config.display()
                    ),
                    suggestion: Some(format!(
                        "Create an agent config with: ta agent new {}",
                        role_def.agent
                    )),
                });
            }

            // Warn on empty prompts.
            if role_def.prompt.trim().is_empty() {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Warning,
                    location: format!("roles.{}.prompt", role_name),
                    message: format!("Role '{}' has an empty prompt.", role_name),
                    suggestion: Some(
                        "Add a system prompt describing this role's responsibilities.".to_string(),
                    ),
                });
            }
        }
    }

    // 5. Artifact I/O type validation (v0.14.10).
    // Run the DAG resolver — it warns on unresolved inputs and errors on cycles.
    // Cycle errors are already caught above; here we surface unresolved inputs
    // as warnings so the human knows which types need to be pre-loaded.
    if !def.stages.is_empty() {
        match artifact_dag::resolve_dag(&def.stages) {
            Ok(dag) => {
                for missing in &dag.unresolved_inputs {
                    findings.push(ValidationFinding {
                        severity: ValidationSeverity::Warning,
                        location: format!("stages.{}.inputs", missing.stage),
                        message: format!(
                            "Stage '{}' declares input '{}' but no other stage produces it. \
                             The artifact must be pre-loaded in the session store before the workflow runs.",
                            missing.stage, missing.artifact_type
                        ),
                        suggestion: Some(format!(
                            "Either add a stage that outputs '{}' or pre-populate with: \
                             ta memory store --key workflow/<run-id>/{}/{}",
                            missing.artifact_type,
                            missing.stage,
                            missing.artifact_type,
                        )),
                    });
                }
            }
            Err(crate::WorkflowError::CycleDetected { .. }) => {
                // Already reported above by stage_order() check.
            }
            Err(e) => {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    location: "stages".to_string(),
                    message: format!("Artifact DAG resolution error: {}", e),
                    suggestion: None,
                });
            }
        }
    }

    // Verdict config checks.
    if let Some(verdict) = &def.verdict {
        if verdict.pass_threshold < 0.0 || verdict.pass_threshold > 1.0 {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "verdict.pass_threshold".to_string(),
                message: format!(
                    "pass_threshold must be between 0.0 and 1.0 (got {}).",
                    verdict.pass_threshold
                ),
                suggestion: Some("Use a value like 0.7 (70% pass rate).".to_string()),
            });
        }

        for rp in &verdict.required_pass {
            if !defined_roles.contains(rp.as_str()) {
                findings.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    location: "verdict.required_pass".to_string(),
                    message: format!("required_pass references undefined role '{}'.", rp),
                    suggestion: Some(format!("Add '{}' to the 'roles:' section.", rp)),
                });
            }
        }
    }

    ValidationResult { findings }
}

/// Validate an agent config YAML file.
///
/// Checks:
/// - Required fields: name, command
/// - Warning if injects_settings without injects_context_file
pub fn validate_agent_config(content: &str) -> ValidationResult {
    let mut findings = Vec::new();

    let doc: HashMap<String, serde_yaml::Value> = match serde_yaml::from_str(content) {
        Ok(d) => d,
        Err(e) => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "file".to_string(),
                message: format!("Invalid YAML: {}", e),
                suggestion: Some("Fix the YAML syntax errors.".to_string()),
            });
            return ValidationResult { findings };
        }
    };

    // Required: name
    match doc.get("name") {
        Some(serde_yaml::Value::String(s)) if !s.trim().is_empty() => {}
        Some(_) => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "name".to_string(),
                message: "Agent name must be a non-empty string.".to_string(),
                suggestion: Some("Add: name: my-agent".to_string()),
            });
        }
        None => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "name".to_string(),
                message: "Missing required field 'name'.".to_string(),
                suggestion: Some("Add: name: my-agent".to_string()),
            });
        }
    }

    // Required: command
    match doc.get("command") {
        Some(serde_yaml::Value::String(s)) if !s.trim().is_empty() => {}
        Some(_) => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "command".to_string(),
                message: "Agent command must be a non-empty string.".to_string(),
                suggestion: Some("Add: command: claude".to_string()),
            });
        }
        None => {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "command".to_string(),
                message: "Missing required field 'command'.".to_string(),
                suggestion: Some("Add: command: claude".to_string()),
            });
        }
    }

    // Warning: injects_settings without injects_context_file
    let injects_settings = doc
        .get("injects_settings")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let injects_context = doc
        .get("injects_context_file")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if injects_settings && !injects_context {
        findings.push(ValidationFinding {
            severity: ValidationSeverity::Warning,
            location: "injects_settings".to_string(),
            message: "injects_settings is true but injects_context_file is false.".to_string(),
            suggestion: Some(
                "Settings injection usually requires context file injection. Set injects_context_file: true."
                    .to_string(),
            ),
        });
    }

    // Check args_template is a list.
    if let Some(args) = doc.get("args_template") {
        if !args.is_sequence() {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "args_template".to_string(),
                message: "args_template must be a list of strings.".to_string(),
                suggestion: Some("Use:\nargs_template:\n  - \"{prompt}\"".to_string()),
            });
        }
    }

    // Check alignment section if present.
    if let Some(alignment) = doc.get("alignment") {
        if !alignment.is_mapping() {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Warning,
                location: "alignment".to_string(),
                message: "alignment should be a mapping with fields like security_level, allowed_actions."
                    .to_string(),
                suggestion: None,
            });
        }
    }

    ValidationResult { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{RoleDefinition, StageDefinition, WorkflowDefinition};

    fn minimal_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            name: "test".to_string(),
            stages: vec![StageDefinition {
                name: "build".to_string(),
                depends_on: vec![],
                roles: vec!["engineer".to_string()],
                then: vec![],
                review: None,
                on_fail: None,
                await_human: Default::default(),
                inputs: vec![],
                outputs: vec![],
            }],
            roles: {
                let mut m = HashMap::new();
                m.insert(
                    "engineer".to_string(),
                    RoleDefinition {
                        agent: "claude-code".to_string(),
                        constitution: None,
                        prompt: "Build it".to_string(),
                        framework: None,
                    },
                );
                m
            },
            verdict: None,
            agent_framework: None,
        }
    }

    #[test]
    fn valid_workflow_no_errors() {
        let result = validate_workflow(&minimal_workflow(), None);
        assert!(!result.has_errors(), "findings: {:?}", result.findings);
    }

    #[test]
    fn empty_name_is_error() {
        let mut wf = minimal_workflow();
        wf.name = "".to_string();
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
        assert!(result.findings.iter().any(|f| f.location == "name"));
    }

    #[test]
    fn no_stages_is_error() {
        let mut wf = minimal_workflow();
        wf.stages.clear();
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
    }

    #[test]
    fn undefined_role_reference() {
        let mut wf = minimal_workflow();
        wf.stages[0].roles.push("nonexistent".to_string());
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("nonexistent")));
    }

    #[test]
    fn undefined_stage_dependency() {
        let mut wf = minimal_workflow();
        wf.stages[0].depends_on.push("phantom".to_string());
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("phantom")));
    }

    #[test]
    fn cycle_detected() {
        let wf = WorkflowDefinition {
            name: "cycle".to_string(),
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
            roles: HashMap::new(),
            verdict: None,
            agent_framework: None,
        };
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
        assert!(result.findings.iter().any(|f| f.message.contains("cycle")));
    }

    #[test]
    fn unused_role_warning() {
        let mut wf = minimal_workflow();
        wf.roles.insert(
            "unused".to_string(),
            RoleDefinition {
                agent: "claude-code".to_string(),
                constitution: None,
                prompt: "Never used".to_string(),
                framework: None,
            },
        );
        let result = validate_workflow(&wf, None);
        assert!(!result.has_errors());
        assert!(result.warning_count() > 0);
        assert!(result.findings.iter().any(|f| f.message.contains("unused")));
    }

    #[test]
    fn duplicate_stage_name() {
        let mut wf = minimal_workflow();
        wf.stages.push(StageDefinition {
            name: "build".to_string(),
            depends_on: vec![],
            roles: vec![],
            then: vec![],
            review: None,
            on_fail: None,
            await_human: Default::default(),
            inputs: vec![],
            outputs: vec![],
        });
        let result = validate_workflow(&wf, None);
        assert!(result.has_errors());
    }

    #[test]
    fn valid_agent_config() {
        let yaml = "name: test-agent\ncommand: claude\nargs_template:\n  - \"{prompt}\"\n";
        let result = validate_agent_config(yaml);
        assert!(!result.has_errors());
    }

    #[test]
    fn agent_missing_name() {
        let yaml = "command: claude\n";
        let result = validate_agent_config(yaml);
        assert!(result.has_errors());
        assert!(result.findings.iter().any(|f| f.location == "name"));
    }

    #[test]
    fn agent_missing_command() {
        let yaml = "name: test\n";
        let result = validate_agent_config(yaml);
        assert!(result.has_errors());
        assert!(result.findings.iter().any(|f| f.location == "command"));
    }

    #[test]
    fn agent_injects_settings_warning() {
        let yaml =
            "name: test\ncommand: claude\ninjects_settings: true\ninjects_context_file: false\n";
        let result = validate_agent_config(yaml);
        assert!(!result.has_errors());
        assert!(result.warning_count() > 0);
    }

    #[test]
    fn agent_invalid_yaml() {
        let yaml = "name: [invalid\n";
        let result = validate_agent_config(yaml);
        assert!(result.has_errors());
    }

    #[test]
    fn artifact_unresolved_input_is_warning() {
        // Stage declares PlanDocument as input but no stage produces it.
        let yaml = r#"
name: test
stages:
  - name: implement
    inputs: [PlanDocument]
    outputs: [DraftPackage]
    roles: []
roles: {}
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        let result = validate_workflow(&def, None);
        assert!(!result.has_errors());
        assert!(result.warning_count() > 0);
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("PlanDocument")));
    }

    #[test]
    fn artifact_chain_no_warnings() {
        // Complete chain: plan → PlanDocument → implement → DraftPackage.
        // No unresolved inputs.
        let yaml = r#"
name: test
stages:
  - name: plan
    outputs: [PlanDocument]
    roles: []
  - name: implement
    inputs: [PlanDocument]
    outputs: [DraftPackage]
    roles: []
roles: {}
"#;
        let def = WorkflowDefinition::from_yaml(yaml).unwrap();
        let result = validate_workflow(&def, None);
        // No errors; no unresolved-input warnings expected.
        assert!(!result.has_errors());
        // Filter to artifact-related warnings only.
        let artifact_warnings: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.location.contains("inputs"))
            .collect();
        assert!(artifact_warnings.is_empty());
    }
}
