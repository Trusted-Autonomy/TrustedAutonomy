//! `WikiMcpClient` -- a thin MCP client speaking Streamable HTTP to
//! Wayfinder's wiki MCP endpoint (`wayfinder-api`, per
//! `docs/superpowers/specs/2026-08-17-wiki-design.md` in the `wayfinder`
//! repo and its companion `ta-virtual-team` designs,
//! `2026-09-15-virtual-team-wiki-retrieval-design.md` /
//! `-ingestion-design.md`).
//!
//! One MCP session per call, not a persistent connection. Each `wiki_*`
//! method builds a fresh `StreamableHttpClientTransport`, connects,
//! issues exactly one `call_tool`, and lets the resulting `RunningService`
//! drop (its own `Drop` impl cancels the underlying transport). This is
//! simpler than session lifecycle/reconnection management for a
//! request/response tool-call pattern, at the cost of a fresh handshake
//! per call -- acceptable since `tools/wiki.rs`'s local cache already
//! minimizes how often this client is actually invoked.
//!
//! `Wayfinder`'s own spec places a "needs verification" caveat on whether
//! `rmcp` actually supports Streamable HTTP the way this needs on their
//! server side; the client side used here (`rmcp` 0.14, the `client` +
//! `transport-streamable-http-client-reqwest` features, already the
//! workspace-pinned version) is confirmed to support it.

