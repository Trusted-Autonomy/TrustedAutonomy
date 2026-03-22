# Trusted Autonomy -- User Guide

**Version**: 0.13.1-alpha.1

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

**Option A -- Installer (recommended)**

*macOS*: Download `ta-<version>-macos.dmg` from the [latest release](https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest), open it, and run the installer. `ta` and `ta-daemon` are installed to `/usr/local/bin/`.

*Windows*: Download `ta-<version>-x86_64-pc-windows-msvc.msi` from the [latest release](https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest) and run it. `ta` and `ta-daemon` are installed to `%ProgramFiles%\TrustedAutonomy\` and added to your PATH automatically. An uninstaller is registered in Add/Remove Programs.

**Option B -- One-line installer (macOS / Linux)**

```bash
curl -fsSL https://raw.githubusercontent.com/Trusted-Autonomy/TrustedAutonomy/main/install.sh | bash
```

Set a specific version: `TA_VERSION=v0.10.12-alpha curl -fsSL ... | bash`

**Option C -- Binary download (manual)**

Each release archive contains two binaries: `ta` (the CLI) and `ta-daemon` (the background daemon). Both must be available — `ta` spawns `ta-daemon` as a sibling process, looking for it next to the `ta` binary first, then falling back to `$PATH`.

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest/download/ta-aarch64-apple-darwin.tar.gz
tar xzf ta-aarch64-apple-darwin.tar.gz
sudo cp ta ta-daemon /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest/download/ta-x86_64-apple-darwin.tar.gz
tar xzf ta-x86_64-apple-darwin.tar.gz
sudo cp ta ta-daemon /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest/download/ta-x86_64-unknown-linux-musl.tar.gz
tar xzf ta-x86_64-unknown-linux-musl.tar.gz
sudo cp ta ta-daemon /usr/local/bin/

# Windows (x86_64) — manual
# Download ta-x86_64-pc-windows-msvc.zip from the latest release
# Extract ta.exe and ta-daemon.exe into the same directory, then add that directory to your PATH
```

> **Note**: If `ta-daemon` is not found, commands that require the daemon (e.g. `ta daemon start`, `ta run`, `ta shell`) will fail with a "daemon binary not found" error. Ensure both binaries are on your `$PATH` or in the same directory.

#### Windows platform notes

On Windows, `ta daemon start`, `ta run`, and all non-interactive commands work normally. The interactive shell (`ta shell`) uses a Unix PTY and **is not available on Windows**. Use `ta run` for agent-driven goals and review drafts with `ta draft view`/`ta draft apply` on Windows.

```powershell
# Run a goal (works on Windows)
ta run "Fix the authentication bug"

# Review and apply the draft (works on Windows)
ta draft list
ta draft view <id>
ta draft apply <id>
```

**Option D -- Docker** *(Coming Soon)*

```bash
docker pull ghcr.io/trustedautonomy/ta:latest
docker run -it -v $(pwd):/workspace ta --help
```

**Option E -- Cargo install**

```bash
cargo install ta-cli  # coming soon — not yet published
```

### System Requirements

| Platform | Min RAM | Recommended | Disk (TA binary) | Staging disk |
|---|---|---|---|---|
| macOS (Apple Silicon) | 8 GB | 16 GB | ~15 MB | 1–5 GB per active goal |
| macOS (Intel) | 8 GB | 16 GB | ~15 MB | 1–5 GB per active goal |
| Linux x86_64 | 4 GB | 8 GB | ~12 MB | 1–5 GB per active goal |
| Windows x86_64 | 8 GB | 16 GB | ~15 MB | 1–5 GB per active goal |

Staging disk usage depends on project size. A typical Rust workspace (~500 MB with `target/`) uses ~600 MB per active goal. Use `ta gc` to reclaim staging space.

#### Agent Framework Requirements

| Framework | Min RAM | Notes |
|---|---|---|
| Claude Code (claude-sonnet-4-6) | 8 GB | Requires `ANTHROPIC_API_KEY`; network access to `api.anthropic.com` |
| Claude Code (claude-opus-4-6) | 8 GB | Higher quality, slower; same API key + network |
| Codex CLI | 8 GB | Requires `OPENAI_API_KEY`; network access to `api.openai.com` |
| Local model (Ollama, v0.13.8+) | 16 GB | 7B models need ~8 GB VRAM or ~12 GB RAM (CPU fallback); 70B needs ~40 GB RAM |

**Option E -- Nix**

```bash
nix run github:Trusted-Autonomy/TrustedAutonomy
```

**Option F -- Build from source**

```bash
git clone https://github.com/Trusted-Autonomy/TrustedAutonomy.git
cd TrustedAutonomy
./dev cargo build --workspace --release
# Binary is at target/release/ta
```

#### Platform packaging

After building from source, create platform-native packages with icons:

```bash
# macOS — creates TrustedAutonomy.app with icon
just build
just package-macos
# Output: target/TrustedAutonomy.app/

# Linux — install desktop entry and icons
just build
just package-linux
# Installs to ~/.local/bin, ~/.local/share/icons, ~/.local/share/applications

# Linux — install to system prefix
just package-linux PREFIX=/usr/local
```

To regenerate all icon formats from the master 1024px PNG:

```bash
just icons
# Generates: PNG sizes (16-512), ta.ico (Windows), ta.icns (macOS)
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
ta init run --detect
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

Start the shell (auto-starts the daemon if needed):

```bash
ta shell                        # Starts daemon if needed, opens interactive shell
```

Or manage the daemon explicitly:

```bash
ta daemon start                 # Start the daemon in the background
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
ta shell                                # Starts daemon if needed, opens shell

# Or manage daemon explicitly:
# ta daemon start                       # Background daemon on port 7700
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

**Pasting large text**: The shell automatically compacts large pastes (over 500 chars or 10 lines). Instead of flooding the input with raw text, you'll see an indicator:

```
ta> [Pasted 2,847 chars / 47 lines — Tab to preview, Esc to cancel]
```

- Press **Tab** to toggle an inline preview of the first few lines.
- Press **Enter** to send the full paste text (combined with anything you typed before pasting).
- Press **Esc** or **Ctrl-C** to discard the paste.

---

## Core Concepts

### The Staging Model

TA creates an isolated copy of your project (the *virtual workspace*) for every goal. The agent works inside this copy using its native tools. TA is invisible to the agent -- it does not know TA exists. When the agent finishes, TA diffs the workspace against the original and packages the differences into a reviewable draft.

This means:
- Your source files are never modified until you explicitly apply a draft.
- Multiple goals can run concurrently without interfering with each other.
- If something goes wrong, you reject the draft and start over.

**Copy-on-write staging (macOS/Linux)**: On APFS (macOS) and Btrfs (Linux), TA creates staging workspaces using the kernel's native copy-on-write mechanism. The staging copy appears instantly and consumes no additional disk space until the agent actually modifies a file. On other filesystems (ext4, network mounts, etc.), TA falls back to a regular byte-for-byte copy. The strategy is detected automatically — no configuration needed. Staging creation timing and file counts are logged to help diagnose performance issues.

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
ta draft approve <id>           # Mark as approved (--as <identity> for multi-party)
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

# Full diffs included (renders colored unified diffs from changeset store)
ta draft view <id> --detail full

# Machine-readable JSON output
ta draft view <id> --json
```

Agents can populate the Design Decisions section by passing an `alternatives` array to the `ta_pr_build` MCP tool. Each entry has `option`, `rationale`, and `chosen` fields.

### Agents

TA wraps any agent framework. Out of the box it supports:
- **Claude Code** (default) -- Anthropic's coding agent
- **Codex** -- OpenAI's coding agent (`OPENAI_API_KEY` required)
- **Claude Flow** -- multi-agent swarm orchestration
- **Ollama** -- generic local model agent (`ta-agent-ollama` required)

Use `--agent` to select a framework for a specific goal:

```bash
ta run "Fix the bug" --agent codex
ta run "Write tests" --agent claude-flow
```

#### List available frameworks

```bash
ta agent frameworks
ta agent list --frameworks   # alias
```

#### Show framework details

```bash
ta agent info codex
ta agent info claude-flow
```

#### Add a custom framework

Create a TOML manifest in `.ta/agents/` (project-level) or `~/.config/ta/agents/` (user-level):

```toml
# .ta/agents/my-agent.toml
name        = "my-agent"
version     = "1.0.0"
description = "My custom agent backend"
command     = "my-agent-bin"
args        = ["--headless"]
context_file = "CLAUDE.md"
```

Validate with:

```bash
ta agent framework-validate .ta/agents/my-agent.toml
```

Then use it:

```bash
ta run "Fix the bug" --agent my-agent
```

You can also add YAML agent configs (see [Agent Configuration](#agent-configuration)).

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

When a VCS adapter is detected (e.g., Git), `ta draft apply` automatically runs the full submit workflow (commit + push + PR). Use `--no-submit` to copy files only:

```bash
ta draft apply <draft-id>              # auto-submits when VCS detected
ta draft apply <draft-id> --no-submit  # copy files only, no VCS ops
ta draft apply <draft-id> --dry-run    # preview what would happen
```

### Workflow Selection

Every `ta run` uses a named *workflow* that controls how the goal is dispatched. The default is `single-agent` (one agent, one staging directory), which is backwards-compatible with all existing usage.

**Choosing a workflow:**

```bash
ta run "goal"                              # default: single-agent
ta run "goal" --workflow single-agent      # explicit (same as default)
ta run "goal" --workflow serial-phases     # chain phases in series (stub — v0.13.7.1)
```

**See available built-in workflows:**

```bash
ta workflow list --builtin
```

Output:

```
Built-in workflows (use with: ta run "goal" --workflow <name>):

  single-agent         Default: one agent in one staging directory (backwards-compatible)
  serial-phases        Chain phases serially: each phase as follow-up in same staging, one PR at end
  swarm                Parallel sub-goals with integration agent (v0.13.7.2+)
  approval-chain       Sequential human approval steps (v0.13.7.3+)
```

**Resolution order** (first match wins):

1. `--workflow <name>` flag on `ta run`
2. Plan phase metadata (future)
3. `.ta/config.yaml` `default_workflow` (future)
4. Built-in `single-agent` (backwards-compatible default)

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
# .ta/workflow.toml — flat string list (simple)
[verify]
commands = [
    "cargo build --workspace",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
]
on_failure = "block"   # "block" (no draft), "warn" (draft with warnings)
timeout = 300          # seconds per command (legacy global timeout)
```

For per-command timeouts, use structured commands:

```toml
# .ta/workflow.toml — per-command timeouts
[verify]
default_timeout_secs = 300     # default when command omits timeout_secs
heartbeat_interval_secs = 30   # progress heartbeat interval (default: 30)

[[verify.commands]]
run = "cargo fmt --all -- --check"
timeout_secs = 60

[[verify.commands]]
run = "cargo clippy --workspace --all-targets -- -D warnings"
timeout_secs = 300

[[verify.commands]]
run = "./dev 'cargo test --workspace'"
timeout_secs = 900
```

Both formats are supported. Verification output is streamed in real time with command labels (e.g., `[cargo] Compiling...`), and a heartbeat is emitted every `heartbeat_interval_secs` so you know the process is still running. If a command times out, the error includes the last 20 lines of output and a suggestion to increase the timeout.

When a command fails in block mode, TA shows the full command output (stdout + stderr) with the exit code, then offers to re-enter the agent immediately:

```
--- cargo test --workspace (exit code: 101) ---
  running 42 tests
  test auth::tests::token_refresh ... FAILED
  ... (18 lines omitted) ...
  1 test failed
---

Re-enter the agent to fix these issues? [Y/n]
```

If you confirm, the agent re-launches with the failure details injected into CLAUDE.md. After the agent exits, verification runs again automatically. In non-interactive or headless mode, TA prints instructions instead:

```bash
# Re-enter the agent to fix issues
ta run --follow-up

# Re-run verification manually
ta verify <goal-id-prefix>

# Skip verification on run (use sparingly)
ta run --skip-verify

# Skip pre-submit verification on apply
ta draft apply <draft-id> --skip-verify
```

If pre-submit verification fails during `ta draft apply`, the changes are already applied to your project but not committed. You can fix the issues and re-run the apply, skip verification, or revert with `git checkout -- .`.

In warn mode (`on_failure = "warn"`), the draft is created but carries verification warnings visible in `ta draft view`.

`ta init` generates a pre-populated `[verify]` section for Rust projects. Other project types get commented-out examples.

#### Constitution Pattern Scan

When `ta draft build` runs, TA automatically scans changed Rust files for potential §4 (CLAUDE.md injection cleanup) violations — functions that inject context into the workspace but may not restore it on all error paths.

```
[constitution] 2 potential §4 violation(s) — review before approving
  run.rs: inject_claude_md (3 inject, 1 restore)
```

The scan is static and grep-based (no agent), runs in under a second, and is non-blocking by default — the draft is still created but carries warnings visible in `ta draft view`. The warnings are also printed to stderr during `ta draft build` so they appear in CI logs.

The scanner counts `inject_*` and `restore_*` call sites in each changed `.rs` file. If a file has more inject calls than restore calls and contains early `return` statements, it is flagged as a candidate for review.

### Constitution Config (`constitution.toml`)

You can define project-specific invariants, inject/restore rules, and validation steps in `.ta/constitution.toml`. This makes TA's scanner and release gate aware of your codebase's conventions — not just TA's own internal patterns.

#### Scaffold a starter config

```bash
ta constitution init-toml
```

Creates `.ta/constitution.toml` pre-populated with TA's default inject/restore rules and validation steps. Edit it for your project's patterns.

#### Config format

