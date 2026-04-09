//! ta-social-buffer — Buffer social media adapter plugin for Trusted Autonomy.
//!
//! Implements the TA social plugin protocol (JSON-over-stdio, version 1).
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Spawned fresh per call by ExternalSocialAdapter.
//!
//! ## Operations
//!
//! - `create_draft`     — Create a draft post in Buffer's draft queue
//! - `create_scheduled` — Add a post to Buffer's scheduled queue
//! - `draft_status`     — Poll a Buffer post's current state
//! - `health`           — Check connectivity and return connected handle
//! - `capabilities`     — Return the op list above
//!
//! ## Cross-platform fan-out
//!
//! Buffer supports multiple connected profiles (LinkedIn, X, Instagram, Facebook).
//! A single `create_draft` or `create_scheduled` call creates the post across
//! ALL profiles linked to the configured Buffer channel. This means one goal
//! can produce drafts on LinkedIn, X, and Instagram simultaneously.
//!
//! ## Credentials
//!
//! Buffer access token is read from the environment variable
//! `TA_SECRET_TA_SOCIAL_BUFFER` or from `~/.config/ta/secrets/ta-social_buffer`.
//!
//! Set up credentials with: `ta adapter setup social/buffer`
//!
//! ## Safety
//!
//! There is no `publish` operation. TA only creates drafts or queues scheduled
//! posts; Buffer controls the actual send schedule. This is enforced at the type level.

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
// Buffer API client (v1)
// ---------------------------------------------------------------------------

struct BufferClient {
    access_token: String,
    client: reqwest::blocking::Client,
}

impl BufferClient {
    fn new(access_token: String) -> Self {
        Self {
            access_token,
            client: reqwest::blocking::Client::new(),
        }
    }

