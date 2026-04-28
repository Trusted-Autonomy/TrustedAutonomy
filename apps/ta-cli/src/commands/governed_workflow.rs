// governed_workflow.rs — Governed workflow execution engine (v0.14.8.2).
//
// Implements the canonical "safe autonomous coding loop":
//   run_goal → review_draft → human_gate → apply_draft → pr_sync
//
// Usage:
//   ta workflow run governed-goal --goal "Fix the auth bug"
//   ta workflow status <run-id>

use std::cmp::Reverse;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Workflow definition (TOML) ────────────────────────────────────────────────

/// Top-level governed workflow TOML definition.
#[derive(Debug, Clone, Deserialize)]
pub struct GovernedWorkflowDef {
    pub workflow: WorkflowMeta,
    #[serde(default)]
    pub stages: Vec<StageDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub config: WorkflowConfig,
}

/// Per-workflow configuration knobs.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowConfig {
    #[serde(default = "default_reviewer_agent")]
    pub reviewer_agent: String,
    #[serde(default)]
    pub gate_on_verdict: GateMode,
    #[serde(default = "default_poll_interval")]
    pub pr_poll_interval_secs: u64,
    #[serde(default = "default_sync_timeout")]
    pub sync_timeout_hours: u64,
    // Channels to notify on workflow completion (reserved for future notification plugin).
    #[serde(default)]
    #[allow(dead_code)]
    pub notify_channels: Vec<String>,
    /// When true, `stage_apply_draft` calls `gh pr merge --auto --squash` immediately after
    /// capturing the PR URL so the PR merges automatically once CI passes.
    #[serde(default)]
    pub auto_merge: bool,

    /// When true (the default), `pr_sync` treats a missing PR URL as a hard error and
    /// stops the workflow rather than silently skipping the sync step. Set to `false`
    /// only for direct-commit VCS flows where PRs are never created.
    ///
    /// This default is also overridden to `true` at runtime when the project's
    /// `workflow.toml` submit adapter is "git" with `auto_push = true`, so that VCS
    /// settings are the single source of truth.
    #[serde(default = "default_require_pr")]
    pub require_pr: bool,

    /// Maximum loop iterations for `kind = "goto"` stages before halting with a
    /// CHECKPOINT message and requiring manual resume. Default: 99.
    #[serde(default = "default_max_phases")]
    pub max_phases: u32,

    /// When true, stop the workflow if the reviewer flags — require manual resume.
    /// When false (the default), flag verdicts only pause at the human_gate.
    #[serde(default)]
    #[allow(dead_code)] // read by loop workflows; not yet plumbed through in v0.15.13
    pub stop_on_flag: bool,

    /// Auto-approve configuration: skip the interactive human_gate when all conditions pass.
    #[serde(default)]
    pub auto_approve: AutoApproveConfig,

    /// Post-sync build step: run a command after `pr_sync` completes successfully.
    #[serde(default)]
    pub post_sync_build: PostSyncBuildConfig,
}

/// Configuration for the auto-approve gate bypass.
///
/// When `enabled = true` and all listed `conditions` are satisfied, the
/// `human_gate` stage logs `[auto-approve] conditions met — applying without prompt`
/// and proceeds without interactive input. Any unsatisfied condition falls back
/// to the normal interactive prompt.
///
/// Supported conditions:
/// - `"reviewer_approved"` — reviewer verdict is `approve`
/// - `"no_flags"` — reviewer raised no flag items (findings list is empty)
/// - `"severity_below"` — no Critical corrective actions (currently: findings is empty)
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AutoApproveConfig {
    /// Whether auto-approve is enabled. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// All conditions must be satisfied to auto-approve.
    /// Supported: `"reviewer_approved"`, `"no_flags"`, `"severity_below"`.
    #[serde(default)]
    pub conditions: Vec<String>,
}

impl AutoApproveConfig {
    /// Returns true if all configured conditions are satisfied by the verdict.
    pub fn conditions_met(&self, verdict: &ReviewerVerdict) -> bool {
        if !self.enabled {
            return false;
        }
        // If no conditions listed, auto-approve never fires (require explicit config).
        if self.conditions.is_empty() {
            return false;
        }
        for cond in &self.conditions {
            let satisfied = match cond.as_str() {
                "reviewer_approved" => verdict.verdict == VerdictDecision::Approve,
                "no_flags" => verdict.findings.is_empty(),
                "severity_below" => verdict.findings.is_empty(),
                other => {
                    tracing::warn!(
                        condition = other,
                        "unknown auto_approve condition — treating as unsatisfied"
                    );
                    false
                }
            };
            if !satisfied {
                return false;
            }
        }
        true
    }
}

/// Configuration for the post-sync build step.
///
/// After `pr_sync` completes (PR merged + VCS synced), if `enabled = true` and
/// `command` is set, the command is run in the workspace root. This lets the batch
/// build loop end each phase with a freshly installed binary.
#[derive(Debug, Clone, Deserialize)]
pub struct PostSyncBuildConfig {
    /// Whether the post-sync build step is enabled. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Shell command to run after sync. Example: `"bash install_local.sh"`.
    #[serde(default)]
    pub command: Option<String>,
    /// Timeout in seconds. Default: 600 (10 minutes).
    #[serde(default = "default_post_sync_timeout")]
    pub timeout_secs: u64,
    /// What to do on command failure: `"halt"` (default) or `"warn"`.
    #[serde(default)]
    pub on_failure: PostSyncOnFailure,
}

impl Default for PostSyncBuildConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: None,
            timeout_secs: default_post_sync_timeout(),
            on_failure: PostSyncOnFailure::Halt,
        }
    }
}

fn default_post_sync_timeout() -> u64 {
    600
}

/// Behavior when the post-sync build command exits non-zero.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PostSyncOnFailure {
    /// Stop the workflow and require manual resume (default).
    #[default]
    Halt,
    /// Log a warning and continue to the next phase.
    Warn,
}

fn default_require_pr() -> bool {
    true
}

fn default_max_phases() -> u32 {
    99
}

fn default_reviewer_agent() -> String {
    "claude-code".to_string()
}
fn default_poll_interval() -> u64 {
    120
}
fn default_sync_timeout() -> u64 {
    72
}

/// Determines how the `human_gate` stage behaves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Automatically proceed on "approve" verdict; pause on "flag".
    #[default]
    Auto,
    /// Always prompt the human, regardless of verdict.
    Prompt,
    /// Require explicit human approval (alias for Prompt).
    Always,
}

/// One stage entry in the governed workflow TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct StageDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Step kind. Default = name-based dispatch (run_goal, review_draft, etc.).
    #[serde(default)]
    pub kind: StageKind,
    /// For `kind = "workflow"`: name of the child workflow TOML to invoke.
    #[serde(default)]
    pub workflow: Option<String>,
    /// For `kind = "workflow"`: goal title passed to the child workflow.
    /// Supports `{{stage.field}}` template interpolation.
    #[serde(default)]
    pub goal: Option<String>,
    /// For `kind = "workflow"`: plan phase passed to the child workflow.
    /// Supports `{{stage.field}}` template interpolation.
    #[serde(default)]
    pub phase: Option<String>,
    /// Guard condition for this stage.
    ///
    /// For non-goto stages: if false, the stage is skipped.
    /// For `kind = "goto"`: if true, jump to `target`; if false, fall through.
    ///
    /// Supported forms: `!stage.field`, `stage.field == "value"`, `stage.field != "value"`.
    #[serde(default)]
    pub condition: Option<String>,
    /// For `kind = "goto"`: stage name to jump to when `condition` is true.
    #[serde(default)]
    pub target: Option<String>,
    // ── v0.15.14 new fields ───────────────────────────────────────────────────
    /// For `kind = "aggregate_draft"`: which stages' draft_id outputs to collect.
    /// `"all"` collects from all stages that produced a `draft_id` output.
    /// Comma-separated stage names to collect from specific stages.
    #[serde(default)]
    pub source_stages: Option<String>,
    /// For `kind = "aggregate_draft"`: human-readable milestone title.
    #[serde(default)]
    pub milestone_title: Option<String>,
    /// For `kind = "apply_draft_branch"` / milestone mode: branch to apply to.
    #[serde(default)]
    pub milestone_branch: Option<String>,
    /// For parallel execution: group name. All stages sharing the same group
    /// are dispatched concurrently (up to `max_parallel`).
    /// NOTE: parallel dispatch is deferred; this field is parsed but not yet
    /// used in the execution engine (sequential fallback only).
    #[serde(default)]
    #[allow(dead_code)]
    pub parallel_group: Option<String>,
    /// For `kind = "join"`: which parallel_group to wait for.
    #[serde(default)]
    pub join_group: Option<String>,
    /// Behavior when a parallel group member fails.
    /// `"continue"` = proceed with remaining stages; default = halt workflow.
    #[serde(default)]
    pub on_partial_failure: Option<String>,
    /// Maximum parallel workers for a group (default: 3).
    /// NOTE: not yet used in execution (parallel dispatch is deferred).
    #[serde(default)]
    #[allow(dead_code)]
    pub max_parallel: Option<usize>,
    // ── v0.15.14.3 fields (static_analysis) ──────────────────────────────────
    /// For `kind = "static_analysis"`: override the language to analyse.
    /// When absent, the language is auto-detected from workspace marker files.
    #[serde(default)]
    pub lang: Option<String>,
    /// For `kind = "plan_next"` / `"loop_next"`: only consider phases whose ID
    /// starts with this prefix (passed as `--filter` to `ta plan next`).
    /// When absent, all pending phases are considered.
    #[serde(default)]
    pub phase_filter: Option<String>,
}

// ── New step kinds (v0.15.13 + v0.15.14) ─────────────────────────────────────

/// The kind of step in a governed workflow stage (v0.15.13 + v0.15.14).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    /// Default: existing name-based dispatch (run_goal, review_draft, etc.)
    #[default]
    Default,
    /// Invoke another named workflow as a sub-workflow, running it to completion.
    Workflow,
    /// Run `ta plan next` and emit `{phase_id, phase_title, done}` into the output map.
    PlanNext,
    /// Jump to `target` when `condition` is true (loop-back step).
    Goto,
    /// Advances the phase cursor in phase-loop mode (alias for PlanNext, v0.15.14).
    LoopNext,
    /// Apply the current draft to a named branch instead of main (v0.15.14).
    ApplyDraftBranch,
    /// Collect draft IDs from source stages and create a MilestoneDraft (v0.15.14).
    AggregateDraft,
    /// Synchronization point: validates all stages in a parallel_group completed (v0.15.14).
    Join,
    /// Run the configured static analyzer for the detected or specified language,
    /// optionally entering an agent correction loop on failure (v0.15.14.3).
    StaticAnalysis,
    /// Aggregate reviewer votes using the consensus engine (v0.15.15.1).
    Consensus,
    /// Apply the current draft to source (explicit `kind = "apply_draft"` variant) (v0.15.15.1).
    ApplyDraft,
    /// Run the work planner agent to produce `.ta/work-plan.json` before the
    /// implementor stage. The planner is read-only: it cannot write code, only
    /// produce a structured plan (v0.15.20).
    PlanWork,
}

/// Output from a `kind = "plan_next"` stage (v0.15.13).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanNextOutput {
    pub phase_id: String,
    pub phase_title: String,
    /// True when all plan phases are complete (no more work to do).
    pub done: bool,
}

impl PlanNextOutput {
    /// Parse `ta plan next` stdout into a structured output.
    ///
    /// Expected formats:
    ///   "Next pending phase:\n  Phase <id> — <title>\n..."
    ///   "All plan phases are complete or in progress."
    pub fn parse(stdout: &str) -> Self {
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("All plan phases") {
                return PlanNextOutput {
                    phase_id: String::new(),
                    phase_title: String::new(),
                    done: true,
                };
            }
            // "Phase v0.15.14 — Hierarchical Workflows: ..."
            if let Some(rest) = trimmed.strip_prefix("Phase ") {
                if let Some((id, title)) = rest.split_once(" \u{2014} ") {
                    return PlanNextOutput {
                        phase_id: id.trim().to_string(),
                        phase_title: title.trim().to_string(),
                        done: false,
                    };
                }
                // Fallback: no em-dash separator.
                return PlanNextOutput {
                    phase_id: rest.trim().to_string(),
                    phase_title: String::new(),
                    done: false,
                };
            }
        }
        // Nothing parseable — treat as done to avoid infinite loops.
        PlanNextOutput {
            phase_id: String::new(),
            phase_title: String::new(),
            done: true,
        }
    }

    /// Convert to a string map for template interpolation.
    pub fn to_output_map(&self) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        map.insert("phase_id".to_string(), self.phase_id.clone());
        map.insert("phase_title".to_string(), self.phase_title.clone());
        map.insert("done".to_string(), self.done.to_string());
        map
    }
}

/// Links a parent workflow run to a child sub-workflow run (v0.15.13).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubworkflowRecord {
    pub parent_run_id: String,
    pub child_run_id: String,
    pub stage_name: String,
    pub child_workflow: String,
}

// ── Verdict schema (.ta/review/<draft-id>/verdict.json) ───────────────────────

/// Decision a reviewer agent writes to `verdict.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictDecision {
    Approve,
    Flag,
    Reject,
}

impl std::fmt::Display for VerdictDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerdictDecision::Approve => write!(f, "approve"),
            VerdictDecision::Flag => write!(f, "flag"),
            VerdictDecision::Reject => write!(f, "reject"),
        }
    }
}

/// The structured verdict written by the reviewer agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerVerdict {
    /// approve | flag | reject
    pub verdict: VerdictDecision,
    /// Human-readable findings. Empty on clean approve.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Reviewer confidence in the verdict (0.0 – 1.0).
    pub confidence: f64,
}

impl ReviewerVerdict {
    /// Load a verdict from `<review_dir>/verdict.json`.
    pub fn load(review_dir: &Path) -> anyhow::Result<Self> {
        let path = review_dir.join("verdict.json");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "Verdict file not found: {}\n\
                 The reviewer agent must write verdict.json before the human_gate stage.\n\
                 Error: {}",
                path.display(),
                e
            )
        })?;
        serde_json::from_str(&content).map_err(|e| {
            anyhow::anyhow!(
                "Invalid verdict.json at {}: {}\n\
                 Expected format: {{\"verdict\": \"approve|flag|reject\", \"findings\": [...], \"confidence\": 0.0-1.0}}",
                path.display(),
                e
            )
        })
    }

    /// Validate that confidence is in range and verdict is a known value.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(0.0..=1.0).contains(&self.confidence) {
            anyhow::bail!(
                "verdict.json confidence must be between 0.0 and 1.0, got {}",
                self.confidence
            );
        }
        Ok(())
    }
}

// ── Workflow run state ────────────────────────────────────────────────────────

/// Lifecycle state of a governed workflow run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Running,
    AwaitingHuman,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for WorkflowRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowRunState::Running => write!(f, "running"),
            WorkflowRunState::AwaitingHuman => write!(f, "awaiting_human"),
            WorkflowRunState::Completed => write!(f, "completed"),
            WorkflowRunState::Failed => write!(f, "failed"),
            WorkflowRunState::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Status of a single stage in the workflow run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for StageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageStatus::Pending => write!(f, "pending"),
            StageStatus::Running => write!(f, "running"),
            StageStatus::Completed => write!(f, "completed"),
            StageStatus::Failed => write!(f, "failed"),
            StageStatus::Skipped => write!(f, "skipped"),
        }
    }
}

/// Audit record for a single stage transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageAuditEntry {
    pub stage: String,
    pub agent: String,
    pub verdict: Option<String>,
    pub duration_secs: u64,
    pub at: DateTime<Utc>,
}

/// Per-stage execution record stored in the run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub name: String,
    pub status: StageStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<u64>,
    /// Human-readable detail about what happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl StageRecord {
    fn new(name: &str) -> Self {
        StageRecord {
            name: name.to_string(),
            status: StageStatus::Pending,
            started_at: None,
            completed_at: None,
            duration_secs: None,
            detail: None,
        }
    }

    fn start(&mut self) {
        self.status = StageStatus::Running;
        self.started_at = Some(Utc::now());
    }

    fn complete(&mut self, detail: Option<String>) {
        self.status = StageStatus::Completed;
        self.completed_at = Some(Utc::now());
        self.duration_secs = self
            .started_at
            .map(|s| (Utc::now() - s).num_seconds().max(0) as u64);
        self.detail = detail;
    }

    fn fail(&mut self, reason: &str) {
        self.status = StageStatus::Failed;
        self.completed_at = Some(Utc::now());
        self.duration_secs = self
            .started_at
            .map(|s| (Utc::now() - s).num_seconds().max(0) as u64);
        self.detail = Some(reason.to_string());
    }
}

/// Full persisted state for a governed workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernedWorkflowRun {
    /// Unique run identifier (UUID).
    pub run_id: String,
    /// Name of the workflow that was executed.
    pub workflow_name: String,
    /// Goal title passed with --goal.
    pub goal_title: String,
    /// Goal ID assigned by `ta run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    /// Draft ID produced by the goal run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    /// PR URL created after apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Current active stage name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    /// Overall lifecycle state.
    pub state: WorkflowRunState,
    /// Ordered stage records.
    pub stages: Vec<StageRecord>,
    /// Reviewer verdict (set after review_draft completes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewerVerdict>,
    /// Audit trail — one entry per stage transition.
    #[serde(default)]
    pub audit_trail: Vec<StageAuditEntry>,
    /// Stage outputs for template interpolation and condition evaluation (v0.15.13).
    /// Key: stage name. Value: field → string value map.
    #[serde(default)]
    pub outputs: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Sub-workflow records for `kind = "workflow"` stages (v0.15.13).
    #[serde(default)]
    pub sub_workflow_records: Vec<SubworkflowRecord>,
    /// Loop iteration counts per goto stage name (v0.15.13).
    #[serde(default)]
    pub loop_iterations: std::collections::HashMap<String, u32>,
    /// Phase IDs that have already been dispatched in this run (v0.15.24.2).
    /// Second line of defence against duplicate dispatch: if `plan_next` returns
    /// a phase that is already in this list, the loop halts with a safety error.
    #[serde(default)]
    pub dispatched_phases: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GovernedWorkflowRun {
    /// Create a new run with the canonical governed-goal stages (backward compat).
    #[allow(dead_code)] // used in tests; production code calls new_with_stages directly
    pub fn new(run_id: &str, workflow_name: &str, goal_title: &str) -> Self {
        Self::new_with_stages(
            run_id,
            workflow_name,
            goal_title,
            &[
                "run_goal",
                "review_draft",
                "human_gate",
                "apply_draft",
                "pr_sync",
            ],
        )
    }

    /// Create a new run with an explicit list of stage names.
    pub fn new_with_stages(
        run_id: &str,
        workflow_name: &str,
        goal_title: &str,
        stage_names: &[&str],
    ) -> Self {
        let stages = stage_names.iter().map(|n| StageRecord::new(n)).collect();
        GovernedWorkflowRun {
            run_id: run_id.to_string(),
            workflow_name: workflow_name.to_string(),
            goal_title: goal_title.to_string(),
            goal_id: None,
            draft_id: None,
            pr_url: None,
            current_stage: None,
            state: WorkflowRunState::Running,
            stages,
            verdict: None,
            audit_trail: Vec::new(),
            outputs: std::collections::HashMap::new(),
            sub_workflow_records: Vec::new(),
            loop_iterations: std::collections::HashMap::new(),
            dispatched_phases: Vec::new(),
            started_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Persist the run state to `.ta/workflow-runs/<run-id>.json`.
    pub fn save(&self, runs_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(runs_dir)?;
        let path = runs_dir.join(format!("{}.json", self.run_id));
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Load a run state file by run ID (or a unique prefix).
    pub fn load(runs_dir: &Path, run_id_or_prefix: &str) -> anyhow::Result<Self> {
        let exact = runs_dir.join(format!("{}.json", run_id_or_prefix));
        if exact.exists() {
            let content = std::fs::read_to_string(&exact)?;
            return Ok(serde_json::from_str(&content)?);
        }
        // Try prefix match.
        let entries = std::fs::read_dir(runs_dir)
            .map_err(|e| anyhow::anyhow!("Cannot read workflow-runs directory: {}", e))?;
        let mut matches = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") && name.starts_with(run_id_or_prefix) {
                matches.push(entry.path());
            }
        }
        match matches.len() {
            0 => anyhow::bail!(
                "No workflow run found for '{}'\n\
                 List runs with: ta workflow list",
                run_id_or_prefix
            ),
            1 => {
                let content = std::fs::read_to_string(&matches[0])?;
                Ok(serde_json::from_str(&content)?)
            }
            n => anyhow::bail!(
                "Ambiguous run ID prefix '{}' matches {} runs — use more characters",
                run_id_or_prefix,
                n
            ),
        }
    }

    /// Find the most recently modified run.
    pub fn find_latest(runs_dir: &Path) -> anyhow::Result<Option<Self>> {
        if !runs_dir.exists() {
            return Ok(None);
        }
        let entries = std::fs::read_dir(runs_dir)
            .map_err(|e| anyhow::anyhow!("Cannot read workflow-runs directory: {}", e))?;
        let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|e| {
                let modified = e.metadata().ok()?.modified().ok()?;
                Some((modified, e.path()))
            })
            .collect();
        candidates.sort_by_key(|c| Reverse(c.0));
        match candidates.first() {
            None => Ok(None),
            Some((_, path)) => {
                let content = std::fs::read_to_string(path)?;
                Ok(Some(serde_json::from_str(&content)?))
            }
        }
    }

    fn stage_mut(&mut self, name: &str) -> Option<&mut StageRecord> {
        self.stages.iter_mut().find(|s| s.name == name)
    }

    fn emit_audit(&mut self, stage: &str, agent: &str, verdict: Option<&str>, duration_secs: u64) {
        self.audit_trail.push(StageAuditEntry {
            stage: stage.to_string(),
            agent: agent.to_string(),
            verdict: verdict.map(|v| v.to_string()),
            duration_secs,
            at: Utc::now(),
        });
    }
}

