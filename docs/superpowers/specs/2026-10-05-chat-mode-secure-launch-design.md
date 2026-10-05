# Chat-Mode Secure Launch Primitive — Design

**Status:** approved by user 2026-10-05, ready for implementation plan.

## Context

Two building blocks merged earlier this session:

- **PR #632** — `ta-ask`: a narrow, synchronous `ask(backend, question, context, schema) -> DecisionResponse` decision primitive, with `DeciderBackend` (real local model) and `FixtureBackend` (deterministic, for tests).
- **PR #633** — `ta-policy::chat_manifest`: `compile_chat_manifest(agent_id, workspace_resource_scope, validity_hours) -> CapabilityManifest`, a narrow "chat mode" profile (broad `fs_read`, `fs_write_patch` scoped to `.ta/chat-scratch/**` only, nothing else).

Neither was wired into anything that runs a real chat session. This doc designs that wiring, with security as the explicit top priority per the user's own framing: **"make sure the agent cannot break out"** and **"asking a question cannot let prompt injection or errant agent trigger changes."**

### Corrected architecture finding (supersedes this session's earlier, wrong claim)

An earlier pass in this session concluded capability-manifest enforcement didn't exist anywhere in a real code path. That was wrong. Re-verified directly against source:

- `crates/ta-mcp-gateway/src/tools/fs.rs`'s `handle_fs_read`/`handle_fs_write`/`handle_fs_diff` call `GatewayState::check_policy()` → the real `PolicyEngine::evaluate()` → `enforce_policy()` on **every single call**, already default-deny, already tested (`crates/ta-policy/src/engine.rs`'s own test suite, 183 tests, all real).
- `GatewayState::start_goal_with_profile()` (`server.rs:697`) already uses the **lightweight** `StagingWorkspace::new()` (an empty dir + a connector registration) — not the expensive `OverlayWorkspace::create_with_strategy()` full-tree copy. That full copy only happens in the *separate* `ta run`/`ta goal start` CLI path (`apps/ta-cli/src/commands/{run,goal}.rs`), which exists to spawn a whole native-tool-wielding subprocess agent (Claude Code itself, or similar) working directly on a staged copy of the tree — a fundamentally different trust model (isolation via copy + human review of the diff, not via mediated tool calls).

So there are two pre-existing, independent isolation mechanisms in TA today:

