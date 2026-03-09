# Trusted Autonomy — Mission & Scope

> **Status**: Living document
> **Complements**: ADR-product-concept-model.md (five-layer architecture), WHY-TA-vs-VM.md (containment model comparison), user-experience.md (persona walkthroughs)

---

## Mission

**TA exists so that humans can delegate work to AI agents and trust the outcome.**

Agents propose. Humans dispose. TA is the governance layer that makes this safe, observable, and auditable — for any agent framework, any resource type, any delivery channel.

---

## What TA Is

TA is a **governance substrate for autonomous AI work.** It mediates every state-changing action an agent proposes, holds it for human review, and provides a complete audit trail of what happened and why.

TA's value is the shift from **synchronous conversation** (human drives, agent assists) to **asynchronous delegation** (human sets goals, agent works independently, human reviews at decision points).

## What TA Is Not

- **Not an agent framework.** TA does not provide LLM inference, prompt engineering, or agent logic. It wraps any agent framework (Claude Code, Codex, Ollama, LangChain, LangGraph, or custom) and governs its output.
- **Not a build system.** TA does not compile code, run test suites, or manage dependencies. It triggers build tools and captures results as governed events.
- **Not a CI/CD platform.** TA does not replace GitHub Actions, Jenkins, or ArgoCD. It can trigger releases and observe results, but the execution is delegated to purpose-built tools.
- **Not a version control system.** TA does not replace Git, SVN, or Perforce. It uses VCS adapters to stage and submit changes through whatever VCS the project uses.

---

## The Scope Test

Every feature in TA must pass this test:

> **Does this feature govern a transition in the lifecycle of agent work?**

If yes, it belongs in TA. If it's the work itself, it belongs in the agent, the build tool, or an external system.

### Lifecycle transitions TA governs

```
Goal defined
  → Agent configured and launched
    → Agent works in staging (invisible to TA)
      → Agent asks human a question (interactive mode)
    → Draft built from staging diff
      → Human reviews draft
        → Draft approved / denied / partially approved
          → Changes applied to project
            → VCS sync (feature branch → main)
              → Build verified
                → Release cut
```

Each `→` is a **governed transition** — a point where TA enforces policy, captures audit data, emits events, and optionally pauses for human input.

### The three categories

| Category | TA's role | Examples |
|----------|-----------|---------|
| **Govern** | Enforce policy, require approval, log decisions | Draft review, selective approval, policy violations, access constitutions |
| **Orchestrate** | Coordinate transitions between governed steps | Goal → agent launch → draft build → review → apply → sync → build → release |
| **Observe** | Capture results from external tools as events | Build output, test results, VCS status, release artifacts |

TA governs and orchestrates. It does **not** execute the underlying work — agents write code, build tools compile it, VCS tools version it.

---

## Feature Boundary Decisions

### `ta sync` — In Scope

**Why**: TA already governs the submit flow (stage → commit → push → open PR via `SubmitAdapter`). After a PR merges, the project's main branch is stale. `ta sync` governs the reverse transition: pull main, rebase staging if active, surface conflicts.

**Boundary**: TA calls the VCS adapter's sync method. It does not implement rebase logic, cherry-pick, or conflict resolution — those are VCS operations. TA captures the result (success, conflict, error) as an event and surfaces it to the human.

**Adapter**: Unified `SourceAdapter` trait (merges current `SubmitAdapter` + sync into one interface). The trait defines abstract operations that make sense across all VCS types. Provider-specific mechanics (rebase, fast-forward, shelving) live in each implementation — TA's core only speaks the abstract vocabulary.

```
trait SourceAdapter {
    // Abstract operations — every VCS has these concepts
    fn sync_upstream(&self) -> Result<SyncResult>;     // "make local current"
    fn submit_changes(&self, ...) -> Result<...>;      // "publish changes"
    fn open_review(&self, ...) -> Result<...>;         // "request review"
    fn save_state(&self) -> Result<...>;               // "bookmark where I am"
    fn restore_state(&self, ...) -> Result<()>;        // "go back to bookmark"
    fn exclude_patterns(&self) -> Vec<String>;         // "VCS metadata to ignore"
    fn detect(root: &Path) -> bool;                    // "is this my repo?"
}
```

Each provider implements the abstract operations using its native mechanics:

| Abstract operation | Git | SVN | Perforce |
|--------------------|-----|-----|----------|
| `sync_upstream` | `fetch` + `merge` or `rebase` (per config) | `svn update` | `p4 sync` |
| `submit_changes` | `add` + `commit` + `push` | `svn add` + `svn commit` | `p4 reconcile` + `p4 submit` |
| `open_review` | `gh pr create` | N/A | Helix Swarm (if configured) |
| `save_state` | Save current branch name | N/A | Save client/changelist |
| `restore_state` | Checkout saved branch | N/A | Log restore |

