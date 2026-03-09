//! Slack channel plugin for Trusted Autonomy.
//!
//! External JSON-over-stdio plugin that posts agent questions as Slack Block Kit
//! messages with interactive buttons to a Slack channel.
//!
//! ## Protocol
//!
//! - Reads one JSON line from stdin: a `ChannelQuestion` object
//! - Posts the question as a Block Kit message with buttons to the configured channel
//! - Optionally posts context detail as a thread reply
//! - Writes one JSON line to stdout: a `DeliveryResult` object
//!
//! ## Environment Variables
//!
//! - `TA_SLACK_BOT_TOKEN` (required): Slack Bot User OAuth Token (`xoxb-...`)
//! - `TA_SLACK_CHANNEL_ID` (required): Slack channel ID (e.g., `C01ABC23DEF`)
//! - `TA_SLACK_ALLOWED_USERS` (optional): Comma-separated Slack user IDs allowed to respond
//!
//! ## Installation
//!
//! 1. Build: `cargo build --release` (or `ta plugin build slack`)
//! 2. Plugin is auto-installed to `.ta/plugins/channels/slack/`
//! 3. Set `TA_SLACK_BOT_TOKEN` and `TA_SLACK_CHANNEL_ID` environment variables
//! 4. Create a Slack app with `chat:write` scope and install to your workspace

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

mod payload;

/// Input: question from TA daemon via stdin.
#[derive(Debug, Deserialize)]
struct ChannelQuestion {
    interaction_id: String,
    #[allow(dead_code)]
    goal_id: String,
    question: String,
    context: Option<String>,
    response_hint: String,
    #[serde(default)]
    choices: Vec<String>,
    #[serde(default = "default_turn")]
    turn: u32,
    callback_url: String,
}

fn default_turn() -> u32 {
    1
}

/// Output: delivery result written to stdout.
#[derive(Debug, Serialize)]
struct DeliveryResult {
    channel: String,
    delivery_id: String,
    success: bool,
    error: Option<String>,
}

impl DeliveryResult {
    fn success(delivery_id: String) -> Self {
        Self {
            channel: "slack".into(),
            delivery_id,
            success: true,
            error: None,
        }
    }

    fn error(msg: String) -> Self {
        Self {
            channel: "slack".into(),
            delivery_id: String::new(),
            success: false,
            error: Some(msg),
        }
    }
}

