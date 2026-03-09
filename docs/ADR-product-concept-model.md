# ADR: Product Concept Model — TA as a Governance Platform

> **Status**: Accepted
> **Author**: Claude (v0.5.7 session)
> **Supersedes**: Complements ADR-modular-decomposition (crate extraction); this ADR defines the product layer model and extension points
> **Context**: Untangling TA's concerns into a clear product model that supports building projects on top (Virtual Office, Autonomous Infra Ops, etc.)

---

## The Thesis

TA is a **governance infrastructure platform** — a base layer that other projects build on. It sits between AI agents and the resources they act on, enforcing that humans retain meaningful control.

**Core invariant: agents propose, humans dispose.**

TA is not an agent framework. It is not an orchestrator. It is the mediation substrate that makes any agent framework safe to use autonomously.

---

## The Five Layers

```
┌─────────────────────────────────────────────────────────┐
│  Projects on top: Virtual Office, Infra Ops, others     │
│  (generate workflows + agent guidance + security plans) │
├─────────────────────────────────────────────────────────┤
│  L5  IO & Delivery     CLI · Web · Slack · Discord      │
├─────────────────────────────────────────────────────────┤
│  L4  Agent Integration  MCP gateway · Memory · Creds    │
├─────────────────────────────────────────────────────────┤
│  L3  Session & Review   Human control plane · Drafts    │
├─────────────────────────────────────────────────────────┤
│  L2  Supervision        Policy engine · Drift · Audit   │
├─────────────────────────────────────────────────────────┤
│  L1  Resource Mediation Staging for files, APIs, email  │
└─────────────────────────────────────────────────────────┘
```

Each layer has a single responsibility, clear boundaries, and defined extension points.

---

### Layer 1 — Resource Mediation

**"The staging pattern, generalized to anything."**

Every state-changing action an agent proposes is staged before it touches the real world. For files, this is the existing staging workspace. For emails, it's a draft in the queue. For database operations, it's a recorded statement. For API calls, it's a serialized request.

**Key trait:**

```rust
trait ResourceMediator: Send + Sync {
    fn scheme(&self) -> &str;           // "fs", "email", "db", "api"
    fn stage(&self, action: ProposedAction) -> Result<StagedMutation>;
    fn preview(&self, staged: &StagedMutation) -> Result<MutationPreview>;
    fn apply(&self, staged: &StagedMutation) -> Result<ApplyResult>;
    fn rollback(&self, staged: &StagedMutation) -> Result<()>;
    fn classify(&self, action: &ProposedAction) -> ActionClassification;
}
```

**URI schemes are categories, not products.** `email://` is the scheme; Gmail, Outlook, Fastmail are provider variants configured via credentials. Same pattern: `db://` with Postgres/MySQL/SQLite variants, `cloud://` with AWS/GCP/Azure variants.

| Scheme | Provider examples | Staging mechanism |
|---|---|---|
| `fs://` | local filesystem | Copy to staging directory |
| `email://` | Gmail, Outlook, Fastmail | Create draft via provider API |
| `db://` | Postgres, MySQL, SQLite | Record SQL in transaction log |
| `api://` | Any MCP tool call | Serialize request + parameters |
| `social://` | Twitter/X, LinkedIn, Bluesky | Create draft post |
| `cloud://` | AWS, GCP, Azure | Record IaC change / API call |

**Core vs. extension:**
- Core: `ResourceMediator` trait, `FsMediator` (wraps existing `FsConnector` + `StagingWorkspace`)
- Extension: `EmailMediator`, `DbMediator`, `ApiMediator` — each a separate crate with provider-specific adapters

**Configuration** (`.ta/config.yaml`):
```yaml
mediators:
  fs:
    enabled: true              # always on
  email:
    enabled: true
    provider: gmail            # provider variant
    credential: "gmail-oauth"  # references CredentialVault entry
  db:
    enabled: false             # opt-in
    provider: postgres
    credential: "pg-prod"
```

**Opt-in scope:** File mediation is always on. Other mediators are enabled explicitly by the human, not the agent. You would NOT mediate a resource when: (a) it's read-only, (b) it's in a sandbox with no real-world effect, or (c) the cost of review exceeds the cost of the mistake.

