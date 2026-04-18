// advisor_agent.rs — Advisor agent spawner for governed interactive session (v0.15.19).
//
// `spawn_advisor_agent()` builds the context for an advisor run (draft summary,
// available tools, phase summary at milestone) and launches a short-lived
// `ta run --headless --persona advisor` subprocess. The subprocess uses
// `ta_ask_human` to converse with the human, then calls `ta draft approve` +
// `ta draft apply` (or `ta draft deny`). We poll draft status until it reaches
// a terminal state and return the outcome.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::phase_summary::PhaseSummary;
use crate::workflow_session::AdvisorSecurity;

/// Outcome reported by the advisor agent after the session item is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorOutcome {
    /// Human approved; draft was applied.
    Applied,
    /// Human declined; draft was denied.
    Denied,
    /// Advisor timed out before the human responded.
    TimedOut,
    /// Advisor subprocess failed to start or exited with an error.
    SpawnFailed { reason: String },
}

impl std::fmt::Display for AdvisorOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdvisorOutcome::Applied => write!(f, "applied"),
            AdvisorOutcome::Denied => write!(f, "denied"),
            AdvisorOutcome::TimedOut => write!(f, "timed_out"),
            AdvisorOutcome::SpawnFailed { reason } => write!(f, "spawn_failed: {}", reason),
        }
    }
}

/// Configuration for a single advisor agent invocation.
#[derive(Debug, Clone)]
pub struct AdvisorConfig {
    /// Workspace root (project directory, where `.ta/` lives).
    pub workspace_root: PathBuf,
    /// ID of the draft package to review.
    pub draft_id: Uuid,
    /// Session item title (shown in advisor greeting).
    pub item_title: String,
    /// Session ID (used in context file naming to avoid collisions).
    pub session_id: Uuid,
    /// Item ID (used in context file naming).
    pub item_id: Uuid,
    /// Advisor security level (controls available tools in the prompt).
    pub security: AdvisorSecurity,
    /// Optional persona name (references `.ta/personas/<name>.toml`).
    pub persona: Option<String>,
    /// Optional pre-built phase summary for milestone review.
    pub phase_summary: Option<PhaseSummary>,
    /// Timeout for the advisor conversation (default: 30 min).
    pub timeout: Duration,
}

