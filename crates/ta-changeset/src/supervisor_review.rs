// supervisor_review.rs — AI-powered supervisor that reviews staged changes against goal alignment and constitution.

use std::io::Read;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Verdict from the supervisor agent review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupervisorVerdict {
    /// Changes are aligned with the goal and constitution.
    Pass,
    /// Minor concerns but not blocking — shown in draft view with yellow.
    Warn,
    /// Significant alignment or constitution violation — can block approval.
    Block,
}

impl std::fmt::Display for SupervisorVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "pass"),
            Self::Warn => write!(f, "warn"),
            Self::Block => write!(f, "block"),
        }
    }
}

#[allow(clippy::derivable_impls)]
impl Default for SupervisorVerdict {
    fn default() -> Self {
        Self::Warn
    }
}

/// The result of an AI supervisor reviewing staged changes.
/// Embedded in `DraftPackage.supervisor_review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorReview {
    /// Overall verdict: pass, warn, or block.
    pub verdict: SupervisorVerdict,
    /// Whether changes stayed within the goal's declared scope.
    pub scope_ok: bool,
    /// Specific findings from the review (concerns, observations).
    pub findings: Vec<String>,
    /// One-sentence summary of the review.
    pub summary: String,
    /// Which supervisor produced this review ("builtin", "claude-code", "codex", etc.).
    pub agent: String,
    /// How long the supervisor took in seconds.
    pub duration_secs: f32,
}

/// Configuration for the supervisor passed at runtime (derived from WorkflowConfig).
#[derive(Debug, Clone)]
pub struct SupervisorRunConfig {
    /// Enabled flag.
    pub enabled: bool,
    /// Agent name: "builtin" | "claude-code" | "codex" | "ollama" | manifest name.
    pub agent: String,
    /// What to do when verdict is Block: "warn" (just show) or "block" (refuse approve).
    pub verdict_on_block: String,
    /// Path to project constitution file.
    pub constitution_path: Option<std::path::PathBuf>,
    /// Don't fail if constitution is missing.
    pub skip_if_no_constitution: bool,
    /// Timeout in seconds (default 120).
    pub timeout_secs: u64,
    /// Optional env var name to check before spawning the agent (pre-flight UX check).
    /// When set, TA verifies the var exists and prints an actionable message if missing.
    /// The agent binary reads the var itself — TA never passes the value.
    pub api_key_env: Option<String>,
    /// Staging directory path (required for manifest-based custom agents).
    pub staging_path: Option<std::path::PathBuf>,
}

/// Raw LLM response structure (expected JSON from the supervisor prompt).
#[derive(Deserialize, Debug)]
struct LlmSupervisorResponse {
    verdict: Option<String>,
    scope_ok: Option<bool>,
    findings: Option<Vec<String>>,
    summary: Option<String>,
}

/// Unified supervisor agent dispatcher.
///
/// Dispatches on `config.agent`:
/// - `"builtin"` | `"claude-code"` → spawn `claude --print --output-format stream-json`
///   (delegates auth entirely to the `claude` binary — supports subscription OAuth, API key, etc.)
/// - `"codex"` → spawn `codex --approval-mode full-auto --quiet`
/// - `"ollama"` → spawn `ta agent run ollama --headless`
/// - any other string → look up `.ta/agents/<name>.toml` manifest in `config.staging_path`
///
/// Falls back to `SupervisorVerdict::Warn` on any failure — never blocks a draft build.
pub fn invoke_supervisor_agent(
    objective: &str,
    changed_files: &[String],
    constitution_text: Option<&str>,
    config: &SupervisorRunConfig,
) -> SupervisorReview {
    let started = Instant::now();
    let prompt = build_supervisor_prompt(objective, changed_files, constitution_text);

    // Pre-flight check: verify api_key_env exists before spawning agent.
    if let Some(ref env_var) = config.api_key_env {
        if std::env::var(env_var).is_err() {
            let msg = format!(
                "Supervisor agent '{}' requires {} — set it or change [supervisor] agent in workflow.toml.",
                config.agent, env_var
            );
            tracing::warn!("{}", msg);
            return fallback_supervisor_review(&config.agent, &msg, 0.0);
        }
    }

    let result = match config.agent.as_str() {
        "builtin" | "claude-code" => invoke_claude_cli_supervisor(&prompt, config),
        "codex" => invoke_codex_supervisor(&prompt, config),
        "ollama" => invoke_ollama_supervisor(&prompt, config),
        other => {
            if let Some(ref staging) = config.staging_path {
                run_manifest_supervisor(staging, other, objective, changed_files, config, started)
            } else {
                Err(anyhow::anyhow!(
                    "Custom agent '{}' requires staging_path to be set in SupervisorRunConfig",
                    other
                ))
            }
        }
    };

    let duration_secs = started.elapsed().as_secs_f32();

    match result {
        Ok(mut review) => {
            review.duration_secs = duration_secs;
            review
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                agent = %config.agent,
                "Supervisor agent failed — falling back to warn verdict"
            );
            fallback_supervisor_review(&config.agent, &e.to_string(), duration_secs)
        }
    }
}

