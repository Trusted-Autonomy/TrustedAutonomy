//! Behavioural tests of the file-backed intake store: concurrency, torn
//! writes, crash recovery, caps, dedupe and project isolation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use ta_task_intake::*;
use uuid::Uuid;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 17, 3, 0, 0).unwrap()
}

fn task(title: &str) -> RequestBody {
    RequestBody::CreateTask {
        title: title.to_string(),
        verb: "build".to_string(),
        description: None,
    }
}

fn sub(project: &str, goal: Uuid, role: &str, body: RequestBody) -> Submission {
    Submission {
        project_id: project.to_string(),
        goal_id: goal,
        role: role.to_string(),
        body,
        why_now: "needed for the release".to_string(),
        blocks_goal: false,
    }
}

fn big_caps() -> IntakeSettings {
    IntakeSettings {
        caps: IntakeCaps {
            per_goal: 10_000,
            per_role_per_hour: 10_000,
            per_project_outstanding: 10_000,
        },
        ..IntakeSettings::default()
    }
}

fn store(dir: &Path, settings: IntakeSettings) -> FileIntakeStore {
    FileIntakeStore::open(dir, settings, None)
}

fn drain_req(now: DateTime<Utc>, max: usize, lease_min: u64) -> DrainRequest {
    DrainRequest {
        max_records: max,
        lease: Duration::from_secs(lease_min * 60),
        actor: "test-drain".into(),
        now,
    }
}

fn raw_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn queued_id(o: EnqueueOutcome) -> String {
    match o {
        EnqueueOutcome::Queued { request_id } => request_id,
        other => panic!("expected Queued, got {other:?}"),
    }
}

// ── concurrent append ────────────────────────────────────────────────────

#[test]
fn concurrent_threads_never_interleave_and_every_line_is_valid() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(store(dir.path(), big_caps()));
    let mut handles = Vec::new();
    for t in 0..8 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                let goal = Uuid::new_v4();
                s.enqueue(
                    sub(
                        "p1",
                        goal,
                        "eng",
                        task(&format!("thread {t} item {i} {}", "x".repeat(400))),
                    ),
                    t0(),
                )
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let q = store(dir.path(), big_caps()).dir().join("queue.jsonl");
    let lines = raw_lines(&q);
    assert_eq!(lines.len(), 200);
    let mut ids = std::collections::HashSet::new();
    for l in &lines {
        let r: IntakeRecord = serde_json::from_str(l).expect("every queue line is a whole record");
        assert!(ids.insert(r.request_id));
    }
    let ledger = raw_lines(&s.dir().join("ledger.jsonl"));
    assert_eq!(ledger.len(), 200);
    for l in &ledger {
        serde_json::from_str::<LedgerEvent>(l).expect("every ledger line is a whole event");
    }
    assert!(s
        .list(&ListFilter::default(), t0())
        .unwrap()
        .warnings
        .is_empty());
}

/// Child half of the multi-process test: runs only when spawned by it.
#[test]
fn child_appender() {
    let Ok(dir) = std::env::var("TA_INTAKE_TEST_CHILD_DIR") else {
        return;
    };
    let tag = std::env::var("TA_INTAKE_TEST_CHILD_TAG").unwrap();
    let s = store(Path::new(&dir), big_caps());
    for i in 0..30 {
        s.enqueue(
            sub(
                "p1",
                Uuid::new_v4(),
                "eng",
                task(&format!("proc {tag} item {i} {}", "y".repeat(500))),
            ),
            t0(),
        )
        .unwrap();
    }
}