**Output types** feed into existing `DraftPackage.changes` — Artifacts for files, PatchSets for external resources, PendingActions for API calls.

**Crate:** New `ta-mediation` (trait + shared types). Existing `ta-connectors/fs` evolves to implement `ResourceMediator`.

---

### Layer 2 — Supervision & Policy

**"Default-deny, configurable via YAML."**

Every proposed action is evaluated against policy before staging. The policy engine is default-deny — no manifest means denied.

#### Policy cascade: layers that stack

Policies compose through a layered cascade. Each layer can tighten but never loosen the layer above it:

```
┌─────────────────────────────────────────────┐
│  1. Built-in defaults (hardcoded)           │  Path traversal always denied.
│     Cannot be overridden.                   │  Approval-required verbs enforced.
├─────────────────────────────────────────────┤
│  2. Project policy (.ta/policy.yaml)        │  Global rules for this project.
│     Sets the baseline for all agents/goals. │  Schemes, escalation, security level.
├─────────────────────────────────────────────┤
│  3. Workflow policy (.ta/workflows/*.yaml)  │  Per-workflow overrides.
│     E.g., "code-review" vs. "email-triage"  │  Can restrict, cannot expand.
├─────────────────────────────────────────────┤
│  4. Agent profile (agents/<name>.yaml)      │  Per-agent capabilities.
│     E.g., claude-code vs. codex             │  Bounded actions, forbidden actions.
├─────────────────────────────────────────────┤
│  5. Goal constitution (.ta/constitutions/)  │  Per-goal scope.
│     E.g., "this goal can only touch src/"   │  Narrowest scope wins.
├─────────────────────────────────────────────┤
│  6. CLI overrides (flags)                   │  Session-level tweaks.
│     E.g., --auto-approve, --strict          │  Temporary, not persisted.
└─────────────────────────────────────────────┘
```

**Resolution rule:** At each layer, rules can only add restrictions or escalation triggers — never remove them. If the project policy says `email: { approval_required: [send] }`, a workflow policy cannot set `email: { approval_required: [] }`. It can add `email: { approval_required: [send, delete] }`.

**Project policy** (`.ta/policy.yaml`):
```yaml
version: "1"

defaults:
  enforcement: warning          # warning | error | strict
  auto_approve:
    read_only: true             # reads pass without review
    internal_tools: true        # ta_* tools pass without review

schemes:
  fs:
    approval_required: [apply, delete]
  email:
    approval_required: [send, delete]
    credential_required: true
  db:
    approval_required: ["*"]    # everything requires approval

escalation:
  - new_dependency
  - security_sensitive
  - breaking_change
  - budget_exceeded
  - external_communication
```

**Workflow policy** (`.ta/workflows/code-review.yaml`):
```yaml
extends: .ta/policy.yaml         # inherits project defaults
schemes:
  fs:
    file_patterns: ["src/**", "tests/**"]   # restrict to source files only
    approval_required: [apply, delete]
auto_approve:
  supervisor: true                # enable constitutional auto-approval
  risk_score_max: 20
```

**Goal constitution** (`.ta/constitutions/goal-<id>.yaml`):
```yaml
# Narrowest scope — this specific goal can only touch these URIs
allowed_uris:
  - "fs://workspace/src/auth/**"
  - "fs://workspace/tests/auth/**"
enforcement: error                # violations block the draft
```

**Runtime-aware decisions** via `PolicyContext`:
```rust
struct PolicyContext {
    goal_id: Option<Uuid>,
    session_id: Option<Uuid>,
    budget_spent: Option<f64>,
    action_count: usize,
    drift_score: Option<f64>,
}
```

This enables rules like "allow file writes, but escalate after 50 writes" or "allow email sends, but escalate if budget exceeds $5."

**Security levels (opt-in):**

| Level | Behavior | Use case |
|---|---|---|
| Open | Auto-approve all, audit-only | Sandboxed experiments, trusted agents |
| Checkpoint (default) | Agent runs freely, human reviews at draft | Normal development |
| Supervised | Every state-changing action needs approval | Production, external comms |
| Strict | Everything logged, constitutions required | Regulated environments |

