// follow_up.rs — Smart Follow-Up UX (v0.10.9).
//
// Provides a frictionless, context-aware entry point for resuming prior work.
// Scans goals, drafts, plan phases, and verification failures to build a
// ranked list of follow-up candidates the user can pick from interactively.

use std::fmt;

use chrono::{DateTime, Utc};
use ta_changeset::draft_package::{DraftPackage, DraftStatus, VerificationWarning};
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

use super::plan::{self, PlanPhase, PlanStatus};

// ── Follow-Up Candidate ─────────────────────────────────────────

/// The source of a follow-up candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateSource {
    /// A goal that can be continued (running, failed, has denied draft).
    Goal,
    /// A draft that was denied or has verification warnings.
    Draft,
    /// A plan phase that is in-progress or pending with prior work.
    Phase,
    /// A verification failure from a prior goal.
    VerifyFailure,
}

impl fmt::Display for CandidateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateSource::Goal => write!(f, "goal"),
            CandidateSource::Draft => write!(f, "draft"),
            CandidateSource::Phase => write!(f, "phase"),
            CandidateSource::VerifyFailure => write!(f, "verify-failure"),
        }
    }
}

/// A follow-up candidate — something the user might want to resume working on.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FollowUpCandidate {
    /// Source type (goal, draft, phase, verify-failure).
    pub source: CandidateSource,
    /// Human-readable title.
    pub title: String,
    /// Current status description (e.g., "draft denied", "verify failed").
    pub status: String,
    /// When this candidate was last updated.
    pub updated_at: DateTime<Utc>,
    /// Human-readable age string (e.g., "2h ago", "3d ago").
    pub age: String,
    /// The goal ID to follow up on (if available).
    pub goal_id: Option<Uuid>,
    /// The draft ID (if this candidate is draft-based).
    pub draft_id: Option<Uuid>,
    /// The plan phase ID (if this candidate is phase-based).
    pub phase_id: Option<String>,
    /// Path to the staging directory (if still available).
    pub staging_path: Option<std::path::PathBuf>,
    /// One-line context summary for display.
    pub context_summary: String,
    /// Denial reason (if draft was denied).
    pub denial_reason: Option<String>,
    /// Verification warnings (if present).
    pub verification_warnings: Vec<VerificationWarning>,
}

impl fmt::Display for FollowUpCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} — {} ({})", self.title, self.status, self.age)
    }
}

// ── Candidate Gathering ──────────────────────────────────────────

/// Gather all actionable follow-up candidates from goals, drafts, and plan phases.
///
/// Returns candidates sorted by recency (most recent first).
pub fn gather_follow_up_candidates(
    config: &GatewayConfig,
    goal_store: &GoalRunStore,
) -> anyhow::Result<Vec<FollowUpCandidate>> {
    let mut candidates = Vec::new();
    let now = Utc::now();

    let all_goals = goal_store.list()?;
    let all_drafts = load_all_drafts(config);

    // 1. Goals with actionable states.
    for goal in &all_goals {
        if let Some(candidate) = goal_to_candidate(goal, &all_drafts, now) {
            candidates.push(candidate);
        }
    }

    // 2. Denied drafts whose goals aren't already candidates.
    for draft in &all_drafts {
        if let Some(candidate) = draft_to_candidate(draft, &all_goals, &candidates, now) {
            candidates.push(candidate);
        }
    }

    // 3. In-progress plan phases that have no active goal.
    if let Some(source_dir) = find_source_dir(config, &all_goals) {
        if let Ok(phases) = plan::load_plan(&source_dir) {
            for phase in &phases {
                if let Some(candidate) = phase_to_candidate(phase, &all_goals, &candidates, now) {
                    candidates.push(candidate);
                }
            }
        }
    }

    // Sort by recency (most recent first).
    candidates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(candidates)
}

