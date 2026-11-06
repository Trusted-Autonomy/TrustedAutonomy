## Unreleased

**Agent login and launch failures**

- **Agent logins: `ta agent-login list|login|logout|verify`.** Give Claude, OpenAI, Gemini, xAI
  (and any framework you add) a login once; TA keeps it in its per-user credential custody (OS
  keychain or an owner-only file) and `ta run` puts the stored login of the launching agent, and
  only that agent's, into every sandboxed launch, including the ones `ta-daemon` starts. A login
  variable already set in the launch environment wins. The framework catalog is data
  (`agent-frameworks.default.toml`, overridable in `~/.config/ta/agent-frameworks.toml` and
  `.ta/agent-frameworks.toml`). `ta agent-login login claude-code` runs `claude setup-token` in a
  pseudo-terminal and captures the token (timeout `TA_AGENT_LOGIN_TIMEOUT_SECS`); `verify` checks
  a login with the cheapest real request the vendor offers. Secrets travel only on standard input
  or in the vendor's terminal, never in arguments, output, logs or audit lines. `--json` output
  and exit codes (0, 2 usage, 3 vendor login failed, 4 vendor command missing) are scripted
  against by the Wayfinder poller. `ta doctor`'s `Daemon agent login` line is `ok` when a login is
  stored, naming what and where (never the value). `XAI_API_KEY` joins the variables the sandbox
  forwards. See "Logging your agents in" in docs/USAGE.md.
- **`ta init` creates the project's state directory.** A freshly initialized project now has its
  id (`.ta/project-id`) and state directory straight away, so `ta show state` prints every
  location, NATS JetStream included, before anything has been stored.

**Launch failures reported to the source**

- **`launch_failed` outcome** — when a goal started for an external-intake candidate fails to start,
  exits non-zero before doing work, runs past `wake_launch_timeout_secs`, is refused by the
  sandbox, has no or a rejected login, or its wake message is dead-lettered, the daemon publishes
  `{"kind":"launch_failed","reason_code":"no_login|login_invalid|agent_missing|sandbox_denied|crashed|timed_out","detail","fix"}`
  on the `external-outcome` stream, keyed to the candidate. Signed with the new `ta-daemon` role; an
  agent cannot send it. Detail and fix text never contain secrets. Undelivered outcomes wait in
  `.ta/launch-failed-outbox/`.
- **`ta goal list --json`** (new) and **`ta goal status --json`** (extended) expose `poller_state`
  (`launching|running|waiting_for_review|failed|finished`), `candidate_id`, `task_id`, `started_at`,
  `launch_failure`; `ta goal status` also prints `Launch:`/`Detail:`/`Fix:` for a failed launch.
- **`ta run`** records the classified reason on the failed goal and prints a
  `ta-launch-failure:` marker line (identifiers only); a missing agent command in a headless run
  now exits non-zero instead of exiting 0.

**CI cost and local validation**

- **`scripts/ci-local.sh`**: one command that runs every CI check on a developer Mac (version
  consistency, no-ip-docs, secret scan, actionlint, host fmt/clippy/test/Seatbelt conformance,
  standalone plugins, Windows `clippy --target x86_64-pc-windows-msvc`, and Linux clippy/test/sandbox
  containers in Docker), prints a PASS/FAIL table, what it did not cover, and an evidence line for
  the pull request. Throwaway `HOME`, no Keychain, no paid agents. How-to: docs/USAGE.md
  "Validating changes locally (ci-local)".
- **Pull requests run one ubuntu job plus the cheap checks.** macOS, Windows and Linux Sandbox
  Conformance run on push to `main`, `workflow_dispatch`, or a pull request labelled `full-ci`;
  docs-only changes skip all compilation. Required check names are unchanged (a skipped job reports
  success). The separate `Windows Check` job and the per-push release builds are gone.
- **No Nix in CI.** Jobs install the toolchain pinned by `rust-toolchain.toml` with rustup
  (`.github/actions/setup-rust`), which also fixes the ubuntu build cache that was saving 0 bytes.
- **Nightly skips docs-only changes** (it already skipped when nothing changed).
- `scripts/check-version-consistency.sh` also checks that CLAUDE.md "Current version" matches
  `Cargo.toml`; `check-no-ip-docs.sh --all` now runs in CI.
