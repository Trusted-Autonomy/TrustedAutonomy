//! Detached `ta draft apply` (v0.17.12.25 item 14a).
//!
//! The pre-submit verification of an apply can be a full workspace test run. A caller
//! with a command limit (an agent tool, a CI step, an SSH session) kills the apply
//! half way: the files are already copied, the lock still names a dead process and
//! the draft stays approved. `ta draft apply --detach` starts the apply as its own
//! background process instead, so it outlives the caller, and leaves two things a
//! person can read afterwards:
//!
//! * a **run record** `.ta/apply-runs/<run-id>.json` (state, pid, phase, exit code,
//!   error) next to the **log** `.ta/apply-runs/<run-id>.log` (everything the apply
//!   printed);
//! * the **phase** (loading, copying, verifying, committing, ...) in `.ta/apply.lock`
//!   while it runs, so `ta draft apply --status` shows what it is doing and for how
//!   long.
//!
//! The detached process is the ordinary `ta draft apply` with the same arguments
//! minus `--detach`; the only difference is the environment variable
//! [`RECORD_ENV`], which makes it keep its run record up to date.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Environment variable naming the run record the detached process keeps current.
pub const RECORD_ENV: &str = "TA_APPLY_RUN_RECORD";

/// How many run records (and logs) are kept; older ones are removed at launch.
const KEEP_RUNS: usize = 10;

/// A `starting` record whose process never reported in is called lost after this long.
const START_GRACE_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Spawned; the process has not reported in yet.
    Starting,
    Running,
    Succeeded,
    Failed,
}

/// What `ta draft apply --detach` records about one background apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    /// The draft as the user named it (an id, a title or a phase).
    pub draft: String,
    /// The command line the background process runs.
    pub command: Vec<String>,
    pub log: PathBuf,
    pub started_at: DateTime<Utc>,
    pub state: RunState,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub phase_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Directory holding run records and logs.
pub fn runs_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".ta").join("apply-runs")
}

fn record_path(workspace_root: &Path, run_id: &str) -> PathBuf {
    runs_dir(workspace_root).join(format!("{run_id}.json"))
}

fn write_record(path: &Path, rec: &RunRecord) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(rec)?;
    std::fs::write(&tmp, body).map_err(|e| {
        anyhow::anyhow!(
            "could not write the apply run record {}: {e}. Check that {} is writable.",
            tmp.display(),
            path.parent().unwrap_or(Path::new(".")).display()
        )
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        anyhow::anyhow!(
            "could not move the apply run record into place at {}: {e}",
            path.display()
        )
    })
}

fn read_record(path: &Path) -> Option<RunRecord> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The newest run record, if any.
pub fn latest_record(workspace_root: &Path) -> Option<RunRecord> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(runs_dir(workspace_root))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    // Run ids start with a UTC timestamp, so name order is age order.
    files.sort();
    files.iter().rev().find_map(|p| read_record(p))
}

fn prune_old_runs(workspace_root: &Path) {
    let Ok(rd) = std::fs::read_dir(runs_dir(workspace_root)) else {
        return;
    };
    let mut stems: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension().is_some_and(|x| x == "json"))
                .then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .flatten()
        })
        .collect();
    stems.sort();
    if stems.len() <= KEEP_RUNS {
        return;
    }
    for stem in &stems[..stems.len() - KEEP_RUNS] {
        for ext in ["json", "log"] {
            let _ = std::fs::remove_file(runs_dir(workspace_root).join(format!("{stem}.{ext}")));
        }
    }
}

/// The command line for the background process: this invocation without `--detach`.
pub fn child_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    args.into_iter().filter(|a| a != "--detach").collect()
}

/// Everything `launch` needs, so tests can start something other than `ta`.
pub struct LaunchSpec<'a> {
    pub workspace_root: &'a Path,
    pub draft: &'a str,
    pub program: &'a Path,
    pub args: Vec<std::ffi::OsString>,
}