/// Resolve a follow-up candidate by plan phase ID.
pub fn resolve_by_phase(
    config: &GatewayConfig,
    goal_store: &GoalRunStore,
    phase_id: &str,
) -> anyhow::Result<FollowUpCandidate> {
    let all_goals = goal_store.list()?;
    let all_drafts = load_all_drafts(config);
    let now = Utc::now();

    // Normalize phase ID (strip optional 'v' prefix).
    let normalized = phase_id.strip_prefix('v').unwrap_or(phase_id);
    let with_v = format!("v{}", normalized);

    // First: find a goal that worked on this phase.
    let mut phase_goals: Vec<_> = all_goals
        .iter()
        .filter(|g| {
            g.plan_phase
                .as_deref()
                .is_some_and(|p| p == phase_id || p == normalized || p == with_v)
        })
        .collect();
    phase_goals.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    if let Some(goal) = phase_goals.first() {
        if let Some(candidate) = goal_to_candidate(goal, &all_drafts, now) {
            return Ok(candidate);
        }
        // Even if it's not in an "actionable" state, create a candidate from the most recent goal.
        return Ok(goal_to_basic_candidate(goal, now));
    }

    // No goal found — check if the phase exists in PLAN.md.
    if let Some(source_dir) = find_source_dir(config, &all_goals) {
        if let Ok(phases) = plan::load_plan(&source_dir) {
            if let Some(phase) = phases
                .iter()
                .find(|p| p.id == phase_id || p.id == normalized || p.id == with_v)
            {
                return Ok(FollowUpCandidate {
                    source: CandidateSource::Phase,
                    title: format!("{} — {}", phase.id, phase.title),
                    status: format!("phase {}", phase.status),
                    updated_at: now,
                    age: "now".to_string(),
                    goal_id: None,
                    draft_id: None,
                    phase_id: Some(phase.id.clone()),
                    staging_path: None,
                    context_summary: format!(
                        "Plan phase {} is {} — no prior goals found",
                        phase.id, phase.status
                    ),
                    denial_reason: None,
                    verification_warnings: vec![],
                });
            }
        }
    }

    anyhow::bail!(
        "No goal or plan phase found matching '{}'. \
         Use `ta plan list` to see available phases.",
        phase_id
    )
}