#[test]
fn concurrent_processes_never_interleave() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..4)
        .map(|n| {
            std::process::Command::new(&exe)
                .args(["--exact", "child_appender", "--nocapture"])
                .env("TA_INTAKE_TEST_CHILD_DIR", dir.path())
                .env("TA_INTAKE_TEST_CHILD_TAG", n.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let s = store(dir.path(), big_caps());
    let lines = raw_lines(&s.dir().join("queue.jsonl"));
    assert_eq!(lines.len(), 120, "4 processes x 30 appends");
    for l in &lines {
        serde_json::from_str::<IntakeRecord>(l).expect("whole record");
    }
    for l in raw_lines(&s.dir().join("ledger.jsonl")) {
        serde_json::from_str::<LedgerEvent>(&l).expect("whole event");
    }
}

// ── torn trailing line ───────────────────────────────────────────────────

#[test]
fn torn_trailing_queue_line_is_skipped_and_next_append_is_not_corrupted() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let first = queued_id(s.enqueue(sub("p1", g, "eng", task("first")), t0()).unwrap());

    // A writer killed mid-line: a fragment with no newline.
    let q = s.dir().join("queue.jsonl");
    let mut bytes = std::fs::read(&q).unwrap();
    bytes
        .extend_from_slice(br#"{"schema_version":1,"request_id":"req-torn","project_id":"p1","go"#);
    std::fs::write(&q, bytes).unwrap();

    let report = s.list(&ListFilter::default(), t0()).unwrap();
    assert_eq!(report.requests.len(), 1, "torn fragment is not a request");
    assert_eq!(report.requests[0].request_id, first);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("torn trailing line")),
        "the reader says data was skipped: {:?}",
        report.warnings
    );
    let second = queued_id(
        s.enqueue(
            sub("p1", g, "eng", task("second")),
            t0() + chrono::Duration::seconds(5),
        )
        .unwrap(),
    );
    let after = s.list(&ListFilter::default(), t0()).unwrap();
    let ids: Vec<_> = after
        .requests
        .iter()
        .map(|r| r.request_id.clone())
        .collect();
    assert_eq!(
        ids,
        vec![first, second],
        "new record is intact, not glued to the fragment"
    );
}

#[test]
fn torn_trailing_ledger_line_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    let l = s.dir().join("ledger.jsonl");
    let mut bytes = std::fs::read(&l).unwrap();
    bytes.extend_from_slice(br#"{"event":"transition","request_id":""#);
    std::fs::write(&l, bytes).unwrap();
    let r = s.list(&ListFilter::default(), t0()).unwrap();
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.requests[0].state, RequestState::Queued);
    assert_eq!(r.requests[0].request_id, id);
    assert!(!r.warnings.is_empty());
}

#[test]
fn oversized_body_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let err = s
        .enqueue(
            sub(
                "p1",
                Uuid::new_v4(),
                "eng",
                task(&"z".repeat(MAX_LINE_BYTES + 10)),
            ),
            t0(),
        )
        .unwrap_err();
    assert!(matches!(err, IntakeError::TooLarge { .. }), "{err}");
    assert!(raw_lines(&s.dir().join("queue.jsonl")).is_empty());
    assert!(raw_lines(&s.dir().join("ledger.jsonl")).is_empty());
}

#[cfg(unix)]
#[test]
fn directory_is_0700_and_files_are_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    s.enqueue(sub("p1", Uuid::new_v4(), "eng", task("a")), t0())
        .unwrap();
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(s.dir().to_path_buf()), 0o700);
    assert_eq!(mode(s.dir().join("queue.jsonl")), 0o600);
    assert_eq!(mode(s.dir().join("ledger.jsonl")), 0o600);
    // A drained batch and the fresh queue after it keep the mode.
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    s.enqueue(sub("p1", Uuid::new_v4(), "eng", task("b")), t0())
        .unwrap();
    assert_eq!(mode(s.dir().join("queue.jsonl")), 0o600);
    for e in std::fs::read_dir(s.dir()).unwrap() {
        let p = e.unwrap().path();
        if p.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("batch-")
        {
            assert_eq!(mode(p), 0o600);
        }
    }
}

// ── drain, ack and kill-between-drain-and-ack ────────────────────────────

#[test]
fn drain_renames_queue_to_batch_and_claims_with_a_lease() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let a = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    let b = queued_id(s.enqueue(sub("p1", g, "eng", task("b")), t0()).unwrap());
    let batch = s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    assert!(batch.path.exists());
    assert!(
        !s.dir().join("queue.jsonl").exists(),
        "queue was renamed away"
    );
    let ids: Vec<_> = batch.records.iter().map(|r| r.request_id.clone()).collect();
    assert_eq!(ids, vec![a.clone(), b]);
    assert_eq!(batch.lease_expires_at, t0() + chrono::Duration::minutes(30));
    let v = &s.list(&ListFilter::default(), t0()).unwrap().requests[0];
    assert_eq!(v.state, RequestState::Claimed);
    assert_eq!(v.batch_id.as_deref(), Some(batch.batch_id.as_str()));
    assert_eq!(
        s.drain(&drain_req(t0(), 25, 30))
            .unwrap()
            .map(|b| b.batch_id),
        None
    );
}

