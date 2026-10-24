//! The [`IntakeStore`] trait and its file-backed implementation.
//!
//! ## On-disk layout (one directory per project root: `<root>/.ta/task-intake/`)
//!
//! | file | content |
//! |------|---------|
//! | `queue.jsonl` | [`IntakeRecord`] lines, appended by the gateway. File mode 0600, directory 0700. |
//! | `ledger.jsonl` | Append-only [`LedgerEvent`] lines: the ONLY place request state lives. |
//! | `batch-<id>.jsonl` | A drained queue, renamed atomically from `queue.jsonl`. Immutable. |
//! | `.lock` | `flock(2)` target serialising every writer (append, drain, ack, recovery). |
//!
//! ## Guarantees
//!
//! * Every append is ONE `write_all` of `line + '\n'` followed by `fsync`, under
//!   the exclusive store lock, so concurrent writers (threads or processes)
//!   never interleave bytes.
//! * A writer killed mid-line leaves a torn trailing line. Readers skip it
//!   (and say so in [`ListReport::warnings`]); the next append starts with a
//!   newline so the fragment can never corrupt a new record.
//! * The enqueue order is queue line first, ledger `queued` event second. A
//!   crash in between leaves a queue record with no ledger event; it counts as
//!   `queued` and the next drain or [`IntakeStore::recover`] writes the missing
//!   event. A request therefore can be late in the ledger but never lost.
//! * Drain renames `queue.jsonl` to `batch-<id>.jsonl`, then records `claimed`
//!   (with a lease) per request. If the process dies anywhere after the
//!   rename, [`IntakeStore::recover`] (also run at the start of every drain)
//!   resolves it deterministically from files plus the clock: a request
//!   `claimed` past its lease returns to `queued` (record copied back into the
//!   queue) until it has used its claim attempts, then it becomes `expired`;
//!   a `queued` request found only in a batch file is copied back; a request
//!   `queued` for longer than the TTL expires.
//! * Reading never takes the lock and never mutates anything.
//!
//! The batch file is the durable copy of record bodies: a request's body is
//! read from the queue or from the newest batch that contains it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ta_audit::{AuditAction, AuditEvent, AuditLog};
use uuid::Uuid;

use crate::fence::sanitize;
use crate::fs::{append_line, ensure_dir, read_jsonl, Layout, StoreLock};
use crate::model::*;
use crate::settings::{CapKind, IntakeError, IntakeSettings};

/// Longest reason text stored on a transition, in characters.
pub const MAX_REASON_CHARS: usize = 500;

/// Result of [`IntakeStore::enqueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new request was queued.
    Queued {
        /// Id of the new request.
        request_id: String,
    },
    /// An identical request is already in flight; nothing new was queued and
    /// no cap was charged.
    Duplicate {
        /// Id of the existing request.
        request_id: String,
        /// Its current state.
        state: RequestState,
    },
}

impl EnqueueOutcome {
    /// The request id the requester should hold on to.
    pub fn request_id(&self) -> &str {
        match self {
            EnqueueOutcome::Queued { request_id }
            | EnqueueOutcome::Duplicate { request_id, .. } => request_id,
        }
    }
}

/// Filter for [`IntakeStore::list`].
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Only this project.
    pub project_id: Option<String>,
    /// Only this state.
    pub state: Option<RequestState>,
    /// Only requests made by (or folded into from) this goal.
    pub goal_id: Option<Uuid>,
}

/// Parameters of [`IntakeStore::drain`].
#[derive(Debug, Clone)]
pub struct DrainRequest {
    /// Claim at most this many requests; the rest stay queued.
    pub max_records: usize,
    /// How long the claim is held before recovery may take it back.
    pub lease: Duration,
    /// Who is draining (recorded in the ledger and audit log).
    pub actor: String,
    /// Current time.
    pub now: DateTime<Utc>,
}

/// A claimed batch handed to a drain consumer. Bodies are UNTRUSTED: fence
/// them (see [`crate::fence`]) before a model or human reads them.
#[derive(Debug, Clone)]
pub struct Batch {
    /// Batch id (`<utc-timestamp>-<8 hex>`).
    pub batch_id: String,
    /// The immutable `batch-<id>.jsonl` file.
    pub path: PathBuf,
    /// When the claim was made.
    pub claimed_at: DateTime<Utc>,
    /// When the lease lapses.
    pub lease_expires_at: DateTime<Utc>,
    /// The claimed requests, oldest first.
    pub records: Vec<IntakeRecord>,
}