**Extension point — policy generation:** TA evaluates any YAML you give it. External tools (including the paid Policy Studio) can generate and validate policy YAML. Virtual Office and Infra Ops generate `AlignmentProfile` and `AccessConstitution` YAML that TA consumes. Projects own "smart security plan" generation; TA owns enforcement.

**Crate:** `ta-policy` gains `PolicyDocument` loading. `ta-audit` unchanged.

---

### Layer 3 — Session & Review

**"The human control plane."**

This is the most significant reframing. Layer 3 is not just "review drafts" — it is the **ongoing interactive session between the human and TA** that the agent framework does not see or control.

#### The TA Session

A TA session is a **continuous conversation** between the human and TA about a goal. It:

- **Starts** when the human begins work (`ta run` or equivalent from any IO channel)
- **Runs one agent framework, one goal** at a time (simple lifecycle)
- **Provides** a command surface the agent cannot access (safety boundary)
- **Streams** events in real-time across all IO channels
- **Continues across iterations** — when the human rejects a draft and gives feedback, TA relaunches the agent with full context (previous work + human feedback) so it feels like one conversation
- **Ends** when the human decides

The human does not track goal IDs, session IDs, or draft IDs. TA tracks the conversational thread. `ta draft view` shows the latest. `ta draft approve` approves it. If the human says "redo the auth module using JWT instead," TA starts a continuation with the original context + the feedback, and the agent picks up where it left off.

#### TA is invisible to the agent

The agent framework (Claude Code, Codex, Claude Flow, LangGraph, etc.) does not call TA MCP tools. TA is invisible:

1. TA launches the agent as a subprocess in the staging workspace
2. Agent works normally — reads files, writes files, runs commands
3. Agent writes `change_summary.json` with rationale (what, why, alternatives considered)
4. Agent exits (process terminates)
5. TA diffs the staging workspace against the source
6. TA builds the draft automatically and notifies the human

The "I'm done" signal is process exit. Every framework does this reliably. TA watches the child process.

For **orchestrators** (Claude Flow, LangGraph, CrewAI) that run multiple agents internally, TA treats the orchestrator as a single agent framework. TA doesn't know or care that there are multiple agents inside — it just sees the final workspace state when the orchestrator exits. The orchestrator handles its own internal coordination (coder→reviewer→supervisor loops, exit criteria, agent-to-agent communication). TA mediates all resource access through the MCP gateway.

#### Human control plane commands

These go through TA's own endpoint. The agent framework connects to a *different* endpoint (the gateway). **The agent never sees session commands.**

```
ta session status          # what's the agent doing right now?
ta session pause/resume    # pause agent execution
ta draft view              # review the latest draft (no ID needed)
ta draft approve           # approve latest draft
ta draft reject "reason"   # reject with feedback → TA continues session
ta audit trail             # what happened so far
ta context list            # agent memory from this and previous sessions
```

When the human rejects with feedback, TA:
1. Stores the human's feedback in memory
2. Relaunches the agent with: original goal + previous work context + human feedback
3. The agent sees it as a continuation, not a fresh start

#### Conversational continuity

From the human's perspective, a sequence of goal→draft→reject→revise→approve is **one conversation**, not separate sessions. TA stitches this together via the memory module:

- Previous agent work (what was tried, what was rejected, why)
- Human feedback (comments, requested changes)
- Applied changes (what was approved in earlier iterations)
- Decision history (which approaches were accepted/rejected)

The next agent invocation receives this context through CLAUDE.md injection (or equivalent for other frameworks). The human never re-explains context — it carries forward.

#### Orchestrators are agent frameworks, not TA features

TA does NOT orchestrate multi-agent workflows. Orchestrators (Claude Flow, LangGraph, CrewAI) do that. From TA's perspective, an orchestrator is just another entry in `agents/`:

```yaml
# agents/claude-flow.yaml
name: claude-flow
command: claude-flow
args: ["--workflow", "feature-dev", "--project", "{workspace_path}"]
```

TA launches it, mediates its resource access through the MCP gateway, and diffs the result when it exits. The orchestrator's internal agent coordination is opaque to TA.

**Workflow policy files** (`.ta/workflows/*.yaml`) define policy overrides per workflow type, not agent coordination:

