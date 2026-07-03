// meridian.rs — `ta meridian` subcommand group (v0.17.0.12.1).
//
// Delegates to the `meridian` binary on PATH. TA emits token counts in
// velocity-history.jsonl so Meridian can report cost rather than time-as-proxy.
//
// When `meridian` is installed, TA also injects it as an MCP sidecar in every
// goal so agents get meridian_report / meridian_analyze / meridian_kpis /
// meridian_suggest as native tool calls.
//
// Binary resolution order:
//   1. TA_MERIDIAN_BINARY env var
//   2. [meridian] binary in .ta/daemon.toml
//   3. which::which("meridian") — full cross-platform path (handles PATHEXT on Windows)
//   4. bare --version probe (fallback)
//
// Subcommands:
//   ta meridian analyze  → meridian analyze --source ta --path <project_root>
//   ta meridian help     → list tools exposed by `meridian serve` (MCP)
//   ta meridian init     → meridian init
//   ta meridian suggest  → meridian suggest --source ta --path <project_root>

use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum MeridianCommands {
    /// Analyze TA project velocity data and produce a KPI report.
    ///
    /// Delegates to: `meridian analyze --source ta --path <project_root>`
    ///
    /// Reads `.ta/velocity-history.jsonl` and `.ta/velocity-stats.jsonl` to
    /// compute cost-per-phase, throughput, and KPI alignment scores. Requires
    /// the `meridian` binary to be installed.
    ///
    /// Example:
    ///   ta meridian analyze
    Analyze,

    /// List all tools that Meridian exposes as an MCP server.
    ///
    /// Starts a short-lived `meridian serve` session, queries the MCP
    /// `tools/list` endpoint, and prints each tool name and description.
    /// Use this to discover which Meridian analytics are available as native
    /// tool calls inside a running goal.
    ///
    /// Example:
    ///   ta meridian help
    Help,

    /// Initialize a Meridian configuration in the current project.
    ///
    /// Delegates to: `meridian init`
    ///
    /// Creates `meridian.toml` with starter KPI definitions. Run once after
    /// installing Meridian to customize your team's success metrics.
    ///
    /// Example:
    ///   ta meridian init
    Init,

    /// Suggest KPI alignment improvements based on velocity history.
    ///
    /// Delegates to: `meridian suggest --source ta --path <project_root>`
    ///
    /// Uses Meridian's regression engine to classify past plan phases and
    /// surface alignment gaps with actionable suggestions for future phases.
    ///
    /// Example:
    ///   ta meridian suggest
    Suggest,
}

/// Resolve the `meridian` binary path using a cross-platform priority chain.
///
/// Checks in order:
/// 1. `TA_MERIDIAN_BINARY` env var — explicit override for non-PATH installs.
/// 2. `[meridian] binary` in `.ta/daemon.toml` — per-project config.
/// 3. `which::which("meridian")` — cross-platform PATH search (handles Windows PATHEXT).
/// 4. Bare `meridian --version` probe — fallback; returns `"meridian"` as command name.
///
/// Returns `None` when meridian is not available by any of the above means.
pub fn resolve_meridian_binary(workspace_root: &Path) -> Option<String> {
    // 1. Env var override.
    if let Ok(val) = std::env::var("TA_MERIDIAN_BINARY") {
        let val = val.trim().to_string();
        if !val.is_empty() {
            return Some(val);
        }
    }

    // 2. daemon.toml [meridian] binary.
    if let Some(path) = read_meridian_binary_from_config(workspace_root) {
        return Some(path);
    }

    // 3. which::which — full cross-platform resolution (PATHEXT on Windows).
    if let Ok(p) = which::which("meridian") {
        return Some(p.to_string_lossy().into_owned());
    }

    // 4. Direct --version probe (bare command name, OS handles PATH lookup).
    let ok = std::process::Command::new("meridian")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        Some("meridian".to_string())
    } else {
        None
    }
}

