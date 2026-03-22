// action.rs — ExternalAction trait and built-in action type stubs (v0.13.4).
//
// The `ExternalAction` trait is the plugin contract: external plugins (e.g.,
// ta-action-email, ta-action-slack) implement this to provide real send/post/call
// logic. TA provides the governance pipeline — policy, capture, rate-limiting,
// and review routing.
//
// Built-in stubs are schema-only: they define what fields are required for each
// action type but do not implement any network I/O. Plugins registered via
// the `ActionRegistry` replace stubs with real implementations.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

// ── Error ────────────────────────────────────────────────────────────────────

/// Errors from action validation or execution.
#[derive(Debug, Error)]
pub enum ActionError {
    #[error("validation failed: {0}")]
    Validation(String),

    #[error("execution failed: {0}")]
    Execution(String),

    #[error("action type '{0}' has no registered executor (stub only — register a plugin)")]
    StubOnly(String),

    #[error("missing required field: {0}")]
    MissingField(String),
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// The contract every external action plugin must implement.
///
/// Plugins are discovered at runtime; TA ships four built-in stubs
/// (`email`, `social_post`, `api_call`, `db_query`) that validate payloads
/// but return `ActionError::StubOnly` from `execute()`.
pub trait ExternalAction: Send + Sync {
    /// Canonical name for this action type (e.g., `"email"`, `"api_call"`).
    fn action_type(&self) -> &str;

    /// JSON Schema describing the payload fields this action expects.
    /// Returned to agents so they know how to construct a valid request.
    fn payload_schema(&self) -> Value;

    /// Validate a payload against this action's schema.
    /// Returns `Ok(())` if the payload is structurally valid.
    fn validate(&self, payload: &Value) -> Result<(), ActionError>;

    /// Execute the action with the given payload.
    /// Built-in stubs return `Err(ActionError::StubOnly(...))`.
    /// Plugin implementations perform the real I/O here.
    fn execute(&self, payload: &Value) -> Result<Value, ActionError>;
}

// ── Built-in stubs ───────────────────────────────────────────────────────────

/// Stub for sending email. Schema only — no SMTP/API implementation.
///
/// Required payload fields: `to` (string), `subject` (string), `body` (string).
/// Optional: `cc` (array of strings), `reply_to` (string).
pub struct EmailAction;

impl ExternalAction for EmailAction {
    fn action_type(&self) -> &str {
        "email"
    }

    fn payload_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["to", "subject", "body"],
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient email address"
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject line"
                },
                "body": {
                    "type": "string",
                    "description": "Email body (plain text or HTML)"
                },
                "cc": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "CC recipients"
                },
                "reply_to": {
                    "type": "string",
                    "description": "Reply-to address"
                }
            }
        })
    }

    fn validate(&self, payload: &Value) -> Result<(), ActionError> {
        require_string_field(payload, "to")?;
        require_string_field(payload, "subject")?;
        require_string_field(payload, "body")?;
        Ok(())
    }

    fn execute(&self, _payload: &Value) -> Result<Value, ActionError> {
        Err(ActionError::StubOnly("email".into()))
    }
}

/// Stub for posting to social media. Schema only — no platform API implementation.
///
/// Required payload fields: `platform` (string), `content` (string).
/// Optional: `media_urls` (array of strings), `thread_reply_to` (string).
pub struct SocialPostAction;

impl ExternalAction for SocialPostAction {
    fn action_type(&self) -> &str {
        "social_post"
    }

    fn payload_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["platform", "content"],
            "properties": {
                "platform": {
                    "type": "string",
                    "description": "Target platform (e.g., 'twitter', 'linkedin', 'mastodon')"
                },
                "content": {
                    "type": "string",
                    "description": "Post text content"
                },
                "media_urls": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional media attachments"
                },
                "thread_reply_to": {
                    "type": "string",
                    "description": "Post ID to reply to (for thread continuation)"
                }
            }
        })
    }

    fn validate(&self, payload: &Value) -> Result<(), ActionError> {
        require_string_field(payload, "platform")?;
        require_string_field(payload, "content")?;
        Ok(())
    }

    fn execute(&self, _payload: &Value) -> Result<Value, ActionError> {
        Err(ActionError::StubOnly("social_post".into()))
    }
}

/// Stub for making HTTP API calls. Schema only — no HTTP client implementation.
///
/// Required payload fields: `method` (string), `url` (string).
/// Optional: `headers` (object), `body` (any), `timeout_secs` (number).
pub struct ApiCallAction;

impl ExternalAction for ApiCallAction {
    fn action_type(&self) -> &str {
        "api_call"
    }

    fn payload_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["method", "url"],
            "properties": {
                "method": {
                    "type": "string",
                    "description": "HTTP method (GET, POST, PUT, DELETE, PATCH)"
                },
                "url": {
                    "type": "string",
                    "description": "Full URL including query parameters"
                },
                "headers": {
                    "type": "object",
                    "description": "HTTP headers as key-value pairs"
                },
                "body": {
                    "description": "Request body (any JSON-serializable value)"
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Request timeout in seconds (default: 30)"
                }
            }
        })
    }

    fn validate(&self, payload: &Value) -> Result<(), ActionError> {
        require_string_field(payload, "method")?;
        require_string_field(payload, "url")?;
        Ok(())
    }

    fn execute(&self, _payload: &Value) -> Result<Value, ActionError> {
        Err(ActionError::StubOnly("api_call".into()))
    }
}