/// A consumer's outcome for claimed or drafted requests.
#[derive(Debug, Clone)]
pub enum Ack {
    /// A draft containing the requests exists (`claimed` -> `drafted`).
    Drafted {
        /// Draft id.
        draft_id: String,
    },
    /// Wayfinder confirmed the change (`drafted` -> `applied`).
    Applied {
        /// Draft that carried it, when known.
        draft_id: Option<String>,
    },
    /// Declined (`claimed` or `drafted` -> `rejected`).
    Rejected {
        /// Why.
        reason: String,
    },
    /// Gave up (`claimed` or `drafted` -> `expired`).
    Expired {
        /// Why.
        reason: String,
    },
}

impl Ack {
    fn target(&self) -> RequestState {
        match self {
            Ack::Drafted { .. } => RequestState::Drafted,
            Ack::Applied { .. } => RequestState::Applied,
            Ack::Rejected { .. } => RequestState::Rejected,
            Ack::Expired { .. } => RequestState::Expired,
        }
    }
}

/// Parameters of [`IntakeStore::acknowledge`].
#[derive(Debug, Clone)]
pub struct AckRequest {
    /// Project the ids belong to; ids of other projects are "unknown".
    pub project_id: String,
    /// Requests to move. All are validated before any is written.
    pub request_ids: Vec<String>,
    /// Outcome.
    pub ack: Ack,
    /// Who acknowledges (recorded in the ledger and audit log).
    pub actor: String,
    /// Current time.
    pub now: DateTime<Utc>,
}

/// What a recovery pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// `claimed` past lease, returned to `queued`.
    pub requeued: Vec<String>,
    /// Moved to `expired` (lease attempts exhausted, TTL, or body lost).
    pub expired: Vec<String>,
    /// Queue records that had no ledger `queued` event; the event was written.
    pub healed: Vec<String>,
    /// `queued` requests found only in a batch file and copied back to the queue.
    pub restored_to_queue: Vec<String>,
}

/// Storage for the task intake funnel. The gateway uses `enqueue` and
/// `status_for_requester`; the CLI uses `list`; a drain consumer (the
/// virtual-team poller, a later phase) uses `drain`, `acknowledge` and
/// `recover`. Implementations must serialise writers across processes.
pub trait IntakeStore {
    /// Admit a request: dedupe, caps, then append to queue and ledger.
    fn enqueue(
        &self,
        submission: Submission,
        now: DateTime<Utc>,
    ) -> Result<EnqueueOutcome, IntakeError>;

    /// State of `request_id` for the goal that made (or duplicated) it,
    /// scoped to `project_id`. `None` for unknown ids, other projects and
    /// other goals alike, so the answer cannot be used to probe.
    fn status_for_requester(
        &self,
        project_id: &str,
        request_id: &str,
        goal_id: Uuid,
    ) -> Result<Option<RequestState>, IntakeError>;

    /// Read-only, sanitised view of requests, oldest first.
    fn list(&self, filter: &ListFilter, now: DateTime<Utc>) -> Result<ListReport, IntakeError>;

    /// Claim queued requests into a batch by atomically renaming the queue
    /// file. `None` when nothing is queued.
    fn drain(&self, request: &DrainRequest) -> Result<Option<Batch>, IntakeError>;

    /// Record a consumer's outcome for requests it holds.
    fn acknowledge(&self, request: &AckRequest) -> Result<(), IntakeError>;

    /// Resolve lapsed leases, stale queued requests and half-finished writes.
    fn recover(&self, now: DateTime<Utc>, actor: &str) -> Result<RecoveryReport, IntakeError>;
}

/// File-backed [`IntakeStore`] (layout in the module docs).
#[derive(Debug, Clone)]
pub struct FileIntakeStore {
    layout: Layout,
    settings: IntakeSettings,
    audit_log: Option<PathBuf>,
}

impl FileIntakeStore {
    /// Store for the project rooted at `project_root` (directory
    /// `<root>/.ta/task-intake`). Nothing is created until the first write.
    /// `audit_log` is the project audit log every transition is recorded in.
    pub fn open(
        project_root: &Path,
        settings: IntakeSettings,
        audit_log: Option<PathBuf>,
    ) -> FileIntakeStore {
        FileIntakeStore::open_dir(
            project_root.join(".ta").join("task-intake"),
            settings,
            audit_log,
        )
    }

    /// Store in an explicit directory.
    pub fn open_dir(
        dir: PathBuf,
        settings: IntakeSettings,
        audit_log: Option<PathBuf>,
    ) -> FileIntakeStore {
        FileIntakeStore {
            layout: Layout { dir },
            settings,
            audit_log,
        }
    }

    /// The store directory.
    pub fn dir(&self) -> &Path {
        &self.layout.dir
    }

    fn lock(&self) -> Result<StoreLock, IntakeError> {
        StoreLock::acquire(&self.layout.dir)
    }

