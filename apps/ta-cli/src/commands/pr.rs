// pr.rs — Thin backwards-compatibility shim for `ta pr` commands.
//
// Legacy commands (Build, List, View, Approve, Deny, Apply) delegate to draft.rs.
// New PR CI commands (Checks, Fix) are implemented here directly (v0.15.11.2).

use std::fs;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

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
        /// Run the full submit workflow. Default when VCS adapter is detected.
        #[arg(long, overrides_with = "no_submit")]
        submit: bool,
        /// Copy files only — skip all VCS operations.
        #[arg(long)]
        no_submit: bool,
        /// Open a review after submitting.
        #[arg(long, overrides_with = "no_review")]
        review: bool,
        /// Skip review creation.
        #[arg(long)]
        no_review: bool,
        /// Show what would happen without executing.
        #[arg(long)]
        dry_run: bool,
        /// **Deprecated**: Use --submit instead.
        #[arg(long, hide = true)]
        git_commit: bool,
        /// **Deprecated**: Use --submit instead.
        #[arg(long, hide = true)]
        git_push: bool,
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

    // ── v0.15.11.2: PR CI Failure Recovery ────────────────────────────────
    /// Poll CI check status for an open PR and print a status table.
    ///
    /// Exits non-zero if any check has failed. On failure, prints the next
    /// action: `ta pr fix <shortref>` to spawn a targeted fix agent.
    ///
    /// The <shortref> can be:
    ///   - An 8-char goal shortref (e.g., "2159d87e")
    ///   - A draft shortref/seq (e.g., "2159d87e/01")
    ///   - A draft ID prefix (8+ chars)
    ///   - A goal or draft title substring
    ///
    /// Examples:
    ///   ta pr checks 2159d87e          # check by goal shortref
    ///   ta pr checks 2159d87e/01       # check by draft shortref
    Checks {
        /// Goal shortref, draft shortref/seq, draft ID prefix, or title substring.
        shortref: String,
    },

    /// Fetch the failing CI log, spawn a targeted fix agent, and push the fix.
    ///
    /// This is the one-command CI failure recovery path:
    ///   1. Fetches failing check logs from the PR.
    ///   2. Spawns a micro-fix agent with the error log as context.
    ///      The agent is constrained to only touch files mentioned in the error.
    ///   3. After the agent exits, commits and pushes to the existing PR branch.
    ///      CI re-runs automatically.
    ///
    /// Use --no-push to commit locally without pushing (for review before push).
    ///
    /// Examples:
    ///   ta pr fix 2159d87e             # fix and push
    ///   ta pr fix 2159d87e --no-push   # fix, commit, but don't push
    Fix {
        /// Goal shortref, draft shortref/seq, draft ID prefix, or title substring.
        shortref: String,
        /// Agent to use for the fix (default: claude-code).
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Set up the workspace but don't launch the agent.
        #[arg(long)]
        no_launch: bool,
        /// Commit the fix locally but don't push to the remote branch.
        #[arg(long)]
        no_push: bool,
    },
}

/// Dispatch PR commands.
pub fn execute(cmd: &PrCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        PrCommands::Checks { shortref } => pr_checks(config, shortref),
        PrCommands::Fix {
            shortref,
            agent,
            no_launch,
            no_push,
        } => pr_fix(config, shortref, agent, *no_launch, *no_push),
        // Legacy commands — delegate to draft::execute.
        other => {
            let draft_cmd = to_draft_command(other);
            draft::execute(&draft_cmd, config)
        }
    }
}

// ── ta pr checks ──────────────────────────────────────────────────────────────

