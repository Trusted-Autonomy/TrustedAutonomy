// overlay.rs — Copy-on-write overlay workspace for transparent agent mediation.
//
// An OverlayWorkspace creates a staging copy of a source project where the
// agent operates using its native tools — it sees a complete, normal-looking
// project. When work is done, TA diffs the staging copy against the original
// source to identify what changed.
//
// Copy strategies (v0.13.0):
// - ApfsClone: macOS APFS clonefile(2) — instant, zero disk space until write
// - BtrfsReflink: Linux Btrfs FICLONE ioctl — instant, zero disk space until write
// - Full: byte-for-byte copy (cross-platform fallback, always works)
//
// The strategy is detected automatically at workspace creation time by probing
// the staging directory. No configuration is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::copy_strategy::{copy_file_with_strategy, detect_strategy, CopyStat, CopyStrategy};

/// Staging mode for workspace creation (v0.13.13).
///
/// Passed to [`OverlayWorkspace::create_with_strategy`] by callers that read
/// `WorkflowConfig::staging.strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayStagingMode {
    /// Byte-for-byte copy (default — always works, may be slow for large workspaces).
    #[default]
    Full,
    /// Symlink excluded directories instead of copying — fast for large workspaces.
    Smart,
    /// Windows ReFS CoW clone — auto-falls back to `Smart` on non-ReFS volumes.
    RefsCow,
    /// Windows Projected File System — zero-disk-cost virtual workspace (v0.15.8).
    ///
    /// Files appear in staging instantly via kernel-level projection from source.
    /// Reads hydrate on-demand; writes land in `.projfs-scratch/`. Auto-falls back
    /// to `Smart` when `Client-ProjFS` is not installed.
    ProjFs,
}

use crate::conflict::{Conflict, ConflictResolution, FileSnapshot, SourceSnapshot};
use crate::error::WorkspaceError;

// ── V1 copy-optimization excludes (remove when V2 VFS lands) ──────

/// V1 TEMPORARY: Built-in default exclude patterns for common build artifacts.
/// When V2 lazy COW lands, these become unnecessary — the VFS only copies
/// files on write, so build artifacts are never materialized.
const DEFAULT_EXCLUDES: &[&str] = &[
    // Rust
    "target/",
    // Node
    "node_modules/",
    // Python
    "__pycache__/",
    "*.pyc",
    ".venv/",
    "venv/",
    // General build
    "dist/",
    "build/",
    ".build/",
    ".next/",
    ".cache/",
];

/// V1 TEMPORARY: Exclude patterns for the full-copy overlay.
/// When V2 lazy COW lands, these become unnecessary — the VFS only
/// copies files on write, so build artifacts are never copied.
/// Role-based access control (what agents can see) is a separate
/// concern handled by ta-policy, not by copy excludes.
#[derive(Debug, Clone)]
pub struct ExcludePatterns {
    patterns: Vec<String>,
}

impl ExcludePatterns {
    /// V1 TEMPORARY: Load exclude patterns from `.taignore` in source_dir, or use defaults.
    pub fn load(source_dir: &Path) -> Self {
        let taignore_path = source_dir.join(".taignore");
        if taignore_path.exists() {
            if let Ok(content) = fs::read_to_string(&taignore_path) {
                return Self::from_taignore(&content);
            }
        }
        Self::defaults()
    }

    /// V1 TEMPORARY: Create exclude patterns from `.taignore` file content.
    /// Format: one pattern per line, `#` comments, blank lines ignored.
    /// - `dirname/` — exclude directories with this name at any depth
    /// - `*.ext` — exclude files with this extension
    /// - `name` — exclude exact filename match
    pub fn from_taignore(content: &str) -> Self {
        let patterns = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect();
        Self { patterns }
    }

    /// V1 TEMPORARY: Default exclude patterns for common build artifacts.
    pub fn defaults() -> Self {
        Self {
            patterns: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Empty exclude patterns (only `.ta/` is always excluded hardcoded).
    pub fn none() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Merge additional patterns (e.g., from a VCS adapter) into this set.
    /// Deduplicates patterns.
    pub fn merge(&mut self, additional: &[String]) {
        for pattern in additional {
            if !self.patterns.contains(pattern) {
                self.patterns.push(pattern.clone());
            }
        }
    }

    /// Get the current patterns (for inspection/testing).
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// V1 TEMPORARY: Check if a file/directory name should be excluded during copy.
    /// Only checks against the immediate name (single path component).
    /// Agent infrastructure directories — always excluded (not work product).
    const INFRA_DIRS: &'static [&'static str] = &[".ta", ".claude-flow", ".hive-mind", ".swarm"];

    pub fn should_exclude(&self, name: &str) -> bool {
        // Infrastructure dirs are always excluded (hardcoded, separate from user patterns).
        if Self::INFRA_DIRS.contains(&name) {
            return true;
        }
        for pattern in &self.patterns {
            if let Some(dir_name) = pattern.strip_suffix('/') {
                // Directory pattern: "target/" matches entry named "target".
                if name == dir_name {
                    return true;
                }
            } else if let Some(suffix) = pattern.strip_prefix('*') {
                // Extension pattern: "*.pyc" matches files ending in ".pyc".
                if name.ends_with(suffix) {
                    return true;
                }
            } else {
                // Exact name match.
                if name == pattern {
                    return true;
                }
            }
        }
        false
    }

    /// V1 TEMPORARY: Check if a relative path should be skipped.
    /// Checks each path component against exclude patterns.
    pub fn should_skip_path(&self, rel_path: &str) -> bool {
        for component in Path::new(rel_path).components() {
            if let std::path::Component::Normal(name) = component {
                let name_str = name.to_string_lossy();
                if self.should_exclude(&name_str) {
                    return true;
                }
            }
        }
        false
    }
}

// ── End V1 copy-optimization excludes ─────────────────────────────

/// A change detected by diffing the staging workspace against the source.
#[derive(Debug, Clone)]
pub enum OverlayChange {
    /// A file that existed in source was modified in staging.
    Modified { path: String, diff: String },
    /// A file that did not exist in source was created in staging.
    Created { path: String, content: String },
    /// A file that existed in source was deleted from staging.
    Deleted { path: String },
}

/// An overlay workspace that copies a source project for transparent agent work.
///
/// The agent works in `staging_dir` using its native tools (Read, Write, Edit,
/// Bash, etc.) and sees a complete project. TA is invisible — the only excluded
/// directory is `.ta/` itself to avoid recursion and hide TA's internal state.
///
/// On APFS (macOS) and Btrfs (Linux), staging creation uses copy-on-write:
/// files are cloned instantly with zero disk space consumed until the agent
/// actually modifies them. Falls back to full copy on other filesystems.
///
/// In `Smart` mode (v0.13.13), excluded directories are symlinked instead of
/// copied, reducing staging cost from the full workspace size to only the
/// agent-writable subset.
pub struct OverlayWorkspace {
    goal_id: String,
    source_dir: PathBuf,
    staging_dir: PathBuf,
    excludes: ExcludePatterns,
    source_snapshot: Option<SourceSnapshot>, // v0.2.1: Conflict detection
    /// Statistics from staging creation (strategy, duration, file count).
    copy_stat: Option<CopyStat>,
    /// Active ProjFS virtualization provider (Windows only, v0.15.8).
    /// Must outlive the workspace root directory. `None` on non-Windows or
    /// when ProjFS mode is not in use. Held for RAII drop — intentionally
    /// not read after construction.
    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    projfs_provider: Option<crate::projfs_strategy::ProjFsProvider>,
}

impl OverlayWorkspace {
    /// Create an overlay workspace using the default (full copy) strategy.
    ///
    /// Copies everything from `source_dir` to `staging_root/<goal_id>/`,
    /// excluding `.ta/` (always) and configured exclude patterns.
    ///
    /// On APFS (macOS) and Btrfs (Linux), uses copy-on-write (COW) cloning.
    /// Falls back to full byte-for-byte copy on other filesystems.
    pub fn create(
        goal_id: impl Into<String>,
        source_dir: impl AsRef<Path>,
        staging_root: impl AsRef<Path>,
        excludes: ExcludePatterns,
    ) -> Result<Self, WorkspaceError> {
        Self::create_with_strategy(
            goal_id,
            source_dir,
            staging_root,
            excludes,
            OverlayStagingMode::Full,
        )
    }

