// draft_package.rs — Draft Package: the milestone deliverable for human review.
//
// A Draft Package bundles all staged changes (ChangeSets) from a goal iteration
// into a single reviewable artifact. It includes:
// - What changed and why (summary)
// - The actual changes (artifacts + patch_sets)
// - Risk assessment
// - Provenance (where inputs came from)
// - Review requests (what approvals are needed)
//
// The structure aligns with schema/draft_package.schema.json.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---- Goal ----

/// The high-level goal this PR package contributes to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub goal_id: String,
    pub title: String,
    pub objective: String,
    pub success_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    /// Title of the root/parent goal this is a follow-up to (v0.13.0.1).
    /// Preserved so draft view and apply can show the chain context even if
    /// the parent goal record is no longer available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_goal_title: Option<String>,
}

// ---- Iteration ----

/// Which iteration of the goal this package represents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Iteration {
    pub iteration_id: String,
    pub sequence: u32,
    pub workspace_ref: WorkspaceRef,
}

/// Reference to the workspace where changes were staged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRef {
    #[serde(rename = "type")]
    pub ref_type: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
}

// ---- Agent Identity ----

/// Identity of the agent that produced this PR package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: String,
    pub agent_type: String,
    pub constitution_id: String,
    pub capability_manifest_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator_run_id: Option<String>,
}

// ---- Summary ----

/// Human-readable summary of what changed and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub what_changed: String,
    pub why: String,
    pub impact: String,
    pub rollback_plan: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    /// Design alternatives considered during this work (v0.9.5).
    /// Populated by agents via the `alternatives` parameter on `ta_pr_build`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives_considered: Vec<DesignAlternative>,
}

/// A design alternative considered during agent work (v0.9.5).
///
/// Agents report which options they evaluated and why they chose one over others.
/// Displayed under "Design Decisions" in `ta draft view`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesignAlternative {
    /// The option that was considered (e.g., "Use a HashMap for lookup").
    pub option: String,
    /// Why this option was chosen or rejected.
    pub rationale: String,
    /// Whether this was the chosen approach.
    #[serde(default)]
    pub chosen: bool,
}

// ---- Changes ----

/// The changes section: artifacts (local FS changes) + patch_sets (external changes)
/// + pending_actions (intercepted MCP tool calls, v0.5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Changes {
    pub artifacts: Vec<Artifact>,
    pub patch_sets: Vec<PatchSet>,
    /// MCP tool calls intercepted for human review (v0.5.1).
    /// State-changing external actions are captured here instead of being
    /// executed immediately. Read-only calls pass through unintercepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_actions: Vec<PendingAction>,
}

/// An MCP tool call intercepted during agent execution, pending human review.
///
/// When an agent calls an external MCP tool (e.g., `gmail_send`, `slack_post`),
/// TA intercepts the call, records it here, and holds it for human approval.
/// Read-only calls (search, list, get) pass through immediately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAction {
    /// Unique identifier for this action instance.
    pub action_id: Uuid,
    /// The MCP tool name that was called (e.g., "gmail_send", "slack_post").
    pub tool_name: String,
    /// Serialized tool parameters as provided by the agent (credentials redacted).
    pub parameters: serde_json::Value,
    /// How this action was classified.
    pub kind: ActionKind,
    /// When the tool call was intercepted.
    pub intercepted_at: DateTime<Utc>,
    /// Human-readable description for the reviewer.
    pub description: String,
    /// Resource this action targets (URI scheme, e.g., "mcp://gmail/send").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_uri: Option<String>,
    /// Whether this action has been approved for replay.
    #[serde(default)]
    pub disposition: ArtifactDisposition,
}

/// How an intercepted MCP tool call is classified.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Read-only — no side effects. Passed through without interception.
    ReadOnly,
    /// Produces a side effect — captured for human review.
    StateChanging,
    /// Cannot be automatically classified — requires human review.
    Unclassified,
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionKind::ReadOnly => write!(f, "read-only"),
            ActionKind::StateChanging => write!(f, "state-changing"),
            ActionKind::Unclassified => write!(f, "unclassified"),
        }
    }
}

