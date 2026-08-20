# Agent Coordination System ("agent-whiteboard") — Design Options & Recommendation

> Research spike, 2026-08-19. Triggered by the v0.17.10.2 concurrent-goal staging isolation bug (real data loss found live via an external report) and the broader v0.17.11 virtual-team push. Scope: how independently-running agents in a virtual team advertise current activity, discover what siblings are doing now/next, and hand off work — safely, locally and in a hosted/distributed daemon deployment.

---

## 1. Problem statement

TA can already run multiple agents concurrently (the `swarm` workflow, `v0.13.7`/`v0.17.0.12.34`), but concurrency safety today comes entirely from **static, plan-time declarations**: `SubGoalSpec.depends_on` and `SubGoalSpec.api_impact` let `task-graph`'s `compute_waves` serialize sub-goals whose declared surfaces overlap, before anything runs. This works when conflicts are knowable in advance.

It does not cover:

- **Undeclared/unknowable overlap** — exactly the v0.17.10.2 bug: two goals were launched with the same `source` directory, no `depends_on`/`api_impact` between them, and nothing in the system today would have flagged that as a conflict before launch.
- **Live status visibility** — a running agent has no way to ask "what is anyone else doing right now" or "is anyone about to touch this file/module."
- **Opportunistic coordination** — ad-hoc, mid-task signals ("I just changed the token schema, heads up," "I'm blocked on X, can someone unblock me") that aren't expressible as a pre-declared dependency.
- **Handoffs between peers** — `AgentAction::Escalate` (`crates/ta-session/src/agent_action.rs:134`) is the closest existing primitive, but it's a single-recipient, role-directed action embedded in one team session's sequential stage loop (`team_session.rs`: "fires one `ta run` goal per role **in sequence**"). It doesn't support broadcast, discovery, or peer-to-peer handoff between independently-running goals.

This is the gap "agent-whiteboard" is meant to fill: a **live, runtime coordination layer**, complementary to (not a replacement for) `task-graph`'s static wave planning.

---

## 2. Requirements

1. **Local-first**: works with zero external infrastructure for a single-laptop `ta daemon` dev setup — this is a hard requirement per TA's existing local-first architecture (`MEMORY.md`: "local-first substrate").
2. **Distributed-capable**: the same mechanism must work when the daemon is hosted (e.g. Render) and goal-agents run against shared GPU services or multiple concurrent virtual-team members across machines — without a second implementation.
3. **Presence/activity advertisement**: an agent can publish "what I'm doing right now" (goal ID, phase, files/resources touched, expected duration) and have it expire automatically if the agent dies (no manual cleanup).
4. **Discovery/query**: any agent (or the daemon itself) can ask "what is everyone doing now/next" without polling a database table.
5. **Durable handoff messages**: a message sent to a peer that isn't currently listening must not be silently lost — this is the same durability class as the `.ta-decisions.json`/audit-log gaps found in the v0.17.10.2 investigation.
6. **Low operational burden**: no multi-node consensus cluster to run for a feature whose primary consumer is presence/pub-sub traffic, not strongly-consistent locking.
7. **Rust-native, embeddable as a library**: per the user's stated preference — a new shared crate, not a wrapper around a heavyweight external service.

---

## 3. Options considered

### 3.1 Extend the existing `FsEventStore`/in-process `EventBus`

TA already has `crates/ta-events`: an in-process `EventBus` (`bus.rs`, tokio `broadcast` channel) and a file-based `FsEventStore` (`store.rs`, appends `EventEnvelope`s to `.ta/events/`).

