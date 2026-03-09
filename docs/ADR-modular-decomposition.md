# ADR: Modular Decomposition — TA Core vs. Agent Infrastructure vs. Applications

> **Status**: Deferred — the monorepo approach remains for now. Memory and credential extraction are candidates for post-v1.0.
> **Author**: Claude (v0.5.6 session)
> **Context**: Discussion during v0.5.6 implementation about whether TA is trying to do too many things. As of v0.10.7, all crates remain in the workspace. The channel plugin system (v0.10.2+) validates the out-of-process plugin pattern for extensions, but core crate extraction has not been prioritized.

---

## The Question

TA's plan through v1.0 conflates three distinct concerns into a single project:

1. **TA Core** — the mediation/governance layer (the actual thesis)
2. **Agent Infrastructure** — general-purpose tools any agent system needs
3. **Applications** — products built on top of TA

Should these be separated into independent, composable layers?

---

## Current State

Everything lives in one workspace under `crates/`. As of v0.5.6:

```
crates/
  ta-audit          # Core: append-only hash-chained audit log
  ta-changeset      # Core: draft/PR package model, review channels
  ta-credentials    # Infra: credential broker, identity abstraction
  ta-daemon         # Core: MCP server binary
  ta-goal           # Core: goal lifecycle state machine, events
  ta-mcp-gateway    # Core: MCP tool handlers, policy enforcement
  ta-memory         # Infra: persistent memory store + auto-capture
  ta-policy         # Core: default-deny capability engine
  ta-sandbox        # Infra: sandbox runner (placeholder)
  ta-submit         # Core: submit adapter trait
  ta-workspace      # Core: staging workspace, overlay, change store
  ta-connectors/    # Core: filesystem, web, mock connectors
```

The problem: `ta-memory` and `ta-credentials` have almost no dependencies on TA core types. They're general-purpose agent infrastructure that happens to live in TA's workspace.

---

## Detailed Analysis: What's Core vs. What's Not

### TA Core (the thesis: "all agent actions are mediated")

These crates directly implement TA's unique value proposition — staging isolation, policy enforcement, human-in-the-loop review, and audit trail:

| Crate | Role | Dependencies on other TA crates |
|-------|------|---------------------------------|
| `ta-policy` | Default-deny capability engine | None |
| `ta-audit` | Hash-chained append-only audit log | None |
| `ta-workspace` | Staging workspace, overlay, change store | None |
| `ta-changeset` | Draft/PR packages, review channels, interactions | None |
| `ta-goal` | Goal lifecycle state machine, event dispatch | None |
| `ta-submit` | Submit adapter trait (git, etc.) | None |
| `ta-mcp-gateway` | MCP tool handlers wiring everything together | All of the above |
| `ta-daemon` | Binary that runs the MCP server | `ta-mcp-gateway` |
| `ta-connectors/*` | Filesystem/web/mock resource connectors | `ta-changeset`, `ta-workspace` |

**These should stay in TA.** They *are* TA.

### Agent Infrastructure (general-purpose, not TA-specific)

These solve problems that every agent system faces, regardless of whether TA's governance layer is involved:

#### `ta-memory` — Does it add value over RuVector?

**What ta-memory provides today:**
- `MemoryStore` trait abstracting storage backends
- `FsMemoryStore` — zero-dependency JSON file backend
- `RuVectorStore` — HNSW-indexed semantic search backend (wraps ruvector-core)
- `MemoryEntry` — structured entries with key, value, tags, source, goal_id, category
- `AutoCapture` — lifecycle event → memory entry conversion
- `build_memory_context_section()` — formats entries for agent prompt injection
- Config parsing from `.ta/workflow.toml`

**What ruvector-core provides:**
- `VectorDB` — HNSW-indexed vector database
- `VectorEntry` — id + vector + metadata (HashMap<String, Value>)
- `SearchQuery` — k-NN similarity search
- Distance metrics (cosine, euclidean, dot product)
- Persistence to `.rvf` files

**The gap ta-memory fills over raw ruvector:**

| Capability | ruvector-core | ta-memory |
|-----------|--------------|-----------|
| Vector storage + search | Yes | Yes (delegates to ruvector) |
| Structured entry model (key, tags, source, category) | No (raw metadata HashMap) | Yes |
| Exact-key recall | No (vector search only) | Yes |
| Tag-based filtering | No | Yes |
| Zero-dependency fallback (JSON files) | No | Yes (FsMemoryStore) |
| Lifecycle auto-capture (goal complete, draft reject) | No | Yes |
| Agent prompt injection formatting | No | Yes |
| Config-driven capture rules | No | Yes |
| Framework-agnostic source tracking | No | Yes |

**Verdict:** ta-memory adds meaningful value over raw ruvector. ruvector is a vector database; ta-memory is an **agent knowledge store** that can use ruvector as a backend. The structured entry model, tag-based queries, auto-capture, and prompt injection are agent-specific concerns that don't belong in a vector DB library.

