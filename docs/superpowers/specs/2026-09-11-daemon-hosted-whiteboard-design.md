# Daemon-Hosted Whiteboard: Design

**Status:** Approved for planning (2026-09-11)
**Owner boundary:** TA core provides the mechanism (transport, MCP tool surface, auth). `ta-virtual-team` (private repo) owns the "virtual office" policy layer built on top of it — chief-of-staff triage, task-claim workflow rules, handoff conventions. This mirrors the same split already chosen for `ta-plan-wayfinder` (see `docs/superpowers/plans/2026-09-09-ta-plan-wayfinder-extraction.md`).

## Problem

`ta-agent-whiteboard` (v0.17.11.2) is real, unit-tested code that is structurally unreachable from any live goal run:

1. Each agent session spawns its own isolated `ta serve` MCP gateway subprocess (per `mcp-agent.json`). `InMemoryTransport` is explicitly single-process. With `transport = "memory"` — a valid-looking config value — two concurrent agents each see an *empty* whiteboard and report no conflict, even when one genuinely exists. This is not "unimplemented," it's a silent false negative in the exact safety feature (staged-resource conflict detection) it exists to provide, violating this project's Observability Mandate.
2. `presence.rs`, `discovery.rs`, `handoff.rs`, `tasks.rs` have zero callers anywhere outside their own crate's tests. Only the advisory pre-launch conflict check (`whiteboard_check.rs`) touches the transport at all, and even that inherits problem #1.
3. The daemon — the one genuinely long-lived, shared process per project — does not own the transport. It lives in the ephemeral per-agent gateway process instead, which is backwards from where shared coordination state belongs.