**Config**: Per-project in `.ta/workflow.toml`. Provider-specific options are namespaced — options that don't apply to a provider are silently ignored:

```toml
[source]
adapter = "git"          # or "svn", "perforce", "none"

[source.sync]
auto_sync = false        # sync upstream after ta draft apply

[source.git]
sync_strategy = "merge"  # merge | rebase | fast-forward (git-specific)
branch_prefix = "ta/"
target_branch = "main"

[source.perforce]
client_name = "my-workspace"  # perforce-specific
```

### `ta build` — In Scope (as governed event wrapper)

**Why**: After code lands, the human needs to know if it builds. TA doesn't compile code — but it needs to know the build result to decide whether to proceed (to release, to next phase, or to surface an error). Without `ta build`, there's a gap: TA applies a draft, then goes silent. The human manually runs the build, mentally tracks the result, and manually continues.

**Boundary**: TA calls a configured build command, captures stdout/stderr, emits `build_completed` or `build_failed` events, and logs the result. It does not parse build output, manage dependencies, or fix build errors.

**Adapter**: `BuildAdapter` trait. Implementations are thin wrappers around project-specific build tools:

| Adapter | Build command |
|---------|--------------|
| Cargo (auto-detected) | `cargo build --workspace` |
| npm (auto-detected) | `npm run build` |
| Script | User-defined command |
| Webhook | HTTP POST to external CI, poll for result |

**Config**: Per-project in `.ta/workflow.toml`:
```toml
[build]
command = "cargo build --workspace"   # or auto-detected from framework
test_command = "cargo test --workspace"
on_fail = "notify"                     # notify | block_release | block_next_phase
```

**What TA adds**: The build result flows through TA's event system. Workflows can gate on it. Channels deliver it. Audit logs record it. Without TA, the build is a side effect; with TA, it's a governed checkpoint.

**Event-driven agent routing** (post-0.10.x): Build failures (and other events) should be routable to agent workflows, not just human notification. A user could configure: "on `build_failed`, launch an agent goal to diagnose and fix the error." This is not scripted hooks — it's intelligent agent routing where the agent receives the event context and decides what to do. TA manages a default set of event responses (notify human, block next phase), but users can override any event with an agent workflow. See v0.11.0 in PLAN.md.

### `ta release` — In Scope (already exists)

**Why**: Releasing is a governed transition — version bumps, changelogs, and artifact publication affect the project's public state. TA already has `ta release run`.

**Extension**: Wire `ta sync` and `ta build` as optional pre-release steps:

```toml
[release]
pre_steps = ["sync", "build", "test"]  # run before cutting release
```

### `ta plan from` — In Scope (via interactive mode)

**Why**: Generating a development plan from a product document is a governed agent interaction — the agent reads the doc, asks clarifying questions (interactive mode), and produces a PLAN.md draft for human review. It's a specific use case of the general interactive mode primitive.

**Boundary**: TA provides the interactive mode infrastructure (`ta_ask_human`, `QuestionRegistry`, conversation history). The planning logic is in the agent's system prompt, not in TA's code.

### Workflow Engine — In Scope (with care)

**Why**: Multi-stage workflows (build → review → deploy) are sequences of governed transitions. TA's workflow engine orchestrates the sequence; it does not execute the stages — agents and adapters do.

**Boundary**: The workflow engine handles stage ordering, dependency resolution, verdict scoring, failure routing, and human-in-the-loop gates. It does not contain domain logic. Stage execution is delegated to agents (via roles) or adapters (via configured commands).

### External Channels (Slack, Discord, Email) — In Scope

**Why**: TA's governance is only useful if humans can interact with it where they already work. Channels are delivery adapters — they render TA's events and collect human responses. They don't add governance logic.

**Boundary**: Channel adapters implement `deliver_question()` and `render_event()`. Response handling flows through the standard HTTP API (`POST /api/interactions/:id/respond`). No channel-specific business logic.

---

## Out of Scope (Belongs in Projects on Top)

These are not part of TA core. They are independent projects that consume TA's extension points (see ADR-product-concept-model.md):

| Capability | Why it's out of scope | Where it belongs |
|------------|----------------------|------------------|
| Role definitions & scheduling | Domain-specific orchestration logic | Virtual Office Runtime |
| Infrastructure provisioning | Domain-specific resource management | Autonomous Infra Ops |
| LLM prompt engineering | Agent framework concern | Agent configs / system prompts |
| Code generation | Agent capability, not governance | The agent itself |
| Dependency management | Build tool concern | npm, cargo, pip, etc. |
| Deployment orchestration | CI/CD concern | GitHub Actions, ArgoCD, etc. |
| Database migrations | Framework concern | ORM / migration tool |
| Compliance rule authoring | Regulatory domain expertise | Compliance add-on project |