- **Fixes found by the first local Linux run.** `ta doctor` no longer treats a PID above `i32::MAX`
  as alive (`kill -0 -1` signals everything, so a dead PID file read as live on some Linux
  distributions). An external channel plugin that exits without reading its stdin now reports its
  exit status and stderr instead of intermittently reporting a broken-pipe write error.

## v0.17.12-alpha.11 — Key custody, one state directory, model/effort profiles, sandboxing and cost tracking

Covers `git log v0.17.11-alpha.7..main` (about 280 commits; PR numbers in parentheses). Not yet
done in this release: Apple notarization and Windows code signing (phase v0.17.12.12, blocked;
release checksums are Sigstore-signed), and model-mapping correctness for Codex/Gemini/Ollama
(phase v0.17.12.33, pending).

**Highlights**

- **Key custody and Keychain prompts (#742, #744, #745, #673, #661, #617).** Persistent per-user
  custody choice (`ta credentials custody status|file|keychain`); dev installs default to file
  custody; the vault is never silently re-keyed; secret directories are `0700`; every keychain wait
  is bounded; tests can no longer touch the real Keychain. Details below.
- **State directory unification and `ta state` (#747, #750).** One resolver, `ta state show|relocate`,
  a verified one-time migration and a `ta doctor` check. Windows migration failures fixed in #750.
- **Model tier and effort profiles (#748).** `ta run --profile|--tier|--effort`, mapped per agent
  framework from data. `--phases` and `--sub-goals` runs ignore these flags.
- **`ta doctor` (#746, #741, #744, #747).** Warns about a running daemon with no agent login
  and about committable `*.cred` files; key custody and state-directory checks.
- **Cost reporting (#674, #681, #682, #711, #725, #722).** Goal spend and origin-class tracking,
  `ta cost report` across goals and interactive sessions, output-token split, native per-goal export
  and Meridian export, `plan_usage_pct` / `budget_used_pct` usage metrics.
- **Windows (#689, #690, #692, #670, #750).** AppContainer sandbox parity with measured results,
  CI process-leak fixes, and the state-dir migration fix. End-to-end Windows behaviour is only
  covered by CI, not by a real-world soak.
- **Sandbox and security hardening (#684, #686, #687, #689, #691, #694, #666, #669, #703-#705).**
  Sandbox launch plumbing, macOS Seatbelt v2 with egress proxy, Linux bubblewrap/Landlock/seccomp,
  default-on isolation for CoS and team roles, remaining red-team findings, deny-by-default gate,
  Discord channel access control, wiki writes through draft review. Real Linux/macOS/Windows
  end-to-end enforcement has been exercised mainly through tests and CI.
- **Chief-of-Staff / chat mode and Wayfinder (#635-#639, #713-#715, #728, #737).** Chat-mode secure
  launch, read-only persona tool surface, Wayfinder read tools, goal/KPI/datapoint proposals,
  task intake funnel.
- **Other notable changes.** Atomic apply and rules engine/postures (#675, #677); VCS detection and
  `ta setup vcs` (#678); golden-set capture and replay (#697); swappable optimizer slots and RTK
  optimizer (#698, #707); one `ta cleanup` engine (#708); automatic pruning of merged branches and
  worktrees (#712); shared build cache and daemon GC for goal agents (#726, #730); phase resolution
  for `ta run` (#716); draft apply fidelity (#663); automatic version sync (#660, #738, #739);
  daemon project identity on every response and `.ta/daemon.json` as the one daemon address record; daemon self-update
  library (#665); wake-on-demand listeners and persistent agent ids (#615, #624); `ta credentials
  update` (#616); Wayfinder wiki MCP tools (#618, #619); cost-experiment framework (#622);
  constitution shadow mode (#733, #736); plan-merge fixes (#620, #710); `blocked` plan phase status
  (#720).

### Per-change detail

### One state-directory resolver, `ta state`, and a verified one-time migration

- **One resolver for the state directory** (`ta_credentials::state_dir`): `TA_STATE_DIR`, then a
  persisted per-user pointer file (`<user dir>/state-dir`, owner-only, written atomically), then
  the cargo-test temp dir, then the OS default. `ta`, `ta-daemon`, the credential broker, audit
  and rules code all go through it (`StateLocator::from_env()` now returns a `Result`). With no
  home directory and no override it fails with what to set instead of using the current
  directory. Why: the root used to be decided independently per process by an environment
  variable, so a daemon started without `TA_STATE_DIR` quietly used a different directory than
  the installer that set it.
- **`ta state show | relocate --to <dir> | user-dir`** — `show` prints the resolved directory and
  which rule decided it (and the old location's status); `relocate` migrates once and records the
  pointer, for installers; `user-dir` prints the per-user directory for shell scripts.
- **One-time migration**: refuses while a daemon or NATS server runs against the old directory;
  copies to a temp dir, verifies file count and the SHA-256 of every file (and, on unix, the
  0600/0700 permissions; Windows has no mode bits, so there the copy is verified by content and
  entry kind and symlinks are not copied), then
  renames atomically; writes `.migrated-from` (new) and `MIGRATED-TO` (old); never deletes or
  modifies the old data and prints its path, size and the `rm -rf` command; idempotent; never
  merges when both sides hold state (reports both, sizes and mtimes); reports stale temp dirs.
  `ta` and `ta-daemon` run it automatically, once, when the configured directory is empty and the
  OS default holds unmigrated state.
- **`ta doctor` "State directory" check** — shows the resolved directory and its source, warns on
  an unmigrated or still-present old directory, on `TA_STATE_DIR` disagreeing with the pointer,
  on an unusable pointer, and on leftover temp dirs.
- **`scripts/custody-decision.sh`** no longer carries its own copy of the rule: it asks
  `ta state user-dir`, and mirrors the documented precedence only when `ta` cannot answer; with
  no HOME and no override it now writes nothing instead of a cwd-relative path.

### Model tier and reasoning effort, mapped per agent framework

- **`ta run --profile <name> | --tier <t> | --effort <e>`** — one vocabulary for "which model and
  how hard it reasons": tiers `local|light|standard|max`, efforts `default|low|medium|high|xhigh|max`,
  shipped profiles `local`, `light`, `standard`, `deep` (standard + `xhigh`), `max` (max tier + `max`
  effort). `--model <id>` still works and beats the tier's model. Unknown names fail with the valid
  values. `.ta/team.toml` members may set `profile` / `effort`; the daemon's `build_ta_run_args`
  passes `--profile` / `--effort`.
- **Per-framework mapping is data.** New `model_map` / `effort_map` / `env` fields on the agent
  manifest (YAML and TOML), a shipped catalog (`crates/ta-runtime/src/model-profiles.default.toml`:
  Claude Code, Codex, Gemini CLI, Antigravity, Ollama; every id carries its vendor doc URL and read
  date), and `.ta/model-profiles.toml` / `~/.config/ta/model-profiles.toml` overrides and custom
  profiles. Precedence: CLI flags, `PLAN.md` phase markers (`<!-- profile: deep -->`), project file,
  user file, manifest, shipped catalog. There is no per-framework branching in `run.rs`.
- **Nothing is dropped silently.** An effort or tier a framework cannot express (e.g. Gemini CLI
  effort, unmapped Ollama/Antigravity tiers, Claude Flow) prints a warning with how to configure it.
  Each launch logs `agent= tier= effort= model= effort_applied= via= model_via=`; the same values
  are stored on the goal (`ta goal status` `Model:` line) and `ta cost report --group-by tier,effort`
  groups by them.
- **Claude Code effort is passed as `CLAUDE_CODE_EFFORT_LEVEL`**, an explicit env var that survives
  the sandbox environment scrub; settings injection and the stream-json headless arguments are
  unchanged. Codex now receives `--model` and `-c model_reasoning_effort`. Manifest `env` reaches
  the agent through the sandbox scrub (deny-listed names are still dropped, with a warning).
- **Not yet forwarded:** `--phases` (serial-phases) and `--sub-goals` (swarm) runs warn and ignore
  these flags, as they already did for `--model`.

### `ta doctor` flags a daemon started without an agent login

- **`ta doctor` "Daemon agent login" check** — when a `ta-daemon` is running (pid from
  `.ta/daemon.pid`, alive), the active agent is `claude-code` and chat-class launches run in the
  sandbox, doctor reads the daemon's environment (`/proc/<pid>/environ` on Linux, `/bin/ps -E` on
  macOS) and warns when none of `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN` is set: goals the daemon starts would fail with "The sandboxed agent has no
  usable login". The warning names what to set, where, and `ta daemon restart`. Only variable names
  are examined or printed, never values. When the environment cannot be read (another user's
  process, Windows) doctor says "cannot check" instead of warning. New helper
  `sandbox_launch::chat_launch_is_sandboxed` compiles the same profile `ta run` does.

### Persistent per-user key custody (fixes endless macOS Keychain prompts on dev builds)

- **`ta credentials custody status|file|keychain`** — one persistent per-user choice between the OS
  keychain and owner-only file custody, stored in `<user state dir>/custody`. Resolved in one
  function (`ta_credentials::custody`) for the vault, control-plane keys, human credential, MCP
  gateway and shadow spawn: `TA_NO_KEYCHAIN`, then the cargo-test guard, then the custody file,
  then the keychain default (production unchanged). `file`/`keychain` migrate the current project's
  secrets with a verified copy and print the exact commands to remove the old copies.
- **`install_local.sh`** — when the signed binary has no Team ID (self-signed local cert), writes
  `file` to the custody file (never overwriting an existing one) and prints why. Root cause: a
  keychain item's `partition_id` ACL keys on the Team ID, or the cdhash without one, so every
  rebuild re-prompted; signing only fixes the app list. `CLAUDE.md` corrected accordingly.
- **A missing vault key is no longer replaced silently** — when `.ta/credentials.json` already holds
  ciphertext and no key is found in the active custody store, `FileVault::open` now fails with
  `VaultError::KeyMissing` (what happened, where it looked, that no replacement key was created, and
  `ta credentials custody file|keychain` or moving the vault aside) instead of generating a key
  that cannot decrypt it and shadows the real one. A project with no vault, a legacy plaintext
  vault, or an empty file still gets a key. Root and audit keys are unchanged.
- **`ta doctor` "Key custody" check** — on macOS, warns when the effective custody is `keychain` but
  the running `ta` has no Team ID (or `codesign` cannot report one), names why (cdhash identity,
  re-prompt on every rebuild) and the fix (`ta credentials custody file`, once per project with a
  `.ta/credentials.json`; or `TA_CODESIGN_IDENTITY` in `.env.local`). Never reads the keychain and
  does not run `codesign` in file custody. The vault-key-missing doctor advice no longer tells you
  to run a command that would create a replacement key; it points at `ta credentials custody file`.
- **`docs/security/KEY-CUSTODY.md`** — each secret (root key, audit key, vault key, human
  credential, vault file): what it protects, where it lives per custody mode, file/dir modes, who can
  read it; what an agent can and cannot see; how secrets stay out of git; what CI verifies and what
  has no CI check; recovery (and the key-rotation commands that do not exist). Linked from
  `README.md` (new Security section), `CONTRIBUTING.md` (new "Local setup: keys, keychain and
  custody") and `docs/USAGE.md`.
- **Secret directories are created `0700`** — the per-project state directory, its `secrets/`
  directory and `~/.config/ta/human` were created with the default umask. Newly created ones are now
  owner-only; an existing directory keeps its mode.
- **Sandbox denies `~/.config/ta/human`** — the `ta login` credential in file custody was not in the
  macOS Seatbelt deny list (`HOME_PROTECTED`); it is now.
- **`scripts/secret-scan.sh`** also matches age secret keys and `tahc_` human credentials, and fails
  on a tracked or not-ignored `.ta/credentials.json`, `.ta/*.key|*.pem|*.cred` or any `*.cred`
  (the self-test covers both). A test fixture that held an age-shaped literal now builds it at runtime.
- **Tests** — `ta init` is checked with a real `git check-ignore` for every secret path under `.ta/`;
  directory modes, the sandbox deny entry and the doctor check (with injected `codesign` output and
  custody resolution) have unit tests.

---

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
