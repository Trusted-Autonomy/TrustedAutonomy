## TA v0.15.30-alpha.2.1 — Windows Startup Crash Fix

**Patch release on top of v0.15.30-alpha.2.**

### What's fixed in this patch

- **Windows startup crash without Client-ProjFS** — `ta` no longer crashes on launch when
  the Windows Projected File System optional feature is not installed. `ProjectedFSLib.dll`
  is now delay-loaded; the binary starts normally and only activates ProjFS staging when
  the feature is present. (v0.15.30.2.1)

Users on Windows who were unable to launch `ta` without first enabling Client-ProjFS
should upgrade to this release.

---

## v0.15.30-alpha.2 — Agent Framework Enforcement, Full Workflow Automation & Studio

This is a large milestone release covering 164 completed plan phases since v0.13.17-alpha.7.
The headline work is the completion of the agent framework abstraction layer, a full-featured
workflow automation system, the TA Studio desktop UI, creative content pipeline connectors,
and end-to-end release automation.

---

### Agent Framework Abstraction (v0.15.28–v0.15.30)

The agent invocation layer is now fully abstracted — no code in the TA source calls agent
CLIs directly. All agent interactions go through the `AgentFramework` trait.

- **Shim removal** — all legacy direct-call shims replaced with trait dispatch (v0.15.30)
- **Secondary injection paths hardened** — `run.rs` exemptions audited and cleaned; every
  agent launch path injects the governed context through a single channel (v0.15.30.1)
- **Release pipeline fully studio-runnable** — the release workflow compiles clean with no
  residual direct-call warnings and can be triggered from TA Studio (v0.15.30.2)
- **Approval gate TTY policy** — interactive approval gates now call `ta_ask_human` (MCP)
  rather than assuming a TTY is present; headless runs that hit an approval gate fail fast
  with an actionable error instead of hanging (v0.15.30.3)
- **Unified Agent Context Channel** — a single `AgentContextChannel` abstraction covers
  all context injection and live human interjection (`ta advise`) paths (v0.15.28)

### PLAN.md Integrity & VCS Enforcement (v0.15.28–v0.15.29)

- **PLAN.md integrity diagnostics** — `ta plan status` now validates item consistency,
  detects stray separators, and reports duplicate or misordered phase markers (v0.15.28.1)
- **Pre-merge rebase + atomic status commit** — `ta draft apply` rebases the feature branch
  on `origin/main` before writing, then commits the PLAN.md status update atomically with
  the code changes to prevent merge-time corruption (v0.15.28.2)
- **VCS adapter enforcement** — all direct `git` subprocess calls replaced with
  `VcsAdapter` trait calls; the compiler now prevents regressions (v0.15.29)
- **Item consistency enforcement** — PLAN.md post-apply validation checks that every item
  in a newly-done phase is marked `[x]`; mismatches are surfaced as apply warnings (v0.15.29.2)

---

### Workflow Automation System (v0.15.20–v0.15.27)

A complete workflow authoring, publishing, and execution system built on parameterized
TOML templates.

- **Workflow Template Library** — `ta workflow install <name>`, `ta workflow publish`,
  and `ta workflow search` for discovering and sharing reusable workflow templates (v0.15.27)
- **Auto-Approve Constitution** — rule-based policy engine for automated approval decisions;
  amendment flow lets reviewers refine the constitution inline (v0.15.25)
- **Intent Resolver** — natural language input maps to the right workflow invocation;
  `ta run "describe what you want"` resolves to a structured goal + phase (v0.15.24)
- **Parameterized Workflow Templates** — workflow `.toml` files support `{{variable}}`
  substitution; parameters validated at dispatch time (v0.15.23)
- **Work Planner + Implementor split** — governed workflows now run a planner agent pass
  before the implementor, improving quality on complex multi-file changes (v0.15.20)
- **Batch phase build loop** — `build_phases.sh` drives a `build` sub-workflow per phase,
  with auto-approve and post-sync build verification (v0.15.15.5)
- **Phase claim locking** — duplicate workflow dispatches for the same phase are prevented
  by a daemon-side claim; racing agents get a clear "phase claimed" error (v0.15.24.2)
- **Audit trail integrity** — every workflow run appends a signed entry to
  `.ta/goal-audit.jsonl`; `ta audit verify` detects tampering (v0.15.24.1)
- **PLAN.md compaction** — a release-time audit step compacts PLAN.md, stripping
  internal-only entries and producing the reviewer-facing summary (v0.15.24.3)
- **workflow.local.toml** — canonical local override file with deprecation fallback from
  the old name; TOML values deep-merged rather than replaced (v0.15.6)

---

### Multi-Agent Governance & Review (v0.15.14–v0.15.22)

- **Multi-Agent Consensus Review** — configurable reviewer quorum; agents vote independently
  and a consensus engine produces a unified verdict with dissent notes (v0.15.15)
- **Governed Interactive Session** — `ta advise` attaches an advisor agent to a running
  goal; the advisor can inject guidance mid-run without interrupting the implementor (v0.15.19)
