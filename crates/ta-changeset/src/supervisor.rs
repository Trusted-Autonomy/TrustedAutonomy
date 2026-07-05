// supervisor.rs — Supervisor agent for dependency graph analysis and validation.
//
// The supervisor validates artifact dispositions against their dependency graph,
// warning about coupled rejections and broken dependencies before apply.

use std::collections::{HashMap, HashSet};

use crate::draft_package::{Artifact, ArtifactDisposition, DependencyKind};

#[cfg(test)]
use crate::draft_package::ChangeDependency;

/// Result of supervisor validation with warnings and errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    /// Whether the configuration is valid (no hard errors).
    pub valid: bool,
    /// Non-blocking warnings (e.g., rejecting an artifact others depend on).
    pub warnings: Vec<ValidationWarning>,
    /// Blocking errors (e.g., cycles in dependency graph).
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    /// Create a valid result with no issues.
    pub fn valid() -> Self {
        Self {
            valid: true,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Check if there are any warnings.
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Check if there are any errors.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Add a warning to the result.
    pub fn add_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }

    /// Add an error to the result (sets valid = false).
    pub fn add_error(&mut self, error: ValidationError) {
        self.valid = false;
        self.errors.push(error);
    }
}

/// Warning about potentially problematic dispositions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationWarning {
    /// Rejecting an artifact that others depend on.
    CoupledRejection {
        artifact: String,
        required_by: Vec<String>,
    },
    /// Approving an artifact that depends on rejected ones.
    BrokenDependency {
        artifact: String,
        depends_on_rejected: Vec<String>,
    },
    /// An artifact marked "discuss" is blocking others.
    DiscussBlockingApproval {
        artifact: String,
        blocking: Vec<String>,
    },
}

/// Hard errors in the dependency graph or configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Circular dependency detected.
    CyclicDependency { cycle: Vec<String> },
    /// Self-dependency (artifact depends on itself).
    SelfDependency { artifact: String },
}

/// Dependency graph built from artifact dependencies.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// Adjacency list: artifact URI -> set of artifacts it depends on.
    pub depends_on: HashMap<String, HashSet<String>>,
    /// Reverse adjacency list: artifact URI -> set of artifacts that depend on it.
    pub depended_by: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    /// Build a dependency graph from a list of artifacts.
    pub fn from_artifacts(artifacts: &[Artifact]) -> Self {
        let mut depends_on: HashMap<String, HashSet<String>> = HashMap::new();
        let mut depended_by: HashMap<String, HashSet<String>> = HashMap::new();

        for artifact in artifacts {
            let uri = artifact.resource_uri.clone();

            // Initialize entries for this artifact
            depends_on.entry(uri.clone()).or_default();
            depended_by.entry(uri.clone()).or_default();

            // Process dependencies
            for dep in &artifact.dependencies {
                match dep.kind {
                    DependencyKind::DependsOn => {
                        depends_on
                            .entry(uri.clone())
                            .or_default()
                            .insert(dep.target_uri.clone());
                        depended_by
                            .entry(dep.target_uri.clone())
                            .or_default()
                            .insert(uri.clone());
                    }
                    DependencyKind::DependedBy => {
                        depended_by
                            .entry(uri.clone())
                            .or_default()
                            .insert(dep.target_uri.clone());
                        depends_on
                            .entry(dep.target_uri.clone())
                            .or_default()
                            .insert(uri.clone());
                    }
                }
            }
        }

        Self {
            depends_on,
            depended_by,
        }
    }

    /// Get all artifacts that directly depend on the given artifact.
    pub fn get_dependents(&self, uri: &str) -> Vec<String> {
        self.depended_by
            .get(uri)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all artifacts that the given artifact directly depends on.
    pub fn get_dependencies(&self, uri: &str) -> Vec<String> {
        self.depends_on
            .get(uri)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Detect cycles in the dependency graph using DFS.
    pub fn detect_cycles(&self) -> Vec<Vec<String>> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        let mut cycles = Vec::new();

        for node in self.depends_on.keys() {
            if !visited.contains(node) {
                self.dfs_cycle_detect(
                    node,
                    &mut visited,
                    &mut rec_stack,
                    &mut Vec::new(),
                    &mut cycles,
                );
            }
        }

        cycles
    }

    fn dfs_cycle_detect(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        visited.insert(node.to_string());
        rec_stack.insert(node.to_string());
        path.push(node.to_string());

        if let Some(neighbors) = self.depends_on.get(node) {
            for neighbor in neighbors {
                if !visited.contains(neighbor) {
                    self.dfs_cycle_detect(neighbor, visited, rec_stack, path, cycles);
                } else if rec_stack.contains(neighbor) {
                    // Found a cycle - extract it from path
                    if let Some(start_idx) = path.iter().position(|n| n == neighbor) {
                        let cycle = path[start_idx..].to_vec();
                        cycles.push(cycle);
                    }
                }
            }
        }

        path.pop();
        rec_stack.remove(node);
    }

    /// Check for self-dependencies (artifact depends on itself).
    pub fn detect_self_dependencies(&self) -> Vec<String> {
        let mut self_deps = Vec::new();

        for (uri, deps) in &self.depends_on {
            if deps.contains(uri) {
                self_deps.push(uri.clone());
            }
        }

        self_deps
    }
}

