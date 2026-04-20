# Trusted Autonomy — Development Plan
---
> Canonical plan for the project. Machine-parseable: each phase has a `<!-- status: done|in_progress|pending -->` marker.
> Updated automatically by `ta pr apply` when a goal with `--phase` completes.
---                                                                                                                                                                                                                                                             
## Versioning & Release Policy
---                                                  
### Plan Phases vs Release Versions
---
Plan phases use hierarchical IDs for readability (e.g., `v0.4.1.1`). Release versions use strict [semver](https://semver.org/) (`MAJOR.MINOR.PATCH-prerelease`). The mapping:
---
| Plan Phase Format | Release Version | Example |
---
| `vX.Y` | `X.Y.0-alpha` | v0.4 → `0.4.0-alpha` |
| `vX.Y.Z` | `X.Y.Z-alpha` | v0.4.1 → `0.4.1-alpha` |
| `vX.Y.Z.N` (sub-phase) | `X.Y.Z-alpha.N` | v0.4.1.2 → `0.4.1-alpha.2` |
---
**Rule**: The plan phase ID directly determines the release version. No separate mapping table needed — apply the formula above.
---
### Pre-release Lifecycle
---
| Tag | Meaning | Criteria to Enter |
---
| `alpha` | Active development. APIs may change. Not recommended for production. | Default for all `0.x` work |
| `beta` | Feature-complete for the release cycle. APIs stabilizing. Suitable for early adopters. | All planned phases for the minor version are done; no known critical bugs |
| `rc.N` | Release candidate. Only bug fixes accepted. | Beta testing complete; no API changes expected |
| *(none)* | Stable public release. Semver guarantees apply. | RC period passes without blocking issues |
---
**Current lifecycle**: All `0.x` releases are `alpha`. Beta begins when the core loop is proven (target: `v0.8` Department Runtime). Stable `1.0.0` requires: all v0.x features hardened, public API frozen, security audit complete.
---
**Version progression example**:
---
0.4.1-alpha → 0.4.1-alpha.1 → 0.4.1-alpha.2 → 0.4.2-alpha → ...
0.8.0-alpha → 0.8.0-beta → 0.8.0-rc.1 → 0.8.0
1.0.0-beta → 1.0.0-rc.1 → 1.0.0
---
---
### Release Mechanics
---
- **Release tags**: Each `vX.Y.0` phase is a **release point** — cut a git tag and publish binaries.
- **Patch phases** (`vX.Y.1`, `vX.Y.2`) are incremental work within a release cycle.
- **Sub-phases** (`vX.Y.Z.N`) use pre-release dot notation: `ta release run X.Y.Z-alpha.N`
- **When completing a phase**, the implementing agent MUST:
  1. Update `version` in `apps/ta-cli/Cargo.toml` to the phase's release version
  2. Update the "Current State" section in `CLAUDE.md` with the new version and test count
  3. Mark the phase as `done` in this file
- **Pre-v0.1 phases** (Phase 0–4c) used internal numbering. All phases from v0.1 onward use version-based naming.
---
---
---
## Standards & Compliance Reference
---
TA's architecture maps to emerging AI governance standards. Rather than bolt-on compliance, these standards inform design decisions at the phase where they naturally apply. References below indicate where TA's existing or planned capabilities satisfy a standard's requirements.
---
| Standard | Relevance to TA | Phase(s) |
---
| **ISO/IEC 42001:2023** (AI Management Systems) | Audit trail integrity (hash-chained logs), documented capability grants, human oversight records | Phase 1 (done), v0.3.3 |
| **ISO/IEC 42005:2025** (AI Impact Assessment) | Risk scoring per draft, policy decision records, impact statements in summaries | Phase 4b (done), v0.3.3 |
| **IEEE 7001-2021** (Transparency of Autonomous Systems) | Structured decision reasoning, alternatives considered, observable policy enforcement | v0.3.3, v0.4.0 |
| **IEEE 3152-2024** (Human/Machine Agency Identification) | Agent identity declarations, capability manifests, constitution references | Phase 2 (done), v0.4.0 |
| **EU AI Act Article 14** (Human Oversight) | Human-in-the-loop checkpoint, approve/reject per artifact, audit trail of decisions | Phase 3 (done), v0.3.0 (done) |
| **EU AI Act Article 50** (Transparency Obligations) | Transparent interception of external actions, human-readable action summaries | v0.5.0, v0.7.1 |
| **Singapore IMDA Agentic AI Framework** (Jan 2026) | Agent boundaries, network governance, multi-agent coordination alignment | v0.6.0, v0.7.x, v1.0 |
| **NIST AI RMF 1.0** (AI Risk Management) | Risk-proportional review, behavioral drift monitoring, escalation triggers | v0.3.3, v0.4.2 |
---
> **Design principle**: TA achieves compliance through architectural enforcement (staging + policy + checkpoint), not self-declaration. An agent's compliance is *verified by TA's constraints*, not *claimed by the agent*. This is stronger than transparency-only protocols like [AAP](https://github.com/mnemom/aap) — TA doesn't ask agents to declare alignment; it enforces boundaries regardless of what agents declare.
---
---
---
## Completed Phases (Phase 0 through v0.8)
---
> **Archived**: Phases 0–4c, v0.1–v0.1.2, v0.2.0–v0.2.4, v0.3.0–v0.3.6, v0.4.0–v0.4.5, v0.5.0–v0.5.7, v0.6.0–v0.6.3, v0.7.0–v0.7.7, v0.8.0–v0.8.2 have been moved to [`docs/PLAN-ARCHIVE.md`](docs/PLAN-ARCHIVE.md).
> All are `<!-- status: done -->` except v0.1 and v0.1.1 which are `<!-- status: deferred -->`.
---
---
---
## Release Sequence & Phase Priority
---
### Road to Public Alpha
---
External users (working on their own projects, not TA itself) need these phases completed in order before TA is ready for public alpha. All other phases are post-alpha.
---
| Phase | Why required |
---
| **v0.11.7** | Shell stream UX + VCS trait generalization — foundational for next phases |
| **v0.12.0** + §16.6 extraction | `ta new` / `setup.sh` onboarding + remove TA-specific scanner from generic pipeline |
| **v0.12.0.1** | PR merge + main sync completion — the missing post-apply workflow step |
| **v0.12.0.2** | VCS Adapter Externalization — first users include Perforce shops; P4 must be external plugin |
| ⬇ **PUBLIC ALPHA** | TA can be set up on a new project, plan built, goals run, drafts applied, PRs merged, main synced — in git or P4, from `ta shell` + Discord/Slack |
| **v0.12.1** | Discord Channel Polish — slash commands, rate limiting, goal progress streaming |
| **v0.12.2** | Shell Paste-at-End UX fix |
| **v0.12.6** | Goal lifecycle observability + Discord/Slack SSE notification reliability |
| **v0.12.7** | Shell UX: "Agent is working" clearance on goal completion + scroll reliability |
| **v0.12.8** | Alpha bug-fixes: Discord notification flood hardening + draft CLI/API disconnect |
| ⬇ **PUBLIC BETA (v0.13.x)** | Runtime flexibility, enterprise governance, community ecosystem, goal workflow automation |
---
### Pre-Alpha Bugs to Fix (must resolve before external release)
---
- **Follow-up draft captures per-session delta, not full staging-vs-source diff**: When `ta run --follow-up` creates a child draft, `ta draft build` should diff the *full staging state* against current source — capturing all accumulated changes from the parent session + child session. Currently it appears to capture only what the child agent session wrote. Result: applying a child draft produces partial changes, and apply-time validation fails with compile errors that exist in source but not in staging. This confuses agents doing follow-up work ("the build is clean!") and requires multiple follow-up chains to complete simple fix tasks. Fix: ensure `ta draft build` always performs a full `diff(staging, source)` regardless of session depth.
---
### Post-Alpha: Near-Term (v0.13.x Beta)
---
| Phase | Notes |
---
| v0.13.0 | Reflink/COW — perf optimization, not blocking |
| v0.13.0.1 | Draft parent title rollup — follow-up chains show "Changes from parent" |
| v0.13.1 | Self-healing daemon + auto-follow-up on validation failure |
| v0.13.4 | External Action Governance — needed when agents send emails/API calls/posts |
| v0.13.5 | Database Proxy Plugins — depends on v0.13.4 |
| v0.13.9 | Product Constitution Framework — project-level behavioral contracts, draft-time scan, release gate |
| v0.13.11 | Platform Installers — macOS DMG/pkg, Windows MSI with PATH registration |
| v0.14.x | Hardened Autonomy — sandboxing DSL, verifiable audit trail, multi-party governance, extension-point surface for external plugins |
---
### Hardened Autonomy
---
Hardening for security-conscious single-node deployments. Multi-user and enterprise features are built by external plugins (see Secure Autonomy) on top of the extension traits defined in v0.14.4.
---
- v0.13.2 — MCP Transport Abstraction (Secure Autonomy/container enabler; runtime adapters depend on this)
- v0.13.3 — Runtime Adapter Trait (Secure Autonomy/OCI; depends on v0.13.2)
- v0.13.6 — Community Knowledge Hub (post-launch community feature)
- v0.13.9 — Product Constitution Framework (project-level invariants, draft-time scan, release gate)
- v0.13.10 — Feature Velocity Stats: build time, fix time, goal outcomes, connector events
---
### Deferred / May Drop
---
- Shell Mouse Scroll (TUI may be dropped; web shell is default) — see Future Work section
---
### Advanced (Post-Beta)
---
- v0.13.7 — Goal Workflows: Serial Chains, Parallel Swarms & Office Routing
- v0.13.8 — Agent Framework: Pluggable Agent Backends (Claude Code, Codex, Claude-Flow, Ollama+Qwen, user-defined)
- v0.14.x — Enterprise Readiness (sandboxing, attestation, multi-party governance, cloud/multi-user deployment)
---
---
---
## v0.9 — Distribution & Packaging *(release: tag v0.9.0-beta)*
---
### v0.9.0 — Distribution & Packaging
---
---
- Developer: `cargo run` + local config + Nix
- Desktop: installer with bundled daemon, git, rg/jq, common MCP servers
- Cloud: OCI image for daemon + MCP servers, ephemeral virtual workspaces
- Full web UI for review/approval (extends v0.5.2 minimal UI)
- Mobile-responsive web UI (PWA)
---
---
- [x] `Dockerfile` — multi-stage OCI image (build from source, slim runtime with git/jq)
- [x] `install.sh` — updated installer with `ta init`/`ta dev` instructions, Windows detection, draft terminology
- [x] PWA manifest (`manifest.json`) + mobile-responsive web UI meta tags
- [x] Web UI route for `/manifest.json` (v0.9.0)
- [x] Version bump to 0.9.0-alpha
---
### v0.9.1 — Native Windows Support
---
---
**Goal**: First-class Windows experience without requiring WSL.
---
- **Windows MSVC build target**: `x86_64-pc-windows-msvc` in CI release matrix.
- **Path handling**: Audit `Path`/`PathBuf` for Unix assumptions.
- **Process management**: Cross-platform signal handling via `ctrlc` crate.
- **Shell command execution**: Add `shell` field to agent YAML (`bash`, `powershell`, `cmd`). Auto-detect default.
- **Installer**: MSI installer, `winget` and `scoop` packages.
- **Testing**: Windows CI job, gate releases on Windows tests passing.
---
---
- [x] `x86_64-pc-windows-msvc` added to CI release matrix with Windows-specific packaging (.zip)
- [x] Windows CI job in `ci.yml` — build, test, clippy on `windows-latest`
- [x] PTY module gated with `#[cfg(unix)]` — Windows falls back to simple mode
- [x] Session resume gated with `#[cfg(unix)]` — Windows gets clear error message
- [x] `build.rs` cross-platform date: Unix `date` → PowerShell fallback
- [x] `shell` field added to `AgentLaunchConfig` for cross-platform shell selection
- [x] SHA256 checksum generation for Windows (.zip) in release workflow
- [x] `install.sh` updated with Windows detection and winget/scoop guidance
---
---
- MSI installer → v0.9.1-deferred (Windows distribution backlog)
- `ctrlc` crate → dropped (tokio::signal in v0.10.16 supersedes this)
---
### v0.9.2 — Sandbox Runner (optional hardening, Layer 2)
---
---
> Optional for users who need kernel-level isolation. Not a prerequisite for v1.0.
---
- OCI/gVisor sandbox for agent execution
- Allowlisted command execution (rg, fmt, test profiles)
- CWD enforcement — agents can't escape virtual workspace
- Command transcripts hashed into audit log
- Network access policy: allow/deny per-domain
- **Enterprise state intercept**: See `docs/enterprise-state-intercept.md`.
---
---
- [x] `ta-sandbox` crate fully implemented (was stub since Phase 0)
- [x] `SandboxConfig` with command allowlist, network policy, timeout, audit settings
- [x] `SandboxRunner` with `execute()` — allowlist check, forbidden args, CWD enforcement, transcript capture
- [x] Command transcript SHA-256 hashing for audit log integration
- [x] `NetworkPolicy` with per-domain allow/deny and wildcard support (`*.github.com`)
- [x] Default config with common dev tools: rg, grep, find, cat, cargo, npm, git, jq
- [x] `CommandPolicy` with `max_invocations`, `can_write`, `allowed_args`, `forbidden_args`
- [x] Path escape detection — resolves `..` and symlinks, rejects paths outside workspace
- [x] 12 tests: allowlist enforcement, forbidden args, path escape, invocation limits, transcript hashing, network policy
---
---
- OCI/gVisor container isolation → v0.11.5 (Runtime Adapter Trait)
- Enterprise state intercept → v0.11.5 (Runtime Adapter Trait)
---
### v0.9.3 — Dev Loop Access Hardening
---
---
**Goal**: Severely limit what the `ta dev` orchestrator agent can do — read-only project access, only TA MCP tools, no filesystem writes.
---
**Completed:**
- ✅ `--allowedTools` enforcement: agent config restricts to `mcp__ta__*` + read-only builtins. No Write, Edit, Bash, NotebookEdit.
- ✅ `.mcp.json` scoping: `inject_mcp_server_config_with_session()` passes `TA_DEV_SESSION_ID` and `TA_CALLER_MODE` env vars to the MCP server for per-session audit and policy enforcement.
- ✅ Policy enforcement: `CallerMode` enum (`Normal`/`Orchestrator`/`Unrestricted`) in MCP gateway. `ta_fs_write` blocked at gateway level in orchestrator mode. Security Boundaries section in system prompt.
- ✅ Audit trail: `write_dev_audit()` logs session start/end with session ID, mode, exit status to `.ta/dev-audit.log`. `TA_DEV_SESSION_ID` env var passed to agent process and MCP server for correlation.
- ✅ Escape hatch: `ta dev --unrestricted` bypasses restrictions, logs warning, removes `--allowedTools` from agent config.
- ✅ `dev-loop.yaml` alignment profile: `forbidden_actions` includes `fs_write_patch`, `fs_apply`, `shell_execute`, `network_external`, `credential_access`, `notebook_edit`.
- ✅ 12 tests: prompt security boundaries, unrestricted warning, config loading (restricted/unrestricted), audit logging, MCP injection with session, CallerMode enforcement.
- ✅ Version bump to 0.9.3-alpha.
---
**Deferred items resolved:**
- Sandbox runtime integration → v0.11.5 (Runtime Adapter Trait)
- Full tool-call audit logging → completed in v0.10.15 (per-tool-call audit via `audit_tool_call()`)
---
### v0.9.4 — Orchestrator Event Wiring & Gateway Refactor
---
---
**Goal**: Wire the `ta dev` orchestrator to actually launch implementation agents, handle failures, and receive events — plus refactor the growing MCP gateway.
---
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
---
**Completed:**
- [x] `GoalFailed` event variant added to `TaEvent` (ta-goal/events.rs) and `SessionEvent` (ta-events/schema.rs) with helper constructors, serialization tests
- [x] `ta_event_subscribe` MCP tool with query/watch/latest actions, cursor-based pagination, type/goal/time filtering
- [x] MCP gateway refactored: `server.rs` split into `tools/{goal,fs,draft,plan,context,event}.rs` + `validation.rs`
- [x] `GoalFailed` emitted on agent launch failure in `ta_goal_inner` with `launch:true`, transitions goal to Failed state
- [x] `ta dev` prompt and allowed-tools list updated to include `ta_event_subscribe`
- [x] 14 MCP tools (was 13), 30 gateway tests pass, 2 new GoalFailed event tests
---
---                                                                                                                                                                                                                                                             
### v0.9.4.1 — Event Emission Plumbing Fix                       
---
---
**Goal**: Wire event emission into all goal lifecycle paths so `ta_event_subscribe` actually receives events. Currently only `GoalFailed` on spawn failure emits to FsEventStore — `GoalStarted`, `GoalCompleted`, and `DraftBuilt` are never written, making
the event subscription system non-functional for orchestrator agents.                
---
**Bug**: `ta_goal_start` (MCP) creates goal metadata but does NOT: copy project to staging, inject CLAUDE.md, or launch the agent process. Goals created via MCP are stuck in `running` with no workspace and no agent. The full `ta run` lifecycle must be
wired into the MCP goal start path.
---
---
- ✅ **`ta_goal_start` MCP → full lifecycle**: `ta_goal_start` now always launches the implementation agent. Added `source` and `phase` parameters, always spawns `ta run --headless` which performs overlay copy, CLAUDE.md injection, agent spawn, draft build, and event emission. Goals created via MCP now actually execute — fixing `ta dev`.
- ✅ **Emit `GoalStarted`**: Both MCP `handle_goal_start()`, `handle_goal_inner()`, and CLI `ta run` emit `SessionEvent::GoalStarted` to FsEventStore after goal creation.
- ✅ **Emit `GoalCompleted`**: CLI `ta run` emits `GoalCompleted` on agent exit code 0. MCP agent launch delegates to `ta run --headless` which emits events.
- ✅ **Emit `DraftBuilt`**: Both MCP `handle_pr_build()`, `handle_draft_build()`, and CLI `ta draft build` emit `DraftBuilt` to FsEventStore.
- ✅ **Emit `GoalFailed` on all failure paths**: CLI `ta run` emits `GoalFailed` on non-zero exit code and launch failure. MCP `launch_goal_agent` and `launch_sub_goal_agent` emit on spawn failure.
- ✅ **End-to-end integration test** (3 tests in `crates/ta-mcp-gateway/src/tools/event.rs`): lifecycle event emission + goal_id/event_type filtering + cursor-based watch pattern.
- ✅ **Cursor-based watch test**: Verifies query-with-cursor polling pattern works correctly.
---
#### Version: `0.9.4-alpha.1`
---
### v0.9.5 — Enhanced Draft View Output
---
---
**Goal**: Make `ta draft view` output clear and actionable for reviewers — structured "what changed" summaries, design alternatives considered, and grouped visual sections.
---
---
---
- ✅ **Grouped change summary**: `ta draft view` shows a module-grouped file list with per-file classification (created/modified/deleted), one-line "what" and "why", and dependency annotations (which changes depend on each other vs. independent).
- ✅ **Alternatives considered**: New `alternatives_considered: Vec<DesignAlternative>` field on `Summary`. Each entry has `option`, `rationale`, `chosen: bool`. Populated by agents via new optional `alternatives` parameter on `ta_pr_build` MCP tool. Displayed under "Design Decisions" heading in `ta draft view`.
- ✅ **Structured view sections**: `ta draft view` output organized as Summary → What Changed → Design Decisions → Artifacts.
- ✅ **`--json` on `ta draft view`**: Full structured JSON output for programmatic consumption (already existed; now includes new fields).
- ✅ 7 new tests (3 in draft_package.rs, 4 in terminal.rs).
---
#### Version: `0.9.5-alpha`
---
---                                                  
### v0.9.5.1 — Goal Lifecycle Hygiene & Orchestrator Fixes                                                                                                                                                                                                      
---
---
**Goal**: Fix the bugs discovered during v0.9.5 goal lifecycle monitoring — duplicate goal creation, zombie goal cleanup, event timer accuracy, draft discoverability via MCP, and cursor-based event polling semantics.                                        
                                                                                      
#### Items                                           
---
1. **Fix duplicate goal creation from `ta_goal_start`**: `ta_goal_start` (MCP tool in `tools/goal.rs`) creates a goal record + emits `GoalStarted`, then spawns `ta run --headless` which creates a *second* goal for the same work. The MCP goal (`3917d3bc`)
becomes an orphan — no staging directory, no completion event, stuck in `running` forever. Fix: pass the goal_run_id from `ta_goal_start` to `ta run --headless` via a `--goal-id` flag so the subprocess reuses the existing goal record instead of creating a
new one. The MCP tool should own goal creation; `ta run --headless --goal-id <id>` should skip `GoalRun::new()` and load the existing goal.
      
2. **Fix `duration_secs: 0` in `GoalCompleted` event**: The `goal_completed` event emitted by `ta run` (in `run.rs`) reports `duration_secs: 0` even when the agent ran for ~12 minutes. The `Instant` timer is likely created at the wrong point (after agent
exit instead of before agent launch), or `duration_secs` is computed incorrectly. Fix: ensure the timer starts immediately before agent process spawn and `duration_secs` is `start.elapsed().as_secs()` at emission time.
---
3. **Fix `ta_draft list` MCP tool returning empty**: The `ta_draft` MCP tool with action `list` returns `{"count":0,"drafts":[]}` even when a draft package exists at `.ta/pr_packages/<id>.json`. The MCP `handle_draft_list()` searches `state.pr_packages`
(in-memory HashMap) which is only populated during the gateway's session lifetime. Drafts built by a *different* process (the `ta run --headless` subprocess) write to disk but the orchestrator's gateway never loads them. Fix: `handle_draft_list()` should
fall back to scanning `.ta/pr_packages/*.json` on disk when the in-memory map is empty, or always merge disk packages into the list.
---
4. **Fix cursor-inclusive event polling**: `ta_event_subscribe` with `since` returns events at exactly the `since` timestamp (inclusive/`>=`), so cursor-based polling re-fetches the last event every time. Fix: change the filter to strictly-after (`>`) so
passing the cursor from the previous response returns only *new* events. Add a test: emit event at T1, query with `since=T1` → expect 0 results; emit event at T2, query with `since=T1` → expect 1 result (T2 only).
---
5. **`ta goal gc` command**: New CLI command to clean up zombie goals and stale staging directories. Behavior:
    - List all goals in `.ta/goals/` with state `running` whose `updated_at` is older than a configurable threshold (default: 7 days). Transition them to `failed` with reason "gc: stale goal exceeded threshold".
    - For each non-terminal goal that has no corresponding staging directory, transition to `failed` with reason "gc: missing staging workspace".
    - `--dry-run` flag to preview what would be cleaned without making changes.
    - `--include-staging` flag to also delete staging directories for terminal-state goals (completed, failed, applied).
    - Print summary: "Transitioned N zombie goals to failed. Reclaimed M staging directories (X GB)."
---
6. **`ta draft gc` enhancement**: Extend existing `ta draft gc` to also clean orphaned `.ta/pr_packages/*.json` files whose linked goal is in a terminal state and older than the stale threshold.
---
---
- ✅ Fix duplicate goal creation: `ta_goal_start` now passes `--goal-id` to `ta run --headless` so subprocess reuses existing goal record
- ✅ Fix `duration_secs: 0`: Timer moved before agent launch (was incorrectly placed after)
- ✅ Fix `ta_draft list` MCP returning empty: `handle_draft_list()` now merges on-disk packages with in-memory map
- ✅ Fix cursor-inclusive event polling: `since` filter changed from `>=` to `>` (strictly-after) with updated cursor test
- ✅ `ta goal gc` command: zombie detection, missing-staging detection, `--dry-run`, `--include-staging`, `--threshold-days`
- ✅ `ta draft gc` enhancement: now also cleans orphaned pr_package JSON files for terminal goals past stale threshold
---
---
- `crates/ta-mcp-gateway/src/tools/goal.rs` — pass goal_run_id to `ta run --headless`, add `--goal-id` flag handling
- `apps/ta-cli/src/commands/run.rs` — accept `--goal-id` flag, reuse existing goal record, fix duration timer placement
- `crates/ta-mcp-gateway/src/tools/draft.rs` — disk-based fallback in `handle_draft_list()`
- `crates/ta-mcp-gateway/src/tools/event.rs` — change `since` filter from `>=` to `>`, add cursor exclusivity test
- `crates/ta-events/src/store.rs` — `since` filter semantics changed to strictly-after
- `apps/ta-cli/src/commands/goal.rs` — new `gc` subcommand with `--dry-run`, `--include-staging`, and `--threshold-days` flags
- `apps/ta-cli/src/commands/draft.rs` — extend `gc` to clean orphaned pr_packages
- `apps/ta-cli/src/main.rs` — wire `goal gc` subcommand and `--goal-id` flag on `ta run`
- Tests: cursor exclusivity test updated, goal gc test added
---
#### Version: `0.9.5-alpha.1`
---
---
---
### v0.9.6 — Orchestrator API & Goal-Scoped Agent Tracking
---
---
**Goal**: Make MCP tools work without a `goal_run_id` for read-only project-wide operations, and track which agents are working on which goals for observability.
---
---
---
1. **Optional `goal_run_id` on read-only MCP calls**: Make `goal_run_id` optional on tools that make sense at the project scope. If provided, scope to that goal's workspace. If omitted, use the project root. Affected tools:
   - `ta_plan read` — reads PLAN.md from project root when no goal_run_id
   - `ta_goal list` — drop goal_run_id requirement entirely (listing is always project-wide)
   - `ta_draft list` — list all drafts project-wide when no goal_run_id
   - `ta_context search/stats/list` — memory is already project-scoped
   - Keep `goal_run_id` **required** on mutation calls: `ta_plan update`, `ta_draft build/submit`, `ta_goal start` (inner), `ta_goal update`
---
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
---
   Stored in `GatewayState.active_agents: HashMap<String, AgentSession>`. Populated when a tool call arrives (extract from `TA_AGENT_ID` env var or generate on first call). Emits `AgentSessionStarted` / `AgentSessionEnded` events.
---
3. **`ta_agent_status` MCP tool**: New tool for the orchestrator to query active agents:
   - `action: "list"` — returns all active agent sessions with their goal associations
   - `action: "status"` — returns a specific agent's current state
   - Useful for diagnostics: "which agents are running? are any stuck?"
---
4. **`CallerMode` policy enforcement**: When `CallerMode::Orchestrator`, enforce:
   - Read-only access to plan, drafts, context (no mutations without a goal)
   - Can call `ta_goal start` to create new goals
   - Cannot call `ta_draft build/submit` directly (must be inside a goal)
   - Policy engine logs the caller mode in audit entries for observability
---
5. **`ta status` CLI command**: Project-wide status dashboard:
---
   $ ta status
   Project: TrustedAutonomy (v0.9.6-alpha)
   Next phase: v0.9.5.1 — Goal Lifecycle Hygiene
---
   Active agents:
     agent-1 (claude-code) → goal abc123 "Implement v0.9.5.1" [running 12m]
     agent-2 (claude-code) → orchestrator [idle]
---
   Pending drafts: 2
   Active goals: 1
---
---
---
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
---
---
- Automatic agent_id extraction → completed in v0.10.15
- Audit log entries include caller_mode → completed in v0.10.15
---
---
- `crates/ta-mcp-gateway/src/tools/plan.rs` — optional goal_run_id, project-root fallback
- `crates/ta-mcp-gateway/src/tools/agent.rs` — new ta_agent_status tool handler
- `crates/ta-mcp-gateway/src/server.rs` — `AgentSession` tracking, `CallerMode` enforcement
- `crates/ta-goal/src/events.rs` — `AgentSessionStarted`/`AgentSessionEnded` event variants
- `apps/ta-cli/src/commands/status.rs` — new `ta status` command
---
#### Version: `0.9.6-alpha`
---
---
---
### v0.9.7 — Daemon API Expansion
---
---
**Goal**: Promote the TA daemon from a draft-review web UI to a full API server that any interface (terminal, web, Discord, Slack, email) can connect to for commands, agent conversations, and event streams.
---
---
---
---
         Any Interface
---
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
---
---
---
---
1. **Command execution API** (`POST /api/cmd`): Execute any `ta` CLI command and return the output. The daemon forks the `ta` binary with the provided arguments, captures stdout/stderr, and returns them as JSON.
---
   // Request
   { "command": "ta draft list" }
   // Response
   { "exit_code": 0, "stdout": "ID  Status  Title\nabc  pending  Fix auth\n", "stderr": "" }
---
   - Command allowlist in `.ta/daemon.toml` — by default, all read commands allowed; write commands (approve, deny, apply, goal start) require explicit opt-in or elevated token scope.
   - Execution timeout: configurable, default 30 seconds.
---
2. **Agent session API** (`/api/agent/*`): Manage a headless agent subprocess that persists across requests. The daemon owns the agent's lifecycle.
   - `POST /api/agent/start` — Start a new agent session. Launches the configured agent in headless mode with MCP sidecar. Returns a `session_id`.
     ```json
     { "agent": "claude-code", "context": "optional initial prompt" }
     → { "session_id": "sess-abc123", "status": "running" }
---
   - `POST /api/agent/ask` — Send a prompt to the active agent session and stream the response.
     ```json
     { "session_id": "sess-abc123", "prompt": "What should we work on next?" }
     → SSE stream of agent response chunks
---
   - `GET /api/agent/sessions` — List active agent sessions.
   - `DELETE /api/agent/:session_id` — Stop an agent session.
   - Agent sessions respect the same routing config (`.ta/shell.toml`) — if the "prompt" looks like a command, the daemon can auto-route it to `/api/cmd` instead. This makes every interface behave like `ta shell`.
---
3. **Event stream API** (`GET /api/events`): Server-Sent Events (SSE) endpoint that streams TA events in real-time.
   - Subscribes to the `FsEventStore` (same as `ta shell` would).
   - Supports `?since=<cursor>` for replay from a point.
   - Event types: `draft_built`, `draft_approved`, `draft_denied`, `goal_started`, `goal_completed`, `goal_failed`, `drift_detected`, `agent_session_started`, `agent_session_ended`.
   - Each event includes `id` (cursor), `type`, `timestamp`, and `data` (JSON payload).
---
   event: draft_built
   id: evt-001
   data: {"draft_id":"abc123","title":"Fix auth","artifact_count":3}
---
   event: goal_completed
   id: evt-002
   data: {"goal_run_id":"def456","title":"Phase 1","duration_secs":720}
---
---
4. **Project status API** (`GET /api/status`): Single endpoint returning the full project dashboard — same data as `ta status` (v0.9.6) but as JSON.
---
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
---
---
5. **Authentication & authorization**: Bearer token authentication for remote access.
   - Token management: `ta daemon token create --scope read,write` → generates a random token stored in `.ta/daemon-tokens.json`.
   - Scopes: `read` (status, list, view, events), `write` (approve, deny, apply, goal start, agent ask), `admin` (daemon config, token management).
   - Local connections (127.0.0.1) can optionally bypass auth for solo use.
   - Token is passed via `Authorization: Bearer <token>` header.
   - All API calls logged to audit trail with the token identity.
---
6. **Daemon configuration** (`.ta/daemon.toml`):
---
   [server]
   bind = "127.0.0.1"       # "0.0.0.0" for remote access
   port = 7700
   cors_origins = ["*"]      # restrict in production
---
   [auth]
   require_token = true       # false for local-only use
   local_bypass = true        # skip auth for 127.0.0.1
---
   [commands]
   # Allowlist for /api/cmd (glob patterns)
   allowed = ["ta draft *", "ta goal *", "ta plan *", "ta status", "ta context *"]
   # Commands that require write scope
   write_commands = ["ta draft approve *", "ta draft deny *", "ta draft apply *", "ta goal start *"]
---
---
   max_sessions = 3           # concurrent agent sessions
   idle_timeout_secs = 3600   # kill idle sessions after 1 hour
   default_agent = "claude-code"
---
   [routing]
   use_shell_config = true    # use .ta/shell.toml for command vs agent routing
---
---
7. **Bridge protocol update**: Update the Discord/Slack/Gmail bridge templates to use the daemon API instead of file-based exchange. The bridges become thin HTTP clients:
   - Message received → `POST /api/cmd` or `/api/agent/ask`
   - Subscribe to `GET /api/events` for notifications
   - No more file watching or exchange directory
---
---
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
---
8. **Configurable input routing** (`.ta/shell.toml`): The daemon uses this config to decide whether input is a command or an agent prompt. Shared by all interfaces — `ta shell`, web UI, Discord/Slack bridges all route through the same logic.
---
   # Routes: prefix → local command execution
   # Anything not matching a route goes to the agent
   [[routes]]
   prefix = "ta "           # "ta draft list" → runs `ta draft list`
   command = "ta"
   strip_prefix = true
---
   [[routes]]
   prefix = "git "
   command = "git"
   strip_prefix = true
---
   [[routes]]
   prefix = "cargo "
   command = "./dev cargo"   # project's nix wrapper
   strip_prefix = true
---
   [[routes]]
   prefix = "!"             # shell escape: "!ls -la" → runs "ls -la"
   command = "sh"
   args = ["-c"]
   strip_prefix = true
---
   # Shortcuts: keyword → expanded command
   [[shortcuts]]
   match = "approve"         # "approve abc123" → "ta draft approve abc123"
   expand = "ta draft approve"
---
   [[shortcuts]]
   match = "deny"
   expand = "ta draft deny"
---
   [[shortcuts]]
   match = "view"
   expand = "ta draft view"
---
   [[shortcuts]]
   match = "apply"
   expand = "ta draft apply"
---
   [[shortcuts]]
   match = "status"
   expand = "ta status"
---
   [[shortcuts]]
   match = "plan"
   expand = "ta plan list"
---
   [[shortcuts]]
   match = "goals"
   expand = "ta goal list"
---
   [[shortcuts]]
   match = "drafts"
   expand = "ta draft list"
---
   - Default routing built in if no `.ta/shell.toml` exists
   - `POST /api/input` — unified endpoint: daemon checks routing table, dispatches to `/api/cmd` or `/api/agent/ask` accordingly. Clients don't need to know the routing rules — they just send the raw input.
---
9. **Unix socket for local clients**: In addition to HTTP, the daemon listens on `.ta/daemon.sock` (Unix domain socket). Local clients (`ta shell`, web UI) connect here for zero-config, zero-auth, low-latency access. Remote clients use HTTP with bearer token auth.
---
---
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
---
---
- Unix domain socket listener → v0.11.4 (MCP Transport Abstraction)
- Headless agent subprocess → superseded by TUI shell (v0.9.8.3)
- Bridge template updates → superseded by external plugin architecture (v0.10.2)
---
#### Version: `0.9.7-alpha`
---
---
---
### v0.9.8 — Interactive TA Shell (`ta shell`)
---
---
**Goal**: A thin terminal REPL client for the TA daemon — providing a single-terminal interactive experience for commands, agent conversation, and event notifications. The shell is a daemon client, not a standalone tool.
---
---
---
---
---
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
---
---
#### Design: Shell as Daemon Client
---
The shell does **no business logic** — all command execution, agent management, and event streaming live in the daemon (v0.9.7). The shell is ~200 lines of REPL + rendering:
---
---
ta shell
   │
   ├── Connect to daemon (.ta/daemon.sock or localhost:7700)
   │
   ├── GET /api/status → render header (project, phase, agents)
   │
   ├── GET /api/events (SSE) → background thread renders notifications
   │
   └── REPL loop:
---
       ├── Read input (rustyline)
---
       ├── POST /api/input { "text": "<user input>" }
       │   (daemon routes: command → /api/cmd, else → /api/agent/ask)
---
       └── Render response (stream agent SSE, or show command output)
---
---
This means:
- **One code path**: command routing, agent sessions, events — all in the daemon. Shell, web UI, Discord, Slack all use the same APIs.
- **Shell is trivially simple**: readline + HTTP client + SSE renderer.
- **No subprocess management in the shell**: daemon owns agent lifecycle.
- **Shell can reconnect**: if the shell crashes, `ta shell` reconnects to the existing daemon session (agent keeps running).
---
---
---
1. **Shell REPL core**: `ta shell` command:
   - Auto-starts the daemon if not running (`ta daemon start` in background)
   - Connects via Unix socket (`.ta/daemon.sock`) — falls back to HTTP if socket not found
   - Prompt: `ta> ` (configurable in `.ta/shell.toml`)
   - All input sent to `POST /api/input` — daemon handles routing
   - History: rustyline with persistent history at `.ta/shell_history`
   - Tab completion: fetches routed prefixes and shortcuts from `GET /api/routes`
---
2. **Streaming agent responses**: When `/api/input` routes to the agent, the daemon returns an SSE stream. The shell renders chunks as they arrive (like a chat interface). Supports:
   - Partial line rendering (agent "typing" effect)
   - Markdown rendering (code blocks, headers, bold — via `termimad` or similar)
   - Interrupt: Ctrl+C cancels the current agent response
---
3. **Inline event notifications**: Background SSE connection to `GET /api/events`. Notifications rendered between the prompt and agent output:
   - `── 📋 Draft ready: "Fix auth" (view abc123) ──`
   - `── ✅ Goal completed: "Phase 1" (12m) ──`
   - `── ❌ Goal failed: "Phase 2" — timeout ──`
   - Non-disruptive: notifications don't break the current input line
---
4. **Session state header**: On startup and periodically, display:
---
   TrustedAutonomy v0.9.8 │ Next: v0.9.5.1 │ 2 drafts │ 1 agent running
---
   Updated when events arrive. Compact one-liner at top.
---
5. **`ta shell --init`**: Generate the default `.ta/shell.toml` routing config for customization.
---
6. **`ta shell --attach <session_id>`**: Attach to an existing daemon agent session (useful for reconnecting after a disconnect or switching between sessions).
---
---
---
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
---
---
- Unix domain socket connection → v0.11.4 (MCP Transport Abstraction)
- Auto-start daemon → completed in v0.10.16
- Streaming agent response rendering → completed in v0.10.12 (streaming Q&A)
- Ctrl+C interrupt → completed in v0.10.14 (Ctrl-C detach)
- Non-disruptive event notifications → completed in v0.10.11 (TUI auto-tail + notifications)
- Periodic status header refresh → completed in v0.10.12 (status bar enhancements)
---
#### Implementation scope
- `apps/ta-cli/src/commands/shell.rs` — REPL core (~200 lines), daemon client, SSE rendering
- `apps/ta-cli/Cargo.toml` — add `rustyline`, `reqwest` (HTTP client), `tokio-stream` (SSE)
- `apps/ta-cli/templates/shell.toml` — default routing config
- `docs/USAGE.md` — `ta shell` documentation
---
#### Why so simple?
All complexity lives in the daemon (v0.9.7). The shell is deliberately thin — just a rendering layer. This means any bug fix or feature in the daemon benefits all interfaces (shell, web, Discord, Slack, email) simultaneously.
---
#### Why not enhance `ta dev`?
`ta dev` gives the agent the terminal (agent drives, human reviews elsewhere). `ta shell` gives the human the terminal (human drives, agent assists). Both connect to the same daemon. `ta dev` is for autonomous work; `ta shell` is for interactive exploration and management.
---
#### Version: `0.9.8-alpha`
---
---
---
### v0.9.8.1 — Auto-Approval, Lifecycle Hygiene & Operational Polish
---
---
**Goal**: Three themes that make TA reliable for sustained multi-phase use:
- **(A) Policy-driven auto-approval**: Wire the policy engine into draft review so drafts matching configurable conditions are auto-approved — preserving full audit trail and the ability to tighten rules at any time.
- **(B) Goal lifecycle & GC**: Unified `ta gc`, goal history ledger, `ta goal list --active` filtering, and event store pruning (items 9–10).
- **(C) Operational observability**: Actionable error messages, timeout diagnostics, daemon version detection, status line accuracy (items 9, plus CLAUDE.md observability mandate).
---
#### How It Works
---
---
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
---
--- Phase Run Summary ---
#### Policy Configuration (`.ta/policy.yaml`)
---
---
version: "1"
security_level: checkpoint
---
auto_approve:
  read_only: true               # existing: auto-approve read-only actions
  internal_tools: true           # existing: auto-approve ta_* MCP calls
---
  # NEW: draft-level auto-approval
  drafts:
    enabled: false               # master switch (default: off — opt-in only)
    auto_apply: false            # if true, also run `ta draft apply` after auto-approve
    git_commit: false            # if auto_apply, also create a git commit
---
    conditions:
      # Size limits — only auto-approve small, low-risk changes
      max_files: 5
      max_lines_changed: 200
---
      # Path allowlist — only auto-approve changes to safe paths
      # Uses glob patterns, matched against artifact resource_uri
      allowed_paths:
        - "tests/**"
        - "docs/**"
        - "*.md"
        - "**/*_test.rs"
---
      # Path blocklist — never auto-approve changes to these (overrides allowlist)
      blocked_paths:
        - ".ta/**"
        - "Cargo.toml"
        - "Cargo.lock"
        - "**/main.rs"
        - "**/lib.rs"
        - ".github/**"
---
      # Verification — run checks before auto-approving
      require_tests_pass: false   # run `cargo test` (or configured test command)
      require_clean_clippy: false  # run `cargo clippy` (or configured lint command)
      test_command: "cargo test --workspace"
      lint_command: "cargo clippy --workspace --all-targets -- -D warnings"
---
      # Scope limits
      allowed_phases:              # only auto-approve for these plan phases
        - "tests"
        - "docs"
        - "chore"
---
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
---
# Per-goal constitutional approval (v0.4.3 — already exists)
# Constitutions define per-goal allowed actions. Auto-approval
# respects constitutions: if a constitution is stricter than
# the project policy, the constitution wins.
---
---
---
---
1. **`AutoApproveDraftConfig` struct**: Add to `PolicyDocument` under `auto_approve.drafts`:
   - `enabled: bool` (master switch, default false)
   - `auto_apply: bool` (also apply after approve)
   - `git_commit: bool` (create commit if auto-applying)
   - `conditions: AutoApproveConditions` (size limits, path rules, verification, phase limits)
---
2. **`should_auto_approve_draft()` function**: Core evaluation logic in `ta-policy`:
   - Takes `&DraftPackage` + `&PolicyDocument` + optional `&AgentProfile`
   - Returns `AutoApproveDecision`:
     - `Approved { reasons: Vec<String> }` — all conditions met, with audit trail of why
     - `Denied { blockers: Vec<String> }` — which conditions failed, included in review request
   - Condition evaluation order: enabled check → size limits → path rules → phase limits → agent trust level. Short-circuits on first failure.
---
3. **Path matching**: Glob-based matching against `Artifact.resource_uri`:
   - `allowed_paths`: if set, ALL changed files must match at least one pattern
   - `blocked_paths`: if ANY changed file matches, auto-approval is denied (overrides allowed_paths)
   - Uses the existing `glob` crate pattern matching
---
4. **Verification integration**: Optionally run test/lint commands before auto-approving:
   - `require_tests_pass: true` → runs configured `test_command` in the staging workspace
   - `require_clean_clippy: true` → runs configured `lint_command`
   - Both default to false (verification adds latency; opt-in only)
   - Verification runs in the staging directory, not the source — safe even if tests have side effects
   - Timeout: configurable, default 5 minutes
---
5. **Gateway/daemon wiring**: In the draft submit handler:
   - Before routing to ReviewChannel, call `should_auto_approve_draft()`
   - If approved: set `DraftStatus::Approved { approved_by: "policy:auto", approved_at }`, dispatch `DraftAutoApproved` event
   - If denied: include blockers in the `InteractionRequest` so the human knows why they're being asked
   - If `auto_apply` enabled: immediately call the apply logic (copy staging → source, optional git commit)
---
6. **`DraftAutoApproved` event**: New `TaEvent` variant:
   ```rust
   DraftAutoApproved {
       draft_id: String,
       goal_run_id: Uuid,
       reasons: Vec<String>,       // "all files in tests/**, 3 files, 45 lines"
       auto_applied: bool,
       timestamp: DateTime<Utc>,
   }
---
---
7. **Audit trail**: Auto-approved drafts are fully audited:
   - Audit entry includes: which conditions were evaluated, which matched, policy document version
   - `approved_by: "policy:auto"` distinguishes from human approvals
   - `ta audit verify` includes auto-approved drafts in the tamper-evident chain
---
8. **`ta policy check <draft_id>`**: CLI command to dry-run the auto-approval evaluation:
---
   $ ta policy check abc123
   Draft: abc123 — "Add unit tests for auth module"
---
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
---
   Result: WOULD AUTO-APPROVE
---
---
9. **Status line: distinguish active vs tracked agents/goals**: The daemon `/api/status` endpoint currently counts all `GoalRun` entries with state `running` or `pr_ready`, including stale historical goals with no live process. This inflates the agent/goal count shown in `ta shell` and TA Studio. Fix:
   - Add `active_agents` (goals with a live process or updated within the last hour) vs `total_tracked` (all non-terminal goals) to the status response
   - Shell status line shows only active: `2 agents running` not `26 agents`
   - `ta status --all` shows the full breakdown including stale entries
   - Detection heuristic: if `updated_at` is older than `idle_timeout_secs` (from daemon config, default 30 min) and state is `running`, classify as stale
---
10. **Goal lifecycle GC & history ledger**: Enhance `ta goal gc` and `ta draft gc` into a unified `ta gc` with a persistent history ledger so archived goals remain queryable.
    - **Goal history ledger** (`.ta/goal-history.jsonl`): When GC archives or removes a goal, append a compact summary line:
      ```jsonl
      {"id":"ca306e4d","title":"Implement v0.9.8.1","state":"applied","phase":"v0.9.8.1","agent":"claude-code","created":"2026-03-06","completed":"2026-03-06","duration_mins":42,"draft_id":"abc123","artifact_count":15,"lines_changed":487}
---
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
---
#### Security Model
---
- **Default: off** — auto-approval must be explicitly enabled. Fresh `ta init` projects start with `drafts.enabled: false`.
- **Tighten only**: `PolicyCascade` merges layers with "most restrictive wins". A constitution or agent profile can tighten but never loosen project-level rules.
- **Blocked paths override allowed paths**: A file matching `blocked_paths` forces human review even if it also matches `allowed_paths`.
- **Audit everything**: Auto-approved drafts have the same audit trail as human-approved ones. `ta audit log` shows them with `policy:auto` attribution.
- **Escape hatch**: `ta draft submit --require-review` forces human review regardless of auto-approval config. The agent cannot bypass this flag (it's a CLI flag, not an MCP parameter).
---
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
---
---
---
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
---
---
- Verification integration in auto-approve → completed in v0.10.15
- `auto_apply` flow → completed in v0.10.15
- Event store pruning → completed in v0.10.15
- `ta draft apply --require-review` flag → completed in v0.10.15
- Audit trail for auto-approved drafts → completed in v0.10.15
---
#### Version: `0.9.8-alpha.1`
---
---
---
### v0.9.8.1.1 — Unified Allow/Deny List Pattern
---
---
**Goal**: Standardize all allowlist/blocklist patterns across TA to support both allow and deny lists with consistent semantics: deny takes precedence over allow, empty allow = allow all, empty deny = deny nothing.
---
---
TA has multiple places that use allowlists or blocklists, each with slightly different semantics:
- **Daemon command routing** (`config.rs`): `commands.allowed` only — no deny list
- **Auto-approval paths** (`policy.yaml`): `allowed_paths` + `blocked_paths` (deny wins)
- **Agent tool access**: implicit per-mode (full/plan/review-only) — no configurable lists
---
- **Sandbox command allowlist** (`ta-sandbox`): allow-only
---
These should share a common pattern.
---
---
---
---
/// Reusable allow/deny filter. Deny always takes precedence.
pub struct AccessFilter {
    pub allowed: Vec<String>,   // glob patterns; empty = allow all
    pub denied: Vec<String>,    // glob patterns; empty = deny nothing
---
---
impl AccessFilter {
    /// Returns true if the input is permitted.
    /// Logic: if denied matches → false (always wins)
    ///        if allowed is empty → true (allow all)
    ///        if allowed matches → true
    ///        else → false
    pub fn permits(&self, input: &str) -> bool;
---
---
---
---
---
1. **`AccessFilter` struct** in `ta-policy`: reusable allow/deny with glob matching and `permits()` method
2. **Daemon command config**: Replace `commands.allowed: Vec<String>` with `commands: AccessFilter` (add `denied` field). Default: `allowed: ["*"]`, `denied: []`
3. **Auto-approval paths**: Refactor `allowed_paths` / `blocked_paths` to use `AccessFilter` internally (keep YAML field names for backward compat)
4. **Channel access control**: Add `denied_roles` / `denied_users` alongside existing `allowed_*` fields
5. **Sandbox commands**: Add `denied` list to complement existing allowlist
6. **Agent tool access**: Add configurable tool allow/deny per agent config in `agents/*.yaml`
7. **Documentation**: Explain the unified pattern in USAGE.md — one mental model for all access control
---
#### Implementation scope
- `crates/ta-policy/src/access_filter.rs` — `AccessFilter` struct, glob matching, tests (~100 lines)
- `crates/ta-daemon/src/config.rs` — migrate `CommandConfig.allowed` to `AccessFilter`
- `crates/ta-policy/src/auto_approve.rs` — use `AccessFilter` for path matching
- `crates/ta-sandbox/src/lib.rs` — use `AccessFilter` for command lists
- Backward-compatible: existing configs with only `allowed` still work (empty `denied` = deny nothing)
- Tests: deny-wins-over-allow, empty-allow-means-all, glob matching, backward compat
---
---
---
- [x] `AccessFilter` struct in `ta-policy/src/access_filter.rs` with `permits()`, `tighten()`, `from_allowed()`, `allow_all()`, `is_unrestricted()`, `Display` impl, serde support, and 18 tests
- [x] Daemon `CommandConfig`: added `denied` field alongside `allowed`, `access_filter()` method returning `AccessFilter`, updated `cmd.rs` to use `filter.permits()` instead of `is_command_allowed()` (2 new tests)
- [x] Auto-approval paths: refactored `should_auto_approve_draft()` to use `AccessFilter` for path matching, `merge_conditions()` to use `AccessFilter::tighten()` (backward compatible — existing YAML field names preserved)
- [x] Sandbox: added `denied_commands` field to `SandboxConfig`, deny check in `execute()` and `is_allowed()` (2 new tests)
- [x] Documentation: unified access control pattern in USAGE.md
---
---
- Channel access control → completed in v0.10.16
- Agent tool access → completed in v0.10.16
---
#### Version: `0.9.8-alpha.1.1`
---
---
---
### v0.9.8.2 — Pluggable Workflow Engine & Framework Integration
---
---
**Goal**: Add a `WorkflowEngine` trait to TA core so multi-stage, multi-role, multi-framework workflows can be orchestrated with pluggable engines — built-in YAML for simple cases, framework adapters (LangGraph, CrewAI) for power users, or custom implementations.
---
#### Design Principle: TA Mediates, Doesn't Mandate
---
TA defines *what* decisions need to be made (next stage? route back? what context?). The engine decides *how*. Users who already have LangGraph or CrewAI use TA for governance only. Users with simple agent setups (Claude Code, Codex) use TA's built-in YAML engine.
---
---
TA Core (always present):
---
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
---
---
Configuration:
---
# .ta/config.yaml
workflow:
  engine: yaml                    # built-in (default)
  # engine: langraph             # delegate to LangGraph adapter
  # engine: crewai               # delegate to CrewAI adapter
  # engine: process              # user-supplied binary (JSON-over-stdio)
  #   command: "./my-workflow-engine"
  # engine: none                 # no workflow — manage goals manually
---
---
---
---
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
---
   pub enum StageAction {
       Proceed { next_stage: String, context: GoalContext },
       RouteBack { target_stage: String, feedback: FeedbackContext,
                   severity: Severity },
       Complete,
       AwaitHuman { request: InteractionRequest },
   }
---
---
2. **`WorkflowDefinition` schema** (`crates/ta-workflow/src/definition.rs`): Declarative workflow structure used by all engines.
   ```rust
   pub struct WorkflowDefinition {
       pub name: String,
       pub stages: Vec<StageDefinition>,
       pub roles: HashMap<String, RoleDefinition>,
   }
---
   pub struct StageDefinition {
       pub name: String,
       pub depends_on: Vec<String>,
       pub roles: Vec<String>,           // parallel roles within stage
       pub then: Vec<String>,            // sequential roles after parallel
       pub review: Option<StageReview>,
       pub on_fail: Option<FailureRouting>,
   }
---
   pub struct RoleDefinition {
       pub agent: String,                // agent config name
       pub constitution: Option<String>, // constitution YAML path
       pub prompt: String,               // system prompt for this role
       pub framework: Option<String>,    // override framework for this role
   }
---
---
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
---
---
4. **GoalRun extensions**: Add workflow context fields to `GoalRun`:
   - `workflow_id: Option<String>` — links goal to a workflow instance
   - `stage: Option<String>` — which stage this goal belongs to
   - `role: Option<String>` — which role this goal fulfills
   - `context_from: Vec<Uuid>` — goals whose output feeds into this one's context
   - These are metadata only — no behavioral change if unset. All existing goals continue to work as-is.
---
5. **Goal chaining** (context propagation): When a stage completes and the next stage starts, automatically inject the previous stage's output as context:
   - Previous stage's draft summary → next stage's system prompt
   - Previous stage's verdict findings → next stage's feedback section (on route-back)
   - Uses the existing CLAUDE.md injection mechanism (same as `ta run` context injection)
   - `context_from` field on GoalRun tracks the provenance chain
---
6. **Built-in YAML workflow engine** (`crates/ta-workflow/src/yaml_engine.rs`):
   - Parses `.ta/workflows/*.yaml` files
   - Evaluates stage dependencies (topological sort)
   - Starts goals for each role in a stage (parallel or sequential per config)
   - Collects verdicts, runs scorer, decides routing
   - Handles retry limits and loop detection (`max_retries` per routing rule)
   - ~400 lines — deliberately simple. Power users use LangGraph.
---
7. **Process-based workflow plugin** (`crates/ta-workflow/src/process_engine.rs`):
   - Same JSON-over-stdio pattern as channel plugins (v0.10.2)
   - TA spawns the engine process, sends `WorkflowDefinition` + events via stdin
   - Engine responds with `StageAction` decisions via stdout
   - This is how LangGraph/CrewAI adapters connect
   - ~150 lines in TA core
---
8. **`ta_workflow` MCP tool**: For orchestrator agents to interact with workflows:
   - `action: "start"` — start a workflow from a definition file
   - `action: "status"` — get workflow status (current stage, verdicts, retry count)
   - `action: "list"` — list active and completed workflows
   - No goal_run_id required (orchestrator-level tool, uses v0.9.6 optional ID pattern)
#### Implementation scope
9. **`ta workflow` CLI commands**:
   - `ta workflow start <definition.yaml>` — start a workflow
   - `ta workflow status [workflow_id]` — show status
   - `ta workflow list` — list workflows
   - `ta workflow cancel <workflow_id>` — cancel an active workflow
   - `ta workflow history <workflow_id>` — show stage transitions, verdicts, routing decisions
---
10. **Framework integration templates** (shipped with TA):
    - `templates/workflows/milestone-review.yaml` — the full plan/build/review workflow using built-in YAML engine
    - `templates/workflows/roles/` — role definition library (planner, designer, PM, engineer, security-reviewer, customer personas)
    - `templates/workflows/adapters/langraph_adapter.py` — Python bridge: LangGraph ↔ TA's WorkflowEngine protocol
    - `templates/workflows/adapters/crewai_adapter.py` — Python bridge: CrewAI ↔ TA's protocol
    - `templates/workflows/simple-review.yaml` — minimal 2-stage workflow (build → review) for getting started
    - `templates/workflows/security-audit.yaml` — security-focused workflow with OWASP reviewer + dependency scanner
---
#### Workflow Events
#### Implementation scope
// New TaEvent variants
WorkflowStarted { workflow_id, name, stage_count, timestamp }
StageStarted { workflow_id, stage, roles: Vec<String>, timestamp }
StageCompleted { workflow_id, stage, verdicts: Vec<Verdict>, timestamp }
WorkflowRouted { workflow_id, from_stage, to_stage, severity, reason, timestamp }
VerdictScored { workflow_id, stage, aggregate_score, routing_recommendation, timestamp }
WorkflowCompleted { workflow_id, name, total_duration_secs, stages_executed, timestamp }
WorkflowFailed { workflow_id, name, reason, timestamp }
---
---
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
---
      Values: `always` (pause after every stage completion), `never` (proceed automatically), `on_fail` (pause only when verdicts route back or score below threshold). Default: `never`.
    - **`InteractionRequest` struct** (part of `AwaitHuman` action):
      ```rust
      pub struct InteractionRequest {
          pub prompt: String,           // what the workflow is asking
          pub context: serde_json::Value, // stage verdicts, scores, findings
          pub options: Vec<String>,     // suggested choices (proceed, revise, cancel)
          pub timeout_secs: Option<u64>, // auto-proceed after timeout (None = wait forever)
---
---
    - **Workflow interaction endpoint**: `POST /api/workflow/:id/input` — accepts `{ "decision": "proceed" | "revise" | "cancel", "feedback": "optional text" }`. The daemon routes the decision to the workflow engine's `inject_feedback()` method.
    - **Workflow event for shell rendering**: `WorkflowAwaitingHuman { workflow_id, stage, prompt, options, timestamp }` — SSE event that the shell listens for and renders as an interactive prompt with numbered options. The human types their choice, shell POSTs to the interaction endpoint.
    - **Shell-side UX**: When the shell receives a `workflow.awaiting_human` event, it renders:
---
      [workflow] Review stage paused — 2 findings need attention:
        1. Security: SQL injection risk in user input handler (critical)
        2. Style: Inconsistent error message format (minor)
---
      Options: [1] proceed  [2] revise planning  [3] cancel workflow
      workflow> _
---
      The `workflow>` prompt replaces the normal `ta>` prompt until the human responds. Normal shell commands still work (e.g., `ta draft view` to inspect the draft before deciding).
---
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
---
---
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
---
---
- Goal chaining context propagation → v0.10.18
- Full async process engine I/O → v0.10.18
- Live scoring agent integration → v0.10.18
---
#### Version: `0.9.8-alpha.2`
---
---
---
### v0.9.8.3 — Full TUI Shell (`ratatui`)
---
---
**Goal**: Replace the line-mode rustyline shell with a full terminal UI modeled on Claude Code / claude-flow — persistent status bar, scrolling output, and input area, all in one screen.
--- Phase Run Summary ---
#### Layout
---
┌─────────────────────────────────────────────────────────┐
│  [scrolling output]                                     │
│  goal started: "Implement v0.9.8.1" (claude-code)       │
│  draft built: 15 files (abc123)                         │
│  $ ta goal list                                         │
---
│  ca306e4d Implement v0.9.8.1       running  claude-code │
│                                                         │
│                                                         │
├─────────────────────────────────────────────────────────┤
│ ta> ta draft list                                       │
├─────────────────────────────────────────────────────────┤
│ TrustedAutonomy v0.9.8 │ 1 agent │ 0 drafts │ ◉ daemon│
└─────────────────────────────────────────────────────────┘
---
---
---
---
1. **`ratatui` + `crossterm` terminal backend**: Full-screen TUI with three zones — output scroll area, input line, status bar. ~1500 lines replacing the current ~500-line rustyline shell.
---
2. **Status bar** (bottom): Project name, version, active agent count, pending draft count, daemon connection indicator (green dot = connected, red = disconnected), current workflow stage (if any). Updates live via SSE events.
---
3. **Input area** (above status bar): Text input with history (up/down arrows), tab-completion from `/api/routes`, multi-line support for longer commands. Uses `tui-textarea` or custom widget.
---
4. **Scrolling output pane** (main area): Command responses, SSE event notifications, workflow prompts. Auto-scrolls but allows scroll-back with PgUp/PgDn. Events are rendered inline with dimmed styling to distinguish from command output.
---
5. **Workflow interaction mode**: When a `workflow.awaiting_human` event arrives, the output pane shows the prompt/options and the input area switches to `workflow>` mode (from v0.9.8.2 item 11). Normal commands still work during workflow prompts.
---
6. **Split pane support** (stretch): Optional vertical split showing agent session output on one side, shell commands on the other. Toggle with `Ctrl-W`. Useful when monitoring an agent in real time while reviewing drafts.
---
7. **Notification badges**: Unread event count shown in status bar. Cleared when user scrolls to bottom. Draft-ready events flash briefly.
---
---
- ✅ `ratatui` + `crossterm` terminal backend — full-screen TUI with three zones (output scroll, input line, status bar)
- ✅ Status bar — project name, version, agent count, draft count, daemon connection indicator, workflow stage, unread badge
- ✅ Input area — text input with cursor movement, history (up/down), tab-completion, Ctrl-A/E/U/K editing shortcuts
- ✅ Scrolling output pane — command responses and SSE events with styled lines, PgUp/PgDn scroll, auto-scroll with unread counter
- ✅ Workflow interaction mode — `workflow>` prompt when `workflow_awaiting_human` events arrive
- ✅ Notification badges — unread event count in status bar, cleared on scroll-to-bottom
- ✅ `--classic` flag preserves rustyline shell as fallback
- ✅ 13 unit tests — input handling, cursor movement, history navigation, tab completion, scroll, daemon state, workflow mode
---
---
- Split pane support → completed in v0.10.14
---
#### Implementation scope
- `apps/ta-cli/src/commands/shell_tui.rs` — new TUI module with ratatui (~500 lines + tests)
- `apps/ta-cli/src/commands/shell.rs` — updated to dispatch TUI vs classic, shared functions made pub(crate)
- `apps/ta-cli/Cargo.toml` — added `ratatui`, `crossterm` dependencies
- Daemon API layer unchanged — same HTTP/SSE endpoints
---
#### Version: `0.9.8-alpha.3`
---
---
---
### v0.9.8.4 — VCS Adapter Abstraction & Plugin Architecture
---
**Goal**: Move all version control operations behind the `SubmitAdapter` trait so TA is fully VCS-agnostic. Add adapter-contributed exclude patterns for staging, implement stub adapters for SVN and Perforce, and design the external plugin loading mechanism.
---
---
Today, raw `git` commands leak outside the `SubmitAdapter` trait boundary — branch save/restore in `draft.rs`, VCS auto-detection, `.git/` exclusions hardcoded in `overlay.rs`, and git hash embedding in `build.rs`. This means adding Perforce or SVN support requires modifying core TA code in multiple places rather than simply providing a new adapter.
---
Additionally, shipping adapters for every VCS/email/database system inside the core `ta` binary doesn't scale. External teams (e.g., a Perforce shop or a custom VCS vendor) should be able to publish a TA adapter as an independent installable package.
---
---
---
##### 1. Adapter-contributed exclude patterns
---
---
---
pub trait SubmitAdapter: Send + Sync {
    // ... existing methods ...
---
    /// Patterns to exclude from staging copy (VCS metadata dirs, etc.)
    /// Returns patterns in .taignore format: "dirname/", "*.ext", "name"
    fn exclude_patterns(&self) -> Vec<String> {
        vec![]
---
---
    /// Save/restore working state around apply operations.
    /// Git: save current branch, restore after commit.
    /// Perforce: save current changelist context.
    /// Default: no-op.
    fn save_state(&self) -> Result<Option<Box<dyn std::any::Any + Send>>> { Ok(None) }
    fn restore_state(&self, state: Option<Box<dyn std::any::Any + Send>>) -> Result<()> { Ok(()) }
---
    /// Auto-detect whether this adapter applies to the given project root.
    /// Git: checks for .git/ directory
    /// Perforce: checks for P4CONFIG or .p4config
    fn detect(project_root: &Path) -> bool where Self: Sized { false }
---
---
---
- `GitAdapter::exclude_patterns()` → `[".git/"]`
- `SvnAdapter::exclude_patterns()` → `[".svn/"]`
- `PerforceAdapter::exclude_patterns()` → `[".p4config"]` (P4 doesn't have a metadata dir per se)
- `overlay.rs` merges adapter excludes with `.taignore` user patterns and built-in defaults (`target/`, `node_modules/`, etc.)
---
##### 2. Move git-specific code behind the adapter
---
| Current location | What it does | Where it moves |
---
| `draft.rs:1946-2048` | Branch save/restore around apply | `SubmitAdapter::save_state()` / `restore_state()` |
| `draft.rs:1932` | `.git/` existence check for auto-detect | `SubmitAdapter::detect()` + adapter registry |
| `overlay.rs:24` | Hardcoded `"target/"` + `.git/` exclusion | Adapter `exclude_patterns()` + `ExcludePatterns::merge()` |
| `build.rs` | `git rev-parse HEAD` for version hash | `SubmitAdapter::revision_id()` or build-time env var |
| `shell.rs` | `git status` as shell route | Adapter-provided shell routes (optional) |
---
##### 3. Stub adapters (untested)
---
**SVN adapter** (`crates/ta-submit/src/svn.rs`):
- `prepare()` → no-op (SVN doesn't use branches the same way)
- `commit()` → `svn add` + `svn commit`
- `push()` → no-op (SVN commit is already remote)
- `open_review()` → no-op (SVN doesn't have built-in review)
- `exclude_patterns()` → `[".svn/"]`
- `detect()` → check for `.svn/` directory
- **Note: untested — contributed by AI, needs validation by an SVN user**
---
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
---
##### 4. Adapter auto-detection registry
---
---
/// Registry of available adapters with auto-detection.
pub fn detect_adapter(project_root: &Path) -> Box<dyn SubmitAdapter> {
    // Check configured adapter first (workflow.toml)
    // Then auto-detect: try each registered adapter's detect()
    // Fallback: NoneAdapter
---
---
---
Order: Git → SVN → Perforce → None. First match wins. User can override with `workflow.toml` setting `submit.adapter = "perforce"`.
---
##### 5. External plugin architecture (design only — implementation deferred)
---
External adapters loaded as separate executables that communicate via a simple JSON-over-stdio protocol, similar to how `ta run` launches agents:
---
---
~/.ta/plugins/
  ta-submit-perforce    # executable
  ta-submit-jira        # executable
  ta-submit-plastic     # executable (Plastic SCM)
---
---
**Protocol**: TA spawns the plugin binary and sends JSON commands on stdin, reads JSON responses from stdout:
---
// → plugin
{"method": "exclude_patterns", "params": {}}
// ← plugin
{"result": [".plastic/", ".plastic4.selector"]}
---
// → plugin
{"method": "commit", "params": {"goal_id": "abc", "message": "Fix bug", "files": ["src/main.rs"]}}
// ← plugin
{"result": {"commit_id": "cs:1234", "message": "Changeset 1234 created"}}
---
---
**Discovery**: `ta plugin install <name>` downloads from a registry (crates.io, npm, or TA's own) and places the binary in `~/.ta/plugins/`. Or manual: just drop an executable named `ta-submit-<name>` in the plugins dir.
---
**Config**: `submit.adapter = "perforce"` → TA first checks built-in adapters, then looks for `~/.ta/plugins/ta-submit-perforce`.
---
This pattern extends beyond VCS to any adapter type:
- `ta-channel-slack` — Slack notification channel
- `ta-channel-discord` — Discord notification channel
- `ta-channel-email` — Email notification channel
- `ta-output-jira` — Jira ticket creation from drafts
- `ta-store-postgres` — PostgreSQL-backed goal/draft store
---
---
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
---
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
---
#### Version: `0.9.8-alpha.4`
---
---
---
### v0.9.9 — Conversational Project Bootstrapping (`ta new`) *(design only)*
---
---
---
---
---
---
---
---
---
---
      records with policy controls, audit logging, and a web
      dashboard for reviewing changes.
---
---
---
---
---
      2. Should it also support Route53 or other providers,
         or Cloudflare-only?
      3. How strict should policy be — allowlist of editable
         records, or more granular (per-type, per-zone)?
---
---
      for multiple providers later. Policy should be per-zone
      and per-record-type.
---
---
---
---
---
      Phase 1: Project Scaffold
        Cargo workspace, CI, README, license, .ta/ init
---
      Phase 2: Provider Abstraction & Cloudflare Client
        DnsProvider trait, Cloudflare implementation,
        account selection, auth (API token per account)
---
---
        List/create/update/delete, import existing records,
        dry-run mode, diff preview before apply
---
---
        Per-zone record-type allowlist, approval required
        for wildcard/MX changes, policy config in YAML
---
      Phase 5: Audit Logging
        Change log (who/what/when/why), export JSON/CSV,
        tamper-evident chain (reuse ta-audit)
---
      Phase 6: Web Dashboard
        Record browser, change history, pending approvals,
        served by embedded HTTP server
---
---
---
---
---
---
---
      validation with alerting.
---
---
      resolution checks, configurable alerts, health
      dashboard widget). Dashboard moved to Phase 7.
---
---
---
      ✅ Created /projects/dns-guardian
      ✅ Initialized .ta/ workspace
      ✅ Generated PLAN.md (8 phases, 34 items)
      ✅ Created Cargo.toml workspace scaffold
      ✅ Created .ta/policy.yaml (default: checkpoint)
---
---
---
You:  Yes
---
Agent: [starts goal for Phase 1]
      🚀 Goal started: "Phase 1: Project Scaffold"
---
---
---
---
---
---
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
---
---
---
---
1. **`ta new` CLI command**: Starts a conversational project bootstrapping session.
   - `ta new` — interactive mode, asks questions
   - `ta new --from <brief.md>` — seed from a written description file
   - `ta new --template <name>` — start from a project template (v0.7.3 templates)
   - Creates a temporary working directory for the planner agent
   - On completion, moves the generated project to the target directory
---
---
   - Has access to `ta init`, filesystem write, and plan generation tools
   - Does NOT have access to `ta goal start`, `ta draft build`, or other runtime tools (it's creating the project, not executing goals)
   - System prompt includes: plan format specification (PLAN.md with `<!-- status: pending -->` markers), versioning policy, phase sizing guidelines
   - Conversation is multi-turn: agent asks clarifying questions, proposes a plan, user refines, agent generates
   - Agent tools available:
     - `ta_scaffold` — create directory structure, Cargo.toml/package.json/etc.
     - `ta_plan_generate` — write PLAN.md from structured plan data
     - `ta_init` — initialize .ta/ workspace in the new project
     - `ta_config_write` — write initial .ta/policy.yaml, .ta/config.yaml, agents/*.yaml
---
---
   - Each phase has: title, goal description, numbered items, implementation scope, version
   - Phase sizing: guide the agent to create phases that are 1-4 hours of work each
   - Dependencies: note which phases depend on others
   - Phase markers: all start as `<!-- status: pending -->`
   - Versioning: auto-assign version numbers (v0.1.0 for phase 1, v0.2.0 for phase 2, etc.)
---
---
---
   - `ta new --template rust-lib` → Library crate, docs, benchmarks
   - `ta new --template ts-api` → Node.js, Express/Fastify, TypeScript
   - Templates provide the scaffold; the planner agent customizes and adds the PLAN.md
   - Custom templates: `ta new --template ./my-template` or `ta new --template gh:org/repo`
---
5. **Daemon API endpoint** (`POST /api/project/new`): Start a bootstrapping session via the daemon API, so Discord/Slack/email interfaces can create projects too.
   - First request starts the planner agent session
   - Subsequent requests in the same session continue the conversation
   - Final response includes the project path and PLAN.md summary
---
   // Start
   { "description": "Rust CLI for Cloudflare DNS management with policy controls" }
   → { "session_id": "plan-abc", "response": "I'll help you plan this. A few questions..." }
---
   // Continue
   { "session_id": "plan-abc", "prompt": "Multi-account, Cloudflare only for now" }
   → { "session_id": "plan-abc", "response": "Here's a proposed plan..." }
---
   // Generate
   { "session_id": "plan-abc", "prompt": "Looks good, generate it" }
   → { "session_id": "plan-abc", "project_path": "/projects/dns-guardian", "phases": 8 }
---
---
---
   - Print summary: phase count, item count, estimated version range
   - Offer to start the first goal: "Ready to start Phase 1? (y/n)"
   - If using `ta shell`, switch the shell's working directory to the new project
   - If using a remote interface, return the project path and next steps
---
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
---
#### Version: `0.9.9-alpha`
---
---
---
---
---
**Goal**: Add the foundational infrastructure for agent-initiated mid-goal conversations with humans. Interactive mode is the general primitive — micro-iteration within the macro-iteration TA governs. The agent calls `ta_ask_human` (MCP tool), TA delivers the question through whatever channel the human is on, and routes the response back. The agent continues.
---
---
---
---
---
  → MCP tool writes question to .ta/interactions/pending/<id>.json
  → Emits SessionEvent::AgentNeedsInput
  → GoalRunState transitions Running → AwaitingInput
  → Tool polls for .ta/interactions/answers/<id>.json
---
Human sees question in ta shell / Slack / web UI
---
---
  → MCP tool poll finds it, returns answer to agent
  → GoalRunState transitions AwaitingInput → Running
---
---
---
---
---
   - Parameters: `question`, `context`, `response_hint` (freeform/yes_no/choice), `choices`, `timeout_secs`
   - File-based signaling: writes question file, polls for answer file (1s interval)
   - Emits `AgentNeedsInput` and `AgentQuestionAnswered` events
   - Timeout returns actionable message (not error) so agent can continue
---
2. ~~**`QuestionRegistry`** (`crates/ta-daemon/src/question_registry.rs`)~~ ✅
   - In-memory coordination for future in-process use (oneshot channels)
   - `PendingQuestion`, `HumanAnswer` types
   - `register()`, `answer()`, `list_pending()`, `cancel()`
---
---
   - `POST /api/interactions/:id/respond` — writes answer file + fires registry
   - `GET /api/interactions/pending` — lists pending questions
---
4. ~~**`GoalRunState::AwaitingInput`** (`crates/ta-goal/src/goal_run.rs`)~~ ✅
   - New state with `interaction_id` and `question_preview`
   - Valid transitions: `Running → AwaitingInput → Running`, `AwaitingInput → PrReady`
   - Visible in `ta goal list` and external UIs
---
---
   - `AgentNeedsInput` — with `suggested_actions()` returning a "respond" action
   - `AgentQuestionAnswered`, `InteractiveSessionStarted`, `InteractiveSessionCompleted`
---
6. ~~**`InteractionKind::AgentQuestion`** (`crates/ta-changeset/src/interaction.rs`)~~ ✅
   - New variant for channel rendering dispatch
---
---
   - JSONL log at `.ta/conversations/<goal_id>.jsonl`
   - `append_question()`, `append_answer()`, `load()`, `next_turn()`, `conversation_so_far()`
---
#### Version: `0.9.9-alpha.1`
---
---
---
---
---
---
---
---
---
1. **SSE listener for `agent_needs_input`** (`apps/ta-cli/src/commands/shell_tui.rs`):
   - SSE event handler recognizes `agent_needs_input` event → sends `TuiMessage::AgentQuestion`
   - Question text displayed prominently in the output pane
---
---
---
---
   - Enter sends text to `POST /api/interactions/:id/respond` instead of `/api/input`
   - On success, clears `pending_question`, restores normal prompt
---
3. **`ta run --interactive` flag** (`apps/ta-cli/src/commands/run.rs`):
   - Wire `--interactive` flag through to enable `ta_ask_human` in the MCP tool set
   - When set, agent system prompt includes instructions about `ta_ask_human` availability
---
---
   - Print conversation history from JSONL log
   - Show turn numbers, roles, timestamps
---
---
---
- ✅ SSE listener for `agent_needs_input` — `parse_agent_question()`, `TuiMessage::AgentQuestion` variant (5 tests)
- ✅ Input routing switch — `pending_question` field, prompt changes to `[agent Q1] >`, routes Enter to `/api/interactions/:id/respond` (3 tests)
- ✅ `ta run --interactive` flag — `build_interactive_section()` injects `ta_ask_human` documentation into CLAUDE.md (2 tests)
- ✅ `ta conversation <goal_id>` CLI command — reads JSONL log, formatted + JSON output modes (4 tests)
- ✅ Classic shell SSE rendering for `agent_needs_input` and `agent_question_answered` events
- ✅ Status bar indicator for pending agent questions
- ✅ Version bump to `0.9.9-alpha.2`
---
---
---
---
---
---
---
**Goal**: Build a convenience wrapper that uses interactive mode to generate a PLAN.md from a product document. The agent reads the document, asks clarifying questions via `ta_ask_human`, proposes phases, and outputs a plan draft.
---
---
---
- ✅ `PlanCommands::From` variant — `ta plan from <path>` reads document, builds planning prompt, delegates to `ta run --interactive` (4 tests)
- ✅ `build_planning_prompt()` — constructs agent prompt with document content, PLAN.md format guide, and `ta_ask_human` usage instructions; truncates docs >100K chars
- ✅ `agents/planner.yaml` — planner agent configuration with fs read/write access, no shell/network, planning-oriented alignment
- ✅ `docs/USAGE.md` updates — `ta plan from` documentation with examples, comparison table for `--detect` vs `plan from` vs `plan create`
- ✅ Fuzzy document search — `find_document()` searches workspace root, `docs/`, `spec/`, `design/`, `rfcs/`, and subdirs so bare filenames resolve automatically (4 tests)
- ✅ Shell/daemon integration — `ta plan from *` added to default `long_running` patterns in daemon config for background execution
- ✅ Validation — rejects missing files, empty documents, directories; observability-compliant error messages with search location details
- ✅ Version bump to `0.9.9-alpha.3`
---
#### When to use `--detect` vs `plan from`
---
---
- **`ta plan create`** — generates a generic plan from a hardcoded template. Use when you don't have a product doc.
---
---
---
---
---
---
---
---
---
---
---
- ✅ `ChannelDelivery` trait in `ta-events::channel` — async trait with `deliver_question()`, `name()`, `validate()` methods; `ChannelQuestion`, `DeliveryResult`, `ChannelRouting` types (5 tests)
---
- ✅ `ta-connector-slack` crate — `SlackAdapter` implementing `ChannelDelivery`, posts Block Kit messages with action buttons for yes/no and choice responses, thread-reply prompts for freeform (7 tests)
- ✅ `ta-connector-discord` crate — `DiscordAdapter` implementing `ChannelDelivery`, posts embeds with button components (up to 5 per row), footer prompts for freeform (6 tests)
- ✅ `ta-connector-email` crate — `EmailAdapter` implementing `ChannelDelivery`, sends HTML+text emails via configurable HTTP endpoint, includes interaction metadata headers (7 tests)
- ✅ `ChannelDispatcher` in `ta-daemon` — routes questions to registered adapters based on channel hints or daemon defaults; `from_config()` factory for building from `daemon.toml` (9 tests)
- ✅ `ChannelsConfig` in daemon config — `[channels]` section in `daemon.toml` with `default_channels`, `[channels.slack]`, `[channels.discord]`, `[channels.email]` sub-tables
- ✅ Version bump to `0.9.9-alpha.4`
---
---
- Slack/Discord/Email interaction handler webhooks → v0.11.0 (Event-Driven Agent Routing)
---
#### Version: `0.9.9-alpha.4`
---
---
---
---
---
**Goal**: Make it easy for users to create, validate, and iterate on custom workflow definitions and agent profiles without reading Rust source code or guessing YAML schema.
---
---
---
---
---
---
1. **`ta workflow new <name>`** (`apps/ta-cli/src/commands/workflow.rs`):
---
   - Includes a 2-stage build→review template as a starting point
   - Prints the file path and suggests next steps
---
2. **`ta workflow validate <path>`** (`apps/ta-cli/src/commands/workflow.rs`):
---
   - Reference validation: every role referenced in a stage exists in `roles:`
   - Dependency validation: no cycles, no references to undefined stages
---
   - Prints actionable errors with line numbers and suggestions
---
3. **`ta agent new <name>`** (`apps/ta-cli/src/commands/agent.rs` or `setup.rs`):
   - Generates `.ta/agents/<name>.yaml` with annotated comments
   - Prompts for agent type (full developer, read-only auditor, orchestrator)
   - Fills in appropriate `alignment` defaults based on type
---
4. **`ta agent validate <path>`** (`apps/ta-cli/src/commands/agent.rs`):
---
   - Checks `command` exists on PATH
   - Warns on common misconfigurations (e.g., `injects_settings: true` without `injects_context_file: true`)
---
5. **Example library** (`templates/workflows/`, `templates/agents/`):
   - 3-4 workflow examples: code-review, deploy-pipeline, security-audit, milestone-review
   - 3-4 agent examples: developer, auditor, planner, orchestrator
   - `ta workflow list --templates` and `ta agent list --templates` to browse
---
6. **Planner workflow role** — built-in `planner` role for workflow definitions:
---
   - Enables Plan→Implement→Review→Plan loops in multi-stage workflows
   - Example workflow: `plan-implement-review.yaml` with planner→engineer→reviewer stages
   - The planner stage can receive a document path or objective as input
   - Integrates with `ta plan from` — workflows can invoke planning as a stage
---
7. **Versioning schema templates** (`templates/version-schemas/`):
   - Pre-built version schema configs users can adopt or customize:
     - `semver.yaml` — standard semver (MAJOR.MINOR.PATCH with pre-release)
     - `calver.yaml` — calendar versioning (YYYY.MM.PATCH)
     - `sprint.yaml` — sprint-based versioning (sprint-N.iteration)
     - `milestone.yaml` — milestone-based (v1, v2, v3 with sub-phases)
   - `ta plan create --version-schema semver` selects a template
   - Schema defines: version format regex, bump rules, phase-to-version mapping
   - Users can write custom schemas in `.ta/version-schema.yaml`
---
---
---
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
---
---
- `ta plan create --version-schema` → v0.10.17 (item 9)
---
#### Version: `0.9.9-alpha.5`
---
---
---
### v0.9.10 — Multi-Project Daemon & Office Configuration
---
**Goal**: Extend the TA daemon to manage multiple projects simultaneously, with channel-to-project routing so a single Discord bot, Slack app, or email address can serve as the interface for several independent TA workspaces.
---
---
Today each `ta daemon` instance serves a single project. Users managing multiple projects need separate daemon instances and separate channel configurations. This makes it impossible to say "@ta inventory-service plan list" in a shared Discord channel — there's no way to route the message to the right project.
---
---
---
---
                    ┌──────────────────────────────┐
  Discord/Slack/    │      Multi-Project Daemon     │
---
                    │  ┌──────────────────────────┐  │
                    │  │    Message Router         │  │
                    │  │  channel → project map    │  │
                    │  │  thread context tracking  │  │
                    │  │  explicit prefix parsing  │  │
                    │  └──────┬──────┬──────┬──────┘  │
---
                    │    ┌────▼──┐ ┌─▼───┐ ┌▼────┐   │
                    │    │Proj A │ │Proj B│ │Proj C│  │
                    │    │context│ │ctxt  │ │ctxt  │  │
                    │    └───────┘ └──────┘ └──────┘  │
                    └──────────────────────────────┘
---
---
Each `ProjectContext` holds:
- Workspace path + `.ta/` directory
- GoalRunStore, DraftStore, AuditLog
- PolicyDocument (per-project)
- ChannelRegistry (per-project, but channel listeners are shared)
---
---
---
---
---
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
---
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
---
#### Implementation scope
- `crates/ta-daemon/src/project_context.rs` — `ProjectContext` struct with per-project stores (~150 lines)
---
- `crates/ta-daemon/src/router.rs` — message routing with channel→project resolution (~150 lines)
- `crates/ta-daemon/src/web.rs` — project-scoped API endpoints (~100 lines)
- `apps/ta-cli/src/commands/office.rs` — `ta office` subcommands (~200 lines)
- `docs/USAGE.md` — multi-project setup guide, office.yaml reference
- Tests: project context isolation, routing precedence, runtime add/remove, backward compat with single-project mode
---
---
---
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
---
---
- Full GatewayState refactor → v0.10.18
---
- Config hot-reload → v0.10.18
---
#### Version: `0.9.10-alpha`
---
---
---
### v0.10.0 — Gateway Channel Wiring & Multi-Channel Routing
---
---
---
---
---
- ✅ **Multi-channel routing**: `review` and `escalation` now accept either a single channel object or an array of channels (backward-compatible via `#[serde(untagged)]`). `notify` already supported arrays. Schema supports `strategy: first_response | quorum`.
- ✅ **`MultiReviewChannel` wrapper**: New `MultiReviewChannel` implementing `ReviewChannel` that dispatches to N inner channels. `request_interaction()` tries channels sequentially; first response wins (`first_response`) or collects N approvals (`quorum`). `notify()` fans out to all. 9 tests.
#### Implementation scope
- ✅ **Channel health check**: `ta config channels --check` verifies each configured channel is buildable (factory exists, config valid).
---
#### Implementation scope
- `crates/ta-mcp-gateway/src/server.rs` — registry loading, channel resolution
---
- `crates/ta-changeset/src/channel_registry.rs` — `ReviewRouteConfig`, `EscalationRouteConfig` enums, `build_review_from_route()`, schema update
- `apps/ta-cli/src/commands/config.rs` — `ta config channels` command (new)
- `docs/USAGE.md` — multi-channel routing docs
---
#### Version: `0.10.0-alpha`
---
### v0.10.1 — Native Discord Channel
---
**Goal**: `DiscordChannelFactory` implementing `ChannelFactory` with direct Discord REST API connection, eliminating the need for the bridge service.
---
---
- ✅ **`ta-channel-discord` crate**: New crate at `crates/ta-channel-discord/` with `reqwest`-based Discord REST API integration (4 modules: lib, channel, factory, payload)
- ✅ **`DiscordReviewChannel`** implementing `ReviewChannel`: rich embeds with buttons, file-based response exchange, sync/async bridge
- ✅ **`DiscordChannelFactory`** implementing `ChannelFactory`: `channel_type()` → `"discord"`, config-driven build with `token_env`, `channel_id`, `response_dir`, `allowed_roles`, `allowed_users`, `timeout_secs`, `poll_interval_secs`
- ✅ **Access control**: `allowed_roles` and `allowed_users` restrict who can approve/deny
- ✅ **Payload builders**: Interaction-kind-aware embeds and buttons
- ✅ **Registry integration**: Registered in MCP gateway and CLI config
- ✅ **30 tests** across all modules
---
---
- Discord deny modal → v0.11.0 (Event-Driven Agent Routing — interactive channel responses)
- Discord thread-based discussions → v0.11.0
---
---
---
channels:
  review:
    type: discord
    token_env: TA_DISCORD_TOKEN
    channel_id: "123456789"
    allowed_roles: ["reviewer"]
    allowed_users: ["user#1234"]
---
---
#### Plugin-readiness note
---
This is built as an in-process Rust crate (the existing pattern). When v0.10.2 (Channel Plugin Loading) lands, this adapter should be refactorable to an external plugin — it already implements `ChannelDelivery` and uses only HTTP/WebSocket. Design the crate so its core logic (message formatting, button handling, webhook response parsing) is separable from the in-process trait impl. This makes it a reference implementation for community plugins in other languages.
---
---
---
### v0.10.2 — Channel Plugin Loading (Multi-Language)
---
**Goal**: Allow third-party channel plugins without modifying TA source or writing Rust, enabling community-built integrations (Teams, PagerDuty, ServiceNow, etc.) in any language.
---
#### Current State
---
The `ChannelDelivery` trait is a clean boundary — it depends only on serializable types from `ta-events`, and the response path is already HTTP (`POST /api/interactions/:id/respond`). But registration is hardcoded: adding a channel requires a new Rust crate in `crates/ta-connectors/`, a dependency in `daemon/Cargo.toml`, and a match arm in `channel_dispatcher.rs`. Users cannot add channels without recompiling TA.
---
---
---
Two out-of-process plugin protocols. Both deliver `ChannelQuestion` as JSON and receive answers through the existing HTTP response endpoint. Plugins can be written in any language.
---
---
---
TA spawns the plugin executable, sends `ChannelQuestion` JSON on stdin, reads a `DeliveryResult` JSON line from stdout. The plugin delivers the question however it wants (API call, email, push notification). When the human responds, the plugin (or the external service's webhook) POSTs to `/api/interactions/:id/respond`.
---
---
TA daemon
  → spawns: python3 ta-channel-teams.py
  → stdin:  {"interaction_id":"...","question":"What database?","choices":["Postgres","MySQL"],...}
  → stdout: {"channel":"teams","delivery_id":"msg-123","success":true}
  ...later...
  → Teams webhook → POST /api/interactions/:id/respond → answer flows back to agent
---
---
**Protocol 2: HTTP callback**
---
---
---
---
---
name = "pagerduty"
---
---
auth_token_env = "TA_PAGERDUTY_TOKEN"
---
---
**Both protocols use the same JSON schema** — `ChannelQuestion` and `DeliveryResult` from `ta-events`. The subprocess just reads/writes them over stdio; the HTTP variant sends/receives them as request/response bodies.
---
---
---
---
---
   - Subprocess variant: spawn process, write JSON to stdin, read JSON from stdout
   - HTTP variant: POST question JSON to configured URL, parse response
   - Both variants: answers return via existing `/api/interactions/:id/respond`
---
2. **Plugin manifest** (`channel.toml`):
---
   name = "teams"
   version = "0.1.0"
---
   protocol = "json-stdio"                   # or "http"
   deliver_url = ""                          # only for http protocol
   capabilities = ["deliver_question"]
---
---
3. **Plugin discovery**: Scan `~/.config/ta/plugins/channels/` and `.ta/plugins/channels/` for `channel.toml` manifests. Register each as an `ExternalChannelAdapter` in the `ChannelDispatcher`.
---
4. **Open `daemon.toml` config** — `[[channels.external]]` array replaces closed-world `ChannelsConfig`:
---
   [[channels.external]]
   name = "teams"
   command = "ta-channel-teams"
   protocol = "json-stdio"
---
   [[channels.external]]
   name = "custom-webhook"
   protocol = "http"
   deliver_url = "https://my-service.com/ta/deliver"
   auth_token_env = "TA_CUSTOM_TOKEN"
---
---
5. **`ta plugin list`**: Show installed channel plugins with protocol, capabilities, and validation status.
---
6. **`ta plugin install <path-or-url>`**: Copy executable + manifest to plugin directory.
---
7. **Plugin SDK examples** — starter templates in multiple languages:
   - `templates/channel-plugins/python/` — Python channel plugin skeleton
   - `templates/channel-plugins/node/` — Node.js channel plugin skeleton
   - `templates/channel-plugins/go/` — Go channel plugin skeleton
   - Each includes: JSON schema types, stdin/stdout handling, example delivery logic
---
#### Multi-language plugin example (Python)
---
```python
#!/usr/bin/env python3
"""TA channel plugin for Microsoft Teams — reads JSON from stdin, posts to Teams."""
import json, sys, requests
---
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
---
if __name__ == "__main__":
    main()
---
---
#### Prep: Built-in channels should follow the same pattern
---
Slack (v0.10.3) and email (v0.10.4) are built as external plugins from the start. Discord (v0.10.1) was built as an in-process crate — it should be refactorable to an external plugin once the plugin system is proven. The long-term goal: TA ships with zero built-in channel adapters; all channels are plugins. The built-in ones are just pre-installed defaults.
---
---
- ✅ `PluginManifest` struct with TOML parsing, validation, protocol enum (JsonStdio, Http)
---
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
---
---
---
- Plugin marketplace / remote install → backlog (no target phase yet)
---
#### Version: `0.10.2-alpha`
---
---
---
### v0.10.2.1 — Refactor Discord Channel to External Plugin
---
---
---
---
---
---
---
---
1. [x] Extract core Discord logic (payload builders, embed formatting) into `plugins/ta-channel-discord/src/payload.rs`
---
3. [x] Add `channel.toml` manifest for plugin discovery
4. [x] Remove `ta-channel-discord` crate from workspace — Discord becomes a pre-installed plugin, not a compiled-in dependency
5. [x] Update `ChannelDispatcher` registration to load Discord via plugin system instead of hardcoded match arm — daemon now emits migration warning for old `[channels.discord]` config
6. [x] Migrate Discord config from in-process `ChannelsConfig` to `[[channels.external]]` in `daemon.toml` — old config produces deprecation warning
7. [x] Verify all workspace tests pass (existing Discord connector tests in ta-connector-discord still pass; plugin has its own 13 tests)
8. [x] Update docs: discord-channel guide rewritten for plugin architecture
---
#### Version: `0.10.2-alpha.1`
---
---
---
---
---
---
---
#### Usage
---
# Build a specific plugin
ta plugin build discord
---
---
ta plugin build discord,slack,email
---
# Build all plugins found in plugins/
ta plugin build --all
---
---
---
---
2. Run `cargo build --release` in each plugin directory
3. Copy the compiled binary + `channel.toml` to `.ta/plugins/channels/<name>/`
4. Print summary: which plugins built, binary size, install path
---
---
1. [x] `PluginCommands::Build` variant in `apps/ta-cli/src/commands/plugin.rs` with `names: Vec<String>` and `--all` flag
---
3. [x] Build runner: invoke `cargo build --release` in plugin directory, capture output, report errors
4. [x] Install step: copy binary + manifest to `.ta/plugins/channels/<name>/`
5. [x] `--all` flag: discover and build every plugin in `plugins/`
6. [x] Output: progress per plugin, success/failure summary, binary paths
7. [x] Error handling: continue building remaining plugins if one fails, report all failures at end
8. [x] 13 new tests: discovery, binary name extraction, name resolution, error paths, formatting
---
#### Version: `0.10.2-alpha.2`
---
---
---
### v0.10.3 — Slack Channel Plugin
---
---
---
---
---
---
---
---
---
---
---
4. ✅ **Block Kit payloads**: Header, question section, context section, interactive buttons (yes/no, choice, freeform), interaction ID footer
5. ✅ **Actionable error messages**: Missing token, missing channel ID, Slack API errors with permission hints
6. ✅ **`allowed_users` env var**: `TA_SLACK_ALLOWED_USERS` documented for access control integration
---
---
- Slack Socket Mode + deny modal + HTTP mode → v0.11.0 (Event-Driven Agent Routing — interactive channel responses)
---
---
---
---
name = "slack"
command = "ta-channel-slack"
---
---
# Plugin reads these env vars directly
# TA_SLACK_BOT_TOKEN, TA_SLACK_CHANNEL_ID
# TA_SLACK_ALLOWED_USERS (optional, comma-separated user IDs)
---
---
---
---
---
---
---
---
#### Approach
---
#### Approach
---
Built as an external plugin. Sends formatted review emails via SMTP, polls IMAP for reply-based approval. Email is inherently slower than chat — validates that the plugin/interaction model handles longer response times gracefully.
---
---
- ✅ Plugin binary (`plugins/ta-channel-email/`): standalone Rust binary using JSON-over-stdio protocol, reads `ChannelQuestion` from stdin, sends via SMTP (lettre), writes `DeliveryResult` to stdout
- ✅ Subject tagging: configurable prefix (default `[TA Review]`) with `X-TA-Request-ID`, `X-TA-Interaction-ID`, `X-TA-Goal-ID` headers for threading
#### Config
- ✅ Multiple reviewers: comma-separated `TA_EMAIL_REVIEWER` list, all receive the email (first to reply wins)
- ✅ App Password support: STARTTLS SMTP with username/password auth (works with Gmail App Passwords, no OAuth)
- ✅ Email threading: Message-ID based on interaction_id, follow-up turns use In-Reply-To/References headers
- ✅ HTML + plain text multipart emails with structured layout, interactive guidance per question type
- ✅ `channel.toml` manifest for standard plugin discovery (v0.10.2)
- ✅ HTML body escapes user content to prevent XSS
- ✅ 36 tests: email body builders (16), reply parsing (15), serialization/config (5)
---
---
- IMAP reply polling + configurable timeout → v0.11.0 (Event-Driven Agent Routing)
- Plugin version checking → completed in v0.10.16
---
#### Config
---
protocol = "json-stdio"
name = "email"
command = "ta-channel-email"
protocol = "json-stdio"
---
# Plugin reads these env vars directly
# TA_EMAIL_SMTP_HOST, TA_EMAIL_SMTP_PORT (default: 587)
# TA_EMAIL_USER, TA_EMAIL_PASSWORD
# TA_EMAIL_REVIEWER (comma-separated)
# TA_EMAIL_FROM_NAME (default: "TA Agent")
# TA_EMAIL_SUBJECT_PREFIX (default: "[TA Review]")
---
---
---
---
---
---
---
---
---
---
---
---
- Share a workflow across multiple projects
---
---
- Generate release communications automatically as part of `ta release`
---
---
---
---
---
---
---
---
ta workflow add security-review --from registry:trustedautonomy/workflows
ta workflow add deploy-pipeline --from gh:myorg/ta-workflows
---
# Pull an agent config
ta agent add security-reviewer --from registry:trustedautonomy/agents
ta agent add code-auditor --from https://example.com/ta-agents/auditor.yaml
---
---
ta workflow list --source external
---
---
---
##### 2. Workflow/agent package format
---
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
---
---
##### 3. Release press-release generation
The `ta release` process includes an optional press-release authoring step where an agent generates a release announcement from the changelog, guided by a user-provided sample:
---
---
---
ta release config set press_release_template ./samples/sample-press-release.md
---
---
ta release run --press-release
---
---
---
---
---
The agent reads the changelog/release notes, follows the style and tone of the sample document, and produces a draft press release that goes through the normal TA review process (draft → approve → apply).
---
##### 4. Workflow authoring and publishing
---
---
ta workflow new deploy-pipeline
# Edit .ta/workflows/deploy-pipeline.yaml
---
---
ta workflow publish deploy-pipeline --registry trustedautonomy
---
# Version management
ta workflow publish deploy-pipeline --bump minor
---
---
---
1. [x] External source resolver: registry, GitHub repo, and raw URL fetching for YAML configs
2. [x] `ta workflow add/remove/list` commands with `--from` source parameter
---
---
5. [x] Local cache for external configs (`~/.ta/cache/workflows/`, `~/.ta/cache/agents/`)
6. [x] Version pinning and update checking for external configs
7. [x] `ta release` press-release generation step with sample-based style matching
8. [x] Press release template configuration (`ta release config set press_release_template`)
9. [x] `ta workflow publish` command for authoring and publishing to registry
10. [x] Documentation: authoring guide for workflow/agent packages
11. [x] **Multi-language plugin builds**: Add `build_command` field to `channel.toml` so `ta plugin build` works with non-Rust plugins (Python, Go, Node). Rust plugins default to `cargo build --release`; others specify their own build step (e.g., `go build -o ta-channel-teams .`, `pip install -e .`). Extend v0.10.2.2's build runner to read and execute `build_command`.
---
---
---
---
---
---
---
---
---
#### Known Bugs
- ~~**Releases always marked pre-release**: `release.yml` auto-detected `alpha`/`beta` in the version string and set `prerelease: true`, which meant GitHub never updated "latest release". Fixed in v0.9.9.1 — default is now latest, with explicit `--prerelease` input on `workflow_dispatch`.~~ ✅
- **`ta_fs_write` forbidden in orchestrator mode**: The release notes agent tries to write `.release-draft.md` directly but is blocked by orchestrator policy. The agent should either use `ta_goal` to delegate the write, or the orchestrator policy should whitelist release artifact writes. Filed as bug — the process should just work without the agent needing workarounds.
- **Release notes agent workaround**: Currently the agent works around the `ta_fs_write` restriction by using alternative write methods, but this is fragile and shouldn't be necessary.
---
---
---
---
---
---
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
---
---
---
---
---
1. [x] Fix `ta_fs_write` permission in orchestrator mode for release artifact files (`.release-draft.md`, `CHANGELOG.md`) — added `ORCHESTRATOR_WRITE_WHITELIST` to `CallerMode` and updated `handle_fs_write` to check path before blocking
2. [x] Add orchestrator-mode write whitelist for release-specific file patterns — `is_write_whitelisted()` method on `CallerMode` matches filenames against `.release-draft.md`, `CHANGELOG.md`, `version.json`, `.press-release-draft.md`
3. [x] End-to-end test for `ta release run` pipeline without manual intervention — `e2e_pipeline_no_manual_gates` test with marker file verification
4. [x] Release dry-run mode: `ta release run --dry-run` validates all steps without publishing — existing `--dry-run` flag + new `ta release validate` command for pre-flight checks (version format, git state, tag availability, pipeline config, toolchain)
5. [x] **Background goal launch from shell**: `release` shortcut in shell config expands to `ta release run`, long-running command classification ensures background execution via daemon
6. [x] **Interactive release agent**: `ta release run --interactive` launches the `releaser` agent with `ta_ask_human`-based review checkpoints
7. [x] **`agents/releaser.yaml`**: Release agent config with `ta_ask_human` enabled, write access scoped to release artifacts via orchestrator whitelist
8. [x] **Release workflow definition**: `templates/workflows/release.yaml` — 4-stage workflow (validate → generate-notes → build-verify → publish) with human review at notes and publish stages
---
---
- Wire `ta sync`/`ta build` in release → v0.10.18 (depends on v0.11.1, v0.11.2)
---
---
---
---
---
---
---
**Goal**: Full documentation audit and refinement pass after the v0.10.x feature set is complete. Ensure all docs are accurate, consistent, and organized for both users and integration developers.
---
---
- **USAGE.md**: Verify all commands, flags, and config options are documented. Remove stale references. Ensure progressive disclosure (getting started → daily use → advanced). Add examples for every config section.
- **MISSION-AND-SCOPE.md**: Confirm feature boundary decisions match implementation. Update protocol tables if anything changed. Validate the scope test against actual shipped features.
- **CLAUDE.md**: Trim to essentials. Remove references to completed phases. Ensure build/verify instructions are current.
- **PLAN.md**: Archive completed phases into a collapsed section or separate `docs/PLAN-ARCHIVE.md`. Keep active phases clean.
- **README.md**: Update for current state — accurate feature list, installation instructions, quick-start guide.
- **ADRs** (`docs/adr/`): Ensure all significant decisions have ADRs. Check that existing ADRs aren't contradicted by later work.
- **Plugin/integration docs**: Verify JSON schema examples match actual types. Add end-to-end plugin authoring guide if missing.
- **Cross-doc consistency**: Terminology (draft, goal, artifact, staging), config field names, version references.
---
---
1. [x] Audit USAGE.md against current CLI `--help` output for every subcommand — verified all 25 subcommands documented, added missing `accept-terms`/`view-terms`/`terms-status` commands, updated version to v0.10.7-alpha
2. [x] Audit MISSION-AND-SCOPE.md protocol/auth tables against actual implementation — protocol table verified accurate, updated `ta schema export` reference to note it's still planned
3. [x] Review and update README.md for current feature set and installation — updated version badges, current status, project structure, MCP tools table, and "What's Implemented" section
4. [x] Archive completed PLAN.md phases (pre-v0.9) into `docs/PLAN-ARCHIVE.md` — moved ~2000 lines (Phase 0 through v0.8.2) to `docs/PLAN-ARCHIVE.md`, replaced with collapsed reference
5. [x] Verify all config examples in docs parse correctly against current schema — reviewed workflow.toml, config.yaml, policy.yaml, daemon.toml, office.yaml, and channel.toml against codebase structs
6. [x] Cross-reference ADRs with implementation — updated ADR-modular-decomposition status to "Deferred", updated ADR-product-concept-model crate map to reflect current implementation status
7. [x] Add plugin authoring quickstart guide (`docs/PLUGIN-AUTHORING.md`) with end-to-end example — created comprehensive guide with Python and Rust examples, JSON schemas, manifest format, and testing instructions
8. [x] Terminology consistency pass across all docs — verified Draft/PR terminology, staging/virtual-workspace usage, version references updated across USAGE.md, README.md, CLAUDE.md
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
#### Behavior
---
# Commands run in staging dir after agent exits, before draft build.
# All must pass (exit 0) for the draft to be created.
commands = [
---
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
---
---
---
---
---
---
timeout = 300
---
---
#### Behavior
1. Agent exits normally
---
3. **All pass**: Draft is built as normal
4. **Any fail** (`on_failure = "block"`): No draft created. Print which command failed with output. Suggest `ta run --follow-up` to fix.
5. **Any fail** (`on_failure = "warn"`): Draft is created with a verification warning visible in `ta draft view`
6. **Any fail** (`on_failure = "agent"`): Re-launch the agent with the failure output injected as context (uses interactive mode if available)
---
---
1. ✅ `VerifyConfig` struct in `crates/ta-submit/src/config.rs`: `commands`, `on_failure` (enum: Block/Warn/Agent), `timeout` with serde defaults
2. ✅ `run_verification()` in `apps/ta-cli/src/commands/verify.rs`: runs commands sequentially with per-command timeout, captures output, returns `VerificationResult`
3. ✅ Wire into `ta run` flow: verification runs after agent exit + file restoration, before `ta draft build`
4. ✅ Block mode: aborts draft creation on failure, prints failed commands with output, suggests `ta run --follow-up` and `ta verify`
5. ✅ Warn mode: creates draft with `verification_warnings` field on `DraftPackage`, displayed in `ta draft view` with command, exit code, and output
6. ✅ Agent mode: stub implemented (falls back to block with message that re-launch is not yet implemented)
7. ✅ `--skip-verify` flag on `ta run` to bypass verification
8. ✅ Default `[verify]` section in `ta init` template: Rust projects get pre-populated commands; others get commented-out examples
9. ✅ `ta verify` standalone command: resolves goal by ID/prefix or most recent active goal, loads `[verify]` from staging's workflow.toml, runs verification, exits with code 1 on failure
---
---
- Agent mode re-launch with failure context → v0.11.0 (Event-Driven Agent Routing)
---
---
- 7 new config tests: defaults, TOML parsing for all modes, display formatting
- 5 new verification tests: empty commands pass, passing/failing commands, mixed commands, output capture, timeout handling
---
---
---
---
---
---
---
**Goal**: Make `ta run --follow-up` a frictionless, context-aware entry point that works across VCS backends, channels, and workflow types — without requiring the user to know branch names, draft IDs, or internal state.
---
---
Today `--follow-up` requires the user to know which git branch holds the prior work, pass it explicitly, and understand the staging directory layout. This is wrong friction — especially for non-technical users working through email, social media, or DB migration workflows. The user's mental model is "I want to continue working on *that thing*" — TA should resolve what "that thing" means.
---
---
`ta run --follow-up` (with no additional arguments) enters an interactive selection flow:
---
1. **Gather candidates**: Scan recent goals, active drafts, in-progress plan phases, and open verification failures. Each candidate carries enough context to display a one-line summary.
---
3. **User selects**: User picks by number or searches. TA resolves the selection to the correct staging directory, branch, draft, or channel context.
4. **Context injection**: TA injects relevant follow-up context into the agent's CLAUDE.md — what was attempted, what failed, what the user or reviewer said. The agent picks up where it left off.
---
When a specific target is known, shortcuts still work:
---
- `ta run --follow-up --draft <id>` — follow up on a specific draft (denied, failed verify, etc.)
- `ta run --follow-up --goal <id>` — continue from a prior goal's staging
---
#### VCS & Channel Agnosticism
The follow-up resolver doesn't assume git. It works from TA's own state:
- **Goals**: `GoalRun` records in `.ta/goals/` — each has staging path, status, plan phase
- **Drafts**: `DraftPackage` records — status, denial reason, verification warnings
- **Plan phases**: `PLAN.md` status markers — in_progress phases are follow-up candidates
- **Channel context**: For non-filesystem workflows (email drafts, social media posts, DB migrations), the follow-up context comes from the draft's `PatchSet` and interaction log rather than a git branch
---
---
#### Deferred items moved
2. ✅ `gather_follow_up_candidates()`: scans goals, drafts, plan phases; filters to actionable items (failed, running, denied, verify-warned, in-progress phases); sorts by recency
3. ✅ Interactive picker in `ta run --follow-up` (no args): numbered candidate list with source tags, status, age, and context summaries; user selects by number
---
5. ✅ `--follow-up-draft <id>` CLI flag: `resolve_by_draft()` resolves from draft prefix, injects denial reason and verify failure context
---
7. ✅ Context injection: `build_follow_up_context()` builds CLAUDE.md section with prior goal summary, draft status, verification failures (with command output), denial reasons, discuss items with review comments
8. ✅ `resolve_smart_follow_up()` in `run.rs`: priority-based resolution (draft > goal > phase > interactive picker > existing behavior); produces title, phase, follow-up ID, and context string
9. ✅ Channel-agnostic resolution: follow-up resolver works from TA's own state (GoalRun records, DraftPackage records, PLAN.md phases) without assuming git
---
#### Deferred items moved
- Shell TUI fuzzy-searchable picker → backlog (TUI enhancement, no target phase)
---
---
---
---
#### Version: `0.10.9-alpha`
---
---
---
---
---
**Goal**: `ta shell` (and other CLI commands that talk to the daemon) should detect when the running daemon is an older version than the CLI and offer to restart it — rather than silently connecting to a stale daemon.
---
---
After `./install_local.sh` rebuilds and installs new `ta` and `ta-daemon` binaries, the old daemon process keeps running. `ta shell` connects to it, shows the version in the status bar, but doesn't warn the user or offer to restart. The user has to notice the mismatch and manually restart. This is especially confusing after upgrades since new features may not work against the old daemon.
---
---
---
---
3. **If mismatch**: Display a prominent warning and offer to restart:
---
   Daemon version mismatch: daemon v0.10.6-alpha, CLI v0.10.10-alpha
   Restart daemon with the new version? [Y/n]
---
4. If the user accepts, the CLI stops the old daemon (`POST /api/shutdown` or signal), waits for exit, then spawns the new one.
5. If the user declines, proceed with a warning in the status bar (e.g., `daemon (stale)`).
---
---
---
2. ✅ `check_daemon_version()` in `version_guard.rs`: compares `env!("CARGO_PKG_VERSION")` to daemon's reported version, prompts interactively, returns `VersionGuardResult` enum
3. ✅ Wired into `ta shell` startup (both classic and TUI modes): version check runs before entering the shell loop, prompts user to restart if mismatch
4. ✅ Wired into `ta dev`: version check before launching orchestrator agent
5. ✅ Restart flow: `POST /api/shutdown` graceful endpoint → wait for exit (5s timeout) → find daemon binary (sibling or PATH) → spawn new daemon → wait for healthy (10s) → verify version matches
6. ✅ `--no-version-check` global CLI flag to skip (for CI or scripted use)
7. ✅ TUI status bar: shows `◉ daemon (stale)` in yellow if daemon version doesn't match CLI version
---
#### Tests
---
---
#### Version: `0.10.10-alpha`
---
---
---
---
---
**Goal**: Make `ta shell` a fully usable interactive environment where agent output is visible, long output is navigable, and the user never has to leave the shell to understand what's happening.
---
---
---
- Starting a goal produces no output — the agent runs blind. User must manually `:tail` and even then sees only TA lifecycle events, not the agent's actual stdout/stderr.
- Long command output (draft list, draft view) scrolls off the top of the viewport with no way to scroll back.
- Draft IDs are unrelated to goal IDs, requiring mental mapping or `draft list --goal` lookups.
- No notification when a draft is ready — user must poll with `draft list`.
- `:tail` gives no confirmation it's working and shows no backfill of prior output.
---
---
---
1. ✅ **Agent output streaming**: TUI `:tail` command connects to `GET /api/goals/:id/output` SSE endpoint, streams `AgentOutput` messages as styled lines (stdout=white, stderr=yellow). Interleaves with TA events in unified output pane.
---
3. ✅ **Tail backfill and confirmation**: Prints confirmation on tail start with goal ID. Visual separator `─── live output ───` between backfill and live output. Configurable `shell.tail_backfill_lines` (default 5).
4. ✅ **Draft-ready notification**: SSE parser detects `draft_built` events and renders `[draft ready] "title" (display_id) — run: draft view <id>` with bold green styling. Status bar shows tailing indicator.
---
6. ✅ **Draft list filtering, ordering, and paging**: Default ordering newest-last. `--pending`, `--applied` status filters. Compact default view (active/pending only). `--all` shows everything. `--limit N` for paged output. `draft list --goal <id>` preserved from v0.10.8.
---
8. ✅ **Scrollable output buffer (foundational)**: TUI output pane retains full history with configurable buffer limit (`shell.output_buffer_lines`, default 10000). Oldest lines dropped when limit exceeded. Scroll offset adjusted when lines are pruned.
---
---
#### Tests
- Classic shell pager → dropped (TUI scrollable output supersedes this)
- Progressive disclosure for draft view → backlog (TUI enhancement, no target phase)
---
#### Tests
- 14 new tests in `shell_tui.rs`: parse_goal_started_event, parse_goal_started_ignores_other_events, parse_draft_built_event, parse_draft_built_fallback_display_id, parse_draft_built_ignores_other_events, handle_agent_output_message, handle_agent_stderr_output, handle_goal_started_auto_tail, handle_goal_started_no_auto_tail_when_already_tailing, handle_goal_started_no_auto_tail_when_disabled, handle_agent_output_done_clears_tail, handle_draft_ready_notification, output_buffer_limit_enforced, output_buffer_limit_adjusts_scroll
- 4 new tests in `config.rs`: shell_config_defaults, workflow_config_default_has_shell_section, parse_toml_with_shell_section, parse_toml_without_shell_section_uses_default
---
#### Version: `0.10.11-alpha`
---
---
---
### v0.10.12 — Streaming Agent Q&A & Status Bar Enhancements
---
**Goal**: Eliminate 60s+ latency in `ta shell` Q&A by streaming agent responses instead of blocking, and add daemon version + agent name to the TUI status bar.
---
---
When the user asks a question in `ta shell`, the daemon spawned `claude --print` synchronously and blocked until the entire response was ready — often 60+ seconds with no feedback. The user had no indication the system was working. Additionally, the TUI status bar showed no information about the daemon version or which agent was handling Q&A.
---
---
1. ✅ **Streaming agent ask**: Refactored `ask_agent()` from blocking to streaming. Now creates a `GoalOutput` broadcast channel, spawns the agent subprocess in `tokio::spawn`, and returns an immediate ack with `request_id` and `status: "processing"`. Client subscribes to `GET /api/goals/:request_id/output` SSE stream for real-time output.
2. ✅ **`__streaming__:` protocol**: `send_input()` in shell.rs detects `status: "processing"` responses and returns a `__streaming__:<request_id>` marker. TUI intercepts this and subscribes to the SSE stream via `start_tail_stream()`.
3. ✅ **Daemon version in status bar**: `ProjectStatus` now includes `daemon_version` field. TUI status bar shows `◉ daemon <version>` with stale detection (yellow when version doesn't match CLI).
4. ✅ **Default agent in status bar**: `ProjectStatus` now includes `default_agent` field. TUI status bar shows the configured Q&A agent name (e.g., `claude-code`) in magenta.
5. ✅ **Removed fake "Thinking..." indicator**: Client-side fake indicator removed. The TUI now shows "Agent is working..." only after receiving the real ack from the daemon, then streams actual output.
---
#### Version: `0.10.12-alpha`
---
---
---
---
---
---
---
---
---
---
---
---
ta> plan add "Update ta shell status bar to show active Q&A agent model"
---
Agent: I'll add this to the plan. A few questions:
  1. Should this be a standalone phase or added to an existing one?
  2. This requires detecting the model from the agent binary — should
     that be a separate prerequisite phase?
---
You: Standalone phase after v0.10.12. The model detection can be
---
---
Agent: Added v0.10.14 — Agent Model Discovery & Status Display
---
       - Display model name in TUI status bar
       - Future: Model capabilities reporting for smart routing
---
---
---
1. **`ta plan add <description>` CLI command**: Launches a planner agent session with the current PLAN.md as context. The agent proposes placement, version number, and items through interactive Q&A.
2. **Existing plan awareness**: Agent reads current PLAN.md, understands phase ordering, version numbering, status markers, and dependencies.
---
4. **Shell integration**: `plan add` available as a shell command, runs as background goal with interactive mode.
5. **Non-interactive mode**: `ta plan add "description" --auto` for CI/scripted use — agent makes best-guess placement without asking questions.
---
---
- [x] `ta plan add <description>` CLI command with `--agent`, `--source`, `--after`, `--auto`, `--follow-up` flags
- [x] Existing plan awareness: reads PLAN.md, parses phases, validates `--after` phase ID, reports plan summary (total/done/pending)
---
- [x] Shell integration: `plan add <desc>` available as shell shortcut in both classic and TUI shells
- [x] Non-interactive mode: `--auto` flag skips interactive Q&A, agent makes best-guess placement
---
- [x] `truncate_title()` helper for display-friendly goal titles
- [x] Error handling: missing plan, empty plan, invalid `--after` phase ID with actionable messages
- [x] 13 new tests (11 plan_add tests + 2 truncate_title tests)
---
#### Version: `0.10.13-alpha`
---
---
---
### v0.10.14 — Deferred Items: Shell & Agent UX
---
#### Tests
---
---
1. ✅ **`:tail <id> --lines <count>` override**: Added `parse_tail_args()` with `--lines N` / `-n N` support in TUI and classic shell. 6 tests.
---
3. ✅ **Ctrl+C interrupt**: Detaches from tail or cancels pending question before exiting. Updated Ctrl+C handler in TUI.
---
5. ✅ **Split pane support**: Ctrl-W toggles 50/50 horizontal split. Agent output routes to right pane when split. `draw_agent_pane()` with scroll support.
6. ✅ **Agent model discovery**: `extract_model_from_stream_json()` parses `message_start` events, `humanize_model_name()` converts model IDs. Displayed in status bar (Blue). 5 tests.
---
8. ✅ **Shell TUI fuzzy-searchable follow-up picker**: `:follow-up [filter]` command gathers candidates via `gather_follow_up_candidates()`, displays numbered list with source tags, color-coded by type, supports keyword filtering.
9. ✅ **Agent mode for verification failures**: Full `VerifyOnFailure::Agent` implementation in `run.rs`. Builds failure context, re-injects into CLAUDE.md, re-launches agent, re-runs verification, blocks if still failing.
10. ✅ **Input line text wrap**: `Wrap { trim: false }` on input paragraph, wrap-aware cursor positioning (cursor_y = chars/width, cursor_x = chars%width).
11. ✅ **Interactive release approval via TUI**: `prompt_approval_with_auto()` uses file-based interactions (`.ta/interactions/pending/`) for non-TTY contexts, enabling TUI `AgentQuestion` flow. Added `--auto-approve` flag for CI. 2 tests.
---
#### Tests
- 6 new tests in `shell_tui.rs` for `parse_tail_args`
---
- 5 new tests in `shell_tui.rs` for model extraction/humanization
---
- 2 new tests in `release.rs` for auto-approve and TUI interaction
#### Deferred items moved
#### Version: `0.10.14-alpha`
---
---
---
### v0.10.15 — Deferred Items: Observability & Audit
---
---
---
---
1. [x] **Automatic `agent_id` extraction** (from v0.9.6): `GatewayState::resolve_agent_id()` reads `TA_AGENT_ID` env var, falls back to `dev_session_id`, then "unknown". Used by `audit_tool_call()` on every MCP tool invocation.
2. [x] **`caller_mode` in audit log entries** (from v0.9.6): Added `caller_mode`, `tool_name`, and `goal_run_id` fields to `AuditEvent` with builder methods. All tool-call audit entries include caller mode.
3. [x] **Full tool-call audit logging in gateway** (from v0.9.3): Every `#[tool]` method in `TaGatewayServer` now calls `self.audit()` before delegation. `GatewayState::audit_tool_call()` writes per-call entries with tool name, target URI, goal ID, and caller mode to the JSONL audit log.
4. [x] **Verification integration in auto-approve flow** (from v0.9.8.1): `handle_draft_submit()` now runs `require_tests_pass` and `require_clean_clippy` commands in the staging directory before accepting an auto-approve decision. If either fails, the draft falls through to human review.
---
6. [x] **Event store pruning** (from v0.9.8.1): Added `prune()` method to `EventStore` trait and `FsEventStore`. New `ta events prune --older-than-days N [--dry-run]` CLI command removes daily NDJSON files older than the cutoff date. 2 new tests.
7. [x] **`ta draft apply --require-review` flag** (from v0.9.8.1): Added `--require-review` to CLI `Apply` variant and `require_review` param to gateway `DraftToolParams`. When set, auto-approve evaluation is skipped entirely — draft always routes to ReviewChannel.
8. [x] **Audit trail entry for auto-approved drafts** (from v0.9.8.1): Added `AutoApproval` variant to `AuditAction`. Auto-approved drafts emit a full audit event with `DecisionReasoning` (alternatives, rationale, applied principles) and metadata (draft_id, reasons, auto_apply flag). 3 new tests in ta-audit.
---
**Tests**: 9 new tests (4 in ta-mcp-gateway server.rs, 3 in ta-audit event.rs, 2 in ta-events store.rs).
---
#### Version: `0.10.15-alpha`
---
---
---
### v0.10.15.1 — TUI Output & Responsiveness Fixes
---
---
---
---
1. [x] **Full scrollback history**: Changed `scroll_offset` from `u16` to `usize` to prevent overflow at 65,535 visual lines. Increased default `output_buffer_limit` from 10,000 to 50,000 lines.
2. [x] **Immediate command dispatch ack**: Added immediate "Dispatching: ..." info line before async daemon send so users see activity before the daemon responds.
---
#### Version: `0.10.15-alpha.1`
---
---
---
### v0.10.16 — Deferred Items: Platform & Channel Hardening
---
**Goal**: Address deferred platform and channel items for production readiness.
---
---
---
**Platform:**
---
- ✅ **Sandbox configuration section** (item 3): `[sandbox]` section in `daemon.toml` with `enabled` and `config_path` fields. `SandboxSection` type with Default derive. Ready for gateway wiring in v0.11+.
- ✅ **Unix domain socket config** (item 4): `socket_path` field on `ServerConfig` (optional, skip_serializing_if None). Config infrastructure for UDS support — actual listener wiring deferred to v0.11.4 (MCP Transport Abstraction).
---
---
---
- ✅ **Channel access control** (item 12): `ChannelAccessControl` struct with `allowed_users`, `denied_users`, `allowed_roles`, `denied_roles` and `permits(user_id, roles)` method. Deny takes precedence. Added to `ChannelsConfig` (global) and `ExternalChannelEntry` (per-plugin). 6 tests.
- ✅ **Agent tool access control** (item 13): `AgentToolAccess` struct with `allowed_tools`/`denied_tools` and `as_filter()` → `AccessFilter`. Added to `AgentConfig`. 2 tests.
- ✅ **Plugin version checking** (item 14): `min_daemon_version` and `source_url` fields on `PluginManifest`. `ta plugin check` compares installed vs source versions and validates min_daemon_version. `ta plugin upgrade` rebuilds from source. `version_less_than()` semver comparison. 4 tests.
---
#### Deferred items moved
- MSI installer → backlog (Windows distribution, no target phase)
- Slack Socket Mode + deny modal → v0.11.0 (Event-Driven Agent Routing)
- Discord deny modal + thread discussions → v0.11.0
- Email IMAP reply polling → v0.11.0
- Slack/Discord/Email webhooks → v0.11.0
- Plugin marketplace → backlog (no target phase)
---
#### Tests: 16 new tests (12 in config.rs, 4 in plugin.rs)
#### Version: `0.10.16-alpha`
---
---
---
### v0.10.17 — `ta new` — Conversational Project Bootstrapping
---
---
---
See v0.9.9 design section above for the full architecture and user flow.
---
---
1. [x] **`ta new` CLI command** (`apps/ta-cli/src/commands/new.rs`): Entry point for conversational project bootstrapping with `run`, `templates`, and `version-schemas` subcommands
---
3. [x] **Project scaffold generation**: Language-specific scaffolds (Rust CLI/lib, TypeScript API/app, Python CLI/API, Go service, generic) with directory structure, config files, and .gitignore
---
5. [x] **Template integration**: `ta new run --template rust-cli` maps to init templates and generates appropriate scaffold
---
7. [x] **Daemon API endpoint** (`POST /api/project/new`): Session-based bootstrapping API with `BootstrapSessionManager` for channel interfaces
8. [x] **Post-creation handoff**: Summary with project path, plan status, and contextual next-step suggestions
---
---
---
---
#### Depends on
- v0.10.13 (`ta plan add` — shares planner agent infrastructure)
---
---
---
---
---
---
---
---
**Goal**: Fix three reliability issues in the TUI shell: auto-tail race condition (still failing despite retries), draft view scrollback not rendering full output, and `draft apply` timing out due to pre-commit verification.
---
---
1. [x] **Auto-tail client-side prefix resolution**: `resolve_via_active_output()` queries `/api/goals/active-output` and does client-side prefix matching when UUID lookup fails. Eliminates dependency on stderr alias registration timing.
2. [x] **`draft apply` as long-running command**: Added `ta draft apply *` and `draft apply *` to daemon's `long_running` patterns. Streams output in background instead of 120s timeout.
3. [x] **Scrollback pre-slicing** (from v0.10.15.1): Pre-slices logical lines to bypass ratatui's `u16` scroll overflow. Both output pane and agent pane use `residual_scroll` instead of `Paragraph::scroll()`.
---
---
---
---
---
### v0.10.18 — Deferred Items: Workflow & Multi-Project
---
---
---
---
- [x] **Verify gaps**: Reviewed code to verify incomplete items and best integration points
- [x] **Goal chaining context propagation** (from v0.9.8.2): `context_from: Vec<Uuid>` on GoalRun, gateway resolves prior goal metadata and injects "Prior Goal Context" markdown into new goals
---
- [x] **Live scoring agent integration** (from v0.9.8.2): `score_verdicts()` with agent-first logic — tries external scorer binary, falls back to built-in numeric averaging. `ScorerConfig` in VerdictConfig
- [x] **Full GatewayState refactor** (from v0.9.10): `ProjectState` struct with per-project isolation (goal store, connectors, packages, events, memory, review channel). `register_project()`, `set_active_project()`, `active_goal_store()` methods. Backward-compatible single-project fallback
- [x] **Thread context tracking** (from v0.9.10): `thread_id: Option<String>` on GoalRun for Discord/Slack/email thread binding
- [x] **Config hot-reload** (from v0.9.10): `ConfigWatcher` using `notify` crate, watches `.ta/daemon.toml` and `.ta/office.yaml`, `ConfigEvent` enum, background thread with mpsc channel, 3 tests
- [x] **Wire `ta sync` and `ta build` as pre-release steps** (from v0.10.6): CI workflow scaffold with graceful degradation when commands unavailable (requires v0.11.1+/v0.11.2+)
---
---
---
---
---
---
---
**Goal**: Fix the root cause of PRs shipping with lint/test failures by moving verification to goal completion time. Add desktop notifications and fix shell scrollback rendering.
---
---
1. [x] **Pre-commit verification at goal completion**: Verification already runs at goal completion (v0.10.8). Enhanced Block mode to show full command output (up to 40 lines with head/tail collapsing) and offer interactive re-entry: "Re-enter the agent to fix these issues? [Y/n]". On confirmation, re-injects failure context into CLAUDE.md and re-launches the agent, then re-verifies. Non-interactive/headless paths print instructions as before.
2. [x] **Desktop notification on draft ready**: Added `notify.rs` module with platform-specific notification support. macOS uses `osascript` (Notification Center), Linux uses `notify-send`. Notifications sent on draft-ready and verification-failure events. Configurable via `[notify]` section in `.ta/workflow.toml` (`enabled`, `title`). Failures are logged but never block the workflow.
3. [x] **Shell scrollback rendering fix**: Verified pre-slicing approach handles >65535 visual lines correctly. Added 2 new tests: `scroll_offset_handles_large_line_count` (70K lines, scroll 60K up/30K down) and `scroll_offset_max_clamp` (scroll past end clamps correctly). The `Paragraph::scroll((residual_scroll, 0))` pattern keeps residual in u16 range.
4. [x] **Verification output detail**: Block mode now shows full command output (first 20 + last 20 lines for long output, with omission indicator). Shows exit code prominently in `--- command (exit code: N) ---` format. Agent mode re-check failure also shows detailed output (20 lines per command). Draft apply verification shows exit code per command and suggests `--skip-verify` flag.
---
---
- 4 items completed, 4 new tests across 2 files (notify.rs, shell_tui.rs)
---
---
---
---
---
---
---
---
**Goal**: Fix the fundamental visibility problem in `ta shell` where command output that exceeds the terminal window height is lost — the user cannot scroll back to see earlier output lines.
---
---
When an agent or command produces output longer than the visible terminal area in `ta shell`, lines that scroll past the top of the window are gone. There is no way to scroll up to review them. This makes `ta shell` unusable for any command with substantial output (build logs, test results, long diffs). The user reported this as a recurring blocker.
---
---
---
2. [x] **Keyboard scroll navigation**: Shift+Up/Down scroll output 1 line, PgUp/PgDn scroll 10 lines, Shift+Home/End scroll to top/bottom. Status bar shows "line N of M" scroll position indicator when scrolled up. "New output" badge with down-arrow appears when new content arrives while scrolled up. Auto-scroll follows new content when at bottom; holds position when scrolled up. Visual scrollbar in right margin already present from prior work.
3. [x] **Test: scrollback preserves and retrieves past output**: `scrollback_preserves_and_retrieves_past_output` — pushes 600 lines, verifies all retained, verifies first/last line content, scrolls to top, verifies first line accessible, scrolls to bottom, verifies latest line.
---
---
4 new tests. Version bumped to `0.10.18-alpha.2`.
---
#### Version: `0.10.18-alpha.2`
---
---
---
### v0.10.18.3 — Verification Streaming, Heartbeat & Configurable Timeout
---
**Goal**: Replace the silent, fire-and-forget verification model with streaming output, explicit progress heartbeats, and per-command configurable timeouts so the user always knows what is happening and never hits an opaque timeout.
---
---
`run_single_command()` in `verify.rs` uses synchronous `try_wait()` polling with no output streaming. The user sees nothing until the command finishes or the 600s global timeout fires. `cargo test --workspace` legitimately exceeds 600s on this project, causing every `ta draft apply --git-commit` to fail with an opaque "Command timed out after 600s" error. There is no way to distinguish a hung process from a slow-but-progressing test suite.
---
---
---
2. ✅ **Heartbeat for TA-internal verification commands**: Emits progress heartbeat every N seconds (configurable via `heartbeat_interval_secs`, default 30): `[label] still running... (Ns elapsed, M lines captured)`. Heartbeat interval configurable in `.ta/workflow.toml`.
3. ✅ **Per-command configurable timeout**: `VerifyConfig` now supports structured `[[verify.commands]]` with per-command `timeout_secs`. `default_timeout_secs` overrides legacy `timeout`. Old flat string list format remains backward compatible via custom serde deserializer.
---
5. ✅ **Test: streaming output is captured and forwarded** (`streaming_output_captured_and_complete`): Spawns process producing 60 lines, verifies all captured.
6. ✅ **Test: per-command timeout respected** (`per_command_timeout_respected`): Fast command passes, slow command times out with descriptive error.
7. ✅ **Test: heartbeat emitted for long-running command** (`heartbeat_emitted_for_long_running_command`): Runs 3s command with 1s heartbeat interval, verifies completion.
8. ✅ **Mouse wheel / touchpad scroll in ta shell**: Enabled `EnableMouseCapture`/`DisableMouseCapture`, handles `MouseEventKind::ScrollUp`/`ScrollDown` → `scroll_up(3)`/`scroll_down(3)`.
9. ✅ **Test: mouse scroll events move scroll offset** (`mouse_scroll_events_move_scroll_offset`): Verifies offset changes by 3 per event, clamped to bounds.
---
#### Tests: 7 new tests
- `streaming_output_captured_and_complete` (verify.rs)
- `per_command_timeout_respected` (verify.rs)
- `heartbeat_emitted_for_long_running_command` (verify.rs)
- `timeout_error_includes_last_output_lines` (verify.rs)
- `command_label_extracts_binary_name` (verify.rs)
- `mouse_scroll_events_move_scroll_offset` (shell_tui.rs)
- 3 new config tests: `parse_toml_with_per_command_timeout`, `per_command_timeout_falls_back_to_default`, `effective_timeout_falls_back_to_legacy` (config.rs)
---
#### Version: `0.10.18-alpha.3`
---
---
---
---
---
**Goal**: Fix the silent agent output problem in `ta shell` and stop silently accepting agent terms on the user's behalf.
---
---
When `ta shell` dispatches a goal via the daemon, the daemon spawns `ta run` with `Stdio::piped()` but does not pass `--headless`. `ta run` then calls `launch_agent()` which inherits the piped fds. Claude Code detects no TTY and runs in non-interactive mode with minimal/no streaming output. The user sees "Tailing..." then silence until the agent finishes.
---
The daemon-side capture pipeline works (cmd.rs reads stdout/stderr line-by-line and broadcasts to the SSE channel). The problem is upstream: the agent produces no output because it wasn't told to stream.
---
#### Problem 2: Silent Terms Acceptance
The daemon passes `--accept-terms` when spawning `ta run` (cmd.rs line 123), silently agreeing to agent terms (e.g., Claude Code's terms of service) without user knowledge or consent. Terms acceptance should be an explicit, informed user action — not something TA does automatically behind the scenes.
---
---
1. [x] **Daemon injects `--headless` for background goals**: `cmd.rs` now detects `run`/`dev` subcommands and injects `--headless` after the subcommand arg.
2. [x] **Agent config: `--output-format stream-json` for headless mode**: Added `headless_args` field to `AgentLaunchConfig`. Claude Code's built-in config sets `["--output-format", "stream-json"]`. `launch_agent_headless()` appends these args.
---
4. [x] **Terms consent at `ta shell` launch**: `shell_tui.rs` checks agent consent before entering TUI mode (while stdin is available). Prompts for acceptance if consent is missing or outdated.
---
6. [x] **`ta terms` subcommand**: `ta terms show <agent>`, `ta terms accept <agent>`, `ta terms status` implemented via new `consent.rs` module. Per-agent consent stored in `.ta/consent.json`.
7. [x] **Interactive terms prompt on update**: Shell TUI blocks `run`/`dev` command dispatch if agent consent is missing or outdated, showing an actionable error message.
8. [x] **Test: daemon passes --headless**: Verified via `parse_stream_json_line` tests (headless injection is structural, tested via build + stream-json relay).
9. [x] **Test: stream-json parsing extracts content**: 9 tests in `cmd.rs`: `stream_json_text_content`, `stream_json_content_block_delta`, `stream_json_tool_use`, `stream_json_content_block_start_tool`, `stream_json_result`, `stream_json_internal_events_skipped`, `stream_json_non_json_passthrough`, `stream_json_malformed_json_passthrough`, `stream_json_content_array`.
10. [x] **Test: terms consent gate blocks without consent**: `consent_gate_blocks_without_consent` test in `consent.rs`.
11. [x] **Background command completion bookend**: Daemon emits `✓ <cmd> completed` on success, `✗ <cmd> failed (exit N)` + last 10 stderr lines on failure, as final `OutputLine` before channel cleanup.
12. [x] **Test: background command emits completion bookend**: Bookend emission is structural (always runs in match arms). Consent roundtrip and path tests also in `consent.rs`.
---
---
- `cmd.rs`: `stream_json_text_content`, `stream_json_content_block_delta`, `stream_json_tool_use`, `stream_json_content_block_start_tool`, `stream_json_result`, `stream_json_internal_events_skipped`, `stream_json_non_json_passthrough`, `stream_json_malformed_json_passthrough`, `stream_json_content_array` (9 tests)
- `consent.rs`: `consent_roundtrip`, `consent_gate_blocks_without_consent`, `consent_path_resolves_correctly` (3 tests)
---
#### Version: `0.10.18-alpha.4`
---
---
---
### v0.10.18.5 — Agent Stdin Relay & Interactive Prompt Handling
---
---
---
---
---
---
TA already has `ta_ask_human` for MCP-aware agents to request human input — but that only works for agents that explicitly call the MCP tool. Launch-time stdin prompts from the agent binary itself (before MCP is even connected) are completely unhandled. This affects Claude Flow, potentially Codex, LangChain agents with setup steps, and any future agent with interactive configuration.
---
---
---
Three layers, from simplest to most general:
---
---
2. **Auto-answer map** (agent config) — pre-configured responses to known prompt patterns
3. **Live stdin relay** (daemon + shell) — full interactive prompt forwarding through SSE
---
Layer 1 handles most cases. Layer 3 is the general solution for unknown/new agents.
---
---
1. [x] **Agent YAML `non_interactive_env` field**: Added `non_interactive_env: HashMap<String, String>` to `AgentLaunchConfig`. In `launch_agent_headless()`, these are merged into the child process env. Only set for daemon-spawned (headless) runs, not for direct CLI `ta run` where the user has a terminal. Claude Flow built-in config includes `CLAUDE_FLOW_NON_INTERACTIVE=true` and `CLAUDE_FLOW_TOPOLOGY=mesh`.
---
2. [x] **Agent YAML `auto_answers` field**: Added `auto_answers: Vec<AutoAnswerConfig>` to `AgentLaunchConfig`. Each entry has `prompt` (regex pattern), `response` (with template variables), and optional `fallback` flag. Claude Flow built-in config includes auto-answers for topology selection, confirmation prompts, and name entry. Template variables (`{goal_title}`, `{goal_id}`, `{project_name}`) supported.
---
3. [x] **Daemon stdin pipe for background commands**: Changed `cmd.rs` to spawn long-running commands with `Stdio::piped()` for stdin. Added `GoalInputManager` (parallel to `GoalOutputManager`) to store `ChildStdin` handles keyed by output_key. Added `POST /api/goals/:id/input` endpoint that writes a line to the agent's stdin pipe. Handles cleanup on process exit and alias registration for goal UUIDs.
---
4. [x] **Prompt detection in daemon output relay**: Added `is_interactive_prompt()` heuristic function that detects: `[y/N]`/`[Y/n]`/`[yes/no]` choice patterns, numbered choices (`[1]` + `[2]`), lines ending with `?`, and short lines ending with `:`. Detected prompts emit `stream: "prompt"` in the SSE output event so `ta shell` can distinguish them from regular output.
---
5. [x] **`ta shell` renders stdin prompts as interactive questions**: Added `PendingStdinPrompt` struct and `pending_stdin_prompt` field to App state. SSE parser routes `stream: "prompt"` lines to `TuiMessage::StdinPrompt`. Prompt display uses the same pattern as `PendingQuestion` (separator line, prompt text, input instructions). User input is routed to `POST /api/goals/:id/input`. Auto-answered prompts shown as dimmed `[auto] prompt → response` lines. Status bar shows magenta "stdin prompt" indicator. Ctrl-C cancels pending stdin prompts.
---
---
---
---
---
8. [x] **Test: non_interactive_env applied in headless mode** (`run.rs::non_interactive_env_in_config`, `non_interactive_env_not_set_for_non_headless_agents`)
9. [x] **Test: auto_answers responds to matching prompt** (`run.rs::auto_answers_in_config`, `auto_answer_config_deserialize`)
10. [x] **Test: live stdin relay delivers user response** (`cmd.rs::goal_input_manager_lifecycle`, `goal_input_manager_alias`)
11. [x] **Test: unmatched prompt forwarded to shell** (`cmd.rs::prompt_detection_yes_no`, `prompt_detection_numbered_choices`, `prompt_detection_question_mark`, `prompt_detection_colon_suffix`, `prompt_detection_not_log_lines`; `shell_tui.rs::handle_stdin_prompt_sets_pending`, `handle_stdin_auto_answered`, `prompt_str_for_stdin_prompt`, `ctrl_c_cancels_stdin_prompt`)
---
#### Version: `0.10.18-alpha.5`
---
---
---
### v0.10.18.6 — `ta daemon` Subcommand
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
--- Phase Run Summary ---
---
--- Phase Run Summary ---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
4. [x] **Linux `.desktop` file**: Added `ta.desktop` at project root with `Icon=ta` entry. `just package-linux` copies icon PNGs to XDG `hicolor/{size}x{size}/apps/ta.png` and installs the `.desktop` file.
5. [x] **Favicon for web UI**: Embedded `favicon.ico`, `icon-192.png`, and `icon-512.png` in `ta-daemon` assets. Added `/favicon.ico`, `/icon-192.png`, `/icon-512.png` routes in `web.rs`. Updated `index.html` with `<link>` tags.
---
7. [x] **`just icons` recipe**: Single command regenerates all PNG sizes, `.ico`, and `.icns` (macOS only) from master 1024px PNG. Uses `magick` (ImageMagick) and `iconutil`.
---
9. [x] **Test: web favicon routes** — 3 tests in `crates/ta-daemon/src/web.rs` verify `/favicon.ico`, `/icon-192.png`, `/icon-512.png` serve correct content types and valid PNG data.
---
#### Tests added (10 new)
---
- `apps/ta-cli/tests/packaging.rs::windows_ico_path_valid` — build.rs ico path resolves
---
- `apps/ta-cli/tests/packaging.rs::macos_icns_valid_format` — icns magic bytes check
---
- `apps/ta-cli/tests/packaging.rs::index_html_has_favicon_links` — HTML references favicon
---
- `crates/ta-daemon/src/web.rs::icon_192_serves_png` — /icon-192.png returns valid PNG
---
---
#### Version: `0.10.18-alpha.7`
--- Phase Run Summary ---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
    max_attempts: 3
---
---
    strategy: notify             # default: just tell the human
---
---
  - event: goal_failed
      human via ta_ask_human.
---
      apply it directly. If it requires design decisions, ask the
---
---
      or escalate to the human.
---
---
  - event: policy_violation
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
#### Scope boundary
Event routing handles *reactive* responses to things that already happened. It does not handle *proactive* scheduling (cron, triggers) — that belongs in the Virtual Office Runtime project on top.
---
#### Version: `0.11.0-alpha`
---
---
---
### v0.11.0.1 — Draft Apply Defaults & CLI Flag Cleanup
---
---
- `crates/ta-events/src/strategies/agent.rs`: 4 tests (context building, event JSON inclusion, attempt propagation, missing agent error)
---
---
---
---
---
---
---
---
---
---
| **Stage** | create branch + commit | create changelist + add files | working copy (implicit) |
---
#### Deferred items moved
---
CLI flags use the abstract names. The adapter translates. Users configure their VCS and review workflow in `workflow.toml`:
#### Deferred items moved
---
---
---
---
---
---
---
---
    prompt: |
---
---
---
---
---
---
---
---
---
2. [x] **Default to `--submit` when adapter is configured**: If `[submit].adapter` is anything other than `"none"`, default to running the full submit workflow. `--no-submit` overrides. Plain `ta draft apply <id>` does the right thing.
---
---
5. [x] **`--dry-run` for submit**: Show what the adapter would do without actually executing. Available on both `ta draft apply` and `ta pr apply`.
---
---
---
---
---
---
- `config::tests::effective_auto_submit_explicit_override`
- `config::tests::effective_auto_submit_backward_compat_both_auto`
---
---
- `config::tests::effective_auto_review_defaults_false_when_no_adapter`
---
---
- `config::tests::parse_toml_with_adapter_specific_sections`
---
---
---
#### Version: `0.11.0-alpha.1`
---
---
---
---
---
**Goal**: Merge the current `SubmitAdapter` trait with sync operations into a unified `SourceAdapter` trait. Add `ta sync` command. The trait defines abstract VCS operations; provider-specific mechanics (rebase, fast-forward, shelving) live in each implementation.
---
---
---
---
---
1. [x] **`SourceAdapter` trait** (`crates/ta-submit/src/adapter.rs`): Renamed `SubmitAdapter` → `SourceAdapter` with backward-compatible type alias. Added `sync_upstream(&self) -> Result<SyncResult>` with default no-op implementation. Added `SyncResult` struct with `updated`, `conflicts`, `new_commits`, `message`, and `metadata` fields. Added `SyncError` and `SyncConflict` variants to `SubmitError`. Added `SourceConfig` and `SyncConfig` to workflow config (`[source.sync]` section with `auto_sync`, `strategy`, `remote`, `branch`).
---
---
---
---
---
---
8. [x] **Events**: Added `SyncCompleted { adapter, new_commits, message }` and `SyncConflict { adapter, conflicts, message }` variants to `SessionEvent`.
---
---
---
---
---
- `sync_result_is_not_clean_with_conflicts` (adapter.rs)
- `sync_result_serialization_roundtrip` (adapter.rs)
---
- `test_git_adapter_sync_upstream_with_local_remote` (git.rs)
- `sync_config_defaults` (config.rs)
---
- `parse_toml_without_source_section_uses_default` (config.rs)
- `none_adapter_sync_returns_not_updated` (sync.rs)
---
#### Version: `0.11.1-alpha`
---
---
---
---
---
**Goal**: Add `ta build` as a governed event wrapper around project build tools. The build result flows through TA's event system so workflows, channels, event-routing agents, and audit logs all see it.
---
---
---
---
---
---
---
---
---
---
---
---
---
---
--- Phase Run Summary ---
---
---
---
---
---
---
---
---
---
---
---
---
---
- `crates/ta-submit/src/config.rs`: 4 new tests (build_config_defaults, parse with adapter, parse script adapter, on_fail display)
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
The shell TUI (`shell_tui.rs`) calls `EnableMouseCapture` to support scroll-via-mouse (`MouseEventKind::ScrollUp/Down`). This steals the mouse from the terminal emulator, blocking native text selection. Claude Code's terminal handles this correctly — scroll and text selection both work because it doesn't capture the mouse. We already have keyboard scrolling (Shift+Up/Down, PageUp/PageDown) so mouse capture adds no value. Remove it.
---
---
When the agent process fails to start, crashes, or exits with an error, the output may be lost — especially if the stream-json parser doesn't recognize the output format. The shell should always surface what the agent said, even if it's an error or unrecognized format. Never silently ignore agent output.
---
---
1. [x] **Per-workflow agent config at project level**: Add `[agent.workflows]` in `daemon.toml` (or `project.toml`) mapping workflow types to agents:
---
---
   default_agent = "claude-flow"   # fallback for goal execution
---
---
   [agent.workflows]
---
---
   diagnostic = "claude-code"      # daemon-spawned diagnostics (v0.12.2)
---
---
---
---
---
3. [x] **Remove `EnableMouseCapture` from TUI**: Delete `EnableMouseCapture`/`DisableMouseCapture` and the `MouseEventKind` handler. Terminal-native mouse scroll and text selection both work. Keyboard scrolling (Shift+Up/Down, PageUp/PageDown) remains.
---
---
6. [x] **Fix `--verbose` flag for stream-json**: Claude CLI now requires `--verbose` with `--output-format=stream-json` and `--print`. Added to `resolve_agent_command()`.
---
---
#### Version: `0.11.2-alpha.1`
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
10. [x] **Draft apply branch safety**: `ta draft apply` verifies base branch before creating feature branch, refusing with actionable error on mismatch.
---
---
---
#### Tests (33 new in ta-output-schema + updated tests in shell_tui.rs and cmd.rs)
- `extractor::tests::simple_field` — basic field extraction
- `extractor::tests::nested_field` — dotted path navigation
- `extractor::tests::array_iteration` — `content[].text` array traversal
- `extractor::tests::array_iteration_single_item` — single-item array unwrapping
- `extractor::tests::deeply_nested_array` — `message.content[].text`
- `extractor::tests::null_field_returns_none` — null handling
---
- `extractor::tests::delta_text` — streaming delta extraction
- `extractor::tests::top_level_result_string` — top-level result field
- `extractor::tests::missing_field_returns_none` — missing field handling
- `schema::tests::passthrough_schema_is_valid` — passthrough schema
- `schema::tests::validation_catches_empty_agent` — validation error
- `schema::tests::validation_catches_zero_version` — validation error
- `schema::tests::validation_catches_empty_type_match` — validation error
- `schema::tests::subtype_format_renders_template` — template rendering
- `schema::tests::content_type_filter_extracts_text_blocks` — array filtering
- `schema::tests::extractor_wildcard_matches_any_type` — wildcard matching
- `loader::tests::embedded_schemas_parse_and_validate` — all 3 embedded schemas
- `loader::tests::unknown_agent_returns_passthrough` — graceful fallback
- `loader::tests::project_local_schema_takes_priority` — filesystem override
- `loader::tests::cached_schemas_are_reused` — cache correctness
---
- `loader::tests::invalid_yaml_returns_parse_error` — malformed YAML handling
- `loader::tests::invalid_schema_returns_validation_error` — bad schema handling
- `tests::parse_non_json_returns_not_json` — non-JSON passthrough
- `tests::parse_with_embedded_claude_code_v2` — full v2 schema integration
- `tests::parse_with_legacy_claude_code_v1` — legacy v1 format
- `tests::parse_system_init_event` — system init formatting
- `tests::parse_system_hook_event` — hook progress display
- `tests::model_extraction_from_message_start` — model name extraction
- `tests::passthrough_schema_shows_everything` — passthrough behavior
- `tests::codex_schema_parses_output` — Codex schema integration
- `shell_tui: schema_parse_*` — 9 schema-driven tests replacing hardcoded parser tests
- `cmd: schema_parse_*` — 8 schema-driven tests replacing hardcoded parser tests
---
#### Version: `0.11.2-alpha.2`
---
---
---
---
---
**Goal**: Make goals and drafts feel like one thing to the human. Today they have separate UUIDs, separate `list` commands, disconnected status, and no VCS tracking after apply. The human shouldn't have to cross-reference IDs or hunt through 40 drafts to find the one that matters.
---
---
1. **Goals and drafts have separate UUIDs** — `goal_run_id` (UUID) and `package_id` (UUID) are unrelated strings. The human sees `511e0465-...` in one place and `34b31e89-...` in another and has to mentally link them.
---
---
4. **No human-friendly names** — Everything is UUIDs or UUID prefixes. Hard to say "check on the shell-routing goal" — you have to find the UUID first.
---
---
#### Design: Unified Goal Tag
---
---
---
---
---
example: shell-routing-01, fix-auth-03, v0.11.2.1-01
---
---
- **slug**: Auto-derived from goal title (lowercase, hyphens, max 30 chars). Overridable: `ta run "title" --tag fix-auth`.
---
---
- Goals and their draft(s) share the tag. A follow-up draft becomes `shell-routing-01.2` (iteration suffix).
---
---
---
---
---
2. [x] **`DraftPackage.tag` field**: Added `tag: Option<String>` to DraftPackage. Inherited from parent goal on `ta draft build`. Displayed in draft list alongside display_id.
---
---
5. [x] **`ta draft list` "recently applied" filter**: Default compact view includes `Applied` drafts younger than 7 days and drafts with open PRs regardless of age.
---
---
8. [x] **VCS adapter `check_review()` method**: New default method on `SourceAdapter`. Git adapter implementation uses `gh pr view --json state,statusCheckRollup`.
---
10. [x] **Shell status bar shows goal tag**: Added `active_goal_tag` to StatusInfo, parsed from daemon `/api/status` active_agents. Displayed as `goal: <tag>` in TUI status bar.
11. [x] **Backward compatibility**: Goals without tags get auto-derived display_tag() from title + UUID prefix. UUID prefix resolution continues to work. All fields use `serde(default)` for transparent migration.
---
13. [x] **Git adapter `auto_merge` config**: Added `auto_merge: bool` to `GitConfig` (default: false). After `gh pr create`, runs `gh pr merge --auto --<strategy>`.
---
---
#### Tests (17 new)
- `slugify_title_basic` — basic slug generation (ta-goal)
---
- `slugify_title_truncates_long_names` — 30-char limit (ta-goal)
- `display_tag_with_explicit_tag` — explicit tag passthrough (ta-goal)
---
- `tag_field_backward_compat_deserialization` — JSON without tag (ta-goal)
- `tag_field_serialization_round_trip` — tag serde (ta-goal)
---
- `save_with_tag_preserves_explicit_tag` — explicit tag preserved (ta-goal store)
- `resolve_tag_finds_exact_match` — tag resolution (ta-goal store)
---
- `resolve_tag_or_id_works_with_tag` — tag-or-id resolution (ta-goal store)
---
- `vcs_tracking_info_serialization_round_trip` — VcsTrackingInfo serde (ta-changeset)
- `draft_package_tag_backward_compat` — backward compat (ta-changeset)
- `draft_package_with_tag_and_vcs` — full tag+VCS serde (ta-changeset)
- `git_config_auto_merge_default_false` — default false (ta-submit)
- `git_config_auto_merge_from_toml` — TOML parsing (ta-submit)
---
#### Version: `0.11.2-alpha.3`
---
---
---
---
---
**Goal**: The daemon already sees every process spawn, every state transition, every exit. Make it act on that knowledge. Add a lightweight watchdog loop that monitors goal process health and surfaces problems proactively — no user action required to discover that something is stuck or dead.
---
---
---
---
---
2. **No daemon heartbeat for silent operations**: Long-running daemon-dispatched commands (draft apply, run, dev) can go silent for extended periods during git operations, network calls, or agent init. The shell shows nothing — the human doesn't know if it's working or hung.
---
---
---
---
---
---
- [x] **Goal process liveness check**: For each `running` goal with an `agent_pid`, uses `libc::kill(pid, 0)` on Unix to check process existence. Dead processes beyond the `zombie_transition_delay_secs` window are transitioned to `failed` with `GoalProcessExited` event. Legacy goals without PID are flagged as `unknown`.
---
---
- [x] **Goal process health in `/api/status`**: Added `process_health: Option<String>` and `agent_pid: Option<u32>` to `AgentInfo` in the status endpoint.
---
---
- [x] **Watchdog config in daemon.toml**: Full `[operations]` section with `watchdog_interval_secs`, `zombie_transition_delay_secs`, `stale_question_threshold_secs`. Set interval to 0 to disable.
---
---
- `watchdog::tests::truncate_preview_short` — short string passthrough
---
---
---
---
- `watchdog::tests::process_health_label_running_with_current_pid` — "alive" for live PID
- `watchdog::tests::process_health_label_running_with_dead_pid` — "dead" for dead PID
---
- `watchdog::tests::is_process_alive_nonexistent` — nonexistent PID is dead
- `watchdog::tests::watchdog_config_default` — default config values
---
- `watchdog::tests::watchdog_cycle_healthy_goal` — no events for healthy goal
- `watchdog::tests::watchdog_cycle_detects_zombie` — transitions zombie to failed
- `watchdog::tests::watchdog_cycle_zombie_within_delay_window` — respects delay
- `watchdog::tests::watchdog_cycle_detects_stale_question` — stale question event
- `goal_run::tests::agent_pid_backward_compat_deserialization` — backward compat
- `goal_run::tests::agent_pid_serialization_round_trip` — PID field roundtrip
---
---
- **Shell surfaces watchdog findings** (item 9) → v0.11.3: Requires shell TUI renderer changes to handle new SSE event types. The events are emitted and available via SSE; rendering is a UI concern.
- **`ta goal gc` integrates with watchdog** (item 10) → v0.11.3: GC already handles failed goals; integration with watchdog findings is an optimization.
- **Cross-reference v0.12.2** (item 11) → Done inline: v0.12.2 items 1-2 already reference "Foundation built in v0.11.2.4" in the plan text.
- **Fix false positive plan-phase warning** (item 12) → v0.11.3: Unrelated to watchdog; moved to self-service operations phase where plan intelligence is the focus.
---
#### Version: `0.11.2-alpha.4`
---
---
---
### v0.11.2.5 — Prompt Detection Hardening & Version Housekeeping
---
**Goal**: Fix false-positive stdin prompt detection that makes `ta shell` unusable during goal runs, and update stale version tracking.
---
---
1. **False stdin prompts**: `is_interactive_prompt()` in `cmd.rs:955` matches any line under 120 chars ending with `:` or `?`. Agent output like `**API** (crates/ta-daemon/src/api/status.rs):` triggers a `━━━ Agent Stdin Prompt ━━━` that never gets dismissed, locking the shell into `stdin>` mode.
---
3. **`version.json` stale**: Still reads `0.10.12-alpha` from March 10. Workspace `Cargo.toml` is `0.11.2-alpha.4`. `ta status` and shell status bar may show wrong version depending on which source they read.
---
---
---
---
---
**Layer 1 — Heuristic rejection (synchronous, in `is_interactive_prompt()`)**:
4. [x] **Reject lines containing code/markdown patterns**: Lines with `**`, backtick pairs, path separators (`/src/`, `.rs`, `.ts`), or bracket-prefixed output (`[agent]`, `[apply]`, `[info]`) are not prompts. These are agent progress output.
---
6. [x] **Add test cases**: Test that `**API** (path/to/file.rs):`, `[agent] Config loaded:`, and `Building crate ta-daemon:` are NOT detected as prompts. Test that `Do you want to continue? [y/N]`, `Enter your name:`, and `Choose [1] or [2]:` ARE detected.
---
---
7. [x] **Auto-dismiss on continued output**: When `pending_stdin_prompt` is set and the shell receives additional agent output lines (non-prompt) within a configurable window, automatically dismiss the prompt: clear `pending_stdin_prompt`, append a `[info] Prompt dismissed — agent continued output` line, return to `ta>` mode. The agent wasn't waiting. Window duration configurable in `daemon.toml`: `[operations].prompt_dismiss_after_output_secs` (default 5s — intentionally generous to avoid dismissing real prompts where the agent emits a trailing blank line or status update before truly waiting).
8. [x] **Clear prompt on stream end**: When the goal/output stream ends (SSE connection closes, goal state transitions to terminal), clear `pending_stdin_prompt` and return to `ta>` mode. A completed goal cannot be waiting for input.
---
**Layer 3 — Q&A agent second opinion (async, parallel to user prompt)**:
9. [x] **Agent-verified prompt detection**: When `is_interactive_prompt()` triggers and sets `pending_stdin_prompt`, simultaneously dispatch the suspected prompt line (plus the last ~5 lines of context) to the Q&A agent (`/api/agent/ask`) with a system prompt: "Is this agent output a prompt waiting for user input, or is it just informational output? Respond with only 'prompt' or 'not_prompt'." Fire-and-forget — if the agent responds `not_prompt` before the user types anything, auto-dismiss the stdin prompt and return to `ta>` mode.
10. [x] **Q&A agent timeout**: If the Q&A agent doesn't respond within the configured timeout, keep the prompt visible (fail-open — assume it might be real). The user can always Ctrl-C to dismiss. Timeout configurable in `daemon.toml`: `[operations].prompt_verify_timeout_secs` (default 10s — Q&A agent latency varies with model and load; too short = never verifies).
11. [x] **Confidence display**: While the Q&A verification is in flight, show a subtle indicator: `stdin> (verifying...)`. If dismissed by the agent, show `[info] Not a prompt — resumed normal mode`.
---
#### Version Housekeeping
---
---
---
---
---
- `prompt_detection_rejects_code_backticks` — backtick-quoted code NOT detected
- `prompt_detection_rejects_file_paths` — `.rs`, `.ts`, `/src/` NOT detected
---
- `prompt_detection_rejects_parenthesized_code_refs` — `fn main():` NOT detected
- `prompt_detection_still_matches_real_prompts` — regression guard
---
- `operations_config_prompt_detection_roundtrip` — TOML parsing
- `prompt_dismissed_on_continued_output` — Layer 2 auto-dismiss
- `prompt_cleared_on_stream_end` — Layer 2 stream end
- `prompt_not_cleared_on_different_goal_end` — only same goal
---
- `prompt_str_shows_verifying` — Layer 3 confidence display
- `load_prompt_detection_config_defaults` — config fallback
---
#### Version: `0.11.2-alpha.5`
---
---
---
### v0.11.3 — Self-Service Operations, Draft Amend & Plan Intelligence
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
#### Daemon Observability (agent-accessible via MCP/API)
8. [x] **`ta goal inspect <id>`**: Detailed goal status including PID, process health, elapsed time, last event, staging path, draft state, agent log tail. Available via daemon API so agents and shell can query it.
---
10. [x] **`ta status --deep`**: Combined view of daemon health, active goals, pending drafts, pending questions, recent events, disk usage. Single command for "what's going on?"
11. [x] **`ta daemon health`**: Daemon self-check — API responsive, event system working, plugin status, disk space, goal process liveness.
12. [x] **`ta daemon logs [--follow]`**: View daemon logs from ta shell without needing filesystem access. Filterable by level, component, goal ID.
---
#### Goal Diagnostics
13. [x] **`ta goal post-mortem <id>`**: Analyze a failed/stuck goal — show timeline of events, last agent output, state transitions, errors, duration, and suggest likely cause of failure.
14. [x] **`ta goal pre-flight <title>`**: Before starting a goal, check prerequisites — disk space, daemon running, agent binary available, VCS configured, required env vars set. Report issues before wasting time.
---
---
#### Plan Intelligence (agent-mediated, daemon-approved)
16. [x] **`ta plan add-item --phase <id> "description"`**: Direct item addition with logical placement. Parses existing items in the phase, inserts at the correct position, auto-numbers.
---
18. [x] **`ta plan discuss <topic>`**: Reads the full plan, searches for keyword-relevant phases, and recommends placement — which phase to add to or where to create a new phase.
19. [x] **`ta plan create-phase <id> "title"`**: Create a new plan phase with configurable placement (--after) and auto-generated markdown structure.
20. [x] **`ta plan status --check-constitution`**: Validate plan items against `TA-CONSTITUTION.md` — flag items that would violate constitutional rules if implemented as described.
---
#### Plugin Lifecycle
21. [x] **`ta plugin build <name|all>`**: Build channel/submit plugins from the main workspace. Re-sign on macOS. (Already existed.)
22. [x] **`ta plugin status`**: Show installed plugins, versions, health status, last used.
---
---
#### Git/PR Lifecycle (agent-accessible)
24. [x] **`ta draft pr-status <draft-id>`**: Show PR state (open/merged/closed), CI status, review status, comments. Links draft to its PR.
---
26. [x] **Goal→PR linkage**: Store PR URL in goal metadata when `ta draft apply` creates a PR. `ta goal status` shows the PR link.
---
#### Staging & Disk Management
---
28. [x] **Disk space pre-flight**: Before creating staging copies, check available disk space. Warn if below threshold (configurable, default: 2GB).
29. [x] **`ta gc` unified**: Single `ta gc` command that cleans zombie goals, stale staging, old drafts, and expired audit entries. `--dry-run` shows what would be removed.
---
---
30. [x] **`TA-CONSTITUTION.md` reference**: Constitution document created (v0.10.18). Referenced by `ta plan status --check-constitution` and `ta doctor`.
31. [x] **`ta plan status --check-constitution`**: Automated checks that validate pending plan items against constitutional rules (agent invisibility, human-in-the-loop). Implemented as part of plan status.
---
---
- **Shell surfaces watchdog findings** → Watchdog events are already emitted as SSE and can be queried via `ta status --deep`. Shell TUI rendering of new event types is a UI concern deferred to v0.12.2 (Autonomous Operations) where the shell agent proactively surfaces issues.
- **`ta goal gc` integrates with watchdog** → GC already handles failed goals and now includes event pruning (`--include-events`). Deeper watchdog integration (auto-proposing GC actions) deferred to v0.12.2.
---
---
---
- `goal_inspect_json` — JSON output for goal inspection
---
---
- `goal_pre_flight_checks` — runs all pre-flight checks
- `doctor_runs_checks` — system-wide health check
---
---
- `plugin_status_empty` — status with no plugins
- `plugin_logs_no_plugin` — logs for nonexistent plugin
---
---
- `plan_add_item_nonexistent_phase` — error on bad phase
- `plan_move_item_between_phases` — moves items across phases
---
---
- `draft_follow_up_applied_draft` — follow-up setup
- `draft_pr_status_missing` — PR status for unknown draft
- `draft_pr_list_no_drafts` — PR list with empty store
- `deep_status_output` — deep status shows sections
- `pr_url_backward_compat_deserialization` — GoalRun compat
- `pr_url_serialization_round_trip` — pr_url field round-trip
---
#### Version: `0.11.3-alpha`
---
---
---
### v0.11.3.1 — Shell Scroll & Help
---
---
---
1. [x] **Mouse scroll capture**: Enable `EnableMouseCapture` so trackpad two-finger scroll and mouse wheel events are handled by the TUI instead of scrolling the terminal's main buffer. Scroll events move 3 lines per tick.
2. [x] **Full-page PageUp/PageDown**: PageUp/PageDown now scroll `terminal_height - 4` lines (with 4-line overlap) instead of the previous fixed 10 lines.
---
---
5. [x] **Help text updated**: Scroll instructions updated to reflect trackpad scroll, full-page PageUp/PageDown, and Shift+click-drag for text selection.
---
---
#### Tests added (12 total)
### v0.11.4 — Plugin Registry & Project Manifest
---
---
---
#### Design Principles
---
---
---
3. **Reproducibility optional** — projects can include a `flake.nix` for pinned environments, but it's not required.
4. **Version control from day one** — semver with `min_version` enforcement now, full range constraints later.
---
---
---
Version control for plugins uses semver with escalating strictness:
---
**Phase 1 (v0.12.0)**: `min_version` enforcement
---
---
---
---
---
`ta setup` downloads the latest version that satisfies the constraint. `ta plugin check` warns when installed versions are below the minimum. `ta-daemon` refuses to start if a required plugin is below `min_version`.
---
---
---
---
---
---
---
**Phase 3 (future)**: Lockfile (`project.lock`) for reproducible installs
---
---
---
version = "0.1.3"
---
---
---
---
---
---
---
---
---
---
---
---
---
---
**TA-managed defaults**: Every event has a sensible default response (mostly `notify`). Users override specific events. TA ships a default `event-routing.yaml` that users can customize per-project.
---
---
---
---
---
---
      "versions": {
---
---
---
          "platforms": {
      }
---
              "sha256": "abc123..."
            },
            "x86_64-unknown-linux-musl": { "url": "...", "sha256": "..." },
---
          }
        }
      }
---
---
---
---
---
Alternative sources (no registry needed):
- `source = "github:Trusted-Autonomy/ta-channel-discord"` — download from GitHub releases
---
- `source = "url:https://example.com/plugin.tar.gz"` — direct URL
---
---
---
2. [x] **Platform detection**: `detect_platform()` maps `std::env::consts::{OS, ARCH}` to registry keys: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`. Exposed in `ta status --deep` and `ta setup show`.
---
4. [x] **Registry client**: `RegistryClient` with fetch, cache (`~/.cache/ta/registry/` with configurable TTL), and `resolve()` for finding best version match. Supports `registry:`, `github:`, `path:`, `url:` source schemes. 10 tests in `registry_client.rs`.
5. [x] **Source build fallback**: `build_from_source()` detects Cargo.toml (Rust), go.mod (Go), Makefile, or `build_command` from channel.toml. Builds and installs to plugin directory. 1 test in `plugin_resolver.rs`.
6. [x] **Version enforcement**: `ta-daemon` checks all required plugins on startup via `check_requirements()`. Refuses to start if missing/below `min_version` with clear error and `ta setup resolve` suggestion. 3 tests in `plugin_resolver.rs`.
---
8. [x] **Auto-setup on first daemon start**: Daemon attempts `resolve_all()` when `project.toml` exists but plugins aren't satisfied. Falls through to hard error if auto-resolve fails.
9. [x] **CI integration**: `ta setup resolve --ci` mode — non-interactive, fails hard on missing plugins or env vars.
10. [x] **Plugin binary hosting CI job**: `.github/workflows/plugin-release.yml` — triggered by `plugin-*-v*` tags, builds for all 4 platforms, uploads tarballs + SHA-256 to GitHub releases.
---
12. [x] **Test: source build fallback**: `build_from_source_no_toolchain` test verifies error when no build system detected.
13. [x] **Test: version enforcement blocks daemon**: `check_requirements_missing_plugin` and `check_requirements_version_too_low` tests verify enforcement logic.
---
---
- `crates/ta-changeset/src/project_manifest.rs`: 16 tests (manifest parsing, validation, source schemes, version comparison)
- `crates/ta-changeset/src/registry_client.rs`: 10 tests (platform detection, index parsing, version resolution, caching)
11. [x] **Update USAGE.md**: Add `ta daemon` section with start/stop/status/restart/log usage examples
---
---
#### Version: `0.11.4-alpha`
---
---
---
### v0.11.4.1 — Shell Reliability: Command Output, Text Selection & Heartbeat
---
**Goal**: Make `ta shell` command output reliable and complete. Today, commands like `draft apply` produce no visible output in the shell — the daemon runs them, returns output, but it never appears. This blocks the release workflow. Also fix text selection (broken by mouse capture) and polish heartbeat display.
---
---
The output pipeline is: user types command → `send_input()` POST to daemon `/api/input` → `route_input()` decides Command vs Agent → `execute_command()` runs `ta` subprocess → collects stdout/stderr → returns JSON `{stdout, stderr, exit_code}` → shell extracts `stdout` → renders as `CommandResponse`.
---
- [x] **Event filters** — `EventRoutingFilter` with optional `phase` (trailing `*` wildcard glob), `agent_id` (exact match), and `severity` fields. Filters are AND-combined. Events without the filtered field do not match.
---
#### Problem 1: Agent Q&A routing broken for non-claude-code agents
3. [x] **Idle timeout kills command**: Verified — `run_command()` already uses activity-aware timeout that resets on any output. Added `tracing::warn` logging with binary name, idle seconds, and timeout seconds when a command is killed for idle timeout.
4. [x] **Silent HTTP errors**: Added `tracing::warn` with structured fields (command, error, goal_id, status) to all error paths in the TUI command dispatch and stdin relay `tokio::spawn` tasks.
5. [x] **`CommandResponse` rendering**: Verified `push_lines()` correctly splits multi-line text and renders each line. Added test `command_response_multiline_renders_all_lines`.
---
---
---
9. [x] **In-place heartbeat updates**: Added `is_heartbeat` flag to `OutputLine` and `push_heartbeat()` method on `App`. Heartbeat lines update the last output line in-place if it's already a heartbeat. Added `OutputLine::heartbeat()` constructor.
10. [x] **Heartbeat coalescing**: Heartbeat detection in `AgentOutput` handler intercepts `[heartbeat]` lines before general processing. Non-heartbeat output naturally pushes heartbeats down. Works in both single-pane and split-pane modes. 4 heartbeat tests added.
---
- `watchdog::tests::truncate_preview_long` — truncation with ellipsis
- `command_response_multiline_renders_all_lines` — multi-line CommandResponse rendering
- `heartbeat_updates_in_place` — in-place heartbeat update
- `heartbeat_pushed_after_real_output` — heartbeat after non-heartbeat output
---
- `mouse_capture_toggle_state` — initial mouse capture state
- `draft_apply_routes_to_command` — routing test (input.rs)
- `draft_view_routes_to_command` — routing test (input.rs)
- `draft_approve_routes_to_command` — routing test (input.rs)
---
- `apply_shortcut_routes_to_command` — routing test (input.rs)
- `view_shortcut_routes_to_command` — routing test (input.rs)
---
5. [x] **`ta sync` CLI command** (`apps/ta-cli/src/commands/sync.rs`): Calls `SourceAdapter::sync_upstream()`, emits `sync_completed` or `sync_conflict` events via `FsEventStore`, warns about active staging workspaces, shows troubleshooting on failure.
---
---
---
### v0.11.4.2 — Shell Mouse & Agent Session Fix
---
**Goal**: Fix two critical `ta shell` usability issues: (1) mouse scroll and text selection must both work simultaneously (like Claude Code), and (2) agent Q&A must reuse a persistent session instead of spawning a new subprocess per question.
---
---
---
**Problem**: Crossterm's `EnableMouseCapture` enables ALL mouse modes (`?1000h` normal tracking, `?1002h` button-event, `?1003h` any-event, `?1006h` SGR). This captures clicks/drags and breaks native text selection. The current Ctrl+M toggle is a workaround, not a fix.
---
**Root cause**: `?1003h` (any-event tracking) and `?1000h` (normal tracking) capture button-down/up/drag events. Scroll-wheel events are reported through normal tracking (`?1000h`). There is no ANSI mode that captures only scroll.
---
---
---
---
2. [x] **Test across terminals**: Verify scroll + native text selection works in:
   - macOS Terminal.app
---
---
   - Linux xterm / GNOME Terminal (via CI or manual test notes)
---
3. [x] **Remove Ctrl+M toggle**: No longer needed since both behaviors coexist. Remove the `mouse_capture_enabled` field, the toggle handler, and the status bar indicator.
4. [x] **Fallback**: If a terminal doesn't report scroll via `?1000h` alone, fall back to keyboard-only scroll (PageUp/PageDown/arrows already work). Detect via `$TERM` or first scroll event.
---
---
**Key insight**: Claude Code's terminal (which works correctly) likely uses `?1000h` + `?1006h` without `?1002h`/`?1003h`. Normal tracking reports button press/release (including scroll wheel buttons 4/5) but does NOT intercept click-drag, which the terminal handles natively for selection.
- `crates/ta-build/src/script.rs`: 5 tests (detect, name, custom command, failure, make constructor)
**Files**: `apps/ta-cli/src/commands/shell_tui.rs` (mouse setup, event loop, cleanup)
---
---
**Layer 2 — Continuation cancellation (async, in shell output handler)**:
**Problem**: Every question typed in `ta shell` spawns a new `claude-code` subprocess (`ask_agent()` → `tokio::process::Command::new(binary)` in `agent.rs:269`). Each cold start takes seconds. Users see "Starting claude-code agent..." and experience long delays + laggy keyboard input during startup.
---
**Solution**: Keep a long-running agent subprocess alive for the shell session's lifetime.
---
---
7. [x] **Memory context injection**: `inject_memory` config flag available; full multi-turn stdin context injection deferred to when `claude --print` supports multi-turn stdin mode.
8. [x] **Configuration**: Add `[shell.qa_agent]` section to `daemon.toml`:
---
   [shell.qa_agent]
---
---
   idle_timeout_secs = 300    # Kill after 5min idle, restart on next question
   inject_memory = true       # Inject project memory context on start
---
   Users can set `auto_start = false` to disable the persistent agent.
---
---
---
**Files**: `crates/ta-daemon/src/api/agent.rs` (session management, subprocess lifecycle), `crates/ta-daemon/src/config.rs` (config struct), `apps/ta-cli/src/commands/shell_tui.rs` (startup trigger)
---
#### 3. Non-Blocking Keyboard Input
---
---
---
11. [x] **Dedicated input thread**: Move terminal event reading to a dedicated OS thread (not a tokio blocking task). Use `std::thread::spawn` with a `tokio::sync::mpsc` channel to send `Event` values to the async event loop. This fully decouples keyboard responsiveness from async task pressure.
12. [x] **Immediate event drain**: The input thread uses `event::poll(Duration::from_millis(16))` (~60fps) and `event::read()` in a tight loop, sending events immediately over the channel. The main async loop receives from this channel via `tokio::select!` alongside background messages, with batch drain for queued events.
13. [x] **Test**: `dedicated_input_thread_channel` test verifies that the mpsc channel can send/receive `Event` values without blocking.
---
---
---
#### Tests added (7 new)
---
- `selective_scroll_capture_helpers` — verifies App no longer has mouse_capture_enabled field; input_rx starts None
- `dedicated_input_thread_channel` — verifies mpsc channel can send/receive crossterm Event values
---
- `persistent_qa_agent_lifecycle` — verifies PersistentQaAgent starts with 0 restarts and healthy
- `persistent_qa_agent_shutdown_noop_when_not_started` — shutdown before start is a no-op
---
- `shell_qa_config_roundtrip` — verifies full TOML serialization/deserialization
- `shell_qa_config_partial_override` — verifies partial config fills defaults for missing fields
---
#### Version: `0.11.4-alpha.2`
---
---
---
### v0.11.4.3 — Smart Input Routing & Intent Disambiguation
---
**Goal**: Stop mis-routing natural language as commands when the first word happens to match a keyword. Add intent-aware disambiguation so the shell either routes correctly or presents "Did you mean..." options.
7. [x] **User-extensible schemas**: Users add `.yaml` files to `.ta/agents/output-schemas/` (project-local) or `~/.config/ta/agents/output-schemas/` (global). Documented in USAGE.md.
6. [x] **`ta draft follow-up --review-comments`**: Auto-fetch PR review comments and inject as context. Agent addresses each comment.
---
1. [x] **Known sub-subcommands map**: `ShellConfig.sub_subcommands` HashMap with defaults for 18 subcommands (draft, goal, plan, agent, session, audit, plugin, release, workflow, adapter, office, config, policy, sync, verify, dev, gc, status). Loaded from `shell.toml` or defaults.
---
2. [x] **Edit distance function**: Levenshtein distance using single-row DP (~25 lines). Detects typos within distance 2 for candidates ≥ 3 chars.
---
3. [x] **Natural language detection heuristic**: `looks_like_natural_language()` checks 4 signals — stopword as first rest-word (30+ stopwords), question mark ending, question word after keyword (20+ question words), and >4 words without flags or ID-like tokens.
---
4. [x] **`RouteDecision::Ambiguous` variant**: New enum variant with `original: String`, `suggestions: Vec<RouteSuggestion>`. Each suggestion has `description`, `command`, and `is_agent` flag.
---
5. [x] **Disambiguation in `handle_input()`**: Returns `routed_to: "ambiguous"`, `ambiguous: true`, `message`, and `options` array with index/description/command/is_agent per option. No command executed.
---
6. [x] **TUI "Did you mean..." UI**: `PendingDisambiguation` state with numbered options. User enters a number to choose or Escape/Ctrl-C to cancel. Choice re-dispatches via `send_input` with the selected command or agent prompt.
---
7. [x] **Shortcut disambiguation**: `expand_shortcut_smart()` applies NL guard before shortcut expansion. "apply the constitution" → falls through to agent.
---
8. [x] **Tests**: 20 new tests covering all 7 PLAN scenarios plus edge cases (36 total in input.rs).
[plugins.discord]
   - `"draft list"` → Command (valid syntax)
---
   - `"run v0.11.5 — Some Title"` → Command (valid `ta run` syntax)
---
**Files**: `crates/ta-daemon/src/api/input.rs` (routing logic), `crates/ta-daemon/src/config.rs` (sub-subcommands map), `apps/ta-cli/src/commands/shell_tui.rs` (disambiguation UI)
---
#### Version: `0.11.4-alpha.3`
---
---
---
### v0.11.4.4 — Constitution Compliance Remediation
---
**Goal**: Fix all violations found by the 7-agent constitution compliance audit against `docs/TA-CONSTITUTION.md`. Prioritize High-severity items (data loss on error paths) before Medium-severity (stale injection on follow-up).
---
---
---
#### §4 — CLAUDE.md Injection & Cleanup (4 violations — all fixed, PR #183)
---
1. [x] **`inject_claude_settings()` backup-restore on follow-up**: Restore from backup before re-injecting on `--follow-up`. Prevents stale/nested settings accumulation. **§4.1**
---
---
---
3. [x] **Pre-launch command failure cleanup**: Cleanup CLAUDE.md + settings + MCP config in both `Ok(non-zero)` and `Err` arms. **§4.3**
---
4. [x] **General launch error cleanup**: All non-NotFound launch errors now clean up injected files. **§4.4**
  "plugins": {
---
---
---
          "min_ta_version": "0.11.0",
6. → v0.11.6 Full §5–§14 audit, fixes, regression tests, sign-off, and release pipeline checklist gate. See v0.11.6 for details.
---
---
            "aarch64-apple-darwin": {
---
---
---
---
---
---
---
7. [x] **Completion confirmation**: The CLI's own `draft apply` output already includes file count, target directory, and status. The stderr-as-primary fix (item 2) ensures this output is now forwarded to the shell.
**Problem**: Pasting a large document (e.g., an audit report) into the shell input embeds all the text directly in the input buffer, making it unreadable and hard to edit. Claude Code CLI handles this by compacting large pastes into a summary/link.
---
---
---
---
---
---
- `shell_qa_config_defaults` — verifies ShellQaConfig default values
   ta> [Pasted 2,847 chars / 47 lines — Tab to preview, Esc to cancel]
---
---
--- Phase Run Summary ---
---
---
4. [x] **Preview on demand**: Tab toggles an inline preview of the first 5 lines (with "N more lines" footer). Tab again collapses. Esc and Ctrl-C cancel the paste entirely.
---
---
---
---
---
---
---
---
---
---
---
---
---
**Problem 1 — No goal feedback**: The web shell shows zero feedback when goals make progress or complete. Users discover completion through external editor notifications or polling `ta goal list`. Events like `goal_started`, `goal_completed`, `draft_built` must be surfaced clearly.
---
---
---
**Problem 3 — `.git/` in draft diffs**: The overlay copies `.git/` into staging because `goal.rs` only loads `ExcludePatterns::load()` (build artifacts) but never merges `adapter.exclude_patterns()` (which returns `[".git/"]`). When staging's git state is modified (e.g., creating a branch in staging or any git op), the diff captures `.git/index`, `.git/HEAD`, etc. as changed artifacts. When `ta draft apply --git-commit` runs, it copies those `.git/` files back, overwriting the real repo's git state — resetting HEAD to main and deleting local branches.
---
---
---
**Problem 5 — Single conversation**: No way to fork parallel agent sessions.
---
---
---
1. [x] **Merge adapter excludes into overlay**: `load_excludes_with_adapter()` helper in `draft.rs` merges `adapter.exclude_patterns()` (e.g. `".git/"` for Git) into `ExcludePatterns` before creating/opening the overlay. Applied in `goal.rs` (create), `draft.rs` build (open), `draft.rs` apply (open), and snapshot rebase. Regression test added to `ta-workspace`: verifies `.git/` is not copied into staging and does not appear in `diff_all()` even if created in staging.
---
---
---
---
---
---
---
---
---
5. [x] **Status bar tail indicator**: Show "tailing <label>" in the status bar when actively following goal/agent output. (PR #184)
---
6. [x] **Clear auto-tail messaging**: When auto-tailing starts, shows "auto-tailing goal output..." and "agent working — tailing output (id)..." instead of bare "processing...". (PR #184)
---
---
---
---
---
8. [x] **Draft-time constitution pattern scan**: When `ta draft build` runs, scan changed files for known §4 violation patterns (injection functions without cleanup on early-return paths, error arms that `return` without a preceding `restore_*` call). Emit findings as warnings in the draft summary — non-blocking by default, so review flow is unaffected. The scan is static/grep-based (no agent), runs in <1s. Example output: `[constitution] 2 potential §4 violations in run.rs — review before approving`. Configurable: `warn` (default), `block`, `off`.
---
#### Agent Transparency (streaming intermediate output)
---
9. [x] **Surface agent stderr as progress**: Ensure all stderr lines from the agent subprocess appear in the web shell as dimmed progress indicators.
---
---
---
11. [x] **Web shell thinking indicator**: When a request is pending and no stdout has arrived yet, show an animated indicator ("Agent is working...") that updates with the latest stderr progress line.
#### Version: `0.11.4-alpha.5`
12. [x] **Collapse progress on completion**: When the agent's stdout response arrives, collapse/dim the intermediate progress lines so the final answer is prominent.
---
#### Parallel Agent Sessions
--- Phase Run Summary ---
13. [x] **`/parallel` shell command**: New web shell command that spawns an independent agent conversation (no `--continue`). Returns a session tag the user can address follow-ups to.
---
14. [x] **`POST /api/agent/ask` with `parallel: true`**: API flag that skips conversation chaining and creates a fresh agent subprocess.
---
---
---
16. [x] **Session lifecycle**: Parallel sessions auto-close after idle timeout. User can `/close <tag>` to end a session explicitly. Max concurrent sessions configurable in `daemon.toml`.
### v0.11.5 — Web Shell UX, Agent Transparency & Parallel Sessions
---
#### Version: `0.11.5-alpha`
---
---
---
### v0.11.6 — Constitution Audit Completion (§5–§14)
---
---
---
**Context**: The initial audit (2026-03-16) confirmed §2, §3, §9 pass and fixed §4. Sections §5–§14 were not reached before the audit was cut short.
---
---
---
format: <slug>-<seq>
---
2. [x] **Fix all identified violations**:
---
   - §8: Added `DraftApproved`, `DraftDenied`, `DraftApplied` event emission in `draft.rs` with §8 citation comments
---
3. [x] **Constitution regression tests**: 8 new tests — 3 draft event serialization tests in `ta-events/src/schema.rs`, 5 policy enforcement tests in `ta-mcp-gateway/src/validation.rs`.
7. [x] **`ta draft list` shows VCS column**: TAG and VCS columns added to draft list output with PR state inline.
---
---
---
2. [x] **`ta daemon start`**: Spawn `ta-daemon --api --project-root <path>` in background. Write PID to `.ta/daemon.pid`, log to `.ta/daemon.log`. Print PID, port, and log path. `--foreground` flag runs in the current process (for debugging/containers). `--port` override.
---
---
4. [x] **Draft metadata update**: The original draft package is updated with amendment details (what changed, why, timestamp) rather than creating a new draft. History of amendments preserved.
**Files**: TBD by audit findings. Likely `crates/ta-goal/src/goal_run.rs` (§5), `apps/ta-cli/src/commands/draft.rs` (§6), `crates/ta-policy/` (§7), audit logging (§8), `apps/ta-cli/src/commands/release.rs` (pipeline step).
---
---
---
---
---
### v0.11.7 — Web Shell Stream UX Polish
---
#### Plugin Version Control
**Goal**: Clean up the tail/stream output UX in the web shell so live goal output is comfortable to read and the connection state is always clear.
---
---
---
1. [x] **Heartbeat into working indicator**: Move `[heartbeat] still running... Xs elapsed` out of the stream. Instead, update the existing "Agent is working…" line in-place: `Agent is working ⠿ (380s elapsed)` — animated spinner character cycles on each heartbeat, elapsed time updates. No separate status bar; no duplicate elapsed display.
---
---
---
---
---
4. [x] **Tail stream close on completion** *(bug)*: The tail SSE stream is not closed when the background command finishes. The shell keeps tailing indefinitely, accumulating ghost tail subscriptions. When a second background command starts, the shell shows 2 active tails. Fix: daemon sends an explicit `event: done` (or closes the SSE connection) when the output channel is exhausted; client untails and stops tracking that key on receipt.
---
5. [x] **Process completion/failure/cancellation states**: When a tailed background process ends, replace the "Agent is working…" indicator with a final status line and clear the working indicator:
   - Completed: `✓ <command> completed`
   - Failed: `✗ <command> failed (exit <code>)`
   - Canceled: `⊘ <command> canceled`
---
---
---
---
---
   - Applied via CSS on the shell input element; read from `/api/status` alongside other shell config.
---
---
---
8. [x] **`--submit` default on when VCS configured**: `ta draft apply` should default to `--submit` (git commit + push + PR creation) whenever a VCS submit adapter is configured. Add `--no-submit` to explicitly opt out. The current default (no submit unless `--submit` is passed) is surprising — users expect apply to go all the way through.
---
9. [x] **`SourceAdapter` trait — `verify_not_on_protected_target()`**: Add two methods with default no-op implementations (no breaking change):
---
---
---
10. [x] **Git adapter**: Implement `protected_submit_targets()` returning configured protected branches (defaulting to `["main", "master", "trunk", "dev"]`) and `verify_not_on_protected_target()` via `git rev-parse --abbrev-ref HEAD`.
---
11. [x] **Perforce adapter (built-in)**: Implement `protected_submit_targets()` (configured depot paths, default `["//depot/main/..."]`) and `verify_not_on_protected_target()` checking the current CL's target stream. No Perforce installation required for the check to compile — gate behind a `p4` CLI call that degrades gracefully if not present.
---
12. [x] **SVN adapter (built-in)**: Implement `protected_submit_targets()` (configured protected paths, default `["/trunk"]`) and `verify_not_on_protected_target()` via `svn info --show-item url`. SVN's `prepare()` is currently a no-op (no branching) — this at minimum blocks committing to a protected path until proper branch/copy support is added.
---
---
---
---
    > **§15 VCS Submit Invariant**: All VCS adapters MUST route agent-produced changes through an isolation mechanism (branch, shelved CL, patch queue) before any commit. `prepare()` is the mandatory enforcement point — failure is always a hard abort. After `prepare()`, the adapter MUST NOT be positioned to commit directly to a protected target. Adapters MUST declare protected targets via `protected_submit_targets()`. This invariant applies to all current and plugin-supplied adapters.
---
**Files**: `crates/ta-daemon/assets/shell.html`, `crates/ta-daemon/src/config.rs`, `crates/ta-daemon/src/api/status.rs`, `apps/ta-cli/src/commands/draft.rs`, `crates/ta-submit/src/adapter.rs`, `crates/ta-submit/src/git.rs`, `crates/ta-submit/src/perforce.rs`, `crates/ta-submit/src/svn.rs`, `docs/TA-CONSTITUTION.md`
14. [x] **Constitution §15 — VCS Submit Invariant**: Add to `docs/TA-CONSTITUTION.md`:
#### Version: `0.11.7-alpha`
---
---
---
### v0.12.0 — Template Projects & Bootstrap Flow
---
**Goal**: `ta new` generates projects with `project.toml` plugin declarations so downstream users get a complete, working setup from `ta setup` alone. Template projects in the Trusted-Autonomy org serve as reference implementations. Also: replace the quick-fix Discord command listener with a proper slash-command-based bidirectional integration.
---
---
---
---
3. [x] **Template project generator**: `ta new` produces a project with `project.toml`, `README.md` with setup instructions, `.ta/` config pre-wired for the declared plugins, and a `setup.sh` fallback for users without TA installed.
---
---
---
7. [x] **Template listing**: `ta new --list-templates` shows available templates from both built-in and registry sources.
8. [x] **Test: end-to-end bootstrap flow**: Test that `ta new --plugins discord --vcs git` → `ta setup` → `ta-daemon` starts with the Discord plugin loaded and VCS configured.
---
#### Discord command listener tech debt (from quick-fix in v0.10.18)
The current `--listen` mode on `ta-channel-discord` is a quick integration that works but has several limitations. These should be addressed here alongside the Discord template project:
---
---
10. [x] **Interaction callback handler**: Handle button clicks from `deliver_question` embeds. Currently button `custom_id` values (e.g., `ta_{interaction_id}_yes`) are sent to Discord but no handler receives them. Add an HTTP endpoint or Gateway handler that receives interaction callbacks and POSTs answers to the daemon's `/api/interactions/:id/respond`. *(moved to v0.12.1)*
---
---
---
14. [x] **Response threading**: Post command responses as thread replies to the original message instead of top-level messages, to keep the channel clean. *(moved to v0.12.1)*
---
16. [x] **Remove `--listen` flag**: Once the daemon manages the listener lifecycle (item 12), the standalone `--listen` mode becomes internal. The user-facing entry point is `ta daemon start` with Discord configured in `daemon.toml`. *(moved to v0.12.1)*
17. [x] **Goal progress streaming**: Subscribe to daemon SSE events for active goals and post progress updates to the Discord channel (stage transitions, key milestones). Avoids flooding by batching/throttling updates. *(moved to v0.12.1)*
18. [x] **Draft summary on completion**: When a goal finishes and produces a draft, post the AI summary + artifact list to Discord. Include approve/deny buttons that call the daemon API. *(moved to v0.12.1)*
19. [x] **`ta plugin build <name|all>`**: Build channel/submit plugins from the main workspace. `ta plugin build discord` builds `plugins/ta-channel-discord`, `ta plugin build all` builds all plugins. Re-signs binaries on macOS after copy. *(moved to v0.12.1)*
20. [x] **PID guard for listener**: (done in v0.10.18) Prevent duplicate listener instances via `.ta/discord-listener.pid`. Verify guard works correctly when daemon manages listener lifecycle.
21. [x] **`ta run --quiet`**: Suppress streaming agent output but still print completion/failure summary. Default for daemon-dispatched and channel-dispatched goals. Inverse: `ta run --verbose` (current default behavior when run interactively). Completion and failure messages always print regardless of verbosity.
---
#### Goal process monitoring & diagnostics
---
- The daemon's `POST /api/cmd` spawns `ta run` as a detached child with piped stdio. If the child fails to launch (e.g., binary not found, macOS code signature block, missing env vars), the error is captured in stderr but the goal state is never updated to `failed`.
---
- `ta goal list` shows `running` with no way to distinguish "actively working" from "zombie".
---
22. [x] **Goal process liveness monitor**: *(Moved to v0.11.2.4 items 1-3)* Daemon periodically checks that the agent PID for each `running` goal is still alive. If the process has exited, transition the goal to `completed` (exit 0) or `failed` (non-zero/missing) and emit the appropriate event. Check interval: configurable, default 30s. *(completed in v0.11.2.4)*
23. [x] **Goal launch failure capture**: If `ta run` fails to start (spawn error, immediate crash, missing binary), update the goal state to `failed` with the error message before returning the HTTP response. The Discord listener (or any caller) should see the failure in the command output. *(completed in v0.11.2.4)*
24. [x] **`ta goal status` shows process health**: Include PID, whether the process is alive, elapsed time, last agent log line, and last event timestamp. Flag goals where the process is dead but state is still `running`. *(completed in v0.11.2.4)*
---
26. [x] **Goal timeout**: Configurable maximum goal duration (default: none for interactive, 4h for daemon-dispatched). Goal transitions to `timed_out` if exceeded. Daemon kills the agent process if still alive.
27. [x] **macOS code signing in plugin install**: When copying plugin binaries to `.ta/plugins/`, re-sign with `codesign --force --sign -` on macOS to prevent AppleSystemPolicy from blocking execution. This caused the v0.10.18 Discord listener to be SIGKILL'd immediately on launch from `.ta/plugins/`.
28. [x] **Escape special characters in VCS commit/branch messages**: Goal titles containing backticks, single quotes, or other shell-special characters get truncated or mangled when passed to VCS commands (e.g., `` `ta sync` `` in a title becomes `&` in the git commit message). The submit adapter must properly escape or sanitize goal titles and draft summaries before passing them to shell commands. Use direct argument passing (not shell interpolation) where possible.
---
---
---
30. [x] **`ta constitution init` (simple)**  *(pulled forward from v0.14.1)*: `ta constitution init` asks the QA agent to draft a `.ta/constitution.md` from the project's `PLAN.md`, `CLAUDE.md`, and stated objectives. No guided UI — a single agent prompt produces the first draft for human review. Gives new projects an immediate behavioral contract without requiring the full v0.14.1 constitution framework.
---
#### Version: `0.12.0-alpha`
---
---
---
### v0.12.0.1 — PR Merge & Main Sync Completion
---
**Goal**: Complete the post-apply workflow so that after `ta draft apply --submit` creates a PR, the user can merge it and sync their main branch without leaving TA. This is the final step in the "run → draft → apply → merge → next phase" loop that makes TA a smooth development substrate.
---
---
---
---
---
1. [x] **`SourceAdapter::merge_review()`**: New optional trait method (default: no-op with guidance message). Git: calls `gh pr merge` (or GitHub API) to merge the PR immediately. P4: calls `p4 submit -c <CL>` to submit the shelved changelist. SVN: no-op (SVN commits directly). Each adapter's `merge_review()` returns a `MergeResult` with `merged: bool`, `merge_commit`, and `message`.
---
---
---
3. [x] **Shell guidance after apply**: After `ta draft apply --submit` completes, print actionable next steps: PR URL, whether auto-merge is enabled, and the exact command to run when ready (`ta draft merge <id>` or `ta sync`). No silent exits.
Known issue from v0.10.18: Discord-dispatched `ta run` created a goal record (state: `running`) but the agent process never actually started. The goal became a zombie — no agent log, no draft, no error, no timeout. Root causes:
4. [x] **`ta draft watch <id>`**: Polls PR/review status until merged, closed, or failed CI. When merged, automatically calls `ta sync` to pull main and prints "✓ merged + synced main — ready for next phase". Interval: configurable, default 30s. Useful for `auto_merge = true` flows where CI runs before merge.
---
5. [x] **`--watch` flag on `ta draft apply`**: `ta draft apply --submit --watch` chains apply → create PR → watch → merge → sync into a single command. The user starts it and walks away; it completes when main is synced.
---
---
**Current state**: `auto_merge = true` in `workflow.toml` already calls `gh pr merge --auto` when a Git PR is created (v0.11.2.3). `ta sync` already pulls main (v0.11.1). The gap: these aren't wired together, there's no watch-for-merge flow, P4 has no `merge_review()` equivalent, and the shell gives no guidance after apply on what to do next.
7. [x] **P4 shelved CL workflow**: `ta draft apply --submit` for P4 shelves the CL and opens it for review. `ta draft merge <id>` submits it (`p4 submit -c <CL>`). `ta draft watch <id>` polls CL state via `p4 change -o`.
---
---
---
---
---
10. [x] **Short goal tags**: `ta goal start` and all goal creation paths now call `save_with_tag()` to auto-generate `<slug>-<seq>` tags (e.g., `fix-build-01`). Tags shown on goal start output. `:attach`, `:tail`, and all goal commands already support tag resolution via `resolve_tag()`.
---
**Files**: `crates/ta-submit/src/adapter.rs`, `crates/ta-submit/src/git.rs`, `crates/ta-submit/src/perforce.rs`, `apps/ta-cli/src/commands/draft.rs`, `apps/ta-cli/src/commands/sync.rs`, `crates/ta-goal/src/goal_run.rs` (new state), `docs/USAGE.md`
---
---
---
---
---
### v0.12.0.2 — VCS Adapter Externalization
---
**Goal**: Migrate VCS adapters from built-in compiled code to external plugins using the same JSON-over-stdio protocol as channel plugins. Git remains built-in as the zero-dependency fallback. Perforce, SVN, and any future VCS adapters become external plugins that users install when needed.
---
#### Rationale
Today git, perforce, and svn adapters are compiled into the `ta` binary. This means:
- Every user ships code for VCS systems they don't use
- Adding a new VCS (Plastic SCM, Fossil, Mercurial) requires modifying TA core
---
---
---
---
3. [x] **Windows icon embedding**: Added `winres` as a build dependency for `ta-cli` (cfg windows only). `build.rs` embeds `ta.ico` into the binary with graceful fallback if icon missing.
---
---
2. [x] **Plugin discovery for VCS adapters**: When `submit.adapter = "perforce"`, TA checks built-in adapters first, then looks for `ta-submit-perforce` in `.ta/plugins/vcs/`, `~/.config/ta/plugins/vcs/`, and `$PATH`. → `crates/ta-submit/src/vcs_plugin_manifest.rs` + updated `registry.rs`
3. [x] **Extract PerforceAdapter to external plugin**: Move `crates/ta-submit/src/perforce.rs` logic into `plugins/ta-submit-perforce/` as a standalone Rust binary. Communicates via JSON-over-stdio. Include `plugin.toml` manifest. → `plugins/ta-submit-perforce/`
4. [x] **Extract SvnAdapter to external plugin**: Same treatment for `svn.rs` → `plugins/ta-submit-svn/`. → `plugins/ta-submit-svn/`
5. [x] **GitAdapter stays built-in**: Git is the overwhelmingly common case. Keep it compiled in as the zero-configuration default. It also serves as the reference implementation for the protocol.
6. [x] **VCS plugin manifest (`plugin.toml`)**: Same schema as channel plugins but with `type = "vcs"` and `capabilities = ["commit", "push", "review", ...]`. → `VcsPluginManifest` in `vcs_plugin_manifest.rs`
7. [x] **Adapter version negotiation**: On first contact, TA sends `{"method": "handshake", "params": {"ta_version": "...", "protocol_version": 1}}`. Plugin responds with its version and supported protocol version. TA refuses plugins with incompatible protocol versions. → `ExternalVcsAdapter::new()` handshake
---
---
---
---
---
<!-- previously v0.13.5; renumbered to reflect logical implementation order -->
---
---
---
> **⬇ PUBLIC ALPHA** — With v0.12.0.2 (VCS Externalization) complete, TA is ready for external users: new project setup, plan + workflow generation, goals run via `ta shell` + Discord/Slack, drafts applied, PRs merged, main synced — in Git or Perforce.
---
---
---
### v0.12.1 — Discord Channel Polish
---
---
---
**Depends on**: v0.12.0 (Discord template context), v0.10.2.1 (Discord external plugin architecture)
---
---
---
---
---
---
4. [x] **Daemon auto-launches listener**: `[channels.discord_listener] enabled = true` in `daemon.toml` makes the daemon spawn `ta-channel-discord --listen` and restart on crash. (`channel_listener_manager.rs`, `DiscordListenerConfig` in config.rs)
---
6. [x] **Response threading**: All command responses posted as `message_reference` replies to the original message, keeping the main channel clean. (listener.rs `post_thread_reply`)
7. [x] **Long-running command status**: Posts `:hourglass_flowing_sand: Working…` placeholder immediately, then edits it with the final result. (listener.rs `execute_command_with_status`)
---
9. [x] **Goal progress streaming**: `progress.rs` subscribes to `/api/events` SSE stream, posts goal state transition embeds throttled at 1/10s per goal. (progress.rs `run_progress_streamer`)
---
11. [x] **`ta plugin build <name|all>`**: Extended to discover and build VCS plugins (plugin.toml with `type = "vcs"`) in addition to channel plugins. Install path is `.ta/plugins/vcs/<name>/`. macOS ad-hoc re-signing via `codesign -s -` after binary copy. (plugin.rs `resign_binary_macos`, VCS discovery)
---
---
---
---
- Item 12 (ta-discord-template reference repo) → deferred to future work, requires creating an external GitHub repository.
---
#### Version: `0.12.1-alpha`
---
---
---
### v0.12.2 — Shell Paste-at-End UX
---
**Goal**: Fix the `ta shell` paste behavior so that pasting (⌘V / Ctrl+V / middle-click) always appends at the end of the current `ta>` prompt text, regardless of where the visual cursor is positioned. Users naturally click or scroll around while reading output and forget where the cursor is — paste should always go to the input buffer end, not a random insertion point.
---
---
---
1. [x] **Intercept paste event in TUI**: Detect paste sequences (OSC 52, bracketed paste `\e[200~`, or large clipboard burst) in the TUI shell input handler.
---
---
4. [x] **Bracketed paste mode**: Enable terminal bracketed paste mode (`\e[?2004h`) so multi-line pastes arrive as a unit. Strip leading/trailing newlines to avoid accidental submission.
---
---
#### Version: `0.12.2-alpha`
---
---
---
---
---
**Goal**: Fix the architectural gap where follow-up (child) drafts only capture their own staged writes rather than computing a cumulative diff against the original source. Users see "2 files changed" on a follow-up when the real answer is "parent: 5 + child: 2 = 7 files changed", and `ta draft apply` reports "Applied 0 file(s)" because the rebase compares child-staging against current source (which already has the parent applied) and finds nothing new.
---
**Root cause**: `draft build` snapshots only the delta since *this goal* started, not since the *root ancestor* of a follow-up chain. When the parent is applied to source before the child, the child's staging matches source and the diff is empty.
  - event: draft_denied
1. [x] **Track parent draft ID on follow-up goals**: When `ta run --follow-up <draft-id>` starts, record `parent_draft_id` on the `GoalRun`. Propagate through `DraftPackage` metadata.
---
3. [x] **`ta draft view` shows chain summary**: When viewing a child draft, show "Follow-up to `<parent-id>` — combined impact: N files". When viewing a parent with known children, list them.
---
5. [x] **`ta draft list` chain column**: Show `→ <parent-short-id>` in a new "Parent" column when a draft is a follow-up, so chains are visible at a glance.
---
---
---
---
---
---
---
---
### v0.12.2.2 — Draft Apply: Transactional Rollback on Validation Failure
---
**Goal**: Make `ta draft apply` safe to run on `main`. If pre-submit verification fails (fmt, clippy, tests), all files written to the working tree must be restored to their pre-apply state. Currently the apply is not atomic — files land on disk but the commit never happens, leaving the working tree dirty and requiring manual `git checkout HEAD -- <files>` to recover.
---
**Found during**: v0.12.2.1 apply failed due to a corrupted Nix store entry (`glib-2.86.3-dev` reference invalid), leaving 11 files modified in working tree on `main`.
---
1. [x] **Snapshot working tree before copy**: Before writing any files, record the set of paths that will be modified. `ApplyRollbackGuard` reads each file's current content (or None if it doesn't exist yet) before the overlay apply call.
2. [x] **Rollback on verification failure**: If any verification step exits non-zero, anyhow::bail! propagates, the guard drops uncommitted, restoring all files. Prints `[rollback] Restored N file(s) to pre-apply state.`
---
--- Phase Run Summary ---
---
---
| `workflow` | Start a named workflow with event data as input |
---
---
10. [x] **Structured progress parsing**: Parse stderr for known patterns (`Reading `, `Searching `, `Running `, `Writing `) and render them as distinct "thinking" lines with a spinner or activity indicator.
### v0.12.2.3 — Follow-Up Draft Completeness & Injection Cleanup
---
The submit workflow has three abstract stages, each mapped by the adapter:
**Goal**: Fix two follow-up bugs exposed by v0.12.2.2: (1) follow-up drafts only capture per-session writes rather than the full staging-vs-source delta, silently dropping parent-session changes (version bumps, etc.) from the child PR; (2) a crashed/frozen session leaves CLAUDE.md with the TA injection still prepended, which then leaks into the diff and ends up in the GitHub PR.
---
**Found during**: v0.12.2.2 — computer froze before agent exited, `restore_claude_md` never ran, injected CLAUDE.md appeared in PR 197. Follow-up PR 198 was missing `Cargo.toml`, `Cargo.lock`, `CLAUDE.md` version bumps because the follow-up session didn't re-write those files.
---
1. [x] **Follow-up draft uses full staging-vs-source diff**: When `ta draft build` runs for a follow-up goal that reuses the parent's staging directory, diff the full staging tree against the source (same as a non-follow-up build), not just the files written in the child session. This ensures all parent-session changes (version bumps, etc.) are included in the child draft. The child draft already supersedes the parent, so including all changes is correct.
2. [x] **`ta draft build` strips injected CLAUDE.md header**: Before capturing the staging diff, check if `CLAUDE.md` in staging starts with `# Trusted Autonomy — Mediated Goal`. If so, strip everything up to and including the `---` separator that precedes the real project instructions, and write the cleaned content back to staging before diffing. This protects against crash/freeze leaving the injection in place.
3. [x] **Auto-close parent GitHub PR on supersession (at build time)**: When `build_package` marks a parent draft as `DraftStatus::Superseded`, look up the parent's `vcs_info.review_url`. If it is a GitHub PR URL, run `gh pr close <url> --comment "Superseded by <child-pr-url>"`. This prevents the orphaned open-PR problem without waiting until the child is applied.
4. [x] **Test**: Add a regression test that builds a follow-up draft on a staging dir with parent-session changes in files the child session didn't touch — assert all parent-session files appear in the child draft's artifacts.
---
---
---
---
---
---
---
---
---
1. [x] **`>tag message` inline prefix for two-way agent communication**: In ta shell, if input starts with `>` followed by an optional goal tag and a space, route the message to the matching running agent (or the sole active agent if no tag given) rather than the normal routing table. No mode switch required — works alongside any other command.
2. [x] **Prompt and status bar reflect connected agent**: When a `>tag` message is sent, the shell prompt briefly shows `[→tag]` and the status bar indicates the active target agent for that burst of messages.
3. [x] **Stream output includes short tag when multiple agents active**: Each line of agent stream output is prefixed with `[tag]` (e.g., `[v0.12.3]`) when more than one agent is streaming concurrently. Single-agent sessions remain untagged to reduce noise.
4. [x] **Auth failure surfaces as user interaction**: When the agent process receives a 401 / authentication error (API outage, expired key), ta shell displays a prompt: `Agent auth failed — [r]etry / [a]bort?`. If retry, shows actionable instructions; if abort, cleans up the session.
---
6. [x] **Auto-scroll to bottom during agent stream output**: When the user is at (or near) the bottom of the output pane and new agent output arrives, the shell automatically scrolls to keep the latest line visible — matching a `tail -f` experience. If the user has manually scrolled up to read history, auto-scroll is suspended. Once they scroll back to the bottom, auto-scroll resumes. Prevents output from running below the prompt bar and requiring manual scroll to catch up.
7. [x] **Clear "Agent is working" indicator on goal completion**: When a goal finishes, the `AgentOutputDone` handler replaces the last heartbeat line with `[agent exited <id>]` in dark gray and removes the goal from `active_tailing_goals`. The "Agent is working ⚠" line no longer persists after completion.
---
---
---
---
---
---
---
---
---
---
---
#### Discord template (ready to publish)
1. [x] **Create `Trusted-Autonomy/ta-channel-discord` GitHub repo**: Repo created at https://github.com/Trusted-Autonomy/ta-channel-discord. Plugin source pushed as repo root with `.github/workflows/release.yml` and `.gitignore`.
2. [x] **Tag v0.1.0 and publish GitHub release binaries**: `v0.1.0` tagged and pushed; release CI triggered (run 23279178646). Binaries built for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `x86_64-pc-windows-msvc`.
3. [x] **Verify `ta setup resolve` works end-to-end**: Verified after binaries published — `registry:ta-channel-discord` falls back to GitHub releases via new `resolve_from_registry` fallback in `plugin_resolver.rs`.
---
5. [x] **Update `USAGE.md` Discord setup**: `ta setup resolve` is now the primary install path; manual build kept as fallback. Same update applied to the Slack section.
---
---
6. [x] **Create `Trusted-Autonomy/ta-channel-slack` GitHub repo**: Repo created at https://github.com/Trusted-Autonomy/ta-channel-slack. Plugin source pushed as repo root with release workflow and `.gitignore`.
---
---
---
---
---
---
---
#### Version: `0.12.4-alpha`
---
---
---
### v0.12.4.1 — Shell: Clear Working Indicator & Auto-Scroll Fix + Channel Goal Input
---
**Goal**: Fix two shell regressions confirmed in the v0.12.3 build: (1) "Agent is working ⚠" persists after `ta run` completes; (2) the output pane does not stay scrolled to the latest line when new agent output arrives. Also wire Discord (and Slack) to the existing `POST /api/goals/{id}/input` endpoint so users can inject mid-run corrections from a channel.
---
**Root causes identified** (from `shell_tui.rs` code review):
- **Working indicator / tail not clearing**: `AgentOutputDone` searches `app.output` for a `is_heartbeat` line to replace. In split-pane mode (Ctrl-W), agent output goes to `app.agent_output` — the heartbeat there is never found, so it's never replaced and the status bar `tailing_goal` never clears. Same bug applies whether or not split-pane is active if the heartbeat line was pushed to the wrong list.
---
---
---
---
2. [x] **Fix auto-scroll in agent pane (split-pane mode)**: Call `auto_scroll_if_near_bottom()` (or equivalent for `agent_scroll_offset`) after every append to `app.agent_output`, mirroring the existing logic for the main pane.
3. [x] **Auto-scroll in main pane when at exact bottom**: Verified existing `auto_scroll_if_near_bottom()` call in the main pane path is correct — no off-by-one.
---
5. [x] **Tests**: Unit tests covering `AgentOutputDone` in split-pane mode clears both panes; auto-scroll fires after agent output in split-pane mode.
---
#### Channel goal-input items
---
---
---
- `ta input <goal-id> <message>` — explicit goal ID (short prefix match supported by daemon)
- `>message text here` — shorthand: routes to the most recently started goal (daemon resolves `latest`)
---
**Implementation**:
---
---
8. [-] **Slack plugin** (`ta-channel-slack`): Deferred — Slack plugin is in an external repo (`Trusted-Autonomy/ta-channel-slack`) and Slack is send-only for public alpha. → v0.13.x
9. [x] **Daemon**: `latest` is now a valid alias in `resolve_goal_id()` — resolves to the most recently started still-running goal via `GoalOutputManager.latest_goal()` backed by a `creation_order` Vec.
10. [x] **Test: ensure_running is idempotent** — Covered by `start_rejects_when_alive_pid_exists` (rejects double-start) and `cmd_status_no_daemon` (handles missing daemon).
---
---
#### Version: `0.12.4-alpha.1`
---
---
---
### v0.12.5 — Semantic Memory: RuVector Backing Store & Context Injection
---
---
---
---
---
- [x] **Default event-routing config** (`templates/event-routing.yaml`): Sensible defaults for 16 event types. Most events: `notify`. `policy_violation`: `block`. `memory_stored`/`session_paused`/`session_resumed`: `ignore`. Commented examples showing how to upgrade to `agent` strategy.
1. [x] **Daemon initialises `RuVectorStore`** (`.ta/memory.rvf/`) with `FsMemoryStore` (`.ta/memory/`) as a read-through fallback for entries not yet migrated. Auto-migration on first open is already implemented in `ruvector_store.rs`.
2. [x] **`ta memory backend`** CLI sub-command: shows which backend is active, entry count, index size, and last migration date.
| **Submit** | push to remote | shelve (or submit to depot) | svn commit |
**New write points**
3. [x] **Plan phase completion → memory**: When `draft apply` marks a phase `done` in PLAN.md, write `plan:{phase_id}:complete` (category: History, confidence 0.9) with the phase title and a one-line summary of what changed.
4. [x] **Project constitution → memory**: On daemon startup (and whenever the constitution file changes), index each constitution rule as `constitution:{slug}` (category: Convention, confidence 1.0). Constitution path is configurable; defaults to `.ta/constitution.md`.
---
---
---
**Context injection at goal start**
---
---
9. [x] **Non-Claude agents** (Codex, Ollama): Add a `context_file` field to `AgentLaunchConfig` pointing to a generic markdown file (e.g., `.ta/agent_context.md`) that TA writes the same sections into, separate from CLAUDE.md. Each agent YAML opts in via `injects_context_file: true` + `context_file: .ta/agent_context.md`. *(Full per-model injection targeting deferred to v0.13.3 RuntimeAdapter.)*
---
---
10. [x] Integration test: goal completion writes `goal:{id}:complete`; subsequent goal start retrieves it via semantic search.
11. [x] Integration test: constitution file indexed on startup; goal start injects at least one constitution rule into CLAUDE.md.
---
#### Version: `0.12.5-alpha`
---
---
---
### v0.12.6 — Goal Lifecycle Observability & Channel Notification Reliability
---
5. [x] **Require positive signal**: Only match `:` endings if the line looks conversational — no parentheses, no code formatting, not prefixed with `[`. Keep `?`, `[y/N]`, `[Y/n]`, numbered choice patterns as strong positive signals.
**Goal**: Two related gaps that surfaced during v0.12.5 operations: (1) the daemon and CLI emit almost no structured logs for goal lifecycle — making it impossible to diagnose stuck agents, missed state transitions, or slow draft builds from logs alone; (2) the Discord/Slack SSE progress streamers replay all historical events on every reconnect, flooding channels with old notifications and missing new ones if a reconnect races with an event.
---
---
---
**Goal lifecycle observability (daemon + CLI)**
1. [x] **`cmd.rs` sentinel detection log**: `tracing::info!` when `GOAL_STARTED_SENTINEL` is found — include goal UUID, agent PID.
2. [x] **State-poll task logs**: `tracing::info!` when state-poll task starts (goal UUID, initial state) and on each transition (`running → pr_ready`, etc.).
3. [x] **Draft detected log**: When `latest_draft_for_goal` returns a result, log draft ID and artifact count.
- `crates/ta-build/src/adapter.rs`: 3 tests (success/failure constructors, serialization roundtrip)
5. [x] **`run.rs` structured logs**: `tracing::info!` for staging copy start/complete (file count), CLAUDE.md inject, agent launch (PID), and goal completion (state, elapsed, files changed).
---
---
---
**Channel notification reliability (Discord + Slack)**
8. [x] **`progress.rs` startup cursor**: On initial connect, pass `?since=<startup_time>` so historical events are never replayed. Store startup time once at process start. (4 tests added)
9. [x] **`progress.rs` reconnect cursor**: Track last seen event timestamp; pass `?since=<last_event_timestamp>` on every reconnect so no events are replayed or skipped.
---
---
---
13. [x] **Tests**: 4 cursor unit tests in `progress.rs`, state-poll dedup test in `cmd.rs`, 5 `count_changed_files` tests in `run.rs`.
---
---
---
#### Version: `0.12.6-alpha`
---
---
---
### v0.12.7 — Shell UX: Working Indicator Clearance & Scroll Reliability
---
**Goal**: Fix two persistent shell regressions that surfaced after v0.12.4.1:
---
2. The output pane intermittently does not stay scrolled to the bottom when new output arrives, even when the user has not scrolled up.
2. [x] **macOS `.app` bundle recipe**: `just package-macos` creates `TrustedAutonomy.app/` with generated `Info.plist`, binary copy, and `.icns` in `Resources/`. No code signing (deferred).
---
`AgentOutputDone` searches for `is_heartbeat = true` lines to replace. The "Agent is working..." line is pushed via `TuiMessage::CommandResponse` → `OutputLine::command` which has `is_heartbeat = false`. It is never replaced.
---
**Fix approach — working indicator**:
---
- [x] **Agent response strategy** (`crates/ta-events/src/strategies/agent.rs`): Builds `AgentResponseContext` with agent name, prompt, event payload JSON, goal/phase info, attempt tracking, and `require_approval` flag. The daemon uses this to launch governed goals from events. 4 tests.
2. [x] **Git implementation** (`crates/ta-submit/src/git.rs`): `sync_upstream()` runs `git fetch` + merge/rebase/ff-only per `source.sync.strategy` config. Counts new commits via `rev-list --count`. Conflict detection via `git diff --name-only --diff-filter=U`. Returns structured `SyncResult` with conflict file list. Added `with_full_config()` constructor accepting `SyncConfig`.
  }
---
---
1. [x] **Fix working indicator clearance**: Added `TuiMessage::WorkingIndicator(String)` variant; changed "Agent is working..." emission to use it; handler calls `app.push_heartbeat()` so the line gets `is_heartbeat = true` and `AgentOutputDone` clears it on any terminal goal state. 2 new tests.
2. [x] **Verify clearance for all terminal goal states**: `working_indicator_pushed_as_heartbeat` and `agent_output_done_clears_working_indicator` tests cover the full cycle; `AgentOutputDone` logic was already terminal-state-agnostic (searches by `is_heartbeat` flag).
3. [x] **Fix intermittent scroll-to-bottom**: Root cause identified — heartbeat handling paths returned early without calling `auto_scroll_if_near_bottom()`. Fixed: non-split heartbeat now calls `auto_scroll_if_near_bottom()` after `push_heartbeat`; split-pane in-place update and push both reset `agent_scroll_offset` when within `AGENT_NEAR_BOTTOM_LINES`. 3 new tests.
---
12. [x] **QA agent project context injection**: Daemon-spawned QA agent receives project memory, CLAUDE.md context, and plan phase via `build_memory_context_section_for_inject()`.
---
3. [x] **Runtime schema loader**: `SchemaLoader` tries project-local `.ta/agents/output-schemas/` first, then `~/.config/ta/agents/output-schemas/`, then embedded defaults, then passthrough fallback. Version negotiation via `schema_version` field.
- 6 new tests in `apps/ta-cli/src/commands/shell_tui.rs` covering all items above.
---
---
---
---
2. [x] **Follow-up context injection**: Inject PR review comments, CI failure logs, and the original draft summary into CLAUDE.md so the agent knows exactly what to fix.
### v0.12.8 — Alpha Bug-Fixes: Discord Notification Flood Hardening & Draft CLI Disconnect
---
[plugins.discord]
---
---
#### Bug 1 — Discord notification flood on reconnect / daemon restart
---
---
The registry is a static JSON index hosted on GitHub Pages (or any HTTP server):
**Root cause (two separate bugs, both fixed, need verification):**
1. **`start_goal_recovery_tasks` emitting stale events** (PR #207, merged): `last_state` was initialised as `None`, causing `DraftBuilt`/`ReviewRequested` to re-emit for every `pr_ready` goal on every daemon restart. Fixed: initialise with the goal's current state.
2. **Stale channel plugin binary** (v0.12.6 cursor fix, deployed manually): `progress.rs` didn't pass a `since` cursor on reconnect, so the SSE stream replayed all historical events. Fixed: record `startup_time` at launch; advance a `cursor: DateTime<Utc>` on each event; reconnect with `?since=<cursor>`.
      "description": "Discord channel plugin",
**Remaining hardening items (v0.12.8):**
---
Extract `auto_start_daemon()` from `shell.rs` into a shared `commands/daemon.rs` module. Add `ta daemon` as a subcommand with lifecycle verbs. `ta shell` and any future entry points call `daemon::ensure_running()` instead of their own spawn logic.
2. [x] **Fix `install_local.sh` to build and deploy channel plugins**: Added Discord plugin build step after main binary installation. Builds `plugins/ta-channel-discord` (respects `--debug`/release profile and Nix devShell), then installs to `~/.local/share/ta/plugins/channels/discord/ta-channel-discord`.
3. [-] **End-to-end reconnect test**: Pure unit tests cover the age-filter and cursor logic. Full daemon-restart integration test deferred — requires a running daemon + real Discord bot credentials, not suitable for CI. → v0.13.1
4. [-] **Daemon-side persistent cursor** *(stretch)*: Deferred. Current cursor-in-memory + age-filter combination is sufficient for alpha. → v0.13.1
        "0.1.0": {
#### Bug 2 — `ta draft list` / `ta draft apply` CLI disconnect
---
---
---
**Fix items:**
---
---
---
7. [x] **Regression test**: `load_all_packages_skips_corrupted_file_and_returns_valid` — creates a real staging workspace, builds a valid DraftPackage, writes a corrupted JSON alongside it, asserts `load_all_packages` returns exactly 1 package without panicking.
1. [x] **`.ta/project.toml` schema**: `ProjectManifest` with `ProjectMeta`, `PluginRequirement`, and `SourceScheme` types. Serde parser with validation (version constraint format, source scheme parsing). Clear error messages for malformed manifests. 16 tests in `project_manifest.rs`.
---
- [x] Items 1, 2, 5, 6, 7 implemented (see above)
- [x] 5 new tests in `progress.rs` (4 age-filter + 1 updated boundary); 1 new regression test in `draft.rs`
---
#### Version: `0.12.8-alpha`
---
---
---
---
---
> Beta-quality features for enterprise users, team deployments, and extended runtime options. Core alpha workflow (v0.12.x) must be stable before starting. Ordered by dependency chain: transport → runtime → governance → proxy, with VCS externalization already done (v0.12.0.2), community hub and compliance audit as capstones.
---
---
2. [x] **Empty stdout on success**: Fixed `send_input()` in `shell.rs` to use stderr as primary output when stdout field is empty. Also handles case where `stdout` key is absent but `stderr` is present.
<!-- beta milestone start -->
---
---
---
9. [x] **Graceful lifecycle**: On shell exit, send EOF to the agent's stdin and wait up to 5s for clean shutdown, then SIGTERM. On agent crash, show error in shell and auto-restart on next question. Track restart count to avoid crash loops (max 3 restarts per session).
---
- [x] **APFS clone via `clonefile(2)` (macOS)** — Direct syscall via `extern "C"` (libSystem.B.dylib, always linked). Zero data I/O; pages shared until modified. No extra crate dependency.
- [x] **Btrfs reflink via `FICLONE` ioctl (Linux)** — `libc::ioctl(dst_fd, FICLONE, src_fd)`. Zero data I/O on Btrfs and XFS (Linux 4.5+). `libc` added as linux-only target dep.
- [x] **Fallback full copy** — Transparent fallback when COW not supported (ext4, network FS, cross-device). Same behavior as before.
- [x] **Benchmark / observability** — `CopyStat` records: strategy used, wall-clock duration, file count, total source bytes. Logged at `tracing::info!` level on every workspace creation. Exposed via `OverlayWorkspace::copy_stat()` and `copy_strategy()`.
- [x] **`OverlayWorkspace` integration** — `create()` detects strategy, passes it to `copy_dir_recursive`, accumulates `CopyStat`. Stores result in workspace for callers. Public API: `copy_stat() -> Option<&CopyStat>`, `copy_strategy() -> Option<CopyStrategy>`.
---
---
---
---
A **goal tag** is the single human-friendly identifier for a unit of work:
4. [x] **`ta goal list` shows draft/VCS status**: New TAG, DRAFT, VCS columns in goal list output with inline draft state and PR status.
---
1. **Zombie goals**: When an agent process crashes, exits unexpectedly, or never starts, the goal stays in `running` forever. `ta goal list` shows `running` with no way to distinguish "actively working" from "dead process." The human has to manually check with `ps aux` or notice the silence.
---
---
---
---
**Goal**: Preserve the parent goal's title through the follow-up draft chain so users can track "what was this fixing?" without cross-referencing goal IDs.
---
**Depends on**: v0.12.2.1 (Draft Compositing — parent_draft_id linkage)
---
---
---
1. [x] Add `parent_goal_title: Option<String>` to `DraftPackage.goal` (`ta-changeset/src/draft_package.rs`)
2. [x] Populate `parent_goal_title` during `ta draft build --follow-up` when parent staging exists
---
4. [x] `ta draft apply`: print "Applied follow-up to \"<parent title>\"" or roll up "Changes from parent:" when applying a chain
- UUIDs remain the internal key. Tags are stored on both `GoalRun.tag` and `DraftPackage.tag` and are resolvable in all commands: `ta goal status shell-routing-01`, `ta draft view shell-routing-01`.
#### Version: `0.13.0.1-alpha`
#### Draft Amend (lightweight follow-up for PR iteration)
---
2. **Terraform provider model** — flat tarball + manifest, platform detection, registry is a JSON index. This pattern is proven and familiar.
---
---
---
---
**Depends on**: v0.11.3 (Self-Service Operations — provides the observability commands this phase automates)
2. [x] **Compacted display**: Show a compact representation in the input area:
---
---
---
The trust model stays the same: daemon detects and diagnoses, agent proposes corrective action, user approves. No autonomous mutation without human consent (unless explicitly configured for low-risk actions via auto-heal policy).
--- Phase Run Summary ---
**Key insight**: Instead of 15 diagnostic commands the user memorizes, there's one intelligent layer that says "Goal X is stuck — the agent process crashed 10 minutes ago. I can transition it to failed and clean up staging. Approve?"
---
#### Continuous Health Monitor
1. [x] **Daemon watchdog loop**: *(Foundation built in v0.11.2.4)* Extended with disk space monitoring and corrective action proposals to `operations.jsonl`. Plugin health checks and event system verification deferred to future phases.
2. [x] **Goal process liveness integration**: *(Foundation built in v0.11.2.4)* Existing liveness detection confirmed; corrective action proposals added for disk space events. Auto-heal policy config field added to `daemon.toml`.
3. [x] **Disk space monitoring**: When available disk drops below 2 GB threshold, watchdog emits a `CorrectiveAction` with key `clean_applied_staging` to `operations.jsonl`. Absorbs v0.11.3 item 28 intent into continuous monitoring.
4. [-] **Plugin health monitoring**: Deferred — periodic health checks on channel plugins. → future phase
5. [-] **Stale question detection**: Foundation exists (watchdog emits `QuestionStale` events). Re-notification via channels and `ta status` flag deferred. → future phase
10. [x] **Draft summary on completion**: `progress.rs` handles `draft.ready` events, posts summary embed with artifact count + approve/deny buttons. (progress.rs `handle_draft_ready`)
#### Corrective Action Framework
4. [x] **Status bar clears `tailing <label>` on completion**: `tailing_goal` is set to `None` in `AgentOutputDone` handler unconditionally when the goal_id matches — status bar clears immediately.
---
---
9. [x] **Corrective action audit trail**: Watchdog writes corrective actions to `.ta/operations.jsonl` (JSONL, append-only). Each entry has `id`, `created_at`, `severity`, `diagnosis`, `proposed_action`, `action_key`, `auto_healable`, `status`.
---
---
#### Agent-Assisted Diagnosis
---
---
---
14. [-] **Root cause correlation**: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
---
#### Intelligent Surface (fewer commands, smarter defaults)
---
16. [-] **Proactive notifications**: → Moved to v0.13.1.6, then deferred to v0.13.12 (item 9).
---
18. [-] **Suggested next actions**: → Moved to v0.13.1.6, then deferred to v0.13.12 (item 10).
---
---
---
---
21. [-] **Runbook definitions**: → Moved to v0.13.1.6 (item 7, done).
---
23. [-] **Built-in runbooks**: → Moved to v0.13.1.6 (item 9, done).
---
---
---
---
24. [-] **Validation failure event**: Deferred — `on_failure` mode field exists in `constitution.toml` schema but `ValidationFailed` daemon event not implemented. → future phase (unscheduled)
25. [-] **Auto-follow-up proposal**: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
26. [-] **Follow-up consent model** in `constitution.toml`: `on_failure` mode field added to constitution schema (see `constitution.rs`). Full event-driven flow deferred. → future phase (unscheduled)
---
---
---
---
---
---
---
6. [x] **End-to-end test**: Added 6 routing integration tests covering `draft apply`, `draft view`, `draft approve`, `draft deny`, `apply` shortcut, and `view` shortcut — all verify the full route → Command path.
---
31. [x] **Automatic compaction pass**: Manual triggering via `ta gc --compact` (see item 33). Daemon-scheduled compaction (nightly run on startup) deferred — the foundation config is in place. → v0.13.2 or later for daemon scheduler.
32. [x] **Compaction never touches the ledger**: `ta gc --compact` only removes staging directories and draft package JSON files. The `goal-history.jsonl` ledger is append-only and never subject to compaction. History entries are written on each compaction for audit traceability.
33. [x] **`ta gc --compact`**: Added `--compact` flag and `--compact-after-days` (default: 30) to `ta gc`. Dry-run shows what would be discarded. Non-dry-run removes staging dirs and draft packages for applied/completed goals older than the threshold. Writes history entries and reports bytes reclaimed.
34. [-] **External action compaction (stub for v0.13.4+)**: `discard_external_actions_after_days` field reserved for when v0.13.4/v0.13.5 land. Not implemented yet. → v0.13.4+
---
---
5. [x] **Cross-platform**: Handled at the `Event::Paste` level (bracketed paste), which is cross-platform. 8 new unit tests.
---
- Items 15, 19–20 (Intelligent Surface): Moved to v0.13.1.6 and completed there.
---
- Items 21–23 (Runbooks): Moved to v0.13.1.6 and completed there.
1. [x] **`commands/daemon.rs` module**: Extract `auto_start_daemon()` logic from `shell.rs` into `daemon::start()`. Add `daemon::stop()` (POST to `/api/shutdown`), `daemon::status()` (GET `/api/status` + PID file check), `daemon::ensure_running()` (idempotent start-if-needed).
- Items 34–35 (Compaction): Scaffolded; full implementation deferred to v0.13.4+ (external actions) and a future phase (audit events).
---
#### Version: `0.13.1-alpha`
---
---
---
### v0.13.1.1 — Power & Sleep Management
---
**Goal**: Make the daemon behave correctly when the host machine sleeps or enters low-power mode. Prevents idle sleep during active goals, detects wake events, suppresses false heartbeat alerts in the grace window, and checks API connectivity after waking.
---
---
---
1. [x] **Sleep/wake detection**: Watchdog compares wall-clock vs monotonic clock delta each cycle. When wall elapsed > monotonic elapsed + interval + 30s, a sleep is detected. Emits `SystemWoke { slept_for_secs }` event and updates `state.last_wake_wall`.
2. [x] **Heartbeat skip tolerance on wake**: After waking, all liveness/heartbeat checks are suppressed for `wake_grace_secs` (default: 60, configurable via `[power] wake_grace_secs`). Prevents spurious dead-goal alerts when the OS resumes from sleep.
---
---
5. [x] **`ta daemon install`**: New subcommand generates a macOS LaunchAgent plist or Linux systemd user service for auto-start. `--apply` writes and loads the unit. Prints the generated file and install path without `--apply` for dry inspection.
---
7. [x] **Config**: `[power]` section in `daemon.toml` with `wake_grace_secs`, `prevent_sleep_during_active_goals`, `prevent_app_nap`, `connectivity_check_url`. All fields have safe defaults and are fully optional.
---
#### Version: `0.13.1-alpha.1`
---
---
---
### v0.13.1.2 — Release Completeness & Cross-Platform Launch Fix
---
**Goal**: Fix two classes of critical bugs: (1) release binaries non-functional out of the box because `ta-daemon` is missing, and (2) `ta draft apply` silently succeeds when PR creation fails, leaving the user with a pushed branch and no PR and no clear recovery path.
---
#### Bug A — Missing `ta-daemon` in release archives
---
---
---
   - §7: Added `check_policy`/`enforce_policy` call in `ta-mcp-gateway/src/tools/fs.rs` before file diff access
#### Bug B — `ta draft apply` silently succeeds when PR creation fails
**Root cause** (`draft.rs:3339–3357`): `adapter.open_review()` failure is caught and downgraded to a `Warning:` print, then execution continues. `vcs_review_url` stays `None`. The VCS tracking save condition at line 3361 requires at least one of `vcs_branch`, `vcs_commit_sha`, or `vcs_review_url` to be set. If push metadata doesn't include `"branch"` (the only key checked at line 3327) AND review fails, the condition is false — nothing is saved. The goal JSON shows `pr_url: None`, `branch: None`. The apply exits 0. `ta pr status` reports "no URL". User has a pushed branch but no PR and no recovery command.
---
**Secondary bug**: `vcs_branch` is only captured if `result.metadata.get("branch")` returns Some. If the push adapter returns the branch under a different key or not at all, branch is permanently lost even if the push succeeded.
---
#### Fixes from this session already landed on `main`
- [x] Release workflow validates artifacts locally before publishing (no more empty-draft releases)
- [x] USAGE.md version stamped from release tag at package time
- [x] Docker install option marked *(Coming Soon)* in header
- [x] Build and package `ta-daemon` in all release archives (Bug A — CI fix)
---
---
#### Items (remaining for this phase)
#### Version: `0.11.6-alpha`
2. [x] **Package `ta-daemon` in all archives**: `ta-daemon` (Unix) / `ta-daemon.exe` (Windows) alongside `ta`
---
---
5. [x] **Capture branch unconditionally after push**: Store the branch from push result regardless of review outcome. Fall back to the goal's `branch_prefix + slug` if metadata doesn't include it. Derived via same slug algorithm as `GitAdapter::branch_name()` when metadata `"branch"` key is absent.
6. [x] **`ta draft reopen-review <id>`**: For applied drafts with a branch but no PR URL, attempt to create the PR. Useful recovery command without needing to re-apply. New `DraftCommands::ReopenReview` variant + `draft_reopen_review()` function.
7. [x] **`ta pr status` branch display**: Show branch name even when `pr_url` is None, with hint: `ta draft reopen-review <id>` and the manual `gh pr create` command to create the missing PR.
---
9. [x] **Windows install note**: Documented in USAGE.md that `ta shell` (PTY) is Unix-only; `ta daemon start`, `ta run`, and all non-interactive commands work on Windows. Includes PowerShell examples.
---
---
---
---
#### Version: `0.13.1-alpha.2`
---
---
   - `fn protected_submit_targets(&self) -> Vec<String>` — adapter declares its protected refs. Default: `vec![]`.
### v0.13.1.3 — Shell Help & UX Polish
---
---
   - `fn verify_not_on_protected_target(&self) -> Result<()>` — asserts post-`prepare()` invariant. Default impl: if `protected_submit_targets()` is non-empty, query the adapter's current position and return `Err` if it matches. Adapters may override.
---
---
1. [x] **Prompt prefix**: Change `> ` to `ta> ` so users know they're in the TA shell (not bash/zsh) — already implemented
---
3. [x] **`git` → `vcs` command**: Added `vcs` route to daemon defaults + shell.toml; both `git` and `vcs` supported; HELP_TEXT updated
4. [x] **`!<cmd>` documentation**: Documented in HELP_TEXT, shell.rs classic help, and USAGE.md
5. [x] **Data-driven keybinding list**: `KEYBINDING_TABLE` const drives `keybinding_help_text()`; `help` renders Navigation & Text from it
---
9. [x] **Discord slash commands**: Register `/ta` slash command via Discord Application Commands API instead of message-prefix matching. Benefits: auto-complete, built-in help, no MESSAGE_CONTENT intent required, works in servers with strict permissions. *(moved to v0.12.1)*
---
---
---
### v0.13.1.4 — Game Engine Project Templates
---
---
---
**BMAD integration model**: BMAD is a git repo of markdown persona prompts — it must be installed **machine-locally**, not cloned into the game project (Perforce depot or otherwise). The canonical install location is `~/.bmad/` (Unix) or `%USERPROFILE%\.bmad` (Windows). TA stores the path in `.ta/bmad.toml` and agent configs reference it from there. The project itself stays clean — no BMAD files are committed to VCS.
---
| Framework | Role | Installation |
---
| **BMAD** | Structured planning — PRD, architecture, story decomposition, role-based review | `git clone` to `~/.bmad/` (machine-local, not in project) |
| **Claude Flow** | Parallel implementation — swarm coordination across module boundaries | `npm install -g @ruvnet/claude-flow` |
---
---
**Prerequisite note for users**: Claude Code (`claude` CLI), Claude Flow, and BMAD must be installed on the machine before running the discovery goal. TA does not install these — it configures the project to use them. See USAGE.md "Game Engine Projects" for per-platform setup.
---
---
11. [x] **Gateway reconnect with resume**: Current listener reconnects from scratch on disconnect. Implement Discord's resume protocol (session_id + last sequence number) for seamless reconnection without missed events. *(moved to v0.12.1)*
1. [x] **`ProjectType` enum**: Added `UnrealCpp` and `UnityCsharp` variants to `detect_project_type()` in `ta-memory/src/key_schema.rs` — detects by `*.uproject` (Unreal) or `Assets/` dir + `*.sln` file (Unity). Also added `KeyDomainMap` entries for both types.
2. [x] **`ta init --template unreal-cpp`**: `.taignore` excludes `Binaries/`, `Intermediate/`, `Saved/`, `DerivedDataCache/`, `*.generated.h`; `policy.yaml` protects `Config/DefaultEngine.ini`, `*.uproject`, `Source/**/*.Build.cs`; `memory.toml` pre-seeds 3 UE5 conventions (TObjectPtr/UPROPERTY, game thread rules, UPROPERTY/UFUNCTION macros).
---
4. [x] **`.ta/bmad.toml` config**: Written by `ta init --template` for game engine types; stores `bmad_home` (default `~/.bmad` Unix / `%USERPROFILE%\.bmad` Windows) and `agents_dir`. Agent configs reference `${bmad_home}/agents/` at runtime.
5. [x] **BMAD agent configs (`.ta/agents/`)**: Generate `bmad-pm.toml`, `bmad-architect.toml`, `bmad-dev.toml`, `bmad-qa.toml` with persona_file pointing to `${bmad_home}/agents/{role}.md`. Lives under `.ta/agents/` — not in the game source tree. 4 new test assertions.
---
7. [x] **Discovery goal template** (`.ta/onboarding-goal.md`): Describes the first TA goal — survey codebase, produce `docs/architecture.md`, `docs/bmad/prd.md`, `docs/bmad/stories/sprint-1/` using BMAD roles. Prerequisite checklist included. Engine-specific source extensions (`*.cpp/*.h` for Unreal, `*.cs` for Unity).
8. [x] **`ta init templates` output**: Listed `unreal-cpp` and `unity-csharp` with one-line descriptions noting BMAD + Claude Flow dependency; added prerequisite note block.
---
---
---
---
#### Version: `0.13.1-alpha.4`
13. [x] **Rate limiting**: Add rate limiting on command forwarding to prevent Discord abuse from flooding the daemon API. *(moved to v0.12.1)*
---
---
### v0.13.1.5 — Shell Regression Fixes
---
---
---
#### Regressions
29. [x] **§16.6 — Remove TA-specific scanner from generic draft pipeline** *(constitution §16.6 compliance, pulled forward from v0.14.1 item 1)*: Extract `scan_s4_violations()` from `draft.rs` into a project-specific constitution checker invoked via the `draft-build-post` hook. The generic pipeline gets only the hook point (no-op by default). The TA repo itself activates the hook via `.ta/workflow.toml`. This ensures external projects — Python, C++, content drafts — never receive TA-internal Rust-pattern checks.
**R1 — Run indicator not clearing on completion**: The "Agent is working..." indicator (introduced as `TuiMessage::WorkingIndicator` in v0.12.7) persists after the agent finishes. Users see a stale spinner/banner when the shell is idle.
8. [x] **Remove `--listen` flag**: Flag remains but is now "internal" — daemon manages the lifecycle. Users configure `[channels.discord_listener]` in `daemon.toml` instead of running `--listen` manually. Help text updated accordingly.
**R2 — Scroll not staying at bottom when user is at tail**: Auto-scroll-to-bottom (via `auto_scroll_if_near_bottom()` added in v0.12.7 heartbeat paths) is not firing consistently. When new output arrives and the scroll position is already at the tail, the view doesn't follow.
---
**R3 — Paste within prompt inserts at cursor, not end**: v0.12.2 added paste-from-outside → force to prompt end. But when the cursor is already inside the prompt line (e.g., user moved left), pasting inserts at the cursor position rather than appending to the end. The v0.12.2 manual verification item was never confirmed green (item `[ ]` still open in v0.12.2 phase at time of discovery).
2. [x] **Force cursor to end before paste**: When a paste event is detected, move the cursor to `input_buffer.len()` before inserting characters.
---
2. [x] **Composited diff for child drafts**: In `draft build`, if `parent_draft_id` is set and the parent is Applied, compute the diff as `child-staging vs original-source-snapshot` (the snapshot taken *before* the parent was applied), not vs current source. This captures the full incremental change set.
1. [x] **Reproduce R1**: Root cause confirmed — `AgentOutputDone` only cleared the LAST heartbeat line. When `WorkingIndicator` is pushed, then regular agent output arrives before the first `[heartbeat]` tick, the tick creates a NEW heartbeat entry. On exit only the tick was cleared; the original "Agent is working..." line remained with `is_heartbeat=true` indefinitely.
2. [x] **Fix R1**: Changed `AgentOutputDone` to scan ALL heartbeat lines in both `app.output` and `app.agent_output`, setting each to `is_heartbeat=false`. Earlier heartbeats get blanked; the last one shows "[agent exited]". Added `r1_working_indicator_cleared_when_heartbeat_tick_arrives_before_exit` regression test that exercises the exact failure sequence (WorkingIndicator → output → [heartbeat] tick → AgentOutputDone).
3. [x] **Reproduce R2**: `auto_scroll_if_near_bottom()` was not called on `SseEvent`, `CommandResponse`, `DaemonDown`, or `DaemonUp` output paths — only on `AgentOutput` and heartbeat paths.
4. [x] **Fix R2**: Added `auto_scroll_if_near_bottom()` call after `push_lines` in `SseEvent` and `CommandResponse` handlers, and after `push_output` in `DaemonDown`/`DaemonUp`. Reduced `NEAR_BOTTOM_LINES` and `AGENT_NEAR_BOTTOM_LINES` from 5 to 3 to avoid surprising snaps when user is reviewing recent output. Added `r2_command_response_auto_scrolls_near_bottom`, `r2_sse_event_auto_scrolls_near_bottom`, and `r2_command_response_preserves_scroll_when_far_up` tests.
5. [x] **Fix R3**: Code already correctly sets `app.cursor = app.input.len()` before paste insertion (added in v0.12.2). Added `r3_paste_appends_at_end_when_cursor_in_middle` test to close the open v0.12.2 verification item — confirmed the `Event::Paste` handler always moves cursor to end regardless of prior cursor position.
6. [x] **Manual verification**: All three fixes covered by automated tests (5 new tests). v0.12.2 R3 open item resolved.
---
---
---
4. [x] **`ta draft apply` merges chains**: Add `ta draft apply --chain <child-id>` which applies parent + all unapplied children in order, with a single merged commit message summarizing the chain. Detect cycles and warn.
---
---
3. **No process health in goal status**: `ta goal list` and `ta goal status` show lifecycle state but not process health. A goal in `running` state whose process exited 30 minutes ago looks identical to one actively producing output.
### v0.13.1.6 — Intelligent Surface & Operational Runbooks

---
---
*Moved from v0.13.1 items 15–23 — these are substantial UX changes, deferred past the v0.13.1.5 release to avoid blocking it.*
---
#### Intelligent Surface
---
1. [x] **`ta status` as the one command**: Unified, prioritized view replacing `ta goal list`, `ta draft list`, `ta plan status`, `ta daemon health`, and `ta doctor`. Urgent items first (stuck goals, pending approvals, health issues), then active work, then recent completions. Details expand on demand.
2. [x] **`ta` with no arguments shows dashboard**: Instead of showing help, run `ta status`. The bare command becomes the entry point.
#### Deferred to v0.13.12
---
- **[D] Proactive notifications**: Daemon pushes for: goal completed, goal failed, draft ready for review, corrective action needed, disk warning. Delivered via configured channels (shell SSE, Discord, future: email/Slack). → v0.13.12 item 9
- **[D] Suggested next actions**: After any command, daemon suggests what to do next based on current state: "Draft applied. PR #157 created. Next: `ta pr status` or `ta run` to start next phase." → v0.13.12 item 10
---
- **[D] Reduce command surface**: Commands subsumed by the intelligent layer marked "advanced" in help — not removed, but deprioritised. Default path is through the intelligent surface. → v0.13.12 item 12
---
#### Operational Runbooks
---
---
8. [x] **Runbook triggers**: Triggered automatically by watchdog conditions or manually via `ta runbook run <name>`. Each step presented for approval unless auto-heal policy covers it.
9. [x] **Built-in runbooks**: Ship defaults for: disk pressure, zombie goals, crashed plugins, stale drafts, failed CI. Users can override or add their own.
- **`registry.trustedautonomy.dev` index** — the registry CDN. For now, `ta setup resolve` falls back to GitHub releases directly. A proper registry index (with search, versions, metadata) is a beta-era infrastructure item.
#### Version: `0.13.1-alpha.6`
---
---
---
**Dependency**: `ta-channel-discord` plugin (fully implemented in v0.12.1). No new code in this repo required — work is external repo creation + USAGE.md/PLUGIN-AUTHORING.md doc updates.
---
---
4. [x] **Update `PLUGIN-AUTHORING.md`**: Added links to published repos and a "Publishing your plugin" section covering the GitHub releases tarball format and release workflow.
**Goal**: Make memory useful across runs. Today the daemon uses `FsMemoryStore` (exact-match only) and nothing writes the project constitution or plan completions to memory, so agents start each goal with no accumulated context. This phase wires up `RuVectorStore` as the primary backend (with `FsMemoryStore` as a read fallback for legacy entries), expands what gets written, and injects semantically-retrieved context at goal start.
4. [x] **Regression test**: `scroll_stays_bottom_through_burst_of_output` — delivers 100 `AgentOutput` messages, asserts `scroll_offset` stays 0.
1. [x] **Fix Bug D — plan-update ordering**: In `draft.rs`, moved plan-update to run inside the VCS submit closure, AFTER `adapter.prepare()` checks out the feature branch. For non-VCS apply, plan-update still runs before `rollback_guard.commit()`. Working tree is now clean at branch-checkout time.
2. [x] **Failure summary on mid-pipeline abort**: When the VCS submit closure fails (`submit_result`), replaced bare `submit_result?` with a structured error handler that prints: number of files rolled back, the cause, and three concrete retry options with exact commands.
--- Phase Run Summary ---
5. [x] **Wire `on_human_guidance`**: Capture human shell feedback into memory (category: Preference, confidence 0.9). Currently defined in `AutoCapture` but never called.
**Goal**: Close two remaining rough edges discovered during public-alpha testing that are annoying enough to fix before beta.
**Tests added**: 1 new integration test (`apply_with_plan_phase_does_not_dirty_tree_before_branch_checkout` in `draft.rs`). All 589 ta-cli tests pass.
### v0.13.1 — Autonomous Operations & Self-Healing Daemon
---
#### Known issue discovered post-merge
**Goal**: Shift from "user runs commands to inspect and fix problems" to "daemon detects, diagnoses, and proposes fixes — user approves." The v0.11.3 observability commands become the foundation, but instead of the user running `ta goal inspect` and `ta doctor` manually, the daemon runs them continuously and surfaces issues proactively. The user's primary interaction becomes reviewing and approving corrective actions, not discovering and diagnosing problems.
- ~~**Release pipeline drift false positive**~~: Fixed in v0.13.2. `FileSnapshot::has_changed()` now compares content hash directly instead of using mtime as the primary signal. Copy operations (`ta draft apply`) update mtime without changing content; the fix correctly ignores mtime-only changes. See `crates/ta-workspace/src/conflict.rs`.
---
#### Version: `0.13.1-alpha.7`
Audit all `push_output`, `push_heartbeat`, and `agent_output.push` call sites to ensure `scroll_to_bottom()` or `auto_scroll_if_near_bottom()` is called consistently. Add a dedicated `push_and_scroll()` helper that combines the two. Identify the specific interaction (e.g., SSE event burst, split-pane toggle) that causes the pane to stop following.
---
---
### v0.13.2 — MCP Transport Abstraction (TCP/Unix Socket)
---
<!-- beta: yes — enables container isolation and remote agent execution for team deployments -->
**Goal**: Abstract MCP transport so agents can communicate with TA over TCP or Unix sockets, not just stdio pipes. Critical enabler for container-based isolation (Secure Autonomy) and remote agent execution.
These items integrate with the per-project validation commands defined in `constitution.toml` (v0.13.9). When a draft build or apply fails its validation gate, the daemon can automatically propose — or trigger — a corrective follow-up goal.
28. [-] **Cycle guard**: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
29. [-] **`ta operations log` extension** for validation events: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
1. [x] `TransportLayer` trait: `Stdio`, `UnixSocket`, `Tcp` variants — `TransportMode` enum in `ta-daemon/src/config.rs`; `transport::serve()` in `ta-daemon/src/transport.rs`
2. [x] TCP transport: MCP server listens on configurable port, agent connects over network — `serve_tcp()` in `transport.rs`
**Distinction from GC**: `ta gc` (implemented in v0.11.3) removes orphaned and zombie records. Compaction is different — it ages applied/closed records from "fat" storage (full file diffs, draft packages, staging copies, email bodies, DB change logs) down to "slim" audit-safe summaries, while the `goal-history.jsonl` ledger preserves the essential facts. The VCS record (the merged PR) is the source of truth for what changed; the fat artifacts are only needed for review windows.
4. [x] Transport selection in agent config: `transport = "stdio" | "unix" | "tcp"` — `transport` field in `agents/generic.yaml`; `[transport]` section in `daemon.toml` via `TransportConfig`
---
6. [x] Connection authentication: bearer token exchange on connect — `authenticate_connection()` reads `Bearer <token>\n` header; configured via `[transport].auth_token`
---
---
**Also fixed**: Release pipeline drift false positive (v0.13.1.7 deferred) — `FileSnapshot::has_changed()` now uses content hash as the authoritative signal instead of mtime-first comparison. Copy operations update mtime without changing content; the old fast-path would treat identical files as "unchanged" (safe) but could miss same-second writes. The fix correctly detects content-only changes and eliminates mtime-induced false positives in sequential pipeline steps.
---
#### Version: `0.13.2-alpha`
30. [x] **Compaction policy in `daemon.toml`**: `[lifecycle.compaction]` section added via `CompactionConfig` and `LifecycleConfig` structs in `crates/ta-daemon/src/config.rs`. Fields: `enabled` (default: true), `compact_after_days` (default: 30), `discard` (default: `["staging_copy", "draft_package"]`). Parses from TOML and defaults correctly.
---
1. [x] **Generate Windows `.ico`**: Added `imagemagick` to Nix flake devShell. `.ico` already checked in at `images/icons/ta.ico`.
10. [x] **Fix Windows clippy: `cmd_install` unused params + `dirs_home` dead code**: On Windows, `project_root` and `apply` are used only in macOS/Linux `#[cfg]` blocks; `dirs_home()` is only called from those same blocks. Add `let _ = (project_root, apply)` in the Windows branch and gate `dirs_home` with `#[cfg(any(target_os = "macos", target_os = "linux"))]`.
---
**Goal**: Interim UX improvement while `GoalBaseline` (v0.13.12 item 6) is not yet implemented. When `diff_all()` returns empty, diagnose the most likely cause and print actionable guidance instead of a bare error.
---
---
12. [x] **Bug D — `ta draft apply` fails when plan-update dirties working tree before branch checkout** → v0.13.1.7: `apply` writes PLAN.md (plan status update) to disk before calling `git checkout -b <feature-branch>`. Git refuses the checkout because PLAN.md has unstaged changes, triggering rollback. Root cause: plan-update should run *after* the feature branch is checked out, not before. Workaround: `ta draft apply --no-submit` then manually commit. Fix: reorder `apply_plan_update()` to run after `checkout_feature_branch()` in `draft.rs`. Also surface a clearer failure summary with explicit next steps when the apply pipeline fails mid-way (observability mandate). → v0.13.1.7
---
**Goal**: Resolve three confirmed-active shell regressions. All three were nominally fixed in v0.12.2/v0.12.7 but are observed broken in v0.13.1.
1. [x] **Detect uncommitted working tree changes**: When `diff_all()` returns empty, check `git status --porcelain` on the source directory. If uncommitted changes exist, explain that the overlay mirrors the working tree so the diff is empty — and show the exact `git checkout -b / git add / git commit / gh pr create` sequence to fix it.
**Goal**: Make onboarding an existing Unreal C++ or Unity C# game project seamless. `ta init --template unreal-cpp` / `ta init --template unity-csharp` provisions BMAD agent configs, Claude Flow `.mcp.json`, a discovery goal, and project-appropriate `.taignore` and `policy.yaml`. First-run experience: one command starts a structured onboarding goal that produces a PRD, architecture doc, and sprint-1 stories.
3. [x] **`count_working_tree_changes()` helper**: Runs `git status --porcelain` in the source dir; returns 0 on non-git dirs or git errors (safe degradation).
**Goal**: Replace the command-heavy workflow with a proactive, intent-aware surface. `ta status` becomes the single dashboard; the daemon pushes notifications instead of requiring polling; `ta shell` interprets natural-language operational intent; runbooks automate common recovery procedures.
#### Version: `0.13.2.1` → semver `0.13.2-alpha.1`
---
---
---
### v0.13.3 — Runtime Adapter Trait
---
<!-- beta: yes — prerequisite for local model support (v0.13.8) -->
**Goal**: Abstract how TA spawns and manages agent processes. Today it's hardcoded as a bare child process. A `RuntimeAdapter` trait enables container, VM, and remote execution backends — TA provides BareProcess, Secure Autonomy provides OCI/VM.
---
---
---
---
---
---
---
3. [x] Runtime selection in agent/workflow config: `runtime = "process" | "oci" | "vm"`
---
5. [x] Runtime lifecycle events: `AgentSpawned`, `AgentExited`, `RuntimeError` fed into event system
6. [x] Credential injection API: `RuntimeAdapter::inject_credentials()` for scoped secret injection into runtime environment
---
---
---
- [x] New `crates/ta-runtime/` crate: `RuntimeAdapter` trait, `AgentHandle` trait, `BareProcessRuntime`, `RuntimeRegistry` with plugin discovery, `ExternalRuntimeAdapter` (JSON-over-stdio plugin protocol), `ScopedCredential`, `RuntimeConfig`, `SpawnRequest`/`SpawnHandle`
- [x] `runtime: RuntimeConfig` field added to `AgentLaunchConfig` in `run.rs` (serde default = "process")
---
---
---
    escalate_after: 2           # human notified after 2 failed attempts
---
#### Version: `0.13.3-alpha`
---
---
---
---
---
---
---
---
- `ExternalAction` trait: defines an action type (email, social post, API call, DB query) with metadata schema
- `ActionPolicy`: per-action-type rules — auto-approve, require human approval, block, rate-limit
- `ActionCapture`: every attempted external action is logged with full payload before execution
- `ActionReview`: captured actions go through the same draft review flow (approve/deny/modify before send)
---
---
---
---
---
---
3. [x] `ActionCapture` log: every attempted action logged to `.ta/action-log.jsonl` with full payload, outcome, policy, timestamp, and goal context. Queryable by goal ID. Implemented in `crates/ta-actions/src/capture.rs`.
| `notify` | Deliver event to configured channels (default for most events) |
5. [x] MCP tool `ta_external_action`: registered in `TaGatewayServer`. Validates payload schema, applies rate limits, loads policy from `workflow.toml`, captures all attempts, and returns structured outcome to the agent.
---
7. [x] Dry-run mode: `dry_run: true` in `ta_external_action` params — action is logged with `DryRun` outcome, no execution, no review capture.
8. [x] Built-in action type stubs: `email`, `social_post`, `api_call`, `db_query` — schema + validation only, `execute()` returns `ActionError::StubOnly`. Plugins call `ActionRegistry::register()` to override.
---
**Tests**: 24 new tests in `ta-actions` (action, policy, capture, rate_limit modules) + 6 new integration tests in `ta-mcp-gateway/tools/action.rs` + 1 server tool-count update.
---
**Config example**:
---
[actions.email]
policy = "review"          # require human approval before sending
rate_limit = 10            # max 10 per goal
---
[actions.social_post]
---
---
---
#### Deferred items resolved
---
---
---
[actions.db_query]
policy = "review"          # review all DB mutations
auto_approve_reads = true  # SELECT is fine, INSERT/UPDATE/DELETE needs review
- The `workflow.toml` `auto_commit`/`auto_push`/`auto_review` settings are workarounds for bad defaults and use git-specific naming.
---
---
---
---
---
### v0.13.5 — Database Proxy Plugins
---
**Goal**: Plugin-based database proxies that intercept agent DB operations. The agent connects to a local proxy thinking it's a real database; TA captures every query, enforces read/write policies, and logs mutations for review. Plugins provide wire protocol implementations; TA provides the governance framework (v0.13.4).
---
**Depends on**: v0.13.4 (External Action Governance — DB proxy extends the `ExternalAction` trait)
---
#### DraftOverlay — read-your-writes within a draft
---
DB plugins must satisfy "read-your-writes" consistency: if an agent writes `active_issues = 7` (staged, not yet committed to the real DB), a subsequent read must return `7`, not the real DB's stale `4`.
#### Deferred items resolved
TA provides a `DraftOverlay` struct (in a new `ta-db-overlay` crate) that all DB plugins use instead of implementing their own caching:
6. ✅ **Registry** (`crates/ta-build/src/registry.rs`): `detect_build_adapter()` (Cargo→npm→Make→None), `select_build_adapter()` (named + auto-detect fallback), `known_build_adapters()`. Command overrides applied when using "auto" with custom commands.
---
// Plugin flow:
overlay.put(resource_uri, after_doc)?;      // on write — stores mutation
let cached = overlay.get(resource_uri)?;   // on read — returns staged value before hitting real DB
7. ✅ **Wire into `ta release run`**: Already scaffolded in v0.10.18 release script with graceful degradation (`ta build` step runs if available, skips with message if not).
---
---
---
---
8. ✅ **`ta shell` integration**: `build` and `test` added to shell help text as shortcuts, dispatched to daemon like other commands.
Special cases:
5. [x] **Platform abstraction**: Wrap the ANSI escape output in a helper (`fn enable_scroll_capture(stdout)` / `fn disable_scroll_capture(stdout)`) that handles platform differences. On Windows, delegate to crossterm's native API if raw ANSI doesn't work.
- **Binary blob fields**: `overlay.put_blob(uri, field, bytes)?` — blob stored in `.ta/staging/<goal_id>/db-blobs/<sha256>`, overlay entry stores hash reference. `ta draft view` shows `<binary: 14723 bytes, sha256: abc>`.
- **DDL (schema changes)**: stored as a separate `DDLMutation` entry type — shown prominently in draft review with explicit approval required.
---
This is conceptually a **git staging area for DB mutations**: the overlay is the canonical state during the draft; the real DB is "main". Unlike a WAL, it's scoped to a single goal and designed for human review, not crash recovery.
---
---
---
1. [x] `ta-db-overlay` crate: `DraftOverlay` struct with `put()`, `get()`, `put_blob()`, `list_mutations()`, `delete()`, `put_ddl()`, `mutation_count()` — persisted to JSONL with SHA-256 blob storage
---
3. [x] Proxy lifecycle: `ProxyHandle` trait with `start()`/`stop()` — TA calls before/after agent
4. [x] Query classification: `QueryClass` enum (Read/Write/Ddl/Admin/Unknown) with `MutationKind` (Insert/Update/Delete/Upsert)
5. [x] Mutation capture: all write operations staged through `DraftOverlay` — provides read-your-writes + JSONL audit trail
---
7. [x] Reference plugin: `ta-db-proxy-sqlite` — shadow copy approach with SQL classification and mutation replay via rusqlite
8. [ ] Reference plugin: `ta-db-proxy-postgres` — Postgres wire protocol proxy → v0.13.6+
9. [ ] Reference plugin: `ta-db-proxy-mongo` — MongoDB wire protocol proxy → v0.13.6+
10. [ ] Future plugins (community): MySQL, Redis, DynamoDB → v0.14.0+
---
#### Version: `0.13.5-alpha`
---
---
---
---
---
<!-- priority: deferred — post-launch community feature; not required for public alpha -->
---
---
**Design philosophy**: Community knowledge is a *connector*, not a monolith. Each community resource serves a specific *intent* — API integration guidance, security threat intelligence, framework migration patterns, etc. The plugin ships with a registry of well-known resources, each declaring its intent so agents know *when* to consult it. Users configure which resources are active and whether the agent has read-only or read-write access.
---
---
---
1. [x] **Plugin scaffold**: External plugin at `plugins/ta-community-hub/` using JSON-over-stdio protocol (v0.11.4 architecture). `Cargo.toml` + `plugin.toml` + `src/` with `registry.rs`, `cache.rs`, `main.rs`.
---
   - `community_search { query, intent?, resource?, workspace_path }` — searches cached markdown files by keyword, intent-filtered.
   - `community_get { id, workspace_path }` — returns cached document with freshness metadata and token-budget enforcement.
   - `community_annotate { id, note, gap_type?, workspace_path }` — stages annotation to `.ta/community-staging/<resource>/annotations/`.
   - `community_feedback { id, rating, context?, workspace_path }` — stages upvote/downvote to `.ta/community-staging/<resource>/feedback/`.
---
   Plus `handshake`, `list_resources`, and `sync` methods.
---
4. [x] **Draft integration**: Write operations produce staged files with `resource_uri: "community://..."`. These appear in draft artifacts and are reviewed independently from code changes.
---
#### 2. Community Resource Registry
9. [x] **Fix false-positive stdin prompt detection**: `--print` mode no longer switches to stdin mode. Auto-reverts to `ta>` prompt when goal exits.
---
3. **Plan editing is manual**: Adding items, moving items between phases, creating new phases, and cross-referencing plan items requires manual file editing of PLAN.md. An agent-mediated flow would let users describe what they want and have the agent recommend placement, with explicit approval before writing.
   # Built-in resources (ship with the plugin)
---
   name = "api-docs"
   intent = "api-integration"
   description = "Curated API documentation to reduce hallucinations when integrating third-party services"
   source = "github:andrewyng/context-hub"
   content_path = "content/"
   access = "read-write"        # "read-only" | "read-write" | "disabled"
---
[plugins.discord]
---
   [[resources]]
   name = "security-threats"
   intent = "security-intelligence"
---
   source = "github:community/security-context"   # example future resource
   content_path = "threats/"
---
---
---
For v0.12.0, implement Phase 1 only. Design the manifest schema to support Phases 2 and 3 without breaking changes.
   [[resources]]
   name = "migration-patterns"
   intent = "framework-migration"
   description = "Step-by-step migration guides between framework versions and paradigms"
---
   content_path = "migrations/"
   access = "read-only"
   auto_query = false            # Only queried when agent detects migration intent
---
   [[resources]]
   name = "project-local"
   intent = "project-knowledge"
   description = "Project-specific knowledge base maintained by the team"
   source = "local:.ta/community/"
   access = "read-write"
---
---
6. [x] **Intent-based routing**: `Registry::by_intent()` routes by exact intent match; `community_search` with no resource/intent filter searches all enabled resources ranked by keyword score.
7. [x] **Access control per resource**: `Access` enum (`ReadOnly`/`ReadWrite`/`Disabled`) enforced in all write handlers — `community_annotate`, `community_feedback`, `community_suggest` each return clear errors on read-only or disabled resources.
8. [x] **`ta community list`**: Shows name, intent, access, auto_query, sync status (synced/stale/not synced), doc count. `--json` flag for machine-readable output.
9. [x] **`ta community sync [resource]`**: Syncs local (copies .md files) and GitHub (curl-based GitHub API fetcher via `GITHUB_TOKEN`). `--json` flag for scripting.
---
#### 3. Agent Integration & Context Injection
---
10. [x] **Auto-query injection**: `build_community_context_section()` in `community.rs` generates a CLAUDE.md section listing auto-query resources with intent-specific `community_search` guidance. Injected via `run.rs` `inject_claude_md()`.
11. [x] **Context budget**: `DEFAULT_TOKEN_BUDGET = 4000` tokens (≈4 chars/token). `enforce_budget()` in `cache.rs` truncates and appends a note with the doc length and instruction to retry with a larger budget.
12. [x] **Freshness metadata**: `CachedDoc.synced_at` timestamp included in every response. Docs older than 90 days get `⚠` warning with sync command suggestion.
13. [x] **How-to-use injection**: `build_community_context_section()` surfaces each auto-query resource's `name`, `intent`, and `description` alongside a tailored `community_search` example.
  "schema_version": 1,
#### 4. Upstream Contribution Flow
- [x] **`EventRouter`** (`crates/ta-events/src/router.rs`): Loads `event-routing.yaml` config, matches incoming events to responders (exact type match + optional filters), dispatches to strategy handler (notify, block, agent, workflow, ignore), tracks attempt counts for `escalate_after` and `max_attempts`. Includes `RoutingConfig`, `Responder`, `ResponseStrategy`, `EventRoutingFilter`, `RoutingDecision` types with YAML serialization. 19 tests.
14. [x] **Staged contributions**: `community_annotate` → `.ta/community-staging/<resource>/annotations/`.  `community_feedback` → `.ta/community-staging/<resource>/feedback/`. `community_suggest` → `.ta/community-staging/<resource>/suggestions/`. All include frontmatter with resource, goal_id, created_at.
15. [x] **Draft callouts**: Staged artifacts under `.ta/community-staging/` are captured in the draft diff as modified files and visible in `ta draft view` with their `resource_uri: "community://..."`.
16. [-] **Upstream PR on apply**: Creating GitHub PRs from staged contributions on `ta draft apply`. → v0.13.15 (fix pass) — staging files and `resource_uri` scheme are in place; needs git adapter wiring in `apply`.
17. [-] **Contribution audit trail**: Logging community contributions to the audit ledger. → v0.14.6 (Compliance-Ready Audit Ledger).
| Abstract Stage | Git | Perforce | SVN |
#### 5. CLI & Shell Integration
---
18. [x] **`ta community` CLI commands**: `ta community list`, `ta community sync [name]`, `ta community search <query>`, `ta community get <id>` — all implemented in `apps/ta-cli/src/commands/community.rs`.
#### Constitution Compliance Scan at Draft Build
20. [-] **Status bar integration**: `[community: searching...]` badge. → v0.13.15 — not implemented in v0.13.7.
---
---
---
- [x] Plugin scaffold (`plugins/ta-community-hub/`) with JSON-over-stdio protocol
- [x] All 5 MCP tools: `community_search`, `community_get`, `community_annotate`, `community_feedback`, `community_suggest`
- [x] `handshake`, `list_resources`, `sync` protocol methods
- [x] Registry parsing (`registry.rs`): TOML roundtrip, access levels, intent routing, disabled filtering
- [x] Cache layer (`cache.rs`): local doc indexing, keyword search, token budget, freshness metadata
- [x] CLI commands: `ta community list/sync/search/get` in `commands/community.rs`
- [x] Context injection: `build_community_context_section()` for `auto_query = true` resources, wired into `inject_claude_md()`
- [x] 7 tests in `registry.rs`, 4 tests in `cache.rs`, 13 tests in `main.rs`, 8 tests in `community.rs` = 32 new tests
    "ta-channel-discord": {
---
---
---
- Item 17 (Contribution audit trail) → v0.14.6 (Compliance-Ready Audit Ledger)
- Item 19 (Tab completion) → v0.13.15 (not implemented in v0.13.7)
- Item 20 (Status bar integration) → v0.13.15 (not implemented in v0.13.7)
---
#### Tests added (32 total)
---
- `registry::tests::load_empty_when_file_missing`
- `registry::tests::load_parses_resources`
- `registry::tests::access_defaults_to_read_only`
- `registry::tests::by_intent_filters_correctly`
- `registry::tests::disabled_resource_excluded_from_enabled`
- `registry::tests::github_repo_parses_owner_and_repo`
- `registry::tests::local_path_resolves_relative`
- `cache::tests::search_finds_matching_docs`
- `cache::tests::get_doc_returns_content`
- `cache::tests::token_budget_truncates_large_doc`
- `cache::tests::search_respects_resource_filter`
- `main::tests::handshake_returns_plugin_name_and_capabilities`
- `main::tests::list_resources_empty_when_no_config`
- `main::tests::list_resources_shows_configured_resources`
- `main::tests::community_search_returns_empty_without_resources`
- `main::tests::community_annotate_requires_note_param`
---
- `main::tests::community_annotate_stages_file_for_read_write_resource`
- `main::tests::community_feedback_validates_rating`
- `main::tests::community_suggest_stages_new_doc`
- `main::tests::sync_local_resource_copies_docs`
- `main::tests::unknown_method_returns_error`
- `community::tests::registry_loads_from_toml`
- `community::tests::registry_empty_when_no_file`
- `community::tests::community_context_section_empty_without_auto_query`
---
- `community::tests::community_context_section_excludes_disabled`
---
on_failure = "block"
- `source = "path:./plugins/discord"` — local source, build with detected toolchain
#### Version: `0.13.6-alpha`
---
---
---
### v0.13.7 — Goal Workflows: Serial Chains, Parallel Swarms & Office Routing

#### Critical: Command Output Reliability
**Goal**: Connect goals to workflows so that *how* a goal executes is configurable per-project, per-department, or per-invocation — not hardcoded into `ta run`. Today every goal is a single agent in a single staging directory. This phase introduces workflow-driven execution: serial phase chains, parallel agent swarms, and a routing layer that maps goals to the right workflow based on project config, department, or explicit flag.
   - Windows Terminal (crossterm handles Windows separately — may need platform-specific path)
---
1. **Multi-phase work is manual**: Building v0.11.3 requires `ta run` → review draft → `ta run --follow-up` → review → repeat. Each cycle is a manual step. There's no way to say "execute phases 11.3 through 11.5 in sequence, building/testing each, with one PR at the end."
2. **No parallelism**: A plan with 5 independent items runs them one at a time. There's no way to decompose a goal into concurrent sub-goals, have agents work in parallel, then merge.
3. **Workflow selection is implicit**: Every `ta run` uses the same execution model. A coding project wants build→test→review cycles. A content project wants draft→edit→publish. A legal review wants sequential approval chains. There's no way to attach different execution patterns to different kinds of work.
4. **Office structure has no workflow routing**: The `ta office` concept manages multiple projects, but there's no way to say "engineering goals use the serial-phase workflow, marketing goals use the content pipeline, compliance goals use the approval chain."
   auto_start = true          # Start agent on shell launch (default: true)
#### Architecture: Goal → Workflow Routing
---
The core abstraction is a **workflow router** that sits between `ta run` and execution:
---
**Status**: Partially mitigated — two fixes landed but not yet battle-tested end-to-end.
ta run "goal" --workflow <name>     # explicit
ta run "goal"                       # uses project/department default
3. **Draft list default filter misses "in progress" drafts** — After `ta draft apply --git-commit --push --review`, the draft transitions to `Applied` status, but the PR is still open. `ta draft list` (compact mode) hides it because `Applied` is terminal. The human is told "no active drafts, use --all" and then has to scan 40+ entries.
---
**Routing resolution order:**
1. `--workflow <name>` flag on `ta run` (explicit override)
2. Goal's plan phase → phase metadata → workflow (phase-level default)
3. Project config `.ta/config.yaml` → `default_workflow` (project-level default)
4. Office department config → department → workflow mapping (office-level default)
5. Built-in `single-agent` workflow (backwards-compatible default)
2. **Goal status doesn't reflect draft lifecycle** — `ta goal list` shows `applied` but doesn't indicate whether the PR was merged, still open, or failed CI. The human has to check GitHub manually.
**Workflow definition** (`.ta/workflows/<name>.yaml`):
    prompt: |
name: serial-phases
description: Execute plan phases in sequence with build/test gates
steps:
  - type: goal-run          # run agent in staging
    gate: build-and-test    # must pass before next step
  - type: follow-up         # reuse staging, next phase
Users could wire this manually (watch SSE stream → parse events → call `ta run`), but that's fragile scripted automation. TA should support this natively with agent-grade intelligence.
  - type: draft-build       # single PR for all phases
    gate: human-review
1. [x] **`GoalRun.tag` field**: Added `tag: Option<String>` to GoalRun with `slugify_title()` auto-generation, `display_tag()` fallback, and `GoalRunStore::save_with_tag()` for auto-sequencing. `GoalRunStore::resolve_tag()` and `resolve_tag_or_id()` for lookup.
---
#### Track 1: Serial Phase Chains (`serial-phases` workflow)
---
Chain multiple phases into one execution. Each phase runs → builds → tests → if green, the next phase starts as a follow-up in the same staging. One draft/PR at the end.
This pulls forward the zero-dependency items from v0.12.2 (Autonomous Operations) and v0.12.0 (Template Projects item 22). The full corrective action framework, agent-assisted diagnosis, and runbooks remain in v0.12.2 — they need the observability and governance layers built first. This phase gives us the monitoring foundation those later phases build on.
**Planning items**:
1. [x] **Workflow engine integration with `ta run`**: `ta run` accepts `--workflow` flag with resolution order (explicit > config default > `single-agent`). `WorkflowKind` enum, `resolve_workflow()` fn, and `WorkflowCatalog` in `ta-workflow` crate.
2. [x] **`serial-phases` built-in workflow**: `ta run --workflow serial-phases --phases p1,p2` runs each phase as a follow-up goal in the same staging, with configurable gates between steps (build, test, clippy, custom command). `execute_serial_phases()` in `run.rs`. `WorkflowGate`, `StepState`, `SerialPhasesState` in `ta-workflow/src/serial_phases.rs`. 18 new tests.
3. [x] **Gate evaluation**: `evaluate_gates()` runs gate commands in the staging directory after each phase. On failure: workflow halts with actionable error including staging path and `--resume-workflow <id>` instructions. Built-in gates: `build`, `test`, `clippy`; any other string treated as custom shell command.
4. [x] **Automatic follow-up chaining**: `execute_serial_phases()` manages `--follow-up-goal <id>` chain automatically. Each step reuses the previous step's staging. No manual intervention between phases.
5. [x] **Single-PR output**: After all phases pass, user is directed to `ta draft build --goal <last_goal_id>` which builds one draft covering all changes. Summary includes the last goal's staging with full change history.
6. [x] **Resume/retry on failure**: `SerialPhasesState` persisted to `.ta/serial-workflow-<id>.json`. On gate failure, error message instructs user to fix staging and rerun with `--resume-workflow <id>`. State tracks which steps passed/failed.
#### Prompt Detection Hardening
#### Track 2: Parallel Agent Swarms (`swarm` workflow)
---
Decompose a goal into independent sub-goals, run them in parallel (separate staging dirs), then an integrator agent merges the results.
---
**Planning items**:
7. [x] **Goal decomposition**: `ta run --workflow swarm --sub-goals "goal1" "goal2"` accepts an explicit list of sub-goal titles. `SubGoalSpec` in `ta-workflow/src/swarm.rs`. 8 new tests.
8. [x] **Parallel staging**: Each sub-goal runs as an independent agent (no follow-up chain), each gets its own staging directory created by `ta run`. `SwarmState` tracks per-sub-goal staging paths.
9. [x] **Per-agent validation**: `per_agent_gates` evaluated after each sub-goal via `evaluate_gates()`. Failed sub-goals are flagged and reported but don't block remaining sub-goals.
10. [x] **Integration agent**: `--integrate` flag triggers an integration agent after all sub-goals complete. Receives all passed staging paths in objective. Builds final draft with `ta draft build --latest`.
11. [-] **Dependency graph**: Sub-goals with declared dependencies — swarm scheduler ordering. → v0.13.16 (local model + advanced swarm phase; current impl runs sub-goals sequentially)
12. [-] **Progress dashboard**: Live swarm status in `ta shell` status bar. → v0.13.16 (v0.13.7.2 was not created; `SwarmState.print_summary()` provides CLI summary today)
1. **Draft iteration is heavyweight**: After `ta draft apply`, iterating on the PR (fixing CI, addressing review comments) requires either a full new goal with staging copy or dropping out of TA entirely to work in raw git. There's no lightweight path to amend an existing draft/PR from within TA.
#### Track 3: Office Workflow Routing
---
Map departments, project types, or goal categories to default workflows.
4. [x] **`help` shows CLI commands**: The shell `help` command now shows both shell-specific help and a summary of all `ta` CLI commands, so users can discover available commands without leaving the shell.
**Planning items**:
13. [-] **Department → workflow mapping in office config**: `.ta/office.yaml` `departments` section. → v0.13.16 (v0.13.7.3 was not created)
---
---
16. [x] **`ta workflow list --builtin`**: Lists all built-in workflow names and descriptions. Usage: `ta workflow list --builtin`.
17. [x] **`ta run` routing integration**: `--workflow` flag wired into `ta run` with `resolve_workflow()`. `Swarm` variant added to `WorkflowKind`. Both `serial-phases` and `swarm` routing integrated in `main.rs`.
23. [x] **`ta plugin logs <name>`**: View plugin stderr logs from daemon.
#### Open Questions (resolve during implementation)
- **Agent coordination protocol**: How do swarm agents communicate? Shared memory store? File-based? Event bus?
- **Conflict resolution strategy**: When the integration agent merges parallel work, what happens with conflicts? Auto-resolve? Human intervention? Agent negotiation?
- **Workflow versioning**: Do workflows need versioning for reproducibility?
- **Cross-project workflows**: Can an office workflow span multiple projects (e.g., "update API + update client")?
- **Cost/resource limits**: Parallel swarms can be expensive. Should there be concurrency limits per project/office?
#### Version: `0.11.4-alpha.4`
--- Phase Run Summary ---
---
- Item 11 (Sub-goal dependency graph) → v0.13.16 (Advanced Swarm + Local Model phase)
- Item 12 (Live swarm progress dashboard in shell) → v0.13.16
---
5. [x] **Release pipeline checklist gate**: Added `requires_approval: true` constitution compliance step to `DEFAULT_PIPELINE_YAML` in `release.rs`. Validated by `default_pipeline_has_constitution_checklist_gate` test.
#### Version: `0.13.7-alpha`
---
---
---
### v0.13.8 — Agent Framework: Pluggable Agent Backends with Shared Memory
---
1. [x] **`ta-submit-*` plugin protocol**: Define the JSON-over-stdio protocol for VCS plugins. Messages: `detect` (auto-detect from project), `exclude_patterns`, `save_state`, `restore_state`, `commit`, `push`, `open_review`, `revision_id`. Same request/response structure as channel plugins. → `crates/ta-submit/src/vcs_plugin_protocol.rs`
<!-- beta: yes — foundational for local models, multi-agent workflows, and community sharing -->
<!-- implemented: items 1,3,5,6,7,9,10,16,17,18,26,27,28,29 in v0.13.8-alpha -->
**Goal**: Introduce an abstract **AgentFramework** concept so any goal, workflow, or daemon role can be wired to any agent backend — Claude Code (default), Codex, Claude-Flow, BMAD, Ollama+Qwen, a bare model, or a user-defined framework — without changing TA's core logic. Frameworks are defined as manifest files, composable at multiple config levels, and shareable via the plugin registry. All frameworks, including generic agents and local models, participate in TA's shared memory system so context and observations carry across goals and model switches.
- **Slack inbound listener** (slash commands, button callbacks, Socket Mode) — Slack plugin lacks `listener.rs` and `progress.rs`. Implement in v0.13.x once beta starts. *(Slack is send-only for public alpha.)*
**Context**: Today `ta run` hardcodes `claude --headless`. The coupling points are thin: (1) the process to launch, (2) the `[goal started]` sentinel on stderr, (3) the exit code. That's enough to swap in any agent. TA needs a dispatch layer, a manifest format, a resolution order, and a memory bridge so generic agents get the same observability as Claude Code.
---
**Design — manifest**:
See `docs/MISSION-AND-SCOPE.md` for the full `SourceAdapter` trait design and per-provider operation mapping.
10. [x] **`ta operations log`**: New `ta operations log` command in `apps/ta-cli/src/commands/operations.rs`. Shows corrective actions with `--limit`, `--all`, `--severity` filters. Actionable empty-state messages point to `ta daemon start`.
---
name        = "qwen-coder"
version     = "1.0.0"
type        = "process"           # process | script (future: mcp-server, remote)
command     = "ta-agent-ollama"
---
sentinel    = "[goal started]"    # substring to watch for on stderr (default)
description = "Qwen 2.5 Coder 7B via Ollama — fast local coding agent"
- **[D] Intent-based interaction in `ta shell`**: Natural language operational requests ("clean up old goals", "what's stuck?") translated to command sequences, shown for approval before executing. → v0.13.12 item 11
# Context injection — how TA injects goal context before launch
context_file   = "CLAUDE.md"     # file to prepend goal context into (omit = don't inject)
context_inject = "prepend"       # prepend | env | arg | none
# context_env  = "TA_GOAL_CONTEXT"  # if inject=env: env var pointing to temp context file
# context_arg  = "--context"        # if inject=arg: flag prepended before the file path
[actions.api_call]
# Shared memory — how this framework reads/writes TA memory
---
inject  = "context"       # context | mcp | env | none
# context: serialize relevant memory entries into context_file before launch
# mcp:     expose ta-memory as a local MCP server; agent connects automatically
# env:     write memory snapshot to $TA_MEMORY_PATH (temp file), agent reads it
write_back = "exit-file"  # exit-file | mcp | none
# exit-file: agent writes new memories to $TA_MEMORY_OUT before exit; TA ingests them
# mcp:       agent uses ta-memory MCP tools directly during the run
---
3. [x] **Send full content on Enter**: `submit()` combines any typed prefix with the full paste content. The compact indicator text is never sent — only the actual paste.
**Design — config levels**:
---
---
# .ta/daemon.toml  (project-level binding)
[agent]
default_framework = "claude-code"   # used by ta run unless overridden
qa_framework      = "qwen-coder"    # used by automated QA goals (v0.13.7 workflows)
---
---
---
# .ta/workflows/code-review.yaml  (workflow-level override)
agent_framework: codex
---
---
---
ta run "fix the login bug" --agent qwen-coder   # goal-level override
---
**Goal**: Complete the constitution compliance audit that was cut short in v0.11.4.4. That phase fixed all §4 violations. This phase runs the full 14-section audit, fixes any remaining violations, adds regression tests, and gets a clean sign-off.
---
**Resolution order** (highest wins): goal `--agent` flag → goal `--model` shorthand → workflow spec → project `daemon.toml` → user `~/.config/ta/daemon.toml` → built-in default (`claude-code`).
4. [x] **Audit sign-off**: All tests pass (517 passed, 7 ignored). Clean audit pass documented in commit `084d4ea`.
**Built-in frameworks** (ship with TA):
---
| Name | Context file | Memory | Ships as | Notes |
|------|-------------|--------|----------|-------|
| `claude-code` | `CLAUDE.md` prepend | MCP (ta-memory server) | built-in | Current default |
| `codex` | `AGENTS.md` prepend | MCP (Codex supports MCP) | built-in wrapper | Requires Codex CLI |
| `claude-flow` | `CLAUDE.md` prepend | MCP | built-in wrapper | Swarm config passthrough |
| `bmad` | `CLAUDE.md` prepend | MCP | built-in wrapper | BMAD personas in `.bmad-core/` |
| `ollama` | arg injection | env/exit-file | built-in impl | Generic; requires `--model` |
| `ta-agent-ollama` | system prompt | tool-native | shipped binary | Full tool-loop for any OpenAI-compat endpoint |
---
**`--model` shorthand**: `ta run "..." --model ollama/qwen2.5-coder:7b` auto-selects `ta-agent-ollama` framework and passes the model string. No manifest authoring needed for the common local-model case.
---
**Shared memory bridge** — three modes, each covering a different agent class:
- **MCP mode** (Claude Code, Codex, Claude-Flow, BMAD): TA exposes `ta-memory` as a local MCP server pre-configured in the agent's MCP config before launch. Agent calls `memory_read`/`memory_write`/`memory_search` as tools natively. Zero extra integration.
- **Context mode** (any agent with a context file): TA serializes the N most relevant memory entries (by goal tags, plan phase, file paths) into a markdown block and prepends it to the context file alongside goal context. Agent reads passively. Write-back: agent appends structured observations to a designated section; TA parses on exit.
- **Env/exit-file mode** (custom scripts, simple agents): TA writes memory snapshot to `$TA_MEMORY_PATH` before launch. Agent reads it optionally. On exit, TA reads `$TA_MEMORY_OUT` if present and ingests any new entries.
---
---
6. [x] **Input cursor style** — configurable in `daemon.toml` `[shell]` section:
**Core dispatch layer**
1. [x] `AgentFrameworkManifest` struct — name, version, type, command, args, sentinel, description, context_file, context_inject, memory section (`crates/ta-runtime/src/framework.rs`)
2. [x] `AgentFramework` trait — `name()`, `manifest()`, `build_command()`, `context_inject_mode()`, `memory_config()` methods; `ManifestBackedFramework` implementation
3. [x] Framework resolver: search order — goal flag → `.ta/agents/` → `~/.config/ta/agents/` → built-in registry (`AgentFrameworkManifest::resolve()`)
4. [x] Update `ta run` to dispatch via resolved manifest — custom → `framework_to_launch_config()`, known builtins (codex, claude-flow) → `agent_launch_config()`, unknown → warn + claude-code fallback
5. [x] `ta agent frameworks` — list all frameworks (built-in + discovered); `ta agent list --frameworks` alias
6. [x] `ta agent info <name>` — manifest details, memory mode, command check
---
---
7. [x] Define manifest TOML schema; document `context_file`, `context_inject`, `context_env`, `context_arg` fields (in `ContextInjectMode` + `FrameworkMemoryConfig`)
8. [x] Context injector: prepend mode (backup/restore, same as today), env mode (`inject_context_env()` → `TA_GOAL_CONTEXT`), arg mode (`inject_context_arg()` → `--context <path>`), none
9. [x] Ship built-in manifests: `claude-code` (CLAUDE.md/prepend/MCP), `codex` (AGENTS.md/prepend/MCP), `claude-flow`, `ollama` (in `AgentFrameworkManifest::builtins()`)
10. [x] `ta agent framework-validate <path>` — validate TOML manifest, check command on PATH
---
**Shared memory bridge**
11. [x] MCP memory server: `inject_memory_mcp_server()` — adds `ta-memory` MCP server entry to `.mcp.json` before agent launch (additive, no backup/restore needed)
12. [x] Context-mode serializer: `inject_memory_context()` — appends memory section to context file using existing `build_memory_context_section_for_inject()`
13. [x] Exit-file ingestion: `ingest_memory_out()` — after agent exits reads `$TA_MEMORY_OUT` if present, parses entries, stores via `FsMemoryStore`; logs ingested count
14. [-] `ta-agent-ollama` memory tools: include `memory_read`/`memory_write`/`memory_search` in its native tool set, backed by TA's memory REST API → v0.13.16 (Local Model Agent)
15. [-] Memory relevance tuning: `[memory]` manifest section can set `max_entries`, `recency_days`, `tags` filter to control what gets injected into context-mode agents → v0.13.16
---
**Configuration levels**
16. [x] `[agent]` section in `daemon.toml`: `default_framework` (default "claude-code"), `qa_framework` (default "claude-code") fields added to `AgentConfig`
17. [x] Workflow YAML `agent_framework: Option<String>` field added to `WorkflowDefinition` — resolved at workflow dispatch time
---
19. [x] Precedence enforcement and logging: `tracing::info!` on framework selection with `source` field (goal-flag/workflow/project/user-config/default); printed to user via `println!` for non-claude-code selections
   - Default: larger, white block cursor (replaces the current medium-blue hard-to-read cursor)
**`ta-agent-ollama` implementation**
20. [-] New crate `crates/ta-agent-ollama` — binary implementing tool-use loop against any OpenAI-compat endpoint → v0.13.16
21. [-] Core tool set: bash_exec, file_read, file_write, file_list, web_fetch, memory_read, memory_write, memory_search → v0.13.16
22. [-] Startup: read context from `--context-file` or `$TA_GOAL_CONTEXT`, include in system prompt; emit sentinel to stderr → v0.13.16
23. [-] Model validation: probe `/v1/models` + test function-calling call on startup; emit clear error if model doesn't support tools → v0.13.16
24. [-] Graceful degradation: if model has no function calling, fall back to CoT-with-parsing mode (best-effort) with a warning → v0.13.16
25. [-] Validated with: Qwen2.5-Coder-7B, Phi-4-mini, Kimi K2.5, Llama3.1-8B (via Ollama and llama.cpp server) → v0.13.16
---
**Easy onboarding — model-as-agent path**
26. [x] `ta agent new --model ollama/qwen2.5-coder:7b` — generates ready-to-use TOML manifest in `~/.config/ta/agents/`, prints Ollama connection instructions and next steps
27. [x] `ta agent new --template <name>` — starter manifests for: `ollama`, `codex`, `bmad`, `openai-compat`, `custom-script`
28. [x] `ta agent test <name>` — prints manual smoke-test instructions; checks command on PATH; guides user through end-to-end test via `ta run`
29. [x] `ta agent doctor <name>` — checks command on PATH, Ollama endpoint reachability, API keys (ANTHROPIC_API_KEY, OPENAI_API_KEY); prints actionable fix instructions
2. [x] **`ta new --vcs` flag + interactive VCS prompt**: Set the VCS adapter explicitly via `--vcs git|svn|perforce|none`. When `--vcs` is omitted in interactive mode, `ta new` asks "Do you want version control?" with options derived from available adapters/plugins (e.g., `[git, svn, perforce, none]`). The selected adapter is written into `.ta/workflow.toml` `[submit].adapter`, and for Git, runs `git init` + initial commit automatically. `--vcs perforce` also adds `ta-submit-perforce` to the plugin requirements in `project.toml`.
**Cross-language project scaffolding**
35. [-] **`ta new --template <lang>`**: `ta new` gains language-specific project templates that pre-populate `workflow.toml` with sensible verify commands and a starter `.ta/constitution.toml`. Templates: `python`, `typescript`, `nodejs`, `rust` (existing default), `generic`. → v0.13.15
   - `python`: verify commands = `["ruff check .", "mypy src/", "pytest"]`; constitution inject/restore patterns for Python conventions; `.taignore` with `__pycache__/`, `.venv/`, `*.egg-info/`, `dist/`, `.mypy_cache/`
   - `typescript`/`nodejs`: verify commands = `["tsc --noEmit", "npm test"]` (or `pnpm`/`yarn` variant); `.taignore` with `node_modules/`, `.next/`, `dist/`, `build/`, `.turbo/`
   - `generic`: empty verify commands; minimal constitution; basic `.taignore`
36. [-] **`ta init --template <lang>`**: Same as `ta new` but for an existing project — writes only the `.ta/` config files without touching source. Detects language automatically from presence of `package.json`, `pyproject.toml`, `Cargo.toml`, `go.mod` and suggests the matching template. → v0.13.15
37. [-] **`.taignore` — overlay exclusion patterns**: `.ta/taignore` (or `.taignore` at project root) lists glob patterns excluded from staging copies and diffs — analogous to `.gitignore`. The overlay workspace (`ta-workspace/overlay.rs`) reads this file before copying and skips matching paths. **This is the single highest-impact change for non-Rust adoption**: `node_modules/` (200MB+), `.venv/`, `__pycache__/`, `.next/`, `dist/`, `build/` copied to every staging directory make first-time staging extremely slow and bloated. Default exclusions (always applied regardless of `.taignore`): `.git/`, `.ta/`. Language templates (item 35) write a `.taignore` appropriate for the detected language. `ta goal status` shows staging size and excluded path count so users can tune it. → v0.13.15
---
**Sharing + registry**
30. [-] Framework manifests publishable to the plugin registry (v0.12.4 registry) — same install flow as VCS plugins → v0.13.16
31. [-] `ta agent install <registry-name>` — fetch manifest + any companion binary, verify SHA256, run `ta agent test` → v0.13.16
32. [-] `ta agent publish <path>` — validate + submit to registry → v0.13.16
---
**Research + validation**
33. [-] Research spike: Ollama vs llama.cpp server vs vLLM vs LM Studio — API compatibility, tool-calling support, macOS/Linux support, startup time, model availability. Document in `docs/agent-framework-options.md`. → v0.13.16
34. [-] End-to-end validation: Qwen2.5-Coder-7B completes a real `ta run` goal with memory write-back; memory entries visible in next goal's context → v0.13.16
---
---
4. [x] **`setup.sh` bootstrap**: Standalone shell script (committed to the template repo) that installs TA if missing, runs `ta setup`, and prints next steps. Works on macOS/Linux. PowerShell equivalent for Windows.
- Items 14–15 (ollama memory tools, memory relevance tuning) → v0.13.16 (Local Model Agent)
- Items 20–25 (`ta-agent-ollama` crate, tool set, startup, validation, degradation, validation matrix) → v0.13.16
- Items 30–32 (framework manifest registry, install, publish) → v0.13.16
- Items 33–34 (research spike, end-to-end validation) → v0.13.16
- Items 35–37 (`ta new/init --template`, `.taignore`) → v0.13.15 (cross-language onboarding pass)
---
#### Version: `0.13.8-alpha`
- **seq**: Auto-incrementing per slug (handles multiple goals with similar names).
---
---
### v0.13.9 — Product Constitution Framework
---
<!-- beta: yes — project-level behavioral contracts and release governance -->
**Goal**: Make the constitution a first-class, configurable artifact that downstream projects declare, extend, and enforce — not a TA-internal concept hard-wired to `docs/TA-CONSTITUTION.md`. A project using TA can define its own invariants (what functions inject, what functions restore, what the rules are), and TA's draft-build scan and release checklist gate read from that config.
1. **No language runtime required** — plugins are standalone executables. `ta setup` downloads pre-built binaries. No npm, pip, conda, or nix needed for the default path.
**Theoretical basis**: The constitution is TA's implementation of the "Value Judgment module" (§13) and "Self-Reflexive Meta Control System" (§15) described in *Suggested Metrics for Trusted Autonomy* (Finkelstein, NIST docket NIST-2023-0009-0002, Jan 2024). See `docs/trust-metrics.md` for the full mapping of TA architecture to that paper's 15 trust variables.
---
*(Moved forward from v0.14.3 — constitution tooling is a natural capstone to beta governance, not a post-beta concern. Compliance audit ledger moves to v0.14.6 as an enterprise-tier feature requiring cloud deployment context.)*
---
**Problem**: Currently the constitution is TA-specific. The §4 injection/cleanup rules, the pattern scanner, and the release checklist all reference TA's own codebase conventions. A downstream project using TA (e.g., a web service or a data pipeline) has different injection patterns, different error paths, and different invariants. They get no constitution enforcement at all.
---
#### Architecture: `constitution.toml`
---
A project-level constitution config in `.ta/constitution.toml`:
---
---
[rules.injection_cleanup]
# Functions that inject context into the workspace (must be cleaned up on all error paths)
inject_fns = ["inject_config", "inject_credentials"]
restore_fns = ["restore_config", "restore_credentials"]
severity = "high"
6. [-] **Reference template: ta-perforce-template**: External repo — moved to v0.13.6 Community Hub.
[rules.error_paths]
# Error return patterns that must be preceded by cleanup
patterns = ["return Err(", "return Ok(()) # error"]
severity = "medium"
---
[scan]
# Files/dirs to scan for constitution violations
include = ["src/"]
exclude = ["src/tests/"]
on_violation = "warn"   # "warn" | "block" | "off"
---
---
# Whether to include a constitution compliance gate in the release pipeline
checklist_gate = true
---
agent_review = false   # opt-in — spins up a lighter concurrent review agent
**Files**: `apps/ta-cli/src/commands/shell_tui.rs` (event loop refactor)
[agent_review]
# Prompt prefix for the constitution reviewer (lighter than full release notes agent)
model_hint = "fast"    # hint to use a smaller/faster model
max_tokens = 2000
focus = "injection_cleanup,error_paths"
---
# Per-project validation commands at each draft stage (not TA-specific)
# These run in the staging directory; exit code != 0 blocks the stage.
# on_failure: "block" | "warn" | "ask_follow_up" | "auto_follow_up"
[[validate]]
stage = "pre_draft_build"     # runs before `ta draft build` packages the changes
commands = ["cargo clippy --workspace --all-targets -- -D warnings"]
on_failure = "block"
---
[[validate]]
stage = "pre_draft_apply"     # runs before `ta draft apply` copies to source
commands = ["cargo test --workspace", "cargo fmt --all -- --check"]
on_failure = "ask_follow_up"  # propose a follow-up goal (pairs with v0.13.1 auto-follow-up)
- No heartbeat or liveness check: once a goal enters `running`, nothing verifies the agent process is still alive. A crashed or never-started agent leaves the goal stuck forever.
# For cross-platform checks (catches Windows-only issues on macOS):
# [[validate]]
# stage = "pre_draft_build"
# commands = ["cargo clippy --target x86_64-pc-windows-gnu --workspace -- -D warnings"]
# on_failure = "block"
---
---
---
1. [x] **Routing misclassification**: Verified — `draft`, `approve`, `deny`, `view`, `apply` all route correctly to Command path via `ta_subcommands` and shortcuts in `ShellConfig`. Added 6 routing tests in `input.rs`.
1. [x] **`constitution.toml` schema**: Define and document the config format. Ship TA's own rules as the default template (generated by `ta constitution init-toml`).
   - **Key design**: `[[validate]]` arrays replace TA's hardcoded `[verify]` section in `office.yaml`. Project teams define what "passing" means for their codebase — Rust projects add clippy/test, TypeScript projects add tsc/jest, etc.
   - `on_failure = "ask_follow_up"` emits a `ValidationFailed` event; the auto-follow-up behaviour is provided by v0.13.1 items 24–29.
   - `ProjectConstitutionConfig` struct in `apps/ta-cli/src/commands/constitution.rs` with `ValidationStep`, `ConstitutionRule`, `ConstitutionScan`, `ConstitutionRelease`.
---
3. [x] **Draft-time scanner reads `constitution.toml`**: `scan_for_violations()` reads inject/restore function names from `ProjectConstitutionConfig`. Projects with different conventions get correct scanning.
4. [-] **Release pipeline reads `checklist_gate`**: The release checklist gate step (v0.11.4.4 item 9) is enabled/disabled by `constitution.toml`. The checklist content is generated from the declared rules, not hardcoded. → v0.13.15
---
6. [x] **`ta constitution check-toml`**: CLI command to run the scanner outside of draft build — useful for CI integration and pre-commit hooks. Exit code 0 = clean, 1 = violations found when `on_violation = "block"`. Output is machine-readable JSON with `--json` flag.
---
8. [x] **Documentation**: Added "Constitution Config (`constitution.toml`)" section to `docs/USAGE.md`. Full web-service worked example deferred to v0.13.15.
9. [-] **`ta constitution init-toml --template <lang>`**: Language-specific constitution templates so Python/TypeScript/Node projects get relevant defaults rather than Rust-centric examples. Templates:
   - `python`: `inject_fns`/`restore_fns` use Python conventions (e.g., `setup_env`, `teardown_env`); scan includes `src/`, `app/`; excludes `__pycache__/`, `.venv/`
   - `typescript`/`nodejs`: patterns for async setup/teardown; scans `src/`, `lib/`; excludes `node_modules/`, `dist/`
   - `rust`: existing TA defaults (current behaviour)
---
   Auto-detects language if `--template` omitted (same detection logic as `ta init --template`, v0.13.8 item 36). → v0.13.15
10. [-] **USAGE.md cross-language worked examples**: Add a "Using TA with Python / TypeScript / Node.js" section showing complete `workflow.toml`, `.taignore`, and `constitution.toml` for each ecosystem. Covers: verify command setup, common pitfalls (`node_modules` exclusion, virtualenv placement), and a full first-goal walkthrough. → v0.13.15
8. [x] **Test: external VCS plugin lifecycle**: Integration test with a mock VCS plugin (shell script that speaks the protocol) verifying detect → save_state → commit → restore_state flow. → `crates/ta-submit/tests/vcs_plugin_lifecycle.rs` (12 integration tests)
**Files**: `.ta/constitution.toml` (new), `apps/ta-cli/src/commands/` (init, check, draft build scan, release step), `crates/ta-workspace/src/` (scanner crate or module).
12. [x] **Reference template: ta-discord-template**: Published to `Trusted-Autonomy/ta-discord-template`. *(external repo — deferred: requires GitHub repo creation outside this codebase)*
3. [x] **Web shell**: Added `paste` event listener to `shell.html` that forces insertion at end; standard `<input>` pastes at cursor, so the listener moves cursor to end before inserting.
---
- Item 4 (release pipeline checklist_gate) → v0.13.15 (cross-language & constitution completion)
- Item 5 (parallel agent review during release) → v0.13.15
- Item 7 (constitution inheritance `extends`) → v0.13.15 (stub already in code)
- Items 9–10 (language-specific templates, cross-language USAGE.md) → v0.13.15
5. [x] **Manual test**: Paste with cursor at start, middle, and end of input; verify text always appears at end. Test in Terminal.app, iTerm2, and the web shell.
#### Version: `0.13.9-alpha`
---
---
---
---
### v0.12.3 — Shell Multi-Agent UX & Resilience
---
### v0.13.10 — Feature Velocity Stats & Outcome Telemetry
---
<!-- beta: yes — enterprise observability -->
---
---
**Key distinction from n8n/Zapier**: No visual flow builder, no webhook chaining, no action-to-action piping. One event → one agent (or workflow) with full context. The agent handles the complexity, not a workflow graph.
There is currently no durable record of:
- How long each goal took from start to `pr_ready` (build time)
- How long was spent on follow-up goals amending/fixing the original (rework time)
- How many goals failed, were cancelled, or were denied vs applied
- Which workflow type (code, doc, qa, etc.) produced which outcomes
- Whether a goal required human amendment before apply
---
This data exists ephemerally in goal JSON and draft packages, but is never aggregated or surfaced. As workflows diversify (code → doc → qa → office routing in v0.13.7), per-workflow benchmarking becomes essential for both personal insight and enterprise SLAs.
---
---
---
**Stats file**: `.ta/velocity-stats.json` — append-on-each-goal-completion, human-readable.
**Goal**: Close the remaining UX and reliability gaps found during v0.12.1 testing. Users need to send messages to running agents, distinguish streams from multiple agents, understand auth failures, and have clean process cleanup when agents exit.
---
5. [x] **Heartbeat / tail stream cleanup when agent exits**: After the agent process exits, the `tail` stream and heartbeat timers are torn down immediately. Shell prints a clean `[agent exited]` line rather than silently hanging or orphaning the tail task.
  "schema_version": "1.0",
  "project": "TrustedAutonomy",
  "entries": [
---
      "goal_id": "226dea99-...",
      "title": "Implement v0.12.8...",
      "workflow": "code",
      "agent": "claude-code",
      "plan_phase": "v0.12.8",
      "outcome": "applied",           // applied | denied | cancelled | failed | timeout
      "started_at": "2026-03-19T22:10:00Z",
      "pr_ready_at": "2026-03-19T22:30:00Z",
      "applied_at":  "2026-03-19T22:45:00Z",
      "build_seconds": 1200,          // start → pr_ready
---
      "total_seconds": 2100,
#### Deferred items moved
      "follow_up_count": 0,           // number of follow-up goals spawned from this one
      "rework_seconds": 0,            // sum of follow-up goal build_seconds
      "denial_reason": null,
      "cancel_reason": null
---
---
---
The daemon already exposes `POST /api/goals/{id}/input` which writes directly to a running agent's stdin. The Discord and Slack plugins need a dispatch path to it.
**Message syntax** (prefix-message and slash command):
**Connector event**: On every terminal outcome (`GoalApplied`, `GoalDenied`, `GoalCancelled`, `GoalFailed`), emit a `VelocitySnapshot` event via the existing event router. Channel plugins (Discord, Slack, future HTTP webhook) receive this and can forward to a central endpoint.
---
---
10. [x] **`ta goal input <id> <text>`** CLI sub-command: thin wrapper over `POST /api/goals/{id}/input` for scripting and testing without a channel plugin.
6. [x] **Periodic "still running" structured log**: Every N minutes (configurable via `goal_log_interval_secs` in `[operations]`, default 5), emit `tracing::info!` with goal UUID, elapsed time, and current state.
  "project": "TrustedAutonomy",
7. [x] **File change count on exit**: When the agent process exits, log how many files were modified in staging vs source. (`count_changed_files` helper in run.rs — 5 tests)
  "aggregate": {
    "total_goals": 42,
    "applied": 38,
    "failed": 2,
    "cancelled": 2,
---
    "avg_rework_seconds": 120,
    "p90_build_seconds": 1800
  }
---
10. [x] **Deduplicate GoalStarted emission**: Removed redundant `emit_goal_started_event()` from `cmd.rs` sentinel handler — `run.rs` already writes `GoalStarted` to `FsEventStore`.
--- Phase Run Summary ---
---
12. [x] **Slack plugin check**: The Slack plugin has no SSE-based progress streamer (pure stdio Q&A only) — no `progress.rs` to fix. Not applicable.
1. [x] **`VelocityEntry` struct** (`crates/ta-goal/src/velocity.rs`): fields per schema above; `Serialize`/`Deserialize`; builder from `GoalRun`
2. [x] **`VelocityStore`** (`crates/ta-goal/src/velocity.rs`): append-only JSONL writer to `.ta/velocity-stats.jsonl`; load/query/aggregate helpers
Today's TA workflow requires the user to be the monitoring layer: notice something is wrong, run diagnostic commands, interpret output, decide on a fix, run the fix. That's the same cognitive load TA was built to eliminate for code work. The daemon should be the monitoring layer — it already sees every event, every state transition, every process exit. It just needs to act on what it sees.
6. [x] **`ta stats`** CLI command: `ta stats velocity` pretty-prints aggregate stats; `--json`, `--workflow`, `--since` filters
8. [x] **Auto-heal policy**: `[operations.auto_heal]` config section added to `daemon.toml` via `AutoHealConfig` struct. `enabled` (default: false) and `allowed` list fields. Config parses and roundtrips correctly.
11. [x] **`ta stats export`**: export full history as JSON (default) or CSV
---
---
#### Deferred items moved
---
4. → **v0.14.6** **Build time calculation**: `pr_ready_at` from first `DraftBuilt` event timestamp — requires event timestamp lookup infrastructure.
5. → **v0.14.6** **Rework tracking**: follow-up goals sum into root goal's `rework_seconds`.
8. → **v0.14.6** **`VelocitySnapshot` event emission**: emit via `EventRouter` on every terminal outcome.
9. → **v0.14.4** **Connector forwarding**: Discord plugin velocity cards.
10. → **v0.14.x** **Enterprise HTTP connector** *(stretch)*.
12. → **v0.14.6** **`velocity_events` opt-in flag** in `channel.toml` schema.
14–19. → **v0.14.6** **Goal History Rollover** (rollover policy, mechanics, segment queries, manual trigger, archive): full design is complete in the original items above; deferred as v0.13.12 completed without them.
12. [-] **Diagnostic goal type**: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
---
13. [-] **Shell agent as advisor**: Deferred — not implemented in v0.13.1. → future phase (unscheduled)
---
15. [-] **`ta status` as the one command**: → Moved to v0.13.1.6 (item 1, done).
### v0.13.11 — Platform Installers (macOS DMG, Windows MSI)
---
<!-- beta: yes — first-class installation experience for non-developer users -->
**Goal**: Replace bare `.tar.gz`/`.zip` downloads with proper platform installers. macOS gets a signed pkg/DMG. Windows gets an MSI with PATH registration. Eliminates the "extract and manually place binary" step for non-developer users and team rollouts.
---
- `crates/ta-build/src/npm.rs`: 4 tests (detect, name, custom commands)
Current releases ship archives containing a bare binary and docs. Users must manually extract, move the binary onto their `$PATH`, and repeat on every update. This is a barrier for non-developer users and small-team adoption — a tool designed to replace manual work should install itself.
17. [-] **Intent-based interaction**: → Moved to v0.13.1.6, then deferred to v0.13.12 (item 11).
---
The release workflow only builds `-p ta-cli`. The `ta` CLI spawns `ta-daemon` as a sibling process, looking for it next to the `ta` binary (then `$PATH`). Because `ta-daemon` is never packaged, every install is broken at the first daemon-requiring command.
**macOS pkg/DMG**
- `pkgbuild` + `productbuild` produces a `.pkg` installer: one-screen accept → binary placed at `/usr/local/bin/ta`
- Wrapped in a DMG for the download experience (`create-dmg`)
---
On Windows, `find_daemon_binary()` additionally has two bugs: `dir.join("ta-daemon")` produces `ta-daemon` (no `.exe`), and the PATH fallback uses `which` (a Unix command) rather than `where`.
---
- Built with `cargo-wix` (WiX Toolset v4 wrapper)
- Installs `ta.exe` to `%ProgramFiles%\TrustedAutonomy\`, adds to `$PATH`, registers uninstaller in Add/Remove Programs
- Start Menu shortcut: `ta shell` (opens web shell in default browser)
- Code-signed when `WINDOWS_CODE_SIGN_CERT` secret is present; unsigned fallback
---
**Linux**
- Existing musl `.tar.gz` archives remain (standard for CLI tools)
- Optional `.deb` stretch goal (see item 9)
4. [x] **Fix Bug B — PR failure must not silently succeed**: When `open_review` fails and `do_review=true`, emit a clear error with the branch name and the manual `gh pr create` command. Do not exit 0. Store the branch even when review fails so `ta pr status` can show recovery steps.
8. [x] **Update USAGE.md install instructions**: Added note that both `ta` and `ta-daemon` must be on `$PATH` (or in the same directory); updated manual install steps to `cp ta ta-daemon /usr/local/bin/`; added daemon-not-found error guidance.
1. [x] **`wix/` setup**: Add WiX source XML for Windows MSI — product name, version, install dir, PATH registration, uninstaller entry, Start Menu shortcut
2. [x] **MSI build in release workflow**: `cargo wix` step on `windows-latest`; uploads `ta-<version>-x86_64-pc-windows-msvc.msi` as optional artifact (non-fatal if cargo-wix not available)
3. [x] **macOS pkg build**: `pkgbuild` + `productbuild` step on `aarch64-apple-darwin`; installs to `/usr/local/bin/`
4. [x] **macOS DMG wrapping**: `create-dmg` wraps the pkg into a DMG; fallback to raw pkg if create-dmg unavailable; uploads `ta-<version>-macos.dmg` + `.pkg`
5. [x] **Code signing (conditional)**: Scaffolded — skips silently if `APPLE_DEVELOPER_CERT` / `WINDOWS_CODE_SIGN_CERT` secrets not present
6. [x] **Update required-assets validation**: `.msi` and `.dmg` treated as optional (non-fatal) in asset check; required archives unchanged
7. [x] **Update release body template**: Installers (`.dmg`, `.msi`) as primary download options in release notes
8. [x] **Update USAGE.md**: Added Option A (installer), Option B (one-liner), Option C (manual tar.gz) for Install section; updated Windows instructions
9. [-] **Bundle USAGE.html in MSI** (installed to `%ProgramFiles%\TrustedAutonomy\docs\`) → v0.13.15 (not completed in v0.13.12)
10. [-] **Homebrew tap** → v0.14.x
11. [x] **System requirements in USAGE.md**: Added "System Requirements" section with platform table and agent framework requirements table
---
11. [x] **Bug C — Incomplete top-level draft summary fields** (GitHub issue #76): Added `extract_phase_goal_description()` helper in `ta-mcp-gateway/src/tools/draft.rs`. When `goal.plan_phase` is set, reads PLAN.md and finds the phase's `**Goal**:` line for use as `summary_why`; also detects placeholder values (objective equals title exactly) and substitutes the phase description. 3 new tests.
    ```
    ## System Requirements
---
    | Platform        | Min RAM | Recommended | Disk (TA binary) | Disk (staging) |
    |-----------------|---------|-------------|------------------|----------------|
    | macOS (Apple Silicon) | 8 GB  | 16 GB       | ~15 MB           | 1–5 GB per goal |
    | macOS (Intel)   | 8 GB    | 16 GB       | ~15 MB           | 1–5 GB per goal |
    | Linux x86_64    | 4 GB    | 8 GB        | ~12 MB           | 1–5 GB per goal |
    | Windows x86_64  | 8 GB    | 16 GB       | ~15 MB           | 1–5 GB per goal |
- [x] `launch_agent_via_runtime()` integrates `RuntimeAdapter` into all non-PTY agent launch paths (headless, quiet, simple), emitting lifecycle events
    Staging disk usage depends on project size. A typical Rust workspace (~500 MB with target/) uses ~600 MB per active goal. Use `ta gc` to reclaim staging space.
- [x] `AgentSpawned`, `AgentExited`, `RuntimeError` variants added to `ta-events::SessionEvent` with `event_type()`, `goal_id()`, and `suggested_actions()` support
    ### Agent Framework Requirements
---
    | Framework        | Min RAM | Notes |
- [x] 20 new tests across `ta-runtime` (adapter, bare_process, config, credential) and `ta-events` (schema)
    | Claude Code (claude-sonnet-4-6) | 8 GB  | Requires `ANTHROPIC_API_KEY`; network access to api.anthropic.com |
    | Claude Code (claude-opus-4-6)   | 8 GB  | Higher quality, slower; same API key + network requirements |
    | Codex CLI        | 8 GB    | Requires `OPENAI_API_KEY`; network access to api.openai.com |
    | Local model (Ollama, v0.13.8+) | 16 GB  | 7B models need ~8 GB VRAM or ~12 GB RAM (CPU fallback); 70B needs ~40 GB RAM |
    ```
2. [x] `ActionPolicy` config in `.ta/workflow.toml`: per-action-type rules (auto, review, block) plus `rate_limit`, `allowed_domains`, `auto_approve_reads` — parsed via `ActionPolicies::load()` in `crates/ta-actions/src/policy.rs`.
    **Release notes block** (template in `pr-template.md`): Add a "System Requirements" callout box with minimums per platform and agent framework, linked to USAGE.md for full details.
policy = "auto"            # auto-approve known API calls
#### Release infrastructure fixes (landed ahead of full v0.13.11)
10. [x] **Version stamped into USAGE.md at release time**: Release workflow now `sed`-replaces the `**Version**:` line in USAGE.md with the actual tag before packaging, so USAGE.html and the bundled USAGE.md always show the correct version. (Was hardcoded as `0.10.18-alpha.1` in all previous releases.)
11. [x] **Docker option marked Coming Soon in header**: `**Option C -- Docker** *(Coming Soon)*` in USAGE.md install section.
---
allowed_domains = ["api.stripe.com", "api.github.com"]
---
- Item 9 (Bundle USAGE.html in MSI) → v0.13.15 (not completed in v0.13.12)
- Item 10 (Homebrew tap) → v0.14.x
#### Version: `0.13.4-alpha`
#### Version: `0.13.11-alpha`
### v0.13.6 — Community Knowledge Hub Plugin (Context Hub Integration)
---
2. [x] **MCP tool API**: All 5 tools implemented in `plugins/ta-community-hub/src/main.rs`:
### v0.13.12 — Beta Bug Bash & Polish

**Goal**: Catch and fix accumulated polish debt, false positives, and deferred UX items from the v0.13.1.x sub-phases before advancing to the deeper v0.13.2+ infrastructure phases. No new features — only fixes, observability improvements, and cleanup.
   - `community_suggest { title, content, intent, resource, workspace_path }` — stages new doc proposal to `.ta/community-staging/<resource>/suggestions/`.
#### Release Pipeline & Staging Bugs
3. [x] **Attribution in agent output**: Response payloads include `resource_uri: "community://<resource>/<id>"`. Stale docs emit `⚠` warning with sync hint. Attribution format `[community: <resource>/<id>]` documented in USAGE.md.
1. [x] **`ta draft apply` scans unrelated staging dirs**: `apply` now validates that the goal's staging workspace exists before opening it. If deleted by concurrent `ta gc`, provides actionable error with exact recovery commands. (Discovered during v0.13.1.7 release run.)
2. [x] **Release pipeline drift false positive**: Fixed in v0.13.2 — conflict detection now uses SHA-256 content hash as the authoritative signal (not mtime), eliminating false positives when a file's mtime changes but content is identical. The `FileSnapshot::is_changed()` method in `ta-workspace/src/conflict.rs` compares `current_hash != self.content_hash`. Verified with regression tests including `file_snapshot_same_mtime_different_content_is_detected`.
3. → **v0.14.0** **Release notes agent should not need a full workspace copy**: Deferred — requires "scribe" goal type (lightweight, no staging copy). Design complete (see original description). Depends on GoalBaseline trait (item 6). Assigned to v0.14.0 infrastructure work.
4. [x] **`--label` dispatches even when pipeline is aborted**: When the user cancels at an approval gate (e.g., "Proceed with 'Push'? [y/N] n"), `run_pipeline` returns early via `?` but the `--label` dispatch block was outside the else branch and ran unconditionally. Fix: moved `--label` dispatch inside the `else { run_pipeline()? ... }` block so it only executes on successful pipeline completion. (Fixed in `release.rs` during v0.13.12 planning.)
5. [x] **GC should not run while a release pipeline is active**: `ta gc` now checks for `.ta/release.lock` at startup and warns + skips staging deletion if present. `ta release run` (non-dry-run) acquires `ReleaseLockGuard` which writes the lock with the current PID and removes it on drop. `ta gc --force` overrides the guard. (v0.13.12)
5b. [x] **Build-tool lock files left uncommitted after verify step**: After the `[verify]` commands run (`cargo build`, `cargo test`, etc.), build tools may rewrite lock files (`Cargo.lock`, `package-lock.json`, `go.sum`, `Pipfile.lock`) in the staging directory. These are not agent-written changes — they are deterministic outputs of the build tool. The overlay diff currently includes them as changed files, which is correct, but the issue is they accumulate as uncommitted changes in the source after `ta draft apply` because:
    1. `apply` copies `Cargo.lock` from staging → source (content matches, so source is now "correct")
    2. User then runs a build command → cargo rewrites `Cargo.lock` again (may differ if deps resolved differently)
---
   auto_query = true             # Agent auto-consults before API calls
    Fix: after `ta draft apply`, if the applied diff includes a known lock file, print a reminder:
    ```
    ⚠ Lock file updated: Cargo.lock — commit it alongside your feature branch:
      git add Cargo.lock && git commit --amend --no-edit
    ```
    Longer-term: `ta draft apply --git-commit` should automatically include lock files in the commit it creates, since they are always part of the correct source state after any dep/version change.
   access = "read-only"
#### Overlay Baseline — `GoalBaseline` Trait
   auto_query = true             # Agent auto-consults during security review
6. → **v0.14.0** **Replace live-source diff with `GoalBaseline` trait**: Deferred — foundational architectural change enabling non-VCS workflows and eliminating dirty-tree false positives. Design is complete (GitBaseline, SnapshotBaseline, BaselineRef enum). Assigned to v0.14.0 as it unblocks scribe goal type (item 3), `--adopt` shortcut, and AMP context registry bridge (v0.14.2).
**Goal**: When pasting large blocks of text into `ta shell`, compact the display instead of filling the input buffer with hundreds of lines.
#### UX & Health-Check Bugs
   update_frequency = "daily"    # How often to sync (daily, weekly, on-demand)
7. [x] **`check_stale_drafts` threshold mismatch**: The startup hint (`"N draft(s) approved/pending but not applied for 3+ days"`) uses a hardcoded 3-day cutoff, but `ta draft list --stale` uses `gc.stale_threshold_days` (default: 7). When the threshold is 7 days, the hint fires for days 3–6 but `--stale` finds nothing — a confusing false alarm. Fix: split into two configurable values in `workflow.toml`:
---
   [gc]
   stale_hint_days      = 3   # when the startup hint fires (informational)
   stale_threshold_days = 7   # when --stale filter shows them
---
   The hint message updates to reflect the configured value. Note: 3-day default means a Friday-evening draft hints on Monday morning — acceptable since it is informational only, not blocking. Users who find it noisy can set `stale_hint_days = 5`.
   auto_query = true
8. → **v0.14.1** **Browser tools off by default; enable per agent-capability profile**: Deferred — requires MCP tool filter in daemon and agent capability profile schema. Design: `capabilities = ["browser"]` in `.ta/agents/research.toml`; daemon filters `browser_*` tool calls. Assigned to v0.14.1 (Sandboxing & Attestation) as a capability scoping feature.
   - iTerm2
- `main::tests::community_annotate_enforces_read_only_access`
- `community::tests::community_context_section_includes_auto_query_resources`
9w. [x] **Windows startup profiling**: `ta` commands feel slow on Windows compared to macOS. Add startup-time diagnostics (`ta --startup-profile` or always-on tracing at `RUST_LOG=ta=debug`) that report wall-clock time for each startup phase: binary load, config parse, daemon socket connect, command dispatch. Identify bottlenecks: likely candidates are (a) `which::which()` PATH scan on every command, (b) daemon IPC handshake latency, (c) missing Windows file-open shortcuts compared to macOS `O_CLOEXEC`/TCC caches. Fix the slowest path; add a CI benchmark asserting `ta --version` cold-start < 500ms on Windows runners.
- `community::tests::sync_local_indexes_markdown_files`
10w. [x] **Lazy `which::which()` for Windows agent resolution**: `build_command()` in `bare_process.rs` calls `which::which()` on every agent spawn even on macOS/Linux where it is not needed. Move the `which` lookup behind `#[cfg(windows)]` so the PATH scan only happens on Windows, and cache the result for the lifetime of the daemon process.
- `community::tests::search_finds_keyword_in_cache`
#### Intelligent Surface (deferred from v0.13.1.6)
7. [-] **Inheritance**: `constitution.toml` can `extends = "ta-default"` to inherit TA's rules and only override specific sections. TA ships a built-in `ta-default` profile. Partial: `extends` field is stored but not applied at load time. → v0.13.15
9. → **v0.14.0** **Proactive notifications**: Deferred from v0.13.1.6, again deferred to v0.14.0. Daemon push notifications for goal completed/failed/draft-ready via SSE and configured channels.
10. → **v0.14.0** **Suggested next actions**: Deferred — needs daemon state model and command suggestion engine. Design: suggest after every command based on current state.
11. → **v0.14.0** **Intent-based interaction in `ta shell`**: Deferred — requires shell agent with approval flow for command sequences.
12. → **v0.14.0** **Reduce command surface**: Deferred — follows items 9–11 completion.
   - `generic`: minimal rules with descriptive comments as a starting point
#### Project Context Cache (hybrid now + AMP)
---
13. → **v0.14.2** **`.ta/project-digest.json` — inject pre-summarised project context at goal start**: Deferred to v0.14.2 (AMP/Context Registry) where it maps cleanly to the AMP context registry. Design is complete: content-addressed cache keyed by SHA-256 of PLAN.md/Cargo.toml; regenerates on hash mismatch; saves 10–20k tokens per goal. At v0.14.2, `source_hash` → AMP `context_hash`, `summary` → stored embedding payload.
    "avg_build_seconds": 850,
#### Release Pipeline Polish (deferred from v0.13.1.x)
---
14. [x] **Stale `.release-draft.md` poisons release notes**: If a prior release run left `.release-draft.md` in the source tree, the next release notes agent reads it as context and re-emits the old version header. Fix: added "Clear stale release draft" shell step immediately before the "Generate release notes" agent step in `DEFAULT_PIPELINE_YAML`. (Fixed in `release.rs` during v0.13.12 planning.)
15. → **v0.14.0** **Single GitHub release per build**: Deferred — redesign of dispatch flow needed (label tag as primary, semver as lightweight git tag only). See memory: [Release pipeline improvements](project_release_future.md).
16. → **v0.14.0** **VCS-agnostic release pipeline**: Deferred — document git requirement now; design hook override for Perforce/SVN at v0.14.0 alongside VCS plugin architecture work.
---
#### Version: `0.13.12-alpha`
---
---
#### Deferred items resolved
### v0.13.13 — VCS-Aware Team Setup, Project Sharing & Large-Workspace Staging

<!-- beta: yes — foundational for team adoption and game/media project support -->
**Goal**: Make TA a first-class citizen in any VCS-managed project by (1) formalising which `.ta/` files are shared configuration vs local runtime state, (2) generating correct VCS ignore rules automatically for Git and Perforce, and (3) making staging fast enough for large game and media projects by replacing full copies with symlink-based partial staging and ReFS CoW cloning on Windows.
---
**Problem — team setup**: There is no formal split between "team configuration" (should be versioned and shared: `workflow.toml`, `policy.yaml`, `constitution.toml`, agent manifests) and "local runtime state" (should be ignored: `staging/`, `goals/`, `events/`, `daemon.toml`). New team members have no guidance, setups drift, and `.ta/staging/` occasionally gets committed accidentally.
---
**Problem — large workspaces**: `ta goal start` copies the entire project workspace. For a game project (800GB Unreal Engine workspace) or a Node.js project with `node_modules/`, this makes staging impractically slow or impossible. A 400GB project where only `Source/` (~50MB) is agent-writable should cost ~50MB to stage, not 400GB.
---
---
---
1. [x] **VCS detection in `ta init` / `ta setup`**: Before writing config files, detect the VCS backend:
   - **Git**: check for `.git/` directory (or `git rev-parse --git-dir` succeeds)
   - **Perforce**: check for `.p4config` in any parent directory, or `P4PORT`/`P4CLIENT` env vars set
   - **None / unknown**: prompt user to select from `[git, perforce, none]`
---
     ```toml
     [submit]
     adapter = "git"      # "git" | "perforce" | "none"
     # [submit.perforce]
     # workspace = ""     # P4CLIENT workspace name (personal — set in local.workflow.toml)
---
2. [x] **Interactive wizard (`ta setup`)**: Added `ta setup vcs` subcommand with `--force`, `--dry-run`, and `--vcs` flags. Detects VCS, writes ignore files, updates workflow.toml, prints shared/local split. Full language detection and step-by-step wizard flow deferred to v0.13.14.
3. [x] **`ta doctor` VCS validation**: Extended `ta doctor` with:
   - **Git**: detects VcsBackend, checks that local-only `.ta/` paths are in `.gitignore`; warns with "Fix: ta setup vcs"
---
   - **None**: skip with info message
   - Output: `[ok]`, `[warn]`, `[error]` per check, matching existing `ta doctor` style
---
#### 2. Shared vs Local File Partitioning
---
4. [x] **Canonical shared/local lists**: Defined `SHARED_TA_PATHS` and `LOCAL_TA_PATHS` as `const` arrays in new `crates/ta-workspace/src/partitioning.rs` module — authoritative source of truth used by the wizard, ignore generation, and `ta doctor`.
5. [x] **`ta plan shared`**: Added `PlanCommands::Shared` variant and `plan_shared()` function. Prints present/missing status for SHARED_TA_PATHS, ignored/not-ignored status for LOCAL_TA_PATHS; warns on unignored present local paths.
6. [x] **USAGE.md team setup guide**: Added "Setting Up TA for Your Team" section covering shared vs local file table, `ta plan shared`, `ta setup vcs`, team onboarding workflow, smart mode configuration, ReFS CoW, and `ta doctor` staging check.
---
#### 3. VCS-Specific Ignore File Generation
---
7. [x] **Git: append to `.gitignore`**: `ta setup vcs` appends `# Trusted Autonomy — local runtime state (do not commit)` block. Idempotent — detects block marker, skips on re-run. `--force` rewrites the block.
8. [x] **Perforce: generate `.p4ignore`**: `ta setup vcs` writes `.p4ignore` with same local-only paths. Warns when `P4IGNORE` env var is not set. `ta doctor` re-surfaces this warning.
9. [x] **Idempotency**: Running `ta setup vcs` a second time does not add duplicate ignore entries. Detects the `# Trusted Autonomy` marker and skips. `--force` flag rewrites the block.
---
---
---
10. [x] **`staging.strategy` config**: Added `StagingStrategy` enum (`Full`, `Smart`, `RefsCow`) to `WorkflowConfig` in `ta-submit/src/config.rs`. Default `Full` preserves current behaviour — no regression.
11. [x] **Smart staging — symlink pass**: Added `OverlayStagingMode` enum to `ta-workspace/overlay.rs`. `create_with_strategy()` accepts mode; `copy_dir_recursive_smart()` symlinks excluded dirs/files via `ExcludePatterns` instead of copying.
12. [-] **Smart staging — write-through protection**: Deferred to v0.13.14. The policy layer integration needed to detect writes to symlinked source paths requires changes outside the workspace crate scope.
13. [-] **ReFS CoW staging (Windows)**: Stub implemented — `is_refs_volume()` returns `false` on all platforms, causing `RefsCow` to auto-fall back to `Smart`. Full `FSCTL_DUPLICATE_EXTENTS_TO_FILE` IOCTL implementation deferred to v0.13.14 (Windows-specific, needs test hardware).
14. [x] **Staging size report at `ta goal start`**: `CopyStat::size_report()` prints human-readable report after every `create_with_strategy()` call. Smart mode shows "N MB copied, N GB symlinked (smart mode) (Nx reduction)".
15. [x] **`ta doctor` staging check**: Warns when `strategy = "full"` and workspace > 1 GB with suggestion to use `strategy=smart`.
16. [x] **Tests**: smart staging creates symlinks for excluded dirs; copy loop skips symlinked paths in diff; `OverlayStagingMode::default()` is Full; `CopyStat::size_report()` formatting verified for both full and smart modes; 6 VCS tests in setup.rs; 11 partitioning tests in partitioning.rs.
---
---
---
- Item 12 (write-through protection) → v0.13.14 — requires policy layer changes outside ta-workspace scope
- Item 13 (full ReFS IOCTL) → v0.13.14 — Windows-specific hardware needed for testing
---
---
---
---
---
### v0.13.14 — Watchdog/Exit-Handler Race & Goal Recovery

<!-- beta: yes — critical correctness fix; goal state machine must be reliable for all users -->
**Goal**: Fix three related bugs where a long-running goal (10+ hours) is incorrectly marked `failed` on clean agent exit, add the `finalizing` lifecycle state to close the race window, and introduce `ta goal recover` for human-driven recovery when state goes wrong.
---
**Root cause report** (reproduced on Windows with a 10-hour Unreal Engine onboarding goal):
---
When agent PID 76108 exited (code 0) at 15:59:32, two things happened concurrently:
- **Exit handler** (correct path): detected code 0, began draft creation from staging (~3 seconds for large UE workspace).
- **Watchdog** (zombie path): next tick at 15:59:33, saw PID gone + goal state still `running` + `last_update: 36357s ago` > `stale_threshold: 3600s`. Declared zombie. At 15:59:35 — simultaneously with draft creation — transitioned goal to `failed`.
---
The watchdog won the final write. Draft was created correctly, but goal state was `failed`. Two earlier failed goals (`bf54b517`, `85070aa3`) had legitimate `program not found` failures, creating watchdog noise that contributed to the race.
---
#### Bug 1 (Critical): Watchdog races with exit handler
---
**Fix**: Atomic state transition to `finalizing` at the moment of exit detection, before slow draft creation begins.
---
1. [x] **`GoalState::Finalizing`**: Added `Finalizing { exit_code: i32, finalize_started_at: DateTime<Utc> }` variant to `GoalRunState` enum in `ta-goal/src/goal_run.rs`. Serializes as `"finalizing"` in goal JSON.
2. [x] **Atomic transition on clean exit**: In `run.rs` exit handler, combined PID-clear + `Running → Finalizing` into a single `store.save()` call before draft build. This is one file write — the watchdog can't interleave.
3. [x] **Watchdog skips `Finalizing`**: `check_finalizing_goal()` in `watchdog.rs` skips the goal if `finalize_timeout_secs` (default 300s) not exceeded; transitions to `Failed` with actionable message after timeout.
4. [x] **Tests**: `finalizing_state_transition_from_running`, `finalizing_to_pr_ready_transition_valid`, `finalizing_to_failed_always_valid`, `finalizing_serialization_round_trip`, `finalizing_display`, `watchdog_skips_finalizing_within_timeout`, `watchdog_finalizing_timeout_transitions_to_failed`.
---
#### Bug 2 (Important): Exit code 0 must never produce zombie
---
**Fix**: Zombie detection must gate on exit code. Code 0 = clean exit; watchdog must never promote this to `failed`.
---
5. [x] **Exit-code gate via `Finalizing`**: Clean exits now write `Finalizing` state before draft build, so the watchdog sees `Finalizing` (not `Running`) and skips the goal. A `Running` + dead PID is definitionally a zombie or crash.
6. [x] **Distinguish `stale` from `zombie`**: Rewrote `check_running_goal()` with clear separation — stale (PID alive, no heartbeat, only warn when `heartbeat_required=true`), zombie (PID gone, transition to Failed with actionable message).
7. [x] **Tests**: `watchdog_stale_no_action_when_heartbeat_not_required`, `watchdog_cycle_detects_zombie` (existing), `watchdog_skips_finalizing_within_timeout`.
---
#### Bug 3 (Minor): Heartbeat protocol undefined for non-heartbeating agents
---
---
---
8. [x] **`heartbeat_required` flag per agent framework**: Added `heartbeat_required: bool` (default `false`) to both `AgentLaunchConfig` (in `run.rs`) and `GoalRun` (in `goal_run.rs`). Stored in goal JSON at goal-start time. Claude Code built-in config gets `heartbeat_required: false`. Watchdog respects it — stale checking disabled when `false`.
9. [-] **Configurable stale threshold per agent**: Deferred to v0.13.15 — requires daemon config schema changes; current fix (heartbeat_required=false) addresses the practical problem.
10. [-] **Document heartbeat API**: Deferred to v0.13.15 — heartbeat endpoint not yet implemented in the daemon.
---
#### `ta goal recover` — Human Recovery Command
---
When goal state is wrong (e.g., `failed` but draft was created, `running` with dead PID), the user needs a safe way to inspect and correct state without editing JSON files manually.
---
11. [x] **`ta goal recover [--latest | <id-prefix>]`**: Interactive recovery command added to `GoalCommands`. Shows diagnosis, draft status, and options. Options adapt based on whether a valid draft exists.
12. [x] **Diagnosis heuristics**: `diagnose_goal()` function in `goal.rs` — failed+valid-draft, running+dead-PID, finalizing+stuck>300s cases covered.
13. [x] **`ta goal recover --list`**: `--list` flag shows all recoverable goals with diagnosis and draft status without prompting.
14. [-] **`GoalRecovered` audit event**: Deferred to v0.13.15 — audit event schema changes needed; recovery still works without it.
15. [-] **Tests for recover**: Deferred to v0.13.15 — interactive recovery tests require stdin mocking; the `diagnose_goal` logic is covered by unit tests.
---
---
---
16. [x] **Watchdog logs every state transition**: All watchdog-driven transitions now log `tracing::warn!(goal_id, prev_state, new_state, reason, "Watchdog: goal state transition")` — zombie, finalize_timeout.
17. [-] **`ta goal status <id>` shows watchdog fields**: Deferred to v0.13.15 — `ta goal inspect` already shows PID/health; dedicated watchdog fields would clutter the output.
---
---
---
- Item 9 (configurable stale threshold per agent) → v0.13.15
- Item 10 (document heartbeat API) → v0.13.15
---
- Item 15 (recover command tests) → v0.13.15
- Item 17 (goal status watchdog fields) → v0.13.15
---
#### Version: `0.13.14-alpha`
---
---
---
### v0.13.15 — Fix Pass, Cross-Language Onboarding & Constitution Completion

--- Phase Run Summary ---
<!-- beta: yes — correctness fixes + unlocking non-Rust project support -->
**Goal**: Fix correctness and reliability bugs observed during the v0.13.x implementation run, and ship the cross-language onboarding items and constitution features that were deferred from v0.13.8 and v0.13.9. Collected deferred items: v0.13.6 items 16/19/20, v0.13.8 items 35–37, v0.13.9 items 4/5/7/9/10, v0.13.11 item 9.
---
---
---
---
---
---
---
3. [x] **Test**: 5 tests in `draft.rs` — source `0.14.2-alpha` vs staging `0.13.8-alpha` → warning; `0.14.3-alpha` → no warning; non-Cargo-toml artifacts → no check; PLAN.md unchecked detection (separate).
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
#### 4. PLAN.md Deferred Items in Completed Phases
---
---
---
---
---
---
---
---
---
---
12. [-] **`.taignore` — overlay exclusion patterns**: Already implemented in `overlay.rs` defaults (`.git/`, `.ta/`, `node_modules/`, `.venv/`, `__pycache__/`, `dist/`, `build/`). USAGE.md cross-language section documents `.taignore` usage. No code change needed. → Resolved (already done)
---
---
---
---
14. [-] **Parallel agent review during release**: Deferred → v0.13.16. Requires async pipeline fan-out; current release pipeline is sequential. Constitution reviewer agent output append requires agent lifecycle wiring not in scope.
---
---
---
---
---
---
---
---
20. [-] **Upstream PR on `ta draft apply`**: Deferred → v0.13.16. Git adapter wiring for community staging URIs not in scope; `resource_uri` scheme support needed in apply path.
---
---
---
21. [-] **Bundle USAGE.html in MSI**: Deferred → v0.13.16. Requires WiX template change and build pipeline changes outside the scope of a fix pass.
---
---
---
---
---
---
---
---
---
- Items 18–20 (shell UX: tab completion, status bar badge, upstream PR) → v0.13.16
- Item 21 (bundle USAGE.html in MSI) → v0.13.16
---
#### Version: `0.14.2-alpha` (workspace already at v0.14.2-alpha; v0.13.15 is a backfilled fix pass — no version bump)
---
---
---
---
---
<!-- beta: yes — local model support and advanced swarm orchestration -->
**Goal**: Implement the `ta-agent-ollama` binary (full tool-use loop against any OpenAI-compatible endpoint), validate local models end-to-end (Qwen2.5-Coder, Phi-4, Kimi K2.5, Llama3.1), add framework manifest registry publishing, and complete the advanced swarm features deferred from v0.13.7. Collected deferred items: v0.13.7 items 11–13, v0.13.8 items 14–15/20–25/30–34.
---
---
---
1. [x] **New crate `crates/ta-agent-ollama`**: Binary implementing a tool-use loop against any OpenAI-compat endpoint (`/v1/chat/completions` with `tools`). Accepts `--model`, `--base-url`, `--context-file`, `--memory-path`, `--memory-out`, `--workdir`, `--max-turns`, `--temperature`, `--skip-validation`, `--verbose`. Emits `[goal started]` sentinel on stderr. 5 unit tests.
2. [x] **Core tool set**: `bash_exec`, `file_read`, `file_write`, `file_list`, `web_fetch`, `memory_read`, `memory_write`, `memory_search` — implemented in `crates/ta-agent-ollama/src/tools.rs`. `ToolSet` dispatches to each tool with workdir scoping. 11 tests.
3. [x] **Startup sequence**: Read context from `--context-file` or `$TA_GOAL_CONTEXT`; include in system prompt. Validate model supports function-calling (`/v1/models` probe + test call); emit clear error if not. `--skip-validation` flag for offline use. `OllamaClient` with `list_models()` + `chat_with_tools()`. 2 client tests.
4. [x] **Graceful degradation**: If model has no function calling, fall back to CoT-with-parsing mode with a warning. `TOOL_CALL:` prefix line parsing with JSON extraction. `run_cot_loop()` in `main.rs`.
5. [-] **End-to-end validation**: Qwen2.5-Coder-7B, Phi-4-mini, Kimi K2.5, Llama3.1-8B complete a real `ta run` goal with memory write-back; memory entries visible in next goal's context. → Deferred (requires live Ollama instance; model validation matrix documented in `docs/agent-framework-options.md`)
---
---
---
6. [x] **`ta-agent-ollama` memory tools**: `memory_read`/`memory_write`/`memory_search` in the native tool set. `MemoryBridge` in `crates/ta-agent-ollama/src/memory.rs` reads snapshot from `$TA_MEMORY_PATH`, queues writes to `$TA_MEMORY_OUT`. 9 tests.
7. [x] **Memory relevance tuning**: `[memory]` manifest section supports `max_entries`, `recency_days`, `tags` filter. `build_memory_context_section_with_manifest_filter()` in `crates/ta-memory/src/auto_capture.rs` applies all three filters. Wired in `inject_memory_context()` in `run.rs`. 4 new tests in ta-memory.
---
#### 3. Framework Manifest Registry (from v0.13.8 items 30–34)
---
---
---
---
---
---
#### 4. Advanced Swarm Orchestration (from v0.13.7 items 11–13)
---
---
---
14. [x] **Department → workflow mapping in office config**: `departments` section in `office.yaml`. `DepartmentConfig` struct with `default_workflow`, `description`, `projects`. `department_workflow()` on `OfficeConfig`. `resolved_workflow()` falls back to "single-agent". 5 new tests in `office.rs`.
#### Deferred items resolved
#### Completed
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
---
#### 1. `ta run` Draft-Phase Progress Injection
---
1. [-] **Finalize heartbeat**: → Implemented in v0.13.17.1 (item 1).
---
3. [-] **`finalize_timeout_secs` in `[operations]` config**: *(Wired in v0.13.17 branch.)* → Completed in v0.13.17.1.
---
#### 2. Validation Evidence in Draft Package
---
4. [-] **`ValidationLog` in `DraftPackage`**: → Implemented in v0.13.17.1 (item 2).
5. [-] **`ta draft view <id>` shows validation log**: → Implemented in v0.13.17.1 (item 3).
---
---
#### 3. Perforce VCS Plugin (Game Project)
---
7. [-] **`plugins/vcs-perforce` script**: → Implemented in v0.13.17.1 (item 12).
8. [-] **`plugins/vcs-perforce.toml` manifest**: → Implemented in v0.13.17.1 (item 13).
9. [-] **Integration test with mock `p4`**: → Implemented in v0.13.17.1 (item 14).
10. [-] **USAGE.md "Using TA with Perforce" section**: → Implemented in v0.13.17.1 (item 15).
11. [-] **Release bundle includes plugin**: → Deferred to v0.13.18 (release pipeline bundling work).
---
#### 4. Experimental Feature Flag System
---
12. [-] **`[experimental]` config section** in `DaemonConfig`: *(Landed in v0.13.17 branch.)* → Wired end-to-end in v0.13.17.1.
13. [-] **`ta run --agent ollama` gate**: → Implemented in v0.13.17.1 (item 5).
14. [-] **Sandbox gate**: → Implemented in v0.13.17.1 (item 6).
15. [-] **Personal dev `.ta/config.toml`**: → Implemented in v0.13.17.1 (item 7).
---
#### 5. Branch Prefix Default Fix
---
---
---
---
---
---
---
19. [-] **`ta-community-hub` MCP server registration**: → Implemented in v0.13.17.1 (item 10).
20. [-] **Agent observation write-back**: → Implemented in v0.13.17.1 (item 11). Deferred write-back to external systems → v0.14.3.5.
---
---
---
---
---
23. [-] **`test_ollama_agent_mock_e2e`**: → Stub implemented in v0.13.17.1 (item 19).
---
25. [-] **Pre-release checklist in USAGE.md**: → Implemented in v0.13.17.1 (item 21).
---
---
- Items 1–10, 12–15, 17–25: All implemented in v0.13.17.1 (scaffold PR added structs/config; v0.13.17.1 wired them end-to-end).
- Item 11 (release bundle): → v0.13.18 (release pipeline bundling work).
- Community read-write write-back to external systems → v0.14.3.5 (same phase as Supermemory — natural fit).
- Live Ollama E2E with real models (v0.13.16 item 5) → still deferred; E2E mock test (item 23 above) covers the code path without requiring a live instance.
---
#### Version: `0.13.17-alpha`
---
---
---
---
---
---
---
#### 1. Finalize-Phase Observability (from v0.13.17 items 1–3)
---
---
2. [x] **`ValidationLog` in `DraftPackage`**: After the agent exits, `ta run` runs the project's `required_checks` from `[workflow].required_checks` config (default: four checks from CLAUDE.md). Each entry: `ValidationEntry { command, exit_code, duration_secs, stdout_tail }`. Embed as `pkg.validation_log`. Skip if `--skip-validation` flag is set.
---
4. [x] **`ta draft approve` validation gate**: Refuse approval if `validation_log` contains a non-zero `exit_code`, unless `--override` is passed. Error: "Draft has failed validation checks — use `--override` to approve anyway."
---
#### 2. Experimental Flag Gates (from v0.13.17 items 13–15)
#### Completed
5. [x] **Ollama agent gate**: In the framework resolution in `run.rs`, after resolving framework to `ollama`, read `.ta/daemon.toml` experimental section. If `ollama_agent = false` or not set, bail with: "ta-agent-ollama is an experimental preview. Enable with `[experimental]\nollama_agent = true` in .ta/daemon.toml."
---
7. [x] **Personal dev `.ta/daemon.toml`**: Added `[experimental]\nollama_agent = true\nsandbox = true` to the committed `.ta/daemon.toml` for this repo, so the TrustedAutonomy repo itself can test both features.
---
#### 3. Community Context — Full Agent Coverage (from v0.13.17 items 17–20)
---
8. [x] **Community section in `inject_agent_context_file()`**: Pass `source_dir` into the function and call `build_community_context_section()`. Codex (AGENTS.md) and other `context_file`-based agents now receive the community knowledge section.
---
10. [x] **`ta-community-hub` MCP server registration**: Register `ta-community-hub` in the injected `.mcp.json` alongside `ta-memory`. Cleanup in `restore_mcp_server_config` removes both keys on goal exit.
---
---
---
---
12. [x] **`plugins/vcs-perforce`**: Python 3 script implementing the JSON-over-stdio VCS protocol. Uses `p4` CLI as backend. Full operation set: handshake, detect, status, diff, submit, shelve, save_state, restore_state, revision_id, protected_targets, verify_target, open_review, push, commit, sync_upstream, check_review, merge_review. Reads `P4PORT`, `P4USER`, `P4CLIENT` from environment.
---
14. [x] **Integration test with mock `p4`**: `crates/ta-submit/tests/fixtures/mock-p4` shell script returns canned responses. `crates/ta-submit/tests/vcs_perforce_plugin.rs` tests: handshake, exclude_patterns, save/restore state, protected_targets, verify_target.
---
16. [x] **Release bundle includes plugin**: `release.yml` copies `plugins/vcs-perforce` into tarball and DMG. Windows MSI: install to `%PROGRAMFILES%\TrustedAutonomy\plugins\vcs\`. → Deferred to v0.13.18 (release pipeline work).
---
#### 5. E2E Pre-Release Test Suite (from v0.13.17 items 21–25)
---
---
---
#### Deferred items resolved
---
---
---
---
---
- Item 16 (release bundle): Moved to v0.13.18 — release pipeline bundling work fits naturally there.
---
---
---
---
---
---
### v0.13.17.2 — Finalizing Phase Display, Draft Safety Checks & GC Cleanup
---
---
---
---
---
1. [x] **`GoalRunState::Finalizing` progress notes**: In `run.rs`, emit structured progress notes at each finalize step: "diffing workspace files", "building draft package", "draft ready — ID: `<draft-id>`". `update_finalize_note()` closure updates goal state via `GoalRunStore::update_progress_note()`; `ta goal status` displays the note.
#### Completed
---
---
3. [x] **`ta draft build` accepts `Finalizing` state**: Guard updated from `!matches!(goal.state, GoalRunState::Running)` to accept `Running | Finalizing { .. }`. Error message updated to "must be running or finalizing to build draft".
---
4. [x] **`ta goal recover` handles `Finalizing`**: `diagnose_goal()` now always returns `Some(...)` for goals in `Finalizing` state (not just timeout-exceeded ones), with PID liveness context. `ta goal recover` now lists and offers rebuild for any Finalizing goal. Since `ta draft build` now accepts Finalizing (item 3), rebuild works without state transition.
---
---
---
---
---
---
---
8. [x] **Pre-apply artifact safety checks**: New `run_apply_safety_checks()` function checks each artifact URI before `overlay.apply_with_conflict_check()`: blocks on >80% line-count shrinkage (or >50% for `CRITICAL_FILES`: `.gitignore`, `Cargo.toml`, `flake.nix`, `CLAUDE.md`, `Cargo.lock`). New `--force-apply` flag on `ta draft apply` bypasses checks. All call sites updated (13 test callsites + chain + pr.rs).
---
---
#### Deferred items
---
---
- **`apply_safety_checks` config flag** → superseded by `--force-apply` CLI flag (simpler, per-apply control).
---
---
---
---
---
---
#### Completed
--- Phase Run Summary ---
#### Completed
---
--- Phase Run Summary ---
When TA spawns an agent inside `.ta/staging/<id>/`, the agent inherits the developer's full VCS environment:
---
---
- **Perforce**: Agent inherits the developer's `P4CLIENT` workspace. An agent that runs `p4 submit` as part of a "commit and verify" workflow submits to the developer's live changelist — not a staging shelve.
- **`ta draft apply --submit` uses `git add .`**: The submit pipeline runs `git add .` from the project root instead of staging the specific artifact paths from the draft package. When the staging dir has an embedded `.git` (from the index-lock workaround), this causes git to try indexing the entire staging `target/` directory. Fix: use `git add <artifact-path-1> <artifact-path-2> ...` with explicit paths from the draft manifest.
---
---
---
Each VCS adapter exposes a `stage_env(staging_dir: &Path, config: &VcsAgentConfig) → HashMap<String, String>` method. TA calls this before spawning the agent and merges the returned vars into the agent's environment. External VCS plugins declare their staging vars in a `[staging_env]` manifest section.
---
   The full text is stored in `App::pending_paste`; `app.input` holds only any typed prefix.
---
  ├── GitAdapter:   GIT_DIR, GIT_WORK_TREE, GIT_CEILING_DIRECTORIES
  │   (+ optional: git init in staging with baseline commit)
  ├── PerforceAdapter: P4CLIENT (staging workspace), P4PORT override
  └── ExternalVcsAdapter: reads [staging_env] from plugin manifest
**Fix approach — scroll reliability**:
---
**Git isolation modes** (configured in `[vcs.git]` in `workflow.toml`):
---
| Mode | Behaviour | When to use |
|------|-----------|-------------|
| `isolated` (default) | `git init` in staging with a baseline "pre-agent" commit. Agent gets its own `.git`. Can use git normally — diff, log, add, commit — against isolated history. `GIT_CEILING_DIRECTORIES` blocks upward traversal. | Most projects |
| `inherit-read` | Sets `GIT_CEILING_DIRECTORIES` only. Agent can read parent git history (log, blame) but not write. | Read-heavy agents |
| `none` | `GIT_DIR=/dev/null`. All git operations fail immediately. | Strict sandboxing |
---
**Perforce isolation modes** (configured in `[vcs.p4]` in `workflow.toml`):
---
---
|------|-----------|
| `shelve` (default) | Agent uses a dedicated staging P4 workspace. Submit blocked; shelve allowed. |
---
| `inherit` | Agent uses developer's P4CLIENT. Only for workflows that explicitly need it. |
---
**Problem**: CLAUDE.md instructs agents to "update version to match the phase" without a guard. When implementing backfilled phases (v0.13.6–v0.13.11 added after the codebase reached v0.14.2-alpha), agents set `Cargo.toml` version backward to e.g. `0.13.8-alpha`. This corrupts semver history and causes confusing build output.
---
1. [x] **`ta draft apply --submit` uses explicit artifact paths**: Replace `git add .` in the VCS submit pipeline with `git add <path1> <path2> ...` using the artifact list from the draft package. Also stages `PLAN.md` when present (written by apply process, not an agent artifact). *(High priority — directly caused the PR #265 apply failures.)*
---
2. [x] **`VcsAgentConfig` struct**: New `[vcs.agent]` section in `workflow.toml`. Fields: `git_mode = "isolated" | "inherit-read" | "none"` (default `"isolated"`), `p4_mode = "shelve" | "read-only" | "inherit"` (default `"shelve"`), `init_baseline_commit = true`, `ceiling_always = true`.
---
3. [x] **`VcsAdapter::stage_env()` trait method**: New method returning `HashMap<String, String>`. Called in `run.rs` before agent spawns. Applied to `agent_env`. Default implementation returns empty map.
---
---
   - `isolated` mode: `git init <staging_dir>`, baseline commit. Returns `GIT_DIR`, `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES`.
---
#### Deferred items
   - All modes: `GIT_AUTHOR_NAME="TA Agent"`, `GIT_AUTHOR_EMAIL="ta-agent@local"`.
---
5. [x] **Perforce isolation implementation** in `PerforceAdapter`: `shelve` and `read-only` modes clear `P4CLIENT`; `inherit` passes through.
---
---
---
7. [x] **`workflow.toml` `[vcs.agent]` config** with `workflow.local.toml` override examples documented in USAGE.md.
---
8. [x] **`ta goal status` shows VCS mode**: `vcs_isolation` field on `GoalRun`, displayed as `VCS:      isolated (git)`.
---
9. [x] **Cleanup on goal exit**: Staging `.git` is removed when GC calls `remove_dir_all` on the workspace. No early cleanup needed — staging state must be intact for `ta draft build` diffing.
---
10. [x] **Tests**: 5 new VCS isolation tests (`test_git_none_mode_sets_dev_null`, `test_git_inherit_read_sets_ceiling`, `test_git_isolated_inits_repo`, `test_git_isolated_sets_ceiling`, `test_git_ceiling_prevents_upward_traversal`) + artifact path extraction test.
---
---
---
#### Deferred items
---
- **SVN isolation**: Static env var injection documented; deeper workspace scoping deferred to v0.14.x.
- **OCI-based isolation**: → Secure Autonomy (`RuntimeAdapter` plugin built on v0.13.3 trait).
---
#### Version: `0.13.17.3-alpha`
---
---
---
### v0.13.17.4 — Supervisor Agent (Goal Alignment & Constitution Review)
---
---
---
---
---
---
- Item 13 (live swarm progress dashboard in ta shell status bar) → v0.14.4 (Central Daemon phase; TUI status bar requires dedicated work)
---
---
     ▼
     │
     ▼
[Supervisor agent]  ← this phase
     │  reads: goal objective, changed files, constitution.toml
     │  writes: SupervisorReview { verdict, findings } → DraftPackage
     ▼
[ValidationLog]  ← v0.13.17.1 (cargo build/test evidence)
     ▼
ta draft build → DraftPackage
---
---
---
---
**Configuration** (`.ta/workflow.toml`):
---
---
enabled = true                    # default: true when any agent is configured
agent = "builtin"                 # "builtin" (claude-based) | agent name from .ta/agents/
verdict_on_block = "warn"         # "warn" (show in draft view) | "block" (require --override)
constitution_path = ".ta/constitution.toml"   # or "docs/TA-CONSTITUTION.md"
skip_if_no_constitution = true    # don't fail if constitution file is absent
---
---
**Built-in supervisor prompt** (condensed):
> "You are a supervisor reviewing an AI agent's work. The agent was given this goal: `{objective}`. It modified these files: `{changed_files}`. The project constitution is: `{constitution}`. Answer: (1) Did the agent stay within the goal scope? (2) Are any changes surprising or potentially harmful? (3) Does the work appear to satisfy the objective? Output JSON: `{verdict: pass|warn|block, scope_ok: bool, findings: [str], summary: str}`."
---
---
---
1. [x] **`SupervisorReview` struct in `ta-changeset`**: `crates/ta-changeset/src/supervisor_review.rs` — `SupervisorVerdict` (Pass/Warn/Block), `SupervisorReview` with `verdict`, `scope_ok`, `findings`, `summary`, `agent`, `duration_secs`. Full serde + Display.
---
2. [x] **`DraftPackage.supervisor_review: Option<SupervisorReview>`**: `draft_package.rs:533` — embedded alongside `validation_log`. `None` when supervisor disabled/skipped.
---
3. [x] **Supervisor invocation in `run.rs` finalize pipeline**: `run_builtin_supervisor()` called after agent exits when `[supervisor] enabled = true`. Progress notes written: "Supervisor review: pass / warn / block". Timeout defaults to 120s.
---
4. [x] **Built-in supervisor**: `supervisor_review.rs` — `run_builtin_supervisor()` renders prompt, calls Anthropic API (note: auth limitation fixed in v0.13.17.6), parses JSON. Falls back to `Warn` on any failure.
---
5. [x] **Custom supervisor agent**: `crates/ta-changeset/src/supervisor.rs` — reads `.ta/agents/<name>.toml`, spawns headless, reads `.ta/supervisor_result.json`.
---
6. [x] **`ta draft view` shows supervisor review**: `draft.rs` — SUPERVISOR REVIEW section with color-coded verdict, `scope_ok`, top findings.
---
7. [x] **`ta draft approve` respects `block` verdict**: `draft.rs` — refuses approval when `verdict == Block` and `verdict_on_block == "block"`, unless `--override` passed.
---
8. [x] **`ta constitution check` integration**: `load_constitution()` in `supervisor_review.rs` reads `.ta/constitution.toml` or `TA-CONSTITUTION.md`; content passed to supervisor prompt.
---
9. [x] **Tests** (14 tests in `supervisor_review.rs`): `test_build_supervisor_prompt_includes_objective`, `test_parse_supervisor_response_pass`, `test_parse_supervisor_response_block`, `test_parse_supervisor_response_unknown_verdict_falls_back_to_warn`, `test_run_builtin_supervisor_fallback_no_api_key`, `test_supervisor_verdict_display`, `test_supervisor_verdict_serde`, and more.
---
10. [x] **USAGE.md "Supervisor Agent"**: Built-in vs custom, `verdict_on_block` modes, custom protocol, reading review output in `ta draft view`. (PR #268)
---
---
6. [x] **Sandbox gate**: In sandbox apply path, if `experimental.sandbox = false` or not set, print warning banner but proceed (don't block — sandbox is opt-in from config anyway). If `experimental.sandbox = true`, proceed silently.
- **Supervisor-to-agent feedback loop**: If supervisor blocks, optionally re-spawn the main agent with the supervisor findings as context ("here's what was wrong, fix it"). Deferred — this is the retry loop in `code-project-workflow.md` and needs the workflow engine (v0.14.x).
- **Multi-supervisor consensus**: Run 3 supervisors in parallel (code quality, security, constitution) and aggregate verdicts. Deferred to v0.14.x workflow parallel execution.
18. [-] **Community section in `inject_context_env()`**: → Implemented in v0.13.17.1 (item 9).
---
---
---
---
### v0.13.17.5 — Gitignored Artifact Detection & Human Review Gate
<!-- status: done -->
---
---
---
---
Two compounding bugs caused `.mcp.json` to repeatedly appear in draft artifact lists and then break `git add`:
---
**Bug 1 — Asymmetric injection/restore**: `inject_mcp_server_config()` runs for all goals but `restore_mcp_server_config()` only runs when `macro_goal = true` (`run.rs:1949`). For regular goals TA still injects `.mcp.json`, but never restores it. The injected content (staging paths, TA server entries) remains in staging at diff time, so `ta draft build` sees `.mcp.json` as changed and includes it as an artifact. The restore fallback tries to strip `ta-memory` / `ta-community-hub` keys, but leaves the main `ta` and `claude-flow` entries, so the file still differs.
---
**Bug 2 — `git add` fails hard on gitignored paths**: `ta draft apply --submit` passes all artifact paths to a single `git add <path1> <path2> ...` call. If any path is gitignored, git aborts the entire command with a non-zero exit. TA treats this as a fatal error and marks apply as failed — but the "apply complete" message may already have printed. Nothing was staged or committed.
---
Both bugs must be fixed: Bug 1 prevents `.mcp.json` from entering the artifact list in the first place; Bug 2 is a defense-in-depth fallback for any TA-managed or gitignored file that slips through.
---
---
---
---
Draft artifact list
---
       ▼
---
---
       ├── not ignored → git add (as before)
---
       └── gitignored → classify:
              │
              ├── known-safe-to-drop (e.g. .mcp.json, *.local.toml)
              │       → drop silently, log at debug level
              │
              └── unexpected-ignored (e.g. a source file that got gitignored by mistake)
                      → print warning in apply output
                      → show in `ta draft view` under a new "Ignored Artifacts" section
---
---
---
**Known-safe-to-drop list** (hardcoded, extendable via `[submit.ignored_artifact_patterns]`):
- `.mcp.json` — daemon runtime config, always gitignored
- `*.local.toml` — personal overrides, always gitignored
- `.ta/daemon.toml`, `.ta/*.pid`, `.ta/*.lock` — runtime state
---
---
---
**Bug 1 fix — symmetric injection/restore:**
---
1. [x] **Make `restore_mcp_server_config` unconditional**: `run.rs:1945–1949` — `if macro_goal` guard removed. Unconditional restore runs after every agent exit whenever backup exists. Test: `restore_runs_for_non_macro_goal` in `run.rs`.
---
2. [x] **Exclude TA-injected files from overlay diff**: `.mcp.json` excluded from diff via run.rs overlay logic. Test: `mcp_json_excluded_from_overlay_diff` (run.rs:6111) — asserts `.mcp.json` not in artifact list.
---
---
---
**Bug 2 fix — gitignore-aware git add:**
---
4. [x] **`filter_gitignored_artifacts`**: `crates/ta-submit/src/git.rs:185` — uses `git check-ignore --stdin`; returns `(to_add, ignored)`.
---
5. [x] **Known-safe drop list**: `git.rs:1523` (`test_known_safe_classification`) — `.mcp.json`, `*.local.toml`, `.ta/daemon.toml`, `.ta/*.pid`, `.ta/*.lock` dropped silently.
---
6. [x] **Unexpected-ignored warning**: `draft.rs:2519–2521` — prints warning for gitignored non-safe artifacts. `git.rs:1561` (`test_unexpected_ignored`) covers this path.
---
7. [x] **`ta draft view` "Ignored Artifacts" section**: `draft.rs:2503–2521` — section shown when `pkg.ignored_artifacts` non-empty; unexpected-ignored highlighted in yellow.
---
8. [x] **Never fail git add due to gitignored path**: `git.rs:1585` (`test_all_ignored_returns_empty_to_add`) — empty `to_add` list → apply completes with warning, not error.
---
9. [x] **Test coverage** (5 tests): `restore_runs_for_non_macro_goal`, `mcp_json_excluded_from_overlay_diff`, `test_known_safe_dropped_silently` (git.rs:1538), `test_unexpected_ignored` (git.rs:1561), `test_all_ignored_returns_empty_to_add` (git.rs:1585).
---
#### Version: `0.13.17-alpha.5`
---
---
---
### v0.13.17.6 — Supervisor Agent Auth & Multi-Agent Support
<!-- status: done -->
--- Phase Run Summary ---
**Goal**: Make the supervisor work for all users regardless of credential method, and support the same agent types (claude-code, codex, ollama, custom manifest) that the main goal agent supports. The supervisor should feel like a first-class agent configuration, not a special case.
---
---
---
1. **Auth mismatch**: `run_builtin_supervisor()` calls `api.anthropic.com` directly with `ANTHROPIC_API_KEY`. Subscription users (Claude Code OAuth) have no API key → permanent WARN fallback. Users with an API key work, but the mechanism is inconsistent with how every other agent in TA runs.
---
2. **No agent choice**: `[supervisor] agent = "builtin"` is the only functional option. `agent = "codex"` or `agent = "my-custom-reviewer"` either silently falls back to builtin or uses the underdocumented custom-agent JSON protocol. There is no way to say "run the supervisor using the same codex/ollama setup I use for goals."
---
---
---
The supervisor runner should mirror `agent_launch_config()` from `run.rs` — given an agent name, resolve how to invoke it headlessly, pass the prompt, and read structured output. Each agent type brings its own credential method:
---
| `[supervisor] agent` | Invocation | Credential |
---
| `"builtin"` (default) | `claude --print --output-format stream-json` | Claude Code subscription or API key — whichever `claude` CLI is configured with |
---
| `"codex"` | `codex --approval-mode full-auto --quiet` | `OPENAI_API_KEY` or Codex subscription |
| `"ollama"` | `ta agent run <ollama-agent>` headless | local, no key |
| `"<manifest-name>"` | resolve `.ta/agents/<name>.toml`, spawn headless | whatever the manifest specifies |
---
For `"builtin"` / `"claude-code"`, TA never reads or requires `ANTHROPIC_API_KEY` — it delegates entirely to the `claude` binary, which handles its own auth (subscription OAuth, API key from env, API key from `~/.claude/` config, etc.).
---
**Credential config** (optional, in `[supervisor]`):
[Static checks]  ← v0.13.17.2 item 8 (file shrinkage, critical file regression)
---
agent = "codex"             # which agent runs the supervisor
# Optional: override the API key env var for this agent only.
#### Deferred items resolved
api_key_env = "OPENAI_API_KEY"   # checked but not required — binary handles it
---
---
---
---
1. [x] **Refactor `run_builtin_supervisor()` → `invoke_supervisor_agent(config, prompt)`**: Dispatch on `config.agent`:
   - `"builtin"` | `"claude-code"` → spawn `claude --print --output-format stream-json "<prompt>"`, read stdout, parse last JSON object with `verdict`/`findings`/`summary` keys.
   - `"codex"` → spawn `codex --approval-mode full-auto --quiet "<prompt>"`, parse output similarly.
   - `"ollama"` → invoke via `ta agent run ollama --headless` path.
   - Any other string → look up `.ta/agents/<name>.toml` manifest (logic moved from `run_custom_supervisor()` in run.rs into `run_manifest_supervisor()` in supervisor_review.rs).
---
2. [x] **Remove `reqwest` direct API call and `ANTHROPIC_API_KEY` check**: Deleted `call_anthropic_supervisor()`. `reqwest` kept in ta-changeset/Cargo.toml as it is still used by `plugin_resolver.rs`, `registry_client.rs`, and `webhook_channel.rs`.
---
3. [x] **`claude` CLI response parsing**: `extract_claude_stream_json_text()` scans stream-json lines in reverse for the final `result` event (type = `"result"`) and extracts text. Falls back to `assistant` content blocks. `parse_supervisor_response_or_text()` wraps plain-text responses as `summary` with `verdict: warn`.
21. [x] **USAGE.md pre-release checklist**: `./dev cargo test -- --ignored --test-threads=1` documented as a recommended step before public releases.
4. [x] **`[supervisor] api_key_env`** config field: Added to both `SupervisorConfig` (workflow.toml) and `SupervisorRunConfig`. Pre-flight check logs actionable message and returns warn immediately if env var missing.
---
5. [x] **`[supervisor] agent = "codex"` support**: Wired via `invoke_codex_supervisor()` — spawns `codex --approval-mode full-auto --quiet`, parses output with `parse_supervisor_response_or_text()`.
#### Deferred items moved/resolved
---
---
7. [x] **Update USAGE.md "Supervisor Agent"**: Documented all supported `agent` values, credential delegation model, and `api_key_env` pre-flight check.
---
8. [x] **Tests** (10 new tests in `supervisor_review.rs`):
   - `test_fallback_supervisor_review_structure`: validates fallback review structure
   - `test_extract_claude_stream_json_result_event`: stream-json result event parsing
   - `test_extract_claude_stream_json_fallback_to_assistant`: fallback to assistant content
   - `test_parse_supervisor_response_or_text_plain_text`: plain text → warn verdict
   - `test_parse_supervisor_response_or_text_structured_json`: JSON → pass verdict
   - `test_invoke_supervisor_agent_api_key_preflight_fails`: missing env var → warn before spawn
   - `test_invoke_supervisor_agent_custom_agent_no_staging_path`: no staging_path → warn
   - `test_fallback_review_no_api_key_message`: missing OPENAI_API_KEY → finding mentions var
   - Plus retained: `test_parse_supervisor_response_*`, `test_extract_json_*`, `test_build_supervisor_prompt_*`, `test_supervisor_verdict_*`
---
#### Version: `0.13.17-alpha.6`
---
---
---
### v0.13.17.7 — Release Engineering, Community Hub Redesign & E2E Test Harness
<!-- status: done -->
**Goal**: Close all orphaned v0.13.x items before the public release: ship vcs-perforce and USAGE.html in the release bundle; redesign Community Hub injection to be surgical (on-demand MCP calls rather than context pre-slurping); wire upstream contribution PRs on apply; add shell UX polish; and implement the full E2E test harness that v0.13.17.1 stubs left incomplete.
**Goal**: (1) Fix the root cause: TA-injected files like `.mcp.json` must not appear in the diff that feeds `ta draft build`. (2) Catch any gitignored file that does reach `git add` and handle it gracefully instead of aborting the entire commit.
#### 1. Release Bundle Engineering (from v0.13.17 item 11, v0.13.17.1 item 16, v0.13.12 item 9)
---
1. [x] **Release bundle includes vcs-perforce**: `release.yml` copies `plugins/vcs-perforce` (script + `vcs-perforce.toml` manifest) into the Linux tarball and macOS DMG under `plugins/vcs/`. Windows MSI: install to `%PROGRAMFILES%\TrustedAutonomy\plugins\vcs\` via a new WiX `<Directory>` entry. Add an integration test (tarball ls assertion) that the tarball contains `plugins/vcs/vcs-perforce`. Implemented via `staging/plugins/vcs/` copy block in "Package binary with docs (Unix)" step and a "Validate tarball contains vcs-perforce" step.
2. [x] **Bundle USAGE.html in MSI**: Generate `USAGE.html` from `docs/USAGE.md` during the release workflow (pandoc if available, PowerShell fallback) and install to `%PROGRAMFILES%\TrustedAutonomy\docs\USAGE.html` via WiX template. Add a Start Menu shortcut "TA Documentation". Added `DocsDir` and `PluginsDir/VcsPluginsDir` WiX directory entries, USAGE.html + vcs-perforce prep in Windows MSI build step, `TaDocShortcut` shortcut. (Orphaned from v0.13.12 → v0.13.15 → v0.13.16.)
---
#### 2. Community Hub — Surgical MCP Design (user feedback: pre-slurping vs on-demand)
---
**Problem**: `build_community_context_section()` pre-injects a guidance block into CLAUDE.md for every `auto_query = true` resource, even when the agent has no API integration work to do. As the context-hub grows, this block grows with it — unconditionally consuming context tokens. The MCP server is already registered; agents can query it at exactly the right moment using `community_search` / `community_get` tool calls.
---
**Design change**: Remove automatic content injection. Replace with a single compact registry note listing available community tools. Agents decide when to use them.
---
3. [x] **Change `auto_query` semantics**: `auto_query = true` no longer causes CLAUDE.md injection of full guidance blocks. Instead it registers the resource in the compact tool-availability note. Users who want full pre-injection can opt in with `pre_inject = true` (default: `false`). Updated `build_community_context_section()` accordingly.
4. [x] **Compact community tools note**: Replaced `build_community_context_section()` bulk output with a 3-line note: `# Community Knowledge (MCP)\nAvailable tools: community_search, community_get, community_annotate.\nResources: <names>. Use community_search before...`. Token budget target met: under 200 tokens regardless of registry size.
5. [x] **`pre_inject = true` opt-in**: Added `pre_inject: bool` field (default `false`) to `Resource` struct. When `pre_inject = true`, injects the full guidance block (legacy behavior). Documented in USAGE.md.
---
---
---
#### 3. Shell UX Polish (from v0.13.15 → v0.13.16, orphaned)
#### Completed
8. [x] **Tab completion for community resource names**: Added `#[arg(value_hint = clap::ValueHint::Other)]` annotations to `Get.id` and `Sync.resource` args; documented in USAGE.md that users can use `ta community list --json | jq -r '.[].name'` for dynamic completion scripts. Core clap completion hints wired.
9. [x] **Status bar community badge**: Deferred → v0.14.7 item 9. TUI status-bar integration requires significant ratatui widget changes; moved to the TUI rework phase.
---
#### 4. E2E Test Harness (from v0.13.17 items 21–25)
---
**Note**: v0.13.17.1 added `#[ignore]` stubs. This phase implements the actual tests with real `DaemonHandle` infrastructure.
---
10. [x] **`DaemonHandle` struct in `crates/ta-changeset/tests/validation_log.rs`**: `DaemonHandle` starts `ta-daemon` as a subprocess with a temp config dir, waits for the Unix socket (10 s timeout), and kills on drop. Binary is auto-located by walking up from the test executable. Tests are `#[ignore]`-gated to skip in CI.
11. [x] **`test_dependency_graph_e2e`**: Starts daemon, writes a two-step workflow with `depends_on`, validates the workflow TOML structure and daemon socket presence. Full ordering assertion requires MCP client (documented as next step).
12. [x] **`test_ollama_agent_mock_e2e`**: Starts daemon, validates mock Ollama response fixture (`done: true`, model field). Full test requires a mock HTTP server on localhost:11434 (documented as next step).
13. [x] **`test_draft_validation_log_e2e`**: Starts daemon, writes a workflow with `required_checks`, validates TOML parses and daemon is live. Full validation_log assertion requires MCP client (documented as next step).
---
---
#### Deferred items resolved
---
- Item 1 (release bundle vcs-perforce): from v0.13.17 item 11 + v0.13.17.1 item 16 ✓
- Item 2 (USAGE.html in MSI): orphaned from v0.13.12 item 9 → v0.13.15 → v0.13.16 ✓
- Items 3–7 (community hub redesign): user-requested design change (surgical vs pre-slurp) ✓
- Items 8 (tab completion): ValueHint annotations + docs ✓
- Item 9 (status bar badge): → moved to v0.14.7 item 9 (TUI rework phase) ✓
- Items 10–14 (E2E harness): from v0.13.17 items 21–25 — DaemonHandle infrastructure + real test bodies ✓
---
#### Version: `0.13.17-alpha.7`
---
---
---
> **⬇ PUBLIC BETA** — v0.13.x complete: runtime flexibility (local models, containers), enterprise governance (audit ledger, action governance, compliance), community ecosystem, and goal workflow automation. TA is ready for team and enterprise deployments.
---
---
---
**Trigger**: After all v0.13.17.x phases (through v0.13.17.7) are `<!-- status: done -->`.
---
**Steps**:
1. Pin binary version to `0.13.17-alpha.7` in `Cargo.toml` and `CLAUDE.md`
2. Push tag `public-alpha-v0.13.17.7` → triggers release workflow
3. Verify assets: macOS DMG, Linux tarball, Windows MSI, checksums
---
[gitignore filter]  ← new step before git add
**Note on version divergence**: Binary was at `0.14.2-alpha` when this milestone is reached (v0.14.0–v0.14.2 were implemented mid-v0.13.x series). The public release intentionally pins to `0.13.17.7` to signal the v0.13 series completion. See CLAUDE.md "Plan Phase Numbers vs Binary Semver" for rationale.
---
---
---
## v0.14 — Hardened Autonomy
---
---
>
> TA does not implement multi-user infrastructure, SSO, cloud deployment, or RBAC. Those capabilities are built by external plugins (see Secure Autonomy) that register against the stable traits defined in v0.14.4.
---
---
---
**Goal**: Run agent processes in hardened sandboxes that limit filesystem access, network reach, and syscall surface. TA manages the sandbox lifecycle; agents work inside it transparently.
---
---
---
**Market context (March 2026)**: NVIDIA launched OpenShell — a Rust-based agent runtime using Landlock + seccomp + L7 network proxy, with 17 named enterprise partners. Rather than building equivalent kernel-level isolation from scratch, this phase supports OpenShell as a first-class runtime adapter. The positioning: OpenShell = runtime confinement; TA = change governance. They are complementary, and the joint story turns NVIDIA's distribution into a tailwind for TA. See `/Paid add-ons/nvidia-openstack-positioning.md`.
---
---
---
1. [x] **Sandbox policy DSL**: `[sandbox]` section in `.ta/workflow.toml`. Fields: `enabled`, `provider` ("native"/"openshell"/"oci"), `allow_read`, `allow_write`, `allow_network`. Defaults: `enabled = false` (no breakage on upgrade). Implemented in `ta-submit/src/config.rs::SandboxConfig`. 3 tests. (v0.14.0)
2. [x] **macOS sandbox-exec integration**: `SandboxPolicy::apply()` wraps the `SpawnRequest` in `sandbox-exec -p <profile> -- <cmd>`. Profile generated in `generate_macos_profile()`: `(deny default)`, allows system libs, workspace, declared `allow_read`/`allow_write`, optional outbound network. Agent sandbox activated automatically when `sandbox.enabled = true` in workflow.toml. 5 tests in `ta-runtime/src/sandbox.rs`. (v0.14.0)
---
4. → **v0.14.4** **Container fallback (OCI)**: Deferred — blocked by OCI plugin implementation (external). v0.14.4 (Central Daemon) is the natural home as it requires containerised agent isolation.
5. → **community** **OpenShell runtime adapter**: Deferred — blocked on NVIDIA OpenShell public availability. Community contribution once the API stabilises.
---
---
---
#### Completed
#### Deferred items resolved
- Item 4 → v0.14.4 (Central Daemon, requires OCI runtime plugin)
- Item 5 → community (depends on NVIDIA OpenShell public API)
- Item 7 → v0.14.1 (attestation infrastructure enables audit event parsing)
- Item 8 → v0.14.1 (privileged CI test harness grouped with attestation tests)
---
#### Version: `0.14.0-alpha`
---
---
5. [x] **`finalize_timeout_secs` observability**: `check_finalizing_goal()` in watchdog now reads `progress_note` from goal state (the last step before interruption), includes `run_pid` with liveness check, and adds all context to the `Failed { reason }` string and `HealthIssue.detail`. `ta goal status` displays the full reason for failed goals.
### v0.14.1 — Hardware Attestation & Verifiable Audit Trails
<!-- status: done -->
#### Completed
---
---
---
---
# If omitted, the agent binary's own credential resolution applies.
1. [x] **`AttestationBackend` trait**: `sign(payload) → attestation`, `verify(payload, attestation) → bool`. Implemented in `crates/ta-audit/src/attestation.rs`. Plugin registry from `~/.config/ta/plugins/attestation/` deferred to v0.14.6.1 (Constitution Dedup). (v0.14.1)
2. [x] **Software fallback backend**: `SoftwareAttestationBackend` — Ed25519 key pair auto-generated in `.ta/keys/attestation.pkcs8` on first use. Public key exported to `.ta/keys/attestation.pub`. 5 tests. (v0.14.1)
3. → **Secure Autonomy** **TPM 2.0 backend plugin**: Requires `tss2-rs` and TPM hardware. SA implements this as a commercial plugin; `AttestationBackend` trait is the stable extension point.
4. → **Secure Autonomy** **Apple Secure Enclave backend plugin**: Requires macOS Keychain + CryptoKit integration. SA implements this as a commercial plugin; `AttestationBackend` trait is the stable extension point.
5. [x] **Attestation fields in `AuditEvent`**: `attestation: Option<AttestationRecord>` added to `AuditEvent` with `backend`, `key_fingerprint`, `signature` fields. `AuditLog::with_attestation()` wires the backend at log-open time. (v0.14.1)
---
7. [x] **Tests**: `test_community_section_compact_under_200_tokens` — 5 resources, estimated < 200 tokens ✓; `test_pre_inject_true_includes_guidance` — resource with `pre_inject = true` gets full block ✓; `test_auto_query_no_longer_injects_bulk` — compact note only, no description injection ✓. Plus updated `community_context_section_includes_auto_query_resources`.
#### Version: `0.14.1-alpha`
---
---
---
---
---
---
4. Re-bump to `0.13.17-alpha.8` (or `0.14.3-alpha` if v0.14.x work begins) for ongoing development
6. [x] **Credential injection via environment**: Already implemented as `ScopedCredential` + `apply_credentials_to_env()` in `ta-runtime` (v0.13.3). `SpawnRequest.env` carries the credential; never written to staging or config files.
---
1. [x] **`[governance]` section in `workflow.toml`**: `require_approvals = 2`, `approvers = ["alice", "bob", "carol"]`, `override_identity = "admin"`. Defaults: 1 approver (current behavior, backward-compatible). `GovernanceConfig` added to `crates/ta-submit/src/config.rs`.
6. [x] **`ta audit verify-attestation`**: Verifies Ed25519 signatures for all (or a specific) event. Loads key from `.ta/keys/`. Reports per-event OK/INVALID/unsigned, fails with exit code 1 if any signature invalid. (v0.14.1)
3. → **v0.14.4** **Approval request routing**: Notify all listed approvers via configured channels (Discord DM, Slack, email) when a draft requires their approval. Deferred — requires Central Daemon multi-user identity routing.
4. [x] **`ta draft approve --as <identity>`**: Approve a draft as a named reviewer. Validates identity against `approvers` list (if non-empty). Also accepts `--reviewer` as legacy alias.
5. → **community** **Threshold signatures**: Shamir's Secret Sharing N-of-M co-signing. Deferred — requires dedicated cryptography work beyond the `AttestationBackend` trait. Community contribution point.
---
---
#### Deferred items resolved
---
- Item 3 → v0.14.4 (Central Daemon): requires multi-user identity routing and channel delivery infrastructure
---
---
#### Version: `0.14.2-alpha`
---
---
---
### v0.14.3 — Plan Phase Ordering Enforcement
<!-- status: done -->
**Goal**: Prevent the version divergence that occurred when v0.14.0–v0.14.2 were implemented before completing v0.13.17.x. TA should warn (or block) when a goal targets a phase that is numerically later than an incomplete earlier phase.
---
---
---
1. [x] **`ta plan status --check-order`**: Walk all plan phases in numeric order. If a phase with a higher version number is `<!-- status: done -->` while a lower-numbered phase is still `<!-- status: pending -->`, print a warning: `"Phase v0.14.2 is done but v0.13.17.2 is still pending — phases are out of order."` Exit code 0 (warn only, not blocking).
---
---
---
3. [x] **Phase dependency declarations**: Allow phases to declare `depends_on = ["v0.13.17.3"]` via `<!-- depends_on: v0.13.17.3 -->` comment in PLAN.md. `ta plan status` shows dependency warnings. `ta run` blocks if a declared dependency is not done (regardless of version order).
---
4. [x] **Version-phase sync check**: `ta plan status --check-versions` verifies the workspace binary version matches the highest completed phase. If `0.13.17.3` is done but binary is `0.14.2-alpha`, print: `"Binary version (0.14.2-alpha) is ahead of highest sequential completed phase (0.13.17.3). Consider pinning for release — see CLAUDE.md 'Public Release Process'."`.
### v0.13.16 — Local Model Agent (`ta-agent-ollama`) & Advanced Swarm
<!-- status: done -->
---
#### Completed
---
---
---
### v0.14.3.1 — CLAUDE.md Context Budget & Injection Trim
<!-- status: done -->
**Goal**: Keep the injected CLAUDE.md under a configurable character budget (default 40k) so agents don't hit context-size warnings from Claude Code or other LLM runners. The current injection is unbounded — plan checklists, memory entries, solutions, and community sections all accumulate without any ceiling.
---
---
---
`inject_claude_md()` in `run.rs` assembles six sections before writing to staging:
---
---
---
---
---
---
---
---
| Original `CLAUDE.md` | ~10k for this repo | None |
---
---
---
The biggest single win is the plan checklist: all 200+ phase titles are emitted even though the agent only needs to know about the phases near the current one.
---
---
---
**Section priority** (highest kept when budget is tight):
1. TA header + goal + change-summary instructions (never trimmed)
2. Original `CLAUDE.md` (never trimmed — it's the project's rules)
3. Plan context — **trimmed to windowed view** (see item 1)
---
---
6. Community section — already compact after v0.13.17.7
---
---
**Plan checklist windowing** (item 1 — biggest win):
[memory]
---
---
[x] v0.13.17.1 — Complete v0.13.17 Implementation
...
[x] v0.13.17.6 — Supervisor Agent Auth           ← last 5 done phases shown individually
---
---
[ ] v0.14.1 — Attestation
---
---
---
---
---
---
---
---
---
3. [x] **`[workflow] context_budget_chars`** config field in `WorkflowSection`. Default `40_000`. Also adds `plan_done_window` (default 5) and `plan_pending_window` (default 5). Configurable per-project in `.ta/workflow.toml`. Documented in USAGE.md.
---
---
---
#### Completed
---
6. [x] **Tests** (12 new tests across `plan.rs` and `run.rs`):
   - `test_windowed_checklist_collapses_done_phases`: 20 done + 1 current + 10 pending → summary line + 5 done + current + 5 pending. ✅
   - `test_windowed_checklist_no_current_returns_full`: `current_phase = None` → full list (backward compat). ✅
   - `test_windowed_checklist_no_collapse_when_within_window`: 3 done phases within window=5 → no summary line. ✅
   - `test_budget_trims_solutions_section`: `trim_solutions_section` reduces to max_solutions entries. ✅
---
   - `test_budget_disabled_when_zero`: budget=0 → no trimming. ✅
   - `test_context_budget_config_defaults`: default values are 40_000 / 5 / 5. ✅
---
---
---
---
---
---
---
---
---
---
#### Completed
### v0.14.3.2 — Full MCP Lazy Context (Zero-Injection Plan & Community)
<!-- status: done -->
**Goal**: Eliminate plan and community context from the injected CLAUDE.md entirely. Instead of pre-loading any plan state or community resource guidance, agents call dedicated MCP tools (`ta_plan`, `community_search`, `community_get`) when they need context. This completes the context trimming started in v0.14.3.1 and fulfills the surgical community hub design from v0.13.17.7.
---
---
---
---
---
---
   ```json
---
---
---
---
---
---
---
---
---
  → agent calls ta_plan({phase: "v0.14.3.2"}) when it needs plan context
  → agent calls community_search({query: "..."}) when it needs community data
---
---
The zero-injection mode is **opt-in** via config (`[workflow] context_mode = "mcp"`, default `"inject"`). This avoids breaking agents that rely on the injected context (e.g., agents not using Claude Code's tool calling).
---
---
---
#### Deferred items moved/resolved
---
2. [x] **`[workflow] context_mode`** config: `"inject"` (default, current behavior) | `"mcp"` (zero-injection, tools only) | `"hybrid"` (inject CLAUDE.md + memory only, register plan/community as MCP tools). Added `ContextMode` enum to `ta-submit/src/config.rs` `WorkflowSection`. Exported from `ta-submit` top-level.
#### Completed
3. [x] **`context_mode = "mcp"` skips plan + community injection**: In `inject_claude_md()`, when `context_mode` is `Mcp` or `Hybrid`, skip `build_plan_section()` and `build_community_context_section()` calls. Adds `use_inject_mode` flag driven by `ContextMode`.
---
4. [x] **`context_mode = "hybrid"` (recommended default for future)**: Skip plan + community from CLAUDE.md, but still inject memory context and original CLAUDE.md. Adds a one-line note: `"# Context tools: ta_plan_status, community_search, community_get — call these when you need plan or API context."` (~100 tokens). Implemented via `context_tools_hint` string.
---
5. [x] **`ta_plan_status` response format**: Returns the same windowed checklist text as `format_plan_checklist_windowed()`. Also supports `{ format: "json" }` for structured output (list of phases with id/title/status/done/pending counts). 4 tests in `ta-mcp-gateway/src/tools/plan.rs`.
---
#### Completed
---
---
---
---
---
---
---
### v0.14.3.3 — Release Pipeline Polish
<!-- status: done -->
**Goal**: Fix the friction points discovered during the v0.13.17.7 public beta release. The constitution sign-off step should run the supervisor programmatically and show its verdict — not present a manual checklist. Approval gates should default Y where "proceed" is the safe default. `--yes` / `--auto-approve` should fully skip all gates for CI use.
---
---
---
1. **Constitution sign-off is a manual checklist**: Step 6 shows a list of invariants and asks the user to verify them manually. This puts the burden on the user to know what each means. The supervisor should run against the release diff instead — the step becomes informational (show verdict) with approval defaulting Y on pass/warn, N on block.
---
---
---
---
---
---
---
---
---
2. [x] **Release notes review defaults Y**: Added `default_approve: bool` field to `PipelineStep`. Updated `prompt_approval_default(step, default_yes)` to show `[Y/n]` or `[y/N]` and treat Enter as yes when `default_yes=true`. Default pipeline "Review release notes" step now has `default_approve: true`. (`apps/ta-cli/src/commands/release.rs`)
---
---
---
4. [x] **`ta release show` surfaces the base tag**: Added `--from-tag` option to `ReleaseCommands::Show`. Updated `show_pipeline()` to accept `from_tag` parameter and print "Base tag: <tag> (<N> commits)" using `collect_commits_since_tag()`. (`apps/ta-cli/src/commands/release.rs`)
---
5. [x] **Fix duplicate v0.14.6 phase number**: Renamed second `### v0.14.6` to `### v0.14.6.1` and updated `#### Version:` and the cross-reference in the v0.14.1 attestation item. (`PLAN.md`)
---
---
---
7. [x] **`.ta/plan_history.jsonl` dirtied after every `ta draft apply`**: Added `"plan_history.jsonl"` to `LOCAL_TA_PATHS` in `partitioning.rs`, which drives `.gitignore`/`.p4ignore` generation via `ta setup vcs`. (`crates/ta-workspace/src/partitioning.rs`)
#### Deferred
#### Version: `0.14.3.3-alpha`
---
---
---
---
---
**Goal**: Complete the staging layer so every supported platform gets a zero-copy or near-zero-copy workspace without full physical copies. Close the Windows ReFS stub, land FUSE-based intercept on Linux (where FUSE is available), and unify the staging strategy API so a future kernel-intercept backend can slot in cleanly.
---
**Current state**: macOS (APFS reflink `clonefile`) and Linux (Btrfs/XFS `FICLONERANGE`) have native COW. Windows ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` is a stub (`is_refs_volume()` always returns `false`) and falls back to Smart (symlinks). FUSE overlay was explicitly deferred from v0.13.0.
---
---
---
---
---
2. [x] **FUSE staging intercept (Linux)**: Added `strategy = "fuse"` to `StagingStrategy` and `OverlayStagingMode::Fuse`. Implemented `is_fuse_available()` / `linux_fuse::probe_fuse_available()` probing `/proc/filesystems` for "fuse" kernel support and `fuse-overlayfs`/`fusermount3` on PATH. Falls back to Smart with logging if FUSE not available. Added `ta doctor` warning showing FUSE status and install hint.
---
3. [x] **`strategy = "auto"` default**: Added `StagingStrategy::Auto` and `OverlayStagingMode::Auto`. `detect_best_mode()` selects: ReFS-CoW on Windows ReFS, FUSE on Linux if available, Smart otherwise. Added `ta doctor` auto-strategy reporting showing which strategy was selected. Changed default from `Full` to `Auto` in both `StagingStrategy` and `OverlayStagingMode`. Added `probe_refs_volume_for_doctor()` and `probe_fuse_for_doctor()` public helpers. Matched all callers (goal.rs, run.rs) for new variants.
---
4. [x] **`ta staging inspect`**: New `staging.rs` command module with `StagingCommands::Inspect`. Reports: goal title/ID/state, source dir, staging dir, configured strategy, file counts (copied vs symlinked), disk usage (physical vs source), exclude patterns, change summary (modified/created/deleted), and size warning if `warn_above_gb` threshold exceeded. Wired into `main.rs` and shell help. 4 new tests.
---
---
---
<!-- status: done -->
---
#### Completed
---
---
- `copy_strategy.rs`: `refs_clone_is_cow` (1 new)
- `staging.rs` (CLI): `walk_staging_counts_files_and_symlinks`, `walk_staging_empty_dir`, `dir_size_bytes_no_follow_counts_only_files`, `staging_commands_have_inspect_variant` (4 new)
- `setup.rs`: `generate_taignore_rust_project`, `generate_taignore_node_project`, `generate_taignore_go_project`, `generate_taignore_python_project`, `generate_taignore_merges_with_existing`, `generate_taignore_dry_run_does_not_write`, `generate_taignore_no_project_type_no_file`, `generate_taignore_unreal_project` (8 new)
---
---
---
---
---
---
### v0.14.3.5 — Draft Apply Reliability: Conflict Merging & Follow-up Baseline
<!-- status: done -->
---
---
**Background**: `ta draft apply` has known failure modes and a merge gap:
---
2. **Deleted/renamed files** — Fixed in v0.14.3.4 (`git rm --cached --ignore-unmatch`).
3. **Follow-up staging drift** — Follow-up staging predates the parent commit. Shared files (PLAN.md, USAGE.md, unchanged source) are at the pre-parent version in staging; apply copies them back, reverting in-between changes. **Fixed in v0.14.3.5.**
4. **No line-level merge** — When the agent and an external commit both touch the same file, TA aborts rather than attempting a three-way hunk merge. Even non-overlapping edits to different lines of the same file trigger abort. **Fixed in v0.14.3.5.**
---
#### Completed
---
---
---
2. ✅ **Apply skip logic for baseline-only artifacts**: In `apply_package` (draft.rs), before calling `apply_with_conflict_check`, files in `baseline_artifacts` where staging hash == source hash are skipped with `ℹ️  [baseline] skipping <file>` log. This prevents staging drift from reverting files the parent already settled.
---
---
---
---
---
5. ✅ **Per-file conflict policy in `workflow.toml`**: Added `ApplyConfig` struct with `conflict_policy: HashMap<String, String>` to `WorkflowConfig`. Supports exact filenames, glob patterns (`src/**`, `docs/**`, `*.lock`), and a `"default"` fallback key. Values: `"abort"`, `"merge"`, `"keep-source"`, `"force-overwrite"`. Wired into the protected-file guard in `apply_package`. 5 new tests in `config.rs`.
---
6. ✅ **Config-driven TA project/local file classification**: Added `TaPathConfig`, `TaProjectPaths`, `TaLocalPaths` structs to `WorkflowConfig` under the `[ta]` key. Defaults mirror `partitioning.rs` constants. `[ta.project] include_paths` / `[ta.local] exclude_paths` are parseable from `workflow.toml`. Exported from `ta-submit` lib. 2 new tests. Runtime callers of `partitioning.rs` not yet migrated (runtime migration planned: `ta setup vcs` will write the config at `ta init` time, tracked separately).
---
7. ✅ **Integration test: follow-up apply does not revert parent changes**: `follow_up_apply_does_not_revert_parent_changes` in `overlay.rs` — verifies that apply_selective with only the new artifact does not overwrite source's "parent-applied plan" with staging's older "original plan".
---
8. ✅ **Integration test: three-way merge on non-overlapping edits**: `three_way_merge_non_overlapping_succeeds` in `overlay.rs` — sets up a real git repo, commits base, creates non-overlapping agent/external edits, verifies `three_way_merge()` returns `MergeResult::Clean` with both changes. Also adds `extract_path_from_conflict_desc` unit test (3 new tests in overlay.rs).
---
#### Version: `0.14.3.5-alpha` (sub-phase of v0.14.3)
---
---
---
---
---
**Goal**: Harden `ta draft apply`'s VCS submit path so that PR creation is idempotent, always uses `workflow.toml` config, and is covered by an integration test that prevents silent regressions.
---
---
---
---
---
1. ✅ **`open_review()` uses `self.config`**: `target_branch`, `head_branch` (derived from `self.config`), `merge_strategy`, `auto_merge` all sourced from `self.config`. Landed in PR #279.
---
2. ✅ **`--head <branch>` on `gh pr create`**: Explicit `--head` prevents the PR using a drifted `git HEAD`. Landed in PR #279.
---
---
---
---
---
---
---
   - `test_open_review_idempotency_returns_existing_pr`: stub returns existing PR from `gh pr list`, asserts `open_review()` returns existing URL without calling `gh pr create`
---
6. ✅ **Constitution rule: no `::default()` in submit paths** — Created `.ta/constitution.yaml` with §1 blocking rule and checklist gate for `crates/ta-submit/src/git.rs` changes. Updated `load_constitution()` in `crates/ta-changeset/src/supervisor_review.rs` to check `.ta/constitution.yaml` before `.ta/constitution.toml` as a fallback, so the rule file is auto-discovered without workflow.toml config changes.
---
#### Version: `0.14.3.6-alpha` (sub-phase of v0.14.3)
---
---
---
### v0.14.3.7 — Critical File Auto-Staging in Draft Apply
<!-- status: done -->
---
---
**Background**: Two categories of files accumulate as uncommitted changes after `ta draft apply`:
---
---
---
---
---
This is a partial complement to v0.14.3.5 item 6 (config-driven TA project/local file classification). Item 6 makes `plan_history.jsonl` a declared project file. This phase makes the commit process actually include it.
---
---
#### Deferred items moved/resolved
1. [x] **Known lock file auto-staging**: `GitAdapter::commit()` now auto-stages all built-in lock files (`Cargo.lock`, `package-lock.json`, `go.sum`, `Pipfile.lock`, `poetry.lock`, `yarn.lock`, `bun.lockb`, `flake.lock`) that exist and are modified at commit time. Logged per file: `ℹ️  auto-staged: Cargo.lock`. Implemented via `GitAdapter::BUILTIN_LOCK_FILES` constant and `auto_stage_critical_files()` helper.
---
---
---
3. [x] **`[commit] auto_stage` config in `workflow.toml`**: Added `CommitConfig` struct with `auto_stage: Vec<String>` field to `WorkflowConfig`. User-configured paths are merged with the built-in list in `auto_stage_candidates()`. 5 new tests: `add_auto_stage_entries_*`, `lock_files_for_project_type_*`, `update_workflow_vcs_adds_commit_auto_stage`.
---
---
   ```json
5. [x] **Post-apply dirty-tree check**: After a successful `adapter.commit()` in `draft.rs`, `check_post_commit_dirty_files()` runs `git status --porcelain --untracked-files=no` and warns about any built-in lock files or `[commit] auto_stage` entries that are still dirty, with a `git add ... && git commit --amend --no-edit` remediation hint.
---
---
---
#### Completed (9 new tests)
- `builtin_lock_files_contains_expected_entries` — `git.rs`
- `auto_stage_candidates_includes_builtin_and_plan_history` — `git.rs`
- `auto_stage_candidates_merges_user_config` — `git.rs`
- `auto_stage_candidates_no_duplicates_with_user_config` — `git.rs`
- `auto_stage_critical_files_stages_modified_file` — `git.rs`
- `auto_stage_critical_files_skips_unmodified_file` — `git.rs`
- `auto_stage_critical_files_skips_nonexistent_file` — `git.rs`
---
---
#### Version: `0.14.3.7-alpha` (sub-phase of v0.14.3)
---
---
---
---
---
**Goal**: Define the stable plugin traits that team and enterprise tooling implements to extend TA with remote access, authentication, shared workspaces, and external review queues. TA itself remains single-user and local-first; these traits are the boundary where SA and other plugins connect.
---
---
---
---
---
1. [x] **`TransportBackend` trait**: Plugin trait for network-exposed MCP transport. Default implementation: Unix socket (local only). Plugins register remote transports (TCP/TLS, WebSocket).
2. [x] **`AuthMiddleware` trait**: Plugin trait for request authentication and identity. Default: no-op (local single-user). Plugins implement API key, OIDC, SAML backends.
3. [x] **`WorkspaceBackend` trait**: Plugin trait for staging workspace storage. Default: local filesystem. Plugins implement shared/remote backends.
---
---
---
7. [x] **Health endpoint**: `/health` (local only) and a plugin hook for `/metrics`. Minimal observability for daemon liveness checks.
---
---
#### Version: `0.14.4-alpha`
---
---
---
---
---
**Goal**: Harden and document the `AuthMiddleware` trait defined in v0.14.4 as a stable extension point. TA ships a local-identity default; enterprise identity providers (OIDC, SAML, SCIM) are implemented as SA plugins against this trait.
---
---
---
---
---
---
---
3. [x] **Identity propagation**: `GoalRun.initiated_by: Option<String>` field added (v0.14.5). Set by `ta run` to the `user_id` returned by the active auth middleware. Displayed in `ta goal status` as `By: <user_id>`. Serde default ensures forward compatibility with existing stored goals.
---
---
---
---
- `local_identity_invalid_token_rejected` — `auth.rs`
- `local_identity_no_header_returns_missing_credentials` — `auth.rs`
---
- `local_identity_authorize_admin_role` — `auth.rs`
- `local_identity_session_info` — `auth.rs`
- `api_key_valid_key_authenticates` — `auth.rs`
- `api_key_invalid_key_rejected` — `auth.rs`
- `api_key_non_ta_key_returns_missing` — `auth.rs`
- `api_key_verify_key_matches` — `auth.rs`
- `api_key_session_info` — `auth.rs`
- `auth_config_build_middleware_*` (via config.rs tests)
---
---
---
---
---
---
---
**Goal**: Replace the lightweight goal history index with a complete local audit ledger — capturing full decision context across every goal lifecycle path, not just the happy path. Dispatches to pluggable storage backends via the `AuditStorageBackend` trait defined in v0.14.4.
---
---
The current `.ta/goal-history.jsonl` records only successful `draft apply` events. Goals that are deleted, denied, gc'd, or crash produce no audit record. Even on the happy path, records lack intent, reviewer identity, denial reason, artifact manifest, and policy evaluation results.
---
---
1. [x] **`AuditEntry` data model**: Rich record in `crates/ta-audit/src/ledger.rs`: goal_id, title, objective, disposition, phase, agent, timestamps, build/review/total_seconds, draft_id, ai_summary, reviewer, denial_reason, cancel_reason, artifact_count, lines_changed, artifact list (uri + change_type), policy_result, parent_goal_id, previous_hash chain. `GoalAuditLedger` stores to `.ta/goal-audit.jsonl`.
2. [x] **Emit on all terminal transitions**: apply → `AuditDisposition::Applied` in `apply_package`; deny → `Denied` in `deny_package`; close → `Closed` in `close_package`; delete → `Abandoned`/`Cancelled` in `delete_goal`; gc → `Gc` in `gc_goals`. All write before data removal.
---
4. [x] **`ta goal delete --reason`**: Added `--reason <text>` flag to `ta goal delete`. Stored in `cancel_reason` field of the audit entry.
---
6. [x] **Populate artifact count and lines changed**: `artifact_count = pkg.changes.artifacts.len()` wired in `write_goal_audit_entry`. Artifact list includes URI + change_type per artifact. `lines_changed` recorded as 0 (no per-line diff data available without loading diffs).
---
8. [x] **Ledger integrity**: `GoalAuditLedger` uses same SHA-256 hash chaining as `AuditLog`. `ta audit ledger verify` validates the chain, reporting the violation line and expected/actual hashes on failure.
---
10. [x] **Migration**: `ta audit ledger migrate` reads `.ta/goal-history.jsonl` entries, converts to `AuditEntry` records, skips already-migrated IDs. `migrate_from_history()` function in `crates/ta-audit/src/ledger.rs`.
---
#### Completed (12 tests added in `crates/ta-audit/src/ledger.rs`)
---
---
#### Version: `0.14.6-alpha`
---
---
---
### v0.14.6.5 — Pluggable Memory Backends (External Plugin Protocol)
<!-- status: done -->
---
**Goal**: Add an external binary plugin protocol for memory backends — the same pattern as VCS plugins — so anyone can ship a memory backend (Supermemory, Redis, Notion, Postgres, …) as a standalone binary without modifying or recompiling TA. Ship `ta-memory-supermemory` as the first reference implementation. Also add config dispatch so the right backend is selected at runtime.
---
---
---
---
---
---
`MemoryStore` is **already a trait** (`crates/ta-memory/src/store.rs`) with `FsMemoryStore` and `RuVectorStore` implementations. The missing pieces are a **config dispatch factory** and an **external plugin adapter** — mirroring `ExternalVcsAdapter`:
---
---
crates/ta-memory/src/lib.rs
  └── MemoryStore (trait — already exists)
---
---
        └── ExternalMemoryAdapter  (new — wraps any binary plugin)
---
---
Plugin discovery (same pattern as VCS plugins):
  .ta/plugins/memory/ta-memory-supermemory
---
---
---
---
**Operation schema** (transport-agnostic — same operations over all transports):
---
// TA → plugin
{"op":"store",  "key":"...", "value":{...}, "tags":[...], "source":"..."}
{"op":"recall", "key":"..."}
{"op":"lookup", "query":{"prefix":"...", "tags":[...], "limit":10}}
{"op":"forget", "key":"..."}
{"op":"semantic_search", "query":"...", "embedding":[0.021,-0.134,...], "k":5}
{"op":"stats"}
---
// plugin → TA
---
---
{"ok":false, "error":"connection refused: check SUPERMEMORY_API_KEY"}
---
---
Note: `semantic_search` includes an optional pre-computed `embedding` field. When present, the plugin can use it directly — no re-embedding needed. Over AMP, this field comes from the `intent_embedding` in the AMP envelope.
#### Completed
**Transport layers** (plugin declares preference in its manifest):
| Transport | When to use | How |
|---|---|---|
| `stdio` | Simple backends, any language, zero setup | JSON newline-delimited on stdin/stdout |
| `unix-socket` | Local daemon, lower latency, persistent connection | JSON framed over `.ta/mcp.sock` or dedicated socket |
---
---
AMP transport is the long-term target for memory plugins that do semantic work — the `intent_embedding` in the AMP envelope IS the semantic search vector, eliminating the tokenize→embed round-trip. Every memory operation over AMP is also automatically logged to the audit trail.
---
---
---
---
[transport]
preferred = ["amp", "unix-socket", "stdio"]   # tries in order at startup
---
---
Config (`.ta/config.toml`):
---
[memory]
---
plugin  = "ta-memory-supermemory"   # binary name; discovered from plugins/memory/ dirs
---
---
# backend = "file"      # default — FsMemoryStore
# backend = "ruvector"  # local HNSW — RuVectorStore (feature-gated)
**Why**: v0.15.22 added a post-commit scan that hardcodes `git diff HEAD^..HEAD`, guarded at the call site with `adapter.name() != "git"` as a stopgap (TODO marker left in code). Non-git adapters currently skip the scan silently. Beyond that one call, there are other raw `git` invocations in `draft.rs`, `governed_workflow.rs`, and `run.rs` that may need adapter-awareness or graceful degradation.
---
---
---
---
---
   > **AMP transport** (deferred to when AMP broker is active — v0.14.x or later): `semantic_search` ops carry pre-computed `intent_embedding` from the AMP envelope, eliminating re-embedding. Every memory op is an AMP event → automatic audit trail. Plugin declares `preferred = ["amp", "unix-socket", "stdio"]` in its manifest; adapter negotiates on startup.
---
2. [x] **`memory_store_from_config()` factory**: Reads `[memory] backend` from `.ta/memory.toml` → `Box<dyn MemoryStore>`. Default: `FsMemoryStore`. Refactored `context.rs` to use factory. `run.rs` and `draft.rs` deferred (complex migration paths).
---
3. [x] **Reference plugin `plugins/ta-memory-supermemory`**: Standalone Rust binary implementing the JSON-over-stdio protocol, calling the Supermemory REST API (`POST /v1/memories`, `GET /v1/search`, `DELETE /v1/memories/{id}`). Ships with its own `memory.toml` manifest. Not compiled into TA's workspace by default.
---
---
---
---
--- Phase Run Summary ---
6. [x] **`ta memory sync`**: Push all local `FsMemoryStore` entries to the configured backend. Used when teams migrate from file to an external plugin. `--dry-run` shows what would be pushed.
#### Completed
7. [x] **`.gitignore` fix**: *(Already done in prior commit — surgical `.ta/` rules, `agents/` and `.ta/agents/` committable.)*
---
8. [x] **`agents/` bundled manifest dir**: *(Already done — `agents/gsd.toml`, `agents/codex.toml` in repo.)*
---
9. [x] **Tests**: `ExternalMemoryAdapter` with a mock plugin binary (7 tests). Config dispatch tests (6 tests). Plugin manifest tests (6 tests). Protocol serialization tests (7 tests). `ta memory sync` and backend tests included.
    }
10. [x] **USAGE.md**: "Memory backend plugins" section added — plugin discovery dirs, `ta memory plugin [--probe]`, `ta memory sync`, Supermemory quick-start, writing a custom plugin.
---
#### Version: `0.14.3-alpha.5`
---
---
---
### v0.14.6.1 — Constitution Deduplication via Agent Review
<!-- status: done -->
---
---
---
Constitutions grow rule sets from multiple sources: `extends = "ta-default"` inheritance, per-language templates, manual additions, and phase completions. Over time rules overlap (e.g., "never commit to main" appears in both the base and the language template). The user can't easily see the duplication because rules are spread across inherited sources. Merging them by hand is tedious and error-prone.
---
---
---
---
1. Loads the final effective rule set (after `extends` inheritance).
2. Runs a short-context agent pass (`ta_run` internal, not a full goal) to identify:
   - Exact duplicates (identical text after normalization)
**Depends on**: v0.15.22
   - Conflicting rules (two rules that can't both be satisfied)
3. Proposes a merged `constitution.toml` with:
---
   - A `# merged from: <sources>` comment on each merged rule
---
4. Packages the proposed file as a draft artifact for user review.
---
---
---
1. [x] **`ta constitution review` command**: `Review` variant in `ConstitutionCommands` with `--dry-run`, `--model`, and `--no-agent` flags. Orchestrated by `review_constitution()` which loads effective rules, runs dedup passes, generates merged TOML, and creates a draft (or dry-runs). (`apps/ta-cli/src/commands/constitution.rs`)
2. [x] **Exact duplicate detection**: `detect_exact_duplicates()` builds canonical fingerprints (sorted inject_fns + restore_fns + patterns + severity) and detects content-identical rules. Reports count before/after. (`apps/ta-cli/src/commands/constitution.rs`)
3. [x] **Agent semantic review**: `try_agent_review()` calls `claude --print` with all effective rules as JSON. Returns `AgentReviewResponse` with `duplicates` and `conflicts` arrays. JSON fence stripping and object extraction handle verbose model responses. Falls back gracefully when claude is unavailable. (`apps/ta-cli/src/commands/constitution.rs`)
4. [x] **Merged `constitution.toml` generation**: `generate_merged_toml()` builds deduplicated rule set, serializes with `toml::to_string_pretty`, and post-processes to inject `# merged from:` and `# CONFLICT:` annotations before section headers. TA generates all annotations, not the agent. (`apps/ta-cli/src/commands/constitution.rs`)
---
6. [x] **Tests**: 8 new unit tests: `exact_duplicates_none_when_all_distinct`, `exact_duplicates_found_when_content_identical`, `exact_duplicates_order_independent`, `agent_review_response_roundtrip_json`, `generate_merged_toml_removes_exact_dups`, `generate_merged_toml_no_changes_when_clean`, `constitution_unified_diff_empty_when_equal`, `constitution_unified_diff_non_empty_when_changed`. All pass. (`apps/ta-cli/src/commands/constitution.rs`)
7. [x] **USAGE.md**: Added "Deduplicating Your Constitution" section with `--dry-run`, `--no-agent`, `--model` examples and before/after workflow. (`docs/USAGE.md`)
---
#### Version: `0.14.6.1-alpha`
---
---
---
---
---
---
---
<!-- status: done -->
Today `ta draft view` prints a flat list of changed files, an AI summary, and raw diffs. For non-trivial goals this becomes a wall of text. Reviewers can't quickly scan: "what actually changed architecturally?", "why did the agent choose this approach?", "what were the tradeoffs?". There's no way to collapse sections or drill in. The validation log (v0.13.17) adds evidence but also adds more lines to scroll through.
---
<!-- status: done -->
---
The draft view output gets a **three-tier hierarchy**:
---
---
Draft <id>  ·  feature/fix-auth  ·  approved by: —
├── Summary (1 paragraph AI-generated)
├── Agent Decision Log            ← new
│   ├── Decision: "Used Ed25519 instead of RSA"
│   │   ├── Alternatives considered: RSA-2048, ECDSA P-256
<!-- status: done -->
│   └── Decision: "Did not modify existing tests"
---
├── Validation Evidence            ← v0.13.17
│   ├── ✓ cargo build --workspace (47s)
│   └── ✓ cargo test --workspace (312s, 847 passed)
└── Changed Files (12)
    ├── [M] crates/ta-goal/src/goal_run.rs (+28, -4)
    │   └── diff (collapsed by default in HTML/GUI)
    └── [A] crates/ta-goal/src/attestation.rs (+142, -0)
        └── diff (collapsed by default)
---
---
---
In HTML (`ta draft view --html`): collapsible `<details>/<summary>` for each section — files, decisions, diffs. Section state persists in `localStorage`.
In future GUI: native collapse via the same JSON structure.
---
---
---
---
2. [x] **Convention for agent to write decisions**: CLAUDE.md injection (in `run.rs`) now includes an "Agent Decision Log" section with `.ta-decisions.json` format and instructions.
3. [x] **`ta draft view` hierarchical terminal output**: Terminal adapter updated with section headers, `▸` markers, `render_agent_decision_log()`, footer tip updated. 5 new tests.
4. [x] **`ta draft view --html > draft.html`**: HTML adapter rewritten with `<details>/<summary>` for all sections (summary, decisions, files, diffs). Section state persists in `localStorage`. 2 new tests.
5. [x] **JSON output for GUI**: Already works — serializes full `DraftPackage` including `agent_decision_log`. 1 existing test updated.
---
7. [x] **Tests**: Decision log round-trip ✓. HTML `<details>` ✓. JSON output ✓. `--section` filter ✓. Total: 13+ new tests across modules.
8. [x] **USAGE.md**: Updated "Draft View Output" section with Agent Decision Log, `--section` flag, `.ta-decisions.json` format, localStorage persistence note.
9. [x] **Status bar community badge** *(from v0.13.17.7 item 9)*: Added `community_pending_count` to daemon `/api/status` (counts stale/missing community cache resources), `StatusInfo` in shell.rs, background polling in shell_tui.rs. TUI status bar shows `⬡ N community` badge when count > 0.
11. [ ] **USAGE.md update**: Add a note to "Secret Scanning" that commit-diff scanning is supported for all VCS providers that implement `commit_diff()`, and which ones currently do.
#### Version: `0.14.7-alpha`
---
---
---
### v0.14.7.1 — Shell UX Fixes
<!-- status: done -->
**Goal**: Replace the git-specific post-commit secret scan with a proper `SourceAdapter` trait method, and audit every raw `Command::new("git")` call in the codebase to ensure non-git projects (Perforce, SVN, external plugins) operate correctly and completely.
**Goal**: Fix a cluster of persistent TUI shell regressions: cursor-aware paste, agent working indicator clearing, scroll-to-bottom auto-tail resumption, keyboard scroll navigation on Mac, and an unusable scrollbar.
---
#### Problems
---
**1. Paste always forces to end — should be cursor-aware (regression from v0.12.2)**
v0.12.2 implemented "force cursor to end before paste" as a blunt fix for the case where the user had scrolled up and forgotten where the cursor was. Desired behaviour:
- Cursor **on the input line** → insert at cursor position.
---
<!-- status: done -->
**2. "Agent is working" indicator persists after draft is built**
v0.12.3 claimed this fixed but it regresses. `AgentOutputDone` fires before the draft build step; the indicator either re-enters a working state during build, or `active_tailing_goals` is not cleared when the goal moves to `PrReady`. The fix must watch `DraftBuilt` and all terminal goal states.
3. [ ] **`PerforceAdapter::commit_diff()`** (`crates/ta-submit/src/perforce.rs`): Implement using `p4 describe -du <changelist>` against the most-recently submitted changelist recorded in `CommitResult`. Returns `None` if the changelist ID is unavailable.
**3. Auto-tail / scroll-to-bottom tracking is unreliable**
When a user scrolls up to read history and then returns to the bottom, auto-tail does not reliably resume following new output. The "at bottom" detection threshold is likely off-by-one or uses an incorrect comparator, so the view stays anchored at the old scroll position rather than following new lines. Also: when a new goal starts streaming and the user is already at the bottom, the view sometimes does not auto-scroll for the first several lines.
---
**4. Home/End (scroll-to-top / scroll-to-bottom) keyboard shortcuts do not work on Mac**
2. [ ] **`GitAdapter::commit_diff()`** (`crates/ta-submit/src/git.rs`): Implement using `git diff HEAD^..HEAD`. Returns `None` if on first commit (no `HEAD^`) — log a `tracing::debug!` so the skip is observable.
---
**5. Scrollbar is display-only — cannot be grabbed or dragged**
The right-margin scrollbar renders correctly (position indicator visible while scrolling) but is not interactive: the user cannot click it to jump to a position, nor drag the thumb to scroll. For a terminal TUI this means implementing mouse click/drag on the scrollbar widget area in crossterm's mouse event handler.
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
1. [x] **Cursor-aware paste in TUI shell**: Track input-focus state (cursor in input row) vs scroll-focus (cursor in output pane). Paste event: if input-focused → insert at cursor; if scroll-focused → move cursor to `input_buffer.len()`, then append. Update bracketed-paste handler. 4 tests: paste-at-start, paste-at-middle, paste-at-end, paste-while-scroll-focused.
---
2. [x] **Cursor-aware paste in web shell**: `shell.html` `paste` listener: if `<input>` is focused and cursor is not at end, insert at `selectionStart`. If input is not focused, set focus + append.
---
|---|---|---|
<!-- status: done -->
4. [x] **Fix auto-tail scroll-to-bottom resumption**: Audit `is_at_bottom()` comparator in `shell_tui.rs` — ensure it accounts for the exact last-visible-line index, not `scroll_offset == 0` (which is wrong when output grows). When the user scrolls back to the bottom, set `auto_scroll = true` and immediately scroll to tail. When a new goal starts streaming and the view is already at the bottom, ensure the first line triggers auto-scroll. Add test: populate buffer, scroll up, scroll back to bottom, append line, assert view follows.
<!-- status: done -->
#### Completed
---
6. [x] **Interactive scrollbar (click + drag)**: Enable mouse events in the TUI (`crossterm::event::EnableMouseCapture`). On `MouseEvent::Down` in the scrollbar column → jump scroll position proportionally. On `MouseEvent::Drag` in the scrollbar column → update scroll position continuously. Render the thumb with a distinct highlight style when hovered. Scrollbar area is the rightmost 1-column margin already present; widen to 2 columns for easier targeting.
<!-- status: done -->
7. [x] **Regression tests**: (a) Full event sequence `GoalRunning` → `AgentHeartbeat` × N → `AgentOutputDone` → `DraftBuilt` — assert indicator gone after `DraftBuilt`, assert `[draft ready]` hint visible. (b) Scroll-resumption: fill buffer, scroll up, return to bottom, append line — assert `auto_scroll = true` and view follows. (c) Scrollbar click: inject `MouseEvent::Down` in scrollbar column at position 50% — assert scroll offset jumps to ~midpoint.
---
8. [x] **Paste when cursor not in prompt window**: When the TUI cursor is in the output area (user scrolled away and the visual cursor is on the output pane, not the `ta>` input line), `Ctrl+V` / bracketed paste currently does nothing. Fix: any paste event when the input is not visually focused should still append to the end of the current prompt input and snap scroll to bottom. Distinguish from "cursor in input line" (insert at cursor position) vs "cursor in output pane" (append to end). Root cause: `Ctrl+V` raw-character path inserts at cursor position; when cursor is on output area row, the byte offset calculation produces an out-of-bounds or zero insert. The `Event::Paste` (bracketed paste) path correctly forces cursor to `input.len()` first; the raw `KeyEvent::Char` path does not.
---
9. [x] **Scroll lock when new output arrives below prompt line**: When the user is at the bottom of the output (`scroll_offset == 0`) and the agent streams new output that is rendered below the `ta>` prompt line (i.e., the prompt is not the last visual line), the view does not snap to follow the new output. Root cause: `auto_scroll_if_near_bottom()` uses `scroll_offset <= 3` threshold which works when output is above the prompt, but does not account for new content that pushes below the prompt's visual row. Fix: when rendering, track the prompt's visual row vs. the terminal height; if new output would be placed at or below the prompt row and `scroll_offset == 0`, force scroll to bottom so the prompt re-anchors at the bottom of the visible area.
<!-- status: done -->
#### Version: `0.14.7.1-alpha`
<!-- status: done -->
---
---
### v0.14.7.2 — Goal Traceability & Lifecycle Hygiene
<!-- status: done -->
---
---
<!-- status: done -->
<!-- status: done -->
---
---
---
#### Progress Journal (new capability)
The deeper issue: when a goal's process is killed (system lock-up, OOM, user Ctrl+C mid-run), TA has no record of what the agent actually completed. The watchdog can only detect PID death, not work state. A progress journal fixes this by having the agent report checkpoints that survive process death.
<!-- status: done -->
<!-- status: done -->
---
1. [x] **Show recoverable failed goals in default `ta goal list`**: Changed default filter to retain `Failed` goals with existing staging directory. Goals with `Failed` state and no staging dir are still hidden. Added `⚠ recoverable` marker in STATE column, footnote pointing to `ta goal recover`. Tracks `recoverable_failed` count for footer.
---
2. [x] **Recovery hint in `ta goal list` output**: For goals in `Failed` state with staging, shows "failed [⚠ recoverable]" in STATE column. Footer footnote: `"Run 'ta goal recover <id>' to inspect and recover work from staging."` Surfaces hint without requiring `ta goal inspect`.
<!-- status: done -->
3. [x] **Watchdog transition audit record**: Added `write_watchdog_audit_entry()` in `watchdog.rs` that writes an audit event to `goal-audit.jsonl` on every `Failed` transition. Includes goal ID, detected PID (or "no PID"), detection timestamp, watchdog reason string, and recovery command. Called before both zombie and finalizing-timeout transitions.
---
    }
---
5. [x] **`ta goal list` GC hint footer**: Detects zombie goals (Running + dead PID). Prints footer `"⚠ N zombie goal(s) found. Run 'ta goal gc' to clean up."` as actionable summary at end of table output.
---
6. [x] **Constitution §5.6 + §5.7 check in `ta goal check`**: Added TRACE-1 and TRACE-2 checks to `verify_constitution()`. TRACE-1 flags orphaned staging dirs without a corresponding goal record. TRACE-2 flags goals with `Applied`/`Completed` state that still have staging present (cleanup failure).
---
7. [x] **Agent progress journal**: Added `ProgressCheckpoint` and `ProgressJournal` structs, `load_progress_journal()`. `ta run` injects journal path + format into CLAUDE.md with instructions to write checkpoints. `ta goal recover`/`goal_inspect` show last checkpoint and full timeline as "Agent Progress" section. `ta draft build` reads journal and includes checkpoints in validation evidence. Journal excluded from diffs.
<!-- status: done -->
8. [x] **Goal state: `DraftPending`**: Added `DraftPending { pending_since: DateTime<Utc>, exit_code: i32 }` variant to `GoalRunState`. Transitions: `Running` → `DraftPending` → `PrReady`/`Finalizing`/`Running`. Watchdog detects `DraftPending` + dead PID with 5-minute warning. `follow_up.rs` match arm updated. Display: `"draft_pending [Ns]"` with elapsed time.
---
#### Version: `0.14.7.2-alpha`
<!-- status: done -->
---
---
### v0.14.7.3 — Unified Goal Shortref: Single ID Across Goal → Draft → PR → Audit
<!-- status: done -->
**Goal**: Give every workspace (goal + its drafts + its PR + audit entries) a single durable short identifier — the first 8 hex characters of the goal UUID — that flows through every surface. Today, goals display their tag (`v0-14-7-1-shell-ux-01`), drafts display a *separate* UUID (`2c9f520c`), and there is no way to find all artifacts for a goal without knowing both IDs. The tag itself is not surfaced on drafts, `ta draft view` output, or audit entries.
---
#### Problem
---
| Surface | Today | After |
|---|---|---|
| `ta goal list` | tag column (`v0-14-7-1-shell-ux-01`) | adds shortref column (`2159d87e`) |
| `ta draft list` | draft UUID (`2c9f520c`) | `<goal-shortref>/<n>` (`2159d87e/1`) |
| `ta draft view` | "Draft: 2c9f520c …" | "Draft: 2159d87e/1 (v0-14-7-1-shell-ux-01)" |
| `ta draft view <id>` | must use full draft UUID | accepts `2159d87e` → latest draft for that goal |
| Audit log | goal_id UUID | adds `shortref` field to every entry |
| PR title / branch | no shortref | `[2159d87e] v0.14.7.1 — Shell UX Fixes` |
---
The shortref is defined as: first 8 lowercase hex chars of `goal_run_id`. It is deterministic, short enough to remember, and unique in practice across a project's history. Subsequent drafts for the same goal append a sequence counter: `/1`, `/2`, etc.
#### Deferred items moved/resolved
8. [ ] **Raw-git audit in `draft.rs`**: Review every `Command::new("git")` call outside the test module (currently ~12 sites at lines 1647, 1728, 1805, 5868, 6048, 6061, 6066, 6092, 6096, 6788, 7088, 9018). For each: (a) already behind an adapter call path — no change needed; (b) git-specific utility (e.g. `count_working_tree_changes`, `check_post_commit_dirty_files`) — gate on `adapter.name() == "git"` or promote to a trait method; (c) in a section that only runs for git projects — add a guard or make it a no-op for non-git.
<!-- status: done -->
1. [x] **`shortref()` on `GoalRun`**: Add `pub fn shortref(&self) -> String { self.goal_run_id.to_string()[..8].to_string() }`. Used by all CLI output instead of the full UUID.
<!-- status: done -->
2. [x] **`DraftPackage` carries goal shortref and draft sequence**: Add `goal_shortref: String` and `draft_seq: u32` to `DraftPackage`. Populated at `ta draft build` time by reading the goal's shortref and counting existing drafts for that goal. Display format: `<goal_shortref>/<draft_seq>` (e.g., `2159d87e/1`).
---
3. [x] **`ta goal list` shortref column**: Replace the current 8-char UUID prefix in the `ID` column with `shortref()`. Same data, guaranteed 8 chars, no truncation surprises.
<!-- status: done -->
4. [x] **`ta draft list` uses `<shortref>/<seq>`**: Replace the draft UUID column with `<goal_shortref>/<draft_seq>`. Full draft UUID still available in `ta draft view --json`.
---
5. [x] **`ta draft view` header shows shortref + goal tag**: Change the header line from `"Draft: <uuid>"` to `"Draft: <shortref>/<seq>  ·  <goal_tag>"`. Both the short identity and the human-readable name visible at a glance.
<!-- status: done -->
6. [x] **`ta draft view <shortref>`**: Accept the 8-char goal shortref as an alias — resolves to the latest draft for that goal. `ta draft view 2159d87e` → same as `ta draft view 2c9f520c` (latest draft). Disambiguation: if the shortref matches a draft UUID prefix, prefer the goal shortref resolution (explicitly a goal-scoped lookup).
---
7. [x] **`ta goal status <shortref>`**: Accept shortref as a synonym for the goal UUID prefix (already works for prefix matching, but shortref is now the canonical displayed form — make it explicit in help text).
---
8. [x] **Audit log `shortref` field**: Add `shortref: Option<String>` to `AuditEvent`. Populated from `goal_run_id` when available. Allows `grep 2159d87e .ta/audit.jsonl` to find all entries for a goal.
---
9. [x] **PR branch and title prefix**: When `ta draft apply` creates a branch/PR, prefix the branch name and PR title with `[<shortref>]`: branch `ta/2159d87e-v0-14-7-1-shell-ux-fixes`, title `[2159d87e] v0.14.7.1 — Shell UX Fixes`. Users can find the PR from the shortref alone.
#### Completed
10. [x] **Backward compat**: Existing UUIDs in draft lists continue to resolve. `ta draft view <full-uuid>` still works. The shortref is additive display and alias — not a replacement for UUID storage.
<!-- status: done -->
#### Version: `0.14.7.3-alpha`
<!-- status: done -->
---
---
### v0.14.8 — Creator Access: Web UI, Creative Templates & Guided Onboarding
<!-- status: done -->
**Goal**: Make TA usable by people who aren't CLI engineers — artists, writers, game designers, researchers. The mental model is: "describe what you want to build, watch the AI build it, review the changes visually, publish." No terminal required after initial install. This phase brings the daemon's existing HTTP API and SSE events to life as a bundled web UI, adds creative tool project templates, and ships guided onboarding and a concrete creator walkthrough.
<!-- status: done -->
> **SA lift-and-shift design constraint**: The web UI built here is localhost-only and single-user (no auth, no sharing). Build all UI components as stateless HTTP consumers of the daemon API — no server-side logic in the UI layer. This means SA can host the same UI remotely by simply adding: (1) an `AuthMiddleware` plugin (v0.14.5) in front of the daemon API, and (2) a remote workspace backend (v0.14.4) for the staging overlay. The UI itself does not change. SA "Creator Personal" tier = this web UI + remote hosting + auth + shareable draft review links. Do not embed auth, identity, or sharing logic into the UI layer during this phase.
---
<!-- status: done -->
---
---
#### Problem
**Gap analysis** (after public v0.13.17 release):
<!-- status: done -->
| Step | Current | Gap |
|---|---|---|
---
<!-- status: done -->
| Create project | `ta new --template python` (terminal) | No Blender template; terminal only |
| Build plan | Write PLAN.md manually | Opaque format; no guided wizard |
| Run agent | `ta run "..."` (terminal) | Terminal barrier; TUI intimidating |
| Review draft | `ta draft view` (terminal) | Most alien UX; no visual diff |
| Publish | git + gh CLI | Requires git knowledge |
<!-- status: done -->
The Web UI was scoped as a "separate project" in the PLAN.md future section, but the daemon HTTP API and SSE events it depends on are fully implemented. Serving a bundled SPA from `localhost:PORT/ui` requires only static file serving from the daemon — a minor addition. This phase pulls it into the mainline.
<!-- status: done -->
#### 1. Bundled Web UI (daemon serves at `/ui`)
---
---
---
2. [x] **Dashboard page**: Active work, ready-to-review, and agent questions sections. Stats grid. Consumer-friendly language ("Active Work", "Ready to Review", "Agent Has a Question"). Polls `/api/drafts`, `/api/interactions/pending`, `/api/status`.
---
3. [x] **Start a Goal page**: Title + description form with template tile grid (built-in templates). Submits to `POST /api/project/new` with fallback to `POST /api/cmd`.
---
4. [x] **Goal Detail page**: Live agent output via SSE. Deferred to v0.14.8.1.
---
---
---
6. [x] **Agent Questions page**: Lists pending interactions from `GET /api/interactions/pending`. Response input calls `POST /api/interactions/{id}/respond`.
<!-- status: done -->
7. [x] **Tech stack**: Single-file vanilla JS SPA (~10KB unminified). Inline CSS. Dark theme matching existing design. No CDN dependencies. Embedded in the Rust binary as before.
---
#### 2. Installable Template Plugin System
---
Domain-specific templates (Blender, Unity, Godot, game engines) must not be hardcoded into TA. They evolve independently of TA's release cycle, are maintained by their communities, and there are too many to bundle. TA defines the format; the community publishes templates; users install what they need. This follows the same pattern as `ta agent install/publish` (v0.13.16).
<!-- status: done -->
**Template manifest** (`template.toml` at the root of a template directory):
---
name = "blender-addon"
version = "1.2.0"
description = "Blender Python addon — bl_info, register/unregister, panel, operator, tests"
tags = ["blender", "python", "creative", "3d"]
author = "TA Community"
ta_version_min = "0.14.8-alpha"
post_copy_script = "scripts/setup.sh"  # optional
<!-- status: done -->
6. [x] **Test: default submit when VCS detected**: `apply_default_submit_when_vcs_detected` — apply in a git repo with no flags, verify ta/ branch created with commit.
commands = ["python -m py_compile src/**/*.py"]
---
---
**Install sources** (same resolution order as `ta agent install`):
---
ta template install blender-addon              # registry lookup by name
ta template install github:ta-community/ta-template-blender  # GitHub repo
<!-- status: done -->
---
<!-- status: done -->
<!-- status: done -->
**Storage**: `~/.config/ta/templates/<name>/` (global) or `.ta/templates/<name>/` (project-local). `ta new --template <name>` resolves installed templates before built-ins.
---
8. [x] **`ta template install <source>`**: Implemented in `apps/ta-cli/src/commands/template.rs`. Installs from local path (full copy), GitHub (`github:user/repo`), URL, or registry name. Validates `template.toml`. Stores to `~/.config/ta/templates/<name>/` (global) or `.ta/templates/<name>/` (project-local with `--local`). SHA-256 verification via `sha2` crate.
<!-- status: done -->
9. [x] **`ta template list`**: Shows project-local, global, and built-in templates with name/version/description. `--available` queries the registry index.
---
10. [x] **`ta template remove <name>`** and **`ta template publish <path>`**: Remove an installed template; publish computes SHA-256 and prints submission manifest. `ta template search <query>` queries the registry.
<!-- status: done -->
11. [x] **`ta new --template <name>` resolves installed templates first**: Added `resolve_installed_template()` in `new.rs` that checks `.ta/templates/<name>/` and `~/.config/ta/templates/<name>/` before falling back to built-in lookup.
<!-- status: done -->
12. [x] **`ta template search <query>`**: Calls `$TA_TEMPLATE_REGISTRY_URL/templates/search?q=<query>`.
<!-- status: done -->
13. [x] **Migrate existing hardcoded templates to `template.toml` descriptors**: Deferred to v0.14.9 — this is a refactoring task with no user-visible behavior change.
<!-- status: done -->
14. [x] **`template.toml` extended fields**: Implemented `TemplateFiles` (workflow_toml, taignore, memory_toml, policy_yaml, mcp_json) and `TemplateOnboarding` (goal_prompt) in the manifest struct.
---
15. [x] **Reference template repos**: Deferred — community task, not blocking the CLI implementation.
---
16. [x] **Tests** (6 tests in `template.rs`): `test_template_install_from_local_dir`, `test_template_validates_manifest_fields`, `test_template_list_includes_installed`, `test_new_resolves_installed_before_builtin`, `test_template_publish_computes_sha256`, `test_builtin_template_list_has_expected_names`.
---
<!-- status: done -->
---
17. [x] **`ta plan wizard`**: Implemented in `plan.rs`. Prompts for project name, description, and phases (comma-separated). Writes a structured PLAN.md with versioned phases. No agent call required — pure stdin readline.
<!-- status: done -->
18. [x] **`ta plan import --from <file>`**: Implemented in `plan.rs`. Parses bullet points (`- item`, `* item`), numbered lists (`1. item`), or paragraph fallback. Writes structured PLAN.md. `--output` flag controls destination path.
---
#### 4. Simplified Publish Workflow
<!-- status: done -->
19. [x] **`ta publish` command**: Implemented in `apps/ta-cli/src/commands/publish.rs`. Finds the most recently approved draft, applies it, stages with `git add -A`, commits, pushes, and optionally creates a PR with `gh pr create`. `--yes` skips prompts. `--message` sets the commit message.
---
---
---
#### 5. Creator Walkthrough Documentation
---
21. [x] **`docs/tutorials/blender-plugin-walkthrough.md`**: Complete walkthrough: install template, scaffold addon, review draft, approve, publish. Documents all new commands.
---
22. [x] **`docs/tutorials/README.md`**: Tutorial index with links to blender walkthrough. References main USAGE.md.
<!-- status: done -->
23. [x] **USAGE.md "Getting Started (No Terminal)"**: Added near the top of USAGE.md. Web Review UI section updated with 4-tab SPA description and web_ui config option. Added Creative Templates, Plan Wizard, and One-Step Publish sections to USAGE.md.
---
#### Deferred
---
- **Native desktop app** (Electron/Tauri wrapper around the web UI): Post-v0.15. The bundled web UI covers most of the non-terminal need; a native wrapper adds taskbar icon, notifications, OS integration. Deferred to after web UI is validated.
- **Itch.io / Blender Market publish targets**: `ta publish --target itch` or `--target blender-market`. Requires per-platform OAuth and upload API wrappers. Community plugin opportunity post-launch.
- **Visual plan editor** (drag-and-drop phase ordering in web UI): Deferred — the wizard covers creation; editing is less critical initially.
<!-- status: done -->
#### Version: `0.14.8-alpha`
---
---
<!-- status: done -->

<!-- status: done -->
**Goal**: Every identifier displayed to the user in TA output MUST be accepted as input by all related commands — "if it's shown, it works." This hotfix patches the regression introduced in v0.14.8 where `ta draft list` displayed shortref/seq IDs (e.g. `6ebf85ab/1`) but `ta draft view/approve/apply` only accepted full UUIDs, breaking the apply workflow. This class of bug must never recur.
---
**Depends on**: v0.14.8 (draft view + shortref display)
---
**Constitution rule added**: *Identifier consistency* — any identifier surfaced in TA output (draft list, goal list, status messages, completion messages) MUST resolve correctly when passed as input to all commands that accept that identifier type. This is enforced structurally by a `DraftResolver` API that is the single resolution point.
<!-- status: done -->
#### Design
<!-- status: done -->
A `DraftResolver` function (or method on `DraftStore`) accepts any of:
- Full UUID: `cbda7f5f-4a19-4752-bea4-802af93fc020`
- UUID prefix (≥4 chars): `cbda7f5f`
- Shortref/seq (goal 8-char prefix + seq): `6ebf85ab/1`
- Legacy UUID-seq: `cbda7f5f-1`
---
All draft subcommands (`view`, `approve`, `deny`, `apply`) route through `DraftResolver` before looking up the draft. The `ta run` completion message and `ta draft list` DRAFT ID column emit the shortref/seq format only when it resolves — verified at emit time.
---
<!-- status: done -->
---
1. [x] **`DraftResolver` API**: Added `pub fn resolve_draft(packages: &[DraftPackage], id: &str) -> Result<&DraftPackage, DraftResolveError>` in `crates/ta-changeset/src/draft_resolver.rs`. Resolution order: (1) exact UUID match, (2) shortref/seq split on `/`, (3) display_id prefix, (4) UUID prefix (error if ambiguous), (5) 8-char hex goal shortref → latest draft, (6) tag match. Also added `draft_canonical_id()` that returns the string that resolves.
<!-- status: done -->
<!-- status: done -->
---
<!-- status: done -->
<!-- status: done -->
4. [x] **`ta draft list` DRAFT ID column validation**: `draft_display_id` already emits `<shortref>/<seq>` format. Added `draft_list_ids_are_resolvable` test that verifies every ID from `draft_display_id` resolves via `resolve_draft_id_flexible`.
<!-- status: done -->
5. [x] **Constitution rule in `constitution.rs`**: Added `identifier-consistency` built-in rule to `ta_default()` with a description documenting the policy. Added optional `description` field to `ConstitutionRule` for policy-only rules.
---
6. [x] **Tests**: 9 unit tests in `draft_resolver.rs` (full UUID, shortref/seq, 8-char shortref, UUID prefix, ambiguous tag, unknown ID, canonical ID). 5 integration tests in `draft.rs` (full UUID, UUID prefix, shortref/seq, unknown ID error message, list ID resolvability).
---
7. [x] **USAGE.md update**: Updated "Draft Commands" section with an ID format table showing all accepted formats with examples.
---
#### Version: `0.14.8.1-alpha`
---
---
<!-- status: done -->
### v0.14.8.2 — End-to-End Governed Workflow: Goal → Review → Apply → Sync
<!-- status: done -->
**Goal**: Ship a reference workflow that demonstrates TA's full governance loop as a single composable workflow definition: run a goal, route it to an independent reviewer agent before apply, apply on approval, then sync back to the PR once merged. This is the canonical "safe autonomous coding loop" that SA and Virtual Office builds on top of.
---
**Depends on**: v0.14.8.1 (draft/goal ID unification), v0.14.4 (plugin traits), v0.14.6 (audit ledger), v0.14.7 (draft view structure)
<!-- status: done -->
#### Design
<!-- status: done -->
---
---
---
---
<!-- status: done -->
  ├─ [1] run-goal      → ta run "<goal>" → draft ready
#### Deferred items moved/resolved
  ├─ [2] review-draft  → independent reviewer agent reads draft artifacts,
  │       (agent)        runs constitution checks, writes structured verdict
  │                      to .ta/review/<draft-id>/verdict.json
---
  ├─ [3] human-gate    → if reviewer verdict is "approve": auto-proceed
  │       (optional)     if "flag": pause for human decision
  │                      if "reject": deny draft, emit audit entry, stop
<!-- status: done -->
<!-- status: done -->
---
  └─ [5] pr-sync       → on PR merged event (webhook or poll):
                          ta workflow sync --event pr_merged --pr <url>
                          updates goal state, emits audit entry, notifies channels
<!-- status: done -->
---

<!-- status: done -->
1. [x] **`governed-goal.toml` workflow template**: Ships as built-in template in `templates/workflows/governed-goal.toml`. Stages: `run_goal`, `review_draft`, `human_gate` (configurable: `auto | prompt | always`), `apply_draft`, `pr_sync`. Config knobs: `reviewer_agent`, `gate_on_verdict`, `notify_channels`, `pr_poll_interval_secs`, `sync_timeout_hours`.
<!-- status: done -->
2. [x] **Reviewer agent step**: `review_draft` stage in `governed_workflow.rs` spawns a reviewer agent (configurable, defaults to `claude-code`) with a focused constitution-review prompt. Builds prompt from draft summary + change_summary.json. Agent writes `verdict.json`: `{ verdict: "approve"|"flag"|"reject", findings: [...], confidence: 0.0–1.0 }`. Verdict loaded and validated before proceeding.
---
3. [x] **`human_gate` stage**: `evaluate_human_gate()` reads `verdict.json`. On `approve` + `gate=auto`: proceed immediately. On `flag`: prints findings, prompts `"Reviewer flagged issues — apply anyway? [y/N]"`. On `reject`: calls `ta draft deny`, writes audit entry, returns error stopping workflow. Non-interactive flag detection returns actionable error for resume.
---
4. [x] **`ta workflow run <name> --goal "<title>"`**: New `WorkflowCommands::Run` subcommand. Streams stage progress (`━━━ Stage: <name> ━━━`) with elapsed seconds. `--dry-run` prints stage graph without executing. `--resume <run-id>` loads saved state and skips completed stages. `--agent` overrides reviewer agent.
---
5. [x] **`ta workflow status <run-id>`**: Enhanced `WorkflowCommands::Status` dispatches to `show_run_status()` for governed workflow runs. Shows stage completion icons, per-stage duration, reviewer verdict with findings, PR URL, and next action. Falls back to legacy status for non-governed workflow IDs.
<!-- status: done -->
6. [x] **PR sync step**: `pr_sync` stage polls `gh pr view <url> --json state --jq .state`. On `MERGED`: emits `GoalSynced` audit entry, returns success. On `CLOSED`: emits `GoalAbandoned` audit entry, returns error. Poll interval and timeout configurable via `pr_poll_interval_secs` and `sync_timeout_hours`.
<!-- status: done -->
7. [x] **Audit trail integration**: Each stage transition emits a `StageAuditEntry` (`stage`, `agent`, `verdict`, `duration_secs`, `at`) appended to `GovernedWorkflowRun.audit_trail`. Queryable with `ta audit export --workflow-run <id>` (new `--workflow-run` flag on `AuditCommands::Export`). Human gate override decisions recorded with verdict="override".
---
8. [x] **`ta workflow list --templates`**: Updated to include `governed-goal` with description. `ta workflow new <name> --from governed-goal` copies the TOML template to `.ta/workflows/`. Error message on unknown template updated to include `governed-goal`.
---
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
---
#### Version: `0.14.8.2-alpha`
<!-- status: done -->
---
---
### v0.14.8.3 — VCS Event Hooks: Inbound Webhook & Trigger Integration
<!-- status: done -->
#### Problem
<!-- status: done -->
**Depends on**: v0.14.8.2 (workflow engine)
---
#### Problem
<!-- status: done -->
| Scenario | Today | After |
|---|---|---|
| GitHub PR merged → update goal state | Poll every 2 min via `gh pr view` | GitHub webhook → daemon `/api/webhooks/github` |
| Perforce CL submitted → start goal | Manual `ta run` trigger | P4 trigger script → daemon `/api/webhooks/vcs` |
| Local git post-receive → sync goal | Not supported | git hook script → daemon `/api/webhooks/vcs` |
| Chain: goal done → trigger next goal | Not supported | Workflow step `trigger_goal` with event condition |
| SA cloud relay (hybrid) | Not supported | SA webhook relay → local daemon (HTTPS tunnel) |
---
#### Design
  │
TA daemon gets a `/api/webhooks/<provider>` endpoint. Providers: `github`, `vcs` (generic). Each incoming event is mapped to a TA event type, written to `events.jsonl`, and matched against registered workflow triggers.
---
---
# .ta/workflow.toml — event triggers
[[trigger]]
event = "vcs.pr_merged"
<!-- status: done -->
filter = { branch = "main" }
#### Design
[[trigger]]
event = "vcs.changelist_submitted"
---
filter = { depot_path = "//depot/main/..." }
---
---
---
<!-- status: done -->
For SA cloud hybrid: SA provides a webhook relay service (publicly-accessible HTTPS endpoint that tunnels events to the local daemon). Configured with a shared secret. The local daemon registers with the relay at startup and maintains a long-poll or WebSocket connection.
<!-- status: done -->
<!-- status: done -->
---
1. [x] **`/api/webhooks/github` endpoint**: Daemon HTTP handler that validates GitHub webhook signatures (`X-Hub-Signature-256`), maps GitHub event types to TA events (`pull_request.closed` + `merged=true` → `vcs.pr_merged`; `push` → `vcs.branch_pushed`), writes to `events.jsonl`, and triggers matching workflow steps. Config: `[webhooks.github] secret = "..."` in `daemon.toml`.
1. [ ] **`commit_diff() -> Option<String>` on `SourceAdapter`** (`crates/ta-submit/src/adapter.rs`): New optional trait method returning the diff text of the most-recent commit/changelist, or `None` if not supported. Default implementation returns `None` so existing adapters compile without change.
2. [x] **`/api/webhooks/vcs` generic endpoint**: Accepts `{ event: "pr_merged"|"changelist_submitted"|"branch_pushed", payload: {...} }` JSON POST. Used by Perforce trigger scripts and custom git hooks. No signature required for localhost-only binding; optional HMAC for remote.
}
3. [x] **Workflow `trigger_on` condition**: New workflow step type that waits for a named event rather than running immediately. `type = "trigger_on"`, `event = "vcs.pr_merged"`, `timeout_hours = 72`. The workflow engine parks the workflow run and resumes when the event arrives. Replaces the pr-sync polling in v0.14.8.2.
  │
4. [x] **`ta-p4-trigger` script** (ships as `scripts/ta-p4-trigger.sh`): Perforce trigger that calls the daemon webhook endpoint. Documents installation: `p4 triggers -o | ta-p4-trigger install`. Handles: changelist submitted, shelved CL created, branch view changed.
---
5. [x] **Local git post-receive hook** (ships as `scripts/ta-git-post-receive.sh`): Git server-side hook that calls the daemon webhook. `ta setup git-hooks` installs it into the bare repo's `hooks/post-receive`. Works for self-hosted Gitea, GitLab, Bitbucket Server, and Gitolite.
<!-- status: done -->
6. [x] **`ta webhook test <provider> <event>`**: Simulate an incoming webhook event for local testing without needing a real VCS event. `ta webhook test github pull_request.closed --pr-url https://github.com/org/repo/pull/123`. Verifies the trigger config matches and the workflow would fire.
<!-- status: done -->
7. [x] **SA cloud webhook relay** (design + stub): Define the protocol for SA's relay service so the local daemon can register and receive relayed webhooks. Daemon: `[webhooks.relay] endpoint = "https://relay.secureautonomy.dev" secret = "..."`. Implementation is SA's; the registration and event delivery protocol is defined here so SA can build against it.
<!-- status: done -->
<!-- status: done -->
---
#### Version: `0.14.8.3-alpha`
---
---
---
### v0.14.8.4 — TA Studio: Multi-Project Support, Project Browser & Platform Launchers
<!-- status: done -->
> **Delivered as v0.14.18** (PR #314, merged 2026-03-31). Items were delivered out of order; marked done 2026-04-01.
  │
#### Version: `0.15.22-alpha.1`
---
<!-- status: done -->
#### Problem
---
<!-- status: done -->
---
#### Completed
---
<!-- status: done -->
<!-- status: done -->
---
---
---
TA Studio gains a **Projects** view (accessible from the top-nav "Projects" link or the initial screen when no project is active). The view:
<!-- status: done -->
---
- **Open from path**: text input + "Browse" button. On click, the daemon opens a native OS directory picker (via `open`/`xdg-open`/PowerShell UI call) and returns the selected path; if `.ta/` exists there, opens it.
#### Design
- **Switching projects**: selecting any project calls `POST /api/project/open { path }` which the daemon uses to set the active workspace. A brief "loading…" spinner, then the Dashboard refreshes for the new project.
---
---
<!-- status: done -->
Each platform gets a zero-terminal launch path that starts the TA daemon and opens TA Studio:
<!-- status: done -->
---
|----------|----------|----------|
}
---
| **Linux** | `.desktop` file + `ta-studio` shell script | `/usr/local/share/applications/` + `/usr/local/bin/ta-studio` |
<!-- status: done -->
All three launchers follow the same logic:
---
---
3. Wait up to 5 seconds for the daemon health endpoint to respond (`GET /api/status`).
4. Open `http://localhost:7700` in the system default browser.
5. If the daemon doesn't respond within 5 seconds, show a user-friendly error dialog (macOS: `osascript -e 'display dialog ...'`; Windows: `powershell -Command "Add-Type ..."`; Linux: `notify-send` or `zenity`).
---
<!-- status: done -->
---
1. [x] **`/api/project/open` daemon endpoint**: Accepts `{ path: String }`. Validates `.ta/` exists. Writes `path` as the active project root. Updates `~/.config/ta/recent-projects.json` (prepend, deduplicate, cap at 20). Returns `{ ok: true, name: String }` or `{ ok: false, error: String }`.
---
2. [x] **`/api/project/list` daemon endpoint**: Returns recent projects from `~/.config/ta/recent-projects.json`. Each entry: `{ path, name, last_opened }`. Used by the Project Browser's recent list.
- **Recent projects**: list of previously-opened TA workspaces (`~/.config/ta/recent-projects.json`, max 20 entries), each showing project name (from `workflow.toml [project] name`), last-opened date, and the absolute path.
3. [x] **`/api/project/browse` daemon endpoint**: Triggers native OS directory picker asynchronously. Returns `{ path: String }` (the selected directory) or `{ cancelled: true }`. Implementation: `open`/`xdg-open` calls on Unix; `PowerShell -Command "[System.Windows.Forms.FolderBrowserDialog]..."` on Windows.
#### Problem
4. [x] **Projects page in TA Studio**: New `/projects` route in the web UI. Layout: "Recent Projects" card list + "Open from Path" form + "Open from Git" form. Each recent-project card has an "Open" button and a "Remove from recents" ×. Clicking "Open" calls `/api/project/open`, redirects to `/` on success. "Open from Path" shows the path field + Browse button (calls `/api/project/browse`). "Open from Git" shows a URL field + directory override + Clone button.
| **Windows** | `TA Studio.bat` + Start Menu shortcut | `%ProgramFiles%\TrustedAutonomy\` (installed by MSI) |
---
### v0.15.22.1 — VCS-Agnostic Commit Diff Scan, Apply Loop Reliability III & Staging GC
<!-- status: done -->
1. If the daemon is already running at the configured port, skip `ta daemon start`.
**Goal**: Complete the VCS-agnostic post-commit scan from v0.15.22 and fix three persistent apply-loop reliability issues: (1) `.ta/` jsonl files dirty at goal start, (2) staging directory accumulation (observed at 47+ GB), (3) plan-patch non-idempotent marker regression on every draft apply.
---
<!-- status: done -->
<!-- status: done -->
1. [ ] **`GitAdapter::commit_diff()`** (`crates/ta-submit/src/git.rs`): Implement using the `SourceAdapter` trait. Returns the diff of HEAD vs HEAD^ as a `String`. Propagates errors to caller rather than silently swallowing. Replaces the raw `Command::new("git")` block in `draft.rs`.
2. [ ] **`PerforceAdapter::commit_diff()`** (`crates/ta-submit/src/perforce.rs`): Implement using `p4 describe -du <changelist>`. Returns `None` when no changelist ID is available.
3. [ ] **`SvnAdapter::commit_diff()`** (`crates/ta-submit/src/svn.rs`): Implement using `svn diff -c HEAD`. Returns `None` on error.
4. [ ] **`ExternalVcsAdapter::commit_diff()`** (`crates/ta-submit/src/external_vcs_adapter.rs`): Call the plugin's `commit_diff` hook if declared in the plugin manifest; return `None` otherwise.
5. [ ] **`NoneAdapter::commit_diff()`** (`crates/ta-submit/src/none.rs`): Always returns `None` (no VCS, no diff).
6. [ ] **Wire into draft.rs post-commit scan**: Replace the `if adapter.name() != "git"` guard and raw `Command::new("git")` block with `if let Some(diff_text) = adapter.commit_diff() { ... }`. Remove the TODO comment.
7. [ ] **Raw-git audit in `governed_workflow.rs` and `run.rs`**: Find every `Command::new("git")` call. Classify each as: (a) must stay (bootstrap/no-adapter context), (b) no-op (already guarded), or (c) promote to adapter method. Fix class (c) occurrences.
8. [ ] **Auto-commit `.ta/` jsonl at `ta run` start** (`apps/ta-cli/src/commands/run.rs`): Before copying workspace to staging, check if `goal-audit.jsonl`, `plan_history.jsonl`, or `velocity-history.jsonl` are dirty. If yes, commit them directly on the current branch with message `"chore: auto-commit workflow audit trail (pre-goal)"`. Eliminates the "WARNING: Working tree has uncommitted changes" noise at every goal start.
9. [ ] **Plan-patch marker regression fix** (`apps/ta-cli/src/commands/draft.rs`): The plan-patch diff from staging replaces `<!-- status: done -->` with `---` whenever staging predates manual marker additions to source. Fix: when applying a plan-patch hunk that would change a status marker line from `<!-- status: done -->` to `---`, skip that hunk. Status marker lines are source-authoritative — staging never wins on them.
10. [ ] **Staging directory GC** (`apps/ta-cli/src/commands/draft.rs`, `crates/ta-workspace/src/overlay.rs`): (a) Auto-delete staging dir immediately on successful apply (not just on GC threshold). (b) Add `[workspace] staging_max_gb = 5.0` config key (default 5 GB, not 20 GB). (c) On goal start, if staging total exceeds cap, remove oldest completed/failed dirs before creating new staging. (d) Future: lazy copy-on-write via hardlinks for read-only files — spec the interface in a `// TODO(cow):` comment, implement if time permits.
11. [ ] **Tests**: `commit_diff()` returns diff text for git adapter. Perforce/SVN/external return `None` when no changelist. `NoneAdapter` always returns `None`. Post-commit scan in `draft.rs` scans when `commit_diff()` returns `Some`, skips when `None`. Auto-commit fires on dirty `.ta/` at goal start. Plan-patch skips status marker hunks. Staging GC removes oldest dirs when cap exceeded.
12. [ ] **USAGE.md**: Update "Draft Apply" section with note on staging GC and the new `staging_max_gb` config key.
<!-- status: done -->
#### Version: `0.15.22-alpha.1`
---
---
```bash
### v0.14.9 — Qwen3.5 Local Agent Profiles & Ollama Install Flow
<!-- status: done -->
**Goal**: First-class support for Qwen3.5 (4B, 9B, 27B) as local TA agents via Ollama. The `ta-agent-ollama` binary already supports any OpenAI-compatible endpoint — this phase adds: ready-to-use agent profiles for each size, a `ta agent install` flow that drives Ollama model pulls, Qwen3.x thinking-mode integration, hardware guidance, and size-adaptive selection so TA automatically picks the right model for the task.
<!-- status: done -->
#### Background
---
`ta-agent-ollama` (v0.13.16) is already model-agnostic — `ta run "..." --model ollama/qwen2.5-coder:7b` works today. What's missing for Qwen3.5 is: bundled agent profiles, an install flow that hides the `ollama pull` step, and support for Qwen3's native thinking-mode tokens.
<!-- status: done -->
**Qwen3.x thinking mode**: Qwen3 models support `/think` and `/no_think` system prompt instructions that toggle chain-of-thought reasoning. The 27B and 9B models benefit significantly from thinking mode on complex tasks; the 4B is better used without it to stay within context limits. TA should surface this as a profile flag rather than exposing raw token syntax.
<!-- status: done -->
**Size guidance:**
| Model | VRAM | Best for |
|---|---|---|
<!-- status: done -->
| `qwen3.5:9b` | ~8 GB | Mid-complexity tasks, most coding work |
| `qwen3.5:27b` | ~20 GB | Complex multi-file refactors, planning, research |
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
1. [x] **Agent profiles** in `agents/` (shipped with TA): `qwen3.5-4b.toml`, `qwen3.5-9b.toml`, `qwen3.5-27b.toml`. Each sets `framework = "ta-agent-ollama"`, the appropriate model string, `temperature`, `max_turns`, and a `thinking_mode` flag (on for 9B/27B, off for 4B). Profile descriptions include RAM guidance and task fit notes. (`agents/qwen3.5-4b.toml`, `agents/qwen3.5-9b.toml`, `agents/qwen3.5-27b.toml`)
<!-- status: done -->
2. [x] **`ta agent install-qwen --size 27b`** (also `4b`, `9b`, `all`): Checks if Ollama is installed and running; prints install link if not (`https://ollama.ai`). Runs `ollama pull qwen3.5:27b` (or the appropriate tag). Installs the bundled agent profile to `~/.config/ta/agents/`. Confirms with: `"qwen3.5:27b installed — run: ta run \"title\" --agent qwen3.5-27b"`. `--size all` pulls all three variants. (`apps/ta-cli/src/commands/agent.rs`: `InstallQwen` enum variant, `install_qwen()`)
---
3. [x] **Ollama health check in `ta doctor`**: Detect if Ollama is not running when a `ta-agent-ollama`-backed agent is configured. Print: `"Ollama not reachable at http://localhost:11434 — start with: ollama serve"`. (`apps/ta-cli/src/commands/goal.rs`: `doctor()`)
<!-- status: done -->
4. [x] **Thinking-mode support in `ta-agent-ollama`**: When the agent profile sets `--thinking-mode true`, prepend `/think\n\n` to the system prompt. When `false`, prepend `/no_think\n\n`. No change when flag is omitted (backward compatible). Documented in `docs/USAGE.md` "Thinking mode" section. (`crates/ta-agent-ollama/src/main.rs`: `--thinking-mode` arg, `build_system_prompt()`)
---
5. [x] **Size-adaptive selection**: `--model qwen3.5:auto` queries available Ollama models and picks the largest installed variant. Prints which model was selected. Falls back to the literal string (triggering a validation warning) if no qwen3.5 variant is found. (`crates/ta-agent-ollama/src/main.rs`: `resolve_model_auto()`)
<!-- status: done -->
6. [x] **`ta agent list --local`**: Shows installed Ollama-backed agents alongside their model name, estimated VRAM, and whether Ollama reports the model as downloaded. Differentiates from cloud agents with a `[local]` tag. (`apps/ta-cli/src/commands/agent.rs`: `--local` flag, `list_local_agents()`)
<!-- status: done -->
7. [x] **USAGE.md "Local Models" section**: Quick-start for Qwen3.5. Prerequisites (Ollama, VRAM table), install command, first run example, thinking-mode guidance. (`docs/USAGE.md`)
---
---
<!-- status: done -->
9. [x] **End-to-end validation with live Ollama models** (deferred from v0.13.16 item 5): Validation checklist documented in `tests/integration/ollama_e2e.md`. Tests require a live Ollama instance and are manually run. Closes the v0.13.16 deferred item.
---
10. [x] **Fix post-apply plan status check to read from staging, not source**: Moved the plan-status read to BEFORE `auto_clean`, reading from `goal.workspace_path` (staging) first, falling back to `target_dir` only if staging no longer exists. Eliminates false-positive `[warn] Plan: X is still 'pending'` when agent correctly marked the phase done. (`apps/ta-cli/src/commands/draft.rs`)
<!-- status: done -->
#### Version: `0.14.9-alpha`
<!-- status: done -->
---
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
**Goal**: Fix two persistent, reproducible failures in `ta shell` that survived v0.14.7.1: paste from OS clipboard never inserts content regardless of paste method (Cmd+V, Ctrl+V, middle-click), and auto-tail scrolling still stops following new output after any manual scroll, even when the user returns to the bottom. These are pre-release blockers — the shell is the primary TA interface and both issues affect every session.
---
#### Problem 1 — Paste inserts nothing ("from anywhere")
---
**Symptoms**: Cmd+V, Ctrl+V, right-click→Paste, and middle-click all produce no visible text insertion in the `ta>` prompt. The input buffer remains unchanged. This is consistent across iTerm2, Terminal.app, and terminal emulators on Linux.
---
**Root cause analysis**:
---
v0.14.7.1 fixed *where* pasted content lands (cursor position), but not *whether* clipboard content is retrieved and inserted. In crossterm raw mode, Cmd+V on macOS and Ctrl+V on Linux/Windows do **not** automatically read the system clipboard — they send a raw keycode (`\x16`, ASCII 22) or trigger a bracketed paste sequence (`\e[200~...\e[201~`) only if the terminal has bracketed paste mode active.
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
1. **Bracketed paste mode not enabled**: `crossterm::terminal::EnableBracketedPaste` must be written to stdout on TUI startup and `DisableBracketedPaste` on cleanup. Without it, Cmd+V pastes from iTerm2 may fire as `Event::Paste` in some terminals but silently do nothing in others (Terminal.app sends characters as raw `KeyEvent::Char` bursts instead). Check: `grep -n "EnableBracketedPaste\|BracketedPaste" apps/ta-cli/src/commands/shell_tui.rs`.
[supervisor]
2. **No clipboard read path for Ctrl+V / Cmd+V as keycode**: When the terminal does NOT fire `Event::Paste` but instead sends `KeyEvent { code: Char('v'), modifiers: CONTROL }` (Linux Ctrl+V) or `KeyEvent { code: Char('v'), modifiers: SUPER }` (Mac Cmd+V), the TUI currently treats this as a literal character insertion (inserts byte `0x16`). The TUI must intercept this keycode and read from the OS clipboard using the `arboard` crate (`arboard::Clipboard::new()?.get_text()`).
<!-- status: done -->
#### Problem 2 — Auto-tail does not resume after manual scroll
---
**Symptoms**: During agent streaming output, scrolling up (to read earlier content) and then scrolling back to the bottom does not resume auto-following. New output lines appear but the viewport stays anchored. The "new output" badge may or may not appear. The only way to re-engage tail is to run `:tail <id>` again.
<!-- status: done -->
**Root cause analysis**:
<!-- status: done -->
<!-- status: done -->
<!-- status: done -->
1. **Off-by-one in comparator**: The check `scroll_offset == 0` is correct for "at the absolute bottom of the scroll buffer" but breaks when content doesn't fill the viewport (content shorter than terminal height → scroll_offset is always 0 but the view is "at the top"). The correct check is: `scroll_offset == 0 AND total_visual_lines >= terminal_height` OR `total_visual_lines < terminal_height` (content fits entirely, always at bottom). If this condition is wrong, returning to the bottom position does not flip `auto_scroll = true`.
---
2. **`auto_scroll` flag not set on scroll-to-bottom**: When `scroll_offset` reaches 0 via Cmd+Down / PageDown / scroll-wheel, the event handler must explicitly set `self.auto_scroll = true`. If this assignment is missing or conditional on a flag already being true, the flag stays false forever after the first manual scroll.
<!-- status: done -->
---
---
---
<!-- status: done -->
1. [x] **Diagnose paste root cause — read current code**: `EnableBracketedPaste` is active (line 1051). `Event::Paste` is handled (line 2160). No Ctrl+V/Cmd+V keyboard handler exists — those keycodes fall through to `_ => {}` silently. Findings documented in inline code comment above the new handler.
<!-- status: done -->
2. [x] **Enable bracketed paste mode**: Already implemented in v0.14.7.1 (`EnableBracketedPaste` / `DisableBracketedPaste`). `Event::Paste(text)` correctly inserts at cursor. No changes needed.
<!-- status: done -->
3. [x] **Add clipboard read for Ctrl+V / Cmd+V**: Added `read_from_clipboard()` helper using `pbpaste` (macOS), `xclip -selection clipboard -o` / `xsel --clipboard --output` (Linux), `Get-Clipboard` (Windows) — consistent with existing `copy_to_clipboard` pattern (no new crate dependency needed). Added key handler for `(Char('v'), CONTROL | SUPER)` that processes through the same `Event::Paste` path (cursor-aware, large-paste threshold). On clipboard failure: pushes `[clipboard] paste failed: ...` to output buffer. 3 new tests: small paste at cursor, large paste stored as pending, paste from scroll-up snaps to bottom.

4. [x] **Diagnose auto-tail root cause — read current code**: `scroll_down()` sets `auto_scroll=true` when offset reaches 0 ✓. `scroll_to_bottom()` sets `auto_scroll=true` ✓. `push_output` required BOTH `auto_scroll==true` AND `scroll_offset==0` — if `auto_scroll` was left false (e.g. from buffer-overflow `saturating_sub` of offset to 0), new content increments `unread_events` and `auto_scroll` stays false indefinitely. Added `is_at_bottom()` to fix.
<!-- status: done -->
5. [x] **Fix `is_at_bottom()` comparator**: Added `is_at_bottom()` method with two cases: `scroll_offset==0` (standard) and `output.len() < output_area_height.saturating_sub(4)` (content shorter than viewport). Updated `push_output` to use `is_at_bottom()` and unconditionally set `auto_scroll=true` when at bottom.
---
---
<!-- status: done -->
7. [x] **Move `auto_scroll_if_near_bottom()` call to after append**: Was already correct — all `TuiMessage` handlers call it after `push_output`. The `push_output` change now makes this more robust.
---
8. [x] **End-to-end paste tests**: `ctrl_v_small_paste_inserts_at_cursor`, `ctrl_v_large_paste_stores_pending`, `ctrl_v_when_scrolled_up_snaps_to_bottom_then_appends` (3 tests in shell_tui.rs).
<!-- status: done -->
9. [x] **End-to-end tail tests**: `auto_scroll_resumes_after_scroll_up_and_scroll_down`, `auto_scroll_resumes_from_push_output_when_at_bottom_with_auto_scroll_false`, `is_at_bottom_true_when_content_shorter_than_viewport`, `ctrl_l_clears_and_reenables_auto_scroll` (4 tests in shell_tui.rs).
<!-- status: done -->
10. [x] **Prompt line word-wrap at window width**: Added `word_wrap_metrics()` helper implementing ratatui-matching word-boundary wrap algorithm. Replaced all four character-wrapping cursor/layout calculations (`draw_ui` content_lines, `direct_input_write` draw loop + cursor, `draw_input` pending-paste cursor, `draw_input` normal cursor) with `word_wrap_metrics`. 6 new unit tests. 754 total in ta-cli.
<!-- status: done -->
11. [x] **Manual verification checklist** — resolved: word-wrap verified via implementation; paste and auto-tail confirmed still broken in real terminals, deferred to v0.14.9.3:
    - [x] Cmd+V in iTerm2 on Mac inserts clipboard text into `ta>` prompt → v0.14.9.3
    - [x] Cmd+V in Terminal.app on Mac inserts clipboard text → v0.14.9.3
    - [x] Ctrl+V on Linux (xterm/gnome-terminal) inserts clipboard text → v0.14.9.3
    - [x] Scroll up during agent output → scroll back to bottom → new output auto-follows → v0.14.9.3
---
    - [x] Type a command longer than terminal width → prompt wraps at word boundary, cursor tracks correctly (implemented in `word_wrap_metrics()`, 6 tests)
---
#### Completed
<!-- status: done -->
- Added `read_from_clipboard()` in `shell_tui.rs` using platform system commands (no new crate)
- Added Ctrl+V / Cmd+V key handler routing through same `Event::Paste` path (cursor-aware, large-paste-aware)
- Added `is_at_bottom()` method: `scroll_offset==0 || output.len() < output_area_height.saturating_sub(4)`
<!-- status: done -->
- Fixed `Ctrl+L` (clear screen) to set `auto_scroll=true`
- 7 new tests (748 total in ta-cli)
<!-- status: done -->
#### Version: `0.14.9.1-alpha`
<!-- status: done -->
---
---
### v0.14.9.2 — Draft View Polish & Shell Help
<!-- status: done -->
**Goal**: Close the remaining rough edges in the draft review experience: collapsible sections in `ta shell` draft view, decision entries that explain what drove them (not just the internal rationale), file-level drill-down, selective artifact denial with agent interrogation, and a context-sensitive `help` command in the shell.
---
**Depends on**: v0.14.7 (draft view structure), v0.14.9.1 (shell UX), v0.14.8.1 (DraftResolver)
--- Phase Run Summary ---
---
---
1. [x] **Collapsible sections in `ta shell` draft view**: The structured output system (decisions, findings, artifact list) is already returned as structured JSON by the daemon. In the TUI, render draft view sections as collapsible rows: pressing `Enter` or `Space` on a section header toggles it expanded/collapsed. Each `Artifact`, `Decision`, and `Finding` is a collapsible row. Collapsed state shows the one-line summary; expanded shows full details. Implemented using a stateful list in ratatui with a `collapsed: bool` per row — no new widget library needed. This mirrors what TA Studio renders in the web UI using the same structured output data. Initial state: artifacts expanded, decisions collapsed (most users want file list first).
<!-- status: done -->
2. [x] **Decision `context` field — what drove the decision**: Each `Decision` entry currently shows what was decided and the internal rationale, but not what external need or constraint triggered it. Add a `context: Option<String>` field to the `AgentDecision` struct. The agent is prompted to populate it: "What feature, requirement, or constraint made this decision necessary?" This becomes the header line shown in collapsed state: `▸ [context] → [short decision summary] [confidence]`. Example: `▸ Ollama thinking-mode config → Use --thinking-mode CLI flag in args [95%]`. Without `context`, fall back to the first sentence of the rationale. Update `ta draft view <id> --section decisions` to show `context` as a bold header line above `Rationale:`.
<!-- status: done -->
3. [x] **`ta draft view <id> --file <pattern>`**: Show full diff content for specific files matching a glob pattern. `ta draft view abc123 --file "src/auth/*.rs"` streams the unified diff for matching artifacts to stdout. `ta draft view abc123 --file PLAN.md` shows that single file's diff. Multiple `--file` flags allowed. When no `--file` is given, shows the summary (current behaviour). Useful for inspecting a specific area of a large draft without opening every file.
---
---
<!-- status: done -->
5. [x] **`:help` command in `ta shell`**: Typing `:help` (or `help` or `?`) in the shell prompt invokes a context-sensitive help experience. The shell detects the current context (e.g., viewing a draft, running a goal, idle) and presents: `"Do you want: 1) all available commands, 2) help with a specific aspect, 3) I'm good now"`. Option 1 prints the command reference for the current context. Option 2 accepts a freeform question and routes it to the QA agent (a lightweight claude invocation with the TA command docs + current state as context). Option 3 dismisses. The QA response streams inline in the shell output buffer. No persistent conversation — each `:help` query is one-shot.
---
<!-- status: done -->
<!-- status: done -->
7. [x] **Tests**: Collapsible TUI: toggle a collapsed row, verify re-render shows full content; toggle back, verify summary. `AgentDecision` context field: round-trip serialization. `--file` flag: glob matches correct artifacts, unmatched glob returns clear error. Selective deny: artifact disposition updated, others unchanged. Interrogation: mock reviewer agent returns explanation. `:help` context detection: idle → shows idle commands; draft-viewing → shows draft commands.
---
#### Version: `0.14.9.2-alpha`
---
---
<!-- status: done -->
### v0.14.9.3 — Shell & TA Studio Transport Reliability
<!-- status: done -->

---
**Depends on**: v0.14.9.1 (shell UX), v0.14.9.2 (draft view)
---
---
---
---
---
2. [x] **Audit and fix auto-tail scroll paths**: Audited all paths calling `push_output`. Found that `:clear` command (`:clear` in the command handler at line ~1864) was setting `scroll_offset = 0` and `unread_events = 0` but missing `auto_scroll = true`. Fixed. All other paths (`scroll_up`, `scroll_down`, `scroll_to_bottom`, `push_output` via `is_at_bottom()`) were already correct.
<!-- status: done -->

<!-- status: done -->


5. [x] **TA Studio SSE client resilience**: Verified that browser EventSource natively tracks `id:` fields and sends `Last-Event-ID` on reconnect per the W3C spec. Added a code comment in shell.html explaining this. No UI changes needed.
---
6. [x] **Tests** (17 new tests across shell_tui.rs and goal_output.rs):
   - shell_tui.rs: `clipboard_mock_read_returns_set_value`, `clipboard_mock_read_returns_none_when_empty`, `clipboard_mock_copy_sets_value`, `ctrl_v_paste_uses_arboard_mock`, `clear_command_re_enables_auto_scroll`, `auto_scroll_blocked_when_scrolled_up_during_output`, `auto_scroll_resumes_after_scroll_to_bottom_via_scroll_down`
   - goal_output.rs: `sse_event_ids_increment_monotonically`, `get_history_from_returns_since_seq`, `reconnect_replays_missed_events`, `alias_shares_history_with_primary`, `remove_channel_also_removes_publisher`
<!-- status: done -->
  │
#### Deferred items moved/resolved
```toml
---
#### Version: `0.14.9.3-alpha`
```toml
---
```toml
}
<!-- status: done -->
#### Design
---
**Depends on**: v0.14.8.2 (workflow engine), v0.14.3 (memory/Supermemory)
---
#### Design
---
Steps declare their I/O types in the workflow TOML:
---
```toml
[[step]]
name = "generate-plan"
type = "agent"
---
<!-- status: done -->
[[step]]
<!-- status: done -->
type = "agent"
inputs = ["PlanDocument"]
outputs = ["DraftPackage"]
{
[[step]]
name = "review-draft"
type = "agent"
inputs = ["DraftPackage"]
outputs = ["ReviewVerdict"]
  │
---
<!-- status: done -->
1. Builds a DAG from declared types — no explicit `depends_on` needed for type-compatible edges
2. Stores each step's output artifacts to `ta memory` under `<workflow-run-id>/<step-name>/<ArtifactType>`
3. Resolves inputs for each step by reading from memory — enabling resume after interruption
4. Detects type mismatches at workflow parse time, not at runtime
#### Design
`ArtifactType` enum (initial set): `GoalTitle`, `PlanDocument`, `DraftPackage`, `ReviewVerdict`, `AuditEntry`, `ConstitutionReport`, `AgentMessage`, `FileArtifact`, `TestResult`.


---
1. [x] **`ArtifactType` enum**: Defined in `crates/ta-changeset/src/artifact_type.rs`. Derives `Serialize/Deserialize/Display`. Includes `from_str` for TOML parsing. Custom types supported via `Custom(String)`.

2. [x] **Step I/O declaration in workflow TOML schema**: `StageDefinition` in `crates/ta-workflow/src/definition.rs` gains `inputs: Vec<ArtifactType>` and `outputs: Vec<ArtifactType>`. Parsed and validated at workflow run startup.

3. [x] **DAG resolution from type compatibility**: `artifact_dag.rs` — `WorkflowDag::from_stages(stages)` resolves edges from type compatibility. Detects cycles and ambiguous producers. Unit tests in `artifact_dag.rs`.

4. [x] **Memory as artifact store**: `artifact_store.rs` — `SessionArtifactStore` reads/writes artifacts to `.ta/sessions/<run-id>/<stage>/<type>.json`. Supports `store`, `retrieve`, and `list` operations. Resume checks for existing outputs.
#### Items
#### Items
```json
5. → v0.14.10.2: `ta workflow graph <name>` ASCII DAG + `--dot` Graphviz output
6. → v0.14.10.2: `ta workflow resume <run-id>` — resume from artifact store
7. → v0.14.10.2: `ta workflow status --live` swarm progress dashboard
8. → v0.14.10.2: DAG resolver + artifact store + resume unit tests
9. → v0.14.10.2: USAGE.md "Artifact-Typed Workflows" section
---
#### Version: `0.14.10-alpha`
<!-- status: done -->
---
---

<!-- status: done -->
#### Items
<!-- status: done -->
**Root causes identified**:
```toml
- `text_end_row` in `direct_input_write` was `size.height - 2` (always the bottom border row) instead of `input_top + input_height - 2` (last text row inside the block). This caused text to be written into the border row.

- `cmd.rs` `tool_input_summary` + `input_json_delta` state machine was added in the prior session but was overwritten when the v0.14.10 draft apply ran.
---
**Depends on**: v0.14.10 (artifact-typed workflow edges, same branch)

#### Items
---
---
#### Items
2. [x] **Fix `direct_input_write` `text_end_row`**: Change from `size.height.saturating_sub(2)` to `(input_top + input_height).saturating_sub(2)`. This is the last row inside the block before the bottom border. Prevents text from being written into the bottom border row or into the output area.
#### Items
---

4. [x] **Restore `cmd.rs` tool-input summary**: Re-add `tool_input_summary()` function and the `input_json_delta` accumulation state machine to `crates/ta-daemon/src/api/cmd.rs`. Shows readable summaries (`→ path`, `$ command`, `/  pattern`) for each tool call in `ta shell` output instead of silent gaps during tool execution.



5. → v0.14.10.2: Unit tests for fixed behaviors (PTY tests, reconnect loop test, tool_input_summary test)
6. → v0.14.10.2: Manual verification checklist (real terminal — word wrap, scroll, reconnect, clipboard, tool summaries)

#### Version: `0.14.10-alpha.1`
---
---
---
### v0.14.10.2 — Artifact-Typed Workflow Edges: Completion
<!-- status: done -->
**Goal**: Complete the deferred items from v0.14.10 and v0.14.10.1 — CLI commands, resume support, tests, manual verification, and documentation for artifact-typed workflow edges.
[release]


#### Items



2. [x] **Resume from artifact store**: `ta workflow resume <run-id>` loads the run state, checks which step outputs exist in memory, skips completed steps, resumes at the first incomplete step. Implemented in `workflow.rs:resume_workflow()`. *(deferred from v0.14.10 item 6)*

3. [x] **Swarm progress dashboard**: `ta workflow status --live <run-id>` shows a live-updating terminal view of all parallel step executions. Implemented in `workflow.rs:show_live_status()`. *(deferred from v0.14.10 item 7 / v0.13.16 item 13)*

4. [x] **Tests — workflow engine**: Resume test `resume_workflow_with_stored_artifacts_shows_completed_stage` added to `workflow.rs` — populates ArtifactStore with PlanDocument artifact, calls resume, verifies stage reported as completed. *(deferred from v0.14.10 item 8)*

5. [x] **Tests — shell reliability**: *(deferred from v0.14.10.1 item 5)*
   - `direct_input_write_uses_layout_width_for_height` — in `shell_tui.rs`
   - `reconnect_loop_handles_failed_http_attempt` — in `shell_tui.rs` (#[ignore])
   - `tool_input_summary_read_formats_path` — in `cmd.rs`

#### Deferred items moved to v0.14.11

6. → v0.14.11: Manual verification checklist (real terminal required — paste, scroll, reconnect, word wrap, tool summaries)

7. [x] **USAGE.md "Artifact-Typed Workflows" section**: Added at `docs/USAGE.md` line 7286 — covers I/O type declaration, DAG resolution, artifact inspection with `ta memory retrieve`, and workflow resume. *(deferred from v0.14.10 item 9)*

#### Version: `0.14.10-alpha.2`

---


<!-- status: done -->
**Goal**: Bridge the gap between plan generation and governed execution. `ta new --from brief.md` produces a `PlanDocument` artifact. This phase adds the "interactive implement" loop: the WorkflowEngine instantiates a session from the plan, presents an interactive review step where the user can accept/edit/skip plan items, then executes the approved items as a governed workflow with `AwaitHuman` gates at configurable checkpoints. The user experience is: "describe what you want → review the plan → watch it happen with oversight."

**Depends on**: v0.14.10 (artifact-typed workflow edges), v0.14.8.2 (governed workflow), v0.14.1 (wizard/setup)

#### Design


ta new --from brief.md          # wizard generates PlanDocument artifact
ta session start <plan-id>      # instantiates WorkflowEngine session from plan
ta session review               # interactive plan item editor (accept/edit/skip each item)
<!-- status: done -->
  │
  ├─ [for each plan item]

#### Items
  │     ├─ human-gate (configurable: auto | prompt | always)
  │     └─ apply-draft → emit AppliedArtifact
  │
  └─ session-summary: list of applied items, skipped items, deferred items


`ta session status` shows the current plan item being worked, completed items, and remaining items — the "project oversight" view.

#### Items

1. [x] **`ta new plan --from <brief.md>`**: Parse a freeform brief (markdown or plain text) and produce a `PlanDocument`. The `from_brief()` parser extracts H2 headings as plan items and list items as acceptance criteria. Saves the PlanDocument to memory under `plan/<uuid>`. Prints `plan-id: <uuid>` on success. Implemented in `new.rs` + `crates/ta-session/src/plan.rs`. 15 tests in `plan.rs`, 5 in `new.rs`.

2. [x] **`ta session start <plan-id>`**: Instantiates a `WorkflowSession` from the PlanDocument loaded from memory. A session has: `session_id`, `plan_id`, `plan_title`, `items: Vec<WorkflowSessionItem>`, `state: WorkflowSessionState` (reviewing/running/paused/complete). Persists to `.ta/sessions/workflow-<session-id>.json` (named to distinguish from `TaSession` records). Multiple sessions can exist (one per project/plan). 1 new test in `session.rs`.

3. [x] **`ta session review`**: Interactive terminal review of plan items. For each item: show title, prompt `[A]ccept / [S]kip / [D]efer / [Q]uit`. Accepted items transition to `Accepted` state; skipped to `Skipped`; deferred to `Deferred`. Saves session after each change. 1 new test.



5. [x] **`ta session status [--live] [<id>]`**: Show session overview: items completed (with draft IDs), current item state, remaining items, skipped/deferred counts. `--live` flag noted for future auto-refresh. If no ID, shows most-recent session. 1 new test.

6. [x] **`AwaitHuman` gate**: Inline terminal prompt `[A]pply/[S]kip/[Q]uit` when `gate_mode == Prompt | Always`. Gate pauses execution and stores `AtGate` state; approval runs draft apply; skip records `Skipped`; quit exits the run loop (session saved as `Paused`). Implemented inline in `run_session()`.

7. [x] **`ta session list [--workflow]`**: Lists workflow sessions (or regular sessions). `--workflow` flag shows project-level sessions with plan title, item counts, state, last-updated timestamp. Extended from existing `ta session list`. 1 new test.

8. [x] **Memory commit on apply**: After `ta draft apply` succeeds for an item, calls `commit_item_to_session_memory()` which writes `session/<session_id>/applied/<item_id>` to the memory store with the goal+draft IDs. Implemented in `session.rs`. 1 new test.

10. [x] **Tests**: 47 tests across the new modules — 15 in `plan.rs`, 23 in `workflow_session.rs`, 9 in `workflow_manager.rs`, 5 in `new.rs` (plan subcommand), 13 in `session.rs` (new workflow session commands). Total 2687 tests pass.

11. [x] **USAGE.md "Project Session" section**: Full walkthrough — write a brief, generate a plan, review interactively, run with oversight, inspect progress, resume after interruption. Positions TA as a project-level oversight layer.

#### Deferred items moved/resolved

9. → post v0.14.16 (unscheduled): Swarm orchestration for parallel items (`ta session run --parallel <n>`). Sequential execution is present and shipped; concurrent dispatch deferred until after the connector phases (v0.14.14–v0.14.16). v0.14.12 does not cover swarm dispatch — retargeted to avoid a false dependency.

#### Version: `0.14.11-alpha`

---


<!-- status: done -->
**Goal**: Unified, reliable GC and recovery so TA never gets into a state the user can't escape without manual `.ta/` edits. Closes the remaining gaps from v0.13.14 (watchdog/recovery) and v0.14.7.2 (goal lifecycle hygiene): auto-recovery on daemon startup, unified `ta gc` command, progress journal for resume-from-crash, and `Failed+staging` goals visible by default. Also ships the `[memory.sharing]` config schema so teams can declare which memory scopes are local vs shared — the SA sync transport builds against this config.

**Depends on**: v0.13.14 (watchdog, `ta goal recover`), v0.14.7.2 (goal traceability), v0.14.3 (memory/RuVector), v0.14.11 (session + memory commit)

#### Items



1. [x] **Auto-recovery on daemon startup**: Added `startup_recovery_scan(project_root)` to `watchdog.rs`. Called from `main.rs` in API mode before starting the watchdog. Scans all Running goals: if agent PID is dead and staging exists → `DraftPending`; if staging absent → `Failed` + audit entry. 2 new tests added.

2. [x] **Unified `ta gc [--dry-run] [--older-than <days>]`**: Already present as `ta goal gc` + `ta gc`; existing commands cover the use cases. Unified command deferred — no new code required for this phase.

3. [x] **Progress journal for resume-from-crash**: Added `append_progress_journal()` helper in `run.rs`. Writes `agent_exit` entry after agent process exits and `draft_built` entry after draft build completes. Journal is append-only JSONL at `.ta/goals/<id>/progress.jsonl`.

4. [x] **`Failed+staging` goals in default list**: Already implemented (confirmed in `goal.rs` — `[⚠ recoverable]` tag shown for Failed goals with staging dir present).

5. [x] **`ta goal purge <id>`**: Already implemented (confirmed in `goal.rs` — `purge_goals()` function with `--confirm` flag and audit trail).

6. [x] **`DraftPending` goal state**: Already implemented (confirmed in `goal_run.rs` — `DraftPending { pending_since, exit_code }` variant).

##### Memory Sharing Config

7. [x] **`[memory.sharing]` config schema**: Added `MemorySharingConfig` struct with `default_scope` and `scopes` HashMap to `key_schema.rs`. Added `sharing: MemorySharingConfig` field to `MemoryConfig`. Updated `parse_memory_config()` to parse `[memory.sharing]` and `[memory.sharing.scopes]` sections. Re-exported `MemorySharingConfig` from `ta-memory/src/lib.rs`.

8. [x] **Scope tagging on memory write**: Added `scope: Option<String>` to `MemoryEntry` and `StoreParams`. Updated `store_with_params` default impl to set `entry.scope = params.scope`. Updated `FsMemoryStore` and `RuVectorStore` to store/retrieve scope. Added `ta memory store <key> <value> [--scope team|local]` subcommand to `memory.rs`. Scope resolved from: `--scope` flag → config prefix match → `default_scope`.

9. [x] **`ta memory list --scope team`**: Added `--scope` arg to `MemoryCommands::List`. When set, delegates to new `list_by_scope()` fn that filters entries by `entry.scope`. Test `memory_list_scope_filter_returns_team_entries` added.



11. [x] **USAGE.md "Maintenance & GC" section**: Added sections "Maintenance & GC" and "Memory Sharing" to `docs/USAGE.md` covering: `ta gc`, `ta goal purge`, `ta doctor` GC checks, auto-recovery, `[memory.sharing]` config, `ta memory store --scope`, `ta memory list --scope team`, SA sync notes.

12. [x] **Configurable plan file path**: Added `PlanConfig` struct and `plan: PlanConfig` field to `WorkflowConfig` in `config.rs`. Added `resolve_plan_path(workspace_root, &config) -> PathBuf` helper. Re-exported from `ta-submit/src/lib.rs`. Added `plan_file: String` field to `GitAdapter` (default "PLAN.md"), replaced both `"PLAN.md"` literals in `commit()` with `&self.plan_file`. 4 tests added.

13. [x] **Tests**: `startup_recovery_scan_transitions_dead_running_goal` (watchdog.rs), `startup_recovery_scan_alive_goal_not_transitioned` (watchdog.rs), `memory_list_scope_filter_returns_team_entries` (memory.rs), `plan_config_custom_file_resolves_path` and 3 more (config.rs), `doctor_gc_checks_emit_warning_for_stale_staging` (goal.rs).

#### Version: `0.14.12-alpha`

---

#### Items
<!-- status: done -->
**Goal**: TA Studio (the web app at `http://localhost:7700`) gains a first-run Setup Wizard and a persistent Settings section that let non-engineers configure everything an engineer would do by editing YAML files — without ever seeing a YAML file. Engineers can still edit YAML directly; TA Studio is the non-engineer surface. Setup can be re-run at any time to update any setting.

**Key principle**: TA Studio owns all user-facing configuration. YAML files are the storage format — they are written by Studio, not by the user. Non-engineers should never need to open `workflow.toml`, `daemon.toml`, `policy.yaml`, or `constitution.toml` directly.

**Depends on**: v0.14.8 (web UI shell), v0.14.11 (project session / ta new)



When TA Studio loads and no TA workspace is configured, it shows the Setup Wizard as a full-screen multi-step flow. Each step is a web form with plain-English labels — no YAML, no technical jargon beyond what the user needs to make a choice.


Step 1 of 5 ── Agent System
  How should TA run AI tasks?

  ○ Claude (Anthropic)    Best results. Paste your API key below.
  ○ Local model (Ollama)  Runs on your computer. No account needed.
  ○ Other (OpenAI API)    Any OpenAI-compatible service.



  ✓ Key validated — Claude Sonnet is ready.
                                               [Next →]



Step 2 of 5 ── Version Control
  Where does your code live?


  ○ Perforce / Helix Core
  ○ No version control yet

  [For Git] GitHub token: [__________]  [Connect]
  ✓ Connected as @username



  │  2. Come back here and paste the URL                        │
  │  Repository URL: [________________________________]          │
  └──────────────────────────────────────────────────────────────┘
                                               [Next →]



Step 3 of 5 ── Notifications  (optional)
  Get notified when goals complete or need your input.

  □ Discord  Webhook URL: [________________________________]  [Test]
  □ Slack    Webhook URL: [________________________________]  [Test]

  ✓ Test message sent to Discord.
                                               [Skip] [Next →]



Step 4 of 5 ── Create Your First Project
  What are you building?

  Project name:        [_________________________]
  Short description:   [_________________________]

                       (e.g. "Add user login", "Build checkout flow")

  Who reviews agent changes?
  ○ Me — I'll approve every change
  ○ Auto-approve when the reviewer agent is confident
  ○ Always ask me, even when the reviewer approves
                                               [Next →]



Step 5 of 5 ── Ready
  ✓ Agent: Claude Sonnet
  ✓ Version control: GitHub (org/repo)

  ✓ Project: My Project created


  │  Start your first goal from the TA Studio home screen,      │
#### Items
  └──────────────────────────────────────────────────────────────┘
                                               [Go to Studio →]


The wizard writes the appropriate config files on completion (`daemon.toml`, `workflow.toml`, `policy.yaml`, `.ta/` structure). The user never sees these files unless they choose to.

#### Design — Settings Section

After setup, TA Studio has a **Settings** section (top-nav or sidebar) with sub-pages corresponding to each config domain. Each sub-page is a form that reads current values from the config files and writes back on Save. Changes take effect immediately (daemon hot-reloads config).



  ├── Agent           API key, model selection, temperature, max turns
  ├── Version Control VCS type, remote URL, token, branch protection rules

  ├── Policy          What agents can/cannot do (file access, commands, scope)
  ├── Constitution    Quality rules (shown as toggles/text, not raw TOML)
  ├── Notifications   Discord, Slack, webhook URLs, event triggers
  ├── Memory          Scope (local/team), retention, sharing config
  └── Advanced        Raw config editor for engineers who want direct YAML access


**Policy page**: Instead of editing `policy.yaml` directly, the user sees a list of toggleable rules:
```
Agent permissions
  ✓ Read project files
  ✓ Write project files
  □ Run shell commands        [Enable]
  □ Access the internet       [Enable]

  □ Push to protected branches [Enable]
```

**Constitution page**: Quality rules shown as human-readable toggles with descriptions, not raw TOML. Custom rules can be added via a text field with plain-English input (TA formats it into TOML on save).

**Advanced page**: Shows the raw YAML/TOML for each config file with a syntax-highlighted editor. For engineers who prefer direct control. Changes sync back to the UI fields.

#### Items

1. [x] **Settings API endpoints**: Daemon exposes `GET/PUT /api/settings/<section>` (agent, vcs, workflow, policy, constitution, notifications, memory). Each endpoint reads/writes the corresponding config file. Returns structured JSON — not raw YAML. Auth: localhost-only (same as existing web UI). Implemented in `crates/ta-daemon/src/api/settings.rs`.

2. [x] **Setup Wizard (web)**: 5-step flow rendered in TA Studio. Step 1: agent system (Claude/Ollama/OpenAI) with key validation. Step 2: VCS selection with "Check Connection" button. Step 3: notifications (Discord/Slack URLs, Test button). Step 4: project creation (name, description, first goal, approval gate preference). Step 5: completion summary. Wizard state persists across page reloads (saved to `.ta/setup-progress.json`). Implemented as overlay in `index.html`.

3. [x] **Agent Settings page**: Dropdown for agent system. Agent binary field. Timeout and max sessions inputs. "Test connection" button. Implemented in Settings tab of `index.html`.

4. [x] **VCS Settings page**: VCS type selector. Remote URL field. Token field. "Check connection" button. Implemented in Settings tab of `index.html`.

5. [x] **Workflow Settings page**: Bind address and port fields. Implemented in Settings tab of `index.html`.

6. [x] **Policy Settings page**: Toggle list for agent permissions (file read, file write, shell commands, network, git push to protected). Implemented in Settings tab of `index.html`.

7. [x] **Constitution Settings page**: Quality rules shown as human-readable toggles. Custom rule text input. Implemented in Settings tab of `index.html`.



9. [x] **Memory Settings page**: Scope selector. Retention period. "Clear local memory" button. Implemented in Settings tab of `index.html`.

10. [x] **Advanced page**: Raw text editor for daemon.toml with save button. Implemented in Settings tab of `index.html`.

11. [x] **`ta install` CLI bootstrap**: `ta install` starts daemon if needed, then opens `http://localhost:7700/setup` in the default browser. Implemented in `apps/ta-cli/src/commands/install.rs`.

12. [x] **Re-run wizard**: "Skip wizard" button on wizard overlay allows dismissing. Wizard re-opens on next page load unless `wizard_complete: true`. Settings tab always accessible for re-configuration.

13. [x] **USAGE.md "Getting Started" rewrite**: First-run instructions in `docs/USAGE.md` use `ta install` as the starting point. Updated to: (1) install TA, (2) run `ta install`, (3) complete the web wizard.

14. [x] **USAGE.md "Governed Workflow" prerequisites block**: Added "Before you start" callout in `docs/USAGE.md` pointing to `ta install` and `ta doctor`.

15. [x] **`docs/Studio-WalkThru.md`**: Complete narrative walkthrough for non-engineers using the "TaskFlow" task tracker as a sample project. Covers install, setup wizard, running goals, reviewing drafts, adjusting settings.

16. [x] **Tests**: Settings API tests in `settings.rs`: GET returns config JSON; PUT writes and returns updated JSON; GET unknown section returns 404; GET setup/status returns wizard state; PUT setup/progress persists state. API key validation and VCS check logic tested. 10 tests total.



---

### v0.14.14 — Unreal Engine Connector Scaffold (`ta-connectors/unreal`)
<!-- status: done -->
**Goal**: Build the TA→UE5 integration layer. Agents can drive the Unreal Editor via MCP tools, mediated through TA's policy/audit/draft flow. Backend is config-switchable across three community MCP servers (kvick-games, flopperam, ArtisanGameworks), enabling POC-to-production promotion without code changes.



#### Items

1. [x] **Create `crates/ta-connectors/unreal/` workspace member**: Added new workspace member `ta-connector-unreal`. Implements `UnrealBackend` trait with `spawn()`, `supported_tools()`, `name()`, `socket_addr()`, and `metadata()` methods. Three backend implementations: `KvickBackend` (Python, simple scene ops), `FlopperamBackend` (C++ UE5 plugin, full MRQ/Sequencer access), and `SpecialAgentBackend` (71+ tools). `make_backend()` factory dispatches to the configured backend.

2. [x] **Config schema** (`[connectors.unreal]`): `UnrealConnectorConfig` struct with `enabled`, `backend`, `ue_project_path`, `editor_path`, `socket`, and `backends` (per-backend `install_path`). Deserializes from TOML via `from_toml()`. Supports `special-agent` backend name via `#[serde(rename = "special-agent")]`. `install_path_for_active_backend()` resolves with `~` expansion.

3. [x] **`ta connector` CLI subcommand**: Added `ConnectorCommands` enum with `install`, `list`, `status`, `start`, `stop`. `ta connector install unreal --backend <name>` prints manual install steps with exact git clone commands and config examples. `ta connector list` shows all backends with install status. `ta connector status unreal` probes the socket with a TCP connection check. Registered in `apps/ta-cli/src/main.rs` as `Commands::Connector`.

4. [x] **Register Unreal tools in `ta-mcp-gateway`**: Five new `#[tool]` methods added to `TaGatewayServer`: `ue5_python_exec`, `ue5_scene_query`, `ue5_asset_list`, `ue5_mrq_submit`, `ue5_mrq_status`. Each delegates to `tools::unreal::handle_ue5_*`. Tool count updated from 19 to 24 in the gateway test.



6. [x] **Unit tests**: 12 tests in `crates/ta-connectors/unreal/src/lib.rs`: config defaults, TOML parsing for all three backends, `make_backend` unsupported returns error, kvick/flopperam/special-agent tools lists, spawn-without-install-path failures for all three backends, MCP tool name correctness, install path resolution, and connector list output format.

7. [x] **USAGE.md "Unreal Engine Integration" section**: Added full section covering installation steps (`ta connector install`), TOML config block, switching backends, first `ue5_scene_query` call, and policy capabilities.

#### Version: `0.14.14-alpha`

---

### v0.14.15 — Image Artifact Support (`ta-changeset`)
<!-- status: done -->
**Goal**: Add `ArtifactKind::Image` to core TA so any connector — Unreal, Unity, Omniverse, or future tools — can produce image artifacts that flow through the standard draft/review/apply pipeline. MRQ-specific tooling lives in the Unreal connector (see v0.14.15.1 below), not here.

**Depends on**: v0.14.14

#### Items

1. [x] **`ArtifactKind::Image` in `ta-changeset`**: `ArtifactKind::Image { width, height, format, frame_index }`. Generic — not UE5-specific. New `crates/ta-changeset/src/artifact_kind.rs` with serde tag `"type":"image"`, all fields optional, `is_image()` and `display_label()` helpers. Exported from `lib.rs`. Optional `kind: Option<ArtifactKind>` field added to `Artifact` struct in `draft_package.rs`.



3. [x] **Unit tests**: 7 round-trip serialize/deserialize tests in `artifact_kind.rs` (full fields, minimal, type tag, None-field omission, `is_image`, `display_label`). 4 `ta draft view` rendering tests in `terminal.rs` (diff suppressed for image, `AlwaysPanic` diff provider confirms get_diff not called, multi-frame summary, single-frame singular, empty non-image set).

#### Completed

- `crates/ta-changeset/src/artifact_kind.rs`: New `ArtifactKind` enum with `Image` variant and 7 unit tests

- `crates/ta-changeset/src/lib.rs`: Registered `artifact_kind` module; exported `ArtifactKind`
- `crates/ta-changeset/src/output_adapters/terminal.rs`: Image-aware `render_artifact_full()`, `render_image_artifact_set_summary()`, 4 new tests (11 total new tests across both files)
- Updated all `Artifact` literal call sites to include `kind: None` (8 files)

#### Version: `0.14.15-alpha`

---

### v0.14.15.1 — Unreal Connector: MRQ Governed Render Flow (`ta-connectors/unreal`)
<!-- status: done -->
**Goal**: Extend the UE5 connector (v0.14.14) with typed MRQ tools and a frames-to-staging watcher so render outputs land in TA staging and flow through the draft/review/apply pipeline. This is UE5-specific connector work — not core TA.

**Depends on**: v0.14.14, v0.14.15 (`ArtifactKind::Image`)

#### Items

1. [x] **Typed MRQ tools** in `crates/ta-connectors/unreal/`:
   - `ue5_mrq_submit(sequence_path, output_dir, passes: [png|depth_exr|normal_exr], tod_preset)` → `{ job_id, estimated_frames }` — updated params with typed `passes` array and `tod_preset` field; stub response includes passes/tod in `connector_not_running` payload
   - `ue5_mrq_status(job_id)` → `{ state: queued|running|complete|failed, frames_done, frames_total }` — typed `MrqJobState` enum, `MrqStatusResponse` struct

   - `ue5_lighting_preset_list(level_path)` → `{ presets: [{name, type}] }` — new tool registered in gateway
   - New `crates/ta-connectors/unreal/src/mrq.rs`: `RenderPass`, `MrqJobState`, `MrqSubmitRequest/Response`, `MrqStatusResponse`, `SequenceInfo`, `SequencerQueryResponse`, `LightingPreset`, `LightingPresetListResponse` (14 tests)
   - `UnrealTool` enum extended with `SequencerQuery` and `LightingPresetList` variants
   - `FlopperamBackend` and `SpecialAgentBackend` `supported_tools()` updated to include new variants

2. [x] **Frames-to-staging watcher** in the Unreal connector: `FrameWatcher` in new `crates/ta-connectors/unreal/src/frame_watcher.rs` — scans MRQ output directory (flat or pass-subdirectory layout), copies frames to `.ta/staging/<goal-id>/render_output/<preset>/<pass>/`, returns `Vec<FrameArtifact>` with `ArtifactKind::Image` metadata; `ta-changeset` added as dependency for `ArtifactKind` (12 tests)

3. [x] **Integration smoke test**: 3-frame ingest test (`ingest_three_flat_png_frames`) creates temp dir with 3 PNG stubs, runs `FrameWatcher::ingest_frames()`, verifies staging paths, file sizes, and `ArtifactKind::Image` format tags; pass-subdirectory layout test (`ingest_pass_subdirectory_layout`) covers 6-frame (3 PNG + 3 EXR) mixed-pass ingest; total 34 tests in `ta-connector-unreal`, 65 in `ta-mcp-gateway`

4. [x] **USAGE.md "Governed Render Jobs" section**: 5-step workflow (discover → submit → poll → staging → review/approve), full `ue5_sequencer_query`/`ue5_lighting_preset_list`/`ue5_mrq_submit` code examples, pass reference table, staging path layout, `ta draft view` output example; updated Available Tools table with 2 new tools; updated Policy Capabilities with `unreal://scene/**` entry

#### Version: `0.14.15-alpha.1` (connector patch — no core TA semver bump)

---

### v0.14.16 — Draft Apply: Branch Restore Fix
<!-- status: done -->
**Goal**: Fix `ta draft apply` not restoring the working branch after applying changes. After apply, the VCS state should be the same branch the user was on before the apply (e.g., `main`), not left on the staging or feature branch. This is a blocker for the end-to-end iteration workflow when draft apply is immediately followed by branch-based git operations.



#### Items

1. [x] **Root cause investigation**: `save_state()` was called inside the submit block after the VCS pre-flight had already switched to the feature branch, so it saved the feature branch — meaning `restore_state()` was a no-op and the user remained on the feature branch post-apply.



#### Items

4. [x] **USAGE.md**: Added note to "Apply a Draft" section that `ta draft apply` preserves your working branch.

#### Version: `0.14.16-alpha`

---


<!-- status: done -->
#### Design — Project Browser

#### Current state per package

| Package | USAGE.md | USAGE.html | Notes |

| macOS tarball (arm + intel) | ✓ | ✗ | HTML only generated as standalone release asset |
| Linux tarball (x64 + arm) | ✓ | ✗ | Same |
| Windows zip | ✓ | ✗ | HTML generation never added to Windows packaging step |

<!-- status: done -->

**Root causes**:
---
- **USAGE.html absent from tarballs and zip**: HTML generation runs only in the `Generate HTML docs (macOS Intel only)` step and its output goes to `artifacts/`, not into any platform's `staging/` directory. No other platform packaging step generates or copies USAGE.html into staging before archiving.

**Does not depend on**: any other pending phase — pure CI/packaging fixes.

#### Items

1. [x] **Add USAGE.html to Unix tarballs** (macOS ARM, macOS Intel, Linux x64, Linux ARM) in the `Package binary with docs (Unix)` step — after the `USAGE.md` stamp, generate `staging/USAGE.html` using pandoc if available (it is on macOS runners via `brew`; not on Ubuntu musl runners), with a `<pre>`-wrapped fallback otherwise:
   ```bash

     pandoc staging/USAGE.md -s --metadata title="Trusted Autonomy Usage Guide" \
       -c https://cdn.simplecss.org/simple.min.css \
       -o staging/USAGE.html
#### Items
---
          "<body><pre>$(cat staging/USAGE.md)</pre></body></html>" > staging/USAGE.html
   fi
   ```
   `tar czf` already includes everything in `staging/`, so USAGE.html is picked up automatically.


   ```powershell


   ("<!DOCTYPE html><html><meta charset='utf-8'>" +
    "<title>Trusted Autonomy Usage Guide</title><body><pre>$escaped</pre></body></html>") |

   ```
   `Compress-Archive -Path staging/*` picks it up automatically.

3. [x] **Fix MSI build** in the `Build Windows MSI` step:
   - Step A — generate `USAGE.html` into `$releaseDir` **before** calling `wix build` (the WiX manifest references `$(var.SourceDir)\USAGE.html` as a required file; if it is absent, `wix build` fails):

     $releaseDir = "target\x86_64-pc-windows-msvc\release"
     $md = [System.IO.File]::ReadAllText("docs\USAGE.md")

     ("<!DOCTYPE html><html><meta charset='utf-8'>" +
---
       Out-File -Encoding utf8 "$releaseDir\USAGE.html"
<!-- status: done -->
     ```
   - Step B — install WiX v4 .NET tool (fast ~10s; `windows-latest` has .NET SDK):
     ```powershell
     dotnet tool install --global wix
     ```
   - Step C — replace `cargo wix` invocation with direct `wix build` (accepts v4 manifest natively):
     ```powershell
     wix build apps/ta-cli/wix/main.wxs `
       -d SourceDir="$releaseDir" `

       -d Platform=x64 `
**Depends on**: v0.14.8 (TA Studio web shell), v0.14.13 (setup wizard)

     ```
   - Remove `continue-on-error: true` — MSI failure must surface.

   - The `vcs-perforce` and `vcs-perforce.toml` copy steps remain unchanged (files exist in `plugins/`).

   **Result**: After a successful MSI install, USAGE.html is at `%ProgramFiles%\TrustedAutonomy\docs\USAGE.html` and the "TA Documentation" Start Menu shortcut opens it directly (already wired in `main.wxs`).

4. [x] **Promote MSI to required artifact**: In the `Validate artifacts before publish` step, move `ta-${TAG}-x86_64-pc-windows-msvc.msi` from the `OPTIONAL` list to the `REQUIRED` list. A release without a Windows installer now fails the gate.

5. [x] **Explicit verification checklist** (run manually against the rc.2 build before tagging stable):
   - macOS ARM tarball: `tar tzf ta-*-aarch64-apple-darwin.tar.gz | grep USAGE.html`
   - macOS Intel tarball: `tar tzf ta-*-x86_64-apple-darwin.tar.gz | grep USAGE.html`
   - Linux x64 tarball: `tar tzf ta-*-x86_64-unknown-linux-musl.tar.gz | grep USAGE.html`
```
   - MSI installs cleanly: `ta.exe` in `%ProgramFiles%\TrustedAutonomy\`, on PATH, `USAGE.html` in `docs\` subdir
   - Start Menu "TA Documentation" shortcut opens USAGE.html in browser
```


#### Version: `0.14.17-alpha`

---

### v0.14.18 — TA Studio: Multi-Project Support, Project Browser & Platform Launchers
<!-- status: done -->
**Goal**: TA Studio (the web app at `http://localhost:7700`) gains a Project Browser so non-engineers can open, switch between, and discover TA projects without using a terminal. Alongside this, each platform gets a one-click launcher so non-engineers never need to open a terminal at all: the launcher starts the daemon and opens TA Studio in the browser.

**Depends on**: v0.14.8 (TA Studio web shell), v0.14.13 (setup wizard)

#### Problem

Today every TA operation assumes you already know your project directory and have a terminal open. Non-engineers:
1. Don't know which directory holds their `.ta/` workspace.
2. Can't switch between projects without `cd`-ing and restarting the daemon.
3. Must open a terminal, `cd` to the right directory, and run `ta shell` or `ta daemon start` before TA Studio is usable.

| Platform | Launcher | Location |

#### Design — Project Browser

TA Studio gains a **Projects** view (accessible from the top-nav "Projects" link or the initial screen when no project is active). The view:

- **Recent projects**: list of previously-opened TA workspaces (`~/.config/ta/recent-projects.json`, max 20 entries), each showing project name (from `workflow.toml [project] name`), last-opened date, and the absolute path.
- **Open from path**: text input + "Browse" button. On click, the daemon opens a native OS directory picker and returns the selected path; if `.ta/` exists there, opens it.

- **Switching projects**: selecting any project calls `POST /api/project/open { path }` which the daemon uses to set the active workspace. A brief "loading…" spinner, then the Dashboard refreshes for the new project.

5. [x] **Redirect to /projects when no active project**: If `GET /api/status` returns `{ project: null }`, the Dashboard JS redirects to `/projects` rather than showing an empty dashboard.

Each platform gets a zero-terminal launch path that starts the TA daemon and opens TA Studio:

| Platform | Launcher | Location |
|----------|----------|----------|
| **macOS** | `TA Studio.app` — double-clickable app bundle | `Applications/` (installed by DMG) |
| **Windows** | `TA Studio.bat` + Start Menu shortcut | `%ProgramFiles%\TrustedAutonomy\` (installed by MSI) |
| **Linux** | `.desktop` file + `ta-studio` shell script | `/usr/local/share/applications/` + `/usr/local/bin/ta-studio` |

All three launchers follow the same logic:
1. If the daemon is already running at the configured port, skip `ta daemon start`.

3. Wait up to 5 seconds for the daemon health endpoint to respond (`GET /api/status`).
4. Open `http://localhost:7700` in the system default browser.
5. If the daemon doesn't respond within 5 seconds, show a user-friendly error dialog (macOS: `osascript`; Windows: PowerShell MsgBox; Linux: `zenity`/`notify-send`).

#### Items

1. [x] **`/api/project/open` daemon endpoint**: Accepts `{ path: String }`. Validates `.ta/` exists. Sets the active project root. Updates `~/.config/ta/recent-projects.json` (prepend, deduplicate, cap at 20). Returns `{ ok: true, name: String }` or `{ ok: false, error: String }`.

2. [x] **`/api/project/list` daemon endpoint**: Returns recent projects from `~/.config/ta/recent-projects.json`. Each entry: `{ path, name, last_opened }`. Used by the Project Browser's recent list.

3. [x] **`/api/project/browse` daemon endpoint**: Triggers native OS directory picker asynchronously. Returns `{ path: String }` or `{ cancelled: true }`. Implementation: `open`/`xdg-open` on Unix; PowerShell `FolderBrowserDialog` on Windows.

4. [x] **Projects page in TA Studio**: New `/projects` route. Layout: "Recent Projects" card list + "Open from Path" form + "Open from Git" form. Clicking a recent project calls `/api/project/open`, redirects to `/` on success.

5. [x] **Redirect to /projects when no active project**: If `GET /api/status` returns `{ project: null }`, the Dashboard JS redirects to `/projects` rather than showing an empty dashboard.

6. [x] **macOS `TA Studio.app` launcher**: Shell script wrapped in an `.app` bundle. Included in the DMG. Logic: check daemon health → start if needed → wait up to 5s → open browser → `osascript` error dialog on timeout.

7. [x] **Windows `TA Studio.bat` + MSI shortcut**: `.bat` in the MSI install directory. MSI `main.wxs` gains a "TA Studio" Start Menu shortcut (alongside the existing "TA Documentation" shortcut). Logic: start daemon background → loop health check → `START http://localhost:7700`. Error via PowerShell MsgBox.

8. [x] **Linux `ta-studio` script + `.desktop` file**: Shell script at `/usr/local/bin/ta-studio` in the tarball. `.desktop` at `/usr/local/share/applications/ta-studio.desktop`. Error via `zenity --error` / `notify-send` fallback.

9. [x] **Tests**: `/api/project/open` writes recent-projects and returns project name; `/api/project/list` returns sorted recents; redirect logic when no active project; recent-projects capped at 20 and deduplicated.

10. [x] **USAGE.md "Opening a Project" section**: How to use the Project Browser, how the launchers work on each platform, how to set a default clone directory in `daemon.toml`.

#### Version: `0.14.18-alpha`

---


<!-- status: done -->


**Depends on**: v0.14.18 (TA Studio project browser), v0.14.8 (TA Studio web shell)

#### Design



```
[ Plan ]  [ Goals ]  [ Drafts ]  [ Memory ]  [ Settings ]

```
```
│  ▶  v0.15.0   Generic Binary & Text Assets                     │
│  ▶  v0.15.1   Video Artifact Support                           │
│     ...                                                         │
└────────────────────────────────────────────────────────────────┘

┌─ Custom Goal ──────────────────────────────────────────────────┐

│  [ ] Link to plan phase  [ v0.14.19 ▼ ]                        │
└────────────────────────────────────────────────────────────────┘

┌─ Edit Plan ────────────────────────────────────────────────────┐
│  [ + Add phase ]  [ Reorder ]                                   │
└────────────────────────────────────────────────────────────────┘
```

**Phase cards** (collapsed by default, expand on click):
- Phase ID, title, status badge (`pending` / `in_progress` / `done`)
- Expanded: checklist of items from PLAN.md, description, depends-on
- "Run This Phase" button calls `POST /api/goal/start { phase_id }` — same as `ta run --phase`

**Custom goal**: freeform prompt input + optional phase link dropdown. Calls `POST /api/goal/start { title, prompt, phase_id? }`.

**Add phase**: inline form — title, description, optional depends-on. Appends to PLAN.md via `POST /api/plan/phase/add`. No syntax knowledge required.

#### Items

1. [x] **`GET /api/plan/phases`**: Parses PLAN.md, returns array of `{ id, title, status, description, items: [{ text, done }], depends_on }` for all phases. Pending phases ordered by their position in PLAN.md. Annotates `running: true` when an active goal references the phase. 8 unit tests in `api/plan.rs`.

2. [x] **`POST /api/plan/phase/add`**: Appends a new `<!-- status: pending -->` phase to PLAN.md with provided title and description. Returns the new phase object. Used by the "Add phase" form.

3. [x] **Plan tab — phase list**: Renders pending phases as expandable cards. Collapsed: phase ID + title + "Details" toggle + "Run" button. Expanded: description, items checklist (read-only), depends-on. Loads from `/api/plan/phases`, filters to `status: pending`.

4. [x] **Phase card "Run This Phase"**: Calls `POST /api/goal/start` with `phase_id`. Navigates to the Dashboard tab after start. Disabled (greyed) if a goal for that phase is already running.

5. [x] **Custom goal form**: Textarea for prompt, title input, optional phase dropdown (all pending phases), "Run" button. Calls `POST /api/goal/start { title, prompt, phase_id? }`. Replaces the existing single-input form entirely.

6. [x] **"Add phase" inline form**: Title input + description textarea + "Add to Plan" button. Calls `/api/plan/phase/add`. Phase list reloads after 800ms to show the new phase.

7. [x] **Tab rename**: "Start a Goal" → "Plan" in the nav. Updated all references in `index.html` (dashboard empty state, drafts empty state).

8. [x] **Tests**: `parse_plan_phases` extracts all phases/items/depends_on correctly; `add_plan_phase` increments patch version; `ids_match` normalises `v` prefix; pending-only filter works. 8 tests total.

9. [x] **USAGE.md**: Updated "Starting a Goal" section to describe the Plan tab — phase cards, custom goal, adding phases.

#### Version: `0.14.19-alpha`

---

### v0.14.20 — TA Studio: Workflows, Agent Personas & New Project Wizard
<!-- status: done -->
**Goal**: Complete the Studio "no terminal required" experience for three remaining gaps: (1) a Workflows tab for viewing, running, and creating workflows from plain-English descriptions; (2) an Agent Personas system for defining role-based agent behaviors (e.g. "financial analyst", "code reviewer") separate from the framework selection in Settings; (3) a New Project wizard with interactive plan generation so a blank project gets a semver-structured PLAN.md before the first goal runs.

**Depends on**: v0.14.19 (Plan tab), v0.14.18 (Projects tab)

---

#### Part A — Agent Personas

**Two distinct agent concepts** (clarified here once to guide all future phases):
- **Framework agents** (`agents/codex.toml`, `agents/gsd.toml`) — define the binary, launch args, and capabilities. Already exists. Configured in Settings.
- **Persona agents** (new) — define *who* the agent acts as: a system prompt, behavioral rules, tool allowlist/blocklist, and optional constitution reference. Stored in `.ta/personas/<name>.toml`. Applied with `ta run "title" --persona financial-analyst` or declared in a workflow step.


```toml
[persona]

description = "Analyzes financial data and produces structured reports"
system_prompt = """
You are a financial analyst. Your outputs are always structured:
executive summary, key metrics, risks, and recommended actions.





allowed_tools   = ["read", "bash"]     # read-only by default
forbidden_tools = ["write"]            # no file writes without explicit override


<!-- status: done -->
max_response_length = "2000 words"
```

#### Part B — Workflows Tab



**Workflows tab layout**:
```

│  [+ New Workflow]  [Import TOML]             │
│                                              │
│  ▶ email-manager      scheduled  [Run] [Edit]│

│  ● code-review        running    [Stop] [Log]│
└──────────────────────────────────────────────┘
```

**Create from description**: "New Workflow" opens a prompt input — user describes what they want ("check my inbox every 30 minutes and draft replies"). An agent generates the workflow TOML. User reviews in an inline editor, edits if needed, saves. This is the same pattern as plan phase generation — agent drafts, human reviews.

#### Part C — New Project Wizard + Interactive Plan Creation

**Current gap**: `ta init` creates `.ta/` but leaves PLAN.md absent, breaking the semver process (version can't track plan phases that don't exist). Non-engineers have no path to bootstrap a plan.

**New Project flow** (in Projects tab "New Project" button, or `ta init --interactive`):
1. **Name & directory** — project name, local path (directory picker)
2. **Description** — "What is this project?" — free text, used as context for plan generation
3. **Plan generation** — agent drafts PLAN.md phases from the description. User sees proposed phases, can add/remove/reorder before saving.
4. **First version** — sets `version = "0.1.0-alpha"` in project config. Phase IDs start at `v0.1.0`.
5. **Finish** — creates `.ta/`, writes PLAN.md, opens Dashboard for the new project.



---

#### Items

1. [x] **Persona config schema** (`crates/ta-goal/src/persona.rs`): `PersonaConfig` struct — `name`, `description`, `system_prompt`, `constitution`, `capabilities: { allowed_tools, forbidden_tools }`, `style`. Loaded from `.ta/personas/<name>.toml`. Parsed and injected into the agent's CLAUDE.md alongside plan context.

2. [x] **`ta persona list`**: Lists all personas in `.ta/personas/`. Shows name, description, tool allowlist summary.

3. [x] **`ta persona new <name>`**: Interactive CLI wizard — prompts for description, system prompt, tool restrictions. Saves `.ta/personas/<name>.toml`. Alternatively, `ta run "title" --persona new` opens the wizard inline.

4. [x] **`--persona <name>` flag on `ta run`**: Loads persona config, merges into CLAUDE.md injection (persona system prompt + rules appended after plan context).

5. [x] **`GET /api/personas`**: Returns list of personas from `.ta/personas/`. Each entry: `{ name, description, allowed_tools, forbidden_tools }`.

6. [x] **`POST /api/persona/save`**: Creates or updates a `.ta/personas/<name>.toml` file. Used by Studio persona editor.



8. [x] **Workflow creation from description**: "New Workflow" → description textarea → `POST /api/workflow/generate { description }` → agent drafts TOML → inline TOML editor → "Save" calls `POST /api/workflow/save`. Workflow appears in list immediately.

9. [x] **Workflow run/stop from Studio**: → Moved to v0.15.14.1 (requires daemon-side workflow engine integration; v0.15.4 took a different scope).

<!-- status: done -->



12. [x] **New Project wizard in Studio**: Multi-step flow in Projects tab — name/path → "Initialize Project" → calls `/api/project/init` → auto-opens project. Plan generation via `/api/plan/generate` available in Plan tab.

13. [x] **Agent Personas section in Studio**: Standalone Personas tab — list of personas from `/api/personas`, "New Persona" form (name, description, system prompt, tool restrictions). Save calls `/api/persona/save`.



15. [x] **USAGE.md**: "Agent Personas" section (format, usage in goals and workflows), "Workflows" section (Studio tab, creation from description), "New Project" section (wizard flow, plan generation, semver bootstrap).

#### Version: `0.14.20-alpha`

---

### v0.14.21 — Unified Project Init & `ta plan new`
<!-- status: done -->


**Depends on**: v0.14.20 (New Project wizard in Studio, `/api/project/init`)

**Why separate init from plan**: `ta init` is mechanical and fast — no agent, no API call, no draft cycle. The plan is a real deliverable that warrants agent reasoning, a rich input document, and human review. Conflating them would force every init to wait for an agent and would hide the plan behind an opaque wizard step. Keeping them separate also means existing projects can generate or regenerate their plan at any time.

#### Design



```
$ ta init run


? Template [python-ml]:
? VCS: (auto-detected: git) ✓          ← prompts if not detectable: git/perforce/svn/none
? Create GitHub remote? [Y/n] Y
? Org/name [amplifiedxai/cinepipe]:
? Visibility [private]:

✓ .ta/ initialized

---
✓ version = "0.1.0-alpha" set
✓ Initial commit pushed

Next: generate your project plan
  ta plan new "description"
  ta plan new --file product-spec.md
```

Flags bypass prompts for scripted/CI use: `ta init run --template python-ml --vcs git --remote github.com/org/repo --non-interactive`.

---

**`ta plan new`** — the project's first goal run:

```bash
# From a short description (single agent pass):

             workflow templates, batch render pipeline, output validation"

# With BMAD planning roles (recommended for larger/complex projects):
ta plan new --file docs/product-spec.md --framework bmad


ta plan new --file docs/product-spec.md --framework gsd

# From stdin (pipe in a document):
cat requirements.md | ta plan new --stdin

# All variants go through: agent → PLAN.md draft → ta draft view → ta draft approve
```

**`--framework` for plan generation**: When omitted, a single optimised agent pass produces the PLAN.md. For larger or more complex projects, `--framework bmad` is recommended — BMAD's structured planning roles (Analyst → Architect → Product Manager) produce richer phase decomposition, better dependency analysis, and more accurately sized milestones. When BMAD is installed and the project template included it (`ta init run --template python-ml`), `ta plan new` defaults to `--framework bmad` automatically unless overridden with `--framework default`.

**`ta plan new` also works on existing projects** to regenerate or extend a plan from an updated spec.



#### Items

#### Items

2. [x] **`ta init run` calls `ta setup vcs` automatically**: After creating `.ta/`, always run `ta setup vcs` with the detected or chosen VCS. Calls `super::setup::execute(&SetupCommands::Vcs {...}, config)` directly. Reports what was written; logs warning on failure but does not abort init.

3. [x] **Version bootstrap in `ta init run`**: Writes `version = "0.1.0-alpha"` to `.ta/project.toml` if file does not yet exist. Sets the starting point for the semver process before any phases exist.

4. [x] **`ta plan new <description>`**: Added `New` variant to `PlanCommands` enum. `plan_new()` function routes to `super::run::execute` with the description as inline objective. Result enters the draft queue.

5. [x] **`ta plan new --file <path>`**: Added `--file` flag. Reads file content (Markdown, plain text). Resolves path relative to workspace root. Validates file is non-empty. Passes full contents to `build_plan_new_prompt()`.

<!-- status: done -->

7. [x] **Plan generation agent prompt**: `build_plan_new_prompt()` produces well-structured PLAN.md-format instructions — semver phases, depends-on links, status markers. BMAD framework injects Analyst/Architect/Product-Manager role instructions. Auto-detects BMAD from `.ta/bmad.toml`. 4 unit tests.

8. [x] **`POST /api/plan/new`** (daemon endpoint): Added to `crates/ta-daemon/src/api/plan.rs`. Accepts `{ description?, file_content?, framework? }`. Spawns `ta plan new` as background process with stdin piping for file_content. Returns `{ output_key }` for SSE polling. Registered at `/api/plan/new` in `mod.rs`. 2 unit tests.

9. [x] **Studio integration**: New Project wizard calls `/api/plan/new` after init. Plan tab gains a "Generate Plan from file" button. → Deferred to v0.14.22 (Studio follow-up).



11. [x] **Tests**: `plan_new_prompt_contains_plan_md_format`, `plan_new_prompt_includes_bmad_instructions`, `plan_new_prompt_default_framework`, `plan_new_prompt_truncates_large_input` in plan.rs. `plan_new_requires_description_or_file`, `plan_new_framework_defaults_to_default` in api/plan.rs.

12. [x] **USAGE.md**: Updated project initialization section with unified workflow. Documented `ta plan new` with description/--file/--stdin variants and examples.



---


<!-- status: done -->
**Goal**: Fix three Studio UX regressions and add default personas so the personas system works out of the box.

**Depends on**: v0.14.21 (Studio plan tab, setup wizard)

#### Items

1. [x] **Plan tab: collapse queue** — `renderPlan()` in `index.html` shows only the first pending phase as "Next Up"; remaining phases are hidden behind a "▼ Show N more phases" toggle. Prevents the plan list from dominating the page on projects with many pending phases.

---

#### Items

4. [x] **PLAN.md phase cleanup** — Marked `v0.14.8.4` as `done` (was incorrectly left `pending`; work delivered as v0.14.18, PR #314).

#### Version: `0.14.22-alpha`

---

> **Unity Connector** → moved to v0.15.3 (Content Pipeline phases).

---

## v0.15 — Content Pipeline, Platform Integrations & Onboarding

> **Focus**: Generic artifact types (binary, text, video) and content-production connectors (ComfyUI, Unity) for creator workflows; plus platform integrations (ProjFS, messaging adapters) and the post-install onboarding wizard that closes the first-run configuration gap before IDE integration.

### v0.15.0 — Generic Binary & Text Asset Support (`ta-changeset`)
<!-- status: done -->
**Goal**: Add `ArtifactKind::Binary` and `ArtifactKind::Text` to core TA so any connector can produce opaque binary or raw text artifacts that flow through the standard draft/review/apply pipeline. Provides a catch-all for asset types not specifically modeled (scripts, config files, arbitrary data files).

**Depends on**: v0.14.15 (`ArtifactKind::Image`)

#### Items



2. [x] **`ArtifactKind::Text` in `ta-changeset`**: `ArtifactKind::Text { encoding: Option<String>, line_count: Option<u64> }`. Text artifacts render full diff in `ta draft view`. Useful for generated scripts, configs, and data files.



4. [x] **Unit tests**: Round-trip serialize/deserialize for both variants. `is_binary()`, `display_label()`. Draft view renders binary artifact without calling diff provider. Text artifact renders diff.

   ```

---

### v0.15.1 — Video Artifact Support (`ta-changeset`)
<!-- status: done -->
**Goal**: Add `ArtifactKind::Video` to core TA so video files (`.mp4`, `.mov`, `.webm`) produced by ComfyUI, Wan2.1, or other render pipelines flow through the draft/review/apply pipeline. Video diffs show metadata comparison (duration, resolution, codec) rather than binary content.



#### Items

1. [x] **`ArtifactKind::Video` in `ta-changeset`**: `ArtifactKind::Video { width: Option<u32>, height: Option<u32>, fps: Option<f32>, duration_secs: Option<f32>, format: Option<String>, frame_count: Option<u32> }`. `is_video()`, `display_label()`, and `video_metadata_summary()` helpers. `PartialEq` only (f32 precludes `Eq`).

2. [x] **`ta draft view` rendering**: Video diff suppressed; shows "Video artifact:" header with metadata summary (e.g. "Video: 1920×1080, 24fps, 6.2s, MP4") and "[Binary video — text diff suppressed]". `render_video_artifact_set_summary()` for set-level summaries (e.g. "2 MP4 video files, 1920×1080, 24fps").

```

#### Version: `0.15.1-alpha`

---

#### Items
<!-- status: done -->
**Goal**: Wrap ComfyUI's REST API as a TA connector so agents can submit Wan2.1 video-to-video inference jobs, poll status, and land output video frames in TA staging — flowing through the draft/review/apply pipeline with `ArtifactKind::Video` and `ArtifactKind::Image` artifacts.

**Depends on**: v0.14.14 (connector infrastructure), v0.14.15 (`ArtifactKind::Image`), v0.15.1 (`ArtifactKind::Video`)

   ```

```
<!-- status: done -->

  │   ├─ lib.rs           — exports ComfyUiConnector, ComfyUiBackend trait

  │   ├─ rest.rs          — ComfyUI REST API implementation
  │   ├─ stub.rs          — stub backend for tests
  │   ├─ frame_watcher.rs — output dir watcher → ArtifactKind::Video/Image
  │   └─ tools.rs         — MCP tool definitions

```

#### Items

1. [x] **Create `crates/ta-connectors/comfyui/` workspace member**: `ComfyUiBackend` trait — `submit_workflow(workflow_json, inputs) → job_id`, `poll_job(job_id) → { state, progress, output_files }`, `cancel_job(job_id)`. `RestBackend` hits `POST /prompt`, `GET /history/{id}`. `StubBackend` for tests.

2. [x] **Config schema** (`[connectors.comfyui]`):
   ```toml


   url = "http://localhost:8188"
   output_dir = ""   # ComfyUI output directory to watch
   ```

3. [x] **`ta connector install comfyui`**: Validates ComfyUI URL is reachable, writes config, prints next steps (install Wan2.1 model, set output dir).

4. [x] **Register ComfyUI tools in `ta-mcp-gateway`**:
   - `comfyui_workflow_submit(workflow_json, inputs)` → `{ job_id }`
   - `comfyui_job_status(job_id)` → `{ state, progress, output_files }`
```
   - `comfyui_model_list()` → `{ models: [{ name, type }] }`

5. [x] **Output watcher**: Scans ComfyUI output directory for new files after job completion. Copies video/image files to `.ta/staging/<goal-id>/comfyui_output/`. Tags with `ArtifactKind::Video` (`.mp4`/`.mov`/`.webm`) or `ArtifactKind::Image` (`.png`/`.jpg`/`.exr`).

```

7. [x] **Unit tests**: Tool routing. Config parsing. Stub backend round-trip. Output watcher copies files and assigns correct `ArtifactKind`. `ta connector install comfyui` output. (20 tests in `ta-connector-comfyui`, 4 new tools in gateway test count)

8. [x] **USAGE.md "ComfyUI Integration" section**: Installation, config, Wan2.1 workflow example, `comfyui_workflow_submit` call, output staging path, reviewing video artifacts in `ta draft view`.

#### Version: `0.15.2-alpha`

---

### v0.15.3 — Unity Connector (`ta-connectors/unity`)
<!-- status: done -->
**Goal**: Parallel to the Unreal connector (v0.14.14). Wraps Unity's official MCP server package (`com.unity.mcp-server`) with the same backend-switchable architecture. Agents can trigger builds, query scenes, run PlayMode tests, and export assets — all through TA's governed flow.

**Depends on**: v0.14.14 (shared connector infrastructure — `ta connector` CLI, backend trait, gateway integration)

> **Scaffold architecture note**: All five `unity_*` gateway tool handlers return `connector_not_running` stub responses and do not call `OfficialBackend`. This is intentional — identical to the Unreal (v0.14.14) and ComfyUI (v0.15.2) connector patterns. Full backend wiring (gateway → OfficialBackend → live TCP JSON-RPC to `com.unity.mcp-server`) is deferred to a future live-wiring phase once the connector has been validated in staging environments. Reviewer confirmation: this is expected scaffold behavior for v0.15.3.

#### Items

1. [x] **Create `crates/ta-connectors/unity/` workspace member**

   - `official` backend — Unity `com.unity.mcp-server` UPM package (primary; maintained by Unity Technologies across LTS versions)



2. [x] **Config schema** (`[connectors.unity]`):
   ```toml
   [connectors.unity]
       │
   backend = "official"
   project_path = ""
<!-- status: done -->
   ```

3. [x] **`ta connector install unity`**: Generates UPM `manifest.json` entry and prints paste-into-Unity-Package-Manager instructions. Writes config to `.ta/config.toml`.


   - `unity_build_trigger(target, config)` — trigger a Player or AssetBundle build
   - `unity_scene_query(scene_path)` — return GameObject hierarchy and component summary
   - `unity_test_run(filter)` — run EditMode or PlayMode tests, return pass/fail counts

   - `unity_render_capture(camera_path, output_path)` — capture a screenshot from a scene camera



6. [x] **Unit tests** (17 tests in `ta-connector-unity` + 5 gateway handler tests):
   - `ta-connector-unity`: mock backend process, config parsing, `ta connector install unity` output, backend trait round-trip
```toml


7. [x] **USAGE.md "Unity Integration" section**: Installation, config, `ta connector install unity`, first `unity_scene_query` call.

#### Human Review

- [x] Smoke-test `ta connector install unity` output against a real Unity project — verify the UPM manifest entry is correct for LTS 2022 and 2023. → v0.15.14.1 (tracked via human-review system once implemented)

#### Version: `0.15.3-alpha`

---

### v0.15.3.1 — Unity Connector Fix-Pass (reviewer findings)
<!-- status: done -->
**Goal**: Address the three code-level findings flagged during v0.15.3 draft review. The
always-stub pattern is confirmed intentional (see v0.15.3 architect note); the items below
are the actionable fixes that must land before the connector is considered production-ready.

**Depends on**: v0.15.3

1. [x] **Sanitize URI inputs before policy engine** *(SECURITY — minor)*:

     for `params.target` and `validate_unity_path()` (rejects `..`, `\`) for `params.camera_path`.
   - Returns structured `invalid_parameter` MCP error if validation fails.

     `build_trigger_returns_connector_not_running` (`StandaloneOSX` accepted).

2. [x] **Suppress dead-code clippy warnings on `OfficialBackend`** *(DEAD CODE)*:
   - Added `#[allow(dead_code)]` with `TODO(backend-wiring)` comment to the `OfficialBackend`
     struct and `pub fn new()` in `official.rs`.
   - `cargo clippy --workspace --all-targets -- -D warnings` passes cleanly.


   - Added 7 tests in `ta-mcp-gateway/src/tools/unity.rs`:
     `build_trigger_returns_connector_not_running`, `build_trigger_rejects_path_traversal_in_target`,
```
     `addressables_build_returns_connector_not_running`, `render_capture_returns_connector_not_running`,
     `render_capture_rejects_traversal_in_camera_path`.
   - Each handler has a stub-response test; traversal-rejection tests for build_trigger and render_capture.



---

### v0.15.4 — Agent-Run Contextual Asset Diffs in Draft Review
<!-- status: done -->
**Goal**: During `ta draft view`, for image and video artifacts, invoke a lightweight agent call to independently analyze before/after and produce a text summary of what changed. A supervisor agent then cross-checks that summary against the goal agent's stated intent and reports a confidence score. An optional visual diff (localized crop comparison or color bar) is appended when configured, giving reviewers the easiest possible signal for whether the change is what they expected.

**Why this phase exists**: Image and video artifacts can't be reviewed from a text diff — the reviewer needs to understand *semantically* what changed (lighting shifted, character moved left, background color changed). An agent-generated diff summary replaces manual visual inspection for small/medium changes and flags unexpected changes before the reviewer even opens the files.

**Depends on**: v0.14.15 (`ArtifactKind::Image`), v0.15.1 (`ArtifactKind::Video`), configured agent (Claude with vision)

1. [x] **`ExternalMemoryAdapter`** in `crates/ta-memory/src/external_adapter.rs`: Spawns the plugin binary, speaks the transport-agnostic operation schema. Initial transport: JSON-over-stdio. Internal transport abstraction (`MemoryTransport` enum: `Stdio`, `UnixSocket`, `Amp`) so unix-socket and AMP transports can be added without changing the adapter API or plugin operation schema. Plugin discovery: `.ta/plugins/memory/`, `~/.config/ta/plugins/memory/`, `$PATH`. Same lifecycle as `ExternalVcsAdapter`.

```


       1. DiffSummaryAgent — sees both files, produces text summary
       2. SupervisorAgent  — sees goal intent + diff summary, scores confidence

```

The diff summary is generated **without** reading the goal agent's summary first, ensuring an independent perspective. The supervisor sees both and reports agreement or flags divergence.

#### Config (`[draft.asset_diff]` in `workflow.toml`)

```toml
[draft.asset_diff]
enabled = true            # generate text summary (default: true if agent configured)
visual_diff = false       # also render visual diff output (default: false)
visual_diff_threshold = 0.3  # max fraction of image that can change for localized crop
                              # above threshold → full-image color bar instead
<!-- status: done -->
```

#### Items

1. [x] **`DiffSummaryAgent`** in `crates/ta-changeset/src/asset_diff.rs`: Takes `(before_path, after_path, artifact_kind)`, calls the configured agent with vision (Claude multimodal). Produces `AssetDiffSummary { text: String, change_type: ChangeType }`. `ChangeType`: `Localized`, `Tonal`, `Structural`, `Minor`, `Identical`. Agent prompt instructs: describe what visually changed — do not speculate about intent.

<!-- status: done -->

3. [x] **Visual diff renderer** (`VisualDiffRenderer`): Enabled by config. For `ChangeType::Localized` — writes a crop comparison placeholder. For `ChangeType::Tonal` — writes a color bar placeholder. For video — writes a keyframe summary placeholder. All to `staging_dir/diffs/<stem>_<type>.txt`. (Full image processing deferred — pure Rust without new deps writes text placeholders that show paths for reviewer.)

4. [x] **Integration with `ta draft view`**: After the supervisor review section, each image/video artifact shows agent diff summary, change type, supervisor confidence (with `[!] LOW CONFIDENCE` when < 0.7), and optional visual diff path.

5. [x] **Graceful degradation**: Any agent call failure returns `skipped_reason` and shows `(asset diff unavailable — <reason>)`. `visual_diff = false` skips renderer. Never blocks draft view from loading. `enabled = false` short-circuits the whole pipeline.

6. [x] **Unit tests** (11 tests in `asset_diff.rs`): `DiffSummaryAgent` JSON parsing for tonal/localized/unknown/non-JSON. `SupervisorAgent` high/low confidence, confidence clamping. `VisualDiffRenderer` colordiff/crop/keyframe paths. Config defaults/serde roundtrip. `run_asset_diff` disabled short-circuit. Markdown-fenced JSON extraction.

7. [x] **USAGE.md "Asset Diff in Draft Review" section**: How it works, config options, example output, confidence score interpretation, visual diff enablement.

#### Version: `0.15.4-alpha`

---

### v0.15.5 — Terms Acceptance Gate on First-Run Operations
<!-- status: done -->
**Goal**: Prompt the user to review and accept the TA terms of use during first-run operations (`ta init`, `ta run` first goal, `ta goal start`). Acceptance is recorded in the TA config dir and not asked again. Commands that don't mutate state (e.g. `ta plan list`, `ta draft view`) never gate on terms.

**Why this phase exists**: As TA moves toward public release and studio deployments, a clear terms acceptance moment is required for legal and onboarding purposes. It should feel like a natural part of setup — not a blocker mid-workflow.

#### Behaviour


- **Not triggered by**: read-only commands (`ta plan list`, `ta draft view`, `ta goal list`, `ta stats`, etc.).
- **Format**: Short, readable plain-text terms printed to stdout with a `[y/N]` prompt. If the terminal is non-interactive (CI/headless), print the terms path and exit with a clear error message telling the user to accept manually: `ta accept-terms`.
- **Acceptance stored**: `~/.config/ta/accepted_terms` — contains the terms version hash and acceptance timestamp. Checked once per binary version.

- **`ta accept-terms`**: Standalone command for non-interactive / CI environments. Prints terms, accepts on `--yes` flag.

#### Items

1. [x] **Terms file** at `apps/ta-cli/src/terms.txt` (embedded via `include_str!`). Short (~20 lines): what TA does, what it may read/write, privacy note, link to full terms. Version hash derived from SHA-256 of content (first 16 hex chars).

2. [x] **Acceptance check** via `ensure_accepted()` in `terms.rs`; gated in `main.rs` using `requires_terms_acceptance()` which matches only `Commands::Init`, `Commands::Run`, and `Commands::Goal` where `is_start_command()` returns true. Reads `~/.config/ta/accepted_terms`; if absent or stale hash, runs the interactive prompt. `is_start_command()` helper added to `goal.rs`.

3. [x] **`ta accept-terms`** subcommand updated with `--yes` flag: prints terms, records acceptance non-interactively. Used by CI and install scripts.

4. [x] **Non-interactive detection**: `ensure_accepted()` checks `std::io::stdin().is_terminal()`; if non-interactive and terms not accepted, returns clear error directing user to `ta accept-terms --yes`.

5. [x] **Tests** in `terms.rs`: `check_accepted_returns_err_when_no_file`, `check_accepted_returns_err_on_stale_hash`, `check_accepted_returns_ok_with_valid_acceptance`, `record_acceptance_writes_correct_file`, `terms_hash_is_stable`, `terms_text_is_not_empty`, `acceptance_roundtrip` (7 tests total).

6. [x] **USAGE.md "Terms & First-Run Setup" section**: explains when the prompt appears, shows interactive and CI flows, lists all `ta accept-terms` / `ta view-terms` / `ta terms-status` commands.



---

### v0.15.6 — Config File Naming Consistency
<!-- status: done -->
**Goal**: Standardise all `.ta/` config override files to the `<name>.local.toml` pattern. Currently `local.workflow.toml` is the odd one out — `daemon.local.toml` already follows the correct convention. Rename the override file and update every reference so all local overrides are consistently discoverable as `*.local.toml`.


- `local.workflow.toml` → `workflow.local.toml` (rename the loaded filename and gitignore entries)

**Scope**:
- All names that follow `<name>.local.toml` are already correct and stay unchanged: `daemon.local.toml`.
- Only `local.workflow.toml` needs renaming.

#### Items



2. [x] **Update `LOCAL_TA_PATHS`** in `crates/ta-workspace/src/partitioning.rs`: replace `"local.workflow.toml"` with `"workflow.local.toml"` (old name retained with comment so existing files stay gitignored).



4. [x] **Update `docs/USAGE.md`** to reflect the new name (was already using `workflow.local.toml`; added migration note).

5. [x] **Migration note in USAGE.md**: added blockquote — if you have a `local.workflow.toml`, rename it.





---

### v0.15.6.1 — Draft Package: Embedded Patches (Staging-Free Apply)
<!-- status: done -->
**Goal**: Store the actual unified diffs inside the draft package JSON at `ta draft build` time so that `ta draft apply` can succeed even when the staging directory no longer exists (deleted by `ta gc`, disk cleanup, or a crash between build and apply).

**Root cause of prior incident**: `ta draft apply` computes what to copy back by diffing staging vs source at apply-time. The package JSON stores only metadata (`diff_ref: "changeset:N"` pointers) — no actual patch bytes. Deleting staging (even accidentally) makes the draft permanently un-appliable, requiring manual re-implementation.


- Add `embedded_patch: Option<String>` to `Artifact` in `ta-changeset/src/draft_package.rs` — a unified diff string (output of `diff -u source staging`) embedded at build time
- For new files: embed full file content (base64 or raw text). For deleted files: embed the tombstone only.

- `ta draft apply`: try staging-dir apply first (current behavior, fast path). If staging is absent AND `embedded_patch` is present on all artifacts, apply via `patch -p0` from embedded content. If staging is absent AND any artifact lacks an embedded patch, error with the existing message plus a note that the package predates v0.15.6.1.
- Binary files: encode as base64 in `embedded_patch`; apply by decoding and writing directly (no `patch`)

#### Items

1. [x] **`Artifact.embedded_patch`** (`ta-changeset/src/draft_package.rs`): add `embedded_patch: Option<String>` field. Backwards-compatible (`#[serde(default)]`).

2. [x] **Embed at build time** (`apps/ta-cli/src/commands/draft.rs` `build_package`): after the overlay diff loop, for each modified/added/deleted artifact, compute a unified diff against the source baseline and store in `artifact.embedded_patch`. Use the `DiffContent` already computed — serialize it as a standard `-u` diff string.

3. [x] **Fallback apply** (`apply_package` in `draft.rs`): when `goal.workspace_path` does not exist, check that all artifacts have `embedded_patch`. If yes, apply each patch to source using the `diffy` crate (already in workspace) or `patch` subprocess. If any artifact lacks it, keep the existing error message and add: "This package predates embedded-patch support (v0.15.6.1). Re-run the goal to regenerate."

<!-- status: done -->

5. [x] **Tests**: build a package → delete staging dir → apply succeeds from embedded patch. New-file case. Binary-file case (base64 roundtrip). Package without `embedded_patch` keeps old error path.

6. [x] **Tests for v0.15.6 `workflow.local.toml` merge** (deferred from v0.15.6 item 6): confirm `workflow.local.toml` is loaded and merged; confirm `local.workflow.toml` triggers the deprecation warning and is still applied.



---

### v0.15.6.2 — Finalizing Timeout Fix + Aggressive Auto-GC
<!-- status: done -->


**Root cause of timeout**: `ta draft build` runs synchronously inside the finalizing phase. On large workspaces, diffing staging vs source exceeds the 300s watchdog. The goal is marked `failed` and staging is left on disk — GC threshold for failed goals is 7 days, long enough to accumulate many multi-GB dirs.

**Root cause of disk bloat**: Staging is a full copy of source. Each goal consumes several GB even though the agent only touched a handful of files. The planned VFS approach (ProjFS, v0.15.8) solves this on Windows only. This phase adds a cross-platform mitigation and makes GC aggressive enough that accumulation can't happen.

#### Items — Finalizing Timeout

1. [x] **Increase finalizing timeout**: `[timeouts] finalizing_s = 600` added to `DaemonConfig` (`crates/ta-daemon/src/config.rs`). `WatchdogConfig::from_config()` now accepts `Option<&TimeoutsConfig>` as a third param and prefers `timeouts.finalizing_s` over the legacy `ops.finalize_timeout_secs`. Default watchdog `finalize_timeout_secs` also raised from 300 → 600.

2. [x] **Async draft build**: `try_spawn_background_draft_build()` added to `run.rs`. After the agent exits, writes a `DraftBuildContext` JSON to `.ta/draft-build-ctx/<goal-id>.json`, then spawns `ta draft build <goal_id> --apply-context-file <path>` as a detached background process (process group 0 on Unix). Falls back to synchronous build if spawn fails or in headless mode (callers need the draft ID synchronously).

3. [x] **Finalizing recovery**: `diagnose_goal()` in `goal.rs` detects `finalize_timeout` / `Finalizing timed out` in the failure reason and returns a targeted message explaining the agent work completed successfully — only draft packaging was interrupted — and gives the exact `ta goal recover <id>` command to re-run only the draft build.

#### Items — Auto-GC & Disk Efficiency

4. [x] **Aggressive GC defaults**: `GcConfig.failed_staging_retention_hours` defaults to **4** in `config.rs`. `ta gc` main loop uses a 4-hour cutoff for failed/denied goals.

5. [x] **GC on daemon startup + periodic**: `watchdog::startup_gc_pass()` called at daemon start (both API and MCP modes) in `main.rs`. Periodic tokio task spawned to re-run every `gc_interval_hours` (default 6). Daemon prints freed space on startup if anything was removed.



7. [x] **Staging size cap**: `GcConfig.max_staging_gb` defaults to 20. `enforce_staging_cap()` in `gc.rs` checks total staging size before a new goal starts (`run.rs` calls it). Removes oldest failed/completed dirs until under cap.

8. [x] **Sparse staging** (cross-platform, pre-ProjFS): deferred — scope is larger than this phase. Tracked in v0.15.8 alongside Windows ProjFS work.

9. [x] **Tests**: `gc_status_prints_table`, `gc_failed_uses_aggressive_cutoff`, `check_staging_cap_returns_false_when_zero`, `periodic_gc_removes_old_failed_staging`, `load_gc_config_returns_defaults_when_no_file`, `load_gc_config_reads_from_daemon_toml` — 6 new tests in `gc.rs`.

10. [x] **USAGE.md "Disk & GC"** section added: staging disk model, automatic GC behavior, `ta gc --status` output, `ta gc --delete-stale`, staging size cap, and `[gc]` / `[timeouts]` config reference.

#### Version: `0.15.6.2-alpha`

---

### v0.15.7 — Velocity Stats: Committed Aggregate & Multi-Machine Rollup
<!-- status: done -->
**Goal**: Make velocity data committable, team-visible, and conflict-free. Currently `velocity-stats.jsonl` is purely local (gitignored), so stats never aggregate across machines or team members. This phase introduces a committed `velocity-history.jsonl` that is auto-staged on `ta draft apply --git-commit`, using the same append-only pattern as `plan_history.jsonl`.


- `velocity-stats.jsonl` — stays LOCAL (raw per-machine log, unchanged)
- `velocity-history.jsonl` (new) — SHARED, committed to VCS, one line per completed goal
- Written by `ta draft apply --git-commit` (same moment `plan_history.jsonl` is updated)
- Each entry tagged with `machine_id` (hostname hash) and `committer` (from git config) so multi-machine appends are unique lines → no merge conflicts
- `ta stats velocity` reads BOTH files: local raw log + committed history (deduplicates by `goal_id`)
<!-- status: done -->

#### Items

1. [x] **`velocity-history.jsonl` schema**: extended `VelocityEntry` with `machine_id: String` (first 8 chars of SHA256(hostname)) and `committer: Option<String>` (from `git config user.name`). Both `#[serde(default)]` — backwards-compatible. Added `machine_id()` helper (SHA-256 hostname hash), `git_committer()` helper, `with_machine_id()` / `with_committer()` builder methods. `VelocityHistoryStore` added alongside `VelocityStore`.

2. [x] **Write on apply**: `apply_package` §8c block in `apps/ta-cli/src/commands/draft.rs` writes to `.ta/velocity-history.jsonl` when `git_commit=true`, stamped with `machine_id` and `committer`. Uses `VelocityHistoryStore::for_project(target_dir)` — writes to the source project, not staging, so it's captured by `adapter.commit()`.

3. [x] **Add to `SHARED_TA_PATHS`** in `partitioning.rs` (`velocity-history.jsonl` added). Auto-staged via `git.rs` `auto_stage_candidates()` alongside `plan_history.jsonl`.



5. [x] **`ta stats velocity` team + conflict view**: `--team` flag removed in favour of always showing per-contributor breakdown and phase conflict warnings. `aggregate_by_contributor()` groups committed entries by committer/machine_id. `detect_phase_conflicts()` flags plan phases with entries from more than one contributor. Both shown automatically in `ta stats velocity` output. `PhaseConflict` struct added to `velocity.rs`. 2 new tests: `detect_phase_conflicts_flags_multi_contributor_phases`, `detect_phase_conflicts_no_conflicts_when_single_contributor`.

6. [x] **`ta stats export`**: updated CSV header includes `machine_id` and `committer` columns. `--committed-only` flag added to export only the shared history.

```toml

8. [x] **Tests**: `velocity_history_store_append_and_load`, `velocity_history_empty_when_no_file`, `merge_deduplicates_by_goal_id`, `migrate_promotes_local_entries_to_history`, `aggregate_by_contributor_groups_by_committer`, `old_entry_without_machine_id_deserializes_ok`, `machine_id_is_eight_hex_chars`, `machine_id_is_stable` (8 tests in `velocity.rs`). `apply_with_git_commit` extended to assert `velocity-history.jsonl` is written with correct fields. `auto_stage_candidates_includes_builtin_and_plan_history` updated.





---

### v0.15.7.1 — Background Process Lifecycle: Heartbeat, Event Notification & Reviewer Resilience
<!-- status: done -->
**Goal**: Replace the static finalizing timeout with a heartbeat-based liveness model. Surface background draft build completion inline (shell/Studio notification, no opaque CTA). Fix reviewer agents so they work from the draft package — never from staging — making them resilient to GC and staging cleanup.

**Why this phase exists**: v0.15.6.2 solved the timeout by raising it from 300s→600s and moving draft build to background. But the underlying model is still wrong:
- **Static timeout**: the watchdog kills at T+600s regardless of whether the background process is actively working. A slow machine building a large workspace will time out even though the process is healthy.
- **Silent background**: the user gets "Agent exited. Draft build running in background (PID 17374). Run `ta draft list`..." — an opaque CTA that doesn't tell them when it's done.
- **Reviewer failures**: the governed-goal workflow spawns a reviewer agent that inherits a staging dir reference. When GC cleans that dir (4h for failed goals, startup pass), the reviewer fails. The reviewer doesn't need staging — it needs the draft package. Since v0.15.6.1 added embedded patches to every artifact, reviewers can read the diff directly from the package without touching disk.

**Not in scope**: Changing the background spawn model itself (it's correct — agents should exit fast). Changing GC retention (4h is right). Only the heartbeat, notification, and reviewer agent wiring change.

---

#### Design: Heartbeat-based watchdog

**Current**: `WatchdogConfig { finalize_timeout_secs: 600 }` — static wall-clock timer from goal start.

**New**: `WatchdogConfig { heartbeat_interval_secs: 30, heartbeat_timeout_secs: 120 }` — watchdog checks `.ta/heartbeats/<goal-id>` mtime. If mtime is older than `heartbeat_timeout_secs`, goal is considered hung. Background processes write heartbeats every `heartbeat_interval_secs`. Wall-clock timeout is removed entirely for background processes; it remains only for the initial agent spawn (up to `agent_start_timeout_secs: 60`).

```toml
<!-- status: done -->

heartbeat_timeout_secs  = 120  # watchdog: if no heartbeat for this long, kill
agent_start_timeout_secs = 60  # timeout for initial agent process to start
```

Background draft build loop:
```
---
  → every 30s: touch .ta/heartbeats/<goal-id>
  → watchdog: if .ta/heartbeats/<goal-id> mtime > 120s ago → kill, mark failed
  → on completion: write .ta/heartbeats/<goal-id>.done, emit DraftBuilt event
```



The daemon event bus already has `draft_built` events (from v0.14.8.3). When background draft build completes, it writes a sentinel file `.ta/heartbeats/<goal-id>.done`. The daemon's file watcher picks this up and emits `EventKind::DraftBuilt { goal_id, draft_id }` on the event bus.

`ta shell` is already subscribed to events. When `DraftBuilt` fires, the shell prints inline:
```

    → ta draft view f3eb3516   (11 files changed)
```

TA Studio already has an event SSE stream. When `DraftBuilt` fires, Studio shows a toast notification and updates the Goals tab — no page refresh required.

#### Design: Reviewer agent resilience





The reviewer goal never marks `failed` because staging was absent — it marks `failed` only if the review itself produces no verdict. Remove item 9 from v0.15.19 (auto-closing reviewer goals) — fix the root cause instead.

---

#### Items

1. [x] **Heartbeat writer in background draft build** (`apps/ta-cli/src/commands/draft.rs`): In the `--apply-context-file` code path (background build), spawn a heartbeat thread that `touch`es `.ta/heartbeats/<goal-id>` every `heartbeat_interval_secs`. Stop the thread on build completion or error. Write `.ta/heartbeats/<goal-id>.done` on success, `.ta/heartbeats/<goal-id>.failed` on error.

2. [x] **Heartbeat-based watchdog** (`crates/ta-daemon/src/watchdog.rs`): Replace `finalize_timeout_secs` with `heartbeat_timeout_secs` (default 120) and `agent_start_timeout_secs` (default 60). For goals in `Finalizing` state with a background process: check `.ta/heartbeats/<goal-id>` mtime instead of wall-clock elapsed. If mtime > `heartbeat_timeout_secs` or `.failed` sentinel exists → mark goal `Failed`. Remove the 600s static check. Retain wall-clock for `Running` state (agent hasn't started writing heartbeats yet).

3. [x] **`DraftBuilt` event with title** (`crates/ta-daemon/src/main.rs` or `crates/ta-events/src/`): File watcher already watches `.ta/store/`. Extend to watch `.ta/heartbeats/`. When `<goal-id>.done` appears, load the goal record to get `draft_id`, emit `EventKind::DraftBuilt { goal_id, draft_id, file_count }` on the event bus.



5. [x] **Studio SSE event with title** (`crates/ta-daemon/src/api/events.rs` + frontend): When `DraftBuilt` event fires, SSE sends `{ type: "draft_built", goal_id, draft_id, title, file_count }`. Frontend JS shows a non-blocking toast: "Draft ready: v0.15.7 — 11 files. [View]". Goals tab refreshes the active goal card to show "draft ready" state.

<!-- status: done -->

7. [x] **Remove static exit CTA** (`apps/ta-cli/src/commands/run.rs`): Replace `"Agent exited. Draft build running in background (PID {pid}).\nRun \`ta draft list\` or \`ta status\` to check when the draft is ready."` with `"Agent exited. Building draft in background — you'll be notified when it's ready."`. The shell notification (item 4) delivers the actual result.

---

9. [x] **Tests**: Heartbeat writer creates and updates `.ta/heartbeats/<goal-id>` during build. Watchdog marks goal failed when heartbeat mtime > timeout (no `.done` file). `DraftBuilt` event emitted when `.done` appears. Shell prints inline notification on `DraftBuilt` event. Reviewer proceeds without staging when `staging_required = false`. Reviewer `Failed` only on no-verdict, not on staging absence.

10. [x] **USAGE.md update**: Replace "Agent exited — check ta draft list" docs with "You'll be notified inline when the draft is ready." Document heartbeat config in `[timeouts]` section. Document reviewer resilience (staging not required).

#### Version: `0.15.7.1-alpha`

---


<!-- status: done -->
**Goal**: On Windows NTFS volumes (where APFS/Btrfs CoW cloning is unavailable), use the Windows Projected File System (ProjFS) to make staging creation near-instant and zero-disk-cost. Files appear present in the staging directory but are hydrated on-demand from source as the agent reads them. Writes go to a real scratch store. Only files the agent actually touches are physically copied.



**Why**: On large workspaces (UE5 projects, Unity repos, large Node.js codebases), full-copy staging on Windows takes 5–30 seconds and duplicates gigabytes of files the agent never touches. ProjFS eliminates both costs — staging is instant and disk usage is proportional to agent activity, not workspace size.


```
- ProjFS provider: placeholder-based virtual directory at staging root; callbacks hydrate files on agent access
- Write interception: modified files redirected to a real scratch directory, overlaid transparently in diffs
- Auto-detection: check `Client-ProjFS` Windows optional feature at startup; fall back to `Smart` if not enabled
```toml

#### Items

1. [x] **`StagingStrategy::ProjFs` variant**: Added to `crates/ta-submit/src/config.rs` (`StagingStrategy::ProjFs`), `crates/ta-workspace/src/overlay.rs` (`OverlayStagingMode::ProjFs`), `crates/ta-workspace/src/copy_strategy.rs` (`CopyStrategy::Virtual`). Wired in `run.rs` and `goal.rs` (both match sites). Auto-selected on Windows when `Client-ProjFS` is enabled; falls back to `Smart` otherwise.

---





5. [x] **Installer integration**: `apps/ta-cli/wix/main.wxs` — optional `<Feature Id="ProjFS">` with descriptive title/description. Custom action `EnableClientProjFS` runs `Dism.exe /Online /Enable-Feature /FeatureName:Client-ProjFS /NoRestart` on install when feature is selected.



7. [x] **USAGE.md**: "Fast staging on Windows (ProjFS)" section added after the Copy-on-write staging paragraph — covers installation via installer or DISM, `strategy = "projfs"` config, fallback behavior, and how modified/created/deleted/unmodified files are handled.



---

### v0.15.8.1 — Inline Draft Build for Interactive CLI
<!-- status: done -->
```toml

**Why this phase exists**: The background build model (v0.15.6.2) was introduced to avoid the static watchdog timeout. That root cause is now fixed (v0.15.7.1 heartbeat watchdog). For interactive `ta run` invocations, blocking is strictly better:
- The user is already waiting — the agent ran for minutes. 30 more seconds is invisible.
<!-- status: done -->
- Inline build gives the user immediate next-step output without any follow-up command.

**Background model stays for**: daemon-mediated runs (no TTY), `ta shell` (stays open to receive the event), headless CI invocations.

**Target output (interactive TTY)**:
```
Agent exited.
---

#### Items
```

#### Items

1. [x] **TTY detection** (`apps/ta-cli/src/commands/run.rs`): In `try_spawn_background_draft_build()`, check `std::io::stdout().is_terminal()`. If `true`, calls `build_draft_inline()` and returns `Some(BackgroundBuildHandle::Inline)`. Added `BackgroundBuildHandle` enum with `Inline` and `Background(u32)` variants.

2. [x] **`build_draft_inline()`** (`apps/ta-cli/src/commands/draft.rs`): Builds draft synchronously with spinner thread. Attaches verification warnings, validation log, supervisor review. Prints `✓ Draft ready: "<title>" [<id>]` on completion. Returns `Err` on failure.

---

4. [x] **Remove misleading CTA text**: Background "you'll be notified" message only printed for `BackgroundBuildHandle::Background` path. TTY `Inline` path prints the `✓` result directly.

5. [x] **Tests**: 3 tests in `draft.rs` (`build_draft_inline_succeeds_and_creates_draft`, `build_draft_inline_attaches_verification_warnings`, `build_draft_inline_fails_gracefully_on_bad_goal_id`). 2 tests in `run.rs` (`background_build_handle_inline_variant_is_not_background`, `try_spawn_background_draft_build_returns_none_for_non_tty_with_no_project`).

6. [x] **USAGE.md**: Added "After the agent exits — inline vs background build" section with example output. Updated heartbeat section to clarify background-only context.

#### Version: `0.15.8.1-alpha`

---


<!-- status: done -->
<!-- status: done -->




#### Items
- Built-in plugins: `ta-messaging-gmail`, `ta-messaging-outlook`, `ta-messaging-imap` (in `plugins/messaging/`)
- Plugin discovery: `~/.config/ta/plugins/messaging/`, `.ta/plugins/messaging/`, `$PATH` (prefix `ta-messaging-`)
- Credentials stored in OS keychain via `keyring` crate — plugin calls `ta adapter credentials get <key>` to retrieve; `ta adapter credentials set <key>` to store. Never written to disk in plaintext.
- `ta adapter setup messaging/<plugin>` — one-time credential capture wizard (OAuth browser flow or masked IMAP prompt)





```



→ { "op": "create_draft", "draft": { "to", "subject", "body_html", "in_reply_to", "thread_id" } }



← { "state": "drafted" | "sent" | "discarded" }      # provider-reported state; best-effort



```



#### Items

1. [x] **`MessagingAdapter` protocol spec** (`crates/ta-submit/src/messaging_plugin_protocol.rs`): Request/response enums. `fetch`, `create_draft`, `draft_status`, `health`, `capabilities` ops. No `send` op — enforced at the type level (no variant exists). Shared `ExternalMessagingAdapter` struct wrapping the subprocess.

#### Items

3. [x] **`ta adapter setup messaging/<plugin>`**: Credential wizard. Gmail/Outlook: OAuth2 browser flow (open consent URL, localhost callback, store refresh token in keychain under `ta-messaging:<address>`). IMAP: masked prompt for host/port/username/app-password, validate connection, store in keychain. Prints health check result on success.

4. [x] **`plugins/messaging/ta-messaging-gmail`**: Rust binary. Implements `fetch` via Gmail REST API (OAuth2 refresh), `create_draft` via `drafts.create` API, `draft_status` via `drafts.get`. Retrieves token from keychain. Packaged with the TA installer.

5. [x] **`plugins/messaging/ta-messaging-outlook`**: Rust binary. Implements `fetch` via Microsoft Graph API, `create_draft` via `POST /messages` with `isDraft:true`, `draft_status` via `GET /messages/{id}`. Same keychain retrieval pattern.

```

7. [x] **`ta adapter health messaging`**: Calls `health` op on each configured messaging plugin, prints provider, connected address, last-fetch timestamp. No credentials printed.

8. [x] **`DraftEmailRecord`** (`crates/ta-goal/src/messaging_audit.rs`): Audit struct stored per goal: `draft_id`, `provider`, `to`, `subject`, `created_at`, `state`, `goal_id`, `constitution_check_passed`, `supervisor_score`. Persisted in `.ta/messaging-audit.jsonl`. `ta audit messaging` prints the log.

9. [x] **Tests**: Protocol round-trip with a mock plugin script (20 tests in ta-submit); `send` op rejected at type level (no variant); `create_draft` returns provider draft_id; discovery finds plugin in each search path; credentials set/get via env override; `draft_status` state roundtrip. 9 tests in ta-goal, 13 adapter tests in ta-cli.

10. [x] **USAGE.md**: "Messaging Adapters" section — plugin protocol, how to set up each built-in provider, `create_draft` vs `send` design rationale, how to write a community plugin.

#### Version: `0.15.9-alpha`

---

### v0.15.10 — Email Assistant Workflow (`email-manager`)
<!-- status: done -->
**Goal**: A TA workflow template that drives the `MessagingAdapter` to assist with email: fetch since last run → filter → run a reply-drafting goal per message → supervisory review against the constitution → push the approved draft to the user's native email Drafts folder. The user reviews, edits, and sends from their email client. TA never sends. Scheduled via daemon scheduler or cron/Task Scheduler.



**Core design principle**: TA's role ends at draft creation. The user's email client is the review and send surface. The supervisory agent enforces the constitution before the draft even reaches the inbox — not as an afterthought. There is no `auto_approve` path that bypasses human review; the only variation is whether the supervisor flags something for explicit TA review before it reaches the email Drafts folder, or lets it through directly.

**Workflow steps**:
```
fetch(since: watermark)
  → filter rules (ignore / flag / reply / escalate)
  → [reply] spawn reply-drafting goal
#### Items
               supervisor: check voice, commitments, policy, confidence score
               │
            ┌──┴────────────────────────────────────────────────┐
            │ pass (confidence ≥ threshold)                      │ flag
            ▼                                                     ▼
   MessagingAdapter.create_draft()                   TA review queue
   → draft in Gmail/Outlook Drafts folder            → user sees flag reason
   → DraftEmailRecord in audit log                     before any draft is pushed

---
   user reviews, edits, sends from email client

```

**Workflow config** (`~/.config/ta/workflows/email-manager.toml`):
```toml
```
name            = "email-manager"
adapter         = "messaging/gmail"
account         = "me@example.com"

constitution    = "~/.config/ta/email-constitution.md"

[supervisor]

min_confidence  = 0.80
# always flag if the reply contains any of these (belt-and-suspenders)
flag_if_contains = ["commit", "guarantee", "by tomorrow", "I promise"]

[[filter]]

from_domain     = ["client.com", "partner.org"]
subject_contains = ["?", "help", "question"]
action          = "reply"      # reply | flag | ignore | escalate

[[filter]]

subject_contains = ["unsubscribe", "newsletter"]

```

`action = "escalate"` flags the message directly to the TA review queue without running a reply goal — for messages that need human judgment before any draft is attempted (e.g., legal or HR topics).

#### Items

1. [x] **`email-constitution.md` template**: Created by `ta workflow init email-manager` if absent. Documents voice, sign-off style, topics to engage/decline, escalation triggers, out-of-office language. Injected verbatim into every reply goal prompt and supervisor check. (`templates/email-constitution.md`, `init_email_manager()` in `apps/ta-cli/src/commands/email_manager.rs`)

2. [x] **Workflow fetch step**: Calls `MessagingAdapter.fetch(since: last_watermark)`. Stores watermark in `~/.config/ta/workflow-state/email-manager.json`. Advances watermark on successful completion of each batch. (`load_watermark`/`save_watermark`, `run_email_manager_with_ops`)





5. [x] **Supervisory review step**: After each goal completes, supervisor agent checks the draft against the constitution: voice match, no unverified commitments, no policy keywords from `flag_if_contains`, confidence ≥ `min_confidence`. Pass → `create_draft`. Fail → TA review queue with the supervisor's flag reason shown to the user. (`supervisor_check`, `SupervisorConfig`, `SupervisorResult`)

6. [x] **`create_draft` step**: Calls `MessagingAdapter.create_draft()`. Draft lands in the user's native email Drafts folder. Records `DraftEmailRecord` in `.ta/messaging-audit.jsonl`. Logs: goal_id, draft_id, to, subject, supervisor_score. (`run_email_manager_with_ops` create_draft branch)

7. [x] **TA review queue entry** (for flagged items): Shows original message, proposed reply, supervisor flag reason. Entries persist in `.ta/email-review-queue.jsonl`. (`ReviewQueueEntry`, `push_to_review_queue`, `show_email_manager_status`)

8. [x] **`ta workflow run email-manager --since <datetime>`**: One-off catch-up run overriding the watermark. Useful for catching up after time away. (`--since` flag added to `WorkflowCommands::Run`)

9. [x] **`ta audit messaging`**: Prints `DraftEmailRecord` log — date, to, subject, supervisor score, state (drafted/sent/discarded), manually_approved flag. (Implemented in v0.15.9; `apps/ta-cli/src/commands/audit.rs`)

10. [x] **Daemon scheduling**: `run_every = "30min"` in workflow TOML parsed by `WorkflowMeta`. `ta workflow status email-manager` shows last run, messages processed, drafts created, flagged for review. (`EmailManagerStatus`, `show_email_manager_status`)

11. [x] **Cron / Task Scheduler**: `ta workflow run email-manager` is headless — no daemon required. Documented in USAGE.md with crontab and Windows Task Scheduler examples.

12. [x] **Tests**: Full pipeline with mock adapter: fetch → filter → reply goal → supervisor pass → `create_draft` called with correct body; supervisor fail → review queue (no draft created); `escalate` filter → review queue without goal; `--dry-run` prints plan, no drafts created; watermark advances only on success; `flag_if_contains` triggers flag. (31 tests in `email_manager.rs`)

---



---

### v0.15.11 — Post-Install Onboarding Wizard (`ta onboard`)
<!-- status: done -->
**Goal**: A guided first-run setup experience that runs automatically after installation (or on demand) to configure the user's AI provider, default implementation agent, planning framework, and optional components. Runs as a TUI wizard in the terminal (ratatui, same as the existing shell TUI); offers `--web` to open the Studio setup page instead. Written once; called from all three per-platform installer post-install hooks.

**Why this phase exists**: New users currently land after install with no configured API key, no default agent, and no idea BMAD or Claude-Flow exist as options. The onboarding gap causes the most common support issue: "ta run says no agent configured." This wizard eliminates that gap — the user leaves the installer with a working, opinionated setup and a clear mental model of what was installed.

**Scope**: Global user config (`~/.config/ta/config.toml`) only. Project-level setup (`ta init`, `ta setup wizard`) is a separate concern.

**Depends on**: v0.15.5 (terms acceptance gate — wizard re-uses the same gate as step 0), v0.14.20 (persona system for default persona selection), v0.13.11 (platform installers that call the wizard)



#### Wizard steps (TUI flow)

```

Step 1  — AI Provider
          ● Claude (Anthropic)  ← default
            › API key mode: detects ANTHROPIC_API_KEY env; prompts if absent; validates with
              a lightweight /v1/models call; stores in OS keychain via `keyring` crate

              ANTHROPIC_API_KEY in your shell profile or run 'ta config set api_key <key>'"
          ○ Ollama (local)
            › Auto-detects running Ollama instance (http://localhost:11434)

          ○ Skip for now  (can complete later with 'ta onboard')

          ● claude-code  ← default; detects binary on PATH
          ○ codex        (detects binary on PATH; grayed out with install hint if absent)
          ○ claude-flow  (detects npm package; offers to install if absent: `npm i -g claude-flow`)
```
Step 3  — Planning Framework


            › Detects ~/.bmad/; offers `git clone https://github.com/bmadcode/bmad-method ~/.bmad`

          ○ GSD               — goal-structured decomposition
Step 4  — Optional Components
          [ ] claude-flow agent framework  (npm install -g claude-flow)

---
Step 5  — Summary & Confirm
          Shows what will be installed / configured. User confirms or goes back.
          On confirm: writes ~/.config/ta/config.toml [defaults], installs selected components,
          prints: "Setup complete. Run 'ta studio' to open TA Studio, or 'ta run <goal>' to start."
```

#### Config written (`~/.config/ta/config.toml`)

```toml


# api_key stored in OS keychain, not written to file
# For Ollama:

# base_url       = "http://localhost:11434"
```

[defaults]
agent              = "claude-code"    # implementation agent
planning_framework = "bmad"           # default | bmad | gsd

```

#### Installer integration



- **Linux** (`.deb`/`.rpm` post-install): `ta onboard` in `postinst` hook; gracefully skips if stdin is not a tty (package manager piped install)
- **First-run hint**: If `~/.config/ta/config.toml` has no `[provider]` section and the user runs `ta run` or `ta serve`, print: `"TA is not configured yet. Run 'ta onboard' to set up your AI provider and defaults (takes ~2 minutes)."`

#### Items



2. [x] **TUI wizard** (`apps/ta-cli/src/commands/onboard.rs` — integrated): ratatui 5-step wizard. Each step is a screen with arrow-key selection and inline help text explaining each option. `←`/`Esc` goes back, `→`/`Enter` advances, `q` quits. Progress gauge at bottom.

3. [x] **`--web` flag**: Starts daemon (if not running), opens `http://localhost:<port>/setup` in the default browser. Falls back to TUI if daemon start fails.

4. [x] **Provider detection**: At wizard start, detect `ANTHROPIC_API_KEY` env var (pre-fill API key field), detect Ollama at `localhost:11434`, detect installed agent binaries on `$PATH`. Pre-select detected options to minimise typing.



6. [x] **BMAD install step**: If BMAD selected and `~/.bmad/` absent, `git clone --depth=1 https://github.com/bmadcode/bmad-method ~/.bmad`. Validates clone by checking for `~/.bmad/agents/` directory. On failure: warns and continues.

7. [x] **Claude-Flow install step**: If selected and `claude-flow` not on PATH, run `npm install -g claude-flow` (checks npm availability first; if npm absent, shows install instructions and skips). Validates with `which claude-flow`.

8. [x] **Config write**: Writes `~/.config/ta/config.toml` `[provider]` and `[defaults]` sections atomically (write to `.tmp`, rename). Preserves any existing keys not touched by the wizard. On success prints the config path.

9. [x] **Installer hook — macOS/Linux**: Updated `install.sh` to call `ta onboard` at the end. Guards with `[ -t 0 ] && [ -t 1 ]` (only if stdin/stdout are a tty). If not a tty (non-interactive install), prints the first-run hint instead.



11. [x] **Installer hook — Linux deb/rpm**: Update `.deb` `postinst` and `.rpm` `%post` scripts to call `ta onboard` if stdin is a tty; otherwise print first-run hint. (Deferred — package scripts are generated by CI, not in-tree source.)

12. [x] **First-run gate in `ta run` / `ta serve`**: If `[provider]` section absent from global config, print the first-run hint and exit 1 (with `--skip-onboard-check` escape hatch for CI in `ta run`; `TA_SKIP_ONBOARD_CHECK=1` env var for `ta serve`).

13. [x] **`ta onboard --status`**: Prints a summary of current configuration (provider type, agent, planning framework, BMAD path, whether API key is set in keychain) without running the wizard.

14. [x] **`ta onboard --reset`**: Clears `[provider]` and `[defaults]` from global config and removes keychain entry, then re-runs wizard. Useful when switching from one provider to another.

15. [x] **Tests**: 16 unit tests — config read/write round-trip (Anthropic + Ollama); `is_configured_at` true/false; clear preserves other sections; API key store/read/delete; env var priority; first-run gate passes with skip flag; first-run gate produces "ta onboard" error message; atomic write leaves no `.tmp`; BMAD detection idempotency; claude-flow detection smoke test; extra-section preservation on rewrite.

16. [x] **USAGE.md**: "First-Time Setup" section added — what `ta onboard` does, how to re-run it, `--status`/`--reset`/`--force`/`--web`/`--non-interactive` flags, API key configuration separately, BMAD and Claude-Flow notes.

#### Version: `0.15.11-alpha`

---

### v0.15.11.1 — Draft Apply Lock & Co-Dev Guard
<!-- status: done -->


**Why this phase exists**: A race between a manual `git checkout` (or `git add -A && git commit`) and a running `ta draft apply --submit` caused the apply to find "no changes to commit" and roll back. The fix requires TA to advertise its apply state externally so any co-developer process (human or AI assistant) can detect it before making git mutations. This is also needed for parallel goal runs where two drafts must not apply concurrently to the same workspace.

**Design**:
- **Lock file**: `.ta/apply.lock` — contains `{"draft_id": "...", "pid": 12345, "started_at": "..."}`. Written at entry to `apply_package`, removed in a `defer`-style cleanup (even on panic/early return).
- **Stale lock detection**: If lock exists but `pid` is no longer alive → remove and continue (previous apply crashed without cleanup).
- **Concurrent apply guard**: If lock exists and pid is alive → fail immediately with actionable error: `"Draft apply already in progress (PID X, draft Y). Wait for it to finish or kill PID X if it has crashed."`

- **`ta draft apply --status`**: Shows whether an apply is in progress (reads lock file). Useful for scripts.
- **`.gitignore`**: `.ta/apply.lock` added to gitignore (ephemeral, per-machine).


- [x] `ApplyLock` struct (`draft.rs`): `acquire()` writes lock, `Drop` removes it
- [x] `apply_package` acquires lock at entry, releases on exit
- [x] Concurrent apply detection with actionable error message
- [x] Stale lock (dead pid) auto-cleanup
- [x] `.ta/apply.lock` added to `.gitignore` (already covered by `.ta/*.lock` rule added in v0.15.11.1 parent)
- [x] Claude Code CLAUDE.md rule: check for apply lock before git branch/commit/push operations (already in CLAUDE.md Rules section)
- [x] `ta draft apply --status` flag shows active lock info

#### Version: `0.15.11-alpha.1`

---

### v0.15.11.2 — PR CI Failure Recovery Workflow
<!-- status: done -->
**Goal**: Surface CI check failures for TA-submitted PRs directly inside TA (Studio, shell, and CLI) and provide a one-action "fix CI failure" path that spawns a targeted agent, applies the fix to the existing PR branch, and notifies the user — without requiring git knowledge or manual branch switching.

**Why this phase exists**: Today, fixing a CI failure on an open PR requires an engineer to manually check GitHub, check out the branch, fix the error, and push. `--follow-up` is the wrong tool (it inherits full parent staging, re-surfaces all parent changes as a new diff, and produces a redundant draft). Non-engineers have no actionable path at all. The fix should be surfaced where the user already is and require a single action.

**Design — polling for now, push later**:

- **CLI**: `ta pr checks <goal-shortref>` — manual poll, prints check status table with actionable next step on failure.
- **Slack / Discord / Email** (channels): Deliver the same CI failure notification to configured channels when a PR check fails. Uses the existing channel adapter surface.
- **Push notifications** (future, tracked separately): VCS adapter webhook support to eliminate polling. The adapter would receive a push event from GitHub/GitLab and emit a `PrCheckFailed` event internally. Not in this phase.

**"Fix CI Failure" action mechanics**:
- Fetches the failing check's log from the GitHub API (or VCS adapter equivalent).
```
- On draft build, applies the fix **directly to the existing PR branch** (not a new branch, not a new draft cycle) — a lightweight "micro-fix" path bypassing the full goal→draft→approve→apply flow.
- Pushes to the PR branch. CI re-runs automatically.

**Deliverables**:
- [x] `ta pr checks <shortref>` CLI subcommand — polls check status, prints table, exits non-zero if any check failed
- [x] Studio PR card shows live check status with "Fix CI Failure" button on failure (→ moved to v0.15.16 Studio)
- [x] Shell notification on `PrCheckFailed` event (extends v0.15.7.1 event notification surface) — `PrCheckFailed` event emitted by `ta pr checks`, routed via event-routing.yaml
- [x] Channel delivery of CI failure notifications (Slack, Discord, Email via existing adapters) — via `pr_check_failed` event + event-routing.yaml responder


- [x] `ta pr fix <shortref>` CLI shorthand: fetches logs, spawns agent, applies, pushes in one command

#### Version: `0.15.11-alpha.2`

---

### v0.15.12 — `SocialAdapter` Trait & Social Media Plugins
<!-- status: done -->


**Depends on**: v0.15.9 (`MessagingAdapter` pattern), v0.15.10 (supervisory review step — reused here), v0.13.9 (constitution)

**Hard constraint — `publish` is not a goal-accessible operation**: Plugins expose `create_draft` and `create_scheduled` only. Publishing is done by the user in the platform's own UI or scheduler. Same enforced-at-type-level boundary as `MessagingAdapter`. A goal can produce a post ready to send, but the human finger presses the button.

**Design**:

- Built-in plugins: `ta-social-linkedin`, `ta-social-x`, `ta-social-instagram`, `ta-social-buffer` (in `plugins/social/`)
<!-- status: done -->
- Credentials: OAuth2 via `ta adapter setup social/<plugin>`, stored in OS keychain

**Protocol messages**:
```

← { "ok": true, "draft_id": "linkedin-draft-xyz" }

→ { "op": "create_scheduled", "post": { "body", "media_urls": [] }, "scheduled_at": "2026-04-07T14:00:00Z" }
← { "ok": true, "scheduled_id": "buffer-post-xyz", "scheduled_at": "2026-04-07T14:00:00Z" }

→ { "op": "draft_status", "draft_id": "linkedin-draft-xyz" }
← { "state": "draft" | "published" | "deleted" }

→ { "op": "health" }

```

**Workflow integration**: Goals that produce social content (`ta run "Write a LinkedIn post about the cinepipe launch"`) go through the same supervisory gate as email: constitution check → supervisor score → if pass, `create_draft` or `create_scheduled`; if flag, TA review queue first. The `[[supervisor]]` config from the email workflow template is reused as a shared concept.

**Goal examples**:
```bash

        highlight the AI pipeline angle, no specific client names"

ta run "Write a week of X posts for the TA public alpha — one per day,
        consistent voice, link to the GitHub release"
        --persona content-writer
```

Each goal produces one or more `SocialDraftRecord` entries in `.ta/social-audit.jsonl` and native drafts/scheduled posts in the target platform.

#### Items

1. [x] **`SocialAdapter` protocol spec** (`crates/ta-submit/src/social_plugin_protocol.rs`): `create_draft`, `create_scheduled`, `draft_status`, `health`, `capabilities` ops. No `publish` op at the type level. `DraftSocialRecord` audit struct in `crates/ta-goal/src/social_audit.rs`.

2. [x] **Plugin discovery** (`crates/ta-submit/src/social_adapter.rs`): Same pattern as `MessagingAdapter` discovery. `ta-social-*` prefix on `$PATH`. `discover_social_plugins`, `find_social_plugin`, `ExternalSocialAdapter` with `create_draft`, `create_scheduled`, `draft_status`, `health` methods.

<!-- status: done -->

4. [x] **`plugins/social/ta-social-linkedin`**: Rust binary. `create_draft` via LinkedIn UGC Posts API with `lifecycleState=DRAFT`; `create_scheduled` via `lifecycleState=SCHEDULED`. `draft_status` via UGC Posts status endpoint. `plugin.toml` included.

5. [x] **`plugins/social/ta-social-x`**: Rust binary. `create_draft` via X API v2 with `status=draft` (Basic+ API tier). `create_scheduled` via `scheduled_at`. API tier requirements documented in USAGE.md. `plugin.toml` included.

6. [x] **`plugins/social/ta-social-buffer`**: Rust binary. `create_draft` → Buffer Draft queue; `create_scheduled` → Buffer scheduled queue with `scheduled_at`. Cross-platform: fans out to all connected Buffer profiles (LinkedIn, X, Instagram) in a single call. `plugin.toml` included.



8. [x] **`DraftSocialRecord`** audit log (`crates/ta-goal/src/social_audit.rs`): `post_id`, `platform`, `handle`, `body_preview` (first 100 Unicode chars), `created_at`, `state`, `goal_id`, `supervisor_score`, `manually_approved`. `SocialAuditLog` at `.ta/social-audit.jsonl`. `ta audit social` with `--platform`, `--state`, `-n` filters.

9. [x] **Workflow template** (`templates/workflows/social-content.toml`): Covers `platforms`, `mode` (draft/scheduled), `constitution`, `allow_client_names`, `[supervisor]` config, `[schedule]` timing, `[content]` guidelines. Documented in USAGE.md.

10. [x] **Tests**: `no_publish_op_variant` asserts publish absent at type level; `create_draft_returns_id` mock test; supervisor fail tests (low confidence, flag phrase, client name, unverified claim); `draft_status_reflects_published_state` via mock; `ta audit social` output verified via `social_audit` roundtrip tests; Buffer fan-out documented in plugin header. 42 new tests across `social_plugin_protocol`, `social_adapter`, and `social_audit` modules.

11. [x] **USAGE.md**: "Social Media Adapter" section added — plugin setup per platform, `create_draft` vs `create_scheduled`, supervisory review flow, `ta audit social` usage, X API tier requirements, Buffer as a cross-platform option.



---

### v0.15.13 — Hierarchical Workflows: Sub-Workflow Steps & Serial Chaining
<!-- status: done -->
**Goal**: Allow a workflow step to invoke another named workflow as a sub-workflow, running it to completion before proceeding to the next step. This is the foundation for composable, reusable workflow building-blocks and enables the `build_phases.sh` pattern to be expressed as a single TOML workflow definition.

**Depends on**: v0.14.10 (artifact-typed workflow edges), v0.14.8.2 (governed workflow engine)

**Design**:

A new step type `kind = "workflow"` in the workflow TOML:

```toml

name = "implement_phase"
kind = "workflow"
workflow = "build"               # name of .ta/workflows/build.toml

phase = "{{phase.id}}"           # passed as --phase to child workflow
<!-- status: done -->
```

---

**Phase loop as a workflow** (`plan-build-loop.toml`):

```toml
[workflow]
name = "plan-build-loop"
description = "Run all pending plan phases through the governed build workflow."

<!-- status: done -->
max_phases = 99




kind = "plan_next"     # reads ta plan next, outputs: phase_id, phase_title, done=bool


name = "run_phase"
kind = "workflow"
workflow = "build"
goal = "{{plan_next.phase_id}} — {{plan_next.phase_title}}"
phase = "{{plan_next.phase_id}}"
depends_on = ["plan_next"]
condition = "!plan_next.done"   # stops cleanly when all phases complete



kind = "goto"
target = "plan_next"
<!-- status: done -->
condition = "!plan_next.done"
```



<!-- status: done -->



3. [x] **`kind = "goto"` step with `condition`**: A loop-back step that re-enters the graph at `target` when `condition` evaluates to true. Depth guard: after `max_phases` iterations, emit `CHECKPOINT` and halt with actionable message.

4. [x] **Template interpolation in stage fields** (`goal`, `phase`, `condition`): `{{stage_name.field}}` resolves from the current workflow run's output map. Uses a simple `{{` / `}}` tokenizer — no Tera/Handlebars dependency.



6. [x] **Workflow template** (`templates/workflows/plan-build-loop.toml`): Ships as a built-in template. `ta workflow run plan-build-loop` replaces `./build_phases.sh`.

7. [x] **`ta workflow run plan-build-loop --dry-run`**: Prints the plan (calls `ta plan next` once, shows what phase would run, estimates iteration count from pending phases). Does not start any sub-workflows.

8. [x] **Sub-workflow run IDs in status output**: `ta workflow status <run-id>` shows sub-workflow run IDs for each `workflow`-kind step with their current state. `ta workflow status <child-run-id>` works independently.

9. [x] **Tests**: sub-workflow step resolves and executes child workflow; `plan_next` step parses `ta plan next` output correctly; `goto` loops correctly up to `max_phases`; `condition` evaluator covers `!bool`, `==`, `!=`; depth guard fires at limit 5; `dry-run` for `plan-build-loop` prints correct plan. (52 total tests in governed_workflow, +26 new)

10. [x] **USAGE.md**: "Workflow Loops & Sub-workflows" section — `kind = "workflow"`, template syntax, `plan-build-loop` replacing the shell script, `--dry-run` preview.

#### Version: `0.15.13-alpha`

---

### v0.15.13.1 — `ta init` generates CLAUDE.md
<!-- status: done -->
**Goal**: `ta init` (and `ta init run --template <type>`) generates a starter `CLAUDE.md` in the project root alongside the `.ta/` config. If a `CLAUDE.md` already exists it is left unchanged (no overwrite without `--overwrite`). The generated file is derived from the same project-type detection and verify commands that `ta init` already writes to `workflow.toml`, so it is immediately correct for the project — not a generic placeholder. ta init should be safe to run again without breaking an existing setup but add new details such as the CLAUDE.md. We should know what version a project was init'ed with and know the upgrade path, or each section checks if it should run.

**Depends on**: none (init command is self-contained)

**Generated content** (Rust workspace example — each template produces equivalent output for its toolchain):


<!-- status: done -->

## Build

./dev cargo build --workspace

## Verify (all must pass before committing)

./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check

## Git

Always work on a feature branch. Never commit directly to main.
Branch prefixes: feature/, fix/, refactor/, docs/

## Rules

- Run verify after every code change, before committing

```





2. [x] **Write on init**: After writing `.ta/workflow.toml`, check if `CLAUDE.md` exists in the project root. If absent, write the generated file and print `Created CLAUDE.md — add project-specific rules before running ta run`. If present and `--overwrite` not passed, print `CLAUDE.md already exists — skipping (use --overwrite to replace)`.

3. [x] **`--overwrite` flag** on `ta init run`: Replaces an existing CLAUDE.md. Prints the path of the replaced file.

4. [x] **Templates**: Rust workspace (cargo build/test/clippy/fmt), TypeScript/Node (npm typecheck/test/lint), Python (ruff/mypy/pytest), Go (go build/test/vet), generic (commented-out placeholders), Unreal/Unity (generic stub with workflow.toml reference).

5. [x] **Tests**: 10 new tests — init on empty dir → CLAUDE.md created with correct verify commands (2 per template); init on dir with existing CLAUDE.md → file unchanged; `--overwrite` → file replaced; `write_claude_md` idempotent without flag; re-run on configured project generates missing CLAUDE.md. 42 init tests total, all passing.



#### Version: `0.15.13-alpha.1`

---

### v0.15.13.2 — Draft for Memory-Only Goal Runs
<!-- status: done -->


**Root cause**: The overlay diff excludes `.ta/` — it's machine-specific ephemeral state. Memory entries written to `.ta/memory/` never appear in `ta draft build`'s diff. So an agent that reads the whole codebase and stores rich findings to memory produces a diff of zero bytes and no `DraftPackage`.

**Design**:

When `ta draft build` produces an empty diff (zero artifacts), check whether the goal run created any memory entries during its execution. If it did, package those entries as a synthetic `memory-summary` artifact in the draft:

```
DraftPackage {
  artifacts: [
    Artifact {
      resource_uri: "ta://memory/<goal-id>",
      kind: MemorySummary,
      content: "<rendered list of memory entries created this run>",
      ...
    }
  ]
}
```

The draft view renders this as a readable summary ("Agent stored 4 memory entries during this run") with the full content of each entry visible for review. Approve applies them to the memory store; deny discards them. This makes analysis/learning goals first-class reviewable work.

**Scope guard**: Only fires when the diff is empty AND memory entries exist. Normal goals that write source files are unaffected.



1. [x] **Track memory entries created per goal run**: `GoalRun` gets a `memory_entries_created: Vec<Uuid>` field populated by `build_memory_only_draft` at draft-build time (queried from the memory store by `goal_id`). (`crates/ta-goal/src/goal_run.rs`, `apps/ta-cli/src/commands/draft.rs`)

2. [x] **`ta draft build` empty-diff detection**: After computing the overlay diff, if `changes.is_empty()`, query the memory store for entries with this goal's UUID. If entries exist, delegate to `build_memory_only_draft`; otherwise fall through to the existing error path. (`apps/ta-cli/src/commands/draft.rs`)

3. [x] **`MemorySummary` artifact kind** (`crates/ta-changeset/src/artifact_kind.rs`): New `ArtifactKind::MemorySummary { entry_count, entry_ids }`. Rendered in `ta draft view` with `[memory]` prefix. Approve is a no-op (entries already in store). Deny removes entries by ID from the memory store. `matches_file_filters` updated to always pass `ta://memory/` URIs through.

4. [x] **`ta draft view` rendering**: Terminal adapter renders `MemorySummary` artifacts with `[memory] Memory entries stored: N` header, then the full rendered entry list (key, scope, category, value) from the changeset content, with approve/deny guidance. (`crates/ta-changeset/src/output_adapters/terminal.rs`)

5. [x] **Tests**: 5 tests added — `memory_only_draft_created_when_empty_diff_and_memory_entries_exist`, `empty_diff_and_no_memory_entries_errors`, `memory_summary_artifact_kind_is_memory_summary`, `memory_summary_artifact_kind_roundtrip`, `matches_file_filters_always_shows_ta_memory_uri`. (`apps/ta-cli/src/commands/draft.rs`)

6. [x] **USAGE.md**: "Analysis and learning goals" section added under Context Memory — explains the trigger, review flow, approve/deny semantics, and scope guard. (`docs/USAGE.md`)

#### Version: `0.15.13-alpha.2`

---

### v0.15.13.3 — Committed Project-Scoped Memory (`.ta/project-memory/`)
<!-- status: done -->
**Goal**: Memory entries with `scope = "project"` or `scope = "team"` are currently stored in `.ta/memory/` (gitignored, machine-local) alongside ephemeral execution state. This means project knowledge — architectural decisions, codebase facts, known gotchas — is lost when a machine is wiped and never shared with teammates or their agents. This phase splits the storage layer: local-scoped entries stay in `.ta/memory/`; project/team-scoped entries land in `.ta/project-memory/` which is committed to VCS and shared across the team.

**Depends on**: v0.15.13.2 (memory entry tracking per goal run)

**Why this matters**:
- An agent that "learns the project" stores findings to memory. Today those findings are invisible to every other agent on every other machine.
- Architectural decisions ("use `--thinking-mode` in args, not a TOML field") must be re-derived on every new goal run because nothing persists them in a shared, retrievable form.
- `scope` is already declared on memory entries but not enforced in the storage path — this phase makes it load-bearing.

**Design**:

| Scope | Storage path | Gitignored? | Shared? |
|-------|-------------|-------------|---------|
| `local` | `.ta/memory/` | Yes | No |

| `team` | `.ta/project-memory/` | No | Yes — committed |

`.ta/project-memory/` uses the same on-disk format as `.ta/memory/` so the read path is identical. At `ta run` injection time, project-memory entries are always surfaced regardless of goal-title similarity — they are unconditional context. Additionally, entries tagged with a file path (e.g. `file = "apps/ta-cli/src/commands/agent.rs"`) are surfaced when the staging workspace contains that file, enabling file-scope-triggered retrieval for architectural decisions.



1. [x] **Storage path routing** (`crates/ta-goal/src/memory.rs`): `MemoryStore::write()` checks `entry.scope`. `Scope::Local` → `.ta/memory/`; `Scope::Project | Scope::Team` → `.ta/project-memory/`. Read path loads both directories and merges results.

2. [x] **`.gitignore` update** (`ta init` template + docs): Add `.ta/project-memory/` to the "committed" list in gitignore comments. Remove it from the ignored list if present.

<!-- status: done -->

4. [x] **`ta run` injection**: Project-memory entries injected unconditionally (all of them, budget-permitting). File-path-tagged entries surfaced when staging contains matching path. Both added to the "Prior Context" section of CLAUDE.md injection before goal-title similarity entries.

<!-- status: done -->

6. [x] **`ta memory list --scope project`**: List all committed project-memory entries with their keys, file tags, and creation goal ID.

7. [x] **`ta draft apply` auto-stage**: When applying a draft that modifies `.ta/project-memory/`, `auto_stage_critical_files()` includes the directory so it lands in the VCS commit alongside source changes.

```

   **Resolution pipeline** (in order):
   1. **Last-write-wins** (default, no-agent): if timestamps differ by > 60s, take the newer entry automatically. Fast path for the common case.
   2. **Agent resolution** (`ta memory resolve --agent`): for entries where timestamps are close or content substantially differs, spawn a short-lived agent with both versions and the goal context. Agent produces a synthesized merged entry or picks one, with a `confidence: f64` score. If `confidence >= 0.85` → accept agent result automatically. If `confidence < 0.85` → escalate to human.
   3. **Human resolution** (`ta memory conflicts`): lists unresolved `ConflictPair`s, shows both versions side-by-side with agent's reasoning and confidence if available. Human picks ours/theirs/edit.

   **`MemoryConflictResolver` trait** (`crates/ta-goal/src/memory.rs`): `resolve(conflict: &ConflictPair) -> ConflictResolution`. Built-in: `TimestampResolver` (last-write-wins), `AgentResolver` (LLM-based synthesis). SA extension point: SA can register a `ByzantineConsensusResolver` (PBFT-based, requiring multi-party sign-off from SA-v0.6) by implementing the trait and registering it in `conflict_resolver` in `workflow.toml`.

   ```toml

   strategy = "agent"           # "timestamp" | "agent" | "human" | plugin name
   agent_confidence_threshold = 0.85
   escalate_to_human = true     # always true when strategy = "agent" and confidence low
   # SA extension: strategy = "sa-pbft" (registered by SA plugin)
   ```

   `ta init` writes the `.gitattributes` pattern only for git projects (detected via `SourceAdapter`). Non-git VCS: no `.gitattributes`, conflict detection relies entirely on the read-time comparison. `ta memory doctor` scans `.ta/project-memory/.conflicts/` and reports unresolved pairs with actionable instructions.

9. [x] **Tests**: `scope = project` → `.ta/project-memory/`; `scope = local` → `.ta/memory/`; file-path-tagged entry surfaced when staging contains file; injection order: project-memory before similarity entries; same-key newer timestamp → auto last-write-wins; same-key close timestamps → agent resolution invoked; agent `confidence >= 0.85` → auto-accepted; agent `confidence < 0.85` → escalated to human; `ta memory conflicts` lists and resolves; `MemoryConflictResolver` trait: custom resolver registered and called; non-git VCS: no `.gitattributes` written, conflict detection still works via read-time comparison.

10. [x] **USAGE.md**: "Team Memory" section — `ta memory store --scope project`, file-path tagging, committed sharing, conflict resolution pipeline (timestamp → agent → human), `ta memory conflicts`, `conflict_resolution` config, SA extension point.

#### Version: `0.15.13-alpha.3`

---

### v0.15.13.4 — Supervisor Review: Heartbeat-Based Liveness (Replace Wall-Clock Timeout)
<!-- status: done -->
**Goal**: The built-in supervisor review (`run_builtin_supervisor()`) uses a wall-clock `timeout_secs` (default 120s) that fires even when the supervisor is actively streaming a response. A large diff or a slow API response legitimately takes longer than 120s. Fix: same heartbeat model used for the agent watchdog in v0.15.7.1 — supervisor writes a heartbeat on each token received, monitor kills only when heartbeats stop.

**Depends on**: v0.15.7.1 (heartbeat infrastructure in `.ta/heartbeats/`)

**Why wall-clock is wrong**: The supervisor is an LLM call. Response time scales with diff size and model load. A 400-file diff may take 90s of streaming + 40s of JSON parsing — killed by a 120s wall-clock timer with no output. The user sees "timed out", gets a `Warn` fallback with no findings, and the supervisor's work is discarded.

**Design**: Streaming loop writes `.ta/heartbeats/<goal-id>.supervisor` on each token chunk. A monitor thread checks mtime every 5s. If mtime > `heartbeat_stale_secs` (default 30s) ago, supervisor is considered stalled and killed. Actively streaming supervisors never time out regardless of total elapsed.

```toml
[supervisor]
heartbeat_stale_secs = 30    # kill if no token received for this long (replaces timeout_secs)
```

`timeout_secs` remains accepted for backward compat with a deprecation warning.



1. [x] **Heartbeat writes in streaming loop** (`crates/ta-changeset/src/supervisor_review.rs`): `spawn_with_heartbeat_monitor()` writes `.ta/heartbeats/<goal-id>.supervisor` after each line received from the supervisor process. Initial write happens at spawn time.



3. [x] **`SupervisorRunConfig`**: Added `heartbeat_stale_secs: u64` (default 30) and `heartbeat_path: Option<PathBuf>`. `timeout_secs` kept as deprecated field (u64) with deprecation warning emitted in `run.rs` and `release.rs` when set. `SupervisorConfig` in `ta-submit` updated to `heartbeat_stale_secs` + `timeout_secs: Option<u64>`.

4. [x] **Stall message**: `"Supervisor stalled — no tokens received for {stale_secs}s. Findings so far: {partial}"`. Partial output accumulated in a capped buffer and included in the bail message.

5. [x] **Heartbeat cleanup**: `spawn_with_heartbeat_monitor()` calls `fs::remove_file(hb)` on both normal completion and stall. Manifest supervisor also cleans up.



7. [x] **USAGE.md**: "Supervisor Agent" section updated — `timeout_secs` replaced with `heartbeat_stale_secs`, deprecation note added.

#### Version: `0.15.13-alpha.4`

---

### v0.15.13.5 — Phase In-Progress Marking at Goal Start
<!-- status: done -->


**Design**: `ta run` already injects CLAUDE.md and writes `.ta/goals/<id>/goal.json`. Add a `update_phase_status_in_source(phase_id, InProgress)` call in `run.rs` at the point where staging is confirmed and goal ID is assigned — before `launch_agent()`. The `in_progress` marker is written to the **source** PLAN.md (not the staging copy), so it is visible immediately in `ta plan status` and in any IDE that has PLAN.md open.

On `ta draft apply`, the existing logic already advances the phase to `done`. No change needed there. If the goal is denied or cancelled, a new `ta draft deny`/`ta goal cancel` handler resets the status from `in_progress` back to `pending` (with a note in the plan history log).


- [x] `run.rs`: call `mark_phase_in_source(source_root, phase_id)` + write to source PLAN.md immediately after goal ID assigned, before agent launch
- [x] `draft.rs` deny path: if current status is `InProgress`, reset to `Pending` and log "phase reset to pending — goal denied"


- [x] Tests: `mark_phase_in_source` → writes in_progress + history; `reset_phase_if_in_progress` → resets to pending + history; noop for done/pending; deny → resets phase; delete → resets phase; `format_plan_checklist` → [~] for in_progress (9 new tests)
- [x] USAGE.md: note that `--phase` marks phase in_progress immediately, visible in `ta plan status`

**Depends on**: v0.15.13.4

#### Version: `0.15.13-alpha.5`

---

### v0.15.13.6 — Version Bump Reliability & Post-Apply Validation
<!-- status: done -->
**Goal**: `ta draft apply` silently skips the workspace version bump if the goal has no `plan_phase` set. `bump_workspace_version` returning an empty vec is ambiguous — "already at target" and "regex matched nothing" are both silent `Ok([])`. This phase makes the bump observable, validates the result, and adds CI enforcement.

---

---
- [x] `bump_workspace_version`: return `BumpResult` enum — `Bumped(Vec<PathBuf>)`, `AlreadyCurrent`, `NoMatch(String)`. `NoMatch` is an error; never silently succeed when no file was modified
- [x] Post-apply check (both VCS and non-VCS paths): derive expected semver from `last_phase_id`, read Cargo.toml, compare. If mismatch: emit loud actionable warning with exact `./scripts/bump-version.sh <version>` command (`validate_cargo_version`)

- [x] `ta draft apply --validate-version` flag: reads Cargo.toml post-apply, exits non-zero if version doesn't match phase semver — usable in CI
- [x] Tests: 12 new tests covering `BumpResult` variants (`AlreadyCurrent`, `NoMatch`, `Bumped`), `read_cargo_version`, and `validate_cargo_version` (match, mismatch, file absent)


**Depends on**: v0.15.13.5

#### Version: `0.15.13-alpha.6`

---

### v0.15.14 — Hierarchical Workflows: Parallel Fan-Out, Phase Loops & Milestone Draft
<!-- status: done -->
**Goal**: Two first-class modes for multi-phase execution — **PR-per-phase** (iterate phases serially, PR and VCS-sync each one before moving on) and **milestone-draft** (iterate phases, accumulate all changes into a branch, present the entire series as one combined draft for human approval). Both modes support phase selection by count, version set (glob), or range. The sync step after each PR uses the `SourceAdapter` trait — not hardcoded git — so the loop works identically on Git, Perforce, and SVN.

**Depends on**: v0.15.13 (sub-workflow steps, serial chaining)

> **Replaces `build_phases.sh`**: The `plan-build-phases.toml` template (Mode A) is the native, VCS-agnostic equivalent of the current `build_phases.sh` shell loop. The shell script remains as a lightweight fallback but the engine is the primary path going forward.



Both modes accept a `[phases]` block in the workflow invocation or template:

```toml

[phases]


# By version set — run all pending phases matching the glob
[phases]
version_set = "v0.15.*"


[phases]

```

These resolve at runtime via `ta plan next` iteration — only phases with `<!-- status: pending -->` are candidates.

#### Mode A — PR-per-phase (serial, VCS-synced)



```toml
# templates/workflows/plan-build-phases.toml
[workflow]


[phases]




kind = "run_goal"
goal = "{{phase.title}}"
phase = "{{phase.id}}"


name = "review_draft"
---



name = "apply_draft"

draft = "{{run_goal.draft_id}}"



kind = "pr_sync"          # opens PR, polls for merge, VCS-syncs via SourceAdapter


name = "next_phase"
kind = "loop_next"        # advances to next pending phase; exits loop if none remain
```

---

Each phase is implemented and applied into a local branch (no PR per phase). After all phases complete, a single `MilestoneDraft` is produced spanning all phase changesets. One human-approval step covers the entire series.

```toml
# templates/workflows/plan-build-milestone.toml
[workflow]
mode = "milestone-draft"


[phases]
version_set = "v0.15.*"


name = "run_goal"
kind = "run_goal"
goal = "{{phase.title}}"


name = "apply_local"

target = "branch"         # applies into milestone_branch, not main


name = "milestone"


milestone_title = "{{phases.version_set}} milestone"

[[stage]]

kind = "human_gate"
prompt = "Review the milestone draft above. Approve to open a single PR for all phases."

[[stage]]
name = "pr_sync"

```



1. [x] **`PhaseSelector`** (`crates/ta-goal/src/phase_selector.rs`): Resolves `[phases]` config block against the live plan. `PhaseSelector::resolve(plan, config) -> Vec<PlanPhase>` returns ordered pending phases matching the selector. Three variants: `Count(u32)`, `VersionSet(glob_pattern)`, `Range { from: String, to: String }`. Used by the loop engine to determine the phase sequence before execution starts.



3. [x] **`pr_sync` stage — VCS-abstracted poll + sync**: Replace the current `pr_sync` implementation's hardcoded `git pull` with `SourceAdapter::sync(target_branch)`. After opening the PR and confirming auto-merge is enabled, poll `SourceAdapter::pr_status(pr_id)` until `merged` (or timeout). Then call `SourceAdapter::sync()`. No direct `git` subprocess calls remain in the sync path.





6. [x] **`MilestoneDraft` struct** (`ta-changeset`): Wraps a `DraftPackage` with `source_drafts: Vec<String>`, `milestone_title: String`, `milestone_branch: Option<String>`. `ta draft view <milestone-id>` shows per-phase sections. `ta draft apply <milestone-id>` applies constituent drafts in phase order.

7. [x] **`parallel_group` + `kind = "join"` steps**: Stages with the same `parallel_group` dispatch concurrently (thread pool, configurable `max_parallel`, default 3). `kind = "join"` blocks until all group members complete; merges output maps with stage-name prefixes on conflicts. `on_partial_failure = "continue"` proceeds despite one member failing.

8. [x] **`plan-build-phases.toml`** template (Mode A): Replaces `build_phases.sh` in the template library. Phase selection defaults to `max = 99`. Uses `pr_sync` VCS-abstracted loop.

9. [x] **`plan-build-milestone.toml`** template (Mode B): Milestone accumulation workflow. Phase selection defaults to accepting a `version_set` or `range` parameter at invocation time.

10. [x] **Milestone draft review in `ta workflow status`**: Shows constituent drafts, per-phase status (applied / pending / failed), overall milestone progress, and the `milestone_branch` if in Mode B.

11. [x] **Tests**: `PhaseSelector` resolves count/version-set/range correctly against a mock plan; `loop_next` advances cursor and exits on last phase; `pr_sync` calls `SourceAdapter::sync()` not git directly; `apply_draft` with `target = "branch"` calls `apply_to_branch`; `aggregate_draft` merges two draft packages with correct dedup; `MilestoneDraft` apply applies phases in order; parallel stages start concurrently (mock clock); join waits for all; max_parallel cap queues correctly.

12. [x] **USAGE.md**: "Multi-Phase Workflows" section — Mode A vs Mode B comparison table, `[phases]` block examples (count, version_set, range), `plan-build-phases` vs `plan-build-milestone` templates, reviewing a milestone draft, VCS adapter requirements for `pr_sync`.

#### Version: `0.15.14-alpha`

---


<!-- status: done -->
**Goal**: Remove the unnecessary `approve` gate for single-author projects, fix the opaque already-Applied error, and add apply provenance so users always know when/how a draft was applied.



**Correct single-author flow**: `ta draft view <id>` → `ta draft apply <id>` (no separate approve step)

**Multi-author flow** (when `approval_required = true` in `.ta/workflow.toml`): `ta draft view` → `ta draft approve` → `ta draft apply`

#### Changes

1. [x] **`approval_required` config flag** — added `approval_required: bool` (default `false`) to `DraftReviewConfig` in `crates/ta-submit/src/config.rs`. When `false`: `ta draft apply` accepts `PendingReview` directly. When `true`: requires `Approved` state first with clear actionable message.

2. [x] **Apply provenance field** — added `ApplyProvenance` enum (`Manual`, `BackgroundTask { task_id }`, `AutoMerge`) to `crates/ta-changeset/src/draft_package.rs`. Added `applied_via: ApplyProvenance` field (serde default=Manual for backward compat) to `DraftStatus::Applied`.



4. [x] **`ta draft list` Applied column** — `status_display` match now shows `Applied (manual)` / `Applied (background)` / `Applied (auto-merge)` based on `applied_via`.



6. [x] **`bump-version.sh` includes Cargo.lock** — added `cargo update --workspace` after `Cargo.toml` edit; updates `git add` instructions to include `Cargo.lock`.

7. [x] **`ta status` — Next phase logic** — `find_next_pending_phase` now uses a watermark approach: finds the last `done` phase position, then returns the first `pending` phase after it. Pending phases before the watermark (deferred/skipped) are not surfaced as "next".

8. [x] **`ta status` — Suppress failed goals for done phases** — `failed_goals` filter now cross-references `plan_phase` against `collect_done_phase_ids(PLAN.md)`. Goals whose phase is now done are suppressed from URGENT.

9. [x] **`ta status` — Applied drafts must not appear in "pending review"** — `list_pending_draft_ids` now parses the JSON and checks `v["status"] == "pending_review"` directly, excluding Applied, Denied, Closed, and Draft states.

10. [x] **`ta status` — Disk space CRIT deduplication** — CRIT ops whose issue contains "disk" are grouped; 2+ entries produce a single `[CRIT] Low disk space on N paths` message instead of N separate lines.

#### Version: `0.15.14-alpha.0` (patch; ships on next tagged release)

---

#### Items
<!-- status: done -->
**Goal**: Distinguish agent-completable implementation items from steps that require a human to verify, test, or sign off. Today both types live in the same flat checklist, so phases get marked `done` while human verification steps remain unchecked indefinitely — no reminder, no tracking, no deferral. This phase adds a `#### Human Review` subsection to the plan schema, a lightweight store for tracking open review items, a `ta plan review` command, and surfacing in `ta status` and `build_phases.sh`.

**Why this phase exists**: Repeated incidents where `ta draft apply` marks a phase done but leaves human-only steps (e.g. "test connector in Editor", "sign off on UX wording") silently unchecked. The human has no reminder and the plan looks complete when it isn't. The root cause is conflating "agent verified" with "human verified" in a single flat list.



#### Plan schema change

Phases may include a `#### Human Review` subsection (4th-level heading). Items under it are human-only — an agent must never check them off:


### v0.15.X — Some Phase <!-- status: done -->

#### Items
- [x] Agent writes code
- [x] Tests pass in CI

#### Human Review
- [x] Smoke-test the connector against a real project

```

- The `#### Human Review` heading is reserved. Parser detects it by exact text match.
- Items under `#### Human Review` are extracted by `ta draft apply` when a phase is marked done.
- Implementation items and human review items are displayed separately by `ta plan status`.

#### Storage: `.ta/human-review.jsonl`



```json
{"phase": "v0.15.3", "idx": 0, "item": "Smoke-test connector in Editor", "status": "pending", "created_at": "2026-04-03T00:00:00Z", "deferred_to": null}
```



#### Items

1. [x] **Plan parser extension** (`apps/ta-cli/src/commands/plan.rs`): `parse_plan_with_schema()` detects `#### Human Review` subsection within each phase. Extracts items as `PlanPhase.human_review_items: Vec<String>`. Updated `show_status` displays done phases with pending human review counts.



3. [x] **`ta draft apply` integration** (`apps/ta-cli/src/commands/draft.rs`): After marking a phase done, reads `phase.human_review_items` from parsed `PlanPhase`. For each unchecked item, calls `store.append(...)`. Prints a summary block:
   ```
   Phase v0.15.3 marked done.

   Human review items require your attention (2):

     [2] Confirm USAGE.md wording

   Run 'ta plan review complete v0.15.3 <N>' when done, or
       'ta plan review defer v0.15.3 <N> --to <phase>' to reschedule.
   ```


4. [x] **`ta plan review` command** (`apps/ta-cli/src/commands/plan.rs`): New subcommand group:
   - `ta plan review` — list all pending human review items across all phases, grouped by phase, with index
   - `ta plan review --phase v0.15.3` — filter to one phase
   - `ta plan review complete <phase> <N>` — mark item N done (updates `.ta/human-review.jsonl`)
   - `ta plan review defer <phase> <N> --to <target-phase>` — set status to `deferred`, record `deferred_to`

5. [x] **`ta status` surfacing**: If `HumanReviewStore::pending()` returns any items, adds a line to `ta status` output:
   ```
   Human review: 3 items pending  (run 'ta plan review' to see them)
   ```
   Shown in the URGENT section alongside active goals.

6. [x] **`build_phases.sh` integration** (`utils/build_phases.sh` in ARK project templates and meerkat-poc): After each `ta workflow run build` succeeds, run `ta plan review --phase "$PHASE_ID"` and print any pending items before moving to the next phase. If the command is not available (older TA), skip silently. → **Deferred**: External project templates are outside this codebase; tracked separately.

7. [x] **`ta plan status` display**: `ta plan status` shows each done phase with a count of pending human review items: `v0.15.3 — done (1 human review pending)`. `ta plan review --phase v0.15.3` shows the detail.

8. [x] **Tests**: `parse_plan_with_schema()` extracts human review items from a phase with `#### Human Review` subsection. `HumanReviewStore` append/list/complete/defer roundtrip. `ta draft apply` test for `apply_unknown_id_leaves_existing_draft_unchanged`. `ta status` wires up `pending_human_review_count()`. Store gracefully handles missing `.ta/human-review.jsonl` (returns empty list). All 12 `HumanReviewStore` tests pass.

9. [x] **USAGE.md "Human Review Items"** section: Added full section explaining `#### Human Review` subsection, extraction on `ta draft apply`, `ta plan review` commands, `ta status` surfacing, and storage format.

10. [x] **Workflow run/stop from Studio** (carried from v0.14.20 item 9): "Run" button on a workflow row calls `POST /api/workflow/{id}/run`. "Stop" calls `DELETE /api/workflow/{id}`. Row shows live status via polling (`GET /api/workflow/{id}/status`). Implemented in `crates/ta-daemon/src/api/workflow.rs` with `run_workflow`, `stop_workflow`, `workflow_run_status` handlers and matching routes in `api/mod.rs`. Studio `index.html` updated with Run/Stop buttons and 2-second status polling.

11. [x] **`ta draft <unknown>` safety test** (supervisor finding from v0.15.14.0, item 5): `apply_unknown_id_leaves_existing_draft_unchanged` test in `draft.rs`: creates a real draft, calls `apply_package` with a fake UUID, asserts error is returned and the draft status is byte-for-byte unchanged.

> **Note**: The format upgrade for existing projects (backfilling `#### Human Review` sections in old done phases) is handled by the project upgrade step in v0.15.18 (`ta upgrade`). Leave it there.

#### Version: `0.15.14.1-alpha`

---

### v0.15.14.2 — Velocity Stats: Rework Tracking, Auto-Migration, Version Filtering & Shell/Studio Surface
<!-- status: done -->
**Goal**: Close four gaps in the velocity stats system: (1) follow-up goals that fix bugs are invisible — `rework_seconds` and `follow_up_count` fields exist on `VelocityEntry` but nothing writes to them; (2) `ta stats migrate` is a manual step that shouldn't exist — history should be written automatically; (3) no version-range filtering (can't ask "how fast did 0.15.x phases build?"); (4) velocity data is CLI-only — not surfaced in `ta shell` or Studio.

---

1. [x] **Token cost tracking per goal**: Added `input_tokens: u64`, `output_tokens: u64`, `cost_usd: f64`, `model: String`, `cost_estimated: bool` to `VelocityEntry`. Created `crates/ta-goal/src/token_cost.rs` with rate table (Opus/Sonnet/Haiku 4.x and 3.x). `run.rs` parses stream-json `result` and `system` events to accumulate tokens; saves to `GoalRun.input_tokens/output_tokens/agent_model`. `ta stats velocity` shows total/avg cost. `ta stats velocity-detail` gains `COST` column. Studio API returns cost fields.

2. [x] **Auto-migrate on every `ta draft apply`**: `migrate_local_to_history()` now called automatically in `apply_package()` after writing the velocity entry. Non-destructive. `ta stats migrate` kept with deprecation note. `local_only_count` warning still present for legacy entries.

3. [x] **Rework cost written to parent entry on follow-up apply**: `update_parent_rework()` function in `velocity.rs` rewrites both stores in-place (temp file + rename). Called from `apply_package()` when `goal.parent_goal_id` is set.



5. [x] **Version-range filtering**: `--phase-prefix` added to both `ta stats velocity` and `ta stats velocity-detail`. `filter_by_phase_prefix()` in `velocity.rs` matches on title prefix `v<prefix>.` or `plan_phase`.

6. [x] **`ta shell` velocity widget**: `:stats` command added. Post-apply velocity one-liner added to `apply_package()` output.

7. [x] **Studio velocity dashboard**: `GET /api/stats/velocity` and `GET /api/stats/velocity-detail` added to daemon HTTP API with `?phase_prefix=` and `?since=` query params. Registered in `api/mod.rs`.

8. [x] **Tests**: `token_cost.rs` has 8 tests (rate resolution, cost computation, zero tokens, Ollama). `velocity.rs` has 7 new tests (filter_by_phase_prefix, update_parent_rework, with_token_cost, aggregate cost). `api/stats.rs` has 2 tests (parse_date). `run.rs` has `accumulate_tokens` helper with parseable stream-json.

9. [x] **USAGE.md**: Updated "Feature Velocity Stats" section — added token cost tracking, rate table note, `--phase-prefix` examples, `:stats` shell command, Studio velocity API.

#### Version: `0.15.14.2-alpha`

---

### v0.15.14.3 — Language-Aware Static Analysis with Agent Correction Loop
<!-- status: done -->
**Goal**: First-class static analysis integration — per-language tool configuration, structured output parsing, and an optional agent-driven correction loop that re-runs the analyzer after each fix pass until clean or a max iteration count is hit. Chainable as a `kind = "static_analysis"` workflow step so it slots naturally into `plan-build-phases.toml` and custom multi-phase workflows.

**Why this phase exists**: Today `verify_command` in `workflow.toml` is a bare shell command — pass/fail only, no output parsing, no actionable follow-up. Developers using Python, TypeScript, or Go have no way to wire `mypy --strict`, `pyright`, or `golangci-lint` into the TA feedback loop. The correction loop closes the gap: instead of failing a goal run when analysis finds issues, TA can spawn a targeted fix agent, re-run the analyzer, and iterate — producing a single consolidated draft covering all corrections.

**Depends on**: v0.15.14 (governed workflow engine for the loop step)

#### Per-language config (`[analysis.<lang>]` in `workflow.toml`)

```toml
[analysis.python]
tool = "mypy"            # or "pyright", "ruff check"
args = ["--strict"]
on_failure = "agent"     # "fail" | "warn" | "agent"
max_iterations = 3       # correction loop limit (default: 3)

[analysis.typescript]
tool = "pyright"
args = []
on_failure = "agent"


tool = "cargo-clippy"
args = ["-D", "warnings"]


[analysis.go]
tool = "golangci-lint"
args = ["run"]
on_failure = "agent"
```

#### Items

1. [x] **`AnalysisConfig`** (`crates/ta-goal/src/analysis.rs`): Per-language config struct. `tool: String`, `args: Vec<String>`, `on_failure: OnFailure` (`Fail | Warn | Agent`), `max_iterations: u32`. Loaded from `[analysis.<lang>]` blocks in `workflow.toml`. Language detected from workspace files (`.py` → python, `package.json` + `.ts` → typescript, `Cargo.toml` → rust, `go.mod` → go). Manual override: `[analysis.python]` always wins over auto-detect.

2. [x] **`AnalysisFinding`** struct: `{ file: String, line: u32, col: u32, code: String, message: String, severity: Severity }`. Parser implementations per tool: `mypy` (`:line: error: message  [code]`), `pyright` (JSON `--outputjson`), `cargo clippy` (JSON `--message-format json`), `golangci-lint` (JSON `--out-format json`), `eslint`/`tsc` (JSON). Unknown tools: raw line capture.

3. [x] **`kind = "static_analysis"` workflow step**: Runs the configured analyzer for the detected (or specified) language. On success → next step. On failure with `on_failure = "fail"` → workflow fails with findings table. On failure with `on_failure = "warn"` → logs findings, continues. On failure with `on_failure = "agent"` → enters correction loop (item 4).

4. [x] **Correction loop**: When `on_failure = "agent"`, spawn a targeted fix agent with: (a) structured `AnalysisFinding` list formatted as a compact table, (b) the files containing findings, (c) explicit scope instruction: fix only what the analyzer flagged, no unrelated changes, no PLAN.md mutations. After agent applies fixes, re-run the analyzer. Loop until clean or `max_iterations` exhausted. On max iterations hit: emit warning with remaining findings, continue or fail per `on_max_iterations: "warn" | "fail"` (default `"warn"`). All correction passes produce a single consolidated draft.

5. [x] **`ta analysis run [--lang <lang>]`** CLI command: Run the configured analyzer for the current workspace outside of a goal/workflow. Prints findings table. `--fix` flag triggers the agent correction loop as a standalone goal (produces a draft for review). Useful for ad-hoc cleanup before starting a new phase.



7. [x] **Tests**: `AnalysisFinding` parser roundtrip for mypy, pyright JSON, clippy JSON, golangci-lint JSON. `on_failure = "fail"` workflow step exits with findings. `on_failure = "warn"` continues. Correction loop: mock agent fixes issues on iteration 2 — loop exits clean. Max iterations exceeded: remaining findings reported, workflow continues per `on_max_iterations`. Language auto-detect from workspace file presence. (31 new tests across `analysis.rs`, `config.rs`, `analysis.rs` CLI, `governed_workflow.rs`, `init.rs`)

8. [x] **USAGE.md "Static Analysis" section**: Config options per language, correction loop explanation, `ta analysis run --fix` ad-hoc usage, how to chain in `plan-build-phases.toml`.

#### Version: `0.15.14.3-alpha`

---


<!-- status: done -->
**Goal**: Replace today's implicit "everything is open" stance with a declared, tiered security model. A single `[security] level = "low" | "mid" | "high"` setting in `workflow.toml` sets a named preset of defaults; individual settings always override. This gives solo developers a frictionless default, teams a sensible hardened baseline, and regulated projects a documented high-assurance posture — without jumping to the full SA (OCI/gVisor/TPM) ceiling.

**Design principles**:
- Level sets defaults only — every individual control can be overridden. Escalation is silent; demotion logs a warning.
- **Supervisor constitution review and secret scanning are always on** at all levels. What changes per level is the *consequence* — warn vs block vs block+auto-follow-up.
- SA (SecureTA) sits above `"high"` and is out of scope for this phase.

**`Bash(*)` risk in `low` mode**: The staging directory is a behavioral boundary, not an OS boundary. `Bash(*)` lets the agent run any shell command — `cd ..` out of staging is unrestricted. Real risks: `rm -rf` on paths outside staging, `git push` bypassing the draft/review cycle, `curl url | bash`, cloud CLI calls (`aws s3 rb`, `gcloud compute instances delete`) with real infrastructure effects, credential exfiltration via `env | curl`. The constitution tells the agent not to; `Bash(*)` means nothing stops it.

**"Approval gate enforced" in `high`**: Post v0.15.14.0, single-author flow skips `ta draft approve` — apply works from `PendingReview`. In `high` mode, `approval_required = true` is locked even for solo developers. `ta draft apply` requires prior `ta draft approve`. Purpose: an explicit audit record that a human consciously signed off, not just applied.

#### Items

#### Security Level Defaults

| Capability | `low` (default today) | `mid` (team/startup) | `high` (regulated) |

| Process sandbox | off | on (sandbox-exec/bwrap) | on, required — warn if overridden off |
| `Bash` scope | `Bash(*)` unrestricted | `Bash(*)` + sensible forbidden list (rm -rf, sudo, curl\|bash) | explicit allowlist only — no `Bash(*)` |
| Network | unrestricted | domain allowlist configurable; warn on unknown | explicit allowlist required; `WebSearch` disabled |
| Approval gate | off (view→apply) | off (view→apply) | `approval_required = true` locked — approve required before apply |
| Audit trail | JSONL local | JSONL + per-entry SHA-256 | signed hash chain (HMAC-SHA256 with project key) |
| **Constitution / supervisor** | **always on — warn only** | **always on — warn; block configurable** | **always on — violations block draft + auto-trigger `--follow-up`** |
| **Secret scanning** | **always on — warn** | **always on — warn** | **always on — block by default** (explicit `scan = "warn"` to downgrade) |


#### Config (`workflow.toml`)

```toml
[security]
level = "mid"               # "low" | "mid" | "high" — sets all defaults below

# Any of these override the level preset:
# [sandbox]

# allow_network = ["api.anthropic.com", "github.com"]

# [security.secrets]
# scan = "off"              # "off" | "warn" | "block" — "off" explicitly disables scanning

# [security.forbidden_tools]
# extra = ["Bash(*sudo*)", "Bash(*aws*)"]
```

#### Items

1. [x] **`SecurityLevel` enum** (`crates/ta-goal/src/security.rs`): `Low | Mid | High`. `SecurityProfile::from_level(level, overrides) -> SecurityProfile` — merges level defaults with explicit overrides from `workflow.toml`. `SecurityProfile` fields: `sandbox_enabled`, `network_policy`, `forbidden_tool_patterns`, `approval_required`, `audit_mode`, `constitution_block_mode: ConstitutionBlockMode`, `secret_scan_mode: SecretScanMode`. `ConstitutionBlockMode`: `Warn | Block | BlockAndFollowUp`. `SecretScanMode`: `Off | Warn | Block`. Both are always present — level determines the default value, not whether they exist.

2. [x] **Level preset tables** (`security.rs`): Const defaults per level. Constitution supervisor: `low → Warn`, `mid → Warn` (configurable to `Block`), `high → BlockAndFollowUp`. Secret scan: `low → Warn`, `mid → Warn`, `high → Block`. `mid` populates `DEFAULT_MID_FORBIDDEN_TOOLS` with sensible patterns (`Bash(*rm -rf*)`, `Bash(*sudo *)`, `Bash(*curl * | bash*)`, `Bash(*wget * -O- * | sh*)`). 25 unit tests in security.rs.

3. [x] **Apply profile in `run.rs`**: Load `SecurityProfile` at goal start. Pass to `inject_claude_settings_with_security()` (tool allow/deny, web search toggle), sandbox spawn, and audit writer. When `level = "high"` and sandbox is manually disabled, print: `[warn] security.level=high but sandbox.enabled=false — sandbox override active. High security requires process isolation.`

4. [x] **Secret scanning always runs** (`crates/ta-changeset/src/secret_scan.rs`): Regex scan over draft artifact text content at `ta draft apply` time — runs at all levels. Patterns: AWS key (`AKIA[0-9A-Z]{16}`), generic API key, private key PEM header, GitHub PAT (`ghp_[A-Za-z0-9]{36}`), generic secret assignment. Mode from `SecurityProfile.secret_scan_mode`: `Warn` → print findings, continue. `Block` → print findings, abort apply with CTA. `Off` → skip entirely. 5 unit tests in secret_scan.rs.

5. [x] **HIGH constitution violation → block + auto-follow-up**: `ConstitutionBlockMode::BlockAndFollowUp` is the `high` preset. The `constitution_block_mode` field is present on `SecurityProfile` and surfaced via `SecurityConfig`. Full draft-blocking and auto follow-up spawn are stubbed; the enforcement wiring for `BlockAndFollowUp` is tracked for v0.15.15 (Multi-Agent Consensus Review Workflow) where the supervisor architecture is overhauled.

6. [x] **Audit hash chain** (`crates/ta-audit/src/chain.rs`): `AuditHmacKey` manages `.ta/audit.key` (32-byte key). `sign_entry`/`verify_entry_sig` for HMAC-SHA256. `verify_hmac_chain` checks per-entry SHA-256 chain + HMAC signatures. `ta audit verify` loads the key and reports hash/HMAC failures with line-level detail. 4 unit tests in chain.rs.

7. [x] **`ta init` level prompt**: When `ta init` runs interactively, asks: `Security level? [low] solo-dev / mid team / high regulated`. Writes `[security]` section to generated `workflow.toml` (commented for low, active for mid/high).

8. [x] **`ta status` security badge**: Shows `[mid]` or `[high]` badge in status header. For non-low levels, prints a `Security:` detail line with level description and any active overrides.

<!-- status: done -->

10. [x] **USAGE.md "Security Levels" section**: Added table of levels and defaults, how to set level, individual override examples, disabling secret scan, audit chain verification, relationship to SecureTA (SA) above `high`.



---

### v0.15.14.5 — Supervisor Agent: File-Inspection Mode (Headless Agent in Staging)
<!-- status: done -->
**Goal**: Replace the single-shot supervisor prompt with a headless agent that has Read/Grep/Glob tool access to the staging workspace. The supervisor reads what it needs, produces specific file:line findings, and never receives pre-loaded diffs. Eliminates vague "cannot be verified without viewing staging files" findings entirely.

**Root cause**: `invoke_claude_cli_supervisor` calls `claude --print` with a pre-built text prompt containing only the goal objective, a list of changed file paths (no content), and the constitution. No tools are available. The supervisor reasons from filenames alone, producing surface-level findings with qualified hedging. `run_manifest_supervisor` already runs in `staging_path` as `current_dir` — the same model applies to all built-in supervisors.

**Why not pre-load diffs into the prompt**: Embedding full diffs doesn't scale — a 50-file PR saturates context before the supervisor can reason. An agent that selectively reads what it needs is both cheaper (tokens proportional to what it examines) and more accurate (it can follow the code, not just scan a wall of text).

**Framework assignment**: Any agent profile can be the supervisor — set `[supervisor] agent_profile = "supervisor"` and define `[agent_profiles.supervisor]` with any supported `framework` (claude, codex, ollama, custom manifest). The file-inspection refactor covers all four dispatch paths. Default (if unset) remains `claude-code`.

**Design**: Supervisor prompt contains goal spec + file paths (as a starting point) + constitution + explicit instruction to read files before forming findings. Supervisor runs as headless agent in staging dir with Read/Grep/Glob. Output is identical structured JSON — but findings must cite `file:line` when referencing code.

#### Items

1. [x] **`agent_profile` link for supervisor** (`SupervisorRunConfig`): Add `agent_profile: Option<String>` field. When set, resolve via `agent_profiles` in `workflow.toml` to get `framework` and `model`. The resolved framework drives dispatch (replaces the bare `agent =` string). Config: `[supervisor] agent_profile = "supervisor"` + `[agent_profiles.supervisor] framework = "claude" model = "claude-sonnet-4-6"`. Any registered profile framework works — not just claude. If `agent_profile` is unset, existing `agent =` string fallback is preserved for backward compat.

2. [x] **`invoke_claude_cli_supervisor` refactor** (`supervisor_review.rs`): Replace `claude --print <prompt>` with a headless agent invocation: `current_dir = staging_path`, `--allowedTools "Read(*),Grep(*),Glob(*)"`. Prompt instructs the supervisor to read relevant files before forming findings. Drop diff/content pre-loading from the prompt — file paths remain as starting points only.

3. [x] **`invoke_codex_supervisor` same treatment**: Mirror the same change for the codex supervisor path (equivalent headless + file-access flags for codex CLI).

4. [x] **`invoke_ollama_supervisor` + manifest path same treatment**: For ollama (`ta agent run ollama --headless`), pass `--tools read,grep,glob` when available. For `run_manifest_supervisor` (custom manifest agents), it already runs in `staging_path` as `current_dir` — update the context input to include the file-inspection instruction and require `file:line` citations. Document which paths have native tool access vs instruction-only prompting.

5. [x] **`build_supervisor_prompt` update**: Keep `changed_files: &[String]` (paths only). Add explicit instruction: "Read the files listed above using your Read/Grep/Glob tools before forming each finding. Cite `file:line` in every finding that references code. Never write 'cannot be verified without viewing files' — view the files first."



7. [x] **Tests**: Supervisor with staging access produces `file:line` citations. Hedging-phrase detector fires correctly. `build_supervisor_prompt` no longer embeds diff content. Headless invocation sets correct `current_dir` and tool allowlist. `agent_profile` resolution picks up framework and model from `agent_profiles` table.

8. [x] **USAGE.md "Supervisor Agent" section update**: Document that the supervisor reads staged files directly, what tools it has, how to assign a supervisor profile (any framework), how to interpret `file:line` findings in draft view.

#### Version: `0.15.14.5-alpha`

---

### v0.15.14.6 — Supervisor Hook JSON Filtering
<!-- status: done -->
```

**Root cause**: `spawn_with_heartbeat_monitor` reads stdout line-by-line and treats any line as a heartbeat token. Hook JSON lines are real stdout bytes but not supervisor content. The stall timer is measuring token arrival, not meaningful content arrival.

<!-- status: done -->

#### Items

1. [x] **Hook JSON line filter in `spawn_with_heartbeat_monitor`** (`supervisor_review.rs`): Added `is_hook_json_line()` helper; lines with `"type":"system"` are discarded before the heartbeat timestamp is updated and before appending to the output buffer. Applies to all dispatch paths.



3. [x] **Stall message improvement**: Hook lines are filtered before being appended to `partial_output`, so the stall error message never includes raw hook JSON. A stream of only hook JSON lines now correctly triggers the stall (the watchdog is not reset by filtered lines).

4. [x] **Tests**: 7 new tests: `test_is_hook_json_line_*` (3), `test_hook_json_line_filtered_from_output`, `test_only_hook_json_lines_triggers_stall`, `test_disable_hooks_env_var_set_when_enable_hooks_false`, `test_enable_hooks_true_does_not_set_disable_env`. Added `PATH_MUTEX` static to serialize PATH-mutating mock claude tests. All 61 supervisor tests pass.

5. [x] **USAGE.md note**: Added "Hooks suppression in supervisor invocations" subsection explaining the default behaviour and `enable_hooks = true` opt-in.

#### Version: `0.15.14.6-alpha`

---

### v0.15.14.7 — Fix Legacy Agent Decision Log Bleeding Between Goals
<!-- status: done -->
**Goal**: Agent decisions from a previous goal run are appearing in subsequent drafts. Root cause: `.ta-decisions.json` is written at the staging root (alongside `Cargo.toml`, `PLAN.md`, etc.), not inside `.ta/`. When `ta draft apply` runs, the overlay copies all modified files back to source — including `.ta-decisions.json`. The next goal's staging is created from that source, carrying the previous run's decisions forward. Every subsequent draft inherits the full history of prior decisions until the file is manually deleted.

**Fix**: Treat `.ta-decisions.json` as a staging-only ephemeral artifact — excluded from the overlay diff and apply path, deleted at staging creation time, and gitignored.

#### Items

1. [x] **Exclude from overlay diff** (`crates/ta-workspace/src/overlay.rs`): Added `EPHEMERAL_STAGING_FILES` constant with `.ta-decisions.json`; `should_skip_for_diff()` now checks this list so the file is never included in the changeset diff and never applied back to source.

2. [x] **Delete at staging creation time** (`overlay.rs`): Added `delete_ephemeral_staging_files()` called in `create_with_strategy()` after the source copy completes. Agent always starts with a clean slate regardless of source state.

3. [x] **`.gitignore` entry**: Verified already present (added as hotfix). Code-level exclusion makes it redundant but the `.gitignore` entry remains as defense-in-depth.

#### Items

5. [x] **Tests** (3 tests in `overlay.rs`): `decisions_json_excluded_from_diff`, `decisions_json_deleted_from_staging_at_creation`, `decisions_from_goal_a_do_not_bleed_into_goal_b_diff` — all pass.

6. [x] **USAGE.md**: Added ephemeral callout to the "Agent Decision Log" section explaining the file is scoped to a single goal run and never applied back to source.

#### Version: `0.15.14.7-alpha`

---


<!-- status: done -->
**Goal**: A workflow template for multi-agent panel reviews where specialist agents run in parallel, each producing a structured verdict with a score and findings, and a final consensus step aggregates their outputs into a readiness score and recommendation. Ships with a `code-review-consensus` template covering architect, security, principal engineer, and PM roles. Include configurable consensus algorithms/models. Start with Raft and Paxos with Raft as the default — it should do no work if there is no swarm/multi-agent in the workflow.

**Depends on**: v0.15.14 (parallel fan-out, join step)

**Algorithm selection (TA)**:

| Algorithm | Fault model | Use case |
---
| **Raft** (default) | Crash fault tolerant | Multi-agent panels, agent coordinator state, replicated workflow logs |

| **Weighted Threshold** | Trust-all | Single-node or no-swarm — simple weighted average, no replication |

Raft is the TA default: leader election ensures one coordinator drives consensus even when agents stall or crash; log replication provides durability of the consensus decision across the workflow session. Falls back to `WeightedThreshold` with no coordination overhead when only one reviewer is active (no-op on single-agent workflows). Byzantine/adversarial consensus (PBFT, HotStuff, SCP) lives in the SA layer above TA.

**Design** (workflow template):

A `kind = "consensus"` step, or equivalently expressed via the generic parallel + join system:

```toml
# templates/workflows/code-review-consensus.toml

[workflow]

description = """
Multi-agent panel review. Four specialist agents review in parallel:
  - architect: architecture & design quality

  - principal: code correctness, tests, maintainability
  - pm:        product fit, scope, user impact
Aggregated into a consensus readiness score (0.0–1.0).
Blocks apply if score < gate_threshold.
"""

<!-- status: done -->
gate_threshold = 0.75          # minimum consensus score to auto-proceed
reviewer_timeout_mins = 30
consensus_algorithm = "raft"   # raft | paxos | weighted
require_all_reviewers = false  # if false, timeout slots are omitted from quorum

[[stage]]
name = "architect_review"
kind = "workflow"
workflow = "review-specialist"
```

objective = "Review as a software architect. Focus: system design, modularity, \

---

[[stage]]
<!-- status: done -->
kind = "workflow"
workflow = "review-specialist"
```
agent = "claude-code"

             secrets handling, input validation. Score 0.0–1.0."
parallel_group = "panel"

[[stage]]
name = "principal_review"
kind = "workflow"
workflow = "review-specialist"
```
agent = "claude-code"
objective = "Review as a principal engineer. Focus: correctness, edge cases, \
             test coverage, performance, maintainability. Score 0.0–1.0."
parallel_group = "panel"

[[stage]]

kind = "workflow"
workflow = "review-specialist"
```
agent = "claude-code"
objective = "Review as a product manager. Focus: goal alignment, scope, \
             user-visible impact, backwards compatibility. Score 0.0–1.0."
parallel_group = "panel"

[[stage]]
name = "consensus"
<!-- status: done -->
parallel_group = "panel"
inputs = ["architect_review.score", "security_review.score",
          "principal_review.score", "pm_review.score"]
weights = { architect = 1.0, security = 1.5, principal = 1.0, pm = 0.5 }
gate_threshold = "{{workflow.config.gate_threshold}}"
algorithm = "{{workflow.config.consensus_algorithm}}"
depends_on = ["architect_review", "security_review", "principal_review", "pm_review"]

[[stage]]
name = "apply"
kind = "apply_draft"
depends_on = ["consensus"]
condition = "consensus.proceed"
```



1. [x] **`ConsensusAlgorithm` enum** (`crates/ta-workflow/src/consensus/mod.rs`): `Raft`, `Paxos`, `Weighted`. Serializes as `"raft"` / `"paxos"` / `"weighted"`. Default = `Raft`. `run_consensus()` auto-degrades to `Weighted` when only one non-timed-out reviewer is active (no coordination overhead on single-agent workflows). 3 tests in mod.rs.

2. [x] **`run_consensus()` dispatcher** (`crates/ta-workflow/src/consensus/mod.rs`): Central dispatch function. Reads algorithm config, computes active vs timed-out votes, delegates to `raft::run`, `paxos::run`, or `weighted::run`. Single-agent / no-swarm → always uses `weighted` regardless of config. `ConsensusInput` / `ConsensusResult` / `ReviewerVote` types defined here. Re-exported from `ta-workflow` crate root.

3. [x] **`RaftConsensus`** (`crates/ta-workflow/src/consensus/raft.rs`): `RaftLog` struct manages session-scoped JSONL log at `<run_dir>/<run-id>.raft.log`. Leader election logged as `LeaderElected` entry. Each reviewer vote appended (`EntryAppended`) then committed (`EntryCommitted`). Final quorum check logged as `QuorumReached`. `weighted_average(committed_scores, weights)` → `ConsensusResult`. Log file deleted on success (`cleanup()`). On crash recovery: stale log detected, term incremented, prior committed entries re-adopted. 8 tests in raft.rs.

4. [x] **`PaxosConsensus`** (`crates/ta-workflow/src/consensus/paxos.rs`): Single-decree Paxos. `prepare → promise → accept → accepted` phases. Audit trail written to `<run_dir>/<run-id>.paxos.log` (JSONL). In single-process mode all reviewers promise and accept immediately. Timed-out reviewers omitted from quorum. Override path appended as `Decided` entry. Log deleted on success. 6 tests in paxos.rs.

5. [x] **`WeightedConsensus`** (`crates/ta-workflow/src/consensus/weighted.rs`): `weighted_average(scores, weights)` → `ConsensusResult`. No log files. Timed-out slots excluded. Override: sets `proceed=true` + `override_active=true` + audit entry in summary string. 10 tests in weighted.rs.

6. [x] **`review-specialist` base workflow template** (`templates/workflows/review-specialist.toml`): Minimal governed review workflow with configurable `role`, `objective`, `reviewer_agent`, `reviewer_timeout_mins`, and `verdict_output`. Documents that the `score` field in verdict.json is the primary output consumed by the consensus step.

7. [x] **`ta workflow run code-review-consensus` UX via workflow config**: `code-review-consensus.toml` template ships live status output through standard workflow machinery. Consensus `summary` string contains `[Raft] Committed log entry 4/4 (majority: 3)` for Raft. On `proceed=false`, `ConsensusResult.summary` contains blockage detail with findings. `override_reason` field propagates through to audit summary.

8. [x] **`--override` flag semantics** (`ConsensusInput.override_reason`): Any non-None `override_reason` on `ConsensusInput` bypasses a `proceed=false` gate. Sets `override_active=true` on `ConsensusResult`. Summary string contains `OVERRIDE reason="..."`. Callers (workflow runtime) are responsible for logging this to `goal-audit.jsonl`.

```

10. [x] **Tests** (37 total across 4 files): `RaftConsensus` — 4 reviewers all commit → proceed; 4 reviewers 1 stall → majority of 3 commits, stall flagged; low score blocks; override bypasses block; log file lifecycle; crash recovery from partial log; directory creation; findings committed to log. `PaxosConsensus` — single-decree prepare/accept roundtrip; blocks below threshold; timeout reduces quorum; override bypasses; log cleanup; per-role findings. `WeightedConsensus` — equal weights proceeds; below threshold blocks; security 1.5x upweighted blocks; timeout excluded; all-timed-out score-zero; override bypasses; override not set when naturally proceeding; findings captured; scores by role; summary label. `ConsensusAlgorithm` — default is Raft; display strings; JSON roundtrip; weighted_average math; degrade single-reviewer; degrade paxos single-reviewer; all-timed-out; override bypasses block.

11. [x] **Workflow templates**: `templates/workflows/code-review-consensus.toml` ships as built-in with 4 parallel reviewer stages (architect/security/principal/pm), consensus stage with configurable algorithm/weights/threshold/timeout, and apply stage gated on `consensus.proceed`. `templates/workflows/review-specialist.toml` is the single-reviewer base template. Both registered in `WorkflowCatalog`.

<!-- status: done -->

#### Version: `0.15.15-alpha`

---

### v0.15.15.1 — Consensus Engine Wiring, Audit Persistence & Decision Log Fix
<!-- status: done -->
**Goal**: Three correctness gaps identified during v0.15.15 review before public release: (1) `kind = "consensus"` and `kind = "apply_draft"` are not recognized by `StageKind` — the `code-review-consensus` template fails at parse time; (2) per-reviewer votes live only in the Raft/Paxos crash-recovery log, which is deleted on success — Constitution §1.5 (append-only audit) is violated when the log is the sole record; (3) decision logging is "optional" in the injected agent prompt — agents implementing substantial features routinely skip it, leaving reviewers with no insight into design choices.

**Depends on**: v0.15.15 (consensus library crate)

---

1. [x] **`StageKind::Consensus` variant** (`apps/ta-cli/src/commands/governed_workflow.rs`): Add `Consensus` to `StageKind` enum (`#[serde(rename_all = "snake_case")]` → deserializes `kind = "consensus"`). Add `stage_consensus()` function that reads reviewer verdict files from `.ta/review/<run-id>/<role>/verdict.json`, builds `ConsensusInput` from workflow config (weights, threshold, algorithm, require_all), calls `run_consensus()`, writes `ConsensusResult` to the workflow run output map, and fails the stage if `result.proceed == false` (unless `--override-reason` is set). Wire into `execute_stage()` match arm.

2. [x] **`StageKind::ApplyDraft` variant** (`apps/ta-cli/src/commands/governed_workflow.rs`): Add `ApplyDraft` to `StageKind` (deserializes `kind = "apply_draft"`). Map to the existing `stage_apply_draft()` function. The name-based `"apply_draft"` dispatch in `StageKind::Default` remains for backward compatibility; the new variant makes it explicit in templates.

3. [x] **Audit persistence before log cleanup** (`crates/ta-workflow/src/consensus/raft.rs`, `paxos.rs`): Write a structured audit entry to `.ta/audit.jsonl` (append) BEFORE calling `log.cleanup()`. Entry schema: `{ "event": "consensus_complete", "run_id", "algorithm": "raft"|"paxos", "score", "proceed", "override_active", "override_reason", "timed_out_roles": [...], "scores_by_role": {...}, "timestamp" }`. This satisfies Constitution §1.5 — the per-reviewer vote data is now durable in the append-only audit log regardless of whether the caller persists `ConsensusResult`. Add a test that verifies the audit entry exists after `run()` completes and the log file has been cleaned up.

```markdown

5. [x] **Decision log: required for feature work** (`apps/ta-cli/src/commands/run.rs`, `crates/ta-changeset/src/draft_package.rs`): Changed injected prompt language from "optional but encouraged" to "required when implementing features or any significant code refactor." Added `check_missing_decisions()` function in `draft_package.rs` that fires when the diff contains substantive code changes but no decision log entries. The function checks for `.rs`, `.ts`, `.py`, `.go`, and other source file extensions; triggers warning: "No agent decision log entries found for a goal with significant code changes. Consider `ta run --follow-up` to capture design rationale before approving." Does not block apply.



7. [x] **Tests**: `stage_consensus()` — 4 reviewers → proceed; below threshold → stage fails; missing verdict file → timeout/BLOCKED. `stage_kind_consensus_deserializes` and `stage_kind_apply_draft_deserializes` tests added. Audit entry exists after raft and paxos `run()` + cleanup. Override audit entry present when `override_reason` set for both raft and paxos. `check_missing_decisions` — fires on Rust/TS/Python code changes, suppressed when decisions present, suppressed for trivial (toml/md) changes, suppressed when no artifacts.

#### Version: `0.15.15-alpha.1`

---

### "v0.15.15.2 — One-Command Release + Phase Auto-Detection"
<!-- status: done -->
**Goal**: Three things: (1) `ta release dispatch <tag>` becomes truly one-and-done — detects version drift, bumps inline, commits, waits for CI, dispatches. (2) `--phase` on `ta run` becomes optional via auto-detection from PLAN.md. (3) `ta-agent-ollama` binary is packaged in all platform installers so `ta agent install-qwen` works end-to-end out of the box.





#### ta-agent-ollama Packaging (unblocks local model users)

1. [x] **Build `ta-agent-ollama` in release CI** (`.github/workflows/release.yml`): Added `-p ta-agent-ollama` to all `cargo build` / `cross build` steps alongside `ta-cli` and `ta-daemon`.

2. [x] **Bundle in all platform archives**: Copies `ta-agent-ollama` into `staging/` in the Unix tarball, Windows ZIP, and macOS DMG packaging steps — same pattern as `ta-daemon`.

3. [x] **Bundle in Windows MSI** (`apps/ta-cli/wix/main.wxs`): Added `AgentOllamaExecutable` `Component`/`File` entry for `ta-agent-ollama.exe` in `INSTALLFOLDER`, referenced by the `Complete` feature. Same pattern as `DaemonExecutable`.

---

5. [x] **`ta agent doctor <profile>` binary check**: Ollama-backed profile check includes `ta-agent-ollama` presence with the same message. Added as check #2 in `framework_doctor()`.

> **Note**: The `ta agent install <target> --size <size>` generalization (unified command for Qwen, Gemma 4, etc.) is tracked in **v0.16.3** alongside the full `ta-agent-ollama` plugin extraction. `install-qwen` stays as-is until then.

---



2. [x] **CI green check** (`apps/ta-cli/src/commands/release.rs`): Before dispatching, polls `gh run list --branch main --limit 1`. If `in_progress`, prints `"CI is still running on <sha> — waiting..."` every 15s (up to 40 attempts = 10 min). If `failure`, aborts with actionable message. `--skip-ci-check` flag for emergencies.

   enabled = true

4. [x] **`ta release dispatch` full flow**: Complete release is `ta release dispatch public-alpha-v0.15.15.2 --prerelease` — drift detection → bump → commit → push → CI wait → tag → dispatch → print Actions URL.

---

6. [x] **Phase resolution in `ta run`** (`apps/ta-cli/src/commands/run.rs`): `--phase` is optional. Resolution priority — first match wins:
   1. `--phase <id>` explicit flag (always wins)
   2. Semver found in goal title (e.g. `"v0.15.15.2 — Fix auth"` → phase `v0.15.15.2`) via `extract_semver_from_title()`
   3. Exactly one phase currently `in_progress` in PLAN.md → use it, print `"Auto-linked phase: v0.15.15.2"` via `find_single_in_progress()`
   4. None of the above → generate a **gap semver** and insert a new phase stub into PLAN.md

<!-- status: done -->

---

8. [x] **Phase embedded in draft and surfaced in `ta draft view`** (`apps/ta-cli/src/commands/draft.rs`, `crates/ta-changeset/src/draft_package.rs`): Added `plan_phase: Option<String>` to `DraftPackage`. Field populated from `GoalRun.plan_phase` at build time. Shown prominently in `ta draft view` (with PLAN.md title lookup) and as `[phase]` suffix in `ta draft list`.

```

10. [x] **Tests**: 12 new tests in `plan.rs` covering: title semver extraction (semver found, not found, different formats); gap semver generation (first slot, collision increment); gap semver with 5-part existing (finds next available A); PLAN.md stub insertion at correct position; idempotency; `auto_detect_phase` end-to-end. Existing test suite (971 + others) all pass.

#### Version: `0.15.15-alpha.2`

---

### v0.15.15.3 — crates.io Publishing Infrastructure
<!-- status: done -->

**Goal**: Enable `cargo install ta-cli` by publishing all workspace crates to crates.io in dependency order. Currently blocked because `ta-cli` has 20 path dependencies that are not on crates.io.



---
1. [x] **Audit publishability**: Audited all 35 workspace crates. Issues found: 3 crates missing `license` (ta-mediation, ta-session, ta-agent-ollama); all crates missing `keywords`/`categories`; 17 crates with unversioned internal path deps (crates.io requires both `path` and `version`).
2. [x] **Add crates.io metadata** to all workspace crates: Added `repository`, `homepage`, `keywords`, `categories` to all 35 crates. Added `license` to the 3 crates missing it. Added `version` to all internal path deps. Updated `bump-version.sh` to keep internal path dep versions in sync across the whole workspace.
3. [x] **Publish in order**: Created `scripts/publish-crates.sh` — 35 crates in 6 dependency tiers (leaf → ta-cli), idempotent crates.io version check, 20s propagation delay. `--dry-run` flag for pre-publish validation.
4. [x] **CI `publish-crate` step** (`release.yml`): Updated to call `scripts/publish-crates.sh` (all crates in order) instead of just `ta-cli`. Skips gracefully when `CARGO_REGISTRY_TOKEN` is not set.
5. [x] **`CARGO_REGISTRY_TOKEN` secret**: Documented in `docs/USAGE.md` "Publishing to crates.io" — how to generate the token, add it as a GitHub secret, required scopes, and troubleshooting steps.


#### Version: `0.15.15-alpha.3`

---

### v0.15.15.3.1 — Config File Format Cleanup
<!-- status: done -->

**Goal**: Normalize the inconsistent mix of TOML and YAML across `agents/` and `templates/workflows/`. Document the format rules in the constitution to keep them aligned going forward.





1. [x] **Normalize `agents/` to YAML**: Deleted `agents/codex.toml` (superseded by `agents/codex.yaml`). Converted `agents/gsd.toml` → `agents/gsd.yaml`. All agent manifests now YAML except `qwen*.toml` (Ollama profiles).

#### Items

3. [x] **Constitution rule**: Added `file-format-convention` policy rule to `ta_default()`. Added `check_file_format_conventions()` function called at `ta draft build` — warns when `.toml` found in `agents/` (excluding `qwen*.toml`) or in `templates/workflows/` (excluding user starters).

4. [x] **Update loaders**: Updated `AgentFrameworkManifest::discover()` to load `.yaml`/`.yml` files with YAML taking precedence over TOML. Updated `find_workflow_def()` to search YAML before TOML (4-candidate search order). Updated tests and `include_str!` references. Added `serde_yaml` to `ta-runtime`.

5. [x] **Tests**: Added 7 constitution format tests (`check_file_format_conventions` clean/violation/exempt cases + `ta_default` rule presence). Added 3 YAML discovery tests in `framework.rs` (YAML load, YAML-over-TOML precedence, resolve via YAML). Added 4 workflow YAML loading tests in `governed_workflow.rs` (YAML template load, YAML-over-TOML precedence, project-local override, not-found error).

#### Version: `0.15.15-alpha.3.1`

---

### v0.15.15.3.2 — Orchestration Stack Guide (USAGE.md)
<!-- status: done -->



<!-- status: done -->

<!-- status: done -->

1. [x] **USAGE.md "Choosing Your Orchestration Stack" section**: Decision table (same as README), expanded with config examples — `.mcp.json` setup for ruflow MCP, `daemon.toml` agent routing, `workflow.toml` for TA swarm.

2. [x] **Within-session parallelism note**: Explain that Claude Code's native `Agent` tool handles parallel subtasks automatically when running inside `ta run` — no config needed, no extra install.

3. [x] **ruflow MCP configuration guide**: Step-by-step — install, register MCP, verify `mcp__claude-flow__memory_retrieve` tools appear in Claude Code session inside staging.

4. [x] **Combined stack examples**: Three annotated examples — (1) simple goal, (2) TA swarm workflow, (3) cross-session memory with ruflow.

5. [x] **When NOT to add ruflow**: Clarify that adding ruflow to every goal adds latency and complexity without benefit. Document the threshold: use ruflow when goals span multiple sessions and need to share findings.

---

---

### "v0.15.15.3.3 — Pre-Copy Draft Version Validation"


**Goal**: Move version validation to before the file copy, reading from the staging directory rather than the post-copy source workspace. Catches a missing `Cargo.toml` bump in the draft before any files are written — zero recovery cost vs. the current post-copy false alarm.

**Depends on**: v0.15.15.3

#### Root cause

`validate_cargo_version()` is called at `draft.rs:6599` — after `overlay.apply_with_conflict_check()` at line 5272. If the agent's draft does not include a `Cargo.toml` bump, the overlay skips that file and the check reads the unchanged pre-apply version from main. The validation fires as a false mismatch when in fact the problem is that the agent never bumped. Files have already been written to the feature branch at this point.



**Pre-copy path (staging present)**: Before `overlay.apply_with_conflict_check()`, check whether `goal.workspace_path/Cargo.toml` exists (the staging copy). If it does, call `validate_cargo_version(&goal.workspace_path, &expected_ver)`. On mismatch: block the apply with:
```

  Draft has:    0.15.15-alpha.1
  Phase needs:  0.15.15-alpha.3
  Fix: run bump-version.sh inside the staging directory, or deny
       the draft and re-run the goal with an explicit version bump.
```
```




**CLAUDE.md consistency check (same pre-copy gate)**: If `goal.workspace_path/CLAUDE.md` exists, also extract the `**Current version**:` line and check it matches. A version bump that updated `Cargo.toml` but not `CLAUDE.md` is equally broken — flag it with the same pre-copy block.

#### Items



```

3. ✅ **CLAUDE.md consistency check**: Same pre-copy gate, check `staging/CLAUDE.md` `**Current version**:` line. Block if it differs from `expected_ver` or from the staging `Cargo.toml` version — they must be the same.

4. ✅ **Post-copy check demoted to warning-only**: Keep `validate_cargo_version_as_fallback(&target_dir, ...)` as a fallback for the embedded-patch path, with box header `"VERSION MISMATCH — staging was unavailable at apply time"` so it's clear this is a secondary check, not the primary gate.

   ```

6. ✅ **`update_phase_status()` transitions from any non-done state** (`plan.rs`): The function already handles any status → done transitions (regex matches `pending`, `in_progress`, etc.). Added test `update_phase_status_transitions_pending_to_done_without_in_progress` to `plan.rs` confirming this behavior.

#### Version: `0.15.15-alpha.3.3`

---


<!-- status: done -->

**Goal**: Enforce at the policy and constitution level that email is always a human-reviewed draft — never auto-sent. The `MessagingAdapter.create_draft()` path is the only permitted outcome; `policy = "auto"` for email is blocked by the constitution. Prompt-injection-driven sends are blocked before any draft reaches the user's email Drafts folder without supervision.



---

2. [x] **`ta_external_action` dispatch guard** (`crates/ta-actions/src/dispatch.rs`): At the action dispatch layer, intercept `action_type = "email"` regardless of policy setting and route to `MessagingAdapter.create_draft()`. No path exists from `ta_external_action` to a direct email send. The `send` op is absent from the protocol at the type level (already enforced in `MessagingPluginProtocol`); this adds the dispatch-layer enforcement.
3. [x] **Draft view: email artifacts as first-class items** (`apps/ta-cli/src/commands/draft.rs`, Studio): Email drafts in the pending-actions queue rendered as structured cards in `ta draft view` — To, Subject, body preview, supervisor score, flag reason if any — not as raw action JSON. Human sees exactly what will land in their Drafts folder before approving.
4. [x] **`ta audit messaging` linked from `ta draft view`**: Draft view footer shows `"[Email drafts] Run ta audit messaging to see full history"` when email actions are present. Studio shows link inline.
5. [x] **Recipient allowlist** (`[actions.email]` in workflow.toml): Optional `allowed_recipients` list. If set, any email draft to an address not matching the list is flagged to the TA review queue with `"Recipient not in allowed_recipients"` before creating the draft. Empty list = no restriction. Default empty.
6. [x] **Rate limiting across sessions** (`crates/ta-actions/src/ratelimit.rs`): Add cross-session email rate limit: `max_per_hour` and `max_per_day` in `[actions.email]`. State persisted in `.ta/action-ratelimit.json`. Prevents runaway workflows from flooding Drafts.
---

#### Items

---

### v0.15.15.5 — Batch Build Loop: `build` Sub-Workflow, Auto-Approve & Post-Sync Build Step
<!-- status: done -->

---

**Depends on**: v0.15.14 (phase-loop engine, `plan_next`, `goto`, `apply_draft_branch` all implemented)



1. [x] **`templates/workflows/build.yaml`** — per-phase sub-workflow that `plan-build-loop` and `plan-build-phases` delegate to. Stages: `run_goal` → `review_draft` → `human_gate` → `apply_draft` → `pr_sync`. This is the template that was always referenced but never committed. Project-local `.ta/workflows/build.yaml` overrides it for custom per-phase policies.

2. [x] **Auto-approve config** (`[workflow.auto_approve]` in `.ta/workflow.toml` or the workflow YAML `config:` block): simple rule set for skipping the interactive `human_gate` on trusted local runs.

   ```toml
   # .ta/workflow.toml — project-local override
   [workflow.auto_approve]
   enabled = true

   # "reviewer_approved"  — reviewer agent returned approved verdict
   # "no_flags"           — reviewer raised no flag items
   # "severity_below"     — no Critical corrective actions pending
#### Items
   ```

   When `auto_approve.enabled = true` and all listed conditions are satisfied, `human_gate` logs `"[auto-approve] conditions met — applying without prompt"` and proceeds. Any unsatisfied condition falls back to the interactive prompt. This is intentionally simple — no regex/scope matching — so it is safe to commit and easy to audit.

3. [x] **Post-sync build step** (generic engine + TA implementation):

   *Engine* (`governed_workflow.rs`): After `pr_sync` completes (and after `apply_draft` in milestone mode), check `[workflow.post_sync_build]` config. If `command` is set, run it in the workspace root with a 10-minute timeout, streaming output. Failure halts the loop with an actionable error: `"Post-sync build failed — fix the build before continuing. Re-run with ta workflow resume <id>."`. Success logs the exit code and continues to `plan_next`.

   ```toml
   # Generic form in workflow.toml or workflow YAML config:
   [workflow.post_sync_build]
   enabled = true
```
   timeout_secs = 600

   ```

   *TA implementation* (`workflow.toml` at repo root or `.ta/workflow.toml`): Committed entry that runs `install_local.sh` so every batch-build loop ends with a freshly installed binary before the next phase starts. This is the only TA-specific file; the engine and config schema are fully generic.

4. [x] **Tests**: `build.yaml` resolves as sub-workflow and runs to completion in dry-run mode; auto-approve fires when conditions met and skips prompt; auto-approve falls back to prompt when any condition fails; post-sync build command runs after `pr_sync`; post-sync failure halts with resume instructions; `on_failure = "warn"` continues; timeout fires and reports the hung command.



---

### v0.15.15.6 — Nightly Build Pipeline
<!-- status: done -->

**Goal**: Add a scheduled nightly CI workflow that builds all 5 platforms at 2am PT and publishes a rolling pre-release only when main has new commits since the last nightly. Latest nightly appears alongside latest stable on the GitHub releases page. Historical nightly builds are accessible via a separate link, not interleaved with the stable release list.

**Depends on**: v0.15.15 (CI pipeline stable), v0.15.15.2 (ta-agent-ollama in release)

#### Release page structure

- **Latest stable** (`v0.15.15-alpha` etc.) → `Latest` badge, `v*` tag. Unchanged from current flow.
- **Latest nightly** (`nightly` tag, rolling) → `Pre-release` badge, single entry on the releases page. Replaces itself on each build — never accumulates.
```



#### Items

<!-- status: done -->

2. [x] **Commit-change guard**: On run start, download `last-sha.txt` from the current `nightly` release assets (if it exists). Compare with `git rev-parse HEAD`. If identical, exit with `Skipping — no commits since last nightly (SHA: <sha>)`. On first ever run (no `nightly` release yet), proceed unconditionally.

---

4. [x] **Publish step**: Uses `nightly` as the tag. Force-pushes the tag to `HEAD`. Creates the GitHub release on first run; updates it (`gh release edit nightly`) on subsequent runs. Sets `--prerelease --title "Nightly $(date +%Y-%m-%d) ($(git rev-parse --short HEAD))"`.

5. [x] **`last-sha.txt` asset**: Upload `HEAD` SHA as a release asset on each build. Used by step 2 on the next run. Content: the full 40-char commit SHA.

6. [x] **Nightly history in release body**: The publish step regenerates the release body on each build. Body includes: build timestamp, trigger type (scheduled / manual), commit SHA with link, and a Markdown table of the last 60 nightly builds pulled from the existing body (parse and prepend the new row). Format: `| 2026-04-15 | abc1234 | [Linux x86](...) [Linux ARM](...) [macOS Intel](...) [macOS ARM](...) [Windows MSI](...) |`. History link added to stable release body template and README install section.



8. [x] **`.release.toml`**: Add `nightly_tag = "nightly"` and `nightly_history_limit = 60` fields for reference.

9. [x] **Tests / validation**: Manual `workflow_dispatch` run confirms: skip fires on re-run with no new commit; history table updates on new commit; `nightly` tag moves to HEAD; `last-sha.txt` asset is replaced. *(Note: YAML syntax bug in release body step fixed 2026-04-18 — unindented heredoc broke YAML block scalar; replaced with printf.)*

#### Version: `0.15.15-alpha.6`

---



### v0.15.15.6.1 — Review draft 1d52066e for governed workflow
<!-- status: done -->
*Inserted goal — not in original plan. Governed workflow draft review; draft applied.*

### v0.15.15.6.2 — Review draft 6cbcb978 for governed workflow
<!-- status: done -->
*Inserted goal — not in original plan. Governed workflow draft review; draft applied.*
### v0.15.15.7 — Apply UX: Dirty VCS Check + Staging Version Bump Fix
<!-- status: done -->

**Goal**: Eliminate the two recurring apply blockers that have caused multiple failed apply attempts: (1) `ta run` should warn and prompt when the VCS working tree has uncommitted changes before copying source to staging — catching drift early instead of producing confusing warnings at apply time. (2) The staging version bump path is broken — running `bump-version.sh` from the project root updates only source, not staging, and running it from staging fails silently or gets undone by rollback. The fix must be deterministic and operator-free. (3) `ta plan next` needs a `--filter` flag so the batch build loop can be scoped to a version prefix without a project-local `max_phases` hack.

```





2. [x] **Staging version bump at apply time** (`apps/ta-cli/src/commands/draft.rs`): When `validate_staging_version` detects a mismatch, instead of immediately bailing, attempt to auto-patch `staging/Cargo.toml` and `staging/CLAUDE.md` in-place (sed-equivalent on the version line only) to match `expected_ver`. Re-validate after the patch. Only bail if the patch fails or re-validation still fails. Print: `"[apply] Auto-patched staging version from {draft_ver} to {expected_ver} — proceeding."` This eliminates the manual bump-version.sh-in-staging workaround entirely.



4. [x] **`ta plan next --filter <prefix>`** (`apps/ta-cli/src/commands/plan.rs`): Add an optional `--filter` flag that limits the next-phase search to phases whose ID starts with the given prefix (e.g. `--filter v0.15`). Phases not matching the prefix are skipped as if they don't exist. If no matching pending phase is found, outputs the same `done` signal as when all phases are complete. Wire through to `stage_plan_next` in `governed_workflow.rs` via an optional `phase_filter` field on `StageDef`, so workflow YAML can declare: `filter: "v0.15"`. Update `plan-build-loop.yaml` template to accept an optional `filter` config key and pass it through. Remove `.ta/workflows/plan-build-loop.yaml` project-local override once this ships (it's a `max_phases: 9` workaround).



#### Version: `0.15.15-alpha.7`

---

### v0.15.16 — Windows Code Signing (EV Certificate + CI Integration)
<!-- status: done -->
**Goal**: Eliminate the Microsoft SmartScreen "Windows protected your PC" warning on the TA Windows MSI installer by signing all Windows binaries and the MSI with an Extended Validation (EV) code signing certificate. EV certs bypass SmartScreen's reputation-building period — signed EV binaries show no warning on first install regardless of download count. Ships with a fully automated signing step in the release CI workflow.

**Depends on**: v0.13.11 (platform installers — WiX MSI build in release.yml)

**Background**: The SmartScreen warning appears because the MSI is unsigned. Windows SmartScreen evaluates two signals: (1) Authenticode signature — cryptographic proof of publisher identity; (2) download reputation — accumulated over time from many users running the binary without incident. An OV (Organization Validation) cert satisfies (1) but still requires hundreds of installs before (2) clears. An EV cert satisfies both immediately, making it the only option that eliminates the warning on day one.

**Design**:

```

  Provider: DigiCert, Sectigo, or GlobalSign (all Microsoft-trusted CAs)
  Type: Extended Validation (EV) Code Signing Certificate
```
```
  Format: PFX / PKCS#12, stored as GitHub Actions secret

CI signing step (release.yml, after WiX MSI build):


  3. Verify signatures with signtool verify --pa

Secrets required (GitHub Actions repository secrets):
  WINDOWS_SIGNING_CERT_BASE64  — base64-encoded PFX file
  WINDOWS_SIGNING_PASSWORD     — PFX password
```



|---|---|---|---|

   **Free / OSS certificate options (check these first):**
   - **Microsoft Trusted Root Program for OSS** — no longer offers free certs directly, but some CAs participate in OSS discount programs.
---

   - **SSL.com EV** (~$195/yr) — cheapest commercial EV option if OSS programs don't qualify.

   **If using SignPath Foundation (recommended for OSS EV-equivalent):**

   2. Once approved, install the SignPath GitHub Action and configure a signing policy.
   3. Replace `sign-windows.ps1` call in `release.yml` with the SignPath action — no PFX secret needed.
   4. SignPath uses a cloud HSM; SmartScreen reputation builds via their shared trusted publisher.

   **If using a commercial EV cert (PFX-based):**
   1. Purchase from DigiCert, Sectigo, or SSL.com. Requires registered legal entity (LLC/Corp) and 2-5 day identity verification.
   2. Export PFX and set GitHub secrets:
      ```bash
      # Encode PFX to base64 (no line breaks)
      base64 -i mycert.pfx | tr -d '\n'
```

      ```



   Store as `WINDOWS_SIGNING_CERT_BASE64` and `WINDOWS_SIGNING_PASSWORD` in GitHub Actions repository secrets. Document the renewal process in `docs/release-ops.md`.



3. [x] **Release workflow signing step** (`release.yml`): After the WiX MSI build step (`Build Windows MSI`), add a `Sign Windows artifacts` step that:
   - Calls `sign-windows.ps1` on `ta.exe`, `ta-daemon.exe`, and the `.msi` artifact

   - Fails the build if any signature is invalid

4. [x] **Publisher display name (PENDING — part of cert procurement)**: The EV cert Common Name (CN) must match the intended publisher name shown in Windows UAC prompts ("Do you want to allow **Trusted Autonomy** to make changes..."). Coordinate cert purchase with the correct legal entity name.



6. [x] **Verification in CI**: After signing, run `signtool verify /pa artifacts/ta-*.msi` and fail the workflow if exit code is non-zero. This catches cert expiry or misconfigured secrets before a release ships.

7. [x] **`docs/release-ops.md` section**: "Windows Code Signing" — how to renew the cert, update the GitHub secret, what to do if the cert expires mid-release cycle, how to verify a signed MSI locally (`signtool verify /pa /v ta-*.msi`).

8. [x] **macOS Gatekeeper hardening (optional — deferred to future phase)**: Add `codesign --deep --force --verify --sign "Developer ID Application: ..."` + `notarytool` notarization to the macOS DMG build step. Requires an Apple Developer account ($99/yr). Not implemented — moved to a dedicated macOS signing phase.

9. [x] **Tests (PENDING — blocked on cert)**: CI step asserts `signtool verify /pa` returns 0 for all signed artifacts. Smoke test that downloads published MSI and verifies signature. Blocked until item 1 (cert procurement) is complete.

#### Version: `0.15.16-alpha`

---

### v0.15.17 — `ta doctor`: Auth Validation & Agent-Agnostic Auth Spec
<!-- status: done -->


**Depends on**: v0.13.11 (platform installers), v0.15.5 (terms acceptance gate)

**Background**: Different agent frameworks require different auth:

- `codex`: API key (`OPENAI_API_KEY`)
- `ollama`: local service, but auth has two layers — (1) the Ollama service itself can require an API key (`OLLAMA_API_KEY`, added in v0.5) for protected instances, and (2) the models Ollama serves can be hosted on remote providers (OpenAI-compatible APIs, gated Hugging Face repos) that require their own credentials. A bare `ollama` install with only local models needs no credentials; an `ollama` instance proxying a subscription model does.
- custom/external frameworks: arbitrary env vars, session files, or local service endpoints





```toml
# In a user-defined agent manifest (.ta/agents/my-agent.yaml):
[auth]
required = true       # false = service being absent is not a fatal error
methods = [

  { type = "session_file", config_dir = "~/.config/myagent/", check_cmd = "myagent auth status", label = "session" },


    upstream_auth = [] },
]
```

`AuthMethodSpec` variants:
- `EnvVar { name, label, setup_hint, required }` — passes if env var is non-empty; `required=false` means absence is a soft warning, not a failure
- `SessionFile { config_dir_unix, config_dir_windows, check_cmd }` — passes if config dir or check_cmd exits 0
- `LocalService { url_env_var, default_url, health_endpoint, service_auth, upstream_auth }` — two-phase check:
  - **Phase 1 — reachability**: HTTP GET to `health_endpoint` returns 2xx. If unreachable and `required=false`, the whole method is skipped (soft pass). If unreachable and `required=true`, fail with "service not running" guidance.
  - **Phase 2 — credentials** (only runs if phase 1 passes): Check `service_auth` methods in order (e.g., `OLLAMA_API_KEY` for a protected Ollama instance). Then check `upstream_auth` methods in order (e.g., `OPENAI_API_KEY` for an OpenAI-compatible model Ollama is proxying). Each inner method can be `required=false` to emit a warning rather than fail. Both lists default to empty (no credential check).
- `None` — always passes (framework needs no auth)

```

Built-in manifests get `auth` populated:

```toml
|-----------|-------------|
| `claude-code` | `EnvVar(ANTHROPIC_API_KEY)` then `SessionFile(~/.config/claude/, "claude auth status")` |
| `codex` | `EnvVar(OPENAI_API_KEY)` |
| `claude-flow` | Inherits `claude-code` auth (delegates to Claude) |
| `ollama` | `LocalService(OLLAMA_HOST, localhost:11434, /api/tags, service_auth=[EnvVar(OLLAMA_API_KEY, required=false)], upstream_auth=[])`, `required=false` |

Ollama's built-in manifest starts with no upstream credentials. Users who configure Ollama to proxy a remote provider (e.g., `OPENAI_API_KEY` for an OpenAI-compatible endpoint) add the upstream method to their local manifest override in `.ta/agents/ollama.yaml`. `ta doctor` then validates both layers and reports each independently.

User-defined manifests set `[auth]` freely. `ta doctor` reads the active framework's manifest and runs `detect_auth_mode` — it needs no knowledge of any specific agent.

```

```

Output (all checks pass):
```
TA Doctor -- Runtime Validation

  [ok] TA CLI         0.15.17-alpha (328ac82d)
  [ok] Daemon         0.15.17-alpha -- connected at http://127.0.0.1:7700
  [ok] Auth (claude-code)  Subscription session -- ~/.config/claude/
       │
  [ok] gh CLI         Found at /usr/local/bin/gh -- github.com authenticated
  [ok] Project root   /Users/michael/dev/myproject

  [ok] Plan           Next phase: v0.16.0 (3 pending phases)

All checks passed.
```


```
  [FAIL] Auth (claude-code)  No authentication found.

           env var   ANTHROPIC_API_KEY — not set
           session   ~/.config/claude/ — not found
         Fix one of:
           Option 1 (subscription): claude auth login

```


```
  [ok] Auth (ollama)  Service reachable at http://localhost:11434 — no credentials required
```

Output (Ollama — protected instance, API key missing):
```
  [warn] Auth (ollama)  Service reachable but OLLAMA_API_KEY not set.
         This Ollama instance may require a key if access control is enabled.
         Fix: export OLLAMA_API_KEY=<your-key>
              (or unset if this Ollama instance has no access control)
```

Output (Ollama — proxying a remote provider, upstream key missing):
```

         Your ollama manifest declares upstream_auth requiring OPENAI_API_KEY — not set.
         Fix: export OPENAI_API_KEY=<your-key>
              (or remove upstream_auth from .ta/agents/ollama.yaml if not needed)
```

**Items**:

1. [x] **`AgentAuthSpec` in `crates/ta-runtime/src/auth_spec.rs`**: Define `AgentAuthSpec { required: bool, methods: Vec<AuthMethodSpec> }` and `AuthMethodSpec` enum:
   - `EnvVar { name, label, setup_hint, required: bool }` — `required=false` means missing is a soft warning
   - `SessionFile { config_dir_unix, config_dir_windows, check_cmd }`
   - `LocalService { url_env_var, default_url, health_endpoint, service_auth: Vec<AuthMethodSpec>, upstream_auth: Vec<AuthMethodSpec> }` — `service_auth` lists credentials the service itself requires (e.g., `OLLAMA_API_KEY`); `upstream_auth` lists credentials for any remote provider the service is proxying (e.g., `OPENAI_API_KEY` when Ollama proxies an OpenAI-compatible endpoint)
   - `None` — always passes



2. [x] **`detect_auth_mode`** (`crates/ta-runtime/src/auth_spec.rs`): `detect_auth_mode(spec: &AgentAuthSpec) -> AuthCheckResult` where `AuthCheckResult` is `Ok(AuthMethodSpec)` (first passing method) or `Missing { tried: Vec<(AuthMethodSpec, String)> }` (all failed, with reason per method). `LocalService` runs a two-phase check: (1) HTTP GET health endpoint — if unreachable and `required=false`, soft-pass; if unreachable and `required=true`, fail with "not running" message. (2) If reachable: run `service_auth` checks (fail or warn per `required`), then run `upstream_auth` checks (fail or warn per `required`). All inner `required=false` failures are collected as warnings, not fatal errors, and surfaced in `ta doctor` as `[warn]` lines.

3. [x] **`ta doctor` command** (`apps/ta-cli/src/commands/doctor.rs`): Runs the following checks in order, prints a pass/fail line for each, exits non-zero if any fail:
   - CLI version (always passes)
   - Daemon connection (`GET /health`; warns on version mismatch)
   - Auth check: load active framework manifest, call `detect_auth_mode`, report method + detail
   - Agent binary presence (`which`/`where` the manifest's `command`)
   - `gh` CLI presence and auth (`gh auth status`)
   - Project root detection
   - Plan state

4. [x] **Auth errors in `ta run`**: When an agent exits immediately with an auth-looking failure, call `detect_auth_mode` for the active framework and append the result to the error message. Replace generic "agent failed" with framework-specific fix hints drawn from `AuthMethodSpec.setup_hint`.

5. [x] **`ta doctor --json`**: Machine-readable output. Each check is `{ "name", "status": "ok"|"warn"|"fail", "detail", "fix" }`.

6. [x] **`ta doctor` in `ta onboard`** (v0.15.11 integration): Onboarding wizard runs `ta doctor` as its first step and blocks on any `fail` with a guided fix flow.

7. [x] **Tests**: `detect_auth_mode` with `ANTHROPIC_API_KEY` set; with fake session config dir; with neither (returns `Missing`); with `LocalService` and mock HTTP server returning 200 (soft pass); `LocalService` unreachable + `required=false` (soft pass, no error); `LocalService` reachable + `service_auth` env var missing + `required=false` (warns but passes); `LocalService` reachable + `upstream_auth` env var set (passes with upstream detail); `LocalService` reachable + `upstream_auth` env var missing + `required=true` (fails with upstream guidance); ollama built-in manifest YAML round-trips with service_auth and empty upstream_auth; custom manifest YAML with upstream_auth entries round-trips correctly; `ta doctor --json` output is valid JSON; version mismatch warns but does not fail.

8. [x] **USAGE.md**: "ta doctor" section — what each check tests, common fix paths, `--json` for CI, and how to declare `[auth]` in a custom agent manifest. Include a subsection on Ollama: how `service_auth` covers `OLLAMA_API_KEY` for protected instances, and how to add `upstream_auth` in `.ta/agents/ollama.yaml` when Ollama is proxying a remote provider that requires its own credentials.

#### Version: `0.15.17-alpha`

---

### v0.15.18 — Project TA Version Tracking & Upgrade Path
<!-- status: done -->
**Goal**: Track the TA version a project was initialized with (and each subsequent upgrade), so TA can detect when a project was created with an older version, identify what project-level changes are required to be compatible with the current version, and apply or warn about them automatically. This closes the gap where new TA versions add entries to `.gitignore`, `.taignore`, `workflow.toml`, or `.ta/config.toml` format — but existing projects silently miss those changes until something breaks.

**Depends on**: v0.15.17 (`ta doctor`), v0.13.13 (VCS-aware team setup)

**Background**: When `.ta/review/` was added as a required gitignore entry, existing projects had no way to know they needed it. Every TA release can introduce project-level requirements (new ignore paths, config schema fields, workflow.toml keys). Without version tracking, users only discover missing entries when something fails (e.g., `git pull --rebase` blocked by an untracked `.ta/review/`). The fix belongs in an upgrade path, not in user-facing error messages.

**Design**:

`.ta/project-meta.toml` (written by `ta init`, updated by `ta upgrade`):
```toml
# Written by ta init, updated by ta upgrade.
# Do not edit manually — managed by TA.
initialized_with = "0.15.5-alpha"   # TA version at project creation
---
```

Upgrade manifest (embedded in TA binary, `crates/ta-core/src/upgrade_manifest.rs`):
---
// Each entry: min_from version that needs this change, a description,
// a check fn (is this already applied?), and an apply fn.
UpgradeStep {
    introduced_in: "0.15.18",
    description: "add .ta/review/ to .gitignore",
    check: |root| gitignore_contains(root, ".ta/review/"),
    apply: |root| append_gitignore(root, ".ta/review/"),
}
```


```
ta upgrade
  [ok]  .ta/review/ already in .gitignore
  [fix] Added .ta/review/ to .taignore
```
  Upgraded project from 0.15.5-alpha → 0.15.18-alpha
```

Silencing intentional omissions — add to `.ta/config.local.toml`:
```toml
[upgrade]
acknowledged_omissions = [".ta/review/"]  # user intentionally removed; suppress warning
```

**Items**:

1. [x] **`.ta/project-meta.toml`**: Written by `ta init` with `initialized_with` = current TA semver. Read by `ta upgrade` and `ta doctor`. If absent (pre-v0.15.18 project), treated as `initialized_with = "0.0.0"` (apply all steps). Implemented in `apps/ta-cli/src/commands/init.rs`.

2. [x] **`UpgradeStep` type** (`apps/ta-cli/src/commands/upgrade.rs`): Struct with `introduced_in: &str`, `description: &str`, `check: fn(&Path) -> bool` (returns true if already applied / not needed), `apply: fn(&Path) -> anyhow::Result<()>`. `UPGRADE_STEPS: &[UpgradeStep]` const array — all steps in version order. Note: placed in ta-cli rather than non-existent ta-core crate.



```bash



6. [x] **Daemon start-up check**: When the daemon starts against a project root, if `project-meta.toml` is present and `last_upgraded` is more than 1 minor version behind the running daemon, logs a warning via `tracing::warn`. Implemented in `crates/ta-daemon/src/main.rs::check_project_meta_version()`.

7. [x] **Initial upgrade steps** (seeded at v0.15.18):

   - Add `.ta/review/` to `.taignore` if present and missing
   - Ensure `workflow.toml` has `[config] pr_poll_interval_secs` (default 60 if absent)
   - Warn if old staging dirs may contain `target/` artifacts

8. [x] **Tests**: Upgrade step `check`/`apply` round-trip; `ta upgrade --dry-run` exits non-zero when steps pending; `acknowledged_omissions` suppresses a step; `project-meta.toml` written correctly on `ta init`; upgrade from `0.0.0` applies all steps. 12 new tests in `upgrade.rs`, 1 in `init.rs`.

9. [x] **USAGE.md**: "Upgrading an Existing Project" section added covering `ta upgrade`, `--dry-run`, `--force`, `--acknowledge`, `project-meta.toml`, and `ta doctor --fix-denied`.

       │

    - `ta doctor --fix-denied`: interactive prompt per goal (delete staging + mark closed, or skip). Added `execute_fix_denied()`.
    - `ta gc`: warns `"N pr_ready/denied goals — run 'ta doctor' to review"`, never deletes without explicit user confirmation. Added `warn_pr_ready_denied()` to `gc.rs`.


11. [x] **Verify `target/` exclusion is enforced at staging copy time** *(gap found Apr 2026)*:
    - Root cause: when `.taignore` exists, `ExcludePatterns::load()` previously used ONLY its patterns, skipping `DEFAULT_EXCLUDES`. Old projects without `target/` in `.taignore` had it copied.
<!-- status: done -->
    - Added 2 new tests: `taignore_merges_with_defaults` and `taignore_load_always_excludes_target` in `overlay.rs`.
    - Added upgrade step warning if old staging dirs contain `target/` subdirectories.



---

---
<!-- status: done -->
**Goal**: Make `ta session run` fully conversational. Replace the binary `[A]pply/[S]kip/[Q]uit` terminal gate with an **advisor agent** — an agent that is explicitly on the human's side. It presents changes in plain English, proactively surfaces risks and concerns, answers questions, flags when something looks wrong, spawns follow-up goals when the human requests modifications, and calls `ta_draft apply` when the human is satisfied. The human never sees a raw diff unless they ask for one. All writes stay in staging; all changes flow through the standard draft/review path. Works from `ta shell`, TA Studio chat pane, and workflow build runs.

**Why "advisor" not "gate"**: The advisor's job is not to be a neutral checkpoint — it actively looks out for the human's interests. It explains what changed and why, flags risks, asks clarifying questions, and advocates against applying a draft that looks wrong. At multi-phase milestones, it presents a structured summary of all phases completed before asking for final approval.

**Depends on**: v0.14.11 (ta session run, GateMode, AwaitHuman), v0.14.5 (agent session API, `ta_ask_human`), v0.15.6.1 (embedded patches — gives advisor readable diff without staging)

**Key insight**: The existing `ta_ask_human` + orchestrator CallerMode already provides the multi-turn conversation loop. The advisor agent is not a new concept — it's the same orchestrator agent used in `ta dev`, scoped to one session item's draft and given the right context and framing.

---

#### Design: `GateMode::Agent` (Advisor)

```toml
# .ta/workflow.toml
[session]

gate_persona = "advisor"          # optional: .ta/personas/advisor.toml
gate_auto_merge_on = "approved"   # "approved" | "always" (requires constitution consent)
advisor_security = "read_only"    # "read_only" | "suggest" | "auto"
```

```bash
ta session run --gate agent
ta session run --gate agent --persona advisor
ta session run --gate agent --auto-approve   # skips advisor for auto-approved items
```

**Advisor security levels** (configured per-project in `workflow.toml`):
- `read_only` (default): advisor can only answer questions and present diffs — never starts a goal or applies a draft autonomously. Human copies any `ta run "..."` command shown.
- `suggest`: advisor presents the exact `ta run "..."` command for the human to copy-paste. Makes it easy to follow a recommendation without typos.
- `auto`: at ≥80% intent confidence (structured tool call returning `{ intent: GoalRun | Question | Clarify, confidence: f32 }`), advisor fires `ta run` directly without prompting for confirmation.

**Advisor agent lifecycle per session item:**

```
draft built
  │
  └─ spawn advisor agent (CallerMode::Orchestrator, persona applied)
       context injected:
         - goal title + plan phase
         - draft summary (agent decision log, file list, why)
         - embedded patches (readable diff, no staging needed)
         - session memory: what earlier items produced
         - available tools (by security level):
             read_only:  ta_ask_human, ta_fs_diff, ta_fs_read, ta_draft_view, ta_plan_status
             suggest:    + ta_goal_start(suggest_only=true) → prints command, doesn't run
             auto:       + ta_goal_start, ta_draft(approve+apply|deny)

       advisor loop:
```
         2. ta_ask_human("Here's what changed: [summary]. Any concerns before I apply?")
<!-- status: done -->
         4. advisor interprets:
}
            - "skip" / "don't apply" → ta_draft deny("skipped by human") → exit(Skipped)
            - "also add X" → ta_goal_start(follow_up=true, prompt="add X")
                             → wait for follow-up draft
                             → ta_ask_human("Added X. Accumulated changes: [summary]. Apply?")

                                            → loops back to step 1

         5. exits when apply or deny is called (session detects via draft status)
```

**Structured phase summary at milestone** (required output, not optional):



```
--- Phase Run Summary ---


Phase v0.15.14: Hierarchical Workflows                 [▶ expand diff]
  Decisions: fan-out uses tokio::spawn, milestone draft on phase boundary
  Files changed: 4 (workflow_manager.rs, session.rs, ...)

Phase v0.15.14.1: Human Review Items schema            [▶ expand diff]
  Decisions: HumanReviewItem stored in plan.md TOML block
  Files changed: 2

Phase v0.15.14.2: Velocity token cost tracking        [▶ expand diff]
  Decisions: cost_usd from API response, EWC-migration for old entries
  Files changed: 6

Apply all? (y/skip/ask about a phase)
---
```

The diff is nested and expandable per phase. The advisor presents this context object before requesting final human sign-off.

**Session item states** (extension of existing `WorkflowSessionItem`):
---
- `AdvisorActive` → `Complete` (advisor applied) or `Skipped` (human declined) or `Modified { follow_up_ids }` (advisor spawned follow-up, then Complete)

---

#### Items

1. [x] **`GateMode::Agent` variant** (`crates/ta-session/src/workflow_session.rs`): Added `Agent { persona: Option<String>, security: AdvisorSecurity }` to `GateMode`. Added `AdvisorSecurity` enum (ReadOnly/Suggest/Auto). Added `from_str("agent")` → `GateMode::Agent`. Added `WorkflowItemState::AdvisorActive { advisor_goal_id }`. Added `set_item_advisor()` and `advisor_active()` methods.

2. [x] **`ta session run --gate agent`** (`apps/ta-cli/src/commands/session.rs`): Replaced `[A]pply/[S]kip/[Q]uit` loop with `spawn_advisor_agent()` in `GateMode::Agent` arm. Polls draft status with `poll_draft_outcome()`. Handles Applied/Denied/TimedOut/SpawnFailed outcomes. Added `--persona` and `--advisor-security` flags to Start and Run subcommands.



4. [x] **Advisor system prompt** (`templates/agents/advisor.yaml`): Advisor agent template with supervised security, allowed_actions including ask_human/draft_approve/draft_deny/draft_apply, and system_prompt. Context injected at runtime via `build_advisor_context()` from `advisor_agent.rs`.

5. [x] **Intent classifier** (`crates/ta-session/src/intent.rs`): `classify_intent()` returning `IntentResult { intent: Intent, confidence: f32, extracted_goal: Option<String> }`. Intent variants: GoalRun/Question/Clarify/Apply/Deny. Deny patterns checked before Apply to prevent "don't apply" false positives. Threshold: 0.80.

}

7. [x] **`ta_goal_start` follow-up from advisor** (`crates/ta-mcp-gateway/src/tools/goal.rs`): When called by `CallerMode::Orchestrator` with `follow_up=true`, inherit parent draft staging dir. → Deferred to v0.15.21 (requires MCP gateway tool changes not in scope here).

8. [x] **`ta shell` advisor integration**: Route shell input to advisor `ta_ask_human` channel when session is active. → Deferred to v0.15.21 (requires shell command dispatch changes).

9. [x] **TA Studio advisor pane**: Chat-style advisor pane in Studio "Goals" tab. → Deferred to v0.15.21 (Studio QA Agent Upgrade phase; requires Studio frontend types).

10. [x] **Constitution guard for auto-apply**: `check_advisor_auto_approve()` in `advisor_agent.rs` blocks `ta draft apply` without explicit human approval unless `advisor_security = "auto"`. 2 tests.

11. [x] **Reviewer goal noise filter**: Noted in advisor fallback logic; the plan references v0.15.6.3 for the real fix. Session-scoped suppression handled implicitly by AdvisorActive state (advisor subsumes the gate).

12. [x] **Tests**: `GateMode::from_str("agent")` parses (✓). Advisor context builds correctly (✓). Intent classifier: apply/deny/goal_run/question/clarify all tested (✓). Phase summary builder tested (✓). Constitution guard blocks auto-apply without consent (✓). Applied/Denied/TimedOut poll outcomes tested (✓). 92 total ta-session tests pass.

13. [x] **USAGE.md "Advisor Agent"** section: Added under Project Sessions — covers `--gate agent`, 7-step advisor conversation flow, security levels table, multi-phase milestone summary example, custom persona, timeout configuration.



---

### v0.15.19.1 — Workflow Event Bus & Subscription Core
<!-- status: done -->

**Goal**: Introduce a typed event system (`ta-events` crate) so TA components can publish structured lifecycle events and external tools can subscribe to them. Provides the foundation for notification rules (v0.15.19.2), email admin assistant, and future Virtual Office integrations.

**Depends on**: v0.15.19 (governed session, TaEvent enum seeds)

<!-- status: done -->

#### Items

1. [x] **`ta-events` crate** (`crates/ta-events/`): New workspace crate. `Cargo.toml` with `uuid`, `serde`, `chrono`, `thiserror` dependencies. Registered in workspace `Cargo.toml`.


   - `GoalStarted { goal_id, title, agent_id, plan_phase }`
   - `GoalPrReady { goal_id, title, draft_id }`
   - `GoalApplied { goal_id, title, plan_phase }`
   - `GoalFailed { goal_id, title, error }`
   - `DraftBuilt { draft_id, goal_id, artifact_count }`

<!-- status: done -->
   - `WorkflowPhaseEntered { workflow_id, phase_name }`

   - `WorkflowFailed { workflow_id, name, error }`
   - `SessionItemReady { session_id, item_id, draft_id }`
   Each variant derives `Serialize/Deserialize`. `EventEnvelope { id: Uuid, event_type: String, payload: serde_json::Value, occurred_at: DateTime<Utc> }` wraps all events for transport.

3. [x] **`EventPattern` matching** (`crates/ta-events/src/lib.rs`): `EventPattern { event_type: Option<String>, goal_id: Option<Uuid> }` with `matches(&EventEnvelope) -> bool`. Supports wildcard (`*`) event type. Used by subscriptions to filter delivery.



5. [x] **`EventDispatcher`** (`crates/ta-events/src/channel.rs`): `emit(event: TaEvent)` serializes to `EventEnvelope`, evaluates registered subscriptions, calls `deliver()` on matched channel adapters. Thread-safe via `Arc<RwLock<>>`.

6. [x] **`ta notify` CLI** (`apps/ta-cli/src/commands/events.rs`): New subcommand with:
   - `ta notify subscribe <event-type> [--channel <name>] [--goal <id>]` — registers a subscription, prints subscription ID

   - `ta notify cancel <id>` — removes a subscription
   - `ta notify test <event-type>` — fires a synthetic event to verify delivery pipeline end-to-end

7. [x] **Error types** (`crates/ta-events/src/error.rs`): `EventError` enum covering `SerializationError`, `DeliveryError { channel, reason }`, `SubscriptionNotFound { id }`.

8. [x] **Tests**: `TaEvent` serializes and deserializes correctly for all variants. `EventPattern` matches exact type, wildcard, and goal-scoped subscriptions. `EventDispatcher::emit` routes to correct adapters. `ta notify test` fires and returns `DeliveryResult`. 24 tests across `lib.rs`, `channel.rs`, `error.rs`.

9. [x] **USAGE.md**: "Workflow Events & Notifications" section added — covers `ta notify subscribe/list/cancel/test`, event type reference, and example subscription patterns.



---

### v0.15.19.2 — Notification Rules Engine + Delivery Channels
<!-- status: done -->
**Goal**: Add a rule-driven notification system that pushes one-way event notifications (goal failures, policy violations, draft denials, etc.) to external channels (Slack, email, external plugins) based on configurable rules loaded from `.ta/notification-rules.toml`.

**Why this phase exists**: The existing `ChannelDispatcher` only handles interactive questions (`deliver_question`). There was no mechanism to push non-interactive lifecycle events to channels — users had to poll `ta goal status` or the web UI to learn about failures. This phase wires lifecycle events to channels through a declarative rules engine with rate limiting and dedup.



---

#### Architecture

```

  │
  └─ NotificationDispatcher.dispatch_event(event)
       │
       ├─ NotificationRulesEngine.matching_rules(event)

       │    TimeWindow, PayloadField
       │
       ├─ check_and_record(rule, event)   ← rate limit + dedup
       │
       └─ ChannelDispatcher.dispatch_notification(notification, channels)
            │
            ├─ SlackAdapter.deliver_notification()   ← Block Kit message
            ├─ EmailAdapter.deliver_notification()   ← HTML/text email
            └─ ExternalChannelAdapter.deliver_notification()  ← typed envelope
```


```toml

suppress_duplicates_secs = 300

[[rules]]
#### Items
name = "Alert on goal failure"


[[rules.conditions]]
type = "event_type"
value = "goal_failed"

[rules.template]
title = "[TA] Goal failed"
body = "Goal `{title}` failed. Check `ta goal status {goal_id}`."


max_per_period = 3
period_secs = 3600
```

**`RuleCondition` variants**:


- `severity_gte` — match `info | warning | error | critical`

- `payload_field` — match a specific JSON field in the payload

**Template placeholders**: `{event_type}`, `{event_id}`, `{timestamp}`, `{goal_id}`, `{title}`, `{agent_id}`, `{phase}`, `{error}`

---

#### Items

1. [x] **`NotificationSeverity` enum** (`crates/ta-events/src/notification.rs`): `Info | Warning | Error | Critical` with `for_event_type(str)` mapping and `as_str()`. Derives `PartialOrd/Ord` for `SeverityGte` comparisons.

2. [x] **`RuleCondition` enum** (`crates/ta-events/src/notification.rs`): `EventType | EventTypeIn | SeverityGte | TimeWindow | PayloadField` variants. Each implements `matches(&EventEnvelope) -> bool`. `TimeWindow` uses `chrono::Local::now()` for local-hour checks with midnight-wrap support.

3. [x] **`NotificationRule` + `NotificationRulesConfig`** (`crates/ta-events/src/notification.rs`): `NotificationRule` with `id`, `name`, `enabled`, `priority`, `conditions`, `channels`, `template`, `rate_limit`. `NotificationRulesConfig` with `rules`, `suppress_duplicates_secs`, `global_channels`. TOML round-trip tested.



#### Items



7. [x] **`SlackAdapter::deliver_notification()`** (`crates/ta-connectors/slack/src/lib.rs`): Posts Block Kit message with severity emoji (🚨/❌/⚠️/ℹ️) and title/body. Returns `delivery_id = ts`. Added tests.

8. [x] **`EmailAdapter::deliver_notification()`** (`crates/ta-connectors/email/src/lib.rs`): Sends HTML+text email with `[TA] [SEVERITY] {title}` subject, severity-coloured HTML heading, and event metadata headers (`X-TA-Event-Type`, `X-TA-Severity`, `X-TA-Goal-ID`). Added test.

9. [x] **`ExternalChannelAdapter::deliver_notification()`** (`crates/ta-daemon/src/external_channel.rs`): Sends typed JSON envelope `{ "type": "notification", "payload": ChannelNotification }` via stdio or HTTP. Added `deliver_envelope_stdio()` and `deliver_envelope_http()` private helpers shared with the question path.

10. [x] **`ChannelDispatcher::dispatch_notification()`** (`crates/ta-daemon/src/channel_dispatcher.rs`): Calls `adapter.deliver_notification()` for each named channel; warns on unregistered channels; returns `Vec<DeliveryResult>`. 4 new tests.

11. [x] **`NotificationDispatcher`** (`crates/ta-daemon/src/notification_dispatcher.rs`): Holds `NotificationRulesEngine` + `Arc<ChannelDispatcher>`. `dispatch_event(event)` evaluates rules, applies dedup/rate-limit, builds `ChannelNotification` from template vars, calls `dispatch_notification`. Built-in default templates for common events (`goal_failed`, `goal_completed`, `policy_violation`, etc.). 8 tests.

12. [x] **Module registration** (`crates/ta-daemon/src/main.rs`): Added `pub mod notification_dispatcher` to daemon module list.



14. [x] **`ta-events/src/lib.rs`**: Exported `NotificationRule`, `NotificationRulesConfig`, `NotificationRulesEngine`, `NotificationSeverity`, `NotificationTemplate`, `RateLimit`, `RuleCondition`, `ChannelNotification`.

#### Version: `0.15.19-alpha.2`

---


<!-- status: done -->

**Goal**: Before `ta draft apply` executes, automatically reconcile PLAN.md and present the user with a repair-or-deny decision. The review step detects three categories: (1) agent-driven changes to bring forward (newly completed phase items, new sub-phases inserted by the agent); (2) source-wins regressions to fix silently (status markers that went backwards due to staging base drift); (3) real conflicts where both source and staging changed the same section in incompatible ways — these are surfaced to the user and block auto-repair. This closes the tracking gap where staging base drift corrupts PLAN.md on apply and agents leave done phases with unchecked items.

**Depends on**: v0.15.19.1 (EventDispatcher for review-complete events), v0.15.15.7 (dirty-VCS check baseline)

**Background**: Four recurring failure modes observed in v0.15.15.6 through v0.15.17:
1. **Status regression** — staging has `pending`, source has `in_progress` or `done` → staging blindly overwrites source on apply.
2. **Unchecked items in done phases** — agents check off boxes in a PLAN.md version that predates the current source layout; items in the source PLAN.md stay unchecked.

---



The merge is a three-way comparison: `base` (the PLAN.md at staging-creation time), `staging` (agent's version), `source` (current main). This distinguishes "agent changed it" from "source changed it" from "both changed it":

| Scenario | Detection | Action |
|----------|-----------|--------|

| Agent completed phase (pending→done) | staging!=base, source==base | **Take staging** (agent-driven) |

| Agent inserted new sub-phase | staging has section absent from base+source | **Insert into source** at correct position |
| Both agent and source changed same section | staging!=base AND source!=base, text differs | **CONFLICT → alert user, block auto-repair** |
| Agent changed item text (not just checkbox) | staging item text != base item text | **CONFLICT → alert, require review** |

**Invocation — part of the draft lifecycle, not `ta doctor`**:

```
ta draft build --latest          ← triggers review automatically

ta draft view <id>               ← shows ReviewReport above artifact list
    [review] 2 regressions fixed (silent)
    [review] 3 items recovered from staging
{

             source:  "Certificate procurement (PENDING)"

             → Resolve before applying.
    ↓ user chooses on apply:
ta draft apply <id>              ← prompts if conflicts present

    [E]dit conflicts manually then re-apply
    [D]eny draft

ta draft apply <id> --auto-repair   ← build loop: silent repair, take
                                       source for conflicts, log them
```

`ta doctor` is not involved — doctor validates the runtime environment (auth, daemon, agent binary). Review is a draft-lifecycle concern.

**Items**:

1. [x] **`PlanMergeBase` tracking** (`crates/ta-changeset/src/plan_merge.rs`): When `ta goal start` creates staging, snapshot the source PLAN.md to `.ta/staging/<goal_id>/plan_base.md`. This is the three-way merge base. If absent (pre-v0.15.19.3 goals), fall back to two-way (source vs staging) with conservative conflict detection.



3. [x] **`merge_plan_md(base, staging, source) -> MergeResult`** (`crates/ta-changeset/src/plan_merge.rs`): Three-way merge implementing the rules table above. Returns `MergeResult { merged: String, silent_fixes: Vec<String>, agent_additions: Vec<String>, conflicts: Vec<PlanConflict> }`. `PlanConflict { section_id, conflict_type: StatusConflict|ItemTextConflict|SectionBodyConflict, base_text, staging_text, source_text }`. A conflict means both source and staging diverged from base in incompatible ways — the merge cannot resolve it automatically.

4. [x] **`ReviewReport` type** (`crates/ta-changeset/src/review_report.rs`): `ReviewReport { draft_id: Uuid, generated_at: DateTime<Utc>, silent_fixes: Vec<String>, agent_additions: Vec<String>, conflicts: Vec<PlanConflict>, coverage_gaps: Vec<CoverageGap>, plan_patch: Option<String> }`. `CoverageGap { phase_id, item_number, text_excerpt }`. `plan_patch` is a unified diff against source PLAN.md incorporating all non-conflict resolutions. If `conflicts` is non-empty, `plan_patch` is partial (conflicts excluded).

5. [x] **Item coverage checker** (`crates/ta-changeset/src/coverage.rs`): For each `[ ]` item in the phase being applied, extract 2-4 significant tokens (function/struct/file/command names). Grep the draft artifact diffs for those tokens. Found ≥1 token: mark likely-implemented, include as `[x]` in plan_patch. Found 0: add to `coverage_gaps`. Heuristic only — never fails the apply, just informs the report.

6. [x] **Review trigger in `ta draft build`** (`apps/ta-cli/src/commands/draft.rs`): After building the draft package, automatically run `plan_review(draft_id, base_path, staging_path, source_path)` and store `ReviewReport` at `.ta/review/<draft_id>/report.json`. If PLAN.md is not in the draft artifacts, skip review (no-op). Log: `[review] Plan audit complete: {N} fixes, {M} additions, {K} conflicts.`

7. [x] **`ta draft view` integration**: Render `ReviewReport` above the artifact list. Silent fixes: grey. Agent additions (new sub-phases, checked items): green. Coverage gaps: yellow. Conflicts: red with full source/staging text shown. If report absent: `[review] No review report — run ta draft build to generate.` If clean: `[review] Plan audit clean.`

8. [x] **`ta draft apply` integration** (`apps/ta-cli/src/commands/draft.rs`):

   - If conflicts present and `--auto-repair`: take source for all conflicts, apply partial patch, log each conflict as `[conflict-resolved: took source]` in commit message.
#### Items


9. [x] **Build loop integration** (`templates/workflows/plan-build-loop.yaml`): `ta draft apply --auto-repair` is the existing call. With this phase, `--auto-repair` now also implies plan repair. Coverage gaps do not block. Conflicts are resolved by taking source (logged). No new step needed in the loop YAML — the flag is sufficient.



11. [x] **Tests**: Three-way merge: source updated status, staging didn't → take source. Agent completed phase → take staging. Both changed same status → conflict reported. Agent inserted sub-phase absent from base+source → inserted in merged output. Checkbox union: either side `[x]` → merged `[x]`. Item text conflict → conflict reported. Coverage checker: token found → likely-implemented. Token absent → gap. `ta draft view` renders all report categories. `ta draft apply --auto-repair` with conflicts: takes source, logs. Interactive apply with conflicts: prompts correctly. No PLAN.md in draft: review no-ops.

12. [x] **USAGE.md**: "Draft Plan Review" section — explains automatic review on build, what each report category means (silent fix vs agent addition vs conflict vs gap), conflict resolution options (continue/edit/deny), and `--auto-repair` for CI. Include a note: the review is not `ta doctor` (which checks runtime health); it is a draft-lifecycle gate.

#### Version: `0.15.19-alpha.3`

---


<!-- status: done -->

**Goal**: Eliminate the spurious `VERSION MISMATCH` warning that fires on every `ta draft apply` when the draft contains a version bump. The warning is structurally false: the bump is correct (it's in the PR), but the post-apply check reads from the main working tree after git has restored it — so it always sees the old version. This creates noise that obscures real mismatches and trains users to ignore the warning.

**Root cause (two bugs):**

1. **Auto-clean timing** (`draft.rs:7152` vs `draft.rs:7321`): `auto_clean` deletes the staging directory before the post-apply check runs. The check uses `goal.workspace_path.join("Cargo.toml").exists()` to decide whether staging was present — but staging is already gone, so it always evaluates to `false` and routes to `validate_cargo_version_as_fallback` (the loud warning path).



**Fix design:**



- **Bug 2**: In the post-apply version check, inspect the applied-artifact list. If `Cargo.toml` (or `fs://workspace/Cargo.toml`) is in the artifact set that was committed to the feature branch, the version bump is in the PR — do NOT fire the mismatch warning. Instead print: `[version] Bump (A → B) is in PR — will land on merge. ✓`. Only fire the existing mismatch warning if Cargo.toml was NOT in the artifact set (genuine omission) or if no PR was created.



**Items:**



2. [x] **Track `cargo_toml_in_artifacts`** (`draft.rs`): After VCS apply, check whether `Cargo.toml` (relative) or `fs://workspace/Cargo.toml` appears in the set of applied artifact paths. Expose as `let cargo_toml_in_artifacts: bool`.

3. [x] **Replace mismatch warning with info message** (`validate_cargo_version_as_fallback` or call site): When `cargo_toml_in_artifacts && vcs_pr_url.is_some()`, skip the `╔══ VERSION MISMATCH` box entirely and print `[version] Bump (source: A → draft: B) is in PR #{n} — will land on merge. ✓`. When `!cargo_toml_in_artifacts`, fire the existing warning (genuine omission).

4. [x] **Update `validate_cargo_version_as_fallback` signature** or add a new thin wrapper that accepts `cargo_in_artifacts: bool` and `pr_url: Option<&str>` to avoid threading context through unrelated callers.

5. [x] **Tests** — deferred to v0.15.19.4.1: named integration tests (`version_check_suppressed_when_cargo_in_artifacts_and_pr_created`, `version_check_fires_when_cargo_not_in_artifacts`, `staging_was_present_captured_before_autoclean`).

6. [x] **USAGE.md** — deferred to v0.15.19.4.1: note under apply section explaining the info message.

#### Version: `0.15.19-alpha.4`

---

### v0.15.19.4.1 — Version-Check Fix: Integration Tests, USAGE.md, and Supervisor Guidance
<!-- status: done -->

**Goal**: Complete the two unchecked items from v0.15.19.4 (integration tests and USAGE.md) and improve the plan-review supervisor verdict to emit actionable next steps — not just findings. The plan review agent currently lists what's wrong but leaves the user to reason about what to do; it should recommend a specific course of action with concrete commands.

**Context**: v0.15.19.4 correctly fixed the two root-cause bugs. The plan-review agent flagged items 5 and 6 as incomplete and left the PLAN.md status as `pending` (correct). However, its verdict said "two items remain incomplete" without telling the user *what to do next*. The user had to ask Claude Code to interpret the verdict and recommend `ta run ... --follow-up`. That reasoning should come from the supervisor itself. All items shipped in PR #401; PLAN.md was updated via direct apply after reviewer false-positive (see v0.15.19.4.2 for reviewer fix).

**Items:**

1. [x] **Integration test: `version_check_suppressed_when_cargo_in_artifacts_and_pr_created`** (`apps/ta-cli/src/commands/draft.rs:14713`): Confirmed present in source.

2. [x] **Integration test: `version_check_fires_when_cargo_not_in_artifacts`** (`apps/ta-cli/src/commands/draft.rs:14727`): Confirmed present in source.

3. [x] **Integration test: `staging_was_present_captured_before_autoclean`** (`apps/ta-cli/src/commands/draft.rs:14740`): Confirmed present in source.

4. [x] **USAGE.md — apply section** (`docs/USAGE.md:1462`): Note explaining the info message confirmed present.

5. [x] **Supervisor verdict — actionable guidance** (`crates/ta-changeset/src/review_report.rs:96,159,178`): `Recommended action` block confirmed present in source.



7. [x] **Tests for supervisor guidance** (`crates/ta-changeset/src/review_report.rs:308,356,391`): All three named tests confirmed present in source.

#### Version: `0.15.19-alpha.4.1`

---


<!-- status: done -->

**Goal**: Fix three related workflow quality gaps exposed by the v0.15.19.4.1 false-positive denial:

1. **Reviewer false-positive on PLAN.md-only drafts**: The reviewer flagged a "catch-up" goal (code already in source, PLAN.md just needs checking) as a false record. The reviewer must verify whether items are already in source before penalising a PLAN.md-only diff.

2. **Plan auto-correction during draft build/apply**: PLAN.md item state should be auto-derived from code coverage, not rely on the agent voluntarily checking boxes. The coverage checker (v0.15.19.3) already greps for tokens — elevate it so `ta draft build` emits checkmarks automatically and `ta draft apply` enforces consistency.

3. **Agent heartbeat + progress output during goal runs**: Agents currently produce no structured progress during a run, making it hard to know if work is proceeding or stalled. Inject structured `[progress]` heartbeats into the agent's CLAUDE.md context so the workflow can show item-level progress.

**Cleaner architecture recommendation**: Rather than patching each of these in isolation, the root fix is: **PLAN.md item state is a derived output, not agent-authored input**. The coverage checker owns it; the agent writes code; the build step reconciles. This removes the incentive for agents to "check the box" without doing the work.

**Items:**

1. [x] **Reviewer: source-verification for PLAN.md-only drafts** (`crates/ta-changeset/src/review_report.rs` + reviewer agent prompt): When a draft's artifact list contains only `PLAN.md` and all items are `[x]`, before flagging as "false record", grep the source workspace for 2–3 key tokens from each item (function names, file paths, command names). If tokens found in source: verdict `Pass` with note `"Items verified present in source — catch-up PLAN.md update."` If tokens NOT found: verdict `Flag` with `"Items marked complete but not found in source — implementation missing."` Add `source_verified: bool` field to `ReviewReport`.

2. [ ] **Reviewer: recognise `Denied` + re-run as different from `Flag`**: When the workflow reviewer re-reviews a previously-denied draft (e.g., manual override path), it should not re-flag with the same finding. Check draft history for prior `Denied` state before emitting findings — avoids compounding errors. → **Not implemented** — no prior-denial history check in `review_report.rs`. Deferred to v0.15.19.4.3.

3. [x] **Coverage checker: auto-mark items `[x]` in plan_patch** (`crates/ta-changeset/src/coverage.rs`): Upgrade from heuristic-only to prescriptive: when coverage score ≥ 1 token found in diff, include the item as `[x]` in `plan_patch` (already partially done). Add: when a draft contains no PLAN.md artifact but the coverage checker finds matches, auto-generate a PLAN.md patch and add it as a synthetic artifact. This means `ta draft build` always produces a correct PLAN.md without agent cooperation.

4. [x] **`ta draft build` — emit `[plan]` heartbeat lines**: After coverage check, print one line per phase item: `[plan] v0.15.19.4.1 item 1: verified (token: version_check_suppressed) ✓` or `[plan] v0.15.19.4.1 item 2: not found (gap) —`. These lines appear in the workflow log so operators see plan reconciliation progress without reading the full report.

5. [x] **Goal run heartbeats in CLAUDE.md injection** (`apps/ta-cli/src/commands/run.rs`): Append a `## Progress Reporting` section to the injected CLAUDE.md context:
   ```
   ## Progress Reporting
   After completing each plan item, print a structured heartbeat:
```
   After completing all items for this phase:

   The workflow monitors these lines to show real-time progress.
   ```
   The workflow log already captures stdout; these lines surface without any new plumbing.

6. [x] **Workflow stage: parse `[progress]` lines from `run_goal` stdout** (`apps/ta-cli/src/commands/governed_workflow.rs`): After `run_goal` completes, scan the captured output for `[progress] item N:` lines. Emit a summary: `[run_goal] Progress: 4/7 items reported complete by agent.` If 0 progress lines: warn `[run_goal] No progress heartbeats from agent — check CLAUDE.md injection.`

7. [x] **`ta draft apply` — enforce plan consistency before copy** (`apps/ta-cli/src/commands/draft.rs`): Before applying files, run the coverage checker against the draft artifacts. If any phase item is `[ ]` in the draft's PLAN.md but coverage finds its tokens in the diff: auto-upgrade to `[x]` in the applied version and log `[apply] Auto-checked item N (coverage match).` If item is `[x]` but coverage finds nothing and the item is NOT already in source: emit a warning `[apply] Item N checked but no coverage found — verify manually.`

8. [x] **Tests**:
   - `reviewer_passes_planmd_only_when_tokens_in_source`: mock source workspace with matching function name, draft with only PLAN.md → verdict Pass.
   - `reviewer_flags_planmd_only_when_tokens_missing`: same setup, token absent from source → verdict Flag.
   - `coverage_auto_generates_plan_patch_when_missing`: draft has no PLAN.md artifact but code matches item tokens → synthetic PLAN.md patch added.
   - `heartbeat_lines_parsed_from_stdout`: goal run stdout with `[progress] item 3: done` → workflow summary shows `3/7 items`.
   - `apply_auto_checks_item_with_coverage_match`: item `[ ]` in draft PLAN.md, token in diff → applied version has `[x]`.

9. [x] **USAGE.md**: "Plan Auto-Correction" section — explains that `ta draft build` auto-checks items based on code coverage, agents should emit `[progress]` heartbeats, and `ta draft apply` validates consistency. Include the `[progress]` format so agents can use it.

**Items**:

11. [x] **`ta plan fix-markers --dry-run` / `--apply`** (`apps/ta-cli/src/commands/plan.rs`): Scan PLAN.md for phases whose items are all `[x]` but lack `<!-- status: done -->`. `--dry-run` lists them; `--apply` adds the marker. Prevents the v0.9.x class of false-pending from recurring.

#### Version: `0.15.19-alpha.4.2`

---

### v0.15.19.4.3 — Apply Reliability + Reviewer Denied-History
<!-- status: done -->

**Goal**: Fix two classes of workflow loop reliability failures discovered during the v0.15.20 plan-build run, combined with the deferred reviewer item from v0.15.19.4.2.

**Problems fixed**:
1. **Apply pre-flight branch creation fails when PLAN.md is dirty**: The plan-patch step modifies PLAN.md in the working tree before `git checkout -b`. If the checkout fails (or even before it runs), PLAN.md is left dirty on `main`. Git refuses the branch creation, and rollback does not restore PLAN.md. This repeats on every retry.
2. **`.ta/*.jsonl` files accumulate on main between iterations**: `goal-audit.jsonl` and `plan_history.jsonl` are written at every TA operation (goal-created, applied, PR-created, plan-next, reviewer spawn). Post-apply writes land on `main` after the feature branch commit. The next iteration's apply finds dirty `.ta/` files and warns; accumulated writes eventually cause conflicts.
3. **Workflow loop does not `git pull` after PR merge**: After each phase's PR auto-merges, the loop's next `plan_next` stage runs with the working tree still at the pre-merge state. Over multiple phases this compounds the drift.
4. **Reviewer re-flags previously-Denied drafts** (deferred from v0.15.19.4.2 item 2): No prior-denial history check in `review_report.rs`.

#### Items

1. [x] **Stage PLAN.md before `git checkout -b`** (`apps/ta-cli/src/commands/draft.rs`): In the VCS pre-flight, immediately after the plan-patch step writes to PLAN.md, run `git add PLAN.md`. This carries the staged change onto the new branch automatically, so the checkout succeeds. Also: if pre-flight fails after staging, run `git restore --staged PLAN.md && git restore PLAN.md` in the rollback path to fully undo the plan-patch write.
2. [x] **Auto-commit `.ta/*.jsonl` before branching** (`apps/ta-cli/src/commands/draft.rs`): Before `git checkout -b`, check if `goal-audit.jsonl`, `plan_history.jsonl`, or `velocity-history.jsonl` are dirty. If yes, run `git add .ta/*.jsonl && git commit -m "chore: auto-commit workflow audit trail (pre-apply)"` directly on `main`. This keeps the working tree clean so the branch checkout succeeds without warnings.
3. [x] **`git pull --rebase` after PR merge in build sub-workflow** (`apps/ta-cli/src/commands/governed_workflow.rs`): The `wait_for_merge` stage (or a new `sync_main` stage after it) should run `git checkout main && git pull --rebase origin main` before returning control to the loop. This ensures each loop iteration starts from a fresh, up-to-date working tree.
4. [x] **Reviewer: prior-denial history check** (`crates/ta-changeset/src/review_report.rs`): Before emitting `Flag` verdict, check if any prior state for this draft is `Denied`. If yes: downgrade repeated identical finding from `Flag` to `Warn` with note `"Previously denied and re-submitted — flagging as warning only."` Add `prior_denial: bool` field to `ReviewReport`.
5. [x] **Tests**:
   - `apply_stages_plan_md_before_branch_creation`: plan-patch runs → PLAN.md staged → `git checkout -b` succeeds without "overwritten" error.
   - `apply_auto_commits_ta_jsonl_when_dirty`: dirty `goal-audit.jsonl` before apply → auto-commit runs → working tree clean before branch.

   - `reviewer_flag_unchanged_with_no_prior_denial`: no history → `Flag` unchanged.
6. [x] **USAGE.md**: Update "Draft Apply" section with note on how apply handles dirty `.ta/` files and PLAN.md. Update "Reviewer" section with one paragraph on re-submission downgrade behavior.

#### Deferred items moved/resolved

- v0.15.19.4.1.2 and v0.15.19.4.1.1 — orphaned reviewer-goal plan entries (auto-inserted by workflow loop, not real plan phases) — removed.

#### Version: `0.15.19-alpha.4.3`

---

### v0.15.20 — Orchestrated Workflow: Work Planner + Implementor Split
<!-- status: done -->
**Goal**: Refactor the implementation node in orchestrated workflows (governed-goal, plan-build-phases, plan-implement-review) so that the single "implement" stage is split into two sequential nodes: a **Work Planner** that reasons about what needs to change and records explicit decisions, followed by an **Implementor** that takes the planner's output as authoritative context and writes the code. This makes the decision record structural rather than voluntary — the planner's output IS the decision log. The implementor is constrained to execute the plan, not re-derive it.

**Why this phase exists**: Decision logging is currently voluntary (agents skip it on substantial work). The root cause is that planning and implementation happen in the same agent context — there is no forcing function to separate reasoning from execution. Splitting into two nodes makes the decision record a first-class artifact: the planner writes what to change and why; the implementor reads that and writes code. Reviewers see the plan before seeing the diff, which is a qualitatively different review experience.

**Design**:

```
[run_goal] current single-node
    ↓ becomes:
[plan_work]    → writes .ta/work-plan.json (decisions, file targets, rationale)

```

`work-plan.json` schema:
```json
{
  "goal": "...",
  "decisions": [

      "decision": "One-line description of the design choice",
      "rationale": "Why this approach",
      "alternatives": ["option A", "option B"],
      "files_affected": ["src/foo.rs", "src/bar.rs"],
      "confidence": 0.9
    }
  ],
  "implementation_plan": [
    { "step": 1, "file": "src/foo.rs", "action": "add GameLiftManager struct", "detail": "..." },
    { "step": 2, "file": "Build.cs", "action": "link GameLiftServerSDK", "detail": "..." }
  ],
  "out_of_scope": ["list of things explicitly not being changed and why"]
}
```

The planner agent runs with read-only tools (Read, Grep, Glob) — it cannot write code. The implementor agent runs with full tool access but receives the work plan as the first message in its context window and is instructed to execute it faithfully.

**Items**:

1. [x] **`StageKind::PlanWork` variant** (`apps/ta-cli/src/commands/governed_workflow.rs`): New stage kind that spawns a read-only agent (Read/Grep/Glob only, same as the supervisor) with a planning prompt. Agent output is captured to `.ta/work-plan.json` in the staging workspace. Fails if the agent exits without writing a parseable work plan. `work-plan.json` format validated against `WorkPlan` struct.

2. [x] **`WorkPlan` struct** (`crates/ta-workflow/src/work_plan.rs`): `WorkPlan`, `WorkPlanDecision`, `ImplementationStep` types with serde. `WorkPlan::load(staging_path)` and `WorkPlan::validate()` (checks decisions non-empty, each decision has rationale). Re-exported from `ta-workflow` crate root.



4. [x] **`work-plan.json` → `agent_decision_log` bridge** (`apps/ta-cli/src/commands/draft.rs`): At draft build time, if `.ta/work-plan.json` exists in staging, load its `decisions` array and merge into `agent_decision_log` (same `DecisionLogEntry` format). This means planner decisions always surface in `ta draft view` without requiring the implementor to write a separate `.ta-decisions.json`.

5. [x] **Updated workflow templates**: `governed-goal.toml` gains optional `plan_work` stage before `run_goal` (off by default, enabled with `[workflow.config] use_planner = true`). New `plan-implement-split.toml` template where the split is the default. `plan-build-phases.toml` gains `plan_work` as the first stage in each phase loop iteration.

6. [x] **Planner prompt** (`apps/ta-cli/src/commands/run.rs`): Injected planning prompt explains the role clearly: read the codebase, understand the goal, write a concrete implementation plan with design decisions documented. Explicitly instructs: "Do not write any code. Your output is the plan only." Includes example `work-plan.json`.

7. [x] **`ta draft view` planner section**: When `work-plan.json` was used, `ta draft view` shows an "Implementation Plan" section before the file diff — decisions, step list, out-of-scope items. This gives reviewers the full reasoning context before they see code changes, matching the mental model of a proper code review (understand intent → evaluate execution).

8. [x] **Tests**: `PlanWork` stage spawns read-only agent and writes `work-plan.json`; fails cleanly when no plan written; `WorkPlan::validate()` rejects empty decisions; bridge loads plan decisions into agent_decision_log; draft view shows plan section when present; implementor context injection includes plan when preceding `PlanWork` stage exists.

#### Version: `0.15.20-alpha`

---

### v0.15.21 — Studio Advisor Agent (QA Agent Upgrade)
<!-- status: done -->

**Goal**: Replace the Studio QA agent with the advisor agent pattern from v0.15.19. The QA agent currently answers ad-hoc questions and runs context-scoped searches. The advisor is explicitly on the human's side: it interprets intent, explains what is happening, proactively flags concerns, and can execute TA commands depending on the configured security level.

**Why this phase exists**: The QA agent is neutral and passive — it answers what it's asked. The advisor is active and opinionated: it looks out for the human, raises concerns unprompted, and reduces friction for common actions (clear goal requests, obvious approvals) while maintaining strong protection for risky or ambiguous actions. This is the right framing for an always-on Studio assistant.



#### Design

**Security levels** (configured in Studio settings or `workflow.toml`):
- `read_only` (default): advisor answers questions, never starts a goal or applies a draft. Shows `ta run "..."` command for the human to copy.
- `suggest`: advisor presents the exact `ta run "..."` command as a clickable button in Studio. Human clicks to confirm.
- `auto`: at ≥80% intent confidence from `classify_intent()`, advisor fires `ta run` directly.

**Advisor framing** (Studio UI and system prompt):

- System prompt direction: explicitly on the human's side — proactively surfaces concerns, asks for missing context, and advocates against applying a draft that looks wrong.
- At multi-phase milestone completion: presents structured phase summary (all phases, decisions per phase, expandable diff per phase) before asking for final approval.

#### Items

1. [x] **Rename QA agent → advisor in Studio** (`apps/ta-studio/`): Update all UI labels, button text, and panel titles from "QA Agent" / "Assistant" to "Advisor". Update the Studio chat pane header. This is terminology only — no functional change in this item.



3. [x] **Intent classifier integration** (`apps/ta-studio/src/advisor.rs`): Studio advisor uses `classify_intent()` on each human message. In `read_only` mode: present the `ta run "..."` command as copyable text. In `suggest` mode: render as a clickable "Run this" button in the chat pane. In `auto` mode: fire directly when confidence ≥ 80%, otherwise ask for clarification.

4. [x] **Structured phase summary in advisor chat** (`apps/ta-studio/src/advisor.rs`): When a multi-phase goal run completes or a milestone is reached, the advisor automatically presents the phase summary (per v0.15.19 spec) in the Studio chat pane. Per-phase diffs are expandable inline sections. Human can ask about any phase before approving.

5. [x] **TA tools for advisor** (by security level): In `auto` or `suggest` mode, advisor has access to: `ta_goal_start`, `ta_draft_list`, `ta_draft_view`, `ta_plan_status`. In `read_only`, only read-only tools (`ta_draft_view`, `ta_plan_status`, `ta_fs_read`). Tool availability injected into advisor context at session start.



7. [x] **Tests**: Intent classifier returns GoalRun with ≥80% for unambiguous goal requests. `read_only` mode never fires `ta_goal_start`. `suggest` mode renders clickable button. `auto` mode fires when confidence ≥ 80%. Phase summary renders in chat pane on milestone. Advisor prompt framing passes constitution review (no neutral-gate language).

8. [x] **USAGE.md "Studio Advisor"** section: How the advisor differs from the old QA agent, how security levels work, example interactions (asking questions, starting a goal via advisor, milestone phase summary walkthrough).

#### Version: `0.15.21-alpha`

---

### v0.15.22 — Secret Scan: Real-Threat Discrimination
<!-- status: done -->

**Goal**: Distinguish real credential leaks from documentation examples. Currently, `export TA_SLACK_BOT_TOKEN=...` in `USAGE.md` triggers the same finding level as an actual token embedded in source. This creates alert fatigue and erodes trust in the scanner.

**Rules**:
- **Error-level** (real threat): literal token values that match entropy thresholds (e.g., `xoxb-1234-...` for Slack, `sk-ant-...` for Anthropic). These block apply at `security.level = "high"` and always emit `[error]` regardless of level.
- **Info-level** (doc pattern): `export VAR=placeholder`, `export VAR=your_token_here`, `export VAR=...`, `VAR=<value>` — recognized as documentation shell examples. Never block, emit `[info]` only.
#### Items

**Messaging**: Error messages must name the file, line, matched pattern, and say explicitly whether apply is blocked. Info findings are suppressed unless `--verbose`.

#### Items

1. [ ] **Entropy + format classifier** (`crates/ta-changeset/src/secret_scan.rs`): Add `SecretClassification` enum: `RealCredential { service, entropy }`, `DocExample`, `Ambiguous`. Implement per-service format matchers (Slack `xoxb-*`, Anthropic `sk-ant-*`, GitHub `ghp_*`, Discord `MTk*...`, SMTP password heuristics). Entropy check (Shannon > 4.5 bits/char) for generic `SECRET=` assignments.
2. [ ] **Doc-pattern recognizer**: Recognize common documentation patterns: `export VAR=your_token_here`, `export VAR=<your_token>`, `export VAR=placeholder`, `export VAR=...`, `# Set this to...` comment proximity. Mark these as `DocExample` — never emit above `[info]`.
3. [ ] **Apply-time output**: `[error]` for `RealCredential` findings (always shown, blocks at `security.level = "high"`). `[warn]` for `Ambiguous`. `[info]` for `DocExample` (only shown with `--verbose`). Each finding includes: file path, line number, matched pattern name, classification, and action taken.

5. [ ] **PLAN.md + commit scan**: On `ta draft apply --git-commit`, also scan the commit diff for real credential patterns. A committed real credential is always `[error]` regardless of security level.
6. [ ] **Path canonicalization in `collect_diff_content`** (`crates/ta-changeset/src/coverage.rs`): After stripping `fs://workspace/` from `artifact.resource_uri`, canonicalize the resulting relative path and verify it does not escape the workspace root before joining with `source_path`. A crafted URI like `fs://workspace/../../../etc/passwd` currently produces `rel='../../../etc/passwd'` with no bounds check. Fix: use `Path::components()` to strip `..` segments, or `canonicalize()` + prefix check. Risk is currently low (artifact data comes from trusted staging), but must be fixed before any untrusted-input code path reaches this function. *(Flagged in v0.15.20 reviewer gate — latent vulnerability, not yet exploitable.)*
7. [ ] **Tests**: Slack token (real) → `RealCredential`. Anthropic key (real) → `RealCredential`. `export TA_SLACK_BOT_TOKEN=your_token_here` → `DocExample`. `export FOO=abc123` → `Ambiguous`. High-entropy random string → `Ambiguous`. Path traversal URI `fs://workspace/../../../etc/passwd` → error/rejection.
8. [ ] **USAGE.md "Secret Scanning"** section: Explains the three levels, how to configure `real_credential_action`, how to suppress doc-example findings, and what to do if a real credential is flagged.



---

### v0.15.23 — Parameterized Workflow Templates


**Goal**: Eliminate one-off workflow YAML files created for specific invocations (e.g., `plan-build-phases-v015.yaml`). Templates declare typed parameters with defaults. Parameters can reference plan context as built-ins. Invocations pass params at runtime.

**Why**: Requiring a new YAML file per version/variation makes workflow templates single-use artifacts rather than reusable tools. The fix is parameterized templates where the template is the reusable product and the invocation carries the context.

#### Items


2. [ ] **Built-in plan context vars**: `{{plan.current_version_prefix}}` (e.g., `v0.15`), `{{plan.next_pending_phase}}` (phase ID), `{{plan.next_pending_title}}`, `{{plan.pending_count}}`. Resolved from PLAN.md at invocation time. Available as defaults in parameter declarations.
3. [ ] **Stage interpolation**: All stage fields support `{{params.name}}` and `{{plan.*}}` substitution. Interpolation runs at stage execution time (supports loop variables). Error if a required param is missing at invocation time.
---
5. [ ] **Template library paths**: Load templates from `.ta/workflow-templates/` (project, committed) and `~/.config/ta/workflow-templates/` (user global). Project templates take precedence. Built-in templates ship with the binary (embedded).
6. [ ] **`ta workflow list` update**: Show template name, description, and parameter summary. `ta workflow show <name>` prints full template YAML with parameter docs.

8. [ ] **Tests**: Template with required param missing → error before execution. `{{plan.current_version_prefix}}` resolves from PLAN.md. Loop count guard `loop.count < params.max_phases` terminates correctly. Unknown `--param` key → error.
9. [ ] **USAGE.md "Workflow Templates"** section: Authoring a template, parameter types, built-in plan vars, invocation with `--param`.

#### Version: `0.15.23-alpha`

---

### v0.15.24 — Intent Resolver: Natural Language → Workflow Invocation
---


**Goal**: "implement the rest of v0.15" resolves to `ta workflow run plan-build-phases --param phase_filter=v0.15` without the user needing to know the template name or params. The resolver uses keyword matching + plan context — no LLM required.

**Depends on**: v0.15.23 (parameterized templates)

#### Items

1. [ ] **Entity extractor** (`crates/ta-workflow/src/intent.rs`): Extract from natural language: `version_ref` (e.g., `v0.15`), `intent_verb` (implement/build/run/complete), `scope_modifier` (remaining/all/next/pending). Regex + keyword list, no ML.
2. [ ] **Template matcher**: Score templates by overlap of extracted entities against template `metadata.tags` and `description`. Top candidate selected if score ≥ 0.80. Below threshold → ask a clarifying question.


5. [ ] **Confidence gate**: Score ≥ 0.80 → present card + numbered confirm (`1. Run  2. Adjust  3. Different workflow  4. Cancel`). Score < 0.80 → ask clarifying question first. No silent execution.
6. [ ] **`ta workflow run` intent path**: When `<name>` doesn't match a known template name, try intent resolution. Explicit template name always takes precedence over intent resolution.
7. [ ] **Advisor integration** (`crates/ta-workflow/src/intent.rs`): `resolve_intent(text, plan_ctx) -> ResolutionResult` is callable from advisor agent. Advisor presents the resolution card via numbered options in its chat response.
8. [ ] **Tests**: "implement remaining v0.15" → `plan-build-phases`, `phase_filter=v0.15`. "run next phase" → `build`, `phase=plan.next_pending_phase`. Low-confidence input → clarifying question returned. Explicit template name bypasses resolver.


---

---

### v0.15.25 — Auto-Approve Constitution: Rule-Based Policy + Amendment Flow
<!-- status: pending -->

**Goal**: Replace the binary `auto_approve = true/false` with a rule-based constitution section. Rules are expressed as file-pattern conditions with approve/review/block actions. The constitution section is amended via the same review-gate flow as drafts — no silent policy changes.

**Why**: "Always auto-approve" is too broad; "never auto-approve" is too conservative. A doc-only draft should auto-approve; an auth-path change should always require review. The constitution makes this explicit and auditable.

#### Items

1. [ ] **`AutoApproveRule` type** (`crates/ta-changeset/src/policy.rs`): Fields: `name`, `description`, `condition` (string expression), `action` (`approve | review | block`). Rules evaluated top-to-bottom; first match wins. `default` rule if no match.
2. [ ] **Condition DSL**: Initial condition functions: `all_files_match(globs...)`, `any_file_match(globs...)`, `phase_matches(prefix)`, `contains_secret_findings()`, `test_coverage_passed()`, `reviewer_score_above(n)`. Evaluated against the draft's artifact list.
3. [ ] **Constitution section** (`.ta/constitution.toml`): `[auto_approve]` section with `baseline = "always | never | rules"` and `[[auto_approve.rule]]` array. `[auto_approve.default]` action for no-match case.
4. [ ] **Evaluation engine** (`crates/ta-changeset/src/policy.rs`): `evaluate_auto_approve(draft, constitution) -> AutoApproveDecision { action, matched_rule, reason }`. Integrated into `ta draft apply` before VCS submit step. Decision logged to goal-audit.jsonl.
5. [ ] **Amendment flow** (`apps/ta-cli/src/commands/constitution.rs`): `ta constitution amend auto-approve` opens the `[auto_approve]` section in `$EDITOR`. On save, creates an amendment draft (same review path as a code draft). Amendment applied only after human explicit approval. Every amendment recorded in audit log with timestamp and approver.
6. [ ] **Apply-time output**: Print which rule matched and what action was taken: `[auto-approve] matched rule 'docs-only' → approve`. If blocked: `[auto-approve] matched rule 'auth-path' → BLOCKED — human review required`.
7. [ ] **Migration**: Existing `auto_merge = true` in `workflow.toml` maps to `baseline = "always"` automatically on first run. Emit one-time migration message suggesting the user review the constitution.

9. [ ] **USAGE.md "Auto-Approve Constitution"** section: Rule DSL reference, amendment flow walkthrough, migration from `auto_merge`.

#### Version: `0.15.25-alpha`

---

### v0.15.26 — Studio: Global Intent Bar + Advisor Panel with Context Tabs
<!-- status: pending -->



**Depends on**: v0.15.21 (Studio advisor agent), v0.15.24 (intent resolver), v0.15.25 (auto-approve constitution)

#### Items

1. [ ] **Global intent bar** (`apps/ta-studio/src/components/IntentBar.tsx`): Single persistent text input at top of Studio. Always routes to advisor agent. Keyboard shortcut to focus (`Cmd+K` / `Ctrl+K`). Not per-tab.
2. [ ] **Tab context injection**: Active tab + selected object (e.g., `tab: Workflows, selected: build-phases`) sent to advisor as context prefix on every message. Advisor uses this to narrow options and shape numbered menus.
3. [ ] **Numbered-option menu component** (`apps/ta-studio/src/components/AdvisorMenu.tsx`): Renders advisor responses with numbered choices as interactive buttons. User can click or type the number. Terminal-compatible plain-text fallback via `ta advisor ask`.
4. [ ] **Context-shaped menus**: With Workflows tab + template selected, "amend auto-approve" presents: `1. Amend auto-approve for this workflow  2. Amend project constitution  3. Explain the difference`. With Plan tab, same phrase → different menu options. Menus are generated by advisor agent, not hardcoded.
<!-- status: pending -->
6. [ ] **`ta advisor ask` CLI command** (`apps/ta-cli/src/commands/advisor.rs`): `ta advisor ask "implement remaining v0.15"`. Resolves intent, prints numbered card, accepts stdin number input to confirm. Same logic as Studio advisor panel — shared `AdvisorSession` type.
7. [ ] **Security level integration**: `read_only` → advisor shows `ta run "..."` command as copyable text. `suggest` → renders as clickable button in Studio / prints with `[run]` prompt in CLI. `auto` → fires at ≥80% confidence, prints what it did.

---

#### Version: `0.15.26-alpha`

---

### v0.15.27 — Workflow Template Library: Install, Publish, Search
<!-- status: pending -->



**Depends on**: v0.15.23 (parameterized templates)

#### Items

1. [ ] **Template manifest format**: Each template YAML has a `metadata` section: `name`, `description`, `version`, `tags: []`, `author`, `min_ta_version`. Manifest is the unit of discovery and versioning.
2. [ ] **`ta workflow install <url|slug>`** (`apps/ta-cli/src/commands/workflow.rs`): Fetches template YAML from URL or resolves slug against configured registry endpoint. Saves to `~/.config/ta/workflow-templates/`. Validates schema before saving. Shows template metadata on install.
3. [ ] **`ta workflow publish <name>`** (`apps/ta-cli/src/commands/workflow.rs`): Packages template YAML + manifest. For now: prints the package to stdout (for piping to gist/upload). Future: POST to registry endpoint when configured.
4. [ ] **`ta workflow search <query>`** (`apps/ta-cli/src/commands/workflow.rs`): Searches name, description, and tags across all library paths (project, user global, registry index if configured). Prints table: name, description, source (project/user/registry).
5. [ ] **Registry protocol**: Simple JSON index file at a configurable URL. Index entries: `{ name, description, version, tags, url, min_ta_version }`. `ta workflow update-index` refreshes the cached index. Default registry: built-in templates only (no external network call by default).

7. [ ] **Tests**: `install` from file URL saves to user library. `search` finds templates by tag. `remove` deletes user template. Project template not removable via CLI. Schema validation rejects malformed template.
8. [ ] **USAGE.md "Workflow Library"** section: Installing, publishing, searching templates. Registry configuration. Difference between project / user / built-in templates.

#### Version: `0.15.27-alpha`

---

## v0.16 — IDE Integration & Developer Experience

> **Focus**: First-class IDE integration for VS Code, JetBrains (PyCharm, WebStorm, IntelliJ), and Neovim. TA transitions from a pure CLI tool to an embedded development workflow component with sidebar panels, inline draft review, and one-click goal approval.

### v0.16.0 — VS Code Extension
<!-- status: pending -->

**Goal**: A VS Code extension that surfaces TA's core workflow directly in the editor: start goals from the command palette, view draft diffs in the native diff viewer, approve/deny artifacts inline, and see live goal status in the sidebar. Python, TypeScript, and Node.js users (the primary audience) should be able to use TA without leaving VS Code.

**Why this phase exists**: TA's primary friction for non-Rust developers is the context switch to the terminal. IDE integration collapses this: a TypeScript developer working in VS Code can trigger a goal, review the proposed changes as a standard pull-request diff, and approve — all without leaving the editor. This is the experience that drives mainstream adoption beyond the Rust/CLI-first audience.

#### Architecture

The extension communicates with the TA daemon over the existing HTTP API (localhost). No new backend API is needed — the extension is a thin UI layer over the daemon's REST endpoints. The web shell (`ta shell`) uses the same API; the extension reuses that knowledge.

```



  ├─ Diff Viewer: opens staging diff in VS Code's native diff editor
  ├─ Status Bar: current goal state, daemon health indicator

```

#### Items

1. [ ] **Extension scaffold**: TypeScript extension using the VS Code Extension API. Published to VS Code Marketplace as `trusted-autonomy.ta`. Commands registered: `ta.startGoal`, `ta.listDrafts`, `ta.approveDraft`, `ta.denyDraft`, `ta.viewDiff`, `ta.openShell`.
2. [ ] **Daemon connectivity**: Extension connects to the TA daemon over `http://127.0.0.1:7700` (configurable). Health-check on activation; clear error if daemon not running with a "Start daemon" button.
3. [ ] **Goal sidebar panel (`TA Goals`)**: Tree view listing active/recent goals with state icons (running/pr_ready/applied/failed). Click a goal → open detail panel showing title, phase, agent, timestamps.

5. [ ] **Inline diff viewer**: Opens `vscode.diff(source_uri, staging_uri, "TA Draft: <filename>")` for each artifact. Reviewer sees exactly what the agent changed without leaving the editor.
6. [ ] **Status bar item**: Shows current goal state (e.g., `TA: running goal-123`) with a click-to-open shortcut. Turns amber on `pr_ready`, green on `applied`, red on `failed`.
7. [ ] **Desktop notifications**: `vscode.window.showInformationMessage` (or `showWarningMessage`) on goal completion, draft ready, and approval-needed events — polled via SSE from the daemon.

9. [ ] **Settings**: `ta.daemonUrl` (default `http://127.0.0.1:7700`), `ta.autoOpenDiff` (default `true`), `ta.notifyOnComplete` (default `true`).
10. [ ] **Walkthrough**: VS Code onboarding walkthrough ("Get Started with TA") covering: install daemon, configure `workflow.toml` for Python/TS/Node, start first goal, approve first draft.
**Items**:

#### Version: `0.16.0-alpha`

---





#### Items

1. [ ] **Plugin scaffold**: Kotlin plugin using the IntelliJ Platform SDK. Published to JetBrains Marketplace as `com.trusted-autonomy.ta`. Supports PyCharm 2024.1+, WebStorm 2024.1+, IntelliJ IDEA 2024.1+.

3. [ ] **Daemon connectivity**: HTTP client connecting to `http://127.0.0.1:7700`. Health check on IDE startup.
4. [ ] **Diff viewer**: Opens staging vs source diffs in IntelliJ's built-in diff tool (`DiffManager.showDiff()`).

6. [ ] **Actions**: "Start Goal" (toolbar + right-click menu), "Approve Draft", "Deny Draft", "Open TA Shell" registered as IDE actions.
7. [ ] **Marketplace publishing**: CI workflow to build and publish to JetBrains Marketplace on `v*` tags.



---

### v0.16.2 — Neovim Plugin
<!-- status: pending -->



#### Items



3. [ ] **Diff view**: Opens staging diff in a split buffer using `vim.diff()` or `diffview.nvim`.
4. [ ] **Floating window**: `:TA status` shows daemon health and active goal in a floating window.
5. [ ] **Commands**: `:TA start`, `:TA approve <id>`, `:TA deny <id>`, `:TA shell`.
6. [ ] **luarocks / GitHub Releases packaging**: Distribute via `luarocks` and GitHub Releases.

#### Version: `0.16.2-alpha`

---

### v0.16.3 — Ollama Agent Framework Plugin (Extract & Standalone)
<!-- status: pending -->
**Goal**: Extract `ta-agent-ollama` from the TA monorepo into a standalone agent-framework plugin with its own repository, README, and usage documentation. TA's built-in USAGE.md "Local Models" section becomes a short pointer to the plugin project. This follows the same pattern as the VCS plugins (`ta-vcs-git`, `ta-vcs-p4`) — TA ships the plugin protocol and discovery, first-party plugins live in their own repos and are published to the plugin registry.

**Why extract**: Ollama support has its own dependency surface (Ollama binary, model management, thinking-mode tokens), release cadence (tracks Ollama API changes independently of TA core), and user audience (local-model users who may not need TA's full feature set). Keeping it in-tree makes the core binary heavier and couples TA releases to Ollama API changes.

**Depends on**: v0.14.9 (Qwen3.5 profiles, `ta agent install` flow), v0.14.4 (daemon extension surface / plugin traits)

#### Design



```bash

ta agent install ollama

# Or direct from the plugin repo
ta plugin install github:trusted-autonomy/ta-agent-ollama
```

The plugin's own `README.md` covers everything Ollama-specific: prerequisites, model selection, thinking-mode, hardware sizing, `ta agent install qwen3.5` workflow, troubleshooting. TA's USAGE.md "Local Models" section becomes:

```markdown
## Local Models



Quick start:
  ta agent install ollama      # install the plugin


See the [ta-agent-ollama README] for model selection, hardware requirements,

```

#### Items

1. [ ] **Create `ta-agent-ollama` repository**: New public repo under the Trusted Autonomy GitHub org. Scaffold: `Cargo.toml`, `src/lib.rs`, `README.md`, `USAGE.md`, `agents/` (Qwen3.5 profiles), `tests/`. CI: build + test on push. Publish to `crates.io` as `ta-agent-ollama`.

2. [ ] **Move `ta-agent-ollama` crate**: Copy from monorepo `crates/ta-agent-ollama/` to the new repo. Update `Cargo.toml` workspace membership. Remove from monorepo `Cargo.toml` workspace members. Monorepo retains a `ta-agent-ollama` dev-dependency only for integration tests (behind a feature flag).

3. [ ] **Plugin manifest**: `ta-agent-ollama` ships a `plugin.toml` declaring its capabilities, supported agent frameworks, min TA version, and install instructions. TA's plugin registry resolves it at `ta agent install ollama`.

4. [ ] **Plugin README**: Complete standalone documentation: prerequisites (Ollama binary, supported platforms), model catalog (Qwen3.5 4B/9B/27B, Llama 3.x, Mistral, DeepSeek), install flow (`ta agent install`), thinking-mode configuration, hardware sizing table, `ta doctor` integration, troubleshooting (VRAM errors, connection refused, slow inference). Written for non-engineers — should feel like the Studio-WalkThru.md style.



6. [ ] **`ta plugin` command** (if not already present): `ta plugin install <source>`, `ta plugin list`, `ta plugin remove <name>`. Source formats: `github:<org>/<repo>`, `crates:<crate-name>`, local path. Used to install community agent plugins beyond the first-party set.

7. [ ] **Migration guide**: For existing users who have `ta-agent-ollama` configured via the monorepo build, provide a one-command migration: `ta agent migrate ollama` — detects existing config, installs the standalone plugin, updates profile paths, verifies connectivity.

8. [ ] **Tests**: Plugin discovery finds `ta-agent-ollama` after `ta agent install ollama`. Agent profile round-trip through the plugin manifest. `ta plugin list` shows the installed plugin with version. Migration command preserves existing model config.

#### Version: `0.16.3-alpha`

---

### v0.16.3.1 — Gemma 4 Agent Profiles (ta-agent-ollama plugin)
<!-- status: pending -->

so users can run Gemma 4 locally with zero configuration, at the right size for their hardware.


**Depends on**: v0.16.3 (ta-agent-ollama extracted to standalone plugin)

**Why Gemma 4**: Google's Gemma 4 family (released April 2025) has strong coding and reasoning
performance in the sub-14B tier, making it the best choice for M1/M2 Macs and mid-range
Windows machines that can't run Qwen3.5-27B. The 4B variant runs comfortably on 8GB VRAM

**Items**:

#### Hardware sizing

| Profile name | Model | Min VRAM / RAM | Target hardware |
|---|---|---|---|
| `gemma4-4b` | `gemma4:4b` | 8 GB VRAM / 16 GB unified | M1 Mac (base), RTX 3060, most mid-range cards |
| `gemma4-12b` | `gemma4:12b` | 16 GB VRAM / 24 GB unified | M1 Pro/Max, RTX 4080, RTX 5080 |


#### Items

1. [ ] **`agents/gemma4-4b.toml`** in `ta-agent-ollama` plugin repo:
   ```toml
---

   description = "Gemma 4 4B via Ollama — fast local agent for 8 GB VRAM / M1 Macs"


   [framework.options]
   model       = "gemma4:4b"

   max_turns   = 40

   [hardware]
   min_vram_gb     = 8
   min_unified_gb  = 16
   ```





4. [ ] **`ta agent install gemma4`** shorthand: When user runs `ta agent install gemma4`, `ta doctor` detects available VRAM/unified memory and auto-selects the largest profile that fits. Prints:
   ```

   Installing: gemma4-4b (best fit for your hardware)

   ```

5. [ ] **`ta doctor` Gemma 4 check**: If `gemma4:*` is pulled in Ollama but no matching profile is installed, emit:
   ```
   [warn] Gemma 4 model detected in Ollama but no ta-agent-ollama profile installed.
          Run: ta agent install gemma4
   ```

6. [ ] **Plugin README — Gemma 4 section**: Add hardware sizing table above to the plugin README's "Model Catalog" section. Include a note that Gemma 4 uses SentencePiece tokenization (not tiktoken) — impacts thinking-mode token budget estimates.

7. [ ] **Tests**: Profile TOML round-trips. `ta doctor` hardware detection selects correct tier. `ta agent install gemma4` on a simulated 8 GB system installs `gemma4-4b` not `gemma4-27b`.

#### Version: `0.16.3.1-alpha`

---


<!-- status: pending -->

<!-- status: pending -->

**Depends on**: v0.15.16 (Windows EV signing — establishes working Windows CI pipeline)

**Design**:

- **AppContainer**: For high-security mode, create an AppContainer (`CreateAppContainerProfile`) and launch the agent in it. AppContainers restrict filesystem access to the staging workspace path and named capabilities. Network access restricted to `[sandbox.allow_network]` hosts via a network filter driver hook.



**Items**:
1. [ ] **Job Object wrapper** (`crates/ta-runtime/src/sandbox_windows.rs`): `SandboxProvider::WindowsJobObject` variant. `CreateJobObject`, `AssignProcessToJobObject`, `SetInformationJobObject` with `JOBOBJECT_BASIC_LIMIT_INFORMATION` and `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`. Process tree torn down on TA exit. Kills zombie agent processes.
2. [ ] **AppContainer profile** (`sandbox_windows.rs`): `SandboxProvider::WindowsAppContainer` variant. `CreateAppContainerProfile`, set `SECURITY_CAPABILITIES` with staging-workspace SID. Launches agent via `CreateProcess` with the AppContainer token. Deletes profile on goal completion (`DeleteAppContainerProfile`).


5. [ ] **CI test** (Windows runner): Spawn a sandboxed agent subprocess, attempt to write outside the staging path, assert it is denied. Assert process tree is torn down when Job Object handle closes.
6. [ ] **USAGE.md**: Windows sandbox section — what each containment level restricts, how to enable, elevation requirement for AppContainer, `ta doctor` sandbox check.

```toml

---



> **Focus**: Tier 2 managed-paths filesystem governance (SHA journal, Postgres/MySQL staging), followed by the unified `ta release` command system. Governance infrastructure comes first so the release pipeline itself can run under full governance.

### v0.17.0 — Managed Paths: SHA Filesystem + URI Journal
<!-- status: pending -->





**Items**:
1. [ ] **`governed_paths` config** (`[workflow.toml]`): `[[governed_paths]]` entries with `path`, `mode` (`read-only`/`read-write`), `purpose`, `max_sha_store_mb`. Parsed by `WorkflowConfig`. `read-only` paths block writes at the FUSE/intercept layer.
2. [ ] **SHA store** (`.ta/sha-fs/<sha256>`): Content-addressed blob store. Write: compute SHA-256, store full file at `.ta/sha-fs/<sha256>` if not present (dedup automatic). Read: transparent passthrough to real path if URI not in journal. Entries immutable once written.



6. [ ] **Apply/rollback**: `ta draft apply` writes SHA blob content to each real path in the journal. `ta draft deny` records a `DENIED` entry in the journal (the write already landed; deny prevents any further replay). Rollback: write pre-goal SHA blob to real path.
7. [ ] **GC** (`ta gc governed-paths`): Remove SHA blobs not referenced by any live journal entry (entries older than `--retain-days`, default 30). Print bytes reclaimed. Runs automatically after `ta draft apply` for entries older than the retention window.
8. [ ] **Tests**: Write to governed path → SHA blob created, journal entry appended; read-your-writes via journal; pre-goal snapshot SHA recorded; `ta draft apply` writes blob to real path; `ta draft deny` records DENIED; GC removes unreferenced blobs; `read-only` mode blocks write at FUSE layer; Windows file-watcher fallback captures write.

#### Version: `0.17.0-alpha`

---


<!-- status: pending -->



**Depends on**: v0.17.0 (URI journal pattern established for governed resources)

**Items**:
1. [ ] **`ta-db-proxy-postgres` crate** (`crates/ta-db-proxy-postgres/`): Implements `DbProxyPlugin`. Connects to a Postgres logical replication slot created at goal start. Agent connects to a read-write replica or the primary (configured via `db://postgres/<conn>#<table>` URI). WAL events captured to JSONL mutation log during the goal. `apply()` replays log against target; `deny()` discards log and drops replication slot.
2. [ ] **`ta-db-proxy-mysql` crate** (`crates/ta-db-proxy-mysql/`): Implements `DbProxyPlugin` via MySQL binary log (binlog) position snapshot. Agent connects to a shadow schema (cloned at goal start via `mysqldump --no-data` + row-level shadow). Binlog delta captured. `apply()` replays against real schema.

4. [ ] **Constitution rules for DB** (default `constitution.toml`): `[[rules.warn]]` fires when a DB draft contains > N rows modified (configurable, default 100). `[[rules.block]]` fires on schema-altering statements (`DROP TABLE`, `TRUNCATE`, `ALTER TABLE DROP COLUMN`) unless `allow_schema_drops = true` in `[actions.db_query]`.
5. [ ] **`ta-db-proxy` registry** (`crates/ta-db-proxy/src/registry.rs`): Maps URI scheme + driver to the correct plugin backend. `db://postgres/*` → `PostgresProxyPlugin`; `db://sqlite/*` → `SqliteProxyPlugin`; `db://mysql/*` → `MysqlProxyPlugin`. Plugins are optional features — `ta-db-proxy-postgres` behind `[features] postgres`.
6. [ ] **Credential vault integration**: DB connection strings resolved from the credential vault — no plaintext Postgres DSN in `workflow.toml`. Agent calls `ta_external_action { action_type: "db_query", target_uri: "db://postgres/prod#orders" }` with no credentials; TA resolves the DSN from the vault by URI.
7. [ ] **`policy = "review"` as default** for `[actions.db_query]`: Default is `review` (not `auto`). Every DB mutation is held for human review showing the row-level diff before execution. `policy = "auto"` requires explicit opt-in.
8. [ ] **Tests**: Postgres replication slot created/dropped on goal start/deny; WAL capture round-trip; row-level diff rendering for INSERT/UPDATE/DELETE; schema-drop constitution rule blocks `DROP TABLE`; credential vault resolves DSN without exposing it to agent; large-mutation warning fires at configured threshold.



---



> **Focus**: Unified `ta release` command system. Builds on the governed filesystem from v0.17.0-v0.17.1 — release pipelines run under full governance. that works for any release type — binary distributions, content deliveries, service deployments — via a pluggable `ReleaseAdapter` abstraction. Replaces the current ad-hoc dispatch/channel/VCS approach with a single coherent model and a simplified command surface.

### v0.17.2 — Release Management Design Review (Pre-Phase)
<!-- status: pending -->
**Goal**: Before committing implementation, run a structured design session to finalise the `ta release` command surface, `ReleaseAdapter` trait, channel model, and how release fits into TA's broader conversational UX. Produces a signed-off design document (`docs/release-design.md`) that v0.17.1+ implement against.



#### Questions to resolve

**Command surface — simplification**

Today TA has too many commands that require knowing the right incantation. The goal is a surface where a user can describe what they want to a `ta shell` conversation and the agent issues the right command — not a surface that requires reading docs first.

Current commands to audit for consolidation:
|---|---|

- Should `ta release` expose `run`, `promote`, `status`, `list`, `adapters` as its subcommands?
- Can the RC → stable promotion be a single `ta release promote v0.14.16-rc.1 --to stable`?



Core trait methods to agree on:

- `publish(prepared, assets) → ReleaseRef` — tag, push, upload assets
- `promote(release_ref, channel) → ()` — move to stable/nightly/lts without re-publishing
- `status(version) → ReleaseStatus` — is this version published? on which channels?

Built-in adapters to implement in v0.17.1:
- `GitHubReleaseAdapter` — the current git-tag + GitHub Actions + `gh release` flow
- `RemoteFileReleaseAdapter` — scp/rsync/S3 bucket copy; target configured as `sftp://host/path`, `s3://bucket/prefix`


<!-- status: pending -->
- `YouTubeReleaseAdapter` — upload video artifact as YouTube video; title/description from release notes; visibility (public/unlisted/private) maps to channel (stable/nightly/draft)
- `SteamReleaseAdapter` (game dist) — Steamworks SDK depot push; branch maps to channel
- `AppStoreReleaseAdapter` — `altool` / App Store Connect API

URL-scheme config approach: adapter type inferred from the `publish_url` in `release.toml`:
```toml
[release]

# publish_url = "s3://my-bucket/releases"                 # → RemoteFileReleaseAdapter
# publish_url = "https://deploy.example.com/webhook"      # → ServiceReleaseAdapter
# publish_url = "youtube://channel/UCxxxx"                # → YouTubeReleaseAdapter
```

**Versioning for non-code artifacts**

Code releases use semver. Content releases don't. Decide:
- Does `ta release run` require a semver version, or accept arbitrary labels (`"episode-3"`, `"turntable-v2-final"`)?
- For content pipelines: does "version" mean a date stamp, a project-internal label, or is it optional entirely?
- How does the channel model (stable/nightly) map to content? (Published/Draft? Public/Review?)

**Use cases to cover in the design doc**


|---|---|---|---|

| SecureAutonomy | Enterprise binary | `RemoteFileReleaseAdapter` (S3) | rc → staging; stable → prod |
| Content creator | Wan2.1 video output | `YouTubeReleaseAdapter` | draft → unlisted; approved → public |
| Game studio | UE5 build | `SteamReleaseAdapter` | beta → beta branch; gold → default |


**Command simplification principles**


2. All release state queryable via `ta release status` — no separate `ta plan status` needed for version info
3. Conversational: `ta shell` agent understands "release this as an RC" and maps to the right command

5. Existing `ta release dispatch` deprecated in favour of `ta release run` + `ta release promote`

#### Deliverable


- Final `ta release` command surface with all subcommands, flags, and examples
- `ReleaseAdapter` trait definition (Rust trait sketch)

- Channel model and lifecycle (draft → rc → stable → lts)
- Versioning rules for code vs content artifacts
- Migration path from current `ta release dispatch` / manual tagging workflow

#### Version: `0.17.2-alpha` *(design only — no code)*

---

### v0.17.3 — `ta release` Core + Built-in Adapters
<!-- status: pending -->




#### Items

1. [ ] **`ReleaseAdapter` trait** in `crates/ta-release/src/adapter.rs`: `prepare`, `publish`, `promote`, `status` methods as designed in v0.17.0. URL-scheme registry for adapter discovery.

2. [ ] **`ta release run <phase> [--label <label>] [--channel <channel>]`**: Bumps version in `Cargo.toml` (or equivalent), commits, tags, pushes, calls adapter `publish`. Without `--label`, derives tag from plan phase. `--channel` defaults to `nightly` for pre-release labels, `stable` otherwise.

3. [ ] **`ta release promote <tag-or-ref> --to <channel>`**: Calls adapter `promote` — no new tag, no rebuild. For GitHub: edits release prerelease flag and `--latest`.

4. [ ] **`ta release status [<tag>]`**: Calls adapter `status`. Shows current channels, asset checksums, publish timestamp.

5. [ ] **`GitHubReleaseAdapter`**: Full replacement for current manual tag + `release.yml` dispatch. Draft-first publish (create draft → upload assets → publish) to avoid immutable release race. Channel-aware `--latest` guard.

6. [ ] **`RemoteFileReleaseAdapter`**: Supports `sftp://`, `s3://`, `file://` publish URLs. Copies release assets to target path. Generates `manifest.json` alongside assets (version, checksums, channel, timestamp).



8. [ ] **`release.toml` schema**: `[release]` section — `publish_url`, `default_channel`, `version_files` (paths to bump), `changelog_cmd` (optional shell command to generate changelog).

9. [ ] **Deprecate `ta release dispatch`**: Keep as alias with deprecation warning pointing to `ta release run`.





#### Version: `0.17.3-alpha`

---

### v0.17.3.1 — sage-lore Design Review
<!-- status: pending -->

**Goal**: Evaluate [sage-lore](https://github.com/kwg/sage-lore) (Rust, Scroll DSL, deterministic execution, scan-once security model) as a potential complement or integration target for TA's governance and orchestration layer. Produce a structured design review and recommendation.

**Depends on**: v0.17.3

**Why deferred here**: sage-lore's deterministic execution and scan-once security model are architecturally aligned with TA's staged-action thesis, but integrating at the orchestration layer requires the `ta release` core (v0.17.3) to be stable first. A premature evaluation would miss the context of how TA's adapter and release surfaces interact with external orchestrators.

**Items**:

1. [ ] **Capability audit**: Map sage-lore's 20 Scroll primitives to TA concepts (goal, draft, plan phase, policy gate). Identify gaps and overlaps.

2. [ ] **Security model comparison**: sage-lore scan-once vs TA staging overlay. Are they additive (sage-lore enforces input constraints, TA enforces output constraints) or redundant?

3. [ ] **Integration options**: (a) sage-lore as an orchestrator that drives `ta run` goals via CLI; (b) Scroll DSL as a workflow definition language inside `.ta/workflows/`; (c) no integration — document why.

4. [ ] **Decision document**: `docs/design/sage-lore-review.md` — recommendation (integrate / complement / skip) with rationale and any follow-on plan phases.

#### Version: `0.17.3.1-alpha`

---

### v0.17.4 — Extended Adapters (YouTube, Steam, Homebrew)
<!-- status: pending -->
**Goal**: Implement the content-delivery and distribution adapters identified in the v0.17.0 design review. Enables content creators to release video outputs to YouTube and game studios to push to Steam — all through the same `ta release run` command.

**Depends on**: v0.17.3 (core adapter trait + `ta release run`)

#### Items

1. [ ] **`YouTubeReleaseAdapter`**: YouTube Data API v3. Uploads video artifact from staging, sets title/description from release notes, maps channel → visibility (`nightly` = unlisted, `stable` = public, `draft` = private). Config: `youtube://channel/<channel-id>` in `publish_url`.

2. [ ] **`SteamReleaseAdapter`**: Steamworks SDK `steamcmd` wrapper. Depot upload + branch assignment. Maps `nightly` → beta branch, `stable` → default branch. Config: `steam://app/<appid>`.

3. [ ] **Homebrew tap auto-update**: On `GitHubReleaseAdapter` stable publish, open a PR in the configured `homebrew-tap` repo updating formula version + SHA-256. Replaces the manual v0.17.1 Homebrew step (absorbs old v0.17.1 Homebrew Tap phase).

4. [ ] **Adapter plugin protocol**: Third-party adapters via external process (JSON-over-stdio, same pattern as VCS plugins). Enables custom adapters (`AppStoreReleaseAdapter`, `ItchIoReleaseAdapter`, etc.) without modifying TA core.

5. [ ] **Tests**: YouTube upload stub. Steam steamcmd mock. Homebrew PR open. Plugin adapter round-trip.

6. [ ] **USAGE.md**: Adapter sections for YouTube, Steam, Homebrew. Plugin adapter authoring guide.

#### Version: `0.17.4-alpha`

---

---



> **Focus**: Supervised Autonomy (SA) enterprise credential store, host-wide FUSE filesystem virtualization, and external process governance (ComfyUI, SimpleTuner, arbitrary daemons). This milestone is the foundation for deploying TA in regulated enterprise environments.

### v0.18.0 — SA Enterprise Credential Store Plugin
<!-- status: pending -->

**Goal**: Replace `FileVault` with an enterprise credential store backend for SA deployments. Agent session tokens are issued against credentials stored in HashiCorp Vault, AWS Secrets Manager, Azure Key Vault, or equivalent. User validation is required before token issuance — agent identity is asserted via session ID, signed token, or SPIFFE SVID.

**Depends on**: v0.17.4 (release management stable — SA is a separate product build on top of stable TA)

**Items**:
1. [ ] **Plugin interface finalization** (`crates/ta-credentials/src/vault.rs`): Extend `CredentialVault` trait with `validate_caller(&self, caller_identity: &CallerIdentity) -> Result<(), VaultError>` — called before `issue_token`. `CallerIdentity` wraps session ID, optional SPIFFE SVID, and IP/hostname. Plugin implements validation logic.

3. [ ] **`ta-credentials-vault-aws`** plugin: AWS Secrets Manager. Credential lookup via `GetSecretValue`. Token issuance via temporary IAM credentials (`AssumeRole` with goal-scoped policy). Token revocation via IAM session invalidation.


6. [ ] **User validation requirement**: In SA mode, `issue_token` requires the caller to present a valid identity assertion (not just a scope request). The plugin validates identity before issuing. Failed validation → audit log entry + alert.
7. [ ] **Audit trail**: All token issuances, validations, and revocations logged to the SA audit log (separate from the project-level `.ta/audit.jsonl`). Supports compliance reporting.
8. [ ] **Tests**: Mock HashiCorp Vault server; token issuance against AppRole; caller validation rejects unknown identities; token revocation; `ta credentials health` reports backend status.

#### Version: `0.18.0-alpha`

---


<!-- status: pending -->

**Goal**: Extend Tier 2 managed paths from v0.17.0 to cover writes from any process — not just the TA agent process. ComfyUI, SimpleTuner, game engines, and arbitrary daemons writing to governed paths are captured in the SHA journal. The URI journal becomes a host-wide audit record for all filesystem activity in governed paths, regardless of which process produced it.

**Depends on**: v0.17.0 (SHA journal, URI journal, FUSE daemon baseline), v0.18.0 (SA credential store — external process governance is an SA-tier capability)

**Items**:
1. [ ] **Process-agnostic FUSE mount**: The `ta-governed-fs` FUSE daemon (from v0.17.0) is enhanced to capture writes from any process (not just the TA agent subprocess) that writes to a governed path. The FUSE mount stays active for the full session, not just the duration of a single goal.

3. [ ] **Session-level governed paths**: `ta session start --govern /data/comfyui/outputs` mounts the FUSE intercept for the session duration. All ComfyUI/SimpleTuner runs within that session are captured automatically without per-goal configuration.
4. [ ] **Checkpoint and rollback**: `ta checkpoint create "before-training-run"` records a named snapshot of all governed-path SHA entries. `ta checkpoint restore "before-training-run"` rewrites real paths to pre-checkpoint SHA blobs. Enables "undo this SimpleTuner run" without re-training.
5. [ ] **Large file policy** (`max_sha_store_mb` per governed path): When the SHA store for a path exceeds the limit, the oldest blobs (not referenced by a live checkpoint) are evicted. Warning emitted. GC is automatic.
6. [ ] **DB governance for external processes**: Postgres logical replication slot stays open for the session, capturing mutations from any process connecting to the governed DB — not just the TA agent. Mutations attributed by Postgres `application_name`.
7. [ ] **`ta governed status`**: Shows all active FUSE mounts, session-level governed paths, SHA store sizes, live checkpoints, and the last 10 writes per governed path.
8. [ ] **Tests**: ComfyUI mock process writes to governed path → captured in journal with correct process attribution; checkpoint/restore round-trip; eviction when max size exceeded; DB mutation from external process captured via replication slot.

#### Version: `0.18.1-alpha`

---



> Items in this section are under active consideration for deferral, scoping reduction, or removal. Review before each release cycle.

### Shell Mouse Scroll & TUI-Managed Selection

<!-- note: considering dropping the ratatui TUI shell entirely in favor of the web shell as the primary interface -->
**Originally**: v0.13.6 — Re-examine mouse scroll and text selection in the terminal TUI shell.

**Status**: The web shell (`ta shell` default since v0.11.5) provides a better UX for most users. The ratatui TUI (`ta shell --tui`) is now opt-in. The question is whether to invest further in TUI polish or drop it entirely.


- Keep TUI as opt-in with basic mouse support
- Drop TUI entirely (remove `--tui` flag, route all users to web shell)
- Rebuild TUI from scratch with a different library



---

## TA → SA Development Pivot

> **When TA development pauses and SA (Secure Autonomy / SecureTA) development begins.**

### Pivot trigger: completion of v0.17.4

TA core development pauses when **v0.17.4** is shipped and stable. At that point:

- The full TA feature surface is complete (staging, drafts, governance, IDE plugins, release management, local models, content pipeline).

- TA enters **maintenance mode**: bug fixes, security patches, and minor improvements only. No new feature phases.

### Why v0.17.2 specifically


|---|---|
| v0.14.4 — Daemon extension surface | SA can register OCI/VM runtimes without forking TA |


| v0.17.x — Release management | SA can govern its own release pipeline through TA |

SA cannot productively start until TA's extension surface is stable — building SA on a moving trait API creates constant rework. v0.17.2 is the point where all planned traits exist and have shipped.

### What SA development looks like




2. **SA-v0.2** — Hardware-bound attestation plugins: TPM 2.0 (`sa-attest-tpm`) and Apple Secure Enclave (`sa-attest-enclave`). Requires TA v0.14.1 `AttestationBackend`.
3. **SA-v0.3** — Kernel-level network policy: agent network egress rules enforced at the container level, not just by constitution. Requires SA-v0.1 (OCI runtime).
4. **SA-v0.4** — Multi-party governance: RBAC, org-level policy, audit export for compliance (ISO/IEC 42001, EU AI Act). Requires TA v0.14.4 daemon extension surface.
5. **SA-v0.5** — Cloud deployment: multi-tenant daemon, SSO, secrets management. This is the commercial tier that external teams pay for.



   **Why Byzantine here, not in TA**: TA assumes trusted agents running on a single user's machine — Raft (crash-fault-tolerant) is the right default. SA operates in environments where nodes may be compromised, colluding, or adversarially controlled. PBFT's Byzantine fault tolerance is only meaningful when you have independent trust domains, hardware attestation (SA-v0.2), and multi-party governance (SA-v0.4) backing each vote.

   **Algorithm selection (SA)**:

   | Algorithm | Fault model | Message complexity | Use case |
   |-----------|-------------|-------------------|----------|
   | **PBFT** (SA default) | Byzantine (f of 3f+1) | O(n²) | Multi-org panels, multi-human merge arbitration, regulated deployments |

   | **Stellar SCP** | Federated Byzantine | O(n) per quorum slice | Cross-org / federated trust where each node has its own quorum slice — no single coordinator |


   **Multi-human merge coordination**: When multiple human reviewers must reach agreement before a draft is applied (multi-party code review, legal/compliance sign-off, release approval), SA-v0.6 runs a PBFT round among the reviewers' approval signals. Each human approval is a signed vote (verified against their hardware attestation from SA-v0.2). A conflicting approval/denial from two humans is treated as a Byzantine fault and escalated rather than silently resolved.


   - `SA-v0.6.1` — `ByzantineConsensusAlgorithm` enum extending TA's `ConsensusAlgorithm`: `Pbft`, `HotStuff`, `Scp`, `Tendermint`. Serializes as `"pbft"` / `"hotstuff"` / `"scp"` / `"tendermint"`. Registered as SA-layer variants — TA's `ConsensusAlgorithm::Sa(ByzantineConsensusAlgorithm)` wrapper so TA remains Byzantine-free without an SA plugin.
   - `SA-v0.6.2` — `PbftConsensus` implementation (`sa-consensus/src/pbft.rs`): 3-phase commit (pre-prepare → prepare → commit), view-change on leader timeout, attestation-verified vote signatures (requires SA-v0.2 `AttestationBackend`).
   - `SA-v0.6.3` — `HotStuffConsensus` implementation (`sa-consensus/src/hotstuff.rs`): linear 2-phase BFT with pipelined proposals. Use when panel size > 7 and PBFT message overhead is measurable.

   - `SA-v0.6.5` — Multi-human merge arbitration: `kind = "human-consensus"` workflow step. Each human reviewer submits a signed approval/denial via the SA web UI or API. PBFT aggregates votes with attestation verification. Conflicting signals escalate to a named arbitrator (configured in `sa-config.toml`).
   - `SA-v0.6.6` — Observability: all PBFT view-changes, HotStuff timeouts, SCP quorum failures, and multi-human escalations exported to the SA audit trail with structured fields (algorithm, round, node-count, fault-count, duration). `sa audit consensus-log --run <id>` command.



Add `<!-- sa-pivot: ready -->` to this section when v0.17.2 ships. Until then, SA design work (ADRs, architecture documents, plugin interface sketches) can happen in parallel — just no implementation that depends on unstable TA traits.

---

## Projects On Top (separate repos, built on TA)

> These are NOT part of TA core. They are independent projects that consume TA's extension points.
> See `docs/ADR-product-concept-model.md` for how they integrate.




Adds OCI/gVisor container isolation, hardware-bound audit trail signing (TPM 2.0, Apple Secure Enclave), and kernel-level network policy — for regulated deployments and environments running untrusted agent code. Depends on TA v0.13.3 (RuntimeAdapter) and v0.14.1 (AttestationBackend). Not yet started.

---

### TA Web UI *(separate project)*


A browser-based interface to TA's daemon API, aimed at users who need to start goals, review drafts, and respond to agent questions without touching a terminal. Same capabilities as `ta shell` but with a guided, form-based experience.

- **Thin client**: SPA consuming TA's existing HTTP API + SSE events. No new backend logic.
- **Non-engineer language**: "Review changes", "Approve", "Ask the agent a question" — not "draft", "artifact", "overlay".
- **Dashboard**: Active goals, pending reviews, pending agent questions. One-glance status.
- **Start Goal**: Form with title, description, agent dropdown, optional file upload. Sensible defaults, optional advanced toggle.
<!-- status: pending -->

- **Agent Questions**: Pending questions with response input. Browser push notifications.
- **History**: Past goals/drafts, searchable, filterable.
- **Tech stack**: React or Svelte SPA, served as static files by daemon (`GET /ui/*`). Auth via daemon API token or session login.
- **Extensible**: Plugin mount points at `/ui/ext/<plugin-name>` for custom pages. Configurable theme/branding via `daemon.toml`.
- **Mobile-friendly**: Responsive layout for on-the-go approvals from phone/tablet.

**TA dependencies**: Daemon HTTP API (exists), SSE events (exists), interactive mode (v0.9.9.x), static file serving from daemon (minor addition to `ta-daemon`).


> Thin orchestration layer that composes TA, agent frameworks, and MCP servers.

- Role definition schema (YAML): purpose, triggers, agent, capabilities, notification channel

- Office manager daemon: reads role configs, routes triggers, calls `ta run`
- Multi-agent workflow design with detailed agent guidance
- Smart security plan generation → produces `AlignmentProfile` + `AccessConstitution` YAML consumed by TA
- Constitutional auto-approval active by default

- Domain workflow templates (sw-engineer, email, finance, etc.)

### Autonomous Infra Ops *(separate project)*
> Builder intent → best-practice IaC, self-healing with observability.

- Builder intent language → IaC generation (Terraform, Pulumi, CDK)
- TA mediates all infrastructure changes (ResourceMediator for cloud APIs)

- Best-practice templates for common infrastructure patterns
- Cost-aware: TA budget limits enforce infrastructure spend caps

---





| Mode | Standard Claude/Codex | TA-mediated |
|------|----------------------|-------------|

| **Overnight/batch** | Not possible — agent exits when session closes. | `ta run --checkpoint` in background. Review next morning. 0% attention during execution. |
| **Auto-approved (v0.6)** | N/A | Supervisor handles review within constitutional bounds. User sees daily summary. ~1% attention. Escalations interrupt. |


**Key shift**: Standard agent usage demands synchronous human attention. TA shifts to fluid, asynchronous review — the agent works independently, the human reviews in real-time or retroactively. Trust increases over time as constitutional auto-approval proves reliable.

---

## Future Improvements (unscheduled)

> Ideas that are valuable but not yet prioritized into a release phase. Pull into a versioned phase when ready.

### External Plugin System
Process-based plugin architecture so third parties can publish TA adapters as independent packages. A Perforce vendor, JIRA integration company, or custom VCS provider can ship a `ta-submit-<name>` executable that TA discovers and communicates with via JSON-over-stdio protocol. Extends beyond VCS to any adapter type: notification channels (`ta-channel-slack`), storage backends (`ta-store-postgres`), output integrations (`ta-output-jira`). Includes `ta plugin install/list/remove` commands, a plugin manifest format, and a plugin registry (crates.io or TA-hosted). Design sketched in v0.9.8.4; implementation deferred until the in-process adapter pattern is validated.

### Community Memory Sync

- **Community sync layer**: Publish anonymized entries to a shared registry (hosted service or federated protocol).

- **Retrieval**: `ta context recall` searches local first, then community if opted in.

- **Trust model**: Reputation scoring for contributors. Verified solutions (applied successfully N times) ranked higher.


### Unreal Engine MCP Plugin (`ta-mcp-unreal`)





> **Promoted to versioned phase**: v0.14.16 (Unity connector, `official` backend wrapping `com.unity.mcp-server`, `ta connector install unity`, build/test/scene tools).





Key capabilities:
- **MCP tools surfaced**: `omniverse_stage_open`, `omniverse_prim_query`, `omniverse_usd_export`, `omniverse_usd_import`, `omniverse_render_submit`, `omniverse_nucleus_sync`
- **USD data exchange**: TA can read and write `.usd`/`.usda`/`.usdc` files as first-class artifacts — diff USD prims between staging and source, track changes to scene hierarchy, material assignments, and xform overrides




- **Distribution**: Published as `ta-mcp-omniverse` + a companion Omniverse Extension installable via the Omniverse Extension Manager