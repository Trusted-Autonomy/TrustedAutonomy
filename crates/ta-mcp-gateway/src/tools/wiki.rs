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
//! ## Writes are reviewed, never live
//!
//! `ta_wiki_create` and `ta_wiki_update` validate their input and capture a
//! `PendingAction` in the goal's draft; they make no Wayfinder call and
//! resolve no write credential. The write-rank credential is used only
//! when the approved draft is applied (see [`apply_wiki_action`]).
//!
//! ## Config source
//!
//! `.ta/wiki-resources.toml` in the real project root (not a goal's
//! staging copy, same reasoning `tools/whiteboard.rs` documents for its
//! own daemon client). A project with no such file has no wiki access:
//! every tool here fails with a clear, actionable error, never a silent
//! no-op.

use chrono::Utc;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use ta_changeset::draft_package::{ActionKind, ArtifactDisposition, PendingAction};

use crate::server::GatewayState;
use crate::wiki_cache::WikiCache;
use crate::wiki_client::{WikiMcpClient, WikiPage};
use crate::wiki_resources::{self, WikiResourcesConfig};

use super::sync_bridge::run_on_dedicated_thread;

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
    let config = wiki_resources::load_wiki_resources_config(&project_root)
        .map_err(|e| McpError::invalid_request(format!("ta_wiki_*: {e}"), None))?;
    Ok(WikiContext {
        project_root,
        config,
        use_keychain,
    })
}

/// Refuses a wiki READ when `[wiki] seed_context = false` in
/// `.ta/workflow.toml`. Called first thing by every read handler so the
/// refusal is enforced in the gateway, not just in a prompt. Writes never
/// call this: they stay reviewed and keep working. A malformed setting is
/// also a refusal (fail closed), with the error naming file and key.
pub fn require_wiki_reads_enabled(
    state: &Arc<Mutex<GatewayState>>,
    tool: &str,
) -> Result<(), McpError> {
    let project_root = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?
        .config
        .workspace_root
        .clone();
    let path = project_root.join(".ta").join("workflow.toml");
    match ta_submit::WorkflowConfig::wiki_seed_context(&path) {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::warn!(tool, path = %path.display(), "wiki read refused: [wiki] seed_context = false");
            Err(McpError::invalid_request(
                format!(
                    "{tool}: wiki reads are turned off. `[wiki] seed_context = false` in {}. \
                     Agents may not read the wiki while this is set (wiki writes still go \
                     through draft review). To turn reads back on, set `seed_context = true` \
                     under [wiki] in that file or remove the key.",
                    path.display()
                ),
                None,
            ))
        }
        Err(e) => {
            tracing::warn!(tool, error = %e, "wiki read refused: invalid [wiki] config");
            Err(McpError::invalid_request(format!("{tool}: {e}"), None))
        }
    }
}

fn read_client(ctx: &WikiContext) -> Result<WikiMcpClient, McpError> {
    wiki_resources::read_client(&ctx.project_root, ctx.use_keychain, &ctx.config)
        .map_err(|e| McpError::internal_error(format!("ta_wiki_*: {e}"), None))
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
    require_wiki_reads_enabled(state, "ta_wiki_search")?;
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
    require_wiki_reads_enabled(state, "ta_wiki_get")?;
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
    require_wiki_reads_enabled(state, "ta_wiki_types")?;
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

// ── Writes: validated, then captured for draft review ─────────────────────
//
// `ta_wiki_create` / `ta_wiki_update` never call Wayfinder. They validate
// the input and append a `StateChanging` `PendingAction` to the goal's
// pending actions, exactly like `ta_propose_task_*` (`tools/wayfinder_task.rs`).
// The write happens only when the draft holding the action is approved and
// applied: `apply_wiki_action` below is called from
// `apps/ta-cli/src/commands/draft_task_replay.rs`, the same replay step
// (and the same ledger) the task proposals use. The write-rank credential
// is resolved there, at apply time, and never at capture time.

/// Tool name carried by a captured `ta_wiki_create` action.
pub const CREATE_TOOL_NAME: &str = "ta_wiki_create";
/// Tool name carried by a captured `ta_wiki_update` action.
pub const UPDATE_TOOL_NAME: &str = "ta_wiki_update";

/// Whether `tool_name` is one of the two captured wiki write tools.
pub fn is_wiki_write_tool(tool_name: &str) -> bool {
    tool_name == CREATE_TOOL_NAME || tool_name == UPDATE_TOOL_NAME
}

/// Maximum title length, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Maximum body size, in bytes.
pub const MAX_BODY_BYTES: usize = 100_000;
/// Maximum number of tags on one page.
pub const MAX_TAGS: usize = 20;
/// Maximum length of one tag, in characters.
pub const MAX_TAG_CHARS: usize = 64;
/// Maximum length of the page type, in characters.
pub const MAX_TYPE_CHARS: usize = 64;
/// Maximum length of a page id or `if_sha`, in characters.
pub const MAX_PAGE_ID_CHARS: usize = 128;
/// Characters of body shown in `ta draft view`.
pub const BODY_PREVIEW_CHARS: usize = 200;

fn invalid(tool: &str, msg: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(format!("{tool}: {msg}"), None)
}

/// Bidirectional-override and zero-width characters: U+200B..U+200F,
/// U+202A..U+202E, U+2066..U+2069 and U+FEFF.
pub fn is_bidi_or_zero_width(c: char) -> bool {
    matches!(
        c as u32,
        0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF
    )
}

/// Single-line field (title, tag, type, ids): no control characters of any
/// kind and no bidi or zero-width characters.
fn check_single_line(tool: &str, field: &str, value: &str) -> Result<(), McpError> {
    if let Some(c) = value
        .chars()
        .find(|c| c.is_control() || is_bidi_or_zero_width(*c))
    {
        return Err(invalid(
            tool,
            format!(
                "{field} contains a disallowed control, bidi or zero-width character \
                 (U+{:04X}); remove it and resubmit",
                c as u32
            ),
        ));
    }
    Ok(())
}

fn check_title(tool: &str, title: &str) -> Result<(), McpError> {
    if title.trim().is_empty() {
        return Err(invalid(tool, "title must be non-empty"));
    }
    let n = title.chars().count();
    if n > MAX_TITLE_CHARS {
        return Err(invalid(
            tool,
            format!("title is {n} characters, over the limit of {MAX_TITLE_CHARS}"),
        ));
    }
    check_single_line(tool, "title", title)
}

/// Body: normal newlines and tabs are fine; NUL and every other control
/// character, and bidi or zero-width characters, are rejected.
fn check_body(tool: &str, body: &str) -> Result<(), McpError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(invalid(
            tool,
            format!(
                "body is {} bytes, over the limit of {MAX_BODY_BYTES}",
                body.len()
            ),
        ));
    }
    if let Some(c) = body
        .chars()
        .find(|c| is_bidi_or_zero_width(*c) || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(invalid(
            tool,
            format!(
                "body contains a disallowed character (U+{:04X}): NUL, other control, bidi and \
                 zero-width characters are rejected (newlines and tabs are fine)",
                c as u32
            ),
        ));
    }
    Ok(())
}