```toml
[rules.injection_cleanup]
# Functions that must be paired: every file calling an inject_fn must also
# call the corresponding restore_fn, or the scanner flags a violation.
inject_fns = ["inject_config", "inject_credentials"]
restore_fns = ["restore_config", "restore_credentials"]
severity = "high"   # "high" | "medium" | "low"

[scan]
include = ["src/"]          # relative dirs to scan for .rs files
exclude = ["src/tests/"]    # skip these paths
on_violation = "warn"       # "warn" | "block" | "off"

[release]
checklist_gate = true   # require constitution sign-off in release pipeline
agent_review = false    # fan out a parallel constitution reviewer (opt-in)

# Per-project validation commands at each draft stage.
# These run inside the staging directory before the draft is packaged/applied.
[[validate]]
stage = "pre_draft_build"
commands = ["cargo clippy --workspace --all-targets -- -D warnings"]
on_failure = "block"   # "block" | "warn" | "ask_follow_up" | "auto_follow_up"

[[validate]]
stage = "pre_draft_apply"
commands = ["cargo test --workspace", "cargo fmt --all -- --check"]
on_failure = "warn"
```

#### Run the scanner manually

```bash
ta constitution check-toml            # human-readable output
ta constitution check-toml --json     # JSON output for CI/scripts
```

If `.ta/constitution.toml` is absent, the scanner uses TA's built-in `ta-default` rules (inject/restore pairs from `run.rs`). Exit code 1 is returned only when `on_violation = "block"`.

### Desktop Notifications

TA sends a system notification when a draft is ready for review, so you don't have to watch the terminal. On macOS this uses Notification Center (via `osascript`); on Linux it uses `notify-send`.

```toml
# .ta/workflow.toml
[notify]
enabled = true   # default: true — set false to disable
title = "TA"     # prefix for notification titles
```

Notifications are also sent when verification fails at goal completion. Notification failures are silently ignored and never block your workflow.

### Shell Configuration

The `[shell]` section in `.ta/workflow.toml` controls the TUI shell behavior:

```toml
# .ta/workflow.toml
[shell]
# Lines to show when attaching to a tail stream. Default: 5.
tail_backfill_lines = 5
# Maximum lines retained in the scrollable output buffer. Default: 50000.
output_buffer_lines = 50000
# Alias for output_buffer_lines. If set, overrides it. Minimum: 10000.
# scrollback_lines = 20000
# Auto-tail agent output when a goal starts. Default: true.
auto_tail = true
```

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
ta plan add "Add auth middleware"    # Add a phase to the existing plan (interactive)
ta plan add "Quick fix" --auto       # Add phase non-interactively (best-guess placement)
```

#### Generating a Plan from a Document

Use `ta plan from <path>` to generate a phased development plan from a product document (PRD, spec, RFC, design doc, etc.):

```bash
ta plan from docs/PRD.md
ta plan from ~/specs/feature-design.md --agent claude-code
ta plan from requirements.txt --source ./my-project
```

The agent reads the document, asks clarifying questions interactively, and writes a `PLAN.md` in the staging workspace. The result goes through the standard draft review flow — you review, approve, and apply it just like any other TA draft.

#### Adding Phases to an Existing Plan

Use `ta plan add <description>` to intelligently add a new phase to your existing plan. The agent reads the current PLAN.md, understands the phase structure, and proposes placement and version numbering through interactive dialog:

```bash
ta plan add "Add status bar model display"
ta plan add "Refactor auth middleware" --after v0.10.12
ta plan add "Quick bugfix phase" --auto
```

In interactive mode (default), the agent asks clarifying questions before modifying the plan — confirming whether this should be a standalone phase or added to an existing one, proposing version numbers, and checking for dependencies.

Use `--auto` for non-interactive mode where the agent makes best-guess placement without asking questions. Use `--after <phase-id>` to hint where the new phase should be inserted.

The result goes through standard draft review, so you always see and approve the plan change before it's applied.

**When to use which command:**

| Command | Use when | AI-powered? |
|---|---|---|
| `ta new run` | Starting a brand-new project from scratch with conversational planning | Yes (interactive) |
| `ta new run --from brief.md` | Starting a new project seeded from a written description | Yes (interactive) |
| `ta new run --template rust-cli` | Starting a new project with a language-specific scaffold | Yes (interactive) |
| `ta init --detect` | Scaffolding a `.ta/` config for an existing project | No |
| `ta plan create` | Starting from a generic template (greenfield/feature/bugfix) | No |
| `ta plan from <doc>` | You have a product document and want a tailored plan | Yes (interactive) |
| `ta plan add <desc>` | Adding a phase to an existing plan | Yes (interactive or `--auto`) |

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
stale_threshold_days = 7    # When --stale filter shows drafts (default: 7 days)
stale_hint_days = 3         # When startup hint fires (default: 3 days, informational)
health_check = true         # One-line warning on startup if stale drafts exist
```

`stale_hint_days` and `stale_threshold_days` are independent: the startup hint can fire early (e.g., after a weekend) without `ta draft list --stale` showing anything yet. Set `stale_hint_days = 5` to reduce noise if you find the Monday-morning reminder too aggressive.

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

**Release pipeline protection**: `ta gc` checks for a `.ta/release.lock` file written by `ta release run`. If the lock exists, GC skips staging deletion and prints a warning — this prevents accidentally deleting active release staging dirs mid-pipeline. Once the release pipeline completes (or is interrupted), the lock is removed automatically. Use `ta gc --force` to override.

#### Lifecycle Compaction (`ta gc --compact`)

Compaction removes "fat" artifacts — staging copies and draft packages — from applied/completed goals that are older than a configurable age threshold. Unlike standard GC (which handles zombie and orphaned records), compaction specifically targets successfully completed work where the VCS record is the source of truth and the staging copy is no longer needed.

```bash
# Preview compaction (no changes)
ta gc --compact --dry-run

# Compact goals applied more than 30 days ago (default)
ta gc --compact

# Compact goals applied more than 14 days ago
ta gc --compact --compact-after-days 14
```

The `goal-history.jsonl` ledger is **never** compacted — it is append-only and preserves a compact audit record of every goal. Compaction only discards staging directory copies and draft package JSON files.

You can also configure compaction in `.ta/daemon.toml` for reference by tooling:

```toml
[lifecycle.compaction]
enabled = true
compact_after_days = 30
discard = ["staging_copy", "draft_package"]
```

### Autonomous Operations (`ta operations`)

The daemon watchdog continuously monitors goal health, disk space, and system status. When it detects issues, it records **corrective action proposals** to `.ta/operations.jsonl`. Use `ta operations log` to review what the daemon has detected:

```bash
# Show last 20 corrective actions (default)
ta operations log

# Show all recorded actions
ta operations log --all

# Filter by severity: info, warning, critical
ta operations log --severity critical

# Show more entries
ta operations log --limit 50
```

Each corrective action shows:
- **Severity** (`INFO`, `WARN`, `CRIT`) and timestamp
- **Issue**: one-line description of what was detected
- **Diagnosis**: what caused the issue
- **Proposed action**: what to do about it
- **Status**: `proposed`, `approved by <who>`, `executed`, etc.
- **Auto-heal eligibility**: actions marked as auto-healable can be executed without human approval when configured

**Auto-heal policy** (`daemon.toml`): configure which low-risk actions the daemon can take automatically:

```toml
[operations.auto_heal]
enabled = true
allowed = [
  "restart_crashed_plugin",      # restart a plugin that exited unexpectedly
  "transition_zombie_to_failed", # mark dead-process goals as failed
  "clean_applied_staging",       # remove staging for successfully applied goals
]
# All other actions require human approval
```

By default, auto-heal is **disabled** (`enabled = false`). Opt in explicitly for the actions you trust. All corrective actions — auto-healed or manually approved — are recorded in `.ta/operations.jsonl` for audit traceability.

### Operational Runbooks (`ta runbook`)

Runbooks automate common recovery procedures as sequenced, approval-gated steps. TA ships with five built-in runbooks; you can add project-local runbooks as YAML files in `.ta/runbooks/`.

```bash
# List all available runbooks
ta runbook list

# Show the steps of a runbook without running it
ta runbook show disk-pressure

# Run a runbook interactively (each step requires confirmation)
ta runbook run disk-pressure

# Run with --auto to skip prompts for auto-approve steps
ta runbook run zombie-goals --auto

# Dry run: show what would execute without doing anything
ta runbook run stale-drafts --dry-run
```

**Built-in runbooks:**

| Name | Description | Trigger |
|------|-------------|---------|
| `disk-pressure` | Clean up staging dirs to free disk space | Disk < 2 GB |
| `zombie-goals` | Recover goals whose agent process has died | Running goals with no heartbeat > 30m |
| `crashed-plugins` | Detect and restart failed channel plugins | Plugin process exited |
| `stale-drafts` | Clean up PendingReview drafts older than 7 days | Drafts > 7 days old |
| `failed-ci` | Diagnose and re-run failed verification | `ta verify` fails |

**Project-local runbooks** — create YAML files in `.ta/runbooks/`:

```yaml
# .ta/runbooks/my-procedure.yaml
name: my-procedure
description: Custom recovery steps for my project
trigger:
  condition: When X goes wrong
  severity: warning
steps:
  - id: step1
    name: Check current state
    command: status --deep
    auto_approve: true
  - id: step2
    name: Fix the issue
    command: gc --compact
    description: Remove stale staging directories.
    auto_approve: false
```

### Proactive Notifications (Daemon API)

The daemon exposes a `GET /api/notifications` endpoint that returns actionable items needing attention. Notifications are ordered by severity and include a `suggested_action` field:

```bash
# Poll for current notifications (example with curl)
curl http://localhost:7700/api/notifications
```

Each notification includes: `id` (stable, for deduplication), `notification_type`, `severity`, `summary`, `suggested_action`, and optionally `detail` and `entity_id`.

### Shell Intent Routing

In `ta shell`, natural-language operational questions are automatically mapped to specific commands — no need to remember the exact syntax:

| You type... | Runs... |
|-------------|---------|
| `what's stuck?` | `ta goal list` |
| `clean up old goals` | `ta gc --dry-run` |
| `disk space` | `ta status --deep` |
| `daemon health` | `ta status --deep` |
| `show notifications` | `ta operations log` |
| `list runbooks` | `ta runbook list` |
| `what drafts need review?` | `ta draft list` |
| `show running goals` | `ta goal list` |

Unrecognised input is forwarded to the Q&A agent as usual.

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

### Goal Diagnostics

Inspect, diagnose, and pre-check goals without needing filesystem access:

```bash
# Detailed inspection: PID, health, elapsed time, staging size, draft state
ta goal inspect <goal-id>
ta goal inspect <goal-id> --json

# Analyze a failed/stuck goal: timeline, failure reason, suggested actions
ta goal post-mortem <goal-id>

# Check prerequisites before starting a goal
ta goal pre-flight
ta goal pre-flight "My new feature"
```

### System Health Check (`ta doctor`)

Run a comprehensive system check covering project structure, daemon, git, disk space, and plugins:

```bash
ta doctor
```

Reports each check as ok/warning/failed with actionable suggestions.

### Intelligent Status Dashboard

Running `ta` with no arguments (or `ta status`) shows the unified project dashboard. Items are prioritized: urgent issues first, then active work, then recent completions, then suggested next actions.

```bash
ta             # equivalent to ta status
ta status      # same
ta status --deep  # adds daemon health, disk usage, pending questions, recent events
```

Example output:
```
╭─ myproject (ta v0.13.1-alpha.6)
│  Next phase: v0.13.2 — MCP Transport Abstraction
│
│  ⚠ URGENT
│    1 draft(s) awaiting your review
│    → `ta draft view abc12345` to review
│
│  Active agents: 1
│    [42m] running — "Implement v0.13.1.6"  [b4953528]
│
│  Goals: 1 active  1 pending drafts  47 total
│
│  Suggested next:
│    `ta draft view abc12345` — review pending draft
╰─
```

The `--deep` flag adds daemon health, disk usage, pending interaction questions, and recent events.

### Daemon Health

Self-check the daemon's API, event system, plugins, disk space, and goal process liveness:

```bash
ta daemon health
```

### Draft Follow-Up (PR Iteration)

After `ta draft apply` creates a PR, iterate on it without creating a new staging copy:

```bash
# Lightweight follow-up on an existing PR branch
ta draft follow-up <draft-id>

# Auto-fetch CI failure logs and inject as agent context
ta draft follow-up <draft-id> --ci-failure

# Auto-fetch PR review comments as agent context
ta draft follow-up <draft-id> --review-comments

# Provide custom guidance
ta draft follow-up <draft-id> --guidance "Fix the auth test"
```

### PR Lifecycle

Track PRs created by TA:

```bash
# Show PR status for a specific draft
ta draft pr-status <draft-id>

# List all open PRs created by TA
ta draft pr-list
```

### Merging a PR and Syncing Main

After `ta draft apply --submit` creates a PR, complete the loop with `ta draft merge` or `ta draft watch`:

```bash
# Merge the PR immediately and sync local main
ta draft merge <draft-id>

# Poll until the PR merges (useful with auto_merge + CI gates), then sync
ta draft watch <draft-id>

# Or combine apply + watch into one command:
ta draft apply <draft-id> --submit --watch
```

`ta draft merge` calls `gh pr merge --auto` (or the configured merge strategy) then runs `ta sync` to fast-forward your local branch. `ta draft watch` polls every 30 seconds (configurable with `--interval`) until the PR state is `merged`, then syncs automatically.

After a successful merge, the goal transitions to `Merged` state — visible in `ta goal list`.

For Perforce, `ta draft merge` submits the shelved changelist (`p4 submit -c <CL>`) and `ta draft watch` polls the changelist state.

### Plan Intelligence

Edit the plan directly from the CLI without manual PLAN.md editing:

```bash
# Add an item to a phase
ta plan add-item "Add retry logic" --phase v0.11.3

# Move an item between phases
ta plan move-item "Add retry logic" --from v0.11.3 --to v0.11.4

# Discuss where a topic fits
ta plan discuss "webhook support"

# Create a new phase
ta plan create-phase v0.11.3.1 "Webhook Support" --after v0.11.3
ta plan create-phase v0.11.3.1 "Webhook Support" --goal "Add webhook delivery for events"
```

### Plugin Lifecycle

Monitor installed channel plugins:

```bash
# Show plugin health and version info
ta plugin status

# View plugin stderr logs
ta plugin logs discord
ta plugin logs discord --follow
```

### Project Manifest & Plugin Registry

