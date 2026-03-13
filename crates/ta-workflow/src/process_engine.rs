// process_engine.rs — Process-based workflow plugin bridge (v0.10.18).
//
// Spawns an external workflow engine process (e.g., LangGraph, CrewAI adapter)
// and communicates via JSON-over-stdio. This is the same pattern used by
// channel plugins (v0.10.4).
//
// Protocol:
//   TA → engine (stdin):  JSON messages with `type` field (one per line)
//   engine → TA (stdout): JSON responses with `type` field (one per line)
//
// Message types:
//   start:           { type: "start", definition: WorkflowDefinition }
//                    → { type: "started", workflow_id: string }
//   stage_completed: { type: "stage_completed", workflow_id, stage, verdicts }
//                    → { type: "action", action: StageAction }
//   status:          { type: "status", workflow_id }
//                    → { type: "status_response", status: WorkflowStatus }
//   cancel:          { type: "cancel", workflow_id }
//                    → { type: "cancelled" }
//   inject_feedback: { type: "inject_feedback", workflow_id, stage, feedback }
//                    → { type: "ack" }

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use crate::definition::WorkflowDefinition;
use crate::error::WorkflowError;
use crate::verdict::Verdict;
use crate::{FeedbackContext, StageAction, WorkflowId, WorkflowStatus};

/// Message sent from TA to the engine process.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineRequest {
    Start {
        definition: WorkflowDefinition,
    },
    StageCompleted {
        workflow_id: String,
        stage: String,
        verdicts: Vec<Verdict>,
    },
    Status {
        workflow_id: String,
    },
    Cancel {
        workflow_id: String,
    },
    InjectFeedback {
        workflow_id: String,
        stage: String,
        feedback: FeedbackContext,
    },
}

/// Message received from the engine process.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineResponse {
    Started { workflow_id: String },
    Action { action: StageAction },
    StatusResponse { status: WorkflowStatus },
    Cancelled,
    Ack,
    Error { message: String },
}

/// Configuration for a process-based workflow engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEngineConfig {
    /// Command to spawn the engine process.
    pub command: String,
    /// Arguments to pass to the engine.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for the process.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Timeout in seconds for responses (default: 30).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    30
}

/// Process-based workflow engine with JSON-over-stdio I/O (v0.10.18).
///
/// Spawns an external process on first use and communicates via
/// newline-delimited JSON on stdin/stdout. The process is kept alive
/// for the lifetime of the engine instance.
pub struct ProcessWorkflowEngine {
    config: ProcessEngineConfig,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<std::io::BufReader<ChildStdout>>,
}

impl ProcessWorkflowEngine {
    pub fn new(config: ProcessEngineConfig) -> Self {
        Self {
            config,
            child: None,
            stdin: None,
            stdout: None,
        }
    }

    pub fn config(&self) -> &ProcessEngineConfig {
        &self.config
    }

