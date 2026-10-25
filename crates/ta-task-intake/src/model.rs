//! Wire and in-memory types of the task intake store.
//!
//! Everything in this module is a documented, serde-stable shape. The queue
//! file holds [`IntakeRecord`] lines, the ledger file holds [`LedgerEvent`]
//! lines, and [`RequestView`] is what readers (the `ta task-intake list` CLI,
//! the `ta_request_status` tool, a later drain consumer) get back.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Version stamped on every line written by this crate.
pub const SCHEMA_VERSION: u32 = 1;

/// Hard cap on the typed body of one request (typed fields plus `why_now`,
/// serialised as JSON), in bytes.
pub const MAX_BODY_BYTES: usize = 2048;

/// Hard cap on one serialised queue line, in bytes. A body at the 2 KB cap
/// plus the envelope fits with room to spare; anything larger is refused so a
/// single append can never become an unbounded write.
pub const MAX_LINE_BYTES: usize = 4096;

/// The only request kinds the funnel accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestKind {
    /// Ask for a new Wayfinder task to be created.
    CreateTask,
    /// Ask for a number to be recorded against an `external` KPI metric.
    RecordDatapoint,
}

impl RequestKind {
    /// Every accepted kind, in documentation order.
    pub const ALL: [RequestKind; 2] = [RequestKind::CreateTask, RequestKind::RecordDatapoint];