- **Workflow Event Bus** — pub/sub event stream for workflow lifecycle events; external
  tools subscribe via `ta workflow subscribe` (v0.15.19.1)
- **Notification Rules Engine** — configurable delivery channels (terminal, Slack, Discord,
  email) with per-event routing rules (v0.15.19.2)
- **Draft Pre-Apply Plan Review Agent** — before `ta draft apply`, a reviewer agent checks
  that all PLAN.md items claimed in the draft are actually implemented (v0.15.19.3)
- **Reviewer Agent source verification** — reviewer checks claimed items against actual
  changed files, not just the agent's summary; flags unimplemented claims (v0.15.19.4.2)
- **Hierarchical workflows** — parallel fan-out to sub-workflows, phase loops, and
  milestone draft collection; parent workflow waits for all branches (v0.15.14)
- **Language-aware static analysis** — the supervisor runs language-specific linters on
  changed files and loops back to the agent for correction before signing off (v0.15.14.3)
- **Security level profiles** — Low / Mid / High profiles control which supervisor checks
  run and what constitutes a blocking finding (v0.15.14.4)
- **Supervisor file-inspection mode** — supervisor can request specific files for manual
  review before approving; triggered by anomaly patterns in the diff (v0.15.14.5)
- **Studio Advisor Agent** — QA agent upgrade with structured finding categories and
  per-finding severity ratings surfaced in the Studio review panel (v0.15.21)
- **Apply loop reliability** — VCS-agnostic commit-diff scan, three-way conflict merging,
  and staging GC; apply no longer leaves orphaned staging copies (v0.15.22)
- **Velocity stats** — `ta velocity` shows rework rate, token cost per phase, and
  per-version filtering; data auto-migrates from the old schema (v0.15.14.2)

---

### TA Studio Desktop UI (v0.14.13–v0.14.22)

A native cross-platform desktop application for managing goals, plans, and drafts.

- **Multi-project browser** — sidebar project switcher; each project maintains its own
  daemon connection and plan state (v0.14.18)
- **Plan tab** — browse PLAN.md phases with status indicators; run any phase with one
  click or enter a custom goal; status updates live as the agent works (v0.14.19)
- **Workflows tab** — trigger, monitor, and cancel governed workflow runs from the UI;
  live log stream with collapsible phase entries (v0.14.20)
- **Agent Personas** — per-project identity configuration: model, system prompt, and tool
  allowlist editable from a dedicated Personas tab (v0.14.20)
- **New Project Wizard** — guided setup for new TA projects with agent config and
  CLAUDE.md generation; guard prevents re-running in an already-initialised project (v0.14.21/v0.14.22)
- **Setup Wizard & Settings Management** — first-run guided configuration for API keys,
  agent selection, and project paths (v0.14.13)
- **Platform launchers** — `ta-studio` binary plus `.app` (macOS) and `.bat` (Windows)
  launchers included in all release packages (v0.14.18)
- **Shell word-wrap & scroll fixes** — prompt wraps at word boundaries; scroll position
  preserved on reconnect; reconnect panic fixed (v0.14.10.1)
- **Draft view polish** — side-by-side diff display with syntax highlighting; agent
  decision log surfaced per file (v0.14.7 / v0.14.9.2)

---

### Creative Content Pipeline Connectors (v0.15.0–v0.15.3)

- **Generic asset support** — `ta-changeset` now tracks binary and text assets (images,
  video, 3D files) alongside code with URI-based identity (v0.15.0)
- **Video artifact support** — video files diff as metadata-only entries; frame thumbnails
  shown in draft review (v0.15.1)
- **ComfyUI inference connector** — `ta connector start comfyui` launches a managed
  ComfyUI process; workflows submit inference jobs via MCP and track completion (v0.15.2)
- **Unity connector** — full Unity project support with scene query, addressables build,
  test run, and render capture via MCP tools (v0.15.3)
- **Unreal Engine connector** — scaffold for UE5 project management; MRQ-governed render
  flows submit to Movie Render Queue and wait for completion (v0.14.14/v0.14.15)
- **Image artifact support** — PNG/JPEG/EXR tracked in drafts with thumbnail previews in
  `ta draft view` and TA Studio (v0.14.15)
- **Artifact-typed workflow edges** — workflow steps declare input/output artifact types;
  the engine validates type compatibility at dispatch time (v0.14.10)

---

### Infrastructure & Reliability (v0.14.0–v0.15.13)

**Agent sandboxing**: `ta run` wraps agent processes in platform-appropriate sandboxes
(macOS sandbox-exec, Linux seccomp) with configurable resource limits (v0.14.0).

**Multi-party approval**: configurable approval threshold (e.g. 2-of-3 reviewers); each
approver signs with their key; `ta draft apply` requires quorum before proceeding (v0.14.2).

**Project-scoped memory**: `.ta/project-memory/` directory committed alongside code; agents
read and write structured memory entries that persist across goal runs (v0.15.13.3).

**Hierarchical sub-workflows**: workflow steps can invoke other workflows as sub-steps;
results collected and passed to the parent via typed artifact edges (v0.15.13).