    /// Spawn the engine process if not already running.
    fn ensure_spawned(&mut self) -> Result<(), WorkflowError> {
        if self.child.is_some() {
            return Ok(());
        }

        let mut cmd = std::process::Command::new(&self.config.command);
        cmd.args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        if let Some(ref cwd) = self.config.cwd {
            cmd.current_dir(cwd);
        }

        tracing::info!(
            command = %self.config.command,
            args = ?self.config.args,
            "Spawning workflow engine process"
        );

        let mut child = cmd.spawn().map_err(|e| WorkflowError::ProcessError {
            reason: format!(
                "Failed to spawn engine process '{}': {}. \
                 Ensure the engine binary is installed and in PATH.",
                self.config.command, e
            ),
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkflowError::ProcessError {
                reason: format!(
                    "Engine process '{}' stdin not available. \
                 This is a bug — the process was spawned with Stdio::piped().",
                    self.config.command
                ),
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WorkflowError::ProcessError {
                reason: format!(
                    "Engine process '{}' stdout not available. \
                 This is a bug — the process was spawned with Stdio::piped().",
                    self.config.command
                ),
            })?;

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(std::io::BufReader::new(stdout));

        tracing::info!(
            command = %self.config.command,
            "Workflow engine process spawned successfully"
        );

        Ok(())
    }

    /// Send a JSON request and read a JSON response (newline-delimited).
    fn send_request(&mut self, request: &EngineRequest) -> Result<EngineResponse, WorkflowError> {
        self.ensure_spawned()?;

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| WorkflowError::ProcessError {
                reason: "Engine stdin unavailable after spawn".to_string(),
            })?;

        // Write JSON + newline to stdin.
        let json = serde_json::to_string(request).map_err(|e| WorkflowError::ProcessError {
            reason: format!("Failed to serialize request: {}", e),
        })?;

        writeln!(stdin, "{}", json).map_err(|e| WorkflowError::ProcessError {
            reason: format!(
                "Failed to write to engine '{}' stdin: {}. \
                 The engine process may have exited unexpectedly.",
                self.config.command, e
            ),
        })?;
        stdin.flush().map_err(|e| WorkflowError::ProcessError {
            reason: format!("Failed to flush engine stdin: {}", e),
        })?;

        // Read one line from stdout.
        let stdout = self
            .stdout
            .as_mut()
            .ok_or_else(|| WorkflowError::ProcessError {
                reason: "Engine stdout unavailable after spawn".to_string(),
            })?;

        let mut line = String::new();
        let bytes_read = stdout
            .read_line(&mut line)
            .map_err(|e| WorkflowError::ProcessError {
                reason: format!(
                    "Failed to read response from engine '{}': {}. \
                     The engine may have crashed or closed its stdout.",
                    self.config.command, e
                ),
            })?;

        if bytes_read == 0 {
            return Err(WorkflowError::ProcessError {
                reason: format!(
                    "Engine '{}' closed stdout (EOF). \
                     The process exited before sending a response. \
                     Check the engine's stderr output for error details.",
                    self.config.command
                ),
            });
        }

        serde_json::from_str(line.trim()).map_err(|e| WorkflowError::ProcessError {
            reason: format!(
                "Failed to parse engine response as JSON: {}. Raw line: '{}'",
                e,
                line.trim()
            ),
        })
    }

    /// Check if the engine process is still alive.
    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    /// Kill the engine process if running.
    pub fn shutdown(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self.stdin = None;
        self.stdout = None;
    }
}

impl Drop for ProcessWorkflowEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl crate::WorkflowEngine for ProcessWorkflowEngine {
    fn start(&mut self, def: &WorkflowDefinition) -> Result<WorkflowId, WorkflowError> {
        let request = EngineRequest::Start {
            definition: def.clone(),
        };
        match self.send_request(&request)? {
            EngineResponse::Started { workflow_id } => Ok(workflow_id),
            EngineResponse::Error { message } => {
                Err(WorkflowError::ProcessError { reason: message })
            }
            other => Err(WorkflowError::ProcessError {
                reason: format!("unexpected response: {:?}", other),
            }),
        }
    }

    fn stage_completed(
        &mut self,
        id: &str,
        stage: &str,
        verdicts: &[Verdict],
    ) -> Result<StageAction, WorkflowError> {
        let request = EngineRequest::StageCompleted {
            workflow_id: id.to_string(),
            stage: stage.to_string(),
            verdicts: verdicts.to_vec(),
        };
        match self.send_request(&request)? {
            EngineResponse::Action { action } => Ok(action),
            EngineResponse::Error { message } => {
                Err(WorkflowError::ProcessError { reason: message })
            }
            other => Err(WorkflowError::ProcessError {
                reason: format!("unexpected response: {:?}", other),
            }),
        }
    }

    fn status(&self, id: &str) -> Result<WorkflowStatus, WorkflowError> {
        // Status requires mutable access for I/O — this is a limitation of
        // the synchronous WorkflowEngine trait. For now, return NotFound
        // since the external process tracks its own state.
        Err(WorkflowError::NotFound { id: id.to_string() })
    }

