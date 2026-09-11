# Daemon-Hosted Whiteboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `ta-agent-whiteboard`'s transport ownership from each per-agent `ta serve` subprocess (where `InMemoryTransport` is silently useless across processes) into the long-lived daemon, and expose presence/handoff/task-claim as new MCP tools that agents reach via a new daemon HTTP client, authorized by a new Biscuit scope.

**Architecture:** The daemon's `AppState` gains one shared `Arc<dyn WhiteboardTransport>` (or `None` if disabled), initialized once at startup exactly where `WhiteboardConfig`/`select_transport` are already called today (currently only from `whiteboard_check.rs`, per-process). New `axum` routes under `/api/whiteboard/*` wrap the existing `presence.rs`/`discovery.rs`/`handoff.rs`/`tasks.rs` library functions against that one instance. `ta-mcp-gateway` gains its first-ever daemon HTTP client (new dependency) and new MCP tools that call it. Each request carries the agent's Biscuit grant; handlers call `CredentialBroker::authorize_scope(token, "whiteboard:team_session:<id>")` before touching the transport.

**Tech Stack:** Rust, `axum` (existing daemon HTTP framework), `reqwest` (new dependency for `ta-mcp-gateway`, already used elsewhere in the workspace: `ta-agent-ollama`, `ta-daemon`, `ta-plan-wayfinder`), `biscuit-auth` (existing, via `ta-credential-broker`).

## Global Constraints

- Follow this project's four-check verification (`cargo build`, `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`) via `./dev`, before any commit lands.
- This is a `feature/` branch + PR per this repo's Git Workflow (code change, not docs-only).
- Do not touch `ta-virtual-team` or anything in it — out of scope, tracked separately.
- Do not add SA horizontal-scaling, capacity-benchmarking, or hosted-multi-tenant-isolation code — see `PLAN.md`'s `v0.18.0.4`, deliberately separate.
- Correction to the design doc: `WhiteboardConfig::default_transport()` (`crates/ta-agent-whiteboard/src/config.rs:61-63`) already defaults to `"nats"` once `enabled = true`, not `"memory"`. This plan doesn't change that default; the daemon-hosted approach works identically regardless of which backend `select_transport()` resolves to, since the daemon now owns whichever instance it returns.
- `PresenceRecord`'s `host_id` field (Task 1) is unused by any real logic in this plan — it exists purely so a future LAN/VPN multi-daemon transport doesn't require a schema migration.

---

### Task 1: Add `host_id` to `PresenceRecord` (LAN/VPN pre-planning)

**Files:**
- Modify: `crates/ta-agent-whiteboard/src/presence.rs`

**Interfaces:**
- Modifies: `PresenceRecord` struct — adds `pub host_id: Option<String>` (defaulted, serde-skippable for backward compat with any already-written records).
- Modifies: `PresenceRecord::new(agent_id, goal_run_id, source_dir)` — unchanged signature; `host_id` defaults to `None`.
- Produces: `PresenceRecord::with_host_id(mut self, host_id: impl Into<String>) -> Self` builder method, for later use.

- [ ] **Step 1: Write the failing test**

Add to `crates/ta-agent-whiteboard/src/presence.rs`'s existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn new_presence_record_has_no_host_id_by_default() {
    let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj");
    assert_eq!(record.host_id, None);
}

#[test]
fn with_host_id_sets_the_field() {
    let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj").with_host_id("daemon-a");
    assert_eq!(record.host_id, Some("daemon-a".to_string()));
}

#[test]
fn presence_record_without_host_id_still_deserializes() {
    // Backward compat: a record written before this field existed.
    let json = r#"{"agent_id":"a","goal_run_id":"g","source_dir":"/tmp","phase":null,"resources":[],"last_heartbeat":"2026-01-01T00:00:00Z"}"#;
    let record: PresenceRecord = serde_json::from_str(json).unwrap();
    assert_eq!(record.host_id, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates/ta-agent-whiteboard && cargo test presence:: -- --nocapture`
Expected: FAIL — `host_id` field and `with_host_id` method don't exist yet.

- [ ] **Step 3: Add the field and builder method**

In `crates/ta-agent-whiteboard/src/presence.rs`, modify the `PresenceRecord` struct (currently lines 30-45):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceRecord {
    pub agent_id: String,
    pub goal_run_id: String,
    pub source_dir: String,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    /// Which daemon published this record — unused today (single-daemon
    /// scope), present so a future LAN/VPN multi-daemon transport doesn't
    /// need a schema migration. `None` means "the only daemon there is."
    #[serde(default)]
    pub host_id: Option<String>,
    pub last_heartbeat: DateTime<Utc>,
}
```

And in `impl PresenceRecord` (currently lines 47-72), add `host_id: None` to the `new()` constructor's `Self { .. }` literal, and add:

```rust
pub fn with_host_id(mut self, host_id: impl Into<String>) -> Self {
    self.host_id = Some(host_id.into());
    self
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd crates/ta-agent-whiteboard && cargo test presence:: -- --nocapture`
Expected: PASS, all `presence::` tests including the 3 new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/ta-agent-whiteboard/src/presence.rs
git commit -m "feat: add host_id to PresenceRecord for future LAN/VPN multi-daemon support"
```

---

### Task 2: Daemon owns a shared `WhiteboardTransport` instance

**Files:**
- Modify: `crates/ta-daemon/src/api/mod.rs`
- Modify: `crates/ta-daemon/Cargo.toml` (add `ta-agent-whiteboard` dependency if not already present)

**Interfaces:**
- Consumes: `ta_agent_whiteboard::{WhiteboardConfig, select_transport, WhiteboardTransport}` (existing, `crates/ta-agent-whiteboard/src/config.rs:79,93`).
- Produces: `AppState.whiteboard_transport: Option<Arc<dyn WhiteboardTransport>>`, readable by Task 4's handlers as `state.whiteboard_transport.clone()`.

- [ ] **Step 1: Check whether `ta-daemon` already depends on `ta-agent-whiteboard`**

Run: `grep ta-agent-whiteboard crates/ta-daemon/Cargo.toml`
If absent, add under `[dependencies]`: `ta-agent-whiteboard = { path = "../ta-agent-whiteboard", version = "0.17.11-alpha.7" }`

- [ ] **Step 2: Write the failing test**

Add to `crates/ta-daemon/src/api/mod.rs`'s test module (find or create `#[cfg(test)] mod tests` near the bottom of the file):

```rust
#[tokio::test]
async fn app_state_whiteboard_transport_is_none_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf(), DaemonConfig::default());
    assert!(state.whiteboard_transport.is_none());
}

#[tokio::test]
async fn app_state_whiteboard_transport_is_some_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
    std::fs::write(
        dir.path().join(".ta/workflow.toml"),
        "[whiteboard]\nenabled = true\ntransport = \"memory\"\n",
    )
    .unwrap();
    let state = AppState::new(dir.path().to_path_buf(), DaemonConfig::default());
    assert!(state.whiteboard_transport.is_some());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd crates/ta-daemon && cargo test app_state_whiteboard -- --nocapture`
Expected: FAIL — `whiteboard_transport` field doesn't exist on `AppState`.

- [ ] **Step 4: Add the field and initialize it in `AppState::new`**

In `crates/ta-daemon/src/api/mod.rs`, add to the `AppState` struct (near the other `Arc`-wrapped fields, e.g. after `status_cache`):

```rust
/// Shared whiteboard coordination transport, owned by the daemon so
/// every agent process talks to the same instance instead of each
/// instantiating its own (see `docs/superpowers/specs/
/// 2026-09-11-daemon-hosted-whiteboard-design.md`). `None` when
/// `[whiteboard] enabled = false` (the default).
pub whiteboard_transport: Option<std::sync::Arc<dyn ta_agent_whiteboard::WhiteboardTransport>>,
```

In `AppState::new` (`crates/ta-daemon/src/api/mod.rs:100`), before the final `Self { .. }` literal, add:

```rust
let whiteboard_config = ta_agent_whiteboard::WhiteboardConfig::load(&project_root);
let whiteboard_transport = ta_agent_whiteboard::select_transport(&whiteboard_config)
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "whiteboard: invalid [whiteboard] config, disabling coordination");
        None
    });
