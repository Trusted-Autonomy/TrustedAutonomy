//! The MCP gateway's daemon HTTP client — first introduced for the
//! daemon-hosted whiteboard (see `docs/superpowers/specs/
//! 2026-09-11-daemon-hosted-whiteboard-design.md`). Before this, no
//! `ta-mcp-gateway` code called the daemon's HTTP API at all; every tool
//! handler (including `whiteboard_check.rs`'s pre-launch conflict check)
//! operated on local filesystem/library state directly.

use std::path::Path;

use anyhow::{Context, Result};
use ta_agent_whiteboard::presence::PresenceRecord;

/// PID-file path: `.ta/daemon.pid`. Mirrors `apps/ta-cli/src/commands/
/// daemon.rs`'s equivalent — duplicated rather than shared because that
/// logic lives in a binary target this library crate cannot depend on.
fn pid_path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".ta").join("daemon.pid")
}

pub fn read_pid_port(project_root: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(pid_path(project_root)).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("port="))
        .and_then(|l| l.strip_prefix("port="))
        .and_then(|s| s.parse::<u16>().ok())
}

pub fn resolve_daemon_url(project_root: &Path) -> String {
    let port = read_pid_port(project_root).unwrap_or(7700);
    format!("http://127.0.0.1:{port}")
}

pub struct WhiteboardDaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl WhiteboardDaemonClient {
    pub fn new(project_root: &Path) -> Self {
        Self {
            base_url: resolve_daemon_url(project_root),
            client: reqwest::Client::new(),
        }
    }

    pub async fn register_presence(
        &self,
        token: &str,
        team_session: &str,
        record: &PresenceRecord,
    ) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/presence", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "record": record,
            }))
            .send()
            .await
            .context("whiteboard presence register: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "whiteboard presence register failed: HTTP {}",
                resp.status()
            );
        }
        Ok(())
    }

    pub async fn list_presence(
        &self,
        token: &str,
        team_session: &str,
    ) -> Result<Vec<PresenceRecord>> {
        let resp = self
            .client
            .get(format!("{}/api/whiteboard/presence", self.base_url))
            .query(&[("team_session", team_session), ("token", token)])
            .send()
            .await
            .context("whiteboard presence list: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard presence list failed: HTTP {}", resp.status());
        }
        resp.json()
            .await
            .context("whiteboard presence list: bad response body")
    }

    pub async fn send_handoff(
        &self,
        token: &str,
        team_session: &str,
        sender: &str,
        recipient: &ta_session::RoleRef,
        payload: &str,
    ) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/handoff/send", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "sender": sender,
                "recipient": recipient,
                "payload": payload,
            }))
            .send()
            .await
            .context("whiteboard handoff send: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard handoff send failed: HTTP {}", resp.status());
        }
        Ok(())
    }

    pub async fn receive_handoff(
        &self,
        token: &str,
        team_session: &str,
        recipient: &ta_session::RoleRef,
    ) -> Result<Option<serde_json::Value>> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/handoff/receive", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "recipient": recipient,
            }))
            .send()
            .await
            .context("whiteboard handoff receive: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard handoff receive failed: HTTP {}", resp.status());
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .context("whiteboard handoff receive: bad response body")?;
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    pub async fn claim_task(
        &self,
        token: &str,
        team_session: &str,
        task_id: &str,
        agent_id: &str,
    ) -> Result<bool> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/tasks/claim", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "task_id": task_id,
                "agent_id": agent_id,
            }))
            .send()
            .await
            .context("whiteboard task claim: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard task claim failed: HTTP {}", resp.status());
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .context("whiteboard task claim: bad response body")?;
        Ok(value
            .get("claimed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }

    pub async fn complete_task(
        &self,
        token: &str,
        team_session: &str,
        task_id: &str,
    ) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/tasks/complete", self.base_url))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "task_id": task_id,
            }))
            .send()
            .await
            .context("whiteboard task complete: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard task complete failed: HTTP {}", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_daemon_url_defaults_to_7700_when_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_daemon_url(dir.path()), "http://127.0.0.1:7700");
    }

    #[test]
    fn read_pid_port_reads_the_written_port() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/daemon.pid"), "pid=123\nport=8899\n").unwrap();
        assert_eq!(read_pid_port(dir.path()), Some(8899));
    }
}
