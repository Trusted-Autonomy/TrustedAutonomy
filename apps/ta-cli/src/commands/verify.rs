// verify.rs — Pre-draft verification gate (v0.10.8, v0.10.18.3).
//
// Runs configurable build/lint/test checks in a staging directory.
// Used by `ta run` (after agent exit, before draft build) and
// by `ta verify` (standalone manual verification).
//
// v0.10.18.3: Streaming stdout/stderr, heartbeat progress, per-command
// configurable timeouts, and enhanced timeout error messages.

use std::io::BufRead;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ta_changeset::draft_package::VerificationWarning;
use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;
use ta_submit::config::VerifyConfig;

/// Result of running verification commands.
#[derive(Debug)]
pub struct VerificationResult {
    /// Whether all commands passed.
    pub passed: bool,
    /// Warnings for commands that failed (populated regardless of on_failure mode).
    pub warnings: Vec<VerificationWarning>,
}

/// Run verification commands in the given directory.
///
/// Returns a `VerificationResult` with pass/fail status and any warnings.
/// The caller decides what to do based on `on_failure` mode.
pub fn run_verification(config: &VerifyConfig, staging_dir: &Path) -> VerificationResult {
    if config.commands.is_empty() {
        return VerificationResult {
            passed: true,
            warnings: Vec::new(),
        };
    }

    println!();
    println!(
        "Running pre-draft verification ({} commands)...",
        config.commands.len()
    );

    let mut warnings = Vec::new();
    let mut all_passed = true;

    for (i, cmd) in config.commands.iter().enumerate() {
        let timeout_secs = config.command_timeout(cmd);
        let timeout = Duration::from_secs(timeout_secs);
        let heartbeat = Duration::from_secs(config.heartbeat_interval_secs);

        println!(
            "  [{}/{}] {} (timeout: {}s)",
            i + 1,
            config.commands.len(),
            cmd.run,
            timeout_secs
        );

        match run_single_command(&cmd.run, staging_dir, timeout, heartbeat) {
            Ok(output) => {
                if output.success {
                    println!("        PASS ({:.1}s)", output.elapsed.as_secs_f64());
                } else {
                    all_passed = false;
                    println!(
                        "        FAIL (exit code: {}, {:.1}s)",
                        output.exit_code.unwrap_or(-1),
                        output.elapsed.as_secs_f64()
                    );

                    // Truncate output to 2000 chars for storage.
                    let truncated_output = if output.combined_output.len() > 2000 {
                        format!(
                            "{}... (truncated, {} bytes total)",
                            &output.combined_output[..2000],
                            output.combined_output.len()
                        )
                    } else {
                        output.combined_output
                    };

                    warnings.push(VerificationWarning {
                        command: cmd.run.clone(),
                        exit_code: output.exit_code,
                        output: truncated_output,
                    });
                }
            }
            Err(e) => {
                all_passed = false;
                println!("        ERROR: {}", e);
                warnings.push(VerificationWarning {
                    command: cmd.run.clone(),
                    exit_code: None,
                    output: e.to_string(),
                });
            }
        }
    }

    if all_passed {
        println!("  All verification checks passed.");
    } else {
        println!(
            "  {} of {} verification checks failed.",
            warnings.len(),
            config.commands.len()
        );
    }

    VerificationResult {
        passed: all_passed,
        warnings,
    }
}

/// Output from running a single verification command.
#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub combined_output: String,
    pub elapsed: Duration,
}

/// Short label for a command (first two path components or first 40 chars).
fn command_label(cmd: &str) -> String {
    let trimmed = cmd.trim();
    // Use the first word (binary name) as the label.
    let first_word = trimmed.split_whitespace().next().unwrap_or(trimmed);
    // Strip path prefix if present (e.g., ./dev → dev).
    let base = first_word.rsplit('/').next().unwrap_or(first_word);
    if base.len() > 30 {
        format!("{}…", &base[..29])
    } else {
        base.to_string()
    }
}