/// Stub for executing database queries. Schema only — no DB driver implementation.
///
/// Required payload fields: `query` (string).
/// Optional: `params` (array), `database` (string).
pub struct DbQueryAction;

impl ExternalAction for DbQueryAction {
    fn action_type(&self) -> &str {
        "db_query"
    }

    fn payload_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "SQL or query-language statement to execute"
                },
                "params": {
                    "type": "array",
                    "description": "Positional or named query parameters"
                },
                "database": {
                    "type": "string",
                    "description": "Target database name or connection alias"
                }
            }
        })
    }

    fn validate(&self, payload: &Value) -> Result<(), ActionError> {
        require_string_field(payload, "query")?;
        Ok(())
    }

    fn execute(&self, _payload: &Value) -> Result<Value, ActionError> {
        Err(ActionError::StubOnly("db_query".into()))
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// Registry of known action type implementations.
///
/// Populated at startup with the four built-in stubs. Projects or plugins can
/// register additional action types before the gateway begins serving requests.
pub struct ActionRegistry {
    actions: Vec<Box<dyn ExternalAction>>,
}

impl ActionRegistry {
    /// Create a registry pre-populated with the four built-in stubs.
    pub fn new() -> Self {
        Self {
            actions: vec![
                Box::new(EmailAction),
                Box::new(SocialPostAction),
                Box::new(ApiCallAction),
                Box::new(DbQueryAction),
            ],
        }
    }

    /// Register a custom action type (replaces an existing stub of the same name).
    pub fn register(&mut self, action: Box<dyn ExternalAction>) {
        let action_type = action.action_type().to_owned();
        self.actions.retain(|a| a.action_type() != action_type);
        self.actions.push(action);
    }

    /// Look up an action by type name.
    pub fn get(&self, action_type: &str) -> Option<&dyn ExternalAction> {
        self.actions
            .iter()
            .find(|a| a.action_type() == action_type)
            .map(|a| a.as_ref())
    }

    /// List all registered action types with their schemas.
    pub fn list(&self) -> Vec<ActionTypeInfo> {
        self.actions
            .iter()
            .map(|a| ActionTypeInfo {
                action_type: a.action_type().to_owned(),
                schema: a.payload_schema(),
            })
            .collect()
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of a registered action type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTypeInfo {
    pub action_type: String,
    pub schema: Value,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn require_string_field(payload: &Value, field: &str) -> Result<(), ActionError> {
    match payload.get(field) {
        Some(Value::String(s)) if !s.is_empty() => Ok(()),
        Some(Value::String(_)) => Err(ActionError::Validation(format!(
            "field '{}' must not be empty",
            field
        ))),
        Some(_) => Err(ActionError::Validation(format!(
            "field '{}' must be a string",
            field
        ))),
        None => Err(ActionError::MissingField(field.into())),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn email_validates_required_fields() {
        let action = EmailAction;
        let valid = json!({"to": "alice@example.com", "subject": "Hi", "body": "Hello"});
        assert!(action.validate(&valid).is_ok());

        let missing_body = json!({"to": "alice@example.com", "subject": "Hi"});
        assert!(action.validate(&missing_body).is_err());
    }

    #[test]
    fn email_execute_returns_stub_only_error() {
        let action = EmailAction;
        let payload = json!({"to": "a@b.com", "subject": "s", "body": "b"});
        let err = action.execute(&payload).unwrap_err();
        assert!(matches!(err, ActionError::StubOnly(_)));
    }

    #[test]
    fn social_post_validates_platform_and_content() {
        let action = SocialPostAction;
        let valid = json!({"platform": "twitter", "content": "Hello world!"});
        assert!(action.validate(&valid).is_ok());

        let missing = json!({"platform": "twitter"});
        assert!(action.validate(&missing).is_err());
    }

    #[test]
    fn api_call_validates_method_and_url() {
        let action = ApiCallAction;
        let valid = json!({"method": "POST", "url": "https://api.example.com/data"});
        assert!(action.validate(&valid).is_ok());

        let missing = json!({"method": "GET"});
        assert!(action.validate(&missing).is_err());
    }

    #[test]
    fn db_query_validates_query_field() {
        let action = DbQueryAction;
        let valid = json!({"query": "SELECT * FROM users WHERE id = $1", "params": [42]});
        assert!(action.validate(&valid).is_ok());

        let empty = json!({"query": ""});
        assert!(action.validate(&empty).is_err());
    }

    #[test]
    fn registry_lists_four_built_in_types() {
        let registry = ActionRegistry::new();
        let types: Vec<_> = registry.list().into_iter().map(|i| i.action_type).collect();
        assert!(types.contains(&"email".to_owned()));
        assert!(types.contains(&"social_post".to_owned()));
        assert!(types.contains(&"api_call".to_owned()));
        assert!(types.contains(&"db_query".to_owned()));
    }

    #[test]
    fn registry_get_returns_correct_action() {
        let registry = ActionRegistry::new();
        assert!(registry.get("email").is_some());
        assert!(registry.get("unknown_type").is_none());
    }
}