#[test]
fn drain_respects_max_records_and_leaves_the_rest_queued() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    for i in 0..5 {
        s.enqueue(
            sub("p1", Uuid::new_v4(), "eng", task(&format!("t{i}"))),
            t0() + chrono::Duration::seconds(i),
        )
        .unwrap();
    }
    let b = s
        .drain(&drain_req(t0() + chrono::Duration::minutes(1), 2, 30))
        .unwrap()
        .unwrap();
    assert_eq!(b.records.len(), 2);
    let all = s.list(&ListFilter::default(), t0()).unwrap().requests;
    let claimed = all
        .iter()
        .filter(|r| r.state == RequestState::Claimed)
        .count();
    let queued = all
        .iter()
        .filter(|r| r.state == RequestState::Queued)
        .count();
    assert_eq!((claimed, queued), (2, 3));
    assert_eq!(raw_lines(&s.dir().join("queue.jsonl")).len(), 3);
    let b2 = s
        .drain(&drain_req(t0() + chrono::Duration::minutes(2), 25, 30))
        .unwrap()
        .unwrap();
    assert_eq!(b2.records.len(), 3, "the remainder drains next");
}

#[test]
fn kill_between_drain_and_ack_returns_to_queue_when_the_lease_lapses() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    // ... the drain consumer is killed here: nothing acknowledges. ...

    // Lease still held: recovery must not touch it.
    let early = s
        .recover(t0() + chrono::Duration::minutes(29), "recovery")
        .unwrap();
    assert_eq!(early, RecoveryReport::default());
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Claimed)
    );
    let v = &s
        .list(&ListFilter::default(), t0() + chrono::Duration::minutes(31))
        .unwrap()
        .requests[0];
    assert!(
        v.lease_expired,
        "readers see the lapsed lease before recovery runs"
    );

    // Lease lapsed: deterministic return to queued, record copied back.
    let late = t0() + chrono::Duration::minutes(31);
    let rep = s.recover(late, "recovery").unwrap();
    assert_eq!(rep.requeued, vec![id.clone()]);
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Queued)
    );
    assert_eq!(raw_lines(&s.dir().join("queue.jsonl")).len(), 1);
    // Idempotent: a second pass changes nothing.
    assert_eq!(
        s.recover(late, "recovery").unwrap(),
        RecoveryReport::default()
    );

    // It drains again, and a late ack from the dead consumer is refused.
    let b = s.drain(&drain_req(late, 25, 30)).unwrap().unwrap();
    assert_eq!(b.records[0].request_id, id);
    let v = &s.list(&ListFilter::default(), late).unwrap().requests[0];
    assert_eq!(v.claim_count, 2);
}

#[test]
fn a_request_that_never_gets_acknowledged_expires_after_its_claim_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let settings = IntakeSettings {
        max_claim_attempts: 2,
        ..big_caps()
    };
    let s = store(dir.path(), settings);
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    let mut now = t0();
    for _ in 0..2 {
        s.drain(&drain_req(now, 25, 10))
            .unwrap()
            .expect("claimable");
        now += chrono::Duration::minutes(11);
        s.recover(now, "recovery").unwrap();
    }
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Expired)
    );
    assert!(s.drain(&drain_req(now, 25, 10)).unwrap().is_none());
    let v = &s.list(&ListFilter::default(), now).unwrap().requests[0];
    assert!(
        v.reason.as_deref().unwrap().contains("2 attempt"),
        "{:?}",
        v.reason
    );
}

