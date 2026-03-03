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
<!-- status: pending -->
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
<!-- status: in_progress -->

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
<!-- status: pending -->
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

### v0.5.6 — Framework-Agnostic Agent State
<!-- status: pending -->
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

### v0.5.7 — Semantic Memory Queries & Memory Dashboard
<!-- status: pending -->
**Goal**: Rich querying and visualization of the memory store. Enables users to audit what TA has learned, curate memory entries, and understand how memory influences agent behavior.

- **Semantic CLI queries**:
  ```bash
  ta context search "how to handle errors in this project"  # semantic search
  ta context similar <entry-id>                              # find related entries
  ta context explain <entry-id>                              # show provenance chain
  ta context stats                                           # memory usage, entry counts by category
  ```

- **Memory dashboard in web UI** (extends v0.5.2):
  - New tab: `/memory` — browse, search, edit, delete memory entries
  - Provenance view: which goal/agent/session created each entry
  - Usage analytics: which memories get recalled most, which are stale
  - Manual entry creation: human adds knowledge directly

- **Memory lifecycle**:
  - TTL support: entries can expire (`ta context store --expires-in 30d`)
  - Confidence scoring: entries from approved drafts rank higher than auto-captured
  - Conflict resolution: when two agents store contradictory memories, flag for human arbitration
  ```bash
  ta context conflicts       # List contradictory entries
  ta context resolve <id>    # Human picks the canonical version
  ```

- **Web UI API routes** (extends v0.5.2 web.rs):
  ```
  GET  /api/memory           → list memory entries (JSON)
  GET  /api/memory/search    → semantic search (?q=query)
  POST /api/memory           → create entry
  DELETE /api/memory/:id     → delete entry
  GET  /api/memory/stats     → usage statistics
  ```

#### Tests (minimum 6)
Semantic search API, entry expiration, conflict detection, web UI memory list, provenance chain, stats endpoint.

---

## v0.6 — Supervisor & Auto-Approval *(release: tag v0.6.0-alpha)*

### v0.6.0 — Supervisor Agent & Constitutional Workflows
<!-- status: pending -->
**Goal**: A TA-internal supervisor agent that verifies agent work stays within constitutional bounds before auto-approving. Enables trust escalation from "approve everything" to "auto-approve within policy."

> **Key insight**: Auto-approval isn't "skip review." It's "have the supervisor review instead of the human, within bounds the human defined." The human sets the constitution; the supervisor enforces it.

- **Workflow constitution**: Per-workflow YAML defining what's in-bounds for auto-approval:
```yaml
# .ta/constitutions/code-review.yaml
name: "Code changes — low risk"
auto_approve_when:
  risk_score_max: 20
  artifact_count_max: 10
  file_patterns: ["src/**", "tests/**"]    # only source files
  no_new_dependencies: true
  no_security_sensitive_files: true        # no .env, credentials, CI configs
  change_types: [modify]                   # no deletes, no new files
  agent_drift_clear: true                  # no drift alerts (v0.4.2)
escalate_to_human_when:
  - "any artifact rejected in previous draft for same goal"
  - "agent accessed files outside file_patterns"
  - "risk_score > threshold"
notify_on_auto_approve: true               # always tell the human what was auto-approved
```
- **Supervisor verification**: Before auto-approving, the supervisor agent checks:
  1. All artifacts within constitutional bounds
  2. No drift alerts active for this agent
  3. Access constitution (v0.4.3) not violated if one exists
  4. No new patterns not seen in agent's historical baseline
- **Trust levels**: None (all manual) → Constitutional (auto within bounds) → Full (auto-approve everything, audit only). Default: None. User escalates explicitly.
- **Audit trail**: Every auto-approval records: which constitution, which checks passed, supervisor reasoning. Human can audit retroactively.
- **"TA supervises TA"**: The supervisor itself runs through TA governance — its config is a draft that the human approves. Supervisor can't expand its own authority.

### v0.6.1 — Cost Tracking & Budget Limits
<!-- status: pending -->
- Track token usage per goal, per agent, per session
- Estimated cost displayed in draft summary: "This goal used ~45K tokens ($0.18)"
- Budget limits in workflow config: `max_cost_per_goal: 5.00`, `max_cost_per_day: 50.00`
- Agent warned at 80% budget; stopped at 100%. Human can override.
- Cost history: `ta audit cost --agent claude-code --last 30d`

---

## v0.7 — Guided Setup & Workflow Templates *(release: tag v0.7.0-alpha)*

> **Design principle**: All setup operates like a smart agent acting in the user's best interests, with full review via TA's own draft model. "Use TA to build TA user config."

### v0.7.0 — Agent-Guided Setup (`ta setup`)
<!-- status: pending -->
**Goal**: A conversational setup flow where a TA agent helps configure workflows, connect services, and create role definitions — and the resulting config is a TA draft the user reviews before activation.