---

## The Plugin Line

TA's extension model has two layers:

**Adapters** extend TA's core by implementing a trait for a new system:
- `SourceAdapter` (Git, SVN, Perforce) — VCS submit + sync operations
- `BuildAdapter` (Cargo, npm, script, webhook) — build verification
- `ChannelAdapter` (CLI, web, Slack, Discord, email) — human interaction
- `ResourceMediator` (fs, email, db, api) — staging for any resource type
- `EventResponder` (notify, block, agent-route) — event-driven responses (post-0.10.x)

Adapters are governed — they operate within TA's policy engine and emit events through TA's event system.

**Plugins** observe and advise but cannot bypass policy:
- Advisors (pre-execution recommendations)
- Reviewers (draft review assistance)
- Auditors (post-execution analysis)
- Optimizers (longitudinal improvements)

See `docs/plugins-architecture-guidance.md` for the full plugin event model.

---

## Integration Protocols

TA uses three protocols. All are JSON-native — no protobuf, no code generation, no language-specific stubs required.

### Protocol summary

| Interface | Protocol | Transport | Auth | Direction |
|-----------|----------|-----------|------|-----------|
| Agent ↔ TA | MCP (JSON-RPC 2.0) | stdio pipes | N/A (same process boundary) | Bidirectional |
| CLI / Web / Plugins ↔ Daemon | REST JSON | HTTP (`127.0.0.1:7700`) | Bearer token | Request/Response |
| Events (real-time) | SSE | HTTP chunked | Bearer token | Server → Client |
| Events (external) | Webhook POST | HTTPS | Configurable per-hook | Server → External |
| Plugin (subprocess) | JSON-over-stdio | stdin/stdout pipes | N/A (same machine) | Bidirectional |
| Plugin (HTTP callback) | REST JSON | HTTPS | `auth_token_env` per-plugin | Server → External |

### Why REST JSON, not gRPC or GraphQL

**REST JSON** is TA's integration protocol because:

1. **Universal consumability.** Any language, any HTTP client, zero tooling. `curl` works. A 10-line Python script works. No `.proto` files, no code generation, no client library required.
2. **TA's payloads are small.** Questions, answers, events, and draft metadata are simple JSON objects (typically <10KB). gRPC's value is streaming and large payloads — TA doesn't need either.
3. **MCP already uses JSON-RPC 2.0.** The agent interface is defined by the MCP spec. Adding a second RPC protocol for plugins would mean two serialization formats, two schema systems, and two sets of client libraries for no benefit.
4. **Plugin simplicity.** A channel plugin in Python is: read JSON from stdin, POST to a webhook, print JSON to stdout. Adding protobuf compilation or gRPC stubs would make plugins harder to write, not easier.

**When to reconsider:** If a future use case demands high-throughput event streaming (thousands of events/second across many agents), gRPC streaming could be added as an alternative transport alongside REST — not replacing it. This would be an optimization, not an architectural change.

### Authentication

| Boundary | Auth mechanism | Config |
|----------|---------------|--------|
| **Daemon API** (inbound) | Bearer token | `daemon.toml: [api] auth_token_env = "TA_API_TOKEN"` |
| **Webhook hooks** (outbound) | Per-hook token/header | `.ta/hooks.toml: auth_header = "Bearer $TA_HOOK_TOKEN"` |
| **Plugin HTTP callback** (outbound) | Per-plugin token | `daemon.toml: [[channels.external]] auth_token_env = "..."` |
| **Plugin subprocess** (local) | Process isolation | Inherits daemon's trust boundary — no network auth needed |
| **MCP stdio** (local) | Process isolation | Daemon spawns agent — same trust boundary |

**Daemon API token**: Required when the daemon is exposed beyond localhost. When bound to `127.0.0.1` (default), token auth is optional but recommended. External channel services (Discord bots, Slack apps) calling `/api/interactions/:id/respond` must include the bearer token.

**No TLS in the daemon itself.** TA relies on reverse proxies (nginx, Caddy, cloud load balancers) or SSH tunnels for encryption. The daemon is a local-first tool — adding TLS certificate management would add complexity without matching the deployment model. For remote access, put the daemon behind a TLS-terminating proxy.

### JSON schemas

All protocol payloads derive `serde::Serialize`/`Deserialize` and `schemars::JsonSchema`. JSON Schema definitions can be generated from the Rust types for use by plugin authors in any language.

Key schemas for plugin/integration authors:

