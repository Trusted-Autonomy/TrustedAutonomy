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
    /// Kill supervisor if no token is received for this many seconds (default 30).
    ///
    /// Replaces wall-clock `timeout_secs`: a supervisor actively streaming a large diff
    /// will never be killed as long as tokens keep arriving. Only a truly stalled process
    /// (no output for `heartbeat_stale_secs`) is terminated.
    pub heartbeat_stale_secs: u64,
    /// Deprecated: use `heartbeat_stale_secs` instead. Accepted for backward compat and
    /// mapped to `heartbeat_stale_secs` at construction time with a deprecation warning.
    pub timeout_secs: u64,
    /// Optional env var name to check before spawning the agent (pre-flight UX check).
    /// When set, TA verifies the var exists and prints an actionable message if missing.
    /// The agent binary reads the var itself — TA never passes the value.
    pub api_key_env: Option<String>,
    /// Staging directory path (required for manifest-based custom agents).
    pub staging_path: Option<std::path::PathBuf>,
    /// Path to the heartbeat file written after each token chunk. When `None`, heartbeat
    /// writes are skipped (e.g. in tests or when no workspace dir is available).
    pub heartbeat_path: Option<std::path::PathBuf>,
    /// Optional agent profile name resolved from workflow.toml `[agent_profiles]`.
    /// When set, the `agent` field is treated as a fallback; the profile's `framework`
    /// drives dispatch and `model` is forwarded to the agent binary when applicable.
    pub agent_profile: Option<String>,
    /// Resolved model from agent_profile (if any). Passed to agent CLI via --model flag.
    pub resolved_model: Option<String>,
    /// Allow session hooks to fire in the supervisor subprocess. Default: false.
    ///
    /// When false, `CLAUDE_CODE_DISABLE_HOOKS=1` is set so that `SessionStart` and other
    /// hooks do not write JSON to stdout before supervisor content arrives. Set to `true`
    /// only if a custom hook must run during supervisor invocations.
    pub enable_hooks: bool,
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
/// - `"builtin"` | `"claude-code"` → spawn `claude --print --verbose --output-format stream-json`
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
/// Uses `claude --print --verbose --output-format stream-json <prompt>`. Auth is handled
/// entirely by the `claude` binary (subscription OAuth, API key from env or config).
/// `--verbose` is required when combining `--print` with `--output-format stream-json`.
fn invoke_claude_cli_supervisor(
    prompt: &str,
    config: &SupervisorRunConfig,
) -> anyhow::Result<SupervisorReview> {
    let staging = config.staging_path.as_deref();

    let mut args_owned: Vec<String> = vec![
        "--print".into(),
        "--verbose".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--allowedTools".into(),
        "Read(*),Grep(*),Glob(*)".into(),
    ];

    if let Some(ref model) = config.resolved_model {
        args_owned.push("--model".into());
        args_owned.push(model.clone());
    }

    args_owned.push(prompt.to_string());

    let args_refs: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();

    let disable_hooks_env: &[(&str, &str)] = if config.enable_hooks {
        &[]
    } else {
        &[("CLAUDE_CODE_DISABLE_HOOKS", "1")]
    };

    let stdout = spawn_with_heartbeat_monitor(
        "claude",
        &args_refs,
        config.heartbeat_stale_secs,
        config.heartbeat_path.as_deref(),
        "Claude Code CLI",
        staging,
        disable_hooks_env,
    )?;

    let text = extract_claude_stream_json_text(&stdout);
    let mut review = parse_supervisor_response_or_text(&text, "claude-code");
    apply_hedging_quality_gate(&mut review);
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
    let staging = config.staging_path.as_deref();
    let disable_hooks_env: &[(&str, &str)] = if config.enable_hooks {
        &[]
    } else {
        &[("CLAUDE_CODE_DISABLE_HOOKS", "1")]
    };
    let stdout = spawn_with_heartbeat_monitor(
        "codex",
        &["--approval-mode", "full-auto", "--quiet", prompt],
        config.heartbeat_stale_secs,
        config.heartbeat_path.as_deref(),
        "Codex CLI",
        staging,
        disable_hooks_env,
    )?;

    let mut review = parse_supervisor_response_or_text(&stdout, "codex");
    apply_hedging_quality_gate(&mut review);
    Ok(review)
}