fn check_type(tool: &str, r#type: Option<&str>) -> Result<(), McpError> {
    let Some(t) = r#type else { return Ok(()) };
    let n = t.chars().count();
    if n > MAX_TYPE_CHARS {
        return Err(invalid(
            tool,
            format!("type is {n} characters, over the limit of {MAX_TYPE_CHARS}"),
        ));
    }
    check_single_line(tool, "type", t)
}

fn check_tags(tool: &str, tags: &[String]) -> Result<(), McpError> {
    if tags.len() > MAX_TAGS {
        return Err(invalid(
            tool,
            format!("{} tags given, over the limit of {MAX_TAGS}", tags.len()),
        ));
    }
    for (i, tag) in tags.iter().enumerate() {
        let field = format!("tags[{i}]");
        if tag.trim().is_empty() {
            return Err(invalid(tool, format!("{field} must be non-empty")));
        }
        let n = tag.chars().count();
        if n > MAX_TAG_CHARS {
            return Err(invalid(
                tool,
                format!("{field} is {n} characters, over the limit of {MAX_TAG_CHARS}"),
            ));
        }
        check_single_line(tool, &field, tag)?;
    }
    Ok(())
}

fn check_id_like(tool: &str, field: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() {
        return Err(invalid(tool, format!("{field} must be non-empty")));
    }
    let n = value.chars().count();
    if n > MAX_PAGE_ID_CHARS {
        return Err(invalid(
            tool,
            format!("{field} is {n} characters, over the limit of {MAX_PAGE_ID_CHARS}"),
        ));
    }
    check_single_line(tool, field, value)
}