/// Three-tier explanation for an artifact (v0.2.3).
///
/// Agents populate this via `.diff.explanation.yaml` sidecar files.
/// Enables tiered review: top (one-line) → medium (paragraph) → full (with diff).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplanationTiers {
    /// One-line summary (e.g., "Refactored auth middleware to use JWT").
    pub summary: String,
    /// Paragraph explaining what changed and why, dependencies affected.
    pub explanation: String,
    /// Optional tags for categorization (e.g., "security", "breaking-change").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Related artifacts (URIs) that are connected to this change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_artifacts: Vec<String>,
}

/// A local filesystem change artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub resource_uri: String,
    pub change_type: ChangeType,
    pub diff_ref: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests_run: Vec<String>,
    /// Per-artifact review disposition (defaults to Pending).
    #[serde(default)]
    pub disposition: ArtifactDisposition,
    /// Why this change was made (from agent's change_summary.json).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Dependencies: other artifacts this one requires or is required by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ChangeDependency>,
    /// Three-tier explanation (summary, explanation, tags) from sidecar YAML (v0.2.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation_tiers: Option<ExplanationTiers>,
    /// Comment thread for this artifact (v0.3.0 — Review Sessions).
    /// Comments from ReviewSession are merged here during draft finalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<crate::review_session::CommentThread>,
    /// Amendment record if this artifact was amended after initial creation (v0.3.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendment: Option<AmendmentRecord>,
}

/// Record of a human amendment to an artifact (v0.3.4).
///
/// Tracks who amended the artifact, when, and how — for audit trail purposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmendmentRecord {
    /// Who performed the amendment (e.g., "human", reviewer name).
    pub amended_by: String,
    /// When the amendment was made.
    pub amended_at: DateTime<Utc>,
    /// What kind of amendment was performed.
    pub amendment_type: AmendmentType,
    /// Optional reason for the amendment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The type of amendment applied to an artifact (v0.3.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AmendmentType {
    /// Artifact content replaced with a corrected file (--file).
    FileReplaced,
    /// A patch was applied to the artifact (--patch).
    PatchApplied,
    /// Artifact was removed from the draft (--drop).
    Dropped,
}

/// Per-artifact review disposition.
///
/// Tracks the reviewer's decision on each individual artifact,
/// enabling selective approval (approve some, reject others, discuss the rest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDisposition {
    /// Not yet reviewed.
    #[default]
    Pending,
    /// Approved — will be applied.
    Approved,
    /// Rejected — will not be applied.
    Rejected,
    /// Needs discussion before deciding.
    Discuss,
}

impl fmt::Display for ArtifactDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactDisposition::Pending => write!(f, "pending"),
            ArtifactDisposition::Approved => write!(f, "approved"),
            ArtifactDisposition::Rejected => write!(f, "rejected"),
            ArtifactDisposition::Discuss => write!(f, "discuss"),
        }
    }
}

/// A dependency relationship between artifacts.
///
/// Reported by the agent via .ta/change_summary.json, used by the
/// reviewer to understand which changes can be independently accepted/rejected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeDependency {
    /// The resource_uri of the related artifact.
    pub target_uri: String,
    /// The nature of the dependency.
    pub kind: DependencyKind,
}

/// How two artifacts are related.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// This artifact requires the target (can't apply without it).
    DependsOn,
    /// The target requires this artifact (target breaks if this is reverted).
    DependedBy,
}

/// The type of filesystem change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Add,
    Modify,
    Delete,
    Rename,
}

/// A staged change to an external resource (Drive, Gmail, DB, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSet {
    pub patch_set_id: String,
    pub target_uri: String,
    pub action: PatchAction,
    pub preview_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_intent: Option<String>,
}

/// Action types for external patch sets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchAction {
    WritePatch,
    CreateDraft,
    LabelChange,
    PermissionChange,
    DbPatch,
    SchedulePost,
}

// ---- Risk ----

/// Risk assessment for the PR package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Risk {
    pub risk_score: u32,
    pub findings: Vec<RiskFinding>,
    pub policy_decisions: Vec<PolicyDecisionRecord>,
}

