// dependency_wave.rs — Dependency-wave planner (v0.17.0.12.34).
//
// Given a set of candidate nodes (plan phases or swarm sub-goals), each
// declaring which other candidates it depends on and which API surfaces it
// touches, partitions the set into ordered "waves": every node in wave N
// depends only on nodes in waves < N, so all nodes within a wave are safe to
// run concurrently as far as the *declared* dependency graph is concerned.
//
// This is read-only analysis — it does not execute anything. Two separate
// graphs feed the same wave computation:
//   - the explicit dependency graph (`depends_on`) — a real ordering
//     constraint a human wrote down.
//   - the declared API-impact graph (`api_impact`) — a same-wave overlap
//     check that downgrades two otherwise-independent nodes to sequential
//     waves when they declare touching the same API surface. This is a
//     cheap, conservative heuristic with expected false negatives (real,
//     undeclared drift) — that's why integration-time gating exists as the
//     real backstop, not a replacement for it.

use std::collections::{HashMap, HashSet};

/// A single node in the dependency-wave graph.
#[derive(Debug, Clone, Default)]
pub struct WaveNode {
    /// Unique identifier within the candidate set (a plan phase ID or a
    /// swarm sub-goal title).
    pub id: String,
    /// IDs of other candidate-set nodes that must complete before this one
    /// starts. Dependencies pointing outside the candidate set are the
    /// caller's responsibility to filter out before calling [`plan_waves`]
    /// (e.g. because they're already-completed phases).
    pub depends_on: Vec<String>,
    /// Free-text tokens describing API surfaces this node's work touches
    /// (e.g. `"TeamRole::find_by_role"`). Two nodes with any overlapping
    /// token are treated as conflicting even absent an explicit dependency.
    pub api_impact: Vec<String>,
}

impl WaveNode {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            depends_on: Vec::new(),
            api_impact: Vec::new(),
        }
    }

    pub fn with_deps(mut self, deps: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.depends_on = deps.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_api_impact(mut self, impact: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.api_impact = impact.into_iter().map(Into::into).collect();
        self
    }
}

/// Failure modes for wave planning — both are structural problems in the
/// declared dependency graph itself, not execution failures.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum WaveError {
    #[error("node '{0}' declares unknown dependency '{1}' (not in the candidate set)")]
    UnknownDependency(String, String),
    #[error("cycle detected in dependency graph involving: {0}")]
    Cycle(String),
}

/// Returns true if `a` and `b` share any declared API-impact token (trimmed,
/// exact match, case-sensitive since these are meant to be code identifiers).
///
/// This is deliberately cheap and conservative: it will miss real conflicts
/// nobody declared, and it may occasionally flag two nodes that don't
/// actually collide if their declared tokens happen to coincide. Both
/// directions are acceptable trade-offs for an upfront heuristic — see the
/// module doc comment.
pub fn api_impact_overlaps(a: &[String], b: &[String]) -> bool {
    let a_set: HashSet<&str> = a
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    b.iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .any(|token| a_set.contains(token))
}

/// Partition `nodes` into ordered waves, honoring both the explicit
/// dependency graph and the declared API-impact overlap heuristic.
///
/// Returns waves in execution order: every node in `waves[n]` depends only
/// on nodes in `waves[0..n]`, and no two nodes within the same wave declare
/// overlapping `api_impact` tokens. Nodes within a wave carry no ordering
/// requirement relative to each other and are safe to run concurrently.
///
/// Errors if a node declares a dependency not present in `nodes` (the caller
/// should filter out dependencies already satisfied outside the candidate
/// set before calling this), or if the dependency graph contains a cycle.
pub fn plan_waves(nodes: &[WaveNode]) -> Result<Vec<Vec<String>>, WaveError> {
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for node in nodes {
        for dep in &node.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(WaveError::UnknownDependency(node.id.clone(), dep.clone()));
            }
        }
    }

    let by_id: HashMap<&str, &WaveNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut in_degree: HashMap<&str, usize> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.depends_on.len()))
        .collect();
    // dependents: node id -> ids that declare a dependency on it
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes {
        for dep in &node.depends_on {
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(node.id.as_str());
        }
    }

    let mut waves: Vec<Vec<String>> = Vec::new();
    let mut processed: HashSet<&str> = HashSet::new();
    let mut ready_batch: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(id, _)| *id)
        .collect();
    ready_batch.sort_unstable();

    while !ready_batch.is_empty() {
        // This topological batch may still contain nodes that conflict on
        // declared API impact — split it further into sequential sub-waves.
        for sub_wave in split_by_api_overlap(&ready_batch, &by_id) {
            waves.push(sub_wave.iter().map(|s| s.to_string()).collect());
        }
        for &id in &ready_batch {
            processed.insert(id);
        }

        let mut next_batch: Vec<&str> = Vec::new();
        for &id in &ready_batch {
            if let Some(deps) = dependents.get(id) {
                for &dependent in deps {
                    if let Some(d) = in_degree.get_mut(dependent) {
                        *d -= 1;
                        if *d == 0 {
                            next_batch.push(dependent);
                        }
                    }
                }
            }
        }
        next_batch.sort_unstable();
        next_batch.dedup();
        ready_batch = next_batch;
    }

    if processed.len() != nodes.len() {
        let mut unresolved: Vec<&str> = nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| !processed.contains(id))
            .collect();
        unresolved.sort_unstable();
        return Err(WaveError::Cycle(unresolved.join(", ")));
    }

    Ok(waves)
}

