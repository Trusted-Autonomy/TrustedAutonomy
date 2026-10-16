//! Types shared by the scan, act and run stages.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Who decides what happens to a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Ground truth is unambiguous and the action loses nothing. Done without asking.
    AutoSafe,
    /// Probably right, but loses information or rests on indirect evidence.
    /// One confirmation covers the whole batch.
    Propose,
    /// Evidence conflicts or is missing. One question per finding.
    Ask,
    /// Nothing to do (kept on purpose). Shown so the reasoning is visible.
    Skip,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::AutoSafe => "auto_safe",
            Verdict::Propose => "propose",
            Verdict::Ask => "ask",
            Verdict::Skip => "skip",
        })
    }
}

/// What kind of thing was found. Also the unit of `--only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    StuckDraft,
    StaleVcsLabel,
    StuckGoal,
    Staging,
    TargetDir,
    PidFile,
    ApplyLock,
    LeftoverDecisions,
    PhaseClaim,
    Archive,
    /// A local branch (and maybe its worktree) whose work is done or in question.
    Branch,
    /// A remote branch whose PR was closed without merging. Reported only.
    RemoteBranch,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::StuckDraft => "stuck_draft",
            Kind::StaleVcsLabel => "stale_vcs_label",
            Kind::StuckGoal => "stuck_goal",
            Kind::Staging => "staging",
            Kind::TargetDir => "target_dir",
            Kind::PidFile => "pid_file",
            Kind::ApplyLock => "apply_lock",
            Kind::LeftoverDecisions => "leftover_decisions",
            Kind::PhaseClaim => "phase_claim",
            Kind::Archive => "archive",
            Kind::Branch => "branch",
            Kind::RemoteBranch => "remote_branch",
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Kind::StuckDraft | Kind::StaleVcsLabel => Category::Drafts,
            Kind::StuckGoal => Category::Goals,
            Kind::Staging | Kind::TargetDir => Category::Staging,
            Kind::PidFile | Kind::ApplyLock | Kind::LeftoverDecisions | Kind::PhaseClaim => {
                Category::Locks
            }
            Kind::Archive => Category::Archive,
            Kind::Branch | Kind::RemoteBranch => Category::Branches,
        }
    }
}

/// The `--only` groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Drafts,
    Staging,
    Goals,
    Locks,
    Archive,
    Branches,
}

impl Category {
    pub fn parse(s: &str) -> Option<Category> {
        match s.trim().to_ascii_lowercase().as_str() {
            "drafts" | "draft" => Some(Category::Drafts),
            "staging" => Some(Category::Staging),
            "goals" | "goal" => Some(Category::Goals),
            "locks" | "lock" => Some(Category::Locks),
            "archive" => Some(Category::Archive),
            "branches" | "branch" => Some(Category::Branches),
            _ => None,
        }
    }

    pub fn names() -> &'static str {
        "drafts, staging, goals, locks, archive, branches"
    }
}

/// One fact a verdict rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub fact: String,
}

impl Evidence {
    pub fn new(source: &str, fact: impl Into<String>) -> Self {
        Evidence {
            source: source.to_string(),
            fact: fact.into(),
        }
    }
}

/// A concrete change cleanup can make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Nothing changes (the "keep it" answer).
    None,
    CloseDraft {
        draft_id: Uuid,
        reason: String,
        applied_externally_ref: Option<String>,
    },
    RefreshVcsLabel {
        draft_id: Uuid,
        new_state: String,
        review_ref: String,
    },
    SetGoalState {
        goal_id: Uuid,
        /// `applied` or `closed`.
        state: String,
        reason: String,
        applied_externally_ref: Option<String>,
    },
    RemoveDir {
        path: PathBuf,
    },
    RemoveFile {
        path: PathBuf,
    },
    ReleasePhaseClaim {
        phase: String,
    },
    /// Delete a local branch, after removing its worktree if it has one. The branch is only
    /// deleted while it still points at `tip_sha`, and the deletion is logged with that SHA so
    /// `git branch <name> <sha>` brings it back.
    PruneBranch {
        branch: String,
        tip_sha: String,
        worktree: Option<PathBuf>,
        /// Remove the worktree even with uncommitted or untracked files (a person said so).
        force: bool,
        /// Why this was safe or chosen, copied into the cleanup log.
        reason: String,
    },
    ArchiveRecords {
        goals: Vec<Uuid>,
        drafts: Vec<Uuid>,
    },
}