/// A single risk finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskFinding {
    pub category: RiskCategory,
    pub severity: Severity,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mitigation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskCategory {
    Pii,
    Secrets,
    Exfiltration,
    ExternalComm,
    PromptInjection,
    PolicyViolation,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// A recorded policy decision relevant to this PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecisionRecord {
    pub rule_id: String,
    pub effect: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Grants that were checked during evaluation (v0.3.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants_checked: Vec<String>,
    /// The grant that matched (if any) (v0.3.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matching_grant: Option<String>,
    /// Evaluation steps the policy engine performed (v0.3.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluation_steps: Vec<String>,
}

// ---- Provenance ----

/// Provenance information: where inputs came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub inputs: Vec<ProvenanceInput>,
    pub tool_trace_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceInput {
    pub source_type: String,
    #[serde(rename = "ref")]
    pub ref_uri: String,
    pub trust_level: TrustLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Trusted,
    Untrusted,
    Quarantined,
}

// ---- Review Requests ----

/// What approvals this PR needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequests {
    pub requested_actions: Vec<RequestedAction>,
    pub reviewers: Vec<String>,
    #[serde(default = "default_required_approvals")]
    pub required_approvals: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_to_reviewer: Option<String>,
}

fn default_required_approvals() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestedAction {
    pub action: String,
    pub targets: Vec<String>,
}

// ---- Signatures ----

/// Cryptographic signatures for the PR package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signatures {
    pub package_hash: String,
    pub agent_signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_attestation: Option<String>,
}

// ---- Approval Record ----

/// Records a single reviewer's approval of a draft package (v0.14.2).
///
/// Multiple `ApprovalRecord`s accumulate in `DraftPackage::pending_approvals`
/// until the governance quorum is reached, at which point the draft transitions
/// to `DraftStatus::Approved`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Reviewer identity (name or email).
    pub reviewer: String,
    /// When this approval was recorded.
    pub approved_at: DateTime<Utc>,
}

// ---- Draft Package (top level) ----

/// The Draft Package — a complete, reviewable milestone deliverable.
///
/// This is the central artifact of Trusted Autonomy. Every goal iteration
/// produces one of these for human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPackage {
    pub package_version: String,
    pub package_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub goal: Goal,
    pub iteration: Iteration,
    pub agent_identity: AgentIdentity,
    pub summary: Summary,
    pub plan: Plan,
    pub changes: Changes,
    pub risk: Risk,
    pub provenance: Provenance,
    pub review_requests: ReviewRequests,
    pub signatures: Signatures,

    /// Tracks the review status (not in the JSON schema — internal state).
    #[serde(default)]
    pub status: DraftStatus,

    /// Verification warnings from pre-draft verification gate (v0.10.8).
    /// Populated when `[verify] on_failure = "warn"` and a command fails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_warnings: Vec<VerificationWarning>,

    /// Human-friendly display ID derived from the goal ID (v0.10.11).
    /// Format: `<goal-id-prefix>-NN` (e.g., `511e0465-01`).
    /// Falls back to `package_id` short prefix for legacy drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,

    /// Human-friendly goal tag inherited from the parent GoalRun (v0.11.2.3).
    /// The primary display identifier in all draft listing contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,

    /// VCS tracking information populated during commit/push/open_review (v0.11.2.3).
    /// Tracks the PR lifecycle after apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_status: Option<VcsTrackingInfo>,

    /// Parent draft ID for follow-up goals (v0.12.2.1).
    /// When set, this draft is a follow-up to the parent draft. Used for chain
    /// display (`ta draft view` combined impact) and chain apply (`ta draft apply --chain`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_draft_id: Option<Uuid>,

    /// Accumulated reviewer approvals for multi-party governance (v0.14.2).
    /// Empty for single-approver workflows (legacy / require_approvals = 1).
    /// Grows as each reviewer calls `ta draft approve --as <identity>`.
    /// When `pending_approvals.len() >= require_approvals` the draft transitions
    /// to `DraftStatus::Approved`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_approvals: Vec<ApprovalRecord>,
}

