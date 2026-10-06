# Chat-Mode Secure Launch Primitive: Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the already-merged `ta-ask` classifier and `ta-policy::chat_manifest` into a real, lightweight, securely-enforced chat session primitive inside `ta-mcp-gateway`, and close a real secrets-exposure gap found while designing it.

**Architecture:** A chat session is internally just a `GoalRun` whose manifest comes from `ta_policy::compile_chat_manifest()` instead of the developer profile. This reuses `ta-mcp-gateway`'s existing, already-tested `ta_fs_read`/`ta_fs_write`/`ta_fs_diff`/`ta_fs_list` tool handlers (`crates/ta-mcp-gateway/src/tools/fs.rs`) completely unchanged, since they already enforce `PolicyEngine::evaluate()` per call via `GatewayState::check_policy()`. Before building on that enforcement, `PolicyEngine::evaluate()` itself gets a new unconditional secrets-path backstop, since chat mode's broad `fs_read` grant would otherwise expose `.env`/credential files to any chat session.

**Tech Stack:** Rust, `ta-policy` (capability manifests, `PolicyEngine`), `ta-mcp-gateway` (MCP tool handlers, `GatewayState`), `ta-goal` (`GoalRun`), `ta-connectors-fs` (`FsConnector`, `StagingWorkspace`), `glob` (resource-pattern matching).

## Global Constraints

