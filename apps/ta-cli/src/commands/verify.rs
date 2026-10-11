// verify.rs - Pre-draft verification gate (v0.10.8, v0.10.18.3, v0.17.11.29).
//
// Runs configurable build/lint/test checks in a staging directory.
// Used by `ta run` (after agent exit, before draft build) and
// by `ta verify` (standalone manual verification).
//
// v0.10.18.3: Streaming stdout/stderr, heartbeat progress, per-command
// configurable timeouts, and enhanced timeout error messages.
//
// v0.17.11.29: Diagnosable failures. A step with no output for a configured time
// is reported as hung (with the test names that started and never finished) and
// killed instead of waiting forever; failures name the failing tests; every step's
// duration is summarised at the end.

use std::cmp::Reverse;
use std::io::{BufRead, IsTerminal};
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
    run_verification_with_env(config, staging_dir, &[])
}

/// Run verification commands inside a goal's staging directory.
///
/// Cargo is pointed at the goal's own target directory (`<staging>/target`), never the
/// source project's: artifacts compiled in staging bake the staging path into test
/// binaries (`env!("CARGO_MANIFEST_DIR")`), which break once staging is cleaned.
pub fn run_verification_in_staging(
    config: &VerifyConfig,
    staging_dir: &Path,
) -> VerificationResult {
    let env = goal_cargo_env(staging_dir);
    run_verification_with_env(config, staging_dir, &env)
}

/// The cargo environment for commands run in a goal's staging directory.
pub fn goal_cargo_env(staging_dir: &Path) -> Vec<(String, String)> {
    if staging_dir.join("Cargo.toml").exists() {
        vec![(
            "CARGO_TARGET_DIR".to_string(),
            goal_target_dir(staging_dir).display().to_string(),
        )]
    } else {
        Vec::new()
    }
}

/// A goal's own cargo target directory. Lives inside the staging directory, so it is
/// removed with it and is never shared with the source tree's `target/`.
pub fn goal_target_dir(staging_dir: &Path) -> std::path::PathBuf {
    staging_dir.join("target")
}