/// VCS tracking information for post-apply lifecycle monitoring (v0.11.2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcsTrackingInfo {
    /// Branch name the changes were committed to.
    pub branch: String,
    /// PR/review URL (e.g., GitHub PR URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_url: Option<String>,
    /// PR/review identifier (e.g., PR number).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    /// PR/review state: "open", "merged", "closed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    /// Commit SHA of the applied changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// When VCS status was last checked/updated.
    pub last_checked: DateTime<Utc>,
}

/// A warning from a pre-draft verification command failure (v0.10.8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationWarning {
    /// The command that failed.
    pub command: String,
    /// The exit code (if available).
    pub exit_code: Option<i32>,
    /// Captured stderr/stdout output (truncated to 2000 chars).
    pub output: String,
}

/// Execution plan included in the PR package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub completed_steps: Vec<String>,
    pub next_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_log: Vec<DecisionLogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionLogEntry {
    pub decision: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// Structured alternatives with rejection reasons (v0.3.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives_considered: Vec<AlternativeConsidered>,
}

/// A structured alternative considered during a decision (v0.3.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlternativeConsidered {
    pub description: String,
    pub rejected_reason: String,
}

/// Review status of a draft package (internal tracking, not in JSON schema).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DraftStatus {
    #[default]
    Draft,
    PendingReview,
    Approved {
        approved_by: String,
        approved_at: DateTime<Utc>,
    },
    Denied {
        reason: String,
        denied_by: String,
    },
    Applied {
        applied_at: DateTime<Utc>,
    },
    /// This draft has been superseded by a follow-up goal's draft.
    Superseded {
        superseded_by: Uuid,
    },
    /// This draft has been manually closed (abandoned, hand-merged, or obsolete).
    Closed {
        closed_at: DateTime<Utc>,
        reason: Option<String>,
        closed_by: String,
    },
}

