# TA Security Hypotheses

A running ledger of adversarial hypotheses about TA's security boundaries. Each one is a concrete claim about what an attacker, a prompt injection, or an
errant agent cannot do, backed by a real test against the live system (never
a mock of the thing being tested). Started 2026-10-05 alongside the
chat-mode secure launch primitive; extend this whenever a new security
boundary is built, or a real incident/finding surfaces (same spirit as the
2026-08-24 TA red-team review).

Status values: **blocked** (a real test proves the attack fails today),
**open** (identified, not yet covered by a test), **regressed** (was blocked; a later change broke it, and this is treated as a P0 bug).

| ID | Hypothesis | Status | Covering test |
|----|------------|--------|----------------|
| H1 | A manifest's `fs_read` grant, however broad, cannot expose secret-bearing paths (`.env`, credentials, private keys). | blocked | `crates/ta-policy/src/engine.rs::deny_secret_env_file_even_with_broadest_possible_grant`, `::deny_credentials_directory_even_with_broadest_possible_grant` |
| H2 | A chat-mode session cannot write outside its ephemeral scratch directory, even through the real MCP tool handlers (not just the policy layer in isolation). | blocked | `crates/ta-mcp-gateway/src/server.rs::chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers` |
| H3 | A chat-mode session cannot perform `git`/`email` actions. There is no grant for either tool in the chat-mode profile, so these are denied before any approval-gating logic is even reached. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest`, `::prompt_injection_cannot_change_what_a_real_work_routing_decision_grants` |
| H4 | A classifier's answer (even a fully compromised one that agrees with an injected "grant full access" payload) has no path into what a chat session's compiled manifest actually grants. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest` |
| H5 | The "read anywhere in the workspace" grant (`fs://workspace/**`) cannot reach files outside the workspace via an absolute path. | open | none yet |
| H6 | A caller cannot defeat a chat session's manifest isolation by deliberately choosing an agent_id containing the reserved ':chat:' marker used for chat-session policy identities. | blocked | `crates/ta-mcp-gateway/src/server.rs::start_goal_rejects_agent_id_containing_chat_marker`, `::start_goal_with_profile_rejects_agent_id_containing_chat_marker` |

## Not yet covered (open, tracked for future work)

- Whether a chat-mode agent process can bypass manifest enforcement entirely by using a native tool (Bash, native file read/write) instead of TA's mediated MCP tools, rather than any gap in the manifest or `PolicyEngine` itself. This is a tool-surface-restriction question, addressed by design in `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`'s Phase 1 item 3, but not yet covered by a test here because the actual wiring (which repo/process grants the chat agent its tools) is in `ta-virtual-team`, out of scope for this plan.
- OS-level sandbox backstop (Phase 2 in the same design doc). No test here until that phase is designed.
- **H5, absolute-path workspace escape (pre-existing, found during final whole-branch review of chat-mode secure launch Phase 1, 2026-10-05):** `check_policy()` in `crates/ta-mcp-gateway/src/server.rs` builds the target URI as `format!("fs://workspace/{}", path)`. When `path` is an absolute path (e.g. `/Users/me/.aws/credentials`), the result is `fs://workspace//Users/me/.aws/credentials`, which the glob pattern `fs://workspace/**` still matches, so the grant check passes. Separately, `crates/ta-connectors/fs/src/connector.rs`'s `read_source()`/`write_patch()` join the (attacker-controlled) relative path onto the source/staging base directory using `PathBuf::join()`, which discards the base entirely when the joined path is absolute, reaching arbitrary absolute paths on disk, not just workspace files. This means the design's "read anywhere in the workspace" grant for chat mode can, in practice, read anywhere on disk the process can see. Compounding this, the secrets backstop's `SECRET_PATH_PATTERNS` (`crates/ta-policy/src/engine.rs`) only covers workspace-relative secret filenames (`.env`, `credentials.json`, `*.pem`, `*.key`, `id_rsa*`) and does not cover common absolute-path secret locations such as `~/.aws/credentials` or `~/.ssh/id_ed25519`. Not fixed here: per the review's recommendation, this is recorded for a future session rather than fixed in this branch (out of scope, pre-existing, not introduced by this work). Starting points for whoever picks this up: the URI construction in `check_policy()` (`crates/ta-mcp-gateway/src/server.rs`) and the path joining in `read_source`/`write_patch` (`crates/ta-connectors/fs/src/connector.rs`).
