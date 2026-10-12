//! `[whiteboard]` config in `.ta/workflow.toml` + transport selection —
//! mirrors `ta_submit`'s `[submit] adapter` / `select_adapter()` pattern
//! (this is the third instance of that convention: `SourceAdapter` for VCS,
//! `PlanStore` for plan storage, now `WhiteboardTransport` for
//! coordination).
//!
//! Deliberately self-contained: reads its own `[whiteboard]` table directly
//! out of `.ta/workflow.toml` rather than adding a field to
//! `ta_submit::WorkflowConfig` (which would require `ta-agent-whiteboard`
//! and `ta-submit` to depend on each other). `WorkflowConfig`'s own parser
//! has no `deny_unknown_fields`, so the two coexist safely in the same file.
//!
//! **Opt-in by design**: whiteboard participation is disabled unless a
//! project explicitly enables it. Item 10 (confirming NATS's deployment
//! shape on Render) is unverified as of this phase, and defaulting every
//! `ta run`/`ta_goal_start` call to require a live NATS connection would be
//! a breaking change for every existing project with no NATS server
//! running. Enabling is a deliberate choice, not an upgrade surprise.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Result, WhiteboardError};
use crate::memory_transport::InMemoryTransport;
use crate::nats_transport::NatsTransport;
use crate::transport::WhiteboardTransport;

/// `[whiteboard]` section of `.ta/workflow.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhiteboardConfig {
    /// Whether whiteboard coordination is active for this project at all.
    /// Default: `false` — see the module doc for why this stays opt-in.
    #[serde(default)]
    pub enabled: bool,

    /// Transport backend: `"nats"` (default once enabled) or `"memory"`
    /// (single-process only — real coordination across separately-launched
    /// goal processes needs `"nats"`; `"memory"` is for embedding
    /// `ta-agent-whiteboard` directly inside a single long-lived process,
    /// e.g. a future concurrent-stage `team_session.rs`).
    #[serde(default = "default_transport")]
    pub transport: String,

    /// NATS server URL, only used when `transport = "nats"`.
    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    /// Wake-on-demand listeners: how many times one message may launch its
    /// role (`ta run`) before it is dead-lettered (acked so it is never
    /// redelivered, and recorded in `.ta/wake-dead-letter.jsonl`).
    /// Default 3.
    #[serde(default = "default_wake_max_attempts")]
    pub wake_max_attempts: u32,

    /// Wake-on-demand listeners: seconds to wait before retrying a message
    /// whose launch failed, indexed by failure count (the last value
    /// repeats). Default `[30, 120, 600]` (30s, 2m, 10m).
    #[serde(default = "default_wake_backoff_secs")]
    pub wake_backoff_secs: Vec<u64>,

    /// Wake-on-demand listeners: cost safety net. At most this many
    /// launches per listener in any rolling hour; further messages wait
    /// (they are not dropped). Default 6. `0` disables the guard.
    #[serde(default = "default_wake_max_launches_per_hour")]
    pub wake_max_launches_per_hour: u32,

    /// Wake-on-demand listeners: the longest one launch (`ta run`) may take,
    /// in seconds, before the daemon kills it and counts the attempt as
    /// failed. Default 3600.
    #[serde(default = "default_wake_launch_timeout_secs")]
    pub wake_launch_timeout_secs: u64,

    /// Wake-on-demand listeners: how long the message transport waits for an
    /// ack before redelivering a message, in seconds. Must be at least
    /// `wake_launch_timeout_secs` plus 60. Unset (default) derives it as
    /// `wake_launch_timeout_secs + 60`.
    #[serde(default)]
    pub wake_ack_wait_secs: Option<u64>,

    /// Wake-on-demand listeners: how often, in seconds, a running launch
    /// tells the transport it is still working (JetStream `AckProgress`), so
    /// the message is not redelivered. Must be at most half of the ack wait.
    /// Unset (default) derives it as one third of the ack wait, at most 10.
    #[serde(default)]
    pub wake_ack_heartbeat_secs: Option<u64>,

    /// Outcome envelope verification (CR-04). When `true`, the daemon-side
    /// verifier refuses any outcome that is not a validly stamped and signed
    /// envelope. Unset (default) means "off in the default posture, on under
    /// the `strict` posture"; an explicit value always wins.
    #[serde(default)]
    pub require_stamped_outcomes: Option<bool>,

    /// Roles whose launches are granted the `whiteboard:outcome:send` right.
    /// Default `["chief-of-staff"]`. A role not listed cannot post outcomes.
    #[serde(default = "default_outcome_roles")]
    pub outcome_roles: Vec<String>,

    /// An outcome envelope older than this many seconds is rejected as stale
    /// by the verifier. Default 300.
    #[serde(default = "default_outcome_max_age_secs")]
    pub outcome_max_age_secs: u64,

    /// Name of the environment variable holding NATS credentials (a `.creds`
    /// file path, or `user:password`, or a bare token; see docs/USAGE.md).
    /// Default `TA_NATS_CREDENTIALS`. The vault entry name is
    /// `nats-credentials`.
    #[serde(default = "default_nats_credentials_env")]
    pub nats_credentials_env: String,

    /// Daemon wake listeners: seconds before the FIRST retry of a failed NATS
    /// connection (or after a dropped one). Each further failed attempt
    /// doubles the wait, with jitter, up to `connect_retry_max_secs`.
    /// Default 1. `0` is treated as 1.
    #[serde(default = "default_connect_retry_initial_secs")]
    pub connect_retry_initial_secs: u64,

    /// Daemon wake listeners: the longest wait between connection attempts,
    /// in seconds. This cap is what keeps a down NATS server from being
    /// hammered: after a few failures the daemon tries once per this many
    /// seconds, and keeps trying until the server is back. Default 60.
    /// Never below `connect_retry_initial_secs`.
    #[serde(default = "default_connect_retry_max_secs")]
    pub connect_retry_max_secs: u64,
}

