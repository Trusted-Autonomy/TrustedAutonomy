// config.rs — `ta config` commands (v0.10.0).
//
// Inspect and validate the resolved channel configuration.

use clap::Subcommand;
use ta_changeset::channel_registry::{self, default_registry};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show resolved channel configuration — which channels are active,
    /// their types, capabilities, and status.
    Channels {
        /// Verify each configured channel is reachable/valid.
        #[arg(long)]
        check: bool,
    },
}

pub fn execute(command: &ConfigCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        ConfigCommands::Channels { check } => show_channels(config, *check),
    }
}

fn show_channels(config: &GatewayConfig, check: bool) -> anyhow::Result<()> {
    let ta_config = channel_registry::load_config(&config.workspace_root);
    let mut registry = default_registry();
    registry.register(Box::new(ta_channel_discord::DiscordChannelFactory));
    let routing = &ta_config.channels;

    let config_path = config.workspace_root.join(".ta").join("config.yaml");
    if config_path.exists() {
        println!("Config: {}", config_path.display());
    } else {
        println!("Config: (defaults — no .ta/config.yaml found)");
    }
    println!();

    // Review channels
    let review_configs = routing.review.configs();
    println!(
        "Review ({} channel{}):",
        review_configs.len(),
        if review_configs.len() != 1 { "s" } else { "" }
    );
    for rc in &review_configs {
        print_channel_entry(&registry, rc, check);
    }
    if routing.review.is_multi() {
        let strategy = routing.strategy.as_deref().unwrap_or("first_response");
        println!("  Strategy: {strategy}");
    }
    println!();

    // Notify channels
    println!(
        "Notify ({} channel{}):",
        routing.notify.len(),
        if routing.notify.len() != 1 { "s" } else { "" }
    );
    if routing.notify.is_empty() {
        println!("  (none configured)");
    }
    for nc in &routing.notify {
        let route = ta_changeset::ChannelRouteConfig {
            channel_type: nc.channel_type.clone(),
            config: nc.config.clone(),
        };
        print!("  ");
        print_channel_status(&registry, &route, check);
        println!("    Level filter: {}", nc.level);
    }
    println!();

    // Session channel
    println!("Session (1 channel):");
    print_channel_entry(&registry, &routing.session, check);
    println!();

    // Escalation channels
    match &routing.escalation {
        Some(esc) => {
            let esc_configs = esc.configs();
            println!(
                "Escalation ({} channel{}):",
                esc_configs.len(),
                if esc_configs.len() != 1 { "s" } else { "" }
            );
            for ec in &esc_configs {
                print_channel_entry(&registry, ec, check);
            }
        }
        None => {
            println!("Escalation: (none configured)");
        }
    }
    println!();

    // Defaults
    if let Some(agent) = &routing.default_agent {
        println!("Default agent: {agent}");
    }
    if let Some(wf) = &routing.default_workflow {
        println!("Default workflow: {wf}");
    }

    // Registered factories
    println!();
    let mut types = registry.channel_types();
    types.sort();
    println!("Registered channel types: {}", types.join(", "));

    Ok(())
}

fn print_channel_entry(
    registry: &ta_changeset::ChannelRegistry,
    route: &ta_changeset::ChannelRouteConfig,
    check: bool,
) {
    print!("  ");
    print_channel_status(registry, route, check);
}

fn print_channel_status(
    registry: &ta_changeset::ChannelRegistry,
    route: &ta_changeset::ChannelRouteConfig,
    check: bool,
) {
    let type_name = &route.channel_type;
    let registered = registry.has_channel(type_name);

    if !check {
        let status = if registered { "ok" } else { "unknown type" };
        println!("[{status}] type: {type_name}");
        if let Some(factory) = registry.get(type_name) {
            let caps = factory.capabilities();
            println!(
                "    Capabilities: review={}, session={}, notify={}, rich_media={}, threads={}",
                caps.supports_review,
                caps.supports_session,
                caps.supports_notify,
                caps.supports_rich_media,
                caps.supports_threads,
            );
        }
        return;
    }

    // Health check mode
    if !registered {
        println!("[FAIL] type: {type_name} — unknown channel type");
        return;
    }

    match registry.build_review_from_config(route) {
        Ok(channel) => {
            println!(
                "[PASS] type: {type_name} — channel_id: {}",
                channel.channel_id()
            );
            if let Some(factory) = registry.get(type_name) {
                let caps = factory.capabilities();
                println!(
                    "    Capabilities: review={}, session={}, notify={}, rich_media={}, threads={}",
                    caps.supports_review,
                    caps.supports_session,
                    caps.supports_notify,
                    caps.supports_rich_media,
                    caps.supports_threads,
                );
            }
        }
        Err(e) => {
            println!("[FAIL] type: {type_name} — {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_channels_with_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic with default config (no .ta/config.yaml)
        let result = show_channels(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn show_channels_with_check() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = show_channels(&config, true);
        assert!(result.is_ok());
    }

    #[test]
    fn show_channels_with_config_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("config.yaml"),
            r#"
channels:
  review:
    - type: auto-approve
    - type: auto-approve
  session:
    type: auto-approve
  strategy: first_response
"#,
        )
        .unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = show_channels(&config, true);
        assert!(result.is_ok());
    }
}