impl AdvisorConfig {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        draft_id: Uuid,
        item_title: impl Into<String>,
        session_id: Uuid,
        item_id: Uuid,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            draft_id,
            item_title: item_title.into(),
            session_id,
            item_id,
            security: AdvisorSecurity::ReadOnly,
            persona: None,
            phase_summary: None,
            timeout: Duration::from_secs(30 * 60),
        }
    }

    pub fn with_security(mut self, security: AdvisorSecurity) -> Self {
        self.security = security;
        self
    }

    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.persona = Some(persona.into());
        self
    }

    pub fn with_phase_summary(mut self, summary: PhaseSummary) -> Self {
        self.phase_summary = Some(summary);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Build the advisor context markdown injected into the advisor's CLAUDE.md.
pub fn build_advisor_context(config: &AdvisorConfig) -> String {
    let mut ctx = String::new();

    ctx.push_str("# Advisor Context\n\n");
    ctx.push_str(&format!(
        "You are the **advisor** for session item: **{}**\n\n",
        config.item_title
    ));
    ctx.push_str(
        "You are explicitly on the human's side. Your job is to look out for their interests:\n\
         - Present what changed clearly in plain English.\n\
         - Proactively flag risks, missing tests, or incomplete work.\n\
         - Advocate against applying a draft that looks wrong.\n\
         - Ask clarifying questions when something is ambiguous.\n\
         - When the human approves, call `ta draft approve` then `ta draft apply`.\n\
         - When the human declines, call `ta draft deny`.\n\n",
    );

    ctx.push_str(&format!("**Draft ID**: `{}`\n\n", config.draft_id));
    ctx.push_str("Use `ta_draft_view` to read the full draft, `ta_fs_read` for file contents.\n\n");

    // Security level: list available tools.
    ctx.push_str("## Available Actions\n\n");
    match config.security {
        AdvisorSecurity::ReadOnly => {
            ctx.push_str(
                "Security level: **read_only**\n\
                 - You may answer questions and present diffs.\n\
                 - You may NOT start a goal or apply a draft autonomously.\n\
                 - When suggesting a follow-up, show the exact command for the human to run.\n\n",
            );
        }
        AdvisorSecurity::Suggest => {
            ctx.push_str(
                "Security level: **suggest**\n\
                 - You may present exact `ta run \"...\"` commands for the human to copy-paste.\n\
                 - The human must run any follow-up goals themselves.\n\n",
            );
        }
        AdvisorSecurity::Auto => {
            ctx.push_str(
                "Security level: **auto**\n\
                 - At ≥80% intent confidence, you may fire `ta run` directly.\n\
                 - You MUST call `ta_ask_human` first to confirm before applying.\n\
                 - Use `classify_intent()` to assess confidence before acting autonomously.\n\n",
            );
        }
    }

    // Phase summary if present.
    if let Some(ref ps) = config.phase_summary {
        ctx.push_str("## Phase Run Summary\n\n");
        ctx.push_str(&ps.render_terminal());
        ctx.push('\n');
    }

    ctx.push_str(
        "## Conversation Protocol\n\n\
         1. Call `ta_draft_view` to load the draft summary.\n\
         2. Present: what changed, key decisions, any risks flagged, questions for the human.\n\
         3. Call `ta_ask_human(\"Here's what changed: [summary]. Any concerns before I apply?\")` \
            — use `response_hint: freeform`.\n\
         4. Interpret the human's response:\n\
            - \"apply\" / \"looks good\" → call `ta draft approve`, then `ta draft apply`, then exit.\n\
            - \"skip\" / \"don't apply\" → call `ta draft deny`, then exit.\n\
            - A modification request → present the `ta run \"...\"` command (or fire it in auto mode).\n\
            - A question → answer from the decision log and `ta_fs_read`, then loop back to step 2.\n\
         5. Never apply without explicit human approval (unless security = auto and confidence ≥ 80%).\n"
    );

    ctx
}

/// Write the advisor context to `.ta/advisor/<item_id>/context.md` in the workspace.
///
/// Returns the path to the written context file.
pub fn write_advisor_context(config: &AdvisorConfig) -> std::io::Result<PathBuf> {
    let advisor_dir = config
        .workspace_root
        .join(".ta")
        .join("advisor")
        .join(config.item_id.to_string());
    std::fs::create_dir_all(&advisor_dir)?;

    let context_path = advisor_dir.join("context.md");
    let content = build_advisor_context(config);
    std::fs::write(&context_path, content)?;
    Ok(context_path)
}

/// Spawn an advisor agent for the given session item.
///
/// Launches `ta run --headless` as a subprocess with:
/// - `TA_ADVISOR_DRAFT_ID=<id>` environment variable
/// - `TA_ADVISOR_CONTEXT_FILE=<path>` pointing to the context markdown
/// - `--persona advisor` (or the configured persona)
///
/// Returns the advisor goal run ID extracted from stdout.
pub fn spawn_advisor_agent(config: &AdvisorConfig, ta_bin: &Path) -> Result<Uuid, String> {
    let context_path = write_advisor_context(config)
        .map_err(|e| format!("Failed to write advisor context: {}", e))?;

    let persona = config.persona.as_deref().unwrap_or("advisor");
    let goal_title = format!("Advisor: review session item '{}'", config.item_title);

    let mut cmd = std::process::Command::new(ta_bin);
    cmd.args([
        "--project-root",
        &config.workspace_root.to_string_lossy(),
        "run",
        &goal_title,
        "--headless",
        "--no-version-check",
        "--persona",
        persona,
    ]);
    cmd.env("TA_ADVISOR_DRAFT_ID", config.draft_id.to_string());
    cmd.env("TA_ADVISOR_CONTEXT_FILE", &context_path);
    cmd.env("TA_ADVISOR_SECURITY", config.security.to_string());
    cmd.env("TA_ADVISOR_SESSION_ID", config.session_id.to_string());
    cmd.env("TA_ADVISOR_ITEM_ID", config.item_id.to_string());

    tracing::info!(
        draft_id = %config.draft_id,
        item = %config.item_title,
        security = %config.security,
        "Spawning advisor agent"
    );

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ta run: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "ta run --headless exited {} for advisor goal.\nstdout: {}\nstderr: {}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        ));
    }

    // Extract goal_id from stdout (emitted by ta run on spawn).
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(id_str) = line.strip_prefix("goal_id: ") {
            let id_str = id_str.trim();
            return Uuid::parse_str(id_str)
                .map_err(|e| format!("Failed to parse advisor goal_id '{}': {}", id_str, e));
        }
    }

    Err(format!(
        "Advisor subprocess exited successfully but did not emit goal_id.\n\
         stdout: {}\nstderr: {}",
        stdout.trim(),
        stderr.trim()
    ))
}