    fn get_user(&self) -> Result<serde_json::Value, String> {
        let resp = self
            .client
            .get("https://api.bufferapp.com/1/user.json")
            .bearer_auth(&self.access_token)
            .send()
            .map_err(|e| format!("Buffer API request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("Buffer API error {}: {}", status, body));
        }

        resp.json::<serde_json::Value>()
            .map_err(|e| format!("Failed to parse Buffer user response: {}", e))
    }

    fn get_profiles(&self) -> Result<Vec<serde_json::Value>, String> {
        let resp = self
            .client
            .get("https://api.bufferapp.com/1/profiles.json")
            .bearer_auth(&self.access_token)
            .send()
            .map_err(|e| format!("Buffer get profiles request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("Buffer API error {}: {}", status, body));
        }

        let profiles: Vec<serde_json::Value> = resp
            .json()
            .map_err(|e| format!("Failed to parse Buffer profiles: {}", e))?;

        Ok(profiles)
    }

    fn create_draft(&self, post: &PostContent) -> Result<String, String> {
        // Get all profile IDs to fan out across all connected platforms.
        let profiles = self.get_profiles()?;
        if profiles.is_empty() {
            return Err(
                "No Buffer profiles connected. Connect profiles at https://buffer.com".to_string(),
            );
        }

        let profile_ids: Vec<String> = profiles
            .iter()
            .filter_map(|p| p["id"].as_str().map(str::to_string))
            .collect();

        // Buffer v1: POST /1/updates/create.json with now=false to create a draft.
        let form: Vec<(&str, String)> = profile_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (Box::leak(format!("profile_ids[{}]", i).into_boxed_str()) as &str, id.clone()))
            .chain([
                ("text", post.body.clone()),
                ("now", "false".to_string()),
                ("top", "false".to_string()),
            ])
            .collect();

        let resp = self
            .client
            .post("https://api.bufferapp.com/1/updates/create.json")
            .bearer_auth(&self.access_token)
            .form(&form)
            .send()
            .map_err(|e| format!("Buffer create draft request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("Buffer API error {}: {}", status, body));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse Buffer create response: {}", e))?;

        // Buffer returns updates array; use the first update's ID.
        let update_id = result["updates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|u| u["id"].as_str())
            .unwrap_or("buffer-unknown");

        Ok(format!("buffer-draft-{}", update_id))
    }

    fn create_scheduled(
        &self,
        post: &PostContent,
        scheduled_at: &str,
    ) -> Result<(String, String), String> {
        let profiles = self.get_profiles()?;
        if profiles.is_empty() {
            return Err(
                "No Buffer profiles connected. Connect profiles at https://buffer.com".to_string(),
            );
        }

        let profile_ids: Vec<String> = profiles
            .iter()
            .filter_map(|p| p["id"].as_str().map(str::to_string))
            .collect();

        // Buffer v1: POST /1/updates/create.json with scheduled_at.
        let mut form: Vec<(String, String)> = profile_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (format!("profile_ids[{}]", i), id.clone()))
            .collect();

        form.push(("text".to_string(), post.body.clone()));
        form.push(("scheduled_at".to_string(), scheduled_at.to_string()));

        let form_refs: Vec<(&str, &str)> = form
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let resp = self
            .client
            .post("https://api.bufferapp.com/1/updates/create.json")
            .bearer_auth(&self.access_token)
            .form(&form_refs)
            .send()
            .map_err(|e| format!("Buffer create scheduled request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("Buffer API error {}: {}", status, body));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse Buffer scheduled response: {}", e))?;

        let update_id = result["updates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|u| u["id"].as_str())
            .unwrap_or("buffer-unknown");

        let confirmed_at = result["updates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|u| u["scheduled_at"].as_str())
            .unwrap_or(scheduled_at)
            .to_string();

        Ok((
            format!("buffer-scheduled-{}", update_id),
            confirmed_at,
        ))
    }

    fn get_draft_status(&self, draft_id: &str) -> Result<String, String> {
        let raw_id = draft_id
            .strip_prefix("buffer-draft-")
            .or_else(|| draft_id.strip_prefix("buffer-scheduled-"))
            .unwrap_or(draft_id);

        let url = format!(
            "https://api.bufferapp.com/1/updates/{}.json",
            raw_id
        );

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .map_err(|e| format!("Buffer get update request failed: {}", e))?;

        if resp.status().as_u16() == 404 {
            return Ok("deleted".to_string());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("Buffer API error {}: {}", status, body));
        }

        let result: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Failed to parse Buffer update status: {}", e))?;

        let status = result["status"].as_str().unwrap_or("unknown");
        Ok(match status {
            "sent" => "published",
            "draft" | "buffer" | "pending" | "scheduled" => "draft",
            _ => "unknown",
        }
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

fn load_access_token() -> Result<String, String> {
    let env_key = "TA_SECRET_TA_SOCIAL_BUFFER";

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
        .join("ta-social_buffer");

    if path.exists() {
        let token = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read credential file: {}", e))?;
        return Ok(token.trim().to_string());
    }

    Err(format!(
        "No Buffer access token found. Set {} or run: ta adapter setup social/buffer",
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
            let client = BufferClient::new(token);
            match client.get_user() {
                Ok(user) => {
                    let handle = user["name"]
                        .as_str()
                        .unwrap_or_else(|| user["id"].as_str().unwrap_or("<unknown>"))
                        .to_string();
                    let mut resp = Response::ok_empty();
                    resp.handle = Some(handle);
                    resp.provider = Some("buffer".to_string());
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("Buffer health check failed: {}", e))),
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
            let client = BufferClient::new(token);
            match client.create_draft(&post) {
                Ok(draft_id) => {
                    let mut resp = Response::ok_empty();
                    resp.draft_id = Some(draft_id);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!("Buffer create_draft failed: {}", e))),
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
            let client = BufferClient::new(token);
            match client.create_scheduled(&post, &scheduled_at) {
                Ok((scheduled_id, at)) => {
                    let mut resp = Response::ok_empty();
                    resp.scheduled_id = Some(scheduled_id);
                    resp.scheduled_at = Some(at);
                    respond(resp);
                }
                Err(e) => respond(Response::err(format!(
                    "Buffer create_scheduled failed: {}",
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
            let client = BufferClient::new(token);
            match client.get_draft_status(&draft_id) {
                Ok(state) => {
                    let mut resp = Response::ok_empty();
                    resp.state = Some(state);
                    respond(resp);
                }
                Err(e) => {
                    respond(Response::err(format!("Buffer draft_status failed: {}", e)))
                }
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
