# Agent Messaging Protocol (AMP)

**Status**: Draft — v0.1
**Owner**: Trusted Autonomy
**License**: Apache 2.0 (OSS standard)

---

## Overview

Agent Messaging Protocol (AMP) is a structured, embedding-native communication protocol for direct agent-to-agent interaction. It replaces natural-language intermediation between agents with typed message envelopes carrying semantic vector embeddings, structured payloads, and cryptographic audit trails.

**Core goals:**

| Goal | How AMP achieves it |
|---|---|
| Reduce token usage | Embeddings encode semantic content in 768–1536 floats; no prose round-trips |
| Increase clarity | Typed payloads eliminate ambiguity; schema-validated at send and receive |
| Enable full auditability | Every message logged with sender, receiver, embedding, timestamp, hash |
| Agent-agnostic | Works across Claude, Codex, local models, BMAD roles, custom agents |
| OSS composability | Protocol spec is standalone; TA ships the reference broker as a plugin |

---

## Problem Statement

Today, multi-agent workflows communicate through natural language:

```
Agent A writes: "Please implement the authentication module following the
                architecture we discussed, ensuring it uses JWT tokens and
                integrates with the existing user service..."

Agent B reads, tokenizes, embeds, plans, then writes back:
                "I have implemented the authentication module. It uses JWT
                tokens and integrates with the UserService via..."
```

This is expensive, lossy, and unauditable:
- The same semantic intent is re-encoded in prose at every hop
- Context that was already embedded gets serialized to text and re-embedded by the receiver
- There is no machine-readable audit record of what was actually communicated
- Latency compounds: each hop requires a full generation pass to restate prior context

AMP eliminates the prose layer between agents while preserving full human-readability for audit and debugging.

---

## Design Principles

1. **Embeddings are the primary semantic channel** — intent, context, and results travel as vectors. Prose summaries are optional metadata for human readers, not the authoritative content.

2. **Typed payloads for structured data** — parameters, file paths, goal IDs, draft IDs, CI status, approval decisions, and similar structured values are typed fields, never embedded in prose.

3. **Context hashing eliminates re-transmission** — agents share a context hash registry. If the receiver already has the context (same hash), the sender omits the embedding body and sends only the hash reference.

4. **Every message is an audit event** — the AMP broker logs each message to the TA audit trail with full fidelity. Compliance queries are embedding searches, not log grep.

5. **Degrade gracefully to natural language** — if a receiver doesn't support AMP, the broker serializes the embedding + payload to a prose summary and delivers it as a normal channel message. No message is ever dropped.

---

## Message Format

### Envelope

```json
{
  "amp_version": "1.0",
  "id": "amp-<uuid>",
  "from": "<agent-id>",
  "to": "<agent-id | broadcast>",
  "timestamp": "2026-03-21T15:00:00Z",
  "ttl": 3,

  "intent_embedding": [0.021, -0.134, ...],
  "intent_model": "text-embedding-3-small",
  "intent_dims": 1536,

  "context_hash": "sha256:<hash>",
  "context_embedding": null,

  "payload_type": "command | query | response | event | ack",
  "payload": { ... },

  "signature": "<hmac-sha256-hex | null>"
}
```

**Fields:**

| Field | Required | Description |
|---|---|---|
| `amp_version` | yes | Protocol version |
| `id` | yes | Unique message ID (`amp-` prefix + UUID) |
| `from` | yes | Sending agent ID (registered in AMP broker) |
| `to` | yes | Receiving agent ID or `"broadcast"` |
| `timestamp` | yes | ISO 8601 send time |
| `ttl` | yes | Hop count; broker decrements and drops at 0 |
| `intent_embedding` | yes | Vector encoding of message intent |
| `intent_model` | yes | Embedding model used |
| `context_hash` | yes | SHA-256 of shared context; null if no prior context |
| `context_embedding` | no | Full context vector if receiver may not have it |
| `payload_type` | yes | One of: `command`, `query`, `response`, `event`, `ack` |
| `payload` | yes | Typed struct (see below) |
| `signature` | no | HMAC-SHA256 of canonical message bytes; nil in dev mode |

### Payload Types

#### `command`
Direct instruction from one agent to another.