```yaml
# .ta/workflows/deploy.yaml
# Policy overrides — the orchestrator defines the agent flow
defaults:
  enforcement: supervised
escalation:
  - any_external_call
```

#### Change summary with rationale

The agent writes `change_summary.json` before exiting. v0.6 adds a `rationale` field:

```json
{
  "summary": "Implemented JWT auth middleware",
  "rationale": {
    "approach": "JWT with RS256 via jsonwebtoken crate",
    "alternatives_considered": [
      "Session cookies — simpler but requires server-side state",
      "OAuth2 only — too complex for first-party auth"
    ],
    "tradeoffs": "Stateless but requires token refresh logic"
  },
  "changes": [...]
}
```

The human sees this in `ta draft view` — the agent's reasoning, not just the code.

#### Key types

```rust
struct TaSession {
    session_id: Uuid,
    current_goal: Option<GoalRun>,
    conversation_history: Vec<ConversationTurn>,  // human↔agent thread
    pending_review: Option<PendingReviewItem>,
    event_stream: broadcast::Sender<SessionEvent>,
}

enum ConversationTurn {
    AgentDraft { draft_id: Uuid, summary: String, rationale: Option<Rationale> },
    HumanFeedback { disposition: Disposition, comments: String },
    AgentRevision { draft_id: Uuid, changes_from_previous: String },
}

enum SessionEvent {
    FileChanged { path: String, diff_preview: String },
    ActionIntercepted { action: PendingAction },
    DraftReady { draft_id: Uuid, summary: String },
    GoalCompleted { goal_id: Uuid, status: GoalRunState },
    ReviewDecision { target: String, disposition: ArtifactDisposition },
}
```

**Crate:** New `ta-session` (session lifecycle, conversational continuity, event streaming). `ta-changeset` keeps `DraftPackage`, `ReviewChannel`, interaction types.

---

### Layer 4 — Agent Integration

**"TA connects to any framework without coupling to any."**

TA launches agent frameworks as subprocesses, mediates their resource access, and captures their output. TA is a Rust daemon — not an LLM. It does NOT orchestrate agents.

**TA is invisible to the agent.** The agent doesn't call TA MCP tools. It works normally in the staging workspace, writes files, runs commands, and exits. TA observes, diffs, and builds drafts.

**What TA provides (the Agent Contract) — injected before launch:**
- Goal description + context from previous iterations (conversational continuity)
- Access scope (from policy + constitution)
- Memory context (relevant entries from this and previous sessions)
- Instructions to write `change_summary.json` with rationale before exiting
- Credential tokens (scoped, time-bounded, brokered through MCP gateway)

**How the contract is delivered:**
- Claude Code: CLAUDE.md injection (markdown prepended to project instructions)
- Codex: system prompt or context file
- Orchestrators (Claude Flow, LangGraph): environment variables + config file in workspace

