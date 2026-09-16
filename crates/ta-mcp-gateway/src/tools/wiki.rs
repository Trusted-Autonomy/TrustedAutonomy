//! `ta_wiki_*` MCP tools: reading and writing Wayfinder's org/project
//! wiki, per `docs/superpowers/specs/2026-09-15-virtual-team-wiki-retrieval-design.md`
//! in the `ta-virtual-team` repo.
//!
//! ## Why this proxies through the gateway (security-load-bearing)
//!
//! An agent's own MCP client never talks to Wayfinder directly and never
//! holds the Wayfinder bearer token: this module does, resolved via
//! `ta-credentials` (broker-mediated), the same "agents never hold raw
//! credentials" principle `ta-credentials`' own docs establish and every
//! other connector-broker-mediated secret in this codebase already
//! follows. See the design doc's "The core architectural decision"
//! section for the full rationale.
//!
//! ## Two credentials, not one
//!
//! Reads (`search`/`get`/`types`) use the `wayfinder-service-account`
//! credential (general, member-rank, the same one `wayfinder-pair.sh`
//! already stores during pairing). Writes (`create`/`update`) use
//! `wayfinder-wiki-writer` (admin-rank, provisioned specifically for wiki
//! writes): see the design doc's "Credentials: two identities" section
//! for why one identity doesn't cover both.
//!
//! ## Config source
//!
//! `.ta/wiki-resources.toml` in the real project root (not a goal's
//! staging copy, same reasoning `tools/whiteboard.rs` documents for its
//! own daemon client). A project with no such file has no wiki access:
//! every tool here fails with a clear, actionable error, never a silent
//! no-op.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::server::GatewayState;
use crate::wiki_cache::WikiCache;
use crate::wiki_client::{WikiMcpClient, WikiPage};

use super::sync_bridge::run_on_dedicated_thread;

// ── Config: .ta/wiki-resources.toml ─────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct WikiResourcesConfig {
    #[serde(default)]
    scopes: Vec<ScopeConfig>,
    wayfinder: WayfinderConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct ScopeConfig {
    /// Human-readable label, not otherwise used by these tools (callers
    /// address a scope by `scope`+`id`, matching Wayfinder's own tool
    /// signatures), kept for the config file's own readability and for
    /// the background sync task (sub-project 1's daemon task, not yet
    /// implemented) to enumerate what to sync.
    #[allow(dead_code)]
    name: String,
    /// `"project"` or `"org"`.
    scope: String,
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WayfinderConfig {
    /// Wayfinder's wiki MCP endpoint, e.g. `https://wayfinder.example.com/mcp`.
    base_url: String,
    read_credential_name: String,
    write_credential_name: String,
}

fn load_wiki_resources_config(project_root: &Path) -> Result<WikiResourcesConfig, McpError> {
    let path = project_root.join(".ta").join("wiki-resources.toml");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        McpError::invalid_request(
            format!(
                "ta_wiki_*: no wiki configuration found at {} ({e}). Wiki access requires a \
                 .ta/wiki-resources.toml declaring the org/project scopes to reach and the \
                 Wayfinder credentials to use; see the virtual-team install+config docs.",
                path.display()
            ),
            None,
        )
    })?;
    toml::from_str(&raw).map_err(|e| {
        McpError::internal_error(
            format!("ta_wiki_*: malformed {} ({e}).", path.display()),
            None,
        )
    })
}

/// Confirms `scope`/`id` is one this project actually declared, rather
/// than silently proxying a call for an org/project this installation was
/// never configured to reach, the same "no silent pass-through of an
/// unvalidated caller-supplied identifier" discipline
/// `resolve_connector_authorization` (`tools/action.rs`) already applies
/// to connector ids.
fn require_declared_scope(
    config: &WikiResourcesConfig,
    scope: &str,
    id: &str,
) -> Result<(), McpError> {
    let declared = config.scopes.iter().any(|s| s.scope == scope && s.id == id);
    if declared {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            format!(
                "ta_wiki_*: scope '{scope}' id '{id}' is not declared in \
                 .ta/wiki-resources.toml. Add it under [[scopes]] before querying it."
            ),
            None,
        ))
    }
}

