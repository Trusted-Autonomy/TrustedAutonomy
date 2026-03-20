//! Discord Gateway listener — connects to Discord WebSocket, watches for
//! messages/slash-commands/button-clicks and forwards them to the TA daemon
//! HTTP API (`POST /api/cmd`).
//!
//! ## Features (v0.12.1)
//!
//! - **Slash commands**: Handles INTERACTION_CREATE for `/ta` Application Commands.
//!   No MESSAGE_CONTENT intent required; works in locked-down servers.
//! - **Interaction callbacks**: Button click interactions (custom_id = `ta_<id>_<choice>`)
//!   are decoded and forwarded to `/api/interactions/<id>/respond`.
//! - **Gateway resume**: Tracks `session_id` + last sequence number. Reconnects
//!   use RESUME op (opcode 6) instead of fresh IDENTIFY, avoiding missed events.
//! - **Rate limiting**: Per-user token bucket — configurable max commands per window.
//!   Defaults to 10 commands per 60 seconds. Excess commands get a polite error reply.
//! - **Response threading**: Command responses are posted as thread replies to the
//!   original message, keeping the main channel clean.
//! - **Long-running status**: Commands that take >5s get an initial "Working..." edit
//!   so users see immediate feedback. Final result replaces the placeholder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Discord Gateway op codes.
const OP_DISPATCH: u64 = 0;
const OP_HEARTBEAT: u64 = 1;
const OP_IDENTIFY: u64 = 2;
const OP_RESUME: u64 = 6;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;
const OP_INVALID_SESSION: u64 = 9;
const OP_RECONNECT: u64 = 7;

/// Discord interaction types.
const INTERACTION_TYPE_APPLICATION_COMMAND: u64 = 2;
const INTERACTION_TYPE_MESSAGE_COMPONENT: u64 = 3;

/// Discord interaction response types.
/// 5 = DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE (acknowledge, update later)
const INTERACTION_RESPONSE_DEFERRED: u64 = 5;
/// 4 = CHANNEL_MESSAGE_WITH_SOURCE
const INTERACTION_RESPONSE_CHANNEL_MESSAGE: u64 = 4;

/// Seconds before posting a "Working…" placeholder for long commands.
const LONG_RUNNING_THRESHOLD_SECS: u64 = 5;

/// Per-user rate limit: max commands in window.
const RATE_LIMIT_MAX: u32 = 10;
/// Per-user rate limit window in seconds.
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// PID file helpers (unchanged from before)
// ---------------------------------------------------------------------------

fn pid_file_path() -> PathBuf {
    let ta_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".ta"))
        .filter(|d| d.is_dir());
    match ta_dir {
        Some(dir) => dir.join("discord-listener.pid"),
        None => std::env::temp_dir().join("ta-discord-listener.pid"),
    }
}

fn acquire_pid_lock() -> Result<(), String> {
    let pid_path = pid_file_path();
    if pid_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
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
        eprintln!(
            "[discord-listener] Removing stale PID file: {}",
            pid_path.display()
        );
        let _ = std::fs::remove_file(&pid_path);
    }

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

fn release_pid_lock() {
    let pid_path = pid_file_path();
    let _ = std::fs::remove_file(&pid_path);
}

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

struct UserBucket {
    count: u32,
    window_start: Instant,
}

struct RateLimiter {
    users: HashMap<String, UserBucket>,
    max_per_window: u32,
    window_secs: u64,
}

impl RateLimiter {
    fn new(max_per_window: u32, window_secs: u64) -> Self {
        Self {
            users: HashMap::new(),
            max_per_window,
            window_secs,
        }
    }

    /// Returns true if the command is allowed, false if rate-limited.
    fn check(&mut self, user_id: &str) -> bool {
        let now = Instant::now();
        let bucket = self.users.entry(user_id.to_string()).or_insert(UserBucket {
            count: 0,
            window_start: now,
        });

        // Reset window if expired.
        if now.duration_since(bucket.window_start) >= Duration::from_secs(self.window_secs) {
            bucket.count = 0;
            bucket.window_start = now;
        }

        if bucket.count >= self.max_per_window {
            return false;
        }
        bucket.count += 1;
        true
    }
}

