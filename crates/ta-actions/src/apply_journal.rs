// apply_journal.rs: the two-phase apply journal and startup recovery (v0.17.12.3).
//
// `ta draft apply` is atomic for everything local. The protocol:
//
//   1. PREPARE   Check policy, paths, conflicts (the caller's job). Then `begin`
//                writes a fsynced journal that lists the queued actions and the
//                target root. Nothing has changed yet.
//   2. COMMIT    For every file the apply is about to overwrite, `record_prior`
//                saves the prior content (or the fact that the file did not
//                exist) into the journal BEFORE the file is touched. Then, in
//                order: each file is installed (staged copy, then rename), the
//                local commit is made if requested, the draft is marked Applied
//                (a single atomic file replace: this is THE commit point), and
//                every action is written to the durable queue in one append.
//   3. COMPLETE  The journal is removed.
//
// The journal lives in the daemon-only per-user state directory
// (`ProjectState::apply_journal_dir()`), outside the project tree and outside
// every staging workspace, so an agent cannot write, forge or delete it.
//
// RECOVERY (`recover_all`, run at daemon startup and before `ta draft apply`).
// For every journal that never completed, the draft's durable status decides:
//
//   * Marked Applied      -> ROLL FORWARD: enqueue the actions (idempotent by
//                            key, so a half-finished enqueue is safe), finish.
//   * NOT marked Applied  -> ROLL BACK: restore every recorded prior, remove
//                            staged temp files, undo the local commit if one
//                            was made, finish.
//
// So a crash at ANY step leaves either the fully applied state or the fully
// rolled back state, never a mix. The status flip is the single decision
// point, which is why it must be one atomic replace of one file.
//
// Authority: docs/superpowers/specs/2026-10-07-atomic-apply-and-action-queue-design.md.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::queue::{ActionQueue, Clock, NewAction};

const JOURNAL_FILE: &str = "journal.jsonl";
const BACKUP_DIR: &str = "backups";
/// Suffix of the staged temp file written next to a destination before the rename.
pub const STAGED_SUFFIX: &str = ".ta-staged";

/// Errors from the journal. Each says what was attempted and what to do.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error(
        "apply journal: could not {op} {path}: {source}. Nothing was applied by this step. \
         Check that the daemon state directory is writable and the disk is not full, then re-run \
         `ta draft apply`."
    )]
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "apply journal: refusing path '{rel}': {why}. The journal only restores paths inside \
         the project. Delete {journal} after checking it by hand."
    )]
    UnsafePath {
        rel: String,
        why: &'static str,
        journal: PathBuf,
    },
    #[error(
        "apply journal: rollback incomplete: {}. The journal is kept so nothing is lost; fix the \
         cause and restart the daemon (or re-run `ta draft apply`) to retry, or restore the \
         listed files by hand.",
        .failures.join("; ")
    )]
    RollbackIncomplete { failures: Vec<String> },
    #[error("apply journal: could not serialize a record: {0}. This is a bug; report it.")]
    Serialize(#[from] serde_json::Error),
    /// Test-only fault: simulates the process dying right after `step`.
    #[error("injected crash after journal step {0:?}")]
    InjectedCrash(Step),
}

fn io<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> JournalError + 'a {
    move |source| JournalError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

/// Removes a file on a cleanup path. A failure is logged with the path and the
/// operation instead of being dropped; a file that is already gone is fine.
fn cleanup_file(path: &Path, op: &'static str) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), operation = op, error = %e,
                "apply journal cleanup failed; remove the file by hand if it lingers");
        }
    }
}

/// Removes a directory tree on a cleanup path, logging failures like [`cleanup_file`].
fn cleanup_dir(path: &Path, op: &'static str) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), operation = op, error = %e,
                "apply journal cleanup failed; remove the directory by hand if it lingers");
        }
    }
}

/// The journal steps, in protocol order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Prepared,
    CommitBegun,
    FileMoved(String),
    LocalCommitted,
    MarkedApplied,
    Enqueued,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    Begin {
        draft_id: String,
        target_root: PathBuf,
        started_at: DateTime<Utc>,
        prior_head: Option<String>,
        actions: Vec<NewAction>,
    },
    Prior {
        rel: String,
        existed: bool,
        backup: Option<String>,
    },
    CommitBegun,
    FileMoved {
        rel: String,
    },
    LocalCommitted {
        commit: String,
    },
    MarkedApplied,
    Enqueued,
}

/// A rel path must stay inside the target root.
fn check_rel(rel: &str, journal: &Path) -> Result<(), JournalError> {
    let p = Path::new(rel);
    let bad = |why| JournalError::UnsafePath {
        rel: rel.to_string(),
        why,
        journal: journal.to_path_buf(),
    };
    if rel.is_empty() {
        return Err(bad("empty path"));
    }
    if p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(bad("absolute path"));
    }
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err(bad("contains `..`")),
            _ => return Err(bad("not a plain relative path")),
        }
    }
    Ok(())
}