    fn ttl(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.settings.queued_ttl).unwrap_or(chrono::Duration::days(14))
    }

    fn load(&self) -> Result<Snapshot, IntakeError> {
        Snapshot::load(&self.layout)
    }

    /// State used for caps and dedupe: a `queued` request past the TTL is
    /// already as good as expired even if no recovery pass has recorded it.
    fn effective_state(&self, t: &Tracked, now: DateTime<Utc>) -> RequestState {
        if t.state == RequestState::Queued && now - t.created_at > self.ttl() {
            RequestState::Expired
        } else {
            t.state
        }
    }

    fn ledger_append(&self, ev: &LedgerEvent) -> Result<(), IntakeError> {
        append_line(&self.layout.ledger(), "ledger event", ev, MAX_LINE_BYTES)
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_event(
        &self,
        t: &Tracked,
        state: RequestState,
        now: DateTime<Utc>,
        actor: &str,
        batch_id: Option<String>,
        lease: Option<DateTime<Utc>>,
        draft_id: Option<String>,
        reason: Option<String>,
    ) -> LedgerEvent {
        LedgerEvent::Transition(Transition {
            schema_version: SCHEMA_VERSION,
            request_id: t.request_id.clone(),
            project_id: t.project_id.clone(),
            state,
            at: now,
            actor: actor.to_string(),
            origin: None,
            batch_id,
            lease_expires_at: lease,
            draft_id,
            reason: reason.map(clip_reason),
        })
    }
}

fn clip_reason(r: String) -> String {
    if r.chars().count() <= MAX_REASON_CHARS {
        r
    } else {
        r.chars().take(MAX_REASON_CHARS).collect()
    }
}

/// Audit sink for one operation: the log is opened lazily once (opening
/// recovers the hash chain) and every transition of the operation is appended.
/// A failing audit log is reported loudly but does not undo state that is
/// already durably recorded.
struct Auditor<'a> {
    path: Option<&'a Path>,
    log: Option<AuditLog>,
    failed: bool,
}

impl<'a> Auditor<'a> {
    fn new(path: Option<&'a Path>) -> Self {
        Auditor {
            path,
            log: None,
            failed: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        actor: &str,
        label: &str,
        project_id: &str,
        request_id: Option<&str>,
        goal_id: Option<Uuid>,
        draft_id: Option<&str>,
        extra: serde_json::Value,
    ) {
        let Some(path) = self.path else { return };
        if self.failed {
            return;
        }
        if self.log.is_none() {
            match AuditLog::open(path) {
                Ok(l) => self.log = Some(l),
                Err(e) => {
                    self.failed = true;
                    tracing::error!(path = %path.display(), error = %e,
                        "task intake: could not open the audit log; transitions of this operation are NOT audited. Check the permissions of the file.");
                    return;
                }
            }
        }
        let target = match request_id {
            Some(id) => format!("task-intake://{project_id}/{id}"),
            None => format!("task-intake://{project_id}"),
        };
        let mut meta = serde_json::json!({
            "event": label,
            "project_id": project_id,
            "request_id": request_id,
            "goal_id": goal_id.map(|g| g.to_string()),
            "draft_id": draft_id,
        });
        if let (Some(m), Some(x)) = (meta.as_object_mut(), extra.as_object()) {
            for (k, v) in x {
                m.insert(k.clone(), v.clone());
            }
        }
        let mut ev = AuditEvent::new(format!("task-intake:{actor}"), AuditAction::TaskIntake)
            .with_target(target)
            .with_tool_name("task-intake")
            .with_metadata(meta);
        if let Some(g) = goal_id {
            ev = ev.with_goal_run_id(g);
        }
        if let Some(log) = self.log.as_mut() {
            if let Err(e) = log.append(&mut ev) {
                self.failed = true;
                tracing::error!(path = %path.display(), error = %e,
                    "task intake: audit append failed; the transition is recorded in the ledger but not in the audit log");
            }
        }
    }
}

/// In-memory fold of the ledger joined with the queue and batch files.
struct Snapshot {
    tracked: Vec<Tracked>,
    index: HashMap<String, usize>,
    queue_ids: HashSet<String>,
    records: HashMap<String, IntakeRecord>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Tracked {
    request_id: String,
    project_id: String,
    goal_id: Uuid,
    role: String,
    kind: RequestKind,
    dedupe_key: String,
    created_at: DateTime<Utc>,
    state: RequestState,
    state_at: DateTime<Utc>,
    batch_id: Option<String>,
    lease_expires_at: Option<DateTime<Utc>>,
    draft_id: Option<String>,
    reason: Option<String>,
    claim_count: u32,
    also: Vec<Uuid>,
    in_ledger: bool,
}

impl Tracked {
    fn from_record(r: &IntakeRecord) -> Tracked {
        Tracked {
            request_id: r.request_id.clone(),
            project_id: r.project_id.clone(),
            goal_id: r.goal_id,
            role: r.role.clone(),
            kind: r.kind(),
            dedupe_key: r.dedupe_key.clone(),
            created_at: r.created_at,
            state: RequestState::Queued,
            state_at: r.created_at,
            batch_id: None,
            lease_expires_at: None,
            draft_id: None,
            reason: None,
            claim_count: 0,
            also: Vec::new(),
            in_ledger: false,
        }
    }