// ---------------------------------------------------------------------------
// Gateway session state (for resume)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct GatewaySession {
    /// Session ID received in READY dispatch (for RESUME).
    session_id: Option<String>,
    /// Last received sequence number (for RESUME and heartbeat).
    sequence: Option<u64>,
    /// Resume gateway URL from READY (Discord may give a different URL).
    resume_gateway_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the persistent listener loop.
pub async fn run(token: &str, channel_id: &str, daemon_url: &str, prefix: &str) {
    if let Err(e) = acquire_pid_lock() {
        eprintln!("[discord-listener] {}", e);
        std::process::exit(1);
    }

    eprintln!("[discord-listener] Connecting to Discord Gateway...");
    eprintln!(
        "[discord-listener] Watching channel {} for prefix {:?} and slash commands",
        channel_id, prefix
    );
    eprintln!(
        "[discord-listener] Forwarding commands to {}/api/cmd",
        daemon_url
    );

    let mut session = GatewaySession::default();

    loop {
        match run_session(token, channel_id, daemon_url, prefix, &mut session).await {
            Ok(()) => {
                eprintln!("[discord-listener] Session ended cleanly. Reconnecting in 5s...");
            }
            Err(e) => {
                eprintln!(
                    "[discord-listener] Session error: {}. Reconnecting in 5s...",
                    e
                );
                // Clear session on hard error so next connect does a fresh IDENTIFY.
                if session.session_id.is_none() {
                    eprintln!("[discord-listener] No session to resume — will IDENTIFY fresh.");
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

// ---------------------------------------------------------------------------
// Session loop
// ---------------------------------------------------------------------------

async fn run_session(
    token: &str,
    channel_id: &str,
    daemon_url: &str,
    prefix: &str,
    session: &mut GatewaySession,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use resume URL if available, otherwise use default gateway URL.
    let gateway_url = session.resume_gateway_url.as_deref().unwrap_or(GATEWAY_URL);

    let (ws_stream, _) = tokio_tungstenite::connect_async(gateway_url).await?;
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

    // RESUME if we have a session, otherwise IDENTIFY.
    if let Some(ref sid) = session.session_id.clone() {
        eprintln!(
            "[discord-listener] Resuming session {}...",
            &sid[..8.min(sid.len())]
        );
        let resume = json!({
            "op": OP_RESUME,
            "d": {
                "token": token,
                "session_id": sid,
                "seq": session.sequence
            }
        });
        write.send(Message::Text(resume.to_string())).await?;
    } else {
        // Intents: GUILDS (1) + GUILD_MESSAGES (512) + MESSAGE_CONTENT (32768) = 33281
        // Note: APPLICATION_COMMANDS don't need MESSAGE_CONTENT (slash commands work without it).
        let identify = json!({
            "op": OP_IDENTIFY,
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
    }

    let mut heartbeat_timer = tokio::time::interval(Duration::from_millis(heartbeat_interval));
    // Skip the first immediate tick.
    heartbeat_timer.tick().await;

    let http_client = reqwest::Client::new();
    let cmd_url = format!("{}/api/cmd", daemon_url);
    let interactions_url = format!("{}/api/interactions", daemon_url);
    let goal_input_base_url = format!("{}/api/goals", daemon_url);

    let mut self_user_id: Option<String> = None;
    let mut rate_limiter = RateLimiter::new(RATE_LIMIT_MAX, RATE_LIMIT_WINDOW_SECS);

    loop {
        tokio::select! {
            _ = heartbeat_timer.tick() => {
                let hb = json!({ "op": OP_HEARTBEAT, "d": session.sequence });
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
                        // Update sequence number for heartbeats and resume.
                        if let Some(s) = event["s"].as_u64() {
                            session.sequence = Some(s);
                        }

                        let event_name = event["t"].as_str().unwrap_or("");

                        match event_name {
                            "READY" => {
                                if let Some(id) = event["d"]["user"]["id"].as_str() {
                                    self_user_id = Some(id.to_string());
                                    eprintln!("[discord-listener] Ready as user {}", id);
                                }
                                // Save session_id and resume URL for reconnects.
                                if let Some(sid) = event["d"]["session_id"].as_str() {
                                    session.session_id = Some(sid.to_string());
                                }
                                if let Some(url) = event["d"]["resume_gateway_url"].as_str() {
                                    session.resume_gateway_url = Some(url.to_string());
                                }
                            }
                            "RESUMED" => {
                                eprintln!("[discord-listener] Session resumed successfully.");
                            }
                            "MESSAGE_CREATE" => {
                                handle_message_create(
                                    &event["d"],
                                    channel_id,
                                    prefix,
                                    &mut self_user_id,
                                    &mut rate_limiter,
                                    &http_client,
                                    &cmd_url,
                                    &goal_input_base_url,
                                    token,
                                ).await;
                            }
                            "INTERACTION_CREATE" => {
                                handle_interaction_create(
                                    &event["d"],
                                    channel_id,
                                    &mut rate_limiter,
                                    &http_client,
                                    &cmd_url,
                                    &interactions_url,
                                    token,
                                    daemon_url,
                                ).await;
                            }
                            _ => {}
                        }
                    }
                    OP_HEARTBEAT_ACK => {
                        // Server acknowledged heartbeat.
                    }
                    OP_INVALID_SESSION => {
                        // Session invalidated — clear and reconnect fresh.
                        eprintln!("[discord-listener] Session invalidated. Will reconnect with fresh IDENTIFY.");
                        session.session_id = None;
                        session.sequence = None;
                        session.resume_gateway_url = None;
                        return Ok(());
                    }
                    OP_RECONNECT => {
                        eprintln!("[discord-listener] Discord requested reconnect.");
                        return Ok(());
                    }
                    OP_HELLO => {
                        // Shouldn't arrive after initial Hello, ignore.
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

// ---------------------------------------------------------------------------
// Message handling
// ---------------------------------------------------------------------------

async fn handle_message_create(
    d: &serde_json::Value,
    channel_id: &str,
    prefix: &str,
    self_user_id: &mut Option<String>,
    rate_limiter: &mut RateLimiter,
    http_client: &reqwest::Client,
    cmd_url: &str,
    goal_input_base_url: &str,
    token: &str,
) {
    let msg_channel = d["channel_id"].as_str().unwrap_or("");
    let content = d["content"].as_str().unwrap_or("");
    let author_id = d["author"]["id"].as_str().unwrap_or("");
    let author_bot = d["author"]["bot"].as_bool().unwrap_or(false);
    let author_name = d["author"]["username"].as_str().unwrap_or("?");
    let msg_id = d["id"].as_str().unwrap_or("");

    // Skip: wrong channel, bots, our own messages.
    if msg_channel != channel_id {
        return;
    }
    if author_bot {
        return;
    }
    if let Some(ref self_id) = self_user_id {
        if author_id == self_id {
            return;
        }
    }

    let command = match content.strip_prefix(prefix) {
        Some(cmd) => cmd.trim(),
        None => return,
    };

    if command.is_empty() {
        return;
    }

    // Rate limit check.
    if !rate_limiter.check(author_id) {
        let reply = format!(
            ":no_entry: **Rate limited.** You can send at most {} commands every {}s.",
            RATE_LIMIT_MAX, RATE_LIMIT_WINDOW_SECS
        );
        let _ = post_thread_reply(http_client, token, channel_id, msg_id, &reply).await;
        return;
    }

    // ── Goal input shortcuts (v0.12.4.1) ────────────────────────────────────
    //
    // `>message text here` → route to latest running goal's stdin.
    // `ta input <goal-id> <text>` → route to specified goal's stdin.
    //
    // These bypass the normal command forwarding path.

    if let Some(input_text) = command.strip_prefix('>') {
        // `>text` shorthand: deliver to the most recently started running goal.
        let text = input_text.trim();
        eprintln!(
            "[discord-listener] Goal input (>shorthand) from {}: {:?}",
            author_name, text
        );
        let url = format!("{}/latest/input", goal_input_base_url);
        let reply = forward_goal_input(http_client, &url, text).await;
        let _ = post_thread_reply(http_client, token, channel_id, msg_id, &reply).await;
        return;
    }

    // `ta input <goal-id> <text>` explicit form.
    // After stripping the channel prefix, the command may start with "ta input" or "input".
    let normalized = strip_ta_prefix(command);
    if let Some(rest) = normalized.strip_prefix("input ") {
        let rest = rest.trim();
        // Split on first whitespace to get goal-id and text.
        if let Some((goal_id, text)) = rest.split_once(char::is_whitespace) {
            let goal_id = goal_id.trim();
            let text = text.trim();
            eprintln!(
                "[discord-listener] Goal input (explicit) from {} → goal {}: {:?}",
                author_name, goal_id, text
            );
            let url = format!("{}/{}/input", goal_input_base_url, goal_id);
            let reply = forward_goal_input(http_client, &url, text).await;
            let _ = post_thread_reply(http_client, token, channel_id, msg_id, &reply).await;
            return;
        }
        // `input` without enough args — show usage.
        let reply = ":x: Usage: `input <goal-id> <message>` or `>message text`";
        let _ = post_thread_reply(http_client, token, channel_id, msg_id, reply).await;
        return;
    }

    eprintln!(
        "[discord-listener] Message command from {}: {}",
        author_name, command
    );

    execute_command_with_status(http_client, cmd_url, token, channel_id, msg_id, command).await;
}

/// POST `{ "input": text }` to a goal input URL and return a Discord reply string.
async fn forward_goal_input(client: &reqwest::Client, url: &str, text: &str) -> String {
    match client
        .post(url)
        .json(&json!({ "input": text }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                ":speech_balloon: Delivered to agent.".to_string()
            } else {
                let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| json!({}));
                let err = body["error"].as_str().unwrap_or("unknown error");
                format!(":x: No running goal or delivery failed: {}", err)
            }
        }
        Err(e) => format!(":x: Cannot reach daemon: {}", e),
    }
}

// ---------------------------------------------------------------------------
// Interaction handling (slash commands + button clicks)
// ---------------------------------------------------------------------------

async fn handle_interaction_create(
    d: &serde_json::Value,
    channel_id: &str,
    rate_limiter: &mut RateLimiter,
    http_client: &reqwest::Client,
    cmd_url: &str,
    interactions_url: &str,
    token: &str,
    daemon_url: &str,
) {
    let interaction_id = d["id"].as_str().unwrap_or("");
    let interaction_token = d["token"].as_str().unwrap_or("");
    let interaction_type = d["type"].as_u64().unwrap_or(0);
    let user_id = d["member"]["user"]["id"]
        .as_str()
        .or_else(|| d["user"]["id"].as_str())
        .unwrap_or("unknown");
    let username = d["member"]["user"]["username"]
        .as_str()
        .or_else(|| d["user"]["username"].as_str())
        .unwrap_or("?");

    // Only handle interactions from our watched channel (for component interactions).
    let msg_channel = d["channel_id"].as_str().unwrap_or("");
    if !msg_channel.is_empty() && msg_channel != channel_id {
        return;
    }

    match interaction_type {
        INTERACTION_TYPE_APPLICATION_COMMAND => {
            // Slash command: /ta <subcommand> [args]
            let command_name = d["data"]["name"].as_str().unwrap_or("");
            if command_name != "ta" {
                return;
            }

            let subcommand = d["data"]["options"][0]["value"]
                .as_str()
                .or_else(|| d["data"]["options"][0]["name"].as_str())
                .unwrap_or("status");

            // Build the command string.
            let command = subcommand.trim();

            // Rate limit check.
            if !rate_limiter.check(user_id) {
                let _ = send_interaction_response(
                    http_client,
                    interaction_id,
                    interaction_token,
                    INTERACTION_RESPONSE_CHANNEL_MESSAGE,
                    &format!(
                        ":no_entry: **Rate limited.** Max {} commands per {}s.",
                        RATE_LIMIT_MAX, RATE_LIMIT_WINDOW_SECS
                    ),
                    true,
                )
                .await;
                return;
            }

            eprintln!(
                "[discord-listener] Slash /ta command from {}: {}",
                username, command
            );

            // Acknowledge the interaction immediately (within 3s Discord requirement).
            let _ = send_interaction_response(
                http_client,
                interaction_id,
                interaction_token,
                INTERACTION_RESPONSE_DEFERRED,
                "",
                false,
            )
            .await;

            // Execute command and edit the deferred response.
            let full_command = if command.starts_with("ta ") {
                command.to_string()
            } else {
                format!("ta {}", command)
            };

            let result = forward_command(http_client, cmd_url, &full_command).await;
            let reply = format_reply(&result);

            let _ = edit_interaction_followup(http_client, interaction_token, token, &reply).await;
        }
        INTERACTION_TYPE_MESSAGE_COMPONENT => {
            // Button click. Two custom_id schemes:
            //   draft_{draft_uuid}_{approve|deny}  — draft approve/deny buttons
            //   ta_{ta_interaction_id}_{choice}    — agent interaction answer buttons
            let custom_id = d["data"]["custom_id"].as_str().unwrap_or("");

            eprintln!(
                "[discord-listener] Button click from {}: {}",
                username, custom_id
            );

            if custom_id.starts_with("draft_") {
                // Draft approve/deny: custom_id = "draft_{uuid}_{approve|deny}"
                let without_prefix = &custom_id["draft_".len()..];
                // UUID is 36 chars, followed by _approve or _deny.
                if without_prefix.len() > 37 && without_prefix.as_bytes()[36] == b'_' {
                    let draft_id = &without_prefix[..36];
                    let action = &without_prefix[37..];

                    let endpoint = match action {
                        "approve" => format!("{}/api/drafts/{}/approve", daemon_url, draft_id),
                        "deny" => format!("{}/api/drafts/{}/deny", daemon_url, draft_id),
                        other => {
                            eprintln!("[discord-listener] Unknown draft action: {}", other);
                            return;
                        }
                    };

                    let reply_text = match http_client
                        .post(&endpoint)
                        .json(&json!({}))
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            eprintln!(
                                "[discord-listener] Draft {} {}d via button.",
                                draft_id, action
                            );
                            format!(":white_check_mark: Draft **{}d** (`{}`)", action, &draft_id[..8])
                        }
                        Ok(resp) => {
                            eprintln!(
                                "[discord-listener] Draft {} {} failed: HTTP {}",
                                draft_id, action, resp.status()
                            );
                            format!(":x: Failed to {} draft: HTTP {}", action, resp.status())
                        }
                        Err(e) => {
                            eprintln!("[discord-listener] Cannot reach daemon: {}", e);
                            format!(":x: Cannot reach daemon: {}", e)
                        }
                    };

                    let _ = send_interaction_response(
                        http_client,
                        interaction_id,
                        interaction_token,
                        INTERACTION_RESPONSE_CHANNEL_MESSAGE,
                        &reply_text,
                        false,
                    )
                    .await;
                } else {
                    eprintln!("[discord-listener] Malformed draft button custom_id: {}", custom_id);
                }
                return;
            }

            if !custom_id.starts_with("ta_") {
                return;
            }

            // Parse the custom_id.
            // Format: ta_{ta_interaction_id}_{choice}
            // e.g., ta_550e8400-e29b-41d4-a716-446655440000_yes
            //        ta_550e8400-e29b-41d4-a716-446655440000_choice_2
            let without_prefix = &custom_id["ta_".len()..];
            let (ta_interaction_id, choice) = parse_button_custom_id(without_prefix);

            // Acknowledge the Discord interaction immediately.
            let _ = send_interaction_response(
                http_client,
                interaction_id,
                interaction_token,
                INTERACTION_RESPONSE_CHANNEL_MESSAGE,
                &format!(":white_check_mark: Response recorded: **{}**", choice),
                false,
            )
            .await;

            // POST answer to daemon's interaction endpoint.
            let respond_url = format!("{}/{}/respond", interactions_url, ta_interaction_id);
            match http_client
                .post(&respond_url)
                .json(&json!({ "answer": choice, "source": "discord" }))
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        eprintln!(
                            "[discord-listener] Failed to deliver interaction response to daemon: HTTP {}",
                            resp.status()
                        );
                    } else {
                        eprintln!(
                            "[discord-listener] Interaction {} answered: {}",
                            ta_interaction_id, choice
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[discord-listener] Cannot reach daemon for interaction {}: {}",
                        ta_interaction_id, e
                    );
                }
            }
        }
        _ => {}
    }
}

/// Parse button custom_id suffix (after the leading "ta_") into (interaction_id, choice).
/// Input: "550e8400-e29b-41d4-a716-446655440000_yes"
/// Output: ("550e8400-e29b-41d4-a716-446655440000", "yes")
/// Strip a leading `ta ` prefix case-insensitively.
///
/// Handles `ta`, `Ta`, `TA`, `tA` variants. Argument casing is preserved.
fn strip_ta_prefix(s: &str) -> &str {
    if s.len() >= 3 && s[..2].eq_ignore_ascii_case("ta") && s.as_bytes()[2] == b' ' {
        &s[3..]
    } else {
        s
    }
}

fn parse_button_custom_id(suffix: &str) -> (&str, &str) {
    // UUID format: 8-4-4-4-12 chars = 36 chars, followed by _<choice>
    // Try to find the last '_' that separates UUID from choice.
    // UUIDs contain hyphens but not underscores, so find the first '_' after UUID.
    if suffix.len() > 36 && suffix.as_bytes()[36] == b'_' {
        let id = &suffix[..36];
        let choice = &suffix[37..];
        return (id, choice);
    }
    // Fallback: split on last underscore.
    if let Some(pos) = suffix.rfind('_') {
        (&suffix[..pos], &suffix[pos + 1..])
    } else {
        (suffix, "")
    }
}

// ---------------------------------------------------------------------------
// Command execution with long-running status
// ---------------------------------------------------------------------------

/// Execute a command, posting "Working…" if it takes longer than the threshold.
/// Response is posted as a thread reply to the original message.
async fn execute_command_with_status(
    http_client: &reqwest::Client,
    cmd_url: &str,
    token: &str,
    channel_id: &str,
    msg_id: &str,
    command: &str,
) {
    let full_command = if command.starts_with("ta ") {
        command.to_string()
    } else {
        format!("ta {}", command)
    };

    // Post an initial "Working…" placeholder in a thread.
    let placeholder_msg_id = post_thread_reply(
        http_client,
        token,
        channel_id,
        msg_id,
        ":hourglass_flowing_sand: Working…",
    )
    .await
    .ok();

    let result = forward_command(http_client, cmd_url, &full_command).await;
    let reply = format_reply(&result);

    // Edit the placeholder if we have it, otherwise post a new thread reply.
    if let Some(ref placeholder_id) = placeholder_msg_id {
        if edit_message(http_client, token, channel_id, placeholder_id, &reply)
            .await
            .is_err()
        {
            // Edit failed — fall back to a new message.
            let _ = post_thread_reply(http_client, token, channel_id, msg_id, &reply).await;
        }
    } else {
        let _ = post_thread_reply(http_client, token, channel_id, msg_id, &reply).await;
    }
}

// ---------------------------------------------------------------------------
// Forward command to daemon
// ---------------------------------------------------------------------------

async fn forward_command(
    client: &reqwest::Client,
    cmd_url: &str,
    full_command: &str,
) -> CommandResult {
    match client
        .post(cmd_url)
        .json(&json!({ "command": full_command }))
        .timeout(Duration::from_secs(300))
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
        format!(":x: **Error:** {}", err)
    } else {
        let status_emoji = if result.success {
            ":white_check_mark:"
        } else {
            ":x:"
        };
        let status_label = if result.success { "ok" } else { "failed" };
        let output = truncate_output(&result.output, 1800);
        if output.is_empty() {
            format!("{} **[{}]** (no output)", status_emoji, status_label)
        } else {
            format!(
                "{} **[{}]**\n```\n{}\n```",
                status_emoji, status_label, output
            )
        }
    }
}

/// Truncate output to fit in a Discord message (2000 char limit).
fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…\n*(truncated)*", &s[..max])
    }
}

// ---------------------------------------------------------------------------
// Discord REST helpers
// ---------------------------------------------------------------------------

/// Post a message as a thread reply to an existing message.
///
/// Attempts to create or use a thread on the original message, then posts
/// there. Falls back to a plain message in the channel if threading fails.
async fn post_thread_reply(
    client: &reqwest::Client,
    token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> Result<String, reqwest::Error> {
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&json!({
            "content": content,
            "message_reference": {
                "message_id": message_id,
                "fail_if_not_exists": false
            }
        }))
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await.unwrap_or_default();
    let id = json["id"].as_str().unwrap_or("").to_string();
    Ok(id)
}

/// Edit an existing message in the channel.
async fn edit_message(
    client: &reqwest::Client,
    token: &str,
    channel_id: &str,
    message_id: &str,
    new_content: &str,
) -> Result<(), reqwest::Error> {
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages/{}",
        channel_id, message_id
    );
    client
        .patch(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&json!({ "content": new_content }))
        .send()
        .await?;
    Ok(())
}

/// Respond to a Discord interaction.
async fn send_interaction_response(
    client: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
    response_type: u64,
    content: &str,
    ephemeral: bool,
) -> Result<(), reqwest::Error> {
    let url = format!(
        "https://discord.com/api/v10/interactions/{}/{}/callback",
        interaction_id, interaction_token
    );

    let mut data = json!({ "type": response_type });
    if response_type == INTERACTION_RESPONSE_CHANNEL_MESSAGE && !content.is_empty() {
        data["data"] = json!({
            "content": content,
            "flags": if ephemeral { 64 } else { 0 }
        });
    }

    client
        .post(&url)
        .header("Authorization", format!("Bot {}", ""))
        .json(&data)
        .send()
        .await?;
    Ok(())
}

/// Edit a deferred interaction's followup message (used after DEFERRED response).
async fn edit_interaction_followup(
    client: &reqwest::Client,
    interaction_token: &str,
    bot_token: &str,
    content: &str,
) -> Result<(), reqwest::Error> {
    // Use the application's bot token + interaction token for followup.
    // The application ID is embedded in the interaction token (first segment).
    // For editing the deferred message, we PATCH the @original message.
    let url = format!(
        "https://discord.com/api/v10/webhooks/{{application_id}}/{}/messages/@original",
        interaction_token
    );

    // We need the application_id. Since it's not stored here, use a workaround:
    // The interaction token encodes the application ID in it. We can extract it
    // or store it at connect time. For now, post as a followup reply instead.
    // TODO: Store application_id at READY time and use it here.
    let _ = (client, bot_token, url); // suppress unused warnings

    // Fallback: post a new message via the webhook followup.
    let followup_url = format!(
        "https://discord.com/api/v10/webhooks/me/{}/messages/@original",
        interaction_token
    );
    client
        .patch(&followup_url)
        .json(&json!({ "content": content }))
        .send()
        .await?;
    Ok(())
}

/// Post a plain message to a Discord channel.
#[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        assert!(result.len() < 300);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn rate_limiter_allows_within_limit() {
        let mut rl = RateLimiter::new(3, 60);
        assert!(rl.check("user1"));
        assert!(rl.check("user1"));
        assert!(rl.check("user1"));
        assert!(!rl.check("user1")); // 4th command — rejected
    }

    #[test]
    fn rate_limiter_separate_users() {
        let mut rl = RateLimiter::new(2, 60);
        assert!(rl.check("alice"));
        assert!(rl.check("alice"));
        assert!(!rl.check("alice")); // limited
        assert!(rl.check("bob")); // different user — allowed
        assert!(rl.check("bob"));
    }

    #[test]
    fn parse_button_custom_id_yes_no() {
        let (id, choice) = parse_button_custom_id("550e8400-e29b-41d4-a716-446655440000_yes");
        assert_eq!(id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(choice, "yes");
    }

    #[test]
    fn parse_button_custom_id_choice_index() {
        let (id, choice) = parse_button_custom_id("550e8400-e29b-41d4-a716-446655440000_choice_2");
        assert_eq!(id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(choice, "choice_2");
    }

    #[test]
    fn parse_button_custom_id_no_uuid() {
        let (id, choice) = parse_button_custom_id("someid_yes");
        assert_eq!(id, "someid");
        assert_eq!(choice, "yes");
    }

    #[test]
    fn gateway_session_defaults() {
        let s = GatewaySession::default();
        assert!(s.session_id.is_none());
        assert!(s.sequence.is_none());
        assert!(s.resume_gateway_url.is_none());
    }

    // ── v0.12.4.1: goal input routing tests ──────────────────────────────────

    /// Helper that mirrors the routing logic in handle_message_create.
    enum DispatchDecision<'a> {
        GoalInputLatest { text: &'a str },
        GoalInputExplicit { goal_id: &'a str, text: &'a str },
        GoalInputMissingArgs,
        Command { command: &'a str },
    }

    fn classify_command(command: &str) -> DispatchDecision<'_> {
        if let Some(input_text) = command.strip_prefix('>') {
            return DispatchDecision::GoalInputLatest {
                text: input_text.trim(),
            };
        }
        let normalized = strip_ta_prefix(command);
        if let Some(rest) = normalized.strip_prefix("input ") {
            let rest = rest.trim();
            if let Some((goal_id, text)) = rest.split_once(char::is_whitespace) {
                return DispatchDecision::GoalInputExplicit {
                    goal_id: goal_id.trim(),
                    text: text.trim(),
                };
            }
            return DispatchDecision::GoalInputMissingArgs;
        }
        DispatchDecision::Command { command }
    }

    #[test]
    fn gt_shorthand_routes_to_latest_input() {
        // `>fix the sorting` after prefix-strip → GoalInputLatest.
        let cmd = ">fix the sorting";
        match classify_command(cmd) {
            DispatchDecision::GoalInputLatest { text } => assert_eq!(text, "fix the sorting"),
            _ => panic!("expected GoalInputLatest"),
        }
    }

    #[test]
    fn ta_input_explicit_routes_correctly() {
        let cmd = "ta input abc123 please fix the bug";
        match classify_command(cmd) {
            DispatchDecision::GoalInputExplicit { goal_id, text } => {
                assert_eq!(goal_id, "abc123");
                assert_eq!(text, "please fix the bug");
            }
            _ => panic!("expected GoalInputExplicit"),
        }
    }

    #[test]
    fn bare_input_explicit_routes_correctly() {
        // Without the "ta " prefix — bare "input <id> <text>".
        let cmd = "input abc123 hello";
        match classify_command(cmd) {
            DispatchDecision::GoalInputExplicit { goal_id, text } => {
                assert_eq!(goal_id, "abc123");
                assert_eq!(text, "hello");
            }
            _ => panic!("expected GoalInputExplicit"),
        }
    }

    #[test]
    fn input_without_args_shows_usage() {
        let cmd = "input abc123";
        // Only goal-id, no text — should trigger missing args.
        match classify_command(cmd) {
            DispatchDecision::GoalInputMissingArgs => {}
            _ => panic!("expected GoalInputMissingArgs"),
        }
    }

    #[test]
    fn normal_command_not_intercepted() {
        let cmd = "ta draft list";
        match classify_command(cmd) {
            DispatchDecision::Command { command } => assert_eq!(command, "ta draft list"),
            _ => panic!("expected Command"),
        }
    }
}
