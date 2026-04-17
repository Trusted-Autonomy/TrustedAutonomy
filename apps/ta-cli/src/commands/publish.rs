// publish.rs — One-step publish: apply approved draft, commit, push, and create PR.
//
// `ta publish` is a convenience command that:
//   1. Finds the most recent approved draft in .ta/pr-packages/
//   2. Prompts for (or accepts) a commit message
//   3. Applies the draft (calls `ta draft apply <id>`)
//   4. Stages and commits the changes with git
//   5. Pushes to the remote (if git is configured)
//   6. Creates a GitHub PR (if `gh` CLI is available)

use std::cmp::Reverse;
use std::path::Path;
use std::process::Command;

use ta_changeset::draft_package::{DraftPackage, DraftStatus};

/// Find the most-recently-created approved draft in .ta/pr-packages/.
fn find_latest_approved(project_root: &Path) -> anyhow::Result<Option<DraftPackage>> {
    let packages_dir = project_root.join(".ta").join("pr-packages");
    if !packages_dir.exists() {
        return Ok(None);
    }

    let mut candidates: Vec<DraftPackage> = Vec::new();

    for entry in std::fs::read_dir(&packages_dir).map_err(|e| {
        anyhow::anyhow!(
            "Could not read draft packages directory '{}': {e}",
            packages_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let pkg: DraftPackage = match serde_json::from_str(&content) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if matches!(pkg.status, DraftStatus::Approved { .. }) {
            candidates.push(pkg);
        }
    }

    if candidates.is_empty() {
        return Ok(None);
    }

    // Sort by created_at descending.
    candidates.sort_by_key(|c| Reverse(c.created_at));
    Ok(candidates.into_iter().next())
}

/// Prompt the user for a line of input from stdin.
fn prompt(prompt_text: &str, default: Option<&str>) -> String {
    if let Some(d) = default {
        print!("{} [{}]: ", prompt_text, d);
    } else {
        print!("{}: ", prompt_text);
    }
    // Flush stdout so the prompt appears before the input.
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        default.map(str::to_string).unwrap_or_default()
    } else {
        trimmed
    }
}

/// Check whether a command-line tool is available on PATH.
fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Run a shell command in the given directory, printing output.
fn run_cmd(dir: &Path, program: &str, args: &[&str]) -> anyhow::Result<bool> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to run '{} {}': {e}\n\
                 Make sure '{program}' is installed and on your PATH.",
                program,
                args.join(" ")
            )
        })?;
    Ok(status.success())
}

