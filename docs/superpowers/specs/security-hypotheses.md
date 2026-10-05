# TA Security Hypotheses

A running ledger of adversarial hypotheses about TA's security boundaries —
each one a concrete claim about what an attacker, a prompt injection, or an
errant agent cannot do, backed by a real test against the live system (never
a mock of the thing being tested). Started 2026-10-05 alongside the
chat-mode secure launch primitive; extend this whenever a new security
boundary is built, or a real incident/finding surfaces (same spirit as the
2026-08-24 TA red-team review).

Status values: **blocked** (a real test proves the attack fails today),
**open** (identified, not yet covered by a test), **regressed** (was
blocked, a later change broke it — treat as a P0 bug).

| ID | Hypothesis | Status | Covering test |
|----|------------|--------|----------------|
| H1 | A manifest's `fs_read` grant, however broad, cannot expose secret-bearing paths (`.env`, credentials, private keys). | blocked | `crates/ta-policy/src/engine.rs::deny_secret_env_file_even_with_broadest_possible_grant`, `::deny_credentials_directory_even_with_broadest_possible_grant` |
| H2 | A chat-mode session cannot write outside its ephemeral scratch directory, even through the real MCP tool handlers (not just the policy layer in isolation). | blocked | `crates/ta-mcp-gateway/src/server.rs::chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers` |
| H3 | A chat-mode session cannot perform `git`/`email` actions — there is no grant for either tool in the chat-mode profile, so these are denied before any approval-gating logic is even reached. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest`, `::prompt_injection_cannot_change_what_a_real_work_routing_decision_grants` |
| H4 | A classifier's answer — even a fully compromised one that agrees with an injected "grant full access" payload — has no path into what a chat session's compiled manifest actually grants. | blocked | `crates/ta-policy/tests/chat_classifier_security_e2e.rs::prompt_injection_cannot_escalate_past_the_compiled_manifest` |

## Not yet covered (open, tracked for future work)

- Whether a chat-mode agent process can bypass manifest enforcement entirely by using a native tool (Bash, native file read/write) instead of TA's mediated MCP tools, rather than any gap in the manifest or `PolicyEngine` itself. This is a tool-surface-restriction question, addressed by design in `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`'s Phase 1 item 3, but not yet covered by a test here because the actual wiring (which repo/process grants the chat agent its tools) is in `ta-virtual-team`, out of scope for this plan.
- OS-level sandbox backstop (Phase 2 in the same design doc) — no test here until that phase is designed.
