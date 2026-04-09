//! ta-social-linkedin — LinkedIn social media adapter plugin for Trusted Autonomy.
//!
//! Implements the TA social plugin protocol (JSON-over-stdio, version 1).
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Spawned fresh per call by ExternalSocialAdapter.
//!
//! ## Operations
//!
//! - `create_draft`     — Create a draft post via LinkedIn Draft Share API
//! - `create_scheduled` — Schedule a post via LinkedIn Scheduled Share
//! - `draft_status`     — Poll a draft via LinkedIn Share status endpoint
//! - `health`           — Check connectivity and return connected handle
//! - `capabilities`     — Return the op list above
//!
//! ## Credentials
//!
//! OAuth2 access token is read from the environment variable
//! `TA_SECRET_TA_SOCIAL_LINKEDIN_<HANDLE>` (set by `ta adapter credentials get`).
//! Alternatively, falls back to `~/.config/ta/secrets/ta-social_linkedin_<handle>`.
//!
//! Set up credentials with: `ta adapter setup social/linkedin`
//!
//! ## Safety
//!
//! There is no `publish` operation. TA only creates drafts or schedules posts;
//! the user publishes from their LinkedIn account. This is enforced at the type level.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol types (inline to avoid workspace dependency)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    CreateDraft {
        post: PostContent,
    },
    CreateScheduled {
        post: PostContent,
        scheduled_at: String,
    },
    DraftStatus {
        draft_id: String,
    },
    Health {},
    Capabilities {},
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct PostContent {
    body: String,
    #[serde(default)]
    media_urls: Vec<String>,
    reply_to_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduled_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Vec<String>>,
}

