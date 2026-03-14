//! Discord Gateway listener — connects to Discord WebSocket, watches for
//! messages with a configurable prefix, and forwards them to the TA daemon
//! HTTP API (`POST /api/cmd`).
//!
//! This is a quick integration for dev use. Known tech debt (tracked in
//! PLAN.md v0.12.1):
//! - No slash command registration (uses message prefix matching)
//! - No interaction callback handling (button clicks from deliver_question)
//! - No reconnect backoff / resume (reconnects from scratch)
//! - Hardcoded intents (GUILD_MESSAGES + MESSAGE_CONTENT)
//! - No rate limiting on command forwarding

use std::path::PathBuf;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Discord Gateway op codes.
const OP_DISPATCH: u64 = 0;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;

/// PID file path for the listener (prevents duplicate instances).
fn pid_file_path() -> PathBuf {
    // Use .ta/ if it exists (running from project root), otherwise /tmp.
    let ta_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".ta"))
        .filter(|d| d.is_dir());
    match ta_dir {
        Some(dir) => dir.join("discord-listener.pid"),
        None => std::env::temp_dir().join("ta-discord-listener.pid"),
    }
}

/// Check if another listener is already running. Returns Ok(()) if clear to proceed.
fn acquire_pid_lock() -> Result<(), String> {
    let pid_path = pid_file_path();
    if pid_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                // Check if the process is still alive.
                #[cfg(unix)]
                {
                    use std::process::Command;
                    if Command::new("kill")
                        .args(["-0", &pid.to_string()])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                    {
                        return Err(format!(
                            "Another Discord listener is already running (PID {}). \
                             Stop it first, or remove {}",
                            pid,
                            pid_path.display()
                        ));
                    }
                }
                #[cfg(windows)]
                {
                    use std::process::Command;
                    if Command::new("tasklist")
                        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
                        .output()
                        .map(|o| {
                            let stdout = String::from_utf8_lossy(&o.stdout);
                            stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
                        })
                        .unwrap_or(false)
                    {
                        return Err(format!(
                            "Another Discord listener is already running (PID {}). \
                             Stop it first, or remove {}",
                            pid,
                            pid_path.display()
                        ));
                    }
                }
            }
        }
        // Stale PID file — remove it.
        eprintln!(
            "[discord-listener] Removing stale PID file: {}",
            pid_path.display()
        );
        let _ = std::fs::remove_file(&pid_path);
    }

    // Write our PID.
    let pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, pid.to_string()) {
        return Err(format!(
            "Cannot write PID file {}: {}",
            pid_path.display(),
            e
        ));
    }
    eprintln!(
        "[discord-listener] PID file: {} ({})",
        pid_path.display(),
        pid
    );
    Ok(())
}

/// Remove PID file on shutdown.
fn release_pid_lock() {
    let pid_path = pid_file_path();
    let _ = std::fs::remove_file(&pid_path);
}

