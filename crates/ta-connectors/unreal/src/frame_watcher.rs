// frame_watcher.rs — Frames-to-staging watcher for UE5 MRQ output (v0.14.15.1).
//
// Scans an MRQ output directory, copies rendered frame files into the TA
// staging path structure, and returns `FrameArtifact` descriptors so the
// draft/review/apply pipeline can surface them to the reviewer.
//
// Staging layout:
//   <staging_base>/render_output/<preset_name>/<pass>/filename
//
// The watcher supports the three standard render passes defined in `mrq::RenderPass`
// (png, depth_exr, normal_exr). Pass is inferred from:
//   1. The immediate parent directory name (matched against `RenderPass::dir_name()`).
//   2. The file extension (`png` → Png, `exr` → DepthExr) when the parent dir
//      name is not a recognised pass name.

use std::path::{Path, PathBuf};

use ta_changeset::ArtifactKind;

use crate::{error::UnrealConnectorError, mrq::RenderPass};

/// A rendered frame that has been ingested into TA staging.
#[derive(Debug, Clone)]
pub struct FrameArtifact {
    /// Path to the copied frame inside the TA staging directory.
    pub staging_path: PathBuf,
    /// Original path in the MRQ output directory.
    pub source_path: PathBuf,
    /// Render pass this frame belongs to.
    pub pass: RenderPass,
    /// Zero-based frame index derived from the filename.
    pub frame_index: u32,
    /// Byte size of the frame file.
    pub file_size: u64,
    /// `ArtifactKind::Image` metadata for the draft pipeline.
    pub kind: ArtifactKind,
}

/// Watches an MRQ output directory and ingests frames into TA staging.
pub struct FrameWatcher {
    /// Root of the MRQ output directory.
    output_dir: PathBuf,
    /// TA staging base directory (`.ta/staging/<goal-id>/`).
    staging_base: PathBuf,
    /// Preset name used as the first path component under `render_output/`.
    preset_name: String,
}

impl FrameWatcher {
    pub fn new(
        output_dir: impl Into<PathBuf>,
        staging_base: impl Into<PathBuf>,
        preset_name: impl Into<String>,
    ) -> Self {
        Self {
            output_dir: output_dir.into(),
            staging_base: staging_base.into(),
            preset_name: preset_name.into(),
        }
    }

    /// Scan the output directory, copy all image frames into staging, and return
    /// a descriptor for each ingested frame.
    ///
    /// This is a synchronous, single-pass scan. Call it repeatedly while the MRQ
    /// job runs to pick up new frames, or once after completion for a bulk ingest.
    pub fn ingest_frames(&self) -> Result<Vec<FrameArtifact>, UnrealConnectorError> {
        if !self.output_dir.exists() {
            return Ok(Vec::new());
        }

        let mut artifacts = Vec::new();

        // Walk the output directory. We handle both flat and pass-subdirectory layouts.
        for entry in walkdir(&self.output_dir)? {
            if !is_image_file(&entry) {
                continue;
            }

            let pass = detect_pass(&entry, &self.output_dir);
            let frame_index = parse_frame_index(&entry);
            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);

            let format = pass.artifact_format();
            let kind = ArtifactKind::Image {
                width: None,
                height: None,
                format: Some(format.to_string()),
                frame_index: Some(frame_index),
            };

            // Build staging destination.
            let filename = entry.file_name().unwrap_or_default();
            let staging_path = self
                .staging_base
                .join("render_output")
                .join(&self.preset_name)
                .join(pass.dir_name())
                .join(filename);

            // Create parent dirs and copy.
            if let Some(parent) = staging_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&entry, &staging_path)?;

            artifacts.push(FrameArtifact {
                staging_path,
                source_path: entry.clone(),
                pass,
                frame_index,
                file_size,
                kind,
            });
        }

        // Sort by pass dir_name then frame_index for deterministic ordering.
        artifacts.sort_by(|a, b| {
            a.pass
                .dir_name()
                .cmp(b.pass.dir_name())
                .then(a.frame_index.cmp(&b.frame_index))
        });

        Ok(artifacts)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Collect all files under `dir` (up to 2 levels deep — output root + pass subdirs).