/// Invoke the `claude` CLI in headless stream-json mode.
///
/// Uses `claude --print --output-format stream-json <prompt>`. Auth is handled entirely
/// by the `claude` binary (subscription OAuth, API key from env or config).
fn invoke_claude_cli_supervisor(
    prompt: &str,
    config: &SupervisorRunConfig,
) -> anyhow::Result<SupervisorReview> {
    let stdout = spawn_with_timeout(
        "claude",
        &["--print", "--output-format", "stream-json", prompt],
        config.timeout_secs,
        "Claude Code CLI",
    )?;

    let text = extract_claude_stream_json_text(&stdout);
    let review = parse_supervisor_response_or_text(&text, "claude-code");
    Ok(review)
}

/// Invoke the `codex` CLI in headless mode.
///
/// Uses `codex --approval-mode full-auto --quiet <prompt>`.
/// Codex outputs plain text; we wrap it as summary and attempt JSON extraction.
fn invoke_codex_supervisor(
    prompt: &str,
    config: &SupervisorRunConfig,
) -> anyhow::Result<SupervisorReview> {
    let stdout = spawn_with_timeout(
        "codex",
        &["--approval-mode", "full-auto", "--quiet", prompt],
        config.timeout_secs,
        "Codex CLI",
    )?;

    let review = parse_supervisor_response_or_text(&stdout, "codex");
    Ok(review)
}

/// Invoke the ollama agent via `ta agent run ollama --headless`.
fn invoke_ollama_supervisor(
    prompt: &str,
    config: &SupervisorRunConfig,
) -> anyhow::Result<SupervisorReview> {
    let stdout = spawn_with_timeout(
        "ta",
        &["agent", "run", "ollama", "--headless", "--prompt", prompt],
        config.timeout_secs,
        "ta-agent-ollama",
    )?;

    let review = parse_supervisor_response_or_text(&stdout, "ollama");
    Ok(review)
}