Declare your project's plugin requirements in `.ta/project.toml` so that `ta setup resolve` installs everything automatically:

```toml
[project]
name = "my-project"
description = "My TA-managed project"

[plugins.discord]
type = "channel"
version = ">=0.1.0"
source = "registry:ta-channel-discord"
env_vars = ["DISCORD_BOT_TOKEN"]

[plugins.custom-webhook]
type = "channel"
version = ">=0.2.0"
source = "path:./plugins/custom-webhook"
required = false
```

Resolve and install all declared plugins:

```bash
# Interactive mode — installs missing plugins, warns about env vars
ta setup resolve

# CI mode — fails hard on missing plugins or env vars
ta setup resolve --ci

# Show plugin status (installed vs required)
ta setup show --section plugins
```

Plugin source schemes:
- `registry:<name>` — download from the TA plugin registry (cached in `~/.cache/ta/registry/`)
- `github:<owner/repo>` — download from GitHub releases
- `path:<local-path>` — build from local source (auto-detects Rust, Go, Make)
- `url:<download-url>` — direct tarball download with SHA-256 verification

The daemon enforces plugin requirements on startup. If a required plugin is missing or below `min_version`, the daemon attempts auto-setup. If that fails, it refuses to start with a clear error pointing to `ta setup resolve`.

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

### Goal Tags

Every goal has a human-friendly tag (e.g., `fix-auth-01`, `shell-routing-02`) that replaces UUIDs in all display contexts. Tags are auto-generated from the goal title with an auto-incrementing sequence number:

```bash
# View goal status by tag instead of UUID
ta goal status fix-auth-01

# Draft commands also accept tags
ta draft view fix-auth-01
ta draft apply fix-auth-01

# Goal list now shows TAG, DRAFT, and VCS columns
ta goal list
#  TAG                    TITLE                     STATE          DRAFT        VCS
#  shell-routing-01       Shell agent routing...    applied        applied      PR #166 (open)
#  fix-auth-01            Fix OAuth token...        running        —            —
```

Override the auto-generated tag with `--tag`:

```bash
ta run "Fix the auth bug" --tag fix-auth
```

### VCS Post-Apply Tracking

After `ta draft apply --git-commit --push --review`, TA tracks the PR lifecycle:

```bash
ta goal status fix-auth-01
#  Tag:      fix-auth-01
#  ...
#  --- Draft ---
#  Draft ID: 34b31e89-...
#  Status:   applied
#  Files:    8
#  --- VCS ---
#  Branch:   ta/fix-the-auth-bug
#  PR URL:   https://github.com/org/repo/pull/42
#  PR:       #42 (open)
```

The `ta draft list` default view now includes recently-applied drafts (< 7 days) and any draft with an open PR. This prevents the "no active drafts" false negative that previously hid in-progress PRs.

### Auto-Merge

Enable GitHub auto-merge after PR creation:

```toml
# .ta/workflow.toml
[submit.git]
auto_merge = true
```

When enabled, `gh pr merge --auto --squash` runs automatically after `gh pr create`.

### Daemon Command Heartbeat

Long-running commands dispatched through the daemon emit periodic heartbeat messages to prevent the shell from appearing frozen:

```
[heartbeat] still running... 10s elapsed
[heartbeat] still running... 20s elapsed
```

Configure the interval in `.ta/daemon.toml`:

```toml
[operations]
heartbeat_interval_secs = 10   # default: 10
```

### Goal Lifecycle Structured Logging

TA emits structured `tracing` log events at every major goal lifecycle milestone, making it easy to diagnose stuck agents, slow builds, or missed state transitions using your existing log aggregation tools.

Key log events emitted during a goal run:

| Event | Fields | When |
|-------|--------|------|
| `CLAUDE.md inject started` | `goal_id`, `staging`, `target_file` | Before CLAUDE.md is written to staging |
| `CLAUDE.md inject complete` | same | After successful inject |
| `Launching agent` | `goal_id`, `agent`, `staging` | Just before the agent process spawns |
| `Goal started — alias registered for output relay` | `goal_id`, `pid` | When sentinel is detected in agent output |
| `Goal state-poll task started` | `goal_id`, `initial_state` | When the background state watcher starts |
| `Goal state transition` | `goal_id`, `from`, `to` | On every state change |
| `Draft detected for goal` | `goal_id`, `draft_id`, `artifact_count` | When a draft is first built |
| `Goal still running` | `goal_id`, `elapsed_secs`, `state` | Periodically (default: every 5 minutes) |
| `Agent exited` | `goal_id`, `exit_code`, `elapsed_secs` | When the agent process terminates |
| `Files changed in staging workspace after agent exit` | `goal_id`, `changed_files` | After agent exits |

To see these logs, run the daemon with `RUST_LOG=info ta daemon start`.

#### Configuring the periodic "still running" log

By default, a structured log is emitted every 5 minutes for any in-flight goal. Configure this in `.ta/daemon.toml`:

```toml
[operations]
goal_log_interval_secs = 300   # default: 300 (5 minutes); set higher to reduce log volume
```

#### Daemon startup recovery

When the daemon starts, it scans for goals left in `running` or `pr_ready` state from before a restart and immediately resumes state-poll tasks for them. This ensures no notifications are missed across daemon restarts.

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

When `auto_apply: true` is set, auto-approved drafts are also applied to the source directory automatically — no manual `ta draft apply` step required. If `git_commit: true` is also set, a commit is created after auto-apply.

When `require_tests_pass` or `require_clean_clippy` is enabled, TA runs verification commands (`cargo test`, `cargo clippy`) in the staging workspace before auto-approving. If any verification step fails, the draft falls through to human review instead of being auto-approved.

All auto-approval decisions are recorded in the audit log with an `auto_approval` action, including which conditions were evaluated and whether verification passed.

To force human review regardless of policy (e.g., for sensitive changes), pass `--require-review` on apply:

```bash
ta draft apply <draft-id> --require-review   # Bypasses auto-approve policy
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

## Game Engine Projects

Use `ta init run --template` to onboard an existing game project. Templates are provided for Unreal Engine C++, Unity C#, and Godot — and any custom or proprietary engine can be onboarded with the generic template. Each template wires up [BMAD](https://github.com/bmadcode/BMAD-METHOD) (structured AI planning roles) and [Claude Flow](https://github.com/ruvnet/claude-flow) (parallel implementation) alongside TA governance.

| Engine | Template | Language |
|---|---|---|
| Unreal Engine 5 | `ta init run --template unreal-cpp` | C++ |
| Unity | `ta init run --template unity-csharp` | C# |
| Godot 4 | `ta init run --template godot-gdscript` | GDScript / C++ |
| Custom / other | `ta init run --template game-generic` | Any |

### Prerequisites

Install these once per machine — they are not bundled with TA:

**1. Claude Code (the `claude` CLI)**

```bash
# macOS / Linux
npm install -g @anthropic-ai/claude-code

# Windows (PowerShell)
npm install -g @anthropic-ai/claude-code
```

**2. Claude Flow MCP server**

```bash
npm install -g @ruvnet/claude-flow
```

Verify: `claude-flow --version`

**3. BMAD — install machine-locally, NOT inside your game project**

BMAD is a collection of markdown persona prompts. Install it once to a home directory location. **Do not clone it into your Perforce depot or game repo** — it is a tool, not project source.

```bash
# macOS / Linux
git clone https://github.com/bmadcode/BMAD-METHOD ~/.bmad

# Windows (PowerShell — run from your user home directory)
git clone https://github.com/bmadcode/BMAD-METHOD "$env:USERPROFILE\.bmad"
```

TA stores the path in `.ta/bmad.toml` (set automatically by `ta init run --template`). You can override it with the `TA_BMAD_HOME` environment variable:

```bash
# If you cloned BMAD somewhere else
export TA_BMAD_HOME=/path/to/bmad   # Unix
$env:TA_BMAD_HOME = "C:\tools\bmad"  # Windows PowerShell
```

**4. Anthropic API key**

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # Unix
$env:ANTHROPIC_API_KEY = "sk-ant-..."  # Windows PowerShell
```

Add to your shell profile (`.zshrc`, `.bashrc`) or Windows user environment variables so it persists across sessions.

---

### Initialize a game project

Navigate to the root of your game project and run the template for your engine:

```bash
# Unreal Engine C++ (directory containing *.uproject)
ta init run --template unreal-cpp

# Unity C# (directory containing Assets/ and ProjectSettings/)
ta init run --template unity-csharp

# Godot 4 (directory containing project.godot)
ta init run --template godot-gdscript

# Custom or proprietary engine — prompts for source dirs, build command, and VCS
ta init run --template game-generic
```

This writes:

| File | Purpose |
|---|---|
| `.ta/bmad.toml` | Path to your machine-local BMAD install |
| `.ta/agents/bmad-*.toml` | PM, architect, dev, QA role configs referencing BMAD personas |
| `.ta/workflow.toml` | TA config with VCS adapter, verify commands, auto-approval policy |
| `.ta/policy.yaml` | Protects critical project files from accidental agent modification |
| `.ta/.taignore` | Excludes build artifacts from staging (Binaries/, Intermediate/, .godot/, etc.) |
| `.mcp.json` | MCP server entries for `ta` and `claude-flow` |
| `.ta/onboarding-goal.md` | First goal prompt — produces PRD, architecture doc, sprint stories |

Engine-specific policy protection defaults:

| Engine | Protected files |
|---|---|
| Unreal | `*.uproject`, `Build.cs`, `Config/DefaultEngine.ini`, `Config/DefaultGame.ini` |
| Unity | `ProjectSettings/*.asset`, `Packages/manifest.json`, `*.asmdef` |
| Godot | `project.godot`, `export_presets.cfg`, `*.gdextension` |
| Generic | Configured interactively during `ta init` |

> **VCS note**: None of these files need to go into your depot or repo. Add `.ta/`, `.mcp.json`, and `.ta/onboarding-goal.md` to `.p4ignore`, `.gitignore`, or equivalent if you prefer to keep them local to each developer machine.

---

### Run the discovery goal

After `ta init`, start the daemon and run the onboarding goal:

```bash
ta daemon start
ta run --objective-file .ta/onboarding-goal.md
```

The agent explores your codebase and writes planning documents. What it scans depends on the template:

| Engine | Scanned paths |
|---|---|
| Unreal | `Source/`, `Config/`, `*.uproject`, `Plugins/` |
| Unity | `Assets/Scripts/`, `ProjectSettings/`, `Packages/` |
| Godot | `*.gd`, `*.tscn`, `*.tres`, `project.godot` |
| Generic | Paths you configured during `ta init run --template game-generic` |

It always produces:
1. `docs/architecture.md` — module/scene/node graph, key classes, build targets
2. `docs/bmad/prd.md` — inferred product requirements from game logic and scene structure
3. `docs/bmad/stories/` — top 5 inferred feature areas as BMAD story stubs

Review and approve the draft:

```bash
ta draft list
ta draft view <id>
ta draft approve <id>
```

Once approved, the docs are in your workspace and you have a BMAD-ready project.

---

### Walkthrough: first feature from scratch

The full workflow is the same across all engines: init → discover → design → implement → QA → apply. The steps below use Unreal C++ as the example; Unity, Godot, and custom engine notes follow.

#### Unreal Engine C++

**Assumptions**: Unreal Engine 5.x, source or Launcher build, git or Perforce depot at project root, `ta` and `claude` both installed and on PATH, `ANTHROPIC_API_KEY` set.

#### Step 1 — Open your project root

Your project root is the folder containing `MyGame.uproject` (not the Engine folder). If you use Perforce this is your clientspec root for the game stream.

```bash
cd /path/to/MyGame
```

#### Step 2 — Initialize TA with the Unreal template

```bash
ta init run --template unreal-cpp
```

Expected output:
```
Created .ta/bmad.toml           (BMAD home: ~/.bmad)
Created .ta/agents/bmad-pm.toml
Created .ta/agents/bmad-architect.toml
Created .ta/agents/bmad-dev.toml
Created .ta/agents/bmad-qa.toml
Created .ta/workflow.toml
Created .ta/policy.yaml
Created .ta/.taignore
Created .mcp.json
Created .ta/onboarding-goal.md
```

If you installed BMAD somewhere other than `~/.bmad`, set `TA_BMAD_HOME` first:

```bash
export TA_BMAD_HOME=/path/to/BMAD-METHOD   # then re-run ta init run --template unreal-cpp
```

> **Perforce users**: add `.ta/`, `.mcp.json`, and `ONBOARDING.md` to your `.p4ignore`. These are developer-local tooling files — they should not go into the depot.

#### Step 3 — Run the discovery goal

This is a one-time onboarding step. The agent reads your codebase and produces planning documents.

```bash
ta daemon start
ta run --objective-file .ta/onboarding-goal.md
```

The agent will take a few minutes. It will:
1. Walk `Source/`, `Config/`, and scan `*.uproject` for modules, plugins, and targets
2. Write `docs/architecture.md` — module graph, key classes, build dependencies
3. Write `docs/bmad/prd.md` — inferred product requirements from GameMode, levels, and feature flags
4. Write `docs/bmad/stories/` — top 5 inferred feature areas as BMAD story stubs

When the agent finishes, review and approve the draft:

```bash
ta draft list                 # find the draft ID
ta draft view <id>            # read the proposed docs
ta draft approve <id>         # accept and copy to your workspace
```

You now have `docs/architecture.md` and `docs/bmad/prd.md` in your project. The BMAD cycle can begin.

#### Step 4 — Pick a story and design it

Open `docs/bmad/stories/` and choose a story (or write your own in that folder). Then run the architect:

```bash
ta run "Design: <story title from docs/bmad/stories/story-01.md>" --agent bmad-architect
```

The architect will read the story stub and `docs/architecture.md`, then write:
- `docs/bmad/design/<story>.md` — technical design with module breakdown, class signatures, and interface contracts

Review the design draft before moving to implementation:

```bash
ta draft view <id>
ta draft approve <id>
```

#### Step 5 — Implement the story

```bash
ta run "Implement: <story title>" --agent bmad-dev
```

The dev agent reads the design doc and story, writes C++ in `Source/`, and calls `ta_pr_build` when done.

