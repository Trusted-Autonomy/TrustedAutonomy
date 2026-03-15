//! Schema-driven agent output parsing engine.
//!
//! Replaces hardcoded stream-json parsers with YAML schemas that define how
//! to extract displayable content from each agent's output format. Schemas
//! can be embedded, user-global, or project-local.

mod extractor;
mod loader;
mod schema;

pub use extractor::extract_path;
pub use loader::SchemaLoader;
pub use schema::{
    Extractor, ExtractorOutput, OutputSchema, OutputSchemaError, ParseResult, SchemaFormat,
};

/// Parse a single line of agent output using the given schema.
///
/// Returns `ParseResult::Text(s)` for displayable text, `ParseResult::ToolUse(name)`
/// for tool invocations, `ParseResult::Suppress` for events to hide, `ParseResult::Model(name)`
/// for model identification, and `ParseResult::NotJson` for non-JSON lines.
pub fn parse_line(schema: &OutputSchema, line: &str) -> ParseResult {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return ParseResult::NotJson;
    }

    let json: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return ParseResult::NotJson,
    };

    let event_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

    // Check suppress list first.
    if schema.is_suppressed(event_type) {
        return ParseResult::Suppress;
    }

    // Try each extractor in order.
    for ext in &schema.extractors {
        if ext.matches_type(event_type) {
            return ext.extract(&json);
        }
    }

    // Fallback: if the event has a "type" field but no extractor matched, suppress it
    // (unknown internal event). If no "type" field at all, treat as not-JSON-we-understand.
    if json.get("type").is_some() {
        ParseResult::Suppress
    } else {
        ParseResult::NotJson
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_non_json_returns_not_json() {
        let schema = OutputSchema::passthrough();
        assert_eq!(parse_line(&schema, "plain text"), ParseResult::NotJson);
        assert_eq!(parse_line(&schema, "{invalid json"), ParseResult::NotJson);
    }

    #[test]
    fn parse_with_embedded_claude_code_v2() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("claude-code").unwrap();

        // assistant with nested message.content
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","content":[{"type":"text","text":"Hello world"}]}}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("Hello world".into())
        );

        // content_block_delta
        let line =
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"chunk "}}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("chunk ".into())
        );

        // result
        let line = r#"{"type":"result","result":"done"}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("[result] done".into())
        );

        // tool_use
        let line = r#"{"type":"tool_use","name":"Read"}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::ToolUse("Read".into())
        );

        // content_block_start with tool_use
        let line =
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","name":"Edit"}}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::ToolUse("Edit".into())
        );

        // suppressed events
        assert_eq!(
            parse_line(&schema, r#"{"type":"message_start","message":{}}"#),
            ParseResult::Suppress
        );
        assert_eq!(
            parse_line(&schema, r#"{"type":"ping"}"#),
            ParseResult::Suppress
        );
    }

    #[test]
    fn parse_with_legacy_claude_code_v1() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("claude-code-v1").unwrap();

        // Legacy format: content at top level.
        let line = r#"{"type":"assistant","content":[{"type":"text","text":"Legacy hello"}]}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("Legacy hello".into())
        );
    }

    #[test]
    fn parse_system_init_event() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("claude-code").unwrap();

        let line = r#"{"type":"system","subtype":"init","model":"claude-opus-4-6"}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("[init] model: claude-opus-4-6".into())
        );
    }

    #[test]
    fn parse_system_hook_event() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("claude-code").unwrap();

        let line =
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("[hook] SessionStart:startup...".into())
        );
    }

    #[test]
    fn model_extraction_from_message_start() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("claude-code").unwrap();

        // message_start is suppressed but model extractor should still capture it.
        let line = r#"{"type":"message_start","message":{"model":"claude-sonnet-4-20250514"}}"#;
        // The model extractor checks message_start independently via extract_model.
        let model = schema.extract_model(line);
        assert_eq!(model, Some("claude-sonnet-4-20250514".into()));
    }

    #[test]
    fn passthrough_schema_shows_everything() {
        let schema = OutputSchema::passthrough();
        // Passthrough has no extractors, so typed JSON just gets suppressed.
        // Non-JSON passes through.
        let line = "Hello raw";
        assert_eq!(parse_line(&schema, line), ParseResult::NotJson);
    }

    #[test]
    fn codex_schema_parses_output() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("codex").unwrap();

        let line = r#"{"type":"message","content":"Codex says hello"}"#;
        assert_eq!(
            parse_line(&schema, line),
            ParseResult::Text("Codex says hello".into())
        );
    }
}
