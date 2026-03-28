// api/webhooks.rs — Inbound webhook handlers for VCS event integration (v0.14.8.3).
//
// Endpoints:
//   POST /api/webhooks/github  — GitHub webhook with X-Hub-Signature-256 validation
//   POST /api/webhooks/vcs     — Generic VCS webhook for Perforce triggers and git hooks
//
// Both endpoints map incoming events to TA SessionEvents, write them to the
// event store (events.jsonl), and are available for workflow trigger matching.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::net::SocketAddr;

use ta_events::schema::{EventEnvelope, SessionEvent};
use ta_events::store::{EventStore, FsEventStore};

use crate::api::AppState;

// ── GitHub webhook ─────────────────────────────────────────────────────────

/// POST /api/webhooks/github
///
/// Receives GitHub webhook events, validates the HMAC-SHA256 signature, maps
/// event types to TA SessionEvents, and writes them to events.jsonl.
///
/// GitHub sends:
///   X-GitHub-Event:     the event type (e.g., "pull_request", "push")
///   X-Hub-Signature-256: "sha256=<hex-digest>" of the payload using the webhook secret
///
/// Config: `[webhooks.github] secret = "..."` in `.ta/daemon.toml`.
pub async fn github_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let secret = &state.daemon_config.webhooks.github.secret;

    // Validate signature if a secret is configured.
    if !secret.is_empty() {
        let sig_header = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_github_signature(secret.as_bytes(), &body, sig_header) {
            tracing::warn!(
                "GitHub webhook signature validation failed — check webhook secret in daemon.toml"
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid signature. Verify that [webhooks.github] secret in daemon.toml matches the GitHub webhook secret.",
                    "hint": "ta config show webhooks.github.secret"
                })),
            )
                .into_response();
        }
    } else {
        tracing::warn!(
            "GitHub webhook received without signature validation — set [webhooks.github] secret in daemon.toml for production use"
        );
    }

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("GitHub webhook: failed to parse JSON payload: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid JSON payload: {}", e)
                })),
            )
                .into_response();
        }
    };

    tracing::info!(
        event_type = %event_type,
        "GitHub webhook received"
    );

    let ta_event = map_github_event(&event_type, &payload);

    match ta_event {
        Some(event) => {
            let event_type_str = event.event_type();
            let store = FsEventStore::new(&state.events_dir);
            let envelope = EventEnvelope::new(event);
            if let Err(e) = store.append(&envelope) {
                tracing::error!("Failed to persist webhook event: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to write event to store: {}", e)
                    })),
                )
                    .into_response();
            }
            tracing::info!(
                event_type = %event_type_str,
                event_id = %envelope.id,
                "VCS event written to store"
            );
            Json(serde_json::json!({
                "status": "ok",
                "event_type": event_type_str,
                "event_id": envelope.id,
            }))
            .into_response()
        }
        None => {
            // Event is recognized but not mapped (e.g., PR opened — we only care about merged).
            Json(serde_json::json!({
                "status": "ignored",
                "github_event": event_type,
                "reason": "Event type does not map to a TA workflow trigger"
            }))
            .into_response()
        }
    }
}