/// Spawn a process and collect its stdout, killing it if it exceeds the timeout.
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
                        "{} timed out after {}s — increase [supervisor] timeout_secs in workflow.toml",
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
///
/// Claude CLI with `--output-format stream-json` emits newline-delimited JSON events.
/// We look for the `result` event (type = "result") and extract its text.
/// Falls back to scanning for `assistant` message content.
fn extract_claude_stream_json_text(stdout: &str) -> String {
    // Scan in reverse for the last result event.
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(|t| t.as_str()) == Some("result") {
            // `result` field contains the final text in most CLI versions.
            if let Some(text) = val.get("result").and_then(|r| r.as_str()) {
                if !text.trim().is_empty() {
                    return text.to_string();
                }
            }
            // Some versions embed content in a `content` array.
            if let Some(content) = val.get("content") {
                let text = extract_content_text(content);
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }

    // Fallback: pick the last non-empty assistant text block.
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

    // Last resort: return raw stdout (may contain JSON on one line).
    stdout.to_string()
}

/// Extract plain text from a JSON content value (array of blocks or string).
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

/// Parse supervisor response text, falling back to a warn verdict with the text as summary.
///
/// Tries structured JSON parsing first (with `extract_json`). If that fails, wraps
/// the full text as `summary` with `verdict: warn` — so plain-text responses from
/// agents that don't follow the JSON format still produce a useful review.
fn parse_supervisor_response_or_text(text: &str, agent: &str) -> SupervisorReview {
    if let Ok(review) = parse_supervisor_response(text) {
        return SupervisorReview {
            agent: agent.to_string(),
            ..review
        };
    }
    // Non-JSON response: treat full text as summary with warn.
    let summary = if text.len() > 300 {
        format!("{}…", &text[..300])
    } else if text.trim().is_empty() {
        format!("Supervisor agent '{}' returned empty response.", agent)
    } else {
        text.trim().to_string()
    };
    SupervisorReview {
        verdict: SupervisorVerdict::Warn,
        scope_ok: true,
        findings: vec![],
        summary,
        agent: agent.to_string(),
        duration_secs: 0.0,
    }
}

/// Run a manifest-based custom supervisor agent.
///
/// Reads `.ta/agents/<name>.toml`, writes `.ta/supervisor_input.json`, spawns the
/// command, waits for `.ta/supervisor_result.json` to be written by the agent,
/// and parses the result. Falls back to warn on any failure.
fn run_manifest_supervisor(
    staging_path: &Path,
    agent_name: &str,
    objective: &str,
    changed_files: &[String],
    config: &SupervisorRunConfig,
    started: Instant,
) -> anyhow::Result<SupervisorReview> {
    // Write input context for the custom agent.
    let input = serde_json::json!({
        "objective": objective,
        "changed_files": changed_files,
    });
    let input_path = staging_path.join(".ta/supervisor_input.json");
    if let Err(e) = std::fs::write(
        &input_path,
        serde_json::to_string_pretty(&input).unwrap_or_default(),
    ) {
        tracing::warn!(error = %e, "Failed to write supervisor input file");
    }

    // Look up agent manifest.
    let agent_manifest = staging_path
        .join(".ta/agents")
        .join(format!("{}.toml", agent_name));
    if !agent_manifest.exists() {
        anyhow::bail!(
            "Custom supervisor agent '{}' manifest not found at .ta/agents/{}.toml",
            agent_name,
            agent_name
        );
    }

    // Clear any stale result file.
    let result_path = staging_path.join(".ta/supervisor_result.json");
    let _ = std::fs::remove_file(&result_path);

    // Read command from agent manifest.
    let manifest_content = std::fs::read_to_string(&agent_manifest)
        .map_err(|e| anyhow::anyhow!("Failed to read agent manifest: {}", e))?;
    let manifest: toml::Value = toml::from_str(&manifest_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse agent manifest: {}", e))?;
    let cmd_str = manifest
        .get("agent")
        .and_then(|a| a.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if cmd_str.is_empty() {
        anyhow::bail!(
            "Agent manifest '{}' missing [agent] command field",
            agent_name
        );
    }

    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let mut child = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .current_dir(staging_path)
        .env("TA_SUPERVISOR_INPUT", input_path.to_str().unwrap_or(""))
        .env("TA_SUPERVISOR_OUTPUT", result_path.to_str().unwrap_or(""))
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn custom agent '{}': {}", agent_name, e))?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    anyhow::bail!(
                        "Custom agent '{}' timed out after {}s",
                        agent_name,
                        config.timeout_secs
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                anyhow::bail!("Error waiting for custom agent '{}': {}", agent_name, e);
            }
        }
    }

    let content = std::fs::read_to_string(&result_path).map_err(|e| {
        anyhow::anyhow!(
            "Custom agent '{}' did not write result file (.ta/supervisor_result.json): {}",
            agent_name,
            e
        )
    })?;

    let mut review: SupervisorReview = serde_json::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse result JSON from custom agent '{}': {}",
            agent_name,
            e
        )
    })?;
    review.agent = agent_name.to_string();
    review.duration_secs = started.elapsed().as_secs_f32();
    Ok(review)
}