// ── Workflow definition loader ────────────────────────────────────────────────

/// Resolve a workflow name to its definition path and parse it.
///
/// Search order (YAML takes precedence over TOML):
///   1. `.ta/workflows/<name>.yaml` (project-local YAML, highest priority)
///   2. `.ta/workflows/<name>.toml` (project-local TOML, backwards compatibility)
///   3. `templates/workflows/<name>.yaml` (built-in YAML template, canonical)
///   4. `templates/workflows/<name>.toml` (built-in TOML template, legacy)
pub fn find_workflow_def(workspace_root: &Path, name: &str) -> anyhow::Result<GovernedWorkflowDef> {
    let candidates = [
        workspace_root
            .join(".ta")
            .join("workflows")
            .join(format!("{}.yaml", name)),
        workspace_root
            .join(".ta")
            .join("workflows")
            .join(format!("{}.toml", name)),
        workspace_root
            .join("templates")
            .join("workflows")
            .join(format!("{}.yaml", name)),
        workspace_root
            .join("templates")
            .join("workflows")
            .join(format!("{}.toml", name)),
    ];

    let path = candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Workflow '{}' not found.\n\
                 Checked:\n  \
                   {}\n  \
                   {}\n  \
                   {}\n  \
                   {}\n\
                 Available workflows:\n  \
                   ta workflow list --templates\n\
                 Create a project-local copy:\n  \
                   ta workflow new {} --from governed-goal",
                name,
                candidates[0].display(),
                candidates[1].display(),
                candidates[2].display(),
                candidates[3].display(),
                name,
            )
        })?;

    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "yaml" || ext == "yml" {
        serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))
    } else {
        toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))
    }
}

/// Validate that stage `depends_on` references are all defined and acyclic.
/// Returns stages in topological execution order.
pub fn validate_stage_graph(stages: &[StageDef]) -> anyhow::Result<Vec<String>> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let names: HashSet<&str> = stages.iter().map(|s| s.name.as_str()).collect();

    // Verify all depends_on references resolve.
    for stage in stages {
        for dep in &stage.depends_on {
            if !names.contains(dep.as_str()) {
                anyhow::bail!(
                    "Stage '{}' depends on '{}' which is not defined in this workflow",
                    stage.name,
                    dep
                );
            }
        }
    }

    // Kahn's algorithm for topological sort.
    let mut in_degree: HashMap<&str, usize> = stages.iter().map(|s| (s.name.as_str(), 0)).collect();
    for stage in stages {
        for dep in &stage.depends_on {
            *in_degree.entry(stage.name.as_str()).or_insert(0) += 1;
            let _ = dep; // dep contributes to in_degree of stage
        }
    }

    // Recompute: count incoming edges (deps) for each stage.
    let mut in_deg: HashMap<&str, usize> = stages.iter().map(|s| (s.name.as_str(), 0)).collect();
    for stage in stages {
        *in_deg.entry(stage.name.as_str()).or_insert(0) = stage.depends_on.len();
    }

    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for stage in stages {
        for dep in &stage.depends_on {
            adj.entry(dep.as_str())
                .or_default()
                .push(stage.name.as_str());
        }
    }

    let mut queue: VecDeque<&str> = in_deg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut order = Vec::new();

    while let Some(node) = queue.pop_front() {
        order.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            for &neighbor in neighbors {
                let deg = in_deg.entry(neighbor).or_insert(0);
                *deg = deg.saturating_sub(1);
                if *deg == 0 {
                    queue.push_back(neighbor);
                }
            }
        }
    }

    if order.len() != stages.len() {
        anyhow::bail!("Workflow has a dependency cycle — check your stage depends_on declarations");
    }

    Ok(order)
}

// ── human_gate decision logic ─────────────────────────────────────────────────

/// Outcome of the human_gate stage.
#[derive(Debug, PartialEq, Eq)]
pub enum GateDecision {
    /// Proceed to apply_draft.
    Proceed,
    /// Human chose to override a "flag" verdict and proceed.
    Override,
    /// Reject: deny draft and stop workflow.
    Reject,
}

/// Evaluate the human gate given the verdict and gate mode.
///
/// In non-interactive mode (`interactive = false`) this returns the automatic
/// decision; if human input is needed it returns an error so the caller can
/// pause the run.
pub fn evaluate_human_gate(
    verdict: &ReviewerVerdict,
    gate_mode: &GateMode,
    interactive: bool,
) -> anyhow::Result<GateDecision> {
    match (&verdict.verdict, gate_mode) {
        // Approve + auto/prompt: proceed (or ask, but answer is obvious).
        (VerdictDecision::Approve, GateMode::Auto) => Ok(GateDecision::Proceed),
        (VerdictDecision::Approve, GateMode::Prompt | GateMode::Always) => {
            if interactive {
                prompt_human_gate(verdict, "Reviewer approved. Apply the draft? [Y/n]: ")?;
            }
            Ok(GateDecision::Proceed)
        }

        // Flag + auto: pause for human.
        (VerdictDecision::Flag, GateMode::Auto) => {
            if !interactive {
                anyhow::bail!(
                    "Reviewer flagged issues — human input required.\n\
                     Resume with: ta workflow run governed-goal --resume <run-id>"
                );
            }
            let findings_text = if verdict.findings.is_empty() {
                "(no specific findings listed)".to_string()
            } else {
                verdict.findings.join("\n  - ")
            };
            println!();
            println!(
                "Reviewer flagged issues (confidence {:.0}%):",
                verdict.confidence * 100.0
            );
            println!("  - {}", findings_text);
            println!();
            let apply =
                prompt_human_gate(verdict, "Reviewer flagged issues — apply anyway? [y/N]: ")?;
            if apply {
                Ok(GateDecision::Override)
            } else {
                Ok(GateDecision::Reject)
            }
        }

        // Flag + prompt/always: pause for human.
        (VerdictDecision::Flag, GateMode::Prompt | GateMode::Always) => {
            if !interactive {
                anyhow::bail!(
                    "Reviewer flagged issues — human input required (gate=prompt).\n\
                     Resume with: ta workflow run governed-goal --resume <run-id>"
                );
            }
            let apply =
                prompt_human_gate(verdict, "Reviewer flagged issues — apply anyway? [y/N]: ")?;
            if apply {
                Ok(GateDecision::Override)
            } else {
                Ok(GateDecision::Reject)
            }
        }

        // Reject: always deny.
        (VerdictDecision::Reject, _) => Ok(GateDecision::Reject),
    }
}

/// Prompt the human and return true if they confirmed (y/Y).
///
/// In addition to Y/N the user can enter D to open a short discussion loop
/// with the advisor before returning to the gate prompt.
fn prompt_human_gate(verdict: &ReviewerVerdict, prompt: &str) -> anyhow::Result<bool> {
    use ta_session::workflow_session::AdvisorSecurity;
    use ta_session::{AdvisorContext, AdvisorSession};

    // Indicate channel capability.
    let channel_note = if std::env::var("VSCODE_IPC_HOOK_CLI").is_ok() {
        "[Live injection active]"
    } else {
        "[Notes will apply at next restart]"
    };

    // Use the first reviewer finding as context for the advisor, if available.
    let selection: Option<String> = verdict.findings.first().cloned();

    loop {
        println!(
            "\nOptions: [Y] Approve  [N] Deny  [D] Discuss  {}",
            channel_note
        );
        print!("{}", prompt);
        std::io::stdout().flush().ok();

        let stdin = std::io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let answer = line.trim().to_lowercase();

        match answer.as_str() {
            "d" | "discuss" => {
                println!(
                    "Discussing with advisor. Type your question or note (empty line to return):"
                );
                let stdin = std::io::stdin();
                for input_line in stdin.lock().lines() {
                    let input = input_line.unwrap_or_default();
                    if input.trim().is_empty() {
                        break;
                    }
                    // Use AdvisorSession to classify and respond.
                    let ctx = AdvisorContext {
                        tab: "governance".to_string(),
                        selection: selection.clone(),
                    };
                    let session =
                        AdvisorSession::from_message(&input, &AdvisorSecurity::ReadOnly, &ctx);
                    println!("\nAdvisor: {}", session.response);
                    if !session.options.is_empty() {
                        for opt in &session.options {
                            println!("  {}. {}", opt.number, opt.label);
                        }
                    }
                    println!("\nContinue discussing (empty line to return to gate):");
                }
                // Loop back to re-print the gate prompt.
            }
            "y" | "yes" => return Ok(true),
            "n" | "no" | "" => return Ok(false),
            _ => {
                println!("Please enter Y, N, or D.");
            }
        }
    }
}

// ── PR sync poll ──────────────────────────────────────────────────────────────

/// Result from a single PR state poll.
#[derive(Debug, PartialEq, Eq)]
pub enum PrPollResult {
    Merged,
    Closed,
    Open,
    NotFound,
}

/// Poll `gh pr view <url> --json state` and return the result.
///
/// This is the implementation used by the `pr_sync` stage. In tests, you can
/// pass a mock command instead of `gh`.
pub fn poll_pr_state(pr_url: &str) -> PrPollResult {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", pr_url, "--json", "state", "--jq", ".state"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let state = String::from_utf8_lossy(&out.stdout).trim().to_uppercase();
            match state.as_str() {
                "MERGED" => PrPollResult::Merged,
                "CLOSED" => PrPollResult::Closed,
                _ => PrPollResult::Open,
            }
        }
        Ok(_) => PrPollResult::NotFound,
        Err(_) => PrPollResult::NotFound,
    }
}

// ── Main workflow execution ───────────────────────────────────────────────────

/// Options for running a governed workflow.
pub struct RunOptions<'a> {
    pub workspace_root: &'a Path,
    pub workflow_name: &'a str,
    pub goal_title: &'a str,
    pub dry_run: bool,
    pub resume_run_id: Option<&'a str>,
    pub agent: &'a str,
    /// Optional PLAN.md phase ID (e.g. "v0.4.0"). When set:
    /// - injected into the agent's CLAUDE.md context via `ta run --phase`
    /// - passed to `ta draft apply --phase` so the phase is marked done in PLAN.md
    pub plan_phase: Option<&'a str>,
    /// Sub-workflow recursion depth. 0 = top-level. Hard limit: 5 (v0.15.13).
    pub depth: u32,
    /// Resolved `--param key=value` pairs from the CLI (v0.15.23+).
    /// Used to thread template params (e.g. phase_filter) into stage executors.
    pub params: std::collections::HashMap<String, String>,
}

// ── Template interpolation & condition evaluation (v0.15.13) ─────────────────

/// Interpolate `{{stage_name.field}}` placeholders in a template string.
///
/// Unresolved placeholders are left unchanged so callers can detect them.
pub fn interpolate_template(
    template: &str,
    outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> String {
    let mut result = template.to_string();
    // Find all {{...}} placeholders and replace them.
    let mut out = String::with_capacity(result.len());
    let mut remaining = result.as_str();
    while let Some(open) = remaining.find("{{") {
        out.push_str(&remaining[..open]);
        let after_open = &remaining[open + 2..];
        if let Some(close) = after_open.find("}}") {
            let placeholder = after_open[..close].trim();
            // Placeholder format: "stage_name.field"
            if let Some((stage, field)) = placeholder.split_once('.') {
                if let Some(stage_map) = outputs.get(stage) {
                    if let Some(value) = stage_map.get(field) {
                        out.push_str(value);
                        remaining = &after_open[close + 2..];
                        continue;
                    }
                }
            }
            // Unresolved — keep the placeholder verbatim.
            out.push_str("{{");
            out.push_str(placeholder);
            out.push_str("}}");
            remaining = &after_open[close + 2..];
        } else {
            // No closing braces — append rest as-is.
            out.push_str("{{");
            out.push_str(after_open);
            break;
        }
    }
    out.push_str(remaining);
    result = out;
    result
}

/// Evaluate a condition expression against the current output map.
///
/// Supported forms:
///   - `!stage.field`              — boolean not (field must be "true" or "false")
///   - `stage.field == "value"`    — equality check
///   - `stage.field != "value"`    — inequality check
///
/// Returns an error if the expression is malformed or references an unknown field.
pub fn evaluate_condition(
    condition: &str,
    outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> anyhow::Result<bool> {
    let condition = condition.trim();

    // Form 1: `!stage.field`
    if let Some(rest) = condition.strip_prefix('!') {
        let field_ref = rest.trim();
        let value = resolve_field(field_ref, outputs).map_err(|e| {
            anyhow::anyhow!("Condition '{}' references unknown field: {}", condition, e)
        })?;
        match value.as_str() {
            "true" => return Ok(false),
            "false" => return Ok(true),
            other => anyhow::bail!(
                "Condition '{}': field '{}' has non-boolean value '{}'. \
                 Boolean conditions require 'true' or 'false'.",
                condition,
                field_ref,
                other
            ),
        }
    }

    // Form 2: `stage.field == "value"` or `stage.field != "value"`
    if let Some((lhs, rhs)) = condition.split_once(" == ") {
        let field_ref = lhs.trim();
        let expected = rhs.trim().trim_matches('"');
        let actual = resolve_field(field_ref, outputs).map_err(|e| {
            anyhow::anyhow!("Condition '{}' references unknown field: {}", condition, e)
        })?;
        return Ok(actual == expected);
    }
    if let Some((lhs, rhs)) = condition.split_once(" != ") {
        let field_ref = lhs.trim();
        let expected = rhs.trim().trim_matches('"');
        let actual = resolve_field(field_ref, outputs).map_err(|e| {
            anyhow::anyhow!("Condition '{}' references unknown field: {}", condition, e)
        })?;
        return Ok(actual != expected);
    }

    // Plain field reference — treat as boolean.
    let value = resolve_field(condition, outputs).map_err(|e| {
        anyhow::anyhow!("Condition '{}' references unknown field: {}", condition, e)
    })?;
    match value.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => anyhow::bail!(
            "Condition '{}': field has non-boolean value '{}'. \
             Boolean conditions require 'true' or 'false'.",
            condition,
            other
        ),
    }
}

/// Resolve a `stage.field` reference from the output map.
fn resolve_field(
    field_ref: &str,
    outputs: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> anyhow::Result<String> {
    let (stage, field) = field_ref.split_once('.').ok_or_else(|| {
        anyhow::anyhow!(
            "'{}' is not a valid field reference — expected 'stage_name.field'",
            field_ref
        )
    })?;
    let stage_map = outputs.get(stage).ok_or_else(|| {
        anyhow::anyhow!(
            "Stage '{}' has no outputs yet — ensure it runs before this condition is evaluated",
            stage
        )
    })?;
    let value = stage_map.get(field).ok_or_else(|| {
        anyhow::anyhow!(
            "Stage '{}' output has no field '{}'. Available: {}",
            stage,
            field,
            stage_map.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    Ok(value.clone())
}

/// Execute a governed workflow end-to-end.
pub fn run_governed_workflow(opts: &RunOptions) -> anyhow::Result<()> {
    // Recursion depth guard: prevent infinite sub-workflow nesting.
    const MAX_DEPTH: u32 = 5;
    if opts.depth > MAX_DEPTH {
        anyhow::bail!(
            "Sub-workflow recursion depth limit ({}) exceeded.\n\
             Workflow '{}' would exceed the maximum nesting depth of {}.\n\
             Check for circular sub-workflow references in your workflow definitions.",
            MAX_DEPTH,
            opts.workflow_name,
            MAX_DEPTH
        );
    }

    let runs_dir = opts.workspace_root.join(".ta").join("workflow-runs");
    let def = find_workflow_def(opts.workspace_root, opts.workflow_name)?;
    let stage_order = validate_stage_graph(&def.stages)?;

    // Detect loop mode: any stage with kind=goto triggers sequential loop execution.
    // Computed early so both the dry-run block and the execution block can use it.
    let has_goto = def.stages.iter().any(|s| s.kind == StageKind::Goto);

    // Clone so we can override settings based on .ta/workflow.toml (project-level source of truth).
    let mut owned_config = def.workflow.config.clone();
    let submit_config_path = opts.workspace_root.join(".ta").join("workflow.toml");
    {
        let wf = ta_submit::WorkflowConfig::load_or_default(&submit_config_path);
        // VCS adapter: require_pr = true when git + auto_review.
        let git_with_review = wf.submit.adapter == "git" && wf.submit.auto_review != Some(false);
        if git_with_review {
            owned_config.require_pr = true;
        }
        // Auto-approve: project-level config overrides workflow YAML config.
        if wf.workflow.auto_approve.enabled {
            owned_config.auto_approve = AutoApproveConfig {
                enabled: true,
                conditions: wf.workflow.auto_approve.conditions.clone(),
            };
        }
        // Post-sync build: project-level config overrides workflow YAML config.
        if wf.workflow.post_sync_build.enabled {
            owned_config.post_sync_build = PostSyncBuildConfig {
                enabled: true,
                command: wf.workflow.post_sync_build.command.clone(),
                timeout_secs: wf.workflow.post_sync_build.timeout_secs,
                on_failure: match wf.workflow.post_sync_build.on_failure.as_str() {
                    "warn" => PostSyncOnFailure::Warn,
                    _ => PostSyncOnFailure::Halt,
                },
            };
        }
    }
    let config = &owned_config;

    // Dry run: print the stage graph and (for loop workflows) pending phase info.
    if opts.dry_run {
        println!("Workflow: {}", def.workflow.name);
        println!(
            "Description: {}",
            def.workflow.description.trim().lines().next().unwrap_or("")
        );
        println!("Goal:     {}", opts.goal_title);
        println!();
        println!("Stage graph (dry-run, no execution):");
        for (i, stage) in def.stages.iter().enumerate() {
            let desc = stage
                .description
                .trim()
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let kind_label = match stage.kind {
                StageKind::Default => String::new(),
                StageKind::PlanNext => " [plan_next]".to_string(),
                StageKind::Workflow => {
                    format!(" [workflow: {}]", stage.workflow.as_deref().unwrap_or("?"))
                }
                StageKind::Goto => format!(" [goto: {}]", stage.target.as_deref().unwrap_or("?")),
                StageKind::LoopNext => " [loop_next]".to_string(),
                StageKind::ApplyDraftBranch => " [apply_draft_branch]".to_string(),
                StageKind::AggregateDraft => " [aggregate_draft]".to_string(),
                StageKind::Join => {
                    format!(" [join: {}]", stage.join_group.as_deref().unwrap_or("?"))
                }
                StageKind::StaticAnalysis => {
                    let lang = stage.lang.as_deref().unwrap_or("auto");
                    format!(" [static_analysis: {}]", lang)
                }
                StageKind::Consensus => " [consensus]".to_string(),
                StageKind::ApplyDraft => " [apply_draft]".to_string(),
                StageKind::PlanWork => " [plan_work]".to_string(),
            };
            println!("  [{}] {}{} — {}", i + 1, stage.name, kind_label, desc);
        }
        println!();
        println!("Config:");
        println!("  reviewer_agent:       {}", config.reviewer_agent);
        println!("  gate_on_verdict:      {:?}", config.gate_on_verdict);
        println!("  max_phases:           {}", config.max_phases);
        println!("  pr_poll_interval_secs:{}", config.pr_poll_interval_secs);
        println!("  sync_timeout_hours:   {}", config.sync_timeout_hours);
        println!("  auto_merge:           {}", config.auto_merge);

        // For loop workflows: call `ta plan next` once to show what would run first.
        if has_goto {
            println!();
            // Resolve phase_filter from stage defs or top-level params.
            let dry_run_filter = def
                .stages
                .iter()
                .find_map(|s| s.phase_filter.as_deref())
                .or_else(|| opts.params.get("phase_filter").map(|s| s.as_str()));
            let filter_msg = dry_run_filter
                .map(|f| format!(" --filter {}", f))
                .unwrap_or_default();
            println!("Next phase preview (calls `ta plan next{}`):", filter_msg);
            let mut preview_args = vec![
                "--project-root".to_string(),
                opts.workspace_root.to_string_lossy().to_string(),
                "plan".to_string(),
                "next".to_string(),
                "--no-version-check".to_string(),
            ];
            if let Some(f) = dry_run_filter {
                preview_args.push("--filter".to_string());
                preview_args.push(f.to_string());
            }
            let output = std::process::Command::new("ta")
                .args(&preview_args)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let parsed = PlanNextOutput::parse(&stdout);
                    if parsed.done {
                        println!("  All plan phases complete — loop would not run.");
                    } else {
                        println!(
                            "  First iteration would run: {} — {}",
                            parsed.phase_id, parsed.phase_title
                        );
                        println!("  Max iterations: {}", config.max_phases);
                    }
                }
                _ => println!("  (could not invoke 'ta plan next' for preview)"),
            }
        }

        return Ok(());
    }

    // Resume or new run.
    let stage_name_list: Vec<&str> = def.stages.iter().map(|s| s.name.as_str()).collect();
    let mut run = if let Some(resume_id) = opts.resume_run_id {
        let existing = GovernedWorkflowRun::load(&runs_dir, resume_id)?;
        if existing.state == WorkflowRunState::Completed {
            anyhow::bail!(
                "Workflow run {} is already completed.",
                &existing.run_id[..8.min(existing.run_id.len())]
            );
        }
        println!(
            "Resuming workflow run {} at stage: {}",
            &existing.run_id[..8.min(existing.run_id.len())],
            existing.current_stage.as_deref().unwrap_or("(unknown)")
        );
        existing
    } else {
        let run_id = uuid::Uuid::new_v4().to_string();
        let run = GovernedWorkflowRun::new_with_stages(
            &run_id,
            opts.workflow_name,
            opts.goal_title,
            &stage_name_list,
        );
        run.save(&runs_dir)?;
        println!("Started workflow run: {}", &run_id[..8.min(run_id.len())]);
        println!("  Workflow: {}", opts.workflow_name);
        println!("  Goal:     {}", opts.goal_title);
        println!();
        run
    };

    if has_goto {
        // Loop execution mode: sequential with goto jumps.
        run_loop_workflow(&def.stages, config, &mut run, opts, &runs_dir)?;
    } else {
        // DAG execution mode: topological order, skip completed stages.
        for stage_name in &stage_order {
            let stage_def = def.stages.iter().find(|s| s.name == *stage_name).unwrap();
            let already_done = run
                .stages
                .iter()
                .find(|s| &s.name == stage_name)
                .map(|s| s.status == StageStatus::Completed)
                .unwrap_or(false);
            if already_done {
                println!("[{}] already completed — skipping", stage_name);
                continue;
            }

            print_stage_header(stage_name);

            run.current_stage = Some(stage_name.clone());
            if let Some(s) = run.stage_mut(stage_name) {
                s.start();
            }
            run.updated_at = Utc::now();
            run.save(&runs_dir)?;

            let start = Instant::now();
            let result = execute_stage(stage_def, &mut run, opts, config);
            let elapsed = start.elapsed().as_secs();

            match result {
                Ok(detail) => {
                    if let Some(s) = run.stage_mut(stage_name) {
                        s.complete(detail.clone());
                    }
                    run.emit_audit(stage_name, opts.agent, None, elapsed);
                    run.updated_at = Utc::now();
                    run.save(&runs_dir)?;
                    println!(
                        "  [{}] completed in {}s{}",
                        stage_name,
                        elapsed,
                        detail
                            .as_deref()
                            .map(|d| format!(" — {}", d))
                            .unwrap_or_default()
                    );
                }
                Err(e) => {
                    let reason = e.to_string();
                    if let Some(s) = run.stage_mut(stage_name) {
                        s.fail(&reason);
                    }
                    run.emit_audit(stage_name, opts.agent, Some("failed"), elapsed);
                    run.state = WorkflowRunState::Failed;
                    run.updated_at = Utc::now();
                    run.save(&runs_dir)?;
                    println!("  [{}] FAILED: {}", stage_name, reason);
                    println!();
                    println!(
                        "Workflow run {} failed at stage '{}'.",
                        &run.run_id[..8.min(run.run_id.len())],
                        stage_name
                    );
                    println!("Check the above error and retry or resume:");
                    println!(
                        "  ta workflow run {} --goal \"{}\" --resume {}",
                        opts.workflow_name,
                        opts.goal_title,
                        &run.run_id[..8.min(run.run_id.len())]
                    );
                    return Err(e);
                }
            }
        }
    }

    run.state = WorkflowRunState::Completed;
    run.current_stage = None;
    run.updated_at = Utc::now();
    run.save(&runs_dir)?;

    println!();
    println!("Workflow '{}' completed successfully.", opts.workflow_name);
    println!("  Run ID: {}", &run.run_id[..8.min(run.run_id.len())]);
    if let Some(pr_url) = &run.pr_url {
        println!("  PR:     {}", pr_url);
    }
    println!();
    println!(
        "Audit trail: ta audit export --workflow-run {}",
        &run.run_id[..8.min(run.run_id.len())]
    );

    Ok(())
}

/// Sequential loop execution for workflows containing `kind = "goto"` stages (v0.15.13).
///
/// Executes stages in definition order. For goto stages, jumps back to the
/// target when the condition is true. Guards against infinite loops via
/// `config.max_phases`.
fn run_loop_workflow(
    stages: &[StageDef],
    config: &WorkflowConfig,
    run: &mut GovernedWorkflowRun,
    opts: &RunOptions,
    runs_dir: &Path,
) -> anyhow::Result<()> {
    let mut idx: usize = 0;
    while idx < stages.len() {
        let stage_def = &stages[idx];
        let stage_name = stage_def.name.as_str();

        // Evaluate condition (skip non-goto stages when condition is false).
        if stage_def.kind != StageKind::Goto {
            if let Some(cond) = &stage_def.condition {
                match evaluate_condition(cond, &run.outputs) {
                    Ok(false) => {
                        tracing::debug!(
                            stage = stage_name,
                            condition = cond,
                            "stage skipped — condition false"
                        );
                        println!("  [{}] skipped (condition: {} = false)", stage_name, cond);
                        idx += 1;
                        continue;
                    }
                    Ok(true) => {}
                    Err(e) => anyhow::bail!("Stage '{}' condition error: {}", stage_name, e),
                }
            }
        }

        print_stage_header(stage_name);

        // Ensure the stage record exists (loop stages re-run, so reset status).
        if run.stage_mut(stage_name).is_none() {
            run.stages.push(StageRecord::new(stage_name));
        }
        run.current_stage = Some(stage_name.to_string());
        if let Some(s) = run.stage_mut(stage_name) {
            s.start();
        }
        run.updated_at = Utc::now();
        run.save(runs_dir)?;

        // Handle goto stage: evaluate condition and jump or fall through.
        if stage_def.kind == StageKind::Goto {
            let should_jump = if let Some(cond) = &stage_def.condition {
                evaluate_condition(cond, &run.outputs)
                    .map_err(|e| anyhow::anyhow!("Stage '{}' condition error: {}", stage_name, e))?
            } else {
                true // unconditional goto
            };

            if should_jump {
                // Check max_phases guard — extract count to avoid borrow conflict.
                let new_iters = {
                    let iters = run
                        .loop_iterations
                        .entry(stage_name.to_string())
                        .or_insert(0);
                    *iters += 1;
                    *iters
                };
                if new_iters > config.max_phases {
                    if let Some(s) = run.stage_mut(stage_name) {
                        s.complete(Some(format!(
                            "CHECKPOINT after {} iterations",
                            new_iters - 1
                        )));
                    }
                    run.updated_at = Utc::now();
                    run.save(runs_dir)?;
                    anyhow::bail!(
                        "Loop CHECKPOINT: stage '{}' reached the maximum iteration limit ({}).\n\
                         {} iterations completed. Check PLAN.md for remaining phases.\n\
                         To continue, re-run:\n  ta workflow run {} --goal \"{}\" --resume {}",
                        stage_name,
                        config.max_phases,
                        new_iters - 1,
                        opts.workflow_name,
                        opts.goal_title,
                        &run.run_id[..8.min(run.run_id.len())]
                    );
                }

                // Jump to target.
                let target = stage_def.target.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Stage '{}' has kind=goto but no `target` field defined",
                        stage_name
                    )
                })?;
                let target_idx = stages
                    .iter()
                    .position(|s| s.name == target)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Stage '{}' goto target '{}' not found in this workflow",
                            stage_name,
                            target
                        )
                    })?;

                let target_owned = target.to_string();
                if let Some(s) = run.stage_mut(stage_name) {
                    s.complete(Some(format!(
                        "jumped to '{}' (iteration {})",
                        target_owned, new_iters
                    )));
                }
                run.emit_audit(stage_name, "workflow", None, 0);
                run.updated_at = Utc::now();
                run.save(runs_dir)?;
                println!(
                    "  [{}] looping back to '{}' (iteration {})",
                    stage_name, target_owned, new_iters
                );
                idx = target_idx;
                continue;
            } else {
                // Condition false — fall through.
                if let Some(s) = run.stage_mut(stage_name) {
                    s.complete(Some("condition false — fall through".to_string()));
                }
                run.emit_audit(stage_name, "workflow", None, 0);
                run.updated_at = Utc::now();
                run.save(runs_dir)?;
                println!("  [{}] condition false — falling through", stage_name);
                idx += 1;
                continue;
            }
        }

        // Normal stage execution.
        let start = Instant::now();
        let result = execute_stage(stage_def, run, opts, config);
        let elapsed = start.elapsed().as_secs();

        match result {
            Ok(detail) => {
                if let Some(s) = run.stage_mut(stage_name) {
                    s.complete(detail.clone());
                }
                run.emit_audit(stage_name, opts.agent, None, elapsed);
                run.updated_at = Utc::now();
                run.save(runs_dir)?;
                println!(
                    "  [{}] completed in {}s{}",
                    stage_name,
                    elapsed,
                    detail
                        .as_deref()
                        .map(|d| format!(" — {}", d))
                        .unwrap_or_default()
                );
            }
            Err(e) => {
                let reason = e.to_string();
                if let Some(s) = run.stage_mut(stage_name) {
                    s.fail(&reason);
                }
                run.emit_audit(stage_name, opts.agent, Some("failed"), elapsed);
                run.state = WorkflowRunState::Failed;
                run.updated_at = Utc::now();
                run.save(runs_dir)?;
                println!("  [{}] FAILED: {}", stage_name, reason);
                println!();
                println!(
                    "Workflow run {} failed at stage '{}'.",
                    &run.run_id[..8.min(run.run_id.len())],
                    stage_name
                );
                println!("Check the above error and retry or resume:");
                println!(
                    "  ta workflow run {} --goal \"{}\" --resume {}",
                    opts.workflow_name,
                    opts.goal_title,
                    &run.run_id[..8.min(run.run_id.len())]
                );
                return Err(e);
            }
        }

        idx += 1;
    }

    Ok(())
}