/// Resolve a follow-up candidate by draft ID (prefix match).
pub fn resolve_by_draft(
    config: &GatewayConfig,
    goal_store: &GoalRunStore,
    draft_prefix: &str,
) -> anyhow::Result<FollowUpCandidate> {
    let all_drafts = load_all_drafts(config);
    let now = Utc::now();

    let matches: Vec<_> = all_drafts
        .iter()
        .filter(|d| d.package_id.to_string().starts_with(draft_prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!(
            "No draft found matching prefix '{}'. Use `ta draft list` to see available drafts.",
            draft_prefix
        ),
        1 => {}
        n => anyhow::bail!(
            "Ambiguous draft prefix '{}' matches {} drafts. Use a longer prefix.",
            draft_prefix,
            n
        ),
    }

    let draft = matches[0];

    // Find the goal that produced this draft.
    let all_goals = goal_store.list()?;
    let goal = all_goals
        .iter()
        .find(|g| g.pr_package_id == Some(draft.package_id));

    let goal_id = goal.map(|g| g.goal_run_id);
    let staging_path = goal.and_then(|g| {
        if g.workspace_path.exists() {
            Some(g.workspace_path.clone())
        } else {
            None
        }
    });

    let (status, denial_reason) = match &draft.status {
        DraftStatus::Denied { reason, .. } => (format!("denied: {}", reason), Some(reason.clone())),
        other => (other.to_string(), None),
    };

    Ok(FollowUpCandidate {
        source: CandidateSource::Draft,
        title: draft.goal.title.clone(),
        status,
        updated_at: draft.created_at,
        age: format_age(now, draft.created_at),
        goal_id,
        draft_id: Some(draft.package_id),
        phase_id: goal.and_then(|g| g.plan_phase.clone()),
        staging_path,
        context_summary: format!(
            "Draft {} — {}",
            &draft.package_id.to_string()[..8],
            draft.summary.what_changed
        ),
        denial_reason,
        verification_warnings: draft.verification_warnings.clone(),
    })
}

/// Resolve a follow-up candidate by goal ID (prefix match).
pub fn resolve_by_goal(
    goal_store: &GoalRunStore,
    goal_prefix: &str,
) -> anyhow::Result<FollowUpCandidate> {
    let all_goals = goal_store.list()?;
    let now = Utc::now();

    let matches: Vec<_> = all_goals
        .iter()
        .filter(|g| g.goal_run_id.to_string().starts_with(goal_prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!(
            "No goal found matching prefix '{}'. Use `ta goal list --all` to see all goals.",
            goal_prefix
        ),
        1 => {}
        n => anyhow::bail!(
            "Ambiguous goal prefix '{}' matches {} goals. Use a longer prefix.",
            goal_prefix,
            n
        ),
    }

    Ok(goal_to_basic_candidate(matches[0], now))
}

// ── Interactive Picker ───────────────────────────────────────────

/// Display follow-up candidates and let the user pick one interactively.
///
/// Returns the selected candidate, or an error if no candidates or user cancels.
pub fn pick_candidate(candidates: &[FollowUpCandidate]) -> anyhow::Result<&FollowUpCandidate> {
    if candidates.is_empty() {
        anyhow::bail!(
            "No follow-up candidates found.\n\n\
             There are no goals, denied drafts, verification failures, or in-progress phases \
             to follow up on.\n\n\
             To start fresh: ta run \"your goal title\"\n\
             To see all goals: ta goal list --all"
        );
    }

    eprintln!("\nFollow-up candidates:\n");
    for (i, candidate) in candidates.iter().enumerate() {
        let source_tag = match candidate.source {
            CandidateSource::Goal => "goal",
            CandidateSource::Draft => "draft",
            CandidateSource::Phase => "phase",
            CandidateSource::VerifyFailure => "verify",
        };
        eprintln!(
            "  {:>2}) [{}] {} — {} ({})",
            i + 1,
            source_tag,
            candidate.title,
            candidate.status,
            candidate.age
        );
        if !candidate.context_summary.is_empty() {
            // Truncate long context summaries.
            let summary = if candidate.context_summary.len() > 80 {
                format!("{}...", &candidate.context_summary[..77])
            } else {
                candidate.context_summary.clone()
            };
            eprintln!("      {}", summary);
        }
    }

    eprintln!();
    eprint!(
        "Select candidate [1-{}] (or 'q' to cancel): ",
        candidates.len()
    );

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.eq_ignore_ascii_case("q") || trimmed.is_empty() {
        anyhow::bail!("Follow-up cancelled.");
    }

    let selection: usize = trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "Invalid selection '{}'. Enter a number 1-{} or 'q' to cancel.",
            trimmed,
            candidates.len()
        )
    })?;

    if selection == 0 || selection > candidates.len() {
        anyhow::bail!(
            "Selection {} out of range. Enter a number 1-{}.",
            selection,
            candidates.len()
        );
    }

    Ok(&candidates[selection - 1])
}

// ── Context Injection ────────────────────────────────────────────