- **`ta setup`**: Launches a TA goal where the agent is the setup assistant. User describes what they want in natural language. Agent proposes:
  - Workflow config (`.ta/workflow.toml`)
  - Agent configs (`agents/*.yaml`)
  - Credential connections (OAuth flows)
  - Role definitions (for virtual office)
  - Plan schema (`.ta/plan-schema.yaml`)
- **Output is a draft**: All proposed configs appear as artifacts in a TA draft. User reviews each config file, approves/rejects/edits. Nothing activates until approved.
- **Templates**: Pre-built workflow templates for common use cases:
  - `ta setup --template sw-engineer` — git integration, code review, plan tracking
  - `ta setup --template email-assistant` — Gmail connection, auto-draft replies, daily digest
  - `ta setup --template social-media` — scheduled posts, content calendar, engagement tracking
  - `ta setup --template home-finance` — bank connections (Plaid), transaction categorization, monthly reports
  - `ta setup --template family-office` — multi-account aggregation, portfolio dashboards, tax document prep
- **Progressive disclosure**: Start simple, add complexity as needed. Initial setup creates minimal config. User can run `ta setup refine` later to add more.

### v0.7.1 — Domain Workflow Templates
<!-- status: pending -->
**Pre-built workflow definitions for specific domains**. Each template defines roles, triggers, constitutional bounds, and MCP server requirements.

#### Software Engineering
- Code review workflow: goal → agent works → draft with diffs + explanations → approve → commit
- CI/CD integration: `ta release` pipeline, test-before-merge gates
- Plan-driven development: optional, activates when plan exists

#### Personal Productivity (Email + Social)
- Email triage: agent categorizes, drafts replies for routine emails, escalates complex ones
- Social media: content calendar → scheduled post drafts → review → publish
- Calendar management: meeting prep, follow-up drafts

#### Home Finance
- **Bank integration**: Plaid MCP server for transaction feeds. TA holds all financial data locally — never in cloud.
- **Transaction categorization**: Agent categorizes transactions, human reviews miscategorized ones. Learns over time.
- **Monthly dashboard**: Agent generates spending summary, budget vs actual, investment performance. Output as HTML report (same adapter system as `ta draft view`).
- **Bill tracking**: Agent monitors recurring charges, flags anomalies (unexpected charges, price increases)
- **Tax prep**: Agent collects deductible transactions, generates summary for accountant

#### Family Office Finance
- **Multi-account aggregation**: Multiple bank/brokerage accounts across family members. Per-member dashboards.
- **Tiered access**: Principal sees everything. Advisor sees portfolio. Accountant sees tax-relevant transactions. Enforced via TA's identity/credential system (v0.5.0).
- **Portfolio reporting**: Agent aggregates positions, generates performance reports, tracks rebalancing needs
- **Document management**: Agent organizes financial documents (statements, tax forms, contracts) via MCP to Google Drive/local filesystem
- **Compliance**: Audit trail of every agent access to financial data. Who saw what, when, why.

---

## v0.8 — Events & Orchestration *(release: tag v0.8.0-beta)*

### v0.8.0 — Event System & Orchestration API
<!-- status: pending -->
> See `docs/VISION-virtual-office.md` for full vision.
- `--json` output flag on all CLI commands for programmatic consumption
- Event hook execution: call webhooks/scripts on goal + draft state transitions
- `ta events listen` command — stream JSON events for external consumers
- Stable event schema matching `docs/plugins-architecture-guidance.md` hooks
- Non-interactive approval API: token-based approve/reject (for Slack buttons, email replies)
- Foundation for virtual office runtime
- **Compliance event export**: Structured event stream enables external compliance dashboards

### v0.8.1 — Community Memory
<!-- status: pending -->
**Goal**: Opt-in sharing of memory across TA instances. Builds on the `MemoryStore` and ruvector backend from v0.5.4.

- **Local memory already exists** (v0.5.4): Each TA instance has a `MemoryStore` with automatic capture from goal results, draft reviews, and human guidance.
- **Community sync layer**: Publish anonymized problem → solution pairs to a shared registry. ruvector's distributed sync (Raft consensus) provides the replication mechanism.
- **Privacy controls**: User chooses what to share — tag-based opt-in, never auto-publish. PII stripping before publish.
- **Retrieval**: `ta context recall` searches local memory first, then community registry if opt-in is enabled. Local results ranked higher.
- **Not a chatbot knowledge base** — focused on actionable problem → solution pairs with provenance and verification status (did this actually work when applied?)

---

## v0.9 — Distribution & Packaging *(release: tag v0.9.0-beta)*

### v0.9.0 — Distribution & Packaging
<!-- status: pending -->
- Developer: `cargo run` + local config + Nix
- Desktop: installer with bundled daemon, git, rg/jq, common MCP servers
- Cloud: OCI image for daemon + MCP servers, ephemeral virtual workspaces
- Full web UI for review/approval (extends v0.5.2 minimal UI)
- Mobile-responsive web UI (not a native app — PWA is sufficient for v1.0)