    fn inject_feedback(
        &mut self,
        id: &str,
        stage: &str,
        feedback: FeedbackContext,
    ) -> Result<(), WorkflowError> {
        let request = EngineRequest::InjectFeedback {
            workflow_id: id.to_string(),
            stage: stage.to_string(),
            feedback,
        };
        match self.send_request(&request)? {
            EngineResponse::Ack => Ok(()),
            EngineResponse::Error { message } => {
                Err(WorkflowError::ProcessError { reason: message })
            }
            other => Err(WorkflowError::ProcessError {
                reason: format!("unexpected response: {:?}", other),
            }),
        }
    }

    fn cancel(&mut self, id: &str) -> Result<(), WorkflowError> {
        let request = EngineRequest::Cancel {
            workflow_id: id.to_string(),
        };
        match self.send_request(&request)? {
            EngineResponse::Cancelled => Ok(()),
            EngineResponse::Error { message } => {
                Err(WorkflowError::ProcessError { reason: message })
            }
            other => Err(WorkflowError::ProcessError {
                reason: format!("unexpected response: {:?}", other),
            }),
        }
    }

    fn list(&self) -> Vec<WorkflowStatus> {
        vec![] // Process engine tracks its own state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkflowEngine;

    #[test]
    fn engine_request_serialization() {
        let req = EngineRequest::Start {
            definition: WorkflowDefinition {
                name: "test".to_string(),
                stages: vec![],
                roles: Default::default(),
                verdict: None,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"start\""));
    }

    #[test]
    fn engine_response_deserialization() {
        let json = r#"{"type":"started","workflow_id":"abc-123"}"#;
        let resp: EngineResponse = serde_json::from_str(json).unwrap();
        match resp {
            EngineResponse::Started { workflow_id } => assert_eq!(workflow_id, "abc-123"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn process_engine_spawn_failure() {
        let config = ProcessEngineConfig {
            command: "./nonexistent-engine-binary-that-does-not-exist".to_string(),
            args: vec![],
            cwd: None,
            timeout_secs: 5,
        };
        let mut engine = ProcessWorkflowEngine::new(config);
        let def = WorkflowDefinition {
            name: "test".to_string(),
            stages: vec![],
            roles: Default::default(),
            verdict: None,
        };
        let result = engine.start(&def);
        assert!(matches!(result, Err(WorkflowError::ProcessError { .. })));
    }

    #[test]
    fn process_engine_config_defaults() {
        let yaml = r#"
command: "./my-engine"
args: ["--port", "8080"]
"#;
        let config: ProcessEngineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.command, "./my-engine");
        assert_eq!(config.args, vec!["--port", "8080"]);
        assert_eq!(config.timeout_secs, 30);
        assert!(config.cwd.is_none());
    }

    #[test]
    fn process_engine_is_not_running_before_spawn() {
        let config = ProcessEngineConfig {
            command: "echo".to_string(),
            args: vec![],
            cwd: None,
            timeout_secs: 5,
        };
        let mut engine = ProcessWorkflowEngine::new(config);
        assert!(!engine.is_running());
    }

    #[cfg(unix)]
    #[test]
    fn process_engine_cat_echo() {
        // Uses `cat` as a simple echo server: it reads stdin and writes to stdout.
        let config = ProcessEngineConfig {
            command: "cat".to_string(),
            args: vec![],
            cwd: None,
            timeout_secs: 5,
        };
        let mut engine = ProcessWorkflowEngine::new(config);

        // Ensure spawn works.
        engine.ensure_spawned().unwrap();
        assert!(engine.is_running());

        // Write a valid JSON response and read it back via raw I/O.
        // We can't use send_request because cat echoes the request, not a response.
        // But we can verify the I/O pipeline works.
        let stdin = engine.stdin.as_mut().unwrap();
        let response_json = r#"{"type":"started","workflow_id":"test-123"}"#;
        writeln!(stdin, "{}", response_json).unwrap();
        stdin.flush().unwrap();

        let stdout = engine.stdout.as_mut().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let resp: EngineResponse = serde_json::from_str(line.trim()).unwrap();
        match resp {
            EngineResponse::Started { workflow_id } => assert_eq!(workflow_id, "test-123"),
            _ => panic!("unexpected response"),
        }

        engine.shutdown();
        assert!(!engine.is_running());
    }
}