impl std::fmt::Display for DraftStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftStatus::Draft => write!(f, "draft"),
            DraftStatus::PendingReview => write!(f, "pending_review"),
            DraftStatus::Approved { .. } => write!(f, "approved"),
            DraftStatus::Denied { .. } => write!(f, "denied"),
            DraftStatus::Applied { .. } => write!(f, "applied"),
            DraftStatus::Superseded { .. } => write!(f, "superseded"),
            DraftStatus::Closed { .. } => write!(f, "closed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal valid draft package for testing.
    fn test_package() -> DraftPackage {
        DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::new_v4(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: "goal-1".to_string(),
                title: "Test Goal".to_string(),
                objective: "Test the system".to_string(),
                success_criteria: vec!["tests pass".to_string()],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging_dir".to_string(),
                    ref_name: "staging/goal-1/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "research".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "abc123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Added test file".to_string(),
                why: "To verify the system works".to_string(),
                impact: "No production impact".to_string(),
                rollback_plan: "Delete the file".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec!["Created file".to_string()],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![Artifact {
                    resource_uri: "fs://workspace/test.txt".to_string(),
                    change_type: ChangeType::Add,
                    diff_ref: "diff-001".to_string(),
                    tests_run: vec![],
                    disposition: Default::default(),
                    rationale: None,
                    dependencies: vec![],
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                }],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 10,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace-hash-123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![RequestedAction {
                    action: "merge".to_string(),
                    targets: vec!["fs://workspace/test.txt".to_string()],
                }],
                reviewers: vec!["human-reviewer".to_string()],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "pkg-hash-456".to_string(),
                agent_signature: "sig-789".to_string(),
                gateway_attestation: None,
            },
            status: DraftStatus::Draft,
            verification_warnings: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
        }
    }

    #[test]
    fn draft_package_serialization_round_trip() {
        let pkg = test_package();
        let json = serde_json::to_string_pretty(&pkg).unwrap();
        let restored: DraftPackage = serde_json::from_str(&json).unwrap();

        assert_eq!(pkg.package_id, restored.package_id);
        assert_eq!(pkg.package_version, restored.package_version);
        assert_eq!(pkg.goal.goal_id, restored.goal.goal_id);
        assert_eq!(
            pkg.changes.artifacts.len(),
            restored.changes.artifacts.len()
        );
    }

    #[test]
    fn draft_status_transitions() {
        // Draft → PendingReview
        let status = DraftStatus::PendingReview;
        assert_eq!(status.to_string(), "pending_review");

        // PendingReview → Approved
        let status = DraftStatus::Approved {
            approved_by: "reviewer".to_string(),
            approved_at: Utc::now(),
        };
        assert_eq!(status.to_string(), "approved");

        // PendingReview → Denied
        let status = DraftStatus::Denied {
            reason: "needs changes".to_string(),
            denied_by: "reviewer".to_string(),
        };
        assert_eq!(status.to_string(), "denied");

        // Approved → Applied
        let status = DraftStatus::Applied {
            applied_at: Utc::now(),
        };
        assert_eq!(status.to_string(), "applied");
    }

    #[test]
    fn draft_status_default_is_draft() {
        let status = DraftStatus::default();
        assert_eq!(status, DraftStatus::Draft);
    }

    #[test]
    fn draft_package_json_contains_required_fields() {
        // Verify the serialized JSON includes all required fields from the schema.
        let pkg = test_package();
        let json = serde_json::to_string(&pkg).unwrap();

        // Check required top-level fields from draft_package.schema.json
        assert!(json.contains("\"package_version\""));
        assert!(json.contains("\"package_id\""));
        assert!(json.contains("\"created_at\""));
        assert!(json.contains("\"goal\""));
        assert!(json.contains("\"iteration\""));
        assert!(json.contains("\"agent_identity\""));
        assert!(json.contains("\"summary\""));
        assert!(json.contains("\"changes\""));
        assert!(json.contains("\"risk\""));
        assert!(json.contains("\"provenance\""));
        assert!(json.contains("\"review_requests\""));
        assert!(json.contains("\"signatures\""));
    }

    #[test]
    fn risk_finding_serialization() {
        let finding = RiskFinding {
            category: RiskCategory::Secrets,
            severity: Severity::High,
            description: "API key detected in file".to_string(),
            evidence_refs: vec!["line 42".to_string()],
            mitigation: Some("Remove the key".to_string()),
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(json.contains("\"secrets\""));
        assert!(json.contains("\"high\""));
    }

    #[test]
    fn change_type_serialization() {
        assert_eq!(serde_json::to_string(&ChangeType::Add).unwrap(), "\"add\"");
        assert_eq!(
            serde_json::to_string(&ChangeType::Modify).unwrap(),
            "\"modify\""
        );
    }

    #[test]
    fn artifact_disposition_default_is_pending() {
        let d = ArtifactDisposition::default();
        assert_eq!(d, ArtifactDisposition::Pending);
        assert_eq!(d.to_string(), "pending");
    }

    #[test]
    fn artifact_disposition_serialization() {
        assert_eq!(
            serde_json::to_string(&ArtifactDisposition::Approved).unwrap(),
            "\"approved\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactDisposition::Rejected).unwrap(),
            "\"rejected\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactDisposition::Discuss).unwrap(),
            "\"discuss\""
        );
    }

    #[test]
    fn artifact_with_disposition_round_trip() {
        let artifact = Artifact {
            resource_uri: "fs://workspace/src/main.rs".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Approved,
            rationale: Some("Fixed the bug".to_string()),
            dependencies: vec![ChangeDependency {
                target_uri: "fs://workspace/src/lib.rs".to_string(),
                kind: DependencyKind::DependsOn,
            }],
            explanation_tiers: None,
            comments: None,
            amendment: None,
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let restored: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.disposition, ArtifactDisposition::Approved);
        assert_eq!(restored.rationale, Some("Fixed the bug".to_string()));
        assert_eq!(restored.dependencies.len(), 1);
        assert_eq!(restored.dependencies[0].kind, DependencyKind::DependsOn);
    }

    #[test]
    fn artifact_without_new_fields_deserializes_with_defaults() {
        // Backward compatibility: old JSON without disposition/rationale/dependencies.
        let json = r#"{
            "resource_uri": "fs://workspace/test.txt",
            "change_type": "add",
            "diff_ref": "changeset:0"
        }"#;
        let artifact: Artifact = serde_json::from_str(json).unwrap();
        assert_eq!(artifact.disposition, ArtifactDisposition::Pending);
        assert!(artifact.rationale.is_none());
        assert!(artifact.dependencies.is_empty());
    }

    #[test]
    fn dependency_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&DependencyKind::DependsOn).unwrap(),
            "\"depends_on\""
        );
        assert_eq!(
            serde_json::to_string(&DependencyKind::DependedBy).unwrap(),
            "\"depended_by\""
        );
    }

    #[test]
    fn draft_status_superseded_serialization() {
        let superseding_id = Uuid::new_v4();
        let status = DraftStatus::Superseded {
            superseded_by: superseding_id,
        };
        assert_eq!(status.to_string(), "superseded");
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"superseded\""));
        assert!(json.contains(&superseding_id.to_string()));
        let restored: DraftStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn draft_status_closed_serialization() {
        let status = DraftStatus::Closed {
            closed_at: Utc::now(),
            reason: Some("Hand-merged upstream".to_string()),
            closed_by: "human-reviewer".to_string(),
        };
        assert_eq!(status.to_string(), "closed");
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"closed\""));
        assert!(json.contains("Hand-merged upstream"));
        let restored: DraftStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn draft_status_closed_without_reason() {
        let status = DraftStatus::Closed {
            closed_at: Utc::now(),
            reason: None,
            closed_by: "human-reviewer".to_string(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let restored: DraftStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn explanation_tiers_serialization() {
        let tiers = ExplanationTiers {
            summary: "Refactored auth middleware to use JWT".to_string(),
            explanation: "Replaced session-based auth with JWT validation.".to_string(),
            tags: vec!["security".to_string(), "breaking-change".to_string()],
            related_artifacts: vec![
                "fs://workspace/src/auth/config.rs".to_string(),
                "fs://workspace/tests/auth_test.rs".to_string(),
            ],
        };
        let json = serde_json::to_string(&tiers).unwrap();
        assert!(json.contains("\"summary\""));
        assert!(json.contains("\"explanation\""));
        assert!(json.contains("\"tags\""));
        assert!(json.contains("\"security\""));
        let restored: ExplanationTiers = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.summary, tiers.summary);
        assert_eq!(restored.tags.len(), 2);
        assert_eq!(restored.related_artifacts.len(), 2);
    }

    #[test]
    fn artifact_with_explanation_tiers_round_trip() {
        let artifact = Artifact {
            resource_uri: "fs://workspace/src/auth/middleware.rs".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:1".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Modernize auth".to_string()),
            dependencies: vec![],
            explanation_tiers: Some(ExplanationTiers {
                summary: "Refactored auth to JWT".to_string(),
                explanation: "Full JWT integration with validation.".to_string(),
                tags: vec!["security".to_string()],
                related_artifacts: vec![],
            }),
            comments: None,
            amendment: None,
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let restored: Artifact = serde_json::from_str(&json).unwrap();
        assert!(restored.explanation_tiers.is_some());
        assert_eq!(
            restored.explanation_tiers.as_ref().unwrap().summary,
            "Refactored auth to JWT"
        );
    }

    #[test]
    fn artifact_without_explanation_tiers_deserializes_correctly() {
        // Backward compatibility: old JSON without explanation_tiers.
        let json = r#"{
            "resource_uri": "fs://workspace/test.txt",
            "change_type": "add",
            "diff_ref": "changeset:0"
        }"#;
        let artifact: Artifact = serde_json::from_str(json).unwrap();
        assert!(artifact.explanation_tiers.is_none());
    }

    // ── v0.3.3 Decision Observability tests ──

    #[test]
    fn decision_log_entry_with_alternatives_considered() {
        let entry = DecisionLogEntry {
            decision: "Migrated to JWT auth".to_string(),
            rationale: "Session tokens don't scale".to_string(),
            alternatives: vec![],
            alternatives_considered: vec![
                AlternativeConsidered {
                    description: "Sticky sessions".to_string(),
                    rejected_reason: "Couples auth to infrastructure".to_string(),
                },
                AlternativeConsidered {
                    description: "Redis session store".to_string(),
                    rejected_reason: "Adds operational dependency".to_string(),
                },
            ],
        };

        let json = serde_json::to_string(&entry).unwrap();
        let restored: DecisionLogEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.alternatives_considered.len(), 2);
        assert_eq!(
            restored.alternatives_considered[0].description,
            "Sticky sessions"
        );
        assert_eq!(
            restored.alternatives_considered[1].rejected_reason,
            "Adds operational dependency"
        );
    }

    #[test]
    fn decision_log_entry_backward_compatible() {
        // Old JSON without alternatives_considered should deserialize fine.
        let json = r#"{
            "decision": "Used JWT",
            "rationale": "Scalability"
        }"#;
        let entry: DecisionLogEntry = serde_json::from_str(json).unwrap();
        assert!(entry.alternatives.is_empty());
        assert!(entry.alternatives_considered.is_empty());
    }

    #[test]
    fn policy_decision_record_with_trace_fields() {
        let record = PolicyDecisionRecord {
            rule_id: "default-deny".to_string(),
            effect: "allow".to_string(),
            notes: Some("Grant matched".to_string()),
            grants_checked: vec!["fs.read on workspace/**".to_string()],
            matching_grant: Some("fs.read on workspace/**".to_string()),
            evaluation_steps: vec![
                "path_traversal: passed".to_string(),
                "grant_match: allowed".to_string(),
            ],
        };

        let json = serde_json::to_string(&record).unwrap();
        let restored: PolicyDecisionRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.grants_checked.len(), 1);
        assert!(restored.matching_grant.is_some());
        assert_eq!(restored.evaluation_steps.len(), 2);
    }

    #[test]
    fn policy_decision_record_backward_compatible() {
        // Old JSON without v0.3.3 fields should deserialize fine.
        let json = r#"{
            "rule_id": "test",
            "effect": "deny",
            "notes": "No grant"
        }"#;
        let record: PolicyDecisionRecord = serde_json::from_str(json).unwrap();
        assert!(record.grants_checked.is_empty());
        assert!(record.matching_grant.is_none());
        assert!(record.evaluation_steps.is_empty());
    }

    // ── v0.3.4 Draft Amendment tests ──

    #[test]
    fn amendment_record_serialization() {
        let record = AmendmentRecord {
            amended_by: "human".to_string(),
            amended_at: Utc::now(),
            amendment_type: AmendmentType::FileReplaced,
            reason: Some("Fixed typo in struct name".to_string()),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"file_replaced\""));
        assert!(json.contains("\"human\""));
        let restored: AmendmentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.amendment_type, AmendmentType::FileReplaced);
        assert_eq!(
            restored.reason,
            Some("Fixed typo in struct name".to_string())
        );
    }

    #[test]
    fn amendment_type_all_variants() {
        assert_eq!(
            serde_json::to_string(&AmendmentType::FileReplaced).unwrap(),
            "\"file_replaced\""
        );
        assert_eq!(
            serde_json::to_string(&AmendmentType::PatchApplied).unwrap(),
            "\"patch_applied\""
        );
        assert_eq!(
            serde_json::to_string(&AmendmentType::Dropped).unwrap(),
            "\"dropped\""
        );
    }

    #[test]
    fn artifact_with_amendment_round_trip() {
        let artifact = Artifact {
            resource_uri: "fs://workspace/src/lib.rs".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Discuss,
            rationale: Some("Needs dedup".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: Some(AmendmentRecord {
                amended_by: "human".to_string(),
                amended_at: Utc::now(),
                amendment_type: AmendmentType::FileReplaced,
                reason: Some("Deduplicated struct".to_string()),
            }),
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let restored: Artifact = serde_json::from_str(&json).unwrap();
        assert!(restored.amendment.is_some());
        let amend = restored.amendment.unwrap();
        assert_eq!(amend.amended_by, "human");
        assert_eq!(amend.amendment_type, AmendmentType::FileReplaced);
    }

    #[test]
    fn artifact_without_amendment_backward_compatible() {
        // Old JSON without amendment field should deserialize fine.
        let json = r#"{
            "resource_uri": "fs://workspace/test.txt",
            "change_type": "add",
            "diff_ref": "changeset:0"
        }"#;
        let artifact: Artifact = serde_json::from_str(json).unwrap();
        assert!(artifact.amendment.is_none());
    }

    // ── v0.9.5 Design Alternatives tests ──

    #[test]
    fn design_alternative_serialization() {
        let alt = DesignAlternative {
            option: "Use HashMap for O(1) lookup".to_string(),
            rationale: "Best performance for frequent reads".to_string(),
            chosen: true,
        };
        let json = serde_json::to_string(&alt).unwrap();
        assert!(json.contains("\"option\""));
        assert!(json.contains("\"chosen\":true"));
        let restored: DesignAlternative = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, alt);
    }

    #[test]
    fn summary_with_alternatives_round_trip() {
        let summary = Summary {
            what_changed: "Refactored lookup".to_string(),
            why: "Performance".to_string(),
            impact: "None".to_string(),
            rollback_plan: "Revert".to_string(),
            open_questions: vec![],
            alternatives_considered: vec![
                DesignAlternative {
                    option: "HashMap".to_string(),
                    rationale: "O(1) lookup".to_string(),
                    chosen: true,
                },
                DesignAlternative {
                    option: "BTreeMap".to_string(),
                    rationale: "Ordered but O(log n)".to_string(),
                    chosen: false,
                },
            ],
        };
        let json = serde_json::to_string(&summary).unwrap();
        let restored: Summary = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.alternatives_considered.len(), 2);
        assert!(restored.alternatives_considered[0].chosen);
        assert!(!restored.alternatives_considered[1].chosen);
    }

    #[test]
    fn summary_without_alternatives_backward_compatible() {
        let json = r#"{
            "what_changed": "test",
            "why": "test",
            "impact": "none",
            "rollback_plan": "revert"
        }"#;
        let summary: Summary = serde_json::from_str(json).unwrap();
        assert!(summary.alternatives_considered.is_empty());
    }

    #[test]
    fn vcs_tracking_info_serialization_round_trip() {
        let vcs = VcsTrackingInfo {
            branch: "ta/fix-auth".to_string(),
            review_url: Some("https://github.com/org/repo/pull/42".to_string()),
            review_id: Some("42".to_string()),
            review_state: Some("open".to_string()),
            commit_sha: Some("abc1234".to_string()),
            last_checked: Utc::now(),
        };
        let json = serde_json::to_string(&vcs).unwrap();
        assert!(json.contains("\"branch\""));
        assert!(json.contains("\"review_url\""));
        let restored: VcsTrackingInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.branch, "ta/fix-auth");
        assert_eq!(restored.review_id, Some("42".to_string()));
    }

    #[test]
    fn draft_package_tag_backward_compat() {
        // JSON without tag/vcs_status should deserialize fine (backward compat).
        let pkg = test_package();
        assert!(pkg.tag.is_none());
        assert!(pkg.vcs_status.is_none());
        let json = serde_json::to_string(&pkg).unwrap();
        assert!(!json.contains("\"vcs_status\""));
        let restored: DraftPackage = serde_json::from_str(&json).unwrap();
        assert!(restored.tag.is_none());
        assert!(restored.vcs_status.is_none());
    }

    #[test]
    fn draft_package_with_tag_and_vcs() {
        let mut pkg = test_package();
        pkg.tag = Some("fix-auth-01".to_string());
        pkg.vcs_status = Some(VcsTrackingInfo {
            branch: "ta/fix-auth".to_string(),
            review_url: None,
            review_id: None,
            review_state: None,
            commit_sha: Some("def5678".to_string()),
            last_checked: Utc::now(),
        });
        let json = serde_json::to_string(&pkg).unwrap();
        assert!(json.contains("\"tag\""));
        assert!(json.contains("fix-auth-01"));
        assert!(json.contains("\"vcs_status\""));
        let restored: DraftPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tag, Some("fix-auth-01".to_string()));
        assert!(restored.vcs_status.is_some());
    }
}