/// Poll the draft status until it reaches a terminal state (Applied or Denied).
///
/// Checks every `poll_interval` until `timeout` is reached. Returns the outcome.
pub fn poll_draft_outcome(
    workspace_root: &Path,
    draft_id: Uuid,
    timeout: Duration,
    poll_interval: Duration,
) -> AdvisorOutcome {
    let drafts_dir = workspace_root.join(".ta").join("drafts");
    let draft_file = drafts_dir.join(format!("{}.json", draft_id));
    let deadline = Instant::now() + timeout;

    loop {
        if Instant::now() >= deadline {
            tracing::warn!(
                draft_id = %draft_id,
                timeout_secs = timeout.as_secs(),
                "Advisor timed out waiting for draft outcome"
            );
            return AdvisorOutcome::TimedOut;
        }

        match std::fs::read_to_string(&draft_file) {
            Ok(content) => {
                // Quick heuristic parse — look for "status" field in the JSON.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    let status_key = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
                    match status_key {
                        "applied" => return AdvisorOutcome::Applied,
                        "denied" => return AdvisorOutcome::Denied,
                        // For nested status objects (Approved/Denied with metadata)
                        _ if content.contains("\"applied\"") => return AdvisorOutcome::Applied,
                        _ if content.contains("\"denied\"") => return AdvisorOutcome::Denied,
                        _ => {}
                    }
                    // Check for nested object variant {"applied": {...}} or {"denied": {...}}.
                    if v.get("applied").is_some() {
                        return AdvisorOutcome::Applied;
                    }
                    if v.get("denied").is_some() {
                        return AdvisorOutcome::Denied;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(draft_id = %draft_id, "Draft file not found yet, waiting...");
            }
            Err(e) => {
                tracing::warn!(draft_id = %draft_id, error = %e, "Error reading draft file");
            }
        }

        std::thread::sleep(poll_interval);
    }
}

/// Constitution guard: verify that auto-apply is permitted by the project constitution.
///
/// Blocks `ta draft apply` unless either:
/// 1. `advisor_security = "auto"` is configured (explicit opt-in), or
/// 2. The human sent an explicit approval message.
pub fn check_advisor_auto_approve(
    security: &AdvisorSecurity,
    human_approved_explicitly: bool,
) -> Result<(), String> {
    if human_approved_explicitly {
        return Ok(());
    }
    match security {
        AdvisorSecurity::Auto => Ok(()),
        AdvisorSecurity::ReadOnly | AdvisorSecurity::Suggest => {
            Err("Constitution guard: auto-apply blocked. \
             The advisor may not call 'ta draft apply' without explicit human approval \
             unless advisor_security = \"auto\" is configured. \
             Set `advisor_security = \"auto\"` in .ta/workflow.toml to enable autonomous apply."
                .to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase_summary::{PhaseRecord, PhaseSummary};
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> AdvisorConfig {
        AdvisorConfig::new(
            tmp.path(),
            Uuid::new_v4(),
            "Implement feature X",
            Uuid::new_v4(),
            Uuid::new_v4(),
        )
    }

    #[test]
    fn build_advisor_context_read_only() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("read_only"));
        assert!(ctx.contains("Implement feature X"));
        assert!(ctx.contains("ta draft approve"));
        assert!(ctx.contains("ta draft deny"));
        assert!(ctx.contains("ta_ask_human"));
    }

    #[test]
    fn build_advisor_context_auto_security() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp).with_security(AdvisorSecurity::Auto);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("≥80% intent confidence"));
        assert!(ctx.contains("auto"));
    }

    #[test]
    fn build_advisor_context_includes_phase_summary() {
        let tmp = TempDir::new().unwrap();
        let mut ps = PhaseSummary::new();
        ps.add_phase(PhaseRecord::new("v0.15.14").with_decision("tokio spawn"));
        let config = make_config(&tmp).with_phase_summary(ps);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("Phase Run Summary"));
        assert!(ctx.contains("v0.15.14"));
    }

    #[test]
    fn write_advisor_context_creates_file() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let path = write_advisor_context(&config).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Advisor Context"));
    }

    #[test]
    fn constitution_guard_allows_explicit_approval() {
        assert!(check_advisor_auto_approve(&AdvisorSecurity::ReadOnly, true).is_ok());
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Suggest, true).is_ok());
    }

    #[test]
    fn constitution_guard_blocks_without_approval_in_read_only() {
        let result = check_advisor_auto_approve(&AdvisorSecurity::ReadOnly, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Constitution guard"));
    }

    #[test]
    fn constitution_guard_allows_auto_security_without_explicit() {
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Auto, false).is_ok());
    }

    #[test]
    fn advisor_outcome_display() {
        assert_eq!(AdvisorOutcome::Applied.to_string(), "applied");
        assert_eq!(AdvisorOutcome::Denied.to_string(), "denied");
        assert_eq!(AdvisorOutcome::TimedOut.to_string(), "timed_out");
        assert_eq!(
            AdvisorOutcome::SpawnFailed {
                reason: "no binary".to_string()
            }
            .to_string(),
            "spawn_failed: no binary"
        );
    }

    #[test]
    fn poll_draft_outcome_not_found_returns_timeout() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        // File doesn't exist; should time out quickly with a very short timeout.
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::TimedOut);
    }

    #[test]
    fn poll_draft_outcome_applied_status() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let drafts_dir = tmp.path().join(".ta/drafts");
        std::fs::create_dir_all(&drafts_dir).unwrap();
        let draft_file = drafts_dir.join(format!("{}.json", draft_id));
        // Write a draft JSON with "applied" status.
        std::fs::write(
            &draft_file,
            r#"{"draft_package_id": "00000000-0000-0000-0000-000000000000", "status": "applied"}"#,
        )
        .unwrap();
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::Applied);
    }

    #[test]
    fn poll_draft_outcome_denied_status() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let drafts_dir = tmp.path().join(".ta/drafts");
        std::fs::create_dir_all(&drafts_dir).unwrap();
        let draft_file = drafts_dir.join(format!("{}.json", draft_id));
        std::fs::write(
            &draft_file,
            r#"{"draft_package_id": "00000000-0000-0000-0000-000000000000", "status": "denied"}"#,
        )
        .unwrap();
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::Denied);
    }
}
