// whiteboard_link.rs -- connecting the whiteboard transport with bounded
// exponential backoff (v0.17.12.16).
//
// The wake listener used to connect to NATS once. If the server was not up
// yet, the listener logged an error and never ran again, so a wake message
// was never answered and nothing in `/api/status` said so. Now a failed
// connection is retried for as long as the daemon runs:
//
//   * the wait doubles each attempt (`connect_retry_initial_secs`, default 1)
//     up to a cap (`connect_retry_max_secs`, default 60), with jitter so
//     several listeners do not retry in lockstep;
//   * every attempt is logged with the server, the attempt number and the
//     next delay;
//   * the link state is published in `/api/status` ("whiteboard disconnected")
//     and `/health` reports `degraded` with the reason until it reconnects.
//
// Both settings live under `[whiteboard]` in `.ta/workflow.toml`.

use std::time::Duration;

use rand::Rng;
use ta_agent_whiteboard::{WhiteboardConfig, WhiteboardTransport};

use crate::startup::{DaemonHealth, WhiteboardLink};

/// Bounded exponential backoff settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectBackoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for ConnectBackoff {
    fn default() -> Self {
        Self::from_config(&WhiteboardConfig::default())
    }
}

impl ConnectBackoff {
    pub fn from_config(c: &WhiteboardConfig) -> Self {
        let initial = Duration::from_secs(c.connect_retry_initial_secs.max(1));
        let max = Duration::from_secs(c.connect_retry_max_secs).max(initial);
        Self { initial, max }
    }

    pub fn load(project_root: &std::path::Path) -> Self {
        Self::from_config(&WhiteboardConfig::load(project_root))
    }

    /// The wait after failed attempt number `attempt` (1-based). `unit` is a
    /// random number in `[0, 1)`. The base doubles per attempt and is capped
    /// at `max`; "equal jitter" then picks a value between half the base and
    /// the base, so a retry is never immediate and never above the cap.
    pub fn delay_for(&self, attempt: u32, unit: f64) -> Duration {
        let doublings = attempt.saturating_sub(1).min(32);
        let base = self
            .initial
            .checked_mul(2u32.saturating_pow(doublings))
            .unwrap_or(self.max)
            .min(self.max);
        let half = base / 2;
        half + base.saturating_sub(half).mul_f64(unit.clamp(0.0, 1.0))
    }

    pub fn next_delay(&self, attempt: u32) -> Duration {
        self.delay_for(attempt, rand::thread_rng().gen::<f64>())
    }
}

/// What to call the server in logs and in `/api/status`.
pub fn describe_server(
    transport: &dyn WhiteboardTransport,
    project_root: &std::path::Path,
) -> String {
    match transport.backend_name() {
        "nats" => WhiteboardConfig::load(project_root).nats_url,
        other => format!("{other} (in-process)"),
    }
}