/// Build an enhanced follow-up context section for CLAUDE.md injection.
///
/// Includes prior attempt summary, verification failures, denial reasons,
/// and reviewer feedback — everything the agent needs to pick up where
/// the previous attempt left off.
pub fn build_follow_up_context(
    candidate: &FollowUpCandidate,
    goal_store: &GoalRunStore,
    config: &GatewayConfig,
) -> String {
    let mut ctx = String::new();

    ctx.push_str("\n## Follow-Up Context\n\n");
    ctx.push_str(&format!(
        "This is a follow-up goal. You are resuming work on: **{}**\n\n",
        candidate.title
    ));

    // Source-specific context.
    match candidate.source {
        CandidateSource::Goal | CandidateSource::VerifyFailure => {
            if let Some(goal_id) = candidate.goal_id {
                if let Ok(Some(goal)) = goal_store.get(goal_id) {
                    ctx.push_str(&format!(
                        "**Prior Goal:** {} ({})\n",
                        goal.title,
                        &goal_id.to_string()[..8]
                    ));
                    ctx.push_str(&format!("**Prior Status:** {}\n", goal.state));
                    if !goal.objective.is_empty() {
                        ctx.push_str(&format!("**Objective:** {}\n", goal.objective));
                    }
                    ctx.push('\n');

                    // Include draft context if available.
                    if let Some(pr_id) = goal.pr_package_id {
                        append_draft_context(&mut ctx, config, pr_id);
                    }
                }
            }
        }
        CandidateSource::Draft => {
            if let Some(draft_id) = candidate.draft_id {
                append_draft_context(&mut ctx, config, draft_id);
            }
        }
        CandidateSource::Phase => {
            if let Some(ref phase_id) = candidate.phase_id {
                ctx.push_str(&format!(
                    "**Plan Phase:** {} (status: {})\n\n",
                    phase_id, candidate.status
                ));
                ctx.push_str(
                    "No prior goal found for this phase. Starting fresh from plan specification.\n",
                );
            }
        }
    }

    // Verification warnings.
    if !candidate.verification_warnings.is_empty() {
        ctx.push_str("\n### Verification Failures\n\n");
        ctx.push_str("The previous attempt failed pre-draft verification. Fix these issues:\n\n");
        for warn in &candidate.verification_warnings {
            ctx.push_str(&format!("- **`{}`**", warn.command));
            if let Some(code) = warn.exit_code {
                ctx.push_str(&format!(" (exit code {})", code));
            }
            ctx.push('\n');
            if !warn.output.is_empty() {
                // Truncate long output.
                let output = if warn.output.len() > 500 {
                    format!("{}...(truncated)", &warn.output[..497])
                } else {
                    warn.output.clone()
                };
                ctx.push_str(&format!("  ```\n  {}\n  ```\n", output));
            }
        }
        ctx.push('\n');
    }

    // Denial reason.
    if let Some(ref reason) = candidate.denial_reason {
        ctx.push_str("\n### Denial Reason\n\n");
        ctx.push_str(&format!(
            "The previous draft was **denied** with the following reason:\n\n> {}\n\n",
            reason
        ));
        ctx.push_str("Address the reviewer's concerns in this follow-up iteration.\n\n");
    }

    ctx
}

// ── Internal Helpers ─────────────────────────────────────────────

/// Load all draft packages from the packages directory.
fn load_all_drafts(config: &GatewayConfig) -> Vec<DraftPackage> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return Vec::new();
    }

    let mut packages = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(pkg) = serde_json::from_str::<DraftPackage>(&json) {
                        packages.push(pkg);
                    }
                }
            }
        }
    }
    packages
}

/// Append draft-specific context (denial reason, discuss items, verify warnings).
fn append_draft_context(ctx: &mut String, config: &GatewayConfig, draft_id: Uuid) {
    use super::draft::load_package;
    let draft = match load_package(config, draft_id) {
        Ok(d) => d,
        Err(_) => return,
    };

    ctx.push_str(&format!(
        "**Draft:** {} (status: {})\n",
        &draft_id.to_string()[..8],
        draft.status
    ));
    ctx.push_str(&format!(
        "**What changed:** {}\n",
        draft.summary.what_changed
    ));
    ctx.push_str(&format!("**Why:** {}\n\n", draft.summary.why));

    // Denial context.
    if let DraftStatus::Denied { ref reason, .. } = draft.status {
        ctx.push_str(&format!(
            "### Denial Reason\n\n> {}\n\nAddress the reviewer's concerns.\n\n",
            reason
        ));
    }

    // Verification warnings.
    if !draft.verification_warnings.is_empty() {
        ctx.push_str("### Verification Warnings\n\n");
        for warn in &draft.verification_warnings {
            ctx.push_str(&format!(
                "- `{}` (exit {})\n",
                warn.command,
                warn.exit_code.unwrap_or(-1)
            ));
        }
        ctx.push('\n');
    }

    // Discuss items.
    let discuss_items: Vec<_> = draft
        .changes
        .artifacts
        .iter()
        .filter(|a| {
            matches!(
                a.disposition,
                ta_changeset::draft_package::ArtifactDisposition::Discuss
            )
        })
        .collect();

    if !discuss_items.is_empty() {
        ctx.push_str("### Items for Discussion\n\n");
        for artifact in discuss_items {
            ctx.push_str(&format!("- **{}**", artifact.resource_uri));
            if let Some(ref why) = artifact.rationale {
                ctx.push_str(&format!(": {}", why));
            }
            ctx.push('\n');
            if let Some(ref comments) = artifact.comments {
                for comment in &comments.comments {
                    ctx.push_str(&format!(
                        "  - **{}**: {}\n",
                        comment.commenter, comment.text
                    ));
                }
            }
        }
        ctx.push('\n');
    }
}