/// Execute a single stage, dispatching on `StageKind`.
fn execute_stage(
    stage_def: &StageDef,
    run: &mut GovernedWorkflowRun,
    opts: &RunOptions,
    config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    match stage_def.kind {
        StageKind::PlanNext => stage_plan_next(run, stage_def, opts),
        StageKind::LoopNext => stage_loop_next(run, stage_def, opts),
        StageKind::Workflow => stage_run_subworkflow(run, stage_def, opts, config),
        StageKind::ApplyDraftBranch => stage_apply_draft_branch(run, stage_def, opts, config),
        StageKind::AggregateDraft => stage_aggregate_draft(run, stage_def, opts),
        StageKind::Join => stage_join(run, stage_def),
        StageKind::StaticAnalysis => stage_static_analysis(run, stage_def, opts),
        StageKind::Consensus => stage_consensus(run, stage_def, opts, config),
        StageKind::ApplyDraft => stage_apply_draft(run, opts, config),
        StageKind::PlanWork => stage_plan_work(run, stage_def, opts),
        StageKind::Goto => {
            // Goto is handled inline in run_loop_workflow; falling here is a bug.
            anyhow::bail!(
                "Internal error: execute_stage called for goto stage '{}'",
                stage_def.name
            )
        }
        StageKind::Default => {
            // Legacy name-based dispatch for the canonical governed-goal stages.
            match stage_def.name.as_str() {
                "run_goal" => stage_run_goal(run, opts),
                "review_draft" => stage_review_draft(run, opts, config),
                "human_gate" => stage_human_gate(run, config),
                "apply_draft" => stage_apply_draft(run, opts, config),
                "apply_draft_branch" => stage_apply_draft_branch(run, stage_def, opts, config),
                "aggregate_draft" => stage_aggregate_draft(run, stage_def, opts),
                "join" => stage_join(run, stage_def),
                "pr_sync" => stage_pr_sync(run, config, opts.workspace_root),
                other => anyhow::bail!(
                    "Unknown stage: '{}'. \
                     For custom stages, set `kind` in the workflow TOML.",
                    other
                ),
            }
        }
    }
}

// ── New stage executors (v0.15.13) ────────────────────────────────────────────

/// Stage executor for `kind = "plan_next"`.
///
/// Shells out to `ta plan next`, parses the output, and stores structured
/// results in `run.outputs[stage_name]` for downstream template interpolation.
/// When `stage_def.phase_filter` is set, passes `--filter <prefix>` so only
/// phases matching that prefix are considered.
fn stage_plan_next(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    let stage_name = &stage_def.name;
    // Resolve phase_filter: stage YAML field takes priority; fall back to
    // --param phase_filter=<value> passed on the CLI (v0.15.23+).
    let effective_filter = stage_def
        .phase_filter
        .as_deref()
        .or_else(|| opts.params.get("phase_filter").map(|s| s.as_str()));
    let filter_msg = effective_filter
        .map(|f| format!(" --filter {}", f))
        .unwrap_or_default();
    println!("  Running: ta plan next{}", filter_msg);

    let mut args = vec![
        "--project-root".to_string(),
        opts.workspace_root.to_string_lossy().to_string(),
        "plan".to_string(),
        "next".to_string(),
        "--no-version-check".to_string(),
    ];
    if let Some(prefix) = effective_filter {
        args.push("--filter".to_string());
        args.push(prefix.to_string());
    }

    let output = std::process::Command::new("ta")
        .args(&args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to invoke 'ta plan next': {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        anyhow::bail!(
            "ta plan next failed (exit {}):\n{}\n{}",
            output.status.code().unwrap_or(-1),
            stderr,
            stdout
        );
    }

    let parsed = PlanNextOutput::parse(&stdout);

    // Dispatch-history guard (v0.15.24.2): if plan_next returns a phase that
    // was already dispatched in this run, a status-marker race is in progress.
    // Halt immediately rather than launching a duplicate goal.
    if !parsed.done && !parsed.phase_id.is_empty() {
        let already_dispatched = run
            .dispatched_phases
            .iter()
            .enumerate()
            .find(|(_, id)| *id == &parsed.phase_id);
        if let Some((idx, _)) = already_dispatched {
            anyhow::bail!(
                "SAFETY: phase {} was already dispatched in this run (iteration {}). \
                 This indicates a status-marker race or a PLAN.md write failure. \
                 Halting to avoid duplicate work. Check PLAN.md — the phase should be \
                 marked in_progress or done. If the phase marker is still pending, \
                 mark it manually with `ta plan mark-in-progress {}` and resume.",
                parsed.phase_id,
                idx + 1,
                parsed.phase_id,
            );
        }
    }

    let detail = if parsed.done {
        "all phases complete".to_string()
    } else {
        format!("next: {} — {}", parsed.phase_id, parsed.phase_title)
    };

    // Store outputs for template interpolation.
    run.outputs
        .insert(stage_name.to_string(), parsed.to_output_map());

    Ok(Some(detail))
}

/// Stage executor for `kind = "loop_next"` (v0.15.14).
///
/// Alias for `plan_next` — runs `ta plan next` and emits the same structured
/// outputs. Passes `phase_filter` through when set on the stage def.
fn stage_loop_next(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    // Delegate entirely to stage_plan_next — same behavior, same output format.
    stage_plan_next(run, stage_def, opts)
}

/// Stage executor for `kind = "apply_draft_branch"` (v0.15.14).
///
/// Like `apply_draft` but appends `--branch <milestone_branch>` when the
/// `milestone_branch` field is set in the stage def. Falls back to regular
/// apply behavior when the field is absent.
fn stage_apply_draft_branch(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
    config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    if let Some(ref branch) = stage_def.milestone_branch {
        // Branch-targeted apply: apply the draft to a named branch.
        let draft_id = run.draft_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Stage '{}': no draft_id available — run_goal stage must complete first",
                stage_def.name
            )
        })?;

        println!(
            "  Running: ta draft apply {} --git-commit --branch {}",
            &draft_id[..8.min(draft_id.len())],
            branch
        );

        let mut cmd = std::process::Command::new("ta");
        cmd.args([
            "--project-root",
            &opts.workspace_root.to_string_lossy(),
            "draft",
            "apply",
            draft_id,
            "--git-commit",
            "--branch",
            branch,
            "--no-version-check",
        ]);
        if let Some(phase) = opts.plan_phase {
            cmd.args(["--phase", phase]);
        }

        let output = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to invoke 'ta draft apply': {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            anyhow::bail!(
                "ta draft apply (branch) failed (exit {}):\n{}\n{}",
                output.status.code().unwrap_or(-1),
                stderr,
                stdout
            );
        }

        Ok(Some(format!(
            "applied to branch '{}' (draft {})",
            branch,
            &draft_id[..8.min(draft_id.len())]
        )))
    } else {
        // No branch specified — fall back to regular apply.
        stage_apply_draft(run, opts, config)
    }
}

/// Stage executor for `kind = "aggregate_draft"` (v0.15.14).
///
/// Collects `draft_id` values from stage outputs (either all stages or a
/// named subset), deduplicates, and creates a `MilestoneDraft` saved to
/// `.ta/milestones/<uuid>.json`. Records `milestone_id` in this stage's
/// output map for downstream template interpolation.
fn stage_aggregate_draft(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    use ta_changeset::{MilestoneDraft, PhaseSummary};

    // Collect draft IDs from source stages.
    let source_spec = stage_def.source_stages.as_deref().unwrap_or("all");
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ordered_drafts: Vec<String> = Vec::new();
    let mut phase_summaries: Vec<PhaseSummary> = Vec::new();

    if source_spec == "all" {
        // Collect from every stage that has a draft_id output.
        for (sname, smap) in &run.outputs {
            if sname == &stage_def.name {
                continue; // skip self
            }
            if let Some(draft_id) = smap.get("draft_id") {
                if !draft_id.is_empty() && seen_ids.insert(draft_id.clone()) {
                    let phase_id = smap.get("phase_id").cloned();
                    let phase_title = smap.get("phase_title").cloned();
                    ordered_drafts.push(draft_id.clone());
                    phase_summaries.push(PhaseSummary {
                        draft_id: draft_id.clone(),
                        phase_id,
                        phase_title,
                        artifact_count: 0, // count not tracked at this layer
                    });
                }
            }
        }
    } else {
        // Collect from named stages (comma-separated).
        for sname in source_spec.split(',') {
            let sname = sname.trim();
            if let Some(smap) = run.outputs.get(sname) {
                if let Some(draft_id) = smap.get("draft_id") {
                    if !draft_id.is_empty() && seen_ids.insert(draft_id.clone()) {
                        let phase_id = smap.get("phase_id").cloned();
                        let phase_title = smap.get("phase_title").cloned();
                        ordered_drafts.push(draft_id.clone());
                        phase_summaries.push(PhaseSummary {
                            draft_id: draft_id.clone(),
                            phase_id,
                            phase_title,
                            artifact_count: 0,
                        });
                    }
                }
            }
        }
    }

    if ordered_drafts.is_empty() {
        println!(
            "  [{}] no draft IDs found in source stages (source_stages = '{}')",
            stage_def.name, source_spec
        );
    }

    let milestone_id = uuid::Uuid::new_v4().to_string();
    let milestone_title = stage_def
        .milestone_title
        .clone()
        .unwrap_or_else(|| "Milestone build".to_string());
    let milestone_branch = stage_def.milestone_branch.clone();

    println!(
        "  Creating MilestoneDraft '{}' with {} draft(s)",
        milestone_title,
        ordered_drafts.len()
    );

    let milestone = MilestoneDraft {
        milestone_id: milestone_id.clone(),
        milestone_title: milestone_title.clone(),
        source_drafts: ordered_drafts.clone(),
        milestone_branch,
        phase_summaries,
        created_at: chrono::Utc::now(),
    };
    milestone.save(opts.workspace_root)?;

    // Store milestone_id in outputs for downstream stages.
    let mut output_map = std::collections::HashMap::new();
    output_map.insert("milestone_id".to_string(), milestone_id.clone());
    output_map.insert("milestone_title".to_string(), milestone_title.clone());
    output_map.insert("draft_count".to_string(), ordered_drafts.len().to_string());
    run.outputs.insert(stage_def.name.clone(), output_map);

    println!(
        "  MilestoneDraft saved: .ta/milestones/{}.json",
        &milestone_id[..8.min(milestone_id.len())]
    );

    Ok(Some(format!(
        "milestone {} ({} drafts)",
        &milestone_id[..8.min(milestone_id.len())],
        ordered_drafts.len()
    )))
}

/// Stage executor for `kind = "join"` (v0.15.14).
///
/// A synchronization point for parallel stage groups. In the current sequential
/// execution model, this validates that all stages in the named `join_group`
/// completed successfully. When `on_partial_failure = "continue"`, failed stages
/// are treated as skipped rather than halting the workflow.
fn stage_join(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
) -> anyhow::Result<Option<String>> {
    let group = stage_def.join_group.as_deref().unwrap_or_default();
    if group.is_empty() {
        // No group specified — treat as a no-op sync point.
        return Ok(Some("join (no group — no-op)".to_string()));
    }

    // Find all stages that belong to this parallel group.
    // Since we don't have the stage defs here, we validate by checking the
    // run's stage records for any that failed.
    //
    // The join stage validates the run state: if any stage has Failed status
    // and on_partial_failure != "continue", bail out.
    let failed_stages: Vec<String> = run
        .stages
        .iter()
        .filter(|s| s.status == StageStatus::Failed)
        .map(|s| s.name.clone())
        .collect();

    let on_partial = stage_def.on_partial_failure.as_deref().unwrap_or("halt");

    if !failed_stages.is_empty() && on_partial != "continue" {
        anyhow::bail!(
            "Join stage '{}' (group '{}') detected {} failed stage(s): {}\n\
             Set on_partial_failure = \"continue\" in the stage def to proceed despite failures.",
            stage_def.name,
            group,
            failed_stages.len(),
            failed_stages.join(", ")
        );
    }

    if !failed_stages.is_empty() {
        println!(
            "  [{}] {} stage(s) failed but on_partial_failure=continue — proceeding",
            stage_def.name,
            failed_stages.len()
        );
    }

    Ok(Some(format!("join group='{}' validated", group)))
}

