// intent_agent.rs — Headless clarifying-question agent (v0.17.0.12.23).
//
// Advisor-driven goal creation needs to ask a human exactly one clarifying
// question when `ta-brain::route()`'s workload-classification confidence is
// low, without building a new conversational loop. This module reuses the
// same subprocess-spawn + poll-file pattern `advisor_agent::spawn_advisor_agent`
// already established for draft-review conversations: spawn a short-lived
// `ta run --headless` subprocess whose context instructs it to call
// `ta_ask_human` exactly once and write the answer to a well-known file,
// then poll that file for the result.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Configuration for a single clarifying-question agent invocation.
#[derive(Debug, Clone)]
pub struct IntentAgentConfig {
    /// Workspace root (project directory, where `.ta/` lives).
    pub workspace_root: PathBuf,
    /// The raw free-text prompt the human originally gave.
    pub raw_prompt: String,
    /// The clarifying question to ask, pre-built by the caller from the
    /// low-confidence `RoutingDecision` (e.g. `ta-advisor::pipeline`).
    pub question: String,
    /// Item ID (used in context/answer file naming to avoid collisions).
    pub item_id: Uuid,
    /// Optional persona name (references `.ta/personas/<name>.toml`).
    pub persona: Option<String>,
    /// Timeout for the clarifying-question conversation (default: 10 min).
    pub timeout: Duration,
}

impl IntentAgentConfig {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        raw_prompt: impl Into<String>,
        question: impl Into<String>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            raw_prompt: raw_prompt.into(),
            question: question.into(),
            item_id: Uuid::new_v4(),
            persona: None,
            timeout: Duration::from_secs(10 * 60),
        }
    }

    pub fn with_item_id(mut self, item_id: Uuid) -> Self {
        self.item_id = item_id;
        self
    }

    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.persona = Some(persona.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn item_dir(&self) -> PathBuf {
        self.workspace_root
            .join(".ta")
            .join("advisor-intent")
            .join(self.item_id.to_string())
    }
}

/// Build the context markdown injected into the clarifying-question agent's
/// CLAUDE.md. Deliberately narrow in scope compared to `build_advisor_context`
/// (draft review): this agent's only job is to ask one question and report
/// the answer, not to review a draft or apply/deny anything.
pub fn build_intent_context(config: &IntentAgentConfig) -> String {
    format!(
        "# Advisor Context — Free-Text Goal Clarification\n\n\
         A human asked for the following, but routing confidence was too low \
         to act on directly:\n\n> {}\n\n\
         Your only job is to ask ONE clarifying question and report the answer. \
         Do not start any other work.\n\n\
         ## Conversation Protocol\n\n\
         1. Call `ta_ask_human(\"{}\")` with `response_hint: freeform`.\n\
         2. Write the human's raw answer, verbatim, to `.ta/advisor-intent/{}/answer.json` \
            (via `ta_fs_write`) as: `{{\"answer\": \"<their reply>\"}}`.\n\
         3. Exit — do not ask a second question, and do not start a goal yourself.\n",
        config.raw_prompt, config.question, config.item_id
    )
}

/// Write the clarification context to `.ta/advisor-intent/<item_id>/context.md`.
///
/// Returns the path to the written context file.
pub fn write_intent_context(config: &IntentAgentConfig) -> std::io::Result<PathBuf> {
    let dir = config.item_dir();
    std::fs::create_dir_all(&dir)?;
    let context_path = dir.join("context.md");
    std::fs::write(&context_path, build_intent_context(config))?;
    Ok(context_path)
}