/// Map a GitHub event type + JSON payload to a TA `SessionEvent`.
///
/// Currently mapped:
///   pull_request (action=closed, merged=true) → VcsPrMerged
///   push                                       → VcsBranchPushed (non-tag pushes only)
fn map_github_event(event_type: &str, payload: &serde_json::Value) -> Option<SessionEvent> {
    match event_type {
        "pull_request" => {
            let action = payload["action"].as_str().unwrap_or("");
            let merged = payload["pull_request"]["merged"].as_bool().unwrap_or(false);
            if action == "closed" && merged {
                Some(SessionEvent::VcsPrMerged {
                    repo: payload["repository"]["full_name"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    branch: payload["pull_request"]["base"]["ref"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    pr_number: payload["pull_request"]["number"].as_u64().unwrap_or(0),
                    pr_title: payload["pull_request"]["title"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    merged_by: payload["pull_request"]["merged_by"]["login"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    commit_sha: payload["pull_request"]["merge_commit_sha"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    provider: "github".to_string(),
                })
            } else {
                None
            }
        }
        "push" => {
            let r#ref = payload["ref"].as_str().unwrap_or("");
            // Skip tag pushes (refs/tags/...) — only handle branch pushes.
            if r#ref.starts_with("refs/tags/") {
                return None;
            }
            let branch = r#ref.strip_prefix("refs/heads/").unwrap_or(r#ref);
            // Skip empty pushes (delete events).
            let after = payload["after"].as_str().unwrap_or("");
            if after == "0000000000000000000000000000000000000000" {
                return None;
            }
            Some(SessionEvent::VcsBranchPushed {
                repo: payload["repository"]["full_name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                branch: branch.to_string(),
                pushed_by: payload["pusher"]["name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                commit_sha: after.to_string(),
                provider: "github".to_string(),
            })
        }
        _ => None,
    }
}

/// Verify a GitHub webhook signature.
///
/// GitHub sends `X-Hub-Signature-256: sha256=<hex>`.
/// We compute HMAC-SHA256 of the raw body using the secret and compare.
/// Uses a constant-time comparison to prevent timing attacks.
fn verify_github_signature(secret: &[u8], body: &[u8], signature_header: &str) -> bool {
    let expected_hex = match signature_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    // Compute HMAC-SHA256: hash(secret || hash(body)) using a hand-rolled HMAC
    // to avoid pulling in the hmac crate. Uses SHA-256 (sha2 already in workspace).
    let computed = hmac_sha256(secret, body);
    let computed_hex = hex_encode(&computed);

    // Constant-time comparison.
    constant_time_eq(computed_hex.as_bytes(), expected_hex.as_bytes())
}

/// Compute HMAC-SHA256(key, message) using only sha2.
/// Implements RFC 2104 directly.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;

    // Normalize key: hash it if longer than block size.
    let mut k = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hash = sha2::Sha256::digest(key);
        k[..32].copy_from_slice(&hash);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    // ipad and opad.
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    // inner = SHA256(ipad || message)
    let mut inner_hasher = sha2::Sha256::new();
    inner_hasher.update(ipad);
    inner_hasher.update(message);
    let inner = inner_hasher.finalize();

    // outer = SHA256(opad || inner)
    let mut outer_hasher = sha2::Sha256::new();
    outer_hasher.update(opad);
    outer_hasher.update(inner);
    outer_hasher.finalize().into()
}

/// Lowercase hex encode.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time byte slice comparison (timing-safe).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Generic VCS webhook ─────────────────────────────────────────────────────

/// Payload for the generic VCS webhook endpoint.
///
/// Used by Perforce trigger scripts and custom git post-receive hooks.
#[derive(Debug, Deserialize, Serialize)]
pub struct VcsWebhookPayload {
    /// Event type: "pr_merged", "changelist_submitted", "branch_pushed"
    pub event: String,
    /// Event payload (provider-specific fields).
    pub payload: serde_json::Value,
    /// Optional HMAC-SHA256 signature for authentication (hex, no prefix).
    #[serde(default)]
    pub signature: Option<String>,
}

/// POST /api/webhooks/vcs
///
/// Generic VCS webhook endpoint for Perforce triggers and custom git hooks.
/// Accepts JSON with `{ event, payload, signature? }`.
///
/// If `[webhooks.vcs] secret` is set, the `signature` field is required and
/// validated as HMAC-SHA256 of the raw body.
///
/// For localhost-only calls (e.g., from git hooks on the same machine),
/// the secret can be left empty and signature validation is skipped.
pub async fn vcs_webhook(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let config = &state.daemon_config.webhooks.vcs;
    let is_localhost = addr.ip().is_loopback();

    // Validate signature if secret is configured, or reject non-localhost if no secret.
    if !config.secret.is_empty() {
        // Extract signature from X-TA-Signature header (preferred) or body field.
        let sig = headers
            .get("x-ta-signature")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let sig = match sig {
            Some(s) => s,
            None => {
                // Try to parse body to get signature field.
                if let Ok(parsed) = serde_json::from_slice::<VcsWebhookPayload>(&body) {
                    parsed.signature.unwrap_or_default()
                } else {
                    String::new()
                }
            }
        };

        if !verify_github_signature(config.secret.as_bytes(), &body, &format!("sha256={}", sig)) {
            tracing::warn!(
                remote_addr = %addr,
                "VCS webhook signature validation failed"
            );
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid or missing signature. Set X-TA-Signature header with HMAC-SHA256 of the request body.",
                    "hint": "See scripts/ta-p4-trigger.sh for an example of how to compute the signature."
                })),
            )
                .into_response();
        }
    } else if !is_localhost {
        tracing::warn!(
            remote_addr = %addr,
            "VCS webhook from non-localhost without secret configured — rejecting. \
             Set [webhooks.vcs] secret in daemon.toml or restrict to localhost only."
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "VCS webhook from non-localhost requires a secret. \
                          Set [webhooks.vcs] secret in .ta/daemon.toml.",
                "hint": "For localhost-only scripts (git hooks on this machine), no secret is needed."
            })),
        )
            .into_response();
    }

    let webhook: VcsWebhookPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid JSON payload: {}", e),
                    "expected": { "event": "pr_merged|changelist_submitted|branch_pushed", "payload": {} }
                })),
            )
                .into_response();
        }
    };

    tracing::info!(
        event = %webhook.event,
        remote_addr = %addr,
        "VCS webhook received"
    );

    let ta_event = map_vcs_event(&webhook.event, &webhook.payload);

    match ta_event {
        Some(event) => {
            let event_type_str = event.event_type();
            let store = FsEventStore::new(&state.events_dir);
            let envelope = EventEnvelope::new(event);
            if let Err(e) = store.append(&envelope) {
                tracing::error!("Failed to persist VCS webhook event: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to write event: {}", e)
                    })),
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "status": "ok",
                "event_type": event_type_str,
                "event_id": envelope.id,
            }))
            .into_response()
        }
        None => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Unknown event type: '{}'", webhook.event),
                "supported": ["pr_merged", "changelist_submitted", "branch_pushed"]
            })),
        )
            .into_response(),
    }
}

