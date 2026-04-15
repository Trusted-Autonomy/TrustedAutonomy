# Agent Resource Strategy

TA treats every resource an agent can read or modify — files, databases, email, APIs, cloud storage — as a governed URI. The URI is the identity of the resource. Policy, staging, audit, and draft review all operate on URIs. This document covers the full resource taxonomy: what is implemented, what is planned, and where security gaps exist today.

See [file-system-strategy.md](file-system-strategy.md) for the filesystem-specific three-tier architecture (overlay, managed paths, OS sandbox).

---

## URI Scheme Registry

All resource identities in TA use a URI scheme that determines how the resource is staged, diffed, and reviewed. Scheme-aware pattern matching in `crates/ta-changeset/src/uri_pattern.rs` prevents cross-scheme confusion: a policy rule scoped to `fs://workspace/**` will never match `gmail://inbox/**`, even if the path segment is identical.

| Scheme | Example | Stage/Diff | Status |
|---|---|---|---|
| `fs://workspace/<path>` | `fs://workspace/src/main.rs` | CoW overlay | **Implemented** |
| `fs://governed/<path>` | `fs://governed/comfyui/outputs/img.png` | SHA journal | **Planned** |
| `db://sqlite/<db-path>#<table>` | `db://sqlite/app.db#users` | Shadow DB + mutation replay | **Implemented** |
| `db://postgres/<conn>#<table>` | `db://postgres/prod#orders` | Framework only | **Planned** |
| `db://mysql/<conn>#<table>` | `db://mysql/analytics#events` | Framework only | **Planned** |
| `email://outbound/<id>` | `email://outbound/draft-abc` | Captured to pending action | **Implemented** |
| `gmail://inbox/<msg-id>` | `gmail://inbox/msg-xyz` | URI pattern only | **Planned** |
| `gmail://sent/<id>` | `gmail://sent/msg-abc` | URI pattern only | **Planned** |
| `drive://docs/<id>` | `drive://docs/brief-xyz` | URI pattern only | **Planned** |
| `api://<host>/<path>` | `api://api.stripe.com/charges` | Captured to pending action | **Implemented (generic)** |

Bare patterns in policy rules (e.g., `src/**`) auto-prefix to `fs://workspace/` — a convenience for the common case.

---

## Filesystem Resources (`fs://`)

See [file-system-strategy.md](file-system-strategy.md) for the full design.

**Summary:**
- `fs://workspace/<path>` — project directory, CoW overlay staging. Every write the agent makes is captured and diffed. **Implemented.**
- `fs://governed/<path>` — external paths (ComfyUI outputs, model checkpoints, etc.), SHA filesystem + URI journal. **Planned.**

**Security gaps:**
- Agent can read any file reachable from the user's shell (`~/.ssh`, `~/.aws`, other repos) without restriction at Tier 1 or Tier 2. Tier 3 OS sandbox is required to restrict reads.
- At Tier 1 and 2, writes to ungoverned paths are invisible to TA. A misbehaving agent can persist state outside the project that survives `ta draft deny`.

---

## Database Resources (`db://`)

### SQLite — Implemented

**Crates:** `crates/ta-db-proxy-sqlite/`, `crates/ta-db-overlay/`, `crates/ta-db-proxy/`

The agent works against a shadow copy of the SQLite database in the staging area. Mutations (INSERT, UPDATE, DELETE, schema changes) are captured to a JSONL mutation log. On `ta draft apply`, the mutation log is replayed against the real database. On `ta draft deny`, the real database is untouched — the shadow is discarded.

**Architecture:**

```
agent → SQLite shadow DB (in .ta/staging/)
             ↓ mutations
        DraftOverlay (JSONL log)
             ↓ ta draft apply
        real database
```

`DraftOverlay` provides read-your-writes consistency: the agent reads back rows it has written within the same session without those changes touching the real DB.

`ta draft view` shows database mutations as structured diffs — table name, row key, before/after values. This is more informative than a raw SQL diff.

**URI example:**
```
db://sqlite/path/to/app.db#users
```
Policy rules can scope to specific tables (`db://sqlite/**#users`) or entire databases (`db://sqlite/app.db#*`).

**Security gaps:**
- The shadow copy is a full duplicate of the database file at goal start. Large databases (>1 GB) add significant staging overhead. No partial-table staging exists.
- Schema migrations that alter the database beyond what the mutation log captures (e.g., `VACUUM`, `PRAGMA` changes) may not replay cleanly. These are flagged as warnings but not blocked.
- The shadow is based on the database state at staging time. Concurrent writes to the real database during the goal are not captured — the shadow will be stale. This is a known limitation for multi-writer SQLite databases.

### Postgres / MySQL — Framework Only

**Crate:** `crates/ta-db-proxy/src/lib.rs` (trait `DbProxyPlugin`)

The framework exists and is designed for extension. No Postgres or MySQL implementation is present. Contributing a plugin requires implementing `DbProxyPlugin` and registering it with the runtime.

