// mrq.rs — Typed MRQ request/response types for the UE5 connector (v0.14.15.1).
//
// Provides strongly-typed structs for MRQ job submission, status polling,
// Sequencer queries, and lighting preset enumeration. These types are serialised
// over the MCP wire as JSON and also used internally by the frame watcher.

use serde::{Deserialize, Serialize};

/// Output pass selector for MRQ renders.
///
/// MRQ can produce multiple output passes per frame; callers specify which
/// passes to request in `MrqSubmitRequest::passes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPass {
    /// Standard RGB colour pass rendered to PNG.
    Png,
    /// Linear 32-bit EXR depth buffer.
    DepthExr,
    /// Linear 32-bit EXR surface normals.
    NormalExr,
}

impl RenderPass {
    /// File extension used by frames produced by this pass.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::DepthExr | Self::NormalExr => "exr",
        }
    }

    /// Directory component used under the preset path in staging.
    pub fn dir_name(&self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::DepthExr => "depth_exr",
            Self::NormalExr => "normal_exr",
        }
    }

    /// Attempt to parse a directory or extension string into a RenderPass.
    /// Returns `None` for unrecognised strings.
    pub fn from_dir_name(name: &str) -> Option<Self> {
        match name {
            "png" => Some(Self::Png),
            "depth_exr" => Some(Self::DepthExr),
            "normal_exr" => Some(Self::NormalExr),
            _ => None,
        }
    }

    /// Image format label for `ArtifactKind::Image { format }`.
    pub fn artifact_format(&self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::DepthExr | Self::NormalExr => "EXR",
        }
    }
}

/// Runtime state of an MRQ render job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MrqJobState {
    /// Job is queued but not yet started.
    Queued,
    /// Job is actively rendering.
    Running,
    /// All frames rendered successfully.
    Complete,
    /// Job encountered an error and stopped.
    Failed,
}

/// Typed request for `ue5_mrq_submit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MrqSubmitRequest {
    /// Content-browser path to the Level Sequence to render (e.g. `/Game/Sequences/TurntableShot`).
    pub sequence_path: String,
    /// Filesystem output directory where MRQ will write frames.
    pub output_dir: String,
    /// Render passes to produce for each frame.
    pub passes: Vec<RenderPass>,
    /// Optional time-of-day lighting preset name (e.g. `"GoldenHour"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tod_preset: Option<String>,
}

/// Response from `ue5_mrq_submit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MrqSubmitResponse {
    /// Opaque job identifier for polling with `ue5_mrq_status`.
    pub job_id: String,
    /// Estimated total frame count (frames × passes).
    pub estimated_frames: u32,
}

/// Response from `ue5_mrq_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MrqStatusResponse {
    /// Current job state.
    pub state: MrqJobState,
    /// Number of frames completed so far.
    pub frames_done: u32,
    /// Total frames expected (matches `MrqSubmitResponse::estimated_frames`).
    pub frames_total: u32,
}

/// Information about a single Level Sequence found in the project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceInfo {
    /// Display name of the sequence (e.g. `"TurntableShot"`).
    pub name: String,
    /// Full content-browser path (e.g. `/Game/Sequences/TurntableShot`).
    pub path: String,
    /// First frame number of the sequence's play range.
    pub frame_start: i32,
    /// Last frame number (inclusive) of the sequence's play range.
    pub frame_end: i32,
}

/// Response from `ue5_sequencer_query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencerQueryResponse {
    pub sequences: Vec<SequenceInfo>,
}

/// One available lighting preset in the level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightingPreset {
    /// Preset identifier (e.g. `"GoldenHour"`, `"Overcast"`, `"StudioHDRI"`).
    pub name: String,
    /// Preset category, e.g. `"time_of_day"`, `"hdri"`, `"static"`.
    #[serde(rename = "type")]
    pub preset_type: String,
}

/// Response from `ue5_lighting_preset_list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightingPresetListResponse {
    pub presets: Vec<LightingPreset>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_pass_extension() {
        assert_eq!(RenderPass::Png.extension(), "png");
        assert_eq!(RenderPass::DepthExr.extension(), "exr");
        assert_eq!(RenderPass::NormalExr.extension(), "exr");
    }

    #[test]
    fn render_pass_dir_names() {
        assert_eq!(RenderPass::Png.dir_name(), "png");
        assert_eq!(RenderPass::DepthExr.dir_name(), "depth_exr");
        assert_eq!(RenderPass::NormalExr.dir_name(), "normal_exr");
    }

    #[test]
    fn render_pass_from_dir_name_roundtrip() {
        for pass in [RenderPass::Png, RenderPass::DepthExr, RenderPass::NormalExr] {
            let parsed = RenderPass::from_dir_name(pass.dir_name());
            assert_eq!(parsed, Some(pass));
        }
    }

    #[test]
    fn render_pass_from_dir_name_unknown() {
        assert!(RenderPass::from_dir_name("beauty").is_none());
        assert!(RenderPass::from_dir_name("").is_none());
    }

    #[test]
    fn render_pass_artifact_formats() {
        assert_eq!(RenderPass::Png.artifact_format(), "PNG");
        assert_eq!(RenderPass::DepthExr.artifact_format(), "EXR");
        assert_eq!(RenderPass::NormalExr.artifact_format(), "EXR");
    }

    #[test]
    fn mrq_submit_request_serializes() {
        let req = MrqSubmitRequest {
            sequence_path: "/Game/Sequences/Shot".to_string(),
            output_dir: "/tmp/render_out".to_string(),
            passes: vec![RenderPass::Png, RenderPass::DepthExr],
            tod_preset: Some("GoldenHour".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("GoldenHour"));
        assert!(json.contains("depth_exr"));
    }

    #[test]
    fn mrq_submit_request_omits_none_tod() {
        let req = MrqSubmitRequest {
            sequence_path: "/Game/Shot".to_string(),
            output_dir: "/tmp/out".to_string(),
            passes: vec![RenderPass::Png],
            tod_preset: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tod_preset"));
    }

    #[test]
    fn mrq_job_state_roundtrip() {
        for state in [
            MrqJobState::Queued,
            MrqJobState::Running,
            MrqJobState::Complete,
            MrqJobState::Failed,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: MrqJobState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn sequencer_query_response_roundtrip() {
        let resp = SequencerQueryResponse {
            sequences: vec![SequenceInfo {
                name: "TurntableShot".to_string(),
                path: "/Game/Sequences/TurntableShot".to_string(),
                frame_start: 0,
                frame_end: 239,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: SequencerQueryResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sequences.len(), 1);
        assert_eq!(back.sequences[0].frame_end, 239);
    }

    #[test]
    fn lighting_preset_list_roundtrip() {
        let resp = LightingPresetListResponse {
            presets: vec![
                LightingPreset {
                    name: "GoldenHour".to_string(),
                    preset_type: "time_of_day".to_string(),
                },
                LightingPreset {
                    name: "StudioHDRI".to_string(),
                    preset_type: "hdri".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: LightingPresetListResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.presets.len(), 2);
        assert_eq!(back.presets[0].preset_type, "time_of_day");
    }
}
