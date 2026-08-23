// schema.rs — PLAN.md data model and parse schema (extracted from
// apps/ta-cli/src/commands/plan.rs, v0.17.11.1).

use std::fmt;
use std::path::Path;

/// Status of a plan phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
    /// Deferred phases are excluded from "next pending" but still appear in the checklist.
    Deferred,
}

impl PlanStatus {
    /// Returns true if this phase can be dispatched as new work.
    ///
    /// `InProgress` is NOT actionable — it means the phase is already claimed by a running
    /// goal and must be skipped by `find_next_pending`. Only `Pending` phases are eligible
    /// for new dispatch. (v0.15.24.2: fixed from `Pending | InProgress` to `Pending` only.)
    pub fn is_actionable(&self) -> bool {
        matches!(self, PlanStatus::Pending)
    }

    /// Returns true if the transition from `self` to `to` is a legal state-machine move.
    ///
    /// Legal transitions:
    ///   `pending    → in_progress`  (claim: ta run)
    ///   `in_progress → done`         (complete: ta draft apply)
    ///   `in_progress → pending`      (reset: ta draft deny or ta goal delete)
    ///
    /// Everything else is illegal.
    pub fn is_valid_transition_to(&self, to: &PlanStatus) -> bool {
        matches!(
            (self, to),
            (PlanStatus::Pending, PlanStatus::InProgress)
                | (PlanStatus::InProgress, PlanStatus::Done)
                | (PlanStatus::InProgress, PlanStatus::Pending)
        )
    }
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanStatus::Pending => write!(f, "pending"),
            PlanStatus::InProgress => write!(f, "in_progress"),
            PlanStatus::Done => write!(f, "done"),
            PlanStatus::Deferred => write!(f, "deferred"),
        }
    }
}

/// A parsed plan phase from PLAN.md.
#[derive(Debug, Clone)]
pub struct PlanPhase {
    /// Phase identifier (e.g., "0", "4b", "4a.1").
    pub id: String,
    /// Human-readable title (e.g., "Per-Artifact Review Model").
    pub title: String,
    /// Current status.
    pub status: PlanStatus,
    /// Explicit dependencies declared via `<!-- depends_on: v0.13.17.3 -->` comment
    /// (v0.14.3) or, far more commonly in practice, a `**Depends on**: v0.13.17.3
    /// (explanation), v0.14.1 (...)` prose line (parsed since v0.17.0.12.34 —
    /// see `find_depends_on_in_lookahead`). Parenthetical explanations are
    /// stripped; only the leading phase-id-shaped token from each entry is kept.
    pub depends_on: Vec<String>,
    /// Items from the `#### Human Review` subsection of this phase (v0.15.14.1).
    ///
    /// These items require a human to verify or sign off — agents must not check them.
    pub human_review_items: Vec<String>,
    /// Declared API surfaces this phase's work touches (v0.17.0.12.34), from a
    /// `**API impact**: adds Foo::bar; modifies Baz::qux` prose line. Used by
    /// the dependency-wave planner (`candidate_waves`) to downgrade two
    /// otherwise-independent phases to sequential waves when they declare
    /// touching the same API surface — a conflict class plain file-overlap
    /// can't catch.
    pub api_impact: Vec<String>,
}

// ── Schema-driven parsing ────────────────────────────────────────

/// A single phase-header pattern in the schema.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PhasePattern {
    /// Regex with capturing groups: group 1 = phase ID, group 2 (optional) = title.
    pub regex: String,
    /// Human-readable label for what this pattern captures (informational only).
    #[serde(default)]
    pub id_capture: String,
}

/// Schema describing how to parse a project's plan document.
/// Loaded from `.ta/plan-schema.yaml`. If absent, the built-in default is used.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanSchema {
    /// Path to the plan file, relative to project root (default: "PLAN.md").
    #[serde(default = "default_source")]
    pub source: String,
    /// One or more header patterns for phase detection (evaluated in order, first match wins).
    pub phase_patterns: Vec<PhasePattern>,
    /// Regex with one capture group that extracts the status value.
    pub status_marker: String,
    /// Recognized status values. Anything not in this list maps to Pending.
    #[serde(default = "default_statuses")]
    pub statuses: Vec<String>,
    /// Directories to search when resolving document paths in `ta plan from`.
    /// Relative to the project root. Searched in order; first match wins.
    /// If omitted, uses sensible defaults (docs/, spec/, design/, etc.).
    #[serde(default = "default_doc_search_dirs")]
    pub doc_search_dirs: Vec<String>,
}

fn default_source() -> String {
    "PLAN.md".to_string()
}

/// The default recognized status values (`PlanSchema::statuses`' default).
/// Public so callers that need to build a custom `PlanSchema` without going
/// through `PlanSchema::default_schema()` can still reuse the same defaults.
pub fn default_statuses() -> Vec<String> {
    vec![
        "done".to_string(),
        "in_progress".to_string(),
        "pending".to_string(),
        "deferred".to_string(),
    ]
}

/// The default document search directories (`PlanSchema::doc_search_dirs`'
/// default). Public for the same reason as [`default_statuses`].
pub fn default_doc_search_dirs() -> Vec<String> {
    vec![
        ".".to_string(),
        "docs".to_string(),
        "doc".to_string(),
        "documentation".to_string(),
        "specs".to_string(),
        "spec".to_string(),
        "design".to_string(),
        "rfcs".to_string(),
        "rfc".to_string(),
        "planning".to_string(),
        "plans".to_string(),
        "requirements".to_string(),
        ".ta".to_string(),
    ]
}

impl PlanSchema {
    /// The built-in default schema — matches the current PLAN.md format.
    /// Used when no `.ta/plan-schema.yaml` is present.
    pub fn default_schema() -> Self {
        PlanSchema {
            source: "PLAN.md".to_string(),
            phase_patterns: vec![
                PhasePattern {
                    // Matches: "## Phase 4b — Title" and "## Phase 4a.1 — Title"
                    regex: r"^##\s+Phase[\s\u{a0}]+([0-9a-z.]+)\s+[—\-]\s+(.+)$".to_string(),
                    id_capture: "phase_number".to_string(),
                },
                PhasePattern {
                    // Matches: "### v0.3.1 — Title" or "### v0.3.1.1 — Title"
                    regex: r"^###\s+(v[\d.]+[a-z]?)\s+[—\-]\s+(.+)$".to_string(),
                    id_capture: "version_number".to_string(),
                },
            ],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
            doc_search_dirs: default_doc_search_dirs(),
        }
    }

    /// Load schema from `.ta/plan-schema.yaml`, falling back to `default_schema()`.
    pub fn load_or_default(project_root: &Path) -> Self {
        let schema_path = project_root.join(".ta/plan-schema.yaml");
        if schema_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&schema_path) {
                if let Ok(schema) = serde_yaml::from_str::<PlanSchema>(&content) {
                    return schema;
                }
                eprintln!("Warning: failed to parse .ta/plan-schema.yaml — using default schema");
            }
        }
        Self::default_schema()
    }

    /// Serialize to YAML string.
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }
}
