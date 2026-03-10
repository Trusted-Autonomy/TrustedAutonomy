# Trusted Autonomy -- User Guide

**Version**: v0.10.10-alpha

Trusted Autonomy (TA) is a governance wrapper for AI agents. It lets any agent work freely in an isolated workspace, then holds the proposed changes at a human review checkpoint before anything takes effect. You see what the agent wants to do, approve or reject each change, and maintain a complete audit trail.

---

## Table of Contents

1. [Quick Start](#quick-start)
   - [Install](#install)
   - [Set up your project](#set-up-your-project)
   - [Start a development session](#start-a-development-session)
   - [Your first goal](#your-first-goal)
   - [Quick Start with ta shell](#quick-start-with-ta-shell)
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
   - [Channel Setup](#channel-setup)
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
   - [Daemon API](#daemon-api)
   - [Interactive Shell](#interactive-shell)
   - [Webhook Review Channel](#webhook-review-channel)
   - [Discord Channel Plugin](#discord-channel-plugin)
   - [Slack Channel Plugin](#slack-channel-plugin)
   - [Email Channel Plugin](#email-channel-plugin)
   - [MCP Tool Call Interception](#mcp-tool-call-interception)
   - [Session Lifecycle](#session-lifecycle)
   - [Unified Policy Config](#unified-policy-config)
   - [Unified Access Control Pattern](#unified-access-control-pattern)
   - [Resource Mediation](#resource-mediation)
   - [Channel Registry](#channel-registry)
   - [Multi-Channel Routing](#multi-channel-routing)
   - [Channel Plugins](#channel-plugins)
   - [Inspecting Channel Configuration](#inspecting-channel-configuration)
   - [API Mediation](#api-mediation)
   - [Project Setup](#project-setup)
   - [Project Initialization](#project-initialization)
   - [Add TA to an Existing Project](#add-ta-to-an-existing-project)
   - [Framework Registry](#framework-registry)
   - [Workflow Engine](#workflow-engine)
6. [Roadmap](#roadmap)
7. [Troubleshooting](#troubleshooting)
8. [Getting Help](#getting-help)

---

## Quick Start

### Install

**Option A -- One-line installer (macOS / Linux)**

```bash
curl -fsSL https://raw.githubusercontent.com/trustedautonomy/ta/main/install.sh | bash
```

Set a specific version: `TA_VERSION=v0.9.3-alpha curl -fsSL ... | bash`

**Option B -- Binary download**

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

# Windows (x86_64)
# Download ta-x86_64-pc-windows-msvc.zip from the latest release
# Extract and add ta.exe to your PATH
```

**Option C -- Docker**

```bash
docker pull ghcr.io/trustedautonomy/ta:latest
docker run -it -v $(pwd):/workspace ta --help
```

**Option D -- Cargo install**

```bash
cargo install ta-cli
```

**Option E -- Nix**

```bash
nix run github:trustedautonomy/ta
```

**Option F -- Build from source**

```bash
git clone https://github.com/trustedautonomy/ta.git
cd ta
./dev cargo build --workspace --release
# Binary is at target/release/ta
```

### Set up your project

**New project** -- generate TA config from a template:

```bash
mkdir my-project && cd my-project
git init
ta init run --template rust-workspace   # or: typescript-monorepo, python-ml, go-service, generic
```

This creates `.ta/` with workflow config, agent configs, policy, memory settings, and a `.taignore`. Everything is generated as a reviewable draft.

**Existing project** -- auto-detect what's in use:

```bash
cd my-existing-project
ta init --detect
```

TA scans your project root (Cargo.toml, package.json, pyproject.toml, go.mod, etc.) and generates config matched to your toolchain, test runner, and build system.

**See what was detected:**

```bash
ta setup show                    # display resolved config
ta setup refine agents           # tweak agent config
ta setup refine workflow         # adjust workflow settings
```

**Available templates:**

```bash
ta init templates                # list all built-in templates
```

### Start a development session

There are three ways to work with TA, depending on how much control you want:

**Option A -- Interactive shell** (recommended for getting started):

Use the convenience script (starts the daemon automatically):

```bash
./scripts/ta-shell.sh           # macOS / Linux
.\scripts\ta-shell.ps1          # Windows PowerShell
```

Or start manually:

```bash
ta-daemon --project-root . &    # Start the daemon in the background
ta shell                        # Open the interactive shell
```

The shell gives you a single terminal with commands, agent conversation, and live event notifications. Type `ta` commands directly, use shortcuts like `approve` and `status`, or ask the agent natural-language questions:

```
ta> status                            # Project overview
ta> drafts                            # List pending drafts
ta> What should we work on next?      # Ask the agent (if attached)
ta> approve abc123                    # Approve a draft
```

See [Interactive Shell](#interactive-shell) for the full guide.

**Option B -- Developer loop** (autonomous agent-driven):

```bash
ta dev
```

`ta dev` gives the agent the terminal -- it reads your plan, suggests what to do next, and handles the goal/draft/review/apply loop. You review elsewhere. Best for autonomous work on planned phases.

From inside the session you can say things like:
- "what's next" -- shows the next pending plan phase
- "run that" -- kicks off the goal with the right agent and phase
- "status" -- plan progress summary
- "release" -- cut a release

**Option C -- Manual commands** (full control):

If you prefer manual control, use individual commands instead (see [Common Workflows](#common-workflows)).

**When to use which:**

| Mode | Who drives? | Best for |
|------|------------|----------|
| `ta shell` | Human drives, agent assists | Exploring, managing drafts, ad-hoc questions, multi-agent oversight |
| `ta dev` | Agent drives, human reviews | Planned phase execution, autonomous development sessions |
| Manual CLI | Human drives everything | Scripting, CI, one-off commands |

### Your first goal

The manual equivalent of `ta dev` -- three commands:

```bash
# 1. Run a goal -- TA copies your project to an isolated workspace,
#    launches the agent, and captures all changes as a draft.
ta run "Add a README badge for build status"

# 2. Review the draft -- see what changed and why.
ta draft view <draft-id>

# 3. Approve and apply -- changes land in your working directory.
ta draft approve <draft-id>
ta draft apply <draft-id>
```

**What just happened:**

1. **Staging**: TA copied your project into `.ta/staging/`. The agent worked there, not in your real files.
2. **Draft**: TA diffed the workspace against your source and packaged the changes into a reviewable draft.
3. **Review**: You saw every changed file with a summary of what changed and why.
4. **Apply**: Approved changes were copied back into your project.

The agent never touched your real files. Reject the draft and nothing changes.

### Quick Start with ta shell

If you prefer a persistent interactive session over one-off commands, use the shell. This walkthrough takes you from zero to a working project:

```bash
# 1. Set up the project (new or existing)
cd my-project
ta init --detect                        # Auto-detect toolchain, generate config

# 2. Start daemon + open shell (one command)
./scripts/ta-shell.sh                   # Starts daemon if needed, opens shell

# Or do it manually:
# ta-daemon --project-root . &          # Background daemon on port 7700
# ta shell                              # Open the shell
```

Now you're in the interactive shell. Everything happens from here:

```
ta> status                              # See project overview
ta> plan                                # Check plan phases
ta> ta run "Add CI workflow" # Start a goal
ta> drafts                              # Wait, then list drafts
ta> view abc123                         # Review the draft
ta> approve abc123                      # Approve it
ta> ta draft apply abc123               # Apply to your project
ta> :status                             # Refresh status header
ta> exit                                # Done for now
```

The shell remembers your command history across sessions. Reconnect any time with `ta shell` -- the daemon keeps running.

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

#### Draft View Output

`ta draft view` organizes its output into structured sections for clear review:

- **Summary** — high-level what changed, why, and impact
- **What Changed** — module-grouped file list with change icons (+/~/−), one-line descriptions, and dependency annotations
- **Design Decisions** — alternatives the agent considered, with `[chosen]`/`[considered]` markers and rationale
- **Artifacts** — detailed per-file view with explanations (at `--detail medium` or `--detail full`)

```bash
# Default view (medium detail)
ta draft view <id>

# Summary only (grouped file list, no detailed artifacts)
ta draft view <id> --detail top

# Full diffs included
ta draft view <id> --detail full

# Machine-readable JSON output
ta draft view <id> --json
```

Agents can populate the Design Decisions section by passing an `alternatives` array to the `ta_pr_build` MCP tool. Each entry has `option`, `rationale`, and `chosen` fields.

### Agents

TA wraps any agent framework. Out of the box it supports:
- **Claude Code** (default) -- Anthropic's coding agent
- **Codex** -- OpenAI's coding agent
- **Claude Flow** -- multi-agent orchestration

Use `--agent` to select:

```bash
ta run "Fix the bug" --agent codex
```

You can add any agent by creating a YAML config file (see [Agent Configuration](#agent-configuration)).

---

## Common Workflows

### Single Task

The most basic workflow: one goal, one review, one apply.

```bash
ta run "Refactor the auth module"
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

Fix issues discovered during review without losing context. The smart follow-up system scans your goals, drafts, plan phases, and verification failures to find what you want to resume — no need to remember branch names, draft IDs, or internal state.

```bash
# Interactive picker — shows all actionable follow-up candidates
ta run --follow-up

# Follow up on a specific draft (denied, failed verify, etc.)
ta run --follow-up-draft <draft-id-prefix>

# Follow up on a specific goal
ta run --follow-up-goal <goal-id-prefix>

# Resume work on a specific plan phase
ta run --follow-up --phase v0.10.9

# With a specific parent goal (legacy shortcut, still works)
ta run "Address review feedback" --follow-up <goal-id-prefix>

# With detailed instructions
ta run --follow-up --objective "Fix the discuss items on config.toml -- add env var override support"
```

When you run `ta run --follow-up` with no arguments, TA presents an interactive picker:

```
Follow-up candidates:

   1) [goal] v0.10.8 — Pre-Draft Verification Gate — failed: build error (2h ago)
      Added VerifyConfig struct with block/warn/agent failure modes
   2) [draft] v0.10.7 — Documentation Review — denied: needs more examples (1d ago)
      Updated USAGE.md with new command documentation
   3) [verify] v0.10.6 — Release Process — verify warnings (2) (3d ago)
      Added release workflow template

Select candidate [1-3] (or 'q' to cancel):
```

The agent receives rich follow-up context injected into CLAUDE.md — including what was attempted previously, verification failures with command output, denial reasons, and reviewer discussion comments.

When the parent goal's staging directory still exists, TA prompts to reuse it. Choosing yes (the default) means work accumulates into a single unified draft.

```toml
# .ta/workflow.toml -- follow-up behavior
[follow_up]
default_mode = "extend"       # "extend" (reuse staging) or "standalone" (fresh copy)
auto_supersede = true          # auto-supersede parent draft when extending
```

### Pre-Draft Verification

Run build/lint/test checks automatically after the agent exits but before the draft is created. If any check fails, the draft is blocked — no broken code reaches review.

```toml
# .ta/workflow.toml
[verify]
commands = [
    "cargo build --workspace",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
]
on_failure = "block"   # "block" (no draft), "warn" (draft with warnings)
timeout = 300          # seconds per command
```

When a command fails in block mode, TA prints the failed command and output, then suggests next steps:

```bash
# Re-enter the agent to fix issues
ta run --follow-up

# Re-run verification manually
ta verify <goal-id-prefix>

# Skip verification (use sparingly)
ta run --skip-verify
```

In warn mode (`on_failure = "warn"`), the draft is created but carries verification warnings visible in `ta draft view`.

`ta init` generates a pre-populated `[verify]` section for Rust projects. Other project types get commented-out examples.

### Macro Goals (multi-draft sessions)

For complex tasks that span multiple logical units of change, use `--macro`. The agent stays in a single long-running session and can submit multiple drafts for review without exiting.

```bash
ta run "Build the v0.7 features" --macro
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
ta run "Implement channel registry" --interactive
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
ta session close <session-id>      # Close cleanly (auto-builds draft if changes exist)
ta session abort <session-id>      # Cancel
```

**When to use**: When you want visibility into the agent's process — watching it work, steering it when it goes off track, or learning how it approaches a problem.

#### Agent Questions (`ta_ask_human`)

When running with `--interactive`, agents can ask you questions using the `ta_ask_human` MCP tool. Questions appear inline in `ta shell` or the classic terminal:

```
━━━ Agent Question (turn 1) ━━━
Which database should I use for the backend?
  Context: Setting up the storage layer for user data.
  [1] PostgreSQL
  [2] SQLite
Type your response and press Enter:
[agent Q1] > 1
Response delivered to agent (interaction: abc12345)
```

The prompt changes to `[agent Q1] >` while a question is pending. Your answer is routed to the agent via the daemon API, and the agent continues with your guidance.

#### Viewing Conversation History

Use `ta conversation` to review the interactive Q&A history for any goal:

```bash
ta conversation <goal-id>          # Formatted output with turns, roles, timestamps
ta conversation <goal-id> --json   # Raw JSONL for programmatic access
```

### Macro vs Interactive: when to use which

These are **different concerns** and can be combined:

| Flag | What it controls | Adds |
|------|-----------------|------|
| `--macro` | **Review loop** — agent can submit multiple drafts mid-session | MCP tools for draft/plan/sub-goal management |
| `--interactive` | **I/O mode** — real-time PTY streaming + human input | PTY capture, stdin interleaving, session persistence |
| `--headless` | **Non-interactive** — piped stdout, no PTY, structured JSON result | Orchestrator-driven execution |

**Decision guide**:

| Scenario | Recommended flags |
|----------|------------------|
| Simple single-file fix | *(neither)* — default mode, one draft on exit |
| Complex feature (multiple files, needs incremental review) | `--macro` |
| Unfamiliar codebase (want to watch and steer) | `--interactive` |
| Large multi-phase implementation with oversight | `--macro --interactive` |
| CI/batch automation | `--headless` — or `--macro` with `auto-approve` channel |
| Orchestrator-launched sub-goal | `--headless` — returns draft ID for automated processing |

**The full experience** — both flags together:

```bash
ta run "Build the v0.7 features" --macro --interactive --phase v0.7.0
```

You see the agent working in real-time, can inject guidance, and review each logical unit of change as it's submitted. This is the recommended mode for implementing plan phases.

### Interactive Developer Loop (`ta dev`)

`ta dev` launches an orchestration agent that coordinates the entire development loop from a single persistent session. Unlike `ta run`, the dev agent does NOT write code — it reads the plan, suggests goals, launches implementation agents, reviews drafts, and manages releases.

```bash
# Start the dev loop (uses built-in dev-loop agent config)
ta dev

# Use a custom agent for orchestration
ta dev --agent codex

# Bypass security restrictions (full access — use with caution)
ta dev --unrestricted
```

On launch, `ta dev` prints the current plan status directly to your terminal — you see progress and the next actionable phase before the agent even starts. Deferred phases (like public preview milestones) are automatically skipped.

**Security model:** By default, the orchestrator agent runs in **restricted mode** — it can only read project files and use TA MCP tools. No file writes, no shell access, no outbound mutations. The `--allowedTools` flag limits the agent to `Read`, `Grep`, `Glob`, `WebFetch`, `WebSearch`, and TA tools (`ta_plan`, `ta_goal`, `ta_draft`, `ta_context`, `ta_release`, `ta_event_subscribe`). The MCP gateway enforces `CallerMode::Orchestrator` which blocks `ta_fs_write` at the server level.

Use `--unrestricted` if you need the orchestrator to have full access (logs a warning and removes tool restrictions).

All dev sessions are audit-logged to `.ta/dev-audit.log` with session ID, timestamps, and mode.

The dev agent automatically:
- Reads PLAN.md and shows progress on startup
- Injects project memory context (architecture, conventions, negative paths)
- Highlights the next actionable phase (skips deferred phases)
- Lists any pending drafts awaiting review

You interact with it using natural language:
- "what's next" — show next pending phase
- "run v0.7.6" — launch a sub-goal for that phase
- "show drafts" — list drafts pending review
- "approve abc123" — approve a draft
- "check events" — query recent lifecycle events
- "release" — run the release pipeline
- "context search X" — search project memory

When the orchestrator launches a goal via the MCP `ta_goal_start` tool, TA spawns `ta run --headless` as a background process. This performs the full lifecycle: overlay workspace copy, CLAUDE.md injection, agent spawn, draft build on exit, and event emission. The orchestrator can then poll for completion using `ta_event_subscribe`.

The dev-loop agent config lives at `agents/dev-loop.yaml` and can be overridden per-project (`.ta/agents/dev-loop.yaml`) or per-user (`~/.config/ta/agents/dev-loop.yaml`).

### Plan-Linked Goals

Link goals to `PLAN.md` phases for automatic tracking:

```bash
ta run "Complete Phase v0.4.5" --phase v0.4.5

# When applied, PLAN.md is auto-updated to mark the phase done
```

Plan commands:

```bash
ta plan list                         # List all phases with status
ta plan status                       # Progress summary
ta plan status --json                # Machine-readable progress (includes deferred count)
ta plan next                         # Next pending phase with suggested command
ta plan validate v0.3.1              # Phase details, linked goals, draft summaries
ta plan history                      # Status transition history
ta plan mark-done v0.8.0,v0.8.1     # Batch-mark multiple phases as done
ta plan init                         # Extract plan-schema.yaml from existing plan
ta plan create                       # Generate new plan from template
ta plan create --template feature    # Feature template
ta plan from docs/PRD.md             # Generate plan from a product document (interactive)
```

#### Generating a Plan from a Document

Use `ta plan from <path>` to generate a phased development plan from a product document (PRD, spec, RFC, design doc, etc.):

```bash
ta plan from docs/PRD.md
ta plan from ~/specs/feature-design.md --agent claude-code
ta plan from requirements.txt --source ./my-project
```

The agent reads the document, asks clarifying questions interactively, and writes a `PLAN.md` in the staging workspace. The result goes through the standard draft review flow — you review, approve, and apply it just like any other TA draft.

**When to use which command:**

| Command | Use when | AI-powered? |
|---|---|---|
| `ta init --detect` | Scaffolding a `.ta/` config for an existing project | No |
| `ta plan create` | Starting from a generic template (greenfield/feature/bugfix) | No |
| `ta plan from <doc>` | You have a product document and want a tailored plan | Yes (interactive) |

#### Deferred Phases

Mark phases as `deferred` in PLAN.md when they're legitimate work items but shouldn't block current development:

```markdown
## Phase v0.1 — Public Preview
<!-- status: deferred -->
```

Deferred phases are:
- Shown with `[-]` in plan checklists
- Skipped when finding the "next pending" phase
- Included in status counts (`ta plan status --json`)
- Not candidates for `ta plan next` suggestions

#### Batch Phase Marking

When a single draft covers multiple plan phases:

```bash
# Mark multiple phases done at once
ta plan mark-done v0.8.0,v0.8.1

# Override the goal's phase on apply
ta draft apply <id> --git-commit --phase v0.8.0,v0.8.1
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
ta run "Rework auth to use JWT per review feedback" --follow-up
```

### Draft Lifecycle Hygiene

```bash
# Close without applying (abandoned, hand-merged, obsolete)
ta draft close <draft-id>
ta draft close <draft-id> --reason "Hand-merged upstream"

# Find forgotten drafts
ta draft list --stale

# Clean up staging directories for old drafts (also removes orphaned package files)
ta draft gc --dry-run       # Preview
ta draft gc                 # Remove
ta draft gc --archive       # Archive instead of delete

# Clean up zombie goals (stuck in running, missing staging)
ta goal gc --dry-run                  # Preview what would be cleaned
ta goal gc                            # Transition zombie goals to failed
ta goal gc --include-staging          # Also delete staging dirs for terminal goals
ta goal gc --threshold-days 3         # Custom stale threshold (default: 7 days)
```

Configure thresholds:

```toml
# .ta/workflow.toml
[gc]
stale_threshold_days = 7
health_check = true          # One-line warning on startup if stale drafts exist
```

### Unified Garbage Collection (`ta gc`)

Run all cleanup tasks in a single pass — zombie goals, stale staging directories, and orphaned draft packages:

```bash
# Preview what would be cleaned
ta gc --dry-run

# Clean everything older than 7 days (default threshold)
ta gc

# Clean everything older than 3 days
ta gc --threshold-days 3

# Clean all terminal goals regardless of age
ta gc --all

# Archive staging dirs instead of deleting
ta gc --archive
```

`ta gc` performs:
- **Zombie detection**: running/pr_ready goals past the stale threshold → transitioned to failed
- **Missing staging detection**: non-terminal goals whose staging directory no longer exists → marked failed
- **Staging cleanup**: terminal goals past threshold → staging directories removed (or archived with `--archive`)
- **Orphaned draft cleanup**: draft package JSON files whose goal no longer exists → removed
- **History ledger writes**: every GC'd goal gets a compact summary appended to `.ta/goal-history.jsonl`

### Goal History

Browse archived and completed goals, even after their JSON files have been GC'd:

```bash
# Show recent history (last 20 entries)
ta goal history

# Filter by plan phase
ta goal history --phase v0.9.8.1

# Filter by agent
ta goal history --agent claude-code

# Filter by date
ta goal history --since 2026-03-01

# Raw JSONL output for scripting
ta goal history --json

# Limit results
ta goal history --limit 50
```

### Goal List Filtering

By default, `ta goal list` shows only active (non-terminal) goals:

```bash
# Active goals only (default)
ta goal list

# All goals including completed/failed/applied
ta goal list --all

# Filter by specific state
ta goal list --state running
```

### Auto-Approval Policy

Configure policy-driven draft auto-approval in `.ta/policy.yaml`:

```yaml
defaults:
  auto_approve:
    drafts:
      enabled: true          # master switch (default: off)
      auto_apply: false      # also apply changes after approval
      git_commit: false       # create git commit if auto-applying
      conditions:
        max_files: 5          # only small changes
        max_lines_changed: 200
        allowed_paths:        # only safe paths
          - "tests/**"
          - "docs/**"
        blocked_paths:        # never auto-approve these
          - ".ta/**"
          - "**/main.rs"
        require_tests_pass: false
        require_clean_clippy: false
```

Dry-run auto-approval evaluation:

```bash
ta policy check <draft-id>    # Shows condition-by-condition evaluation
ta policy show                # Show resolved policy document
```

Per-agent overrides can tighten (never loosen) conditions:

```yaml
agents:
  codex:
    security_level: open
    auto_approve:
      drafts:
        enabled: true
        conditions:
          max_files: 3        # tighter than project default
```

---

## Configuration

### Workflow Config (`.ta/workflow.toml`)

The central configuration file for TA behavior:

```toml
[submit]
adapter = "git"                    # "git", "svn", "perforce", or "none"
auto_commit = true                 # Commit on ta draft apply
auto_push = true                   # Push after commit
auto_review = true                 # Open GitHub PR after push
co_author = "Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>"  # Co-author trailer on commits

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

Without this file, TA auto-detects your VCS (Git > SVN > Perforce > none) and uses sensible defaults.

### Commit Co-Authorship

Every commit made through `ta draft apply --git-commit` includes a `Co-Authored-By` trailer. This gives TA shared credit alongside the human author in GitHub's contribution graph, PR history, and `git log`.

The default co-author is `Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>`. To make this appear in GitHub's contribution graph, the email must match a verified email on a GitHub account.

Configure per-project in `.ta/workflow.toml`:

```toml
[submit]
# Default — shows as TA co-authored on GitHub (requires matching GitHub account)
co_author = "Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>"

# Use your org's bot account
co_author = "my-org-ta-bot <ta-bot@myorg.com>"

# Disable co-author trailer entirely
co_author = ""
```

The resulting commit looks like:

```
Add input validation to the API

Goal-ID: a1b2c3d4-...
PR-ID: e5f6g7h8-...
Phase: v0.3.1

Co-Authored-By: Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>
```

### VCS Adapters

TA uses pluggable adapters for version control operations. When `submit.adapter` is not explicitly set, TA auto-detects the VCS from the project directory:

| Adapter | Detection | Exclude patterns | Status |
|---------|-----------|-----------------|--------|
| `git` | `.git/` directory | `.git/` | Fully tested |
| `svn` | `.svn/` directory | `.svn/` | Stub (untested) |
| `perforce` | `.p4config` file or `P4CONFIG` env | `.p4config`, `.p4ignore` | Stub (untested) |
| `none` | Fallback | (none) | Fully tested |

Each adapter contributes VCS-specific exclude patterns that are merged with your `.taignore` and built-in defaults during staging. This means TA never copies VCS metadata into staging directories.

**Adapter operations:**

| Operation | Git | SVN | Perforce |
|-----------|-----|-----|----------|
| `prepare()` | Create feature branch | No-op | Create pending changelist |
| `commit()` | `git add` + `git commit` | `svn add` + `svn commit` | `p4 reconcile` + `p4 shelve` |
| `push()` | `git push` | No-op (commit is remote) | `p4 submit` |
| `open_review()` | `gh pr create` | No-op | Helix Swarm (if configured) |
| `save_state()` | Save current branch | No-op | Save client/changelist |
| `restore_state()` | Switch back to original branch | No-op | Log restore |

**SVN and Perforce adapters are stubs** — they implement the correct protocol but have not been tested against real servers. If you use SVN or Perforce, please test and report issues.

To explicitly select an adapter (bypassing auto-detection):

```toml
# .ta/workflow.toml
[submit]
adapter = "perforce"
```

**Build-time VCS revision**: The `ta` binary embeds a VCS revision at build time. Set `TA_REVISION` environment variable to override auto-detection (useful in CI for non-Git builds).

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

### Channel Setup

Channels control how TA communicates with you during review. When an agent finishes work and builds a draft, TA sends a review request through the configured channel and waits for your decision.

Configure channels in `.ta/config.yaml`:

```yaml
channels:
  review:
    type: terminal        # How you review drafts (approve/reject)
  session:
    type: terminal        # How interactive sessions stream output
```

Without this file, TA defaults to `terminal` for everything — review prompts appear directly in your terminal.

#### Available channel types

| Channel | Description | When to use |
|---------|-------------|-------------|
| `terminal` | Interactive terminal prompts (default) | Local development |
| `auto-approve` | Approves everything automatically | CI pipelines, batch jobs, testing |
| `webhook` | File-based exchange with external systems | Slack bots, custom review UIs |
| `discord` | External plugin — Discord embeds with buttons | Team review via Discord server |

#### Choosing a review channel

For **local development**, the default `terminal` channel works out of the box — you'll see review prompts inline and can approve, reject, or discuss.

For **CI/headless** environments, use `auto-approve`:

```yaml
channels:
  review:
    type: auto-approve
```

For **team review via Discord**, install the Discord channel plugin and configure the daemon to deliver questions to Discord. See [Discord Channel Plugin](#discord-channel-plugin) for setup instructions.

For **external review** (Slack bot, custom dashboard), use `webhook`:

```yaml
channels:
  review:
    type: webhook
    endpoint: /tmp/ta-reviews   # Directory for file-based exchange
```

See [Webhook Review Channel](#webhook-review-channel) for the full exchange protocol.

#### How approval and rejection work

When a draft is ready for review, TA sends an `InteractionRequest` to the configured review channel. The channel presents the request and collects a decision:

- **approve** — accept the draft; TA transitions it to `Approved` and it can be applied
- **reject** / **deny** — reject the draft; TA records the reasoning and transitions to `Denied`
- **discuss** — request more information; TA keeps the draft in review

For terminal channels, you type your decision interactively. For webhook channels, you write a JSON response file. For auto-approve, every request is automatically approved.

You can also add **policy-driven auto-approval** rules in `.ta/policy.yaml` so small, safe changes skip the review prompt entirely. See [Auto-Approval Policy](#auto-approval-policy).

#### Notifications

Notifications are fire-and-forget status updates (no response needed). Configure multiple notification targets:

```yaml
channels:
  notify:
    - type: terminal
    - type: webhook
      endpoint: /tmp/ta-notifications
      level: warning    # Only deliver warnings and errors
```

Level filter values: `debug`, `info` (default), `warning`, `error`.

#### Inspecting your setup

```bash
ta config channels         # Show what's configured
ta config channels --check # Verify channels build correctly
```

For multi-channel routing (sending reviews to multiple channels simultaneously), see [Multi-Channel Routing](#multi-channel-routing).

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

# Validate prerequisites without running anything
ta release validate 0.4.0-alpha

# Interactive release with human-in-the-loop review checkpoints
ta release run 0.4.0-alpha --interactive

# Skip approval gates (CI mode)
ta release run 0.4.0-alpha --yes

# Show pipeline steps
ta release show

# Create a customizable .ta/release.yaml
ta release init
```

From `ta shell`, the `release` shortcut launches the pipeline as a long-running command:

```
ta shell> release v0.10.6
```

The `--interactive` flag uses the `releaser` agent with `ta_ask_human` for review checkpoints. The human stays in `ta shell` throughout — the agent asks for release notes approval and publish confirmation interactively.

The `ta release validate` command checks prerequisites before running: version format, git cleanliness, tag availability, pipeline configuration, and toolchain presence. Use it in CI to gate releases.

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
      id: releaser
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

### Event System

TA publishes structured lifecycle events that external tools and scripts can consume.

#### Streaming events

```bash
# Stream all events as NDJSON (one JSON object per line)
ta events listen

# Filter by event type
ta events listen --filter draft_approved --filter goal_completed

# Filter by goal
ta events listen --goal <goal-id>

# Limit results
ta events listen --limit 50
```

Events are persisted to `.ta/events/<YYYY-MM-DD>.jsonl` files, rotated daily. Both CLI commands (`ta run`, `ta draft build`) and MCP tool handlers emit events to the same store, so orchestrator agents see a unified event stream regardless of how goals were created.

#### Event types

Events cover the full TA lifecycle: `goal_started`, `goal_completed`, `goal_failed`, `draft_built`, `draft_submitted`, `draft_approved`, `draft_denied`, `draft_applied`, `session_paused`, `session_resumed`, `session_aborted`, `plan_phase_completed`, `review_requested`, `policy_violation`, `memory_stored`, `workflow_started`, `stage_started`, `stage_completed`, `workflow_routed`, `workflow_completed`, `workflow_failed`, `workflow_awaiting_human`.

#### Event hooks

Configure shell commands or webhooks to run when specific events occur:

```toml
# .ta/hooks.toml
[[hooks]]
event = "draft_approved"
command = "notify-send 'Draft approved!'"

[[hooks]]
event = "policy_violation"
webhook = "https://hooks.slack.com/services/..."
```

Hook commands receive the event JSON via the `TA_EVENT_JSON` environment variable and the event type via `TA_EVENT_TYPE`.

```bash
# View configured hooks
ta events hooks
```

#### MCP event queries

MCP-connected agents (orchestrators, macro goal agents) can query events programmatically via the `ta_event_subscribe` tool:

```json
// Query events since a cursor timestamp
{ "action": "query", "since": "2026-03-05T10:00:00Z", "event_types": ["goal_failed", "draft_approved"] }

// Watch for new events (pass the cursor from the previous response)
{ "action": "watch", "since": "2026-03-05T10:05:23.456Z", "goal_id": "<goal-id>" }

// Get the most recent events
{ "action": "latest", "limit": 10 }
```

The response includes a `cursor` timestamp — pass it back as `since` on the next call to get only newer events. This enables efficient polling without re-reading old events.

#### JSON output

Key CLI commands support `--json` for programmatic consumption:

```bash
ta draft list --json
ta draft view <id> --json
ta goal status <id> --json
ta plan status --json
```

### Approval Tokens

For CI pipelines, chatbots, or other automated workflows, create tokens that authorize draft approval without interactive confirmation:

```bash
# Create a token (default: 24h expiry, draft:approve scope)
ta token create --scope draft:approve --expires 24h

# Use the token for non-interactive approval
ta draft approve <draft-id> --token <token-value>

# List all tokens
ta token list

# Clean up expired tokens
ta token cleanup
```

### Solution Knowledge Base

TA can extract reusable problem/solution pairs from memory into a curated `solutions.toml` file that ships with your project.

#### Exporting solutions

```bash
# Export NegativePath and Convention entries to .ta/solutions/solutions.toml
ta context export

# Skip interactive confirmation
ta context export --non-interactive

# Custom output path
ta context export --output path/to/solutions.toml
```

Each solution entry contains a problem description, solution, context (language/framework), and tags. Project-specific paths and UUIDs are stripped automatically.

#### Importing solutions

```bash
# Import from a local file
ta context import path/to/other/solutions.toml
```

Duplicate entries (matching by problem text) are automatically skipped.

#### Injection at runtime

When `ta run` launches an agent, solution entries matching the project type are included in the CLAUDE.md context injection under a "Known Solutions" section. Agents benefit from past solutions without rediscovering them.

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

### Daemon API

The TA daemon exposes a full HTTP API that any interface (terminal, web, Discord, Slack, email) can connect to for commands, agent conversations, and event streams.

#### Starting the API

```bash
# API mode (standalone HTTP server)
ta-daemon --api --project-root .

# MCP mode also starts the API server on port 7700
ta-daemon --project-root .
```

The API listens on `127.0.0.1:7700` by default. Configure via `.ta/daemon.toml`:

```toml
[server]
bind = "127.0.0.1"
port = 7700
cors_origins = ["*"]
```

#### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/cmd` | Execute a `ta` CLI command |
| `GET` | `/api/status` | Project dashboard (JSON) |
| `GET` | `/api/events` | SSE event stream |
| `POST` | `/api/input` | Unified input with routing |
| `GET` | `/api/routes` | Route table (for tab completion) |
| `POST` | `/api/agent/start` | Start an agent session |
| `POST` | `/api/agent/ask` | Send a prompt to an agent |
| `GET` | `/api/agent/sessions` | List agent sessions |
| `DELETE` | `/api/agent/:id` | Stop an agent session |

Plus the existing draft and memory endpoints (`/api/drafts/*`, `/api/memory/*`) and multi-project endpoints (`/api/projects/*`, `/api/office/*` — see [Multi-Project Office](#multi-project-office)).

#### Command Execution

```bash
curl -X POST http://127.0.0.1:7700/api/cmd \
  -H "Content-Type: application/json" \
  -d '{"command": "ta draft list"}'
```

Response:
```json
{"exit_code": 0, "stdout": "...", "stderr": ""}
```

Commands are validated against an allowlist in `.ta/daemon.toml`. Write commands (approve, deny, apply) require write scope.

#### Event Stream

Subscribe to real-time events via Server-Sent Events:

```bash
# Stream all events
curl -N http://127.0.0.1:7700/api/events

# Replay from a cursor
curl -N "http://127.0.0.1:7700/api/events?since=2024-01-01T00:00:00Z"

# Filter by event type
curl -N "http://127.0.0.1:7700/api/events?types=draft_built,goal_completed"
```

#### Unified Input

The `/api/input` endpoint routes text through the routing table (`.ta/shell.toml`). Input matching a route prefix runs as a command; everything else goes to the agent:

```bash
# This gets routed to /api/cmd (matches "ta " prefix)
curl -X POST http://127.0.0.1:7700/api/input \
  -H "Content-Type: application/json" \
  -d '{"text": "ta draft list"}'

# This gets routed to the agent (no prefix match)
curl -X POST http://127.0.0.1:7700/api/input \
  -H "Content-Type: application/json" \
  -d '{"text": "What should we work on next?", "session_id": "sess-abc123"}'
```

Shortcuts expand automatically: `"approve abc123"` becomes `"ta draft approve abc123"`.

#### Authentication

For remote access, enable token authentication:

```toml
# .ta/daemon.toml
[auth]
require_token = true
local_bypass = true   # 127.0.0.1 connections skip auth
```

Tokens are stored in `.ta/daemon-tokens.json`. Pass them via the `Authorization` header:

```bash
curl -H "Authorization: Bearer ta_..." http://your-server:7700/api/status
```

Token scopes: `read` (status, list, events), `write` (approve, deny, agent), `admin` (config, tokens).

#### Input Routing

Customize how input is routed in `.ta/shell.toml`:

```toml
[[routes]]
prefix = "ta "
command = "ta"
strip_prefix = true

[[routes]]
prefix = "!"           # Shell escape
command = "sh"
args = ["-c"]
strip_prefix = true

[[shortcuts]]
match = "approve"
expand = "ta draft approve"
```

### Multi-Project Office

Manage multiple projects with a single daemon using `office.yaml`.

#### Office Configuration

Create an `office.yaml` at your workspace root:

```yaml
office:
  name: "My Dev Office"
  daemon:
    http_port: 3140

projects:
  inventory-service:
    path: ~/dev/inventory-service
    plan: PLAN.md
    default_branch: main
  customer-portal:
    path: ~/dev/customer-portal

channels:
  discord:
    token_env: TA_DISCORD_TOKEN
    routes:
      "#backend-reviews":
        project: inventory-service
        type: review
      "#frontend-reviews":
        project: customer-portal
        type: review
      "#office-status":
        type: notify
        projects: all
  email:
    routes:
      "backend@acme.dev":
        project: inventory-service
        type: review
```

#### Starting the Office

```bash
# Start in background
ta office start --config office.yaml

# Start in foreground (for development)
ta office start --config office.yaml --foreground

# Or pass via environment variable
TA_OFFICE_CONFIG=office.yaml ta-daemon --api
```

#### Office Commands

```bash
ta office status                    # Overview of all projects
ta office status inventory-service  # Detail for one project
ta office project list              # List managed projects
ta office project add my-proj ~/dev/my-proj  # Add at runtime
ta office project remove my-proj    # Remove at runtime
ta office reload                    # Reload config without restart
ta office stop                      # Graceful shutdown
```

#### Message Routing

In multi-project mode, messages route to projects using this precedence:

1. **Channel route** — configured in `office.yaml` (`#backend-reviews` → `inventory-service`)
2. **Thread context** — replies in a goal thread stay with the same project
3. **Explicit prefix** — `@ta inventory-service plan list`
4. **User default** — user's `default_project` setting
5. **Ambiguous** — daemon asks the user to clarify

In single-project mode (no `office.yaml`), routing always resolves to the sole project.

#### Per-Project Overrides

Each project can have a `.ta/office-override.yaml` that overrides office-level settings:

```yaml
security_level: strict
default_agent: codex
max_sessions: 5
tags:
  - backend
  - critical
```

#### API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/projects` | List all managed projects |
| `GET` | `/api/projects/:name` | Project detail |
| `POST` | `/api/projects` | Add a project at runtime |
| `DELETE` | `/api/projects/:name` | Remove a project |
| `POST` | `/api/office/reload` | Reload office config |

Existing endpoints accept an optional `?project=<name>` query parameter to scope operations to a specific project.

### Interactive Shell

The interactive shell (`ta shell`) is a full-screen TUI client for the TA daemon. It gives you a persistent terminal with three zones: scrolling output, input line, and a live status bar.

#### Prerequisites

The shell connects to a running TA daemon. The easiest way is the convenience script that handles everything:

```bash
# macOS / Linux -- starts daemon if needed, then opens the shell
./scripts/ta-shell.sh

# Windows PowerShell
.\scripts\ta-shell.ps1

# With options
./scripts/ta-shell.sh --port 8080 --project-root /path/to/project
```

Or manage the daemon manually:

```bash
# Start the daemon (runs on http://127.0.0.1:7700 by default)
ta-daemon --project-root .

# Or run it in the background
ta-daemon --project-root . &

# Verify it's running
curl -s http://127.0.0.1:7700/api/status | head
```

If the daemon is not running, `ta shell` will show connection errors on startup.

#### Daemon Version Guard

When `ta shell` or `ta dev` connects to the daemon, it checks whether the daemon version matches the CLI version. After an upgrade (e.g., `./install_local.sh`), the old daemon process may still be running with the previous version. The version guard detects this and prompts you:

```
Daemon version mismatch: daemon v0.10.6-alpha, CLI v0.10.10-alpha
Restart daemon with the new version? [Y/n]
```

- **Yes** (default): The CLI sends a graceful shutdown request, waits for the old daemon to exit, starts the new one, and verifies it's healthy.
- **No**: The shell proceeds but shows `daemon (stale)` in the status bar so you know you're running against a mismatched daemon.

To skip the version check (useful in CI or scripts):

```bash
ta --no-version-check shell
ta --no-version-check dev
```

The `--no-version-check` flag is global — it works with any subcommand.

#### Starting the shell

```bash
# Start the full TUI shell (default)
ta shell

# Connect to a custom daemon URL
ta shell --url http://my-server:7700

# Attach to an existing agent session
ta shell --attach sess-abc123

# Use the classic line-mode shell (rustyline REPL, pre-v0.9.8.3 behavior)
ta shell --classic
```

The TUI shell provides a three-zone layout:

```
┌─────────────────────────────────────────────────────────┐
│  [scrolling output]                                     │
│  goal started: "Implement v0.9.8.1" (claude-code)       │
│  draft built: 15 files (abc123)                         │
│  $ ta goal list                                         │
│  ID       Title                    State    Agent       │
│  ca306e4d Implement v0.9.8.1       running  claude-code │
├─────────────────────────────────────────────────────────┤
│ ta> ta draft list                                       │
├─────────────────────────────────────────────────────────┤
│ TrustedAutonomy v0.9.8 │ 1 agent │ 0 drafts │ ◉ daemon│
└─────────────────────────────────────────────────────────┘
```

- **Output pane** (top): Command responses and SSE event notifications. Events are rendered in dimmed styling. Auto-scrolls to bottom; use PgUp/PgDn to scroll back. Unread events are tracked when scrolled up.
- **Input area** (middle): Text input with cursor movement, command history (up/down), and tab-completion.
- **Status bar** (bottom): Project name, version, agent count, draft count, daemon connection indicator (green/red dot), unread event badge, and workflow stage indicator.

#### Using the shell

Input is routed through the daemon's routing table. Recognized prefixes run as commands; everything else goes to the agent (if a session is attached):

```
ta> ta draft list                     # Runs: ta draft list
ta> git status                        # Runs: git status
ta> !ls -la                           # Shell escape: sh -c ls -la
ta> approve abc123                    # Shortcut: ta draft approve abc123
ta> status                            # Shortcut: ta status
ta> What should we work on next?      # Sent to agent session
```

Built-in shell commands:

| Command | Description |
|---------|-------------|
| `help` / `?` | Show help |
| `:status` | Refresh the status bar |
| `clear` / `Ctrl-L` | Clear the output pane |
| `PgUp` / `PgDn` | Scroll output |
| `Tab` | Auto-complete commands |
| `Ctrl-A` / `Ctrl-E` | Jump to start/end of input |
| `Ctrl-U` / `Ctrl-K` | Clear input before/after cursor |
| `Ctrl-C` / `exit` / `quit` / `:q` | Exit the shell |

#### Workflow interaction mode

When a workflow stage pauses for human input, the shell switches to `workflow>` prompt mode. The output pane shows the workflow prompt and available options. Normal commands still work during workflow prompts.

Tab completion is available for shortcuts and shell commands. Command history persists across sessions in `~/.ta/shell_history`.

#### Typical workflow

A common session using the shell looks like:

```
ta> status                            # Check project state
ta> plan                              # See plan phases
ta> ta run "Fix auth bug"  # Start a goal
ta> drafts                            # Check for completed drafts
ta> view abc123                       # Review a draft
ta> approve abc123                    # Approve it
ta> ta draft apply abc123             # Apply changes
```

Background events appear inline as goals complete and drafts become ready:

```
ta> status

-- Event: Draft abc123 is ready for review --

ta> view abc123
```

#### Customizing routing

Generate the default routing config:

```bash
ta shell --init
```

This creates `.ta/shell.toml` with the default route prefixes (`ta`, `git`, `!`) and shortcuts (`approve`, `deny`, `view`, etc.).

#### Adding project-specific shortcuts

Edit `.ta/shell.toml` to add shortcuts for commands you use often:

```toml
# Quick access to your test suite
[[shortcuts]]
match = "test"
expand = "!cargo test --workspace"

# Deploy shortcut
[[shortcuts]]
match = "deploy"
expand = "!./scripts/deploy.sh"

# Quick context lookup
[[shortcuts]]
match = "ctx"
expand = "ta context list"
```

#### Adding custom route prefixes

Routes map input prefixes to command executors. Add routes for tools your project uses:

```toml
# Route "npm ..." commands
[[routes]]
prefix = "npm "
command = "npm"
strip_prefix = true

# Route "make ..." commands
[[routes]]
prefix = "make "
command = "make"
strip_prefix = true

# Route "docker ..." commands
[[routes]]
prefix = "docker "
command = "docker"
strip_prefix = true
```

#### Remote shell access

To use `ta shell` from a different machine, configure authentication on the daemon:

```toml
# .ta/daemon.toml
[server]
bind = "0.0.0.0"          # Listen on all interfaces (not just localhost)
port = 7700

[auth]
require_token = true       # Require Bearer token for all requests
local_bypass = false       # No bypass, even for localhost
```

Generate a token and connect:

```bash
# On the server: create a write-scoped token
ta token create --scope write --label "remote-shell"
# Output: ta_abc123...

# On the client: connect with --url
ta shell --url http://my-server:7700
```

The shell does not currently pass tokens (remote auth requires using the daemon API directly or extending the shell config). For now, remote access is best served via the [Web Review UI](#web-review-ui) or the [Daemon API](#daemon-api) directly.

#### Daemon configuration reference

The daemon reads `.ta/daemon.toml` for server, auth, command, and agent settings:

```toml
[server]
bind = "127.0.0.1"         # Bind address
port = 7700                 # Listen port
cors_origins = ["*"]        # CORS origins for web clients

[auth]
require_token = false       # Require Bearer tokens
local_bypass = true         # Skip auth for 127.0.0.1

[commands]
allowed = [                 # Command allowlist (glob patterns)
  "ta draft *",
  "ta goal *",
  "ta plan *",
  "ta status",
  "ta context *",
]
denied = [                  # Command denylist (deny takes precedence)
  "ta draft apply --force *",
]
write_commands = [           # Commands requiring write scope
  "ta draft approve *",
  "ta draft deny *",
  "ta draft apply *",
  "ta goal start *",
]
timeout_secs = 30           # Command execution timeout

[agent]
max_sessions = 3            # Maximum concurrent agent sessions
idle_timeout_secs = 3600    # Idle session cleanup
default_agent = "claude-code"

[routing]
use_shell_config = true     # Load routes from .ta/shell.toml
```

#### How it works

The shell does no business logic -- all command execution, agent management, and event streaming live in the daemon. The shell is a rendering layer that:

1. Fetches status from `GET /api/status`
2. Reads input with line editing and history
3. Sends input to `POST /api/input` (daemon routes to command or agent)
4. Listens for events on `GET /api/events` (SSE) in the background

This means `ta shell`, the web UI, Discord, and Slack all use the same daemon APIs.

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
| `discord` | Plugin | External plugin — Discord embeds with buttons (v0.10.2.1) |
| `slack` | Plugin | External plugin — Slack Block Kit messages with buttons (v0.10.3) |
| `email` | Stub | Future: SMTP send with IMAP reply parsing |

### Discord Channel Plugin

Discord is available as an **external channel plugin** (refactored from a built-in crate in v0.10.2.1). It delivers agent questions as rich embeds with button components to a Discord channel.

#### Quick setup

1. **Create a Discord bot** at [discord.com/developers](https://discord.com/developers/applications):
   - New Application → Bot → copy the token
   - Under OAuth2 → URL Generator: select `bot` scope with `Send Messages` and `Embed Links` permissions
   - Invite the bot to your server using the generated URL

2. **Set environment variables**:
   ```bash
   export TA_DISCORD_TOKEN="your-bot-token-here"
   export TA_DISCORD_CHANNEL_ID="123456789012345678"
   ```

3. **Install the plugin**:
   ```bash
   # Build from source
   cd plugins/ta-channel-discord
   cargo build --release

   # Install to project
   mkdir -p .ta/plugins/channels/discord
   cp target/release/ta-channel-discord .ta/plugins/channels/discord/
   cp channel.toml .ta/plugins/channels/discord/
   ```

4. **Configure** `.ta/daemon.toml`:
   ```toml
   [channels]
   default_channels = ["discord"]
   ```

5. **Verify**:
   ```bash
   ta config channels
   # Should show "discord" in the registered channel types list
   ```

#### How it works

1. An agent calls `ta_ask_human` or needs input
2. The daemon builds a `ChannelQuestion` and dispatches to the Discord plugin
3. The plugin posts a rich embed with buttons (Yes/No, choices, or freeform prompt) to the configured Discord channel
4. A human clicks a button or replies in a thread
5. The response flows back to TA via `POST /api/interactions/{id}/respond`

#### Alternative: Inline config (no plugin install needed)

Instead of installing the plugin directory, configure Discord directly in `.ta/daemon.toml`:

```toml
[[channels.external]]
name = "discord"
command = "/path/to/ta-channel-discord"
protocol = "json-stdio"
timeout_secs = 30

[channels]
default_channels = ["discord"]
```

See [Discord Channel Guide](guides/discord-channel.md) for the full setup guide including bot permissions, access control, and troubleshooting.

### Slack Channel Plugin

Slack is available as an **external channel plugin**. It delivers agent questions as Block Kit messages with interactive buttons to a Slack channel.

#### Quick setup

1. **Create a Slack app** at [api.slack.com/apps](https://api.slack.com/apps):
   - Create New App → From scratch
   - Under **OAuth & Permissions**, add the `chat:write` bot scope
   - Install the app to your workspace and copy the **Bot User OAuth Token** (`xoxb-...`)
   - Invite the bot to your target channel: `/invite @YourBotName`

2. **Set environment variables**:
   ```bash
   export TA_SLACK_BOT_TOKEN="xoxb-your-bot-token"
   export TA_SLACK_CHANNEL_ID="C01ABC23DEF"
   # Optional: restrict who can respond
   export TA_SLACK_ALLOWED_USERS="U01ABC,U02DEF"
   ```

3. **Build and install the plugin**:
   ```bash
   # Build from source (or use ta plugin build)
   ta plugin build slack

   # Or manually:
   cd plugins/ta-channel-slack
   cargo build --release
   mkdir -p .ta/plugins/channels/slack
   cp target/release/ta-channel-slack .ta/plugins/channels/slack/
   cp channel.toml .ta/plugins/channels/slack/
   ```

4. **Configure** `.ta/daemon.toml`:
   ```toml
   [channels]
   default_channels = ["slack"]
   ```

5. **Verify**:
   ```bash
   ta plugin validate
   # Should show "slack" as valid with json-stdio protocol
   ```

#### How it works

1. An agent calls `ta_ask_human` or needs input
2. The daemon builds a `ChannelQuestion` and dispatches to the Slack plugin
3. The plugin posts a Block Kit message with buttons (Approve/Deny, choices, or freeform instructions) to the configured Slack channel
4. For long context, detail is posted as a thread reply
5. A human clicks a button or replies in a thread
6. The response flows back to TA via `POST /api/interactions/{id}/respond`

#### Alternative: Inline config (no plugin install needed)

Instead of installing the plugin directory, configure Slack directly in `.ta/daemon.toml`:

```toml
[[channels.external]]
name = "slack"
command = "/path/to/ta-channel-slack"
protocol = "json-stdio"
timeout_secs = 30

[channels]
default_channels = ["slack"]
```

### Email Channel Plugin

Email is available as an **external channel plugin**. It delivers agent questions as formatted HTML+text emails via SMTP with reply-based response parsing.

#### Setup

1. **Build the plugin:**

```bash
ta plugin build email
# Or manually:
cd plugins/ta-channel-email
cargo build --release
cp target/release/ta-channel-email .ta/plugins/channels/email/
cp channel.toml .ta/plugins/channels/email/
```

2. **Set environment variables:**

```bash
export TA_EMAIL_SMTP_HOST="smtp.gmail.com"   # SMTP server
export TA_EMAIL_SMTP_PORT="587"              # SMTP port (default: 587, STARTTLS)
export TA_EMAIL_USER="agent@company.com"     # Sender email / SMTP username
export TA_EMAIL_PASSWORD="xxxx-xxxx-xxxx"    # SMTP password or app password
export TA_EMAIL_REVIEWER="reviewer@company.com,lead@company.com"  # Comma-separated
```

Optional:
```bash
export TA_EMAIL_FROM_NAME="TA Agent"         # Display name (default: "TA Agent")
export TA_EMAIL_SUBJECT_PREFIX="[TA Review]" # Subject prefix (default: "[TA Review]")
```

3. **Configure the daemon** (`.ta/daemon.toml`):

```toml
[[channels.external]]
name = "email"
command = "ta-channel-email"
protocol = "json-stdio"
timeout_secs = 60

[channels]
default_channels = ["email"]
```

#### How it works

1. An agent calls `ta_ask_human` with a question
2. The daemon spawns `ta-channel-email`, passing the question as JSON on stdin
3. The plugin sends an HTML+text email to all reviewers via SMTP
4. Emails include the question, context, and response guidance based on question type
5. The reviewer replies to the email (APPROVE/DENY for yes/no, or freeform text)
6. The response flows back to TA via `POST /api/interactions/{id}/respond`

#### Gmail App Passwords

For Gmail, use an [App Password](https://myaccount.google.com/apppasswords) instead of your account password:
1. Enable 2-Step Verification on your Google account
2. Generate an App Password at the link above
3. Set `TA_EMAIL_PASSWORD` to the generated 16-character password

#### Reply format

The email plugin recognizes these keywords in replies (case-insensitive):
- **Approve**: `APPROVE`, `APPROVED`, `YES`, `LGTM`, `ACK`
- **Deny**: `DENY`, `DENIED`, `NO`, `REJECT`, `REJECTED`, `NACK`
- **Freeform**: Any other text (quoted text, signatures, and attribution lines are stripped)

#### Email threading

Follow-up questions on the same interaction use `In-Reply-To` and `References` headers, so email clients group them into a single thread.

### Project Status Dashboard

Get a quick overview of your project's current state:

```bash
ta status
# Project: MyProject (v0.9.6-alpha)
# Next phase: v0.9.7 — Daemon API Expansion
#
# Active agents:
#   agent-1 (agent-1) → goal abc12345 "Fix auth bug" [running 12m]
#
# Pending drafts: 2
# Active goals:   1
# Total goals:    5
```

This shows the project name, version, next pending plan phase, active agents with their goal associations, and counts of pending drafts and goals.

### Agent Tracking (MCP)

When running as an MCP server, TA tracks active agent sessions for observability. The `ta_agent_status` tool lets orchestrators query which agents are running:

```json
// List all active agents
{ "action": "list" }
// → { "agents": [...], "count": 2 }

// Check a specific agent
{ "action": "status", "agent_id": "agent-1" }
// → { "agent_id": "agent-1", "agent_type": "claude-code", "goal_run_id": "abc...", "running_secs": 720 }
```

Agent sessions emit `AgentSessionStarted` and `AgentSessionEnded` events for the event system.

#### CallerMode enforcement

When `TA_CALLER_MODE=orchestrator`, the MCP gateway restricts operations to read-only project-scoped tools. Orchestrators can read plans, list goals/drafts, query context, and start new goals — but cannot directly write files or build PRs (those require an active goal context).

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

# Close a session cleanly (auto-builds a draft if changes exist)
ta session close <session-id>

# Close without building a draft
ta session close <session-id> --no-draft

# List all sessions (including completed/aborted)
ta session list --all

# Show session details and conversation history
ta session show <session-id>
```

Use `ta session close` instead of `ta session abort` when the agent's work is worth keeping — it will automatically build a draft from any uncommitted changes in the staging workspace before marking the session as completed. This prevents losing work when PTY sessions exit abnormally (Ctrl-C, crash).

When resuming a session, TA now checks workspace health before reattaching. If the workspace is missing or the child process has died, you'll see actionable suggestions (close or abort) instead of a raw error.

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

### Unified Access Control Pattern

All allow/deny lists in TA follow the same `AccessFilter` pattern:

- **Deny always takes precedence** over allow
- **Empty `allowed`** = allow all
- **Empty `denied`** = deny nothing

This pattern is used across daemon command routing, auto-approval path matching, and sandbox command policies.

```yaml
# In .ta/daemon.toml — command access control
[commands]
allowed = ["ta draft *", "ta goal *", "ta status"]
denied = ["ta draft apply --force *"]  # deny always wins

# In .ta/policy.yaml — auto-approval path rules
defaults:
  auto_approve:
    drafts:
      conditions:
        allowed_paths: ["tests/**", "docs/**"]
        blocked_paths: [".ta/**", "**/main.rs"]
```

The `AccessFilter` struct in `ta-policy` provides this as a reusable building block:

```rust
use ta_policy::AccessFilter;

let filter = AccessFilter::new(
    vec!["src/**".to_string()],     // allowed patterns
    vec!["**/secret*".to_string()], // denied patterns
);
assert!(filter.permits("src/lib.rs"));
assert!(!filter.permits("src/secret_key.rs")); // denied wins
```

Sandbox commands also support a `denied_commands` list that takes precedence over the allowlist.

### Resource Mediation (v0.6.2)

The `ResourceMediator` trait generalizes TA's staging pattern from files to any resource type. Each mediator handles a URI scheme (`fs://`, `email://`, `db://`, etc.) and provides stage → preview → apply → rollback operations.

Built-in mediators:
- **FsMediator** (`fs://`): File system staging (wraps existing staging workspace)

The `MediatorRegistry` routes actions to the correct mediator by URI scheme.

Built-in mediators:
- **ApiMediator** (`mcp://`): MCP tool call staging (see [API Mediation](#api-mediation))

### Channel Registry

TA's IO channels (terminal, webhook, Slack, etc.) register through a pluggable `ChannelFactory` trait. All channels are equal — the routing config determines which channel handles review, notifications, sessions, and escalation.

Configure channels in `.ta/config.yaml`:

```yaml
channels:
  review: { type: terminal }
  notify:
    - { type: terminal }
    - { type: webhook, endpoint: "https://hooks.example.com/ta", level: warning }
  session: { type: terminal }
  escalation: { type: webhook, endpoint: "https://hooks.example.com/escalate" }
  default_agent: claude-code
```

Built-in channel types: `terminal`, `auto-approve`, `webhook`. Third-party channels implement the `ChannelFactory` trait and register in the `ChannelRegistry`.

Each channel declares capabilities (`supports_review`, `supports_session`, `supports_notify`, `supports_rich_media`, `supports_threads`) so TA can validate routing config at startup.

#### Multi-Channel Routing

Send review requests and escalations to multiple channels simultaneously. Each route (`review`, `escalation`) accepts either a single channel object or an array:

```yaml
channels:
  review:
    - type: terminal
    - type: webhook
      endpoint: .ta/channel-exchange
  escalation:
    - type: webhook
      endpoint: .ta/esc-exchange-1
    - type: webhook
      endpoint: .ta/esc-exchange-2
  strategy: first_response   # or "quorum"
```

Dispatch strategies:
- **first_response** (default) — first channel to return a response wins; failures fall through to the next channel.
- **quorum** — require N approvals before returning (default quorum size: 2).

Notifications (`notify`) already support arrays and fan out to all configured channels.

#### Channel Plugins

Add custom channel integrations (Teams, PagerDuty, ServiceNow, etc.) without writing Rust or modifying TA source. Plugins are external executables in any language.

**Two protocols:**

| Protocol | How it works | Best for |
|----------|-------------|----------|
| **json-stdio** | TA spawns plugin, sends question JSON on stdin, reads result from stdout | Local tools, scripts, CLIs |
| **http** | TA POSTs question JSON to a URL, reads result from response body | Cloud functions, webhooks, running services |

**Writing a plugin:**

Create a directory with a `channel.toml` manifest:

```toml
name = "teams"
version = "0.1.0"
command = "python3 ta-channel-teams.py"
protocol = "json-stdio"
capabilities = ["deliver_question"]
description = "Microsoft Teams channel plugin"
timeout_secs = 30
```

The plugin reads `ChannelQuestion` JSON from stdin and writes `DeliveryResult` JSON to stdout:

```python
#!/usr/bin/env python3
import json, sys

question = json.loads(sys.stdin.readline())
# Deliver the question (Teams API, webhook, etc.)
print(json.dumps({
    "channel": "teams",
    "delivery_id": "msg-123",
    "success": True,
    "error": None
}))
```

Human responses flow back via `POST {callback_url}/api/interactions/{id}/respond`.

**Building plugins from source:**

If you have Rust plugin source code in `plugins/`, build and install in one step:

```bash
# Build a specific plugin by name
ta plugin build discord

# Build multiple plugins
ta plugin build discord,slack

# Build all discoverable plugins in plugins/
ta plugin build --all
```

This scans `plugins/` for subdirectories containing both `Cargo.toml` and `channel.toml`, runs `cargo build --release`, and copies the binary + manifest to `.ta/plugins/channels/<name>/`. You can reference plugins by their manifest name (e.g., `discord`), directory name (e.g., `ta-channel-discord`), or shorthand (e.g., `discord` resolves to `ta-channel-discord/`).

**Installing pre-built plugins:**

```bash
# Install to project (.ta/plugins/channels/)
ta plugin install ./my-plugin-dir

# Install globally (~/.config/ta/plugins/channels/)
ta plugin install ./my-plugin-dir --global

# List installed plugins
ta plugin list

# Validate all plugins
ta plugin validate
```

**Inline config (no install needed):**

Register plugins directly in `.ta/daemon.toml`:

```toml
[[channels.external]]
name = "teams"
command = "ta-channel-teams"
protocol = "json-stdio"

[[channels.external]]
name = "pagerduty"
protocol = "http"
deliver_url = "https://my-service.com/ta/deliver"
auth_token_env = "TA_PAGERDUTY_TOKEN"
```

**Plugin discovery order:**
1. `[[channels.external]]` in daemon.toml (inline config)
2. `.ta/plugins/channels/*/channel.toml` (project-local)
3. `~/.config/ta/plugins/channels/*/channel.toml` (user-global)

Inline config takes priority — if a plugin name is already registered from daemon.toml, discovered plugins with the same name are skipped.

**Starter templates** are provided in `templates/channel-plugins/` for Python, Node.js, and Go.

#### Inspecting Channel Configuration

View the resolved channel setup for your project:

```bash
ta config channels           # Show active channels, types, capabilities
ta config channels --check   # Verify each channel builds successfully
```

Example output:
```
Config: /path/to/project/.ta/config.yaml

Review (2 channels):
  [ok] type: terminal
    Capabilities: review=true, session=true, notify=true, rich_media=false, threads=false
  [ok] type: webhook
    Capabilities: review=true, session=false, notify=true, rich_media=false, threads=false
  Strategy: first_response

Registered channel types: auto-approve, terminal, webhook
```

### External Channel Delivery

When an agent calls `ta_ask_human`, the question can be delivered to external channels (Slack, Discord, email) in addition to the local `ta shell`. Configure channels in `.ta/daemon.toml`:

```toml
[channels]
default_channels = ["discord"]  # Deliver questions to these channels by default

# Slack is an external plugin (v0.10.3). Set TA_SLACK_BOT_TOKEN and
# TA_SLACK_CHANNEL_ID as environment variables. The plugin is auto-discovered
# from .ta/plugins/channels/slack/ or can be configured inline:
# [[channels.external]]
# name = "slack"
# command = "ta-channel-slack"
# protocol = "json-stdio"

# Discord is an external plugin (v0.10.2.1). Set TA_DISCORD_TOKEN and
# TA_DISCORD_CHANNEL_ID as environment variables. The plugin is auto-discovered
# from .ta/plugins/channels/discord/ or can be configured inline:
# [[channels.external]]
# name = "discord"
# command = "ta-channel-discord"
# protocol = "json-stdio"

[channels.email]
send_endpoint = "https://api.sendgrid.com/v3/mail/send"
api_key = "your-api-key"
from_address = "agent@yourcompany.com"
to_address = "reviewer@yourcompany.com"
```

Each channel renders questions in its native format:

| Channel | Rendering | Response mechanism |
|---------|-----------|-------------------|
| **Slack** | Block Kit message with action buttons | Button click or thread reply |
| **Discord** | Embed with button components | Button interaction or thread reply |
| **Email** | HTML email with choices listed | Reply email or API call |

All responses flow back through `POST /api/interactions/:id/respond`, which is the same endpoint `ta shell` uses. This means any channel adapter is a thin delivery layer — the core interaction protocol is channel-agnostic.

Questions can specify routing hints via the `channels` field in the `AgentNeedsInput` event. If no hints are provided, the daemon uses `default_channels` from the config.

### API Mediation

The `ApiMediator` stages intercepted MCP tool calls for human review before execution. It implements the `ResourceMediator` trait for the `mcp://` URI scheme.

When an agent calls an MCP tool (e.g., `gmail_send`, `slack_post_message`), TA:
1. **Stages** the call as a JSON file with tool name, parameters, and classification
2. **Previews** a human-readable summary with risk flags (IRREVERSIBLE, EXTERNAL)
3. **Applies** the call after human approval (marks ready for MCP gateway replay)
4. **Rolls back** by removing the staged file if denied

Tool calls are auto-classified by name patterns:
- **ReadOnly**: `_read`, `_get`, `_list`, `_search`, `_find`, `_query`, `_fetch`
- **Irreversible**: `_send`, `_publish`, `_tweet`, `_delete`, `_drop`
- **ExternalSideEffect**: `_post`, `_create`, `_update`, `_put`, `_patch`, `_upload`
- **StateChanging**: everything else

### Terms of Use

TA includes a terms-of-use acceptance step on first run.

```bash
# Accept terms non-interactively (CI/scripted usage)
ta accept-terms

# View the current terms
ta view-terms

# Check acceptance status
ta terms-status

# All commands also accept --accept-terms flag
ta run "task" --accept-terms
```

### Project Setup

Use `ta setup` to configure TA for an existing project interactively.

```bash
# Full wizard — auto-detects project type, generates all config
ta setup wizard

# Refine a single section
ta setup refine policy
ta setup refine memory
ta setup refine agents

# Show resolved configuration
ta setup show
```

The wizard detects your project type (Rust, TypeScript, Python, Go, or generic) and generates appropriate `.ta/` configuration files: `workflow.toml`, `memory.toml`, `policy.yaml`, agent YAML, and channel config.

Use `ta setup refine <section>` to update one config file at a time. Available sections: `workflow`, `memory`, `policy`, `agents`, `channels`.

### Project Initialization

Use `ta init` to bootstrap a new TA-managed project from a template.

```bash
# Initialize with auto-detection
ta init run --detect

# Initialize with a specific template
ta init run --template rust-workspace

# List available templates
ta init templates
```

Available templates: `rust-workspace`, `typescript-monorepo`, `python-ml`, `go-service`, `generic`.

Each template generates:
- `.ta/workflow.toml` — workflow defaults for the project type
- `.ta/memory.toml` — key schema and backend config
- `.ta/policy.yaml` — starter policy with appropriate security level
- `.ta/agents/claude-code.yaml` — agent config with bounded actions
- `.taignore` — exclude patterns for the language/framework
- `.ta/constitutions/` — starter constitutions for common task types
- Seeded memory entries from project structure (e.g., Cargo.toml workspace members → `arch:module-map`)

`ta init` reads existing project files and tailors config to the actual structure — not just generic templates.

### Add TA to an Existing Project

If you have an existing codebase and want to add TA governance:

```bash
# Auto-detect project type and installed agent frameworks
ta init run --detect

# Or use the setup wizard for more control
ta setup wizard
```

Both commands detect your project type (Rust, TypeScript, Python, Go) and scan for installed agent frameworks on your PATH. They generate appropriate `.ta/` configuration files.

**What TA creates:**
- `.ta/workflow.toml` — auto-capture settings for memory
- `.ta/memory.toml` — key schema tuned to your project type
- `.ta/policy.yaml` — starter security policy (checkpoint mode)
- `.ta/agents/<framework>.yaml` — agent launch config for each detected framework
- `.taignore` — exclude patterns for your language/framework

**What you provide:**
- Your project's source code (TA reads but doesn't modify existing files during setup)
- A `PLAN.md` if you want plan-linked goals (optional)

**Framework-specific notes:**
- **Ollama**: Requires a running Ollama server (`ollama serve`). Configure model in `.ta/agents/ollama.yaml`.
- **LangChain / LangGraph**: Requires Python environment with packages installed (`pip install langchain langchain-cli` or `pip install langgraph langgraph-cli`).
- **BMAD-METHOD**: Wraps another runtime (typically Claude Code). No separate install needed beyond the wrapped agent.

### Framework Registry

TA ships a built-in registry of known agent frameworks. During `ta init` and `ta setup wizard`, TA checks which frameworks are installed on your PATH and generates agent configs automatically.

**Supported frameworks:**

| Framework | Command | Runtime |
|-----------|---------|---------|
| Claude Code | `claude` | native-cli |
| Codex | `codex` | native-cli |
| Ollama | `ollama` | native-cli |
| LangChain | `langchain` | python |
| LangGraph | `langgraph` | python |
| BMAD-METHOD | *(methodology)* | wraps another runtime |
| Claude Flow | `claude-flow` | native-cli |

**Override the registry** by placing a `frameworks.toml` at `.ta/frameworks.toml` (project-level) or `~/.config/ta/frameworks.toml` (user-level). Project overrides take priority.

```toml
[frameworks.my-agent]
name = "My Custom Agent"
description = "A custom agent framework"
homepage = "https://example.com"
install = "npm install -g my-agent"
detect = ["my-agent"]
agent_config = "my-agent.yaml"
runtime = "native-cli"
```

After adding a custom framework, run `ta setup refine agents` to generate its agent config.

### Workflow Engine

TA includes a pluggable workflow engine for orchestrating multi-stage, multi-role workflows. Define stages, assign roles to agents, and let TA handle routing, verdict scoring, and human-in-the-loop interaction.

#### Quick start

```bash
# Scaffold a new workflow
ta workflow new my-workflow

# Scaffold from a built-in template
ta workflow new my-pipeline --from deploy-pipeline

# Validate before running
ta workflow validate .ta/workflows/my-workflow.yaml

# Start a workflow from a YAML definition
ta workflow start .ta/workflows/my-workflow.yaml

# Check status
ta workflow status <workflow-id>

# List active workflows
ta workflow list

# Browse available templates
ta workflow list --templates

# Cancel a workflow
ta workflow cancel <workflow-id>

# Show stage transitions and routing history
ta workflow history <workflow-id>
```

#### Authoring a workflow end-to-end

Creating a custom workflow is a three-step process: scaffold the workflow, create any missing agent configs, then validate and run.

**Step 1: Scaffold a workflow.**

```bash
ta workflow new my-pipeline
```

This creates `.ta/workflows/my-pipeline.yaml` with annotated comments explaining every field. The default scaffold is a 2-stage build→review workflow. To start from a richer template:

```bash
ta workflow new my-pipeline --from deploy-pipeline
```

Available templates: `simple-review`, `security-audit`, `milestone-review`, `deploy-pipeline`, `plan-implement-review`. Browse them with `ta workflow list --templates`.

**Step 2: Create missing agent configs.**

After scaffolding, TA checks which agents the workflow references and tells you which ones are missing:

```
Created workflow: .ta/workflows/my-pipeline.yaml

Missing agent configs (create them to complete setup):
  ta agent new claude-code --type developer
```

Each workflow role has an `agent:` field that points to a config file in `.ta/agents/`. TA guesses the right agent type from the role name (reviewer → auditor, planner → planner, etc.). Run the suggested commands:

```bash
ta agent new claude-code --type developer
```

This creates `.ta/agents/claude-code.yaml` with appropriate defaults for the agent type:

| Type | Security level | Permissions |
|------|---------------|-------------|
| `developer` | `checkpoint` | read, write, execute |
| `auditor` | `supervised` | read, list, search (read-only) |
| `planner` | `checkpoint` | read, list, search, plan |
| `orchestrator` | `checkpoint` | read, list, search, plan, delegate |

Edit the generated config to customize the command, args, and alignment for your project. Validate it:

```bash
ta agent validate .ta/agents/claude-code.yaml
```

This checks required fields, verifies the command exists on PATH, and warns on common misconfigurations (e.g., `injects_settings: true` without `injects_context_file: true`).

**Step 3: Validate and run.**

```bash
ta workflow validate .ta/workflows/my-pipeline.yaml
```

Validation checks:
- **Schema**: required fields, non-empty name, at least one stage
- **References**: every role used in a stage is defined in `roles:`
- **Dependencies**: no cycles, no references to undefined stages
- **Agents**: every `roles.*.agent` has a matching config in `.ta/agents/`
- **Verdict config**: valid threshold range, referenced roles exist

Even when the workflow is valid, the validator shows any remaining missing agent configs with ready-to-run commands. Once everything passes:

```bash
ta workflow start .ta/workflows/my-pipeline.yaml
```

#### Managing agents and workflows

```bash
# List configured agents
ta agent list

# List active workflows
ta workflow list

# Browse agent templates
ta agent list --templates

# Browse workflow templates
ta workflow list --templates
```

#### Version schema templates

Pre-built version schemas in `templates/version-schemas/`:

```bash
# Copy a version schema to your project
cp templates/version-schemas/semver.yaml .ta/version-schema.yaml
```

| Schema | Format | Use case |
|--------|--------|----------|
| `semver.yaml` | MAJOR.MINOR.PATCH-pre | Standard semantic versioning |
| `calver.yaml` | YYYY.MM.PATCH | Calendar-based releases |
| `sprint.yaml` | sprint-N.iteration | Agile sprint cycles |
| `milestone.yaml` | vN.phase | Simple milestone tracking |

#### Workflow YAML format

```yaml
name: my-workflow
stages:
  - name: build
    roles: [engineer]
  - name: review
    depends_on: [build]
    roles: [reviewer]
    review:
      pass_threshold: 0.7
      required_pass: [security-reviewer]
    on_fail:
      route_to: build
      max_retries: 3
    await_human: on_fail    # always | never | on_fail

roles:
  engineer:
    agent: claude-code
    prompt: "Build the feature described in the goal"
  reviewer:
    agent: claude-code
    prompt: "Review the implementation for correctness and security"

verdict:
  pass_threshold: 0.7
  required_pass: [security-reviewer]
  scorer:
    agent: claude-code
    prompt: |
      Synthesize review verdicts into an aggregate assessment.
      Weight security findings 2x.
```

**Stages** execute in dependency order (topological sort). Each stage assigns one or more roles. When a stage completes, verdicts are scored and the engine decides: proceed, route back, complete, or pause for human input.

**Verdict scoring** aggregates findings from all roles. Findings have severity levels (critical, major, minor) that affect the aggregate score. Required roles must pass for the overall verdict to pass.

**Failure routing** sends work back to an earlier stage with feedback from the review. `max_retries` prevents infinite loops.

#### Interactive workflow prompts

When a stage has `await_human: always` or `await_human: on_fail` (and the verdict fails), the workflow pauses and presents options in `ta shell`:

```
[workflow] Review stage paused — 2 findings need attention:
  1. Security: SQL injection risk (critical)
  2. Style: Inconsistent error format (minor)

Options: [1] proceed  [2] revise  [3] cancel
workflow> _
```

Respond via the daemon API:

```bash
# POST /api/workflow/:id/input
curl -X POST http://localhost:3001/api/workflow/<id>/input \
  -H 'Content-Type: application/json' \
  -d '{"decision": "proceed", "feedback": "Accepted with minor issue noted"}'
```

Valid decisions: `proceed`, `revise`, `cancel`.

#### Built-in templates

TA ships three workflow templates:

| Template | Stages | Use case |
|----------|--------|----------|
| `simple-review.yaml` | build → review | Quick build-and-review cycle |
| `milestone-review.yaml` | plan → build → review → approval | Full milestone with scorer |
| `security-audit.yaml` | scan → review → remediate | Security-focused audit |

Role definitions are in `templates/workflows/roles/` (engineer, reviewer, security-reviewer, planner, pm).

#### Framework adapters

For LangGraph or CrewAI users, TA ships adapter scripts that bridge the JSON-over-stdio protocol:

```bash
# LangGraph adapter
python templates/workflows/adapters/langraph_adapter.py

# CrewAI adapter
python templates/workflows/adapters/crewai_adapter.py
```

Configure a process-based engine in `.ta/config.yaml`:

```yaml
workflow:
  engine: process
  command: "python templates/workflows/adapters/langraph_adapter.py"
```

#### MCP tool

Orchestrator agents can manage workflows via the `ta_workflow` MCP tool:

```json
{"action": "start", "definition_path": "templates/workflows/simple-review.yaml"}
{"action": "status", "workflow_id": "abc-123"}
{"action": "list"}
{"action": "cancel", "workflow_id": "abc-123"}
{"action": "history", "workflow_id": "abc-123"}
```

### Creating Custom Workflows

You can define project-specific workflows as YAML files in `.ta/workflows/`. A workflow is an ordered pipeline of stages, each assigned to agent roles, with optional review gates and failure routing.

#### Step-by-step

1. Create the workflow file:

```yaml
# .ta/workflows/deploy.yaml
name: deploy
stages:
  - name: build
    roles: [engineer]
  - name: test
    depends_on: [build]
    roles: [tester]
  - name: review
    depends_on: [test]
    roles: [reviewer]
    review:
      reviewers: [security-reviewer]
      require_all: true
    on_fail:
      route_to: build
      max_retries: 2
    await_human: on_fail
  - name: deploy
    depends_on: [review]
    roles: [engineer]

roles:
  engineer:
    agent: claude-code
    prompt: "Build and deploy the feature"
  tester:
    agent: claude-code
    prompt: "Write and run tests for the implementation"
  reviewer:
    agent: claude-code
    prompt: "Review the code for correctness and security"
  security-reviewer:
    agent: claude-code
    prompt: "Audit for OWASP Top 10 vulnerabilities"

verdict:
  pass_threshold: 0.8
  required_pass: [security-reviewer]
```

2. Run it:

```bash
ta workflow start .ta/workflows/deploy.yaml
```

#### Workflow YAML reference

**Stages:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Unique stage identifier |
| `depends_on` | string[] | no | Stages that must complete first (topological sort) |
| `roles` | string[] | no | Roles that execute in parallel within this stage |
| `then` | string[] | no | Roles that execute sequentially after parallel roles |
| `review` | object | no | Review gate configuration |
| `on_fail` | object | no | Where to route on review failure |
| `await_human` | string | no | `always`, `never` (default), or `on_fail` |

**Roles:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent` | string | yes | Agent config name (e.g., `claude-code`, `codex`, or custom) |
| `prompt` | string | no | System prompt for this role |
| `constitution` | string | no | Path to constitution YAML |
| `framework` | string | no | Override framework detection |

**Review:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `reviewers` | string[] | `[]` | Roles that perform the review |
| `require_all` | bool | `true` | All reviewers must pass (vs any one) |

**Failure routing:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `route_to` | string | — | Stage to retry |
| `max_retries` | int | `3` | Maximum retries before workflow fails |

**Verdict scoring:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `pass_threshold` | float | `0.7` | Minimum aggregate score (0.0–1.0) |
| `required_pass` | string[] | `[]` | Roles that must pass regardless of aggregate |
| `scorer.agent` | string | — | Agent that synthesizes review verdicts |
| `scorer.prompt` | string | — | System prompt for the scorer |

#### Example workflows

**Code review pipeline** — build, then parallel code review + security review:

```yaml
name: code-review
stages:
  - name: implement
    roles: [engineer]
  - name: review
    depends_on: [implement]
    roles: [code-reviewer, security-reviewer]
    review:
      reviewers: [code-reviewer, security-reviewer]
    on_fail:
      route_to: implement
      max_retries: 2
roles:
  engineer:
    agent: claude-code
    prompt: "Implement the feature"
  code-reviewer:
    agent: claude-code
    prompt: "Review for correctness, readability, and test coverage"
  security-reviewer:
    agent: claude-code
    prompt: "Audit for security vulnerabilities (OWASP Top 10)"
```

**Multi-stage with human gate** — plan, build, review with mandatory human approval:

```yaml
name: milestone
stages:
  - name: plan
    roles: [planner]
    await_human: always
  - name: build
    depends_on: [plan]
    roles: [engineer]
  - name: review
    depends_on: [build]
    roles: [reviewer]
    await_human: always
roles:
  planner:
    agent: claude-code
    prompt: "Break the goal into implementation steps"
  engineer:
    agent: claude-code
    prompt: "Implement the plan"
  reviewer:
    agent: claude-code
    prompt: "Review the implementation"
```

### External Workflows & Agents

Share and reuse workflow definitions and agent configurations across projects by pulling them from external sources — registries, GitHub repos, or raw URLs.

#### Adding external workflows

```bash
# Pull a workflow from a registry
ta workflow add security-review --from registry:trustedautonomy/workflows

# Pull from a GitHub repo
ta workflow add deploy-pipeline --from gh:myorg/ta-workflows

# Pull from a raw URL
ta workflow add ci-pipeline --from https://example.com/workflows/ci.yaml

# List installed external workflows
ta workflow list --source external

# Update all pinned workflows to latest
ta workflow update --all

# Update a specific workflow
ta workflow update security-review

# Remove an external workflow
ta workflow remove security-review
```

External workflows are cached locally in `~/.ta/cache/workflows/` and version-pinned in `.ta/workflow-lock.yaml`. The lockfile records the source URL, SHA-256 checksum, and fetch timestamp for each entry.

#### Adding external agents

```bash
# Pull an agent config from a registry
ta agent add security-reviewer --from registry:trustedautonomy/agents

# Pull from a URL
ta agent add code-auditor --from https://example.com/agents/auditor.yaml

# List external agents
ta agent list --source external

# Remove an external agent
ta agent remove code-auditor
```

Agent configs are cached in `~/.ta/cache/agents/` with the same lockfile-based version pinning.

#### Publishing workflows

Package and publish your workflows for others to use:

```bash
# Publish a workflow to a registry
ta workflow publish my-workflow --registry trustedautonomy

# Bump the version before publishing
ta workflow publish my-workflow --bump minor
```

Publishing generates a `workflow-package.yaml` manifest if one doesn't exist:

```yaml
# workflow-package.yaml
name: my-workflow
version: 1.0.0
author: your-org
description: "Multi-stage deploy pipeline with review gates"
ta_version: ">=0.10.5"
files:
  - workflows/my-workflow.yaml
  - agents/deployer.yaml
  - policies/deploy-baseline.yaml
```

#### Source URL schemes

| Scheme | Example | Resolves to |
|--------|---------|-------------|
| `registry:` | `registry:org/name` | TA registry (future) |
| `gh:` | `gh:org/repo` | GitHub raw content |
| `https://` | `https://example.com/file.yaml` | Direct URL fetch |

### Press Release Generation

Generate a press-release-style announcement from your release notes:

```bash
# Configure a sample press release as the style template
ta release config set press_release_template ./samples/sample-press-release.md

# Generate a press release during release
ta release run --press-release

# Provide a custom prompt to guide the content
ta release run --press-release --prompt "Focus on the workflow engine improvements"
```

The agent reads `.release-draft.md` (or falls back to recent `git log`), matches the tone and structure of your template document, and produces a draft press release that goes through the normal TA review process.

### Multi-Language Plugin Builds

Channel plugins can now use any language — not just Rust. Add a `build_command` field to `channel.toml` to specify how to build your plugin:

```toml
# channel.toml for a Go plugin
name = "teams"
version = "0.1.0"
protocol = "jsonrpc"
transport = "stdio"
build_command = "go build -o ta-channel-teams ."
```

```toml
# channel.toml for a Python plugin (no build needed, install deps)
name = "webhook"
version = "0.1.0"
protocol = "jsonrpc"
transport = "stdio"
build_command = "pip install -e ."
```

When `ta plugin build` runs, it uses `build_command` if present, otherwise falls back to `cargo build --release` for Rust plugins. Non-Rust plugin directories are copied in their entirety (excluding `target/`, `node_modules/`, `__pycache__/`).

### Creating Custom Agent Profiles

Agent profiles control how TA launches and constrains an agent. Create profiles for different roles (auditor, planner, full developer) or different agent tools (Ollama, LangChain, custom CLI).

#### When to customize

| Scenario | What to change |
|----------|----------------|
| Use a different AI tool | `command`, `args_template` |
| Restrict agent to read-only | `alignment.bounded_actions`, `alignment.forbidden_actions` |
| Agent needs env vars (API keys, config) | `env` |
| Agent should see goal context in CLAUDE.md | `injects_context_file: true` |
| Agent needs auto-approved permissions | `injects_settings: true` |
| Interactive terminal session | `interactive` section |
| Multi-agent coordination | `alignment.coordination` |

#### Building a read-only auditor

An agent that can read your project but not write files or run commands:

```yaml
# .ta/agents/auditor.yaml
name: auditor
description: "Read-only code auditor — no write access"
command: claude
args_template:
  - "--allowedTools"
  - "Read,Grep,Glob,WebFetch,WebSearch"
  - "--system-prompt"
  - "{prompt}"
injects_context_file: false
injects_settings: false
env: {}

alignment:
  principal: "project-owner"
  autonomy_envelope:
    bounded_actions:
      - "fs_read"
    escalation_triggers: []
    forbidden_actions:
      - "fs_write_patch"
      - "fs_apply"
      - "shell_execute"
      - "network_external"
      - "credential_access"
  constitution: "default-v1"
```

#### Building a full developer agent

An agent with read/write file access and build tool execution:

```yaml
# .ta/agents/developer.yaml
name: developer
description: "Full developer agent with build tool access"
command: claude
args_template:
  - "{prompt}"
injects_context_file: true
injects_settings: true
env: {}

alignment:
  principal: "project-owner"
  autonomy_envelope:
    bounded_actions:
      - "fs_read"
      - "fs_write_patch"
      - "fs_apply"
      - "exec: npm test"
      - "exec: npm run build"
      - "exec: npm run lint"
    escalation_triggers:
      - "new_dependency"
      - "security_sensitive"
      - "breaking_change"
    forbidden_actions:
      - "network_external"
      - "credential_access"
  constitution: "default-v1"
  coordination:
    allowed_collaborators: ["auditor"]
    shared_resources: ["src/**", "tests/**"]
```

#### Building a non-Claude agent

Any CLI tool that accepts a prompt and writes to the filesystem:

```yaml
# .ta/agents/ollama-coder.yaml
name: ollama-coder
description: "Local Ollama model for code generation"
command: ollama
args_template:
  - "run"
  - "codellama"
  - "{prompt}"
injects_context_file: false
injects_settings: false
env:
  OLLAMA_HOST: "http://localhost:11434"

alignment:
  principal: "project-owner"
  autonomy_envelope:
    bounded_actions:
      - "fs_read"
      - "fs_write_patch"
    forbidden_actions:
      - "network_external"
      - "credential_access"
```

#### Using custom profiles

Reference your custom agent in goals or workflows:

```bash
# Use directly in a goal
ta run "Add input validation" --agent auditor

# Reference in a workflow role
roles:
  security-check:
    agent: auditor
    prompt: "Audit for injection vulnerabilities"
```

Agent configs are resolved in priority order: `.ta/agents/` (project) → `~/.config/ta/agents/` (user) → built-in defaults.

---

## Roadmap

### What's Done

TA has a working end-to-end workflow: staging isolation, agent wrapping, draft review with per-artifact approval, follow-up iterations, macro goals with inner-loop review, interactive sessions, plan tracking, release pipelines, behavioral drift detection, access constitutions, alignment profiles, decision observability, credential management, MCP tool call interception, web review UI, webhook review channels, persistent context memory with semantic search, session lifecycle management, unified policy configuration (6-layer cascade), resource mediation (extensible by URI scheme), pluggable channel registry, API mediation for MCP tool calls, agent-guided project setup, project template initialization, interactive developer loop (`ta dev`), extensible agent framework registry with auto-detection, daemon HTTP API with SSE events and agent session management, an interactive terminal shell (`ta shell`), and a pluggable workflow engine for multi-stage, multi-role orchestration with verdict scoring and human-in-the-loop interaction, multi-project office management, channel plugin system (Discord, Slack, Email), external workflow and agent definitions, release pipeline hardening with interactive mode, conversational project bootstrapping via interactive mode, external channel delivery, and multi-language plugin builds.

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
| v0.1 | Public preview and call for feedback | Deferred |
| v0.1.1 | Release automation and binary distribution | Deferred |
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
| v0.6.3 | Active memory injection (project-aware keys, phase tagging, smart context) | Done |

### v0.6 -- Platform Substrate

| Phase | Description | Status |
|-------|-------------|--------|
| v0.6.0 | Session & human control plane (TaSession, SessionManager, CLI commands) | Done |
| v0.6.1 | Unified policy config (PolicyDocument, PolicyCascade, PolicyContext) | Done |
| v0.6.2 | Resource mediation trait (ResourceMediator, FsMediator, MediatorRegistry) | Done |
| v0.6.3 | Active memory injection (project-aware keys, smart context injection) | Done |

### v0.7 -- Extensibility

| Phase | Description | Status |
|-------|-------------|--------|
| v0.7.0 | Channel registry (pluggable IO channels, ChannelFactory, ChannelRegistry) | Done |
| v0.7.1 | API mediator (MCP tool call staging via ResourceMediator) | Done |
| v0.7.2 | Agent-guided setup (`ta setup wizard/refine/show`) | Done |
| v0.7.3 | Project templates and `ta init` (5 built-in templates) | Done |
| v0.7.4 | Memory & config cleanup (backend toggle, guidance domain classification) | Done |
| v0.7.5 | Interactive session fixes & cross-platform release | Done |
| v0.7.6 | Interactive developer loop (`ta dev`) | Done |
| v0.7.7 | Agent framework registry & setup integration | Done |

### v0.8+ — Event System through Channels & Plugins

| Phase | Description | Status |
|-------|-------------|--------|
| v0.8.0 | Event system and subscription API | Done |
| v0.8.1 | Solution memory export | Done |
| v0.8.2 | Developer loop refinements and orchestrator wiring | Done |
| v0.9.0 | Distribution and packaging (Dockerfile, install.sh, PWA) | Done |
| v0.9.1 | Native Windows support (CI, cross-platform builds) | Done |
| v0.9.2 | Sandbox runner (command allowlisting, path escape detection) | Done |
| v0.9.3 | Dev loop access hardening (`--unrestricted`, audit, CallerMode) | Done |
| v0.9.4 | Orchestrator event wiring and gateway refactor | Done |
| v0.9.4.1 | Event emission plumbing fix | Done |
| v0.9.5 | Enhanced draft view output | Done |
| v0.9.5.1 | Goal lifecycle hygiene and orchestrator fixes | Done |
| v0.9.6 | Orchestrator API and goal-scoped agent tracking | Done |
| v0.9.7 | Daemon API expansion (HTTP API, SSE events, agent sessions) | Done |
| v0.9.8 | Interactive TA shell (`ta shell` REPL, daemon client) | Done |
| v0.9.8.1 | Auto-approval, lifecycle hygiene & operational polish | Done |
| v0.9.8.1.1 | Unified allow/deny list pattern | Done |
| v0.9.8.2 | Pluggable workflow engine & framework integration | Done |
| v0.9.8.3 | Full TUI shell (ratatui) | Done |
| v0.9.8.4 | VCS adapter abstraction & plugin architecture | Done |
| v0.9.9 | Conversational project bootstrapping (`ta new`) | Pending |
| v0.9.9.1 | Interactive mode core plumbing | Done |
| v0.9.9.2 | Shell TUI interactive mode | Done |
| v0.9.9.3 | `ta plan from <doc>` wrapper | Done |
| v0.9.9.4 | External channel delivery | Done |
| v0.9.9.5 | Workflow & agent authoring tooling | Done |
| v0.9.10 | Multi-project daemon & office configuration | Done |
| v0.10.0 | Gateway channel wiring & multi-channel routing | Done |
| v0.10.1 | Native Discord channel | Done |
| v0.10.2 | Channel plugin loading (multi-language) | Done |
| v0.10.2.1 | Refactor Discord channel to external plugin | Done |
| v0.10.2.2 | `ta plugin build` command | Done |
| v0.10.3 | Slack channel plugin | Done |
| v0.10.4 | Email channel plugin | Done |
| v0.10.5 | External workflow & agent definitions | Done |
| v0.10.6 | Release process hardening & interactive release flow | Done |
| v0.10.7 | Documentation review & consolidation | Done |
| v0.10.8 | Pre-draft verification gate | Done |
| v0.10.9 | Smart follow-up UX | Done |
| v0.10.10 | Daemon version guard | Done |

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
ta run "Redo the task"

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
