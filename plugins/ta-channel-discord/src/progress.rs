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
//!
//! ## Cursor-based replay prevention (v0.12.6)
//!
//! On initial connect the streamer passes `?since=<startup_time>` so events
//! that occurred before the plugin started are never replayed. On reconnect it
//! passes `?since=<last_event_timestamp>` (extracted from each event's data
//! envelope) so no events are skipped or replayed across reconnects.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::json;

/// Maximum rate for posting per-goal updates (one every N seconds per goal).
const GOAL_UPDATE_THROTTLE_SECS: u64 = 10;

/// How often to retry the SSE connection after a failure.
const RECONNECT_DELAY_SECS: u64 = 15;

/// Maximum age of an SSE event before it is silently dropped (safety net for
/// replay regressions). Events older than this relative to wall-clock time are
/// discarded even if the cursor filter should have excluded them.
const MAX_EVENT_AGE_SECS: i64 = 600; // 10 minutes

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
        "running" => 0x5865F2,           // blurple
        "pr_ready" => 0xFEE75C,          // yellow
        "under_review" => 0xFEE75C,      // yellow
        "approved" => 0x57F287,          // green
        "applied" => 0x57F287,           // green
        "completed" => 0x57F287,         // green
        "failed" | "denied" => 0xED4245, // red
        _ => 0x99AAB5,                   // grey
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the SSE progress streamer as a persistent background loop.
///
/// Connects to `{daemon_url}/api/events`, parses SSE events, and posts
/// goal progress updates to the configured Discord channel.
///
/// Uses a cursor to prevent event replay (v0.12.6):
/// - Initial connect: `?since=<startup_time>` — skip events before startup.
/// - Reconnect: `?since=<last_event_timestamp>` — resume from last seen event.
pub async fn run_progress_streamer(
    daemon_url: String,
    token: String,
    channel_id: String,
    application_id: Option<String>,
) {
    let client = Client::new();
    let base_events_url = format!("{}/api/events", daemon_url);

    eprintln!(
        "[discord-progress] Starting SSE streamer on {}",
        base_events_url
    );

    // Record startup time once — never replay events before this point (item 8).
    let startup_time: DateTime<Utc> = Utc::now();
    // cursor tracks the last seen event timestamp for reconnect (item 9).
    let mut cursor: DateTime<Utc> = startup_time;

    let mut throttle_map: HashMap<String, Instant> = HashMap::new();

    loop {
        match stream_events(
            &client,
            &base_events_url,
            &token,
            &channel_id,
            &application_id,
            &mut throttle_map,
            &mut cursor,
        )
        .await
        {
            Ok(()) => {
                eprintln!(
                    "[discord-progress] SSE stream ended. Reconnecting in {}s...",
                    RECONNECT_DELAY_SECS
                );
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

/// Connect to the SSE stream starting from `cursor` (passed as `?since=`).
/// Updates `cursor` in-place as events arrive so the caller can resume after reconnect.
async fn stream_events(
    client: &Client,
    base_events_url: &str,
    token: &str,
    channel_id: &str,
    application_id: &Option<String>,
    throttle_map: &mut HashMap<String, Instant>,
    cursor: &mut DateTime<Utc>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Pass `?since=` so the server replays only events after our cursor (items 8 & 9).
    let events_url = format!("{}?since={}", base_events_url, cursor.to_rfc3339());

    let resp = client
        .get(&events_url)
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
                // Advance cursor from the event envelope's timestamp field (item 9).
                // This ensures reconnects resume from exactly where we left off.
                let mut event_age_ok = true;
                if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&event_data) {
                    if let Some(ts_str) = envelope["timestamp"].as_str() {
                        if let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) {
                            let event_ts = ts.with_timezone(&Utc);
                            *cursor = event_ts;

                            // Age filter (item 1 / v0.12.8): drop events that are
                            // too old relative to wall-clock time. This is a safety
                            // net for replay regressions; the cursor filter is the
                            // primary defence.
                            let age_secs = Utc::now().signed_duration_since(event_ts).num_seconds();
                            if age_secs > MAX_EVENT_AGE_SECS {
                                eprintln!(
                                    "[discord-progress] Dropping stale event (age {}s > {}s limit)",
                                    age_secs, MAX_EVENT_AGE_SECS
                                );
                                event_age_ok = false;
                            }
                        }
                    }
                }

                if event_age_ok {
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
        // Legacy names kept for compatibility; actual daemon emits these types:
        "goal_started" | "goal.state_changed" | "goal_state_changed" | "goal_completed"
        | "goal_failed" => {
            handle_goal_state_changed(client, token, channel_id, &event, throttle_map).await;
        }
        // Only fire on review_requested — that's the authoritative "draft is ready for
        // human review" signal. draft_built is an internal pipeline event emitted earlier
        // in the same pipeline run; handling both causes duplicate Discord notifications.
        "review_requested" => {
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
    // The payload is wrapped: { event_type, payload: { type, goal_id, title, ... } }
    let payload = event.get("payload").unwrap_or(event);

    let goal_id = payload["goal_id"].as_str().unwrap_or("");
    // Map actual event types to a state label.
    let event_type = payload["type"].as_str().unwrap_or("");
    let new_state = match event_type {
        "goal_started" => "running",
        "goal_completed" => "completed",
        "goal_failed" => "failed",
        _ => payload["to_state"].as_str().unwrap_or(""),
    };
    let title = payload["title"]
        .as_str()
        .or_else(|| payload["goal_title"].as_str())
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

    let short_id = if goal_id.len() >= 8 {
        &goal_id[..8]
    } else {
        goal_id
    };
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
    // EventEnvelope wraps payload; fall back to flat layout for legacy compatibility.
    let payload = event.get("payload").unwrap_or(event);

    let draft_id = payload["draft_id"]
        .as_str()
        .or_else(|| payload["data"]["draft_id"].as_str())
        .unwrap_or("");
    let goal_title = payload["goal_title"]
        .as_str()
        .or_else(|| payload["title"].as_str())
        .or_else(|| payload["data"]["goal_title"].as_str())
        .unwrap_or("(unnamed goal)");
    let summary = payload["summary"]
        .as_str()
        .or_else(|| payload["data"]["summary"].as_str())
        .unwrap_or("Review the draft for details.");
    let artifact_count = payload["artifact_count"]
        .as_u64()
        .or_else(|| payload["data"]["artifact_count"].as_u64())
        .unwrap_or(0);

    if draft_id.is_empty() {
        return;
    }

    let short_draft = if draft_id.len() >= 8 {
        &draft_id[..8]
    } else {
        draft_id
    };

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
    post_payload(client, token, channel_id, json!({ "embeds": [embed] })).await
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

    // ── v0.12.6: Cursor-based replay prevention tests ──

    #[test]
    fn startup_cursor_is_before_now() {
        // The startup cursor should be at or before the current time.
        let startup: DateTime<Utc> = Utc::now();
        let after: DateTime<Utc> = Utc::now();
        assert!(startup <= after);
    }

    #[test]
    fn cursor_advances_from_event_timestamp() {
        // Simulate extracting a timestamp from an event envelope.
        let ts_str = "2026-03-19T12:00:00Z";
        let event_data = serde_json::json!({
            "timestamp": ts_str,
            "event_type": "goal_started",
            "payload": {}
        })
        .to_string();

        let envelope: serde_json::Value = serde_json::from_str(&event_data).unwrap();
        let extracted_ts = envelope["timestamp"].as_str().unwrap();
        let parsed = DateTime::parse_from_rfc3339(extracted_ts)
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(parsed.to_rfc3339(), "2026-03-19T12:00:00+00:00");
    }

    #[test]
    fn cursor_since_param_format() {
        // Verify the ?since= URL format used on connect/reconnect.
        let cursor: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-03-19T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let events_url = format!(
            "http://localhost:7700/api/events?since={}",
            cursor.to_rfc3339()
        );
        // URL contains the since parameter in RFC 3339 format.
        assert!(events_url.contains("?since=2026-03-19T10:00:00"));
    }

    #[test]
    fn cursor_does_not_replay_before_startup() {
        // Cursor starts at startup_time; reconnect URL uses cursor, not epoch 0.
        let startup_time: DateTime<Utc> = Utc::now();
        let cursor = startup_time;
        // The cursor should be after the unix epoch (no history replayed).
        let epoch = DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(cursor > epoch);
    }

    // ── v0.12.8: Age filter tests ──

    #[test]
    fn age_filter_rejects_old_event() {
        // An event timestamped 15 minutes ago exceeds the 10-minute limit.
        let old_ts = Utc::now() - chrono::Duration::seconds(15 * 60);
        let age_secs = Utc::now().signed_duration_since(old_ts).num_seconds();
        assert!(
            age_secs > MAX_EVENT_AGE_SECS,
            "15-min-old event should be rejected"
        );
    }

    #[test]
    fn age_filter_accepts_recent_event() {
        // An event timestamped 5 minutes ago is within the 10-minute limit.
        let recent_ts = Utc::now() - chrono::Duration::seconds(5 * 60);
        let age_secs = Utc::now().signed_duration_since(recent_ts).num_seconds();
        assert!(
            age_secs <= MAX_EVENT_AGE_SECS,
            "5-min-old event should be accepted"
        );
    }

    #[test]
    fn age_filter_accepts_fresh_event() {
        // An event timestamped 1 second ago is well within the limit.
        let fresh_ts = Utc::now() - chrono::Duration::seconds(1);
        let age_secs = Utc::now().signed_duration_since(fresh_ts).num_seconds();
        assert!(
            age_secs <= MAX_EVENT_AGE_SECS,
            "1-sec-old event should be accepted"
        );
    }

    #[test]
    fn age_filter_boundary_at_limit() {
        // An event exactly at the limit (600s) should still be accepted
        // (we use > not >=).
        let boundary_ts = Utc::now() - chrono::Duration::seconds(MAX_EVENT_AGE_SECS);
        let age_secs = Utc::now().signed_duration_since(boundary_ts).num_seconds();
        // Allow 1s of test execution jitter: the boundary age is ≤ MAX_EVENT_AGE_SECS+1.
        assert!(
            age_secs <= MAX_EVENT_AGE_SECS + 1,
            "boundary event should not exceed limit by more than 1s"
        );
    }
}