/// Run a command and capture its stdout as a String.
fn capture_cmd(dir: &Path, program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run '{program}': {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Execute `ta publish`: apply the latest approved draft, commit, push, open PR.
pub fn execute(project_root: &Path, message: Option<&str>, auto_yes: bool) -> anyhow::Result<()> {
    // 1. Find latest approved draft.
    let draft = find_latest_approved(project_root)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No approved drafts found in .ta/pr-packages/.\n\
             Approve a draft first with: ta draft approve <id>\n\
             Or review pending drafts with: ta draft list"
        )
    })?;

    let draft_id = draft.package_id.to_string();
    let draft_title = draft.goal.title.clone();

    println!(
        "Found approved draft: {} ({}...)",
        draft_title,
        &draft_id[..8]
    );
    println!("  {} artifact(s)", draft.changes.artifacts.len());
    println!();

    // 2. Determine commit message.
    let commit_msg = if let Some(m) = message {
        m.to_string()
    } else if auto_yes {
        draft_title.clone()
    } else {
        prompt("Commit message", Some(&draft_title))
    };

    if commit_msg.is_empty() {
        anyhow::bail!(
            "Commit message is required.\n\
             Provide one with --message or enter it at the prompt."
        );
    }

    // 3. Apply the draft.
    println!("Applying draft {}...", &draft_id[..8]);
    let apply_ok = run_cmd(project_root, "ta", &["draft", "apply", &draft_id])?;
    if !apply_ok {
        anyhow::bail!(
            "Draft apply failed for draft {}.\n\
             Run `ta draft apply {}` manually to diagnose.",
            &draft_id[..8],
            draft_id
        );
    }
    println!("Draft applied successfully.");
    println!();

    // 4. Git operations.
    if !tool_available("git") {
        println!("git is not available — skipping commit and push.");
        println!("Install git to enable automatic commit and push.");
        return Ok(());
    }

    // Check if we're in a git repo.
    let git_root = capture_cmd(project_root, "git", &["rev-parse", "--show-toplevel"]);
    if git_root.is_err() {
        println!("Not inside a git repository — skipping commit and push.");
        return Ok(());
    }

    // Stage all changes.
    println!("Staging changes...");
    let stage_ok = run_cmd(project_root, "git", &["add", "-A"])?;
    if !stage_ok {
        anyhow::bail!(
            "git add -A failed.\n\
             Check the git status with `git status` and resolve any issues."
        );
    }

    // Check if there's anything to commit.
    let status_out = capture_cmd(project_root, "git", &["status", "--porcelain"])?;
    // If nothing staged, nothing to commit.
    let staged_out = capture_cmd(project_root, "git", &["diff", "--cached", "--name-only"])?;
    if staged_out.is_empty() && status_out.is_empty() {
        println!("Nothing to commit — working tree is clean.");
        println!("The draft may have already been applied.");
        return Ok(());
    }

    // Commit.
    println!("Committing: {}", commit_msg);
    let commit_ok = run_cmd(project_root, "git", &["commit", "-m", &commit_msg])?;
    if !commit_ok {
        anyhow::bail!(
            "git commit failed.\n\
             Review the error above and commit manually with: git commit -m \"{}\"",
            commit_msg
        );
    }
    println!("Committed successfully.");
    println!();

    // 5. Push.
    let should_push = auto_yes || {
        let ans = prompt("Push to remote?", Some("y"));
        ans.eq_ignore_ascii_case("y") || ans.eq_ignore_ascii_case("yes")
    };

    if should_push {
        // Determine current branch.
        let branch = capture_cmd(project_root, "git", &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "HEAD".to_string());

        println!("Pushing branch '{}'...", branch);
        let push_ok = run_cmd(project_root, "git", &["push", "-u", "origin", &branch])?;
        if !push_ok {
            println!(
                "Push failed. You can push manually with: git push -u origin {}",
                branch
            );
        } else {
            println!("Pushed to origin/{}.", branch);
            println!();

            // 6. Create GitHub PR if gh is available.
            if tool_available("gh") {
                let should_pr = auto_yes || {
                    let ans = prompt("Create a GitHub pull request?", Some("y"));
                    ans.eq_ignore_ascii_case("y") || ans.eq_ignore_ascii_case("yes")
                };

                if should_pr {
                    let pr_body = format!(
                        "## Summary\n\nApplied TA draft `{}`.\n\n{}\n\n## Draft ID\n\n`{}`",
                        &draft_id[..8],
                        draft_title,
                        draft_id
                    );
                    println!("Creating GitHub pull request...");
                    let pr_ok = run_cmd(
                        project_root,
                        "gh",
                        &["pr", "create", "--title", &commit_msg, "--body", &pr_body],
                    )?;
                    if !pr_ok {
                        println!(
                            "PR creation failed. Create manually with:\n\
                             gh pr create --title \"{}\" --body \"Applied draft {}\"",
                            commit_msg,
                            &draft_id[..8]
                        );
                    }
                }
            } else {
                println!("Tip: install the GitHub CLI (gh) to create pull requests automatically.");
            }
        }
    } else {
        println!("Skipped push. Push manually with: git push -u origin <branch>");
    }

    println!();
    println!("Publish complete.");
    Ok(())
}