Discovered while diagnosing a real crash-looping test session (`ta-virtual-team`'s `model-tier-proof` team-session, 2026-09-10): a separate root cause (stale dependency pin missing the role-prompt-delivery fix, PR #611) was the proximate crash cause, but investigating it surfaced that the "exercise the whiteboard substrate for real" test item was never actually testable — there was no live path to reach it.

## Goals and scope

Three deployment shapes this must eventually serve:
1. **Single operator, local**, with web Wayfinder.
2. **Team shared** — clarified: this means shared plan/state via VCS (git), *not* a live multi-user network daemon. A LAN/VPN-shared-daemon mode for in-office/VPN teams is a real, separate future configuration, not required now.
3. **Self-hosted, private-label cloud** — the customer hosts their own instance on their own private cloud. This is *not* Trusted Autonomy or Wayfinder operating shared multi-tenant infrastructure. A future TA/Wayfinder-hosted multi-tenant option is possible later but out of scope here; if built, it would need real tenant isolation (see Deferred section).

**Key simplification:** because (2) is VCS-based and (3) is single-customer-per-deployment, all three shapes reduce architecturally to the same near-term case: **one daemon, one in-process shared state.** This pass targets that case, while pre-planning (not implementing) the LAN/VPN multi-daemon case so the design doesn't need a rewrite to add it later.

## Architecture

### 1. Transport ownership moves to the daemon

At daemon startup, it instantiates one transport via the existing `select_transport()` (today's `InMemoryTransport` semantics, just daemon-owned instead of per-agent-process-owned). Per-agent `ta serve` processes remain exactly as isolated as today for everything else — Claude Code sandboxing, per-agent credential scoping. For whiteboard operations specifically, they become thin RPC clients to the daemon.

### 2. New MCP tools, backed by daemon RPC

New tools registered in `ta-mcp-gateway`, mirroring the existing pattern `ta_goal_start`'s whiteboard pre-launch check already uses to reach the daemon (HTTP call, not direct transport instantiation):

- `ta_whiteboard_presence_register` — register this agent as active for a team-session/goal.
- `ta_whiteboard_presence_list` — list currently-active agents within scope.
- `ta_whiteboard_handoff_send` — send a message to another team member.
- `ta_whiteboard_handoff_receive` — poll/receive pending handoff messages.
- `ta_whiteboard_task_claim` / `ta_whiteboard_task_release` — single-consumer-by-design task claim primitive.

Each tool handler makes an authenticated HTTP call to a new daemon endpoint; the daemon is the only process that ever touches the transport directly.

### 3. Biscuit scope: `whiteboard:team_session:<session_id>`

Minted into the agent's `GrantedToken.allowed_scopes` (existing mechanism, `crates/ta-credential-broker/src/broker.rs`) at `ta team-session start` (or `ta_goal_start` for a standalone goal with whiteboard enabled), alongside its existing credential scopes. The daemon verifies this scope on every whiteboard RPC call before servicing it. An agent can only see/act within the team-session it was actually issued into — this is also the natural tenant boundary if a hosted multi-tenant option is ever built later (each tenant's daemon only ever issues scopes for its own sessions; no cross-daemon scope forgery is possible since verification is against the issuing daemon's own root key).

### 4. LAN/VPN pre-planning (not implemented this pass)

The transport trait already supports swapping in `NatsTransport` (existing code). This design doesn't change that swap point — `select_transport()` still reads `.ta/workflow.toml`'s `[whiteboard]` config (`transport = "nats"`, `nats_url`), we're only moving *where* it's called from (daemon startup, not per-agent-process). Pre-planning requirements so this doesn't need a rewrite later:

- Presence records must carry a daemon/host identifier field (not assume "the daemon" is singular), even though it's unused/always-self in the single-daemon case.
- The new MCP tool RPC contract must not assume same-machine locality in its request/response shapes.
- Document (don't implement) the config shape for a future `[whiteboard] transport = "nats"` multi-daemon deployment.

### 5. Observability

Every whiteboard RPC failure (auth rejection, transport error, `[whiteboard] enabled = false`) returns a structured, actionable error through the MCP tool result — never a silent empty result. This directly closes the false-negative gap found in the red-team pass.

## Data flow example

A team-session with two roles (`chief-of-staff`, `implementer`) running as two separate concurrent goals:

1. `ta team-session start` mints each agent a Biscuit token with `whiteboard:team_session:<id>` alongside its normal scopes, then launches each role's agent in its own `ta serve` subprocess as today.
2. Each agent calls `ta_whiteboard_presence_register` on startup. The MCP tool handler calls the daemon's `/api/whiteboard/presence` endpoint with the agent's token; the daemon verifies the scope, then records presence in its in-process transport.
3. `chief-of-staff` calls `ta_whiteboard_handoff_send` targeting `implementer`; the daemon validates both the sender's scope and that the target is a valid member of the same session, then appends to the transport's message stream.
4. `implementer` calls `ta_whiteboard_handoff_receive` (or the daemon proactively surfaces it — implementation detail for the plan) and gets the message.
5. `ta_whiteboard_presence_list`, called by either agent, shows both agents active — this is the concrete, live proof that today's crash-looping/false-negative failure mode is fixed.

## Testing strategy

- Unit tests already exist for `InMemoryTransport`'s presence/discovery/handoff/tasks logic (unchanged, still correct).
- New integration tests: daemon-hosted transport reachable via the new MCP tools from a real (not mocked) `ta serve` subprocess round-trip.
- New regression test: two concurrently-running goals against the same daemon both see each other via `presence_list` — the test that would have caught problem #1 above.
- Live dogfood test (this repo, `ta-virtual-team`): re-run the Phase 1 test plan's item 3, this time with a genuinely reachable whiteboard.

## Deferred (not in this design; flagged for separate planning)

- **Capacity data**: how many concurrent agent connections/RPC calls a single daemon can sustain. No existing benchmark. Recommended as a prerequisite research/benchmarking task before any scaling design is written — do not spec numeric limits without data.
- **Horizontal scaling / sharding per logical execution unit**: real requirement, but this is a **Secure Autonomy (SA)** enterprise concern, not TA core or the virtual-team add-on. Tracked separately in TA's `PLAN.md` under the SA/v0.18.x track, not detailed in this spec.
- **Hosted multi-tenant Wayfinder option** (if TA/Wayfinder ever operates shared infrastructure rather than customers self-hosting): would need real tenant isolation beyond the per-session Biscuit scope described here (e.g. namespacing at the transport level, not just the MCP auth level). Not needed for the private-label self-host model this design targets.