/// Joins a forward-slash rel path onto a root using the platform separator.
fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in rel.split(['/', '\\']).filter(|s| !s.is_empty()) {
        out.push(part);
    }
    out
}

fn staged_path(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(STAGED_SUFFIX);
    dst.with_file_name(name)
}

/// Flush an existing file to disk. Opened with write access (without
/// truncating) because Windows `FlushFileBuffers` refuses a read-only handle
/// with "Access is denied"; Unix accepts either.
fn fsync_existing_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.sync_all())
}

/// Staged copy then rename: the destination is either the old file or the
/// complete new file, never a partial one. Used for installs and for restores.
pub fn install_file(src: &Path, dst: &Path) -> Result<(), JournalError> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(io("create the directory for", parent))?;
    }
    let tmp = staged_path(dst);
    std::fs::copy(src, &tmp).map_err(io("stage a copy for", dst))?;
    fsync_existing_file(&tmp).map_err(io("fsync the staged copy of", &tmp))?;
    std::fs::rename(&tmp, dst).map_err(io("move the staged copy into place at", dst))?;
    Ok(())
}

/// The open journal for one apply.
pub struct ApplyJournal {
    dir: PathBuf,
    draft_id: String,
    target_root: PathBuf,
    seen: HashSet<String>,
    next_backup: usize,
    /// Test-only: die (return `InjectedCrash`) right after this step is journaled.
    pub crash_after: Option<Step>,
}

impl ApplyJournal {
    /// PREPARE. Writes the fsynced journal. `actions` are the queue entries to
    /// write at commit; `prior_head` is the VCS head before any local commit.
    pub fn begin(
        root: &Path,
        draft_id: &str,
        target_root: &Path,
        actions: Vec<NewAction>,
        prior_head: Option<String>,
    ) -> Result<Self, JournalError> {
        let dir = root.join(draft_id);
        std::fs::create_dir_all(dir.join(BACKUP_DIR)).map_err(io("create the journal in", &dir))?;
        restrict_dir(root);
        restrict_dir(&dir);
        let mut j = Self {
            dir,
            draft_id: draft_id.to_string(),
            target_root: target_root.to_path_buf(),
            seen: HashSet::new(),
            next_backup: 0,
            crash_after: None,
        };
        // A stale journal for the same draft is replaced only if it is closed
        // or empty; an open one must be recovered first (the caller runs
        // `recover_all` before `begin`).
        let path = j.journal_path();
        if path.exists() {
            cleanup_file(&path, "remove a stale journal before begin");
        }
        j.append(&Record::Begin {
            draft_id: draft_id.to_string(),
            target_root: target_root.to_path_buf(),
            started_at: Utc::now(),
            prior_head,
            actions,
        })?;
        j.maybe_crash(Step::Prepared)?;
        Ok(j)
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.join(JOURNAL_FILE)
    }

