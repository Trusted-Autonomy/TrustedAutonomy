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

## Phase 0 — Repo Layout & Core Data Model
<!-- status: done -->
Workspace structure with 12 crates under `crates/` and `apps/`. Resource URIs (`fs://workspace/<path>`, `gmail://`, etc.), ChangeSet as universal staged mutation, capability manifests, PR package schema.

## Phase 1 — Kernel: Audit, Policy, Changeset, Workspace
<!-- status: done -->
- `ta-audit` (13 tests): Append-only JSONL log with SHA-256 hash chain
- `ta-policy` (16 tests): Default-deny capability engine with glob pattern matching on URIs
- `ta-changeset` (14 tests): ChangeSet + PRPackage data model aligned with schema/pr_package.schema.json
- `ta-workspace` (29 tests): StagingWorkspace + OverlayWorkspace + ExcludePatterns + ChangeStore + JsonFileStore

## Phase 2 — MCP Gateway, Goal Lifecycle, CLI
<!-- status: done -->
- `ta-connector-fs` (11+1 tests): FsConnector bridging MCP to staging
- `ta-goal` (20 tests): GoalRun lifecycle state machine + event dispatch
- `ta-mcp-gateway` (15 tests): Real MCP server using rmcp 0.14 with 9 tools
- `ta-daemon`: MCP server binary (stdio transport, tokio async)
- `ta-cli` (15+1 tests): goal start/list/status/delete, pr build/list/view/approve/deny/apply, run, audit, adapter, serve

## Phase 3 — Transparent Overlay Mediation
<!-- status: done -->
- OverlayWorkspace: full copy of source to staging (.ta/ excluded)
- ExcludePatterns (V1 TEMPORARY): .taignore or defaults (target/, node_modules/, etc.)
- Flow: `ta goal start` → copy source → agent works in staging → `ta pr build` → diff → PRPackage → approve → apply
- CLAUDE.md injection: `ta run` prepends TA context, saves backup, restores before diff
- AgentLaunchConfig: per-agent configs with settings injection (replaces --dangerously-skip-permissions)
- Settings injection: `.claude/settings.local.json` with allow/deny lists + community `.ta-forbidden-tools` deny file
- Git integration: `ta pr apply --git-commit` runs git add + commit after applying
- Dogfooding validated: 1.6MB staging copy with exclude patterns

## Phase 4a — Agent Prompt Enhancement
<!-- status: done -->
- CLAUDE.md injection includes instructions for `.ta/change_summary.json`
- Agent writes per-file rationale + dependency info (depends_on, depended_by, independent)
- Foundation for selective approval (Phase 4c)
- **v0.2.4 update**: Added `what` field (per-target "what I did" description) alongside existing `why` (motivation). `what` populates `explanation_tiers.summary`; `why` populates `explanation_tiers.explanation`. Backward compatible — old summaries with only `why` still work via `rationale` field.

## Phase 4a.1 — Plan Tracking & Lifecycle
<!-- status: done -->
- Canonical PLAN.md with machine-parseable status markers
- GoalRun.plan_phase links goals to plan phases
- `ta plan list/status` CLI commands
- CLAUDE.md injection includes plan progress context
- `ta pr apply` auto-updates PLAN.md when phase completes

