//! JSON-over-stdio protocol types for external social media adapter plugins.
//!
//! Social adapter plugins communicate with TA using a request/response
//! protocol over stdin/stdout. TA spawns the plugin process, writes one JSON
//! request line to stdin, reads one JSON response line from stdout.
//!
//! ## Protocol overview
//!
//! Every exchange is a single JSON line in each direction:
//!
//! ```text
//! TA → plugin: {"op":"<name>",...params...}
//! plugin → TA: {"ok":true,...result...}   or   {"ok":false,"error":"..."}
//! ```
//!
//! ## Operations
//!
//! | Op                 | Description                                              |
//! |--------------------|----------------------------------------------------------|
//! | `create_draft`     | Write a draft to the platform's native draft state       |
//! | `create_scheduled` | Schedule a post at a future time (platform scheduler)    |
//! | `draft_status`     | Poll whether a draft was published, deleted, or open     |
//! | `health`           | Connectivity + credential check                          |
//! | `capabilities`     | Advertise which optional ops this plugin supports        |
//!
//! ## Safety invariant — `publish` is absent by design
//!
//! The `publish` operation is intentionally absent from this protocol.
//! TA never publishes social media posts on behalf of the user. Plugins
//! expose only `create_draft` and `create_scheduled`; the user publishes
//! from their native platform UI or scheduler (e.g., LinkedIn, Buffer).
//! This is a deliberate safety boundary enforced at the type level.

use serde::{Deserialize, Serialize};

/// Protocol version implemented by this TA build.
pub const SOCIAL_PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request envelope
// ---------------------------------------------------------------------------

/// Request sent from TA to a social media plugin over stdin.
///
/// One JSON line per request. The plugin processes it and writes one
/// `SocialPluginResponse` line to stdout, then the process exits.
///
/// The `op` field selects the operation. Additional fields carry
/// operation-specific parameters (flat layout, not nested).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SocialPluginRequest {
    /// Create a draft in the platform's native draft state.
    ///
    /// NOTE: There is intentionally no `Publish` variant. TA never publishes.
    CreateDraft(CreateSocialDraftParams),

    /// Schedule a post at a future time via the platform's native scheduler.
    ///
    /// NOTE: This schedules a post but does not publish it immediately.
    /// The platform (or its scheduler, e.g., Buffer) controls the actual send.
    CreateScheduled(CreateScheduledParams),

    /// Poll the current state of a previously created draft or scheduled post.
    DraftStatus(SocialDraftStatusParams),

    /// Connectivity and credential health check.
    Health(SocialHealthParams),

    /// Advertise optional capabilities supported by this plugin.
    Capabilities(SocialCapabilitiesParams),
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

/// Response sent from a social media plugin to TA over stdout.
///
/// One JSON line per response. Always contains `ok`; on success contains
/// the operation result fields; on failure contains `error`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SocialPluginResponse {
    /// Whether the operation succeeded.
    pub ok: bool,

    /// Human-readable error message (only set when ok=false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Native draft ID assigned by the platform (only for create_draft op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,

    /// Native scheduled post ID (only for create_scheduled op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_id: Option<String>,

    /// ISO-8601 timestamp when the post is scheduled to go out (create_scheduled op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,

    /// Current state of a draft or scheduled post (only for draft_status op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SocialPostState>,

    /// Connected handle / username (only for health op, e.g., "@username").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,

    /// Provider name reported by the plugin (only for health op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Capabilities declared by the plugin (only for capabilities op).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

impl SocialPluginResponse {
    /// Construct a success response with no result fields.
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            draft_id: None,
            scheduled_id: None,
            scheduled_at: None,
            state: None,
            handle: None,
            provider: None,
            capabilities: None,
        }
    }

    /// Construct an error response.
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            draft_id: None,
            scheduled_id: None,
            scheduled_at: None,
            state: None,
            handle: None,
            provider: None,
            capabilities: None,
        }
    }
}

// ---------------------------------------------------------------------------
// create_draft
// ---------------------------------------------------------------------------

/// Parameters for the `create_draft` operation.
///
/// The plugin writes this draft to the platform's native draft state.
/// For LinkedIn: Draft Share API. For X: draft tweet endpoint.
/// For Buffer: Buffer Draft queue.
///
/// The user sees the draft in their platform UI and publishes when ready.
/// TA never publishes directly.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateSocialDraftParams {
    /// Post content to draft.
    pub post: SocialPostContent,
}

/// The content of a social media post to be drafted or scheduled.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct SocialPostContent {
    /// Main post body text.
    pub body: String,

    /// URLs to media attachments (images, videos). May be empty.
    #[serde(default)]
    pub media_urls: Vec<String>,

    /// Post ID or URL being replied to (for threaded replies). None for new posts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<String>,
}

// ---------------------------------------------------------------------------
// create_scheduled
// ---------------------------------------------------------------------------

/// Parameters for the `create_scheduled` operation.
///
/// The plugin queues this post in the platform's native scheduler.
/// The post goes live at `scheduled_at` without further TA involvement.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateScheduledParams {
    /// Post content to schedule.
    pub post: SocialPostContent,

    /// ISO-8601 timestamp when the post should go live.
    pub scheduled_at: String,
}

// ---------------------------------------------------------------------------
// draft_status
// ---------------------------------------------------------------------------

/// Parameters for the `draft_status` operation.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SocialDraftStatusParams {
    /// Platform-specific draft ID returned by `create_draft` or `create_scheduled`.
    pub draft_id: String,
}

/// Current state of a social post as reported by the platform.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "snake_case")]
pub enum SocialPostState {
    /// Draft exists and has not been published.
    Draft,
    /// The post has been published (by the user or scheduler).
    Published,
    /// The draft or scheduled post was deleted.
    Deleted,
    /// State cannot be determined (e.g., platform API limitations).
    Unknown,
}