/// Poll CI check status for an open PR.
///
/// Prints a table of all CI checks with pass/fail/pending status.
/// Emits a `PrCheckFailed` event and exits non-zero if any check failed.
fn pr_checks(config: &GatewayConfig, shortref: &str) -> anyhow::Result<()> {
    use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};

    // Resolve shortref → draft package.
    let pkg_id_str = draft::resolve_draft_id_flexible(config, Some(shortref))?;
    let package_id = Uuid::parse_str(&pkg_id_str).map_err(|e| {
        anyhow::anyhow!("Invalid draft ID after resolution: {} — {}", pkg_id_str, e)
    })?;
    let pkg = draft::load_package(config, package_id)?;

    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Apply with `ta draft apply --submit` first.",
            &package_id.to_string()[..8],
        )
    })?;

    let pr_url = vcs.review_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no PR URL. The branch '{}' may have been pushed without creating a PR.\n\
             Create the PR with: ta draft reopen-review {}",
            &package_id.to_string()[..8],
            vcs.branch,
            &package_id.to_string()[..8],
        )
    })?;

    println!("=== CI Checks: {} ===\n", pkg.goal.title);
    println!("Draft:   {}", &package_id.to_string()[..8]);
    println!("Branch:  {}", vcs.branch);
    println!("PR:      {}", pr_url);

    // Fetch live check status via `gh`.
    let gh_output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            pr_url,
            "--json",
            "state,statusCheckRollup,title",
        ])
        .current_dir(&config.workspace_root)
        .output();

    let mut failed_checks: Vec<String> = Vec::new();
    let mut all_check_names: Vec<String> = Vec::new();

    match gh_output {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                let state = data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!("State:   {}\n", state);

                if let Some(checks) = data.get("statusCheckRollup").and_then(|v| v.as_array()) {
                    if checks.is_empty() {
                        println!("No CI checks found for this PR.");
                    } else {
                        println!("{:<42} {:<8} DETAILS", "CHECK", "STATUS");
                        println!("{}", "-".repeat(90));

                        for check in checks {
                            let name = check
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let conclusion = check
                                .get("conclusion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            let details_url = check
                                .get("detailsUrl")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-");

                            all_check_names.push(name.to_string());

                            let (status_label, is_failed) = match conclusion {
                                "SUCCESS" => ("PASS", false),
                                "NEUTRAL" | "SKIPPED" => ("SKIP", false),
                                "FAILURE" | "ERROR" => ("FAIL", true),
                                _ if status == "IN_PROGRESS" || status == "QUEUED" => {
                                    ("PENDING", false)
                                }
                                _ => ("?", false),
                            };

                            if is_failed {
                                failed_checks.push(name.to_string());
                            }

                            let name_display = if name.len() > 40 {
                                format!("{}...", &name[..37])
                            } else {
                                name.to_string()
                            };
                            println!("{:<42} {:<8} {}", name_display, status_label, details_url);
                        }

                        println!();
                        let total = checks.len();
                        let passed = total - failed_checks.len();
                        println!(
                            "Result:  {}/{} passed{}",
                            passed,
                            total,
                            if failed_checks.is_empty() {
                                String::new()
                            } else {
                                format!(", {} failed", failed_checks.len())
                            }
                        );
                    }
                } else {
                    println!("\nNo CI checks found for this PR (statusCheckRollup is empty).");
                }
            } else {
                anyhow::bail!(
                    "Could not parse `gh pr view` output. Is the PR URL valid?\n  PR: {}",
                    pr_url
                );
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "Failed to fetch PR status from GitHub.\n\
                 Command: gh pr view {} --json state,statusCheckRollup,title\n\
                 Error: {}",
                pr_url,
                stderr.trim()
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Could not run `gh` to check PR status — is GitHub CLI installed and authenticated?\n\
                 Install: https://cli.github.com\n\
                 Error: {}",
                e
            );
        }
    }

    if !failed_checks.is_empty() {
        // Emit PrCheckFailed event for shell notifications and channel routing.
        let goal_id = Uuid::parse_str(&pkg.goal.goal_id).unwrap_or(Uuid::nil());
        {
            let events_dir = config.workspace_root.join(".ta").join("events");
            let event_store = FsEventStore::new(&events_dir);
            let event = SessionEvent::PrCheckFailed {
                goal_id,
                draft_id: package_id,
                pr_url: pr_url.to_string(),
                branch: vcs.branch.clone(),
                failed_checks: failed_checks.clone(),
                title: pkg.goal.title.clone(),
            };
            if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
                tracing::warn!("Failed to persist PrCheckFailed event: {}", e);
            }
        }

        println!();
        println!("Failed checks:");
        for check in &failed_checks {
            println!("  - {}", check);
        }
        println!();
        println!("To fix automatically:  ta pr fix {}", shortref);
        println!(
            "Manual follow-up:      ta draft follow-up {} --ci-failure",
            &package_id.to_string()[..8]
        );

        // Exit non-zero so scripts and CI can detect failures.
        std::process::exit(1);
    }

    Ok(())
}

