// webhook.rs — `ta webhook` command for local testing of webhook triggers (v0.14.8.3).
//
// Usage:
//   ta webhook test github pull_request.closed --pr-url https://github.com/org/repo/pull/123
//   ta webhook test vcs changelist_submitted --change 12345 --depot //depot/main/...
//   ta webhook test vcs branch_pushed --branch main --repo org/repo

use anyhow::{bail, Result};
use clap::Subcommand;

use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum WebhookCommands {
    /// Simulate an inbound webhook event for local testing.
    ///
    /// Sends a test event to the TA daemon webhook endpoint without needing
    /// a real VCS event. Use this to verify your trigger configuration in
    /// workflow.toml before setting up a live webhook.
    ///
    /// Examples:
    ///   ta webhook test github pull_request.closed --pr 123 --branch main
    ///   ta webhook test vcs changelist_submitted --change 12345
    ///   ta webhook test vcs branch_pushed --branch main
    Test {
        /// Webhook provider: "github" or "vcs"
        provider: String,
        /// Event type:
        ///   github: pull_request.closed, push
        ///   vcs:    pr_merged, changelist_submitted, branch_pushed
        event: String,
        /// Pull request number (for PR events).
        #[arg(long, default_value = "1")]
        pr: u64,
        /// Branch name.
        #[arg(long, default_value = "main")]
        branch: String,
        /// Repository name (e.g., "org/repo").
        #[arg(long, default_value = "test-org/test-repo")]
        repo: String,
        /// Perforce depot path (for changelist_submitted).
        #[arg(long, default_value = "//depot/main/...")]
        depot: String,
        /// Perforce changelist number.
        #[arg(long, default_value = "1")]
        change: u64,
        /// Commit SHA to use in the event.
        #[arg(long, default_value = "0000000000000000000000000000000000000001")]
        sha: String,
        /// GitHub PR URL (informational — used to set pr_title in test events).
        #[arg(long)]
        pr_url: Option<String>,
    },
}

pub fn execute(command: &WebhookCommands, config: &GatewayConfig) -> Result<()> {
    match command {
        WebhookCommands::Test {
            provider,
            event,
            pr,
            branch,
            repo,
            depot,
            change,
            sha,
            pr_url,
        } => test_webhook(
            config,
            provider,
            event,
            *pr,
            branch,
            repo,
            depot,
            *change,
            sha,
            pr_url.as_deref(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn test_webhook(
    config: &GatewayConfig,
    provider: &str,
    event: &str,
    pr: u64,
    branch: &str,
    repo: &str,
    depot: &str,
    change: u64,
    sha: &str,
    pr_url: Option<&str>,
) -> Result<()> {
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    let client = reqwest::blocking::Client::new();

    let pr_title = pr_url
        .map(|u| format!("Test PR ({})", u))
        .unwrap_or_else(|| format!("Test PR #{}", pr));

    let (endpoint, body) = match (provider, event) {
        ("github", "pull_request.closed") | ("github", "pull_request") => {
            let body = serde_json::json!({
                "action": "closed",
                "pull_request": {
                    "merged": true,
                    "number": pr,
                    "title": pr_title,
                    "base": { "ref": branch },
                    "merge_commit_sha": sha,
                    "merged_by": { "login": "ta-test" }
                },
                "repository": { "full_name": repo }
            });
            (format!("{}/api/webhooks/github", daemon_url), body)
        }
        ("github", "push") => {
            let body = serde_json::json!({
                "ref": format!("refs/heads/{}", branch),
                "after": sha,
                "pusher": { "name": "ta-test" },
                "repository": { "full_name": repo }
            });
            (format!("{}/api/webhooks/github", daemon_url), body)
        }
        ("vcs", "pr_merged") => {
            let body = serde_json::json!({
                "event": "pr_merged",
                "payload": {
                    "repo": repo,
                    "branch": branch,
                    "pr_number": pr,
                    "pr_title": pr_title,
                    "merged_by": "ta-test",
                    "commit_sha": sha,
                    "provider": "vcs"
                }
            });
            (format!("{}/api/webhooks/vcs", daemon_url), body)
        }
        ("vcs", "changelist_submitted") => {
            let body = serde_json::json!({
                "event": "changelist_submitted",
                "payload": {
                    "depot_path": depot,
                    "change_number": change,
                    "submitter": "ta-test",
                    "description": "Test changelist submission",
                    "provider": "perforce"
                }
            });
            (format!("{}/api/webhooks/vcs", daemon_url), body)
        }
        ("vcs", "branch_pushed") => {
            let body = serde_json::json!({
                "event": "branch_pushed",
                "payload": {
                    "repo": repo,
                    "branch": branch,
                    "pushed_by": "ta-test",
                    "commit_sha": sha,
                    "provider": "vcs"
                }
            });
            (format!("{}/api/webhooks/vcs", daemon_url), body)
        }
        _ => bail!(
            "Unknown provider/event combination: {}/{}.\nSupported:\n  github pull_request.closed\n  github push\n  vcs pr_merged\n  vcs changelist_submitted\n  vcs branch_pushed",
            provider,
            event
        ),
    };

    println!("Sending test webhook: {}/{}", provider, event);
    println!("  Endpoint: POST {}", endpoint);
    println!("  Payload:  {}", serde_json::to_string_pretty(&body)?);
    println!();

    let response = client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .header("X-GitHub-Event", map_github_event_header(provider, event))
        .json(&body)
        .send();

    match response {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            if status.is_success() {
                let parsed: serde_json::Value = serde_json::from_str(&text)
                    .unwrap_or_else(|_| serde_json::json!({ "raw": text }));
                let result_status = parsed["status"].as_str().unwrap_or("ok");
                match result_status {
                    "ignored" => {
                        println!("Event sent but not matched by any workflow trigger.");
                        println!("Reason: {}", parsed["reason"].as_str().unwrap_or("unknown"));
                        println!();
                        println!(
                            "To trigger a workflow, add a [[trigger]] block to .ta/workflow.toml:"
                        );
                        println!("  [[trigger]]");
                        println!("  event = \"vcs.pr_merged\"");
                        println!("  workflow = \"governed-goal\"");
                    }
                    "ok" => {
                        println!("Webhook accepted");
                        if let Some(event_id) = parsed["event_id"].as_str() {
                            println!("  Event ID:   {}", event_id);
                        }
                        if let Some(event_type) = parsed["event_type"].as_str() {
                            println!("  Event type: {}", event_type);
                        }
                        println!();
                        println!("Check events: ta events list");
                    }
                    _ => {
                        println!("Response: {}", serde_json::to_string_pretty(&parsed)?);
                    }
                }
            } else {
                println!("Webhook rejected: HTTP {}", status);
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(err) = parsed["error"].as_str() {
                        println!("Error: {}", err);
                    }
                    if let Some(hint) = parsed["hint"].as_str() {
                        println!("Hint:  {}", hint);
                    }
                } else {
                    println!("Response: {}", text);
                }
            }
        }
        Err(e) => {
            if e.is_connect() {
                bail!(
                    "Could not connect to TA daemon at {}.\n\
                     Start the daemon with: ta daemon start\n\
                     Or check: ta daemon status",
                    daemon_url
                );
            }
            bail!("Webhook request failed: {}", e);
        }
    }

    Ok(())
}

fn map_github_event_header(provider: &str, event: &str) -> &'static str {
    if provider == "github" {
        match event {
            "pull_request.closed" | "pull_request" => "pull_request",
            "push" => "push",
            _ => "unknown",
        }
    } else {
        "unknown"
    }
}
