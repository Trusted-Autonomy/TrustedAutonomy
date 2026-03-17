<p align="center">
  <img src="images/Trusted Autonomy Icon Small.png" alt="Trusted Autonomy" width="180">
</p>

<h1 align="center">Trusted Autonomy</h1>

<p align="center">
  <img src="https://img.shields.io/badge/latest-v0.10.18--alpha-blue" alt="Latest">
  <img src="https://img.shields.io/badge/released-v0.10.18--alpha-green" alt="Released">
  <img src="https://img.shields.io/badge/license-Apache--2.0-orange" alt="License">
</p>

**Trusted Autonomy** is a local-first, Rust-based substrate for running autonomous AI agents **safely**, **reviewably**, and **without changing how agents behave**.

It is not an agent framework.  
It is not an orchestrator.  

It is a **trust and control plane** that sits underneath *any* agent or multi-agent system and ensures:

- agents can operate autonomously inside a defined charter
- all real-world effects are staged, reviewable, and auditable
- humans remain in control at meaningful boundaries (a draft at each milestones in simple English with detailed diffs for deep inspection)
- orchestration layers remain swappable and unaware of the substrate

![Agent changes are collected and presented at goal completion for approval](./product-vision-arch.svg)

---

## Why Trusted Autonomy

Today's agent tooling forces trade-offs forcing choices between security and convenience; many people opt for convenience. This shouldn't be the choice for tech and non-tech users alike. Default behavior is locked down requiring **constant permission prompts** (secure but annoying) or folder level access that stil requires many approvals with options to "always allow". An all too common alternative, or the result of many "yeah, always allow" choices is **full auto-approve** (convenient but risky). Users get tired of approving seemingly trivial actions, needing to constantly check and babysit their smart agents, and disable safeguards — many users without understanding the security consequences. Experienced users can configure fine-grained tool permissions in frameworks like Claude Code or Claude Flow, but doing this well requires knowing which actions are actually dangerous, how they interact, and what to scope — a level of security reasoning that shouldn't be a prerequisite for safe agent use. M

The standard answer is to run agents inside a VM with strict network controls and file access mappings. That works, but it requires a reasonable amount of technical sophistication to manage correctly, requires some customization per workflow, and requires some knowledge to track and review diffs - all of which is time not focused on the users goal. There are two classes of problem to solve: malicious intent (purposefully built Claude skills to cause harm as witnissed in recent Clawdbot security scans), and well-intentioned AI doing the wrong thing for the "right" reasons. VMs solve the former, humans have to engage to solve the latter. TA helps with both.

TA takes a different approach:

- **Agents work freely** in a staging copy of your project using their native tools. No permission prompts, no special APIs. TA is invisible to the agent.
- **Nothing reaches the real world** until you review and approve. TA diffs the staging copy against the original and presents changes as a Draft-style package — with the agent's rationale for each file.
- **You review at goal completion**, not per-action. This is the right abstraction: let the agent finish the task, then evaluate the result as a whole.
- **Selective approval**: accept the code changes, reject the config changes, discuss the rest. Not all-or-nothing.
- **Any agent, no lock-in**: Claude Code, Codex, Claude Flow, or any future agent works on top without modification.
- **Layers with VMs and sandboxes** when warranted — TA adds the semantic review layer that containers alone can't provide.

Today TA mediates filesystem changes. The same staging model in future releases extends to any state change actions including email drafts, database writes, API calls, and any external action — each becoming a reviewable artifact in the same Draft package.