/// Map a generic VCS event to a TA `SessionEvent`.
fn map_vcs_event(event: &str, payload: &serde_json::Value) -> Option<SessionEvent> {
    match event {
        "pr_merged" => Some(SessionEvent::VcsPrMerged {
            repo: payload["repo"].as_str().unwrap_or("unknown").to_string(),
            branch: payload["branch"].as_str().unwrap_or("unknown").to_string(),
            pr_number: payload["pr_number"].as_u64().unwrap_or(0),
            pr_title: payload["pr_title"].as_str().unwrap_or("").to_string(),
            merged_by: payload["merged_by"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            commit_sha: payload["commit_sha"].as_str().unwrap_or("").to_string(),
            provider: payload["provider"].as_str().unwrap_or("vcs").to_string(),
        }),
        "branch_pushed" => Some(SessionEvent::VcsBranchPushed {
            repo: payload["repo"].as_str().unwrap_or("unknown").to_string(),
            branch: payload["branch"].as_str().unwrap_or("unknown").to_string(),
            pushed_by: payload["pushed_by"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            commit_sha: payload["commit_sha"].as_str().unwrap_or("").to_string(),
            provider: payload["provider"].as_str().unwrap_or("vcs").to_string(),
        }),
        "changelist_submitted" => Some(SessionEvent::VcsChangelistSubmitted {
            depot_path: payload["depot_path"]
                .as_str()
                .unwrap_or("//depot/...")
                .to_string(),
            change_number: payload["change_number"].as_u64().unwrap_or(0),
            submitter: payload["submitter"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            description: payload["description"].as_str().unwrap_or("").to_string(),
        }),
        _ => None,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_known_vector() {
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog")
        // = f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8
        let key = b"key";
        let msg = b"The quick brown fox jumps over the lazy dog";
        let result = hmac_sha256(key, msg);
        let hex = hex_encode(&result);
        assert_eq!(
            hex,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn verify_signature_valid() {
        let secret = b"test-secret";
        let body = b"hello world";
        let mac = hmac_sha256(secret, body);
        let sig = format!("sha256={}", hex_encode(&mac));
        assert!(verify_github_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_invalid() {
        assert!(!verify_github_signature(
            b"secret",
            b"body",
            "sha256=badhex"
        ));
        assert!(!verify_github_signature(b"secret", b"body", "noprefixhere"));
        assert!(!verify_github_signature(
            b"secret",
            b"body",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000"
        ));
    }

    #[test]
    fn map_github_pr_merged() {
        let payload = serde_json::json!({
            "action": "closed",
            "pull_request": {
                "merged": true,
                "number": 42,
                "title": "Add feature",
                "base": { "ref": "main" },
                "merge_commit_sha": "abc123",
                "merged_by": { "login": "alice" }
            },
            "repository": { "full_name": "org/repo" }
        });
        let event = map_github_event("pull_request", &payload).unwrap();
        assert_eq!(event.event_type(), "vcs.pr_merged");
        if let SessionEvent::VcsPrMerged {
            pr_number,
            pr_title,
            branch,
            ..
        } = event
        {
            assert_eq!(pr_number, 42);
            assert_eq!(pr_title, "Add feature");
            assert_eq!(branch, "main");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn map_github_pr_closed_not_merged() {
        let payload = serde_json::json!({
            "action": "closed",
            "pull_request": { "merged": false, "number": 1, "title": "", "base": { "ref": "main" }, "merge_commit_sha": "", "merged_by": null },
            "repository": { "full_name": "org/repo" }
        });
        assert!(map_github_event("pull_request", &payload).is_none());
    }

    #[test]
    fn map_github_push() {
        let payload = serde_json::json!({
            "ref": "refs/heads/feature-x",
            "after": "deadbeef",
            "pusher": { "name": "bob" },
            "repository": { "full_name": "org/repo" }
        });
        let event = map_github_event("push", &payload).unwrap();
        assert_eq!(event.event_type(), "vcs.branch_pushed");
        if let SessionEvent::VcsBranchPushed { branch, .. } = event {
            assert_eq!(branch, "feature-x");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn map_github_push_tag_ignored() {
        let payload = serde_json::json!({
            "ref": "refs/tags/v1.0.0",
            "after": "deadbeef",
            "pusher": { "name": "bob" },
            "repository": { "full_name": "org/repo" }
        });
        assert!(map_github_event("push", &payload).is_none());
    }

    #[test]
    fn map_vcs_changelist() {
        let payload = serde_json::json!({
            "depot_path": "//depot/main/...",
            "change_number": 12345,
            "submitter": "alice",
            "description": "Fix the login bug"
        });
        let event = map_vcs_event("changelist_submitted", &payload).unwrap();
        assert_eq!(event.event_type(), "vcs.changelist_submitted");
        if let SessionEvent::VcsChangelistSubmitted { change_number, .. } = event {
            assert_eq!(change_number, 12345);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn map_vcs_unknown_event_returns_none() {
        let payload = serde_json::json!({});
        assert!(map_vcs_event("unknown_event", &payload).is_none());
    }
}
