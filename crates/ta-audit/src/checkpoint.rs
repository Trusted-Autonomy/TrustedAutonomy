// checkpoint.rs: signed audit-chain checkpoints (red-team CR-42).
//
// The goal audit ledger is hash-chained, but an attacker who can rewrite the
// file can rewrite the whole chain. A checkpoint anchors the chain HEAD
// outside the file it protects: every few entries the ledger's entry count
// and head hash are signed with the audit key and appended to a checkpoint
// file in the daemon-only state directory (not in the project tree).
//
// Each checkpoint signs: sequence number, time, entry count, head hash, the
// previous checkpoint's signature, and whether it starts a new epoch. Chaining
// the signatures means deleting or reordering a checkpoint is detected too.
//
// Verification re-reads the ledger and checks, for every checkpoint of the
// current epoch, that the ledger still has at least that many entries and that
// the entry at that position still hashes to the signed head. That detects
// truncation (fewer entries than a checkpoint promised) and in-place rewrite
// (a different line at a signed position), neither of which the unsigned hash
// chain alone can detect when the attacker recomputes it.
//
// A legitimate rewrite (`ta audit ledger gc` re-anchors the chain) records an
// epoch-start checkpoint, signed like any other, so history before it is no
// longer expected to be a prefix of the new ledger.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_credentials::state::{ControlSecret, CustodyOpts, ProjectState, StateLocator};

use crate::chain::AuditHmacKey;
use crate::error::AuditError;
use crate::hasher;

/// A checkpoint is recorded every this many appended entries.
pub const CHECKPOINT_EVERY: usize = 10;
const DOMAIN: &str = "ta-audit-checkpoint-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub seq: u64,
    pub recorded_at: DateTime<Utc>,
    /// Number of non-empty ledger lines covered.
    pub entries: usize,
    /// Hash of the last covered line (None for an empty ledger).
    pub head_hash: Option<String>,
    /// Signature of the previous checkpoint (empty for the first).
    pub prev_sig: String,
    /// True when the ledger was legitimately rewritten at this point.
    #[serde(default)]
    pub epoch_start: bool,
    pub sig: String,
}

impl Checkpoint {
    fn signing_input(&self) -> String {
        format!(
            "{DOMAIN}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.seq,
            self.recorded_at.to_rfc3339(),
            self.entries,
            self.head_hash.as_deref().unwrap_or(""),
            self.prev_sig,
            self.epoch_start
        )
    }
}

/// Outcome of verifying a ledger against its checkpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckpointReport {
    pub checkpoints: usize,
    pub verified: usize,
    pub problems: Vec<String>,
}

impl CheckpointReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn ledger_lines(ledger: &Path) -> Result<Vec<String>, AuditError> {
    if !ledger.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(ledger).map_err(|source| AuditError::OpenFailed {
        path: ledger.to_path_buf(),
        source,
    })?;
    Ok(text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

pub fn read_checkpoints(path: &Path) -> Result<Vec<Checkpoint>, AuditError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|source| AuditError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Checkpoint>(l).map_err(AuditError::from))
        .collect()
}

fn append_line(path: &Path, line: &str) -> Result<(), AuditError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AuditError::OpenFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|source| AuditError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Sign the ledger's current head and append the checkpoint to
/// `checkpoints_path`. `epoch_start` marks a legitimate rewrite.
pub fn record_checkpoint(
    ledger: &Path,
    key: &AuditHmacKey,
    checkpoints_path: &Path,
    epoch_start: bool,
) -> Result<Checkpoint, AuditError> {
    let lines = ledger_lines(ledger)?;
    let prior = read_checkpoints(checkpoints_path)?;
    let mut cp = Checkpoint {
        seq: prior.last().map(|c| c.seq + 1).unwrap_or(1),
        recorded_at: Utc::now(),
        entries: lines.len(),
        head_hash: lines.last().map(|l| hasher::hash_str(l)),
        prev_sig: prior.last().map(|c| c.sig.clone()).unwrap_or_default(),
        epoch_start,
        sig: String::new(),
    };
    cp.sig = key.sign(cp.signing_input().as_bytes());
    append_line(checkpoints_path, &serde_json::to_string(&cp)?)?;
    Ok(cp)
}

