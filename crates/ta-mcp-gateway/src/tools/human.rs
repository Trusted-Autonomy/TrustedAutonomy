// tools/human.rs — ta_ask_human MCP tool handler (v0.9.9.1).
//
// File-based signaling design:
//   1. Write question to .ta/interactions/pending/<interaction_id>.json
//   2. Emit AgentNeedsInput event to FsEventStore
//   3. Poll .ta/interactions/answers/<interaction_id>.json every 1s
//   4. When answer arrives, clean up both files and return the answer
//   5. On timeout, clean up question file and return an error message
//
// The daemon's HTTP endpoint (api/interactions.rs) writes the answer file.
// This mirrors the ta_event_subscribe polling pattern from event.rs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::server::GatewayState;

fn default_response_hint() -> String {
    "freeform".to_string()
}

fn default_timeout() -> Option<u64> {
    Some(600)
}

/// Parameters for `ta_ask_human`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskHumanParams {
    /// The question to ask the human.
    pub question: String,
    /// Optional context for the human (what the agent has done so far).
    #[serde(default)]
    pub context: Option<String>,
    /// Expected response shape: "freeform" (default), "yes_no", "choice".
    #[serde(default = "default_response_hint")]
    pub response_hint: String,
    /// Suggested choices when response_hint is "choice".
    #[serde(default)]
    pub choices: Vec<String>,
    /// How long to wait in seconds before timing out. Default: 600 (10 min).
    #[serde(default = "default_timeout")]
    pub timeout_secs: Option<u64>,
}

/// Serialized form of a question written to the pending directory.
#[derive(Debug, Serialize, Deserialize)]
struct PendingQuestion {
    interaction_id: String,
    goal_id: Option<String>,
    question: String,
    context: Option<String>,
    response_hint: String,
    choices: Vec<String>,
    turn: u32,
    created_at: String,
    timeout_secs: Option<u64>,
}

/// Serialized form of an answer written by the daemon HTTP endpoint.
#[derive(Debug, Deserialize)]
struct AnswerFile {
    text: String,
    #[allow(dead_code)]
    responder_id: Option<String>,
    #[allow(dead_code)]
    answered_at: Option<String>,
}

