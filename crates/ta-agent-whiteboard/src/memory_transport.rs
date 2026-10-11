//! `InMemoryTransport` — a fully-functional, single-process
//! `WhiteboardTransport` implementation. Two roles: (1) proves the trait
//! boundary is real, not NATS-shaped in disguise (v0.17.11.2 item 9), and
//! (2) a legitimate standalone backend for single-agent/no-coordination-
//! needed setups per the design doc's item 1 ("an in-process no-op...
//! without touching whiteboard-consumer code anywhere else").

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::Result;
use crate::transport::{StreamEnvelope, WhiteboardTransport};

struct KvEntry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}

#[derive(Default)]
struct StreamState {
    messages: Vec<Vec<u8>>,
    /// Per-consumer: index of the next message to deliver.
    cursors: HashMap<String, usize>,
    /// Per-consumer: the delivered-but-unacked message and when its ack
    /// wait runs out. Until then it is not redelivered, mirroring JetStream.
    in_flight: HashMap<String, (usize, Instant)>,
    /// Per-consumer ack wait set by `stream_set_ack_wait`. A consumer with
    /// none (or zero) redelivers an unacked message on the very next read,
    /// which is this transport's original, simplest behavior.
    ack_waits: HashMap<String, Duration>,
}

#[derive(Default)]
struct Inner {
    /// Time added to the real clock by [`InMemoryTransport::advance`], so a
    /// test can step past an ack wait or a nak delay without sleeping.
    skew: Duration,
    kv: HashMap<String, HashMap<String, KvEntry>>,
    streams: HashMap<String, StreamState>,
}

/// In-process, in-memory `WhiteboardTransport`. Not shared across OS
/// processes — coordination only spans agents within the same daemon
/// process. Real multi-process/multi-machine coordination needs
/// `NatsTransport`.
#[derive(Default)]
pub struct InMemoryTransport {
    inner: Mutex<Inner>,
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move this transport's clock forward by `by`. Every deadline it keeps
    /// (KV expiry, ack wait, nak delay) is judged against the moved clock, so
    /// a test of a 30 s, 2 m or 10 m retry (or a one-hour ack wait) runs in
    /// microseconds and cannot flake.
    pub fn advance(&self, by: Duration) {
        self.inner.lock().unwrap().skew += by;
    }
}

impl Inner {
    fn now(&self) -> Instant {
        Instant::now() + self.skew
    }
}

#[async_trait]
impl WhiteboardTransport for InMemoryTransport {
    fn backend_name(&self) -> &str {
        "memory"
    }

    async fn connect(&self) -> Result<()> {
        Ok(())
    }

    async fn kv_put(
        &self,
        bucket: &str,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.now();
        let b = inner.kv.entry(bucket.to_string()).or_default();
        b.insert(
            key.to_string(),
            KvEntry {
                value,
                expires_at: ttl.map(|d| now + d),
            },
        );
        Ok(())
    }