/// Greedily bin-pack a topological batch into sub-waves so that no two
/// nodes sharing a declared `api_impact` token land in the same sub-wave.
/// Stable insertion order (batch is already sorted) keeps results
/// deterministic for tests and for repeat planning runs.
fn split_by_api_overlap<'a>(
    batch: &[&'a str],
    by_id: &HashMap<&str, &WaveNode>,
) -> Vec<Vec<&'a str>> {
    let mut sub_waves: Vec<Vec<&str>> = Vec::new();
    'outer: for &id in batch {
        let node = by_id[id];
        for sub in sub_waves.iter_mut() {
            let conflicts = sub.iter().any(|&other_id| {
                api_impact_overlaps(&node.api_impact, &by_id[other_id].api_impact)
            });
            if !conflicts {
                sub.push(id);
                continue 'outer;
            }
        }
        sub_waves.push(vec![id]);
    }
    sub_waves
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_deps_single_wave() {
        let nodes = vec![WaveNode::new("a"), WaveNode::new("b"), WaveNode::new("c")];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0], vec!["a", "b", "c"]);
    }

    #[test]
    fn linear_chain_produces_one_wave_each() {
        let nodes = vec![
            WaveNode::new("a"),
            WaveNode::new("b").with_deps(["a"]),
            WaveNode::new("c").with_deps(["b"]),
        ];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves, vec![vec!["a"], vec!["b"], vec!["c"]]);
    }

    #[test]
    fn diamond_dependency_groups_middle_wave() {
        // a -> b, c ; b, c -> d
        let nodes = vec![
            WaveNode::new("a"),
            WaveNode::new("b").with_deps(["a"]),
            WaveNode::new("c").with_deps(["a"]),
            WaveNode::new("d").with_deps(["b", "c"]),
        ];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves, vec![vec!["a"], vec!["b", "c"], vec!["d"]]);
    }

    #[test]
    fn unknown_dependency_errors() {
        let nodes = vec![WaveNode::new("a").with_deps(["missing"])];
        let err = plan_waves(&nodes).unwrap_err();
        assert_eq!(
            err,
            WaveError::UnknownDependency("a".to_string(), "missing".to_string())
        );
    }

    #[test]
    fn cycle_is_detected() {
        let nodes = vec![
            WaveNode::new("a").with_deps(["b"]),
            WaveNode::new("b").with_deps(["a"]),
        ];
        let err = plan_waves(&nodes).unwrap_err();
        assert!(matches!(err, WaveError::Cycle(_)));
    }

    #[test]
    fn three_node_cycle_is_detected() {
        let nodes = vec![
            WaveNode::new("a").with_deps(["c"]),
            WaveNode::new("b").with_deps(["a"]),
            WaveNode::new("c").with_deps(["b"]),
        ];
        assert!(plan_waves(&nodes).is_err());
    }

    #[test]
    fn api_impact_overlap_downgrades_independent_nodes_to_sequential() {
        // a and b have no declared dependency but touch the same API surface.
        let nodes = vec![
            WaveNode::new("a").with_api_impact(["TeamRole::find_by_role"]),
            WaveNode::new("b").with_api_impact(["TeamRole::find_by_role"]),
        ];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves, vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn api_impact_overlap_only_splits_conflicting_pair() {
        // a and b conflict; c is independent and should still run alongside a.
        let nodes = vec![
            WaveNode::new("a").with_api_impact(["gate::Diverge"]),
            WaveNode::new("b").with_api_impact(["gate::Diverge"]),
            WaveNode::new("c").with_api_impact(["unrelated::Thing"]),
        ];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves, vec![vec!["a", "c"], vec!["b"]]);
    }

    #[test]
    fn no_api_impact_declared_is_never_a_conflict() {
        let nodes = vec![WaveNode::new("a"), WaveNode::new("b")];
        let waves = plan_waves(&nodes).unwrap();
        assert_eq!(waves, vec![vec!["a", "b"]]);
    }

    #[test]
    fn api_impact_overlaps_exact_token_match() {
        assert!(api_impact_overlaps(
            &["Foo::bar".to_string()],
            &["Foo::bar".to_string(), "Baz::qux".to_string()]
        ));
        assert!(!api_impact_overlaps(
            &["Foo::bar".to_string()],
            &["Baz::qux".to_string()]
        ));
        assert!(!api_impact_overlaps(&[], &[]));
    }

    #[test]
    fn dependency_across_wave_and_api_overlap_together() {
        // a -> b (real dep); c independent but overlaps with b's API impact.
        let nodes = vec![
            WaveNode::new("a"),
            WaveNode::new("b").with_deps(["a"]).with_api_impact(["X"]),
            WaveNode::new("c").with_api_impact(["X"]),
        ];
        let waves = plan_waves(&nodes).unwrap();
        // Wave 1: only "a" (b depends on it). Wave 2 candidates: b and c —
        // but c has no dependency so it's ready immediately alongside a;
        // b only becomes ready after a. So actual topological batches are
        // {a, c} then {b}. Within {a, c} there's no api overlap (a has
        // none), so wave 1 = [a, c], wave 2 = [b].
        assert_eq!(waves, vec![vec!["a", "c"], vec!["b"]]);
    }
}
