# ta-ask Decision Primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `ta-ask`, a new in-tree TA-core crate providing one reusable primitive —
`ask(question, context, schema) -> {result, confidence}` — backed by a pluggable
`DecisionBackend`, with a real working backend (Decider-4b, via its own HTTP server) and one real
consumer wired in (`ta-workflow`'s reviewer chain).

**Architecture:** A thin, backend-pluggable library crate (`DecisionRequest`/`DecisionResponse`/
`DecisionBackend` trait), a `FixtureBackend` for tests, a `DeciderBackend` that spawns and keeps
warm Decider-4b's own HTTP server process and talks to it over plain HTTP, and a new
`DecisionReviewerNode` in `ta-workflow` that fills the previously-empty `advisor_confidence`-style
reviewer slot this crate was built to serve. The interface is **synchronous**, not async — see
Global Constraints for why — which makes the `ta-workflow` integration trivial (no runtime
bridging).

**Tech Stack:** Rust, matching this workspace's existing conventions (`thiserror`, `reqwest`
with the `blocking` feature, `serde`/`serde_json`, `wiremock` for HTTP-mocked tests). Decider-4b
(Mapika, Apache 2.0, github.com/Mapika/decider) as the reference backend, run via its own
documented `scripts/serve.sh` HTTP server, not Ollama/GGUF.

## Global Constraints

- **Repo and branch**: `/Users/michael/development/TrustedAutonomy`, already on feature branch
  `feature/ta-ask-decision-primitive` (off `main` at `a2119f901f`). Never commit to `main`
  directly — this repo's own `CLAUDE.md` requires feature branches + PRs for all code changes.
- **Verification before every commit** (copied verbatim from this repo's `CLAUDE.md`), all four,
  run through the Nix wrapper:
  ```bash
  ./dev cargo build --workspace
  ./dev cargo test --workspace
  ./dev cargo clippy --workspace --all-targets -- -D warnings
  ./dev cargo fmt --all -- --check
  ```
  where `./dev "command"` runs inside this repo's Nix devShell (equivalently:
  `export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"` then
  `nix develop --command bash -c "COMMAND_HERE"`).
