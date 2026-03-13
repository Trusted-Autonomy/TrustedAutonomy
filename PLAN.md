# Trusted Autonomy — Development Plan

> Canonical plan for the project. Machine-parseable: each phase has a `<!-- status: done|in_progress|pending -->` marker.
> Updated automatically by `ta pr apply` when a goal with `--phase` completes.

## Versioning & Release Policy

### Plan Phases vs Release Versions

Plan phases use hierarchical IDs for readability (e.g., `v0.4.1.1`). Release versions use strict [semver](https://semver.org/) (`MAJOR.MINOR.PATCH-prerelease`). The mapping:

| Plan Phase Format | Release Version | Example |
|---|---|---|
| `vX.Y` | `X.Y.0-alpha` | v0.4 → `0.4.0-alpha` |
| `vX.Y.Z` | `X.Y.Z-alpha` | v0.4.1 → `0.4.1-alpha` |
| `vX.Y.Z.N` (sub-phase) | `X.Y.Z-alpha.N` | v0.4.1.2 → `0.4.1-alpha.2` |

**Rule**: The plan phase ID directly determines the release version. No separate mapping table needed — apply the formula above.

### Pre-release Lifecycle

| Tag | Meaning | Criteria to Enter |
|---|---|---|
| `alpha` | Active development. APIs may change. Not recommended for production. | Default for all `0.x` work |
| `beta` | Feature-complete for the release cycle. APIs stabilizing. Suitable for early adopters. | All planned phases for the minor version are done; no known critical bugs |
| `rc.N` | Release candidate. Only bug fixes accepted. | Beta testing complete; no API changes expected |
| *(none)* | Stable public release. Semver guarantees apply. | RC period passes without blocking issues |

**Current lifecycle**: All `0.x` releases are `alpha`. Beta begins when the core loop is proven (target: `v0.8` Department Runtime). Stable `1.0.0` requires: all v0.x features hardened, public API frozen, security audit complete.

**Version progression example**:
```
0.4.1-alpha → 0.4.1-alpha.1 → 0.4.1-alpha.2 → 0.4.2-alpha → ...
0.8.0-alpha → 0.8.0-beta → 0.8.0-rc.1 → 0.8.0
1.0.0-beta → 1.0.0-rc.1 → 1.0.0
```

### Release Mechanics

- **Release tags**: Each `vX.Y.0` phase is a **release point** — cut a git tag and publish binaries.
- **Patch phases** (`vX.Y.1`, `vX.Y.2`) are incremental work within a release cycle.
- **Sub-phases** (`vX.Y.Z.N`) use pre-release dot notation: `ta release run X.Y.Z-alpha.N`
- **When completing a phase**, the implementing agent MUST:
  1. Update `version` in `apps/ta-cli/Cargo.toml` to the phase's release version
  2. Update the "Current State" section in `CLAUDE.md` with the new version and test count
  3. Mark the phase as `done` in this file
- **Pre-v0.1 phases** (Phase 0–4c) used internal numbering. All phases from v0.1 onward use version-based naming.

---

## Standards & Compliance Reference

TA's architecture maps to emerging AI governance standards. Rather than bolt-on compliance, these standards inform design decisions at the phase where they naturally apply. References below indicate where TA's existing or planned capabilities satisfy a standard's requirements.

| Standard | Relevance to TA | Phase(s) |
|---|---|---|
| **ISO/IEC 42001:2023** (AI Management Systems) | Audit trail integrity (hash-chained logs), documented capability grants, human oversight records | Phase 1 (done), v0.3.3 |
| **ISO/IEC 42005:2025** (AI Impact Assessment) | Risk scoring per draft, policy decision records, impact statements in summaries | Phase 4b (done), v0.3.3 |
| **IEEE 7001-2021** (Transparency of Autonomous Systems) | Structured decision reasoning, alternatives considered, observable policy enforcement | v0.3.3, v0.4.0 |
| **IEEE 3152-2024** (Human/Machine Agency Identification) | Agent identity declarations, capability manifests, constitution references | Phase 2 (done), v0.4.0 |
| **EU AI Act Article 14** (Human Oversight) | Human-in-the-loop checkpoint, approve/reject per artifact, audit trail of decisions | Phase 3 (done), v0.3.0 (done) |
| **EU AI Act Article 50** (Transparency Obligations) | Transparent interception of external actions, human-readable action summaries | v0.5.0, v0.7.1 |
| **Singapore IMDA Agentic AI Framework** (Jan 2026) | Agent boundaries, network governance, multi-agent coordination alignment | v0.6.0, v0.7.x, v1.0 |
| **NIST AI RMF 1.0** (AI Risk Management) | Risk-proportional review, behavioral drift monitoring, escalation triggers | v0.3.3, v0.4.2 |

> **Design principle**: TA achieves compliance through architectural enforcement (staging + policy + checkpoint), not self-declaration. An agent's compliance is *verified by TA's constraints*, not *claimed by the agent*. This is stronger than transparency-only protocols like [AAP](https://github.com/mnemom/aap) — TA doesn't ask agents to declare alignment; it enforces boundaries regardless of what agents declare.

---

## Completed Phases (Phase 0 through v0.8)

> **Archived**: Phases 0–4c, v0.1–v0.1.2, v0.2.0–v0.2.4, v0.3.0–v0.3.6, v0.4.0–v0.4.5, v0.5.0–v0.5.7, v0.6.0–v0.6.3, v0.7.0–v0.7.7, v0.8.0–v0.8.2 have been moved to [`docs/PLAN-ARCHIVE.md`](docs/PLAN-ARCHIVE.md).
> All are `<!-- status: done -->` except v0.1 and v0.1.1 which are `<!-- status: deferred -->`.

---

## v0.9 — Distribution & Packaging *(release: tag v0.9.0-beta)*

### v0.9.0 — Distribution & Packaging
<!-- status: done -->
- Developer: `cargo run` + local config + Nix
- Desktop: installer with bundled daemon, git, rg/jq, common MCP servers
- Cloud: OCI image for daemon + MCP servers, ephemeral virtual workspaces
- Full web UI for review/approval (extends v0.5.2 minimal UI)
- Mobile-responsive web UI (PWA)

#### Completed
- [x] `Dockerfile` — multi-stage OCI image (build from source, slim runtime with git/jq)
- [x] `install.sh` — updated installer with `ta init`/`ta dev` instructions, Windows detection, draft terminology
- [x] PWA manifest (`manifest.json`) + mobile-responsive web UI meta tags
- [x] Web UI route for `/manifest.json` (v0.9.0)
- [x] Version bump to 0.9.0-alpha

### v0.9.1 — Native Windows Support
<!-- status: done -->
**Goal**: First-class Windows experience without requiring WSL.

- **Windows MSVC build target**: `x86_64-pc-windows-msvc` in CI release matrix.
- **Path handling**: Audit `Path`/`PathBuf` for Unix assumptions.
- **Process management**: Cross-platform signal handling via `ctrlc` crate.
- **Shell command execution**: Add `shell` field to agent YAML (`bash`, `powershell`, `cmd`). Auto-detect default.
- **Installer**: MSI installer, `winget` and `scoop` packages.
- **Testing**: Windows CI job, gate releases on Windows tests passing.

#### Completed
- [x] `x86_64-pc-windows-msvc` added to CI release matrix with Windows-specific packaging (.zip)
- [x] Windows CI job in `ci.yml` — build, test, clippy on `windows-latest`
- [x] PTY module gated with `#[cfg(unix)]` — Windows falls back to simple mode
- [x] Session resume gated with `#[cfg(unix)]` — Windows gets clear error message
- [x] `build.rs` cross-platform date: Unix `date` → PowerShell fallback
- [x] `shell` field added to `AgentLaunchConfig` for cross-platform shell selection
- [x] SHA256 checksum generation for Windows (.zip) in release workflow
- [x] `install.sh` updated with Windows detection and winget/scoop guidance

#### Deferred items moved
- MSI installer → v0.9.1-deferred (Windows distribution backlog)
- `ctrlc` crate → dropped (tokio::signal in v0.10.16 supersedes this)

### v0.9.2 — Sandbox Runner (optional hardening, Layer 2)
<!-- status: done -->
> Optional for users who need kernel-level isolation. Not a prerequisite for v1.0.

- OCI/gVisor sandbox for agent execution
- Allowlisted command execution (rg, fmt, test profiles)
- CWD enforcement — agents can't escape virtual workspace
- Command transcripts hashed into audit log
- Network access policy: allow/deny per-domain
- **Enterprise state intercept**: See `docs/enterprise-state-intercept.md`.

#### Completed
- [x] `ta-sandbox` crate fully implemented (was stub since Phase 0)
- [x] `SandboxConfig` with command allowlist, network policy, timeout, audit settings
- [x] `SandboxRunner` with `execute()` — allowlist check, forbidden args, CWD enforcement, transcript capture
- [x] Command transcript SHA-256 hashing for audit log integration
- [x] `NetworkPolicy` with per-domain allow/deny and wildcard support (`*.github.com`)
- [x] Default config with common dev tools: rg, grep, find, cat, cargo, npm, git, jq
- [x] `CommandPolicy` with `max_invocations`, `can_write`, `allowed_args`, `forbidden_args`
- [x] Path escape detection — resolves `..` and symlinks, rejects paths outside workspace
- [x] 12 tests: allowlist enforcement, forbidden args, path escape, invocation limits, transcript hashing, network policy

#### Deferred items moved
- OCI/gVisor container isolation → v0.11.5 (Runtime Adapter Trait)
- Enterprise state intercept → v0.11.5 (Runtime Adapter Trait)

### v0.9.3 — Dev Loop Access Hardening
<!-- status: done -->
**Goal**: Severely limit what the `ta dev` orchestrator agent can do — read-only project access, only TA MCP tools, no filesystem writes.

**Completed:**
- ✅ `--allowedTools` enforcement: agent config restricts to `mcp__ta__*` + read-only builtins. No Write, Edit, Bash, NotebookEdit.
- ✅ `.mcp.json` scoping: `inject_mcp_server_config_with_session()` passes `TA_DEV_SESSION_ID` and `TA_CALLER_MODE` env vars to the MCP server for per-session audit and policy enforcement.
- ✅ Policy enforcement: `CallerMode` enum (`Normal`/`Orchestrator`/`Unrestricted`) in MCP gateway. `ta_fs_write` blocked at gateway level in orchestrator mode. Security Boundaries section in system prompt.
- ✅ Audit trail: `write_dev_audit()` logs session start/end with session ID, mode, exit status to `.ta/dev-audit.log`. `TA_DEV_SESSION_ID` env var passed to agent process and MCP server for correlation.
- ✅ Escape hatch: `ta dev --unrestricted` bypasses restrictions, logs warning, removes `--allowedTools` from agent config.
- ✅ `dev-loop.yaml` alignment profile: `forbidden_actions` includes `fs_write_patch`, `fs_apply`, `shell_execute`, `network_external`, `credential_access`, `notebook_edit`.
- ✅ 12 tests: prompt security boundaries, unrestricted warning, config loading (restricted/unrestricted), audit logging, MCP injection with session, CallerMode enforcement.
- ✅ Version bump to 0.9.3-alpha.

**Deferred items resolved:**
- Sandbox runtime integration → v0.11.5 (Runtime Adapter Trait)
- Full tool-call audit logging → completed in v0.10.15 (per-tool-call audit via `audit_tool_call()`)

### v0.9.4 — Orchestrator Event Wiring & Gateway Refactor
<!-- status: done -->
**Goal**: Wire the `ta dev` orchestrator to actually launch implementation agents, handle failures, and receive events — plus refactor the growing MCP gateway.

1. **Fix `ta_goal_start` MCP → full agent launch**: Currently `ta_goal_start` via MCP only creates goal metadata — it doesn't copy the project to staging, inject CLAUDE.md, or launch the agent process. The orchestrator (`ta dev`) cannot actually launch implementation agents. Wire `ta_goal_start` (and `ta_goal_inner` with `launch:true`) to perform the full `ta run` lifecycle: overlay workspace copy → context injection → agent spawn. This is the critical blocker for `ta dev` orchestration.
2. **`GoalFailed` / `GoalError` event**: Add a `GoalFailed { goal_run_id, error, exit_code, timestamp }` variant to `TaEvent` in `crates/ta-goal/src/events.rs`. Emit it when an agent process exits with a non-zero code, crashes, or when the workspace setup fails. Currently agent failures are silent — the goal stays in "running" forever.
3. **MCP event subscription tool**: Add `ta_event_subscribe` (or similar) to the MCP gateway that lets orchestrator agents receive events without polling. Options: SSE-style streaming, long-poll, or callback registration. The orchestrator should be notified when a goal completes, fails, or produces a draft — not burn context window on repeated identical polls.
4. **MCP gateway `server.rs` refactor**: Split the 2,200+ line `server.rs` into modules by domain:
   - `server.rs` → State, config, CallerMode, ServerHandler dispatch (~200 lines)
   - `tools/goal.rs` → `ta_goal_start`, `ta_goal_status`, `ta_goal_list`, `ta_goal_inner`
   - `tools/fs.rs` → `ta_fs_read`, `ta_fs_write`, `ta_fs_list`, `ta_fs_diff`
   - `tools/draft.rs` → `ta_draft`, `ta_pr_build`, `ta_pr_status`
   - `tools/plan.rs` → `ta_plan`
   - `tools/context.rs` → `ta_context`
   - `validation.rs` → `parse_uuid`, `enforce_policy`, `validate_goal_exists` (shared helpers)

**Completed:**
- [x] `GoalFailed` event variant added to `TaEvent` (ta-goal/events.rs) and `SessionEvent` (ta-events/schema.rs) with helper constructors, serialization tests
- [x] `ta_event_subscribe` MCP tool with query/watch/latest actions, cursor-based pagination, type/goal/time filtering
- [x] MCP gateway refactored: `server.rs` split into `tools/{goal,fs,draft,plan,context,event}.rs` + `validation.rs`
- [x] `GoalFailed` emitted on agent launch failure in `ta_goal_inner` with `launch:true`, transitions goal to Failed state
- [x] `ta dev` prompt and allowed-tools list updated to include `ta_event_subscribe`
- [x] 14 MCP tools (was 13), 30 gateway tests pass, 2 new GoalFailed event tests

---                                                                                                                                                                                                                                                             
### v0.9.4.1 — Event Emission Plumbing Fix                       
<!-- status: done -->
**Goal**: Wire event emission into all goal lifecycle paths so `ta_event_subscribe` actually receives events. Currently only `GoalFailed` on spawn failure emits to FsEventStore — `GoalStarted`, `GoalCompleted`, and `DraftBuilt` are never written, making
the event subscription system non-functional for orchestrator agents.                
                                                                
**Bug**: `ta_goal_start` (MCP) creates goal metadata but does NOT: copy project to staging, inject CLAUDE.md, or launch the agent process. Goals created via MCP are stuck in `running` with no workspace and no agent. The full `ta run` lifecycle must be
wired into the MCP goal start path.

#### Completed
- ✅ **`ta_goal_start` MCP → full lifecycle**: `ta_goal_start` now always launches the implementation agent. Added `source` and `phase` parameters, always spawns `ta run --headless` which performs overlay copy, CLAUDE.md injection, agent spawn, draft build, and event emission. Goals created via MCP now actually execute — fixing `ta dev`.
- ✅ **Emit `GoalStarted`**: Both MCP `handle_goal_start()`, `handle_goal_inner()`, and CLI `ta run` emit `SessionEvent::GoalStarted` to FsEventStore after goal creation.
- ✅ **Emit `GoalCompleted`**: CLI `ta run` emits `GoalCompleted` on agent exit code 0. MCP agent launch delegates to `ta run --headless` which emits events.
- ✅ **Emit `DraftBuilt`**: Both MCP `handle_pr_build()`, `handle_draft_build()`, and CLI `ta draft build` emit `DraftBuilt` to FsEventStore.
- ✅ **Emit `GoalFailed` on all failure paths**: CLI `ta run` emits `GoalFailed` on non-zero exit code and launch failure. MCP `launch_goal_agent` and `launch_sub_goal_agent` emit on spawn failure.
- ✅ **End-to-end integration test** (3 tests in `crates/ta-mcp-gateway/src/tools/event.rs`): lifecycle event emission + goal_id/event_type filtering + cursor-based watch pattern.
- ✅ **Cursor-based watch test**: Verifies query-with-cursor polling pattern works correctly.

#### Version: `0.9.4-alpha.1`

### v0.9.5 — Enhanced Draft View Output
<!-- status: done -->
**Goal**: Make `ta draft view` output clear and actionable for reviewers — structured "what changed" summaries, design alternatives considered, and grouped visual sections.

#### Completed

- ✅ **Grouped change summary**: `ta draft view` shows a module-grouped file list with per-file classification (created/modified/deleted), one-line "what" and "why", and dependency annotations (which changes depend on each other vs. independent).
- ✅ **Alternatives considered**: New `alternatives_considered: Vec<DesignAlternative>` field on `Summary`. Each entry has `option`, `rationale`, `chosen: bool`. Populated by agents via new optional `alternatives` parameter on `ta_pr_build` MCP tool. Displayed under "Design Decisions" heading in `ta draft view`.
- ✅ **Structured view sections**: `ta draft view` output organized as Summary → What Changed → Design Decisions → Artifacts.
- ✅ **`--json` on `ta draft view`**: Full structured JSON output for programmatic consumption (already existed; now includes new fields).
- ✅ 7 new tests (3 in draft_package.rs, 4 in terminal.rs).

#### Version: `0.9.5-alpha`

---                                                  
### v0.9.5.1 — Goal Lifecycle Hygiene & Orchestrator Fixes                                                                                                                                                                                                      
<!-- status: done -->
**Goal**: Fix the bugs discovered during v0.9.5 goal lifecycle monitoring — duplicate goal creation, zombie goal cleanup, event timer accuracy, draft discoverability via MCP, and cursor-based event polling semantics.                                        
                                                                                      
#### Items                                           
                                                
1. **Fix duplicate goal creation from `ta_goal_start`**: `ta_goal_start` (MCP tool in `tools/goal.rs`) creates a goal record + emits `GoalStarted`, then spawns `ta run --headless` which creates a *second* goal for the same work. The MCP goal (`3917d3bc`)
becomes an orphan — no staging directory, no completion event, stuck in `running` forever. Fix: pass the goal_run_id from `ta_goal_start` to `ta run --headless` via a `--goal-id` flag so the subprocess reuses the existing goal record instead of creating a
new one. The MCP tool should own goal creation; `ta run --headless --goal-id <id>` should skip `GoalRun::new()` and load the existing goal.
      
2. **Fix `duration_secs: 0` in `GoalCompleted` event**: The `goal_completed` event emitted by `ta run` (in `run.rs`) reports `duration_secs: 0` even when the agent ran for ~12 minutes. The `Instant` timer is likely created at the wrong point (after agent
exit instead of before agent launch), or `duration_secs` is computed incorrectly. Fix: ensure the timer starts immediately before agent process spawn and `duration_secs` is `start.elapsed().as_secs()` at emission time.

3. **Fix `ta_draft list` MCP tool returning empty**: The `ta_draft` MCP tool with action `list` returns `{"count":0,"drafts":[]}` even when a draft package exists at `.ta/pr_packages/<id>.json`. The MCP `handle_draft_list()` searches `state.pr_packages`
(in-memory HashMap) which is only populated during the gateway's session lifetime. Drafts built by a *different* process (the `ta run --headless` subprocess) write to disk but the orchestrator's gateway never loads them. Fix: `handle_draft_list()` should
fall back to scanning `.ta/pr_packages/*.json` on disk when the in-memory map is empty, or always merge disk packages into the list.

4. **Fix cursor-inclusive event polling**: `ta_event_subscribe` with `since` returns events at exactly the `since` timestamp (inclusive/`>=`), so cursor-based polling re-fetches the last event every time. Fix: change the filter to strictly-after (`>`) so
passing the cursor from the previous response returns only *new* events. Add a test: emit event at T1, query with `since=T1` → expect 0 results; emit event at T2, query with `since=T1` → expect 1 result (T2 only).

5. **`ta goal gc` command**: New CLI command to clean up zombie goals and stale staging directories. Behavior:
    - List all goals in `.ta/goals/` with state `running` whose `updated_at` is older than a configurable threshold (default: 7 days). Transition them to `failed` with reason "gc: stale goal exceeded threshold".
    - For each non-terminal goal that has no corresponding staging directory, transition to `failed` with reason "gc: missing staging workspace".
    - `--dry-run` flag to preview what would be cleaned without making changes.
    - `--include-staging` flag to also delete staging directories for terminal-state goals (completed, failed, applied).
    - Print summary: "Transitioned N zombie goals to failed. Reclaimed M staging directories (X GB)."

6. **`ta draft gc` enhancement**: Extend existing `ta draft gc` to also clean orphaned `.ta/pr_packages/*.json` files whose linked goal is in a terminal state and older than the stale threshold.

#### Completed
- ✅ Fix duplicate goal creation: `ta_goal_start` now passes `--goal-id` to `ta run --headless` so subprocess reuses existing goal record
- ✅ Fix `duration_secs: 0`: Timer moved before agent launch (was incorrectly placed after)
- ✅ Fix `ta_draft list` MCP returning empty: `handle_draft_list()` now merges on-disk packages with in-memory map
- ✅ Fix cursor-inclusive event polling: `since` filter changed from `>=` to `>` (strictly-after) with updated cursor test
- ✅ `ta goal gc` command: zombie detection, missing-staging detection, `--dry-run`, `--include-staging`, `--threshold-days`
- ✅ `ta draft gc` enhancement: now also cleans orphaned pr_package JSON files for terminal goals past stale threshold

#### Implementation scope
- `crates/ta-mcp-gateway/src/tools/goal.rs` — pass goal_run_id to `ta run --headless`, add `--goal-id` flag handling
- `apps/ta-cli/src/commands/run.rs` — accept `--goal-id` flag, reuse existing goal record, fix duration timer placement
- `crates/ta-mcp-gateway/src/tools/draft.rs` — disk-based fallback in `handle_draft_list()`
- `crates/ta-mcp-gateway/src/tools/event.rs` — change `since` filter from `>=` to `>`, add cursor exclusivity test
- `crates/ta-events/src/store.rs` — `since` filter semantics changed to strictly-after
- `apps/ta-cli/src/commands/goal.rs` — new `gc` subcommand with `--dry-run`, `--include-staging`, and `--threshold-days` flags
- `apps/ta-cli/src/commands/draft.rs` — extend `gc` to clean orphaned pr_packages
- `apps/ta-cli/src/main.rs` — wire `goal gc` subcommand and `--goal-id` flag on `ta run`
- Tests: cursor exclusivity test updated, goal gc test added

#### Version: `0.9.5-alpha.1`

---

### v0.9.6 — Orchestrator API & Goal-Scoped Agent Tracking
<!-- status: done -->
**Goal**: Make MCP tools work without a `goal_run_id` for read-only project-wide operations, and track which agents are working on which goals for observability.

#### Items

1. **Optional `goal_run_id` on read-only MCP calls**: Make `goal_run_id` optional on tools that make sense at the project scope. If provided, scope to that goal's workspace. If omitted, use the project root. Affected tools:
   - `ta_plan read` — reads PLAN.md from project root when no goal_run_id
   - `ta_goal list` — drop goal_run_id requirement entirely (listing is always project-wide)
   - `ta_draft list` — list all drafts project-wide when no goal_run_id
   - `ta_context search/stats/list` — memory is already project-scoped
   - Keep `goal_run_id` **required** on mutation calls: `ta_plan update`, `ta_draft build/submit`, `ta_goal start` (inner), `ta_goal update`

2. **Goal-scoped agent tracking**: Track which agent sessions are actively working on each goal. New `AgentSession` struct:
   ```rust
   pub struct AgentSession {
       pub agent_id: String,        // unique per session (e.g., PID or UUID)
       pub agent_type: String,      // "claude-code", "codex", "custom"
       pub goal_run_id: Option<Uuid>, // None for orchestrator
       pub caller_mode: CallerMode,
       pub started_at: DateTime<Utc>,
       pub last_heartbeat: DateTime<Utc>,
   }
   ```
   Stored in `GatewayState.active_agents: HashMap<String, AgentSession>`. Populated when a tool call arrives (extract from `TA_AGENT_ID` env var or generate on first call). Emits `AgentSessionStarted` / `AgentSessionEnded` events.

3. **`ta_agent_status` MCP tool**: New tool for the orchestrator to query active agents:
   - `action: "list"` — returns all active agent sessions with their goal associations
   - `action: "status"` — returns a specific agent's current state
   - Useful for diagnostics: "which agents are running? are any stuck?"

4. **`CallerMode` policy enforcement**: When `CallerMode::Orchestrator`, enforce:
   - Read-only access to plan, drafts, context (no mutations without a goal)
   - Can call `ta_goal start` to create new goals
   - Cannot call `ta_draft build/submit` directly (must be inside a goal)
   - Policy engine logs the caller mode in audit entries for observability

5. **`ta status` CLI command**: Project-wide status dashboard:
   ```
   $ ta status
   Project: TrustedAutonomy (v0.9.6-alpha)
   Next phase: v0.9.5.1 — Goal Lifecycle Hygiene

   Active agents:
     agent-1 (claude-code) → goal abc123 "Implement v0.9.5.1" [running 12m]
     agent-2 (claude-code) → orchestrator [idle]

   Pending drafts: 2
   Active goals: 1
   ```

#### Completed
- [x] Optional `goal_run_id` on `ta_plan read` — falls back to project root PLAN.md
- [x] `ta_goal list` already project-scoped (no goal_run_id required)
- [x] `ta_draft list` already project-scoped (no goal_run_id required)
- [x] `ta_context search/stats/list` already project-scoped
- [x] `AgentSession` struct with agent_id, agent_type, goal_run_id, caller_mode, started_at, last_heartbeat
- [x] `GatewayState.active_agents` HashMap with touch_agent_session/end_agent_session methods
- [x] `AgentSessionStarted`/`AgentSessionEnded` event variants with helpers and tests
- [x] `ta_agent_status` MCP tool: `list` and `status` actions
- [x] `CallerMode` expanded enforcement: orchestrator blocks ta_fs_write, ta_pr_build, ta_fs_diff
- [x] `CallerMode.as_str()` and `requires_goal()` helpers
- [x] `ta status` CLI command: project name, version, next phase, active agents, pending drafts
- [x] Tests: agent session lifecycle, CallerMode enforcement, event serialization, status phase parsing

#### Deferred items resolved
- Automatic agent_id extraction → completed in v0.10.15
- Audit log entries include caller_mode → completed in v0.10.15

#### Implementation scope
- `crates/ta-mcp-gateway/src/tools/plan.rs` — optional goal_run_id, project-root fallback
- `crates/ta-mcp-gateway/src/tools/agent.rs` — new ta_agent_status tool handler
- `crates/ta-mcp-gateway/src/server.rs` — `AgentSession` tracking, `CallerMode` enforcement
- `crates/ta-goal/src/events.rs` — `AgentSessionStarted`/`AgentSessionEnded` event variants
- `apps/ta-cli/src/commands/status.rs` — new `ta status` command

#### Version: `0.9.6-alpha`

---

### v0.9.7 — Daemon API Expansion
<!-- status: done -->
**Goal**: Promote the TA daemon from a draft-review web UI to a full API server that any interface (terminal, web, Discord, Slack, email) can connect to for commands, agent conversations, and event streams.

#### Architecture

```
         Any Interface
              │
              ▼
    TA Daemon (HTTP API)
    ┌─────────────────────────────┐
    │  /api/cmd      — run ta CLI │
    │  /api/agent    — talk to AI │
    │  /api/events   — SSE stream │
    │  /api/status   — project    │
    │  /api/drafts   — review     │  (existing)
    │  /api/memory   — context    │  (existing)
    ├─────────────────────────────┤
    │  Auth: Bearer token or mTLS │
    │  CORS: configurable origins │
    │  Rate limit: per-token      │
    └─────────────────────────────┘
```

#### Items

1. **Command execution API** (`POST /api/cmd`): Execute any `ta` CLI command and return the output. The daemon forks the `ta` binary with the provided arguments, captures stdout/stderr, and returns them as JSON.
   ```json
   // Request
   { "command": "ta draft list" }
   // Response
   { "exit_code": 0, "stdout": "ID  Status  Title\nabc  pending  Fix auth\n", "stderr": "" }
   ```
   - Command allowlist in `.ta/daemon.toml` — by default, all read commands allowed; write commands (approve, deny, apply, goal start) require explicit opt-in or elevated token scope.
   - Execution timeout: configurable, default 30 seconds.

2. **Agent session API** (`/api/agent/*`): Manage a headless agent subprocess that persists across requests. The daemon owns the agent's lifecycle.
   - `POST /api/agent/start` — Start a new agent session. Launches the configured agent in headless mode with MCP sidecar. Returns a `session_id`.
     ```json
     { "agent": "claude-code", "context": "optional initial prompt" }
     → { "session_id": "sess-abc123", "status": "running" }
     ```
   - `POST /api/agent/ask` — Send a prompt to the active agent session and stream the response.
     ```json
     { "session_id": "sess-abc123", "prompt": "What should we work on next?" }
     → SSE stream of agent response chunks
     ```
   - `GET /api/agent/sessions` — List active agent sessions.
   - `DELETE /api/agent/:session_id` — Stop an agent session.
   - Agent sessions respect the same routing config (`.ta/shell.toml`) — if the "prompt" looks like a command, the daemon can auto-route it to `/api/cmd` instead. This makes every interface behave like `ta shell`.

3. **Event stream API** (`GET /api/events`): Server-Sent Events (SSE) endpoint that streams TA events in real-time.
   - Subscribes to the `FsEventStore` (same as `ta shell` would).
   - Supports `?since=<cursor>` for replay from a point.
   - Event types: `draft_built`, `draft_approved`, `draft_denied`, `goal_started`, `goal_completed`, `goal_failed`, `drift_detected`, `agent_session_started`, `agent_session_ended`.
   - Each event includes `id` (cursor), `type`, `timestamp`, and `data` (JSON payload).
   ```
   event: draft_built
   id: evt-001
   data: {"draft_id":"abc123","title":"Fix auth","artifact_count":3}

   event: goal_completed
   id: evt-002
   data: {"goal_run_id":"def456","title":"Phase 1","duration_secs":720}
   ```

4. **Project status API** (`GET /api/status`): Single endpoint returning the full project dashboard — same data as `ta status` (v0.9.6) but as JSON.
   ```json
   {
     "project": "TrustedAutonomy",
     "version": "0.9.8-alpha",
     "current_phase": { "id": "v0.9.5.1", "title": "Goal Lifecycle Hygiene", "status": "pending" },
     "active_agents": [
       { "agent_id": "agent-1", "type": "claude-code", "goal": "abc123", "running_secs": 720 }
     ],
     "pending_drafts": 2,
     "active_goals": 1,
     "recent_events": [ ... ]
   }
   ```

5. **Authentication & authorization**: Bearer token authentication for remote access.
   - Token management: `ta daemon token create --scope read,write` → generates a random token stored in `.ta/daemon-tokens.json`.
   - Scopes: `read` (status, list, view, events), `write` (approve, deny, apply, goal start, agent ask), `admin` (daemon config, token management).
   - Local connections (127.0.0.1) can optionally bypass auth for solo use.
   - Token is passed via `Authorization: Bearer <token>` header.
   - All API calls logged to audit trail with the token identity.

6. **Daemon configuration** (`.ta/daemon.toml`):
   ```toml
   [server]
   bind = "127.0.0.1"       # "0.0.0.0" for remote access
   port = 7700
   cors_origins = ["*"]      # restrict in production

   [auth]
   require_token = true       # false for local-only use
   local_bypass = true        # skip auth for 127.0.0.1

   [commands]
   # Allowlist for /api/cmd (glob patterns)
   allowed = ["ta draft *", "ta goal *", "ta plan *", "ta status", "ta context *"]
   # Commands that require write scope
   write_commands = ["ta draft approve *", "ta draft deny *", "ta draft apply *", "ta goal start *"]

   [agent]
   max_sessions = 3           # concurrent agent sessions
   idle_timeout_secs = 3600   # kill idle sessions after 1 hour
   default_agent = "claude-code"

   [routing]
   use_shell_config = true    # use .ta/shell.toml for command vs agent routing
   ```

7. **Bridge protocol update**: Update the Discord/Slack/Gmail bridge templates to use the daemon API instead of file-based exchange. The bridges become thin HTTP clients:
   - Message received → `POST /api/cmd` or `/api/agent/ask`
   - Subscribe to `GET /api/events` for notifications
   - No more file watching or exchange directory

#### Implementation scope
- `crates/ta-daemon/src/api/mod.rs` — API module organization
- `crates/ta-daemon/src/api/cmd.rs` — command execution endpoint
- `crates/ta-daemon/src/api/agent.rs` — agent session management, headless subprocess, SSE streaming
- `crates/ta-daemon/src/api/events.rs` — SSE event stream from FsEventStore
- `crates/ta-daemon/src/api/status.rs` — project status endpoint
- `crates/ta-daemon/src/api/auth.rs` — token authentication, scope enforcement
- `crates/ta-daemon/src/web.rs` — integrate new API routes alongside existing draft/memory routes
- `crates/ta-daemon/src/api/input.rs` — unified `/api/input` endpoint with routing table dispatch
- `crates/ta-daemon/src/api/router.rs` — `.ta/shell.toml` parsing, prefix matching, shortcut expansion
- `crates/ta-daemon/src/socket.rs` — Unix domain socket listener (`.ta/daemon.sock`)
- `crates/ta-daemon/Cargo.toml` — add `tokio-stream` (SSE), `rand` (token gen), `hyperlocal` (Unix socket)
- `templates/daemon.toml` — default daemon configuration
- `templates/shell.toml` — default routing config (routes + shortcuts)
- `templates/channels/discord-bridge-api.js` — updated bridge using daemon API
- `templates/channels/slack-bridge-api.js` — updated bridge using daemon API
- `docs/USAGE.md` — daemon API documentation, remote access setup, routing customization
- Tests: command execution with auth, agent session lifecycle, SSE event stream, token scope enforcement, input routing dispatch, Unix socket connectivity

8. **Configurable input routing** (`.ta/shell.toml`): The daemon uses this config to decide whether input is a command or an agent prompt. Shared by all interfaces — `ta shell`, web UI, Discord/Slack bridges all route through the same logic.
   ```toml
   # Routes: prefix → local command execution
   # Anything not matching a route goes to the agent
   [[routes]]
   prefix = "ta "           # "ta draft list" → runs `ta draft list`
   command = "ta"
   strip_prefix = true

   [[routes]]
   prefix = "git "
   command = "git"
   strip_prefix = true

   [[routes]]
   prefix = "cargo "
   command = "./dev cargo"   # project's nix wrapper
   strip_prefix = true

   [[routes]]
   prefix = "!"             # shell escape: "!ls -la" → runs "ls -la"
   command = "sh"
   args = ["-c"]
   strip_prefix = true

   # Shortcuts: keyword → expanded command
   [[shortcuts]]
   match = "approve"         # "approve abc123" → "ta draft approve abc123"
   expand = "ta draft approve"

   [[shortcuts]]
   match = "deny"
   expand = "ta draft deny"

   [[shortcuts]]
   match = "view"
   expand = "ta draft view"

   [[shortcuts]]
   match = "apply"
   expand = "ta draft apply"

   [[shortcuts]]
   match = "status"
   expand = "ta status"

   [[shortcuts]]
   match = "plan"
   expand = "ta plan list"

   [[shortcuts]]
   match = "goals"
   expand = "ta goal list"

   [[shortcuts]]
   match = "drafts"
   expand = "ta draft list"
   ```
   - Default routing built in if no `.ta/shell.toml` exists
   - `POST /api/input` — unified endpoint: daemon checks routing table, dispatches to `/api/cmd` or `/api/agent/ask` accordingly. Clients don't need to know the routing rules — they just send the raw input.

9. **Unix socket for local clients**: In addition to HTTP, the daemon listens on `.ta/daemon.sock` (Unix domain socket). Local clients (`ta shell`, web UI) connect here for zero-config, zero-auth, low-latency access. Remote clients use HTTP with bearer token auth.

#### Completed
- [x] Command execution API (`POST /api/cmd`) with allowlist validation, write scope enforcement, configurable timeout
- [x] Agent session API (`/api/agent/start`, `/api/agent/ask`, `/api/agent/sessions`, `DELETE /api/agent/:id`) with session lifecycle management and max session limits
- [x] SSE event stream API (`GET /api/events`) with cursor-based replay (`?since=`) and event type filtering (`?types=`)
- [x] Project status API (`GET /api/status`) with JSON dashboard (project, version, phase, agents, drafts, events)
- [x] Bearer token authentication middleware with scopes (read/write/admin), local bypass for 127.0.0.1
- [x] Token store (`TokenStore`) with create/validate/revoke persisted in `.ta/daemon-tokens.json`
- [x] Daemon configuration (`.ta/daemon.toml`) with server, auth, commands, agent, routing sections
- [x] Configurable input routing (`.ta/shell.toml`) with prefix-based routes and shortcut expansion
- [x] Unified input endpoint (`POST /api/input`) dispatching to cmd or agent via routing table
- [x] Route listing endpoint (`GET /api/routes`) for tab completion
- [x] Combined router merging new API routes with existing draft/memory web UI routes
- [x] API-only mode (`--api` flag) and co-hosted MCP+API mode
- [x] Default template files (`templates/daemon.toml`, `templates/shell.toml`)
- [x] Version bumps: ta-daemon 0.9.7-alpha, ta-cli 0.9.7-alpha
- [x] 35 tests: config roundtrip, token CRUD, session lifecycle/limits, input routing, glob matching, status parsing, auth scopes

#### Deferred items moved
- Unix domain socket listener → v0.11.4 (MCP Transport Abstraction)
- Headless agent subprocess → superseded by TUI shell (v0.9.8.3)
- Bridge template updates → superseded by external plugin architecture (v0.10.2)

#### Version: `0.9.7-alpha`

---

### v0.9.8 — Interactive TA Shell (`ta shell`)
<!-- status: done -->
**Goal**: A thin terminal REPL client for the TA daemon — providing a single-terminal interactive experience for commands, agent conversation, and event notifications. The shell is a daemon client, not a standalone tool.

#### Architecture

```
$ ta shell
┌──────────────────────────────────────────┐
│  TA Shell v0.9.8                         │
│  Project: TrustedAutonomy                │
│  Next: v0.9.5.1 — Goal Lifecycle Hygiene │
│  Agent: claude-code (ready)              │
├──────────────────────────────────────────┤
│                                          │
│  ta> What should we work on next?        │
│  [Agent]: Based on PLAN.md, the next     │
│  pending phase is v0.9.5.1...            │
│                                          │
│  ta> ta draft list                       │
│  ID       Status   Title                 │
│  abc123   pending  Fix login flow        │
│                                          │
│  ta> ta draft view abc123                │
│  [structured diff output]               │
│                                          │
│  ta> approve abc123                      │
│  ✅ Approved abc123                       │
│                                          │
│  ── Event: draft ready (goal def456) ──  │
│                                          │
│  ta> view def456-draft                   │
│  [diff output]                           │
│                                          │
│  ta> deny def456-draft: needs error      │
│     handling for the retry case          │
│  ❌ Denied def456-draft                   │
│                                          │
└──────────────────────────────────────────┘
```

#### Design: Shell as Daemon Client

The shell does **no business logic** — all command execution, agent management, and event streaming live in the daemon (v0.9.7). The shell is ~200 lines of REPL + rendering:

```
ta shell
   │
   ├── Connect to daemon (.ta/daemon.sock or localhost:7700)
   │
   ├── GET /api/status → render header (project, phase, agents)
   │
   ├── GET /api/events (SSE) → background thread renders notifications
   │
   └── REPL loop:
       │
       ├── Read input (rustyline)
       │
       ├── POST /api/input { "text": "<user input>" }
       │   (daemon routes: command → /api/cmd, else → /api/agent/ask)
       │
       └── Render response (stream agent SSE, or show command output)
```

This means:
- **One code path**: command routing, agent sessions, events — all in the daemon. Shell, web UI, Discord, Slack all use the same APIs.
- **Shell is trivially simple**: readline + HTTP client + SSE renderer.
- **No subprocess management in the shell**: daemon owns agent lifecycle.
- **Shell can reconnect**: if the shell crashes, `ta shell` reconnects to the existing daemon session (agent keeps running).

#### Items

1. **Shell REPL core**: `ta shell` command:
   - Auto-starts the daemon if not running (`ta daemon start` in background)
   - Connects via Unix socket (`.ta/daemon.sock`) — falls back to HTTP if socket not found
   - Prompt: `ta> ` (configurable in `.ta/shell.toml`)
   - All input sent to `POST /api/input` — daemon handles routing
   - History: rustyline with persistent history at `.ta/shell_history`
   - Tab completion: fetches routed prefixes and shortcuts from `GET /api/routes`

2. **Streaming agent responses**: When `/api/input` routes to the agent, the daemon returns an SSE stream. The shell renders chunks as they arrive (like a chat interface). Supports:
   - Partial line rendering (agent "typing" effect)
   - Markdown rendering (code blocks, headers, bold — via `termimad` or similar)
   - Interrupt: Ctrl+C cancels the current agent response

3. **Inline event notifications**: Background SSE connection to `GET /api/events`. Notifications rendered between the prompt and agent output:
   - `── 📋 Draft ready: "Fix auth" (view abc123) ──`
   - `── ✅ Goal completed: "Phase 1" (12m) ──`
   - `── ❌ Goal failed: "Phase 2" — timeout ──`
   - Non-disruptive: notifications don't break the current input line

4. **Session state header**: On startup and periodically, display:
   ```
   TrustedAutonomy v0.9.8 │ Next: v0.9.5.1 │ 2 drafts │ 1 agent running
   ```
   Updated when events arrive. Compact one-liner at top.

5. **`ta shell --init`**: Generate the default `.ta/shell.toml` routing config for customization.

6. **`ta shell --attach <session_id>`**: Attach to an existing daemon agent session (useful for reconnecting after a disconnect or switching between sessions).

#### Completed

- [x] Shell REPL core: `ta shell` command with rustyline, persistent history at `~/.ta/shell_history`, `ta> ` prompt
- [x] Input routing through `POST /api/input` — daemon handles command vs agent dispatch
- [x] Tab completion from `GET /api/routes` (shortcuts + built-in shell commands)
- [x] Status header on startup from `GET /api/status` — project, version, next phase, drafts, agents
- [x] Background SSE event listener (`GET /api/events`) rendering inline notifications
- [x] `ta shell --init` generates default `.ta/shell.toml` routing config
- [x] `ta shell --attach <session_id>` attaches to existing daemon agent session
- [x] `ta shell --url <url>` for custom daemon URL override
- [x] Built-in shell commands: help, :status, exit/quit/:q
- [x] Default routing config template (`apps/ta-cli/templates/shell.toml`)
- [x] 8 tests (SSE rendering, completions, config init, daemon URL resolution)

#### Deferred items resolved
- Unix domain socket connection → v0.11.4 (MCP Transport Abstraction)
- Auto-start daemon → completed in v0.10.16
- Streaming agent response rendering → completed in v0.10.12 (streaming Q&A)
- Ctrl+C interrupt → completed in v0.10.14 (Ctrl-C detach)
- Non-disruptive event notifications → completed in v0.10.11 (TUI auto-tail + notifications)
- Periodic status header refresh → completed in v0.10.12 (status bar enhancements)

#### Implementation scope
- `apps/ta-cli/src/commands/shell.rs` — REPL core (~200 lines), daemon client, SSE rendering
- `apps/ta-cli/Cargo.toml` — add `rustyline`, `reqwest` (HTTP client), `tokio-stream` (SSE)
- `apps/ta-cli/templates/shell.toml` — default routing config
- `docs/USAGE.md` — `ta shell` documentation

#### Why so simple?
All complexity lives in the daemon (v0.9.7). The shell is deliberately thin — just a rendering layer. This means any bug fix or feature in the daemon benefits all interfaces (shell, web, Discord, Slack, email) simultaneously.

#### Why not enhance `ta dev`?
`ta dev` gives the agent the terminal (agent drives, human reviews elsewhere). `ta shell` gives the human the terminal (human drives, agent assists). Both connect to the same daemon. `ta dev` is for autonomous work; `ta shell` is for interactive exploration and management.

#### Version: `0.9.8-alpha`

---

### v0.9.8.1 — Auto-Approval, Lifecycle Hygiene & Operational Polish
<!-- status: done -->
**Goal**: Three themes that make TA reliable for sustained multi-phase use:
- **(A) Policy-driven auto-approval**: Wire the policy engine into draft review so drafts matching configurable conditions are auto-approved — preserving full audit trail and the ability to tighten rules at any time.
- **(B) Goal lifecycle & GC**: Unified `ta gc`, goal history ledger, `ta goal list --active` filtering, and event store pruning (items 9–10).
- **(C) Operational observability**: Actionable error messages, timeout diagnostics, daemon version detection, status line accuracy (items 9, plus CLAUDE.md observability mandate).

#### How It Works

```
Agent calls ta_draft submit
        │
        ▼
  PolicyEngine.should_auto_approve_draft(draft, policy)?
        │
        ├── Evaluate conditions:
        │   ├── max files changed?
        │   ├── max lines changed?
        │   ├── all paths in allowed_paths?
        │   ├── no paths in blocked_paths?
        │   ├── tests pass? (if require_tests_pass)
        │   ├── clippy clean? (if require_clean_clippy)
        │   ├── agent trusted? (per-agent security_level)
        │   └── phase in allowed_phases?
        │
        ├── ALL conditions met ──► Auto-approve
        │     ├── DraftStatus::Approved { approved_by: "policy:auto" }
        │     ├── Audit entry: auto_approved, conditions matched
        │     ├── Event: DraftAutoApproved { draft_id, reason }
        │     └── If auto_apply enabled: immediately apply changes
        │
        └── ANY condition fails ──► Route to ReviewChannel (human review)
              └── Review request includes: "Why review needed:
                  draft touches src/main.rs (blocked path)"
```

#### Policy Configuration (`.ta/policy.yaml`)

```yaml
version: "1"
security_level: checkpoint

auto_approve:
  read_only: true               # existing: auto-approve read-only actions
  internal_tools: true           # existing: auto-approve ta_* MCP calls

  # NEW: draft-level auto-approval
  drafts:
    enabled: false               # master switch (default: off — opt-in only)
    auto_apply: false            # if true, also run `ta draft apply` after auto-approve
    git_commit: false            # if auto_apply, also create a git commit

    conditions:
      # Size limits — only auto-approve small, low-risk changes
      max_files: 5
      max_lines_changed: 200

      # Path allowlist — only auto-approve changes to safe paths
      # Uses glob patterns, matched against artifact resource_uri
      allowed_paths:
        - "tests/**"
        - "docs/**"
        - "*.md"
        - "**/*_test.rs"

      # Path blocklist — never auto-approve changes to these (overrides allowlist)
      blocked_paths:
        - ".ta/**"
        - "Cargo.toml"
        - "Cargo.lock"
        - "**/main.rs"
        - "**/lib.rs"
        - ".github/**"

      # Verification — run checks before auto-approving
      require_tests_pass: false   # run `cargo test` (or configured test command)
      require_clean_clippy: false  # run `cargo clippy` (or configured lint command)
      test_command: "cargo test --workspace"
      lint_command: "cargo clippy --workspace --all-targets -- -D warnings"

      # Scope limits
      allowed_phases:              # only auto-approve for these plan phases
        - "tests"
        - "docs"
        - "chore"

# Per-agent security overrides
agents:
  claude-code:
    security_level: checkpoint    # always human review for this agent
  codex:
    security_level: open          # trusted for batch work
    auto_approve:
      drafts:
        enabled: true
        conditions:
          max_files: 3
          max_lines_changed: 100
          allowed_paths: ["tests/**"]

# Per-goal constitutional approval (v0.4.3 — already exists)
# Constitutions define per-goal allowed actions. Auto-approval
# respects constitutions: if a constitution is stricter than
# the project policy, the constitution wins.
```

#### Items

1. **`AutoApproveDraftConfig` struct**: Add to `PolicyDocument` under `auto_approve.drafts`:
   - `enabled: bool` (master switch, default false)
   - `auto_apply: bool` (also apply after approve)
   - `git_commit: bool` (create commit if auto-applying)
   - `conditions: AutoApproveConditions` (size limits, path rules, verification, phase limits)

2. **`should_auto_approve_draft()` function**: Core evaluation logic in `ta-policy`:
   - Takes `&DraftPackage` + `&PolicyDocument` + optional `&AgentProfile`
   - Returns `AutoApproveDecision`:
     - `Approved { reasons: Vec<String> }` — all conditions met, with audit trail of why
     - `Denied { blockers: Vec<String> }` — which conditions failed, included in review request
   - Condition evaluation order: enabled check → size limits → path rules → phase limits → agent trust level. Short-circuits on first failure.

3. **Path matching**: Glob-based matching against `Artifact.resource_uri`:
   - `allowed_paths`: if set, ALL changed files must match at least one pattern
   - `blocked_paths`: if ANY changed file matches, auto-approval is denied (overrides allowed_paths)
   - Uses the existing `glob` crate pattern matching

4. **Verification integration**: Optionally run test/lint commands before auto-approving:
   - `require_tests_pass: true` → runs configured `test_command` in the staging workspace
   - `require_clean_clippy: true` → runs configured `lint_command`
   - Both default to false (verification adds latency; opt-in only)
   - Verification runs in the staging directory, not the source — safe even if tests have side effects
   - Timeout: configurable, default 5 minutes

5. **Gateway/daemon wiring**: In the draft submit handler:
   - Before routing to ReviewChannel, call `should_auto_approve_draft()`
   - If approved: set `DraftStatus::Approved { approved_by: "policy:auto", approved_at }`, dispatch `DraftAutoApproved` event
   - If denied: include blockers in the `InteractionRequest` so the human knows why they're being asked
   - If `auto_apply` enabled: immediately call the apply logic (copy staging → source, optional git commit)

6. **`DraftAutoApproved` event**: New `TaEvent` variant:
   ```rust
   DraftAutoApproved {
       draft_id: String,
       goal_run_id: Uuid,
       reasons: Vec<String>,       // "all files in tests/**, 3 files, 45 lines"
       auto_applied: bool,
       timestamp: DateTime<Utc>,
   }
   ```

7. **Audit trail**: Auto-approved drafts are fully audited:
   - Audit entry includes: which conditions were evaluated, which matched, policy document version
   - `approved_by: "policy:auto"` distinguishes from human approvals
   - `ta audit verify` includes auto-approved drafts in the tamper-evident chain

8. **`ta policy check <draft_id>`**: CLI command to dry-run the auto-approval evaluation:
   ```
   $ ta policy check abc123
   Draft: abc123 — "Add unit tests for auth module"

   Auto-approval evaluation:
     ✅ enabled: true
     ✅ max_files: 3 ≤ 5
     ✅ max_lines_changed: 87 ≤ 200
     ✅ all paths match allowed_paths:
        tests/auth_test.rs → tests/**
        tests/fixtures/auth.json → tests/**
        tests/README.md → *.md
     ✅ no blocked paths matched
     ⏭️  require_tests_pass: skipped (not enabled)
     ✅ phase "tests" in allowed_phases

   Result: WOULD AUTO-APPROVE
   ```

9. **Status line: distinguish active vs tracked agents/goals**: The daemon `/api/status` endpoint currently counts all `GoalRun` entries with state `running` or `pr_ready`, including stale historical goals with no live process. This inflates the agent/goal count shown in `ta shell` and the Console. Fix:
   - Add `active_agents` (goals with a live process or updated within the last hour) vs `total_tracked` (all non-terminal goals) to the status response
   - Shell status line shows only active: `2 agents running` not `26 agents`
   - `ta status --all` shows the full breakdown including stale entries
   - Detection heuristic: if `updated_at` is older than `idle_timeout_secs` (from daemon config, default 30 min) and state is `running`, classify as stale

10. **Goal lifecycle GC & history ledger**: Enhance `ta goal gc` and `ta draft gc` into a unified `ta gc` with a persistent history ledger so archived goals remain queryable.
    - **Goal history ledger** (`.ta/goal-history.jsonl`): When GC archives or removes a goal, append a compact summary line:
      ```jsonl
      {"id":"ca306e4d","title":"Implement v0.9.8.1","state":"applied","phase":"v0.9.8.1","agent":"claude-code","created":"2026-03-06","completed":"2026-03-06","duration_mins":42,"draft_id":"abc123","artifact_count":15,"lines_changed":487}
      ```
    - **`ta gc`** — unified top-level command that runs both goal GC and draft GC in one pass:
      - Transitions stale `running` goals to `failed` (existing behavior)
      - Also handles `pr_ready` goals older than threshold (draft built but never reviewed)
      - Writes history summary before archiving/removing goal JSON files
      - Removes staging directories for all terminal goals
      - Cleans orphaned draft package JSON files
      - Flags: `--dry-run`, `--threshold-days N` (default 7), `--all` (ignore threshold, GC everything terminal), `--archive` (move to `.ta/goals/archive/` instead of deleting)
      - Prints disk usage summary: "Reclaimed 93 GB across 56 staging directories"
    - **`ta goal history`** — read and render the history ledger:
      - Default: compact table of recent goals (last 20)
      - `--phase v0.9.8.1` — filter by plan phase
      - `--since 2026-03-01` — filter by date
      - `--agent claude-code` — filter by agent
      - `--json` — raw JSONL output for scripting
    - **`ta goal list --active`** — filter to non-terminal goals only (default behavior change: `ta goal list` shows only active, `ta goal list --all` shows everything including terminal)
    - **Event store pruning**: `ta gc` also prunes events linked to archived goals from the daemon's event store, preventing stale event replay

#### Security Model

- **Default: off** — auto-approval must be explicitly enabled. Fresh `ta init` projects start with `drafts.enabled: false`.
- **Tighten only**: `PolicyCascade` merges layers with "most restrictive wins". A constitution or agent profile can tighten but never loosen project-level rules.
- **Blocked paths override allowed paths**: A file matching `blocked_paths` forces human review even if it also matches `allowed_paths`.
- **Audit everything**: Auto-approved drafts have the same audit trail as human-approved ones. `ta audit log` shows them with `policy:auto` attribution.
- **Escape hatch**: `ta draft submit --require-review` forces human review regardless of auto-approval config. The agent cannot bypass this flag (it's a CLI flag, not an MCP parameter).

#### Implementation scope
- `crates/ta-policy/src/document.rs` — `AutoApproveDraftConfig`, `AutoApproveConditions` structs
- `crates/ta-policy/src/auto_approve.rs` — `should_auto_approve_draft()`, `AutoApproveDecision`, condition evaluation, path matching
- `crates/ta-policy/src/engine.rs` — wire auto-approve check into policy evaluation
- `crates/ta-mcp-gateway/src/tools/draft.rs` — check auto-approve before routing to ReviewChannel
- `crates/ta-daemon/src/api/cmd.rs` — same check in daemon's draft submit handler
- `crates/ta-goal/src/events.rs` — `DraftAutoApproved` event variant
- `apps/ta-cli/src/commands/policy.rs` — `ta policy check` dry-run command
- `apps/ta-cli/src/commands/gc.rs` — unified `ta gc` command with history ledger writes
- `apps/ta-cli/src/commands/goal.rs` — `ta goal list --active`, `ta goal history` subcommand
- `crates/ta-goal/src/history.rs` — `GoalHistoryEntry` struct, append/read/filter for `.ta/goal-history.jsonl`
- `docs/USAGE.md` — auto-approval configuration guide, security model explanation, goal GC & history docs
- Tests: condition evaluation (each condition individually), path glob matching, tighten-only cascade, verification command execution, auto-apply flow, audit trail correctness, history ledger write/read round-trip, GC threshold filtering

#### Completed

- [x] `AutoApproveDraftConfig` and `AutoApproveConditions` structs in `ta-policy/src/document.rs`
- [x] `should_auto_approve_draft()` function with `DraftInfo` / `AutoApproveDecision` types in `ta-policy/src/auto_approve.rs` (14 tests)
- [x] Cascade tighten-only merge for draft auto-approve conditions in `cascade.rs` (2 tests)
- [x] `DraftAutoApproved` event variant in `ta-goal/src/events.rs` (1 test)
- [x] Gateway wiring: auto-approve check in `ta-mcp-gateway/src/tools/draft.rs` before ReviewChannel
- [x] `GoalHistoryEntry` and `GoalHistoryLedger` in `ta-goal/src/history.rs` (6 tests)
- [x] Unified `ta gc` command in `apps/ta-cli/src/commands/gc.rs` with history writes, staging cleanup, orphan draft cleanup
- [x] `ta policy check <draft_id>` and `ta policy show` in `apps/ta-cli/src/commands/policy.rs`
- [x] `ta goal list --active` (default: non-terminal only) and `ta goal list --all`
- [x] `ta goal history` subcommand with `--phase`, `--agent`, `--since`, `--json`, `--limit` filters
- [x] Status endpoint: `active` flag on `AgentInfo` distinguishing active (updated within 10m) vs tracked agents

#### Deferred items resolved
- Verification integration in auto-approve → completed in v0.10.15
- `auto_apply` flow → completed in v0.10.15
- Event store pruning → completed in v0.10.15
- `ta draft apply --require-review` flag → completed in v0.10.15
- Audit trail for auto-approved drafts → completed in v0.10.15

#### Version: `0.9.8-alpha.1`

---

### v0.9.8.1.1 — Unified Allow/Deny List Pattern
<!-- status: done -->
**Goal**: Standardize all allowlist/blocklist patterns across TA to support both allow and deny lists with consistent semantics: deny takes precedence over allow, empty allow = allow all, empty deny = deny nothing.

#### Problem
TA has multiple places that use allowlists or blocklists, each with slightly different semantics:
- **Daemon command routing** (`config.rs`): `commands.allowed` only — no deny list
- **Auto-approval paths** (`policy.yaml`): `allowed_paths` + `blocked_paths` (deny wins)
- **Agent tool access**: implicit per-mode (full/plan/review-only) — no configurable lists
- **Channel reviewer access**: `allowed_roles` / `allowed_users` — no deny
- **Sandbox command allowlist** (`ta-sandbox`): allow-only

These should share a common pattern.

#### Design

```rust
/// Reusable allow/deny filter. Deny always takes precedence.
pub struct AccessFilter {
    pub allowed: Vec<String>,   // glob patterns; empty = allow all
    pub denied: Vec<String>,    // glob patterns; empty = deny nothing
}

impl AccessFilter {
    /// Returns true if the input is permitted.
    /// Logic: if denied matches → false (always wins)
    ///        if allowed is empty → true (allow all)
    ///        if allowed matches → true
    ///        else → false
    pub fn permits(&self, input: &str) -> bool;
}
```

#### Items

1. **`AccessFilter` struct** in `ta-policy`: reusable allow/deny with glob matching and `permits()` method
2. **Daemon command config**: Replace `commands.allowed: Vec<String>` with `commands: AccessFilter` (add `denied` field). Default: `allowed: ["*"]`, `denied: []`
3. **Auto-approval paths**: Refactor `allowed_paths` / `blocked_paths` to use `AccessFilter` internally (keep YAML field names for backward compat)
4. **Channel access control**: Add `denied_roles` / `denied_users` alongside existing `allowed_*` fields
5. **Sandbox commands**: Add `denied` list to complement existing allowlist
6. **Agent tool access**: Add configurable tool allow/deny per agent config in `agents/*.yaml`
7. **Documentation**: Explain the unified pattern in USAGE.md — one mental model for all access control

#### Implementation scope
- `crates/ta-policy/src/access_filter.rs` — `AccessFilter` struct, glob matching, tests (~100 lines)
- `crates/ta-daemon/src/config.rs` — migrate `CommandConfig.allowed` to `AccessFilter`
- `crates/ta-policy/src/auto_approve.rs` — use `AccessFilter` for path matching
- `crates/ta-sandbox/src/lib.rs` — use `AccessFilter` for command lists
- Backward-compatible: existing configs with only `allowed` still work (empty `denied` = deny nothing)
- Tests: deny-wins-over-allow, empty-allow-means-all, glob matching, backward compat

#### Completed

- [x] `AccessFilter` struct in `ta-policy/src/access_filter.rs` with `permits()`, `tighten()`, `from_allowed()`, `allow_all()`, `is_unrestricted()`, `Display` impl, serde support, and 18 tests
- [x] Daemon `CommandConfig`: added `denied` field alongside `allowed`, `access_filter()` method returning `AccessFilter`, updated `cmd.rs` to use `filter.permits()` instead of `is_command_allowed()` (2 new tests)
- [x] Auto-approval paths: refactored `should_auto_approve_draft()` to use `AccessFilter` for path matching, `merge_conditions()` to use `AccessFilter::tighten()` (backward compatible — existing YAML field names preserved)
- [x] Sandbox: added `denied_commands` field to `SandboxConfig`, deny check in `execute()` and `is_allowed()` (2 new tests)
- [x] Documentation: unified access control pattern in USAGE.md

#### Deferred items resolved
- Channel access control → completed in v0.10.16
- Agent tool access → completed in v0.10.16

#### Version: `0.9.8-alpha.1.1`

---

### v0.9.8.2 — Pluggable Workflow Engine & Framework Integration
<!-- status: done -->
**Goal**: Add a `WorkflowEngine` trait to TA core so multi-stage, multi-role, multi-framework workflows can be orchestrated with pluggable engines — built-in YAML for simple cases, framework adapters (LangGraph, CrewAI) for power users, or custom implementations.

#### Design Principle: TA Mediates, Doesn't Mandate

TA defines *what* decisions need to be made (next stage? route back? what context?). The engine decides *how*. Users who already have LangGraph or CrewAI use TA for governance only. Users with simple agent setups (Claude Code, Codex) use TA's built-in YAML engine.

```
TA Core (always present):
  ┌───────────────────────────────────────────────┐
  │  WorkflowEngine trait                          │
  │    start(definition) → WorkflowId              │
  │    stage_completed(id, stage, verdicts)         │
  │      → StageAction (Proceed/RouteBack/Complete)│
  │    status(id) → WorkflowStatus                 │
  │    inject_feedback(id, stage, feedback)         │
  │                                                │
  │  GoalRun extensions:                           │
  │    workflow_id, stage, role, context_from       │
  │                                                │
  │  Verdict schema + Feedback scoring agent       │
  └──────────────────┬─────────────────────────────┘
                     │
        ┌────────────┼────────────┐
        │            │            │
  ┌──────────┐ ┌──────────┐ ┌──────────────┐
  │ Built-in │ │ Framework│ │ User-supplied│
  │ YAML     │ │ Adapters │ │ Custom impl  │
  │ Engine   │ │(LangGraph│ │              │
  │          │ │ CrewAI)  │ │ Implements   │
  │ Ships    │ │ Ship as  │ │ WorkflowEngine│
  │ with TA  │ │ templates│ │ trait or     │
  │ (default)│ │          │ │ process plugin│
  └──────────┘ └──────────┘ └──────────────┘
```

Configuration:
```yaml
# .ta/config.yaml
workflow:
  engine: yaml                    # built-in (default)
  # engine: langraph             # delegate to LangGraph adapter
  # engine: crewai               # delegate to CrewAI adapter
  # engine: process              # user-supplied binary (JSON-over-stdio)
  #   command: "./my-workflow-engine"
  # engine: none                 # no workflow — manage goals manually
```

#### Items

1. **`WorkflowEngine` trait** (`crates/ta-workflow/src/lib.rs`): Core abstraction that all engines implement.
   ```rust
   pub trait WorkflowEngine: Send + Sync {
       fn start(&self, def: &WorkflowDefinition) -> Result<WorkflowId>;
       fn stage_completed(&self, id: WorkflowId, stage: &str,
                          verdicts: &[Verdict]) -> Result<StageAction>;
       fn status(&self, id: WorkflowId) -> Result<WorkflowStatus>;
       fn inject_feedback(&self, id: WorkflowId, stage: &str,
                          feedback: FeedbackContext) -> Result<()>;
   }

   pub enum StageAction {
       Proceed { next_stage: String, context: GoalContext },
       RouteBack { target_stage: String, feedback: FeedbackContext,
                   severity: Severity },
       Complete,
       AwaitHuman { request: InteractionRequest },
   }
   ```

2. **`WorkflowDefinition` schema** (`crates/ta-workflow/src/definition.rs`): Declarative workflow structure used by all engines.
   ```rust
   pub struct WorkflowDefinition {
       pub name: String,
       pub stages: Vec<StageDefinition>,
       pub roles: HashMap<String, RoleDefinition>,
   }

   pub struct StageDefinition {
       pub name: String,
       pub depends_on: Vec<String>,
       pub roles: Vec<String>,           // parallel roles within stage
       pub then: Vec<String>,            // sequential roles after parallel
       pub review: Option<StageReview>,
       pub on_fail: Option<FailureRouting>,
   }

   pub struct RoleDefinition {
       pub agent: String,                // agent config name
       pub constitution: Option<String>, // constitution YAML path
       pub prompt: String,               // system prompt for this role
       pub framework: Option<String>,    // override framework for this role
   }
   ```

3. **`Verdict` schema and feedback scoring** (`crates/ta-workflow/src/verdict.rs`):
   - `Verdict { role, decision: Pass|Fail|Conditional, severity, findings: Vec<Finding> }`
   - `Finding { title, description, severity: Critical|Major|Minor, category }`
   - **Feedback scoring agent**: When verdicts arrive, optionally pass them to a scoring agent (metacritic pattern). The scoring agent's system prompt is a template — users customize the rubric. The scorer produces:
     - Aggregate score (0.0–1.0)
     - Severity classification (critical/major/minor)
     - Routing recommendation (which stage to route back to, if any)
     - Synthesized feedback for the next iteration
   - Scoring agent config in workflow YAML:
     ```yaml
     verdict:
       scorer:
         agent: claude-code
         prompt: |
           You are a metacritic reviewer. Given multiple review verdicts,
           synthesize them into an aggregate assessment. Weight security
           findings 2x. Classify overall severity and recommend routing.
       pass_threshold: 0.7
       required_pass: [security-reviewer]
     ```

4. **GoalRun extensions**: Add workflow context fields to `GoalRun`:
   - `workflow_id: Option<String>` — links goal to a workflow instance
   - `stage: Option<String>` — which stage this goal belongs to
   - `role: Option<String>` — which role this goal fulfills
   - `context_from: Vec<Uuid>` — goals whose output feeds into this one's context
   - These are metadata only — no behavioral change if unset. All existing goals continue to work as-is.

5. **Goal chaining** (context propagation): When a stage completes and the next stage starts, automatically inject the previous stage's output as context:
   - Previous stage's draft summary → next stage's system prompt
   - Previous stage's verdict findings → next stage's feedback section (on route-back)
   - Uses the existing CLAUDE.md injection mechanism (same as `ta run` context injection)
   - `context_from` field on GoalRun tracks the provenance chain

6. **Built-in YAML workflow engine** (`crates/ta-workflow/src/yaml_engine.rs`):
   - Parses `.ta/workflows/*.yaml` files
   - Evaluates stage dependencies (topological sort)
   - Starts goals for each role in a stage (parallel or sequential per config)
   - Collects verdicts, runs scorer, decides routing
   - Handles retry limits and loop detection (`max_retries` per routing rule)
   - ~400 lines — deliberately simple. Power users use LangGraph.

7. **Process-based workflow plugin** (`crates/ta-workflow/src/process_engine.rs`):
   - Same JSON-over-stdio pattern as channel plugins (v0.10.2)
   - TA spawns the engine process, sends `WorkflowDefinition` + events via stdin
   - Engine responds with `StageAction` decisions via stdout
   - This is how LangGraph/CrewAI adapters connect
   - ~150 lines in TA core

8. **`ta_workflow` MCP tool**: For orchestrator agents to interact with workflows:
   - `action: "start"` — start a workflow from a definition file
   - `action: "status"` — get workflow status (current stage, verdicts, retry count)
   - `action: "list"` — list active and completed workflows
   - No goal_run_id required (orchestrator-level tool, uses v0.9.6 optional ID pattern)

9. **`ta workflow` CLI commands**:
   - `ta workflow start <definition.yaml>` — start a workflow
   - `ta workflow status [workflow_id]` — show status
   - `ta workflow list` — list workflows
   - `ta workflow cancel <workflow_id>` — cancel an active workflow
   - `ta workflow history <workflow_id>` — show stage transitions, verdicts, routing decisions

10. **Framework integration templates** (shipped with TA):
    - `templates/workflows/milestone-review.yaml` — the full plan/build/review workflow using built-in YAML engine
    - `templates/workflows/roles/` — role definition library (planner, designer, PM, engineer, security-reviewer, customer personas)
    - `templates/workflows/adapters/langraph_adapter.py` — Python bridge: LangGraph ↔ TA's WorkflowEngine protocol
    - `templates/workflows/adapters/crewai_adapter.py` — Python bridge: CrewAI ↔ TA's protocol
    - `templates/workflows/simple-review.yaml` — minimal 2-stage workflow (build → review) for getting started
    - `templates/workflows/security-audit.yaml` — security-focused workflow with OWASP reviewer + dependency scanner

#### Workflow Events
```rust
// New TaEvent variants
WorkflowStarted { workflow_id, name, stage_count, timestamp }
StageStarted { workflow_id, stage, roles: Vec<String>, timestamp }
StageCompleted { workflow_id, stage, verdicts: Vec<Verdict>, timestamp }
WorkflowRouted { workflow_id, from_stage, to_stage, severity, reason, timestamp }
VerdictScored { workflow_id, stage, aggregate_score, routing_recommendation, timestamp }
WorkflowCompleted { workflow_id, name, total_duration_secs, stages_executed, timestamp }
WorkflowFailed { workflow_id, name, reason, timestamp }
```

11. **Interactive workflow interaction from `ta shell`**: When a workflow reaches an `AwaitHuman` stage action, the shell renders it as an interactive prompt the human can respond to in real time.
    - **`await_human` per-stage config** in workflow YAML:
      ```yaml
      stages:
        - name: planning
          await_human: always     # always pause for human input before proceeding
        - name: build
          await_human: never      # fully automated
        - name: review
          await_human: on_fail    # pause only if verdicts fail the pass_threshold
      ```
      Values: `always` (pause after every stage completion), `never` (proceed automatically), `on_fail` (pause only when verdicts route back or score below threshold). Default: `never`.
    - **`InteractionRequest` struct** (part of `AwaitHuman` action):
      ```rust
      pub struct InteractionRequest {
          pub prompt: String,           // what the workflow is asking
          pub context: serde_json::Value, // stage verdicts, scores, findings
          pub options: Vec<String>,     // suggested choices (proceed, revise, cancel)
          pub timeout_secs: Option<u64>, // auto-proceed after timeout (None = wait forever)
      }
      ```
    - **Workflow interaction endpoint**: `POST /api/workflow/:id/input` — accepts `{ "decision": "proceed" | "revise" | "cancel", "feedback": "optional text" }`. The daemon routes the decision to the workflow engine's `inject_feedback()` method.
    - **Workflow event for shell rendering**: `WorkflowAwaitingHuman { workflow_id, stage, prompt, options, timestamp }` — SSE event that the shell listens for and renders as an interactive prompt with numbered options. The human types their choice, shell POSTs to the interaction endpoint.
    - **Shell-side UX**: When the shell receives a `workflow.awaiting_human` event, it renders:
      ```
      [workflow] Review stage paused — 2 findings need attention:
        1. Security: SQL injection risk in user input handler (critical)
        2. Style: Inconsistent error message format (minor)

      Options: [1] proceed  [2] revise planning  [3] cancel workflow
      workflow> _
      ```
      The `workflow>` prompt replaces the normal `ta>` prompt until the human responds. Normal shell commands still work (e.g., `ta draft view` to inspect the draft before deciding).

#### Implementation scope
- `crates/ta-workflow/` — new crate:
  - `src/lib.rs` — `WorkflowEngine` trait, `StageAction`, re-exports (~100 lines)
  - `src/definition.rs` — `WorkflowDefinition`, `StageDefinition`, `RoleDefinition` (~150 lines)
  - `src/verdict.rs` — `Verdict`, `Finding`, `Severity`, `FeedbackContext` (~100 lines)
  - `src/yaml_engine.rs` — built-in YAML engine with DAG execution (~400 lines)
  - `src/process_engine.rs` — JSON-over-stdio plugin bridge (~150 lines)
  - `src/scorer.rs` — feedback scoring agent integration (~100 lines)
  - `src/interaction.rs` — `InteractionRequest`, `InteractionResponse`, `AwaitHumanConfig` (~80 lines)
- `crates/ta-goal/src/goal_run.rs` — add workflow_id, stage, role, context_from fields
- `crates/ta-goal/src/events.rs` — workflow event variants including `WorkflowAwaitingHuman`
- `crates/ta-mcp-gateway/src/tools/workflow.rs` — `ta_workflow` MCP tool
- `crates/ta-daemon/src/routes/` — `POST /api/workflow/:id/input` endpoint
- `apps/ta-cli/src/commands/workflow.rs` — `ta workflow` CLI commands
- `apps/ta-cli/src/commands/shell.rs` — workflow prompt rendering and interaction input handling
- `templates/workflows/` — workflow definitions, role library, framework adapters
- `docs/USAGE.md` — workflow engine docs, framework integration guide, interactive workflow section
- Tests: YAML engine stage execution, verdict scoring, routing decisions, goal chaining context propagation, process plugin protocol, loop detection, await_human interaction round-trip

#### Completed
- ✅ `WorkflowEngine` trait with start/stage_completed/status/inject_feedback/cancel/list methods
- ✅ `WorkflowDefinition` schema with stages, roles, verdict config, topological sort
- ✅ `Verdict` schema with Finding, Severity, VerdictDecision, aggregate scoring
- ✅ GoalRun extensions: workflow_id, stage, role, context_from fields (backward compatible)
- ✅ Built-in YAML workflow engine (~400 lines) with retry routing and loop detection
- ✅ Process-based workflow plugin bridge (JSON-over-stdio protocol types + stub)
- ✅ Feedback scoring module (ScoringResult, score_verdicts with required role checks)
- ✅ Interactive human-in-the-loop (AwaitHumanConfig: always/never/on_fail, InteractionRequest/Response)
- ✅ 7 workflow TaEvent variants: WorkflowStarted, StageStarted, StageCompleted, WorkflowRouted, WorkflowCompleted, WorkflowFailed, WorkflowAwaitingHuman
- ✅ `ta_workflow` MCP tool (start, status, list, cancel, history actions)
- ✅ `ta workflow` CLI commands (start, status, list, cancel, history)
- ✅ Daemon API endpoints: GET /api/workflows, POST /api/workflow/:id/input
- ✅ Shell SSE rendering for all 7 workflow event types including awaiting_human prompts
- ✅ Framework integration templates: 3 workflow definitions, 5 role definitions, 2 adapter scripts (LangGraph, CrewAI)
- ✅ ~44 new tests across ta-workflow (31), ta-goal (3), ta-mcp-gateway (1), ta-cli (2), ta-daemon (1)

#### Deferred items moved
- Goal chaining context propagation → v0.10.18
- Full async process engine I/O → v0.10.18
- Live scoring agent integration → v0.10.18

#### Version: `0.9.8-alpha.2`

---

### v0.9.8.3 — Full TUI Shell (`ratatui`)
<!-- status: done -->
**Goal**: Replace the line-mode rustyline shell with a full terminal UI modeled on Claude Code / claude-flow — persistent status bar, scrolling output, and input area, all in one screen.

#### Layout
```
┌─────────────────────────────────────────────────────────┐
│  [scrolling output]                                     │
│  goal started: "Implement v0.9.8.1" (claude-code)       │
│  draft built: 15 files (abc123)                         │
│  $ ta goal list                                         │
│  ID       Title                    State    Agent       │
│  ca306e4d Implement v0.9.8.1       running  claude-code │
│                                                         │
│                                                         │
├─────────────────────────────────────────────────────────┤
│ ta> ta draft list                                       │
├─────────────────────────────────────────────────────────┤
│ TrustedAutonomy v0.9.8 │ 1 agent │ 0 drafts │ ◉ daemon│
└─────────────────────────────────────────────────────────┘
```

#### Items

1. **`ratatui` + `crossterm` terminal backend**: Full-screen TUI with three zones — output scroll area, input line, status bar. ~1500 lines replacing the current ~500-line rustyline shell.

2. **Status bar** (bottom): Project name, version, active agent count, pending draft count, daemon connection indicator (green dot = connected, red = disconnected), current workflow stage (if any). Updates live via SSE events.

3. **Input area** (above status bar): Text input with history (up/down arrows), tab-completion from `/api/routes`, multi-line support for longer commands. Uses `tui-textarea` or custom widget.

4. **Scrolling output pane** (main area): Command responses, SSE event notifications, workflow prompts. Auto-scrolls but allows scroll-back with PgUp/PgDn. Events are rendered inline with dimmed styling to distinguish from command output.

5. **Workflow interaction mode**: When a `workflow.awaiting_human` event arrives, the output pane shows the prompt/options and the input area switches to `workflow>` mode (from v0.9.8.2 item 11). Normal commands still work during workflow prompts.

6. **Split pane support** (stretch): Optional vertical split showing agent session output on one side, shell commands on the other. Toggle with `Ctrl-W`. Useful when monitoring an agent in real time while reviewing drafts.

7. **Notification badges**: Unread event count shown in status bar. Cleared when user scrolls to bottom. Draft-ready events flash briefly.

#### Completed
- ✅ `ratatui` + `crossterm` terminal backend — full-screen TUI with three zones (output scroll, input line, status bar)
- ✅ Status bar — project name, version, agent count, draft count, daemon connection indicator, workflow stage, unread badge
- ✅ Input area — text input with cursor movement, history (up/down), tab-completion, Ctrl-A/E/U/K editing shortcuts
- ✅ Scrolling output pane — command responses and SSE events with styled lines, PgUp/PgDn scroll, auto-scroll with unread counter
- ✅ Workflow interaction mode — `workflow>` prompt when `workflow_awaiting_human` events arrive
- ✅ Notification badges — unread event count in status bar, cleared on scroll-to-bottom
- ✅ `--classic` flag preserves rustyline shell as fallback
- ✅ 13 unit tests — input handling, cursor movement, history navigation, tab completion, scroll, daemon state, workflow mode

#### Deferred items resolved
- Split pane support → completed in v0.10.14

#### Implementation scope
- `apps/ta-cli/src/commands/shell_tui.rs` — new TUI module with ratatui (~500 lines + tests)
- `apps/ta-cli/src/commands/shell.rs` — updated to dispatch TUI vs classic, shared functions made pub(crate)
- `apps/ta-cli/Cargo.toml` — added `ratatui`, `crossterm` dependencies
- Daemon API layer unchanged — same HTTP/SSE endpoints

#### Version: `0.9.8-alpha.3`

---

### v0.9.8.4 — VCS Adapter Abstraction & Plugin Architecture
<!-- status: done -->
**Goal**: Move all version control operations behind the `SubmitAdapter` trait so TA is fully VCS-agnostic. Add adapter-contributed exclude patterns for staging, implement stub adapters for SVN and Perforce, and design the external plugin loading mechanism.

#### Problem
Today, raw `git` commands leak outside the `SubmitAdapter` trait boundary — branch save/restore in `draft.rs`, VCS auto-detection, `.git/` exclusions hardcoded in `overlay.rs`, and git hash embedding in `build.rs`. This means adding Perforce or SVN support requires modifying core TA code in multiple places rather than simply providing a new adapter.

Additionally, shipping adapters for every VCS/email/database system inside the core `ta` binary doesn't scale. External teams (e.g., a Perforce shop or a custom VCS vendor) should be able to publish a TA adapter as an independent installable package.

#### Design

##### 1. Adapter-contributed exclude patterns
Each `SubmitAdapter` provides a list of directory/file patterns that should be excluded when copying source to staging. This replaces the hardcoded `.git/` exclusion in `overlay.rs`.

```rust
pub trait SubmitAdapter: Send + Sync {
    // ... existing methods ...

    /// Patterns to exclude from staging copy (VCS metadata dirs, etc.)
    /// Returns patterns in .taignore format: "dirname/", "*.ext", "name"
    fn exclude_patterns(&self) -> Vec<String> {
        vec![]
    }

    /// Save/restore working state around apply operations.
    /// Git: save current branch, restore after commit.
    /// Perforce: save current changelist context.
    /// Default: no-op.
    fn save_state(&self) -> Result<Option<Box<dyn std::any::Any + Send>>> { Ok(None) }
    fn restore_state(&self, state: Option<Box<dyn std::any::Any + Send>>) -> Result<()> { Ok(()) }

    /// Auto-detect whether this adapter applies to the given project root.
    /// Git: checks for .git/ directory
    /// Perforce: checks for P4CONFIG or .p4config
    fn detect(project_root: &Path) -> bool where Self: Sized { false }
}
```

- `GitAdapter::exclude_patterns()` → `[".git/"]`
- `SvnAdapter::exclude_patterns()` → `[".svn/"]`
- `PerforceAdapter::exclude_patterns()` → `[".p4config"]` (P4 doesn't have a metadata dir per se)
- `overlay.rs` merges adapter excludes with `.taignore` user patterns and built-in defaults (`target/`, `node_modules/`, etc.)

##### 2. Move git-specific code behind the adapter

| Current location | What it does | Where it moves |
|---|---|---|
| `draft.rs:1946-2048` | Branch save/restore around apply | `SubmitAdapter::save_state()` / `restore_state()` |
| `draft.rs:1932` | `.git/` existence check for auto-detect | `SubmitAdapter::detect()` + adapter registry |
| `overlay.rs:24` | Hardcoded `"target/"` + `.git/` exclusion | Adapter `exclude_patterns()` + `ExcludePatterns::merge()` |
| `build.rs` | `git rev-parse HEAD` for version hash | `SubmitAdapter::revision_id()` or build-time env var |
| `shell.rs` | `git status` as shell route | Adapter-provided shell routes (optional) |

##### 3. Stub adapters (untested)

**SVN adapter** (`crates/ta-submit/src/svn.rs`):
- `prepare()` → no-op (SVN doesn't use branches the same way)
- `commit()` → `svn add` + `svn commit`
- `push()` → no-op (SVN commit is already remote)
- `open_review()` → no-op (SVN doesn't have built-in review)
- `exclude_patterns()` → `[".svn/"]`
- `detect()` → check for `.svn/` directory
- **Note: untested — contributed by AI, needs validation by an SVN user**

**Perforce adapter** (`crates/ta-submit/src/perforce.rs`):
- `prepare()` → `p4 change -o | p4 change -i` (create pending changelist)
- `commit()` → `p4 reconcile` + `p4 shelve`
- `push()` → `p4 submit`
- `open_review()` → `p4 shelve` + Swarm API (if configured)
- `exclude_patterns()` → `[".p4config", ".p4ignore"]`
- `detect()` → check for `P4CONFIG` env var or `.p4config`
- `save_state()` → record current client/changelist
- `restore_state()` → revert to saved client state
- **Note: untested — contributed by AI, needs validation by a Perforce user**

##### 4. Adapter auto-detection registry

```rust
/// Registry of available adapters with auto-detection.
pub fn detect_adapter(project_root: &Path) -> Box<dyn SubmitAdapter> {
    // Check configured adapter first (workflow.toml)
    // Then auto-detect: try each registered adapter's detect()
    // Fallback: NoneAdapter
}
```

Order: Git → SVN → Perforce → None. First match wins. User can override with `workflow.toml` setting `submit.adapter = "perforce"`.

##### 5. External plugin architecture (design only — implementation deferred)

External adapters loaded as separate executables that communicate via a simple JSON-over-stdio protocol, similar to how `ta run` launches agents:

```
~/.ta/plugins/
  ta-submit-perforce    # executable
  ta-submit-jira        # executable
  ta-submit-plastic     # executable (Plastic SCM)
```

**Protocol**: TA spawns the plugin binary and sends JSON commands on stdin, reads JSON responses from stdout:
```json
// → plugin
{"method": "exclude_patterns", "params": {}}
// ← plugin
{"result": [".plastic/", ".plastic4.selector"]}

// → plugin
{"method": "commit", "params": {"goal_id": "abc", "message": "Fix bug", "files": ["src/main.rs"]}}
// ← plugin
{"result": {"commit_id": "cs:1234", "message": "Changeset 1234 created"}}
```

**Discovery**: `ta plugin install <name>` downloads from a registry (crates.io, npm, or TA's own) and places the binary in `~/.ta/plugins/`. Or manual: just drop an executable named `ta-submit-<name>` in the plugins dir.

**Config**: `submit.adapter = "perforce"` → TA first checks built-in adapters, then looks for `~/.ta/plugins/ta-submit-perforce`.

This pattern extends beyond VCS to any adapter type:
- `ta-channel-slack` — Slack notification channel
- `ta-channel-discord` — Discord notification channel
- `ta-channel-email` — Email notification channel
- `ta-output-jira` — Jira ticket creation from drafts
- `ta-store-postgres` — PostgreSQL-backed goal/draft store

#### Completed
1. [x] Add `exclude_patterns()`, `save_state()`/`restore_state()`, `detect()`, `revision_id()` to `SubmitAdapter` trait
2. [x] Implement `exclude_patterns()` for `GitAdapter` (returns `[".git/"]`)
3. [x] Move branch save/restore from `draft.rs` into `GitAdapter::save_state()`/`restore_state()`
4. [x] Remove hardcoded `.git/` exclusion from `overlay.rs`, add `ExcludePatterns::merge()` for adapter patterns
5. [x] Add adapter auto-detection registry in `ta-submit` (`registry.rs`)
6. [x] Move `draft.rs` git auto-detection to use `select_adapter()` from registry
7. [x] Add `SvnAdapter` stub (`crates/ta-submit/src/svn.rs`) — **untested**
8. [x] Add `PerforceAdapter` stub (`crates/ta-submit/src/perforce.rs`) — **untested**
9. [x] Add `revision_id()` method to adapter, update `build.rs` with `TA_REVISION` env var fallback
10. [x] Update `docs/USAGE.md` with adapter configuration documentation
11. [x] Tests: 39 tests — adapter detection (5), exclude patterns (3), state save/restore lifecycle (1), registry selection (6), known adapters, stub adapter basics (8), git operations (4)

#### Implementation scope
- `crates/ta-submit/src/adapter.rs` — extended `SubmitAdapter` trait with new methods
- `crates/ta-submit/src/git.rs` — implement new trait methods, absorb branch logic from `draft.rs`
- `crates/ta-submit/src/svn.rs` — NEW: SVN adapter stub (untested)
- `crates/ta-submit/src/perforce.rs` — NEW: Perforce adapter stub (untested)
- `crates/ta-submit/src/registry.rs` — NEW: adapter auto-detection and selection
- `crates/ta-submit/src/lib.rs` — export new adapters and registry
- `crates/ta-workspace/src/overlay.rs` — accept adapter exclude patterns, remove hardcoded `.git/`
- `apps/ta-cli/src/commands/draft.rs` — remove raw git calls, use adapter state save/restore
- `apps/ta-cli/build.rs` — use adapter-provided revision ID or env var fallback
- `docs/USAGE.md` — adapter configuration, available adapters, stub adapter warnings

#### Version: `0.9.8-alpha.4`

---

### v0.9.9 — Conversational Project Bootstrapping (`ta new`) *(design only)*
<!-- status: deferred -->
**Status note**: Sub-phases v0.9.9.1–v0.9.9.5 are all **done** — they built the infrastructure (interactive mode, plan-from-doc, channel delivery, authoring tooling). However, the **parent `ta new` CLI command itself was never implemented**. The end-to-end bootstrapping flow described below doesn't exist yet. Implementation moved to **v0.10.17**.

**Goal**: Start a new project from any interface by describing what you want in natural language. A planner agent generates the project structure and PLAN.md through conversation, then initializes the TA workspace.

#### User Flow (from any interface)

```
You:  I want to build a Rust CLI that manages Cloudflare DNS
      records with policy controls, audit logging, and a web
      dashboard for reviewing changes.

Agent: I'll help you plan this. A few questions:

      1. Multi-account support? (manage DNS for several
         Cloudflare accounts)
      2. Should it also support Route53 or other providers,
         or Cloudflare-only?
      3. How strict should policy be — allowlist of editable
         records, or more granular (per-type, per-zone)?

You:  Multi-account yes. Cloudflare-only for now but design
      for multiple providers later. Policy should be per-zone
      and per-record-type.

Agent: Here's a proposed plan:

      **dns-guardian** — Cloudflare DNS governance CLI

      Phase 1: Project Scaffold
        Cargo workspace, CI, README, license, .ta/ init

      Phase 2: Provider Abstraction & Cloudflare Client
        DnsProvider trait, Cloudflare implementation,
        account selection, auth (API token per account)

      Phase 3: DNS Record Operations
        List/create/update/delete, import existing records,
        dry-run mode, diff preview before apply

      Phase 4: Policy Engine
        Per-zone record-type allowlist, approval required
        for wildcard/MX changes, policy config in YAML

      Phase 5: Audit Logging
        Change log (who/what/when/why), export JSON/CSV,
        tamper-evident chain (reuse ta-audit)

      Phase 6: Web Dashboard
        Record browser, change history, pending approvals,
        served by embedded HTTP server

      Phase 7: Tests & Documentation
        Integration tests with API mock, user guide

      Want to adjust anything before I generate the project?

You:  Add a health check phase — periodic DNS resolution
      validation with alerting.

Agent: Added Phase 6.5: DNS Health Monitor (periodic
      resolution checks, configurable alerts, health
      dashboard widget). Dashboard moved to Phase 7.

      Generating project now...

      ✅ Created /projects/dns-guardian
      ✅ Initialized .ta/ workspace
      ✅ Generated PLAN.md (8 phases, 34 items)
      ✅ Created Cargo.toml workspace scaffold
      ✅ Created .ta/policy.yaml (default: checkpoint)

      Ready to start Phase 1?

You:  Yes

Agent: [starts goal for Phase 1]
      🚀 Goal started: "Phase 1: Project Scaffold"
```

#### Architecture

```
  "ta new" or daemon API
          │
          ▼
   Planner Agent Session
   (no project yet — runs in temp dir)
          │
          ├── Conversation to refine requirements
          │   (iterative Q&A, user describes what they want)
          │
          ├── Plan generation
          │   (agent produces PLAN.md from conversation)
          │
          ├── Project initialization
          │   ├── mkdir + cargo init / npm init / etc.
          │   ├── ta init (creates .ta/ structure)
          │   ├── Write PLAN.md
          │   ├── Write initial config (.ta/policy.yaml, agents/*.yaml)
          │   └── git init + initial commit
          │
          └── Hand off to normal TA workflow
              (project exists, can run goals)
```

#### Items

1. **`ta new` CLI command**: Starts a conversational project bootstrapping session.
   - `ta new` — interactive mode, asks questions
   - `ta new --from <brief.md>` — seed from a written description file
   - `ta new --template <name>` — start from a project template (v0.7.3 templates)
   - Creates a temporary working directory for the planner agent
   - On completion, moves the generated project to the target directory

2. **Planner agent mode**: A specialized agent configuration (`agents/planner.yaml`) that:
   - Has access to `ta init`, filesystem write, and plan generation tools
   - Does NOT have access to `ta goal start`, `ta draft build`, or other runtime tools (it's creating the project, not executing goals)
   - System prompt includes: plan format specification (PLAN.md with `<!-- status: pending -->` markers), versioning policy, phase sizing guidelines
   - Conversation is multi-turn: agent asks clarifying questions, proposes a plan, user refines, agent generates
   - Agent tools available:
     - `ta_scaffold` — create directory structure, Cargo.toml/package.json/etc.
     - `ta_plan_generate` — write PLAN.md from structured plan data
     - `ta_init` — initialize .ta/ workspace in the new project
     - `ta_config_write` — write initial .ta/policy.yaml, .ta/config.yaml, agents/*.yaml

3. **Plan generation from conversation**: The planner agent converts the conversation into a structured PLAN.md:
   - Each phase has: title, goal description, numbered items, implementation scope, version
   - Phase sizing: guide the agent to create phases that are 1-4 hours of work each
   - Dependencies: note which phases depend on others
   - Phase markers: all start as `<!-- status: pending -->`
   - Versioning: auto-assign version numbers (v0.1.0 for phase 1, v0.2.0 for phase 2, etc.)

4. **Project template integration**: Leverage v0.7.3 templates as starting points:
   - `ta new --template rust-cli` → Cargo workspace, clap, CI, README
   - `ta new --template rust-lib` → Library crate, docs, benchmarks
   - `ta new --template ts-api` → Node.js, Express/Fastify, TypeScript
   - Templates provide the scaffold; the planner agent customizes and adds the PLAN.md
   - Custom templates: `ta new --template ./my-template` or `ta new --template gh:org/repo`

5. **Daemon API endpoint** (`POST /api/project/new`): Start a bootstrapping session via the daemon API, so Discord/Slack/email interfaces can create projects too.
   - First request starts the planner agent session
   - Subsequent requests in the same session continue the conversation
   - Final response includes the project path and PLAN.md summary
   ```json
   // Start
   { "description": "Rust CLI for Cloudflare DNS management with policy controls" }
   → { "session_id": "plan-abc", "response": "I'll help you plan this. A few questions..." }

   // Continue
   { "session_id": "plan-abc", "prompt": "Multi-account, Cloudflare only for now" }
   → { "session_id": "plan-abc", "response": "Here's a proposed plan..." }

   // Generate
   { "session_id": "plan-abc", "prompt": "Looks good, generate it" }
   → { "session_id": "plan-abc", "project_path": "/projects/dns-guardian", "phases": 8 }
   ```

6. **Post-creation handoff**: After the project is generated:
   - Print summary: phase count, item count, estimated version range
   - Offer to start the first goal: "Ready to start Phase 1? (y/n)"
   - If using `ta shell`, switch the shell's working directory to the new project
   - If using a remote interface, return the project path and next steps

#### Implementation scope
- `apps/ta-cli/src/commands/new.rs` — `ta new` command, planner agent session, template integration
- `apps/ta-cli/src/commands/new/planner.rs` — planner agent system prompt, plan generation tools
- `apps/ta-cli/src/commands/new/scaffold.rs` — project directory creation, language-specific scaffolding
- `agents/planner.yaml` — planner agent configuration (restricted tool set)
- `crates/ta-daemon/src/api/project.rs` — `/api/project/new` endpoint for remote bootstrapping
- `crates/ta-mcp-gateway/src/tools/scaffold.rs` — `ta_scaffold`, `ta_plan_generate`, `ta_config_write` MCP tools
- `templates/projects/rust-cli/` — Rust CLI project template
- `templates/projects/rust-lib/` — Rust library template
- `templates/projects/ts-api/` — TypeScript API template
- `docs/USAGE.md` — `ta new` documentation, template authoring guide
- Tests: plan generation from description, template application, scaffold creation, daemon API session lifecycle

#### Version: `0.9.9-alpha`

---

### v0.9.9.1 — Interactive Mode Core Plumbing
<!-- status: done -->
**Goal**: Add the foundational infrastructure for agent-initiated mid-goal conversations with humans. Interactive mode is the general primitive — micro-iteration within the macro-iteration TA governs. The agent calls `ta_ask_human` (MCP tool), TA delivers the question through whatever channel the human is on, and routes the response back. The agent continues.

#### Architecture

```
Agent calls ta_ask_human("What database?")
  → MCP tool writes question to .ta/interactions/pending/<id>.json
  → Emits SessionEvent::AgentNeedsInput
  → GoalRunState transitions Running → AwaitingInput
  → Tool polls for .ta/interactions/answers/<id>.json

Human sees question in ta shell / Slack / web UI
  → Responds via POST /api/interactions/:id/respond
  → HTTP handler writes answer file
  → MCP tool poll finds it, returns answer to agent
  → GoalRunState transitions AwaitingInput → Running
```

#### Items

1. ~~**`ta_ask_human` MCP tool** (`crates/ta-mcp-gateway/src/tools/human.rs`)~~ ✅
   - Parameters: `question`, `context`, `response_hint` (freeform/yes_no/choice), `choices`, `timeout_secs`
   - File-based signaling: writes question file, polls for answer file (1s interval)
   - Emits `AgentNeedsInput` and `AgentQuestionAnswered` events
   - Timeout returns actionable message (not error) so agent can continue

2. ~~**`QuestionRegistry`** (`crates/ta-daemon/src/question_registry.rs`)~~ ✅
   - In-memory coordination for future in-process use (oneshot channels)
   - `PendingQuestion`, `HumanAnswer` types
   - `register()`, `answer()`, `list_pending()`, `cancel()`

3. ~~**HTTP response endpoints** (`crates/ta-daemon/src/api/interactions.rs`)~~ ✅
   - `POST /api/interactions/:id/respond` — writes answer file + fires registry
   - `GET /api/interactions/pending` — lists pending questions

4. ~~**`GoalRunState::AwaitingInput`** (`crates/ta-goal/src/goal_run.rs`)~~ ✅
   - New state with `interaction_id` and `question_preview`
   - Valid transitions: `Running → AwaitingInput → Running`, `AwaitingInput → PrReady`
   - Visible in `ta goal list` and external UIs

5. ~~**New `SessionEvent` variants** (`crates/ta-events/src/schema.rs`)~~ ✅
   - `AgentNeedsInput` — with `suggested_actions()` returning a "respond" action
   - `AgentQuestionAnswered`, `InteractiveSessionStarted`, `InteractiveSessionCompleted`

6. ~~**`InteractionKind::AgentQuestion`** (`crates/ta-changeset/src/interaction.rs`)~~ ✅
   - New variant for channel rendering dispatch

7. ~~**`ConversationStore`** (`crates/ta-goal/src/conversation.rs`)~~ ✅
   - JSONL log at `.ta/conversations/<goal_id>.jsonl`
   - `append_question()`, `append_answer()`, `load()`, `next_turn()`, `conversation_so_far()`

#### Version: `0.9.9-alpha.1`

---

### v0.9.9.2 — Shell TUI Interactive Mode
<!-- status: done -->
**Goal**: Wire interactive mode into `ta shell` so humans can see agent questions and respond inline. This is the first user-facing surface for interactive mode.

#### Items

1. **SSE listener for `agent_needs_input`** (`apps/ta-cli/src/commands/shell_tui.rs`):
   - SSE event handler recognizes `agent_needs_input` event → sends `TuiMessage::AgentQuestion`
   - Question text displayed prominently in the output pane

2. **Input routing switch** (`apps/ta-cli/src/commands/shell_tui.rs`):
   - `App` gets `pending_question: Option<PendingQuestion>` field
   - When `pending_question` is `Some`, prompt changes to `[agent Q1] >`
   - Enter sends text to `POST /api/interactions/:id/respond` instead of `/api/input`
   - On success, clears `pending_question`, restores normal prompt

3. **`ta run --interactive` flag** (`apps/ta-cli/src/commands/run.rs`):
   - Wire `--interactive` flag through to enable `ta_ask_human` in the MCP tool set
   - When set, agent system prompt includes instructions about `ta_ask_human` availability

4. **`ta conversation <goal_id>` CLI command** (`apps/ta-cli/src/commands/conversation.rs`):
   - Print conversation history from JSONL log
   - Show turn numbers, roles, timestamps

#### Completed

- ✅ SSE listener for `agent_needs_input` — `parse_agent_question()`, `TuiMessage::AgentQuestion` variant (5 tests)
- ✅ Input routing switch — `pending_question` field, prompt changes to `[agent Q1] >`, routes Enter to `/api/interactions/:id/respond` (3 tests)
- ✅ `ta run --interactive` flag — `build_interactive_section()` injects `ta_ask_human` documentation into CLAUDE.md (2 tests)
- ✅ `ta conversation <goal_id>` CLI command — reads JSONL log, formatted + JSON output modes (4 tests)
- ✅ Classic shell SSE rendering for `agent_needs_input` and `agent_question_answered` events
- ✅ Status bar indicator for pending agent questions
- ✅ Version bump to `0.9.9-alpha.2`

#### Version: `0.9.9-alpha.2`

---

### v0.9.9.3 — `ta plan from <doc>` Wrapper
<!-- status: done -->
**Goal**: Build a convenience wrapper that uses interactive mode to generate a PLAN.md from a product document. The agent reads the document, asks clarifying questions via `ta_ask_human`, proposes phases, and outputs a plan draft.

#### Completed

- ✅ `PlanCommands::From` variant — `ta plan from <path>` reads document, builds planning prompt, delegates to `ta run --interactive` (4 tests)
- ✅ `build_planning_prompt()` — constructs agent prompt with document content, PLAN.md format guide, and `ta_ask_human` usage instructions; truncates docs >100K chars
- ✅ `agents/planner.yaml` — planner agent configuration with fs read/write access, no shell/network, planning-oriented alignment
- ✅ `docs/USAGE.md` updates — `ta plan from` documentation with examples, comparison table for `--detect` vs `plan from` vs `plan create`
- ✅ Fuzzy document search — `find_document()` searches workspace root, `docs/`, `spec/`, `design/`, `rfcs/`, and subdirs so bare filenames resolve automatically (4 tests)
- ✅ Shell/daemon integration — `ta plan from *` added to default `long_running` patterns in daemon config for background execution
- ✅ Validation — rejects missing files, empty documents, directories; observability-compliant error messages with search location details
- ✅ Version bump to `0.9.9-alpha.3`

#### When to use `--detect` vs `plan from`
- **`ta init --detect`** — detects project *type* for config scaffolding. Fast, deterministic, no AI.
- **`ta plan from <doc>`** — reads a product document and generates a phased *development plan* via interactive agent session. Use after `ta init`.
- **`ta plan create`** — generates a generic plan from a hardcoded template. Use when you don't have a product doc.

#### Version: `0.9.9-alpha.3`

---

### v0.9.9.4 — External Channel Delivery
<!-- status: done -->
**Goal**: Enable interactive mode questions to flow through external channels (Slack, Discord, email) — not just `ta shell`. The `QuestionRegistry` + HTTP endpoint design is already channel-agnostic; this phase adds the delivery adapters.

#### Completed

- ✅ `ChannelDelivery` trait in `ta-events::channel` — async trait with `deliver_question()`, `name()`, `validate()` methods; `ChannelQuestion`, `DeliveryResult`, `ChannelRouting` types (5 tests)
- ✅ `channels` routing field on `AgentNeedsInput` event — backward-compatible `#[serde(default)]` Vec<String> for channel routing hints
- ✅ `ta-connector-slack` crate — `SlackAdapter` implementing `ChannelDelivery`, posts Block Kit messages with action buttons for yes/no and choice responses, thread-reply prompts for freeform (7 tests)
- ✅ `ta-connector-discord` crate — `DiscordAdapter` implementing `ChannelDelivery`, posts embeds with button components (up to 5 per row), footer prompts for freeform (6 tests)
- ✅ `ta-connector-email` crate — `EmailAdapter` implementing `ChannelDelivery`, sends HTML+text emails via configurable HTTP endpoint, includes interaction metadata headers (7 tests)
- ✅ `ChannelDispatcher` in `ta-daemon` — routes questions to registered adapters based on channel hints or daemon defaults; `from_config()` factory for building from `daemon.toml` (9 tests)
- ✅ `ChannelsConfig` in daemon config — `[channels]` section in `daemon.toml` with `default_channels`, `[channels.slack]`, `[channels.discord]`, `[channels.email]` sub-tables
- ✅ Version bump to `0.9.9-alpha.4`

#### Deferred items moved
- Slack/Discord/Email interaction handler webhooks → v0.11.0 (Event-Driven Agent Routing)

#### Version: `0.9.9-alpha.4`

---

### v0.9.9.5 — Workflow & Agent Authoring Tooling
<!-- status: done -->
**Goal**: Make it easy for users to create, validate, and iterate on custom workflow definitions and agent profiles without reading Rust source code or guessing YAML schema.

#### Problem
Today, creating a custom workflow or agent config requires copying an existing file and modifying it by trial and error. There's no scaffolding command, no schema validation beyond serde parse errors, and no way to check for common mistakes (undefined role references, unreachable stages, missing agent configs). USAGE.md now has authoring guides (added in v0.9.9.1), but tooling support is missing.

#### Items

1. **`ta workflow new <name>`** (`apps/ta-cli/src/commands/workflow.rs`):
   - Generates `.ta/workflows/<name>.yaml` with annotated comments explaining every field
   - Includes a 2-stage build→review template as a starting point
   - Prints the file path and suggests next steps

2. **`ta workflow validate <path>`** (`apps/ta-cli/src/commands/workflow.rs`):
   - Schema validation: all required fields present, correct types
   - Reference validation: every role referenced in a stage exists in `roles:`
   - Dependency validation: no cycles, no references to undefined stages
   - Agent validation: every `roles.*.agent` has a matching agent config file
   - Prints actionable errors with line numbers and suggestions

3. **`ta agent new <name>`** (`apps/ta-cli/src/commands/agent.rs` or `setup.rs`):
   - Generates `.ta/agents/<name>.yaml` with annotated comments
   - Prompts for agent type (full developer, read-only auditor, orchestrator)
   - Fills in appropriate `alignment` defaults based on type

4. **`ta agent validate <path>`** (`apps/ta-cli/src/commands/agent.rs`):
   - Schema validation for agent config YAML
   - Checks `command` exists on PATH
   - Warns on common misconfigurations (e.g., `injects_settings: true` without `injects_context_file: true`)

5. **Example library** (`templates/workflows/`, `templates/agents/`):
   - 3-4 workflow examples: code-review, deploy-pipeline, security-audit, milestone-review
   - 3-4 agent examples: developer, auditor, planner, orchestrator
   - `ta workflow list --templates` and `ta agent list --templates` to browse

6. **Planner workflow role** — built-in `planner` role for workflow definitions:
   - Uses `agents/planner.yaml` (shipped in v0.9.9.3) as the agent config
   - Enables Plan→Implement→Review→Plan loops in multi-stage workflows
   - Example workflow: `plan-implement-review.yaml` with planner→engineer→reviewer stages
   - The planner stage can receive a document path or objective as input
   - Integrates with `ta plan from` — workflows can invoke planning as a stage

7. **Versioning schema templates** (`templates/version-schemas/`):
   - Pre-built version schema configs users can adopt or customize:
     - `semver.yaml` — standard semver (MAJOR.MINOR.PATCH with pre-release)
     - `calver.yaml` — calendar versioning (YYYY.MM.PATCH)
     - `sprint.yaml` — sprint-based versioning (sprint-N.iteration)
     - `milestone.yaml` — milestone-based (v1, v2, v3 with sub-phases)
   - `ta plan create --version-schema semver` selects a template
   - Schema defines: version format regex, bump rules, phase-to-version mapping
   - Users can write custom schemas in `.ta/version-schema.yaml`

#### Completed
- [x] `ta workflow new <name>` with annotated scaffold and `--from` template selection
- [x] `ta workflow validate <path>` with schema, reference, dependency, and agent config validation
- [x] `ta agent new <name>` with `--type` (developer, auditor, orchestrator, planner) and alignment defaults
- [x] `ta agent validate <path>` with schema validation and PATH checking
- [x] Example library: 5 workflow templates, 6 role templates, 4 agent templates
- [x] `ta workflow list --templates` and `ta agent list --templates` browsing commands
- [x] Planner workflow role with `plan-implement-review.yaml` template
- [x] Versioning schema templates: semver, calver, sprint, milestone
- [x] Validation module in ta-workflow crate with 12 tests
- [x] Agent CLI command module with 10 tests
- [x] Workflow CLI new/validate commands with 7 tests

#### Deferred items moved
- `ta plan create --version-schema` → v0.10.17 (item 9)

#### Version: `0.9.9-alpha.5`

---

### v0.9.10 — Multi-Project Daemon & Office Configuration
<!-- status: done -->
**Goal**: Extend the TA daemon to manage multiple projects simultaneously, with channel-to-project routing so a single Discord bot, Slack app, or email address can serve as the interface for several independent TA workspaces.

#### Problem
Today each `ta daemon` instance serves a single project. Users managing multiple projects need separate daemon instances and separate channel configurations. This makes it impossible to say "@ta inventory-service plan list" in a shared Discord channel — there's no way to route the message to the right project.

#### Architecture

```
                    ┌──────────────────────────────┐
  Discord/Slack/    │      Multi-Project Daemon     │
  Email/CLI ───────▶│                                │
                    │  ┌──────────────────────────┐  │
                    │  │    Message Router         │  │
                    │  │  channel → project map    │  │
                    │  │  thread context tracking  │  │
                    │  │  explicit prefix parsing  │  │
                    │  └──────┬──────┬──────┬──────┘  │
                    │         │      │      │         │
                    │    ┌────▼──┐ ┌─▼───┐ ┌▼────┐   │
                    │    │Proj A │ │Proj B│ │Proj C│  │
                    │    │context│ │ctxt  │ │ctxt  │  │
                    │    └───────┘ └──────┘ └──────┘  │
                    └──────────────────────────────┘
```

Each `ProjectContext` holds:
- Workspace path + `.ta/` directory
- GoalRunStore, DraftStore, AuditLog
- PolicyDocument (per-project)
- ChannelRegistry (per-project, but channel listeners are shared)

#### Items

1. **`ProjectContext` struct**: Encapsulate per-project state (stores, policy, workspace path, plan). Refactor `GatewayState` to hold a `HashMap<String, ProjectContext>` instead of a single project context. Single-project mode (no `office.yaml`) remains the default — wraps current behavior in one `ProjectContext`.
2. **Office config schema**: Define `office.yaml` with `projects`, `channels.routes`, and `daemon` sections:
   ```yaml
   office:
     name: "My Dev Office"
     daemon:
       socket: ~/.ta/office.sock
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
         "#backend-reviews": { project: inventory-service, type: review }
         "#backend-chat":    { project: inventory-service, type: session }
         "#frontend-reviews": { project: customer-portal, type: review }
         "#office-status":   { type: notify, projects: all }
     email:
       routes:
         "backend@acme.dev":  { project: inventory-service, type: review }
         "frontend@acme.dev": { project: customer-portal, type: review }
   ```
3. **Message routing**: Implement channel → project resolution with precedence:
   - Dedicated channel route (from config)
   - Thread context (reply in a goal thread → same project)
   - Explicit prefix (`@ta <project-name> <command>`)
   - User's `default_project` setting
   - Ambiguous → ask user to clarify
4. **`ta office` CLI commands**:
   - `ta office start --config office.yaml` — start multi-project daemon
   - `ta office stop` — graceful shutdown (finish active goals)
   - `ta office status` — overview of projects, active goals, channel connections
   - `ta office status <project>` — per-project detail
   - `ta office project add/remove` — runtime project management
   - `ta office reload` — reload config without restart
5. **Daemon API expansion**: Extend daemon HTTP/socket API with project scoping:
   - All existing endpoints gain optional `?project=<name>` query parameter
   - `GET /api/projects` — list managed projects with status
   - `GET /api/projects/:name/status` — per-project detail
   - `POST /api/projects` — add project at runtime
   - `DELETE /api/projects/:name` — remove project
6. **Per-project overrides**: Support `.ta/office-override.yaml` in each project for project-specific policy or channel overrides that take precedence over the office config.
7. **Backward compatibility**: When no `office.yaml` exists, `ta daemon` works exactly as before (single project). The multi-project behavior is opt-in.

#### Implementation scope
- `crates/ta-daemon/src/project_context.rs` — `ProjectContext` struct with per-project stores (~150 lines)
- `crates/ta-daemon/src/office.rs` — office config parsing, project registry, lifecycle (~200 lines)
- `crates/ta-daemon/src/router.rs` — message routing with channel→project resolution (~150 lines)
- `crates/ta-daemon/src/web.rs` — project-scoped API endpoints (~100 lines)
- `apps/ta-cli/src/commands/office.rs` — `ta office` subcommands (~200 lines)
- `docs/USAGE.md` — multi-project setup guide, office.yaml reference
- Tests: project context isolation, routing precedence, runtime add/remove, backward compat with single-project mode

#### Completed

- [x] `ProjectContext` struct with per-project state encapsulation, path helpers, validation, status summary, per-project overrides from `.ta/office-override.yaml` (8 tests)
- [x] `OfficeConfig` schema parsing (`office.yaml`): office metadata, daemon settings, project entries, channel routing with route targets (7 tests)
- [x] `ProjectRegistry` runtime management: single-project and multi-project modes, add/remove at runtime, default project resolution, names listing
- [x] `MessageRouter` with 5-level precedence routing: dedicated channel route, thread context, explicit `@ta <project>` prefix, user default, ambiguous fallback (10 tests)
- [x] `ta office` CLI commands: start (foreground/background), stop (PID-based), status (overview + per-project detail), project add/remove/list, reload
- [x] Daemon API expansion: `GET /api/projects`, `GET /api/projects/:name`, `POST /api/projects`, `DELETE /api/projects/:name`, `POST /api/office/reload`
- [x] `AppState` extended with `ProjectRegistry`, `resolve_project_root()` for project-scoped queries
- [x] `--office-config` CLI flag and `TA_OFFICE_CONFIG` env var for multi-project daemon startup
- [x] Per-project overrides via `.ta/office-override.yaml` (security_level, default_agent, max_sessions, tags)
- [x] Backward compatibility: no `office.yaml` = single-project mode, all existing behavior preserved
- [x] Version bump to `0.9.10-alpha`

#### Deferred items moved
- Full GatewayState refactor → v0.10.18
- Thread context tracking → v0.10.18
- Config hot-reload → v0.10.18

#### Version: `0.9.10-alpha`

---

### v0.10.0 — Gateway Channel Wiring & Multi-Channel Routing
<!-- status: done -->
**Goal**: Wire `ChannelRegistry` into the MCP gateway so `.ta/config.yaml` actually controls which channels handle reviews, notifications, and escalations — and support routing a single event to multiple channels simultaneously.

#### Completed
- ✅ **Gateway `ChannelRegistry` integration**: `GatewayState::new()` loads `.ta/config.yaml`, builds `ChannelRegistry` via `default_registry()`, resolves `config.channels.review` → `ChannelFactory` → `ReviewChannel`. Replaced hardcoded `AutoApproveChannel` default. Falls back to `TerminalChannel` if config is missing or type is unknown.
- ✅ **Multi-channel routing**: `review` and `escalation` now accept either a single channel object or an array of channels (backward-compatible via `#[serde(untagged)]`). `notify` already supported arrays. Schema supports `strategy: first_response | quorum`.
- ✅ **`MultiReviewChannel` wrapper**: New `MultiReviewChannel` implementing `ReviewChannel` that dispatches to N inner channels. `request_interaction()` tries channels sequentially; first response wins (`first_response`) or collects N approvals (`quorum`). `notify()` fans out to all. 9 tests.
- ✅ **`ta config channels` command**: Shows resolved channel configuration — active channels, types, capabilities, and status. 3 tests.
- ✅ **Channel health check**: `ta config channels --check` verifies each configured channel is buildable (factory exists, config valid).

#### Implementation scope
- `crates/ta-mcp-gateway/src/server.rs` — registry loading, channel resolution
- `crates/ta-changeset/src/multi_channel.rs` — `MultiReviewChannel` wrapper (new)
- `crates/ta-changeset/src/channel_registry.rs` — `ReviewRouteConfig`, `EscalationRouteConfig` enums, `build_review_from_route()`, schema update
- `apps/ta-cli/src/commands/config.rs` — `ta config channels` command (new)
- `docs/USAGE.md` — multi-channel routing docs

#### Version: `0.10.0-alpha`

### v0.10.1 — Native Discord Channel
<!-- status: done -->
**Goal**: `DiscordChannelFactory` implementing `ChannelFactory` with direct Discord REST API connection, eliminating the need for the bridge service.

#### Completed
- ✅ **`ta-channel-discord` crate**: New crate at `crates/ta-channel-discord/` with `reqwest`-based Discord REST API integration (4 modules: lib, channel, factory, payload)
- ✅ **`DiscordReviewChannel`** implementing `ReviewChannel`: rich embeds with buttons, file-based response exchange, sync/async bridge
- ✅ **`DiscordChannelFactory`** implementing `ChannelFactory`: `channel_type()` → `"discord"`, config-driven build with `token_env`, `channel_id`, `response_dir`, `allowed_roles`, `allowed_users`, `timeout_secs`, `poll_interval_secs`
- ✅ **Access control**: `allowed_roles` and `allowed_users` restrict who can approve/deny
- ✅ **Payload builders**: Interaction-kind-aware embeds and buttons
- ✅ **Registry integration**: Registered in MCP gateway and CLI config
- ✅ **30 tests** across all modules

#### Deferred items moved
- Discord deny modal → v0.11.0 (Event-Driven Agent Routing — interactive channel responses)
- Discord thread-based discussions → v0.11.0

#### Config
```yaml
channels:
  review:
    type: discord
    token_env: TA_DISCORD_TOKEN
    channel_id: "123456789"
    allowed_roles: ["reviewer"]
    allowed_users: ["user#1234"]
```

#### Plugin-readiness note

This is built as an in-process Rust crate (the existing pattern). When v0.10.2 (Channel Plugin Loading) lands, this adapter should be refactorable to an external plugin — it already implements `ChannelDelivery` and uses only HTTP/WebSocket. Design the crate so its core logic (message formatting, button handling, webhook response parsing) is separable from the in-process trait impl. This makes it a reference implementation for community plugins in other languages.

#### Version: `0.10.1-alpha`

### v0.10.2 — Channel Plugin Loading (Multi-Language)
<!-- status: done -->
**Goal**: Allow third-party channel plugins without modifying TA source or writing Rust, enabling community-built integrations (Teams, PagerDuty, ServiceNow, etc.) in any language.

#### Current State

The `ChannelDelivery` trait is a clean boundary — it depends only on serializable types from `ta-events`, and the response path is already HTTP (`POST /api/interactions/:id/respond`). But registration is hardcoded: adding a channel requires a new Rust crate in `crates/ta-connectors/`, a dependency in `daemon/Cargo.toml`, and a match arm in `channel_dispatcher.rs`. Users cannot add channels without recompiling TA.

#### Design

Two out-of-process plugin protocols. Both deliver `ChannelQuestion` as JSON and receive answers through the existing HTTP response endpoint. Plugins can be written in any language.

**Protocol 1: JSON-over-stdio (subprocess)**

TA spawns the plugin executable, sends `ChannelQuestion` JSON on stdin, reads a `DeliveryResult` JSON line from stdout. The plugin delivers the question however it wants (API call, email, push notification). When the human responds, the plugin (or the external service's webhook) POSTs to `/api/interactions/:id/respond`.

```
TA daemon
  → spawns: python3 ta-channel-teams.py
  → stdin:  {"interaction_id":"...","question":"What database?","choices":["Postgres","MySQL"],...}
  → stdout: {"channel":"teams","delivery_id":"msg-123","success":true}
  ...later...
  → Teams webhook → POST /api/interactions/:id/respond → answer flows back to agent
```

**Protocol 2: HTTP callback**

TA POSTs `ChannelQuestion` to a configured URL. The external service delivers it and POSTs the response back to `/api/interactions/:id/respond`. No subprocess needed — works with any HTTP-capable service, cloud function, or webhook relay.

```toml
[[channels.external]]
name = "pagerduty"
protocol = "http"
deliver_url = "https://my-service.com/ta/deliver"
auth_token_env = "TA_PAGERDUTY_TOKEN"
```

**Both protocols use the same JSON schema** — `ChannelQuestion` and `DeliveryResult` from `ta-events`. The subprocess just reads/writes them over stdio; the HTTP variant sends/receives them as request/response bodies.

#### Items

1. **`ExternalChannelAdapter`** (`crates/ta-daemon/src/channel_dispatcher.rs`):
   - Implements `ChannelDelivery` by delegating to subprocess or HTTP
   - Subprocess variant: spawn process, write JSON to stdin, read JSON from stdout
   - HTTP variant: POST question JSON to configured URL, parse response
   - Both variants: answers return via existing `/api/interactions/:id/respond`

2. **Plugin manifest** (`channel.toml`):
   ```toml
   name = "teams"
   version = "0.1.0"
   command = "python3 ta-channel-teams.py"  # or any executable
   protocol = "json-stdio"                   # or "http"
   deliver_url = ""                          # only for http protocol
   capabilities = ["deliver_question"]
   ```

3. **Plugin discovery**: Scan `~/.config/ta/plugins/channels/` and `.ta/plugins/channels/` for `channel.toml` manifests. Register each as an `ExternalChannelAdapter` in the `ChannelDispatcher`.

4. **Open `daemon.toml` config** — `[[channels.external]]` array replaces closed-world `ChannelsConfig`:
   ```toml
   [[channels.external]]
   name = "teams"
   command = "ta-channel-teams"
   protocol = "json-stdio"

   [[channels.external]]
   name = "custom-webhook"
   protocol = "http"
   deliver_url = "https://my-service.com/ta/deliver"
   auth_token_env = "TA_CUSTOM_TOKEN"
   ```

5. **`ta plugin list`**: Show installed channel plugins with protocol, capabilities, and validation status.

6. **`ta plugin install <path-or-url>`**: Copy executable + manifest to plugin directory.

7. **Plugin SDK examples** — starter templates in multiple languages:
   - `templates/channel-plugins/python/` — Python channel plugin skeleton
   - `templates/channel-plugins/node/` — Node.js channel plugin skeleton
   - `templates/channel-plugins/go/` — Go channel plugin skeleton
   - Each includes: JSON schema types, stdin/stdout handling, example delivery logic

#### Multi-language plugin example (Python)

```python
#!/usr/bin/env python3
"""TA channel plugin for Microsoft Teams — reads JSON from stdin, posts to Teams."""
import json, sys, requests

def main():
    question = json.loads(sys.stdin.readline())
    # Post to Teams webhook
    resp = requests.post(TEAMS_WEBHOOK, json={
        "type": "message",
        "attachments": [{
            "content": {
                "type": "AdaptiveCard",
                "body": [{"type": "TextBlock", "text": question["question"]}],
                "actions": [{"type": "Action.OpenUrl",
                             "title": "Respond",
                             "url": f"{question['callback_url']}/api/interactions/{question['interaction_id']}/respond"}]
            }
        }]
    })
    print(json.dumps({"channel": "teams", "delivery_id": resp.headers.get("x-msg-id", ""), "success": resp.ok}))

if __name__ == "__main__":
    main()
```

#### Prep: Built-in channels should follow the same pattern

Slack (v0.10.3) and email (v0.10.4) are built as external plugins from the start. Discord (v0.10.1) was built as an in-process crate — it should be refactorable to an external plugin once the plugin system is proven. The long-term goal: TA ships with zero built-in channel adapters; all channels are plugins. The built-in ones are just pre-installed defaults.

#### Completed
- ✅ `PluginManifest` struct with TOML parsing, validation, protocol enum (JsonStdio, Http)
- ✅ Plugin discovery: scans `.ta/plugins/channels/` (project) and `~/.config/ta/plugins/channels/` (global) for `channel.toml` manifests
- ✅ `ExternalChannelAdapter` implementing `ChannelDelivery` for both protocols:
  - JSON-over-stdio: spawn process, write question JSON to stdin, read result from stdout
  - HTTP callback: POST question JSON to configured URL, parse response
  - Comprehensive error handling with actionable messages and timeout support
- ✅ `[[channels.external]]` config in `daemon.toml` for inline plugin registration
- ✅ `ChannelDispatcher::from_config_with_plugins()` — loads inline config + discovered plugins
- ✅ `ta plugin list` — show installed plugins with protocol, capabilities, validation status
- ✅ `ta plugin install <path>` — copy plugin to project or global directory
- ✅ `ta plugin validate` — check commands exist on PATH, URLs are well-formed
- ✅ Plugin SDK templates: Python, Node.js, Go skeletons in `templates/channel-plugins/`
- ✅ 29 tests: manifest parsing, discovery, installation, stdio/HTTP delivery, error paths, validation

#### Deferred items resolved
- Plugin version checking → completed in v0.10.16
- Plugin marketplace / remote install → backlog (no target phase yet)

#### Version: `0.10.2-alpha`

---

### v0.10.2.1 — Refactor Discord Channel to External Plugin
<!-- status: done -->
**Goal**: Extract the in-process `ta-channel-discord` crate into an external plugin using the v0.10.2 plugin system. Validates the plugin architecture with a real, tested adapter and establishes the migration pattern for any future in-process-to-plugin conversions.

#### Approach

The Discord adapter already implements `ChannelDelivery` and uses only HTTP (no daemon internals). The refactoring separates the core logic (message formatting, embed building, button handling) from the in-process trait impl, then wraps it in a standalone binary that speaks JSON-over-stdio.

#### Completed
1. [x] Extract core Discord logic (payload builders, embed formatting) into `plugins/ta-channel-discord/src/payload.rs`
2. [x] Create standalone binary (`plugins/ta-channel-discord/src/main.rs`) that reads `ChannelQuestion` from stdin, calls Discord REST API, writes `DeliveryResult` to stdout — 13 tests
3. [x] Add `channel.toml` manifest for plugin discovery
4. [x] Remove `ta-channel-discord` crate from workspace — Discord becomes a pre-installed plugin, not a compiled-in dependency
5. [x] Update `ChannelDispatcher` registration to load Discord via plugin system instead of hardcoded match arm — daemon now emits migration warning for old `[channels.discord]` config
6. [x] Migrate Discord config from in-process `ChannelsConfig` to `[[channels.external]]` in `daemon.toml` — old config produces deprecation warning
7. [x] Verify all workspace tests pass (existing Discord connector tests in ta-connector-discord still pass; plugin has its own 13 tests)
8. [x] Update docs: discord-channel guide rewritten for plugin architecture

#### Version: `0.10.2-alpha.1`

---

### v0.10.2.2 — `ta plugin build` Command
<!-- status: done -->
**Goal**: Add a CLI command to build plugin binaries from source, removing the manual `cd && cargo build && cp` workflow.

#### Usage
```bash
# Build a specific plugin
ta plugin build discord

# Build multiple plugins
ta plugin build discord,slack,email

# Build all plugins found in plugins/
ta plugin build --all
```

#### Behavior
1. Discover plugin source directories under `plugins/ta-channel-<name>/`
2. Run `cargo build --release` in each plugin directory
3. Copy the compiled binary + `channel.toml` to `.ta/plugins/channels/<name>/`
4. Print summary: which plugins built, binary size, install path

#### Completed
1. [x] `PluginCommands::Build` variant in `apps/ta-cli/src/commands/plugin.rs` with `names: Vec<String>` and `--all` flag
2. [x] Plugin source discovery: scan `plugins/` directory for `Cargo.toml` + `channel.toml` pairs
3. [x] Build runner: invoke `cargo build --release` in plugin directory, capture output, report errors
4. [x] Install step: copy binary + manifest to `.ta/plugins/channels/<name>/`
5. [x] `--all` flag: discover and build every plugin in `plugins/`
6. [x] Output: progress per plugin, success/failure summary, binary paths
7. [x] Error handling: continue building remaining plugins if one fails, report all failures at end
8. [x] 13 new tests: discovery, binary name extraction, name resolution, error paths, formatting

#### Version: `0.10.2-alpha.2`

---

### v0.10.3 — Slack Channel Plugin
<!-- status: done -->
**Goal**: Slack channel plugin built on the v0.10.2 plugin system — validates that the plugin loading infrastructure works end-to-end with a real service.

#### Approach

Built as an external plugin (JSON-over-stdio or standalone Rust binary), not an in-process crate. Uses Slack Block Kit for rich review messages and Socket Mode for outbound-only connectivity.

#### Completed
1. ✅ **Plugin binary** (`plugins/ta-channel-slack/`): Reads `ChannelQuestion` JSON from stdin, posts Block Kit message with Approve/Deny buttons to Slack via `chat.postMessage`, writes `DeliveryResult` to stdout — 17 tests
2. ✅ **Thread-based detail**: Posts context as thread reply when context exceeds 500 chars (best-effort, non-blocking)
3. ✅ **`channel.toml` manifest**: Plugin discovery via standard plugin loading (v0.10.2)
4. ✅ **Block Kit payloads**: Header, question section, context section, interactive buttons (yes/no, choice, freeform), interaction ID footer
5. ✅ **Actionable error messages**: Missing token, missing channel ID, Slack API errors with permission hints
6. ✅ **`allowed_users` env var**: `TA_SLACK_ALLOWED_USERS` documented for access control integration

#### Deferred items moved
- Slack Socket Mode + deny modal + HTTP mode → v0.11.0 (Event-Driven Agent Routing — interactive channel responses)

#### Config
```toml
[[channels.external]]
name = "slack"
command = "ta-channel-slack"
protocol = "json-stdio"

# Plugin reads these env vars directly
# TA_SLACK_BOT_TOKEN, TA_SLACK_CHANNEL_ID
# TA_SLACK_ALLOWED_USERS (optional, comma-separated user IDs)
```

#### Version: `0.10.3-alpha`

---

### v0.10.4 — Email Channel Plugin
<!-- status: done -->
**Goal**: Email channel plugin built on the v0.10.2 plugin system — demonstrates the plugin model works for async, non-real-time channels.

#### Approach

Built as an external plugin. Sends formatted review emails via SMTP, polls IMAP for reply-based approval. Email is inherently slower than chat — validates that the plugin/interaction model handles longer response times gracefully.

#### Completed
- ✅ Plugin binary (`plugins/ta-channel-email/`): standalone Rust binary using JSON-over-stdio protocol, reads `ChannelQuestion` from stdin, sends via SMTP (lettre), writes `DeliveryResult` to stdout
- ✅ Subject tagging: configurable prefix (default `[TA Review]`) with `X-TA-Request-ID`, `X-TA-Interaction-ID`, `X-TA-Goal-ID` headers for threading
- ✅ Reply parsing module: strips quoted text (`>` lines, `On ... wrote:` blocks, signatures, mobile footers), recognizes APPROVE/DENY/YES/NO/LGTM/REJECT keywords — supports English, French, German attribution patterns
- ✅ Multiple reviewers: comma-separated `TA_EMAIL_REVIEWER` list, all receive the email (first to reply wins)
- ✅ App Password support: STARTTLS SMTP with username/password auth (works with Gmail App Passwords, no OAuth)
- ✅ Email threading: Message-ID based on interaction_id, follow-up turns use In-Reply-To/References headers
- ✅ HTML + plain text multipart emails with structured layout, interactive guidance per question type
- ✅ `channel.toml` manifest for standard plugin discovery (v0.10.2)
- ✅ HTML body escapes user content to prevent XSS
- ✅ 36 tests: email body builders (16), reply parsing (15), serialization/config (5)

#### Deferred items moved
- IMAP reply polling + configurable timeout → v0.11.0 (Event-Driven Agent Routing)
- Plugin version checking → completed in v0.10.16

#### Config
```toml
[[channels.external]]
name = "email"
command = "ta-channel-email"
protocol = "json-stdio"

# Plugin reads these env vars directly
# TA_EMAIL_SMTP_HOST, TA_EMAIL_SMTP_PORT (default: 587)
# TA_EMAIL_USER, TA_EMAIL_PASSWORD
# TA_EMAIL_REVIEWER (comma-separated)
# TA_EMAIL_FROM_NAME (default: "TA Agent")
# TA_EMAIL_SUBJECT_PREFIX (default: "[TA Review]")
```

#### Version: `0.10.4-alpha`

---

### v0.10.5 — External Workflow & Agent Definitions
<!-- status: done -->
**Goal**: Allow workflow definitions and agent configurations to be pulled from external sources (registries, git repos, URLs) so teams and third-party authors can publish reusable configurations. Include an automated release process with press-release generation.

#### Problem
Today, workflow YAML files and agent configs (`agents/*.yaml`) live only in the project's `.ta/` directory. There's no mechanism to:
- Share a workflow across multiple projects
- Publish an agent configuration for others to use (e.g., "security-reviewer" agent with specialized system prompt)
- Pull in community-authored configurations
- Generate release communications automatically as part of `ta release`

Builds on v0.9.9.5 (local authoring tooling: `ta workflow new`, `ta workflow validate`, `ta agent new`, `ta agent validate`) by adding the external distribution layer.

#### Design

##### 1. External workflow/agent sources
```bash
# Pull a workflow from a registry
ta workflow add security-review --from registry:trustedautonomy/workflows
ta workflow add deploy-pipeline --from gh:myorg/ta-workflows

# Pull an agent config
ta agent add security-reviewer --from registry:trustedautonomy/agents
ta agent add code-auditor --from https://example.com/ta-agents/auditor.yaml

# List installed external configs
ta workflow list --source external
ta agent list --source external
```

##### 2. Workflow/agent package format
```yaml
# workflow-package.yaml (published to registry)
name: security-review
version: 1.0.0
author: trustedautonomy
description: "Multi-step security review workflow with SAST, dependency audit, and manual sign-off"
ta_version: ">=0.9.8"
files:
  - workflows/security-review.yaml
  - agents/security-reviewer.yaml
  - policies/security-baseline.yaml
```

##### 3. Release press-release generation
The `ta release` process includes an optional press-release authoring step where an agent generates a release announcement from the changelog, guided by a user-provided sample:

```bash
# Configure a sample press release as the style template
ta release config set press_release_template ./samples/sample-press-release.md

# During release, the agent generates a press release matching the sample's style
ta release run --press-release

# The user can update the prompt to refine the output
ta release run --press-release --prompt "Focus on the workflow engine and VCS adapter features"
```

The agent reads the changelog/release notes, follows the style and tone of the sample document, and produces a draft press release that goes through the normal TA review process (draft → approve → apply).

##### 4. Workflow authoring and publishing
```bash
# Author a new workflow
ta workflow new deploy-pipeline
# Edit .ta/workflows/deploy-pipeline.yaml

# Publish to registry
ta workflow publish deploy-pipeline --registry trustedautonomy

# Version management
ta workflow publish deploy-pipeline --bump minor
```

#### Completed
1. [x] External source resolver: registry, GitHub repo, and raw URL fetching for YAML configs
2. [x] `ta workflow add/remove/list` commands with `--from` source parameter
3. [x] `ta agent add/remove/list` commands with `--from` source parameter
4. [x] Workflow/agent package manifest format (`workflow-package.yaml`)
5. [x] Local cache for external configs (`~/.ta/cache/workflows/`, `~/.ta/cache/agents/`)
6. [x] Version pinning and update checking for external configs
7. [x] `ta release` press-release generation step with sample-based style matching
8. [x] Press release template configuration (`ta release config set press_release_template`)
9. [x] `ta workflow publish` command for authoring and publishing to registry
10. [x] Documentation: authoring guide for workflow/agent packages
11. [x] **Multi-language plugin builds**: Add `build_command` field to `channel.toml` so `ta plugin build` works with non-Rust plugins (Python, Go, Node). Rust plugins default to `cargo build --release`; others specify their own build step (e.g., `go build -o ta-channel-teams .`, `pip install -e .`). Extend v0.10.2.2's build runner to read and execute `build_command`.

#### Version: `0.10.5-alpha`

---

### v0.10.6 — Release Process Hardening & Interactive Release Flow
<!-- status: done -->
**Goal**: Fix release process issues, harden the `ta release run` pipeline, and make releases an interactive-mode workflow so the human never leaves `ta shell`.

#### Known Bugs
- ~~**Releases always marked pre-release**: `release.yml` auto-detected `alpha`/`beta` in the version string and set `prerelease: true`, which meant GitHub never updated "latest release". Fixed in v0.9.9.1 — default is now latest, with explicit `--prerelease` input on `workflow_dispatch`.~~ ✅
- **`ta_fs_write` forbidden in orchestrator mode**: The release notes agent tries to write `.release-draft.md` directly but is blocked by orchestrator policy. The agent should either use `ta_goal` to delegate the write, or the orchestrator policy should whitelist release artifact writes. Filed as bug — the process should just work without the agent needing workarounds.
- **Release notes agent workaround**: Currently the agent works around the `ta_fs_write` restriction by using alternative write methods, but this is fragile and shouldn't be necessary.

#### Interactive Release Flow

Today `ta release run` runs synchronously in the foreground — the human must exit the agent, review notes externally, then re-run. The release should be a background goal that uses interactive mode for human review checkpoints:

```
ta shell> release v0.10.6
  → TA launches release agent as background goal
  → Agent generates changelog, release notes draft
  → Agent calls ta_ask_human: "Draft release notes below. Any changes?"
  → Human reviews in ta shell, responds with feedback
  → Agent revises, calls ta_ask_human: "Updated. Ready to publish?"
  → Human: "yes"
  → Agent bumps version, tags, pushes — GH Actions takes over
  → TA emits release_completed event
  → Shell shows: "Release v0.10.6 published. View: https://github.com/..."
```

The human stays in `ta shell` throughout. Release notes go through the standard draft review flow. Interactive mode (v0.9.9.1–v0.9.9.2) provides the `ta_ask_human` infrastructure.

#### Completed
1. [x] Fix `ta_fs_write` permission in orchestrator mode for release artifact files (`.release-draft.md`, `CHANGELOG.md`) — added `ORCHESTRATOR_WRITE_WHITELIST` to `CallerMode` and updated `handle_fs_write` to check path before blocking
2. [x] Add orchestrator-mode write whitelist for release-specific file patterns — `is_write_whitelisted()` method on `CallerMode` matches filenames against `.release-draft.md`, `CHANGELOG.md`, `version.json`, `.press-release-draft.md`
3. [x] End-to-end test for `ta release run` pipeline without manual intervention — `e2e_pipeline_no_manual_gates` test with marker file verification
4. [x] Release dry-run mode: `ta release run --dry-run` validates all steps without publishing — existing `--dry-run` flag + new `ta release validate` command for pre-flight checks (version format, git state, tag availability, pipeline config, toolchain)
5. [x] **Background goal launch from shell**: `release` shortcut in shell config expands to `ta release run`, long-running command classification ensures background execution via daemon
6. [x] **Interactive release agent**: `ta release run --interactive` launches the `releaser` agent with `ta_ask_human`-based review checkpoints
7. [x] **`agents/releaser.yaml`**: Release agent config with `ta_ask_human` enabled, write access scoped to release artifacts via orchestrator whitelist
8. [x] **Release workflow definition**: `templates/workflows/release.yaml` — 4-stage workflow (validate → generate-notes → build-verify → publish) with human review at notes and publish stages

#### Deferred items moved
- Wire `ta sync`/`ta build` in release → v0.10.18 (depends on v0.11.1, v0.11.2)

#### Version: `0.10.6-alpha`

---

### v0.10.7 — Documentation Review & Consolidation
<!-- status: done -->
**Goal**: Full documentation audit and refinement pass after the v0.10.x feature set is complete. Ensure all docs are accurate, consistent, and organized for both users and integration developers.

#### Scope
- **USAGE.md**: Verify all commands, flags, and config options are documented. Remove stale references. Ensure progressive disclosure (getting started → daily use → advanced). Add examples for every config section.
- **MISSION-AND-SCOPE.md**: Confirm feature boundary decisions match implementation. Update protocol tables if anything changed. Validate the scope test against actual shipped features.
- **CLAUDE.md**: Trim to essentials. Remove references to completed phases. Ensure build/verify instructions are current.
- **PLAN.md**: Archive completed phases into a collapsed section or separate `docs/PLAN-ARCHIVE.md`. Keep active phases clean.
- **README.md**: Update for current state — accurate feature list, installation instructions, quick-start guide.
- **ADRs** (`docs/adr/`): Ensure all significant decisions have ADRs. Check that existing ADRs aren't contradicted by later work.
- **Plugin/integration docs**: Verify JSON schema examples match actual types. Add end-to-end plugin authoring guide if missing.
- **Cross-doc consistency**: Terminology (draft, goal, artifact, staging), config field names, version references.

#### Completed
1. [x] Audit USAGE.md against current CLI `--help` output for every subcommand — verified all 25 subcommands documented, added missing `accept-terms`/`view-terms`/`terms-status` commands, updated version to v0.10.7-alpha
2. [x] Audit MISSION-AND-SCOPE.md protocol/auth tables against actual implementation — protocol table verified accurate, updated `ta schema export` reference to note it's still planned
3. [x] Review and update README.md for current feature set and installation — updated version badges, current status, project structure, MCP tools table, and "What's Implemented" section
4. [x] Archive completed PLAN.md phases (pre-v0.9) into `docs/PLAN-ARCHIVE.md` — moved ~2000 lines (Phase 0 through v0.8.2) to `docs/PLAN-ARCHIVE.md`, replaced with collapsed reference
5. [x] Verify all config examples in docs parse correctly against current schema — reviewed workflow.toml, config.yaml, policy.yaml, daemon.toml, office.yaml, and channel.toml against codebase structs
6. [x] Cross-reference ADRs with implementation — updated ADR-modular-decomposition status to "Deferred", updated ADR-product-concept-model crate map to reflect current implementation status
7. [x] Add plugin authoring quickstart guide (`docs/PLUGIN-AUTHORING.md`) with end-to-end example — created comprehensive guide with Python and Rust examples, JSON schemas, manifest format, and testing instructions
8. [x] Terminology consistency pass across all docs — verified Draft/PR terminology, staging/virtual-workspace usage, version references updated across USAGE.md, README.md, CLAUDE.md

#### Version: `0.10.7-alpha`

---

### v0.10.8 — Pre-Draft Verification Gate
<!-- status: done -->
**Goal**: Run configurable build/lint/test checks after the agent exits but before the draft is created. Catches CI failures locally so broken drafts never reach review.

#### Problem
Today `ta run` builds a draft as soon as the agent exits — even if the agent left broken code. The user reviews, approves, applies, pushes, and CI fails. That's a wasted cycle. If TA runs the same checks CI would run *before* creating the draft, failures are caught immediately.

#### Design
A `[verify]` section in `.ta/workflow.toml` defines commands to run in the staging directory after the agent exits. If any command fails, the draft is not created — the agent can be re-entered (`ta run --follow-up`) to fix the issue.

```toml
[verify]
# Commands run in staging dir after agent exits, before draft build.
# All must pass (exit 0) for the draft to be created.
commands = [
    "cargo build --workspace",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
]

# On failure: "block" (no draft, default), "warn" (create draft with warning), "agent" (re-launch agent with error context)
on_failure = "block"

# Timeout per command in seconds (default: 300)
timeout = 300
```

#### Behavior
1. Agent exits normally
2. TA runs each verify command sequentially in the staging directory
3. **All pass**: Draft is built as normal
4. **Any fail** (`on_failure = "block"`): No draft created. Print which command failed with output. Suggest `ta run --follow-up` to fix.
5. **Any fail** (`on_failure = "warn"`): Draft is created with a verification warning visible in `ta draft view`
6. **Any fail** (`on_failure = "agent"`): Re-launch the agent with the failure output injected as context (uses interactive mode if available)

#### Completed
1. ✅ `VerifyConfig` struct in `crates/ta-submit/src/config.rs`: `commands`, `on_failure` (enum: Block/Warn/Agent), `timeout` with serde defaults
2. ✅ `run_verification()` in `apps/ta-cli/src/commands/verify.rs`: runs commands sequentially with per-command timeout, captures output, returns `VerificationResult`
3. ✅ Wire into `ta run` flow: verification runs after agent exit + file restoration, before `ta draft build`
4. ✅ Block mode: aborts draft creation on failure, prints failed commands with output, suggests `ta run --follow-up` and `ta verify`
5. ✅ Warn mode: creates draft with `verification_warnings` field on `DraftPackage`, displayed in `ta draft view` with command, exit code, and output
6. ✅ Agent mode: stub implemented (falls back to block with message that re-launch is not yet implemented)
7. ✅ `--skip-verify` flag on `ta run` to bypass verification
8. ✅ Default `[verify]` section in `ta init` template: Rust projects get pre-populated commands; others get commented-out examples
9. ✅ `ta verify` standalone command: resolves goal by ID/prefix or most recent active goal, loads `[verify]` from staging's workflow.toml, runs verification, exits with code 1 on failure

#### Deferred items moved
- Agent mode re-launch with failure context → v0.11.0 (Event-Driven Agent Routing)

#### Tests
- 7 new config tests: defaults, TOML parsing for all modes, display formatting
- 5 new verification tests: empty commands pass, passing/failing commands, mixed commands, output capture, timeout handling

#### Version: `0.10.8-alpha`

---

### v0.10.9 — Smart Follow-Up UX
<!-- status: done -->
**Goal**: Make `ta run --follow-up` a frictionless, context-aware entry point that works across VCS backends, channels, and workflow types — without requiring the user to know branch names, draft IDs, or internal state.

#### Problem
Today `--follow-up` requires the user to know which git branch holds the prior work, pass it explicitly, and understand the staging directory layout. This is wrong friction — especially for non-technical users working through email, social media, or DB migration workflows. The user's mental model is "I want to continue working on *that thing*" — TA should resolve what "that thing" means.

#### Design
`ta run --follow-up` (with no additional arguments) enters an interactive selection flow:

1. **Gather candidates**: Scan recent goals, active drafts, in-progress plan phases, and open verification failures. Each candidate carries enough context to display a one-line summary.
2. **Present picker**: Show a numbered list (or fuzzy-searchable in shell TUI) of follow-up candidates, sorted by recency. Each entry shows: phase/goal title, status (e.g., "draft denied", "verify failed", "in progress"), and age.
3. **User selects**: User picks by number or searches. TA resolves the selection to the correct staging directory, branch, draft, or channel context.
4. **Context injection**: TA injects relevant follow-up context into the agent's CLAUDE.md — what was attempted, what failed, what the user or reviewer said. The agent picks up where it left off.

When a specific target is known, shortcuts still work:
- `ta run --follow-up --phase 4b` — resume work on plan phase 4b
- `ta run --follow-up --draft <id>` — follow up on a specific draft (denied, failed verify, etc.)
- `ta run --follow-up --goal <id>` — continue from a prior goal's staging

#### VCS & Channel Agnosticism
The follow-up resolver doesn't assume git. It works from TA's own state:
- **Goals**: `GoalRun` records in `.ta/goals/` — each has staging path, status, plan phase
- **Drafts**: `DraftPackage` records — status, denial reason, verification warnings
- **Plan phases**: `PLAN.md` status markers — in_progress phases are follow-up candidates
- **Channel context**: For non-filesystem workflows (email drafts, social media posts, DB migrations), the follow-up context comes from the draft's `PatchSet` and interaction log rather than a git branch

#### Completed
1. ✅ `FollowUpCandidate` struct in `apps/ta-cli/src/commands/follow_up.rs`: `source` (CandidateSource enum: Goal/Draft/Phase/VerifyFailure), `title`, `status`, `age`, `staging_path`, `context_summary`, `denial_reason`, `verification_warnings`
2. ✅ `gather_follow_up_candidates()`: scans goals, drafts, plan phases; filters to actionable items (failed, running, denied, verify-warned, in-progress phases); sorts by recency
3. ✅ Interactive picker in `ta run --follow-up` (no args): numbered candidate list with source tags, status, age, and context summaries; user selects by number
4. ✅ `--follow-up --phase <id>` shortcut: `resolve_by_phase()` finds most recent goal for a plan phase, with phase ID normalization (v-prefix handling)
5. ✅ `--follow-up-draft <id>` CLI flag: `resolve_by_draft()` resolves from draft prefix, injects denial reason and verify failure context
6. ✅ `--follow-up-goal <id>` CLI flag: `resolve_by_goal()` resolves from goal prefix with rich context injection
7. ✅ Context injection: `build_follow_up_context()` builds CLAUDE.md section with prior goal summary, draft status, verification failures (with command output), denial reasons, discuss items with review comments
8. ✅ `resolve_smart_follow_up()` in `run.rs`: priority-based resolution (draft > goal > phase > interactive picker > existing behavior); produces title, phase, follow-up ID, and context string
9. ✅ Channel-agnostic resolution: follow-up resolver works from TA's own state (GoalRun records, DraftPackage records, PLAN.md phases) without assuming git

#### Deferred items moved
- Shell TUI fuzzy-searchable picker → backlog (TUI enhancement, no target phase)

#### Tests
- 13 new tests in `follow_up.rs`: format_age (4 variants), truncate (2 variants), candidate display, candidate source display, empty picker error, goal state filtering (completed skipped, failed included, running included), phase filtering (only in-progress), basic candidate creation

#### Version: `0.10.9-alpha`

---

### v0.10.10 — Daemon Version Guard
<!-- status: done -->
**Goal**: `ta shell` (and other CLI commands that talk to the daemon) should detect when the running daemon is an older version than the CLI and offer to restart it — rather than silently connecting to a stale daemon.

#### Problem
After `./install_local.sh` rebuilds and installs new `ta` and `ta-daemon` binaries, the old daemon process keeps running. `ta shell` connects to it, shows the version in the status bar, but doesn't warn the user or offer to restart. The user has to notice the mismatch and manually restart. This is especially confusing after upgrades since new features may not work against the old daemon.

#### Design
1. The daemon already exposes its version via `GET /api/status` (or similar health endpoint). The CLI knows its own version from `env!("CARGO_PKG_VERSION")`.
2. On connection, `ta shell` (and `ta run`, `ta dev`, etc.) compares CLI version to daemon version.
3. **If mismatch**: Display a prominent warning and offer to restart:
   ```
   Daemon version mismatch: daemon v0.10.6-alpha, CLI v0.10.10-alpha
   Restart daemon with the new version? [Y/n]
   ```
4. If the user accepts, the CLI stops the old daemon (`POST /api/shutdown` or signal), waits for exit, then spawns the new one.
5. If the user declines, proceed with a warning in the status bar (e.g., `daemon (stale)`).

#### Completed
1. ✅ `GET /api/status` response includes `daemon_version` field — added alongside existing `version` field in `ProjectStatus`
2. ✅ `check_daemon_version()` in `version_guard.rs`: compares `env!("CARGO_PKG_VERSION")` to daemon's reported version, prompts interactively, returns `VersionGuardResult` enum
3. ✅ Wired into `ta shell` startup (both classic and TUI modes): version check runs before entering the shell loop, prompts user to restart if mismatch
4. ✅ Wired into `ta dev`: version check before launching orchestrator agent
5. ✅ Restart flow: `POST /api/shutdown` graceful endpoint → wait for exit (5s timeout) → find daemon binary (sibling or PATH) → spawn new daemon → wait for healthy (10s) → verify version matches
6. ✅ `--no-version-check` global CLI flag to skip (for CI or scripted use)
7. ✅ TUI status bar: shows `◉ daemon (stale)` in yellow if daemon version doesn't match CLI version

#### Tests
- 3 unit tests in `version_guard.rs`: variant construction, `find_daemon_binary` safety, stale result version extraction

#### Version: `0.10.10-alpha`

---

### v0.10.11 — Shell TUI UX Overhaul
<!-- status: done -->
**Goal**: Make `ta shell` a fully usable interactive environment where agent output is visible, long output is navigable, and the user never has to leave the shell to understand what's happening.

#### Problem
Today `ta shell` has several UX gaps that force users to work around the TUI rather than through it:
- Starting a goal produces no output — the agent runs blind. User must manually `:tail` and even then sees only TA lifecycle events, not the agent's actual stdout/stderr.
- Long command output (draft list, draft view) scrolls off the top of the viewport with no way to scroll back.
- Draft IDs are unrelated to goal IDs, requiring mental mapping or `draft list --goal` lookups.
- No notification when a draft is ready — user must poll with `draft list`.
- `:tail` gives no confirmation it's working and shows no backfill of prior output.

#### Completed

1. ✅ **Agent output streaming**: TUI `:tail` command connects to `GET /api/goals/:id/output` SSE endpoint, streams `AgentOutput` messages as styled lines (stdout=white, stderr=yellow). Interleaves with TA events in unified output pane.
2. ✅ **Auto-tail on goal start**: SSE parser detects `goal_started` events and auto-subscribes to agent output. Single goal auto-tails immediately. Multiple goals prompt selection via `:tail <id>`. Configurable via `shell.auto_tail` in workflow.toml.
3. ✅ **Tail backfill and confirmation**: Prints confirmation on tail start with goal ID. Visual separator `─── live output ───` between backfill and live output. Configurable `shell.tail_backfill_lines` (default 5).
4. ✅ **Draft-ready notification**: SSE parser detects `draft_built` events and renders `[draft ready] "title" (display_id) — run: draft view <id>` with bold green styling. Status bar shows tailing indicator.
5. ✅ **Draft ID derived from goal ID**: New `display_id` field on `DraftPackage` in format `<goal-prefix>-NN` (e.g., `511e0465-01`). Resolver matches on `display_id` alongside UUID prefix. Legacy drafts fall back to 8-char package_id prefix. `draft list` shows display_id instead of full UUID.
6. ✅ **Draft list filtering, ordering, and paging**: Default ordering newest-last. `--pending`, `--applied` status filters. Compact default view (active/pending only). `--all` shows everything. `--limit N` for paged output. `draft list --goal <id>` preserved from v0.10.8.
7. ✅ **Draft view paging / scrollable output**: TUI retains all output in scrollable buffer with PgUp/PgDn. Command output (draft view, list, etc.) rendered into the same scrollable buffer.
8. ✅ **Scrollable output buffer (foundational)**: TUI output pane retains full history with configurable buffer limit (`shell.output_buffer_lines`, default 10000). Oldest lines dropped when limit exceeded. Scroll offset adjusted when lines are pruned.

#### Deferred items resolved
- `:tail --lines` override → completed in v0.10.14
- Classic shell pager → dropped (TUI scrollable output supersedes this)
- Progressive disclosure for draft view → backlog (TUI enhancement, no target phase)

#### Tests
- 14 new tests in `shell_tui.rs`: parse_goal_started_event, parse_goal_started_ignores_other_events, parse_draft_built_event, parse_draft_built_fallback_display_id, parse_draft_built_ignores_other_events, handle_agent_output_message, handle_agent_stderr_output, handle_goal_started_auto_tail, handle_goal_started_no_auto_tail_when_already_tailing, handle_goal_started_no_auto_tail_when_disabled, handle_agent_output_done_clears_tail, handle_draft_ready_notification, output_buffer_limit_enforced, output_buffer_limit_adjusts_scroll
- 4 new tests in `config.rs`: shell_config_defaults, workflow_config_default_has_shell_section, parse_toml_with_shell_section, parse_toml_without_shell_section_uses_default

#### Version: `0.10.11-alpha`

---

### v0.10.12 — Streaming Agent Q&A & Status Bar Enhancements
<!-- status: done -->
**Goal**: Eliminate 60s+ latency in `ta shell` Q&A by streaming agent responses instead of blocking, and add daemon version + agent name to the TUI status bar.

#### Problem
When the user asks a question in `ta shell`, the daemon spawned `claude --print` synchronously and blocked until the entire response was ready — often 60+ seconds with no feedback. The user had no indication the system was working. Additionally, the TUI status bar showed no information about the daemon version or which agent was handling Q&A.

#### Completed
1. ✅ **Streaming agent ask**: Refactored `ask_agent()` from blocking to streaming. Now creates a `GoalOutput` broadcast channel, spawns the agent subprocess in `tokio::spawn`, and returns an immediate ack with `request_id` and `status: "processing"`. Client subscribes to `GET /api/goals/:request_id/output` SSE stream for real-time output.
2. ✅ **`__streaming__:` protocol**: `send_input()` in shell.rs detects `status: "processing"` responses and returns a `__streaming__:<request_id>` marker. TUI intercepts this and subscribes to the SSE stream via `start_tail_stream()`.
3. ✅ **Daemon version in status bar**: `ProjectStatus` now includes `daemon_version` field. TUI status bar shows `◉ daemon <version>` with stale detection (yellow when version doesn't match CLI).
4. ✅ **Default agent in status bar**: `ProjectStatus` now includes `default_agent` field. TUI status bar shows the configured Q&A agent name (e.g., `claude-code`) in magenta.
5. ✅ **Removed fake "Thinking..." indicator**: Client-side fake indicator removed. The TUI now shows "Agent is working..." only after receiving the real ack from the daemon, then streams actual output.

#### Version: `0.10.12-alpha`

---

### v0.10.13 — `ta plan add` Command (Agent-Powered Plan Updates)
<!-- status: pending -->
**Goal**: Add a `ta plan add` command that uses the planner agent to intelligently update PLAN.md through interactive dialog — not just raw text insertion.

#### Problem
Today, updating PLAN.md requires manual editing or knowing the exact phase structure. There's no way to say "add a phase for status bar improvements" and have the system figure out where it goes, what version number to assign, and what items belong in it. `ta plan create` generates a plan from scratch; `ta plan add` should modify an existing plan intelligently.

#### Design
```
ta> plan add "Update ta shell status bar to show active Q&A agent model"

Agent: I'll add this to the plan. A few questions:
  1. Should this be a standalone phase or added to an existing one?
  2. This requires detecting the model from the agent binary — should
     that be a separate prerequisite phase?

You: Standalone phase after v0.10.12. The model detection can be
     a future item within the same phase.

Agent: Added v0.10.14 — Agent Model Discovery & Status Display
       - Detect LLM model name from agent process (framework-specific)
       - Display model name in TUI status bar
       - Future: Model capabilities reporting for smart routing
```

#### Items
1. **`ta plan add <description>` CLI command**: Launches a planner agent session with the current PLAN.md as context. The agent proposes placement, version number, and items through interactive Q&A.
2. **Existing plan awareness**: Agent reads current PLAN.md, understands phase ordering, version numbering, status markers, and dependencies.
3. **Diff-based output**: Agent produces a PLAN.md diff that goes through standard draft review (not direct write).
4. **Shell integration**: `plan add` available as a shell command, runs as background goal with interactive mode.
5. **Non-interactive mode**: `ta plan add "description" --auto` for CI/scripted use — agent makes best-guess placement without asking questions.

#### Completed
- [x] `ta plan add <description>` CLI command with `--agent`, `--source`, `--after`, `--auto`, `--follow-up` flags
- [x] Existing plan awareness: reads PLAN.md, parses phases, validates `--after` phase ID, reports plan summary (total/done/pending)
- [x] Diff-based output: delegates to `ta run` so changes go through standard draft review
- [x] Shell integration: `plan add <desc>` available as shell shortcut in both classic and TUI shells
- [x] Non-interactive mode: `--auto` flag skips interactive Q&A, agent makes best-guess placement
- [x] `build_plan_add_prompt()`: constructs agent prompt with full plan context, placement hints, and format rules
- [x] `truncate_title()` helper for display-friendly goal titles
- [x] Error handling: missing plan, empty plan, invalid `--after` phase ID with actionable messages
- [x] 13 new tests (11 plan_add tests + 2 truncate_title tests)

#### Version: `0.10.13-alpha`

---

### v0.10.14 — Deferred Items: Shell & Agent UX
<!-- status: done -->
**Goal**: Address deferred shell and agent UX items that improve daily workflow before the v0.11 architecture changes.

#### Completed
1. ✅ **`:tail <id> --lines <count>` override**: Added `parse_tail_args()` with `--lines N` / `-n N` support in TUI and classic shell. 6 tests.
2. ✅ **Streaming agent response rendering**: `stylize_markdown_line()` renders `**bold**`, `` `code` ``, `# headers`, and fenced code blocks with ratatui Span styles in the agent split pane. 6 tests.
3. ✅ **Ctrl+C interrupt**: Detaches from tail or cancels pending question before exiting. Updated Ctrl+C handler in TUI.
4. ✅ **Non-disruptive event notifications**: Classic shell reprints `ta> ` prompt after SSE event display. TUI already handles this natively.
5. ✅ **Split pane support**: Ctrl-W toggles 50/50 horizontal split. Agent output routes to right pane when split. `draw_agent_pane()` with scroll support.
6. ✅ **Agent model discovery**: `extract_model_from_stream_json()` parses `message_start` events, `humanize_model_name()` converts model IDs. Displayed in status bar (Blue). 5 tests.
7. ✅ **Progressive disclosure for draft view**: `ChangeSetDiffProvider` replaces stub `StagingDiffProvider`. Loads changesets from `JsonFileStore`, resolves `changeset:N` refs to actual diff content (unified diff, create file, delete file, binary). Wired into `view_package()` when `--detail full`. 6 tests.
8. ✅ **Shell TUI fuzzy-searchable follow-up picker**: `:follow-up [filter]` command gathers candidates via `gather_follow_up_candidates()`, displays numbered list with source tags, color-coded by type, supports keyword filtering.
9. ✅ **Agent mode for verification failures**: Full `VerifyOnFailure::Agent` implementation in `run.rs`. Builds failure context, re-injects into CLAUDE.md, re-launches agent, re-runs verification, blocks if still failing.
10. ✅ **Input line text wrap**: `Wrap { trim: false }` on input paragraph, wrap-aware cursor positioning (cursor_y = chars/width, cursor_x = chars%width).
11. ✅ **Interactive release approval via TUI**: `prompt_approval_with_auto()` uses file-based interactions (`.ta/interactions/pending/`) for non-TTY contexts, enabling TUI `AgentQuestion` flow. Added `--auto-approve` flag for CI. 2 tests.

#### Tests
- 6 new tests in `shell_tui.rs` for `parse_tail_args`
- 6 new tests in `shell_tui.rs` for markdown styling (`stylize_markdown_line`)
- 5 new tests in `shell_tui.rs` for model extraction/humanization
- 6 new tests in `draft.rs` for `ChangeSetDiffProvider`
- 2 new tests in `release.rs` for auto-approve and TUI interaction

#### Version: `0.10.14-alpha`

---

### v0.10.15 — Deferred Items: Observability & Audit
<!-- status: done -->
**Goal**: Address deferred observability and audit items that strengthen governance before v0.11.

#### Completed
1. [x] **Automatic `agent_id` extraction** (from v0.9.6): `GatewayState::resolve_agent_id()` reads `TA_AGENT_ID` env var, falls back to `dev_session_id`, then "unknown". Used by `audit_tool_call()` on every MCP tool invocation.
2. [x] **`caller_mode` in audit log entries** (from v0.9.6): Added `caller_mode`, `tool_name`, and `goal_run_id` fields to `AuditEvent` with builder methods. All tool-call audit entries include caller mode.
3. [x] **Full tool-call audit logging in gateway** (from v0.9.3): Every `#[tool]` method in `TaGatewayServer` now calls `self.audit()` before delegation. `GatewayState::audit_tool_call()` writes per-call entries with tool name, target URI, goal ID, and caller mode to the JSONL audit log.
4. [x] **Verification integration in auto-approve flow** (from v0.9.8.1): `handle_draft_submit()` now runs `require_tests_pass` and `require_clean_clippy` commands in the staging directory before accepting an auto-approve decision. If either fails, the draft falls through to human review.
5. [x] **Auto-apply flow after auto-approve** (from v0.9.8.1): When `auto_apply: true` in policy.yaml, auto-approved drafts are immediately copied from staging to the source directory. File count and git_commit flag logged.
6. [x] **Event store pruning** (from v0.9.8.1): Added `prune()` method to `EventStore` trait and `FsEventStore`. New `ta events prune --older-than-days N [--dry-run]` CLI command removes daily NDJSON files older than the cutoff date. 2 new tests.
7. [x] **`ta draft apply --require-review` flag** (from v0.9.8.1): Added `--require-review` to CLI `Apply` variant and `require_review` param to gateway `DraftToolParams`. When set, auto-approve evaluation is skipped entirely — draft always routes to ReviewChannel.
8. [x] **Audit trail entry for auto-approved drafts** (from v0.9.8.1): Added `AutoApproval` variant to `AuditAction`. Auto-approved drafts emit a full audit event with `DecisionReasoning` (alternatives, rationale, applied principles) and metadata (draft_id, reasons, auto_apply flag). 3 new tests in ta-audit.

**Tests**: 9 new tests (4 in ta-mcp-gateway server.rs, 3 in ta-audit event.rs, 2 in ta-events store.rs).

#### Version: `0.10.15-alpha`

---

### v0.10.15.1 — TUI Output & Responsiveness Fixes
<!-- status: done -->
**Goal**: Fix two UX regressions in the TUI shell: truncated scrollback for long command output, and missing immediate acknowledgment when long-running commands are dispatched.

#### Items
1. [x] **Full scrollback history**: Changed `scroll_offset` from `u16` to `usize` to prevent overflow at 65,535 visual lines. Increased default `output_buffer_limit` from 10,000 to 50,000 lines.
2. [x] **Immediate command dispatch ack**: Added immediate "Dispatching: ..." info line before async daemon send so users see activity before the daemon responds.

#### Version: `0.10.15-alpha.1`

---

### v0.10.16 — Deferred Items: Platform & Channel Hardening
<!-- status: done -->
**Goal**: Address deferred platform and channel items for production readiness.

#### Completed

**Platform:**
- ✅ **Cross-platform signal handling** (item 2): `tokio::signal` SIGINT + SIGTERM on Unix, Ctrl-C on Windows. Shared `Arc<Notify>` shutdown notifier passed to HTTP server for graceful termination. PID file at `.ta/daemon.pid` with `pid=` and `bind=` fields, cleaned up on shutdown.
- ✅ **Sandbox configuration section** (item 3): `[sandbox]` section in `daemon.toml` with `enabled` and `config_path` fields. `SandboxSection` type with Default derive. Ready for gateway wiring in v0.11+.
- ✅ **Unix domain socket config** (item 4): `socket_path` field on `ServerConfig` (optional, skip_serializing_if None). Config infrastructure for UDS support — actual listener wiring deferred to v0.11.4 (MCP Transport Abstraction).
- ✅ **Auto-start daemon** (item 5): `auto_start_daemon()` in shell.rs finds daemon binary via `version_guard::find_daemon_binary()`, checks PID file for existing instance, spawns background process, waits up to 10s for health. Invoked from `ta shell` when daemon is unreachable.

**Channels:**
- ✅ **Channel access control** (item 12): `ChannelAccessControl` struct with `allowed_users`, `denied_users`, `allowed_roles`, `denied_roles` and `permits(user_id, roles)` method. Deny takes precedence. Added to `ChannelsConfig` (global) and `ExternalChannelEntry` (per-plugin). 6 tests.
- ✅ **Agent tool access control** (item 13): `AgentToolAccess` struct with `allowed_tools`/`denied_tools` and `as_filter()` → `AccessFilter`. Added to `AgentConfig`. 2 tests.
- ✅ **Plugin version checking** (item 14): `min_daemon_version` and `source_url` fields on `PluginManifest`. `ta plugin check` compares installed vs source versions and validates min_daemon_version. `ta plugin upgrade` rebuilds from source. `version_less_than()` semver comparison. 4 tests.

#### Deferred items moved
- MSI installer → backlog (Windows distribution, no target phase)
- Slack Socket Mode + deny modal → v0.11.0 (Event-Driven Agent Routing)
- Discord deny modal + thread discussions → v0.11.0
- Email IMAP reply polling → v0.11.0
- Slack/Discord/Email webhooks → v0.11.0
- Plugin marketplace → backlog (no target phase)

#### Tests: 16 new tests (12 in config.rs, 4 in plugin.rs)
#### Version: `0.10.16-alpha`

---

### v0.10.17 — `ta new` — Conversational Project Bootstrapping
<!-- status: pending -->
**Goal**: Implement the `ta new` CLI command that starts a conversational project bootstrapping session. The infrastructure exists (interactive mode v0.9.9.1, plan generation v0.9.9.3, channel delivery v0.9.9.4, authoring tooling v0.9.9.5) but the parent command and end-to-end flow were never built.

See v0.9.9 design section above for the full architecture and user flow.

#### Items
1. [x] **`ta new` CLI command** (`apps/ta-cli/src/commands/new.rs`): Entry point for conversational project bootstrapping with `run`, `templates`, and `version-schemas` subcommands
2. [x] **Planner agent session**: Interactive session via `ta run --interactive` with bootstrapping prompt, multi-turn Q&A using `ta_ask_human`
3. [x] **Project scaffold generation**: Language-specific scaffolds (Rust CLI/lib, TypeScript API/app, Python CLI/API, Go service, generic) with directory structure, config files, and .gitignore
4. [x] **PLAN.md generation from conversation**: Planner agent produces structured PLAN.md with phases, versions, status markers through interactive prompt
5. [x] **Template integration**: `ta new run --template rust-cli` maps to init templates and generates appropriate scaffold
6. [x] **`ta new run --from <brief.md>`**: Seed from written description, loaded and injected into bootstrapping prompt
7. [x] **Daemon API endpoint** (`POST /api/project/new`): Session-based bootstrapping API with `BootstrapSessionManager` for channel interfaces
8. [x] **Post-creation handoff**: Summary with project path, plan status, and contextual next-step suggestions
9. [x] **`ta plan create --version-schema`** (from v0.9.9.5): Version schema template selection (semver, calver, sprint, milestone) with auto-install to `.ta/version-schema.yaml`

Tests: 25 new tests (name validation, template resolution, scaffold generation, version schema installation, prompt building, session management, post-creation output)

#### Depends on
- v0.10.13 (`ta plan add` — shares planner agent infrastructure)
- v0.9.9.1–v0.9.9.5 (all done — interactive mode, plan generation, channel delivery, authoring tooling)

#### Version: `0.10.17-alpha`

---

### v0.10.17.1 — Shell Reliability & Command Timeout Fixes
<!-- status: done -->
**Goal**: Fix three reliability issues in the TUI shell: auto-tail race condition (still failing despite retries), draft view scrollback not rendering full output, and `draft apply` timing out due to pre-commit verification.

#### Items
1. [x] **Auto-tail client-side prefix resolution**: `resolve_via_active_output()` queries `/api/goals/active-output` and does client-side prefix matching when UUID lookup fails. Eliminates dependency on stderr alias registration timing.
2. [x] **`draft apply` as long-running command**: Added `ta draft apply *` and `draft apply *` to daemon's `long_running` patterns. Streams output in background instead of 120s timeout.
3. [x] **Scrollback pre-slicing** (from v0.10.15.1): Pre-slices logical lines to bypass ratatui's `u16` scroll overflow. Both output pane and agent pane use `residual_scroll` instead of `Paragraph::scroll()`.

#### Version: `0.10.17-alpha.1`

---

### v0.10.18 — Deferred Items: Workflow & Multi-Project
<!-- status: done -->
**Goal**: Address remaining deferred items from workflow engine and multi-project phases.

#### Completed
- [x] **Verify gaps**: Reviewed code to verify incomplete items and best integration points
- [x] **Goal chaining context propagation** (from v0.9.8.2): `context_from: Vec<Uuid>` on GoalRun, gateway resolves prior goal metadata and injects "Prior Goal Context" markdown into new goals
- [x] **Full async process engine I/O** (from v0.9.8.2): `ProcessWorkflowEngine` with long-lived child process, JSON-over-stdio protocol, lazy spawn, graceful shutdown, timeout support, 4 tests
- [x] **Live scoring agent integration** (from v0.9.8.2): `score_verdicts()` with agent-first logic — tries external scorer binary, falls back to built-in numeric averaging. `ScorerConfig` in VerdictConfig
- [x] **Full GatewayState refactor** (from v0.9.10): `ProjectState` struct with per-project isolation (goal store, connectors, packages, events, memory, review channel). `register_project()`, `set_active_project()`, `active_goal_store()` methods. Backward-compatible single-project fallback
- [x] **Thread context tracking** (from v0.9.10): `thread_id: Option<String>` on GoalRun for Discord/Slack/email thread binding
- [x] **Config hot-reload** (from v0.9.10): `ConfigWatcher` using `notify` crate, watches `.ta/daemon.toml` and `.ta/office.yaml`, `ConfigEvent` enum, background thread with mpsc channel, 3 tests
- [x] **Wire `ta sync` and `ta build` as pre-release steps** (from v0.10.6): CI workflow scaffold with graceful degradation when commands unavailable (requires v0.11.1+/v0.11.2+)

#### Version: `0.10.18-alpha`

---

### v0.10.18.1 — Developer Loop: Verification Timing, Notifications & Shell Fixes
<!-- status: done -->
**Goal**: Fix the root cause of PRs shipping with lint/test failures by moving verification to goal completion time. Add desktop notifications and fix shell scrollback rendering.

#### Items
1. [x] **Pre-commit verification at goal completion**: Verification already runs at goal completion (v0.10.8). Enhanced Block mode to show full command output (up to 40 lines with head/tail collapsing) and offer interactive re-entry: "Re-enter the agent to fix these issues? [Y/n]". On confirmation, re-injects failure context into CLAUDE.md and re-launches the agent, then re-verifies. Non-interactive/headless paths print instructions as before.
2. [x] **Desktop notification on draft ready**: Added `notify.rs` module with platform-specific notification support. macOS uses `osascript` (Notification Center), Linux uses `notify-send`. Notifications sent on draft-ready and verification-failure events. Configurable via `[notify]` section in `.ta/workflow.toml` (`enabled`, `title`). Failures are logged but never block the workflow.
3. [x] **Shell scrollback rendering fix**: Verified pre-slicing approach handles >65535 visual lines correctly. Added 2 new tests: `scroll_offset_handles_large_line_count` (70K lines, scroll 60K up/30K down) and `scroll_offset_max_clamp` (scroll past end clamps correctly). The `Paragraph::scroll((residual_scroll, 0))` pattern keeps residual in u16 range.
4. [x] **Verification output detail**: Block mode now shows full command output (first 20 + last 20 lines for long output, with omission indicator). Shows exit code prominently in `--- command (exit code: N) ---` format. Agent mode re-check failure also shows detailed output (20 lines per command). Draft apply verification shows exit code per command and suggests `--skip-verify` flag.

#### Completed
- 4 items completed, 4 new tests across 2 files (notify.rs, shell_tui.rs)
- Version bumped to `0.10.18-alpha.1`

#### Version: `0.10.18-alpha.1`

---

### v0.10.18.2 — Shell TUI: Scrollback & Command Output Visibility
<!-- status: pending -->
**Goal**: Fix the fundamental visibility problem in `ta shell` where command output that exceeds the terminal window height is lost — the user cannot scroll back to see earlier output lines.

#### Problem
When an agent or command produces output longer than the visible terminal area in `ta shell`, lines that scroll past the top of the window are gone. There is no way to scroll up to review them. This makes `ta shell` unusable for any command with substantial output (build logs, test results, long diffs). The user reported this as a recurring blocker.

#### Items
1. [ ] **Scrollback buffer for command output pane**: The shell TUI output widget must retain a scrollback buffer (minimum 10,000 lines, configurable via `[shell] scrollback_lines` in `.ta/shell.toml`). All command output appended to the buffer persists even when it scrolls past the visible area. The widget renders a sliding window over the buffer based on scroll position.
2. [ ] **Keyboard scroll navigation**: Up/Down arrow keys (when not in input mode), PgUp/PgDn, and Home/End scroll through the output buffer. Scroll position indicator shows "line N of M" or a visual scrollbar in the right margin. When new output arrives and the user is scrolled to the bottom, auto-scroll follows new content. When the user has scrolled up, new output does NOT auto-scroll — a "new output ↓" indicator appears instead.
3. [ ] **Test: scrollback preserves and retrieves past output**: Integration test that pushes 500+ lines into the output buffer, verifies the buffer retains all lines, scrolls to line 0, and asserts the first line content matches. Verifies scroll-to-bottom returns to the latest line.
4. [ ] **Test: auto-scroll vs manual scroll behavior**: Test that verifies: (a) when scroll position is at bottom and new content arrives, view follows; (b) when scroll position is NOT at bottom and new content arrives, view stays put and a "new output" flag is set.

#### Version: `0.10.18-alpha.2`

---

### v0.10.18.3 — Verification Streaming, Heartbeat & Configurable Timeout
<!-- status: pending -->
**Goal**: Replace the silent, fire-and-forget verification model with streaming output, explicit progress heartbeats, and per-command configurable timeouts so the user always knows what is happening and never hits an opaque timeout.

#### Problem
`run_single_command()` in `verify.rs` uses synchronous `try_wait()` polling with no output streaming. The user sees nothing until the command finishes or the 600s global timeout fires. `cargo test --workspace` legitimately exceeds 600s on this project, causing every `ta draft apply --git-commit` to fail with an opaque "Command timed out after 600s" error. There is no way to distinguish a hung process from a slow-but-progressing test suite.

#### Items
1. [ ] **Streaming stdout/stderr from verification commands**: `run_single_command()` must capture stdout and stderr as they are produced (not after process exit). Each line is printed to the terminal in real time, prefixed with the command name (e.g., `[cargo test] line content`). Output is also accumulated in the `VerifyResult` for post-run display. Implementation: spawn with `Stdio::piped()`, read lines from `BufReader` on stdout/stderr in a tokio task (or thread for sync context), forward each line to the terminal immediately.
2. [ ] **Heartbeat for TA-internal verification commands**: For commands TA controls (the `./dev` wrapper and any built-in verification), emit a progress heartbeat every 30 seconds: `[cargo test] still running... (90s elapsed, 147 tests passed)`. Parse test runner output where possible to include counts. For external/opaque commands, emit a simpler heartbeat: `[command] still running... (90s elapsed)`. Heartbeat interval configurable via `[verify] heartbeat_interval_secs` in `.ta/workflow.toml` (default: 30).
3. [ ] **Per-command configurable timeout**: Replace the single global `timeout_secs` with per-command timeout support in `.ta/workflow.toml`:
   ```toml
   [verify]
   default_timeout_secs = 300

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
   Each command gets its own timeout. If `timeout_secs` is omitted, `default_timeout_secs` applies. The old flat `timeout_secs` field is supported as a fallback for backward compatibility.
4. [ ] **Timeout message includes elapsed output context**: When a command does time out, the error message includes: (a) the command that timed out, (b) the configured timeout value, (c) the last 20 lines of captured output so the user can see where it stalled, (d) suggestion to increase `timeout_secs` for that specific command in workflow.toml.
5. [ ] **Test: streaming output is captured and forwarded**: Unit test that spawns a child process producing 50+ lines over 2 seconds, verifies each line appears in the accumulated output, and verifies the output is complete after process exit.
6. [ ] **Test: per-command timeout respected**: Test with two commands — one with 2s timeout (sleeps 1s, succeeds) and one with 1s timeout (sleeps 5s, times out). Verify the first passes and the second fails with timeout error containing the last output lines.
7. [ ] **Test: heartbeat emitted for long-running command**: Test that a command running >60s with heartbeat_interval_secs=1 produces at least 2 heartbeat messages in the captured output.

#### Version: `0.10.18-alpha.3`

---

### v0.11.0 — Event-Driven Agent Routing
<!-- status: pending -->
**Goal**: Allow any TA event to trigger an agent workflow instead of (or in addition to) a static response. This is intelligent, adaptive event handling — not scripted hooks or n8n-style flowcharts. An agent receives the event context and decides what to do.

#### Problem
Today TA events have static responses: notify the human, block the next phase, or log to audit. When a build fails, TA tells you it failed. When a draft is denied, TA records the denial. There's no way for the system to *act* on events intelligently — try to fix the build error, re-run a goal with different parameters, escalate only certain kinds of failures.

Users could wire this manually (watch SSE stream → parse events → call `ta run`), but that's fragile scripted automation. TA should support this natively with agent-grade intelligence.

#### Design

**Event responders** are the core primitive. Each responder binds an event type to a response strategy:

```yaml
# .ta/event-routing.yaml
responders:
  - event: build_failed
    strategy: agent
    agent: claude-code
    prompt: |
      A build failed. Diagnose the error from the build output and
      propose a fix. If the fix is trivial (missing import, typo),
      apply it directly. If it requires design decisions, ask the
      human via ta_ask_human.
    escalate_after: 2           # human notified after 2 failed attempts
    max_attempts: 3

  - event: draft_denied
    strategy: notify             # default: just tell the human
    channels: [shell, slack]

  - event: goal_failed
    strategy: agent
    agent: claude-code
    prompt: |
      A goal failed. Review the error log and suggest whether to
      retry with modified parameters, break into smaller goals,
      or escalate to the human.
    require_approval: true       # agent proposes, human approves

  - event: policy_violation
    strategy: block              # always block, never auto-handle
```

**Response strategies:**

| Strategy | Behavior |
|----------|----------|
| `notify` | Deliver event to configured channels (default for most events) |
| `block` | Halt the pipeline, require human intervention |
| `agent` | Launch an agent goal with event context injected as prompt |
| `workflow` | Start a named workflow with event data as input |
| `ignore` | Suppress the event (no notification, no action) |

**TA-managed defaults**: Every event has a sensible default response (mostly `notify`). Users override specific events. TA ships a default `event-routing.yaml` that users can customize per-project.

**Key distinction from scripted hooks**: The agent receives full event context (what failed, the build output, the goal history, the draft diff) and makes intelligent decisions. It can call `ta_ask_human` for interactive clarification. It produces governed output (drafts, not direct changes). This is agent routing, not `if/then/else`.

**Key distinction from n8n/Zapier**: No visual flow builder, no webhook chaining, no action-to-action piping. One event → one agent (or workflow) with full context. The agent handles the complexity, not a workflow graph.

#### Items

1. **`EventRouter`** (`crates/ta-events/src/router.rs`):
   - Loads `event-routing.yaml` config
   - Matches incoming events to responders (exact type match + optional filters)
   - Dispatches to strategy handler (notify, block, agent, workflow, ignore)
   - Tracks attempt counts for `escalate_after` and `max_attempts`

2. **Agent response strategy** (`crates/ta-events/src/strategies/agent.rs`):
   - Launches a goal via `ta run` with event context as prompt
   - Injects event payload (build output, error log, draft diff) into agent context
   - Respects `require_approval` — agent output goes through standard draft review
   - Uses interactive mode (`ta_ask_human`) if agent needs human input
   - Tracks attempts; escalates to human notification after `escalate_after`

3. **Workflow response strategy** (`crates/ta-events/src/strategies/workflow.rs`):
   - Starts a named workflow definition with event data as input variables
   - Workflow stages can reference event fields via template expansion

4. **Default event-routing config** (`templates/event-routing.yaml`):
   - Sensible defaults for all event types
   - Most events: `notify`
   - `policy_violation`: `block`
   - `build_failed`: `notify` (user can upgrade to `agent`)

5. **Event filters** — optional conditions on responders:
   ```yaml
   - event: build_failed
     filter:
       severity: critical        # only on critical failures
       phase: "v0.9.*"          # only for certain phases
     strategy: agent
   ```

6. **`ta events routing`** CLI command:
   - `ta events routing list` — show active responders
   - `ta events routing test <event-type>` — dry-run: show what would happen
   - `ta events routing set <event-type> <strategy>` — quick override

7. **Guardrails**:
   - Agent-routed events are governed goals — full staging, policy, audit
   - `max_attempts` prevents infinite loops (agent fails → event → agent fails → ...)
   - `escalate_after` ensures humans see persistent failures
   - `policy_violation` and `sandbox_escape` events cannot be routed to `ignore`

#### Scope boundary
Event routing handles *reactive* responses to things that already happened. It does not handle *proactive* scheduling (cron, triggers) — that belongs in the Virtual Office Runtime project on top.

#### Version: `0.11.0-alpha`

---

### v0.11.1 — `SourceAdapter` Unification & `ta sync`
<!-- status: pending -->
**Goal**: Merge the current `SubmitAdapter` trait with sync operations into a unified `SourceAdapter` trait. Add `ta sync` command. The trait defines abstract VCS operations; provider-specific mechanics (rebase, fast-forward, shelving) live in each implementation.

See `docs/MISSION-AND-SCOPE.md` for the full `SourceAdapter` trait design and per-provider operation mapping.

#### Items

1. **`SourceAdapter` trait** (`crates/ta-submit/src/adapter.rs`):
   - Rename `SubmitAdapter` → `SourceAdapter`
   - Add `sync_upstream(&self) -> Result<SyncResult>` abstract method
   - `SyncResult`: `{ updated: bool, conflicts: Vec<String>, new_commits: u32 }`
   - Provider-specific config namespaced under `[source.git]`, `[source.perforce]`, etc.

2. **Git implementation** (`crates/ta-submit/src/git.rs`):
   - `sync_upstream`: `git fetch origin` + merge/rebase/ff per `source.git.sync_strategy`
   - Conflict detection: parse merge output, return structured `SyncResult`

3. **`ta sync` CLI command** (`apps/ta-cli/src/commands/sync.rs`):
   - Calls `SourceAdapter::sync_upstream()`
   - Emits `sync_completed` or `sync_conflict` event
   - If staging is active, warns or auto-rebases per config

4. **`ta shell` integration**:
   - `ta> sync` as shell shortcut
   - SSE event for sync results

5. **Wire into `ta draft apply`**:
   - Optional `auto_sync = true` in `[source.sync]` config
   - After apply + commit + push, auto-sync main if configured

#### Version: `0.11.1-alpha`

---

### v0.11.2 — `BuildAdapter` & `ta build`
<!-- status: pending -->
**Goal**: Add `ta build` as a governed event wrapper around project build tools. The build result flows through TA's event system so workflows, channels, event-routing agents, and audit logs all see it.

See `docs/MISSION-AND-SCOPE.md` for the full design.

#### Items

1. **`BuildAdapter` trait** (`crates/ta-build/src/adapter.rs` — new crate):
   - `fn build(&self) -> Result<BuildResult>`
   - `fn test(&self) -> Result<BuildResult>`
   - `BuildResult`: `{ success: bool, exit_code: i32, stdout: String, stderr: String, duration: Duration }`
   - Auto-detection from framework registry (cargo, npm, make, etc.)

2. **Built-in adapters**:
   - `CargoAdapter`: `cargo build --workspace`, `cargo test --workspace`
   - `NpmAdapter`: `npm run build`, `npm test`
   - `ScriptAdapter`: user-defined command from config
   - `WebhookAdapter`: POST to external CI, poll for result

3. **`ta build` CLI command** (`apps/ta-cli/src/commands/build.rs`):
   - Calls `BuildAdapter::build()` (and optionally `test()`)
   - Emits `build_completed` or `build_failed` event with full output
   - Exit code reflects build result

4. **Config** (`.ta/workflow.toml`):
   ```toml
   [build]
   adapter = "cargo"                      # or "npm", "script", "webhook", auto-detected
   command = "cargo build --workspace"    # override for script adapter
   test_command = "cargo test --workspace"
   on_fail = "notify"                     # notify | block_release | block_next_phase | agent
   ```

5. **Wire into `ta release run`**:
   - Optional `pre_steps = ["sync", "build", "test"]` in `[release]` config
   - Release blocked if build/test fails

6. **`ta shell` integration**:
   - `ta> build` and `ta> test` as shell shortcuts

#### Version: `0.11.2-alpha`

---

### v0.11.3 — Reflink/COW Overlay Optimization
<!-- status: pending -->
**Goal**: Replace full-copy staging with copy-on-write to eliminate filesystem bloat. Detect APFS/Btrfs and use native reflinks; fall back to FUSE overlay on unsupported filesystems.

#### Items

1. [ ] Detect filesystem type (APFS, Btrfs, ext4, etc.) at staging creation time
2. [ ] APFS: use `cp -c` (reflink) for instant zero-cost copies on macOS
3. [ ] Btrfs: use `cp --reflink=always` on Linux
4. [ ] Fallback: full copy on unsupported filesystems (current behavior)
5. [ ] Optional FUSE overlay for cross-platform COW on filesystems without reflink support
6. [ ] Benchmark: measure staging creation time and disk usage before/after
7. [ ] Update OverlayWorkspace to detect and select strategy automatically

#### Version: `0.11.3-alpha`

---

### v0.11.4 — MCP Transport Abstraction (TCP/Unix Socket)
<!-- status: pending -->
**Goal**: Abstract MCP transport so agents can communicate with TA over TCP or Unix sockets, not just stdio pipes. Critical enabler for container-based isolation (SecureTA) and remote agent execution.

#### Items

1. [ ] `TransportLayer` trait: `Stdio`, `UnixSocket`, `Tcp` variants
2. [ ] TCP transport: MCP server listens on configurable port, agent connects over network
3. [ ] Unix socket transport: MCP server creates socket file, agent connects locally (faster than TCP, works across container boundaries via mount)
4. [ ] Transport selection in agent config: `transport = "stdio" | "unix" | "tcp"`
5. [ ] TLS support for TCP transport (optional, for remote agents)
6. [ ] Connection authentication: bearer token exchange on connect
7. [ ] Update `ta run` to configure transport based on runtime adapter

#### Version: `0.11.4-alpha`

---

### v0.11.5 — Runtime Adapter Trait
<!-- status: pending -->
**Goal**: Abstract how TA spawns and manages agent processes. Today it's hardcoded as a bare child process. A `RuntimeAdapter` trait enables container, VM, and remote execution backends — TA provides BareProcess, SecureTA provides OCI/VM.

#### Items

1. [ ] `RuntimeAdapter` trait with `spawn()`, `stop()`, `status()`, `attach_transport()` methods
2. [ ] `BareProcessRuntime`: extract current process spawning into this adapter (no behavior change)
3. [ ] Runtime selection in agent/workflow config: `runtime = "process" | "oci" | "vm"`
4. [ ] Plugin-based runtime loading: SecureTA registers OCI/VM runtimes as plugins
5. [ ] Runtime lifecycle events: `AgentSpawned`, `AgentExited`, `RuntimeError` fed into event system
6. [ ] Credential injection API: `RuntimeAdapter::inject_credentials()` for scoped secret injection into runtime environment

#### Version: `0.11.5-alpha`

---

### v0.11.6 — External Action Governance Framework
<!-- status: pending -->
**Goal**: Provide the governance framework for agents performing external actions — sending emails, posting on social media, making API calls, executing financial transactions. TA doesn't implement the actions; it provides the policy, approval, capture, and audit layer so projects like SecureTA or custom workflows can govern them.

**Design**:
- `ExternalAction` trait: defines an action type (email, social post, API call, DB query) with metadata schema
- `ActionPolicy`: per-action-type rules — auto-approve, require human approval, block, rate-limit
- `ActionCapture`: every attempted external action is logged with full payload before execution
- `ActionReview`: captured actions go through the same draft review flow (approve/deny/modify before send)
- Plugins register action types; TA provides the governance pipeline

#### Items

1. [ ] `ExternalAction` trait: `action_type()`, `payload_schema()`, `validate()`, `execute()` — plugins implement this
2. [ ] `ActionPolicy` config in `.ta/workflow.toml`: per-action-type rules (auto, review, block, rate-limit)
3. [ ] `ActionCapture` log: every attempted action logged with full payload, timestamp, goal context
4. [ ] Review flow integration: captured actions surface in `ta draft view` as "pending external actions" alongside file changes
5. [ ] MCP tool `ta_external_action`: agent calls this to request an external action; TA applies policy before execution
6. [ ] Rate limiting: configurable per-action-type limits (e.g., max 5 emails per goal, max 1 social post per hour)
7. [ ] Dry-run mode: capture and log actions without executing, for testing workflows
8. [ ] Built-in action type stubs: `email`, `social_post`, `api_call`, `db_query` — schema only, no implementation (plugins provide the actual send/post/call logic)

**Config example**:
```toml
[actions.email]
policy = "review"          # require human approval before sending
rate_limit = 10            # max 10 per goal

[actions.social_post]
policy = "review"
rate_limit = 1

[actions.api_call]
policy = "auto"            # auto-approve known API calls
allowed_domains = ["api.stripe.com", "api.github.com"]

[actions.db_query]
policy = "review"          # review all DB mutations
auto_approve_reads = true  # SELECT is fine, INSERT/UPDATE/DELETE needs review
```

#### Version: `0.11.6-alpha`

---

### v0.11.7 — Database Proxy Plugins
<!-- status: pending -->
**Goal**: Plugin-based database proxies that intercept agent DB operations. The agent connects to a local proxy thinking it's a real database; TA captures every query, enforces read/write policies, and logs mutations for review. Plugins provide wire protocol implementations; TA provides the governance framework (v0.11.6).

#### Items

1. [ ] `DbProxyPlugin` trait extending `ExternalAction`: `wire_protocol()`, `parse_query()`, `classify_mutation()`, `proxy_port()`
2. [ ] Proxy lifecycle: TA starts proxy before agent, stops after agent exits
3. [ ] Query classification: READ vs WRITE vs DDL vs ADMIN — policy applied per class
4. [ ] Mutation capture: all write operations logged with full query + parameters in draft audit trail
5. [ ] Replay support: captured mutations can replay against real DB on `ta draft apply`
6. [ ] Reference plugin: `ta-db-proxy-sqlite` — SQLite VFS shim, simplest implementation
7. [ ] Reference plugin: `ta-db-proxy-postgres` — Postgres wire protocol proxy
8. [ ] Future plugins (community): MySQL, MongoDB, Redis

#### Version: `0.11.7-alpha`

---

---

## Projects On Top (separate repos, built on TA)

> These are NOT part of TA core. They are independent projects that consume TA's extension points.
> See `docs/ADR-product-concept-model.md` for how they integrate.

### TA Web UI *(separate project)*
> Lightweight web frontend for non-engineers to use TA without the CLI.

A browser-based interface to TA's daemon API, aimed at users who need to start goals, review drafts, and respond to agent questions without touching a terminal. Same capabilities as `ta shell` but with a guided, form-based experience.

- **Thin client**: SPA consuming TA's existing HTTP API + SSE events. No new backend logic.
- **Non-engineer language**: "Review changes", "Approve", "Ask the agent a question" — not "draft", "artifact", "overlay".
- **Dashboard**: Active goals, pending reviews, pending agent questions. One-glance status.
- **Start Goal**: Form with title, description, agent dropdown, optional file upload. Sensible defaults, optional advanced toggle.
- **Goal Detail**: Live agent output via SSE, state transitions, conversation history (interactive mode Q&A).
- **Draft Review**: Side-by-side diff viewer, file tree, AI summary. Approve/deny/comment buttons. Selective approval per file.
- **Agent Questions**: Pending questions with response input. Browser push notifications.
- **History**: Past goals/drafts, searchable, filterable.
- **Tech stack**: React or Svelte SPA, served as static files by daemon (`GET /ui/*`). Auth via daemon API token or session login.
- **Extensible**: Plugin mount points at `/ui/ext/<plugin-name>` for custom pages. Configurable theme/branding via `daemon.toml`.
- **Mobile-friendly**: Responsive layout for on-the-go approvals from phone/tablet.

**TA dependencies**: Daemon HTTP API (exists), SSE events (exists), interactive mode (v0.9.9.x), static file serving from daemon (minor addition to `ta-daemon`).

### Virtual Office Runtime *(separate project)*
> Thin orchestration layer that composes TA, agent frameworks, and MCP servers.

- Role definition schema (YAML): purpose, triggers, agent, capabilities, notification channel
- Trigger system: cron scheduler + webhook receiver + TA event listener
- Office manager daemon: reads role configs, routes triggers, calls `ta run`
- Multi-agent workflow design with detailed agent guidance
- Smart security plan generation → produces `AlignmentProfile` + `AccessConstitution` YAML consumed by TA
- Constitutional auto-approval active by default
- **Compliance dashboard**: ISO/IEC 42001, EU AI Act evidence package
- Domain workflow templates (sw-engineer, email, finance, etc.)

### Autonomous Infra Ops *(separate project)*
> Builder intent → best-practice IaC, self-healing with observability.

- Builder intent language → IaC generation (Terraform, Pulumi, CDK)
- TA mediates all infrastructure changes (ResourceMediator for cloud APIs)
- Self-healing loop: observability alerts → agent proposes fix → TA reviews → apply
- Best-practice templates for common infrastructure patterns
- Cost-aware: TA budget limits enforce infrastructure spend caps

---

## Supervision Frequency: TA vs Standard Agent Usage

> How often does a user interact with TA compared to running Claude/Codex directly?

| Mode | Standard Claude/Codex | TA-mediated |
|------|----------------------|-------------|
| **Active coding** | Continuous back-and-forth. ~100% attention. | Fluid session: agent works, human reviews in real-time. ~10-20% attention. |
| **Overnight/batch** | Not possible — agent exits when session closes. | `ta run --checkpoint` in background. Review next morning. 0% attention during execution. |
| **Auto-approved (v0.6)** | N/A | Supervisor handles review within constitutional bounds. User sees daily summary. ~1% attention. Escalations interrupt. |
| **Virtual office** | N/A | Roles run on triggers. User reviews when notified. Minutes per day for routine workflows. |

**Key shift**: Standard agent usage demands synchronous human attention. TA shifts to fluid, asynchronous review — the agent works independently, the human reviews in real-time or retroactively. Trust increases over time as constitutional auto-approval proves reliable.

---

## Future Improvements (unscheduled)

> Ideas that are valuable but not yet prioritized into a release phase. Pull into a versioned phase when ready.

### OCI/gVisor Container Isolation (from v0.9.2)
Enterprise-grade sandbox using OCI containers with gVisor for kernel-level agent isolation. The `ta-sandbox` crate provides command allowlists and CWD enforcement; OCI adds true process isolation with network/filesystem namespace separation.

### Enterprise State Intercept (from v0.9.2)
See `docs/enterprise-state-intercept.md`. Allows enterprises to intercept and audit all agent state transitions for compliance.

### External Plugin System
Process-based plugin architecture so third parties can publish TA adapters as independent packages. A Perforce vendor, JIRA integration company, or custom VCS provider can ship a `ta-submit-<name>` executable that TA discovers and communicates with via JSON-over-stdio protocol. Extends beyond VCS to any adapter type: notification channels (`ta-channel-slack`), storage backends (`ta-store-postgres`), output integrations (`ta-output-jira`). Includes `ta plugin install/list/remove` commands, a plugin manifest format, and a plugin registry (crates.io or TA-hosted). Design sketched in v0.9.8.4; implementation deferred until the in-process adapter pattern is validated.

### Community Memory Sync
Federated sharing of anonymized problem→solution pairs across TA instances. Builds on v0.8.1 (Solution Memory Export) with:
- **Community sync layer**: Publish anonymized entries to a shared registry (hosted service or federated protocol).
- **Privacy controls**: Tag-based opt-in, never auto-publish. PII stripping before publish. User reviews every entry before it leaves the local machine.
- **Retrieval**: `ta context recall` searches local first, then community if opted in.
- **Provenance tracking**: Did this solution actually work when applied downstream? Feedback loop from consumers back to publishers.
- **Trust model**: Reputation scoring for contributors. Verified solutions (applied successfully N times) ranked higher.
- **Spam/quality**: Moderation queue for new contributors. Automated quality checks (is the problem statement clear? is the solution actionable?).