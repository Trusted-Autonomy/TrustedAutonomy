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

use crate::artifact_kind::ArtifactKind;

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
    /// Semantic kind of the artifact (v0.14.15). When present, the renderer
    /// uses kind-specific display logic (e.g. image artifacts suppress the
    /// binary diff and show a human-readable frame/resolution summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ArtifactKind>,
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

    /// Hard evidence that required checks passed/failed (v0.13.17).
    /// Each entry records the outcome of one required_check command.
    /// Non-zero exit_code blocks `ta draft approve` unless --override is passed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_log: Vec<ValidationEntry>,

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

    /// AI supervisor review embedded after agent exit (v0.13.17.4).
    /// Present when supervisor is enabled; `None` when disabled or skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_review: Option<crate::supervisor_review::SupervisorReview>,

    /// Gitignored artifacts encountered during apply --submit (v0.13.17.5).
    /// Populated by the VCS adapter when `git add` would fail on ignored paths.
    /// Known-safe artifacts (.mcp.json, *.local.toml, .ta/ runtime files) are
    /// recorded here but silently dropped from the commit.
    /// Unexpected-ignored artifacts are highlighted in `ta draft view`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_artifacts: Vec<IgnoredArtifact>,

    /// Artifact URIs inherited from the parent draft (v0.14.3.5).
    ///
    /// Populated when building a follow-up draft. Contains all `resource_uri` values
    /// from the parent draft's artifact list at build time. During `ta draft apply`,
    /// files in this list that are unchanged in staging (staging hash == source hash)
    /// are skipped — they were already settled by the parent apply and staging just
    /// has an older copy.
    ///
    /// This prevents "follow-up staging drift": applying a follow-up draft from
    /// staging that predates the parent commit would otherwise revert files
    /// (PLAN.md, USAGE.md, shared source) that the parent apply had already updated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_artifacts: Vec<String>,

    /// Agent-authored decision log (v0.14.7).
    ///
    /// Populated from `.ta-decisions.json` written by the agent during its run.
    /// Records non-obvious implementation choices with alternatives and rationale.
    /// Distinct from `plan.decision_log` which is extracted from `change_summary.json`.
    /// Shown as the "Agent Decision Log" section in `ta draft view`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_decision_log: Vec<DecisionLogEntry>,

    /// Goal shortref inherited from the parent GoalRun (v0.14.7.3).
    ///
    /// First 8 lowercase hex characters of `goal.goal_id`. Populated at `ta draft build`
    /// time. Used to display drafts as `<goal_shortref>/<draft_seq>` across all CLI
    /// surfaces (list, view, PR title, audit log). Allows `grep 2159d87e audit.jsonl`
    /// to find all entries for a goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_shortref: Option<String>,

    /// Sequence number for this draft within its goal (v0.14.7.3).
    ///
    /// First draft for a goal is 1, second is 2, etc. Combined with `goal_shortref`
    /// to produce the `<shortref>/<seq>` display identifier (e.g., `2159d87e/1`).
    /// Defaults to 0 for legacy drafts without this field.
    #[serde(default)]
    pub draft_seq: u32,

    /// Plan phase ID linked to this draft (v0.15.15.2).
    ///
    /// Populated from `GoalRun.plan_phase` at `ta draft build` time.
    /// Shown prominently in `ta draft view` (below the draft title) and as
    /// a column in `ta draft list`. Empty for goals not linked to a plan phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_phase: Option<String>,
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

/// A gitignored artifact encountered during `ta draft apply --submit` (v0.13.17.5).
///
/// Classified as either known-safe (silently dropped) or unexpected (requires attention).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IgnoredArtifact {
    /// Relative path of the artifact that was gitignored.
    pub path: String,
    /// Whether the path matched the known-safe drop list (e.g., .mcp.json).
    pub known_safe: bool,
}

/// Result of one `required_checks` entry run after agent exit (v0.13.17).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationEntry {
    /// The command that was run.
    pub command: String,
    /// Exit code (0 = success).
    pub exit_code: i32,
    /// How long the command took.
    pub duration_secs: u64,
    /// Last 20 lines of combined stdout+stderr output.
    pub stdout_tail: String,
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
    /// Optional agent confidence in this decision (0.0–1.0) (v0.14.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// What external need, feature, or constraint triggered this decision (v0.14.9.2).
    /// Shown as the header line in collapsed state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// A structured alternative considered during a decision (v0.3.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlternativeConsidered {
    pub description: String,
    pub rejected_reason: String,
}

/// How a draft was applied — provenance for [`DraftStatus::Applied`] (v0.15.14.0).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum ApplyProvenance {
    /// Triggered by an explicit `ta draft apply` CLI invocation.
    #[default]
    Manual,
    /// Triggered by a background or agent task.
    BackgroundTask { task_id: String },
    /// Triggered by an auto-merge hook.
    AutoMerge,
}

