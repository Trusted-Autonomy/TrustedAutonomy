// asset_diff.rs — Agent-run contextual diff for image and video artifacts (v0.15.4).
//
// Provides `run_asset_diff`, which spawns a DiffSummaryAgent and an optional
// SupervisorAgent to describe what changed between before/after asset files,
// and cross-checks whether the change aligns with the stated goal intent.
//
// The implementation follows the same spawn_with_timeout / extract_claude_stream_json_text
// patterns used in `supervisor_review.rs`. No new Cargo dependencies are added.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifact_kind::ArtifactKind;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How the image/video changed between before and after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// Change is spatially confined to a region of the image/frame.
    Localized,
    /// Color tone, brightness, or contrast shifted globally.
    Tonal,
    /// Major structural rearrangement of scene elements.
    Structural,
    /// Small, difficult-to-notice change.
    Minor,
    /// Files are visually identical.
    Identical,
}

impl std::fmt::Display for ChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ChangeType::Localized => "localized",
            ChangeType::Tonal => "tonal",
            ChangeType::Structural => "structural",
            ChangeType::Minor => "minor",
            ChangeType::Identical => "identical",
        };
        write!(f, "{}", s)
    }
}

/// Text description of what changed between before and after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDiffSummary {
    /// Human-readable description of what visually changed.
    pub text: String,
    /// Categorisation of the type of change.
    pub change_type: ChangeType,
}

/// Supervisor assessment of whether the diff matches the goal intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSupervisorVerdict {
    /// Alignment confidence: 0.0 (unrelated) – 1.0 (perfect match).
    pub confidence: f32,
    /// One-sentence assessment.
    pub match_assessment: String,
    /// Specific concerns or flags. Empty when confident.
    pub flags: Vec<String>,
}

/// Configuration for asset diff behaviour (from `[draft.asset_diff]` in workflow.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDiffConfig {
    /// Whether to run agent diff summaries at all.
    pub enabled: bool,
    /// Whether to also write a visual diff file alongside the review.
    pub visual_diff: bool,
    /// Threshold (0–1) for classifying localized vs. global change when rendering.
    pub visual_diff_threshold: f32,
    /// Whether to run the supervisor confidence check.
    pub supervisor: bool,
    /// Which agent binary to use. "builtin"/"claude-code" → `claude` CLI, others by name.
    pub agent: String,
    /// Timeout for each agent call in seconds.
    pub timeout_secs: u64,
}

impl Default for AssetDiffConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            visual_diff: false,
            visual_diff_threshold: 0.3,
            supervisor: true,
            agent: "builtin".to_string(),
            timeout_secs: 60,
        }
    }
}

/// Path and type of a visual diff output file.
#[derive(Debug, Clone)]
pub struct VisualDiffOutput {
    /// Path to the written diff file.
    pub diff_path: PathBuf,
    /// Category of visual diff that was rendered.
    pub diff_type: VisualDiffType,
}

/// Category of visual diff file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualDiffType {
    /// Side-by-side crop comparison for `ChangeType::Localized`.
    CropComparison,
    /// Horizontal color bar for `ChangeType::Tonal`.
    ColorBar,
    /// Video keyframe summary for `ArtifactKind::Video`.
    KeyframeSummary,
}