#[test]
fn ack_after_the_lease_was_recovered_is_an_invalid_transition() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let id = queued_id(
        s.enqueue(sub("p1", Uuid::new_v4(), "eng", task("a")), t0())
            .unwrap(),
    );
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    let late = t0() + chrono::Duration::hours(1);
    s.recover(late, "recovery").unwrap();
    let err = s
        .acknowledge(&AckRequest {
            project_id: "p1".into(),
            request_ids: vec![id.clone()],
            ack: Ack::Drafted {
                draft_id: "d1".into(),
            },
            actor: "pm".into(),
            now: late,
        })
        .unwrap_err();
    assert!(
        matches!(err, IntakeError::InvalidTransition { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("queued"), "{err}");
}

#[test]
fn kill_right_after_the_rename_restores_unclaimed_requests_to_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    // Simulate a drain killed after the rename and before any claim event.
    std::fs::rename(
        s.dir().join("queue.jsonl"),
        s.dir().join("batch-killed-1.jsonl"),
    )
    .unwrap();
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Queued)
    );
    let rep = s.recover(t0(), "recovery").unwrap();
    assert_eq!(rep.restored_to_queue, vec![id.clone()]);
    let b = s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    assert_eq!(b.records[0].request_id, id, "nothing was lost");
}

#[test]
fn kill_between_queue_append_and_ledger_append_is_healed() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    // Drop the ledger: the queue record exists, its ledger event never landed.
    std::fs::remove_file(s.dir().join("ledger.jsonl")).unwrap();
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Queued)
    );
    let rep = s.recover(t0(), "recovery").unwrap();
    assert_eq!(rep.healed, vec![id.clone()]);
    assert_eq!(raw_lines(&s.dir().join("ledger.jsonl")).len(), 1);
    assert_eq!(
        s.recover(t0(), "recovery").unwrap(),
        RecoveryReport::default()
    );
}

#[test]
fn queued_requests_expire_after_the_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let settings = IntakeSettings {
        queued_ttl: Duration::from_secs(3600),
        ..big_caps()
    };
    let s = store(dir.path(), settings);
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    assert!(s
        .recover(t0() + chrono::Duration::minutes(59), "r")
        .unwrap()
        .expired
        .is_empty());
    let rep = s
        .recover(t0() + chrono::Duration::minutes(61), "r")
        .unwrap();
    assert_eq!(rep.expired, vec![id.clone()]);
    assert_eq!(
        s.status_for_requester("p1", &id, g).unwrap(),
        Some(RequestState::Expired)
    );
}

#[test]
fn full_lifecycle_acknowledgements_and_illegal_moves() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let a = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    let b = queued_id(s.enqueue(sub("p1", g, "eng", task("b")), t0()).unwrap());
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    let ack = |ids: Vec<&str>, ack: Ack| AckRequest {
        project_id: "p1".into(),
        request_ids: ids.into_iter().map(String::from).collect(),
        ack,
        actor: "pm".into(),
        now: t0(),
    };
    // applied straight from claimed is not allowed (needs a draft first)
    let err = s
        .acknowledge(&ack(vec![&a], Ack::Applied { draft_id: None }))
        .unwrap_err();
    assert!(matches!(err, IntakeError::InvalidTransition { .. }));
    // all-or-nothing: unknown id in the list leaves `a` untouched
    let err = s
        .acknowledge(&ack(
            vec![&a, "req-nope"],
            Ack::Drafted {
                draft_id: "d1".into(),
            },
        ))
        .unwrap_err();
    assert!(matches!(err, IntakeError::UnknownRequest(_)));
    assert_eq!(
        s.status_for_requester("p1", &a, g).unwrap(),
        Some(RequestState::Claimed)
    );

    s.acknowledge(&ack(
        vec![&a],
        Ack::Drafted {
            draft_id: "d1".into(),
        },
    ))
    .unwrap();
    s.acknowledge(&ack(
        vec![&a],
        Ack::Applied {
            draft_id: Some("d1".into()),
        },
    ))
    .unwrap();
    s.acknowledge(&ack(
        vec![&b],
        Ack::Rejected {
            reason: "duplicate of an existing task".into(),
        },
    ))
    .unwrap();
    let all = s.list(&ListFilter::default(), t0()).unwrap().requests;
    let va = all.iter().find(|r| r.request_id == a).unwrap();
    let vb = all.iter().find(|r| r.request_id == b).unwrap();
    assert_eq!(va.state, RequestState::Applied);
    assert_eq!(va.draft_id.as_deref(), Some("d1"));
    assert_eq!(vb.state, RequestState::Rejected);
    assert_eq!(vb.reason.as_deref(), Some("duplicate of an existing task"));
    // terminal states stay terminal
    assert!(s
        .acknowledge(&ack(vec![&a], Ack::Rejected { reason: "x".into() }))
        .is_err());
}