/// Same as [`run_verification`], with extra environment variables for every command.
pub fn run_verification_with_env(
    config: &VerifyConfig,
    staging_dir: &Path,
    env: &[(String, String)],
) -> VerificationResult {
    if config.commands.is_empty() {
        return VerificationResult {
            passed: true,
            warnings: Vec::new(),
        };
    }

    let total = config.commands.len();
    let total_timeout_secs: u64 = config
        .commands
        .iter()
        .map(|c| config.command_timeout(c))
        .sum();

    println!();
    println!(
        "[apply] Running verification ({} command{}, timeout {}s):",
        total,
        if total == 1 { "" } else { "s" },
        total_timeout_secs
    );
    for (i, cmd) in config.commands.iter().enumerate() {
        let quiet_limit = match config.command_no_output_timeout(cmd) {
            Some(s) => format!("hung after {}s without output", s),
            None => "no-output limit off".to_string(),
        };
        println!(
            "  {}/{}  {}  ({}s, {})",
            i + 1,
            total,
            cmd.run,
            config.command_timeout(cmd),
            quiet_limit
        );
    }
    println!();

    let mut warnings = Vec::new();
    let mut all_passed = true;
    // (label, outcome, elapsed) per step, printed as a summary at the end.
    let mut timings: Vec<(String, &'static str, Duration)> = Vec::new();

    for (i, cmd) in config.commands.iter().enumerate() {
        let timeout_secs = config.command_timeout(cmd);
        let timeout = Duration::from_secs(timeout_secs);
        let heartbeat = Duration::from_secs(config.heartbeat_interval_secs);
        let no_output = config
            .command_no_output_timeout(cmd)
            .map(Duration::from_secs);

        // Overwrite the previous spinner line (or blank line) to highlight the active step.
        if std::io::stdout().is_terminal() {
            let indicator = format!("▶ {}/{}  {}", i + 1, total, cmd.run);
            print!(
                "\r{}{}\r",
                indicator,
                " ".repeat(80_usize.saturating_sub(indicator.len()))
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }

        let step_start = Instant::now();
        match run_single_command_ext(
            &cmd.run,
            staging_dir,
            &CommandLimits {
                timeout,
                no_output_timeout: no_output,
                heartbeat_interval: heartbeat,
            },
            (i + 1, total),
            env,
        ) {
            Ok(output) => {
                if output.success {
                    timings.push((cmd.run.clone(), "PASS", output.elapsed));
                    println!(
                        "  [{}/{}] PASS  {} ({:.1}s)",
                        i + 1,
                        total,
                        cmd.run,
                        output.elapsed.as_secs_f64()
                    );
                } else {
                    all_passed = false;
                    timings.push((cmd.run.clone(), "FAIL", output.elapsed));
                    println!(
                        "  [{}/{}] FAIL  {} (exit code: {}, {:.1}s)",
                        i + 1,
                        total,
                        cmd.run,
                        output.exit_code.unwrap_or(-1),
                        output.elapsed.as_secs_f64()
                    );

                    let lines: Vec<String> =
                        output.combined_output.lines().map(str::to_string).collect();
                    let diag = diagnose_output(&lines);
                    let summary = diag.failure_summary();
                    if !summary.is_empty() {
                        println!("{}", summary);
                    }
                    let stored =
                        format!("{}{}", summary, truncate_head_tail(&output.combined_output));

                    warnings.push(VerificationWarning {
                        command: cmd.run.clone(),
                        exit_code: output.exit_code,
                        output: stored,
                    });
                }
            }
            Err(e) => {
                all_passed = false;
                let outcome = if e.to_string().contains("HUNG") {
                    "HUNG"
                } else {
                    "ERROR"
                };
                timings.push((cmd.run.clone(), outcome, step_start.elapsed()));
                println!("  [{}/{}] {}  {}\n{}", i + 1, total, outcome, cmd.run, e);
                warnings.push(VerificationWarning {
                    command: cmd.run.clone(),
                    exit_code: None,
                    output: e.to_string(),
                });
            }
        }
    }

    println!();
    println!("  Step timings:");
    for (i, (run, outcome, elapsed)) in timings.iter().enumerate() {
        println!(
            "    {}/{}  {:<5} {:>7.1}s  {}",
            i + 1,
            total,
            outcome,
            elapsed.as_secs_f64(),
            run
        );
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

/// Keep the first and last part of long output (failing test names are usually at the
/// end). Splits on char boundaries, so multi-byte output never panics.
fn truncate_head_tail(s: &str) -> String {
    const HEAD: usize = 600;
    const TAIL: usize = 1400;
    if s.len() <= HEAD + TAIL {
        return s.to_string();
    }
    let mut head_end = HEAD;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - TAIL;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n... ({} bytes omitted, {} bytes total) ...\n{}",
        &s[..head_end],
        tail_start - head_end,
        s.len(),
        &s[tail_start..]
    )
}

/// What a command's output says about test progress.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputDiagnosis {
    /// Tests that reported a failure (`test x ... FAILED`, `---- x stdout ----`).
    pub failing: Vec<String>,
    /// Tests that started (or are reported as running long) and never finished.
    pub unfinished: Vec<String>,
    /// The last cargo `Running ...` line (which test binary was executing).
    pub last_binary: Option<String>,
}

impl OutputDiagnosis {
    /// Lines naming the failing tests, or an empty string when there are none.
    pub fn failure_summary(&self) -> String {
        if self.failing.is_empty() {
            return String::new();
        }
        let mut out = format!("  Failing tests ({}):\n", self.failing.len());
        for name in self.failing.iter().take(25) {
            out.push_str(&format!("    - {}\n", name));
        }
        if self.failing.len() > 25 {
            out.push_str(&format!("    ... and {} more\n", self.failing.len() - 25));
        }
        out
    }

    /// Lines naming the tests that were still running, or an empty string.
    pub fn hang_summary(&self) -> String {
        let mut out = String::new();
        if let Some(bin) = &self.last_binary {
            out.push_str(&format!("  Last test binary started: {}\n", bin));
        }
        if self.unfinished.is_empty() {
            out.push_str("  No test was seen starting without finishing (the step is not a libtest run, or it printed nothing).\n");
        } else {
            out.push_str("  Last tests started and not finished:\n");
            let start = self.unfinished.len().saturating_sub(8);
            for name in &self.unfinished[start..] {
                out.push_str(&format!("    - {}\n", name));
            }
        }
        out
    }
}

/// Read libtest/cargo output and extract failing and still-running test names.
pub(crate) fn diagnose_output(lines: &[String]) -> OutputDiagnosis {
    let mut diag = OutputDiagnosis::default();
    for raw in lines {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Running ") {
            diag.last_binary = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("---- ") {
            if let Some(name) = rest.strip_suffix(" stdout ----") {
                let name = name.trim().to_string();
                if !diag.failing.contains(&name) {
                    diag.failing.push(name);
                }
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        if let Some((name, _)) = rest.split_once(" has been running for over ") {
            let name = name.trim().to_string();
            diag.unfinished.retain(|n| n != &name);
            diag.unfinished.push(name);
            continue;
        }
        if let Some((name, result)) = rest.split_once(" ... ") {
            let name = name.trim().to_string();
            let result = result.trim();
            if result.is_empty() {
                diag.unfinished.retain(|n| n != &name);
                diag.unfinished.push(name);
            } else {
                diag.unfinished.retain(|n| n != &name);
                if result.starts_with("FAILED") && !diag.failing.contains(&name) {
                    diag.failing.push(name);
                }
            }
        } else if let Some(name) = rest.strip_suffix(" ...") {
            let name = name.trim().to_string();
            diag.unfinished.retain(|n| n != &name);
            diag.unfinished.push(name);
        }
    }
    diag
}

/// Output from running a single verification command.
#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub combined_output: String,
    pub elapsed: Duration,
}

/// Time limits for one verification command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CommandLimits {
    /// Hard limit on total run time.
    pub timeout: Duration,
    /// Report the step as hung (and kill it) after this long without any output.
    /// `None` disables the check.
    pub no_output_timeout: Option<Duration>,
    pub heartbeat_interval: Duration,
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

/// Kill the command and everything it started (the shell plus its cargo/test children).
fn kill_command_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child was spawned as its own process group leader, so -pid reaches every
        // descendant; killing only the shell would leave a hung test binary running.
        let pid = child.id() as i32;
        // SAFETY: plain syscall on a pid we spawned; failure is ignored and we fall back
        // to killing the direct child below.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .output();
    }
    let _ = child.kill();
    let _ = child.wait(); // Reap zombie.
}

/// Run a single shell command with only a total timeout (no no-output limit).
#[cfg(test)]
fn run_single_command(
    cmd: &str,
    working_dir: &Path,
    timeout: Duration,
    heartbeat_interval: Duration,
    position: (usize, usize),
) -> anyhow::Result<CommandOutput> {
    run_single_command_ext(
        cmd,
        working_dir,
        &CommandLimits {
            timeout,
            no_output_timeout: None,
            heartbeat_interval,
        },
        position,
        &[],
    )
}

/// Run a single shell command with streaming output, heartbeat, and timeouts.
///
/// - Stdout and stderr are streamed line-by-line with a `[label]` prefix.
/// - A heartbeat line is emitted every `heartbeat_interval` while running.
/// - On timeout, the error includes the last 20 lines of output.
/// - With `no_output_timeout`, a step that prints nothing for that long is killed and
///   reported as HUNG, naming the tests that started and never finished.
/// - `position` is (1-based index, total) used in heartbeat and error labels.
fn run_single_command_ext(
    cmd: &str,
    working_dir: &Path,
    limits: &CommandLimits,
    position: (usize, usize),
    env: &[(String, String)],
) -> anyhow::Result<CommandOutput> {
    use std::process::{Command, Stdio};

    let CommandLimits {
        timeout,
        no_output_timeout,
        heartbeat_interval,
    } = *limits;
    let label = command_label(cmd);

    #[cfg(windows)]
    let mut command = {
        let mut c = Command::new("cmd");
        c.arg("/c").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
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

    // Progress tracking for the no-output (hang) limit and the stall warning.
    let stall_threshold = Duration::from_secs(300); // 5 minutes of silence
    let mut last_output_len: usize = 0;
    let mut last_progress_at = start;
    let mut stall_warned = false;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited — wait for reader threads to finish.
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();

                // Clear the in-place spinner line before printing final status.
                if std::io::stdout().is_terminal() {
                    print!("\r{}\r", " ".repeat(80));
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }

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

                // Track output progress for hang and stall detection.
                let current_len = output_lines.lock().unwrap().len();
                if current_len > last_output_len {
                    last_output_len = current_len;
                    last_progress_at = Instant::now();
                }

                // Hung: no output for the configured time.
                if let Some(limit) = no_output_timeout {
                    if last_progress_at.elapsed() >= limit {
                        kill_command_tree(&mut child);
                        let _ = stdout_handle.join();
                        let _ = stderr_handle.join();

                        let lines = output_lines.lock().unwrap().clone();
                        let diag = diagnose_output(&lines);
                        let tail = last_lines(&lines, 20);
                        return Err(anyhow::anyhow!(
                            "Verification step {}/{} HUNG: `{}` printed no output for {}s (ran {}s \
                             in total) and was killed.\n\
                             {}\n\
                             Last {} line(s) of output:\n{}\n\n\
                             To change the limit, set it for this command in .ta/workflow.toml:\n\
                             [[verify.commands]]\n\
                             run = \"{}\"\n\
                             no_output_timeout_secs = {}   # 0 disables the check\n\
                             or for every command:\n\
                             [verify]\n\
                             no_output_timeout_secs = {}   # default 300, 0 disables",
                            position.0,
                            position.1,
                            cmd,
                            limit.as_secs(),
                            elapsed.as_secs(),
                            diag.hang_summary().trim_end(),
                            tail.1,
                            tail.0,
                            cmd.replace('"', "\\\""),
                            limit.as_secs().max(1) * 2,
                            limit.as_secs().max(1) * 2,
                        ));
                    }
                }

                // Check timeout.
                if elapsed > timeout {
                    kill_command_tree(&mut child);
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();

                    let lines = output_lines.lock().unwrap().clone();
                    let (context, count) = last_lines(&lines, 20);

                    return Err(anyhow::anyhow!(
                        "Command timed out after {}s: {}\n\n\
                         Last {} lines of output:\n{}\n\n\
                         To increase the timeout, set timeout_secs for this command in .ta/workflow.toml:\n\
                         [[verify.commands]]\n\
                         run = \"{}\"\n\
                         timeout_secs = {}",
                        timeout.as_secs(),
                        cmd,
                        count,
                        context,
                        cmd,
                        timeout.as_secs() * 2
                    ));
                }

                // Stall warning: emit once when >80% of timeout elapsed with no output.
                if !stall_warned
                    && elapsed.as_secs_f64() > timeout.as_secs_f64() * 0.8
                    && last_progress_at.elapsed() >= stall_threshold
                {
                    stall_warned = true;
                    if std::io::stdout().is_terminal() {
                        print!("\r{}\r", " ".repeat(80));
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                    println!(
                        "\n[warn] Verification command appears stalled (no output for {}s). \
                         Press Ctrl-C to cancel and re-run with --skip-verify.",
                        last_progress_at.elapsed().as_secs()
                    );
                }

                // Heartbeat: emit an in-place spinner line (overwrites previous) so
                // the terminal doesn't flood with repeated "still running" messages.
                if last_heartbeat.elapsed() >= heartbeat_interval {
                    let line_count = output_lines.lock().unwrap().len();
                    let position_label = format!("{}/{} {}", position.0, position.1, label);
                    // \r returns to start of line; trailing spaces wipe leftover chars.
                    // Falls back to a newline if stdout is not a TTY (e.g. CI logs).
                    if std::io::stdout().is_terminal() {
                        print!(
                            "\r        [{}] still running... {}s, {} lines    ",
                            position_label,
                            elapsed.as_secs(),
                            line_count
                        );
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    } else {
                        println!(
                            "        [{}] still running... ({}s elapsed, {} lines captured, \
                             {}s since last output)",
                            position_label,
                            elapsed.as_secs(),
                            line_count,
                            last_progress_at.elapsed().as_secs()
                        );
                    }
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

/// The last `n` lines joined with newlines, and how many there were.
fn last_lines(lines: &[String], n: usize) -> (String, usize) {
    let start = lines.len().saturating_sub(n);
    let tail = &lines[start..];
    if tail.is_empty() {
        ("(no output captured)".to_string(), 0)
    } else {
        (tail.join("\n"), tail.len())
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
        goals.sort_by_key(|g| Reverse(g.created_at));
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
                    no_output_timeout_secs: None,
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
        // Spawn a child that produces 60 lines.
        // Windows cmd does not support bash `for`/`seq` syntax — use for /L instead.
        #[cfg(not(windows))]
        let script = r#"for i in $(seq 1 60); do echo "line $i"; done"#;
        #[cfg(windows)]
        let script = "for /L %i in (1,1,60) do @echo line %i";

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
            (1, 1),
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
            (1, 2),
        );
        assert!(fast.is_ok());
        assert!(fast.unwrap().success);

        // Command 2: very short timeout, slow command → should timeout.
        let slow = run_single_command(
            "sleep 10 && echo done",
            dir.path(),
            Duration::from_secs(1),
            Duration::from_secs(30),
            (2, 2),
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
        // Windows cmd does not support bash loop syntax — use ping as a 1s delay.
        #[cfg(not(windows))]
        let script = "for i in 1 2 3; do echo tick$i; sleep 1; done";
        #[cfg(windows)]
        let script =
            "echo tick1 & ping -n 2 -w 1000 127.0.0.1 & echo tick2 & ping -n 2 -w 1000 127.0.0.1 & echo tick3";

        let dir = TempDir::new().unwrap();
        let output = run_single_command(
            script,
            dir.path(),
            Duration::from_secs(30),
            Duration::from_secs(1),
            (1, 1),
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
        // Produce some output then block until the timeout fires.
        // Windows cmd does not support bash loop/seq syntax — use for /L + ping.
        #[cfg(not(windows))]
        let script = "for i in $(seq 1 5); do echo line$i; done; sleep 30";
        // for /L prints line1..line5 quickly, then ping blocks for ~30 s.
        #[cfg(windows)]
        let script = "for /L %i in (1,1,5) do @echo line%i & ping -n 31 -w 1000 127.0.0.1";

        let result = run_single_command(
            script,
            dir.path(),
            Duration::from_secs(2),
            Duration::from_secs(30),
            (1, 1),
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

    // ── v0.17.11.29: diagnosable verification ─────────────────────────────

    /// A fake step that behaves like a hung `cargo test`: it prints the test binary,
    /// one test that started, and one reported as slow, then blocks without output.
    fn hanging_script() -> &'static str {
        #[cfg(not(windows))]
        {
            "echo 'Running unittests src/lib.rs (target/debug/deps/fake-abc123)'; \
             echo 'test alpha::finishes ... ok'; \
             echo 'test beta::starts_and_hangs ... '; \
             echo 'test gamma::slow has been running for over 60 seconds'; \
             sleep 30"
        }
        #[cfg(windows)]
        {
            "echo Running unittests src/lib.rs (target/debug/deps/fake-abc123) & \
             echo test alpha::finishes ... ok & \
             echo test beta::starts_and_hangs ... & \
             echo test gamma::slow has been running for over 60 seconds & \
             ping -n 31 -w 1000 127.0.0.1"
        }
    }

    #[test]
    fn hung_step_is_reported_with_started_tests_and_how_to_change_the_limit() {
        let dir = TempDir::new().unwrap();
        let started = Instant::now();
        let err = run_single_command_ext(
            hanging_script(),
            dir.path(),
            &CommandLimits {
                timeout: Duration::from_secs(60),
                no_output_timeout: Some(Duration::from_secs(1)),
                heartbeat_interval: Duration::from_secs(30),
            },
            (2, 3),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the hung step must be killed at the no-output limit, took {:?}",
            started.elapsed()
        );
        assert!(err.contains("HUNG"), "{err}");
        assert!(err.contains("2/3"), "names the step: {err}");
        assert!(err.contains("no output for 1s"), "names the limit: {err}");
        assert!(
            err.contains("beta::starts_and_hangs"),
            "last started test: {err}"
        );
        assert!(err.contains("gamma::slow"), "slow test: {err}");
        assert!(
            !err.contains("alpha::finishes\n    -"),
            "finished tests are not listed as hung: {err}"
        );
        assert!(err.contains("fake-abc123"), "which test binary: {err}");
        assert!(
            err.contains("no_output_timeout_secs"),
            "says how to change it: {err}"
        );
        assert!(err.contains(".ta/workflow.toml"), "says where: {err}");
    }

    #[test]
    fn hung_step_through_run_verification_uses_the_configured_limit() {
        let mut config = simple_config(vec![hanging_script()], 60);
        config.no_output_timeout_secs = Some(1);
        let dir = TempDir::new().unwrap();
        let started = Instant::now();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        assert!(started.elapsed() < Duration::from_secs(20));
        let out = &result.warnings[0].output;
        assert!(
            out.contains("HUNG") && out.contains("beta::starts_and_hangs"),
            "{out}"
        );
    }

    #[test]
    fn no_output_limit_of_zero_disables_the_hang_check() {
        #[cfg(not(windows))]
        let quiet = "sleep 2";
        #[cfg(windows)]
        let quiet = "ping -n 3 -w 1000 127.0.0.1 >nul";
        let mut config = simple_config(vec![quiet], 60);
        config.no_output_timeout_secs = Some(0);
        let dir = TempDir::new().unwrap();
        assert!(run_verification(&config, dir.path()).passed);
    }

    #[test]
    fn failing_step_names_the_failing_tests_even_when_output_is_long() {
        #[cfg(not(windows))]
        let script = "for i in $(seq 1 400); do echo \"test filler::t$i ... ok\"; done; \
                      echo 'test real::broken ... FAILED'; exit 1";
        #[cfg(windows)]
        let script = "for /L %i in (1,1,400) do @echo test filler::t%i ... ok & \
                      echo test real::broken ... FAILED & exit /b 1";
        let config = simple_config(vec![script], 60);
        let dir = TempDir::new().unwrap();
        let result = run_verification(&config, dir.path());
        assert!(!result.passed);
        let out = &result.warnings[0].output;
        assert!(out.contains("Failing tests (1)"), "{out}");
        assert!(out.contains("real::broken"), "{out}");
    }

    #[test]
    fn diagnose_output_tracks_started_finished_and_failed_tests() {
        let lines: Vec<String> = [
            "   Running unittests src/main.rs (target/debug/deps/ta-1)",
            "test a::done ... ok",
            "test b::open ... ",
            "test c::bad ... FAILED",
            "test d::slow has been running for over 60 seconds",
            "---- e::panicked stdout ----",
            "test b::open ... ok",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let d = diagnose_output(&lines);
        assert_eq!(d.failing, vec!["c::bad", "e::panicked"]);
        assert_eq!(d.unfinished, vec!["d::slow"]);
        assert_eq!(
            d.last_binary.as_deref(),
            Some("unittests src/main.rs (target/debug/deps/ta-1)")
        );
    }

    #[test]
    fn truncate_head_tail_keeps_the_tail_and_never_splits_a_char() {
        let long = format!("{}{}END", "é".repeat(3000), "x".repeat(10));
        let t = truncate_head_tail(&long);
        assert!(t.ends_with("END"));
        assert!(t.contains("bytes omitted"));
        assert_eq!(truncate_head_tail("short"), "short");
    }

    #[test]
    fn goal_cargo_env_points_at_the_staging_target_only_for_cargo_projects() {
        let dir = TempDir::new().unwrap();
        assert!(goal_cargo_env(dir.path()).is_empty());
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let env = goal_cargo_env(dir.path());
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CARGO_TARGET_DIR");
        assert_eq!(std::path::Path::new(&env[0].1), dir.path().join("target"));
    }
}