> **Compile check**: `ta init run --template unreal-cpp` sets up `verify_command` in `.ta/workflow.toml` to run `UnrealBuildTool` (or `msbuild`/`xbuild`) before the draft is approved. If the build fails, the agent is re-invoked with the error output to fix it before you ever see the draft.

Review the diff:

```bash
ta draft view <id>     # see every file changed
ta draft approve <id>  # or: ta draft deny <id> --reason "..."
```

#### Step 6 — Write tests with the QA role

```bash
ta run "Write tests for: <story title>" --agent bmad-qa
```

The QA agent writes Gauntlet/Automation test stubs in `Source/<Module>/Tests/`. Review and approve the same way.

#### Step 7 — Apply and commit

```bash
ta draft apply <id> --git-commit    # or --p4-submit for Perforce
```

This copies approved changes from staging to your real workspace and creates a commit (or CL). TA never touches your working files until you explicitly apply.

---

#### Unity C#

```bash
# 1 — Navigate to your Unity project root (contains Assets/ and ProjectSettings/)
cd /path/to/MyUnityProject

# 2 — Initialize
ta init run --template unity-csharp

# 3 — Run discovery (scans Assets/Scripts/, ProjectSettings/, Packages/)
ta daemon start
ta run --objective-file .ta/onboarding-goal.md
ta draft approve <id>

# 4–6 — BMAD design → implement → QA cycle
ta run "Design: <story>" --agent bmad-architect
ta run "Implement: <story>" --agent bmad-dev
ta run "Write tests for: <story>" --agent bmad-qa

# 7 — Apply
ta draft apply <id> --git-commit
```

The Unity verify command (set in `.ta/workflow.toml`) runs `dotnet build` or the Unity batch-mode compiler before any draft is approved, so you only ever see code that compiles.

---

#### Godot 4 (GDScript / C++)

```bash
# 1 — Navigate to your Godot project root (contains project.godot)
cd /path/to/MyGodotProject

# 2 — Initialize
ta init run --template godot-gdscript

# 3 — Run discovery (scans *.gd, *.tscn, *.tres, project.godot)
ta daemon start
ta run --objective-file .ta/onboarding-goal.md
ta draft approve <id>

# 4–6 — BMAD design → implement → QA cycle
ta run "Design: <story>" --agent bmad-architect
ta run "Implement: <story>" --agent bmad-dev
ta run "Write tests for: <story>" --agent bmad-qa

# 7 — Apply
ta draft apply <id> --git-commit
```

For GDExtension (C++) projects the dev agent writes `.cpp`/`.h` under the extension source directory. The verify command runs `scons` (or `cmake`) to confirm the extension compiles before the draft reaches you.

---

#### Custom or proprietary engine

```bash
# 1 — Navigate to your project root
cd /path/to/MyGame

# 2 — Initialize interactively; you will be prompted for:
#     - Source directories to scan
#     - Build/verify command (e.g. "make game" or "msbuild Game.sln")
#     - VCS type (git, perforce, or none)
#     - Files to protect from accidental modification
ta init run --template game-generic

# 3–7 — same discovery → design → implement → QA → apply cycle as above
ta daemon start
ta run --objective-file .ta/onboarding-goal.md
ta draft approve <id>
ta run "Design: <story>" --agent bmad-architect
ta run "Implement: <story>" --agent bmad-dev
ta draft apply <id>
```

For engines with proprietary build systems, set the verify command in `.ta/workflow.toml` to whatever command produces a clean build error on failure (exit non-zero). TA will use it as the pre-draft gate.

---

### Start implementing with BMAD roles

```bash
# Use the architect role to design a feature
ta run "Design the inventory system" --agent bmad-architect

# Use the dev role to implement a story
ta run "Implement inventory pickup and drop" --agent bmad-dev

# Use the QA role to write test cases
ta run "Write integration tests for inventory system" --agent bmad-qa
```

Each goal runs in staging, produces a draft, and requires your approval before any changes land in the project.

---

### Using Claude Flow for parallel implementation

Once BMAD has produced module boundaries in `docs/architecture.md`, Claude Flow can parallelize implementation across those modules. Add a swarm step to your `workflow.toml`:

```toml
[workflow.swarm]
enabled = true
max_agents = 4
split_by = "module"   # each agent works on a separate module directory
```

Then run with swarm mode:

```bash
ta run "Implement sprint 1 stories" --agent bmad-dev --swarm
```

Claude Flow orchestrates the agents; TA collects all their changes into a single draft for your review.

---

## Configuration

### Workflow Config (`.ta/workflow.toml`)

The central configuration file for TA behavior:

```toml
[submit]
adapter = "git"                    # "git", "svn", "perforce", or "none"
auto_submit = true                 # Run full submit workflow on apply (default: true when adapter != "none")
auto_review = true                 # Open review after submit (default: true when adapter != "none")
co_author = "Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>"  # Co-author trailer on commits

[submit.git]
branch_prefix = "ta/"              # Branch naming: ta/goal-title
target_branch = "main"             # GitHub PR base branch
merge_strategy = "squash"          # squash | merge | rebase
pr_template = ".ta/pr-template.md" # GitHub PR body template
remote = "origin"                  # Git remote name

[submit.perforce]
workspace = "my-workspace"         # Perforce workspace/client name
shelve_by_default = true           # Shelve instead of submit to depot

[submit.svn]
repo_url = "svn://example.com/trunk"  # SVN repository URL

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

Without this file, TA auto-detects your VCS (Git > SVN > Perforce > none) and uses sensible defaults. When VCS is detected, `ta draft apply` runs the full submit workflow automatically — no flags needed.

### Agent Sandboxing (`[sandbox]`)

Restrict the agent's filesystem and network access using OS-level sandboxing. Disabled by default — enable when you want to confine what an agent can read, write, or reach:

```toml
[sandbox]
enabled = true
provider = "native"        # "native" (macOS sandbox-exec / Linux bwrap) | "openshell" | "oci"

# Paths the agent may read beyond its staging workspace and system libraries.
# The staging workspace root is always readable/writable.
allow_read = ["/usr/local/share", "/opt/homebrew"]

# Additional writable paths (staging workspace is always writable).
allow_write = []

# Network destinations the agent may reach. Empty = block all outbound network.
# Use ["*"] to allow all network (equivalent to no sandboxing).
allow_network = ["api.anthropic.com", "api.github.com"]
```

**Platform support:**
- **macOS**: Uses built-in `sandbox-exec` (Seatbelt). No installation required.
- **Linux**: Uses `bwrap` (bubblewrap). Install via `apt install bubblewrap` or equivalent. If unavailable, TA warns and runs without sandboxing.
- **Windows**: Sandboxing not yet supported — `enabled = true` is silently ignored.

**What is always allowed** (regardless of `allow_read`):
- The staging workspace (agent's working directory)
- OS system libraries (`/usr`, `/System`, `/Library`, `/nix`, `/lib`)
- `/tmp` (tmpfs on Linux)
- `/dev/null`, `/dev/tty`

**Network filtering note**: On macOS, `allow_network` is a declaration (used for auditing) but the sandbox profile currently allows all outbound when any host is listed. Hostname-level network filtering (L7 proxy) is planned for v0.14.1.

### Commit Co-Authorship

Every commit made through `ta draft apply` includes a `Co-Authored-By` trailer. This gives TA shared credit alongside the human author in GitHub's contribution graph, PR history, and `git log`.

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
| `sync_upstream()` | `git fetch` + merge/rebase/ff | `svn update` | `p4 sync` |
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

### Syncing Upstream (`ta sync`)

After a PR merges, your local branch is stale. `ta sync` pulls upstream changes through the configured VCS adapter:

```bash
ta sync
```

For Git, this runs `git fetch` followed by merge, rebase, or fast-forward depending on your configuration. If conflicts are detected, `ta sync` reports them without aborting — you resolve conflicts manually.

Configure sync behavior in `.ta/workflow.toml`:

```toml
[source.sync]
auto_sync = false    # auto-sync after ta draft apply (default: false)
strategy = "merge"   # "merge" (default), "rebase", or "ff-only"
remote = "origin"    # remote to sync from (default: "origin")
branch = "main"      # branch to sync from (default: "main")
```

**Auto-sync after apply**: Set `auto_sync = true` to automatically sync upstream after `ta draft apply` completes the submit workflow. If conflicts are found during auto-sync, a warning is printed and you can run `ta sync` manually to resolve.

**Shell shortcut**: In `ta shell`, type `sync` to run `ta sync` directly.

**Events**: `ta sync` emits `sync_completed` or `sync_conflict` events, which flow through channels and event routing like all other TA events.

### Building and Testing (`ta build`)

Run your project's build and test suite through TA's build adapter system:

```bash
# Build the project
ta build

# Build and run tests
ta build --test
```

TA auto-detects the build system by looking for `Cargo.toml` (Rust/Cargo), `package.json` (Node.js/npm), or `Makefile`. You can also configure a specific adapter or custom commands in `.ta/workflow.toml`:

```toml
[build]
adapter = "cargo"                      # "cargo", "npm", "script", "webhook", "auto" (default), "none"
command = "cargo build --release"      # override the default build command
test_command = "cargo test --release"  # override the default test command
on_fail = "notify"                     # "notify" (default), "block_release", "block_next_phase", "agent"
timeout_secs = 600                     # per-command timeout (default: 600s)
```

For arbitrary build systems, use the `script` adapter with custom commands:

```toml
[build]
adapter = "script"
command = "make all"
test_command = "make test"
```

**Events**: `ta build` emits `build_completed` or `build_failed` events, which flow through channels and event routing like all other TA events. Failed builds include the exit code and stderr output.

**Shell shortcuts**: In `ta shell`, type `build` or `test` to run `ta build` / `ta build --test` directly.

**Release integration**: The release pipeline scaffold (from v0.10.6) runs `ta build` as a pre-release step when available. If the build fails, the release is blocked.

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
| `context_file` | string | Path for non-Claude agents to receive memory context (e.g., `.ta/agent_context.md`) |
| `injects_settings` | bool | Inject `.claude/settings.local.json` with permissions |
| `pre_launch` | object | Command to run before agent launch |
| `env` | map | Environment variables for the agent process |
| `headless_args` | string[] | Extra args appended in headless (daemon-spawned) mode |
| `non_interactive_env` | map | Env vars set ONLY in headless mode to suppress interactive prompts |
| `auto_answers` | list | Regex→response mappings for auto-answering known prompts |
| `interactive` | object | Interactive session config (PTY capture, resume) |
| `alignment` | object | Alignment profile (see below) |

#### Handling Agent Stdin Prompts

> **Planned for v0.10.18.5** — not yet implemented. See [PLAN.md](../PLAN.md) for status.

When the daemon spawns an agent as a background process, stdin is normally unavailable. TA provides three layers to handle agents that require interactive input:

**Layer 1: Non-interactive env vars** — Suppress prompts entirely by setting environment variables in headless mode:

```yaml
# .ta/agents/claude-flow.yaml
non_interactive_env:
  CLAUDE_FLOW_NON_INTERACTIVE: "true"
  CLAUDE_FLOW_TOPOLOGY: "mesh"
```

These are only set for daemon-spawned (headless) runs, not for direct CLI usage where the user has a terminal.

**Layer 2: Auto-answer map** — Pre-configure responses to known prompts using regex patterns:

```yaml
auto_answers:
  - prompt: "Select.*topology.*\\[1\\]"
    response: "1"
  - prompt: "Continue\\?.*\\[y/N\\]"
    response: "y"
  - prompt: "Enter.*name:"
    response: "{goal_title}"
    fallback: true    # use as default for unmatched prompts at timeout
```

Template variables: `{goal_title}`, `{goal_id}`, `{project_name}`.

**Layer 3: Live stdin relay** — Unmatched prompts are forwarded to `ta shell` as interactive questions. The prompt appears in the output pane, the input area switches to response mode, and your typed response is sent to the agent's stdin via `POST /api/goals/:id/input`.

Auto-answered prompts appear as dimmed lines: `[auto] Select topology: → 1 (mesh)`.

#### Prompt Detection Hardening

The shell uses heuristics to detect when an agent is waiting for stdin input. Three layers prevent false positives from locking the shell into `stdin>` mode:

**Layer 1 — Heuristic rejection**: Lines containing markdown bold (`**word**`), backtick-quoted code, file paths (`.rs`, `.ts`, `/src/`), or bracket-prefixed output (`[agent]`, `[info]`) are never classified as prompts. Strong positive signals like `[y/N]`, `[Y/n]`, and numbered choices `[1] [2]` always match.

**Layer 2 — Continuation cancellation**: If a prompt is detected but the agent keeps producing output, the prompt is automatically dismissed. A real prompt means the agent has stopped. The dismiss window is configurable:

```toml
# .ta/daemon.toml
[operations]
prompt_dismiss_after_output_secs = 5   # default: 5 seconds
```

When the agent output stream ends (goal completes), any pending prompt is also cleared.

**Layer 3 — Q&A agent verification**: When a prompt is detected, the shell also dispatches the suspected line to the Q&A agent (`/api/agent/ask`) for a second opinion. If the agent responds "not a prompt" before the user types anything, the prompt is auto-dismissed. The prompt shows `(verifying...)` while the check is in flight:

```toml
# .ta/daemon.toml
[operations]
prompt_verify_timeout_secs = 10   # default: 10 seconds (fail-open on timeout)
```

You can always dismiss a false-positive prompt manually with Ctrl-C.

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

### Runtime Adapter

By default, TA launches agents as bare OS child processes. The **runtime adapter** system lets you select an alternative execution environment per agent — for example, an OCI container or VM.

#### Selecting a runtime in agent config

Add a `runtime` field to your agent YAML file:

```yaml
# agents/claude.yaml
command: claude
args: ["--output-format", "stream-json"]

# Use the default bare-process runtime (no extra config needed)
runtime: process

# Or specify a container runtime (requires ta-runtime-oci plugin)
runtime: oci
runtime_options:
  image: "ghcr.io/myorg/agent-sandbox:latest"
  pull_policy: if_not_present