// ── Credential resolution ────────────────────────────────────────────────

fn resolve_credential_secret(
    project_root: &Path,
    use_keychain: bool,
    credential_name: &str,
) -> Result<String, McpError> {
    use ta_credentials::CredentialVault;

    let mut cred_config = ta_credentials::CredentialsConfig::for_project(project_root);
    cred_config.use_keychain = use_keychain;
    let vault = ta_credentials::FileVault::open(&cred_config).map_err(|e| {
        McpError::internal_error(
            format!("ta_wiki_*: could not open the credential vault: {e}"),
            None,
        )
    })?;
    let summaries = vault.list().map_err(|e| {
        McpError::internal_error(format!("ta_wiki_*: could not list credentials: {e}"), None)
    })?;
    let summary = summaries
        .iter()
        .find(|c| c.name == credential_name)
        .ok_or_else(|| {
            McpError::invalid_request(
                format!(
                    "ta_wiki_*: no credential named '{credential_name}' found. Run \
                     `ta credentials list` to see what's stored, or re-run the pairing step \
                     if this project hasn't been paired with Wayfinder yet."
                ),
                None,
            )
        })?;
    let full = vault.get(summary.id).map_err(|e| {
        McpError::internal_error(
            format!("ta_wiki_*: could not resolve credential '{credential_name}': {e}"),
            None,
        )
    })?;
    Ok(full.secret)
}

// ── Shared plumbing ──────────────────────────────────────────────────────

struct WikiContext {
    project_root: PathBuf,
    config: WikiResourcesConfig,
    use_keychain: bool,
}

fn load_context(state: &Arc<Mutex<GatewayState>>) -> Result<WikiContext, McpError> {
    let (project_root, use_keychain) = {
        let locked = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        (
            locked.config.workspace_root.clone(),
            locked.config.credential_vault_use_keychain,
        )
    };
    let config = load_wiki_resources_config(&project_root)?;
    Ok(WikiContext {
        project_root,
        config,
        use_keychain,
    })
}

fn read_client(ctx: &WikiContext) -> Result<WikiMcpClient, McpError> {
    let token = resolve_credential_secret(
        &ctx.project_root,
        ctx.use_keychain,
        &ctx.config.wayfinder.read_credential_name,
    )?;
    Ok(WikiMcpClient::new(
        ctx.config.wayfinder.base_url.clone(),
        token,
    ))
}

fn write_client(ctx: &WikiContext) -> Result<WikiMcpClient, McpError> {
    let token = resolve_credential_secret(
        &ctx.project_root,
        ctx.use_keychain,
        &ctx.config.wayfinder.write_credential_name,
    )?;
    Ok(WikiMcpClient::new(
        ctx.config.wayfinder.base_url.clone(),
        token,
    ))
}

fn success_json(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::json(value)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn page_to_json(page: &WikiPage) -> serde_json::Value {
    serde_json::json!({
        "id": page.id,
        "title": page.title,
        "body": page.body,
        "type": page.r#type,
        "tags": page.tags,
        "sha": page.sha,
        "updated_at": page.updated_at,
    })
}

// ── ta_wiki_search ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiSearchParams {
    /// `"project"` or `"org"`.
    pub scope: String,
    /// The project id or org id, matching `scope`.
    pub id: String,
    pub query: String,
}

pub fn handle_wiki_search(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiSearchParams,
) -> Result<CallToolResult, McpError> {
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;
    let client = read_client(&ctx)?;

    let results = run_on_dedicated_thread(move || async move {
        client
            .wiki_search(&params.scope, &params.id, &params.query)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    })?;

    success_json(serde_json::json!({ "results": results }))
}

// ── ta_wiki_get ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiGetParams {
    pub scope: String,
    pub id: String,
    pub page_id: String,
}