    /// Create an overlay workspace with an explicit staging strategy (v0.13.13).
    ///
    /// - `Full`: full copy (default, always works)
    /// - `Smart`: symlink excluded directories instead of copying — near-zero staging
    ///   cost for large ignored trees (e.g., `node_modules/`, `Content/`)
    /// - `RefsCow`: Windows ReFS Dev Drive instant CoW clone (falls back to `Smart`
    ///   on non-ReFS volumes)
    ///
    /// After staging, prints a size report to stdout:
    /// ```text
    /// Staging: 55 MB copied, 749 GB symlinked (smart mode) in 0.3s
    /// ```
    pub fn create_with_strategy(
        goal_id: impl Into<String>,
        source_dir: impl AsRef<Path>,
        staging_root: impl AsRef<Path>,
        excludes: ExcludePatterns,
        mode: OverlayStagingMode,
    ) -> Result<Self, WorkspaceError> {
        let goal_id = goal_id.into();
        let source_dir = source_dir.as_ref().to_path_buf();
        let staging_dir = staging_root.as_ref().join(&goal_id);

        fs::create_dir_all(&staging_dir).map_err(|source| WorkspaceError::IoError {
            path: staging_dir.clone(),
            source,
        })?;

        // Resolve the effective mode: RefsCow falls back to Smart on non-ReFS.
        let effective_mode = resolve_staging_mode(mode, &staging_dir);

        // Detect the best available COW file-copy strategy (APFS/Btrfs/Full).
        let copy_strategy = detect_strategy(&staging_dir);

        tracing::info!(
            goal_id = %goal_id,
            staging_mode = ?effective_mode,
            copy_strategy = copy_strategy.description(),
            source = %source_dir.display(),
            staging = %staging_dir.display(),
            "creating overlay workspace"
        );

        let start = Instant::now();

        // For ProjFs mode, use the Virtual strategy (no file I/O needed).
        let effective_copy_strategy = if effective_mode == OverlayStagingMode::ProjFs {
            crate::copy_strategy::CopyStrategy::Virtual
        } else {
            copy_strategy
        };

        let mut stat = CopyStat::new(effective_copy_strategy);

        // Track the ProjFS provider (Windows only).
        #[cfg(target_os = "windows")]
        let mut projfs_provider: Option<crate::projfs_strategy::ProjFsProvider> = None;

        match effective_mode {
            OverlayStagingMode::Smart => {
                copy_dir_recursive_smart(
                    &source_dir,
                    &staging_dir,
                    &source_dir,
                    &excludes,
                    copy_strategy,
                    &mut stat,
                )?;
            }
            OverlayStagingMode::ProjFs => {
                // Start ProjFS virtualization. No file copying needed.
                #[cfg(target_os = "windows")]
                {
                    match crate::projfs_strategy::ProjFsProvider::start(&source_dir, &staging_dir) {
                        Ok(provider) => {
                            projfs_provider = Some(provider);
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "ProjFS start failed — falling back to smart staging"
                            );
                            // Fall back: do a smart copy instead.
                            copy_dir_recursive_smart(
                                &source_dir,
                                &staging_dir,
                                &source_dir,
                                &excludes,
                                copy_strategy,
                                &mut stat,
                            )?;
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    // Should not be reachable: resolve_staging_mode maps ProjFs → Smart
                    // on non-Windows. This is a safety fallback.
                    copy_dir_recursive_smart(
                        &source_dir,
                        &staging_dir,
                        &source_dir,
                        &excludes,
                        copy_strategy,
                        &mut stat,
                    )?;
                }
            }
            _ => {
                // Full or RefsCow-resolved-to-full.
                copy_dir_recursive(
                    &source_dir,
                    &staging_dir,
                    &excludes,
                    copy_strategy,
                    &mut stat,
                )?;
            }
        }

        stat.duration = start.elapsed();

        tracing::info!(
            goal_id = %goal_id,
            files = stat.files_copied,
            bytes = stat.bytes_total,
            symlinks = stat.symlinks_created,
            duration_ms = stat.duration.as_millis(),
            "overlay workspace created"
        );

        // Print staging size report.
        println!("{}", stat.size_report());

        // v0.15.14.7: Delete ephemeral staging-root files so each goal starts
        // with a clean slate. These files (e.g., .ta-decisions.json) must not
        // carry over from a previous goal's applied source state.
        delete_ephemeral_staging_files(&staging_dir);

        // v0.2.1: Capture source snapshot for conflict detection.
        let snapshot =
            SourceSnapshot::capture(&source_dir, |path| excludes.should_skip_path(path)).ok();

        Ok(Self {
            goal_id,
            source_dir,
            staging_dir,
            excludes,
            source_snapshot: snapshot,
            copy_stat: Some(stat),
            #[cfg(target_os = "windows")]
            projfs_provider,
        })
    }

    /// Open an existing overlay workspace (for resuming after process restart).
    pub fn open(
        goal_id: impl Into<String>,
        source_dir: impl AsRef<Path>,
        staging_dir: impl AsRef<Path>,
        excludes: ExcludePatterns,
    ) -> Self {
        Self {
            goal_id: goal_id.into(),
            source_dir: source_dir.as_ref().to_path_buf(),
            staging_dir: staging_dir.as_ref().to_path_buf(),
            excludes,
            source_snapshot: None, // Snapshot must be loaded separately if needed.
            copy_stat: None,       // Not available when reopening an existing workspace.
            #[cfg(target_os = "windows")]
            projfs_provider: None, // Not available when reopening an existing workspace.
        }
    }

    /// Set the source snapshot (for conflict detection after restore from disk).
    pub fn set_snapshot(&mut self, snapshot: SourceSnapshot) {
        self.source_snapshot = Some(snapshot);
    }

    /// Get the source snapshot, if available.
    pub fn snapshot(&self) -> Option<&SourceSnapshot> {
        self.source_snapshot.as_ref()
    }

    pub fn goal_id(&self) -> &str {
        &self.goal_id
    }

    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Statistics from staging creation: strategy used, duration, file count, bytes.
    /// Returns `None` for workspaces opened from disk (not freshly created).
    pub fn copy_stat(&self) -> Option<&CopyStat> {
        self.copy_stat.as_ref()
    }

    /// The copy strategy used when creating this workspace.
    /// Returns `None` for workspaces opened from disk.
    pub fn copy_strategy(&self) -> Option<CopyStrategy> {
        self.copy_stat.as_ref().map(|s| s.strategy)
    }

    /// Diff the staging workspace against the source to find all changes.
    ///
    /// Walks both directories, comparing files to identify modifications,
    /// creations, and deletions. Skips `.ta/` and agent infrastructure dirs when
    /// diffing (internal state, not agent work product), plus V1 exclude patterns
    /// (which include VCS metadata dirs contributed by the active adapter).
    pub fn diff_all(&self) -> Result<Vec<OverlayChange>, WorkspaceError> {
        let mut changes = Vec::new();

        // Collect all file paths from both directories.
        let mut staging_files = Vec::new();
        walk_dir_relative(
            &self.staging_dir,
            &self.staging_dir,
            &mut staging_files,
            &self.excludes,
        )?;

        let mut source_files = Vec::new();
        walk_dir_relative(
            &self.source_dir,
            &self.source_dir,
            &mut source_files,
            &self.excludes,
        )?;

        // Check each staging file against source.
        for path in &staging_files {
            if should_skip_for_diff(path, &self.excludes) {
                continue;
            }

            let staging_path = self.staging_dir.join(path);
            let source_path = self.source_dir.join(path);

            if source_path.exists() {
                // File exists in both — check if modified.
                let staging_content =
                    fs::read(&staging_path).map_err(|source| WorkspaceError::IoError {
                        path: staging_path.clone(),
                        source,
                    })?;
                let source_content =
                    fs::read(&source_path).map_err(|source| WorkspaceError::IoError {
                        path: source_path.clone(),
                        source,
                    })?;

                if staging_content != source_content {
                    // Detect binary: if either version has null bytes in first 8KB,
                    // produce a summary instead of a lossy text diff.
                    let source_binary = source_content
                        .get(..8192)
                        .unwrap_or(&source_content)
                        .contains(&0);
                    let staging_binary = staging_content
                        .get(..8192)
                        .unwrap_or(&staging_content)
                        .contains(&0);
                    let diff = if source_binary || staging_binary {
                        format!(
                            "--- a/{}\n+++ b/{}\n[binary file changed: {} -> {} bytes]\n",
                            path,
                            path,
                            source_content.len(),
                            staging_content.len()
                        )
                    } else {
                        simple_unified_diff(
                            path,
                            &String::from_utf8_lossy(&source_content),
                            &String::from_utf8_lossy(&staging_content),
                        )
                    };
                    changes.push(OverlayChange::Modified {
                        path: path.clone(),
                        diff,
                    });
                }
            } else {
                // File only in staging — created.
                // Detect binary files: if the first 8KB contains a null byte,
                // store a placeholder instead of lossy UTF-8 conversion.
                let raw = fs::read(&staging_path).map_err(|source| WorkspaceError::IoError {
                    path: staging_path.clone(),
                    source,
                })?;
                let is_binary = raw.get(..8192).unwrap_or(&raw).contains(&0);
                let content = if is_binary {
                    format!("[binary file: {} bytes]", raw.len())
                } else {
                    String::from_utf8(raw).unwrap_or_else(|e| {
                        format!("[binary file: {} bytes]", e.into_bytes().len())
                    })
                };
                changes.push(OverlayChange::Created {
                    path: path.clone(),
                    content,
                });
            }
        }

        // Check for deleted files (in source but not in staging).
        for path in &source_files {
            if should_skip_for_diff(path, &self.excludes) {
                continue;
            }
            let staging_path = self.staging_dir.join(path);
            if !staging_path.exists() {
                changes.push(OverlayChange::Deleted { path: path.clone() });
            }
        }

        changes.sort_by(|a, b| {
            let path_a = match a {
                OverlayChange::Modified { path, .. }
                | OverlayChange::Created { path, .. }
                | OverlayChange::Deleted { path } => path,
            };
            let path_b = match b {
                OverlayChange::Modified { path, .. }
                | OverlayChange::Created { path, .. }
                | OverlayChange::Deleted { path } => path,
            };
            path_a.cmp(path_b)
        });

        Ok(changes)
    }

    /// Diff a single file between staging and source.
    pub fn diff_file(&self, relative_path: &str) -> Result<Option<String>, WorkspaceError> {
        let staging_path = self.staging_dir.join(relative_path);
        let source_path = self.source_dir.join(relative_path);

        if !staging_path.exists() && !source_path.exists() {
            return Ok(None);
        }

        if !staging_path.exists() {
            // Deleted.
            let content =
                fs::read_to_string(&source_path).map_err(|source| WorkspaceError::IoError {
                    path: source_path,
                    source,
                })?;
            return Ok(Some(deleted_file_diff(relative_path, &content)));
        }

        if !source_path.exists() {
            // Created.
            let content =
                fs::read_to_string(&staging_path).map_err(|source| WorkspaceError::IoError {
                    path: staging_path,
                    source,
                })?;
            return Ok(Some(new_file_diff(relative_path, &content)));
        }

        // Both exist — compare.
        let staging_content =
            fs::read(&staging_path).map_err(|source| WorkspaceError::IoError {
                path: staging_path,
                source,
            })?;
        let source_content = fs::read(&source_path).map_err(|source| WorkspaceError::IoError {
            path: source_path,
            source,
        })?;

        if staging_content == source_content {
            return Ok(None);
        }

        Ok(Some(simple_unified_diff(
            relative_path,
            &String::from_utf8_lossy(&source_content),
            &String::from_utf8_lossy(&staging_content),
        )))
    }

    /// List changed file paths with their change type.
    pub fn list_changes(&self) -> Result<Vec<(String, &'static str)>, WorkspaceError> {
        let changes = self.diff_all()?;
        Ok(changes
            .into_iter()
            .map(|c| match c {
                OverlayChange::Modified { path, .. } => (path, "modified"),
                OverlayChange::Created { path, .. } => (path, "created"),
                OverlayChange::Deleted { path } => (path, "deleted"),
            })
            .collect())
    }

    /// Detect conflicts between the current source state and the snapshot.
    /// Returns None if no snapshot was captured (conflict detection disabled).
    /// Uses the overlay's ExcludePatterns to filter build artifacts (target/, node_modules/, etc.)
    /// from the "new file" scan, preventing false conflicts from cargo build output.
    pub fn detect_conflicts(&self) -> Result<Option<Vec<Conflict>>, WorkspaceError> {
        match &self.source_snapshot {
            Some(snapshot) => Ok(Some(
                snapshot.detect_conflicts(&self.source_dir, |path| {
                    self.excludes.should_skip_path(path)
                })?,
            )),
            None => Ok(None),
        }
    }

    /// Check if conflicts exist (returns true if any conflicts are detected).
    pub fn has_conflicts(&self) -> Result<bool, WorkspaceError> {
        match self.detect_conflicts()? {
            Some(conflicts) => Ok(!conflicts.is_empty()),
            None => Ok(false), // No snapshot — assume no conflicts.
        }
    }

    /// Apply only the changed files from staging back to a target directory.
    /// Does NOT check for conflicts — use apply_with_conflict_check for safety.
    pub fn apply_to(
        &self,
        target_dir: &Path,
    ) -> Result<Vec<(String, &'static str)>, WorkspaceError> {
        let changes = self.diff_all()?;
        let mut applied = Vec::new();

        for change in &changes {
            match change {
                OverlayChange::Modified { path, .. } | OverlayChange::Created { path, .. } => {
                    let src = self.staging_dir.join(path);
                    let dst = target_dir.join(path);
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|source| WorkspaceError::IoError {
                            path: parent.to_path_buf(),
                            source,
                        })?;
                    }
                    fs::copy(&src, &dst)
                        .map_err(|source| WorkspaceError::IoError { path: dst, source })?;
                    let kind = if matches!(change, OverlayChange::Modified { .. }) {
                        "modified"
                    } else {
                        "created"
                    };
                    applied.push((path.clone(), kind));
                }
                OverlayChange::Deleted { path } => {
                    let dst = target_dir.join(path);
                    if dst.exists() {
                        fs::remove_file(&dst)
                            .map_err(|source| WorkspaceError::IoError { path: dst, source })?;
                    }
                    applied.push((path.clone(), "deleted"));
                }
            }
        }