**What is NOT TA's job:**
- Agent selection ("which agent for this task?")
- Goal decomposition ("break this into sub-tasks")
- Multi-agent coordination (orchestrators handle this internally)
- Agent-to-agent communication
- Tool implementation (TA intercepts calls, doesn't implement `email_send`)

**Extension points for projects on top:**
- Virtual Office generates agent guidance + workflow definitions → TA consumes as `AlignmentProfile` + `AgentLaunchConfig`
- Infra Ops generates IaC plans → TA mediates the infrastructure changes
- Projects provide their own agent selection; TA enforces policy on whatever they choose

**Crates:** `ta-mcp-gateway`, `ta-credentials`, `ta-memory` — all unchanged.

---

### Layer 5 — IO & Delivery

**"Route TA to wherever the human is."**

All IO channels are equal. CLI, web UI, Slack, Discord, email — pluggable implementations of the same traits. No channel is special.

**Channel routing** (`.ta/config.yaml`):
```yaml
channels:
  review:
    type: slack
    channel: "#reviews"
  notify:
    - type: terminal
    - type: slack
      level: warning
  session:
    type: terminal
    fallback: web
  escalation:
    type: email
    to: "manager@company.com"
```

**Channels can set defaults for agent frameworks:**
```yaml
channels:
  review:
    type: slack
    channel: "#engineering"
    default_agent: claude-code
    default_workflow: standard-dev
```

**Key abstraction:**
```rust
trait ChannelFactory: Send + Sync {
    fn build_review(&self, config: &Value) -> Result<Box<dyn ReviewChannel>>;
    fn build_session(&self, config: &Value) -> Result<Box<dyn SessionChannel>>;
    fn capabilities(&self) -> ChannelCapabilities;
}

struct ChannelRegistry {
    factories: HashMap<String, Box<dyn ChannelFactory>>,
}
```

**Core:** `ReviewChannel`, `SessionChannel`, `ChannelRegistry`, `TerminalChannel`, web UI.
**Plugin:** `ta-channel-slack`, `ta-channel-discord`, `ta-channel-email` — separate crates.

---

## TA as a Platform

```
┌───────────────────────┐  ┌──────────────────────────┐  ┌──────────┐
│   Virtual Office      │  │   Autonomous Infra Ops   │  │  Others  │
│                       │  │                          │  │          │
│ • Multi-agent         │  │ • Builder intent → IaC   │  │ • Custom │
│   workflow design     │  │ • Self-healing infra     │  │   apps   │
│ • Agent guidance      │  │ • Observability          │  │          │
│   generation          │  │ • Best-practice          │  │          │
│ • Smart security      │  │   templates              │  │          │
│   plan generation     │  │                          │  │          │
├───────────────────────┴──┴──────────────────────────┴──┴──────────┤
│                    TA Extension Points                             │
│                                                                   │
│  • AlignmentProfile YAML    — projects generate, TA enforces      │
│  • AgentLaunchConfig YAML   — projects define, TA launches        │
│  • ResourceMediator plugins — projects provide, TA stages/reviews │
│  • ReviewChannel plugins    — projects provide, TA routes through │
│  • Memory entries           — projects write, TA persists/queries │
│  • PolicyDocument rules     — projects propose, TA merges/enforces│
│  • TaSession events         — projects subscribe, TA publishes    │
├───────────────────────────────────────────────────────────────────┤
│                    TA Core (this repo)                             │
│  L1 Resource Mediation  │  L2 Supervision & Policy                │
│  L3 Session & Review    │  L4 Agent Integration                   │
│  L5 IO & Delivery       │                                         │
└───────────────────────────────────────────────────────────────────┘
```

**What flows down (projects → TA):**
- Workflow definitions (which agents, what guidance, what security)
- Policy proposals (alignment profiles, constitutions)
- Custom mediators and channels

**What flows up (TA → projects):**
- Session events (what the agent is doing)
- Policy decisions (what was allowed/denied)
- Audit trail (what happened)
- Memory (what was learned)

---

## Data Flow (complete request)

```
Agent calls email_send via MCP
  → L4 (Agent Integration): MCP gateway receives the tool call
  → L2 (Supervision): PolicyEngine evaluates — check email:// grants, budget, drift
  → L1 (Resource Mediation): EmailMediator.stage() → creates provider draft (Gmail, Outlook, etc.)
  → L3 (Session & Review): StagedMutation added to DraftPackage
  → L5 (IO): Slack notification — "Agent wants to send email to X, approve?"
  → Human approves via Slack
  → L1 (Resource Mediation): EmailMediator.apply() → sends via provider
  → L2 (Supervision): AuditLog records outcome
```

---

## Crate Map

| Crate | Layer | Status |
|---|---|---|
| `ta-policy` | L2 | Done: `PolicyDocument` YAML loading, `PolicyCascade`, `PolicyContext` |
| `ta-audit` | L2 | Stable |
| `ta-workspace` | L1 | Stable (low-level file staging) |
| `ta-changeset` | L3 | Done: DraftPackage, ReviewChannel, ChannelRegistry, interactions |
| `ta-goal` | L3 | Done: goal stacking, multi-agent session tracking |
| `ta-submit` | L1 | Stable: SubmitAdapter trait + git implementation |
| `ta-memory` | L4 | Done: file + ruvector backends, auto-capture, semantic search |
| `ta-credentials` | L4 | Done: credential vault, identity broker |
| `ta-mcp-gateway` | L4 | Done: ResourceMediator routing, tool interception |
| `ta-connectors/fs` | L1 | Done: implements `ResourceMediator` trait |
| `ta-daemon` | L5 | Done: HTTP API, SSE events, session streaming, channel plugin host |
| `ta-sandbox` | L2 | Done: command allowlisting, path escape detection |
| `ta-cli` | L5 | Done: full human control plane (session, shell, dev, office, etc.) |
| `ta-mediation` | L1 | Done: `ResourceMediator` trait + shared types |
| `ta-session` | L3 | Done: session lifecycle, event streaming |
| `ta-events` | L3 | Done: event types, schemas, channel question protocol |
| `ta-workflow` | L3 | Done: workflow engine, stage orchestration, verdict scoring |
| Channel plugins | L5 | Done: Discord, Slack, Email (out-of-process JSON-over-stdio) |

---

## Paid Add-Ons

Paid add-ons are convenience and enterprise features — they never gate core functionality. A solo developer can use TA fully with hand-written YAML and the terminal.

**Boundary:** Open-source core has all traits, the default-deny engine, file mediation, terminal/web UI, and the audit trail. Paid add-ons provide better tooling, enterprise integrations, and compliance packaging.

**Planned add-ons** (maintained in a separate project):
- **Policy Studio** — Interactive policy YAML generation, compliance mapping, drift analysis
- **Enterprise Channels** — Teams, ServiceNow, PagerDuty, Jira channel implementations
- **Advanced Mediators** — Production DB (with staging overlay), cloud API, and social media mediators
- **Compliance Reporting** — ISO 42001, EU AI Act, NIST AI RMF evidence package generation

---

## Restructured Roadmap

### Near-term — Complete the platform substrate

| Phase | Layer | What |
|---|---|---|
| v0.6.0 — Session & Control Plane | L3 | `TaSession`, goal stacking, human control plane commands, session events |
| v0.6.1 — Unified Policy Config | L2 | `.ta/policy.yaml`, `PolicyDocument` loading, `PolicyContext` |
| v0.6.2 — Resource Mediation Trait | L1 | `ta-mediation` crate, `FsMediator` adaptation, mediator registry |

### Mid-term — Extensibility

| Phase | Layer | What |
|---|---|---|
| v0.7.0 — Channel Registry | L5 | `ChannelRegistry`, webhook improvements, first channel plugin (Slack) |
| v0.7.1 — API Mediator | L1 | Stage/preview/apply for intercepted MCP calls (builds on `PendingAction`) |
| v0.8.0 — Event System | L3→projects | Stable event types, subscription API for projects on top |
| v0.8.1 — Cost Tracking | L2 | Budget limits as policy rules, token counting per goal/agent |

### Later — Distribution + projects on top

| Phase | Layer | What |
|---|---|---|
| v0.9.0 — Distribution & Packaging | L5 | Desktop installer, OCI image, full web UI |
| v0.9.1 — Native Windows Support | L5 | MSVC build, cross-platform path handling |
| v0.9.2 — Sandbox Runner | L2 | OCI/gVisor, network policy, CWD enforcement |
| v1.0.0 — Virtual Office Runtime | Separate project | Multi-agent orchestration built on TA |
| Infra Ops | Separate project | Builder intent → IaC + self-healing on TA |

---

## Decision Record

1. **ResourceMediator is the L1 abstraction gap.** The file staging pattern works; it needs generalizing to arbitrary URI schemes. The data model (`PatchSet`, `PendingAction`) already anticipates this.

2. **Policy configuration unifies in `.ta/policy.yaml`.** The engine stays the same; the input becomes a merged PolicyDocument.

3. **The human control plane (L3) is TA's most distinctive feature.** Session commands that agents cannot see are the safety boundary that distinguishes TA from running agents directly.

4. **Fluid sessions are the default.** Checkpoint mode is opt-in for batch/CI. All IO channels support fluid interaction.

5. **TA does not orchestrate agents.** Agent selection, goal decomposition, and multi-agent coordination are explicitly out of scope. Projects on top do that.

6. **All IO channels are equal.** The web UI is not special — it's a channel plugin like Slack or Discord.

7. **Paid add-ons are convenience, not core functionality gating.** A solo developer can use TA fully with hand-written YAML and the terminal.