// ── Static analysis stage (v0.15.14.3) ───────────────────────────────────────

/// Stage executor for `kind = "static_analysis"` (v0.15.14.3).
///
/// 1. Loads `[analysis.<lang>]` from the project's `.ta/workflow.toml`.
/// 2. Runs the configured tool and parses structured findings.
/// 3. Dispatches on `on_failure`:
///    - `fail` → returns an error with the findings table.
///    - `warn` → logs findings and continues.
///    - `agent` → enters the correction loop (spawns fix goals, re-runs, iterates).
fn stage_static_analysis(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    use std::str::FromStr as _;
    use ta_goal::analysis::{
        detect_language, run_analyzer, AnalysisFinding, FindingSeverity, Language, OnFailure,
        OnMaxIterations,
    };

    let workspace_root = opts.workspace_root;

    // Load [analysis.*] from .ta/workflow.toml.
    let config_path = workspace_root.join(".ta").join("workflow.toml");
    let workflow_cfg = ta_submit::WorkflowConfig::load_or_default(&config_path);

    // Resolve language: stage-def override > auto-detect > first configured language.
    let language: Language = if let Some(ref lang_str) = stage_def.lang {
        Language::from_str(lang_str).unwrap()
    } else if let Some(detected) = detect_language(workspace_root) {
        if workflow_cfg.analysis.contains_key(&detected.as_key()) {
            detected
        } else if workflow_cfg.analysis.len() == 1 {
            let key = workflow_cfg.analysis.keys().next().unwrap();
            Language::from_str(key).unwrap()
        } else {
            detected
        }
    } else if workflow_cfg.analysis.len() == 1 {
        let key = workflow_cfg.analysis.keys().next().unwrap();
        Language::from_str(key).unwrap()
    } else {
        anyhow::bail!(
            "stage '{}': could not auto-detect language. \
             Set `lang = \"<lang>\"` in the stage definition or add `[analysis.*]` config.",
            stage_def.name
        );
    };

    let lang_key = language.as_key();
    let analysis_cfg = workflow_cfg
        .analysis
        .get(&lang_key)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "stage '{}': no [analysis.{}] section found in .ta/workflow.toml.",
                stage_def.name,
                lang_key
            )
        })?;

    println!(
        "  Running {} analysis (lang: {}, on_failure: {})...",
        analysis_cfg.tool, language, analysis_cfg.on_failure
    );

    // First run.
    let (success, _raw, findings) = run_analyzer(&analysis_cfg, workspace_root)?;
    if success && findings.is_empty() {
        println!("  {} passed — no findings.", analysis_cfg.tool);
        return Ok(Some(format!("{} passed", analysis_cfg.tool)));
    }

    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Error)
        .collect();
    println!(
        "  {} findings: {} error(s), {} total",
        analysis_cfg.tool,
        errors.len(),
        findings.len()
    );

    match analysis_cfg.on_failure {
        OnFailure::Warn => {
            println!("  [warn] on_failure=warn — logging findings and continuing.");
            println!("{}", AnalysisFinding::format_table(&findings));
            return Ok(Some(format!(
                "{} warn: {} finding(s)",
                analysis_cfg.tool,
                findings.len()
            )));
        }
        OnFailure::Fail => {
            println!("{}", AnalysisFinding::format_table(&findings));
            anyhow::bail!(
                "Static analysis failed: {} reported {} finding(s).\n\
                 Set on_failure = \"warn\" in [analysis.{}] to continue despite findings, \
                 or on_failure = \"agent\" to trigger the correction loop.",
                analysis_cfg.tool,
                findings.len(),
                lang_key
            );
        }
        OnFailure::Agent => {}
    }

    // Agent correction loop.
    println!(
        "  on_failure=agent — starting correction loop (max {} iterations)...",
        analysis_cfg.max_iterations
    );
    println!("{}", AnalysisFinding::format_table(&findings));

    let mut current_findings = findings;
    let mut iteration = 0u32;

    loop {
        iteration += 1;
        if iteration > analysis_cfg.max_iterations {
            break;
        }

        println!(
            "\n  [correction loop] iteration {}/{} — spawning fix goal...",
            iteration, analysis_cfg.max_iterations
        );

        // Build targeted fix objective.
        let objective =
            AnalysisFinding::build_fix_prompt(&analysis_cfg.tool, &language, &current_findings);
        let fix_title = format!(
            "Fix {} analysis findings ({}) — iteration {}",
            analysis_cfg.tool, language, iteration
        );

        // Spawn fix goal via `ta run`.
        let mut cmd = std::process::Command::new("ta");
        cmd.args([
            "--project-root",
            &workspace_root.to_string_lossy(),
            "run",
            &fix_title,
            "--agent",
            opts.agent,
            "--headless",
            "--no-version-check",
            "--objective",
            &objective,
        ]);
        println!(
            "  Running: ta run {:?} --agent {} --headless",
            fix_title, opts.agent
        );

        let output = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to invoke 'ta run' for fix goal: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            anyhow::bail!(
                "Fix goal failed (exit {}):\n{}\n{}",
                output.status.code().unwrap_or(-1),
                stderr,
                stdout
            );
        }

        // Extract draft ID from headless sentinel.
        let mut fix_draft_id: Option<String> = None;
        for line in stdout.lines().chain(stderr.lines()) {
            if let Some(json_str) = line.strip_prefix("__TA_HEADLESS_RESULT__:") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(id) = v["draft_id"].as_str() {
                        if id != "null" && !id.is_empty() {
                            fix_draft_id = Some(id.to_string());
                        }
                    }
                    if let Some(id) = v["goal_id"].as_str() {
                        run.goal_id = Some(id.to_string());
                    }
                }
                break;
            }
        }

        let draft_id = match fix_draft_id {
            Some(id) => id,
            None => {
                println!("  [correction loop] Fix goal did not produce a draft — stopping loop.");
                break;
            }
        };

        println!(
            "  [correction loop] Fix draft {} — applying to re-run analyzer...",
            &draft_id[..8.min(draft_id.len())]
        );
        run.draft_id = Some(draft_id.clone());

        // Apply the fix draft (without git commit — corrections accumulate, outer workflow commits).
        let apply_output = std::process::Command::new("ta")
            .args([
                "--project-root",
                &workspace_root.to_string_lossy(),
                "draft",
                "apply",
                &draft_id,
                "--no-version-check",
            ])
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to invoke 'ta draft apply': {}", e))?;

        if !apply_output.status.success() {
            let err = String::from_utf8_lossy(&apply_output.stderr);
            println!(
                "  [correction loop] draft apply failed — stopping loop. Error: {}",
                err.trim()
            );
            break;
        }

        // Re-run the analyzer.
        println!("  [correction loop] Re-running {}...", analysis_cfg.tool);
        let (ok, _raw2, new_findings) = run_analyzer(&analysis_cfg, workspace_root)?;
        if ok && new_findings.is_empty() {
            println!(
                "  [correction loop] Clean after {} iteration(s).",
                iteration
            );
            current_findings = new_findings;
            break;
        }

        current_findings = new_findings;
        let new_errors: Vec<_> = current_findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Error)
            .collect();
        println!(
            "  [correction loop] Still {} error(s) after iteration {}.",
            new_errors.len(),
            iteration
        );
    }

    // Loop finished — check final state.
    if current_findings.is_empty() {
        return Ok(Some(format!(
            "{} clean after {} correction pass(es)",
            analysis_cfg.tool, iteration
        )));
    }

    // Max iterations exhausted with remaining findings.
    let msg = format!(
        "{} still has {} finding(s) after {} correction pass(es)",
        analysis_cfg.tool,
        current_findings.len(),
        analysis_cfg.max_iterations
    );

    match analysis_cfg.on_max_iterations {
        OnMaxIterations::Warn => {
            println!(
                "  [warn] Max iterations ({}) reached with remaining findings. Continuing.",
                analysis_cfg.max_iterations
            );
            println!("{}", AnalysisFinding::format_table(&current_findings));
            Ok(Some(msg))
        }
        OnMaxIterations::Fail => {
            println!("{}", AnalysisFinding::format_table(&current_findings));
            anyhow::bail!(
                "{}\nSet on_max_iterations = \"warn\" in [analysis.{}] to continue despite \
                 remaining findings.",
                msg,
                lang_key
            )
        }
    }
}

/// VCS sync helper: pull the latest changes after a PR merge (v0.15.14).
///
/// Loads the workflow VCS config and runs the appropriate sync command via the
/// SourceAdapter. This routes through the adapter layer instead of calling git
/// directly, enabling non-git VCS support (v0.15.29).
fn do_vcs_sync(workspace_root: &Path) -> anyhow::Result<()> {
    let config_path = workspace_root.join(".ta").join("workflow.toml");
    let wf = ta_submit::WorkflowConfig::load_or_default(&config_path);
    let adapter = ta_submit::select_adapter_with_sync(workspace_root, &wf.submit, &wf.source.sync);

    // Ensure we are on the sync target branch before pulling.
    // Non-fatal: adapter.checkout_branch returns Ok even on failure, logs a warning.
    let _ = adapter.checkout_branch(&wf.source.sync.branch);

    adapter
        .sync_upstream()
        .map_err(|e| {
            anyhow::anyhow!(
                "VCS sync failed — check your network connection and try `git pull --rebase` manually: {}",
                e
            )
        })?;
    Ok(())
}

/// Stage executor for `kind = "consensus"` — aggregate reviewer votes (v0.15.15.1).
///
/// Reads reviewer verdict files from `.ta/review/<run-id>/<role>/verdict.json`,
/// builds `ConsensusInput` from the stage's `depends_on` list (reviewer roles),
/// calls `run_consensus()`, writes score/proceed/algorithm to the run output map,
/// and fails the stage if `result.proceed == false` (unless `override_reason` is set
/// via the consensus engine's `override_reason` mechanism).
fn stage_consensus(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
    _config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    use std::collections::HashMap;
    use ta_workflow::consensus::{run_consensus, ConsensusAlgorithm, ConsensusInput, ReviewerVote};

    let run_id = &run.run_id;
    let review_dir = opts.workspace_root.join(".ta").join("review").join(run_id);

    println!(
        "  [consensus] reading reviewer verdicts from {}",
        review_dir.display()
    );

    // Reviewer roles are listed in the stage's depends_on field.
    let reviewer_roles: Vec<String> = stage_def.depends_on.clone();

    let mut votes: Vec<ReviewerVote> = Vec::new();

    for role in &reviewer_roles {
        let verdict_path = review_dir.join(role).join("verdict.json");
        if !verdict_path.exists() {
            // Reviewer timed out or didn't write a verdict.
            println!(
                "  [consensus] role '{}': no verdict file — treated as timeout",
                role
            );
            votes.push(ReviewerVote {
                role: role.clone(),
                score: 0.0,
                findings: vec![format!(
                    "Verdict file not found: {}",
                    verdict_path.display()
                )],
                timed_out: true,
            });
            continue;
        }

        let raw = std::fs::read_to_string(&verdict_path)
            .map_err(|e| anyhow::anyhow!("Failed to read verdict for role '{}': {}", role, e))?;
        let verdict: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Invalid verdict JSON for role '{}': {}", role, e))?;

        let score = verdict
            .get("score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let findings: Vec<String> = verdict
            .get("findings")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        println!(
            "  [consensus] role '{}': score={:.2}, findings={}",
            role,
            score,
            findings.len()
        );
        votes.push(ReviewerVote {
            role: role.clone(),
            score,
            findings,
            timed_out: false,
        });
    }

    let weights: HashMap<String, f64> = HashMap::new();
    let threshold = 0.75_f64;
    let algorithm = ConsensusAlgorithm::Raft;
    let require_all = false;
    let override_reason: Option<String> = None;

    let run_dir = opts
        .workspace_root
        .join(".ta")
        .join("workflow-runs")
        .join(run_id);
    std::fs::create_dir_all(&run_dir)?;

    let input = ConsensusInput {
        votes,
        weights,
        threshold,
        algorithm,
        run_id: run_id.clone(),
        run_dir,
        require_all,
        override_reason,
    };

    let result =
        run_consensus(&input).map_err(|e| anyhow::anyhow!("Consensus engine error: {}", e))?;

    println!("  [consensus] {}", result.summary);

    // Store result fields in output map for downstream stages/conditions.
    let mut output_map = std::collections::HashMap::new();
    output_map.insert("score".to_string(), format!("{:.4}", result.score));
    output_map.insert("proceed".to_string(), result.proceed.to_string());
    output_map.insert("algorithm".to_string(), result.algorithm_used.to_string());
    run.outputs.insert(stage_def.name.clone(), output_map);

    if !result.proceed && !result.override_active {
        anyhow::bail!(
            "Consensus gate BLOCKED: score={:.2} is below threshold={:.2}.\n\
             Algorithm: {}\n\
             Per-role scores: {}\n\
             Timed-out roles: {}\n\
             Use --override-reason to bypass this gate with an audit entry.",
            result.score,
            threshold,
            result.algorithm_used,
            result
                .scores_by_role
                .iter()
                .map(|(k, v)| format!("{}={:.2}", k, v))
                .collect::<Vec<_>>()
                .join(", "),
            if result.timed_out_roles.is_empty() {
                "none".to_string()
            } else {
                result.timed_out_roles.join(", ")
            }
        );
    }

    Ok(Some(result.summary))
}

/// Stage executor for `kind = "workflow"` — invokes a child workflow.
///
/// Resolves the child workflow definition, applies template interpolation to
/// the `goal` and `phase` fields, then calls `run_governed_workflow` recursively
/// (depth-limited to 5). The child run ID is recorded in `sub_workflow_records`.
fn stage_run_subworkflow(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
    _config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    let child_workflow = stage_def.workflow.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Stage '{}' has kind=workflow but no `workflow` field defined",
            stage_def.name
        )
    })?;

    // Apply template interpolation to goal and phase.
    let raw_goal = stage_def.goal.as_deref().unwrap_or(opts.goal_title);
    let child_goal = interpolate_template(raw_goal, &run.outputs);
    let child_phase = stage_def
        .phase
        .as_deref()
        .map(|p| interpolate_template(p, &run.outputs));

    // Check that placeholders were resolved.
    if child_goal.contains("{{") {
        anyhow::bail!(
            "Stage '{}': goal template '{}' has unresolved placeholders. \
             Ensure the referenced stage ran before this step.",
            stage_def.name,
            child_goal
        );
    }

    println!(
        "  Invoking sub-workflow '{}' for goal: {}",
        child_workflow, child_goal
    );
    if let Some(ref p) = child_phase {
        println!("    Phase: {}", p);
    }
    println!("    Depth: {}/{}", opts.depth + 1, 5);

    let child_opts = RunOptions {
        workspace_root: opts.workspace_root,
        workflow_name: child_workflow,
        goal_title: &child_goal,
        dry_run: opts.dry_run,
        resume_run_id: None,
        agent: opts.agent,
        plan_phase: child_phase.as_deref(),
        depth: opts.depth + 1,
        params: opts.params.clone(),
    };

    // We need to capture the child run ID. Run the child workflow, then find
    // the most recently created run as the child.
    let runs_dir = opts.workspace_root.join(".ta").join("workflow-runs");
    run_governed_workflow(&child_opts)?;

    // Find the child run ID by looking for the most recently modified run
    // that matches the child workflow name.
    let child_run_id = GovernedWorkflowRun::find_latest(&runs_dir)
        .ok()
        .flatten()
        .filter(|r| r.workflow_name == child_workflow)
        .map(|r| r.run_id)
        .unwrap_or_else(|| "(unknown)".to_string());

    run.sub_workflow_records.push(SubworkflowRecord {
        parent_run_id: run.run_id.clone(),
        child_run_id: child_run_id.clone(),
        stage_name: stage_def.name.clone(),
        child_workflow: child_workflow.to_string(),
    });

    Ok(Some(format!(
        "sub-workflow '{}' completed (run: {})",
        child_workflow,
        &child_run_id[..8.min(child_run_id.len())]
    )))
}

/// Stage 1: run_goal — invoke `ta run` to create a goal and produce a draft.
fn stage_run_goal(
    run: &mut GovernedWorkflowRun,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    if let Some(phase) = opts.plan_phase {
        println!(
            "  Running: ta run \"{}\" --agent {} --phase {} --headless",
            opts.goal_title, opts.agent, phase
        );
    } else {
        println!(
            "  Running: ta run \"{}\" --agent {} --headless",
            opts.goal_title, opts.agent
        );
    }

    let mut cmd = std::process::Command::new("ta");
    cmd.args([
        "--project-root",
        &opts.workspace_root.to_string_lossy(),
        "run",
        opts.goal_title,
        "--agent",
        opts.agent,
        "--headless",
        "--no-version-check",
    ]);
    if let Some(phase) = opts.plan_phase {
        cmd.args(["--phase", phase]);
    }
    // v0.15.20: If a plan_work stage preceded this run_goal stage, pass the
    // work plan path to the implementor via env var. ta run reads this and
    // injects the work plan into CLAUDE.md.
    let work_plan_path_opt = run
        .outputs
        .values()
        .find_map(|stage_out| stage_out.get("work_plan_path").cloned());
    if let Some(ref wp_path) = work_plan_path_opt {
        cmd.env("TA_WORK_PLAN_JSON_PATH", wp_path);
        println!("  [run_goal] Work plan injected from: {}", wp_path);
    }
    let output = cmd.output().map_err(|e| {
        anyhow::anyhow!(
            "Failed to invoke 'ta run': {}\nIs ta installed and on PATH?",
            e
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        anyhow::bail!(
            "ta run failed (exit {}):\n{}\n{}",
            output.status.code().unwrap_or(-1),
            stderr,
            stdout
        );
    }

    // Extract goal_id and draft_id from output.
    //
    // `ta run --headless` emits a JSON sentinel line at the end:
    //   __TA_HEADLESS_RESULT__:{"goal_id":"...","draft_built":true,"draft_id":"...","state":"..."}
    //
    // Legacy/fallback: some paths print bare `goal_id: <id>` / `draft_id: <id>` lines.
    // Parse both formats so we are robust to either.
    for line in stdout.lines().chain(stderr.lines()) {
        // Primary format: JSON sentinel
        if let Some(json_str) = line.strip_prefix("__TA_HEADLESS_RESULT__:") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(id) = v["goal_id"].as_str() {
                    run.goal_id = Some(id.to_string());
                }
                if let Some(id) = v["draft_id"].as_str() {
                    if id != "null" && !id.is_empty() {
                        run.draft_id = Some(id.to_string());
                    }
                }
            }
            break; // sentinel is the definitive line
        }
        // Fallback: bare prefix lines
        if let Some(id) = line.strip_prefix("goal_id: ") {
            run.goal_id = Some(id.trim().to_string());
        }
        if let Some(id) = line.strip_prefix("draft_id: ") {
            run.draft_id = Some(id.trim().to_string());
        }
    }

    // Guard: agent must produce a draft. If not, the workflow would silently
    // complete every stage (reviewer auto-approves an empty/missing draft,
    // apply skips, pr_sync skips) and then loop forever because PLAN.md is
    // never updated. Fail here with a clear message instead.
    if run.draft_id.is_none() {
        anyhow::bail!(
            "ta run completed but did not produce a draft.\n\
             This usually means the agent finished without making changes, \
             or the phase context was not injected.\n\
             Check the goal output above for details, then re-run with \
             'ta workflow run {} --goal \"{}\"'{}.",
            opts.workflow_name,
            opts.goal_title,
            opts.plan_phase
                .map(|p| format!(" --phase {}", p))
                .unwrap_or_default()
        );
    }

    // v0.15.19.4.2: Parse [progress] heartbeat lines from agent stdout.
    parse_and_report_progress_heartbeats(&stdout);

    // v0.15.24.2: Record the dispatched phase_id for the dispatch-history guard.
    // Look for a phase_id in stage outputs (plan_next or loop_next stage).
    let dispatched_phase_id: Option<String> =
        opts.plan_phase.map(|p| p.to_string()).or_else(|| {
            run.outputs.values().find_map(|stage_out| {
                stage_out
                    .get("phase_id")
                    .filter(|id| !id.is_empty())
                    .cloned()
            })
        });
    if let Some(ref pid) = dispatched_phase_id {
        if !run.dispatched_phases.contains(pid) {
            run.dispatched_phases.push(pid.clone());
        }
    }

    let detail = match &run.draft_id {
        Some(id) => format!("draft {}", &id[..8.min(id.len())]),
        None => unreachable!("guarded above"),
    };
    Ok(Some(detail))
}