/// Find dependency targets that artifacts describe (via `[deps: ...]` /
/// `change_summary.json`'s `depends_on`/`depended_by`) but that never became a
/// tracked artifact in the same draft.
///
/// Root-caused 2026-07-03: an agent's decision-log/change-description text can
/// claim a file was edited (e.g. `plan.rs`'s entry lists `CLAUDE.md` under
/// `[deps: ...]`) without `CLAUDE.md` ever becoming its own `Artifact` in the
/// draft package. No existing check caught this before a supervisor `PASS`.
///
/// Returns the missing target URIs, in first-seen order, deduplicated.
pub fn untracked_dependency_targets(artifacts: &[Artifact]) -> Vec<String> {
    let tracked: std::collections::HashSet<&str> =
        artifacts.iter().map(|a| a.resource_uri.as_str()).collect();

    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for artifact in artifacts {
        for dep in &artifact.dependencies {
            if !tracked.contains(dep.target_uri.as_str()) && seen.insert(dep.target_uri.clone()) {
                missing.push(dep.target_uri.clone());
            }
        }
    }
    missing
}

/// Pre-PASS artifact/description consistency gate.
///
/// A draft's supervisor review must never return verdict `Pass` when an artifact's
/// own description references a file that isn't itself tracked as an artifact —
/// that disagreement between description and tracked-artifact-list is exactly the
/// root cause class from the 2026-07-03 incident (see `untracked_dependency_targets`).
/// When found, downgrades `Pass` to `Warn` and appends an explicit
/// "described but not tracked: <path>" finding per missing file, rather than
/// silently passing. Reviews already at `Warn`/`Block` are left untouched — they
/// are not "silently passing".
///
/// Returns `true` if the verdict was downgraded.
pub fn apply_artifact_consistency_gate(
    review: &mut crate::supervisor_review::SupervisorReview,
    artifacts: &[Artifact],
) -> bool {
    if review.verdict != crate::supervisor_review::SupervisorVerdict::Pass {
        return false;
    }

    let missing = untracked_dependency_targets(artifacts);
    if missing.is_empty() {
        return false;
    }

    review.verdict = crate::supervisor_review::SupervisorVerdict::Warn;
    for target in &missing {
        review
            .findings
            .push(format!("described but not tracked: {}", target));
    }
    true
}

/// Result of plan validation — checking if completed work matches plan expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanValidationResult {
    /// Whether the draft has content (at least one artifact).
    pub has_artifacts: bool,
    /// Number of artifacts in the draft.
    pub artifact_count: usize,
    /// Number of artifacts with descriptions (what/rationale populated).
    pub described_count: usize,
    /// Informational messages about plan alignment.
    pub notes: Vec<String>,
}

impl PlanValidationResult {
    /// Check if the draft has enough described artifacts to be considered complete.
    pub fn is_well_described(&self) -> bool {
        self.has_artifacts && self.described_count > 0
    }
}