// ── ta pr fix ─────────────────────────────────────────────────────────────────

/// Spawn a micro-fix agent to address CI failures, then push the fix commit.
///
/// Fetches failing CI logs, writes a scoped context file, launches the agent
/// constrained to files mentioned in the error, and (unless --no-push) commits
/// and pushes to the existing PR branch so CI re-runs automatically.
fn pr_fix(
    config: &GatewayConfig,
    shortref: &str,
    agent: &str,
    no_launch: bool,
    no_push: bool,
) -> anyhow::Result<()> {
    // Resolve shortref → draft package.
    let pkg_id_str = draft::resolve_draft_id_flexible(config, Some(shortref))?;
    let package_id = Uuid::parse_str(&pkg_id_str).map_err(|e| {
        anyhow::anyhow!("Invalid draft ID after resolution: {} — {}", pkg_id_str, e)
    })?;
    let pkg = draft::load_package(config, package_id)?;

    let vcs = pkg.vcs_status.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no VCS tracking info. Apply with `ta draft apply --submit` first.",
            &package_id.to_string()[..8],
        )
    })?;

    let pr_url = vcs.review_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Draft {} has no PR URL. Create it first with: ta draft reopen-review {}",
            &package_id.to_string()[..8],
            &package_id.to_string()[..8],
        )
    })?;

    let branch = &vcs.branch;
    println!(
        "PR Fix: {} (draft {}, branch {})",
        pkg.goal.title,
        &package_id.to_string()[..8],
        branch,
    );

    // Find the source directory for this goal.
    let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir)?;
    let goals = goal_store.list()?;
    let goal = goals
        .iter()
        .find(|g| g.pr_package_id == Some(package_id))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No goal record found for draft {}. Cannot determine source directory.",
                &package_id.to_string()[..8],
            )
        })?;

    let target_dir = goal
        .source_dir
        .clone()
        .unwrap_or_else(|| config.workspace_root.clone());

    // Fetch failing CI logs.
    println!("Fetching CI failure context from PR...");
    let ci_context = fetch_ci_failure_log(pr_url, &target_dir);
    let basic_context = draft::fetch_ci_failure_context(pr_url, &target_dir);

    let ci_log = if !ci_context.is_empty() {
        ci_context
    } else if !basic_context.is_empty() {
        basic_context
    } else {
        println!("  No CI failures found or `gh` not available — proceeding without log context.");
        String::new()
    };

    if !ci_log.is_empty() {
        println!("  Fetched CI failure context ({} chars).", ci_log.len());
    }

    // Write a scoped micro-fix context file.
    let context_path = target_dir.join(".ta/pr-fix-context.md");
    fs::create_dir_all(target_dir.join(".ta"))?;

    let context = format!(
        "# PR CI Fix Context\n\n\
         **PR**: {pr_url}\n\
         **Branch**: {branch}\n\
         **Goal**: {title}\n\n\
         ## CI Failures\n\n\
         {ci_section}\n\n\
         ## Instructions\n\n\
         Fix the CI failures listed above. Important constraints:\n\
         - Only modify files that are directly related to the error messages\n\
         - Do NOT modify PLAN.md or any planning/documentation files\n\
         - Do NOT refactor or improve code beyond what is needed to fix the CI failure\n\
         - Make the minimal change needed to make CI pass\n\
         - After making changes, verify your fix addresses the specific error shown\n",
        pr_url = pr_url,
        branch = branch,
        title = pkg.goal.title,
        ci_section = if ci_log.is_empty() {
            "No CI log available. Check the PR URL above for the current check status.".to_string()
        } else {
            format!("```\n{}\n```", ci_log)
        },
    );

    fs::write(&context_path, &context)?;
    println!("  Context written to: {}", context_path.display());

    if no_launch {
        println!("\n-- Setup complete (--no-launch) --");
        println!("  Branch:     {}", branch);
        println!("  Target dir: {}", target_dir.display());
        println!("  Context:    {}", context_path.display());
        println!("\nTo start the fix manually:");
        println!("  cd {}", target_dir.display());
        println!("  git checkout {}", branch);
        return Ok(());
    }

    // Checkout the PR branch.
    println!("\nChecking out branch '{}'...", branch);
    let checkout = std::process::Command::new("git")
        .args(["checkout", branch])
        .current_dir(&target_dir)
        .status();

    match checkout {
        Ok(s) if s.success() => println!("  On branch '{}'.", branch),
        Ok(s) => anyhow::bail!(
            "Failed to checkout branch '{}' (exit {}). Try `git fetch` first.",
            branch,
            s.code().unwrap_or(-1),
        ),
        Err(e) => anyhow::bail!("Failed to run git checkout: {}. Is git in PATH?", e),
    }

    // Launch the micro-fix agent.
    let objective = format!(
        "Fix CI failures for PR on branch '{}'. \
         Read .ta/pr-fix-context.md for the failing checks and error logs. \
         Only modify files directly mentioned in the error — do not touch PLAN.md or \
         unrelated files. Make the minimal change to fix CI.",
        branch,
    );

    println!("\nLaunching {} for micro-fix...", agent);
    println!("  Objective: {}", &objective[..objective.len().min(120)]);

    let agent_args: Vec<String> = match agent {
        "claude-code" => vec!["claude".into(), "--print".into(), objective],
        other => vec![other.into(), objective],
    };

    let status = std::process::Command::new(&agent_args[0])
        .args(&agent_args[1..])
        .current_dir(&target_dir)
        .status();

    match &status {
        Ok(s) if s.success() => println!("\nAgent completed successfully."),
        Ok(s) => {
            eprintln!(
                "\nWarning: agent exited with code {}. Proceeding with commit/push.",
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => anyhow::bail!("Failed to launch agent '{}': {}", agent, e),
    }

    // Commit the fix.
    println!("\nStaging changes...");
    let add_result = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&target_dir)
        .status();

    match add_result {
        Ok(s) if s.success() => {}
        Ok(s) => anyhow::bail!("git add failed (exit {})", s.code().unwrap_or(-1)),
        Err(e) => anyhow::bail!("Failed to run git add: {}", e),
    }

    // Check if there is anything to commit.
    let diff_check = std::process::Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(&target_dir)
        .status();

    let has_staged = match diff_check {
        Ok(s) => !s.success(), // exit 1 means there are staged changes
        Err(_) => true,        // assume there are changes if we can't check
    };

    if !has_staged {
        println!("No changes staged — agent may not have modified any files.");
        println!("Check the output above and the CI failure log for details.");
        return Ok(());
    }

    let commit_msg = format!(
        "ci: fix failing CI check(s)\n\n\
         Auto-fix spawned by `ta pr fix {}` (draft {}).\n\
         Failing checks: {}",
        shortref,
        &package_id.to_string()[..8],
        failed_check_summary(&ci_log),
    );

    let commit_result = std::process::Command::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(&target_dir)
        .status();

    match commit_result {
        Ok(s) if s.success() => println!("  Committed fix to branch '{}'.", branch),
        Ok(s) => anyhow::bail!("git commit failed (exit {})", s.code().unwrap_or(-1)),
        Err(e) => anyhow::bail!("Failed to run git commit: {}", e),
    }

    if no_push {
        println!("\n-- Fix committed (--no-push) --");
        println!("  Branch:  {}", branch);
        println!("  To push: cd {} && git push", target_dir.display());
        return Ok(());
    }

    // Push to the PR branch.
    println!("Pushing to origin/{}...", branch);
    let push_result = std::process::Command::new("git")
        .args(["push", "origin", branch])
        .current_dir(&target_dir)
        .status();

    match push_result {
        Ok(s) if s.success() => {
            println!("  Pushed. CI will re-run automatically.");
            println!("\nMonitor: ta pr checks {}", shortref);
        }
        Ok(s) => anyhow::bail!(
            "git push failed (exit {}). Check your remote permissions and try:\n  cd {} && git push origin {}",
            s.code().unwrap_or(-1),
            target_dir.display(),
            branch,
        ),
        Err(e) => anyhow::bail!("Failed to run git push: {}", e),
    }

    Ok(())
}

