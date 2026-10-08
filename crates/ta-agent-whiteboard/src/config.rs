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
        }
    }
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
    if !config.enabled {
        return Ok(None);
    }
    let transport: Arc<dyn WhiteboardTransport> = match config.transport.as_str() {
        "nats" => Arc::new(NatsTransport::new(config.nats_url.clone())),
        "memory" => Arc::new(InMemoryTransport::new()),
        other => return Err(WhiteboardError::UnknownTransport(other.to_string())),
    };
    Ok(Some(transport))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\nwake_max_attempts = 5\n\
             wake_backoff_secs = [1, 2]\nwake_max_launches_per_hour = 2\n",
        )
        .unwrap();
        let c = WhiteboardConfig::load(dir.path());
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
    fn select_transport_rejects_unknown_backend() {
        let config = WhiteboardConfig {
            enabled: true,
            transport: "carrier-pigeon".to_string(),
            ..Default::default()
        };
        assert!(select_transport(&config).is_err());
    }
}