/// Verify the ledger against the signed checkpoints. Every problem found is
/// reported with what it means and what to do.
pub fn verify_checkpoints(
    ledger: &Path,
    key: &AuditHmacKey,
    checkpoints_path: &Path,
) -> Result<CheckpointReport, AuditError> {
    let cps = read_checkpoints(checkpoints_path)?;
    let mut report = CheckpointReport {
        checkpoints: cps.len(),
        ..Default::default()
    };
    if cps.is_empty() {
        return Ok(report);
    }
    // Signature chain: every checkpoint is genuine and links to its predecessor.
    let mut prev_sig = String::new();
    let mut chain_ok = true;
    for cp in &cps {
        if !key.verify(cp.signing_input().as_bytes(), &cp.sig) {
            report.problems.push(format!(
                "checkpoint #{} has an invalid signature (the checkpoint file {} was edited or \
                 signed with a different audit key)",
                cp.seq,
                checkpoints_path.display()
            ));
            chain_ok = false;
        } else if cp.prev_sig != prev_sig {
            report.problems.push(format!(
                "checkpoint #{} does not link to the checkpoint before it (a checkpoint was \
                 removed or reordered in {})",
                cp.seq,
                checkpoints_path.display()
            ));
            chain_ok = false;
        }
        prev_sig = cp.sig.clone();
    }
    if !chain_ok {
        return Ok(report);
    }
    // Only checkpoints of the current epoch constrain the current ledger.
    let start = cps.iter().rposition(|c| c.epoch_start).unwrap_or(0);
    let lines = ledger_lines(ledger)?;
    for cp in &cps[start..] {
        if lines.len() < cp.entries {
            report.problems.push(format!(
                "the ledger {} has {} entries but checkpoint #{} (signed {}) covered {}: entries \
                 were removed. If this was a deliberate `ta audit ledger gc`, run \
                 `ta audit ledger checkpoint --epoch` to record it; otherwise treat the ledger \
                 as tampered with",
                ledger.display(),
                lines.len(),
                cp.seq,
                cp.recorded_at.format("%Y-%m-%d %H:%M:%SZ"),
                cp.entries
            ));
            continue;
        }
        let actual = cp
            .entries
            .checked_sub(1)
            .map(|i| hasher::hash_str(&lines[i]));
        if actual != cp.head_hash {
            report.problems.push(format!(
                "entry {} of the ledger {} no longer matches checkpoint #{}: the history was \
                 rewritten after it was signed. Treat the ledger as tampered with",
                cp.entries,
                ledger.display(),
                cp.seq
            ));
            continue;
        }
        report.verified += 1;
    }
    Ok(report)
}

/// Where and how the ledger for one project checkpoints itself.
pub struct CheckpointSink {
    key: AuditHmacKey,
    path: PathBuf,
}

impl CheckpointSink {
    /// Resolve the sink for a project, or `None` when the project has no
    /// audit key (the unsigned default) or its state directory cannot be
    /// used. Never creates a key or a state directory.
    pub fn for_project(project_root: &Path) -> Option<Self> {
        let locator = StateLocator::from_env();
        let ps = ProjectState::locate(project_root, &locator)
            .ok()
            .flatten()?;
        let opts = CustodyOpts::from_env(&locator);
        let key = match AuditHmacKey::load_from_state(&ps, opts) {
            Ok(Some(k)) => k,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(
                    project = %project_root.display(),
                    error = %e,
                    "audit checkpoints disabled: the audit key could not be loaded"
                );
                return None;
            }
        };
        Some(Self {
            key,
            path: ps.checkpoints_path(),
        })
    }

    pub fn with_key(key: AuditHmacKey, path: PathBuf) -> Self {
        Self { key, path }
    }

    pub fn checkpoints_path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, ledger: &Path, epoch_start: bool) -> Result<Checkpoint, AuditError> {
        record_checkpoint(ledger, &self.key, &self.path, epoch_start)
    }

    pub fn verify(&self, ledger: &Path) -> Result<CheckpointReport, AuditError> {
        verify_checkpoints(ledger, &self.key, &self.path)
    }
}

