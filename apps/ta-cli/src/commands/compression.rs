// compression.rs — `ta compression` commands (v0.17.0.10).
//
// Subcommands:
//   ta compression status        — show plugin name, proxy URL, process status
//   ta compression enable        — set [compression].enabled = true in daemon.toml
//   ta compression disable       — set [compression].enabled = false in daemon.toml
//   ta compression plugin show   — print the active plugin configuration

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum CompressionCommands {
    /// Show context compression status: plugin name, proxy URL, and performance stats.
    ///
    /// Displays whether the optimizer is running, the proxy URL agents use,
    /// the configured plugin command, and token-savings metrics for
    /// the current session.
    ///
    /// Example:
    ///   ta compression status
    Status,

    /// Enable context compression (sets [compression].enabled = true in daemon.toml).
    ///
    /// If the daemon is running, sends a restart signal to the optimizer supervisor.
    /// Otherwise, restart the daemon to apply: `ta daemon restart`.
    ///
    /// Example:
    ///   ta compression enable
    Enable,

    /// Disable context compression (sets [compression].enabled = false in daemon.toml).
    ///
    /// The daemon must be restarted to fully stop the proxy: `ta daemon restart`.
    ///
    /// Example:
    ///   ta compression disable
    Disable,

    /// Manage the active prompt-optimizer plugin.
    ///
    /// Example:
    ///   ta compression plugin show
    Plugin {
        #[command(subcommand)]
        command: PluginCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginCommands {
    /// Print the active plugin configuration (name, command, args, proxy URL, health endpoint).
    ///
    /// When no `[compression.plugin]` block is set in daemon.toml, shows the
    /// headroom built-in defaults derived from `[compression].port`.
    ///
    /// Example:
    ///   ta compression plugin show
    Show,
}

pub fn execute(command: &CompressionCommands, config: &GatewayConfig) -> Result<()> {
    match command {
        CompressionCommands::Status => show_status(&config.workspace_root),
        CompressionCommands::Enable => set_enabled(&config.workspace_root, true),
        CompressionCommands::Disable => set_enabled(&config.workspace_root, false),
        CompressionCommands::Plugin { command } => match command {
            PluginCommands::Show => show_plugin(&config.workspace_root),
        },
    }
}

// ─── status ──────────────────────────────────────────────────────────────────

fn show_status(workspace_root: &Path) -> Result<()> {
    let cfg = load_config(workspace_root);
    let plugin = cfg.effective_plugin();

    println!("Context Compression");
    println!(
        "  Enabled:        {}",
        if cfg.enabled { "yes" } else { "no" }
    );
    println!("  Plugin:         {}", plugin.name);
    println!("  Proxy URL:      {}", plugin.proxy_base_url);
    println!(
        "  Cache aligner:  {}",
        if cfg.cache_aligner { "active" } else { "off" }
    );
    println!(
        "  headroom_learn: {} (always disabled in TA-managed runs)",
        if cfg.headroom_learn {
            "true (override → false)"
        } else {
            "false"
        }
    );

    // Supervisor process status.
    if let Some(status) = read_optimizer_status(workspace_root) {
        let pid_str = status
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string());
        let display_name = if status.plugin_name.is_empty() {
            plugin.name.clone()
        } else {
            status.plugin_name.clone()
        };
        println!("  Process:        {} (PID {})", status.status, pid_str);
        println!("  Restarts:       {}", status.restart_count);
        if status.status == "suspended" {
            println!(
                "\n  ⚠ {} supervisor is suspended after repeated failures.\n  \
                 To resume:  ta compression enable",
                display_name
            );
        }
    } else if cfg.enabled {
        println!("  Process:        not started (daemon not running or proxy not yet spawned)");
    } else {
        println!("  Process:        disabled");
    }

    // Binary detection using plugin command.
    match which::which(&plugin.command) {
        Ok(path) => {
            println!("  Binary:         {}", path.display());
            if let Ok(out) = std::process::Command::new(&path).arg("--version").output() {
                let ver = String::from_utf8_lossy(&out.stdout);
                let ver = ver.trim();
                if !ver.is_empty() {
                    println!("  Version:        {}", ver);
                }
            }
        }
        Err(_) => {
            println!("  Binary:         {} (not found on PATH)", plugin.command);
            if cfg.enabled {
                println!();
                println!("  Install the plugin binary to activate compression.");
                if plugin.name == "headroom" {
                    println!("    pip install headroom-ai[all]");
                    println!();
                    println!("  Or disable compression:");
                    println!("    ta compression disable");
                } else {
                    println!(
                        "  Set the correct command in [compression.plugin] in .ta/daemon.toml."
                    );
                    println!("  Or disable compression: ta compression disable");
                }
            }
        }
    }

    // Performance stats (only when the proxy is running).
    let is_running = read_optimizer_status(workspace_root)
        .map(|s| s.status == "running")
        .unwrap_or(false);

    if is_running {
        if let Ok(binary_path) = which::which(&plugin.command) {
            match std::process::Command::new(&binary_path)
                .arg("perf")
                .output()
            {
                Ok(out) if out.status.success() => {
                    let perf = String::from_utf8_lossy(&out.stdout);
                    let perf = perf.trim();
                    if !perf.is_empty() {
                        println!();
                        println!("  Performance (current session):");
                        for line in perf.lines() {
                            println!("    {}", line);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ─── plugin show ─────────────────────────────────────────────────────────────

fn show_plugin(workspace_root: &Path) -> Result<()> {
    let cfg = load_config(workspace_root);
    let plugin = cfg.effective_plugin();

    let source = if cfg.has_explicit_plugin() {
        "[compression.plugin] (explicit)"
    } else {
        "built-in headroom default"
    };

    println!("Active Prompt-Optimizer Plugin  ({})", source);
    println!("  Name:             {}", plugin.name);
    println!("  Command:          {}", plugin.command);
    println!("  Args:             {}", plugin.args.join(" "));
    println!("  Proxy base URL:   {}", plugin.proxy_base_url);
    println!("  Health endpoint:  {}", plugin.health_endpoint);
    if !plugin.env.is_empty() {
        println!("  Env:");
        let mut pairs: Vec<_> = plugin.env.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (k, v) in pairs {
            println!("    {}={}", k, v);
        }
    }

    if !cfg.has_explicit_plugin() {
        println!();
        println!("  To customise, add to .ta/daemon.toml:");
        println!("    [compression.plugin]");
        println!("    name            = \"{}\"", plugin.name);
        println!("    command         = \"{}\"", plugin.command);
        println!("    args            = {:?}", plugin.args);
        println!("    proxy_base_url  = \"{}\"", plugin.proxy_base_url);
        println!("    health_endpoint = \"{}\"", plugin.health_endpoint);
    }

    Ok(())
}

// ─── enable / disable ────────────────────────────────────────────────────────

fn set_enabled(workspace_root: &Path, enabled: bool) -> Result<()> {
    let daemon_toml_path = workspace_root.join(".ta").join("daemon.toml");

    let existing = if daemon_toml_path.exists() {
        std::fs::read_to_string(&daemon_toml_path)
            .with_context(|| format!("Cannot read {}", daemon_toml_path.display()))?
    } else {
        String::new()
    };

    let updated = update_compression_enabled(&existing, enabled);

    if let Some(parent) = daemon_toml_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create directory {}", parent.display()))?;
    }

    std::fs::write(&daemon_toml_path, updated)
        .with_context(|| format!("Cannot write {}", daemon_toml_path.display()))?;

    let action = if enabled { "enabled" } else { "disabled" };
    println!("Context compression {}.", action);

    if enabled {
        if let Err(e) = write_restart_signal(workspace_root) {
            tracing::debug!(
                error = %e,
                "Could not write optimizer restart signal (daemon may not be running)"
            );
        }
        println!(
            "  If the daemon is already running, it will pick up the new setting.\n  \
             Otherwise: ta daemon restart"
        );
    } else {
        println!("  Restart the daemon to stop the proxy: ta daemon restart");
    }

    Ok(())
}

/// Update `[compression]\nenabled = <value>` in a daemon.toml string.
///
/// If the `[compression]` section exists, replaces or adds the `enabled` line.
/// If the section does not exist, appends it.
fn update_compression_enabled(content: &str, enabled: bool) -> String {
    let enabled_str = if enabled { "true" } else { "false" };
    let new_line = format!("enabled = {}", enabled_str);

    // If the file is empty or has no [compression] section, append one.
    if !content.contains("[compression]") {
        let sep = if content.is_empty() || content.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return format!("{}{}\n[compression]\n{}\n", content, sep, new_line);
    }

    // Walk line by line to find [compression] and replace/insert enabled.
    let mut in_compression = false;
    let mut enabled_replaced = false;
    let mut lines: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();

        // Detect section headers.
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // If we were in [compression] and haven't yet replaced `enabled`,
            // insert it before the new section starts.
            if in_compression && !enabled_replaced {
                lines.push(new_line.clone());
                enabled_replaced = true;
            }
            in_compression = trimmed == "[compression]";
        }

        if in_compression && !enabled_replaced && trimmed.starts_with("enabled") {
            lines.push(new_line.clone());
            enabled_replaced = true;
            continue; // skip the old line
        }

        lines.push(raw_line.to_string());
    }

    // If [compression] was the last section and enabled wasn't replaced yet.
    if in_compression && !enabled_replaced {
        lines.push(new_line);
    }

    let mut result = lines.join("\n");
    // Preserve trailing newline.
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct PluginSection {
    name: String,
    command: String,
    args: Vec<String>,
    proxy_base_url: String,
    health_endpoint: String,
    env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct CompressionConfig {
    enabled: bool,
    port: u16,
    cache_aligner: bool,
    headroom_learn: bool,
    per_agent: std::collections::HashMap<String, bool>,
    plugin: Option<PluginSection>,
}

struct EffectivePlugin {
    name: String,
    command: String,
    args: Vec<String>,
    proxy_base_url: String,
    health_endpoint: String,
    env: std::collections::HashMap<String, String>,
}

impl CompressionConfig {
    fn with_defaults(mut self) -> Self {
        if self.port == 0 {
            self.port = 8787;
        }
        self
    }

    fn has_explicit_plugin(&self) -> bool {
        self.plugin.is_some()
    }

    fn effective_plugin(&self) -> EffectivePlugin {
        if let Some(ref p) = self.plugin {
            let port = self.port;
            let name = if p.name.is_empty() {
                "headroom".to_string()
            } else {
                p.name.clone()
            };
            let command = if p.command.is_empty() {
                "headroom".to_string()
            } else {
                p.command.clone()
            };
            let proxy_base_url = if p.proxy_base_url.is_empty() {
                format!("http://127.0.0.1:{}", port)
            } else {
                p.proxy_base_url.clone()
            };
            let health_endpoint = if p.health_endpoint.is_empty() {
                format!("http://127.0.0.1:{}/health", port)
            } else {
                p.health_endpoint.clone()
            };
            let args = if p.args.is_empty() {
                vec!["proxy".to_string(), "--port".to_string(), port.to_string()]
            } else {
                p.args.clone()
            };
            return EffectivePlugin {
                name,
                command,
                args,
                proxy_base_url,
                health_endpoint,
                env: p.env.clone(),
            };
        }
        let port = self.port;
        let mut env = std::collections::HashMap::new();
        env.insert("HEADROOM_LEARN".to_string(), "false".to_string());
        EffectivePlugin {
            name: "headroom".to_string(),
            command: "headroom".to_string(),
            args: vec!["proxy".to_string(), "--port".to_string(), port.to_string()],
            proxy_base_url: format!("http://127.0.0.1:{}", port),
            health_endpoint: format!("http://127.0.0.1:{}/health", port),
            env,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct DaemonTomlCompression {
    compression: CompressionConfig,
}

fn load_config(workspace_root: &Path) -> CompressionConfig {
    let path = workspace_root.join(".ta").join("daemon.toml");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(parsed) = toml::from_str::<DaemonTomlCompression>(&content) {
            return parsed.compression.with_defaults();
        }
    }
    CompressionConfig {
        enabled: true,
        port: 8787,
        cache_aligner: true,
        headroom_learn: false,
        per_agent: {
            let mut m = std::collections::HashMap::new();
            m.insert("claude-code".to_string(), true);
            m.insert("codex".to_string(), false);
            m
        },
        plugin: None,
    }
}

#[derive(Debug, serde::Deserialize)]
struct OptimizerStatusFile {
    pub status: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default)]
    pub plugin_name: String,
}

fn read_optimizer_status(workspace_root: &Path) -> Option<OptimizerStatusFile> {
    let path = workspace_root
        .join(".ta")
        .join("compression")
        .join("status.json");
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_restart_signal(workspace_root: &Path) -> std::io::Result<()> {
    let dir = workspace_root.join(".ta").join("compression");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("restart-signal"), "restart")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_adds_compression_section_when_absent() {
        let content = "[server]\nport = 7700\n";
        let result = update_compression_enabled(content, true);
        assert!(result.contains("[compression]"));
        assert!(result.contains("enabled = true"));
        assert!(result.contains("[server]"));
    }

    #[test]
    fn update_replaces_enabled_line_in_existing_section() {
        let content = "[compression]\nenabled = false\nport = 8787\n";
        let result = update_compression_enabled(content, true);
        assert!(result.contains("enabled = true"));
        assert!(!result.contains("enabled = false"));
        assert!(result.contains("port = 8787"));
    }

    #[test]
    fn update_inserts_enabled_when_section_has_no_enabled_line() {
        let content = "[compression]\nport = 8787\n";
        let result = update_compression_enabled(content, false);
        assert!(result.contains("enabled = false"));
        assert!(result.contains("port = 8787"));
    }

    #[test]
    fn update_preserves_other_sections() {
        let content =
            "[server]\nport = 7700\n\n[compression]\nenabled = true\n\n[gc]\nmax_staging_gb = 20\n";
        let result = update_compression_enabled(content, false);
        assert!(result.contains("enabled = false"));
        assert!(result.contains("[server]"));
        assert!(result.contains("[gc]"));
    }

    #[test]
    fn update_empty_file_appends_section() {
        let content = "";
        let result = update_compression_enabled(content, true);
        assert!(result.contains("[compression]"));
        assert!(result.contains("enabled = true"));
    }

    #[test]
    fn update_disables_compression() {
        let content = "[compression]\nenabled = true\n";
        let result = update_compression_enabled(content, false);
        assert!(result.contains("enabled = false"));
        assert!(!result.contains("enabled = true"));
    }

    #[test]
    fn effective_plugin_defaults_to_headroom() {
        let cfg = CompressionConfig {
            port: 8787,
            ..Default::default()
        };
        let plugin = cfg.effective_plugin();
        assert_eq!(plugin.name, "headroom");
        assert_eq!(plugin.command, "headroom");
        assert_eq!(plugin.proxy_base_url, "http://127.0.0.1:8787");
    }

    #[test]
    fn effective_plugin_uses_explicit_config() {
        let cfg = CompressionConfig {
            port: 8787,
            plugin: Some(PluginSection {
                name: "my-proxy".to_string(),
                command: "my-proxy-bin".to_string(),
                args: vec!["--listen".to_string(), "9090".to_string()],
                proxy_base_url: "http://127.0.0.1:9090".to_string(),
                health_endpoint: "http://127.0.0.1:9090/healthz".to_string(),
                env: std::collections::HashMap::new(),
            }),
            ..Default::default()
        };
        let plugin = cfg.effective_plugin();
        assert_eq!(plugin.name, "my-proxy");
        assert_eq!(plugin.proxy_base_url, "http://127.0.0.1:9090");
        assert_eq!(plugin.health_endpoint, "http://127.0.0.1:9090/healthz");
    }

    #[test]
    fn has_explicit_plugin_false_when_absent() {
        let cfg = CompressionConfig::default();
        assert!(!cfg.has_explicit_plugin());
    }

    #[test]
    fn has_explicit_plugin_true_when_set() {
        let cfg = CompressionConfig {
            plugin: Some(PluginSection::default()),
            ..Default::default()
        };
        assert!(cfg.has_explicit_plugin());
    }
}
