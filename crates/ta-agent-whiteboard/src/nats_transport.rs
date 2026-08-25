//! `NatsTransport` — the default, recommended `WhiteboardTransport`
//! implementation, backed by NATS JetStream (v0.17.11.2 item 1).
//!
//! **Presence TTL is bucket-wide, not per-key** (see `transport.rs`'s module
//! doc for the rationale): true per-key TTL needs nats-server 2.11+'s
//! per-message-TTL feature enabled per-bucket (`limit_markers` in
//! `async-nats`'s `kv::Config`, gated behind the `server_2_11` cargo
//! feature), which adds a hard minimum server-version requirement for a
//! marginal semantic gain over "every entry in this bucket expires after
//! `max_age` unless refreshed" — sufficient for presence, which already
//! calls `kv_put` on every heartbeat. Revisit if a future bucket genuinely
//! needs per-key TTLs that differ within the same bucket.

use std::collections::HashMap;
use std::time::Duration;

use async_nats::jetstream::consumer::{pull, AckPolicy, Consumer};
use async_nats::jetstream::kv::Store;
use async_nats::jetstream::stream::Config as StreamConfig;
use async_nats::jetstream::{self, Context, Message};
use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::{Mutex, OnceCell};

use crate::error::{Result, WhiteboardError};
use crate::transport::{StreamEnvelope, WhiteboardTransport};

/// How long a pull-consumer fetch waits for a new message before returning
/// `None`. Short enough that `stream_read_next` behaves like a responsive
/// poll, long enough to avoid a tight empty-poll loop from a naive caller.
const FETCH_TIMEOUT: Duration = Duration::from_millis(500);

pub struct NatsTransport {
    url: String,
    context: OnceCell<Context>,
    kv_stores: Mutex<HashMap<String, Store>>,
    streams: Mutex<HashMap<String, jetstream::stream::Stream>>,
    consumers: Mutex<HashMap<(String, String), Consumer<pull::Config>>>,
    /// Pending, unacknowledged deliveries keyed by the opaque `msg_id` handed
    /// back to callers — the actual JetStream `Message` (which carries the
    /// broker-assigned ack reply subject) is stashed here between
    /// `stream_read_next` and `stream_ack`.
    ack_pending: Mutex<HashMap<String, Message>>,
}

impl NatsTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            context: OnceCell::new(),
            kv_stores: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            consumers: Mutex::new(HashMap::new()),
            ack_pending: Mutex::new(HashMap::new()),
        }
    }

    fn context(&self) -> Result<&Context> {
        self.context.get().ok_or_else(|| {
            WhiteboardError::NotConnected("NatsTransport::connect() was not called".into())
        })
    }

    /// Returns a cached KV store handle for `bucket`, creating it with
    /// `max_age` (`Duration::ZERO` = unlimited) only if it doesn't already
    /// exist. Never touches the config of an already-existing bucket, so a
    /// later call with a different `max_age` cannot silently clobber a TTL
    /// set by an earlier one.
    async fn ensure_bucket_for_write(&self, bucket: &str, max_age: Duration) -> Result<Store> {
        {
            let cache = self.kv_stores.lock().await;
            if let Some(store) = cache.get(bucket) {
                return Ok(store.clone());
            }
        }
        let ctx = self.context()?;
        let store = match ctx.get_key_value(bucket).await {
            Ok(store) => store,
            Err(_) => ctx
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: bucket.to_string(),
                    max_age,
                    ..Default::default()
                })
                .await
                .map_err(|e| kv_err(bucket, "<create>", e))?,
        };
        self.kv_stores
            .lock()
            .await
            .insert(bucket.to_string(), store.clone());
        Ok(store)
    }

    /// Returns a cached KV store handle for `bucket` if it exists, or `None`
    /// if the bucket has never been created — a pure lookup that never
    /// creates anything (used by the read paths, which must not implicitly
    /// materialize an empty bucket).
    async fn lookup_bucket(&self, bucket: &str) -> Result<Option<Store>> {
        {
            let cache = self.kv_stores.lock().await;
            if let Some(store) = cache.get(bucket) {
                return Ok(Some(store.clone()));
            }
        }
        let ctx = self.context()?;
        match ctx.get_key_value(bucket).await {
            Ok(store) => {
                self.kv_stores
                    .lock()
                    .await
                    .insert(bucket.to_string(), store.clone());
                Ok(Some(store))
            }
            Err(_) => Ok(None),
        }
    }

    async fn ensure_stream(&self, name: &str) -> Result<jetstream::stream::Stream> {
        {
            let cache = self.streams.lock().await;
            if let Some(s) = cache.get(name) {
                return Ok(s.clone());
            }
        }
        let subject = stream_subject(name);
        let ctx = self.context()?;
        let stream = ctx
            .get_or_create_stream(StreamConfig {
                name: stream_name(name),
                subjects: vec![subject],
                ..Default::default()
            })
            .await
            .map_err(|e| stream_err(name, e))?;
        self.streams
            .lock()
            .await
            .insert(name.to_string(), stream.clone());
        Ok(stream)
    }

    async fn ensure_consumer(
        &self,
        stream: &str,
        consumer: &str,
    ) -> Result<Consumer<pull::Config>> {
        let cache_key = (stream.to_string(), consumer.to_string());
        {
            let cache = self.consumers.lock().await;
            if let Some(c) = cache.get(&cache_key) {
                return Ok(c.clone());
            }
        }
        let s = self.ensure_stream(stream).await?;
        let c = s
            .get_or_create_consumer(
                consumer,
                pull::Config {
                    durable_name: Some(consumer.to_string()),
                    ack_policy: AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| stream_err(stream, e))?;
        self.consumers.lock().await.insert(cache_key, c.clone());
        Ok(c)
    }
}

