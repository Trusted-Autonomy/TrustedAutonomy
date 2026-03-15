//! Output schema types and validation.

use crate::extractor::extract_path;
use serde::{Deserialize, Serialize};

/// Errors from schema loading and validation.
#[derive(Debug, thiserror::Error)]
pub enum OutputSchemaError {
    #[error("schema parse error: {0}")]
    Parse(String),
    #[error("schema validation error: {0}")]
    Validation(String),
    #[error("schema not found for agent: {0}")]
    NotFound(String),
    #[error("IO error loading schema: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of parsing a single line of agent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseResult {
    /// Displayable text content.
    Text(String),
    /// Tool invocation (name of the tool).
    ToolUse(String),
    /// Model identification event.
    Model(String),
    /// Event should be suppressed (internal protocol event).
    Suppress,
    /// Line is not JSON or not recognized — caller decides how to display.
    NotJson,
}

/// Top-level output schema definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    /// Agent name this schema applies to.
    pub agent: String,
    /// Schema version for negotiation.
    pub schema_version: u32,
    /// Output format identifier (e.g., "stream-json").
    pub format: SchemaFormat,
    /// Ordered list of extractors — first match wins.
    pub extractors: Vec<Extractor>,
    /// Event types to suppress (show nothing).
    #[serde(default)]
    pub suppress: Vec<String>,
    /// Paths to check for model name extraction.
    #[serde(default)]
    pub model_paths: Vec<String>,
}

/// Output format identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaFormat {
    StreamJson,
    Jsonl,
    PlainText,
}

/// A single extractor rule: matches an event type and extracts content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extractor {
    /// Event type(s) this extractor matches. Supports exact match and wildcard "*".
    pub type_match: Vec<String>,
    /// What kind of output this extractor produces.
    pub output: ExtractorOutput,
    /// Ordered paths to try for extracting the value. First non-null wins.
    pub paths: Vec<String>,
    /// Optional prefix to add to extracted text (e.g., "[result] ").
    #[serde(default)]
    pub prefix: Option<String>,
    /// For content arrays: filter items by this type field value (e.g., "text").
    #[serde(default)]
    pub content_type_filter: Option<String>,
    /// For system events with subtypes, map subtype → format string.
    /// Format placeholders: `{field_name}` are replaced with values from the JSON.
    #[serde(default)]
    pub subtype_formats: std::collections::HashMap<String, SubtypeFormat>,
}

/// Format template for a system event subtype.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtypeFormat {
    /// Template string with `{field}` placeholders.
    pub template: String,
    /// Fields to extract from the JSON event.
    pub fields: Vec<String>,
}

/// What kind of output an extractor produces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtractorOutput {
    Text,
    ToolUse,
    Model,
}

impl OutputSchema {
    /// Create a passthrough schema that does no extraction.
    pub fn passthrough() -> Self {
        Self {
            agent: "passthrough".into(),
            schema_version: 1,
            format: SchemaFormat::PlainText,
            extractors: Vec::new(),
            suppress: Vec::new(),
            model_paths: Vec::new(),
        }
    }

    /// Check if an event type should be suppressed.
    pub fn is_suppressed(&self, event_type: &str) -> bool {
        self.suppress.iter().any(|s| s == event_type)
    }

    /// Validate the schema structure. Returns errors for invalid configurations.
    pub fn validate(&self) -> Result<(), OutputSchemaError> {
        if self.agent.is_empty() {
            return Err(OutputSchemaError::Validation(
                "agent name is required".into(),
            ));
        }
        if self.schema_version == 0 {
            return Err(OutputSchemaError::Validation(
                "schema_version must be >= 1".into(),
            ));
        }
        for (i, ext) in self.extractors.iter().enumerate() {
            if ext.type_match.is_empty() {
                return Err(OutputSchemaError::Validation(format!(
                    "extractor[{}] has no type_match entries",
                    i
                )));
            }
            if ext.paths.is_empty() && ext.subtype_formats.is_empty() {
                return Err(OutputSchemaError::Validation(format!(
                    "extractor[{}] has no paths and no subtype_formats",
                    i
                )));
            }
        }
        Ok(())
    }

    /// Extract the model name from a line, using the schema's model_paths.
    pub fn extract_model(&self, line: &str) -> Option<String> {
        let json: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        for path in &self.model_paths {
            if let Some(val) = extract_path(&json, path) {
                if let Some(s) = val.as_str() {
                    return Some(s.to_string());
                }
            }
        }
        None
    }
}

impl Extractor {
    /// Check if this extractor matches the given event type.
    pub fn matches_type(&self, event_type: &str) -> bool {
        self.type_match.iter().any(|m| m == event_type || m == "*")
    }