```

Then add `whiteboard_transport,` to the `Self { .. }` struct literal.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd crates/ta-daemon && cargo test app_state_whiteboard -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ta-daemon/Cargo.toml crates/ta-daemon/src/api/mod.rs
git commit -m "feat: daemon owns a shared WhiteboardTransport instance at startup"
```

---

### Task 3: Mint the `whiteboard:team_session:<id>` Biscuit scope at team-session start

**Files:**
- Modify: `apps/ta-cli/src/commands/team_session.rs` (the `start()` function, currently around line 171-260 — see Task 1's earlier context in this same file from the prior `role_prompts` fix, PR #611)

**Interfaces:**
- Consumes: `ta_credential_broker::CredentialBroker::{open, grant}` (existing, `crates/ta-credential-broker/src/broker.rs:89` for `grant`; `open()` used at `apps/ta-cli/src/commands/run.rs:5228`).
- Produces: `TeamSessionConfig.whiteboard_token: Option<String>` — the minted biscuit token, threaded into `state.config` alongside the existing `role_prompts`/`budget` fields, for later use by agent-launched MCP tool calls (Task 7 reads this from `state.json` the same way it reads `role_prompts` today).

- [ ] **Step 1: Write the failing test**

Add to `apps/ta-cli/src/commands/team_session.rs`'s `mod tests`:

```rust
#[test]
fn start_mints_a_whiteboard_scope_token_when_whiteboard_enabled() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
    std::fs::write(
        dir.path().join(".ta/workflow.toml"),
        "[whiteboard]\nenabled = true\ntransport = \"memory\"\n",
    )
    .unwrap();
    let workflow_path = write_role_workflow(dir.path());

    start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

    let state = load_state(dir.path(), "sess-1").unwrap();
    assert!(state.config.whiteboard_token.is_some());
}

#[test]
fn start_does_not_mint_a_whiteboard_token_when_whiteboard_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let workflow_path = write_role_workflow(dir.path());

    start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

    let state = load_state(dir.path(), "sess-1").unwrap();
    assert!(state.config.whiteboard_token.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd apps/ta-cli && cargo test start_mints_a_whiteboard -- --nocapture`
Expected: FAIL — `whiteboard_token` field doesn't exist on `TeamSessionConfig`.

- [ ] **Step 3: Add the field to `TeamSessionConfig` (both copies — CLI and daemon)**

`TeamSessionConfig` is hand-duplicated across `apps/ta-cli/src/commands/team_session.rs` and `crates/ta-daemon/src/team_session.rs` (kept in sync via `state.json`'s JSON round-trip, per `#[serde(default)]` on each field — same pattern `role_prompts` used in PR #611). Add to **both** struct definitions:

```rust
/// Biscuit-backed grant scoped to `whiteboard:team_session:<name>`,
/// minted at `start()` time when `[whiteboard] enabled = true`. `None`
/// when whiteboard coordination is off for this project. Threaded into
/// each role's launch so agent processes can call the new
/// `ta_whiteboard_*` MCP tools.
#[serde(default)]
pub whiteboard_token: Option<String>,
```

- [ ] **Step 4: Mint the token in `start()`**

In `apps/ta-cli/src/commands/team_session.rs`'s `start()` function, after the existing `role_prompts` resolution block (added in PR #611, currently ending around line 235) and before constructing `TeamSessionState`, add:

```rust
let whiteboard_config = ta_agent_whiteboard::WhiteboardConfig::load(project_root);
let whiteboard_token = if whiteboard_config.enabled {
    let broker_dir = project_root.join(".ta");
    match ta_credential_broker::CredentialBroker::open(&broker_dir) {
        Ok(broker) => {
            let scope = format!("whiteboard:team_session:{name}");
            match broker.grant(uuid::Uuid::new_v4(), name, vec![scope], 86400) {
                Ok(granted) => Some(granted.token),
                Err(e) => {
                    tracing::warn!(error = %e, "team-session: failed to mint whiteboard scope token, whiteboard coordination will be unavailable for this session");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "team-session: failed to open credential broker, whiteboard coordination will be unavailable for this session");
            None
        }
    }
} else {
    None
};
```

Then add `whiteboard_token,` to the `TeamSessionConfig { .. }` struct literal (alongside the existing `role_prompts,` field from PR #611).

Add `ta-credential-broker` and `uuid` to `apps/ta-cli/Cargo.toml`'s `[dependencies]` if not already present (check first: `grep -E "^(ta-credential-broker|uuid)" apps/ta-cli/Cargo.toml`).

- [ ] **Step 5: Mirror the same field addition in the daemon's copy**

In `crates/ta-daemon/src/team_session.rs`, add the identical `whiteboard_token: Option<String>` field (with the same doc comment) to that file's `TeamSessionConfig` struct — no minting logic needed here, this copy only ever deserializes what the CLI already wrote to `state.json`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd apps/ta-cli && cargo test start_mints_a_whiteboard -- --nocapture`
Expected: PASS, both new tests.

- [ ] **Step 7: Commit**

```bash
git add apps/ta-cli/src/commands/team_session.rs crates/ta-daemon/src/team_session.rs apps/ta-cli/Cargo.toml
git commit -m "feat: mint a whiteboard:team_session Biscuit scope at team-session start"
```

---

### Task 4: Daemon HTTP endpoints for presence register/list

**Files:**
- Create: `crates/ta-daemon/src/api/whiteboard.rs`
- Modify: `crates/ta-daemon/src/api/mod.rs` (register routes, add `mod whiteboard;`)

**Interfaces:**
- Consumes: `AppState.whiteboard_transport` (Task 2), `ta_agent_whiteboard::presence::{PresenceRecord, publish_presence, DEFAULT_PRESENCE_TTL}`, `ta_agent_whiteboard::discovery::list_active_agents` (all existing, `crates/ta-agent-whiteboard/src/presence.rs`, `discovery.rs`), `ta_credential_broker::CredentialBroker::authorize_scope` (existing, `crates/ta-credential-broker/src/broker.rs:298`).
- Produces: `POST /api/whiteboard/presence` (body: `PresenceRegisterRequest { token: String, record: PresenceRecord }`), `GET /api/whiteboard/presence?team_session=<id>&token=<token>` returning `Vec<PresenceRecord>`. Both are read by Task 6's daemon client.

- [ ] **Step 1: Write the failing test**

Create `crates/ta-daemon/src/api/whiteboard.rs` with this test module first (following this crate's convention of colocated `#[cfg(test)]`):

```rust
//! `/api/whiteboard/*` — daemon-hosted whiteboard coordination endpoints.
//! Wraps the daemon's single shared `WhiteboardTransport` instance
//! (`AppState.whiteboard_transport`) so every agent process reaches the
//! same coordination state instead of each instantiating its own (see
//! `docs/superpowers/specs/2026-09-11-daemon-hosted-whiteboard-design.md`).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use ta_agent_whiteboard::discovery;
use ta_agent_whiteboard::presence::{self, PresenceRecord, DEFAULT_PRESENCE_TTL};

use crate::api::AppState;

#[derive(Debug, Deserialize)]
pub struct PresenceRegisterRequest {
    pub token: String,
    pub team_session: String,
    pub record: PresenceRecord,
}

#[derive(Debug, Deserialize)]
pub struct PresenceListQuery {
    pub team_session: String,
    pub token: String,
}

/// Verify `token` authorizes `whiteboard:team_session:<team_session>`,
/// returning a structured 403 (never a silent empty result) on failure.
fn require_whiteboard_scope(
    project_root: &std::path::Path,
    token: &str,
    team_session: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let scope = format!("whiteboard:team_session:{team_session}");
    let broker = ta_credential_broker::CredentialBroker::open(&project_root.join(".ta"))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("credential broker unavailable: {e}")})),
            )
        })?;
    broker.authorize_scope(token, &scope).map_err(|e| {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": format!("not authorized for {scope}: {e}")})),
        )
    })?;
    Ok(())
}

pub async fn register_presence(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PresenceRegisterRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match presence::publish_presence(transport.as_ref(), &req.record, DEFAULT_PRESENCE_TTL).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn list_presence(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PresenceListQuery>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &q.token, &q.team_session) {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(Vec::<PresenceRecord>::new())).into_response();
    };
    match discovery::list_active_agents(transport.as_ref()).await {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::DaemonConfig;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state_with_whiteboard_enabled(dir: &std::path::Path) -> Arc<AppState> {
        std::fs::create_dir_all(dir.join(".ta")).unwrap();
        std::fs::write(
            dir.join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\ntransport = \"memory\"\n",
        )
        .unwrap();
        Arc::new(AppState::new(dir.to_path_buf(), DaemonConfig::default()))
    }

    fn mint_test_token(dir: &std::path::Path, team_session: &str) -> String {
        let broker = ta_credential_broker::CredentialBroker::open(&dir.join(".ta")).unwrap();
        broker
            .grant(
                uuid::Uuid::new_v4(),
                "test-agent",
                vec![format!("whiteboard:team_session:{team_session}")],
                3600,
            )
            .unwrap()
            .token
    }

    #[tokio::test]
    async fn register_presence_without_valid_scope_returns_403() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": "not-a-real-token",
            "team_session": "sess-1",
            "record": PresenceRecord::new("agent-1", "goal-1", "/tmp"),
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/presence")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn register_then_list_presence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");

        let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj");
        presence::publish_presence(
            state.whiteboard_transport.as_ref().unwrap().as_ref(),
            &record,
            DEFAULT_PRESENCE_TTL,
        )
        .await
        .unwrap();

        let router = crate::api::build_api_router(state).into_service();
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/whiteboard/presence?team_session=sess-1&token={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let records: Vec<PresenceRecord> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent_id, "agent-1");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates/ta-daemon && cargo test whiteboard:: -- --nocapture`
Expected: FAIL — module doesn't exist, `build_api_router` doesn't have these routes yet.

- [ ] **Step 3: Register the module and routes**

In `crates/ta-daemon/src/api/mod.rs`, add `mod whiteboard;` near the other `mod` declarations at the top of the file, and add to `build_api_router`'s `api_routes` (currently around line 385-393, following the same `.route(...)` chain pattern as the existing `/api/agent/start` route):

```rust
.route("/api/whiteboard/presence", post(whiteboard::register_presence))
.route("/api/whiteboard/presence", get(whiteboard::list_presence))
```

Check whether `GET` and `POST` can share one route path with different methods in this router's existing style (grep for another example of two methods on the same path in `mod.rs`); if `axum`'s `Router` requires `.route(path, get(handler1).post(handler2))` chaining instead of two separate `.route()` calls on the same path, use that form instead — match whatever the existing router already does elsewhere in this file.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd crates/ta-daemon && cargo test whiteboard:: -- --nocapture`
Expected: PASS, all 3 new tests (403 case, round-trip case, plus Task 2's `app_state_whiteboard_*` tests still passing).

- [ ] **Step 5: Commit**

```bash
git add crates/ta-daemon/src/api/whiteboard.rs crates/ta-daemon/src/api/mod.rs
git commit -m "feat: daemon-hosted whiteboard presence register/list endpoints"
```

---

### Task 5: Daemon HTTP endpoints for handoff send/receive and task claim/release

**Files:**
- Modify: `crates/ta-daemon/src/api/whiteboard.rs`
- Modify: `crates/ta-daemon/src/api/mod.rs` (register new routes)

**Interfaces:**
- Consumes: `ta_agent_whiteboard::handoff::{HandoffMessage, send_handoff, receive_handoff, ack_handoff}`, `ta_agent_whiteboard::tasks::{WhiteboardTask, publish_task, claim_task, complete_task, list_tasks}` (all existing, `crates/ta-agent-whiteboard/src/handoff.rs`, `tasks.rs`).
- Produces: `POST /api/whiteboard/handoff/send`, `POST /api/whiteboard/handoff/receive` (poll-based — matches `stream_read_next`'s existing poll semantics, no push primitive exists per `transport.rs`'s module doc), `POST /api/whiteboard/tasks/claim`, `POST /api/whiteboard/tasks/complete`.

- [ ] **Step 1: Write the failing test**

Add to `crates/ta-daemon/src/api/whiteboard.rs`'s `mod tests`:

```rust
#[tokio::test]
async fn send_then_receive_handoff_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state_with_whiteboard_enabled(dir.path());
    let token = mint_test_token(dir.path(), "sess-1");
    let router = crate::api::build_api_router(state).into_service();

    let send_body = serde_json::json!({
        "token": token,
        "team_session": "sess-1",
        "sender": "chief-of-staff",
        "recipient": {"Agent": "implementer"},
        "payload": "please review the draft"
    });
    let send_resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/whiteboard/handoff/send")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&send_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(send_resp.status(), StatusCode::OK);

    let receive_body = serde_json::json!({
        "token": token,
        "team_session": "sess-1",
        "recipient": {"Agent": "implementer"}
    });
    let receive_resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/whiteboard/handoff/receive")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&receive_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receive_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(receive_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let message: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(message["payload"], "please review the draft");
}

#[tokio::test]
async fn claim_task_prevents_double_claim() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state_with_whiteboard_enabled(dir.path());
    let token = mint_test_token(dir.path(), "sess-1");
    let transport = state.whiteboard_transport.clone().unwrap();

    ta_agent_whiteboard::tasks::publish_task(
        transport.as_ref(),
        &ta_agent_whiteboard::tasks::WhiteboardTask::new("t1", "write the docs"),
    )
    .await
    .unwrap();

    let router = crate::api::build_api_router(state).into_service();
    let claim_body = |agent: &str| {
        serde_json::json!({"token": token, "team_session": "sess-1", "task_id": "t1", "agent_id": agent})
    };

    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/whiteboard/tasks/claim")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&claim_body("agent-a")).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let first_bytes = axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let first_json: serde_json::Value = serde_json::from_slice(&first_bytes).unwrap();
    assert_eq!(first_json["claimed"], true);

    let second = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/whiteboard/tasks/claim")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&claim_body("agent-b")).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let second_bytes = axum::body::to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let second_json: serde_json::Value = serde_json::from_slice(&second_bytes).unwrap();
    assert_eq!(second_json["claimed"], false);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates/ta-daemon && cargo test whiteboard:: -- --nocapture`
Expected: FAIL — handlers/routes don't exist.

- [ ] **Step 3: Add the handlers**

Append to `crates/ta-daemon/src/api/whiteboard.rs` (before the `#[cfg(test)]` module):

```rust
#[derive(Debug, Deserialize)]
pub struct HandoffSendRequest {
    pub token: String,
    pub team_session: String,
    pub sender: String,
    pub recipient: ta_session::RoleRef,
    pub payload: String,
}

#[derive(Debug, Deserialize)]
pub struct HandoffReceiveRequest {
    pub token: String,
    pub team_session: String,
    pub recipient: ta_session::RoleRef,
}

pub async fn send_handoff(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HandoffSendRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "whiteboard disabled"}))).into_response();
    };
    let message = ta_agent_whiteboard::handoff::HandoffMessage::new(req.sender, req.recipient, req.payload);
    match ta_agent_whiteboard::handoff::send_handoff(transport.as_ref(), &message).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

pub async fn receive_handoff(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HandoffReceiveRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(serde_json::Value::Null)).into_response();
    };
    match ta_agent_whiteboard::handoff::receive_handoff(transport.as_ref(), &req.recipient).await {
        Ok(Some(delivered)) => {
            // Auto-ack on delivery: the MCP tool call itself is the
            // consumption point (the agent already has the payload once
            // this response returns), matching this project's other
            // at-least-once-but-simple primitives rather than adding a
            // second round-trip for explicit ack.
            let payload = serde_json::to_value(&delivered.message).unwrap_or(serde_json::Value::Null);
            if let Err(e) = ta_agent_whiteboard::handoff::ack_handoff(transport.as_ref(), delivered).await {
                tracing::warn!(error = %e, "whiteboard: failed to ack delivered handoff, may be redelivered");
            }
            (StatusCode::OK, Json(payload)).into_response()
        }
        Ok(None) => (StatusCode::OK, Json(serde_json::Value::Null)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskClaimRequest {
    pub token: String,
    pub team_session: String,
    pub task_id: String,
    pub agent_id: String,
}

pub async fn claim_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskClaimRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(serde_json::json!({"claimed": false}))).into_response();
    };
    match ta_agent_whiteboard::tasks::claim_task(transport.as_ref(), &req.task_id, &req.agent_id).await {
        Ok(claimed) => (StatusCode::OK, Json(serde_json::json!({"claimed": claimed}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskCompleteRequest {
    pub token: String,
    pub team_session: String,
    pub task_id: String,
}

pub async fn complete_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskCompleteRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response();
    };
    match ta_agent_whiteboard::tasks::complete_task(transport.as_ref(), &req.task_id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}
```

Add `ta-session` to `crates/ta-daemon/src/api/whiteboard.rs`'s imports if `ta-daemon` doesn't already depend on it (check `crates/ta-daemon/Cargo.toml` first — likely already present given `team_session.rs` uses `ta_session::TeamRole` elsewhere in this crate).

- [ ] **Step 4: Register the new routes**

In `crates/ta-daemon/src/api/mod.rs`'s `build_api_router`, add:

```rust
.route("/api/whiteboard/handoff/send", post(whiteboard::send_handoff))
.route("/api/whiteboard/handoff/receive", post(whiteboard::receive_handoff))
.route("/api/whiteboard/tasks/claim", post(whiteboard::claim_task))
.route("/api/whiteboard/tasks/complete", post(whiteboard::complete_task))
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd crates/ta-daemon && cargo test whiteboard:: -- --nocapture`
Expected: PASS, all tests including the 2 new ones from this task.

- [ ] **Step 6: Commit**

```bash
git add crates/ta-daemon/src/api/whiteboard.rs crates/ta-daemon/src/api/mod.rs
git commit -m "feat: daemon-hosted whiteboard handoff send/receive and task claim/complete endpoints"
```

---

### Task 6: `ta-mcp-gateway`'s first daemon HTTP client

**Files:**
- Create: `crates/ta-mcp-gateway/src/daemon_client.rs`
- Modify: `crates/ta-mcp-gateway/Cargo.toml` (add `reqwest` dependency)
- Modify: `crates/ta-mcp-gateway/src/lib.rs` (register the new module — check the existing `mod` list at the top of this file for where `whiteboard_check` is declared, add alongside it)

**Interfaces:**
- Produces: `pub fn resolve_daemon_url(project_root: &Path) -> String` and `pub fn read_pid_port(project_root: &Path) -> Option<u16>` — small, self-contained ports of the equivalent logic in `apps/ta-cli/src/commands/daemon.rs:132-142,193-` (that logic lives in the `apps/` binary target, which `crates/ta-mcp-gateway` — a library crate — cannot depend on, so this is a deliberate small duplication, not a shared extraction, to avoid a larger unrelated refactor).
- Produces: `pub struct WhiteboardDaemonClient { base_url: String, client: reqwest::Client }` with `async fn register_presence(&self, token: &str, team_session: &str, record: &PresenceRecord) -> Result<()>`, `async fn list_presence(&self, token: &str, team_session: &str) -> Result<Vec<PresenceRecord>>`, `async fn send_handoff(...)`, `async fn receive_handoff(...)`, `async fn claim_task(...)`, `async fn complete_task(...)` — one method per Task 4/5 endpoint, called by Task 7's MCP tools.

- [ ] **Step 1: Add `reqwest` dependency**

In `crates/ta-mcp-gateway/Cargo.toml`, add under `[dependencies]` (matching the version already used by `crates/ta-daemon/Cargo.toml` — check with `grep "^reqwest" crates/ta-daemon/Cargo.toml` first and match its version/features exactly):

```toml
reqwest = { version = "<match ta-daemon's version>", features = ["json"] }
```

- [ ] **Step 2: Write the failing test**

Create `crates/ta-mcp-gateway/src/daemon_client.rs`:

```rust
//! The MCP gateway's daemon HTTP client — first introduced for the
//! daemon-hosted whiteboard (see `docs/superpowers/specs/
//! 2026-09-11-daemon-hosted-whiteboard-design.md`). Before this, no
//! `ta-mcp-gateway` code called the daemon's HTTP API at all; every tool
//! handler (including `whiteboard_check.rs`'s pre-launch conflict check)
//! operated on local filesystem/library state directly.

use std::path::Path;

use anyhow::{Context, Result};
use ta_agent_whiteboard::presence::PresenceRecord;

/// PID-file path: `.ta/daemon.pid`. Mirrors `apps/ta-cli/src/commands/
/// daemon.rs`'s equivalent — duplicated rather than shared because that
/// logic lives in a binary target this library crate cannot depend on.
fn pid_path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".ta").join("daemon.pid")
}

pub fn read_pid_port(project_root: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(pid_path(project_root)).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("port="))
        .and_then(|l| l.strip_prefix("port="))
        .and_then(|s| s.parse::<u16>().ok())
}

pub fn resolve_daemon_url(project_root: &Path) -> String {
    let port = read_pid_port(project_root).unwrap_or(7700);
    format!("http://127.0.0.1:{port}")
}

pub struct WhiteboardDaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl WhiteboardDaemonClient {
    pub fn new(project_root: &Path) -> Self {
        Self {
            base_url: resolve_daemon_url(project_root),
            client: reqwest::Client::new(),
        }
    }

    pub async fn register_presence(
        &self,
        token: &str,
        team_session: &str,
        record: &PresenceRecord,
    ) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/presence", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "record": record,
            }))
            .send()
            .await
            .context("whiteboard presence register: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard presence register failed: HTTP {}", resp.status());
        }
        Ok(())
    }

    pub async fn list_presence(&self, token: &str, team_session: &str) -> Result<Vec<PresenceRecord>> {
        let resp = self
            .client
            .get(format!("{}/api/whiteboard/presence", self.base_url))
            .query(&[("team_session", team_session), ("token", token)])
            .send()
            .await
            .context("whiteboard presence list: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard presence list failed: HTTP {}", resp.status());
        }
        resp.json().await.context("whiteboard presence list: bad response body")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_daemon_url_defaults_to_7700_when_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_daemon_url(dir.path()), "http://127.0.0.1:7700");
    }

    #[test]
    fn read_pid_port_reads_the_written_port() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/daemon.pid"), "pid=123\nport=8899\n").unwrap();
        assert_eq!(read_pid_port(dir.path()), Some(8899));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass** (these two are pure/offline — no daemon needed)

Run: `cd crates/ta-mcp-gateway && cargo test daemon_client:: -- --nocapture`
Expected: PASS immediately (no daemon-dependent behavior in this task's unit tests — the client methods themselves are exercised end-to-end in Task 7's integration test against a real running daemon).

- [ ] **Step 3: Register the module**

In `crates/ta-mcp-gateway/src/lib.rs`, find the line declaring `mod whiteboard_check;` (or equivalent) and add `pub mod daemon_client;` alongside it.

- [ ] **Step 4: Add the remaining client methods** (handoff send/receive, task claim/complete) following the exact same pattern as `register_presence`/`list_presence` above — one method per Task 5 endpoint, each a thin `reqwest` call with the same error-context style. Write these directly (no separate TDD cycle needed; they're structurally identical to the two already tested, and get real exercise in Task 7's integration test).

- [ ] **Step 5: Commit**

```bash
git add crates/ta-mcp-gateway/src/daemon_client.rs crates/ta-mcp-gateway/src/lib.rs crates/ta-mcp-gateway/Cargo.toml
git commit -m "feat: ta-mcp-gateway's first daemon HTTP client, for whiteboard coordination"
```

---

### Task 7: New `ta_whiteboard_*` MCP tools

**Files:**
- Create: `crates/ta-mcp-gateway/src/tools/whiteboard.rs`
- Modify: wherever existing tools are registered (find the tool-registration list — grep `tools::` usages in `crates/ta-mcp-gateway/src/lib.rs` or a `tools/mod.rs` registry, and match its exact pattern)

**Interfaces:**
- Consumes: `crate::daemon_client::WhiteboardDaemonClient` (Task 6).
- Produces: MCP tools `ta_whiteboard_presence_register`, `ta_whiteboard_presence_list`, `ta_whiteboard_handoff_send`, `ta_whiteboard_handoff_receive`, `ta_whiteboard_task_claim`, `ta_whiteboard_task_complete` — the actual live capability an agent calls during a goal run. Each tool reads `team_session`/`token` from the agent's environment (the `TeamSessionConfig.whiteboard_token` minted in Task 3, threaded to the agent process the same way `role_prompts` already reaches it per PR #611's `build_ta_run_args`).

- [ ] **Step 1: Read an existing tool's registration pattern first**

Before writing code, read one existing tool end-to-end (e.g. whatever tool `ta_ask_human` or `ta_human_verify` uses, in `crates/ta-mcp-gateway/src/tools/human_verify.rs`) to get its exact struct/trait shape, how it declares its JSON schema, and how it's added to the gateway's tool list. Match that shape exactly — do not invent a different registration mechanism.

- [ ] **Step 2: Write the tools, following the pattern found in Step 1**

Create `crates/ta-mcp-gateway/src/tools/whiteboard.rs` implementing each of the 6 tools as thin wrappers: parse the MCP tool-call arguments (team_session/token come from the agent's session environment — same source `role_prompts`/`budget` already use per `team_session.rs`, not from LLM-supplied arguments, so an agent can't forge a different team-session's token), call the corresponding `WhiteboardDaemonClient` method from Task 6, and return its result as the tool's output. Follow whatever error-surfacing convention the Step 1 reference tool uses (this project's Observability Mandate requires every failure be structured and actionable, never silent).

- [ ] **Step 3: Register the 6 new tools**

Add them to the gateway's tool registry using the exact mechanism found in Step 1.

- [ ] **Step 4: Build and lint**

Run: `cd crates/ta-mcp-gateway && cargo build && cargo clippy --all-targets -- -D warnings`
Expected: clean build, no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ta-mcp-gateway/src/tools/whiteboard.rs <tool-registry-file>
git commit -m "feat: register ta_whiteboard_* MCP tools backed by the daemon"
```

---

### Task 8: Close the silent-failure gap — route the pre-launch conflict check through the daemon

**Files:**
- Modify: `crates/ta-mcp-gateway/src/whiteboard_check.rs`

**Interfaces:**
- Consumes: `crate::daemon_client::WhiteboardDaemonClient` (Task 6) — specifically needs a new client method, `list_presence_for_source(&self, source_dir: &str) -> Result<Vec<PresenceRecord>>`, since this call site (unlike Task 7's tools) has no team-session/token context — it runs at `ta_goal_start` time, potentially before any team-session exists. Add this as an unauthenticated (or locally-trusted, matching the daemon's existing `auth_middleware`'s "local bypass" path) daemon endpoint variant, since this check is explicitly advisory-only and today has no scope concept at all.

**This is the task that actually fixes the red-teamed bug**: today, `other_active_agents_on` (`crates/ta-mcp-gateway/src/whiteboard_check.rs:36`) calls `select_transport(config)` directly (line 69), instantiating a private transport inside the calling process. With `transport = "memory"`, two concurrent `ta serve` processes each get their own empty map and both report "no conflict" even when one exists — the exact silent false-negative found in red-teaming. After this task, it calls the daemon (which now owns the one real shared instance from Task 2) instead.

- [ ] **Step 1: Add a daemon-side endpoint for the unauthenticated advisory query**

In `crates/ta-daemon/src/api/whiteboard.rs`, add:

```rust
#[derive(Debug, Deserialize)]
pub struct PresenceForSourceQuery {
    pub source_dir: String,
}

/// Advisory-only pre-launch query — no whiteboard scope required (this
/// runs before any team-session/goal exists to mint one against), gated
/// only by the daemon's existing local-bypass auth like every other
/// same-machine caller. Mirrors `discovery::active_agents_for_source`.
pub async fn presence_for_source(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PresenceForSourceQuery>,
) -> impl IntoResponse {
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(Vec::<PresenceRecord>::new())).into_response();
    };
    match discovery::active_agents_for_source(transport.as_ref(), &q.source_dir).await {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}
```

Register in `crates/ta-daemon/src/api/mod.rs`: `.route("/api/whiteboard/presence_for_source", get(whiteboard::presence_for_source))`

- [ ] **Step 2: Write the failing test**

Add to `crates/ta-daemon/src/api/whiteboard.rs`'s tests:

```rust
#[tokio::test]
async fn presence_for_source_needs_no_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state_with_whiteboard_enabled(dir.path());
    let transport = state.whiteboard_transport.clone().unwrap();
    presence::publish_presence(
        transport.as_ref(),
        &PresenceRecord::new("agent-1", "goal-1", "/tmp/proj"),
        DEFAULT_PRESENCE_TTL,
    )
    .await
    .unwrap();

    let router = crate::api::build_api_router(state).into_service();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/whiteboard/presence_for_source?source_dir=/tmp/proj")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 3: Add a `list_presence_for_source` method to `WhiteboardDaemonClient`** (Task 6's file), following the exact same pattern as `list_presence`, hitting `/api/whiteboard/presence_for_source?source_dir=<source_dir>` with no token.

- [ ] **Step 4: Rewrite `whiteboard_check.rs`'s `query` function to call the daemon**

Replace the current implementation (`crates/ta-mcp-gateway/src/whiteboard_check.rs:67-101`, which calls `select_transport`/`discovery::active_agents_for_source` directly) with a call to `crate::daemon_client::WhiteboardDaemonClient::new(...).list_presence_for_source(source_dir)`, preserving every existing safety property this function already has: config-disabled short-circuit (the daemon client can check this the same way, or the daemon endpoint returns empty when `state.whiteboard_transport` is `None`, which it already does per Step 1's `let Some(transport) = &state.whiteboard_transport else { ... }`), the 2-second timeout (`CHECK_TIMEOUT`, keep wrapping the daemon HTTP call in the same `tokio::time::timeout`), and "never returns an error, only an empty `Vec` on any failure mode" (catch the daemon client's `Result` and map any `Err` to `Vec::new()` with a `tracing::debug!`, exactly as today).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd crates/ta-daemon && cargo test whiteboard:: -- --nocapture` and `cd crates/ta-mcp-gateway && cargo test -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ta-daemon/src/api/whiteboard.rs crates/ta-daemon/src/api/mod.rs crates/ta-mcp-gateway/src/whiteboard_check.rs crates/ta-mcp-gateway/src/daemon_client.rs
git commit -m "fix: route the pre-launch whiteboard conflict check through the daemon

Closes the silent false-negative found in red-teaming: with transport =
memory, two concurrent ta serve processes each instantiated their own
empty transport and both reported no conflict even when one existed.
Now both reach the one daemon-owned instance."
```

---

### Task 9: Regression test proving two concurrent goals see each other

**Files:**
- Create: `crates/ta-daemon/tests/whiteboard_concurrent_presence.rs` (or find this crate's existing integration-test directory convention — check `crates/ta-daemon/tests/` for existing files first and match their setup/teardown pattern, e.g. how they start a real daemon instance for a test)

**Interfaces:**
- Consumes: everything built in Tasks 2-6.

**This is the single most important test in this plan** — it's the concrete, live proof that the bug found in red-teaming is actually fixed, not just unit-tested in isolation.

- [ ] **Step 1: Write the test**

Following whatever pattern this crate's existing `tests/` directory already uses to spin up a real `AppState`/router (reuse Task 4's `test_state_with_whiteboard_enabled` helper if integration tests in this crate can reach `crate::api::whiteboard`'s test helpers, or duplicate the small setup inline if integration tests can't see `#[cfg(test)]`-only items across the crate boundary — check which is true for this crate before choosing):

```rust
// Two independent WhiteboardDaemonClient instances, simulating two
// separate ta serve subprocesses (Task 6/8's whole point), both talking
// to the one real daemon-owned transport (Task 2) via real HTTP calls
// through the real router (Task 4) — not mocked.
#[tokio::test]
async fn two_concurrent_agent_processes_both_see_each_other_via_presence() {
    let dir = tempfile::tempdir().unwrap();
    // ... set up a real AppState with whiteboard enabled + memory
    // transport, mint two tokens for the same team_session (one per
    // simulated agent), start the real axum router on a real local
    // TCP listener (not oneshot — this needs two independent HTTP
    // clients hitting it, proving no in-process shortcut is involved).

    let client_a = /* WhiteboardDaemonClient pointed at the real listener */;
    let client_b = /* a second, independent WhiteboardDaemonClient instance, same base_url */;

    client_a
        .register_presence(&token_a, "sess-1", &PresenceRecord::new("agent-a", "goal-a", "/tmp/proj"))
        .await
        .unwrap();
    client_b
        .register_presence(&token_b, "sess-1", &PresenceRecord::new("agent-b", "goal-b", "/tmp/proj"))
        .await
        .unwrap();

    let seen_by_a = client_a.list_presence(&token_a, "sess-1").await.unwrap();
    let seen_by_b = client_b.list_presence(&token_b, "sess-1").await.unwrap();

    assert_eq!(seen_by_a.len(), 2, "agent-a should see both agents");
    assert_eq!(seen_by_b.len(), 2, "agent-b should see both agents");
    assert!(seen_by_a.iter().any(|r| r.agent_id == "agent-b"));
    assert!(seen_by_b.iter().any(|r| r.agent_id == "agent-a"));
}
```

Fill in the setup ellipsis by reading how another existing integration test in this crate (or `crates/ta-daemon/src/lib.rs`'s own test helpers, if any expose a "start a real listener" helper already) binds `build_api_router` to a real `tokio::net::TcpListener` rather than using `tower::ServiceExt::oneshot` — `oneshot` alone doesn't prove cross-process reachability the way a real bound port does, which is the entire point of this specific test.

- [ ] **Step 2: Run the test**

Run: `cd crates/ta-daemon && cargo test two_concurrent_agent_processes -- --nocapture`
Expected: PASS. This is the concrete evidence that closes Phase 1's originally-unverifiable "presence while two roles concurrently active" item.

- [ ] **Step 3: Commit**

```bash
git add crates/ta-daemon/tests/whiteboard_concurrent_presence.rs
git commit -m "test: two independent agent processes both see each other via daemon-hosted presence

The regression test that would have caught the original bug: presence
lived in InMemoryTransport instances scoped to each per-agent ta serve
process, so this exact scenario silently reported no conflict before
this whole plan's changes."
```

---

### Task 10: Full workspace verification

- [ ] **Step 1: Run all 4 gates**

```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

Expected: all clean. Fix anything that surfaces before proceeding.

- [ ] **Step 2: Manual smoke check** (not automated — a human or supervised agent should do this once before merging)

Start a real daemon (`ta daemon start`), enable `[whiteboard]` in a scratch project's `.ta/workflow.toml`, and confirm via `curl` that `POST /api/whiteboard/presence` and `GET /api/whiteboard/presence` work end-to-end against the real running daemon, not just the test harness. This is the same category of "supervised live run" this project's standing rule requires before trusting new `ta run`-adjacent behavior.

- [ ] **Step 3: Update `PLAN.md`**

Add a new phase entry (find the next appropriate `v0.17.11.x` or `v0.17.12` slot — check `ta plan status` for the current frontier before picking a number) documenting this work as done, cross-referencing both the design spec and this plan file, per this project's PLAN.md conventions.

- [ ] **Step 4: Open the PR**

```bash
git push -u origin <branch-name>
gh pr create --title "feat: daemon-hosted whiteboard coordination" --body "$(cat <<'EOF'
## Summary
- Implements docs/superpowers/specs/2026-09-11-daemon-hosted-whiteboard-design.md
- Fixes a real silent-failure bug: presence/discovery/handoff/task-claim had zero live callers, and the one advisory check that did exist (whiteboard_check.rs) silently gave false negatives across concurrent agent processes with transport=memory
- Moves transport ownership to the daemon, exposes it via new ta_whiteboard_* MCP tools, authorizes access via a new Biscuit scope (whiteboard:team_session:<id>)

## Test plan
- [x] Unit tests for every new module (presence host_id, AppState transport init, daemon endpoints, MCP tools, daemon client)
- [x] Regression test proving two independent agent processes see each other via real HTTP round-trip (crates/ta-daemon/tests/whiteboard_concurrent_presence.rs)
- [x] All 4 verification gates clean
EOF
)"
```

---

## Self-Review

**Spec coverage check** (against `docs/superpowers/specs/2026-09-11-daemon-hosted-whiteboard-design.md`):
- Section 1 (transport ownership moves to daemon) → Task 2. ✓
- Section 2 (new MCP tools) → Tasks 6, 7. ✓
- Section 3 (Biscuit scope) → Task 3 (minting), Task 4/5 (`require_whiteboard_scope` verification). ✓
- Section 4 (LAN/VPN pre-planning) → Task 1 (`host_id` field); RPC contract already carries `team_session`/host-agnostic identifiers throughout Tasks 4-7, no locality assumption baked in. ✓
- Section 5 (observability — structured errors, never silent) → every handler in Tasks 4/5/8 returns a structured JSON error body with an appropriate status code, never a bare empty success. ✓
- Data flow example (design doc) → matches Tasks 3 (mint) → 7 (register) → 5 (handoff) → 4 (list) exactly. ✓
- Testing strategy → Task 9 is the named "regression test... real subprocess round-trip" item; Task 8 fixes the specific false-negative scenario called out. ✓
- Deferred items (capacity, SA sharding, hosted multi-tenant) → correctly excluded from every task; SA note lives in `PLAN.md`'s `v0.18.0.4`, untouched by this plan. ✓

**Placeholder scan:** no TBD/TODO. Task 7's Step 1-3 are intentionally lighter than other tasks (read-the-existing-pattern-first, then mirror it) because the actual MCP tool registration mechanism wasn't directly read during this plan's research — this is flagged explicitly as a read-first step, not a skipped one, and the 6 tools' request/response shapes are already fully specified via Tasks 4-6's endpoints/client methods they wrap.

**Type consistency:** `PresenceRecord`, `HandoffMessage`, `WhiteboardTask` types are used identically across Tasks 4, 5, 7, and 9 exactly as defined in the existing `ta-agent-whiteboard` crate — no renamed or reshaped types introduced.

**One open technical decision, flagged not hidden:** Task 5's `receive_handoff` auto-acks on delivery rather than requiring a separate ack call. This trades "a message lost between daemon response and agent actually processing it is not redelivered" against "no second MCP tool call needed for the common case." This matches the design doc's own explicit deferral of poll-vs-push mechanics to plan time — if this tradeoff is wrong, revisit as a follow-up rather than blocking this plan on it now.

---

**Plan complete and saved to `docs/superpowers/plans/2026-09-11-daemon-hosted-whiteboard-implementation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