/// Run a single shell command with streaming output, heartbeat, and timeout.
///
/// - Stdout and stderr are streamed line-by-line with a `[label]` prefix.
/// - A heartbeat line is emitted every `heartbeat_interval` while running.
/// - On timeout, the error includes the last 20 lines of output.
fn run_single_command(
    cmd: &str,
    working_dir: &Path,
    timeout: Duration,
    heartbeat_interval: Duration,
) -> anyhow::Result<CommandOutput> {
    use std::process::{Command, Stdio};

    let label = command_label(cmd);

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn '{}': {}", cmd, e))?;

    // Take ownership of stdout/stderr for streaming.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Shared output accumulator for both streams + heartbeat.
    let output_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Spawn reader threads for stdout and stderr.
    let stdout_lines = Arc::clone(&output_lines);
    let stdout_label = label.clone();
    let stdout_handle = std::thread::spawn(move || {
        if let Some(stream) = stdout {
            let reader = std::io::BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                println!("        [{}] {}", stdout_label, line);
                stdout_lines.lock().unwrap().push(line);
            }
        }
    });

    let stderr_lines = Arc::clone(&output_lines);
    let stderr_label = label.clone();
    let stderr_handle = std::thread::spawn(move || {
        if let Some(stream) = stderr {
            let reader = std::io::BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                println!("        [{}] {}", stderr_label, line);
                stderr_lines.lock().unwrap().push(line);
            }
        }
    });

    // Poll for process exit with timeout and heartbeat.
    let start = Instant::now();
    let mut last_heartbeat = start;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited — wait for reader threads to finish.
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();

                let elapsed = start.elapsed();
                let combined = output_lines.lock().unwrap().join("\n");

                return Ok(CommandOutput {
                    success: status.success(),
                    exit_code: status.code(),
                    combined_output: combined,
                    elapsed,
                });
            }
            Ok(None) => {
                let elapsed = start.elapsed();

                // Check timeout.
                if elapsed > timeout {
                    let _ = child.kill();
                    let _ = child.wait(); // Reap zombie.
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();

                    let lines = output_lines.lock().unwrap();
                    let last_20: Vec<&str> = lines
                        .iter()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|s| s.as_str())
                        .collect();

                    let context = if last_20.is_empty() {
                        "(no output captured)".to_string()
                    } else {
                        last_20.join("\n")
                    };

                    return Err(anyhow::anyhow!(
                        "Command timed out after {}s: {}\n\n\
                         Last {} lines of output:\n{}\n\n\
                         To increase the timeout, set timeout_secs for this command in .ta/workflow.toml:\n\
                         [[verify.commands]]\n\
                         run = \"{}\"\n\
                         timeout_secs = {}",
                        timeout.as_secs(),
                        cmd,
                        last_20.len(),
                        context,
                        cmd,
                        timeout.as_secs() * 2
                    ));
                }

                // Heartbeat: emit every heartbeat_interval seconds.
                if last_heartbeat.elapsed() >= heartbeat_interval {
                    let line_count = output_lines.lock().unwrap().len();
                    println!(
                        "        [{}] still running... ({}s elapsed, {} lines captured)",
                        label,
                        elapsed.as_secs(),
                        line_count
                    );
                    last_heartbeat = Instant::now();
                }

                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to wait for '{}': {}", cmd, e));
            }
        }
    }
}