```json
{
  "action": "implement_feature",
  "parameters": {
    "goal_id": "dc7fe852-...",
    "phase": "v0.13.2",
    "scope_embedding": [0.041, ...],
    "constraints": ["no_new_deps", "must_pass_verify"],
    "priority": "high"
  },
  "approval_required": false,
  "timeout_secs": 3600
}
```

#### `query`
Request for information; expects a `response`.

```json
{
  "question_embedding": [0.089, ...],
  "scope": "codebase | goal | draft | plan",
  "entity_id": "dc7fe852-...",
  "expected_type": "embedding | structured | bool",
  "deadline_secs": 30
}
```

#### `response`
Reply to a prior `query` or `command`.

```json
{
  "to_message_id": "amp-<uuid>",
  "status": "ok | partial | error | deferred",
  "result_embedding": [0.032, ...],
  "structured_result": { ... },
  "error_code": null,
  "error_message": null
}
```

#### `event`
Broadcast notification of state change. No reply expected.

```json
{
  "event_type": "draft_ready | goal_completed | ci_passed | approval_needed | file_changed",
  "entity_id": "<draft-id | goal-id | pr-id>",
  "entity_type": "draft | goal | pr | file",
  "delta_embedding": [0.011, ...],
  "metadata": { "draft_id": "...", "file_count": 12 }
}
```

#### `ack`
Lightweight acknowledgement. No embedding required.

```json
{
  "to_message_id": "amp-<uuid>",
  "status": "received | processing | rejected",
  "reason": null
}
```

---

## Context Hash Registry

The context hash eliminates the most expensive pattern in multi-agent work: re-transmitting the same context at every message boundary.

**How it works:**

1. When an agent first encounters a context (codebase snapshot, prior conversation, goal state), the AMP broker embeds it and registers `sha256(canonical_bytes) → embedding`.

2. Subsequent messages reference the context by hash only. The broker resolves the hash to the stored embedding for the receiver.

3. If the receiver is on a different machine or session, the broker includes the full `context_embedding` in the envelope (cache miss path).

**Estimated savings:** In a 10-message goal run where each agent would otherwise re-embed 4,000 tokens of prior context, context hashing reduces total embedding tokens by ~60–80%.

---

## Integration with Trusted Autonomy

### Architecture

```
┌─────────────────────────────────────────────────────┐
│                   TA Daemon                          │
│                                                      │
│  ┌──────────┐    ┌──────────────┐    ┌───────────┐  │
│  │ Agent A  │───▶│  AMP Broker  │───▶│  Agent B  │  │
│  │(claude)  │    │  (plugin)    │    │ (codex /  │  │
│  └──────────┘    │              │    │  bmad-dev)│  │
│                  │ • route msgs │    └───────────┘  │
│                  │ • ctx cache  │                    │
│                  │ • audit log  │                    │
│                  │ • fallback   │                    │
│                  └──────┬───────┘                    │
│                         │                            │
│                  ┌──────▼───────┐                    │
│                  │  AMP Audit   │                    │
│                  │  Trail       │                    │
│                  │ (.ta/amp/    │                    │
│                  │  messages/)  │                    │
│                  └──────────────┘                    │
└─────────────────────────────────────────────────────┘
```

### Plugin Installation

AMP ships as a TA plugin — declare it in `.ta/project.toml`:

```toml
[plugins.amp]
type    = "broker"
version = ">=0.1.0"
source  = "registry:ta-amp-broker"
```

Or build from source:
```bash
git clone https://github.com/amp-protocol/amp-broker
cd amp-broker && cargo build --release
mkdir -p .ta/plugins/brokers/amp
cp target/release/amp-broker .ta/plugins/brokers/amp/
```

### Configuration

```toml
# .ta/daemon.toml
[amp]
enabled        = true
broker_url     = "unix://.ta/amp.sock"   # or tcp://127.0.0.1:7701
embedding_model = "text-embedding-3-small"
context_cache_mb = 256                   # in-process context hash registry
audit_path     = ".ta/amp/messages"      # append-only JSONL audit log
fallback_to_nl = true                    # deliver as prose if receiver is non-AMP

[amp.auth]
mode = "hmac"                # "none" (dev), "hmac" (local), "mtls" (distributed)
secret_env = "TA_AMP_SECRET" # for hmac mode
```

### Agent Registration

Agents register with the AMP broker on startup. Registration declares:
- Agent ID and capabilities
- Supported payload types
- Embedding model and dimensionality
- Whether the agent can send and/or receive AMP messages