// ── caps and dedupe ──────────────────────────────────────────────────────

fn caps(per_goal: usize, per_role: usize, per_project: usize) -> IntakeSettings {
    IntakeSettings {
        caps: IntakeCaps {
            per_goal,
            per_role_per_hour: per_role,
            per_project_outstanding: per_project,
        },
        ..IntakeSettings::default()
    }
}

#[test]
fn defaults_are_documented_values() {
    let c = IntakeCaps::default();
    assert_eq!(
        (c.per_goal, c.per_role_per_hour, c.per_project_outstanding),
        (10, 20, 50)
    );
}

fn refusal(r: Result<EnqueueOutcome, IntakeError>) -> (CapKind, String) {
    match r.unwrap_err() {
        IntakeError::CapExceeded { cap, .. } => {
            let msg = IntakeError::CapExceeded {
                cap,
                detail: String::new(),
            }
            .to_string();
            (cap, msg)
        }
        other => panic!("expected CapExceeded, got {other}"),
    }
}

#[test]
fn per_goal_cap_refuses_loudly_and_names_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), caps(10, 1000, 1000));
    let g = Uuid::new_v4();
    for i in 0..10 {
        s.enqueue(sub("p1", g, "eng", task(&format!("t{i}"))), t0())
            .unwrap();
    }
    let err = s
        .enqueue(sub("p1", g, "eng", task("one too many")), t0())
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("queue full, retry or ask_human"), "{text}");
    assert!(text.contains("per-goal"), "{text}");
    assert!(text.contains("per_goal_max"), "{text}");
    // another goal is unaffected
    s.enqueue(sub("p1", Uuid::new_v4(), "eng", task("fine")), t0())
        .unwrap();
    // nothing was queued by the refused call
    assert_eq!(raw_lines(&s.dir().join("queue.jsonl")).len(), 11);
}

#[test]
fn per_role_cap_is_hourly_and_rolls() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), caps(1000, 3, 1000));
    for i in 0..3 {
        s.enqueue(
            sub("p1", Uuid::new_v4(), "eng", task(&format!("t{i}"))),
            t0(),
        )
        .unwrap();
    }
    let (cap, msg) = refusal(s.enqueue(sub("p1", Uuid::new_v4(), "eng", task("t3")), t0()));
    assert_eq!(cap, CapKind::PerRolePerHour);
    assert!(msg.contains("queue full, retry or ask_human"));
    // a different role is fine
    s.enqueue(sub("p1", Uuid::new_v4(), "qa", task("t3")), t0())
        .unwrap();
    // an hour later the window has rolled
    s.enqueue(
        sub("p1", Uuid::new_v4(), "eng", task("t3")),
        t0() + chrono::Duration::minutes(61),
    )
    .unwrap();
}

#[test]
fn per_project_cap_counts_outstanding_and_frees_up_on_terminal_states() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), caps(1000, 1000, 2));
    let g = Uuid::new_v4();
    let a = queued_id(s.enqueue(sub("p1", g, "r1", task("a")), t0()).unwrap());
    s.enqueue(sub("p1", g, "r2", task("b")), t0()).unwrap();
    let (cap, _) = refusal(s.enqueue(sub("p1", g, "r3", task("c")), t0()));
    assert_eq!(cap, CapKind::PerProject);
    // another project has its own budget
    s.enqueue(sub("p2", g, "r3", task("c")), t0()).unwrap();
    // resolving one frees a slot
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    s.acknowledge(&AckRequest {
        project_id: "p1".into(),
        request_ids: vec![a],
        ack: Ack::Rejected {
            reason: "no".into(),
        },
        actor: "pm".into(),
        now: t0(),
    })
    .unwrap();
    s.enqueue(sub("p1", g, "r3", task("c")), t0()).unwrap();
}