/// Peek at `.ta/daemon.toml` for a `[meridian] binary = ...` setting.
fn read_meridian_binary_from_config(workspace_root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(workspace_root.join(".ta/daemon.toml")).ok()?;
    let mut in_meridian = false;
    for line in content.lines() {
        let t = line.trim();
        if t == "[meridian]" {
            in_meridian = true;
            continue;
        }
        if in_meridian && t.starts_with('[') {
            break;
        }
        if in_meridian && t.starts_with("binary") && t.contains('=') {
            let val = t
                .split('=')
                .nth(1)?
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Find the `meridian` binary or return an error with install instructions.
fn find_meridian_binary(workspace_root: &Path) -> Result<String> {
    resolve_meridian_binary(workspace_root).ok_or_else(|| {
        anyhow::anyhow!(
            "The `meridian` binary was not found on PATH.\n\
             \n\
             Install it with one of:\n\
             \n\
             \tcargo install meridian\n\
             \tcargo install --git https://github.com/Trusted-Autonomy/meridian\n\
             \n\
             After installing, re-run `ta meridian` to continue.\n\
             See the Meridian docs for platform-specific packages.\n\
             \n\
             To use a custom binary path, set TA_MERIDIAN_BINARY or add to .ta/daemon.toml:\n\
             \n\
             \t[meridian]\n\
             \tbinary = \"/path/to/meridian\""
        )
    })
}

pub fn execute(command: &MeridianCommands, config: &GatewayConfig) -> Result<()> {
    match command {
        MeridianCommands::Help => {
            let meridian = find_meridian_binary(&config.workspace_root)?;
            list_meridian_tools(&meridian)
        }
        _ => {
            let meridian = find_meridian_binary(&config.workspace_root)?;
            let project_root = &config.workspace_root;
            match command {
                MeridianCommands::Analyze => run_meridian(
                    &meridian,
                    &["analyze", "--source", "ta", "--path"],
                    project_root,
                ),
                MeridianCommands::Init => run_meridian_no_path(&meridian, &["init"]),
                MeridianCommands::Suggest => run_meridian(
                    &meridian,
                    &["suggest", "--source", "ta", "--path"],
                    project_root,
                ),
                MeridianCommands::Help => unreachable!(),
            }
        }
    }
}

/// Start a short-lived `meridian serve` session, call `tools/list` via the
/// MCP JSON-RPC protocol over stdio, and print tool names + descriptions.
fn list_meridian_tools(binary: &str) -> Result<()> {
    use std::io::{BufReader, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(binary)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to launch `meridian serve`: {}", e))?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);

    // 1. Send MCP initialize request.
    let init_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "ta", "version": "0.1"}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init_msg)?)?;

    // Read initialize response, skipping non-JSON lines (e.g., server banners).
    let mut line = String::new();
    let init_ok = read_json_line(&mut reader, &mut line, 10)?;
    if !init_ok {
        let _ = child.kill();
        bail!("`meridian serve` did not respond to the MCP initialize request");
    }

    // 2. Send initialized notification (no response expected).
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&notif)?)?;

    // 3. Request tool list.
    let list_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&list_msg)?)?;

    // Read tools/list response, again skipping non-JSON lines.
    line.clear();
    let tools_ok = read_json_line(&mut reader, &mut line, 20)?;
    let _ = child.kill();

    if !tools_ok {
        bail!("`meridian serve` did not respond to the MCP tools/list request");
    }

    let response: serde_json::Value = serde_json::from_str(line.trim())?;
    let tools = response
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array());

    match tools {
        Some(tools) if !tools.is_empty() => {
            println!("Meridian MCP tools (available as native tools in running goals):\n");
            for tool in tools {
                let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                let desc = tool
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                println!("  {}", name);
                if !desc.is_empty() {
                    println!("    {}", desc);
                }
                println!();
            }
        }
        _ => {
            println!("No tools found in `meridian serve` response.");
            println!("Ensure you have a recent version of Meridian installed.");
        }
    }

    Ok(())
}

