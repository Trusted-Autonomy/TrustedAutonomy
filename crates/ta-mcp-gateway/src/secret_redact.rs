//! Secret redaction for audit artifacts written to disk: agent transcripts,
//! the extracted list of TA MCP tool calls, and wake-on-demand launch logs.
//!
//! Two layers:
//! - JSON values: any object key that names a credential (`token`,
//!   `api_key`, `password`, `authorization`, ...) has its value replaced.
//!   Keys are matched as whole names, so usage counters such as
//!   `input_tokens` are left alone.
//! - Strings: well-known credential shapes (`sk-ant-...`, `ghp_...`,
//!   `github_pat_...`, `xox?-...`, `Bearer ...`) are replaced wherever they
//!   appear, including inside free text.

use serde_json::Value;

/// Replacement for a redacted value.
pub const REDACTED: &str = "[REDACTED]";

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase().replace('-', "_");
    const EXACT: &[&str] = &[
        "token",
        "access_token",
        "refresh_token",
        "id_token",
        "session_token",
        "bearer_token",
        "auth_token",
        "api_key",
        "apikey",
        "x_api_key",
        "password",
        "passwd",
        "passphrase",
        "secret",
        "client_secret",
        "authorization",
        "cookie",
        "set_cookie",
        "private_key",
        "credential",
        "credentials",
    ];
    EXACT.contains(&k.as_str())
        || k.ends_with("_secret")
        || k.ends_with("_password")
        || k.ends_with("_api_key")
        || k.ends_with("_private_key")
}

/// Prefixes of credential strings worth catching anywhere in text.
const SECRET_PREFIXES: &[&str] = &[
    "sk-ant-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// Redact credential-shaped substrings inside free text.
pub fn redact_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        // "Bearer <token>" (case-insensitive "bearer ").
        if rest.len() >= 7 && rest.is_char_boundary(7) && rest[..7].eq_ignore_ascii_case("bearer ")
        {
            let after = &rest[7..];
            let n = after
                .find(|c: char| !is_token_char(c))
                .unwrap_or(after.len());
            if n >= 8 {
                out.push_str(&rest[..7]);
                out.push_str(REDACTED);
                rest = &after[n..];
                continue;
            }
        }
        for p in SECRET_PREFIXES {
            if rest.starts_with(p) {
                let n = rest.find(|c: char| !is_token_char(c)).unwrap_or(rest.len());
                if n > p.len() + 8 {
                    out.push_str(REDACTED);
                    rest = &rest[n..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Redact a JSON value in place (see module doc).
pub fn redact_json(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if is_secret_key(k) && !val.is_null() {
                    *val = Value::String(REDACTED.to_string());
                } else {
                    redact_json(val);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::String(s) => {
            let r = redact_text(s);
            if r != *s {
                *s = r;
            }
        }
        _ => {}
    }
}

/// Redact one line of output: parsed and redacted as JSON when it is JSON
/// (stream-json transcripts), else redacted as text.
pub fn redact_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(mut v) if v.is_object() || v.is_array() => {
            redact_json(&mut v);
            v.to_string()
        }
        _ => redact_text(line),
    }
}

/// Redact every line of a multi-line blob (keeps line structure).
pub fn redact_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    out
}

/// One TA MCP tool call found in a Claude Code stream-json transcript.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRecord {
    /// Full tool name as the agent called it, e.g. `mcp__ta__ta_whiteboard_outcome_send`.
    pub tool: String,
    /// The tool-use id, to match with its result in the transcript.
    pub id: Option<String>,
    /// Arguments, with secrets redacted.
    pub input: Value,
}

/// Extract the TA MCP tool calls (`mcp__ta__*`) from one stream-json line.
/// Claude Code emits each assistant turn as
/// `{"type":"assistant","message":{"content":[{"type":"tool_use","name":...,"input":...}]}}`.
pub fn ta_tool_calls_in_line(line: &str) -> Vec<ToolCallRecord> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(content) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            if !name.starts_with("mcp__ta__") {
                return None;
            }
            let mut input = item.get("input").cloned().unwrap_or(Value::Null);
            redact_json(&mut input);
            Some(ToolCallRecord {
                tool: name.to_string(),
                id: item.get("id").and_then(|i| i.as_str()).map(str::to_string),
                input,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_keys_are_redacted_but_token_counters_are_not() {
        let mut v = serde_json::json!({
            "token": "tok-123",
            "nested": {"api_key": "k", "Authorization": "x", "client_secret": "s"},
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "list": [{"password": "p"}],
        });
        redact_json(&mut v);
        assert_eq!(v["token"], REDACTED);
        assert_eq!(v["nested"]["api_key"], REDACTED);
        assert_eq!(v["nested"]["Authorization"], REDACTED);
        assert_eq!(v["nested"]["client_secret"], REDACTED);
        assert_eq!(v["list"][0]["password"], REDACTED);
        assert_eq!(v["usage"]["input_tokens"], 10);
    }

    #[test]
    fn credential_shapes_in_text_are_redacted() {
        let s = "key sk-ant-api03-abcdefghijklmnop and Bearer abcdefghijkl1234 ok";
        let r = redact_text(s);
        assert!(!r.contains("abcdefghijklmnop"), "{r}");
        assert!(!r.contains("abcdefghijkl1234"), "{r}");
        assert!(r.contains("Bearer [REDACTED]"), "{r}");
        assert!(r.ends_with(" ok"));
        assert_eq!(
            redact_text("plain text, no secrets"),
            "plain text, no secrets"
        );
    }

    #[test]
    fn extracts_only_ta_tool_calls_with_redacted_arguments() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "text", "text": "sending outcome"},
                {"type": "tool_use", "id": "tu_1", "name": "mcp__ta__ta_whiteboard_outcome_send",
                 "input": {"candidate_id": "c1", "outcome": "done", "token": "tok-secret"}},
                {"type": "tool_use", "id": "tu_2", "name": "Read", "input": {"file_path": "/x"}},
            ]}
        })
        .to_string();
        let calls = ta_tool_calls_in_line(&line);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "mcp__ta__ta_whiteboard_outcome_send");
        assert_eq!(calls[0].id.as_deref(), Some("tu_1"));
        assert_eq!(calls[0].input["candidate_id"], "c1");
        assert_eq!(calls[0].input["token"], REDACTED);
    }

    #[test]
    fn redact_line_handles_json_and_text() {
        assert_eq!(
            redact_line(r#"{"password":"p"}"#),
            r#"{"password":"[REDACTED]"}"#
        );
        assert_eq!(redact_line("not json"), "not json");
    }
}