    fn append(&mut self, rec: &Record) -> Result<(), JournalError> {
        let path = self.journal_path();
        let mut line = serde_json::to_string(rec)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io("open the journal", &path))?;
        f.write_all(line.as_bytes())
            .map_err(io("append to the journal", &path))?;
        f.sync_all().map_err(io("fsync the journal", &path))?;
        Ok(())
    }

    fn maybe_crash(&self, step: Step) -> Result<(), JournalError> {
        if self.crash_after.as_ref() == Some(&step) {
            return Err(JournalError::InjectedCrash(step));
        }
        Ok(())
    }

    /// Saves the prior state of `rel` (relative to the target root) BEFORE the
    /// caller touches it. Only the first call per path is kept: that is the
    /// true pre-apply state.
    pub fn record_prior(&mut self, rel: &str) -> Result<(), JournalError> {
        let rel_norm = rel.replace('\\', "/");
        check_rel(&rel_norm, &self.journal_path())?;
        if !self.seen.insert(rel_norm.clone()) {
            return Ok(());
        }
        let abs = join_rel(&self.target_root, &rel_norm);
        let (existed, backup) = if abs.is_file() {
            let name = format!("{}", self.next_backup);
            self.next_backup += 1;
            let dst = self.dir.join(BACKUP_DIR).join(&name);
            std::fs::copy(&abs, &dst).map_err(io("back up", &abs))?;
            fsync_existing_file(&dst).map_err(io("fsync the backup of", &dst))?;
            (true, Some(name))
        } else {
            (false, None)
        };
        self.append(&Record::Prior {
            rel: rel_norm,
            existed,
            backup,
        })
    }

    /// COMMIT begins. After this record a crash is still rolled back unless the
    /// draft was marked Applied, so this is bookkeeping, not the decision.
    pub fn commit_begun(&mut self) -> Result<(), JournalError> {
        self.append(&Record::CommitBegun)?;
        self.maybe_crash(Step::CommitBegun)
    }

    /// Records the prior then installs `src` at `rel` (staged copy, rename),
    /// then journals the move. For callers that move files themselves, call
    /// `record_prior` before and `file_moved` after instead.
    pub fn install(&mut self, rel: &str, src: &Path) -> Result<(), JournalError> {
        self.record_prior(rel)?;
        let dst = join_rel(&self.target_root, rel);
        install_file(src, &dst)?;
        self.file_moved(rel)
    }

    /// Journals that `rel` is now in its new state.
    pub fn file_moved(&mut self, rel: &str) -> Result<(), JournalError> {
        let rel = rel.replace('\\', "/");
        self.append(&Record::FileMoved { rel: rel.clone() })?;
        self.maybe_crash(Step::FileMoved(rel))
    }

    /// Journals the local commit.
    pub fn local_committed(&mut self, commit: &str) -> Result<(), JournalError> {
        self.append(&Record::LocalCommitted {
            commit: commit.to_string(),
        })?;
        self.maybe_crash(Step::LocalCommitted)
    }

    /// Journals that the draft's durable status is now Applied. Call AFTER the
    /// atomic status write.
    pub fn marked_applied(&mut self) -> Result<(), JournalError> {
        self.append(&Record::MarkedApplied)?;
        self.maybe_crash(Step::MarkedApplied)
    }

    /// Writes every action to the queue in one append, journals it, and closes
    /// the journal. Returns the number of actions newly queued.
    pub fn enqueue_and_complete(
        mut self,
        queue: &ActionQueue,
        clock: &dyn Clock,
    ) -> Result<usize, EnqueueError> {
        let actions = read_state(&self.journal_path())
            .map(|s| s.actions)
            .unwrap_or_default();
        let report = queue
            .enqueue_batch(&actions, clock)
            .map_err(EnqueueError::Queue)?;
        self.append(&Record::Enqueued)
            .map_err(EnqueueError::Journal)?;
        self.maybe_crash(Step::Enqueued)
            .map_err(EnqueueError::Journal)?;
        self.close();
        Ok(report.added.len())
    }

    /// The caller rolled everything back itself (an error before the commit
    /// point): drop the journal. Priors are restored first so this is safe to
    /// call from any failure path.
    ///
    /// If any restore fails the journal is KEPT (recovery retries it on the next
    /// start) and the failure is logged with every path and its backup.
    pub fn abandon_after_rollback(self) {
        if let Ok(state) = read_state(&self.journal_path()) {
            if let Err(e) = restore_priors(&state, &self.journal_path()) {
                tracing::error!(journal = %self.journal_path().display(), error = %e,
                    operation = "restore priors after in-process rollback",
                    "rollback incomplete; the journal is kept for recovery");
                return;
            }
        }
        self.close();
    }

    fn close(self) {
        cleanup_dir(&self.dir, "remove the closed journal directory");
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn draft_id(&self) -> &str {
        &self.draft_id
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    #[error("{0}")]
    Queue(crate::queue::QueueError),
    #[error("{0}")]
    Journal(JournalError),
}

fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!(path = %dir.display(), operation = "restrict journal directory to 0700",
                error = %e, "the journal directory may be readable by other users; run chmod 700 on it");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

// ── Reading and recovery ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct State {
    begun: bool,
    draft_id: String,
    target_root: PathBuf,
    prior_head: Option<String>,
    actions: Vec<NewAction>,
    priors: Vec<(String, bool, Option<String>)>,
    moved: Vec<String>,
    local_commit: Option<String>,
    marked_applied: bool,
    dir: PathBuf,
}

fn read_state(journal: &Path) -> Result<State, JournalError> {
    let text = std::fs::read_to_string(journal).map_err(io("read the journal", journal))?;
    let mut s = State {
        dir: journal.parent().map(Path::to_path_buf).unwrap_or_default(),
        ..State::default()
    };
    // `lines()` strips \r\n too, so a CRLF-normalised journal reads the same.
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // A torn last line (crash mid-append) is ignored: the step it would
        // have recorded had not completed.
        let Ok(rec) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        match rec {
            Record::Begin {
                draft_id,
                target_root,
                prior_head,
                actions,
                ..
            } => {
                s.begun = true;
                s.draft_id = draft_id;
                s.target_root = target_root;
                s.prior_head = prior_head;
                s.actions = actions;
            }
            Record::Prior {
                rel,
                existed,
                backup,
            } => s.priors.push((rel, existed, backup)),
            Record::CommitBegun | Record::Enqueued => {}
            Record::FileMoved { rel } => s.moved.push(rel),
            Record::LocalCommitted { commit } => s.local_commit = Some(commit),
            Record::MarkedApplied => s.marked_applied = true,
        }
    }
    Ok(s)
}