> **[User Guide](docs/USAGE.md)** — quick start, workflows, configuration, and troubleshooting
> **[Roadmap](docs/USAGE.md#roadmap)** — what's done, what's next, and the vision
> **[PLAN.md](PLAN.md)** — detailed development plan with per-phase status
> **[WHY-TA-vs-VM.md](docs/WHY-TA-vs-VM.md)** — comparison of TA's approach vs VM sandboxing

---

## Core idea

> **Give agents everything they need to do the work — but nothing that can irreversibly affect the world without passing through a single, auditable gateway.**

Trusted Autonomy achieves this by:
- mediating *all* filesystem, network, execution, and external-service access through MCP tools
- defaulting **mutations** to *collection* (staging), not execution
- defaulting **capabilities** to deny unless explicitly granted
- representing each milestone within a goal as a **draft package** for review

---

## What this is (and is not)

### This **is**
- a capability-gated runtime substrate
- a policy-enforced MCP gateway
- a staging and review system for agent actions
- a foundation for high-autonomy workflows with human trust

### This is **not**
- an “Agent OS VM”
- a replacement for LangGraph, Claude Flow, CrewAI, etc.
- a UI-first product (comes later but independently)
- a monolithic orchestration framework

---

## Current status: v0.10.18-alpha

Under active development. See [PLAN.md](PLAN.md) for the full roadmap.

This is an alpha release for feedback. Please note:

- **Not production-ready.** Do not use for critical or irreversible operations.
- **The security model is not yet audited.** Sandbox isolation exists (command allowlisting, path escape detection) but has not been independently audited. Do not trust it with secrets or sensitive data without your own review.

If any of these are blockers for your use case, watch the repo — progress is tracked on the [roadmap](PLAN.md).

---

## Quick Start

See the [detailed Quick Start](#quick-start-5-minutes) below for full install + agent setup instructions, or jump to [docs/USAGE.md](docs/USAGE.md) for comprehensive usage guidance. Run `ta --help` for CLI reference.

---

## Design principles
- Normal environment illusion: tools feel like standard filesystem/network access, but all effects are mediated.
- Default collect (staged-by-default): changes accumulate as pending review artifacts (Draft package) rather than applying immediately.
- Capability boundary (default deny): agents can only perform actions explicitly granted by a signed capability manifest.
- Single chokepoint: all reads/writes and external effects flow through an MCP Gateway with policy enforcement and audit.
- Draft-per-milestone workflow: complex goals are decomposed into major steps, each producing a Draft package for approval.
- Replaceable orchestration: the substrate is the trust layer; planners/swarms are pluggable.

## Why MCP is the abstraction boundary

Trusted Autonomy uses **Model Context Protocol (MCP)** as its sole integration surface with agents.

This is intentional.

### MCP gives us:
- a standardized tool interface
- explicit, inspectable actions
- a single chokepoint for policy, audit, and transformation
- compatibility with existing and future agent frameworks

### MCP is treated as:
> **The agent’s “operating environment”, not an API to bypass.**

Agents do not know they are operating in a staged, policy-controlled system — and they do not need to.

---

## Filesystem abstraction (why it works this way)

### Design goal
Agents should be able to **read, write, and modify files normally**, without learning a new model — while all changes remain reviewable.

### How it works
- Agents interact with filesystem tools exposed via MCP
- Those tools operate on a **staging workspace** (isolated directory per goal today, sparse virtual file system in the future)
- Reads snapshot the original file; writes create diffs against the snapshot
- All writes become **ChangeSets with diffs**, bundled into Draft packages

```
Agent
  ↓
MCP ta_fs_read / ta_fs_write
  ↓
StagingWorkspace (isolated temp directory)
  ↓
ChangeSet → Diff → Draft Package → Human Review → Apply
```

### Why staging directories?
- cross-platform (Windows/macOS/Linux)
- no kernel drivers, FUSE mounts, or Git dependency
- native diff/rollback semantics
- binary changes can be summarized and hashed
- maps cleanly to Draft-style review
- each GoalRun gets complete isolation

### Why not a mounted VFS?
Kernel-level VFS (FUSE, sandboxfs, etc.) introduces:
- install friction
- permissions complexity
- platform inconsistencies

Those can be added later, but staging workspaces keep the system **portable and maintainable**.

![Current temp abstraction is a staging folder to get devs operational now](./dev-working-example.svg)

---

## Network & web access abstraction

### Design goal
Allow agents to fetch web content **without enabling prompt injection or uncontrolled exfiltration**.

### How it works
- Agents use MCP web tools (`web.fetch`, `web.search`)
- The gateway:
  - enforces allowlists and rate limits
  - sanitizes active content
  - labels provenance and trust level
  - treats fetched content as **data, never instructions**

### Why not raw network sockets?
Raw sockets:
- bypass policy
- bypass audit
- enable hidden side effects

If needed, a transparent local proxy can be added later — but MCP tools are the correct starting point for deterministic autonomy.

---

## Email, social media, databases: everything is a ChangeSet

### Unifying principle
> **Anything that changes the world is a staged artifact.**

This includes:
- emails
- social media posts
- database writes
- API mutations
- permissions changes

### How it works
Each connector implements:
- `read`
- `write_patch` / `create_draft`
- `preview`
- `commit` (gated)

So:
- an email is drafted, not sent
- a post is created, not published
- a DB mutation is recorded, not applied

All appear in the **same Draft package** alongside filesystem changes.

---

## “Default deny” vs “default collect”

Trusted Autonomy uses **two defaults**, intentionally separated:

### Capability default: **deny**
If an agent does not have an explicit capability, the gateway rejects the action.

This is the hard security boundary.

### Mutation default: **collect**
If an agent *does* have permission to write:
- the write is staged
- the change is collected
- a Draft package is generated

Commit/send/post requires explicit approval or a narrowly scoped write-through capability.

This allows agents to “just work” inside their charter without risk.

---

## Execution environment (why just-bash exists)

### Design goal
Allow agents to:
- search
- format
- run tests
- scaffold code

…without giving them a real shell or OS.

### Approach
- default execution uses a **just-bash-style emulated shell**
- commands are allowlisted
- filesystem access is limited to the workspace
- transcripts are hashed and audited

For workloads requiring real runtimes, isolated containers or microVMs can be added later — still behind the same MCP gateway.

---

## Nix: why and how it’s used

### Why Nix is included
- reproducible Rust toolchains
- deterministic builds in CI
- consistent dev environments

### Why Nix is **not required** for users
- Nix has a learning curve
- some environments prohibit it
- bundling Nix into desktop apps is heavy

### Strategy
- **Use Nix for developers and CI**
- **Ship bundled binaries for end users**
- **Produce OCI images for cloud deployment**

Nix improves correctness without becoming a dependency tax.

---

## Compatibility with existing agent systems

Trusted Autonomy is designed so that:
- Claude Code (tested)
- Codex
- LangGraph
- claude-flow (tested and my default operating model)
- Ollama-based agents
- future orchestration layers

…can all run **unchanged** on top.

They see:
- a normal workspace
- normal tools
- normal outputs

They do **not** need to know:
- staging exists
- policies exist
- approvals exist

That separation is the core architectural property.

---

## Why Draft-style milestones matter

Continuous human oversight destroys autonomy.  
Zero oversight destroys trust.

Trusted Autonomy enforces review at **meaningful boundaries**:
- when a milestone is complete
- when external effects are requested
- when risk increases

This mirrors how high-trust engineering systems already work.

---

## Future extensions (by design)

- continuous security auditor agents
- automatic least-privilege recommendations
- anomaly detection over audit logs
- richer diff renderers (spreadsheets, docs, binaries)
- multi-tenant cloud deployments
- stronger runtime isolation tiers

These are additive — not architectural rewrites.

---

## Quick Start (5 minutes)

### 1. Install TA from source

```bash
# Clone the repo
git clone https://github.com/Trusted-Autonomy/TrustedAutonomy.git
cd TrustedAutonomy

# Build (pick one):

# Option A — With Nix (reproducible, recommended for contributors)
nix develop --command cargo build --release -p ta-cli

# Option B — Without Nix (just needs Rust)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh  # if needed
cargo build --release -p ta-cli
```

The binary lands at `target/release/ta`. Add it to your PATH:

```bash
# Add to your shell profile (~/.bashrc, ~/.zshrc, etc.)
export PATH="$HOME/path-to/ta/target/release:$PATH"
```

### 2. Install an agent

TA works with any coding agent. Pick one (or more):

**Claude Code** (recommended)
```bash
# Native install (preferred)
curl -fsSL https://claude.ai/install.sh | bash

# Or via npm
npm install -g @anthropic-ai/claude-code
```
Requires an [Anthropic API key](https://console.anthropic.com/) or a Claude Max plan.

**Claude Flow** (multi-agent orchestration on top of Claude Code)
```bash
# Requires Node.js 20+ and Claude Code installed first
npx claude-flow@alpha init --wizard

# Or full install
curl -fsSL https://cdn.jsdelivr.net/gh/ruvnet/claude-flow@main/scripts/install.sh | bash -s -- --full
```
See the [claude-flow repo](https://github.com/ruvnet/claude-flow) for configuration options.

**OpenAI Codex CLI**
```bash
npm install -g @openai/codex
```
Requires an [OpenAI API key](https://platform.openai.com/api-keys) or ChatGPT Plus/Pro subscription.

### API key configuration

You can set API keys globally or per-project.

**Global** (all projects):
```bash
# Add to ~/.bashrc or ~/.zshrc
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
```

**Per-project** (recommended — uses [direnv](https://direnv.net/)):
```bash
# Copy the example and add your key
cp .envrc.example .envrc
# Edit .envrc — uncomment and set your API key
direnv allow
```

The key activates when you `cd` into the project and deactivates when you leave. The `.envrc` also contains `use flake` which auto-activates the Nix dev shell (cargo, rustc, etc.).

**Important:** `.envrc` is already in `.gitignore` — your keys will never be committed. Only `.envrc.example` (with placeholder values) is tracked in git.

### 3. Run your first mediated task

```bash
cd your-project/

# One command: create staging copy → launch agent → build Draft on exit
ta run "Fix the auth bug"

# Uses Claude Code by default. For other agents:
ta run "Fix the auth bug" --agent claude-flow

# TA copies your project to .ta/staging/, injects context into CLAUDE.md,
# launches the agent in the staging copy. Agent works normally.
# When the agent exits, TA diffs staging vs source and builds a Draft package.

# Review what the agent did
ta draft list
ta draft view <package-id>

# Approve and apply changes back to your project
ta draft approve <package-id>
ta draft apply <package-id> --git-commit
```

That's it. The agent never knew it was in a staging workspace.

> **For detailed usage, configuration options, and troubleshooting, see [docs/USAGE.md](docs/USAGE.md)** or run `ta --help`.
>
> **New in v0.3.1.2:** Interactive session orchestration — see [docs/interactive-sessions.md](docs/interactive-sessions.md) for the full guide.

---

## How It Works

```
Your Project                     Staging Copy (.ta/staging/)
     |                                    |
     |-- ta run "task" -------->|  (full copy, minus build artifacts)
     |                                    |
     |                              Agent works here
     |                              (reads, writes, tests — normal tools)
     |                                    |
     |                              ta draft build --latest
     |                                    |
     |<--- ta draft apply <id> -------------|  (only approved changes copied back)
     |
   Your project updated + optional git commit
```

TA is invisible to the agent. It works by:
1. Copying your project to a staging directory (with `.taignore` to skip build artifacts)
2. Letting the agent work normally in the copy using its native tools
3. Diffing the staging copy against the original to create a Draft package
4. Letting you review, approve, and apply changes back

---

## Step-by-Step Workflow

### Manual workflow (any agent)

```bash
# 1. Start a goal — creates a staging copy of your project
ta goal start "Fix the auth bug"

# 2. Note the staging path and goal ID from the output, then enter staging
cd .ta/staging/<goal-id>/

# 3. Launch your agent (Claude Code, Codex, or any tool)
claude    # or: codex, or any tool

# 4. Agent works normally — reads, writes, runs tests, etc.
#    It doesn't know it's in a staging workspace.
#    When done, exit the agent.

# 5. Build a Draft package from the diff
ta draft build <goal-id> --summary "Fixed the auth bug"

# 6. Review the changes
ta draft view <package-id>

# 7. Approve and apply back to your project (with optional git commit)
ta draft approve <package-id>
ta draft apply <package-id> --git-commit
```

### One-command shortcut

`ta run` wraps the manual steps into a single command:

```bash
ta run "Fix the auth bug"
# Uses Claude Code by default. For other agents:
ta run "Fix the auth bug" --agent claude-flow

# Follow-up on a previous goal to iterate on feedback:
ta run "Address config validation" --follow-up
# Automatically links to the most recent goal, includes parent context

# Follow-up with detailed review notes:
ta run --follow-up --objective-file review-notes.md
# Then review + approve + apply as above.
```

### Using with Claude Flow (detailed setup)

[Claude Flow](https://github.com/ruvnet/claude-flow) adds multi-agent orchestration on top of Claude Code. It can run as an MCP server (giving Claude Code extra tools for swarm coordination, memory, and task routing) or launch Claude Code processes directly via its hive-mind command. Both approaches work inside a TA staging workspace.

**Prerequisites:**
- Node.js 20+, npm 9+
- Claude Code installed (`npm install -g @anthropic-ai/claude-code`)
- An `ANTHROPIC_API_KEY` set in your environment

#### Step 1: Install claude-flow

```bash
# Option A: Use via npx (no global install needed)
npx claude-flow@alpha --version

# Option B: Global install
npm install -g claude-flow@alpha
```

#### Step 2: Register claude-flow as an MCP server for Claude Code

This is the primary integration — it gives Claude Code access to claude-flow's swarm, memory, and task-routing tools:

```bash
claude mcp add claude-flow -- npx claude-flow@alpha mcp start
```

Or add it to your project's `.mcp.json` (TA will copy this into staging):

```json
{
  "mcpServers": {
    "claude-flow": {
      "command": "npx",
      "args": ["claude-flow@alpha", "mcp", "start"]
    }
  }
}
```

#### Step 3: Create a TA goal and staging workspace

```bash
cd your-project/
ta goal start "Refactor auth system"
# Note the goal ID from the output
```

#### Step 4: Launch — pick one approach

**Approach A: TA run + MCP tools (recommended)**

Use `ta run` to launch Claude Code inside the staging workspace. Because claude-flow is registered as an MCP server, Claude Code can call swarm/memory/task tools automatically:

```bash
ta run "Refactor auth system"

# Claude Code (default agent) launches in .ta/staging/<goal-id>/
# It can use mcp__claude-flow__swarm_init, mcp__claude-flow__task_orchestrate,
# etc. alongside its normal tools.
# When Claude exits, TA diffs and builds the Draft package.
```

**Approach B: Hive-mind (spawns Claude Code directly)**

Claude-flow's `hive-mind` command spawns a real Claude Code process with the task prompt. You must initialize the hive-mind first:

```bash
cd .ta/staging/<goal-id>/
npx claude-flow@alpha hive-mind init       # required — sets up the hive mind
npx claude-flow@alpha hive-mind spawn "Refactor auth system" --claude
# Spawns Claude Code with --dangerously-skip-permissions in the staging dir.
# When done, return to project root and build the Draft:
cd your-project/
ta draft build <goal-id> --summary "Auth system refactored"
```

> **Note:** `ta run "task" --agent claude-flow` handles `hive-mind init` automatically.

**Approach C: Headless swarm (no interactive session)**

For fully automated runs:

```bash
cd .ta/staging/<goal-id>/
npx claude-flow@alpha swarm "Refactor auth system" --headless
# Runs to completion without interaction.
cd your-project/
ta draft build <goal-id> --summary "Auth system refactored by swarm"
```

#### Step 5: Review and apply

```bash
ta draft list
ta draft view <package-id>
ta draft approve <package-id>
ta draft apply <package-id> --git-commit
```

#### CLAUDE.md conflict note

Both TA and claude-flow write to `CLAUDE.md`. TA injects goal context and plan progress into CLAUDE.md when launching via `ta run`. If you use claude-flow's `init` command inside a staging directory, use `--skip-claude` to avoid overwriting TA's injected context:

```bash
cd .ta/staging/<goal-id>/
npx claude-flow@alpha init --minimal --skip-claude
# Sets up .claude-flow/ config without touching CLAUDE.md
```

If you need claude-flow's CLAUDE.md content (governance rules, skill definitions), append it to the existing file rather than replacing it:

```bash
npx claude-flow@alpha init --minimal --skip-claude
# Then manually merge any needed claude-flow instructions into CLAUDE.md
```

#### Environment variables

```bash
# Required
export ANTHROPIC_API_KEY="sk-ant-..."

# Optional claude-flow tuning
export CLAUDE_FLOW_LOG_LEVEL=info          # debug, info, warn, error
export CLAUDE_FLOW_MAX_AGENTS=12           # max concurrent agents
export CLAUDE_FLOW_NON_INTERACTIVE=true    # for CI/headless runs
export CLAUDE_FLOW_MEMORY_BACKEND=hybrid   # memory persistence backend
```

### Using with OpenAI Codex

```bash
# Same workflow — TA doesn't care which agent you use
ta goal start "Add input validation"
cd .ta/staging/<goal-id>/
codex    # Codex works in the staging copy like normal
# Exit Codex, then build/review/apply as above
```

### Exclude patterns (.taignore)

By default, TA excludes common build artifacts from the staging copy (`target/`, `node_modules/`, `__pycache__/`, etc.) to keep copies fast. To customize, create a `.taignore` file in your project root:

```
# .taignore — one pattern per line
# Lines starting with # are comments

# Rust
target/

# Custom project-specific excludes
large-data/
*.sqlite
```

If no `.taignore` exists, sensible defaults are used. The `.ta/` directory is always excluded.

---

## Review Workflow

```bash
ta draft list                            # List pending PRs
ta draft view <package-id>               # View details + diffs
ta draft approve <package-id>            # Approve
ta draft deny <package-id> --reason "x"  # Deny with reason
ta draft apply <package-id> --git-commit # Apply + commit
ta draft apply <package-id> --submit     # Apply + commit + push + open Draft

# Selective approval (approve some, reject/discuss others)
ta draft apply <id> --approve "src/**/*.rs" --reject "*.test.rs" --discuss "config/*"
ta draft apply <id> --approve all        # Approve everything
ta draft apply <id> --approve rest       # Approve everything not explicitly matched
```

### Workflow Automation (`.ta/workflow.toml`)

By default, `ta draft apply` only copies files back to your project. To automatically commit, push, and open a draf set of changes (e.g. a pull request in GIT terminology), you can either pass CLI flags each time or create a `.ta/workflow.toml` config file to set your preferences once.

**Option A: CLI flags (no config needed)**

```bash
ta draft apply <id> --git-commit         # Commit only
ta draft apply <id> --git-push           # Commit + push (implies --git-commit)
ta draft apply <id> --submit             # Commit + push + open Draft
```

**Option B: Config file (set once, always active)**

Create `.ta/workflow.toml` in your project root:

```toml
[submit]
adapter = "git"          # "git" or "none" (default: "none")
auto_commit = true       # Commit on every ta draft apply
auto_push = true         # Push after commit
auto_review = true       # Open a Draft after push

[submit.git]
branch_prefix = "ta/"    # Branch naming: ta/<goal-slug>
target_branch = "main"   # Draft target branch
merge_strategy = "squash" # squash, merge, or rebase
remote = "origin"        # Git remote name
# pr_template = ".ta/draft-template.md"  # Optional Draft body template
```

With this config, `ta draft apply <id>` automatically creates a feature branch, commits, pushes, and opens a GitHub Draft — no flags needed.

**How it works:**
- Without `.ta/workflow.toml`: all `auto_*` settings default to `false`. You must use `--git-commit`, `--git-push`, or `--submit` explicitly.
- With `.ta/workflow.toml`: the `auto_*` settings activate on every `ta draft apply`. CLI flags always work as overrides on top.
- The git adapter auto-detects git repos. If `adapter = "none"` but you're in a git repo and pass `--git-commit`, it uses git automatically.
- **Draft creation requires the [GitHub CLI](https://cli.github.com/)** (`gh`). Install it and run `gh auth login` once.

**Quick setup from examples:**

```bash
mkdir -p .ta
cp examples/workflow.toml .ta/workflow.toml
cp examples/draft-template.md .ta/draft-template.md
# Edit .ta/workflow.toml to match your project (branch prefix, target branch, etc.)
```

The `examples/` directory ships with commented reference configs. The `.ta/` directory is gitignored — your config stays local.

### Iterative Review with Follow-Up Goals

When you need to iterate on feedback or address discuss items:

```bash
# Mark items for discussion during initial review
ta draft apply <id> --approve "src/**" --discuss "config/*"

# Start a follow-up goal to address feedback
ta run "Fix config validation" --follow-up
# - Automatically links to most recent goal
# - Agent receives parent context (what was approved/rejected/discussed)
# - New Draft supersedes parent Draft if parent wasn't applied yet

# Follow up on a specific goal (ID prefix matching)
ta run "Address security feedback" --follow-up abc123

# Provide detailed review notes from a file
ta run --follow-up --objective-file review-feedback.md

# View goal chain
ta goal list  # Shows parent relationships: "title (→ parent_id)"
ta draft list    # Shows superseded PRs: "superseded (abc12345)"
```

---

## Alternative: MCP-Native Tools

For agents that support MCP tool integration directly, TA can also expose tools via MCP instead of the overlay approach:

```bash
# Install the Claude Code adapter (generates .mcp.json + .ta/config.toml)
ta adapter install claude-code

# Start Claude Code — TA tools appear alongside built-in tools
claude
```

| Tool | What it does |
|------|-------------|
| `ta_goal_start` | Create a GoalRun (allocates staging workspace + capabilities) |
| `ta_goal_list` | List all GoalRuns |
| `ta_goal_status` | Check GoalRun state |
| `ta_goal_inner` | Create an inner-loop sub-goal within a macro goal |
| `ta_fs_read` | Read a source file (snapshots the original) |
| `ta_fs_write` | Write to staging (creates a ChangeSet with diff) |
| `ta_fs_list` | List staged files |
| `ta_fs_diff` | Show diff for a staged file |
| `ta_draft` | Bundle staged changes into a Draft package for review |
| `ta_pr_build` | Alias for `ta_draft` (backward compatibility) |
| `ta_pr_status` | Alias for draft status (backward compatibility) |
| `ta_context` | Retrieve project context and configuration |
| `ta_ask_human` | Request human input during agent execution |
| `ta_plan` | Query and update plan phase status |
| `ta_workflow` | Manage workflow definitions and execution |
| `ta_agent_status` | Check agent health and configuration |
| `ta_event_subscribe` | Subscribe to real-time event streams |

---

## Architecture

```
Transparent overlay mode (recommended):
  Agent works in staging copy (native tools)
    → TA diffs staging vs source
    → Draft Package → Human Review → Approve → Apply

MCP-native mode:
  Agent (Claude Code / Codex / any MCP client)
    |  MCP stdio
    v
  TaGatewayServer (ta-mcp-gateway)
    |-- PolicyEngine (ta-policy)        default deny
    |-- StagingWorkspace (ta-workspace)  all writes staged
    |-- ChangeStore (ta-workspace)       JSONL persistence
    |-- AuditLog (ta-audit)             tamper-evident trail
    '-- GoalRunStore (ta-goal)          lifecycle management
    |
    v
  Draft Package → Human Review (CLI) → Approve → Apply
```

Multiple agents can work simultaneously. Each gets an isolated GoalRun with its own staging workspace and capabilities.

---

## Project Structure

```
crates/
  ta-audit/             Append-only event log + SHA-256 hash chain
  ta-changeset/         Draft package model, review channels, channel registry
  ta-policy/            Capability engine + policy document cascade
  ta-workspace/         Staging + overlay workspace manager
  ta-goal/              GoalRun lifecycle state machine + event dispatch
  ta-mcp-gateway/       MCP server — tool handlers, policy enforcement
  ta-daemon/            Daemon binary (HTTP API, SSE events, shell client)
  ta-submit/            SubmitAdapter trait + git implementation
  ta-memory/            Persistent context memory (file + ruvector backends)
  ta-credentials/       Credential vault + identity broker
  ta-sandbox/           Command allowlisting + path escape detection
  ta-events/            Event types, schemas, channel question protocol
  ta-workflow/          Workflow engine, stage orchestration, verdict scoring
  ta-connectors/
    fs/                 Filesystem connector: staging + diffs + apply
    web/                Web fetch connector
    mock-drive/         Mock Google Drive
    mock-gmail/         Mock Gmail
apps/
  ta-cli/               CLI: goals, drafts, run, plan, shell, dev, release, plugins, workflows
plugins/
  ta-channel-discord/   Discord channel plugin (JSON-over-stdio)
  ta-channel-slack/     Slack channel plugin (JSON-over-stdio)
  ta-channel-email/     Email channel plugin (JSON-over-stdio)
```

---

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md) for latest details

### Prerequisites

**Option A: Using Nix (recommended)**

```bash
# 1. Install Nix (Determinate Systems installer — adds shell integration automatically)
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

# 2. Open a NEW terminal (so nix-daemon.sh is sourced), then:
cd path/to/TrustedAutonomy
nix develop   # enters dev shell with Rust 1.93, cargo, just, clippy, rustfmt
```

> **Note:** Nix provides `cargo`, `rustc`, `clippy`, and `rustfmt` inside the dev shell only — they are not installed globally. You must either run `nix develop` first or use the `./dev` wrapper script.

**Option A+ (automatic dev shell with direnv)**

If you install [direnv](https://direnv.net/), the dev shell activates automatically when you `cd` into the project:

```bash
# macOS
brew install direnv

# Add to ~/.zshrc (or ~/.bashrc):
eval "$(direnv hook zsh)"

# Then allow the project's .envrc:
cd path/to/TrustedAutonomy
direnv allow
# Now cargo, rustc, just, etc. are available automatically in this directory
```

**Option B: Without Nix**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# macOS: brew install openssl pkg-config
# Ubuntu: apt install libssl-dev pkg-config
```

### Build and test

```bash
# Inside nix develop (or with direnv active):
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Or without entering the dev shell (one-shot wrapper):
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
```

---

## What's Implemented

- **Transparent overlay mediation** — agents work in staging copies using native tools, TA is invisible
- **Selective approval** — `--approve "src/**" --reject "*.test.rs" --discuss "config/*"` with dependency warnings
- **Concurrent session conflict detection** — detects source changes during active goals, prevents stale overwrites
- **Interactive session orchestration** — `ta run --interactive` for tracked human-agent sessions with lifecycle management. See [docs/interactive-sessions.md](docs/interactive-sessions.md)
- **External diff routing** — route binary/media files to external tools (`*.uasset` to Unreal, `*.png` to image diff, etc.)
- **YAML agent configs** — discoverable config files for any agent framework (`.ta/agents/`, `~/.config/ta/agents/`)
- **Settings injection** — auto-configures agent permissions (replaces `--dangerously-skip-permissions`)
- **Follow-up goals** — `--follow-up` to iterate on review feedback with full parent context injection
- **Submit adapters** — pluggable VCS integration (git commit/push/Draft, or no-VCS file copy)
- **Multi-stage workflow engine** — pluggable adapters, stage orchestration, verdict scoring
- **Web shell** — responsive browser-based shell (`ta shell`) with conversation chaining, live agent streaming, and full command routing. Terminal TUI available via `ta shell --tui`
- **Developer loop** — `ta dev` for iterative development with automatic staging and review
- **Channel plugin system** — out-of-process review channels (Discord, Slack, Email) via JSON-over-stdio protocol
- **Multi-project office management** — `ta office` for managing multiple projects under a single daemon
- **External workflow and agent definitions** — install from registry, GitHub, or URL sources (`ta workflow add`, `ta agent add`)
- **Release pipeline** — `ta release run` with validation, interactive mode, and multi-stage workflow templates
- **Unified garbage collection** — `ta gc` for cleaning up stale staging workspaces and old drafts
- **Project status dashboard** — `ta status` for at-a-glance project health
- **Conversational project bootstrapping** — `ta new` for guided project setup
- **External channel delivery** — route review questions to external services
- **Multi-language plugin builds** — `ta plugin build` for compiling channel plugins from source
- **Plan-from-document generation** — `ta plan from <doc>` to generate PLAN.md from requirements docs
- **Persistent context memory** — file and ruvector backends with semantic search
- **Credential vault and identity broker** — `ta credentials` for managing agent secrets
- **Sandbox runner** — command allowlisting and path escape detection
- **Event system** — subscription API with SSE streaming and event-driven hooks
- Append-only audit log with SHA-256 hash chain
- Default-deny capability engine with glob pattern matching and policy document cascade
- Per-artifact review model (disposition, dependencies, rationale)
- URI-aware pattern matching (scheme-scoped safety — `src/**` can't match `gmail://`)
- MCP server with policy enforcement and tool call interception
- CLI: `goal`, `draft`, `run`, `plan`, `context`, `credentials`, `events`, `token`, `dev`, `setup`, `init`, `agent`, `adapter`, `release`, `shell`, `office`, `plugin`, `workflow`, `policy`, `config`, `gc`, `status`, `serve`
- Plan tracking (`ta plan list/status`, auto-update on `ta draft apply`)

---

## License

Apache 2.0

---

## Philosophy (tl;dr)

> Autonomy is not about removing humans.
>  
> It’s about **moving human involvement to the right abstraction layer**.

Trusted Autonomy exists to make that layer explicit, enforceable, and trustworthy.