    async fn kv_create(&self, bucket: &str, key: &str, value: Vec<u8>) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.now();
        let b = inner.kv.entry(bucket.to_string()).or_default();
        if let Some(existing) = b.get(key) {
            if !is_expired(existing, now) {
                return Ok(false);
            }
        }
        b.insert(
            key.to_string(),
            KvEntry {
                value,
                expires_at: None,
            },
        );
        Ok(true)
    }

    async fn kv_get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let inner = self.inner.lock().unwrap();
        let now = inner.now();
        Ok(inner
            .kv
            .get(bucket)
            .and_then(|b| b.get(key))
            .filter(|e| !is_expired(e, now))
            .map(|e| e.value.clone()))
    }

    async fn kv_delete(&self, bucket: &str, key: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(b) = inner.kv.get_mut(bucket) {
            b.remove(key);
        }
        Ok(())
    }

    async fn kv_list(&self, bucket: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let inner = self.inner.lock().unwrap();
        let now = inner.now();
        Ok(inner
            .kv
            .get(bucket)
            .map(|b| {
                b.iter()
                    .filter(|(_, e)| !is_expired(e, now))
                    .map(|(k, e)| (k.clone(), e.value.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn stream_append(&self, stream: &str, payload: Vec<u8>) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let s = inner.streams.entry(stream.to_string()).or_default();
        s.messages.push(payload);
        Ok(())
    }

    async fn stream_read_next(
        &self,
        stream: &str,
        consumer: &str,
    ) -> Result<Option<StreamEnvelope>> {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.now();
        let s = match inner.streams.get_mut(stream) {
            Some(s) => s,
            None => return Ok(None),
        };
        let cursor = *s.cursors.entry(consumer.to_string()).or_insert(0);
        let Some(payload) = s.messages.get(cursor) else {
            return Ok(None);
        };
        if let Some((idx, deadline)) = s.in_flight.get(consumer) {
            if *idx == cursor && now < *deadline {
                // Still within its ack wait: leased to the earlier read.
                return Ok(None);
            }
        }
        let ack_wait = s.ack_waits.get(consumer).copied().unwrap_or_default();
        // Recorded even with no ack wait (the lease then ends at once), so a
        // delayed nak can still find the delivery it applies to.
        s.in_flight
            .insert(consumer.to_string(), (cursor, now + ack_wait));
        Ok(Some(StreamEnvelope {
            msg_id: cursor.to_string(),
            payload: payload.clone(),
        }))
    }

    async fn stream_ack(&self, stream: &str, consumer: &str, msg_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.streams.get_mut(stream) {
            if let Ok(acked_index) = msg_id.parse::<usize>() {
                let cursor = s.cursors.entry(consumer.to_string()).or_insert(0);
                if *cursor == acked_index {
                    *cursor += 1;
                    s.in_flight.remove(consumer);
                }
            }
        }
        Ok(())
    }

    async fn stream_ack_progress(&self, stream: &str, consumer: &str, msg_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.now();
        if let Some(s) = inner.streams.get_mut(stream) {
            let ack_wait = s.ack_waits.get(consumer).copied().unwrap_or_default();
            if let (Ok(idx), Some(entry)) = (msg_id.parse::<usize>(), s.in_flight.get_mut(consumer))
            {
                if entry.0 == idx {
                    entry.1 = now + ack_wait;
                }
            }
        }
        Ok(())
    }

    async fn stream_nak_delayed(
        &self,
        stream: &str,
        consumer: &str,
        msg_id: &str,
        delay: Duration,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.now();
        if let (Some(s), Ok(idx)) = (inner.streams.get_mut(stream), msg_id.parse::<usize>()) {
            // Only a message that is currently leased to this consumer can be
            // nak'd; the delay replaces the rest of its ack wait.
            if s.in_flight.get(consumer).is_some_and(|(i, _)| *i == idx) {
                s.in_flight.insert(consumer.to_string(), (idx, now + delay));
            }
        }
        Ok(())
    }

    async fn stream_recover_pending(
        &self,
        stream: &str,
        consumer: &str,
        msg_ids: &[String],
    ) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        let Some(s) = inner.streams.get_mut(stream) else {
            return Ok(0);
        };
        let wanted: Vec<usize> = msg_ids.iter().filter_map(|m| m.parse().ok()).collect();
        // The cursor only ever sits on the oldest unacked message, so that is
        // the only one that can be leased.
        let leased = s
            .in_flight
            .get(consumer)
            .is_some_and(|(idx, _)| wanted.contains(idx));
        if leased {
            s.in_flight.remove(consumer);
            return Ok(1);
        }
        Ok(0)
    }

    async fn stream_set_ack_wait(
        &self,
        stream: &str,
        consumer: &str,
        ack_wait: Duration,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .streams
            .entry(stream.to_string())
            .or_default()
            .ack_waits
            .insert(consumer.to_string(), ack_wait);
        Ok(())
    }
}

fn is_expired(entry: &KvEntry, now: Instant) -> bool {
    matches!(entry.expires_at, Some(t) if now >= t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kv_put_get_roundtrip() {
        let t = InMemoryTransport::new();
        t.connect().await.unwrap();
        t.kv_put("bucket", "k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(t.kv_get("bucket", "k").await.unwrap(), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn kv_get_missing_key_returns_none() {
        let t = InMemoryTransport::new();
        assert_eq!(t.kv_get("bucket", "missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn kv_ttl_expiry_makes_entry_disappear() {
        let t = InMemoryTransport::new();
        t.kv_put(
            "bucket",
            "k",
            b"v".to_vec(),
            Some(Duration::from_millis(20)),
        )
        .await
        .unwrap();
        assert!(t.kv_get("bucket", "k").await.unwrap().is_some());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(t.kv_get("bucket", "k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn kv_list_excludes_expired_entries() {
        let t = InMemoryTransport::new();
        t.kv_put("bucket", "live", b"v".to_vec(), None)
            .await
            .unwrap();
        t.kv_put(
            "bucket",
            "dying",
            b"v".to_vec(),
            Some(Duration::from_millis(20)),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let listed = t.kv_list("bucket").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "live");
    }

    #[tokio::test]
    async fn kv_create_fails_if_key_already_exists() {
        let t = InMemoryTransport::new();
        assert!(t.kv_create("bucket", "k", b"first".to_vec()).await.unwrap());
        assert!(!t
            .kv_create("bucket", "k", b"second".to_vec())
            .await
            .unwrap());
        assert_eq!(
            t.kv_get("bucket", "k").await.unwrap(),
            Some(b"first".to_vec())
        );
    }

    #[tokio::test]
    async fn kv_create_succeeds_after_expiry() {
        let t = InMemoryTransport::new();
        t.kv_put(
            "bucket",
            "k",
            b"old".to_vec(),
            Some(Duration::from_millis(20)),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(t.kv_create("bucket", "k", b"new".to_vec()).await.unwrap());
    }

    #[tokio::test]
    async fn stream_delivers_in_order_and_redelivers_until_acked() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"one".to_vec()).await.unwrap();
        t.stream_append("s", b"two".to_vec()).await.unwrap();

        let first = t
            .stream_read_next("s", "consumer-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.payload, b"one");
        // Not acked yet — same message is redelivered.
        let redelivered = t
            .stream_read_next("s", "consumer-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(redelivered.msg_id, first.msg_id);

        t.stream_ack("s", "consumer-a", &first.msg_id)
            .await
            .unwrap();
        let second = t
            .stream_read_next("s", "consumer-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.payload, b"two");
    }

    #[tokio::test]
    async fn stream_survives_no_consumer_attached_at_send_time() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"durable".to_vec()).await.unwrap();
        // Consumer attaches only now, well after the send.
        let msg = t
            .stream_read_next("s", "late-consumer")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.payload, b"durable");
    }

    #[tokio::test]
    async fn independent_consumers_have_independent_cursors() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"one".to_vec()).await.unwrap();
        let a = t.stream_read_next("s", "a").await.unwrap().unwrap();
        t.stream_ack("s", "a", &a.msg_id).await.unwrap();
        assert!(t.stream_read_next("s", "a").await.unwrap().is_none());
        // "b" never read/acked — still sees the message.
        assert!(t.stream_read_next("s", "b").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unacked_message_is_leased_for_the_ack_wait_then_redelivered() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        t.stream_set_ack_wait("s", "c", Duration::from_millis(80))
            .await
            .unwrap();
        let first = t.stream_read_next("s", "c").await.unwrap().unwrap();
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(120)).await;
        let again = t.stream_read_next("s", "c").await.unwrap().unwrap();
        assert_eq!(first.msg_id, again.msg_id, "redelivery keeps the msg_id");
    }

    #[tokio::test]
    async fn ack_progress_extends_the_lease_and_ack_advances() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        t.stream_set_ack_wait("s", "c", Duration::from_millis(100))
            .await
            .unwrap();
        let env = t.stream_read_next("s", "c").await.unwrap().unwrap();
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            t.stream_ack_progress("s", "c", &env.msg_id).await.unwrap();
            assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        }
        t.stream_ack("s", "c", &env.msg_id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn without_an_ack_wait_an_unacked_message_is_redelivered_at_once() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        assert!(t.stream_read_next("s", "c").await.unwrap().is_some());
        assert!(t.stream_read_next("s", "c").await.unwrap().is_some());
    }

    const LONG_ACK_WAIT: Duration = Duration::from_secs(3660);

    #[tokio::test]
    async fn delayed_nak_redelivers_after_the_delay_not_the_long_ack_wait() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        t.stream_set_ack_wait("s", "c", LONG_ACK_WAIT)
            .await
            .unwrap();
        let first = t.stream_read_next("s", "c").await.unwrap().unwrap();
        t.stream_nak_delayed("s", "c", &first.msg_id, Duration::from_secs(30))
            .await
            .unwrap();
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        t.advance(Duration::from_secs(29));
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        t.advance(Duration::from_secs(2));
        let again = t.stream_read_next("s", "c").await.unwrap().unwrap();
        assert_eq!(first.msg_id, again.msg_id, "redelivery keeps the msg_id");
    }

    #[tokio::test]
    async fn without_a_nak_the_long_ack_wait_holds_the_message() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        t.stream_set_ack_wait("s", "c", LONG_ACK_WAIT)
            .await
            .unwrap();
        t.stream_read_next("s", "c").await.unwrap().unwrap();
        t.advance(Duration::from_secs(600));
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        t.advance(Duration::from_secs(3100));
        assert!(t.stream_read_next("s", "c").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delayed_nak_works_without_an_ack_wait_and_ignores_unknown_ids() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        let first = t.stream_read_next("s", "c").await.unwrap().unwrap();
        t.stream_nak_delayed("s", "c", "99", Duration::from_secs(5))
            .await
            .unwrap();
        t.stream_nak_delayed("s", "c", &first.msg_id, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        t.advance(Duration::from_secs(6));
        assert!(t.stream_read_next("s", "c").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn recover_pending_releases_a_message_left_leased_by_an_earlier_run() {
        let t = InMemoryTransport::new();
        t.stream_append("s", b"m".to_vec()).await.unwrap();
        t.stream_set_ack_wait("s", "c", LONG_ACK_WAIT)
            .await
            .unwrap();
        let first = t.stream_read_next("s", "c").await.unwrap().unwrap();
        assert!(t.stream_read_next("s", "c").await.unwrap().is_none());
        let n = t
            .stream_recover_pending("s", "c", &["77".to_string()])
            .await
            .unwrap();
        assert_eq!(n, 0, "an id that is not leased is not counted");
        let n = t
            .stream_recover_pending("s", "c", std::slice::from_ref(&first.msg_id))
            .await
            .unwrap();
        assert_eq!(n, 1);
        let again = t.stream_read_next("s", "c").await.unwrap().unwrap();
        assert_eq!(first.msg_id, again.msg_id);
    }

    #[tokio::test]
    async fn kv_expiry_follows_the_controllable_clock() {
        let t = InMemoryTransport::new();
        t.kv_put("b", "k", b"v".to_vec(), Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert!(t.kv_get("b", "k").await.unwrap().is_some());
        t.advance(Duration::from_secs(61));
        assert!(t.kv_get("b", "k").await.unwrap().is_none());
    }
}
