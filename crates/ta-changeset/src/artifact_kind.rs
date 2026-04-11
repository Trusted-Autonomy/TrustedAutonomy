// artifact_kind.rs — ArtifactKind enum for typed artifact metadata (v0.14.15+).
//
// Describes the semantic kind of an artifact so the draft review pipeline can
// render appropriate summaries. For example, binary image artifacts suppress
// the text diff and show a human-readable frame/resolution summary instead.

use serde::{Deserialize, Serialize};

/// Semantic kind of an artifact produced by a connector or agent.
///
/// Stored on [`Artifact`] as an optional field. When absent, the artifact is
/// treated as a generic file. The `ta draft view` renderer uses this to pick
/// the appropriate display format (e.g. suppress binary diffs for images).
///
/// # Extensibility
/// Future connectors can add new variants here (e.g. `Audio`, `PointCloud`).
/// The `Image` and `Video` variants are intentionally generic — they are not
/// tied to Unreal Engine or any other specific connector.
///
/// [`Artifact`]: crate::draft_package::Artifact
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A raster image artifact (PNG, EXR, JPEG, …).
    ///
    /// Fields are optional — connectors fill in what they know. Width and
    /// height are in pixels; `frame_index` is zero-based within a sequence.
    Image {
        /// Image width in pixels, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// Image height in pixels, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        /// File format string, e.g. `"PNG"`, `"EXR"`, `"JPEG"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// Zero-based frame index within a render sequence, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_index: Option<u32>,
    },
    /// A video artifact (MP4, MOV, WebM, …) produced by render pipelines.
    ///
    /// Text diff is suppressed — `ta draft view` shows a metadata summary like
    /// "Video: 1920×1080, 24fps, 6.2s, MP4" instead of binary content.
    Video {
        /// Frame width in pixels, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// Frame height in pixels, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        /// Frames per second, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<f32>,
        /// Duration in seconds, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f32>,
        /// Container/format string, e.g. `"MP4"`, `"MOV"`, `"WebM"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// Total frame count, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_count: Option<u32>,
    },
    /// An opaque binary artifact (compiled output, archive, model weights, …).
    ///
    /// Text diff is suppressed — `ta draft view` shows a hex summary or
    /// `(binary file, N bytes)` instead.
    Binary {
        /// MIME type string, e.g. `"application/octet-stream"`, `"application/zip"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// File size in bytes, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        byte_size: Option<u64>,
    },
    /// A raw text artifact (generated script, config file, data file, …).
    ///
    /// Full unified diff is rendered in `ta draft view`.
    Text {
        /// Character encoding, e.g. `"utf-8"`, `"latin-1"`. Defaults to UTF-8 if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<String>,
        /// Number of lines in the file, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_count: Option<u64>,
    },

    /// A memory summary artifact produced by an analysis/learning goal run (v0.15.13.2).
    ///
    /// Produced when `ta draft build` detects an empty overlay diff but finds memory
    /// entries written by this goal run. The changeset holds a rendered summary of all
    /// entries (key, scope, value) for human review.
    ///
    /// On approve: entries remain in the store — they were written during the run.
    /// On deny: entries are removed from the store using `entry_ids`.
    MemorySummary {
        /// Number of memory entries created during this goal run.
        entry_count: usize,
        /// Entry IDs (as strings) used by `ta draft deny` to remove them from the store.
        ///
        /// These are the `entry_id` (UUID) values from each `MemoryEntry`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entry_ids: Vec<String>,
    },
}