/// Spawn a clarifying-question agent for the given config.
///
/// Launches `ta run --headless` as a subprocess with
/// `TA_ADVISOR_INTENT_CONTEXT_FILE=<path>` and `TA_ADVISOR_INTENT_ITEM_ID=<id>`
/// environment variables, mirroring `advisor_agent::spawn_advisor_agent`.
///
/// Returns the clarification goal run ID extracted from stdout.
pub fn spawn_intent_agent(config: &IntentAgentConfig, ta_bin: &Path) -> Result<Uuid, String> {
    let context_path = write_intent_context(config)
        .map_err(|e| format!("Failed to write clarification context: {}", e))?;

    let persona = config.persona.as_deref().unwrap_or("advisor");
    let goal_title = "Advisor: clarify free-text goal request".to_string();

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
    cmd.env("TA_ADVISOR_INTENT_CONTEXT_FILE", &context_path);
    cmd.env("TA_ADVISOR_INTENT_ITEM_ID", config.item_id.to_string());

    tracing::info!(
        item_id = %config.item_id,
        raw_prompt = %config.raw_prompt,
        "Spawning clarifying-question agent"
    );

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ta run: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "ta run --headless exited {} for clarification agent.\nstdout: {}\nstderr: {}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        ));
    }

    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(id_str) = line.strip_prefix("goal_id: ") {
            let id_str = id_str.trim();
            return Uuid::parse_str(id_str)
                .map_err(|e| format!("Failed to parse clarification goal_id '{}': {}", id_str, e));
        }
    }

    Err(format!(
        "Clarification agent exited successfully but did not emit goal_id.\n\
         stdout: {}\nstderr: {}",
        stdout.trim(),
        stderr.trim()
    ))
}

/// Outcome of polling for the clarifying-question agent's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAgentOutcome {
    /// The human answered; here's their raw reply.
    Answered(String),
    /// No answer arrived before the timeout.
    TimedOut,
}

/// Poll `.ta/advisor-intent/<item_id>/answer.json` until it appears or the
/// timeout elapses. Sleep-based (the answer path is low-frequency enough
/// that a file watcher isn't warranted, unlike `poll_draft_outcome`).
pub fn poll_intent_answer(
    workspace_root: &Path,
    item_id: Uuid,
    timeout: Duration,
    poll_interval: Duration,
) -> IntentAgentOutcome {
    let answer_file = workspace_root
        .join(".ta")
        .join("advisor-intent")
        .join(item_id.to_string())
        .join("answer.json");
    let deadline = Instant::now() + timeout;
    let effective_interval = poll_interval.max(Duration::from_millis(200));

    loop {
        if let Some(outcome) = check_answer_file(&answer_file) {
            return outcome;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                item_id = %item_id,
                timeout_secs = timeout.as_secs(),
                "Clarification agent timed out waiting for an answer"
            );
            return IntentAgentOutcome::TimedOut;
        }
        std::thread::sleep(effective_interval);
    }
}

fn check_answer_file(path: &Path) -> Option<IntentAgentOutcome> {
    let content = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let answer = v.get("answer")?.as_str()?.to_string();
    Some(IntentAgentOutcome::Answered(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> IntentAgentConfig {
        IntentAgentConfig::new(
            tmp.path(),
            "also clean up that auth thing",
            "Which auth module — login, session refresh, or OAuth callback?",
        )
    }

    #[test]
    fn build_intent_context_includes_prompt_and_question() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let ctx = build_intent_context(&config);
        assert!(ctx.contains("also clean up that auth thing"));
        assert!(ctx.contains("Which auth module"));
        assert!(ctx.contains("ta_ask_human"));
        assert!(ctx.contains("answer.json"));
    }

    #[test]
    fn write_intent_context_creates_file() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let path = write_intent_context(&config).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Free-Text Goal Clarification"));
    }

    #[test]
    fn poll_intent_answer_returns_timeout_when_missing() {
        let tmp = TempDir::new().unwrap();
        let outcome = poll_intent_answer(
            tmp.path(),
            Uuid::new_v4(),
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, IntentAgentOutcome::TimedOut);
    }

    #[test]
    fn poll_intent_answer_reads_written_answer() {
        let tmp = TempDir::new().unwrap();
        let item_id = Uuid::new_v4();
        let dir = tmp
            .path()
            .join(".ta/advisor-intent")
            .join(item_id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("answer.json"),
            r#"{"answer": "the OAuth callback handler"}"#,
        )
        .unwrap();

        let outcome = poll_intent_answer(
            tmp.path(),
            item_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        assert_eq!(
            outcome,
            IntentAgentOutcome::Answered("the OAuth callback handler".to_string())
        );
    }

    #[test]
    fn item_dir_is_scoped_by_item_id() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp).with_item_id(Uuid::nil());
        let path = write_intent_context(&config).unwrap();
        assert!(path
            .to_string_lossy()
            .contains("00000000-0000-0000-0000-000000000000"));
    }
}