/// Combined result of running all configured diff stages for a single artifact.
#[derive(Debug)]
pub struct AssetDiffResult {
    /// Text summary from the DiffSummaryAgent (None when skipped).
    pub summary: Option<AssetDiffSummary>,
    /// Supervisor confidence verdict (None when skipped).
    pub supervisor: Option<AssetSupervisorVerdict>,
    /// Visual diff output file (None when `visual_diff = false`).
    pub visual_diff: Option<VisualDiffOutput>,
    /// If set, the diff was skipped and this describes why.
    pub skipped_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Raw LLM response types (internal)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct RawDiffResponse {
    text: Option<String>,
    change_type: Option<String>,
}

#[derive(Deserialize, Debug)]
struct RawSupervisorResponse {
    confidence: Option<f64>,
    match_assessment: Option<String>,
    flags: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the full asset diff pipeline for a single before/after artifact pair.
///
/// The function is infallible — on any error it returns a result with
/// `skipped_reason` set and all optional fields as `None`.
pub fn run_asset_diff(
    before_path: &Path,
    after_path: &Path,
    kind: &ArtifactKind,
    goal_intent: &str,
    config: &AssetDiffConfig,
    staging_dir: &Path,
) -> AssetDiffResult {
    if !config.enabled {
        return AssetDiffResult {
            summary: None,
            supervisor: None,
            visual_diff: None,
            skipped_reason: Some("asset diff disabled in config".to_string()),
        };
    }

    // -- Stage 1: DiffSummaryAgent --
    let summary_result = run_diff_summary_agent(before_path, after_path, kind, config);
    let summary = match summary_result {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "DiffSummaryAgent failed — skipping asset diff");
            return AssetDiffResult {
                summary: None,
                supervisor: None,
                visual_diff: None,
                skipped_reason: Some(format!("diff summary agent failed: {}", e)),
            };
        }
    };

    // -- Stage 2: SupervisorAgent (optional) --
    let supervisor_verdict = if config.supervisor {
        match run_supervisor_agent(goal_intent, &summary, config) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, "AssetSupervisorAgent failed — skipping supervisor verdict");
                None
            }
        }
    } else {
        None
    };

    // -- Stage 3: VisualDiffRenderer (optional) --
    let visual_diff_output = if config.visual_diff {
        match VisualDiffRenderer::render(
            before_path,
            after_path,
            &summary.change_type,
            kind,
            staging_dir,
        ) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, "VisualDiffRenderer failed");
                None
            }
        }
    } else {
        None
    };

    AssetDiffResult {
        summary: Some(summary),
        supervisor: supervisor_verdict,
        visual_diff: visual_diff_output,
        skipped_reason: None,
    }
}

// ---------------------------------------------------------------------------
// DiffSummaryAgent
// ---------------------------------------------------------------------------

fn build_diff_summary_prompt(before_path: &Path, after_path: &Path, kind: &ArtifactKind) -> String {
    let kind_label = kind.display_label();
    format!(
        r#"You are reviewing a visual asset change. Describe ONLY what you observe visually — do not speculate about intent.

Artifact: {kind_label} at {after_path}
Before: {before}
After: {after}

Respond with JSON:
{{
  "text": "one or two sentence description of what visually changed",
  "change_type": "localized|tonal|structural|minor|identical"
}}

Use:
- "localized": change is confined to a region of the image/frame
- "tonal": color, brightness, or contrast shifted globally
- "structural": major rearrangement of scene elements
- "minor": small, difficult-to-notice change
- "identical": files appear visually identical

Respond with ONLY the JSON object."#,
        kind_label = kind_label,
        after_path = after_path.display(),
        before = before_path.display(),
        after = after_path.display(),
    )
}

fn run_diff_summary_agent(
    before_path: &Path,
    after_path: &Path,
    kind: &ArtifactKind,
    config: &AssetDiffConfig,
) -> anyhow::Result<AssetDiffSummary> {
    let prompt = build_diff_summary_prompt(before_path, after_path, kind);
    let stdout = invoke_agent_cli(&prompt, config)?;
    let text = extract_claude_stream_json_text(&stdout);
    Ok(parse_diff_summary_from_json(&text))
}

/// Parse a DiffSummaryAgent JSON response into an `AssetDiffSummary`.
///
/// Falls back gracefully: unknown `change_type` values become `Minor`,
/// missing `text` becomes a generic placeholder.
pub fn build_diff_summary_from_json(json_str: &str) -> AssetDiffSummary {
    parse_diff_summary_from_json(json_str)
}

