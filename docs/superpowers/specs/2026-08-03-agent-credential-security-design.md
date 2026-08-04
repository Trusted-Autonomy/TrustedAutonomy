# TA Least-Privilege Agent Authorization Design

**Status**: Proposed — not yet a PLAN.md phase. Requires human sequencing decision (see Open Questions) before phase IDs are assigned.
**Baseline**: [`docs/architecture/ta-architecture-reference.md` §5](../../architecture/ta-architecture-reference.md#5-agent-credential--authorization-model-current-state) (agent credential/authz model, current state) — this design closes every gap enumerated there: plaintext-at-rest storage, dead-code capability-token primitives, unenforced `ScopedCredential.scopes`, post-hoc-only mediation, and zero credential isolation on swarm fan-out.
**Process**: produced by a 10-agent design panel (4 parallel research agents on biscuit tokens / secret-broker patterns / human-escalation standards / swarm-attenuation integration points → 3 independent architecture proposals scored against 5 criteria → synthesis → 2 adversarial verification passes that read the actual current code against every claim). The two verification passes found real holes in the first synthesis; this document has those corrections folded in — see "Corrections from adversarial review" below before reading the staged rollout.

## Summary of the gap

Today a credential is a plaintext string from mint to use: `FileVault` stores it unencrypted at `.ta/credentials.json`; `bare_process.rs::apply_credentials_to_env` injects the raw value into the agent's process environment; the agent (and therefore the LLM driving it) can read it directly and can embed it in any tool call, where it is visible again in `ta-mcp-gateway`'s staged review JSON. The one real capability-token abstraction that exists (`CredentialVault::issue_token`/`validate_token`, `SessionToken`) is dead code nothing calls. Swarm fan-out (`run_one_swarm_sub_goal` in `apps/ta-cli/src/commands/run.rs`) spawns each sub-goal with `std::process::Command` and no `.env_clear()`, so every concurrent sub-goal inherits the parent's full environment, including every credential, with no narrowing of any kind.

This design replaces that model end to end with a broker-mediated, biscuit-token-based system delivered as seven independently shippable stages.

## Corrections from adversarial review

The first synthesized draft was checked by two independent adversarial passes that read the actual current code, not just the design's own claims. Both found real, code-grounded holes that change the staged rollout below:

1. **The dominant credential-leak path was undercounted.** TA's agents are Bash-driven coding agents — `apply_credentials_to_env` sets an env var on the agent's *entire process*, and that process runs `git push`, `gh pr create`, `curl -H "Authorization: Bearer $TOKEN"`, `npm publish`, `docker login` directly, none of which passes through any MCP tool-call interception. For a coding agent this shell/CLI usage is plausibly the *majority* of credentialed actions, not a fallback edge case. The original draft's Stage 3 described this as "a documented, reduced-security fallback" with no target date to close it — that undersold a path that defeats requirement (d) (agent/LLM never sees plaintext secrets) for most real usage. **Fixed by adding Stage 7 below**, promoted from an open question to a real, sized stage.
2. **Stage 3 was a hidden big-bang.** The original draft assumed `ta-mcp-gateway::ToolCallInterceptor` "already sits on every tool call" and just needed "one more step." Verified false: `ToolCallInterceptor` is constructed and stored but its `.classify()` method is never actually invoked outside its own tests — it is itself dead code, in the same category as `CredentialVault`. There is no live synchronous request-interception/substitution point anywhere in the codebase today; building one is new middleware architecture (parse tool-call before dispatch, block on an authorization decision, rewrite the outbound payload, relay the response), not an incremental addition to something live. **Fixed by resizing Stage 3 below and stating this explicitly.**
3. **Stage 1 targeted the wrong process.** `run_one_swarm_sub_goal` doesn't spawn the agent directly — it spawns a *nested `ta run <title>` CLI invocation*, which independently resolves its own credentials deep inside the child process via its own `BareProcessRuntime::spawn`/`apply_credentials_to_env` call. `.env_clear()` on the outer wrapper only blocks ambient-shell-env leakage; it does not narrow what the child `ta run` resolves internally from `.ta/credentials.json`. Real per-sub-goal narrowing requires an explicit handoff (a scope-carrying CLI flag, temp file, or env var) into the recursive `ta run` call. **Fixed in Stage 1's description below.**
4. **A factual claim was wrong.** The draft's Open Question 7 called `biscuit-auth` "Eclipse-Foundation-licensed." Confirmed via crates.io: `biscuit-auth` is **Apache-2.0** licensed (the Biscuit *project* has Eclipse Foundation governance/branding, but the crate license is Apache-2.0). Corrected in Open Questions below.

## Target architecture

### Biscuit token model mapped onto TA's concepts

The daemon holds one root Ed25519 keypair. Every issued token is a `Biscuit` whose authority block encodes Datalog facts built directly from types TA already has:

- `credential($id)` — from `ScopedCredential.name` / the vault's credential id.
- `agent($goal_id)` — the requesting goal's id (sub-goals get their own fact per delegation hop).
- `uri($scheme, $path)` — reuses the existing `fs://workspace/<path>` / `gmail://` / connector URI scheme from `Artifact.resource_uri` / `PatchSet.target_uri`, so "what resource" is expressed the same way credentials-land and artifact-land already express it.
- `verb($v)` — the allowed operation set, sourced from `ScopedCredential.scopes` today, `PolicyCascade`-derived tomorrow.
- `security_tier($tier)` — the goal's `route()`-resolved tier (`read_only`/`suggest`/`auto`), so a token can be authorized differently depending on autonomy level without a second parallel check.
- `expiry($ttl)` — checked against an ambient `time()` fact the `Authorizer` injects at verification, not stored in the token itself.

Each `PolicyCascade` layer (built-in → project → workflow → agent profile → goal constitution → CLI flags) becomes one attenuating block appended at goal-configuration time. Because biscuit blocks can only add `check` constraints, never remove one, the cascade's existing tighten-only merge semantics and biscuit's block-chain semantics become literally the same operation instead of two models kept in sync by hand.

### The broker: where it sits

A new lightweight crate, `ta-credential-broker`, embedded as a library inside `ta-daemon` (not a separate OS process — TA's topology is already single-daemon, and a socket hop buys nothing today). It owns:
- `RawSecret { name, value }` — never serialized outside the broker's address space.
- `CapabilityToken(Biscuit)` — the only credential-shaped thing that ever reaches an agent process, a tool schema, or the LLM's context.
- The live implementation of the currently-dead `CredentialVault::issue_token`/`validate_token` trait (same trait surface, real body).
- A local revocation-ID denylist (`.ta/revoked-blocks.jsonl`), checked on every authorize call — cheap, since the daemon is the sole verifier.

`ta-mcp-gateway` becomes the enforcement/substitution point for MCP tool calls, but — per correction #2 above — this means *building* the live interception path (`ToolCallInterceptor` is not currently wired to anything), not repurposing an active one.

### End-to-end data flow, one agent action (MCP tool-call path)

1. At goal setup, `ta-daemon` calls `issue_token(credential_id, agent_id, scopes, ttl)` → a biscuit authority block, scopes derived from `PolicyCascade::merge` intersected with the credential's declared scopes.
2. `bare_process.rs::apply_credentials_to_env` no longer inserts `cred.value`. It inserts exactly `TA_CAPABILITY_TOKEN=<biscuit b64>`. The agent process and every prompt the LLM sees contain only this opaque token and symbolic connector ids ("github", "slack-ops") in tool schemas — never a secret value, never even a scoped-but-real one.
3. The agent's LLM decides to call a connector tool with `{connector: "github", verb: "repo.read", params: {...}}`. No credential material appears in this call.
4. The (newly built) gateway interception point extracts the biscuit from the goal's context, builds an `Authorizer` from the goal's Datalog facts, calls `.authorize()` against `{connector, verb, target_uri}`.
5. On success, the broker resolves the matching `RawSecret` and attaches it only to the gateway's *own* outbound HTTP call to GitHub. The result returned to the agent contains the response body only — the credential used to fetch it never round-trips back into agent-visible state.
6. On a scope deficit, control passes to human escalation (below) instead of a hard failure.

This closes the leak path for MCP-tool-call-mediated actions. It does **not**, on its own, close the shell/CLI path — see Stage 7.

## Swarm fan-out: zero-round-trip attenuation

The gate sits where the research located the real spawn point: `run_one_swarm_sub_goal` (`apps/ta-cli/src/commands/run.rs`, around line 1633), which constructs `Command::new(ta_bin).arg("run")...` — a **nested `ta run` CLI respawn**, not a direct agent spawn (see correction #3). Before each sub-goal closure is boxed, the parent's biscuit is attenuated **in-process**, no network call:

```
child_biscuit = parent_biscuit.attenuate(
    check if sub_goal_id($id), resource($r), allow($r) <- $r == sub_goal.declared_resource_glob
    + ttl = min(parent_remaining_ttl, sub_goal_wave_deadline)
)
```

Because biscuit's append operation is structurally incapable of removing an earlier `check`, the child token is provably narrower than the parent's. `cmd.env_clear()` is set before spawn, with only a non-secret baseline (`PATH`, `HOME`, etc.) re-added explicitly, and the attenuated biscuit is passed to the child **explicitly** (`TA_CAPABILITY_TOKEN=<child_biscuit>` on the `Command`, or a `--capability-token` CLI flag) — since the child is a recursive `ta run` process that would otherwise re-resolve its own credentials from `.ta/credentials.json` independently of anything the parent computed. Nested fan-out (a sub-goal spawning its own sub-sub-goals) repeats the identical operation recursively — `run_concurrently` itself stays generic and credential-blind exactly as today.

## Human escalation: composes with `ta_human_verify`, doesn't duplicate it

Trigger condition, at the gateway's interception point: `requested_scope ⊄ token.allowed_scopes` — a deterministic, mechanical comparison, the same "requested exceeds held" check every mature PAM system (CyberArk, Vault, AWS IAM Identity Center) uses, no LLM judgment needed to detect it.

On trigger, call `ta_human_verify` with a **structured** (not freeform) question carrying `{requested_scope, current_caveats, target_uri, goal_id, parent_goal_scope}` as context. This runs the existing opinion/validator/gate pipeline unchanged, with one addition: the validator stage gets a non-bypassable computational pre-check, `requested_scope ⊆ parent_goal_scope`, asserted before the LLM critique runs — any violation forces `verdict: Block` regardless of model output, making the "no child broader than parent" invariant structural even at the human-escalation boundary, not just at the biscuit layer. A new `credential_scope_elevation` workload type is added to `.ta/workflow.toml` with a stricter default `escalate_risk_score` than code-edit workloads.

On `Commit`: the broker mints a fresh, narrowly attenuated biscuit and the audit entry — reusing `.ta/human-verify-audit.jsonl` as-is — gets two additive fields, `granted_scope` and `ttl`. On `Block`/`Reject`/`Escalate`: falls through to the existing blocking `ta_ask_human` UI unchanged; the gateway returns a tool-call failure to the agent; no secret is ever touched. No new escalation surface, no new audit store, no new red-team sampling path.

## Secret storage: close it in the same effort, don't defer

`FileVault`'s plaintext-JSON-at-rest gap is already flagged in the module's own doc comment. Deferring it produces a design that closes every *in-transit* leak path while leaving the single biggest *at-rest* leak path (anyone who reads `.ta/credentials.json`, or a backup of it) untouched. Since Stage 2 already requires touching `vault.rs` to wire up live issuance, add age-based encryption at rest in the same stage. Key material for the age identity lives outside `.ta/` (OS keychain where available, falling back to a chmod-0600 file with a loud `ta doctor` warning on non-macOS/Linux-without-keychain environments).

## Staged rollout (PLAN.md phase-sized)

**Stage 1 — Enforce declared scopes at existing call sites, with a real handoff for swarm (S-M, ~3-5 days).** `apply_credentials_to_env` filters by `ScopedCredential.scopes`/`security_tier` before injecting (new plumbing needed — today it takes a credential list with no required-scope input to compare against). `run_one_swarm_sub_goal` gets `.env_clear()` **plus an explicit scope handoff into the recursive `ta run` invocation** (CLI flag or env var carrying the intersected scope set), since the child process resolves its own credentials independently and outer `.env_clear()` alone doesn't narrow that. No new crates. *Deliverable: swarm sub-goals are no longer full-environment clones of the parent, and the child's own credential resolution actually respects the narrower scope.*

**Stage 2 — Live token issuance + encryption at rest (S-M, ~1 week).** `CredentialVault::issue_token`/`validate_token` become the real path (UUID `SessionToken`, biscuit comes later — ship value now). `ta credential grant <id> --agent <goal_id> --scope <s> --ttl <n>` CLI subcommand. `FileVault` gains age encryption in the same pass. *Deliverable: every credential handed to a process traces to an issued, expiring token record; secrets at rest are encrypted.*

**Stage 3 — Build the gateway's live interception/substitution point (M-L, ~2-3 weeks — resized up from the original estimate; this is new middleware, not an addition to something live).** `ToolCallInterceptor` today is constructed but never invoked — this stage makes it (or a purpose-built replacement) a genuine synchronous pre-dispatch gate: parse the tool call before dispatch, authorize, substitute the real secret only on the gateway's own outbound call, relay the response. Tool schemas expose only symbolic connector ids. `ConnectorRegistry` (`.ta/connectors.toml`) plus a per-connector `broker_mediated: bool` flag allows connector-by-connector migration, not a flag day. `bare_process.rs`'s direct env injection becomes an explicitly-flagged reduced-security fallback until Stage 7 closes it. *Deliverable: requirement (d) is met for every gateway-mediated MCP tool call — but not yet for shell/CLI usage, see Stage 7.*

**Stage 4 — Migrate SessionToken to biscuit (M-L, ~1.5-2 weeks).** New `ta-credential-broker` crate (library, embedded in `ta-daemon`). `issue_token`/`validate_token` reimplemented on `biscuit-auth`'s `BiscuitBuilder`/`Authorizer` (crates.io `biscuit-auth`, Apache-2.0 licensed, actively maintained under the Eclipse Biscuit project). `PolicyCascade` layers become attenuating blocks. Revocation denylist in `.ta/`. `ScopedCredential` formally retired as a delivery type in favor of `RawSecret`/`CapabilityToken`. *Deliverable: offline, no-round-trip attenuation capability exists — the prerequisite for Stage 5.*

**Stage 5 — Swarm fan-out cryptographic attenuation (S-M, ~3-5 days).** Stage 1's manual scope-intersection/handoff is replaced with `biscuit.attenuate()` plus the same explicit token handoff into the recursive `ta run` call. *Deliverable: requirement (e) fully met — cryptographically provable narrower-than-parent for every concurrent sub-goal, structural rather than conventional.*

**Stage 6 — Human escalation for scope elevation (S, ~3-5 days).** `requested_scope ⊄ allowed_scopes` trigger wired into `ta_human_verify` with structured context; validator computational pre-check; `credential_scope_elevation` workload type in `.ta/workflow.toml`; additive `granted_scope`/`ttl` fields on `.ta/human-verify-audit.jsonl`. *Deliverable: requirement (b) fully integrated, zero new escalation surface.*

**Stage 7 — Shell/CLI credential isolation, the dominant real-world path (M, ~1.5-2 weeks). New stage, added by adversarial review.** Stages 1-6 close the MCP-tool-call leak path, but a Bash-driven coding agent's *majority* credentialed actions (`git push`, `gh pr create`, `npm publish`, `curl` with a bearer header, `docker login`) never touch an MCP tool call at all — today they get the raw secret via env injection, and nothing in Stages 1-6 changes that. This stage closes it with per-tool credential shims rather than a single environment secret: a `git-credential-helper` backed by the broker (git already supports pluggable credential helpers — no git behavior change needed, just point `credential.helper` at a local broker-backed binary), a `gh auth` shim for the GitHub CLI (same pattern — `gh` supports external auth token resolution), and for the general case, a local loopback HTTP(S) forward proxy the agent's shell environment is pointed at (`HTTPS_PROXY`) that injects the `Authorization` header for allow-listed hosts server-side and lets everything else pass through unmodified. The proxy approach requires the agent's process trust a local CA for TLS interception on allow-listed hosts only — flag this as a real design/trust tradeoff, not a footnote (see Open Questions). *Deliverable: requirement (d) is met for the actual dominant path, not just the MCP-tool-call minority of it.*

## What does NOT change

- `PolicyCascade`'s six-layer action-approval gating stays exactly as-is; this design governs *credential reachability*, a different axis from *action approval*.
- `ta_human_verify`'s opinion/validator/gate pipeline, its audit file, and its red-team sampling (`ta audit human-verify sample`) are reused unchanged, not rebuilt.
- `ta-mediation::ApiMediator`'s post-hoc stage/replay role for non-credentialed calls is untouched (note: it currently has its own separate, also-unwired `classify()` duplicating `ToolCallInterceptor`'s heuristics — worth deduplicating during Stage 3, not a new requirement but a cleanup opportunity while that code is being touched anyway).
- `ta-changeset::secret_scan` remains as the last-line net through and after the transition.
- Single-daemon topology is preserved — no distributed broker, no external HSM/Vault/KMS dependency introduced.
- `route()` and `SecurityLevel` signatures are unchanged; they're consumed as inputs to scope derivation, not rewritten.
- No Studio/TUI UX work is required by any of these seven stages.
- `run_concurrently` (`ta-workflow/concurrent.rs`) stays a generic, credential-blind thread pool; attenuation happens one layer up at the call site that already has per-sub-goal context.

## Open questions requiring a human decision

1. **PLAN.md phase placement.** v0.17.3-v0.17.5.3 and the v0.17.7.1-4/v0.18.4 workflow-graph-engine track are already queued. Does this land as a new v0.19.x track, or interleave with workflow-graph-engine (a future credential-broker approval flow was already flagged in that design as something to evaluate fitting into the graph model per constitution §16)?
2. **Workload identity attestation.** A SPIFFE-lite nonce handshake (proving *which process* presents a token) adds real hardening but real cost. Given TA's daemon already spawns every process directly, is implicit trust from the spawn call sufficient, or is explicit attestation worth adding — and if so, at Stage 4 or as a later stage?
3. **Age-key custody.** OS keychain vs. file-with-warning fallback for the vault's own encryption key — acceptable, or does this need its own small design pass?
4. **Stage 7's TLS-interception trust model.** The local forward-proxy approach for generic shell/CLI credential injection requires trusting a local CA inside the agent's process for allow-listed hosts. Is this an acceptable trust boundary for a locally-run agent, or does Stage 7 need a narrower approach (helper-binary shims only, for git/gh/npm/docker specifically, with the generic-proxy case dropped or deferred)?
5. **Cross-host swarm workers.** Not on the roadmap today, but if it ever is, that's the point where biscuit's offline-verification property starts paying for itself beyond what Stage 4 already captures — worth flagging now so Stage 4's block-encoding choices don't need rework later.
6. **Revocation TTL defaults.** Short TTLs keep the denylist small but increase re-issuance frequency; needs a concrete default (minutes vs. hours) and per-connector override policy.
7. **`biscuit-auth` dependency approval.** Apache-2.0 licensed (confirmed via crates.io; the Biscuit *project* has Eclipse Foundation governance, but the crate itself is Apache-2.0) — a new external crate dependency for a workspace that currently has none for this purpose. Needs explicit sign-off before Stage 4 starts, though license is not itself a blocker.