## Phase 4b — Per-Artifact Review Model
<!-- status: done -->
- [x] ArtifactDisposition enum: Pending / Approved / Rejected / Discuss (per artifact, not per package)
- [x] ChangeDependency struct for agent-reported inter-file dependencies
- [x] URI-aware pattern matching: scheme-scoped glob (fs:// patterns can't match gmail:// URIs)
- [x] Bare patterns auto-prefix with `fs://workspace/` for convenience; `*` respects `/`, `**` for deep
- [x] `ta pr build` reads `.ta/change_summary.json` into PRPackage (rationale, dependencies, summary)
- [x] `ta pr view` displays per-artifact rationale and dependencies

## Phase 4c — Selective Review CLI
<!-- status: done -->
- `ta pr apply <id> --approve "src/**" --reject "*.test.rs" --discuss "config/*"`
- Special values: `all` (everything), `rest` (everything not explicitly listed)
- Selective apply: only copies approved artifacts; tracks partial application state
- Coupled-change warnings: reject B also requires rejecting A if dependent

## Phase v0.1 — Public Preview & Call for Feedback
<!-- status: deferred -->
**Goal**: Get TA in front of early adopters for feedback. Not production-ready — explicitly disclaimed.

### Required for v0.1
- [x] **Version info**: `ta --version` shows `0.1.0-alpha (git-hash date)`, build.rs embeds git metadata
- **Simple install**: `cargo install ta-cli` or single binary download (cross-compile for macOS/Linux)
- [x] **Agent launch configs as YAML**: YAML files in `agents/` (claude-code.yaml, codex.yaml, claude-flow.yaml, generic.yaml). Config search: `.ta/agents/` (project) → `~/.config/ta/agents/` (user) → shipped defaults → hard-coded fallback. Schema: command, args_template (`{prompt}`), injects_context_file, injects_settings, pre_launch, env. Added `serde_yaml` dep, 2 tests.
- **Agent setup guides**: Step-by-step for Claude Code, Claude Flow (when available), Codex/similar
- **README rewrite**: Quick-start in <5 minutes, architecture overview, what works / what doesn't
- **`ta adapter install claude-code`** works end-to-end (already partially implemented)
- **Smoke-tested happy path**: `ta run "task" --source .` → review → approve → apply works reliably
- **Error messages**: Graceful failures with actionable guidance (not panics or cryptic errors)
- **.taignore defaults** cover common project types (Rust, Node, Python, Go)

### Disclaimers to include (added to README)
- "Alpha — not production-ready. Do not use for critical/irreversible operations"
- "The security model is not yet audited. Do not trust it with secrets or sensitive data"
- ~~"Selective approval (Phase 4b-4c) is not yet implemented — review is all-or-nothing"~~ — DONE (Phase 4b-4c complete)
- "No sandbox isolation yet — agent runs with your permissions in a staging copy"
- "No conflict detection yet — editing source files while a TA session is active may lose changes on apply (git protects committed work)"

### Nice-to-have for v0.1
- `ta pr view --file` accepts **comma-separated list** to review select files (e.g., `--file src/main.rs,src/lib.rs`)
- `ta pr view` shows colored diffs in terminal
- Basic telemetry opt-in (anonymous usage stats for prioritization)
- GitHub repo with issues template for feedback
- Short demo video / animated GIF in README
- **Git workflow config** (`.ta/workflow.toml`): branch naming, auto-PR on apply — see Phase v0.2

### What feedback to solicit
- "Does the staging → PR → review → apply flow make sense for your use case?"
- "What agents do you want to use with this? What's missing for your agent?"
- "What connectors matter most? (Gmail, Drive, DB, Slack, etc.)"
- "Would you pay for a hosted version? What would that need to include?"

## Phase v0.1.1 — Release Automation & Binary Distribution
<!-- status: deferred -->

### Done
- [x] **GitHub Actions CI** (`.github/workflows/ci.yml`): lint (clippy + fmt), test, build on push/PR
  - Ubuntu + macOS matrix, Nix devShell via DeterminateSystems/nix-installer-action
  - Magic Nix Cache (no auth token needed), step timeouts, graceful degradation
- [x] **Release workflow** (`.github/workflows/release.yml`): triggered by version tag or manual dispatch
  - Cross-compile matrix: macOS aarch64 + x86_64 (native), Linux x86_64 + aarch64 (musl via `cross`)
  - Creates GitHub Release with binary tarballs + SHA256 checksums
  - Publishes to crates.io (requires `CARGO_REGISTRY_TOKEN` secret)

### Remaining
- **Validate release end-to-end** (manual — see checklist below)
- **Install script**: `curl -fsSL https://ta.dev/install.sh | sh` one-liner (download + place in PATH)
- **Version bumping**: `cargo release` or manual Cargo.toml + git tag workflow
- **Auto-generated release notes**: Collect PR titles merged since last tag and format into GitHub Release body. Use `gh api repos/{owner}/{repo}/releases/generate-notes` or `git log --merges --oneline <prev-tag>..HEAD`. Optionally configurable via `.ta/release.toml` (include/exclude labels, group by category).
- **Nix flake output**: `nix run github:trustedautonomy/ta` for Nix users
- **Homebrew formula**: Future — tap for macOS users (`brew install trustedautonomy/tap/ta`)

### Release Validation Checklist (manual, one-time)
These steps must be done by the repo owner to validate the release pipeline:

1. **Set GitHub secrets** (Settings → Secrets and variables → Actions):
   - `CARGO_REGISTRY_TOKEN` — from `cargo login` / crates.io API tokens page
   - (Optional) `CACHIX_AUTH_TOKEN` — only needed if you want to push Nix cache binaries

2. **Verify CI passes on a PR to main**:
   ```bash
   git checkout feature/release-automation
   gh pr create --base main --title "Release Automation" --body "CI + release workflows"
   # Wait for CI checks to pass on both Ubuntu and macOS
   ```

3. **Merge to main** and verify CI runs on the main branch push.

4. **Test release workflow** (dry run via manual dispatch):
   ```bash
   # From GitHub Actions tab → Release → Run workflow → enter tag "v0.1.0-alpha"
   # Or from CLI:
   gh workflow run release.yml -f tag=v0.1.0-alpha
   ```
   - Verify: 4 binary artifacts built (2× macOS, 2× Linux musl)
   - Verify: GitHub Release page created with binaries + checksums
   - Verify: crates.io publish attempted (will fail if metadata incomplete — check Cargo.toml)

5. **Test the binaries**:
   ```bash
   # Download and verify on macOS:
   tar xzf ta-v0.1.0-alpha-aarch64-apple-darwin.tar.gz
   ./ta --version
   # Should show: ta 0.1.0-alpha (git-hash date)
   ```

6. **Validate `cargo install`** (after crates.io publish succeeds):
   ```bash
   cargo install ta-cli
   ta --version
   ```

## Phase v0.1.2 — Follow-Up Goals & Iterative Review
<!-- status: done -->
**Goal**: Enable iterative refinement — fix CI failures, address discuss items, revise rejected changes — without losing context from the original goal.

### Core: `ta goal start "title" --follow-up [id]` ✅ **Implemented**
- ✅ `--follow-up` without ID: finds the most recent goal (prefers unapplied, falls back to latest applied)
- ✅ `--follow-up <id-prefix>`: match by first N characters of goal UUID (no full hash needed)
- ✅ `GoalRun` gets `parent_goal_id: Option<Uuid>` linking to the predecessor

### Staging Behavior (depends on parent state)

> **Note (v0.1.2 implementation)**: The optimization to start from parent staging is **deferred to a future release**. Current implementation always starts from source, which works correctly but may require manually re-applying parent changes when parent PR is unapplied. The parent context injection and PR supersession work as designed.

**Parent NOT yet applied** (PrReady / UnderReview / Approved) — *Planned optimization*:
- Follow-up staging should start from the **parent's staging** (preserves in-flight work)
- `ta pr build` should diff against the **original source** (same base as parent)
- The follow-up's PR **supersedes** the parent's PR — single unified diff covering both rounds ✅ **Implemented**
- Parent PR status transitions to `Superseded { superseded_by: Uuid }` ✅ **Implemented**
- Result: one collapsed PR for review, not a chain of incremental PRs

**Parent already applied** (Applied / Completed) — *Current behavior*:
- Follow-up staging starts from **current source** (which already has applied changes) ✅ **Implemented**
- Creates a new, independent PR for the follow-up changes ✅ **Implemented**
- Parent link preserved for audit trail / context injection only ✅ **Implemented**

### Context Injection ✅ **Implemented**
When a follow-up goal starts, `inject_claude_md()` includes parent context:
- ✅ Parent goal title, objective, summary (what was done)
- ✅ Artifact list with dispositions (what was approved/rejected/discussed)
- ✅ Any discuss items with their rationale (from `change_summary.json`)
- ✅ Free-text follow-up context from the objective field

**Specifying detailed context**:
- ✅ Short: `ta run "Fix CI lint failures" --source . --follow-up` (title IS the context)
- ✅ Detailed: `ta run --source . --follow-up --objective "Fix clippy warnings in pr.rs and add missing test for edge case X. Also address the discuss item on config.toml — reviewer wanted env var override support."` (objective field scales to paragraphs)
- ✅ From file: `ta run --source . --follow-up --objective-file review-notes.md` (for structured review notes)
- **Phase 4d integration** (future): When discuss items have comment threads (Phase 4d), those comments auto-populate follow-up context — each discussed artifact's thread becomes a structured section in CLAUDE.md injection. The `--follow-up` flag on a goal with discuss items is the resolution path for Phase 4d's discussion workflow.

### CLI Changes
- ✅ `ta goal start` / `ta run`: add `--follow-up [id-prefix]` and `--objective-file <path>` flags
- ✅ `ta goal list`: show parent chain (`goal-abc → goal-def (follow-up)`)
- ✅ `ta pr list`: show superseded PRs with `[superseded]` marker
- ✅ `ta pr build`: when parent PR exists and is unapplied, mark it superseded

### Data Model Changes
- ✅ `GoalRun`: add `parent_goal_id: Option<Uuid>`
- ✅ `PRStatus`: add `Superseded { superseded_by: Uuid }` variant
- ✅ `PRPackage`: no changes (the new PR package is a complete, standalone package)

### Phase 4d Note
> Follow-up goals are the **resolution mechanism** for Phase 4d discuss items. When 4d adds per-artifact comment threads and persistent review sessions, `--follow-up` on a goal with unresolved discuss items will inject those threads as structured agent instructions. The agent addresses each discussed artifact; the resulting PR supersedes the original. This keeps discuss → revise → re-review as a natural loop without new CLI commands — just `ta run --follow-up`.

---

## v0.2 — Submit Adapters & Workflow Automation *(release: tag v0.2.0-alpha)*

### v0.2.0 — SubmitAdapter Trait & Git Implementation
<!-- status: done -->
**Architecture**: The staging→review→apply loop is VCS-agnostic. "Submit" is a pluggable adapter — git is the first implementation, but the trait supports Perforce, SVN, plain file copy, or non-code workflows (art pipelines, document review).

#### SubmitAdapter Trait (`crates/ta-workspace` or new `crates/ta-submit`)
```rust
pub trait SubmitAdapter: Send + Sync {
    /// Create a working branch/changelist/workspace for this goal.
    fn prepare(&self, goal: &GoalRun, config: &SubmitConfig) -> Result<()>;
    /// Commit/shelve the approved changes from staging.
    fn commit(&self, goal: &GoalRun, pr: &PRPackage, message: &str) -> Result<CommitResult>;
    /// Push/submit the committed changes for review.
    fn push(&self, goal: &GoalRun) -> Result<PushResult>;
    /// Open a review request (GitHub PR, Perforce review, email, etc.).
    fn open_review(&self, goal: &GoalRun, pr: &PRPackage) -> Result<ReviewResult>;
    /// Adapter display name (for CLI output).
    fn name(&self) -> &str;
}
```
`CommitResult`, `PushResult`, `ReviewResult` are adapter-neutral structs carrying identifiers (commit hash, changelist number, PR URL, etc.).

#### Built-in Adapters
- **`git`** (default): Git branching + GitHub/GitLab PR creation
  - `branch_prefix`: naming convention for auto-created branches (e.g., `ta/`, `feature/`)
  - `auto_branch`: create a feature branch automatically on `ta goal start`
  - `auto_review`: open a GitHub/GitLab PR automatically after commit+push
  - `pr_template`: path to PR body template with `{summary}`, `{artifacts}`, `{plan_phase}` substitution
  - `merge_strategy`: `squash` | `merge` | `rebase` (default: `squash`)
  - `target_branch`: base branch for PRs (default: `main`)
  - `remote`: git remote name (default: `origin`)
- **`none`** (fallback): Just copy files back to source. No VCS operations. Current behavior when no config exists.
- **Future adapters** (not in v0.2): `perforce` (changelists + Swarm), `svn`, `art-pipeline` (file copy + notification)

#### Workflow Config (`.ta/workflow.toml`)
```toml
[submit]
adapter = "git"                    # or "none"; future: "perforce", "svn"
auto_commit = true                 # commit on ta pr apply
auto_push = true                   # push after commit
auto_review = true                 # open PR/review after push

[submit.git]                       # adapter-specific settings
branch_prefix = "ta/"
target_branch = "main"
merge_strategy = "squash"
pr_template = ".ta/pr-template.md"
```

#### CLI Changes
- **`ta pr apply <id> --submit`** runs the full adapter pipeline: commit → push → open review
- **`ta pr apply <id> --git-commit`** remains as shorthand (equivalent to `--submit` with git adapter, no push)
- **`ta pr apply <id> --git-commit --push`** equivalent to `--submit` with git adapter + push + open review
- **Branch lifecycle**: `ta goal start` calls `adapter.prepare()` (git: creates branch), `ta pr apply --submit` calls commit → push → open_review

#### Integration Points
- **CLAUDE.md injection**: injects workflow instructions so agents respect the configured VCS (e.g., commit to feature branches for git, don't touch VCS for `none`)
- **Backwards-compatible**: without `.ta/workflow.toml`, behavior is identical to today (`none` adapter — just file copy)
- **Agent launch configs**: YAML agent configs can reference workflow adapter for prompt context

#### Future Extensibility & Design Evolution
**Vision**: The `SubmitAdapter` pattern is designed to extend beyond VCS to any "submit" workflow where changes need approval before affecting the outside world.

**Potential Non-VCS Adapters** (post-v0.2):
- **Webhook/API adapter**: POST PRPackage JSON to REST endpoints for external review systems
- **Email adapter**: Send PR summaries via SMTP with reply-to-approve workflows (integrates with v0.9 notification connectors)
- **Storage adapter**: Upload artifacts to S3/GCS/Drive with shareable review links
- **Ticketing adapter**: Create JIRA/Linear/GitHub Issues for review workflows
- **Slack/Discord adapter**: Post review requests as interactive messages with approval buttons (v0.9 integration)

**Architectural Decision (v0.3+ if needed)**:
- **Recommendation**: Keep `SubmitAdapter` VCS-focused for clarity. Introduce parallel traits for other domains:
  - `NotifyAdapter` — for notification/communication workflows (v0.9)
  - `PublishAdapter` — for API/webhook publishing workflows (v0.4-v0.5 timeframe)
  - `StorageAdapter` — for artifact upload/sharing workflows (v0.5 timeframe)
- **Rationale**: Specialized traits provide clearer semantics than forcing all workflows through VCS-oriented method names (prepare/commit/push/open_review). Each domain gets methods that make semantic sense for that domain.
- **Alternative considered**: Generalize `SubmitAdapter` methods to `prepare/submit/request_review/finalize`. Rejected because VCS workflows are the primary use case and generic names lose clarity.

**Roadmap Integration**:
- **v0.3-v0.4**: If demand arises, introduce `PublishAdapter` for webhook/API submission workflows
- **v0.5**: Evaluate `StorageAdapter` for external connector integration (Gmail, Drive per existing plan)
- **v0.9**: `NotifyAdapter` integrates with notification connectors (email, Slack, Discord)
- **v1.0**: Virtual office roles can compose multiple adapter types (VCS + notifications + storage) for comprehensive workflows

**Design Principle**: "Submit" isn't just VCS — it's any workflow where changes need approval before affecting external state. The adapter pattern enables pluggable approval workflows across all domains.

### v0.2.1 — Concurrent Session Conflict Detection
<!-- status: done -->
- Detect when source files have changed since staging copy was made (stale overlay)
- On `ta pr apply`: compare source file mtime/hash against snapshot taken at `ta goal start`
- Conflict resolution strategies: abort, merge (delegate to VCS adapter's merge if available), force-overwrite
- `SourceSnapshot` captured automatically at overlay creation (mtime + SHA-256)
- `--conflict-resolution abort|force-overwrite|merge` CLI flag on `ta pr apply`
- `apply_with_conflict_check()` aborts on conflict by default, warns and proceeds on force-overwrite
- 8 unit tests + integration tests
- **Remaining**: lock files or advisory locks for active goals (deferred to future)
- **Adapter integration**: git adapter can use `git merge`/`git diff` for smarter conflict resolution; `none` adapter falls back to mtime/hash comparison only
- **Multi-agent intra-staging conflicts**: When multiple agents work in the same staging workspace (e.g., via Claude Flow swarms), consider integrating [agentic-jujutsu](https://github.com/ruvnet/claude-flow) for lock-free concurrent file operations with auto-merge. This handles agent-to-agent coordination; TA handles agent-to-human review. Different layers, composable.

### v0.2.2 — External Diff Routing
<!-- status: done -->
- ✅ Config file (`.ta/diff-handlers.toml`) maps file patterns to external applications
- ✅ Examples: `*.uasset` → Unreal Editor, `*.png` → image diff tool, `*.blend` → Blender
- ✅ `ta pr view <id> --file model.uasset` opens the file in the configured handler
- ✅ Default handlers: text → inline diff (current), binary → byte count summary
- ✅ Integration with OS `open` / `xdg-open` as fallback
- ✅ New module: `ta-changeset::diff_handlers` with TOML parsing and pattern matching
- ✅ CLI flags: `--open-external` (default) / `--no-open-external` to control behavior
- ✅ Documentation and example config at `.ta/diff-handlers.example.toml`

### v0.2.3 — Tiered Diff Explanations & Output Adapters
<!-- status: done -->
**Goal**: Rich, layered diff review — top-level summary → medium detail → full diff, with pluggable output formatting.

#### Tiered Explanation Model
Each artifact in a PR gets a three-tier explanation:
1. **Top**: One-line summary (e.g., "Refactored auth middleware to use JWT")
2. **Medium**: Paragraph explaining what changed and why, dependencies affected
3. **Detail**: Full unified diff with inline annotations

Agents populate tiers via sidecar files: `<filename>.diff.explanation.yaml` (or JSON) written alongside changes. Schema:
```yaml
file: src/auth/middleware.rs
summary: "Refactored auth middleware to use JWT instead of session tokens"
explanation: |
  Replaced session-based auth with JWT validation. The middleware now
  checks the Authorization header for a Bearer token, validates it
  against the JWKS endpoint, and extracts claims into the request context.
  This change touches 3 files: middleware.rs (core logic), config.rs
  (JWT settings), and tests/auth_test.rs (updated test fixtures).
tags: [security, breaking-change]
related_artifacts:
  - src/auth/config.rs
  - tests/auth_test.rs
```

#### Output Adapters (Plugin System)
Configurable output renderers for `ta pr view`, designed for reuse:
- **terminal** (default): Colored inline diff with collapsible tiers (summary → expand for detail)
- **markdown**: Render PR as `.md` file — useful for GitHub PR bodies or documentation
- **json**: Machine-readable structured output for CI/CD integration
- **html**: Standalone review page with expandable sections (JavaScript-free progressive disclosure)
- Config: `.ta/output.toml` or `--format <adapter>` flag on `ta pr view`
- Plugin interface: adapter receives `PRPackage` + explanation sidecars, returns formatted output
- Adapters are composable: `ta pr view <id> --format markdown > review.md`

#### CLI Changes
- `ta pr view <id> --detail top|medium|full` (default: medium — shows summary + explanation, not full diff)
- `ta pr view <id> --format terminal|markdown|json|html`
- `ta pr build` ingests `*.diff.explanation.yaml` sidecars into PRPackage (similar to `change_summary.json`)
- CLAUDE.md injection instructs agents to produce explanation sidecars alongside changes

#### Data Model
- `Artifact` gains optional `explanation_tiers: Option<ExplanationTiers>` (summary, explanation, tags)
- `PRPackage` stores tier data; output adapters read it at render time
- Explanation sidecars are ingested at `ta pr build` time, not stored permanently in staging

### v0.2.4 — Terminology & Positioning Pass
<!-- status: done -->
**Goal**: Rename user-facing concepts for clarity. TA is an **agentic governance wrapper** — it wraps agent execution transparently, holds proposed changes at a human review checkpoint, and applies approved changes to the user's world. Terminology should work for developers and non-developers alike, and avoid VCS jargon since TA targets Perforce, SVN, document platforms, email, social media, and more.

#### Core Terminology Changes

| Old term | New term | Rationale |
|---|---|---|
| **PRPackage** | **Draft** | A draft is the package of agent work products awaiting review. Implies "complete enough to review, not final until approved." No git connotation. |
| **PRStatus** | **DraftStatus** | Follows from Draft rename. |
| **`ta pr build/view/approve/deny/apply`** | **`ta draft build/view/approve/deny/apply`** | CLI surface rename. Keep `apply` — it's VCS-neutral and universally understood. |
| **PendingReview (status)** | **Checkpoint** | The human-in-the-loop review gate where a Draft is examined for approval. |
| **staging dir / overlay** | **Virtual Workspace** | Where the agent works. Invisible to the agent. Will become lightweight/virtual (V2: reflinks/FUSE). "Staging" is git jargon; "virtual workspace" is self-explanatory. |
| **"substrate" / "layer"** | **Wrapper** | TA wraps agent execution. "Substrate" sounds like marketing; "layer" is vague; "wrapper" is literal and clear. |
| **PR (in docs/README)** | **Draft** | Everywhere user-facing text says "PR" in the TA-specific sense (not git PRs). |

#### Flow in New Terminology
```
Agent works in Virtual Workspace
  -> produces a Draft
    -> human reviews at Checkpoint
      -> Approves / Rejects each change
        -> Approved changes are Applied
```

#### Scope of Changes
- **Code**: Rename `PRPackage` -> `DraftPackage`, `PRStatus` -> `DraftStatus`, `pr_package.rs` -> `draft_package.rs`
- **CLI**: `ta draft` subcommand replaces `ta pr`. Keep `ta pr` as hidden alias for backwards compatibility during transition.
- **Docs**: README, USAGE.md, CLAUDE.md, PLAN.md — replace TA-specific "PR" with "Draft", "staging" with "virtual workspace" in user-facing text
- **Schema**: `schema/pr_package.schema.json` -> `schema/draft_package.schema.json` (or alias)
- **Internal code comments**: Update incrementally, not a big-bang rename. Internal variable names can migrate over time.

#### What Stays the Same
- `apply` — VCS-neutral, universally understood
- `artifact` — standard term for individual changed items within a Draft
- `goal` — clear, no issues
- `checkpoint` — only replaces `PendingReview` status; the concept name for the review gate
- All internal architecture (overlay, snapshot, conflict detection) — implementation names are fine; only user-facing surface changes

#### Positioning Statement (draft)
> **Trusted Autonomy** is an agentic governance wrapper. It lets AI agents work freely using their native tools in a virtual workspace, then holds their proposed changes — code commits, document edits, emails, posts — at a checkpoint for human review before anything takes effect. The human sees what the agent wants to do, approves or rejects each action, and maintains an audit trail of every decision.

#### Open Questions
- Should `DraftPackage` just be `Draft`? Shorter, but `Draft` alone is generic. `DraftPackage` parallels the current data model. Decide during implementation. **Decision**: keep `DraftPackage`
- `Checkpoint` as a status vs. a concept: currently the status enum has `PendingReview`. Rename to `AtCheckpoint`? Or keep `PendingReview` internally and use "checkpoint" only in user-facing text? **Decision**: keep `PendingReview`
- `ta draft` vs `ta review` as the subcommand? `draft` emphasizes the agent's output; `review` emphasizes the human's action. Both valid. `draft` chosen because the subcommand operates on the draft object (`build`, `view`, `apply`). **Decision**: keep `draft` 

---

## v0.3 — Review & Plan Automation *(release: tag v0.3.0-alpha)*

### v0.3.0 — Review Sessions
<!-- status: done -->
**Completed**:
- ✅ ReviewSession data model with persistent storage (review_session.rs, review_session_store.rs)
- ✅ Per-artifact comment threads integrated into Artifact model (`comments: Option<Vec<Comment>>`)
- ✅ Session state tracking (Active, Paused, Completed, Abandoned)
- ✅ Disposition counts and summary methods
- ✅ CLI review workflow: `ta draft review start/comment/next/finish/list/show`
- ✅ 50+ new unit tests (total: 258 tests across 12 crates)
- ✅ **Supervisor agent** (`crates/ta-changeset/src/supervisor.rs`): Dependency graph analysis with cycle detection, self-dependency detection, coupled rejection warnings, and broken dependency warnings. Integrated into `ta draft apply` with enhanced error/warning display (13 new tests, total: 271 tests)
- ✅ **Discussion workflow implementation**: Comment threads from discuss items are now injected into CLAUDE.md when creating follow-up goals. The `build_parent_context_section` function in `apps/ta-cli/src/commands/run.rs` includes full comment threads, explanation tiers, and agent rationale for each discussed artifact. Agents receive structured discussion history as context, enabling them to address reviewer concerns in follow-up iterations. (2 new tests, total: 273 tests)

- ✅ **Per-target summary enforcement**: At `ta draft build` time, configurable enforcement (ignore/warning/error via `[build] summary_enforcement` in `.ta/workflow.toml`) warns or errors when artifacts lack a `what` description. Lockfiles, config manifests, and docs are auto-exempt via hardcoded list. (3 new tests, total: 289 tests) *(Exemption patterns become configurable in v0.4.0; per-goal access constitutions in v0.4.3)*
- ✅ **Disposition badges in HTML output**: HTML adapter renders per-artifact disposition badges (pending/approved/rejected/discuss) with color-coded CSS classes. Added `.status.discuss` styling. (3 new tests)
- ✅ **Config bugfix**: Added `#[serde(default)]` to `WorkflowConfig.submit` field so partial `.ta/workflow.toml` files parse correctly without requiring a `[submit]` section.

### v0.3.0.1 — Consolidate `pr.rs` into `draft.rs`
<!-- status: done -->
**Completed**:
- ✅ `pr.rs` reduced from 2205 lines to ~160 lines: thin shim that converts `PrCommands` → `DraftCommands` and delegates to `draft::execute()`
- ✅ `run.rs` updated to call `draft::DraftCommands::Build` instead of `pr::PrCommands::Build`
- ✅ `run.rs` follow-up context updated to use `draft::load_package` and `draft_package::ArtifactDisposition`
- ✅ All ~20 duplicated private functions removed from `pr.rs` (~2050 lines eliminated)
- ✅ `ta pr` remains as a hidden alias for backward compatibility
- ✅ All 278 tests passing (11 duplicate pr.rs tests removed; all functionality covered by draft.rs tests)

### v0.3.1 — Plan Lifecycle Automation
<!-- status: done -->
**Completed** (294 tests across 12 crates):
- ✅ Supervisor `validate_against_plan()` reads change_summary.json, validates completed work against plan at `ta draft build` time (4 new tests)
- ✅ Completing one phase auto-suggests/creates goal for next pending phase (output after `ta draft apply --phase`)
- ✅ Plan parser extended to handle `### v0.X.Y` sub-phase headers in addition to `## Phase` top-level headers
- ✅ `ta plan next` command shows next pending phase and suggests `ta run` command (new CLI command)
- ✅ `ta plan validate <phase>` command shows phase status, linked goals, and latest draft summary (new CLI command)
- ✅ Plan versioning and history: status transitions recorded to `.ta/plan_history.jsonl`, viewable via `ta plan history` (new CLI command)
- ✅ Git commit message in `ta draft apply` now includes complete draft summary with per-artifact descriptions (`build_commit_message` function)
- ✅ 16 new tests: plan parsing for sub-phases (4), plan lifecycle (find_next, suggest, history — 8), supervisor plan validation (4)

### v0.3.1.1 — Configurable Plan Format Parsing
<!-- status: done -->

**Completed** (307 tests across 12 crates):
- ✅ `PlanSchema` data model with `PhasePattern` and YAML serde support (`.ta/plan-schema.yaml`)
- ✅ `parse_plan_with_schema()` — regex-driven plan parser that replaces hardcoded parsing logic
- ✅ `parse_plan()` and `load_plan()` now delegate to schema-driven parser with default schema (full backward compatibility)
- ✅ `update_phase_status_with_schema()` — schema-aware status updates
- ✅ `PlanSchema::load_or_default()` — loads `.ta/plan-schema.yaml` or falls back to built-in default
- ✅ `ta plan init` command — auto-detects plan format, proposes schema, writes `.ta/plan-schema.yaml`
- ✅ `ta plan create` command — generates plan documents from templates (greenfield, feature, bugfix)
- ✅ `detect_schema_from_content()` — heuristic schema detection for `ta plan init`
- ✅ Bug fix: `strip_html()` in terminal adapter prevents HTML tags from leaking into terminal output (garbled `ÆpendingÅ` display)
- ✅ `regex` crate added to workspace dependencies
- ✅ 13 new tests: schema round-trip (1), schema loading (2), custom schema parsing (2), schema detection (2), template parsing (1), custom schema status update (1), custom schema load_plan (1), invalid regex handling (2), terminal HTML regression (3)

#### Problem
`plan.rs` hardcodes this project's PLAN.md format (`## v0.X`, `### v0.X.Y`, `<!-- status: -->` markers). Any other project using TA would need to adopt the same markdown conventions or nothing works. The parser should be schema-driven, not format-hardcoded.

#### Solution: `.ta/plan-schema.yaml`
Declarative config describing how to parse a project's plan document. Shipped with sensible defaults that match common markdown patterns.
```yaml
# .ta/plan-schema.yaml
source: PLAN.md                          # or ROADMAP.md, TODO.md, etc.
phase_patterns:
  - regex: "^##+ (?:v?[\\d.]+[a-z]? — |Phase \\d+ — )(.+)"
    id_capture: "version_or_phase_number"
status_marker: "<!-- status: (\\w+) -->"   # regex with capture group
statuses: [done, in_progress, pending]     # valid values
```

#### CLI
- **`ta plan init`**: Agent-guided schema extraction — reads an existing plan document, proposes a `plan-schema.yaml`, human approves. Zero effort for projects that already have a plan.
- **`ta plan create`**: Generate a new plan document from a template + schema. Templates for common workflows (feature, bugfix, greenfield).
- Refactor `parse_plan()` to read schema at runtime instead of hardcoded regexes. Existing behavior preserved as the default schema (zero-config for projects that adopt the current convention).

#### Bug fix: garbled HTML in terminal output
`ta draft view` renders `ÆpendingÅ` instead of `[pending]` — HTML `<span>` tags leaking into terminal output with encoding corruption. Fix: `strip_html()` helper in `TerminalAdapter` sanitizes all user-provided text fields before rendering. Regression test asserts terminal output contains no HTML tags.

### v0.3.1.2 — Interactive Session Orchestration
<!-- status: done -->

#### Vision
The human orchestrates construction iteratively across multiple goal sessions — observing agent work, injecting guidance, reviewing drafts, and resuming sessions — through a unified interaction layer. This phase builds the **session interaction protocol** that underpins both the local CLI experience and the future TA web app / messaging integrations (Discord, Slack, email).

> **Design principle**: Every interaction between human and TA is a **message** on a **channel**. The CLI is one channel. A Discord thread is another. The protocol is the same — TA doesn't care where the message came from, only that it's authenticated and routed to the right session.

#### Session Interaction Protocol
The core abstraction: a `SessionChannel` trait that any frontend implements.

```rust
/// A bidirectional channel between a human and a TA-mediated agent session.
pub trait SessionChannel: Send + Sync {
    /// Display agent output to the human (streaming).
    fn emit(&self, event: SessionEvent) -> Result<()>;
    /// Receive human input (blocks until available or timeout).
    fn receive(&self, timeout: Duration) -> Result<Option<HumanInput>>;
    /// Channel identity (for audit trail).
    fn channel_id(&self) -> &str;  // "cli:tty0", "discord:thread:123", "slack:C04..."
}

pub enum SessionEvent {
    AgentOutput { stream: Stream, content: String },  // stdout/stderr
    DraftReady { draft_id: Uuid, summary: String },   // checkpoint
    GoalComplete { goal_id: Uuid },
    WaitingForInput { prompt: String },                // agent needs guidance
}

pub enum HumanInput {
    Message(String),                    // guidance injected into agent context
    Approve { draft_id: Uuid },         // inline review
    Reject { draft_id: Uuid, reason: String },
    Abort,                              // kill session
}
```

#### CLI implementation (`ta run --interactive`)
The first `SessionChannel` implementation — wraps the agent CLI with PTY capture.

- **Observable output**: Agent stdout/stderr piped through TA, displayed to human, captured for audit.
- **Session wrapping**: TA launches agent CLI inside a session envelope. Agent doesn't know TA exists. TA controls environment injection and exit.
- **Human interrogation**: stdin interleaving lets human inject guidance. Agent responds using existing context — no token cost for re-learning state.
- **Context preservation on resume**: Uses agent-framework-native resume (Claude `--resume`, Codex session files) when available. Falls back to CLAUDE.md context injection.
- **Per-agent config**: `agents/<name>.yaml` gains `interactive` block:
```yaml
interactive:
  launch_cmd: "claude --resume {session_id}"
  output_capture: "pty"              # pty, pipe, or log
  allow_human_input: true
  auto_exit_on: "idle_timeout: 300s" # or "goal_complete"
```

#### MCP integration surface (for messaging channels)
The `SessionChannel` trait is designed so that messaging platform integrations are thin adapters, not new features. Each maps platform primitives to `SessionEvent` / `HumanInput`:

| Platform | `emit()` | `receive()` | Session identity |
|----------|----------|-------------|-----------------|
| CLI (v0.3.1.2) | PTY stdout | stdin | `cli:{tty}` |
| Discord (future) | Thread message | Thread reply | `discord:{thread_id}` |
| Slack (future) | Channel message | Thread reply | `slack:{channel}:{ts}` |
| Email (future) | Reply email | Incoming email | `email:{thread_id}` |
| Web app (future) | WebSocket push | WebSocket message | `web:{session_id}` |

Each adapter is ~100-200 lines: authenticate, map to `SessionChannel`, route to the correct TA session. All governance (draft review, audit, policy) is handled by TA core — the channel just carries messages.

#### Stepping stones to the TA app
This phase deliberately builds the protocol layer that the TA local/web app will consume:
- **Session list + status**: `ta session list` shows active sessions across all channels. Web app renders the same data.
- **Draft review inline**: Human can approve/reject drafts from within the session (any channel), not just via separate `ta draft approve` commands.
- **Multi-session orchestration**: Human can have multiple active sessions (different goals/agents) and switch between them. Web app shows them as tabs; Discord shows them as threads.
- Relates to v0.4.1 (macro goals) — interactive sessions are the human-facing complement to the agent-facing MCP tools in macro goal mode.

### v0.3.2 — Configurable Release Pipeline (`ta release`)
<!-- status: done -->
A `ta release` command driven by a YAML task script (`.ta/release.yaml`). Each step is either a TA goal (agent-driven) or a shell command, with optional approval gates. Replaces `scripts/release.sh` with a composable, extensible pipeline.

- ✅ **YAML schema**: Steps with `name`, `agent` or `run`, `objective`, `output`, `requires_approval`
- ✅ **Agent steps**: Create a TA goal for the agent to execute (e.g., synthesize release notes from commits)
- ✅ **Shell steps**: Run build/test/tag commands directly
- ✅ **Commit collection**: Automatically gather commits since last tag as context for agent steps
- ✅ **Built-in pipeline**: Default release.yaml ships with the binary (version bump, verify, release notes, tag)
- ✅ **Customizable**: Users override with `.ta/release.yaml` in their project
- ✅ **Approval gates**: `requires_approval: true` pauses for human review before proceeding (e.g., before push)

### v0.3.3 — Decision Observability & Reasoning Capture
<!-- status: done -->
**Goal**: Make every decision in the TA pipeline observable — not just *what happened*, but *what was considered and why*. Foundation for drift detection (v0.4.2) and compliance reporting (ISO 42001, IEEE 7001).

> **Research note**: Evaluated [AAP](https://github.com/mnemom/aap) (Agent Alignment Protocol) for this role. AAP provides transparency through self-declared alignment cards and traced decisions, but is a Python/TypeScript decorator-based SDK that can't instrument external agents (Claude Code, Codex). TA's approach is stronger: enforce constraints architecturally, then capture the reasoning of TA's own decision pipeline. The *agent's* internal reasoning is captured via `change_summary.json`; TA's *governance* reasoning is captured here.

#### Data Model: `DecisionReasoning` in `ta-audit`
```rust
pub struct DecisionReasoning {
    /// What alternatives were considered.
    pub alternatives: Vec<Alternative>,
    /// Why this outcome was selected.
    pub rationale: String,
    /// Values/principles that informed the decision.
    pub applied_principles: Vec<String>,
}

pub struct Alternative {
    pub description: String,
    pub score: Option<f64>,
    pub rejected_reason: String,
}
```
Extends `AuditEvent` with an optional `reasoning: Option<DecisionReasoning>` field. Backward-compatible — existing events without reasoning still deserialize.

#### Integration Points
- **PolicyEngine.evaluate()**: Log which grants were checked, which matched, why allow/deny/require-approval. Captures the full capability evaluation chain, not just the final verdict.
- **Supervisor.validate()**: Log dependency graph analysis — which warnings were generated, which artifacts triggered them, what the graph structure looked like.
- **Human review decisions**: Extend ReviewSession comments with structured `reasoning` field — reviewer can explain *why* they approved/rejected, not just leave a text comment.
- **`ta draft build`**: Log why each artifact was classified (Add/Modify/Delete), what diff heuristics were applied.
- **`ta draft apply`**: Log conflict detection reasoning — which files conflicted, which were phantom (auto-resolved), what resolution strategy was applied and why.

#### Agent-Side: Extend `change_summary.json`
Add optional `alternatives_considered` field per change entry:
```json
{
  "path": "src/auth.rs",
  "what": "Migrated to JWT",
  "why": "Session tokens don't scale to multiple servers",
  "alternatives_considered": [
    { "description": "Sticky sessions", "rejected_reason": "Couples auth to infrastructure" },
    { "description": "Redis session store", "rejected_reason": "Adds operational dependency" }
  ]
}
```
Agents that support it get richer review context; agents that don't still work fine (field is optional).

#### CLI
- `ta audit show <goal-id>` — display decision trail for a goal with reasoning
- `ta audit export <goal-id> --format json` — structured export for compliance reporting

#### Standards Alignment
- **ISO/IEC 42001**: Documented decision processes with rationale (Annex A control A.6.2.3)
- **IEEE 7001**: Transparent autonomous systems — decisions are explainable to stakeholders
- **NIST AI RMF**: MAP 1.1 (intended purpose documentation), GOVERN 1.3 (decision documentation)

#### Completed
- `DecisionReasoning` + `Alternative` structs in `ta-audit` with `reasoning` field on `AuditEvent`
- `EvaluationTrace` + `EvaluationStep` in `ta-policy` — full trace from `PolicyEngine::evaluate_with_trace()`
- `AlternativeConsidered` struct and enriched `DecisionLogEntry` in `ta-changeset`
- Extended `PolicyDecisionRecord` with `grants_checked`, `matching_grant`, `evaluation_steps`
- `ReviewReasoning` struct on `Comment` — reviewers can document structured reasoning
- Extended `ChangeSummaryEntry` with `alternatives_considered` (agent-side)
- Decision log extraction in `ta draft build` — alternatives flow from change_summary.json into draft packages
- `ta audit show <goal-id>` — display decision trail with reasoning
- `ta audit export <goal-id> --format json` — structured compliance export
- 17 new tests across ta-audit, ta-policy, ta-changeset
- All backward-compatible — old data deserializes correctly

### v0.3.4 — Draft Amendment & Targeted Re-Work
<!-- status: done -->
**Goal**: Let users correct draft issues inline without a full agent re-run. Today the only correction path is a full `ta run --follow-up` cycle — overkill for a 10-line struct deduplication or a typo fix.

#### `ta draft amend` — Human-Provided Corrections
```bash
# Replace an artifact's content with a corrected file
ta draft amend <draft-id> <artifact-uri> --file path/to/corrected.rs

# Apply a patch to an artifact
ta draft amend <draft-id> <artifact-uri> --patch fix.patch

# Remove an artifact from the draft entirely
ta draft amend <draft-id> <artifact-uri> --drop
```
- Amends the draft package in-place (new artifact content, re-diffs against source)
- Records `amended_by: "human"` + timestamp in artifact metadata for audit trail
- Draft remains in review — user can approve/apply after amendment
- Decision log entry auto-added: "Human amended artifact: <reason>"

#### `ta draft fix` — Scoped Agent Re-Work
```bash
# Agent targets only discuss items with your guidance
ta draft fix <draft-id> --guidance "Remove AgentAlternative, reuse AlternativeConsidered directly"

# Target a specific artifact
ta draft fix <draft-id> <artifact-uri> --guidance "Consolidate duplicate struct"
```
- Creates a **scoped follow-up goal** targeting only discuss/amended artifacts (not the full source tree)
- Injects: artifact content + comment threads + user guidance into agent context
- Agent works in a minimal staging copy (only affected files, not full overlay)
- Builds a new draft that supersedes the original — review + apply as normal
- Much faster than full `ta run --follow-up` since scope is constrained

#### Usage Documentation
- Add "Correcting a Draft" section to USAGE.md covering the three correction paths:
  1. **Small fix**: `ta draft amend` (human edits directly)
  2. **Agent-assisted fix**: `ta draft fix --guidance` (scoped re-work)
  3. **Full re-work**: `ta run --follow-up` (complete re-run with discussion context)
- Document when to use each: amend for typos/renames, fix for logic changes, follow-up for architectural rework

#### Completed ✅
- `ta draft amend <id> <uri> --file <path>`: Replace artifact content with corrected file, recompute diff, record `AmendmentRecord` with `amended_by` + timestamp
- `ta draft amend <id> <uri> --drop`: Remove artifact from draft, record in decision log
- `AmendmentRecord` type added to `Artifact` struct (audit trail: who, when, how, why)
- `AmendmentType` enum: `FileReplaced`, `PatchApplied`, `Dropped`
- URI normalization: shorthand paths (e.g., `src/main.rs`) auto-expand to `fs://workspace/src/main.rs`
- Disposition reset to `Pending` after amendment (content changed, needs re-review)
- Decision log entries auto-added for all amendments
- Corrected files written back to staging workspace for consistency
- `ta draft fix <id> --guidance "<text>"`: Scoped follow-up goal targeting discuss/amended artifacts
- `ta draft fix <id> <uri> --guidance "<text>"`: Target a specific artifact
- Builds on existing `--follow-up` mechanism with focused context injection
- New draft supersedes the original via `DraftStatus::Superseded`
- USAGE.md "Correcting a Draft" section updated (removed "planned" markers)
- 10 new tests: 4 for `AmendmentRecord` serialization, 6 for `amend_package` integration (drop, file replace, state validation, error cases, diff computation)

#### Remaining
- `--patch fix.patch` mode for `ta draft amend` (deferred — `--file` covers the common case)
- Minimal staging workspace for `ta draft fix` (currently uses full overlay like `--follow-up`)

#### Existing Infrastructure This Builds On
- `ReviewSession` comment threads (v0.3.0) — comments + discuss items already tracked
- `GoalRun.parent_goal_id` + `PRStatus::Superseded` — follow-up chain already works
- `build_parent_context_section()` in run.rs — discuss items + comments already injected into follow-up goals
- `ArtifactDisposition::Discuss` (v0.3.0 Phase 4b) — selective review already identifies items needing attention

### v0.3.5 — Release Pipeline Fixes
<!-- status: done -->
**Goal**: Fix release pipeline issues discovered during v0.3.3 and v0.3.4 releases.

- **Release notes in GitHub Release**: `.release-draft.md` content now included in the GitHub Release body (was using hardcoded template ignoring generated notes)
- **Release notes in binary archives**: `.release-draft.md` shipped as `RELEASE-NOTES.md` inside each tar.gz
- **Release notes link in documentation section**: GitHub Release body includes link to release notes
- **PLAN.md status in commits**: Moved plan phase status update to before git commit so `<!-- status: done -->` is included in the release commit (was written after commit, lost on PR merge)
- **Post-apply validation**: `ta draft apply` prints state summary with warning if plan status didn't update
- **DISCLAIMER.md version removed**: Terms hash no longer changes on version bump, so users aren't forced to re-accept terms every release
- **Commit/tag step robustness**: Checks out main, skips commit if tree clean, skips tag if exists
- **Nix dirty-tree warning suppressed**: `./dev` uses `--no-warn-dirty`

### v0.3.6 — Draft Lifecycle Hygiene
<!-- status: done -->
**Goal**: Automated and manual cleanup of stale draft state so TA stays consistent without manual intervention.

- ✅ **`ta draft close <id> [--reason <text>]`**: Manually mark a draft as closed/superseded without applying it. For drafts that were hand-merged, abandoned, or made obsolete by later work. Records reason + timestamp in audit log.
- ✅ **`ta draft gc`**: Garbage-collect stale drafts and staging directories.
  - Remove staging dirs for drafts in terminal states (Applied, Denied, closed) older than N days (default 7, configurable in `.ta/workflow.toml`)
  - List what would be removed with `--dry-run`
  - Optionally archive to `.ta/archive/` instead of deleting (`--archive`)
- ✅ **`ta draft list --stale`**: Show drafts that are in non-terminal states (Approved, PendingReview) but whose staging dirs are older than a threshold — likely forgotten or hand-applied.
- ✅ **Auto-close on follow-up**: When `ta run --follow-up <id>` completes and its draft is applied, auto-close the parent draft if still in Approved/PendingReview state.
- ✅ **Startup health check**: On any `ta` invocation, emit a one-line warning if stale drafts exist (e.g. "1 draft approved but not applied for 3+ days — run `ta draft list --stale`"). Suppressible via config.

---

## v0.4 — Agent Intelligence *(release: tag v0.4.0-alpha)*

### v0.4.0 — Intent-to-Access Planner & Agent Alignment Profiles
<!-- status: done -->
- ✅ **Agent Alignment Profiles**: `ta-policy/src/alignment.rs` — `AlignmentProfile`, `AutonomyEnvelope`, `CoordinationConfig` types with YAML/JSON serialization. Profiles declare `bounded_actions`, `escalation_triggers`, `forbidden_actions`, plus `coordination` block for multi-agent scenarios. (10 tests)
- ✅ **Policy Compiler**: `ta-policy/src/compiler.rs` — `PolicyCompiler::compile()` transforms `AlignmentProfile` into `CapabilityManifest` grants. Validates forbidden/bounded overlap, parses `tool_verb` and `exec: command` formats, applies resource scoping. Replaces hardcoded manifest generation in `ta-mcp-gateway/server.rs`. (14 tests)
- ✅ **AgentSetupProposal**: `ta-policy/src/alignment.rs` — `AgentSetupProposal`, `ProposedAgent`, `Milestone` types for LLM-based intent-to-policy planning. JSON-serializable proposal structure for agent roster + scoped capabilities + milestone plan. (2 tests)
- ✅ **Configurable summary exemption**: `ta-policy/src/exemption.rs` — `ExemptionPatterns` with `.gitignore`-style pattern matching against `fs://workspace/` URIs. Replaces hardcoded `is_auto_summary_exempt()` in `draft.rs`. Loads from `.ta/summary-exempt` with default fallback. Example file at `examples/summary-exempt`. (13 tests)
- ✅ **Gateway integration**: `ta-mcp-gateway/server.rs` now uses `PolicyCompiler::compile_with_id()` with `AlignmentProfile::default_developer()`. New `start_goal_with_profile()` method accepts custom alignment profiles.
- ✅ **Agent YAML configs**: All agents (`claude-code.yaml`, `codex.yaml`, `claude-flow.yaml`) updated with `alignment` blocks. `generic.yaml` template documents the alignment schema.
- ✅ **CLI integration**: `AgentLaunchConfig` in `run.rs` gained `alignment: Option<AlignmentProfile>` field. `draft.rs` uses `ExemptionPatterns` for configurable summary enforcement.
- Agent setup evaluates how to run the agents efficiently at lowest cost (model selection, prompt caching, etc) and advises tradeoffs with human opt in where appropriate *(deferred to LLM integration phase)*

*(39 new tests in ta-policy; 415 total tests passing across all crates)*

#### Agent Alignment Profiles (extends YAML agent configs)
Inspired by [AAP alignment cards](https://github.com/mnemom/aap) but *enforced* rather than self-declared. Each agent's YAML config gains a structured `alignment` block:
```yaml
# agents/claude-code.yaml
alignment:
  principal: "project-owner"           # Who this agent serves
  autonomy_envelope:
    bounded_actions: ["fs_read", "fs_write", "exec: cargo test"]
    escalation_triggers: ["new_dependency", "security_sensitive", "breaking_change"]
    forbidden_actions: ["network_external", "credential_access"]
  constitution: "default-v1"           # Reference to enforcement rules
  coordination:
    allowed_collaborators: ["codex", "claude-flow"]
    shared_resources: ["src/**", "tests/**"]
```
- **Key difference from AAP**: These declarations are *compiled into CapabilityManifest grants* by the Policy Compiler. An agent declaring `forbidden_actions: ["network_external"]` gets a manifest with no network grants — it's not a promise, it's a constraint.
- **Coordination block**: Used by v0.4.1 macro goals and v1.0 virtual office to determine which agents can co-operate on shared resources.
- **Configurable summary exemption patterns**: Replace hardcoded `is_auto_summary_exempt()` with a `.gitignore`-style pattern file (e.g., `.ta/summary-exempt`), seeded by workflow templates and refined by the supervisor agent based on project structure analysis. Patterns would match against `fs://workspace/` URIs. (see v0.3.0 per-target summary enforcement)

#### Standards Alignment
- **IEEE 3152-2024**: Agent identity + capability declarations satisfy human/machine agency identification
- **ISO/IEC 42001**: Agent setup proposals + human approval = documented AI lifecycle management
- **NIST AI RMF GOVERN 1.1**: Defined roles and responsibilities for each agent in the system

### v0.4.1 — Macro Goals & Inner-Loop Iteration
<!-- status: done -->
**Goal**: Let agents stay in a single session, decompose work into sub-goals, submit drafts, and iterate — without exiting and restarting `ta run` each time.

> **Core insight**: Currently each `ta run` session is one goal → one draft → exit. For complex tasks (e.g., "build Trusted Autonomy v0.5"), the agent must exit, the human must approve, then another `ta run` starts. Macro goals keep the agent in-session while maintaining governance at every checkpoint.

#### MCP Tools Exposed to Agent (Passthrough Model)
TA injects MCP tools that mirror the CLI structure — same commands, same arguments:
- ✅ **`ta_draft`** `action: build|submit|status|list` — package, submit, and query drafts
- ✅ **`ta_goal`** (`ta_goal_inner`) `action: start|status` — create sub-goals, check status
- ✅ **`ta_plan`** `action: read|update` — read plan progress, propose updates

> **Design**: Passthrough mirrors the CLI (`ta draft build` = `ta_draft { action: "build" }`). No separate tool per subcommand — agents learn one pattern, new CLI commands are immediately available as MCP actions. Arguments map 1:1 to CLI flags.

#### Security Boundaries
- ✅ Agent **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- ✅ Agent **CANNOT**: approve its own drafts, apply changes, bypass checkpoints, modify policies
- ✅ Every sub-goal draft goes through the same human review gate as a regular draft
- ✅ Agent sees approval/rejection results and can iterate (revise and resubmit)
- ✅ `ta_draft { action: "submit" }` blocks until human responds (blocking mode) — agent cannot self-approve

#### Execution Modes
- ✅ **Blocking** (default): Agent submits draft, blocks until human responds. Safest — human reviews each step.
- **Optimistic** (future): Agent continues to next sub-goal while draft is pending. Human reviews asynchronously. Faster but requires rollback capability if earlier draft is rejected.
- **Hybrid** (future): Agent marks sub-goals as blocking or non-blocking based on risk. High-risk changes block; low-risk ones proceed optimistically.

#### CLI
- ✅ `ta run "Build v0.5" --source . --macro` — starts a macro goal session
- ✅ Agent receives MCP tools for inner-loop iteration alongside standard workspace tools
- ✅ `ta goal status <id>` shows sub-goal tree with approval status

#### Integration
- ✅ Sub-goals inherit the macro goal's plan phase, source dir, and agent config
- ✅ Each sub-goal draft appears in `ta draft list` as a child of the macro goal
- ✅ PLAN.md updates proposed via `ta_plan_update` are held at checkpoint (agent proposes, human approves)
- ✅ Works with existing follow-up goal mechanism — macro goals are the automated version of `--follow-up`

#### Data Model (v0.4.1)
- ✅ `GoalRun.is_macro: bool` — marks a goal as a macro session
- ✅ `GoalRun.parent_macro_id: Option<Uuid>` — links sub-goals to their macro parent
- ✅ `GoalRun.sub_goal_ids: Vec<Uuid>` — tracks sub-goals within a macro session
- ✅ `GoalRunState: PrReady → Running` transition for inner-loop iteration
- ✅ `TaEvent::PlanUpdateProposed` event variant for governance-gated plan updates
- ✅ CLAUDE.md injection includes macro goal context with MCP tool documentation
- ✅ 4 new tests (3 in ta-goal, 1 in ta-cli), tool count updated from 9 to 12 in ta-mcp-gateway

### v0.4.1.1 — Runtime Channel Architecture & Macro Session Loop
<!-- status: done -->
**Goal**: Wire up the runtime loop that makes `ta run --macro` actually work end-to-end. Implement a pluggable `ReviewChannel` trait for bidirectional human–agent communication at any interaction point (draft review, approval discussion, plan negotiation, etc.), with a terminal adapter as the default.

> **Core insight**: v0.4.1 laid down the data model and MCP tool definitions. This phase connects them — starting an MCP server alongside the agent, routing tool calls through the review channel, and allowing humans to respond via any medium (terminal, Slack, Discord, email, SMS, etc.). The channel abstraction is not specific to `ta_draft submit` — it covers every interaction point where a human and agent need to communicate.

#### Completed

- ✅ `ReviewChannel` trait with `request_interaction`, `notify`, `capabilities`, `channel_id` methods
- ✅ `InteractionRequest` / `InteractionResponse` / `Decision` / `Notification` data model in `ta-changeset::interaction`
- ✅ `InteractionKind`: `DraftReview | ApprovalDiscussion | PlanNegotiation | Escalation | Custom(String)`
- ✅ `Urgency`: `Blocking | Advisory | Informational`
- ✅ `ChannelCapabilities` flags: `supports_async`, `supports_rich_media`, `supports_threads`
- ✅ `TerminalChannel` adapter: renders interactions to stdout, collects responses from stdin, supports mock I/O for testing
- ✅ `AutoApproveChannel`: no-op channel for batch/non-interactive mode
- ✅ `ReviewChannelConfig`: channel type, blocking mode, notification level (stored in `GatewayConfig`)
- ✅ MCP gateway integration: `ta_draft submit` routes through `ReviewChannel`, returns decision to agent
- ✅ MCP gateway integration: `ta_plan update` routes through `ReviewChannel`, returns decision to agent
- ✅ `GatewayState.review_channel`: pluggable channel with `set_review_channel()` method
- ✅ Macro goal loop: approved drafts transition macro goals `PrReady → Running` for inner-loop iteration
- ✅ Audit trail: all interactions logged via `tracing::info!` with interaction_id, kind, and decision
- ✅ 45 new tests across interaction, review_channel, terminal_channel modules (12 + 4 + 18 + 11 existing gateway tests pass)

#### Data Model

```rust
pub trait ReviewChannel: Send + Sync {
    fn request_interaction(&self, request: &InteractionRequest) -> Result<InteractionResponse, ReviewChannelError>;
    fn notify(&self, notification: &Notification) -> Result<(), ReviewChannelError>;
    fn capabilities(&self) -> ChannelCapabilities;
    fn channel_id(&self) -> &str;
}
```

#### Runtime Loop (for `ta run --macro`)
1. Start MCP gateway server in background thread, bound to a local socket
2. Launch agent with `--mcp-server` endpoint configured
3. Agent calls MCP tools → gateway routes to TA core logic
4. When interaction is needed (draft submit, approval question, plan update), emit `InteractionRequest` through the configured `ReviewChannel`
5. Channel adapter delivers to human via configured medium
6. Human responds through same channel
7. Channel adapter translates response → `InteractionResponse`, unblocks the MCP handler
8. Agent receives result and continues working
9. Loop until agent exits or macro goal completes

#### Channel Adapters
- **`TerminalChannel`** (default): Renders interaction in the terminal, collects response via stdin. Ships with v0.4.1.1.
- **`AutoApproveChannel`**: Auto-approves all interactions for batch/CI mode.
- Future adapters (v0.5.3+): Slack, Discord, email, SMS, webhook — each implements `ReviewChannel` and is selected via config.

#### Standards Alignment
- NIST AI 600-1 (2.11 Human-AI Configuration): Humans respond through their preferred channel, not forced into terminal
- ISO 42001 (A.9.4 Communication): Communication channels are configurable and auditable

### v0.4.1.2 — Follow-Up Draft Continuity
<!-- status: done -->
**Goal**: `--follow-up` reuses the parent goal's staging directory by default, so iterative work accumulates into a single draft instead of creating disconnected packages.

> **Problem**: Today `--follow-up` creates a fresh staging copy. Each `ta draft build` produces a separate draft. When iterating on work (e.g., adding usage docs to a code draft), the user ends up with multiple drafts that must be applied separately. This breaks the "review everything together" mental model. Additionally, `build_package` blindly auto-supersedes the parent draft even when the follow-up uses separate staging and is **not** a superset of the parent's changes — orphaning the parent's work.

#### Default Behavior: Extend Existing Staging
When `--follow-up` detects the parent goal's staging directory still exists:
1. List open drafts from the parent goal (and any ancestors in the follow-up chain)
2. Prompt: `"Continue in staging for <parent_title>? [Y/n]"` — default yes, with the most recent draft shown
3. If yes: reuse the parent's staging directory, create a new goal linked to the same workspace
4. Next `ta draft build` diffs against the original source → produces a single unified draft that supersedes the previous one
5. Previous draft auto-transitions to `Superseded` status (valid here because new draft is a superset)

#### Standalone Option
If the user declines to extend:
- Fresh staging copy as today
- `ta draft build` produces an independent draft
- **No auto-supersede** — both drafts remain independently reviewable and appliable

#### Fix Auto-Supersede Logic
Current `build_package` unconditionally supersedes the parent draft on follow-up. Change to:
- **Same staging directory** (extend case): auto-supersede is correct — new draft is a superset
- **Different staging directory** (standalone case): do NOT auto-supersede — drafts are independent

#### Sequential Apply with Rebase
When multiple drafts target the same source and the user applies them in succession:
- Second `ta draft apply` detects the source has changed since its snapshot (first draft was just applied)
- Rebase-style merge: re-diffs staging against updated source, applies cleanly if no conflicts
- On conflict: same conflict resolution flow as existing `apply_with_conflict_check()`

#### Configuration
```yaml
# .ta/config.yaml
follow_up:
  default_mode: extend    # extend | standalone
  auto_supersede: true    # auto-supersede parent draft when extending (only when same staging)
  rebase_on_apply: true   # rebase sequential applies against updated source
```

#### Completed ✅
- `FollowUpConfig` added to `WorkflowConfig` in `crates/ta-submit/src/config.rs` (default_mode, auto_supersede, rebase_on_apply)
- `start_goal` detects parent staging and prompts to extend or create fresh copy
- `start_goal_extending_parent()` reuses parent workspace, source_dir, and source_snapshot
- `build_package` auto-supersede now checks `workspace_path` equality (same staging = supersede, different = independent)
- `apply_package` auto-close now checks `workspace_path` equality (only closes parent when same staging)
- Rebase-on-apply: `apply_package` re-snapshots source when source has changed and `rebase_on_apply` is configured

#### Tests (6 added, 463 total)
- ✅ Unit: follow-up detects parent staging, reuses workspace (`follow_up_extend_reuses_parent_staging`)
- ✅ Unit: parent staging missing returns None (`check_parent_staging_returns_none_when_staging_missing`)
- ✅ Unit: `ta draft build` after extend produces unified diff (`follow_up_extend_build_produces_unified_diff`)
- ✅ Unit: previous draft marked `Superseded` on new build, same staging (`follow_up_same_staging_supersedes_parent_draft`)
- ✅ Unit: follow-up with different staging does NOT supersede parent (`follow_up_different_staging_does_not_supersede_parent`)
- Note: sequential apply rebase and conflict detection are covered by the existing `apply_with_conflict_check` infrastructure + the new rebase-on-apply code path

### v0.4.2 — Behavioral Drift Detection
<!-- status: done -->
**Goal**: Detect when an agent's behavior patterns diverge from its historical baseline or declared alignment profile. Uses the decision reasoning data from v0.3.3 and alignment profiles from v0.4.0.

> **Why built-in, not AAP**: AAP's drift detection (`aap drift`) compares traces against self-declared alignment cards. TA's approach compares *actual enforced behavior* across goals — what resources an agent accesses, what kinds of changes it makes, how often it triggers escalation, what rejection rate it has. This is empirical, not declarative.

#### Drift Signals (computed from `ta-audit` event log)
- **Resource scope drift**: Agent accessing files/URIs outside its historical pattern (e.g., suddenly modifying CI configs when it normally only touches `src/`)
- **Escalation frequency change**: Significant increase/decrease in policy escalations may indicate changed behavior or stale manifest
- **Rejection rate drift**: If an agent's drafts start getting rejected more often, something changed
- **Change volume anomaly**: Unexpectedly large or small diffs compared to historical baseline
- **Dependency pattern shift**: Agent introducing new external dependencies at unusual rates

#### CLI
- `ta audit drift <agent-id>` — show drift report comparing recent N goals against historical baseline
- `ta audit drift --all` — drift summary across all agents
- `ta audit baseline <agent-id>` — compute and store behavioral baseline from historical data
- Warning integration: `ta draft build` optionally warns if current goal's behavior diverges from baseline

#### Data Model
```rust
pub struct BehavioralBaseline {
    pub agent_id: String,
    pub computed_at: DateTime<Utc>,
    pub goal_count: usize,      // Number of goals in baseline
    pub resource_patterns: Vec<String>,  // Typical URI patterns accessed
    pub avg_artifact_count: f64,
    pub avg_risk_score: f64,
    pub escalation_rate: f64,   // Fraction of actions triggering escalation
    pub rejection_rate: f64,    // Fraction of artifacts rejected by reviewers
}
```

#### Completed
- ✅ `BehavioralBaseline` data model with serde round-trip
- ✅ `DriftReport`, `DriftSignal`, `DriftSeverity`, `DriftFinding` types
- ✅ `BaselineStore` — JSON persistence in `.ta/baselines/<agent-id>.json`
- ✅ `compute_baseline()` — computes escalation rate, rejection rate, avg artifact count, avg risk score, resource patterns from audit events + draft summaries
- ✅ `compute_drift()` — five drift signals: resource scope, escalation frequency, rejection rate, change volume, dependency pattern
- ✅ `DraftSummary` bridge type to decouple `ta-audit` from `ta-changeset`
- ✅ `is_dependency_file()` helper for Cargo.toml, package.json, go.mod, etc.
- ✅ CLI: `ta audit drift <agent-id>` — show drift report vs baseline
- ✅ CLI: `ta audit drift --all` — drift summary across all agents
- ✅ CLI: `ta audit baseline <agent-id>` — compute and store baseline from history
- ✅ Version bump to 0.4.2-alpha across all crates

#### Tests (17 added, 482 total)
- ✅ Unit: `baseline_serialization_round_trip`
- ✅ Unit: `compute_baseline_empty_inputs`
- ✅ Unit: `compute_baseline_escalation_rate`
- ✅ Unit: `compute_baseline_draft_metrics`
- ✅ Unit: `compute_baseline_resource_patterns`
- ✅ Unit: `baseline_store_save_and_load_round_trip`
- ✅ Unit: `baseline_store_load_returns_none_when_missing`
- ✅ Unit: `baseline_store_list_agents`
- ✅ Unit: `drift_report_serialization_round_trip`
- ✅ Unit: `compute_drift_no_deviation`
- ✅ Unit: `compute_drift_escalation_spike`
- ✅ Unit: `compute_drift_novel_uris`
- ✅ Unit: `compute_drift_rejection_rate_jump`
- ✅ Unit: `compute_drift_volume_anomaly`
- ✅ Unit: `compute_drift_dependency_shift`
- ✅ Unit: `uri_prefix_extraction`
- ✅ Unit: `is_dependency_file_detection`
- ✅ Unit: `unique_agent_ids_extraction` (actually 18 drift tests, typo in count above — corrected)

#### Standards Alignment
- **NIST AI RMF MEASURE 2.6**: Monitoring AI system behavior for drift from intended purpose
- **ISO/IEC 42001 A.6.2.6**: Performance monitoring and measurement of AI systems
- **EU AI Act Article 9**: Risk management system with continuous monitoring

### v0.4.3 — Access Constitutions
<!-- status: done -->
**Goal**: Human-authorable or TA-agent-generated "access constitutions" that declare what URIs an agent should need to access to complete a given goal. Serves as a pre-declared intent contract — any deviation from the constitution is a behavioral drift signal.

> **Relationship to v0.4.0**: Alignment profiles describe an agent's *general* capability envelope. Access constitutions are *per-goal* — scoped to a specific task. An agent aligned for `src/**` access (v0.4.0 profile) might have a goal-specific constitution limiting it to `src/commands/draft.rs` and `crates/ta-submit/src/config.rs`.

- **Authoring**: Human writes constitution directly, or TA supervisor agent proposes one based on the goal objective + plan phase + historical access patterns
- **Format**: URI-scoped pattern list with intent annotations, stored alongside goal metadata
```yaml
# .ta/constitutions/goal-<id>.yaml
access:
  - pattern: "fs://workspace/src/commands/draft.rs"
    intent: "Add summary enforcement logic"
  - pattern: "fs://workspace/crates/ta-submit/src/config.rs"
    intent: "Add BuildConfig struct"
  - pattern: "fs://workspace/crates/ta-changeset/src/output_adapters/html.rs"
    intent: "Add disposition badges"
```
- **Enforcement**: At `ta draft build` time, compare actual artifacts against declared access constitution. Undeclared access triggers a warning (or error in strict mode).
- **Drift integration** (depends on v0.4.2): Constitution violations feed into the behavioral drift detection pipeline as a high-signal indicator.

#### Standards Alignment
- **IEEE 3152-2024**: Pre-declared intent satisfies transparency requirements for autonomous system actions
- **NIST AI RMF GOVERN 1.4**: Documented processes for mapping AI system behavior to intended purpose
- **EU AI Act Article 14**: Human oversight mechanism — constitution is a reviewable, pre-approved scope of action

#### Completed
- ✅ **Data model**: `AccessConstitution`, `ConstitutionEntry`, `EnforcementMode` types in `ta-policy::constitution` module with YAML/JSON serialization
- ✅ **Storage**: `ConstitutionStore` for `.ta/constitutions/goal-<id>.yaml` with load/save/list operations
- ✅ **Validation**: `validate_constitution()` function compares artifact URIs against declared access patterns using scheme-aware glob matching
- ✅ **Enforcement**: At `ta draft build` time, constitution is loaded and validated; violations trigger warning or error based on `EnforcementMode`
- ✅ **Drift integration**: New `ConstitutionViolation` drift signal added to `DriftSignal` enum in `ta-audit`; `constitution_violation_finding()` generates drift findings from undeclared access
- ✅ **CLI**: `ta goal constitution view|set|propose|list` subcommands for creating, viewing, and managing per-goal constitutions
- ✅ **Proposal**: `propose_constitution()` generates a constitution from agent baseline patterns for automated authoring
- ✅ **Agent identity**: `constitution_id` in `AgentIdentity` now populated with actual constitution reference when one exists

#### Tests (22 new, 504 total)
- ✅ Unit: `constitution_yaml_round_trip`, `constitution_json_round_trip`, `enforcement_mode_defaults_to_warning`, `enforcement_mode_display`
- ✅ Unit: `validate_all_declared_passes`, `validate_detects_undeclared_access`, `validate_detects_unused_entries`, `validate_explicit_uri_patterns`, `validate_scheme_mismatch_is_undeclared`, `validate_empty_constitution_flags_everything`, `validate_empty_artifacts_passes`
- ✅ Unit: `store_save_and_load_round_trip`, `store_load_returns_none_when_missing`, `store_list_goals`, `store_list_empty_dir`
- ✅ Unit: `pattern_matches_bare_path`, `pattern_matches_glob`, `pattern_matches_explicit_uri`
- ✅ Unit: `propose_from_historical_patterns`
- ✅ Unit: `constitution_violation_finding_none_when_empty`, `constitution_violation_finding_warning_for_few`, `constitution_violation_finding_alert_for_majority`, `constitution_violation_signal_serialization`

### v0.4.4 — Interactive Session Completion
<!-- status: done -->
**Goal**: Complete the `ta run --interactive` experience so users can inject mid-session guidance while the agent works.

> **Note**: The core of this phase is now **absorbed by v0.4.1.1** (ReviewChannel Architecture). The `ReviewChannel` trait with `TerminalChannel` provides the bidirectional human-agent communication loop, including mid-session guidance, pause/resume (channel disconnect/reconnect), and audit-logged interactions. What remains here are the PTY-specific enhancements for real-time agent output streaming.

- ✅ **PTY capture**: Wrap agent subprocess in a PTY so output streams to the terminal in real-time while TA captures it for session history
- ✅ **Stdin interleaving**: User types guidance mid-session → TA routes it via `ReviewChannel` (replaces direct stdin injection)
- ✅ **Guidance logged**: All human injections recorded as `InteractionRequest`/`InteractionResponse` pairs with timestamps
- ✅ **Pause/resume**: `ReviewChannel` disconnect = pause, reconnect = resume. `ta run --resume <session-id>` reattaches to a running session.
- ✅ **Integration with `ta draft fix`** (v0.3.4): During interactive review, pause → `ta draft fix` → resume through the same channel

> **Depends on**: v0.4.1.1 (ReviewChannel + TerminalChannel). Remaining scope after v0.4.1.1 is PTY wrapping for real-time output streaming — the interaction protocol is handled by ReviewChannel.

### v0.4.5 — CLI UX Polish
<!-- status: done -->
**Goal**: Quality-of-life improvements across all CLI commands.

- ✅ **Partial ID matching**: Accept 8+ character UUID prefixes in all `ta draft`, `ta goal`, and `ta session` commands (currently requires full UUID)
- ✅ **Apply on PendingReview**: `ta draft apply` works directly on PendingReview drafts without requiring a separate `ta draft approve` first (auto-approves on apply)
- ✅ **Terminal encoding safety**: Ensure disposition badges and status markers render cleanly in all terminal encodings (no garbled characters)
- ✅ **Plan phase in `ta release run`**: Accept plan phase IDs (e.g., `0.4.1.2`) and auto-convert to semver release versions (`0.4.1-alpha.2`) via configurable `version_policy` in `.ta/release.yaml`. Strip `v` prefix if provided.

---

## v0.5 — MCP Interception & External Actions *(release: tag v0.5.0-alpha)*

> **Architecture shift**: Instead of building custom connectors per service (Gmail, Drive, etc.),
> TA intercepts MCP tool calls that represent state-changing actions. MCP servers handle the
> integration. TA handles the governance. Same pattern as filesystem: hold changes at a
> checkpoint, replay on apply.

### v0.5.0 — Credential Broker & Identity Abstraction
<!-- status: done -->
**Prerequisite for all external actions**: Agents must never hold raw credentials. TA acts as an identity broker — agents request access, TA provides scoped, short-lived session tokens.

- **Credential vault**: TA stores OAuth tokens, API keys, database credentials in an encrypted local vault (age/sops or OS keychain integration). Agents never see raw secrets.
- **Scoped session tokens**: When an agent needs to call an MCP server that requires auth, TA issues a scoped bearer token with: limited TTL, restricted actions (read-only vs read-write), restricted resources (which mailbox, which DB table)
- **OAuth broker**: For services that use OAuth (Gmail, Slack, social media), TA handles the OAuth flow. Agent receives a session token that TA proxies to the real OAuth token. Token refresh is TA's responsibility, not the agent's.
- **SSO/SAML integration**: Enterprise users can connect TA to their SSO provider. Agent sessions inherit the user's identity but with TA-scoped restrictions.
- **Credential rotation**: TA can rotate tokens without agent awareness. Agent's session token stays valid; TA maps it to new real credentials.
- **Audit**: Every credential issuance logged — who (which agent), what (which service, which scope), when, for how long.

```yaml
# .ta/credentials.yaml (encrypted at rest)
services:
  gmail:
    type: oauth2
    provider: google
    scopes: ["gmail.send", "gmail.readonly"]
    token_ttl: 3600
  plaid:
    type: api_key
    key_ref: "keychain://ta/plaid-production"
    agent_scope: read_only  # agents can read transactions but not initiate transfers
```

### v0.5.1 — MCP Tool Call Interception
<!-- status: done -->
**Core**: Intercept outbound MCP tool calls that change external state. Hold them in the draft as pending actions. Replay on apply.

- **MCP action capture**: When an agent calls an MCP tool (e.g., `gmail_send`, `slack_post`, `tweet_create`), TA intercepts the call, records the tool name + arguments + timestamp in the draft as a `PendingAction`
- **Action classification**: Read-only calls (search, list, get) pass through immediately. State-changing calls (send, post, create, update, delete) are captured and held
- **Draft action display**: `ta draft view` shows pending actions alongside file artifacts — "Gmail: send to alice@example.com, subject: Q3 Report" with full payload available at `--detail full`
- **Selective approval**: Same `--approve`/`--reject` pattern works for actions. URI scheme distinguishes them: `mcp://gmail/send`, `mcp://slack/post_message`, etc.
- **Apply = replay**: `ta draft apply` replays approved MCP calls against the live MCP server (using credentials from the vault, never exposed to agent). Failed replays reported with retry option.
- **Bundled MCP server configs**: Ship default configs for common MCP servers (Google, Slack, Discord, social media, databases). User runs `ta setup connect gmail` → OAuth flow → credentials stored → MCP server config generated.
- **Data model**: `DraftPackage.changes` gains `pending_actions: Vec<PendingAction>` alongside existing `artifacts` and `patch_sets`

```rust
pub struct PendingAction {
    pub action_uri: String,           // mcp://server/tool_name
    pub tool_name: String,            // Original MCP tool name
    pub arguments: serde_json::Value, // Captured arguments (credentials redacted)
    pub captured_at: DateTime<Utc>,
    pub disposition: ArtifactDisposition,
    pub summary: String,              // Human-readable description
    pub reversible: bool,             // Can this action be undone?
    pub estimated_cost: Option<f64>,  // API call cost estimate if applicable
}
```

#### What TA does NOT build
- No Gmail API client. No Slack bot. No Twitter SDK. The MCP servers handle all service-specific logic.
- TA only adds: credential brokering, interception, capture, display, approval, replay.

### v0.5.2 — Minimal Web Review UI
<!-- status: done -->
**Goal**: A single-page web UI served by `ta daemon` at localhost for draft review and approval. Unblocks non-CLI users.

- **Scope**: View draft list, view draft detail (same as `ta draft view`), approve/reject/comment per artifact and per action. That's it.
- **Implementation**: Static HTML + minimal JS. No framework. Calls TA daemon's JSON API.
- **Auth**: Localhost-only by default. Optional token auth for LAN access.
- **Foundation**: This becomes the shell that the full web app (v0.9) fills in.

### v0.5.3 — Additional ReviewChannel Adapters
<!-- status: done -->
> Moved up from v0.10 — non-dev users need notifications from day one of MCP usage.

> **Architecture note**: These are implementations of the `ReviewChannel` trait from v0.4.1.1, not a separate notification system. Every interaction point (draft review, approval, plan negotiation, escalation) flows through the same trait — adding a channel adapter means all interactions work through that medium automatically.

- **SlackChannel**: Block Kit cards for draft review, button callbacks for approve/reject/discuss, thread-based discussion
- **DiscordChannel**: Embed PR summaries, reaction-based approval, slash command for detailed view
- **EmailChannel**: SMTP-based summaries, IMAP reply parsing for approve/reject
- **WebhookChannel**: POST `InteractionRequest` to URL, await callback with `InteractionResponse`
- Unified config: `review.channel` in `.ta/config.yaml` (replaces `notification_channel`)
- Non-interactive approval API: token-based approval for bot callbacks (Slack buttons, email replies)

#### Standards Alignment
- **EU AI Act Article 50**: Transparency — humans see exactly what the agent wants to do before it happens
- **ISO/IEC 42001 A.10.3**: Third-party AI component management via governance wrapper

### v0.5.4 — Context Memory Store (ruvector integration)
<!-- status: done -->
**Goal**: Agent-agnostic persistent memory that works across agent frameworks. When a user switches from Claude Code to Codex mid-project, or runs multiple agents in parallel, context doesn't get lost. TA owns the memory — agents consume it.

> **Problem today**: Each agent framework has its own memory (Claude Code's CLAUDE.md/project memory, Codex's session state, Cursor's codebase index). None of it transfers. TA currently relies on "agent-native mechanisms" for session resume, which means TA has no control over context persistence. A user who switches agents mid-goal starts from scratch.

#### Core: `MemoryStore` trait + ruvector backend

```rust
/// Agent-agnostic memory store. TA owns the memory; agents read/write through it.
pub trait MemoryStore: Send + Sync {
    /// Store a memory entry with semantic embedding for retrieval.
    fn store(&self, entry: MemoryEntry) -> Result<MemoryId>;
    /// Retrieve entries semantically similar to a query.
    fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    /// Retrieve entries by exact key or tag.
    fn lookup(&self, key: &str) -> Result<Option<MemoryEntry>>;
    /// List entries for a goal, agent, or session.
    fn list(&self, filter: MemoryFilter) -> Result<Vec<MemoryEntry>>;
    /// Delete or expire entries.
    fn forget(&self, id: MemoryId) -> Result<()>;
}

pub struct MemoryEntry {
    pub id: MemoryId,
    pub content: String,              // The actual memory (text, structured data, etc.)
    pub context: MemoryContext,       // Where this came from (goal, agent, session)
    pub tags: Vec<String>,            // User or agent-applied labels
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: MemorySource,         // AgentOutput, HumanGuidance, GoalResult, DraftReview
}

pub enum MemorySource {
    AgentOutput { agent_id: String, session_id: Uuid },
    HumanGuidance { session_id: Uuid },
    GoalResult { goal_id: Uuid, outcome: GoalOutcome },
    DraftReview { draft_id: Uuid, decision: String },
    SystemCapture,  // TA auto-extracted
}
```

#### Backends (pluggable via trait)
- **Filesystem (default, zero-dep)**: JSON files in `.ta/memory/`. Exact-match lookup only. Ships immediately, no extra dependencies. Sufficient for small projects.
- **ruvector (recommended)**: Rust-native vector database with HNSW indexing. Sub-millisecond semantic recall. Enables "find memories similar to this problem" across thousands of entries. Added as optional cargo feature: `ta-cli --features ruvector`.
  - [ruvector](https://github.com/ruvnet/ruvector): Rust-native, 61μs p50 latency, SIMD-optimized, self-learning GNN layer
  - Local-first — no external service required
  - Embedding generation: use agent LLM or local model (ONNX runtime) for vector generation

#### CLI surface
```bash
ta context store "Always use tempfile::tempdir() for test fixtures"  # manual memory
ta context recall "how do we handle test fixtures"                   # semantic search
ta context list --goal <id>                                          # list by scope
ta context forget <id>                                               # delete entry
```

#### Automatic capture (opt-in per workflow)
- On goal completion: extract "what worked" patterns from approved drafts
- On draft rejection: store rejection reason + what the agent tried (learn from mistakes)
- On human guidance during interactive session: store as reusable context
- On repeated corrections: auto-promote to persistent memory ("user always wants X")

#### How agents consume memory
- **Context injection**: When `ta run` launches an agent, TA queries the memory store for relevant entries and injects them into the agent's context (CLAUDE.md injection, system prompt, or MCP tool).
- **MCP tool**: `ta_memory_recall` MCP tool lets agents query memory mid-session. "Have I solved something like this before?"
- **Agent-agnostic**: Same memory available to Claude Code, Codex, Cursor, or any agent. Switch agents without losing context.

#### Design decisions to resolve before implementation
1. **Embedding model**: Use the goal's agent LLM for embeddings (adds API cost per memory op) vs ship a small local model (ONNX, ~50MB). Recommend: local model for embeddings, LLM only for extraction.
2. **Memory scope**: Per-project (`.ta/memory/`) vs global (`~/.config/ta/memory/`). Recommend: per-project by default, global opt-in for cross-project patterns.
3. **Conflict on shared memory**: If two agents write contradictory memories, which wins? Recommend: timestamp-based, human arbitrates via `ta context list --conflicts`.
4. **ruvector maturity**: Evaluate production-readiness before committing. Fallback to filesystem backend must always work.
5. **Binary size**: ruvector adds ~2-5MB to the binary. Acceptable for desktop; may matter for cloud/edge.

#### Forward-looking: where memory feeds later phases

| Phase | How it uses memory |
|-------|-------------------|
| **v0.6.0 Supervisor** | Query past approve/reject decisions to inform auto-approval. "Last 5 times the agent modified CI config, the human rejected 4 of them" → escalate. |
| **v0.6.1 Cost tracking** | Remember which agent/prompt patterns are cost-efficient vs wasteful. |
| **v0.7.0 Guided setup** | Remember user preferences from past setup sessions. "User prefers YAML configs" → skip the config format question. |
| **v0.8.1 Community memory** | ruvector becomes the backing store. Local → shared is just a sync layer on top. |
| **v0.4.2 Drift detection** | Store agent behavioral baselines as vectors. Detect when new behavior deviates from learned patterns. |
| **v1.0 Virtual office** | Role-specific memory: "the code reviewer role remembers common review feedback for this codebase." |

### v0.5.5 — RuVector Memory Backend
<!-- status: done -->
**Goal**: Replace the filesystem JSON backend with [ruvector](https://github.com/ruvnet/ruvector) for semantic search, self-learning retrieval, and sub-millisecond recall at scale. The `MemoryStore` trait stays the same — this is a backend swap behind a cargo feature flag.

> **Why now**: v0.5.4 shipped the `MemoryStore` trait and `FsMemoryStore` backend. That's sufficient for key-value recall by exact match or prefix. But the real value of persistent memory is *semantic retrieval* — "find memories similar to this problem" — which requires vector embeddings and approximate nearest-neighbor search. ruvector provides this in pure Rust with zero external services.

#### Implementation

- **New file**: `crates/ta-memory/src/ruvector_store.rs` — `RuVectorStore` implementing `MemoryStore`
- **Cargo feature**: `ruvector` in `crates/ta-memory/Cargo.toml`, optional dependency on `ruvector` crate
- **Trait extension**: Add `semantic_search(&self, query: &str, k: usize) -> Result<Vec<MemoryEntry>>` to `MemoryStore` (with default no-op impl for `FsMemoryStore`)
- **Embedding pipeline**: On `store()`, generate a vector embedding from the value. Options:
  1. Use ruvector's built-in SONA engine for zero-config embeddings
  2. Use agent LLM as embedding source (higher quality, adds API cost)
  3. Ship a small local ONNX model (~50MB) for offline embeddings
  Decision: Start with ruvector's native embeddings; add LLM embeddings as opt-in.
- **HNSW index**: ruvector's HNSW indexing provides O(log n) semantic recall vs O(n) filesystem scan
- **Self-learning**: ruvector's GNN layer improves search quality over time as agents store/query context — no explicit retraining needed
- **Storage format**: Single `.rvf` cognitive container file at `.ta/memory.rvf` (replaces JSON directory)
- **Migration**: Auto-import existing `.ta/memory/*.json` entries on first run when `ruvector` feature is enabled

#### Config

```toml
# .ta/workflow.toml
[memory]
backend = "ruvector"      # "filesystem" (default) or "ruvector"
embedding_model = "sona"  # "sona" (built-in), "local-onnx", or "llm"
# ruvector_path = ".ta/memory.rvf"  # default
```

#### CLI changes
```bash
# Semantic search (only available with ruvector backend)
ta context recall "how do we handle authentication" --semantic

# Existing exact-match still works
ta context recall "auth-token-pattern"  # exact key match
```

#### Tests (minimum 8)
Store/recall round-trip, semantic search returns relevant results, self-learning improves ranking after repeated queries, migration from filesystem, feature-flag gating (fs-only build still compiles), concurrent access safety, HNSW index rebuild, empty-store search returns empty.

#### Completed
- ✅ `crates/ta-memory/src/ruvector_store.rs` — `RuVectorStore` implementing `MemoryStore` with all trait methods + `semantic_search`
- ✅ `ruvector` cargo feature in `crates/ta-memory/Cargo.toml` — optional `ruvector-core` v2.0.5 dependency
- ✅ `semantic_search()` added to `MemoryStore` trait with default no-op for `FsMemoryStore`
- ✅ Hash-based embeddings (FNV-1a n-gram + cosine similarity) — zero-config, pure Rust
- ✅ HNSW indexing via `ruvector-core::VectorDB` with persistent `.rvf` storage
- ✅ Auto-migration from `.ta/memory/*.json` to ruvector on first use
- ✅ `ta context recall "query" --semantic` CLI flag with `--limit`
- ✅ Feature-flag gating — `cargo build` without `ruvector` feature works (fs-only)
- ✅ `ruvector` feature forwarded from `ta-cli` Cargo.toml
- ✅ 10 ruvector tests: roundtrip, semantic search, overwrite, forget, list, empty search, migration, lookup by tag, concurrent access, forget-nonexistent
- ✅ Bug fix: macro session exit no longer errors when goal already applied/submitted via MCP

### v0.5.6 — Framework-Agnostic Agent State
<!-- status: done -->
**Goal**: Use TA's memory store as the canonical source of project state so users can switch between agentic frameworks (Claude Code, Codex, Cursor, Claude Flow, etc.) across tasks — or run them simultaneously — without losing context or locking into any framework's native state management.

> **Problem today**: Each framework keeps its own state. Claude Code has CLAUDE.md and project memory. Codex has session state. Cursor has codebase indices. None of it transfers. When you switch agents mid-project, the new agent starts cold — it doesn't know what the previous agent learned, what conventions the human established, or what approaches were tried and rejected.

> **TA's advantage**: TA already wraps every agent framework. It sees every goal, every draft, every approval, every rejection. It can capture this knowledge into the memory store and inject it into *any* agent's context on the next run, regardless of framework.

#### Automatic state capture (opt-in per workflow)

```toml
# .ta/workflow.toml
[memory.auto_capture]
on_goal_complete = true    # Extract "what worked" patterns from approved drafts
on_draft_reject = true     # Store rejection reason + what the agent tried (learn from mistakes)
on_human_guidance = true   # Store human feedback from interactive sessions
on_repeated_correction = true  # Auto-promote to persistent memory ("user always wants X")
```

Capture events:
- **Goal completion** → extract working patterns, conventions discovered, successful approaches
- **Draft rejection** → record what was tried, why it failed, what the human said — prevents repeating mistakes
- **Human guidance** → "always use tempfile::tempdir()" becomes persistent knowledge, not session-ephemeral
- **Repeated corrections** → if the human corrects the same pattern 3 times, TA auto-stores it as a persistent preference

#### Context injection on agent launch

When `ta run` launches any agent, TA:
1. Queries the memory store for entries relevant to the goal title, objective, and affected file paths
2. Ranks by relevance (semantic if ruvector, tag-match if filesystem)
3. Injects top-K entries into the agent's context:
   - For Claude Code: appended to CLAUDE.md injection
   - For Codex: included in system prompt
   - For custom agents: available via `ta_context` MCP tool at session start
4. The agent sees unified project knowledge regardless of which agent produced it

#### MCP tool: `ta_context` (already exists from v0.5.4)

Extended with framework metadata:
```bash
# Agent stores a convention it discovered
ta_context store --key "test-conventions" \
  --value '{"pattern": "Use tempfile::tempdir() for all filesystem tests"}' \
  --tags "convention,testing" \
  --source "claude-code:goal-abc123"

# Different agent recalls it in a later session
ta_context recall "test-conventions"
# → Returns the entry regardless of which agent stored it
```

#### State categories

| Category | Example | Capture trigger |
|----------|---------|----------------|
| **Conventions** | "Use 4-space indent", "Always run clippy" | Human guidance, repeated corrections |
| **Architecture** | "Auth module is in src/auth/", "Uses JWT not sessions" | Goal completion, draft review |
| **History** | "Tried Redis caching, rejected — too complex for MVP" | Draft rejection |
| **Preferences** | "Human prefers small PRs", "Never auto-commit" | Repeated human behavior patterns |
| **Relationships** | "config.toml depends on src/config.rs" | Draft dependency analysis |

#### Tests (minimum 6)
Auto-capture on goal complete, auto-capture on rejection, context injection into CLAUDE.md, context injection via MCP tool, cross-framework recall (store from "claude-code", recall from "codex"), repeated-correction auto-promotion.

#### Completed
- ✅ `MemoryCategory` enum (convention, architecture, history, preference, relationship, other)
- ✅ `StoreParams` with `goal_id` and `category` — `store_with_params()` on `MemoryStore` trait
- ✅ `AutoCaptureConfig` parsed from `.ta/workflow.toml` `[memory.auto_capture]` section
- ✅ `AutoCapture` event handlers: `on_goal_complete`, `on_draft_reject`, `on_human_guidance`, `check_repeated_correction`
- ✅ `build_memory_context_section()` for CLAUDE.md injection from prior sessions
- ✅ `ta_context` MCP tool extended: `source`, `goal_id`, `category` params; new `search` action
- ✅ Draft submit wired: PrApproved/PrDenied events dispatched, rejection auto-captured to memory
- ✅ `ta run` context injection: memory context section injected into CLAUDE.md at launch
- ✅ `ta run` auto-capture: goal completion + change_summary captured after draft build
- ✅ Tests: auto_capture_goal_complete, auto_capture_draft_rejection, context_injection_builds_markdown_section, cross_framework_recall, repeated_correction_auto_promotes, config_parsing_from_toml, config_defaults_when_no_section, disabled_capture_is_noop, slug_generation (9 new tests, 18 total in ta-memory)

### v0.5.7 — Semantic Memory Queries & Memory Dashboard
<!-- status: done -->
**Goal**: Rich querying and visualization of the memory store. Enables users to audit what TA has learned, curate memory entries, and understand how memory influences agent behavior.

**Completed**:
- ✅ `ta context search "query"` — dedicated semantic search CLI command
- ✅ `ta context similar <entry-id>` — find entries similar to a given entry by ID
- ✅ `ta context explain <key-or-id>` — show provenance chain (source, goal, category, timestamps, confidence)
- ✅ `ta context stats` — memory store statistics (total, by category, by source, avg confidence, expired count)
- ✅ `ta context store --expires-in 30d --confidence 0.9 --category convention` — TTL + confidence + category on store
- ✅ `ta context list --category convention` — filter by category
- ✅ `MemoryEntry.expires_at` — optional TTL field with duration parsing (d/h/m)
- ✅ `MemoryEntry.confidence` — 0.0–1.0 score; approved drafts default to 1.0, auto-captured to 0.5–0.8
- ✅ `MemoryStats` struct with total_entries, by_category, by_source, expired_count, avg_confidence, oldest/newest
- ✅ `MemoryStore.stats()` trait method with default implementation
- ✅ `MemoryStore.find_by_id(uuid)` trait method for UUID lookups
- ✅ Web UI Memory tab: `/memory` with browse, search, create, delete, stats dashboard
- ✅ Web API: `GET /api/memory`, `GET /api/memory/search?q=`, `GET /api/memory/stats`, `POST /api/memory`, `DELETE /api/memory/:key`
- ✅ MCP `ta_context` tool: new `stats` and `similar` actions
- ✅ Confidence scoring on auto-capture: goal_complete=0.8, draft_reject=0.6, human_guidance=0.9, auto-promoted=0.9
- ✅ 3 new web UI tests (memory_list_empty, memory_stats_empty, memory_create_and_list)
- ✅ Backward-compatible: `expires_at` and `confidence` fields use `#[serde(default)]` — old entries deserialize fine

**Deferred to future**:
- Conflict resolution (`ta context conflicts`, `ta context resolve`) — needs a conflict detection heuristic
- Usage analytics (recall frequency tracking) — needs MCP middleware instrumentation

---

## v0.6 — Platform Substrate *(release: tag v0.6.0-alpha)*

> **Architecture**: See `docs/ADR-product-concept-model.md` for the 5-layer model driving these phases.
> TA is a governance infrastructure platform. v0.6 completes the substrate that projects (Virtual Office, Infra Ops) build on.

### v0.6.0 — Session & Human Control Plane (Layer 3)
<!-- status: done -->
**Goal**: The TA Session — a continuous conversation between the human and TA about a goal. TA is invisible to the agent framework. The agent works, exits, and TA captures the result.

> **Key insight**: The human control plane is TA's most distinctive feature. The agent does not call TA — TA observes, diffs, and mediates. Session commands that agents cannot see are the safety boundary.

> **Design principle**: TA is a Rust daemon, not an LLM. It launches agent frameworks as subprocesses, mediates resource access, and builds drafts from workspace diffs when the agent exits.

**Completed**:
- ✅ **`TaSession`**: Core session object with `session_id`, `goal_id`, `agent_id`, `state` (SessionState enum), `conversation` (Vec<ConversationTurn>), `pending_draft`, `iteration_count`, `checkpoint_mode`
- ✅ **New crate: `ta-session`**: Session lifecycle with `TaSession`, `SessionState` (Starting → AgentRunning → DraftReady → WaitingForReview → Iterating → Completed → Aborted → Paused → Failed), `ConversationTurn`, `SessionManager`, `SessionError`
- ✅ **SessionManager**: CRUD persistence in `.ta/sessions/<id>.json` with `create()`, `load()`, `save()`, `find_for_goal()`, `list()`, `list_active()`, `pause()`, `resume()`, `abort()`, `delete()`
- ✅ **Human control plane commands**: `ta session status`, `ta session pause <id>`, `ta session resume <id>`, `ta session abort <id>`
- ✅ **SessionEvent variants**: `SessionPaused`, `SessionResumed`, `SessionAborted`, `DraftBuilt`, `ReviewDecision`, `SessionIteration` added to `TaEvent` enum with helper constructors
- ✅ **Checkpoint mode**: `with_checkpoint_mode()` builder on TaSession
- ✅ **Conversational continuity**: `ConversationTurn` tracks agent_context, human_feedback, draft_id per iteration
- ✅ **20 ta-session tests**, 4 new ta-goal event tests

**Remaining (deferred)**:
- Change rationale field in `change_summary.json` (needs draft viewer integration)
- Full agent subprocess lifecycle management (launch, signal, relaunch with feedback)

### v0.6.1 — Unified Policy Config (Layer 2)
<!-- status: done -->
**Goal**: All supervision configuration resolves to a single `PolicyDocument` loaded from `.ta/policy.yaml`.

**Completed**:
- ✅ **PolicyDocument**: Unified config struct with `version`, `defaults` (PolicyDefaults), `schemes` (HashMap<String, SchemePolicy>), `escalation` (EscalationConfig), `agents` (HashMap<String, AgentPolicyOverride>), `security_level`, `budget` (BudgetConfig)
- ✅ **PolicyCascade**: 6-layer tighten-only merge: built-in defaults → `.ta/policy.yaml` → `.ta/workflows/<name>.yaml` → `.ta/agents/<agent>.policy.yaml` → `.ta/constitutions/goal-<id>.yaml` → CLI overrides
- ✅ **`.ta/policy.yaml`**: YAML-serializable config surface with `defaults`, `schemes`, `escalation`, `agents` sections
- ✅ **PolicyContext**: Runtime context with `goal_id`, `session_id`, `agent_id`, `budget_spent`, `action_count`, `drift_score`; methods for `is_over_budget()`, `is_budget_warning()`, `is_drifting()`
- ✅ **Security levels**: `SecurityLevel` enum with Ord: Open < Checkpoint (default) < Supervised < Strict
- ✅ **PolicyEnforcement**: Warning < Error < Strict enforcement modes
- ✅ **`evaluate_with_document()`**: New method on PolicyEngine layering document-level checks (scheme approval, agent overrides, drift escalation, action limits, budget limits, supervised mode)
- ✅ **Cost tracking**: BudgetConfig with `max_tokens_per_goal` and `warn_at_percent` (default 80%)
- ✅ **24 new tests** across document.rs (8), context.rs (6), cascade.rs (10) + 5 engine integration tests

**Remaining (deferred)**:
- Supervisor agent verification (needs agent runtime integration)
- "TA supervises TA" pattern (needs supervisor config draft flow)

### v0.6.2 — Resource Mediation Trait (Layer 1)
<!-- status: done -->
**Goal**: Generalize the staging pattern from files to any resource.

**Completed**:
- ✅ **New crate: `ta-mediation`**: `ResourceMediator` trait with `scheme()`, `stage()`, `preview()`, `apply()`, `rollback()`, `classify()` methods
- ✅ **Core types**: `ProposedAction`, `StagedMutation`, `MutationPreview`, `ActionClassification` (ReadOnly < StateChanging < Irreversible < ExternalSideEffect), `ApplyResult`
- ✅ **`FsMediator`**: Implements `ResourceMediator` for `fs://` URIs — stage writes to staging dir, preview generates diffs, apply copies to source, rollback removes staged
- ✅ **`MediatorRegistry`**: Routes URIs to mediators by scheme with `register()`, `get()`, `route()`, `schemes()`, `has_scheme()`
- ✅ **22 ta-mediation tests** (5 mediator, 9 fs_mediator, 8 registry)

**Remaining (deferred)**:
- `.ta/config.yaml` mediators section (needs config system)
- Output alignment with DraftPackage.changes (needs draft builder integration)

### v0.6.3 — Active Memory Injection & Project-Aware Key Schema
<!-- status: done -->
**Goal**: Agents start smart. Instead of spending hours exploring the codebase, `ta run` injects structured architectural knowledge, conventions, negative paths, and project state from the memory store into the agent's context. Keys are project-aware (auto-detected from project type) and phase-tagged.

> **Problem today**: Memory captures lifecycle events (goal completions, rejections) but not active project state. Agents launched via `ta run` still spend extensive time re-discovering crate maps, trait signatures, coding patterns, and module relationships that previous sessions already established.

> **Design**: See `docs/ADR-active-memory-injection.md` (to be written from the design in claude memory). Full design covers key schema, auto-detection, injection logic, and RuVector default-on.

#### Project-Aware Key Schema

Keys use `{domain}:{topic}` where the domain is derived from auto-detected project type:

| Project Type | Detection Signal | `module_map` key | `type_system` key |
|---|---|---|---|
| `rust-workspace` | `Cargo.toml` with `[workspace]` | `arch:crate-map` | `arch:trait:*` |
| `typescript` | `package.json` + `tsconfig.json` | `arch:package-map` | `arch:interface:*` |
| `python` | `pyproject.toml` or `setup.py` | `arch:module-map` | `arch:protocol:*` |
| `go` | `go.mod` | `arch:package-map` | `arch:interface:*` |
| `generic` | fallback | `arch:component-map` | `arch:type:*` |

Configurable via `.ta/memory.toml` (optional — auto-detected defaults when absent):

```toml
[project]
type = "rust-workspace"

[key_domains]
module_map = "crate-map"
module = "crate"
type_system = "trait"
build_tool = "cargo"
```

#### New MemoryCategory Variants

- `NegativePath` — approaches tried and failed, with context on why (prevents agents from repeating mistakes)
- `State` — mutable project state snapshots (plan progress, dependency graphs, file structure)

#### Phase Tagging

New `phase_id: Option<String>` field on `MemoryEntry` and `StoreParams`. Abstract string (not coupled to semver) — works with any versioning scheme. Auto-populated from `GoalRun.plan_phase` during auto-capture.

#### Enhanced Injection (`build_memory_context_section`)

1. Filter by phase: entries matching current phase or global (`phase_id: None`)
2. Category priority: Architecture > NegativePath > Convention > State > History
3. Semantic ranking via RuVector (enabled by default)
4. Structured markdown output (sections per category, not flat list)

#### Enhanced Auto-Capture

- **On goal completion (enhanced)**: Extract architectural knowledge — key types, file layout, module boundaries — not just change summary blob
- **On draft rejection (enhanced)**: Create `neg:{phase}:{slug}` entries as negative paths
- **New: On human guidance (enhanced)**: Auto-classify into domains using key mapping

#### RuVector Default-On

- `ruvector` feature flag enabled by default in `ta-memory/Cargo.toml`
- `build_memory_context_section()` semantic search as primary path, tag-based fallback
- Config toggle: `.ta/memory.toml` → `backend = "ruvector"` (default) or `backend = "fs"`

#### Implementation Scope

New/modified files:
- `crates/ta-memory/src/store.rs` — `NegativePath`, `State` categories; `phase_id` on MemoryEntry/StoreParams
- `crates/ta-memory/src/auto_capture.rs` — enhanced event handlers, architectural knowledge extraction
- `crates/ta-memory/src/key_schema.rs` — NEW: project type detection, domain mapping, key resolution
- `crates/ta-memory/src/lib.rs` — re-exports, ruvector default feature
- `crates/ta-memory/Cargo.toml` — ruvector feature default-on
- `apps/ta-cli/src/commands/run.rs` — enhanced injection with phase-aware queries, structured output
- `apps/ta-cli/src/commands/context.rs` — `ta context schema` to inspect key mapping
- `.ta/memory.toml` — new config file format (optional, auto-detected defaults)

#### Tests (minimum 8)
- Project type auto-detection (Rust workspace, TypeScript, Python, fallback)
- Key schema resolution with custom `.ta/memory.toml`
- Phase-filtered injection (current phase + global entries)
- Category-prioritized injection order
- NegativePath entry creation from draft rejection
- Architectural knowledge extraction from goal completion
- RuVector semantic search as primary injection path
- Backward compatibility (old entries without phase_id work)

#### Completed ✅
- ✅ `NegativePath` and `State` MemoryCategory variants added to `store.rs`
- ✅ `phase_id: Option<String>` added to `MemoryEntry`, `StoreParams`, `MemoryQuery`
- ✅ Phase-aware filtering in `FsMemoryStore` and `RuVectorStore` lookup
- ✅ `key_schema.rs` — project type detection (Rust, TS, Python, Go, Generic), `KeyDomainMap`, `.ta/memory.toml` config parsing, key generation helpers
- ✅ `build_memory_context_section_with_phase()` — phase-filtered, category-prioritized, structured markdown output
- ✅ Draft rejection auto-capture uses `NegativePath` category with `neg:{phase}:{slug}` keys
- ✅ Goal completion auto-capture extracts architectural module map from `change_summary`
- ✅ `build_memory_context_section_for_inject()` uses RuVector backend when available, passes `plan_phase` for filtering
- ✅ `ta context schema` CLI subcommand to inspect key domain mapping
- ✅ `ruvector` feature flag default-on in `ta-memory/Cargo.toml`
- ✅ Version bumped to `0.6.3-alpha`
- ✅ 10 new tests (5 in key_schema.rs, 5 in auto_capture.rs) covering all 8 required scenarios

#### Remaining — moved to v0.7.4

---

## v0.7 — Extensibility *(release: tag v0.7.0-alpha)*

> TA becomes extensible: pluggable IO channels, non-file mediators, and the event subscription API.

### v0.7.0 — Channel Registry (Layer 5)
<!-- status: done -->
**Goal**: Pluggable IO channel system where all channels (CLI, web, Slack, Discord, email) are equal.

- **`ChannelFactory` trait**: `build_review() → Box<dyn ReviewChannel>`, `build_session() → Box<dyn SessionChannel>`, `capabilities()`.
- **`ChannelRegistry`**: HashMap of channel type → factory. Channels register at startup.
- **Channel routing config** (`.ta/config.yaml`):
  ```yaml
  channels:
    review: { type: slack, channel: "#reviews" }
    notify: [{ type: terminal }, { type: slack, level: warning }]
    session: { type: terminal }
    escalation: { type: email, to: "mgr@co.com" }
  ```
- **Default agent per channel**: Channels can set `default_agent` and `default_workflow` for routing.
- **First plugin: `ta-channel-slack`** — Slack integration for review notifications, approval buttons, and session streaming.
- **Webhook improvements**: Signature verification, retry logic, structured payloads.

#### Completed

- ✅ `ChannelFactory` trait with `channel_type()`, `build_review()`, `build_session()`, `capabilities()`
- ✅ `ChannelRegistry` with `register()`, `get()`, `build_review_from_config()`, `build_session_from_config()`
- ✅ `ChannelCapabilitySet` (supports_review, supports_session, supports_notify, supports_rich_media, supports_threads)
- ✅ Channel routing config types: `ChannelRoutingConfig`, `ChannelRouteConfig`, `NotifyRouteConfig`, `TaConfig`
- ✅ `.ta/config.yaml` loader with `load_config()` and sensible defaults
- ✅ Built-in factories: `TerminalChannelFactory`, `AutoApproveChannelFactory`, `WebhookChannelFactory`
- ✅ `default_registry()` creates pre-loaded registry with all built-in factories
- ✅ `TerminalSessionChannel` implementing `SessionChannel` trait
- ✅ 10 tests covering registration, build, config deserialization, missing file handling

#### Remaining

- Slack channel plugin (`ta-channel-slack`) — deferred to separate project
- Webhook signature verification, retry logic — deferred to v0.8+

### v0.7.1 — API Mediator (Layer 1)
<!-- status: done -->
**Goal**: Stage, preview, and apply intercepted MCP tool calls (builds on existing `PendingAction` from v0.5.1).

- **`ApiMediator`**: Implements `ResourceMediator` for `mcp://` scheme.
- **Stage**: Serialize the MCP tool call (name + parameters) as a `StagedMutation`.
- **Preview**: Human-readable summary of what the API call would do (tool name, key parameters, classification).
- **Apply**: Replay the original MCP tool call after human approval.
- **Rollback**: Best-effort (some API calls are not reversible). Record outcome for audit.
- **Integration with ToolCallInterceptor**: Existing `ActionKind` classification drives the mediator's behavior.

#### Completed

- ✅ `ApiMediator` implementing `ResourceMediator` for `mcp://` scheme
- ✅ `StagedApiCall` struct for serializable staged API call data
- ✅ Stage: serialize MCP tool call as JSON to staging dir + in-memory cache
- ✅ Preview: human-readable summary with risk flags (IRREVERSIBLE, EXTERNAL)
- ✅ Apply: marks call as approved, cleans up staging file
- ✅ Rollback: removes staged file and cache entry
- ✅ Pattern-based classification: ReadOnly, Irreversible, ExternalSideEffect, StateChanging
- ✅ URI parsing: `mcp://gmail_send` → `gmail_send`, `mcp://slack/post/message` → `slack_post_message`
- ✅ Human-readable description from tool params (to, subject, channel, etc.)
- ✅ 12 tests covering stage/preview/apply/rollback/classify/extract/describe

### v0.7.2 — Agent-Guided Setup
<!-- status: done -->
**Goal**: Conversational setup flow where a TA agent helps configure workflows — and the resulting config is a TA draft the user reviews.

- **`ta setup`**: Launches a TA goal where the agent is the setup assistant.
- **Output is a draft**: Proposed workflow config, agent configs, credential connections appear as artifacts for review.
- **Progressive disclosure**: Minimal config first, `ta setup refine` for more.
- **Extension point**: Projects on top (Virtual Office, Infra Ops) can provide setup templates that `ta setup --template <name>` consumes.

#### Completed

- ✅ `ta setup wizard` — auto-detects project type, generates full .ta/ config suite
- ✅ `ta setup refine <section>` — updates single config section (workflow, memory, policy, agents, channels)
- ✅ `ta setup show` — displays resolved config from .ta/ files
- ✅ Template generators for workflow.toml, memory.toml, policy.yaml, agent YAML, channel config
- ✅ Project type detection (Cargo.toml → Rust, package.json → TypeScript, etc.)
- ✅ 5 tests covering wizard, refine, show, and project detection

### v0.7.3 — Project Template Repository & `ta init`
<!-- status: done -->
**Goal**: Starter project templates for different project types. `ta init` runs an agent to generate project structure, workflow config, memory key schema, and agent configs — all as a reviewable TA draft.

- **`ta init`**: Creates a new TA-managed project from a template. Runs an agent to generate initial config.
- **`ta init --template <name>`**: Use a named template (e.g., `rust-workspace`, `typescript-monorepo`, `python-ml`, `generic`).
- **`ta init --detect`**: Auto-detect project type from existing files and generate appropriate TA config.
- **Template contents**: Each template produces:
  - `.ta/workflow.toml` — workflow config with sensible defaults for the project type
  - `.ta/memory.toml` — key schema and backend config
  - `.ta/policy.yaml` — starter policy with project-appropriate security level
  - `.ta/agents/<framework>.yaml` — agent configs with bounded actions matching the project's toolchain
  - `.taignore` — exclude patterns for the language/framework
  - `.ta/constitutions/` — optional starter constitutions for common task types
  - Seeded memory entries: `arch:module-map`, `conv:*` entries from the template
- **Template repository**: Templates stored in a public repo (or bundled in the binary). Users can contribute templates via PR.
- **Agent-assisted generation**: The init agent reads existing project files (Cargo.toml, package.json, etc.) and generates config tailored to the actual project structure — not just generic templates.
- **Output is a draft**: Everything generated is a TA draft. User reviews before anything lands in the project.
- **Integration with v0.7.2**: `ta setup` is interactive refinement of existing config; `ta init` is bootstrapping a new project. Both produce drafts.

#### Completed

- ✅ `ta init run` with `--template <name>` and `--detect` flags
- ✅ `ta init templates` — lists all available templates with descriptions
- ✅ 5 built-in templates: rust-workspace, typescript-monorepo, python-ml, go-service, generic
- ✅ Full config generation: workflow.toml, memory.toml, policy.yaml, agent YAML, .taignore, constitutions
- ✅ Memory seeding: parses Cargo.toml/package.json for workspace members → seeds arch:module-map
- ✅ Language-specific .taignore patterns
- ✅ Project type auto-detection with `--detect`
- ✅ 10 tests covering init, templates, detection, memory seeding, workspace extraction

### v0.7.4 — Memory & Config Cleanup
<!-- status: done -->
**Goal**: Wire up deferred memory integration points from v0.6.3.

- **`.ta/memory.toml` backend toggle**: `run.rs` store construction currently always uses RuVector-first fallback logic. Wire the parsed `backend = "fs"` / `backend = "ruvector"` toggle so users can explicitly choose filesystem-only mode.
- **Human guidance domain auto-classification**: Guidance events currently pass `phase_id` but don't use `KeyDomainMap` to classify domains. Route human guidance through the key schema so entries get project-appropriate keys (e.g., "always use bun" → `conv:build-tool` instead of a generic slug).

#### Completed

- ✅ `run.rs` respects `.ta/memory.toml` `backend` toggle — skips RuVector when backend = "fs"
- ✅ `classify_guidance_domain()` in auto_capture.rs — keyword-based domain classification for 7 domains
- ✅ Guidance stored with domain-aware keys (e.g., `conv:build-tool:slug` instead of `guidance:slug`)
- ✅ Explicit tag override: `domain:X` tag takes priority over auto-classification
- ✅ 7 new tests for domain classification and storage behavior
- ✅ Version bumped to `0.7.0-alpha`

### v0.7.5 — Interactive Session Fixes & Cross-Platform Release
<!-- status: done -->
**Goal**: Fix interactive session lifecycle bugs and Linux-musl cross-compilation failure. Harden release pipeline to fail-as-one across all platform targets.

**Completed:**
- ✅ **`ta session close <id>`**: New CLI command that marks an interactive session as completed. If the session's staging directory has uncommitted changes, automatically triggers `ta draft build` before closing. Prevents orphaned sessions when PTY exits abnormally (Ctrl-C, crash). Supports `--no-draft` flag to skip draft build. 3 new tests.
- ✅ **PTY health check on `ta session resume`**: Before reattaching to a session, checks workspace health (existence, staging changes). If workspace is gone, informs user and suggests `ta session close` or `ta session abort`. Added `check_session_health()` function and `SessionHealthStatus` enum. `is_process_alive()` utility for PID-based process checks. 2 new tests.
- ✅ **Linux-musl `ioctl` type fix**: Platform-conditional cast using `#[cfg(target_env = "musl")]` → `libc::c_int`, `#[cfg(not(...))]` → `libc::c_ulong`. Fixes Linux-musl cross-compilation failure.
- ✅ **Release pipeline fail-as-one**: Updated `.github/workflows/release.yml` with `fail-fast: true` and a `release-gate` job that blocks `publish-release` unless all platform builds succeed. No partial releases with missing platform binaries.
- ✅ Version bumped to `0.7.5-alpha`

### v0.7.6 — Interactive Developer Loop (`ta dev`)
<!-- status: done -->
**Goal**: Ship `ta dev` — a local interactive channel where an LLM agent orchestrates the development loop using TA's MCP tools. The agent reads the plan, suggests next goals, launches implementation agents, handles draft review, and cuts releases — all from one persistent session.

**Architecture**: `ta dev` is the **local terminal channel** — the same pattern as Slack, Discord, or a web app. It uses a reusable `agents/dev-loop.yaml` config that any channel can consume. `ta dev` is the convenience CLI entry point that skips staging (orchestration, not implementation), auto-selects `--macro --interactive`, and uses the built-in dev-loop agent config.

```
┌───────────────────────────────────────┐
│  ta dev (local terminal channel)      │  ← LLM agent with system prompt
├───────────────────────────────────────┤
│  TA MCP Gateway                       │  ← ta_plan, ta_draft, ta_goal, ta_context
├───────────────────────────────────────┤
│  TA Core (policy, audit, staging)     │  ← already built
└───────────────────────────────────────┘
```

- **`ta dev` CLI command**: Launches an orchestration agent session. No staging overlay needed — this agent doesn't write code, it coordinates. Auto-reads plan on startup, shows next pending phase with summary.
- **`agents/dev-loop.yaml`**: Agent config with orchestration-focused system prompt. Instructs the agent to: read plan status, suggest next goals, launch sub-goals with implementation agents, handle draft review inline, manage releases. Reusable by any channel (Slack bot, web app).
- **Plan-aware goal launch**: When the user says "run that" or "run v0.7.5", the dev-loop agent calls `ta_goal` with the correct `--phase`, `--source`, and `--agent` (auto-detected from project type + agent configs). No manual flag composition.
- **Inline draft review**: Implementation agent finishes → draft surfaces in the dev session. User can view diff, approve, deny, or ask questions — without leaving the session.
- **Status and navigation**: Agent responds to natural language: "what's next", "status", "show plan", "release", "context search X". Maps to MCP tool calls (`ta_plan`, `ta_draft`, `ta_context`, etc.).
- **Session continuity**: The dev session persists across multiple goals. Step through v0.7.5 → v0.8.0 → release without restarting.
- **No staging for orchestration**: `ta dev` does not create an overlay workspace. The orchestration agent has read-only access to the project (via MCP tools and plan status). Implementation happens in sub-goals with their own staging.

#### Implementation scope

**New files:**
- `apps/ta-cli/src/commands/dev.rs` — `ta dev` command: session setup, agent launch (no staging), plan auto-read on startup
- `agents/dev-loop.yaml` — orchestration agent config with system prompt, tool permissions (ta_plan, ta_goal, ta_draft, ta_context, ta_release), no filesystem write access

**Modified files:**
- `apps/ta-cli/src/commands/mod.rs` — register `dev` subcommand
- `apps/ta-cli/src/main.rs` — wire `dev` command

**Not in scope:**
- Remote channels (Slack, web) — those are projects on top
- New MCP tools — uses existing ta_plan, ta_goal, ta_draft, ta_context
- Changes to goal lifecycle or draft workflow — orchestration only

#### Completed
- ✅ `ta dev` CLI command with `--agent` flag, plan auto-read on startup, no staging overlay
- ✅ `agents/dev-loop.yaml` orchestration agent config with tool permissions and alignment profile
- ✅ Plan-aware prompt generation (plan summary, pending phase highlight, drafts summary)
- ✅ Config loading from YAML (project → user → shipped → fallback)
- ✅ 5 tests: prompt generation, plan summary, drafts summary, config fallback

### v0.7.7 — Agent Framework Registry & Setup Integration
<!-- status: done -->
**Goal**: Make agent frameworks a first-class extensible concept. Ship a framework registry with installation metadata, integrate framework selection into `ta init` and `ta setup wizard`, and add built-in configs for popular frameworks beyond Claude Code.

**Framework Registry**: A `frameworks.toml` (bundled in binary, overridable at `~/.config/ta/frameworks.toml` or `.ta/frameworks.toml`) that maps known frameworks to their metadata:

```toml
[frameworks.claude-code]
name = "Claude Code"
description = "Anthropic's Claude Code CLI — interactive coding agent"
homepage = "https://docs.anthropic.com/en/docs/claude-code"
install = "npm install -g @anthropic-ai/claude-code"
detect = ["claude"]  # commands to check on PATH
agent_config = "claude-code.yaml"
runtime = "native-cli"

[frameworks.codex]
name = "OpenAI Codex CLI"
homepage = "https://github.com/openai/codex"
install = "npm install -g @openai/codex"
detect = ["codex"]
agent_config = "codex.yaml"
runtime = "native-cli"

[frameworks.ollama]
name = "Ollama"
description = "Local LLM runner — run models locally without cloud API keys"
homepage = "https://ollama.ai"
install = { macos = "brew install ollama", linux = "curl -fsSL https://ollama.ai/install.sh | sh" }
detect = ["ollama"]
agent_config = "ollama.yaml"
runtime = "local-llm"

[frameworks.langchain]
name = "LangChain"
description = "Python framework for LLM application development"
homepage = "https://python.langchain.com"
install = "pip install langchain langchain-cli"
detect = ["langchain"]
agent_config = "langchain.yaml"
runtime = "python"

[frameworks.langgraph]
name = "LangGraph"
description = "LangChain's framework for building stateful multi-agent workflows"
homepage = "https://langchain-ai.github.io/langgraph/"
install = "pip install langgraph langgraph-cli"
detect = ["langgraph"]
agent_config = "langgraph.yaml"
runtime = "python"

[frameworks.bmad]
name = "BMAD-METHOD"
description = "Business/Market-driven AI Development methodology"
homepage = "https://github.com/bmad-code-org/BMAD-METHOD"
install = "See https://github.com/bmad-code-org/BMAD-METHOD#installation"
detect = []
agent_config = "bmad.yaml"
runtime = "methodology"  # wraps another runtime (claude-code, etc.)

[frameworks.claude-flow]
name = "Claude Flow"
description = "Multi-agent orchestration with MCP coordination"
homepage = "https://github.com/ruvnet/claude-flow"
install = "npm install -g claude-flow"
detect = ["claude-flow"]
agent_config = "claude-flow.yaml"
runtime = "native-cli"
```

- **`ta init` framework selection**: During `ta init run`, prompt user to select agent framework(s) from the registry. Show detected (on PATH) frameworks first, then available-but-not-installed, then "Custom". For not-installed frameworks, show install instructions and link. Generate `.ta/agents/<framework>.yaml` for each selected framework.
- **`ta setup wizard` framework step**: Add a framework selection step to the setup wizard. Detect installed frameworks, show registry options, generate agent configs. If user selects a framework not on PATH, show installation instructions and offer to re-detect after install.
- **Custom framework from URL or Q&A**: User can select "Custom" → prompted for: command name, args template, whether it reads CLAUDE.md, whether it needs settings injection. Generates a config from `generic.yaml` template with answers filled in. Or user can point to a URL/repo for a community-contributed config.
- **Community contribution path**: Document how to add a framework to the registry via PR (add entry to `frameworks.toml` + agent config YAML in `agents/`). Community configs tagged with `community: true` in the registry.

**New built-in agent configs:**
- `agents/ollama.yaml` — local LLM via Ollama CLI, configurable model selection
- `agents/langchain.yaml` — LangChain agent runner with TA tool integration
- `agents/langgraph.yaml` — LangGraph stateful agent with TA as a node
- `agents/bmad.yaml` — BMAD-METHOD workflow (wraps claude-code or other runtime with BMAD system prompt and phased methodology)

**Bug fix: `ta dev` exits immediately instead of starting interactive session**: `ta dev` prints plan status and pending phases then exits. It should start a persistent interactive agent session (LLM agent with TA MCP tools) where the user can issue natural language commands ("run that", "status", "release"). The dev command needs to launch the agent using the `dev-loop.yaml` config and keep the session alive for user interaction — same pattern as `ta run --interactive` but without staging.

**Bug fix: Macro goal MCP server injection** (GitHub [#60](https://github.com/michaelhunley/TrustedAutonomy/issues/60)): `ta run --macro` injects CLAUDE.md with MCP tool documentation and `.claude/settings.local.json` with permissions, but does NOT inject the `trusted-autonomy` MCP server into `.mcp.json`. The agent sees tool descriptions but can't call them. Fix: inject TA MCP server config into staging workspace's `.mcp.json` (merge with existing entries) during macro goal setup in `run.rs`.

**Bug fix: PR "Why" field** (GitHub [#76](https://github.com/michaelhunley/TrustedAutonomy/issues/76)): The draft summary `why` field (`draft.rs:884`) uses `goal.objective` which often just restates the title. The MCP gateway (`server.rs:881`) passes `goal.title` as `summary_why`. When a goal is linked to a plan phase, pull the phase's `**Goal**:` description from PLAN.md as the "why" — that's where the real motivation lives. Falls back to `goal.objective` when no plan phase is linked.

**"Add TA to an existing project" docs**: Add a clear section to `docs/USAGE.md` covering:
- `ta init --detect` for existing projects (auto-detects project type + installed frameworks)
- Manual setup: copy `generic.yaml`, edit, configure `.ta/` directory
- What TA creates vs what the user needs to provide
- Framework-specific setup notes (e.g., Ollama needs a running server, LangChain needs Python env)

#### Completed

- ✅ Framework Registry (`framework_registry.rs`): Custom TOML parser, bundled registry with 7 frameworks (claude-code, codex, ollama, langchain, langgraph, bmad, claude-flow), project/user override support, PATH detection via `which` (11 tests)
- ✅ `ta init` framework selection: Auto-detects installed frameworks, generates agent YAML configs, shows available-but-not-installed with install instructions
- ✅ `ta setup wizard` framework step: Uses framework registry for detection, shows installed + available frameworks with install guidance
- ✅ New agent configs: `ollama.yaml`, `langchain.yaml`, `langgraph.yaml`, `bmad.yaml`
- ✅ Bug fix: `ta dev` interactive mode — changed `-p` to `--system-prompt` in both `dev-loop.yaml` and hard-coded fallback so Claude stays interactive
- ✅ Bug fix: Macro goal MCP server injection (#60) — `run.rs` injects TA MCP server into `.mcp.json` during macro goal setup, restores on exit
- ✅ Bug fix: PR "Why" field (#76) — `draft.rs` resolves phase description from PLAN.md via `extract_phase_description()`, MCP gateway uses `goal.objective` over `goal.title`
- ✅ Updated `generic.yaml` with Q&A field annotations and community contribution guide
- ✅ Version bump to 0.7.7-alpha
- ✅ Documentation: "Add TA to an existing project" section in USAGE.md, framework registry docs

#### Remaining (deferred)

- Custom framework from URL or Q&A (interactive prompting for custom framework setup)
- Community contribution path documentation (PR workflow for adding frameworks)

#### Implementation scope

**New files:**
- `agents/ollama.yaml` — Ollama agent config
- `agents/langchain.yaml` — LangChain agent config
- `agents/langgraph.yaml` — LangGraph agent config
- `agents/bmad.yaml` — BMAD-METHOD agent config
- `apps/ta-cli/src/framework_registry.rs` — registry loader, detection, install instructions
- Bundled `frameworks.toml` — framework metadata registry

**Modified files:**
- `apps/ta-cli/src/commands/init.rs` — framework selection during init, multi-framework config generation
- `apps/ta-cli/src/commands/setup.rs` — framework step in wizard, detection + install guidance
- `apps/ta-cli/src/commands/run.rs` — inject TA MCP server into `.mcp.json` during `--macro` setup
- `apps/ta-cli/src/commands/draft.rs:884` — replace `goal.objective.clone()` with plan phase description when available
- `crates/ta-mcp-gateway/src/server.rs:881` — replace `&goal.title` (4th arg) with plan phase description
- `agents/generic.yaml` — updated with Q&A field annotations for guided custom setup
- `docs/USAGE.md` — "Add TA to an existing project" section, framework contribution guide

---

## v0.8 — Event System & Stable API *(release: tag v0.8.0-beta)*

> TA publishes stable event types that projects on top subscribe to. This is the "platform API" layer.

### v0.8.0 — Event System & Subscription API (Layer 3 → projects)
<!-- status: done -->
> See `docs/VISION-virtual-office.md` for full vision.

- **Stable `SessionEvent` schema**: Versioned event types with backward compatibility guarantees.
- **`ta events listen`**: Stream JSON events for external consumers.
- **Event hook execution**: Webhooks/scripts on goal + draft state transitions.
- **Non-interactive approval API**: Token-based approve/reject (for Slack buttons, email replies).
- **`--json` output flag**: All CLI commands support programmatic consumption.
- **Compliance event export**: Structured event stream for external compliance dashboards.
- **Extension point for projects**: Virtual Office subscribes to `SessionEvent`s to trigger workflow logic. Infra Ops subscribes to detect infrastructure drift.

#### Completed

- ✅ New `crates/ta-events` crate with `EventEnvelope`, `SessionEvent` enum (14 variants), schema versioning (33 tests)
- ✅ `EventBus` with `tokio::sync::broadcast` channel, `EventFilter` (All, ByType, ByGoal, ByPhase), filtered subscriptions
- ✅ `FsEventStore` writing NDJSON to `.ta/events/<YYYY-MM-DD>.jsonl` with date-based rotation and query filtering
- ✅ `HookConfig` parsed from `.ta/hooks.toml`, `HookRunner` executing shell commands on matching events with env vars
- ✅ `TokenStore` with HMAC-SHA256 tokens, scope-based validation, expiration, single-use marking, cleanup
- ✅ `ta events listen` CLI: NDJSON streaming with `--filter`, `--goal`, `--limit` flags
- ✅ `ta events stats` and `ta events hooks` CLI commands
- ✅ `ta token create/list/cleanup` CLI commands for non-interactive approval workflows
- ✅ `--json` flag on `ta draft list`, `ta draft view`, `ta goal status`, `ta plan status`

#### Remaining (deferred)

- Compliance event export (structured event stream for external dashboards)
- Extension point documentation for Virtual Office / Infra Ops project subscriptions

### v0.8.1 — Solution Memory Export
<!-- status: done -->
**Goal**: Extract reusable problem→solution knowledge from TA memory into a curated, git-committed datastore that ships with the project.

- **`ta context export`**: Extracts `NegativePath` and `Convention` entries from `.ta/memory/` into a human-readable `solutions.toml` (or `.ta/solutions/` directory). Strips project-specific paths and IDs. Preserves the problem description, what was tried, why it failed/worked, and the resolution.
- **Curated format**: Each entry has `problem`, `solution`, `context` (language/framework/platform), and `tags`. Entries are reviewed by the user before committing — not auto-published.
- **Git-committed knowledge**: `solutions.toml` lives in the repo. New team members and future agents benefit from accumulated knowledge without needing a shared registry.
- **Injection at `ta run`**: `build_memory_context_section()` includes relevant solution entries (matched by project type + semantic similarity) in the agent's CLAUDE.md injection. Agents learn from past mistakes without rediscovering them.
- **Import from community**: `ta context import <url>` fetches a solutions file from a public URL or another project and merges it into the local datastore. Community-curated solution packs can be shared as gists or repos.

#### Completed

- ✅ `SolutionEntry` struct with `problem`, `solution`, `context` (language/framework), `tags`, `source_category`, `created_at` (12 tests)
- ✅ `SolutionStore` with TOML serialization, load/save/add/remove/find_by_tag/find_by_context/merge, deduplication by word-set Jaccard similarity
- ✅ `ta context export` CLI: reads NegativePath + Convention entries, strips UUIDs, interactive confirmation, `--non-interactive` flag
- ✅ `ta context import <path>` CLI: reads solutions.toml from local file, merges with deduplication, reports new vs duplicate counts
- ✅ Injection at `ta run`: `build_solutions_section_for_inject()` adds "Known Solutions" section to CLAUDE.md, filtered by project type
- ✅ Custom TOML serializer/parser for `solutions.toml` format (no `toml` crate dependency)

### v0.8.2 — Developer Loop Refinements & Orchestrator Wiring
<!-- status: done -->
**Goal**: Fix `ta dev` bugs and wire the orchestrator→implementation agent loop so `ta dev` can actually launch and monitor goals end-to-end.

**Bug fix: `ta dev` no status summary on launch**: `ta dev` builds the plan summary into `--system-prompt` but never prints it to the terminal. The user sees "Starting interactive developer loop..." then Claude starts with no context. Fix: print plan progress + next pending phase to stdout before launching the agent. (`dev.rs:232`)

**Bug fix: `ta dev` no memory injection**: `ta dev` bypasses `build_memory_context_section_for_inject()` entirely. The orchestration agent starts without project architecture, conventions, or negative paths from the memory store. Fix: query memory store in `build_dev_prompt()` and include a "Project Context" section alongside the plan summary.

**Bug fix: `ta dev` shows v0.1/v0.1.1 as next pending**: `build_plan_summary()` picks the first non-done phase linearly. v0.1 (Public Preview) and v0.1.1 (Release Automation) are legitimately pending but shouldn't appear as "next" ahead of v0.8.x. Fix: add `<!-- status: deferred -->` marker support to plan parser. Phases marked `deferred` are excluded from "next pending" but still show in the full checklist. Mark v0.1 and v0.1.1 as deferred.

**Bug fix: Batch phase status marking**: When a macro goal implements multiple plan phases in one draft (e.g., v0.8.0 + v0.8.1), `ta draft apply` only marks one phase as done. Fix: support `--phase v0.8.0,v0.8.1` (comma-separated) on `ta draft apply` to mark multiple phases done in one operation. Or: `ta plan mark-done <phase-id>` command for manual batch marking.

**Orchestrator→agent wiring via events**: When `ta dev` orchestrator calls `ta_goal action:"start"`, it should spawn the implementation agent asynchronously and subscribe to v0.8.0 `SessionEvent`s for goal state transitions. Flow:
1. `ta_goal action:"start"` creates goal + spawns agent in staging (background)
2. Orchestrator subscribes to events: `goal.draft_ready`, `goal.completed`, `goal.failed`
3. When `goal.draft_ready` fires, orchestrator notifies user: "Draft ready — review?"
4. No polling — event-driven via the v0.8.0 subscription API
5. This is the same pattern Slack/Discord/web channels would use

**`ta run --headless` flag**: Non-interactive agent execution mode for orchestrator-driven goals. No PTY, pipe stdout, return draft ID on completion. Used internally by `ta_goal` when invoked from an orchestrator session. Agent output can optionally stream to the orchestrator's terminal.

#### Completed
- [x] `ta dev` prints plan summary + next phase to terminal before launching agent
- [x] `ta dev` injects memory context (via `build_memory_context_section_for_inject`)
- [x] `PlanStatus::Deferred` added — deferred phases skipped by `find_next_pending()`
- [x] v0.1 and v0.1.1 marked `<!-- status: deferred -->` in PLAN.md
- [x] `ta plan mark-done v0.8.0,v0.8.1` — batch phase status marking
- [x] `ta draft apply --phase v0.8.0,v0.8.1` — comma-separated phase override
- [x] `ta run --headless` — non-interactive execution mode (piped stdout, no PTY, structured JSON result)
- [x] `format_plan_checklist` shows `[-]` for deferred phases
- [x] `ta plan status --json` includes `deferred` count

- [x] `ta_goal action:"start" launch:true` spawns `ta run --headless` in background
- [x] `ta_goal` publishes `GoalStarted` event to `.ta/events/` store on sub-goal creation
- [x] `ta_goal` supports `agent`, `phase`, and `launch` parameters for orchestrator control
- [x] `ta-mcp-gateway` depends on `ta-events` for event publishing

#### Implementation scope

**Modified files:**
- `apps/ta-cli/src/commands/dev.rs` — print status on launch, inject memory context, deferred phase filtering
- `apps/ta-cli/src/commands/plan.rs` — add `Deferred` status to `PlanStatus` enum, parser, and `ta plan mark-done`
- `apps/ta-cli/src/commands/run.rs` — add `--headless` flag, `launch_agent_headless()`, `find_latest_draft_id()`
- `apps/ta-cli/src/commands/draft.rs` — `--phase` override on apply, comma-separated batch marking
- `apps/ta-cli/src/main.rs` — wire `--headless` flag to run command
- `crates/ta-mcp-gateway/src/server.rs` — `ta_goal` launch, agent, phase params + event publishing
- `crates/ta-mcp-gateway/Cargo.toml` — add `ta-events` dependency
- `PLAN.md` — mark v0.1 and v0.1.1 as `<!-- status: deferred -->`

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

#### Remaining (deferred)
- MSI installer and `winget`/`scoop` package definitions (needs release testing)
- `ctrlc` crate integration (current signal handling works via std)

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

#### Remaining (deferred)
- OCI/gVisor container isolation (enterprise feature)
- Enterprise state intercept (see `docs/enterprise-state-intercept.md`)

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

**Remaining (deferred):**
- Sandbox runtime integration: wire `ta-sandbox` as command validator for orchestrator process. Currently relies on `--allowedTools` client-side + gateway-side `CallerMode` enforcement.
- Full tool-call audit logging in gateway: currently logs session start/end; per-tool-call logging deferred to event system integration.

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

#### Remaining (deferred)
- [ ] Automatic agent_id extraction from TA_AGENT_ID env var on every tool call
- [ ] Audit log entries include caller_mode field

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

#### Remaining (deferred)
- Unix domain socket listener (`.ta/daemon.sock`) — deferred until `ta shell` (v0.9.8) needs it
- Full headless agent subprocess wiring in `/api/agent/ask` — deferred until `ta shell` provides client-side rendering
- Bridge template updates (`discord-bridge-api.js`, `slack-bridge-api.js`) — deferred to channel phases (v0.10.x)

#### Version: `0.9.7-alpha`

---

### v0.9.8 — Interactive TA Shell (`ta shell`)
<!-- status: pending -->
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

#### Implementation scope
- `apps/ta-cli/src/commands/shell.rs` — REPL core (~200 lines), daemon client, SSE rendering
- `apps/ta-cli/Cargo.toml` — add `rustyline`, `reqwest` (HTTP client), `eventsource-client` (SSE)
- `templates/shell.toml` — default routing config
- `docs/USAGE.md` — `ta shell` documentation

#### Why so simple?
All complexity lives in the daemon (v0.9.7). The shell is deliberately thin — just a rendering layer. This means any bug fix or feature in the daemon benefits all interfaces (shell, web, Discord, Slack, email) simultaneously.

#### Why not enhance `ta dev`?
`ta dev` gives the agent the terminal (agent drives, human reviews elsewhere). `ta shell` gives the human the terminal (human drives, agent assists). Both connect to the same daemon. `ta dev` is for autonomous work; `ta shell` is for interactive exploration and management.

#### Version: `0.9.8-alpha`

---

### v0.9.8.1 — Policy-Driven Auto-Approval
<!-- status: pending -->
**Goal**: Wire the policy engine into the draft review flow so that drafts matching configurable conditions are auto-approved without human intervention — while preserving full audit trail and the ability to tighten rules at any time.

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
- `docs/USAGE.md` — auto-approval configuration guide, security model explanation
- Tests: condition evaluation (each condition individually), path glob matching, tighten-only cascade, verification command execution, auto-apply flow, audit trail correctness

#### Version: `0.9.8-alpha.1`

---

### v0.9.9 — Conversational Project Bootstrapping (`ta new`)
<!-- status: pending -->
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

### v0.10.0 — Gateway Channel Wiring & Multi-Channel Routing
<!-- status: pending -->
**Goal**: Wire `ChannelRegistry` into the MCP gateway so `.ta/config.yaml` actually controls which channels handle reviews, notifications, and escalations — and support routing a single event to multiple channels simultaneously.

#### Items

1. **Gateway `ChannelRegistry` integration**: `GatewayState::new()` loads `.ta/config.yaml`, builds `ChannelRegistry` via `default_registry()`, resolves `config.channels.review.type` → `ChannelFactory` → `ReviewChannel`. Replace the hardcoded `AutoApproveChannel` default. Fallback to `TerminalChannel` if config is missing or type is unknown.
2. **Multi-channel routing**: Allow `review`, `notify`, and `escalation` to each specify multiple targets. A review request is sent to all configured review channels; first response wins. Notifications fan out to all configured channels. Schema:
   ```yaml
   channels:
     review:
       - type: terminal
       - type: webhook
         endpoint: .ta/channel-exchange
     notify:
       - type: terminal
       - type: webhook
         endpoint: .ta/channel-exchange
         level: warning
     escalation:
       - type: webhook
         endpoint: .ta/channel-exchange
   ```
3. **`MultiChannel` wrapper**: New `MultiReviewChannel` implementing `ReviewChannel` that dispatches to N inner channels. `request_interaction()` sends to all, returns first response. `notify()` fans out to all. Configurable strategy: `first_response` (default) or `quorum` (require N approvals).
4. **`ta config channels` command**: Show resolved channel configuration — which channels are active, their types, capabilities, and status. Useful for debugging channel setup.
5. **Channel health check**: `ta config channels --check` verifies each configured channel is reachable (webhook endpoint exists, credentials valid, etc.).

#### Implementation scope
- `crates/ta-mcp-gateway/src/server.rs` (or post-refactor modules) — registry loading, channel resolution
- `crates/ta-changeset/src/multi_channel.rs` — `MultiReviewChannel` wrapper
- `crates/ta-changeset/src/channel_registry.rs` — schema update for array-of-channels
- `apps/ta-cli/src/commands/config.rs` — `ta config channels` command
- `docs/USAGE.md` — multi-channel routing docs

#### Version: `0.10.0-alpha`

### v0.10.1 — Native Discord Channel
<!-- status: pending -->
**Goal**: `DiscordChannelFactory` implementing `ChannelFactory` with direct Discord gateway connection, eliminating the need for the bridge service.

#### Items

1. **`ta-channel-discord` crate**: New crate at `crates/ta-channel-discord/` with `serenity` (or `twilight`) dependency.
2. **`DiscordReviewChannel`** implementing `ReviewChannel`:
   - `request_interaction()` → posts rich embed with Approve/Deny buttons → awaits interaction via Discord gateway → returns decision
   - `notify()` → posts notification embed
   - `capabilities()` → review, notify, rich_content, buttons
   - Sync/async bridge: runs Discord client on background tokio runtime, bridges via oneshot channel
3. **`DiscordChannelFactory`** implementing `ChannelFactory`:
   - `channel_type()` → `"discord"`
   - `build_review(config)` → reads `token_env`, `channel_id`, `allowed_roles`, `allowed_users`
   - `build_session(config)` → returns error (Discord not suitable for interactive sessions)
4. **Access control**: `allowed_roles` and `allowed_users` in config restrict who can approve/deny.
5. **Deny modal**: Uses Discord modal for denial reason input.

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

#### Version: `0.10.1-alpha`

### v0.10.2 — Native Slack Channel
<!-- status: pending -->
**Goal**: `SlackChannelFactory` implementing `ChannelFactory` with Slack Block Kit and Socket Mode support.

#### Items

1. **`ta-channel-slack` crate**: New crate at `crates/ta-channel-slack/` with `slack-morphism` (or raw `reqwest`) dependency.
2. **`SlackReviewChannel`** implementing `ReviewChannel`:
   - `request_interaction()` → posts Block Kit message with Approve/Deny buttons → awaits action payload → returns decision
   - Socket Mode: connects outbound (no public URL needed) — recommended for solo/small team use
   - HTTP Mode: runs small Axum server for Slack interactivity endpoint
   - `notify()` → posts notification message
3. **`SlackChannelFactory`** implementing `ChannelFactory`:
   - `channel_type()` → `"slack"`
   - `build_review(config)` → reads `bot_token_env`, `channel_id`, `socket_mode`, `app_token_env`, `allowed_users`
4. **Deny modal**: Uses Slack modal (views.open) for denial reason.
5. **Thread-based detail**: Post main review as message, diff details as thread replies.

#### Config
```yaml
channels:
  review:
    type: slack
    bot_token_env: TA_SLACK_BOT_TOKEN
    channel_id: "C0123456789"
    socket_mode: true
    app_token_env: TA_SLACK_APP_TOKEN
    allowed_users: ["U01234567"]
```

#### Version: `0.10.2-alpha`

### v0.10.3 — Native Email Channel
<!-- status: pending -->
**Goal**: `EmailChannelFactory` implementing `ChannelFactory` with SMTP send and IMAP poll for reply-based approval.

#### Items

1. **`ta-channel-email` crate**: New crate at `crates/ta-channel-email/` with `lettre` (SMTP) and `imap`/`async-imap` dependencies.
2. **`EmailReviewChannel`** implementing `ReviewChannel`:
   - `request_interaction()` → sends formatted email via SMTP → polls IMAP for reply → parses APPROVE/DENY keyword from reply body
   - Subject tagging: `[TA Review] {title}` with `X-TA-Request-ID` header for threading
   - Strips quoted text (`>` lines, `On ... wrote:` blocks) before parsing
   - Configurable timeout (default 2 hours — email is slower than chat)
   - `notify()` → sends notification email (no reply expected)
3. **`EmailChannelFactory`** implementing `ChannelFactory`:
   - `channel_type()` → `"email"`
   - Supports any SMTP/IMAP provider (Gmail, Outlook, self-hosted)
4. **Multiple reviewers**: Send to comma-separated list, first to reply wins.
5. **App Password support**: Works with Gmail App Passwords (no OAuth needed for simple setups).

#### Config
```yaml
channels:
  review:
    type: email
    smtp_host: smtp.gmail.com
    smtp_port: 587
    imap_host: imap.gmail.com
    imap_port: 993
    username_env: TA_EMAIL_USER
    password_env: TA_EMAIL_PASSWORD
    reviewer: reviewer@example.com
    poll_interval_seconds: 30
    subject_prefix: "[TA Review]"
```

#### Version: `0.10.3-alpha`

### v0.10.4 — Channel Plugin Loading
<!-- status: pending -->
**Goal**: Allow third-party channel plugins without modifying TA source, enabling community-built integrations (Teams, PagerDuty, ServiceNow, etc.).

#### Items

1. **Process-based plugin protocol**: Plugin is an executable that speaks JSON-over-stdio. TA spawns the process, sends `InteractionRequest` JSON via stdin, reads `InteractionResponse` from stdout. Works with any language.
2. **Plugin discovery**: Scan `~/.config/ta/plugins/channels/` and `.ta/plugins/channels/` for plugin manifests (`channel.toml`):
   ```toml
   name = "teams"
   version = "0.1.0"
   command = "ta-channel-teams"
   capabilities = ["review", "notify"]
   ```
3. **`ProcessChannelFactory`**: Generic `ChannelFactory` that wraps any plugin executable. Registered in `ChannelRegistry` under the plugin's `name`.
4. **`ta plugin list`**: Show installed channel plugins with their capabilities and status.
5. **Plugin install**: `ta plugin install <path-or-url>` copies the executable and manifest to the plugin directory.

#### Config (using a community plugin)
```yaml
channels:
  review:
    type: teams              # resolved from plugin manifest
    webhook_url_env: TA_TEAMS_WEBHOOK
    channel: "General"
```

#### Version: `0.10.4-alpha`

---

## Projects On Top (separate repos, built on TA)

> These are NOT part of TA core. They are independent projects that consume TA's extension points.
> See `docs/ADR-product-concept-model.md` for how they integrate.

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

### Community Memory Sync
Federated sharing of anonymized problem→solution pairs across TA instances. Builds on v0.8.1 (Solution Memory Export) with:
- **Community sync layer**: Publish anonymized entries to a shared registry (hosted service or federated protocol).
- **Privacy controls**: Tag-based opt-in, never auto-publish. PII stripping before publish. User reviews every entry before it leaves the local machine.
- **Retrieval**: `ta context recall` searches local first, then community if opted in.
- **Provenance tracking**: Did this solution actually work when applied downstream? Feedback loop from consumers back to publishers.
- **Trust model**: Reputation scoring for contributors. Verified solutions (applied successfully N times) ranked higher.
- **Spam/quality**: Moderation queue for new contributors. Automated quality checks (is the problem statement clear? is the solution actionable?).