impl Default for WhiteboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: default_transport(),
            nats_url: default_nats_url(),
            wake_max_attempts: default_wake_max_attempts(),
            wake_backoff_secs: default_wake_backoff_secs(),
            wake_max_launches_per_hour: default_wake_max_launches_per_hour(),
            wake_launch_timeout_secs: default_wake_launch_timeout_secs(),
            wake_ack_wait_secs: None,
            wake_ack_heartbeat_secs: None,
            require_stamped_outcomes: None,
            outcome_roles: default_outcome_roles(),
            outcome_max_age_secs: default_outcome_max_age_secs(),
            nats_credentials_env: default_nats_credentials_env(),
            connect_retry_initial_secs: default_connect_retry_initial_secs(),
            connect_retry_max_secs: default_connect_retry_max_secs(),
        }
    }
}

fn default_connect_retry_initial_secs() -> u64 {
    1
}

fn default_connect_retry_max_secs() -> u64 {
    60
}

fn default_outcome_roles() -> Vec<String> {
    vec!["chief-of-staff".to_string()]
}

fn default_outcome_max_age_secs() -> u64 {
    300
}

fn default_nats_credentials_env() -> String {
    "TA_NATS_CREDENTIALS".to_string()
}

fn default_wake_max_attempts() -> u32 {
    3
}

fn default_wake_backoff_secs() -> Vec<u64> {
    vec![30, 120, 600]
}

fn default_wake_max_launches_per_hour() -> u32 {
    6
}

fn default_wake_launch_timeout_secs() -> u64 {
    3600
}

fn default_transport() -> String {
    "nats".to_string()
}

fn default_nats_url() -> String {
    "localhost:4222".to_string()
}

#[derive(Debug, Default, Deserialize)]
struct WorkflowTomlWhiteboardSection {
    #[serde(default)]
    whiteboard: WhiteboardConfig,
}

impl WhiteboardConfig {
    /// Load the `[whiteboard]` section from `<project_root>/.ta/workflow.toml`.
    /// Missing file or missing section both resolve to `WhiteboardConfig::default()`
    /// (disabled) rather than an error.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".ta").join("workflow.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str::<WorkflowTomlWhiteboardSection>(&content)
            .map(|w| w.whiteboard)
            .unwrap_or_default()
    }
}

/// Resolve a `WhiteboardTransport` from config. Returns `None` when
/// coordination is disabled (`enabled = false`) — callers should treat this
/// as "whiteboard participation is off," not an error.
pub fn select_transport(config: &WhiteboardConfig) -> Result<Option<Arc<dyn WhiteboardTransport>>> {
    select_transport_inner(config, None)
}

/// Like [`select_transport`], but also consults the project's credential
/// vault (entry `nats-credentials`) for NATS credentials, and refuses at once
/// (rather than on the first message) an unauthenticated non-loopback NATS.
pub fn select_transport_for_project(
    config: &WhiteboardConfig,
    project_root: &std::path::Path,
) -> Result<Option<Arc<dyn WhiteboardTransport>>> {
    select_transport_inner(config, Some(project_root))
}