```

#### Built-in runtimes

| Name | Description |
|------|-------------|
| `process` | Bare OS child process (default). No isolation. |

#### Installing runtime plugins

External runtimes are provided as plugin binaries named `ta-runtime-<name>`. TA discovers them in order:

1. `.ta/plugins/runtimes/` — project-local
2. `~/.config/ta/plugins/runtimes/` — user-global
3. Directories on `$PATH`

The plugin binary speaks a JSON-over-stdio protocol. See `crates/ta-runtime/src/plugin.rs` for the protocol specification.

#### Runtime lifecycle events

When an agent is launched, TA emits structured events that appear in `ta events`:

```
agent_spawned   goal started, runtime=process pid=12345
agent_exited    goal finished exit_code=0 duration_secs=42
runtime_error   spawn failed: command not found
```

These events are also surfaced in the shell TUI and dashboard.

### External Action Governance

When an agent needs to perform an action with real-world side effects — sending an email, posting to social media, calling an external API, or running a database query — TA intercepts the request, applies your policy, and captures a full audit log regardless of outcome.

TA provides the governance layer (policy, approval, capture, rate limiting). The actual implementations live in your connectors and plugins.

#### Built-in action types

| Action type | Description |
|-------------|-------------|
| `email` | Send an email message (requires `to`, `subject`) |
| `social_post` | Post to a social media platform (requires `platform`, `content`) |
| `api_call` | Make an HTTP API request (requires `url`, `method`) |
| `db_query` | Execute a database query (requires `query`) |

#### Configuring action policies

Add an `[actions]` section to `.ta/workflow.toml`:

```toml
[actions.email]
policy = "review"        # auto | review | block (default: review)
rate_limit = 10          # max per goal (omit for unlimited)
allowed_domains = ["@mycompany.com"]

[actions.social_post]
policy = "block"         # never allow

[actions.api_call]
policy = "auto"          # execute immediately without review
rate_limit = 50

[actions.db_query]
policy = "review"
rate_limit = 100
```

**Policy values:**
- `auto` — execute immediately (stub returns result; real connector needed)
- `review` — capture for human review; surfaces in `ta draft view` alongside file changes
- `block` — always reject; agent receives a clear refusal

Unknown action types default to `review`.

#### How agents use it

Agents call the `ta_external_action` MCP tool:

```json
{
  "action_type": "email",
  "payload": {
    "to": "alice@example.com",
    "subject": "Weekly report",
    "body": "..."
  },
  "dry_run": false
}
```

TA applies policy, logs the attempt, and returns a structured response the agent can act on.

#### Dry-run mode

Pass `"dry_run": true` to preview what would happen without any side effects. The action is logged with outcome `dry_run` and the agent receives a description of what would have been sent.

#### Rate limiting

Rate limits are per-goal, per-action-type and reset when the daemon restarts. Once the limit is reached, the agent receives a `rate_limited` response with the current count and configured limit.

#### Action capture log

Every action attempt — regardless of outcome — is appended to `.ta/action-log.jsonl`. Each entry includes:

- `capture_id` — unique identifier
- `action_type` — e.g. `email`
- `payload` — the full request payload
- `goal_run_id` — which goal triggered it
- `timestamp` — ISO 8601
- `policy` — the policy that was applied
- `outcome` — `executed`, `captured_for_review`, `blocked`, `dry_run`, or `rate_limited`

#### Reviewing captured actions

Actions with `policy = "review"` appear in `ta draft view` alongside file changes. Approve or deny them as part of the normal draft review flow.

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

### VCS Integration

`ta draft apply` automatically runs the full submit workflow when a VCS adapter is detected or configured. No flags needed in the common case.

```bash
# Default: auto-detects VCS, commits, pushes, opens PR
ta draft apply <draft-id>

# Preview what would happen without executing
ta draft apply <draft-id> --dry-run

# Copy files only, skip all VCS operations
ta draft apply <draft-id> --no-submit

# Submit but skip review (PR) creation
ta draft apply <draft-id> --no-review
```

Configure in `.ta/workflow.toml`:

```toml
[submit]
adapter = "git"          # auto-detected if not set
auto_submit = true       # default: true when adapter != "none"
auto_review = true       # default: true when adapter != "none"

[submit.git]
branch_prefix = "ta/"
target_branch = "main"
```

The deprecated `--git-commit` and `--git-push` flags still work as aliases for `--submit`.

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
ta release run 0.4.0-alpha --auto-approve  # Explicit auto-approve for CI

# Show pipeline steps
ta release show

# Create a customizable .ta/release.yaml
ta release init
```

#### Dispatching a release with a custom tag label

The standard `ta release run` pipeline creates a `v<semver>` git tag and pushes it, which triggers the `push: tags: v*` CI path. For public alpha labels that don't follow the `v*` pattern (e.g. `public-alpha-v0.13.1.1`), use `ta release dispatch` to trigger the release workflow via `workflow_dispatch` instead:

```bash
# Trigger a release with a human-readable public label
ta release dispatch public-alpha-v0.13.1.1

# Mark as pre-release on GitHub
ta release dispatch public-alpha-v0.13.1.1 --prerelease

# Explicit repo (defaults to git remote auto-detection)
ta release dispatch public-alpha-v0.13.1.1 --repo Trusted-Autonomy/TrustedAutonomy

# Different workflow file
ta release dispatch public-alpha-v0.13.1.1 --workflow release.yml
```