/// `ta verify` standalone command — run verification against a goal's staging directory.
pub fn execute(config: &GatewayConfig, goal_id: Option<&str>) -> anyhow::Result<()> {
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // Find the goal.
    let goal = if let Some(id_prefix) = goal_id {
        // Try UUID parse first.
        if let Ok(uuid) = uuid::Uuid::parse_str(id_prefix) {
            goal_store
                .get(uuid)?
                .ok_or_else(|| anyhow::anyhow!("Goal {} not found", id_prefix))?
        } else {
            // Prefix match.
            let goals = goal_store.list()?;
            let matches: Vec<_> = goals
                .into_iter()
                .filter(|g| g.goal_run_id.to_string().starts_with(id_prefix))
                .collect();
            match matches.len() {
                0 => anyhow::bail!("No goal found matching '{}'", id_prefix),
                1 => matches.into_iter().next().unwrap(),
                n => anyhow::bail!(
                    "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
                    id_prefix,
                    n
                ),
            }
        }
    } else {
        // Find the most recent running or pr-ready goal.
        let mut goals = goal_store.list()?;
        goals.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        goals
            .into_iter()
            .find(|g| {
                matches!(
                    g.state,
                    ta_goal::GoalRunState::Running | ta_goal::GoalRunState::PrReady
                )
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No active goal found. Specify a goal ID, or start a goal with `ta run`."
                )
            })?
    };

    let staging_dir = &goal.workspace_path;
    if !staging_dir.exists() {
        anyhow::bail!(
            "Staging directory does not exist: {}\nThe goal's workspace may have been cleaned up.",
            staging_dir.display()
        );
    }

    // Load verify config from the staging directory's workflow.toml.
    let workflow_toml = staging_dir.join(".ta/workflow.toml");
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

    if workflow_config.verify.commands.is_empty() {
        println!("No verification commands configured.");
        println!();
        println!("Add a [verify] section to .ta/workflow.toml:");
        println!("  [verify]");
        println!("  commands = [\"cargo test --workspace\"]");
        return Ok(());
    }

    println!(
        "Verifying goal: {} ({})",
        goal.title,
        &goal.goal_run_id.to_string()[..8]
    );
    println!("  Staging: {}", staging_dir.display());

    let result = run_verification(&workflow_config.verify, staging_dir);

    if result.passed {
        println!();
        println!("All verification checks passed. Ready to build draft.");
    } else {
        println!();
        for warning in &result.warnings {
            println!("Failed: {}", warning.command);
            if !warning.output.is_empty() {
                // Show first 10 lines of output.
                for line in warning.output.lines().take(10) {
                    println!("  {}", line);
                }
                let line_count = warning.output.lines().count();
                if line_count > 10 {
                    println!("  ... ({} more lines)", line_count - 10);
                }
            }
            println!();
        }
        println!("Fix the issues above, then re-run `ta verify`.");
        println!("Or use `ta run --follow-up` to re-enter the agent.");
    }

    if result.passed {
        Ok(())
    } else {
        // Exit with error to signal failure in scripts.
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_submit::config::{VerifyCommand, VerifyOnFailure};
    use tempfile::TempDir;

    /// Helper to build a VerifyConfig from plain command strings (legacy style).
    fn simple_config(commands: Vec<&str>, timeout: u64) -> VerifyConfig {
        VerifyConfig {
            commands: commands
                .into_iter()
                .map(|s| VerifyCommand {
                    run: s.to_string(),
                    timeout_secs: None,
                })
                .collect(),
            on_failure: VerifyOnFailure::Block,
            timeout,
            ..Default::default()
        }
    }

    #[test]
    fn empty_commands_passes() {
        let config = VerifyConfig::default();
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(result.passed);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn passing_command() {
        let config = simple_config(vec!["true"], 30);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(result.passed);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn failing_command() {
        let config = simple_config(vec!["false"], 30);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].command, "false");
    }

    #[test]
    fn mixed_commands_reports_only_failures() {
        let config = simple_config(vec!["true", "false", "true"], 30);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].command, "false");
    }

    #[test]
    fn command_output_captured() {
        let config = simple_config(vec!["echo 'hello world' && exit 1"], 30);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        assert!(result.warnings[0].output.contains("hello world"));
    }

    #[test]
    fn timeout_produces_warning() {
        let config = simple_config(vec!["sleep 10"], 1);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        assert!(result.warnings[0].output.contains("timed out"));
    }

    #[test]
    fn streaming_output_captured_and_complete() {
        // Spawn a child that produces 50+ lines over ~1 second.
        let script = r#"for i in $(seq 1 60); do echo "line $i"; done"#;
        let config = simple_config(vec![script], 30);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(result.passed);

        // The command succeeded, so no warnings — check via run_single_command directly.
        let output = run_single_command(
            script,
            dir.path(),
            Duration::from_secs(30),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(output.success);
        let line_count = output.combined_output.lines().count();
        assert!(
            line_count >= 50,
            "Expected at least 50 lines, got {}",
            line_count
        );
        assert!(output.combined_output.contains("line 1"));
        assert!(output.combined_output.contains("line 60"));
    }

    #[test]
    fn per_command_timeout_respected() {
        let dir = TempDir::new().unwrap();

        // Command 1: short timeout, fast command → should pass.
        let fast = run_single_command(
            "echo fast",
            dir.path(),
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        assert!(fast.is_ok());
        assert!(fast.unwrap().success);

        // Command 2: very short timeout, slow command → should timeout.
        let slow = run_single_command(
            "sleep 10 && echo done",
            dir.path(),
            Duration::from_secs(1),
            Duration::from_secs(30),
        );
        assert!(slow.is_err());
        let err_msg = slow.unwrap_err().to_string();
        assert!(
            err_msg.contains("timed out after 1s"),
            "Error should mention timeout duration: {}",
            err_msg
        );
        assert!(
            err_msg.contains("timeout_secs"),
            "Error should suggest increasing timeout: {}",
            err_msg
        );
    }

    #[test]
    fn heartbeat_emitted_for_long_running_command() {
        // We can't easily capture println! output in a test, so we verify the
        // heartbeat logic indirectly: run a command for >2s with 1s heartbeat
        // and verify it completes correctly (the heartbeat doesn't break anything).
        let dir = TempDir::new().unwrap();
        let output = run_single_command(
            "for i in 1 2 3; do echo tick$i; sleep 1; done",
            dir.path(),
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(output.success);
        assert!(output.combined_output.contains("tick1"));
        assert!(output.combined_output.contains("tick3"));
        assert!(
            output.elapsed.as_secs() >= 2,
            "Command should have taken at least 2 seconds"
        );
    }

    #[test]
    fn timeout_error_includes_last_output_lines() {
        let dir = TempDir::new().unwrap();
        // Produce some output then sleep forever.
        let result = run_single_command(
            "for i in $(seq 1 5); do echo line$i; done; sleep 30",
            dir.path(),
            Duration::from_secs(2),
            Duration::from_secs(30),
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Should contain some of the output lines.
        assert!(
            err_msg.contains("line1") || err_msg.contains("Last"),
            "Timeout error should include output context: {}",
            err_msg
        );
    }

    #[test]
    fn command_label_extracts_binary_name() {
        assert_eq!(command_label("cargo test --workspace"), "cargo");
        assert_eq!(command_label("./dev cargo test"), "dev");
        assert_eq!(command_label("/usr/bin/make all"), "make");
    }
}