/// Convert a goal to a follow-up candidate if it's in an actionable state.
fn goal_to_candidate(
    goal: &GoalRun,
    drafts: &[DraftPackage],
    now: DateTime<Utc>,
) -> Option<FollowUpCandidate> {
    // Find the draft for this goal (if any).
    let draft = goal
        .pr_package_id
        .and_then(|pr_id| drafts.iter().find(|d| d.package_id == pr_id));

    let (status, denial_reason, verification_warnings) = match &goal.state {
        GoalRunState::Failed { reason } => {
            (format!("failed: {}", truncate(reason, 40)), None, vec![])
        }
        GoalRunState::Running | GoalRunState::AwaitingInput { .. } => {
            ("in progress".to_string(), None, vec![])
        }
        GoalRunState::PrReady | GoalRunState::UnderReview => {
            // Check if draft was denied.
            if let Some(d) = draft {
                match &d.status {
                    DraftStatus::Denied { reason, .. } => (
                        format!("draft denied: {}", truncate(reason, 40)),
                        Some(reason.clone()),
                        d.verification_warnings.clone(),
                    ),
                    _ => {
                        // Draft pending or under review — check for verify warnings.
                        if !d.verification_warnings.is_empty() {
                            (
                                format!("verify warnings ({})", d.verification_warnings.len()),
                                None,
                                d.verification_warnings.clone(),
                            )
                        } else {
                            return None; // Not actionable — draft is pending/approved.
                        }
                    }
                }
            } else {
                return None; // No draft yet.
            }
        }
        GoalRunState::Configured => ("configured (not started)".to_string(), None, vec![]),
        // Terminal states aren't follow-up candidates.
        GoalRunState::Applied
        | GoalRunState::Completed
        | GoalRunState::Approved { .. }
        | GoalRunState::Created => return None,
    };

    let phase_label = goal
        .plan_phase
        .as_ref()
        .map(|p| format!("{} — ", p))
        .unwrap_or_default();

    let staging_path = if goal.workspace_path.exists() {
        Some(goal.workspace_path.clone())
    } else {
        None
    };

    let context_summary = if let Some(d) = draft {
        truncate(&d.summary.what_changed, 80).to_string()
    } else {
        truncate(&goal.objective, 80).to_string()
    };

    let source = if !verification_warnings.is_empty() {
        CandidateSource::VerifyFailure
    } else {
        CandidateSource::Goal
    };

    Some(FollowUpCandidate {
        source,
        title: format!("{}{}", phase_label, goal.title),
        status,
        updated_at: goal.updated_at,
        age: format_age(now, goal.updated_at),
        goal_id: Some(goal.goal_run_id),
        draft_id: goal.pr_package_id,
        phase_id: goal.plan_phase.clone(),
        staging_path,
        context_summary,
        denial_reason,
        verification_warnings,
    })
}

/// Convert a goal to a basic follow-up candidate (no filtering by state).
fn goal_to_basic_candidate(goal: &GoalRun, now: DateTime<Utc>) -> FollowUpCandidate {
    let phase_label = goal
        .plan_phase
        .as_ref()
        .map(|p| format!("{} — ", p))
        .unwrap_or_default();

    FollowUpCandidate {
        source: CandidateSource::Goal,
        title: format!("{}{}", phase_label, goal.title),
        status: goal.state.to_string(),
        updated_at: goal.updated_at,
        age: format_age(now, goal.updated_at),
        goal_id: Some(goal.goal_run_id),
        draft_id: goal.pr_package_id,
        phase_id: goal.plan_phase.clone(),
        staging_path: if goal.workspace_path.exists() {
            Some(goal.workspace_path.clone())
        } else {
            None
        },
        context_summary: truncate(&goal.objective, 80).to_string(),
        denial_reason: None,
        verification_warnings: vec![],
    }
}