`ta release dispatch` requires the [GitHub CLI (`gh`)](https://cli.github.com) to be installed and authenticated (`gh auth login`). The release workflow creates the tag automatically — no local `git tag` needed. Monitor the run with:

```bash
gh run list --repo Trusted-Autonomy/TrustedAutonomy --workflow release.yml
```

From `ta shell`, the `release` shortcut launches the pipeline as a long-running command:

```
ta shell> release v0.10.6
```

The `--interactive` flag uses the `releaser` agent with `ta_ask_human` for review checkpoints. The human stays in `ta shell` throughout — the agent asks for release notes approval and publish confirmation interactively.

When running release commands from `ta shell` (non-TTY context), approval gates are presented as interactive questions in the TUI via the same file-based interaction mechanism used by `ta_ask_human`. Use `--auto-approve` to skip all gates in CI/non-interactive environments.

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

# Verify cryptographic attestation signatures on all events
ta audit verify-attestation

# Verify a specific event by UUID prefix
ta audit verify-attestation <event-id-prefix>

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
- Per-tool-call MCP audit entries (tool name, caller mode, target URI, goal run ID)
- Auto-approval decisions with condition evaluation details

Every MCP tool invocation (`ta_fs_write`, `ta_goal_start`, `ta_pr_build`, etc.) is individually logged to the audit trail with the agent identity, caller mode (`Normal`, `Orchestrator`, or `Unrestricted`), and the tool name. Agent identity is resolved from `TA_AGENT_ID` (set by orchestrators), falling back to the dev session ID, then `"unknown"`. This gives full traceability of which agent called which tool, when, and in what security context.

#### Cryptographic Attestation

Audit events can be cryptographically signed with an Ed25519 key so that retroactive forgery is detectable — even without a hardware TPM.

Enable attestation in `.ta/workflow.toml`:

```toml
[audit]
attestation = true
# keys_dir defaults to .ta/keys/
```

On first use, TA auto-generates a keypair:
- `.ta/keys/attestation.pkcs8` — PKCS8 DER private key (keep private)
- `.ta/keys/attestation.pub` — hex-encoded public key (safe to share)

Verify signatures:

```sh
# Verify all signed events
ta audit verify-attestation

# Verify a specific event
ta audit verify-attestation <event-id-prefix>

# Use a custom keys directory
ta audit verify-attestation --keys /path/to/keys
```

Output shows `OK`, `INVALID SIGNATURE`, or `unsigned` for each event. Any `INVALID SIGNATURE` exits with code 1 — wiring this into CI catches tampering automatically.

Hardware backends (TPM 2.0, Apple Secure Enclave) are community-contributed plugins that implement the `AttestationBackend` trait.

### Multi-Party Approval

By default, a single `ta draft approve` is enough to move a draft to `Approved`. For high-stakes projects you can require N distinct approvals before a draft can be applied.

Configure in `.ta/workflow.toml`:

```toml
[governance]
require_approvals = 2
approvers = ["alice", "bob", "carol"]   # empty = any reviewer accepted

# Optional: identity that may bypass quorum in emergencies (logged prominently)
override_identity = "emergency-admin"
```

Each team member approves independently:

```bash
# Alice approves
ta draft approve <id> --as alice
# → Recorded approval from 'alice' (1/2 approvals — 1 more needed)

# Bob approves — quorum reached
ta draft approve <id> --as bob
# → Approved draft package <id> (2/2 approvals — quorum reached)

# Now the draft can be applied
ta draft apply <id>
```

Rules:
- Approvals accumulate on the draft. The same reviewer cannot approve twice.
- If `approvers` is non-empty, only listed identities are accepted.
- If `require_approvals = 1` (default), the old `--reviewer` flag continues to work.
- Legacy `ta draft approve <id>` (no `--as`) uses `--reviewer human-reviewer` and counts as one approval.

#### Emergency Override

A designated `override_identity` can bypass the quorum requirement. The override is written to the audit log and printed with a `⚠` warning:

```bash
ta draft approve <id> --as emergency-admin --override
# ⚠  Override by 'emergency-admin': quorum requirement bypassed (audit trail updated).
# Approved draft package <id> by emergency-admin
```

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

#### New commands

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

#### Memory dashboard

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

#### Automatic State Capture

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

#### Project-Aware Key Schema

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

#### Negative Paths

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
| **Plan phases** | "v0.12.5 Semantic Memory completed" | Auto-captured when `ta draft apply` marks phase done |
| **Constitution rules** | "Never commit directly to main" | Indexed from `.ta/constitution.md` on every goal start |

#### Inspecting the memory backend

```bash
# Show which backend is active, entry count, and storage size
ta memory backend

# List entries (optionally filtered by category)
ta memory list
ta memory list --category convention
ta memory list --limit 5
```

Example output of `ta memory backend`:

```
Memory Backend
  Active backend:  ruvector
  RuVector store:  .ta/memory.rvf (42 entries, 1.2 MiB)
  FsMemory store:  .ta/memory (3 legacy entries)

  Note: 3 legacy entries found. They will be auto-migrated
        the next time RuVectorStore is opened (at goal start).
```

#### Constitution indexing

Place a `.ta/constitution.md` file with your project's behavioral rules. TA indexes every bullet-point rule into memory as a `constitution:{section}:{slug}` entry (category: Convention, confidence 1.0) on every goal start. These rules are injected into agent context alongside history and architectural entries.

Example constitution file:

```markdown
## Core Invariants

- **Never commit directly to main**: All changes must go through a PR.
- Always run the full test suite before committing.

## Development Standards

- Use `tempfile::tempdir()` for all test fixtures that need filesystem access.
```

The indexing is idempotent — re-indexing the same rules overwrites without duplication.

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

#### Pruning old events

Over time, daily event log files accumulate. Prune old files to reclaim disk space:

```bash
# Remove event files older than 30 days (default)
ta events prune

# Custom retention window
ta events prune --older-than-days 7

# Preview what would be removed without deleting
ta events prune --dry-run
```

Pruning removes entire daily `.jsonl` files whose date falls before the cutoff. It does not modify recent files.

#### Event routing

Event routing lets any TA event trigger an intelligent response — launch an agent to fix a build failure, start a workflow on draft denial, or escalate persistent errors to a human. Configure routing in `.ta/event-routing.yaml`:

```yaml
# .ta/event-routing.yaml
defaults:
  max_attempts: 3
  escalate_after: 2
  default_strategy: notify

responders:
  - event: goal_failed
    strategy: agent
    agent: claude-code
    prompt: |
      A goal failed. Review the error and suggest a fix or retry.
    require_approval: true
    escalate_after: 2
    max_attempts: 3

  - event: policy_violation
    strategy: block

  - event: draft_denied
    strategy: notify
    channels: [shell, slack]

  - event: memory_stored
    strategy: ignore
```

**Strategies:**

| Strategy | Behavior |
|----------|----------|
| `notify` | Deliver event to configured channels (default) |
| `block` | Halt the pipeline, require human intervention |
| `agent` | Launch a governed agent goal with event context |
| `workflow` | Start a named workflow with event data as input |
| `ignore` | Suppress the event entirely |

**Manage routing from the CLI:**

```bash
# List all configured responders
ta events routing list

# Dry-run: see what would happen for an event type
ta events routing test goal_failed

# Quick override a strategy
ta events routing set goal_failed agent
```

**Filters** narrow when a responder fires:

```yaml
- event: goal_started
  strategy: block
  filter:
    phase: "v0.9.*"        # only for v0.9.x phases
    agent_id: codex         # only for this agent
```

**Guardrails** prevent runaway automation:
- `max_attempts` stops agent retries after N failures (overrides to `notify`)
- `escalate_after` flags decisions for human attention after N attempts
- `policy_violation` events cannot be routed to `ignore`
- Agent-routed events produce governed goals with full staging and draft review

A default config ships at `templates/event-routing.yaml`. Copy it to `.ta/event-routing.yaml` to customize.

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

### Daemon Lifecycle Management

The `ta daemon` subcommand provides first-class control over the TA daemon process. You can start, stop, restart, inspect, and tail logs without needing wrapper scripts or the `ta-daemon` binary directly.

#### Starting the daemon

```bash
# Start in the background (default)
ta daemon start

# Start on a custom port
ta daemon start --port 9900

# Start in the foreground (for debugging or containers)
ta daemon start --foreground
```

The daemon writes its PID to `.ta/daemon.pid` and logs to `.ta/daemon.log`. On success, the command prints the PID, port, and log path.

#### Stopping the daemon

```bash
ta daemon stop
```

Sends a graceful shutdown request via `POST /api/shutdown`, waits up to 5 seconds for the process to exit, and cleans up the PID file. If the HTTP endpoint is unreachable, falls back to sending SIGTERM by PID.

#### Restarting the daemon

```bash
ta daemon restart

# Restart on a different port
ta daemon restart --port 8800
```

Stops the running daemon (if any), then starts a fresh one. Useful after upgrades or when the daemon version doesn't match the CLI version.

#### Checking status

```bash
ta daemon status
```

Shows whether the daemon is running, its PID, port, version, project root, active agent count, and pending draft count. If the daemon is not running, it suggests `ta daemon start`.

#### Tailing logs

```bash
# Show the last 50 lines (default)
ta daemon log

# Show the last 200 lines
ta daemon log 200

# Follow in real time (like tail -f)
ta daemon log --follow
```

#### Auto-start

Commands that need the daemon (`ta shell`, `ta run`, `ta dev`) automatically start it if it's not running. You don't need to run `ta daemon start` manually in normal workflows — it's there for explicit lifecycle control, debugging, and headless/server deployments.

### Daemon Watchdog & Process Liveness

The daemon includes a background watchdog that monitors goal process health. It detects zombie goals (agent process exited but goal still shows "running"), stale questions (awaiting input for too long), and reports findings via the event system.

#### How it works

The watchdog runs every 30 seconds and checks:
- **Running goals**: Verifies the agent PID is still alive. If the process has exited and enough time has passed (configurable delay to avoid false positives), the goal transitions to `failed` automatically.
- **Stale questions**: Goals in `awaiting_input` state for longer than the threshold (default: 1 hour) emit a `question.stale` event as a reminder.

When issues are found, a `health.check` event is emitted via the SSE stream. No events are emitted when everything is healthy.

#### Configuration

Add to `.ta/daemon.toml`:

```toml
[operations]
watchdog_interval_secs = 30        # check cycle (default: 30, 0 to disable)
zombie_transition_delay_secs = 60  # wait before transitioning dead process (default: 60)
stale_question_threshold_secs = 3600  # re-notify after this (default: 1h)
```

#### Goal health in CLI output

`ta goal list` includes a HEALTH column showing the agent process state:

```
TAG                    TITLE                  STATE        HEALTH     DRAFT      VCS
------------------------------------------------------------------------------------------
shell-routing-01       Shell agent routing    running      alive      —          —
fix-auth-03            Fix OAuth token        running      dead       —          —
v0.11.2.2-01           Agent output schema    applied      —          approved   merged
```

- `alive` — agent PID is running
- `dead` — agent PID has exited (watchdog will auto-transition to failed)
- `unknown` — no PID stored (legacy goal or spawn failure)
- `—` — terminal state (no active process)

#### Process health in the API

The `/api/status` endpoint includes `process_health` and `agent_pid` fields in the `active_agents` array:

```json
{
  "active_agents": [
    {
      "goal_id": "abc123...",
      "tag": "fix-auth-01",
      "state": "running",
      "process_health": "alive",
      "agent_pid": 45678
    }
  ]
}
```

### CLI Startup Profiling

If `ta` commands feel slow (especially on Windows), use `--startup-profile` to print wall-clock timing for each startup phase:

```bash
ta --startup-profile version
# [startup-profile] arg parse:          0.3ms
# [startup-profile] project root:       0.8ms  (+0.5ms)
# [startup-profile] config load:        1.2ms  (+0.4ms)
# [startup-profile] health check:       2.1ms  (+0.9ms)
# [startup-profile] command dispatch:   2.2ms  (+0.1ms)
# [startup-profile] ---
# [startup-profile] total to dispatch:  2.2ms
```

The flag is global — works with any command. Typical bottlenecks on slow systems: config file I/O, daemon socket connect, PATH scan (`which` on Windows).

### Daemon Debugging

For diagnosing issues with agent output, request routing, or subprocess lifecycle, run the daemon in the foreground with debug logging:

```bash
ta daemon stop
RUST_LOG=ta_daemon=debug ta daemon start --foreground
```

This shows detailed logs including:
- Subprocess spawn details (binary, args, working directory, PID)
- Each stderr line from the agent process
- Stdout line counts when the agent finishes
- Elapsed time and exit codes for every agent invocation
- Broadcast channel subscriber counts (helps diagnose missed output)

Log levels: `info` (default), `debug` (verbose subprocess details), `trace` (everything).

You can also target specific modules:

```bash
# Only debug agent subprocess lifecycle
RUST_LOG=ta_daemon::api::agent=debug ta daemon start --foreground

# Debug everything including HTTP routing
RUST_LOG=ta_daemon=debug,tower_http=debug ta daemon start --foreground
```

### Daemon API

The TA daemon exposes a full HTTP API that any interface (terminal, web, Discord, Slack, email) can connect to for commands, agent conversations, and event streams.

#### Configuration

The API listens on `127.0.0.1:7700` by default. Configure via `.ta/daemon.toml`:

```toml
[server]
bind = "127.0.0.1"
port = 7700
cors_origins = ["*"]
# socket_path = ".ta/daemon.sock"   # Optional Unix domain socket path
```

#### Graceful Shutdown and PID File

The daemon handles SIGINT (Ctrl-C) and SIGTERM for graceful shutdown on Unix, and Ctrl-C on Windows. In-flight requests complete before the server stops.

A PID file is written to `.ta/daemon.pid` on startup with the process ID, port, and log path. It is automatically cleaned up on shutdown.

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

#### Sandbox Configuration

Enable command validation for the orchestrator process:

```toml
# .ta/daemon.toml
[sandbox]
enabled = true
config_path = ".ta/sandbox.toml"   # Optional — uses built-in defaults if omitted
```

When enabled, the daemon loads the sandbox config and validates commands against the allowlist before execution. See the `ta-sandbox` crate for the sandbox configuration format.

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

#### Config Hot-Reload

The daemon watches `.ta/daemon.toml` and `.ta/office.yaml` for changes and reloads configuration automatically without requiring a restart. When you edit either file, the daemon detects the change and applies the new settings to subsequent requests. Active connections are unaffected.

#### Thread Context Tracking

When a goal is started from an external channel (Discord, Slack, email), TA records the channel-specific thread identifier on the goal. Subsequent messages in the same thread automatically resolve to the correct project without requiring an `@ta <project>` prefix. This works across channels — a goal started via Discord will track the Discord thread ID, while an email-initiated goal tracks the Message-ID.

Pass `thread_id` and `project_name` when starting goals via MCP to enable thread context tracking:

```json
{
  "title": "Fix auth bug",
  "objective": "...",
  "thread_id": "discord:123456789",
  "project_name": "inventory-service"
}
```

#### Goal Chaining

Goals can reference prior goals whose output feeds into their context. Pass `context_from` (a list of goal UUIDs) when starting a goal to inject summaries of those prior goals into the new goal's context:

```json
{
  "title": "Review changes from prior goal",
  "objective": "...",
  "context_from": ["abc-123", "def-456"]
}
```

The gateway will look up each referenced goal and include its title, objective, and state in the chained context summary.

### Interactive Shell

The interactive shell (`ta shell`) opens a web-based terminal UI in your browser. It connects to the TA daemon for command execution, agent Q&A, and live event streaming.

#### Prerequisites

The shell connects to a running TA daemon. `ta shell` auto-starts the daemon if needed:

```bash
ta shell                                  # Opens web shell (auto-starts daemon)
ta shell --url http://127.0.0.1:8080      # Connect to a specific daemon URL
```

Or manage the daemon explicitly:

```bash
ta daemon start                           # Start in background
ta daemon start --foreground              # Start in foreground (for debugging)
ta daemon status                          # Check if running
```

#### Web Shell (default)

`ta shell` opens a responsive web UI at `http://127.0.0.1:7700/shell`. The web shell completely separates input from output — your typing is never blocked by agent streaming.

Features:
- **Timestamps** on every output line (HH:MM:SS)
- **Pending request counter** in the status bar
- **Agent framework** shown in status bar (e.g., "claude-code")
- **Conversation chaining** — follow-up questions retain context via `--continue`
- **Multiple concurrent SSE streams** for parallel goal output
- **Ctrl+L** to clear, Up/Down for command history
- **Auto-scroll** that pauses when you scroll up
- **Animated working indicator** — background commands show a live spinner (`Agent is working ⠿ (120s elapsed)`) updated on each heartbeat. Heartbeat lines are removed from the output stream so they don't flood the view.
- **No-heartbeat alert** — if no heartbeat arrives within the configured window (default 30 s), the indicator turns red: `Agent is working ⚠ (150s elapsed — no heartbeat)`. Clears automatically when the next heartbeat arrives.
- **Auto-tail on background commands** — when any command runs in the background, the shell immediately starts tailing its output. No manual `:tail` needed.
- **Process completion states** — when a background process exits, the working indicator is replaced by a terminal status: `✓ completed`, `✗ failed (exit N)`, or `⊘ canceled`.
- **Multi-agent tag prefix** — when two or more agents are streaming concurrently, each output line is prefixed with `[short-id]` so you can tell which output belongs to which goal. Single-agent sessions are untagged.
- **Auth failure prompt** — if an agent receives a 401 / invalid API key response, the shell shows `Agent auth failed — type 'r' to retry or 'a' to abort.` and the prompt changes to `auth-fail> [r]etry [a]bort:`.
- **Clean agent exit** — when an agent finishes, the heartbeat `[Agent is working]` line is replaced with `[agent exited <id>]` in gray. No lingering indicators.

#### Sending messages to running agents

While an agent is running in the background you can send it messages without switching modes:

```
# Route a message to the sole running agent
> how far along are you?

# Route to a specific agent by goal tag (first 8 chars of the goal ID)
>abc12345 please prioritise the auth module
```

The shell prompt temporarily changes to `[→abc12345] >` after a `>tag` dispatch, indicating the current routing target. Any non-`>` command clears the target.

Configure the input cursor in `.ta/daemon.toml`:

```toml
[shell.ui]
cursor_color = "#ffffff"     # CSS color for the input cursor (default: white)
cursor_style = "block"       # block | bar | underline (default: block)
no_heartbeat_alert_secs = 30 # How long before the no-heartbeat alert fires
```

#### Terminal Shell (opt-in)

The terminal TUI shell is available for users who prefer a terminal-native experience:

```bash
ta shell --tui                            # Full TUI shell
ta shell --classic                        # Line-mode REPL (implies --tui)
ta shell --attach sess-abc123             # Attach to agent session (implies --tui)
TA_SHELL_TUI=1 ta shell                   # Via environment variable
```

#### Daemon Version Guard

When `ta shell` or `ta dev` connects to the daemon, it checks whether the daemon version matches the CLI version. After an upgrade (e.g., `./install_local.sh`), the old daemon process may still be running with the previous version. The version guard detects this and auto-restarts the daemon.

To skip the version check (useful in CI or scripts):

```bash
ta --no-version-check shell
ta --no-version-check dev
```

#### QA Agent Auto-Start

The daemon automatically starts a QA agent session on boot (configurable in `.ta/daemon.toml`). This means the first question you type in the shell gets an immediate response — no cold-start delay.

The agent retains conversation context across requests. Each follow-up question builds on the previous context, so you can have a natural multi-turn conversation.

Configure in `.ta/daemon.toml`:

```toml
[shell.qa_agent]
auto_start = true          # Start agent on daemon boot (default: true)
agent = "claude-code"      # Which agent binary to use
idle_timeout_secs = 300    # Kill after N seconds idle (default: 5 min)
max_restarts = 3           # Max crash restarts before giving up
inject_memory = true       # Inject project memory context
```

Set `auto_start = false` to disable automatic agent spawning. The agent will start on the first question instead.

#### Starting the shell

```bash
# Web shell (default) — opens browser
ta shell

# Connect to a custom daemon URL
ta shell --url http://my-server:7700

# Terminal TUI shell
ta shell --tui

# Attach to an existing agent session (terminal only)
ta shell --tui --attach sess-abc123

# Classic line-mode shell (rustyline REPL)
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

- **Output pane** (top): Command responses, SSE event notifications, and live agent output. Events are rendered in dimmed styling; agent stdout appears in white, stderr in yellow. Auto-scrolls to bottom when at the end; holds position when scrolled up. Retained as a scrollable buffer (configurable limit, default 50,000 lines, minimum 10,000). Use Shift+Up/Down for line-by-line scroll, PgUp/PgDn for 10-line jumps, or Shift+Home/End to jump to top/bottom. A visual scrollbar appears in the right margin when the buffer exceeds the visible area.
- **Input area** (middle): Text input with cursor movement, command history (up/down), and tab-completion.
- **Status bar** (bottom): Project name, version, agent count, draft count, daemon connection indicator (green/red dot), scroll position ("line N of M" when scrolled up), "new output" badge with count when new content arrives while scrolled up, tailing indicator (green badge when streaming agent output), stdin prompt indicator (magenta badge when an agent is waiting for input), and workflow stage indicator.

#### Using the shell

Input is routed through the daemon's routing table. Recognized prefixes run as commands; everything else goes to the agent (if a session is attached):

```
ta> ta draft list                     # Runs: ta draft list
ta> run Fix the auth bug              # Shortcut for: ta run "Fix the auth bug"
ta> vcs status                        # Runs: git status (or configured VCS)
ta> git log --oneline -5              # Alias for vcs — runs git
ta> !ls -la                           # Shell escape: sh -c "ls -la"
ta> !echo $PWD                        # Shell escape: run any shell command
ta> approve abc123                    # Shortcut: ta draft approve abc123
ta> status                            # Shortcut: ta status
ta> What should we work on next?      # Sent to agent session
```

Built-in shell commands:

| Command | Description |
|---------|-------------|
| `help` / `?` | Show shell help, keybinding reference, and CLI command summary |
| `run <title>` | Start a new agent goal — shortcut for `ta run <title>` |
| `vcs <cmd>` | Run VCS commands (e.g., `vcs status`, `vcs log`). `git <cmd>` is an alias. |
| `!<cmd>` | Shell escape — run any shell command (e.g., `!ls -la`, `!echo $PWD`) |
| `:tail [id] [--lines N]` | Attach to goal output stream (read-only, `--lines` overrides backfill count) |
| `:attach [id]` | Bidirectional attach — stream output AND relay typed input to the agent's stdin |
| `:detach` | Exit attach mode (also exits on Ctrl-D) |
| `:follow-up [filter]` | List follow-up candidates (failed goals, denied drafts); filter by keyword |
| `:status` | Refresh the status bar |
| `/parallel [tag]` | Spawn an independent agent conversation; optional custom tag |
| `/switch <tag>` | Switch the active parallel session |
| `/close <tag>` | Close a named parallel session |
| `/sessions` | List all active parallel sessions |
| `@<tag> <prompt>` | Send a prompt directly to a named parallel session |
| `clear` / `Ctrl-L` | Clear the output pane |
| `Shift+Up` / `Shift+Down` | Scroll output 1 line |
| `PgUp` / `PgDn` | Scroll output one full page (with 4-line overlap) |
| Mouse wheel / touchpad scroll | Scroll output 3 lines per tick |
| Click-drag | Select text for copy (native selection always works) |
| `Shift+Home` / `Shift+End` | Scroll to top/bottom of output |
| `Tab` | Auto-complete commands |
| `Ctrl-W` | Toggle split-pane mode (agent output on the right) |
| `Ctrl-A` / `Ctrl-E` | Jump to start/end of input |
| `Ctrl-U` / `Ctrl-K` | Clear input before/after cursor |
| `Ctrl-C` | Detach from tail or cancel pending question; exit if idle |

#### Live Agent Output

When a goal starts, the shell automatically streams the agent's stdout/stderr into the output pane. Agent output appears in real-time alongside TA events — no need to switch terminals or manually `:tail`.

- **Auto-tail**: When a goal starts (detected via SSE `goal_started` event), the shell subscribes to `GET /api/goals/:id/output` and interleaves agent output with TA events. Stdout lines appear in white, stderr in yellow.
- **Manual tail**: Use `:tail` to attach to a specific goal's output, or `:tail <id>` to target a specific goal when multiple are running. Use `--lines N` to override the backfill count.
- **Bidirectional attach**: Use `:attach [id]` to both stream output and relay your input to the agent's stdin. Useful when an agent pauses for input mid-run. The prompt changes to `[attach:<id>] >` and the status bar shows a cyan badge. Press Ctrl-D or type `:detach` to exit attach mode.
- **Draft-ready notification**: When a draft finishes building, a green notification appears: `[draft ready] "title" (display-id) — run: draft view <id>`.
- **Tailing indicator**: The status bar shows a green badge while streaming agent output.
- **Split pane**: Press `Ctrl-W` to toggle a side-by-side view with agent output in the right pane. Agent output is rendered with markdown styling — bold, inline code, headers, and fenced code blocks are syntax-highlighted.
- **Agent model**: The status bar shows the detected LLM model name (e.g., "Claude Opus 4") when streaming agent output.
- **Heartbeat coalescing**: During long-running operations, heartbeat lines (`[heartbeat] still running... Ns elapsed`) update in-place instead of flooding the output. When real output arrives, the heartbeat line is pushed down naturally.
- **Text selection**: Both mouse scroll and native text selection work simultaneously. The shell uses selective ANSI mouse escapes (`?1000h` + `?1006h`) that capture scroll wheel events without intercepting click-drag, so you can select and copy text normally while still scrolling with the trackpad/mouse wheel.

#### Goal Lifecycle Notifications

When goals complete or fail, the web shell surfaces clear inline notifications:

- **Goal completed**: Shows `[goal completed] "title" (short-id) — Xm Ys` with elapsed time, followed by a next-step prompt (`run: draft view` or `check "drafts"`).
- **Goal failed**: Shows `[goal failed] (short-id) exit N — <error>` with a suggestion to run `ta goal inspect <id>` for details.

These appear automatically in the output pane — no polling needed.

#### Agent Transparency

While the agent is working, intermediate tool-use output is surfaced as dimmed progress lines in the output pane:

- **Progress lines**: Each stderr line from the agent (file reads, searches, writes) appears in a dimmed `progress` style.
- **Thinking indicator**: A sticky banner below the input shows "Agent is working... (latest: <progress>)" while a response is pending. It updates with the most recent activity.
- **Collapse on completion**: Once the agent's answer arrives, all intermediate progress lines for that request are dimmed/collapsed so the final response is prominent.

#### Parallel Agent Sessions

The web shell supports running multiple independent agent conversations at once:

```
/parallel [tag]        # Spawn a new session; auto-assigns a name like "p1" if no tag given
/parallel research     # Spawn a session named "research"
@research <prompt>     # Send a prompt to the "research" session
/switch research       # Make "research" the active session (plain input goes there)
/sessions              # List all active parallel sessions and their status
/close research        # Close the "research" session
```

The status bar shows active parallel sessions as clickable tags. Clicking a tag switches the active session. Sessions auto-close after an idle timeout (default: 30 minutes). The maximum number of concurrent sessions defaults to 3 — both limits are configurable in `daemon.toml`:

```toml
[agent]
max_parallel_sessions = 3       # Maximum concurrent parallel sessions
parallel_idle_timeout_secs = 1800  # Auto-close idle sessions after 30 min
```

#### Draft IDs

Draft packages now have human-friendly IDs derived from the goal ID. Instead of opaque UUIDs, draft IDs look like `511e0465-01` (first 8 chars of goal ID + sequence number). Follow-up drafts increment the sequence: `511e0465-02`, `511e0465-03`.

The resolver accepts display IDs, UUID prefixes, and goal title matches interchangeably. Legacy drafts without a display_id fall back to the 8-char package ID prefix.

#### Draft Listing

```bash
ta draft list                   # Show active/pending drafts (compact view)
ta draft list --all             # Show all drafts including terminal states
ta draft list --pending         # Show only pending drafts
ta draft list --applied         # Show only applied drafts
ta draft list --goal <id>       # Filter by goal
ta draft list --limit 5         # Show last 5 results
ta draft list --json            # JSON output
```

Default ordering is newest-last (chronological). The compact default view shows only active/pending drafts.

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

[shell.qa_agent]
auto_start = true           # Start agent on shell launch (default: true)
agent = "claude-code"       # Which agent binary to use
idle_timeout_secs = 300     # Kill after 5min idle, restart on next question
inject_memory = true        # Inject project memory context on start
max_restarts = 3            # Max crash restarts per session
shutdown_timeout_secs = 5   # Graceful shutdown wait

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

#### Quick setup (recommended)

Declare the plugin in your project's `.ta/project.toml` and let TA install it automatically:

```toml
[plugins.discord]
type     = "channel"
version  = ">=0.1.0"
source   = "registry:ta-channel-discord"
env_vars = ["TA_DISCORD_TOKEN", "TA_DISCORD_CHANNEL_ID"]
```

Then:

1. **Create a Discord bot** at [discord.com/developers](https://discord.com/developers/applications):
   - New Application → Bot → copy the token
   - Under OAuth2 → URL Generator: select `bot` scope with `Send Messages` and `Embed Links` permissions
   - Invite the bot to your server using the generated URL

2. **Set environment variables**:
   ```bash
   export TA_DISCORD_TOKEN="your-bot-token-here"
   export TA_DISCORD_CHANNEL_ID="123456789012345678"
   ```

3. **Resolve and install**:
   ```bash
   ta setup resolve
   ```

   TA downloads the pre-built binary for your platform, installs it to `.ta/plugins/channels/discord/`, and confirms the required env vars are set.

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

#### Manual install (build from source)

If you prefer to build the plugin yourself:

```bash
cd plugins/ta-channel-discord
cargo build --release

mkdir -p .ta/plugins/channels/discord
cp target/release/ta-channel-discord .ta/plugins/channels/discord/
cp channel.toml .ta/plugins/channels/discord/
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

#### Bidirectional mode: run TA goals from Discord

The Discord plugin supports a persistent **listen mode** that watches your channel for `/ta` slash commands and `ta ` prefixed messages, forwarding them to the daemon. Combined with GitHub's native webhook support, this enables a complete iteration cycle — run, review, apply, merge, next goal — without leaving Discord.

**One-time bot setup:**

1. In the [Discord Developer Portal](https://discord.com/developers/applications), open your TA bot application:
   - Under **Bot → Privileged Gateway Intents**, enable **Message Content Intent** (required for `ta ` prefix commands)
   - Under **OAuth2 → URL Generator**: select `bot` + `applications.commands` scopes, with permissions: `Send Messages`, `Embed Links`, `Read Message History`, `Use Slash Commands`
   - Reinvite the bot using the updated URL if you've already added it

2. Register the `/ta` slash command (run once):
   ```bash
   export TA_DISCORD_TOKEN="your-bot-token"
   export TA_DISCORD_APP_ID="your-application-id"   # from General Information tab
   ta-channel-discord --register-commands
   # Registered: /ta — "Run a Trusted Autonomy command"
   ```

3. Enable listen mode in `.ta/daemon.toml`:
   ```toml
   [channels.discord]
   listen = true   # daemon starts ta-channel-discord --listen on startup
   ```

4. Restart the daemon:
   ```bash
   pkill -f ta-daemon && ta daemon start
   ```

**Commands available from Discord:**

| Discord message | Equivalent CLI |
|---|---|
| `/ta run "Fix the auth bug"` | `ta run "Fix the auth bug"` |
| `/ta goal list` | `ta goal list` |
| `/ta goal status <tag>` | `ta goal status <tag>` |
| `/ta draft list` | `ta draft list` |
| `/ta draft approve <tag>` | `ta draft approve <tag>` |
| `/ta draft apply <tag>` | `ta draft apply <tag>` |
| `/ta draft deny <tag> "reason"` | `ta draft deny <tag> "reason"` |
| `/ta pr status` | `ta pr status` |
| `/ta status` | `ta status` |

Agent approval questions are posted as button embeds — click **Approve** or **Deny** directly in Discord without typing a command.

Rate limiting: 10 commands per 60 seconds per user (configurable in `channel.toml`). Command responses are posted as thread replies to keep the main channel clean.

**GitHub → Discord (CI and PR status):**

GitHub can push PR, push, and CI status events directly to Discord using Discord's built-in GitHub webhook format. No extra bot or app required.

1. In Discord: **Server Settings → Integrations → Webhooks → New Webhook**
   - Assign it to your TA channel
   - Copy the webhook URL

2. Append `/github` to the URL: `https://discord.com/api/webhooks/<id>/<token>/github`

3. In GitHub: **Repo → Settings → Webhooks → Add webhook**
   - Payload URL: the modified URL above
   - Content type: `application/json`
   - Events: select **Pull requests**, **Check runs**, **Pushes** (and optionally **Pull request reviews**)

You now receive GitHub PR open/merge/close, CI pass/fail, and push events in Discord alongside TA's own approval embeds.

**Complete iteration cycle from Discord:**

```
# 1. Start a goal
/ta run "Implement feature X"
  → TA responds in thread: "Goal started: implement-feature-x-01 (staging...)"
  → Agent runs, posts progress updates

# 2. Agent posts approval embed when draft is ready
  → Click [Approve] in Discord
  → Draft status: approved

# 3. Apply and push
/ta draft apply implement-feature-x-01
  → TA creates branch, commits, opens PR with auto-merge enabled
  → GitHub webhook fires: "PR #N opened"

# 4. CI runs — GitHub webhook fires: "Check passed ✓"
  → GitHub auto-merges the PR (auto_merge = true in workflow.toml)
  → GitHub webhook fires: "PR #N merged"

# 5. Start next goal
/ta run "v0.13.3"
```

#### Discord bot avatar

To set a custom icon for your TA Discord bot:

1. Open [Discord Developer Portal](https://discord.com/developers/applications) and select your TA bot application
2. Click **General Information**
3. Under **App Icon**, upload `images/Trusted Autonomy Icon Small.png` from the TA repository
4. Click **Save Changes**

The icon appears on all bot messages and in the server member list.

### Slack Channel Plugin

Slack is available as an **external channel plugin** (send-only starter). It delivers agent questions as Block Kit messages to a Slack channel. Inbound callbacks (slash commands, button clicks) are planned for a future release — reviewers respond via the TA web UI or HTTP API.

#### Quick setup (recommended)

Declare the plugin in your project's `.ta/project.toml`:

```toml
[plugins.slack]
type     = "channel"
version  = ">=0.1.0"
source   = "registry:ta-channel-slack"
env_vars = ["TA_SLACK_BOT_TOKEN", "TA_SLACK_CHANNEL_ID"]
```

Then:

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

3. **Resolve and install**:
   ```bash
   ta setup resolve
   ```

   TA downloads the pre-built binary for your platform, installs it to `.ta/plugins/channels/slack/`, and confirms the required env vars are set.

#### Manual install (build from source)

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

### Session Lifecycle

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

### Unified Policy Config

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

### Resource Mediation

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

**Checking and upgrading plugins:**

```bash
# Check all installed plugins for version drift and compatibility
ta plugin check

# Upgrade a specific plugin from source
ta plugin upgrade discord
```

`ta plugin check` compares installed plugin versions against source in `plugins/`, and validates `min_daemon_version` compatibility. `ta plugin upgrade` rebuilds the named plugin from source and re-installs it.

Plugins can declare a minimum daemon version in `channel.toml`:

```toml
min_daemon_version = "0.10.16"
```

If the running daemon is older than the declared minimum, `ta plugin check` warns about the incompatibility.

#### Channel Access Control

Restrict who can interact with channels using access control lists in `.ta/daemon.toml`:

```toml
# Global access control (applies to all channels)
[channels.access_control]
denied_users = ["bot-spam"]
allowed_roles = ["reviewer", "admin"]

# Per-plugin access control
[[channels.external]]
name = "slack"
command = "ta-channel-slack"
protocol = "json-stdio"

[channels.external.access_control]
allowed_users = ["alice", "bob"]
denied_roles = ["readonly"]
```

Rules follow deny-first precedence:
1. Denied users/roles are always blocked
2. If allowed lists are non-empty, only matching users/roles are permitted
3. Empty allowed lists mean "allow all" (after deny checks)

#### Agent Tool Access Control

Restrict which MCP tools agents can use in `.ta/daemon.toml`:

```toml
[agent]
allowed_tools = ["ta_fs_read", "ta_fs_write", "ta_draft"]   # Only these tools
denied_tools = ["ta_fs_write"]                                # Or deny specific tools
```

Deny takes precedence over allow. Empty `allowed_tools` means all tools are available (minus denied ones).

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
# Accept TA terms non-interactively (CI/scripted usage)
ta accept-terms

# View the current TA terms
ta view-terms

# Check TA acceptance status
ta terms-status

# All commands also accept --accept-terms flag
ta run "task" --accept-terms
```

### Agent Terms Consent

> **Planned for v0.10.18.4** — not yet implemented.

When using AI agents (like Claude Code or Codex), TA requires explicit consent for each agent's terms of service. This replaces the previous behavior where the daemon silently passed `--accept-terms` on the user's behalf.

```bash
# View an agent's terms summary
ta terms show claude-code

# Accept an agent's terms (interactive prompt)
ta terms accept claude-code

# Check consent status for all agents
ta terms status
```

Consent is stored per-project in `.ta/consent.json` and tracked per-agent and per-version. When an agent's terms version changes, `ta shell` will prompt you to re-accept before dispatching goals.

### Live Agent Output in Shell

> **Planned for v0.10.18.4** — not yet implemented.

When `ta shell` dispatches a goal, the daemon now runs the agent in headless mode with streaming output. For Claude Code, this uses `--output-format stream-json` to stream rich progress (text output, tool calls, results) to the shell TUI in real time.

Background commands emit a completion bookend when they finish:
- Success: `✓ <command> completed`
- Failure: `✗ <command> failed (exit N)` with the last 10 lines of stderr

Agent-specific streaming arguments can be configured in YAML agent configs using the `headless_args` field:

```yaml
# .ta/agents/claude-code.yaml
command: claude
args_template: ["{prompt}"]
headless_args: ["--output-format", "stream-json"]
```

### Agent Output Schemas

TA uses YAML schema files to parse agent output. Each agent can define its own output format — what JSON fields contain text, tool use, model names, and which events to suppress. This replaces hardcoded parsers with a configurable, extensible system.

**Schema resolution order** (first match wins):

1. **Project-local**: `.ta/agents/output-schemas/<agent-name>.yaml`
2. **User-global**: `~/.config/ta/agents/output-schemas/<agent-name>.yaml`
3. **Embedded defaults**: Ships with the `ta` binary (claude-code, claude-code-v1, codex)
4. **Passthrough**: If no schema matches, raw output is shown as-is

**Built-in schemas**: `claude-code` (current Claude Code format), `claude-code-v1` (legacy format), `codex` (OpenAI Codex CLI).

**Creating a custom schema** for a new agent:

```yaml
# .ta/agents/output-schemas/my-agent.yaml
agent: my-agent
schema_version: 1
format: stream-json

# Paths to extract model name from any JSON event.
model_paths:
  - message.model
  - model

# Events to suppress (no display).
suppress:
  - ping
  - heartbeat

extractors:
  # Extract text from assistant messages.
  - type_match: [assistant]
    output: text
    paths:
      - message.content
      - content
    content_type_filter: text   # Only collect items with "type":"text"

  # Extract streaming text chunks.
  - type_match: [content_block_delta]
    output: text
    paths:
      - delta.text

  # Show tool invocations.
  - type_match: [tool_use]
    output: tool_use
    paths:
      - name
```

**Path syntax**: Dotted paths navigate JSON objects (`message.model`). Array iteration uses `[]` suffix (`content[].text` iterates an array and extracts `text` from each item). The first non-null match in the `paths` list is used.

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

```bash
# Resolve and install plugins from .ta/project.toml
ta setup resolve

# CI mode — fails hard on any missing plugin or env var
ta setup resolve --ci

# Show plugins section (installed vs required)
ta setup show --section plugins
```

### Conversational Project Bootstrapping (`ta new`)

Create a new project through an interactive conversation with a planner agent. The agent asks about your goals, proposes a development plan, and generates a project scaffold with a PLAN.md.

```bash
# Start an interactive bootstrapping session
ta new run --name my-project

# Use a project template (rust-cli, rust-lib, ts-api, ts-app, python-cli, python-api, go-service, generic)
ta new run --name my-project --template rust-cli

# Seed from a written description (PRD, spec, brief)
ta new run --name my-project --from docs/brief.md

# Specify a version schema for the plan
ta new run --name my-project --version-schema calver

# Create in a specific directory
ta new run --name my-project --output-dir ~/projects

# Non-interactive mode (scaffold only, no agent conversation)
ta new run --name my-project --template rust-cli --non-interactive

# List available templates and version schemas
ta new templates
ta new version-schemas
```

The agent generates language-specific project scaffolds (Cargo.toml, package.json, pyproject.toml, etc.), initializes the `.ta/` workspace, and produces a PLAN.md with versioned development phases. After creation, it offers to start the first goal.

The daemon also exposes `POST /api/project/new` for remote bootstrapping from Discord, Slack, or email channel interfaces.

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

### Using TA with BMAD-METHOD

[BMAD-METHOD](https://github.com/bmad-method) is a structured AI-assisted development methodology that uses persona-based agents (Analyst, Product Manager, Architect, Developer, QA) to guide work through defined phases. BMAD wraps an underlying agent runtime — typically Claude Code — so no separate install is needed beyond `claude`.

TA governs BMAD goals the same way it governs any other agent: staging, draft review, and human approval before any change reaches your codebase. BMAD's structured phases map naturally onto TA's plan phases.

#### Setup

```bash
# 1. Initialize TA for your project (detects Claude Code, which BMAD uses)
ta init run --detect

# 2. TA generates .ta/agents/claude-code.yaml — BMAD uses the same runtime.
#    Optionally create a named BMAD agent config for clarity:
ta setup refine agents
```

When prompted during `ta setup refine agents`, select "Claude Code" as the runtime. You can rename the generated config to `bmad.yaml` and set a distinct display name:

```yaml
# .ta/agents/bmad.yaml
name: BMAD
display_name: "BMAD-METHOD Agent"
runtime: claude-code
launch:
  command: claude
  args: []
description: "Persona-driven development via BMAD-METHOD"
```

#### Running a BMAD goal

BMAD goals work like any TA goal. Pass your BMAD prompt (persona + task) as the goal title or via a context file:

```bash
# Start a goal with a BMAD persona prompt
ta run "As the Architect persona: design the authentication module for this service" \
  --agent bmad \
  --phase v0.13.2

# Or run interactively — BMAD personas work naturally in ta shell
ta shell
ta> run "As the PM persona: create a PRD for the new notification system"
```

TA stages the agent's output, builds a draft, and routes it for your review — regardless of which persona produced it.

#### Mapping BMAD phases to TA plan phases

BMAD's methodology phases (Discovery → Architecture → Implementation → QA) map well onto TA plan phases. You can link each BMAD phase run to its corresponding plan entry:

```bash
# Analyst phase
ta run "As the Analyst: define requirements for v0.13.7 goal workflows" --phase v0.13.7

# Architect phase
ta run "As the Architect: design the workflow DAG structure for v0.13.7" --phase v0.13.7

# Developer phase
ta run "As the Developer: implement the workflow engine described in the architect draft" --phase v0.13.7
```

Each run produces a separate draft. You review and apply them in sequence. The draft chain (`--follow-up`) links the implementation draft back to the architecture draft so the full reasoning is traceable.

#### BMAD + ta shell (recommended)

Running BMAD personas interactively in `ta shell` lets you guide the persona conversation and approve drafts without leaving the shell:

```bash
ta shell

ta> run "As the PM persona: write a one-page brief for X feature" --agent bmad
# ... agent works, draft produced ...
ta> draft view latest
ta> draft approve latest
```

#### Notes

- BMAD requires no special TA configuration beyond a Claude Code agent entry — the methodology is in the prompts, not the runtime.
- If your BMAD workflow produces multiple artifacts across personas, use `--follow-up` to chain them into a single reviewable draft thread.
- BMAD's QA persona pairs well with TA's `[validate]` commands in `constitution.toml` — the QA persona writes the tests, and TA's validation gate runs them before the draft is built.

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
  args: []
  timeout_secs: 30
```

The process engine spawns the configured command and communicates via newline-delimited JSON on stdin/stdout. The engine process stays alive for the lifetime of the workflow and receives `start`, `stage_completed`, `status`, `cancel`, and `inject_feedback` messages. See `crates/ta-workflow/src/process_engine.rs` for the full protocol specification.

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

## Feature Velocity Stats

`ta stats` tracks per-goal timing and outcomes in `.ta/velocity-stats.jsonl`. Data is recorded automatically when goals complete (applied, denied, cancelled, or timed out by GC).

### Viewing aggregate stats

```bash
ta stats velocity                      # overall summary
ta stats velocity --since 2026-01-01   # since a date
ta stats velocity --workflow code      # filter by workflow type
ta stats velocity --json               # raw JSON output
```

Output example:

```
Velocity Stats
──────────────────────────────────────────────────
  Total goals:        42
  Applied:            38 (90%)
  Denied:             2 (5%)
  Cancelled:          1 (2%)
  Failed:             1 (2%)
──────────────────────────────────────────────────
  Avg build time:     14m 10s
  P90 build time:     28m 5s
  Avg rework time:    2m 0s
  Total rework:       1h 24m
```

### Per-goal breakdown

```bash
ta stats velocity-detail               # latest 20 goals
ta stats velocity-detail --limit 50    # show more
ta stats velocity-detail --outcome applied   # filter by outcome
ta stats velocity-detail --since 2026-03-01 --json
```

### Exporting history

```bash
ta stats export               # JSON (default)
ta stats export --format csv  # CSV for spreadsheets
```

The velocity stats file is at `.ta/velocity-stats.jsonl` — one JSON entry per line, human-readable, and easy to query with `jq`.

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
| v0.10.11 | Shell TUI UX overhaul | Done |
| v0.10.12 | Streaming agent Q&A & status bar enhancements | Done |
| v0.10.13 | `ta plan add` command (agent-powered plan updates) | Done |
| v0.10.14 | Deferred items: shell & agent UX | Done |
| v0.10.15 | Deferred items: observability & audit | Done |
| v0.10.16 | Deferred items: platform & channel hardening | Done |
| v0.10.17 | `ta new` — conversational project bootstrapping | Done |
| v0.10.17.1 | Shell reliability & command timeout fixes | Done |
| v0.10.18 | Deferred items: workflow & multi-project | Done |
| v0.10.18.1 | Developer loop: verification, notifications & shell fixes | Done |
| v0.10.18.2 | Shell TUI: scrollback & command output visibility | In Progress |
| v0.10.18.3 | Verification streaming, heartbeat & configurable timeout | In Progress |
| v0.10.18.4 | Live agent output in shell & terms consent | Done |
| v0.10.18.5 | Agent stdin relay & interactive prompt handling | Done |
| v0.10.18.6 | `ta daemon` subcommand (start/stop/restart/status/log) | Done |
| v0.10.18.7 | Per-platform icon packaging | Done |
| v0.11.0 | Event-driven agent routing | Done |
| v0.11.0.1 | Draft apply defaults & CLI flag cleanup | Done |
| v0.11.1 | `SourceAdapter` unification & `ta sync` | Done |
| v0.11.2 | `BuildAdapter` & `ta build` | Done |
| v0.11.2.1 | Shell agent routing, TUI mouse fix & agent output diagnostics | Done |
| v0.11.2.2 | Agent output schema engine | Done |
| v0.11.2.3 | Goal & draft unified UX (tags, VCS tracking, auto-merge, heartbeat) | Done |
| v0.11.2.4 | Daemon watchdog & process liveness (zombie detection, stale questions, health events) | Done |
| v0.11.2.5 | Prompt detection hardening & version housekeeping | Done |
| v0.11.3 | Self-service operations, draft amend & plan intelligence | Done |
| v0.11.3.1 | Shell scroll & help | Done |
| v0.11.4 | Plugin registry & project manifest (`ta setup resolve`, daemon enforcement) | Done |
| v0.11.4.1 | Shell reliability: command output, text selection & heartbeat polish | Done |
| v0.11.4.2 | Shell mouse & agent session fix (scroll+selection, persistent QA, input threading) | Done |
| v0.11.4.3 | Smart input routing & intent disambiguation | Done |
| v0.11.4.4 | Constitution compliance remediation | Done |
| v0.11.4.5 | Shell large-paste compaction | Done |
| v0.11.5 | Web shell UX, agent transparency & parallel sessions | Done |
| v0.11.6 | Constitution audit completion | Done |
| v0.11.7 | Web shell stream UX polish | Done |
| v0.12.0 | Template projects & bootstrap flow | Done |
| v0.12.0.1 | PR merge & main sync completion | Done |
| v0.12.0.2 | VCS adapter externalization | Done |
| v0.12.1 | Discord channel polish | Done |
| v0.12.2 | Shell paste-at-end UX | Done |
| v0.12.2.1 | Draft compositing: parent + child chain merge | Done |
| v0.12.2.2 | Draft apply: transactional rollback on validation failure | Done |
| v0.12.2.3 | Follow-up draft completeness & injection cleanup | Done |
| v0.12.3 | Shell multi-agent UX & resilience | Done |
| v0.12.4 | Plugin template publication & registry bootstrap | Done |
| v0.12.4.1 | Shell: clear working indicator & auto-scroll fix + channel goal input | Done |
| v0.12.5 | Semantic memory: RuVector backing store & context injection | Done |
| v0.12.6 | Goal lifecycle observability & channel notification reliability | Done |
| v0.12.7 | Shell UX: working indicator clearance & scroll reliability | Done |
| v0.12.8 | Alpha bug-fixes: Discord notification flood & draft CLI disconnect | Done |
| v0.13.0 | Reflink/COW overlay optimization (APFS + Btrfs zero-cost staging) | Done |
| v0.13.1 | Autonomous operations & self-healing daemon | Done |
| v0.13.1.6 | Intelligent surface & operational runbooks | Done |
| v0.13.2 | MCP transport abstraction (TCP/Unix socket) | Done |
| v0.13.3 | Runtime adapter trait & pluggable agent runtimes | Done |
| v0.13.4 | External action governance framework (policy, capture, rate limiting, dry-run) | Done |

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