**Planned approach for Postgres:**
- Logical replication slot on the source database captures WAL events during the goal.
- Agent connects to a read-write replica (or a local schema clone) for the duration of the goal.
- WAL diff is replayed on `ta draft apply` or discarded on deny.

**Security gaps (Postgres/MySQL):**
- No staging, no diff, no review, no audit for Postgres or MySQL writes today. Agent writes to production directly and immediately. This is the highest-severity resource governance gap in the current implementation.
- If the agent has Postgres credentials, it can modify or delete data and TA has no visibility.

**Interim mitigation:** Set `[actions.db_query] policy = "block"` in workflow.toml to prevent the agent from executing any DB queries via the MCP action system. This does not prevent the agent from running `psql` or `sqlite3` directly from the shell.

---

## Email Resources (`email://`, `gmail://`)

### Outbound Email — Implemented

**Crate:** `crates/ta-connectors/email/src/lib.rs`

The agent sends email via the `ta_external_action` MCP tool with `action_type = "email"`. TA applies the configured policy before any send occurs:

- `policy = "block"` — rejects the action with an error.
- `policy = "review"` — captures the email content to the pending-actions queue; the email is shown in `ta draft view` and only sent on `ta draft apply`.
- `policy = "auto"` — sends immediately via the configured HTTP adapter.

The HTTP adapter supports any provider with a REST send endpoint: SendGrid, Mailgun, Postmark, custom. Plain-text and HTML bodies are both supported. Reply links (for choice-based human questions) are embedded in the body.

All email actions are logged to `.ta/action-log.jsonl` regardless of policy.

**What it does not do:**
- SMTP client (only HTTP-based providers).
- Inbound email parsing (no webhook handler for replies, no inbox polling).
- Gmail API (OAuth, labels, threads) — the `gmail://` URI scheme has pattern matching but no real connector.

**Security gaps:**
- With `policy = "auto"`, the agent can send arbitrary emails to arbitrary recipients without human review. This is appropriate only in controlled environments where the agent's email behavior is fully trusted.
- Email content is logged but not hashed or signed. An attacker with access to the action log could modify the record after the fact. Future: HMAC-signed action log entries.
- No outbound rate limit across sessions (only per-session rate limiting via `rate_limit` in `[actions.email]`). A runaway goal could send many emails if rate limits are set high.
- No recipient allowlist enforcement today. If the agent constructs a recipient address from injected input, it could exfiltrate data via email. Mitigation: `policy = "review"` for all email; production environments should not use `policy = "auto"`.

### Gmail API — Planned

The `gmail://` URI scheme is registered in the pattern matcher. A real connector requires:
- OAuth2 flow for Gmail API credentials.
- Read staging: capture inbox reads to session transcript (no modification to inbox state).
- Write staging: draft emails to Gmail's drafts API; send only on `ta draft apply`.
- Reply parsing: inbound emails from a configured address can trigger webhook callbacks to the TA daemon.

---

## External API Resources (`api://`)

**Implemented** (`crates/ta-actions/`, `crates/ta-mcp-gateway/src/tools/action.rs`)

The agent calls external APIs via the `ta_external_action` MCP tool. TA intercepts the call and applies policy before any real HTTP request is made.

**Policy model:**

```toml
# .ta/workflow.toml
[actions.api_call]
policy = "review"          # "block" | "review" | "auto"
rate_limit = 20            # max calls per session
```

With `policy = "review"`, the API call payload (URL, method, headers, body) is captured to the pending-actions queue and shown in `ta draft view`. The actual HTTP request is only sent on `ta draft apply`.

**Audit:** Every API action is logged to `.ta/action-log.jsonl` with timestamp, URL, method, status code (for auto executions), and goal ID.

**Security gaps:**
- `policy = "auto"` allows the agent to make arbitrary HTTP requests immediately. If the agent's prompt has been injected with a malicious instruction, it can POST sensitive data to an attacker's server. Use `policy = "review"` for any action that can exfiltrate data.
- The command allowlist (`ta-sandbox`) blocks `curl` and `wget` by default, which prevents direct shell-level exfiltration. However, agents using an HTTP library via cargo or an inline Python script may bypass this. The OS sandbox (Tier 3) is the reliable control for network exfiltration.
- API responses are not staged. If the agent reads sensitive data from an API and stores it in a file, that file may appear in the draft diff — but only if it lands in the project directory. Reads to memory-only storage are invisible.

---

## Cloud Storage (`drive://`, `s3://`, etc.)

**URI patterns registered; no real connectors implemented.**

The `drive://` scheme is defined in `ta-changeset/src/uri_pattern.rs` for pattern matching in policy rules. A real Google Drive connector would require OAuth2 and Drive API integration.

No S3, Azure Blob, or GCS connector exists. These would follow the same pattern as the external action framework: intercept via `ta_external_action`, apply block/review/auto policy, capture the upload payload for review.

**Security gap:** If the agent uses an AWS SDK directly (e.g., via a shell command `aws s3 cp ...`), TA has no visibility unless the `aws` command is either blocked by the command allowlist or the agent calls through the `ta_external_action` tool. The command allowlist does not block `aws` by default — add it to `denied_commands` in the sandbox config if cloud uploads are a concern.