/// The control secret whose absence disables checkpoints.
pub const CHECKPOINT_KEY: ControlSecret = ControlSecret::AuditKey;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, AuditHmacKey) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("goal-audit.jsonl");
        let cps = dir.path().join("state").join("audit-checkpoints.jsonl");
        let key_path = dir.path().join("state").join("audit-key");
        let key = AuditHmacKey::load_or_generate(&key_path).unwrap();
        (dir, ledger, cps, key)
    }

    fn write_ledger(path: &Path, lines: &[&str]) {
        fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn untouched_ledger_verifies_and_appends_after_a_checkpoint_are_fine() {
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a", "b", "c"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["a", "b", "c", "d"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["a", "b", "c", "d", "e"]);
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(r.ok(), "{:?}", r.problems);
        assert_eq!((r.checkpoints, r.verified), (2, 2));
    }

    #[test]
    fn truncation_is_detected() {
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a", "b", "c"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["a", "b"]);
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(!r.ok());
        assert!(
            r.problems[0].contains("entries were removed"),
            "{:?}",
            r.problems
        );
    }

    #[test]
    fn rewrite_in_place_is_detected_even_with_the_same_length() {
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a", "b", "c"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["a", "b", "EVIL"]);
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(r.problems[0].contains("rewritten"), "{:?}", r.problems);
    }

    #[test]
    fn forged_or_removed_checkpoints_are_detected() {
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["a", "b"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        // Remove the first checkpoint: the second no longer links.
        let text = fs::read_to_string(&cps).unwrap();
        let second = text.lines().nth(1).unwrap().to_string();
        fs::write(&cps, format!("{second}\n")).unwrap();
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(r.problems[0].contains("does not link"), "{:?}", r.problems);
        // A checkpoint signed with a different key is rejected.
        let other = AuditHmacKey::load_or_generate(_d.path().join("other-key")).unwrap();
        fs::remove_file(&cps).unwrap();
        record_checkpoint(&ledger, &other, &cps, false).unwrap();
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(
            r.problems[0].contains("invalid signature"),
            "{:?}",
            r.problems
        );
    }

    #[test]
    fn an_epoch_start_checkpoint_accepts_a_legitimate_rewrite() {
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a", "b", "c"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        write_ledger(&ledger, &["b2", "c2"]); // gc re-anchored the chain
        assert!(!verify_checkpoints(&ledger, &key, &cps).unwrap().ok());
        record_checkpoint(&ledger, &key, &cps, true).unwrap();
        let r = verify_checkpoints(&ledger, &key, &cps).unwrap();
        assert!(r.ok(), "{:?}", r.problems);
    }

    #[test]
    fn crlf_ledger_lines_hash_the_same() {
        let (_d, ledger, cps, key) = fixture();
        fs::write(&ledger, "a\r\nb\r\n").unwrap();
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        fs::write(&ledger, "a\nb\n").unwrap();
        assert!(verify_checkpoints(&ledger, &key, &cps).unwrap().ok());
    }

    #[test]
    fn ledger_checkpoints_itself_into_the_state_dir_when_the_project_has_a_key() {
        use crate::ledger::{AuditEntry, GoalAuditLedger};
        let project = tempfile::tempdir().unwrap();
        AuditHmacKey::load_or_generate_for_project(project.path()).unwrap();
        // The key is NOT in the project tree.
        assert!(!project.path().join(".ta/audit.key").exists());
        let path = GoalAuditLedger::path_for(project.path());
        let mut ledger = GoalAuditLedger::open(&path).unwrap();
        for i in 0..(CHECKPOINT_EVERY * 2 + 3) {
            let mut e = AuditEntry::abandoned(uuid::Uuid::new_v4(), &format!("g{i}"), "a", None);
            ledger.append(&mut e).unwrap();
        }
        let sink = CheckpointSink::for_project(project.path()).expect("key present");
        let cps = read_checkpoints(sink.checkpoints_path()).unwrap();
        assert_eq!(
            cps.len(),
            2,
            "one checkpoint per {CHECKPOINT_EVERY} entries"
        );
        assert!(sink.checkpoints_path().starts_with(std::env::temp_dir()));
        assert!(sink.verify(&path).unwrap().ok());
        // Truncating the ledger (an attacker dropping the tail) is caught.
        let text = fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text.lines().take(5).collect();
        fs::write(&path, kept.join("\n") + "\n").unwrap();
        assert!(!sink.verify(&path).unwrap().ok());
    }

    #[test]
    fn project_without_an_audit_key_gets_no_checkpoints() {
        let project = tempfile::tempdir().unwrap();
        assert!(CheckpointSink::for_project(project.path()).is_none());
        assert!(!project.path().join(".ta").exists());
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, ledger, cps, key) = fixture();
        write_ledger(&ledger, &["a"]);
        record_checkpoint(&ledger, &key, &cps, false).unwrap();
        assert_eq!(
            fs::metadata(&cps).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
