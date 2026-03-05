# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** Implement v0.7 — Extensibility and all sub-phases
**Goal ID:** 69ab7953-7daf-422f-92f2-9f182845c50b

## Plan Context

Plan progress:
- [x] Phase 0 — Repo Layout & Core Data Model
- [x] Phase 1 — Kernel: Audit, Policy, Changeset, Workspace
- [x] Phase 2 — MCP Gateway, Goal Lifecycle, CLI
- [x] Phase 3 — Transparent Overlay Mediation
- [x] Phase 4a — Agent Prompt Enhancement
- [x] Phase 4a.1 — Plan Tracking & Lifecycle
- [x] Phase 4b — Per-Artifact Review Model
- [x] Phase 4c — Selective Review CLI
- [ ] Phase v0.1 — Public Preview & Call for Feedback
- [ ] Phase v0.1.1 — Release Automation & Binary Distribution
- [x] Phase v0.1.2 — Follow-Up Goals & Iterative Review
- [x] Phase v0.2.0 — SubmitAdapter Trait & Git Implementation
- [x] Phase v0.2.1 — Concurrent Session Conflict Detection
- [x] Phase v0.2.2 — External Diff Routing
- [x] Phase v0.2.3 — Tiered Diff Explanations & Output Adapters
- [x] Phase v0.2.4 — Terminology & Positioning Pass
- [x] Phase v0.3.0 — Review Sessions
- [x] Phase v0.3.0.1 — Consolidate `pr.rs` into `draft.rs`
- [x] Phase v0.3.1 — Plan Lifecycle Automation
- [x] Phase v0.3.1.1 — Configurable Plan Format Parsing
- [x] Phase v0.3.1.2 — Interactive Session Orchestration
- [x] Phase v0.3.2 — Configurable Release Pipeline (`ta release`
- [x] Phase v0.3.3 — Decision Observability & Reasoning Capture
- [x] Phase v0.3.4 — Draft Amendment & Targeted Re-Work
- [x] Phase v0.3.5 — Release Pipeline Fixes
- [x] Phase v0.3.6 — Draft Lifecycle Hygiene
- [x] Phase v0.4.0 — Intent-to-Access Planner & Agent Alignment Profiles
- [x] Phase v0.4.1 — Macro Goals & Inner-Loop Iteration
- [x] Phase v0.4.1.1 — Runtime Channel Architecture & Macro Session Loop
- [x] Phase v0.4.1.2 — Follow-Up Draft Continuity
- [x] Phase v0.4.2 — Behavioral Drift Detection
- [x] Phase v0.4.3 — Access Constitutions
- [x] Phase v0.4.4 — Interactive Session Completion
- [x] Phase v0.4.5 — CLI UX Polish
- [x] Phase v0.5.0 — Credential Broker & Identity Abstraction
- [x] Phase v0.5.1 — MCP Tool Call Interception
- [x] Phase v0.5.2 — Minimal Web Review UI
- [x] Phase v0.5.3 — Additional ReviewChannel Adapters
- [x] Phase v0.5.4 — Context Memory Store (ruvector integration
- [x] Phase v0.5.5 — RuVector Memory Backend
- [x] Phase v0.5.6 — Framework-Agnostic Agent State
- [x] Phase v0.5.7 — Semantic Memory Queries & Memory Dashboard
- [x] Phase v0.6.0 — Session & Human Control Plane (Layer 3
- [x] Phase v0.6.1 — Unified Policy Config (Layer 2
- [x] Phase v0.6.2 — Resource Mediation Trait (Layer 1
- [x] Phase v0.6.3 — Active Memory Injection & Project-Aware Key Schema
- [x] Phase v0.7.0 — Channel Registry (Layer 5
- [x] Phase v0.7.1 — API Mediator (Layer 1
- [x] Phase v0.7.2 — Agent-Guided Setup
- [x] Phase v0.7.3 — Project Template Repository & `ta init`
- [x] Phase v0.7.4 — Memory & Config Cleanup
- [x] Phase v0.7.5 — Interactive Session Fixes & Cross-Platform Release
- [x] Phase v0.7.6 — Interactive Developer Loop (`ta dev`)
- [ ] Phase v0.7.7 — Agent Framework Registry & Setup Integration
- [ ] Phase v0.8.0 — Event System & Subscription API (Layer 3 → projects
- [ ] Phase v0.8.1 — Solution Memory Export
- [ ] Phase v0.9.0 — Distribution & Packaging
- [ ] Phase v0.9.1 — Native Windows Support
- [ ] Phase v0.9.2 — Sandbox Runner (optional hardening, Layer 2

## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** 69ab7953-7daf-422f-92f2-9f182845c50b

## Prior Context (from TA memory)

The following knowledge was captured from previous sessions across all agent frameworks.

- **[history] goal:6790e9a3-9a80-4619-8734-486727bedf71:complete**: {"change_summary":{"changes":[{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":".release-draft.md","what":"Replaced v0.4.5-alpha release notes with comprehensive v0.6.3-alpha release notes covering 7 new features, 6 improvements, and 2 bug fixes synthesized from 7 commits since v0.5.7-alpha.","why":"Release goal requested user-facing release notes for v0.6.3-alpha covering the v0.6.x Platform Substrate work."}],"dependency_notes":"Single independent file change — release notes only.","summary":"Generated release notes for v0.6.3-alpha covering all changes since v0.5.7-alpha: Active Memory Injection, Session & Control Plane, Unified Policy Config, Resource Mediation, and supporting improvements."},"changed_files":[".release-draft.md"],"title":"release: Generate release notes"}
- **[history] goal:3f1a9052-35a2-4fcb-9207-f13280f94ba4:complete**: {"change_summary":{"changes":[{"action":"modified","depended_by":["crates/ta-memory/src/auto_capture.rs","crates/ta-memory/src/fs_store.rs","crates/ta-memory/src/ruvector_store.rs","crates/ta-memory/src/key_schema.rs","apps/ta-cli/src/commands/context.rs","apps/ta-cli/src/commands/run.rs","crates/ta-mcp-gateway/src/server.rs"],"depends_on":[],"independent":false,"path":"crates/ta-memory/src/store.rs","what":"Added NegativePath and State variants to MemoryCategory enum. Added phase_id: Option<String> field to MemoryEntry (serde-compatible with backward compat), StoreParams, and MemoryQuery. Updated Display, from_str_lossy, and default trait impl for store_with_params to handle new fields.","why":"v0.6.3 requires negative path tracking for rejected drafts, mutable state snapshots, and phase-based memory filtering to inject only relevant context per plan phase."},{"action":"created","depended_by":["crates/ta-memory/src/lib.rs","apps/ta-cli/src/commands/context.rs"],"depends_on":[],"independent":false,"path":"crates/ta-memory/src/key_schema.rs","what":"New module implementing project-aware key schema: ProjectType enum (RustWorkspace, TypeScript, Python, Go, Generic) with auto-detection from filesystem signals, KeyDomainMap mapping abstract concepts to project-specific key prefixes, KeySchema resolver with .ta/memory.toml config parsing, and key generation helpers (module_map_key, module_key, type_key, negative_path_key, state_key). 10 tests covering detection, config override, custom domains, key generation, and config parsing.","why":"Memory keys need consistent project-aware naming (arch:crate-map vs arch:package-map) so agents across sessions use the same vocabulary for architectural knowledge."},{"action":"modified","depended_by":["apps/ta-cli/src/commands/run.rs"],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/auto_capture.rs","what":"Added phase_id field to GoalCompleteEvent, DraftRejectEvent, HumanGuidanceEvent structs. Enhanced on_goal_complete to extract architectural module map from change_summary (creates Architecture-category entries with module names parsed from file paths). Changed on_draft_reject to use NegativePath category with neg:{phase}:{slug} keys. Added build_memory_context_section_with_phase() with phase filtering, category-priority ordering, and structured markdown output grouped by category headings. Added extract_module_name() helper. 5 new tests: arch extraction, negative path creation, phase-filtered injection, backward compatibility, category priority ordering.","why":"v0.6.3 requires agents to see structured, phase-relevant context on launch — not a flat list of all memories. Negative paths prevent repeating rejected approaches. Architectural knowledge extraction gives agents instant understanding of the module layout."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/fs_store.rs","what":"Added phase_id to MemoryEntry construction in store_with_params. Added phase_id filter to lookup() — matches entries for the requested phase OR global entries (phase_id: None).","why":"FsMemoryStore backend must persist and filter by the new phase_id field."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/ruvector_store.rs","what":"Added phase_id to MemoryEntry construction in store_with_params. Added phase_id to metadata serialization (entry_to_metadata) and deserialization (metadata_to_entry). Added phase_id filter to lookup(). Updated migration test to include phase_id field.","why":"RuVector backend must serialize/deserialize and filter by phase_id in its metadata layer."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/key_schema.rs"],"independent":false,"path":"crates/ta-memory/src/lib.rs","what":"Added key_schema module declaration and public re-exports for KeyDomainMap, KeySchema, ProjectType.","why":"New key_schema module needs to be exposed as part of the ta-memory public API."},{"action":"modified","depended_by":[],"depends_on":[],"independent":false,"path":"crates/ta-memory/Cargo.toml","what":"Bumped version from 0.5.7-alpha to 0.6.3-alpha. Changed default features from [] to [\"ruvector\"] to enable semantic search by default.","why":"Version bump for v0.6.3 release. RuVector default-on is a v0.6.3 requirement so agents get semantic search without extra configuration."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/auto_capture.rs","crates/ta-memory/src/store.rs"],"independent":false,"path":"apps/ta-cli/src/commands/run.rs","what":"Enhanced build_memory_context_section_for_inject to accept plan_phase parameter, try RuVector backend first (when .ta/memory.rvf exists), and call build_memory_context_section_with_phase for phase-aware injection. Updated inject_claude_md to pass plan_phase to memory injection. Updated auto_capture_goal_completion to pass goal.plan_phase as phase_id.","why":"Context injection must be phase-aware (v0.6.3) and use semantic search when available for better relevance ranking."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/key_schema.rs"],"independent":false,"path":"apps/ta-cli/src/commands/context.rs","what":"Added Schema variant to ContextCommands enum. Added show_schema() function that resolves and displays the project's key schema (project type, backend, domain mapping, special key patterns). Updated store_entry StoreParams construction to use ..Default::default() for forward compatibility. Updated lookup MemoryQuery to use ..Default::default().","why":"Users need a way to inspect the auto-detected key schema for their project, and existing code needs forward-compatible struct construction."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"apps/ta-cli/Cargo.toml","what":"Bumped version from 0.6.0-alpha to 0.6.3-alpha.","why":"Version bump for v0.6.3 release."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/auto_capture.rs"],"independent":false,"path":"crates/ta-mcp-gateway/src/server.rs","what":"Added phase_id: goal.plan_phase.clone() to DraftRejectEvent construction in the MCP submit handler.","why":"MCP gateway constructs DraftRejectEvent which now requires phase_id field."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"CLAUDE.md","what":"Updated 'Current version' from 0.6.0-alpha to 0.6.3-alpha (both occurrences).","why":"Version tracking must match the release."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"PLAN.md","what":"Added Completed section to v0.6.3 phase listing all 12 implemented items with checkmarks. Added Remaining (deferred) section noting backend toggle consumption and guidance domain classification.","why":"Plan progress tracking must reflect what was actually implemented vs deferred."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"docs/USAGE.md","what":"Updated auto-capture event table (negative_path category for rejections, architecture for goal completions). Added phase-aware injection docs, project-aware key schema table, ta context schema command, .ta/memory.toml config, negative paths section. Updated ruvector section to reflect default-on status. Updated 'What gets stored' table with NegativePath and State categories.","why":"User-facing documentation must cover new v0.6.3 commands, categories, and configuration."}],"dependency_notes":"Core dependency chain: store.rs (types) → key_schema.rs + auto_capture.rs + fs_store.rs + ruvector_store.rs (implementations) → run.rs + context.rs + server.rs (consumers). The key_schema.rs is independent of auto_capture.rs. Documentation files (CLAUDE.md, PLAN.md, USAGE.md) are independent of each other and of all code changes.","summary":"Implement v0.6.3 Active Memory Injection & Project-Aware Key Schema: agents start smart with phase-filtered, category-prioritized context injection, project-aware key domains, NegativePath/State categories, architectural knowledge extraction from goal completions, and RuVector default-on."},"changed_files":["crates/ta-memory/src/store.rs","crates/ta-memory/src/key_schema.rs","crates/ta-memory/src/auto_capture.rs","crates/ta-memory/src/fs_store.rs","crates/ta-memory/src/ruvector_store.rs","crates/ta-memory/src/lib.rs","crates/ta-memory/Cargo.toml","apps/ta-cli/src/commands/run.rs","apps/ta-cli/src/commands/context.rs","apps/ta-cli/Cargo.toml","crates/ta-mcp-gateway/src/server.rs","CLAUDE.md","PLAN.md","docs/USAGE.md"],"title":"Implement v0.6.3 Active Memory Injection"}
- **[history] goal:fda52e38-f87b-49c2-9de6-f1ea17a3b017:complete**: {"change_summary":{"changes":[{"action":"created","depended_by":["crates/ta-mediation/src/lib.rs"],"depends_on":["Cargo.toml"],"independent":false,"path":"crates/ta-mediation/Cargo.toml","what":"New crate manifest for ta-mediation v0.6.0-alpha with dependencies on serde, uuid, chrono, thiserror, tracing, ta-workspace, ta-changeset","why":"v0.6.2 Resource Mediation Trait requires a new crate to house the ResourceMediator trait and shared types"},{"action":"created","depended_by":[],"depends_on":["crates/ta-mediation/Cargo.toml"],"independent":false,"path":"crates/ta-mediation/src/lib.rs","what":"Module declarations and public re-exports for error, mediator, fs_mediator, registry types","why":"Crate entry point organizing the ta-mediation public API"},{"action":"created","depended_by":["crates/ta-mediation/src/mediator.rs","crates/ta-mediation/src/fs_mediator.rs","crates/ta-mediation/src/registry.rs"],"depends_on":["crates/ta-mediation/Cargo.toml"],"independent":false,"path":"crates/ta-mediation/src/error.rs","what":"MediationError enum with variants: UnsupportedScheme, StagingFailed, ApplyFailed, RollbackFailed, NoMediator, InvalidUri, Io, Workspace","why":"Error types needed by ResourceMediator implementations and MediatorRegistry"},{"action":"created","depended_by":["crates/ta-mediation/src/fs_mediator.rs","crates/ta-mediation/src/registry.rs"],"depends_on":["crates/ta-mediation/src/error.rs"],"independent":false,"path":"crates/ta-mediation/src/mediator.rs","what":"Core types (ProposedAction, StagedMutation, MutationPreview, ActionClassification, ApplyResult) and ResourceMediator trait with 6 methods (scheme, stage, preview, apply, rollback, classify). 5 tests.","why":"The ResourceMediator trait is the central abstraction for v0.6.2, generalizing the file staging pattern to any resource type"},{"action":"created","depended_by":[],"depends_on":["crates/ta-mediation/src/mediator.rs","crates/ta-mediation/src/error.rs"],"independent":false,"path":"crates/ta-mediation/src/fs_mediator.rs","what":"FsMediator implementing ResourceMediator for fs:// URIs — stage writes to staging dir, preview generates diffs, apply copies to source, rollback removes staged files, classify maps read verbs to ReadOnly. 9 tests.","why":"First concrete ResourceMediator implementation, proving the trait works for the existing file staging pattern"},{"action":"created","depended_by":[],"depends_on":["crates/ta-mediation/src/mediator.rs","crates/ta-mediation/src/error.rs"],"independent":false,"path":"crates/ta-mediation/src/registry.rs","what":"MediatorRegistry routing URIs to mediators by scheme, with register(), get(), route(), schemes(), has_scheme(), len(), is_empty() and extract_scheme() helper. 8 tests.","why":"Central routing layer that connects URI-based resource references to the correct ResourceMediator implementation"},{"action":"created","depended_by":["crates/ta-session/src/lib.rs"],"depends_on":["Cargo.toml"],"independent":false,"path":"crates/ta-session/Cargo.toml","what":"New crate manifest for ta-session v0.6.0-alpha with dependencies on serde, uuid, chrono, thiserror, tracing, ta-goal, ta-changeset","why":"v0.6.0 Session & Human Control Plane requires a new crate for session lifecycle management"},{"action":"created","depended_by":["apps/ta-cli/src/commands/session.rs"],"depends_on":["crates/ta-session/Cargo.toml"],"independent":false,"path":"crates/ta-session/src/lib.rs","what":"Module declarations and public re-exports for TaSession, SessionState, ConversationTurn, SessionManager, SessionError","why":"Crate entry point organizing the ta-session public API"},{"action":"created","depended_by":["crates/ta-session/src/session.rs","crates/ta-session/src/manager.rs"],"depends_on":["crates/ta-session/Cargo.toml"],"independent":false,"path":"crates/ta-session/src/error.rs","what":"SessionError enum with variants: NotFound, InvalidTransition, AlreadyExists, Io, Serialization","why":"Error types for session lifecycle operations"},{"action":"created","depended_by":["crates/ta-session/src/manager.rs"],"depends_on":["crates/ta-session/src/error.rs"],"independent":false,"path":"crates/ta-session/src/session.rs","what":"TaSession struct with SessionState enum (Starting/AgentRunning/DraftReady/WaitingForReview/Iterating/Completed/Aborted/Paused/Failed), ConversationTurn for tracking agent_context and human_feedback, state transition enforcement, checkpoint mode. 12 tests.","why":"Core session object tracking conversational continuity across review iterations — the 'one conversation' model for human-agent interaction"},{"action":"created","depended_by":["apps/ta-cli/src/commands/session.rs"],"depends_on":["crates/ta-session/src/session.rs","crates/ta-session/src/error.rs"],"independent":false,"path":"crates/ta-session/src/manager.rs","what":"SessionManager for CRUD persistence in .ta/sessions/<id>.json with create(), load(), save(), find_for_goal(), list(), list_active(), pause(), resume(), abort(), delete(), exists(). 8 tests.","why":"Persistent storage and lifecycle operations for TaSession objects"},{"action":"created","depended_by":["crates/ta-policy/src/cascade.rs","crates/ta-policy/src/engine.rs","crates/ta-policy/src/lib.rs"],"depends_on":["crates/ta-policy/Cargo.toml"],"independent":false,"path":"crates/ta-policy/src/document.rs","what":"PolicyDocument unified config surface with SecurityLevel enum (Open<Checkpoint<Supervised<Strict), PolicyDefaults, PolicyEnforcement (Warning<Error<Strict), AutoApproveConfig, SchemePolicy, EscalationConfig, AgentPolicyOverride, BudgetConfig. 8 tests.","why":"v0.6.1 needs a single PolicyDocument struct that all supervision config resolves to, with ordered security levels for tighten-only merging"},{"action":"created","depended_by":["crates/ta-policy/src/engine.rs"],"depends_on":["crates/ta-policy/Cargo.toml"],"independent":false,"path":"crates/ta-policy/src/context.rs","what":"PolicyContext runtime state with goal_id, session_id, agent_id, budget_spent, action_count, drift_score and helper methods (is_over_budget, is_budget_warning, is_drifting). 6 tests.","why":"Policy evaluation needs runtime context (budget, drift, action count) alongside static grants for v0.6.1 runtime-aware decisions"},{"action":"created","depended_by":[],"depends_on":["crates/ta-policy/src/document.rs","crates/ta-policy/src/error.rs"],"independent":false,"path":"crates/ta-policy/src/cascade.rs","what":"PolicyCascade loading and merging PolicyDocument from 6 layers (built-in → project → workflow → agent → constitution → CLI), CliOverrides struct. Tighten-only merge: security level only increases, approval verbs union, action limits take lower, escalation thresholds take lower. 10 tests.","why":"v0.6.1 6-layer cascade is the merge mechanism that produces a single PolicyDocument from multiple configuration sources"},{"action":"modified","depended_by":[],"depends_on":["crates/ta-policy/src/document.rs","crates/ta-policy/src/context.rs","crates/ta-policy/src/cascade.rs"],"independent":false,"path":"crates/ta-policy/src/lib.rs","what":"Added cascade, context, document module declarations and public re-exports for PolicyCascade, CliOverrides, PolicyContext, PolicyDocument, PolicyDefaults, PolicyEnforcement, AutoApproveConfig, SchemePolicy, EscalationConfig, AgentPolicyOverride, SecurityLevel, BudgetConfig","why":"New modules need to be exposed as part of the ta-policy public API"},{"action":"modified","depended_by":["crates/ta-policy/src/cascade.rs"],"depends_on":[],"independent":false,"path":"crates/ta-policy/src/error.rs","what":"Added IoError variant (path + source) and ConfigError variant for YAML parse errors","why":"PolicyCascade needs to report I/O errors when loading YAML files and config parse errors"},{"action":"modified","depended_by":[],"depends_on":["crates/ta-policy/src/document.rs","crates/ta-policy/src/context.rs"],"independent":false,"path":"crates/ta-policy/src/engine.rs","what":"Added evaluate_with_document() method layering document-level checks (scheme approval, agent overrides/forbidden actions, drift escalation, action count limits, budget limits, supervised mode) on top of manifest-based evaluation. Added extract_uri_scheme() helper. 5 new tests.","why":"The PolicyEngine needs a document-aware evaluation path that uses the unified PolicyDocument for runtime policy decisions"},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"crates/ta-policy/Cargo.toml","what":"Bumped version from 0.4.5-alpha to 0.6.0-alpha","why":"Version bump for v0.6 release"},{"action":"modified","depended_by":[],"depends_on":[],"independent":false,"path":"crates/ta-goal/src/events.rs","what":"Added 6 new TaEvent variants: SessionPaused, SessionResumed, SessionAborted, DraftBuilt, ReviewDecision, SessionIteration. Added helper constructors for each. Added event_type() match arms. 4 new tests.","why":"v0.6.0 session lifecycle needs events published to the event stream for observability"},{"action":"modified","depended_by":[],"depends_on":["crates/ta-session/src/manager.rs","apps/ta-cli/Cargo.toml"],"independent":false,"path":"apps/ta-cli/src/commands/session.rs","what":"Added Pause, Abort, and Status subcommands to SessionCommands. Added pause_session(), abort_session(), session_status(), resolve_session_id() functions using ta-session::SessionManager.","why":"v0.6.0 human control plane commands: ta session pause/abort/status for session lifecycle management"},{"action":"modified","depended_by":["apps/ta-cli/src/commands/session.rs"],"depends_on":["crates/ta-session/Cargo.toml"],"independent":false,"path":"apps/ta-cli/Cargo.toml","what":"Bumped version from 0.5.7-alpha to 0.6.0-alpha. Added ta-session dependency.","why":"Version bump for v0.6 release and new session management dependency"},{"action":"modified","depended_by":["crates/ta-mediation/Cargo.toml","crates/ta-session/Cargo.toml"],"depends_on":[],"independent":false,"path":"Cargo.toml","what":"Added ta-mediation and ta-session to workspace members list","why":"New crates must be registered as workspace members"},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"CLAUDE.md","what":"Updated 'Current version' from 0.5.7-alpha to 0.6.0-alpha","why":"Version tracking must match the release"},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"PLAN.md","what":"Added Completed sections to v0.6.0, v0.6.1, v0.6.2 phases listing all implemented items with checkmarks. Added Remaining (deferred) items for each phase.","why":"Plan progress tracking must reflect what was actually implemented vs deferred"},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"docs/USAGE.md","what":"Updated version to v0.6.0-alpha. Added Session Lifecycle (v0.6.0), Unified Policy Config (v0.6.1), Resource Mediation (v0.6.2) documentation sections. Updated roadmap tables to mark v0.6 phases as Done and align v0.7+ phase descriptions with restructured plan.","why":"User-facing documentation must cover new commands (ta session pause/abort/status), .ta/policy.yaml configuration, and ResourceMediator extension point"}],"dependency_notes":"Three core work streams: (1) ta-mediation crate (6 files) — self-contained, depends only on workspace root Cargo.toml for registration. (2) ta-policy extensions (3 new files + 2 modified) — document.rs is the root dependency, cascade.rs and engine.rs depend on it. (3) ta-session crate (5 files) — self-contained, consumed by CLI session commands. The CLI session.rs depends on ta-session. All three streams are architecturally independent but share the version bump. docs/USAGE.md, PLAN.md, CLAUDE.md are independent documentation updates.","summary":"Implement v0.6 Platform Substrate: three new architectural layers. (1) ta-mediation crate with ResourceMediator trait, FsMediator, and MediatorRegistry for generalizing staging to any resource. (2) ta-policy extensions with PolicyDocument, PolicyCascade (6-layer tighten-only merge), PolicyContext, and evaluate_with_document(). (3) ta-session crate with TaSession lifecycle, SessionManager, ConversationTurn tracking. Plus new TaEvent variants, CLI session commands (pause/abort/status), version bump to 0.6.0-alpha."},"changed_files":["crates/ta-mediation/Cargo.toml","crates/ta-mediation/src/lib.rs","crates/ta-mediation/src/error.rs","crates/ta-mediation/src/mediator.rs","crates/ta-mediation/src/fs_mediator.rs","crates/ta-mediation/src/registry.rs","crates/ta-session/Cargo.toml","crates/ta-session/src/lib.rs","crates/ta-session/src/error.rs","crates/ta-session/src/session.rs","crates/ta-session/src/manager.rs","crates/ta-policy/src/document.rs","crates/ta-policy/src/context.rs","crates/ta-policy/src/cascade.rs","crates/ta-policy/src/lib.rs","crates/ta-policy/src/error.rs","crates/ta-policy/src/engine.rs","crates/ta-policy/Cargo.toml","crates/ta-goal/src/events.rs","apps/ta-cli/src/commands/session.rs","apps/ta-cli/Cargo.toml","Cargo.toml","CLAUDE.md","PLAN.md","docs/USAGE.md"],"title":"Implement v0.6 — Platform Substrate - all phases"}
- **[history] goal:4bf6cb1b-1ced-4850-bded-8cdfba2444f2:complete**: {"change_summary":{"changes":[{"action":"modified","depended_by":["crates/ta-memory/src/fs_store.rs","crates/ta-memory/src/ruvector_store.rs","crates/ta-memory/src/auto_capture.rs","crates/ta-memory/src/lib.rs","apps/ta-cli/src/commands/context.rs","crates/ta-mcp-gateway/src/server.rs","crates/ta-daemon/src/web.rs"],"depends_on":[],"independent":false,"path":"crates/ta-memory/src/store.rs","what":"Added expires_at (TTL), confidence fields to MemoryEntry and StoreParams. Added MemoryStats struct. Added find_by_id() and stats() methods to MemoryStore trait with default implementations.","why":"v0.5.7 requires semantic memory queries with TTL support, confidence scoring, and aggregate statistics."},{"action":"modified","depended_by":["crates/ta-daemon/src/web.rs","crates/ta-mcp-gateway/src/server.rs"],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/lib.rs","what":"Added MemoryStats to public re-exports.","why":"Downstream crates need access to the new MemoryStats type."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/fs_store.rs","what":"Updated store_with_params to populate expires_at and confidence fields.","why":"FsMemoryStore backend needs to persist new TTL and confidence fields."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/ruvector_store.rs","what":"Updated store_with_params, entry_to_metadata(), and metadata_to_entry() to handle expires_at and confidence.","why":"RuVector backend needs to serialize/deserialize new fields in its metadata layer."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-memory/src/auto_capture.rs","what":"Added confidence scoring per lifecycle event (goal_complete=0.8, draft_reject=0.6, human_guidance=0.9, auto-promoted=0.9).","why":"Auto-captured memories should have varying confidence based on provenance."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"crates/ta-memory/Cargo.toml","what":"Bumped version from 0.5.6-alpha to 0.5.7-alpha.","why":"Version bump for v0.5.7 release."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"apps/ta-cli/src/commands/context.rs","what":"Added search, similar, explain, stats CLI subcommands. Enhanced store with --category, --expires-in, --confidence flags. Enhanced list with --category filter.","why":"v0.5.7 requires CLI access to semantic memory queries and statistics."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"apps/ta-cli/Cargo.toml","what":"Bumped version from 0.5.6-alpha to 0.5.7-alpha.","why":"Version bump for v0.5.7 release."},{"action":"modified","depended_by":["crates/ta-daemon/assets/index.html"],"depends_on":["crates/ta-memory/src/store.rs","crates/ta-daemon/Cargo.toml"],"independent":false,"path":"crates/ta-daemon/src/web.rs","what":"Added memory_dir to WebState, memory API handlers (list, search, stats, create, delete), routes at /api/memory/*, test isolation fix, and 3 new tests.","why":"v0.5.7 requires a web-based memory dashboard with REST API."},{"action":"modified","depended_by":["crates/ta-daemon/src/web.rs"],"depends_on":["crates/ta-memory/Cargo.toml"],"independent":false,"path":"crates/ta-daemon/Cargo.toml","what":"Added ta-memory dependency.","why":"Web module needs access to the memory store for dashboard API routes."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-daemon/src/web.rs"],"independent":false,"path":"crates/ta-daemon/assets/index.html","what":"Added tab-based navigation (Drafts/Memory), memory dashboard with stats grid, search, create/delete, and confidence bars.","why":"v0.5.7 requires a visual memory dashboard."},{"action":"modified","depended_by":[],"depends_on":["crates/ta-memory/src/store.rs"],"independent":false,"path":"crates/ta-mcp-gateway/src/server.rs","what":"Added stats and similar action handlers to ta_context MCP tool.","why":"MCP-connected agents need access to memory statistics and similarity search."},{"action":"created","depended_by":["PLAN.md","CLAUDE.md","docs/extension-points.md","docs/paid-addons/policy-studio.md","docs/paid-addons/enterprise-channels.md","docs/paid-addons/advanced-mediators.md","docs/paid-addons/compliance-reporting.md"],"depends_on":[],"independent":true,"path":"docs/ADR-product-concept-model.md","what":"ADR defining TA's 5-layer product concept model with extension points, crate map, platform diagram, and restructured roadmap. Uses email:// with provider variants (not gmail:// as top-level). Includes explicit 6-layer policy cascade (layers tighten, never loosen). References paid add-ons by doc link instead of inline details.","why":"Codify TA's architecture as a governance platform with clear layer boundaries and extension points for projects built on top (Virtual Office, Infra Ops)."},{"action":"created","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"docs/extension-points.md","what":"Main extension points documentation describing all 8 TA extension surfaces: ResourceMediator, Policy Documents, Review Channels, Memory Backends, Submit Adapters, Credential Providers, Session Events, Agent Launch Configs. Includes trait signatures, config examples, and built-in vs plugin breakdown.","why":"Users need a single reference for how to build on top of TA — plugins, integrations, and customization."},{"action":"created","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"docs/paid-addons/policy-studio.md","what":"Paid add-on spec for Policy Studio: interactive YAML policy generation, conflict/coverage/escalation-gap analysis, compliance mapping (ISO 42001, EU AI Act, NIST AI RMF, IMDA), drift analysis. No runtime dependency on TA core.","why":"Separate paid add-on documentation from core TA docs per user direction."},{"action":"created","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"docs/paid-addons/enterprise-channels.md","what":"Paid add-on spec for enterprise review/session channels: Teams (Adaptive Cards), ServiceNow (change requests), PagerDuty (escalation), Jira (issue sync). Each implements ChannelFactory trait.","why":"Separate paid add-on documentation from core TA docs per user direction."},{"action":"created","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"docs/paid-addons/advanced-mediators.md","what":"Paid add-on spec for advanced ResourceMediator implementations: DB mediator with session-local staging overlay (read-your-writes without touching real DB until approval), Cloud API mediator with cost estimation, Social Media mediator. DB mediator uses provider-specific staging strategies (Postgres: long-lived transaction, SQLite: file clone, DynamoDB: in-memory write cache).","why":"Separate paid add-on documentation from core TA docs. DB staging overlay addresses requirement that agents see their own writes within a session without modifying the real database."},{"action":"created","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"docs/paid-addons/compliance-reporting.md","what":"Paid add-on spec for compliance evidence package generation: ISO/IEC 42001, EU AI Act, NIST AI RMF, Singapore IMDA framework. Reads from TA's existing data stores, outputs PDF/HTML/JSON.","why":"Separate paid add-on documentation from core TA docs per user direction."},{"action":"modified","depended_by":[],"depends_on":["docs/ADR-product-concept-model.md"],"independent":false,"path":"PLAN.md","what":"Restructured v0.6+ roadmap phases: v0.6.0 Session & Control Plane, v0.6.1 Unified Policy Config, v0.6.2 Resource Mediation Trait, v0.7.0 Channel Registry, v0.7.1 API Mediator, v0.7.2 Agent-Guided Setup, v0.8.0 Event System & Subscription API. Moved Virtual Office and Infra Ops to 'Projects On Top' section as separate projects.","why":"Align the roadmap with the 5-layer product concept model from ADR-product-concept-model.md."},{"action":"modified","depended_by":[],"depends_on":["PLAN.md"],"independent":false,"path":"CLAUDE.md","what":"Updated 'Current version' to 0.5.7-alpha. Updated plan progress checklist with new phase names (v0.6.0 Session & Control Plane, v0.6.1 Unified Policy Config, etc.). Replaced 'Virtual Office Runtime' with separate project entries.","why":"Version tracking and plan progress must match the restructured roadmap."},{"action":"modified","depended_by":[],"depends_on":[],"independent":true,"path":"docs/USAGE.md","what":"Added v0.5.7 documentation: CLI commands (search, similar, explain, stats, store with TTL/confidence/category), memory dashboard section, updated MCP tool section.","why":"User-facing documentation must cover new commands and features."}],"dependency_notes":"Three work streams: (1) v0.5.7 code changes — store.rs is the root dependency, all crate changes depend on it; (2) ADR + PLAN.md restructure — the ADR is the concept document, PLAN.md and CLAUDE.md reflect its roadmap; (3) Extension/paid-addon docs — all depend on the ADR for architectural context. Streams 1 and 2-3 are independent of each other.","summary":"v0.5.7 implementation (semantic memory queries, memory dashboard) + ADR-product-concept-model defining TA's 5-layer architecture + restructured PLAN.md roadmap + extension points docs + paid add-on specs + DB mediation caching design."},"changed_files":["crates/ta-memory/src/store.rs","crates/ta-memory/src/lib.rs","crates/ta-memory/src/fs_store.rs","crates/ta-memory/src/ruvector_store.rs","crates/ta-memory/src/auto_capture.rs","crates/ta-memory/Cargo.toml","apps/ta-cli/src/commands/context.rs","apps/ta-cli/Cargo.toml","crates/ta-daemon/src/web.rs","crates/ta-daemon/Cargo.toml","crates/ta-daemon/assets/index.html","crates/ta-mcp-gateway/src/server.rs","docs/ADR-product-concept-model.md","docs/extension-points.md","docs/paid-addons/policy-studio.md","docs/paid-addons/enterprise-channels.md","docs/paid-addons/advanced-mediators.md","docs/paid-addons/compliance-reporting.md","PLAN.md","CLAUDE.md","docs/USAGE.md"],"title":"Implement v0.5.7 — Semantic Memory Queries & Memory Dashboard"}


## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** Build the v0.5 features
**Goal ID:** 7676fe31-1581-49ad-bc16-2f281916a344

## Plan Context

Plan progress:
- [x] Phase 0 — Repo Layout & Core Data Model
- [x] Phase 1 — Kernel: Audit, Policy, Changeset, Workspace
- [x] Phase 2 — MCP Gateway, Goal Lifecycle, CLI
- [x] Phase 3 — Transparent Overlay Mediation
- [x] Phase 4a — Agent Prompt Enhancement
- [x] Phase 4a.1 — Plan Tracking & Lifecycle
- [x] Phase 4b — Per-Artifact Review Model
- [x] Phase 4c — Selective Review CLI
- [ ] Phase v0.1 — Public Preview & Call for Feedback
- [ ] Phase v0.1.1 — Release Automation & Binary Distribution
- [x] Phase v0.1.2 — Follow-Up Goals & Iterative Review
- [x] Phase v0.2.0 — SubmitAdapter Trait & Git Implementation
- [x] Phase v0.2.1 — Concurrent Session Conflict Detection
- [x] Phase v0.2.2 — External Diff Routing
- [x] Phase v0.2.3 — Tiered Diff Explanations & Output Adapters
- [x] Phase v0.2.4 — Terminology & Positioning Pass
- [x] Phase v0.3.0 — Review Sessions
- [x] Phase v0.3.0.1 — Consolidate `pr.rs` into `draft.rs`
- [x] Phase v0.3.1 — Plan Lifecycle Automation
- [x] Phase v0.3.1.1 — Configurable Plan Format Parsing
- [x] Phase v0.3.1.2 — Interactive Session Orchestration
- [x] Phase v0.3.2 — Configurable Release Pipeline (`ta release`
- [x] Phase v0.3.3 — Decision Observability & Reasoning Capture
- [x] Phase v0.3.4 — Draft Amendment & Targeted Re-Work
- [x] Phase v0.3.5 — Release Pipeline Fixes
- [x] Phase v0.3.6 — Draft Lifecycle Hygiene
- [x] Phase v0.4.0 — Intent-to-Access Planner & Agent Alignment Profiles
- [x] Phase v0.4.1 — Macro Goals & Inner-Loop Iteration
- [x] Phase v0.4.1.1 — Runtime Channel Architecture & Macro Session Loop
- [x] Phase v0.4.1.2 — Follow-Up Draft Continuity
- [x] Phase v0.4.2 — Behavioral Drift Detection
- [x] Phase v0.4.3 — Access Constitutions
- [x] Phase v0.4.4 — Interactive Session Completion
- [x] Phase v0.4.5 — CLI UX Polish
- [x] Phase v0.5.0 — Credential Broker & Identity Abstraction
- [x] Phase v0.5.1 — MCP Tool Call Interception
- [x] Phase v0.5.2 — Minimal Web Review UI
- [x] Phase v0.5.3 — Additional ReviewChannel Adapters
- [x] Phase v0.5.4 — Context Memory Store (ruvector integration)
- [x] Phase v0.5.5 — RuVector Memory Backend
- [x] Phase v0.5.6 — Framework-Agnostic Agent State
- [x] Phase v0.5.7 — Semantic Memory Queries & Memory Dashboard
- [x] Phase v0.6.0 — Session & Human Control Plane
- [x] Phase v0.6.1 — Unified Policy Config
- [x] Phase v0.6.2 — Resource Mediation Trait
- [x] Phase v0.6.3 — Active Memory Injection & Project-Aware Key Schema
- [x] Phase v0.7.0 — Channel Registry
- [x] Phase v0.7.1 — API Mediator
- [x] Phase v0.7.2 — Agent-Guided Setup
- [x] Phase v0.7.3 — Project Template Repository & `ta init`
- [x] Phase v0.7.4 — Memory & Config Cleanup
- [x] Phase v0.7.5 — Interactive Session Fixes & Cross-Platform Release
- [x] Phase v0.7.6 — Interactive Developer Loop (`ta dev`)
- [ ] Phase v0.7.7 — Agent Framework Registry & Setup Integration
- [ ] Phase v0.8.0 — Event System & Subscription API
- [ ] Phase v0.8.1 — Solution Memory Export
- [ ] Phase v0.9.0 — Distribution & Packaging
- [ ] Phase v0.9.1 — Native Windows Support
- [ ] Phase v0.9.2 — Sandbox Runner (optional hardening
- [ ] Virtual Office Runtime (separate project)
- [ ] Autonomous Infra Ops (separate project)

## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** 7676fe31-1581-49ad-bc16-2f281916a344

## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

# Claude Code Project Instructions

## Build Environment

Nix provides the Rust toolchain. **Always prefix cargo/just commands with the nix wrapper:**

```bash
export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
nix develop --command bash -c "COMMAND_HERE"
```

Or use the helper script for one-liners:
```bash
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev just verify
```

## Verification Before Every Commit

Run these four checks (all must pass):
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

## Git Workflow — Feature Branches + Pull Requests

All work MUST happen on feature branches. Never commit directly to `main`.

1. **Create a feature branch** before starting work:
   ```bash
   git checkout -b feature/<short-description>
   ```
   Use prefixes: `feature/`, `fix/`, `refactor/`, `docs/` as appropriate.

2. **Commit to the feature branch** in logical working units as you go.

3. **When the goal is complete**, push and open a pull request:
   ```bash
   git push -u origin feature/<short-description>
   gh pr create --title "Short description" --body "## Summary\n- what changed and why\n\n## Test plan\n- verification steps"
   ```

4. **The PR is reviewed and merged** into `main` (squash or merge commit).

This applies to both manual work and TA-mediated goals. When `ta pr apply --git-commit` runs, the commit should land on a feature branch, not `main`.

## Rules

- Never commit directly to `main` — always use a feature branch + PR
- Never disable or skip tests
- Run tests after every code change, before committing
- Commit in logical working units
- All work stays within ~/development/TrustedAutonomy/
- Use `tempfile::tempdir()` for all test fixtures that need filesystem access

## Current State

- **Current version**: `0.7.0-alpha`
- See **PLAN.md** for the canonical development roadmap with per-phase status
- `ta plan list` / `ta plan status` show current progress
- Goals can link to plan phases: `ta run "title" --source . --phase 4b`
- `ta draft apply` auto-updates PLAN.md when a phase completes

## Version Management

When completing a phase, you MUST update versions as part of the work:

1. **`apps/ta-cli/Cargo.toml`**: Update `version` to the phase's target version (e.g., `"0.2.0-alpha"`)
2. **This file (`CLAUDE.md`)**: Update "Current version" above to match
3. **`PLAN.md`**: Mark the phase `<!-- status: done -->` (done automatically by `ta draft apply --phase`)
4. **`docs/USAGE.md`**: Update with any new commands, flags, config options, or workflow changes. USAGE.md is the user onboarding guide — write feature documentation as "how to" sections, not version-annotated changelogs. Keep version references out of feature descriptions (use the Roadmap section for version tracking). When adding a new workflow or command, add it to the appropriate section with a clear code example.

Version format: `MAJOR.MINOR.PATCH-alpha` (semver). See `PLAN.md` "Versioning & Release Policy" for the full mapping of phases to versions. Sub-phases use pre-release dot notation: `v0.4.1.2` → `0.4.1-alpha.2`.

### How It Works (Overlay Flow)
1. `ta goal start "title" --source . --phase 4b` → copies project to `.ta/staging/`
2. `ta run "title" --source . --phase 4b` → creates goal + injects CLAUDE.md (with plan context) + launches agent + builds draft on exit
3. Agent works normally in staging copy — TA is invisible to the agent
4. `ta draft build --latest` → diffs staging vs source → creates draft package with artifacts
5. `ta draft view/approve/deny <id>` → review workflow
6. `ta draft apply <id> --git-commit` → copies changes back to source + updates PLAN.md + optional git commit

### Key Types
- **Artifact.resource_uri**: `"fs://workspace/<path>"` — URI-based identity for all changes
- **PatchSet.target_uri**: Same URI scheme for external resources (gmail://, drive://, etc.)
- **DraftStatus**: Draft → PendingReview → Approved/Denied → Applied/Superseded/Closed
- **GoalRunState**: Created → Configured → Running → PrReady → UnderReview → Approved → Applied → Completed
- **GoalRun.plan_phase**: Optional link to a PLAN.md phase (e.g., "4b")
- **CLAUDE.md injection**: `ta run` prepends TA context + plan progress, saves backup, restores before diff