/// Fetch detailed CI failure logs using `gh run view --log --log-failed`.
///
/// This gives the actual log output of failing steps, not just check names.
/// Falls back to empty string if `gh` is unavailable or no failures are found.
fn fetch_ci_failure_log(pr_url: &str, working_dir: &std::path::Path) -> String {
    // First get the list of failed check run IDs from the PR.
    let checks = std::process::Command::new("gh")
        .args(["pr", "view", pr_url, "--json", "statusCheckRollup"])
        .current_dir(working_dir)
        .output();

    let run_ids: Vec<String> = match checks {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                data.get("statusCheckRollup")
                    .and_then(|v| v.as_array())
                    .map(|checks| {
                        checks
                            .iter()
                            .filter(|c| {
                                matches!(
                                    c.get("conclusion").and_then(|v| v.as_str()),
                                    Some("FAILURE") | Some("ERROR")
                                )
                            })
                            .filter_map(|c| {
                                // GitHub Actions check runs have a databaseId field.
                                c.get("databaseId")
                                    .and_then(|v| v.as_u64())
                                    .map(|id| id.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    if run_ids.is_empty() {
        return String::new();
    }

    // Fetch the log for the first failing run (log output can be large).
    let first_id = &run_ids[0];
    let log_output = std::process::Command::new("gh")
        .args(["run", "view", first_id, "--log-failed"])
        .current_dir(working_dir)
        .output();

    match log_output {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            // Truncate to 4000 chars to avoid overwhelming the agent context.
            let truncated = if raw.len() > 4000 {
                format!(
                    "{}...\n[truncated — {} chars total]",
                    &raw[..4000],
                    raw.len()
                )
            } else {
                raw.to_string()
            };
            truncated
        }
        _ => String::new(),
    }
}

/// Summarise failing checks from CI log for the commit message.
///
/// Returns a comma-separated list of check names if they can be extracted,
/// or a generic fallback.
fn failed_check_summary(ci_log: &str) -> String {
    if ci_log.is_empty() {
        return "see PR CI checks".to_string();
    }
    // Try to extract check names from lines like "X Check Name" in the log.
    let names: Vec<&str> = ci_log
        .lines()
        .filter(|l| l.contains("FAIL") || l.starts_with("X ") || l.contains("failed"))
        .take(3)
        .collect();
    if names.is_empty() {
        "see PR CI checks".to_string()
    } else {
        names.join(", ")
    }
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
            apply_context_file: None,
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
            file: file.as_ref().map(|f| vec![f.clone()]).unwrap_or_default(),
            open_external: *open_external,
            detail: detail.clone(),
            format: format.clone(),
            color: *color,
            json: false,
            section: None,
        },
        PrCommands::Approve { id, reviewer } => draft::DraftCommands::Approve {
            id: Some(id.clone()),
            reviewer: reviewer.clone(),
            reviewer_as: None,
            force_override: false,
        },
        PrCommands::Deny {
            id,
            reason,
            reviewer,
        } => draft::DraftCommands::Deny {
            id: Some(id.clone()),
            reason: reason.clone(),
            reviewer: reviewer.clone(),
            file: None,
        },
        PrCommands::Apply {
            id,
            target,
            submit,
            no_submit,
            review,
            no_review,
            dry_run,
            git_commit,
            git_push,
            conflict_resolution,
            approve_patterns,
            reject_patterns,
            discuss_patterns,
        } => draft::DraftCommands::Apply {
            id: Some(id.clone()),
            target: target.clone(),
            submit: *submit,
            no_submit: *no_submit,
            review: *review,
            no_review: *no_review,
            dry_run: *dry_run,
            git_commit: *git_commit,
            git_push: *git_push,
            conflict_resolution: conflict_resolution.clone(),
            approve_patterns: approve_patterns.clone(),
            reject_patterns: reject_patterns.clone(),
            discuss_patterns: discuss_patterns.clone(),
            skip_verify: false,
            phase: None,
            require_review: false,
            watch: false,
            chain: false,
            force_apply: false,
            status: false,
        },
        // Checks and Fix are handled before reaching this function.
        PrCommands::Checks { .. } | PrCommands::Fix { .. } => {
            unreachable!("Checks and Fix are dispatched before to_draft_command")
        }
    }
}