/// Run the persistent listener loop.
pub async fn run(token: &str, channel_id: &str, daemon_url: &str, prefix: &str) {
    if let Err(e) = acquire_pid_lock() {
        eprintln!("[discord-listener] {}", e);
        std::process::exit(1);
    }

    eprintln!("[discord-listener] Connecting to Discord Gateway...");
    eprintln!(
        "[discord-listener] Watching channel {} for prefix {:?}",
        channel_id, prefix
    );
    eprintln!(
        "[discord-listener] Forwarding commands to {}/api/cmd",
        daemon_url
    );

    // Ensure PID file is cleaned up on exit.
    let result = async {
        loop {
            match run_session(token, channel_id, daemon_url, prefix).await {
                Ok(()) => {
                    eprintln!("[discord-listener] Session ended cleanly. Reconnecting in 5s...");
                }
                Err(e) => {
                    eprintln!(
                        "[discord-listener] Session error: {}. Reconnecting in 5s...",
                        e
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
    .await;

    release_pid_lock();
    result
}

async fn run_session(
    token: &str,
    channel_id: &str,
    daemon_url: &str,
    prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(GATEWAY_URL).await?;
    let (mut write, mut read) = ws_stream.split();

    // Wait for Hello (op 10) to get heartbeat interval.
    let hello = read.next().await.ok_or("Gateway closed before Hello")??;
    let hello_json: serde_json::Value = serde_json::from_str(hello.to_text()?)?;
    let heartbeat_interval = hello_json["d"]["heartbeat_interval"]
        .as_u64()
        .unwrap_or(41250);

    eprintln!(
        "[discord-listener] Connected. Heartbeat interval: {}ms",
        heartbeat_interval
    );

    // Send Identify.
    // Intents: GUILDS (1) + GUILD_MESSAGES (512) + MESSAGE_CONTENT (32768) = 33281
    let identify = json!({
        "op": 2,
        "d": {
            "token": token,
            "intents": 33281,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "ta-channel-discord",
                "device": "ta-channel-discord"
            }
        }
    });
    write.send(Message::Text(identify.to_string())).await?;

    let mut sequence: Option<u64> = None;
    let mut heartbeat_timer =
        tokio::time::interval(std::time::Duration::from_millis(heartbeat_interval));
    // Skip the first immediate tick.
    heartbeat_timer.tick().await;

    let http_client = reqwest::Client::new();
    let cmd_url = format!("{}/api/cmd", daemon_url);

    // Track our own user ID to ignore our own messages.
    let mut self_user_id: Option<String> = None;

    loop {
        tokio::select! {
            _ = heartbeat_timer.tick() => {
                let hb = json!({ "op": 1, "d": sequence });
                write.send(Message::Text(hb.to_string())).await?;
            }
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()), // stream ended
                };

                if msg.is_close() {
                    return Ok(());
                }

                let text = match msg.to_text() {
                    Ok(t) => t,
                    Err(_) => continue, // skip binary frames
                };

                let event: serde_json::Value = match serde_json::from_str(text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let op = event["op"].as_u64().unwrap_or(99);

                match op {
                    OP_DISPATCH => {
                        if let Some(s) = event["s"].as_u64() {
                            sequence = Some(s);
                        }

                        let event_name = event["t"].as_str().unwrap_or("");

                        // Capture our own user ID from READY.
                        if event_name == "READY" {
                            if let Some(id) = event["d"]["user"]["id"].as_str() {
                                self_user_id = Some(id.to_string());
                                eprintln!("[discord-listener] Ready as user {}", id);
                            }
                        }

                        if event_name == "MESSAGE_CREATE" {
                            let d = &event["d"];
                            let msg_channel = d["channel_id"].as_str().unwrap_or("");
                            let content = d["content"].as_str().unwrap_or("");
                            let author_id = d["author"]["id"].as_str().unwrap_or("");
                            let author_bot = d["author"]["bot"].as_bool().unwrap_or(false);
                            let author_name = d["author"]["username"].as_str().unwrap_or("?");

                            // Skip: wrong channel, bot messages, our own messages.
                            if msg_channel != channel_id {
                                continue;
                            }
                            if author_bot {
                                continue;
                            }
                            if let Some(ref self_id) = self_user_id {
                                if author_id == self_id {
                                    continue;
                                }
                            }

                            // Check for command prefix.
                            let command = if let Some(cmd) = content.strip_prefix(prefix) {
                                cmd.trim()
                            } else {
                                continue;
                            };

                            if command.is_empty() {
                                continue;
                            }

                            eprintln!(
                                "[discord-listener] Command from {}: {}",
                                author_name, command
                            );

                            // Forward to daemon.
                            let response = forward_command(
                                &http_client,
                                &cmd_url,
                                command,
                                channel_id,
                                token,
                                d["id"].as_str().unwrap_or(""),
                            )
                            .await;

                            // Post response back to Discord.
                            let reply = format_reply(&response);
                            let _ = post_message(
                                &http_client,
                                token,
                                channel_id,
                                &reply,
                            )
                            .await;
                        }
                    }
                    OP_HEARTBEAT_ACK => {
                        // Good — server acknowledged our heartbeat.
                    }
                    OP_HELLO => {
                        // Shouldn't happen after initial Hello, ignore.
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[discord-listener] Shutting down...");
                release_pid_lock();
                let close = Message::Close(None);
                let _ = write.send(close).await;
                return Ok(());
            }
        }
    }
}

/// Forward a command to the TA daemon HTTP API.
async fn forward_command(
    client: &reqwest::Client,
    cmd_url: &str,
    command: &str,
    _channel_id: &str,
    _token: &str,
    _message_id: &str,
) -> CommandResult {
    // Prepend "ta " if not already present.
    let full_command = if command.starts_with("ta ") {
        command.to_string()
    } else {
        format!("ta {}", command)
    };

    match client
        .post(cmd_url)
        .json(&json!({ "command": full_command }))
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if status.is_success() {
                        let exit_code = json["exit_code"].as_i64().unwrap_or(-1);
                        let stdout = json["stdout"].as_str().unwrap_or("").to_string();
                        let stderr = json["stderr"].as_str().unwrap_or("").to_string();
                        CommandResult {
                            success: exit_code == 0,
                            output: if stdout.is_empty() { stderr } else { stdout },
                            error: None,
                        }
                    } else {
                        let err = json["error"]
                            .as_str()
                            .unwrap_or("unknown error")
                            .to_string();
                        CommandResult {
                            success: false,
                            output: String::new(),
                            error: Some(err),
                        }
                    }
                }
                Err(e) => CommandResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse daemon response: {}", e)),
                },
            }
        }
        Err(e) => CommandResult {
            success: false,
            output: String::new(),
            error: Some(format!("Cannot reach daemon: {}", e)),
        },
    }
}