pub fn handle_ask_human(
    state: &Arc<Mutex<GatewayState>>,
    params: AskHumanParams,
) -> Result<CallToolResult, McpError> {
    // ── 1. Gather workspace root and active goal_id from state ────
    let (workspace_root, goal_id_opt) = {
        let state = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

        let root = state.config.workspace_root.clone();

        // Find goal_id: look for any active agent session that has one, or
        // fall back to the most recent goal in the store.
        let goal_id = state
            .active_agents
            .values()
            .find_map(|session| session.goal_run_id)
            .or_else(|| {
                state
                    .goal_store
                    .list()
                    .ok()
                    .and_then(|goals| goals.into_iter().last().map(|g| g.goal_run_id))
            });

        (root, goal_id)
    };

    // ── 2. Generate interaction ID ────────────────────────────────
    let interaction_id = Uuid::new_v4();

    // ── 3. Prepare directories ────────────────────────────────────
    let ta_dir = workspace_root.join(".ta");
    let pending_dir = ta_dir.join("interactions").join("pending");
    let answers_dir = ta_dir.join("interactions").join("answers");

    std::fs::create_dir_all(&pending_dir).map_err(|e| {
        McpError::internal_error(
            format!("failed to create interactions/pending directory: {}", e),
            None,
        )
    })?;
    std::fs::create_dir_all(&answers_dir).map_err(|e| {
        McpError::internal_error(
            format!("failed to create interactions/answers directory: {}", e),
            None,
        )
    })?;

    // ── 4. Determine turn by counting existing question files ──────
    let turn = count_existing_questions(&pending_dir) + 1;

    // ── 5. Write question file ────────────────────────────────────
    let question_path = pending_dir.join(format!("{}.json", interaction_id));
    let question = PendingQuestion {
        interaction_id: interaction_id.to_string(),
        goal_id: goal_id_opt.map(|id| id.to_string()),
        question: params.question.clone(),
        context: params.context.clone(),
        response_hint: params.response_hint.clone(),
        choices: params.choices.clone(),
        turn,
        created_at: Utc::now().to_rfc3339(),
        timeout_secs: params.timeout_secs,
    };

    let question_json = serde_json::to_string_pretty(&question).map_err(|e| {
        McpError::internal_error(format!("failed to serialize question: {}", e), None)
    })?;

    std::fs::write(&question_path, &question_json).map_err(|e| {
        McpError::internal_error(
            format!(
                "failed to write question file at {}: {}",
                question_path.display(),
                e
            ),
            None,
        )
    })?;

    tracing::info!(
        interaction_id = %interaction_id,
        turn = turn,
        response_hint = %params.response_hint,
        goal_id = ?goal_id_opt,
        "ta_ask_human: question written, waiting for human response"
    );

    // ── 6. Emit AgentNeedsInput event ─────────────────────────────
    // Use goal_id or a zero UUID as a placeholder so the event is valid.
    let event_goal_id = goal_id_opt.unwrap_or_else(Uuid::nil);
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let events_dir = ta_dir.join("events");
        let event_store = FsEventStore::new(&events_dir);
        let event = SessionEvent::AgentNeedsInput {
            goal_id: event_goal_id,
            interaction_id,
            question: params.question.clone(),
            context: params.context.clone(),
            response_hint: params.response_hint.clone(),
            choices: params.choices.clone(),
            turn,
            timeout_secs: params.timeout_secs,
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("ta_ask_human: failed to emit AgentNeedsInput event: {}", e);
        }
    }

    // ── 7. Poll for answer file ───────────────────────────────────
    let timeout_secs = params.timeout_secs.unwrap_or(600);
    let answer_path = answers_dir.join(format!("{}.json", interaction_id));
    let started = Instant::now();
    let poll_interval = Duration::from_secs(1);
    let deadline = Duration::from_secs(timeout_secs);

    loop {
        if answer_path.exists() {
            // Read and parse the answer file.
            let raw = std::fs::read_to_string(&answer_path).map_err(|e| {
                McpError::internal_error(
                    format!(
                        "ta_ask_human: answer file appeared but could not be read ({}): {}. \
                         Check file permissions at {}",
                        interaction_id,
                        e,
                        answer_path.display()
                    ),
                    None,
                )
            })?;

            let answer: AnswerFile = serde_json::from_str(&raw).map_err(|e| {
                McpError::internal_error(
                    format!(
                        "ta_ask_human: answer file for {} is malformed: {}. \
                         Expected JSON with a 'text' field.",
                        interaction_id, e
                    ),
                    None,
                )
            })?;

            // Clean up both files.
            cleanup_file(&question_path, "question");
            cleanup_file(&answer_path, "answer");

            // Emit AgentQuestionAnswered event.
            {
                use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
                let events_dir = ta_dir.join("events");
                let event_store = FsEventStore::new(&events_dir);
                let event = SessionEvent::AgentQuestionAnswered {
                    goal_id: event_goal_id,
                    interaction_id,
                    responder_id: answer
                        .responder_id
                        .clone()
                        .unwrap_or_else(|| "api".to_string()),
                    turn,
                };
                if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
                    tracing::warn!(
                        "ta_ask_human: failed to emit AgentQuestionAnswered event: {}",
                        e
                    );
                }
            }

            tracing::info!(
                interaction_id = %interaction_id,
                turn = turn,
                "ta_ask_human: answer received"
            );

            let response = serde_json::json!({
                "answer": answer.text,
                "turn": turn,
                "interaction_id": interaction_id.to_string(),
            });

            return Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?]));
        }

        if started.elapsed() >= deadline {
            // Timeout — clean up question file and return informative error.
            cleanup_file(&question_path, "question");

            tracing::warn!(
                interaction_id = %interaction_id,
                timeout_secs = timeout_secs,
                "ta_ask_human: timed out waiting for human response"
            );

            let timeout_msg = format!(
                "Question timed out after {}s. No human response received. \
                 The question was: \"{}\". \
                 You may proceed with your best judgment or try asking differently. \
                 To increase the timeout, pass a larger timeout_secs value. \
                 Interaction ID was: {}",
                timeout_secs, params.question, interaction_id
            );

            return Ok(CallToolResult::success(vec![Content::text(timeout_msg)]));
        }

        std::thread::sleep(poll_interval);
    }
}