**However**, ta-memory doesn't need to live inside TA. It could be:
- A standalone crate (`agent-memory` or `ruvector-memory`) that TA depends on
- Published independently for use by Claude Code plugins, Codex extensions, etc.
- The auto-capture module could stay in TA (it references TA concepts like goals/drafts) while the core store is extracted

**Recommended split:**
```
agent-memory/           # Standalone crate (publishable)
  src/
    store.rs            # MemoryStore trait, MemoryEntry, MemoryQuery
    fs_store.rs         # FsMemoryStore (zero-dep JSON backend)
    ruvector_store.rs   # RuVectorStore (semantic search backend)
    error.rs

ta-memory/              # TA-specific layer (stays in TA workspace)
  src/
    auto_capture.rs     # GoalCompleteEvent, DraftRejectEvent, etc.
    context_inject.rs   # build_memory_context_section()
    config.rs           # AutoCaptureConfig from workflow.toml
  [dependencies]
    agent-memory = "..."
```

#### `ta-credentials` — Credential Broker

**What it does:** Manages secrets, OAuth flows, and identity abstraction so agents never hold raw credentials.

**Is it TA-specific?** No. Every agent system that accesses external APIs needs credential management. Claude Code has its own, Codex has its own, etc.

**Should it be extracted?** Yes, but less urgently than memory. Credential management is deeply tied to MCP server patterns — it could become a standalone MCP server that TA (and others) connect to:

```
agent-credentials/      # Standalone MCP server
  # Manages secrets, OAuth, identity abstraction
  # Any MCP client can connect, not just TA

ta integrates via:
  # .ta/workflow.toml
  [credentials]
  provider = "agent-credentials"  # or "ta-builtin" for backwards compat
```

#### Cost Tracking (v0.6.1, not yet implemented)

**Is it TA-specific?** No. Token budget management is a general agent ops concern.

**Recommendation:** Implement as a standalone crate from the start. TA's event system can feed it data, but the tracking/budgeting logic shouldn't couple to TA's goal model.

```
agent-budget/           # Standalone crate
  # Token counting, cost estimation, budget enforcement
  # Pluggable provider model (Anthropic, OpenAI, etc.)

ta-budget/              # TA integration layer
  # Hooks into goal lifecycle events
  # Budget policy enforcement via ta-policy
```

#### Sandbox Runner (v0.9.2, not yet implemented)

**Is it TA-specific?** No. Agent sandboxing (OCI/gVisor, network policy, CWD enforcement) is infrastructure.

**Recommendation:** Standalone from the start. TA configures and invokes it, but the sandbox itself is reusable.

### Applications (built on TA, not part of TA)

These are products/use-cases that compose TA with domain-specific logic:

| Feature | Plan Phase | Why it's an application, not TA |
|---------|-----------|-------------------------------|
| Domain workflow templates | v0.7.1 | Email, finance, social media are use cases |
| Virtual Office Runtime | v1.0 | Orchestration product that depends on TA |
| Community Memory | v0.8.1 | Distributed knowledge sharing is its own product |
| Guided Setup | v0.7.0 | Onboarding UX, could be a separate CLI tool |

**Recommendation:** These become separate repos:

```
ta-office/              # Virtual office runtime
  # Role definitions, trigger system, orchestration
  # Depends on: ta (core), agent-memory, agent-credentials

ta-templates/           # Domain workflow templates
  # sw-engineer, email-assistant, home-finance, etc.
  # Each template is a directory of YAML configs
  # Depends on: ta (core) for the config format

ta-community/           # Community memory registry
  # Opt-in knowledge sharing across TA instances
  # Depends on: agent-memory, ruvector (for sync)
```

---

## Proposed Architecture

```
Layer 3: Applications (separate repos)
┌─────────────┐ ┌──────────────┐ ┌───────────────┐
│  ta-office   │ │ ta-templates │ │ ta-community  │
│ (v1.0)       │ │ (v0.7.1)     │ │ (v0.8.1)      │
└──────┬───────┘ └──────┬───────┘ └───────┬───────┘
       │                │                  │
Layer 2: TA Core (this repo)
┌──────┴────────────────┴──────────────────┴───────┐
│  ta-core                                          │
│  ├── ta-policy       (capability engine)          │
│  ├── ta-audit        (hash-chained log)           │
│  ├── ta-workspace    (staging isolation)           │
│  ├── ta-changeset    (draft/review model)          │
│  ├── ta-goal         (lifecycle state machine)     │
│  ├── ta-submit       (submit adapters)             │
│  ├── ta-mcp-gateway  (MCP server, tool handlers)   │
│  ├── ta-connectors/* (resource connectors)         │
│  └── ta-cli          (CLI binary)                  │
│                                                    │
│  TA-specific integration layers:                   │
│  ├── ta-memory       (auto-capture, context inject)│
│  ├── ta-budget       (budget policy enforcement)   │
│  └── ta-sandbox-cfg  (sandbox config + policy)     │
└──────┬────────────────┬──────────────────┬────────┘
       │                │                  │
Layer 1: Agent Infrastructure (standalone crates, publishable)
┌──────┴───────┐ ┌──────┴──────┐ ┌────────┴───────┐
│ agent-memory │ │ agent-creds │ │ agent-sandbox  │
│              │ │             │ │                │
│ MemoryStore  │ │ SecretStore │ │ SandboxRunner  │
│ FsBackend    │ │ OAuth flows │ │ OCI/gVisor     │
│ RuVector     │ │ Identity    │ │ Network policy │
└──────┬───────┘ └─────────────┘ └────────────────┘
       │
┌──────┴───────┐
│ ruvector-core│  (already a separate crate)
│ HNSW vectors │
└──────────────┘
```

