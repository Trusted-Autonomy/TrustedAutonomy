//! The MCP gateway's daemon HTTP client — first introduced for the
//! daemon-hosted whiteboard (see `docs/superpowers/specs/
//! 2026-09-11-daemon-hosted-whiteboard-design.md`). Before this, no
//! `ta-mcp-gateway` code called the daemon's HTTP API at all; every tool
//! handler (including `whiteboard_check.rs`'s pre-launch conflict check)
//! operated on local filesystem/library state directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ta_agent_whiteboard::presence::PresenceRecord;

/// Port recorded for this project's daemon, via the shared pid-file reader
/// (`crate::daemon_pid`), which understands every pid-file format any TA
/// binary has written.
pub fn read_pid_port(project_root: &Path) -> Option<u16> {
    crate::daemon_pid::read_pid_file(project_root)
        .ok()
        .flatten()
        .and_then(|p| p.port)
}

/// Resolve this project's daemon base URL, failing closed: see
/// `crate::daemon_pid::resolve_daemon_endpoint`. Never silently falls back
/// to 7700 when the project has daemon configuration that cannot be read.
pub fn resolve_daemon_url(project_root: &Path) -> Result<String, String> {
    crate::daemon_pid::resolve_daemon_endpoint(project_root).map(|e| e.base_url())
}

/// Canonical form used to compare project roots (macOS tempdirs live under
/// `/var`, which is a symlink to `/private/var`).
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub struct WhiteboardDaemonClient {
    /// Resolved base URL, or the actionable reason it could not be resolved.
    endpoint: Result<String, String>,
    /// The project this client must talk to. Every call first confirms the
    /// daemon it reached serves THIS project before sending anything, so a
    /// mis-resolved port can never hand a session token to another
    /// project's daemon.
    project_root: PathBuf,
    verified: tokio::sync::OnceCell<String>,
    client: reqwest::Client,
}

impl WhiteboardDaemonClient {
    pub fn new(project_root: &Path) -> Self {
        Self {
            endpoint: resolve_daemon_url(project_root),
            project_root: project_root.to_path_buf(),
            verified: tokio::sync::OnceCell::new(),
            // 2s matches this repo's convention for routine daemon HTTP
            // calls (see `apps/ta-cli/src/commands/daemon.rs`'s health/
            // status checks) rather than the longer timeouts reserved for
            // startup/drain waits.
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// The verified base URL: resolved (fail closed) and confirmed, via
    /// `GET /api/whiteboard/identity`, to be the daemon for this client's
    /// project. Cached after the first success.
    async fn base(&self) -> Result<String> {
        let url = match &self.endpoint {
            Ok(u) => u.clone(),
            Err(reason) => {
                anyhow::bail!("whiteboard: cannot locate this project's daemon: {reason}")
            }
        };
        let verified = self
            .verified
            .get_or_try_init(|| async {
                let resp = self
                    .client
                    .get(format!("{url}/api/whiteboard/identity"))
                    .send()
                    .await
                    .with_context(|| {
                        format!(
                            "whiteboard: could not reach the daemon at {url} for project {}. \
                             Is it running? Check with `ta daemon status` in that project.",
                            self.project_root.display()
                        )
                    })?;
                if !resp.status().is_success() {
                    anyhow::bail!(
                        "whiteboard: refusing to send anything to {url}: it did not confirm \
                         which project it serves (GET /api/whiteboard/identity returned HTTP {}). \
                         It may be a different project's daemon or an older daemon. Run \
                         `ta daemon restart` in {}.",
                        resp.status(),
                        self.project_root.display()
                    );
                }
                let body: serde_json::Value = resp
                    .json()
                    .await
                    .context("whiteboard: daemon identity response was not JSON")?;
                let served = body
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "whiteboard: daemon at {url} did not report a project_root, \
                             refusing to send a session token to it"
                        )
                    })?;
                if canonical(&served) != canonical(&self.project_root) {
                    tracing::error!(
                        daemon_url = %url,
                        daemon_project = %served.display(),
                        expected_project = %self.project_root.display(),
                        "whiteboard: daemon belongs to a different project, refusing to send token"
                    );
                    anyhow::bail!(
                        "whiteboard: refusing to send a session token to {url}: that daemon \
                         serves {} but this session belongs to {}. The pid file or daemon.toml \
                         in {} points at the wrong port. Run `ta daemon restart` in that project.",
                        served.display(),
                        self.project_root.display(),
                        self.project_root.display()
                    );
                }
                Ok::<String, anyhow::Error>(url.clone())
            })
            .await?;
        Ok(verified.clone())
    }

    pub async fn register_presence(
        &self,
        token: &str,
        team_session: &str,
        record: &PresenceRecord,
    ) -> Result<()> {
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/presence", base))
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
        let base = self.base().await?;
        let resp = self
            .client
            .get(format!("{}/api/whiteboard/presence", base))
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

    /// Advisory-only pre-launch conflict query — no token/team-session
    /// required, since this runs at `ta_goal_start` time, potentially
    /// before any team-session exists to mint a scope against. Hits the
    /// daemon's local-bypass-gated `presence_for_source` endpoint.
    pub async fn list_presence_for_source(&self, source_dir: &str) -> Result<Vec<PresenceRecord>> {
        let base = self.base().await?;
        let resp = self
            .client
            .get(format!("{}/api/whiteboard/presence_for_source", base))
            .query(&[("source_dir", source_dir)])
            .send()
            .await
            .context("whiteboard presence_for_source: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "whiteboard presence_for_source failed: HTTP {}",
                resp.status()
            );
        }
        resp.json()
            .await
            .context("whiteboard presence_for_source: bad response body")
    }

    pub async fn send_handoff(
        &self,
        token: &str,
        team_session: &str,
        sender: &str,
        recipient: &ta_session::RoleRef,
        payload: &str,
    ) -> Result<()> {
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/handoff/send", base))
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
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/handoff/receive", base))
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
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/tasks/claim", base))
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
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/tasks/complete", base))
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

    /// Publishes `payload` (an opaque, already-JSON-encoded outcome message)
    /// onto the report-back stream (v0.17.11.11) — the Wayfinder poller
    /// drains it and turns it into a `PATCH`/`POST` back to Wayfinder's
    /// task API. No RoleRef addressing, unlike `send_handoff`: there is
    /// exactly one intended reader.
    pub async fn send_outcome(&self, token: &str, team_session: &str, payload: &str) -> Result<()> {
        let base = self.base().await?;
        let resp = self
            .client
            .post(format!("{}/api/whiteboard/outcome/send", base))
            .json(&serde_json::json!({
                "token": token,
                "team_session": team_session,
                "payload": payload,
            }))
            .send()
            .await
            .context("whiteboard outcome send: request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whiteboard outcome send failed: HTTP {}", resp.status());
        }
        Ok(())
    }
}