fn parse_diff_summary_from_json(text: &str) -> AssetDiffSummary {
    let json_str = extract_json(text);
    if let Ok(raw) = serde_json::from_str::<RawDiffResponse>(json_str) {
        let change_type = match raw.change_type.as_deref() {
            Some("localized") => ChangeType::Localized,
            Some("tonal") => ChangeType::Tonal,
            Some("structural") => ChangeType::Structural,
            Some("identical") => ChangeType::Identical,
            _ => ChangeType::Minor,
        };
        let summary_text = raw
            .text
            .unwrap_or_else(|| "Agent did not describe the change.".to_string());
        return AssetDiffSummary {
            text: summary_text,
            change_type,
        };
    }
    // Non-JSON or parse failure — wrap as minor change with the raw text.
    let summary_text = if text.trim().is_empty() {
        "Agent returned empty response.".to_string()
    } else if text.len() > 300 {
        format!("{}…", &text[..300])
    } else {
        text.trim().to_string()
    };
    AssetDiffSummary {
        text: summary_text,
        change_type: ChangeType::Minor,
    }
}

// ---------------------------------------------------------------------------
// SupervisorAgent
// ---------------------------------------------------------------------------

fn build_supervisor_prompt(goal_intent: &str, diff_summary: &AssetDiffSummary) -> String {
    format!(
        r#"Goal intent: {goal_intent}

Asset diff summary: {summary}
Change type: {change_type}

Cross-check whether this diff is consistent with the stated goal intent.

Respond with JSON:
{{
  "confidence": 0.0-1.0,
  "match_assessment": "one sentence assessment",
  "flags": ["concern 1", ...]
}}

Use:
- confidence 1.0: diff is fully consistent with the goal intent
- confidence 0.7-0.99: mostly consistent with minor uncertainty
- confidence below 0.7: significant mismatch or concern

Respond with ONLY the JSON object."#,
        goal_intent = goal_intent,
        summary = diff_summary.text,
        change_type = diff_summary.change_type,
    )
}

fn run_supervisor_agent(
    goal_intent: &str,
    diff_summary: &AssetDiffSummary,
    config: &AssetDiffConfig,
) -> anyhow::Result<AssetSupervisorVerdict> {
    let prompt = build_supervisor_prompt(goal_intent, diff_summary);
    let stdout = invoke_agent_cli(&prompt, config)?;
    let text = extract_claude_stream_json_text(&stdout);
    Ok(parse_supervisor_verdict_from_json(&text))
}

/// Parse a supervisor JSON response into an `AssetSupervisorVerdict`.
pub fn parse_supervisor_verdict_from_json(text: &str) -> AssetSupervisorVerdict {
    let json_str = extract_json(text);
    if let Ok(raw) = serde_json::from_str::<RawSupervisorResponse>(json_str) {
        let confidence = raw
            .confidence
            .map(|v| v.clamp(0.0, 1.0) as f32)
            .unwrap_or(0.5);
        let match_assessment = raw
            .match_assessment
            .unwrap_or_else(|| "No assessment provided.".to_string());
        let flags = raw.flags.unwrap_or_default();
        return AssetSupervisorVerdict {
            confidence,
            match_assessment,
            flags,
        };
    }
    // Fallback: unknown response.
    AssetSupervisorVerdict {
        confidence: 0.5,
        match_assessment: "Could not parse supervisor response.".to_string(),
        flags: vec![],
    }
}

// ---------------------------------------------------------------------------
// VisualDiffRenderer
// ---------------------------------------------------------------------------

pub struct VisualDiffRenderer;

