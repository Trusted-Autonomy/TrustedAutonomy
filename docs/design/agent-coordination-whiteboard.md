# Agent Coordination System ("agent-whiteboard") — Design Options & Recommendation

> Research spike, 2026-08-19 (updated same day with a second pass on current multi-agent codebase-coordination practice, including Claude Code's own native Agent Teams / cross-session `SendMessage` primitives). Triggered by the v0.17.10.2 concurrent-goal staging isolation bug (real data loss found live via an external report) and the broader v0.17.11 virtual-team push. Scope: how independently-running agents in a virtual team advertise current activity, discover what siblings are doing now/next, and hand off work — safely, locally and in a hosted/distributed daemon deployment.

---

## 1. Problem statement

TA can already run multiple agents concurrently (the `swarm` workflow, `v0.13.7`/`v0.17.0.12.34`), but concurrency safety today comes entirely from **static, plan-time declarations**: `SubGoalSpec.depends_on` and `SubGoalSpec.api_impact` let `task-graph`'s `compute_waves` serialize sub-goals whose declared surfaces overlap, before anything runs. This works when conflicts are knowable in advance.

It does not cover:

- **Undeclared/unknowable overlap** — exactly the v0.17.10.2 bug: two goals were launched with the same `source` directory, no `depends_on`/`api_impact` between them, and nothing in the system today would have flagged that as a conflict before launch.
- **Live status visibility** — a running agent has no way to ask "what is anyone else doing right now" or "is anyone about to touch this file/module."
- **Opportunistic coordination** — ad-hoc, mid-task signals ("I just changed the token schema, heads up," "I'm blocked on X, can someone unblock me") that aren't expressible as a pre-declared dependency.
- **Handoffs between peers** — `AgentAction::Escalate` (`crates/ta-session/src/agent_action.rs:134`) is the closest existing primitive, but it's a single-recipient, role-directed action embedded in one team session's sequential stage loop (`team_session.rs`: "fires one `ta run` goal per role **in sequence**"). It doesn't support broadcast, discovery, or peer-to-peer handoff between independently-running goals.

**This is not a hypothetical problem.** A 2026 dataset study of AI-agent-authored GitHub PRs ("AgenticFlict," arXiv:2604.03551, ACM AI-Powered Software 2026) found **cross-agent PR pairs hit merge conflicts 41.7% of the time, vs. 19.8% for intra-agent pairs** (non-overlapping confidence intervals) — concurrent agents editing a shared codebase collide roughly twice as often as a single agent working sequentially with itself. Separately, an empirical multi-agent study (Pebblous, Aug 2026) observed 18 of 30 independently-launched agents choose the *identical* git branch name absent explicit namespacing — "low variance is what alignment leaves behind." Collision is the default outcome of concurrent agents on shared state, not an edge case; both findings validate that v0.17.10.2 was a specific instance of a general, measured pattern.

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
8. **Agent-runtime-agnostic**: TA's own agents are not exclusively Claude Code — `ta-agent-ollama` runs local models as agents today, and the architecture should not assume every coordinating participant is a Claude Code process specifically (see §3.6).

---

## 3. Prior art: how multiple coding agents coordinate on shared codebases today (2026)

### 3.1 Git-worktree isolation tools

The current landscape of tools that run several coding-agent instances concurrently against one codebase, all converging on the same base pattern — **worktree-per-agent isolation, no live cross-agent coordination, human/lead reviews and resolves at merge time**:

- **Conductor** (conductor.build, Melty Labs) — macOS app, spins up a git worktree per Claude Code/Codex agent in ~10s, auto-names branches, integrates Linear/GitHub. "Hand off plans between agents" is a UI-mediated human action, not agent-to-agent communication.
- **Crystal** — open-source desktop app, same worktree-per-session model.
- **Claude Squad** — terminal UI, tmux session + git worktree per agent.
- **vibe-kanban** (BloopAI, Rust) — kanban board mapped to git state; design philosophy stated directly: "Agent A messes with `auth.ts`, Agent B with `user.ts`, and neither steps on the other's toes." Isolation *is* the entire conflict strategy; PR review/creation happens after the fact, by a human.

None of these tools have a live "what is everyone doing" query or presence layer. They solve filesystem isolation, not coordination — confirming agent-whiteboard addresses a gap unfilled even by the most current tooling in this space.

### 3.2 Claude Code's own native coordination primitives — Agent Teams and cross-session `SendMessage`

This is directly relevant to the "agent-teams via SendMessage" question, and is not hypothetical: it's a real, currently-shipped (experimental) Anthropic feature, described here from the official docs (`code.claude.com/docs/en/agent-teams`, `.../cross-session-messaging`), not third-party summary.

**Agent Teams** (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`): one lead session spawns named teammates — separate full Claude Code sessions, each with its own context window, none inheriting the lead's conversation history. Coordination runs on two primitives:
- **A shared task list**: items are pending/in-progress/completed, can declare dependencies (a task can't be claimed until its dependencies complete), and teammates can self-claim the next unblocked item. **Task claiming uses file locking** to prevent two teammates claiming the same task in a race — a concrete, shipped precedent for exactly the kind of claim-arbitration TA's v0.17.10.2 Item 3 (per-`source_dir` advisory lock) needs.
- **A mailbox**: each agent has a JSON inbox file (`~/.claude/teams/{team}/inboxes/{agent}.json`), delivered via the `SendMessage` tool.

**The critical limitation, stated in Anthropic's own docs**: *"Avoid file conflicts: Two teammates editing the same file leads to overwrites. Break the work so each teammate owns a different set of files."* Agent Teams has **no file-level conflict prevention mechanism**. It relies entirely on the human or lead decomposing work into non-overlapping file ownership up front — the same static-declaration strategy TA's `task-graph`/`api_impact` already implements, just without the tooling to check it. This means TA's existing wave-scheduling is already doing something Anthropic's own official feature does not.

**Scope limitation**: single machine, one team per session for that session's lifetime, no nested teams. Git worktrees are explicitly listed as the *alternative* to Agent Teams for manual parallel sessions, not something Agent Teams integrates with. There is no distributed/multi-machine mode.

**Cross-session `SendMessage`/`ListAgents`** (separate feature, this is the literal mechanism used elsewhere in this very conversation): lets one Claude Code session message another — locally via a per-session Unix domain socket "inbox," or across machines by relaying through Anthropic's own Remote Control infrastructure. Important properties:
- **Not durable**: three delivery outcomes (Delivered / Held / Refused); a session's held-message queue caps at 100 and drops the oldest past that; no retry-until-delivered semantics for an unreachable target.
- **Trust boundary**: a received message is explicitly flagged to the receiving Claude as coming from "another session, not you" — it cannot approve pending permission prompts, change configuration, or have embedded commands executed on its behalf. This mirrors TA's own least-privilege/credential-broker philosophy (v0.17.6.x) and is worth adopting as a pattern for whiteboard messages generally.
- **Platform gap**: macOS and Linux only — not available on native Windows, and not available on Bedrock/Vertex/Microsoft Foundry provider configurations.

### 3.3 Gas Town — merge-queue-as-orchestration (Steve Yegge, OSS, v1.0 shipped 2026)

A more elaborate production pattern, worth naming as a real alternative architectural style: named roles — **Mayor** (orchestrator), **Polecats** (ephemeral task workers), **Refinery** (owns a Bors-style bisecting merge queue — batches completed work, runs verification gates, merges to main), **Witness** (worker health monitoring), **Deacon** (patrol loops). Its most distinctive choice: coordination state lives in **Beads**, a git-backed issue tracker acting as both data plane and control plane, rather than a separate message broker or database. This trades live/low-latency coordination for full durability-via-git and a single source of truth that's inherently versioned and auditable.

### 3.4 Mainstream multi-agent frameworks and cross-vendor conventions

None of the surveyed general-purpose multi-agent frameworks solve *distributed, cross-machine* coordination as a first-class concern — each assumes one process/runtime owns the shared state:

- **LangGraph**: typed shared state graph, but state lives in one process/checkpoint store.
- **AutoGen → AG2 / Microsoft Agent Framework** (AutoGen now in maintenance mode as of Apr 2026): `GroupChat` selector pattern, single shared conversation transcript.
- **CrewAI**: explicitly weaker here — no built-in checkpointing, limited agent-to-agent communication control.
- **OpenAI Agents SDK**: `handoff` primitive — explicit control transfer carrying context, but in-process/single-runtime only.
- **Anthropic's own published multi-agent architecture** (separate from the Claude Code product features above): orchestrator-worker (central dispatch), not blackboard/peer-gossip.
- **Google A2A (Agent2Agent)**: the one genuinely relevant cross-vendor convention — "Agent Cards" advertise capability, "Tasks" structure exchanged work, transport over HTTP/SSE/JSON-RPC. 150+ organizations as of April 2026, positioned as complementary to MCP (MCP = tool integration, A2A = cross-agent coordination). Worth borrowing **vocabulary** from (Agent Card ≈ our activity-advertisement concept) without adopting the wire protocol, since NATS already serves as transport.

Even Anthropic's own official, currently-shipped Agent Teams feature is single-machine only — reinforcing that distributed coordination remains TA's own infrastructure to build; nothing evaluated solves it out of the box.

### 3.5 Why not build directly on Claude Code's own Agent Teams / `SendMessage`

Given TA's goal-agents are, today, literally Claude Code processes (`agent_id: "claude-code"` throughout the codebase), it's worth asking directly whether TA should just use Claude Code's own native coordination instead of building `ta-agent-whiteboard`. The answer is no, for reasons specific to TA's architecture:

1. **Not agent-runtime-agnostic**: `ta-agent-ollama` runs local models as first-class TA agents today. A coordination layer wired to Claude-Code-specific IPC (Unix socket inboxes, `SendMessage`) would exclude every non-Claude-Code agent TA supports or might support — violating requirement §2.8.
2. **Not durable**: as detailed in §3.2, cross-session messaging is explicitly best-effort with a small bounded queue. Fine for interactive, human-supervised sessions where a held-message dialog can be shown; wrong for TA's headless, detached `ta run --headless` goal-agents, which have no interactive surface to approve or notice a held message.
3. **Cross-machine path is the wrong trust boundary**: Claude Code's own cross-machine messaging relays through Anthropic's Remote Control infrastructure. TA's hosted daemon-to-GPU-service architecture (Render daemon shells out to owned/managed GPU infra) should not route internal coordination traffic through a third party as a structural dependency.
4. **No Windows support**: cross-session messaging is macOS/Linux only; TA ships a Windows build and CI target.
5. **No discovery/query primitive**: `SendMessage`/`ListAgents` is point-to-point addressed messaging, not a queryable presence board — it can't answer "who is touching `src/auth/`" without every agent already knowing every other agent's name to ask individually.

None of this diminishes the value of Agent Teams' *schema* as validation — see §7.

---

## 4. Transport/substrate options considered

### 4.1 Extend the existing `FsEventStore`/in-process `EventBus`

TA already has `crates/ta-events`: an in-process `EventBus` (`bus.rs`, tokio `broadcast` channel) and a file-based `FsEventStore` (`store.rs`, appends `EventEnvelope`s to `.ta/events/`).

- **Local fit**: good — it's already there, zero new dependencies.
- **Distributed fit**: poor. `EventBus` is in-process only. `FsEventStore` is a shared-filesystem append log — exactly the pattern that caused the v0.17.10.2 corruption (`.ta-decisions.json` collisions) when multiple processes wrote to the same path. Making it safe for concurrent multi-writer, multi-machine use would mean re-deriving most of what a real message broker already provides, on a worse foundation.
- **Verdict**: keep using it for what it's good at (single-process eventing, durable append-only audit trail) but do not build the whiteboard on top of it.

### 4.2 NATS + JetStream

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
- **Also agent-runtime-agnostic** (§2.8, §3.5) by construction — any process that speaks the NATS wire protocol participates, Claude Code or Ollama-backed or otherwise, unlike Claude-Code-specific IPC.

### 4.3 Redis (Streams + Pub/Sub)

- **Local fit**: weaker — no embedded/local-only mode; needs an actual external Redis process even for single-laptop dev. This means local-dev and hosted deployment diverge on day one, the exact two-codepath problem NATS avoids.
- **Distributed fit**: fine, well-trodden (Redis Streams gives consumer groups and durability comparable to JetStream).
- **Caveat**: plain Pub/Sub has **no delivery guarantee** — a disconnected agent silently misses a message. Disqualifying for handoff messages unless the Streams API is used deliberately throughout, which starts to look like reimplementing JetStream's guarantees on a less purpose-built substrate.
- **Rust ecosystem**: solid (`redis-rs`), and there's a real-world reference project for exactly this use case (`vitaminR/agent-switchboard` — Redis Streams/Pub-Sub for agent collaboration via MCP), confirming the pattern works, just with the local/hosted divergence cost above.
- **Verdict**: credible runner-up, not the recommendation.

### 4.4 etcd / Consul

Both are built for strongly-consistent config + distributed locking + service discovery — not for broadcast-style "many readers watch one writer's activity" traffic. Neither has a native pub/sub-shaped primitive.

**Worth keeping in the toolbox for a narrower, separate need**: v0.17.10.2 Item 3 (a per-`source_dir` advisory lock so concurrent goals against the same source serialize) is exactly the kind of strongly-consistent mutual-exclusion problem etcd/Consul are built for. A file-lock (as `ApplyLock` already does for `workspace_root`) is likely sufficient for that narrower case without introducing etcd at all — flagged here for completeness in case a distributed multi-daemon deployment later needs real distributed locks.

### 4.5 `foca` (Rust SWIM gossip)

Confirmed real and maintained, `no_std`+`alloc`, transport-agnostic. Solves membership/failure-detection only, no message/state payload semantics. Not a standalone contender — NATS KV TTL already gives equivalent-enough liveness expiry with far less new surface area.

### 4.6 Rust actor frameworks (`ractor`, `coerce`)

Both genuinely distributed-capable, but both mean inheriting an actor supervision-tree/lifecycle model for the entire affected subsystem, not just whiteboard traffic. `ractor_cluster`'s own docs flag cluster mode as "not production ready but relatively stable" — a real yellow flag for a 2026 production dependency.

### 4.7 Kafka / Pulsar / Temporal / Restate / git-backed state (Gas Town's Beads model)

- **Kafka/Pulsar**: confirmed too heavy — Kafka needs a 3-broker minimum plus ZooKeeper/KRaft overhead for lightweight presence/coordination traffic.
- **Temporal/Restate**: solve a different problem — durable *execution*, not live peer-to-peer presence/messaging. Genuinely interesting for a **separate** future concern (making a `ta run` goal's own execution durable/resumable across daemon restarts), out of scope here.
- **Git-backed coordination state (Gas Town/Beads model, §3.3)**: a legitimate, production-proven alternative pattern — commit coordination state as data in the repo itself rather than a separate service. Rejected for the whiteboard specifically because it can't natively express TTL-based liveness expiry or low-latency pub/sub-style presence without extra machinery on top of git, both of which are hard requirements here (§2.3, §2.4). Worth revisiting if a future need is closer to "durable decision log" than "live presence" — `.ta/plan_history.jsonl` and PLAN.md itself already play that role today.

---

## 5. Comparison

| | Local (zero-infra) | Distributed/hosted | Delivery guarantee for handoffs | Native presence/TTL | Agent-runtime-agnostic | Ops footprint | Rust maturity |
|---|---|---|---|---|---|---|---|
| `FsEventStore` extension | ✅ (already exists) | ❌ (shared-FS multi-writer = the bug we just fixed) | ❌ | ❌ (build it) | ✅ | none (already running) | n/a, in-tree |
| Claude Code Agent Teams / `SendMessage` | ✅ (built into the CLI) | ❌ (single machine; cross-machine routes through Anthropic) | ❌ (best-effort, capped queue) | ❌ (no presence primitive) | ❌ (Claude-Code-specific) | none (already running) | n/a, product feature not a library |
| **NATS + JetStream** | ✅ single binary | ✅ same binary, clusters | ✅ JetStream | ✅ KV TTL | ✅ | Low (10–50MB idle) | Good (`async-nats` 0.50, active) |
| Redis Streams | ❌ needs external process even locally | ✅ | ✅ (Streams; ⚠️ not Pub/Sub) | ⚠️ (build via key TTL) | ✅ | Low–medium | Good (`redis-rs`) |
| etcd/Consul | ⚠️ possible but wrong shape | ✅ | N/A (not messaging-shaped) | ⚠️ (watch-on-key, indirect) | ✅ | Medium (cluster) | OK, but wrong primitive |
| `foca` (SWIM) | ✅ | ✅ | ❌ (membership only) | ✅ (its whole job) | ✅ | Low | Real, maintained, narrow scope |
| `ractor`/`coerce` | ✅ | ⚠️ (`ractor_cluster` flagged not-production-ready) | Depends on design | Build it | ✅ | Medium (own supervision model) | Real but big commitment |
| Kafka/Pulsar | ❌ | ✅ | ✅ | Build it | ✅ | High | Wrong weight class |
| Git-backed (Gas Town/Beads) | ✅ | ✅ (via remote) | ✅ (git itself) | ❌ (no TTL concept) | ✅ | None extra | N/A, pattern not a crate |

---

## 6. Recommendation

**Build a new crate, `ta-agent-whiteboard`, as a thin schema/semantics layer on top of NATS + JetStream** — don't build a new transport, don't adopt a heavier framework's full model, and don't couple it to Claude Code's own native coordination primitives even though today's TA agents happen to run as Claude Code processes.

**Rationale**:
1. NATS is the only option researched that gives one codepath for both local-first and distributed-capable — same binary, embedded locally, clustered when hosted. Every other credible option (Redis, etcd/Consul, Claude Code's own primitives) forces either a local/hosted deployment split or a single-runtime/single-machine ceiling.
2. Its native primitives map directly onto every functional requirement in §2 without custom durability/TTL engineering: pub/sub for broadcast, JetStream KV with TTL for expiring presence, JetStream streams/consumers for durable handoff delivery.
3. It's agent-runtime-agnostic by construction (§2.8) — Claude Code's own Agent Teams/`SendMessage` explicitly is not, and would exclude `ta-agent-ollama` and any future non-Claude-Code agent runtime.
4. **Anthropic's own shipped Agent Teams feature independently validates the shape of the schema**, not the transport: a shared, dependency-aware task list with file-locked claiming, plus per-agent mailboxes, is essentially the same design already sketched in §7 below — this is corroborating evidence the schema is right, arrived at independently, not a reason to depend on Claude Code's implementation of it.
5. Operational cost is low enough to run as a single additional Render Background Worker alongside the existing daemon (10–50MB idle) — confirm against Render's current service catalog before committing (§4.2 open item).

**What NOT to build**: a new distributed lock service (use file-based locks, e.g. extending `ApplyLock`'s pattern, for the narrower v0.17.10.2 Item 3 need — see §4.4), a new durable-execution engine (Temporal/Restate territory), a full actor framework migration, or a wrapper around Claude Code's own `SendMessage`/Agent Teams (§3.5 — wrong trust boundary, not durable, not agent-agnostic, no Windows support).

**Naming**: avoid "team"/"teammate" as the primary vocabulary for `ta-agent-whiteboard` concepts — Claude Code's own Agent Teams already uses that terminology for a different, CLI-native feature, and TA's docs/UX should not create ambiguity between "a TA virtual team" (`.ta/team.toml`, `TeamSession`) and "a Claude Code Agent Team" (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`). Borrow "Agent Card"-style vocabulary from Google's A2A protocol for the activity-advertisement schema instead, without adopting A2A's wire protocol (NATS is the transport).

---

## 7. Sketch: what `ta-agent-whiteboard` actually contains

Not a full spec — enough to scope a PLAN.md phase. Now cross-validated against Claude Code's own Agent Teams design (§3.2) as an independent, converging reference implementation:

- **Presence records** (JetStream KV, TTL'd): `agent_id`, `goal_run_id`, `source_dir`, current phase/stage, resources being touched (file globs / `api_impact` tokens — reuse `task-graph`'s existing vocabulary rather than inventing a second one), last-heartbeat timestamp. TTL expiry means a crashed agent's presence disappears automatically — no manual cleanup, unlike the orphaned-goal-record pattern already known in TA (`ta draft close` not transitioning parent goal state).
- **Discovery/query API**: "who else is active," "is anyone touching `<path>`/`<api_impact tag>` right now" — a thin wrapper over KV `get`/`watch`, callable from both the daemon (for pre-launch conflict checks, directly relevant to fixing v0.17.10.2's root cause class more generally) and from within a running agent (for opportunistic self-coordination).
- **A shared, dependency-aware task/claim list**, matching the validated Agent Teams pattern: pending/in-progress/completed states, dependency blocking, self-claim, and file-locked (or JetStream-KV-CAS-based) claiming to prevent race conditions — the same mechanism Agent Teams uses for task claiming, generalized past a single-team/single-machine scope.
- **Handoff messages** (JetStream stream): sender, recipient (agent ID or role, extending `RoleRef` from `crates/ta-session/src/agent_action.rs`), payload, durable until acknowledged — a broadcastable, peer-to-peer, durable generalization of both the existing single-recipient `AgentAction::Escalate` and Claude Code's own non-durable `SendMessage`.
- **Integration points**: `ta_goal_start`/`launch_goal_agent` (`crates/ta-mcp-gateway/src/tools/goal.rs`) could consult the whiteboard before launch as a *live* complement to `task-graph`'s *static* wave planning — directly relevant to preventing another v0.17.10.2-class incident where overlap wasn't declared in advance. `team_session.rs`'s currently-sequential stage loop is a natural first internal consumer if/when it moves toward concurrent stage execution.
- **Explicitly out of scope for v1**: file-level conflict *prevention* below the `api_impact`-tag granularity (e.g. AST/symbol-level prediction) — confirmed in research (§3) as still an open research area industry-wide, not something any surveyed production system does today. `api_impact` tags remain the right granularity to start at.

---

## 8. Open items before implementation

1. Confirm NATS deployment shape on Render specifically (Background Worker + private networking reachability from the daemon service) — not verified in this research pass.
2. Decide whether `ta-agent-whiteboard` ships inside this repo initially or is extracted as a standalone OSS crate immediately (matching the `task-graph`/`consensus-panel` precedent) — recommend building in-tree first against real usage, extract once the schema stabilizes.
3. Scope a PLAN.md phase (proposed: part of the v0.17.11 virtual-team work, or its own v0.17.11.x sub-phase) once this design is approved.
4. Decide on final naming to avoid collision with Claude Code's own "Agent Teams"/"teammate" vocabulary (§6) — e.g. keep "virtual team" (`.ta/team.toml`) as TA's user-facing term, and treat `ta-agent-whiteboard` as purely the internal coordination substrate name, not something surfaced to end users as a "team" concept of its own.
5. Consider (low priority, convenience only, not a substitute for the whiteboard) whether `ta run` should avoid suppressing an interactive Claude Code session's own `CLAUDE_CODE_MESSAGING_SOCKET`, so a human supervising several interactive `ta run` sessions in parallel gets Claude Code's native `/list-agents` as a free, incidental peek — explicitly a nice-to-have layered on top, not a replacement for durable, agent-agnostic coordination.