/// Create a fallback `SupervisorReview` with `Warn` verdict for any failure path.
pub fn fallback_supervisor_review(
    agent: &str,
    reason: &str,
    duration_secs: f32,
) -> SupervisorReview {
    SupervisorReview {
        verdict: SupervisorVerdict::Warn,
        scope_ok: true,
        findings: vec![format!("Supervisor review incomplete: {}", reason)],
        summary: "Supervisor could not complete review (fallback to warn).".to_string(),
        agent: agent.to_string(),
        duration_secs,
    }
}

/// Build the supervisor prompt.
pub fn build_supervisor_prompt(
    objective: &str,
    changed_files: &[String],
    constitution_text: Option<&str>,
) -> String {
    let files_list = if changed_files.is_empty() {
        "  (no files changed)".to_string()
    } else {
        changed_files
            .iter()
            .map(|f| format!("  - {}", f))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let constitution_section = match constitution_text {
        Some(text) if !text.trim().is_empty() => format!(
            "\n\nProject Constitution:\n```\n{}\n```",
            &text[..text.len().min(3000)]
        ),
        _ => "\n\nProject Constitution: (not available — skip constitution check)".to_string(),
    };

    format!(
        r#"You are a supervisor reviewing an AI agent's work for goal alignment and constitution compliance.

Goal Objective:
{objective}

Changed Files:
{files_list}{constitution_section}

Review the changes and answer:
1. Did the agent stay within the goal scope? (Only files directly needed for the objective should be modified)
2. Are any changes surprising, potentially harmful, or out of scope?
3. Does the work appear to satisfy the objective?
4. If a constitution was provided, does the work comply with it?

Respond with ONLY a JSON object (no markdown, no explanation):
{{
  "verdict": "pass" | "warn" | "block",
  "scope_ok": true | false,
  "findings": ["finding 1", "finding 2"],
  "summary": "One sentence summary"
}}

Use:
- "pass": Changes align well with the objective and constitution
- "warn": Minor concerns (e.g., one extra file touched, or minor scope drift)
- "block": Significant concerns (e.g., unrelated system files modified, or clear constitution violation)

Keep findings concise (1-2 sentences each, max 5 findings)."#
    )
}

fn parse_supervisor_response(text: &str) -> anyhow::Result<SupervisorReview> {
    // Try to extract JSON from the text (handle potential markdown wrapping).
    let json_str = extract_json(text);

    let parsed: LlmSupervisorResponse = serde_json::from_str(json_str).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse supervisor JSON: {} — response: {}",
            e,
            &text[..text.len().min(300)]
        )
    })?;

    let verdict = match parsed.verdict.as_deref() {
        Some("pass") => SupervisorVerdict::Pass,
        Some("block") => SupervisorVerdict::Block,
        _ => SupervisorVerdict::Warn, // Default to warn for unknown values
    };

    Ok(SupervisorReview {
        verdict,
        scope_ok: parsed.scope_ok.unwrap_or(true),
        findings: parsed.findings.unwrap_or_default(),
        summary: parsed
            .summary
            .unwrap_or_else(|| "No summary provided.".to_string()),
        agent: "builtin".to_string(),
        duration_secs: 0.0, // Will be overwritten by caller
    })
}

/// Extract a JSON object from text that might contain markdown or prose.
fn extract_json(text: &str) -> &str {
    // Look for ```json ... ``` blocks
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Look for ``` ... ``` blocks
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    // Look for { ... } directly
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return &text[start..=end];
            }
        }
    }
    text.trim()
}