| Schema | Rust type | Used by |
|--------|-----------|---------|
| `ChannelQuestion` | `ta_events::channel::ChannelQuestion` | Channel plugins (inbound question to deliver) |
| `DeliveryResult` | `ta_events::channel::DeliveryResult` | Channel plugins (delivery confirmation) |
| `EventEnvelope` | `ta_events::schema::EventEnvelope` | SSE consumers, webhook receivers |
| `SessionEvent` | `ta_events::schema::SessionEvent` | Event routing, hook payloads |
| `HumanAnswer` | `ta_daemon::question_registry::HumanAnswer` | `/api/interactions/:id/respond` request body |

**Schema export** (planned): `ta schema export --format json-schema` will dump all plugin-facing schemas as JSON Schema files. Plugin SDK templates will include pre-generated schemas for Python (dataclasses), TypeScript (interfaces), and Go (structs). Currently, plugin authors reference the Rust types directly or use the example plugins as a template.

### Credential & Secret Configuration

TA has a credential vault (`ta-credentials` crate) that brokers secrets so agents never hold raw credentials. But plugin authors and users deploying TA also need clear patterns for connecting their own credentials.

**How secrets are configured across deployment modes:**

| Deployment | Secret storage | How to configure |
|------------|---------------|-----------------|
| **Local development** | Environment variables | `.env` file (gitignored), shell exports, or `daemon.toml` `*_env` references |
| **CI/CD** | GitHub Actions secrets / CI secret store | Set as env vars in workflow; TA reads via `*_env` config fields |
| **Cloud hosting** | Cloud secret manager (AWS SSM, GCP Secret Manager, etc.) | Inject as env vars at container/instance startup; TA reads via `*_env` |
| **TA credential vault** | `.ta/credentials.json` (0600 permissions) | `ta credentials add --name "..." --service ... --secret "..."` |

**The `*_env` pattern**: Throughout TA's configuration, secrets are referenced by environment variable name, never stored in config files directly:

```toml
# daemon.toml — secrets referenced by env var name
[api]
auth_token_env = "TA_API_TOKEN"        # daemon reads $TA_API_TOKEN at startup

[channels.discord]
token_env = "TA_DISCORD_TOKEN"         # reads $TA_DISCORD_TOKEN

[channels.slack]
token_env = "TA_SLACK_TOKEN"

[[channels.external]]
name = "custom"
auth_token_env = "TA_CUSTOM_TOKEN"     # outbound auth for HTTP callback plugins
```

```bash
# Local: .env file (add to .gitignore)
TA_API_TOKEN=sk-ta-...
TA_DISCORD_TOKEN=MTIz...
TA_SLACK_TOKEN=xoxb-...

# Or export directly
export TA_API_TOKEN=sk-ta-...
```

**TA credential vault** is for secrets the *agent* needs (API keys for external services the agent calls). Plugin/daemon secrets use env vars because they're consumed at startup, not brokered to agents.

**What TA never does**: Store secrets in TOML/YAML config files, log secret values, or pass raw credentials to agents. The `credential_access` alignment action is forbidden by default for all agent profiles.

---

## Design Principles

1. **Invisible to agents.** Agents work in staging using their native tools. They don't know TA exists. TA diffs the result and creates drafts.

2. **Adapters, not implementations.** TA calls external tools through adapter traits. Swapping Git for Perforce, or Cargo for npm, is a config change.

3. **Events, not side effects.** Every transition emits a structured event. Workflows gate on events. Channels deliver events. Audit logs record events. Nothing happens silently.

4. **Human gates, not human labor.** TA pauses at decision points, not at every step. The goal is less human attention per unit of work, not zero human attention.

5. **Config over code.** Workflows, agent profiles, build commands, and channel routing are YAML/TOML configuration. Users customize TA by editing config files, not by writing Rust.

6. **Templates as starting points.** TA ships default templates for workflows, agent profiles, and build configs. Users override per-project. Community publishes to registries.

---

## Summary: Why TA Needs Each Feature

| Feature | Lifecycle transition governed | Without TA |
|---------|------------------------------|------------|
| `ta run` | Goal → agent work → draft | Human babysits agent, manually reviews output |
| `ta draft` | Draft → review → approve/deny → apply | Human manually diffs files, copies them by hand |
| `ta sync` | Feature branch → main current | Human manually merges PR, pulls main |
| `ta build` | Code changed → build verified | Human manually runs build, mentally tracks result |
| `ta release` | Build verified → version cut → published | Human manually bumps version, writes changelog, tags |
| `ta plan` | Product doc → phased development plan | Human manually writes plan or runs agent ad-hoc |
| Interactive mode | Agent paused → human answers → agent continues | Agent runs to completion blind, or human babysits |
| Workflows | Multi-stage pipeline with gates | Human manually sequences steps, tracks state |
| Channels | Event → human notified where they work | Human must watch terminal or check dashboard |
| Policy | Agent action → policy check → allow/deny/escalate | Human trusts agent blindly or reviews everything |
| Audit | Every transition → tamper-evident log | No record beyond chat history |