/// A minimal fake daemon for tests: answers `GET /api/whiteboard/identity`
/// with a chosen project root and records every outcome POST it receives.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    pub struct MockDaemon {
        pub port: u16,
        pub outcomes: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    /// Starts the fake daemon on a random port on the CURRENT tokio
    /// runtime (use a multi-thread runtime if the caller then blocks).
    pub async fn spawn_mock_daemon(reports_project_root: &Path) -> MockDaemon {
        use axum::routing::{get, post};
        let outcomes: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let root = reports_project_root.display().to_string();
        let rec = outcomes.clone();
        let app = axum::Router::new()
            .route(
                "/api/whiteboard/identity",
                get(move || {
                    let root = root.clone();
                    async move { axum::Json(serde_json::json!({ "project_root": root })) }
                }),
            )
            .route(
                "/api/whiteboard/outcome/send",
                post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let rec = rec.clone();
                    async move {
                        rec.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({ "ok": true }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        MockDaemon { port, outcomes }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::spawn_mock_daemon;
    use super::*;

    #[test]
    fn resolve_daemon_url_defaults_to_7700_only_when_project_has_no_daemon_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_daemon_url(dir.path()).unwrap(),
            "http://127.0.0.1:7700"
        );
    }

    #[test]
    fn read_pid_port_reads_the_legacy_port_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/daemon.pid"), "pid=123\nport=8899\n").unwrap();
        assert_eq!(read_pid_port(dir.path()), Some(8899));
    }

    #[test]
    fn read_pid_port_reads_the_daemons_bind_line() {
        // Regression: the daemon writes `pid=` + `bind=host:port` only.
        // The old reader looked for `port=` alone and returned None, which
        // made every caller fall back to 7700.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/daemon.pid"),
            "pid=31751\nbind=127.0.0.1:7710\n",
        )
        .unwrap();
        assert_eq!(read_pid_port(dir.path()), Some(7710));
        assert_eq!(
            resolve_daemon_url(dir.path()).unwrap(),
            "http://127.0.0.1:7710"
        );
    }

    #[tokio::test]
    async fn missing_port_in_pid_file_errors_instead_of_using_7700() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/daemon.pid"), "pid=1\n").unwrap();
        let client = WhiteboardDaemonClient::new(dir.path());
        let err = client
            .send_outcome("tok", "sess", "{}")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("daemon.pid"), "{err}");
        assert!(err.contains("Refusing to guess"), "{err}");
    }

    #[tokio::test]
    async fn outcome_reaches_the_daemon_named_in_the_pid_file() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        let daemon = spawn_mock_daemon(project.path()).await;
        std::fs::write(
            project.path().join(".ta/daemon.pid"),
            format!("pid=1\nbind=127.0.0.1:{}\n", daemon.port),
        )
        .unwrap();

        WhiteboardDaemonClient::new(project.path())
            .send_outcome("tok-1", "sess-1", "{\"outcome\":\"done\"}")
            .await
            .unwrap();
        let got = daemon.outcomes.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["token"], "tok-1");
    }

    #[tokio::test]
    async fn client_refuses_to_send_token_to_another_projects_daemon() {
        let ours = tempfile::tempdir().unwrap();
        let theirs = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ours.path().join(".ta")).unwrap();
        // A daemon serving a DIFFERENT project, which our pid file
        // (wrongly) points at.
        let daemon = spawn_mock_daemon(theirs.path()).await;
        std::fs::write(
            ours.path().join(".ta/daemon.pid"),
            format!("pid=1\nport={}\n", daemon.port),
        )
        .unwrap();

        let err = WhiteboardDaemonClient::new(ours.path())
            .send_outcome("secret-token", "sess-1", "{}")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to send a session token"), "{err}");
        assert!(
            daemon.outcomes.lock().unwrap().is_empty(),
            "the token must never reach the other project's daemon"
        );
    }
}