/// Cache-first, deliberately stale-tolerant: see this crate's
/// `wiki_cache` module doc and the design doc's own explicit
/// acknowledgment of this tradeoff.
pub fn handle_wiki_get(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiGetParams,
) -> Result<CallToolResult, McpError> {
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;

    let cache = WikiCache::new(&ctx.project_root);
    if let Some(cached) = cache
        .get(&params.scope, &params.id, &params.page_id)
        .map_err(|e| {
            McpError::internal_error(format!("ta_wiki_get: cache read failed: {e}"), None)
        })?
    {
        return success_json(page_to_json(&cached));
    }

    let client = read_client(&ctx)?;
    let (scope, id) = (params.scope.clone(), params.id.clone());
    let page = run_on_dedicated_thread(move || async move {
        client
            .wiki_get(&params.scope, &params.id, &params.page_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    })?;

    if let Err(e) = cache.put(&scope, &id, &page) {
        // A cache-write failure must never fail the read that already
        // succeeded: log and return the live result anyway. The next
        // call simply misses the cache again and re-fetches.
        tracing::warn!(error = %e, "ta_wiki_get: failed to write cache entry, continuing");
    }

    success_json(page_to_json(&page))
}

// ── ta_wiki_types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiTypesParams {
    pub scope: String,
    pub id: String,
}

pub fn handle_wiki_types(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiTypesParams,
) -> Result<CallToolResult, McpError> {
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;
    let client = read_client(&ctx)?;

    let types = run_on_dedicated_thread(move || async move {
        client
            .wiki_types(&params.scope, &params.id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    })?;

    success_json(serde_json::json!({ "types": types }))
}

// ── ta_wiki_create ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiCreateParams {
    pub scope: String,
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn handle_wiki_create(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiCreateParams,
) -> Result<CallToolResult, McpError> {
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;
    let client = write_client(&ctx)?;
    let (scope, id) = (params.scope.clone(), params.id.clone());

    let page = run_on_dedicated_thread(move || async move {
        let tags = if params.tags.is_empty() {
            None
        } else {
            Some(params.tags.as_slice())
        };
        client
            .wiki_create(
                &params.scope,
                &params.id,
                &params.title,
                &params.body,
                params.r#type.as_deref(),
                tags,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))
    })?;

    let cache = WikiCache::new(&ctx.project_root);
    if let Err(e) = cache.put(&scope, &id, &page) {
        tracing::warn!(error = %e, "ta_wiki_create: failed to write cache entry, continuing");
    }

    success_json(page_to_json(&page))
}

// ── ta_wiki_update ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiUpdateParams {
    pub scope: String,
    pub id: String,
    pub page_id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub if_sha: Option<String>,
}

pub fn handle_wiki_update(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiUpdateParams,
) -> Result<CallToolResult, McpError> {
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;
    let client = write_client(&ctx)?;

    let (scope, id) = (params.scope.clone(), params.id.clone());
    let page = run_on_dedicated_thread(move || async move {
        client
            .wiki_update(
                &params.scope,
                &params.id,
                &params.page_id,
                &params.title,
                &params.body,
                params.if_sha.as_deref(),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))
    })?;

    let cache = WikiCache::new(&ctx.project_root);
    if let Err(e) = cache.put(&scope, &id, &page) {
        tracing::warn!(error = %e, "ta_wiki_update: failed to write cache entry, continuing");
    }

    success_json(page_to_json(&page))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmcp::handler::server::router::tool::ToolRouter;
    use rmcp::handler::server::wrapper::Parameters as ServerParameters;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use rmcp::{tool, tool_handler, tool_router, ServerHandler};
    use tokio_util::sync::CancellationToken;

    use crate::GatewayConfig;

    // ── Pure-logic tests: no server, no credential vault needed ────────

    #[test]
    fn load_wiki_resources_config_errors_clearly_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_wiki_resources_config(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("no wiki configuration found"));
    }

    #[test]
    fn load_wiki_resources_config_errors_clearly_when_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/wiki-resources.toml"), "not = [valid").unwrap();
        let err = load_wiki_resources_config(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("malformed"));
    }

    #[test]
    fn load_wiki_resources_config_parses_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/wiki-resources.toml"),
            r#"
[[scopes]]
name = "project"
scope = "project"
id = "proj-1"

