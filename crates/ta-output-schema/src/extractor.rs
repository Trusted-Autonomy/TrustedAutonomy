//! Generic path extractor for JSON values.
//!
//! Supports dotted paths like `message.content[].text` to navigate:
//! - Object fields: `message.model`
//! - Array iteration: `content[]` — collects results from all array items
//! - Nested combinations: `message.content[].text`
//! - Fallback alternatives via multiple paths (caller tries each)

use serde_json::Value;

/// Extract a value from a JSON object using a dotted path.
///
/// Path syntax:
/// - `field` — access object field
/// - `field1.field2` — nested object access
/// - `field[]` — iterate array, return array of results
/// - `field[].subfield` — iterate array, extract subfield from each item
///
/// Returns `None` if the path doesn't match the structure.
pub fn extract_path(json: &Value, path: &str) -> Option<Value> {
    let segments = parse_path(path);
    extract_segments(json, &segments)
}

/// A parsed path segment.
#[derive(Debug, Clone, PartialEq)]
enum Segment {
    /// Access a named field.
    Field(String),
    /// Iterate an array (the named field is an array).
    ArrayIter(String),
}

/// Parse a dotted path into segments.
fn parse_path(path: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    for part in path.split('.') {
        if let Some(field) = part.strip_suffix("[]") {
            segments.push(Segment::ArrayIter(field.to_string()));
        } else {
            segments.push(Segment::Field(part.to_string()));
        }
    }
    segments
}

/// Recursively extract a value following the given segments.
fn extract_segments(json: &Value, segments: &[Segment]) -> Option<Value> {
    if segments.is_empty() {
        return Some(json.clone());
    }

    let (seg, rest) = (&segments[0], &segments[1..]);

    match seg {
        Segment::Field(name) => {
            let child = json.get(name.as_str())?;
            if child.is_null() {
                return None;
            }
            extract_segments(child, rest)
        }
        Segment::ArrayIter(name) => {
            let arr = if name.is_empty() {
                // Bare `[]` — current value should be an array.
                json.as_array()?
            } else {
                json.get(name.as_str())?.as_array()?
            };

            if rest.is_empty() {
                // Return the array itself.
                return Some(Value::Array(arr.clone()));
            }

            // Extract from each array item and collect results.
            let results: Vec<Value> = arr
                .iter()
                .filter_map(|item| extract_segments(item, rest))
                .collect();

            if results.is_empty() {
                None
            } else if results.len() == 1 {
                Some(results.into_iter().next().unwrap())
            } else {
                Some(Value::Array(results))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simple_field() {
        let json = json!({"name": "test"});
        assert_eq!(extract_path(&json, "name"), Some(json!("test")));
    }

    #[test]
    fn nested_field() {
        let json = json!({"message": {"model": "claude-opus-4-6"}});
        assert_eq!(
            extract_path(&json, "message.model"),
            Some(json!("claude-opus-4-6"))
        );
    }

    #[test]
    fn missing_field_returns_none() {
        let json = json!({"name": "test"});
        assert_eq!(extract_path(&json, "missing"), None);
    }

    #[test]
    fn nested_missing_returns_none() {
        let json = json!({"message": {}});
        assert_eq!(extract_path(&json, "message.model"), None);
    }

    #[test]
    fn array_iteration() {
        let json = json!({
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "text", "text": " World"}
            ]
        });
        assert_eq!(
            extract_path(&json, "content[].text"),
            Some(json!(["Hello", " World"]))
        );
    }

    #[test]
    fn array_iteration_single_item() {
        let json = json!({
            "content": [{"type": "text", "text": "Only one"}]
        });
        assert_eq!(
            extract_path(&json, "content[].text"),
            Some(json!("Only one"))
        );
    }

    #[test]
    fn deeply_nested_array() {
        let json = json!({
            "message": {
                "content": [
                    {"type": "text", "text": "Nested"}
                ]
            }
        });
        assert_eq!(
            extract_path(&json, "message.content[].text"),
            Some(json!("Nested"))
        );
    }

    #[test]
    fn null_field_returns_none() {
        let json = json!({"field": null});
        assert_eq!(extract_path(&json, "field"), None);
    }

    #[test]
    fn content_block_name() {
        let json = json!({
            "content_block": {"type": "tool_use", "name": "Edit"}
        });
        assert_eq!(
            extract_path(&json, "content_block.name"),
            Some(json!("Edit"))
        );
    }

    #[test]
    fn delta_text() {
        let json = json!({
            "delta": {"type": "text_delta", "text": "chunk"}
        });
        assert_eq!(extract_path(&json, "delta.text"), Some(json!("chunk")));
    }

    #[test]
    fn top_level_result_string() {
        let json = json!({"type": "result", "result": "Task completed"});
        assert_eq!(extract_path(&json, "result"), Some(json!("Task completed")));
    }
}