- **Observability Mandate** (this repo's own, verbatim): every error path states what happened,
  what was being attempted, and what the user/caller can do about it. No bare `.unwrap()`/
  `.expect()` outside test code.
- **Scope boundary, explicit**: this plan builds `ta-ask` itself and its `ta-workflow` wiring
  only. It does **not** touch the separate `untollable-cli` repo or its own §3 classifier — that
  work is explicitly deferred until after a separate, future Wayfinder CoS chat-answering design
  is complete (a different collaboration, not part of this plan). It does **not** extract `ta-ask`
  to a public standalone repo the way `decision-gate`/`consensus-panel`/`task-graph` were — it
  stays an in-tree, private crate in this monorepo for now, consumed via a normal workspace path
  dependency (not a git-tag dependency).
- **Interface is synchronous, not `async_trait`** (a deliberate correction to the design doc's
  original sketch, agreed directly with `agentic-pm-ba` this session, and already reflected in the
  current version of the design doc): `ta_workflow::graph::types::ReviewerNode::review()`
  (`crates/ta-workflow/src/graph/types.rs:214-216`) is plain sync, with no async anywhere in the
  graph engine. `reqwest`'s `blocking` feature is already enabled at the workspace level
  (`Cargo.toml:137`). Making `DecisionBackend::decide` sync means the `ta-workflow` integration in
  Task 4 needs zero runtime-bridging code.
- **`DecisionReviewerNode` is registered opt-in, not inside `NodeRegistry::with_builtins()`**
  (also agreed with `agentic-pm-ba`): `crates/ta-workflow/src/graph/registry.rs`'s own header
  comment (lines 1-11) documents that kinds needing heavier dependencies are registered by the
  caller rather than built into `with_builtins()`, specifically to avoid forcing `ta-workflow`'s
  own default dependency footprint to grow. `ta-ask` has no dependency-cycle risk the way
  `ta-brain`/`ta-goal` do (nothing depends on `ta-ask` yet), so the `DecisionReviewerNode` *type*
  lives directly in `ta-workflow::graph::nodes` (unlike the `ta-brain`/`ta-goal`-dependent kinds,
  which live one layer up in `apps/ta-cli`) — but its *registration* stays an explicit opt-in call
  a caller makes, not automatic, since it requires a live `DecisionBackend` instance the caller
  must construct.
- **No mocking the only hard part**: Task 3's real end-to-end call against a genuinely running
  Decider-4b server must actually be executed, with its real output captured and reported, not
  asserted as "should work." This whole task exists because Untollable's own earlier build shipped
  with every test mocking the real dependency, and nobody ran the real thing until a final
  whole-branch review caught it — don't repeat that here.
- **`DecisionSchema::Score` is out of scope for this plan's `DeciderBackend`**: no real consumer
  needs it yet (the two known near-term consumers — `ta-workflow`'s yes/no safety reviewer here,
  and Untollable/Wayfinder's future 5-way intent classifier — are both `YesNo`/`Choice` shaped).
  `DeciderBackend::decide` must return a clear `DecisionError::UnsupportedSchema` for `Score`
  rather than a lossy or unvalidated approximation. This is a documented limitation, not a silent
  gap — it must appear in the crate's own README (Task 5) as well as the code.

---

## File Structure

```
crates/ta-ask/                                  # NEW crate
├── Cargo.toml
├── README.md                                   # Task 5
└── src/
    ├── lib.rs                                  # Task 1: core types, FixtureBackend, ask()
    └── decider_backend.rs                      # Task 2+3: DeciderBackend, wiremock tests, real smoke test
crates/ta-workflow/
├── Cargo.toml                                  # Task 4: add ta-ask path dependency
└── src/graph/
    ├── nodes/
    │   ├── mod.rs                              # Task 4: add decision_reviewer module
    │   └── decision_reviewer.rs                # Task 4: NEW — DecisionReviewerNode
    └── registry.rs                             # Task 4: add register_decision_reviewer
Cargo.toml                                      # Task 1: add crates/ta-ask to workspace members
```

---

## Task 1: Crate scaffold — core types, `FixtureBackend`, `ask()`

**Files:**
- Modify: `Cargo.toml:14-60` (workspace `members` list — add `"crates/ta-ask"`)
- Create: `crates/ta-ask/Cargo.toml`
- Create: `crates/ta-ask/src/lib.rs`

**Interfaces:**
- Produces: `pub struct DecisionRequest { pub question: String, pub context: String, pub schema:
  DecisionSchema }`; `pub enum DecisionSchema { YesNo, Choice(Vec<String>), Score { min: f64, max:
  f64 } }`; `pub enum DecisionResult { Bool(bool), Choice(String), Score(f64) }`; `pub struct
  DecisionResponse { pub result: DecisionResult, pub confidence: f64, pub model_id: String, pub
  latency_ms: u64 }`; `pub enum DecisionError { UnsupportedSchema(DecisionSchema), SpawnFailed {
  command: String, source: std::io::Error }, BackendUnavailable { url: String, timeout_secs: u64 },
  RequestFailed(reqwest::Error), MalformedResponse(String) }`; `pub trait DecisionBackend: Send +
  Sync { fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError>; }`;
  `pub fn ask(backend: &dyn DecisionBackend, question: impl Into<String>, context: impl
  Into<String>, schema: DecisionSchema) -> Result<DecisionResponse, DecisionError>`; `pub struct
  FixtureBackend` with `pub fn new(response: DecisionResponse) -> Self` and `pub fn yes_no(result:
  bool, confidence: f64) -> Self`; `pub struct ErrorFixtureBackend` with `pub fn new(error_message:
  impl Into<String>) -> Self`. Task 2 (`DeciderBackend`) and Task 4 (`DecisionReviewerNode`)
  consume all of these exactly as named here.

- [ ] **Step 1: Add `ta-ask` to the workspace members list**

Open `Cargo.toml` at the repo root and add `"crates/ta-ask"` to the `members` array (alongside
the existing `"crates/ta-workflow"` entry — insert it anywhere in that list, alphabetical
position isn't enforced elsewhere in this file).

- [ ] **Step 2: Write `crates/ta-ask/Cargo.toml`**

```toml
[package]
name = "ta-ask"
version.workspace = true
edition.workspace = true
description = "A narrow, bounded-question decision primitive: ask(question, context, schema) -> {result, confidence}. Private, in-tree for now — not extracted to a public repo."

[dependencies]
thiserror = { workspace = true }
```

- [ ] **Step 3: Write the failing tests in `crates/ta-ask/src/lib.rs`**

```rust
//! `ta-ask` — a narrow, bounded-question decision primitive.
//!
//! Status: private, in-tree only, as of 2026-10-04. Unlike `decision-gate`,
//! `consensus-panel`, and `task-graph`, this crate has NOT been extracted to
//! a standalone public repo — a future session should not assume it has
//! been, and should not extract it without the user's explicit direction.
//! See `docs/superpowers/specs/2026-10-04-local-decision-model-primitive-design.md`
//! (on branch `docs/local-decision-model-primitive-design`, not yet merged
//! to `main` as of this writing) for the full design rationale.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_backend_returns_configured_response() {
        let backend = FixtureBackend::yes_no(true, 0.9);
        let response =
            ask(&backend, "is this safe?", "some context", DecisionSchema::YesNo).unwrap();
        assert_eq!(response.result, DecisionResult::Bool(true));
        assert_eq!(response.confidence, 0.9);
    }

    #[test]
    fn error_fixture_backend_returns_configured_error() {
        let backend = ErrorFixtureBackend::new("boom");
        let err = ask(&backend, "q", "c", DecisionSchema::YesNo).unwrap_err();
        assert!(matches!(err, DecisionError::MalformedResponse(ref msg) if msg == "boom"));
    }

    #[test]
    fn ask_builds_the_request_the_backend_receives() {
        struct CapturingBackend {
            captured: std::sync::Mutex<Option<DecisionRequest>>,
        }
        impl DecisionBackend for CapturingBackend {
            fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
                *self.captured.lock().unwrap() = Some(req.clone());
                Ok(DecisionResponse {
                    result: DecisionResult::Bool(true),
                    confidence: 1.0,
                    model_id: "capturing".to_string(),
                    latency_ms: 0,
                })
            }
        }
        let backend = CapturingBackend {
            captured: std::sync::Mutex::new(None),
        };
        ask(
            &backend,
            "question text",
            "context text",
            DecisionSchema::Choice(vec!["a".to_string(), "b".to_string()]),
        )
        .unwrap();
        let captured = backend.captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.question, "question text");
        assert_eq!(captured.context, "context text");
        assert_eq!(
            captured.schema,
            DecisionSchema::Choice(vec!["a".to_string(), "b".to_string()])
        );
    }
}
```

- [ ] **Step 4: Run tests to verify they fail (nothing is defined yet)**

Run: `./dev cargo test -p ta-ask`
Expected: compile error — `DecisionRequest`, `FixtureBackend`, `ask`, etc. are not defined.

- [ ] **Step 5: Implement the core types above the test module**

```rust
// crates/ta-ask/src/lib.rs — place above the `#[cfg(test)]` module from Step 3

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    pub question: String,
    pub context: String,
    pub schema: DecisionSchema,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecisionSchema {
    YesNo,
    Choice(Vec<String>),
    Score { min: f64, max: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecisionResult {
    Bool(bool),
    Choice(String),
    Score(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionResponse {
    pub result: DecisionResult,
    pub confidence: f64,
    pub model_id: String,
    pub latency_ms: u64,
}

/// Every variant states what happened, what was attempted, and what to do
/// about it, per this repo's Observability Mandate.
#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error("backend does not support schema {0:?} yet -- use YesNo or Choice instead")]
    UnsupportedSchema(DecisionSchema),
    #[error(
        "failed to spawn backend server (command: '{command}'): {source} -- is the Python \
         environment set up? see crates/ta-ask/README.md"
    )]
    SpawnFailed {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "backend server at {url} did not become healthy within {timeout_secs}s -- is the \
         Python environment set up and the model downloaded? see crates/ta-ask/README.md"
    )]
    BackendUnavailable { url: String, timeout_secs: u64 },
    #[error("request to backend failed: {0}")]
    RequestFailed(#[source] reqwest::Error),
    #[error("backend returned a malformed response: {0}")]
    MalformedResponse(String),
}

/// Pluggable decision-answering backend. Implementations must be safe to
/// share across threads (`Send + Sync`) since a single backend instance is
/// typically held by an `Arc` and reused across many `ask()` calls.
pub trait DecisionBackend: Send + Sync {
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError>;
}

/// Ask `backend` a single bounded question. Thin by design: constructs a
/// `DecisionRequest` and calls straight through to `backend.decide()` — all
/// the real logic lives in the backend implementation.
pub fn ask(
    backend: &dyn DecisionBackend,
    question: impl Into<String>,
    context: impl Into<String>,
    schema: DecisionSchema,
) -> Result<DecisionResponse, DecisionError> {
    let req = DecisionRequest {
        question: question.into(),
        context: context.into(),
        schema,
    };
    backend.decide(&req)
}

/// Deterministic backend returning a pre-configured response, for tests
/// that don't need (and must not require) a real model or network call.
pub struct FixtureBackend {
    response: DecisionResponse,
}

impl FixtureBackend {
    pub fn new(response: DecisionResponse) -> Self {
        Self { response }
    }

    /// Convenience constructor for the common yes/no case.
    pub fn yes_no(result: bool, confidence: f64) -> Self {
        Self::new(DecisionResponse {
            result: DecisionResult::Bool(result),
            confidence,
            model_id: "fixture".to_string(),
            latency_ms: 0,
        })
    }
}

impl DecisionBackend for FixtureBackend {
    fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        Ok(self.response.clone())
    }
}

/// Deterministic backend that always errors with a configured message, for
/// testing a caller's error-handling path without a real failing backend.
pub struct ErrorFixtureBackend {
    error_message: String,
}

impl ErrorFixtureBackend {
    pub fn new(error_message: impl Into<String>) -> Self {
        Self {
            error_message: error_message.into(),
        }
    }
}

impl DecisionBackend for ErrorFixtureBackend {
    fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        Err(DecisionError::MalformedResponse(self.error_message.clone()))
    }
}
```

- [ ] **Step 6: Add `reqwest` as a dependency (needed for `DecisionError::RequestFailed`'s
  `reqwest::Error` field, even though Task 1 doesn't make any HTTP calls itself)**

```toml
# crates/ta-ask/Cargo.toml — add to [dependencies]
reqwest = { workspace = true }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `./dev cargo test -p ta-ask`
Expected: 3 passed (`fixture_backend_returns_configured_response`,
`error_fixture_backend_returns_configured_error`, `ask_builds_the_request_the_backend_receives`);
0 failed.

- [ ] **Step 8: Build the whole workspace to confirm the new crate integrates cleanly**

Run: `./dev cargo build --workspace`
Expected: success, no errors.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/ta-ask
git commit -m "Add ta-ask crate: core decision-primitive types, FixtureBackend, ask()"
```

---

## Task 2: `DeciderBackend` — subprocess lifecycle + HTTP client

**Files:**
- Create: `crates/ta-ask/src/decider_backend.rs`
- Modify: `crates/ta-ask/src/lib.rs` (add `pub mod decider_backend; pub use
  decider_backend::DeciderBackend;`)
- Modify: `crates/ta-ask/Cargo.toml` (add `serde`, `serde_json`, dev-deps `wiremock`, `tokio`,
  `tempfile`)

**Interfaces:**
- Consumes: `DecisionRequest`/`DecisionSchema`/`DecisionResult`/`DecisionResponse`/
  `DecisionError`/`DecisionBackend` from Task 1.
- Produces: `pub struct DeciderBackend` with `pub fn spawn(serve_script: &Path, model_id: &str,
  port: u16, health_timeout: Duration) -> Result<Self, DecisionError>` and `pub fn connect(base_url:
  impl Into<String>, model_id: impl Into<String>) -> Self`, implementing `DecisionBackend`. Task 3
  consumes both constructors; Task 4 consumes `DeciderBackend` only by its `DecisionBackend` trait
  impl (via `Arc<dyn DecisionBackend>`), not by name.

The request/response JSON shape below is this task's best-grounded hypothesis from Decider's
documented Python API (`d.decide(context: str, questions: list[dict]) -> list[dict]`, each
question `{"question": str, "options": [str, ...]}`, each result `{"choice": str, "confidence":
float, "probs": {...}}`) — Decider's README describes its HTTP server as exposing the "same
request/response shape... as JSON over HTTP" but does not show the exact HTTP body verbatim. If
Task 3's real server run shows a different shape, **fix the structs below to match the real
server, then re-run both this task's `wiremock` tests (updated to match) and Task 3's real test
until both pass** — this is expected, verifiable, and not a plan failure; it is exactly why Task 3
exists before this crate is considered done.

- [ ] **Step 1: Add the new dependencies**

```toml
# crates/ta-ask/Cargo.toml — full file after this step
[package]
name = "ta-ask"
version.workspace = true
edition.workspace = true
description = "A narrow, bounded-question decision primitive: ask(question, context, schema) -> {result, confidence}. Private, in-tree for now — not extracted to a public repo."

[dependencies]
thiserror = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
wiremock = "0.6"
tokio = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Write the failing tests in `crates/ta-ask/src/decider_backend.rs`**

```rust
// crates/ta-ask/src/decider_backend.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecisionError, DecisionRequest, DecisionResult, DecisionSchema};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn backend_for(server: &MockServer) -> DeciderBackend {
        DeciderBackend::connect(server.uri(), "decider-4b-test")
    }

    #[tokio::test]
    async fn decide_maps_yes_no_schema_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"choice": "yes", "confidence": 0.93, "probs": {"yes": 0.93, "no": 0.07}}
            ])))
            .mount(&server)
            .await;

        let backend = backend_for(&server);
        let response = backend
            .decide(&DecisionRequest {
                question: "Does this diff look safe to commit?".to_string(),
                context: "3 files changed, 12 lines".to_string(),
                schema: DecisionSchema::YesNo,
            })
            .expect("decide should succeed against the mocked server");

        assert_eq!(response.result, DecisionResult::Bool(true));
        assert_eq!(response.confidence, 0.93);
        assert_eq!(response.model_id, "decider-4b-test");
    }

    #[tokio::test]
    async fn decide_maps_choice_schema() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"choice": "billing", "confidence": 0.97, "probs": {"billing": 0.97, "technical": 0.02, "sales": 0.01}}
            ])))
            .mount(&server)
            .await;

        let backend = backend_for(&server);
        let response = backend
            .decide(&DecisionRequest {
                question: "Which department should handle this?".to_string(),
                context: "customer was charged twice".to_string(),
                schema: DecisionSchema::Choice(vec![
                    "billing".to_string(),
                    "technical".to_string(),
                    "sales".to_string(),
                ]),
            })
            .expect("decide should succeed against the mocked server");

        assert_eq!(
            response.result,
            DecisionResult::Choice("billing".to_string())
        );
    }

    #[tokio::test]
    async fn decide_rejects_score_schema_without_calling_the_server() {
        // No Mock registered -- if decide() ever made an HTTP call for a
        // Score schema, wiremock would fail this test on the unexpected
        // request, proving the rejection happens before any network call.
        let server = MockServer::start().await;
        let backend = backend_for(&server);
        let err = backend
            .decide(&DecisionRequest {
                question: "how risky is this?".to_string(),
                context: "...".to_string(),
                schema: DecisionSchema::Score { min: 0.0, max: 1.0 },
            })
            .unwrap_err();
        assert!(matches!(err, DecisionError::UnsupportedSchema(_)));
    }

    #[tokio::test]
    async fn decide_errors_clearly_on_malformed_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let backend = backend_for(&server);
        let err = backend
            .decide(&DecisionRequest {
                question: "q".to_string(),
                context: "c".to_string(),
                schema: DecisionSchema::YesNo,
            })
            .unwrap_err();
        assert!(matches!(err, DecisionError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn decide_errors_clearly_on_empty_result_array() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let backend = backend_for(&server);
        let err = backend
            .decide(&DecisionRequest {
                question: "q".to_string(),
                context: "c".to_string(),
                schema: DecisionSchema::YesNo,
            })
            .unwrap_err();
        assert!(matches!(err, DecisionError::MalformedResponse(_)));
    }

    #[test]
    fn spawn_errors_clearly_when_the_script_does_not_exist() {
        let err = DeciderBackend::spawn(
            std::path::Path::new("/definitely/not/a/real/script.sh"),
            "decider-4b",
            18237,
            std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, DecisionError::SpawnFailed { .. }));
    }

    #[test]
    fn spawn_errors_clearly_when_health_check_never_succeeds() {
        // A script that starts but never serves /health -- exercises the
        // timeout path without needing the real model or a real server.
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("never_healthy.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let err = DeciderBackend::spawn(
            &script_path,
            "decider-4b",
            18238,
            std::time::Duration::from_millis(800),
        )
        .unwrap_err();
        assert!(matches!(err, DecisionError::BackendUnavailable { .. }));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail (nothing is defined yet)**

Run: `./dev cargo test -p ta-ask`
Expected: compile error — `DeciderBackend` is not defined.

- [ ] **Step 4: Implement `DeciderBackend` above the test module**

```rust
// crates/ta-ask/src/decider_backend.rs — place above the `#[cfg(test)]` module from Step 2

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{DecisionBackend, DecisionError, DecisionRequest, DecisionResponse, DecisionResult, DecisionSchema};

#[derive(serde::Serialize)]
struct DecideRequestBody {
    context: String,
    questions: Vec<QuestionBody>,
}

#[derive(serde::Serialize)]
struct QuestionBody {
    question: String,
    options: Vec<String>,
}

#[derive(serde::Deserialize, Debug)]
struct DecideResponseItem {
    choice: String,
    confidence: f64,
    #[allow(dead_code)]
    probs: std::collections::HashMap<String, f64>,
}

/// Owns the spawned `scripts/serve.sh` child process, if any. `None` when
/// this backend was built via `connect()` against a server it doesn't own
/// (tests, or a caller managing the process itself) -- `Drop` then has
/// nothing to kill.
struct ServerProcess {
    child: Child,
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `DecisionBackend` backed by Decider-4b's own HTTP server
/// (github.com/Mapika/decider, `scripts/serve.sh`). Supports `YesNo` and
/// `Choice` schemas; `Score` returns `DecisionError::UnsupportedSchema` --
/// see this crate's `README.md` for why.
pub struct DeciderBackend {
    base_url: String,
    client: reqwest::blocking::Client,
    model_id: String,
    server: Mutex<Option<ServerProcess>>,
}

impl DeciderBackend {
    fn new_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("building a reqwest client with static, valid config cannot fail")
    }

    /// Spawn `serve_script <model_id> <port>` (Decider's own
    /// `scripts/serve.sh`) and wait for it to report healthy before
    /// returning. The model takes 13-21s to load per
    /// `docs/superpowers/specs/2026-10-04-local-decision-model-evaluation.md`'s
    /// own measurements, so this polls `GET /health` every 500ms until
    /// `health_timeout` elapses, then gives up with a clear, actionable
    /// error rather than hanging forever.
    pub fn spawn(
        serve_script: &Path,
        model_id: &str,
        port: u16,
        health_timeout: Duration,
    ) -> Result<Self, DecisionError> {
        let child = Command::new(serve_script)
            .arg(model_id)
            .arg(port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| DecisionError::SpawnFailed {
                command: format!("{} {} {}", serve_script.display(), model_id, port),
                source: e,
            })?;

        let base_url = format!("http://127.0.0.1:{port}");
        let client = Self::new_client();
        let health_url = format!("{base_url}/health");
        let deadline = Instant::now() + health_timeout;
        loop {
            if let Ok(resp) = client.get(&health_url).send() {
                if resp.status().is_success() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(DecisionError::BackendUnavailable {
                    url: health_url,
                    timeout_secs: health_timeout.as_secs(),
                });
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        Ok(Self {
            base_url,
            client,
            model_id: model_id.to_string(),
            server: Mutex::new(Some(ServerProcess { child })),
        })
    }

    /// Point at an already-running server this backend doesn't own --
    /// used by this module's own tests (against a `wiremock` server) and
    /// available to any caller that manages the Decider server process
    /// itself.
    pub fn connect(base_url: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: Self::new_client(),
            model_id: model_id.into(),
            server: Mutex::new(None),
        }
    }
}

impl DecisionBackend for DeciderBackend {
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let options = match &req.schema {
            DecisionSchema::YesNo => vec!["yes".to_string(), "no".to_string()],
            DecisionSchema::Choice(options) => options.clone(),
            DecisionSchema::Score { .. } => {
                return Err(DecisionError::UnsupportedSchema(req.schema.clone()));
            }
        };

        let body = DecideRequestBody {
            context: req.context.clone(),
            questions: vec![QuestionBody {
                question: req.question.clone(),
                options,
            }],
        };

        let start = Instant::now();
        let response = self
            .client
            .post(format!("{}/decide", self.base_url))
            .json(&body)
            .send()
            .map_err(DecisionError::RequestFailed)?;
        let latency_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            return Err(DecisionError::MalformedResponse(format!(
                "server returned HTTP {}",
                response.status()
            )));
        }

        let items: Vec<DecideResponseItem> = response
            .json()
            .map_err(|e| DecisionError::MalformedResponse(e.to_string()))?;

        let item = items.into_iter().next().ok_or_else(|| {
            DecisionError::MalformedResponse("server returned an empty result array".to_string())
        })?;

        let result = match &req.schema {
            DecisionSchema::YesNo => match item.choice.as_str() {
                "yes" => DecisionResult::Bool(true),
                "no" => DecisionResult::Bool(false),
                other => {
                    return Err(DecisionError::MalformedResponse(format!(
                        "expected 'yes' or 'no' for a YesNo schema, got '{}'",
                        other
                    )));
                }
            },
            DecisionSchema::Choice(_) => DecisionResult::Choice(item.choice),
            DecisionSchema::Score { .. } => unreachable!("Score schema already rejected above"),
        };

        Ok(DecisionResponse {
            result,
            confidence: item.confidence,
            model_id: self.model_id.clone(),
            latency_ms,
        })
    }
}
```

- [ ] **Step 5: Wire the module into `lib.rs`**

```rust
// crates/ta-ask/src/lib.rs — add these two lines
pub mod decider_backend;
pub use decider_backend::DeciderBackend;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `./dev cargo test -p ta-ask`
Expected: 10 passed (3 from Task 1 + 7 here: `decide_maps_yes_no_schema_and_parses_response`,
`decide_maps_choice_schema`, `decide_rejects_score_schema_without_calling_the_server`,
`decide_errors_clearly_on_malformed_response`, `decide_errors_clearly_on_empty_result_array`,
`spawn_errors_clearly_when_the_script_does_not_exist`,
`spawn_errors_clearly_when_health_check_never_succeeds`); 0 failed.

- [ ] **Step 7: Full workspace build**

Run: `./dev cargo build --workspace`
Expected: success, no errors.

- [ ] **Step 8: Commit**

```bash
git add crates/ta-ask
git commit -m "Add DeciderBackend: subprocess lifecycle + HTTP client, wiremock-tested"
```

---

## Task 3: Real end-to-end validation against the actual Decider-4b server

**Files:** none created/modified except a new `#[ignore]`-gated test appended to
`crates/ta-ask/src/decider_backend.rs`'s existing test module.

**Interfaces:** none new — this task proves Task 2's `DeciderBackend` actually works against the
real thing, and fixes Task 2's code if the real server's wire format differs from what was
assumed.

- [ ] **Step 1: Set up a Python environment for Decider-4b**

```bash
python3 -m venv /tmp/ta-ask-decider-env
source /tmp/ta-ask-decider-env/bin/activate
pip install --upgrade pip
pip install 'decider-ai[metal]'
```

Before running the `pip install` line, confirm the package name and `metal` extra are still
current by checking https://pypi.org/project/decider-ai/ directly (or `pip index versions
decider-ai`) — if the real current install command differs from what's written here, use the real
one and note the discrepancy in this task's report.

- [ ] **Step 2: Get `scripts/serve.sh` by cloning Decider's actual repo**

```bash
git clone https://github.com/Mapika/decider /tmp/decider-repo
```

`scripts/serve.sh` is documented in the repo's own README as the way to start its HTTP server; it
is not necessarily bundled inside the `decider-ai` PyPI wheel, so clone the repo to get it rather
than assuming otherwise.

- [ ] **Step 3: Manually verify the server starts and reports healthy**

```bash
source /tmp/ta-ask-decider-env/bin/activate
/tmp/decider-repo/scripts/serve.sh decider-4b 8700
```

In a second terminal, once you see a "model loaded"-style message (or after waiting ~20s):

```bash
curl -s http://127.0.0.1:8700/health
```

Expected: an HTTP 200 response (exact body doesn't matter for this check). Record the real output
in this task's report. If this step fails, troubleshoot the Python environment/model download
before proceeding — do not move to Step 4 against a server that isn't actually healthy.

- [ ] **Step 4: Manually verify the real `/decide` wire shape with `curl`, before trusting Task 2's
  assumed struct shapes**

```bash
curl -s -X POST http://127.0.0.1:8700/decide \
  -H "Content-Type: application/json" \
  -d '{"context": "We should use PostgreSQL for this project because it has the best support for our use case among the three options we considered.", "questions": [{"question": "Does this draft clearly recommend one specific option over the others, rather than just listing them neutrally?", "options": ["yes", "no"]}]}'
```

Record the real raw JSON response in this task's report. Compare it against Task 2's
`DecideRequestBody`/`DecideResponseItem` structs (`crates/ta-ask/src/decider_backend.rs`). If the
real field names or shape differ (e.g. a wrapper object instead of a bare array, a different key
than `"choice"`/`"confidence"`/`"probs"`), **fix those structs now** to match reality, then re-run
`./dev cargo test -p ta-ask` to confirm Task 2's `wiremock` tests still pass against the corrected
shape (updating the mocked JSON bodies in those tests to match the real shape too, if they were
wrong) before continuing.

- [ ] **Step 5: Write the real, `#[ignore]`-gated integration test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/ta-ask/src/decider_backend.rs`:

```rust
    #[test]
    #[ignore = "requires a running decider-4b server at 127.0.0.1:8700 -- see crates/ta-ask/README.md"]
    fn real_decider_backend_answers_a_real_question() {
        let backend = DeciderBackend::connect("http://127.0.0.1:8700", "decider-4b");
        let response = backend
            .decide(&DecisionRequest {
                question: "Does this draft clearly recommend one specific option over the \
                            others, rather than just listing them neutrally?"
                    .to_string(),
                context: "We should use PostgreSQL for this project because it has the best \
                           support for our use case among the three options we considered."
                    .to_string(),
                schema: DecisionSchema::YesNo,
            })
            .expect("real decider-4b server should answer this question");

        println!("REAL DECIDER RESPONSE: {:?}", response);
        assert!(response.confidence > 0.0 && response.confidence <= 1.0);
        assert!(matches!(response.result, DecisionResult::Bool(_)));
    }
```

- [ ] **Step 6: Actually run it, with the real server from Step 3 still running, and capture the
  real output**

Run: `./dev cargo test -p ta-ask real_decider_backend_answers_a_real_question -- --ignored --nocapture`
Expected: PASS, with a `REAL DECIDER RESPONSE: DecisionResponse { ... }` line printed showing a
real `result`/`confidence`/`model_id`/`latency_ms`. **Paste this real output into this task's
report.** If it fails, do not mark this task done — fix `DeciderBackend` (per Step 4's comparison)
or the environment setup until it genuinely passes against the real server.

- [ ] **Step 7: Shut down the manually-started server from Step 3**

```bash
# In the terminal running scripts/serve.sh:
# Ctrl-C
```

- [ ] **Step 8: Run the full non-ignored test suite once more to confirm nothing broke**

Run: `./dev cargo test -p ta-ask`
Expected: same 10 tests from Task 2 still pass (the new test from this task is `#[ignore]`d, so it
doesn't run here — that's correct, it requires the real server).

- [ ] **Step 9: Commit**

```bash
git add crates/ta-ask/src/decider_backend.rs
git commit -m "Add real end-to-end test against a live decider-4b server (run manually, #[ignore]'d in CI)

Verified the real HTTP wire shape against a genuinely running server --
see this commit's PR description / task report for the captured real
output and whether DeciderBackend's structs needed adjustment."
```

(If Step 4 required fixing `DeciderBackend`'s structs, that fix is already staged from Task 2's
working tree changes — include it in this same commit rather than a separate one, since Task 2's
commit already landed and this is a direct correction discovered by this task.)

---

## Task 4: `ta-workflow` integration — `DecisionReviewerNode`

**Files:**
- Create: `crates/ta-workflow/src/graph/nodes/decision_reviewer.rs`
- Modify: `crates/ta-workflow/src/graph/nodes/mod.rs`
- Modify: `crates/ta-workflow/src/graph/registry.rs`
- Modify: `crates/ta-workflow/Cargo.toml` (add `ta-ask` path dependency)

**Interfaces:**
- Consumes: `ta_ask::{ask, DecisionBackend, DecisionSchema, DecisionResult, DecisionError,
  DecisionRequest, DecisionResponse}` (Tasks 1-2); `ta_workflow::graph::types::{GraphContext,
  GraphError, ReviewInput, ReviewerNode, ReviewerVote}` (pre-existing, confirmed at
  `crates/ta-workflow/src/graph/types.rs`).
- Produces: `pub struct DecisionReviewerNode` with `pub fn new(backend:
  std::sync::Arc<dyn ta_ask::DecisionBackend>) -> Self`, implementing `ReviewerNode`; `pub fn
  NodeRegistry::register_decision_reviewer(&mut self, backend: std::sync::Arc<dyn
  ta_ask::DecisionBackend>)`.

- [ ] **Step 1: Add `ta-ask` as a dependency of `ta-workflow`**

```toml
# crates/ta-workflow/Cargo.toml — add to [dependencies], matching the existing
# path-dependency style already used for ta-changeset/ta-policy in this same file
ta-ask = { path = "../ta-ask", version = "0.17.11-alpha.8" }
```

- [ ] **Step 2: Write the failing tests in `crates/ta-workflow/src/graph/nodes/decision_reviewer.rs`**

```rust
// graph/nodes/decision_reviewer.rs — `DecisionReviewerNode`.
//
// Wraps `ta_ask::ask()` as one scored `ReviewerVote`, filling the
// consensus-panel-reviewer and decision-gate-input call sites named in
// docs/superpowers/specs/2026-10-04-local-decision-model-primitive-design.md
// §1's table ("Does this diff look safe to commit?"). Registered opt-in via
// `NodeRegistry::register_decision_reviewer`, not `with_builtins()` -- see
// that method's doc comment for why.

#[cfg(test)]
mod tests {
    use super::*;
    use ta_ask::{DecisionError, DecisionRequest, DecisionResponse, DecisionResult};

    struct YesBackend;
    impl ta_ask::DecisionBackend for YesBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Ok(DecisionResponse {
                result: DecisionResult::Bool(true),
                confidence: 0.92,
                model_id: "test-yes".to_string(),
                latency_ms: 1,
            })
        }
    }

    struct NoBackend;
    impl ta_ask::DecisionBackend for NoBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Ok(DecisionResponse {
                result: DecisionResult::Bool(false),
                confidence: 0.81,
                model_id: "test-no".to_string(),
                latency_ms: 1,
            })
        }
    }

    struct ErrorBackend;
    impl ta_ask::DecisionBackend for ErrorBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Err(DecisionError::MalformedResponse("boom".to_string()))
        }
    }

    fn input() -> crate::graph::types::ReviewInput {
        crate::graph::types::ReviewInput {
            changed_paths: vec!["src/main.rs".to_string()],
            lines_changed: 5,
            agent_id: "claude-code".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn confident_yes_scores_high() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(YesBackend));
        let vote = node.review(&input(), &ctx).unwrap();
        assert_eq!(vote.role, "decision");
        assert_eq!(vote.score, 0.92);
    }

    #[test]
    fn confident_no_scores_low() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(NoBackend));
        let vote = node.review(&input(), &ctx).unwrap();
        assert_eq!(vote.score, 1.0 - 0.81);
    }

    #[test]
    fn backend_error_produces_a_clear_graph_error() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(ErrorBackend));
        let err = node.review(&input(), &ctx).unwrap_err();
        match err {
            crate::graph::types::GraphError::NodeExecution { node_id, message } => {
                assert_eq!(node_id, "decision");
                assert!(message.contains("boom"));
            }
            other => panic!("expected GraphError::NodeExecution, got {:?}", other),
        }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `./dev cargo test -p ta-workflow decision_reviewer`
Expected: compile error — `DecisionReviewerNode` is not defined, and the module doesn't exist yet
(so this exact command may instead report "no tests ran" until the module is registered in Step
5 — if so, proceed to Step 4 and re-run after Step 5).

- [ ] **Step 4: Implement `DecisionReviewerNode` above the test module**

```rust
// crates/ta-workflow/src/graph/nodes/decision_reviewer.rs — place the comment
// header from Step 2 at the top of the file, then this, then the test module

use crate::graph::types::{GraphContext, GraphError, ReviewInput, ReviewerNode, ReviewerVote};

/// Asks a decision-model backend "does this diff look safe to commit?",
/// framed with the changed-paths/lines-changed/agent-id already carried on
/// `ReviewInput`, and converts the yes/no answer plus its calibrated
/// confidence into a `ReviewerVote`. `score` is the confidence in the
/// direction of the answer actually given: a confident "yes" scores near
/// 1.0, a confident "no" scores near 0.0 (i.e. `1.0 - confidence`), so
/// `WeightedDecisionNode`'s existing threshold logic can treat this vote
/// identically to any other reviewer's without new special-casing.
pub struct DecisionReviewerNode {
    backend: std::sync::Arc<dyn ta_ask::DecisionBackend>,
}

impl DecisionReviewerNode {
    pub fn new(backend: std::sync::Arc<dyn ta_ask::DecisionBackend>) -> Self {
        Self { backend }
    }
}

impl ReviewerNode for DecisionReviewerNode {
    fn review(&self, input: &ReviewInput, _ctx: &GraphContext) -> Result<ReviewerVote, GraphError> {
        let context = format!(
            "agent: {}\nchanged paths ({}): {}\nlines changed: {}",
            input.agent_id,
            input.changed_paths.len(),
            input.changed_paths.join(", "),
            input.lines_changed,
        );

        let response = ta_ask::ask(
            self.backend.as_ref(),
            "Does this diff look safe to commit?",
            context,
            ta_ask::DecisionSchema::YesNo,
        )
        .map_err(|e| GraphError::NodeExecution {
            node_id: "decision".to_string(),
            message: format!("ta-ask backend call failed: {}", e),
        })?;

        let is_safe = matches!(response.result, ta_ask::DecisionResult::Bool(true));
        let finding = format!(
            "decision_reviewer: model={} confidence={:.2} answer={}",
            response.model_id, response.confidence, is_safe
        );

        Ok(ReviewerVote {
            role: "decision".to_string(),
            score: if is_safe {
                response.confidence
            } else {
                1.0 - response.confidence
            },
            findings: vec![finding],
            timed_out: false,
        })
    }
}
```

- [ ] **Step 5: Wire the module into `crates/ta-workflow/src/graph/nodes/mod.rs`**

```rust
// crates/ta-workflow/src/graph/nodes/mod.rs — full file after this change
// graph/nodes/mod.rs — Built-in node implementations shippable from
// `ta-workflow` itself (v0.17.7.1). `GoalDispatchAction`, `AutoApproveAction`,
// and `RecommendAction` need `ta-brain`/`ta-goal`/the real draft-apply path
// and are therefore implemented one layer up, in `apps/ta-cli`, and
// registered into a `NodeRegistry` alongside these built-ins — see
// `graph/registry.rs`'s module doc comment for why.

mod advisor_confidence_reviewer;
mod decision_reviewer;
mod policy_reviewer;
mod weighted_decision;

pub use advisor_confidence_reviewer::AdvisorConfidenceReviewer;
pub use decision_reviewer::DecisionReviewerNode;
pub use policy_reviewer::PolicyReviewer;
pub use weighted_decision::WeightedDecisionNode;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `./dev cargo test -p ta-workflow decision_reviewer`
Expected: 3 passed (`confident_yes_scores_high`, `confident_no_scores_low`,
`backend_error_produces_a_clear_graph_error`); 0 failed.

- [ ] **Step 7: Add the opt-in registration method to `NodeRegistry`**

In `crates/ta-workflow/src/graph/registry.rs`, add this method inside `impl NodeRegistry` (place
it after the existing `register_reviewer` method, around line 86):

```rust
    /// Registers `"decision"` → `DecisionReviewerNode`, backed by `backend`.
    /// Deliberately NOT part of `with_builtins()` -- unlike `advisor_confidence`/
    /// `policy`, this needs a live `ta_ask::DecisionBackend` instance (typically
    /// a `DeciderBackend` managing a real subprocess), which a caller must
    /// construct and own; `ta-workflow` itself has no opinion on which backend
    /// or how it's configured. Call this explicitly, after `with_builtins()`,
    /// once a backend is available. Takes an `Arc` (not `Box`) because the
    /// underlying `Fn` factory closure may be invoked more than once (once per
    /// graph run resolving this node kind) and `DecisionBackend` isn't `Clone`
    /// (a `DeciderBackend` owns an unclonable child-process handle) -- `Arc`
    /// lets each invocation cheaply clone a shared reference to the same
    /// backend instance instead.
    pub fn register_decision_reviewer(&mut self, backend: std::sync::Arc<dyn ta_ask::DecisionBackend>) {
        self.register_reviewer("decision", move |_def| {
            Ok(Box::new(super::nodes::DecisionReviewerNode::new(backend.clone()))
                as Box<dyn ReviewerNode>)
        });
    }
```

- [ ] **Step 8: Write a test for the registration method itself**

Add to `crates/ta-workflow/src/graph/registry.rs`'s existing `#[cfg(test)] mod tests` block (the
one already containing `with_builtins_registers_policy_advisor_and_weighted`):

```rust
    #[test]
    fn register_decision_reviewer_adds_the_decision_kind() {
        struct AlwaysYes;
        impl ta_ask::DecisionBackend for AlwaysYes {
            fn decide(
                &self,
                _req: &ta_ask::DecisionRequest,
            ) -> Result<ta_ask::DecisionResponse, ta_ask::DecisionError> {
                Ok(ta_ask::DecisionResponse {
                    result: ta_ask::DecisionResult::Bool(true),
                    confidence: 1.0,
                    model_id: "always-yes".to_string(),
                    latency_ms: 0,
                })
            }
        }

        let mut registry = NodeRegistry::with_builtins();
        registry.register_decision_reviewer(std::sync::Arc::new(AlwaysYes));

        let reviewer_factory = registry
            .reviewers
            .get("decision")
            .expect("register_decision_reviewer should add the 'decision' kind");
        let node_def = super::schema::NodeDef::default();
        let reviewer = reviewer_factory(&node_def).expect("factory should succeed");

        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let vote = reviewer
            .review(&crate::graph::types::ReviewInput::default(), &ctx)
            .unwrap();
        assert_eq!(vote.role, "decision");
        assert_eq!(vote.score, 1.0);
    }
```

If `NodeRegistry`'s `reviewers` field is private to the module and not visible from the test
module the way this snippet assumes, add `#[cfg(test)] pub(crate) fn reviewers_for_test(&self) ->
&HashMap<String, ReviewerFactory>` (or adjust the test to call `registry.register_decision_reviewer(...)`
then build a graph definition that uses kind `"decision"` and runs it through the existing
`run_graph` test helper instead, matching `engine.rs`'s own test style) — whichever requires the
smaller change given the actual current visibility of `NodeRegistry`'s fields; check
`crates/ta-workflow/src/graph/registry.rs`'s existing test module for how
`with_builtins_registers_policy_advisor_and_weighted` already asserts on registered kinds and
match that exact approach instead of guessing a new one.

- [ ] **Step 9: Run the full test suite to confirm everything passes**

Run: `./dev cargo test -p ta-workflow`
Expected: all `ta-workflow` tests pass, including the 3 from Step 6 and the 1 from Step 8, plus
every pre-existing test unaffected.

- [ ] **Step 10: Full workspace verification**

Run:
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```
Expected: all four pass cleanly. Fix any clippy/fmt issues (`./dev cargo fmt --all` then re-check)
before committing.

- [ ] **Step 11: Commit**

```bash
git add crates/ta-workflow
git commit -m "Wire DecisionReviewerNode into ta-workflow's reviewer chain, opt-in registration"
```

---

## Task 5: Docs

**Files:**
- Create: `crates/ta-ask/README.md`

**Interfaces:** none — documentation only.

- [ ] **Step 1: Write `crates/ta-ask/README.md`**

```markdown
# ta-ask

A narrow, bounded-question decision primitive: `ask(question, context, schema) -> {result,
confidence}`.

**Status: private, in-tree only, as of 2026-10-04.** Unlike `decision-gate`, `consensus-panel`,
and `task-graph`, this crate has NOT been extracted to a standalone public repo. Don't assume it
has been, and don't extract it without explicit direction — it's deliberately staying internal for
now.

## Why this exists

Five independent places across TA-core, Wayfinder, and Untollable converged on the same shape
(bounded question + context in, calibrated result out) rather than five divergent
reimplementations. See `docs/superpowers/specs/2026-10-04-local-decision-model-primitive-design.md`
and `docs/superpowers/specs/2026-10-04-local-decision-model-evaluation.md` (both on branch
`docs/local-decision-model-primitive-design` as of this writing) for the full design and model
evaluation.

## Interface is synchronous, not async

`DecisionBackend::decide` is a plain sync function, not `async fn`. This was a deliberate
correction from the design doc's original sketch: `ta-workflow`'s `ReviewerNode::review()` (the
first real consumer, see below) is sync with no async anywhere in its graph engine, and
`reqwest`'s `blocking` feature is already enabled workspace-wide — making this sync means zero
runtime-bridging code at the integration point. A future async caller can wrap a sync `ask()` call
in `tokio::task::spawn_blocking`, which is the standard, safe direction for calling blocking code
from async Rust (the reverse — sync code calling async — is the fragile direction).

## Backends

- `FixtureBackend`/`ErrorFixtureBackend` — deterministic, for tests. No network, no subprocess.
- `DeciderBackend` — the real backend, wrapping Decider-4b (Mapika, Apache 2.0,
  github.com/Mapika/decider). Spawns and keeps warm Decider's own HTTP server
  (`scripts/serve.sh <model> <port>`) rather than a custom stdio protocol — the model takes 13-21s
  to load, so a long-lived process is required; Decider ships `/health` and `/decide` endpoints
  specifically for this kind of integration.

### Running `DeciderBackend` for real

```bash
python3 -m venv /tmp/ta-ask-decider-env
source /tmp/ta-ask-decider-env/bin/activate
pip install 'decider-ai[metal]'   # [metal] is the Apple Silicon MPS acceleration extra; check
                                   # pypi.org/project/decider-ai for the current package/extras
git clone https://github.com/Mapika/decider /tmp/decider-repo
/tmp/decider-repo/scripts/serve.sh decider-4b 8700
```

Then `DeciderBackend::connect("http://127.0.0.1:8700", "decider-4b")`, or
`DeciderBackend::spawn(...)` to have Rust own the process lifecycle itself.

## Known limitation: `DecisionSchema::Score` is not supported by `DeciderBackend`

`DeciderBackend::decide` returns `DecisionError::UnsupportedSchema` for `Score` requests. No real
consumer needs it yet (both known near-term consumers are `YesNo`/`Choice`-shaped). Decider does
have a richer `system_one` API that supports scored/typed answers with discrete levels, not a
continuous `min..max` range — if `Score` support is ever needed, that's the API to map against,
not a client-side approximation layered on `decide()`.

## Consumers

- `ta-workflow`'s `DecisionReviewerNode` (`crates/ta-workflow/src/graph/nodes/decision_reviewer.rs`)
  — opt-in via `NodeRegistry::register_decision_reviewer`, not part of `with_builtins()`.
- Future (not yet built, tracked separately): Wayfinder's CoS chat-vs-governed classifier,
  Untollable's own chat-vs-governed classifier (explicitly specified to share this implementation,
  not just the design, with Wayfinder's), and a semantic-overlap detection signal feeding
  `task-graph`'s `impact_tags` from the caller side (never a `task-graph` dependency itself).
```

- [ ] **Step 2: Add the in-tree-status doc comment to `lib.rs` if not already present from Task 1**

Confirm the `//!` module doc comment written in Task 1 Step 3 is still at the top of
`crates/ta-ask/src/lib.rs` and still accurately states the private/in-tree status — it was written
before this README existed, so no change needed unless Task 1-4 drifted it; just verify.

- [ ] **Step 3: Final full verification**

Run:
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```
Expected: all four pass cleanly.

- [ ] **Step 4: Confirm the working tree is clean**

Run: `git status`
Expected: `nothing to commit, working tree clean` (after the commit in Step 5).

- [ ] **Step 5: Commit**

```bash
git add crates/ta-ask/README.md
git commit -m "Add ta-ask README: usage, sync rationale, Decider-4b setup, known Score-schema gap"
```

---

## Self-Review

**Spec coverage:** design doc §1 (five call sites) → this plan builds the primitive (Task 1) and
fills the "consensus-panel reviewer"/"decision-gate input" call sites in one implementation (Task
4), since `DecisionReviewerNode`'s `ReviewerVote` output feeds both `WeightedDecisionNode` directly
and, via the existing `decision_bridge` adapter already wired elsewhere, `decision-gate`'s
previously-empty input slot — no separate task needed for that second call site. §2 (interface) →
Task 1, with the sync correction from §2's original async sketch carried through consistently.
§3 (arbitration-as-composition) → correctly NOT built here; it's a caller-side pattern for a future
consumer (Wayfinder/Untollable), not part of this crate. §4 (task-graph placement) → correctly NOT
touched by this plan at all; the caller-side `impact_tags`-augmentation work belongs to whichever
future plan builds that specific feature, not this one. §5 (non-functional targets) → not directly
tested as explicit assertions in this plan (the real latency numbers come from the evaluation doc's
own benchmark, already measured), but Task 3's real smoke test reports actual `latency_ms` for a
sanity check. §6 (default backend) → `DeciderBackend` is the only backend this plan implements,
matching the empirically-resolved default. §7 (naming, registration) → `ta-ask`/opt-in-registration
both carried through exactly.

**Placeholder scan:** no TBD/TODO/"add error handling" patterns in task bodies. Step 8 of Task 4 is
the one place with a conditional ("if X, do Y; if Z, do W instead") rather than a single fixed
instruction — this is because `NodeRegistry`'s exact field visibility wasn't independently
re-verified at plan-writing time for test-access purposes; it names the exact fallback approach
(match `with_builtins_registers_policy_advisor_and_weighted`'s existing test style) rather than
leaving it open-ended, which is a real resolution path, not a placeholder.

**Type consistency check:** `DecisionBackend`'s `decide(&self, req: &DecisionRequest) ->
Result<DecisionResponse, DecisionError>` signature is identical across Task 1's trait definition,
Task 2's `DeciderBackend` impl and its tests, and Task 4's `DecisionReviewerNode`/test fixtures —
no drift. `ask()`'s signature (Task 1) matches its one call site (Task 4). `FixtureBackend`/
`ErrorFixtureBackend` (Task 1) are referenced by name consistently wherever cited. `DecisionSchema`/
`DecisionResult` variant names (`YesNo`/`Choice`/`Score`, `Bool`/`Choice`/`Score`) match across every
task that constructs or matches on them.