/// Validates a `ta_wiki_create` request. Pure; no I/O.
pub fn validate_create(params: &WikiCreateParams) -> Result<(), McpError> {
    let tool = CREATE_TOOL_NAME;
    check_title(tool, &params.title)?;
    check_body(tool, &params.body)?;
    check_type(tool, params.r#type.as_deref())?;
    check_tags(tool, &params.tags)
}

/// Validates a `ta_wiki_update` request. Pure; no I/O.
pub fn validate_update(params: &WikiUpdateParams) -> Result<(), McpError> {
    let tool = UPDATE_TOOL_NAME;
    check_id_like(tool, "page_id", &params.page_id)?;
    if let Some(sha) = &params.if_sha {
        check_id_like(tool, "if_sha", sha)?;
    }
    check_title(tool, &params.title)?;
    check_body(tool, &params.body)
}

/// Captures one wiki write as a pending action. Idempotent: an identical
/// action already pending for the goal is returned rather than duplicated.
fn capture_write(
    state: &Arc<Mutex<GatewayState>>,
    tool_name: &str,
    goal_run_id: &str,
    parameters: serde_json::Value,
    description: String,
    target_uri: String,
) -> Result<CallToolResult, McpError> {
    let goal_id = Uuid::parse_str(goal_run_id)
        .map_err(|e| invalid(tool_name, format!("goal_run_id is not a valid UUID: {e}")))?;

    let (action_id, duplicate) = {
        let mut locked = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {e}"), None))?;
        let actions = locked.pending_actions.entry(goal_id).or_default();
        if let Some(existing) = actions
            .iter()
            .find(|a| a.tool_name == tool_name && a.parameters == parameters)
        {
            (existing.action_id, true)
        } else {
            let action_id = Uuid::new_v4();
            actions.push(PendingAction {
                action_id,
                tool_name: tool_name.to_string(),
                parameters,
                kind: ActionKind::StateChanging,
                intercepted_at: Utc::now(),
                description,
                target_uri: Some(target_uri),
                disposition: ArtifactDisposition::Pending,
            });
            (action_id, false)
        }
    };

    let note = "Not written. This wiki change is queued for review in `ta draft view` and is \
                sent to Wayfinder only if this goal's draft is approved and applied.";
    success_json(serde_json::json!({
        "status": if duplicate { "already_captured_for_review" } else { "captured_for_review" },
        "tool": tool_name,
        "action_id": action_id.to_string(),
        "note": note,
    }))
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
    /// The UUID of the goal run whose draft carries this write for review.
    pub goal_run_id: String,
}

pub fn handle_wiki_create(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiCreateParams,
) -> Result<CallToolResult, McpError> {
    validate_create(&params)?;
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;

    let description = format!(
        "Propose creating {} wiki page \"{}\" in {}",
        params.scope, params.title, params.id
    );
    let target_uri = format!(
        "wayfinder://wiki/{}/{}/pages/new/{}",
        params.scope,
        params.id,
        params.title.trim()
    );
    let parameters = serde_json::json!({
        "scope": params.scope,
        "id": params.id,
        "title": params.title,
        "body": params.body,
        "type": params.r#type,
        "tags": params.tags,
    });
    capture_write(
        state,
        CREATE_TOOL_NAME,
        &params.goal_run_id,
        parameters,
        description,
        target_uri,
    )
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
    /// The UUID of the goal run whose draft carries this write for review.
    pub goal_run_id: String,
}

pub fn handle_wiki_update(
    state: &Arc<Mutex<GatewayState>>,
    params: WikiUpdateParams,
) -> Result<CallToolResult, McpError> {
    validate_update(&params)?;
    let ctx = load_context(state)?;
    require_declared_scope(&ctx.config, &params.scope, &params.id)?;

    let description = format!(
        "Propose updating {} wiki page {} in {}: title -> \"{}\"",
        params.scope, params.page_id, params.id, params.title
    );
    let target_uri = format!(
        "wayfinder://wiki/{}/{}/pages/{}",
        params.scope, params.id, params.page_id
    );
    let parameters = serde_json::json!({
        "scope": params.scope,
        "id": params.id,
        "page_id": params.page_id,
        "title": params.title,
        "body": params.body,
        "if_sha": params.if_sha,
    });
    capture_write(
        state,
        UPDATE_TOOL_NAME,
        &params.goal_run_id,
        parameters,
        description,
        target_uri,
    )
}

// ── Apply-time write and review rendering ─────────────────────────────────

fn param_str<'a>(p: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(|v| v.as_str())
}

fn required_param<'a>(p: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    param_str(p, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("pending action is missing required parameter `{key}`"))
}