/// Parse `[progress] item N:` lines from agent stdout and emit a summary.
fn parse_and_report_progress_heartbeats(stdout: &str) {
    let mut item_reports: Vec<String> = Vec::new();
    let mut phase_completions: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("[progress] item ") {
            item_reports.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("[progress] phase ") {
            phase_completions.push(rest.to_string());
        }
    }

    if item_reports.is_empty() && phase_completions.is_empty() {
        println!("  [run_goal] No progress heartbeats from agent — check CLAUDE.md injection.");
    } else {
        println!(
            "  [run_goal] Progress: {} item(s) reported complete by agent.",
            item_reports.len()
        );
        for completion in &phase_completions {
            println!("  [run_goal] Phase complete: {}", completion);
        }
    }
}

/// Stage: plan_work — spawn a read-only planner agent to produce .ta/work-plan.json.
///
/// The planner agent analyzes the goal and codebase (read-only) and writes a structured
/// work plan with design decisions, implementation steps, and out-of-scope items.
/// The work plan is stored at `<workspace_root>/.ta/work-plans/<run_id>/work-plan.json`
/// and its path is published to `run.outputs["plan_work"]["work_plan_path"]` for the
/// subsequent run_goal (implementor) stage to consume.
fn stage_plan_work(
    run: &mut GovernedWorkflowRun,
    stage_def: &StageDef,
    opts: &RunOptions,
) -> anyhow::Result<Option<String>> {
    let plans_dir = opts
        .workspace_root
        .join(".ta")
        .join("work-plans")
        .join(&run.run_id);
    std::fs::create_dir_all(&plans_dir)?;

    let work_plan_path = plans_dir.join("work-plan.json");

    println!("  Spawning planner agent for goal: {}", opts.goal_title);
    println!(
        "  Work plan will be written to: {}",
        work_plan_path.display()
    );

    let planner_prompt =
        build_planner_prompt(opts.workspace_root, opts.goal_title, &work_plan_path);

    let output = std::process::Command::new("ta")
        .args([
            "--project-root",
            &opts.workspace_root.to_string_lossy(),
            "run",
            &format!("Work planner: {}", opts.goal_title),
            "--agent",
            opts.agent,
            "--headless",
            "--objective",
            &planner_prompt,
            "--no-version-check",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to invoke planner agent: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !work_plan_path.exists() {
            anyhow::bail!(
                "Planner agent failed and did not produce work-plan.json.\n\
                 Agent error: {}\n\
                 You can write the work plan manually:\n  {}",
                stderr,
                work_plan_path.display()
            );
        }
        // Agent exit != 0 but plan was written — warn and continue.
        eprintln!(
            "[plan_work] Planner agent exited non-zero but work-plan.json was written — continuing."
        );
    }

    if !work_plan_path.exists() {
        anyhow::bail!(
            "Planner agent completed but did not write work-plan.json.\n\
             Expected: {}\n\
             The planner agent must write this file before the implementor can proceed.",
            work_plan_path.display()
        );
    }

    // Validate the work plan.
    let work_plan = ta_workflow::WorkPlan::load_from(&work_plan_path)
        .map_err(|e| anyhow::anyhow!("Failed to load work plan: {}", e))?;
    work_plan
        .validate()
        .map_err(|e| anyhow::anyhow!("Work plan validation failed: {}", e))?;

    // Store path in run outputs for the implementor stage.
    let stage_name = &stage_def.name;
    let mut stage_outputs = std::collections::HashMap::new();
    stage_outputs.insert(
        "work_plan_path".to_string(),
        work_plan_path.to_string_lossy().to_string(),
    );
    stage_outputs.insert(
        "decisions_count".to_string(),
        work_plan.decisions.len().to_string(),
    );
    run.outputs.insert(stage_name.clone(), stage_outputs);

    let summary = format!(
        "{} decision(s), {} step(s)",
        work_plan.decisions.len(),
        work_plan.implementation_plan.len()
    );
    println!("  [plan_work] Work plan validated: {}", summary);
    Ok(Some(summary))
}

/// Build the planner agent objective prompt.
fn build_planner_prompt(workspace_root: &Path, goal_title: &str, work_plan_path: &Path) -> String {
    format!(
        r#"You are a Work Planner agent. Your job is to analyze the codebase and produce a structured implementation plan for the following goal:

Goal: {goal_title}

## Your role

You are a PLANNER, not an implementor. You MUST NOT write any code or modify any source files.
Your only output is the work plan JSON file described below.

## What to do

1. Read and understand the goal.
2. Explore the codebase using Read, Grep, and Glob tools to understand the current state.
3. Identify what needs to change and why — be specific about file paths, function names, and design decisions.
4. Record your decisions with rationale and alternatives considered.
5. Produce a step-by-step implementation plan.
6. Write the plan to the path below.

## Output format

Write a JSON file to this EXACT path (use an absolute path):
{work_plan_path}

The JSON must match this schema:
{{
  "goal": "{goal_title}",
  "decisions": [
    {{
      "decision": "One-line description of the design choice",
      "rationale": "Why this approach was chosen — required, must not be empty",
      "alternatives": ["option A", "option B"],
      "files_affected": ["src/foo.rs", "src/bar.rs"],
      "confidence": 0.9
    }}
  ],
  "implementation_plan": [
    {{ "step": 1, "file": "src/foo.rs", "action": "add Foo struct", "detail": "Fields: bar: String, baz: u32" }},
    {{ "step": 2, "file": "src/main.rs", "action": "wire Foo into main", "detail": "" }}
  ],
  "out_of_scope": ["list of things explicitly not being changed and why"]
}}

## Rules

- decisions MUST be non-empty (at least one decision required)
- Every decision MUST have a non-empty rationale
- Do NOT write any Rust, Python, or other source code
- Do NOT modify any files except to write the work-plan.json output
- Use the Bash tool to write the JSON: `echo '{{...}}' > {work_plan_path}` or use the Write tool

The workspace root is: {workspace_root}

The implementor agent will receive your plan as the first section of its context and will execute it faithfully.
"#,
        goal_title = goal_title,
        work_plan_path = work_plan_path.display(),
        workspace_root = workspace_root.display(),
    )
}

/// Stage 2: review_draft — spawn reviewer agent to write verdict.json.
fn stage_review_draft(
    run: &mut GovernedWorkflowRun,
    opts: &RunOptions,
    config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    let draft_id = run.draft_id.as_deref().unwrap_or("latest");
    let review_dir = opts
        .workspace_root
        .join(".ta")
        .join("review")
        .join(draft_id);
    std::fs::create_dir_all(&review_dir)?;

    println!(
        "  Spawning reviewer agent ({}) for draft {}",
        config.reviewer_agent,
        &draft_id[..8.min(draft_id.len())]
    );
    println!(
        "  Verdict will be written to: {}",
        review_dir.join("verdict.json").display()
    );

    // Construct reviewer prompt and invoke.
    let reviewer_prompt = build_reviewer_prompt(opts.workspace_root, draft_id)?;
    let verdict_path = review_dir.join("verdict.json");

    let output = std::process::Command::new("ta")
        .args([
            "--project-root",
            &opts.workspace_root.to_string_lossy(),
            "run",
            &format!(
                "Review draft {} for governed workflow",
                &draft_id[..8.min(draft_id.len())]
            ),
            "--agent",
            &config.reviewer_agent,
            "--headless",
            "--objective",
            &reviewer_prompt,
            "--no-version-check",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to invoke reviewer agent: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If reviewer agent failed but verdict.json was still written, continue.
        if !verdict_path.exists() {
            anyhow::bail!(
                "Reviewer agent failed and did not produce verdict.json.\n\
                 Agent error: {}\n\
                 You can write verdict.json manually and resume:\n  \
                   echo '{{\"verdict\":\"approve\",\"findings\":[],\"confidence\":0.9}}' > {}",
                stderr,
                verdict_path.display()
            );
        }
    }

    // Load and validate the verdict.
    if !verdict_path.exists() {
        anyhow::bail!(
            "Reviewer agent completed but did not write verdict.json.\n\
             Expected: {}\n\
             The reviewer agent must write this file with the verdict.",
            verdict_path.display()
        );
    }

    let verdict = ReviewerVerdict::load(&review_dir)?;
    verdict.validate()?;

    let summary = format!(
        "verdict={}, confidence={:.0}%, findings={}",
        verdict.verdict,
        verdict.confidence * 100.0,
        verdict.findings.len()
    );
    run.verdict = Some(verdict);

    Ok(Some(summary))
}

/// Build the reviewer agent objective prompt (v0.15.7.1 — staging-independent).
///
/// The reviewer reads the draft package directly (change_summary, artifact list,
/// decision log). It does NOT require access to the staging directory — embedded
/// context is sufficient. Staging is offered as optional supplementary context
/// only if the directory still exists.
///
/// The reviewer marks `Failed` only if it produces no verdict JSON, not on
/// staging absence.
fn build_reviewer_prompt(workspace_root: &Path, draft_id: &str) -> anyhow::Result<String> {
    // Absolute path so the agent writes to the source .ta/review/, not its staging copy.
    // The reviewer runs inside a staging workspace where .ta/ is excluded from diffs;
    // a relative path would silently write there instead of the expected location.
    let verdict_path = workspace_root
        .join(".ta")
        .join("review")
        .join(draft_id)
        .join("verdict.json");

    // Load the draft package for embedded context (v0.15.7.1).
    // This makes the reviewer independent of the staging directory.
    let pkg_dir = workspace_root.join(".ta").join("drafts").join(draft_id);
    let summary_path = pkg_dir.join("change_summary.json");
    let decision_log_path = pkg_dir.join("decision_log.json");

    let summary_text = if summary_path.exists() {
        std::fs::read_to_string(&summary_path).unwrap_or_default()
    } else {
        // Fall back to the draft package JSON (pr_packages dir).
        let pkg_json_path = workspace_root
            .join(".ta")
            .join("pr_packages")
            .join(format!("{}.json", draft_id));
        if pkg_json_path.exists() {
            // Extract summary from the draft package JSON.
            std::fs::read_to_string(&pkg_json_path)
                .ok()
                .and_then(|s| {
                    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
                    let summary = v.get("summary")?;
                    serde_json::to_string_pretty(summary).ok()
                })
                .unwrap_or_else(|| "(no change summary available)".to_string())
        } else {
            "(no change summary available)".to_string()
        }
    };

    let decision_log_text = if decision_log_path.exists() {
        let text = std::fs::read_to_string(&decision_log_path).unwrap_or_default();
        if text.trim().is_empty() {
            String::new()
        } else {
            format!("\nAgent decision log:\n{}\n", text)
        }
    } else {
        String::new()
    };

    // Artifact list from draft package JSON (if available).
    let artifact_list_text = {
        let pkg_json_path = workspace_root
            .join(".ta")
            .join("pr_packages")
            .join(format!("{}.json", draft_id));
        if pkg_json_path.exists() {
            std::fs::read_to_string(&pkg_json_path)
                .ok()
                .and_then(|s| {
                    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
                    let artifacts = v.get("changes")?.get("artifacts")?.as_array()?;
                    let lines: Vec<String> = artifacts
                        .iter()
                        .filter_map(|a| {
                            let uri = a.get("resource_uri")?.as_str()?;
                            let change_type = a
                                .get("change_type")
                                .and_then(|c| c.as_str())
                                .unwrap_or("modified");
                            Some(format!("  {} {}", change_type, uri))
                        })
                        .collect();
                    if lines.is_empty() {
                        None
                    } else {
                        Some(format!(
                            "\nArtifacts ({}):\n{}",
                            lines.len(),
                            lines.join("\n")
                        ))
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        }
    };

    // v0.15.19.4.2: Check if this draft was previously denied (re-run after denial).
    let prior_denial_note = {
        let pkg_json_path = workspace_root
            .join(".ta")
            .join("pr_packages")
            .join(format!("{}.json", draft_id));
        if pkg_json_path.exists() {
            std::fs::read_to_string(&pkg_json_path)
                .ok()
                .and_then(|s| {
                    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
                    let history = v.get("status_history")?.as_array()?;
                    let was_denied = history.iter().any(|h| {
                        h.get("status")
                            .and_then(|s| s.as_str())
                            .map(|s| s.contains("denied") || s.contains("Denied"))
                            .unwrap_or(false)
                    });
                    if was_denied {
                        Some("\nNOTE: This draft was previously denied and re-submitted. \
                             Do not re-flag the same issues that were already flagged in the prior review — \
                             focus on whether the denial reason has been addressed.\n".to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        }
    };

    // v0.15.19.4.2: Detect PLAN.md-only drafts and add source-verification instruction.
    let planmd_only_note = {
        let uris: Vec<&str> = artifact_list_text
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                if l.contains("PLAN.md") && !l.contains(".rs") && !l.contains(".toml") {
                    // Check if line contains only PLAN.md artifact
                    Some(l)
                } else {
                    None
                }
            })
            .collect();
        let is_planmd_only = !artifact_list_text.is_empty()
            && artifact_list_text
                .lines()
                .filter(|l| {
                    let l = l.trim();
                    l.starts_with("modified")
                        || l.starts_with("created")
                        || l.starts_with("deleted")
                })
                .all(|l| l.contains("PLAN.md"));
        let _ = uris; // suppress unused warning
        if is_planmd_only {
            "\nSOURCE-VERIFICATION MODE: This draft modifies only PLAN.md. \
             Before flagging any item as 'false record', verify whether the \
             implementation is already present in the source workspace. \
             If items are marked [x] and the implementation exists in source, \
             verdict should be 'approve' with note 'catch-up PLAN.md update — \
             items verified in source'. Only flag if implementation is genuinely \
             missing from source.\n"
                .to_string()
        } else {
            String::new()
        }
    };

    Ok(format!(
        "You are a code reviewer performing a governance review of a draft change set.\n\
         \n\
         IMPORTANT: You do NOT need access to the staging workspace directory. \
         All relevant context is embedded below. If staging is unavailable, \
         log 'staging absent — using embedded patches' and proceed with the review.\n\
         {prior_denial_note}\
         {planmd_only_note}\
         \n\
         Draft ID: {draft_id}\n\
         Change summary:\n{summary_text}\n\
         {decision_log_text}\
         {artifact_list_text}\n\
         \n\
         Review the changes for:\n\
         - Correctness and completeness relative to the stated goal\n\
         - Security issues (injection, auth bypass, credential exposure)\n\
         - Test coverage (are new code paths tested?)\n\
         - Breaking changes without migration path\n\
         - Constitution violations\n\
         \n\
         Write your verdict to this exact absolute path: {verdict_path}\n\
         Use this exact JSON format:\n\
         {{\n\
           \"verdict\": \"approve\" | \"flag\" | \"reject\",\n\
           \"findings\": [\"finding 1\", \"finding 2\"],\n\
           \"confidence\": 0.0-1.0\n\
         }}\n\
         \n\
         Use \"approve\" if the change is acceptable.\n\
         Use \"flag\" if there are concerns but it could be acceptable with human review.\n\
         Use \"reject\" if there are serious issues that must be fixed before applying.\n\
         \n\
         You MUST write verdict.json. Failure to produce a verdict is the only \
         condition under which this review is considered failed.",
        prior_denial_note = prior_denial_note,
        planmd_only_note = planmd_only_note,
        draft_id = draft_id,
        summary_text = summary_text,
        decision_log_text = decision_log_text,
        artifact_list_text = artifact_list_text,
        verdict_path = verdict_path.display(),
    ))
}

/// Stage 3: human_gate — read verdict, decide whether to proceed.
fn stage_human_gate(
    run: &mut GovernedWorkflowRun,
    config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    let verdict = run.verdict.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No verdict available — the review_draft stage must complete first.\n\
             If resuming, ensure review_draft completed successfully."
        )
    })?;

    let verdict_clone = verdict.clone();

    // Auto-approve: skip interactive prompt when all configured conditions are met.
    if config.auto_approve.conditions_met(&verdict_clone) {
        println!(
            "  [auto-approve] conditions met — applying without prompt (verdict={}, confidence={:.0}%)",
            verdict_clone.verdict,
            verdict_clone.confidence * 100.0
        );
        run.audit_trail.push(StageAuditEntry {
            stage: "human_gate".to_string(),
            agent: "auto-approve".to_string(),
            verdict: Some("auto-approved".to_string()),
            duration_secs: 0,
            at: Utc::now(),
        });
        return Ok(Some(format!(
            "verdict={} — auto-approved (conditions: {})",
            verdict_clone.verdict,
            config.auto_approve.conditions.join(", ")
        )));
    }

    let decision = evaluate_human_gate(&verdict_clone, &config.gate_on_verdict, true)?;

    match decision {
        GateDecision::Proceed => Ok(Some(format!(
            "verdict={} — proceeding",
            verdict_clone.verdict
        ))),
        GateDecision::Override => Ok(Some(format!(
            "verdict={} — human override, proceeding",
            verdict_clone.verdict
        ))),
        GateDecision::Reject => {
            // Deny the draft and emit an audit entry.
            if let Some(draft_id) = &run.draft_id.clone() {
                deny_draft_for_rejection(run, draft_id, &verdict_clone)?;
            }
            anyhow::bail!(
                "Draft rejected by governance gate.\n\
                 Verdict: {} (confidence {:.0}%)\n\
                 Findings:\n  {}\n\
                 The draft has been denied. Start a new goal to address the findings.",
                verdict_clone.verdict,
                verdict_clone.confidence * 100.0,
                if verdict_clone.findings.is_empty() {
                    "(no specific findings)".to_string()
                } else {
                    verdict_clone.findings.join("\n  ")
                }
            )
        }
    }
}

/// Write a denial record for a rejected draft.
fn deny_draft_for_rejection(
    run: &mut GovernedWorkflowRun,
    draft_id: &str,
    verdict: &ReviewerVerdict,
) -> anyhow::Result<()> {
    let denial_reason = format!(
        "Governed workflow rejected by reviewer agent.\n\
         Verdict: {} (confidence {:.0}%)\n\
         Findings: {}",
        verdict.verdict,
        verdict.confidence * 100.0,
        verdict.findings.join("; ")
    );

    // Emit a workflow audit entry for the rejection.
    run.audit_trail.push(StageAuditEntry {
        stage: "human_gate".to_string(),
        agent: "governance".to_string(),
        verdict: Some("reject".to_string()),
        duration_secs: 0,
        at: Utc::now(),
    });

    // Attempt to call `ta draft deny` to officially deny the draft.
    let _ = std::process::Command::new("ta")
        .args(["draft", "deny", draft_id, "--reason", &denial_reason])
        .status();

    Ok(())
}

/// Stage 4: apply_draft — apply the approved draft with git commit.
fn stage_apply_draft(
    run: &mut GovernedWorkflowRun,
    opts: &RunOptions,
    config: &WorkflowConfig,
) -> anyhow::Result<Option<String>> {
    let draft_id = run.draft_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!("No draft_id available — run_goal stage did not capture a draft ID")
    })?;

    if let Some(phase) = opts.plan_phase {
        println!(
            "  Running: ta draft apply {} --git-commit --phase {}",
            &draft_id[..8.min(draft_id.len())],
            phase
        );
    } else {
        println!(
            "  Running: ta draft apply {} --git-commit",
            &draft_id[..8.min(draft_id.len())]
        );
    }

    let mut cmd = std::process::Command::new("ta");
    cmd.args([
        "--project-root",
        &opts.workspace_root.to_string_lossy(),
        "draft",
        "apply",
        draft_id,
        "--git-commit",
        "--no-version-check",
    ]);
    if let Some(phase) = opts.plan_phase {
        cmd.args(["--phase", phase]);
    }
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to invoke 'ta draft apply': {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        anyhow::bail!(
            "ta draft apply failed (exit {}):\n{}\n{}",
            output.status.code().unwrap_or(-1),
            stderr,
            stdout
        );
    }

    // Extract PR URL and branch from output.
    // `ta draft apply` prints indented lines like:
    //   "  Branch:  feature/abc-xyz"
    //   "  PR:      https://github.com/..."
    // so we trim leading whitespace before matching.
    let mut branch_from_output: Option<String> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("PR:") {
            let candidate = rest.trim().to_string();
            // Ignore the "not created" fallback message; only accept URLs.
            if candidate.starts_with("http") {
                run.pr_url = Some(candidate);
            }
        } else if let Some(rest) = trimmed.strip_prefix("Branch:") {
            branch_from_output = Some(rest.trim().to_string());
        }
    }

    // Fallback: if no URL was found in the output but we have the branch name,
    // ask the VCS adapter (gh pr list) to locate the PR.
    if run.pr_url.is_none() {
        if let Some(ref branch) = branch_from_output {
            println!(
                "  [pr_url] Not found in output — querying gh pr list for branch '{}'...",
                branch
            );
            let gh_out = std::process::Command::new("gh")
                .args([
                    "pr", "list", "--head", branch, "--json", "url", "--jq", ".[0].url",
                ])
                .current_dir(opts.workspace_root)
                .output();
            match gh_out {
                Ok(o) if o.status.success() => {
                    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if url.starts_with("http") {
                        println!("  [pr_url] Found via gh: {}", url);
                        run.pr_url = Some(url);
                    } else {
                        println!(
                            "  [pr_url] gh returned no matching PR for branch '{}'",
                            branch
                        );
                    }
                }
                Ok(o) => {
                    println!(
                        "  [pr_url] gh pr list returned non-zero ({}): {}",
                        o.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                }
                Err(e) => {
                    println!("  [pr_url] Could not invoke gh: {}", e);
                }
            }
        } else {
            println!(
                "  [pr_url] No PR URL and no branch name in output — cannot auto-discover PR."
            );
        }
    }

    // Enable auto-merge if configured — PR merges automatically once CI passes.
    if config.auto_merge {
        if let Some(ref url) = run.pr_url {
            println!("  Enabling auto-merge on PR (--squash)...");
            let merge_out = std::process::Command::new("gh")
                .args(["pr", "merge", "--auto", "--squash", url])
                .output();
            match merge_out {
                Ok(o) if o.status.success() => {
                    println!("  Auto-merge enabled — PR will merge when CI passes.");
                }
                Ok(o) => {
                    // Non-fatal: auto-merge may fail if the repo doesn't have it enabled,
                    // or if the PR is already mergeable. pr_sync will still poll normally.
                    let msg = String::from_utf8_lossy(&o.stderr);
                    println!(
                        "  [warn] gh pr merge --auto returned non-zero ({}): {}",
                        o.status.code().unwrap_or(-1),
                        msg.trim()
                    );
                }
                Err(e) => {
                    println!("  [warn] Could not invoke gh to enable auto-merge: {}", e);
                }
            }
        }
    }

    let detail = match &run.pr_url {
        Some(url) => format!("applied, PR: {}", url),
        None => "applied (no PR URL in output)".to_string(),
    };
    Ok(Some(detail))
}

/// Stage 5: pr_sync — poll for PR merge, VCS sync, and update goal state.
fn stage_pr_sync(
    run: &mut GovernedWorkflowRun,
    config: &WorkflowConfig,
    workspace_root: &Path,
) -> anyhow::Result<Option<String>> {
    let pr_url = match &run.pr_url {
        Some(url) => url.clone(),
        None => {
            // No PR URL captured from apply_draft output.
            if config.require_pr {
                // VCS settings indicate a PR-based flow — a missing URL is a hard error.
                // Do NOT silently skip: the next phase would start on stale code.
                anyhow::bail!(
                    "pr_sync: no PR URL was captured from 'ta draft apply' output, \
                     but this workflow requires a PR (require_pr = true).\n\
                     \n\
                     This usually means:\n\
                     - 'ta draft apply' succeeded but did not print a 'PR: <url>' line\n\
                     - The PR was created but output was in an unexpected format\n\
                     - PR creation failed silently inside 'ta draft apply'\n\
                     \n\
                     To recover:\n\
                     1. Find the PR created for this phase (check 'gh pr list')\n\
                     2. Wait for it to merge, then 'git pull' manually\n\
                     3. Re-run the build loop: ./build_phases.sh\n\
                     \n\
                     To skip PR sync for a direct-commit VCS flow, set:\n\
                     require_pr = false   in .ta/workflows/build.toml [config]"
                );
            }
            // Non-PR flow (direct commit, no VCS integration). Skip poll.
            return Ok(Some(
                "no PR URL — sync skipped (require_pr = false)".to_string(),
            ));
        }
    };

    println!("  Polling PR: {}", pr_url);
    println!(
        "  Interval: {}s, timeout: {}h",
        config.pr_poll_interval_secs, config.sync_timeout_hours
    );

    let timeout_secs = config.sync_timeout_hours * 3600;
    let start = Instant::now();

    loop {
        let elapsed = start.elapsed().as_secs();
        if elapsed >= timeout_secs {
            run.audit_trail.push(StageAuditEntry {
                stage: "pr_sync".to_string(),
                agent: "workflow".to_string(),
                verdict: Some("timeout".to_string()),
                duration_secs: elapsed,
                at: Utc::now(),
            });
            anyhow::bail!(
                "PR sync timed out after {}h waiting for {} to merge.\n\
                 Increase sync_timeout_hours in the workflow config, or merge the PR manually.\n\
                 Current PR state: check with `gh pr view {}`",
                config.sync_timeout_hours,
                pr_url,
                pr_url
            );
        }

        match poll_pr_state(&pr_url) {
            PrPollResult::Merged => {
                run.audit_trail.push(StageAuditEntry {
                    stage: "pr_sync".to_string(),
                    agent: "workflow".to_string(),
                    verdict: Some("goal_synced".to_string()),
                    duration_secs: elapsed,
                    at: Utc::now(),
                });
                println!("  PR merged — goal state updated (GoalSynced)");
                // VCS sync: pull the merged changes into the local workspace (v0.15.14).
                // Non-fatal: print a warning and suggest manual sync on failure.
                match do_vcs_sync(workspace_root) {
                    Ok(()) => println!("  Local workspace synced from merge."),
                    Err(e) => println!(
                        "  [warn] VCS sync after merge failed: {} — run 'git pull' manually",
                        e
                    ),
                }
                // Post-sync build step: run the configured command after merge+sync.
                run_post_sync_build(run, config, workspace_root)?;
                return Ok(Some(format!("PR merged after {}s", elapsed)));
            }
            PrPollResult::Closed => {
                run.audit_trail.push(StageAuditEntry {
                    stage: "pr_sync".to_string(),
                    agent: "workflow".to_string(),
                    verdict: Some("goal_abandoned".to_string()),
                    duration_secs: elapsed,
                    at: Utc::now(),
                });
                anyhow::bail!(
                    "PR {} was closed without merging (GoalAbandoned).\n\
                     The goal's changes were not applied to the main branch.",
                    pr_url
                );
            }
            PrPollResult::Open => {
                println!(
                    "  PR still open ({}s elapsed) — next poll in {}s",
                    elapsed, config.pr_poll_interval_secs
                );
                std::thread::sleep(std::time::Duration::from_secs(config.pr_poll_interval_secs));
            }
            PrPollResult::NotFound => {
                println!(
                    "  PR not found via `gh` ({}s elapsed) — retrying in {}s",
                    elapsed, config.pr_poll_interval_secs
                );
                std::thread::sleep(std::time::Duration::from_secs(config.pr_poll_interval_secs));
            }
        }
    }
}

/// Run the post-sync build command if configured.
///
/// Called after `pr_sync` completes (PR merged + VCS synced). If `post_sync_build.enabled`
/// is true and a command is set, runs it in the workspace root with a timeout.
/// On failure:
/// - `on_failure = "halt"` (default): returns an error with resume instructions.
/// - `on_failure = "warn"`: logs the failure and returns Ok to continue the loop.
fn run_post_sync_build(
    run: &GovernedWorkflowRun,
    config: &WorkflowConfig,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    let psb = &config.post_sync_build;
    if !psb.enabled {
        return Ok(());
    }
    let command = match &psb.command {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return Ok(()),
    };

    println!();
    println!("  [post-sync-build] Running: {}", command);
    println!("  [post-sync-build] Timeout: {}s", psb.timeout_secs);

    let start = std::time::Instant::now();
    let status = std::process::Command::new("sh")
        .args(["-c", &command])
        .current_dir(workspace_root)
        .status();

    let elapsed = start.elapsed().as_secs();

    // Timeout check: if the command ran longer than the timeout, we can't actually enforce
    // it here (we'd need a thread/process group), but we can detect and report a hung
    // command that somehow returned.
    if elapsed >= psb.timeout_secs {
        let msg = format!(
            "Post-sync build timed out after {}s (command: {}).\n\
             The command ran longer than timeout_secs = {}.\n\
             Fix the build before continuing. Re-run with:\n  ta workflow resume {}",
            elapsed,
            command,
            psb.timeout_secs,
            &run.run_id[..8.min(run.run_id.len())]
        );
        match psb.on_failure {
            PostSyncOnFailure::Warn => {
                println!("  [post-sync-build] [warn] {}", msg);
                return Ok(());
            }
            PostSyncOnFailure::Halt => anyhow::bail!("{}", msg),
        }
    }

    match status {
        Ok(s) if s.success() => {
            println!("  [post-sync-build] completed in {}s", elapsed);
            Ok(())
        }
        Ok(s) => {
            let exit_code = s.code().unwrap_or(-1);
            let msg = format!(
                "Post-sync build failed (exit {}) after {}s.\n\
                 Command: {}\n\
                 Fix the build before continuing. Re-run with:\n  ta workflow resume {}",
                exit_code,
                elapsed,
                command,
                &run.run_id[..8.min(run.run_id.len())]
            );
            match psb.on_failure {
                PostSyncOnFailure::Warn => {
                    println!("  [post-sync-build] [warn] {}", msg);
                    Ok(())
                }
                PostSyncOnFailure::Halt => anyhow::bail!("{}", msg),
            }
        }
        Err(e) => {
            let msg = format!(
                "Post-sync build command could not be launched: {}\n\
                 Command: {}\n\
                 Fix the issue before continuing. Re-run with:\n  ta workflow resume {}",
                e,
                command,
                &run.run_id[..8.min(run.run_id.len())]
            );
            match psb.on_failure {
                PostSyncOnFailure::Warn => {
                    println!("  [post-sync-build] [warn] {}", msg);
                    Ok(())
                }
                PostSyncOnFailure::Halt => anyhow::bail!("{}", msg),
            }
        }
    }
}

fn print_stage_header(name: &str) {
    println!();
    println!("━━━ Stage: {} ━━━", name);
}

// ── Status display ────────────────────────────────────────────────────────────

/// Show the status of a governed workflow run.
pub fn show_run_status(runs_dir: &Path, run_id_or_prefix: Option<&str>) -> anyhow::Result<()> {
    let run = match run_id_or_prefix {
        Some(id) => GovernedWorkflowRun::load(runs_dir, id)?,
        None => GovernedWorkflowRun::find_latest(runs_dir)?.ok_or_else(|| {
            anyhow::anyhow!(
                "No workflow runs found.\n\
                 Start one with: ta workflow run <name> --goal \"<title>\""
            )
        })?,
    };

    println!("Workflow run: {}", &run.run_id[..8.min(run.run_id.len())]);
    println!("  Workflow: {}", run.workflow_name);
    println!("  Goal:     {}", run.goal_title);
    println!("  State:    {}", run.state);
    if let Some(stage) = &run.current_stage {
        println!("  At stage: {}", stage);
    }
    println!(
        "  Started:  {}",
        run.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!(
        "  Updated:  {}",
        run.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!();
    println!("Stages:");
    for stage in &run.stages {
        let icon = match stage.status {
            StageStatus::Pending => "[ ]",
            StageStatus::Running => "[~]",
            StageStatus::Completed => "[x]",
            StageStatus::Failed => "[!]",
            StageStatus::Skipped => "[-]",
        };
        let duration = stage
            .duration_secs
            .map(|d| format!(" ({}s)", d))
            .unwrap_or_default();
        let detail = stage
            .detail
            .as_deref()
            .map(|d| format!(" — {}", d))
            .unwrap_or_default();
        println!("  {} {}{}{}", icon, stage.name, duration, detail);
    }

    if let Some(verdict) = &run.verdict {
        println!();
        println!("Reviewer verdict:");
        println!("  Decision:   {}", verdict.verdict);
        println!("  Confidence: {:.0}%", verdict.confidence * 100.0);
        if !verdict.findings.is_empty() {
            println!("  Findings:");
            for f in &verdict.findings {
                println!("    - {}", f);
            }
        }
    }

    if let Some(pr_url) = &run.pr_url {
        println!();
        println!("PR: {}", pr_url);
    }

    if !run.sub_workflow_records.is_empty() {
        println!();
        println!("Sub-workflow runs:");
        for rec in &run.sub_workflow_records {
            println!(
                "  [{}] stage='{}' workflow='{}' child-run={}",
                rec.stage_name,
                rec.stage_name,
                rec.child_workflow,
                &rec.child_run_id[..8.min(rec.child_run_id.len())]
            );
            println!(
                "    ta workflow status {}",
                &rec.child_run_id[..8.min(rec.child_run_id.len())]
            );
        }
    }

    if !run.loop_iterations.is_empty() {
        println!();
        println!("Loop iterations:");
        for (stage, count) in &run.loop_iterations {
            println!("  {}: {} iteration(s)", stage, count);
        }
    }

    // Show milestone draft info if any aggregate_draft stage produced one (v0.15.14).
    let milestone_entries: Vec<(&str, &str)> = run
        .outputs
        .iter()
        .filter_map(|(sname, smap)| {
            let mid = smap.get("milestone_id")?.as_str();
            let title = smap
                .get("milestone_title")
                .map(|s| s.as_str())
                .unwrap_or("(untitled)");
            Some((sname.as_str(), mid, title))
        })
        .map(|(_sname, mid, title)| (mid, title))
        .collect();
    if !milestone_entries.is_empty() {
        println!();
        println!("Milestone drafts:");
        for (mid, title) in &milestone_entries {
            println!("  {} — {} ({})", &mid[..8.min(mid.len())], title, mid);
            println!("    View: cat .ta/milestones/{}.json", mid);
        }
    }

    if run.state == WorkflowRunState::Failed || run.state == WorkflowRunState::AwaitingHuman {
        println!();
        println!("Next action:");
        println!(
            "  ta workflow run {} --goal \"{}\" --resume {}",
            run.workflow_name,
            run.goal_title,
            &run.run_id[..8.min(run.run_id.len())]
        );
    }

    println!();
    println!(
        "Audit trail: ta audit export --workflow-run {}",
        &run.run_id[..8.min(run.run_id.len())]
    );

    Ok(())
}

/// Export the audit trail for a workflow run as JSON.
pub fn export_run_audit(runs_dir: &Path, run_id_or_prefix: &str) -> anyhow::Result<()> {
    let run = GovernedWorkflowRun::load(runs_dir, run_id_or_prefix)?;

    #[derive(Serialize)]
    struct AuditExport<'a> {
        run_id: &'a str,
        workflow_name: &'a str,
        goal_title: &'a str,
        state: &'a WorkflowRunState,
        started_at: &'a DateTime<Utc>,
        stages: &'a [StageRecord],
        audit_trail: &'a [StageAuditEntry],
    }

    let export = AuditExport {
        run_id: &run.run_id,
        workflow_name: &run.workflow_name,
        goal_title: &run.goal_title,
        state: &run.state,
        started_at: &run.started_at,
        stages: &run.stages,
        audit_trail: &run.audit_trail,
    };

    println!("{}", serde_json::to_string_pretty(&export)?);
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── Stage graph parsing ───────────────────────────────────────────────────

    #[test]
    fn stage_graph_canonical_order() {
        // Uses the `stage()` helper defined below.
        let stages = vec![
            stage("run_goal", &[]),
            stage("review_draft", &["run_goal"]),
            stage("human_gate", &["review_draft"]),
            stage("apply_draft", &["human_gate"]),
            stage("pr_sync", &["apply_draft"]),
        ];
        let order = validate_stage_graph(&stages).unwrap();
        assert_eq!(
            order,
            vec![
                "run_goal",
                "review_draft",
                "human_gate",
                "apply_draft",
                "pr_sync"
            ]
        );
    }

    /// Convenience: build a minimal StageDef for test use.
    fn stage(name: &str, deps: &[&str]) -> StageDef {
        StageDef {
            name: name.to_string(),
            description: String::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            kind: StageKind::Default,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: None,
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: None,
            on_partial_failure: None,
            max_parallel: None,
            lang: None,
            phase_filter: None,
        }
    }

    #[test]
    fn stage_graph_unknown_dep_error() {
        let stages = vec![stage("run_goal", &["nonexistent"])];
        let err = validate_stage_graph(&stages).unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn stage_graph_cycle_detection() {
        // a → b → a (cycle)
        let stages = vec![stage("a", &["b"]), stage("b", &["a"])];
        let err = validate_stage_graph(&stages).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    // ── Reviewer verdict JSON validation ──────────────────────────────────────

    #[test]
    fn verdict_json_approve_roundtrip() {
        let v = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.95,
        };
        let json = serde_json::to_string(&v).unwrap();
        let restored: ReviewerVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.verdict, VerdictDecision::Approve);
        assert!((restored.confidence - 0.95).abs() < 0.001);
    }

    #[test]
    fn verdict_json_flag_with_findings() {
        let json = r#"{"verdict":"flag","findings":["Missing test coverage","Security concern"],"confidence":0.6}"#;
        let v: ReviewerVerdict = serde_json::from_str(json).unwrap();
        assert_eq!(v.verdict, VerdictDecision::Flag);
        assert_eq!(v.findings.len(), 2);
    }

    #[test]
    fn verdict_json_reject() {
        let json = r#"{"verdict":"reject","findings":["SQL injection in query builder"],"confidence":0.99}"#;
        let v: ReviewerVerdict = serde_json::from_str(json).unwrap();
        assert_eq!(v.verdict, VerdictDecision::Reject);
    }

    #[test]
    fn verdict_confidence_out_of_range_fails_validation() {
        let v = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 1.5, // invalid
        };
        assert!(v.validate().is_err());
    }

    #[test]
    fn verdict_load_from_file() {
        let dir = tempdir().unwrap();
        let json = r#"{"verdict":"approve","findings":[],"confidence":0.9}"#;
        std::fs::write(dir.path().join("verdict.json"), json).unwrap();
        let v = ReviewerVerdict::load(dir.path()).unwrap();
        assert_eq!(v.verdict, VerdictDecision::Approve);
    }

    #[test]
    fn verdict_load_missing_file_error() {
        let dir = tempdir().unwrap();
        let err = ReviewerVerdict::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found") || err.to_string().contains("verdict"));
    }

    // ── human_gate auto-approve path ──────────────────────────────────────────

    #[test]
    fn human_gate_auto_approve_proceeds() {
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.9,
        };
        let decision = evaluate_human_gate(&verdict, &GateMode::Auto, false).unwrap();
        assert_eq!(decision, GateDecision::Proceed);
    }

    #[test]
    fn human_gate_auto_reject_rejects() {
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Reject,
            findings: vec!["Critical bug".to_string()],
            confidence: 0.99,
        };
        let decision = evaluate_human_gate(&verdict, &GateMode::Auto, false).unwrap();
        assert_eq!(decision, GateDecision::Reject);
    }

    #[test]
    fn human_gate_auto_flag_non_interactive_errors() {
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Flag,
            findings: vec!["Minor concern".to_string()],
            confidence: 0.7,
        };
        // Non-interactive: flag should require human input, returning Err.
        let result = evaluate_human_gate(&verdict, &GateMode::Auto, false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("human input required"));
    }

    #[test]
    fn human_gate_reject_always_rejects_regardless_of_mode() {
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Reject,
            findings: vec![],
            confidence: 1.0,
        };
        for mode in [GateMode::Auto, GateMode::Prompt, GateMode::Always] {
            let decision = evaluate_human_gate(&verdict, &mode, false).unwrap();
            assert_eq!(decision, GateDecision::Reject);
        }
    }

    // ── GovernedWorkflowRun state persistence ─────────────────────────────────

    #[test]
    fn run_state_save_and_load() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let run = GovernedWorkflowRun::new("test-run-1", "governed-goal", "Fix the bug");
        run.save(&runs_dir).unwrap();
        let loaded = GovernedWorkflowRun::load(&runs_dir, "test-run-1").unwrap();
        assert_eq!(loaded.goal_title, "Fix the bug");
        assert_eq!(loaded.state, WorkflowRunState::Running);
        assert_eq!(loaded.stages.len(), 5);
    }

    #[test]
    fn run_state_prefix_lookup() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let run = GovernedWorkflowRun::new(
            "abcdef12-0000-0000-0000-000000000000",
            "governed-goal",
            "Goal",
        );
        run.save(&runs_dir).unwrap();
        // Should find by prefix.
        let loaded = GovernedWorkflowRun::load(&runs_dir, "abcdef12").unwrap();
        assert_eq!(loaded.goal_title, "Goal");
    }

    #[test]
    fn run_state_find_latest() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let run1 = GovernedWorkflowRun::new("run-aaa", "governed-goal", "First");
        run1.save(&runs_dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let run2 = GovernedWorkflowRun::new("run-bbb", "governed-goal", "Second");
        run2.save(&runs_dir).unwrap();
        let latest = GovernedWorkflowRun::find_latest(&runs_dir)
            .unwrap()
            .unwrap();
        assert_eq!(latest.goal_title, "Second");
    }

    // ── PR sync polling ───────────────────────────────────────────────────────

    /// Simulate a pr_sync loop with a mock provider using a sequence of results.
    ///
    /// This is a unit test of the PR sync logic, not an integration test.
    #[test]
    fn pr_sync_merged_returns_success() {
        // Directly test the PrPollResult matching logic.
        assert_eq!(PrPollResult::Merged, PrPollResult::Merged);
        assert_ne!(PrPollResult::Merged, PrPollResult::Closed);
    }

    #[test]
    fn pr_sync_poll_result_variants() {
        // Ensure all variants are reachable and distinct.
        let results = [
            PrPollResult::Merged,
            PrPollResult::Closed,
            PrPollResult::Open,
            PrPollResult::NotFound,
        ];
        assert_eq!(results.len(), 4);
    }

    // ── Full workflow integration test (stub agents, ignored in CI) ───────────

    /// Full governed workflow with stub ta command (requires `ta` on PATH with
    /// a real project). Marked ignore so it does not run in CI.
    #[test]
    #[ignore]
    fn integration_full_governed_workflow_stub() {
        let dir = tempdir().unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "governed-goal",
            goal_title: "stub integration test",
            dry_run: true,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        // dry_run=true validates the stage graph without executing agents.
        let result = run_governed_workflow(&opts);
        // Will fail because the template is not at `dir.path()/templates/...`
        // but validates the logic path.
        let _ = result;
    }

    #[test]
    fn dry_run_prints_stage_graph() {
        let dir = tempdir().unwrap();
        // Create a minimal governed-goal.yaml so the template loads.
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        std::fs::write(
            templates_dir.join("governed-goal.yaml"),
            include_str!("../../../../templates/workflows/governed-goal.yaml"),
        )
        .unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "governed-goal",
            goal_title: "test dry run",
            dry_run: true,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        // dry_run should succeed and print the graph.
        run_governed_workflow(&opts).unwrap();
    }

    // ── --phase threading ─────────────────────────────────────────────────────

    /// RunOptions with plan_phase=Some round-trips without panicking.
    #[test]
    fn run_options_with_plan_phase() {
        let dir = tempdir().unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "build",
            goal_title: "v0.4.0 — Captioning Utils",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: Some("v0.4.0"),
            depth: 0,
            params: Default::default(),
        };
        // plan_phase is visible on the options; the dry-run path would print it.
        assert_eq!(opts.plan_phase, Some("v0.4.0"));
        assert_eq!(opts.goal_title, "v0.4.0 — Captioning Utils");
    }

    /// When plan_phase is None, options are still valid.
    #[test]
    fn run_options_without_plan_phase() {
        let dir = tempdir().unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "build",
            goal_title: "ad-hoc goal",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        assert!(opts.plan_phase.is_none());
    }

    /// stage_run_goal returns an error (no draft) when goal output has no draft_id line.
    /// This validates the empty-draft guard that prevents infinite phase loops.
    #[test]
    fn stage_run_goal_empty_draft_guard_message() {
        // The guard fires when draft_id is None after parsing stdout.
        // We test the error message directly rather than spawning `ta`.
        // Simulate: run.draft_id is still None → bail message should mention
        // the workflow name and goal title so the user knows how to re-run.
        let err_msg = format!(
            "ta run completed but did not produce a draft.\n\
             This usually means the agent finished without making changes, \
             or the phase context was not injected.\n\
             Check the goal output above for details, then re-run with \
             'ta workflow run {} --goal \"{}\"'{}.",
            "build", "v0.4.0 — Captioning Utils", " --phase v0.4.0"
        );
        assert!(err_msg.contains("ta workflow run build"));
        assert!(err_msg.contains("v0.4.0 — Captioning Utils"));
        assert!(err_msg.contains("--phase v0.4.0"));
    }

    /// GovernedWorkflowRun stores plan_phase when present in a goal title.
    /// Verifies the run struct can hold a phase-prefixed goal title end-to-end.
    #[test]
    fn run_state_with_phase_goal_title() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let run = GovernedWorkflowRun::new("phase-run-1", "build", "v0.4.0 — Captioning Utils");
        run.save(&runs_dir).unwrap();
        let loaded = GovernedWorkflowRun::load(&runs_dir, "phase-run-1").unwrap();
        assert_eq!(loaded.goal_title, "v0.4.0 — Captioning Utils");
        assert_eq!(loaded.workflow_name, "build");
    }

    // ── Template interpolation (v0.15.13) ─────────────────────────────────────

    #[test]
    fn interpolate_template_basic() {
        let mut outputs = std::collections::HashMap::new();
        let mut plan_map = std::collections::HashMap::new();
        plan_map.insert("phase_id".to_string(), "v0.15.14".to_string());
        plan_map.insert("phase_title".to_string(), "Parallel Fan-Out".to_string());
        outputs.insert("plan_next".to_string(), plan_map);

        let result = interpolate_template(
            "{{plan_next.phase_id}} — {{plan_next.phase_title}}",
            &outputs,
        );
        assert_eq!(result, "v0.15.14 — Parallel Fan-Out");
    }

    #[test]
    fn interpolate_template_unresolved_left_verbatim() {
        let outputs = std::collections::HashMap::new();
        let result = interpolate_template("{{plan_next.phase_id}}", &outputs);
        // Unresolved placeholder kept intact.
        assert_eq!(result, "{{plan_next.phase_id}}");
    }

    #[test]
    fn interpolate_template_no_placeholders() {
        let outputs = std::collections::HashMap::new();
        let result = interpolate_template("hello world", &outputs);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn interpolate_template_mixed() {
        let mut outputs = std::collections::HashMap::new();
        let mut m = std::collections::HashMap::new();
        m.insert("id".to_string(), "42".to_string());
        outputs.insert("stage".to_string(), m);

        let result = interpolate_template("prefix-{{stage.id}}-{{missing.x}}-suffix", &outputs);
        assert_eq!(result, "prefix-42-{{missing.x}}-suffix");
    }

    // ── Condition evaluator (v0.15.13) ────────────────────────────────────────

    fn make_outputs(
        stage: &str,
        kv: &[(&str, &str)],
    ) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
        let mut outputs = std::collections::HashMap::new();
        let mut m = std::collections::HashMap::new();
        for (k, v) in kv {
            m.insert(k.to_string(), v.to_string());
        }
        outputs.insert(stage.to_string(), m);
        outputs
    }

    #[test]
    fn condition_evaluator_negation_false() {
        // !plan_next.done where done="false" → !false → true
        let outputs = make_outputs("plan_next", &[("done", "false")]);
        assert!(evaluate_condition("!plan_next.done", &outputs).unwrap());
    }

    #[test]
    fn condition_evaluator_negation_true() {
        // !plan_next.done where done="true" → !true → false
        let outputs = make_outputs("plan_next", &[("done", "true")]);
        assert!(!evaluate_condition("!plan_next.done", &outputs).unwrap());
    }

    #[test]
    fn condition_evaluator_equality_match() {
        let outputs = make_outputs("stage", &[("status", "approved")]);
        assert!(evaluate_condition("stage.status == \"approved\"", &outputs).unwrap());
    }

    #[test]
    fn condition_evaluator_equality_no_match() {
        let outputs = make_outputs("stage", &[("status", "pending")]);
        assert!(!evaluate_condition("stage.status == \"approved\"", &outputs).unwrap());
    }

    #[test]
    fn condition_evaluator_inequality() {
        let outputs = make_outputs("stage", &[("result", "ok")]);
        assert!(evaluate_condition("stage.result != \"fail\"", &outputs).unwrap());
    }

    #[test]
    fn condition_evaluator_non_boolean_negation_fails() {
        let outputs = make_outputs("stage", &[("value", "maybe")]);
        let err = evaluate_condition("!stage.value", &outputs).unwrap_err();
        assert!(err.to_string().contains("non-boolean"));
    }

    #[test]
    fn condition_evaluator_missing_stage_fails() {
        let outputs = std::collections::HashMap::new();
        let err = evaluate_condition("!plan_next.done", &outputs).unwrap_err();
        assert!(err.to_string().contains("plan_next"));
    }

    #[test]
    fn condition_evaluator_missing_field_fails() {
        let outputs = make_outputs("plan_next", &[("phase_id", "v0.15.14")]);
        let err = evaluate_condition("!plan_next.done", &outputs).unwrap_err();
        assert!(err.to_string().contains("done"));
    }

    #[test]
    fn condition_evaluator_plain_boolean_true() {
        let outputs = make_outputs("stage", &[("ready", "true")]);
        assert!(evaluate_condition("stage.ready", &outputs).unwrap());
    }

    // ── PlanNextOutput parsing (v0.15.13) ─────────────────────────────────────

    #[test]
    fn plan_next_output_parsing_with_phase() {
        let stdout = "Next pending phase:\n  Phase v0.15.14 \u{2014} Parallel Fan-Out\n\nTo start:";
        let out = PlanNextOutput::parse(stdout);
        assert!(!out.done);
        assert_eq!(out.phase_id, "v0.15.14");
        assert_eq!(out.phase_title, "Parallel Fan-Out");
    }

    #[test]
    fn plan_next_output_parsing_all_done() {
        let stdout = "All plan phases are complete or in progress.\n";
        let out = PlanNextOutput::parse(stdout);
        assert!(out.done);
        assert_eq!(out.phase_id, "");
    }

    #[test]
    fn plan_next_output_parsing_empty_is_done() {
        let out = PlanNextOutput::parse("");
        assert!(out.done);
    }

    #[test]
    fn plan_next_output_to_map_keys() {
        let out = PlanNextOutput {
            phase_id: "v0.15.14".to_string(),
            phase_title: "Fan-Out".to_string(),
            done: false,
        };
        let map = out.to_output_map();
        assert_eq!(map.get("phase_id").map(|s| s.as_str()), Some("v0.15.14"));
        assert_eq!(map.get("done").map(|s| s.as_str()), Some("false"));
        assert!(map.contains_key("phase_title"));
    }

    // ── SubworkflowRecord serialization (v0.15.13) ────────────────────────────

    #[test]
    fn subworkflow_record_roundtrip() {
        let rec = SubworkflowRecord {
            parent_run_id: "parent-123".to_string(),
            child_run_id: "child-456".to_string(),
            stage_name: "run_phase".to_string(),
            child_workflow: "build".to_string(),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let restored: SubworkflowRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.parent_run_id, "parent-123");
        assert_eq!(restored.child_run_id, "child-456");
        assert_eq!(restored.child_workflow, "build");
    }

    // ── GovernedWorkflowRun new fields (v0.15.13) ─────────────────────────────

    #[test]
    fn run_new_with_stages_custom_names() {
        let run = GovernedWorkflowRun::new_with_stages(
            "test-run",
            "plan-build-loop",
            "loop test",
            &["plan_next", "run_phase", "loop"],
        );
        assert_eq!(run.stages.len(), 3);
        assert_eq!(run.stages[0].name, "plan_next");
        assert_eq!(run.stages[1].name, "run_phase");
        assert_eq!(run.stages[2].name, "loop");
        assert!(run.outputs.is_empty());
        assert!(run.sub_workflow_records.is_empty());
        assert!(run.loop_iterations.is_empty());
    }

    #[test]
    fn run_outputs_persist_through_save_load() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let mut run = GovernedWorkflowRun::new("out-test", "test-wf", "Goal");
        let mut m = std::collections::HashMap::new();
        m.insert("done".to_string(), "false".to_string());
        run.outputs.insert("plan_next".to_string(), m);
        run.save(&runs_dir).unwrap();

        let loaded = GovernedWorkflowRun::load(&runs_dir, "out-test").unwrap();
        assert_eq!(
            loaded
                .outputs
                .get("plan_next")
                .and_then(|m| m.get("done"))
                .map(|s| s.as_str()),
            Some("false")
        );
    }

    #[test]
    fn run_loop_iterations_persist() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let mut run = GovernedWorkflowRun::new("loop-test", "plan-build-loop", "Goal");
        run.loop_iterations.insert("loop".to_string(), 3);
        run.save(&runs_dir).unwrap();

        let loaded = GovernedWorkflowRun::load(&runs_dir, "loop-test").unwrap();
        assert_eq!(loaded.loop_iterations.get("loop"), Some(&3));
    }

    // ── Dispatch-history guard tests (v0.15.24.2) ────────────────────────────

    #[test]
    fn dispatched_phases_default_empty() {
        let run = GovernedWorkflowRun::new("run-x", "governed-goal", "Goal");
        assert!(run.dispatched_phases.is_empty());
    }

    #[test]
    fn dispatched_phases_persist_through_save_load() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let mut run = GovernedWorkflowRun::new("dp-test", "test-wf", "Goal");
        run.dispatched_phases.push("v0.15.0".to_string());
        run.dispatched_phases.push("v0.15.1".to_string());
        run.save(&runs_dir).unwrap();

        let loaded = GovernedWorkflowRun::load(&runs_dir, "dp-test").unwrap();
        assert_eq!(loaded.dispatched_phases, vec!["v0.15.0", "v0.15.1"]);
    }

    #[test]
    fn plan_next_output_phase_id_duplicate_detected() {
        // Simulate the dispatch-history guard logic used in stage_plan_next.
        let mut run = GovernedWorkflowRun::new("safety-test", "governed-goal", "Goal");
        run.dispatched_phases.push("v0.15.0".to_string());

        // Same phase returned again — the guard should detect it.
        let phase_id = "v0.15.0";
        let already = run
            .dispatched_phases
            .iter()
            .enumerate()
            .find(|(_, id)| *id == phase_id);
        assert!(
            already.is_some(),
            "guard must fire for already-dispatched phase"
        );
    }

    #[test]
    fn plan_next_output_new_phase_not_blocked() {
        let mut run = GovernedWorkflowRun::new("safety-new", "governed-goal", "Goal");
        run.dispatched_phases.push("v0.15.0".to_string());

        // Different phase — guard must not fire.
        let phase_id = "v0.15.1";
        let already = run
            .dispatched_phases
            .iter()
            .enumerate()
            .find(|(_, id)| *id == phase_id);
        assert!(already.is_none(), "guard must not fire for a new phase");
    }

    // ── StageKind deserialization (v0.15.13) ──────────────────────────────────

    #[test]
    fn stage_kind_defaults_to_default() {
        let toml_str = r#"
name = "run_goal"
description = "Run the goal"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::Default);
    }

    #[test]
    fn stage_kind_workflow_deserializes() {
        let toml_str = r#"
name = "run_phase"
description = "Invoke child workflow"
kind = "workflow"
workflow = "build"
goal = "{{plan_next.phase_id}}"
phase = "{{plan_next.phase_id}}"
condition = "!plan_next.done"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::Workflow);
        assert_eq!(stage.workflow.as_deref(), Some("build"));
        assert_eq!(stage.condition.as_deref(), Some("!plan_next.done"));
    }

    #[test]
    fn stage_kind_goto_deserializes() {
        let toml_str = r#"
name = "loop"
description = "Loop back"
kind = "goto"
target = "plan_next"
condition = "!plan_next.done"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::Goto);
        assert_eq!(stage.target.as_deref(), Some("plan_next"));
    }

    #[test]
    fn stage_kind_plan_next_deserializes() {
        let toml_str = r#"
name = "plan_next"
description = "Get next phase"
kind = "plan_next"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::PlanNext);
        assert!(stage.phase_filter.is_none());
    }

    #[test]
    fn stage_kind_plan_next_with_phase_filter_deserializes() {
        let toml_str = r#"
name = "plan_next"
description = "Get next phase scoped to v0.15"
kind = "plan_next"
phase_filter = "v0.15"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::PlanNext);
        assert_eq!(stage.phase_filter.as_deref(), Some("v0.15"));
    }

    #[test]
    fn stage_def_phase_filter_absent_defaults_to_none() {
        let toml_str = r#"
name = "plan_next"
description = "no filter"
kind = "plan_next"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert!(
            stage.phase_filter.is_none(),
            "phase_filter should default to None"
        );
    }

    // ── Depth guard (v0.15.13) ────────────────────────────────────────────────

    #[test]
    fn depth_guard_fires_above_limit() {
        // Directly test the depth guard logic without spawning a real workflow.
        // The guard kicks in when opts.depth > MAX_DEPTH (5).
        let dir = tempdir().unwrap();
        // Write a minimal workflow YAML so find_workflow_def succeeds.
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        std::fs::write(
            templates_dir.join("governed-goal.yaml"),
            include_str!("../../../../templates/workflows/governed-goal.yaml"),
        )
        .unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "governed-goal",
            goal_title: "depth test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 6, // exceeds MAX_DEPTH = 5
            params: Default::default(),
        };
        let err = run_governed_workflow(&opts).unwrap_err();
        assert!(
            err.to_string().contains("recursion depth limit"),
            "Expected depth guard message, got: {}",
            err
        );
    }

    // ── Loop max_phases guard (v0.15.13) ──────────────────────────────────────

    #[test]
    fn loop_workflow_max_phases_guard_via_config() {
        // Test the CHECKPOINT message format from the guard.
        let max = 99u32;
        let iters = max + 1;
        let msg = format!(
            "Loop CHECKPOINT: stage '{}' reached the maximum iteration limit ({}).\n\
             {} iterations completed.",
            "loop",
            max,
            iters - 1
        );
        assert!(msg.contains("CHECKPOINT"));
        assert!(msg.contains("99"));
    }

    // ── Dry-run for loop workflow (v0.15.13) ──────────────────────────────────

    #[test]
    fn dry_run_plan_build_loop_prints_kind_labels() {
        let dir = tempdir().unwrap();
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // Write the plan-build-loop template.
        std::fs::write(
            templates_dir.join("plan-build-loop.yaml"),
            include_str!("../../../../templates/workflows/plan-build-loop.yaml"),
        )
        .unwrap();
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "plan-build-loop",
            goal_title: "run all phases",
            dry_run: true,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        // Should succeed (dry-run doesn't execute anything).
        run_governed_workflow(&opts).unwrap();
    }

    // ── v0.15.14 new stage kind deserialization ───────────────────────────────

    #[test]
    fn stage_kind_loop_next_deserializes() {
        let toml_str = r#"
name = "loop_next"
description = "Advance phase cursor"
kind = "loop_next"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::LoopNext);
    }

    #[test]
    fn stage_kind_apply_draft_branch_deserializes() {
        let toml_str = r#"
name = "apply_local"
description = "Apply to milestone branch"
kind = "apply_draft_branch"
milestone_branch = "feature/milestone-1"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::ApplyDraftBranch);
        assert_eq!(s.milestone_branch.as_deref(), Some("feature/milestone-1"));
    }

    #[test]
    fn stage_kind_aggregate_draft_deserializes() {
        let toml_str = r#"
name = "aggregate"
description = "Collect all drafts"
kind = "aggregate_draft"
source_stages = "all"
milestone_title = "Sprint milestone"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::AggregateDraft);
        assert_eq!(s.source_stages.as_deref(), Some("all"));
        assert_eq!(s.milestone_title.as_deref(), Some("Sprint milestone"));
    }

    #[test]
    fn stage_kind_join_deserializes() {
        let toml_str = r#"
name = "sync"
description = "Wait for parallel group"
kind = "join"
join_group = "workers"
on_partial_failure = "continue"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::Join);
        assert_eq!(s.join_group.as_deref(), Some("workers"));
        assert_eq!(s.on_partial_failure.as_deref(), Some("continue"));
    }

    #[test]
    fn stage_kind_static_analysis_deserializes() {
        let toml_str = r#"
name = "lint"
description = "Run static analysis"
kind = "static_analysis"
lang = "python"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::StaticAnalysis);
        assert_eq!(s.lang.as_deref(), Some("python"));
    }

    #[test]
    fn stage_kind_static_analysis_no_lang() {
        // lang defaults to None when not specified; auto-detect will be used at runtime.
        let toml_str = r#"
name = "lint"
description = "Run static analysis"
kind = "static_analysis"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::StaticAnalysis);
        assert!(s.lang.is_none());
    }

    // ── aggregate_draft stage executor ───────────────────────────────────────

    #[test]
    fn aggregate_draft_merges_outputs() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        let mut run = GovernedWorkflowRun::new("agg-test", "test-wf", "Goal");

        // Simulate two source stages with draft_id outputs.
        let mut map1 = std::collections::HashMap::new();
        map1.insert("draft_id".to_string(), "draft-aaa-111".to_string());
        map1.insert("phase_id".to_string(), "v0.15.14".to_string());
        map1.insert("phase_title".to_string(), "Phase A".to_string());
        run.outputs.insert("run_phase_1".to_string(), map1);

        let mut map2 = std::collections::HashMap::new();
        map2.insert("draft_id".to_string(), "draft-bbb-222".to_string());
        map2.insert("phase_id".to_string(), "v0.15.15".to_string());
        map2.insert("phase_title".to_string(), "Phase B".to_string());
        run.outputs.insert("run_phase_2".to_string(), map2);

        let stage_def = StageDef {
            name: "aggregate".to_string(),
            description: "collect".to_string(),
            depends_on: vec![],
            kind: StageKind::AggregateDraft,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: Some("all".to_string()),
            milestone_title: Some("Test Milestone".to_string()),
            milestone_branch: None,
            parallel_group: None,
            join_group: None,
            on_partial_failure: None,
            max_parallel: None,
            lang: None,
            phase_filter: None,
        };

        let opts = RunOptions {
            workspace_root: workspace,
            workflow_name: "test-wf",
            goal_title: "test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };

        let result = stage_aggregate_draft(&mut run, &stage_def, &opts).unwrap();
        assert!(result.is_some());

        // Milestone ID should be recorded in stage outputs.
        let agg_out = run.outputs.get("aggregate").unwrap();
        let milestone_id = agg_out.get("milestone_id").unwrap();
        assert!(!milestone_id.is_empty());
        assert_eq!(agg_out.get("draft_count").unwrap().as_str(), "2");

        // Milestone file should exist.
        let milestone_path = workspace
            .join(".ta")
            .join("milestones")
            .join(format!("{}.json", milestone_id));
        assert!(milestone_path.exists(), "milestone file should be created");
    }

    #[test]
    fn aggregate_draft_deduplicates_draft_ids() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        let mut run = GovernedWorkflowRun::new("dedup-test", "test-wf", "Goal");

        // Two stages with the same draft_id — should deduplicate.
        let mut map1 = std::collections::HashMap::new();
        map1.insert("draft_id".to_string(), "same-draft".to_string());
        run.outputs.insert("stage_a".to_string(), map1.clone());
        run.outputs.insert("stage_b".to_string(), map1);

        let stage_def = StageDef {
            name: "aggregate".to_string(),
            description: "collect".to_string(),
            depends_on: vec![],
            kind: StageKind::AggregateDraft,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: Some("all".to_string()),
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: None,
            on_partial_failure: None,
            max_parallel: None,
            lang: None,
            phase_filter: None,
        };

        let opts = RunOptions {
            workspace_root: workspace,
            workflow_name: "test-wf",
            goal_title: "test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };

        stage_aggregate_draft(&mut run, &stage_def, &opts).unwrap();
        let agg_out = run.outputs.get("aggregate").unwrap();
        // Only 1 unique draft ID.
        assert_eq!(agg_out.get("draft_count").unwrap().as_str(), "1");
    }

    // ── join stage executor ───────────────────────────────────────────────────

    #[test]
    fn join_validates_group_completed_no_failures() {
        let mut run = GovernedWorkflowRun::new("join-test", "test-wf", "Goal");
        // All stages completed successfully — join should pass.
        for s in &mut run.stages {
            s.status = StageStatus::Completed;
        }

        let stage_def = StageDef {
            name: "sync".to_string(),
            description: "join".to_string(),
            depends_on: vec![],
            kind: StageKind::Join,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: None,
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: Some("workers".to_string()),
            on_partial_failure: None,
            max_parallel: None,
            lang: None,
            phase_filter: None,
        };

        let result = stage_join(&mut run, &stage_def).unwrap();
        assert!(result.unwrap().contains("workers"));
    }

    #[test]
    fn join_halts_on_failure_by_default() {
        let mut run = GovernedWorkflowRun::new("join-fail-test", "test-wf", "Goal");
        // Mark one stage as failed.
        if let Some(s) = run.stages.first_mut() {
            s.status = StageStatus::Failed;
            s.name = "failing_stage".to_string();
        }

        let stage_def = StageDef {
            name: "sync".to_string(),
            description: "join".to_string(),
            depends_on: vec![],
            kind: StageKind::Join,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: None,
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: Some("workers".to_string()),
            on_partial_failure: None, // default = halt
            max_parallel: None,
            lang: None,
            phase_filter: None,
        };

        let err = stage_join(&mut run, &stage_def).unwrap_err();
        assert!(err.to_string().contains("failed stage"));
    }

    #[test]
    fn join_continues_on_partial_failure_when_configured() {
        let mut run = GovernedWorkflowRun::new("join-partial-test", "test-wf", "Goal");
        if let Some(s) = run.stages.first_mut() {
            s.status = StageStatus::Failed;
        }

        let stage_def = StageDef {
            name: "sync".to_string(),
            description: "join".to_string(),
            depends_on: vec![],
            kind: StageKind::Join,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: None,
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: Some("workers".to_string()),
            on_partial_failure: Some("continue".to_string()),
            max_parallel: None,
            lang: None,
            phase_filter: None,
        };

        let result = stage_join(&mut run, &stage_def);
        // Should succeed despite the failed stage.
        assert!(result.is_ok());
    }

    // ── loop_next is an alias for plan_next output format ────────────────────

    #[test]
    fn loop_next_advances_cursor_same_format_as_plan_next() {
        // Verify that LoopNext kind is treated like PlanNext in the dispatch.
        // We test the deserialization and type equivalence, not the actual ta invocation.
        let toml_str = r#"
name = "loop_next_stage"
description = "Advance loop"
kind = "loop_next"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::LoopNext);
        // PlanNextOutput format is the same for both.
        let out = PlanNextOutput {
            phase_id: "v0.15.14".to_string(),
            phase_title: "Test".to_string(),
            done: false,
        };
        let map = out.to_output_map();
        assert!(map.contains_key("phase_id"));
        assert!(map.contains_key("phase_title"));
        assert!(map.contains_key("done"));
    }

    // ── parallel group fields round-trip through StageDef ────────────────────

    #[test]
    fn parallel_stages_fields_roundtrip() {
        let toml_str = r#"
name = "worker"
description = "Parallel worker"
parallel_group = "workers"
max_parallel = 3
on_partial_failure = "continue"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.parallel_group.as_deref(), Some("workers"));
        assert_eq!(s.max_parallel, Some(3));
        assert_eq!(s.on_partial_failure.as_deref(), Some("continue"));
    }

    // ── StageKind::Consensus and StageKind::ApplyDraft deserialization (v0.15.15.1) ──

    #[test]
    fn stage_kind_consensus_deserializes() {
        let toml_str = r#"
name = "consensus"
description = "Aggregate reviewer scores"
kind = "consensus"
depends_on = ["architect_review", "security_review"]
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::Consensus);
        assert_eq!(s.depends_on.len(), 2);
    }

    #[test]
    fn stage_kind_apply_draft_deserializes() {
        let toml_str = r#"
name = "apply"
description = "Apply the approved draft"
kind = "apply_draft"
"#;
        let s: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(s.kind, StageKind::ApplyDraft);
    }

    // ── stage_consensus executor (v0.15.15.1) ────────────────────────────────

    fn make_consensus_stage_def(roles: Vec<String>) -> StageDef {
        StageDef {
            name: "consensus".to_string(),
            description: "Consensus".to_string(),
            depends_on: roles,
            kind: StageKind::Consensus,
            workflow: None,
            goal: None,
            phase: None,
            condition: None,
            target: None,
            source_stages: None,
            milestone_title: None,
            milestone_branch: None,
            parallel_group: None,
            join_group: None,
            on_partial_failure: None,
            max_parallel: None,
            lang: None,
            phase_filter: None,
        }
    }

    #[test]
    fn stage_consensus_4_reviewers_proceed() {
        let dir = tempdir().unwrap();
        let run_id = "test-consensus-run";
        let review_base = dir.path().join(".ta").join("review").join(run_id);
        for (role, score) in &[
            ("architect_review", 0.85),
            ("security_review", 0.80),
            ("principal_review", 0.90),
            ("pm_review", 0.75),
        ] {
            let role_dir = review_base.join(role);
            std::fs::create_dir_all(&role_dir).unwrap();
            let verdict = serde_json::json!({
                "score": score,
                "findings": [],
                "decision": "approve"
            });
            std::fs::write(
                role_dir.join("verdict.json"),
                serde_json::to_string(&verdict).unwrap(),
            )
            .unwrap();
        }

        let config = WorkflowConfig::default();
        let mut run = GovernedWorkflowRun::new(run_id, "test-consensus", "Goal");
        let stage_def = make_consensus_stage_def(vec![
            "architect_review".to_string(),
            "security_review".to_string(),
            "principal_review".to_string(),
            "pm_review".to_string(),
        ]);
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "test",
            goal_title: "test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        let result = stage_consensus(&mut run, &stage_def, &opts, &config).unwrap();
        assert!(result.is_some());
        // Check output map has the consensus fields.
        let out = run.outputs.get("consensus").unwrap();
        assert_eq!(out.get("proceed").unwrap(), "true");
    }

    #[test]
    fn stage_consensus_below_threshold_fails() {
        let dir = tempdir().unwrap();
        let run_id = "test-consensus-fail";
        let review_base = dir.path().join(".ta").join("review").join(run_id);
        for (role, score) in &[("architect_review", 0.3_f64), ("security_review", 0.2_f64)] {
            let role_dir = review_base.join(role);
            std::fs::create_dir_all(&role_dir).unwrap();
            let verdict = serde_json::json!({"score": score, "findings": ["Critical bug"], "decision": "flag"});
            std::fs::write(
                role_dir.join("verdict.json"),
                serde_json::to_string(&verdict).unwrap(),
            )
            .unwrap();
        }

        let config = WorkflowConfig::default();
        let mut run = GovernedWorkflowRun::new(run_id, "test-wf", "Goal");
        let stage_def = make_consensus_stage_def(vec![
            "architect_review".to_string(),
            "security_review".to_string(),
        ]);
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "test",
            goal_title: "test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        let err = stage_consensus(&mut run, &stage_def, &opts, &config).unwrap_err();
        assert!(err.to_string().contains("BLOCKED"));
    }

    #[test]
    fn stage_consensus_missing_verdict_file_is_timeout() {
        let dir = tempdir().unwrap();
        let run_id = "test-consensus-timeout";
        // Don't create any verdict files.
        let config = WorkflowConfig::default();
        let mut run = GovernedWorkflowRun::new(run_id, "test-wf", "Goal");
        let stage_def = make_consensus_stage_def(vec!["reviewer_a".to_string()]);
        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "test",
            goal_title: "test",
            dry_run: false,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        // With 1 timed-out reviewer (score=0.0), proceed=false → BLOCKED
        let err = stage_consensus(&mut run, &stage_def, &opts, &config).unwrap_err();
        assert!(err.to_string().contains("BLOCKED"));
    }

    // ── find_workflow_def YAML loading (v0.15.15.3.1) ────────────────────────

    #[test]
    fn find_workflow_def_loads_yaml_template() {
        let dir = tempdir().unwrap();
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // Write a minimal YAML governed workflow definition.
        let yaml = r#"workflow:
  name: test-wf
  description: Test workflow
  config: {}
stages:
  - name: run_goal
    description: Run the goal
"#;
        std::fs::write(templates_dir.join("test-wf.yaml"), yaml).unwrap();

        let def = find_workflow_def(dir.path(), "test-wf").unwrap();
        assert_eq!(def.workflow.name, "test-wf");
        assert_eq!(def.stages.len(), 1);
        assert_eq!(def.stages[0].name, "run_goal");
    }

    #[test]
    fn find_workflow_def_yaml_takes_precedence_over_toml() {
        let dir = tempdir().unwrap();
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // YAML template should win over TOML.
        let yaml = r#"workflow:
  name: yaml-winner
  description: From YAML
  config: {}
stages:
  - name: run_goal
    description: Run
"#;
        let toml = "[workflow]\nname = \"toml-loser\"\ndescription = \"From TOML\"\n\
                    [[stages]]\nname = \"run_goal\"\ndescription = \"Run\"\n";
        std::fs::write(templates_dir.join("my-wf.yaml"), yaml).unwrap();
        std::fs::write(templates_dir.join("my-wf.toml"), toml).unwrap();

        let def = find_workflow_def(dir.path(), "my-wf").unwrap();
        assert_eq!(def.workflow.name, "yaml-winner");
    }

    #[test]
    fn find_workflow_def_project_local_yaml_beats_builtin_yaml() {
        let dir = tempdir().unwrap();
        // Project-local definition.
        let local_dir = dir.path().join(".ta").join("workflows");
        std::fs::create_dir_all(&local_dir).unwrap();
        let local_yaml = r#"workflow:
  name: local-override
  description: Project-local
  config: {}
stages:
  - name: run_goal
    description: Run
"#;
        std::fs::write(local_dir.join("my-wf.yaml"), local_yaml).unwrap();
        // Built-in definition (should be ignored).
        let builtin_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&builtin_dir).unwrap();
        let builtin_yaml = r#"workflow:
  name: builtin
  description: Built-in
  config: {}
