//! Git-specific helpers for the release pipeline.
//!
//! All direct `Command::new("git")` calls for release operations live here.
//! This is the only location in ta-cli (outside ta-submit) where direct git
//! calls are permitted for release workflows. Routing through this module
//! satisfies the VCS adapter enforcement rule (v0.15.29).

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Check whether the working tree has any uncommitted changes (staged or unstaged).
pub fn git_is_dirty(root: &Path) -> bool {
    let unstaged = Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(root)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(root)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    unstaged || staged
}

/// Return all existing tag names in the repository.
pub fn git_tags(root: &Path) -> HashSet<String> {
    Command::new("git")
        .args(["tag", "-l"])
        .current_dir(root)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Return the current HEAD commit SHA, or None if unavailable.
pub fn git_head_sha(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Validate that a tag exists in the repository.
/// Returns the tag name on success, or an error with a user-actionable message.
pub fn git_verify_tag(root: &Path, tag: &str) -> anyhow::Result<String> {
    let check = Command::new("git")
        .args(["rev-parse", "--verify", tag])
        .current_dir(root)
        .output();
    match check {
        Ok(out) if out.status.success() => Ok(tag.to_string()),
        _ => anyhow::bail!(
            "Tag '{}' not found in this repository.\nRun `git tag` to list available tags.",
            tag
        ),
    }
}

/// Collect commit subjects since the given tag (or all commits if tag is None).
/// Returns (commit_subjects_joined, last_tag_used).
pub fn git_log_since_tag(
    root: &Path,
    from_tag: Option<&str>,
) -> anyhow::Result<(String, Option<String>)> {
    let last_tag = if let Some(tag) = from_tag {
        git_verify_tag(root, tag)?;
        Some(tag.to_string())
    } else {
        // Try git describe for the most recent tag.
        let out = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .current_dir(root)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            _ => None,
        }
    };

    let log_args: Vec<String> = match &last_tag {
        Some(tag) => vec![
            "log".to_string(),
            format!("{}..HEAD", tag),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
        None => vec![
            "log".to_string(),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
    };

    let output = Command::new("git")
        .args(&log_args)
        .current_dir(root)
        .output()?;

    let commits = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((commits, last_tag))
}

/// Stage a path with `git add`.
pub fn git_add(root: &Path, path: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["add", path])
        .current_dir(root)
        .status()?;
    if !status.success() {
        tracing::warn!("git add {} returned non-zero exit code", path);
    }
    Ok(())
}

/// Commit with the given message.
pub fn git_commit(root: &Path, message: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!("git commit failed — check `git status` for details");
    }
    Ok(())
}

/// Amend the last commit.
#[allow(dead_code)]
pub fn git_commit_amend(root: &Path, message: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["commit", "--amend", "--no-edit", "-m", message])
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!("git commit --amend failed — check `git status` for details");
    }
    Ok(())
}

/// Push the current branch to the given remote.
pub fn git_push(root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd_args = vec!["push", remote];
    cmd_args.extend_from_slice(args);
    let status = Command::new("git")
        .args(&cmd_args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "git push {} failed — check your remote access and try again",
            remote
        );
    }
    Ok(())
}

/// Get the URL of a remote (e.g., "origin").
pub fn git_remote_url(root: &Path, remote: &str) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("Cannot run git remote get-url {}: {}", remote, e))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Get the output of `git log` with a custom format string.
#[allow(dead_code)]
pub fn git_log_format(root: &Path, format: &str, range: Option<&str>) -> anyhow::Result<String> {
    let format_arg = format!("--pretty=format:{}", format);
    let mut args = vec!["log", &format_arg];
    if let Some(r) = range {
        args.push(r);
    }
    let output = Command::new("git").args(&args).current_dir(root).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git log failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