- **Local fit**: good — it's already there, zero new dependencies.
- **Distributed fit**: poor. `EventBus` is in-process only (a `tokio::sync::broadcast::Sender` doesn't cross process boundaries). `FsEventStore` is a shared-filesystem append log — exactly the pattern that caused the v0.17.10.2 corruption (`.ta-decisions.json` collisions) when multiple processes wrote to the same path. Making it safe for concurrent multi-writer, multi-machine use would mean re-deriving most of what a real message broker already provides (durability, delivery semantics, TTL expiry), on top of a shared network filesystem — a materially worse foundation than a purpose-built broker.
- **Verdict**: keep using it for what it's good at (single-process eventing, durable append-only audit trail) but do not build the whiteboard on top of it.

### 3.2 NATS + JetStream

A single self-contained server binary (also embeddable with no external process for pure single-node use) that natively provides pub/sub subjects, a KV store (with per-key TTL), and JetStream streams/consumers for at-least-once/exactly-once durable delivery.

- **Local fit**: excellent — one binary, "up and running in minutes," no external dependency.
- **Distributed fit**: excellent — the *same* binary clusters (and super-clusters) for HA/scale, and supports "leaf node" edge deployments that keep local state and resync on reconnect. This is the one option researched that gives a **single codepath** for both local-dev and hosted-multi-tenant, rather than two different deployment stories.
- **Primitive mapping**:
  - Presence/activity → pub/sub subject per agent (`agent.<id>.activity`) or a JetStream KV bucket keyed by agent ID with TTL — liveness expiry is native, not something we build.
  - Discovery/query → JetStream KV `get`/`watch`, or request/reply on a well-known subject.
  - Durable handoff → JetStream streams + consumers, built-in delivery guarantees.
- **Rust ecosystem**: `async-nats` crate, v0.50.0 as of this research (released ~3 weeks prior), Tokio-native, actively maintained, Apache-2.0. Still 0.x-versioned, but per maintainers that reflects the async ecosystem's own dependencies (e.g. `rustls`) not stabilizing, not neglect — it's in broad production use despite the version number.
- **Operational footprint**: 10–50MB base memory, 100–500MB under load, single 2-core node handles millions of msgs/sec. Cheap enough to run as a Render Background Worker alongside the daemon.
- **Open item**: no Render-specific NATS deployment guidance was found in this research pass — confirm private-network reachability between a NATS Background Worker and the main daemon service on Render before committing.

### 3.3 Redis (Streams + Pub/Sub)

- **Local fit**: weaker — no embedded/local-only mode; needs an actual external Redis process even for single-laptop dev. This means local-dev and hosted deployment diverge on day one, the exact two-codepath problem NATS avoids.
- **Distributed fit**: fine, well-trodden (Redis Streams gives consumer groups and durability comparable to JetStream).
- **Caveat**: plain Pub/Sub (the simpler, more commonly reached-for half of Redis) has **no delivery guarantee** — a disconnected agent silently misses a message. That's disqualifying for handoff messages specifically unless the Streams API is used deliberately throughout, which starts to look like reimplementing JetStream's guarantees on a less purpose-built substrate.
- **Rust ecosystem**: solid (`redis-rs`), and there's already a real-world reference project for exactly this use case (`vitaminR/agent-switchboard` — Redis Streams/Pub-Sub for agent collaboration via MCP), confirming the pattern works, just with the local/hosted divergence cost above.
- **Verdict**: credible runner-up, not the recommendation, mainly because of the local-embedding gap and the Pub/Sub-vs-Streams durability trap.

### 3.4 etcd / Consul

Both are built for strongly-consistent config + distributed locking + service discovery (etcd: Raft-backed, very good at write-heavy locking; Consul: agent-based, multi-datacenter, health-checking) — not for broadcast-style "many readers watch one writer's activity" traffic. Neither has a native pub/sub-shaped primitive; you'd fake presence with watch-on-key semantics, more friction than NATS's KV+pub/sub combo gives natively.

**Worth keeping in the toolbox for a narrower, separate need**: v0.17.10.2 Item 3 (a per-`source_dir` advisory lock so concurrent goals against the same source serialize) is exactly the kind of strongly-consistent mutual-exclusion problem etcd/Consul are built for. That's a different, smaller need than the whiteboard's broadcast/presence traffic — don't conflate the two. A file-lock (as v0.17.10.1's `ApplyLock` already does for `workspace_root`) is likely sufficient for that narrower case without introducing etcd at all; flagging the option here for completeness in case a distributed multi-daemon deployment later needs real distributed locks.

### 3.5 `foca` (Rust SWIM gossip)

Confirmed real and maintained, `no_std`+`alloc`, transport-agnostic. Solves membership/failure-detection ("who's alive") only — no message/state payload semantics beyond that. Could underlie liveness detection but doesn't replace a messaging layer on its own. Not a standalone contender; a NATS KV TTL already gives equivalent-enough liveness expiry for this use case with far less new surface area.

