// artifact_dag.rs — DAG resolution from artifact type compatibility (v0.14.10).
//
// Given a list of stages with declared `inputs` and `outputs`, this module
// resolves implicit dependency edges and returns a topologically sorted
// execution order.
//
// The algorithm:
//   For each stage, any stage that outputs a type matching this stage's inputs
//   becomes an implicit dependency edge. Explicit `depends_on` edges are also
//   respected. Cycles and missing producers are detected.

use std::collections::{HashMap, HashSet, VecDeque};

use ta_changeset::ArtifactType;
use tracing::warn;

use crate::definition::StageDefinition;
use crate::WorkflowError;

/// The execution order after DAG resolution, together with the type-derived
/// edges that were added implicitly.
#[derive(Debug, Clone)]
pub struct ResolvedDag {
    /// Topologically sorted stage names (ready-to-run first).
    pub order: Vec<String>,
    /// Edges derived from type compatibility: (producer, consumer).
    pub type_edges: Vec<(String, String)>,
    /// Types that were requested as inputs but no stage declares as an output.
    /// These are warnings, not errors — a workflow may start with pre-existing
    /// artifacts in the store (e.g., `GoalTitle` injected at run time).
    pub unresolved_inputs: Vec<MissingInput>,
}

/// A stage that declared an input type for which no producer was found.
#[derive(Debug, Clone)]
pub struct MissingInput {
    pub stage: String,
    pub artifact_type: ArtifactType,
}

