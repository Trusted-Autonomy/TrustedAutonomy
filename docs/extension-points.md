# TA Extension Points

> How to build on top of TA — plugins, integrations, and customization.

TA is a governance platform. Its value comes from the mediation substrate, not from implementing every possible integration. This document describes the points where external projects and plugins connect.

---

## Extension Model

TA follows a "traits + YAML" extension model:
- **Rust traits** define the interface (what you implement)
- **YAML config** defines the binding (how TA discovers and uses your implementation)
- **TA never calls paid or external code at compile time** — extensions are runtime-pluggable or feature-gated

---

## 1. Resource Mediators (Layer 1)

**What**: Stage, preview, apply, and rollback mutations for a resource type.

**Trait**: `ResourceMediator` (defined in `ta-mediation` crate)
```rust
trait ResourceMediator: Send + Sync {
    fn scheme(&self) -> &str;           // "fs", "email", "db", "api", "cloud"
    fn stage(&self, action: ProposedAction) -> Result<StagedMutation>;
    fn preview(&self, staged: &StagedMutation) -> Result<MutationPreview>;
    fn apply(&self, staged: &StagedMutation) -> Result<ApplyResult>;
    fn rollback(&self, staged: &StagedMutation) -> Result<()>;
    fn classify(&self, action: &ProposedAction) -> ActionClassification;
}
```

**Config** (`.ta/config.yaml`):
```yaml
mediators:
  email:
    enabled: true
    provider: gmail
    credential: "gmail-oauth"
```

**Built-in**: `FsMediator` (file staging — always available).

**How to add a mediator**: Implement `ResourceMediator` in a crate, register it with `MediatorRegistry` in the MCP gateway. URI schemes are categories (e.g., `email://`), providers are variants (e.g., Gmail, Outlook).

---

## 2. Policy Documents (Layer 2)

**What**: Define rules for what agents can and cannot do.

**Format**: YAML files at well-known paths.

**Policy cascade** (layers that stack, each can tighten but never loosen):

| Layer | Path | Scope |
|---|---|---|
| Project policy | `.ta/policy.yaml` | All agents, all goals |
| Workflow policy | `.ta/workflows/<name>.yaml` | Per-workflow (e.g., code-review, email-triage) |
| Agent profile | `agents/<name>.yaml` | Per-agent framework |
| Goal constitution | `.ta/constitutions/goal-<id>.yaml` | Per-goal |

**How to generate policy**: Write YAML by hand, or use an external tool (like the paid Policy Studio) to generate it. TA's `PolicyCompiler` validates and merges all layers at runtime.

**Key types projects generate**:
- `AlignmentProfile` — agent capability declarations (bounded actions, forbidden actions, escalation triggers)
- `AccessConstitution` — per-goal URI scope declarations with enforcement mode

---

## 3. Review Channels (Layer 5)

**What**: Deliver TA interactions (draft reviews, notifications, approvals) to a communication platform.

**Traits**: `ReviewChannel` + `SessionChannel` (defined in `ta-changeset`)
```rust
trait ReviewChannel: Send + Sync {
    fn request_interaction(&self, request: &InteractionRequest) -> Result<InteractionResponse>;
    fn notify(&self, notification: &Notification) -> Result<()>;
    fn capabilities(&self) -> ChannelCapabilities;
    fn channel_id(&self) -> &str;
}

trait SessionChannel: Send + Sync {
    fn emit(&self, event: SessionEvent) -> Result<()>;
    fn receive(&self, timeout: Duration) -> Result<Option<HumanInput>>;
    fn channel_id(&self) -> &str;
}
```

**Factory**: `ChannelFactory` registered with `ChannelRegistry`.

**Config** (`.ta/config.yaml`):
```yaml
channels:
  review: { type: slack, channel: "#reviews" }
  notify: [{ type: terminal }, { type: slack, level: warning }]
  session: { type: terminal }
  escalation: { type: email, to: "mgr@co.com" }
```

**Built-in**: `TerminalChannel`, `AutoApproveChannel`, `WebhookChannel`.

**How to add a channel**: Implement `ChannelFactory` in a crate (e.g., `ta-channel-slack`). Register it with the channel registry. TA routes interactions to your channel based on `.ta/config.yaml`.

---

## 4. Memory Backends (Layer 4)

**What**: Pluggable storage for agent-persistent memory.

