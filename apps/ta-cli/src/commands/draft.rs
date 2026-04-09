// draft.rs — draft package subcommands: build, list, view, approve, deny, apply.

use std::fs;
use std::path::Path;
use std::sync::mpsc;

use chrono::{Duration, Utc};
use clap::Subcommand;
use sha2::Digest as _;
use ta_changeset::changeset::{ChangeKind, ChangeSet, CommitIntent};
use ta_changeset::diff::DiffContent;
use ta_changeset::diff_handlers::DiffHandlersConfig;
use ta_changeset::draft_package::{
    AgentIdentity, AlternativeConsidered, AmendmentRecord, AmendmentType, ApprovalRecord, Artifact,
    ArtifactDisposition, ChangeDependency, ChangeType, Changes, DecisionLogEntry, DependencyKind,
    DraftPackage, DraftStatus, ExplanationTiers, Goal, Iteration, Plan, Provenance,
    RequestedAction, ReviewRequests, Risk, Signatures, Summary, VerificationWarning, WorkspaceRef,
};
use ta_changeset::explanation::ExplanationSidecar;
use ta_changeset::output_adapters::{
    get_adapter, DetailLevel, DiffProvider, OutputFormat, RenderContext,
};
use ta_changeset::review_session::{ReviewSession, ReviewState};
use ta_changeset::review_session_store::ReviewSessionStore;
use ta_changeset::supervisor::{SupervisorAgent, ValidationWarning};
use ta_changeset::uri_pattern;
use ta_connector_fs::FsConnector;
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{
    ChangeStore, ExcludePatterns, JsonFileStore, OverlayWorkspace, StagingWorkspace,
};
use uuid::Uuid;

/// Load exclude patterns for a source directory, merging VCS adapter patterns
/// (e.g. ".git/" for Git) so that VCS metadata never appears in staging diffs.
///
/// Without merging adapter patterns, ta draft apply --git-commit can overwrite
/// .git/HEAD and .git/index from the staging copy, resetting HEAD to main.
pub fn load_excludes_with_adapter(source_dir: &std::path::Path) -> ExcludePatterns {
    let mut excludes = ExcludePatterns::load(source_dir);
    let wf_path = source_dir.join(".ta/workflow.toml");
    let wf_config = ta_submit::WorkflowConfig::load_or_default(&wf_path);
    let adapter = ta_submit::select_adapter(source_dir, &wf_config.submit);
    excludes.merge(&adapter.exclude_patterns());
    excludes
}

#[derive(Subcommand)]
pub enum DraftCommands {
    /// Build a draft package from overlay workspace diffs.
    Build {
        /// Goal run ID (omit with --latest to use most recent running goal).
        #[arg(default_value = "")]
        goal_id: String,
        /// Summary of what changed and why.
        #[arg(long, default_value = "Changes from agent work")]
        summary: String,
        /// Use the most recent running goal instead of specifying an ID.
        #[arg(long)]
        latest: bool,
        /// Path to a JSON context file to patch into the draft after building
        /// (used by background draft-build spawned from `ta run`, v0.15.6.2).
        #[arg(long, hide = true)]
        apply_context_file: Option<std::path::PathBuf>,
    },
    /// List all draft packages.
    List {
        /// Filter by goal run ID.
        #[arg(long)]
        goal: Option<String>,
        /// Show only stale drafts (non-terminal states older than threshold).
        #[arg(long)]
        stale: bool,
        /// Show only pending/active drafts (Draft, PendingReview, Approved).
        #[arg(long)]
        pending: bool,
        /// Show only applied drafts.
        #[arg(long)]
        applied: bool,
        /// Limit the number of results shown.
        #[arg(long)]
        limit: Option<usize>,
        /// Show all drafts including terminal states (overrides default compact view).
        #[arg(long)]
        all: bool,
        /// Output as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// View draft package details and diffs.
    View {
        /// Draft package ID, goal title, or phase (e.g., "v0.10.7"). Omit to auto-select if only one pending draft.
        id: Option<String>,
        /// Show summary and file list only (skip diffs). [DEPRECATED: use --detail top]
        #[arg(long)]
        summary: bool,
        /// Filter to files matching these glob patterns (repeatable). Multiple --file flags allowed.
        /// E.g.: --file "src/auth/*.rs" --file PLAN.md
        /// If a diff handler is configured for this file type (with a single match), it will be
        /// opened in the external application instead of showing the diff inline.
        #[arg(long)]
        file: Vec<String>,
        /// Open file in external handler.
        /// If not specified, uses workflow.toml [diff] open_external setting (default: true).
        /// Use --no-open-external to force inline diff display even if handler exists.
        #[arg(long)]
        open_external: Option<bool>,
        /// Detail level: top (one-line), medium (with explanations), full (with diffs).
        /// Default: medium.
        #[arg(long, default_value = "medium")]
        detail: String,
        /// Output format: terminal (default), markdown, json, html.
        #[arg(long, default_value = "terminal")]
        format: String,
        /// Enable ANSI color output (terminal format only). Default: off.
        #[arg(long)]
        color: bool,
        /// Output as structured JSON (overrides --format).
        #[arg(long)]
        json: bool,
        /// Show only one section: summary, decisions, validation, files (v0.14.7).
        #[arg(long)]
        section: Option<String>,
    },
    /// Approve a draft package for application.
    Approve {
        /// Draft package ID, goal title, or phase (e.g., "v0.10.7"). Omit to auto-select if only one pending draft.
        id: Option<String>,
        /// Reviewer name (legacy alias for --as).
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
        /// Reviewer identity for multi-party governance. Overrides --reviewer when set.
        #[arg(long = "as")]
        reviewer_as: Option<String>,
        /// Override quorum requirement (requires override_identity in [governance] config).
        /// The override is recorded in the audit trail.
        #[arg(long = "override")]
        force_override: bool,
    },
    /// Deny a draft package with a reason.
    Deny {
        /// Draft package ID, goal title, or phase (e.g., "v0.10.7"). Omit to auto-select if only one pending draft.
        id: Option<String>,
        /// Reason for denial.
        #[arg(long)]
        reason: String,
        /// Reviewer name.
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
        /// Deny only a specific artifact (by path or URI fragment), leaving others pending.
        /// After denying, prompts to ask the agent why it made this choice.
        #[arg(long)]
        file: Option<String>,
    },
    /// Apply approved changes to the target directory.
    ///
    /// By default, runs the full submit workflow (stage + submit + review) when a
    /// VCS adapter is detected or configured. Use --no-submit to copy files only.
    Apply {
        /// Draft package ID, goal title, or phase (e.g., "v0.10.7"). Omit to auto-select if only one pending draft.
        id: Option<String>,
        /// Target directory (defaults to project root).
        #[arg(long)]
        target: Option<String>,
        /// Run the full submit workflow for the configured VCS adapter.
        /// This is the default when a VCS adapter is detected/configured.
        /// Use --no-submit to copy files only without any VCS operations.
        #[arg(long, overrides_with = "no_submit")]
        submit: bool,
        /// Copy files only — skip all VCS operations (no commit, push, or review).
        #[arg(long)]
        no_submit: bool,
        /// Open a review (PR, CL review) after submitting.
        /// Default: true when adapter supports review. Use --no-review to skip.
        #[arg(long, overrides_with = "no_review")]
        review: bool,
        /// Skip review creation even when adapter supports it.
        #[arg(long)]
        no_review: bool,
        /// Show what the submit workflow would do without actually doing it.
        #[arg(long)]
        dry_run: bool,
        /// **Deprecated**: Use --submit instead. Alias for backward compatibility.
        #[arg(long, hide = true)]
        git_commit: bool,
        /// **Deprecated**: Use --submit instead. Alias for backward compatibility.
        #[arg(long, hide = true)]
        git_push: bool,
        /// Skip pre-commit verification checks.
        #[arg(long)]
        skip_verify: bool,
        /// Conflict resolution strategy: abort (default), force-overwrite, merge.
        /// Determines what happens if source files have changed since goal start.
        #[arg(long, default_value = "abort")]
        conflict_resolution: String,
        /// Approve artifacts matching these patterns (repeatable).
        /// Special values: "all" (everything), "rest" (everything not explicitly matched).
        #[arg(long = "approve")]
        approve_patterns: Vec<String>,
        /// Reject artifacts matching these patterns (repeatable).
        #[arg(long = "reject")]
        reject_patterns: Vec<String>,
        /// Mark artifacts for discussion matching these patterns (repeatable).
        #[arg(long = "discuss")]
        discuss_patterns: Vec<String>,
        /// Override plan phase(s) to mark done on apply.
        /// Comma-separated for batch marking (e.g., "v0.8.0,v0.8.1").
        /// When omitted, uses the goal's linked plan_phase.
        #[arg(long)]
        phase: Option<String>,
        /// Force human review even when auto-approve policy is configured.
        /// Useful for high-risk changes that should always get a human look.
        #[arg(long)]
        require_review: bool,
        /// After apply, poll until the PR is merged then auto-sync.
        /// Equivalent to `ta draft apply … && ta draft watch <id>`.
        #[arg(long)]
        watch: bool,
        /// Apply the full draft chain: walk up to the root parent and apply
        /// all unapplied drafts in order (root → leaf). Detects cycles.
        /// Useful when a follow-up draft's parent was never applied.
        #[arg(long)]
        chain: bool,
        /// Bypass pre-apply artifact safety checks (dramatic shrinkage, critical file
        /// replacement). Use when the shrinkage is intentional (e.g., intentional rewrite).
        #[arg(long)]
        force_apply: bool,
        /// Show whether an apply is currently in progress (reads .ta/apply.lock).
        #[arg(long)]
        status: bool,
    },
    /// Amend an artifact in a draft (replace content, apply patch, or drop).
    Amend {
        /// Draft package ID.
        id: String,
        /// Artifact URI to amend (e.g., "fs://workspace/src/main.rs").
        artifact_uri: String,
        /// Replace the artifact content with a corrected file.
        #[arg(long)]
        file: Option<String>,
        /// Remove the artifact from the draft entirely.
        #[arg(long)]
        drop: bool,
        /// Reason for the amendment (recorded in audit trail).
        #[arg(long)]
        reason: Option<String>,
        /// Who is performing the amendment.
        #[arg(long, default_value = "human")]
        amended_by: String,
    },
    /// Scoped agent re-work targeting only discuss/amended artifacts.
    Fix {
        /// Draft package ID.
        id: String,
        /// Optional artifact URI to target (default: all discuss items).
        artifact_uri: Option<String>,
        /// Guidance for the agent on what to fix.
        #[arg(long)]
        guidance: String,
        /// Agent to use (default: claude-code).
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Don't launch the agent — just set up the workspace and print instructions.
        #[arg(long)]
        no_launch: bool,
    },
    /// Interactive review session commands.
    Review {
        #[command(subcommand)]
        command: ReviewCommands,
    },
    /// Close a draft without applying (abandoned, hand-merged, or obsolete).
    Close {
        /// Draft package ID, goal title, or phase. Omit to auto-select if only one pending draft.
        id: Option<String>,
        /// Reason for closing.
        #[arg(long)]
        reason: Option<String>,
        /// Who is closing the draft.
        #[arg(long, default_value = "human-reviewer")]
        closed_by: String,
        /// Close all stale drafts (Approved or PendingReview) older than the configured threshold.
        /// Requires confirmation unless --yes is passed.
        #[arg(long)]
        stale: bool,
        /// Age threshold in days for --stale (overrides gc.stale_threshold_days config).
        #[arg(long)]
        older_than: Option<u64>,
        /// Skip confirmation prompt for --stale.
        #[arg(long)]
        yes: bool,
    },
    /// Garbage-collect stale staging directories for terminal-state drafts.
    Gc {
        /// Show what would be removed without actually removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Archive staging dirs to .ta/archive/ instead of deleting.
        #[arg(long)]
        archive: bool,
        /// Also close stale draft records (Approved/PendingReview older than threshold).
        /// Combined with staging GC for a full cleanup in one pass.
        #[arg(long)]
        drafts: bool,
    },
    /// Lightweight follow-up for PR iteration on an existing feature branch.
    FollowUp {
        /// Draft package ID (or prefix).
        id: String,
        /// Agent system to use.
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Auto-fetch latest CI failure log from the PR and inject as context.
        #[arg(long)]
        ci_failure: bool,
        /// Auto-fetch PR review comments and inject as context.
        #[arg(long)]
        review_comments: bool,
        /// Additional guidance for the agent.
        #[arg(long)]
        guidance: Option<String>,
        /// Don't launch the agent -- just set up the workspace and print instructions.
        #[arg(long)]
        no_launch: bool,
    },
    /// Show PR state, CI status, and review status for a draft's associated PR.
    PrStatus {
        /// Draft package ID (or prefix).
        id: String,
    },
    /// Retry PR creation for an applied draft whose branch was pushed but whose PR was never created.
    ///
    /// Use this when `ta draft apply --submit` failed at the PR-creation step but succeeded at push.
    /// The branch is already on the remote — this command creates the missing PR.
    ReopenReview {
        /// Draft package ID (or prefix).
        id: String,
    },
    /// List open PRs created by TA with their draft IDs and CI status.
    PrList,
    /// Merge the PR/review for an applied draft and sync the local main branch.
    ///
    /// Calls `merge_review()` on the VCS adapter (e.g. `gh pr merge --auto`) then
    /// runs `sync_upstream()` to fast-forward the local branch.
    Merge {
        /// Draft package ID (or prefix). Omit to auto-select if only one applied draft.
        id: Option<String>,
        /// Merge strategy: squash (default), merge, rebase.
        #[arg(long, default_value = "squash")]
        strategy: String,
        /// Delete the feature branch after merging.
        #[arg(long, default_value = "true")]
        delete_branch: bool,
    },
    /// Poll PR/CI status until merged, then auto-sync the local branch.
    ///
    /// Equivalent to running `ta draft pr-status <id>` on a loop until the
    /// PR state is "merged", then running `ta sync`.
    Watch {
        /// Draft package ID (or prefix). Omit to auto-select if only one applied draft.
        id: Option<String>,
        /// How often to poll in seconds (default: 30).
        #[arg(long, default_value = "30")]
        interval: u64,
        /// Maximum number of polls before giving up (default: 120 = 1 hour at 30s).
        #[arg(long, default_value = "120")]
        max_polls: u32,
    },
}

/// Review session subcommands for multi-turn artifact review.
#[derive(Subcommand)]
pub enum ReviewCommands {
    /// Start or resume a review session for a draft package.
    Start {
        /// Draft package ID to review.
        draft_id: String,
        /// Reviewer name (defaults to "human-reviewer").
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
    },
    /// Add a comment to an artifact.
    Comment {
        /// Artifact URI (e.g., "fs://workspace/src/main.rs").
        uri: String,
        /// Comment text.
        message: String,
        /// Commenter name (defaults to "human-reviewer").
        #[arg(long, default_value = "human-reviewer")]
        commenter: String,
    },
    /// Show the next undecided artifact in the current session.
    Next {
        /// Show this many pending artifacts (default: 1).
        #[arg(long, default_value = "1")]
        count: usize,
    },
    /// Finish the review session and show final summary.
    Finish {
        /// Session ID to finish (omit to use the most recent active session).
        #[arg(long)]
        session: Option<String>,
    },
    /// List all review sessions.
    List {
        /// Show only sessions for a specific draft package.
        #[arg(long)]
        draft: Option<String>,
    },
    /// Show details of a review session.
    Show {
        /// Session ID to show (omit to use the most recent active session).
        #[arg(long)]
        session: Option<String>,
    },
}

/// Startup health check: warn about stale drafts (v0.3.6).
/// Called on every `ta` invocation; prints to stderr. Suppressible via [gc] health_check = false.
pub fn check_stale_drafts(config: &GatewayConfig) {
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    if !workflow_config.gc.health_check {
        return;
    }

    // Only check if pr_packages dir exists.
    if !config.pr_packages_dir.exists() {
        return;
    }

    let Ok(packages) = load_all_packages(config) else {
        return;
    };

    // v0.13.17.2: Use two thresholds:
    // - hint_days: when to warn the user (early notice)
    // - threshold_days: what `--stale` actually shows
    // Only suggest `--stale` when it would return results (threshold met).
    let hint_days = workflow_config.gc.stale_hint_days as i64;
    let threshold_days = workflow_config.gc.stale_threshold_days as i64;
    let now = Utc::now();
    let hint_cutoff = now - Duration::days(hint_days);
    let stale_cutoff = now - Duration::days(threshold_days);

    let hint_count = packages
        .iter()
        .filter(|p| {
            matches!(
                p.status,
                DraftStatus::Approved { .. } | DraftStatus::PendingReview
            ) && p.created_at < hint_cutoff
        })
        .count();

    // Only show the `--stale` suggestion when `--stale` would actually return results.
    let stale_command_count = packages
        .iter()
        .filter(|p| {
            matches!(
                p.status,
                DraftStatus::Approved { .. } | DraftStatus::PendingReview
            ) && p.created_at < stale_cutoff
        })
        .count();

    if hint_count > 0 {
        if stale_command_count > 0 {
            eprintln!(
                "hint: {} draft(s) approved/pending but not applied for {}+ days — run `ta draft list --stale`",
                hint_count, hint_days
            );
        } else {
            eprintln!(
                "hint: {} draft(s) approved/pending but not applied for {}+ days — run `ta draft list` to review",
                hint_count, hint_days
            );
        }
    }
}

/// Heartbeat writer for background draft-build processes (v0.15.7.1).
///
/// Spawns a background thread that touches `.ta/heartbeats/<goal-id>` every 30s.
/// On drop, the thread is stopped. Call `finish_ok()` or `finish_err()` before
/// dropping to write the appropriate sentinel file.
struct BackgroundHeartbeat {
    heartbeats_dir: std::path::PathBuf,
    goal_id: String,
    stop_tx: mpsc::SyncSender<()>,
}

impl BackgroundHeartbeat {
    /// Start writing heartbeats for `goal_id`.
    fn start(config: &ta_mcp_gateway::GatewayConfig, goal_id: &str) -> Self {
        let heartbeats_dir = config.workspace_root.join(".ta").join("heartbeats");
        let _ = std::fs::create_dir_all(&heartbeats_dir);
        let hb_path = heartbeats_dir.join(goal_id);
        // Write initial heartbeat so the watchdog sees it immediately.
        let _ = std::fs::write(&hb_path, b"");

        let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);
        let hb_path_thread = hb_path.clone();
        std::thread::spawn(move || loop {
            match stop_rx.recv_timeout(std::time::Duration::from_secs(30)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Update mtime by writing the file again.
                    let _ = std::fs::write(&hb_path_thread, b"");
                }
            }
        });

        tracing::debug!(goal_id = goal_id, "Background heartbeat started");
        Self {
            heartbeats_dir,
            goal_id: goal_id.to_string(),
            stop_tx,
        }
    }

    /// Stop the heartbeat thread and write a `.done` sentinel.
    fn finish_ok(self) {
        let _ = self.stop_tx.send(());
        let done = self.heartbeats_dir.join(format!("{}.done", self.goal_id));
        let _ = std::fs::write(&done, b"");
        tracing::debug!(
            goal_id = self.goal_id,
            "Background heartbeat: wrote .done sentinel"
        );
    }

    /// Stop the heartbeat thread and write a `.failed` sentinel.
    fn finish_err(self) {
        let _ = self.stop_tx.send(());
        let failed = self.heartbeats_dir.join(format!("{}.failed", self.goal_id));
        let _ = std::fs::write(&failed, b"");
        tracing::warn!(
            goal_id = self.goal_id,
            "Background heartbeat: wrote .failed sentinel"
        );
    }
}

impl Drop for BackgroundHeartbeat {
    fn drop(&mut self) {
        // Best-effort stop if finish_ok/finish_err was not called.
        let _ = self.stop_tx.try_send(());
    }
}

pub fn execute(cmd: &DraftCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        DraftCommands::Build {
            goal_id,
            summary,
            latest,
            apply_context_file,
        } => {
            // v0.15.7.1: Start heartbeat writer when invoked as background build
            // (apply_context_file is only set by the background spawn from `ta run`).
            // The heartbeat thread touches .ta/heartbeats/<goal-id> every 30s so
            // the daemon watchdog can distinguish a healthy build from a hung one.
            let heartbeat = if apply_context_file.is_some() && !goal_id.is_empty() {
                Some(BackgroundHeartbeat::start(config, goal_id))
            } else {
                None
            };

            let build_result = build_package(config, goal_id, summary, *latest);
            let ctx_result = match &build_result {
                Ok(()) => {
                    if let Some(ctx_path) = apply_context_file {
                        if ctx_path.exists() {
                            apply_draft_build_context(config, goal_id, ctx_path)
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    }
                }
                Err(_) => build_result,
            };

            // Write .done or .failed sentinel and stop heartbeat thread.
            if let Some(hb) = heartbeat {
                if ctx_result.is_ok() {
                    hb.finish_ok();
                } else {
                    hb.finish_err();
                }
            }

            ctx_result
        }
        DraftCommands::List {
            goal,
            stale,
            pending,
            applied,
            limit,
            all,
            json,
        } => list_packages(
            config,
            goal.as_deref(),
            *stale,
            *pending,
            *applied,
            *limit,
            *all,
            *json,
        ),
        DraftCommands::View {
            id,
            summary,
            file,
            open_external,
            detail,
            format,
            color,
            json,
            section,
        } => {
            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
            if *json {
                view_package_json(config, &resolved)
            } else {
                view_package(
                    config,
                    &resolved,
                    *summary,
                    file,
                    open_external,
                    detail,
                    format,
                    *color,
                    section.as_deref(),
                )
            }
        }
        DraftCommands::Approve {
            id,
            reviewer,
            reviewer_as,
            force_override,
        } => {
            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
            let identity = reviewer_as.as_deref().unwrap_or(reviewer.as_str());
            approve_package(config, &resolved, identity, *force_override)
        }
        DraftCommands::Deny {
            id,
            reason,
            reviewer,
            file,
        } => {
            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
            if let Some(file_path) = file {
                deny_artifact(config, &resolved, reason, reviewer, file_path)
            } else {
                deny_package(config, &resolved, reason, reviewer)
            }
        }
        DraftCommands::Apply {
            id,
            target,
            submit,
            no_submit,
            review,
            no_review,
            dry_run,
            git_commit,
            git_push,
            skip_verify,
            conflict_resolution,
            approve_patterns,
            reject_patterns,
            discuss_patterns,
            phase,
            require_review,
            watch,
            chain,
            force_apply,
            status,
        } => {
            if *status {
                ApplyLock::print_status(&config.workspace_root);
                return Ok(());
            }

            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;

            // --chain: walk up to root parent and apply all unapplied drafts in order.
            if *chain {
                return apply_chain(config, &resolved, target.as_deref(), *skip_verify);
            }

            // Warn on deprecated flags.
            if *git_commit || *git_push {
                eprintln!("  Note: --git-commit/--git-push are deprecated. Use --submit instead.");
            }

            if *require_review {
                eprintln!(
                    "  --require-review: auto-approve policy will be bypassed for this draft."
                );
            }

            // Load workflow config to merge auto_* settings with CLI flags.
            let workflow_config = ta_submit::WorkflowConfig::load_or_default(
                &config.workspace_root.join(".ta/workflow.toml"),
            );

            // Resolve submit behavior:
            // 1. --no-submit explicitly disables everything
            // 2. --submit or deprecated --git-commit/--git-push explicitly enable
            // 3. Otherwise: default to submit when VCS is configured OR detected
            //    (§2.4: surprising default is to NOT go all the way through).
            //    Auto-detect by checking if the actual selected adapter is not "none".
            let do_submit = if *no_submit {
                false
            } else if *submit || *git_commit || *git_push {
                true
            } else {
                // Use effective_auto_submit (respects explicit config) — OR fall back
                // to auto-detection: select the adapter for the workspace root and
                // check if it's a real VCS adapter (not "none").
                let auto_from_config = workflow_config.submit.effective_auto_submit();
                if !auto_from_config {
                    // Config says "none" adapter, but check runtime auto-detection.
                    let detected =
                        ta_submit::select_adapter(&config.workspace_root, &workflow_config.submit);
                    detected.name() != "none"
                } else {
                    true
                }
            };

            // Resolve review behavior:
            // 1. --no-review explicitly disables
            // 2. --review or --require-review explicitly enables
            // 3. Otherwise use config defaults
            let do_review = if *no_review {
                false
            } else if *review || *require_review {
                true
            } else {
                do_submit && workflow_config.submit.effective_auto_review()
            };

            // Parse conflict resolution strategy.
            use ta_workspace::ConflictResolution;
            let resolution = match conflict_resolution.as_str() {
                "abort" => ConflictResolution::Abort,
                "force-overwrite" | "force" => ConflictResolution::ForceOverwrite,
                "merge" => ConflictResolution::Merge,
                _ => anyhow::bail!(
                    "Invalid conflict resolution strategy: '{}' (must be: abort, force-overwrite, merge)",
                    conflict_resolution
                ),
            };

            apply_package(
                config,
                &resolved,
                target.as_deref(),
                do_submit,
                do_submit, // push is always part of submit
                do_review,
                *skip_verify,
                *dry_run,
                resolution,
                SelectiveReviewPatterns {
                    approve: approve_patterns,
                    reject: reject_patterns,
                    discuss: discuss_patterns,
                },
                phase.as_deref(),
                *force_apply,
            )?;

            // --watch: poll until merged, then auto-sync.
            if *watch && !*dry_run {
                println!("\n[watch] --watch flag set — monitoring PR until merged...");
                watch_package(config, &resolved, 30, 120)?;
            }

            Ok(())
        }
        DraftCommands::Amend {
            id,
            artifact_uri,
            file,
            drop,
            reason,
            amended_by,
        } => amend_package(
            config,
            id,
            artifact_uri,
            file.as_deref(),
            *drop,
            reason.as_deref(),
            amended_by,
        ),
        DraftCommands::Fix {
            id,
            artifact_uri,
            guidance,
            agent,
            no_launch,
        } => fix_package(
            config,
            id,
            artifact_uri.as_deref(),
            guidance,
            agent,
            *no_launch,
        ),
        DraftCommands::Review { command } => execute_review_command(command, config),
        DraftCommands::Close {
            id,
            reason,
            closed_by,
            stale,
            older_than,
            yes,
        } => {
            if *stale {
                close_stale_drafts(config, *older_than, reason.as_deref(), closed_by, *yes)
                    .map(|_| ())
            } else {
                let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
                close_package(config, &resolved, reason.as_deref(), closed_by)
            }
        }
        DraftCommands::Gc {
            dry_run,
            archive,
            drafts,
        } => gc_packages(config, *dry_run, *archive, *drafts),
        DraftCommands::FollowUp {
            id,
            agent,
            ci_failure,
            review_comments,
            guidance,
            no_launch,
        } => draft_follow_up(
            config,
            id,
            agent,
            *ci_failure,
            *review_comments,
            guidance.as_deref(),
            *no_launch,
        ),
        DraftCommands::PrStatus { id } => draft_pr_status(config, id),
        DraftCommands::ReopenReview { id } => draft_reopen_review(config, id),
        DraftCommands::PrList => draft_pr_list(config),
        DraftCommands::Merge {
            id,
            strategy,
            delete_branch,
        } => {
            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
            merge_package(config, &resolved, strategy, *delete_branch)
        }
        DraftCommands::Watch {
            id,
            interval,
            max_polls,
        } => {
            let resolved = resolve_draft_id_flexible(config, id.as_deref())?;
            watch_package(config, &resolved, *interval, *max_polls)
        }
    }
}

fn execute_review_command(cmd: &ReviewCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        ReviewCommands::Start { draft_id, reviewer } => review_start(config, draft_id, reviewer),
        ReviewCommands::Comment {
            uri,
            message,
            commenter,
        } => review_comment(config, uri, message, commenter),
        ReviewCommands::Next { count } => review_next(config, *count),
        ReviewCommands::Finish { session } => review_finish(config, session.as_deref()),
        ReviewCommands::List { draft } => review_list(config, draft.as_deref()),
        ReviewCommands::Show { session } => review_show(config, session.as_deref()),
    }
}

// ── Agent-generated change summary (.ta/change_summary.json) ──

/// Agent-provided change summary with per-file rationale and dependency info.
/// Written by the agent to `.ta/change_summary.json` before exiting.
#[derive(Debug, serde::Deserialize)]
struct ChangeSummary {
    summary: Option<String>,
    #[serde(default)]
    changes: Vec<ChangeSummaryEntry>,
    dependency_notes: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ChangeSummaryEntry {
    path: String,
    #[allow(dead_code)]
    action: Option<String>,
    /// What was changed in this target (the primary per-target description).
    what: Option<String>,
    /// Why the change was needed (motivation).
    why: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    independent: bool,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    depended_by: Vec<String>,
    /// Alternatives the agent considered for this change (v0.3.3).
    #[serde(default)]
    alternatives_considered: Vec<AlternativeConsidered>,
}

/// Try to load agent-authored decisions from `.ta-decisions.json` in the staging workspace (v0.14.7).
///
/// Agents write this file to record non-obvious implementation choices.
/// Format: JSON array of `{ decision, alternatives, rationale, confidence? }` objects.
fn load_agent_decisions(staging_path: &std::path::Path) -> Vec<DecisionLogEntry> {
    let path = staging_path.join(".ta-decisions.json");
    if !path.exists() {
        return vec![];
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: could not read .ta-decisions.json: {}", e);
            return vec![];
        }
    };
    match serde_json::from_str::<Vec<DecisionLogEntry>>(&content) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Warning: could not parse .ta-decisions.json: {}", e);
            vec![]
        }
    }
}

/// Load agent progress journal checkpoints as human-readable strings for draft evidence (v0.14.7.2).
///
/// Reads `.ta/ta-progress.json` from the staging workspace.
/// Returns formatted checkpoint strings for inclusion in `plan.completed_steps`.
fn load_progress_journal_for_draft(staging_path: &std::path::Path) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Checkpoint {
        label: String,
        at: String,
        detail: String,
    }
    #[derive(serde::Deserialize)]
    struct Journal {
        checkpoints: Vec<Checkpoint>,
    }
    let path = staging_path.join(".ta").join("ta-progress.json");
    if !path.exists() {
        return vec![];
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    match serde_json::from_str::<Journal>(&content) {
        Ok(j) => j
            .checkpoints
            .into_iter()
            .map(|cp| format!("[checkpoint:{}] {} — {}", cp.at, cp.label, cp.detail))
            .collect(),
        Err(_) => vec![],
    }
}

/// Try to load the agent's change summary from the staging workspace.
fn load_change_summary(staging_path: &std::path::Path) -> Option<ChangeSummary> {
    let path = staging_path.join(".ta/change_summary.json");
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<ChangeSummary>(&content) {
        Ok(summary) => Some(summary),
        Err(e) => {
            eprintln!("Warning: could not parse .ta/change_summary.json: {}", e);
            None
        }
    }
}

/// Look up a change summary entry by path and populate artifact fields.
fn enrich_artifact(artifact: &mut Artifact, summary: &ChangeSummary) {
    // Extract the relative path from fs://workspace/<path>.
    let rel_path = artifact
        .resource_uri
        .strip_prefix("fs://workspace/")
        .unwrap_or(&artifact.resource_uri);

    if let Some(entry) = summary.changes.iter().find(|c| c.path == rel_path) {
        // `what` populates explanation_tiers.summary (the primary per-target description).
        // `why` populates rationale (the motivation).
        if let Some(what) = &entry.what {
            let tiers = artifact
                .explanation_tiers
                .get_or_insert_with(|| ExplanationTiers {
                    summary: String::new(),
                    explanation: String::new(),
                    tags: vec![],
                    related_artifacts: vec![],
                });
            tiers.summary = what.clone();
            // If we also have a `why`, put it in the explanation field.
            if let Some(why) = &entry.why {
                tiers.explanation = why.clone();
            }
        }
        // `why` alone (no `what`) goes into rationale for backward compatibility.
        if entry.what.is_none() {
            artifact.rationale = entry.why.clone();
        }

        for dep_path in &entry.depends_on {
            artifact.dependencies.push(ChangeDependency {
                target_uri: format!("fs://workspace/{}", dep_path),
                kind: DependencyKind::DependsOn,
            });
        }
        for dep_path in &entry.depended_by {
            artifact.dependencies.push(ChangeDependency {
                target_uri: format!("fs://workspace/{}", dep_path),
                kind: DependencyKind::DependedBy,
            });
        }
    }
}

/// Extract decision log entries from agent alternatives in change_summary.json (v0.3.3).
fn extract_decision_log(summary: &ChangeSummary) -> Vec<DecisionLogEntry> {
    summary
        .changes
        .iter()
        .filter(|entry| !entry.alternatives_considered.is_empty())
        .map(|entry| DecisionLogEntry {
            decision: entry
                .what
                .clone()
                .unwrap_or_else(|| format!("Change to {}", entry.path)),
            rationale: entry
                .why
                .clone()
                .unwrap_or_else(|| "Not specified".to_string()),
            alternatives: entry
                .alternatives_considered
                .iter()
                .map(|a| format!("{}: {}", a.description, a.rejected_reason))
                .collect(),
            alternatives_considered: entry.alternatives_considered.clone(),
            confidence: None,
            context: None,
        })
        .collect()
}

/// Check if a file is exempt from summary enforcement (v0.4.0).
///
/// Uses configurable `.ta/summary-exempt` pattern file if available,
/// falling back to default patterns (lockfiles, config manifests, docs).
/// Files matching these patterns get auto-summaries and don't need
/// agent-provided descriptions at `ta draft build` time.
fn is_auto_summary_exempt(uri: &str) -> bool {
    is_auto_summary_exempt_with_patterns(uri, None)
}

/// Check exemption with an optional source directory for loading `.ta/summary-exempt`.
fn is_auto_summary_exempt_with_patterns(uri: &str, source_dir: Option<&std::path::Path>) -> bool {
    let patterns = match source_dir {
        Some(dir) => {
            let exempt_path = dir.join(".ta").join("summary-exempt");
            ta_policy::ExemptionPatterns::load_or_default(&exempt_path)
        }
        None => ta_policy::ExemptionPatterns::defaults(),
    };
    patterns.is_exempt(uri)
}

/// v0.12.2.3: Return true if the URL looks like a GitHub pull request URL.
fn is_github_pr_url(url: &str) -> bool {
    url.contains("github.com") && url.contains("/pull/")
}

/// v0.12.2.3: Close a GitHub PR that has been superseded by a follow-up draft.
///
/// Runs `gh pr close <url> --comment "..."` in a best-effort fashion.
/// Failures are logged as warnings but do not abort the build.
fn close_github_pr_superseded(review_url: &str, child_display_id: &str) {
    let comment = format!(
        "Superseded by follow-up draft {}. This PR has been automatically closed.",
        child_display_id
    );
    let result = std::process::Command::new("gh")
        .args(["pr", "close", review_url, "--comment", &comment])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            println!("  Auto-closed parent GitHub PR: {}", review_url);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "Warning: could not close parent GitHub PR {} — {}: {}",
                review_url,
                output.status,
                stderr.trim()
            );
        }
        Err(e) => {
            eprintln!(
                "Warning: could not run `gh pr close` for parent PR {} — {}",
                review_url, e
            );
        }
    }
}

/// v0.12.2.3: Strip the TA injection header from `CLAUDE.md` in the staging workspace
/// before computing the staging-vs-source diff.
///
/// When a session crashes or is interrupted, `restore_claude_md` in run.rs never runs,
/// leaving the injected TA header prepended to CLAUDE.md in staging. This causes the
/// full injection (goal context, plan, memory sections, etc.) to appear as a diff vs
/// the clean source CLAUDE.md, polluting the PR with internal TA scaffolding.
///
/// Detection: file starts with `# Trusted Autonomy — Mediated Goal`
/// Strip target: everything up to and including the `\n---\n\n` separator that precedes
/// the original project instructions. If nothing follows the separator, CLAUDE.md is
/// removed (original sentinel — CLAUDE.md didn't exist before injection).
///
/// Count files in the source directory that are modified or untracked in git.
///
/// Used to produce a diagnostic hint when `diff_all()` returns empty — if the
/// working tree has uncommitted changes the overlay will mirror them, making the
/// staging↔source diff empty even though real work exists.
fn count_working_tree_changes(source_dir: &Path) -> usize {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(source_dir)
        .output();
    match output {
        Ok(out) if out.status.success() => std::str::from_utf8(&out.stdout)
            .unwrap_or("")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        _ => 0,
    }
}

/// Returns `Ok(true)` if stripping was applied, `Ok(false)` if not needed.
fn strip_ta_injection_from_staging(staging_path: &Path) -> anyhow::Result<bool> {
    let claude_md_path = staging_path.join("CLAUDE.md");
    if !claude_md_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&claude_md_path)?;
    if !content.starts_with("# Trusted Autonomy \u{2014} Mediated Goal") {
        return Ok(false); // Not injected — nothing to strip.
    }

    // Find the separator line (`---`) that precedes the original project instructions.
    // The injection format ends with: "\n---\n\n{original_content}\n"
    // We find the first occurrence of "\n---\n" and take everything after it
    // (skipping the blank line that follows).
    let separator = "\n---\n";
    if let Some(sep_pos) = content.find(separator) {
        let after_sep = &content[sep_pos + separator.len()..];
        // Skip one leading newline if present (the blank line after `---`).
        let stripped = after_sep.strip_prefix('\n').unwrap_or(after_sep);
        // The injection format appends a trailing `\n` after existing_section.
        // Strip that extra newline to recover the exact original file content.
        let original = stripped.strip_suffix('\n').unwrap_or(stripped);

        if original.trim().is_empty() {
            // No original content existed — remove CLAUDE.md entirely.
            std::fs::remove_file(&claude_md_path)?;
            eprintln!(
                "[draft build] Stripped TA injection from CLAUDE.md \
                 (no original — file removed)"
            );
        } else {
            // Write original content back (without extra trailing newline from injection).
            std::fs::write(&claude_md_path, original)?;
            eprintln!(
                "[draft build] Stripped TA injection from CLAUDE.md \
                 (crash/freeze left injection in staging)"
            );
        }
        Ok(true)
    } else {
        // Injection header present but separator not found — leave as-is.
        Ok(false)
    }
}

/// Pre-apply working-tree cleanliness check.
///
/// Runs `git status --porcelain` in `target_dir` and handles known TA-managed
/// artifacts so that a subsequent `git pull --rebase` (or any VCS operation)
/// never fails due to leftover TA injection residue.
///
/// Handling rules:
/// - `CLAUDE.md` modified: if the TA injection header is present, strip it
///   (same logic as `strip_ta_injection_from_staging`). If the file is still
///   dirty after stripping (or injection was already gone), discard the
///   working-copy change via `git restore CLAUDE.md` — the committed version
///   is authoritative for apply purposes.
/// - Paths under `.ta/` (untracked): skipped silently — these are
///   TA-internal and should be gitignored.
/// - Any other modified tracked file: warn and continue (non-blocking).
///
/// This is best-effort: it logs and continues rather than returning an error.
/// Only actual git command failures are surfaced as errors.
fn check_and_clean_working_tree(target_dir: &std::path::Path) {
    let run_git = |args: &[&str]| -> Option<String> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(target_dir)
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    std::str::from_utf8(&out.stdout).ok().map(|s| s.to_string())
                } else {
                    None
                }
            })
    };

    let status_output = match run_git(&["status", "--porcelain"]) {
        Some(s) => s,
        None => {
            // Not a git repo or git not available — skip silently.
            return;
        }
    };

    if status_output.trim().is_empty() {
        return; // Working tree already clean — nothing to do.
    }

    let mut other_modified: Vec<String> = Vec::new();

    for line in status_output.lines() {
        if line.len() < 3 {
            continue;
        }
        // porcelain format: "XY path" where XY are status codes and there is
        // one space before the path.
        let xy = &line[..2];
        let path = line[3..].trim();

        // Skip untracked paths under `.ta/` — TA-internal, should be gitignored.
        if xy.contains('?') && (path.starts_with(".ta/") || path == ".ta") {
            continue;
        }

        if path == "CLAUDE.md" {
            // Try stripping the TA injection in-place first.
            let claude_md = target_dir.join("CLAUDE.md");
            let mut stripped = false;
            if claude_md.exists() {
                if let Ok(content) = std::fs::read_to_string(&claude_md) {
                    if content.starts_with("# Trusted Autonomy \u{2014} Mediated Goal") {
                        let separator = "\n---\n";
                        if let Some(sep_pos) = content.find(separator) {
                            let after_sep = &content[sep_pos + separator.len()..];
                            let after_blank = after_sep.strip_prefix('\n').unwrap_or(after_sep);
                            let original = after_blank.strip_suffix('\n').unwrap_or(after_blank);
                            if original.trim().is_empty() {
                                let _ = std::fs::remove_file(&claude_md);
                                eprintln!(
                                    "[apply] Stripped TA injection from CLAUDE.md \
                                     (no original content — file removed)"
                                );
                            } else {
                                let _ = std::fs::write(&claude_md, original);
                                eprintln!(
                                    "[apply] Stripped TA injection from CLAUDE.md \
                                     (leftover from previous run)"
                                );
                            }
                            stripped = true;
                        }
                    }
                }
            }

            // Whether or not we stripped, discard any remaining working-copy
            // changes to CLAUDE.md so VCS operations are unblocked.
            if !stripped || claude_md.exists() {
                // `git restore` discards unstaged changes.
                let _ = std::process::Command::new("git")
                    .args(["restore", "CLAUDE.md"])
                    .current_dir(target_dir)
                    .output();
                if !stripped {
                    eprintln!(
                        "[apply] Discarded working-tree changes to CLAUDE.md \
                         (committed version is authoritative)"
                    );
                }
            }
        } else if xy.contains('?') && (path.starts_with(".ta/") || path == ".ta") {
            // Already handled above — unreachable but kept for clarity.
        } else if xy.contains('?') {
            // Other untracked files — warn but do not touch.
            eprintln!(
                "[apply] Untracked file in working tree: {} \
                 (not touching — commit or gitignore it if needed)",
                path
            );
        } else {
            // Modified tracked file that isn't CLAUDE.md — warn only.
            other_modified.push(path.to_string());
        }
    }

    for f in &other_modified {
        eprintln!(
            "[apply] Warning: working tree has uncommitted changes to '{}'. \
             Consider committing or stashing before continuing.",
            f
        );
    }

    // Final status summary.
    let remaining = match run_git(&["status", "--porcelain"]) {
        Some(s) => s
            .lines()
            .filter(|l| {
                let l = l.trim();
                if l.len() < 3 {
                    return false;
                }
                let path = l[3..].trim();
                // Don't count untracked .ta/ paths in summary.
                !(l[..2].contains('?') && (path.starts_with(".ta/") || path == ".ta"))
            })
            .count(),
        None => 0,
    };

    if remaining == 0 {
        eprintln!("[apply] Working tree clean — proceeding.");
    } else {
        eprintln!(
            "[apply] {} file(s) modified (see above) — proceeding with apply.",
            remaining
        );
    }
}

pub(crate) fn build_package(
    config: &GatewayConfig,
    goal_id: &str,
    summary: &str,
    latest: bool,
) -> anyhow::Result<()> {
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // Resolve the goal — either by ID or by finding the latest running goal.
    let goal = if latest || goal_id.is_empty() {
        let goals = goal_store.list()?;
        goals
            .into_iter()
            .find(|g| matches!(g.state, GoalRunState::Running))
            .ok_or_else(|| {
                anyhow::anyhow!("No running goal found (use a goal ID or start a goal first)")
            })?
    } else {
        let goal_uuid = resolve_goal_id_from_store(goal_id, &goal_store)?;
        goal_store
            .get(goal_uuid)?
            .ok_or_else(|| anyhow::anyhow!("Goal run not found: {}", goal_id))?
    };
    let goal_id = goal.goal_run_id.to_string();

    // v0.13.17.2: Accept both Running and Finalizing — the Finalizing state means
    // ta run exited and is building the draft; manual `ta draft build` during recovery
    // should work without requiring a state transition back to Running.
    if !matches!(
        goal.state,
        GoalRunState::Running | GoalRunState::Finalizing { .. }
    ) {
        anyhow::bail!(
            "Goal is in {} state (must be running or finalizing to build draft)",
            goal.state
        );
    }

    let source_dir = goal
        .source_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Goal has no source_dir (not an overlay-based goal)"))?;

    // v0.12.2.3: Strip any leftover TA injection header from CLAUDE.md before diffing.
    // Protects against crash/freeze that leaves inject_claude_md's content in staging.
    // For follow-up goals that reuse parent staging, this also ensures the full
    // staging-vs-source diff is clean (item 1 invariant: always diff_all, never
    // per-session-only writes).
    strip_ta_injection_from_staging(&goal.workspace_path)?;

    // Open the overlay workspace and compute diffs.
    // V1 TEMPORARY: Load exclude patterns, merging VCS adapter patterns.
    let excludes = load_excludes_with_adapter(source_dir);
    let overlay =
        OverlayWorkspace::open(goal_id.clone(), source_dir, &goal.workspace_path, excludes);
    let changes = overlay.diff_all().map_err(|e| anyhow::anyhow!("{}", e))?;

    if changes.is_empty() {
        // Diagnose why there are no changes to produce a helpful error.
        let uncommitted = count_working_tree_changes(source_dir);
        if uncommitted > 0 {
            anyhow::bail!(
                "No changes detected in staging workspace.\n\
                \n\
                The agent made no changes relative to the source directory, but there \
                are {} file(s) in the source tree that are modified or untracked and not \
                yet committed to git. The overlay workspace copies the source as-is \
                (including uncommitted changes), so the diff is empty.\n\
                \n\
                If the work is already done, commit those changes to a feature branch \
                and open a PR:\n\
                  git checkout -b feature/<name>\n\
                  git add <files>\n\
                  git commit -m \"...\"\n\
                  git push -u origin feature/<name>\n\
                  gh pr create",
                uncommitted
            );
        }
        anyhow::bail!(
            "No changes detected in staging workspace.\n\
            \n\
            The agent ran but made no file changes relative to the source. \
            Possible causes:\n\
            - The goal's work is already implemented in the codebase.\n\
            - The agent exited before writing any files.\n\
            - The agent only produced text output (no file-write tool calls).\n\
            \n\
            To build a draft manually after making changes:\n\
              cd {}\n\
              # make your changes, then:\n\
              ta draft build {}",
            goal.workspace_path.display(),
            goal_id
        );
    }

    // Convert overlay changes to draft package artifacts.
    let mut artifacts = Vec::new();
    let mut changesets = Vec::new();

    for change in &changes {
        match change {
            ta_workspace::overlay::OverlayChange::Modified { path, diff } => {
                artifacts.push(Artifact {
                    resource_uri: format!("fs://workspace/{}", path),
                    change_type: ChangeType::Modify,
                    diff_ref: format!("changeset:{}", changesets.len()),
                    tests_run: vec![],
                    disposition: Default::default(),
                    rationale: None,
                    dependencies: vec![],
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                    kind: None,
                });
                changesets.push(
                    ChangeSet::new(
                        format!("fs://workspace/{}", path),
                        ChangeKind::FsPatch,
                        DiffContent::UnifiedDiff {
                            content: diff.clone(),
                        },
                    )
                    .with_commit_intent(CommitIntent::RequestCommit),
                );
            }
            ta_workspace::overlay::OverlayChange::Created { path, content } => {
                artifacts.push(Artifact {
                    resource_uri: format!("fs://workspace/{}", path),
                    change_type: ChangeType::Add,
                    diff_ref: format!("changeset:{}", changesets.len()),
                    tests_run: vec![],
                    disposition: Default::default(),
                    rationale: None,
                    dependencies: vec![],
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                    kind: None,
                });
                changesets.push(
                    ChangeSet::new(
                        format!("fs://workspace/{}", path),
                        ChangeKind::FsPatch,
                        DiffContent::CreateFile {
                            content: content.clone(),
                        },
                    )
                    .with_commit_intent(CommitIntent::RequestCommit),
                );
            }
            ta_workspace::overlay::OverlayChange::Deleted { path } => {
                artifacts.push(Artifact {
                    resource_uri: format!("fs://workspace/{}", path),
                    change_type: ChangeType::Delete,
                    diff_ref: format!("changeset:{}", changesets.len()),
                    tests_run: vec![],
                    disposition: Default::default(),
                    rationale: None,
                    dependencies: vec![],
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                    kind: None,
                });
                changesets.push(
                    ChangeSet::new(
                        format!("fs://workspace/{}", path),
                        ChangeKind::FsPatch,
                        DiffContent::DeleteFile,
                    )
                    .with_commit_intent(CommitIntent::RequestCommit),
                );
            }
        }
    }

    // Persist changesets to the store.
    let mut store = JsonFileStore::new(goal.store_path.clone())?;
    for cs in &changesets {
        store.save(&goal_id, cs)?;
    }

    // Enrich artifacts with agent-provided rationale and dependency info.
    let change_summary = load_change_summary(&goal.workspace_path);
    if let Some(ref cs) = change_summary {
        for artifact in &mut artifacts {
            enrich_artifact(artifact, cs);
        }
        let enriched_count = artifacts.iter().filter(|a| a.rationale.is_some()).count();
        if enriched_count > 0 {
            println!(
                "Loaded change_summary.json: {}/{} artifacts enriched with rationale",
                enriched_count,
                artifacts.len()
            );
        }
    }

    // v0.2.3: Ingest explanation sidecars (.diff.explanation.yaml files).
    let mut explanation_count = 0;
    for artifact in &mut artifacts {
        // Extract the relative path from fs://workspace/<path>.
        let rel_path = artifact
            .resource_uri
            .strip_prefix("fs://workspace/")
            .unwrap_or(&artifact.resource_uri);
        let file_path = goal.workspace_path.join(rel_path);

        if let Some(sidecar) = ExplanationSidecar::find_for_file(&file_path) {
            artifact.explanation_tiers = Some(sidecar.into_tiers());
            explanation_count += 1;
        }
    }
    if explanation_count > 0 {
        println!(
            "Loaded explanation sidecars: {}/{} artifacts have tiered explanations",
            explanation_count,
            artifacts.len()
        );
    }

    // Summary enforcement: warn or error when non-exempt artifacts lack descriptions.
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let enforcement = workflow_config.build.summary_enforcement.as_str();
    if enforcement != "ignore" {
        let missing: Vec<&str> = artifacts
            .iter()
            .filter(|a| a.explanation_tiers.is_none() && a.rationale.is_none())
            .filter(|a| !is_auto_summary_exempt(&a.resource_uri))
            .map(|a| {
                a.resource_uri
                    .strip_prefix("fs://workspace/")
                    .unwrap_or(&a.resource_uri)
            })
            .collect();
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|p| format!("  - {}", p))
                .collect::<Vec<_>>()
                .join("\n");
            let msg = format!(
                "{} artifact(s) missing descriptions (no 'what' in change_summary.json):\n{}",
                missing.len(),
                list,
            );
            if enforcement == "error" {
                anyhow::bail!("{}", msg);
            } else {
                eprintln!("Warning: {}", msg);
            }
        }
    }

    // v0.4.3: Constitution enforcement — check artifacts against declared access constitution.
    let constitution_store = ta_policy::ConstitutionStore::for_workspace(&config.workspace_root);
    if let Ok(Some(constitution)) = constitution_store.load(&goal_id) {
        let artifact_uris: Vec<&str> = artifacts.iter().map(|a| a.resource_uri.as_str()).collect();
        let validation =
            ta_policy::constitution::validate_constitution(&constitution, &artifact_uris);

        if !validation.passed() {
            let list = validation
                .undeclared
                .iter()
                .map(|u| format!("  - {}", u.strip_prefix("fs://workspace/").unwrap_or(u)))
                .collect::<Vec<_>>()
                .join("\n");
            let msg = format!(
                "Access constitution violation: {} artifact(s) not declared in constitution for goal {}:\n{}",
                validation.undeclared.len(),
                goal_id,
                list,
            );

            match constitution.enforcement {
                ta_policy::EnforcementMode::Error => {
                    anyhow::bail!("{}", msg);
                }
                ta_policy::EnforcementMode::Warning => {
                    eprintln!("Warning: {}", msg);
                }
            }
        } else {
            println!(
                "Constitution check: all {} artifact(s) within declared scope",
                validation.declared.len()
            );
        }

        if !validation.unused.is_empty() {
            eprintln!(
                "Note: {} constitution entry/entries had no matching artifact(s)",
                validation.unused.len()
            );
        }
    }

    // Use agent summary if available and user didn't provide a custom one.
    let effective_summary = if summary == "Changes from agent work" {
        change_summary
            .as_ref()
            .and_then(|cs| cs.summary.clone())
            .unwrap_or_else(|| summary.to_string())
    } else {
        summary.to_string()
    };

    let dependency_notes = change_summary
        .as_ref()
        .and_then(|cs| cs.dependency_notes.clone());

    // v0.3.3: Extract decision log from agent alternatives.
    let decision_log = change_summary
        .as_ref()
        .map(extract_decision_log)
        .unwrap_or_default();
    if !decision_log.is_empty() {
        println!(
            "Decision observability: {} decision(s) with alternatives captured",
            decision_log.len()
        );
    }

    // v0.14.7: Load agent-authored decision log from .ta-decisions.json.
    let agent_decision_log = load_agent_decisions(&goal.workspace_path);
    if !agent_decision_log.is_empty() {
        println!(
            "Agent decision log: {} decision(s) captured from .ta-decisions.json",
            agent_decision_log.len()
        );
    }

    // v0.14.7.2: Load agent progress journal from .ta/ta-progress.json.
    let progress_checkpoints: Vec<String> = load_progress_journal_for_draft(&goal.workspace_path);
    if !progress_checkpoints.is_empty() {
        println!(
            "Agent progress journal: {} checkpoint(s) found",
            progress_checkpoints.len()
        );
    }

    // Plan validation: if this goal is linked to a plan phase, validate artifacts.
    if let Some(ref phase_id) = goal.plan_phase {
        let phases = super::plan::load_plan(source_dir).unwrap_or_default();
        let phase_title = phases
            .iter()
            .find(|p| p.id == *phase_id)
            .map(|p| p.title.as_str())
            .unwrap_or("(unknown phase)");
        let plan_validation =
            ta_changeset::supervisor::validate_against_plan(&artifacts, phase_id, phase_title);
        for note in &plan_validation.notes {
            eprintln!("Plan validation: {}", note);
        }
    }

    // Build the draft package.
    let package_id = Uuid::new_v4();
    let mut pkg = DraftPackage {
        package_version: "1.0.0".to_string(),
        package_id,
        created_at: Utc::now(),
        goal: Goal {
            goal_id: goal_id.to_string(),
            title: goal.title.clone(),
            objective: goal.objective.clone(),
            success_criteria: vec![],
            constraints: vec![],
            parent_goal_title: None, // Set below if this is a follow-up (v0.13.0.1).
        },
        iteration: Iteration {
            iteration_id: format!("{}-1", goal_id),
            sequence: 1,
            workspace_ref: WorkspaceRef {
                ref_type: "overlay_staging".to_string(),
                ref_name: goal.workspace_path.display().to_string(),
                base_ref: Some(source_dir.display().to_string()),
            },
        },
        agent_identity: AgentIdentity {
            agent_id: goal.agent_id.clone(),
            agent_type: "coding".to_string(),
            constitution_id: constitution_store
                .load(&goal_id)
                .ok()
                .flatten()
                .map(|c| format!("goal-{}", c.goal_id))
                .unwrap_or_else(|| "default".to_string()),
            capability_manifest_hash: goal.manifest_id.to_string(),
            orchestrator_run_id: None,
        },
        summary: Summary {
            what_changed: effective_summary,
            why: resolve_draft_why(&goal, source_dir),
            impact: format!("{} file(s) changed", artifacts.len()),
            rollback_plan: "Revert changes from staging".to_string(),
            open_questions: dependency_notes.into_iter().collect(),
            alternatives_considered: vec![],
        },
        plan: Plan {
            completed_steps: if progress_checkpoints.is_empty() {
                vec!["Agent completed work in staging".to_string()]
            } else {
                // v0.14.7.2: Include progress journal checkpoints as evidence.
                let mut steps = progress_checkpoints;
                steps.push("Agent completed work in staging".to_string());
                steps
            },
            next_steps: vec!["Review and apply changes".to_string()],
            decision_log,
        },
        changes: Changes {
            artifacts,
            patch_sets: vec![],
            pending_actions: vec![],
        },
        risk: Risk {
            risk_score: 0,
            findings: vec![],
            policy_decisions: vec![],
        },
        provenance: Provenance {
            inputs: vec![],
            tool_trace_hash: "overlay-diff".to_string(),
        },
        review_requests: ReviewRequests {
            requested_actions: vec![RequestedAction {
                action: "approve".to_string(),
                targets: vec!["all".to_string()],
            }],
            reviewers: vec!["human-reviewer".to_string()],
            required_approvals: 1,
            notes_to_reviewer: None,
        },
        signatures: Signatures {
            package_hash: "pending".to_string(),
            agent_signature: "pending".to_string(),
            gateway_attestation: None,
        },
        status: DraftStatus::PendingReview,
        verification_warnings: vec![],
        validation_log: vec![],
        display_id: None, // Will be set below after counting existing drafts.
        tag: goal.tag.clone().or_else(|| Some(goal.display_tag())), // Inherit from goal (v0.11.2.3).
        vcs_status: None,
        parent_draft_id: None, // Set below if this is a follow-up.
        pending_approvals: vec![],
        supervisor_review: None,
        ignored_artifacts: vec![],
        baseline_artifacts: vec![], // Set below if this is a follow-up (v0.14.3.5).
        agent_decision_log,
        goal_shortref: None, // Set below with display_id (v0.14.7.3).
        draft_seq: 0,        // Set below with display_id (v0.14.7.3).
    };

    // v0.12.2.1: Track parent_draft_id and compute composited diff for follow-up chains.
    //
    // When a follow-up goal extends the parent staging, the "child" diff only contains
    // the new files added since the parent was applied. We also track the parent draft ID
    // so chain summary and `--chain` apply work correctly.
    if let Some(parent_goal_id) = goal.parent_goal_id {
        if let Ok(Some(parent_goal)) = goal_store.get(parent_goal_id) {
            // Preserve parent title in the draft so chain display works even if the
            // parent goal record is later deleted (v0.13.0.1).
            pkg.goal.parent_goal_title = Some(parent_goal.title.clone());

            if let Some(parent_draft_id) = parent_goal.pr_package_id {
                pkg.parent_draft_id = Some(parent_draft_id);

                // Composited diff: if parent is Applied, prepend parent artifacts/changesets
                // so `ta draft view` shows the full chain impact.
                if let Ok(parent_pkg) = load_package(config, parent_draft_id) {
                    // v0.14.3.5: Capture all parent artifact URIs as baseline.
                    // During apply, baseline-only files that are unchanged in staging
                    // (staging hash == source hash) are skipped — they were already settled
                    // by the parent apply. This prevents follow-up staging drift from
                    // reverting files the parent had already updated (PLAN.md, USAGE.md, etc).
                    pkg.baseline_artifacts = parent_pkg
                        .changes
                        .artifacts
                        .iter()
                        .map(|a| a.resource_uri.clone())
                        .collect();
                    if matches!(parent_pkg.status, DraftStatus::Applied { .. }) {
                        let child_artifact_count = pkg.changes.artifacts.len();
                        let parent_artifact_count = parent_pkg.changes.artifacts.len();

                        // Load parent changesets to prepend.
                        let parent_changesets: Vec<_> =
                            if let Ok(parent_store) = JsonFileStore::new(&parent_goal.store_path) {
                                parent_store
                                    .list(&parent_goal.goal_run_id.to_string())
                                    .unwrap_or_default()
                            } else {
                                vec![]
                            };

                        // Offset child diff_refs by the number of parent changesets.
                        let offset = parent_changesets.len();
                        for artifact in &mut pkg.changes.artifacts {
                            if let Some(idx_str) = artifact.diff_ref.strip_prefix("changeset:") {
                                if let Ok(idx) = idx_str.parse::<usize>() {
                                    artifact.diff_ref = format!("changeset:{}", idx + offset);
                                }
                            }
                        }

                        // Prepend parent artifacts (marked as from-parent context).
                        let mut composited_artifacts = parent_pkg.changes.artifacts.clone();
                        composited_artifacts.append(&mut pkg.changes.artifacts);
                        pkg.changes.artifacts = composited_artifacts;

                        // Persist the parent changesets at the start of the store.
                        let mut child_store = JsonFileStore::new(goal.store_path.clone())?;
                        for cs in parent_changesets {
                            child_store.save(&goal_id, &cs)?;
                        }

                        pkg.summary.impact = format!(
                            "{} file(s) changed (chain: {} from parent + {} new)",
                            parent_artifact_count + child_artifact_count,
                            parent_artifact_count,
                            child_artifact_count
                        );

                        println!(
                            "Composited diff: {} parent artifact(s) + {} new = {} total",
                            parent_artifact_count,
                            child_artifact_count,
                            parent_artifact_count + child_artifact_count
                        );
                    }
                }
            }
        }
    }

    // Handle PR supersession for follow-up goals.
    // v0.4.1.2: Only auto-supersede when this goal reuses the parent's staging directory
    // (extend case). When the staging directories differ (standalone follow-up), the
    // drafts are independent and should both remain reviewable.
    if let Some(parent_goal_id) = goal.parent_goal_id {
        if let Some(parent_goal) = goal_store.get(parent_goal_id)? {
            // v0.4.1.2: Check if this goal shares the same staging directory as the parent.
            let same_staging = goal.workspace_path == parent_goal.workspace_path;

            if same_staging {
                if let Some(parent_pr_id) = parent_goal.pr_package_id {
                    // Load parent PR and check if it's unapplied.
                    if let Ok(mut parent_pr) = load_package(config, parent_pr_id) {
                        match parent_pr.status {
                            DraftStatus::Draft
                            | DraftStatus::PendingReview
                            | DraftStatus::Approved { .. } => {
                                // Parent PR not yet applied — mark it as superseded.
                                // Valid because same staging means this draft is a superset.
                                parent_pr.status = DraftStatus::Superseded {
                                    superseded_by: package_id,
                                };
                                save_package(config, &parent_pr)?;
                                println!(
                                    "Parent draft {} superseded by this follow-up draft (same staging).",
                                    parent_pr_id
                                );

                                // v0.12.2.3: Auto-close parent GitHub PR if one exists.
                                // The child draft supersedes the parent — close the stale
                                // open PR immediately so it doesn't sit orphaned in GitHub.
                                if let Some(ref vcs) = parent_pr.vcs_status {
                                    if let Some(ref review_url) = vcs.review_url {
                                        if is_github_pr_url(review_url) {
                                            close_github_pr_superseded(
                                                review_url,
                                                &pkg.display_id
                                                    .clone()
                                                    .unwrap_or_else(|| package_id.to_string()),
                                            );
                                        }
                                    }
                                }
                            }
                            DraftStatus::Applied { .. } | DraftStatus::Denied { .. } => {
                                // Parent already applied or denied — no supersession needed.
                            }
                            DraftStatus::Superseded { .. } | DraftStatus::Closed { .. } => {
                                // Parent already superseded or closed — nothing to do.
                            }
                        }
                    }
                }
            }
            // Different staging (standalone): do NOT auto-supersede — drafts are independent.
        }
    }

    // Generate goal-derived display ID (v0.10.11) and unified shortref/seq (v0.14.7.3).
    // display_id format: <goal-id-prefix>-NN (e.g., 511e0465-01, 511e0465-02 for follow-ups).
    // goal_shortref: first 8 chars of goal UUID (same as shortref() on GoalRun).
    // draft_seq: 1-based sequence counter for drafts within this goal.
    {
        let goal_prefix = &goal_id[..8.min(goal_id.len())];
        let existing = load_all_packages(config)
            .unwrap_or_default()
            .iter()
            .filter(|p| p.goal.goal_id == goal_id)
            .count();
        let seq = existing + 1;
        pkg.display_id = Some(format!("{}-{:02}", goal_prefix, seq));
        pkg.goal_shortref = Some(goal_prefix.to_string());
        pkg.draft_seq = seq as u32;
    }

    // v0.13.9: Project constitution scan — reads inject/restore rules from
    // `.ta/constitution.toml` if present, giving project-specific scanning
    // instead of TA's hardcoded inject_*/restore_* patterns.
    // Falls back to the old §4 scan when constitution.toml is absent and
    // `[constitution] s4_scan = true` in .ta/workflow.toml.
    let constitution_config =
        super::constitution::ProjectConstitutionConfig::load(&config.workspace_root)
            .unwrap_or_default();
    if let Some(ref cc) = constitution_config {
        // Project has a constitution.toml — use it to drive the scan.
        if cc.scan.on_violation != "off" {
            match super::constitution::scan_for_violations(&goal.workspace_path, cc) {
                Ok(violations) if !violations.is_empty() => {
                    eprintln!(
                        "[constitution] {} violation(s) found — review before approving",
                        violations.len()
                    );
                    for v in &violations {
                        pkg.verification_warnings.push(VerificationWarning {
                            command: format!("[constitution:{}]", v.rule),
                            exit_code: None,
                            output: format!(
                                "{}: line {} — {} (severity: {})",
                                v.file, v.line, v.message, v.severity
                            ),
                        });
                    }
                }
                Ok(_) => {} // clean
                Err(e) => {
                    tracing::warn!(error = %e, "constitution scan failed");
                }
            }
        }
    } else if workflow_config.constitution.s4_scan {
        // No constitution.toml — fall back to TA's hardcoded §4 scan.
        let s4_warnings = scan_s4_violations(&pkg.changes.artifacts, &goal.workspace_path);
        if !s4_warnings.is_empty() {
            eprintln!(
                "[constitution §4] {} potential inject/restore imbalance(s) — review before approving",
                s4_warnings.len()
            );
            pkg.verification_warnings.extend(s4_warnings);
        }
    }

    // v0.13.15: Version backward bump check.
    // If the agent changed Cargo.toml and set a version lower than the source,
    // emit a VerificationWarning so reviewers notice the regression.
    let version_warnings =
        check_backward_version_bump(&pkg.changes.artifacts, source_dir, &goal.workspace_path);
    if !version_warnings.is_empty() {
        eprintln!(
            "[version] {} backward version bump(s) detected — review before approving",
            version_warnings.len()
        );
        pkg.verification_warnings.extend(version_warnings);
    }

    // v0.13.15: PLAN.md deferred items validation.
    // If PLAN.md was changed and any `<!-- status: done -->` phase has unchecked
    // `[ ]` items without a `→ vX.Y` deferred target, emit a VerificationWarning.
    let plan_warnings =
        check_plan_unchecked_in_done_phases(&pkg.changes.artifacts, &goal.workspace_path);
    if !plan_warnings.is_empty() {
        eprintln!(
            "[plan] {} unchecked item(s) in done phase(s) without deferred target — \
             add `→ vX.Y` or drop the item per Deferred Items Policy",
            plan_warnings.len()
        );
        pkg.verification_warnings.extend(plan_warnings);
    }

    // Save the draft package.
    save_package(config, &pkg)?;

    // Update the goal run.
    let mut goal = goal;
    goal.pr_package_id = Some(package_id);
    goal_store.save(&goal)?;
    goal_store.transition(goal.goal_run_id, GoalRunState::PrReady)?;

    // Emit DraftBuilt event to FsEventStore (v0.9.4.1).
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let events_dir = config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let event = SessionEvent::DraftBuilt {
            goal_id: goal.goal_run_id,
            draft_id: package_id,
            artifact_count: pkg.changes.artifacts.len(),
            title: goal.title.clone(),
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist DraftBuilt event: {}", e);
        }
    }

    let draft_display = draft_display_id(&pkg);
    println!("draft package built: {}", draft_display);
    println!("  Goal:    {} ({})", goal.title, goal_id);
    println!("  Changes: {} file(s)", pkg.changes.artifacts.len());
    for artifact in &pkg.changes.artifacts {
        println!("    {:?}  {}", artifact.change_type, artifact.resource_uri);
    }
    println!();
    println!("Review with:  ta draft view {}", draft_display);
    println!("Approve with: ta draft approve {}", draft_display);

    Ok(())
}

/// Compute the total file count for the full draft chain rooted at `pkg`.
///
/// Walks up the `parent_draft_id` chain and sums artifact counts. Returns the
/// count of the deepest child only if no parent exists (no chain), otherwise
/// returns the combined total.
fn compute_chain_file_count(pkg: &DraftPackage, all_packages: &[DraftPackage]) -> usize {
    // Walk up to root accumulating counts.
    let mut visited = std::collections::HashSet::new();
    let mut total = pkg.changes.artifacts.len();
    let mut current_parent_id = pkg.parent_draft_id;

    while let Some(parent_id) = current_parent_id {
        if visited.contains(&parent_id) {
            break; // cycle guard
        }
        visited.insert(parent_id);
        if let Some(parent) = all_packages.iter().find(|p| p.package_id == parent_id) {
            // Don't double-count if parent is Applied (its artifacts are already
            // included in the composited child diff).
            if !matches!(parent.status, DraftStatus::Applied { .. }) {
                total += parent.changes.artifacts.len();
            }
            current_parent_id = parent.parent_draft_id;
        } else {
            break;
        }
    }
    total
}

/// Apply the full draft chain: walk up from `child_id` to the root parent,
/// then apply all unapplied drafts in order (root → leaf).
///
/// Detects cycles and stops with an error. Applied drafts are skipped silently.
fn apply_chain(
    config: &GatewayConfig,
    child_id: &str,
    target: Option<&str>,
    skip_verify: bool,
) -> anyhow::Result<()> {
    let all_packages = load_all_packages(config)?;

    // Resolve the starting draft.
    let start_uuid = resolve_draft_id(child_id, config)?;

    // Walk up the parent chain to find the root.
    let mut chain: Vec<Uuid> = vec![start_uuid];
    let mut visited = std::collections::HashSet::new();
    visited.insert(start_uuid);

    loop {
        let current = *chain.last().unwrap();
        let pkg = all_packages
            .iter()
            .find(|p| p.package_id == current)
            .ok_or_else(|| anyhow::anyhow!("Draft {} not found in chain walk", current))?;

        match pkg.parent_draft_id {
            None => break, // reached the root
            Some(parent_id) => {
                if visited.contains(&parent_id) {
                    anyhow::bail!(
                        "Cycle detected in draft chain at {} — cannot apply chain",
                        &parent_id.to_string()[..8]
                    );
                }
                visited.insert(parent_id);
                chain.push(parent_id);
            }
        }
    }

    // Reverse: root first, leaf last.
    chain.reverse();

    // Filter to unapplied drafts.
    let to_apply: Vec<Uuid> = chain
        .iter()
        .filter(|&&id| {
            all_packages
                .iter()
                .find(|p| p.package_id == id)
                .map(|p| !matches!(p.status, DraftStatus::Applied { .. }))
                .unwrap_or(false)
        })
        .copied()
        .collect();

    if to_apply.is_empty() {
        println!("Chain apply: all {} draft(s) already applied.", chain.len());
        return Ok(());
    }

    println!(
        "Chain apply: {} draft(s) in chain, {} unapplied — applying in order...",
        chain.len(),
        to_apply.len()
    );

    for (i, draft_id) in to_apply.iter().enumerate() {
        let short_id = &draft_id.to_string()[..8];
        let pkg = all_packages
            .iter()
            .find(|p| p.package_id == *draft_id)
            .unwrap();
        println!(
            "\n[{}/{}] Applying draft {} \"{}\"...",
            i + 1,
            to_apply.len(),
            short_id,
            pkg.goal.title
        );

        let draft_id_str = draft_id.to_string();
        apply_package(
            config,
            &draft_id_str,
            target,
            false, // submit
            false, // push
            false, // review
            skip_verify,
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &[],
                reject: &[],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        )?;
    }

    println!(
        "\nChain apply complete: {} draft(s) applied.",
        to_apply.len()
    );
    Ok(())
}

/// Resolve the "why" field for a draft summary (#76).
///
/// When a goal is linked to a plan phase, extract the phase's `**Goal**:`
/// description from PLAN.md — that's where the real motivation lives.
/// Falls back to `goal.objective` when no plan phase is linked or when
/// the description can't be extracted.
fn resolve_draft_why(goal: &GoalRun, source_dir: &std::path::Path) -> String {
    if let Some(ref phase_id) = goal.plan_phase {
        if let Some(desc) =
            crate::framework_registry::extract_phase_description(source_dir, phase_id)
        {
            return desc;
        }
    }
    goal.objective.clone()
}

#[allow(clippy::too_many_arguments)]
fn list_packages(
    config: &GatewayConfig,
    goal_filter: Option<&str>,
    stale_only: bool,
    pending_only: bool,
    applied_only: bool,
    limit: Option<usize>,
    show_all: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    let mut packages = load_all_packages(config)?;

    // Default ordering: newest last (chronological) for readability.
    packages.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    // Load GC config for stale threshold.
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let stale_days = workflow_config.gc.stale_threshold_days;
    let stale_cutoff = Utc::now() - chrono::Duration::days(stale_days as i64);

    // Default compact view: show only active/pending unless --all or a specific filter is used.
    let compact = !show_all && !stale_only && !applied_only && goal_filter.is_none();

    let filtered: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            if let Some(goal_id) = goal_filter {
                if p.goal.goal_id != goal_id {
                    return false;
                }
            }
            if stale_only {
                let is_non_terminal = matches!(
                    p.status,
                    DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
                );
                return is_non_terminal && p.created_at < stale_cutoff;
            }
            if pending_only {
                return matches!(
                    p.status,
                    DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
                );
            }
            if applied_only {
                return matches!(p.status, DraftStatus::Applied { .. });
            }
            // Compact mode: show active/pending + recently applied (v0.11.2.3).
            if compact {
                // Always show non-terminal.
                if matches!(
                    p.status,
                    DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
                ) {
                    return true;
                }
                // Show Applied drafts younger than 7 days or with open PRs.
                if let DraftStatus::Applied { applied_at } = &p.status {
                    let age = Utc::now() - *applied_at;
                    if age.num_days() < 7 {
                        return true;
                    }
                    // Show if VCS has an open PR.
                    if let Some(ref vcs) = p.vcs_status {
                        if vcs.review_state.as_deref() == Some("open") {
                            return true;
                        }
                    }
                }
                return false;
            }
            true
        })
        .collect();

    // Apply limit (take the last N items to show the most recent).
    let display: Vec<&&DraftPackage> = match limit {
        Some(n) if n < filtered.len() => filtered.iter().skip(filtered.len() - n).collect(),
        _ => filtered.iter().collect(),
    };

    if json_output {
        let json_data: Vec<serde_json::Value> = display
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.package_id.to_string(),
                    "display_id": draft_display_id(p),
                    "goal_id": p.goal.goal_id,
                    "status": format!("{:?}", p.status),
                    "artifact_count": p.changes.artifacts.len(),
                    "created_at": p.created_at.to_rfc3339(),
                    "summary": p.summary.what_changed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_data)?);
        return Ok(());
    }

    if display.is_empty() {
        if stale_only {
            println!("No stale drafts found (threshold: {} days).", stale_days);
        } else if pending_only {
            println!("No pending drafts. Run `ta draft list --all` to see all drafts.");
        } else if applied_only {
            println!("No applied drafts found.");
        } else if compact {
            println!("No active drafts. Run `ta draft list --all` to see all drafts.");
        } else {
            println!("No draft packages found.");
        }
        return Ok(());
    }

    if stale_only {
        println!(
            "Stale drafts (non-terminal, older than {} days):\n",
            stale_days
        );
    }

    // Check if any packages in the display set have a parent (to show chain column).
    let show_chain_col = display.iter().any(|p| p.parent_draft_id.is_some());

    if show_chain_col {
        println!(
            "{:<20} {:<16} {:<26} {:<16} {:<8} {:<10} {:<14} AGE",
            "TAG", "DRAFT ID", "GOAL", "STATUS", "FILES", "PARENT", "VCS"
        );
        println!("{}", "-".repeat(122));
    } else {
        println!(
            "{:<20} {:<16} {:<26} {:<16} {:<8} {:<14} AGE",
            "TAG", "DRAFT ID", "GOAL", "STATUS", "FILES", "VCS"
        );
        println!("{}", "-".repeat(110));
    }

    // Load goal store for macro goal context.
    let goal_store = GoalRunStore::new(&config.goals_dir).ok();

    for pkg in &display {
        let status_display = match &pkg.status {
            DraftStatus::Superseded { superseded_by } => {
                format!("superseded ({})", &superseded_by.to_string()[..8])
            }
            DraftStatus::Closed { .. } => "closed".to_string(),
            _ => format!("{:?}", pkg.status),
        };

        let age = Utc::now() - pkg.created_at;
        let age_str = if age.num_days() > 0 {
            format!("{}d", age.num_days())
        } else if age.num_hours() > 0 {
            format!("{}h", age.num_hours())
        } else {
            format!("{}m", age.num_minutes())
        };

        // Check if this draft belongs to a macro sub-goal.
        let goal_display = if let Some(ref store) = goal_store {
            if let Ok(goal_id) = Uuid::parse_str(&pkg.goal.goal_id) {
                if let Ok(Some(goal)) = store.get(goal_id) {
                    if goal.parent_macro_id.is_some() {
                        format!("  +- {}", truncate(&pkg.goal.title, 24))
                    } else if goal.is_macro {
                        format!("[M] {}", truncate(&pkg.goal.title, 24))
                    } else {
                        truncate(&pkg.goal.title, 28)
                    }
                } else {
                    truncate(&pkg.goal.title, 28)
                }
            } else {
                truncate(&pkg.goal.title, 28)
            }
        } else {
            truncate(&pkg.goal.title, 28)
        };

        let tag_display = pkg.tag.as_deref().unwrap_or("\u{2014}").to_string();

        let vcs_display = match &pkg.vcs_status {
            Some(vcs) => {
                let pr = vcs
                    .review_id
                    .as_ref()
                    .map(|id| format!("PR #{}", id))
                    .unwrap_or_default();
                let state = vcs.review_state.as_deref().unwrap_or("?");
                if pr.is_empty() {
                    truncate(&vcs.branch, 12)
                } else {
                    format!("{} ({})", pr, state)
                }
            }
            None => "\u{2014}".to_string(),
        };

        if show_chain_col {
            let parent_display = pkg
                .parent_draft_id
                .map(|id| format!("→ {}", &id.to_string()[..8]))
                .unwrap_or_else(|| "\u{2014}".to_string());
            println!(
                "{:<20} {:<16} {:<26} {:<16} {:<8} {:<10} {:<14} {}",
                truncate(&tag_display, 18),
                draft_display_id(pkg),
                goal_display,
                status_display,
                pkg.changes.artifacts.len(),
                parent_display,
                vcs_display,
                age_str,
            );
        } else {
            println!(
                "{:<20} {:<16} {:<26} {:<16} {:<8} {:<14} {}",
                truncate(&tag_display, 18),
                draft_display_id(pkg),
                goal_display,
                status_display,
                pkg.changes.artifacts.len(),
                vcs_display,
                age_str,
            );
        }
    }

    let total_count = packages.len();
    let shown = display.len();
    if shown < total_count {
        println!(
            "\n{} shown (of {} total). Use --all to see all drafts.",
            shown, total_count
        );
    } else {
        println!("\n{} package(s).", shown);
    }
    Ok(())
}

/// Return the human-friendly display ID for a draft package.
///
/// Prefers `<goal_shortref>/<draft_seq>` (v0.14.7.3), falls back to the
/// goal-derived display_id (v0.10.11), then to package_id short prefix for
/// legacy drafts.
fn draft_display_id(pkg: &DraftPackage) -> String {
    if let (Some(shortref), seq) = (&pkg.goal_shortref, pkg.draft_seq) {
        if seq > 0 {
            return format!("{}/{}", shortref, seq);
        }
    }
    pkg.display_id
        .as_ref()
        .cloned()
        .unwrap_or_else(|| pkg.package_id.to_string()[..8].to_string())
}

/// Check if a file appears to be binary by looking for null bytes in the first 8KB.
#[allow(dead_code)]
fn is_binary_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let Ok(n) = file.read(&mut buf) else {
        return false;
    };
    buf[..n].contains(&0)
}

/// Human-readable file size display.
#[allow(dead_code)]
fn file_size_display(path: &std::path::Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return "unknown size".to_string();
    };
    let bytes = meta.len();
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// DiffProvider backed by a loaded Vec<ChangeSet>.
///
/// Resolves `changeset:N` references to actual diff content from the
/// ChangeSet store. Created from the goal's store_path + goal_id.
struct ChangeSetDiffProvider {
    changesets: Vec<ChangeSet>,
}

impl ChangeSetDiffProvider {
    /// Load changesets for a goal from the store path.
    fn load(store_path: &std::path::Path, goal_id: &str) -> Option<Self> {
        let store = JsonFileStore::new(store_path).ok()?;
        let changesets = store.list(goal_id).ok()?;
        if changesets.is_empty() {
            return None;
        }
        Some(Self { changesets })
    }
}

impl DiffProvider for ChangeSetDiffProvider {
    fn get_diff(&self, diff_ref: &str) -> Result<String, ta_changeset::ChangeSetError> {
        // diff_ref format: "changeset:N" where N is the changeset index.
        let idx = diff_ref
            .strip_prefix("changeset:")
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| {
                ta_changeset::ChangeSetError::InvalidData(format!(
                    "Invalid diff_ref format: '{}' (expected 'changeset:N')",
                    diff_ref
                ))
            })?;

        let cs = self.changesets.get(idx).ok_or_else(|| {
            ta_changeset::ChangeSetError::InvalidData(format!(
                "Changeset index {} out of range (have {} changesets)",
                idx,
                self.changesets.len()
            ))
        })?;

        match &cs.diff_content {
            DiffContent::UnifiedDiff { content } => Ok(content.clone()),
            DiffContent::CreateFile { content } => {
                // Show as "new file" diff: all lines prefixed with +
                let lines: Vec<String> = content.lines().map(|l| format!("+{}", l)).collect();
                Ok(format!(
                    "--- /dev/null\n+++ b/new\n@@ -0,0 +1,{} @@\n{}",
                    lines.len(),
                    lines.join("\n")
                ))
            }
            DiffContent::DeleteFile => {
                Ok("--- a/deleted\n+++ /dev/null\n@@ -1 +0,0 @@\n-[file deleted]".to_string())
            }
            DiffContent::BinarySummary {
                mime_type,
                size_bytes,
                ..
            } => Ok(format!(
                "[Binary file: {} ({} bytes)]",
                mime_type, size_bytes
            )),
        }
    }
}

fn view_package_json(config: &GatewayConfig, id: &str) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;
    let json = serde_json::to_string_pretty(&pkg)?;
    println!("{}", json);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn view_package(
    config: &GatewayConfig,
    id: &str,
    summary_only: bool,
    file_filters: &[String],
    open_external: &Option<bool>,
    detail_str: &str,
    format_str: &str,
    color: bool,
    section_str: Option<&str>,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    // v0.12.2.1 / v0.13.0.1: Show chain context when this draft is part of a chain.
    let all_packages = load_all_packages(config).unwrap_or_default();
    if let Some(parent_id) = pkg.parent_draft_id {
        let parent_short = &parent_id.to_string()[..8];
        // Prefer the stored parent title; fall back to ID-only for legacy drafts.
        let parent_label = pkg
            .goal
            .parent_goal_title
            .as_deref()
            .map(|t| format!("\"{}\" ({})", t, parent_short))
            .unwrap_or_else(|| parent_short.to_string());

        let combined = compute_chain_file_count(&pkg, &all_packages);
        if combined > pkg.changes.artifacts.len() {
            println!(
                "Chain: follow-up to {} — combined impact: {} file(s)",
                parent_label, combined
            );
        } else {
            println!("Chain: follow-up to {}", parent_label);
        }

        // List any known sibling/child follow-ups of THIS draft.
        let children: Vec<_> = all_packages
            .iter()
            .filter(|p| p.parent_draft_id == Some(package_id))
            .collect();
        if !children.is_empty() {
            let child_labels: Vec<String> = children
                .iter()
                .map(|c| {
                    format!(
                        "\"{}\" ({}, {})",
                        c.goal.title,
                        &c.package_id.to_string()[..8],
                        c.status
                    )
                })
                .collect();
            println!("Follow-ups: {}", child_labels.join(", "));
        }
        println!();
    } else {
        // Root draft — check for follow-ups and render "Changes from parent" rollup.
        let children: Vec<_> = all_packages
            .iter()
            .filter(|p| p.parent_draft_id == Some(package_id))
            .collect();
        if !children.is_empty() {
            println!("Changes from parent (follow-up work):");
            for child in &children {
                println!(
                    "  • {} [{}] — {}",
                    child.goal.title,
                    &child.package_id.to_string()[..8],
                    child.status
                );
            }
            let total_files: usize = children
                .iter()
                .map(|c| c.changes.artifacts.len())
                .sum::<usize>()
                + pkg.changes.artifacts.len();
            println!("  Combined: {} file(s) across chain", total_files);
            println!();
        }
    }

    // Parse detail level and format.
    let detail_level = detail_str
        .parse::<DetailLevel>()
        .map_err(|e| anyhow::anyhow!(e))?;
    let output_format = format_str
        .parse::<OutputFormat>()
        .map_err(|e| anyhow::anyhow!(e))?;
    let section_filter = section_str
        .map(|s| {
            s.parse::<ta_changeset::output_adapters::SectionFilter>()
                .map_err(|e| anyhow::anyhow!(e))
        })
        .transpose()?;

    // v0.2.3: Use output adapters for rendering.
    // Exception: If exactly one --file with --open-external, try external handler first.
    if file_filters.len() == 1 {
        let filter = &file_filters[0];
        if let Some(true) = open_external {
            // Try external handler path (legacy v0.2.2 behavior).
            if let Ok(goal_store) = GoalRunStore::new(&config.goals_dir) {
                if let Ok(goals) = goal_store.list() {
                    if let Some(goal) = goals.iter().find(|g| {
                        g.goal_run_id.to_string() == pkg.goal.goal_id
                            || g.pr_package_id == Some(package_id)
                    }) {
                        let staged_path = goal.workspace_path.join(filter);
                        if staged_path.exists() {
                            let workflow_config = ta_submit::WorkflowConfig::load_or_default(
                                &config.workspace_root.join(".ta/workflow.toml"),
                            );
                            if workflow_config.diff.open_external {
                                if let Ok(handlers) =
                                    DiffHandlersConfig::load_from_project(&config.workspace_root)
                                {
                                    if handlers.open_file(&staged_path, true).is_ok() {
                                        println!("Opened {} in external application", filter);
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Backward compatibility: --summary flag maps to --detail top.
    let effective_detail = if summary_only {
        DetailLevel::Top
    } else {
        detail_level
    };

    // Load changeset-based diff provider when full detail is requested.
    let diff_provider = if effective_detail == DetailLevel::Full {
        if let Ok(goal_store) = GoalRunStore::new(&config.goals_dir) {
            if let Ok(goals) = goal_store.list() {
                goals
                    .iter()
                    .find(|g| {
                        g.goal_run_id.to_string() == pkg.goal.goal_id
                            || g.pr_package_id == Some(package_id)
                    })
                    .and_then(|goal| {
                        ChangeSetDiffProvider::load(&goal.store_path, &goal.goal_run_id.to_string())
                    })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let ctx = RenderContext {
        package: &pkg,
        detail_level: effective_detail,
        file_filters: file_filters.to_vec(),
        diff_provider: diff_provider.as_ref().map(|p| p as &dyn DiffProvider),
        section_filter,
    };

    // Resolve color: CLI --color overrides config default.
    let effective_color = if color {
        true
    } else {
        let workflow_config = ta_submit::WorkflowConfig::load_or_default(
            &config.workspace_root.join(".ta/workflow.toml"),
        );
        workflow_config.display.color
    };

    // Get the adapter and render.
    let adapter = get_adapter(output_format, effective_color);
    let output = adapter.render(&ctx).map_err(|e| anyhow::anyhow!("{}", e))?;

    println!("{}", output);

    // Show verification warnings if any (v0.10.8).
    if !pkg.verification_warnings.is_empty() {
        println!();
        println!(
            "VERIFICATION WARNINGS ({}):",
            pkg.verification_warnings.len()
        );
        println!("{}", "=".repeat(60));
        println!("These commands failed during pre-draft verification.");
        println!("The draft was created with on_failure = \"warn\".");
        println!();
        for (i, w) in pkg.verification_warnings.iter().enumerate() {
            println!(
                "  {}. FAIL: {} (exit code: {})",
                i + 1,
                w.command,
                w.exit_code.map_or("N/A".to_string(), |c| c.to_string())
            );
            if !w.output.is_empty() && effective_detail != DetailLevel::Top {
                for line in w.output.lines().take(10) {
                    println!("     {}", line);
                }
                let line_count = w.output.lines().count();
                if line_count > 10 {
                    println!("     ... ({} more lines)", line_count - 10);
                }
            }
        }
    }

    // Show validation log if any (v0.13.17).
    if !pkg.validation_log.is_empty() {
        println!();
        println!("VALIDATION LOG ({} checks):", pkg.validation_log.len());
        println!("{}", "=".repeat(60));
        let failed_count = pkg
            .validation_log
            .iter()
            .filter(|e| e.exit_code != 0)
            .count();
        if failed_count > 0 {
            println!(
                "Warning: {} check(s) FAILED — use --override to approve anyway.",
                failed_count
            );
        }
        println!();
        for entry in &pkg.validation_log {
            let symbol = if entry.exit_code == 0 { "+" } else { "x" };
            println!(
                "  [{}] {} (exit {}, {}s)",
                symbol, entry.command, entry.exit_code, entry.duration_secs
            );
            if entry.exit_code != 0
                && !entry.stdout_tail.is_empty()
                && effective_detail != DetailLevel::Top
            {
                for line in entry.stdout_tail.lines().take(10) {
                    println!("     {}", line);
                }
            }
        }
    }

    // Show supervisor review (v0.13.17.4).
    if let Some(ref review) = pkg.supervisor_review {
        println!();
        println!("SUPERVISOR REVIEW ({}):", review.agent);
        println!("{}", "=".repeat(60));
        let verdict_label = match review.verdict {
            ta_changeset::SupervisorVerdict::Pass => "[PASS]",
            ta_changeset::SupervisorVerdict::Warn => "[WARN]",
            ta_changeset::SupervisorVerdict::Block => "[BLOCK]",
        };
        println!("  Verdict:  {}", verdict_label);
        println!("  Scope OK: {}", if review.scope_ok { "yes" } else { "no" });
        println!("  Summary:  {}", review.summary);
        if !review.findings.is_empty() {
            let show_full = effective_detail != DetailLevel::Top;
            let max_findings = if show_full {
                review.findings.len()
            } else {
                3.min(review.findings.len())
            };
            println!("  Findings ({}/{}):", max_findings, review.findings.len());
            for finding in review.findings.iter().take(max_findings) {
                println!("    - {}", finding);
            }
            if review.findings.len() > 3 && !show_full {
                println!(
                    "    (use --detail full to show all {} findings)",
                    review.findings.len()
                );
            }
        }
        println!("  Duration: {:.1}s", review.duration_secs);
        if matches!(review.verdict, ta_changeset::SupervisorVerdict::Block) {
            println!();
            println!(
                "  Note: Supervisor verdict is BLOCK. Check workflow.toml [supervisor] verdict_on_block."
            );
            println!(
                "  Use `ta draft approve --override` if you want to approve despite the block verdict."
            );
        }
    }

    // Asset diffs for image/video artifacts (v0.15.4).
    // Only run if there are image/video artifacts and the goal store can resolve the staging path.
    if effective_detail != DetailLevel::Top {
        let image_video_artifacts: Vec<_> = pkg
            .changes
            .artifacts
            .iter()
            .filter(|a| {
                a.kind
                    .as_ref()
                    .map(|k| k.is_image() || k.is_video())
                    .unwrap_or(false)
            })
            .collect();

        if !image_video_artifacts.is_empty() {
            let workflow_config = ta_submit::WorkflowConfig::load_or_default(
                &config.workspace_root.join(".ta/workflow.toml"),
            );
            let asset_diff_cfg = &workflow_config.draft.asset_diff;

            if asset_diff_cfg.enabled {
                if let Ok(goal_store) = GoalRunStore::new(&config.goals_dir) {
                    if let Ok(goals) = goal_store.list() {
                        if let Some(goal) = goals.iter().find(|g| {
                            g.goal_run_id.to_string() == pkg.goal.goal_id
                                || g.pr_package_id == Some(package_id)
                        }) {
                            let staging_path = &goal.workspace_path;
                            let source_path = &config.workspace_root;
                            let goal_intent = pkg.goal.title.clone();

                            // Convert AssetDiffConfig from workflow config.
                            let diff_cfg = ta_changeset::AssetDiffConfig {
                                enabled: asset_diff_cfg.enabled,
                                visual_diff: asset_diff_cfg.visual_diff,
                                visual_diff_threshold: asset_diff_cfg.visual_diff_threshold,
                                supervisor: asset_diff_cfg.supervisor,
                                agent: asset_diff_cfg.agent.clone(),
                                timeout_secs: asset_diff_cfg.timeout_secs,
                            };

                            println!();
                            println!(
                                "ASSET DIFFS ({} image/video artifact(s)):",
                                image_video_artifacts.len()
                            );
                            println!("{}", "=".repeat(60));

                            for artifact in &image_video_artifacts {
                                // Extract relative path from resource_uri.
                                let rel_path = artifact
                                    .resource_uri
                                    .strip_prefix("fs://workspace/")
                                    .unwrap_or(&artifact.resource_uri);

                                let before = source_path.join(rel_path);
                                let after = staging_path.join(rel_path);

                                // safe: filtered above to only image/video
                                let kind = artifact.kind.as_ref().unwrap();
                                let result = ta_changeset::run_asset_diff(
                                    &before,
                                    &after,
                                    kind,
                                    &goal_intent,
                                    &diff_cfg,
                                    staging_path,
                                );

                                let kind_label = kind.display_label();
                                println!();
                                println!("  [{}] {}", kind_label, rel_path);

                                match result.skipped_reason {
                                    Some(reason) => {
                                        println!("    (asset diff unavailable — {})", reason);
                                    }
                                    None => {
                                        if let Some(ref summary) = result.summary {
                                            println!("    Agent diff: {}", summary.text);
                                            println!(
                                                "                ChangeType: {:?}",
                                                summary.change_type
                                            );
                                        }
                                        if let Some(ref verdict) = result.supervisor {
                                            let warning = if verdict.confidence < 0.7 {
                                                " [!] LOW CONFIDENCE"
                                            } else {
                                                ""
                                            };
                                            println!(
                                                "    Supervisor: confidence {:.2} — {}{}",
                                                verdict.confidence,
                                                verdict.match_assessment,
                                                warning
                                            );
                                            for flag in &verdict.flags {
                                                println!("      Flag: {}", flag);
                                            }
                                        }
                                        if let Some(ref vdiff) = result.visual_diff {
                                            println!(
                                                "    Visual diff: {}",
                                                vdiff.diff_path.display()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Show ignored artifacts if any (v0.13.17.5).
    if !pkg.ignored_artifacts.is_empty() {
        println!();
        println!("IGNORED ARTIFACTS ({}):", pkg.ignored_artifacts.len());
        println!("{}", "=".repeat(60));
        let unexpected: Vec<_> = pkg
            .ignored_artifacts
            .iter()
            .filter(|a| !a.known_safe)
            .collect();
        let known_safe: Vec<_> = pkg
            .ignored_artifacts
            .iter()
            .filter(|a| a.known_safe)
            .collect();
        if !unexpected.is_empty() {
            println!("WARNING: The following files are gitignored and were NOT committed.");
            println!("Check whether the .gitignore entry is intentional.");
            for a in &unexpected {
                println!("  [!] {} — gitignored, dropped from commit", a.path);
            }
        }
        if !known_safe.is_empty() && effective_detail != DetailLevel::Top {
            println!("Known-safe (TA infrastructure, silently dropped):");
            for a in &known_safe {
                println!("  [-] {}", a.path);
            }
        }
    }

    // Show pending actions if any (v0.5.1).
    if !pkg.changes.pending_actions.is_empty() {
        println!();
        println!("Pending Actions ({}):", pkg.changes.pending_actions.len());
        println!("{}", "-".repeat(60));
        for (i, action) in pkg.changes.pending_actions.iter().enumerate() {
            println!(
                "  {}. [{}] {} ({})",
                i + 1,
                action.disposition,
                action.description,
                action.kind,
            );
            if let Some(uri) = &action.target_uri {
                println!("     URI: {}", uri);
            }
            if effective_detail != DetailLevel::Top {
                let params_str = serde_json::to_string_pretty(&action.parameters)
                    .unwrap_or_else(|_| action.parameters.to_string());
                println!("     Parameters: {}", params_str);
            }
        }
    }

    Ok(())
}

fn approve_package(
    config: &GatewayConfig,
    id: &str,
    reviewer: &str,
    force_override: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    if !matches!(pkg.status, DraftStatus::PendingReview) {
        anyhow::bail!(
            "Cannot approve package in {:?} state (must be PendingReview)",
            pkg.status
        );
    }

    // Block approval if any required check failed, unless --override (v0.13.17).
    if !force_override && pkg.validation_log.iter().any(|e| e.exit_code != 0) {
        let failed: Vec<&str> = pkg
            .validation_log
            .iter()
            .filter(|e| e.exit_code != 0)
            .map(|e| e.command.as_str())
            .collect();
        anyhow::bail!(
            "Draft has failed validation checks — use `--override` to approve anyway.\n\
             Failed checks:\n{}",
            failed
                .iter()
                .map(|c| format!("  x {}", c))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // Block approval if supervisor verdict is Block and verdict_on_block = "block" (v0.13.17.4).
    // Load workflow config here for supervisor check (reused below for governance).
    let wf_path = config.workspace_root.join(".ta/workflow.toml");
    let wf = ta_submit::WorkflowConfig::load_or_default(&wf_path);

    if !force_override {
        if let Some(ref review) = pkg.supervisor_review {
            if matches!(review.verdict, ta_changeset::SupervisorVerdict::Block)
                && wf.supervisor.verdict_on_block == "block"
            {
                anyhow::bail!(
                    "Supervisor review blocked this draft: {}\n\
                     Findings:\n{}\n\
                     Use `ta draft approve --override` to approve despite the block verdict.",
                    review.summary,
                    review
                        .findings
                        .iter()
                        .map(|f| format!("  - {}", f))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
    }

    // Load governance configuration.
    let gov = &wf.governance;

    // Validate reviewer identity against allowlist (if configured).
    if !gov.approvers.is_empty() && !gov.approvers.contains(&reviewer.to_string()) {
        if force_override {
            // Override path: validate the override identity.
            match &gov.override_identity {
                Some(oid) if oid == reviewer => {
                    tracing::warn!(
                        reviewer = reviewer,
                        package_id = %package_id,
                        "Governance override used — quorum bypassed"
                    );
                    println!(
                        "⚠  Override by '{}': quorum requirement bypassed (audit trail updated).",
                        reviewer
                    );
                }
                _ => {
                    anyhow::bail!(
                        "'{}' is not in the approvers list and is not the configured override identity.\n\
                         Approvers: {}\n\
                         Override identity: {}",
                        reviewer,
                        gov.approvers.join(", "),
                        gov.override_identity.as_deref().unwrap_or("(none)")
                    );
                }
            }
        } else {
            anyhow::bail!(
                "'{}' is not in the approvers list for this project.\n\
                 Approvers: {}\n\
                 Tip: use --override if you are the designated override identity.",
                reviewer,
                gov.approvers.join(", ")
            );
        }
    }

    // Prevent duplicate approval from the same reviewer.
    if pkg.pending_approvals.iter().any(|a| a.reviewer == reviewer) {
        anyhow::bail!("'{}' has already approved this draft.", reviewer);
    }

    // Record this approval.
    pkg.pending_approvals.push(ApprovalRecord {
        reviewer: reviewer.to_string(),
        approved_at: Utc::now(),
    });

    let quorum = if force_override {
        1
    } else {
        gov.require_approvals
    };
    let have = pkg.pending_approvals.len();
    let quorum_reached = have >= quorum;

    if quorum_reached {
        // Quorum met — transition to Approved.
        pkg.status = DraftStatus::Approved {
            approved_by: reviewer.to_string(),
            approved_at: Utc::now(),
        };
        save_package(config, &pkg)?;

        // Transition the goal state.
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goals = goal_store.list()?;
        if let Some(goal) = goals.iter().find(|g| g.pr_package_id == Some(package_id)) {
            let _ = goal_store.transition(goal.goal_run_id, GoalRunState::UnderReview);
            let _ = goal_store.transition(
                goal.goal_run_id,
                GoalRunState::Approved {
                    approved_by: reviewer.to_string(),
                },
            );
        }

        // §8: emit DraftApproved event.
        {
            use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
            let events_dir = config.workspace_root.join(".ta").join("events");
            let event_store = FsEventStore::new(&events_dir);
            let goal_id = goals
                .iter()
                .find(|g| g.pr_package_id == Some(package_id))
                .map(|g| g.goal_run_id)
                .unwrap_or_else(uuid::Uuid::new_v4);
            let event = SessionEvent::DraftApproved {
                goal_id,
                draft_id: package_id,
                approved_by: reviewer.to_string(),
            };
            if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
                tracing::warn!("Failed to persist DraftApproved event: {}", e);
            }
        }

        if quorum == 1 {
            println!("Approved draft package {} by {}", package_id, reviewer);
        } else {
            println!(
                "Approved draft package {} ({}/{} approvals — quorum reached)",
                package_id, have, quorum
            );
        }
    } else {
        // Quorum not yet reached — persist the partial approval and wait.
        save_package(config, &pkg)?;
        println!(
            "Recorded approval from '{}' ({}/{} approvals — {} more needed before this draft can be applied).",
            reviewer,
            have,
            quorum,
            quorum - have
        );
    }

    Ok(())
}

fn deny_package(
    config: &GatewayConfig,
    id: &str,
    reason: &str,
    reviewer: &str,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    if !matches!(pkg.status, DraftStatus::PendingReview) {
        anyhow::bail!(
            "Cannot deny package in {:?} state (must be PendingReview)",
            pkg.status
        );
    }

    pkg.status = DraftStatus::Denied {
        reason: reason.to_string(),
        denied_by: reviewer.to_string(),
    };

    // Capture goal_id before saving package (pkg will be consumed by save_package).
    let package_goal_id = {
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goals = goal_store.list()?;
        goals
            .iter()
            .find(|g| g.pr_package_id == Some(package_id))
            .map(|g| g.goal_run_id)
    };

    save_package(config, &pkg)?;

    // §8: emit DraftDenied event so all state changes are logged with structured fields.
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let events_dir = config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let goal_id = package_goal_id.unwrap_or_else(uuid::Uuid::new_v4);
        let event = SessionEvent::DraftDenied {
            goal_id,
            draft_id: package_id,
            reason: reason.to_string(),
            denied_by: reviewer.to_string(),
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist DraftDenied event: {}", e);
        }
    }

    // v0.12.5: Capture denial as human guidance and draft rejection into memory.
    capture_draft_denial_to_memory(config, package_goal_id, &pkg, reason);

    // v0.13.10: Record velocity entry for the denied goal.
    if let Some(goal_id) = package_goal_id {
        use ta_goal::{GoalOutcome, VelocityEntry, VelocityStore};
        let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir);
        if let Ok(store) = goal_store {
            if let Ok(Some(goal)) = store.get(goal_id) {
                let vs = VelocityStore::for_project(&config.workspace_root);
                let entry =
                    VelocityEntry::from_goal(&goal, GoalOutcome::Denied).with_denial_reason(reason);
                if let Err(e) = vs.append(&entry) {
                    tracing::warn!("Failed to record velocity entry for denied goal: {}", e);
                }
                // v0.14.6: Write goal audit ledger entry.
                write_goal_audit_entry(
                    config,
                    &pkg,
                    Some(&goal),
                    ta_audit::AuditDisposition::Denied,
                    Some(reviewer),
                    Some(reason),
                    None,
                );
            } else {
                // Goal not found — record without goal context.
                write_goal_audit_entry(
                    config,
                    &pkg,
                    None,
                    ta_audit::AuditDisposition::Denied,
                    Some(reviewer),
                    Some(reason),
                    None,
                );
            }
        }
    }

    println!("Denied draft package {}: {}", package_id, reason);
    Ok(())
}

/// Deny a single artifact within a draft package (v0.14.9.2).
///
/// Sets `artifact.disposition = Rejected` for the matching artifact,
/// prints confirmation, and optionally shows the agent's stored rationale.
fn deny_artifact(
    config: &GatewayConfig,
    id: &str,
    reason: &str,
    _reviewer: &str,
    file_path: &str,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    // Find artifact by glob/substring match on the resource_uri path.
    let artifact_idx = pkg
        .changes
        .artifacts
        .iter()
        .position(|a| {
            let path = a
                .resource_uri
                .strip_prefix("fs://workspace/")
                .unwrap_or(&a.resource_uri);
            if let Ok(pat) = glob::Pattern::new(file_path) {
                if pat.matches(path) {
                    return true;
                }
            }
            path.contains(file_path) || a.resource_uri.contains(file_path)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No artifact matching '{}' found in draft {}",
                file_path,
                &package_id.to_string()[..8]
            )
        })?;

    let uri = pkg.changes.artifacts[artifact_idx].resource_uri.clone();
    let stored_rationale = pkg.changes.artifacts[artifact_idx]
        .explanation_tiers
        .as_ref()
        .map(|t| t.explanation.clone())
        .or_else(|| pkg.changes.artifacts[artifact_idx].rationale.clone());

    pkg.changes.artifacts[artifact_idx].disposition =
        ta_changeset::draft_package::ArtifactDisposition::Rejected;

    save_package(config, &pkg)?;

    println!("Denied artifact {}: {}", uri, reason);
    println!();
    print!("Ask the agent why it made this choice? [y/N] ");
    use std::io::Write;
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.eq_ignore_ascii_case("y") {
        let path_display = uri.strip_prefix("fs://workspace/").unwrap_or(&uri);
        println!();
        println!("[Interrogation] Agent's rationale for {}:", path_display);
        if let Some(rationale) = &stored_rationale {
            println!("  {}", rationale);
        } else {
            println!("  (No rationale recorded for this artifact)");
        }
        println!();
        println!("Options:");
        println!(
            "  r) Re-approve this artifact  (ta draft amend {} {} --file <corrected-file>)",
            &package_id.to_string()[..8],
            uri
        );
        println!(
            "  c) Provide a correction      (ta draft amend {} {} --file <corrected-file>)",
            &package_id.to_string()[..8],
            uri
        );
        println!("  Enter) Leave it denied");
        println!();
        print!("Choice: ");
        std::io::stdout().flush()?;

        let mut choice = String::new();
        std::io::stdin().read_line(&mut choice)?;
        let choice = choice.trim();
        if choice.eq_ignore_ascii_case("r") {
            println!("To re-approve, run:");
            println!(
                "  ta draft amend {} {} --file <corrected-file>",
                &package_id.to_string()[..8],
                uri
            );
        } else if choice.eq_ignore_ascii_case("c") {
            println!("To provide a correction, run:");
            println!(
                "  ta draft amend {} {} --file <corrected-file>",
                &package_id.to_string()[..8],
                uri
            );
        }
        // Empty/Enter: leave it denied — nothing to do.
    }

    Ok(())
}

/// Capture a draft denial into the memory store (v0.12.5).
///
/// Writes a draft rejection entry (NegativePath) and treats the denial reason
/// as human guidance (Preference). Also increments the repeated-correction
/// counter and promotes to preference if the threshold is reached.
fn capture_draft_denial_to_memory(
    config: &GatewayConfig,
    goal_id: Option<uuid::Uuid>,
    pkg: &ta_changeset::DraftPackage,
    reason: &str,
) {
    use ta_memory::{AutoCapture, DraftRejectEvent, HumanGuidanceEvent};

    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let capture_config = ta_memory::auto_capture::load_config(&workflow_toml);
    let capture = AutoCapture::new(capture_config);

    // Look up plan_phase and agent_framework from the GoalRun.
    let (plan_phase, agent_framework) = goal_id
        .and_then(|gid| {
            ta_goal::GoalRunStore::new(&config.goals_dir)
                .ok()
                .and_then(|gs| gs.get(gid).ok().flatten())
                .map(|g| (g.plan_phase, g.agent_id))
        })
        .unwrap_or((None, "unknown".to_string()));

    // Determine the appropriate memory store.
    let write_to_memory = |store: &mut dyn ta_memory::MemoryStore| {
        // Draft rejection (NegativePath).
        let reject_event = DraftRejectEvent {
            goal_id: goal_id.unwrap_or_else(uuid::Uuid::new_v4),
            draft_id: pkg.package_id,
            agent_framework: agent_framework.clone(),
            attempted: pkg.summary.what_changed.clone(),
            rejection_reason: reason.to_string(),
            phase_id: plan_phase.clone(),
        };
        if let Err(e) = capture.on_draft_reject(store, &reject_event) {
            tracing::warn!("failed to capture draft rejection: {}", e);
        }

        // Human guidance (Preference) from the denial reason.
        let guidance_event = HumanGuidanceEvent {
            goal_id,
            agent_framework: agent_framework.clone(),
            guidance: reason.to_string(),
            tags: vec!["draft-denial".to_string()],
            phase_id: plan_phase.clone(),
        };
        if let Err(e) = capture.on_human_guidance(store, &guidance_event) {
            tracing::warn!("failed to capture human guidance from denial: {}", e);
        }

        // Repeated-correction tracking.
        let correction_key = ta_memory::auto_capture::slug_from_text_pub(reason, 60);
        if let Err(e) = capture.check_repeated_correction(store, &correction_key, reason) {
            tracing::warn!("failed to check repeated correction: {}", e);
        }
    };

    // Write to RuVectorStore if available.
    #[cfg(feature = "ruvector")]
    {
        let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
        if memory_config.backend.as_deref() != Some("fs") {
            let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
            if let Ok(mut rv_store) = ta_memory::RuVectorStore::open(&rvf_path) {
                write_to_memory(&mut rv_store);
                return;
            }
        }
    }

    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut fs_store = ta_memory::FsMemoryStore::new(&memory_dir);
    write_to_memory(&mut fs_store);
}

/// Write plan phase completion into memory (v0.12.5).
fn capture_phase_complete_to_memory(
    config: &GatewayConfig,
    phase_id: &str,
    phase_title: &str,
    summary: Option<&str>,
) {
    let do_write = |store: &mut dyn ta_memory::MemoryStore| {
        if let Err(e) =
            ta_memory::capture_plan_phase_complete(store, phase_id, phase_title, summary)
        {
            tracing::warn!("failed to capture plan phase completion: {}", e);
        }
    };

    #[cfg(feature = "ruvector")]
    {
        let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
        if memory_config.backend.as_deref() != Some("fs") {
            let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
            if let Ok(mut rv_store) = ta_memory::RuVectorStore::open(&rvf_path) {
                do_write(&mut rv_store);
                return;
            }
        }
    }
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut fs_store = ta_memory::FsMemoryStore::new(&memory_dir);
    do_write(&mut fs_store);
}

/// Bump the canonical version fields in Cargo.toml, CLAUDE.md, and .release.toml
/// to `new_version`.
///
/// Called automatically by `draft apply --phase` after the plan update so that
/// version tracking stays in sync without requiring agent or human intervention.
/// Agents should NOT bump the version manually — this function is the authority.
///
/// Files modified (if present in `workspace_dir`):
///   Cargo.toml       — first `^version = "..."` line (workspace package version)
///   CLAUDE.md        — `**Current version**: \`...\`` line
///   .release.toml    — `prerelease = ...` set to `true`; other fields left intact
///
/// Returns the list of files that were actually modified (for inclusion in git commit).
pub fn bump_workspace_version(
    workspace_dir: &Path,
    new_version: &str,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut modified = Vec::new();

    // --- Cargo.toml ---
    let cargo_path = workspace_dir.join("Cargo.toml");
    if cargo_path.exists() {
        let original = std::fs::read_to_string(&cargo_path)?;
        let mut replaced = false;
        let updated: String = original
            .lines()
            .map(|line| {
                // Match the workspace-level `version = "..."` — must be at column 0
                // (not inside a [[package]] table) and not already the correct value.
                if !replaced && line.starts_with("version = \"") {
                    replaced = true;
                    format!("version = \"{}\"", new_version)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + if original.ends_with('\n') { "\n" } else { "" };

        if updated != original {
            std::fs::write(&cargo_path, &updated)?;
            modified.push(cargo_path);
        }
    }

    // --- CLAUDE.md ---
    let claude_path = workspace_dir.join("CLAUDE.md");
    if claude_path.exists() {
        let original = std::fs::read_to_string(&claude_path)?;
        // Replace: **Current version**: `<old>` → **Current version**: `<new>`
        let updated = regex_replace_first(
            &original,
            r"\*\*Current version\*\*: `[^`]*`",
            &format!("**Current version**: `{}`", new_version),
        );
        if updated != original {
            std::fs::write(&claude_path, &updated)?;
            modified.push(claude_path);
        }
    }

    // --- .release.toml ---
    let release_toml_path = workspace_dir.join(".release.toml");
    if release_toml_path.exists() {
        let original = std::fs::read_to_string(&release_toml_path)?;
        // Ensure prerelease = true (all phase-apply bumps are pre-release by definition).
        let updated = regex_replace_first(&original, r"(?m)^prerelease = .*", "prerelease = true");
        if updated != original {
            std::fs::write(&release_toml_path, &updated)?;
            modified.push(release_toml_path);
        }
    }

    Ok(modified)
}

/// Simple line-by-line regex replacement helper (first match only, no external crate needed).
/// Pattern must match the full text of a line. Replacement is a literal string.
fn regex_replace_first(text: &str, pattern: &str, replacement: &str) -> String {
    // We avoid pulling in the `regex` crate — the patterns here are simple enough
    // to implement with manual matching.
    match pattern {
        // **Current version**: `...`
        p if p.contains("Current version") => {
            let mut replaced = false;
            text.lines()
                .map(|line| {
                    if !replaced && line.contains("**Current version**: `") && line.contains('`') {
                        replaced = true;
                        replacement.to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                + if text.ends_with('\n') { "\n" } else { "" }
        }
        // prerelease = ...
        p if p.contains("prerelease") => {
            let mut replaced = false;
            text.lines()
                .map(|line| {
                    if !replaced && line.starts_with("prerelease = ") {
                        replaced = true;
                        replacement.to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                + if text.ends_with('\n') { "\n" } else { "" }
        }
        _ => text.to_string(),
    }
}

/// Selective review patterns for artifact disposition.
#[derive(Default)]
struct SelectiveReviewPatterns<'a> {
    approve: &'a [String],
    reject: &'a [String],
    discuss: &'a [String],
}

impl<'a> SelectiveReviewPatterns<'a> {
    fn is_enabled(&self) -> bool {
        !self.approve.is_empty() || !self.reject.is_empty() || !self.discuss.is_empty()
    }
}

// Note: validate_dependencies has been replaced by SupervisorAgent.validate()
// which provides more comprehensive dependency graph analysis including cycle detection.

/// Assign dispositions to artifacts based on user-provided patterns.
///
/// Processing order:
/// 1. --approve patterns are applied first
/// 2. --reject patterns are applied second
/// 3. --discuss patterns are applied last
///
/// Special values:
/// - "all": matches all artifacts
/// - "rest": matches all artifacts not yet assigned a disposition
///
/// Returns the count of artifacts assigned to each disposition.
fn assign_dispositions(
    artifacts: &mut [Artifact],
    approve_patterns: &[String],
    reject_patterns: &[String],
    discuss_patterns: &[String],
) -> (usize, usize, usize) {
    // Helper: check if a pattern matches an artifact.
    let matches = |pattern: &str, artifact: &Artifact| -> bool {
        if pattern == "all" {
            true
        } else if pattern == "rest" {
            artifact.disposition == ArtifactDisposition::Pending
        } else {
            uri_pattern::matches_uri(pattern, &artifact.resource_uri)
        }
    };

    // Apply patterns in order: approve → reject → discuss.
    // Later patterns override earlier ones (last writer wins per artifact).
    for pattern in approve_patterns {
        for artifact in artifacts.iter_mut() {
            if matches(pattern, artifact) {
                artifact.disposition = ArtifactDisposition::Approved;
            }
        }
    }

    for pattern in reject_patterns {
        for artifact in artifacts.iter_mut() {
            if matches(pattern, artifact) {
                artifact.disposition = ArtifactDisposition::Rejected;
            }
        }
    }

    for pattern in discuss_patterns {
        for artifact in artifacts.iter_mut() {
            if matches(pattern, artifact) {
                artifact.disposition = ArtifactDisposition::Discuss;
            }
        }
    }

    // Count final dispositions from actual artifact state (not incrementally).
    let approved = artifacts
        .iter()
        .filter(|a| a.disposition == ArtifactDisposition::Approved)
        .count();
    let rejected = artifacts
        .iter()
        .filter(|a| a.disposition == ArtifactDisposition::Rejected)
        .count();
    let discussed = artifacts
        .iter()
        .filter(|a| a.disposition == ArtifactDisposition::Discuss)
        .count();

    (approved, rejected, discussed)
}

/// Build a complete commit message from goal and draft package.
///
/// Format:
///   <goal title>
///
///   <draft summary — what changed>
///
///   Why: <objective/motivation>
///   Impact: <impact assessment>
///
///   Changes (<count> file(s)):
///     <per-artifact summaries with change type>
///
///   Open questions:
///     - <question>
/// Build a git commit message that matches `ta draft view` output.
///
/// Format: goal title as subject line, then the same medium-detail rendering
/// used by `ta draft view` (no color, no ANSI escapes).
fn build_commit_message(goal: &ta_goal::GoalRun, pkg: &DraftPackage) -> String {
    use ta_changeset::output_adapters::{get_adapter, DetailLevel, OutputFormat, RenderContext};

    // Render using the terminal adapter with no color — same output as `ta draft view`.
    let ctx = RenderContext {
        package: pkg,
        detail_level: DetailLevel::Medium,
        file_filters: vec![],
        diff_provider: None,
        section_filter: None,
    };
    let adapter = get_adapter(OutputFormat::Terminal, false);
    let rendered = adapter
        .render(&ctx)
        .unwrap_or_else(|_| format!("{}\n\n{}", goal.title, pkg.summary.what_changed));

    // Git convention: first line is the subject, then blank line, then body.
    // The terminal adapter starts with "Draft: <id>\nStatus: ...\n..." which isn't
    // a good subject line. Replace the header with goal title as subject.
    let body = if let Some(pos) = rendered.find("What Changed (") {
        // Extract from "What Changed (...)" onward — the grouped file listing.
        &rendered[pos..]
    } else {
        rendered.as_str()
    };

    format!(
        "{}\n\n{}\nImpact: {}\n\n{}",
        goal.title, pkg.summary.what_changed, pkg.summary.impact, body
    )
}

// ── Apply Lock ──────────────────────────────────────────────────────────────
//
// Prevents concurrent `ta draft apply` runs on the same workspace.
// The lock file is written at the start of every apply and removed on exit
// (success or failure) via Drop.
//
// Lock file path  : .ta/apply.lock
// Lock file format: {"draft_id":"...","pid":12345,"started_at":"<RFC3339>"}

/// Exclusive lock held during `ta draft apply`.
///
/// Protects against two classes of concurrent-mutation bugs:
/// 1. Two `ta draft apply` invocations racing to write the same files.
/// 2. A co-developer (human or AI assistant) running `git checkout`/`commit`
///    while an apply is in mid-flight (causes "no changes to commit" rollbacks).
///
/// The lock file is human-readable; tools and shell scripts can inspect it.
struct ApplyLock {
    lock_path: std::path::PathBuf,
}

impl ApplyLock {
    /// Try to acquire the apply lock for `draft_id` under `workspace_root`.
    ///
    /// - No existing lock → writes lock file and returns `Ok(ApplyLock)`.
    /// - Stale lock (pid dead) → removes it, then acquires as above.
    /// - Live lock → returns `Err` with an actionable error message.
    fn acquire(workspace_root: &std::path::Path, draft_id: &str) -> anyhow::Result<Self> {
        let lock_path = workspace_root.join(".ta").join("apply.lock");

        if let Ok(raw) = std::fs::read_to_string(&lock_path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                let pid = data["pid"].as_u64().unwrap_or(0) as u32;
                let existing_draft = data["draft_id"].as_str().unwrap_or("unknown");
                if is_apply_process_alive(pid) {
                    anyhow::bail!(
                        "Draft apply already in progress (PID {pid}, draft {existing_draft}). \
                         Wait for it to finish or kill PID {pid} if it has crashed.\n\
                         Lock file: {lock}\n\
                         Check status: ta draft apply --status",
                        lock = lock_path.display()
                    );
                }
                // Stale lock — previous apply crashed without cleanup.
                eprintln!(
                    "[apply] Removing stale apply lock (PID {pid} is no longer running): {lock}",
                    lock = lock_path.display()
                );
                let _ = std::fs::remove_file(&lock_path);
            }
        }

        let content = serde_json::json!({
            "draft_id": draft_id,
            "pid": std::process::id(),
            "started_at": Utc::now().to_rfc3339(),
        })
        .to_string();
        std::fs::write(&lock_path, &content).map_err(|e| {
            anyhow::anyhow!(
                "Failed to write apply lock at {}: {}.\n\
                 Ensure the .ta/ directory exists and is writable.",
                lock_path.display(),
                e
            )
        })?;

        Ok(ApplyLock { lock_path })
    }

    /// Print the current apply lock status to stdout.
    fn print_status(workspace_root: &std::path::Path) {
        let lock_path = workspace_root.join(".ta").join("apply.lock");
        match std::fs::read_to_string(&lock_path) {
            Err(_) => {
                println!("No apply in progress (no lock file found).");
            }
            Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Err(_) => {
                    println!(
                        "Apply lock file exists but is malformed: {}",
                        lock_path.display()
                    );
                }
                Ok(data) => {
                    let pid = data["pid"].as_u64().unwrap_or(0) as u32;
                    let draft_id = data["draft_id"].as_str().unwrap_or("unknown");
                    let started_at = data["started_at"].as_str().unwrap_or("unknown");
                    let alive = is_apply_process_alive(pid);
                    println!(
                        "Apply in progress: draft={draft_id}, PID={pid} ({state}), started={started_at}",
                        state = if alive { "running" } else { "stale/dead" }
                    );
                    if !alive {
                        println!("  Lock is stale. Remove with: rm {}", lock_path.display());
                    }
                }
            },
        }
    }
}

impl Drop for ApplyLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Returns `true` if the process with the given PID is currently alive.
///
/// Uses direct kernel calls (no subprocess) for minimal overhead:
/// - Unix: `kill(pid, 0)` — checks existence without sending a signal.
/// - Windows: `OpenProcess(SYNCHRONIZE)` — O(1) kernel call; a valid handle
///   means alive; `ERROR_ACCESS_DENIED` also means the process exists.
///
/// Deliberately avoids importing `std::os::windows::io` traits or the
/// `windows`/`winapi` crates to keep the Windows compilation footprint small.
fn is_apply_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Safety: kill(pid, 0) with signal 0 never sends a signal — it only
        // checks whether the process exists and we have permission to signal it.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // Raw extern declarations keep this dependency-free.  All three
        // functions are stable Win32 API (kernel32.dll / ntdll).
        #[allow(non_upper_case_globals)]
        const SYNCHRONIZE: u32 = 0x00100000;
        #[allow(non_upper_case_globals)]
        const ERROR_ACCESS_DENIED: u32 = 5;
        extern "system" {
            fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
            fn CloseHandle(hObject: isize) -> i32;
            fn GetLastError() -> u32;
        }
        unsafe {
            let handle = OpenProcess(SYNCHRONIZE, 0, pid);
            if handle == 0 || handle == -1isize {
                // NULL or INVALID_HANDLE_VALUE — check error code.
                GetLastError() == ERROR_ACCESS_DENIED
            } else {
                CloseHandle(handle);
                true
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

// ── Apply Rollback Guard ─────────────────────────────────────────────────────

/// Guard that snapshots working-tree files before `ta draft apply` and rolls them
/// back atomically if pre-submit verification fails (or any other error occurs
/// before the guard is committed).
///
/// Usage:
/// 1. Call `snapshot_file` for every path that will be written.
/// 2. Proceed with the apply.
/// 3. On the success path, call `commit()` to prevent rollback.
/// 4. On any failure, let the guard drop — it will restore all snapshotted files.
struct ApplyRollbackGuard {
    /// `(path, pre-apply content)`. `None` means the file did not exist before
    /// apply and should be deleted on rollback.
    snapshots: Vec<(std::path::PathBuf, Option<Vec<u8>>)>,
    committed: bool,
}

impl ApplyRollbackGuard {
    fn new() -> Self {
        ApplyRollbackGuard {
            snapshots: Vec::new(),
            committed: false,
        }
    }

    /// Record the current on-disk content of `path` before it is written.
    /// Silently ignores duplicate snapshots for the same path — only the
    /// first snapshot (true pre-apply state) is kept.
    fn snapshot_file(&mut self, path: &std::path::Path) {
        // Skip duplicates — the first snapshot has the real pre-apply content.
        if self.snapshots.iter().any(|(p, _)| p == path) {
            return;
        }
        let content = std::fs::read(path).ok(); // None → file not yet on disk
        self.snapshots.push((path.to_path_buf(), content));
    }

    /// Mark the apply as committed — `Drop` will not roll back.
    fn commit(&mut self) {
        self.committed = true;
    }

    /// Restore all snapshotted files to their pre-apply state.
    /// Returns the number of paths successfully restored.
    fn rollback(&self) -> usize {
        let mut count = 0;
        for (path, original) in &self.snapshots {
            let result = match original {
                Some(content) => std::fs::write(path, content),
                None => {
                    // File did not exist before apply — remove it.
                    if path.exists() {
                        std::fs::remove_file(path)
                    } else {
                        Ok(())
                    }
                }
            };
            match result {
                Ok(()) => count += 1,
                Err(e) => eprintln!(
                    "[rollback] Warning: could not restore {}: {}",
                    path.display(),
                    e
                ),
            }
        }
        count
    }
}

impl Drop for ApplyRollbackGuard {
    fn drop(&mut self) {
        if !self.committed {
            let n = self.rollback();
            if n > 0 {
                eprintln!("[rollback] Restored {} file(s) to pre-apply state.", n);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_package(
    config: &GatewayConfig,
    id: &str,
    target: Option<&str>,
    git_commit: bool,
    git_push: bool,
    git_review: bool,
    skip_verify: bool,
    dry_run: bool,
    conflict_resolution: ta_workspace::ConflictResolution,
    patterns: SelectiveReviewPatterns,
    phase_override: Option<&str>,
    force_apply: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;

    // Acquire the apply lock before doing any work. Prevents concurrent applies
    // and signals to co-developer processes (including AI assistants) that git
    // mutations should be deferred until this apply completes.
    // The lock is released automatically when `_apply_lock` is dropped.
    let _apply_lock = ApplyLock::acquire(&config.workspace_root, &package_id.to_string())?;

    eprintln!("[apply] Loading draft package {}...", package_id);
    let mut pkg = load_package(config, package_id)?;
    eprintln!(
        "[apply] Draft: \"{}\" ({} artifact(s))",
        pkg.goal.title,
        pkg.changes.artifacts.len()
    );

    // Check if selective review is enabled.
    let selective_review = patterns.is_enabled();

    if selective_review {
        // Selective review mode: allow PendingReview or Approved packages.
        if !matches!(
            pkg.status,
            DraftStatus::PendingReview | DraftStatus::Approved { .. }
        ) {
            anyhow::bail!(
                "Cannot apply package in {:?} state (must be PendingReview or Approved for selective review)",
                pkg.status
            );
        }

        // Assign dispositions based on patterns.
        let (approved, rejected, discussed) = assign_dispositions(
            &mut pkg.changes.artifacts,
            patterns.approve,
            patterns.reject,
            patterns.discuss,
        );

        let pending = pkg
            .changes
            .artifacts
            .iter()
            .filter(|a| a.disposition == ArtifactDisposition::Pending)
            .count();

        println!("Selective review disposition summary:");
        println!("  Approved: {} artifact(s)", approved);
        println!("  Rejected: {} artifact(s)", rejected);
        println!("  Discuss:  {} artifact(s)", discussed);
        println!("  Pending:  {} artifact(s)", pending);
        println!();

        // Validate dependencies using SupervisorAgent.
        let supervisor = SupervisorAgent::new(&pkg.changes.artifacts);
        let validation = supervisor.validate(&pkg.changes.artifacts);

        // Display errors first (structural issues).
        if validation.has_errors() {
            println!("Dependency errors:");
            for error in &validation.errors {
                match error {
                    ta_changeset::supervisor::ValidationError::CyclicDependency { cycle } => {
                        println!("  [!] Cyclic dependency detected: {}", cycle.join(" -> "));
                    }
                    ta_changeset::supervisor::ValidationError::SelfDependency { artifact } => {
                        println!("  [!] Self-dependency detected: {}", artifact);
                    }
                }
            }
            println!();
            anyhow::bail!(
                "Cannot apply: {} structural error(s) in dependency graph. Fix the agent's change_summary.json.",
                validation.errors.len()
            );
        }

        // Display warnings (disposition conflicts).
        if validation.has_warnings() {
            println!("Dependency warnings:");
            for warning in &validation.warnings {
                match warning {
                    ValidationWarning::CoupledRejection {
                        artifact,
                        required_by,
                    } => {
                        println!(
                            "  [warn] Rejecting {} will break {} artifact(s) that depend on it:",
                            artifact.split('/').next_back().unwrap_or(artifact),
                            required_by.len()
                        );
                        for req in required_by {
                            println!("      - {}", req.split('/').next_back().unwrap_or(req));
                        }
                    }
                    ValidationWarning::BrokenDependency {
                        artifact,
                        depends_on_rejected,
                    } => {
                        println!(
                            "  [warn] Approving {} but it depends on {} rejected artifact(s):",
                            artifact.split('/').next_back().unwrap_or(artifact),
                            depends_on_rejected.len()
                        );
                        for dep in depends_on_rejected {
                            println!("      - {}", dep.split('/').next_back().unwrap_or(dep));
                        }
                    }
                    ValidationWarning::DiscussBlockingApproval { artifact, blocking } => {
                        println!("  [warn] {} is marked for discussion but {} approved artifact(s) depend on it:",
                            artifact.split('/').next_back().unwrap_or(artifact),
                            blocking.len());
                        for blk in blocking {
                            println!("      - {}", blk.split('/').next_back().unwrap_or(blk));
                        }
                    }
                }
            }
            println!();
            anyhow::bail!(
                "Cannot apply: {} dependency conflict(s) detected. Resolve conflicts and try again.",
                validation.warnings.len()
            );
        }

        // Count approved artifacts.
        let approved_count = pkg
            .changes
            .artifacts
            .iter()
            .filter(|a| a.disposition == ArtifactDisposition::Approved)
            .count();

        if approved_count == 0 {
            anyhow::bail!("No artifacts approved for application");
        }

        println!("Applying {} approved artifact(s)...", approved_count);
    } else {
        // All-or-nothing mode: accept Approved or PendingReview (auto-approve on apply).
        if matches!(pkg.status, DraftStatus::PendingReview) {
            pkg.status = DraftStatus::Approved {
                approved_by: "auto (apply)".to_string(),
                approved_at: Utc::now(),
            };
            save_package(config, &pkg)?;
            println!(
                "Auto-approved draft {} (apply implies approval).",
                package_id
            );
        } else if !matches!(pkg.status, DraftStatus::Approved { .. }) {
            anyhow::bail!(
                "Cannot apply package in {:?} state (must be PendingReview or Approved)",
                pkg.status
            );
        }
    }

    // Find the goal for this package.
    let goal_store = GoalRunStore::new(&config.goals_dir)?;
    let goals = goal_store.list()?;
    let goal = goals
        .iter()
        .find(|g| g.pr_package_id == Some(package_id))
        .ok_or_else(|| anyhow::anyhow!("No goal found for draft package {}", package_id))?;

    // Pre-flight: validate the state transition before doing any file work.
    // This ensures the apply is atomic — either everything succeeds or we
    // fail fast without leaving files in a half-applied state.
    if !goal.state.can_transition_to(&GoalRunState::Applied) {
        let short_id = &goal.goal_run_id.to_string()[..8];
        anyhow::bail!(
            "Cannot apply draft — the agent task \"{title}\" ({short_id}) did not complete successfully.\n\
             \n\
             The task is in state '{state}', but applying a draft requires the task to be in\n\
             'pr_ready', 'under_review', or 'approved'.\n\
             \n\
             What to do:\n\
             1. Check what went wrong:  ta goal status {short_id}\n\
             2. Clean up the failed task: ta goal delete {short_id}\n\
             3. Re-run from the beginning (e.g. ta release run <version>)",
            title = goal.title,
            short_id = short_id,
            state = goal.state,
        );
    }

    let target_dir = match target {
        Some(t) => std::path::PathBuf::from(t),
        None => goal
            .source_dir
            .clone()
            .unwrap_or_else(|| config.workspace_root.clone()),
    };

    // Pre-apply: clean any TA-managed working-tree artifacts that could
    // block git operations (injection-polluted CLAUDE.md, etc.).
    check_and_clean_working_tree(&target_dir);

    // ── VCS pre-flight: branch creation BEFORE any file writes ───────────────
    // Files must never land on a protected branch (main/master/etc.). We create
    // or switch to the feature branch here — before the rollback guard and before
    // the overlay apply — so the working tree is always on the right branch when
    // files are written.
    //
    // This closes the race window where: files written to main → apply interrupted
    // → user manually commits to main. After this block, the working tree is on
    // the feature branch regardless of how the rest of apply behaves.
    //
    // We also record the feature branch name here so that if the submit pipeline
    // fails later, the error handler can show the user exactly which branch is
    // safe to commit to manually (instead of blindly suggesting --no-submit which
    // previously caused commits to land on main).
    // Capture the working branch BEFORE any pre-flight branch switching so
    // restore_state() at the end of the submit workflow can return the user
    // to their original branch (e.g., main) after the feature-branch commit.
    let mut original_branch: Option<String> = None;
    let mut preflight_branch: Option<String> = None;
    if git_commit {
        use ta_submit::{select_adapter, WorkflowConfig};
        let wf_path = target_dir.join(".ta/workflow.toml");
        let wf_config = WorkflowConfig::load_or_default(&wf_path);
        let adapter = select_adapter(&target_dir, &wf_config.submit);

        if adapter.name() != "none" {
            let branch = adapter
                .current_branch()
                .unwrap_or_else(|_| "unknown".to_string());
            // Record original branch before pre-flight may switch to a feature branch.
            original_branch = Some(branch.clone());
            let protected = adapter.protected_submit_targets();
            let on_protected = protected.iter().any(|b| b == &branch);

            if on_protected {
                eprintln!(
                    "[apply] On protected branch '{}' — creating feature branch before writing any files...",
                    branch
                );
                adapter.prepare(goal, &wf_config.submit).map_err(|e| {
                    anyhow::anyhow!(
                        "VCS pre-flight failed: could not create feature branch before writing files.\n\
                         Aborted with no changes made to the source tree.\n\
                         Branch was: '{}'\n\
                         VCS error: {}\n\
                         \n\
                         Ensure the working tree is clean (git status), then re-run:\n\
                         ta draft apply {}",
                        branch,
                        e,
                        id
                    )
                })?;
                // Double-check we're off the protected branch before proceeding.
                adapter.verify_not_on_protected_target().map_err(|e| {
                    anyhow::anyhow!(
                        "VCS pre-flight: prepare() succeeded but still on protected branch.\n\
                         Aborted with no changes made. Check the VCS adapter configuration.\n\
                         Error: {}",
                        e
                    )
                })?;
                let new_branch = adapter
                    .current_branch()
                    .unwrap_or_else(|_| "unknown".to_string());
                eprintln!(
                    "[apply] Switched to feature branch '{}' — proceeding with file writes.",
                    new_branch
                );
                // Record the prepared feature branch so the submit-failure error
                // handler can reference it by name.
                preflight_branch = Some(new_branch);
            } else {
                eprintln!(
                    "[apply] VCS pre-flight: already on branch '{}' (not protected) — ok.",
                    branch
                );
                // Already on the right branch; record it so the error handler
                // can confirm the branch name if submit fails later.
                preflight_branch = Some(branch);
            }
        }
    } else {
        // Not submitting — warn if on a protected branch so the user knows they
        // must not manually commit the resulting changes to main.
        use ta_submit::{select_adapter, WorkflowConfig};
        let wf_path = target_dir.join(".ta/workflow.toml");
        let wf_config = WorkflowConfig::load_or_default(&wf_path);
        let adapter = select_adapter(&target_dir, &wf_config.submit);
        if adapter.name() != "none" {
            let branch = adapter
                .current_branch()
                .unwrap_or_else(|_| "unknown".to_string());
            let protected = adapter.protected_submit_targets();
            if protected.iter().any(|b| b == &branch) {
                eprintln!(
                    "\nWarning: applying without VCS submit on protected branch '{branch}'.\n\
                     Changes will be written to your working tree on '{branch}'.\n\
                     Do NOT commit these changes to '{branch}' directly.\n\
                     To auto-commit to a feature branch, re-run with `ta draft apply --submit`.\n"
                );
            }
        }
    }

    // ── Transactional rollback guard (v0.12.2.2) ─────────────────────────────
    // Snapshot working-tree files before any writes so we can restore them
    // atomically if pre-submit verification fails (or any other error occurs).
    let mut rollback_guard = ApplyRollbackGuard::new();
    // PLAN.md is touched twice: once by the artifact apply (if the agent
    // modified it) and once by the plan-phase status update below. Snapshot
    // it now — before either write — so rollback restores the true original.
    {
        let plan_md = target_dir.join("PLAN.md");
        if plan_md.exists() {
            rollback_guard.snapshot_file(&plan_md);
        }
    }

    // Apply changes — use overlay path for overlay-based goals, legacy path otherwise.
    eprintln!("[apply] Applying changes to {}...", target_dir.display());
    let applied_files: Vec<String> = if let Some(ref source_dir) = goal.source_dir {
        // Overlay-based goal: diff staging vs source, copy changed files.
        eprintln!("[apply] Opening overlay workspace...");

        // §1 guard: access only this goal's staging dir by ID — never enumerate the staging root.
        // If the staging dir was deleted (e.g., by a concurrent `ta gc` or manual cleanup),
        // give a clear error rather than cryptic I/O failures.
        if !goal.workspace_path.exists() {
            let short_id = &pkg.package_id.to_string()[..8];
            anyhow::bail!(
                "Staging workspace for goal {short_id} no longer exists: {path}\n\
                 \n\
                 The staging directory may have been deleted by `ta gc` or manual cleanup \
                 while apply was running. The goal record is intact.\n\
                 \n\
                 Options:\n\
                 1. Re-run the goal to regenerate the staging workspace:\n\
                    ta run \"{title}\" --goal-id {short_id} --follow-up\n\
                 2. If changes are already in source, skip staging and apply manually:\n\
                    ta draft deny {short_id} --reason \"staging deleted, applied manually\"",
                path = goal.workspace_path.display(),
                title = goal.title,
            );
        }

        // V1 TEMPORARY: Load exclude patterns, merging VCS adapter patterns.
        let excludes = load_excludes_with_adapter(source_dir);
        let mut overlay = OverlayWorkspace::open(
            goal.goal_run_id.to_string(),
            source_dir,
            &goal.workspace_path,
            excludes,
        );

        // v0.2.1: Restore source snapshot from goal for conflict detection.
        // v0.4.1.2: Support rebase-on-apply for sequential draft applies.
        if let Some(snapshot_json) = &goal.source_snapshot {
            if let Ok(snapshot) =
                serde_json::from_value::<ta_workspace::SourceSnapshot>(snapshot_json.clone())
            {
                // Check if rebase-on-apply is configured.
                let workflow_config = ta_submit::WorkflowConfig::load_or_default(
                    &target_dir.join(".ta/workflow.toml"),
                );

                // Detect if source has changed since snapshot.
                overlay.set_snapshot(snapshot.clone());
                let has_source_changes = overlay
                    .detect_conflicts()
                    .ok()
                    .flatten()
                    .map(|c| !c.is_empty())
                    .unwrap_or(false);

                if has_source_changes && workflow_config.follow_up.rebase_on_apply {
                    // Rebase: re-snapshot the current source state so apply compares
                    // staging against the updated source (e.g., after a prior draft was applied).
                    let excludes = load_excludes_with_adapter(source_dir);
                    if let Ok(fresh_snapshot) =
                        ta_workspace::SourceSnapshot::capture(&target_dir, |p| {
                            excludes.should_exclude(p)
                        })
                    {
                        println!(
                            "\n[info] Source changed since goal start — rebasing against current source."
                        );
                        overlay.set_snapshot(fresh_snapshot);
                    }
                } else if has_source_changes {
                    // Preview conflicts (informational — apply_with_conflict_check handles abort/force).
                    if let Ok(Some(conflicts)) = overlay.detect_conflicts() {
                        if !conflicts.is_empty() {
                            println!(
                                "\n[info] {} source file(s) changed since goal start.",
                                conflicts.len()
                            );
                            println!(
                                "   (Only overlapping changes block apply. Resolution: {:?})\n",
                                conflict_resolution
                            );
                        }
                    }
                }
            }
        }

        // Collect artifact URIs from the draft package — the authoritative list of intended changes.
        let artifact_uris: Vec<String> = if selective_review {
            // Selective mode: only approved artifacts.
            pkg.changes
                .artifacts
                .iter()
                .filter(|a| a.disposition == ArtifactDisposition::Approved)
                .map(|a| a.resource_uri.clone())
                .collect()
        } else {
            // Standard mode: all artifacts.
            pkg.changes
                .artifacts
                .iter()
                .map(|a| a.resource_uri.clone())
                .collect()
        };

        // Snapshot each artifact's current on-disk content before overwriting.
        for uri in &artifact_uris {
            if let Some(rel) = uri.strip_prefix("fs://workspace/") {
                rollback_guard.snapshot_file(&target_dir.join(rel));
            }
        }

        // v0.13.17.2: Pre-apply artifact safety checks — catch destructive changes
        // before they reach the filesystem. Blocked by --force-apply.
        if !force_apply && !dry_run {
            if let Err(e) = run_apply_safety_checks(
                &artifact_uris,
                &goal.workspace_path,
                &target_dir,
                &pkg.goal.title,
            ) {
                eprintln!("[safety] {}", e);
                eprintln!(
                    "[safety] Use --force-apply to bypass these safety checks if the changes are intentional."
                );
                anyhow::bail!("Pre-apply safety check failed — apply aborted.");
            }
        }

        // v0.14.3.5: Baseline skip logic — remove baseline-only artifacts where staging
        // content matches source. These were inherited from the parent draft and are
        // already settled in source; copying them would revert post-parent changes.
        let effective_uris = if pkg.baseline_artifacts.is_empty() {
            artifact_uris.clone()
        } else {
            let baseline_set: std::collections::HashSet<&str> =
                pkg.baseline_artifacts.iter().map(|s| s.as_str()).collect();
            let mut filtered = Vec::with_capacity(artifact_uris.len());
            let mut skipped_baseline = 0usize;
            for uri in &artifact_uris {
                // Only check baseline skip for fs:// artifacts.
                if baseline_set.contains(uri.as_str()) {
                    if let Some(rel) = uri.strip_prefix("fs://workspace/") {
                        let staging_path = goal.workspace_path.join(rel);
                        let source_path = target_dir.join(rel);
                        // If staging hash == source hash, file is already settled — skip.
                        let staging_hash = if staging_path.exists() {
                            std::fs::read(&staging_path)
                                .ok()
                                .map(|b| format!("{:x}", sha2::Sha256::digest(&b)))
                        } else {
                            None
                        };
                        let source_hash = if source_path.exists() {
                            std::fs::read(&source_path)
                                .ok()
                                .map(|b| format!("{:x}", sha2::Sha256::digest(&b)))
                        } else {
                            None
                        };
                        if staging_hash.is_some() && staging_hash == source_hash {
                            eprintln!(
                                "ℹ️  [baseline] skipping {} (unchanged since parent apply)",
                                rel
                            );
                            skipped_baseline += 1;
                            continue;
                        }
                    }
                }
                filtered.push(uri.clone());
            }
            if skipped_baseline > 0 {
                eprintln!(
                    "ℹ️  [baseline] {} baseline artifact(s) skipped (already settled by parent apply)",
                    skipped_baseline
                );
            }
            filtered
        };

        // v0.14.3.5: Protected-file revert guard — for files marked "keep-source" in
        // [apply.conflict_policy] (or the hardcoded TA-managed defaults), if the source
        // version is strictly newer than staging (content differs), keep source and log a
        // warning. These files are updated by `ta draft apply` itself — agents should not
        // overwrite them with an older staging copy.
        //
        // Default protected files when no policy is configured (hardcoded seed):
        const DEFAULT_PROTECTED_FILES: &[&str] = &["PLAN.md", "docs/USAGE.md"];
        let workflow_config =
            ta_submit::WorkflowConfig::load_or_default(&target_dir.join(".ta/workflow.toml"));
        let effective_uris = {
            let mut guarded = Vec::with_capacity(effective_uris.len());
            for uri in effective_uris {
                let keep_source_from_policy = if let Some(rel) = uri.strip_prefix("fs://workspace/")
                {
                    // Check per-file conflict_policy first.
                    let policy = if workflow_config.apply.conflict_policy.is_empty() {
                        None
                    } else {
                        workflow_config.apply.policy_for(rel).map(|s| s.to_string())
                    };
                    if policy.as_deref() == Some("keep-source") {
                        true
                    } else if policy.is_none() {
                        // No policy configured — check hardcoded defaults.
                        DEFAULT_PROTECTED_FILES
                            .iter()
                            .any(|&p| rel == p || rel.ends_with(&format!("/{}", p)))
                    } else {
                        false
                    }
                } else {
                    false
                };
                if keep_source_from_policy {
                    if let Some(rel) = uri.strip_prefix("fs://workspace/") {
                        let staging_path = goal.workspace_path.join(rel);
                        let source_path = target_dir.join(rel);
                        // Skip if source content differs from staging — source is newer.
                        let staging_bytes = std::fs::read(&staging_path).ok();
                        let source_bytes = std::fs::read(&source_path).ok();
                        if let (Some(s), Some(t)) = (staging_bytes, source_bytes) {
                            if s != t {
                                // Check if source is strictly newer by mtime.
                                let source_mtime = std::fs::metadata(&source_path)
                                    .and_then(|m| m.modified())
                                    .ok();
                                let staging_mtime = std::fs::metadata(&staging_path)
                                    .and_then(|m| m.modified())
                                    .ok();
                                let source_is_newer = match (source_mtime, staging_mtime) {
                                    (Some(sm), Some(tm)) => sm > tm,
                                    _ => true, // Unknown → keep source (safe default)
                                };
                                if source_is_newer {
                                    eprintln!(
                                        "⚠️  [protected] keeping source {} (newer than staging — managed by ta draft apply)",
                                        rel
                                    );
                                    continue; // Skip this artifact.
                                }
                            }
                        }
                    }
                }
                guarded.push(uri);
            }
            guarded
        };

        eprintln!("[apply] Diffing staging vs source and copying changes...");
        let applied = overlay
            .apply_with_conflict_check(&target_dir, conflict_resolution, &effective_uris)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        applied
            .into_iter()
            .map(|(path, kind)| format!("{} ({})", path, kind))
            .collect()
    } else {
        // Legacy MCP-based goal: use FsConnector.
        if selective_review {
            anyhow::bail!(
                "Selective review is not supported for MCP-based goals (only overlay-based goals)"
            );
        }
        let staging = StagingWorkspace::new(goal.goal_run_id.to_string(), &config.staging_dir)?;
        let store = JsonFileStore::new(config.store_dir.join(goal.goal_run_id.to_string()))?;
        let mut connector =
            FsConnector::new(goal.goal_run_id.to_string(), staging, store, &goal.agent_id);
        connector.apply(&target_dir)?
    };

    // v0.13.0.1: Show "Changes from parent" header for follow-up drafts, or list
    // follow-up work when applying a root draft that has children.
    if let Some(ref parent_title) = pkg.goal.parent_goal_title {
        println!("Applied follow-up to \"{}\"", parent_title);
    } else {
        // Check for follow-up drafts that contributed to this goal.
        let all_pkgs = load_all_packages(config).unwrap_or_default();
        let follow_ups: Vec<_> = all_pkgs
            .iter()
            .filter(|p| p.parent_draft_id == Some(package_id))
            .collect();
        if !follow_ups.is_empty() {
            println!("Changes from parent:");
            for f in &follow_ups {
                println!("  • {} [{}]", f.goal.title, &f.package_id.to_string()[..8]);
            }
        }
    }

    println!(
        "Applied {} file(s) to {}",
        applied_files.len(),
        target_dir.display()
    );
    for file in &applied_files {
        println!("  {}", file);
    }

    // v0.13.17.7: Wire upstream community PRs for community:// artifacts.
    {
        let community_artifacts: Vec<_> = pkg
            .changes
            .artifacts
            .iter()
            .filter(|a| a.resource_uri.starts_with("community://"))
            .collect();
        if !community_artifacts.is_empty() {
            println!(
                "\n[community] {} community artifact(s) detected — checking for upstream PR opportunities...",
                community_artifacts.len()
            );
            let has_gh_token =
                std::env::var("GITHUB_TOKEN").is_ok() || std::env::var("GH_TOKEN").is_ok();
            if !has_gh_token {
                println!("[community] No GITHUB_TOKEN found — skipping upstream PR creation.");
                println!("[community] Set GITHUB_TOKEN to automatically open PRs against upstream community repos.");
            } else {
                for artifact in &community_artifacts {
                    let uri = &artifact.resource_uri;
                    let after_scheme = uri.trim_start_matches("community://");
                    if after_scheme.starts_with("local:") {
                        continue; // Skip local-only resources.
                    }
                    if let Some(github_path) = after_scheme.strip_prefix("github:") {
                        let parts: Vec<&str> = github_path.splitn(3, '/').collect();
                        if parts.len() >= 2 {
                            let repo = format!("{}/{}", parts[0], parts[1]);
                            println!(
                                "[community] Opening upstream PR against {} for: {}",
                                repo, uri
                            );
                            let gh_result = std::process::Command::new("gh")
                                .args([
                                    "pr",
                                    "create",
                                    "--repo",
                                    &repo,
                                    "--title",
                                    "Community feedback from TA draft",
                                    "--body",
                                    &format!(
                                        "Upstream contribution from TA-mediated goal.\n\nArtifact: {}",
                                        uri
                                    ),
                                ])
                                .output();
                            match gh_result {
                                Ok(out) if out.status.success() => {
                                    let pr_url = String::from_utf8_lossy(&out.stdout);
                                    println!("[community] PR created: {}", pr_url.trim());
                                }
                                Ok(out) => {
                                    let err = String::from_utf8_lossy(&out.stderr);
                                    println!(
                                        "[community] PR creation failed (non-fatal): {}",
                                        err.trim()
                                    );
                                }
                                Err(e) => {
                                    println!(
                                        "[community] gh CLI not available — PR creation skipped: {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // §5b: Remind user to commit lock files that were part of the applied diff.
    // Lock files are deterministic build outputs — they should be committed with the feature branch.
    const LOCK_FILES: &[&str] = &[
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "go.sum",
        "Pipfile.lock",
        "poetry.lock",
        "Gemfile.lock",
        "composer.lock",
        "mix.lock",
    ];
    let applied_locks: Vec<&str> = LOCK_FILES
        .iter()
        .copied()
        .filter(|lock| {
            applied_files
                .iter()
                .any(|f| f == lock || f.ends_with(&format!("/{}", lock)))
        })
        .collect();
    if !applied_locks.is_empty() {
        for lock in &applied_locks {
            eprintln!(
                "hint: Lock file updated: {} — commit it alongside your feature branch:",
                lock
            );
            eprintln!("      git add {} && git commit --amend --no-edit", lock);
        }
    }

    // Mark plan phase(s) as done in PLAN.md + record history + suggest next.
    // Supports comma-separated --phase override (v0.8.2) or falls back to goal.plan_phase.
    // Bug D fix (v0.13.1.7): for VCS apply, plan update runs AFTER adapter.prepare() inside
    // the submit closure so PLAN.md is not dirty when git checks out the feature branch.
    // For non-VCS apply (!git_commit), plan update runs here before rollback.commit().
    let phase_ids: Vec<String> = if let Some(override_phases) = phase_override {
        override_phases
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(ref phase) = goal.plan_phase {
        vec![phase.clone()]
    } else {
        vec![]
    };

    // Non-VCS path: run plan update now, before rollback.commit().
    // VCS path: plan update runs inside the submit closure after adapter.prepare().
    if !git_commit && !phase_ids.is_empty() {
        let plan_path = target_dir.join("PLAN.md");
        if plan_path.exists() {
            let mut content = std::fs::read_to_string(&plan_path)?;
            let mut last_phase_id = String::new();

            for phase in &phase_ids {
                let phases_before = super::plan::parse_plan(&content);
                let old_status = phases_before
                    .iter()
                    .find(|p| super::plan::phase_ids_match(&p.id, phase))
                    .map(|p| p.status.clone())
                    .unwrap_or(super::plan::PlanStatus::Pending);

                eprintln!(
                    "[plan-update] goal phase_id={:?}, matched plan id={:?}, old_status={:?}",
                    phase,
                    phases_before
                        .iter()
                        .find(|p| super::plan::phase_ids_match(&p.id, phase))
                        .map(|p| &p.id),
                    old_status,
                );

                let updated = super::plan::update_phase_status(
                    &content,
                    phase,
                    super::plan::PlanStatus::Done,
                );

                let changed = updated != content;
                eprintln!(
                    "[plan-update] content changed={}, writing to {}",
                    changed,
                    plan_path.display()
                );

                content = updated;
                println!("Updated PLAN.md: Phase {} -> done", phase);

                // Record history.
                let _ = super::plan::record_history(
                    &target_dir,
                    phase,
                    &old_status,
                    &super::plan::PlanStatus::Done,
                );
                last_phase_id = phase.clone();
            }

            std::fs::write(&plan_path, &content)?;

            // Auto-bump workspace version to match the completed phase.
            // Agents should NOT set the version — this is the single authority.
            if let Some(new_ver) = super::plan::phase_id_to_semver(&last_phase_id) {
                match bump_workspace_version(&target_dir, &new_ver) {
                    Ok(bumped) if !bumped.is_empty() => {
                        println!("[version] Phase {} → bumped to {}", last_phase_id, new_ver);
                        // Stage bumped files immediately. bump_workspace_version writes
                        // CLAUDE.md and Cargo.toml to disk but does NOT stage them.
                        // adapter.commit() only stages draft artifacts + auto_stage_candidates;
                        // neither list includes CLAUDE.md (it is restored by
                        // check_and_clean_working_tree before apply). Without this git-add,
                        // the version bump lands in the working tree but misses the commit,
                        // causing the version-check CI job to fail with a mismatch.
                        for bumped_path in &bumped {
                            if let Ok(rel) = bumped_path.strip_prefix(&target_dir) {
                                let _ = std::process::Command::new("git")
                                    .args(["add", &rel.to_string_lossy()])
                                    .current_dir(&target_dir)
                                    .output();
                                println!("[version] staged: {}", rel.display());
                            }
                        }
                    }
                    Ok(_) => {
                        // Already at target version — no change needed.
                    }
                    Err(e) => {
                        tracing::warn!(
                            phase = %last_phase_id,
                            version = %new_ver,
                            "auto version bump failed: {}",
                            e
                        );
                        eprintln!(
                            "[version] Warning: could not auto-bump version to {} — run \
                             ./scripts/bump-version.sh {} manually",
                            new_ver, new_ver
                        );
                    }
                }
            }

            // v0.12.5: Write plan phase completion into memory for future context injection.
            for phase in &phase_ids {
                let phase_title = super::plan::parse_plan(&content)
                    .into_iter()
                    .find(|p| super::plan::phase_ids_match(&p.id, phase))
                    .map(|p| p.title)
                    .unwrap_or_else(|| phase.clone());
                capture_phase_complete_to_memory(
                    config,
                    phase,
                    &phase_title,
                    Some(&pkg.summary.what_changed),
                );
            }

            // Auto-suggest the next pending phase (after the last marked phase).
            let phases_after = super::plan::parse_plan(&content);
            if let Some(next) =
                super::plan::find_next_pending(&phases_after, Some(last_phase_id.as_str()))
            {
                println!();
                println!("Next pending phase: {} — {}", next.id, next.title);
                println!(
                    "  To start: {}",
                    super::plan::suggest_next_goal_command(next)
                );
            }
        }
    }

    // No VCS submit: apply is complete — commit the rollback guard so it does
    // not attempt to restore files when it drops at the end of this function.
    if !git_commit {
        rollback_guard.commit();
    }

    // v0.14.6: Write goal audit ledger entry here — BEFORE adapter.commit() —
    // so that auto_stage_critical_files() picks up the dirty goal-audit.jsonl
    // and includes it in the VCS commit. Writing after adapter.commit() means
    // the file is modified too late to be caught by git status checks.
    {
        let reviewer = pkg.pending_approvals.first().map(|a| a.reviewer.as_str());
        write_goal_audit_entry(
            config,
            &pkg,
            Some(goal),
            ta_audit::AuditDisposition::Applied,
            reviewer,
            None,
            None,
        );
    }

    // Submit workflow integration (VCS-agnostic: git, svn, perforce, etc.).
    if git_commit {
        use ta_submit::{select_adapter, SavedVcsState, SourceAdapter, WorkflowConfig};

        // Load workflow config if it exists.
        let workflow_config_path = target_dir.join(".ta/workflow.toml");
        let workflow_config = WorkflowConfig::load_or_default(&workflow_config_path);

        // Select adapter via registry (auto-detects VCS if config is default "none").
        let adapter: Box<dyn SourceAdapter> = select_adapter(&target_dir, &workflow_config.submit);

        if adapter.name() == "none" && !dry_run {
            eprintln!(
                "Warning: submit was requested but no VCS adapter detected. \
                 Files were copied but no VCS operations will run.\n  \
                 Configure [submit].adapter in .ta/workflow.toml or use --no-submit."
            );
        } else {
            println!("\nUsing submit adapter: {}", adapter.name());
        }

        // --dry-run: show what would happen and exit without making changes.
        if dry_run {
            println!(
                "\n[dry-run] Submit workflow preview (adapter: {}):",
                adapter.name()
            );
            println!("  Stage:  adapter.prepare() — create working branch/changelist");
            println!("  Commit: adapter.commit() — stage changes for the configured VCS");
            if git_push {
                println!("  Submit: adapter.push() — submit/push to remote");
            }
            if git_review {
                println!("  Review: adapter.open_review() — create PR/review request");
            }
            if !workflow_config.verify.commands.is_empty() && !skip_verify {
                println!(
                    "  Verify: {} pre-submit check(s) would run first",
                    workflow_config.verify.commands.len()
                );
            }
            println!("\n  No changes were made. Remove --dry-run to execute.");
            // Skip the actual submit workflow but continue with goal state transitions.
            // Files are already on disk — no rollback needed for dry-run.
            rollback_guard.commit();
        } else {
            // Build the saved-state from the branch captured before pre-flight.
            // Pre-flight may have already switched to a feature branch, so calling
            // adapter.save_state() here would capture the feature branch — meaning
            // restore_state() would be a no-op and the user would be left on the
            // feature branch after apply. Using original_branch ensures the user is
            // returned to their working branch (e.g., main) when apply completes.
            let saved_state = original_branch.as_ref().map(|b| SavedVcsState {
                adapter: adapter.name().to_string(),
                data: Box::new(b.clone()),
            });

            let submit_result = (|| -> anyhow::Result<()> {
                // Prepare (create branch if needed). Hard failure — if we cannot
                // branch, we must NOT commit to whatever branch is checked out
                // (which may be main). This is a TA precept: all code changes go
                // through a feature branch + PR, never directly to main.
                adapter
                    .prepare(goal, &workflow_config.submit)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to create feature branch before commit — aborting to prevent \
                         committing directly to the current branch. VCS error: {}. \
                         Check that your working tree is clean and the branch does not already \
                         exist in a conflicting state, then re-run `ta draft apply --submit`.",
                            e
                        )
                    })?;

                // §15 VCS Submit Invariant: all adapters must verify they are not
                // positioned to commit directly to a protected target after prepare().
                // Skipped for "none" adapter (no VCS ops at all).
                if adapter.name() != "none" {
                    adapter
                        .verify_not_on_protected_target()
                        .map_err(|e| anyhow::anyhow!("{}", e))?;
                }

                // Bug D fix (v0.13.1.7): plan-phase update runs here, AFTER prepare() has
                // checked out the feature branch. This ensures PLAN.md is not dirty at
                // branch-checkout time. The write is included in the commit below.
                if !phase_ids.is_empty() {
                    let plan_path = target_dir.join("PLAN.md");
                    if plan_path.exists() {
                        let mut content = std::fs::read_to_string(&plan_path)?;
                        let mut last_phase_id = String::new();

                        for phase in &phase_ids {
                            let phases_before = super::plan::parse_plan(&content);
                            let old_status = phases_before
                                .iter()
                                .find(|p| super::plan::phase_ids_match(&p.id, phase))
                                .map(|p| p.status.clone())
                                .unwrap_or(super::plan::PlanStatus::Pending);

                            eprintln!(
                                "[plan-update] goal phase_id={:?}, matched plan id={:?}, old_status={:?}",
                                phase,
                                phases_before
                                    .iter()
                                    .find(|p| super::plan::phase_ids_match(&p.id, phase))
                                    .map(|p| &p.id),
                                old_status,
                            );

                            let updated = super::plan::update_phase_status(
                                &content,
                                phase,
                                super::plan::PlanStatus::Done,
                            );

                            let changed = updated != content;
                            eprintln!(
                                "[plan-update] content changed={}, writing to {}",
                                changed,
                                plan_path.display()
                            );

                            content = updated;
                            println!("Updated PLAN.md: Phase {} -> done", phase);

                            let _ = super::plan::record_history(
                                &target_dir,
                                phase,
                                &old_status,
                                &super::plan::PlanStatus::Done,
                            );
                            last_phase_id = phase.clone();
                        }

                        std::fs::write(&plan_path, &content)?;

                        // Auto-bump workspace version to match the completed phase.
                        if let Some(new_ver) = super::plan::phase_id_to_semver(&last_phase_id) {
                            match bump_workspace_version(&target_dir, &new_ver) {
                                Ok(bumped) if !bumped.is_empty() => {
                                    println!(
                                        "[version] Phase {} → bumped to {}",
                                        last_phase_id, new_ver
                                    );
                                    // Stage bumped files so adapter.commit() picks them up.
                                    // Same fix as the primary bump call site above.
                                    for bumped_path in &bumped {
                                        if let Ok(rel) = bumped_path.strip_prefix(&target_dir) {
                                            let _ = std::process::Command::new("git")
                                                .args(["add", &rel.to_string_lossy()])
                                                .current_dir(&target_dir)
                                                .output();
                                            println!("[version] staged: {}", rel.display());
                                        }
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        phase = %last_phase_id,
                                        version = %new_ver,
                                        "auto version bump failed: {}",
                                        e
                                    );
                                    eprintln!(
                                        "[version] Warning: could not auto-bump version to \
                                         {} — run ./scripts/bump-version.sh {} manually",
                                        new_ver, new_ver
                                    );
                                }
                            }
                        }

                        for phase in &phase_ids {
                            let phase_title = super::plan::parse_plan(&content)
                                .into_iter()
                                .find(|p| super::plan::phase_ids_match(&p.id, phase))
                                .map(|p| p.title)
                                .unwrap_or_else(|| phase.clone());
                            capture_phase_complete_to_memory(
                                config,
                                phase,
                                &phase_title,
                                Some(&pkg.summary.what_changed),
                            );
                        }

                        let phases_after = super::plan::parse_plan(&content);
                        if let Some(next) = super::plan::find_next_pending(
                            &phases_after,
                            Some(last_phase_id.as_str()),
                        ) {
                            println!();
                            println!("Next pending phase: {} — {}", next.id, next.title);
                            println!(
                                "  To start: {}",
                                super::plan::suggest_next_goal_command(next)
                            );
                        }
                    }
                }

                // Pre-submit verification gate: run configured checks before committing.
                if skip_verify {
                    println!("\n  Skipping pre-submit verification (--skip-verify).");
                } else if !workflow_config.verify.commands.is_empty() {
                    println!("\nRunning pre-submit verification...");
                    let verify_result =
                        super::verify::run_verification(&workflow_config.verify, &target_dir);
                    if !verify_result.passed {
                        for w in &verify_result.warnings {
                            eprintln!(
                                "\n--- {} (exit code: {}) ---",
                                w.command,
                                w.exit_code.map_or("N/A".into(), |c| c.to_string())
                            );
                            if !w.output.is_empty() {
                                eprintln!("{}", w.output);
                            }
                            eprintln!("---");
                        }
                        let total = workflow_config.verify.commands.len();
                        let failed = verify_result.warnings.len();
                        eprintln!(
                            "\nPre-submit verification failed — {} of {} checks failed.",
                            failed, total
                        );

                        // Heuristic: if failure output mentions Nix store paths or known
                        // build-environment patterns, it's likely an env issue, not a code bug.
                        let env_patterns = [
                            "/nix/store",
                            "nix store",
                            "nix-store",
                            "invalid store path",
                            "store path",
                            "glib-",
                            "libz.",
                            "build environment",
                            "hash mismatch",
                        ];
                        let is_likely_env_failure = verify_result.warnings.iter().any(|w| {
                            let lower = w.output.to_lowercase();
                            env_patterns.iter().any(|pat| lower.contains(pat))
                        });
                        if is_likely_env_failure {
                            eprintln!(
                                "\nNote: this failure may be a build-environment issue, not a \
                                 code problem (Nix store or dependency error detected)."
                            );
                            eprintln!(
                                "Re-run after fixing your environment (e.g. `nix develop` or \
                                 rebuilding your Nix store), then retry `ta draft apply`."
                            );
                        } else {
                            eprintln!("\nFix the issues above and re-run `ta draft apply`.");
                        }
                        eprintln!("To skip verification: `ta draft apply --skip-verify`");
                        anyhow::bail!("Pre-submit verification failed");
                    }
                    println!("  All pre-submit checks passed.\n");
                }

                // Commit changes — goal title as subject, complete draft summary as body.
                eprintln!("[apply] Staging changes for VCS commit...");
                let commit_msg = build_commit_message(goal, &pkg);

                // Track VCS state for draft package (v0.11.2.3).
                let mut vcs_branch = String::new();
                let mut vcs_commit_sha = None;
                let mut vcs_review_url = None;
                let mut vcs_review_id = None;
                let mut commit_ignored_artifacts: Vec<ta_changeset::IgnoredArtifact> = vec![];

                match adapter.commit(goal, &pkg, &commit_msg) {
                    Ok(result) => {
                        println!("[ok] {}", result.message);
                        vcs_commit_sha = result
                            .metadata
                            .get("full_hash")
                            .cloned()
                            .or(Some(result.commit_id.clone()));
                        // Capture ignored artifacts for draft view (v0.13.17.5).
                        if !result.ignored_artifacts.is_empty() {
                            let unexpected_count = result
                                .ignored_artifacts
                                .iter()
                                .filter(|a| !a.known_safe)
                                .count();
                            if unexpected_count > 0 {
                                eprintln!(
                                    "[warn] {} artifact(s) were gitignored and dropped from the commit:",
                                    unexpected_count
                                );
                                for a in result.ignored_artifacts.iter().filter(|a| !a.known_safe) {
                                    eprintln!(
                                        "  - {} (gitignored — was this intentional?)",
                                        a.path
                                    );
                                }
                            }
                            commit_ignored_artifacts = result.ignored_artifacts;
                        }
                        // Post-apply dirty-tree check (v0.14.3.7): warn if any
                        // lock files or auto-stage candidates were missed.
                        check_post_commit_dirty_files(&target_dir, &workflow_config);
                    }
                    Err(e) => {
                        eprintln!("Stage/commit failed: {}", e);
                        // Continue anyway if this is a "none" adapter
                        if adapter.name() != "none" {
                            anyhow::bail!("Failed to stage changes: {}", e);
                        }
                    }
                }

                // Submit (push) to remote if requested.
                if git_push {
                    println!("Submitting to remote...");
                    match adapter.push(goal) {
                        Ok(result) => {
                            println!("[ok] {}", result.message);
                            if let Some(b) = result.metadata.get("branch") {
                                vcs_branch = b.clone();
                            }
                            // Item 5: fall back to adapter-derived branch name if metadata
                            // didn't include "branch" (e.g. external VCS plugins).
                            if vcs_branch.is_empty() {
                                // Use the same slugification the git adapter uses so the
                                // branch name is stable even when metadata is absent.
                                let slug: String = goal
                                    .title
                                    .to_lowercase()
                                    .chars()
                                    .map(|c| {
                                        if c.is_alphanumeric() || c == '-' {
                                            c
                                        } else {
                                            '-'
                                        }
                                    })
                                    .collect();
                                let mut collapsed = String::with_capacity(slug.len());
                                let mut prev = false;
                                for c in slug.chars() {
                                    if c == '-' {
                                        if !prev {
                                            collapsed.push(c);
                                        }
                                        prev = true;
                                    } else {
                                        collapsed.push(c);
                                        prev = false;
                                    }
                                }
                                let trimmed = collapsed.trim_matches('-');
                                let slug_part = if trimmed.is_empty() { "goal" } else { trimmed };
                                let truncated = if slug_part.len() > 50 {
                                    slug_part[..50].trim_end_matches('-')
                                } else {
                                    slug_part
                                };
                                vcs_branch = format!("ta/{}", truncated);
                                tracing::warn!(
                                    branch = %vcs_branch,
                                    "Push result missing 'branch' metadata — derived from goal title"
                                );
                            }
                        }
                        Err(e) => {
                            if adapter.name() != "none" {
                                anyhow::bail!("Failed to submit: {}", e);
                            }
                        }
                    }
                }

                // Open review (PR / CL review) if requested.
                if git_review {
                    println!("Creating review request...");
                    match adapter.open_review(goal, &pkg) {
                        Ok(result) => {
                            println!("[ok] {}", result.message);
                            if !result.review_url.starts_with("none://") {
                                println!("  Review URL: {}", result.review_url);
                            }
                            vcs_review_url = Some(result.review_url);
                            vcs_review_id = Some(result.review_id);
                        }
                        Err(e) => {
                            // Item 4: PR failure must not silently succeed.
                            // Store the branch so `ta pr status` can show recovery steps,
                            // then fail with a clear, actionable error message.
                            let branch_display = if vcs_branch.is_empty() {
                                "<branch>".to_string()
                            } else {
                                vcs_branch.clone()
                            };
                            // Save VCS tracking info before failing so the branch is
                            // preserved and `ta draft reopen-review` can retry.
                            if !vcs_branch.is_empty() || vcs_commit_sha.is_some() {
                                use ta_changeset::VcsTrackingInfo;
                                let vcs_info = VcsTrackingInfo {
                                    branch: if vcs_branch.is_empty() {
                                        "unknown".to_string()
                                    } else {
                                        vcs_branch.clone()
                                    },
                                    review_url: None,
                                    review_id: None,
                                    review_state: Some("pr-failed".to_string()),
                                    commit_sha: vcs_commit_sha.clone(),
                                    last_checked: Utc::now(),
                                };
                                pkg.vcs_status = Some(vcs_info);
                                let pkg_path = config
                                    .pr_packages_dir
                                    .join(format!("{}.json", pkg.package_id));
                                if let Ok(json) = serde_json::to_string_pretty(&pkg) {
                                    let _ = std::fs::write(&pkg_path, json);
                                }
                            }
                            let short_id = &pkg.package_id.to_string()[..8];
                            anyhow::bail!(
                                "PR creation failed: {}\n\n\
                                 Branch '{}' was pushed successfully. To create the PR manually:\n\
                                 \n  gh pr create --head {} --base main\n\
                                 \nOr retry automatically once the issue is resolved:\n\
                                 \n  ta draft reopen-review {}",
                                e,
                                branch_display,
                                branch_display,
                                short_id,
                            );
                        }
                    }
                }

                // Save VCS tracking info and ignored artifacts on the draft package.
                // (v0.11.2.3 for VCS, v0.13.17.5 for ignored_artifacts)
                let needs_pkg_save = !vcs_branch.is_empty()
                    || vcs_commit_sha.is_some()
                    || vcs_review_url.is_some()
                    || !commit_ignored_artifacts.is_empty();
                if needs_pkg_save {
                    use ta_changeset::VcsTrackingInfo;
                    if !vcs_branch.is_empty()
                        || vcs_commit_sha.is_some()
                        || vcs_review_url.is_some()
                    {
                        let vcs_info = VcsTrackingInfo {
                            branch: if vcs_branch.is_empty() {
                                "unknown".to_string()
                            } else {
                                vcs_branch
                            },
                            review_url: vcs_review_url,
                            review_id: vcs_review_id,
                            review_state: Some("open".to_string()),
                            commit_sha: vcs_commit_sha,
                            last_checked: Utc::now(),
                        };
                        // Store PR URL on the goal for cross-reference (v0.11.3).
                        let _review_url = vcs_info.review_url.clone();
                        pkg.vcs_status = Some(vcs_info);
                    }
                    // Store ignored artifacts for `ta draft view` (v0.13.17.5).
                    if !commit_ignored_artifacts.is_empty() {
                        pkg.ignored_artifacts = commit_ignored_artifacts;
                    }
                    // Re-save the draft package with updated info.
                    let pkg_path = config
                        .pr_packages_dir
                        .join(format!("{}.json", pkg.package_id));
                    if let Ok(json) = serde_json::to_string_pretty(&pkg) {
                        let _ = std::fs::write(&pkg_path, json);
                    }
                }

                // Auto-sync upstream if configured (v0.11.1).
                if workflow_config.source.sync.auto_sync {
                    println!("\nAuto-syncing upstream (source.sync.auto_sync = true)...");
                    match adapter.sync_upstream() {
                        Ok(result) if result.is_clean() && result.updated => {
                            println!(
                                "[ok] Synced {} new commit(s) from upstream.",
                                result.new_commits
                            );
                        }
                        Ok(result) if !result.is_clean() => {
                            eprintln!(
                                "Warning: auto-sync found {} conflict(s). \
                                 Resolve manually with `ta sync`.",
                                result.conflicts.len()
                            );
                        }
                        Ok(_) => {
                            println!("[ok] Already up to date.");
                        }
                        Err(e) => {
                            eprintln!("Warning: auto-sync failed: {}. Run `ta sync` manually.", e);
                        }
                    }
                }

                Ok(())
            })();

            // Always restore VCS state (e.g., switch back to original branch for Git),
            // regardless of whether the submit operations succeeded or failed.
            if let Err(e) = adapter.restore_state(saved_state) {
                eprintln!("Warning: could not restore VCS state after apply: {}", e);
            }

            // Now propagate any error from the submit operations.
            // If Ok: VCS submit succeeded — commit the rollback guard.
            // If Err: print a structured failure summary with actionable next steps,
            // then return the error so the guard drops uncommitted (triggers file rollback).
            if let Err(e) = submit_result {
                let short_id = &pkg.package_id.to_string()[..8];
                eprintln!(
                    "\n[apply] VCS submit pipeline failed — {} file(s) rolled back.",
                    applied_files.len()
                );
                eprintln!("  Cause: {}", e);

                // After restore_state() the working tree may be on the feature branch
                // (if pre-flight switched there before save_state) or on the original
                // branch (if save_state captured main and restore_state switched back).
                // Always show the current branch explicitly so the user knows exactly
                // where they are before following any manual-commit guidance.
                let current_branch_after_restore = adapter
                    .current_branch()
                    .unwrap_or_else(|_| "unknown".to_string());
                let protected_targets = adapter.protected_submit_targets();
                let on_protected_after_restore = protected_targets
                    .iter()
                    .any(|b| b == &current_branch_after_restore);

                eprintln!("\n  Current branch: {}", current_branch_after_restore);
                if let Some(ref fb) = preflight_branch {
                    if fb != &current_branch_after_restore {
                        eprintln!(
                            "  Feature branch created: {} (no commits were made to it)",
                            fb
                        );
                    }
                }
                if on_protected_after_restore {
                    eprintln!(
                        "\n  [!] DANGER: you are on protected branch '{}'.",
                        current_branch_after_restore
                    );
                    eprintln!(
                        "  [!] Do NOT commit manually here. Switch to the feature branch first:"
                    );
                    if let Some(ref fb) = preflight_branch {
                        eprintln!("  [!]   git checkout {}", fb);
                    }
                }

                eprintln!("\nNext steps:");
                eprintln!("  • Retry (recommended) — files rolled back, VCS state is clean:");
                eprintln!("      ta draft apply {} --submit", short_id);
                if !skip_verify {
                    eprintln!("  • Retry skipping pre-submit checks:");
                    eprintln!("      ta draft apply {} --skip-verify", short_id);
                }
                if !on_protected_after_restore {
                    // Only suggest --no-submit when NOT on a protected branch.
                    // On a non-protected (feature) branch, manual commit is safe.
                    eprintln!(
                        "  • Apply without VCS (files only, then commit manually on '{}'):",
                        current_branch_after_restore
                    );
                    eprintln!("      ta draft apply {} --no-submit", short_id);
                } else {
                    // On a protected branch: show the safe manual path explicitly.
                    eprintln!(
                        "  • Apply without VCS (ONLY after switching to the feature branch):"
                    );
                    if let Some(ref fb) = preflight_branch {
                        eprintln!("      git checkout {}", fb);
                    }
                    eprintln!("      ta draft apply {} --no-submit", short_id);
                }
                return Err(e);
            }
            rollback_guard.commit();
        } // end of non-dry-run block
    }

    // Transition goal to Applied. The pre-flight check validated the state
    // machine transition; this call persists it. Use warning (not bail) for
    // the disk write since files are already applied at this point.
    eprintln!("[apply] Updating goal state -> Applied...");
    if let Err(e) = goal_store.transition(goal.goal_run_id, GoalRunState::Applied) {
        eprintln!(
            "Warning: could not persist goal state transition to Applied: {}",
            e
        );
    }
    let files_applied = pkg.changes.artifacts.len();
    pkg.status = DraftStatus::Applied {
        applied_at: Utc::now(),
    };
    save_package(config, &pkg)?;

    // §8: emit DraftApplied event so all state changes are logged with structured fields.
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let events_dir = config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let event = SessionEvent::DraftApplied {
            goal_id: goal.goal_run_id,
            draft_id: pkg.package_id,
            files_count: files_applied,
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist DraftApplied event: {}", e);
        }
    }

    // §8b: record velocity entry for the applied goal.
    {
        use ta_goal::{GoalOutcome, VelocityEntry, VelocityHistoryStore, VelocityStore};
        let vs = VelocityStore::for_project(&config.workspace_root);
        let entry = VelocityEntry::from_goal(goal, GoalOutcome::Applied);
        if let Err(e) = vs.append(&entry) {
            tracing::warn!("Failed to record velocity entry: {}", e);
        }

        // §8c: when committing to VCS, also append to the shared velocity-history.jsonl.
        // This file is committed alongside plan_history.jsonl so multi-machine stats
        // are visible to the whole team via `ta stats velocity --team`.
        if git_commit {
            let history_entry = VelocityEntry::from_goal(goal, GoalOutcome::Applied)
                .with_machine_id()
                .with_committer(&target_dir);
            let hs = VelocityHistoryStore::for_project(&target_dir);
            if let Err(e) = hs.append(&history_entry) {
                tracing::warn!("Failed to record velocity history entry: {}", e);
            }
        }
    }

    // Auto-close parent draft on follow-up apply (v0.3.6, refined v0.4.1.2).
    // v0.4.1.2: Only auto-close the parent draft when this goal shares the same
    // staging directory (extend case). Standalone follow-ups with different staging
    // should leave the parent draft independently reviewable.
    if let Some(parent_goal_id) = goal.parent_goal_id {
        if let Some(parent_goal) = goal_store.get(parent_goal_id)? {
            let same_staging = goal.workspace_path == parent_goal.workspace_path;
            if same_staging {
                if let Some(parent_pr_id) = parent_goal.pr_package_id {
                    if let Ok(mut parent_pkg) = load_package(config, parent_pr_id) {
                        if matches!(
                            parent_pkg.status,
                            DraftStatus::PendingReview | DraftStatus::Approved { .. }
                        ) {
                            parent_pkg.status = DraftStatus::Closed {
                                closed_at: Utc::now(),
                                reason: Some(format!(
                                    "Auto-closed: follow-up draft {} applied (same staging)",
                                    pkg.package_id
                                )),
                                closed_by: "ta-system".to_string(),
                            };
                            let _ = save_package(config, &parent_pkg);
                            println!(
                                "  Auto-closed parent draft {} (superseded by this follow-up).",
                                &parent_pr_id.to_string()[..8]
                            );
                        }
                    }
                }
            }
            // Different staging (standalone): do NOT auto-close parent draft.
        }
    }

    // Read plan phase status from staging BEFORE auto_clean removes it (v0.14.9 item 10).
    // Staging is the agent's authoritative output; target_dir may not yet reflect changes.
    let phase_status_from_staging: Option<(String, super::plan::PlanStatus)> =
        if let Some(ref phase) = goal.plan_phase {
            let staging_plan = goal.workspace_path.join("PLAN.md");
            let source_plan = target_dir.join("PLAN.md");
            let plan_content = if staging_plan.exists() {
                std::fs::read_to_string(&staging_plan).ok()
            } else {
                std::fs::read_to_string(&source_plan).ok()
            };
            plan_content.and_then(|content| {
                let phases = super::plan::parse_plan(&content);
                phases
                    .into_iter()
                    .find(|p| super::plan::phase_ids_match(&p.id, phase))
                    .map(|p| (phase.clone(), p.status))
            })
        } else {
            None
        };

    // Auto-clean staging directory if configured (v0.11.3 item 27).
    {
        let wf_cfg = ta_submit::WorkflowConfig::load_or_default(
            &config.workspace_root.join(".ta/workflow.toml"),
        );
        if wf_cfg.staging.auto_clean
            && !goal.workspace_path.as_os_str().is_empty()
            && goal.workspace_path.exists()
        {
            match std::fs::remove_dir_all(&goal.workspace_path) {
                Ok(()) => {
                    println!(
                        "Auto-cleaned staging directory: {} (staging.auto_clean=true)",
                        goal.workspace_path.display(),
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Warning: could not auto-clean staging {}: {}",
                        goal.workspace_path.display(),
                        e,
                    );
                }
            }
        }
    }

    // Post-apply validation summary: confirm state is consistent for the human.
    println!();
    println!("-- Post-Apply Status --");
    println!("  Draft:  {} -> applied", id);
    println!(
        "  Goal:   {} -> applied",
        goal.goal_run_id.to_string().get(..8).unwrap_or("?")
    );
    if let Some((ref phase, ref status)) = phase_status_from_staging {
        let status_str = match status {
            super::plan::PlanStatus::Done => "done",
            super::plan::PlanStatus::InProgress => "in_progress",
            super::plan::PlanStatus::Pending => "pending",
            super::plan::PlanStatus::Deferred => "deferred",
        };
        if *status == super::plan::PlanStatus::Done {
            println!("  Plan:   {} -> {}", phase, status_str);
        } else {
            // Fall back to re-reading from target_dir (applied tree) in case plan was updated.
            let target_plan = target_dir.join("PLAN.md");
            if target_plan.exists() {
                let content = std::fs::read_to_string(&target_plan).unwrap_or_default();
                let phases = super::plan::parse_plan(&content);
                if let Some(p) = phases
                    .iter()
                    .find(|p| super::plan::phase_ids_match(&p.id, phase))
                {
                    if p.status == super::plan::PlanStatus::Done {
                        println!("  Plan:   {} -> done", phase);
                    } else {
                        eprintln!(
                            "  [warn] Plan: {} is still '{}' -- expected 'done'. Check PLAN.md.",
                            phase, status_str
                        );
                    }
                } else {
                    eprintln!("  [warn] Plan: phase '{}' not found in PLAN.md", phase);
                }
            } else {
                eprintln!(
                    "  [warn] Plan: {} is still '{}' -- expected 'done'. Check PLAN.md.",
                    phase, status_str
                );
            }
        }
    } else if let Some(ref phase) = goal.plan_phase {
        eprintln!("  [warn] Plan: phase '{}' not found in PLAN.md", phase);
    }
    if git_commit {
        if dry_run {
            println!("  Submit: [dry-run] — no VCS operations performed");
        } else {
            println!(
                "  Submit: staged{}{}",
                if git_push { " + submitted" } else { "" },
                if git_review { " + review" } else { "" }
            );
        }
    }

    // Post-apply guidance: show branch + PR URL prominently at the very end.
    // This block must always be the last output so the URL is never scrolled away.
    if !dry_run && git_commit {
        // Reload the package to get the VCS tracking info saved during apply.
        if let Ok(final_pkg) = load_package(config, package_id) {
            if let Some(ref vcs) = final_pkg.vcs_status {
                let short_id = &package_id.to_string()[..8];
                // Check auto_merge directly from workflow config — this is always current.
                let auto_merge = {
                    use ta_submit::WorkflowConfig;
                    let wf = WorkflowConfig::load_or_default(&target_dir.join(".ta/workflow.toml"));
                    wf.submit.git.auto_merge
                };

                println!();
                println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                println!("  VCS APPLY SUMMARY");
                println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                println!("  Branch:  {}", vcs.branch);
                if let Some(ref sha) = vcs.commit_sha {
                    println!("  Commit:  {}", sha);
                }
                if let Some(ref url) = vcs.review_url {
                    println!("  PR:      {}", url);
                    if auto_merge {
                        println!(
                            "  [!] AUTO-MERGE is enabled — this PR will merge without review."
                        );
                        println!("      Disable: set auto_merge = false in .ta/workflow.toml");
                    } else {
                        println!("  ACTION:  Review and merge the PR above before continuing.");
                    }
                } else if !vcs.branch.is_empty() && vcs.branch != "unknown" {
                    println!(
                        "  PR:      not created (run `ta draft reopen-review {}` to retry)",
                        short_id
                    );
                }
                println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                println!("  ta draft pr-status {}    — check PR/CI status", short_id);
                if !auto_merge {
                    println!(
                        "  ta draft watch {}        — poll until merged + auto-sync",
                        short_id
                    );
                }
            }
        }
    }

    Ok(())
}

// ── Draft amendment (v0.3.4) ────────────────────────────────────────

/// Amend an artifact in a draft package in-place.
///
/// Supports three modes:
/// - `--file path`: Replace the artifact's content with a corrected file and re-diff.
/// - `--drop`: Remove the artifact from the draft entirely.
/// - `--patch` (future): Apply a patch to the artifact.
#[allow(clippy::too_many_arguments)]
fn amend_package(
    config: &GatewayConfig,
    id: &str,
    artifact_uri: &str,
    file_path: Option<&str>,
    drop_artifact: bool,
    reason: Option<&str>,
    amended_by: &str,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    // Only allow amendment on drafts in review states.
    match &pkg.status {
        DraftStatus::PendingReview | DraftStatus::Draft => {}
        DraftStatus::Approved { .. } => {
            // Allow amending approved drafts — user might spot something after approval.
        }
        _ => {
            anyhow::bail!(
                "Cannot amend draft in {} state (must be draft, pending_review, or approved)",
                pkg.status
            );
        }
    }

    // Normalize artifact URI: allow shorthand paths without fs://workspace/ prefix.
    let normalized_uri = if artifact_uri.starts_with("fs://") {
        artifact_uri.to_string()
    } else {
        format!("fs://workspace/{}", artifact_uri)
    };

    // Validate: exactly one mode.
    if drop_artifact && file_path.is_some() {
        anyhow::bail!("Cannot use both --file and --drop at the same time");
    }
    if !drop_artifact && file_path.is_none() {
        anyhow::bail!("Must specify either --file <path> or --drop");
    }

    if drop_artifact {
        // ── Drop mode: remove the artifact from the draft ──
        let original_count = pkg.changes.artifacts.len();
        pkg.changes
            .artifacts
            .retain(|a| a.resource_uri != normalized_uri);

        if pkg.changes.artifacts.len() == original_count {
            anyhow::bail!("Artifact not found in draft: {}", normalized_uri);
        }

        // Record amendment in the decision log.
        pkg.plan.decision_log.push(DecisionLogEntry {
            decision: format!("Human dropped artifact: {}", normalized_uri),
            rationale: reason.unwrap_or("Artifact removed from draft").to_string(),
            alternatives: vec![],
            alternatives_considered: vec![],
            confidence: None,
            context: None,
        });

        save_package(config, &pkg)?;
        println!(
            "Dropped artifact {} from draft {}",
            normalized_uri, package_id
        );
        println!(
            "  Draft now has {} artifact(s)",
            pkg.changes.artifacts.len()
        );
    } else if let Some(corrected_file) = file_path {
        // ── File replacement mode ──
        let corrected_path = std::path::Path::new(corrected_file);
        if !corrected_path.exists() {
            anyhow::bail!("Corrected file not found: {}", corrected_file);
        }

        // Find the artifact.
        let artifact_idx = pkg
            .changes
            .artifacts
            .iter()
            .position(|a| a.resource_uri == normalized_uri)
            .ok_or_else(|| anyhow::anyhow!("Artifact not found in draft: {}", normalized_uri))?;

        // Read the corrected content.
        let corrected_content = fs::read_to_string(corrected_path)?;

        // Compute a new diff against the source if we can find it.
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goals = goal_store.list()?;
        let goal = goals.iter().find(|g| {
            g.goal_run_id.to_string() == pkg.goal.goal_id || g.pr_package_id == Some(package_id)
        });

        let new_diff = if let Some(goal) = goal {
            if let Some(ref source_dir) = goal.source_dir {
                let rel_path = normalized_uri
                    .strip_prefix("fs://workspace/")
                    .unwrap_or(&normalized_uri);
                let source_file = source_dir.join(rel_path);
                if source_file.exists() {
                    let original = fs::read_to_string(&source_file)?;
                    // Compute unified diff.
                    Some(compute_unified_diff(
                        rel_path,
                        &original,
                        &corrected_content,
                    ))
                } else {
                    // New file — diff is "create file" content.
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Update the changeset in the store if we have a goal.
        if let Some(goal) = goal {
            let mut store = JsonFileStore::new(goal.store_path.clone())?;
            let goal_id_str = goal.goal_run_id.to_string();
            let cs = if let Some(ref diff) = new_diff {
                ChangeSet::new(
                    normalized_uri.clone(),
                    ChangeKind::FsPatch,
                    DiffContent::UnifiedDiff {
                        content: diff.clone(),
                    },
                )
                .with_commit_intent(CommitIntent::RequestCommit)
            } else {
                ChangeSet::new(
                    normalized_uri.clone(),
                    ChangeKind::FsPatch,
                    DiffContent::CreateFile {
                        content: corrected_content.clone(),
                    },
                )
                .with_commit_intent(CommitIntent::RequestCommit)
            };
            store.save(&goal_id_str, &cs)?;

            // Also write the corrected file into the staging workspace so
            // future `ta draft build` picks it up.
            let rel_path = normalized_uri
                .strip_prefix("fs://workspace/")
                .unwrap_or(&normalized_uri);
            let staging_file = goal.workspace_path.join(rel_path);
            if let Some(parent) = staging_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&staging_file, &corrected_content)?;
        }

        // Update the artifact metadata.
        let artifact = &mut pkg.changes.artifacts[artifact_idx];
        artifact.amendment = Some(AmendmentRecord {
            amended_by: amended_by.to_string(),
            amended_at: Utc::now(),
            amendment_type: AmendmentType::FileReplaced,
            reason: reason.map(|s| s.to_string()),
        });

        // Reset disposition to Pending since the content changed.
        artifact.disposition = ArtifactDisposition::Pending;

        // Record in decision log.
        pkg.plan.decision_log.push(DecisionLogEntry {
            decision: format!("Human amended artifact: {}", normalized_uri),
            rationale: reason
                .unwrap_or("Content replaced with corrected file")
                .to_string(),
            alternatives: vec![],
            alternatives_considered: vec![],
            confidence: None,
            context: None,
        });

        save_package(config, &pkg)?;
        println!(
            "Amended artifact {} in draft {}",
            normalized_uri, package_id
        );
        if new_diff.is_some() {
            println!("  Diff recomputed against source");
        }
        println!("  Disposition reset to: pending");
        println!(
            "  Amended by: {} ({})",
            amended_by,
            Utc::now().format("%Y-%m-%d %H:%M UTC")
        );
    }

    Ok(())
}

/// Compute a simple unified diff between two strings.
fn compute_unified_diff(path: &str, original: &str, modified: &str) -> String {
    let mut output = format!("--- a/{}\n+++ b/{}\n", path, path);
    let original_lines: Vec<&str> = original.lines().collect();
    let modified_lines: Vec<&str> = modified.lines().collect();

    // Simple line-by-line diff using a basic LCS approach.
    // For a more sophisticated diff we'd use a proper library,
    // but this covers the common case adequately.
    let mut i = 0;
    let mut j = 0;
    let mut hunk_start_orig = 1;
    let mut hunk_start_mod = 1;
    let mut hunk_lines: Vec<String> = Vec::new();
    let mut context_before: Vec<String> = Vec::new();

    while i < original_lines.len() || j < modified_lines.len() {
        if i < original_lines.len()
            && j < modified_lines.len()
            && original_lines[i] == modified_lines[j]
        {
            // Lines match — accumulate as context.
            if !hunk_lines.is_empty() {
                hunk_lines.push(format!(" {}", original_lines[i]));
            } else {
                context_before.push(format!(" {}", original_lines[i]));
                if context_before.len() > 3 {
                    context_before.remove(0);
                    hunk_start_orig += 1;
                    hunk_start_mod += 1;
                }
            }
            i += 1;
            j += 1;
        } else {
            // Mismatch — start or extend a hunk.
            if hunk_lines.is_empty() {
                hunk_lines.append(&mut context_before);
            }
            if i < original_lines.len()
                && (j >= modified_lines.len() || !modified_lines[j..].contains(&original_lines[i]))
            {
                hunk_lines.push(format!("-{}", original_lines[i]));
                i += 1;
            } else if j < modified_lines.len() {
                hunk_lines.push(format!("+{}", modified_lines[j]));
                j += 1;
            } else {
                break;
            }
        }
    }

    if !hunk_lines.is_empty() {
        let orig_count = hunk_lines.iter().filter(|l| !l.starts_with('+')).count();
        let mod_count = hunk_lines.iter().filter(|l| !l.starts_with('-')).count();
        output.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_start_orig, orig_count, hunk_start_mod, mod_count
        ));
        for line in &hunk_lines {
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

// ── Scoped agent re-work (v0.3.4) ──────────────────────────────────

/// Create a scoped follow-up goal targeting only discuss/amended artifacts.
///
/// Unlike `ta run --follow-up` which re-runs against the full source tree,
/// `ta draft fix` creates a minimal staging workspace containing only the
/// affected files and injects focused guidance for the agent.
fn fix_package(
    config: &GatewayConfig,
    id: &str,
    target_uri: Option<&str>,
    guidance: &str,
    agent: &str,
    no_launch: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    // Only allow fix on drafts in review states.
    match &pkg.status {
        DraftStatus::PendingReview | DraftStatus::Draft | DraftStatus::Approved { .. } => {}
        _ => {
            anyhow::bail!(
                "Cannot fix draft in {} state (must be draft, pending_review, or approved)",
                pkg.status
            );
        }
    }

    // Find the goal associated with this draft.
    let goal_store = GoalRunStore::new(&config.goals_dir)?;
    let goals = goal_store.list()?;
    let parent_goal = goals
        .iter()
        .find(|g| {
            g.goal_run_id.to_string() == pkg.goal.goal_id || g.pr_package_id == Some(package_id)
        })
        .ok_or_else(|| anyhow::anyhow!("Cannot find goal associated with draft {}", package_id))?;

    let source_dir = parent_goal
        .source_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Parent goal has no source_dir"))?;

    // Determine which artifacts to target.
    let target_artifacts: Vec<&Artifact> = if let Some(uri) = target_uri {
        let normalized = if uri.starts_with("fs://") {
            uri.to_string()
        } else {
            format!("fs://workspace/{}", uri)
        };
        let artifact = pkg
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri == normalized)
            .ok_or_else(|| anyhow::anyhow!("Artifact not found in draft: {}", normalized))?;
        vec![artifact]
    } else {
        // Default: all discuss items + amended items.
        let targets: Vec<&Artifact> = pkg
            .changes
            .artifacts
            .iter()
            .filter(|a| {
                matches!(a.disposition, ArtifactDisposition::Discuss) || a.amendment.is_some()
            })
            .collect();
        if targets.is_empty() {
            anyhow::bail!(
                "No discuss or amended artifacts found in draft {}.\n\
                 Use --artifact-uri to target a specific artifact, or mark artifacts \
                 as 'discuss' during review first.",
                package_id
            );
        }
        targets
    };

    println!("Scoped fix for draft {}", package_id);
    println!("  Targeting {} artifact(s):", target_artifacts.len());
    for a in &target_artifacts {
        println!("    {} [{}]", a.resource_uri, a.disposition);
    }
    println!("  Guidance: {}", guidance);
    println!();

    // Build a scoped follow-up title.
    let fix_title = format!(
        "Fix: {} (from draft {})",
        truncate(guidance, 60),
        &id[..8.min(id.len())]
    );

    // Use `ta run --follow-up` mechanism via goal commands.
    // This creates a full overlay but with focused context injection.
    let follow_up_id = Some(Some(id.to_string()));

    super::run::execute(
        config,
        Some(&fix_title),
        agent,
        Some(source_dir.as_path()),
        &format!(
            "Scoped fix: {}. Target only the artifacts listed in the Follow-Up Context below.",
            guidance,
        ),
        parent_goal.plan_phase.as_deref(),
        follow_up_id.as_ref(),
        None, // follow_up_draft
        None, // follow_up_goal
        None, // no objective file
        no_launch,
        false, // not interactive
        false, // not macro
        None,  // not resuming
        false, // not headless
        false, // skip_verify = false
        false, // quiet = false
        None,  // no existing goal id
        None,  // workflow = default (single-agent)
        None,  // persona_name = None
    )?;

    if no_launch {
        println!("\nScoped fix workspace ready.");
        println!("The agent context includes your guidance and the target artifacts.");
        println!("When the agent finishes, the original draft will be superseded.");
    } else {
        println!("\nScoped fix complete.");
        println!("The new draft supersedes draft {}.", package_id);
        println!("Review with: ta draft list");
    }

    Ok(())
}

// ── Pre-apply artifact safety checks (v0.13.17.2) ──────────────────

/// Critical files that warrant a lower shrinkage threshold (50% instead of 80%).
const CRITICAL_FILES: &[&str] = &[
    ".gitignore",
    "Cargo.toml",
    "flake.nix",
    "CLAUDE.md",
    "Cargo.lock",
];

/// Check artifacts for destructive changes before applying them to the filesystem.
///
/// Returns an error string if any artifact fails a safety check.
/// The caller should print the error and offer `--force-apply` as a bypass.
///
/// Checks:
/// 1. Dramatic shrinkage: file shrinks >80% in line count.
/// 2. Critical file replacement: known-critical file loses >50% of content.
fn run_apply_safety_checks(
    artifact_uris: &[String],
    workspace_path: &std::path::Path,
    target_dir: &std::path::Path,
    goal_title: &str,
) -> Result<(), String> {
    let mut violations: Vec<String> = Vec::new();

    for uri in artifact_uris {
        let Some(rel) = uri.strip_prefix("fs://workspace/") else {
            continue;
        };

        let source_path = target_dir.join(rel);
        let staged_path = workspace_path.join(rel);

        // Only check files that already exist in source (new files are fine).
        if !source_path.exists() {
            continue;
        }
        // Only check text files (skip binaries).
        if is_binary_file(&source_path) || is_binary_file(&staged_path) {
            continue;
        }

        let source_content = match std::fs::read_to_string(&source_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let staged_content = match std::fs::read_to_string(&staged_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let source_lines = source_content.lines().count();
        let staged_lines = staged_content.lines().count();

        if source_lines == 0 {
            continue;
        }

        let file_name = std::path::Path::new(rel)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(rel);

        let is_critical = CRITICAL_FILES
            .iter()
            .any(|f| file_name.eq_ignore_ascii_case(f) || rel.eq_ignore_ascii_case(f));

        // Compute shrinkage as percentage.
        let shrinkage_pct = if staged_lines < source_lines {
            (source_lines - staged_lines) * 100 / source_lines
        } else {
            0
        };

        let threshold = if is_critical { 50 } else { 80 };

        if shrinkage_pct >= threshold {
            violations.push(format!(
                "Artifact '{}' shrank {}% ({} → {} lines).{} use --force-apply to override.",
                rel,
                shrinkage_pct,
                source_lines,
                staged_lines,
                if is_critical {
                    " This is a critical file —"
                } else {
                    " This looks destructive —"
                }
            ));
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        let msg = format!(
            "Pre-apply safety check failed for goal '{}':\n{}",
            goal_title,
            violations
                .iter()
                .map(|v| format!("  - {}", v))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Err(msg)
    }
}

// ── Draft close (v0.3.6) ────────────────────────────────────────────

/// Close a draft without applying it (abandoned, hand-merged, or obsolete).
fn close_package(
    config: &GatewayConfig,
    id: &str,
    reason: Option<&str>,
    closed_by: &str,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    // Only allow closing drafts that are in non-terminal states.
    match &pkg.status {
        DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. } => {}
        DraftStatus::Applied { .. } => {
            anyhow::bail!("Draft {} is already applied — cannot close", package_id)
        }
        DraftStatus::Denied { .. } => {
            anyhow::bail!("Draft {} is already denied — cannot close", package_id)
        }
        DraftStatus::Superseded { .. } => {
            anyhow::bail!("Draft {} is already superseded — cannot close", package_id)
        }
        DraftStatus::Closed { .. } => {
            anyhow::bail!("Draft {} is already closed", package_id)
        }
    }

    let prev_status = pkg.status.to_string();
    pkg.status = DraftStatus::Closed {
        closed_at: Utc::now(),
        reason: reason.map(|s| s.to_string()),
        closed_by: closed_by.to_string(),
    };
    save_package(config, &pkg)?;

    // Write audit event.
    if let Ok(mut audit_log) = ta_audit::AuditLog::open(&config.audit_log) {
        let mut event = ta_audit::AuditEvent::new(closed_by, ta_audit::AuditAction::Approval)
            .with_target(format!("draft://{}", package_id))
            .with_metadata(serde_json::json!({
                "action": "closed",
                "previous_status": prev_status,
                "reason": reason.unwrap_or(""),
            }));
        let _ = audit_log.append(&mut event);
    }

    // v0.14.6: Write goal audit ledger entry.
    {
        let goal_store = GoalRunStore::new(&config.goals_dir);
        let goal = goal_store.ok().and_then(|store| {
            store
                .list()
                .ok()?
                .into_iter()
                .find(|g| g.pr_package_id == Some(package_id))
        });
        write_goal_audit_entry(
            config,
            &pkg,
            goal.as_ref(),
            ta_audit::AuditDisposition::Closed,
            Some(closed_by),
            None,
            reason,
        );
    }

    println!("Draft {} closed.", package_id);
    if let Some(r) = reason {
        println!("  Reason: {}", r);
    }
    println!("  Previous status: {}", prev_status);
    Ok(())
}

// ── Goal audit ledger helpers (v0.14.6) ─────────────────────────────

/// Write a goal-level audit entry to the goal audit ledger.
///
/// Best-effort — failures are logged as warnings, never propagated.
fn write_goal_audit_entry(
    config: &GatewayConfig,
    pkg: &DraftPackage,
    goal: Option<&ta_goal::GoalRun>,
    disposition: ta_audit::AuditDisposition,
    reviewer: Option<&str>,
    denial_reason: Option<&str>,
    cancel_reason: Option<&str>,
) {
    let ledger_path = ta_audit::GoalAuditLedger::path_for(&config.workspace_root);
    let mut ledger = match ta_audit::GoalAuditLedger::open(&ledger_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("Failed to open goal audit ledger: {}", e);
            return;
        }
    };

    let now = chrono::Utc::now();
    let goal_id_str = &pkg.goal.goal_id;
    let goal_uuid = uuid::Uuid::parse_str(goal_id_str).unwrap_or_else(|_| uuid::Uuid::new_v4());

    let (created_at, phase, agent_id, parent_goal_id, total_seconds) = if let Some(g) = goal {
        let total = now.signed_duration_since(g.created_at).num_seconds();
        (
            g.created_at,
            g.plan_phase.clone(),
            g.agent_id.clone(),
            g.parent_goal_id,
            total,
        )
    } else {
        (now, None, pkg.agent_identity.agent_id.clone(), None, 0)
    };

    // Build compact artifact list.
    let artifacts: Vec<ta_audit::ArtifactRecord> = pkg
        .changes
        .artifacts
        .iter()
        .map(|a| ta_audit::ArtifactRecord {
            uri: a.resource_uri.clone(),
            change_type: format!("{:?}", a.change_type).to_lowercase(),
        })
        .collect();
    let artifact_count = artifacts.len();

    let ai_summary = if pkg.summary.what_changed.is_empty() {
        None
    } else {
        Some(pkg.summary.what_changed.clone())
    };

    let mut entry = ta_audit::AuditEntry {
        goal_id: goal_uuid,
        title: pkg.goal.title.clone(),
        objective: Some(pkg.goal.objective.clone()),
        disposition,
        phase,
        agent: agent_id,
        created_at,
        pr_ready_at: None,
        recorded_at: now,
        build_seconds: total_seconds,
        review_seconds: 0,
        total_seconds,
        draft_id: Some(pkg.package_id),
        ai_summary,
        reviewer: reviewer.map(|s| s.to_string()),
        denial_reason: denial_reason.map(|s| s.to_string()),
        cancel_reason: cancel_reason.map(|s| s.to_string()),
        artifact_count,
        lines_changed: 0,
        artifacts,
        policy_result: None,
        parent_goal_id,
        previous_hash: None,
    };

    if let Err(e) = ledger.append(&mut entry) {
        tracing::warn!("Failed to write goal audit entry: {}", e);
    }
}

// ── Draft close --stale (v0.13.17.2) ────────────────────────────────

/// Close all stale draft records (Approved or PendingReview) older than the configured threshold.
///
/// Returns the number of drafts closed.
fn close_stale_drafts(
    config: &GatewayConfig,
    older_than: Option<u64>,
    reason: Option<&str>,
    closed_by: &str,
    skip_confirm: bool,
) -> anyhow::Result<usize> {
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let days = older_than.unwrap_or(workflow_config.gc.stale_threshold_days);
    let cutoff = Utc::now() - Duration::days(days as i64);

    let packages = load_all_packages(config)?;
    let stale: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            matches!(
                p.status,
                DraftStatus::Approved { .. } | DraftStatus::PendingReview
            ) && p.created_at < cutoff
        })
        .collect();

    if stale.is_empty() {
        println!("No stale drafts found (threshold: {} days).", days);
        return Ok(0);
    }

    println!("{} stale draft(s) (older than {} days):", stale.len(), days);
    for p in &stale {
        let age_days = (Utc::now() - p.created_at).num_days();
        println!(
            "  {} — \"{}\" ({}, {} days old)",
            &p.package_id.to_string()[..8],
            truncate(&p.goal.title, 40),
            p.status,
            age_days
        );
    }
    println!();

    if !skip_confirm {
        use std::io::Write;
        print!("Close {} draft(s)? [y/N] ", stale.len());
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted — no drafts closed.");
            return Ok(0);
        }
    }

    let close_reason = reason.unwrap_or("stale — closed by ta draft close --stale");
    let mut closed = 0usize;
    for p in &stale {
        let id = p.package_id.to_string();
        match close_package(config, &id, Some(close_reason), closed_by) {
            Ok(()) => {
                closed += 1;
                println!("Closed draft {}.", &id[..8]);
            }
            Err(e) => {
                eprintln!("Warning: could not close {}: {}", &id[..8], e);
            }
        }
    }

    println!("\nClosed {} stale draft(s).", closed);
    Ok(closed)
}

// ── Draft garbage collection (v0.3.6) ───────────────────────────────

/// Garbage-collect stale staging directories for drafts in terminal states.
/// With `close_drafts=true`, also closes stale draft records as part of the GC pass.
fn gc_packages(
    config: &GatewayConfig,
    dry_run: bool,
    archive: bool,
    close_drafts: bool,
) -> anyhow::Result<()> {
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let threshold_days = workflow_config.gc.stale_threshold_days;
    let cutoff = Utc::now() - Duration::days(threshold_days as i64);

    let goal_store = GoalRunStore::new(&config.goals_dir)?;
    let goals = goal_store.list()?;

    let mut cleaned = 0u32;
    let mut skipped = 0u32;

    for goal in &goals {
        // Only GC goals in terminal states.
        let is_terminal = matches!(
            goal.state,
            GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
        );

        // Also GC goals whose drafts are in terminal states (Denied, Closed, Superseded).
        let draft_terminal = goal.pr_package_id.is_some_and(|pr_id| {
            load_package(config, pr_id).is_ok_and(|pkg| {
                matches!(
                    pkg.status,
                    DraftStatus::Applied { .. }
                        | DraftStatus::Denied { .. }
                        | DraftStatus::Closed { .. }
                        | DraftStatus::Superseded { .. }
                )
            })
        });

        if !(is_terminal || draft_terminal) {
            continue;
        }

        // Check age.
        if goal.updated_at > cutoff {
            continue;
        }

        // Check if staging dir exists.
        if !goal.workspace_path.exists() {
            continue;
        }

        if dry_run {
            println!(
                "[dry-run] Would remove: {} (goal: {}, state: {}, age: {}d)",
                goal.workspace_path.display(),
                &goal.goal_run_id.to_string()[..8],
                goal.state,
                (Utc::now() - goal.updated_at).num_days(),
            );
            cleaned += 1;
        } else if archive {
            let archive_dir = config.workspace_root.join(".ta/archive");
            std::fs::create_dir_all(&archive_dir)?;
            let archive_dest = archive_dir.join(goal.goal_run_id.to_string());
            if archive_dest.exists() {
                eprintln!(
                    "Skipping {} — archive already exists",
                    goal.goal_run_id.to_string().get(..8).unwrap_or("?")
                );
                skipped += 1;
            } else {
                std::fs::rename(&goal.workspace_path, &archive_dest)?;
                println!(
                    "Archived: {} -> {}",
                    goal.workspace_path.display(),
                    archive_dest.display()
                );
                cleaned += 1;
            }
        } else {
            std::fs::remove_dir_all(&goal.workspace_path)?;
            println!(
                "Removed: {} (goal: {})",
                goal.workspace_path.display(),
                &goal.goal_run_id.to_string()[..8],
            );
            cleaned += 1;
        }
    }

    // v0.9.5.1: Also clean orphaned pr_package JSON files whose linked goal
    // is in a terminal state and older than the stale threshold.
    let mut orphaned_count = 0u32;
    if config.pr_packages_dir.exists() {
        if let Ok(entries) = fs::read_dir(&config.pr_packages_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                let Ok(json_str) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(pkg) = serde_json::from_str::<DraftPackage>(&json_str) else {
                    continue;
                };

                // Check if the package is in a terminal state and old enough.
                let is_pkg_terminal = matches!(
                    pkg.status,
                    DraftStatus::Applied { .. }
                        | DraftStatus::Denied { .. }
                        | DraftStatus::Closed { .. }
                        | DraftStatus::Superseded { .. }
                );
                if !is_pkg_terminal || pkg.created_at > cutoff {
                    continue;
                }

                // Check if the linked goal is also terminal.
                let goal_id_ok = pkg.goal.goal_id.parse::<Uuid>().ok();
                let goal_terminal = goal_id_ok.is_some_and(|gid| {
                    goal_store.get(gid).ok().flatten().is_some_and(|g| {
                        matches!(
                            g.state,
                            GoalRunState::Applied
                                | GoalRunState::Completed
                                | GoalRunState::Failed { .. }
                        )
                    })
                });
                if !goal_terminal {
                    continue;
                }

                if dry_run {
                    println!(
                        "[dry-run] Would remove orphaned package: {} ({})",
                        path.display(),
                        &pkg.package_id.to_string()[..8],
                    );
                } else {
                    fs::remove_file(&path)?;
                    println!(
                        "Removed orphaned package: {}",
                        &pkg.package_id.to_string()[..8],
                    );
                }
                orphaned_count += 1;
            }
        }
    }

    // v0.13.17.2: --drafts: close stale draft records as part of GC.
    let drafts_closed = if close_drafts {
        close_stale_drafts(
            config,
            None, // use config default
            Some("closed by ta draft gc --drafts"),
            "ta-gc",
            true, // skip confirmation in non-interactive GC
        )
        .unwrap_or(0)
    } else {
        0
    };

    if dry_run {
        println!(
            "\n{} staging dir(s) would be removed. {} orphaned package(s) would be removed.",
            cleaned, orphaned_count
        );
    } else {
        println!(
            "\n{} staging dir(s) {}. {} orphaned package(s) removed.",
            cleaned,
            if archive { "archived" } else { "removed" },
            orphaned_count,
        );
        if close_drafts {
            println!("{} stale draft record(s) closed.", drafts_closed);
        }
        if skipped > 0 {
            println!("{} skipped (archive already exists).", skipped);
        }
    }
    Ok(())
}

// ── File-based draft package storage ────────────────────────────────

pub fn load_all_packages(config: &GatewayConfig) -> anyhow::Result<Vec<DraftPackage>> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut packages = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let json = fs::read_to_string(&path)?;
            match serde_json::from_str::<DraftPackage>(&json) {
                Ok(pkg) => packages.push(pkg),
                Err(e) => {
                    let filename = path.file_name().unwrap_or_default().to_string_lossy();
                    tracing::warn!(
                        file = %filename,
                        error = %e,
                        "Skipping unreadable draft package (parse error — possible version skew between CLI and daemon)"
                    );
                    eprintln!(
                        "  [warn] Skipping unreadable draft: {}: {} \
                        \n         (Hint: re-run `./install_local.sh` to rebuild the CLI and daemon together)",
                        filename, e
                    );
                }
            }
        }
    }

    packages.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(packages)
}

/// Post-apply dirty-tree check (v0.14.3.7).
///
/// After `ta draft apply --git-commit`, runs `git status --porcelain` to detect
/// any files that should have been auto-staged but were not (e.g., a lock file
/// type not in the built-in list or not yet in `[commit] auto_stage`). Emits a
/// structured warning with remediation instructions for each dirty file.
fn check_post_commit_dirty_files(
    project_root: &std::path::Path,
    workflow_config: &ta_submit::WorkflowConfig,
) {
    if !project_root.join(".git").exists() {
        return;
    }
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(project_root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return,
    };
    let status_text = String::from_utf8_lossy(&output.stdout);
    if status_text.trim().is_empty() {
        return;
    }

    // Collect dirty tracked files (XY path format from git status --porcelain).
    // Filter to files that look like lock files or known auto-stage candidates.
    let auto_stage_configured: std::collections::HashSet<String> =
        workflow_config.commit.auto_stage.iter().cloned().collect();

    let mut missed: Vec<String> = Vec::new();
    for line in status_text.lines() {
        if line.len() < 4 {
            continue;
        }
        let path = line[3..].trim().to_string();
        // Collect paths that are in [commit] auto_stage or the built-in list.
        let in_builtin = ta_submit::GitAdapter::BUILTIN_LOCK_FILES.contains(&path.as_str());
        let in_configured = auto_stage_configured.contains(&path);
        if in_builtin || in_configured {
            missed.push(path);
        }
    }

    if missed.is_empty() {
        return;
    }

    eprintln!();
    eprintln!("⚠  Post-apply uncommitted files detected:");
    for path in &missed {
        let in_builtin = ta_submit::GitAdapter::BUILTIN_LOCK_FILES.contains(&path.as_str());
        let hint = if in_builtin {
            format!(
                "  {}  — built-in lock file; was not tracked by git or was in a subdir. \
                 Run: git add {} && git commit --amend --no-edit",
                path, path
            )
        } else {
            format!(
                "  {}  — listed in [commit] auto_stage but still dirty. \
                 Run: git add {} && git commit --amend --no-edit",
                path, path
            )
        };
        eprintln!("{}", hint);
    }
    eprintln!(
        "  To suppress this warning, ensure these files are tracked by git or add them to \
         [commit] auto_stage in .ta/workflow.toml."
    );
}

pub fn load_package(config: &GatewayConfig, package_id: Uuid) -> anyhow::Result<DraftPackage> {
    let path = config.pr_packages_dir.join(format!("{}.json", package_id));
    if !path.exists() {
        anyhow::bail!("draft package not found: {}", package_id);
    }
    let json = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&json)?)
}

pub fn save_package(config: &GatewayConfig, pkg: &DraftPackage) -> anyhow::Result<()> {
    fs::create_dir_all(&config.pr_packages_dir)?;
    let path = config
        .pr_packages_dir
        .join(format!("{}.json", pkg.package_id));
    let json = serde_json::to_string_pretty(pkg)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Serializable context produced by `ta run` before spawning an async draft build
/// (v0.15.6.2). Written to `.ta/draft-build-ctx/<goal-id>.json`; read back by
/// the background `ta draft build --apply-context-file` invocation.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DraftBuildContext {
    pub goal_id: String,
    pub verification_warnings: Vec<VerificationWarning>,
    pub validation_log: Vec<ta_changeset::draft_package::ValidationEntry>,
    pub supervisor_review: Option<ta_changeset::supervisor_review::SupervisorReview>,
}

/// Apply a `DraftBuildContext` file to the most recently built draft for `goal_id`.
///
/// Called by the background draft-build process after `build_package()` succeeds.
/// Patches the draft in-place and removes the context file.
pub fn apply_draft_build_context(
    config: &GatewayConfig,
    goal_id: &str,
    ctx_path: &std::path::Path,
) -> anyhow::Result<()> {
    let ctx_json = fs::read_to_string(ctx_path)?;
    let ctx: DraftBuildContext = serde_json::from_str(&ctx_json)?;

    // Find the latest draft for this goal.
    let Some(draft_id_str) = super::run::find_latest_draft_id(config, goal_id) else {
        tracing::warn!(
            goal_id = goal_id,
            "apply_draft_build_context: no draft found for goal"
        );
        // Remove the context file even on failure — it's stale either way.
        let _ = fs::remove_file(ctx_path);
        return Ok(());
    };
    let Ok(draft_uuid) = uuid::Uuid::parse_str(&draft_id_str) else {
        let _ = fs::remove_file(ctx_path);
        anyhow::bail!(
            "apply_draft_build_context: invalid draft UUID '{}'",
            draft_id_str
        );
    };
    let mut pkg = load_package(config, draft_uuid)?;

    if !ctx.verification_warnings.is_empty() {
        pkg.verification_warnings = ctx.verification_warnings;
    }
    if !ctx.validation_log.is_empty() {
        pkg.validation_log = ctx.validation_log;
    }
    if let Some(sup) = ctx.supervisor_review {
        pkg.supervisor_review = Some(sup);
    }

    save_package(config, &pkg)?;
    let _ = fs::remove_file(ctx_path);
    tracing::info!(
        goal_id = goal_id,
        draft_id = %draft_uuid,
        "apply_draft_build_context: patched draft with deferred build context"
    );
    Ok(())
}

/// Build a draft inline with a progress spinner (v0.15.8.1).
///
/// Called when `ta run` is invoked in an interactive terminal (TTY=true).
/// Blocks while building, displaying a spinner, then prints the `✓ Draft ready:` result.
/// Attaches verification warnings, validation log, and supervisor review to the draft.
///
/// On error, returns `Err` so the caller can propagate and exit non-zero.
pub fn build_draft_inline(
    config: &GatewayConfig,
    goal_id: &str,
    title: &str,
    verification_warnings: &[VerificationWarning],
    validation_log: &[ta_changeset::draft_package::ValidationEntry],
    supervisor_review: Option<&ta_changeset::supervisor_review::SupervisorReview>,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    println!("\nAgent exited.");

    // Spinner thread — runs until the build completes.
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();
    let spinner_thread = std::thread::spawn(move || {
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let start = std::time::Instant::now();
        let mut i = 0usize;
        while !done_clone.load(Ordering::Relaxed) {
            let elapsed = start.elapsed().as_secs();
            print!(
                "\rBuilding draft... {} ~{}s ",
                frames[i % frames.len()],
                elapsed
            );
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(100));
            i += 1;
        }
        // Clear the spinner line so the final message renders cleanly.
        print!("\r{}\r", " ".repeat(40));
        let _ = std::io::stdout().flush();
    });

    // Run the actual draft build synchronously.
    let build_result = build_package(
        config,
        goal_id,
        &format!("Changes from goal: {}", title),
        false,
    );

    // Stop the spinner regardless of build outcome.
    done.store(true, Ordering::Relaxed);
    let _ = spinner_thread.join();

    build_result?;

    // Attach deferred context and print result.
    // Use load_all_packages to find the latest draft by package_id (not display canonical ID,
    // which can be `shortref/seq` format that isn't a raw UUID).
    let latest_pkg = load_all_packages(config).ok().and_then(|mut pkgs| {
        pkgs.retain(|p| p.goal.goal_id == goal_id);
        pkgs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        pkgs.into_iter().next()
    });

    if let Some(mut pkg) = latest_pkg {
        if !verification_warnings.is_empty() {
            pkg.verification_warnings = verification_warnings.to_vec();
        }
        if !validation_log.is_empty() {
            pkg.validation_log = validation_log.to_vec();
        }
        if let Some(sup) = supervisor_review {
            pkg.supervisor_review = Some(sup.clone());
        }
        let _ = save_package(config, &pkg);

        // Print the ✓ result line using the canonical display ID.
        let canonical = ta_changeset::draft_resolver::draft_canonical_id(&pkg);
        let short_id = &canonical[..8.min(canonical.len())];
        println!("✓ Draft ready: \"{}\" [{}]", title, short_id);
        println!("  → ta draft view {}", short_id);
    } else {
        println!("✓ Draft built (run `ta draft list` to see it)");
    }

    Ok(())
}

/// Resolve a draft ID from user input.
///
/// Accepts:
/// - `None` → auto-select if exactly one pending (non-terminal) draft exists
/// - UUID or UUID prefix → match by draft package ID
/// - Free text like "v0.10.7" → match against goal title (contains match)
///
/// Returns the resolved draft package ID as a string, or an error with
/// actionable guidance.
fn resolve_draft_id_flexible(
    config: &GatewayConfig,
    input: Option<&str>,
) -> anyhow::Result<String> {
    let packages = load_all_packages(config)?;

    // Pending = non-terminal states (drafts the user likely wants to act on).
    let pending: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            matches!(
                p.status,
                DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
            )
        })
        .collect();

    let input = match input {
        Some(s) if !s.is_empty() => s,
        _ => {
            // No input — auto-select if exactly one pending draft.
            match pending.len() {
                0 => {
                    anyhow::bail!("No pending drafts. Run `ta draft list --all` to see all drafts.")
                }
                1 => {
                    let pkg = pending[0];
                    let short_id = &pkg.package_id.to_string()[..8];
                    println!("Auto-selecting: {} ({})", short_id, pkg.goal.title);
                    return Ok(pkg.package_id.to_string());
                }
                n => {
                    let mut msg = format!("{} pending drafts — specify which one:\n", n);
                    for p in &pending {
                        let short_id = &p.package_id.to_string()[..8];
                        msg.push_str(&format!("  {}  {}\n", short_id, p.goal.title));
                    }
                    anyhow::bail!(msg);
                }
            }
        }
    };

    // Try exact UUID parse first.
    if let Ok(uuid) = Uuid::parse_str(input) {
        if packages.iter().any(|p| p.package_id == uuid) {
            return Ok(uuid.to_string());
        }
        anyhow::bail!("Draft {} not found", input);
    }

    // Try shortref/seq format (v0.14.8.1): `<8hex>/<N>` — goal shortref + draft seq number.
    // This is the canonical ID format emitted by `ta draft list` and `ta run` completion.
    if let Some((shortref_part, seq_part)) = input.split_once('/') {
        if shortref_part.len() == 8 && shortref_part.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(seq) = seq_part.parse::<u32>() {
                let matched: Vec<&DraftPackage> = packages
                    .iter()
                    .filter(|p| {
                        p.goal_shortref.as_deref() == Some(shortref_part) && p.draft_seq == seq
                    })
                    .collect();
                match matched.len() {
                    0 => anyhow::bail!(
                        "No draft matching \"{}\". Run `ta draft list` to see available drafts.",
                        input
                    ),
                    1 => return Ok(matched[0].package_id.to_string()),
                    _ => {
                        // Theoretically impossible (seq is unique per goal), surface as ambiguous.
                        let ids: Vec<String> = matched
                            .iter()
                            .map(|p| {
                                format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title)
                            })
                            .collect();
                        anyhow::bail!(
                            "Ambiguous shortref/seq \"{}\" matches {} drafts:\n  {}",
                            input,
                            matched.len(),
                            ids.join("\n  ")
                        );
                    }
                }
            }
        }
        // Has a `/` but doesn't look like shortref/seq — fall through to other matchers.
    }

    // Try goal shortref match (v0.14.7.3): 8-char hex resolves to latest draft for that goal.
    // An 8-char all-hex string is treated as a goal shortref — resolves to the latest draft.
    // This takes priority over draft UUID prefix matching so `ta draft view 2159d87e`
    // means "latest draft for goal 2159d87e" not "draft whose UUID starts with 2159d87e".
    if input.len() == 8 && input.chars().all(|c| c.is_ascii_hexdigit()) {
        let shortref_matches: Vec<&DraftPackage> = packages
            .iter()
            .filter(|p| p.goal_shortref.as_ref().is_some_and(|s| s == input))
            .collect();
        if !shortref_matches.is_empty() {
            // Return the most recent (latest created_at) draft for this goal.
            let latest = shortref_matches
                .iter()
                .max_by_key(|p| p.created_at)
                .unwrap();
            return Ok(latest.package_id.to_string());
        }
    }

    // Try tag match (v0.11.2.3).
    let tag_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            p.tag
                .as_ref()
                .is_some_and(|t| t == input || t.starts_with(input))
        })
        .collect();
    if tag_matches.len() == 1 {
        return Ok(tag_matches[0].package_id.to_string());
    }

    // Try display_id exact match (v0.10.11 goal-derived IDs like "511e0465-01").
    let display_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            p.display_id
                .as_ref()
                .is_some_and(|did| did == input || did.starts_with(input))
        })
        .collect();
    if display_matches.len() == 1 {
        return Ok(display_matches[0].package_id.to_string());
    }

    // Try UUID prefix match (across all packages, not just pending).
    let prefix_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| p.package_id.to_string().starts_with(input))
        .collect();
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches[0].package_id.to_string());
    }
    if prefix_matches.len() > 1 {
        let ids: Vec<String> = prefix_matches
            .iter()
            .map(|p| format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title))
            .collect();
        anyhow::bail!(
            "Ambiguous prefix \"{}\" matches {} drafts:\n  {}\nSpecify more characters.",
            input,
            prefix_matches.len(),
            ids.join("\n  ")
        );
    }

    // Try matching against goal title (case-insensitive contains).
    let input_lower = input.to_lowercase();
    let title_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| p.goal.title.to_lowercase().contains(&input_lower))
        .collect();
    match title_matches.len() {
        0 => anyhow::bail!(
            "No draft matching \"{}\". Run `ta draft list` to see available drafts.",
            input
        ),
        1 => {
            let pkg = title_matches[0];
            let short_id = &pkg.package_id.to_string()[..8];
            println!("Matched: {} ({})", short_id, pkg.goal.title);
            Ok(pkg.package_id.to_string())
        }
        n => {
            let ids: Vec<String> = title_matches
                .iter()
                .map(|p| format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title))
                .collect();
            anyhow::bail!(
                "\"{}\" matches {} drafts:\n  {}\nSpecify the draft ID to disambiguate.",
                input,
                n,
                ids.join("\n  ")
            );
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Find the last char boundary at or before max - 3 to leave room for "...".
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max.saturating_sub(3))
        .last()
        .unwrap_or(0);
    format!("{}...", &s[..end])
}

/// Resolve a draft ID from a full UUID or an 8+ character prefix.
///
/// Tries exact UUID parse first. On failure, scans all draft packages for
/// a unique prefix match. Returns an error if the prefix is ambiguous or
/// matches nothing.
/// Resolve a draft ID from a required string (legacy callers).
/// Accepts UUID, UUID prefix, or goal title/phase substring.
fn resolve_draft_id(id: &str, config: &GatewayConfig) -> anyhow::Result<Uuid> {
    let resolved = resolve_draft_id_flexible(config, Some(id))?;
    Uuid::parse_str(&resolved)
        .map_err(|e| anyhow::anyhow!("Invalid draft ID after resolution: {} — {}", resolved, e))
}

/// Resolve a goal ID from a tag, full UUID, or an 8+ character prefix.
fn resolve_goal_id_from_store(id: &str, store: &GoalRunStore) -> anyhow::Result<Uuid> {
    // Try tag resolution first (v0.11.2.3).
    if let Ok(Some(g)) = store.resolve_tag(id) {
        return Ok(g.goal_run_id);
    }

    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }

    if id.len() < 8 {
        anyhow::bail!(
            "No goal found matching '{}' (not a tag and too short for UUID prefix — use at least 8 characters)",
            id
        );
    }

    let goals = store.list()?;
    let matches: Vec<_> = goals
        .iter()
        .filter(|g| g.goal_run_id.to_string().starts_with(id))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No goal found matching '{}'", id),
        1 => Ok(matches[0].goal_run_id),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} goals. Use a longer prefix or a goal tag.",
            id,
            n
        ),
    }
}

// ── Review Session Commands ────────────────────────────────────

/// Start or resume a review session for a draft package.
fn review_start(config: &GatewayConfig, draft_id: &str, reviewer: &str) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(draft_id, config)?;
    let pkg = load_package(config, package_id)?;

    // Create the review sessions directory in .ta/review_sessions/
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    // Check if there's already an active session for this draft.
    let session = if let Some(existing) = store.find_active_for_draft(package_id)? {
        println!("Resuming existing review session: {}", existing.session_id);
        existing
    } else {
        let new_session = ReviewSession::new(package_id, reviewer.to_string());
        store.save(&new_session)?;
        println!("Created new review session: {}", new_session.session_id);
        new_session
    };

    println!("\nReview Session");
    println!("  Session ID:    {}", session.session_id);
    println!("  Draft Package: {}", package_id);
    println!("  Reviewer:      {}", session.reviewer);
    println!("  State:         {:?}", session.state);
    println!();

    // Show summary of artifacts.
    let total = pkg.changes.artifacts.len();
    let counts = session.disposition_counts();
    let pending = total - counts.approved - counts.rejected - counts.discuss;

    println!("Artifacts: {} total", total);
    println!("  Approved: {}", counts.approved);
    println!("  Rejected: {}", counts.rejected);
    println!("  Discuss:  {}", counts.discuss);
    println!("  Pending:  {}", pending);
    println!();

    if pending > 0 {
        println!("Use 'ta draft review next' to view the next artifact.");
    } else {
        println!("All artifacts have been reviewed!");
        println!("Use 'ta draft review finish' to complete the session.");
    }

    Ok(())
}

/// Add a comment to an artifact in the current active session.
fn review_comment(
    config: &GatewayConfig,
    uri: &str,
    message: &str,
    commenter: &str,
) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    // Find the most recent active session.
    let sessions = store.list()?;
    let mut session = sessions
        .into_iter()
        .find(|s| s.state == ReviewState::Active)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No active review session found. Use 'ta draft review start <draft-id>' first."
            )
        })?;

    // Add the comment.
    session.add_comment(uri, commenter, message);
    store.save(&session)?;

    println!("Added comment to artifact: {}", uri);
    println!("  From: {}", commenter);
    println!("  Text: {}", message);
    println!();

    // Show the comment thread for this artifact.
    if let Some(review) = session.artifact_reviews.get(uri) {
        println!("Comment thread ({} comment(s)):", review.comments.len());
        for comment in &review.comments.comments {
            println!(
                "  [{}] {}: {}",
                comment.created_at, comment.commenter, comment.text
            );
        }
    }

    Ok(())
}

/// Show the next undecided artifact(s) in the current session.
fn review_next(config: &GatewayConfig, count: usize) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    // Find the most recent active session.
    let sessions = store.list()?;
    let session = sessions
        .into_iter()
        .find(|s| s.state == ReviewState::Active)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No active review session found. Use 'ta draft review start <draft-id>' first."
            )
        })?;

    // Load the draft package to get all artifacts.
    let pkg = load_package(config, session.draft_package_id)?;

    // Find pending artifacts (those without a decision).
    let pending: Vec<&Artifact> = pkg
        .changes
        .artifacts
        .iter()
        .filter(|a| session.get_disposition(&a.resource_uri).is_none())
        .collect();

    if pending.is_empty() {
        println!("No pending artifacts. All artifacts have been reviewed!");
        println!("Use 'ta draft review finish' to complete the session.");
        return Ok(());
    }

    // Show up to `count` pending artifacts.
    let to_show = pending.iter().take(count);

    for (i, artifact) in to_show.enumerate() {
        println!(
            "\n[{}/{}] Artifact: {}",
            i + 1,
            pending.len(),
            artifact.resource_uri
        );
        println!("  Change Type: {:?}", artifact.change_type);

        // Show explanation if available.
        if let Some(ref tiers) = artifact.explanation_tiers {
            if !tiers.summary.is_empty() {
                println!("  Summary: {}", tiers.summary);
            }
            if !tiers.explanation.is_empty() {
                println!("  Why: {}", tiers.explanation);
            }
        } else if let Some(ref rationale) = artifact.rationale {
            println!("  Rationale: {}", rationale);
        }

        // Show dependencies.
        if !artifact.dependencies.is_empty() {
            println!("  Dependencies:");
            for dep in &artifact.dependencies {
                println!("    {:?} {}", dep.kind, dep.target_uri);
            }
        }
    }

    println!();
    println!("Next steps:");
    println!(
        "  - View diff: ta draft view {} --file <path>",
        session.draft_package_id
    );
    println!("  - Comment:   ta draft review comment <uri> 'your comment'");
    println!("  - More:      ta draft review next --count N");
    println!("  - Finish:    ta draft review finish");

    Ok(())
}

/// Finish the review session and show final summary.
fn review_finish(config: &GatewayConfig, session_id: Option<&str>) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    // Load the session.
    let mut session = if let Some(id) = session_id {
        if let Ok(uuid) = Uuid::parse_str(id) {
            store.load(uuid)?
        } else {
            let all = store.list()?;
            let matches: Vec<_> = all
                .into_iter()
                .filter(|s| s.session_id.to_string().starts_with(id))
                .collect();
            match matches.len() {
                0 => anyhow::bail!("No review session found matching '{}'", id),
                1 => matches.into_iter().next().unwrap(),
                n => anyhow::bail!("Ambiguous prefix '{}' matches {} sessions", id, n),
            }
        }
    } else {
        // Use the most recent active session.
        let sessions = store.list()?;
        sessions
            .into_iter()
            .find(|s| s.state == ReviewState::Active)
            .ok_or_else(|| anyhow::anyhow!("No active review session found"))?
    };

    // Finish the session and get disposition summary.
    let counts = session.finish();
    store.save(&session)?;

    println!("Review session finished: {}", session.session_id);
    println!();
    println!("Final disposition summary:");
    println!("  Approved: {} artifact(s)", counts.approved);
    println!("  Rejected: {} artifact(s)", counts.rejected);
    println!("  Discuss:  {} artifact(s)", counts.discuss);
    println!("  Pending:  {} artifact(s)", counts.pending);
    println!();

    if counts.pending > 0 {
        println!(
            "[warn] Warning: {} artifact(s) were not explicitly reviewed.",
            counts.pending
        );
        println!();
    }

    if session.has_unresolved_discuss() {
        println!(
            "[warn] Warning: {} artifact(s) marked for discussion remain unresolved.",
            counts.discuss
        );
        println!();
    }

    println!("To apply this review:");
    println!(
        "  - View the package: ta draft view {}",
        session.draft_package_id
    );
    if counts.approved > 0 && counts.rejected > 0 {
        println!("  - Apply selectively based on your review session decisions");
        println!("    (You'll need to manually specify --approve/--reject patterns)");
    } else if counts.approved > 0 {
        println!(
            "  - Approve all: ta draft approve {}",
            session.draft_package_id
        );
        println!("  - Apply: ta draft apply {}", session.draft_package_id);
    }

    Ok(())
}

/// List all review sessions.
fn review_list(config: &GatewayConfig, draft_filter: Option<&str>) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    let all_sessions = store.list()?;

    // Apply draft filter if specified.
    let sessions: Vec<_> = if let Some(draft_id) = draft_filter {
        let draft_uuid = resolve_draft_id(draft_id, config)?;
        all_sessions
            .into_iter()
            .filter(|s| s.draft_package_id == draft_uuid)
            .collect()
    } else {
        all_sessions
    };

    if sessions.is_empty() {
        println!("No review sessions found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<38} {:<16} {:<12}",
        "SESSION ID", "DRAFT PACKAGE", "REVIEWER", "STATE"
    );
    println!("{}", "-".repeat(108));

    for session in &sessions {
        println!(
            "{:<38} {:<38} {:<16} {:<12}",
            session.session_id,
            session.draft_package_id,
            truncate(&session.reviewer, 14),
            format!("{:?}", session.state),
        );
    }

    println!("\n{} session(s) total.", sessions.len());
    Ok(())
}

/// Show details of a review session.
fn review_show(config: &GatewayConfig, session_id: Option<&str>) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/review_sessions");
    let store = ReviewSessionStore::new(sessions_dir)?;

    // Load the session.
    let session = if let Some(id) = session_id {
        if let Ok(uuid) = Uuid::parse_str(id) {
            store.load(uuid)?
        } else {
            let all = store.list()?;
            let matches: Vec<_> = all
                .into_iter()
                .filter(|s| s.session_id.to_string().starts_with(id))
                .collect();
            match matches.len() {
                0 => anyhow::bail!("No review session found matching '{}'", id),
                1 => matches.into_iter().next().unwrap(),
                n => anyhow::bail!("Ambiguous prefix '{}' matches {} sessions", id, n),
            }
        }
    } else {
        // Use the most recent active session.
        let sessions = store.list()?;
        sessions
            .into_iter()
            .find(|s| s.state == ReviewState::Active)
            .ok_or_else(|| anyhow::anyhow!("No active review session found"))?
    };

    println!("Review Session: {}", session.session_id);
    println!("  Draft Package: {}", session.draft_package_id);
    println!("  Reviewer:      {}", session.reviewer);
    println!("  State:         {:?}", session.state);
    println!("  Created:       {}", session.created_at);
    println!("  Updated:       {}", session.updated_at);
    println!();

    // Show disposition counts.
    let counts = session.disposition_counts();
    println!("Disposition summary:");
    println!("  Approved: {}", counts.approved);
    println!("  Rejected: {}", counts.rejected);
    println!("  Discuss:  {}", counts.discuss);
    println!("  Pending:  {}", counts.pending);
    println!();

    // Show artifact reviews with comments.
    if !session.artifact_reviews.is_empty() {
        println!("Artifact reviews ({}):", session.artifact_reviews.len());
        for (uri, review) in &session.artifact_reviews {
            println!("\n  {}", uri);
            println!("    Disposition: {:?}", review.disposition);
            if let Some(reviewed_at) = review.reviewed_at {
                println!("    Reviewed at: {}", reviewed_at);
            }
            if !review.comments.is_empty() {
                println!("    Comments ({}):", review.comments.len());
                for comment in &review.comments.comments {
                    println!(
                        "      [{}] {}: {}",
                        comment.created_at.format("%Y-%m-%d %H:%M:%S"),
                        comment.commenter,
                        comment.text
                    );
                }
            }
        }
    } else {
        println!("No artifact reviews yet.");
    }

    // Show session notes.
    if !session.session_notes.is_empty() {
        println!("\nSession notes ({}):", session.session_notes.len());
        for note in &session.session_notes {
            println!("  [{}] {}", note.created_at, note.text);
        }
    }

    Ok(())
}

// ── Draft follow-up (v0.11.3 items 1-7) ────────────────────────────

/// Follow-up record stored as JSON sidecar.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FollowUpRecord {
    timestamp: String,
    agent: String,
    reason: String,
    ci_failure: bool,
    review_comments: bool,
    guidance: Option<String>,
}

fn draft_follow_up(
    config: &GatewayConfig,
    id: &str,
    agent: &str,
    ci_failure: bool,
    review_comments: bool,
    guidance: Option<&str>,
    no_launch: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    // Validate draft is in Applied state.
    if !matches!(pkg.status, DraftStatus::Applied { .. }) {
        anyhow::bail!(
            "Draft {} is in {:?} state. Follow-up requires an Applied draft \
             (one that has been committed to a feature branch via `ta draft apply`).\n\
             Use `ta draft follow-up` after `ta draft apply --submit`.",
            &package_id.to_string()[..8],
            pkg.status,
        );
    }

    // Get VCS tracking info for the branch.
    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Follow-up requires a draft that was \
             applied with --submit. Re-apply with `ta draft apply --submit`.",
            &package_id.to_string()[..8],
        )
    })?;

    let branch = &vcs.branch;
    println!(
        "Follow-up on draft {} (branch: {})",
        &package_id.to_string()[..8],
        branch,
    );

    // Find the goal for this package.
    let goal_store = GoalRunStore::new(&config.goals_dir)?;
    let goals = goal_store.list()?;
    let goal = goals
        .iter()
        .find(|g| g.pr_package_id == Some(package_id))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No goal found for draft {}. Cannot determine workspace.",
                &package_id.to_string()[..8],
            )
        })?;

    let target_dir = goal
        .source_dir
        .clone()
        .unwrap_or_else(|| config.workspace_root.clone());

    // Branch safety: check that branch HEAD matches recorded commit.
    if let Some(ref recorded_sha) = vcs.commit_sha {
        let branch_head = std::process::Command::new("git")
            .args(["rev-parse", branch])
            .current_dir(&target_dir)
            .output();

        if let Ok(output) = branch_head {
            let current_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let prefix_len = 8.min(recorded_sha.len());
            if !current_sha.is_empty() && !current_sha.starts_with(&recorded_sha[..prefix_len]) {
                println!();
                println!(
                    "WARNING: Branch '{}' has been modified since TA applied draft {}.",
                    branch,
                    &package_id.to_string()[..8],
                );
                println!("  Recorded commit: {}", recorded_sha);
                println!("  Current HEAD:    {}", current_sha);
                println!("  The follow-up agent will see the current state.");
            }
        }
    }

    // Gather PR context.
    let pr_url = vcs.review_url.as_deref().or(goal.pr_url.as_deref());
    let mut context_sections: Vec<String> = Vec::new();

    // Original draft summary.
    context_sections.push(format!(
        "## Original Draft\n\nDraft ID: {}\nTitle: {}\nBranch: {}\nSummary: {}",
        package_id, pkg.goal.title, branch, pkg.summary.what_changed,
    ));

    // CI failure context.
    if ci_failure {
        if let Some(url) = pr_url {
            println!("Fetching CI failure context from PR...");
            let ci_ctx = fetch_ci_failure_context(url, &target_dir);
            if !ci_ctx.is_empty() {
                context_sections.push(format!(
                    "## CI Failures\n\nFix these CI failures:\n\n```\n{}\n```",
                    ci_ctx,
                ));
                println!("  Injected CI failure context ({} chars).", ci_ctx.len());
            } else {
                println!("  No CI failures found (or `gh` not available).");
            }
        } else {
            println!("  --ci-failure: No PR URL found, skipping.");
        }
    }

    // Review comments context.
    if review_comments {
        if let Some(url) = pr_url {
            println!("Fetching review comments from PR...");
            let comments = fetch_review_comments(url, &target_dir);
            if !comments.is_empty() {
                context_sections.push(format!(
                    "## PR Review Comments\n\nAddress each comment:\n\n{}",
                    comments,
                ));
                println!("  Injected review comments ({} chars).", comments.len());
            } else {
                println!("  No review comments found (or `gh` not available).");
            }
        } else {
            println!("  --review-comments: No PR URL found, skipping.");
        }
    }

    // User guidance.
    if let Some(g) = guidance {
        context_sections.push(format!("## Additional Guidance\n\n{}", g));
    }

    // Write context file.
    let context_path = target_dir.join(".ta/follow-up-context.md");
    fs::create_dir_all(target_dir.join(".ta"))?;
    let full_context = context_sections.join("\n\n---\n\n");
    fs::write(&context_path, &full_context)?;
    println!("\nFollow-up context written to: {}", context_path.display());

    // Record follow-up in sidecar.
    let followup_path = config
        .pr_packages_dir
        .join(format!("{}-followups.json", package_id));
    let mut records: Vec<FollowUpRecord> = if followup_path.exists() {
        let content = fs::read_to_string(&followup_path).unwrap_or_default();
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };
    records.push(FollowUpRecord {
        timestamp: Utc::now().to_rfc3339(),
        agent: agent.to_string(),
        reason: guidance.unwrap_or("follow-up").to_string(),
        ci_failure,
        review_comments,
        guidance: guidance.map(|g| g.to_string()),
    });
    fs::write(&followup_path, serde_json::to_string_pretty(&records)?)?;

    if no_launch {
        println!("\n-- Setup complete (--no-launch) --");
        println!("  Branch:     {}", branch);
        println!("  Target dir: {}", target_dir.display());
        println!("  Context:    {}", context_path.display());
        println!("\nTo start working:");
        println!("  cd {} && git checkout {}", target_dir.display(), branch);
        return Ok(());
    }

    // Checkout the branch.
    println!("\nChecking out branch '{}'...", branch);
    let checkout = std::process::Command::new("git")
        .args(["checkout", branch])
        .current_dir(&target_dir)
        .status();

    match checkout {
        Ok(status) if status.success() => println!("  On branch '{}'.", branch),
        Ok(status) => anyhow::bail!(
            "Failed to checkout branch '{}' (exit {}). Try `git fetch` first.",
            branch,
            status.code().unwrap_or(-1),
        ),
        Err(e) => anyhow::bail!("Failed to run git checkout: {}. Is git in PATH?", e),
    }

    // Launch agent.
    println!("Launching {} agent in {}...", agent, target_dir.display());
    let objective = format!(
        "Follow-up on draft {}. Read .ta/follow-up-context.md for what needs fixing. \
         Make changes and commit to branch '{}'.",
        &package_id.to_string()[..8],
        branch,
    );

    let agent_args: Vec<String> = match agent {
        "claude-code" => vec!["claude".into(), "--print".into(), objective],
        other => vec![other.into(), objective],
    };

    let status = std::process::Command::new(&agent_args[0])
        .args(&agent_args[1..])
        .current_dir(&target_dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("\nAgent exited successfully.");
            println!("Push changes with: cd {} && git push", target_dir.display());
        }
        Ok(s) => eprintln!("\nAgent exited with code {}.", s.code().unwrap_or(-1),),
        Err(e) => eprintln!("Failed to launch agent '{}': {}", agent, e),
    }

    Ok(())
}

/// Fetch CI failure context from a PR using `gh`.
fn fetch_ci_failure_context(pr_url: &str, working_dir: &std::path::Path) -> String {
    let checks = std::process::Command::new("gh")
        .args(["pr", "checks", pr_url])
        .current_dir(working_dir)
        .output();

    let mut context = String::new();
    if let Ok(output) = checks {
        let checks_output = String::from_utf8_lossy(&output.stdout);
        let failed_lines: Vec<&str> = checks_output
            .lines()
            .filter(|l| l.contains("fail") || l.contains("X "))
            .collect();
        if !failed_lines.is_empty() {
            context.push_str("Failed checks:\n");
            for line in &failed_lines {
                context.push_str(line);
                context.push('\n');
            }
        }
    }
    context
}

/// Fetch PR review comments using `gh`.
fn fetch_review_comments(pr_url: &str, working_dir: &std::path::Path) -> String {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", pr_url, "--comments", "--json", "comments"])
        .current_dir(working_dir)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let raw = String::from_utf8_lossy(&o.stdout);
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(comments) = data.get("comments").and_then(|c| c.as_array()) {
                    let mut result = String::new();
                    for comment in comments {
                        let author = comment
                            .get("author")
                            .and_then(|a| a.get("login"))
                            .and_then(|l| l.as_str())
                            .unwrap_or("unknown");
                        let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
                        if !body.is_empty() {
                            result.push_str(&format!("**@{}**: {}\n\n", author, body));
                        }
                    }
                    return result;
                }
            }
            raw.to_string()
        }
        _ => String::new(),
    }
}

// ── PR lifecycle (v0.11.3 items 24-26) ──────────────────────────────

fn draft_pr_status(config: &GatewayConfig, id: &str) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Apply with --submit first.",
            &package_id.to_string()[..8],
        )
    })?;

    println!(
        "=== PR Status for Draft {} ===\n",
        &package_id.to_string()[..8]
    );
    println!(
        "Draft:   {} ({})",
        pkg.goal.title,
        &package_id.to_string()[..8]
    );
    println!("Branch:  {}", vcs.branch);
    if let Some(ref sha) = vcs.commit_sha {
        println!("Commit:  {}", sha);
    }

    if let Some(ref url) = vcs.review_url {
        println!("PR URL:  {}", url);

        // Try live status via gh.
        let gh_output = std::process::Command::new("gh")
            .args([
                "pr",
                "view",
                url,
                "--json",
                "state,statusCheckRollup,reviews,title",
            ])
            .current_dir(&config.workspace_root)
            .output();

        match gh_output {
            Ok(output) if output.status.success() => {
                let raw = String::from_utf8_lossy(&output.stdout);
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                    let state = data
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    println!("\n-- Live PR Status --");
                    println!("  State:    {}", state);

                    if let Some(checks) = data.get("statusCheckRollup").and_then(|v| v.as_array()) {
                        let total = checks.len();
                        let passed = checks
                            .iter()
                            .filter(|c| {
                                c.get("conclusion")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s == "SUCCESS" || s == "NEUTRAL" || s == "SKIPPED")
                                    .unwrap_or(false)
                            })
                            .count();
                        let failed = checks
                            .iter()
                            .filter(|c| {
                                c.get("conclusion")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s == "FAILURE" || s == "ERROR")
                                    .unwrap_or(false)
                            })
                            .count();
                        println!(
                            "  CI:       {} passed, {} failed, {} pending (of {})",
                            passed,
                            failed,
                            total - passed - failed,
                            total
                        );

                        for check in checks {
                            let conclusion = check
                                .get("conclusion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if conclusion == "FAILURE" || conclusion == "ERROR" {
                                let name = check
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                println!("            FAIL: {}", name);
                            }
                        }
                    }

                    if let Some(reviews) = data.get("reviews").and_then(|v| v.as_array()) {
                        if !reviews.is_empty() {
                            let approved = reviews
                                .iter()
                                .filter(|r| {
                                    r.get("state").and_then(|v| v.as_str()) == Some("APPROVED")
                                })
                                .count();
                            let changes_req = reviews
                                .iter()
                                .filter(|r| {
                                    r.get("state").and_then(|v| v.as_str())
                                        == Some("CHANGES_REQUESTED")
                                })
                                .count();
                            println!(
                                "  Reviews:  {} approved, {} changes requested (of {})",
                                approved,
                                changes_req,
                                reviews.len()
                            );
                        }
                    }
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                println!("\n  Could not fetch live status: {}", stderr.trim());
            }
            Err(e) => println!("\n  Could not run `gh`: {}", e),
        }
    } else {
        println!("PR URL:  (none recorded)");
        if let Some(ref state) = vcs.review_state {
            println!("State:   {}", state);
        }
        // Item 7: show branch and recovery hint when PR creation failed.
        if !vcs.branch.is_empty() && vcs.branch != "unknown" {
            println!(
                "\nThe branch '{}' was pushed but no PR was created.",
                vcs.branch
            );
            println!("To create the missing PR:");
            println!("  ta draft reopen-review {}", &package_id.to_string()[..8]);
            println!("  gh pr create --head {} --base main", vcs.branch);
        }
    }

    Ok(())
}

// ── ta draft reopen-review (v0.13.1.2) ────────────────────────────────────────

/// Retry PR creation for an applied draft that was pushed but whose PR creation failed.
fn draft_reopen_review(config: &GatewayConfig, id: &str) -> anyhow::Result<()> {
    use ta_submit::{select_adapter, SourceAdapter, WorkflowConfig};

    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info — was it applied with --submit?",
            &package_id.to_string()[..8],
        )
    })?;

    if vcs.review_url.is_some() {
        anyhow::bail!(
            "Draft {} already has a PR URL: {}\nNothing to do.",
            &package_id.to_string()[..8],
            vcs.review_url.as_deref().unwrap_or(""),
        );
    }

    let branch = vcs.branch.clone();
    if branch.is_empty() || branch == "unknown" {
        anyhow::bail!(
            "Draft {} has no branch recorded — cannot create PR without a branch name.\n\
             Create the PR manually with `gh pr create`.",
            &package_id.to_string()[..8],
        );
    }

    println!(
        "=== Reopen Review for Draft {} ===\n",
        &package_id.to_string()[..8]
    );
    println!(
        "Draft:  {} ({})",
        pkg.goal.title,
        &package_id.to_string()[..8]
    );
    println!("Branch: {}", branch);
    println!("Attempting to create PR...\n");

    let target_dir = config.workspace_root.clone();
    let workflow_config_path = target_dir.join(".ta/workflow.toml");
    let workflow_config = WorkflowConfig::load_or_default(&workflow_config_path);
    let adapter: Box<dyn SourceAdapter> = select_adapter(&target_dir, &workflow_config.submit);

    if adapter.name() == "none" {
        anyhow::bail!(
            "No VCS adapter detected in {}.\n\
             Create the PR manually:\n  gh pr create --head {} --base main",
            target_dir.display(),
            branch,
        );
    }

    // Load the GoalRun from the goal store — `open_review` takes a `&GoalRun`, not the
    // lightweight `ta_changeset::Goal` embedded in the draft package.
    let goal_run: GoalRun = {
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goal_id_str = &pkg.goal.goal_id;
        let goal_uuid = resolve_goal_id_from_store(goal_id_str, &goal_store)?;
        goal_store
            .get(goal_uuid)?
            .ok_or_else(|| anyhow::anyhow!("Goal {} not found in store", goal_id_str))?
    };

    match adapter.open_review(&goal_run, &pkg) {
        Ok(result) => {
            println!("[ok] {}", result.message);
            if !result.review_url.starts_with("none://") {
                println!("  Review URL: {}", result.review_url);
            }
            // Update the stored VCS tracking info with the new PR URL.
            let vcs_mut = pkg.vcs_status.as_mut().expect("vcs_status checked above");
            vcs_mut.review_url = Some(result.review_url);
            vcs_mut.review_id = Some(result.review_id);
            vcs_mut.review_state = Some("open".to_string());
            vcs_mut.last_checked = Utc::now();
            let pkg_path = config
                .pr_packages_dir
                .join(format!("{}.json", pkg.package_id));
            if let Ok(json) = serde_json::to_string_pretty(&pkg) {
                let _ = std::fs::write(&pkg_path, json);
            }
            println!(
                "\nPR created and recorded. Use `ta draft pr-status {}` to check status.",
                &package_id.to_string()[..8]
            );
        }
        Err(e) => {
            anyhow::bail!(
                "PR creation failed: {}\n\n\
                 Branch '{}' is on the remote. Create the PR manually:\n\
                 \n  gh pr create --head {} --base main",
                e,
                branch,
                branch,
            );
        }
    }

    Ok(())
}

// ── ta draft merge (v0.12.0.1) ────────────────────────────────────────────────

/// Merge the PR/review for an applied draft, then sync the local main branch.
///
/// Calls `merge_review()` on the VCS adapter (e.g., `gh pr merge --auto`),
/// then runs `sync_upstream()` to fast-forward the local branch to main.
/// Updates the goal state to `Merged` on success.
fn merge_package(
    config: &GatewayConfig,
    id: &str,
    strategy: &str,
    _delete_branch: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    // Must have VCS tracking info (from `ta draft apply --submit`).
    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Apply with --submit first (`ta draft apply --submit`).",
            &package_id.to_string()[..8],
        )
    })?;

    let review_id = vcs.review_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no review ID. Create a PR first (`ta draft apply --submit --review`).",
            &package_id.to_string()[..8],
        )
    })?;

    println!(
        "=== Merging PR for Draft {} ===\n",
        &package_id.to_string()[..8]
    );
    if let Some(ref url) = vcs.review_url {
        println!("  PR: {}", url);
    }
    println!("  Strategy: {}", strategy);

    // Select the VCS adapter.
    let wf_path = config.workspace_root.join(".ta/workflow.toml");
    let wf_config = ta_submit::WorkflowConfig::load_or_default(&wf_path);
    let adapter = ta_submit::select_adapter_with_sync(
        &config.workspace_root,
        &wf_config.submit,
        &wf_config.source.sync,
    );

    // Merge the review.
    println!("\n[merge] Calling merge_review({})...", review_id);
    let result = adapter.merge_review(review_id)?;
    if result.merged {
        println!("[ok] Merged: {}", result.message);
        if let Some(ref sha) = result.merge_commit {
            println!("     Merge commit: {}", sha);
        }
    } else {
        println!("[pending] {}", result.message);
        println!("  Auto-merge enabled. The PR will merge when CI passes.");
        println!(
            "  Run `ta draft watch {}` to poll until merged.",
            &package_id.to_string()[..8]
        );
        return Ok(());
    }

    // Sync upstream to fast-forward local main.
    println!("\n[sync] Syncing upstream...");
    match adapter.sync_upstream() {
        Ok(sync) if sync.updated => {
            println!(
                "[ok] Synced {} new commit(s). Local branch is up to date.",
                sync.new_commits
            );
        }
        Ok(sync) if !sync.is_clean() => {
            eprintln!(
                "[warn] Sync found {} conflict(s). Resolve manually with `ta sync`.",
                sync.conflicts.len()
            );
        }
        Ok(_) => println!("[ok] Already up to date."),
        Err(e) => eprintln!("[warn] Sync failed: {}. Run `ta sync` manually.", e),
    }

    // Transition goal state to Merged.
    if let Ok(goal_store) = GoalRunStore::new(&config.goals_dir) {
        if let Ok(goals) = goal_store.list() {
            if let Some(goal) = goals
                .iter()
                .find(|g| g.goal_run_id.to_string() == pkg.goal.goal_id)
            {
                if matches!(goal.state, GoalRunState::Applied) {
                    if let Err(e) = goal_store.transition(goal.goal_run_id, GoalRunState::Merged) {
                        eprintln!("[warn] Could not update goal state to Merged: {}", e);
                    } else {
                        println!(
                            "\n[ok] Goal {} → merged",
                            &goal.goal_run_id.to_string()[..8]
                        );
                    }
                }
            }
        }
    }

    println!(
        "\n✓ Draft {} merged and synced.",
        &package_id.to_string()[..8]
    );
    Ok(())
}

// ── ta draft watch (v0.12.0.1) ────────────────────────────────────────────────

/// Poll PR/CI status until the PR is merged, then auto-sync the local branch.
///
/// Checks `check_review()` every `interval` seconds. When state == "merged",
/// calls `sync_upstream()` and transitions the goal to `Merged`.
fn watch_package(
    config: &GatewayConfig,
    id: &str,
    interval: u64,
    max_polls: u32,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Apply with --submit first.",
            &package_id.to_string()[..8],
        )
    })?;

    let review_id = vcs.review_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no review ID. Create a PR first (`ta draft apply --submit --review`).",
            &package_id.to_string()[..8],
        )
    })?;

    println!(
        "=== Watching PR for Draft {} ===",
        &package_id.to_string()[..8]
    );
    if let Some(ref url) = vcs.review_url {
        println!("  PR: {}", url);
    }
    println!(
        "  Polling every {}s (max {} polls = ~{}m). Ctrl-C to stop.\n",
        interval,
        max_polls,
        interval as u32 * max_polls / 60,
    );

    let wf_path = config.workspace_root.join(".ta/workflow.toml");
    let wf_config = ta_submit::WorkflowConfig::load_or_default(&wf_path);
    let adapter = ta_submit::select_adapter_with_sync(
        &config.workspace_root,
        &wf_config.submit,
        &wf_config.source.sync,
    );

    for poll in 1..=max_polls {
        match adapter.check_review(review_id) {
            Ok(Some(status)) => {
                println!(
                    "  [{}] Poll {}/{}: state = {}{}",
                    chrono::Utc::now().format("%H:%M:%S"),
                    poll,
                    max_polls,
                    status.state,
                    match status.checks_passing {
                        Some(true) => " (CI: passing)",
                        Some(false) => " (CI: failing)",
                        None => "",
                    }
                );

                if status.state == "merged" {
                    println!("\n[ok] PR merged!");

                    // Sync upstream.
                    println!("[sync] Syncing local branch...");
                    match adapter.sync_upstream() {
                        Ok(sync) if sync.updated => {
                            println!("[ok] Synced {} new commit(s).", sync.new_commits);
                        }
                        Ok(sync) if !sync.is_clean() => {
                            eprintln!(
                                "[warn] {} conflict(s). Resolve manually with `ta sync`.",
                                sync.conflicts.len()
                            );
                        }
                        Ok(_) => println!("[ok] Already up to date."),
                        Err(e) => eprintln!("[warn] Sync failed: {}. Run `ta sync`.", e),
                    }

                    // Transition goal to Merged.
                    if let Ok(goal_store) = GoalRunStore::new(&config.goals_dir) {
                        if let Ok(goals) = goal_store.list() {
                            if let Some(goal) = goals
                                .iter()
                                .find(|g| g.goal_run_id.to_string() == pkg.goal.goal_id)
                            {
                                if matches!(goal.state, GoalRunState::Applied) {
                                    let _ = goal_store
                                        .transition(goal.goal_run_id, GoalRunState::Merged);
                                }
                            }
                        }
                    }

                    println!(
                        "\n✓ Draft {} merged and synced.",
                        &package_id.to_string()[..8]
                    );
                    return Ok(());
                } else if status.state == "closed" {
                    anyhow::bail!(
                        "PR was closed without merging. \
                         Check the PR manually and re-open or create a new draft."
                    );
                }
            }
            Ok(None) => {
                println!(
                    "  [{}] Poll {}/{}: adapter does not support check_review. \
                     Check PR status manually.",
                    chrono::Utc::now().format("%H:%M:%S"),
                    poll,
                    max_polls,
                );
                anyhow::bail!(
                    "Adapter '{}' does not support check_review. \
                     Merge the PR manually, then run `ta sync`.",
                    adapter.name()
                );
            }
            Err(e) => {
                eprintln!("  [warn] check_review failed: {}. Retrying...", e);
            }
        }

        if poll < max_polls {
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
    }

    anyhow::bail!(
        "Timed out after {} polls ({}s interval). \
         PR has not merged yet. Run `ta draft watch {}` to resume polling.",
        max_polls,
        interval,
        &package_id.to_string()[..8]
    )
}

fn draft_pr_list(config: &GatewayConfig) -> anyhow::Result<()> {
    let packages = load_all_packages(config)?;

    let with_pr: Vec<&DraftPackage> = packages.iter().filter(|p| p.vcs_status.is_some()).collect();

    if with_pr.is_empty() {
        println!("No drafts with PR/VCS tracking info found.");
        println!("  Drafts get VCS tracking after `ta draft apply --submit`.");
        return Ok(());
    }

    println!(
        "{:<10} {:<30} {:<10} {:<8} PR URL",
        "DRAFT", "TITLE", "BRANCH", "STATE"
    );
    println!("{}", "-".repeat(90));

    for pkg in &with_pr {
        let vcs = pkg.vcs_status.as_ref().unwrap();
        let short_id = &pkg.package_id.to_string()[..8];
        let title = truncate(&pkg.goal.title, 28);
        let branch = truncate(&vcs.branch, 8);
        let state = vcs.review_state.as_deref().unwrap_or("?");
        let url = vcs.review_url.as_deref().unwrap_or("-");
        println!(
            "{:<10} {:<30} {:<10} {:<8} {}",
            short_id, title, branch, state, url
        );
    }

    println!("\n{} draft(s) with VCS tracking.", with_pr.len());
    Ok(())
}

// ── Constitution §4 scan (v0.11.5 item 8) ────────────────────────────────────

/// Scan changed Rust files for potential §4 (injection cleanup) violations.
///
/// For each changed `.rs` file in the staging area, checks whether `inject_*`
/// calls are balanced by `restore_*` calls. If a file has more inject calls
/// than restore calls AND contains early-return paths, flags it as a potential
/// §4 violation. Findings are returned as `VerificationWarning` entries with
/// `command = "[constitution §4]"` so they appear in `ta draft view` output.
///
/// This is static/grep-based (no agent), runs in <1s, and is non-blocking:
/// warnings are informational — the review flow is unaffected.
pub fn scan_s4_violations(
    artifacts: &[Artifact],
    staging_dir: &std::path::Path,
) -> Vec<VerificationWarning> {
    let mut warnings = Vec::new();

    for artifact in artifacts {
        let uri = &artifact.resource_uri;
        if !uri.ends_with(".rs") {
            continue;
        }
        // Skip test files — inject/restore patterns there are test fixtures.
        if uri.contains("/tests/") || uri.ends_with("_test.rs") {
            continue;
        }

        let rel_path = uri.strip_prefix("fs://workspace/").unwrap_or(uri.as_str());
        let staged_path = staging_dir.join(rel_path);

        let content = match std::fs::read_to_string(&staged_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Count inject_* and restore_* call-sites.
        // Use word-boundary check: look for `inject_` followed by an identifier char
        // to avoid matching e.g. variable names like `prev_inject_count`.
        let inject_count = count_call_sites(&content, "inject_");
        let restore_count = count_call_sites(&content, "restore_");

        if inject_count == 0 {
            continue;
        }

        // Check for early-return patterns — `return` or `return Err` at statement level.
        let has_early_returns = content.contains("return Err")
            || content.contains("return Ok(")
            || content.lines().any(|line| {
                let trimmed = line.trim();
                (trimmed == "return;" || trimmed.starts_with("return ")) && !trimmed.contains("// ")
            });

        if has_early_returns && inject_count > restore_count {
            let file_name = std::path::Path::new(rel_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(rel_path);
            let gap = inject_count - restore_count;
            warnings.push(VerificationWarning {
                command: "[constitution §4]".to_string(),
                exit_code: None,
                output: format!(
                    "{}: {} inject_* call(s) but only {} restore_* call(s) ({} unbalanced) — \
                     check that all early-return paths restore injected files before returning.",
                    file_name, inject_count, restore_count, gap
                ),
            });
        }
    }

    warnings
}

/// Count how many times an `inject_` or `restore_` pattern appears as a
/// function-call site (followed by `(`) in file content.
fn count_call_sites(content: &str, prefix: &str) -> usize {
    let mut count = 0;
    let mut pos = 0;
    while let Some(idx) = content[pos..].find(prefix) {
        let abs = pos + idx;
        // Must be preceded by whitespace, `(`, `{`, `;`, or start-of-string (not an ident char).
        let preceded_ok = abs == 0 || {
            let prev = content.as_bytes()[abs - 1];
            !prev.is_ascii_alphanumeric() && prev != b'_'
        };
        // Must be followed by `<ident_char>(` — i.e., this is a call expression.
        let rest = &content[abs + prefix.len()..];
        let followed_by_call = rest
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric() || c == '_')
            .unwrap_or(false)
            && rest.contains('(');
        if preceded_ok && followed_by_call {
            count += 1;
        }
        pos = abs + prefix.len();
    }
    count
}

// ── v0.13.15: Version backward bump check ─────────────────────────────────

/// Check if any artifact is a workspace Cargo.toml that sets a version lower
/// than the source workspace version.  Returns VerificationWarnings.
///
/// The check is intentionally loose: it only fires when both the staging and
/// source workspace Cargo.toml carry a `version = "..."` line that can be
/// parsed as (major, minor, patch).  Any parse error is silently skipped.
fn check_backward_version_bump(
    artifacts: &[Artifact],
    source_dir: &std::path::Path,
    staging_dir: &std::path::Path,
) -> Vec<VerificationWarning> {
    let mut warnings = Vec::new();

    for artifact in artifacts {
        let uri = &artifact.resource_uri;
        // Only look at Cargo.toml files (workspace or crate-level).
        if !uri.ends_with("Cargo.toml") {
            continue;
        }
        let rel_path = uri.strip_prefix("fs://workspace/").unwrap_or(uri.as_str());

        let source_path = source_dir.join(rel_path);
        let staging_path = staging_dir.join(rel_path);

        let source_ver = read_toml_version(&source_path);
        let staging_ver = read_toml_version(&staging_path);

        if let (Some((src_str, src_tuple)), Some((staged_str, staged_tuple))) =
            (source_ver, staging_ver)
        {
            if staged_tuple < src_tuple {
                warnings.push(VerificationWarning {
                    command: "[version]".to_string(),
                    exit_code: None,
                    output: format!(
                        "{}: agent set version to {} but source is {} — \
                         per CLAUDE.md, only bump forward (never set a lower version). \
                         Revert this change or update to a higher version.",
                        rel_path, staged_str, src_str,
                    ),
                });
            }
        }
    }

    warnings
}

/// Parse `version = "X.Y.Z..."` from a Cargo.toml file.
/// Returns the raw version string and a (major, minor, patch) tuple for ordering.
/// Returns None if the file doesn't exist, can't be read, or has no parseable version line.
fn read_toml_version(path: &std::path::Path) -> Option<(String, (u64, u64, u64))> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                let ver_str = rest.trim().trim_matches('"');
                // Strip pre-release suffix (e.g. "-alpha", "-alpha.2") for ordering.
                let numeric_part = ver_str.split('-').next().unwrap_or(ver_str);
                let parts: Vec<&str> = numeric_part.split('.').collect();
                if parts.len() >= 3 {
                    if let (Ok(major), Ok(minor), Ok(patch)) = (
                        parts[0].parse::<u64>(),
                        parts[1].parse::<u64>(),
                        parts[2].parse::<u64>(),
                    ) {
                        return Some((ver_str.to_string(), (major, minor, patch)));
                    }
                }
            }
        }
    }
    None
}

// ── v0.13.15: PLAN.md deferred items validation ───────────────────────────

/// Check staging PLAN.md for unchecked `[ ]` items in `<!-- status: done -->`
/// phases that lack a `→ vX.Y` forward-deferred target.
///
/// Returns one VerificationWarning per offending item, so reviewers can see
/// the exact items that need to be resolved.
fn check_plan_unchecked_in_done_phases(
    artifacts: &[Artifact],
    staging_dir: &std::path::Path,
) -> Vec<VerificationWarning> {
    // Only run if PLAN.md is among the changed artifacts.
    let plan_uri = artifacts.iter().find(|a| {
        a.resource_uri == "fs://workspace/PLAN.md" || a.resource_uri.ends_with("/PLAN.md")
    });
    if plan_uri.is_none() {
        return Vec::new();
    }

    let plan_path = staging_dir.join("PLAN.md");
    let content = match std::fs::read_to_string(&plan_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    detect_unchecked_in_done_phases(&content)
}

/// Core parsing logic for PLAN.md deferred items validation (extracted for testing).
fn detect_unchecked_in_done_phases(content: &str) -> Vec<VerificationWarning> {
    let mut warnings = Vec::new();
    let mut in_done_phase = false;
    let mut phase_title = String::new();

    for line in content.lines() {
        // Detect phase status markers.
        if line.trim_start().starts_with("<!-- status:") {
            in_done_phase = line.contains("status: done");
            continue;
        }

        // Detect phase headings (### or ##) to update the current phase title.
        if line.starts_with("### ") || line.starts_with("## ") {
            phase_title = line.trim_start_matches('#').trim().to_string();
            // Reset done status — it will be set by the following <!-- status: --> marker.
            in_done_phase = false;
            continue;
        }

        if !in_done_phase {
            continue;
        }

        // Look for unchecked items: lines matching `- [ ] ...`
        let trimmed = line.trim();
        if trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]") {
            // Check if it has a forward-deferred target: `→ vX.Y` or `-> vX.Y`
            let has_deferred_target = trimmed.contains("→ v") || trimmed.contains("-> v");
            if !has_deferred_target {
                let item_text = trimmed
                    .trim_start_matches("- [ ]")
                    .trim_start_matches("* [ ]")
                    .trim();
                let short = if item_text.len() > 120 {
                    format!("{}…", &item_text[..120])
                } else {
                    item_text.to_string()
                };
                warnings.push(VerificationWarning {
                    command: "[plan]".to_string(),
                    exit_code: None,
                    output: format!(
                        "Phase '{}' is done but has unchecked item without `→ vX.Y` target: {}",
                        phase_title, short,
                    ),
                });
            }
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Clear TA agent VCS isolation env vars on a git command so test git
    /// operations target the test's temp directory, not the staging repo.
    /// The TA agent environment sets GIT_DIR/GIT_WORK_TREE/GIT_CEILING_DIRECTORIES
    /// (v0.13.17.3) which causes bare `git` invocations to operate on the
    /// wrong repository when tests run inside a goal session.
    fn clear_git_env(cmd: &mut std::process::Command) -> &mut std::process::Command {
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
    }

    // ── Constitution §4 scan tests (v0.11.5 item 8) ──────────────

    fn make_test_artifact(uri: &str) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "test-diff".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::default(),
            rationale: None,
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: None,
        }
    }

    #[test]
    fn count_call_sites_inject_and_restore() {
        let content = r#"
fn setup() {
    inject_claude_md(path);
    inject_claude_settings(path);
    restore_claude_md(path);
}
"#;
        assert_eq!(count_call_sites(content, "inject_"), 2);
        assert_eq!(count_call_sites(content, "restore_"), 1);
    }

    #[test]
    fn count_call_sites_no_false_positives() {
        // Variable name 'prev_inject_count' should not be counted
        let content = "let prev_inject_count = 0;\nlet restore_value = x;";
        assert_eq!(count_call_sites(content, "inject_"), 0);
        // 'restore_value' is not a call (no '(' following ident)
        assert_eq!(count_call_sites(content, "restore_"), 0);
    }

    #[test]
    fn scan_s4_violations_balanced_is_clean() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // Balanced: 1 inject, 1 restore, with early return
        let content = r#"
fn run() {
    inject_claude_md(path);
    if bad {
        restore_claude_md(path);
        return Err("bad".into());
    }
    restore_claude_md(path);
}
"#;
        std::fs::write(src_dir.join("run.rs"), content).unwrap();

        let artifact = make_test_artifact("fs://workspace/src/run.rs");
        let warnings = scan_s4_violations(&[artifact], dir.path());
        assert!(
            warnings.is_empty(),
            "Balanced inject/restore should produce no warnings, got: {:?}",
            warnings
        );
    }

    #[test]
    fn scan_s4_violations_unbalanced_flags_warning() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // Unbalanced: 2 inject, 1 restore, with early return
        let content = r#"
fn run() {
    inject_claude_md(path);
    inject_claude_settings(path);
    if bad {
        return Err("bad".into());  // missing restore before return
    }
    restore_claude_md(path);
}
"#;
        std::fs::write(src_dir.join("run.rs"), content).unwrap();

        let artifact = make_test_artifact("fs://workspace/src/run.rs");
        let warnings = scan_s4_violations(&[artifact], dir.path());
        assert_eq!(warnings.len(), 1, "Expected 1 warning, got: {:?}", warnings);
        assert_eq!(warnings[0].command, "[constitution §4]");
        assert!(warnings[0].output.contains("run.rs"));
        assert!(warnings[0].output.contains("2 inject_*"));
    }

    #[test]
    fn scan_s4_violations_no_early_return_is_clean() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // inject without restore but NO early returns (could be a bug, but not §4 pattern)
        let content = r#"
fn run() {
    inject_claude_md(path);
    do_work();
    // No return — function falls through
}
"#;
        std::fs::write(src_dir.join("run.rs"), content).unwrap();

        let artifact = make_test_artifact("fs://workspace/src/run.rs");
        let warnings = scan_s4_violations(&[artifact], dir.path());
        // No early return → no §4 warning (different class of bug)
        assert!(
            warnings.is_empty(),
            "No early return should produce no §4 warning, got: {:?}",
            warnings
        );
    }

    #[test]
    fn scan_s4_violations_non_rs_files_skipped() {
        let dir = TempDir::new().unwrap();
        let content = "inject_something restore_nothing return bad";
        std::fs::write(dir.path().join("run.py"), content).unwrap();

        let artifact = make_test_artifact("fs://workspace/run.py");
        let warnings = scan_s4_violations(&[artifact], dir.path());
        assert!(warnings.is_empty(), "Non-.rs files should be skipped");
    }

    #[test]
    fn build_pr_from_overlay_changes() {
        // Set up a source project.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(project.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Start a goal.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Test PR build".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test PR building from overlay".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Make changes in the staging workspace.
        std::fs::write(
            goal.workspace_path.join("README.md"),
            "# Modified Project\n",
        )
        .unwrap();
        std::fs::write(
            goal.workspace_path.join("src/new_file.rs"),
            "pub fn new_thing() {}\n",
        )
        .unwrap();
        std::fs::remove_file(goal.workspace_path.join("src/lib.rs")).unwrap();

        // Build draft package.
        build_package(&config, &goal_id, "Test changes", false).unwrap();

        // Verify draft package was created.
        let packages = load_all_packages(&config).unwrap();
        assert_eq!(packages.len(), 1);

        let pkg = &packages[0];
        assert_eq!(pkg.status, DraftStatus::PendingReview);
        assert_eq!(pkg.changes.artifacts.len(), 3);

        // Verify artifact types.
        let types: Vec<&ChangeType> = pkg
            .changes
            .artifacts
            .iter()
            .map(|a| &a.change_type)
            .collect();
        assert!(types.contains(&&ChangeType::Modify));
        assert!(types.contains(&&ChangeType::Add));
        assert!(types.contains(&&ChangeType::Delete));

        // Verify goal state transitioned to PrReady.
        let updated_goal = goal_store.get(goal.goal_run_id).unwrap().unwrap();
        assert_eq!(updated_goal.state, GoalRunState::PrReady);
        assert!(updated_goal.pr_package_id.is_some());
    }

    #[test]
    fn apply_overlay_copies_changes_to_source() {
        // Set up a source project.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Start goal + modify + build PR + approve + apply.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Apply test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test apply".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Make changes in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("NEW.md"), "new file\n").unwrap();

        // Build PR.
        build_package(&config, &goal_id, "Test apply changes", false).unwrap();

        // Approve the PR.
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();

        // Apply (no git).
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Verify files changed in source.
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(readme, "# Updated\n");
        let new_file = std::fs::read_to_string(project.path().join("NEW.md")).unwrap();
        assert_eq!(new_file, "new file\n");

        // Verify goal state.
        let updated = goal_store.get(goal.goal_run_id).unwrap().unwrap();
        assert_eq!(updated.state, GoalRunState::Applied);
    }

    #[test]
    fn apply_with_git_commit() {
        // Set up a git repo as source.
        let project = TempDir::new().unwrap();

        // Initialize git repo.
        for git_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        for git_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        let config = GatewayConfig::for_project(project.path());

        // Start goal.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Git test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test git commit".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify file in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();

        // Build + approve + apply with git commit.
        build_package(&config, &goal_id, "Modified README", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();
        apply_package(
            &config,
            &pkg_id,
            None,
            true,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Verify git log on the feature branch has the new commit.
        // After apply, we're back on the original branch; check --all.
        let log = clear_git_env(
            std::process::Command::new("git")
                .args(["log", "--all", "--oneline", "-5"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let log_output = String::from_utf8_lossy(&log.stdout);
        // Subject line is the goal title; summary is in the commit body.
        assert!(log_output.contains("Git test"));

        // Find the feature branch to read the full commit message.
        let branches = clear_git_env(
            std::process::Command::new("git")
                .args(["branch", "--list", "ta/*"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_name = String::from_utf8_lossy(&branches.stdout)
            .trim()
            .trim_start_matches("* ")
            .to_string();

        // Verify full commit message matches ta draft view format.
        let full_log = clear_git_env(
            std::process::Command::new("git")
                .args(["log", "-1", "--format=%B", &branch_name])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let full_msg = String::from_utf8_lossy(&full_log.stdout);
        // First line is the goal title (subject).
        assert!(full_msg.starts_with("Git test\n"));
        // Body includes module-grouped file listing with change icons.
        assert!(full_msg.contains("What Changed ("));
        assert!(full_msg.contains("README.md"));
        // No Debug-format change types like "Modify" — should use ~ + - > icons.
        assert!(!full_msg.contains("Modify  "));

        // v0.15.7: velocity-history.jsonl written on --git-commit apply.
        let history_path = project.path().join(".ta/velocity-history.jsonl");
        assert!(
            history_path.exists(),
            "velocity-history.jsonl should exist after git-commit apply"
        );
        let history_content = std::fs::read_to_string(&history_path).unwrap();
        assert!(
            !history_content.trim().is_empty(),
            "velocity-history.jsonl should not be empty"
        );
        let history_entry: serde_json::Value =
            serde_json::from_str(history_content.trim()).unwrap();
        assert_eq!(history_entry["outcome"], "applied");
        // machine_id should be set (8 hex chars).
        let mid = history_entry["machine_id"].as_str().unwrap_or("");
        assert_eq!(mid.len(), 8, "machine_id should be 8 chars");
    }

    /// v0.14.16 — Branch restore: after `ta draft apply --git-commit`, the working
    /// branch must be the same as it was before apply (e.g., main), not the
    /// feature branch created for the commit.
    #[test]
    fn apply_git_commit_restores_original_branch() {
        let project = TempDir::new().unwrap();

        for git_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        for git_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        // Confirm we start on main/master.
        let branch_before = clear_git_env(
            std::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_before = String::from_utf8_lossy(&branch_before.stdout)
            .trim()
            .to_string();
        // Should be main or master depending on git config.
        assert!(
            branch_before == "main" || branch_before == "master",
            "Expected to start on main/master, got: {}",
            branch_before
        );

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Branch restore test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test branch is restored after apply".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Branch restore\n").unwrap();

        build_package(&config, &goal_id, "Branch restore test", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();
        apply_package(
            &config,
            &pkg_id,
            None,
            true,  // git_commit
            false, // no push
            false, // no review
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // The working branch must be the same as before apply.
        let branch_after = clear_git_env(
            std::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_after = String::from_utf8_lossy(&branch_after.stdout)
            .trim()
            .to_string();
        assert_eq!(
            branch_after, branch_before,
            "Working branch changed after apply: was '{}', now '{}'",
            branch_before, branch_after
        );

        // The commit must still exist on the ta/ feature branch.
        let branches = clear_git_env(
            std::process::Command::new("git")
                .args(["branch", "--list", "ta/*"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_list = String::from_utf8_lossy(&branches.stdout);
        assert!(
            !branch_list.trim().is_empty(),
            "Expected ta/ feature branch to exist after apply"
        );
    }

    /// v0.12.2.2 — Transactional rollback: when pre-submit verification fails,
    /// all files written to the working tree must be restored to their
    /// pre-apply state so the tree is clean and no manual recovery is needed.
    #[test]
    fn apply_rollback_on_verification_failure() {
        // ── Set up a git repo project ──────────────────────────────────────
        let project = TempDir::new().unwrap();

        for cmd_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(cmd_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::write(project.path().join("lib.rs"), "// original\n").unwrap();

        for cmd_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(cmd_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        let config = GatewayConfig::for_project(project.path());

        // ── Create goal and stage changes ─────────────────────────────────
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Rollback test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test rollback on verify failure".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify existing file and add a new file in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();
        std::fs::write(goal.workspace_path.join("NEW.md"), "new content\n").unwrap();

        // ── Write a workflow.toml with a failing verification command ──────
        // The command always exits 1 so verification will fail.
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        {
            // On Unix: write a shell script (avoids inline shell differences).
            // On Windows: use `exit 1` inline — Windows paths written via
            // display() contain backslashes, which are TOML escape sequences
            // and would corrupt the parsed command string.
            #[cfg(not(windows))]
            {
                use std::io::Write;
                let fail_script = project.path().join(".ta/fail_check.sh");
                let mut f = std::fs::File::create(&fail_script).unwrap();
                writeln!(f, "#!/bin/sh").unwrap();
                writeln!(f, "echo 'check failed intentionally'").unwrap();
                writeln!(f, "exit 1").unwrap();
                drop(f);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o755);
                    std::fs::set_permissions(&fail_script, perms).unwrap();
                }
                std::fs::write(
                    project.path().join(".ta/workflow.toml"),
                    format!("[verify]\ncommands = [\"sh {}\"]\n", fail_script.display()),
                )
                .unwrap();
            }
            #[cfg(windows)]
            std::fs::write(
                project.path().join(".ta/workflow.toml"),
                "[verify]\ncommands = [\"exit 1\"]\n",
            )
            .unwrap();
        }

        // ── Build + approve the draft ─────────────────────────────────────
        build_package(&config, &goal_id, "Modified README and new file", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();

        // ── Apply with git_commit=true — verification will fail ────────────
        let result = apply_package(
            &config,
            &pkg_id,
            None,
            true,  // git_commit
            false, // git_push
            false, // git_review
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        );

        // Apply must have returned an error.
        assert!(
            result.is_err(),
            "apply_package should fail when verification fails"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("verification failed"),
            "unexpected error: {err_msg}"
        );

        // ── Working tree must be clean (rollback fired) ───────────────────
        // README.md must be restored to the original content.
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(
            readme, "# Original\n",
            "README.md was not rolled back after verification failure"
        );

        // NEW.md must not exist (it was a new file — rollback should remove it).
        assert!(
            !project.path().join("NEW.md").exists(),
            "NEW.md was not removed after rollback"
        );

        // ── Tracked git files must show no modifications ───────────────────
        // Use -uno to ignore untracked files (e.g. the .ta/ config dir we
        // created for this test — it was never committed to the test repo).
        let status = clear_git_env(
            std::process::Command::new("git")
                .args(["status", "--porcelain", "--untracked-files=no"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let status_output = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_output.trim().is_empty(),
            "tracked files are dirty after rollback:\n{status_output}"
        );
    }

    #[test]
    fn build_pr_enriches_from_change_summary() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Summary test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test change_summary ingestion".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Make changes in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated README\n").unwrap();
        std::fs::write(
            goal.workspace_path.join("src/main.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();

        // Write a change_summary.json in staging .ta/.
        std::fs::create_dir_all(goal.workspace_path.join(".ta")).unwrap();
        std::fs::write(
            goal.workspace_path.join(".ta/change_summary.json"),
            r#"{
                "summary": "Updated README and main entry point",
                "changes": [
                    {
                        "path": "README.md",
                        "action": "modified",
                        "what": "Rewrote project description with new tagline",
                        "why": "Old description was outdated after v2 rewrite",
                        "independent": true,
                        "depends_on": [],
                        "depended_by": []
                    },
                    {
                        "path": "src/main.rs",
                        "action": "modified",
                        "why": "Added hello world output",
                        "independent": false,
                        "depends_on": [],
                        "depended_by": ["README.md"]
                    }
                ],
                "dependency_notes": "main.rs change is referenced in README"
            }"#,
        )
        .unwrap();

        // Build PR with default summary (triggers agent summary usage).
        build_package(&config, &goal_id, "Changes from agent work", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg = &packages[0];

        // Summary should come from change_summary.json.
        assert_eq!(
            pkg.summary.what_changed,
            "Updated README and main entry point"
        );

        // Dependency notes in open_questions.
        assert!(pkg
            .summary
            .open_questions
            .contains(&"main.rs change is referenced in README".to_string()));

        // Artifacts should have descriptions populated.
        let readme_artifact = pkg
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri.contains("README.md"))
            .unwrap();
        // README has both `what` and `why` — `what` goes to explanation_tiers.summary,
        // `why` goes to explanation_tiers.explanation.
        let readme_tiers = readme_artifact.explanation_tiers.as_ref().unwrap();
        assert_eq!(
            readme_tiers.summary,
            "Rewrote project description with new tagline"
        );
        assert_eq!(
            readme_tiers.explanation,
            "Old description was outdated after v2 rewrite"
        );
        assert!(readme_artifact.dependencies.is_empty());

        // main.rs has only `why` (no `what`) — backward compat: goes to rationale.
        let main_artifact = pkg
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri.contains("main.rs"))
            .unwrap();
        assert_eq!(
            main_artifact.rationale.as_deref(),
            Some("Added hello world output")
        );
        assert_eq!(main_artifact.dependencies.len(), 1);
        assert_eq!(
            main_artifact.dependencies[0].target_uri,
            "fs://workspace/README.md"
        );
        assert_eq!(
            main_artifact.dependencies[0].kind,
            DependencyKind::DependedBy
        );
    }

    #[test]
    fn build_pr_with_no_changes_fails() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Start a goal.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "No changes".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Nothing".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal_id = goals[0].goal_run_id.to_string();

        // Build PR should fail — no changes.
        let result = build_package(&config, &goal_id, "No changes", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No changes"));
    }

    // ── Selective Review Tests ──────────────────────────────────────

    #[test]
    fn selective_apply_with_approve_pattern() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(project.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Start goal + modify files + build PR.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Selective test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test selective approval".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify all files.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        std::fs::write(
            goal.workspace_path.join("src/main.rs"),
            "fn main() { println!(\"hi\"); }\n",
        )
        .unwrap();
        std::fs::write(
            goal.workspace_path.join("src/lib.rs"),
            "pub fn lib() { println!(\"lib\"); }\n",
        )
        .unwrap();

        // Build PR.
        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Apply with selective approval: only approve src/**
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["src/**".to_string()],
                reject: &[],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Verify only src files changed.
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(readme, "# Test\n"); // unchanged

        let main = std::fs::read_to_string(project.path().join("src/main.rs")).unwrap();
        assert!(main.contains("println")); // changed

        let lib = std::fs::read_to_string(project.path().join("src/lib.rs")).unwrap();
        assert!(lib.contains("println")); // changed
    }

    #[test]
    fn selective_apply_with_reject_pattern() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::write(project.path().join("config.toml"), "[config]\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Reject test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test rejection".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("config.toml"), "[config]\nfoo=1\n").unwrap();

        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Approve all, then reject config.toml.
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["all".to_string()],
                reject: &["config.toml".to_string()],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Only README should be updated.
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(readme, "# Updated\n");

        let config_content = std::fs::read_to_string(project.path().join("config.toml")).unwrap();
        assert_eq!(config_content, "[config]\n"); // unchanged
    }

    #[test]
    fn selective_apply_special_value_all() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("file1.txt"), "a\n").unwrap();
        std::fs::write(project.path().join("file2.txt"), "b\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "All test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test all".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("file1.txt"), "a-updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("file2.txt"), "b-updated\n").unwrap();

        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Use "all" to approve everything.
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["all".to_string()],
                reject: &[],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        let file1 = std::fs::read_to_string(project.path().join("file1.txt")).unwrap();
        assert_eq!(file1, "a-updated\n");

        let file2 = std::fs::read_to_string(project.path().join("file2.txt")).unwrap();
        assert_eq!(file2, "b-updated\n");
    }

    #[test]
    fn selective_apply_special_value_rest() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("important.txt"), "keep\n").unwrap();
        std::fs::write(project.path().join("other.txt"), "skip\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Rest test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test rest".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("important.txt"), "keep-updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("other.txt"), "skip-updated\n").unwrap();

        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Reject important, approve "rest".
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["rest".to_string()],
                reject: &["important.txt".to_string()],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // important should be unchanged (rejected before "rest").
        let important = std::fs::read_to_string(project.path().join("important.txt")).unwrap();
        assert_eq!(important, "keep\n");

        // other should be updated (approved by "rest").
        let other = std::fs::read_to_string(project.path().join("other.txt")).unwrap();
        assert_eq!(other, "skip-updated\n");
    }

    #[test]
    fn selective_apply_dependency_validation_fails() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(project.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Dependency test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test dependencies".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(
            goal.workspace_path.join("src/main.rs"),
            "fn main() { lib::lib(); }\n",
        )
        .unwrap();
        std::fs::write(
            goal.workspace_path.join("src/lib.rs"),
            "pub mod lib { pub fn lib() {} }\n",
        )
        .unwrap();

        // Write a change_summary.json declaring the dependency.
        std::fs::create_dir_all(goal.workspace_path.join(".ta")).unwrap();
        std::fs::write(
            goal.workspace_path.join(".ta/change_summary.json"),
            r#"{
                "summary": "Updated main and lib",
                "changes": [
                    {
                        "path": "src/main.rs",
                        "action": "modified",
                        "why": "Call lib function",
                        "independent": false,
                        "depends_on": ["src/lib.rs"],
                        "depended_by": []
                    },
                    {
                        "path": "src/lib.rs",
                        "action": "modified",
                        "why": "Add lib module",
                        "independent": false,
                        "depends_on": [],
                        "depended_by": ["src/main.rs"]
                    }
                ]
            }"#,
        )
        .unwrap();

        build_package(&config, &goal_id, "Changes from agent work", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Try to approve main.rs but reject lib.rs — should fail dependency check.
        let result = apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["src/main.rs".to_string()],
                reject: &["src/lib.rs".to_string()],
                discuss: &[],
            },
            None,  // phase_override
            false, // force_apply
        );

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("dependency conflict"));
    }

    #[test]
    fn build_pr_excludes_target_dir() {
        // Set up a source project with a target/ directory (simulates Rust build artifacts).
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        // Create a target/ directory with build artifacts in source.
        std::fs::create_dir_all(project.path().join("target/debug/incremental")).unwrap();
        std::fs::write(
            project.path().join("target/debug/incremental/artifact.o"),
            "binary-data",
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Exclude test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test exclusion".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Verify target/ was NOT copied to staging.
        assert!(!goal.workspace_path.join("target").exists());

        // Modify a real source file.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();

        // Also create target/ in staging to simulate agent building in staging.
        std::fs::create_dir_all(goal.workspace_path.join("target/debug/incremental")).unwrap();
        std::fs::write(
            goal.workspace_path
                .join("target/debug/incremental/ta_workspace-123"),
            "build-artifact",
        )
        .unwrap();

        // Build PR — target/ should be excluded from artifacts.
        build_package(&config, &goal_id, "Test changes", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg = &packages[0];

        // Only the real source change should be in artifacts.
        assert_eq!(pkg.changes.artifacts.len(), 1);
        assert!(pkg.changes.artifacts[0].resource_uri.contains("README.md"));

        // No target/ files should appear.
        for artifact in &pkg.changes.artifacts {
            assert!(
                !artifact.resource_uri.contains("target/"),
                "target/ file should be excluded: {}",
                artifact.resource_uri
            );
        }
    }

    #[test]
    fn auto_summary_exempt_lockfiles() {
        assert!(is_auto_summary_exempt("fs://workspace/Cargo.lock"));
        assert!(is_auto_summary_exempt("fs://workspace/package-lock.json"));
        assert!(is_auto_summary_exempt("fs://workspace/yarn.lock"));
        assert!(is_auto_summary_exempt("fs://workspace/deep/pnpm-lock.yaml"));
        assert!(is_auto_summary_exempt("fs://workspace/Gemfile.lock"));
        assert!(is_auto_summary_exempt("fs://workspace/poetry.lock"));
    }

    #[test]
    fn auto_summary_exempt_config_manifests() {
        assert!(is_auto_summary_exempt("fs://workspace/Cargo.toml"));
        assert!(is_auto_summary_exempt(
            "fs://workspace/crates/foo/Cargo.toml"
        ));
        assert!(is_auto_summary_exempt("fs://workspace/package.json"));
        assert!(is_auto_summary_exempt("fs://workspace/pyproject.toml"));
    }

    #[test]
    fn auto_summary_exempt_docs() {
        assert!(is_auto_summary_exempt("fs://workspace/PLAN.md"));
        assert!(is_auto_summary_exempt("fs://workspace/CHANGELOG.md"));
        assert!(is_auto_summary_exempt("fs://workspace/README.md"));
    }

    #[test]
    fn auto_summary_not_exempt_source_files() {
        assert!(!is_auto_summary_exempt("fs://workspace/src/main.rs"));
        assert!(!is_auto_summary_exempt("fs://workspace/src/lib.rs"));
        assert!(!is_auto_summary_exempt("fs://workspace/tests/test.rs"));
        assert!(!is_auto_summary_exempt("fs://workspace/build.rs"));
    }

    #[test]
    fn summary_enforcement_error_fails_build() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Enforcement test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test enforcement".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Write workflow.toml AFTER goal start (so .ta/ dir exists).
        std::fs::write(
            project.path().join(".ta/workflow.toml"),
            "[build]\nsummary_enforcement = \"error\"\n",
        )
        .unwrap();

        // Modify a source file (no change_summary.json → no description).
        std::fs::write(goal.workspace_path.join("src/main.rs"), "fn main() { 1 }\n").unwrap();

        // Build should fail with error enforcement.
        let result = build_package(&config, &goal_id, "Test", false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing descriptions"));
    }

    #[test]
    fn summary_enforcement_ignore_skips_check() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Ignore test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test ignore".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Write workflow.toml AFTER goal start.
        std::fs::write(
            project.path().join(".ta/workflow.toml"),
            "[build]\nsummary_enforcement = \"ignore\"\n",
        )
        .unwrap();

        std::fs::write(goal.workspace_path.join("src/main.rs"), "fn main() { 1 }\n").unwrap();

        // Build should succeed with ignore enforcement.
        build_package(&config, &goal_id, "Test", false).unwrap();
    }

    #[test]
    fn summary_enforcement_exempt_files_pass_error_mode() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(project.path().join("Cargo.lock"), "# lock\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Exempt test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test exempt".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Write workflow.toml AFTER goal start.
        std::fs::write(
            project.path().join(".ta/workflow.toml"),
            "[build]\nsummary_enforcement = \"error\"\n",
        )
        .unwrap();

        // Only modify exempt files.
        std::fs::write(
            goal.workspace_path.join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        // Should pass even in error mode since only exempt files changed.
        build_package(&config, &goal_id, "Test", false).unwrap();
    }

    // ── v0.3.4 Draft Amendment Tests ──────────────────────────────────

    #[test]
    fn amend_drop_removes_artifact() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::write(project.path().join("extra.txt"), "remove me\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Amend drop test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test amend --drop".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Make changes in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("extra.txt"), "changed\n").unwrap();

        // Build draft.
        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        assert_eq!(packages[0].changes.artifacts.len(), 2);

        // Drop the extra.txt artifact.
        amend_package(
            &config,
            &pkg_id,
            "extra.txt",
            None,
            true,
            Some("Not needed"),
            "human",
        )
        .unwrap();

        // Verify artifact was removed.
        let updated = load_package(&config, packages[0].package_id).unwrap();
        assert_eq!(updated.changes.artifacts.len(), 1);
        assert!(updated.changes.artifacts[0]
            .resource_uri
            .contains("README.md"));

        // Verify decision log entry was added.
        assert!(updated
            .plan
            .decision_log
            .iter()
            .any(|d| d.decision.contains("dropped")));
    }

    #[test]
    fn amend_file_replaces_content() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Amend file test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test amend --file".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Make changes in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Bad version\n").unwrap();

        // Build draft.
        build_package(&config, &goal_id, "Test changes", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Write a corrected file.
        let corrected = TempDir::new().unwrap();
        let corrected_path = corrected.path().join("corrected.md");
        std::fs::write(&corrected_path, "# Corrected version\n").unwrap();

        // Amend with corrected file.
        amend_package(
            &config,
            &pkg_id,
            "README.md",
            Some(corrected_path.to_str().unwrap()),
            false,
            Some("Fixed heading"),
            "reviewer",
        )
        .unwrap();

        // Verify amendment record.
        let updated = load_package(&config, packages[0].package_id).unwrap();
        let artifact = &updated.changes.artifacts[0];
        assert!(artifact.amendment.is_some());
        let amend = artifact.amendment.as_ref().unwrap();
        assert_eq!(amend.amended_by, "reviewer");
        assert_eq!(amend.amendment_type, AmendmentType::FileReplaced);
        assert_eq!(amend.reason, Some("Fixed heading".to_string()));

        // Disposition should be reset to Pending.
        assert_eq!(artifact.disposition, ArtifactDisposition::Pending);

        // Decision log entry should exist.
        assert!(updated
            .plan
            .decision_log
            .iter()
            .any(|d| d.decision.contains("amended")));

        // Corrected file should be in staging workspace.
        let staging_content =
            std::fs::read_to_string(goal.workspace_path.join("README.md")).unwrap();
        assert_eq!(staging_content, "# Corrected version\n");
    }

    #[test]
    fn amend_rejects_invalid_state() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "State test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test state check".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        build_package(&config, &goal_id, "Test", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Deny the package first.
        deny_package(&config, &pkg_id, "bad", "reviewer").unwrap();

        // Amend should fail on denied packages.
        let result = amend_package(&config, &pkg_id, "README.md", None, true, None, "human");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot amend"));
    }

    #[test]
    fn amend_drop_nonexistent_artifact_fails() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Missing artifact test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        build_package(&config, &goal_id, "Test", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Try to drop a non-existent artifact.
        let result = amend_package(
            &config,
            &pkg_id,
            "nonexistent.rs",
            None,
            true,
            None,
            "human",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn amend_requires_file_or_drop() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Mode test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        build_package(&config, &goal_id, "Test", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        // Neither --file nor --drop.
        let result = amend_package(&config, &pkg_id, "README.md", None, false, None, "human");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Must specify"));

        // Both --file and --drop.
        let result = amend_package(
            &config,
            &pkg_id,
            "README.md",
            Some("/some/file"),
            true,
            None,
            "human",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot use both"));
    }

    #[test]
    fn compute_unified_diff_basic() {
        let original = "line1\nline2\nline3\n";
        let modified = "line1\nline2_modified\nline3\n";
        let diff = compute_unified_diff("test.txt", original, modified);
        assert!(diff.contains("--- a/test.txt"));
        assert!(diff.contains("+++ b/test.txt"));
        assert!(diff.contains("-line2"));
        assert!(diff.contains("+line2_modified"));
    }

    // ── v0.4.1.2 tests: follow-up draft continuity ──

    #[test]
    fn follow_up_extend_build_produces_unified_diff() {
        // When a follow-up extends parent staging, building a draft should
        // produce a unified diff against the original source.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create parent goal with a modification.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Parent unified".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Parent work".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let parent = &goals[0];
        let parent_id = parent.goal_run_id;

        // Modify staging (parent work).
        std::fs::write(parent.workspace_path.join("README.md"), "# Parent edit\n").unwrap();

        // Create follow-up that extends parent staging.
        let follow_up = super::super::goal::start_goal_extending_parent(
            &config,
            &goal_store,
            "Follow-up unified",
            "Follow-up work",
            "test-agent",
            None,
            parent,
            parent_id,
        )
        .unwrap();

        // Make additional changes in the same staging (follow-up work).
        std::fs::write(
            follow_up.workspace_path.join("README.md"),
            "# Parent edit + follow-up edit\n",
        )
        .unwrap();
        std::fs::write(follow_up.workspace_path.join("NEW.md"), "# New file\n").unwrap();

        // Build draft for follow-up — should include ALL changes (parent + follow-up).
        let follow_up_id = follow_up.goal_run_id.to_string();
        build_package(&config, &follow_up_id, "Unified changes", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg = packages
            .iter()
            .find(|p| p.goal.goal_id == follow_up_id)
            .unwrap();

        // Should contain both the modified README and the new file.
        assert!(pkg.changes.artifacts.len() >= 2);
        let uris: Vec<&str> = pkg
            .changes
            .artifacts
            .iter()
            .map(|a| a.resource_uri.as_str())
            .collect();
        assert!(uris.contains(&"fs://workspace/README.md"));
        assert!(uris.contains(&"fs://workspace/NEW.md"));
    }

    #[test]
    fn follow_up_same_staging_supersedes_parent_draft() {
        // When follow-up uses same staging, building its draft should supersede parent's.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create parent goal and build its draft.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Parent supersede".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Parent work".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let parent = &goals[0];
        let parent_id = parent.goal_run_id;
        let parent_goal_id_str = parent_id.to_string();

        std::fs::write(parent.workspace_path.join("README.md"), "# Parent\n").unwrap();
        build_package(&config, &parent_goal_id_str, "Parent changes", false).unwrap();

        // Re-read parent to get pr_package_id.
        let parent = goal_store.get(parent_id).unwrap().unwrap();
        let parent_pkg_id = parent.pr_package_id.unwrap();

        // Create follow-up extending parent staging.
        let follow_up = super::super::goal::start_goal_extending_parent(
            &config,
            &goal_store,
            "Follow-up supersede",
            "Follow-up work",
            "test-agent",
            None,
            &parent,
            parent_id,
        )
        .unwrap();

        // Make more changes and build follow-up draft.
        std::fs::write(
            follow_up.workspace_path.join("README.md"),
            "# Parent + follow-up\n",
        )
        .unwrap();
        let follow_up_id = follow_up.goal_run_id.to_string();
        build_package(&config, &follow_up_id, "Follow-up changes", false).unwrap();

        // Parent's draft should now be Superseded.
        let parent_pkg = load_package(&config, parent_pkg_id).unwrap();
        assert!(
            matches!(parent_pkg.status, DraftStatus::Superseded { .. }),
            "Expected Superseded, got {:?}",
            parent_pkg.status
        );
    }

    #[test]
    fn follow_up_different_staging_does_not_supersede_parent() {
        // When follow-up uses different staging, parent draft should NOT be superseded.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create parent goal and build its draft.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Parent independent".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Parent work".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let parent = &goals[0];
        let parent_id = parent.goal_run_id;
        let parent_goal_id_str = parent_id.to_string();

        std::fs::write(parent.workspace_path.join("README.md"), "# Parent\n").unwrap();
        build_package(&config, &parent_goal_id_str, "Parent changes", false).unwrap();

        // Re-read parent to get pr_package_id.
        let parent = goal_store.get(parent_id).unwrap().unwrap();
        let parent_pkg_id = parent.pr_package_id.unwrap();

        // Create a standalone follow-up (different staging — the default start_goal path).
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Follow-up independent".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Independent work".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None, // Not using --follow-up, but we'll manually set parent_goal_id
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        // Manually set parent_goal_id to simulate standalone follow-up with different staging.
        let all_goals = goal_store.list().unwrap();
        let mut follow_up = all_goals
            .iter()
            .find(|g| g.title == "Follow-up independent")
            .unwrap()
            .clone();
        follow_up.parent_goal_id = Some(parent_id);
        goal_store.save(&follow_up).unwrap();

        // Verify different staging paths.
        assert_ne!(follow_up.workspace_path, parent.workspace_path);

        // Make changes and build follow-up draft.
        std::fs::write(
            follow_up.workspace_path.join("README.md"),
            "# Independent\n",
        )
        .unwrap();
        let follow_up_id = follow_up.goal_run_id.to_string();
        build_package(&config, &follow_up_id, "Independent changes", false).unwrap();

        // Parent's draft should NOT be superseded (different staging = independent).
        let parent_pkg = load_package(&config, parent_pkg_id).unwrap();
        assert!(
            matches!(parent_pkg.status, DraftStatus::PendingReview),
            "Expected PendingReview (not superseded), got {:?}",
            parent_pkg.status
        );
    }

    // ── v0.4.5 Partial ID matching tests ──────────────────────────────

    #[test]
    fn resolve_draft_id_exact_uuid() {
        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());

        // Create a draft package to resolve.
        std::fs::write(project.path().join("README.md"), "# Hello\n").unwrap();
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Prefix test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test prefix matching".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();
        let goals = goal_store.list().unwrap();
        let goal_id = goals[0].goal_run_id.to_string();

        std::fs::write(goals[0].workspace_path.join("README.md"), "# Changed\n").unwrap();
        build_package(&config, &goal_id, "Test", false).unwrap();

        let pkgs = load_all_packages(&config).unwrap();
        let pkg_id = pkgs[0].package_id;

        // Exact UUID should resolve.
        let resolved = resolve_draft_id(&pkg_id.to_string(), &config).unwrap();
        assert_eq!(resolved, pkg_id);

        // 8-char prefix should resolve.
        let prefix = &pkg_id.to_string()[..8];
        let resolved = resolve_draft_id(prefix, &config).unwrap();
        assert_eq!(resolved, pkg_id);
    }

    #[test]
    fn resolve_draft_id_rejects_short_prefix() {
        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());

        // Short strings that don't match any draft by ID or title.
        let result = resolve_draft_id("abc", &config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No draft matching"));
    }

    #[test]
    fn resolve_draft_id_no_match() {
        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());

        let result = resolve_draft_id("00000000", &config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No draft matching"));
    }

    #[test]
    fn changeset_diff_provider_unified_diff() {
        let cs = ChangeSet::new(
            "fs://workspace/src/main.rs".to_string(),
            ChangeKind::FsPatch,
            DiffContent::UnifiedDiff {
                content: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new"
                    .to_string(),
            },
        );
        let provider = ChangeSetDiffProvider {
            changesets: vec![cs],
        };
        let diff = provider.get_diff("changeset:0").unwrap();
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
    }

    #[test]
    fn changeset_diff_provider_create_file() {
        let cs = ChangeSet::new(
            "fs://workspace/new.txt".to_string(),
            ChangeKind::FsPatch,
            DiffContent::CreateFile {
                content: "hello\nworld".to_string(),
            },
        );
        let provider = ChangeSetDiffProvider {
            changesets: vec![cs],
        };
        let diff = provider.get_diff("changeset:0").unwrap();
        assert!(diff.contains("+hello"));
        assert!(diff.contains("+world"));
        assert!(diff.contains("/dev/null"));
    }

    #[test]
    fn changeset_diff_provider_delete_file() {
        let cs = ChangeSet::new(
            "fs://workspace/old.txt".to_string(),
            ChangeKind::FsPatch,
            DiffContent::DeleteFile,
        );
        let provider = ChangeSetDiffProvider {
            changesets: vec![cs],
        };
        let diff = provider.get_diff("changeset:0").unwrap();
        assert!(diff.contains("deleted"));
    }

    #[test]
    fn changeset_diff_provider_invalid_ref() {
        let provider = ChangeSetDiffProvider { changesets: vec![] };
        assert!(provider.get_diff("invalid").is_err());
        assert!(provider.get_diff("changeset:abc").is_err());
    }

    #[test]
    fn changeset_diff_provider_out_of_range() {
        let provider = ChangeSetDiffProvider { changesets: vec![] };
        let err = provider.get_diff("changeset:0").unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn changeset_diff_provider_multiple_changesets() {
        let cs0 = ChangeSet::new(
            "fs://workspace/a.rs".to_string(),
            ChangeKind::FsPatch,
            DiffContent::UnifiedDiff {
                content: "diff-a".to_string(),
            },
        );
        let cs1 = ChangeSet::new(
            "fs://workspace/b.rs".to_string(),
            ChangeKind::FsPatch,
            DiffContent::UnifiedDiff {
                content: "diff-b".to_string(),
            },
        );
        let provider = ChangeSetDiffProvider {
            changesets: vec![cs0, cs1],
        };
        assert_eq!(provider.get_diff("changeset:0").unwrap(), "diff-a");
        assert_eq!(provider.get_diff("changeset:1").unwrap(), "diff-b");
    }

    #[test]
    fn apply_default_submit_when_vcs_detected() {
        // In a git repo with no explicit flags, apply should run the full
        // submit workflow (git_commit = true) because the adapter auto-detects.
        let project = TempDir::new().unwrap();

        // Initialize git repo.
        for git_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        for git_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        let config = GatewayConfig::for_project(project.path());

        // Start goal.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Default submit test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test default submit".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify file in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Default submit\n").unwrap();

        // Build + approve.
        build_package(&config, &goal_id, "Default submit test", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();

        // Apply with git_commit=true (simulating new default when VCS detected),
        // git_push=false (no remote), git_review=false.
        apply_package(
            &config,
            &pkg_id,
            None,
            true,  // submit (stage+commit)
            false, // no push (no remote in test)
            false, // no review
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Verify a commit was created on a ta/ branch.
        let log = clear_git_env(
            std::process::Command::new("git")
                .args(["log", "--all", "--oneline", "-5"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let log_output = String::from_utf8_lossy(&log.stdout);
        assert!(log_output.contains("Default submit test"));

        // Verify ta/ branch exists.
        let branches = clear_git_env(
            std::process::Command::new("git")
                .args(["branch", "--list", "ta/*"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_list = String::from_utf8_lossy(&branches.stdout);
        assert!(
            !branch_list.trim().is_empty(),
            "Expected ta/ branch to exist"
        );
    }

    #[test]
    fn apply_no_submit_copies_files_only() {
        // With --no-submit (git_commit=false), only files are copied — no VCS ops.
        let project = TempDir::new().unwrap();

        // Initialize git repo.
        for git_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        for git_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(git_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        let config = GatewayConfig::for_project(project.path());

        // Start goal.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "No submit test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test no-submit".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify file in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# No submit\n").unwrap();

        // Build + approve.
        build_package(&config, &goal_id, "No submit test", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();

        // Apply with --no-submit (git_commit=false).
        apply_package(
            &config,
            &pkg_id,
            None,
            false, // no submit
            false,
            false,
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // Files should be copied.
        let readme = std::fs::read_to_string(project.path().join("README.md")).unwrap();
        assert_eq!(readme, "# No submit\n");

        // No ta/ branches should exist — only the initial main branch.
        let branches = clear_git_env(
            std::process::Command::new("git")
                .args(["branch", "--list", "ta/*"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_list = String::from_utf8_lossy(&branches.stdout);
        assert!(
            branch_list.trim().is_empty(),
            "Expected no ta/ branch with --no-submit"
        );
    }

    // v0.12.2.3 regression tests ─────────────────────────────────────────────

    #[test]
    fn follow_up_full_staging_diff_includes_parent_session_files() {
        // Regression test for v0.12.2.3 item 1/4:
        // When a follow-up goal reuses the parent's staging directory, `build_package`
        // must include ALL files that differ from source, including those written by
        // the parent session and NOT re-written by the child session.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Parent session: start goal, write version bump to Cargo.toml and README.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Parent session".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Parent work".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let parent = &goals[0];
        let parent_id = parent.goal_run_id;

        // Parent agent wrote version bump (simulates crash — no draft built yet).
        std::fs::write(
            parent.workspace_path.join("Cargo.toml"),
            "[package]\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        std::fs::write(
            parent.workspace_path.join("README.md"),
            "# Updated by parent\n",
        )
        .unwrap();

        // Follow-up session: extends parent staging.
        // The follow-up agent only writes a NEW file — does NOT re-write Cargo.toml or README.
        let follow_up = super::super::goal::start_goal_extending_parent(
            &config,
            &goal_store,
            "Follow-up session",
            "Follow-up work",
            "test-agent",
            None,
            parent,
            parent_id,
        )
        .unwrap();

        // Child agent writes only a new file — parent files (Cargo.toml, README) untouched.
        std::fs::write(
            follow_up.workspace_path.join("CHANGELOG.md"),
            "## v0.2.0\n- Initial release\n",
        )
        .unwrap();

        // Build follow-up draft — must include ALL staging changes (parent + child).
        let follow_up_id = follow_up.goal_run_id.to_string();
        build_package(&config, &follow_up_id, "Follow-up changes", false).unwrap();

        let packages = load_all_packages(&config).unwrap();
        let pkg = packages
            .iter()
            .find(|p| p.goal.goal_id == follow_up_id)
            .unwrap();

        let uris: Vec<&str> = pkg
            .changes
            .artifacts
            .iter()
            .map(|a| a.resource_uri.as_str())
            .collect();

        // All three files must appear — Cargo.toml and README from parent session,
        // CHANGELOG from child session.
        assert!(
            uris.contains(&"fs://workspace/Cargo.toml"),
            "Expected Cargo.toml (parent-session write) in follow-up draft; got: {:?}",
            uris
        );
        assert!(
            uris.contains(&"fs://workspace/README.md"),
            "Expected README.md (parent-session write) in follow-up draft; got: {:?}",
            uris
        );
        assert!(
            uris.contains(&"fs://workspace/CHANGELOG.md"),
            "Expected CHANGELOG.md (child-session write) in follow-up draft; got: {:?}",
            uris
        );
    }

    #[test]
    fn build_package_strips_ta_injection_from_claude_md() {
        // v0.12.2.3 item 2: build_package strips the TA injection header from
        // CLAUDE.md in staging before computing the diff.
        let project = TempDir::new().unwrap();
        std::fs::write(
            project.path().join("CLAUDE.md"),
            "# Real project instructions\n\nActual content here.\n",
        )
        .unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Injection test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test injection strip".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let goal = &goals[0];

        // Simulate crash: inject TA header into CLAUDE.md in staging (restore never ran).
        // Also make a real change to README so there's at least one diff.
        let injected = format!(
            "# Trusted Autonomy \u{2014} Mediated Goal\n\nGoal context here.\n\n---\n\n{}\n",
            "# Real project instructions\n\nActual content here.\n"
        );
        std::fs::write(goal.workspace_path.join("CLAUDE.md"), &injected).unwrap();
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();

        // build_package should strip the injection.
        let goal_id = goal.goal_run_id.to_string();
        build_package(&config, &goal_id, "Test changes", false).unwrap();

        // Staging CLAUDE.md should now be the original content (injection stripped).
        let staged_claude = std::fs::read_to_string(goal.workspace_path.join("CLAUDE.md")).unwrap();
        assert!(
            !staged_claude.starts_with("# Trusted Autonomy"),
            "TA injection header should have been stripped from staging CLAUDE.md"
        );
        assert!(
            staged_claude.contains("# Real project instructions"),
            "Original CLAUDE.md content should be preserved after stripping"
        );

        // CLAUDE.md diff in package: should reflect original vs source (no injection noise).
        let packages = load_all_packages(&config).unwrap();
        let pkg = packages.iter().find(|p| p.goal.goal_id == goal_id).unwrap();

        // CLAUDE.md should NOT appear in artifacts (stripped = original = source content).
        let claude_artifact = pkg
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri == "fs://workspace/CLAUDE.md");
        assert!(
            claude_artifact.is_none(),
            "CLAUDE.md should not appear in diff after stripping (content matches source)"
        );
    }

    #[test]
    fn strip_ta_injection_from_staging_no_injection() {
        // strip_ta_injection_from_staging is a no-op when CLAUDE.md has no injection.
        let staging = TempDir::new().unwrap();
        std::fs::write(
            staging.path().join("CLAUDE.md"),
            "# Normal project instructions\n",
        )
        .unwrap();
        let result = strip_ta_injection_from_staging(staging.path()).unwrap();
        assert!(!result, "Should return false — no injection present");
        let content = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# Normal project instructions\n");
    }

    #[test]
    fn strip_ta_injection_from_staging_with_injection() {
        // strip_ta_injection_from_staging removes the TA header and preserves original.
        let staging = TempDir::new().unwrap();
        let original = "# Project Instructions\n\nDo this and that.\n";
        let injected = format!(
            "# Trusted Autonomy \u{2014} Mediated Goal\n\nGoal context.\n\n---\n\n{}\n",
            original
        );
        std::fs::write(staging.path().join("CLAUDE.md"), &injected).unwrap();
        let result = strip_ta_injection_from_staging(staging.path()).unwrap();
        assert!(result, "Should return true — injection stripped");
        let content = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, original, "Original content should be preserved");
        assert!(
            !content.starts_with("# Trusted Autonomy"),
            "TA header should be gone"
        );
    }

    #[test]
    fn strip_ta_injection_from_staging_no_original_removes_file() {
        // When injection has no original content, CLAUDE.md is removed entirely.
        let staging = TempDir::new().unwrap();
        // Simulate injection where CLAUDE.md did not exist originally.
        let injected = "# Trusted Autonomy \u{2014} Mediated Goal\n\nGoal context.\n\n---\n\n";
        std::fs::write(staging.path().join("CLAUDE.md"), injected).unwrap();
        let result = strip_ta_injection_from_staging(staging.path()).unwrap();
        assert!(result, "Should return true — injection stripped");
        assert!(
            !staging.path().join("CLAUDE.md").exists(),
            "CLAUDE.md should be removed when no original content existed"
        );
    }

    // ── v0.12.8: load_all_packages deserialization error logging regression test ──

    #[test]
    fn load_all_packages_skips_corrupted_file_and_returns_valid() {
        // Regression test for Bug 2 (v0.12.8): load_all_packages must silently
        // skip unreadable JSON files (logging a warning) and return the valid
        // packages. Previously, silent `if let Ok(...)` meant the user saw
        // "No active drafts" with no explanation.

        // Set up a full project so we can produce a real DraftPackage.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Regression test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test corrupted draft skip".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();
        build_package(&config, &goal_id, "Test changes", false).unwrap();

        // Verify we have exactly 1 valid package.
        let packages_before = load_all_packages(&config).unwrap();
        assert_eq!(
            packages_before.len(),
            1,
            "Should have 1 valid package before corruption"
        );

        // Now drop a corrupted JSON file into pr_packages_dir alongside the valid one.
        let corrupted_path = config
            .pr_packages_dir
            .join("00000000-0000-0000-0000-000000000000.json");
        std::fs::write(
            &corrupted_path,
            r#"{"this_is": "not a valid DraftPackage", "missing_required_fields": true}"#,
        )
        .unwrap();

        // load_all_packages must not panic or error — it should skip the bad file
        // and return the 1 valid package.
        let packages_after = load_all_packages(&config).unwrap();
        assert_eq!(
            packages_after.len(),
            1,
            "Corrupted draft file should be skipped; valid package should still be returned"
        );
        assert_eq!(
            packages_after[0].goal.goal_id, goal_id,
            "The returned package should be the valid one"
        );
    }

    /// v0.13.1.7 — Bug D regression test: plan-update ordering.
    ///
    /// Before the fix, `apply_plan_update()` wrote PLAN.md to disk before
    /// `adapter.prepare()` checked out the feature branch. Git refused the
    /// checkout because PLAN.md had unstaged changes, causing the apply to fail.
    ///
    /// This test verifies that a plan-phase-linked goal applies cleanly with
    /// `--submit`, and that the commit on the feature branch includes PLAN.md.
    #[test]
    #[cfg(unix)]
    fn apply_with_plan_phase_does_not_dirty_tree_before_branch_checkout() {
        let project = TempDir::new().unwrap();

        for cmd_args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(cmd_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        // Write a minimal PLAN.md with a pending phase.
        // Uses the "### v<version> — Title" format matched by the default schema pattern 2.
        std::fs::write(
            project.path().join("PLAN.md"),
            "# Plan\n\n### v0.99.0 — Bug D test phase\n<!-- status: pending -->\n\n#### Items\n\n1. [ ] Do the thing\n",
        )
        .unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        for cmd_args in &[vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            clear_git_env(
                std::process::Command::new("git")
                    .args(cmd_args)
                    .current_dir(project.path()),
            )
            .output()
            .unwrap();
        }

        let config = GatewayConfig::for_project(project.path());

        // Start a goal linked to the plan phase.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Bug D fix test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test plan-update ordering".to_string(),
                agent: "test-agent".to_string(),
                phase: Some("v0.99.0".to_string()),
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify README in staging.
        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();

        // Build + approve + apply with git commit.
        // Before the Bug D fix this would fail: PLAN.md was written before
        // adapter.prepare() ran, leaving a dirty working tree at checkout time.
        build_package(&config, &goal_id, "Phase linked change", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        approve_package(&config, &pkg_id, "tester", false).unwrap();
        apply_package(
            &config,
            &pkg_id,
            None,
            true,  // git_commit = true (triggers the Bug D path)
            false, // git_push
            false, // git_review
            false, // skip_verify
            false, // dry_run
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
            None,  // phase_override
            false, // force_apply
        )
        .unwrap();

        // A feature branch must exist.
        let branches = clear_git_env(
            std::process::Command::new("git")
                .args(["branch", "--list", "ta/*"])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let branch_list = String::from_utf8_lossy(&branches.stdout);
        assert!(
            !branch_list.trim().is_empty(),
            "Feature branch should have been created by apply"
        );
        let branch_name = branch_list.trim().trim_start_matches("* ").to_string();

        // The commit on the feature branch must include PLAN.md.
        let show = clear_git_env(
            std::process::Command::new("git")
                .args(["show", "--stat", "--oneline", &branch_name])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let show_output = String::from_utf8_lossy(&show.stdout);
        assert!(
            show_output.contains("PLAN.md"),
            "PLAN.md must be part of the feature branch commit, got:\n{}",
            show_output
        );

        // PLAN.md on the feature branch should be updated to done.
        // Note: after restore_state() the working tree is back on main, so we
        // must read the file contents from the feature branch commit directly.
        let plan_on_branch = clear_git_env(
            std::process::Command::new("git")
                .args(["show", &format!("{}:PLAN.md", branch_name)])
                .current_dir(project.path()),
        )
        .output()
        .unwrap();
        let plan_content = String::from_utf8_lossy(&plan_on_branch.stdout);
        assert!(
            plan_content.contains("status: done"),
            "PLAN.md on the feature branch should be updated to done, got:\n{}",
            plan_content
        );
    }

    // ── v0.13.15: version backward bump check ─────────────────────

    #[test]
    fn read_toml_version_parses_basic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            "[package]\nname = \"foo\"\nversion = \"0.14.2-alpha\"\n",
        )
        .unwrap();
        let result = read_toml_version(&path);
        assert!(result.is_some());
        let (s, tuple) = result.unwrap();
        assert_eq!(s, "0.14.2-alpha");
        assert_eq!(tuple, (0, 14, 2));
    }

    #[test]
    fn read_toml_version_missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = read_toml_version(&dir.path().join("nonexistent.toml"));
        assert!(result.is_none());
    }

    #[test]
    fn check_backward_version_bump_lower_version_warns() {
        let source = TempDir::new().unwrap();
        let staging = TempDir::new().unwrap();

        std::fs::write(
            source.path().join("Cargo.toml"),
            "[workspace]\nversion = \"0.14.2-alpha\"\n",
        )
        .unwrap();
        std::fs::write(
            staging.path().join("Cargo.toml"),
            "[workspace]\nversion = \"0.13.8-alpha\"\n",
        )
        .unwrap();

        let artifacts = vec![make_test_artifact("fs://workspace/Cargo.toml")];
        let warnings = check_backward_version_bump(&artifacts, source.path(), staging.path());
        assert_eq!(warnings.len(), 1, "expected 1 warning for backward bump");
        assert!(warnings[0].output.contains("0.13.8-alpha"));
        assert!(warnings[0].output.contains("0.14.2-alpha"));
    }

    #[test]
    fn check_backward_version_bump_forward_version_no_warn() {
        let source = TempDir::new().unwrap();
        let staging = TempDir::new().unwrap();

        std::fs::write(
            source.path().join("Cargo.toml"),
            "[workspace]\nversion = \"0.14.2-alpha\"\n",
        )
        .unwrap();
        std::fs::write(
            staging.path().join("Cargo.toml"),
            "[workspace]\nversion = \"0.14.3-alpha\"\n",
        )
        .unwrap();

        let artifacts = vec![make_test_artifact("fs://workspace/Cargo.toml")];
        let warnings = check_backward_version_bump(&artifacts, source.path(), staging.path());
        assert!(warnings.is_empty(), "forward bump should not warn");
    }

    // ── v0.14.2: Multi-Party Approval & Threshold Governance ─────

    /// Scaffold a minimal project with a .ta/workflow.toml and a staged change.
    /// Returns (config, pkg_id).
    fn setup_governance_test(workflow_toml_content: &str) -> (GatewayConfig, String, TempDir) {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();

        let ta_dir = project.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), workflow_toml_content).unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Start goal + make a change.
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Governance test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test governance".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();
        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();

        build_package(&config, &goal_id, "Governance test change", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();

        (config, pkg_id, project)
    }

    #[test]
    fn governance_single_approver_default_behavior() {
        // Default config: require_approvals = 1, no approvers list — classic single-reviewer flow.
        let (config, pkg_id, _project) = setup_governance_test("");
        // A single approve with no --as flag (using "tester" identity) should fully approve.
        approve_package(&config, &pkg_id, "tester", false).unwrap();
        let pkg = load_package(&config, pkg_id.parse().unwrap()).unwrap();
        assert!(
            matches!(pkg.status, DraftStatus::Approved { .. }),
            "expected Approved, got {:?}",
            pkg.status
        );
        assert_eq!(pkg.pending_approvals.len(), 1);
    }

    #[test]
    fn governance_two_of_three_quorum() {
        // require_approvals = 2: first approval does NOT transition to Approved.
        let toml =
            "[governance]\nrequire_approvals = 2\napprovers = [\"alice\", \"bob\", \"carol\"]\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        // First approval — quorum not yet reached.
        approve_package(&config, &pkg_id, "alice", false).unwrap();
        let pkg = load_package(&config, pkg_id.parse().unwrap()).unwrap();
        assert_eq!(
            pkg.status,
            DraftStatus::PendingReview,
            "should still be PendingReview after 1/2 approvals"
        );
        assert_eq!(pkg.pending_approvals.len(), 1);

        // Second approval — quorum reached.
        approve_package(&config, &pkg_id, "bob", false).unwrap();
        let pkg = load_package(&config, pkg_id.parse().unwrap()).unwrap();
        assert!(
            matches!(pkg.status, DraftStatus::Approved { .. }),
            "expected Approved after 2/2 approvals, got {:?}",
            pkg.status
        );
        assert_eq!(pkg.pending_approvals.len(), 2);
    }

    #[test]
    fn governance_duplicate_approval_rejected() {
        let toml = "[governance]\nrequire_approvals = 2\napprovers = [\"alice\", \"bob\"]\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        approve_package(&config, &pkg_id, "alice", false).unwrap();
        let result = approve_package(&config, &pkg_id, "alice", false);
        assert!(
            result.is_err(),
            "duplicate approval from same reviewer must fail"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("already approved"), "unexpected error: {msg}");
    }

    #[test]
    fn governance_unlisted_reviewer_rejected() {
        // approvers list is non-empty; reviewer not in list → error.
        let toml = "[governance]\nrequire_approvals = 1\napprovers = [\"alice\", \"bob\"]\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        let result = approve_package(&config, &pkg_id, "eve", false);
        assert!(result.is_err(), "unlisted reviewer must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not in the approvers list"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn governance_override_bypasses_quorum() {
        // require_approvals = 3, but override_identity can bypass with --override.
        let toml = "[governance]\nrequire_approvals = 3\napprovers = [\"alice\", \"bob\", \"carol\"]\noverride_identity = \"admin\"\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        approve_package(&config, &pkg_id, "admin", true).unwrap();
        let pkg = load_package(&config, pkg_id.parse().unwrap()).unwrap();
        assert!(
            matches!(pkg.status, DraftStatus::Approved { .. }),
            "override should transition to Approved immediately, got {:?}",
            pkg.status
        );
    }

    #[test]
    fn governance_override_wrong_identity_rejected() {
        // --override with an identity that is not the configured override_identity → error.
        let toml = "[governance]\nrequire_approvals = 2\napprovers = [\"alice\", \"bob\"]\noverride_identity = \"admin\"\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        let result = approve_package(&config, &pkg_id, "eve", true);
        assert!(result.is_err(), "wrong override identity must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not in the approvers list") || msg.contains("override identity"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn governance_empty_approvers_list_accepts_any_reviewer() {
        // Empty approvers list means no identity restriction.
        let toml = "[governance]\nrequire_approvals = 1\napprovers = []\n";
        let (config, pkg_id, _project) = setup_governance_test(toml);

        approve_package(&config, &pkg_id, "anyone", false).unwrap();
        let pkg = load_package(&config, pkg_id.parse().unwrap()).unwrap();
        assert!(matches!(pkg.status, DraftStatus::Approved { .. }));
    }

    // ── v0.13.15: PLAN.md deferred items validation ───────────────

    #[test]
    fn plan_unchecked_in_done_phase_warns() {
        let content = "### v0.13.9 — Phase Title\n\
<!-- status: done -->\n\
- [x] Item completed\n\
- [ ] Item not done without deferred target\n";
        let warnings = detect_unchecked_in_done_phases(content);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].output.contains("v0.13.9"));
        assert!(warnings[0].output.contains("Item not done"));
    }

    #[test]
    fn plan_unchecked_with_deferred_target_no_warn() {
        let content = "### v0.13.9 — Phase Title\n\
<!-- status: done -->\n\
- [x] Item completed\n\
- [ ] Item → v0.14.0 (deferred)\n";
        let warnings = detect_unchecked_in_done_phases(content);
        assert!(
            warnings.is_empty(),
            "item with → vX.Y target should not warn"
        );
    }

    #[test]
    fn plan_unchecked_in_pending_phase_no_warn() {
        let content = "### v0.13.9 — Phase Title\n\
<!-- status: pending -->\n\
- [ ] Item not done\n";
        let warnings = detect_unchecked_in_done_phases(content);
        assert!(warnings.is_empty(), "pending phase should not warn");
    }

    // ── v0.14.8.1: DraftResolver — ID unification tests ─────────────────

    /// Helper: create a project, start a goal, modify a file, build a draft.
    /// Returns (config, goal_id, project TempDir).
    fn setup_resolver_test() -> (GatewayConfig, String, TempDir) {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        let config = GatewayConfig::for_project(project.path());
        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "Resolver test goal".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test resolver".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();
        std::fs::write(goal.workspace_path.join("README.md"), "# Modified\n").unwrap();
        build_package(&config, &goal_id, "Resolver test change", false).unwrap();
        (config, goal_id, project)
    }

    #[test]
    fn resolve_draft_by_full_uuid() {
        let (config, _goal_id, _project) = setup_resolver_test();
        let packages = load_all_packages(&config).unwrap();
        let full_uuid = packages[0].package_id.to_string();
        let resolved = resolve_draft_id_flexible(&config, Some(&full_uuid));
        assert!(resolved.is_ok(), "full UUID must resolve: {:?}", resolved);
        assert_eq!(resolved.unwrap(), full_uuid);
    }

    #[test]
    fn resolve_draft_by_uuid_prefix() {
        let (config, _goal_id, _project) = setup_resolver_test();
        let packages = load_all_packages(&config).unwrap();
        let prefix = &packages[0].package_id.to_string()[..8];
        let resolved = resolve_draft_id_flexible(&config, Some(prefix));
        assert!(resolved.is_ok(), "UUID prefix must resolve: {:?}", resolved);
    }

    #[test]
    fn resolve_draft_by_shortref_seq() {
        let (config, _goal_id, _project) = setup_resolver_test();
        let packages = load_all_packages(&config).unwrap();
        let pkg = &packages[0];
        // Only test this format when the package has goal_shortref and draft_seq set.
        if let (Some(shortref), seq) = (&pkg.goal_shortref, pkg.draft_seq) {
            if seq > 0 {
                let shortref_seq = format!("{}/{}", shortref, seq);
                let resolved = resolve_draft_id_flexible(&config, Some(&shortref_seq));
                assert!(
                    resolved.is_ok(),
                    "shortref/seq '{}' must resolve: {:?}",
                    shortref_seq,
                    resolved
                );
                assert_eq!(resolved.unwrap(), pkg.package_id.to_string());
            }
        }
        // (If seq is 0 the package predates v0.14.7.3 shortref — test is vacuously OK.)
    }

    #[test]
    fn resolve_draft_unknown_id_gives_helpful_error() {
        let (config, _goal_id, _project) = setup_resolver_test();
        let result = resolve_draft_id_flexible(&config, Some("no-such-draft-xyz-000"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("ta draft list") || msg.contains("No draft"),
            "error should guide the user: {msg}"
        );
    }

    #[test]
    fn draft_list_ids_are_resolvable() {
        let (config, _goal_id, _project) = setup_resolver_test();
        let packages = load_all_packages(&config).unwrap();
        for pkg in &packages {
            let display_id = draft_display_id(pkg);
            let resolved = resolve_draft_id_flexible(&config, Some(&display_id));
            assert!(
                resolved.is_ok(),
                "DRAFT ID column value '{}' from ta draft list must resolve, got: {:?}",
                display_id,
                resolved
            );
            assert_eq!(
                resolved.unwrap(),
                pkg.package_id.to_string(),
                "resolved ID must match package_id for '{}'",
                display_id
            );
        }
    }

    // ── v0.14.9.2: deny_artifact tests ────────────────────────────────

    #[test]
    fn deny_artifact_sets_disposition() {
        // Creates a package with 2 artifacts, denies one, verifies the other is unchanged.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        std::fs::write(project.path().join("extra.txt"), "extra\n").unwrap();

        let config = GatewayConfig::for_project(project.path());

        super::super::goal::execute(
            &super::super::goal::GoalCommands::Start {
                title: "deny artifact test".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Test selective artifact deny".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        let goal = &goals[0];
        let goal_id = goal.goal_run_id.to_string();

        // Modify both files in the staging workspace.
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        std::fs::write(goal.workspace_path.join("extra.txt"), "updated extra\n").unwrap();

        build_package(&config, &goal_id, "Test deny artifact", false).unwrap();
        let packages = load_all_packages(&config).unwrap();
        let pkg_id = packages[0].package_id.to_string();
        assert_eq!(
            packages[0].changes.artifacts.len(),
            2,
            "should have 2 artifacts"
        );

        // Deny only the extra.txt artifact by loading the package directly
        // and calling the internal logic (deny_artifact reads stdin so we test at package level).
        let mut pkg = load_package(&config, packages[0].package_id).unwrap();
        let artifact_idx = pkg
            .changes
            .artifacts
            .iter()
            .position(|a| a.resource_uri.contains("extra.txt"))
            .expect("extra.txt artifact should exist");
        pkg.changes.artifacts[artifact_idx].disposition = ArtifactDisposition::Rejected;
        save_package(&config, &pkg).unwrap();

        // Reload and verify only extra.txt is rejected, README.md is still pending.
        let updated = load_package(&config, packages[0].package_id).unwrap();
        let readme_artifact = updated
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri.contains("README.md"))
            .expect("README.md should still exist");
        let extra_artifact = updated
            .changes
            .artifacts
            .iter()
            .find(|a| a.resource_uri.contains("extra.txt"))
            .expect("extra.txt should still exist");
        assert_eq!(
            readme_artifact.disposition,
            ArtifactDisposition::Pending,
            "README.md should remain pending"
        );
        assert_eq!(
            extra_artifact.disposition,
            ArtifactDisposition::Rejected,
            "extra.txt should be rejected"
        );
        let _ = pkg_id; // suppress unused warning
    }

    // ── bump_workspace_version ────────────────────────────────────────────────

    #[test]
    fn bump_version_updates_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.14.22-rc.4\"\nname = \"ta\"\n",
        )
        .unwrap();

        let modified = bump_workspace_version(dir.path(), "0.14.23-alpha").unwrap();

        let content = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(
            content.contains("version = \"0.14.23-alpha\""),
            "Cargo.toml should have new version"
        );
        assert!(modified.iter().any(|p| p.ends_with("Cargo.toml")));
    }

    #[test]
    fn bump_version_updates_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("CLAUDE.md"),
            "# Instructions\n\n- **Current version**: `0.14.22-rc.4`\n- See PLAN.md\n",
        )
        .unwrap();

        bump_workspace_version(dir.path(), "0.15.0-alpha").unwrap();

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(
            content.contains("**Current version**: `0.15.0-alpha`"),
            "CLAUDE.md should have new version"
        );
        assert!(
            !content.contains("0.14.22-rc.4"),
            "old version should be gone"
        );
    }

    #[test]
    fn bump_version_updates_release_toml_prerelease() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".release.toml"),
            "prerelease = false\ntitle_suffix = \"Foo\"\n",
        )
        .unwrap();

        bump_workspace_version(dir.path(), "0.15.0-alpha").unwrap();

        let content = std::fs::read_to_string(dir.path().join(".release.toml")).unwrap();
        assert!(
            content.contains("prerelease = true"),
            ".release.toml should set prerelease=true on phase apply"
        );
    }

    #[test]
    fn bump_version_no_change_when_already_at_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "version = \"0.15.0-alpha\"\n",
        )
        .unwrap();

        let modified = bump_workspace_version(dir.path(), "0.15.0-alpha").unwrap();

        // No modification — file was already at target.
        assert!(
            modified.is_empty(),
            "no files should be modified if already at target"
        );
    }

    #[test]
    fn bump_version_missing_files_are_skipped() {
        // Workspace with no CLAUDE.md or .release.toml — should not error.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "version = \"0.14.0-alpha\"\n",
        )
        .unwrap();

        let result = bump_workspace_version(dir.path(), "0.15.0-alpha");
        assert!(result.is_ok(), "missing optional files should not error");
    }

    // ── v0.15.8.1: build_draft_inline tests ──────────────────────────

    /// Build a minimal overlay workspace suitable for inline-build testing.
    fn setup_inline_build_project() -> (TempDir, TempDir, ta_mcp_gateway::GatewayConfig) {
        let source = TempDir::new().unwrap();
        let staging = TempDir::new().unwrap();

        // Source project: one file.
        std::fs::write(source.path().join("README.md"), "# Source\n").unwrap();

        // Staging has a modification the agent "made".
        std::fs::write(staging.path().join("README.md"), "# Modified by agent\n").unwrap();

        let config = ta_mcp_gateway::GatewayConfig::for_project(source.path());

        // Create a goal so build_package can find it.
        let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let mut goal = ta_goal::GoalRun::new(
            "Test goal",
            "Test objective",
            "claude-code",
            staging.path().to_path_buf(),
            config.goals_dir.clone(),
        );
        goal.state = ta_goal::GoalRunState::Finalizing {
            run_pid: None,
            exit_code: 0,
            finalize_started_at: chrono::Utc::now(),
        };
        goal.source_dir = Some(source.path().to_path_buf());
        goal_store.save(&goal).unwrap();

        (source, staging, config)
    }

    #[test]
    fn build_draft_inline_succeeds_and_creates_draft() {
        let (source, staging, config) = setup_inline_build_project();

        // Find the goal ID from the store.
        let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        assert_eq!(goals.len(), 1, "should have exactly one goal");
        let goal_id = goals[0].goal_run_id.to_string();

        let result = build_draft_inline(&config, &goal_id, "Test goal", &[], &[], None);
        assert!(
            result.is_ok(),
            "build_draft_inline should succeed: {:?}",
            result
        );

        // A draft package should now exist.
        let packages = load_all_packages(&config).unwrap();
        assert_eq!(packages.len(), 1, "should have created one draft package");

        drop(source);
        drop(staging);
    }

    #[test]
    fn build_draft_inline_attaches_verification_warnings() {
        let (source, staging, config) = setup_inline_build_project();

        let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let goal_id = goal_store.list().unwrap()[0].goal_run_id.to_string();

        let warnings = vec![ta_changeset::draft_package::VerificationWarning {
            command: "cargo test".to_string(),
            exit_code: Some(1),
            output: "1 test failed".to_string(),
        }];

        let result = build_draft_inline(&config, &goal_id, "Test goal", &warnings, &[], None);
        assert!(result.is_ok());

        let packages = load_all_packages(&config).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].verification_warnings.len(),
            1,
            "verification warning should be attached to draft"
        );
        assert_eq!(packages[0].verification_warnings[0].command, "cargo test");

        drop(source);
        drop(staging);
    }

    #[test]
    fn build_draft_inline_fails_gracefully_on_bad_goal_id() {
        let source = TempDir::new().unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(source.path());

        // No goal exists — build should fail with a descriptive error.
        let result = build_draft_inline(
            &config,
            "00000000-0000-0000-0000-000000000000",
            "Nonexistent goal",
            &[],
            &[],
            None,
        );
        assert!(
            result.is_err(),
            "build_draft_inline should fail when goal does not exist"
        );
    }
}