use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ClientHandler, ServiceExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum WikiClientError {
    #[error("failed to connect to Wayfinder's wiki MCP endpoint at {url}: {source}")]
    Connect {
        url: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("Wayfinder wiki tool call '{tool}' failed: {source}")]
    ToolCall {
        tool: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("Wayfinder wiki tool '{tool}' returned an error result: {message}")]
    ToolError { tool: &'static str, message: String },
    #[error("Wayfinder wiki tool '{tool}' returned an unexpected/malformed result: {reason}")]
    MalformedResult { tool: &'static str, reason: String },
}

/// Minimal `ClientHandler` -- this client only ever calls tools, never
/// serves prompts/resources/sampling back to Wayfinder, so the default
/// `ClientInfo` (no declared capabilities beyond what `ServiceExt::serve`
/// requires) is sufficient.
#[derive(Debug, Clone, Default)]
struct WikiClientHandler;

impl ClientHandler for WikiClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiManifestEntry {
    pub page_id: String,
    pub sha: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiPage {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub sha: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiSearchResult {
    pub page_id: String,
    pub title: String,
    #[serde(default)]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiTypeEntry {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub struct WikiMcpClient {
    /// Full URL of Wayfinder's wiki MCP endpoint (Streamable HTTP), e.g.
    /// `https://wayfinder.example.com/mcp`.
    base_url: String,
    /// Bearer token -- either the general `wayfinder-service-account`
    /// credential (reads) or `wayfinder-wiki-writer` (writes). Which one
    /// to use is the caller's decision (`tools/wiki.rs`), not this
    /// client's -- it just sends whatever token it's constructed with.
    token: String,
}

impl WikiMcpClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token: token.into(),
        }
    }

    /// Connects, issues one `call_tool`, and returns its parsed JSON
    /// result. Prefers `structured_content` when present (the MCP-native
    /// way a tool returns typed data); falls back to parsing the first
    /// text content block as JSON otherwise, since not every server
    /// populates `structured_content` for every tool.
    async fn call_tool(
        &self,
        tool: &'static str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, WikiClientError> {
        let config = StreamableHttpClientTransportConfig::with_uri(self.base_url.clone())
            .auth_header(self.token.clone());
        let transport = StreamableHttpClientTransport::from_config(config);

        let client =
            WikiClientHandler
                .serve(transport)
                .await
                .map_err(|e| WikiClientError::Connect {
                    url: self.base_url.clone(),
                    source: anyhow::anyhow!(e),
                })?;

        let arguments_obj = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return Err(WikiClientError::ToolCall {
                    tool,
                    source: anyhow::anyhow!(
                        "internal error: arguments must serialize to a JSON object, got {other}"
                    ),
                })
            }
        };

        let result = client
            .call_tool(CallToolRequestParams {
                meta: None,
                name: tool.into(),
                arguments: arguments_obj,
                task: None,
            })
            .await
            .map_err(|e| WikiClientError::ToolCall {
                tool,
                source: anyhow::anyhow!(e),
            })?;

        if result.is_error.unwrap_or(false) {
            let message = result
                .content
                .iter()
                .filter_map(|c| c.raw.as_text())
                .map(|t| t.text.clone())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(WikiClientError::ToolError {
                tool,
                message: if message.is_empty() {
                    "(no error detail returned)".to_string()
                } else {
                    message
                },
            });
        }

        if let Some(structured) = result.structured_content {
            return Ok(structured);
        }

        let text = result
            .content
            .iter()
            .find_map(|c| c.raw.as_text())
            .map(|t| t.text.clone())
            .ok_or_else(|| WikiClientError::MalformedResult {
                tool,
                reason: "result had neither structured_content nor any text content".to_string(),
            })?;

        serde_json::from_str(&text).map_err(|e| WikiClientError::MalformedResult {
            tool,
            reason: format!("text content was not valid JSON: {e}"),
        })
    }

    pub async fn wiki_manifest(
        &self,
        scope: &str,
        id: &str,
    ) -> Result<Vec<WikiManifestEntry>, WikiClientError> {
        let value = self
            .call_tool(
                "wiki_manifest",
                serde_json::json!({ "scope": scope, "id": id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_manifest",
            reason: format!("expected an array of {{page_id, sha, updated_at}}: {e}"),
        })
    }

    pub async fn wiki_get(
        &self,
        scope: &str,
        id: &str,
        page_id: &str,
    ) -> Result<WikiPage, WikiClientError> {
        let value = self
            .call_tool(
                "wiki_get",
                serde_json::json!({ "scope": scope, "id": id, "page_id": page_id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_get",
            reason: format!("expected a wiki page document: {e}"),
        })
    }

    pub async fn wiki_search(
        &self,
        scope: &str,
        id: &str,
        query: &str,
    ) -> Result<Vec<WikiSearchResult>, WikiClientError> {
        let value = self
            .call_tool(
                "wiki_search",
                serde_json::json!({ "scope": scope, "id": id, "query": query }),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_search",
            reason: format!("expected an array of search results: {e}"),
        })
    }

    pub async fn wiki_types(
        &self,
        scope: &str,
        id: &str,
    ) -> Result<Vec<WikiTypeEntry>, WikiClientError> {
        let value = self
            .call_tool(
                "wiki_types",
                serde_json::json!({ "scope": scope, "id": id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_types",
            reason: format!("expected an array of {{key, label, description}}: {e}"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn wiki_create(
        &self,
        scope: &str,
        id: &str,
        title: &str,
        body: &str,
        r#type: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<WikiPage, WikiClientError> {
        let mut args = serde_json::json!({
            "scope": scope,
            "id": id,
            "title": title,
            "body": body,
            "source": "virtual_team",
        });
        if let Some(t) = r#type {
            args["type"] = serde_json::Value::String(t.to_string());
        }
        if let Some(tags) = tags {
            args["tags"] = serde_json::json!(tags);
        }
        let value = self.call_tool("wiki_create", args).await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_create",
            reason: format!("expected the created wiki page: {e}"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn wiki_update(
        &self,
        scope: &str,
        id: &str,
        page_id: &str,
        title: &str,
        body: &str,
        if_sha: Option<&str>,
    ) -> Result<WikiPage, WikiClientError> {
        let mut args = serde_json::json!({
            "scope": scope,
            "id": id,
            "page_id": page_id,
            "title": title,
            "body": body,
        });
        if let Some(sha) = if_sha {
            args["if_sha"] = serde_json::Value::String(sha.to_string());
        }
        let value = self.call_tool("wiki_update", args).await?;
        serde_json::from_value(value).map_err(|e| WikiClientError::MalformedResult {
            tool: "wiki_update",
            reason: format!("expected the updated wiki page: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    //! Tests run `WikiMcpClient` against a real, in-process Streamable
    //! HTTP MCP server implementing (a stand-in for) Wayfinder's wiki
    //! tool set -- proving the transport/client plumbing actually works,
    //! not just that it compiles against the right types. Mirrors this
    //! session's `mock-entitlement-server` pattern (`ta-virtual-team`
    //! repo), which caught two real bugs the same way plain unit tests
    //! wouldn't have.

    use rmcp::handler::server::router::tool::ToolRouter;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use rmcp::{tool, tool_handler, tool_router, ServerHandler};
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // `scope`/`id` are part of the real wire contract (needed for correct
    // deserialization and schema generation) even though this mock's
    // handler bodies don't branch on them -- a real Wayfinder handler
    // would. `#[allow(dead_code)]` rather than deleting the fields, since
    // removing them would silently stop validating that the client sends
    // them at all.
    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct ScopeIdParams {
        scope: String,
        id: String,
    }

    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct GetParams {
        scope: String,
        id: String,
        page_id: String,
    }

    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct SearchParams {
        scope: String,
        id: String,
        query: String,
    }

    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct CreateParams {
        scope: String,
        id: String,
        title: String,
        body: String,
        #[serde(default)]
        r#type: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
    }

    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct UpdateParams {
        scope: String,
        id: String,
        page_id: String,
        title: String,
        body: String,
        #[serde(default)]
        if_sha: Option<String>,
    }

    fn json_result(value: serde_json::Value) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::json(value)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    /// A minimal stand-in for Wayfinder's wiki MCP server -- enough
    /// behavior to prove the client's request/response handling for
    /// every one of its own methods, plus one deliberately-erroring tool
    /// to prove the client's error path.
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

        #[tool(description = "manifest")]
        fn wiki_manifest(
            &self,
            Parameters(params): Parameters<ScopeIdParams>,
        ) -> Result<CallToolResult, McpError> {
            json_result(serde_json::json!([
                { "page_id": format!("{}-page-1", params.id), "sha": "sha-1", "updated_at": "2026-09-15T00:00:00Z" },
            ]))
        }

        #[tool(description = "get")]
        fn wiki_get(
            &self,
            Parameters(params): Parameters<GetParams>,
        ) -> Result<CallToolResult, McpError> {
            if params.page_id == "does-not-exist" {
                return Ok(CallToolResult::error(vec![Content::text(
                    "no such page".to_string(),
                )]));
            }
            json_result(serde_json::json!({
                "id": params.page_id,
                "title": "A Mock Page",
                "body": "Mock body content.",
                "type": "note",
                "tags": ["mock"],
                "sha": "sha-1",
                "updated_at": "2026-09-15T00:00:00Z",
            }))
        }

        #[tool(description = "search")]
        fn wiki_search(
            &self,
            Parameters(params): Parameters<SearchParams>,
        ) -> Result<CallToolResult, McpError> {
            json_result(serde_json::json!([
                { "page_id": "page-1", "title": "A Mock Page", "snippet": format!("...matches '{}'...", params.query) },
            ]))
        }

        #[tool(description = "types")]
        fn wiki_types(
            &self,
            Parameters(_params): Parameters<ScopeIdParams>,
        ) -> Result<CallToolResult, McpError> {
            json_result(serde_json::json!([
                { "key": "note", "label": "Note", "description": "A general note." },
            ]))
        }

        #[tool(description = "create")]
        fn wiki_create(
            &self,
            Parameters(params): Parameters<CreateParams>,
        ) -> Result<CallToolResult, McpError> {
            json_result(serde_json::json!({
                "id": "new-page-1",
                "title": params.title,
                "body": params.body,
                "type": params.r#type,
                "tags": params.tags,
                "sha": "sha-new",
                "updated_at": "2026-09-15T00:00:00Z",
            }))
        }

        #[tool(description = "update")]
        fn wiki_update(
            &self,
            Parameters(params): Parameters<UpdateParams>,
        ) -> Result<CallToolResult, McpError> {
            if params.if_sha.as_deref() == Some("stale-sha") {
                return Ok(CallToolResult::error(vec![Content::text(
                    "sha mismatch (409)".to_string(),
                )]));
            }
            json_result(serde_json::json!({
                "id": params.page_id,
                "title": params.title,
                "body": params.body,
                "type": serde_json::Value::Null,
                "tags": Vec::<String>::new(),
                "sha": "sha-updated",
                "updated_at": "2026-09-15T00:00:00Z",
            }))
        }
    }

    #[tool_handler]
    impl ServerHandler for MockWikiServer {}

    /// Starts the mock server on an ephemeral local port and returns the
    /// MCP endpoint URL plus a guard that shuts it down on drop.
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

    #[tokio::test]
    async fn wiki_manifest_round_trips_through_a_real_streamable_http_server() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let entries = client.wiki_manifest("project", "proj-1").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page_id, "proj-1-page-1");
        assert_eq!(entries[0].sha, "sha-1");
    }

    #[tokio::test]
    async fn wiki_get_round_trips_a_full_page() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let page = client
            .wiki_get("project", "proj-1", "page-1")
            .await
            .unwrap();
        assert_eq!(page.title, "A Mock Page");
        assert_eq!(page.body, "Mock body content.");
        assert_eq!(page.r#type.as_deref(), Some("note"));
        assert_eq!(page.tags, vec!["mock".to_string()]);
    }

    #[tokio::test]
    async fn wiki_get_on_a_tool_level_error_surfaces_as_tool_error() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let err = client
            .wiki_get("project", "proj-1", "does-not-exist")
            .await
            .unwrap_err();
        match err {
            WikiClientError::ToolError { tool, message } => {
                assert_eq!(tool, "wiki_get");
                assert!(message.contains("no such page"));
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wiki_search_returns_ranked_results() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let results = client
            .wiki_search("org", "acme", "onboarding")
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("onboarding"));
    }

    #[tokio::test]
    async fn wiki_types_returns_the_taxonomy() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let types = client.wiki_types("project", "proj-1").await.unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].key, "note");
    }

    #[tokio::test]
    async fn wiki_create_sends_type_and_tags_and_gets_back_the_created_page() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let page = client
            .wiki_create(
                "project",
                "proj-1",
                "New Decision",
                "We decided X.",
                Some("decision"),
                Some(&["architecture".to_string()]),
            )
            .await
            .unwrap();

        assert_eq!(page.id, "new-page-1");
        assert_eq!(page.title, "New Decision");
        assert_eq!(page.r#type.as_deref(), Some("decision"));
        assert_eq!(page.tags, vec!["architecture".to_string()]);
    }

    #[tokio::test]
    async fn wiki_update_with_a_stale_if_sha_surfaces_as_tool_error() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let err = client
            .wiki_update(
                "project",
                "proj-1",
                "page-1",
                "Title",
                "Body",
                Some("stale-sha"),
            )
            .await
            .unwrap_err();
        match err {
            WikiClientError::ToolError { message, .. } => {
                assert!(message.contains("409"));
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wiki_update_without_if_sha_succeeds() {
        let (url, _ct) = spawn_mock_server().await;
        let client = WikiMcpClient::new(url, "test-token");

        let page = client
            .wiki_update("project", "proj-1", "page-1", "New Title", "New body", None)
            .await
            .unwrap();
        assert_eq!(page.title, "New Title");
        assert_eq!(page.sha, "sha-updated");
    }

    #[tokio::test]
    async fn connecting_to_a_nonexistent_server_is_a_clean_connect_error() {
        // Nothing listening on this port -- proves a dead endpoint fails
        // cleanly rather than hanging or panicking.
        let client = WikiMcpClient::new("http://127.0.0.1:1/mcp", "test-token");
        let err = client.wiki_manifest("project", "proj-1").await.unwrap_err();
        assert!(matches!(err, WikiClientError::Connect { .. }));
    }
}
