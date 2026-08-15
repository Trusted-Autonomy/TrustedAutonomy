// graph/types.rs — Core node traits and data types for the workflow graph
// engine (v0.17.7.1).
//
// A workflow graph is a set of typed nodes connected by typed edges. Every
// node type implements exactly one of five traits below. `ReviewerVote` and
// `Decision` are reused from `crate::consensus` rather than redefined, per
// constitution §1.7 / PLAN.md v0.17.7.1 item 1.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use crate::consensus::{ConsensusResult as Decision, ReviewerVote};

// ── GraphContext ─────────────────────────────────────────────────────────

/// Shared, mutable-by-convention context threaded through every node call in
/// a single graph run. `vars` is a free-form pass-through bag (e.g. a
/// worker's `draft_id`) so nodes can hand data downstream without the engine
/// needing to know about every node kind's payload shape.
#[derive(Debug, Clone)]
pub struct GraphContext {
    /// Project root the graph is operating against.
    pub workspace_root: PathBuf,
    /// Unique identifier for this graph run (used for run-dir paths and
    /// consensus log files, same convention as `ConsensusInput::run_id`).
    pub run_id: String,
    /// Directory for this run's persisted state (`.ta/workflow-runs/<run-id>/graph/`).
    pub run_dir: PathBuf,
    /// Free-form key/value data passed between nodes across a single run
    /// (e.g. `"draft_id" -> "abc123"` set by a `WorkerNode`, read later by
    /// an `ActionNode`).
    pub vars: HashMap<String, String>,
}

impl GraphContext {
    pub fn new(workspace_root: impl Into<PathBuf>, run_id: impl Into<String>) -> Self {
        let workspace_root = workspace_root.into();
        let run_id = run_id.into();
        let run_dir = workspace_root
            .join(".ta")
            .join("workflow-runs")
            .join(&run_id)
            .join("graph");
        Self {
            workspace_root,
            run_id,
            run_dir,
            vars: HashMap::new(),
        }
    }
}

// ── TriggerPayload ───────────────────────────────────────────────────────

/// Typed payload a `TriggerSource` hands back when it fires.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TriggerPayload {
    /// The trigger kind that fired (matches the TOML `[[trigger]] kind`).
    pub kind: String,
    /// Free-form event data (e.g. `"review_id" -> "PR-123"`).
    pub data: HashMap<String, String>,
}

// ── WorkItem / WorkResult (WorkerNode) ───────────────────────────────────

/// Spec for a unit of work a `WorkerNode` dispatches. `verb`/`workload_hint`
/// are free-text data fed straight into `ta-brain::route()`'s
/// already-data-defined `workload_type` classification — a new domain (art,
/// docs, whatever) is a `workflow.toml` binding, not new Rust code.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkItem {
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub phase_id: Option<String>,
    /// "implement", "create", "fix" — data, not an enum.
    pub verb: String,
    #[serde(default)]
    pub workload_hint: Option<String>,
}

/// Reference to the work a `WorkerNode` dispatched, for the review graph
/// segment to pick up (e.g. a draft ID).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkResult {
    pub draft_id: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ── ReviewInput (ReviewerNode) ───────────────────────────────────────────

/// The draft/decision context handed to every `ReviewerNode`. Carries the
/// union of fields today's three separate approval mechanisms each need
/// (`ta_policy::DraftInfo`'s size/path fields, `ta_decision::gate`'s
/// verdict/risk/confidence fields) so any reviewer kind can read what it
/// needs without the engine special-casing per-reviewer input shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewInput {
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub lines_changed: usize,
    #[serde(default)]
    pub plan_phase: Option<String>,
    #[serde(default)]
    pub agent_id: String,
    /// 0-100, higher is riskier — feeds `AdvisorConfidenceReviewer`.
    #[serde(default)]
    pub risk_score: u32,
    /// 0.0-1.0 — feeds `AdvisorConfidenceReviewer`.
    #[serde(default)]
    pub confidence: f64,
    /// Pass/Warn/Block — feeds `AdvisorConfidenceReviewer`.
    #[serde(default = "default_verdict")]
    pub verdict: ta_decision::Verdict,
}

fn default_verdict() -> ta_decision::Verdict {
    ta_decision::Verdict::Pass
}

impl Default for ReviewInput {
    fn default() -> Self {
        Self {
            draft_id: None,
            changed_paths: Vec::new(),
            lines_changed: 0,
            plan_phase: None,
            agent_id: String::new(),
            risk_score: 0,
            confidence: 1.0,
            verdict: default_verdict(),
        }
    }
}

// ── ActionOutcome (ActionNode) ───────────────────────────────────────────

/// Effect an `ActionNode` produced.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionOutcome {
    /// The action kind that ran (matches the TOML `[action] kind`).
    pub kind: String,
    /// True when the action actually mutated state (e.g. applied a draft).
    /// `RecommendAction` always reports `false` here — it never applies.
    pub applied: bool,
    /// Human-readable summary, always populated (Observable & Actionable).
    pub message: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ── GraphError ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("failed to read graph definition at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse graph definition: {0}")]
    Parse(String),
    #[error("graph definition invalid: {0}")]
    InvalidDefinition(String),
    #[error("no {category} node registered for kind '{kind}'")]
    NodeNotFound {
        category: &'static str,
        kind: String,
    },
    #[error("node '{node_id}' failed: {message}")]
    NodeExecution { node_id: String, message: String },
}

impl From<crate::WorkflowError> for GraphError {
    fn from(err: crate::WorkflowError) -> Self {
        GraphError::NodeExecution {
            node_id: "decision".to_string(),
            message: err.to_string(),
        }
    }
}

// ── Node traits ───────────────────────────────────────────────────────────

/// Blocks/polls until the event fires; returns a typed payload.
pub trait TriggerSource {
    fn wait(&self, ctx: &GraphContext) -> Result<TriggerPayload, GraphError>;
}

/// Consumes a work-item spec, dispatches it, returns a reference (e.g. a
/// draft ID) for the review graph segment to pick up.
pub trait WorkerNode {
    fn dispatch(&self, item: &WorkItem, ctx: &GraphContext) -> Result<WorkResult, GraphError>;
}

/// Produces one scored vote. Wraps today's separate approval mechanisms as
/// interchangeable implementations of the same trait.
pub trait ReviewerNode {
    fn review(&self, input: &ReviewInput, ctx: &GraphContext) -> Result<ReviewerVote, GraphError>;
}

/// Fans in N `ReviewerVote`s, applies weights/threshold/algorithm, emits a
/// typed `Decision`. Does NOT act.
pub trait DecisionNode {
    fn decide(&self, votes: &[ReviewerVote], ctx: &GraphContext) -> Result<Decision, GraphError>;
}

/// Consumes a `Decision` and performs an effect.
pub trait ActionNode {
    fn act(&self, decision: &Decision, ctx: &GraphContext) -> Result<ActionOutcome, GraphError>;
}
