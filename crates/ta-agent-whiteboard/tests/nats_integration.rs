//! Integration tests against a real, locally-spawned `nats-server` — proves
//! `NatsTransport` actually works end-to-end, not just that it compiles
//! against the `async-nats` API surface.
//!
//! `#[ignore]`d by default: these require `nats-server` on `$PATH` (install
//! via `brew install nats-server` / see nats.io), which CI is not
//! guaranteed to have. Run explicitly with:
//!
//! ```bash
//! cargo test -p ta-agent-whiteboard -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` because each test spawns its own `nats-server` on a
//! fixed port range picked to avoid colliding with a real default NATS
//! deployment (4222) — running them concurrently would need per-test port
//! allocation, not worth the complexity for a local-only verification pass.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ta_agent_whiteboard::discovery::{is_anyone_touching, list_active_agents};
use ta_agent_whiteboard::handoff::{ack_handoff, receive_handoff, send_handoff, HandoffMessage};
use ta_agent_whiteboard::presence::{publish_presence, PresenceRecord};
use ta_agent_whiteboard::tasks::{claim_task, claimable_tasks, publish_task, WhiteboardTask};
use ta_agent_whiteboard::{NatsTransport, WhiteboardTransport};

struct TestServer {
    child: Child,
    url: String,
}

impl TestServer {
    async fn spawn(port: u16) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let child = Command::new("nats-server")
            .args([
                "-js",
                "-p",
                &port.to_string(),
                "--store_dir",
                dir.path().to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("nats-server must be on $PATH — install via `brew install nats-server`");
        // Give it a moment to bind before any test tries to connect.
        tokio::time::sleep(Duration::from_millis(400)).await;
        // tempdir must outlive the server process, but we don't need the
        // handle after spawn — leak it deliberately (test-only, tiny, and
        // the OS reclaims it at process exit).
        std::mem::forget(dir);
        Self {
            child,
            url: format!("localhost:{port}"),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
#[ignore]
async fn nats_kv_put_get_roundtrip() {
    let server = TestServer::spawn(14222).await;
    let t = NatsTransport::new(&server.url);
    t.connect().await.unwrap();

    t.kv_put("test-bucket", "k", b"v".to_vec(), None)
        .await
        .unwrap();
    assert_eq!(
        t.kv_get("test-bucket", "k").await.unwrap(),
        Some(b"v".to_vec())
    );
}

#[tokio::test]
#[ignore]
async fn nats_kv_create_is_race_free_cas() {
    let server = TestServer::spawn(14223).await;
    let t = NatsTransport::new(&server.url);
    t.connect().await.unwrap();

    assert!(t
        .kv_create("test-bucket", "k", b"first".to_vec())
        .await
        .unwrap());
    assert!(!t
        .kv_create("test-bucket", "k", b"second".to_vec())
        .await
        .unwrap());
    assert_eq!(
        t.kv_get("test-bucket", "k").await.unwrap(),
        Some(b"first".to_vec())
    );
}

#[tokio::test]
#[ignore]
async fn nats_presence_ttl_expires_a_crashed_agents_entry() {
    let server = TestServer::spawn(14224).await;
    let t = NatsTransport::new(&server.url);
    t.connect().await.unwrap();

    let record = PresenceRecord::new("crashed-agent", "goal-1", "/repo");
    publish_presence(&t, &record, Duration::from_secs(1))
        .await
        .unwrap();

    let active = list_active_agents(&t).await.unwrap();
    assert_eq!(active.len(), 1);

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let active = list_active_agents(&t).await.unwrap();
    assert!(
        active.is_empty(),
        "presence must expire without manual cleanup"
    );
}

#[tokio::test]
#[ignore]
async fn nats_discovery_finds_overlapping_resource_declarations() {
    let server = TestServer::spawn(14225).await;
    let t = NatsTransport::new(&server.url);
    t.connect().await.unwrap();

    let record = PresenceRecord::new("agent-1", "goal-1", "/repo")
        .with_resources(vec!["src/auth/**".to_string()]);
    publish_presence(&t, &record, Duration::from_secs(30))
        .await
        .unwrap();

    let hits = is_anyone_touching(&t, "/repo", &["src/auth/login.rs".to_string()])
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].agent_id, "agent-1");
}

#[tokio::test]
#[ignore]
async fn nats_task_claiming_is_race_free_between_two_transports() {
    let server = TestServer::spawn(14226).await;
    // Two independent client connections, simulating two separate agent
    // processes racing for the same task.
    let a = NatsTransport::new(&server.url);
    a.connect().await.unwrap();
    let b = NatsTransport::new(&server.url);
    b.connect().await.unwrap();

    publish_task(&a, &WhiteboardTask::new("task-1", "do the thing"))
        .await
        .unwrap();
    assert_eq!(claimable_tasks(&a).await.unwrap().len(), 1);

    let a_won = claim_task(&a, "task-1", "agent-a").await.unwrap();
    let b_won = claim_task(&b, "task-1", "agent-b").await.unwrap();
    assert!(a_won);
    assert!(
        !b_won,
        "a real JetStream KV create must reject the second claimant"
    );
}

#[tokio::test]
#[ignore]
async fn nats_handoff_is_durable_across_reconnect() {
    let server = TestServer::spawn(14227).await;
    let sender = NatsTransport::new(&server.url);
    sender.connect().await.unwrap();

    let recipient = ta_session::RoleRef::Agent("agent-2".to_string());
    let msg = HandoffMessage::new("agent-1", recipient.clone(), "durable payload");
    send_handoff(&sender, &msg).await.unwrap();

    // A brand new connection, simulating the recipient process starting up
    // well after the message was sent.
    let receiver = NatsTransport::new(&server.url);
    receiver.connect().await.unwrap();
    let delivered = receive_handoff(&receiver, &recipient)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.message.payload, "durable payload");
    ack_handoff(&receiver, delivered).await.unwrap();

    assert!(receive_handoff(&receiver, &recipient)
        .await
        .unwrap()
        .is_none());
}