/// Invoke the ollama agent via `ta agent run ollama --headless`.
fn invoke_ollama_supervisor(
    prompt: &str,
    config: &SupervisorRunConfig,
) -> anyhow::Result<SupervisorReview> {
    let staging = config.staging_path.as_deref();
    let disable_hooks_env: &[(&str, &str)] = if config.enable_hooks {
        &[]
    } else {
        &[("CLAUDE_CODE_DISABLE_HOOKS", "1")]
    };
    let stdout = spawn_with_heartbeat_monitor(
        "ta",
        &[
            "agent",
            "run",
            "ollama",
            "--headless",
            "--tools",
            "read,grep,glob",
            "--prompt",
            prompt,
        ],
        config.heartbeat_stale_secs,
        config.heartbeat_path.as_deref(),
        "ta-agent-ollama",
        staging,
        disable_hooks_env,
    )?;

    let mut review = parse_supervisor_response_or_text(&stdout, "ollama");
    apply_hedging_quality_gate(&mut review);
    Ok(review)
}

/// Check whether a stdout line is a Claude Code hook JSON event.
///
/// Hook events look like `{"type":"system","subtype":"hook_started",...}`. They arrive
/// on stdout before any supervisor content when hooks fire (e.g., `SessionStart`). We
/// discard these silently — they are not supervisor tokens and must not reset the stall
/// watchdog timer.
fn is_hook_json_line(line: &str) -> bool {
    let trimmed = line.trim();
    // Quick pre-check before JSON parse for performance.
    if !trimmed.starts_with('{') || !trimmed.contains("\"type\"") {
        return false;
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        val.get("type").and_then(|t| t.as_str()) == Some("system")
    } else {
        false
    }
}