/// Restores every recorded prior, newest first. Returns how many paths were
/// restored. Also removes staged temp files for recorded paths.
fn restore_priors(s: &State, journal: &Path) -> Result<usize, JournalError> {
    let mut restored = 0;
    let mut failures = Vec::new();
    for (rel, existed, backup) in s.priors.iter().rev() {
        check_rel(rel, journal)?;
        let dst = join_rel(&s.target_root, rel);
        cleanup_file(
            &staged_path(&dst),
            "remove a staged temp file during restore",
        );
        if *existed {
            let Some(name) = backup else { continue };
            let src = s.dir.join(BACKUP_DIR).join(name);
            match install_file(&src, &dst) {
                Ok(()) => restored += 1,
                Err(e) => {
                    tracing::warn!(path = %dst.display(), backup = %src.display(), error = %e,
                        operation = "restore a file from its backup", "restore failed");
                    failures.push(format!(
                        "{}: {e}; restore manually from {}",
                        dst.display(),
                        src.display()
                    ));
                }
            }
        } else if dst.exists() {
            match std::fs::remove_file(&dst) {
                Ok(()) => restored += 1,
                Err(e) => {
                    tracing::warn!(path = %dst.display(), error = %e,
                        operation = "remove a file the apply created", "restore failed");
                    failures.push(format!(
                        "{}: {e}; it did not exist before the apply, delete it manually",
                        dst.display()
                    ));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(restored)
    } else {
        Err(JournalError::RollbackIncomplete { failures })
    }
}

/// What recovery needs to know about the outside world.
pub trait ApplyEffects {
    /// Whether the draft's DURABLE status is Applied. This is the decision
    /// point: it must read the same atomically-replaced file the apply wrote.
    fn is_applied(&self, draft_id: &str) -> Result<bool, String>;
    /// Whether a live `ta draft apply` currently holds the apply lock for this
    /// project. Recovery must never touch a journal that is still in use.
    fn apply_in_progress(&self, _target_root: &Path) -> bool {
        false
    }
    /// Undoes the local commit while it is unpushed. Must only touch the
    /// commit's own effect, never unrelated working-tree changes.
    fn undo_local_commit(
        &self,
        target_root: &Path,
        commit: &str,
        prior_head: Option<&str>,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The draft was Applied; the queue now holds every action.
    RolledForward { enqueued: usize },
    /// The draft was not Applied; every file is back to its prior state.
    RolledBack {
        restored: usize,
        commit_undone: bool,
    },
    /// Nothing recoverable was in the directory; it was removed.
    Discarded,
    /// A live apply holds the lock, so its journal was left alone.
    InProgress,
    /// Recovery could not finish. The journal is kept so the next start retries.
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub draft_id: String,
    pub outcome: RecoveryOutcome,
}

/// Recovers every interrupted apply under `root`. Safe to run repeatedly and
/// concurrently with nothing else applying (the caller holds the apply lock or
/// is the daemon at startup).
pub fn recover_all(
    root: &Path,
    effects: &dyn ApplyEffects,
    queue: &ActionQueue,
    clock: &dyn Clock,
) -> Vec<RecoveryReport> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let draft_dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let journal = dir.join(JOURNAL_FILE);
        let state = match read_state(&journal) {
            Ok(s) if s.begun => s,
            _ => {
                // Crashed before `Begin` was durable: nothing was touched.
                cleanup_dir(&dir, "discard a journal that never began");
                out.push(RecoveryReport {
                    draft_id: draft_dir_name,
                    outcome: RecoveryOutcome::Discarded,
                });
                continue;
            }
        };
        let draft_id = state.draft_id.clone();
        let outcome = recover_one(&dir, &journal, &state, effects, queue, clock);
        match &outcome {
            RecoveryOutcome::Failed { reason } => {
                tracing::error!(draft_id = %draft_id, journal = %journal.display(), reason = %reason,
                    "interrupted apply could not be recovered; the journal is kept and recovery will retry on the next start");
            }
            other => {
                tracing::warn!(draft_id = %draft_id, outcome = ?other,
                    "recovered an interrupted apply");
            }
        }
        out.push(RecoveryReport { draft_id, outcome });
    }
    out
}

fn recover_one(
    dir: &Path,
    journal: &Path,
    s: &State,
    effects: &dyn ApplyEffects,
    queue: &ActionQueue,
    clock: &dyn Clock,
) -> RecoveryOutcome {
    let fail = |what: String| RecoveryOutcome::Failed {
        reason: format!(
            "{what}. Journal: {}. Fix the cause and restart the daemon (or run `ta draft apply \
             {}` again), which retries recovery.",
            journal.display(),
            s.draft_id
        ),
    };
    if effects.apply_in_progress(&s.target_root) {
        return RecoveryOutcome::InProgress;
    }
    let applied = match effects.is_applied(&s.draft_id) {
        Ok(a) => a,
        Err(e) => {
            return fail(format!(
                "could not read draft {}'s status to decide between finishing and undoing the \
                 apply: {e}",
                s.draft_id
            ))
        }
    };
    if applied {
        // ROLL FORWARD: the commit point was passed. Enqueue is idempotent.
        return match queue.enqueue_batch(&s.actions, clock) {
            Ok(r) => {
                cleanup_dir(dir, "remove the journal after roll forward");
                RecoveryOutcome::RolledForward {
                    enqueued: r.added.len(),
                }
            }
            Err(e) => fail(format!("could not enqueue the draft's actions: {e}")),
        };
    }
    // ROLL BACK.
    let mut commit_undone = false;
    if let Some(commit) = &s.local_commit {
        match effects.undo_local_commit(&s.target_root, commit, s.prior_head.as_deref()) {
            Ok(()) => commit_undone = true,
            Err(e) => return fail(format!("could not undo the local commit {commit}: {e}")),
        }
    }
    match restore_priors(s, journal) {
        Ok(restored) => {
            cleanup_dir(dir, "remove the journal after roll back");
            RecoveryOutcome::RolledBack {
                restored,
                commit_undone,
            }
        }
        Err(e) => fail(format!("could not restore the files: {e}")),
    }
}

// ── Tests: crash injection at every journal step ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{ManualClock, QueueState};
    use crate::reversibility::{Idempotency, Reversibility};
    use std::cell::RefCell;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn action(id: &str, class: Reversibility) -> NewAction {
        NewAction {
            draft_id: "d1".into(),
            action_id: id.into(),
            tool_name: "ta_external_action:email".into(),
            parameters: serde_json::json!({}),
            description: id.into(),
            class,
            undo: None,
            idempotency: Idempotency::None,
            undo_of: None,
        }
    }

    /// The durable "draft status" and "local commit" of the fake world.
    struct World {
        root: PathBuf,
    }
    impl World {
        fn status_path(&self) -> PathBuf {
            self.root.join("state").join("status")
        }
        fn commit_path(&self) -> PathBuf {
            self.root.join("state").join("commit")
        }
        fn set_applied(&self) {
            // One atomic replace of one file: the commit point.
            let tmp = self.root.join("state").join("status.tmp");
            std::fs::write(&tmp, "Applied").unwrap();
            std::fs::rename(&tmp, self.status_path()).unwrap();
        }
        fn applied(&self) -> bool {
            std::fs::read_to_string(self.status_path()).ok().as_deref() == Some("Applied")
        }
        fn committed(&self) -> bool {
            self.commit_path().exists()
        }
    }
    impl ApplyEffects for World {
        fn is_applied(&self, _: &str) -> Result<bool, String> {
            Ok(self.applied())
        }
        fn undo_local_commit(&self, _: &Path, commit: &str, _: Option<&str>) -> Result<(), String> {
            assert_eq!(commit, "c0ffee");
            let _ = std::fs::remove_file(self.commit_path());
            Ok(())
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        world: World,
        target: PathBuf,
        journals: PathBuf,
        queue: ActionQueue,
        clock: ManualClock,
        staged: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let target = root.join("project");
        std::fs::create_dir_all(target.join("src")).unwrap();
        std::fs::write(target.join("src").join("a.txt"), "old a\r\n").unwrap();
        std::fs::write(target.join("src").join("b.txt"), "old b").unwrap();
        // c.txt does not exist yet: the apply creates it.
        let staged = root.join("staging");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("a.txt"), "NEW a\r\n").unwrap();
        std::fs::write(staged.join("b.txt"), "NEW b").unwrap();
        std::fs::write(staged.join("c.txt"), "NEW c").unwrap();
        Fixture {
            queue: ActionQueue::open(root.join("queue")).unwrap(),
            journals: root.join("journals"),
            world: World { root },
            target,
            clock: ManualClock::new(t0()),
            staged,
            _tmp: tmp,
        }
    }

    const FILES: [(&str, &str); 3] = [
        ("src/a.txt", "a.txt"),
        ("src/b.txt", "b.txt"),
        ("src/c.txt", "c.txt"),
    ];

    /// Runs the apply protocol, dying right after `crash`. `None` runs to completion.
    fn run_apply(f: &Fixture, crash: Option<Step>) -> Result<usize, JournalError> {
        let mut j = ApplyJournal::begin(
            &f.journals,
            "d1",
            &f.target,
            vec![
                action("slack", Reversibility::Compensable),
                action("mail", Reversibility::Irrecoverable),
            ],
            Some("base".into()),
        )?;
        j.crash_after = crash;
        // Prepared crash point fires inside `begin` only if set before; emulate:
        if j.crash_after == Some(Step::Prepared) {
            return Err(JournalError::InjectedCrash(Step::Prepared));
        }
        j.commit_begun()?;
        for (rel, staged) in FILES {
            j.install(rel, &f.staged.join(staged))?;
        }
        std::fs::write(f.world.commit_path(), "c0ffee").unwrap();
        j.local_committed("c0ffee")?;
        f.world.set_applied();
        j.marked_applied()?;
        j.enqueue_and_complete(&f.queue, &f.clock)
            .map_err(|e| match e {
                EnqueueError::Journal(j) => j,
                EnqueueError::Queue(q) => panic!("{q}"),
            })
    }

    fn read(f: &Fixture, rel: &str) -> Option<String> {
        std::fs::read_to_string(f.target.join(rel)).ok()
    }

    fn assert_fully_applied(f: &Fixture) {
        assert_eq!(read(f, "src/a.txt").as_deref(), Some("NEW a\r\n"));
        assert_eq!(read(f, "src/b.txt").as_deref(), Some("NEW b"));
        assert_eq!(read(f, "src/c.txt").as_deref(), Some("NEW c"));
        assert!(f.world.applied());
        assert!(f.world.committed());
        let snap = f.queue.snapshot().unwrap();
        assert_eq!(snap.entries.len(), 2, "both actions are queued");
        assert!(snap.entries.iter().all(|e| e.state == QueueState::Queued));
    }

    fn assert_fully_rolled_back(f: &Fixture) {
        assert_eq!(read(f, "src/a.txt").as_deref(), Some("old a\r\n"));
        assert_eq!(read(f, "src/b.txt").as_deref(), Some("old b"));
        assert_eq!(read(f, "src/c.txt"), None, "created file removed");
        assert!(!f.world.applied());
        assert!(!f.world.committed());
        assert!(
            f.queue.snapshot().unwrap().entries.is_empty(),
            "nothing queued"
        );
    }

    fn assert_no_leftovers(f: &Fixture) {
        assert!(
            std::fs::read_dir(&f.journals)
                .map(|d| d.count())
                .unwrap_or(0)
                == 0,
            "journal directory must be gone"
        );
        for rel in ["src/a.txt", "src/b.txt", "src/c.txt"] {
            assert!(
                !staged_path(&f.target.join(rel)).exists(),
                "staged temp left for {rel}"
            );
        }
    }

    #[test]
    fn clean_run_applies_everything_and_removes_the_journal() {
        let f = fixture();
        assert_eq!(run_apply(&f, None).unwrap(), 2);
        assert_fully_applied(&f);
        assert_no_leftovers(&f);
        // Recovery after a clean run finds nothing.
        assert!(recover_all(&f.journals, &f.world, &f.queue, &f.clock).is_empty());
    }

    /// Every crash point: recovery yields all-or-nothing, never a mix.
    #[test]
    fn crash_at_every_journal_step_recovers_to_all_or_nothing() {
        let points = [
            (Step::Prepared, false),
            (Step::CommitBegun, false),
            (Step::FileMoved("src/a.txt".into()), false),
            (Step::FileMoved("src/b.txt".into()), false),
            (Step::FileMoved("src/c.txt".into()), false),
            (Step::LocalCommitted, false),
            // The status flip is the commit point: from here on, roll forward.
            (Step::MarkedApplied, true),
            (Step::Enqueued, true),
        ];
        for (step, expect_applied) in points {
            let f = fixture();
            let err = run_apply(&f, Some(step.clone())).unwrap_err();
            assert!(
                matches!(err, JournalError::InjectedCrash(_)),
                "{step:?}: {err}"
            );

            let reports = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
            assert_eq!(reports.len(), 1, "{step:?}");
            if expect_applied {
                assert!(
                    matches!(reports[0].outcome, RecoveryOutcome::RolledForward { .. }),
                    "{step:?}: {:?}",
                    reports[0].outcome
                );
                assert_fully_applied(&f);
            } else {
                assert!(
                    matches!(reports[0].outcome, RecoveryOutcome::RolledBack { .. }),
                    "{step:?}: {:?}",
                    reports[0].outcome
                );
                assert_fully_rolled_back(&f);
            }
            assert_no_leftovers(&f);
            // Recovery is idempotent: a second pass has nothing to do.
            assert!(recover_all(&f.journals, &f.world, &f.queue, &f.clock).is_empty());
        }
    }

    #[test]
    fn crash_between_status_flip_and_enqueue_never_loses_or_doubles_actions() {
        let f = fixture();
        // Died after the enqueue append but before the journal knew: re-enqueue is idempotent.
        let _ = run_apply(&f, Some(Step::MarkedApplied));
        f.queue
            .enqueue_batch(&[action("slack", Reversibility::Compensable)], &f.clock)
            .unwrap(); // the enqueue landed, the journal record did not
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert_eq!(r[0].outcome, RecoveryOutcome::RolledForward { enqueued: 1 });
        assert_eq!(f.queue.snapshot().unwrap().entries.len(), 2);
    }

    #[test]
    fn torn_journal_tail_is_ignored_and_rolls_back() {
        let f = fixture();
        let _ = run_apply(&f, Some(Step::FileMoved("src/a.txt".into())));
        let jp = f.journals.join("d1").join(JOURNAL_FILE);
        let mut fh = std::fs::OpenOptions::new().append(true).open(&jp).unwrap();
        fh.write_all(b"{\"record\":\"file_moved\",\"rel\":\"src/b")
            .unwrap();
        drop(fh);
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert!(matches!(r[0].outcome, RecoveryOutcome::RolledBack { .. }));
        assert_fully_rolled_back(&f);
    }

    #[test]
    fn crlf_normalised_journal_recovers() {
        let f = fixture();
        let _ = run_apply(&f, Some(Step::LocalCommitted));
        let jp = f.journals.join("d1").join(JOURNAL_FILE);
        let text = std::fs::read_to_string(&jp).unwrap();
        std::fs::write(&jp, text.replace('\n', "\r\n")).unwrap();
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert_eq!(
            r[0].outcome,
            RecoveryOutcome::RolledBack {
                restored: 3,
                commit_undone: true
            }
        );
        assert_fully_rolled_back(&f);
    }

    #[test]
    fn crash_during_staged_copy_leaves_no_half_written_destination() {
        let f = fixture();
        // Die mid-install of a.txt: prior recorded, partial staged temp beside the destination.
        let mut j = ApplyJournal::begin(&f.journals, "d1", &f.target, vec![], None).unwrap();
        j.commit_begun().unwrap();
        j.record_prior("src/a.txt").unwrap();
        let dst = f.target.join("src").join("a.txt");
        std::fs::write(staged_path(&dst), "NE").unwrap(); // partial
        drop(j);
        // The destination is untouched until the rename.
        assert_eq!(read(&f, "src/a.txt").as_deref(), Some("old a\r\n"));
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert!(matches!(r[0].outcome, RecoveryOutcome::RolledBack { .. }));
        assert_fully_rolled_back(&f);
        assert_no_leftovers(&f);
    }

    #[test]
    fn failed_recovery_keeps_the_journal_and_says_what_to_do() {
        struct Broken;
        impl ApplyEffects for Broken {
            fn is_applied(&self, _: &str) -> Result<bool, String> {
                Err("package file unreadable".into())
            }
            fn undo_local_commit(&self, _: &Path, _: &str, _: Option<&str>) -> Result<(), String> {
                Ok(())
            }
        }
        let f = fixture();
        let _ = run_apply(&f, Some(Step::FileMoved("src/a.txt".into())));
        let r = recover_all(&f.journals, &Broken, &f.queue, &f.clock);
        let RecoveryOutcome::Failed { reason } = &r[0].outcome else {
            panic!("expected Failed, got {:?}", r[0].outcome);
        };
        assert!(reason.contains("package file unreadable"));
        assert!(
            reason.contains("restart the daemon"),
            "must say what to do: {reason}"
        );
        assert!(
            f.journals.join("d1").join(JOURNAL_FILE).exists(),
            "journal kept for retry"
        );
        // Once the cause is fixed, recovery completes.
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert!(matches!(r[0].outcome, RecoveryOutcome::RolledBack { .. }));
        assert_fully_rolled_back(&f);
    }

    #[test]
    fn a_live_apply_is_never_recovered_out_from_under_itself() {
        struct Busy(World);
        impl ApplyEffects for Busy {
            fn is_applied(&self, d: &str) -> Result<bool, String> {
                self.0.is_applied(d)
            }
            fn apply_in_progress(&self, _: &Path) -> bool {
                true
            }
            fn undo_local_commit(&self, r: &Path, c: &str, p: Option<&str>) -> Result<(), String> {
                self.0.undo_local_commit(r, c, p)
            }
        }
        let f = fixture();
        let _ = run_apply(&f, Some(Step::FileMoved("src/a.txt".into())));
        let busy = Busy(World {
            root: f.world.root.clone(),
        });
        let r = recover_all(&f.journals, &busy, &f.queue, &f.clock);
        assert_eq!(r[0].outcome, RecoveryOutcome::InProgress);
        assert_eq!(
            read(&f, "src/a.txt").as_deref(),
            Some("NEW a\r\n"),
            "files untouched"
        );
        assert!(f.journals.join("d1").join(JOURNAL_FILE).exists());
    }

    #[test]
    fn empty_journal_directory_is_discarded() {
        let f = fixture();
        std::fs::create_dir_all(f.journals.join("dX").join(BACKUP_DIR)).unwrap();
        let r = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        assert_eq!(r[0].outcome, RecoveryOutcome::Discarded);
        assert!(!f.journals.join("dX").exists());
    }

    #[test]
    fn journal_refuses_paths_that_escape_the_project() {
        let f = fixture();
        let mut j = ApplyJournal::begin(&f.journals, "d1", &f.target, vec![], None).unwrap();
        for bad in ["../outside", "/etc/passwd", "a/../../b", ""] {
            let e = j.record_prior(bad).unwrap_err().to_string();
            assert!(e.contains("refusing path"), "{bad}: {e}");
        }
        // Windows-style separators are normalised, not misread.
        j.record_prior("src\\a.txt").unwrap();
    }

    #[test]
    fn abandon_after_rollback_restores_and_removes_the_journal() {
        let f = fixture();
        let mut j = ApplyJournal::begin(&f.journals, "d1", &f.target, vec![], None).unwrap();
        j.install("src/a.txt", &f.staged.join("a.txt")).unwrap();
        assert_eq!(read(&f, "src/a.txt").as_deref(), Some("NEW a\r\n"));
        j.abandon_after_rollback();
        assert_eq!(read(&f, "src/a.txt").as_deref(), Some("old a\r\n"));
        assert_no_leftovers(&f);
    }

    /// Deletes the backup copies so every restore from them fails.
    fn break_backups(f: &Fixture) {
        let backups = f.journals.join("d1").join(BACKUP_DIR);
        for e in std::fs::read_dir(&backups).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
    }

    #[test]
    fn failed_restore_keeps_the_journal_and_reports_the_path_and_backup() {
        let f = fixture();
        let mut j = ApplyJournal::begin(&f.journals, "d1", &f.target, vec![], None).unwrap();
        j.install("src/a.txt", &f.staged.join("a.txt")).unwrap();
        j.install("src/c.txt", &f.staged.join("c.txt")).unwrap();
        break_backups(&f);
        // Abandon in-process: the restore of a.txt fails, so the journal stays.
        j.abandon_after_rollback();
        assert!(f.journals.join("d1").join(JOURNAL_FILE).exists());
        // The independent restore (removing the created c.txt) still ran.
        assert_eq!(read(&f, "src/c.txt"), None);

        // Recovery reports the incomplete rollback and keeps the journal.
        let reports = recover_all(&f.journals, &f.world, &f.queue, &f.clock);
        let RecoveryOutcome::Failed { reason } = &reports[0].outcome else {
            panic!("expected Failed, got {:?}", reports[0].outcome);
        };
        assert!(reason.contains("rollback incomplete"), "{reason}");
        assert!(reason.contains("a.txt"), "{reason}");
        assert!(reason.contains("restore manually from"), "{reason}");
        assert!(reason.contains(BACKUP_DIR), "{reason}");
        assert!(f.journals.join("d1").join(JOURNAL_FILE).exists());
    }

    #[test]
    fn no_commit_journaled_means_none_undone() {
        // The fake world's undo is only called when a commit was journaled.
        let calls = RefCell::new(0);
        struct Counting<'a>(&'a RefCell<i32>, World);
        impl ApplyEffects for Counting<'_> {
            fn is_applied(&self, d: &str) -> Result<bool, String> {
                self.1.is_applied(d)
            }
            fn undo_local_commit(&self, r: &Path, c: &str, p: Option<&str>) -> Result<(), String> {
                *self.0.borrow_mut() += 1;
                self.1.undo_local_commit(r, c, p)
            }
        }
        let f = fixture();
        let _ = run_apply(&f, Some(Step::FileMoved("src/c.txt".into())));
        let eff = Counting(
            &calls,
            World {
                root: f.world.root.clone(),
            },
        );
        recover_all(&f.journals, &eff, &f.queue, &f.clock);
        assert_eq!(
            *calls.borrow(),
            0,
            "no commit was journaled, so none is undone"
        );
    }
}