#[test]
fn duplicate_returns_the_existing_id_and_is_not_charged() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), caps(2, 1000, 1000));
    let g = Uuid::new_v4();
    let first = queued_id(
        s.enqueue(sub("p1", g, "eng", task("Add retry to the poller")), t0())
            .unwrap(),
    );
    // whitespace and case do not make a new request
    for variant in ["add retry to the poller", "  Add   RETRY to\tthe poller  "] {
        match s.enqueue(sub("p1", g, "eng", task(variant)), t0()).unwrap() {
            EnqueueOutcome::Duplicate { request_id, state } => {
                assert_eq!(request_id, first);
                assert_eq!(state, RequestState::Queued);
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
    }
    assert_eq!(raw_lines(&s.dir().join("queue.jsonl")).len(), 1);
    // the two duplicates did not use up the per-goal cap of 2
    s.enqueue(sub("p1", g, "eng", task("something else")), t0())
        .unwrap();
    let (cap, _) = refusal(s.enqueue(sub("p1", g, "eng", task("a third")), t0()));
    assert_eq!(cap, CapKind::PerGoal);
    // ... and a duplicate is still answered when the goal is at its cap
    assert_eq!(
        s.enqueue(sub("p1", g, "eng", task("add retry to the poller")), t0())
            .unwrap()
            .request_id(),
        first
    );
}

#[test]
fn dedupe_distinguishes_kind_target_and_body() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let dp = |m: &str, v: f64| RequestBody::RecordDatapoint {
        metric_id: m.into(),
        value: v,
        source: "manual".into(),
    };
    let a = queued_id(s.enqueue(sub("p1", g, "eng", dp("m1", 5.0)), t0()).unwrap());
    assert_eq!(
        s.enqueue(sub("p1", g, "eng", dp("m1", 5.0)), t0())
            .unwrap()
            .request_id(),
        a
    );
    queued_id(s.enqueue(sub("p1", g, "eng", dp("m1", 6.0)), t0()).unwrap());
    queued_id(s.enqueue(sub("p1", g, "eng", dp("m2", 5.0)), t0()).unwrap());
    // metric ids are case-sensitive identifiers
    queued_id(s.enqueue(sub("p1", g, "eng", dp("M1", 5.0)), t0()).unwrap());
    // a different verb is a different task
    let mut other = task("same title");
    queued_id(s.enqueue(sub("p1", g, "eng", other.clone()), t0()).unwrap());
    if let RequestBody::CreateTask { verb, .. } = &mut other {
        *verb = "review".into();
    }
    queued_id(s.enqueue(sub("p1", g, "eng", other), t0()).unwrap());
}

#[test]
fn a_second_goal_is_folded_in_and_can_read_the_shared_status() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let (g1, g2, g3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let id = queued_id(
        s.enqueue(sub("p1", g1, "eng", task("shared")), t0())
            .unwrap(),
    );
    assert_eq!(
        s.enqueue(sub("p1", g2, "qa", task("shared")), t0())
            .unwrap()
            .request_id(),
        id
    );
    assert_eq!(
        s.status_for_requester("p1", &id, g1).unwrap(),
        Some(RequestState::Queued)
    );
    assert_eq!(
        s.status_for_requester("p1", &id, g2).unwrap(),
        Some(RequestState::Queued)
    );
    assert_eq!(
        s.status_for_requester("p1", &id, g3).unwrap(),
        None,
        "only requesters see status"
    );
    assert_eq!(raw_lines(&s.dir().join("queue.jsonl")).len(), 1);
}

#[test]
fn a_rejected_request_can_be_asked_for_again() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("again")), t0()).unwrap());
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    s.acknowledge(&AckRequest {
        project_id: "p1".into(),
        request_ids: vec![id.clone()],
        ack: Ack::Rejected {
            reason: "not now".into(),
        },
        actor: "pm".into(),
        now: t0(),
    })
    .unwrap();
    let again = queued_id(s.enqueue(sub("p1", g, "eng", task("again")), t0()).unwrap());
    assert_ne!(again, id);
}

// ── cross-project isolation ──────────────────────────────────────────────