### v0.9.1 — Native Windows Support
<!-- status: pending -->
**Goal**: First-class Windows experience without requiring WSL, timed for when non-engineers (home finance, family office, personal productivity users) begin adopting TA via v0.7's guided setup and v0.9.0's desktop installer.

- **Windows MSVC build target**: Add `x86_64-pc-windows-msvc` to CI release matrix (GitHub Actions `windows-latest` runner). Ship `.zip` archive with `ta.exe`.
- **Path handling**: Audit all `Path`/`PathBuf` usage for Unix assumptions (`/` separators, `/tmp`, `/usr/local/bin`). Use `std::path` consistently; replace hard-coded `/` with `std::path::MAIN_SEPARATOR` where needed.
- **Process management**: Replace Unix-specific signal handling (`SIGTERM`, `SIGINT`) with cross-platform equivalents. Use `ctrlc` crate for Ctrl+C handling on Windows.
- **Shell command execution**: Agent configs currently assume `bash`. Add `shell` field to agent YAML (`bash`, `powershell`, `cmd`). Default: auto-detect from OS.
- **Installer**: MSI or NSIS installer bundled with desktop build (v0.9.0). Add to `winget` and `scoop` package managers.
- **Testing**: Add Windows CI job running full test suite. Gate releases on Windows tests passing.
- **Known limitations for v0.9.1**: Sandbox runner (v0.9.2) may not support Windows initially — gVisor is Linux-only. Document WSL2 as fallback for sandboxed execution on Windows.

> **Why v0.9.1**: Non-engineer users arrive at v0.7 (guided setup) and v0.9.0 (desktop installer). By v0.9.1, the install experience must be native on all three platforms. Earlier phases target developers who are comfortable with macOS/Linux and WSL.

### v0.9.2 — Sandbox Runner (optional hardening)
<!-- status: pending -->
> Moved from v0.6. Optional for users who need kernel-level isolation. Not a prerequisite for v1.0.

- OCI/gVisor sandbox for agent execution
- Allowlisted command execution (rg, fmt, test profiles)
- CWD enforcement — agents can't escape virtual workspace
- Command transcripts hashed into audit log
- Network access policy: allow/deny per-domain
- **Enterprise state intercept**: For environments requiring network-level capture, see `docs/enterprise-state-intercept.md`. Integrates with sandbox for traffic routing.

---

## v1.0 — Virtual Office *(release: tag v1.0.0)*

### v1.0.0 — Virtual Office Runtime
<!-- status: pending -->
> Thin orchestration layer that composes TA, agent frameworks, and MCP servers.

- Role definition schema (YAML): purpose, triggers, agent, capabilities, notification channel
- Trigger system: cron scheduler + webhook receiver + TA event listener
- Office manager daemon: reads role configs, routes triggers, calls `ta run`
- `ta office start/stop/status` CLI commands
- Role-scoped TA policies auto-generated from role capability declarations
- Constitutional auto-approval (v0.6.0) active by default — supervisor reviews routine work
- Credential broker (v0.5.0) manages all service access — no role holds raw credentials
- Community memory (v0.8.1) shared across office roles
- Does NOT duplicate orchestration — composes existing tools with role/trigger glue
- **Multi-agent alignment verification**: Before agents co-operate on shared resources, TA verifies alignment profile compatibility (v0.4.0 profiles)
- **Compliance dashboard**: Aggregate decision reasoning, drift reports, cost tracking, and approval records into a per-role compliance view. Exportable as ISO/IEC 42001 evidence package.

> **Standards**: Virtual office with defined roles, capability boundaries, human oversight at checkpoints, and continuous drift monitoring satisfies **ISO/IEC 42001**, **EU AI Act** (Articles 9, 14, 50), and **Singapore IMDA Agentic AI Framework**.

---

## Supervision Frequency: TA vs Standard Agent Usage

> How often does a user interact with TA compared to running Claude/Codex directly?

| Mode | Standard Claude/Codex | TA-mediated |
|------|----------------------|-------------|
| **Active coding** | Continuous back-and-forth. User prompts, reads output, prompts again. ~100% attention. | `ta run` launches agent. User checks back when draft is ready. ~10-20% attention. Review takes 2-5 min per draft. |
| **Overnight/batch** | Not possible — agent exits when session closes. | `ta run` in background. Review next morning. 0% attention during execution. |
| **Auto-approved (v0.6)** | N/A | Supervisor handles review. User sees daily summary. ~1% attention. Escalations interrupt. |
| **Virtual office (v1.0)** | N/A | Roles run on triggers. User reviews when notified. Minutes per day for routine workflows. |

**Key shift**: Standard agent usage demands synchronous human attention. TA shifts to asynchronous review — the agent works independently, the human reviews completed work. This gets more asynchronous over time as trust (constitutional auto-approval) increases.