/// Convert a denied/warn draft to a candidate if its goal isn't already represented.
fn draft_to_candidate(
    draft: &DraftPackage,
    goals: &[GoalRun],
    existing: &[FollowUpCandidate],
    now: DateTime<Utc>,
) -> Option<FollowUpCandidate> {
    // Only actionable: denied or has verification warnings.
    let (status, denial_reason) = match &draft.status {
        DraftStatus::Denied { reason, .. } => (
            format!("denied: {}", truncate(reason, 40)),
            Some(reason.clone()),
        ),
        _ if !draft.verification_warnings.is_empty() => (
            format!("verify warnings ({})", draft.verification_warnings.len()),
            None,
        ),
        _ => return None,
    };

    // Find the goal for this draft.
    let goal = goals
        .iter()
        .find(|g| g.pr_package_id == Some(draft.package_id));

    // Skip if goal is already in candidates.
    if let Some(g) = goal {
        if existing.iter().any(|c| c.goal_id == Some(g.goal_run_id)) {
            return None;
        }
    }

    Some(FollowUpCandidate {
        source: CandidateSource::Draft,
        title: draft.goal.title.clone(),
        status,
        updated_at: draft.created_at,
        age: format_age(now, draft.created_at),
        goal_id: goal.map(|g| g.goal_run_id),
        draft_id: Some(draft.package_id),
        phase_id: goal.and_then(|g| g.plan_phase.clone()),
        staging_path: goal.and_then(|g| {
            if g.workspace_path.exists() {
                Some(g.workspace_path.clone())
            } else {
                None
            }
        }),
        context_summary: truncate(&draft.summary.what_changed, 80).to_string(),
        denial_reason,
        verification_warnings: draft.verification_warnings.clone(),
    })
}

/// Convert an in-progress plan phase to a candidate if no active goal covers it.
fn phase_to_candidate(
    phase: &PlanPhase,
    goals: &[GoalRun],
    existing: &[FollowUpCandidate],
    now: DateTime<Utc>,
) -> Option<FollowUpCandidate> {
    // Only in-progress phases are follow-up candidates.
    if phase.status != PlanStatus::InProgress {
        return None;
    }

    // Skip if already covered by a goal-based candidate.
    if existing
        .iter()
        .any(|c| c.phase_id.as_deref().is_some_and(|p| p == phase.id))
    {
        return None;
    }

    // Also skip if there's an active goal for this phase.
    let has_active_goal = goals.iter().any(|g| {
        g.plan_phase.as_deref() == Some(&phase.id)
            && !matches!(
                g.state,
                GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
            )
    });

    if has_active_goal {
        return None;
    }

    Some(FollowUpCandidate {
        source: CandidateSource::Phase,
        title: format!("{} — {}", phase.id, phase.title),
        status: "in progress (no active goal)".to_string(),
        updated_at: now,
        age: "now".to_string(),
        goal_id: None,
        draft_id: None,
        phase_id: Some(phase.id.clone()),
        staging_path: None,
        context_summary: format!("Plan phase {} needs work", phase.id),
        denial_reason: None,
        verification_warnings: vec![],
    })
}

/// Find the best source directory for plan loading.
fn find_source_dir(config: &GatewayConfig, goals: &[GoalRun]) -> Option<std::path::PathBuf> {
    // Try the most recent goal's source dir.
    if let Some(goal) = goals.iter().max_by_key(|g| g.updated_at) {
        if let Some(ref source) = goal.source_dir {
            if source.exists() {
                return Some(source.clone());
            }
        }
    }
    // Fall back to workspace root.
    if config.workspace_root.join("PLAN.md").exists() {
        Some(config.workspace_root.clone())
    } else {
        None
    }
}

/// Format a human-readable age string.
fn format_age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let duration = now.signed_duration_since(then);
    let seconds = duration.num_seconds();

    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}