impl std::fmt::Display for ApplyProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyProvenance::Manual => write!(f, "manual"),
            ApplyProvenance::BackgroundTask { .. } => write!(f, "background"),
            ApplyProvenance::AutoMerge => write!(f, "auto-merge"),
        }
    }
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
        /// How the draft was applied (v0.15.14.0). Defaults to `Manual` for
        /// backward-compatible deserialization of older draft files.
        #[serde(default)]
        applied_via: ApplyProvenance,
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

/// Create a minimal valid [`DraftPackage`] for testing with the given goal
/// shortref and draft sequence number.
///
/// Only available in test builds. Used by `draft_resolver` unit tests and
/// any other test that needs a lightweight package fixture.
#[cfg(test)]
pub fn make_test_pkg(goal_shortref: &str, draft_seq: u32) -> DraftPackage {
    DraftPackage {
        package_version: "1.0.0".to_string(),
        package_id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        goal: Goal {
            goal_id: format!("{}-0000-0000-0000-000000000000", goal_shortref),
            title: format!("Test goal {}", goal_shortref),
            objective: "test".to_string(),
            success_criteria: vec![],
            constraints: vec![],
            parent_goal_title: None,
        },
        iteration: Iteration {
            iteration_id: "iter-1".to_string(),
            sequence: 1,
            workspace_ref: WorkspaceRef {
                ref_type: "staging_dir".to_string(),
                ref_name: "staging/test".to_string(),
                base_ref: None,
            },
        },
        agent_identity: AgentIdentity {
            agent_id: "test-agent".to_string(),
            agent_type: "test".to_string(),
            constitution_id: "default".to_string(),
            capability_manifest_hash: "abc".to_string(),
            orchestrator_run_id: None,
        },
        summary: Summary {
            what_changed: "test".to_string(),
            why: "test".to_string(),
            impact: "none".to_string(),
            rollback_plan: "none".to_string(),
            open_questions: vec![],
            alternatives_considered: vec![],
        },
        plan: Plan {
            completed_steps: vec![],
            next_steps: vec![],
            decision_log: vec![],
        },
        changes: Changes {
            artifacts: vec![],
            patch_sets: vec![],
            pending_actions: vec![],
        },
        risk: Risk {
            risk_score: 0,
            findings: vec![],
            policy_decisions: vec![],
        },
        provenance: Provenance {
            inputs: vec![],
            tool_trace_hash: "test".to_string(),
        },
        review_requests: ReviewRequests {
            requested_actions: vec![],
            reviewers: vec![],
            required_approvals: 1,
            notes_to_reviewer: None,
        },
        signatures: Signatures {
            package_hash: "test".to_string(),
            agent_signature: "test".to_string(),
            gateway_attestation: None,
        },
        status: DraftStatus::PendingReview,
        verification_warnings: vec![],
        validation_log: vec![],
        display_id: None,
        tag: None,
        vcs_status: None,
        parent_draft_id: None,
        pending_approvals: vec![],
        supervisor_review: None,
        ignored_artifacts: vec![],
        baseline_artifacts: vec![],
        agent_decision_log: vec![],
        goal_shortref: Some(goal_shortref.to_string()),
        draft_seq,
        plan_phase: None,
    }
}