/// Count how many `.json` files exist in the pending directory.
/// Used to assign a monotonically increasing turn number.
fn count_existing_questions(pending_dir: &Path) -> u32 {
    std::fs::read_dir(pending_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "json")
                        .unwrap_or(false)
                })
                .count() as u32
        })
        .unwrap_or(0)
}

/// Delete a file, logging a warning if it fails.
fn cleanup_file(path: &PathBuf, label: &str) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!(
            path = %path.display(),
            "ta_ask_human: failed to clean up {} file: {}",
            label,
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    use crate::config::GatewayConfig;
    use crate::server::GatewayState;

    fn make_state(dir: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let config = GatewayConfig::for_project(dir);
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    /// Verify question file is written and answer file is read correctly.
    #[test]
    fn ask_human_writes_question_and_reads_answer() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let interaction_id_holder = Arc::new(Mutex::new(None::<String>));
        let holder_clone = Arc::clone(&interaction_id_holder);

        // Spawn a background thread that watches for the question file and
        // writes the answer file after a short delay.
        let pending_dir = dir.path().join(".ta").join("interactions").join("pending");
        let answers_dir = dir.path().join(".ta").join("interactions").join("answers");

        std::thread::spawn(move || {
            // Wait for the pending directory and a question file to appear.
            for _ in 0..30 {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(entries) = std::fs::read_dir(&pending_dir) {
                    let files: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                        .collect();
                    if let Some(entry) = files.first() {
                        let stem = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        *holder_clone.lock().unwrap() = Some(stem.clone());

                        // Write the answer file.
                        std::fs::create_dir_all(&answers_dir).unwrap();
                        let answer_path = answers_dir.join(format!("{}.json", stem));
                        let answer = serde_json::json!({
                            "text": "PostgreSQL",
                            "responder_id": "test",
                            "answered_at": Utc::now().to_rfc3339(),
                        });
                        std::fs::write(&answer_path, serde_json::to_string(&answer).unwrap())
                            .unwrap();
                        return;
                    }
                }
            }
        });

        let params = AskHumanParams {
            question: "Which database should I use?".to_string(),
            context: Some("Setting up the backend storage layer.".to_string()),
            response_hint: "choice".to_string(),
            choices: vec!["PostgreSQL".to_string(), "SQLite".to_string()],
            timeout_secs: Some(10),
        };

        let result = handle_ask_human(&state, params).expect("handle_ask_human failed");
        let content = &result.content[0];
        let text = serde_json::to_string(content).unwrap();
        assert!(
            text.contains("PostgreSQL"),
            "expected answer in response: {}",
            text
        );
    }

    /// Verify timeout returns an informative message rather than an error.
    #[test]
    fn ask_human_timeout_returns_message() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let params = AskHumanParams {
            question: "Will you answer?".to_string(),
            context: None,
            response_hint: "yes_no".to_string(),
            choices: vec![],
            timeout_secs: Some(2), // Very short for tests.
        };

        let result =
            handle_ask_human(&state, params).expect("handle_ask_human should return Ok on timeout");
        let content = &result.content[0];
        let text = serde_json::to_string(content).unwrap();
        assert!(
            text.contains("timed out") || text.contains("timeout"),
            "expected timeout message in: {}",
            text
        );
    }

    /// Verify turn counter increments with existing question files.
    #[test]
    fn count_existing_questions_counts_json_files() {
        let dir = tempdir().unwrap();
        let pending_dir = dir.path().join("pending");
        std::fs::create_dir_all(&pending_dir).unwrap();

        // Write two JSON files and one non-JSON file.
        std::fs::write(pending_dir.join("a.json"), "{}").unwrap();
        std::fs::write(pending_dir.join("b.json"), "{}").unwrap();
        std::fs::write(pending_dir.join("notes.txt"), "ignore").unwrap();

        assert_eq!(count_existing_questions(&pending_dir), 2);
    }
}