impl VisualDiffRenderer {
    /// Render a visual diff placeholder file in `staging_dir/diffs/`.
    ///
    /// This writes a text placeholder since image processing requires external
    /// dependencies not present in the workspace. The file path is returned so
    /// `ta draft view` can display it for the reviewer.
    pub fn render(
        before_path: &Path,
        after_path: &Path,
        change_type: &ChangeType,
        kind: &ArtifactKind,
        staging_dir: &Path,
    ) -> anyhow::Result<VisualDiffOutput> {
        let diffs_dir = staging_dir.join("diffs");
        std::fs::create_dir_all(&diffs_dir)?;

        let stem = after_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("asset");

        let (diff_type, suffix) = if kind.is_video() {
            (VisualDiffType::KeyframeSummary, "_keyframes.txt")
        } else {
            match change_type {
                ChangeType::Localized => (VisualDiffType::CropComparison, "_crop.txt"),
                ChangeType::Tonal => (VisualDiffType::ColorBar, "_colordiff.txt"),
                _ => (VisualDiffType::CropComparison, "_diff.txt"),
            }
        };

        let diff_filename = format!("{}{}", stem, suffix);
        let diff_path = diffs_dir.join(&diff_filename);

        let content = format!(
            "Visual diff placeholder (v0.15.4)\n\
             Before: {}\n\
             After:  {}\n\
             Type:   {:?}\n\
             \n\
             Note: Full image/video diff rendering requires a vision-capable viewer.\n\
             Review the before/after files directly to verify the change.\n",
            before_path.display(),
            after_path.display(),
            diff_type,
        );

        std::fs::write(&diff_path, content)?;

        Ok(VisualDiffOutput {
            diff_path,
            diff_type,
        })
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (follow supervisor_review.rs patterns)
// ---------------------------------------------------------------------------

/// Invoke the configured agent CLI and return its stdout.
fn invoke_agent_cli(prompt: &str, config: &AssetDiffConfig) -> anyhow::Result<String> {
    let binary = match config.agent.as_str() {
        "builtin" | "claude-code" => "claude",
        other => other,
    };
    spawn_with_timeout(
        binary,
        &[
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            prompt,
        ],
        config.timeout_secs,
        &format!("{} CLI", config.agent),
    )
}

/// Spawn a process, collect stdout, kill it if it exceeds the timeout.
/// Mirrors the function in `supervisor_review.rs`.
fn spawn_with_timeout(
    program: &str,
    args: &[&str],
    timeout_secs: u64,
    label: &str,
) -> anyhow::Result<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to spawn '{}': {} — is {} installed and on PATH?",
                program,
                e,
                label
            )
        })?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if !status.success() && stdout.trim().is_empty() {
                    let mut stderr = String::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_string(&mut stderr);
                    }
                    anyhow::bail!(
                        "{} exited with status {}: {}",
                        label,
                        status,
                        &stderr[..stderr.len().min(200)]
                    );
                }
                return Ok(stdout);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    anyhow::bail!(
                        "{} timed out after {}s — increase [draft.asset_diff] timeout_secs in workflow.toml",
                        label,
                        timeout_secs
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                anyhow::bail!("Error waiting for {}: {}", label, e);
            }
        }
    }
}

