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

    /// Whether this gateway is serving a staging workspace (v0.9.4.1).
    ///
    /// When true, `ta_goal_start` is blocked to prevent re-entrant goal
    /// creation from inside an implementation agent's workspace.
    /// Set via the `TA_IS_STAGING` environment variable.
    #[serde(default)]
    pub is_staging: bool,

    /// Whether the credential vault (opened by the v0.17.6.3 connector
    /// broker to resolve broker-mediated secrets) should try the OS
    /// keychain for its encryption key before falling back to a
    /// chmod-0600 file. Defaults to `true` for real deployments; test
    /// fixtures set this `false` for the same reason
    /// `ta_credentials::CredentialsConfig::use_keychain` documents — the
    /// keychain is a process/OS-global resource that must not leak between
    /// tests or interfere with the developer's real keychain.
    /// `for_project()` resolves this through
    /// `ta_credentials::custody::use_keychain` (`TA_NO_KEYCHAIN`, then the
    /// per-user custody file, then the default), same as
    /// `CredentialsConfig::for_project` — choose file custody to avoid an indefinite
    /// hang on a headless machine where nothing can answer the OS
    /// keychain's permission prompt.
    #[serde(default = "default_credential_vault_use_keychain")]
    pub credential_vault_use_keychain: bool,

    /// The REAL project root, when it differs from `workspace_root`. Set by
    /// the chat-mode launch (`TA_CHAT_PROJECT_ROOT`): a Chief-of-Staff
    /// session's `workspace_root` is a private session directory that holds
    /// the gateway's own state, while the project it reads lives here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
}

fn default_credential_vault_use_keychain() -> bool {
    true
}

/// Split a staging workspace (`<project>/.ta/staging/<goal-uuid>`) into the
/// project root it was copied from.
pub fn source_root_of_staging(workspace_root: &Path) -> Option<PathBuf> {
    crate::tools::task_request::parse_staging_workspace(workspace_root).map(|(root, _)| root)
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
            is_staging: std::env::var("TA_IS_STAGING").is_ok(),
            credential_vault_use_keychain: ta_credentials::custody::use_keychain(),
            project_root: None,
        }
    }

    /// The SOURCE project root: where `.ta/workflow.toml`,
    /// `.ta/wiki-resources.toml`, the plan file and the credential vault of
    /// the project live.
    ///
    /// A gateway serving a goal is rooted in a staging copy
    /// (`<project>/.ta/staging/<goal>`) that carries none of the project's
    /// `.ta/` configuration (and must not: staging is agent-writable). Every
    /// read tool that loads project configuration MUST resolve it through
    /// this method, never from `workspace_root` directly. Order: the
    /// explicit `project_root`, else the project a staging-shaped
    /// `workspace_root` was copied from, else `workspace_root` itself.
    pub fn source_root(&self) -> PathBuf {
        if let Some(root) = &self.project_root {
            return root.clone();
        }
        source_root_of_staging(&self.workspace_root).unwrap_or_else(|| self.workspace_root.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `for_project` routes through the single custody resolver. Inside a test
    /// binary the resolver always answers "file" (the test guard), whether or
    /// not `TA_NO_KEYCHAIN` is set, so a gateway test can never open the real
    /// keychain. The precedence table itself is tested in `ta-credentials`.
    #[test]
    fn for_project_uses_the_custody_resolver_and_is_file_under_test() {
        let config = GatewayConfig::for_project("/tmp/does-not-need-to-exist");
        assert_eq!(
            config.credential_vault_use_keychain,
            ta_credentials::custody::use_keychain()
        );
        assert!(!config.credential_vault_use_keychain);
    }

    #[test]
    fn source_root_prefers_explicit_then_staging_parent_then_workspace() {
        let id = uuid::Uuid::new_v4();
        let staging = std::path::PathBuf::from(format!("/work/proj/.ta/staging/{id}"));
        let mut c = GatewayConfig::for_project(&staging);
        assert_eq!(c.source_root(), std::path::PathBuf::from("/work/proj"));
        c.project_root = Some("/elsewhere/real".into());
        assert_eq!(c.source_root(), std::path::PathBuf::from("/elsewhere/real"));
        // Not staging-shaped (a normal project, or a chat session dir):
        // the workspace itself.
        let plain = GatewayConfig::for_project("/work/proj");
        assert_eq!(plain.source_root(), std::path::PathBuf::from("/work/proj"));
        let not_uuid = GatewayConfig::for_project("/work/proj/.ta/staging/not-a-goal");
        assert_eq!(
            not_uuid.source_root(),
            std::path::PathBuf::from("/work/proj/.ta/staging/not-a-goal")
        );
    }
}