1. **Mediated-tool isolation** (`ta-mcp-gateway`'s `ta_fs_*` tools): the agent has no native filesystem access; every read/write is a policy-checked RPC. Already built, already lightweight, already enforced. This is the right mechanism for chat mode.
2. **Copy-plus-review isolation** (`ta run`'s staged subprocess launch): the agent has full native tool access but only to a disposable copy; a human reviews the diff before anything reaches the real tree. This is the right mechanism for real implementation work, and is out of scope for this doc.

**The actual gap was much narrower than first reported**: nobody calls a chat-flavored equivalent of `start_goal_with_profile()` to load a `compile_chat_manifest()` result for a chat session's `agent_id`, so a CoS chat session hitting `ta_fs_read` today just gets denied (fails safe, but non-functional, since no manifest means `PolicyEngine::evaluate()` returns `Deny { "no capability manifest" }`).

## The real security question

A capability manifest only constrains calls that go through TA's own mediated tools. If the process answering a chat message *also* has an unmediated native tool — Bash, a native file-read/write tool, an unrestricted MCP server — the manifest is decorative: the agent can simply not use the mediated path. This is the literal "confused deputy" / "cannot break out" risk the user named, and it is a tool-surface problem, not a manifest-content problem.

**Decision (user-approved): chat-mode sessions get *only* TA's mediated MCP tools — no Bash, no native Read/Write/Edit, no other MCP server with filesystem/network/git access.** The capability manifest then means what it says, because there is no unmediated path around it.

An OS-level sandbox backstop (reusing the existing, separate `ta_goal::SecurityProfile` mechanism) is the right additional defense-in-depth layer, but is **explicitly deferred to a Phase 2** (see below) — it requires deciding where/how the chat-mode client process itself runs (today TA doesn't necessarily control that process boundary the way it controls a `ta run`-spawned subprocess), and that decision shouldn't block shipping the Phase 1 mechanism, which is already a real, enforced security boundary on its own.

## New: secrets-exclusion in `PolicyEngine::evaluate()`

Found while designing chat mode's read grant: `chat_read_profile()` grants broad `fs_read` across the whole workspace scope with no exclusion for secret-bearing paths. A broad read grant is correct and intended (CoS needs to answer questions about the project) — mediated, audited, scope-bounded, path-traversal-blocked — but as designed today it would also let a chat session read `.env`, credential files, private keys, etc., any of which could then be echoed back in a chat answer.

This is not chat-mode-specific; it is a gap in `PolicyEngine::evaluate()` itself, so the fix benefits every manifest in TA, not just chat mode. Add a new built-in check, at the same tier as the existing path-traversal check (`engine.rs` step 1), that denies any request whose `target_uri` matches a fixed secrets-path denylist — independent of grants, so no manifest can override it by accident:

```rust
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
```

Evaluated as glob patterns (reusing the `glob` crate already used by `matches_resource_pattern`), checked before grant-matching, denying with a clear reason (`"target matches a protected secrets path: <pattern>"`) — observable per this repo's Observability Mandate.

This becomes the first entry in the adversarial hypothesis-test suite (see below): *"H1: a chat session with a broad `fs_read` grant cannot read credential/secret files."*

## Design: `start_chat_session`

A new method on `GatewayState`, sibling to `start_goal_with_profile()`, in `crates/ta-mcp-gateway/src/server.rs`:

```rust
pub fn start_chat_session(
    &mut self,
    agent_id: &str,
    resource_scope: &str,
    validity_hours: i64,
) -> Result<ChatSession, GatewayError> {
    let session_id = Uuid::new_v4();
    let manifest = ta_policy::compile_chat_manifest(agent_id, resource_scope, validity_hours)
        .map_err(|e| GatewayError::Other(format!("chat manifest compilation failed: {}", e)))?;
    self.policy_engine.load_manifest(manifest);

    let staging = StagingWorkspace::new(session_id.to_string(), &self.config.staging_dir)?;
    let store = JsonFileStore::new(self.config.store_dir.join(session_id.to_string()))?;
    let connector = FsConnector::new(session_id.to_string(), staging, store, agent_id);
    self.connectors.insert(session_id, connector);

    Ok(ChatSession { session_id, agent_id: agent_id.to_string() })
}
```

Reuses `ta_fs_read`/`ta_fs_write`/`ta_fs_diff`/`ta_fs_list` exactly as they exist today — no changes needed to `tools/fs.rs` at all, since those handlers already key everything off `goal_run_id`/`agent_for_goal`/`check_policy`, and a chat session's `session_id` slots into that same lookup shape (needs `agent_for_goal` — or a renamed, more general lookup — to resolve a chat session's `agent_id` the same way it resolves a goal's).

No `GoalRun`/`GoalRunState` machinery is created — chat sessions are not goals, don't need draft-build/PR-review lifecycle, and `ta_fs_write`'s existing scratch-only grant means there's nothing to build a draft from in the first place.

**Where this gets called**: `ta-virtual-team`'s poller, at the point it currently self-classifies an incoming message as "chat mode" (the design this session already settled: classify via `ta_ask::ask()`, never trust that classification as the security boundary itself — it only decides routing). The poller calls `start_chat_session()` once per chat exchange (or reuses one for a session's lifetime, bounded by `validity_hours`), then gives CoS a `session_id` to use with the (MCP-only) tool surface.

## Phase boundaries

**Phase 1 (this plan):**
1. Secrets-exclusion check in `PolicyEngine::evaluate()` (TA-wide benefit).
2. `start_chat_session()` + `ChatSession` type in `ta-mcp-gateway`.
3. Wiring so a chat-mode agent process is launched/connected with *only* the TA MCP server's tools available (no Bash/native file tools, no other MCP servers) — the concrete mechanism depends on how `ta-virtual-team`'s poller currently spawns/connects the CoS session; this needs its own investigation task before the plan can specify exact file changes there.
4. Static hypothesis-test suite, extending `crates/ta-policy/tests/chat_classifier_security_e2e.rs` (already landed in PR #634) with more named hypotheses (secrets exclusion, tool-surface bypass attempts, expired-manifest reuse, etc.) — see below.

**Phase 2 (explicitly deferred, not blocking):**
- OS-level sandbox backstop for the chat-mode client process (reusing `ta_goal::SecurityProfile`), once it's decided where that process actually runs.
- Agentic (LLM-driven) red-team runs, as an occasional, explicitly-requested deeper pass — not automatic, not blocking CI.

## Adversarial hypothesis-test methodology (static, CI-enforced)

Per user direction ("both, static first"): formalize the pattern already used in PR #634 into a standing methodology, not a one-off.

- Each hypothesis is named (`H<n>: <claim about what an attacker cannot do>`), lives as a real Rust test against the live `PolicyEngine`/MCP tool handlers (never mocks the thing being tested), and is tracked in a running list (`docs/superpowers/specs/security-hypotheses.md`, new) with status (blocked / open / fixed-on-<date>).
- Not limited to chat mode — applies to any TA security boundary (credential broker, draft-apply approval gating, whiteboard, OS sandboxing). New hypotheses get added whenever a new boundary is built or a real incident/finding surfaces (same spirit as the 2026-08-24 TA red-team review already in project memory).
- This is the CI-enforced floor. Agentic red-team runs (Phase 2) are a deeper, periodic supplement — reserved for when the user explicitly asks for a red-team pass (e.g. before a release), using the Workflow tool only on that explicit request.

## Self-review

- Placeholder scan: none — every section states a concrete mechanism or an explicitly named deferral.
- Internal consistency: the "no OS sandbox yet" phase-2 deferral is consistent with the MCP-only tool-surface decision being the actual enforcement boundary for Phase 1.
- Scope: Phase 1 item 3 (how the poller connects CoS with a restricted tool surface) is flagged as needing its own investigation before task-level planning — this is a real open dependency, not hand-waved.