fn select_transport_inner(
    config: &WhiteboardConfig,
    project_root: Option<&std::path::Path>,
) -> Result<Option<Arc<dyn WhiteboardTransport>>> {
    if !config.enabled {
        return Ok(None);
    }
    let transport: Arc<dyn WhiteboardTransport> = match config.transport.as_str() {
        "nats" => {
            // Fail at startup, not on the first message.
            crate::nats_auth::resolve_for_url(
                &config.nats_url,
                &config.nats_credentials_env,
                project_root,
            )
            .map_err(|e| WhiteboardError::Config(e.to_string()))?;
            let mut t = NatsTransport::new(config.nats_url.clone())
                .with_credentials_env(config.nats_credentials_env.clone());
            if let Some(root) = project_root {
                t = t.with_project_root(root.to_path_buf());
            }
            Arc::new(t)
        }
        "memory" => Arc::new(InMemoryTransport::new()),
        other => return Err(WhiteboardError::UnknownTransport(other.to_string())),
    };
    Ok(Some(transport))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_retry_settings_default_to_1s_and_60s_and_parse() {
        let c = WhiteboardConfig::default();
        assert_eq!(
            (c.connect_retry_initial_secs, c.connect_retry_max_secs),
            (1, 60)
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nconnect_retry_initial_secs = 2\nconnect_retry_max_secs = 30\n",
        )
        .unwrap();
        let c = WhiteboardConfig::load(dir.path());
        assert_eq!(
            (c.connect_retry_initial_secs, c.connect_retry_max_secs),
            (2, 30)
        );
    }

    #[test]
    fn load_missing_file_returns_disabled_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = WhiteboardConfig::load(dir.path());
        assert!(!config.enabled);
    }

    #[test]
    fn load_parses_whiteboard_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\ntransport = \"memory\"\n",
        )
        .unwrap();
        let config = WhiteboardConfig::load(dir.path());
        assert!(config.enabled);
        assert_eq!(config.transport, "memory");
    }

    #[test]
    fn wake_retry_settings_default_and_parse() {
        let d = WhiteboardConfig::default();
        assert_eq!(d.wake_max_attempts, 3);
        assert_eq!(d.wake_backoff_secs, vec![30, 120, 600]);
        assert_eq!(d.wake_max_launches_per_hour, 6);
        assert_eq!(d.wake_launch_timeout_secs, 3600);
        assert_eq!(d.wake_ack_wait_secs, None);
        assert_eq!(d.wake_ack_heartbeat_secs, None);

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\nwake_max_attempts = 5\n\
             wake_backoff_secs = [1, 2]\nwake_max_launches_per_hour = 2\n\
             wake_launch_timeout_secs = 90\nwake_ack_wait_secs = 200\n\
             wake_ack_heartbeat_secs = 5\n",
        )
        .unwrap();
        let c = WhiteboardConfig::load(dir.path());
        assert_eq!(c.wake_launch_timeout_secs, 90);
        assert_eq!(c.wake_ack_wait_secs, Some(200));
        assert_eq!(c.wake_ack_heartbeat_secs, Some(5));
        assert_eq!(c.wake_max_attempts, 5);
        assert_eq!(c.wake_backoff_secs, vec![1, 2]);
        assert_eq!(c.wake_max_launches_per_hour, 2);
    }

    #[test]
    fn select_transport_returns_none_when_disabled() {
        let config = WhiteboardConfig::default();
        assert!(select_transport(&config).unwrap().is_none());
    }

    #[test]
    fn select_transport_resolves_memory_backend() {
        let config = WhiteboardConfig {
            enabled: true,
            transport: "memory".to_string(),
            ..Default::default()
        };
        let t = select_transport(&config).unwrap().unwrap();
        assert_eq!(t.backend_name(), "memory");
    }

    #[test]
    fn select_transport_refuses_unauthenticated_remote_nats() {
        let config = WhiteboardConfig {
            enabled: true,
            transport: "nats".into(),
            nats_url: "10.0.0.5:4222".into(),
            nats_credentials_env: "TA_TEST_NATS_CREDS_UNSET_XYZ".into(),
            ..WhiteboardConfig::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let err = select_transport_for_project(&config, dir.path())
            .err()
            .unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("10.0.0.5:4222") && msg.contains("nats-credentials"),
            "{msg}"
        );
    }

    #[test]
    fn select_transport_allows_loopback_nats_without_creds() {
        let config = WhiteboardConfig {
            enabled: true,
            transport: "nats".into(),
            nats_url: "nats://127.0.0.1:4222".into(),
            nats_credentials_env: "TA_TEST_NATS_CREDS_UNSET_XYZ".into(),
            ..WhiteboardConfig::default()
        };
        let dir = tempfile::tempdir().unwrap();
        assert!(select_transport_for_project(&config, dir.path())
            .unwrap()
            .is_some());
    }

    #[test]
    fn select_transport_rejects_unknown_backend() {
        let config = WhiteboardConfig {
            enabled: true,
            transport: "carrier-pigeon".to_string(),
            ..Default::default()
        };
        assert!(select_transport(&config).is_err());
    }
}