/// A choice offered for an `Ask` finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerOption {
    pub key: String,
    pub label: String,
    pub action: Action,
}

/// An earlier human answer to the same evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub answer: String,
    pub at: String,
}

/// A model's suggestion for an `Ask` finding. Never executed by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub option: String,
    pub confidence: f32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub kind: Kind,
    /// Stable, human-typeable id such as `draft:025f676e`. Used by `--explain`.
    pub id: String,
    pub title: String,
    pub evidence: Vec<Evidence>,
    pub verdict: Verdict,
    pub reason: String,
    /// What runs for `AutoSafe` and `Propose`.
    pub action: Action,
    /// Choices for `Ask`.
    pub options: Vec<AnswerOption>,
    pub recommended: Option<String>,
    /// Hash of the evidence facts that matter, so a recurring situation is recognised.
    pub signature: String,
    pub bytes: u64,
    /// The exact command a person can type instead.
    pub manual_command: Option<String>,
    pub remembered: Option<Remembered>,
    pub suggestion: Option<Suggestion>,
}

/// What an executed action did.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub finding_id: String,
    pub summary: String,
    pub bytes_reclaimed: u64,
    pub decided_by: Decider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decider {
    #[serde(rename = "rule")]
    Rule,
    #[serde(rename = "human")]
    Human,
    #[serde(rename = "human-remembered")]
    HumanRemembered,
    #[serde(rename = "model-suggested+human")]
    ModelSuggestedHuman,
}

impl Decider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decider::Rule => "rule",
            Decider::Human => "human",
            Decider::HumanRemembered => "human-remembered",
            Decider::ModelSuggestedHuman => "model-suggested+human",
        }
    }
}

/// State of a review (PR) according to the VCS adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewState {
    Open,
    Merged,
    Closed,
    /// No adapter, adapter failed or answer not understood. Never an answer.
    Unknown(String),
}

/// One PR opened from a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchReview {
    pub review_id: String,
    pub state: ReviewState,
    /// Commit the PR branch pointed at when the PR ended or was last seen.
    pub head_sha: Option<String>,
}

/// What the VCS says about the PRs of one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchPr {
    /// The VCS looked and found no PR for the branch.
    NoPr,
    /// One or more PRs (a branch can be reused).
    Prs(Vec<BranchReview>),
    /// No adapter, adapter failed or cannot answer. Never read as "no PR".
    Unknown(String),
}

/// One PR as `(review id, state, head sha)`.
pub type BranchPrRow = (String, String, Option<String>);

impl BranchPr {
    /// Build from an adapter answer of `(review id, state, head sha)` triples.
    pub fn from_lookup(r: Result<Option<Vec<BranchPrRow>>, String>) -> Self {
        match r {
            Err(e) => BranchPr::Unknown(format!("VCS lookup failed: {e}")),
            Ok(None) => BranchPr::Unknown("the VCS adapter does not report PRs by branch".into()),
            Ok(Some(v)) if v.is_empty() => BranchPr::NoPr,
            Ok(Some(v)) => BranchPr::Prs(
                v.into_iter()
                    .map(|(review_id, state, head_sha)| BranchReview {
                        review_id,
                        state: match state.to_ascii_lowercase().as_str() {
                            "merged" => ReviewState::Merged,
                            "closed" => ReviewState::Closed,
                            "open" | "draft" => ReviewState::Open,
                            other => {
                                ReviewState::Unknown(format!("unrecognised review state '{other}'"))
                            }
                        },
                        head_sha,
                    })
                    .collect(),
            ),
        }
    }
}

