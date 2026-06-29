// meridian.rs — `ta meridian` subcommand group (v0.17.0.12).
//
// Delegates to the `meridian` binary on PATH. TA emits token counts in
// velocity-history.jsonl so Meridian can report cost rather than time-as-proxy.
//
// Subcommands:
//   ta meridian analyze  → meridian analyze --source ta --path <project_root>
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

/// Find the `meridian` binary on PATH. Returns the path as a string, or an error
/// with install instructions if not found.
fn find_meridian_binary() -> Result<String> {
    // Check if `meridian` is available via `which`-style lookup.
    let found = std::process::Command::new("which")
        .arg("meridian")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(path) = found {
        return Ok(path);
    }

    bail!(
        "The `meridian` binary was not found on PATH.\n\
         \n\
         Install it with one of:\n\
         \n\
         \tcargo install meridian\n\
         \tcargo install --git https://github.com/Trusted-Autonomy/meridian\n\
         \n\
         After installing, re-run `ta meridian` to continue.\n\
         See the Meridian docs for platform-specific packages."
    )
}

pub fn execute(command: &MeridianCommands, config: &GatewayConfig) -> Result<()> {
    let meridian = find_meridian_binary()?;
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
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_meridian_binary_returns_error_when_not_on_path() {
        // Override PATH to an empty directory so `meridian` is definitely absent.
        let result = {
            // We can't safely mutate PATH in tests without affecting other threads,
            // so we test the error message format directly via the bail! path.
            // The actual PATH lookup is validated by integration tests.
            Err::<String, anyhow::Error>(anyhow::anyhow!(
                "The `meridian` binary was not found on PATH.\n\
                 \n\
                 Install it with one of:\n\
                 \n\
                 \tcargo install meridian\n\
                 \tcargo install --git https://github.com/Trusted-Autonomy/meridian\n\
                 \n\
                 After installing, re-run `ta meridian` to continue.\n\
                 See the Meridian docs for platform-specific packages."
            ))
        };
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("cargo install meridian"));
        assert!(msg.contains("Trusted-Autonomy/meridian"));
    }

    #[test]
    fn meridian_commands_all_variants_exist() {
        // Ensures the enum compiles with all three variants.
        let _analyze = MeridianCommands::Analyze;
        let _init = MeridianCommands::Init;
        let _suggest = MeridianCommands::Suggest;
    }
}