/// Resolve the execution DAG for a list of stages.
///
/// Combines explicit `depends_on` edges with implicit type-compatibility edges
/// derived from `inputs`/`outputs` declarations. Returns `Err` if a cycle is
/// detected; unresolved input types are warnings in `unresolved_inputs`.
pub fn resolve_dag(stages: &[StageDefinition]) -> Result<ResolvedDag, WorkflowError> {
    // Map stage name → index for fast lookups.
    let name_to_idx: HashMap<&str, usize> = stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    // Build output_type → [stage_names] map so we can look up producers.
    let mut output_producers: HashMap<String, Vec<String>> = HashMap::new();
    for stage in stages {
        for out_type in &stage.outputs {
            output_producers
                .entry(out_type.to_string())
                .or_default()
                .push(stage.name.clone());
        }
    }

    // For each stage, collect all dependency edges (explicit + type-derived).
    let mut type_edges: Vec<(String, String)> = Vec::new();
    let mut unresolved_inputs: Vec<MissingInput> = Vec::new();

    // adjacency[i] = set of stage indices that stage i depends on.
    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); stages.len()];

    for (i, stage) in stages.iter().enumerate() {
        // Explicit depends_on edges.
        for dep_name in &stage.depends_on {
            if let Some(&j) = name_to_idx.get(dep_name.as_str()) {
                deps[i].insert(j);
            }
            // Unknown explicit deps are caught by validate_workflow, not here.
        }

        // Type-compatibility edges from inputs.
        for in_type in &stage.inputs {
            let type_key = in_type.to_string();
            match output_producers.get(&type_key) {
                None => {
                    warn!(
                        stage = %stage.name,
                        artifact_type = %in_type,
                        "stage declares input '{}' but no stage produces it — \
                         assuming it is pre-loaded in the artifact store",
                        in_type
                    );
                    unresolved_inputs.push(MissingInput {
                        stage: stage.name.clone(),
                        artifact_type: in_type.clone(),
                    });
                }
                Some(producers) => {
                    if producers.len() > 1 {
                        warn!(
                            stage = %stage.name,
                            artifact_type = %in_type,
                            producers = ?producers,
                            "ambiguous producer for '{}': multiple stages output this type. \
                             All will become implicit dependencies.",
                            in_type
                        );
                    }
                    for producer_name in producers {
                        // Don't add self-dependency.
                        if producer_name == &stage.name {
                            continue;
                        }
                        if let Some(&j) = name_to_idx.get(producer_name.as_str()) {
                            if deps[i].insert(j) {
                                // Only record as a type_edge if it wasn't already
                                // covered by an explicit depends_on.
                                if !stage.depends_on.contains(producer_name) {
                                    type_edges.push((producer_name.clone(), stage.name.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Kahn's algorithm for topological sort.
    let mut in_degree = vec![0usize; stages.len()];
    // Build reverse map: adjacency[i] contains stages that depend ON i.
    let mut reverse: Vec<Vec<usize>> = vec![Vec::new(); stages.len()];
    for (i, dep_set) in deps.iter().enumerate() {
        in_degree[i] += dep_set.len();
        for &j in dep_set {
            reverse[j].push(i);
        }
    }

    // Queue starts with all stages that have no dependencies.
    let mut queue: VecDeque<usize> = (0..stages.len()).filter(|&i| in_degree[i] == 0).collect();
    // Deterministic order: sort by stage name within the same in-degree bucket.
    let mut sorted_queue: Vec<usize> = queue.drain(..).collect();
    sorted_queue.sort_by_key(|&i| &stages[i].name);
    queue.extend(sorted_queue);

    let mut order: Vec<String> = Vec::with_capacity(stages.len());

    while let Some(i) = queue.pop_front() {
        order.push(stages[i].name.clone());
        // Collect neighbors, sort for determinism.
        let mut neighbors: Vec<usize> = reverse[i].clone();
        neighbors.sort_by_key(|&j| &stages[j].name);
        for j in neighbors {
            in_degree[j] -= 1;
            if in_degree[j] == 0 {
                queue.push_back(j);
            }
        }
    }

    if order.len() != stages.len() {
        // Find the first stage involved in the cycle for the error message.
        let cyclic = stages
            .iter()
            .find(|s| !order.contains(&s.name))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        return Err(WorkflowError::CycleDetected {
            id: "dag".to_string(),
            stage: cyclic,
        });
    }

    Ok(ResolvedDag {
        order,
        type_edges,
        unresolved_inputs,
    })
}

/// Render the resolved DAG as an ASCII diagram.
///
/// Example output:
/// ```text
/// generate-plan  ──[PlanDocument]──►  implement-plan  ──[DraftPackage]──►  review-draft
/// ```
pub fn render_ascii(stages: &[StageDefinition], dag: &ResolvedDag) -> String {
    if stages.is_empty() {
        return "(empty workflow)".to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Workflow DAG ({} stages):", stages.len()));
    lines.push(String::new());

    // Build a quick lookup: stage name → outputs
    let outputs_by_stage: HashMap<&str, &Vec<ArtifactType>> = stages
        .iter()
        .map(|s| (s.name.as_str(), &s.outputs))
        .collect();

    // Print stages in resolved order with type-edge annotations.
    for (pos, stage_name) in dag.order.iter().enumerate() {
        let stage = stages.iter().find(|s| &s.name == stage_name).unwrap();

        let inputs_str = if stage.inputs.is_empty() {
            String::new()
        } else {
            format!(
                " ← [{}]",
                stage
                    .inputs
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let outputs_str = if stage.outputs.is_empty() {
            String::new()
        } else {
            format!(
                " → [{}]",
                stage
                    .outputs
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        lines.push(format!(
            "  {:>2}. {}{}{}",
            pos + 1,
            stage_name,
            inputs_str,
            outputs_str
        ));
    }

    lines.push(String::new());

    // Print type edges.
    if !dag.type_edges.is_empty() {
        lines.push("Type-compatibility edges (auto-wired):".to_string());
        for (producer, consumer) in &dag.type_edges {
            // Find the types that connect them.
            let connecting: Vec<String> =
                if let Some(outs) = outputs_by_stage.get(producer.as_str()) {
                    let stage_in = stages
                        .iter()
                        .find(|s| &s.name == consumer)
                        .map(|s| &s.inputs);
                    if let Some(inputs) = stage_in {
                        outs.iter()
                            .filter(|t| inputs.contains(t))
                            .map(|t| t.to_string())
                            .collect()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
            let label = if connecting.is_empty() {
                String::new()
            } else {
                format!(" [{}]", connecting.join(", "))
            };
            lines.push(format!("  {}  ──{}──►  {}", producer, label, consumer));
        }
        lines.push(String::new());
    }

    // Warn about unresolved inputs.
    if !dag.unresolved_inputs.is_empty() {
        lines.push("Unresolved inputs (expect pre-loaded in artifact store):".to_string());
        for missing in &dag.unresolved_inputs {
            lines.push(format!(
                "  {} needs {}",
                missing.stage, missing.artifact_type
            ));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Render the DAG as Graphviz DOT format.
pub fn render_dot(name: &str, stages: &[StageDefinition], dag: &ResolvedDag) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("digraph \"{}\" {{", name));
    lines.push("  rankdir=LR;".to_string());
    lines.push("  node [shape=box, style=rounded];".to_string());

    // Nodes.
    for stage in stages {
        let label = if stage.inputs.is_empty() && stage.outputs.is_empty() {
            stage.name.clone()
        } else {
            let in_str = stage
                .inputs
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join("\\n");
            let out_str = stage
                .outputs
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join("\\n");
            let in_part = if in_str.is_empty() {
                String::new()
            } else {
                format!("\\nin: {}", in_str)
            };
            let out_part = if out_str.is_empty() {
                String::new()
            } else {
                format!("\\nout: {}", out_str)
            };
            format!("{}{}{}", stage.name, in_part, out_part)
        };
        lines.push(format!("  \"{}\" [label=\"{}\"];", stage.name, label));
    }

    // Explicit depends_on edges.
    for stage in stages {
        for dep in &stage.depends_on {
            lines.push(format!(
                "  \"{}\" -> \"{}\" [style=solid];",
                dep, stage.name
            ));
        }
    }

    // Type-compatibility edges (dashed with label).
    let outputs_by_stage: HashMap<&str, &Vec<ArtifactType>> = stages
        .iter()
        .map(|s| (s.name.as_str(), &s.outputs))
        .collect();
    for (producer, consumer) in &dag.type_edges {
        let connecting: Vec<String> = if let Some(outs) = outputs_by_stage.get(producer.as_str()) {
            let stage_in = stages
                .iter()
                .find(|s| &s.name == consumer)
                .map(|s| &s.inputs);
            if let Some(inputs) = stage_in {
                outs.iter()
                    .filter(|t| inputs.contains(t))
                    .map(|t| t.to_string())
                    .collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        };
        let label = connecting.join(", ");
        lines.push(format!(
            "  \"{}\" -> \"{}\" [style=dashed, label=\"{}\"];",
            producer, consumer, label
        ));
    }

    lines.push("}".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::StageDefinition;
    use crate::interaction::AwaitHumanConfig;

    fn make_stage(
        name: &str,
        depends_on: &[&str],
        inputs: &[ArtifactType],
        outputs: &[ArtifactType],
    ) -> StageDefinition {
        StageDefinition {
            name: name.to_string(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            roles: vec![],
            then: vec![],
            review: None,
            on_fail: None,
            await_human: AwaitHumanConfig::Never,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
        }
    }

    #[test]
    fn linear_chain_resolved_from_types() {
        // generate-plan → implement-plan → review-draft
        // No explicit depends_on; wired entirely by types.
        let stages = vec![
            make_stage(
                "implement-plan",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
            make_stage("generate-plan", &[], &[], &[ArtifactType::PlanDocument]),
            make_stage(
                "review-draft",
                &[],
                &[ArtifactType::DraftPackage],
                &[ArtifactType::ReviewVerdict],
            ),
        ];
        let dag = resolve_dag(&stages).unwrap();
        // generate-plan must come before implement-plan; implement-plan before review-draft.
        let pos: HashMap<&str, usize> = dag
            .order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        assert!(pos["generate-plan"] < pos["implement-plan"]);
        assert!(pos["implement-plan"] < pos["review-draft"]);
        // Two type edges expected.
        assert_eq!(dag.type_edges.len(), 2);
    }

    #[test]
    fn parallel_fan_out() {
        // plan outputs PlanDocument; both impl-a and impl-b consume it → parallel.
        let stages = vec![
            make_stage("plan", &[], &[], &[ArtifactType::PlanDocument]),
            make_stage(
                "impl-a",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
            make_stage(
                "impl-b",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
        ];
        let dag = resolve_dag(&stages).unwrap();
        let pos: HashMap<&str, usize> = dag
            .order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        // plan must come first.
        assert!(pos["plan"] < pos["impl-a"]);
        assert!(pos["plan"] < pos["impl-b"]);
        // Two type edges (plan→impl-a, plan→impl-b).
        assert_eq!(dag.type_edges.len(), 2);
    }

    #[test]
    fn cycle_detected() {
        // a outputs X, b consumes X and outputs Y, a consumes Y → cycle.
        let stages = vec![
            make_stage(
                "a",
                &[],
                &[ArtifactType::AgentMessage],
                &[ArtifactType::GoalTitle],
            ),
            make_stage(
                "b",
                &[],
                &[ArtifactType::GoalTitle],
                &[ArtifactType::AgentMessage],
            ),
        ];
        let result = resolve_dag(&stages);
        assert!(
            matches!(result, Err(WorkflowError::CycleDetected { .. })),
            "expected CycleDetected, got {:?}",
            result
        );
    }

    #[test]
    fn missing_input_type_is_warning_not_error() {
        // stage needs PlanDocument but nobody produces it.
        let stages = vec![make_stage(
            "implement",
            &[],
            &[ArtifactType::PlanDocument],
            &[ArtifactType::DraftPackage],
        )];
        let dag = resolve_dag(&stages).unwrap(); // must succeed
        assert_eq!(dag.unresolved_inputs.len(), 1);
        assert_eq!(dag.unresolved_inputs[0].stage, "implement");
        assert_eq!(
            dag.unresolved_inputs[0].artifact_type,
            ArtifactType::PlanDocument
        );
    }

    #[test]
    fn explicit_deps_plus_type_edges_no_duplicate() {
        // stage b has explicit depends_on = ["a"] AND a's output matches b's input.
        // The type edge should NOT be recorded separately.
        let stages = vec![
            make_stage("a", &[], &[], &[ArtifactType::PlanDocument]),
            make_stage("b", &["a"], &[ArtifactType::PlanDocument], &[]),
        ];
        let dag = resolve_dag(&stages).unwrap();
        // No type_edges — the dependency is already explicit.
        assert_eq!(dag.type_edges.len(), 0);
        // Order is still correct.
        let pos: HashMap<&str, usize> = dag
            .order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        assert!(pos["a"] < pos["b"]);
    }

    #[test]
    fn five_stage_mixed_chain_resolves() {
        // goal → plan → impl → review → audit (some via types, some explicit)
        let stages = vec![
            make_stage("goal", &[], &[], &[ArtifactType::GoalTitle]),
            make_stage(
                "plan",
                &[],
                &[ArtifactType::GoalTitle],
                &[ArtifactType::PlanDocument],
            ),
            make_stage(
                "impl",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
            make_stage(
                "review",
                &[],
                &[ArtifactType::DraftPackage],
                &[ArtifactType::ReviewVerdict],
            ),
            make_stage(
                "audit",
                &[],
                &[ArtifactType::ReviewVerdict],
                &[ArtifactType::AuditEntry],
            ),
        ];
        let dag = resolve_dag(&stages).unwrap();
        assert_eq!(dag.order.len(), 5);
        let pos: HashMap<&str, usize> = dag
            .order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        assert!(pos["goal"] < pos["plan"]);
        assert!(pos["plan"] < pos["impl"]);
        assert!(pos["impl"] < pos["review"]);
        assert!(pos["review"] < pos["audit"]);
        assert_eq!(dag.type_edges.len(), 4);
    }

    #[test]
    fn render_ascii_smoke() {
        let stages = vec![
            make_stage("plan", &[], &[], &[ArtifactType::PlanDocument]),
            make_stage(
                "impl",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
        ];
        let dag = resolve_dag(&stages).unwrap();
        let ascii = render_ascii(&stages, &dag);
        assert!(ascii.contains("plan"));
        assert!(ascii.contains("impl"));
        assert!(ascii.contains("PlanDocument"));
    }

    #[test]
    fn render_dot_smoke() {
        let stages = vec![
            make_stage("plan", &[], &[], &[ArtifactType::PlanDocument]),
            make_stage(
                "impl",
                &[],
                &[ArtifactType::PlanDocument],
                &[ArtifactType::DraftPackage],
            ),
        ];
        let dag = resolve_dag(&stages).unwrap();
        let dot = render_dot("test-workflow", &stages, &dag);
        assert!(dot.contains("digraph"));
        assert!(dot.contains("plan"));
        assert!(dot.contains("impl"));
        assert!(dot.contains("PlanDocument"));
    }
}