impl std::fmt::Display for SocialPostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocialPostState::Draft => write!(f, "draft"),
            SocialPostState::Published => write!(f, "published"),
            SocialPostState::Deleted => write!(f, "deleted"),
            SocialPostState::Unknown => write!(f, "unknown"),
        }
    }
}

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

/// Parameters for the `health` operation.
///
/// No parameters required — plugins use their configured credentials.
#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SocialHealthParams {}

// ---------------------------------------------------------------------------
// capabilities
// ---------------------------------------------------------------------------

/// Parameters for the `capabilities` operation.
///
/// No parameters required.
#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SocialCapabilitiesParams {}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from social media plugin operations.
#[derive(Debug, thiserror::Error)]
pub enum SocialPluginError {
    #[error("social plugin not found: {name}. Install with: ta adapter setup social/{name}")]
    PluginNotFound { name: String },

    #[error("social plugin '{name}' op '{op}' failed: {reason}")]
    OpFailed {
        name: String,
        op: String,
        reason: String,
    },

    #[error("social plugin '{name}' produced invalid response for op '{op}': {reason}")]
    InvalidResponse {
        name: String,
        op: String,
        reason: String,
    },

    #[error("failed to spawn social plugin '{command}': {reason}. Ensure the plugin is on PATH.")]
    SpawnFailed { command: String, reason: String },

    #[error("social plugin '{name}' timed out after {timeout_secs}s for op '{op}'. Increase timeout in plugin.toml.")]
    Timeout {
        name: String,
        op: String,
        timeout_secs: u64,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_draft_request_roundtrip() {
        let req = SocialPluginRequest::CreateDraft(CreateSocialDraftParams {
            post: SocialPostContent {
                body: "Excited to announce the cinepipe launch! 🎬".to_string(),
                media_urls: vec![],
                reply_to_id: None,
            },
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"create_draft\""));
        let parsed: SocialPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn create_scheduled_request_roundtrip() {
        let req = SocialPluginRequest::CreateScheduled(CreateScheduledParams {
            post: SocialPostContent {
                body: "Week 1 of our public alpha is live!".to_string(),
                media_urls: vec!["https://example.com/screenshot.png".to_string()],
                reply_to_id: None,
            },
            scheduled_at: "2026-04-07T14:00:00Z".to_string(),
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"create_scheduled\""));
        assert!(json.contains("2026-04-07T14:00:00Z"));
        let parsed: SocialPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn no_publish_op_variant() {
        // The protocol MUST NOT have a Publish variant.
        let req = SocialPluginRequest::Health(SocialHealthParams {});
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("\"publish\""),
            "Publish op must not exist in the social protocol"
        );
    }

    #[test]
    fn draft_status_request_roundtrip() {
        let req = SocialPluginRequest::DraftStatus(SocialDraftStatusParams {
            draft_id: "linkedin-draft-xyz".to_string(),
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"draft_status\""));
        let parsed: SocialPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn health_request_roundtrip() {
        let req = SocialPluginRequest::Health(SocialHealthParams {});
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"health\""));
        let parsed: SocialPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn response_ok_roundtrip() {
        let resp = SocialPluginResponse::ok();
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SocialPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = SocialPluginResponse::error("credentials not found");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SocialPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("credentials not found"));
    }

    #[test]
    fn response_with_draft_id() {
        let mut resp = SocialPluginResponse::ok();
        resp.draft_id = Some("linkedin-draft-abc123".to_string());
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SocialPluginResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.draft_id.as_deref(), Some("linkedin-draft-abc123"));
    }

    #[test]
    fn response_with_scheduled_id() {
        let mut resp = SocialPluginResponse::ok();
        resp.scheduled_id = Some("buffer-post-xyz".to_string());
        resp.scheduled_at = Some("2026-04-07T14:00:00Z".to_string());
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SocialPluginResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.scheduled_id.as_deref(), Some("buffer-post-xyz"));
        assert_eq!(parsed.scheduled_at.as_deref(), Some("2026-04-07T14:00:00Z"));
    }

    #[test]
    fn post_state_display() {
        assert_eq!(SocialPostState::Draft.to_string(), "draft");
        assert_eq!(SocialPostState::Published.to_string(), "published");
        assert_eq!(SocialPostState::Deleted.to_string(), "deleted");
        assert_eq!(SocialPostState::Unknown.to_string(), "unknown");
    }

    #[test]
    fn post_state_roundtrip() {
        for state in [
            SocialPostState::Draft,
            SocialPostState::Published,
            SocialPostState::Deleted,
            SocialPostState::Unknown,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: SocialPostState = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn social_protocol_version_is_one() {
        assert_eq!(SOCIAL_PROTOCOL_VERSION, 1);
    }

    #[test]
    fn post_content_with_media_urls() {
        let post = SocialPostContent {
            body: "Check out our new feature!".to_string(),
            media_urls: vec![
                "https://example.com/img1.png".to_string(),
                "https://example.com/img2.png".to_string(),
            ],
            reply_to_id: None,
        };
        let json = serde_json::to_string(&post).unwrap();
        let parsed: SocialPostContent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.media_urls.len(), 2);
    }

    #[test]
    fn post_content_reply_to_id() {
        let post = SocialPostContent {
            body: "Replying to this!".to_string(),
            media_urls: vec![],
            reply_to_id: Some("tweet-12345".to_string()),
        };
        let json = serde_json::to_string(&post).unwrap();
        assert!(json.contains("reply_to_id"));
        let parsed: SocialPostContent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reply_to_id.as_deref(), Some("tweet-12345"));
    }
}
