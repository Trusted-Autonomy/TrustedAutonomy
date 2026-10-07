// serve.rs — Start the MCP server on stdio.
//
// This delegates to the same logic as ta-daemon, allowing users to
// start the server via `ta serve` without needing to know the binary name.

use std::path::Path;

use rmcp::ServiceExt;
use ta_mcp_gateway::chat_launch::{ChatLaunch, ENV_CHAT_MODE};
use ta_mcp_gateway::{GatewayConfig, TaGatewayServer};

pub fn execute(project_root: &Path) -> anyhow::Result<()> {
    // Honor TA_PROJECT_ROOT env var if set (used when launched as MCP server
    // subprocess via .mcp.json). Falls back to --project-root CLI arg.
    let effective_root = std::env::var("TA_PROJECT_ROOT")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_root.to_path_buf());

    // Chat-mode launch (persona `chat_mode = true` / `ta run --chat-mode`):
    // lock this server to one chat session with the compiled chat manifest
    // and only the chat-mode tool profile. Fails closed: a malformed
    // chat-mode environment is a startup error, never a fallback to an
    // unrestricted server.
    let chat_launch = ChatLaunch::from_env().map_err(|e| {
        anyhow::anyhow!(
            "ta serve: {} is set but the chat-mode launch settings are invalid: {}",
            ENV_CHAT_MODE,
            e
        )
    })?;
    let server = match chat_launch {
        Some(launch) => {
            let config = chat_mode_config(&launch, &effective_root);
            TaGatewayServer::new_chat_mode(config, &launch).map_err(|e| {
                anyhow::anyhow!(
                    "ta serve: could not start the chat-mode TA MCP server for agent '{}' \
                     (session {}): {}",
                    launch.agent_id,
                    launch.session_id,
                    e
                )
            })?
        }
        None => TaGatewayServer::new(GatewayConfig::for_project(&effective_root))?,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let transport = rmcp::transport::stdio();
        let server_handle = server
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
        let _ = server_handle.waiting().await;
        Ok::<(), anyhow::Error>(())
    })
}

/// Gateway config for a chat-mode server. Reads resolve against the goal's
/// staging copy (`TA_CHAT_WORKSPACE_ROOT`, falling back to the process cwd,
/// which the agent harness sets to the staging path), so the chat session's
/// own state lives in that disposable copy. Tool calls are still audited to
/// the real project's `.ta/audit.jsonl` when the project root is known.
fn chat_mode_config(launch: &ChatLaunch, project_root: &Path) -> GatewayConfig {
    let workspace_root = launch
        .workspace_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| project_root.to_path_buf());
    let mut config = GatewayConfig::for_project(&workspace_root);
    let project_audit = project_root.join(".ta").join("audit.jsonl");
    if project_root.join(".ta").is_dir() {
        config.audit_log = project_audit;
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_mode_config_uses_staging_root_and_project_audit_log() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let launch = ChatLaunch {
            agent_id: "chief-of-staff".to_string(),
            session_id: uuid::Uuid::new_v4(),
            workspace_root: Some(staging.path().to_path_buf()),
        };
        let config = chat_mode_config(&launch, project.path());
        assert_eq!(config.workspace_root, staging.path());
        assert_eq!(
            config.audit_log,
            project.path().join(".ta").join("audit.jsonl")
        );
        // And the resulting server is chat-locked with the narrow surface.
        let server = TaGatewayServer::new_chat_mode(config, &launch).unwrap();
        assert!(!server.tool_names().iter().any(|t| t == "ta_goal_start"));
    }
}
