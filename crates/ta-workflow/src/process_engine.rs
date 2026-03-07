// process_engine.rs — Process-based workflow plugin bridge.
//
// Spawns an external workflow engine process (e.g., LangGraph, CrewAI adapter)
// and communicates via JSON-over-stdio. This is the same pattern used by
// channel plugins (v0.10.4).
//
// Protocol:
//   TA → engine (stdin):  JSON messages with `type` field
//   engine → TA (stdout): JSON responses with `type` field
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
}

/// Process-based workflow engine stub.
///
/// Full implementation requires async I/O (spawning a child process,
/// reading/writing JSON lines). This provides the protocol types
/// and config so adapters can be developed against the spec.
pub struct ProcessWorkflowEngine {
    config: ProcessEngineConfig,
}

impl ProcessWorkflowEngine {
    pub fn new(config: ProcessEngineConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ProcessEngineConfig {
        &self.config
    }

    /// Send a request and read a response (placeholder for async impl).
    fn send_request(&self, _request: &EngineRequest) -> Result<EngineResponse, WorkflowError> {
        Err(WorkflowError::ProcessError {
            reason: format!(
                "Process engine '{}' not yet spawned. Full async process I/O requires the daemon runtime. \
                 Use YamlWorkflowEngine for built-in workflows, or implement the JSON-over-stdio protocol \
                 in your engine binary.",
                self.config.command
            ),
        })
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
        let request = EngineRequest::Status {
            workflow_id: id.to_string(),
        };
        match self.send_request(&request)? {
            EngineResponse::StatusResponse { status } => Ok(status),
            EngineResponse::Error { message } => {
                Err(WorkflowError::ProcessError { reason: message })
            }
            other => Err(WorkflowError::ProcessError {
                reason: format!("unexpected response: {:?}", other),
            }),
        }
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
    fn process_engine_errors_without_spawn() {
        let config = ProcessEngineConfig {
            command: "./my-engine".to_string(),
            args: vec![],
            cwd: None,
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
}