/// Validate artifacts against plan expectations.
///
/// Checks that the draft has meaningful content and that artifacts are described.
/// This is called by `ta draft build` when a goal has a plan_phase.
pub fn validate_against_plan(
    artifacts: &[Artifact],
    phase_id: &str,
    phase_title: &str,
) -> PlanValidationResult {
    let artifact_count = artifacts.len();
    let described_count = artifacts
        .iter()
        .filter(|a| {
            a.explanation_tiers
                .as_ref()
                .map(|t| !t.summary.is_empty())
                .unwrap_or(false)
                || a.rationale.is_some()
        })
        .count();

    let mut notes = Vec::new();

    if artifact_count == 0 {
        notes.push(format!(
            "No artifacts found for phase {} — {}. Expected code changes.",
            phase_id, phase_title
        ));
    }

    if artifact_count > 0 && described_count == 0 {
        notes.push(format!(
            "None of the {} artifacts for phase {} have descriptions. Consider adding a change_summary.json.",
            artifact_count, phase_id
        ));
    }

    let undescribed = artifact_count.saturating_sub(described_count);
    if undescribed > 0 && described_count > 0 {
        notes.push(format!(
            "{}/{} artifacts for phase {} lack descriptions.",
            undescribed, artifact_count, phase_id
        ));
    }

    PlanValidationResult {
        has_artifacts: artifact_count > 0,
        artifact_count,
        described_count,
        notes,
    }
}

/// Supervisor agent that validates artifact dispositions against dependencies.
pub struct SupervisorAgent {
    graph: DependencyGraph,
}

impl SupervisorAgent {
    /// Create a new supervisor from a list of artifacts.
    pub fn new(artifacts: &[Artifact]) -> Self {
        Self {
            graph: DependencyGraph::from_artifacts(artifacts),
        }
    }

    /// Validate artifact dispositions against the dependency graph.
    ///
    /// Returns a ValidationResult with warnings about:
    /// - Rejecting artifacts that others depend on (coupled rejections)
    /// - Approving artifacts that depend on rejected ones (broken dependencies)
    /// - "Discuss" artifacts blocking approvals
    ///
    /// And errors for:
    /// - Cyclic dependencies
    /// - Self-dependencies
    pub fn validate(&self, artifacts: &[Artifact]) -> ValidationResult {
        let mut result = ValidationResult::valid();

        // Check for structural errors first
        for cycle in self.graph.detect_cycles() {
            result.add_error(ValidationError::CyclicDependency { cycle });
        }

        for self_dep in self.graph.detect_self_dependencies() {
            result.add_error(ValidationError::SelfDependency { artifact: self_dep });
        }

        // Build disposition map for quick lookup
        let dispositions: HashMap<String, ArtifactDisposition> = artifacts
            .iter()
            .map(|a| (a.resource_uri.clone(), a.disposition.clone()))
            .collect();

        // Check for coupled rejections and broken dependencies
        for artifact in artifacts {
            let uri = &artifact.resource_uri;
            let disposition = &artifact.disposition;

            match disposition {
                ArtifactDisposition::Rejected => {
                    // Check if any approved/discuss artifacts depend on this one
                    let dependents = self.graph.get_dependents(uri);
                    let affected: Vec<String> = dependents
                        .into_iter()
                        .filter(|dep_uri| {
                            matches!(
                                dispositions.get(dep_uri),
                                Some(ArtifactDisposition::Approved)
                                    | Some(ArtifactDisposition::Discuss)
                                    | Some(ArtifactDisposition::Pending)
                            )
                        })
                        .collect();

                    if !affected.is_empty() {
                        result.add_warning(ValidationWarning::CoupledRejection {
                            artifact: uri.clone(),
                            required_by: affected,
                        });
                    }
                }
                ArtifactDisposition::Approved => {
                    // Check if this artifact depends on any rejected ones
                    let dependencies = self.graph.get_dependencies(uri);
                    let rejected_deps: Vec<String> = dependencies
                        .into_iter()
                        .filter(|dep_uri| {
                            matches!(
                                dispositions.get(dep_uri),
                                Some(ArtifactDisposition::Rejected)
                            )
                        })
                        .collect();

                    if !rejected_deps.is_empty() {
                        result.add_warning(ValidationWarning::BrokenDependency {
                            artifact: uri.clone(),
                            depends_on_rejected: rejected_deps,
                        });
                    }
                }
                ArtifactDisposition::Discuss => {
                    // Check if any approved artifacts depend on this discuss item
                    let dependents = self.graph.get_dependents(uri);
                    let blocked: Vec<String> = dependents
                        .into_iter()
                        .filter(|dep_uri| {
                            matches!(
                                dispositions.get(dep_uri),
                                Some(ArtifactDisposition::Approved)
                            )
                        })
                        .collect();

                    if !blocked.is_empty() {
                        result.add_warning(ValidationWarning::DiscussBlockingApproval {
                            artifact: uri.clone(),
                            blocking: blocked,
                        });
                    }
                }
                ArtifactDisposition::Pending => {
                    // Pending is neutral - no validation needed
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_artifact(
        uri: &str,
        disposition: ArtifactDisposition,
        deps: Vec<(&str, DependencyKind)>,
    ) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: crate::draft_package::ChangeType::Modify,
            diff_ref: "test".to_string(),
            tests_run: Vec::new(),
            disposition,
            rationale: None,
            dependencies: deps
                .into_iter()
                .map(|(target, kind)| ChangeDependency {
                    target_uri: target.to_string(),
                    kind,
                })
                .collect(),
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: None,
        }
    }

    #[test]
    fn test_dependency_graph_simple() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Pending, vec![]),
        ];

