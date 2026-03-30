pub mod backend;
pub mod backends;
pub mod config;
pub mod error;

pub use backend::UnrealBackend;
pub use config::UnrealConnectorConfig;
pub use error::UnrealConnectorError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::make_backend;
    use crate::config::UnrealConnectorConfig;

    #[test]
    fn config_defaults_parse_correctly() {
        let cfg = UnrealConnectorConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.backend, "flopperam");
        assert_eq!(cfg.socket, "localhost:30100");
    }

    #[test]
    fn config_from_toml_kvick() {
        let toml = r#"
enabled = true
backend = "kvick"
socket = "localhost:30200"

[backends.kvick]
install_path = "/opt/ta-mcp/unreal-kvick"
"#;
        let cfg = UnrealConnectorConfig::from_toml(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.backend, "kvick");
        assert_eq!(cfg.backends.kvick.install_path, "/opt/ta-mcp/unreal-kvick");
    }

    #[test]
    fn config_from_toml_flopperam() {
        let toml = r#"
enabled = true
backend = "flopperam"
ue_project_path = "/path/to/Game.uproject"

[backends.flopperam]
install_path = "/opt/ta-mcp/unreal-flopperam"
"#;
        let cfg = UnrealConnectorConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.backend, "flopperam");
        assert_eq!(cfg.ue_project_path, "/path/to/Game.uproject");
        assert_eq!(
            cfg.backends.flopperam.install_path,
            "/opt/ta-mcp/unreal-flopperam"
        );
    }

    #[test]
    fn config_from_toml_special_agent() {
        let toml = r#"
enabled = true
backend = "special-agent"

[backends."special-agent"]
install_path = "/opt/ta-mcp/special-agent"
"#;
        let cfg = UnrealConnectorConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.backend, "special-agent");
        assert_eq!(
            cfg.backends.special_agent.install_path,
            "/opt/ta-mcp/special-agent"
        );
    }

    #[test]
    fn make_backend_unsupported_returns_error() {
        let cfg = UnrealConnectorConfig {
            backend: "nonexistent".to_string(),
            ..Default::default()
        };
        assert!(make_backend(&cfg).is_err());
    }

    #[test]
    fn kvick_backend_tools() {
        use crate::backend::UnrealTool;
        use crate::backends::KvickBackend;
        let cfg = UnrealConnectorConfig::default();
        let b = KvickBackend::new(&cfg);
        let tools = b.supported_tools();
        assert!(tools.contains(&UnrealTool::PythonExec));
        assert!(tools.contains(&UnrealTool::SceneQuery));
        assert!(!tools.contains(&UnrealTool::MrqSubmit));
    }

    #[test]
    fn flopperam_backend_tools_include_mrq() {
        use crate::backend::UnrealTool;
        use crate::backends::FlopperamBackend;
        let cfg = UnrealConnectorConfig::default();
        let b = FlopperamBackend::new(&cfg);
        let tools = b.supported_tools();
        assert!(tools.contains(&UnrealTool::MrqSubmit));
        assert!(tools.contains(&UnrealTool::MrqStatus));
    }

    #[test]
    fn flopperam_spawn_fails_without_install_path() {
        use crate::backends::FlopperamBackend;
        let cfg = UnrealConnectorConfig::default(); // empty install_path
        let b = FlopperamBackend::new(&cfg);
        assert!(b.spawn().is_err());
    }

    #[test]
    fn kvick_spawn_fails_without_install_path() {
        use crate::backends::KvickBackend;
        let cfg = UnrealConnectorConfig::default();
        let b = KvickBackend::new(&cfg);
        assert!(b.spawn().is_err());
    }

    #[test]
    fn special_agent_spawn_fails_without_install_path() {
        use crate::backends::SpecialAgentBackend;
        let cfg = UnrealConnectorConfig::default();
        let b = SpecialAgentBackend::new(&cfg);
        assert!(b.spawn().is_err());
    }

    #[test]
    fn tool_mcp_names_are_correct() {
        use crate::backend::UnrealTool;
        assert_eq!(UnrealTool::PythonExec.mcp_name(), "ue5_python_exec");
        assert_eq!(UnrealTool::SceneQuery.mcp_name(), "ue5_scene_query");
        assert_eq!(UnrealTool::AssetList.mcp_name(), "ue5_asset_list");
        assert_eq!(UnrealTool::MrqSubmit.mcp_name(), "ue5_mrq_submit");
        assert_eq!(UnrealTool::MrqStatus.mcp_name(), "ue5_mrq_status");
    }

    #[test]
    fn install_path_for_active_kvick_backend() {
        use crate::config::{BackendConfig, BackendsConfig};
        let cfg = UnrealConnectorConfig {
            backend: "kvick".to_string(),
            backends: BackendsConfig {
                kvick: BackendConfig {
                    install_path: "/opt/kvick".to_string(),
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let path = cfg.install_path_for_active_backend();
        assert_eq!(path.to_str().unwrap(), "/opt/kvick");
    }

    #[test]
    fn connector_list_output_format() {
        // Verifies that make_backend returns backends with correct names.
        let cfg = UnrealConnectorConfig {
            backend: "kvick".to_string(),
            ..Default::default()
        };
        let backend = make_backend(&cfg).unwrap();
        assert_eq!(backend.name(), "kvick");
        assert!(backend.metadata().contains_key("source"));
    }
}