/// Load the constitution text from the configured path or common fallback locations.
pub fn load_constitution(staging_path: &Path, config: &SupervisorRunConfig) -> Option<String> {
    // Try the configured path first.
    if let Some(ref path) = config.constitution_path {
        let full = staging_path.join(path);
        if full.exists() {
            return std::fs::read_to_string(&full).ok();
        }
    }
    // Fallback: .ta/constitution.toml
    let toml_path = staging_path.join(".ta/constitution.toml");
    if toml_path.exists() {
        return std::fs::read_to_string(&toml_path).ok();
    }
    // Fallback: docs/TA-CONSTITUTION.md
    let md_path = staging_path.join("docs/TA-CONSTITUTION.md");
    if md_path.exists() {
        return std::fs::read_to_string(&md_path).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_supervisor_prompt_includes_objective() {
        let prompt = build_supervisor_prompt(
            "Add JWT authentication to the API",
            &["src/auth.rs".to_string(), "src/middleware.rs".to_string()],
            None,
        );
        assert!(prompt.contains("Add JWT authentication to the API"));
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("src/middleware.rs"));
    }

    #[test]
    fn test_build_supervisor_prompt_includes_constitution() {
        let prompt = build_supervisor_prompt(
            "Fix bug in parser",
            &["src/parser.rs".to_string()],
            Some("Never modify production database directly."),
        );
        assert!(prompt.contains("Never modify production database directly."));
    }

    #[test]
    fn test_build_supervisor_prompt_no_constitution() {
        let prompt = build_supervisor_prompt("Fix bug", &["src/foo.rs".to_string()], None);
        assert!(prompt.contains("not available — skip constitution check"));
    }

    #[test]
    fn test_build_supervisor_prompt_empty_files() {
        let prompt = build_supervisor_prompt("Fix bug", &[], None);
        assert!(prompt.contains("no files changed"));
    }

    #[test]
    fn test_supervisor_verdict_display() {
        assert_eq!(SupervisorVerdict::Pass.to_string(), "pass");
        assert_eq!(SupervisorVerdict::Warn.to_string(), "warn");
        assert_eq!(SupervisorVerdict::Block.to_string(), "block");
    }

    #[test]
    fn test_supervisor_verdict_serde() {
        let v: SupervisorVerdict = serde_json::from_str("\"pass\"").unwrap();
        assert_eq!(v, SupervisorVerdict::Pass);
        let v: SupervisorVerdict = serde_json::from_str("\"block\"").unwrap();
        assert_eq!(v, SupervisorVerdict::Block);
        let v: SupervisorVerdict = serde_json::from_str("\"warn\"").unwrap();
        assert_eq!(v, SupervisorVerdict::Warn);
    }

    #[test]
    fn test_parse_supervisor_response_pass() {
        let json =
            r#"{"verdict": "pass", "scope_ok": true, "findings": [], "summary": "All good."}"#;
        let review = parse_supervisor_response(json).unwrap();
        assert_eq!(review.verdict, SupervisorVerdict::Pass);
        assert!(review.scope_ok);
        assert_eq!(review.summary, "All good.");
        assert!(review.findings.is_empty());
    }

    #[test]
    fn test_parse_supervisor_response_with_findings() {
        let json = r#"{"verdict": "warn", "scope_ok": false, "findings": ["Extra file modified", "Consider removing debug code"], "summary": "Minor concerns."}"#;
        let review = parse_supervisor_response(json).unwrap();
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert!(!review.scope_ok);
        assert_eq!(review.findings.len(), 2);
    }

    #[test]
    fn test_parse_supervisor_response_markdown_wrapped() {
        let text = "Here is the review:\n```json\n{\"verdict\": \"pass\", \"scope_ok\": true, \"findings\": [], \"summary\": \"LGTM.\"}\n```";
        let review = parse_supervisor_response(text).unwrap();
        assert_eq!(review.verdict, SupervisorVerdict::Pass);
    }

    #[test]
    fn test_parse_supervisor_response_unknown_verdict_falls_back_to_warn() {
        let json =
            r#"{"verdict": "unclear", "scope_ok": true, "findings": [], "summary": "Not sure."}"#;
        let review = parse_supervisor_response(json).unwrap();
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
    }

    #[test]
    fn test_parse_supervisor_response_block() {
        let json = r#"{"verdict": "block", "scope_ok": false, "findings": ["Modified unrelated system files"], "summary": "Significant scope violation."}"#;
        let review = parse_supervisor_response(json).unwrap();
        assert_eq!(review.verdict, SupervisorVerdict::Block);
        assert!(!review.scope_ok);
    }

    #[test]
    fn test_extract_json_backtick_block() {
        let text = "Some prose\n```json\n{\"key\": \"value\"}\n```\nMore prose";
        let extracted = extract_json(text);
        assert_eq!(extracted, "{\"key\": \"value\"}");
    }

    #[test]
    fn test_extract_json_plain() {
        let text = "{\"verdict\": \"pass\"}";
        let extracted = extract_json(text);
        assert_eq!(extracted, "{\"verdict\": \"pass\"}");
    }

    #[test]
    fn test_fallback_supervisor_review_structure() {
        // Validates the structure of the fallback review returned when supervisor fails.
        let fallback = fallback_supervisor_review("builtin", "ANTHROPIC_API_KEY not set", 0.001);
        assert_eq!(fallback.verdict, SupervisorVerdict::Warn);
        assert!(fallback.scope_ok);
        assert!(!fallback.findings.is_empty());
        assert_eq!(fallback.agent, "builtin");
    }

    #[test]
    fn test_extract_claude_stream_json_result_event() {
        // Stream-json with a result event containing the verdict JSON.
        let stream = r#"{"type":"system","subtype":"init"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Analyzing..."}]}}
{"type":"result","subtype":"success","result":"{\"verdict\":\"pass\",\"scope_ok\":true,\"findings\":[],\"summary\":\"All good.\"}"}
"#;
        let text = extract_claude_stream_json_text(stream);
        assert!(text.contains("verdict"));
        assert!(text.contains("pass"));
    }

    #[test]
    fn test_extract_claude_stream_json_fallback_to_assistant() {
        // No result event — should fall back to assistant message content.
        let stream = r#"{"type":"system","subtype":"init"}
{"type":"assistant","message":{"content":[{"type":"text","text":"{\"verdict\":\"warn\",\"scope_ok\":true,\"findings\":[],\"summary\":\"Minor issue.\"}"}]}}
"#;
        let text = extract_claude_stream_json_text(stream);
        assert!(text.contains("verdict"));
    }

    #[test]
    fn test_parse_supervisor_response_or_text_plain_text() {
        // Plain text fallback: no JSON → warn verdict with text as summary.
        let text = "The changes look fine overall but one extra file was touched.";
        let review = parse_supervisor_response_or_text(text, "codex");
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert_eq!(review.agent, "codex");
        assert!(review.summary.contains("extra file"));
    }

    #[test]
    fn test_parse_supervisor_response_or_text_structured_json() {
        let text = r#"{"verdict": "pass", "scope_ok": true, "findings": [], "summary": "LGTM."}"#;
        let review = parse_supervisor_response_or_text(text, "claude-code");
        assert_eq!(review.verdict, SupervisorVerdict::Pass);
        assert_eq!(review.agent, "claude-code");
    }

    #[test]
    fn test_invoke_supervisor_agent_api_key_preflight_fails() {
        // When api_key_env is set and the var is missing, should fall back to warn immediately.
        let config = SupervisorRunConfig {
            enabled: true,
            agent: "codex".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            timeout_secs: 30,
            api_key_env: Some("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR".to_string()),
            staging_path: None,
        };
        // Ensure the env var is not set.
        std::env::remove_var("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR");
        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert!(review.findings[0].contains("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR"));
    }

    #[test]
    fn test_invoke_supervisor_agent_custom_agent_no_staging_path() {
        // Custom agent with no staging_path → fallback to warn.
        let config = SupervisorRunConfig {
            enabled: true,
            agent: "my-custom-reviewer".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            timeout_secs: 30,
            api_key_env: None,
            staging_path: None,
        };
        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
    }

    #[test]
    fn test_fallback_review_no_api_key_message() {
        // Structure test: fallback review should reference the missing env var.
        let config = SupervisorRunConfig {
            enabled: true,
            agent: "codex".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            timeout_secs: 30,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            staging_path: None,
        };
        std::env::remove_var("OPENAI_API_KEY");
        let review = invoke_supervisor_agent("objective", &[], None, &config);
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert!(
            review.findings[0].contains("OPENAI_API_KEY"),
            "finding should mention the missing env var"
        );
    }
}