**Trait**: `MemoryStore` (defined in `ta-memory`)
```rust
trait MemoryStore: Send + Sync {
    fn store(&mut self, key: &str, value: Value, tags: Vec<String>, source: &str) -> Result<MemoryEntry>;
    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>>;
    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>>;
    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>>;
    fn forget(&mut self, key: &str) -> Result<bool>;
    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<MemoryEntry>>;
    fn stats(&self) -> Result<MemoryStats>;
}
```

**Built-in**: `FsMemoryStore` (JSON files), `RuVectorStore` (HNSW semantic search, feature-gated).

**How to add a backend**: Implement `MemoryStore` in a crate, configure it in `.ta/workflow.toml`.

---

## 5. Submit Adapters (Layer 1)

**What**: Post-review submission to a VCS or deployment target.

**Trait**: `SubmitAdapter` (defined in `ta-submit`)
```rust
trait SubmitAdapter: Send + Sync {
    fn prepare(&self, goal: &GoalRun, config: &SubmitConfig) -> Result<()>;
    fn commit(&self, goal: &GoalRun, pr: &DraftPackage, message: &str) -> Result<CommitResult>;
    fn push(&self, goal: &GoalRun) -> Result<PushResult>;
    fn open_review(&self, goal: &GoalRun, pr: &DraftPackage) -> Result<ReviewResult>;
    fn name(&self) -> &str;
}
```

**Built-in**: `GitAdapter` (git commit + push + PR), `NoneAdapter` (file copy only).

**How to add an adapter**: Implement `SubmitAdapter` for your VCS (Perforce, SVN, Mercurial) or deployment target (Docker registry, Kubernetes).

---

## 6. Credential Providers (Layer 4)

**What**: Manage secrets and identity tokens for agent access to external services.

**Trait**: `CredentialVault` (defined in `ta-credentials`)

**Built-in**: `FileVault` (JSON file with restrictive permissions).

**How to add a provider**: Implement `CredentialVault` for your secret store (HashiCorp Vault, AWS Secrets Manager, 1Password CLI).

---

## 7. Session Events (Layer 3)

**What**: Subscribe to real-time events from TA sessions for external automation.

**Event types**: `SessionEvent` enum — `FileChanged`, `ActionIntercepted`, `DraftReady`, `AgentOutput`, `AgentWaiting`, `GoalCompleted`, `ReviewDecision`.

**Subscription**: `ta events listen` streams JSON events. Webhook hooks fire on state transitions.

**How projects on top use this**:
- Virtual Office subscribes to `GoalCompleted` to trigger next workflow step
- Infra Ops subscribes to `ActionIntercepted` for cloud:// actions to show IaC diffs
- Monitoring dashboards subscribe to all events for real-time visibility

---

## 8. Agent Launch Configs (Layer 4)

**What**: Define how to start and configure different agent frameworks.

**Format**: YAML files in `agents/` directory.
```yaml
# agents/claude-code.yaml
name: claude-code
command: claude
args: ["--project-root", "{workspace_path}"]
shell: bash
alignment_profile:
  principal: developer
  autonomy:
    bounded_actions: ["fs_read", "fs_write", "exec: cargo *"]
    forbidden_actions: ["exec: rm -rf *"]
    escalation_triggers: ["new_dependency"]
```

**How to add an agent framework**: Create a YAML file in `agents/` defining the launch command, alignment profile, and settings injection pattern. TA launches it and mediates its resource access.

---

## Summary: What Goes Where

| I want to... | Extension point | Built-in | Plugin/Paid |
|---|---|---|---|
| Stage non-file mutations | ResourceMediator | `FsMediator` | `EmailMediator`, `DbMediator`, `CloudMediator` |
| Define agent permissions | Policy YAML | `.ta/policy.yaml` | Policy Studio (generation tool) |
| Deliver reviews to my team | ReviewChannel | Terminal, Webhook | Slack, Teams, Discord, Email |
| Store agent memory | MemoryStore | JSON files, RuVector | Custom backends |
| Submit to my VCS | SubmitAdapter | Git, None | Perforce, SVN |
| Manage secrets | CredentialVault | File vault | HashiCorp Vault, AWS SM |
| React to TA events | SessionEvent subscription | `ta events listen` | Custom webhook handlers |
| Use a different agent | AgentLaunchConfig | Claude Code, Codex | Any MCP-capable agent |