- Feature branches + PRs only; never commit directly to `main` (this plan's branch: `feature/chat-mode-secure-launch-phase1`).
- Before every commit, all four must pass, run through the Nix devShell: `./dev "cargo build --workspace"`, `./dev "cargo test --workspace"`, `./dev "cargo clippy --workspace --all-targets -- -D warnings"`, `./dev "cargo fmt --all -- --check"`.
- Observability Mandate: every error/deny path states what happened, what was being attempted, and what to do about it; never a bare "denied" or "failed."
- No bare `.unwrap()`/`.expect()` outside test code.
- Commit in logical units; run `git status` after each commit and confirm "nothing to commit, working tree clean."
- Never disable or skip tests. Run tests after every code change, before committing.
- Source of truth for this plan: `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`. Read in full before starting; cite it directly rather than re-deriving its reasoning.

**Out of scope for this plan** (per the design doc's Phase boundaries; do not add tasks for these): wiring `ta-virtual-team`'s poller to call `start_chat_session` and restrict CoS's tool surface (needs investigation of that separate private repo first); the OS-level sandbox backstop (Phase 2); agentic/LLM-driven red-team runs (user-triggered only, not built here).

---

### Task 1: Secrets-path backstop in `PolicyEngine::evaluate()`

**Files:**
- Modify: `crates/ta-policy/src/engine.rs` (both `evaluate()` around line 136-196 and `evaluate_with_trace()` around line 201-376. These are two independently-written, parallel implementations of the same check sequence; the backstop must be added to **both** or `evaluate_with_trace()` silently stays bypassable)
- Test: `crates/ta-policy/src/engine.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `glob::Pattern` (already imported in this file), the existing `matches_resource_pattern(pattern: &str, target: &str) -> bool` helper (line ~543).
- Produces: `fn matches_secret_path(target: &str) -> bool`. A private helper later tasks do not need to call directly (the backstop is internal to `evaluate`/`evaluate_with_trace`), but note its existence and exact name here so a reviewer checking Task 1 against Task 2 knows where the enforcement actually lives.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `crates/ta-policy/src/engine.rs` (after the existing `deny_path_traversal` test, so it sits next to its sibling backstop check):

```rust
    #[test]
    fn deny_secret_env_file_even_with_broadest_possible_grant() {
        let mut engine = PolicyEngine::new();
        engine.load_manifest(test_manifest(
            "agent-1",
            vec![grant("fs", "read", "fs://workspace/**")],
        ));

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "agent-1".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/.env".to_string(),
        });

        match decision {
            PolicyDecision::Deny { reason } => {
                assert!(reason.contains("secrets path"));
            }
            other => panic!("expected Deny, got {:?}", other),
        }
    }

    #[test]
    fn deny_credentials_directory_even_with_broadest_possible_grant() {
        let mut engine = PolicyEngine::new();
        engine.load_manifest(test_manifest(
            "agent-1",
            vec![grant("fs", "read", "fs://workspace/**")],
        ));

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "agent-1".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/.ta/credentials/secret.json".to_string(),
        });

        match decision {
            PolicyDecision::Deny { reason } => {
                assert!(reason.contains("secrets path"));
            }
            other => panic!("expected Deny, got {:?}", other),
        }
    }

    #[test]
    fn trace_records_secrets_backstop_denial() {
        let mut engine = PolicyEngine::new();
        engine.load_manifest(test_manifest(
            "agent-1",
            vec![grant("fs", "read", "fs://workspace/**")],
        ));

        let trace = engine.evaluate_with_trace(&PolicyRequest {
            agent_id: "agent-1".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/.env".to_string(),
        });

        match &trace.decision {
            PolicyDecision::Deny { reason } => assert!(reason.contains("secrets path")),
            other => panic!("expected Deny, got {:?}", other),
        }
        assert!(trace.steps.iter().any(|s| s.check == "secrets_backstop"));
        assert!(trace.steps.last().unwrap().terminal);
    }

    #[test]
    fn normal_workspace_file_is_unaffected_by_secrets_backstop() {
        // Regression guard: the backstop must not over-match ordinary files.
        let mut engine = PolicyEngine::new();
        engine.load_manifest(test_manifest(
            "agent-1",
            vec![grant("fs", "read", "fs://workspace/**")],
        ));

        assert_eq!(
            engine.evaluate(&PolicyRequest {
                agent_id: "agent-1".to_string(),
                tool: "fs".to_string(),
                verb: "read".to_string(),
                target_uri: "fs://workspace/src/main.rs".to_string(),
            }),
            PolicyDecision::Allow
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./dev "cargo test -p ta-policy --lib deny_secret_env_file_even_with_broadest_possible_grant deny_credentials_directory_even_with_broadest_possible_grant trace_records_secrets_backstop_denial normal_workspace_file_is_unaffected_by_secrets_backstop"`

Expected: the first three FAIL (`.env`/credentials reads currently return `Allow`, and `trace.steps` has no `"secrets_backstop"` entry); the fourth already passes (nothing has changed its behavior yet). That is fine; it is a regression guard for the step you are about to add.

- [ ] **Step 3: Add the secrets-path backstop**

In `crates/ta-policy/src/engine.rs`, add this above the `PolicyEngine` struct definition (near the existing `APPROVAL_REQUIRED_VERBS` constant, line ~84):

```rust
/// Workspace-relative glob patterns that are always denied, regardless of
/// any grant in any manifest. This is a hard backstop for secret-bearing
/// paths (credentials, private keys, `.env` files). A manifest with an
/// intentionally broad `fs_read` grant (e.g. chat mode's read-anywhere
/// profile, see `ta_policy::chat_manifest::chat_read_profile`) must not be
/// able to expose these, even by accident. Checked before any grant is
/// considered, so no manifest can override it.
const SECRET_PATH_PATTERNS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/credentials.json",
    "**/*.pem",
    "**/*.key",
    "**/id_rsa*",
    "**/.ta/credentials/**",
    "**/.ta/keychain/**",
];

/// Whether `target_uri` matches any of the hardcoded secret-path patterns.
fn matches_secret_path(target: &str) -> bool {
    SECRET_PATH_PATTERNS
        .iter()
        .any(|pattern| matches_resource_pattern(pattern, target))
}
```

Then, in `evaluate()` (around line 136), insert a new step immediately after the existing path-traversal check and before the manifest lookup:

```rust
    pub fn evaluate(&self, request: &PolicyRequest) -> PolicyDecision {
        // Step 1: Check for path traversal in the target URI.
        // This is a security check — agents must not escape their workspace.
        if contains_path_traversal(&request.target_uri) {
            return PolicyDecision::Deny {
                reason: format!(
                    "path traversal detected in target URI: '{}'",
                    request.target_uri
                ),
            };
        }

        // Step 1b: Secrets backstop (denied unconditionally, before any
        // grant is even considered). See SECRET_PATH_PATTERNS' doc comment.
        if matches_secret_path(&request.target_uri) {
            return PolicyDecision::Deny {
                reason: format!(
                    "target '{}' matches a protected secrets path; no grant can override this",
                    request.target_uri
                ),
            };
        }

        // Step 2: Look up the agent's manifest.
        let manifest = match self.manifests.get(&request.agent_id) {
```

(The rest of `evaluate()` is unchanged. Only the new block is inserted between the existing Step 1 and the existing Step 2 comment, which you should leave as "Step 2" textually even though it's now logically the third check; renumbering every comment in the function is not required by this task.)

Then, in `evaluate_with_trace()` (around line 201-229), insert the parallel check right after the existing path-traversal trace block and before the manifest-lookup trace block:

```rust
        steps.push(EvaluationStep {
            check: "path_traversal".to_string(),
            outcome: "passed".to_string(),
            terminal: false,
        });

        // Step 1b: Secrets backstop (mirrors evaluate()'s check above).
        if matches_secret_path(&request.target_uri) {
            steps.push(EvaluationStep {
                check: "secrets_backstop".to_string(),
                outcome: format!(
                    "failed: '{}' matches a protected secrets path",
                    request.target_uri
                ),
                terminal: true,
            });
            return EvaluationTrace {
                decision: PolicyDecision::Deny {
                    reason: format!(
                        "target '{}' matches a protected secrets path; no grant can override this",
                        request.target_uri
                    ),
                },
                steps,
                grants_checked,
                matching_grant,
            };
        }
        steps.push(EvaluationStep {
            check: "secrets_backstop".to_string(),
            outcome: "passed".to_string(),
            terminal: false,
        });

        // Step 2: Manifest lookup
        let manifest = match self.manifests.get(&request.agent_id) {
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `./dev "cargo test -p ta-policy --lib"`

Expected: all tests pass, including the four new ones and the full pre-existing `ta-policy` suite (183 tests as of this plan's writing). This confirms the new backstop doesn't break any existing grant-matching behavior.

- [ ] **Step 5: Commit**

```bash
git add crates/ta-policy/src/engine.rs
git commit -m "Add secrets-path backstop to PolicyEngine::evaluate()

A manifest's fs_read grant (however broad) must never expose .env,
credential, or key files. Checked unconditionally before any grant,
in both evaluate() and evaluate_with_trace()."
```

---

### Task 2: `start_chat_session()` on `GatewayState`, proven through the real MCP tool handlers

**Files:**
- Modify: `crates/ta-mcp-gateway/src/server.rs` (new method, placed directly after `start_goal_with_profile`. Re-locate that function by name; its line number will have drifted from this plan's writing)
- Test: `crates/ta-mcp-gateway/src/server.rs` (inline `#[cfg(test)] mod tests`, or wherever this file's existing tests for `start_goal`/`start_goal_with_profile` live; place the new tests alongside them)

**Interfaces:**
- Consumes: `ta_policy::compile_chat_manifest(agent_id: &str, workspace_resource_scope: &str, validity_hours: i64) -> Result<CapabilityManifest, CompilerError>` (already merged, `crates/ta-policy/src/chat_manifest.rs`), `ta_policy::CHAT_SCRATCH_DIR` (`".ta/chat-scratch"`), `GoalRun::new(title, objective, agent_id, workspace_path, store_path) -> Self` and `.transition(GoalRunState) -> Result<(), GoalError>` (`crates/ta-goal/src/goal_run.rs`), `StagingWorkspace::new(id, staging_dir) -> Result<StagingWorkspace, _>`, `JsonFileStore::new(path) -> Result<JsonFileStore, _>`, `FsConnector::new(goal_id, staging, store, agent_id) -> FsConnector<S>` (`crates/ta-connectors/fs/src/connector.rs`), the Task 1 secrets backstop (for this task's last test case).
- Produces: `GatewayState::start_chat_session(&mut self, agent_id: &str, resource_scope: &str, validity_hours: i64) -> Result<GoalRun, GatewayError>`. Later work (the explicitly out-of-scope `ta-virtual-team` wiring) calls this exact method with these exact three parameters and gets back a `GoalRun` whose `.goal_run_id` is what a caller passes as `goal_run_id` to the `ta_fs_*` MCP tools.

- [ ] **Step 1: Write the failing test**

Find the existing `mod tests` block in `crates/ta-mcp-gateway/src/server.rs` (search for `fn test_server`). It already has `test_server() -> (TaGatewayServer, tempfile::TempDir)`, `test_server_with_source(source_content: &[(&str, &[u8])]) -> (TaGatewayServer, tempfile::TempDir)` (writes real files into the project root before constructing the server; use this one, since the read-access assertion below needs a real file to read), and a `start_goal(server: &TaGatewayServer) -> Uuid` helper used by the existing `start_goal`/`start_goal_with_profile` tests. Add this test alongside them, accessing `server.state` directly (it is a private field, but this test lives in the same module tree, exactly like the existing `start_goal` helper already does at line ~1796). This test drives the real tool-handler entry points in `crates/ta-mcp-gateway/src/tools/fs.rs`, not `PolicyEngine::evaluate()` directly, because that wiring (goal_run_id → `agent_for_goal` → `check_policy` → connector lookup → actual read/write) has never been proven end to end before. Note that the secrets-path and outside-scratch denials happen in `check_policy()`, before the connector ever touches the filesystem; so only the positive read case needs a real file on disk:

```rust
    #[test]
    fn chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers() {
        use crate::tools::fs::{handle_fs_read, handle_fs_write};
        use crate::server::{FsReadParams, FsWriteParams};

        let (server, _dir) = test_server_with_source(&[("notes.txt", b"hello from the real workspace\n")]);
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };

        // Read anywhere in the workspace: the real handler succeeds.
        let read_result = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: goal_run_id.clone(),
                path: "notes.txt".to_string(),
            },
        );
        assert!(
            read_result.is_ok(),
            "expected chat session to read workspace files, got {:?}",
            read_result.err()
        );

        // Write inside the chat scratch dir: the real handler succeeds.
        let scratch_path = format!("{}/notes.md", ta_policy::CHAT_SCRATCH_DIR);
        let write_ok = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: goal_run_id.clone(),
                path: scratch_path,
                content: "scratch notes".to_string(),
            },
        );
        assert!(
            write_ok.is_ok(),
            "expected chat session to write inside chat-scratch, got {:?}",
            write_ok.err()
        );

        // Write outside the scratch dir: the real handler rejects it,
        // propagated from PolicyDecision::Deny through enforce_policy()
        // into a real McpError.
        let write_denied = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: goal_run_id.clone(),
                path: "src/main.rs".to_string(),
                content: "malicious".to_string(),
            },
        );
        assert!(
            write_denied.is_err(),
            "expected chat session write outside scratch to be denied"
        );

        // Reading a secrets-path file: denied the same way, proving Task 1's
        // backstop applies to chat sessions too (chat mode's broad fs_read
        // grant does not bypass it).
        let secret_read_denied = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id,
                path: ".env".to_string(),
            },
        );
        assert!(
            secret_read_denied.is_err(),
            "expected chat session to be denied reading .env"
        );
    }
```

Check whether this file already has a `test_config()` helper used by the existing `start_goal`/`start_goal_with_profile` tests (search for `fn test_config`). If it exists, reuse it exactly as written (it already returns a `(GatewayConfig, TempDir)` tuple or similar; match whatever the existing tests destructure). If no such helper exists yet, build the minimal `GatewayConfig` the same way the nearest existing `start_goal` test does, by copying its setup verbatim rather than inventing a new pattern. Do not guess at `GatewayConfig`'s fields from this plan; read the existing test next to it.

- [ ] **Step 2: Run test to verify it fails**

Run: `./dev "cargo test -p ta-mcp-gateway --lib chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers"`

Expected: FAIL with a compile error (`start_chat_session` does not exist yet). This is expected at this step; proceed to implement it.

- [ ] **Step 3: Implement `start_chat_session()`**

In `crates/ta-mcp-gateway/src/server.rs`, add this method directly after `start_goal_with_profile` (which ends with `Ok(goal_run) }` followed by `check_policy`'s own doc comment. Insert between them):

```rust
    /// Start a chat-mode session: a `GoalRun` whose capability manifest
    /// comes from `ta_policy::compile_chat_manifest()` (broad `fs_read`,
    /// `fs_write_patch` scoped only to `ta_policy::CHAT_SCRATCH_DIR`)
    /// instead of a developer profile. This deliberately reuses the exact
    /// same `GoalRun`/`StagingWorkspace`/`FsConnector` machinery as
    /// `start_goal`/`start_goal_with_profile` so that `tools/fs.rs`'s
    /// already-tested, already-enforced `ta_fs_read`/`ta_fs_write`/
    /// `ta_fs_diff`/`ta_fs_list` handlers work against a chat session's
    /// `goal_run_id` with zero changes to that file.
    ///
    /// A chat session's `GoalRun` is transitioned to `Running` and left
    /// there permanently; no draft/PR lifecycle (`PrReady`/`Approved`/
    /// `Applied`) is ever invoked against it, since chat mode's intended
    /// tool surface never includes `ta_pr_build`. This is a deliberate
    /// design choice (see
    /// `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`),
    /// not a bug or an unfinished state machine.
    pub fn start_chat_session(
        &mut self,
        agent_id: &str,
        resource_scope: &str,
        validity_hours: i64,
    ) -> Result<GoalRun, GatewayError> {
        let goal_run_id = Uuid::new_v4();
        let staging_path = self.config.staging_dir.join(goal_run_id.to_string());
        let store_path = self.config.store_dir.join(goal_run_id.to_string());

        let mut goal_run = GoalRun::new(
            "chat session",
            "answer a chat-mode question using project context",
            agent_id,
            staging_path,
            store_path,
        );
        goal_run.goal_run_id = goal_run_id;

        // Unlike compile_with_id, compile_chat_manifest generates its own
        // manifest_id internally. Keep GoalRun's own manifest_id field
        // consistent with what's actually loaded, rather than leaving it
        // at the placeholder value GoalRun::new() assigned.
        let manifest = ta_policy::compile_chat_manifest(agent_id, resource_scope, validity_hours)
            .map_err(|e| GatewayError::Other(format!("chat manifest compilation failed: {}", e)))?;
        goal_run.manifest_id = manifest.manifest_id;
        self.policy_engine.load_manifest(manifest);

        let staging = StagingWorkspace::new(goal_run_id.to_string(), &self.config.staging_dir)?;
        let store = JsonFileStore::new(self.config.store_dir.join(goal_run_id.to_string()))?;
        let connector = FsConnector::new(goal_run_id.to_string(), staging, store, agent_id);
        self.connectors.insert(goal_run_id, connector);

        goal_run.transition(GoalRunState::Configured)?;
        goal_run.transition(GoalRunState::Running)?;
        self.goal_store.save(&goal_run)?;

        self.event_dispatcher
            .dispatch(&TaEvent::goal_created(goal_run_id, &goal_run.title, agent_id));

        Ok(goal_run)
    }
```

Check `CapabilityManifest` has a public `manifest_id: Uuid` field accessible from `server.rs` (it is already used the same way for `start_goal`'s own manifest via `PolicyCompiler::compile_with_id(goal_run.manifest_id, ...)`, so the type is already in scope. Confirm the field name is exactly `manifest_id` by checking `crates/ta-policy/src/capability.rs`'s `CapabilityManifest` struct if the compiler errors on this line).

- [ ] **Step 4: Run test to verify it passes**

Run: `./dev "cargo test -p ta-mcp-gateway --lib chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers"`

Expected: PASS. If the write-denied or secret-read-denied assertions fail instead of the read/scratch-write ones, re-check Task 1 landed first (this test depends on it) and that `resource_scope` passed to `start_chat_session` in the test is `"fs://workspace/**"` (matching `compile_chat_manifest`'s expected glob-suffix shape, see its doc comment in `crates/ta-policy/src/chat_manifest.rs`).

- [ ] **Step 5: Run the full gateway test suite**

Run: `./dev "cargo test -p ta-mcp-gateway --lib"`

Expected: all pass, including the pre-existing `start_goal`/`start_goal_with_profile` tests. This confirms the new method didn't disturb shared state (`connectors`, `goal_store`, `policy_engine`).

- [ ] **Step 6: Commit**

```bash
git add crates/ta-mcp-gateway/src/server.rs
git commit -m "Add start_chat_session(): lightweight, policy-enforced chat sessions

Reuses the existing GoalRun/StagingWorkspace/FsConnector machinery and
ta-mcp-gateway's already-tested ta_fs_* tool handlers unchanged, loading
a compile_chat_manifest() result instead of a developer profile. Proven
through the real MCP tool-handler entry points, not just PolicyEngine
directly."
```

---

### Task 3: `docs/superpowers/specs/security-hypotheses.md` (the adversarial hypothesis ledger)

**Files:**
- Create: `docs/superpowers/specs/security-hypotheses.md`

**Interfaces:**
- Consumes: the four real test names from PR #634 (`crates/ta-policy/tests/chat_classifier_security_e2e.rs`) and from Tasks 1-2 of this plan.
- Produces: nothing other code depends on. This is a living reference doc, not a dependency of any later task in this plan.

- [ ] **Step 1: Write the file**

```markdown
# TA Security Hypotheses

A running ledger of adversarial hypotheses about TA's security boundaries.
Each one a concrete claim about what an attacker, a prompt injection, or an
errant agent cannot do, backed by a real test against the live system (never
a mock of the thing being tested). Started 2026-10-05 alongside the
chat-mode secure launch primitive; extend this whenever a new security
boundary is built, or a real incident/finding surfaces (same spirit as the
2026-08-24 TA red-team review).

Status values: **blocked** (a real test proves the attack fails today),
**open** (identified, not yet covered by a test), **regressed** (was
blocked, a later change broke it; treat as a P0 bug).

| ID | Hypothesis | Status | Covering test |
|----|------------|--------|----------------|
| H1 | A manifest's `fs_read` grant, however broad, cannot expose secret-bearing paths (`.env`, credentials, private keys). | blocked | `crates/ta-policy/src/engine.rs::deny_secret_env_file_even_with_broadest_possible_grant`, `::deny_credentials_directory_even_with_broadest_possible_grant` |
| H2 | A chat-mode session cannot write outside its ephemeral scratch directory, even through the real MCP tool handlers (not just the policy layer in isolation). | blocked | `crates/ta-mcp-gateway/src/server.rs::chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers` |
| H3 | A chat-mode session cannot perform `git`/`email` actions; there is no grant for either tool in the chat-mode profile, so these are denied before any approval-gating logic is even reached. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest`, `::prompt_injection_cannot_change_what_a_real_work_routing_decision_grants` |
| H4 | A classifier's answer (even a fully compromised one that agrees with an injected "grant full access" payload) has no path into what a chat session's compiled manifest actually grants. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest` |

## Not yet covered (open, tracked for future work)

- Whether a chat-mode agent process can bypass manifest enforcement entirely by using a native tool (Bash, native file read/write) instead of TA's mediated MCP tools, rather than any gap in the manifest or `PolicyEngine` itself. This is a tool-surface-restriction question, addressed by design in `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`'s Phase 1 item 3, but not yet covered by a test here because the actual wiring (which repo/process grants the chat agent its tools) is in `ta-virtual-team`, out of scope for this plan.
- OS-level sandbox backstop (Phase 2 in the same design doc). No test here until that phase is designed.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/security-hypotheses.md
git commit -m "Add security-hypotheses.md: adversarial hypothesis ledger

Seeds the ledger with the four hypotheses already covered by real
tests from PR #634 and this plan's Tasks 1-2, and records the two
known-open gaps (tool-surface bypass, OS sandbox) that are tracked
but not yet testable from this repo alone."
```

---

## Self-Review

**Spec coverage:** Design doc's Phase 1 items 1 (secrets exclusion) → Task 1; item 2 (`start_chat_session`) → Task 2; item 4 (hypothesis-test suite) → Task 3. Item 3 (poller wiring, tool-surface restriction) is explicitly out of scope per the design doc's own phase boundary and the arguments given for this plan; not silently dropped, named in Global Constraints above.

**Placeholder scan:** no TBD/TODO; every step has complete, verified code. `test_server_with_source`, `server.state` field access, and `GoalRun`/`StagingWorkspace`/`FsConnector`/`CapabilityManifest.manifest_id` signatures were all read directly from source while writing this plan, not guessed.

**Type consistency:** `start_chat_session(&mut self, agent_id: &str, resource_scope: &str, validity_hours: i64) -> Result<GoalRun, GatewayError>` is the signature introduced in Task 2 and is the only later-task dependency (Task 3 only references test *names*, not the function itself). `matches_secret_path` (Task 1) is private to `engine.rs` and not referenced by name in Task 2; Task 2's test instead triggers it indirectly through the real `handle_fs_read` call, which is the correct way to prove the wiring (per Task 2's own stated goal: exercise the real MCP entry points, not the policy layer directly).

**Design-doc sketch divergence, flagged rather than silently diverged:** the design doc sketched a new `ChatSession` wrapper struct; this plan has `start_chat_session` return `GoalRun` directly instead, since reusing `GoalRun` is what actually makes `tools/fs.rs` work unchanged, and a wrapper type would add a conversion step with no consumer in this plan's scope. If the later `ta-virtual-team` wiring work finds it wants a narrower, chat-specific return type, that's a cheap wrapper to add then, informed by what that caller actually needs; not guessed at here.