### 3.6 Rust actor frameworks (`ractor`, `coerce`)

Both are genuinely distributed-capable (`ractor` has a companion `ractor_cluster` crate; `coerce` is built for remote actors from the start), but both mean inheriting an actor supervision-tree/lifecycle model for the *entire* affected subsystem, not just whiteboard traffic — a much bigger architectural commitment. `ractor_cluster`'s own docs flag cluster mode as "not production ready but relatively stable" — a real yellow flag for a 2026 production dependency. Not recommended as the whiteboard's transport.

### 3.7 Kafka / Pulsar / Temporal / Restate

- **Kafka/Pulsar**: confirmed too heavy — Kafka needs a 3-broker minimum plus ZooKeeper/KRaft overhead for what is, for TA's purposes, lightweight presence/coordination traffic. Not a contender.
- **Temporal/Restate**: solve a different problem — durable *execution* (guaranteeing a long-running workflow survives failures and replays correctly), not live peer-to-peer presence/messaging. Genuinely interesting for a **separate** future concern (making a `ta run` goal's own execution durable/resumable across daemon restarts), but out of scope for the whiteboard itself. Worth a note as a related-but-distinct future direction.

### 3.8 How other multi-agent frameworks solve this (context, not a candidate to adopt wholesale)

None of the surveyed frameworks solve *distributed, cross-machine* coordination as a first-class concern — they all assume one process/runtime owns the shared state:

- **LangGraph**: typed shared state graph, but state lives in one process/checkpoint store.
- **AutoGen → AG2 / Microsoft Agent Framework** (AutoGen now in maintenance mode as of Apr 2026): `GroupChat` selector pattern, single shared conversation transcript.
- **CrewAI**: explicitly weaker here — no built-in checkpointing, limited agent-to-agent communication control. Not a model to copy.
- **OpenAI Agents SDK**: `handoff` primitive — explicit control transfer carrying context, but in-process/single-runtime only.
- **Anthropic's own published architecture**: orchestrator-worker (central dispatch), not blackboard/peer-gossip.
- **Google A2A (Agent2Agent)**: the one genuinely relevant cross-vendor convention — "Agent Cards" advertise capability, "Tasks" structure exchanged work, over HTTP/SSE/JSON-RPC. 150+ organizations as of April 2026, positioned as complementary to MCP (MCP = tool integration, A2A = cross-agent coordination). Worth borrowing **vocabulary** from (Agent Card ≈ our activity-advertisement concept) even without adopting the wire protocol — it's becoming a recognizable convention worth aligning naming with.

This reinforces that the coordination layer is TA's own infrastructure to build — no framework solves the distributed case out of the box.

---

## 4. Comparison

| | Local (zero-infra) | Distributed/hosted | Delivery guarantee for handoffs | Native presence/TTL | Ops footprint | Rust maturity |
|---|---|---|---|---|---|---|
| `FsEventStore` extension | ✅ (already exists) | ❌ (shared-FS multi-writer = the bug we just fixed) | ❌ | ❌ (build it) | none (already running) | n/a, in-tree |
| **NATS + JetStream** | ✅ single binary | ✅ same binary, clusters | ✅ JetStream | ✅ KV TTL | Low (10–50MB idle) | Good (`async-nats` 0.50, active) |
| Redis Streams | ❌ needs external process even locally | ✅ | ✅ (Streams; ⚠️ not Pub/Sub) | ⚠️ (build via key TTL) | Low–medium | Good (`redis-rs`) |
| etcd/Consul | ⚠️ possible but wrong shape | ✅ | N/A (not messaging-shaped) | ⚠️ (watch-on-key, indirect) | Medium (cluster) | OK, but wrong primitive |
| `foca` (SWIM) | ✅ | ✅ | ❌ (membership only) | ✅ (its whole job) | Low | Real, maintained, narrow scope |
| `ractor`/`coerce` | ✅ | ⚠️ (`ractor_cluster` flagged not-production-ready) | Depends on design | Build it | Medium (own supervision model) | Real but big commitment |
| Kafka/Pulsar | ❌ | ✅ | ✅ | Build it | High | Wrong weight class |

---

## 5. Recommendation

**Build a new crate, `ta-agent-whiteboard`, as a thin schema/semantics layer on top of NATS + JetStream** — don't build a new transport, and don't adopt a heavier framework's full model.

**Rationale**:
1. NATS is the only option researched that gives one codepath for both requirement (1) local-first and (2) distributed-capable — same binary, embedded locally, clustered when hosted. Every other credible option (Redis, etcd/Consul) forces a local/hosted deployment split.
2. Its native primitives map directly onto every functional requirement in §2 without custom durability/TTL engineering: pub/sub for broadcast, JetStream KV with TTL for expiring presence, JetStream streams/consumers for durable handoff delivery.
3. It keeps the actual new code TA owns small and focused: message schemas (activity records, presence heartbeats, handoff envelopes), subject-naming conventions, and query helpers — not a home-grown broker, not an actor supervision tree, not a distributed lock service. That fits the "new shared library" framing the user asked for: a library that defines *what* gets coordinated, riding on infrastructure that already solves *how* messages move reliably.
4. Operational cost is low enough to run as a single additional Render Background Worker alongside the existing daemon (10–50MB idle) — this should be confirmed against Render's current service catalog before committing (see open item in §3.2), but nothing in the research suggests it's a blocker.

**What NOT to build**: a new distributed lock service (use file-based locks, e.g. extending `ApplyLock`'s pattern, for the narrower v0.17.10.2 Item 3 need — see §3.4), a new durable-execution engine (Temporal/Restate territory, a separate future concern), or a full actor framework migration.

**Naming alignment**: borrow "Agent Card"-style vocabulary from Google's A2A protocol for the activity-advertisement schema (capability + current task + status), since it's becoming a recognizable cross-vendor convention — without adopting A2A's wire protocol itself (HTTP/SSE/JSON-RPC would be redundant with NATS as the transport).

---

## 6. Sketch: what `ta-agent-whiteboard` actually contains

Not a full spec — enough to scope a PLAN.md phase:

- **Presence records** (JetStream KV, TTL'd): `agent_id`, `goal_run_id`, `source_dir`, current phase/stage, resources being touched (file globs / `api_impact` tokens — reuse `task-graph`'s existing vocabulary rather than inventing a second one), last-heartbeat timestamp. TTL expiry means a crashed agent's presence disappears automatically — no manual cleanup, unlike the orphaned-goal-record pattern already known in TA (`ta draft close` not transitioning parent goal state).
- **Discovery/query API**: "who else is active," "is anyone touching `<path>`/`<api_impact tag>`right now" — a thin wrapper over KV `get`/`watch`, callable from both the daemon (for pre-launch conflict checks, directly relevant to fixing v0.17.10.2's root cause class more generally) and from within a running agent (for opportunistic self-coordination).
- **Handoff messages** (JetStream stream): sender, recipient (agent ID or role, extending `RoleRef` from `crates/ta-session/src/agent_action.rs`), payload, durable until acknowledged — a broadcastable, peer-to-peer generalization of the existing single-recipient `AgentAction::Escalate`.
- **Integration points**: `ta_goal_start`/`launch_goal_agent` (`crates/ta-mcp-gateway/src/tools/goal.rs`) could consult the whiteboard before launch as a *live* complement to `task-graph`'s *static* wave planning — directly relevant to preventing another v0.17.10.2-class incident where overlap wasn't declared in advance. `team_session.rs`'s currently-sequential stage loop is a natural first internal consumer if/when it moves toward concurrent stage execution.

---

## 7. Open items before implementation

1. Confirm NATS deployment shape on Render specifically (Background Worker + private networking reachability from the daemon service) — not verified in this research pass.
2. Decide whether `ta-agent-whiteboard` ships inside this repo initially or is extracted as a standalone OSS crate immediately (matching the `task-graph`/`consensus-panel` precedent) — recommend building in-tree first against real usage, extract once the schema stabilizes, consistent with how `task-graph` itself was extracted only after `dependency_wave.rs` proved out inside `ta-workflow`.
3. Scope a PLAN.md phase (proposed: part of the v0.17.11 virtual-team work, or its own v0.17.11.x sub-phase) once this design is approved.