    fn from_origin(t: &Transition, o: &Origin) -> Tracked {
        Tracked {
            request_id: t.request_id.clone(),
            project_id: t.project_id.clone(),
            goal_id: o.goal_id,
            role: o.role.clone(),
            kind: o.kind,
            dedupe_key: o.dedupe_key.clone(),
            created_at: o.created_at,
            state: RequestState::Queued,
            state_at: t.at,
            batch_id: None,
            lease_expires_at: None,
            draft_id: None,
            reason: None,
            claim_count: 0,
            also: Vec::new(),
            in_ledger: true,
        }
    }

    fn origin(&self) -> Origin {
        Origin {
            goal_id: self.goal_id,
            role: self.role.clone(),
            kind: self.kind,
            dedupe_key: self.dedupe_key.clone(),
            created_at: self.created_at,
        }
    }

    fn requested_by(&self, goal: Uuid) -> bool {
        self.goal_id == goal || self.also.contains(&goal)
    }
}

impl Snapshot {
    fn load(layout: &Layout) -> Result<Snapshot, IntakeError> {
        // Order matters for lock-free readers racing a writer: ledger first,
        // then queue, then batches (see module docs).
        let ledger = read_jsonl::<LedgerEvent>(&layout.ledger())?;
        let queue = read_jsonl::<IntakeRecord>(&layout.queue())?;
        let mut warnings = ledger.warnings;
        warnings.extend(queue.warnings);

        let mut snap = Snapshot {
            tracked: Vec::new(),
            index: HashMap::new(),
            queue_ids: HashSet::new(),
            records: HashMap::new(),
            warnings,
        };

        let mut orphan_events = 0usize;
        for ev in ledger.items {
            match ev {
                LedgerEvent::Transition(t) => {
                    if !snap.index.contains_key(&t.request_id) {
                        match &t.origin {
                            Some(o) => {
                                snap.index.insert(t.request_id.clone(), snap.tracked.len());
                                snap.tracked.push(Tracked::from_origin(&t, o));
                            }
                            None => {
                                orphan_events += 1;
                                continue;
                            }
                        }
                    }
                    let tr = &mut snap.tracked[snap.index[&t.request_id]];
                    tr.state = t.state;
                    tr.state_at = t.at;
                    tr.reason = t.reason.clone();
                    match t.state {
                        RequestState::Claimed => {
                            tr.claim_count += 1;
                            tr.batch_id = t.batch_id.clone();
                            tr.lease_expires_at = t.lease_expires_at;
                        }
                        RequestState::Queued => {
                            tr.batch_id = None;
                            tr.lease_expires_at = None;
                        }
                        _ => {
                            tr.lease_expires_at = None;
                        }
                    }
                    if t.draft_id.is_some() {
                        tr.draft_id = t.draft_id.clone();
                    }
                }
                LedgerEvent::Duplicate(d) => {
                    if let Some(i) = snap.index.get(&d.request_id) {
                        let tr = &mut snap.tracked[*i];
                        if !tr.requested_by(d.goal_id) {
                            tr.also.push(d.goal_id);
                        }
                    }
                }
            }
        }
        if orphan_events > 0 {
            snap.warnings.push(format!(
                "{}: ignored {orphan_events} ledger event(s) for requests with no `queued` origin event",
                layout.ledger().display()
            ));
        }

        for r in queue.items {
            snap.queue_ids.insert(r.request_id.clone());
            snap.records.insert(r.request_id.clone(), r);
        }
        // Batches, oldest name first so a newer copy of a body wins.
        let mut batch_files: Vec<PathBuf> = match std::fs::read_dir(&layout.dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("batch-") && n.ends_with(".jsonl"))
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(IntakeError::Io {
                    op: "list",
                    path: layout.dir.clone(),
                    source,
                })
            }
        };
        batch_files.sort();
        for p in batch_files {
            let b = read_jsonl::<IntakeRecord>(&p)?;
            snap.warnings.extend(b.warnings);
            for r in b.items {
                snap.records.insert(r.request_id.clone(), r);
            }
        }

