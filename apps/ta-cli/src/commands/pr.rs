// pr.rs — Thin backwards-compatibility shim for `ta pr` commands.
//
// All implementation lives in draft.rs. This module converts PrCommands
// variants into DraftCommands variants and delegates to draft::execute().
// `ta pr` is a hidden alias for `ta draft` (deprecated since v0.2.4).

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

use super::draft;

#[derive(Subcommand)]
pub enum PrCommands {
    /// Build a PR package from overlay workspace diffs.
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
    /// List all PR packages.
    List {
        /// Filter by goal run ID.
        #[arg(long)]
        goal: Option<String>,
    },
    /// View PR package details and diffs.
    View {
        /// PR package ID.
        id: String,
        /// Show summary and file list only (skip diffs). [DEPRECATED: use --detail top]
        #[arg(long)]
        summary: bool,
        /// Show diff for a single file only (path relative to workspace root).
        #[arg(long)]
        file: Option<String>,
        /// Open file in external handler.
        #[arg(long)]
        open_external: Option<bool>,
        /// Detail level: top (one-line), medium (with explanations), full (with diffs).
        #[arg(long, default_value = "medium")]
        detail: String,
        /// Output format: terminal (default), markdown, json, html.
        #[arg(long, default_value = "terminal")]
        format: String,
        /// Enable ANSI color output (terminal format only). Default: off.
        #[arg(long)]
        color: bool,
    },
    /// Approve a PR package for application.
    Approve {
        /// PR package ID.
        id: String,
        /// Reviewer name.
        #[arg(long, default_value = "human-reviewer")]
        reviewer: String,
    },
    /// Deny a PR package with a reason.
    Deny {
        /// PR package ID.
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
        /// PR package ID.
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
        #[arg(long)]
        submit: bool,
        /// Conflict resolution strategy: abort (default), force-overwrite, merge.
        #[arg(long, default_value = "abort")]
        conflict_resolution: String,
        /// Approve artifacts matching these patterns (repeatable).
        #[arg(long = "approve")]
        approve_patterns: Vec<String>,
        /// Reject artifacts matching these patterns (repeatable).
        #[arg(long = "reject")]
        reject_patterns: Vec<String>,
        /// Mark artifacts for discussion matching these patterns (repeatable).
        #[arg(long = "discuss")]
        discuss_patterns: Vec<String>,
    },
}

/// Convert a PrCommands variant to a DraftCommands variant and delegate.
pub fn execute(cmd: &PrCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let draft_cmd = to_draft_command(cmd);
    draft::execute(&draft_cmd, config)
}

fn to_draft_command(cmd: &PrCommands) -> draft::DraftCommands {
    match cmd {
        PrCommands::Build {
            goal_id,
            summary,
            latest,
        } => draft::DraftCommands::Build {
            goal_id: goal_id.clone(),
            summary: summary.clone(),
            latest: *latest,
        },
        PrCommands::List { goal } => draft::DraftCommands::List {
            goal: goal.clone(),
            stale: false,
            pending: false,
            applied: false,
            limit: None,
            all: true,
            json: false,
        },
        PrCommands::View {
            id,
            summary,
            file,
            open_external,
            detail,
            format,
            color,
        } => draft::DraftCommands::View {
            id: Some(id.clone()),
            summary: *summary,
            file: file.clone(),
            open_external: *open_external,
            detail: detail.clone(),
            format: format.clone(),
            color: *color,
            json: false,
        },
        PrCommands::Approve { id, reviewer } => draft::DraftCommands::Approve {
            id: Some(id.clone()),
            reviewer: reviewer.clone(),
        },
        PrCommands::Deny {
            id,
            reason,
            reviewer,
        } => draft::DraftCommands::Deny {
            id: Some(id.clone()),
            reason: reason.clone(),
            reviewer: reviewer.clone(),
        },
        PrCommands::Apply {
            id,
            target,
            git_commit,
            git_push,
            submit,
            conflict_resolution,
            approve_patterns,
            reject_patterns,
            discuss_patterns,
        } => draft::DraftCommands::Apply {
            id: Some(id.clone()),
            target: target.clone(),
            git_commit: *git_commit,
            git_push: *git_push,
            submit: *submit,
            conflict_resolution: conflict_resolution.clone(),
            approve_patterns: approve_patterns.clone(),
            reject_patterns: reject_patterns.clone(),
            discuss_patterns: discuss_patterns.clone(),
            phase: None,
            require_review: false,
        },
    }
}
