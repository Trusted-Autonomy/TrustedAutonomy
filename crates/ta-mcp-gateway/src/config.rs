// config.rs — Gateway configuration.
//
// GatewayConfig determines where the gateway stores its state: staging
// workspaces, change stores, goal records, audit logs, and event logs.
// The `for_project()` constructor generates sensible defaults under a
// `.ta/` directory in the project root.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ta_changeset::review_channel::ReviewChannelConfig;

/// Configuration for the MCP gateway server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Root directory of the project (source of truth for file reads).
    pub workspace_root: PathBuf,

    /// Base directory for staging workspaces (one subdir per goal).
    pub staging_dir: PathBuf,

    /// Base directory for change stores (one subdir per goal).
    pub store_dir: PathBuf,

    /// Directory for GoalRunStore (one JSON file per goal).
    pub goals_dir: PathBuf,

    /// Path to the append-only audit log.
    pub audit_log: PathBuf,

    /// Path to the event notification log.
    pub events_log: PathBuf,

    /// Directory for PR package JSON files.
    pub pr_packages_dir: PathBuf,

    /// Directory for interactive session records (v0.3.1.2).
    pub interactive_sessions_dir: PathBuf,

    /// ReviewChannel configuration (v0.4.1.1).
    #[serde(default)]
    pub review_channel: ReviewChannelConfig,

    /// Optional port for the web review UI (v0.5.2).
    /// When set, the daemon serves a web UI at `http://127.0.0.1:{port}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_ui_port: Option<u16>,
}

impl GatewayConfig {
    /// Create a config with standard `.ta/` layout for a project.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let root = project_root.as_ref().to_path_buf();
        let ta_dir = root.join(".ta");
        Self {
            workspace_root: root,
            staging_dir: ta_dir.join("staging"),
            store_dir: ta_dir.join("store"),
            goals_dir: ta_dir.join("goals"),
            audit_log: ta_dir.join("audit.jsonl"),
            events_log: ta_dir.join("events.jsonl"),
            pr_packages_dir: ta_dir.join("pr_packages"),
            interactive_sessions_dir: ta_dir.join("interactive_sessions"),
            review_channel: ReviewChannelConfig::default(),
            web_ui_port: None,
        }
    }
}
