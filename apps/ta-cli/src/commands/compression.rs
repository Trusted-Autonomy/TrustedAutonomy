// compression.rs — `ta compression` commands (v0.17.0.7).
//
// Subcommands:
//   ta compression status   — show proxy URL, headroom version, performance stats
//   ta compression enable   — set [compression].enabled = true in daemon.toml
//   ta compression disable  — set [compression].enabled = false in daemon.toml

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum CompressionCommands {
    /// Show context compression status: proxy URL, version, and performance stats.
    ///
    /// Displays whether headroom is running, the proxy URL agents use,
    /// the detected headroom binary version, and token-savings metrics for
    /// the current session (via `headroom perf`).
    ///
    /// Example:
    ///   ta compression status
    Status,

    /// Enable context compression (sets [compression].enabled = true in daemon.toml).
    ///
    /// If the daemon is running, sends a restart signal to the headroom supervisor.
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
}

pub fn execute(command: &CompressionCommands, config: &GatewayConfig) -> Result<()> {
    match command {
        CompressionCommands::Status => show_status(&config.workspace_root),
        CompressionCommands::Enable => set_enabled(&config.workspace_root, true),
        CompressionCommands::Disable => set_enabled(&config.workspace_root, false),
    }
}

// ─── status ──────────────────────────────────────────────────────────────────

fn show_status(workspace_root: &Path) -> Result<()> {
    let cfg = load_config(workspace_root);

    println!("Context Compression (headroom)");
    println!(
        "  Enabled:        {}",
        if cfg.enabled { "yes" } else { "no" }
    );
    println!("  Proxy URL:      http://127.0.0.1:{}", cfg.port);
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
    if let Some(status) = read_headroom_status(workspace_root) {
        let pid_str = status
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string());
        println!("  Process:        {} (PID {})", status.status, pid_str);
        println!("  Restarts:       {}", status.restart_count);
        if status.status == "suspended" {
            println!(
                "\n  ⚠ Headroom supervisor is suspended after repeated failures.\n  \
                 To resume:  ta compression enable"
            );
        }
    } else if cfg.enabled {
        println!("  Process:        not started (daemon not running or proxy not yet spawned)");
    } else {
        println!("  Process:        disabled");
    }

    // Binary detection.
    match find_headroom_binary() {
        Some(path) => {
            println!("  Binary:         {}", path.display());
            // Query headroom version.
            if let Ok(out) = std::process::Command::new(&path).arg("--version").output() {
                let ver = String::from_utf8_lossy(&out.stdout);
                let ver = ver.trim();
                if !ver.is_empty() {
                    println!("  Version:        {}", ver);
                }
            }
        }
        None => {
            println!("  Binary:         not found");
            if cfg.enabled {
                println!();
                println!("  Install headroom to activate compression:");
                println!("    pip install headroom-ai[all]");
                println!();
                println!("  Or disable compression:");
                println!("    ta compression disable");
            }
        }
    }

    // Performance stats (only when the proxy is running).
    let is_running = read_headroom_status(workspace_root)
        .map(|s| s.status == "running")
        .unwrap_or(false);

    if is_running {
        if let Some(binary) = find_headroom_binary() {
            match std::process::Command::new(&binary).arg("perf").output() {
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
        // If the supervisor is suspended, clear it with a restart signal.
        if let Err(e) = write_restart_signal(workspace_root) {
            tracing::debug!(
                error = %e,
                "Could not write headroom restart signal (daemon may not be running)"
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
struct CompressionConfig {
    enabled: bool,
    port: u16,
    cache_aligner: bool,
    headroom_learn: bool,
    per_agent: std::collections::HashMap<String, bool>,
}

impl CompressionConfig {
    fn with_defaults(mut self) -> Self {
        if self.port == 0 {
            self.port = 8787;
        }
        self
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
    }
}

#[derive(Debug, serde::Deserialize)]
struct HeadroomStatusFile {
    pub status: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub restart_count: u32,
}

fn read_headroom_status(workspace_root: &Path) -> Option<HeadroomStatusFile> {
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

fn find_headroom_binary() -> Option<std::path::PathBuf> {
    // 1. PATH
    if let Ok(p) = which::which("headroom") {
        return Some(p);
    }
    // 2. ~/.local/bin  and  3. ~/.venv/bin
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let local = home.join(".local").join("bin").join("headroom");
        if local.exists() {
            return Some(local);
        }
        let venv = home.join(".venv").join("bin").join("headroom");
        if venv.exists() {
            return Some(venv);
        }
    }
    None
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
}