/// Connect, retrying forever with backoff. Returns `true` once connected and
/// `false` if the daemon began shutting down first.
///
/// `who` names the caller in logs (for example the listener's role). While
/// retrying, the link is published as `Disconnected` with the attempt, the
/// last error and the time to the next attempt.
pub async fn connect_with_retry(
    transport: &dyn WhiteboardTransport,
    health: &DaemonHealth,
    server: &str,
    who: &str,
    backoff: ConnectBackoff,
    shutdown: &tokio::sync::Notify,
) -> bool {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if matches!(
            health.whiteboard(),
            WhiteboardLink::Disabled | WhiteboardLink::Connecting { .. }
        ) {
            health.set_whiteboard(WhiteboardLink::Connecting {
                server: server.to_string(),
                attempt,
            });
        }
        match transport.connect().await {
            Ok(()) => {
                if attempt > 1 {
                    tracing::info!(
                        server = %server, who = %who, attempt,
                        "whiteboard: connected to {server} after {attempt} attempts"
                    );
                } else {
                    tracing::info!(server = %server, who = %who, "whiteboard: connected to {server}");
                }
                health.set_whiteboard(WhiteboardLink::Connected {
                    server: server.to_string(),
                });
                return true;
            }
            Err(e) => {
                let delay = backoff.next_delay(attempt);
                let error = e.to_string();
                tracing::warn!(
                    server = %server,
                    who = %who,
                    attempt,
                    next_delay_ms = delay.as_millis() as u64,
                    error = %error,
                    "whiteboard: connect attempt {attempt} to {server} failed ({error}); next \
                     attempt in {}s. Start the NATS server or fix nats_url under [whiteboard] \
                     in .ta/workflow.toml; the daemon keeps retrying",
                    delay.as_secs_f32().round() as u64,
                );
                health.set_whiteboard(WhiteboardLink::Disconnected {
                    server: server.to_string(),
                    attempt,
                    error,
                    retry_in: delay,
                });
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.notified() => return false,
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A transport that fails to connect a set number of times and can be
    //! "dropped" and "restored" mid-run. Wraps the in-memory transport.
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use ta_agent_whiteboard::{
        InMemoryTransport, Result, StreamEnvelope, WhiteboardError, WhiteboardTransport,
    };

    pub struct FlakyTransport {
        inner: InMemoryTransport,
        pub connect_failures_left: AtomicU32,
        pub connect_calls: AtomicU32,
        pub dropped: AtomicBool,
    }

    impl FlakyTransport {
        pub fn new(connect_failures: u32) -> Self {
            Self {
                inner: InMemoryTransport::new(),
                connect_failures_left: AtomicU32::new(connect_failures),
                connect_calls: AtomicU32::new(0),
                dropped: AtomicBool::new(false),
            }
        }
        fn check(&self) -> Result<()> {
            if self.dropped.load(Ordering::SeqCst) {
                Err(WhiteboardError::NotConnected(
                    "connection to the test server dropped".into(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[async_trait]
    impl WhiteboardTransport for FlakyTransport {
        fn backend_name(&self) -> &str {
            "flaky-test"
        }
        async fn connect(&self) -> Result<()> {
            self.connect_calls.fetch_add(1, Ordering::SeqCst);
            let left = self.connect_failures_left.load(Ordering::SeqCst);
            if left > 0 {
                self.connect_failures_left.store(left - 1, Ordering::SeqCst);
                return Err(WhiteboardError::ConnectFailed(
                    "nats://127.0.0.1:4222 (connection refused)".into(),
                ));
            }
            self.inner.connect().await
        }
        async fn kv_put(&self, b: &str, k: &str, v: Vec<u8>, t: Option<Duration>) -> Result<()> {
            self.check()?;
            self.inner.kv_put(b, k, v, t).await
        }
        async fn kv_create(&self, b: &str, k: &str, v: Vec<u8>) -> Result<bool> {
            self.check()?;
            self.inner.kv_create(b, k, v).await
        }
        async fn kv_get(&self, b: &str, k: &str) -> Result<Option<Vec<u8>>> {
            self.check()?;
            self.inner.kv_get(b, k).await
        }
        async fn kv_delete(&self, b: &str, k: &str) -> Result<()> {
            self.check()?;
            self.inner.kv_delete(b, k).await
        }
        async fn kv_list(&self, b: &str) -> Result<Vec<(String, Vec<u8>)>> {
            self.check()?;
            self.inner.kv_list(b).await
        }
        async fn stream_append(&self, s: &str, p: Vec<u8>) -> Result<()> {
            self.check()?;
            self.inner.stream_append(s, p).await
        }
        async fn stream_read_next(&self, s: &str, c: &str) -> Result<Option<StreamEnvelope>> {
            self.check()?;
            self.inner.stream_read_next(s, c).await
        }
        async fn stream_ack(&self, s: &str, c: &str, m: &str) -> Result<()> {
            self.check()?;
            self.inner.stream_ack(s, c, m).await
        }
        async fn stream_ack_progress(&self, s: &str, c: &str, m: &str) -> Result<()> {
            self.check()?;
            self.inner.stream_ack_progress(s, c, m).await
        }
        async fn stream_nak_delayed(&self, s: &str, c: &str, m: &str, d: Duration) -> Result<()> {
            self.check()?;
            self.inner.stream_nak_delayed(s, c, m, d).await
        }
        async fn stream_recover_pending(&self, s: &str, c: &str, m: &[String]) -> Result<usize> {
            self.check()?;
            self.inner.stream_recover_pending(s, c, m).await
        }
        async fn stream_set_ack_wait(&self, s: &str, c: &str, w: Duration) -> Result<()> {
            self.check()?;
            self.inner.stream_set_ack_wait(s, c, w).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FlakyTransport;
    use super::*;
    use crate::startup::Phase;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    fn fast() -> ConnectBackoff {
        ConnectBackoff {
            initial: Duration::from_millis(5),
            max: Duration::from_millis(20),
        }
    }

    #[test]
    fn backoff_doubles_is_capped_and_stays_above_half_the_base() {
        let b = ConnectBackoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
        };
        // Unit 0 is the shortest possible wait, unit ~1 the longest.
        let lo = |n| b.delay_for(n, 0.0).as_secs_f64();
        let hi = |n| b.delay_for(n, 0.999_999).as_secs_f64();
        assert_eq!((lo(1), lo(2), lo(3), lo(4)), (0.5, 1.0, 2.0, 4.0));
        assert!((hi(1) - 1.0).abs() < 0.01 && (hi(4) - 8.0).abs() < 0.01);
        // The cap holds no matter how many attempts have failed.
        for n in [7, 8, 20, 1_000, u32::MAX] {
            assert!(hi(n) <= 60.0, "attempt {n}: {}", hi(n));
            assert!(lo(n) >= 30.0, "never a hot retry at attempt {n}");
        }
    }

    #[test]
    fn backoff_settings_are_sanitized() {
        let c = WhiteboardConfig {
            connect_retry_initial_secs: 0,
            connect_retry_max_secs: 0,
            ..Default::default()
        };
        let b = ConnectBackoff::from_config(&c);
        assert_eq!(b.initial, Duration::from_secs(1), "0 means 1s");
        assert!(b.max >= b.initial, "the cap is never below the first wait");
    }

    #[tokio::test]
    async fn connect_fails_twice_then_succeeds_and_each_attempt_is_logged_and_reported() {
        let transport = FlakyTransport::new(2);
        let health = DaemonHealth::ready();
        let shutdown = tokio::sync::Notify::new();

        // Capture the log lines and sample the status between attempts.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        struct W(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let b2 = buf.clone();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(move || W(b2.clone()))
                .with_ansi(false)
                .finish(),
        );

        let sampler_health = health.clone();
        let phases = Arc::new(Mutex::new(Vec::new()));
        let p2 = phases.clone();
        let sampler = tokio::spawn(async move {
            loop {
                p2.lock().unwrap().push(sampler_health.phase());
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });

        let connected = connect_with_retry(
            &transport,
            &health,
            "nats://127.0.0.1:4222",
            "chief-of-staff",
            fast(),
            &shutdown,
        )
        .await;
        sampler.abort();

        assert!(connected);
        assert_eq!(transport.connect_calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            health.whiteboard(),
            WhiteboardLink::Connected {
                server: "nats://127.0.0.1:4222".into()
            }
        );
        assert_eq!(health.phase(), Phase::Ok);
        assert!(
            phases.lock().unwrap().contains(&Phase::Degraded),
            "status must have said degraded while the server was unreachable"
        );
        let log = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
        assert!(
            log.contains("connect attempt 1 to nats://127.0.0.1:4222 failed"),
            "{log}"
        );
        assert!(
            log.contains("connect attempt 2 to nats://127.0.0.1:4222 failed"),
            "{log}"
        );
        assert!(log.contains("next attempt in"), "{log}");
        assert!(
            log.contains("connected to nats://127.0.0.1:4222 after 3 attempts"),
            "{log}"
        );
    }

    #[tokio::test]
    async fn a_disconnected_link_names_the_server_attempt_and_next_step() {
        let transport = FlakyTransport::new(1_000);
        let health = DaemonHealth::ready();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let h = health.clone();
        let sd = shutdown.clone();
        let task = tokio::spawn(async move {
            connect_with_retry(&transport, &h, "nats://10.0.0.9:4222", "cos", fast(), &sd).await
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        let reason = &health.reasons()[0];
        assert_eq!(reason.component, "whiteboard");
        assert!(reason.reason.contains("whiteboard disconnected"));
        assert!(reason.reason.contains("nats://10.0.0.9:4222"));
        assert!(reason.next_step.contains("nats_url"));
        let mut json = serde_json::json!({});
        health.apply_to(&mut json);
        assert_eq!(json["whiteboard"]["state"], "disconnected");
        assert!(json["whiteboard"]["attempt"].as_u64().unwrap() >= 1);
        // Shutdown ends the retry loop instead of leaking the task.
        for _ in 0..100 {
            shutdown.notify_waiters();
            if task.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap());
    }
}