/// Spawn a process and stream its stdout, writing a heartbeat file after each line received.
///
/// A reader thread collects stdout lines and sends them via a channel. The main thread
/// polls with a short timeout and kills the child if no output has arrived for
/// `stale_secs`. This replaces the wall-clock `spawn_with_timeout` approach: actively
/// streaming supervisors are never killed, only truly stalled ones (no tokens for
/// `stale_secs`).
///
/// `heartbeat_path` is optional; when `None`, heartbeat writes are skipped (e.g. in
/// tests or when no workspace dir is available).
///
/// `extra_env` is a slice of `(key, value)` pairs injected into the subprocess environment.
/// Pass `&[("CLAUDE_CODE_DISABLE_HOOKS", "1")]` to suppress hook stdout pollution from
/// Claude Code session hooks.
///
/// Lines that match `is_hook_json_line` (i.e., `{"type":"system",...}`) are discarded
/// silently — they do not count as heartbeat tokens and are not included in the output.
fn spawn_with_heartbeat_monitor(
    program: &str,
    args: &[&str],
    stale_secs: u64,
    heartbeat_path: Option<&std::path::Path>,
    label: &str,
    current_dir: Option<&std::path::Path>,
    extra_env: &[(&str, &str)],
) -> anyhow::Result<String> {
    use std::io::BufRead;
    use std::sync::mpsc;

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
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

    // Write initial heartbeat immediately so mtime is set from the moment of spawn.
    if let Some(hb) = heartbeat_path {
        let _ = std::fs::write(hb, b"");
    }

    // Spawn reader thread: reads stdout lines and sends them via channel.
    // Sends `None` on EOF.
    let (line_tx, line_rx) = mpsc::channel::<Option<String>>();
    let stdout = child.stdout.take();
    let reader_handle = std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if line_tx.send(Some(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = line_tx.send(None);
    });

    // Main loop: poll for lines, update heartbeat, check for stall.
    let poll_interval = std::time::Duration::from_millis(100);
    let stale_duration = std::time::Duration::from_secs(stale_secs);
    let mut stdout_str = String::new();
    let mut partial_output = String::new();
    let mut last_token = std::time::Instant::now();
    let mut eof = false;

    while !eof {
        match line_rx.recv_timeout(poll_interval) {
            Ok(Some(line)) => {
                // Discard hook JSON system events — they are not supervisor tokens.
                // Hook lines like {"type":"system","subtype":"hook_started",...} appear
                // on stdout before any real content when SessionStart hooks fire.
                // Counting them as heartbeat tokens causes a false stall: the watchdog
                // resets once on the hook line, then waits 30s for real content.
                if is_hook_json_line(&line) {
                    continue;
                }
                last_token = std::time::Instant::now();
                stdout_str.push_str(&line);
                stdout_str.push('\n');
                // Accumulate partial output for stall error messages (cap at 200 chars).
                if partial_output.len() < 200 {
                    partial_output.push_str(&line);
                    partial_output.push('\n');
                }
                // Update heartbeat on each received line.
                if let Some(hb) = heartbeat_path {
                    let _ = std::fs::write(hb, b"");
                }
            }
            Ok(None) => {
                eof = true; // Reader sent EOF sentinel.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No output received in poll_interval — check for stall.
                if last_token.elapsed() >= stale_duration {
                    let _ = child.kill();
                    let _ = reader_handle.join();
                    if let Some(hb) = heartbeat_path {
                        let _ = std::fs::remove_file(hb);
                    }
                    let partial = partial_output.trim().to_string();
                    anyhow::bail!(
                        "Supervisor stalled — no tokens received for {}s. Findings so far: {}",
                        stale_secs,
                        partial
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eof = true;
            }
        }
    }

    let _ = reader_handle.join();

    // Wait for child to exit.
    let status = child.wait()?;

    // Clean up heartbeat sentinel on completion.
    if let Some(hb) = heartbeat_path {
        let _ = std::fs::remove_file(hb);
    }

    if !status.success() && stdout_str.trim().is_empty() {
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

    Ok(stdout_str)
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
        "instruction": "Read the changed files using your available tools before forming each finding. \
                        Cite file:line in every finding that references code. \
                        Never write 'cannot be verified without viewing files' — view the files first.",
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
    let mut spawn_cmd = std::process::Command::new(parts[0]);
    spawn_cmd
        .args(&parts[1..])
        .current_dir(staging_path)
        .env("TA_SUPERVISOR_INPUT", input_path.to_str().unwrap_or(""))
        .env("TA_SUPERVISOR_OUTPUT", result_path.to_str().unwrap_or(""));
    if !config.enable_hooks {
        spawn_cmd.env("CLAUDE_CODE_DISABLE_HOOKS", "1");
    }
    let mut child = spawn_cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn custom agent '{}': {}", agent_name, e))?;

    // Write initial heartbeat for manifest agent.
    if let Some(ref hb) = config.heartbeat_path {
        let _ = std::fs::write(hb, b"");
    }

    // Manifest agents write a result file rather than streaming stdout, so we poll
    // for the result file and update the heartbeat on each poll tick.
    let stale_secs = config.heartbeat_stale_secs;
    let mut last_result_size: u64 = 0;
    let mut last_progress = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                // Update heartbeat if the result file has grown (agent is making progress).
                let current_size = std::fs::metadata(&result_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                if current_size > last_result_size {
                    last_result_size = current_size;
                    last_progress = std::time::Instant::now();
                    if let Some(ref hb) = config.heartbeat_path {
                        let _ = std::fs::write(hb, b"");
                    }
                }
                if last_progress.elapsed().as_secs() >= stale_secs {
                    let _ = child.kill();
                    // Clean up heartbeat.
                    if let Some(ref hb) = config.heartbeat_path {
                        let _ = std::fs::remove_file(hb);
                    }
                    anyhow::bail!(
                        "Custom agent '{}' stalled — no progress for {}s",
                        agent_name,
                        stale_secs
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                anyhow::bail!("Error waiting for custom agent '{}': {}", agent_name, e);
            }
        }
    }

    // Clean up heartbeat on completion.
    if let Some(ref hb) = config.heartbeat_path {
        let _ = std::fs::remove_file(hb);
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

Read the files listed above using your Read/Grep/Glob tools before forming each finding.
Cite `file:line` in every finding that references code.
Never write 'cannot be verified without viewing files' — view the files first.

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

/// Hedging phrases that indicate the supervisor did not read the staged files.
const HEDGING_PHRASES: &[&str] = &[
    "cannot be verified",
    "unable to confirm",
    "without viewing",
    "depends on implementation",
    "cannot verify",
    "unable to verify",
    "not possible to confirm",
];

/// Scan findings for hedging phrases that indicate the supervisor failed to read files.
///
/// Returns `true` if any finding contains a hedging phrase, and mutates the findings
/// to append a meta-finding explaining what happened.
pub(crate) fn apply_hedging_quality_gate(review: &mut SupervisorReview) -> bool {
    let mut hedged = false;
    for finding in &review.findings {
        let lower = finding.to_lowercase();
        if HEDGING_PHRASES.iter().any(|p| lower.contains(p)) {
            hedged = true;
            break;
        }
    }
    if hedged {
        if review.verdict == SupervisorVerdict::Pass {
            review.verdict = SupervisorVerdict::Warn;
        }
        review.findings.push(
            "Supervisor produced unverified finding — staging access may be missing or supervisor did not read the file.".to_string()
        );
    }
    hedged
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
    // Fallback: .ta/constitution.yaml (preferred for YAML-formatted rule sets)
    let yaml_path = staging_path.join(".ta/constitution.yaml");
    if yaml_path.exists() {
        return std::fs::read_to_string(&yaml_path).ok();
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

    /// Mutex to serialize tests that mutate the global PATH environment variable.
    /// Tests that create mock `claude` binaries and prepend a temp dir to PATH must
    /// acquire this lock to prevent parallel races where the wrong mock binary is found.
    static PATH_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            heartbeat_stale_secs: 30,
            timeout_secs: 30,
            api_key_env: Some("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR".to_string()),
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
        };
        // Ensure the env var is not set.
        std::env::remove_var("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR");
        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert!(review.findings[0].contains("TA_TEST_MISSING_KEY_XYZ_SUPERVISOR"));
    }

    #[test]
    fn test_heartbeat_written_per_chunk() {
        use tempfile::tempdir;
        // Use `echo` to produce output — available on all Unix-like systems.
        let dir = tempdir().unwrap();
        let hb_path = dir.path().join("supervisor.heartbeat");

        // Ensure the heartbeat file doesn't exist before the call.
        assert!(!hb_path.exists());

        // spawn_with_heartbeat_monitor with `echo` — produces one line of output.
        let result = spawn_with_heartbeat_monitor(
            "echo",
            &["heartbeat_test"],
            30, // stale_secs — won't trigger for a fast echo
            Some(hb_path.as_path()),
            "echo",
            None,
            &[],
        );
        // echo exits 0 so result is Ok.
        assert!(result.is_ok(), "echo should succeed: {:?}", result);
        let stdout = result.unwrap();
        assert!(stdout.contains("heartbeat_test"));
        // Heartbeat file is cleaned up on completion.
        assert!(
            !hb_path.exists(),
            "heartbeat file should be removed after completion"
        );
    }

    #[test]
    fn test_monitor_kills_stalled_process() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let hb_path = dir.path().join("supervisor_stall.heartbeat");

        // Use `sleep 60` to simulate a process that produces no output.
        // stale_secs = 1 so it should be killed almost immediately.
        let result = spawn_with_heartbeat_monitor(
            "sleep",
            &["60"],
            1, // stale_secs — kill after 1s of no output
            Some(hb_path.as_path()),
            "sleep",
            None,
            &[],
        );
        assert!(result.is_err(), "stalled process should be killed");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stalled") || err.contains("no tokens"),
            "error should mention stall: {}",
            err
        );
        // Heartbeat file should be cleaned up.
        assert!(
            !hb_path.exists(),
            "heartbeat file should be removed after stall"
        );
    }

    #[test]
    fn test_active_streaming_not_killed() {
        // A process that produces output frequently should NOT be killed.
        // We use a shell command to print multiple lines with small delays.
        // stale_secs = 5 (generous), process completes fast.
        let result = spawn_with_heartbeat_monitor(
            "sh",
            &["-c", "echo line1 && echo line2 && echo line3"],
            5,
            None, // no heartbeat file needed
            "sh",
            None,
            &[],
        );
        assert!(
            result.is_ok(),
            "fast-completing process should not be killed: {:?}",
            result
        );
        let stdout = result.unwrap();
        assert!(stdout.contains("line1"));
        assert!(stdout.contains("line3"));
    }

    #[test]
    fn test_timeout_secs_field_preserved() {
        // timeout_secs is preserved for backward compat — verify it doesn't break construction.
        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 30,
            timeout_secs: 120, // deprecated alias — must still be accepted
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
        };
        assert_eq!(config.heartbeat_stale_secs, 30);
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn test_stall_message_includes_partial_output() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let hb_path = dir.path().join("stall_partial.heartbeat");

        // Use a shell command: print some output then hang.
        // stale_secs = 1.
        let result = spawn_with_heartbeat_monitor(
            "sh",
            &["-c", "echo partial_finding && sleep 60"],
            1,
            Some(hb_path.as_path()),
            "sh",
            None,
            &[],
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // The stall message should include the partial output captured before stall.
        assert!(
            err.contains("partial_finding") || err.contains("Findings so far"),
            "stall error should include partial output: {}",
            err
        );
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
            heartbeat_stale_secs: 30,
            timeout_secs: 30,
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
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
            heartbeat_stale_secs: 30,
            timeout_secs: 30,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
        };
        std::env::remove_var("OPENAI_API_KEY");
        let review = invoke_supervisor_agent("objective", &[], None, &config);
        assert_eq!(review.verdict, SupervisorVerdict::Warn);
        assert!(
            review.findings[0].contains("OPENAI_API_KEY"),
            "finding should mention the missing env var"
        );
    }

    /// Verify that the claude CLI invocation includes `--verbose`.
    ///
    /// Creates a mock `claude` script on PATH that exits with an error if `--verbose` is absent
    /// and emits a plain-text JSON pass verdict if `--verbose` is present.  The supervisor
    /// picks up the JSON via its raw-stdout fallback path.  The test fails if the verdict is
    /// not `pass`, which would mean `--verbose` was dropped (mock exits 1, returns fallback Warn).
    #[test]
    #[cfg(unix)]
    fn test_claude_cli_supervisor_passes_verbose_flag() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let claude_path = tmp.path().join("claude");
        {
            let mut f = std::fs::File::create(&claude_path).unwrap();
            // The script checks that --verbose and --allowedTools are in $@ and emits a plain JSON verdict.
            // Using echo so there are no printf escaping issues with nested JSON.
            f.write_all(
                b"#!/bin/sh\n\
                  found_verbose=''\n\
                  found_tools=''\n\
                  for arg in \"$@\"; do\n\
                    [ \"$arg\" = \"--verbose\" ] && found_verbose=1\n\
                    [ \"$arg\" = \"--allowedTools\" ] && found_tools=1\n\
                  done\n\
                  if [ -z \"$found_verbose\" ]; then echo 'Error: --verbose missing' >&2; exit 1; fi\n\
                  if [ -z \"$found_tools\" ]; then echo 'Error: --allowedTools missing' >&2; exit 1; fi\n\
                  echo '{\"verdict\":\"pass\",\"scope_ok\":true,\"findings\":[],\"summary\":\"ok\"}'\n",
            )
            .unwrap();
        }
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();

        let _lock = PATH_MUTEX.lock().unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        // Prepend temp dir so our mock `claude` takes precedence.
        std::env::set_var("PATH", format!("{}:{}", tmp.path().display(), old_path));

        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 10,
            timeout_secs: 10,
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
        };

        let review = invoke_supervisor_agent("test objective", &[], None, &config);

        // Restore PATH before any assertions that might panic.
        std::env::set_var("PATH", old_path);

        assert_eq!(
            review.verdict,
            SupervisorVerdict::Pass,
            "Supervisor must pass --verbose to claude CLI; got findings: {:?}",
            review.findings
        );
    }

    #[test]
    fn test_build_supervisor_prompt_includes_file_inspection_instruction() {
        let prompt =
            build_supervisor_prompt("Add JWT authentication", &["src/auth.rs".to_string()], None);
        assert!(
            prompt.contains("Read"),
            "prompt must instruct supervisor to read files"
        );
        assert!(
            prompt.contains("file:line") || prompt.contains("file:"),
            "prompt must require file:line citations"
        );
        assert!(
            prompt.contains("cannot be verified") || prompt.contains("Never write"),
            "prompt must ban hedging phrases"
        );
    }

    #[test]
    fn test_hedging_quality_gate_fires_on_hedging_phrase() {
        let mut review = SupervisorReview {
            verdict: SupervisorVerdict::Pass,
            scope_ok: true,
            findings: vec![
                "This change cannot be verified without viewing the actual file contents."
                    .to_string(),
            ],
            summary: "Looks fine.".to_string(),
            agent: "claude-code".to_string(),
            duration_secs: 0.0,
        };
        let fired = apply_hedging_quality_gate(&mut review);
        assert!(fired, "quality gate should fire on 'cannot be verified'");
        assert_eq!(
            review.verdict,
            SupervisorVerdict::Warn,
            "verdict should be upgraded to Warn"
        );
        assert!(
            review
                .findings
                .last()
                .unwrap()
                .contains("Supervisor produced unverified finding"),
            "meta-finding should be appended"
        );
    }

    #[test]
    fn test_hedging_quality_gate_no_fire_on_clean_findings() {
        let mut review = SupervisorReview {
            verdict: SupervisorVerdict::Pass,
            scope_ok: true,
            findings: vec![
                "src/auth.rs:42: JWT secret is not rotated — consider adding rotation logic."
                    .to_string(),
            ],
            summary: "One finding.".to_string(),
            agent: "claude-code".to_string(),
            duration_secs: 0.0,
        };
        let fired = apply_hedging_quality_gate(&mut review);
        assert!(
            !fired,
            "quality gate should not fire on clean file:line findings"
        );
        assert_eq!(review.verdict, SupervisorVerdict::Pass);
    }

    #[test]
    fn test_hedging_quality_gate_preserves_block_verdict() {
        let mut review = SupervisorReview {
            verdict: SupervisorVerdict::Block,
            scope_ok: false,
            findings: vec![
                "Unable to confirm whether the migration is reversible without viewing migration files.".to_string(),
            ],
            summary: "Block.".to_string(),
            agent: "claude-code".to_string(),
            duration_secs: 0.0,
        };
        apply_hedging_quality_gate(&mut review);
        // Block should not be downgraded — only Pass is upgraded to Warn
        assert_eq!(
            review.verdict,
            SupervisorVerdict::Block,
            "Block verdict must not be changed"
        );
    }

    #[test]
    fn test_supervisor_run_config_agent_profile_field() {
        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 30,
            timeout_secs: 30,
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: Some("supervisor".to_string()),
            resolved_model: Some("claude-sonnet-4-6".to_string()),
            enable_hooks: false,
        };
        assert_eq!(config.agent_profile.as_deref(), Some("supervisor"));
        assert_eq!(config.resolved_model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[cfg(unix)]
    #[test]
    fn test_claude_supervisor_sets_current_dir_in_staging() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        // Create a staging dir with a sentinel file so we can verify cwd.
        let staging = tempfile::tempdir().unwrap();
        let sentinel = staging.path().join("STAGING_SENTINEL.txt");
        std::fs::write(&sentinel, b"yes").unwrap();

        // Create a mock `claude` that checks if STAGING_SENTINEL.txt exists in its cwd.
        let bin_dir = tempfile::tempdir().unwrap();
        let claude_path = bin_dir.path().join("claude");
        {
            let mut f = std::fs::File::create(&claude_path).unwrap();
            f.write_all(
                b"#!/bin/sh\n\
                  if [ ! -f STAGING_SENTINEL.txt ]; then\n\
                    echo 'Error: not running in staging dir' >&2; exit 1\n\
                  fi\n\
                  echo '{\"verdict\":\"pass\",\"scope_ok\":true,\"findings\":[],\"summary\":\"staging ok\"}'\n",
            )
            .unwrap();
        }
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();

        let _lock = PATH_MUTEX.lock().unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", bin_dir.path().display(), old_path));

        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 10,
            timeout_secs: 10,
            api_key_env: None,
            staging_path: Some(staging.path().to_path_buf()),
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false,
        };

        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        std::env::set_var("PATH", old_path);

        assert_eq!(
            review.verdict,
            SupervisorVerdict::Pass,
            "Supervisor must run in staging dir; got findings: {:?}",
            review.findings
        );
    }

    // ── v0.15.14.6 — Supervisor Hook JSON Filtering ───────────────────────

    #[test]
    fn test_is_hook_json_line_detects_system_type() {
        // SessionStart hook JSON that fires before supervisor content.
        let hook_line = r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart"}"#;
        assert!(
            is_hook_json_line(hook_line),
            "SessionStart hook JSON must be detected"
        );
    }

    #[test]
    fn test_is_hook_json_line_ignores_non_system_type() {
        // Real supervisor content should NOT be filtered.
        let result_line =
            r#"{"type":"result","subtype":"success","result":"{\"verdict\":\"pass\"}"}"#;
        assert!(
            !is_hook_json_line(result_line),
            "result event must not be filtered"
        );

        let assistant_line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        assert!(
            !is_hook_json_line(assistant_line),
            "assistant event must not be filtered"
        );
    }

    #[test]
    fn test_is_hook_json_line_ignores_plain_text() {
        assert!(!is_hook_json_line("some plain output"));
        assert!(!is_hook_json_line(""));
        assert!(!is_hook_json_line("not json at all"));
    }

    #[test]
    fn test_is_hook_json_line_ignores_non_json_braces() {
        // A line that starts with { but is not valid JSON should not crash.
        assert!(!is_hook_json_line("{not valid json}"));
    }

    /// Hook JSON lines must NOT be counted as heartbeat tokens and must NOT appear in
    /// the returned stdout string.
    #[cfg(unix)]
    #[test]
    fn test_hook_json_line_filtered_from_output() {
        // Emit a hook JSON line followed by a real content line.
        // The monitor should filter the hook line and return only the content line.
        let hook_json = r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart"}"#;
        let real_content = r#"{"type":"result","result":"done"}"#;
        let script = format!("echo '{}' && echo '{}'", hook_json, real_content);

        let result = spawn_with_heartbeat_monitor("sh", &["-c", &script], 5, None, "sh", None, &[]);
        assert!(result.is_ok(), "process should succeed: {:?}", result);
        let stdout = result.unwrap();
        // Hook line must be excluded from output.
        assert!(
            !stdout.contains("hook_started"),
            "hook JSON must not appear in output: {}",
            stdout
        );
        // Real content must be present.
        assert!(
            stdout.contains("result"),
            "real content must be in output: {}",
            stdout
        );
    }

    /// A stream consisting only of hook JSON lines should still trigger a stall — the
    /// stall timer must NOT be reset by hook lines.
    #[cfg(unix)]
    #[test]
    fn test_only_hook_json_lines_triggers_stall() {
        // Print a hook JSON line then hang for 60s.
        // stale_secs=1 — should stall because the hook line is filtered.
        let hook_json = r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart"}"#;
        let script = format!("echo '{}' && sleep 60", hook_json);

        let result = spawn_with_heartbeat_monitor(
            "sh",
            &["-c", &script],
            1, // stale_secs — very short so the test is fast
            None,
            "sh",
            None,
            &[],
        );
        assert!(
            result.is_err(),
            "stream of only hook JSON should trigger stall"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stalled") || err.contains("no tokens"),
            "stall error expected: {}",
            err
        );
    }

    /// CLAUDE_CODE_DISABLE_HOOKS env var must be set in the supervisor subprocess env
    /// when enable_hooks is false.
    #[cfg(unix)]
    #[test]
    fn test_disable_hooks_env_var_set_when_enable_hooks_false() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let claude_path = tmp.path().join("claude");
        {
            let mut f = std::fs::File::create(&claude_path).unwrap();
            // Script checks that CLAUDE_CODE_DISABLE_HOOKS=1 is set.
            f.write_all(
                b"#!/bin/sh\n\
                  if [ \"$CLAUDE_CODE_DISABLE_HOOKS\" = \"1\" ]; then\n\
                    echo '{\"verdict\":\"pass\",\"scope_ok\":true,\"findings\":[],\"summary\":\"hooks disabled\"}'\n\
                  else\n\
                    echo '{\"verdict\":\"block\",\"scope_ok\":false,\"findings\":[\"CLAUDE_CODE_DISABLE_HOOKS not set\"],\"summary\":\"hooks not disabled\"}'\n\
                  fi\n",
            )
            .unwrap();
        }
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();

        let _lock = PATH_MUTEX.lock().unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", tmp.path().display(), old_path));

        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 10,
            timeout_secs: 10,
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: false, // hooks should be suppressed
        };

        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        std::env::set_var("PATH", old_path);

        assert_eq!(
            review.verdict,
            SupervisorVerdict::Pass,
            "CLAUDE_CODE_DISABLE_HOOKS=1 must be set when enable_hooks=false; got: {:?}",
            review.findings
        );
    }

    /// When enable_hooks=true, CLAUDE_CODE_DISABLE_HOOKS must NOT be set.
    #[cfg(unix)]
    #[test]
    fn test_enable_hooks_true_does_not_set_disable_env() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let claude_path = tmp.path().join("claude");
        {
            let mut f = std::fs::File::create(&claude_path).unwrap();
            // Script checks that CLAUDE_CODE_DISABLE_HOOKS is NOT set to "1".
            f.write_all(
                b"#!/bin/sh\n\
                  if [ \"$CLAUDE_CODE_DISABLE_HOOKS\" = \"1\" ]; then\n\
                    echo '{\"verdict\":\"block\",\"scope_ok\":false,\"findings\":[\"CLAUDE_CODE_DISABLE_HOOKS was set unexpectedly\"],\"summary\":\"fail\"}'\n\
                  else\n\
                    echo '{\"verdict\":\"pass\",\"scope_ok\":true,\"findings\":[],\"summary\":\"hooks allowed\"}'\n\
                  fi\n",
            )
            .unwrap();
        }
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();

        let _lock = PATH_MUTEX.lock().unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        // Make sure the env var is cleared in the parent too.
        std::env::remove_var("CLAUDE_CODE_DISABLE_HOOKS");
        std::env::set_var("PATH", format!("{}:{}", tmp.path().display(), old_path));

        let config = SupervisorRunConfig {
            enabled: true,
            agent: "builtin".to_string(),
            verdict_on_block: "warn".to_string(),
            constitution_path: None,
            skip_if_no_constitution: true,
            heartbeat_stale_secs: 10,
            timeout_secs: 10,
            api_key_env: None,
            staging_path: None,
            heartbeat_path: None,
            agent_profile: None,
            resolved_model: None,
            enable_hooks: true, // hooks explicitly enabled — must NOT set DISABLE var
        };

        let review = invoke_supervisor_agent("test objective", &[], None, &config);
        std::env::set_var("PATH", old_path);

        assert_eq!(
            review.verdict,
            SupervisorVerdict::Pass,
            "CLAUDE_CODE_DISABLE_HOOKS must not be set when enable_hooks=true; got: {:?}",
            review.findings
        );
    }
}