impl ArtifactKind {
    /// Returns true if this is an image kind (binary; diff should be suppressed).
    pub fn is_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }

    /// Returns true if this is a video kind (binary; diff should be suppressed).
    pub fn is_video(&self) -> bool {
        matches!(self, Self::Video { .. })
    }

    /// Returns true if this is a binary kind (diff should be suppressed).
    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary { .. })
    }

    /// Returns true if this is a text kind (full diff should be rendered).
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text { .. })
    }

    /// Returns true if this is a memory summary kind (no file diff; rendered as entry list).
    pub fn is_memory_summary(&self) -> bool {
        matches!(self, Self::MemorySummary { .. })
    }

    /// Returns a short human-readable label for display (e.g. `"MP4 video"`, `"PNG image"`).
    pub fn display_label(&self) -> String {
        match self {
            Self::Image { format, .. } => match format.as_deref() {
                Some(fmt) => format!("{} image", fmt),
                None => "image".to_string(),
            },
            Self::Video { format, .. } => match format.as_deref() {
                Some(fmt) => format!("{} video", fmt),
                None => "video".to_string(),
            },
            Self::Binary { mime_type, .. } => match mime_type.as_deref() {
                Some(mime) => format!("binary ({})", mime),
                None => "binary".to_string(),
            },
            Self::Text { encoding, .. } => match encoding.as_deref() {
                Some(enc) => format!("text ({})", enc),
                None => "text".to_string(),
            },
            Self::MemorySummary { entry_count, .. } => {
                format!("memory summary ({} entries)", entry_count)
            }
        }
    }

    /// Returns a compact metadata summary for display in `ta draft view`.
    ///
    /// For `Video`: `"Video: 1920×1080, 24fps, 6.2s, MP4"` (omits unknown fields).
    /// For other kinds: returns an empty string.
    pub fn video_metadata_summary(&self) -> String {
        let Self::Video {
            width,
            height,
            fps,
            duration_secs,
            format,
            ..
        } = self
        else {
            return String::new();
        };

        let mut parts: Vec<String> = Vec::new();
        if let (Some(w), Some(h)) = (width, height) {
            parts.push(format!("{}×{}", w, h));
        }
        if let Some(f) = fps {
            parts.push(format!("{}fps", f));
        }
        if let Some(d) = duration_secs {
            parts.push(format!("{:.1}s", d));
        }
        if let Some(fmt) = format {
            parts.push(fmt.clone());
        }

        if parts.is_empty() {
            "Video".to_string()
        } else {
            format!("Video: {}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_roundtrip_full() {
        let kind = ArtifactKind::Image {
            width: Some(1024),
            height: Some(1024),
            format: Some("PNG".to_string()),
            frame_index: Some(0),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn image_roundtrip_minimal() {
        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn image_serialized_has_type_tag() {
        let kind = ArtifactKind::Image {
            width: Some(1920),
            height: Some(1080),
            format: Some("EXR".to_string()),
            frame_index: Some(5),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"image\""), "json: {}", json);
        assert!(json.contains("1920"));
        assert!(json.contains("1080"));
        assert!(json.contains("EXR"));
        assert!(json.contains("5"));
    }

    #[test]
    fn image_minimal_omits_none_fields() {
        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        // Only the type tag should appear; all None fields are skipped.
        assert_eq!(json, r#"{"type":"image"}"#, "json: {}", json);
    }

    #[test]
    fn is_image() {
        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };
        assert!(kind.is_image());
    }

    #[test]
    fn display_label_with_format() {
        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: Some("PNG".to_string()),
            frame_index: None,
        };
        assert_eq!(kind.display_label(), "PNG image");
    }

    #[test]
    fn display_label_no_format() {
        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };
        assert_eq!(kind.display_label(), "image");
    }

    // ── Binary variant tests ──

    #[test]
    fn binary_roundtrip_full() {
        let kind = ArtifactKind::Binary {
            mime_type: Some("application/zip".to_string()),
            byte_size: Some(1_048_576),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn binary_roundtrip_minimal() {
        let kind = ArtifactKind::Binary {
            mime_type: None,
            byte_size: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
        assert_eq!(json, r#"{"type":"binary"}"#, "json: {}", json);
    }

    #[test]
    fn binary_serialized_has_type_tag() {
        let kind = ArtifactKind::Binary {
            mime_type: Some("application/octet-stream".to_string()),
            byte_size: Some(512),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"binary\""), "json: {}", json);
        assert!(json.contains("application/octet-stream"));
        assert!(json.contains("512"));
    }

    #[test]
    fn is_binary() {
        let kind = ArtifactKind::Binary {
            mime_type: None,
            byte_size: None,
        };
        assert!(kind.is_binary());
        assert!(!kind.is_image());
        assert!(!kind.is_text());
    }

    #[test]
    fn binary_display_label_with_mime() {
        let kind = ArtifactKind::Binary {
            mime_type: Some("application/zip".to_string()),
            byte_size: None,
        };
        assert_eq!(kind.display_label(), "binary (application/zip)");
    }

    #[test]
    fn binary_display_label_no_mime() {
        let kind = ArtifactKind::Binary {
            mime_type: None,
            byte_size: None,
        };
        assert_eq!(kind.display_label(), "binary");
    }

    // ── Text variant tests ──

    #[test]
    fn text_roundtrip_full() {
        let kind = ArtifactKind::Text {
            encoding: Some("utf-8".to_string()),
            line_count: Some(200),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn text_roundtrip_minimal() {
        let kind = ArtifactKind::Text {
            encoding: None,
            line_count: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
        assert_eq!(json, r#"{"type":"text"}"#, "json: {}", json);
    }

    #[test]
    fn text_serialized_has_type_tag() {
        let kind = ArtifactKind::Text {
            encoding: Some("latin-1".to_string()),
            line_count: Some(42),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"text\""), "json: {}", json);
        assert!(json.contains("latin-1"));
        assert!(json.contains("42"));
    }

    #[test]
    fn is_text() {
        let kind = ArtifactKind::Text {
            encoding: None,
            line_count: None,
        };
        assert!(kind.is_text());
        assert!(!kind.is_image());
        assert!(!kind.is_binary());
    }

    #[test]
    fn text_display_label_with_encoding() {
        let kind = ArtifactKind::Text {
            encoding: Some("utf-8".to_string()),
            line_count: None,
        };
        assert_eq!(kind.display_label(), "text (utf-8)");
    }

    #[test]
    fn text_display_label_no_encoding() {
        let kind = ArtifactKind::Text {
            encoding: None,
            line_count: None,
        };
        assert_eq!(kind.display_label(), "text");
    }

    // ── Video variant tests ──

    #[test]
    fn video_roundtrip_full() {
        let kind = ArtifactKind::Video {
            width: Some(1920),
            height: Some(1080),
            fps: Some(24.0),
            duration_secs: Some(6.2),
            format: Some("MP4".to_string()),
            frame_count: Some(149),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn video_roundtrip_minimal() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: None,
            frame_count: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
        assert_eq!(json, r#"{"type":"video"}"#, "json: {}", json);
    }

    #[test]
    fn video_serialized_has_type_tag() {
        let kind = ArtifactKind::Video {
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            duration_secs: Some(10.5),
            format: Some("MOV".to_string()),
            frame_count: Some(315),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"video\""), "json: {}", json);
        assert!(json.contains("1920"));
        assert!(json.contains("1080"));
        assert!(json.contains("MOV"));
        assert!(json.contains("315"));
    }

    #[test]
    fn video_minimal_omits_none_fields() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: None,
            frame_count: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#"{"type":"video"}"#, "json: {}", json);
    }

    #[test]
    fn is_video() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: None,
            frame_count: None,
        };
        assert!(kind.is_video());
        assert!(!kind.is_image());
        assert!(!kind.is_binary());
        assert!(!kind.is_text());
    }

    #[test]
    fn video_display_label_with_format() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("MP4".to_string()),
            frame_count: None,
        };
        assert_eq!(kind.display_label(), "MP4 video");
    }

    #[test]
    fn video_display_label_no_format() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: None,
            frame_count: None,
        };
        assert_eq!(kind.display_label(), "video");
    }

    #[test]
    fn video_metadata_summary_full() {
        let kind = ArtifactKind::Video {
            width: Some(1920),
            height: Some(1080),
            fps: Some(24.0),
            duration_secs: Some(6.2),
            format: Some("MP4".to_string()),
            frame_count: Some(149),
        };
        let summary = kind.video_metadata_summary();
        assert!(
            summary.contains("1920×1080"),
            "should contain resolution; got: {}",
            summary
        );
        assert!(
            summary.contains("24fps") || summary.contains("24"),
            "should contain fps; got: {}",
            summary
        );
        assert!(
            summary.contains("6.2s"),
            "should contain duration; got: {}",
            summary
        );
        assert!(
            summary.contains("MP4"),
            "should contain format; got: {}",
            summary
        );
        assert!(
            summary.starts_with("Video:"),
            "should start with 'Video:'; got: {}",
            summary
        );
    }

    #[test]
    fn video_metadata_summary_partial() {
        // Only format known — resolution/fps/duration absent.
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("WebM".to_string()),
            frame_count: None,
        };
        let summary = kind.video_metadata_summary();
        assert!(summary.contains("WebM"), "got: {}", summary);
        assert!(summary.starts_with("Video:"), "got: {}", summary);
    }

    #[test]
    fn video_metadata_summary_empty_fields() {
        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: None,
            frame_count: None,
        };
        let summary = kind.video_metadata_summary();
        assert_eq!(summary, "Video");
    }

    #[test]
    fn video_metadata_summary_non_video_returns_empty() {
        let kind = ArtifactKind::Image {
            width: Some(100),
            height: Some(100),
            format: None,
            frame_index: None,
        };
        assert_eq!(kind.video_metadata_summary(), "");
    }
}
