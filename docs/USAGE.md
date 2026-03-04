# Trusted Autonomy -- User Guide

**Version**: v0.6.0-alpha

Trusted Autonomy (TA) is a governance wrapper for AI agents. It lets any agent work freely in an isolated workspace, then holds the proposed changes at a human review checkpoint before anything takes effect. You see what the agent wants to do, approve or reject each change, and maintain a complete audit trail.

---

## Table of Contents

1. [Quick Start](#quick-start)
   - [Install](#install)
   - [Your first goal in three commands](#your-first-goal-in-three-commands)
   - [Typical session workflow](#typical-session-workflow)
2. [Core Concepts](#core-concepts)
   - [The Staging Model](#the-staging-model)
   - [Goals](#goals)
   - [Drafts](#drafts)
   - [Agents](#agents)
3. [Common Workflows](#common-workflows)
   - [Single Task](#single-task)
   - [Follow-Up Iterations](#follow-up-iterations)
   - [Macro Goals (multi-draft sessions)](#macro-goals-multi-draft-sessions)
   - [Interactive Sessions (real-time streaming)](#interactive-sessions-real-time-streaming)
   - [Macro vs Interactive: when to use which](#macro-vs-interactive-when-to-use-which)
   - [Plan-Linked Goals](#plan-linked-goals)
   - [Review Sessions](#review-sessions)
   - [Correcting a Draft](#correcting-a-draft)
   - [Draft Lifecycle Hygiene](#draft-lifecycle-hygiene)
4. [Configuration](#configuration)
   - [Workflow Config](#workflow-config-taworkflowtoml)
   - [Agent Configuration](#agent-configuration)
   - [Alignment Profiles](#alignment-profiles)
   - [Access Constitutions](#access-constitutions)
   - [Configurable Summary Exemption](#configurable-summary-exemption)
   - [Plan Schema](#plan-schema-taplan-schemayaml)
5. [Advanced Features](#advanced-features)
   - [Selective Approval](#selective-approval)
   - [Behavioral Drift Detection](#behavioral-drift-detection)
   - [Conflict Detection](#conflict-detection)
   - [External Diff Handlers](#external-diff-handlers)
   - [Git Integration](#git-integration)
   - [Release Pipeline](#release-pipeline)
   - [Decision Observability](#decision-observability)
   - [Credential Management](#credential-management)
   - [Context Memory](#context-memory)
   - [Web Review UI](#web-review-ui)
   - [Webhook Review Channel](#webhook-review-channel)
   - [MCP Tool Call Interception](#mcp-tool-call-interception)
   - [Session Lifecycle](#session-lifecycle)
   - [Unified Policy Config](#unified-policy-config)
   - [Resource Mediation](#resource-mediation)
6. [Roadmap](#roadmap)
7. [Troubleshooting](#troubleshooting)
8. [Getting Help](#getting-help)

---

## Quick Start

### Install

**Option A -- Binary download (macOS / Linux)**

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/trustedautonomy/ta/releases/latest/download/ta-aarch64-apple-darwin.tar.gz
tar xzf ta-aarch64-apple-darwin.tar.gz
sudo mv ta /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/trustedautonomy/ta/releases/latest/download/ta-x86_64-apple-darwin.tar.gz
tar xzf ta-x86_64-apple-darwin.tar.gz
sudo mv ta /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/trustedautonomy/ta/releases/latest/download/ta-x86_64-unknown-linux-musl.tar.gz
tar xzf ta-x86_64-unknown-linux-musl.tar.gz
sudo mv ta /usr/local/bin/
```

**Option B -- Cargo install**

```bash
cargo install ta-cli
```

**Option C -- Build from source**

```bash
git clone https://github.com/trustedautonomy/ta.git
cd ta
./dev cargo build --workspace --release
# Binary is at target/release/ta
```

### Your first goal in three commands

```bash
# 1. Run a goal -- TA copies your project to an isolated workspace,
#    launches the agent, and captures all changes as a draft.
ta run "Add a README badge for build status" --source .

# 2. Review the draft -- see what changed and why.
ta draft view <draft-id>

# 3. Approve and apply -- changes land in your working directory.
ta draft approve <draft-id>
ta draft apply <draft-id>
```

### What just happened

1. **Staging**: TA copied your project into an isolated virtual workspace (`.ta/staging/`). The agent worked there, not in your real files.
2. **Draft**: When the agent finished, TA diffed the workspace against your source and packaged the changes into a draft.
3. **Review**: You reviewed the draft -- every changed file with a summary of what changed and why.
4. **Apply**: After approval, TA copied the approved changes back into your project.

The agent never touched your real files. If you reject the draft, nothing changes.

### Typical session workflow

Most TA usage follows this pattern. Whether you're implementing a feature, fixing a bug, or refactoring, the steps are the same:

```bash
# 1. Start a goal linked to a plan phase (if you have PLAN.md)
ta run "Implement credential broker" --source . --phase v0.5.0

# 2. Wait for the agent to finish (or use --macro for mid-session review)

# 3. Review what it did
ta draft list                    # find the draft
ta draft view <id>               # see changes + rationale per file

# 4. Three paths:
#    a) Accept and apply
ta draft approve <id>
ta draft apply <id> --git-commit

#    b) Reject and try again with feedback
ta draft deny <id> --reason "Wrong approach -- use JWT not sessions"
ta run "Fix: use JWT auth" --source . --follow-up

#    c) Partially accept
ta draft apply <id> --approve "src/**" --reject "config.toml"
ta run "Fix config.toml per review" --source . --follow-up
```

**For complex work** (multiple logical units), use macro mode so the agent can submit drafts mid-session and you review inline:

```bash
ta run "Build the v0.7 features" --source . --macro --phase v0.7.0
```

**For iterative refinement** (CI failures, review feedback), follow up without losing context:

```bash
ta run "Fix clippy warnings" --source . --follow-up
```

**To check what's next** in your plan:

```bash
ta plan next                     # shows next pending phase + suggested command
ta plan status                   # progress summary
```

---

## Core Concepts

### The Staging Model

TA creates an isolated copy of your project (the *virtual workspace*) for every goal. The agent works inside this copy using its native tools. TA is invisible to the agent -- it does not know TA exists. When the agent finishes, TA diffs the workspace against the original and packages the differences into a reviewable draft.

This means:
- Your source files are never modified until you explicitly apply a draft.
- Multiple goals can run concurrently without interfering with each other.
- If something goes wrong, you reject the draft and start over.

### Goals

A goal is a unit of work. It has a lifecycle:

```
Created --> Running --> DraftReady --> UnderReview --> Approved --> Applied --> Completed
```

You create a goal with `ta run`. The agent works on it. TA builds a draft. You review, approve, and apply. Each transition is recorded in the audit log.

Goals can link to plan phases (`--phase`), follow up on previous goals (`--follow-up`), or decompose into sub-goals (`--macro`).

### Drafts

A draft is the package of changes an agent produced, waiting for your review. It contains:
- **Artifacts** -- individual changed files with before/after diffs
- **Summaries** -- per-file descriptions of what changed and why
- **Dependencies** -- which files depend on each other
- **Decision log** -- the agent's reasoning and alternatives considered

Draft lifecycle:

```bash
ta draft list                   # See all drafts
ta draft view <id>              # Review a specific draft
ta draft approve <id>           # Mark as approved
ta draft apply <id>             # Copy approved changes to your project
ta draft close <id>             # Abandon without applying
```

All `<id>` arguments accept either a full UUID or an 8+ character prefix (e.g., `ta draft view a1b2c3d4`). If a prefix is ambiguous, you'll be asked to use a longer one.

For simple workflows, `ta draft apply` works directly on unapproved drafts (auto-approves on apply).

### Agents

TA wraps any agent framework. Out of the box it supports:
- **Claude Code** (default) -- Anthropic's coding agent
- **Codex** -- OpenAI's coding agent
- **Claude Flow** -- multi-agent orchestration

Use `--agent` to select:

```bash
ta run "Fix the bug" --source . --agent codex
```

You can add any agent by creating a YAML config file (see [Agent Configuration](#agent-configuration)).

---

## Common Workflows

### Single Task

The most basic workflow: one goal, one review, one apply.

```bash
ta run "Refactor the auth module" --source .
# Wait for agent to finish...

ta draft view <draft-id>
ta draft approve <draft-id>
ta draft apply <draft-id>
```

To apply with a git commit in one step:

```bash
ta draft apply <draft-id> --git-commit
```

### Follow-Up Iterations

Fix issues discovered during review without losing context:

```bash
# Start a follow-up (inherits context from the most recent goal)
ta run "Fix clippy warnings from review" --source . --follow-up

# With a specific parent goal
ta run "Address review feedback" --source . --follow-up <goal-id-prefix>

# With detailed instructions
ta run --source . --follow-up --objective "Fix the discuss items on config.toml -- add env var override support"

# From a file
ta run --source . --follow-up --objective-file review-notes.md
```

When the parent goal's staging directory still exists, TA prompts to reuse it. Choosing yes (the default) means work accumulates into a single unified draft.

```toml
# .ta/workflow.toml -- follow-up behavior
[follow_up]
default_mode = "extend"       # "extend" (reuse staging) or "standalone" (fresh copy)
auto_supersede = true          # auto-supersede parent draft when extending
```

### Macro Goals (multi-draft sessions)

For complex tasks that span multiple logical units of change, use `--macro`. The agent stays in a single long-running session and can submit multiple drafts for review without exiting.

```bash
ta run "Build the v0.7 features" --source . --macro
```

**How it works**: The agent receives MCP tools (`ta_draft`, `ta_goal_inner`, `ta_plan`) and can:
1. Work on a logical unit of change
2. Call `ta_draft { action: "build" }` to package changes
3. Call `ta_draft { action: "submit" }` — this **blocks** until you respond
4. Receive your feedback and continue to the next unit

You review inline as the agent works:

```
  Draft Ready for Review: abc123
  Files: src/auth/mod.rs, src/auth/jwt.rs
  Summary: Extract JWT validation into dedicated module

  [a]pprove  [r]eject  [d]iscuss  [v]iew diff
> a
  Approved. Agent continuing...
```

Use `d` to give feedback that the agent will see and act on:

```
> d please use the existing AuthError type from src/error.rs
```

**When to use**: Multi-phase features, large refactors, anything where you want to review incremental progress rather than waiting for one big draft at the end.

### Interactive Sessions (real-time streaming)

Use `--interactive` when you want to **see what the agent is doing in real-time** and be able to inject guidance mid-session.

```bash
ta run "Implement channel registry" --source . --interactive
```

**How it works**: TA wraps the agent subprocess in a PTY, so:
- Agent output streams to your terminal as it happens (you see edits, test runs, thinking)
- You can type guidance at any time — TA routes it to the agent via the ReviewChannel
- All interactions are logged in the session history
- Sessions support pause/resume

```bash
# Resume a paused session
ta run --resume <session-id>

# Session management
ta session list                    # List sessions
ta session show <session-id>       # View details and history
ta session pause <session-id>      # Pause a running session
ta session resume <session-id>     # Resume
ta session abort <session-id>      # Cancel
```

**When to use**: When you want visibility into the agent's process — watching it work, steering it when it goes off track, or learning how it approaches a problem.

### Macro vs Interactive: when to use which

These are **different concerns** and can be combined:

| Flag | What it controls | Adds |
|------|-----------------|------|
| `--macro` | **Review loop** — agent can submit multiple drafts mid-session | MCP tools for draft/plan/sub-goal management |
| `--interactive` | **I/O mode** — real-time PTY streaming + human input | PTY capture, stdin interleaving, session persistence |

**Decision guide**:

| Scenario | Recommended flags |
|----------|------------------|
| Simple single-file fix | *(neither)* — default mode, one draft on exit |
| Complex feature (multiple files, needs incremental review) | `--macro` |
| Unfamiliar codebase (want to watch and steer) | `--interactive` |
| Large multi-phase implementation with oversight | `--macro --interactive` |
| CI/batch automation | *(neither)* — or `--macro` with `auto-approve` channel |

**The full experience** — both flags together:

```bash
ta run "Build the v0.7 features" --source . --macro --interactive --phase v0.7.0
```

You see the agent working in real-time, can inject guidance, and review each logical unit of change as it's submitted. This is the recommended mode for implementing plan phases.

### Plan-Linked Goals

Link goals to `PLAN.md` phases for automatic tracking:

```bash
ta run "Complete Phase v0.4.5" --source . --phase v0.4.5

# When applied, PLAN.md is auto-updated to mark the phase done
```

Plan commands:

```bash
ta plan list                         # List all phases with status
ta plan status                       # Progress summary
ta plan next                         # Next pending phase with suggested command
ta plan validate v0.3.1              # Phase details, linked goals, draft summaries
ta plan history                      # Status transition history
ta plan init                         # Extract plan-schema.yaml from existing plan
ta plan create                       # Generate new plan from template
ta plan create --template feature    # Feature template
```

### Review Sessions

For thorough multi-step reviews with per-artifact comments:

```bash
# Start a review session
ta draft review start <draft-id>

# Comment on specific files
ta draft review comment "src/auth.rs" "Wrong approach -- use JWT not sessions"

# Set dispositions
ta draft review approve "src/lib.rs"
ta draft review reject "config.toml" --reason "Needs env var override"
ta draft review discuss "src/auth.rs" --comment "Questions about this approach"

# Navigate through unreviewed artifacts
ta draft review next

# Add session-level notes
ta draft review note "Overall well-structured, auth needs rework"

# Finish and apply
ta draft review finish --approve "src/**" --reject "config.toml"
```

### Correcting a Draft

Three paths depending on the size of the fix:

**Direct amendment** -- for typos and small fixes you can make yourself:

```bash
ta draft amend <draft-id> src/main.rs --file corrected_main.rs
ta draft amend <draft-id> config.toml --drop
ta draft amend <draft-id> src/lib.rs --file fixed.rs --reason "Fixed typo in function name"
```

**Scoped agent fix** -- for logic changes that need agent help:

```bash
ta draft fix <draft-id> --guidance "Remove duplicate struct, reuse AlternativeConsidered"
ta draft fix <draft-id> src/draft.rs --guidance "Consolidate the duplicate"
```

**Full re-work** -- for architectural changes:

```bash
ta run "Rework auth to use JWT per review feedback" --source . --follow-up
```

### Draft Lifecycle Hygiene

```bash
# Close without applying (abandoned, hand-merged, obsolete)
ta draft close <draft-id>
ta draft close <draft-id> --reason "Hand-merged upstream"

# Find forgotten drafts
ta draft list --stale

# Clean up staging directories for old drafts
ta draft gc --dry-run       # Preview
ta draft gc                 # Remove
ta draft gc --archive       # Archive instead of delete
```

Configure thresholds:

```toml
# .ta/workflow.toml
[gc]
stale_threshold_days = 7
health_check = true          # One-line warning on startup if stale drafts exist
```

---

## Configuration

### Workflow Config (`.ta/workflow.toml`)

The central configuration file for TA behavior:

```toml
[submit]
adapter = "git"                    # "git" or "none"
auto_commit = true                 # Commit on ta draft apply
auto_push = true                   # Push after commit
auto_review = true                 # Open GitHub PR after push

[submit.git]
branch_prefix = "ta/"              # Branch naming: ta/goal-title
target_branch = "main"             # GitHub PR base branch
merge_strategy = "squash"          # squash | merge | rebase
pr_template = ".ta/pr-template.md" # GitHub PR body template

[follow_up]
default_mode = "extend"            # extend | standalone
auto_supersede = true
rebase_on_apply = true

[build]
summary_enforcement = "warning"    # ignore | warning | error

[gc]
stale_threshold_days = 7
health_check = true
```

Without this file, TA uses sensible defaults (no VCS operations, warning-level enforcement).

### Agent Configuration

TA searches for agent configs in priority order:
1. `.ta/agents/<agent>.yaml` -- project-specific
2. `~/.config/ta/agents/<agent>.yaml` -- user-wide
3. Built-in defaults (shipped with the binary)
4. Hard-coded fallback

Create a custom agent config:

```yaml
# .ta/agents/my-agent.yaml
name: my-agent
description: "My custom coding agent"
command: my-agent-cli
args_template:
  - "--mode"
  - "autonomous"
  - "{prompt}"
injects_context_file: false
injects_settings: false
env:
  MY_AGENT_LOG_LEVEL: "info"
interactive:
  enabled: true
  output_capture: pty
  allow_human_input: true
  auto_exit_on: "idle_timeout: 300s"
  resume_cmd: "my-agent-cli --resume {session_id}"
```

Config fields:

| Field | Type | Description |
|-------|------|-------------|
| `command` | string | Command to execute (must be on PATH) |
| `args_template` | string[] | Arguments; `{prompt}` replaced with goal text |
| `injects_context_file` | bool | Inject goal context into CLAUDE.md |
| `injects_settings` | bool | Inject `.claude/settings.local.json` with permissions |
| `pre_launch` | object | Command to run before agent launch |
| `env` | map | Environment variables for the agent process |
| `interactive` | object | Interactive session config (PTY capture, resume) |
| `alignment` | object | Alignment profile (see below) |

### Alignment Profiles

Alignment profiles declare what an agent can do, must escalate, and must never touch. TA compiles these into enforceable capability grants -- the agent cannot exceed them.

```yaml
# .ta/agents/claude-code.yaml
alignment:
  principal: "project-owner"
  autonomy_envelope:
    bounded_actions:
      - "fs_read"
      - "fs_write_patch"
      - "fs_apply"
      - "exec: cargo test"
      - "exec: cargo build"
    escalation_triggers:
      - "new_dependency"
      - "security_sensitive"
      - "breaking_change"
    forbidden_actions:
      - "network_external"
      - "credential_access"
  constitution: "default-v1"
  coordination:
    allowed_collaborators: ["codex", "claude-flow"]
    shared_resources: ["src/**", "tests/**"]
```

Common profiles:

| Profile | bounded_actions | forbidden_actions |
|---------|----------------|-------------------|
| Read-only auditor | `fs_read` | `fs_write_patch`, `fs_apply`, `network_external`, `credential_access` |
| Full developer (default) | `fs_read`, `fs_write_patch`, `fs_apply`, `exec: cargo test`, `exec: cargo build` | `network_external`, `credential_access` |

### Access Constitutions

Per-goal declarations of what URIs the agent should need. Deviations trigger warnings:

```bash
# Set a constitution for a goal
ta goal constitution set <goal-id> \
  --access "src/commands/draft.rs:Add constitution enforcement" \
  --access "crates/ta-policy/src/**:Constitution data model" \
  --enforcement warning

# Propose one based on historical patterns
ta goal constitution propose <goal-id>

# View or list
ta goal constitution view <goal-id>
ta goal constitution list
```

Constitution files live at `.ta/constitutions/goal-<id>.yaml`.

### Configurable Summary Exemption

`ta draft build` checks that every changed file has a summary. Some files (lockfiles, config manifests) do not need hand-written summaries. Customize with `.ta/summary-exempt`:

```
# .ta/summary-exempt (.gitignore-style patterns)
Cargo.lock
package-lock.json
Cargo.toml
package.json
**/*.generated.*
```

Without this file, TA uses built-in defaults. See `examples/summary-exempt`.

### Plan Schema (`.ta/plan-schema.yaml`)

Customize how TA parses your plan document:

```yaml
source: PLAN.md
phase_patterns:
  - regex: "^##+ (?:v?[\\d.]+[a-z]? -- |Phase \\d+ -- )(.+)"
    id_capture: "version_or_phase_number"
status_marker: "<!-- status: (\\w+) -->"
statuses: [done, in_progress, pending]
```

Generate automatically with `ta plan init`.

---

## Advanced Features

### Selective Approval

Approve, reject, or discuss individual files using glob patterns:

```bash
ta draft apply <draft-id> \
  --approve "src/**" \
  --reject "config.toml" \
  --discuss "README.md"

# Special values
ta draft apply <draft-id> --approve "all"
ta draft apply <draft-id> --approve "src/**" --reject "rest"
```

TA validates dependencies: if you approve file A that depends on rejected file B, you get a warning.

### Behavioral Drift Detection

Monitor agents for behavior that diverges from their historical patterns:

```bash
# Compute and store a baseline
ta audit baseline <agent-id>

# Check for drift
ta audit drift <agent-id>

# Drift summary across all agents
ta audit drift --all
```

Five drift signals are tracked:
- Resource scope drift (accessing unusual URIs)
- Escalation frequency change
- Rejection rate drift
- Change volume anomaly
- Dependency pattern shift

Baselines are stored in `.ta/baselines/<agent-id>.json`.

### Conflict Detection

If source files change while a goal is running:

```bash
ta draft apply <draft-id>
# WARNING: 3 conflict(s) detected

# Resolution strategies:
ta draft apply <draft-id> --conflict-resolution abort           # Default
ta draft apply <draft-id> --conflict-resolution force-overwrite # Dangerous
ta draft apply <draft-id> --conflict-resolution merge           # Git adapter
```

### External Diff Handlers

Configure how non-text files are reviewed. Create `.ta/diff-handlers.toml`:

```toml
[[handler]]
pattern = "*.png"
command = "open"
args = ["-a", "Preview", "{file}"]
description = "PNG image"

[[handler]]
pattern = "Content/**/*.uasset"
command = "UnrealEditor"
args = ["{file}"]
description = "Unreal asset"

[[handler]]
pattern = "models/**/*.blend"
command = "blender"
args = ["--background", "{file}", "--python", "scripts/preview.py"]
description = "Blender scene"
```

When you run `ta draft view <id> --file image.png`, it opens in the configured handler. Use `--no-open-external` to force inline display.

### Git Integration

```bash
# Apply and commit
ta draft apply <draft-id> --git-commit

# Full workflow: apply, commit, push, open GitHub PR
ta draft apply <draft-id> --submit
```

Configure in `.ta/workflow.toml`:

```toml
[submit]
adapter = "git"
auto_commit = true
auto_push = true

[submit.git]
branch_prefix = "ta/"
target_branch = "main"
```

### Release Pipeline

TA includes a YAML-driven release pipeline:

```bash
# Run the built-in release pipeline
ta release run 0.4.0-alpha

# Use plan phase IDs -- auto-converted to semver
ta release run v0.4.1.2           # becomes 0.4.1-alpha.2
ta release run 0.4                # becomes 0.4.0-alpha

# Preview without executing
ta release run 0.4.0-alpha --dry-run

# Show pipeline steps
ta release show

# Create a customizable .ta/release.yaml
ta release init
```

Pipeline steps can be shell commands or agent goals with optional approval gates:

```yaml
# .ta/release.yaml
steps:
  - name: Build & test
    run: |
      ./dev cargo build --workspace
      ./dev cargo test --workspace

  - name: Generate release notes
    agent:
      id: claude-code
      phase: "v0.4.0"
    objective: "Synthesize release notes for ${TAG}."
    output: .release-draft.md

  - name: Push to remote
    requires_approval: true
    run: git push origin main && git push origin ${TAG}
```

Variables available: `${VERSION}`, `${TAG}`, `${COMMITS}`, `${LAST_TAG}`.

The `version_policy` section controls how plan phase IDs are converted to semver. Templates use `{0}`..`{3}` for numeric segments and `{pre}` for the prerelease suffix:

```yaml
# .ta/release.yaml (customize version normalization)
version_policy:
  prerelease_suffix: "alpha"          # default suffix for bare versions
  two_segment: "{0}.{1}.0-{pre}"     # 0.4 -> 0.4.0-alpha
  three_segment: "{0}.{1}.{2}-{pre}" # 0.4.1 -> 0.4.1-alpha
  four_segment: "{0}.{1}.{2}-{pre}.{3}"  # 0.4.1.2 -> 0.4.1-alpha.2
```

Examples for other projects:
- **No prerelease**: set `three_segment: "{0}.{1}.{2}"` and `prerelease_suffix: ""`
- **Beta channel**: set `prerelease_suffix: "beta"`
- **RC numbering**: set `four_segment: "{0}.{1}.{2}-rc.{3}"`

### Versioning and Release Lifecycle

TA uses [semver](https://semver.org/): `MAJOR.MINOR.PATCH-prerelease`.

| Tag | Meaning |
|-----|---------|
| **alpha** | Active development. APIs may change. Not for production. |
| **beta** | Feature-complete for the cycle. APIs stabilizing. |
| **rc.N** | Release candidate. Bug fixes only. |
| *(none)* | Stable release. Semver guarantees apply. |

Plan phases map to release versions:

| Plan Phase | Release Version |
|------------|----------------|
| v0.4 | `0.4.0-alpha` |
| v0.4.1 | `0.4.1-alpha` |
| v0.4.1.2 | `0.4.1-alpha.2` |

All current releases are `alpha`. Beta begins at v0.8 (Event System). Stable `1.0.0` requires all v0.x features hardened, public API frozen, and security audit complete.

### Decision Observability

Every decision in the TA pipeline is observable:

```bash
# Decision trail for a goal
ta audit show <goal-id>

# Structured export for compliance reporting
ta audit export <goal-id> --format json

# Verify audit log integrity (hash chain)
ta audit verify

# Recent events
ta audit tail -n 20
```

Policy decisions capture which grants were checked and why. Agent decisions can include alternatives considered. Review decisions support structured reasoning with rationale.

### Audit Trail

TA maintains an append-only, SHA-256 hash-chained audit log of every action:
- Goal creation, state transitions
- Draft builds, approvals, rejections, amendments
- Policy evaluations with grant-level detail
- Human review decisions with reasoning
- Conflict detection and resolution

### Credential Management

TA manages credentials so agents never hold raw secrets. Agents request access; TA provides scoped, time-limited session tokens. This is the foundation for all external service integrations (MCP servers that need auth, API keys, OAuth tokens).

```bash
# Add a credential (secret is stored with 0600 file permissions)
ta credentials add --name "GitHub Token" --service github --secret "ghp_..."

# Add with scopes
ta credentials add --name "Gmail Read" --service gmail --secret "ya29.a0..." \
  --scope "read" --scope "labels"

# List credentials (secrets are never shown -- only name, service, scopes)
ta credentials list

# Revoke a credential
ta credentials revoke <credential-id>
```

Credentials are stored in `.ta/credentials.json`. The `FileVault` issues session tokens with configurable TTL:

```
Agent requests: "I need gmail read access for goal abc123"
TA issues:      SessionToken { ttl: 3600s, scopes: ["read"], agent: "claude-code" }
Agent uses:     The token (never the raw credential)
TA proxies:     Token → real credential on each API call
```

### Context Memory

Persistent, framework-agnostic memory that survives across sessions and works with any agent. When you switch from Claude Code to Codex mid-project, or run multiple agents in parallel, context doesn't get lost. TA owns the memory -- agents consume it.

#### Why this matters

Each agent framework has its own memory: Claude Code has CLAUDE.md and project memory, Codex has session state, Cursor has codebase indices. None of it transfers. TA's context memory is the *shared layer* that all agents read from and write to.

#### Basic operations

```bash
# Store a convention your team follows
ta context store "test-fixtures" \
  --value '{"rule": "Always use tempfile::tempdir() for filesystem tests"}' \
  --tag "convention" --tag "testing"

# Store structured project knowledge
ta context store "auth-architecture" \
  --value '{"approach": "JWT with RS256", "module": "src/auth/", "decided": "2026-01"}'

# Recall by exact key
ta context recall "test-fixtures"

# Semantic search (requires --features ruvector)
ta context recall "how do we handle authentication" --semantic
ta context recall "testing conventions" --semantic --limit 3

# List all entries
ta context list

# List with limit
ta context list --limit 10

# Remove an entry
ta context forget "auth-architecture"
```

#### New commands (v0.5.7)

```bash
# Semantic search (dedicated command)
ta context search "how to handle errors in this project"
ta context search "testing conventions" --limit 3

# Find entries similar to an existing entry (by UUID)
ta context similar a1b2c3d4-...

# Show full provenance for an entry (by key or UUID)
ta context explain "test-fixtures"

# Memory store statistics
ta context stats

# Store with TTL, confidence, and category
ta context store "temp-note" --value "remember this for 30 days" \
  --expires-in 30d --confidence 0.9 --category convention

# Filter list by category
ta context list --category architecture
```

#### Memory dashboard (v0.5.7)

When running `ta serve`, the web UI at `http://127.0.0.1:<port>` now includes a **Memory** tab alongside Drafts. The memory dashboard lets you:

- Browse all memory entries with category badges and confidence bars
- Search entries by key prefix
- Create new entries directly from the UI
- Delete entries
- View aggregate statistics (total entries, by category, by source, average confidence)

#### Semantic search with ruvector (optional)

Since v0.6.3, the ruvector backend is **enabled by default**, providing semantic search out of the box. `ta context search` and `ta context similar` find relevant entries by meaning rather than exact key match. Existing filesystem entries are auto-migrated on first use. The ruvector backend stores entries in `.ta/memory.rvf` using HNSW indexing for sub-millisecond recall.

To use the filesystem-only backend instead, set `backend = "fs"` in `.ta/memory.toml`.

#### How agents use memory

During macro goal sessions, agents access memory through the `ta_context` MCP tool:

```
Agent calls: ta_context { action: "recall", key: "test-fixtures" }
TA returns:  { "rule": "Always use tempfile::tempdir() for filesystem tests" }
```

This works identically for Claude Code, Codex, or any agent connected via MCP. The agent doesn't need to know which framework stored the entry.

The `ta_context` MCP tool also supports `source` and `category` parameters (v0.5.6):

```
Agent calls: ta_context {
  action: "store",
  key: "test-fixtures",
  value: {"rule": "Use tempfile::tempdir()"},
  tags: ["convention", "testing"],
  source: "claude-code",
  category: "convention"
}
```

And additional actions (v0.5.6+):

```
# Semantic search (requires ruvector backend)
Agent calls: ta_context { action: "search", query: "testing conventions", limit: 5 }

# Memory statistics (v0.5.7)
Agent calls: ta_context { action: "stats" }

# Find similar entries by ID (v0.5.7, requires ruvector)
Agent calls: ta_context { action: "similar", key: "entry-uuid-here", limit: 5 }
```

#### Automatic State Capture (v0.5.6)

TA can automatically capture knowledge from lifecycle events so agents don't repeat mistakes and new agents start with context from previous sessions:

```toml
# .ta/workflow.toml
[memory.auto_capture]
on_goal_complete = true        # Store "what worked" from approved drafts
on_draft_reject = true         # Store rejection reasons to prevent repeated mistakes
on_human_guidance = true       # Store human feedback as persistent knowledge
on_repeated_correction = true  # Auto-promote patterns corrected 3+ times
correction_threshold = 3       # How many repeats before auto-promotion
max_context_entries = 10       # Max entries injected into agent context
```

All settings default to `true` (enabled). Create `.ta/workflow.toml` to customize.

**What gets captured automatically:**

| Event | What's stored | Category |
|-------|--------------|----------|
| Goal completes | Title, changed files, change summary, module map | history, architecture |
| Draft rejected | What was attempted, rejection reason | negative_path |
| Human guidance | The guidance text and tags | preference |
| Repeated correction | Promoted to persistent preference | preference |

#### Context Injection on Launch

When `ta run` launches an agent, it queries the memory store and injects relevant entries into a "Prior Context" section in CLAUDE.md. This means every agent starts with knowledge from all previous sessions, regardless of which framework produced it.

**Phase-aware injection (v0.6.3)**: When a goal is linked to a plan phase (`--phase v0.6.3`), only entries matching that phase or global entries (no phase) are injected. Entries are grouped by category with priority ordering: Architecture > Negative Paths > Conventions > State > History.

**Semantic ranking**: With the ruvector backend (now default), injection uses semantic similarity to rank entries by relevance to the goal title.

#### Project-Aware Key Schema (v0.6.3)

Memory keys use `{domain}:{topic}` format with domains auto-detected from your project type:

| Project Type | Detected By | Module Map Key | Type System Key |
|---|---|---|---|
| Rust workspace | `Cargo.toml` with `[workspace]` | `arch:crate-map` | `arch:trait:*` |
| TypeScript | `package.json` + `tsconfig.json` | `arch:package-map` | `arch:interface:*` |
| Python | `pyproject.toml` | `arch:module-map` | `arch:protocol:*` |
| Go | `go.mod` | `arch:package-map` | `arch:interface:*` |
| Generic | fallback | `arch:component-map` | `arch:type:*` |

Inspect your project's key schema:

```bash
ta context schema
```

Override auto-detection via `.ta/memory.toml`:

```toml
[project]
type = "rust-workspace"

[key_domains]
module_map = "crate-map"
type_system = "trait"

backend = "ruvector"   # default; or "fs" for filesystem-only
```

#### Negative Paths (v0.6.3)

When a draft is rejected, TA stores it as a **negative path** entry (`negative_path` category) with a `neg:{phase}:{slug}` key. Future agents see these during context injection and avoid repeating the same mistakes.

#### What gets stored

| Category | Example | How it's captured |
|----------|---------|-------------------|
| **Conventions** | "Use 4-space indent", "Run clippy before commit" | Human guidance, repeated corrections, auto-capture |
| **Architecture** | "Auth is JWT-based, module at src/auth/" | Goal completion auto-capture (v0.6.3: module extraction), agent via MCP |
| **Negative Paths** | "Tried Redis caching, rejected -- too complex for MVP" | Draft rejection auto-capture (v0.6.3) |
| **State** | "Plan progress snapshot", "dependency graph" | Agent stores via MCP (v0.6.3) |
| **History** | "Goal completed: fixed auth bug" | Goal completion auto-capture |
| **Preferences** | "Human prefers small focused PRs" | Repeated correction auto-promotion |
| **Relationships** | "config.toml depends on src/config.rs" | Agent stores via MCP |

#### Storage details

Entries are JSON files in `.ta/memory/`, one per key. The filesystem backend is the zero-dependency default. For semantic search, enable the ruvector backend (v0.5.5+).

#### Configuration

```toml
# .ta/workflow.toml
[memory]
backend = "filesystem"    # "filesystem" (default) or "ruvector" (v0.5.5+)
```

### Web Review UI

Review drafts from a browser instead of the terminal. Useful for non-CLI users, team reviews, or when you want a visual overview.

```bash
# Start the daemon with web UI on port 7676
ta-daemon --project-root . --web-port 7676
```

Open `http://127.0.0.1:7676` to see:
- **Draft list** -- all drafts with status badges (Draft, Pending, Approved, Denied), timestamps, and artifact counts
- **Draft detail** -- click a draft to see its artifacts, pending actions (intercepted MCP tool calls), and summary
- **Approve/Deny** -- one-click buttons with optional denial reason

The web UI reads from the same `.ta/pr_packages/` directory as the CLI. Approving a draft in the browser is reflected in `ta draft list` and vice versa.

You can also set the port in your gateway config so it starts automatically:

```toml
# .ta/workflow.toml or gateway config
[gateway]
web_ui_port = 7676
```

### Webhook Review Channel

Route draft review interactions to external systems via a file-based webhook exchange. This enables CI bots, Slack integrations, or any external process to participate in the review workflow.

#### Setup

```toml
# .ta/workflow.toml
[review_channel]
channel_type = "webhook"

[review_channel.channel_config]
endpoint = "/tmp/ta-reviews"   # directory for file exchange
```

#### How it works

1. TA writes `request-{id}.json` to the endpoint directory with the full `InteractionRequest`
2. Your external process reads it, makes a decision, writes `response-{id}.json`
3. TA polls for the response (default: every 2s, timeout: 1 hour)

#### Response format

```json
{
  "decision": "approve",
  "reasoning": "All tests pass, no security issues found",
  "responder_id": "ci-bot-v2"
}
```

Valid `decision` values:
- `approve` / `approved` -- accept the draft
- `reject` / `rejected` / `deny` / `denied` -- reject (include `reasoning`)
- `discuss` -- request more information

#### Available channel types

| Channel | Status | Description |
|---------|--------|-------------|
| `terminal` | Default | Interactive terminal prompts |
| `auto-approve` | Available | Auto-approves everything (for CI/batch) |
| `webhook` | Available | File-based exchange for external integrations |
| `slack` | Stub | Future: Slack Block Kit cards with button callbacks |
| `email` | Stub | Future: SMTP send with IMAP reply parsing |

### MCP Tool Call Interception

When agents call MCP tools, TA classifies each call and decides whether to pass it through immediately or capture it for human review.

#### Classification rules

| Classification | Tools | What happens |
|---------------|-------|-------------|
| **Passthrough** (read-only) | `ta_fs_read`, `ta_goal_status`, `ta_goal_list`, `ta_fs_list`, `ta_fs_diff`, `ta_pr_status` | Executes immediately, no review needed |
| **Captured** (state-changing) | `ta_fs_write`, all external/unknown tools | Added to draft as a `PendingAction` for review |

Tools with names matching read patterns (`read`, `get`, `list`, `search`, `status`, `diff`, `query`, `fetch`, `describe`) are classified as passthrough. Everything else is captured.

#### What you see in review

```bash
ta draft view <draft-id>
# ...
# Pending Actions (2):
#   1. gmail_send [state_changing] — Send email via Gmail MCP
#   2. slack_post [state_changing] — Post message to Slack channel
#
# These actions will execute when the draft is applied.
```

Pending actions appear alongside file artifacts in the draft. You can approve the file changes but reject the external actions, or vice versa.

### Claude Flow Optimization

When using Claude Flow as your agent:

```json
{
  "claudeFlow": {
    "modelPreferences": {
      "default": "claude-opus-4-6",
      "routing": "claude-haiku-4-5-20251001"
    },
    "swarm": {
      "topology": "hierarchical-mesh",
      "maxAgents": 15
    },
    "memory": {
      "backend": "hybrid",
      "enableHNSW": true
    }
  }
}
```

See `examples/claude-settings.json` for a complete optimized configuration.

### Session Lifecycle (v0.6.0)

TA sessions track the full conversation lifecycle for a goal, including review iterations. Each session records what the agent was told, what it produced, and how the human responded.

```bash
# View active sessions with state, iteration count, and elapsed time
ta session status

# Pause a running session
ta session pause <session-id>

# Resume a paused session
ta session resume <session-id>

# Abort a session
ta session abort <session-id> --reason "No longer needed"

# List all sessions (including completed/aborted)
ta session list --all

# Show session details and conversation history
ta session show <session-id>
```

Session states: `Starting` → `AgentRunning` → `DraftReady` → `WaitingForReview` → `Completed` (or `Iterating` → back to `AgentRunning` on rejection, or `Paused`/`Aborted`/`Failed`).

Sessions are stored in `.ta/sessions/<session-id>.json` and emit events (`SessionPaused`, `SessionResumed`, `SessionAborted`, `DraftBuilt`, `ReviewDecision`, `SessionIteration`) to the event stream.

### Unified Policy Config (v0.6.1)

All supervision configuration resolves to a single `PolicyDocument` loaded from `.ta/policy.yaml`. Configuration is merged from 6 layers, where each layer can tighten but never loosen restrictions.

```yaml
# .ta/policy.yaml
security_level: checkpoint   # open | checkpoint | supervised | strict

defaults:
  enforcement: warning        # warning | error | strict
  auto_approve:
    read_only: true
    internal_tools: true

schemes:
  fs:
    approval_required: [apply, delete]
  email:
    approval_required: [send]
    credential_required: true
    max_actions_per_session: 50

escalation:
  drift_threshold: 0.7
  action_count_limit: 200
  patterns:
    - new_dependency
    - security_sensitive

agents:
  claude-code:
    additional_approval_required: [network_external]
    forbidden_actions: [credential_access]

budget:
  max_tokens_per_goal: 1000000
  warn_at_percent: 80
```

**Merge cascade** (each layer tightens, never loosens):
1. Built-in defaults (Checkpoint level, auto-approve read-only)
2. `.ta/policy.yaml` (project config)
3. `.ta/workflows/<name>.yaml` (workflow overrides)
4. `.ta/agents/<agent>.policy.yaml` (agent-specific)
5. `.ta/constitutions/goal-<id>.yaml` (goal constitution)
6. CLI overrides (`--strict`, `--auto-approve=false`)

**Security levels**:
- **Open**: Audit-only, no approvals required
- **Checkpoint** (default): Review at draft submission
- **Supervised**: Approve each state-changing action
- **Strict**: Constitutions required for all goals

### Resource Mediation (v0.6.2)

The `ResourceMediator` trait generalizes TA's staging pattern from files to any resource type. Each mediator handles a URI scheme (`fs://`, `email://`, `db://`, etc.) and provides stage → preview → apply → rollback operations.

Built-in mediators:
- **FsMediator** (`fs://`): File system staging (wraps existing staging workspace)

The `MediatorRegistry` routes actions to the correct mediator by URI scheme. Future mediators (API, database, email) implement the same trait.

---

## Roadmap

### What's Done

TA has a working end-to-end workflow: staging isolation, agent wrapping, draft review with per-artifact approval, follow-up iterations, macro goals with inner-loop review, interactive sessions, plan tracking, release pipelines, behavioral drift detection, access constitutions, alignment profiles, decision observability, credential management, MCP tool call interception, web review UI, webhook review channels, persistent context memory with semantic search, session lifecycle management, unified policy configuration (6-layer cascade), and resource mediation (extensible by URI scheme).

### Phase Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 0 | Repo layout and core data model | Done |
| Phase 1 | Kernel: audit, policy, changeset, workspace | Done |
| Phase 2 | MCP gateway, goal lifecycle, CLI | Done |
| Phase 3 | Transparent overlay mediation | Done |
| Phase 4a | Agent prompt enhancement | Done |
| Phase 4a.1 | Plan tracking and lifecycle | Done |
| Phase 4b | Per-artifact review model | Done |
| Phase 4c | Selective review CLI | Done |
| v0.1 | Public preview and call for feedback | Pending |
| v0.1.1 | Release automation and binary distribution | In Progress |
| v0.1.2 | Follow-up goals and iterative review | Done |
| v0.2.0 | SubmitAdapter trait and git implementation | Done |
| v0.2.1 | Concurrent session conflict detection | Done |
| v0.2.2 | External diff routing | Done |
| v0.2.3 | Tiered diff explanations and output adapters | Done |
| v0.2.4 | Terminology and positioning pass | Done |
| v0.3.0 | Review sessions | Done |
| v0.3.0.1 | Consolidate pr.rs into draft.rs | Done |
| v0.3.1 | Plan lifecycle automation | Done |
| v0.3.1.1 | Configurable plan format parsing | Done |
| v0.3.1.2 | Interactive session orchestration | Done |
| v0.3.2 | Configurable release pipeline | Done |
| v0.3.3 | Decision observability and reasoning capture | Done |
| v0.3.4 | Draft amendment and targeted re-work | Done |
| v0.3.5 | Release pipeline fixes | Done |
| v0.3.6 | Draft lifecycle hygiene | Done |
| v0.4.0 | Alignment profiles and policy compiler | Done |
| v0.4.1 | Macro goals and inner-loop iteration | Done |
| v0.4.1.1 | Runtime channel architecture | Done |
| v0.4.1.2 | Follow-up draft continuity | Done |
| v0.4.2 | Behavioral drift detection | Done |
| v0.4.3 | Access constitutions | Done |
| v0.4.4 | Interactive session completion (PTY) | Done |
| v0.4.5 | CLI UX polish | Done |
| v0.5.0 | Credential broker and identity abstraction | Done |
| v0.5.1 | MCP tool call interception | Done |
| v0.5.2 | Minimal web review UI | Done |
| v0.5.3 | ReviewChannel adapters (webhook) | Done |
| v0.5.4 | Context memory store | Done |
| v0.5.5 | RuVector memory backend (semantic search, HNSW indexing) | Done |
| v0.5.6 | Framework-agnostic agent state (auto-capture, context injection) | Done |
| v0.5.7 | Semantic memory queries and memory dashboard | Done |
| v0.6.0 | Session & human control plane | Done |
| v0.6.1 | Unified policy config | Done |
| v0.6.2 | Resource mediation trait | Done |
| v0.6.3 | Active memory injection (project-aware keys, phase tagging, smart context) | Pending |

### v0.6 -- Platform Substrate

| Phase | Description | Status |
|-------|-------------|--------|
| v0.6.0 | Session & human control plane (TaSession, SessionManager, CLI commands) | Done |
| v0.6.1 | Unified policy config (PolicyDocument, PolicyCascade, PolicyContext) | Done |
| v0.6.2 | Resource mediation trait (ResourceMediator, FsMediator, MediatorRegistry) | Done |
| v0.6.3 | Active memory injection (project-aware keys, smart context injection) | Pending |

### What's Next (v0.7+)

| Phase | Description | Status |
|-------|-------------|--------|
| v0.7.0 | Channel registry (pluggable IO channels) | Pending |
| v0.7.1 | API mediator (MCP tool call staging) | Pending |
| v0.7.2 | Agent-guided setup (`ta setup`) | Pending |
| v0.8.0 | Event system and subscription API | Pending |
| v0.8.1 | Community memory (shared knowledge across instances) | Pending |
| v0.9.0 | Distribution and packaging (desktop, cloud, web UI) | Pending |
| v0.9.1 | Native Windows support | Pending |
| v0.9.2 | Sandbox runner (optional kernel-level isolation) | Pending |

See [PLAN.md](../PLAN.md) for full details on each phase.

---

## Troubleshooting

### Agent cannot access files

**Cause**: Exclude patterns (`.taignore`) blocking access.

**Fix**: Check your exclude patterns:
```bash
cat .taignore
```

The default excludes `target/`, `node_modules/`, `.git/`, and similar build artifacts. Add or remove patterns as needed.

### External handler does not open

**Cause**: Command not found or misconfigured path.

**Fix**:
```bash
# Test the command directly
blender /path/to/file.blend

# Fallback to inline diff
ta draft view <draft-id> --file test.blend --no-open-external
```

### Selective approval fails with dependency errors

**Cause**: An approved file depends on a rejected file.

**Fix**: View dependencies and approve coupled changes together:
```bash
ta draft view <draft-id>
ta draft apply <draft-id> --approve "src/main.rs" --approve "src/lib.rs"
```

### Conflicts on apply

**Cause**: Source files changed since the goal started.

**Fix**:
```bash
# Start a fresh goal (safest)
ta draft apply <draft-id> --conflict-resolution abort
ta run "Redo the task" --source .

# Force overwrite (use with caution)
ta draft apply <draft-id> --conflict-resolution force-overwrite

# Git merge (if git adapter configured)
ta draft apply <draft-id> --conflict-resolution merge
```

### Agent does not pause for review in macro mode

**Cause**: Missing `--macro` flag. Without it, the agent has no MCP tools and exits after one pass.

### Session shows "Aborted"

**Cause**: Agent process crashed or was killed.

**Fix**: Check the session log:
```bash
ta session show <session-id>
```

### Garbled characters in terminal output

**Cause**: HTML tags leaking into terminal rendering.

**Fix**: Update to the latest version. This was fixed in v0.3.1.1 with the `strip_html()` sanitizer.

---

## Getting Help

- **Source and documentation**: [github.com/trustedautonomy/ta](https://github.com/trustedautonomy/ta)
- **Report bugs**: [GitHub Issues](https://github.com/trustedautonomy/ta/issues)
- **Development roadmap**: [PLAN.md](../PLAN.md)
- **Architecture overview**: [docs/ARCHITECTURE.md](ARCHITECTURE.md)
