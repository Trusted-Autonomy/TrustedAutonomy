// frame_watcher.rs — ComfyUI output watcher → ArtifactKind::Video/Image.
//
// Scans the ComfyUI output directory after a job completes. Copies video/image
// files to `.ta/staging/<goal-id>/comfyui_output/` and tags each with the
// appropriate ArtifactKind so the draft/review/apply pipeline can surface them.

use std::path::{Path, PathBuf};

use ta_changeset::ArtifactKind;

use crate::error::ComfyUiError;

/// A file artifact produced by ComfyUI and ingested into TA staging.
#[derive(Debug, Clone)]
pub struct ComfyUiArtifact {
    /// Path to the copied file inside the TA staging directory.
    pub staging_path: PathBuf,
    /// Original path in the ComfyUI output directory.
    pub source_path: PathBuf,
    /// Byte size of the file.
    pub file_size: u64,
    /// Semantic kind (Video or Image) for the draft pipeline.
    pub kind: ArtifactKind,
}

/// Watches a ComfyUI output directory and ingests artifacts into TA staging.
pub struct ComfyUiOutputWatcher {
    /// Root of the ComfyUI output directory.
    output_dir: PathBuf,
    /// TA staging base directory (`.ta/staging/<goal-id>/`).
    staging_base: PathBuf,
}

impl ComfyUiOutputWatcher {
    pub fn new(output_dir: impl Into<PathBuf>, staging_base: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            staging_base: staging_base.into(),
        }
    }

    /// Scan the output directory, copy all video/image files into staging, and
    /// return a descriptor for each ingested file.
    ///
    /// Only copies files that match the provided `filenames` list (from a job's
    /// `output_files`). If `filenames` is empty, copies all matching files.
    pub fn ingest(&self, filenames: &[String]) -> Result<Vec<ComfyUiArtifact>, ComfyUiError> {
        if !self.output_dir.exists() {
            return Ok(Vec::new());
        }

        let staging_out = self.staging_base.join("comfyui_output");
        std::fs::create_dir_all(&staging_out)?;

        let mut artifacts = Vec::new();

        for entry in collect_files(&self.output_dir)? {
            if !is_media_file(&entry) {
                continue;
            }

            let filename = match entry.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Filter to the requested files (if any).
            if !filenames.is_empty() && !filenames.contains(&filename) {
                continue;
            }

            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let kind = artifact_kind_for(&entry);

            let staging_path = staging_out.join(&filename);
            std::fs::copy(&entry, &staging_path)?;

            artifacts.push(ComfyUiArtifact {
                staging_path,
                source_path: entry,
                file_size,
                kind,
            });
        }

        Ok(artifacts)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn collect_files(dir: &Path) -> Result<Vec<PathBuf>, ComfyUiError> {
    let mut files = Vec::new();
    collect_recursive(dir, 0, &mut files)?;
    Ok(files)
}

fn collect_recursive(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) -> Result<(), ComfyUiError> {
    if depth > 3 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, depth + 1, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn is_media_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("mp4")
            | Some("mov")
            | Some("webm")
            | Some("png")
            | Some("jpg")
            | Some("jpeg")
            | Some("exr")
    )
}

fn artifact_kind_for(path: &Path) -> ArtifactKind {
    match path.extension().and_then(|e| e.to_str()) {
        Some("mp4") => ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("MP4".to_string()),
            frame_count: None,
        },
        Some("mov") => ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("MOV".to_string()),
            frame_count: None,
        },
        Some("webm") => ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("WebM".to_string()),
            frame_count: None,
        },
        Some("jpg") | Some("jpeg") => ArtifactKind::Image {
            width: None,
            height: None,
            format: Some("JPEG".to_string()),
            frame_index: None,
        },
        Some("exr") => ArtifactKind::Image {
            width: None,
            height: None,
            format: Some("EXR".to_string()),
            frame_index: None,
        },
        _ => ArtifactKind::Image {
            width: None,
            height: None,
            format: Some("PNG".to_string()),
            frame_index: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_stub(path: &Path, bytes: &[u8]) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn ingest_mp4_tagged_as_video() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("output.mp4"), &[0u8; 128]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].kind.is_video(), "mp4 should be Video kind");
        assert_eq!(artifacts[0].kind.display_label(), "MP4 video");
    }

    #[test]
    fn ingest_png_tagged_as_image() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("frame_0001.png"), &[0u8; 64]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].kind.is_image(), "png should be Image kind");
        assert_eq!(artifacts[0].kind.display_label(), "PNG image");
    }

    #[test]
    fn ingest_filters_by_filename() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("a.mp4"), &[0u8; 64]);
        write_stub(&output_dir.path().join("b.mp4"), &[0u8; 64]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&["a.mp4".to_string()]).unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0]
                .source_path
                .file_name()
                .and_then(|n| n.to_str()),
            Some("a.mp4")
        );
    }

    #[test]
    fn ingest_non_media_files_skipped() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("manifest.json"), b"{}");
        write_stub(&output_dir.path().join("output.mp4"), &[0u8; 32]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1, "only mp4 should be ingested");
    }

    #[test]
    fn ingest_empty_dir_returns_empty() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn nonexistent_output_dir_returns_empty() {
        let staging_dir = tempdir().unwrap();
        let watcher = ComfyUiOutputWatcher::new("/nonexistent/comfyui/output", staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn staging_path_under_comfyui_output() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("wan2_0001.mp4"), &[0u8; 16]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1);
        let rel = artifacts[0]
            .staging_path
            .strip_prefix(staging_dir.path())
            .unwrap();
        assert!(
            rel.starts_with("comfyui_output/"),
            "staging path should be under comfyui_output/, got: {:?}",
            rel
        );
    }

    #[test]
    fn webm_tagged_as_video() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("clip.webm"), &[0u8; 64]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].kind.is_video());
        assert_eq!(artifacts[0].kind.display_label(), "WebM video");
    }

    #[test]
    fn exr_tagged_as_image() {
        let output_dir = tempdir().unwrap();
        let staging_dir = tempdir().unwrap();
        write_stub(&output_dir.path().join("depth.exr"), &[0u8; 64]);

        let watcher = ComfyUiOutputWatcher::new(output_dir.path(), staging_dir.path());
        let artifacts = watcher.ingest(&[]).unwrap();

        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].kind.is_image());
        assert_eq!(artifacts[0].kind.display_label(), "EXR image");
    }
}