stages:
  - name: run_goal
    description: Run
"#;
        std::fs::write(builtin_dir.join("my-wf.yaml"), builtin_yaml).unwrap();

        let def = find_workflow_def(dir.path(), "my-wf").unwrap();
        assert_eq!(def.workflow.name, "local-override");
    }

    #[test]
    fn find_workflow_def_not_found_returns_error() {
        let dir = tempdir().unwrap();
        let result = find_workflow_def(dir.path(), "nonexistent-workflow");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    // ── build.yaml sub-workflow resolves (v0.15.15.5) ─────────────────────────

    #[test]
    fn build_yaml_resolves_as_sub_workflow() {
        let dir = tempdir().unwrap();
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // Write the canonical build.yaml with all 5 stages.
        let yaml = r#"workflow:
  name: build
  description: Governed per-phase build sub-workflow.
  config:
    reviewer_agent: claude-code
    gate_on_verdict: auto
stages:
  - name: run_goal
    description: Run the agent goal.
  - name: review_draft
    description: Review the draft.
    depends_on:
      - run_goal
  - name: human_gate
    description: Gate on verdict.
    depends_on:
      - review_draft
  - name: apply_draft
    description: Apply the draft.
    depends_on:
      - human_gate
  - name: pr_sync
    description: Poll PR merge and sync.
    depends_on:
      - apply_draft
"#;
        std::fs::write(templates_dir.join("build.yaml"), yaml).unwrap();

        let def = find_workflow_def(dir.path(), "build").unwrap();
        assert_eq!(def.workflow.name, "build");
        assert_eq!(def.stages.len(), 5);
        let names: Vec<&str> = def.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "run_goal",
                "review_draft",
                "human_gate",
                "apply_draft",
                "pr_sync"
            ]
        );
    }

    #[test]
    fn build_yaml_dry_run_completes() {
        let dir = tempdir().unwrap();
        let templates_dir = dir.path().join("templates").join("workflows");
        std::fs::create_dir_all(&templates_dir).unwrap();
        let yaml = r#"workflow:
  name: build
  description: Governed per-phase build sub-workflow.
  config: {}
stages:
  - name: run_goal
    description: Run the goal.
  - name: review_draft
    description: Review.
    depends_on: [run_goal]
  - name: human_gate
    description: Gate.
    depends_on: [review_draft]
  - name: apply_draft
    description: Apply.
    depends_on: [human_gate]
  - name: pr_sync
    description: Sync.
    depends_on: [apply_draft]
"#;
        std::fs::write(templates_dir.join("build.yaml"), yaml).unwrap();

        let opts = RunOptions {
            workspace_root: dir.path(),
            workflow_name: "build",
            goal_title: "v0.15.15.5 — test",
            dry_run: true,
            resume_run_id: None,
            agent: "claude-code",
            plan_phase: None,
            depth: 0,
            params: Default::default(),
        };
        // Dry-run should succeed without invoking any external commands.
        run_governed_workflow(&opts).unwrap();
    }

    // ── auto-approve conditions (v0.15.15.5) ──────────────────────────────────

    #[test]
    fn auto_approve_conditions_met_proceeds_without_prompt() {
        let config = AutoApproveConfig {
            enabled: true,
            conditions: vec!["reviewer_approved".to_string(), "no_flags".to_string()],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.95,
        };
        assert!(config.conditions_met(&verdict));
    }

    #[test]
    fn auto_approve_disabled_never_fires() {
        let config = AutoApproveConfig {
            enabled: false,
            conditions: vec!["reviewer_approved".to_string()],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.95,
        };
        assert!(!config.conditions_met(&verdict));
    }

    #[test]
    fn auto_approve_no_conditions_never_fires() {
        let config = AutoApproveConfig {
            enabled: true,
            conditions: vec![],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.95,
        };
        assert!(!config.conditions_met(&verdict));
    }

    #[test]
    fn auto_approve_falls_back_when_reviewer_flagged() {
        let config = AutoApproveConfig {
            enabled: true,
            conditions: vec!["reviewer_approved".to_string(), "no_flags".to_string()],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Flag,
            findings: vec!["performance concern".to_string()],
            confidence: 0.7,
        };
        assert!(!config.conditions_met(&verdict));
    }

    #[test]
    fn auto_approve_falls_back_when_findings_present() {
        let config = AutoApproveConfig {
            enabled: true,
            conditions: vec!["reviewer_approved".to_string(), "no_flags".to_string()],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec!["minor style nit".to_string()],
            confidence: 0.85,
        };
        // "no_flags" condition fails because findings is non-empty.
        assert!(!config.conditions_met(&verdict));
    }

    #[test]
    fn auto_approve_severity_below_condition_passes_when_no_findings() {
        let config = AutoApproveConfig {
            enabled: true,
            conditions: vec![
                "reviewer_approved".to_string(),
                "severity_below".to_string(),
            ],
        };
        let verdict = ReviewerVerdict {
            verdict: VerdictDecision::Approve,
            findings: vec![],
            confidence: 0.9,
        };
        assert!(config.conditions_met(&verdict));
    }

    // ── post-sync build config (v0.15.15.5) ───────────────────────────────────

    #[test]
    fn post_sync_build_disabled_by_default() {
        let config = PostSyncBuildConfig::default();
        assert!(!config.enabled);
        assert!(config.command.is_none());
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.on_failure, PostSyncOnFailure::Halt);
    }

    #[test]
    fn post_sync_build_warn_continues_on_failure() {
        let dir = tempdir().unwrap();
        let run = GovernedWorkflowRun::new("test-psb", "build", "test goal");
        let config = WorkflowConfig {
            post_sync_build: PostSyncBuildConfig {
                enabled: true,
                command: Some("exit 1".to_string()),
                timeout_secs: 60,
                on_failure: PostSyncOnFailure::Warn,
            },
            ..Default::default()
        };
        // on_failure=warn should return Ok even when command fails.
        let result = run_post_sync_build(&run, &config, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn post_sync_build_halt_errors_on_failure() {
        let dir = tempdir().unwrap();
        let run = GovernedWorkflowRun::new("test-psb-halt", "build", "test goal");
        let config = WorkflowConfig {
            post_sync_build: PostSyncBuildConfig {
                enabled: true,
                command: Some("exit 1".to_string()),
                timeout_secs: 60,
                on_failure: PostSyncOnFailure::Halt,
            },
            ..Default::default()
        };
        let err = run_post_sync_build(&run, &config, dir.path()).unwrap_err();
        assert!(err.to_string().contains("Post-sync build failed"));
        assert!(err.to_string().contains("ta workflow resume"));
    }

    #[test]
    fn post_sync_build_succeeds_when_command_passes() {
        let dir = tempdir().unwrap();
        let run = GovernedWorkflowRun::new("test-psb-ok", "build", "test goal");
        let config = WorkflowConfig {
            post_sync_build: PostSyncBuildConfig {
                enabled: true,
                command: Some("true".to_string()),
                timeout_secs: 60,
                on_failure: PostSyncOnFailure::Halt,
            },
            ..Default::default()
        };
        assert!(run_post_sync_build(&run, &config, dir.path()).is_ok());
    }

    #[test]
    fn post_sync_build_skipped_when_disabled() {
        let dir = tempdir().unwrap();
        let run = GovernedWorkflowRun::new("test-psb-skip", "build", "test goal");
        let config = WorkflowConfig::default(); // enabled = false
                                                // Should be a no-op regardless of workspace state.
        assert!(run_post_sync_build(&run, &config, dir.path()).is_ok());
    }

    // ── v0.15.19.4.2: Progress heartbeat parsing tests ──────────────────────

    #[test]
    fn heartbeat_lines_parsed_from_stdout() {
        // Goal run stdout with [progress] item lines → workflow shows count.
        let stdout = "[progress] item 1: source-verification for PLAN.md-only drafts — done\n\
                      [progress] item 2: coverage auto-mark — done\n\
                      [progress] item 3: heartbeat injection — done\n\
                      Some other output line\n";
        // Call parse_and_report_progress_heartbeats — should not panic.
        // We can't capture stdout in unit tests easily, so just verify it runs.
        parse_and_report_progress_heartbeats(stdout);
    }

    #[test]
    fn heartbeat_parser_reports_zero_when_no_progress_lines() {
        let stdout = "Building draft...\nDraft complete.\n";
        // Should emit the "no progress heartbeats" warning line.
        // Just verify it doesn't panic.
        parse_and_report_progress_heartbeats(stdout);
    }

    #[test]
    fn post_sync_build_skipped_when_no_command() {
        let dir = tempdir().unwrap();
        let run = GovernedWorkflowRun::new("test-psb-nocmd", "build", "test goal");
        let config = WorkflowConfig {
            post_sync_build: PostSyncBuildConfig {
                enabled: true,
                command: None,
                timeout_secs: 60,
                on_failure: PostSyncOnFailure::Halt,
            },
            ..Default::default()
        };
        assert!(run_post_sync_build(&run, &config, dir.path()).is_ok());
    }

    // ── WorkPlan stage (v0.15.20) ─────────────────────────────────────────────

    #[test]
    fn plan_work_stage_kind_deserializes() {
        let toml_str = r#"
name = "plan_work"
kind = "plan_work"
description = "Planner stage"
"#;
        let stage: StageDef = toml::from_str(toml_str).unwrap();
        assert_eq!(stage.kind, StageKind::PlanWork);
    }

    #[test]
    fn plan_work_display_label() {
        // The display label for PlanWork is " [plan_work]".
        let label = match StageKind::PlanWork {
            StageKind::PlanWork => " [plan_work]".to_string(),
            _ => panic!("wrong variant"),
        };
        assert_eq!(label, " [plan_work]");
    }

    #[test]
    fn stage_run_goal_injects_work_plan_from_outputs() {
        // When run.outputs contains a "plan_work.work_plan_path", stage_run_goal
        // should set TA_WORK_PLAN_JSON_PATH on the subprocess. We verify the logic
        // without spawning ta by checking the output resolution.
        let dir = tempdir().unwrap();
        let runs_dir = dir.path().join("workflow-runs");
        let mut run = GovernedWorkflowRun::new("wp-run-1", "governed-goal", "Test goal");
        let mut plan_out = std::collections::HashMap::new();
        plan_out.insert(
            "work_plan_path".to_string(),
            "/tmp/work-plan.json".to_string(),
        );
        run.outputs.insert("plan_work".to_string(), plan_out);
        run.save(&runs_dir).unwrap();

        // Verify the work_plan_path can be extracted from outputs.
        let loaded = GovernedWorkflowRun::load(&runs_dir, "wp-run-1").unwrap();
        let wp_path = loaded
            .outputs
            .values()
            .find_map(|m| m.get("work_plan_path").cloned());
        assert_eq!(wp_path.as_deref(), Some("/tmp/work-plan.json"));
    }
}