```json
{
  "agent_id": "bmad-architect-01",
  "capabilities": ["design", "architecture", "review"],
  "amp_receive": true,
  "amp_send": true,
  "embedding_model": "text-embedding-3-small",
  "dims": 1536,
  "fallback_nl": false
}
```

### Message Flow in a TA Goal

In a standard TA macro-goal with multiple agents:

```
ta run "v0.13.2" --macro --agents bmad-architect,bmad-dev,bmad-qa

1. Orchestrator → bmad-architect  [AMP command: design_phase]
   intent_embedding: [design intent vector]
   payload: { scope: "MCP transport", constraints: [...] }

2. bmad-architect → Orchestrator  [AMP response]
   result_embedding: [architecture decision vector]
   structured_result: { files: ["docs/architecture.md"], decisions: [...] }

3. Orchestrator → bmad-dev  [AMP command: implement]
   context_hash: <hash of architecture decision>  ← no re-transmission
   payload: { phase: "v0.13.2", scope_embedding: [...] }

4. bmad-dev → bmad-qa  [AMP event: draft_ready]
   entity_id: "draft-abc123"
   delta_embedding: [diff content vector]

5. bmad-qa → Orchestrator  [AMP response: qa_complete]
   status: "ok"
   structured_result: { tests_passed: 47, coverage: 0.91 }
```

Total tokens saved vs. natural language relay: the architecture document (step 2) is never re-transmitted as prose to bmad-dev (step 3) — only the context hash travels.

### Audit Trail

Every AMP message is appended to `.ta/amp/messages/YYYY-MM-DD.jsonl`:

```json
{"timestamp":"2026-03-21T15:01:02Z","id":"amp-abc123","from":"bmad-architect-01","to":"bmad-dev-01","payload_type":"command","intent_preview":"implement MCP transport layer","context_hash":"sha256:deadbeef...","token_estimate":0,"prose_equivalent_estimate":840}
```

The `prose_equivalent_estimate` field records how many tokens the same message would have consumed if sent as natural language, enabling direct measurement of protocol savings.

Query the audit trail:

```bash
ta amp log                                   # recent messages
ta amp log --goal dc7fe852                   # messages for a specific goal
ta amp stats                                 # aggregate token savings
ta amp stats --since 7d                      # last 7 days
```

---

## Measuring Utility and Savings

### Metrics Tracked

| Metric | How measured |
|---|---|
| **Token savings** | `prose_equivalent_estimate - actual_tokens_sent` per message |
| **Latency reduction** | Round-trip time: AMP message vs. equivalent NL exchange |
| **Context re-transmission rate** | `context_hash_hits / total_messages` |
| **Fallback rate** | `fallback_nl_count / total_messages` (lower = more AMP-native agents) |
| **Message clarity score** | Cosine similarity between `intent_embedding` and `result_embedding` (measures whether the response addressed the intent) |

### Reporting

```bash
# Session summary
ta amp stats
# AMP Stats (last 30 days)
# ─────────────────────────────────────────────
# Messages sent:          1,247
# Context hash hits:        891  (71.5%)
# Fallback to NL:            43  (3.4%)
#
# Token savings
#   Estimated NL tokens:  892,400
#   Actual AMP tokens:    124,300
#   Net saved:            768,100  (86.1%)
#   Est. cost saved:        $2.30  (at $3/M tokens)
#
# Avg message latency:      42ms  (vs ~1,800ms NL relay)
# Avg clarity score:        0.94

# Per-goal breakdown
ta amp stats --goal dc7fe852 --verbose
```

### Embedding in TA Reports

`ta plan status` and `ta status` will surface a one-line AMP efficiency indicator when the broker is active:

```
AMP: 1,247 msgs · 86% token savings · 0.94 clarity
```

---

## OSS Model

### Repository Structure

AMP is designed as a standalone open standard with TA as the reference implementation.