/// Truncate a string to at most `max_len` characters.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a safe UTF-8 boundary.
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_age_just_now() {
        let now = Utc::now();
        assert_eq!(format_age(now, now), "just now");
    }

    #[test]
    fn format_age_minutes() {
        let now = Utc::now();
        let then = now - chrono::Duration::minutes(5);
        assert_eq!(format_age(now, then), "5m ago");
    }

    #[test]
    fn format_age_hours() {
        let now = Utc::now();
        let then = now - chrono::Duration::hours(3);
        assert_eq!(format_age(now, then), "3h ago");
    }

    #[test]
    fn format_age_days() {
        let now = Utc::now();
        let then = now - chrono::Duration::days(2);
        assert_eq!(format_age(now, then), "2d ago");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(100);
        assert_eq!(truncate(&long, 10).len(), 10);
    }

    #[test]
    fn candidate_display() {
        let c = FollowUpCandidate {
            source: CandidateSource::Goal,
            title: "Test Goal".to_string(),
            status: "failed".to_string(),
            updated_at: Utc::now(),
            age: "5m ago".to_string(),
            goal_id: Some(Uuid::new_v4()),
            draft_id: None,
            phase_id: None,
            staging_path: None,
            context_summary: "test context".to_string(),
            denial_reason: None,
            verification_warnings: vec![],
        };
        let display = format!("{}", c);
        assert!(display.contains("Test Goal"));
        assert!(display.contains("failed"));
        assert!(display.contains("5m ago"));
    }

    #[test]
    fn candidate_source_display() {
        assert_eq!(CandidateSource::Goal.to_string(), "goal");
        assert_eq!(CandidateSource::Draft.to_string(), "draft");
        assert_eq!(CandidateSource::Phase.to_string(), "phase");
        assert_eq!(CandidateSource::VerifyFailure.to_string(), "verify-failure");
    }

    #[test]
    fn pick_candidate_empty_errors() {
        let result = pick_candidate(&[]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No follow-up candidates found"));
    }

    #[test]
    fn goal_to_candidate_skips_completed() {
        use std::path::PathBuf;
        let mut goal = GoalRun::new(
            "Test",
            "Test objective",
            "test-agent",
            PathBuf::from("/tmp/nonexistent"),
            PathBuf::from("/tmp/store"),
        );
        goal.state = GoalRunState::Completed;
        let result = goal_to_candidate(&goal, &[], Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn goal_to_candidate_includes_failed() {
        use std::path::PathBuf;
        let mut goal = GoalRun::new(
            "Test",
            "Test objective",
            "test-agent",
            PathBuf::from("/tmp/nonexistent"),
            PathBuf::from("/tmp/store"),
        );
        goal.state = GoalRunState::Failed {
            reason: "build error".to_string(),
        };
        let result = goal_to_candidate(&goal, &[], Utc::now());
        assert!(result.is_some());
        let c = result.unwrap();
        assert!(c.status.contains("failed"));
        assert_eq!(c.source, CandidateSource::Goal);
    }

    #[test]
    fn goal_to_candidate_includes_running() {
        use std::path::PathBuf;
        let mut goal = GoalRun::new(
            "Test",
            "Test objective",
            "test-agent",
            PathBuf::from("/tmp/nonexistent"),
            PathBuf::from("/tmp/store"),
        );
        goal.state = GoalRunState::Running;
        let result = goal_to_candidate(&goal, &[], Utc::now());
        assert!(result.is_some());
        assert_eq!(result.unwrap().status, "in progress");
    }

    #[test]
    fn phase_to_candidate_only_in_progress() {
        let pending = PlanPhase {
            id: "v0.10.9".to_string(),
            title: "Smart Follow-Up".to_string(),
            status: PlanStatus::Pending,
        };
        assert!(phase_to_candidate(&pending, &[], &[], Utc::now()).is_none());

        let done = PlanPhase {
            id: "v0.10.8".to_string(),
            title: "Pre-Draft Verification".to_string(),
            status: PlanStatus::Done,
        };
        assert!(phase_to_candidate(&done, &[], &[], Utc::now()).is_none());

        let in_progress = PlanPhase {
            id: "v0.10.9".to_string(),
            title: "Smart Follow-Up".to_string(),
            status: PlanStatus::InProgress,
        };
        let result = phase_to_candidate(&in_progress, &[], &[], Utc::now());
        assert!(result.is_some());
        assert_eq!(result.unwrap().source, CandidateSource::Phase);
    }

    #[test]
    fn basic_candidate_always_works() {
        use std::path::PathBuf;
        let goal = GoalRun::new(
            "Test",
            "obj",
            "agent",
            PathBuf::from("/tmp/x"),
            PathBuf::from("/tmp/s"),
        );
        let c = goal_to_basic_candidate(&goal, Utc::now());
        assert_eq!(c.source, CandidateSource::Goal);
        assert!(c.title.contains("Test"));
    }
}