struct CommandResult {
    success: bool,
    output: String,
    error: Option<String>,
}

/// Format command result as a Discord message.
fn format_reply(result: &CommandResult) -> String {
    if let Some(ref err) = result.error {
        format!("**Error:** {}", err)
    } else {
        let status = if result.success { "ok" } else { "failed" };
        let output = truncate_output(&result.output, 1900);
        if output.is_empty() {
            format!("**[{}]** (no output)", status)
        } else {
            format!("**[{}]**\n```\n{}\n```", status, output)
        }
    }
}

/// Truncate output to fit in a Discord message (2000 char limit).
fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...\n(truncated)", &s[..max])
    }
}

/// Post a text message to a Discord channel.
async fn post_message(
    client: &reqwest::Client,
    token: &str,
    channel_id: &str,
    content: &str,
) -> Result<(), reqwest::Error> {
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );
    client
        .post(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&json!({ "content": content }))
        .send()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_reply_success_with_output() {
        let result = CommandResult {
            success: true,
            output: "Draft list:\n  1. Fix auth bug".into(),
            error: None,
        };
        let reply = format_reply(&result);
        assert!(reply.contains("**[ok]**"));
        assert!(reply.contains("Fix auth bug"));
    }

    #[test]
    fn format_reply_success_no_output() {
        let result = CommandResult {
            success: true,
            output: String::new(),
            error: None,
        };
        let reply = format_reply(&result);
        assert!(reply.contains("(no output)"));
    }

    #[test]
    fn format_reply_error() {
        let result = CommandResult {
            success: false,
            output: String::new(),
            error: Some("command not permitted".into()),
        };
        let reply = format_reply(&result);
        assert!(reply.contains("**Error:**"));
        assert!(reply.contains("command not permitted"));
    }

    #[test]
    fn format_reply_failed_command() {
        let result = CommandResult {
            success: false,
            output: "error: no such goal".into(),
            error: None,
        };
        let reply = format_reply(&result);
        assert!(reply.contains("**[failed]**"));
        assert!(reply.contains("no such goal"));
    }

    #[test]
    fn truncate_output_short() {
        assert_eq!(truncate_output("hello", 100), "hello");
    }

    #[test]
    fn truncate_output_long() {
        let long = "x".repeat(2000);
        let result = truncate_output(&long, 100);
        assert!(result.len() < 200);
        assert!(result.contains("(truncated)"));
    }
}