```
github.com/amp-protocol/
├── amp-spec/            # Protocol specification (this document, versioned)
│   ├── spec/v1.0.md
│   ├── schemas/         # JSON Schema for all message types
│   └── CHANGELOG.md
│
├── amp-broker/          # Reference broker (Rust)
│   ├── src/
│   │   ├── broker.rs    # Routing, TTL, audit
│   │   ├── context.rs   # Hash registry + embedding cache
│   │   ├── fallback.rs  # NL serialization for non-AMP receivers
│   │   └── metrics.rs   # Token savings tracking
│   └── Cargo.toml
│
├── amp-sdk-rust/        # Rust client library
├── amp-sdk-python/      # Python client (for BMAD, LangChain, etc.)
├── amp-sdk-typescript/  # TypeScript client (for Claude Flow, web agents)
└── amp-conformance/     # Test suite for protocol compliance
```

### Governance

- **Spec versioning**: Semantic versioning. Breaking changes require a major version bump and a 90-day deprecation window.
- **Extension points**: Payload types are extensible. New `payload_type` values are registered via a lightweight RFC process in `amp-spec`.
- **Embedding model agnosticism**: The spec mandates that `intent_model` and `dims` are declared per-message. Brokers must support routing between agents using different embedding models (with a cross-model similarity bridge for context hash resolution).
- **Security tiers**: `none` (local dev), `hmac` (single-machine or trusted LAN), `mtls` (distributed / cloud).

### Integration Points Beyond TA

AMP is designed to be adopted by any multi-agent framework:

| Framework | Integration path |
|---|---|
| **Claude Flow** | `amp-sdk-typescript` — swarm agents register with AMP broker, coordinate via events |
| **BMAD** | `amp-sdk-python` — PM/architect/dev/QA roles send typed handoffs |
| **LangChain / LangGraph** | `amp-sdk-python` — graph edges become AMP messages with full audit |
| **AutoGen** | `amp-sdk-python` — agent conversations mediated by AMP broker |
| **Custom** | HTTP/WebSocket API on the broker — any language, no SDK required |

### Versioning and TA Phase

AMP broker development is tracked in PLAN.md:

| Phase | Content |
|---|---|
| v0.13.2 (current) | MCP transport abstraction (foundation for broker socket layer) |
| v0.14.x | AMP broker alpha — local broker, context hash registry, audit trail |
| v0.15.x | AMP SDK releases — Rust, Python, TypeScript |
| v0.16.x | Cross-model context bridge, distributed broker (mTLS), conformance suite |

---

## Example: Goal Handoff Without Natural Language

**Before AMP** (natural language relay, ~1,200 tokens per hop):
```
Orchestrator → Agent:
"You are implementing phase v0.13.2 of the Trusted Autonomy project. The
architecture document at docs/architecture.md specifies that the MCP transport
layer should support both TCP sockets and Unix domain sockets. The current
implementation only supports stdio. You need to add a TransportAdapter trait..."
[800 tokens of context re-stated from prior messages]
```

**With AMP** (~40 tokens equivalent):
```json
{
  "payload_type": "command",
  "intent_embedding": [...],          // "implement transport abstraction"
  "context_hash": "sha256:abc123",    // receiver already has the architecture doc
  "payload": {
    "action": "implement",
    "phase": "v0.13.2",
    "scope": ["crates/ta-mcp/src/transport.rs"],
    "constraints": ["trait_based", "backward_compatible"],
    "deadline_secs": 3600
  }
}
```

The receiver retrieves the architecture document from its local context cache using the hash. No re-transmission. No re-tokenization.

---

## FAQ

**Q: Does AMP require all agents to support it?**
No. The broker's `fallback_nl = true` default serializes AMP messages to a prose summary for non-AMP receivers. Adoption is incremental.

**Q: What embedding model should I use?**
The broker is model-agnostic. `text-embedding-3-small` (1536 dims) is the default for cloud deployments. For local/offline use, `nomic-embed-text` via Ollama works well. The broker handles cross-model routing via a similarity bridge.

**Q: How is this different from function calling / tool use?**
Tool use is for agent-to-tool communication (structured I/O). AMP is for agent-to-agent communication where the semantic content itself needs to travel efficiently. AMP messages can carry tool-call results as structured payload fields.

**Q: What prevents a malicious agent from impersonating another?**
In `hmac` mode, every message is signed with a shared secret. In `mtls` mode, each agent has a certificate. In `none` mode (local dev only), no authentication is enforced — acceptable when all agents run in the same process.

**Q: Can I query the AMP audit trail for compliance reporting?**
Yes. The JSONL audit log is queryable with `ta amp log` and exportable for external SIEM systems. Each entry includes the intent embedding — you can cluster messages by semantic content to identify communication patterns and anomalies.