fn stream_name(name: &str) -> String {
    format!("wb_{}", name)
}

fn stream_subject(name: &str) -> String {
    format!("wb.handoff.{}", name)
}

fn kv_err(bucket: &str, key: &str, detail: impl std::fmt::Display) -> WhiteboardError {
    WhiteboardError::Kv {
        bucket: bucket.to_string(),
        key: key.to_string(),
        detail: detail.to_string(),
    }
}

fn stream_err(stream: &str, detail: impl std::fmt::Display) -> WhiteboardError {
    WhiteboardError::Stream {
        stream: stream.to_string(),
        detail: detail.to_string(),
    }
}

#[async_trait]
impl WhiteboardTransport for NatsTransport {
    fn backend_name(&self) -> &str {
        "nats"
    }

    async fn connect(&self) -> Result<()> {
        if self.context.initialized() {
            return Ok(());
        }
        let client = async_nats::connect(&self.url)
            .await
            .map_err(|e| WhiteboardError::ConnectFailed(format!("{} ({e})", self.url)))?;
        let ctx = jetstream::new(client);
        // OnceCell::set races benignly — the loser's Context is dropped and
        // the winner's is used; both would have connected identically.
        let _ = self.context.set(ctx);
        Ok(())
    }

    async fn kv_put(
        &self,
        bucket: &str,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let store = self
            .ensure_bucket_for_write(bucket, ttl.unwrap_or(Duration::ZERO))
            .await?;
        store
            .put(key, value.into())
            .await
            .map_err(|e| kv_err(bucket, key, e))?;
        Ok(())
    }

    async fn kv_create(&self, bucket: &str, key: &str, value: Vec<u8>) -> Result<bool> {
        let store = self.ensure_bucket_for_write(bucket, Duration::ZERO).await?;
        match store.create(key, value.into()).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == async_nats::jetstream::kv::CreateErrorKind::AlreadyExists => {
                Ok(false)
            }
            Err(e) => Err(kv_err(bucket, key, e)),
        }
    }

    async fn kv_get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let Some(store) = self.lookup_bucket(bucket).await? else {
            return Ok(None);
        };
        let value = store.get(key).await.map_err(|e| kv_err(bucket, key, e))?;
        Ok(value.map(|b| b.to_vec()))
    }

    async fn kv_delete(&self, bucket: &str, key: &str) -> Result<()> {
        let Some(store) = self.lookup_bucket(bucket).await? else {
            return Ok(());
        };
        store
            .delete(key)
            .await
            .map_err(|e| kv_err(bucket, key, e))?;
        Ok(())
    }

    async fn kv_list(&self, bucket: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let Some(store) = self.lookup_bucket(bucket).await? else {
            return Ok(Vec::new());
        };
        let mut keys = store
            .keys()
            .await
            .map_err(|e| kv_err(bucket, "<keys>", e))?;
        let mut out = Vec::new();
        while let Some(key) = keys.next().await {
            let key = key.map_err(|e| kv_err(bucket, "<keys>", e))?;
            if let Some(value) = store.get(&key).await.map_err(|e| kv_err(bucket, &key, e))? {
                out.push((key, value.to_vec()));
            }
        }
        Ok(out)
    }

    async fn stream_append(&self, stream: &str, payload: Vec<u8>) -> Result<()> {
        self.ensure_stream(stream).await?;
        let ctx = self.context()?;
        let ack_future = ctx
            .publish(stream_subject(stream), payload.into())
            .await
            .map_err(|e| stream_err(stream, e))?;
        ack_future.await.map_err(|e| stream_err(stream, e))?;
        Ok(())
    }

    async fn stream_read_next(
        &self,
        stream: &str,
        consumer: &str,
    ) -> Result<Option<StreamEnvelope>> {
        let c = self.ensure_consumer(stream, consumer).await?;
        let mut batch = c
            .fetch()
            .max_messages(1)
            .expires(FETCH_TIMEOUT)
            .messages()
            .await
            .map_err(|e| stream_err(stream, e))?;
        match batch.next().await {
            Some(Ok(msg)) => {
                let msg_id = uuid::Uuid::new_v4().to_string();
                let payload = msg.payload.to_vec();
                self.ack_pending.lock().await.insert(msg_id.clone(), msg);
                Ok(Some(StreamEnvelope { msg_id, payload }))
            }
            Some(Err(e)) => Err(stream_err(stream, e)),
            None => Ok(None),
        }
    }

    async fn stream_ack(&self, stream: &str, _consumer: &str, msg_id: &str) -> Result<()> {
        let msg = self.ack_pending.lock().await.remove(msg_id);
        if let Some(msg) = msg {
            msg.ack()
                .await
                .map_err(|e| stream_err(stream, format!("ack failed: {e}")))?;
        }
        Ok(())
    }
}