        // Records the ledger does not know (crash between queue and ledger
        // append): treat as queued. Ordered by creation time for determinism.
        let mut unledgered: Vec<&IntakeRecord> = snap
            .records
            .values()
            .filter(|r| !snap.index.contains_key(&r.request_id))
            .collect();
        unledgered
            .sort_by(|a, b| (a.created_at, &a.request_id).cmp(&(b.created_at, &b.request_id)));
        let extra: Vec<Tracked> = unledgered.into_iter().map(Tracked::from_record).collect();
        for t in extra {
            snap.index.insert(t.request_id.clone(), snap.tracked.len());
            snap.tracked.push(t);
        }
        Ok(snap)
    }

    fn get(&self, id: &str) -> Option<&Tracked> {
        self.index.get(id).map(|i| &self.tracked[*i])
    }
}

fn valid_batch_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn view_of(t: &Tracked, rec: Option<&IntakeRecord>, now: DateTime<Utc>) -> RequestView {
    let body = rec.map(|r| match &r.body {
        RequestBody::CreateTask {
            title,
            verb,
            description,
        } => RequestBody::CreateTask {
            title: sanitize(title),
            verb: sanitize(verb),
            description: description.as_deref().map(sanitize),
        },
        RequestBody::RecordDatapoint {
            metric_id,
            value,
            source,
        } => RequestBody::RecordDatapoint {
            metric_id: sanitize(metric_id),
            value: *value,
            source: sanitize(source),
        },
    });
    RequestView {
        request_id: t.request_id.clone(),
        project_id: t.project_id.clone(),
        state: t.state,
        kind: t.kind,
        goal_id: t.goal_id,
        role: sanitize(&t.role),
        created_at: t.created_at,
        state_at: t.state_at,
        batch_id: t.batch_id.clone(),
        lease_expires_at: t.lease_expires_at,
        lease_expired: t.state == RequestState::Claimed
            && t.lease_expires_at.is_some_and(|l| l <= now),
        claim_count: t.claim_count,
        draft_id: t.draft_id.clone(),
        reason: t.reason.as_deref().map(sanitize),
        also_requested_by: t.also.clone(),
        body,
        why_now: rec.map(|r| sanitize(&r.why_now)),
        blocks_goal: rec.map(|r| r.blocks_goal),
    }
}

