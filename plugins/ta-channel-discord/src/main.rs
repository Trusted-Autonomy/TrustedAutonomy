//! Discord channel plugin for Trusted Autonomy.
//!
//! External JSON-over-stdio plugin that posts agent questions as rich embeds
//! with button components to a Discord channel.
//!
//! ## Modes
//!
//! **Deliver mode** (default): reads one JSON line from stdin, posts to Discord, exits.
//! **Listen mode** (`--listen`): persistent bot that watches for commands in Discord
//! and forwards them to the TA daemon HTTP API.
//!
//! ## Protocol (deliver mode)
//!
//! - Reads one JSON line from stdin: a `ChannelQuestion` object
//! - Posts the question as a Discord embed with buttons to the configured channel
//! - Writes one JSON line to stdout: a `DeliveryResult` object
//!
//! ## Environment Variables
//!
//! - `TA_DISCORD_TOKEN` (or custom via `token_env`): Discord bot token
//! - `TA_DISCORD_CHANNEL_ID`: Discord channel snowflake ID
//! - `TA_DAEMON_URL` (listen mode): daemon URL (default: `http://127.0.0.1:7700`)
//! - `TA_DISCORD_PREFIX` (listen mode): command prefix (default: `ta `)
//!
//! ## Installation
//!
//! 1. Build: `cargo build --release`
//! 2. Copy binary and `channel.toml` to `.ta/plugins/channels/discord/`
//! 3. Set `TA_DISCORD_TOKEN` and `TA_DISCORD_CHANNEL_ID` environment variables

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

mod listener;
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
            channel: "discord".into(),
            delivery_id,
            success: true,
            error: None,
        }
    }

    fn error(msg: String) -> Self {
        Self {
            channel: "discord".into(),
            delivery_id: String::new(),
            success: false,
            error: Some(msg),
        }
    }
}

fn write_result(result: &DeliveryResult) {
    let json = serde_json::to_string(result).unwrap_or_else(|e| {
        format!(
            r#"{{"channel":"discord","delivery_id":"","success":false,"error":"serialization error: {}"}}"#,
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
    // Check for --listen mode.
    let args: Vec<String> = std::env::args().collect();
    let listen_mode = args.iter().any(|a| a == "--listen");

    // Read config from environment.
    let token_env =
        std::env::var("TA_DISCORD_TOKEN_ENV").unwrap_or_else(|_| "TA_DISCORD_TOKEN".into());
    let token = match std::env::var(&token_env) {
        Ok(t) if !t.is_empty() => t,
        _ => {
            if listen_mode {
                eprintln!(
                    "Error: Environment variable '{}' not set. Set it to your Discord bot token.",
                    token_env
                );
            } else {
                write_result(&DeliveryResult::error(format!(
                    "Environment variable '{}' not set. Set it to your Discord bot token.",
                    token_env
                )));
            }
            std::process::exit(1);
        }
    };

    let channel_id = match std::env::var("TA_DISCORD_CHANNEL_ID") {
        Ok(id) if !id.is_empty() => id,
        _ => {
            if listen_mode {
                eprintln!(
                    "Error: Environment variable 'TA_DISCORD_CHANNEL_ID' not set. \
                     Set it to the Discord channel snowflake ID."
                );
            } else {
                write_result(&DeliveryResult::error(
                    "Environment variable 'TA_DISCORD_CHANNEL_ID' not set. \
                     Set it to the Discord channel snowflake ID."
                        .into(),
                ));
            }
            std::process::exit(1);
        }
    };

    // Listen mode: persistent bot that watches for commands.
    if listen_mode {
        let daemon_url =
            std::env::var("TA_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7700".into());
        let prefix = std::env::var("TA_DISCORD_PREFIX").unwrap_or_else(|_| "ta ".into());
        listener::run(&token, &channel_id, &daemon_url, &prefix).await;
        return;
    }

    // Deliver mode: read question from stdin.
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
                if line.len() > 200 {
                    &line[..200]
                } else {
                    &line
                }
            )));
            std::process::exit(1);
        }
    };

    // Build Discord message payload.
    let discord_payload = payload::build_payload(&question);

    // Post to Discord REST API.
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );
    let client = reqwest::Client::new();

    match client
        .post(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&discord_payload)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if status.is_success() {
                        let message_id = json
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        eprintln!(
                            "[discord] Delivered question {} to channel {} (message: {})",
                            question.interaction_id, channel_id, message_id
                        );
                        write_result(&DeliveryResult::success(message_id));
                    } else {
                        let err_msg = json
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        write_result(&DeliveryResult::error(format!(
                            "Discord API error (HTTP {}): '{}' posting question {} to channel {}",
                            status, err_msg, question.interaction_id, channel_id
                        )));
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    write_result(&DeliveryResult::error(format!(
                        "Failed to parse Discord API response: {}",
                        e
                    )));
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            write_result(&DeliveryResult::error(format!(
                "HTTP request to Discord API failed: {} — check network connectivity and bot token",
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
        let result = DeliveryResult::success("msg-123".into());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"delivery_id\":\"msg-123\""));
        assert!(json.contains("\"channel\":\"discord\""));
    }

    #[test]
    fn serialize_error_result() {
        let result = DeliveryResult::error("token missing".into());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("token missing"));
    }
}