/// Check whether a draft is missing an agent decision log for substantive changes.
///
/// "Substantive" = any artifact that is a `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`,
/// `.py`, `.go`, `.java`, `.cpp`, `.c`, or `.h` file. Config-only changes
/// (`.toml`, `.yaml`, `.json`, `.md`, docs) are excluded from this check.
///
/// Returns a warning annotation string when the check fires, or `None` if
/// a decision log is present or there are no substantive code changes.
///
/// This satisfies Constitution §1.5: reviewers must have design rationale for
/// any goal that creates or modifies non-trivial source code.
pub fn check_missing_decisions(pkg: &DraftPackage) -> Option<String> {
    // No warning if decision log is present.
    if !pkg.agent_decision_log.is_empty() {
        return None;
    }

    let substantive_extensions = [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "cpp", "c", "h",
    ];

    let has_substantive_code = pkg.changes.artifacts.iter().any(|a| {
        let uri = &a.resource_uri;
        // Extract file extension from URI like "fs://workspace/src/main.rs"
        if let Some(path_part) = uri.strip_prefix("fs://workspace/") {
            let ext = std::path::Path::new(path_part)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            substantive_extensions.contains(&ext)
        } else {
            false
        }
    });

    if has_substantive_code {
        Some(
            "No agent decision log entries found for a goal with significant code changes. \
             Consider `ta run --follow-up` to capture design rationale before approving."
                .to_string(),
        )
    } else {
        None
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
                    kind: None,
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
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
            goal_shortref: None,
            draft_seq: 0,
            plan_phase: None,
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
            applied_via: ApplyProvenance::Manual,
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
            kind: None,
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
            kind: None,
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
            confidence: None,
            context: None,
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
            kind: None,
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

    #[test]
    fn agent_decision_log_round_trip() {
        let mut pkg = test_package();
        pkg.agent_decision_log = vec![DecisionLogEntry {
            decision: "Used Ed25519 instead of RSA".to_string(),
            rationale: "Ed25519 is faster, smaller keys, already in Cargo.lock".to_string(),
            alternatives: vec!["RSA-2048".to_string(), "ECDSA P-256".to_string()],
            alternatives_considered: vec![],
            confidence: Some(0.9),
            context: None,
        }];
        let json = serde_json::to_string(&pkg).unwrap();
        assert!(json.contains("agent_decision_log"));
        assert!(json.contains("Ed25519"));
        assert!(json.contains("0.9"));
        let restored: DraftPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.agent_decision_log.len(), 1);
        assert_eq!(
            restored.agent_decision_log[0].decision,
            "Used Ed25519 instead of RSA"
        );
        assert_eq!(restored.agent_decision_log[0].confidence, Some(0.9));
        assert_eq!(restored.agent_decision_log[0].alternatives.len(), 2);
    }

    #[test]
    fn agent_decision_log_backward_compat() {
        // Packages without agent_decision_log should deserialize with empty vec.
        let pkg = test_package();
        let json = serde_json::to_string(&pkg).unwrap();
        assert!(!json.contains("agent_decision_log"));
        let restored: DraftPackage = serde_json::from_str(&json).unwrap();
        assert!(restored.agent_decision_log.is_empty());
    }

    #[test]
    fn decision_log_confidence_optional() {
        // DecisionLogEntry without confidence should deserialize fine.
        let entry_json = r#"{"decision":"test","rationale":"reason","alternatives":[]}"#;
        let entry: DecisionLogEntry = serde_json::from_str(entry_json).unwrap();
        assert_eq!(entry.decision, "test");
        assert!(entry.confidence.is_none());
    }

    #[test]
    fn decision_log_entry_with_context() {
        // Serialization round-trip with context field (v0.14.9.2).
        let entry = DecisionLogEntry {
            decision: "Use Ollama for local inference".to_string(),
            rationale: "Privacy and offline requirements".to_string(),
            alternatives: vec![],
            alternatives_considered: vec![],
            confidence: Some(0.8),
            context: Some("Ollama thinking-mode config".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("context"));
        assert!(json.contains("Ollama thinking-mode config"));
        let restored: DecisionLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.context.as_deref(),
            Some("Ollama thinking-mode config")
        );
        assert_eq!(restored.decision, "Use Ollama for local inference");
    }

    #[test]
    fn decision_log_entry_context_backward_compat() {
        // Old JSON without context should deserialize with context: None (v0.14.9.2).
        let json = r#"{"decision":"Used JWT","rationale":"Scalability"}"#;
        let entry: DecisionLogEntry = serde_json::from_str(json).unwrap();
        assert!(entry.context.is_none());
    }

    // ── check_missing_decisions (v0.15.15.1) ─────────────────────────────────

    fn make_artifact(uri: &str) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: None,
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: None,
        }
    }

    #[test]
    fn missing_decisions_fires_on_code_changes() {
        let mut pkg = test_package();
        // Add a Rust file artifact — substantive code change.
        pkg.changes
            .artifacts
            .push(make_artifact("fs://workspace/src/main.rs"));
        // No decision log entries.
        let warn = check_missing_decisions(&pkg);
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("decision log"));
    }

    #[test]
    fn missing_decisions_suppressed_when_decisions_present() {
        let mut pkg = test_package();
        pkg.changes
            .artifacts
            .push(make_artifact("fs://workspace/src/main.rs"));
        pkg.agent_decision_log.push(DecisionLogEntry {
            decision: "Used trait objects for extensibility".to_string(),
            rationale: "Allows plugin authors to add new adapters".to_string(),
            alternatives: vec!["enum dispatch".to_string()],
            alternatives_considered: vec![],
            confidence: Some(0.9),
            context: None,
        });
        let warn = check_missing_decisions(&pkg);
        assert!(warn.is_none());
    }

    #[test]
    fn missing_decisions_suppressed_for_trivial_changes() {
        let mut pkg = test_package();
        // Only toml + md files — no substantive code.
        pkg.changes
            .artifacts
            .push(make_artifact("fs://workspace/Cargo.toml"));
        pkg.changes
            .artifacts
            .push(make_artifact("fs://workspace/README.md"));
        let warn = check_missing_decisions(&pkg);
        assert!(warn.is_none());
    }

    #[test]
    fn missing_decisions_fires_for_typescript_and_python() {
        let mut pkg = test_package();
        pkg.changes
            .artifacts
            .push(make_artifact("fs://workspace/src/app.ts"));
        let warn = check_missing_decisions(&pkg);
        assert!(warn.is_some());

        let mut pkg2 = test_package();
        pkg2.changes
            .artifacts
            .push(make_artifact("fs://workspace/scripts/process.py"));
        let warn2 = check_missing_decisions(&pkg2);
        assert!(warn2.is_some());
    }

    #[test]
    fn missing_decisions_suppressed_when_no_artifacts() {
        let pkg = test_package();
        // No artifacts at all.
        let warn = check_missing_decisions(&pkg);
        assert!(warn.is_none());
    }
}