impl IntakeStore for FileIntakeStore {
    fn enqueue(&self, sub: Submission, now: DateTime<Utc>) -> Result<EnqueueOutcome, IntakeError> {
        let _lock = self.lock()?;
        let snap = self.load()?;
        let mut audit = Auditor::new(self.audit_log.as_deref());
        let actor = "gateway";
        let kind = sub.body.kind();
        let dedupe_key = sub.body.dedupe_key(&sub.project_id);

        // 1. Dedupe: an identical live request absorbs this one, uncharged.
        if let Some(existing) = snap.tracked.iter().find(|t| {
            t.project_id == sub.project_id
                && t.dedupe_key == dedupe_key
                && self.effective_state(t, now).is_live()
        }) {
            if !existing.requested_by(sub.goal_id) {
                self.ledger_append(&LedgerEvent::Duplicate(Duplicate {
                    schema_version: SCHEMA_VERSION,
                    request_id: existing.request_id.clone(),
                    project_id: sub.project_id.clone(),
                    goal_id: sub.goal_id,
                    at: now,
                }))?;
            }
            audit.record(
                actor,
                "duplicate",
                &sub.project_id,
                Some(&existing.request_id),
                Some(sub.goal_id),
                None,
                serde_json::json!({ "kind": kind.as_str(), "state": existing.state.as_str() }),
            );
            return Ok(EnqueueOutcome::Duplicate {
                request_id: existing.request_id.clone(),
                state: existing.state,
            });
        }

        // 2. Caps.
        let caps = self.settings.caps;
        let hour_ago = now - chrono::Duration::hours(1);
        let goal_n = snap
            .tracked
            .iter()
            .filter(|t| t.goal_id == sub.goal_id)
            .count();
        let role_n = snap
            .tracked
            .iter()
            .filter(|t| {
                t.project_id == sub.project_id && t.role == sub.role && t.created_at > hour_ago
            })
            .count();
        let project_n = snap
            .tracked
            .iter()
            .filter(|t| t.project_id == sub.project_id && self.effective_state(t, now).is_live())
            .count();
        let refusal = if goal_n >= caps.per_goal {
            Some((
                CapKind::PerGoal,
                format!(
                    "per-goal cap hit: goal {} has already made {goal_n} request(s), the limit is {}",
                    sub.goal_id, caps.per_goal
                ),
            ))
        } else if role_n >= caps.per_role_per_hour {
            Some((
                CapKind::PerRolePerHour,
                format!(
                    "per-role hourly cap hit: role `{}` made {role_n} request(s) in the last hour, the limit is {}",
                    sub.role, caps.per_role_per_hour
                ),
            ))
        } else if project_n >= caps.per_project_outstanding {
            Some((
                CapKind::PerProject,
                format!(
                    "per-project cap hit: project `{}` has {project_n} outstanding request(s), the limit is {}",
                    sub.project_id, caps.per_project_outstanding
                ),
            ))
        } else {
            None
        };
        if let Some((cap, detail)) = refusal {
            audit.record(
                actor,
                "refused",
                &sub.project_id,
                None,
                Some(sub.goal_id),
                None,
                serde_json::json!({ "kind": kind.as_str(), "cap": cap.config_key(), "detail": detail }),
            );
            return Err(IntakeError::CapExceeded { cap, detail });
        }

        // 3. Admit: queue line first, ledger event second.
        ensure_dir(&self.layout.dir)?;
        let record = IntakeRecord {
            schema_version: SCHEMA_VERSION,
            request_id: format!("req-{}", Uuid::new_v4().simple()),
            project_id: sub.project_id.clone(),
            goal_id: sub.goal_id,
            role: sub.role.clone(),
            target: sub.body.target(),
            dedupe_key,
            body: sub.body,
            why_now: sub.why_now,
            blocks_goal: sub.blocks_goal,
            created_at: now,
        };
        append_line(
            &self.layout.queue(),
            "queue record",
            &record,
            MAX_LINE_BYTES,
        )?;
        self.ledger_append(&LedgerEvent::Transition(Transition {
            schema_version: SCHEMA_VERSION,
            request_id: record.request_id.clone(),
            project_id: record.project_id.clone(),
            state: RequestState::Queued,
            at: now,
            actor: actor.to_string(),
            origin: Some(Origin {
                goal_id: record.goal_id,
                role: record.role.clone(),
                kind,
                dedupe_key: record.dedupe_key.clone(),
                created_at: now,
            }),
            batch_id: None,
            lease_expires_at: None,
            draft_id: None,
            reason: None,
        }))?;
        audit.record(
            actor,
            "queued",
            &record.project_id,
            Some(&record.request_id),
            Some(record.goal_id),
            None,
            serde_json::json!({
                "from": null, "to": "queued", "kind": kind.as_str(),
                "role": sanitize(&record.role), "blocks_goal": record.blocks_goal,
            }),
        );
        Ok(EnqueueOutcome::Queued {
            request_id: record.request_id,
        })
    }

    fn status_for_requester(
        &self,
        project_id: &str,
        request_id: &str,
        goal_id: Uuid,
    ) -> Result<Option<RequestState>, IntakeError> {
        let snap = self.load()?;
        Ok(snap
            .get(request_id)
            .filter(|t| t.project_id == project_id && t.requested_by(goal_id))
            .map(|t| t.state))
    }

    fn list(&self, filter: &ListFilter, now: DateTime<Utc>) -> Result<ListReport, IntakeError> {
        let snap = self.load()?;
        let mut requests: Vec<RequestView> = snap
            .tracked
            .iter()
            .filter(|t| {
                filter
                    .project_id
                    .as_deref()
                    .is_none_or(|p| t.project_id == p)
            })
            .filter(|t| filter.state.is_none_or(|s| t.state == s))
            .filter(|t| filter.goal_id.is_none_or(|g| t.requested_by(g)))
            .map(|t| view_of(t, snap.records.get(&t.request_id), now))
            .collect();
        // Stable: requests sharing a timestamp keep ledger (arrival) order.
        requests.sort_by_key(|r| r.created_at);
        Ok(ListReport {
            requests,
            warnings: snap.warnings,
        })
    }

