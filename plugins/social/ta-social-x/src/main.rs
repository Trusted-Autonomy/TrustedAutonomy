//! ta-social-x — X (Twitter) social media adapter plugin for Trusted Autonomy.
//!
//! Implements the TA social plugin protocol (JSON-over-stdio, version 1).
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Spawned fresh per call by ExternalSocialAdapter.
//!
//! ## Operations
//!
//! - `create_draft`     — Create a draft tweet via X API v2 draft endpoint
//! - `create_scheduled` — Schedule a tweet via X API v2 scheduled tweets
//! - `draft_status`     — Poll a draft/scheduled tweet status
//! - `health`           — Check connectivity and return connected handle
//! - `capabilities`     — Return the op list above
//!
//! ## API Tier Requirements
//!
//! Draft tweets (`create_draft`) require the X API **Basic tier** or higher.
//! The Free tier does not support draft tweet creation.
//! Scheduled tweets require the Basic tier or higher.
//!
//! Check your API access at: https://developer.twitter.com/en/portal/dashboard
//!
//! ## Credentials
//!
//! OAuth2 bearer token is read from the environment variable
//! `TA_SECRET_TA_SOCIAL_X` or from `~/.config/ta/secrets/ta-social_x`.
//!
//! Set up credentials with: `ta adapter setup social/x`
//!
//! ## Safety
//!
//! There is no `publish` operation. TA only creates drafts or schedules tweets;
//! the user publishes from their X account. This is enforced at the type level.

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
// X API v2 client
// ---------------------------------------------------------------------------

struct XClient {
    bearer_token: String,
    client: reqwest::blocking::Client,
}

impl XClient {
    fn new(bearer_token: String) -> Self {
        Self {
            bearer_token,
            client: reqwest::blocking::Client::new(),
        }
    }

    fn get_me(&self) -> Result<serde_json::Value, String> {
        let resp = self
            .client
            .get("https://api.twitter.com/2/users/me")
            .bearer_auth(&self.bearer_token)
            .send()
            .map_err(|e| format!("X API request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!(
                "X API error {}. Note: draft/scheduled tweets require Basic API tier or higher. body: {}",
                status, body
            ));
        }

        resp.json::<serde_json::Value>()
            .map_err(|e| format!("Failed to parse X user response: {}", e))
    }

    fn create_draft(&self, post: &PostContent) -> Result<String, String> {
        // X API v2: POST /2/tweets with status = "draft".
        // Requires Basic+ API tier.
        let mut body = serde_json::json!({
            "text": post.body,
            "status": "draft"
        });

        if let Some(ref reply_to) = post.reply_to_id {
            body["reply"] = serde_json::json!({ "in_reply_to_tweet_id": reply_to });
        }

        let resp = self
            .client
            .post("https://api.twitter.com/2/tweets")
            .bearer_auth(&self.bearer_token)
            .json(&body)
            .send()
            .map_err(|e| format!("X create draft request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            if status.as_u16() == 403 {
                return Err(format!(
                    "X API error {} — draft tweets require Basic API tier or higher. \
                     Upgrade at https://developer.twitter.com/en/portal/dashboard. body: {}",
                    status, body_text
                ));
            }
            return Err(format!("X API error {}: {}", status, body_text));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse X draft response: {}", e))?;

        let tweet_id = result["data"]["id"]
            .as_str()
            .ok_or_else(|| "X API did not return a tweet ID".to_string())?;

        Ok(format!("x-draft-{}", tweet_id))
    }

    fn create_scheduled(
        &self,
        post: &PostContent,
        scheduled_at: &str,
    ) -> Result<(String, String), String> {
        // X API v2: POST /2/tweets with scheduled_at.
        let mut body = serde_json::json!({
            "text": post.body,
            "scheduled_at": scheduled_at
        });

        if let Some(ref reply_to) = post.reply_to_id {
            body["reply"] = serde_json::json!({ "in_reply_to_tweet_id": reply_to });
        }

        let resp = self
            .client
            .post("https://api.twitter.com/2/tweets")
            .bearer_auth(&self.bearer_token)
            .json(&body)
            .send()
            .map_err(|e| format!("X create scheduled request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            if status.as_u16() == 403 {
                return Err(format!(
                    "X API error {} — scheduled tweets require Basic API tier or higher. body: {}",
                    status, body_text
                ));
            }
            return Err(format!("X API error {}: {}", status, body_text));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse X scheduled response: {}", e))?;

        let tweet_id = result["data"]["id"]
            .as_str()
            .ok_or_else(|| "X API did not return a tweet ID".to_string())?;

        Ok((
            format!("x-scheduled-{}", tweet_id),
            scheduled_at.to_string(),
        ))
    }

    fn get_draft_status(&self, draft_id: &str) -> Result<String, String> {
        let raw_id = draft_id
            .strip_prefix("x-draft-")
            .or_else(|| draft_id.strip_prefix("x-scheduled-"))
            .unwrap_or(draft_id);

        let url = format!("https://api.twitter.com/2/tweets/{}", raw_id);

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.bearer_token)
            .send()
            .map_err(|e| format!("X get tweet request failed: {}", e))?;

        if resp.status().as_u16() == 404 {
            return Ok("deleted".to_string());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("X API error {}: {}", status, body));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse X tweet status: {}", e))?;

        // Check tweet status field if available.
        let status = result["data"]["status"].as_str().unwrap_or("unknown");
        Ok(match status {
            "published" | "live" => "published",
            "draft" => "draft",
            "scheduled" => "draft", // Still pending publication
            "deleted" => "deleted",
            _ => "unknown",
        }
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

fn load_bearer_token() -> Result<String, String> {
    let env_key = "TA_SECRET_TA_SOCIAL_X";

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
        .join("ta-social_x");

    if path.exists() {
        let token = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read credential file: {}", e))?;
        return Ok(token.trim().to_string());
    }

    Err(format!(
        "No X bearer token found. Set {} or run: ta adapter setup social/x\n\
         Note: draft and scheduled tweets require X API Basic tier or higher.",
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
            let token = match load_bearer_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = XClient::new(token);
            match client.get_me() {
                Ok(me) => {
                    let handle = me["data"]["username"]
                        .as_str()
                        .map(|u| format!("@{}", u))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let mut resp = Response::ok_empty();
                    resp.handle = Some(handle);
                    resp.provider = Some("x".to_string());
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("X health check failed: {}", e))),
            }
        }

        Request::CreateDraft { post } => {
            let token = match load_bearer_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = XClient::new(token);
            match client.create_draft(&post) {
                Ok(draft_id) => {
                    let mut resp = Response::ok_empty();
                    resp.draft_id = Some(draft_id);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("X create_draft failed: {}", e))),
            }
        }

        Request::CreateScheduled { post, scheduled_at } => {
            let token = match load_bearer_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = XClient::new(token);
            match client.create_scheduled(&post, &scheduled_at) {
                Ok((scheduled_id, at)) => {
                    let mut resp = Response::ok_empty();
                    resp.scheduled_id = Some(scheduled_id);
                    resp.scheduled_at = Some(at);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("X create_scheduled failed: {}", e))),
            }
        }

        Request::DraftStatus { draft_id } => {
            let token = match load_bearer_token() {
                Ok(t) => t,
                Err(e) => {
                    respond(Response::err(e));
                    return;
                }
            };
            let client = XClient::new(token);
            match client.get_draft_status(&draft_id) {
                Ok(state) => {
                    let mut resp = Response::ok_empty();
                    resp.state = Some(state);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("X draft_status failed: {}", e))),
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
