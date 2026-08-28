//! Discovery/query API — "who else is active," "is anyone touching
//! `<path>`/`<api_impact tag>` right now" (v0.17.11.2 item 3). A thin
//! wrapper over the presence bucket, callable both from the daemon (a live
//! complement to `task-graph`'s static wave planning, for pre-launch
//! conflict checks) and from within a running agent for opportunistic
//! self-coordination.
//!
//! Point-in-time snapshots (`kv_list` + filter), not a live subscription —
//! see `transport.rs`'s module doc for why there's no push-based watch here.

use crate::error::Result;
use crate::presence::{PresenceRecord, PRESENCE_BUCKET};
use crate::resource_match::glob_overlap;
use crate::transport::WhiteboardTransport;

/// All currently-live presence records — "what is everyone doing right now."
/// Malformed entries (a foreign writer, a schema mismatch) are skipped with
/// a `tracing::warn!` rather than failing the whole query.
pub async fn list_active_agents(
    transport: &dyn WhiteboardTransport,
) -> Result<Vec<PresenceRecord>> {
    let raw = transport.kv_list(PRESENCE_BUCKET).await?;
    let mut records = Vec::with_capacity(raw.len());
    for (key, value) in raw {
        match serde_json::from_slice::<PresenceRecord>(&value) {
            Ok(record) => records.push(record),
            Err(e) => tracing::warn!(key, error = %e, "skipping malformed presence record"),
        }
    }
    Ok(records)
}

/// Presence records for agents whose declared `source_dir` matches
/// `source_dir` (exact match — presence is scoped per project checkout,
/// not fuzzy-matched).
pub async fn active_agents_for_source(
    transport: &dyn WhiteboardTransport,
    source_dir: &str,
) -> Result<Vec<PresenceRecord>> {
    Ok(list_active_agents(transport)
        .await?
        .into_iter()
        .filter(|r| r.source_dir == source_dir)
        .collect())
}

/// Presence records whose declared `resources` overlap `resource_patterns`
/// — glob-matched (`task-graph`'s existing `api_impact`/file-glob
/// vocabulary), scoped to the given `source_dir` so two unrelated projects
/// touching a coincidentally-identical-looking path never collide.
///
/// This is advisory information, not enforcement: file-level conflict
/// *prevention* below the `api_impact`-tag granularity is explicitly out of
/// scope for v1 (item 7) — this answers "is anyone touching this," it does
/// not block anyone from doing so.
pub async fn is_anyone_touching(
    transport: &dyn WhiteboardTransport,
    source_dir: &str,
    resource_patterns: &[String],
) -> Result<Vec<PresenceRecord>> {
    if resource_patterns.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = active_agents_for_source(transport, source_dir).await?;
    Ok(candidates
        .into_iter()
        .filter(|record| {
            record
                .resources
                .iter()
                .any(|declared| glob_overlap(declared, resource_patterns))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_transport::InMemoryTransport;
    use crate::presence::publish_presence;
    use std::time::Duration;

    async fn seed(t: &InMemoryTransport, agent: &str, source_dir: &str, resources: &[&str]) {
        let record = PresenceRecord::new(agent, format!("goal-{agent}"), source_dir)
            .with_resources(resources.iter().map(|s| s.to_string()).collect());
        publish_presence(t, &record, Duration::from_secs(60))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_active_agents_reflects_concurrent_declared_activity() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/repo", &["src/**"]).await;
        seed(&t, "b", "/repo", &["docs/**"]).await;
        let active = list_active_agents(&t).await.unwrap();
        assert_eq!(active.len(), 2);
    }

    #[tokio::test]
    async fn active_agents_for_source_filters_by_source_dir() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/repo-one", &[]).await;
        seed(&t, "b", "/repo-two", &[]).await;
        let active = active_agents_for_source(&t, "/repo-one").await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent_id, "a");
    }

    #[tokio::test]
    async fn is_anyone_touching_matches_overlapping_glob() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/repo", &["src/auth/**"]).await;
        seed(&t, "b", "/repo", &["docs/**"]).await;

        let hits = is_anyone_touching(&t, "/repo", &["src/auth/login.rs".to_string()])
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].agent_id, "a");
    }

    #[tokio::test]
    async fn is_anyone_touching_matches_when_query_is_the_broader_glob() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/repo", &["src/auth/login.rs"]).await;

        let hits = is_anyone_touching(&t, "/repo", &["src/auth/**".to_string()])
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn is_anyone_touching_returns_empty_when_no_overlap() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/repo", &["docs/**"]).await;
        let hits = is_anyone_touching(&t, "/repo", &["src/**".to_string()])
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn is_anyone_touching_scopes_to_source_dir() {
        let t = InMemoryTransport::new();
        seed(&t, "a", "/other-repo", &["src/**"]).await;
        let hits = is_anyone_touching(&t, "/repo", &["src/**".to_string()])
            .await
            .unwrap();
        assert!(hits.is_empty());
    }
}
