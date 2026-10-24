//! Caps and timing policy of the intake store, plus the store's error type.

use std::path::PathBuf;
use std::time::Duration;

use crate::model::RequestState;

/// Default per-goal cap: requests one goal may ever make.
pub const DEFAULT_PER_GOAL: usize = 10;
/// Default per-role cap: requests one role may make in any rolling hour.
pub const DEFAULT_PER_ROLE_PER_HOUR: usize = 20;
/// Default per-project cap: requests outstanding (queued, claimed or drafted).
pub const DEFAULT_PER_PROJECT_OUTSTANDING: usize = 50;
/// Default lease a drain holds on its batch.
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(30 * 60);
/// Default number of claims before a request that never gets acknowledged expires.
pub const DEFAULT_MAX_CLAIM_ATTEMPTS: u32 = 3;
/// Default age after which an undrained request expires.
pub const DEFAULT_QUEUED_TTL: Duration = Duration::from_secs(14 * 24 * 3600);
/// Default maximum number of requests one drain claims.
pub const DEFAULT_DRAIN_MAX: usize = 25;

/// The three admission caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeCaps {
    /// Requests one goal may ever make (counted across every state).
    pub per_goal: usize,
    /// Requests one role may make within the last hour.
    pub per_role_per_hour: usize,
    /// Requests of one project that are not yet in a terminal state.
    pub per_project_outstanding: usize,
}

impl Default for IntakeCaps {
    fn default() -> Self {
        Self {
            per_goal: DEFAULT_PER_GOAL,
            per_role_per_hour: DEFAULT_PER_ROLE_PER_HOUR,
            per_project_outstanding: DEFAULT_PER_PROJECT_OUTSTANDING,
        }
    }
}

/// Everything configurable about a store: caps plus recovery timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeSettings {
    /// Admission caps.
    pub caps: IntakeCaps,
    /// A `claimed` request whose lease lapsed returns to `queued` until it has
    /// been claimed this many times, then it expires.
    pub max_claim_attempts: u32,
    /// A request still `queued` after this long expires.
    pub queued_ttl: Duration,
}

impl Default for IntakeSettings {
    fn default() -> Self {
        Self {
            caps: IntakeCaps::default(),
            max_claim_attempts: DEFAULT_MAX_CLAIM_ATTEMPTS,
            queued_ttl: DEFAULT_QUEUED_TTL,
        }
    }
}

/// The `[task_intake]` table of `.ta/workflow.toml`. Every key is optional;
/// omitted keys take the documented defaults. `0` for a cap refuses every
/// request in that scope.
///
/// ```toml
/// [task_intake]
/// per_goal_max = 10            # requests one goal may ever make
/// per_role_per_hour_max = 20   # requests one role may make per rolling hour
/// per_project_max = 50         # requests outstanding (queued/claimed/drafted) per project
/// queued_ttl_days = 14         # a request never drained within this long expires
/// max_claim_attempts = 3       # claims whose lease lapsed before the request expires
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TaskIntakeConfig {
    /// [`IntakeCaps::per_goal`]
    pub per_goal_max: usize,
    /// [`IntakeCaps::per_role_per_hour`]
    pub per_role_per_hour_max: usize,
    /// [`IntakeCaps::per_project_outstanding`]
    pub per_project_max: usize,
    /// [`IntakeSettings::queued_ttl`] in days.
    pub queued_ttl_days: u64,
    /// [`IntakeSettings::max_claim_attempts`]
    pub max_claim_attempts: u32,
}

impl Default for TaskIntakeConfig {
    fn default() -> Self {
        Self {
            per_goal_max: DEFAULT_PER_GOAL,
            per_role_per_hour_max: DEFAULT_PER_ROLE_PER_HOUR,
            per_project_max: DEFAULT_PER_PROJECT_OUTSTANDING,
            queued_ttl_days: DEFAULT_QUEUED_TTL.as_secs() / 86_400,
            max_claim_attempts: DEFAULT_MAX_CLAIM_ATTEMPTS,
        }
    }
}

impl TaskIntakeConfig {
    /// The store settings this configuration describes.
    pub fn settings(&self) -> IntakeSettings {
        IntakeSettings {
            caps: IntakeCaps {
                per_goal: self.per_goal_max,
                per_role_per_hour: self.per_role_per_hour_max,
                per_project_outstanding: self.per_project_max,
            },
            max_claim_attempts: self.max_claim_attempts,
            queued_ttl: Duration::from_secs(self.queued_ttl_days.saturating_mul(86_400)),
        }
    }
}

/// Which admission cap refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapKind {
    /// [`IntakeCaps::per_goal`]
    PerGoal,
    /// [`IntakeCaps::per_role_per_hour`]
    PerRolePerHour,
    /// [`IntakeCaps::per_project_outstanding`]
    PerProject,
}

impl CapKind {
    /// `[task_intake]` config key that raises this cap.
    pub fn config_key(self) -> &'static str {
        match self {
            CapKind::PerGoal => "per_goal_max",
            CapKind::PerRolePerHour => "per_role_per_hour_max",
            CapKind::PerProject => "per_project_max",
        }
    }
}

/// Errors from the store. Every message says what failed, what was being
/// attempted and what the operator can do.
#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    /// A filesystem operation failed.
    #[error("task intake: could not {op} {path}: {source}. Check that the directory exists, is writable by this user and the disk is not full.", path = path.display())]
    Io {
        /// What was being attempted.
        op: &'static str,
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The store lock was not obtained in time.
    #[error("task intake: timed out after {waited_secs}s waiting for the store lock {path}. Another gateway or a drain is holding it; retry, and if it persists check for a hung `ta` process.", path = path.display())]
    LockTimeout {
        /// Lock file.
        path: PathBuf,
        /// Seconds waited.
        waited_secs: u64,
    },
    /// A serialised line is larger than allowed.
    #[error("task intake: refusing to write a {what} of {bytes} bytes; the limit is {max} bytes. Shorten the request text.")]
    TooLarge {
        /// What was too large.
        what: &'static str,
        /// Actual size.
        bytes: usize,
        /// Limit.
        max: usize,
    },
    /// An admission cap refused the request.
    #[error("queue full, retry or ask_human: {detail} (cap `{key}` under [task_intake] in .ta/workflow.toml)", key = cap.config_key())]
    CapExceeded {
        /// Which cap.
        cap: CapKind,
        /// Which scope hit the limit and by how much.
        detail: String,
    },
    /// A state change is not allowed by the state machine.
    #[error("task intake: request {request_id} is `{from}` and cannot become `{to}`. Valid next states from `{from}`: {allowed}.", from = from.as_str(), to = to.as_str())]
    InvalidTransition {
        /// Request.
        request_id: String,
        /// Current state.
        from: RequestState,
        /// Requested state.
        to: RequestState,
        /// Comma-separated legal targets.
        allowed: String,
    },
    /// The request id is not in the store (or not in the caller's project).
    #[error("task intake: no request `{0}` in this project's queue. Run `ta task-intake list` to see the ids that exist.")]
    UnknownRequest(String),
    /// A batch id contains characters that are not allowed in a file name.
    #[error(
        "task intake: invalid batch id `{0}`; batch ids contain only letters, digits and `-`."
    )]
    BadBatchId(String),
    /// A line could not be serialised.
    #[error(
        "task intake: could not serialise a {what}: {source}. This is a bug; please report it."
    )]
    Serialize {
        /// What was being written.
        what: &'static str,
        /// Underlying error.
        #[source]
        source: serde_json::Error,
    },
}