---

## Migration Path

This doesn't need to happen all at once. A phased approach:

### Phase A: After v0.5.7 (memory features complete)
1. Extract `agent-memory` from `ta-memory` (store trait + backends)
2. `ta-memory` becomes a thin layer: auto-capture + context injection + config
3. Publish `agent-memory` as a standalone crate
4. No breaking changes to TA — `ta-memory` re-exports everything

### Phase B: Before v0.6 (supervisor + cost tracking)
1. Implement cost tracking as `agent-budget` from the start
2. `ta-budget` is the TA integration layer
3. Evaluate extracting `ta-credentials` → `agent-credentials`

### Phase C: Before v0.7 (templates + setup)
1. Create `ta-templates` as a separate repo
2. `ta setup` references templates from the external repo
3. Domain-specific MCP server configs live in template packages

### Phase D: Before v1.0 (virtual office)
1. Create `ta-office` as a separate repo
2. Virtual office runtime composes TA core + agent infrastructure
3. TA core stays focused on mediation/governance

---

## Benefits of This Split

1. **TA's thesis stays clear**: "All agent actions are mediated." No dilution from memory stores, finance templates, or office orchestration.

2. **Agent infrastructure gets wider adoption**: `agent-memory` could be used by Claude Code plugins, Codex extensions, LangChain, etc. — not locked to TA users.

3. **Applications can evolve independently**: The virtual office doesn't need to wait for TA core releases. Templates can be community-contributed.

4. **Testing and CI scale better**: TA core tests don't need to build ruvector, Plaid integrations, or OCI runtime dependencies.

5. **Onboarding is simpler**: New contributors see a focused core, not a monolith that does everything from hash-chained audit logs to family office portfolio reporting.

---

## Risks

1. **Coordination overhead**: Multiple repos mean more release coordination. Mitigated by workspace-level version pinning and a CI matrix.

2. **Feature drift**: Extracted crates might evolve in ways TA doesn't expect. Mitigated by TA owning the integration layers and pinning versions.

3. **Premature extraction**: Extracting too early means the interfaces aren't stable yet. The migration path above defers extraction until each feature area is proven (post-v0.5.7 for memory, post-v0.6 for budget/credentials).

---

## Decision Needed

**Post-v0.10.7 status** (reviewed during documentation consolidation):
- [x] Review this ADR — reviewed; extraction deferred. Monorepo is manageable at current scale.
- [ ] Decide whether to extract `agent-memory` as Phase A — deferred. ta-memory works well in-workspace.
- [x] Decide whether v0.7.1 domain templates should be a separate repo — kept in-repo under `templates/`. External sources supported via `ta workflow add --from`.
- [x] Decide whether v1.0 virtual office should be planned as a separate project — yes, listed as a separate project in PLAN.md ("Projects On Top").

---

## Appendix: ta-memory vs. ruvector Decision Matrix

| If you need... | Use ruvector-core directly | Use agent-memory | Use ta-memory (TA integration) |
|---------------|---------------------------|-----------------|-------------------------------|
| Raw vector storage + k-NN search | Yes | Overkill | Overkill |
| Structured agent knowledge with tags, categories | No | Yes | Yes |
| Zero-dependency JSON file fallback | No | Yes | Yes |
| Semantic search + exact-key recall | Partial (search only) | Yes (both) | Yes (both) |
| Auto-capture from TA goal/draft events | No | No | Yes |
| CLAUDE.md context injection | No | No | Yes |
| Cross-framework source tracking | No | Yes (source field) | Yes |
| Use outside of TA | Yes | Yes | No (TA-specific) |

The three layers serve different audiences:
- **ruvector-core**: Anyone who needs a Rust vector database
- **agent-memory** (proposed): Anyone building AI agents who need persistent knowledge
- **ta-memory**: TA users who want lifecycle-driven automatic knowledge capture