/// Everything the engine cannot read from `.ta/` itself.
pub trait GroundTruth {
    /// State of the PR/review named by `review_ref` (a number or URL).
    fn review_state(&self, review_ref: &str) -> ReviewState;
    /// Whether PLAN.md marks `phase` done.
    fn phase_done(&self, phase: &str) -> bool;
    fn process_alive(&self, pid: u32) -> bool {
        ta_lifecycle::process_is_alive(pid)
    }
    /// Phases the daemon holds a claim on. Empty when the daemon cannot be asked.
    fn phase_claims(&self) -> Vec<String> {
        Vec::new()
    }
    /// Release a daemon claim. Errors are reported, not fatal.
    fn release_phase_claim(&self, _phase: &str) -> anyhow::Result<()> {
        Ok(())
    }
    /// The PRs opened from `branch`. This, not `git branch -d` or `git cherry`, decides whether
    /// a branch is done: a squash merge rewrites patch ids.
    fn branch_pr(&self, _branch: &str) -> BranchPr {
        BranchPr::Unknown("no VCS adapter is configured".into())
    }
    /// Branches whose PR was closed without merging. Empty when the VCS cannot say.
    fn closed_unmerged_branches(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Closes a draft. The CLI supplies one that also resets plan phases and writes
/// the goal ledger; [`crate::act::FileCloser`] is the file-level default.
pub trait DraftCloser {
    fn close(
        &self,
        paths: &Paths,
        draft_id: Uuid,
        reason: &str,
        applied_externally_ref: Option<&str>,
    ) -> anyhow::Result<()>;
}

/// Reads evidence and suggests an option. Its output is data, never an instruction.
pub trait Assistant {
    fn suggest(&self, finding: &Finding) -> Option<Suggestion>;
}

/// Asks the person at the terminal.
pub trait Prompter {
    fn is_interactive(&self) -> bool;
    /// One yes/no for a whole batch of proposals.
    fn confirm(&mut self, message: &str) -> bool;
    /// Returns the chosen option key, or `None` to leave the finding open.
    fn choose(&mut self, finding: &Finding) -> Option<String>;
}

/// Where things live.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub goals_dir: PathBuf,
    pub pr_packages_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub audit_log: PathBuf,
}

impl Paths {
    pub fn for_project(root: &Path) -> Self {
        let ta = root.join(".ta");
        Paths {
            root: root.to_path_buf(),
            goals_dir: ta.join("goals"),
            pr_packages_dir: ta.join("pr_packages"),
            staging_dir: ta.join("staging"),
            audit_log: ta.join("audit.jsonl"),
        }
    }

    pub fn cleanup_dir(&self) -> PathBuf {
        self.root.join(".ta").join("cleanup")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.root.join(".ta").join("archive")
    }

    pub fn ta_dir(&self) -> PathBuf {
        self.root.join(".ta")
    }
}

/// The `[cleanup]` and `[gc]` settings the engine needs.
#[derive(Debug, Clone)]
pub struct Settings {
    /// One staleness threshold for drafts, goals and staging (`gc.stale_threshold_days`).
    pub stale_days: u64,
    pub retention_days: u64,
    pub archive: bool,
    pub model_assist: bool,
    /// Include the `branches` group in runs that do not name it. Default: true.
    pub branches: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stale_days: 7,
            retention_days: 90,
            archive: true,
            model_assist: false,
            branches: true,
        }
    }
}

/// The engine: settings plus the things it consults.
pub struct Engine<'a> {
    pub paths: Paths,
    pub settings: Settings,
    pub truth: &'a dyn GroundTruth,
    pub closer: &'a dyn DraftCloser,
    pub assistant: Option<&'a dyn Assistant>,
}

/// Options for one run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub dry_run: bool,
    /// Accept every `Propose`.
    pub yes: bool,
    pub only: Option<Vec<Category>>,
    /// Run `AutoSafe` actions only and neither propose nor ask (daemon, doctor).
    pub auto_safe_only: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub done: Vec<ActionResult>,
    pub failed: Vec<(String, String)>,
    /// `Propose` findings left alone (declined or not confirmed).
    pub proposals_open: Vec<String>,
    /// `Ask` findings still unanswered.
    pub questions_open: Vec<String>,
    pub questions_file_written: bool,
    pub bytes_reclaimed: u64,
}
