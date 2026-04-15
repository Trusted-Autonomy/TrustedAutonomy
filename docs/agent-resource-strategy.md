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

## Credential Architecture

**The agent never holds raw credentials.** This is not a recommendation — it is an enforced invariant. TA acts as an identity broker between the agent and any external resource. The agent calls a TA MCP tool (`ta_external_action`, `ta_context`, etc.) with a logical resource name. TA resolves the real credential internally, applies policy, and executes (or stages for review). The credential is never exposed to the agent process or to the staging workspace.

This changes the security gap analysis significantly. The threat model is not "agent with credentials writes to Postgres." It is "TA has no Postgres staging layer, so TA cannot diff or review the mutation before it lands."

### TA Mode — Local File Vault

**Implemented** (`crates/ta-credentials/`)

Credentials are stored at `.ta/credentials.json` with `0600` file permissions (owner-read-only). The file is listed in `.gitignore` by `ta init` and is never committed to VCS. This is the only correct location for secrets in a TA project.

The vault issues **scoped, time-limited session tokens** to the agent for the duration of a goal:

```
human adds credential:
  ta credentials add --name "gmail" --service "google" --secret "<token>" --scopes gmail.send

agent goal starts:
  TA issues SessionToken { allowed_scopes: ["gmail.send"], expires_at: now + 3600s }
  SessionToken is passed to the agent (not the raw secret)
  Agent calls ta_external_action { action_type: "email", ... }
  TA validates token scope → executes send (or stages for review)
  Raw credential never leaves the vault
```

`SessionToken` carries:
- `credential_id` — which underlying credential to use
- `allowed_scopes` — what the agent may do with it
- `expires_at` — automatic revocation after the goal TTL
- `agent_id` — which agent session holds the token

The `FileVault` backend stores tokens alongside credentials and prunes expired tokens on validation. A future `age`-encrypted-at-rest layer is noted in the code but not yet implemented.

**What is committed to VCS:** nothing credential-related. `.ta/credentials.json` is gitignored. Agents work from session tokens, not raw secrets.

**Security gaps (TA local vault):**
- `credentials.json` is plaintext JSON with `0600` permissions. If the host is compromised (another process running as the same user), secrets are readable. At-rest encryption (`age`) is planned.
- Session tokens are stored in the same file as credentials. Separate token storage would reduce the blast radius of a token leak.
- No credential rotation enforcement. The vault does not expire stored credentials, only issued tokens. Credential lifetime is human-managed.

### SA Mode — Enterprise Credential Store (Planned)

**Planned** — `CredentialVault` trait is designed for pluggable backends.

In Supervised Autonomy (SA) mode, the `FileVault` is replaced by an enterprise credential store plugin. The plugin interface is the `CredentialVault` trait already in place:

```rust
pub trait CredentialVault: Send + Sync {
    fn issue_token(&mut self, credential_id, agent_id, scopes, ttl_secs) -> Result<SessionToken>;
    fn validate_token(&self, token_id) -> Result<SessionToken>;
    // ... add, list, revoke
}
```

A production plugin would delegate to:
- **HashiCorp Vault** — via Vault Agent or direct API, with AppRole or Kubernetes auth.
- **AWS Secrets Manager / Parameter Store** — IAM-scoped access; secret rotation automatic.
- **Azure Key Vault** — Managed Identity authentication.
- **Infisical, Doppler, etc.** — via plugin.

**User validation requirement:** In SA mode, credential issuance requires authentication of the requesting user/agent, not just a scope match. The plugin is responsible for asserting that the agent identity (session ID, signed token, or SPIFFE SVID) is authorized to hold the requested scopes before issuing a `SessionToken`. This prevents one agent from escalating to another agent's credential scope.

The plugin is configured in `workflow.toml`:
```toml
[credentials]
backend = "hashicorp-vault"     # "file" (default) | "hashicorp-vault" | "aws-secrets" | <plugin>
vault_addr = "https://vault.internal"
auth_method = "approle"
```

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
- No staging, no diff, no review, and no audit for Postgres or MySQL writes. **This is the highest-severity resource governance gap in the current implementation.** The agent does not hold credentials directly (TA's credential vault mediates all access), but because TA has no Postgres staging layer, every mutation the agent requests via `ta_external_action` executes immediately against the real database with no diff, no review gate, and no rollback capability.
- With `policy = "review"`, the action payload (SQL statement) is held for human review before execution — this is the correct interim posture for Postgres. However, the reviewed artifact is the SQL text, not a row-level diff. A complex migration is hard to audit from SQL text alone.
- `policy = "auto"` for Postgres is equivalent to giving the agent unrestricted write access to the database. Do not use.

**Interim mitigation:** Set `[actions.db_query] policy = "review"` for Postgres targets. This surfaces every SQL statement for human review before execution. It does not provide row-level diffs or rollback — it is a human-in-the-loop gate, not a staging layer.

Note: `psql` and other DB CLIs are not in the command allowlist by default. If the agent tries to call them directly via shell, the command sandbox (`ta-sandbox`) will block the invocation. The agent must go through the `ta_external_action` MCP tool to reach any database.

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
- The agent never holds SMTP credentials or API keys directly — all email sends go through TA's credential vault and MCP tool. The risk is not credential theft; it is **prompt injection causing TA to send an attacker-controlled email.** With `policy = "auto"`, a malicious instruction embedded in a file the agent reads could trigger an email to an attacker's address with sensitive content. Use `policy = "review"` for any production environment where the agent reads untrusted input.
- No recipient allowlist enforcement. The agent can construct a recipient address from content it reads, making `policy = "auto"` a potential data exfiltration vector via prompt injection.
- Email content is logged to `.ta/action-log.jsonl` but entries are plaintext and not HMAC-signed. A compromised host could tamper with the log. Future: append-only signed log.
- Rate limits are per-session only (`rate_limit` in `[actions.email]`). A goal that iterates over a large list could send many emails across multiple sessions. Cross-session rate limiting is not implemented.

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
- The agent does not hold API keys directly — credentials are resolved by TA's vault at execution time. The risk is **prompt injection directing TA to POST to an attacker-controlled URL** with `policy = "auto"`. An agent that reads a malicious document could be instructed to exfiltrate its context to an external endpoint.
- `policy = "review"` mitigates this: the full URL, method, headers, and body are shown to the human before the request is sent. This is the correct posture for any action that sends data outbound.
- `curl` and `wget` are not in the command allowlist by default — shell-level HTTP is blocked. Agents that try to use an HTTP library via an inline script (Python `requests`, etc.) are blocked by the OS sandbox (Tier 3) network filter. Tier 1/2 have only the command allowlist as a control.
- API responses are not staged. Sensitive data an agent reads from an API and holds only in memory is not captured by TA. Only file writes and MCP-mediated actions are observable.

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