/// Start the detached apply and return its record. The process is its own session
/// leader (Unix) or detached process group (Windows) with no terminal, so closing the
/// caller's terminal or killing the caller does not touch it.
pub fn launch(spec: LaunchSpec<'_>) -> anyhow::Result<RunRecord> {
    use std::process::{Command, Stdio};

    let dir = runs_dir(spec.workspace_root);
    std::fs::create_dir_all(&dir).map_err(|e| {
        anyhow::anyhow!(
            "could not create {} for the detached apply: {e}. Check the directory is writable.",
            dir.display()
        )
    })?;
    prune_old_runs(spec.workspace_root);

    let now = Utc::now();
    let draft_tag: String = spec
        .draft
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let run_id = format!("{}-{}", now.format("%Y%m%dT%H%M%S%3fZ"), draft_tag);
    let log = dir.join(format!("{run_id}.log"));
    let rec_path = record_path(spec.workspace_root, &run_id);

    let mut record = RunRecord {
        run_id: run_id.clone(),
        draft: spec.draft.to_string(),
        command: std::iter::once(spec.program.as_os_str())
            .chain(spec.args.iter().map(|a| a.as_os_str()))
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        log: log.clone(),
        started_at: now,
        state: RunState::Starting,
        pid: None,
        phase: None,
        phase_started_at: None,
        finished_at: None,
        exit_code: None,
        error: None,
    };
    // The record exists before the process does: a process that dies on start-up still
    // leaves a `starting` record and a log to read.
    write_record(&rec_path, &record)?;

    let log_file = std::fs::File::create(&log).map_err(|e| {
        anyhow::anyhow!(
            "could not create the apply log {}: {e}. Check the directory is writable.",
            log.display()
        )
    })?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| anyhow::anyhow!("could not duplicate the handle of {}: {e}", log.display()))?;

    let mut cmd = Command::new(spec.program);
    cmd.args(&spec.args)
        .current_dir(spec.workspace_root)
        .env(RECORD_ENV, &rec_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no memory of ours; it only
        // detaches the child from the caller's session and terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let child = cmd.spawn().map_err(|e| {
        let mut failed = record.clone();
        failed.state = RunState::Failed;
        failed.finished_at = Some(Utc::now());
        failed.error = Some(format!("could not start the background apply: {e}"));
        let _ = write_record(&rec_path, &failed);
        anyhow::anyhow!(
            "could not start the background apply `{}`: {e}. Run `ta draft apply {}` in the foreground instead.",
            spec.program.display(),
            spec.draft
        )
    })?;
    record.pid = Some(child.id());
    // The child reports its own pid once it is running; the spawn pid is only read
    // while the record still says `starting`.
    update_record(&rec_path, |r| {
        if r.state == RunState::Starting {
            r.pid = Some(child.id());
        }
    });
    // Not waited for: the child outlives this process. Dropping the handle neither
    // kills nor reaps it (the OS reparents it).
    drop(child);
    Ok(record)
}

fn update_record(path: &Path, f: impl FnOnce(&mut RunRecord)) {
    let Some(mut rec) = read_record(path) else {
        tracing::warn!(
            record = %path.display(),
            "apply run record is missing or unreadable; `ta draft apply --status` will not show this run's progress"
        );
        return;
    };
    f(&mut rec);
    if let Err(e) = write_record(path, &rec) {
        tracing::warn!(record = %path.display(), error = %e, "could not update the apply run record");
    }
}

/// The detached process's side: keeps its run record current. `None` when this
/// process was not started with `--detach`.
pub struct RunHandle {
    path: PathBuf,
}

impl RunHandle {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os(RECORD_ENV)?;
        Some(Self {
            path: PathBuf::from(path),
        })
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn mark_running(&self) {
        update_record(&self.path, |r| {
            r.state = RunState::Running;
            r.pid = Some(std::process::id());
        });
    }

    pub fn set_phase(&self, phase: &str) {
        update_record(&self.path, |r| {
            r.phase = Some(phase.to_string());
            r.phase_started_at = Some(Utc::now());
        });
    }

    pub fn finish(&self, result: &anyhow::Result<()>) {
        update_record(&self.path, |r| {
            r.finished_at = Some(Utc::now());
            match result {
                Ok(()) => {
                    r.state = RunState::Succeeded;
                    r.exit_code = Some(0);
                }
                Err(e) => {
                    r.state = RunState::Failed;
                    r.exit_code = Some(1);
                    r.error = Some(format!("{e:#}"));
                }
            }
        });
    }
}