    /// The kebab-case wire name (`create-task`, `record-datapoint`).
    pub fn as_str(self) -> &'static str {
        match self {
            RequestKind::CreateTask => "create-task",
            RequestKind::RecordDatapoint => "record-datapoint",
        }
    }

    /// Parse the wire name; `None` for anything else.
    pub fn parse(s: &str) -> Option<RequestKind> {
        RequestKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// The typed, validated payload of one request. Validation of field contents
/// (lengths, character sets) happens in the caller (the MCP gateway) before a
/// [`Submission`] is built; the store only enforces the byte caps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RequestBody {
    /// A new task.
    CreateTask {
        /// Task title.
        title: String,
        /// Task verb (for example `build`, `review`).
        verb: String,
        /// Optional longer description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// A datapoint for an `external` KPI metric.
    RecordDatapoint {
        /// Wayfinder metric id.
        metric_id: String,
        /// The number to record (finite).
        value: f64,
        /// Provenance label for the number.
        source: String,
    },
}

impl RequestBody {
    /// The kind of this body.
    pub fn kind(&self) -> RequestKind {
        match self {
            RequestBody::CreateTask { .. } => RequestKind::CreateTask,
            RequestBody::RecordDatapoint { .. } => RequestKind::RecordDatapoint,
        }
    }

    /// What the request is about, used as one input of the dedupe hash:
    /// `task:new` for a new task, `metric:<id>` for a datapoint.
    pub fn target(&self) -> String {
        match self {
            RequestBody::CreateTask { .. } => "task:new".to_string(),
            RequestBody::RecordDatapoint { metric_id, .. } => format!("metric:{metric_id}"),
        }
    }

    /// Canonical body text for the dedupe hash: free text is lower-cased with
    /// whitespace runs collapsed, ids are kept verbatim, numbers use the
    /// shortest round-trip form (`-0` folds into `0`).
    pub fn normalized(&self) -> String {
        match self {
            RequestBody::CreateTask {
                title,
                verb,
                description,
            } => format!(
                "{}\u{1f}{}\u{1f}{}",
                normalize_text(title),
                normalize_text(verb),
                normalize_text(description.as_deref().unwrap_or(""))
            ),
            RequestBody::RecordDatapoint {
                metric_id,
                value,
                source,
            } => {
                let v = if *value == 0.0 { 0.0 } else { *value };
                format!("{}\u{1f}{}\u{1f}{}", metric_id, v, normalize_text(source))
            }
        }
    }

    /// Server-side dedupe key: SHA-256 over project, kind, target and the
    /// normalised body. Scoped by project so two projects never collide.
    pub fn dedupe_key(&self, project_id: &str) -> String {
        let mut h = Sha256::new();
        for part in [
            project_id,
            self.kind().as_str(),
            &self.target(),
            &self.normalized(),
        ] {
            h.update(part.as_bytes());
            h.update([0u8]);
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Lower-case, trim and collapse every whitespace run to one space.
pub fn normalize_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What a gateway hands to [`crate::IntakeStore::enqueue`]. The identity
/// fields (`project_id`, `goal_id`, `role`) come from gateway state, never
/// from tool arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    /// Wayfinder project the request belongs to.
    pub project_id: String,
    /// Goal that made the request.
    pub goal_id: Uuid,
    /// Role (team role, goal role or agent id) of the requester.
    pub role: String,
    /// Typed payload.
    pub body: RequestBody,
    /// Why the requester needs this now (untrusted free text, single line).
    pub why_now: String,
    /// Whether the requesting goal is blocked until this is handled.
    pub blocks_goal: bool,
}

/// One line of `queue.jsonl` (and of every `batch-<id>.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeRecord {
    /// [`SCHEMA_VERSION`] when written.
    pub schema_version: u32,
    /// Unique id returned to the requester.
    pub request_id: String,
    /// Project the request is scoped to.
    pub project_id: String,
    /// Requesting goal.
    pub goal_id: Uuid,
    /// Requesting role.
    pub role: String,
    /// Typed payload (UNTRUSTED text; fence before showing to a human or model).
    pub body: RequestBody,
    /// Requester's reason for the urgency (UNTRUSTED text).
    pub why_now: String,
    /// Requester says its goal is blocked on this.
    pub blocks_goal: bool,
    /// Dedupe target, see [`RequestBody::target`].
    pub target: String,
    /// Dedupe hash, see [`RequestBody::dedupe_key`].
    pub dedupe_key: String,
    /// Enqueue time.
    pub created_at: DateTime<Utc>,
}

impl IntakeRecord {
    /// Kind of the payload.
    pub fn kind(&self) -> RequestKind {
        self.body.kind()
    }
}

/// Lifecycle state of a request. Stored in the ledger, never in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// Waiting in `queue.jsonl` for a drain.
    Queued,
    /// Drained into a batch; a consumer holds a lease on it.
    Claimed,
    /// A draft containing the request exists and awaits review.
    Drafted,
    /// Wayfinder confirmed the change. Terminal.
    Applied,
    /// Declined (with a reason). Terminal.
    Rejected,
    /// Aged out or exhausted its claim attempts. Terminal.
    Expired,
}

impl RequestState {
    /// Snake-case name used in the ledger and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            RequestState::Queued => "queued",
            RequestState::Claimed => "claimed",
            RequestState::Drafted => "drafted",
            RequestState::Applied => "applied",
            RequestState::Rejected => "rejected",
            RequestState::Expired => "expired",
        }
    }

    /// Parse a snake-case state name.
    pub fn parse(s: &str) -> Option<RequestState> {
        [
            RequestState::Queued,
            RequestState::Claimed,
            RequestState::Drafted,
            RequestState::Applied,
            RequestState::Rejected,
            RequestState::Expired,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
    }

    /// No further transition is possible.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RequestState::Applied | RequestState::Rejected | RequestState::Expired
        )
    }

    /// Counts against the per-project outstanding cap and blocks a duplicate.
    pub fn is_live(self) -> bool {
        !self.is_terminal()
    }

    /// Legal transitions of the state machine.
    pub fn can_become(self, to: RequestState) -> bool {
        use RequestState::*;
        matches!(
            (self, to),
            (Queued, Claimed)
                | (Queued, Expired)
                | (Claimed, Queued)
                | (Claimed, Drafted)
                | (Claimed, Rejected)
                | (Claimed, Expired)
                | (Drafted, Applied)
                | (Drafted, Rejected)
                | (Drafted, Expired)
        )
    }

    /// What a requester is told: `queued` while anything is pending, `done`
    /// once the request reached a terminal state. Deliberately coarse.
    pub fn requester_status(self) -> &'static str {
        if self.is_terminal() {
            "done"
        } else {
            "queued"
        }
    }
}