/// Performs one approved wiki write against Wayfinder with the write-rank
/// credential. Called only at draft apply time. Input is re-validated, since
/// the draft package is a file a person could have edited. Returns a
/// one-line summary.
pub fn apply_wiki_action(
    project_root: &std::path::Path,
    use_keychain: bool,
    tool_name: &str,
    p: &serde_json::Value,
) -> anyhow::Result<String> {
    let config = wiki_resources::load_wiki_resources_config(project_root)?;
    let scope = required_param(p, "scope")?.to_string();
    let id = required_param(p, "id")?.to_string();
    require_declared_scope(&config, &scope, &id).map_err(|e| anyhow::anyhow!("{}", e.message))?;
    let title = required_param(p, "title")?.to_string();
    let body = param_str(p, "body").unwrap_or("").to_string();

    let page = match tool_name {
        CREATE_TOOL_NAME => {
            let r#type = param_str(p, "type").map(str::to_string);
            let tags: Vec<String> = p
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            validate_create(&WikiCreateParams {
                scope: scope.clone(),
                id: id.clone(),
                title: title.clone(),
                body: body.clone(),
                r#type: r#type.clone(),
                tags: tags.clone(),
                goal_run_id: String::new(),
            })
            .map_err(|e| anyhow::anyhow!("{}", e.message))?;
            let client = wiki_resources::write_client(project_root, use_keychain, &config)?;
            run_blocking(move || async move {
                let tags_ref = if tags.is_empty() {
                    None
                } else {
                    Some(tags.as_slice())
                };
                client
                    .wiki_create(&scope, &id, &title, &body, r#type.as_deref(), tags_ref)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            })?
        }
        UPDATE_TOOL_NAME => {
            let page_id = required_param(p, "page_id")?.to_string();
            let if_sha = param_str(p, "if_sha").map(str::to_string);
            validate_update(&WikiUpdateParams {
                scope: scope.clone(),
                id: id.clone(),
                page_id: page_id.clone(),
                title: title.clone(),
                body: body.clone(),
                if_sha: if_sha.clone(),
                goal_run_id: String::new(),
            })
            .map_err(|e| anyhow::anyhow!("{}", e.message))?;
            let client = wiki_resources::write_client(project_root, use_keychain, &config)?;
            run_blocking(move || async move {
                client
                    .wiki_update(&scope, &id, &page_id, &title, &body, if_sha.as_deref())
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            })?
        }
        other => anyhow::bail!("`{other}` is not a wiki write tool"),
    };

    let scope = required_param(p, "scope")?;
    let id = required_param(p, "id")?;
    if let Err(e) = WikiCache::new(project_root).put(scope, id, &page) {
        tracing::warn!(error = %e, tool_name, "wiki write applied but cache update failed, continuing");
    }
    Ok(format!(
        "wiki page {} {} ({scope} {id})",
        page.id,
        if tool_name == CREATE_TOOL_NAME {
            "created"
        } else {
            "updated"
        }
    ))
}

fn run_blocking<F, Fut, T>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    run_on_dedicated_thread(f).map_err(|e| anyhow::anyhow!("{}", e.message))
}

/// Lines `ta draft view` prints under a pending wiki write: scope, org or
/// project id, page id (update), title, type, tags, body length and the
/// first [`BODY_PREVIEW_CHARS`] characters of the body.
pub fn review_lines(tool_name: &str, p: &serde_json::Value) -> Vec<String> {
    let get = |k: &str| param_str(p, k).unwrap_or("(none)");
    let mut lines = vec![format!(
        "Wiki {}: scope={} id={}",
        if tool_name == CREATE_TOOL_NAME {
            "create"
        } else {
            "update"
        },
        get("scope"),
        get("id")
    )];
    if tool_name == UPDATE_TOOL_NAME {
        lines.push(format!("Page id: {}", get("page_id")));
        lines.push(format!("If-sha: {}", get("if_sha")));
    }
    lines.push(format!("Title: {}", get("title")));
    if tool_name == CREATE_TOOL_NAME {
        lines.push(format!("Type: {}", get("type")));
        let tags = p
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "(none)".to_string());
        lines.push(format!("Tags: {tags}"));
    }
    let body = param_str(p, "body").unwrap_or("");
    let preview: String = body
        .chars()
        .take(BODY_PREVIEW_CHARS)
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let ellipsis = if body.chars().count() > BODY_PREVIEW_CHARS {
        "..."
    } else {
        ""
    };
    lines.push(format!("Body: {} bytes", body.len()));
    lines.push(format!("Preview: {preview}{ellipsis}"));
    lines
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
    //
    // Config parsing and credential resolution now live in
    // `wiki_resources.rs` and are tested there; this module only tests
    // logic that's still local to it (`require_declared_scope`).

    fn test_config(scopes: &[(&str, &str)]) -> WikiResourcesConfig {
        WikiResourcesConfig {
            scopes: scopes
                .iter()
                .map(|(scope, id)| wiki_resources::ScopeConfig {
                    name: format!("{scope}-{id}"),
                    scope: scope.to_string(),
                    id: id.to_string(),
                })
                .collect(),
            wayfinder: wiki_resources::WayfinderConfig {
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
    #[derive(Debug, Clone, Deserialize, JsonSchema)]
    struct MockWriteParams {
        scope: String,
        id: String,
        #[serde(default)]
        page_id: Option<String>,
        title: String,
        body: String,
        #[serde(default)]
        r#type: Option<String>,
        #[serde(default)]
        tags: Option<Vec<String>>,
        #[serde(default)]
        if_sha: Option<String>,
    }

    /// What the mock saw, so tests can assert on writes and credentials.
    #[derive(Debug, Clone, Default)]
    struct MockStats {
        /// Every HTTP request of any kind (MCP handshake included).
        http_requests: Arc<std::sync::atomic::AtomicUsize>,
        /// The `Authorization` header of every HTTP request.
        auths: Arc<Mutex<Vec<String>>>,
        /// (tool, arguments) of each write tool call that reached the mock.
        writes: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    #[derive(Debug, Clone)]
    struct MockWikiServer {
        tool_router: ToolRouter<Self>,
        stats: MockStats,
    }

    impl MockWikiServer {
        fn record(&self, tool: &str, p: &MockWriteParams) {
            self.stats.writes.lock().unwrap().push((
                tool.to_string(),
                serde_json::json!({
                    "scope": p.scope, "id": p.id, "page_id": p.page_id, "title": p.title,
                    "body": p.body, "type": p.r#type, "tags": p.tags, "if_sha": p.if_sha,
                }),
            ));
        }

        fn page(p: &MockWriteParams, id: &str) -> CallToolResult {
            CallToolResult::success(vec![Content::json(serde_json::json!({
                "id": id,
                "title": p.title,
                "body": p.body,
                "type": p.r#type.clone().unwrap_or_else(|| "note".to_string()),
                "tags": p.tags.clone().unwrap_or_default(),
                "sha": "sha-written",
                "updated_at": "2026-09-15T00:00:00Z",
            }))
            .unwrap()])
        }
    }

    #[tool_router]
    impl MockWikiServer {
        fn new(stats: MockStats) -> Self {
            Self {
                tool_router: Self::tool_router(),
                stats,
            }
        }

        #[tool(description = "create")]
        fn wiki_create(
            &self,
            ServerParameters(params): ServerParameters<MockWriteParams>,
        ) -> Result<CallToolResult, McpError> {
            self.record("wiki_create", &params);
            Ok(Self::page(&params, "page-new"))
        }

        #[tool(description = "update")]
        fn wiki_update(
            &self,
            ServerParameters(params): ServerParameters<MockWriteParams>,
        ) -> Result<CallToolResult, McpError> {
            self.record("wiki_update", &params);
            let id = params.page_id.clone().unwrap_or_default();
            Ok(Self::page(&params, &id))
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

    async fn spawn_mock_server() -> (String, CancellationToken, MockStats) {
        let ct = CancellationToken::new();
        let stats = MockStats::default();
        let service_stats = stats.clone();
        let service: StreamableHttpService<MockWikiServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(MockWikiServer::new(service_stats.clone())),
                Default::default(),
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    sse_keep_alive: None,
                    cancellation_token: ct.child_token(),
                    ..Default::default()
                },
            );
        let counter = stats.clone();
        let router =
            axum::Router::new()
                .nest_service("/mcp", service)
                .layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        let counter = counter.clone();
                        async move {
                            counter
                                .http_requests
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let auth = req
                                .headers()
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            counter.auths.lock().unwrap().push(auth);
                            next.run(req).await
                        }
                    },
                ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_ct = ct.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { server_ct.cancelled_owned().await })
                .await;
        });
        (format!("http://{addr}/mcp"), ct, stats)
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
        let (state, dir, ct, _stats) = test_project_with_stats().await;
        (state, dir, ct)
    }

    async fn test_project_with_stats() -> (
        Arc<Mutex<GatewayState>>,
        tempfile::TempDir,
        CancellationToken,
        MockStats,
    ) {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();

        let (url, ct, stats) = spawn_mock_server().await;

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

        (state, dir, ct, stats)
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

    // ── Validation: one test per limit and per forbidden character class ──

    fn create_params(title: &str, body: &str) -> WikiCreateParams {
        WikiCreateParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            title: title.to_string(),
            body: body.to_string(),
            r#type: Some("note".to_string()),
            tags: vec!["alpha".to_string(), "beta".to_string()],
            goal_run_id: Uuid::new_v4().to_string(),
        }
    }

    fn update_params(title: &str, body: &str) -> WikiUpdateParams {
        WikiUpdateParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            page_id: "page-1".to_string(),
            title: title.to_string(),
            body: body.to_string(),
            if_sha: Some("sha-1".to_string()),
            goal_run_id: Uuid::new_v4().to_string(),
        }
    }

    fn err_text(r: Result<(), McpError>) -> String {
        r.expect_err("expected a validation error")
            .message
            .to_string()
    }

    #[test]
    fn validation_accepts_a_normal_page() {
        let mut p = create_params("Release checklist", "Line one\n\tindented line two\r\n");
        assert!(validate_create(&p).is_ok());
        p.title = "t".repeat(MAX_TITLE_CHARS);
        p.body = "b".repeat(MAX_BODY_BYTES);
        p.tags = (0..MAX_TAGS).map(|_| "t".repeat(MAX_TAG_CHARS)).collect();
        p.r#type = Some("y".repeat(MAX_TYPE_CHARS));
        assert!(
            validate_create(&p).is_ok(),
            "exactly-at-limit input is valid"
        );
        assert!(validate_update(&update_params("Title", "Body\nmore")).is_ok());
    }

    #[test]
    fn validation_rejects_an_empty_title() {
        let e = err_text(validate_create(&create_params("   ", "body")));
        assert!(e.contains("title") && e.contains("non-empty"), "{e}");
        assert!(validate_update(&update_params("", "body")).is_err());
    }

    #[test]
    fn validation_rejects_a_title_over_the_limit() {
        let e = err_text(validate_create(&create_params(
            &"t".repeat(MAX_TITLE_CHARS + 1),
            "body",
        )));
        assert!(e.contains("title") && e.contains("200"), "{e}");
    }

    #[test]
    fn validation_rejects_a_body_over_the_limit() {
        let e = err_text(validate_create(&create_params(
            "T",
            &"b".repeat(MAX_BODY_BYTES + 1),
        )));
        assert!(e.contains("body") && e.contains("100000"), "{e}");
        assert!(validate_update(&update_params("T", &"b".repeat(MAX_BODY_BYTES + 1))).is_err());
    }

    #[test]
    fn validation_counts_body_limit_in_bytes_not_chars() {
        // 40_000 three-byte characters is 120_000 bytes but only 40_000 chars.
        let e = err_text(validate_create(&create_params(
            "T",
            &"\u{20AC}".repeat(40_000),
        )));
        assert!(e.contains("body"), "{e}");
    }

    #[test]
    fn validation_rejects_too_many_tags() {
        let mut p = create_params("T", "b");
        p.tags = (0..=MAX_TAGS).map(|i| format!("t{i}")).collect();
        let e = err_text(validate_create(&p));
        assert!(e.contains("tags") && e.contains("20"), "{e}");
    }

    #[test]
    fn validation_rejects_a_tag_over_the_limit() {
        let mut p = create_params("T", "b");
        p.tags = vec!["t".repeat(MAX_TAG_CHARS + 1)];
        let e = err_text(validate_create(&p));
        assert!(e.contains("tags[0]") && e.contains("64"), "{e}");
    }

    #[test]
    fn validation_rejects_a_type_over_the_limit() {
        let mut p = create_params("T", "b");
        p.r#type = Some("y".repeat(MAX_TYPE_CHARS + 1));
        let e = err_text(validate_create(&p));
        assert!(e.contains("type") && e.contains("64"), "{e}");
    }

    #[test]
    fn validation_rejects_control_characters_in_the_title() {
        let e = err_text(validate_create(&create_params("Bad\u{1b}[31mTitle", "b")));
        assert!(e.contains("title") && e.contains("U+001B"), "{e}");
        assert!(validate_create(&create_params("Line\nbreak", "b")).is_err());
    }

    #[test]
    fn validation_rejects_bidi_characters_in_the_title() {
        for c in [
            '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{200E}', '\u{200F}',
        ] {
            let e = err_text(validate_create(&create_params(&format!("a{c}b"), "b")));
            assert!(e.contains("title"), "U+{:04X}: {e}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_zero_width_characters_in_the_title() {
        for c in ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'] {
            let e = err_text(validate_create(&create_params(&format!("a{c}b"), "b")));
            assert!(e.contains("title"), "U+{:04X}: {e}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_control_characters_in_a_tag() {
        let mut p = create_params("T", "b");
        p.tags = vec!["ok".to_string(), "bad\ttag".to_string()];
        let e = err_text(validate_create(&p));
        assert!(e.contains("tags[1]"), "{e}");
    }

    #[test]
    fn validation_rejects_bidi_and_zero_width_characters_in_a_tag() {
        for c in ['\u{202E}', '\u{200B}', '\u{FEFF}'] {
            let mut p = create_params("T", "b");
            p.tags = vec![format!("a{c}b")];
            assert!(validate_create(&p).is_err(), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_control_characters_in_the_type() {
        let mut p = create_params("T", "b");
        p.r#type = Some("no\u{0}te".to_string());
        let e = err_text(validate_create(&p));
        assert!(e.contains("type"), "{e}");
    }

    #[test]
    fn validation_rejects_bidi_and_zero_width_characters_in_the_type() {
        for c in ['\u{202D}', '\u{200B}'] {
            let mut p = create_params("T", "b");
            p.r#type = Some(format!("a{c}b"));
            assert!(validate_create(&p).is_err(), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_nul_in_the_body() {
        let e = err_text(validate_create(&create_params("T", "a\0b")));
        assert!(e.contains("body") && e.contains("U+0000"), "{e}");
    }

    #[test]
    fn validation_rejects_other_control_characters_in_the_body() {
        assert!(validate_create(&create_params("T", "esc\u{1b}[2J")).is_err());
    }

    #[test]
    fn validation_rejects_bidi_characters_in_the_body() {
        for c in ['\u{202E}', '\u{2067}', '\u{200F}'] {
            let e = err_text(validate_create(&create_params("T", &format!("a{c}b"))));
            assert!(e.contains("body"), "U+{:04X}: {e}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_zero_width_characters_in_the_body() {
        for c in ['\u{200B}', '\u{200D}', '\u{FEFF}'] {
            let e = err_text(validate_create(&create_params("T", &format!("a{c}b"))));
            assert!(e.contains("body"), "U+{:04X}: {e}", c as u32);
        }
    }

    #[test]
    fn validation_rejects_a_bad_page_id_and_if_sha_on_update() {
        let mut p = update_params("T", "b");
        p.page_id = String::new();
        assert!(err_text(validate_update(&p)).contains("page_id"));
        let mut p = update_params("T", "b");
        p.if_sha = Some("x".repeat(MAX_PAGE_ID_CHARS + 1));
        assert!(err_text(validate_update(&p)).contains("if_sha"));
    }

    // ── Capture: no HTTP, one PendingAction ──────────────────────────────

    fn pending_for(state: &Arc<Mutex<GatewayState>>, goal: &str) -> Vec<PendingAction> {
        let id = Uuid::parse_str(goal).unwrap();
        state
            .lock()
            .unwrap()
            .pending_actions
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    fn result_json(r: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(&r.content[0].raw.as_text().unwrap().text).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_captures_a_pending_action_and_makes_no_http_call() {
        let (state, _dir, _ct, stats) = test_project_with_stats().await;
        let params = create_params("Runbook", "Steps\n1. one");
        let goal = params.goal_run_id.clone();

        let result = handle_wiki_create(&state, params).unwrap();

        let v = result_json(&result);
        assert_eq!(v["status"], "captured_for_review");
        assert!(v["note"].as_str().unwrap().contains("ta draft view"));
        let pending = pending_for(&state, &goal);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, CREATE_TOOL_NAME);
        assert_eq!(pending[0].kind, ActionKind::StateChanging);
        assert_eq!(pending[0].disposition, ArtifactDisposition::Pending);
        assert_eq!(pending[0].parameters["title"], "Runbook");
        assert_eq!(
            pending[0].parameters["tags"],
            serde_json::json!(["alpha", "beta"])
        );
        assert_eq!(
            stats
                .http_requests
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "capture must not contact Wayfinder at all"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_captures_a_pending_action_with_if_sha_and_makes_no_http_call() {
        let (state, _dir, _ct, stats) = test_project_with_stats().await;
        let params = update_params("Runbook v2", "New body");
        let goal = params.goal_run_id.clone();

        handle_wiki_update(&state, params).unwrap();

        let pending = pending_for(&state, &goal);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, UPDATE_TOOL_NAME);
        assert_eq!(pending[0].parameters["page_id"], "page-1");
        assert_eq!(pending[0].parameters["if_sha"], "sha-1");
        assert_eq!(
            stats
                .http_requests
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capturing_the_same_write_twice_is_idempotent() {
        let (state, _dir, _ct, _stats) = test_project_with_stats().await;
        let params = create_params("Runbook", "Steps");
        let goal = params.goal_run_id.clone();

        let first = result_json(&handle_wiki_create(&state, params.clone()).unwrap());
        let second = result_json(&handle_wiki_create(&state, params).unwrap());

        assert_eq!(pending_for(&state, &goal).len(), 1);
        assert_eq!(first["action_id"], second["action_id"]);
        assert_eq!(second["status"], "already_captured_for_review");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_different_write_in_the_same_goal_is_captured_separately() {
        let (state, _dir, _ct, _stats) = test_project_with_stats().await;
        let a = create_params("One", "x");
        let mut b = create_params("Two", "x");
        b.goal_run_id = a.goal_run_id.clone();
        let goal = a.goal_run_id.clone();
        handle_wiki_create(&state, a).unwrap();
        handle_wiki_create(&state, b).unwrap();
        assert_eq!(pending_for(&state, &goal).len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_input_is_rejected_without_capturing() {
        let (state, _dir, _ct, _stats) = test_project_with_stats().await;
        let params = create_params("T", "a\0b");
        let goal = params.goal_run_id.clone();
        assert!(handle_wiki_create(&state, params).is_err());
        assert!(pending_for(&state, &goal).is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_undeclared_scope_and_a_bad_goal_id_are_rejected_without_capturing() {
        let (state, _dir, _ct, _stats) = test_project_with_stats().await;
        let mut params = create_params("T", "b");
        params.id = "other-project".to_string();
        let goal = params.goal_run_id.clone();
        let e = handle_wiki_create(&state, params).unwrap_err();
        assert!(format!("{e}").contains("not declared"));
        assert!(pending_for(&state, &goal).is_empty());

        let mut params = update_params("T", "b");
        params.goal_run_id = "not-a-uuid".to_string();
        let e = handle_wiki_update(&state, params).unwrap_err();
        assert!(format!("{e}").contains("goal_run_id"));
    }

    // ── Apply: exactly one write, with the writer credential ─────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn applying_a_captured_create_performs_exactly_one_write_with_the_writer_credential() {
        let (state, dir, _ct, stats) = test_project_with_stats().await;
        let params = create_params("Runbook", "Steps");
        let goal = params.goal_run_id.clone();
        handle_wiki_create(&state, params).unwrap();
        let action = pending_for(&state, &goal).remove(0);
        assert_eq!(
            stats
                .http_requests
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        let summary =
            apply_wiki_action(dir.path(), false, &action.tool_name, &action.parameters).unwrap();

        assert!(summary.contains("created"), "{summary}");
        let writes = stats.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1, "exactly one write: {writes:?}");
        assert_eq!(writes[0].0, "wiki_create");
        assert_eq!(writes[0].1["title"], "Runbook");
        assert_eq!(writes[0].1["tags"], serde_json::json!(["alpha", "beta"]));
        let auths = stats.auths.lock().unwrap().clone();
        assert!(!auths.is_empty());
        assert!(
            auths.iter().all(|a| a.contains("write-tok")),
            "every request must carry the writer credential: {auths:?}"
        );
        assert!(!auths.iter().any(|a| a.contains("read-tok")));
        // The written page is now in the local cache.
        assert!(WikiCache::new(dir.path())
            .get("project", "proj-1", "page-new")
            .unwrap()
            .is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn applying_a_captured_update_carries_if_sha_to_wayfinder() {
        let (state, dir, _ct, stats) = test_project_with_stats().await;
        let params = update_params("Runbook v2", "New body");
        let goal = params.goal_run_id.clone();
        handle_wiki_update(&state, params).unwrap();
        let action = pending_for(&state, &goal).remove(0);

        let summary =
            apply_wiki_action(dir.path(), false, &action.tool_name, &action.parameters).unwrap();

        assert!(summary.contains("updated"), "{summary}");
        let writes = stats.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "wiki_update");
        assert_eq!(writes[0].1["page_id"], "page-1");
        assert_eq!(writes[0].1["if_sha"], "sha-1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_revalidates_an_edited_action_and_makes_no_write() {
        let (_state, dir, _ct, stats) = test_project_with_stats().await;
        let params = serde_json::json!({
            "scope": "project", "id": "proj-1", "title": "T",
            "body": "evil\u{202E}text", "type": null, "tags": [],
        });
        let err = apply_wiki_action(dir.path(), false, CREATE_TOOL_NAME, &params).unwrap_err();
        assert!(err.to_string().contains("body"), "{err}");
        assert!(stats.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn review_lines_show_scope_ids_title_type_tags_and_a_200_char_preview() {
        let body = format!("{}\nTAIL", "x".repeat(250));
        let lines = review_lines(
            CREATE_TOOL_NAME,
            &serde_json::json!({
                "scope": "project", "id": "proj-1", "title": "Runbook",
                "type": "note", "tags": ["a", "b"], "body": body,
            }),
        )
        .join("\n");
        assert!(lines.contains("scope=project id=proj-1"));
        assert!(lines.contains("Title: Runbook"));
        assert!(lines.contains("Type: note"));
        assert!(lines.contains("Tags: a, b"));
        assert!(lines.contains(&format!("Body: {} bytes", body.len())));
        assert!(lines.contains(&format!("Preview: {}...", "x".repeat(200))));
        assert!(!lines.contains("TAIL"));

        let upd = review_lines(
            UPDATE_TOOL_NAME,
            &serde_json::json!({
                "scope": "org", "id": "org-1", "page_id": "page-9", "title": "T",
                "body": "short", "if_sha": "sha-9",
            }),
        )
        .join("\n");
        assert!(upd.contains("Page id: page-9") && upd.contains("If-sha: sha-9"));
        assert!(upd.contains("Preview: short"));
    }

    // ── [wiki] seed_context read-side switch ────────────────────────────

    fn set_seed_context(dir: &tempfile::TempDir, toml: &str) {
        std::fs::write(dir.path().join(".ta/workflow.toml"), toml).unwrap();
    }

    fn search_params() -> WikiSearchParams {
        WikiSearchParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            query: "anything".to_string(),
        }
    }

    fn get_params() -> WikiGetParams {
        WikiGetParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
            page_id: "page-1".to_string(),
        }
    }

    fn types_params() -> WikiTypesParams {
        WikiTypesParams {
            scope: "project".to_string(),
            id: "proj-1".to_string(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seed_context_default_and_true_allow_reads() {
        let (state, dir, _ct) = test_project_with_mock_server().await;
        // No workflow.toml at all.
        assert!(handle_wiki_get(&state, get_params()).is_ok());
        // File without a [wiki] section.
        set_seed_context(&dir, "[budget]\nmax_tokens = 10\n");
        assert!(handle_wiki_get(&state, get_params()).is_ok());
        set_seed_context(&dir, "[wiki]\nseed_context = true\n");
        assert!(handle_wiki_get(&state, get_params()).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seed_context_false_refuses_each_read_tool_with_actionable_message() {
        let (state, dir, _ct) = test_project_with_mock_server().await;
        // Prime the cache while reads are on, so a refusal can't be a miss.
        handle_wiki_get(&state, get_params()).unwrap();
        set_seed_context(&dir, "[wiki]\nseed_context = false\n");

        let errs = [
            (
                "ta_wiki_search",
                handle_wiki_search(&state, search_params()),
            ),
            ("ta_wiki_get", handle_wiki_get(&state, get_params())),
            ("ta_wiki_types", handle_wiki_types(&state, types_params())),
        ];
        for (tool, result) in errs {
            let msg = format!("{}", result.expect_err(tool));
            assert!(msg.contains(tool), "{msg}");
            assert!(msg.contains("seed_context"), "names the setting: {msg}");
            assert!(msg.contains("workflow.toml"), "names the file: {msg}");
            assert!(
                msg.contains("seed_context = true"),
                "says how to fix: {msg}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seed_context_false_still_allows_wiki_writes() {
        let (state, dir, _ct, _stats) = test_project_with_stats().await;
        set_seed_context(&dir, "[wiki]\nseed_context = false\n");

        assert!(handle_wiki_get(&state, get_params()).is_err());

        let create = create_params("Runbook", "Steps");
        let goal = create.goal_run_id.clone();
        let v = result_json(&handle_wiki_create(&state, create).unwrap());
        assert_eq!(v["status"], "captured_for_review");
        assert_eq!(pending_for(&state, &goal).len(), 1);

        let update = update_params("Runbook", "Steps v2");
        let goal = update.goal_run_id.clone();
        let v = result_json(&handle_wiki_update(&state, update).unwrap());
        assert_eq!(v["status"], "captured_for_review");
        assert_eq!(pending_for(&state, &goal).len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seed_context_malformed_value_errors_with_file_and_key() {
        let (state, dir, _ct) = test_project_with_mock_server().await;
        set_seed_context(&dir, "[wiki]\nseed_context = \"maybe\"\n");
        let msg = format!("{}", handle_wiki_get(&state, get_params()).unwrap_err());
        assert!(msg.contains("workflow.toml"), "{msg}");
        assert!(msg.contains("[wiki] seed_context"), "{msg}");
    }
}