/// `1h 02m 03s`, `4m 05s` or `6s`.
pub fn human_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 3600 {
        format!(
            "{}h {:02}m {:02}s",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Lines `ta draft apply --status` prints for the newest detached run. `alive` is
/// whether the recorded pid is a live process.
pub fn status_lines(rec: &RunRecord, now: DateTime<Utc>, alive: bool) -> Vec<String> {
    let mut out = Vec::new();
    let since = human_elapsed((now - rec.started_at).num_seconds());
    let lost_start = rec.state == RunState::Starting
        && (now - rec.started_at).num_seconds() > START_GRACE_SECS
        && !alive;
    // A `starting` record younger than the grace period is live; a `running` one only
    // while its process is.
    let live = rec.state == RunState::Starting || alive;
    match rec.state {
        _ if lost_start => {
            out.push(format!(
                "Detached apply of {} (run {}) never started: the process is gone and left no progress.",
                rec.draft, rec.run_id
            ));
            out.push(format!(
                "  Read the log for the reason: {}",
                rec.log.display()
            ));
            out.push(format!(
                "  Then run it again: ta draft apply {} --detach (or without --detach)",
                rec.draft
            ));
        }
        RunState::Starting | RunState::Running if live => {
            out.push(format!(
                "Detached apply of {} (run {}): {} for {} (PID {}).",
                rec.draft,
                rec.run_id,
                if rec.state == RunState::Starting {
                    "starting"
                } else {
                    "running"
                },
                since,
                rec.pid.map_or("unknown".to_string(), |p| p.to_string())
            ));
            if let Some(phase) = &rec.phase {
                let in_phase = rec
                    .phase_started_at
                    .map(|t| human_elapsed((now - t).num_seconds()))
                    .unwrap_or_else(|| "unknown".to_string());
                out.push(format!("  Phase: {phase} (for {in_phase})."));
            }
            out.push(format!("  Log: {}", rec.log.display()));
            out.push(format!("  Follow it with: tail -f {}", rec.log.display()));
        }
        RunState::Starting | RunState::Running => {
            out.push(format!(
                "Detached apply of {} (run {}) was interrupted: its process (PID {}) is gone but it never reported a result.",
                rec.draft,
                rec.run_id,
                rec.pid.map_or("unknown".to_string(), |p| p.to_string())
            ));
            if let Some(phase) = &rec.phase {
                out.push(format!("  It was in phase: {phase}."));
            }
            out.push(format!("  Log: {}", rec.log.display()));
            out.push(format!(
                "  Some files may already be copied; run `ta draft apply {}` again to recover (a stale lock is reported with the exact next step).",
                rec.draft
            ));
        }
        RunState::Succeeded => {
            let took = rec
                .finished_at
                .map(|f| human_elapsed((f - rec.started_at).num_seconds()))
                .unwrap_or_else(|| since.clone());
            out.push(format!(
                "Last detached apply: {} (run {}) succeeded in {took}.",
                rec.draft, rec.run_id
            ));
            out.push(format!("  Log: {}", rec.log.display()));
        }
        RunState::Failed => {
            out.push(format!(
                "Last detached apply: {} (run {}) FAILED{}.",
                rec.draft,
                rec.run_id,
                rec.phase
                    .as_ref()
                    .map(|p| format!(" during {p}"))
                    .unwrap_or_default()
            ));
            if let Some(err) = &rec.error {
                for line in err.lines().take(6) {
                    out.push(format!("  {line}"));
                }
            }
            out.push(format!("  Log: {}", rec.log.display()));
            out.push(format!(
                "  Fix the cause, then run: ta draft apply {}",
                rec.draft
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(state: RunState, age_secs: i64) -> RunRecord {
        RunRecord {
            run_id: "20260101T000000000Z-abcd1234".into(),
            draft: "abcd1234".into(),
            command: vec!["ta".into()],
            log: PathBuf::from("/p/.ta/apply-runs/x.log"),
            started_at: Utc::now() - chrono::Duration::seconds(age_secs),
            state,
            pid: Some(4242),
            phase: None,
            phase_started_at: None,
            finished_at: None,
            exit_code: None,
            error: None,
        }
    }

    #[test]
    fn child_args_drop_only_the_detach_flag() {
        let args = ["draft", "apply", "abc", "--detach", "--no-review"]
            .iter()
            .map(std::ffi::OsString::from);
        let got: Vec<String> = child_args(args)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["draft", "apply", "abc", "--no-review"]);
    }

    #[test]
    fn human_elapsed_formats() {
        assert_eq!(human_elapsed(6), "6s");
        assert_eq!(human_elapsed(245), "4m 05s");
        assert_eq!(human_elapsed(3723), "1h 02m 03s");
        assert_eq!(human_elapsed(-5), "0s");
    }

    #[test]
    fn running_status_shows_phase_elapsed_and_log() {
        let mut r = record(RunState::Running, 125);
        r.phase = Some("verifying".into());
        r.phase_started_at = Some(Utc::now() - chrono::Duration::seconds(90));
        let text = status_lines(&r, Utc::now(), true).join("\n");
        assert!(text.contains("running for 2m 05s"), "{text}");
        assert!(text.contains("PID 4242"), "{text}");
        assert!(text.contains("Phase: verifying (for 1m 30s)"), "{text}");
        assert!(text.contains("x.log"), "{text}");
    }

    #[test]
    fn dead_running_process_is_reported_interrupted_with_the_phase() {
        let mut r = record(RunState::Running, 600);
        r.phase = Some("verifying".into());
        let text = status_lines(&r, Utc::now(), false).join("\n");
        assert!(text.contains("was interrupted"), "{text}");
        assert!(text.contains("verifying"), "{text}");
        assert!(text.contains("ta draft apply abcd1234"), "{text}");
    }

    #[test]
    fn starting_record_is_lost_only_after_the_grace_period() {
        let fresh = record(RunState::Starting, 5);
        assert!(status_lines(&fresh, Utc::now(), false)[0].contains("starting"));
        let old = record(RunState::Starting, 300);
        assert!(status_lines(&old, Utc::now(), false)[0].contains("never started"));
    }

    #[test]
    fn failed_status_names_phase_error_and_next_step() {
        let mut r = record(RunState::Failed, 100);
        r.phase = Some("verifying".into());
        r.error = Some("Pre-submit verification failed\nsecond line".into());
        let text = status_lines(&r, Utc::now(), false).join("\n");
        assert!(text.contains("FAILED during verifying"), "{text}");
        assert!(text.contains("Pre-submit verification failed"), "{text}");
        assert!(
            text.contains("Fix the cause, then run: ta draft apply abcd1234"),
            "{text}"
        );
    }

    #[test]
    fn handle_records_running_phase_and_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        write_record(&path, &record(RunState::Starting, 1)).unwrap();
        let h = RunHandle::at(path.clone());
        h.mark_running();
        h.set_phase("copying changes");
        let r = read_record(&path).unwrap();
        assert_eq!(r.state, RunState::Running);
        assert_eq!(r.pid, Some(std::process::id()));
        assert_eq!(r.phase.as_deref(), Some("copying changes"));
        h.finish(&Err(anyhow::anyhow!("boom")));
        let r = read_record(&path).unwrap();
        assert_eq!(r.state, RunState::Failed);
        assert_eq!(r.exit_code, Some(1));
        assert_eq!(r.error.as_deref(), Some("boom"));
        assert!(r.finished_at.is_some());
        h.finish(&Ok(()));
        let r = read_record(&path).unwrap();
        assert_eq!(r.state, RunState::Succeeded);
        assert_eq!(r.exit_code, Some(0));
    }

    #[test]
    fn latest_record_is_the_newest_and_old_runs_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(runs_dir(root)).unwrap();
        for i in 0..(KEEP_RUNS + 3) {
            let mut r = record(RunState::Succeeded, 1);
            r.run_id = format!("2026010{}T0000000{:02}Z-x", 1, i);
            write_record(&record_path(root, &r.run_id), &r).unwrap();
            std::fs::write(runs_dir(root).join(format!("{}.log", r.run_id)), "log").unwrap();
        }
        let newest = latest_record(root).unwrap();
        assert!(
            newest.run_id.contains(&format!("{:02}", KEEP_RUNS + 2)),
            "{}",
            newest.run_id
        );
        prune_old_runs(root);
        let left = std::fs::read_dir(runs_dir(root)).unwrap().count();
        assert_eq!(left, KEEP_RUNS * 2, "json + log per kept run");
    }

    /// The point of `--detach`: the background process keeps running after `launch`
    /// returns, writes to the log, and `launch` never waits for it.
    #[cfg(unix)]
    #[test]
    fn launched_process_outlives_launch_and_writes_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let start = std::time::Instant::now();
        let rec = launch(LaunchSpec {
            workspace_root: root,
            draft: "abcd1234",
            program: Path::new("/bin/sh"),
            args: vec![
                "-c".into(),
                "sleep 1; echo background-done $TA_APPLY_RUN_RECORD".into(),
            ],
        })
        .unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(900),
            "launch must not wait"
        );
        let record_file = record_path(root, &rec.run_id);
        assert!(record_file.is_file());
        assert!(rec.pid.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let log = std::fs::read_to_string(&rec.log).unwrap_or_default();
            if log.contains("background-done") {
                assert!(log.contains(record_file.to_str().unwrap()), "{log}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "log never written: {log:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_cannot_start_leaves_a_failed_record() {
        let dir = tempfile::tempdir().unwrap();
        let err = launch(LaunchSpec {
            workspace_root: dir.path(),
            draft: "abcd1234",
            program: Path::new("/no/such/ta-binary"),
            args: vec![],
        })
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("could not start the background apply"),
            "{err}"
        );
        assert!(err.contains("in the foreground"), "{err}");
        let r = latest_record(dir.path()).unwrap();
        assert_eq!(r.state, RunState::Failed);
        assert!(r.error.unwrap().contains("could not start"));
    }
}