fn walkdir(dir: &Path) -> Result<Vec<PathBuf>, UnrealConnectorError> {
    let mut files = Vec::new();
    collect_files(dir, 0, &mut files)?;
    Ok(files)
}

fn collect_files(
    dir: &Path,
    depth: u32,
    out: &mut Vec<PathBuf>,
) -> Result<(), UnrealConnectorError> {
    if depth > 2 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, depth + 1, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("png") | Some("exr")
    )
}

/// Determine the `RenderPass` for a given frame file.
///
/// Strategy:
/// 1. If the immediate parent directory name matches a pass dir name, use that.
/// 2. Otherwise, fall back to the file extension (`png` → `Png`, `exr` → `DepthExr`).
fn detect_pass(path: &Path, root: &Path) -> RenderPass {
    // Check if the immediate parent is a named pass subdirectory.
    if let Some(parent) = path.parent() {
        if parent != root {
            if let Some(dir_name) = parent.file_name().and_then(|n| n.to_str()) {
                if let Some(pass) = RenderPass::from_dir_name(dir_name) {
                    return pass;
                }
            }
        }
    }
    // Fall back to extension.
    match path.extension().and_then(|e| e.to_str()) {
        Some("exr") => RenderPass::DepthExr,
        _ => RenderPass::Png,
    }
}

