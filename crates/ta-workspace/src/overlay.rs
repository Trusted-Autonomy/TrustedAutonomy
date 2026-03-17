// overlay.rs — Copy-on-write overlay workspace for transparent agent mediation.
//
// An OverlayWorkspace creates a full copy of a source project in a staging
// directory. The agent operates on the copy using its native tools — it sees
// a complete, normal-looking project. When work is done, TA diffs the staging
// copy against the original source to identify what changed.
//
// V1: Full directory copy (cross-platform, simple).
// V2 (future): Lazy copy-on-write via reflinks (APFS/Btrfs) or FUSE overlay.

use std::fs;
use std::path::{Path, PathBuf};

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
pub struct OverlayWorkspace {
    goal_id: String,
    source_dir: PathBuf,
    staging_dir: PathBuf,
    excludes: ExcludePatterns,               // V1 TEMPORARY
    source_snapshot: Option<SourceSnapshot>, // v0.2.1: Conflict detection
}

impl OverlayWorkspace {
    /// Create an overlay workspace by copying the source project to staging.
    ///
    /// Copies everything from `source_dir` to `staging_root/<goal_id>/`,
    /// excluding `.ta/` (always) and V1 exclude patterns.
    pub fn create(
        goal_id: impl Into<String>,
        source_dir: impl AsRef<Path>,
        staging_root: impl AsRef<Path>,
        excludes: ExcludePatterns, // V1 TEMPORARY
    ) -> Result<Self, WorkspaceError> {
        let goal_id = goal_id.into();
        let source_dir = source_dir.as_ref().to_path_buf();
        let staging_dir = staging_root.as_ref().join(&goal_id);

        fs::create_dir_all(&staging_dir).map_err(|source| WorkspaceError::IoError {
            path: staging_dir.clone(),
            source,
        })?;

        copy_dir_recursive(&source_dir, &staging_dir, &excludes)?;

        // v0.2.1: Capture source snapshot for conflict detection.
        let snapshot =
            SourceSnapshot::capture(&source_dir, |path| excludes.should_skip_path(path)).ok(); // Tolerate snapshot failure — conflict detection is optional.

        Ok(Self {
            goal_id,
            source_dir,
            staging_dir,
            excludes,
            source_snapshot: snapshot,
        })
    }

    /// Open an existing overlay workspace (for resuming after process restart).
    pub fn open(
        goal_id: impl Into<String>,
        source_dir: impl AsRef<Path>,
        staging_dir: impl AsRef<Path>,
        excludes: ExcludePatterns, // V1 TEMPORARY
    ) -> Self {
        Self {
            goal_id: goal_id.into(),
            source_dir: source_dir.as_ref().to_path_buf(),
            staging_dir: staging_dir.as_ref().to_path_buf(),
            excludes,
            source_snapshot: None, // Snapshot must be loaded separately if needed.
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
        walk_dir_relative(&self.staging_dir, &self.staging_dir, &mut staging_files)?;

        let mut source_files = Vec::new();
        walk_dir_relative(&self.source_dir, &self.source_dir, &mut source_files)?;

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
                                return Err(WorkspaceError::ConflictDetected {
                                    conflicts: vec![format!(
                                        "{} conflict(s) detected. Merge resolution requires VCS adapter (use `ta pr apply --submit` with git).",
                                        true_conflicts.len()
                                    )],
                                });
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

// ── Directory copy ──────────────────────────────────────────────

/// Recursively copy a directory, excluding `.ta/` (always) and V1 exclude patterns.
fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    excludes: &ExcludePatterns, // V1 TEMPORARY
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

        // V1 TEMPORARY: Check exclude patterns (includes hardcoded .ta/ skip).
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
            copy_dir_recursive(&src_path, &dst_path, excludes)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|source| WorkspaceError::IoError {
                path: dst_path,
                source,
            })?;
        }
    }

    Ok(())
}

// ── Directory walking ───────────────────────────────────────────

/// Walk a directory tree and collect relative file paths.
fn walk_dir_relative(
    dir: &Path,
    root: &Path,
    files: &mut Vec<String>,
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
            walk_dir_relative(&path, root, files)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            // Normalize to forward slashes so URIs are consistent across platforms.
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            files.push(rel_str);
        }
    }

    Ok(())
}

/// Check if a path should be skipped when diffing.
/// We skip infrastructure directories — these are internal state, not agent work product.
/// V1 TEMPORARY: Also checks exclude patterns for build artifacts that
/// agents may generate in staging (e.g., `cargo build` creates `target/`).
fn should_skip_for_diff(path: &str, excludes: &ExcludePatterns) -> bool {
    // Agent infrastructure directories (created at runtime, not work product).
    // Note: VCS metadata dirs (e.g., .git/, .svn/) are excluded via adapter-contributed
    // patterns merged into ExcludePatterns, not hardcoded here.
    const INFRA_DIRS: &[&str] = &[".ta", ".claude-flow", ".hive-mind", ".swarm"];

    for dir in INFRA_DIRS {
        if path == *dir
            || path.starts_with(&format!("{}/", dir))
            || path.starts_with(&format!("{}\\", dir))
        {
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
        copy_dir_recursive(source.path(), target.path(), &ExcludePatterns::none()).unwrap();

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
}
