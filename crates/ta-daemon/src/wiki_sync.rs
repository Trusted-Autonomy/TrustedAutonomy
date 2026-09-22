// wiki_sync.rs -- Periodic background sync of the Wayfinder wiki cache.
//
// Today `ta_wiki_get` (`ta-mcp-gateway/src/tools/wiki.rs`) only populates
// `.ta/wiki-cache/` reactively, on its own cache misses. That means the
// very first `ta_wiki_get` call for any page always pays a live Wayfinder
// round trip, and a page nobody has asked for yet stays unfetched
// indefinitely even if it changed upstream. This loop closes that gap: it
// walks every scope declared in `.ta/wiki-resources.toml`, diffs
// `wiki_manifest`'s sha list against what's already cached
// (`WikiCache::known_shas`), and proactively fetches anything new or
// changed -- so a role's first real query in a session is usually already
// a cache hit.
//
// Independent daemon-startup task, same shape as `token_refresh.rs` and
// `watchdog::run_watchdog`'s periodic-scan pattern -- not anchored to
// `team_session.rs`'s rotation loop for the same reason `token_refresh.rs`
// isn't (see that module's doc comment): a session with zero rotation
// stages stops that loop after one iteration, and wiki freshness has
// nothing to do with whether any team session happens to be active.
//
// A project that never ran `wayfinder-pair.sh`'s wiki-writer step (or
// never paired with Wayfinder at all) has no `.ta/wiki-resources.toml` --
// this loop treats that as "not configured," not an error, and does
// nothing for that project, mirroring `token_refresh.rs`'s treatment of a
// session with no whiteboard token.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ta_mcp_gateway::wiki_cache::WikiCache;
use ta_mcp_gateway::wiki_resources::{self, WikiResourcesConfig};

/// How often this loop wakes to check for upstream wiki changes. The wiki
/// is deliberately stale-tolerant (see the retrieval design doc), so this
/// doesn't need to be aggressive -- proactive warmth, not real-time sync.
const CHECK_INTERVAL: Duration = Duration::from_secs(1800); // 30 min

/// Spawns the daemon-lifetime wiki sync loop.
pub fn start(
    project_root: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = sync_once(&project_root).await {
                tracing::warn!(
                    error = %e,
                    "wiki_sync: failed to sync the wiki cache -- will retry next check cycle"
                );
            }
            tokio::select! {
                _ = tokio::time::sleep(CHECK_INTERVAL) => {}
                _ = shutdown.notified() => return,
            }
        }
    })
}

/// One full sync pass across every declared scope. Synchronous config/
/// credential loading, async network calls -- same split
/// `token_refresh::refresh_one_session` uses for its own state I/O, except
/// this task's actual page fetches are genuinely async (a real MCP call to
/// Wayfinder), unlike token minting's local broker call.
async fn sync_once(project_root: &Path) -> anyhow::Result<()> {
    if !wiki_resources::wiki_resources_configured(project_root) {
        return Ok(());
    }
    let config = wiki_resources::load_wiki_resources_config(project_root)?;
    let use_keychain = wiki_resources::default_use_keychain(project_root);
    let cache = WikiCache::new(project_root);

    for scope in &config.scopes {
        if let Err(e) = sync_scope(project_root, use_keychain, &config, &cache, scope).await {
            tracing::warn!(
                scope = %scope.scope,
                id = %scope.id,
                error = %e,
                "wiki_sync: failed to sync scope '{}' ({}) -- continuing with remaining scopes",
                scope.name,
                scope.id,
            );
        }
    }
    Ok(())
}

async fn sync_scope(
    project_root: &Path,
    use_keychain: bool,
    config: &WikiResourcesConfig,
    cache: &WikiCache,
    scope: &wiki_resources::ScopeConfig,
) -> anyhow::Result<()> {
    let client = wiki_resources::read_client(project_root, use_keychain, config)?;
    let manifest = client.wiki_manifest(&scope.scope, &scope.id).await?;
    let known = cache.known_shas(&scope.scope, &scope.id)?;

    let mut fetched = 0usize;
    for entry in &manifest {
        if known.get(&entry.page_id) == Some(&entry.sha) {
            continue; // already cached at the current sha, nothing to do
        }
        let page = client
            .wiki_get(&scope.scope, &scope.id, &entry.page_id)
            .await?;
        cache.put(&scope.scope, &scope.id, &page)?;
        fetched += 1;
    }

    if fetched > 0 {
        tracing::info!(
            scope = %scope.scope,
            id = %scope.id,
            fetched,
            total = manifest.len(),
            "wiki_sync: warmed {fetched} new/changed page(s) for '{}' ({})",
            scope.name,
            scope.id,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(project_root: &Path, base_url: &str) {
        std::fs::create_dir_all(project_root.join(".ta")).unwrap();
        std::fs::write(
            project_root.join(".ta").join("wiki-resources.toml"),
            format!(
                r#"
[[scopes]]
name = "project"
scope = "project"
id = "proj-1"

[wayfinder]
base_url = "{base_url}"
read_credential_name = "wayfinder-service-account"
write_credential_name = "wayfinder-wiki-writer"
"#
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn sync_once_is_a_no_op_when_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        // No .ta/wiki-resources.toml at all.
        sync_once(dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn sync_once_reports_config_error_when_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("wiki-resources.toml"),
            "not valid toml {{{",
        )
        .unwrap();
        let result = sync_once(dir.path()).await;
        assert!(
            result.is_err(),
            "malformed config must surface as an error, not a silent no-op"
        );
    }

    // multi_thread: a plain #[tokio::test] runs a single-worker-thread
    // runtime. `WikiMcpClient::call_tool`'s own internal timeout (see
    // wiki_client.rs's CALL_TOOL_TIMEOUT) should already bound the
    // connection attempt below, but a real CI hang on macOS runners
    // (connecting to a closed local port apparently never resolves to any
    // readiness event there, an OS/sandbox quirk, not reproduced on Linux,
    // Windows, or this machine) outlasted even that: a multi_thread runtime
    // plus the explicit outer bound below are a second, independent line of
    // defense that doesn't depend on diagnosing that quirk further -- this
    // test must never again be able to hang a CI job regardless of what the
    // underlying connection attempt does.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_once_with_configured_but_unreachable_wayfinder_does_not_panic() {
        // No mock server listening on this port -- confirms a real
        // connection failure is swallowed into a per-scope warning
        // (logged, not propagated) rather than crashing the sync loop,
        // matching token_refresh's per-session error isolation.
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "http://127.0.0.1:1/mcp");
        // No credential stored either -- confirms the "no such credential"
        // path is likewise contained to this one scope.
        //
        // Explicit outer bound, independent of WikiMcpClient's own internal
        // timeout: this test must fail loudly (not hang the CI job) if
        // sync_once ever again takes longer than a real connection attempt
        // reasonably should, on any platform.
        match tokio::time::timeout(std::time::Duration::from_secs(45), sync_once(dir.path())).await
        {
            Ok(result) => result.unwrap(),
            Err(_elapsed) => panic!(
                "sync_once did not return within 45s against an unreachable Wayfinder endpoint \
                 -- this must never hang, see this test's own comment"
            ),
        }
    }
}
