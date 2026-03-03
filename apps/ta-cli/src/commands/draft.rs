// draft.rs — draft package subcommands: build, list, view, approve, deny, apply.

use std::fs;

use chrono::{Duration, Utc};
use clap::Subcommand;
use ta_changeset::changeset::{ChangeKind, ChangeSet, CommitIntent};
use ta_changeset::diff::DiffContent;
use ta_changeset::diff_handlers::DiffHandlersConfig;
use ta_changeset::draft_package::{
    AgentIdentity, AlternativeConsidered, AmendmentRecord, AmendmentType, Artifact,
    ArtifactDisposition, ChangeDependency, ChangeType, Changes, DecisionLogEntry, DependencyKind,
    DraftPackage, DraftStatus, ExplanationTiers, Goal, Iteration, Plan, Provenance,
    RequestedAction, ReviewRequests, Risk, Signatures, Summary, WorkspaceRef,
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
use ta_goal::{GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{
    ChangeStore, ExcludePatterns, JsonFileStore, OverlayWorkspace, StagingWorkspace,
};
use uuid::Uuid;

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
    },
    /// List all draft packages.
    List {
        /// Filter by goal run ID.
        #[arg(long)]
        goal: Option<String>,
        /// Show only stale drafts (non-terminal states older than threshold).
        #[arg(long)]
        stale: bool,
    },
    /// View draft package details and diffs.
    View {
        /// draft package ID.
        id: String,
        /// Show summary and file list only (skip diffs). [DEPRECATED: use --detail top]
        #[arg(long)]
        summary: bool,
        /// Show diff for a single file only (path relative to workspace root).
        /// If a diff handler is configured for this file type, it will be opened
        /// in the external application instead of showing the diff inline.
        #[arg(long)]
        file: Option<String>,
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
    },
    /// Approve a draft package for application.
    Approve {
        /// draft package ID.
        id: String,
        /// Reviewer name.
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
    },
    /// Deny a draft package with a reason.
    Deny {
        /// draft package ID.
        id: String,
        /// Reason for denial.
        #[arg(long)]
        reason: String,
        /// Reviewer name.
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
    },
    /// Apply approved changes to the target directory.
    Apply {
        /// draft package ID.
        id: String,
        /// Target directory (defaults to project root).
        #[arg(long)]
        target: Option<String>,
        /// Create a git commit after applying.
        #[arg(long)]
        git_commit: bool,
        /// Push to remote after committing (implies --git-commit).
        #[arg(long)]
        git_push: bool,
        /// Run full submit workflow (commit + push + open review).
        /// Equivalent to --git-commit --git-push with auto PR creation.
        #[arg(long)]
        submit: bool,
        /// Conflict resolution strategy: abort (default), force-overwrite, merge.
        /// v0.2.1: Determines what happens if source files have changed since goal start.
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
        /// Draft package ID.
        id: String,
        /// Reason for closing.
        #[arg(long)]
        reason: Option<String>,
        /// Who is closing the draft.
        #[arg(long, default_value = "human-reviewer")]
        closed_by: String,
    },
    /// Garbage-collect stale staging directories for terminal-state drafts.
    Gc {
        /// Show what would be removed without actually removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Archive staging dirs to .ta/archive/ instead of deleting.
        #[arg(long)]
        archive: bool,
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

    let stale_cutoff = Utc::now() - Duration::days(3);
    let stale_count = packages
        .iter()
        .filter(|p| {
            matches!(
                p.status,
                DraftStatus::Approved { .. } | DraftStatus::PendingReview
            ) && p.created_at < stale_cutoff
        })
        .count();

    if stale_count > 0 {
        eprintln!(
            "hint: {} draft(s) approved/pending but not applied for 3+ days — run `ta draft list --stale`",
            stale_count
        );
    }
}

pub fn execute(cmd: &DraftCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        DraftCommands::Build {
            goal_id,
            summary,
            latest,
        } => build_package(config, goal_id, summary, *latest),
        DraftCommands::List { goal, stale } => list_packages(config, goal.as_deref(), *stale),
        DraftCommands::View {
            id,
            summary,
            file,
            open_external,
            detail,
            format,
            color,
        } => view_package(
            config,
            id,
            *summary,
            file.as_deref(),
            open_external,
            detail,
            format,
            *color,
        ),
        DraftCommands::Approve { id, reviewer } => approve_package(config, id, reviewer),
        DraftCommands::Deny {
            id,
            reason,
            reviewer,
        } => deny_package(config, id, reason, reviewer),
        DraftCommands::Apply {
            id,
            target,
            git_commit,
            git_push,
            submit,
            conflict_resolution,
            approve_patterns,
            reject_patterns,
            discuss_patterns,
        } => {
            // Load workflow config to merge auto_* settings with CLI flags.
            let workflow_config = ta_submit::WorkflowConfig::load_or_default(
                &config.workspace_root.join(".ta/workflow.toml"),
            );

            // CLI flags override config. --submit implies full workflow.
            let do_commit =
                *git_commit || *git_push || *submit || workflow_config.submit.auto_commit;
            let do_push = *git_push || *submit || workflow_config.submit.auto_push;
            let do_review = *submit || workflow_config.submit.auto_review;

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
                id,
                target.as_deref(),
                do_commit,
                do_push,
                do_review,
                resolution,
                SelectiveReviewPatterns {
                    approve: approve_patterns,
                    reject: reject_patterns,
                    discuss: discuss_patterns,
                },
            )
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
        } => close_package(config, id, reason.as_deref(), closed_by),
        DraftCommands::Gc { dry_run, archive } => gc_packages(config, *dry_run, *archive),
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

fn build_package(
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

    if !matches!(goal.state, GoalRunState::Running) {
        anyhow::bail!(
            "Goal is in {} state (must be running to build PR)",
            goal.state
        );
    }

    let source_dir = goal
        .source_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Goal has no source_dir (not an overlay-based goal)"))?;

    // Open the overlay workspace and compute diffs.
    // V1 TEMPORARY: Load exclude patterns for diff filtering.
    let excludes = ExcludePatterns::load(source_dir);
    let overlay =
        OverlayWorkspace::open(goal_id.clone(), source_dir, &goal.workspace_path, excludes);
    let changes = overlay.diff_all().map_err(|e| anyhow::anyhow!("{}", e))?;

    if changes.is_empty() {
        anyhow::bail!("No changes detected in staging workspace");
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
    let pkg = DraftPackage {
        package_version: "1.0.0".to_string(),
        package_id,
        created_at: Utc::now(),
        goal: Goal {
            goal_id: goal_id.to_string(),
            title: goal.title.clone(),
            objective: goal.objective.clone(),
            success_criteria: vec![],
            constraints: vec![],
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
            why: goal.objective.clone(),
            impact: format!("{} file(s) changed", artifacts.len()),
            rollback_plan: "Revert changes from staging".to_string(),
            open_questions: dependency_notes.into_iter().collect(),
        },
        plan: Plan {
            completed_steps: vec!["Agent completed work in staging".to_string()],
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
    };

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

    // Save the draft package.
    save_package(config, &pkg)?;

    // Update the goal run.
    let mut goal = goal;
    goal.pr_package_id = Some(package_id);
    goal_store.save(&goal)?;
    goal_store.transition(goal.goal_run_id, GoalRunState::PrReady)?;

    println!("draft package built: {}", package_id);
    println!("  Goal:    {} ({})", goal.title, goal_id);
    println!("  Changes: {} file(s)", pkg.changes.artifacts.len());
    for artifact in &pkg.changes.artifacts {
        println!("    {:?}  {}", artifact.change_type, artifact.resource_uri);
    }
    println!();
    println!("Review with:  ta draft view {}", package_id);
    println!("Approve with: ta draft approve {}", package_id);

    Ok(())
}

fn list_packages(
    config: &GatewayConfig,
    goal_filter: Option<&str>,
    stale_only: bool,
) -> anyhow::Result<()> {
    let packages = load_all_packages(config)?;

    // Load GC config for stale threshold.
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let stale_days = workflow_config.gc.stale_threshold_days;
    let stale_cutoff = Utc::now() - chrono::Duration::days(stale_days as i64);

    let filtered: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            if let Some(goal_id) = goal_filter {
                if p.goal.goal_id != goal_id {
                    return false;
                }
            }
            if stale_only {
                // Stale = non-terminal state (PendingReview, Approved, Draft) older than threshold.
                let is_non_terminal = matches!(
                    p.status,
                    DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
                );
                return is_non_terminal && p.created_at < stale_cutoff;
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        if stale_only {
            println!("No stale drafts found (threshold: {} days).", stale_days);
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

    println!(
        "{:<38} {:<30} {:<16} {:<8} AGE",
        "PACKAGE ID", "GOAL", "STATUS", "FILES"
    );
    println!("{}", "-".repeat(104));

    // Load goal store for macro goal context.
    let goal_store = GoalRunStore::new(&config.goals_dir).ok();

    for pkg in &filtered {
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

        println!(
            "{:<38} {:<30} {:<16} {:<8} {}",
            pkg.package_id,
            goal_display,
            status_display,
            pkg.changes.artifacts.len(),
            age_str,
        );
    }
    println!("\n{} package(s).", filtered.len());
    Ok(())
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

/// DiffProvider implementation using StagingWorkspace.
#[allow(dead_code)]
struct StagingDiffProvider {
    staging: StagingWorkspace,
}

impl DiffProvider for StagingDiffProvider {
    fn get_diff(&self, diff_ref: &str) -> Result<String, ta_changeset::ChangeSetError> {
        // diff_ref format: "changeset:N" where N is the changeset index.
        // For now, we'll need to extract the file path from artifacts.
        // This is a simple implementation — in production, we'd store a mapping.
        // For v0.2.3, we'll return a placeholder and enhance in follow-up.
        Ok(format!("[Diff content for {}]", diff_ref))
    }
}

#[allow(clippy::too_many_arguments)]
fn view_package(
    config: &GatewayConfig,
    id: &str,
    summary_only: bool,
    file_filter: Option<&str>,
    open_external: &Option<bool>,
    detail_str: &str,
    format_str: &str,
    color: bool,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let pkg = load_package(config, package_id)?;

    // Parse detail level and format.
    let detail_level = detail_str
        .parse::<DetailLevel>()
        .map_err(|e| anyhow::anyhow!(e))?;
    let output_format = format_str
        .parse::<OutputFormat>()
        .map_err(|e| anyhow::anyhow!(e))?;

    // v0.2.3: Use output adapters for rendering.
    // Exception: If --file with --open-external, try external handler first.
    if let Some(filter) = file_filter {
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

    // Create render context.
    // For DetailLevel::Full, we'd need a DiffProvider, but for v0.2.3 we'll support
    // it without diffs initially (diffs can be added as follow-up).
    let ctx = RenderContext {
        package: &pkg,
        detail_level: effective_detail,
        file_filter: file_filter.map(String::from),
        diff_provider: None, // TODO: Wire up StagingWorkspace diff provider in follow-up
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

fn approve_package(config: &GatewayConfig, id: &str, reviewer: &str) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

    if !matches!(pkg.status, DraftStatus::PendingReview) {
        anyhow::bail!(
            "Cannot approve package in {:?} state (must be PendingReview)",
            pkg.status
        );
    }

    pkg.status = DraftStatus::Approved {
        approved_by: reviewer.to_string(),
        approved_at: Utc::now(),
    };
    save_package(config, &pkg)?;

    // Transition the goal if we can find it.
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

    println!("Approved draft package {} by {}", package_id, reviewer);
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
    save_package(config, &pkg)?;

    println!("Denied draft package {}: {}", package_id, reason);
    Ok(())
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
        file_filter: None,
        diff_provider: None,
    };
    let adapter = get_adapter(OutputFormat::Terminal, false);
    let rendered = adapter
        .render(&ctx)
        .unwrap_or_else(|_| format!("{}\n\n{}", goal.title, pkg.summary.what_changed));

    // Git convention: first line is the subject, then blank line, then body.
    // The terminal adapter starts with "Draft: <id>\nStatus: ...\n..." which isn't
    // a good subject line. Replace the header with goal title as subject.
    let body = if let Some(pos) = rendered.find("Changes (") {
        // Extract from "Changes (...)" onward — the artifact listing.
        &rendered[pos..]
    } else {
        rendered.as_str()
    };

    format!(
        "{}\n\n{}\nImpact: {}\n\n{}",
        goal.title, pkg.summary.what_changed, pkg.summary.impact, body
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_package(
    config: &GatewayConfig,
    id: &str,
    target: Option<&str>,
    git_commit: bool,
    git_push: bool,
    git_review: bool,
    conflict_resolution: ta_workspace::ConflictResolution,
    patterns: SelectiveReviewPatterns,
) -> anyhow::Result<()> {
    let package_id = resolve_draft_id(id, config)?;
    let mut pkg = load_package(config, package_id)?;

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

    let target_dir = match target {
        Some(t) => std::path::PathBuf::from(t),
        None => goal
            .source_dir
            .clone()
            .unwrap_or_else(|| config.workspace_root.clone()),
    };

    // Apply changes — use overlay path for overlay-based goals, legacy path otherwise.
    let applied_files: Vec<String> = if let Some(ref source_dir) = goal.source_dir {
        // Overlay-based goal: diff staging vs source, copy changed files.
        // V1 TEMPORARY: Load exclude patterns for diff filtering.
        let excludes = ExcludePatterns::load(source_dir);
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
                    let excludes = ExcludePatterns::load(source_dir);
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

        let applied = overlay
            .apply_with_conflict_check(&target_dir, conflict_resolution, &artifact_uris)
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

    println!(
        "Applied {} file(s) to {}",
        applied_files.len(),
        target_dir.display()
    );
    for file in &applied_files {
        println!("  {}", file);
    }

    // If the goal has a plan_phase, mark it done in PLAN.md + record history + suggest next.
    // This must happen BEFORE the git commit so the status update is included in the commit.
    if let Some(ref phase) = goal.plan_phase {
        let plan_path = target_dir.join("PLAN.md");
        if plan_path.exists() {
            let content = std::fs::read_to_string(&plan_path)?;

            // Record the old status before updating.
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

            let updated =
                super::plan::update_phase_status(&content, phase, super::plan::PlanStatus::Done);

            // Verify the update actually changed the content.
            let changed = updated != content;
            eprintln!(
                "[plan-update] content changed={}, writing to {}",
                changed,
                plan_path.display()
            );

            std::fs::write(&plan_path, &updated)?;
            println!("Updated PLAN.md: Phase {} -> done", phase);

            // Record history.
            let _ = super::plan::record_history(
                &target_dir,
                phase,
                &old_status,
                &super::plan::PlanStatus::Done,
            );

            // Auto-suggest the next pending phase.
            let phases_after = super::plan::parse_plan(&updated);
            if let Some(next) = super::plan::find_next_pending(&phases_after, Some(phase.as_str()))
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

    // Submit workflow integration (git or other adapters).
    if git_commit {
        use ta_submit::{GitAdapter, NoneAdapter, SubmitAdapter, WorkflowConfig};

        // Load workflow config if it exists.
        let workflow_config_path = target_dir.join(".ta/workflow.toml");
        let workflow_config = WorkflowConfig::load_or_default(&workflow_config_path);

        // Select adapter based on config.
        // Default to "git" if in a git repo and no config exists (backwards compatibility).
        let is_git_repo = target_dir.join(".git").exists();
        let adapter_name = if workflow_config.submit.adapter == "none" && is_git_repo {
            "git"
        } else {
            &workflow_config.submit.adapter
        };

        let adapter: Box<dyn SubmitAdapter> = match adapter_name {
            "git" => Box::new(GitAdapter::new(&target_dir)),
            _ => Box::new(NoneAdapter::new()),
        };

        println!("\nUsing submit adapter: {}", adapter.name());

        // Prepare (create branch if needed).
        if let Err(e) = adapter.prepare(goal, &workflow_config.submit) {
            eprintln!("Warning: adapter prepare failed: {}", e);
        }

        // Commit changes — goal title as subject, complete draft summary as body.
        println!("Committing changes...");
        let commit_msg = build_commit_message(goal, &pkg);

        match adapter.commit(goal, &pkg, &commit_msg) {
            Ok(result) => {
                println!("[ok] {}", result.message);
            }
            Err(e) => {
                eprintln!("Commit failed: {}", e);
                // Continue anyway if this is a "none" adapter
                if adapter.name() != "none" {
                    anyhow::bail!("Failed to commit changes: {}", e);
                }
            }
        }

        // Push to remote if requested.
        if git_push {
            println!("Pushing to remote...");
            match adapter.push(goal) {
                Ok(result) => {
                    println!("[ok] {}", result.message);
                }
                Err(e) => {
                    if adapter.name() != "none" {
                        anyhow::bail!("Failed to push: {}", e);
                    }
                }
            }
        }

        // Open review (PR) if requested.
        if git_review {
            println!("Creating pull request...");
            match adapter.open_review(goal, &pkg) {
                Ok(result) => {
                    println!("[ok] {}", result.message);
                    if !result.review_url.starts_with("none://") {
                        println!("  PR URL: {}", result.review_url);
                    }
                }
                Err(e) => {
                    eprintln!("Warning: PR creation failed: {}", e);
                    eprintln!("  You can manually create a PR from the pushed branch.");
                }
            }
        }
    }

    // Transition goal to Applied → update package status.
    let _ = goal_store.transition(goal.goal_run_id, GoalRunState::Applied);
    pkg.status = DraftStatus::Applied {
        applied_at: Utc::now(),
    };
    save_package(config, &pkg)?;

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

    // Post-apply validation summary: confirm state is consistent for the human.
    println!();
    println!("-- Post-Apply Status --");
    println!("  Draft:  {} -> applied", id);
    println!(
        "  Goal:   {} -> applied",
        goal.goal_run_id.to_string().get(..8).unwrap_or("?")
    );
    if let Some(ref phase) = goal.plan_phase {
        let plan_path = target_dir.join("PLAN.md");
        if plan_path.exists() {
            let content = std::fs::read_to_string(&plan_path).unwrap_or_default();
            let phases = super::plan::parse_plan(&content);
            if let Some(p) = phases.iter().find(|p| p.id == *phase) {
                let status_str = match p.status {
                    super::plan::PlanStatus::Done => "done",
                    super::plan::PlanStatus::InProgress => "in_progress",
                    super::plan::PlanStatus::Pending => "pending",
                };
                if p.status == super::plan::PlanStatus::Done {
                    println!("  Plan:   {} -> {}", phase, status_str);
                } else {
                    eprintln!(
                        "  [warn] Plan: {} is still '{}' -- expected 'done'. Check PLAN.md.",
                        phase, status_str
                    );
                }
            } else {
                eprintln!("  [warn] Plan: phase '{}' not found in PLAN.md", phase);
            }
        }
    }
    if git_commit {
        println!(
            "  Submit: committed{}",
            if git_push { " + pushed" } else { "" }
        );
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
        None, // no objective file
        no_launch,
        false, // not interactive
        false, // not macro
        None,  // not resuming
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

    println!("Draft {} closed.", package_id);
    if let Some(r) = reason {
        println!("  Reason: {}", r);
    }
    println!("  Previous status: {}", prev_status);
    Ok(())
}

// ── Draft garbage collection (v0.3.6) ───────────────────────────────

/// Garbage-collect stale staging directories for drafts in terminal states.
fn gc_packages(config: &GatewayConfig, dry_run: bool, archive: bool) -> anyhow::Result<()> {
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

    if dry_run {
        println!("\n{} staging dir(s) would be removed.", cleaned);
    } else {
        println!(
            "\n{} staging dir(s) {}.",
            cleaned,
            if archive { "archived" } else { "removed" }
        );
        if skipped > 0 {
            println!("{} skipped (archive already exists).", skipped);
        }
    }
    Ok(())
}

// ── File-based draft package storage ────────────────────────────────

fn load_all_packages(config: &GatewayConfig) -> anyhow::Result<Vec<DraftPackage>> {
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
            if let Ok(pkg) = serde_json::from_str::<DraftPackage>(&json) {
                packages.push(pkg);
            }
        }
    }

    packages.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(packages)
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
fn resolve_draft_id(id: &str, config: &GatewayConfig) -> anyhow::Result<Uuid> {
    // Fast path: exact UUID.
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }

    if id.len() < 8 {
        anyhow::bail!(
            "ID prefix '{}' is too short — use at least 8 characters (or a full UUID)",
            id
        );
    }

    let packages = load_all_packages(config)?;
    let matches: Vec<_> = packages
        .iter()
        .filter(|p| p.package_id.to_string().starts_with(id))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No draft found matching '{}'", id),
        1 => Ok(matches[0].package_id),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} drafts. Use a longer prefix.",
            id,
            n
        ),
    }
}

/// Resolve a goal ID from a full UUID or an 8+ character prefix.
fn resolve_goal_id_from_store(id: &str, store: &GoalRunStore) -> anyhow::Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }

    if id.len() < 8 {
        anyhow::bail!(
            "ID prefix '{}' is too short — use at least 8 characters (or a full UUID)",
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
            "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        approve_package(&config, &pkg_id, "tester").unwrap();

        // Apply (no git).
        apply_package(
            &config,
            &pkg_id,
            None,
            false,
            false,
            false,
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
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
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(project.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(project.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(project.path())
            .output()
            .unwrap();

        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(project.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(project.path())
            .output()
            .unwrap();

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
        approve_package(&config, &pkg_id, "tester").unwrap();
        apply_package(
            &config,
            &pkg_id,
            None,
            true,
            false,
            false,
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns::default(),
        )
        .unwrap();

        // Verify git log has new commit.
        let log = std::process::Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(project.path())
            .output()
            .unwrap();
        let log_output = String::from_utf8_lossy(&log.stdout);
        // Subject line is the goal title; summary is in the commit body.
        assert!(log_output.contains("Git test"));

        // Verify full commit message matches ta draft view format.
        let full_log = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(project.path())
            .output()
            .unwrap();
        let full_msg = String::from_utf8_lossy(&full_log.stdout);
        // First line is the goal title (subject).
        assert!(full_msg.starts_with("Git test\n"));
        // Body includes artifact listing with change icons and disposition badges.
        assert!(full_msg.contains("Changes ("));
        assert!(full_msg.contains("[pending]"));
        assert!(full_msg.contains("fs://workspace/README.md"));
        // No Debug-format change types like "Modify" — should use ~ + - > icons.
        assert!(!full_msg.contains("Modify  "));
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
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["src/**".to_string()],
                reject: &[],
                discuss: &[],
            },
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
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["all".to_string()],
                reject: &["config.toml".to_string()],
                discuss: &[],
            },
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
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["all".to_string()],
                reject: &[],
                discuss: &[],
            },
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
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["rest".to_string()],
                reject: &["important.txt".to_string()],
                discuss: &[],
            },
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
            ta_workspace::ConflictResolution::Abort,
            SelectiveReviewPatterns {
                approve: &["src/main.rs".to_string()],
                reject: &["src/lib.rs".to_string()],
                discuss: &[],
            },
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

        let result = resolve_draft_id("abc", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    #[test]
    fn resolve_draft_id_no_match() {
        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());

        let result = resolve_draft_id("00000000", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No draft found"));
    }
}