[wayfinder]
base_url = "http://127.0.0.1:1/mcp"
read_credential_name = "wayfinder-service-account"
write_credential_name = "wayfinder-wiki-writer"
"#,
        )
        .unwrap();
        let config = load_wiki_resources_config(dir.path()).unwrap();
        assert_eq!(config.scopes.len(), 1);
        assert_eq!(config.scopes[0].scope, "project");
        assert_eq!(config.scopes[0].id, "proj-1");
        assert_eq!(
            config.wayfinder.read_credential_name,
            "wayfinder-service-account"
        );
    }

    fn test_config(scopes: &[(&str, &str)]) -> WikiResourcesConfig {
        WikiResourcesConfig {
            scopes: scopes
                .iter()
                .map(|(scope, id)| ScopeConfig {
                    name: format!("{scope}-{id}"),
                    scope: scope.to_string(),
                    id: id.to_string(),
                })
                .collect(),
            wayfinder: WayfinderConfig {
                base_url: "http://127.0.0.1:1/mcp".to_string(),
                read_credential_name: "wayfinder-service-account".to_string(),
                write_credential_name: "wayfinder-wiki-writer".to_string(),
            },
        }
    }

    #[test]
    fn require_declared_scope_accepts_a_declared_pair() {
        let config = test_config(&[("project", "proj-1")]);
        assert!(require_declared_scope(&config, "project", "proj-1").is_ok());
    }

    #[test]
    fn require_declared_scope_rejects_an_undeclared_id_even_with_a_declared_scope() {
        let config = test_config(&[("project", "proj-1")]);
        let err = require_declared_scope(&config, "project", "proj-2").unwrap_err();
        assert!(format!("{err}").contains("not declared"));
    }

    #[test]
    fn require_declared_scope_rejects_an_undeclared_scope_kind() {
        // proj-1 is declared as "project", not "org" -- a caller passing
        // the same id under the wrong scope must not be silently accepted.
        let config = test_config(&[("project", "proj-1")]);
        let err = require_declared_scope(&config, "org", "proj-1").unwrap_err();
        assert!(format!("{err}").contains("not declared"));
    }

    // ── Credential resolution ───────────────────────────────────────────

    #[test]
    fn resolve_credential_secret_errors_clearly_when_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no credential named"));
        assert!(msg.contains("wayfinder-service-account"));
    }

    #[test]
    fn resolve_credential_secret_finds_a_stored_credential_by_name() {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-service-account", "wayfinder", "tok-abc", vec![])
            .unwrap();

        let secret =
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap();
        assert_eq!(secret, "tok-abc");
    }

    #[test]
    fn resolve_credential_secret_distinguishes_read_and_write_credentials() {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-service-account", "wayfinder", "read-tok", vec![])
            .unwrap();
        vault
            .add("wayfinder-wiki-writer", "wayfinder", "write-tok", vec![])
            .unwrap();

        assert_eq!(
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap(),
            "read-tok"
        );
        assert_eq!(
            resolve_credential_secret(dir.path(), false, "wayfinder-wiki-writer").unwrap(),
            "write-tok"
        );
    }

    // ── End-to-end: full handler through a real mock Wayfinder server ──

    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct MockGetParams {
        scope: String,
        id: String,
        page_id: String,
    }

    /// A minimal one-tool mock server -- `tools/wiki.rs`'s own orchestration
    /// (config load, credential resolution, cache-first behavior) is what
    /// these tests exercise; per-tool request/response correctness is
    /// already thoroughly covered by `wiki_client`'s own tests against a
    /// full 6-tool mock, so duplicating that breadth here isn't needed.
    #[derive(Debug, Clone)]
    struct MockWikiServer {
        tool_router: ToolRouter<Self>,
    }

    #[tool_router]
    impl MockWikiServer {
        fn new() -> Self {
            Self {
                tool_router: Self::tool_router(),
            }
        }

        #[tool(description = "get")]
        fn wiki_get(
            &self,
            ServerParameters(params): ServerParameters<MockGetParams>,
        ) -> Result<CallToolResult, McpError> {
            Ok(CallToolResult::success(vec![Content::json(
                serde_json::json!({
                    "id": params.page_id,
                    "title": "End-to-End Page",
                    "body": "Fetched through the real gateway tool handler.",
                    "type": "note",
                    "tags": [],
                    "sha": "sha-e2e",
                    "updated_at": "2026-09-15T00:00:00Z",
                }),
            )
            .unwrap()]))
        }
    }

    #[tool_handler]
    impl ServerHandler for MockWikiServer {}

    async fn spawn_mock_server() -> (String, CancellationToken) {
        let ct = CancellationToken::new();
        let service: StreamableHttpService<MockWikiServer, LocalSessionManager> =
            StreamableHttpService::new(
                || Ok(MockWikiServer::new()),
                Default::default(),
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    sse_keep_alive: None,
                    cancellation_token: ct.child_token(),
                    ..Default::default()
                },
            );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_ct = ct.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { server_ct.cancelled_owned().await })
                .await;
        });
        (format!("http://{addr}/mcp"), ct)
    }

    /// Sets up a full project: real credential vault (both credentials
    /// stored), real `.ta/wiki-resources.toml` pointing at a live mock
    /// server, and a real `GatewayState` -- everything `handle_wiki_get`
    /// actually touches, none of it mocked at the handler's own boundary.
    async fn test_project_with_mock_server() -> (
        Arc<Mutex<GatewayState>>,
        tempfile::TempDir,
        CancellationToken,
    ) {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();

        let (url, ct) = spawn_mock_server().await;

        std::fs::write(
            dir.path().join(".ta/wiki-resources.toml"),
            format!(
                r#"
[[scopes]]
name = "project"
scope = "project"
id = "proj-1"

[wayfinder]
base_url = "{url}"
read_credential_name = "wayfinder-service-account"
write_credential_name = "wayfinder-wiki-writer"
"#
            ),
        )
        .unwrap();

        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-service-account", "wayfinder", "read-tok", vec![])
            .unwrap();
        vault
            .add("wayfinder-wiki-writer", "wayfinder", "write-tok", vec![])
            .unwrap();

        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        let state = Arc::new(Mutex::new(GatewayState::new(config).unwrap()));

        (state, dir, ct)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_wiki_get_fetches_live_on_a_cache_miss_and_then_caches_it() {
        let (state, dir, _ct) = test_project_with_mock_server().await;

        let params = WikiGetParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            page_id: "page-1".to_string(),
        };
        let result = handle_wiki_get(&state, params).unwrap();
        let text = result.content[0].raw.as_text().unwrap().text.clone();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["title"], "End-to-End Page");
        assert_eq!(value["sha"], "sha-e2e");

        // The live call's result must now be on disk in the local cache.
        let cache = WikiCache::new(dir.path());
        let cached = cache.get("project", "proj-1", "page-1").unwrap();
        assert!(
            cached.is_some(),
            "expected the live fetch to populate the cache"
        );
        assert_eq!(cached.unwrap().sha, "sha-e2e");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_wiki_get_second_call_is_served_from_cache_without_hitting_the_server() {
        let (state, _dir, ct) = test_project_with_mock_server().await;

        let params = WikiGetParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            page_id: "page-1".to_string(),
        };
        handle_wiki_get(&state, params.clone()).unwrap();

        // Shut the mock server down entirely -- a second call must still
        // succeed, proving it was served from the cache, not the network.
        // `_dir` (the TempDir guard) stays alive for the whole function, so
        // the cache written by the first call is still on disk here.
        ct.cancel();

        let result = handle_wiki_get(&state, params).unwrap();
        let text = result.content[0].raw.as_text().unwrap().text.clone();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["title"], "End-to-End Page");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_wiki_search_rejects_an_undeclared_scope_before_ever_calling_out() {
        let (state, _dir, _ct) = test_project_with_mock_server().await;

        let params = WikiSearchParams {
            scope: "project".to_string(),
            id: "some-other-project".to_string(),
            query: "anything".to_string(),
        };
        let err = handle_wiki_search(&state, params).unwrap_err();
        assert!(format!("{err}").contains("not declared"));
    }
}
