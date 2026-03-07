// interaction.rs — Human-in-the-loop interaction types.

use serde::{Deserialize, Serialize};

/// When to pause a stage for human input.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AwaitHumanConfig {
    /// Always pause for human input after stage completion.
    Always,
    /// Never pause — fully automated.
    #[default]
    Never,
    /// Pause only when verdicts fail the pass threshold.
    OnFail,
}

/// Request for human input during a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRequest {
    /// What the workflow is asking the human.
    pub prompt: String,
    /// Context (stage verdicts, scores, findings).
    pub context: serde_json::Value,
    /// Suggested choices (e.g., ["proceed", "revise", "cancel"]).
    #[serde(default)]
    pub options: Vec<String>,
    /// Auto-proceed after timeout (None = wait forever).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Human response to an interaction request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionResponse {
    /// The human's decision.
    pub decision: InteractionDecision,
    /// Optional feedback text.
    #[serde(default)]
    pub feedback: Option<String>,
}

/// Possible human decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionDecision {
    Proceed,
    Revise,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn await_human_default_is_never() {
        let config: AwaitHumanConfig = Default::default();
        assert_eq!(config, AwaitHumanConfig::Never);
    }

    #[test]
    fn interaction_request_serialization() {
        let req = InteractionRequest {
            prompt: "Review complete".to_string(),
            context: serde_json::json!({"score": 0.6}),
            options: vec!["proceed".to_string(), "revise".to_string()],
            timeout_secs: Some(300),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Review complete"));
        let restored: InteractionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.options.len(), 2);
    }

    #[test]
    fn interaction_response_serialization() {
        let resp = InteractionResponse {
            decision: InteractionDecision::Revise,
            feedback: Some("Fix the auth module".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"decision\":\"revise\""));
        let restored: InteractionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.decision, InteractionDecision::Revise);
    }

    #[test]
    fn await_human_yaml_parsing() {
        let yaml = "\"always\"";
        let config: AwaitHumanConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config, AwaitHumanConfig::Always);

        let yaml2 = "\"on_fail\"";
        let config2: AwaitHumanConfig = serde_yaml::from_str(yaml2).unwrap();
        assert_eq!(config2, AwaitHumanConfig::OnFail);
    }
}