/// Extract a zero-based frame index from a filename like `frame_0042.png`.
/// Falls back to 0 for unrecognised naming conventions.
fn parse_frame_index(path: &Path) -> u32 {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    // Accept patterns: frame_NNNN, shot_NNNN, 0042, NNNN
    if let Some(n) = stem.rsplit('_').next() {
        if let Ok(idx) = n.parse::<u32>() {
            return idx;
        }
    }
    // Stem is purely numeric (e.g. "0042")
    if let Ok(idx) = stem.parse::<u32>() {
        return idx;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a tiny stub PNG file (just needs to exist and have bytes).
    fn write_stub(path: &Path, bytes: &[u8]) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn ingest_three_flat_png_frames() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();

        for i in 0u32..3 {
            write_stub(
                &output_dir.path().join(format!("frame_{:04}.png", i)),
                &[0u8; 64],
            );
        }

        let watcher = FrameWatcher::new(output_dir.path(), staging_dir.path(), "test_preset");

        let artifacts = watcher.ingest_frames().unwrap();
        assert_eq!(artifacts.len(), 3, "expected 3 frame artifacts");

        for (i, artifact) in artifacts.iter().enumerate() {
            assert_eq!(artifact.pass, RenderPass::Png);
            assert_eq!(artifact.frame_index, i as u32);
            assert_eq!(artifact.file_size, 64);
            assert!(
                artifact.staging_path.exists(),
                "staged file should exist at {:?}",
                artifact.staging_path
            );
            // Staging path must be under render_output/<preset>/<pass>/
            let rel = artifact
                .staging_path
                .strip_prefix(staging_dir.path())
                .unwrap();
            assert!(rel.starts_with("render_output/test_preset/png/"));
        }
    }

    #[test]
    fn ingest_pass_subdirectory_layout() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();

        // PNG pass subdirectory.
        for i in 0u32..3 {
            write_stub(
                &output_dir
                    .path()
                    .join("png")
                    .join(format!("frame_{:04}.png", i)),
                &[1u8; 32],
            );
        }
        // Depth EXR pass subdirectory.
        for i in 0u32..3 {
            write_stub(
                &output_dir
                    .path()
                    .join("depth_exr")
                    .join(format!("frame_{:04}.exr", i)),
                &[2u8; 48],
            );
        }

        let watcher = FrameWatcher::new(output_dir.path(), staging_dir.path(), "day_shot");
        let artifacts = watcher.ingest_frames().unwrap();

        assert_eq!(artifacts.len(), 6, "3 PNG + 3 EXR = 6 frames");

        let png_count = artifacts
            .iter()
            .filter(|a| a.pass == RenderPass::Png)
            .count();
        let exr_count = artifacts
            .iter()
            .filter(|a| a.pass == RenderPass::DepthExr)
            .count();
        assert_eq!(png_count, 3);
        assert_eq!(exr_count, 3);

        // Verify staging paths.
        for a in &artifacts {
            assert!(a.staging_path.exists());
            let rel = a.staging_path.strip_prefix(staging_dir.path()).unwrap();
            let pass_dir = a.pass.dir_name();
            assert!(
                rel.starts_with(format!("render_output/day_shot/{}/", pass_dir).as_str()),
                "bad staging path: {:?}",
                rel
            );
        }
    }

    #[test]
    fn artifact_kind_is_image_with_correct_format() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();

        write_stub(&output_dir.path().join("frame_0000.png"), &[0u8; 16]);
        write_stub(
            &output_dir.path().join("depth_exr").join("frame_0000.exr"),
            &[0u8; 16],
        );

        let watcher = FrameWatcher::new(output_dir.path(), staging_dir.path(), "p");
        let artifacts = watcher.ingest_frames().unwrap();

        for a in &artifacts {
            assert!(a.kind.is_image(), "kind should be Image");
            let label = a.kind.display_label();
            if a.pass == RenderPass::Png {
                assert!(
                    label.contains("PNG"),
                    "PNG pass should have PNG label, got: {}",
                    label
                );
            } else {
                assert!(
                    label.contains("EXR"),
                    "EXR pass should have EXR label, got: {}",
                    label
                );
            }
        }
    }

    #[test]
    fn empty_output_dir_returns_no_artifacts() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        let watcher = FrameWatcher::new(output_dir.path(), staging_dir.path(), "p");
        let artifacts = watcher.ingest_frames().unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn nonexistent_output_dir_returns_empty() {
        let staging_dir = tempdir().unwrap();
        let watcher = FrameWatcher::new("/nonexistent/render/output", staging_dir.path(), "p");
        let artifacts = watcher.ingest_frames().unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn non_image_files_are_skipped() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("manifest.json"), b"{}\n");
        write_stub(&output_dir.path().join("frame_0000.tmp"), &[0u8; 8]);
        write_stub(&output_dir.path().join("frame_0000.png"), &[0u8; 8]);

        let watcher = FrameWatcher::new(output_dir.path(), staging_dir.path(), "p");
        let artifacts = watcher.ingest_frames().unwrap();
        assert_eq!(artifacts.len(), 1, "only the PNG should be ingested");
    }

    #[test]
    fn parse_frame_index_variants() {
        assert_eq!(parse_frame_index(Path::new("frame_0042.png")), 42);
        assert_eq!(parse_frame_index(Path::new("shot_0001.exr")), 1);
        assert_eq!(parse_frame_index(Path::new("0003.png")), 3);
        assert_eq!(parse_frame_index(Path::new("noindex.png")), 0);
    }

    #[test]
    fn detect_pass_prefers_parent_dir() {
        let root = Path::new("/out");
        assert_eq!(
            detect_pass(Path::new("/out/depth_exr/frame_0000.exr"), root),
            RenderPass::DepthExr
        );
        assert_eq!(
            detect_pass(Path::new("/out/normal_exr/frame_0000.exr"), root),
            RenderPass::NormalExr
        );
    }

    #[test]
    fn detect_pass_falls_back_to_extension() {
        let root = Path::new("/out");
        assert_eq!(
            detect_pass(Path::new("/out/frame_0000.exr"), root),
            RenderPass::DepthExr
        );
        assert_eq!(
            detect_pass(Path::new("/out/frame_0000.png"), root),
            RenderPass::Png
        );
    }
}
