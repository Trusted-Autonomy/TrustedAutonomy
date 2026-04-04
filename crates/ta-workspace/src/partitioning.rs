// partitioning.rs — Canonical split between shared (VCS-committed) and local
// (VCS-ignored) .ta/ files.
//
// This is the authoritative source of truth used by:
//   - `ta setup vcs` — ignore generation for Git and Perforce
//   - `ta plan shared` — display shared/local split
//   - `ta doctor` — verify local paths are properly ignored

/// Paths inside `.ta/` that should be committed to VCS and shared with the team.
/// These are configuration and agent definition files — they encode team policy.
pub const SHARED_TA_PATHS: &[&str] = &[
    "workflow.toml",
    "policy.yaml",
    "constitution.toml",
    "memory.toml",
    "bmad.toml",
    "agents/",
    "constitutions/",
    "memory/",
    "templates/",
    "plan_history.jsonl",   // append-only audit trail of plan phase completions
    "release-history.json", // append-only project release changelog
];

/// Paths inside `.ta/` that are local runtime state and must NOT be committed.
/// Committing these by accident causes clutter, credential leaks, and merge conflicts.
pub const LOCAL_TA_PATHS: &[&str] = &[
    "daemon.toml",
    "daemon.local.toml",
    "workflow.local.toml",
    "local.workflow.toml", // deprecated name — kept so existing files stay gitignored
    "memory.rvf",
    "staging/",
    "store/",
    "goals/",
    "events/",
    "sessions/",
    "release.lock",
    "velocity-stats.jsonl",
    "audit-ledger.jsonl",
    "taignore",
    "interactions/",
];

/// VCS backend detected or configured for the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcsBackend {
    Git,
    Perforce,
    None,
}

impl VcsBackend {
    /// Detect the VCS backend for the given project root.
    ///
    /// Check order:
    /// 1. `.git/` directory present — Git
    /// 2. `.p4config` in any parent directory or `P4PORT`/`P4CLIENT` env vars — Perforce
    /// 3. Otherwise — None
    pub fn detect(project_root: &std::path::Path) -> Self {
        // Git: .git/ directory exists, or git rev-parse succeeds.
        if project_root.join(".git").exists() {
            return Self::Git;
        }
        // Try git rev-parse (handles worktrees and submodules).
        // Clear TA agent VCS isolation env vars (set by v0.13.17.3) so we
        // detect VCS based on project_root's own repo, not the staging dir.
        let git_ok = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(project_root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if git_ok {
            return Self::Git;
        }

        // Perforce: P4PORT or P4CLIENT env vars, or .p4config in ancestor dirs.
        if std::env::var("P4PORT").is_ok() || std::env::var("P4CLIENT").is_ok() {
            return Self::Perforce;
        }
        let mut dir = Some(project_root);
        while let Some(d) = dir {
            if d.join(".p4config").exists() {
                return Self::Perforce;
            }
            dir = d.parent();
        }

        Self::None
    }

    /// Machine-readable name for workflow.toml serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Perforce => "perforce",
            Self::None => "none",
        }
    }
}

/// Check whether a given `.ta/` relative path is currently ignored by Git.
///
/// Returns `Ok(true)` if ignored, `Ok(false)` if tracked/untracked-but-not-ignored.
/// Returns `Err` if the check itself failed (not a git repo, git not on PATH, etc.).
pub fn git_is_ignored(project_root: &std::path::Path, ta_rel_path: &str) -> Result<bool, String> {
    let path = format!(".ta/{}", ta_rel_path);
    let output = std::process::Command::new("git")
        .args(["check-ignore", "-q", &path])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git check-ignore failed: {}", e))?;
    // exit 0 = ignored, exit 1 = not ignored, other = error
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) {
        Ok(false)
    } else {
        Err(format!(
            "git check-ignore returned unexpected status: {:?}",
            output.status.code()
        ))
    }
}

/// The marker line used in .gitignore to identify the TA-managed block.
pub const GITIGNORE_MARKER: &str = "# Trusted Autonomy — local runtime state (do not commit)";

/// Build the `.gitignore` block for all local TA paths.
pub fn gitignore_block() -> String {
    let mut block = String::new();
    block.push_str(GITIGNORE_MARKER);
    block.push('\n');
    for path in LOCAL_TA_PATHS {
        block.push_str(&format!(".ta/{}\n", path));
    }
    block
}

/// Append (or update) the TA block in the given `.gitignore` content.
///
/// Idempotent: if the marker is already present, replaces the existing block.
/// If `force` is true, always rewrites the block even if already present.
pub fn update_gitignore(existing: &str, force: bool) -> (String, bool) {
    if !force && existing.contains(GITIGNORE_MARKER) {
        // Already present — nothing to do.
        return (existing.to_string(), false);
    }

    // Remove any existing TA block (marker line + the .ta/ lines that follow).
    let mut result = String::new();
    let mut in_ta_block = false;
    for line in existing.lines() {
        if line.trim() == GITIGNORE_MARKER.trim() {
            in_ta_block = true;
            continue;
        }
        if in_ta_block {
            // Skip lines that belong to the old TA block (.ta/... lines).
            if line.starts_with(".ta/") {
                continue;
            }
            in_ta_block = false;
        }
        result.push_str(line);
        result.push('\n');
    }

    // Ensure there's exactly one blank line separator before our block.
    if !result.is_empty() && !result.ends_with("\n\n") {
        if result.ends_with('\n') {
            result.push('\n');
        } else {
            result.push_str("\n\n");
        }
    }

    result.push_str(&gitignore_block());
    (result, true)
}