**ta init generates CLAUDE.md**: `ta init` now writes a project-appropriate CLAUDE.md
template, reducing new-project setup friction (v0.15.13.1).

**Heartbeat-based supervisor liveness**: supervisor timeout replaced with missed-heartbeat
detection; long-running agents no longer time out mid-task (v0.15.13.4).

**Post-Install Onboarding Wizard**: `ta onboard` guides new users through agent config,
API key setup, and first goal; first-run gate skipped when `ANTHROPIC_API_KEY` is set (v0.15.11).

**Draft apply lock**: `.ta/apply.lock` prevents concurrent applies and co-developer races;
PID checked to avoid stale-lock false positives (v0.15.11.1).

**Windows ProjFS staging**: on Windows, the staging workspace is backed by ProjFS (virtual
filesystem) for copy-on-write semantics without full-copy overhead (v0.15.8).

**Inline draft build**: `ta draft build` can be called inside an interactive session; draft
is built from the current working directory without leaving the session (v0.15.8.1).

**Email Assistant Workflow + MessagingAdapter**: `ta workflow run email-assist` reads the
inbox, drafts replies, and queues them for human review; email providers are plugins
implementing the `MessagingAdapter` trait (v0.15.9/v0.15.10).

**Nightly build pipeline**: `.github/workflows/nightly.yml` builds all platforms on a
nightly schedule; results posted to the `nightly` rolling release tag (v0.15.15.6).

**crates.io publishing infrastructure**: `scripts/publish-crates.sh` handles dependency
ordering, rate-limit retries, and already-published skips; CI publishes on tag push (v0.15.15.3).

**One-command release**: `ta release dispatch` detects the current phase, bumps the
version, triggers CI, and waits for the release to appear on GitHub (v0.15.15.2).

**ta doctor**: `ta doctor` validates API keys, agent binary presence, daemon health, and
version consistency; suggests fixes for each failing check (v0.15.17).

**Project TA version tracking**: `.ta/version.toml` records which TA version last touched
the project; `ta doctor` warns when the project was last used with a significantly older
version (v0.15.18).

**Windows code signing**: EV certificate integration in the release workflow; MSI and
standalone binary are Authenticode-signed (v0.15.16).

**Terms acceptance gate**: first-run operations require explicit acceptance of the
disclaimer; stored in `~/.config/ta/terms.toml` (v0.15.5).

**GC, recovery & self-healing**: `ta gc` prunes orphaned staging copies, stale locks, and
old draft packages; self-healing on daemon restart recovers interrupted applies (v0.14.12).

**Auth plugin surface** / **Daemon extension surface**: `ta-auth` and `ta-daemon-ext`
crates expose stable plugin interfaces for custom auth providers and daemon extensions
(v0.14.4/v0.14.5).

**Local audit ledger**: every goal run, draft apply, and approval decision is appended to
`.ta/goal-audit.jsonl`; retained across GC runs (v0.14.6).

**Pluggable memory backends**: `ta-memory` defines a backend trait; external plugins can
provide custom memory stores (SQLite, Redis, etc.) via dynamic loading (v0.14.6.5).

**Plan phase ordering enforcement**: `ta plan status --check-order` detects out-of-sequence
phase completions; `ta draft apply` refuses to proceed if a lower phase is still pending
(v0.14.3).

---

### Bug Fixes

- Fixed branch restore after draft apply — feature branch correctly set as active after
  `ta draft apply` copies changes back (v0.14.16)
- Fixed draft apply branching from stale local HEAD — now branches from `origin/main` to
  avoid PLAN.md rebase conflicts (v0.15.24.4)
- Fixed protected-file (PLAN.md) mtime heuristic — source PLAN.md always wins on apply;
  no silent overwrites (v0.15.24.5)
- Fixed supervisor prompt piped via stdin when `--allowedTools` is set (v0.15.14)
- Fixed legacy agent decision log bleeding between goals (v0.15.14.7)
- Fixed supervisor hook JSON filtering — only TA-relevant hook events forwarded (v0.15.14.6)
- Fixed version-check false positives in CI (v0.15.19.4 series)
- Fixed nightly tag force-push → delete+recreate to avoid GitHub lock (v0.15.24.2)
- Fixed PR URL capture: handles indented `gh` output and falls back to `gh pr list` (v0.15.5)
- Fixed `ta pr sync` silently skipping when no PR URL found — now bails with an error
- Fixed `bump_workspace_version()` to update subcrate internal dependency versions (v0.15.14)
- Fixed truncate panic on multi-byte (UTF-8) characters at char boundaries (v0.15.19.4.3)
- Fixed MSI build: WiX v4 migration, valid XML comments, correct CustomAction for SystemFolder
- Fixed `ta-studio.bat` missing from MSI (WIX0103)
- Fixed onboarding: first-run gate skipped when `ANTHROPIC_API_KEY` is set; subscription
  agent binaries treated as configured

---

_Changes since public-alpha-v0.13.17.7_