        let graph = DependencyGraph::from_artifacts(&artifacts);

        assert_eq!(
            graph.get_dependencies("fs://workspace/a.rs"),
            vec!["fs://workspace/b.rs"]
        );
        assert_eq!(
            graph.get_dependents("fs://workspace/b.rs"),
            vec!["fs://workspace/a.rs"]
        );
    }

    #[test]
    fn test_coupled_rejection_warning() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Rejected, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(result.valid);
        assert_eq!(result.warnings.len(), 2);

        // Should warn about both: rejecting B that A depends on, and approving A that depends on rejected B
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::CoupledRejection { .. })));
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::BrokenDependency { .. })));
    }

    #[test]
    fn test_no_warning_when_consistent() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Approved, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(result.valid);
        assert_eq!(result.warnings.len(), 0);
    }

    #[test]
    fn test_self_dependency_error() {
        let artifacts = vec![make_artifact(
            "fs://workspace/a.rs",
            ArtifactDisposition::Pending,
            vec![("fs://workspace/a.rs", DependencyKind::DependsOn)],
        )];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(!result.valid);
        // Self-dependency is detected as both a self-dep and a cycle
        assert!(!result.errors.is_empty());
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::SelfDependency { .. })));
    }

    #[test]
    fn test_cycle_detection() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact(
                "fs://workspace/b.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/c.rs", DependencyKind::DependsOn)],
            ),
            make_artifact(
                "fs://workspace/c.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/a.rs", DependencyKind::DependsOn)],
            ),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(!result.valid);
        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
            result.errors[0],
            ValidationError::CyclicDependency { .. }
        ));
    }

    #[test]
    fn test_discuss_blocking_approval() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Discuss, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(result.valid);
        assert_eq!(result.warnings.len(), 1);
        assert!(matches!(
            result.warnings[0],
            ValidationWarning::DiscussBlockingApproval { .. }
        ));
    }

    #[test]
    fn test_depended_by_relationship() {
        let artifacts = vec![
            make_artifact("fs://workspace/a.rs", ArtifactDisposition::Pending, vec![]),
            make_artifact(
                "fs://workspace/b.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/a.rs", DependencyKind::DependedBy)],
            ),
        ];

        let graph = DependencyGraph::from_artifacts(&artifacts);

        // b.rs is depended by a.rs means a.rs depends on b.rs
        assert_eq!(
            graph.get_dependencies("fs://workspace/a.rs"),
            vec!["fs://workspace/b.rs"]
        );
        assert_eq!(
            graph.get_dependents("fs://workspace/b.rs"),
            vec!["fs://workspace/a.rs"]
        );
    }

    #[test]
    fn test_transitive_dependency_chain() {
        // A → B → C: rejecting C should warn about B (direct dependency)
        // A won't be warned because its direct dependency (B) is approved
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact(
                "fs://workspace/b.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/c.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/c.rs", ArtifactDisposition::Rejected, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        // Should have 2 warnings: C coupled rejection (breaks B) + B broken dependency (depends on rejected C)
        assert!(result.valid);
        assert_eq!(result.warnings.len(), 2);
    }

    #[test]
    fn test_disconnected_subgraphs() {
        // Two independent chains: A→B and C→D
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Rejected, vec![]),
            make_artifact(
                "fs://workspace/c.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/d.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/d.rs", ArtifactDisposition::Approved, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        // Should only warn about A→B, not C→D
        assert!(result.valid);
        assert_eq!(result.warnings.len(), 2); // B coupled rejection + A broken dependency
    }

    #[test]
    fn test_mixed_dispositions() {
        // Complex scenario: some approved, some rejected, some pending, some discuss
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Discuss, vec![]),
            make_artifact(
                "fs://workspace/c.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/d.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/d.rs", ArtifactDisposition::Pending, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        // Should warn about discuss blocking approval
        assert!(result.valid);
        assert_eq!(result.warnings.len(), 1);
        assert!(matches!(
            result.warnings[0],
            ValidationWarning::DiscussBlockingApproval { .. }
        ));
    }

    #[test]
    fn test_empty_artifacts() {
        let artifacts = vec![];
        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(result.valid);
        assert_eq!(result.warnings.len(), 0);
        assert_eq!(result.errors.len(), 0);
    }

    #[test]
    fn test_all_approved_no_dependencies() {
        let artifacts = vec![
            make_artifact("fs://workspace/a.rs", ArtifactDisposition::Approved, vec![]),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Approved, vec![]),
            make_artifact("fs://workspace/c.rs", ArtifactDisposition::Approved, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        assert!(result.valid);
        assert_eq!(result.warnings.len(), 0);
        assert_eq!(result.errors.len(), 0);
    }

    #[test]
    fn test_diamond_dependency() {
        // Diamond pattern: A→B, A→C, B→D, C→D
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Approved,
                vec![
                    ("fs://workspace/b.rs", DependencyKind::DependsOn),
                    ("fs://workspace/c.rs", DependencyKind::DependsOn),
                ],
            ),
            make_artifact(
                "fs://workspace/b.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/d.rs", DependencyKind::DependsOn)],
            ),
            make_artifact(
                "fs://workspace/c.rs",
                ArtifactDisposition::Approved,
                vec![("fs://workspace/d.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/d.rs", ArtifactDisposition::Rejected, vec![]),
        ];

        let supervisor = SupervisorAgent::new(&artifacts);
        let result = supervisor.validate(&artifacts);

        // Should warn about D being rejected but depended on by B and C
        assert!(result.valid);
        assert!(result.warnings.len() >= 3); // At least coupled rejection for D and broken deps for B, C
    }

    // ── Plan validation tests ──

    #[test]
    fn test_plan_validation_empty_artifacts() {
        let result = validate_against_plan(&[], "v0.3.1", "Plan Lifecycle");
        assert!(!result.has_artifacts);
        assert!(!result.is_well_described());
        assert_eq!(result.notes.len(), 1);
        assert!(result.notes[0].contains("No artifacts found"));
    }

    #[test]
    fn test_plan_validation_undescribed_artifacts() {
        let artifacts = vec![
            make_artifact("fs://workspace/a.rs", ArtifactDisposition::Pending, vec![]),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Pending, vec![]),
        ];
        let result = validate_against_plan(&artifacts, "v0.3.1", "Plan Lifecycle");
        assert!(result.has_artifacts);
        assert_eq!(result.artifact_count, 2);
        assert_eq!(result.described_count, 0);
        assert!(!result.is_well_described());
    }

    #[test]
    fn test_plan_validation_described_artifacts() {
        let mut a1 = make_artifact("fs://workspace/a.rs", ArtifactDisposition::Pending, vec![]);
        a1.explanation_tiers = Some(crate::draft_package::ExplanationTiers {
            summary: "Added plan validation".to_string(),
            explanation: String::new(),
            tags: vec![],
            related_artifacts: vec![],
        });
        let a2 = make_artifact("fs://workspace/b.rs", ArtifactDisposition::Pending, vec![]);

        let result = validate_against_plan(&[a1, a2], "v0.3.1", "Plan Lifecycle");
        assert!(result.has_artifacts);
        assert_eq!(result.described_count, 1);
        assert!(result.is_well_described());
        // Should note that 1/2 artifacts lack descriptions.
        assert!(result.notes.iter().any(|n| n.contains("1/2")));
    }

    #[test]
    fn test_plan_validation_all_described() {
        let mut a1 = make_artifact("fs://workspace/a.rs", ArtifactDisposition::Pending, vec![]);
        a1.rationale = Some("Reason".to_string());
        let mut a2 = make_artifact("fs://workspace/b.rs", ArtifactDisposition::Pending, vec![]);
        a2.explanation_tiers = Some(crate::draft_package::ExplanationTiers {
            summary: "Summary".to_string(),
            explanation: String::new(),
            tags: vec![],
            related_artifacts: vec![],
        });

        let result = validate_against_plan(&[a1, a2], "v0.3.1", "Plan Lifecycle");
        assert!(result.is_well_described());
        assert!(result.notes.is_empty());
    }

    // ── v0.17.0.12.11 Pre-PASS artifact/description consistency check ──

    #[test]
    fn untracked_dependency_targets_flags_missing_artifact() {
        // plan.rs's entry claims a dependency on CLAUDE.md, but CLAUDE.md never
        // became its own tracked artifact in the draft — the exact root-cause
        // scenario from the 2026-07-03 incident.
        let artifacts = vec![make_artifact(
            "fs://workspace/apps/ta-cli/src/commands/plan.rs",
            ArtifactDisposition::Pending,
            vec![("fs://workspace/CLAUDE.md", DependencyKind::DependsOn)],
        )];

        let missing = untracked_dependency_targets(&artifacts);
        assert_eq!(missing, vec!["fs://workspace/CLAUDE.md".to_string()]);
    }

    #[test]
    fn untracked_dependency_targets_empty_when_all_tracked() {
        let artifacts = vec![
            make_artifact(
                "fs://workspace/a.rs",
                ArtifactDisposition::Pending,
                vec![("fs://workspace/b.rs", DependencyKind::DependsOn)],
            ),
            make_artifact("fs://workspace/b.rs", ArtifactDisposition::Pending, vec![]),
        ];

        assert!(untracked_dependency_targets(&artifacts).is_empty());
    }

    #[test]
    fn apply_artifact_consistency_gate_downgrades_pass_to_warn() {
        let artifacts = vec![make_artifact(
            "fs://workspace/plan.rs",
            ArtifactDisposition::Pending,
            vec![("fs://workspace/CLAUDE.md", DependencyKind::DependsOn)],
        )];
        let mut review = crate::supervisor_review::SupervisorReview {
            verdict: crate::supervisor_review::SupervisorVerdict::Pass,
            scope_ok: true,
            findings: vec![],
            summary: "Looks good".to_string(),
            agent: "claude-code".to_string(),
            duration_secs: 1.0,
        };

        let downgraded = apply_artifact_consistency_gate(&mut review, &artifacts);

        assert!(downgraded);
        assert_eq!(
            review.verdict,
            crate::supervisor_review::SupervisorVerdict::Warn
        );
        assert!(review
            .findings
            .iter()
            .any(|f| f.contains("described but not tracked") && f.contains("CLAUDE.md")));
    }

    #[test]
    fn apply_artifact_consistency_gate_leaves_consistent_pass_untouched() {
        let artifacts = vec![make_artifact(
            "fs://workspace/a.rs",
            ArtifactDisposition::Pending,
            vec![],
        )];
        let mut review = crate::supervisor_review::SupervisorReview {
            verdict: crate::supervisor_review::SupervisorVerdict::Pass,
            scope_ok: true,
            findings: vec![],
            summary: "Looks good".to_string(),
            agent: "claude-code".to_string(),
            duration_secs: 1.0,
        };

        let downgraded = apply_artifact_consistency_gate(&mut review, &artifacts);

        assert!(!downgraded);
        assert_eq!(
            review.verdict,
            crate::supervisor_review::SupervisorVerdict::Pass
        );
        assert!(review.findings.is_empty());
    }
}