    fn drain(&self, req: &DrainRequest) -> Result<Option<Batch>, IntakeError> {
        let _lock = self.lock()?;
        let mut audit = Auditor::new(self.audit_log.as_deref());
        self.recover_locked(req.now, &req.actor, &mut audit)?;
        let snap = self.load()?;
        let mut pending: Vec<&Tracked> = snap
            .tracked
            .iter()
            .filter(|t| t.state == RequestState::Queued && snap.queue_ids.contains(&t.request_id))
            .collect();
        // Stable: requests sharing a timestamp keep ledger (arrival) order.
        pending.sort_by_key(|t| t.created_at);
        if pending.is_empty() || req.max_records == 0 {
            return Ok(None);
        }

        let batch_id = format!(
            "{}-{}",
            req.now.format("%Y%m%dT%H%M%SZ"),
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let batch_path = self.layout.batch(&batch_id);
        // The commit point: after this rename the queued lines live in the
        // batch file and every later step is recoverable from files + clock.
        std::fs::rename(self.layout.queue(), &batch_path).map_err(|source| IntakeError::Io {
            op: "rename the queue to a batch file",
            path: self.layout.queue(),
            source,
        })?;
        let lease = req.lease;
        let lease_expires_at =
            req.now + chrono::Duration::from_std(lease).unwrap_or(chrono::Duration::minutes(30));

        let take = pending.len().min(req.max_records);
        let mut records = Vec::with_capacity(take);
        for t in &pending[..take] {
            let ev = self.transition_event(
                t,
                RequestState::Claimed,
                req.now,
                &req.actor,
                Some(batch_id.clone()),
                Some(lease_expires_at),
                None,
                None,
            );
            self.ledger_append(&ev)?;
            audit.record(
                &req.actor,
                "claimed",
                &t.project_id,
                Some(&t.request_id),
                Some(t.goal_id),
                None,
                serde_json::json!({
                    "from": "queued", "to": "claimed", "batch_id": batch_id,
                    "lease_expires_at": lease_expires_at.to_rfc3339(),
                }),
            );
            if let Some(r) = snap.records.get(&t.request_id) {
                records.push(r.clone());
            }
        }
        // Requests beyond `max_records` go back into a fresh queue file.
        for t in &pending[take..] {
            if let Some(r) = snap.records.get(&t.request_id) {
                append_line(&self.layout.queue(), "queue record", r, MAX_LINE_BYTES)?;
            }
        }
        Ok(Some(Batch {
            batch_id,
            path: batch_path,
            claimed_at: req.now,
            lease_expires_at,
            records,
        }))
    }

    fn acknowledge(&self, req: &AckRequest) -> Result<(), IntakeError> {
        let _lock = self.lock()?;
        let snap = self.load()?;
        let target = req.ack.target();
        // Validate everything first so a bad id leaves the ledger untouched.
        let mut moves: Vec<&Tracked> = Vec::new();
        for id in &req.request_ids {
            let t = snap
                .get(id)
                .filter(|t| t.project_id == req.project_id)
                .ok_or_else(|| IntakeError::UnknownRequest(id.clone()))?;
            if !t.state.can_become(target) {
                return Err(IntakeError::InvalidTransition {
                    request_id: id.clone(),
                    from: t.state,
                    to: target,
                    allowed: [
                        RequestState::Queued,
                        RequestState::Claimed,
                        RequestState::Drafted,
                        RequestState::Applied,
                        RequestState::Rejected,
                        RequestState::Expired,
                    ]
                    .into_iter()
                    .filter(|s| t.state.can_become(*s))
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                });
            }
            moves.push(t);
        }
        let (draft_id, reason) = match &req.ack {
            Ack::Drafted { draft_id } => (Some(draft_id.clone()), None),
            Ack::Applied { draft_id } => (draft_id.clone(), None),
            Ack::Rejected { reason } | Ack::Expired { reason } => (None, Some(reason.clone())),
        };
        let mut audit = Auditor::new(self.audit_log.as_deref());
        for t in moves {
            let ev = self.transition_event(
                t,
                target,
                req.now,
                &req.actor,
                None,
                None,
                draft_id.clone(),
                reason.clone(),
            );
            self.ledger_append(&ev)?;
            let draft_for_audit = draft_id.as_deref().or(t.draft_id.as_deref());
            audit.record(
                &req.actor,
                target.as_str(),
                &t.project_id,
                Some(&t.request_id),
                Some(t.goal_id),
                draft_for_audit,
                serde_json::json!({
                    "from": t.state.as_str(), "to": target.as_str(),
                    "reason": reason.as_deref().map(|r| sanitize(&clip_reason(r.to_string()))),
                }),
            );
        }
        Ok(())
    }

    fn recover(&self, now: DateTime<Utc>, actor: &str) -> Result<RecoveryReport, IntakeError> {
        let _lock = self.lock()?;
        let mut audit = Auditor::new(self.audit_log.as_deref());
        self.recover_locked(now, actor, &mut audit)
    }
}

impl FileIntakeStore {
    fn recover_locked(
        &self,
        now: DateTime<Utc>,
        actor: &str,
        audit: &mut Auditor<'_>,
    ) -> Result<RecoveryReport, IntakeError> {
        let mut report = RecoveryReport::default();
        let snap = self.load()?;
        if snap.tracked.is_empty() {
            return Ok(report);
        }
        let mut in_queue = snap.queue_ids.clone();
        let ttl = self.ttl();
        let max_attempts = self.settings.max_claim_attempts;

        for t in &snap.tracked {
            // Heal: queue record whose ledger `queued` event never landed.
            if !t.in_ledger {
                let rec = &snap.records[&t.request_id];
                self.ledger_append(&LedgerEvent::Transition(Transition {
                    schema_version: SCHEMA_VERSION,
                    request_id: t.request_id.clone(),
                    project_id: t.project_id.clone(),
                    state: RequestState::Queued,
                    at: rec.created_at,
                    actor: actor.to_string(),
                    origin: Some(t.origin()),
                    batch_id: None,
                    lease_expires_at: None,
                    draft_id: None,
                    reason: Some("ledger event restored by recovery".to_string()),
                }))?;
                audit.record(
                    actor,
                    "healed",
                    &t.project_id,
                    Some(&t.request_id),
                    Some(t.goal_id),
                    None,
                    serde_json::json!({ "from": null, "to": "queued" }),
                );
                report.healed.push(t.request_id.clone());
            }

            let expire = |reason: String,
                          report: &mut RecoveryReport,
                          audit: &mut Auditor<'_>|
             -> Result<(), IntakeError> {
                let ev = self.transition_event(
                    t,
                    RequestState::Expired,
                    now,
                    actor,
                    None,
                    None,
                    None,
                    Some(reason.clone()),
                );
                self.ledger_append(&ev)?;
                audit.record(
                    actor,
                    "expired",
                    &t.project_id,
                    Some(&t.request_id),
                    Some(t.goal_id),
                    None,
                    serde_json::json!({ "from": t.state.as_str(), "to": "expired", "reason": reason }),
                );
                report.expired.push(t.request_id.clone());
                Ok(())
            };

            match t.state {
                RequestState::Queued => {
                    if now - t.created_at > ttl {
                        expire(
                            format!(
                                "not drained within {} day(s) of being queued",
                                ttl.num_days()
                            ),
                            &mut report,
                            audit,
                        )?;
                    } else if !in_queue.contains(&t.request_id) {
                        // Queued but only a batch file holds it (a crash
                        // between the drain rename and re-queueing).
                        match snap.records.get(&t.request_id) {
                            Some(r) => {
                                append_line(
                                    &self.layout.queue(),
                                    "queue record",
                                    r,
                                    MAX_LINE_BYTES,
                                )?;
                                in_queue.insert(t.request_id.clone());
                                audit.record(
                                    actor,
                                    "restored_to_queue",
                                    &t.project_id,
                                    Some(&t.request_id),
                                    Some(t.goal_id),
                                    None,
                                    serde_json::json!({ "from": "queued", "to": "queued" }),
                                );
                                report.restored_to_queue.push(t.request_id.clone());
                            }
                            None => expire(
                                "request body is missing from the queue and every batch file"
                                    .to_string(),
                                &mut report,
                                audit,
                            )?,
                        }
                    }
                }
                RequestState::Claimed if t.lease_expires_at.is_some_and(|l| l <= now) => {
                    if t.claim_count >= max_attempts {
                        expire(
                            format!(
                                "claim lease lapsed with no acknowledgement after {} attempt(s)",
                                t.claim_count
                            ),
                            &mut report,
                            audit,
                        )?;
                    } else if let Some(r) = snap.records.get(&t.request_id) {
                        if !in_queue.contains(&t.request_id) {
                            append_line(&self.layout.queue(), "queue record", r, MAX_LINE_BYTES)?;
                            in_queue.insert(t.request_id.clone());
                        }
                        let reason = format!(
                            "claim lease on batch {} lapsed without acknowledgement (attempt {} of {max_attempts}); returned to the queue",
                            t.batch_id.as_deref().unwrap_or("?"),
                            t.claim_count
                        );
                        let ev = self.transition_event(
                            t,
                            RequestState::Queued,
                            now,
                            actor,
                            None,
                            None,
                            None,
                            Some(reason.clone()),
                        );
                        self.ledger_append(&ev)?;
                        audit.record(
                            actor,
                            "requeued",
                            &t.project_id,
                            Some(&t.request_id),
                            Some(t.goal_id),
                            None,
                            serde_json::json!({ "from": "claimed", "to": "queued", "reason": reason }),
                        );
                        report.requeued.push(t.request_id.clone());
                    } else {
                        expire(
                            "claim lease lapsed and the batch file holding the body is missing"
                                .to_string(),
                            &mut report,
                            audit,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(report)
    }

    /// Check a batch id taken from outside before it is turned into a path.
    pub fn batch_path(&self, batch_id: &str) -> Result<PathBuf, IntakeError> {
        if !valid_batch_id(batch_id) {
            return Err(IntakeError::BadBatchId(batch_id.to_string()));
        }
        Ok(self.layout.batch(batch_id))
    }
}
