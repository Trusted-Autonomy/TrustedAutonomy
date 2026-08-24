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

struct StreamState {
    messages: Vec<Vec<u8>>,
    /// Per-consumer: index of the next message to deliver.
    cursors: HashMap<String, usize>,
}

#[derive(Default)]
struct Inner {
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
        let b = inner.kv.entry(bucket.to_string()).or_default();
        b.insert(
            key.to_string(),
            KvEntry {
                value,
                expires_at: ttl.map(|d| Instant::now() + d),
            },
        );
        Ok(())
    }

    async fn kv_create(&self, bucket: &str, key: &str, value: Vec<u8>) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let b = inner.kv.entry(bucket.to_string()).or_default();
        if let Some(existing) = b.get(key) {
            if !is_expired(existing) {
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
        Ok(inner
            .kv
            .get(bucket)
            .and_then(|b| b.get(key))
            .filter(|e| !is_expired(e))
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
        Ok(inner
            .kv
            .get(bucket)
            .map(|b| {
                b.iter()
                    .filter(|(_, e)| !is_expired(e))
                    .map(|(k, e)| (k.clone(), e.value.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn stream_append(&self, stream: &str, payload: Vec<u8>) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let s = inner
            .streams
            .entry(stream.to_string())
            .or_insert_with(|| StreamState {
                messages: Vec::new(),
                cursors: HashMap::new(),
            });
        s.messages.push(payload);
        Ok(())
    }

    async fn stream_read_next(
        &self,
        stream: &str,
        consumer: &str,
    ) -> Result<Option<StreamEnvelope>> {
        let mut inner = self.inner.lock().unwrap();
        let s = match inner.streams.get_mut(stream) {
            Some(s) => s,
            None => return Ok(None),
        };
        let cursor = *s.cursors.entry(consumer.to_string()).or_insert(0);
        match s.messages.get(cursor) {
            Some(payload) => Ok(Some(StreamEnvelope {
                msg_id: cursor.to_string(),
                payload: payload.clone(),
            })),
            None => Ok(None),
        }
    }

    async fn stream_ack(&self, stream: &str, consumer: &str, msg_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.streams.get_mut(stream) {
            if let Ok(acked_index) = msg_id.parse::<usize>() {
                let cursor = s.cursors.entry(consumer.to_string()).or_insert(0);
                if *cursor == acked_index {
                    *cursor += 1;
                }
            }
        }
        Ok(())
    }
}

fn is_expired(entry: &KvEntry) -> bool {
    matches!(entry.expires_at, Some(t) if Instant::now() >= t)
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
}