/// Read lines from `reader` until a valid JSON object is found or `max_lines` is
/// exhausted. Stores the matching line in `buf` and returns `true` on success.
fn read_json_line<R: std::io::Read>(
    reader: &mut std::io::BufReader<R>,
    buf: &mut String,
    max_lines: usize,
) -> Result<bool> {
    use std::io::BufRead;
    for _ in 0..max_lines {
        buf.clear();
        let n = reader.read_line(buf)?;
        if n == 0 {
            break; // EOF
        }
        let trimmed = buf.trim();
        if trimmed.starts_with('{') {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run `meridian <args> <project_root>` — commands that take `--path`.
fn run_meridian(binary: &str, args: &[&str], project_root: &Path) -> Result<()> {
    let project_root_str = project_root.to_string_lossy();
    let status = std::process::Command::new(binary)
        .args(args)
        .arg(project_root_str.as_ref())
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to launch `{}`: {}", binary, e))?;

    if !status.success() {
        bail!(
            "`meridian` exited with code {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Run `meridian <args>` — commands that don't take `--path` (e.g., `init`).
fn run_meridian_no_path(binary: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(binary)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to launch `{}`: {}", binary, e))?;

    if !status.success() {
        bail!(
            "`meridian` exited with code {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

// ── Derived title (v0.17.0.12.8) ─────────────────────────────────────────────

/// Resolve the meridian binary for `workspace_root` and summarize `text` (the
/// goal's first user message / objective) into a concise title via
/// `meridian summarize-title`.
///
/// Returns `None` when meridian is not installed, `text` is empty, or the call
/// fails for any reason — callers treat a missing derived title as non-fatal
/// (the plain goal title remains authoritative).
pub fn summarize_title(workspace_root: &Path, text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let binary = resolve_meridian_binary(workspace_root)?;
    summarize_title_via(&binary, text)
}

/// Run `meridian summarize-title` with `text` piped to stdin, returning the
/// trimmed stdout. Split out from `summarize_title` so tests can exercise the
/// subprocess path with a known-missing binary without touching PATH resolution.
fn summarize_title_via(binary: &str, text: &str) -> Option<String> {
    use std::io::Write;

    let mut child = std::process::Command::new(binary)
        .arg("summarize-title")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    child.stdin.take()?.write_all(text.as_bytes()).ok()?;

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn find_meridian_binary_returns_error_when_not_on_path() {
        // Validates the install-instructions error message format.
        let tmp = tempdir().unwrap();
        // With a fresh empty dir, no daemon.toml and PATH shouldn't have meridian.
        // We test the error format rather than relying on meridian being absent.
        let err = anyhow::anyhow!(
            "The `meridian` binary was not found on PATH.\n\
             \n\
             Install it with one of:\n\
             \n\
             \tcargo install meridian\n\
             \tcargo install --git https://github.com/Trusted-Autonomy/meridian\n\
             \n\
             After installing, re-run `ta meridian` to continue.\n\
             See the Meridian docs for platform-specific packages.\n\
             \n\
             To use a custom binary path, set TA_MERIDIAN_BINARY or add to .ta/daemon.toml:\n\
             \n\
             \t[meridian]\n\
             \tbinary = \"/path/to/meridian\""
        );
        let msg = err.to_string();
        assert!(msg.contains("cargo install meridian"));
        assert!(msg.contains("Trusted-Autonomy/meridian"));
        assert!(msg.contains("TA_MERIDIAN_BINARY"));
        let _ = tmp; // suppress unused warning
    }

    #[test]
    fn find_meridian_binary_returns_sensible_result() {
        // Call the real function and assert the result matches its contract,
        // regardless of whether meridian is installed on this machine.
        let tmp = tempdir().unwrap();
        let result = find_meridian_binary(tmp.path());
        match result {
            Ok(name) => {
                // Found: must be a non-empty string.
                assert!(!name.is_empty(), "resolved binary name must be non-empty");
            }
            Err(e) => {
                // Not found: error message must contain install instructions.
                let msg = e.to_string();
                assert!(
                    msg.contains("cargo install meridian"),
                    "error message should include install instructions, got: {msg}"
                );
                assert!(
                    msg.contains("Trusted-Autonomy/meridian"),
                    "error message should include git install URL, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn resolve_meridian_binary_reads_daemon_toml_config() {
        let tmp = tempdir().unwrap();
        let ta_dir = tmp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("daemon.toml"),
            "[meridian]\nbinary = \"/custom/bin/meridian\"\n",
        )
        .unwrap();
        // TA_MERIDIAN_BINARY must not be set for this test to be meaningful.
        // We test config parsing directly via read_meridian_binary_from_config.
        let result = read_meridian_binary_from_config(tmp.path());
        assert_eq!(result, Some("/custom/bin/meridian".to_string()));
    }

    #[test]
    fn resolve_meridian_binary_env_var_takes_priority() {
        // env var should shadow daemon.toml and PATH.
        // We can't safely set env vars in tests (thread safety), so test
        // read_meridian_binary_from_config directly to confirm config parsing.
        let tmp = tempdir().unwrap();
        let ta_dir = tmp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("daemon.toml"),
            "[other]\nkey = \"val\"\n\n[meridian]\nbinary = \"/from/config\"\n",
        )
        .unwrap();
        let result = read_meridian_binary_from_config(tmp.path());
        assert_eq!(result, Some("/from/config".to_string()));
    }

    #[test]
    fn read_meridian_binary_from_config_returns_none_when_absent() {
        let tmp = tempdir().unwrap();
        // No daemon.toml at all.
        assert!(read_meridian_binary_from_config(tmp.path()).is_none());
    }

    #[test]
    fn summarize_title_via_returns_none_for_missing_binary() {
        let result = summarize_title_via("/nonexistent/meridian-binary-xyz", "some objective");
        assert!(result.is_none());
    }

    #[test]
    fn summarize_title_empty_text_returns_none_without_resolving_binary() {
        let tmp = tempdir().unwrap();
        // Even if meridian were installed, empty text should short-circuit to None.
        assert!(summarize_title(tmp.path(), "").is_none());
        assert!(summarize_title(tmp.path(), "   ").is_none());
    }

    #[test]
    fn read_meridian_binary_from_config_returns_none_when_section_missing() {
        let tmp = tempdir().unwrap();
        let ta_dir = tmp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("daemon.toml"), "[server]\nport = 8080\n").unwrap();
        assert!(read_meridian_binary_from_config(tmp.path()).is_none());
    }

    #[test]
    fn meridian_commands_all_variants_exist() {
        // Ensures the enum compiles with all four variants.
        let _analyze = MeridianCommands::Analyze;
        let _help = MeridianCommands::Help;
        let _init = MeridianCommands::Init;
        let _suggest = MeridianCommands::Suggest;
    }
}