#[test]
fn projects_never_see_dedupe_status_or_acks_of_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    let g = Uuid::new_v4();
    let a = queued_id(
        s.enqueue(sub("proj-a", g, "eng", task("same")), t0())
            .unwrap(),
    );
    // identical body in another project is its own request, not a duplicate
    let b = queued_id(
        s.enqueue(sub("proj-b", g, "eng", task("same")), t0())
            .unwrap(),
    );
    assert_ne!(a, b);
    // status is scoped by project even for the very same goal id
    assert_eq!(s.status_for_requester("proj-b", &a, g).unwrap(), None);
    assert_eq!(
        s.status_for_requester("proj-a", &a, g).unwrap(),
        Some(RequestState::Queued)
    );
    // list filter
    let only_b = s
        .list(
            &ListFilter {
                project_id: Some("proj-b".into()),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
    assert_eq!(only_b.requests.len(), 1);
    assert_eq!(only_b.requests[0].request_id, b);
    // an ack cannot reach into another project
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    let err = s
        .acknowledge(&AckRequest {
            project_id: "proj-b".into(),
            request_ids: vec![a.clone()],
            ack: Ack::Drafted {
                draft_id: "d".into(),
            },
            actor: "pm".into(),
            now: t0(),
        })
        .unwrap_err();
    assert!(matches!(err, IntakeError::UnknownRequest(_)));
}

#[test]
fn separate_project_roots_have_separate_stores() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let sa = store(root_a.path(), big_caps());
    let sb = store(root_b.path(), big_caps());
    let g = Uuid::new_v4();
    let id = queued_id(sa.enqueue(sub("p", g, "eng", task("x")), t0()).unwrap());
    assert_eq!(sb.status_for_requester("p", &id, g).unwrap(), None);
    assert!(sb
        .list(&ListFilter::default(), t0())
        .unwrap()
        .requests
        .is_empty());
}

// ── audit ────────────────────────────────────────────────────────────────

#[test]
fn every_transition_is_audited_with_request_goal_and_draft_ids() {
    let dir = tempfile::tempdir().unwrap();
    let audit = dir.path().join("audit.jsonl");
    let s = FileIntakeStore::open(dir.path(), big_caps(), Some(audit.clone()));
    let g = Uuid::new_v4();
    let id = queued_id(s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap());
    s.enqueue(sub("p1", Uuid::new_v4(), "qa", task("a")), t0())
        .unwrap(); // duplicate
    s.drain(&drain_req(t0(), 25, 30)).unwrap().unwrap();
    let ack = |a: Ack| AckRequest {
        project_id: "p1".into(),
        request_ids: vec![id.clone()],
        ack: a,
        actor: "pm".into(),
        now: t0(),
    };
    s.acknowledge(&ack(Ack::Drafted {
        draft_id: "draft-7".into(),
    }))
    .unwrap();
    s.acknowledge(&ack(Ack::Applied { draft_id: None }))
        .unwrap();

    let events = ta_audit::AuditLog::read_all(&audit).unwrap();
    let meta = |e: &ta_audit::AuditEvent| e.metadata.clone();
    let labels: Vec<String> = events
        .iter()
        .map(|e| meta(e)["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        labels,
        ["queued", "duplicate", "claimed", "drafted", "applied"]
    );
    for e in &events {
        assert_eq!(e.action, ta_audit::AuditAction::TaskIntake);
        assert_eq!(meta(e)["request_id"], id.as_str());
    }
    assert_eq!(meta(&events[0])["goal_id"], g.to_string());
    assert_eq!(events[0].goal_run_id, Some(g));
    assert_eq!(meta(&events[3])["draft_id"], "draft-7");
    assert_eq!(
        meta(&events[4])["draft_id"],
        "draft-7",
        "applied keeps the draft it came from"
    );
}

#[test]
fn refusals_are_audited_too() {
    let dir = tempfile::tempdir().unwrap();
    let audit = dir.path().join("audit.jsonl");
    let s = FileIntakeStore::open(dir.path(), caps(1, 100, 100), Some(audit.clone()));
    let g = Uuid::new_v4();
    s.enqueue(sub("p1", g, "eng", task("a")), t0()).unwrap();
    assert!(s.enqueue(sub("p1", g, "eng", task("b")), t0()).is_err());
    let events = ta_audit::AuditLog::read_all(&audit).unwrap();
    let last = events.last().unwrap().metadata.clone();
    assert_eq!(last["event"], "refused");
    assert_eq!(last["cap"], "per_goal_max");
}

#[test]
fn batch_ids_from_outside_cannot_escape_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), big_caps());
    assert!(matches!(
        s.batch_path("../../etc/passwd"),
        Err(IntakeError::BadBatchId(_))
    ));
    assert!(s.batch_path("20261017T030000Z-abcd1234").is_ok());
}