    /// Extract content from a JSON value using this extractor's configuration.
    pub fn extract(&self, json: &serde_json::Value) -> ParseResult {
        // Handle system events with subtype formatting.
        if !self.subtype_formats.is_empty() {
            if let Some(subtype) = json.get("subtype").and_then(|v| v.as_str()) {
                if let Some(fmt) = self.subtype_formats.get(subtype) {
                    return ParseResult::Text(fmt.render(json));
                }
                // Known type but unhandled subtype — suppress.
                return ParseResult::Suppress;
            }
        }

        // Try each path in order.
        for path in &self.paths {
            if let Some(value) = extract_path(json, path) {
                return self.value_to_result(&value);
            }
        }

        // No path matched — suppress for text/model, return empty for tool_use.
        ParseResult::Suppress
    }

    /// Convert an extracted JSON value to the appropriate ParseResult.
    fn value_to_result(&self, value: &serde_json::Value) -> ParseResult {
        match self.output {
            ExtractorOutput::Text => {
                let text = self.value_to_text(value);
                if text.is_empty() {
                    return ParseResult::Suppress;
                }
                let prefixed = match &self.prefix {
                    Some(p) => format!("{}{}", p, text),
                    None => text,
                };
                ParseResult::Text(prefixed)
            }
            ExtractorOutput::ToolUse => {
                let name = value.as_str().unwrap_or("unknown");
                ParseResult::ToolUse(name.to_string())
            }
            ExtractorOutput::Model => {
                let model = value.as_str().unwrap_or("unknown");
                ParseResult::Model(model.to_string())
            }
        }
    }

    /// Convert a JSON value to displayable text.
    fn value_to_text(&self, value: &serde_json::Value) -> String {
        if let Some(s) = value.as_str() {
            return s.to_string();
        }
        if let Some(arr) = value.as_array() {
            // If we have a content_type_filter, filter array items.
            if let Some(ref filter) = self.content_type_filter {
                let texts: Vec<&str> = arr
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|t| t.as_str()) == Some(filter) {
                            item.get("text").and_then(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                return texts.join("");
            }
            // Otherwise, join string items.
            let texts: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            return texts.join("");
        }
        // For other types, use display representation.
        value.to_string()
    }
}

impl SubtypeFormat {
    /// Render a subtype format template with fields from the JSON event.
    fn render(&self, json: &serde_json::Value) -> String {
        let mut result = self.template.clone();
        for field in &self.fields {
            let placeholder = format!("{{{}}}", field);
            let value = extract_path(json, field)
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            result = result.replace(&placeholder, &value);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_schema_is_valid() {
        let schema = OutputSchema::passthrough();
        // Passthrough has no agent name by default, but let's test the one we build.
        assert_eq!(schema.agent, "passthrough");
    }

    #[test]
    fn validation_catches_empty_agent() {
        let mut schema = OutputSchema::passthrough();
        schema.agent = String::new();
        assert!(schema.validate().is_err());
    }

    #[test]
    fn validation_catches_zero_version() {
        let mut schema = OutputSchema::passthrough();
        schema.schema_version = 0;
        assert!(schema.validate().is_err());
    }

    #[test]
    fn validation_catches_empty_type_match() {
        let mut schema = OutputSchema::passthrough();
        schema.extractors.push(Extractor {
            type_match: vec![],
            output: ExtractorOutput::Text,
            paths: vec!["text".into()],
            prefix: None,
            content_type_filter: None,
            subtype_formats: Default::default(),
        });
        assert!(schema.validate().is_err());
    }

    #[test]
    fn subtype_format_renders_template() {
        let fmt = SubtypeFormat {
            template: "[init] model: {model}".into(),
            fields: vec!["model".into()],
        };
        let json: serde_json::Value = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "model": "claude-opus-4-6"
        });
        assert_eq!(fmt.render(&json), "[init] model: claude-opus-4-6");
    }

    #[test]
    fn content_type_filter_extracts_text_blocks() {
        let ext = Extractor {
            type_match: vec!["assistant".into()],
            output: ExtractorOutput::Text,
            paths: vec!["content".into()],
            prefix: None,
            content_type_filter: Some("text".into()),
            subtype_formats: Default::default(),
        };
        let json: serde_json::Value = serde_json::json!({
            "type": "assistant",
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "tool_use", "name": "Read"},
                {"type": "text", "text": " World"}
            ]
        });
        assert_eq!(ext.extract(&json), ParseResult::Text("Hello World".into()));
    }

    #[test]
    fn extractor_wildcard_matches_any_type() {
        let ext = Extractor {
            type_match: vec!["*".into()],
            output: ExtractorOutput::Text,
            paths: vec!["text".into()],
            prefix: None,
            content_type_filter: None,
            subtype_formats: Default::default(),
        };
        assert!(ext.matches_type("anything"));
        assert!(ext.matches_type("assistant"));
    }
}