/// The marker line used in .p4ignore to identify the TA-managed block.
pub const P4IGNORE_MARKER: &str = "# Trusted Autonomy — local runtime state (do not submit)";

/// Build the `.p4ignore` block for all local TA paths.
pub fn p4ignore_block() -> String {
    let mut block = String::new();
    block.push_str(P4IGNORE_MARKER);
    block.push('\n');
    for path in LOCAL_TA_PATHS {
        block.push_str(&format!(".ta/{}\n", path));
    }
    block
}

/// Append (or update) the TA block in the given `.p4ignore` content.
///
/// Idempotent: if the marker is already present, skips unless `force`.
pub fn update_p4ignore(existing: &str, force: bool) -> (String, bool) {
    if !force && existing.contains(P4IGNORE_MARKER) {
        return (existing.to_string(), false);
    }

    let mut result = String::new();
    let mut in_ta_block = false;
    for line in existing.lines() {
        if line.trim() == P4IGNORE_MARKER.trim() {
            in_ta_block = true;
            continue;
        }
        if in_ta_block {
            if line.starts_with(".ta/") {
                continue;
            }
            in_ta_block = false;
        }
        result.push_str(line);
        result.push('\n');
    }

    if !result.is_empty() && !result.ends_with("\n\n") {
        if result.ends_with('\n') {
            result.push('\n');
        } else {
            result.push_str("\n\n");
        }
    }
    result.push_str(&p4ignore_block());
    (result, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_block_contains_all_local_paths() {
        let block = gitignore_block();
        assert!(block.contains(GITIGNORE_MARKER));
        for path in LOCAL_TA_PATHS {
            assert!(
                block.contains(&format!(".ta/{}", path)),
                "missing .ta/{} in gitignore block",
                path
            );
        }
    }

    #[test]
    fn update_gitignore_appends_to_empty() {
        let (result, changed) = update_gitignore("", false);
        assert!(changed);
        assert!(result.contains(GITIGNORE_MARKER));
        assert!(result.contains(".ta/staging/"));
    }

    #[test]
    fn update_gitignore_idempotent() {
        let (first, _) = update_gitignore("# existing\n", false);
        let (second, changed) = update_gitignore(&first, false);
        assert!(!changed, "second run should not change content");
        assert_eq!(first, second);
    }

    #[test]
    fn update_gitignore_force_rewrites() {
        let existing =
            "# existing\n# Trusted Autonomy — local runtime state (do not commit)\n.ta/staging/\n";
        let (result, changed) = update_gitignore(existing, true);
        assert!(changed);
        // Should contain all LOCAL_TA_PATHS, not duplicates.
        let count = result.matches(GITIGNORE_MARKER).count();
        assert_eq!(
            count, 1,
            "should only have one TA block after force rewrite"
        );
    }

    #[test]
    fn update_gitignore_preserves_existing_entries() {
        let existing = "*.log\n/dist/\n";
        let (result, changed) = update_gitignore(existing, false);
        assert!(changed);
        assert!(result.contains("*.log"));
        assert!(result.contains("/dist/"));
        assert!(result.contains(GITIGNORE_MARKER));
    }

    #[test]
    fn p4ignore_block_contains_all_local_paths() {
        let block = p4ignore_block();
        assert!(block.contains(P4IGNORE_MARKER));
        for path in LOCAL_TA_PATHS {
            assert!(block.contains(&format!(".ta/{}", path)));
        }
    }

    #[test]
    fn update_p4ignore_idempotent() {
        let (first, _) = update_p4ignore("", false);
        let (second, changed) = update_p4ignore(&first, false);
        assert!(!changed);
        assert_eq!(first, second);
    }

    #[test]
    fn vcs_detect_no_git() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        // Plain directory — no .git, no P4 env vars (env may vary in CI, so
        // only check the non-Perforce path when env is clean).
        let vcs = VcsBackend::detect(dir.path());
        // May be Perforce if P4PORT is set in the test environment; that's OK.
        assert!(
            vcs == VcsBackend::None || vcs == VcsBackend::Perforce,
            "unexpected VCS: {:?}",
            vcs
        );
    }

    #[test]
    fn vcs_detect_git_dot_git_dir() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(VcsBackend::detect(dir.path()), VcsBackend::Git);
    }

    #[test]
    fn shared_paths_not_in_local() {
        for shared in SHARED_TA_PATHS {
            assert!(
                !LOCAL_TA_PATHS.contains(shared),
                "{} appears in both SHARED and LOCAL lists",
                shared
            );
        }
    }
}