---

## External Process Governance (ComfyUI, SimpleTuner, etc.)

**Status: No implementation.** Tier 2 managed paths is the planned solution.

Processes like ComfyUI and SimpleTuner run independently of TA. They write to their own output directories. TA has no visibility into those writes today.

**Interim approach (available now):**
1. Run TA's goal with the output directory added to `governed_paths` (once Tier 2 is built).
2. Before Tier 2 exists: snapshot the output directory before the agent starts (`sha256sum -r outputs/ > outputs-before.sha`), then diff after the goal completes. This is manual but auditable.

**Planned approach (Tier 2):**
- `governed_paths` config points TA at the ComfyUI output directory.
- FUSE or LD_PRELOAD intercept captures writes from any process (not just the TA agent) that writes to that path.
- Each write is recorded in the SHA journal and surfaced in `ta draft view`.
- This makes ComfyUI and SimpleTuner outputs first-class TA artifacts, reviewable and rollbackable alongside code changes.

**Security note:** SimpleTuner checkpoints can be hundreds of GB. The SHA store would need a configurable `max_sha_store_mb` and a policy for what happens when the limit is reached (reject write, warn, or evict oldest blobs). This is a design decision for the Tier 2 implementation phase.

---

## Policy Configuration Reference

```toml
# .ta/workflow.toml

# External action policies (all action types)
[actions.email]
policy = "review"          # block | review | auto
rate_limit = 5             # max per goal session

[actions.api_call]
policy = "review"
rate_limit = 20

[actions.db_query]
policy = "review"
rate_limit = 50

[actions.social_post]
policy = "block"           # never allow autonomous social posts

# Filesystem auto-approve conditions
[defaults.auto_approve.drafts]
enabled = false

[defaults.auto_approve.drafts.conditions]
allowed_paths = ["docs/**", "tests/**"]   # only these paths qualify for auto-approve
blocked_paths = [".ta/**", "*.env", "secrets/**"]  # these override allowed_paths
max_files = 10
max_lines_changed = 200

# Sandbox (Tier 3)
[sandbox]
enabled = false            # set true for high-security goals
provider = "auto"

[sandbox.allow_network]
hosts = ["api.anthropic.com", "github.com"]
```

URI-scoped policy overrides (planned — not yet implemented):

```toml
[[policy.uri_rules]]
pattern = "db://sqlite/**"
action = "review"          # all SQLite mutations require human review

[[policy.uri_rules]]
pattern = "email://**"
action = "review"

[[policy.uri_rules]]
pattern = "fs://governed/comfyui/**"
action = "auto"            # ComfyUI outputs auto-approved (low risk)
```

---

## Implementing a New Resource Connector

1. **Define a URI scheme.** Add it to the scheme registry in `crates/ta-changeset/src/uri_pattern.rs` and write scheme-aware tests.

2. **Implement the staging interface.** For database-style resources: implement `DbProxyPlugin` in `crates/ta-db-proxy/`. For action-style resources: register a handler in `crates/ta-actions/` with a `payload_schema` and `execute` method.

3. **Add draft diff rendering.** Update `crates/ta-changeset/src/artifact.rs` to produce a human-readable diff for the new URI scheme. This is what appears in `ta draft view`.

4. **Add replay/rollback.** Implement the apply and deny paths: what happens to the real resource when the draft is approved vs denied.

5. **Register in workflow.toml schema.** Add the new action type to the policy config schema in `crates/ta-policy/`.

6. **Write integration tests.** Use the mock connector pattern (`crates/ta-connectors/mock-gmail/`, `crates/ta-connectors/mock-drive/`) to test staging, diff, apply, and deny without a real external service.

---

## Implementation Status Summary

| Resource | Staging | Diff in draft | Apply/Rollback | Policy gate | Audit | Security gap severity |
|---|---|---|---|---|---|---|
| `fs://workspace` (project files) | ✅ CoW overlay | ✅ | ✅ | ✅ | ✅ | Low — reads unprotected at Tier 1/2 |
| `fs://governed` (external paths) | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | High — no coverage today |
| `db://sqlite` | ✅ Shadow DB | ✅ Row diffs | ✅ Mutation replay | ✅ | ✅ | Low |
| `db://postgres` | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | **Critical** — direct writes, no visibility |
| `db://mysql` | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | ⬜ Planned | **Critical** |
| `email://outbound` | ✅ Pending queue | ✅ | ✅ | ✅ | ✅ | Medium — auto policy bypasses review |
| `gmail://` | ⬜ Planned | ⬜ Planned | ⬜ Planned | ✅ (pattern) | ⬜ | High — real sends not governed |
| `api://` (generic HTTP) | ✅ Pending queue | ✅ | ✅ | ✅ | ✅ | Medium — auto policy risk |
| `drive://` | ⬜ Pattern only | ⬜ | ⬜ | ⬜ | ⬜ | High |
| External processes (ComfyUI etc.) | ⬜ Planned (Tier 2) | ⬜ | ⬜ | ⬜ | ⬜ | High — completely invisible |