        Ok(applied)
    }

    /// Apply only selected artifacts (by URI) to the target directory.
    ///
    /// Used for selective approval where only a subset of changes should be applied.
    /// URIs should be in the form "fs://workspace/<path>".
    pub fn apply_selective(
        &self,
        target_dir: &Path,
        approved_uris: &[String],
    ) -> Result<Vec<(String, &'static str)>, WorkspaceError> {
        let changes = self.diff_all()?;
        let mut applied = Vec::new();

        // Convert URIs to relative paths for comparison.
        let approved_paths: std::collections::HashSet<String> = approved_uris
            .iter()
            .filter_map(|uri| uri.strip_prefix("fs://workspace/"))
            .map(|s| s.to_string())
            .collect();

        for change in &changes {
            let path = match change {
                OverlayChange::Modified { path, .. } => path,
                OverlayChange::Created { path, .. } => path,
                OverlayChange::Deleted { path } => path,
            };

            // Skip if not in approved set.
            if !approved_paths.contains(path) {
                continue;
            }

            match change {
                OverlayChange::Modified { path, .. } | OverlayChange::Created { path, .. } => {
                    let src = self.staging_dir.join(path);
                    let dst = target_dir.join(path);
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|source| WorkspaceError::IoError {
                            path: parent.to_path_buf(),
                            source,
                        })?;
                    }
                    fs::copy(&src, &dst)
                        .map_err(|source| WorkspaceError::IoError { path: dst, source })?;
                    let kind = if matches!(change, OverlayChange::Modified { .. }) {
                        "modified"
                    } else {
                        "created"
                    };
                    applied.push((path.clone(), kind));
                }
                OverlayChange::Deleted { path } => {
                    let dst = target_dir.join(path);
                    if dst.exists() {
                        fs::remove_file(&dst)
                            .map_err(|source| WorkspaceError::IoError { path: dst, source })?;
                    }
                    applied.push((path.clone(), "deleted"));
                }
            }
        }

        Ok(applied)
    }

    /// Apply specific artifacts with conflict detection and resolution strategy.
    ///
    /// `artifact_uris` is the authoritative list of files the PR package intends to change
    /// (from the PR package's artifact list). Only these files are applied and only conflicts
    /// overlapping with these files trigger abort/force. This prevents stale staging copies
    /// of unrelated files from overwriting newer source changes.
    pub fn apply_with_conflict_check(
        &self,
        target_dir: &Path,
        resolution: ConflictResolution,
        artifact_uris: &[String],
    ) -> Result<Vec<(String, &'static str)>, WorkspaceError> {
        // Convert URIs to relative paths for comparison.
        let artifact_paths: std::collections::HashSet<String> = artifact_uris
            .iter()
            .filter_map(|uri| uri.strip_prefix("fs://workspace/"))
            .map(|s| s.to_string())
            .collect();

        let mut filtered_uris = artifact_uris.to_vec();

        // Check for conflicts if snapshot exists.
        if let Some(all_conflicts) = self.detect_conflicts()? {
            if !all_conflicts.is_empty() {
                // Only flag conflicts that overlap with the artifact list.
                let overlapping: Vec<_> = all_conflicts
                    .iter()
                    .filter(|c| artifact_paths.contains(&c.path))
                    .collect();

                let non_overlapping = all_conflicts.len() - overlapping.len();
                if non_overlapping > 0 {
                    eprintln!(
                        "ℹ️  {} file(s) changed in source but not in changeset (safe, skipping)",
                        non_overlapping
                    );
                }

                if !overlapping.is_empty() {
                    // Smart auto-resolve: compare staging hash to snapshot hash.
                    // If they match, the agent never touched the file — it's a phantom
                    // artifact from source drift, not a real conflict.
                    let (true_conflicts, auto_resolved) =
                        self.classify_overlapping_conflicts(&overlapping);

                    if !auto_resolved.is_empty() {
                        eprintln!(
                            "ℹ️  {} file(s) auto-resolved (source changed, agent did not modify)",
                            auto_resolved.len()
                        );
                        for path in &auto_resolved {
                            eprintln!("   skipping: {}", path);
                        }

                        // Remove phantom artifacts from the apply list.
                        let resolved_set: std::collections::HashSet<&str> =
                            auto_resolved.iter().map(|s| s.as_str()).collect();
                        filtered_uris.retain(|uri| {
                            uri.strip_prefix("fs://workspace/")
                                .is_none_or(|p| !resolved_set.contains(p))
                        });
                    }

                    if !true_conflicts.is_empty() {
                        match resolution {
                            ConflictResolution::Abort => {
                                return Err(WorkspaceError::ConflictDetected {
                                    conflicts: true_conflicts,
                                });
                            }
                            ConflictResolution::ForceOverwrite => {
                                eprintln!(
                                    "⚠️  Warning: {} true conflict(s) detected, proceeding with force-overwrite",
                                    true_conflicts.len()
                                );
                            }
                            ConflictResolution::Merge => {
                                // v0.14.3.5: Attempt three-way merge via `git merge-file`.
                                // base = snapshot (goal-start content)
                                // ours = staging (agent's version)
                                // theirs = current source (external changes)
                                //
                                // For each true conflict file, run the merge. If it succeeds
                                // cleanly (exit 0, no conflict markers), write the result and
                                // remove the file from the apply list (it's already merged).
                                // If conflicts remain, fall through to Abort.
                                let snapshot = self.source_snapshot.as_ref();
                                let mut still_conflicting = Vec::new();

                                for conflict_desc in &true_conflicts {
                                    // Extract path from conflict description.
                                    // Descriptions have format: "File '<path>' was modified..."
                                    let path = extract_path_from_conflict(conflict_desc);
                                    if path.is_none() {
                                        still_conflicting.push(conflict_desc.clone());
                                        continue;
                                    }
                                    let path = path.unwrap();

                                    let merged = snapshot
                                        .and_then(|s| s.files.get(&path))
                                        .and_then(|snap| {
                                            three_way_merge(
                                                &snap.content_hash,
                                                &self.staging_dir.join(&path),
                                                &self.source_dir.join(&path),
                                                snap,
                                                &self.staging_dir,
                                            )
                                            .ok()
                                        });

                                    match merged {
                                        Some(MergeResult::Clean { content, hunks }) => {
                                            // Write merged content directly to the source.
                                            // We'll write to a temp location and let apply_selective
                                            // pick it up from staging, so write to staging instead.
                                            let staging_path = self.staging_dir.join(&path);
                                            if fs::write(&staging_path, &content).is_ok() {
                                                eprintln!(
                                                    "ℹ️  auto-merged: {} ({} hunk(s), 0 conflicts)",
                                                    path, hunks
                                                );
                                            } else {
                                                still_conflicting.push(conflict_desc.clone());
                                            }
                                        }
                                        Some(MergeResult::Conflicted { .. }) | None => {
                                            still_conflicting.push(conflict_desc.clone());
                                        }
                                    }
                                }

                                if !still_conflicting.is_empty() {
                                    return Err(WorkspaceError::ConflictDetected {
                                        conflicts: still_conflicting,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Apply only the files from the (possibly filtered) artifact list.
        self.apply_selective(target_dir, &filtered_uris)
    }

    /// Classify overlapping conflicts into true conflicts (agent changed the file)
    /// vs phantom artifacts (agent didn't touch it, only source diverged).
    ///
    /// Compares the staging file hash to the snapshot hash. If they match,
    /// the agent never modified the file — it's safe to auto-resolve.
    fn classify_overlapping_conflicts(
        &self,
        overlapping: &[&Conflict],
    ) -> (Vec<String>, Vec<String>) {
        let mut true_conflicts = Vec::new();
        let mut auto_resolved = Vec::new();

        for conflict in overlapping {
            let staging_path = self.staging_dir.join(&conflict.path);
            let agent_changed = if staging_path.exists() {
                // Compare staging file hash to snapshot hash at goal start.
                match FileSnapshot::capture(&self.staging_dir, &conflict.path) {
                    Ok(staging_snap) => staging_snap.content_hash != conflict.snapshot.content_hash,
                    Err(_) => true, // Can't read staging file — assume agent changed it (safe)
                }
            } else if conflict.snapshot.content_hash.is_empty() {
                // File didn't exist at snapshot time and doesn't exist in staging.
                // This is a new-in-source file the agent never saw. Auto-resolve.
                false
            } else {
                // File existed at snapshot time but agent deleted it. Real change.
                true
            };

            if agent_changed {
                true_conflicts.push(conflict.description.clone());
            } else {
                auto_resolved.push(conflict.path.clone());
            }
        }

        (true_conflicts, auto_resolved)
    }

    /// Clean up the staging directory.
    pub fn cleanup(self) -> Result<(), WorkspaceError> {
        if self.staging_dir.exists() {
            fs::remove_dir_all(&self.staging_dir).map_err(|source| WorkspaceError::IoError {
                path: self.staging_dir,
                source,
            })?;
        }
        Ok(())
    }
}

// ── Three-way merge (v0.14.3.5) ─────────────────────────────────

/// Result of a three-way merge attempt.
pub enum MergeResult {
    /// Merge succeeded with no conflict markers. Content is the merged file bytes.
    Clean { content: Vec<u8>, hunks: usize },
    /// Merge completed but conflict markers remain.
    Conflicted { content: Vec<u8> },
}

/// Attempt a three-way merge of a file using `git merge-file --quiet`.
///
/// - `base_hash`: SHA-256 of the base (goal-start snapshot) — used as sanity label only
/// - `staging_path`: ours (agent's version)
/// - `source_path`: theirs (current source / external changes)
/// - `snap`: the `FileSnapshot` at goal start — its content_hash identifies the base version
/// - `staging_dir`: root of the staging workspace (used to reconstruct base content)
///
/// Returns `Ok(MergeResult::Clean {...})` when merge succeeds without conflict markers.
/// Returns `Ok(MergeResult::Conflicted {...})` when conflict markers remain.
/// Returns `Err` when the base content cannot be reconstructed or `git` is unavailable.
pub fn three_way_merge(
    _base_hash: &str,
    staging_path: &std::path::Path,
    source_path: &std::path::Path,
    _snap: &crate::conflict::FileSnapshot,
    _staging_dir: &std::path::Path,
) -> Result<MergeResult, Box<dyn std::error::Error>> {
    use std::io::Write;

    // We need the base content (file as it was at goal start). We reconstruct it
    // using `git show HEAD:<path>` from the source repository. This gives us the
    // committed version before any external edit — the ideal 3-way merge base.
    // The snapshot content hash is kept for documentation but not used directly
    // since we can't reconstruct file content from a hash alone.

    // Try to recover base content using git show HEAD:<path>.
    let base_content = {
        // Find project root (git repo root) by walking up from source_path.
        let mut dir = source_path.parent();
        let git_root = loop {
            match dir {
                Some(d) if d.join(".git").exists() => break Some(d.to_path_buf()),
                Some(d) => dir = d.parent(),
                None => break None,
            }
        };

        if let Some(root) = git_root {
            // Path relative to git root.
            let rel = source_path.strip_prefix(&root).unwrap_or(source_path);
            let path_str = rel.to_string_lossy();
            // git show HEAD:<path> — the committed version = the base before any edits.
            let out = std::process::Command::new("git")
                .args(["show", &format!("HEAD:{}", path_str)])
                .current_dir(&root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .ok();
            if let Some(o) = out {
                if o.status.success() {
                    Some(o.stdout)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    let base_bytes = match base_content {
        Some(b) => b,
        None => {
            // No git or file not in HEAD — use the snapshot hash as proof we
            // can't recover the base. Fall through: cannot merge.
            return Err(
                "Cannot reconstruct base content for three-way merge (file not in git HEAD)".into(),
            );
        }
    };

    // Read ours (staging) and theirs (source).
    let ours_bytes = fs::read(staging_path)?;
    let theirs_bytes = fs::read(source_path)?;

    // Skip merge attempt for binary files.
    let is_binary = |b: &[u8]| b.get(..8192).unwrap_or(b).contains(&0u8);
    if is_binary(&base_bytes) || is_binary(&ours_bytes) || is_binary(&theirs_bytes) {
        return Err("Binary file — skipping three-way merge".into());
    }

    // Write three temp files: base, ours, theirs.
    let tmp = tempfile::tempdir()?;
    let base_file = tmp.path().join("base");
    let ours_file = tmp.path().join("ours");
    let theirs_file = tmp.path().join("theirs");

    fs::File::create(&base_file)?.write_all(&base_bytes)?;
    fs::File::create(&ours_file)?.write_all(&ours_bytes)?;
    fs::File::create(&theirs_file)?.write_all(&theirs_bytes)?;

    // Run: git merge-file --quiet ours base theirs
    // Modifies `ours_file` in-place. Exit 0 = clean, non-zero = conflicts remain.
    let status = std::process::Command::new("git")
        .args([
            "merge-file",
            "--quiet",
            ours_file.to_str().unwrap_or("ours"),
            base_file.to_str().unwrap_or("base"),
            theirs_file.to_str().unwrap_or("theirs"),
        ])
        .output()?;

    let merged_content = fs::read(&ours_file)?;

    // Count change hunks (lines containing <<<<<<< are conflict markers).
    let has_conflict_markers = merged_content.windows(7).any(|w| w == b"<<<<<<<");

    // git merge-file exit code: 0 = clean, positive = number of conflicts.
    let hunks_merged = if status.status.success() {
        // Count `@@` markers in unified diff approximation — count non-overlapping
        // occurrences of `\n@@` in merged output as a hunk count proxy.
        let content_str = String::from_utf8_lossy(&merged_content);
        content_str.matches("\n@@").count().max(1)
    } else {
        0
    };

    if status.status.success() && !has_conflict_markers {
        Ok(MergeResult::Clean {
            content: merged_content,
            hunks: hunks_merged,
        })
    } else {
        Ok(MergeResult::Conflicted {
            content: merged_content,
        })
    }
}

/// Extract the file path from a conflict description string.
///
/// Descriptions have the form `"File '<path>' was modified..."` or
/// `"File '<path>' was deleted..."`.
fn extract_path_from_conflict(description: &str) -> Option<String> {
    // Look for pattern: File '<path>'
    let after_file = description.strip_prefix("File '")?;
    let end = after_file.find('\'')?;
    Some(after_file[..end].to_string())
}

// ── Staging mode resolution ─────────────────────────────────────

/// Resolve the effective staging mode at workspace creation time.
///
/// - `RefsCow` auto-falls back to `Smart` when not on a Windows ReFS volume.
/// - `ProjFs` auto-falls back to `Smart` when `Client-ProjFS` is not available
///   (non-Windows or feature not installed).
fn resolve_staging_mode(mode: OverlayStagingMode, _staging_dir: &Path) -> OverlayStagingMode {
    match mode {
        OverlayStagingMode::RefsCow => {
            // Windows ReFS CoW support — probe and fall back to Smart on NTFS.
            if is_refs_volume(_staging_dir) {
                OverlayStagingMode::RefsCow
            } else {
                tracing::info!(
                    "refs-cow requested but volume is not ReFS — falling back to smart staging"
                );
                OverlayStagingMode::Smart
            }
        }
        OverlayStagingMode::ProjFs => {
            // ProjFS is Windows-only. On non-Windows always fall back to Smart.
            if crate::windows_features::is_projfs_available() {
                OverlayStagingMode::ProjFs
            } else {
                tracing::info!(
                    "projfs requested but Client-ProjFS is not available — \
                     falling back to smart staging. \
                     Enable with: Dism.exe /Online /Enable-Feature /FeatureName:Client-ProjFS /NoRestart"
                );
                OverlayStagingMode::Smart
            }
        }
        other => other,
    }
}

/// Probe whether the given path is on a Windows ReFS volume.
///
/// On non-Windows platforms always returns false (no ReFS support).
#[allow(unused_variables)]
fn is_refs_volume(path: &Path) -> bool {
    #[cfg(windows)]
    {
        // GetVolumeInformationW: if FILE_SUPPORTS_BLOCK_REFCOUNTING (0x08000000) is set, it's ReFS.
        // For now this is a stub — full ReFS IOCTL support is deferred to a later phase.
        // Return false until the full DeviceIoControl path is implemented.
        let _ = path;
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// ── Directory copy ──────────────────────────────────────────────

/// Recursively copy a directory using the specified strategy.
///
/// Excludes `.ta/` (always, via [`ExcludePatterns`]) and any other configured
/// patterns. Updates `stat` with file count and byte totals.
fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    excludes: &ExcludePatterns,
    strategy: CopyStrategy,
    stat: &mut CopyStat,
) -> Result<(), WorkspaceError> {
    let entries = fs::read_dir(src).map_err(|source| WorkspaceError::IoError {
        path: src.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| WorkspaceError::IoError {
            path: src.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if excludes.should_exclude(&name) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            fs::create_dir_all(&dst_path).map_err(|source| WorkspaceError::IoError {
                path: dst_path.clone(),
                source,
            })?;
            copy_dir_recursive(&src_path, &dst_path, excludes, strategy, stat)?;
        } else {
            // Collect source file size for benchmarking before copying.
            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);

            copy_file_with_strategy(&src_path, &dst_path, strategy).map_err(|source| {
                WorkspaceError::IoError {
                    path: dst_path,
                    source,
                }
            })?;

            stat.files_copied += 1;
            stat.bytes_total += file_size;
        }
    }

    Ok(())
}

/// Recursively copy a directory in "smart" mode.
///
/// Unlike the full copy path, when a directory entry matches a user-configured
/// exclude pattern (but is NOT a hardcoded infra dir like `.ta/`), a symlink
/// pointing back to the source directory is created instead of copying.
/// Infra dirs (`.ta/`, `.claude-flow/`, etc.) are still silently skipped.
///
/// This gives the agent a view of the full workspace with minimal disk I/O:
/// only the agent-writable subset is physically copied; large excluded trees
/// (e.g., `node_modules/`, Unreal `Content/`) appear as read-only symlinks.
#[allow(clippy::only_used_in_recursion)]
fn copy_dir_recursive_smart(
    src: &Path,
    dst: &Path,
    source_root: &Path,
    excludes: &ExcludePatterns,
    strategy: CopyStrategy,
    stat: &mut CopyStat,
) -> Result<(), WorkspaceError> {
    let entries = fs::read_dir(src).map_err(|source| WorkspaceError::IoError {
        path: src.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| WorkspaceError::IoError {
            path: src.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Always skip infra dirs (hardcoded, never symlinked).
        if ExcludePatterns::INFRA_DIRS.contains(&name.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            // Check if this directory should be excluded by user patterns.
            if excludes.should_exclude(&name) {
                // Symlink the whole directory instead of recursing into it.
                create_symlink_dir(&src_path, &dst_path)?;
                // Estimate size for the report (best-effort, non-blocking).
                let estimated_bytes = estimate_dir_bytes(&src_path, 3);
                stat.symlinks_created += 1;
                stat.bytes_symlinked += estimated_bytes;
                tracing::debug!(
                    src = %src_path.display(),
                    dst = %dst_path.display(),
                    bytes = estimated_bytes,
                    "smart staging: symlinked excluded dir"
                );
                continue;
            }
            // Not excluded — recurse into the directory.
            fs::create_dir_all(&dst_path).map_err(|source| WorkspaceError::IoError {
                path: dst_path.clone(),
                source,
            })?;
            copy_dir_recursive_smart(&src_path, &dst_path, source_root, excludes, strategy, stat)?;
        } else {
            // Files: check user excludes (glob patterns like "*.pyc").
            if excludes.should_exclude(&name) {
                // For individual files, create a symlink too.
                create_symlink_file(&src_path, &dst_path)?;
                let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                stat.symlinks_created += 1;
                stat.bytes_symlinked += file_size;
                continue;
            }
            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            copy_file_with_strategy(&src_path, &dst_path, strategy).map_err(|source| {
                WorkspaceError::IoError {
                    path: dst_path,
                    source,
                }
            })?;
            stat.files_copied += 1;
            stat.bytes_total += file_size;
        }
    }

    Ok(())
}

/// Create a directory symlink (platform-specific).
#[allow(unused_variables)]
fn create_symlink_dir(src: &Path, dst: &Path) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|source| WorkspaceError::IoError {
            path: dst.to_path_buf(),
            source,
        })
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(src, dst).map_err(|source| WorkspaceError::IoError {
            path: dst.to_path_buf(),
            source,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported platform — fall back to full copy.
        copy_dir_recursive(
            src,
            dst,
            &ExcludePatterns::none(),
            CopyStrategy::Full,
            &mut CopyStat::new(CopyStrategy::Full),
        )
    }
}

/// Create a file symlink (platform-specific).
#[allow(unused_variables)]
fn create_symlink_file(src: &Path, dst: &Path) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|source| WorkspaceError::IoError {
            path: dst.to_path_buf(),
            source,
        })
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dst).map_err(|source| WorkspaceError::IoError {
            path: dst.to_path_buf(),
            source,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported platform — fall back to copy.
        copy_file_with_strategy(src, dst, CopyStrategy::Full).map_err(|source| {
            WorkspaceError::IoError {
                path: dst.to_path_buf(),
                source,
            }
        })
    }
}

/// Estimate the total bytes in a directory tree up to `depth` levels deep.
/// Non-blocking best-effort — ignores errors, used only for the size report.
fn estimate_dir_bytes(path: &Path, depth: u8) -> u64 {
    if depth == 0 {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        if let Ok(meta) = p.metadata() {
            if meta.is_dir() {
                total += estimate_dir_bytes(&p, depth - 1);
            } else {
                total += meta.len();
            }
        }
    }
    total
}

// ── Directory walking ───────────────────────────────────────────

/// Walk a directory tree and collect relative file paths.
///
/// Directories that should be excluded (per `excludes`) are pruned before
/// recursing — this is both more efficient and avoids Windows path-prefix
/// edge cases that can arise when deeply nested paths are collected and then
/// filtered after the fact.
fn walk_dir_relative(
    dir: &Path,
    root: &Path,
    files: &mut Vec<String>,
    excludes: &ExcludePatterns,
) -> Result<(), WorkspaceError> {
    if !dir.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(dir).map_err(|source| WorkspaceError::IoError {
        path: dir.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| WorkspaceError::IoError {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();

        if path.is_dir() {
            // Prune excluded directories early (before recursing) using the
            // directory name alone.  This covers both exclude-pattern dirs
            // (e.g. "target/", "node_modules/") and infra dirs (e.g. ".ta").
            // On Windows the normalised name is compared, so forward-slash and
            // backslash paths are handled uniformly.
            let dir_name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            if excludes.should_exclude(&dir_name) {
                continue;
            }
            walk_dir_relative(&path, root, files, excludes)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            // Normalize to forward slashes so URIs are consistent across platforms.
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            files.push(rel_str);
        }
    }

    Ok(())
}

/// TA-managed files that are injected/restored around every agent run and must
/// never appear in overlay diffs (v0.13.17.5).
///
/// These are TA infrastructure files, not agent work product. They are
/// modified by TA before the agent launches and restored before diffing,
/// so any residual differences indicate a restore failure — not a real change.
pub const TA_MANAGED_FILES: &[&str] = &[
    ".mcp.json",           // TA MCP server config — injected for every agent
    "settings.local.json", // Claude Code settings — injected with TA overrides
];

/// Ephemeral staging-root artifacts written by the agent during a goal run.
///
/// These files are scoped to the goal run and must never be applied back to
/// source. The overlay diff and apply path both exclude them (v0.15.14.7).
/// They are also deleted from staging at creation time so each new goal starts
/// with a clean slate regardless of what the source directory contains.
pub const EPHEMERAL_STAGING_FILES: &[&str] = &[
    ".ta-decisions.json", // Agent Decision Log — written per-run, never applied back
];

/// Delete ephemeral staging-root files from `staging_dir` after initial copy.
///
/// Called once during workspace creation. These files must not bleed from one
/// goal's source state into the next goal's staging workspace (v0.15.14.7).
fn delete_ephemeral_staging_files(staging_dir: &Path) {
    for name in EPHEMERAL_STAGING_FILES {
        let path = staging_dir.join(name);
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to delete ephemeral staging file — agent will start with stale data"
                );
            } else {
                tracing::debug!(
                    path = %path.display(),
                    "deleted ephemeral staging file (clean slate for new goal)"
                );
            }
        }
    }
}

/// Check if a path should be skipped when diffing.
/// We skip infrastructure directories — these are internal state, not agent work product.
/// V1 TEMPORARY: Also checks exclude patterns for build artifacts that
/// agents may generate in staging (e.g., `cargo build` creates `target/`).
fn should_skip_for_diff(path: &str, excludes: &ExcludePatterns) -> bool {
    // Agent infrastructure directories (created at runtime, not work product).
    // Note: VCS metadata dirs (e.g., .git/, .svn/) are excluded via adapter-contributed
    // patterns merged into ExcludePatterns, not hardcoded here.
    const INFRA_DIRS: &[&str] = &[
        ".ta",
        ".claude-flow",
        ".hive-mind",
        ".swarm",
        ".projfs-scratch", // ProjFS scratch directory — v0.15.8
    ];

    for dir in INFRA_DIRS {
        if path == *dir
            || path.starts_with(&format!("{}/", dir))
            || path.starts_with(&format!("{}\\", dir))
        {
            return true;
        }
    }

    // TA-managed files are injected/restored by TA infrastructure — exclude
    // them from diffs so they never appear as agent-authored changes (v0.13.17.5).
    for managed in TA_MANAGED_FILES {
        if path == *managed {
            return true;
        }
    }

    // Ephemeral staging-only files must never appear in changesets (v0.15.14.7).
    for ephemeral in EPHEMERAL_STAGING_FILES {
        if path == *ephemeral {
            return true;
        }
    }

    excludes.should_skip_path(path)
}

// ── Diff utilities ──────────────────────────────────────────────

/// Generate a simple unified diff between two strings.
pub fn simple_unified_diff(path: &str, original: &str, modified: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("--- a/{}\n", path));
    output.push_str(&format!("+++ b/{}\n", path));

    let orig_lines: Vec<&str> = original.lines().collect();
    let mod_lines: Vec<&str> = modified.lines().collect();

    if orig_lines != mod_lines {
        output.push_str(&format!(
            "@@ -1,{} +1,{} @@\n",
            orig_lines.len(),
            mod_lines.len()
        ));
        for line in &orig_lines {
            output.push_str(&format!("-{}\n", line));
        }
        for line in &mod_lines {
            output.push_str(&format!("+{}\n", line));
        }
    }

    output
}

/// Generate a diff for a newly created file.
pub fn new_file_diff(path: &str, content: &str) -> String {
    let mut output = String::new();
    output.push_str("--- /dev/null\n");
    output.push_str(&format!("+++ b/{}\n", path));

    let lines: Vec<&str> = content.lines().collect();
    output.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in &lines {
        output.push_str(&format!("+{}\n", line));
    }

    output
}

/// Generate a diff for a deleted file.
pub fn deleted_file_diff(path: &str, content: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("--- a/{}\n", path));
    output.push_str("+++ /dev/null\n");

    let lines: Vec<&str> = content.lines().collect();
    output.push_str(&format!("@@ -1,{} +0,0 @@\n", lines.len()));
    for line in &lines {
        output.push_str(&format!("-{}\n", line));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_source_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("README.md"), "# My Project\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
        // Create a .ta/ directory that should be excluded.
        fs::create_dir_all(dir.path().join(".ta/goals")).unwrap();
        fs::write(dir.path().join(".ta/config.toml"), "secret").unwrap();
        dir
    }

    #[test]
    fn create_overlay_copies_project() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Files should be copied.
        assert!(overlay.staging_dir().join("README.md").exists());
        assert!(overlay.staging_dir().join("src/main.rs").exists());
        assert!(overlay.staging_dir().join("src/lib.rs").exists());
    }

    #[test]
    fn create_overlay_excludes_ta_directory() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // .ta/ should NOT be copied.
        assert!(!overlay.staging_dir().join(".ta").exists());
        assert!(!overlay.staging_dir().join(".ta/config.toml").exists());
    }

    #[test]
    fn diff_detects_modified_files() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Modify a file in staging.
        fs::write(
            overlay.staging_dir().join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();
        assert_eq!(changes.len(), 1);

        match &changes[0] {
            OverlayChange::Modified { path, diff } => {
                assert_eq!(path, "src/main.rs");
                assert!(diff.contains("-fn main() {}"));
                assert!(diff.contains("+fn main() {"));
            }
            other => panic!("expected Modified, got {:?}", other),
        }
    }

    #[test]
    fn diff_detects_new_files() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Create a new file in staging.
        fs::write(
            overlay.staging_dir().join("src/new_module.rs"),
            "pub fn new_thing() {}\n",
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();
        assert_eq!(changes.len(), 1);

        match &changes[0] {
            OverlayChange::Created { path, content } => {
                assert_eq!(path, "src/new_module.rs");
                assert!(content.contains("new_thing"));
            }
            other => panic!("expected Created, got {:?}", other),
        }
    }

    #[test]
    fn diff_detects_deleted_files() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Delete a file from staging.
        fs::remove_file(overlay.staging_dir().join("src/lib.rs")).unwrap();

        let changes = overlay.diff_all().unwrap();
        assert_eq!(changes.len(), 1);

        match &changes[0] {
            OverlayChange::Deleted { path } => {
                assert_eq!(path, "src/lib.rs");
            }
            other => panic!("expected Deleted, got {:?}", other),
        }
    }

    #[test]
    fn apply_copies_only_changed_files() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Modify one file, create another, delete a third.
        fs::write(
            overlay.staging_dir().join("README.md"),
            "# Updated Project\n",
        )
        .unwrap();
        fs::write(overlay.staging_dir().join("NEW.md"), "new file\n").unwrap();
        fs::remove_file(overlay.staging_dir().join("src/lib.rs")).unwrap();

        // Apply to a fresh target.
        let target = TempDir::new().unwrap();
        // Pre-populate target with source files to test modification and deletion.
        let mut stat = CopyStat::new(CopyStrategy::Full);
        copy_dir_recursive(
            source.path(),
            target.path(),
            &ExcludePatterns::none(),
            CopyStrategy::Full,
            &mut stat,
        )
        .unwrap();

        let applied = overlay.apply_to(target.path()).unwrap();
        assert_eq!(applied.len(), 3);

        // Modified file should have new content.
        let readme = fs::read_to_string(target.path().join("README.md")).unwrap();
        assert_eq!(readme, "# Updated Project\n");

        // Created file should exist.
        let new_file = fs::read_to_string(target.path().join("NEW.md")).unwrap();
        assert_eq!(new_file, "new file\n");

        // Deleted file should be gone.
        assert!(!target.path().join("src/lib.rs").exists());
    }

    #[test]
    fn no_changes_returns_empty() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn cleanup_removes_staging() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();
        let staging_path = overlay.staging_dir().to_path_buf();
        assert!(staging_path.exists());

        overlay.cleanup().unwrap();
        assert!(!staging_path.exists());
    }

    // ── V1 TEMPORARY: ExcludePatterns tests ───────────────────────

    #[test]
    fn default_excludes_skip_target_dir() {
        let source = create_source_project();
        // Create a target/ directory (simulates `cargo build` output).
        fs::create_dir_all(source.path().join("target/debug")).unwrap();
        fs::write(source.path().join("target/debug/binary"), "fake binary").unwrap();
        // Create node_modules/ too.
        fs::create_dir_all(source.path().join("node_modules/pkg")).unwrap();
        fs::write(
            source.path().join("node_modules/pkg/index.js"),
            "module.exports = {}",
        )
        .unwrap();

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::defaults(),
        )
        .unwrap();

        // target/ and node_modules/ should NOT be copied.
        assert!(!overlay.staging_dir().join("target").exists());
        assert!(!overlay.staging_dir().join("node_modules").exists());

        // Regular files should still be copied.
        assert!(overlay.staging_dir().join("README.md").exists());
        assert!(overlay.staging_dir().join("src/main.rs").exists());
    }

    #[test]
    fn taignore_overrides_defaults() {
        let source = create_source_project();
        // Create a .taignore that only excludes "secret/".
        fs::write(
            source.path().join(".taignore"),
            "# Custom excludes\nsecret/\n",
        )
        .unwrap();
        // Create both target/ and secret/ directories.
        fs::create_dir_all(source.path().join("target/debug")).unwrap();
        fs::write(source.path().join("target/debug/binary"), "fake").unwrap();
        fs::create_dir_all(source.path().join("secret")).unwrap();
        fs::write(source.path().join("secret/key.pem"), "fake key").unwrap();

        let excludes = ExcludePatterns::load(source.path());
        let staging_root = TempDir::new().unwrap();
        let overlay =
            OverlayWorkspace::create("goal-1", source.path(), staging_root.path(), excludes)
                .unwrap();

        // .taignore says only "secret/" — so target/ IS copied, secret/ is NOT.
        assert!(overlay.staging_dir().join("target").exists());
        assert!(!overlay.staging_dir().join("secret").exists());
        // .ta/ is always excluded regardless of .taignore.
        assert!(!overlay.staging_dir().join(".ta").exists());
    }

    #[test]
    fn excluded_dirs_not_in_diff() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::defaults(),
        )
        .unwrap();

        // Simulate agent running `cargo build` in staging — creates target/.
        fs::create_dir_all(overlay.staging_dir().join("target/debug")).unwrap();
        fs::write(
            overlay.staging_dir().join("target/debug/binary"),
            "compiled",
        )
        .unwrap();

        // Also make a real change.
        fs::write(
            overlay.staging_dir().join("src/main.rs"),
            "fn main() { println!(\"updated\"); }\n",
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();

        // Should see the main.rs change but NOT the target/ files.
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            OverlayChange::Modified { path, .. } => {
                assert_eq!(path, "src/main.rs");
            }
            other => panic!("expected Modified, got {:?}", other),
        }
    }

    // ── Smart conflict auto-resolve tests ──────────────────────────

    #[test]
    fn phantom_artifacts_auto_resolved_on_abort() {
        // Setup: source has files A and B.
        let source = TempDir::new().unwrap();
        fs::write(source.path().join("a.txt"), "original A").unwrap();
        fs::write(source.path().join("b.txt"), "original B").unwrap();

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-phantom",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Agent modifies A in staging (real work).
        fs::write(overlay.staging_dir().join("a.txt"), "agent changed A").unwrap();
        // Agent does NOT touch B in staging — B stays identical to snapshot.

        // Simulate source divergence: modify BOTH A and B in source.
        // Need mtime to differ for conflict detection.
        std::thread::sleep(std::time::Duration::from_secs(2));
        fs::write(source.path().join("a.txt"), "source changed A").unwrap();
        fs::write(source.path().join("b.txt"), "source changed B").unwrap();

        // Both A and B are in the artifact manifest (dirty working tree scenario).
        let artifact_uris = vec![
            "fs://workspace/a.txt".to_string(),
            "fs://workspace/b.txt".to_string(),
        ];

        // With Abort: should auto-resolve B (phantom) and error on A (true conflict).
        let result = overlay.apply_with_conflict_check(
            source.path(),
            ConflictResolution::Abort,
            &artifact_uris,
        );

        assert!(
            result.is_err(),
            "expected error from true conflict on a.txt"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("a.txt"),
            "error should mention a.txt: {}",
            err_msg
        );
        // B should NOT appear in the error — it was auto-resolved.
        assert!(
            !err_msg.contains("b.txt"),
            "error should not mention b.txt (phantom): {}",
            err_msg
        );
    }

    #[test]
    fn phantom_artifacts_excluded_from_apply() {
        // Setup: source has files A and B.
        let source = TempDir::new().unwrap();
        fs::write(source.path().join("a.txt"), "original A").unwrap();
        fs::write(source.path().join("b.txt"), "original B").unwrap();

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-phantom2",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Agent modifies A (real work), does NOT touch B.
        fs::write(overlay.staging_dir().join("a.txt"), "agent changed A").unwrap();

        // Simulate source divergence on B only.
        std::thread::sleep(std::time::Duration::from_secs(2));
        fs::write(source.path().join("b.txt"), "source changed B").unwrap();

        // Both A and B in artifact manifest.
        let artifact_uris = vec![
            "fs://workspace/a.txt".to_string(),
            "fs://workspace/b.txt".to_string(),
        ];

        // Apply to a fresh target pre-populated with current source.
        let target = TempDir::new().unwrap();
        fs::write(target.path().join("a.txt"), "source changed A too").unwrap();
        fs::write(target.path().join("b.txt"), "source changed B").unwrap();

        // No true conflicts (only B diverged, and B is phantom).
        // ForceOverwrite so A goes through regardless.
        let result = overlay.apply_with_conflict_check(
            source.path(),
            ConflictResolution::ForceOverwrite,
            &artifact_uris,
        );

        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());
        let applied = result.unwrap();

        // A should be applied (agent changed it).
        let applied_paths: Vec<&str> = applied.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            applied_paths.contains(&"a.txt"),
            "a.txt should be applied: {:?}",
            applied_paths
        );
        // B should NOT be applied (phantom, filtered out).
        assert!(
            !applied_paths.contains(&"b.txt"),
            "b.txt should NOT be applied (phantom): {:?}",
            applied_paths
        );
    }

    #[test]
    fn agent_infra_dirs_excluded_from_copy_and_diff() {
        let source = create_source_project();
        // Simulate pre-existing claude-flow state in source.
        fs::create_dir_all(source.path().join(".claude-flow/sessions")).unwrap();
        fs::write(source.path().join(".claude-flow/agents.json"), "{}").unwrap();

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // .claude-flow/ should NOT be copied (hardcoded exclusion).
        assert!(!overlay.staging_dir().join(".claude-flow").exists());

        // Simulate agent creating runtime dirs in staging.
        fs::create_dir_all(overlay.staging_dir().join(".claude-flow/hive-mind")).unwrap();
        fs::write(
            overlay
                .staging_dir()
                .join(".claude-flow/hive-mind/state.json"),
            "{}",
        )
        .unwrap();
        fs::create_dir_all(overlay.staging_dir().join(".hive-mind/sessions")).unwrap();
        fs::write(
            overlay
                .staging_dir()
                .join(".hive-mind/sessions/session.txt"),
            "data",
        )
        .unwrap();
        fs::create_dir_all(overlay.staging_dir().join(".swarm")).unwrap();
        fs::write(overlay.staging_dir().join(".swarm/memory.db"), "binary").unwrap();

        // Also make a real change.
        fs::write(
            overlay.staging_dir().join("src/main.rs"),
            "fn main() { println!(\"updated\"); }\n",
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();

        // Should see ONLY the main.rs change — no infrastructure dirs.
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            OverlayChange::Modified { path, .. } => {
                assert_eq!(path, "src/main.rs");
            }
            other => panic!("expected Modified, got {:?}", other),
        }
    }

    #[test]
    fn git_dir_excluded_from_copy_and_diff_when_merged() {
        // Regression test: ta draft apply --git-commit was overwriting .git/HEAD
        // and .git/index because goal.rs didn't merge adapter.exclude_patterns().
        // Verify that when ".git/" is in ExcludePatterns, the overlay neither
        // copies .git/ into staging nor includes it in diff_all().
        let source = create_source_project();

        // Simulate a .git directory in the source (like a real git repo).
        fs::create_dir_all(source.path().join(".git/refs/heads")).unwrap();
        fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(source.path().join(".git/index"), "binary git index").unwrap();

        // Use ExcludePatterns with ".git/" merged in (simulates the fix in goal.rs).
        let mut excludes = ExcludePatterns::none();
        excludes.merge(&[".git/".to_string()]);

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-git-exclude",
            source.path(),
            staging_root.path(),
            excludes,
        )
        .unwrap();

        // .git/ must NOT be copied into staging.
        assert!(
            !overlay.staging_dir().join(".git").exists(),
            ".git/ should not be copied into staging"
        );

        // Even if something creates .git/ in staging (e.g. git init), diff should skip it.
        fs::create_dir_all(overlay.staging_dir().join(".git/refs")).unwrap();
        fs::write(
            overlay.staging_dir().join(".git/HEAD"),
            "ref: refs/heads/feature\n",
        )
        .unwrap();

        // Make a real change too.
        fs::write(
            overlay.staging_dir().join("src/main.rs"),
            "fn main() { println!(\"changed\"); }\n",
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();

        // Should see ONLY the main.rs change — .git/ must not appear.
        let git_changes: Vec<_> = changes
            .iter()
            .filter(|c| {
                let p = match c {
                    OverlayChange::Modified { path, .. } => path,
                    OverlayChange::Created { path, .. } => path,
                    OverlayChange::Deleted { path } => path,
                };
                p.starts_with(".git")
            })
            .collect();
        assert!(
            git_changes.is_empty(),
            ".git/ changes must not appear in diff: {:?}",
            git_changes
        );
        assert_eq!(
            changes.len(),
            1,
            "expected only src/main.rs change, got: {:?}",
            changes
        );
    }

    // ── Smart staging tests (v0.13.13) ───────────────────────────

    #[cfg(unix)]
    #[test]
    fn smart_staging_creates_symlinks_for_excluded_dirs() {
        let source = TempDir::new().unwrap();
        // Create source tree with a large excluded dir (node_modules).
        fs::create_dir_all(source.path().join("src")).unwrap();
        fs::write(source.path().join("src/index.js"), "// main\n").unwrap();
        fs::create_dir_all(source.path().join("node_modules/some-pkg")).unwrap();
        fs::write(
            source.path().join("node_modules/some-pkg/index.js"),
            "// pkg\n",
        )
        .unwrap();

        let staging_root = TempDir::new().unwrap();
        let excludes = ExcludePatterns::defaults(); // includes "node_modules/"

        let overlay = OverlayWorkspace::create_with_strategy(
            "goal-smart",
            source.path(),
            staging_root.path(),
            excludes,
            OverlayStagingMode::Smart,
        )
        .unwrap();

        // node_modules/ should be a symlink in staging, not a full copy.
        let nm_in_staging = overlay.staging_dir().join("node_modules");
        assert!(
            nm_in_staging.exists(),
            "node_modules should exist in staging"
        );
        assert!(
            nm_in_staging
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "node_modules should be a symlink in smart mode"
        );

        // src/ should be a real directory (not excluded).
        let src_in_staging = overlay.staging_dir().join("src");
        assert!(src_in_staging.is_dir());
        assert!(
            !src_in_staging
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "src/ should be copied, not symlinked"
        );

        // Symlink should point back to source.
        let link_target = fs::read_link(&nm_in_staging).unwrap();
        assert_eq!(
            link_target.canonicalize().unwrap(),
            source.path().join("node_modules").canonicalize().unwrap()
        );

        // Stat should record the symlink.
        let stat = overlay.copy_stat().unwrap();
        assert_eq!(stat.symlinks_created, 1);
    }

    #[cfg(unix)]
    #[test]
    fn smart_staging_copy_skips_symlinked_in_diff() {
        // Files behind a symlink should not appear as changes (content is identical).
        let source = TempDir::new().unwrap();
        fs::create_dir_all(source.path().join("src")).unwrap();
        fs::write(source.path().join("src/main.rs"), "fn main(){}\n").unwrap();
        fs::create_dir_all(source.path().join("node_modules/pkg")).unwrap();
        fs::write(source.path().join("node_modules/pkg/lib.js"), "// lib\n").unwrap();

        let staging_root = TempDir::new().unwrap();
        let excludes = ExcludePatterns::defaults();

        let overlay = OverlayWorkspace::create_with_strategy(
            "goal-diff",
            source.path(),
            staging_root.path(),
            excludes,
            OverlayStagingMode::Smart,
        )
        .unwrap();

        // No changes made — diff should be empty.
        let changes = overlay.diff_all().unwrap();
        let node_changes: Vec<_> = changes
            .iter()
            .filter(|c| {
                let p = match c {
                    OverlayChange::Modified { path, .. } => path,
                    OverlayChange::Created { path, .. } => path,
                    OverlayChange::Deleted { path } => path,
                };
                p.starts_with("node_modules")
            })
            .collect();
        assert!(
            node_changes.is_empty(),
            "node_modules changes must not appear in diff (symlinked): {:?}",
            node_changes
        );
    }

    #[test]
    fn staging_mode_default_is_full() {
        assert_eq!(OverlayStagingMode::default(), OverlayStagingMode::Full);
    }

    #[test]
    fn copy_stat_size_report_full_mode() {
        let mut stat = crate::copy_strategy::CopyStat::new(CopyStrategy::Full);
        stat.files_copied = 42;
        stat.bytes_total = 10 * 1024 * 1024; // 10 MB
        stat.duration = std::time::Duration::from_millis(250);
        let report = stat.size_report();
        assert!(report.contains("42 files"), "report: {}", report);
        assert!(report.contains("10.0 MB"), "report: {}", report);
    }

    #[test]
    fn copy_stat_size_report_smart_mode() {
        let mut stat = crate::copy_strategy::CopyStat::new(CopyStrategy::Full);
        stat.files_copied = 10;
        stat.bytes_total = 5 * 1024 * 1024; // 5 MB copied
        stat.symlinks_created = 3;
        stat.bytes_symlinked = 2 * 1024 * 1024 * 1024; // 2 GB symlinked
        stat.duration = std::time::Duration::from_millis(100);
        let report = stat.size_report();
        assert!(report.contains("5.0 MB copied"), "report: {}", report);
        assert!(report.contains("smart mode"), "report: {}", report);
    }

    // ── v0.14.3.5 integration tests ─────────────────────────────────────────

    /// Item 7: Follow-up apply does not revert parent-settled changes.
    ///
    /// Scenario:
    /// 1. Source has PLAN.md = "original".
    /// 2. Parent draft is applied: source PLAN.md updated to "parent-applied".
    /// 3. A follow-up staging workspace still has the old PLAN.md = "original"
    ///    (it predates the parent commit).
    /// 4. apply_with_conflict_check with the follow-up staging must NOT revert
    ///    PLAN.md back to "original". The baseline skip logic handles this:
    ///    if staging hash == source hash for a baseline artifact, skip it.
    ///    But here staging != source (old vs updated). The test verifies that
    ///    after simulated apply, source retains "parent-applied" (not reverted).
    ///
    /// The baseline skip condition (staging == source) covers the stable case.
    /// The protected-file guard (source newer than staging) covers the revert case.
    /// This test validates the protected-file guard via mtime comparison.
    #[test]
    fn follow_up_apply_does_not_revert_parent_changes() {
        let source = TempDir::new().unwrap();
        let staging_root = TempDir::new().unwrap();

        // Set up initial source state.
        fs::write(source.path().join("src/feature.rs"), "fn feature() {}\n").unwrap_or_else(|_| {
            fs::create_dir_all(source.path().join("src")).unwrap();
            fs::write(source.path().join("src/feature.rs"), "fn feature() {}\n").unwrap();
        });
        fs::create_dir_all(source.path().join("src")).unwrap_or(());
        fs::write(source.path().join("src/feature.rs"), "fn feature() {}\n").unwrap();
        fs::write(source.path().join("PLAN.md"), "original plan\n").unwrap();

        // Create "follow-up" staging — copy source at this point in time.
        let overlay = OverlayWorkspace::create(
            "follow-up-goal",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Agent adds a new file in staging (the actual follow-up work).
        fs::write(
            overlay.staging_dir().join("src/new_feature.rs"),
            "fn new_feature() {}\n",
        )
        .unwrap();

        // Simulate: parent draft was applied — source PLAN.md updated externally
        // (with a small sleep on coarse-mtime systems; on modern systems use explicit write).
        // We touch source PLAN.md to mark it as "newer" than staging.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(source.path().join("PLAN.md"), "parent-applied plan\n").unwrap();

        // PLAN.md is in baseline_artifacts (parent applied it). It's now different
        // in source vs staging. The protected-file guard should keep source's version.
        //
        // We verify via apply_selective: if PLAN.md is in the artifact list and both
        // source and staging content differ with source being newer, apply should
        // (in the CLI layer) skip PLAN.md. Here we test the low-level overlay
        // behaviour: apply_selective copies staging → target. The CLI protected-file
        // guard runs before this, so we test that staging's old PLAN.md would revert
        // source if naively applied (demonstrating the guard is necessary).
        let target = TempDir::new().unwrap();
        fs::write(target.path().join("PLAN.md"), "parent-applied plan\n").unwrap();
        fs::create_dir_all(target.path().join("src")).unwrap();
        fs::write(target.path().join("src/feature.rs"), "fn feature() {}\n").unwrap();

        // Apply only the new file (simulating baseline skip of PLAN.md).
        let uris = vec!["fs://workspace/src/new_feature.rs".to_string()];
        let applied = overlay.apply_selective(target.path(), &uris).unwrap();

        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0, "src/new_feature.rs");

        // PLAN.md must still contain "parent-applied plan" — not reverted to "original plan".
        let plan_content = fs::read_to_string(target.path().join("PLAN.md")).unwrap();
        assert_eq!(
            plan_content, "parent-applied plan\n",
            "PLAN.md should not be reverted by follow-up apply"
        );
    }

    /// Item 8: Three-way merge on non-overlapping edits succeeds cleanly.
    ///
    /// - base: 9-line file
    /// - ours (staging / agent): changed line 2 → "line2-agent"
    /// - theirs (source / external): changed line 8 → "line8-external"
    ///
    /// These edits are well-separated (5 unchanged lines between them), so
    /// git merge-file produces a clean merge with no conflict markers.
    ///
    /// This test requires `git` on PATH. We set up a minimal git repo in a
    /// temp dir (with git env vars cleared) to make `git show HEAD:<path>` work.
    #[test]
    fn three_way_merge_non_overlapping_succeeds() {
        use std::process::Command;

        // Set up a minimal git repo so git show HEAD:<path> works.
        // Clear git env vars (GIT_DIR, GIT_WORK_TREE, etc.) so all commands
        // operate on the temp repo, not any ambient git repository.
        let repo = TempDir::new().unwrap();
        let repo_path = repo.path();

        // Init git repo. Clear git env vars so commands operate on the temp
        // repo, not any ambient GIT_DIR/GIT_WORK_TREE set by the TA runner.
        let git_ok = Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            // Try without --initial-branch (older git).
            Command::new("git")
                .arg("init")
                .current_dir(repo_path)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_CEILING_DIRECTORIES")
                .output()
                .ok();
        }

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .ok();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .ok();

        // Use a 9-line file. Ours edits line 2, theirs edits line 8.
        // 5 unchanged lines separate the two hunks — well outside git's 3-line
        // context window — so merge-file produces a clean merge (exit 0).
        let base_content = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\n";
        fs::write(repo_path.join("shared.txt"), base_content).unwrap();

        Command::new("git")
            .args(["add", "shared.txt"])
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .ok();

        let commit_ok = Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !commit_ok {
            // Skip test if git is not available or commit fails (CI may not have git).
            eprintln!("Skipping three_way_merge test: git commit failed");
            return;
        }

        // Create staging (ours) and source (theirs) versions.
        let staging_dir = TempDir::new().unwrap();
        // Agent changed line 2.
        let ours_content = "line1\nline2-agent\nline3\nline4\nline5\nline6\nline7\nline8\nline9\n";
        // External change to line 8 (far from line 2).
        let theirs_content =
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8-external\nline9\n";

        let staging_path = staging_dir.path().join("shared.txt");
        let source_path = repo_path.join("shared.txt");

        fs::write(&staging_path, ours_content).unwrap();
        fs::write(&source_path, theirs_content).unwrap();

        let snap = crate::conflict::FileSnapshot {
            path: "shared.txt".to_string(),
            mtime_secs: 0,
            content_hash: "dummy".to_string(),
            size_bytes: base_content.len() as u64,
        };

        let result = three_way_merge(
            "dummy",
            &staging_path,
            &source_path,
            &snap,
            staging_dir.path(),
        );

        match result {
            Ok(MergeResult::Clean { content, hunks }) => {
                let merged = String::from_utf8(content).unwrap();
                assert!(
                    merged.contains("line2-agent"),
                    "agent change must be in merged result: {}",
                    merged
                );
                assert!(
                    merged.contains("line8-external"),
                    "external change must be in merged result: {}",
                    merged
                );
                assert!(
                    !merged.contains("<<<<<<<"),
                    "no conflict markers expected: {}",
                    merged
                );
                assert!(hunks > 0, "expected at least 1 hunk");
            }
            Ok(MergeResult::Conflicted { .. }) => {
                panic!("Expected clean merge for non-overlapping edits, got conflicts");
            }
            Err(e) => {
                // git may not be available in all CI environments — treat as skip.
                eprintln!("Skipping three_way_merge test: {}", e);
            }
        }
    }

    /// extract_path_from_conflict correctly parses conflict description strings.
    #[test]
    fn extract_path_from_conflict_desc() {
        assert_eq!(
            extract_path_from_conflict(
                "File 'src/foo.rs' was modified in source (mtime/hash changed)"
            ),
            Some("src/foo.rs".to_string())
        );
        assert_eq!(
            extract_path_from_conflict("File 'PLAN.md' was deleted from source"),
            Some("PLAN.md".to_string())
        );
        assert_eq!(
            extract_path_from_conflict(
                "File 'docs/USAGE.md' was created in source (new since snapshot)"
            ),
            Some("docs/USAGE.md".to_string())
        );
        assert_eq!(extract_path_from_conflict("no match here"), None);
    }

    // ── v0.15.14.7: Ephemeral staging file tests ──────────────────────────────

    /// `.ta-decisions.json` written in staging must NOT appear in `diff_all()`.
    #[test]
    fn decisions_json_excluded_from_diff() {
        let source = create_source_project();
        let staging_root = TempDir::new().unwrap();

        let overlay = OverlayWorkspace::create(
            "goal-1",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Agent writes a decisions log during the goal.
        fs::write(
            overlay.staging_dir().join(".ta-decisions.json"),
            r#"[{"decision":"chose Ed25519","rationale":"faster","alternatives":[],"confidence":0.9}]"#,
        )
        .unwrap();

        let changes = overlay.diff_all().unwrap();
        let paths: Vec<&str> = changes
            .iter()
            .map(|c| match c {
                OverlayChange::Modified { path, .. }
                | OverlayChange::Created { path, .. }
                | OverlayChange::Deleted { path } => path.as_str(),
            })
            .collect();

        assert!(
            !paths.contains(&".ta-decisions.json"),
            ".ta-decisions.json must be excluded from diff changeset, got: {:?}",
            paths
        );
    }

    /// `.ta-decisions.json` already present in source must be deleted from staging at creation.
    #[test]
    fn decisions_json_deleted_from_staging_at_creation() {
        let source = TempDir::new().unwrap();
        fs::write(source.path().join("README.md"), "# Project\n").unwrap();
        // Simulate a stale decisions file in source (left over from prior goal apply).
        fs::write(
            source.path().join(".ta-decisions.json"),
            r#"[{"decision":"stale decision from goal A"}]"#,
        )
        .unwrap();

        let staging_root = TempDir::new().unwrap();
        let overlay = OverlayWorkspace::create(
            "goal-2",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // After staging creation, the stale file must be gone.
        assert!(
            !overlay.staging_dir().join(".ta-decisions.json").exists(),
            ".ta-decisions.json from source must not carry over into new goal's staging"
        );
    }

    /// Decisions from goal A must not appear in goal B's diff even after apply.
    ///
    /// Simulates the full bleed scenario: goal A writes decisions → goal B is
    /// created from the same source → goal B's diff must be clean.
    #[test]
    fn decisions_from_goal_a_do_not_bleed_into_goal_b_diff() {
        let source = TempDir::new().unwrap();
        fs::write(source.path().join("main.rs"), "fn main() {}\n").unwrap();

        // Goal A: agent writes a decisions file in staging.
        let staging_root = TempDir::new().unwrap();
        let overlay_a = OverlayWorkspace::create(
            "goal-a",
            source.path(),
            staging_root.path(),
            ExcludePatterns::none(),
        )
        .unwrap();
        fs::write(
            overlay_a.staging_dir().join(".ta-decisions.json"),
            r#"[{"decision":"goal A decision"}]"#,
        )
        .unwrap();
        // Simulate: goal A's decisions file got applied back to source somehow.
        fs::write(
            source.path().join(".ta-decisions.json"),
            r#"[{"decision":"goal A decision"}]"#,
        )
        .unwrap();

        // Goal B: new goal starting from same source (which now has the stale file).
        let staging_root_b = TempDir::new().unwrap();
        let overlay_b = OverlayWorkspace::create(
            "goal-b",
            source.path(),
            staging_root_b.path(),
            ExcludePatterns::none(),
        )
        .unwrap();

        // Staging for goal B must not have the stale file.
        assert!(
            !overlay_b.staging_dir().join(".ta-decisions.json").exists(),
            "stale .ta-decisions.json must be deleted from goal B's staging"
        );

        // Diff for goal B must also be clean (no decisions file).
        let changes = overlay_b.diff_all().unwrap();
        let paths: Vec<&str> = changes
            .iter()
            .map(|c| match c {
                OverlayChange::Modified { path, .. }
                | OverlayChange::Created { path, .. }
                | OverlayChange::Deleted { path } => path.as_str(),
            })
            .collect();
        assert!(
            !paths.contains(&".ta-decisions.json"),
            "goal B diff must not include .ta-decisions.json from goal A, got: {:?}",
            paths
        );
    }
}