impl Response {
    fn ok_empty() -> Self {
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

    fn err(msg: impl Into<String>) -> Self {
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
// LinkedIn API client
// ---------------------------------------------------------------------------

struct LinkedInClient {
    access_token: String,
    client: reqwest::blocking::Client,
}

impl LinkedInClient {
    fn new(access_token: String) -> Self {
        Self {
            access_token,
            client: reqwest::blocking::Client::new(),
        }
    }

    fn get_profile(&self) -> Result<serde_json::Value, String> {
        let resp = self
            .client
            .get("https://api.linkedin.com/v2/userinfo")
            .bearer_auth(&self.access_token)
            .send()
            .map_err(|e| format!("LinkedIn API request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("LinkedIn API error {}: {}", status, body));
        }

        resp.json::<serde_json::Value>()
            .map_err(|e| format!("Failed to parse LinkedIn profile response: {}", e))
    }

    fn create_draft(&self, author_urn: &str, post: &PostContent) -> Result<String, String> {
        // LinkedIn Share API: create a draft post (lifecycleState = DRAFT).
        let body = serde_json::json!({
            "author": author_urn,
            "lifecycleState": "DRAFT",
            "specificContent": {
                "com.linkedin.ugc.ShareContent": {
                    "shareCommentary": {
                        "text": post.body
                    },
                    "shareMediaCategory": "NONE"
                }
            },
            "visibility": {
                "com.linkedin.ugc.MemberNetworkVisibility": "PUBLIC"
            }
        });

        let resp = self
            .client
            .post("https://api.linkedin.com/v2/ugcPosts")
            .bearer_auth(&self.access_token)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .json(&body)
            .send()
            .map_err(|e| format!("LinkedIn create draft request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("LinkedIn API error {}: {}", status, body));
        }

        // The post URN is in the X-RestLi-Id response header.
        let post_urn = resp
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "linkedin-draft-unknown".to_string());

        Ok(format!("linkedin-draft-{}", urlencoded_urn(&post_urn)))
    }

    fn create_scheduled(
        &self,
        author_urn: &str,
        post: &PostContent,
        scheduled_at: &str,
    ) -> Result<(String, String), String> {
        // LinkedIn Scheduled Share: lifecycleState = SCHEDULED with publishedAt.
        let body = serde_json::json!({
            "author": author_urn,
            "lifecycleState": "SCHEDULED",
            "scheduledPublishTime": scheduled_at,
            "specificContent": {
                "com.linkedin.ugc.ShareContent": {
                    "shareCommentary": {
                        "text": post.body
                    },
                    "shareMediaCategory": "NONE"
                }
            },
            "visibility": {
                "com.linkedin.ugc.MemberNetworkVisibility": "PUBLIC"
            }
        });

        let resp = self
            .client
            .post("https://api.linkedin.com/v2/ugcPosts")
            .bearer_auth(&self.access_token)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .json(&body)
            .send()
            .map_err(|e| format!("LinkedIn create scheduled request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("LinkedIn API error {}: {}", status, body));
        }

        let post_urn = resp
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "linkedin-scheduled-unknown".to_string());

        Ok((
            format!("linkedin-scheduled-{}", urlencoded_urn(&post_urn)),
            scheduled_at.to_string(),
        ))
    }

    fn get_draft_status(&self, draft_id: &str) -> Result<String, String> {
        // Strip our prefix to get the raw LinkedIn post URN.
        let raw_urn = draft_id
            .strip_prefix("linkedin-draft-")
            .or_else(|| draft_id.strip_prefix("linkedin-scheduled-"))
            .unwrap_or(draft_id);

        // Decode percent-encoding to get the original URN.
        let decoded_urn = percent_decode(raw_urn);

        let url = format!(
            "https://api.linkedin.com/v2/ugcPosts/{}",
            urlencoded_urn(&decoded_urn)
        );

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .send()
            .map_err(|e| format!("LinkedIn get draft request failed: {}", e))?;

        if resp.status().as_u16() == 404 {
            return Ok("deleted".to_string());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("LinkedIn API error {}: {}", status, body));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse draft status: {}", e))?;

        let state = result["lifecycleState"].as_str().unwrap_or("unknown");
        Ok(match state {
            "PUBLISHED" => "published",
            "DRAFT" | "SCHEDULED" => "draft",
            _ => "unknown",
        }
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn urlencoded_urn(urn: &str) -> String {
    urn.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ':' => "%3A".to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    result.push(byte as char);
                    i += 3;
                    continue;
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn load_access_token() -> Result<String, String> {
    let env_key = "TA_SECRET_TA_SOCIAL_LINKEDIN";

    if let Ok(token) = std::env::var(env_key) {
        return Ok(token);
    }

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "Cannot determine home directory for credential lookup".to_string())?;

    let path = std::path::PathBuf::from(home)
        .join(".config")
        .join("ta")
        .join("secrets")
        .join("ta-social_linkedin");

    if path.exists() {
        let token = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read credential file: {}", e))?;
        return Ok(token.trim().to_string());
    }

    Err(format!(
        "No LinkedIn access token found. Set {} or run: ta adapter setup social/linkedin",
        env_key
    ))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let stdin = std::io::stdin();
    let mut line = String::new();
    if let Err(e) = stdin.lock().read_line(&mut line) {
        respond(Response::err(format!("Failed to read stdin: {}", e)));
        return;
    }

    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            respond(Response::err(format!(
                "Invalid request JSON: {}. Got: '{}'",
                e,
                line.trim()
            )));
            return;
        }
    };

    match request {
        Request::Capabilities {} => {
            let mut resp = Response::ok_empty();
            resp.capabilities = Some(vec![
                "create_draft".to_string(),
                "create_scheduled".to_string(),
                "draft_status".to_string(),
                "health".to_string(),
                "capabilities".to_string(),
            ]);
            respond(resp);
        }

        Request::Health {} => {
            let token = match load_access_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = LinkedInClient::new(token);
            match client.get_profile() {
                Ok(profile) => {
                    let handle = profile["name"]
                        .as_str()
                        .or_else(|| profile["sub"].as_str())
                        .unwrap_or("<unknown>")
                        .to_string();
                    let mut resp = Response::ok_empty();
                    resp.handle = Some(handle);
                    resp.provider = Some("linkedin".to_string());
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("LinkedIn health check failed: {}", e))),
            }
        }

        Request::CreateDraft { post } => {
            let token = match load_access_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = LinkedInClient::new(token.clone());
            // Get author URN from profile.
            let author_urn = match client.get_profile() {
                Ok(p) => p["sub"]
                    .as_str()
                    .map(|sub| format!("urn:li:person:{}", sub))
                    .unwrap_or_else(|| "urn:li:person:unknown".to_string()),
                Err(e) => {
                    respond(Response::err(format!("Failed to get LinkedIn profile: {}", e)));
                    return;
                }
            };
            match client.create_draft(&author_urn, &post) {
                Ok(draft_id) => {
                    let mut resp = Response::ok_empty();
                    resp.draft_id = Some(draft_id);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!(
                    "LinkedIn create_draft failed: {}",
                    e
                ))),
            }
        }

        Request::CreateScheduled { post, scheduled_at } => {
            let token = match load_access_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = LinkedInClient::new(token);
            let author_urn = match client.get_profile() {
                Ok(p) => p["sub"]
                    .as_str()
                    .map(|sub| format!("urn:li:person:{}", sub))
                    .unwrap_or_else(|| "urn:li:person:unknown".to_string()),
                Err(e) => {
                    respond(Response::err(format!("Failed to get LinkedIn profile: {}", e)));
                    return;
                }
            };
            match client.create_scheduled(&author_urn, &post, &scheduled_at) {
                Ok((scheduled_id, at)) => {
                    let mut resp = Response::ok_empty();
                    resp.scheduled_id = Some(scheduled_id);
                    resp.scheduled_at = Some(at);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!(
                    "LinkedIn create_scheduled failed: {}",
                    e
                ))),
            }
        }

        Request::DraftStatus { draft_id } => {
            let token = match load_access_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = LinkedInClient::new(token);
            match client.get_draft_status(&draft_id) {
                Ok(state) => {
                    let mut resp = Response::ok_empty();
                    resp.state = Some(state);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!(
                    "LinkedIn draft_status failed: {}",
                    e
                ))),
            }
        }
    }
}

fn respond(resp: Response) {
    let json = serde_json::to_string(&resp).unwrap_or_else(|e| {
        format!(
            r#"{{"ok":false,"error":"Failed to serialize response: {}"}}"#,
            e
        )
    });
    println!("{}", json);
    let _ = std::io::stdout().flush();
}