/// Who or what is mentioned on a [`LedgerEvent::Transition`] for the first
/// time: copied from the queue record so caps and dedupe never need the
/// queue file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// Requesting goal.
    pub goal_id: Uuid,
    /// Requesting role.
    pub role: String,
    /// Request kind.
    pub kind: RequestKind,
    /// Dedupe hash.
    pub dedupe_key: String,
    /// Original enqueue time.
    pub created_at: DateTime<Utc>,
}

/// One state change of one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// [`SCHEMA_VERSION`] when written.
    pub schema_version: u32,
    /// Request this applies to.
    pub request_id: String,
    /// Project scope.
    pub project_id: String,
    /// New state.
    pub state: RequestState,
    /// When the transition was recorded.
    pub at: DateTime<Utc>,
    /// Who recorded it (`gateway`, `drain`, `recovery`, a consumer name).
    pub actor: String,
    /// Present on the first (`queued`) event of a request only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Batch the request was claimed into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// When a `claimed` lease lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Draft this request went into (`drafted`, `applied`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    /// Reason (`rejected`, `expired`, and re-queues from recovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A second goal asked for something already in flight; it is attached to the
/// existing request instead of being queued again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Duplicate {
    /// [`SCHEMA_VERSION`] when written.
    pub schema_version: u32,
    /// The existing request the duplicate was folded into.
    pub request_id: String,
    /// Project scope.
    pub project_id: String,
    /// The additional requesting goal.
    pub goal_id: Uuid,
    /// When it was seen.
    pub at: DateTime<Utc>,
}

/// One line of `ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LedgerEvent {
    /// A state change.
    Transition(Transition),
    /// A deduplicated second requester.
    Duplicate(Duplicate),
}

/// Reader-facing summary of one request. All free text is already sanitised
/// (control characters, ANSI and bidi overrides stripped) but remains
/// UNTRUSTED DATA: fence it before putting it in front of a model or a human.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestView {
    /// Request id.
    pub request_id: String,
    /// Project scope.
    pub project_id: String,
    /// Current state (lease expiry is reported, not applied: see `lease_expired`).
    pub state: RequestState,
    /// Request kind.
    pub kind: RequestKind,
    /// Requesting goal.
    pub goal_id: Uuid,
    /// Requesting role (sanitised).
    pub role: String,
    /// Enqueue time.
    pub created_at: DateTime<Utc>,
    /// Time of the latest transition.
    pub state_at: DateTime<Utc>,
    /// Batch holding the request, once claimed.
    pub batch_id: Option<String>,
    /// Lease expiry of the current claim.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// The request is `claimed` and its lease has lapsed; the next drain or
    /// recovery pass returns it to the queue (or expires it).
    pub lease_expired: bool,
    /// How many times the request has been claimed.
    pub claim_count: u32,
    /// Draft the request went into.
    pub draft_id: Option<String>,
    /// Reason attached to the latest transition (sanitised, untrusted).
    pub reason: Option<String>,
    /// Other goals whose identical request was folded into this one.
    pub also_requested_by: Vec<Uuid>,
    /// Typed payload with sanitised text; `None` when the queue or batch file
    /// holding it is gone.
    pub body: Option<RequestBody>,
    /// Sanitised `why_now`.
    pub why_now: Option<String>,
    /// `blocks_goal` flag.
    pub blocks_goal: Option<bool>,
}

/// Result of a list/read pass.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListReport {
    /// Matching requests, oldest first.
    pub requests: Vec<RequestView>,
    /// Human-readable notes about damaged input (torn trailing lines, corrupt
    /// lines) so the reader can see data was skipped.
    pub warnings: Vec<String>,
}