/// Extract the final text content from Claude CLI's stream-json output.
/// Mirrors `extract_claude_stream_json_text` in `supervisor_review.rs`.
fn extract_claude_stream_json_text(stdout: &str) -> String {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(|t| t.as_str()) == Some("result") {
            if let Some(text) = val.get("result").and_then(|r| r.as_str()) {
                if !text.trim().is_empty() {
                    return text.to_string();
                }
            }
            if let Some(content) = val.get("content") {
                let text = extract_content_text(content);
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_content_text(content);
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    stdout.to_string()
}

fn extract_content_text(content: &serde_json::Value) -> String {
    if let Some(arr) = content.as_array() {
        arr.iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("")
    } else {
        content.as_str().unwrap_or("").to_string()
    }
}

/// Extract a JSON object from text that might be wrapped in markdown fences or prose.
/// Mirrors `extract_json` in `supervisor_review.rs`.
fn extract_json(text: &str) -> &str {
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Find the first { and the last } to extract a raw JSON object.
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            return &text[start..=end];
        }
    }
    text.trim()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // Test 1: DiffSummaryAgent JSON parsing — tonal change.
    #[test]
    fn diff_summary_from_json_tonal() {
        let json = r#"{"text": "Lighting shifted from warm to cool.", "change_type": "tonal"}"#;
        let summary = build_diff_summary_from_json(json);
        assert_eq!(summary.change_type, ChangeType::Tonal);
        assert!(summary.text.contains("warm"));
    }

    // Test 1b: DiffSummaryAgent JSON parsing — localized change.
    #[test]
    fn diff_summary_from_json_localized() {
        let json = r#"{"text": "Shadow deepened in the left corner.", "change_type": "localized"}"#;
        let summary = build_diff_summary_from_json(json);
        assert_eq!(summary.change_type, ChangeType::Localized);
        assert!(summary.text.contains("corner"));
    }

    // Test 1c: Unknown change_type falls back to Minor.
    #[test]
    fn diff_summary_unknown_change_type_falls_back_to_minor() {
        let json = r#"{"text": "Something changed.", "change_type": "cosmic"}"#;
        let summary = build_diff_summary_from_json(json);
        assert_eq!(summary.change_type, ChangeType::Minor);
    }

    // Test 1d: Non-JSON response is wrapped gracefully.
    #[test]
    fn diff_summary_non_json_fallback() {
        let text = "The image changed.";
        let summary = build_diff_summary_from_json(text);
        assert_eq!(summary.change_type, ChangeType::Minor);
        assert!(!summary.text.is_empty());
    }

    // Test 2: Supervisor scores high for matching intent/summary.
    #[test]
    fn supervisor_high_confidence_for_match() {
        let json = r#"{
            "confidence": 0.95,
            "match_assessment": "Consistent with goal to adjust day/night lighting.",
            "flags": []
        }"#;
        let verdict = parse_supervisor_verdict_from_json(json);
        assert!(
            verdict.confidence > 0.7,
            "confidence: {}",
            verdict.confidence
        );
        assert!(verdict.flags.is_empty());
    }

    // Test 3: Supervisor scores low for mismatch.
    #[test]
    fn supervisor_low_confidence_for_mismatch() {
        let json = r#"{
            "confidence": 0.42,
            "match_assessment": "The change affects character position, not lighting.",
            "flags": ["Character moved left — goal only mentioned color adjustment."]
        }"#;
        let verdict = parse_supervisor_verdict_from_json(json);
        assert!(
            verdict.confidence < 0.7,
            "confidence: {}",
            verdict.confidence
        );
        assert!(!verdict.flags.is_empty());
    }

    // Test 3b: Supervisor confidence is clamped to [0, 1].
    #[test]
    fn supervisor_confidence_clamped() {
        let json = r#"{"confidence": 1.5, "match_assessment": "ok", "flags": []}"#;
        let verdict = parse_supervisor_verdict_from_json(json);
        assert!(verdict.confidence <= 1.0);

        let json2 = r#"{"confidence": -0.5, "match_assessment": "bad", "flags": []}"#;
        let verdict2 = parse_supervisor_verdict_from_json(json2);
        assert!(verdict2.confidence >= 0.0);
    }

    // Test 4: VisualDiffRenderer produces expected output path.
    #[test]
    fn visual_diff_renderer_produces_path() {
        let dir = tempdir().unwrap();
        let before = dir.path().join("frame_before.png");
        let after = dir.path().join("frame_after.png");
        std::fs::write(&before, b"before").unwrap();
        std::fs::write(&after, b"after").unwrap();

        let kind = ArtifactKind::Image {
            width: Some(1024),
            height: Some(1024),
            format: Some("PNG".to_string()),
            frame_index: None,
        };

        let result =
            VisualDiffRenderer::render(&before, &after, &ChangeType::Tonal, &kind, dir.path())
                .unwrap();

        assert!(
            result.diff_path.to_str().unwrap().contains("_colordiff"),
            "expected colordiff suffix, got: {}",
            result.diff_path.display()
        );
        assert_eq!(result.diff_type, VisualDiffType::ColorBar);
        assert!(result.diff_path.exists(), "diff file should be written");
    }

    // Test 4b: VisualDiffRenderer produces crop comparison for Localized change.
    #[test]
    fn visual_diff_renderer_localized_produces_crop() {
        let dir = tempdir().unwrap();
        let before = dir.path().join("img.png");
        let after = dir.path().join("img.png");
        std::fs::write(&before, b"x").unwrap();

        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };

        let result =
            VisualDiffRenderer::render(&before, &after, &ChangeType::Localized, &kind, dir.path())
                .unwrap();

        assert_eq!(result.diff_type, VisualDiffType::CropComparison);
        assert!(
            result.diff_path.to_str().unwrap().contains("_crop"),
            "got: {}",
            result.diff_path.display()
        );
    }

    // Test 4c: VisualDiffRenderer uses KeyframeSummary for video.
    #[test]
    fn visual_diff_renderer_video_keyframe() {
        let dir = tempdir().unwrap();
        let before = dir.path().join("clip.mp4");
        let after = dir.path().join("clip.mp4");
        std::fs::write(&before, b"x").unwrap();

        let kind = ArtifactKind::Video {
            width: None,
            height: None,
            fps: None,
            duration_secs: None,
            format: Some("MP4".to_string()),
            frame_count: None,
        };

        let result =
            VisualDiffRenderer::render(&before, &after, &ChangeType::Structural, &kind, dir.path())
                .unwrap();

        assert_eq!(result.diff_type, VisualDiffType::KeyframeSummary);
        assert!(
            result.diff_path.to_str().unwrap().contains("_keyframes"),
            "got: {}",
            result.diff_path.display()
        );
    }

    // Test 5: Config parsing defaults.
    #[test]
    fn asset_diff_config_defaults() {
        let cfg = AssetDiffConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.visual_diff);
        assert!((cfg.visual_diff_threshold - 0.3).abs() < f32::EPSILON);
        assert!(cfg.supervisor);
        assert_eq!(cfg.agent, "builtin");
        assert_eq!(cfg.timeout_secs, 60);
    }

    // Test 5b: Config serializes and deserializes with serde_json.
    #[test]
    fn asset_diff_config_serde_roundtrip() {
        let cfg = AssetDiffConfig {
            enabled: false,
            visual_diff: true,
            visual_diff_threshold: 0.5,
            supervisor: false,
            agent: "codex".to_string(),
            timeout_secs: 120,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AssetDiffConfig = serde_json::from_str(&json).unwrap();
        assert!(!back.enabled);
        assert!(back.visual_diff);
        assert!(!back.supervisor);
        assert_eq!(back.agent, "codex");
        assert_eq!(back.timeout_secs, 120);
    }

    // Test 6: run_asset_diff returns skipped_reason when disabled.
    #[test]
    fn run_asset_diff_skipped_when_disabled() {
        let dir = tempdir().unwrap();
        let before = dir.path().join("img.png");
        let after = dir.path().join("img.png");
        std::fs::write(&before, b"x").unwrap();

        let kind = ArtifactKind::Image {
            width: None,
            height: None,
            format: None,
            frame_index: None,
        };
        let cfg = AssetDiffConfig {
            enabled: false,
            ..AssetDiffConfig::default()
        };

        let result = run_asset_diff(&before, &after, &kind, "adjust lighting", &cfg, dir.path());

        assert!(result.summary.is_none());
        assert!(result.supervisor.is_none());
        assert!(result.visual_diff.is_none());
        assert_eq!(
            result.skipped_reason.as_deref(),
            Some("asset diff disabled in config")
        );
    }

    // Test 6b: JSON with markdown fences is extracted correctly.
    #[test]
    fn extract_json_from_markdown_fence() {
        let text = "Sure, here is the result:\n```json\n{\"text\": \"changed\", \"change_type\": \"tonal\"}\n```";
        let summary = build_diff_summary_from_json(text);
        assert_eq!(summary.change_type, ChangeType::Tonal);
        assert_eq!(summary.text, "changed");
    }
}