fn write_result(result: &DeliveryResult) {
    let json = serde_json::to_string(result).unwrap_or_else(|e| {
        format!(
            r#"{{"channel":"slack","delivery_id":"","success":false,"error":"serialization error: {}"}}"#,
            e
        )
    });
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", json);
    let _ = out.flush();
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Read config from environment.
    let token = match std::env::var("TA_SLACK_BOT_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            write_result(&DeliveryResult::error(
                "Environment variable 'TA_SLACK_BOT_TOKEN' not set. \
                 Set it to your Slack Bot User OAuth Token (xoxb-...)."
                    .into(),
            ));
            std::process::exit(1);
        }
    };

    let channel_id = match std::env::var("TA_SLACK_CHANNEL_ID") {
        Ok(id) if !id.is_empty() => id,
        _ => {
            write_result(&DeliveryResult::error(
                "Environment variable 'TA_SLACK_CHANNEL_ID' not set. \
                 Set it to the Slack channel ID (e.g., C01ABC23DEF)."
                    .into(),
            ));
            std::process::exit(1);
        }
    };

    // Read question from stdin.
    let stdin = io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(line)) if !line.trim().is_empty() => line,
        _ => {
            write_result(&DeliveryResult::error(
                "No input received on stdin. Expected a JSON-encoded ChannelQuestion.".into(),
            ));
            std::process::exit(1);
        }
    };

    let question: ChannelQuestion = match serde_json::from_str(&line) {
        Ok(q) => q,
        Err(e) => {
            write_result(&DeliveryResult::error(format!(
                "Invalid JSON input: {}. Got: '{}'",
                e,
                if line.len() > 200 { &line[..200] } else { &line }
            )));
            std::process::exit(1);
        }
    };

    // Build Slack Block Kit payload.
    let slack_payload = payload::build_payload(&question, &channel_id);

    // Post to Slack Web API (chat.postMessage).
    let client = reqwest::Client::new();

    match client
        .post("https://slack.com/api/chat.postMessage")
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&slack_payload)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    let ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    if status.is_success() && ok {
                        let ts = json
                            .get("ts")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        eprintln!(
                            "[slack] Delivered question {} to channel {} (ts: {})",
                            question.interaction_id, channel_id, ts
                        );

                        // Post context as thread reply if present and long.
                        if let Some(ctx) = &question.context {
                            if ctx.len() > 500 && !ts.is_empty() {
                                let thread_payload = payload::build_thread_detail(
                                    &channel_id,
                                    &ts,
                                    ctx,
                                );
                                // Best-effort thread reply — don't fail the delivery if it errors.
                                match client
                                    .post("https://slack.com/api/chat.postMessage")
                                    .header("Authorization", format!("Bearer {}", token))
                                    .header(
                                        "Content-Type",
                                        "application/json; charset=utf-8",
                                    )
                                    .json(&thread_payload)
                                    .send()
                                    .await
                                {
                                    Ok(thread_resp) => {
                                        if let Ok(thread_json) =
                                            thread_resp.json::<serde_json::Value>().await
                                        {
                                            let thread_ok = thread_json
                                                .get("ok")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            if thread_ok {
                                                eprintln!(
                                                    "[slack] Posted detail thread reply to ts {}",
                                                    ts
                                                );
                                            } else {
                                                let err = thread_json
                                                    .get("error")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("unknown");
                                                eprintln!(
                                                    "[slack] Thread reply warning: {}",
                                                    err
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[slack] Thread reply network error (non-fatal): {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }

                        write_result(&DeliveryResult::success(ts));
                    } else {
                        let err_msg = json
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        write_result(&DeliveryResult::error(format!(
                            "Slack API error: '{}' posting question {} to channel {}. \
                             Check bot token permissions (needs chat:write scope) \
                             and channel membership.",
                            err_msg, question.interaction_id, channel_id
                        )));
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    write_result(&DeliveryResult::error(format!(
                        "Failed to parse Slack API response: {}",
                        e
                    )));
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            write_result(&DeliveryResult::error(format!(
                "HTTP request to Slack API failed: {} — check network connectivity and bot token",
                e
            )));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_channel_question() {
        let json = r#"{
            "interaction_id": "550e8400-e29b-41d4-a716-446655440000",
            "goal_id": "660e8400-e29b-41d4-a716-446655440000",
            "question": "Which database?",
            "context": "Setting up backend",
            "response_hint": "choice",
            "choices": ["PostgreSQL", "SQLite"],
            "turn": 1,
            "callback_url": "http://localhost:7700"
        }"#;
        let q: ChannelQuestion = serde_json::from_str(json).unwrap();
        assert_eq!(q.question, "Which database?");
        assert_eq!(q.choices.len(), 2);
        assert_eq!(q.turn, 1);
    }

    #[test]
    fn deserialize_minimal_question() {
        let json = r#"{
            "interaction_id": "550e8400-e29b-41d4-a716-446655440000",
            "goal_id": "660e8400-e29b-41d4-a716-446655440000",
            "question": "Continue?",
            "response_hint": "yes_no",
            "callback_url": "http://localhost:7700"
        }"#;
        let q: ChannelQuestion = serde_json::from_str(json).unwrap();
        assert_eq!(q.question, "Continue?");
        assert!(q.context.is_none());
        assert!(q.choices.is_empty());
        assert_eq!(q.turn, 1);
    }

    #[test]
    fn serialize_delivery_result() {
        let result = DeliveryResult::success("1234567890.123456".into());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"delivery_id\":\"1234567890.123456\""));
        assert!(json.contains("\"channel\":\"slack\""));
    }

    #[test]
    fn serialize_error_result() {
        let result = DeliveryResult::error("token missing".into());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("token missing"));
    }
}
