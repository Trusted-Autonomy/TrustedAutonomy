//! Goal progress streaming and draft summary notifications for Discord (v0.12.1).
//!
//! Subscribes to the TA daemon's SSE event stream and forwards goal lifecycle
//! events to a Discord channel. Features:
//!
//! - **Goal progress**: Posts stage transitions (Running → PrReady → Applied)
//!   with progress embeds. Throttled to avoid flooding (≤1 update per goal per 10s).
//! - **Draft summary**: When a goal produces a draft, posts the AI summary +
//!   artifact count with approve/deny buttons linking to `ta draft approve <id>`.
//!
//! ## Architecture
//!
//! `run_progress_streamer` connects to `GET /api/events` (SSE stream) and
//! processes events in a loop, reconnecting with backoff on errors. It runs
//! as a background task alongside the Gateway listener.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::json;

/// Maximum rate for posting per-goal updates (one every N seconds per goal).
const GOAL_UPDATE_THROTTLE_SECS: u64 = 10;

/// How often to retry the SSE connection after a failure.
const RECONNECT_DELAY_SECS: u64 = 15;

/// Goal state labels for human-readable Discord messages.
fn state_label(state: &str) -> Option<&'static str> {
    match state {
        "running" => Some(":rocket: Goal started"),
        "pr_ready" => Some(":clipboard: Draft ready for review"),
        "under_review" => Some(":eyes: Draft under review"),
        "approved" => Some(":white_check_mark: Draft approved"),
        "applied" => Some(":tada: Changes applied"),
        "completed" => Some(":checkered_flag: Goal completed"),
        "failed" => Some(":x: Goal failed"),
        "denied" => Some(":no_entry: Draft denied"),
        _ => None,
    }
}

/// Discord embed colors for goal states.
fn state_color(state: &str) -> u32 {
    match state {
        "running" => 0x5865F2,       // blurple
        "pr_ready" => 0xFEE75C,      // yellow
        "under_review" => 0xFEE75C,  // yellow
        "approved" => 0x57F287,      // green
        "applied" => 0x57F287,       // green
        "completed" => 0x57F287,     // green
        "failed" | "denied" => 0xED4245, // red
        _ => 0x99AAB5,               // grey
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the SSE progress streamer as a persistent background loop.
///
/// Connects to `{daemon_url}/api/events`, parses SSE events, and posts
/// goal progress updates to the configured Discord channel.
pub async fn run_progress_streamer(
    daemon_url: String,
    token: String,
    channel_id: String,
    application_id: Option<String>,
) {
    let client = Client::new();
    let events_url = format!("{}/api/events", daemon_url);

    eprintln!(
        "[discord-progress] Starting SSE streamer on {}",
        events_url
    );

    let mut throttle_map: HashMap<String, Instant> = HashMap::new();

    loop {
        match stream_events(
            &client,
            &events_url,
            &token,
            &channel_id,
            &application_id,
            &mut throttle_map,
        )
        .await
        {
            Ok(()) => {
                eprintln!("[discord-progress] SSE stream ended. Reconnecting in {}s...", RECONNECT_DELAY_SECS);
            }
            Err(e) => {
                eprintln!(
                    "[discord-progress] SSE error: {}. Reconnecting in {}s...",
                    e, RECONNECT_DELAY_SECS
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
    }
}

async fn stream_events(
    client: &Client,
    events_url: &str,
    token: &str,
    channel_id: &str,
    application_id: &Option<String>,
    throttle_map: &mut HashMap<String, Instant>,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client
        .get(events_url)
        .header("Accept", "text/event-stream")
        .timeout(Duration::from_secs(3600)) // long-lived connection
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(format!("SSE endpoint returned HTTP {}", resp.status()).into());
    }

    // Read the SSE stream line by line.
    use tokio::io::AsyncBufReadExt;
    let bytes = resp.bytes_stream();
    use futures_util::StreamExt;
    let stream = bytes.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(stream);
    let mut lines = tokio::io::BufReader::new(reader).lines();

    let mut event_type = String::new();
    let mut event_data = String::new();

    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            // End of SSE event — process it.
            if !event_data.is_empty() {
                process_event(
                    client,
                    token,
                    channel_id,
                    application_id,
                    &event_type,
                    &event_data,
                    throttle_map,
                )
                .await;
            }
            event_type.clear();
            event_data.clear();
        } else if let Some(data) = line.strip_prefix("data: ") {
            event_data.push_str(data);
        } else if let Some(etype) = line.strip_prefix("event: ") {
            event_type = etype.to_string();
        }
        // Ignore other SSE fields (id:, retry:, comments).
    }

    Ok(())
}

async fn process_event(
    client: &Client,
    token: &str,
    channel_id: &str,
    application_id: &Option<String>,
    event_type: &str,
    data: &str,
    throttle_map: &mut HashMap<String, Instant>,
) {
    let event: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return,
    };

    // We handle two event types:
    // - "goal.state_changed" (or generic dispatch with type field)
    // - "draft.ready"
    let etype = if event_type.is_empty() {
        event["type"].as_str().unwrap_or("")
    } else {
        event_type
    };

    match etype {
        "goal.state_changed" | "goal_state_changed" => {
            handle_goal_state_changed(client, token, channel_id, &event, throttle_map).await;
        }
        "draft.ready" | "draft_ready" => {
            handle_draft_ready(client, token, channel_id, application_id, &event).await;
        }
        _ => {}
    }
}

async fn handle_goal_state_changed(
    client: &Client,
    token: &str,
    channel_id: &str,
    event: &serde_json::Value,
    throttle_map: &mut HashMap<String, Instant>,
) {
    let goal_id = event["goal_id"]
        .as_str()
        .or_else(|| event["data"]["goal_id"].as_str())
        .unwrap_or("");
    let new_state = event["new_state"]
        .as_str()
        .or_else(|| event["data"]["new_state"].as_str())
        .unwrap_or("");
    let title = event["goal_title"]
        .as_str()
        .or_else(|| event["data"]["goal_title"].as_str())
        .unwrap_or("(unnamed goal)");

    if goal_id.is_empty() || new_state.is_empty() {
        return;
    }

    // Only post for interesting states.
    let label = match state_label(new_state) {
        Some(l) => l,
        None => return,
    };

    // Throttle per goal.
    let now = Instant::now();
    let last = throttle_map.get(goal_id).copied();
    if let Some(last) = last {
        if now.duration_since(last) < Duration::from_secs(GOAL_UPDATE_THROTTLE_SECS) {
            return;
        }
    }
    throttle_map.insert(goal_id.to_string(), now);

    let short_id = if goal_id.len() >= 8 { &goal_id[..8] } else { goal_id };
    let description = format!("**{}**\n`{}`", title, short_id);

    let embed = json!({
        "title": label,
        "description": description,
        "color": state_color(new_state),
        "footer": {
            "text": format!("Goal {} • state: {}", short_id, new_state)
        }
    });

    let _ = post_embed(client, token, channel_id, embed).await;
}

async fn handle_draft_ready(
    client: &Client,
    token: &str,
    channel_id: &str,
    application_id: &Option<String>,
    event: &serde_json::Value,
) {
    let draft_id = event["draft_id"]
        .as_str()
        .or_else(|| event["data"]["draft_id"].as_str())
        .unwrap_or("");
    let goal_title = event["goal_title"]
        .as_str()
        .or_else(|| event["data"]["goal_title"].as_str())
        .unwrap_or("(unnamed goal)");
    let summary = event["summary"]
        .as_str()
        .or_else(|| event["data"]["summary"].as_str())
        .unwrap_or("Review the draft for details.");
    let artifact_count = event["artifact_count"]
        .as_u64()
        .or_else(|| event["data"]["artifact_count"].as_u64())
        .unwrap_or(0);

    if draft_id.is_empty() {
        return;
    }

    let short_draft = if draft_id.len() >= 8 { &draft_id[..8] } else { draft_id };

    let description = format!(
        "{}\n\n**{} file{}** changed.",
        summary,
        artifact_count,
        if artifact_count == 1 { "" } else { "s" }
    );

    let embed = json!({
        "title": format!(":clipboard: Draft ready: {}", goal_title),
        "description": description,
        "color": 0xFEE75C,  // yellow
        "fields": [
            {
                "name": "Draft ID",
                "value": format!("`{}`", short_draft),
                "inline": true
            },
            {
                "name": "Review",
                "value": format!("`ta draft view {}`", draft_id),
                "inline": true
            }
        ],
        "footer": {
            "text": "Use the buttons to approve or deny this draft."
        }
    });

    // Approve/deny buttons using the draft_id in custom_id.
    // Note: these use a different custom_id scheme than interaction questions.
    let components = if application_id.is_some() {
        // Only add buttons if we have an application_id (for interactions to work).
        vec![json!({
            "type": 1, // ACTION_ROW
            "components": [
                {
                    "type": 2,  // BUTTON
                    "style": 3, // SUCCESS (green)
                    "label": "Approve",
                    "custom_id": format!("draft_{}_approve", draft_id),
                },
                {
                    "type": 2,
                    "style": 4, // DANGER (red)
                    "label": "Deny",
                    "custom_id": format!("draft_{}_deny", draft_id),
                }
            ]
        })]
    } else {
        vec![]
    };

    let mut payload = json!({
        "embeds": [embed],
    });

    if !components.is_empty() {
        payload["components"] = json!(components);
    }

    let _ = post_payload(client, token, channel_id, payload).await;
}

// ---------------------------------------------------------------------------
// Discord REST helpers
// ---------------------------------------------------------------------------

async fn post_embed(
    client: &Client,
    token: &str,
    channel_id: &str,
    embed: serde_json::Value,
) -> Result<(), reqwest::Error> {
    post_payload(
        client,
        token,
        channel_id,
        json!({ "embeds": [embed] }),
    )
    .await
}

async fn post_payload(
    client: &Client,
    token: &str,
    channel_id: &str,
    payload: serde_json::Value,
) -> Result<(), reqwest::Error> {
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );
    client
        .post(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&payload)
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
    fn state_label_known_states() {
        assert!(state_label("running").is_some());
        assert!(state_label("pr_ready").is_some());
        assert!(state_label("applied").is_some());
        assert!(state_label("failed").is_some());
    }

    #[test]
    fn state_label_unknown_state() {
        assert!(state_label("initializing").is_none());
        assert!(state_label("").is_none());
    }

    #[test]
    fn state_colors_are_valid() {
        assert!(state_color("running") > 0);
        assert!(state_color("failed") > 0);
        // Unknown state returns grey.
        assert_eq!(state_color("unknown_xyz"), 0x99AAB5);
    }

    #[test]
    fn draft_notify_artifact_plural() {
        // Verify plural logic.
        let count: u64 = 2;
        let suffix = if count == 1 { "" } else { "s" };
        assert_eq!(suffix, "s");
        let count: u64 = 1;
        let suffix = if count == 1 { "" } else { "s" };
        assert_eq!(suffix, "");
    }
}
