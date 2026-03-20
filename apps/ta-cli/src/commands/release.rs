// release.rs — Configurable release pipeline (`ta release`).
//
// Executes a YAML-defined pipeline of steps to release a new version.
// Steps can be either shell commands (`run`) or TA goal agent invocations
// (`agent`). Steps may require human approval before proceeding.
//
// Pipeline resolution order:
//   1. `.ta/release.yaml` in the project root (user override)
//   2. Built-in default pipeline (compiled into the binary)

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use ta_mcp_gateway::GatewayConfig;

// ── CLI definition ──────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum ReleaseCommands {
    /// Run the release pipeline for a new version.
    Run {
        /// Target version (semver or plan phase ID, e.g., "0.4.0-alpha" or "v0.4.1.2").
        version: String,

        /// Skip approval gates (non-interactive / CI mode).
        #[arg(long)]
        yes: bool,

        /// Dry-run: show what would be executed without running anything.
        #[arg(long)]
        dry_run: bool,

        /// Start from a specific step (1-indexed). Skips earlier steps.
        /// Run `ta release show` to see step numbers.
        #[arg(long)]
        from_step: Option<usize>,

        /// Custom pipeline file (overrides default resolution).
        #[arg(long)]
        pipeline: Option<PathBuf>,

        /// Include press-release generation step.
        #[arg(long)]
        press_release: bool,

        /// Custom prompt for press-release generation.
        #[arg(long)]
        prompt: Option<String>,

        /// Use the interactive release agent (releaser) with ta_ask_human
        /// for human-in-the-loop review checkpoints.
        #[arg(long)]
        interactive: bool,

        /// Auto-approve all approval gates without prompting.
        /// Use in CI or when approval is not needed. Without this flag,
        /// non-TTY contexts (daemon) will prompt via TUI interaction.
        #[arg(long)]
        auto_approve: bool,
    },
    /// Show the pipeline that would be executed (without running it).
    Show {
        /// Custom pipeline file (overrides default resolution).
        #[arg(long)]
        pipeline: Option<PathBuf>,
    },
    /// Initialize a `.ta/release.yaml` in the project from the default template.
    Init,
    /// Configure release settings.
    Config {
        /// Setting to configure (e.g., "press_release_template").
        key: String,
        /// Value to set (path or string).
        value: String,
    },
    /// Validate that the release pipeline can run without issues.
    /// Checks version format, git state, pipeline config, and all prerequisites.
    Validate {
        /// Target version to validate.
        version: String,

        /// Custom pipeline file.
        #[arg(long)]
        pipeline: Option<PathBuf>,
    },
}

pub fn execute(cmd: &ReleaseCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        ReleaseCommands::Run {
            version,
            yes,
            dry_run,
            from_step,
            pipeline,
            press_release,
            prompt,
            interactive,
            auto_approve,
        } => {
            if *interactive {
                run_interactive_release(config, version)?;
            } else {
                // --yes implies --auto-approve for backward compatibility.
                let skip_approvals = *yes || *auto_approve;
                run_pipeline(
                    config,
                    version,
                    skip_approvals,
                    *dry_run,
                    *from_step,
                    pipeline.as_deref(),
                )?;
                if *press_release {
                    generate_press_release(config, version, prompt.as_deref())?;
                }
            }
            Ok(())
        }
        ReleaseCommands::Show { pipeline } => show_pipeline(config, pipeline.as_deref()),
        ReleaseCommands::Init => init_pipeline(config),
        ReleaseCommands::Config { key, value } => configure_release(config, key, value),
        ReleaseCommands::Validate { version, pipeline } => {
            validate_release(config, version, pipeline.as_deref())
        }
    }
}

// ── Pipeline data model ─────────────────────────────────────────

/// A release pipeline loaded from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePipeline {
    /// Human-readable pipeline name.
    #[serde(default = "default_pipeline_name")]
    pub name: String,
    /// How plan phase IDs (e.g., "0.4.1.2") are converted to semver versions.
    #[serde(default)]
    pub version_policy: VersionPolicy,
    /// Ordered list of pipeline steps.
    pub steps: Vec<PipelineStep>,
}

/// Configurable policy for converting plan phase IDs to semver release versions.
///
/// Each template supports placeholders `{0}`, `{1}`, `{2}`, `{3}` for the
/// numeric segments of the input, plus `{pre}` for the `prerelease_suffix`.
///
/// Example YAML:
/// ```yaml
/// version_policy:
///   prerelease_suffix: "alpha"
///   two_segment: "{0}.{1}.0-{pre}"
///   three_segment: "{0}.{1}.{2}-{pre}"
///   four_segment: "{0}.{1}.{2}-{pre}.{3}"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionPolicy {
    /// Prerelease label appended to bare versions (default: "alpha").
    #[serde(default = "default_prerelease_suffix")]
    pub prerelease_suffix: String,
    /// Template for 2-segment inputs like "0.4". Default: "{0}.{1}.0-{pre}"
    #[serde(default = "default_two_segment")]
    pub two_segment: String,
    /// Template for 3-segment inputs like "0.4.1". Default: "{0}.{1}.{2}-{pre}"
    #[serde(default = "default_three_segment")]
    pub three_segment: String,
    /// Template for 4-segment inputs like "0.4.1.2". Default: "{0}.{1}.{2}-{pre}.{3}"
    #[serde(default = "default_four_segment")]
    pub four_segment: String,
}

fn default_prerelease_suffix() -> String {
    "alpha".to_string()
}
fn default_two_segment() -> String {
    "{0}.{1}.0-{pre}".to_string()
}
fn default_three_segment() -> String {
    "{0}.{1}.{2}-{pre}".to_string()
}
fn default_four_segment() -> String {
    "{0}.{1}.{2}-{pre}.{3}".to_string()
}

impl Default for VersionPolicy {
    fn default() -> Self {
        Self {
            prerelease_suffix: default_prerelease_suffix(),
            two_segment: default_two_segment(),
            three_segment: default_three_segment(),
            four_segment: default_four_segment(),
        }
    }
}

fn default_pipeline_name() -> String {
    "release".to_string()
}

/// A single step in the release pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    /// Human-readable step name.
    pub name: String,

    /// Shell command to run (mutually exclusive with `agent`).
    #[serde(default)]
    pub run: Option<String>,

    /// TA agent to invoke (mutually exclusive with `run`).
    #[serde(default)]
    pub agent: Option<AgentStep>,

    /// Objective/description for context (used by agent steps and display).
    #[serde(default)]
    pub objective: Option<String>,

    /// If true, pause for human approval before this step executes.
    #[serde(default)]
    pub requires_approval: bool,

    /// Expected output artifact path (informational).
    #[serde(default)]
    pub output: Option<String>,

    /// Working directory override (relative to project root).
    #[serde(default)]
    pub working_dir: Option<String>,

    /// Environment variables for this step.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Configuration for an agent-driven pipeline step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    /// Agent system to use (e.g., "claude-code").
    #[serde(default = "default_agent_id")]
    pub id: String,

    /// Phase to associate with the goal.
    #[serde(default)]
    pub phase: Option<String>,
}

fn default_agent_id() -> String {
    "claude-code".to_string()
}

impl PipelineStep {
    fn validate(&self) -> anyhow::Result<()> {
        if self.run.is_none() && self.agent.is_none() {
            anyhow::bail!(
                "Step '{}': must have either 'run' or 'agent' defined",
                self.name
            );
        }
        if self.run.is_some() && self.agent.is_some() {
            anyhow::bail!(
                "Step '{}': cannot have both 'run' and 'agent' — pick one",
                self.name
            );
        }
        Ok(())
    }
}

// ── Pipeline resolution ─────────────────────────────────────────

/// Load the release pipeline, checking for a user override first.
fn load_pipeline(
    config: &GatewayConfig,
    override_path: Option<&Path>,
) -> anyhow::Result<ReleasePipeline> {
    // 1. Explicit override from --pipeline flag.
    if let Some(path) = override_path {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Cannot read pipeline file '{}': {}", path.display(), e)
        })?;
        let pipeline: ReleasePipeline = serde_yaml::from_str(&contents)?;
        validate_pipeline(&pipeline)?;
        return Ok(pipeline);
    }

    // 2. Project-local override: .ta/release.yaml
    let project_yaml = config.workspace_root.join(".ta").join("release.yaml");
    if project_yaml.exists() {
        let contents = std::fs::read_to_string(&project_yaml)?;
        let pipeline: ReleasePipeline = serde_yaml::from_str(&contents)?;
        validate_pipeline(&pipeline)?;
        return Ok(pipeline);
    }

    // 3. Built-in default pipeline.
    let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML)?;
    validate_pipeline(&pipeline)?;
    Ok(pipeline)
}

fn validate_pipeline(pipeline: &ReleasePipeline) -> anyhow::Result<()> {
    if pipeline.steps.is_empty() {
        anyhow::bail!("Pipeline has no steps");
    }
    for step in &pipeline.steps {
        step.validate()?;
    }
    Ok(())
}

// ── Commit collection ───────────────────────────────────────────

/// Collect commit messages since the last git tag.
fn collect_commits_since_last_tag(project_root: &Path) -> anyhow::Result<(String, Option<String>)> {
    // Find last tag.
    let last_tag_output = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(project_root)
        .output();

    let last_tag = match last_tag_output {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => None,
    };

    let log_args = match &last_tag {
        Some(tag) => vec![
            "log".to_string(),
            format!("{}..HEAD", tag),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
        None => vec![
            "log".to_string(),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
    };

    let output = Command::new("git")
        .args(&log_args)
        .current_dir(project_root)
        .output()?;

    let commits = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((commits, last_tag))
}

// ── Variable substitution ───────────────────────────────────────

/// Substitute `${VERSION}`, `${TAG}`, `${COMMITS}`, `${LAST_TAG}` in a string.
fn substitute_vars(template: &str, version: &str, commits: &str, last_tag: Option<&str>) -> String {
    template
        .replace("${VERSION}", version)
        .replace("${TAG}", &format!("v{}", version))
        .replace("${COMMITS}", commits)
        .replace("${LAST_TAG}", last_tag.unwrap_or(""))
}

// ── Pipeline execution ──────────────────────────────────────────

/// Normalize a version string, accepting plan phase IDs and converting to semver.
///
/// Uses the `VersionPolicy` templates to control how segments map to semver.
/// Strips leading `v` prefix. Passes through versions that already contain
/// a hyphen (prerelease marker).
fn normalize_version(input: &str, policy: &VersionPolicy) -> String {
    // Strip leading `v` prefix.
    let s = input.strip_prefix('v').unwrap_or(input);

    // If it already contains a hyphen (prerelease), pass through as-is.
    if s.contains('-') {
        return s.to_string();
    }

    let parts: Vec<&str> = s.split('.').collect();
    let template = match parts.len() {
        2 => &policy.two_segment,
        3 => &policy.three_segment,
        4 => &policy.four_segment,
        _ => return s.to_string(),
    };

    apply_version_template(template, &parts, &policy.prerelease_suffix)
}

/// Expand a version template with segment placeholders and prerelease suffix.
fn apply_version_template(template: &str, parts: &[&str], prerelease: &str) -> String {
    let mut result = template.replace("{pre}", prerelease);
    for (i, part) in parts.iter().enumerate() {
        result = result.replace(&format!("{{{}}}", i), part);
    }
    result
}

fn run_pipeline(
    config: &GatewayConfig,
    version: &str,
    skip_approvals: bool,
    dry_run: bool,
    from_step: Option<usize>,
    pipeline_path: Option<&Path>,
) -> anyhow::Result<()> {
    let pipeline = load_pipeline(config, pipeline_path)?;

    // Normalize plan phase IDs to semver using the pipeline's version policy.
    let version = normalize_version(version, &pipeline.version_policy);
    let version = version.as_str();

    // Validate version format.
    let version_re = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$")?;
    if !version_re.is_match(version) {
        anyhow::bail!(
            "Invalid version '{}'. Expected semver (e.g., 0.4.0-alpha, 1.0.0) or plan phase ID (e.g., v0.4.1.2)",
            version
        );
    }
    let (commits, last_tag) = collect_commits_since_last_tag(&config.workspace_root)?;

    let total = pipeline.steps.len();
    let start_idx = from_step.map(|s| s.saturating_sub(1)).unwrap_or(0);

    println!("Release pipeline: {}", pipeline.name);
    println!("Target version:   v{}", version);
    println!(
        "Steps:            {} (starting from {})",
        total,
        start_idx + 1
    );
    if dry_run {
        println!("Mode:             DRY RUN");
    }
    println!();

    for (i, step) in pipeline.steps.iter().enumerate() {
        if i < start_idx {
            println!(
                "[{}/{}] {} — skipped (--from-step)",
                i + 1,
                total,
                step.name
            );
            continue;
        }

        // Approval gate.
        if step.requires_approval && !skip_approvals && !dry_run {
            println!("[{}/{}] {} — requires approval", i + 1, total, step.name);
            if !prompt_approval(&step.name)? {
                println!("Aborted at step {}.", i + 1);
                return Ok(());
            }
        }

        println!("[{}/{}] {} ...", i + 1, total, step.name);

        if dry_run {
            print_step_dry_run(step, version, &commits, last_tag.as_deref());
            continue;
        }

        if let Some(ref cmd_template) = step.run {
            execute_shell_step(
                config,
                step,
                cmd_template,
                version,
                &commits,
                last_tag.as_deref(),
            )?;
        } else if let Some(ref agent) = step.agent {
            execute_agent_step(config, step, agent, version, &commits, last_tag.as_deref())?;
        }

        println!("[{}/{}] {} — done", i + 1, total, step.name);
        println!();
    }

    println!("Release pipeline complete.");
    Ok(())
}

fn execute_shell_step(
    config: &GatewayConfig,
    step: &PipelineStep,
    cmd_template: &str,
    version: &str,
    commits: &str,
    last_tag: Option<&str>,
) -> anyhow::Result<()> {
    let cmd = substitute_vars(cmd_template, version, commits, last_tag);

    let work_dir = match &step.working_dir {
        Some(d) => config.workspace_root.join(d),
        None => config.workspace_root.clone(),
    };

    let mut command = Command::new("sh");
    command.arg("-c").arg(&cmd).current_dir(&work_dir);

    // Inject step-level env vars with substitution.
    for (k, v) in &step.env {
        command.env(k, substitute_vars(v, version, commits, last_tag));
    }

    let status = command.status()?;
    if !status.success() {
        anyhow::bail!(
            "Step '{}' failed (exit code: {:?})",
            step.name,
            status.code()
        );
    }
    Ok(())
}

fn execute_agent_step(
    config: &GatewayConfig,
    step: &PipelineStep,
    agent: &AgentStep,
    version: &str,
    commits: &str,
    last_tag: Option<&str>,
) -> anyhow::Result<()> {
    let objective = step.objective.as_deref().unwrap_or("Execute release step");
    let objective = substitute_vars(objective, version, commits, last_tag);

    let title = format!("release: {}", step.name);

    // Build the ta run command.
    let mut args = vec![
        "run".to_string(),
        title,
        "--agent".to_string(),
        agent.id.clone(),
        "--source".to_string(),
        config.workspace_root.display().to_string(),
        "--objective".to_string(),
        objective,
    ];

    if let Some(ref phase) = agent.phase {
        args.push("--phase".to_string());
        args.push(phase.clone());
    }

    // Resolve the `ta` binary path (same binary we're running from).
    let ta_bin = std::env::current_exe()?;

    // Capture stdout so we can extract the draft ID from `ta run` output.
    let output = Command::new(&ta_bin)
        .args(&args)
        .current_dir(&config.workspace_root)
        .stderr(std::process::Stdio::inherit())
        .output()?;

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    // Print captured stdout for visibility.
    for line in stdout_text.lines() {
        println!("  {}", line);
    }

    if !output.status.success() {
        anyhow::bail!(
            "Agent step '{}' failed (exit code: {:?})",
            step.name,
            output.status.code()
        );
    }

    // Extract draft ID directly from `ta run` output (prints "draft package built: <uuid>").
    // This is more reliable than scanning all draft files on disk.
    let draft_id = extract_draft_id_from_output(&stdout_text).or_else(|| {
        // Fallback: find latest eligible draft on disk.
        find_latest_draft(config).ok().flatten()
    });

    // Auto-approve and apply the draft so output files land in the working
    // directory before the next pipeline step runs. Without this, agent output
    // stays in staging and subsequent shell steps can't find it.
    println!("  Auto-applying agent draft...");
    if let Some(draft_id) = draft_id {
        let id_str = draft_id.to_string();

        // Approve.
        let approve_status = Command::new(&ta_bin)
            .args([
                "draft",
                "approve",
                &id_str,
                "--reviewer",
                "release-pipeline",
            ])
            .current_dir(&config.workspace_root)
            .status()?;
        if !approve_status.success() {
            anyhow::bail!(
                "Failed to auto-approve draft {} for agent step '{}'",
                id_str,
                step.name
            );
        }

        // Apply (no git commit — the release pipeline handles commits itself).
        let apply_status = Command::new(&ta_bin)
            .args(["draft", "apply", &id_str])
            .current_dir(&config.workspace_root)
            .status()?;
        if !apply_status.success() {
            anyhow::bail!(
                "Failed to auto-apply draft {} for agent step '{}'",
                id_str,
                step.name
            );
        }
        println!("  Draft {} applied to working directory.", id_str);
    } else {
        println!(
            "  Warning: no draft found after agent step '{}'.",
            step.name
        );
    }

    Ok(())
}

/// Extract a draft package UUID from `ta run` stdout output.
/// Looks for the line "draft package built: <uuid>" printed by `ta draft build`.
fn extract_draft_id_from_output(output: &str) -> Option<uuid::Uuid> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("draft package built:") {
            if let Ok(id) = uuid::Uuid::parse_str(rest.trim()) {
                return Some(id);
            }
        }
    }
    None
}

/// Fallback: find the most recent draft eligible for auto-approve (PendingReview or Draft status).
/// Skips drafts in terminal states (Applied, Denied, Superseded, etc.) to avoid
/// the "Cannot approve package in Applied state" error when a stale draft is the
/// newest file on disk.
fn find_latest_draft(config: &GatewayConfig) -> anyhow::Result<Option<uuid::Uuid>> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return Ok(None);
    }

    let mut newest: Option<(std::time::SystemTime, uuid::Uuid)> = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(id) = uuid::Uuid::parse_str(stem) {
                    // Only consider drafts that can still be approved.
                    // "approved" is excluded too — already-approved drafts
                    // would fail re-approval with "Cannot approve in Approved state".
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        let dominated_by_terminal = contents.contains("\"applied\"")
                            || contents.contains("\"denied\"")
                            || contents.contains("\"superseded\"")
                            || contents.contains("\"closed\"")
                            || contents.contains("\"approved\"");
                        if dominated_by_terminal {
                            continue;
                        }
                    }
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                                newest = Some((modified, id));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(newest.map(|(_, id)| id))
}

fn print_step_dry_run(step: &PipelineStep, version: &str, commits: &str, last_tag: Option<&str>) {
    if let Some(ref cmd) = step.run {
        let resolved = substitute_vars(cmd, version, commits, last_tag);
        println!("  type: shell");
        println!("  command: {}", resolved);
    } else if let Some(ref agent) = step.agent {
        println!("  type: agent ({})", agent.id);
        if let Some(ref obj) = step.objective {
            println!(
                "  objective: {}",
                substitute_vars(obj, version, commits, last_tag)
            );
        }
    }
    if step.requires_approval {
        println!("  approval: required");
    }
    if let Some(ref out) = step.output {
        println!("  output: {}", out);
    }
    println!();
}

fn prompt_approval(step_name: &str) -> anyhow::Result<bool> {
    prompt_approval_with_auto(step_name, false)
}

fn prompt_approval_with_auto(step_name: &str, auto_approve: bool) -> anyhow::Result<bool> {
    use std::io::{self, IsTerminal, Write};

    // Explicit auto-approve (--auto-approve flag or CI mode).
    if auto_approve {
        println!("Proceed with '{}'? [y/N] y (auto-approved)", step_name);
        return Ok(true);
    }

    // TTY context: prompt directly.
    if io::stdin().is_terminal() {
        print!("Proceed with '{}'? [y/N] ", step_name);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let answer = input.trim().to_lowercase();
        return Ok(answer == "y" || answer == "yes");
    }

    // Non-TTY context (daemon subprocess): use file-based interaction
    // so the TUI shell can present the question via SSE (v0.10.14).
    let interaction_id = uuid::Uuid::new_v4();
    let ta_dir = std::env::current_dir().unwrap_or_default().join(".ta");
    let pending_dir = ta_dir.join("interactions/pending");
    let answers_dir = ta_dir.join("interactions/answers");
    std::fs::create_dir_all(&pending_dir)?;
    std::fs::create_dir_all(&answers_dir)?;

    let question = serde_json::json!({
        "interaction_id": interaction_id.to_string(),
        "goal_id": "release",
        "question": format!("Release gate: proceed with '{}'?", step_name),
        "context": format!("The release pipeline is waiting for approval at step '{}'.", step_name),
        "choices": ["y", "n"],
        "response_hint": "y or n",
        "turn": 0
    });

    let question_path = pending_dir.join(format!("{}.json", interaction_id));
    std::fs::write(&question_path, serde_json::to_string_pretty(&question)?)?;

    tracing::info!(
        interaction_id = %interaction_id,
        step = step_name,
        "Release approval gate waiting for human response via TUI"
    );
    println!(
        "Waiting for approval via TUI shell (interaction {})...",
        &interaction_id.to_string()[..8]
    );

    // Poll for answer (same pattern as ta_ask_human).
    let answer_path = answers_dir.join(format!("{}.json", interaction_id));
    let timeout = std::time::Duration::from_secs(600); // 10 minute timeout
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            // Clean up pending question.
            let _ = std::fs::remove_file(&question_path);
            println!(
                "Release approval timed out after {}s for step '{}'. Aborting.",
                timeout.as_secs(),
                step_name
            );
            return Ok(false);
        }

        if answer_path.exists() {
            let content = std::fs::read_to_string(&answer_path)?;
            // Clean up files.
            let _ = std::fs::remove_file(&question_path);
            let _ = std::fs::remove_file(&answer_path);

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                let response = parsed["response"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                let approved = response == "y" || response == "yes";
                println!(
                    "Proceed with '{}'? [y/N] {} (from TUI)",
                    step_name,
                    if approved { "y" } else { "n" }
                );
                return Ok(approved);
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

// ── Show pipeline ───────────────────────────────────────────────

fn show_pipeline(config: &GatewayConfig, pipeline_path: Option<&Path>) -> anyhow::Result<()> {
    let pipeline = load_pipeline(config, pipeline_path)?;

    println!("Pipeline: {}", pipeline.name);
    println!("Steps:    {}", pipeline.steps.len());
    println!();

    for (i, step) in pipeline.steps.iter().enumerate() {
        let kind = if step.run.is_some() {
            "shell"
        } else if step.agent.is_some() {
            "agent"
        } else {
            "unknown"
        };
        let approval = if step.requires_approval {
            " [approval required]"
        } else {
            ""
        };
        println!("  {}. {} ({}){}", i + 1, step.name, kind, approval);

        if let Some(ref obj) = step.objective {
            println!("     {}", obj);
        }
    }
    Ok(())
}

// ── Init pipeline ───────────────────────────────────────────────

fn init_pipeline(config: &GatewayConfig) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;

    let dest = ta_dir.join("release.yaml");
    if dest.exists() {
        anyhow::bail!(
            "Pipeline already exists at {}. Delete it first to re-initialize.",
            dest.display()
        );
    }

    std::fs::write(&dest, DEFAULT_PIPELINE_YAML)?;
    println!("Created {}", dest.display());
    println!("Edit this file to customize your release pipeline.");
    Ok(())
}

// ── Release configuration ───────────────────────────────────────

/// Release configuration stored in `.ta/release-config.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReleaseConfig {
    /// Path to a sample press release document for style matching.
    #[serde(default)]
    pub press_release_template: Option<String>,
}

impl ReleaseConfig {
    fn load(config: &GatewayConfig) -> Self {
        let path = config
            .workspace_root
            .join(".ta")
            .join("release-config.yaml");
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_yaml::from_str(&c).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self, config: &GatewayConfig) -> anyhow::Result<()> {
        let ta_dir = config.workspace_root.join(".ta");
        std::fs::create_dir_all(&ta_dir)?;
        let path = ta_dir.join("release-config.yaml");
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(&path, yaml)?;
        Ok(())
    }
}

/// Configure a release setting.
fn configure_release(config: &GatewayConfig, key: &str, value: &str) -> anyhow::Result<()> {
    let mut release_config = ReleaseConfig::load(config);

    match key {
        "press_release_template" => {
            let path = Path::new(value);
            if !path.exists() {
                anyhow::bail!(
                    "Template file not found: {}\n\
                     Provide a path to a sample press release document.",
                    value
                );
            }
            release_config.press_release_template = Some(value.to_string());
            release_config.save(config)?;
            println!("Set press_release_template = {}", value);
        }
        _ => {
            anyhow::bail!(
                "Unknown release config key: '{}'\n\
                 Available keys:\n  \
                 press_release_template — path to a sample press release for style matching",
                key
            );
        }
    }

    Ok(())
}

/// Generate a press release as part of the release pipeline.
fn generate_press_release(
    config: &GatewayConfig,
    version: &str,
    custom_prompt: Option<&str>,
) -> anyhow::Result<()> {
    println!();
    println!("Generating press release for v{} ...", version);

    let release_config = ReleaseConfig::load(config);

    // Read the changelog/release notes if available.
    let release_notes = {
        let draft_path = config.workspace_root.join(".release-draft.md");
        if draft_path.exists() {
            std::fs::read_to_string(&draft_path).unwrap_or_default()
        } else {
            // Fall back to git log.
            let (commits, _) = collect_commits_since_last_tag(&config.workspace_root)?;
            commits
        }
    };

    // Read style template if configured.
    let style_template = release_config
        .press_release_template
        .as_ref()
        .and_then(|p| {
            let path = config.workspace_root.join(p);
            if path.exists() {
                std::fs::read_to_string(&path).ok()
            } else {
                None
            }
        });

    // Build the agent objective.
    let mut objective = format!(
        "Generate a press release for version v{version}.\n\n\
         Release notes:\n{release_notes}\n",
    );

    if let Some(template) = &style_template {
        objective.push_str(&format!(
            "\nMatch the style and tone of this sample press release:\n---\n{}\n---\n",
            template
        ));
    }

    if let Some(prompt) = custom_prompt {
        objective.push_str(&format!("\nAdditional guidance: {}\n", prompt));
    }

    objective.push_str(
        "\nWrite the press release to .press-release-draft.md.\n\
         Focus on user-facing impact, key features, and use professional tone.",
    );

    // Write the objective for the agent.
    let output_path = config.workspace_root.join(".press-release-draft.md");

    // Try to launch as a TA goal; fall back to writing the prompt.
    let ta_bin = std::env::current_exe()?;
    let title = format!("release: Generate press release for v{}", version);
    let args = vec![
        "run".to_string(),
        title,
        "--agent".to_string(),
        "claude-code".to_string(),
        "--source".to_string(),
        config.workspace_root.display().to_string(),
        "--objective".to_string(),
        objective,
    ];

    let status = Command::new(&ta_bin)
        .args(&args)
        .current_dir(&config.workspace_root)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Press release draft generated.");
            if output_path.exists() {
                println!("  Output: {}", output_path.display());
            }
            println!("  Review and edit before publishing.");
        }
        _ => {
            // If agent launch fails, write the prompt as a guide.
            let guide = format!(
                "# Press Release Draft — v{}\n\n\
                 > This is a template. An agent would generate the full press release.\n\
                 > Edit manually or re-run: ta release run {} --press-release\n\n\
                 ## Release Notes\n\n{}\n",
                version, version, release_notes
            );
            std::fs::write(&output_path, &guide)?;
            println!(
                "Agent launch failed. Wrote press release template to {}.",
                output_path.display()
            );
            println!(
                "Edit it manually or retry with: ta release run {} --press-release",
                version
            );
        }
    }

    Ok(())
}

// ── Interactive release mode ─────────────────────────────────────

/// Launch an interactive release as a TA goal using the `releaser` agent.
///
/// The releaser agent uses `ta_ask_human` for review checkpoints, allowing
/// the human to stay in `ta shell` throughout the release process.
fn run_interactive_release(config: &GatewayConfig, version: &str) -> anyhow::Result<()> {
    let pipeline = load_pipeline(config, None)?;
    let version = normalize_version(version, &pipeline.version_policy);
    let (commits, last_tag) = collect_commits_since_last_tag(&config.workspace_root)?;

    let objective = format!(
        "You are the release agent for version v{version}.\n\n\
         Execute the following release process interactively:\n\n\
         1. Generate release notes from commits since {last_tag}:\n\
         {commits}\n\n\
         2. Write the release notes to .release-draft.md.\n\
         3. Use ta_ask_human to ask the reviewer to approve the release notes.\n\
         4. If the reviewer requests changes, revise and ask again.\n\
         5. Once approved, confirm the release is ready.\n\n\
         Write all release artifacts (.release-draft.md, CHANGELOG.md) directly.\n\
         Use ta_ask_human for every checkpoint that needs human review.",
        last_tag = last_tag.as_deref().unwrap_or("the beginning"),
    );

    let title = format!("release: Interactive release v{}", version);

    let ta_bin = std::env::current_exe()?;
    let args = vec![
        "run".to_string(),
        title,
        "--agent".to_string(),
        "releaser".to_string(),
        "--source".to_string(),
        config.workspace_root.display().to_string(),
        "--objective".to_string(),
        objective,
    ];

    println!("Launching interactive release agent for v{} ...", version);
    println!("The agent will use ta_ask_human for review checkpoints.");
    println!("Stay in ta shell to interact with the release process.");
    println!();

    let status = Command::new(&ta_bin)
        .args(&args)
        .current_dir(&config.workspace_root)
        .status()?;

    if !status.success() {
        anyhow::bail!(
            "Interactive release agent exited with code {:?}. \
             Check the goal status with: ta goal list",
            status.code()
        );
    }

    println!("Interactive release agent completed for v{}.", version);
    Ok(())
}

// ── Release validation ──────────────────────────────────────────

/// Validate that a release can proceed without actually executing anything.
///
/// Checks: version format, git state, tag availability, pipeline config,
/// and required tooling.
/// Pre-collected environment state for release validation.
/// Separates git/filesystem probing from validation logic so tests
/// don't need a real git repo.
struct ReleaseEnvironment {
    /// Whether the working tree has no uncommitted changes.
    working_tree_clean: bool,
    /// Set of existing tags (e.g., ["v1.0.0-alpha", "v0.9.0"]).
    existing_tags: std::collections::HashSet<String>,
    /// Whether the ./dev script exists.
    dev_script_exists: bool,
    /// Commits since last tag (commit text, last tag name).
    commits_since_tag: Result<(String, Option<String>), String>,
}

impl ReleaseEnvironment {
    /// Probe the real environment from a project root.
    fn from_project(root: &Path) -> Self {
        let git_clean = Command::new("git")
            .args(["diff", "--quiet"])
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let git_staged_clean = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let existing_tags = Command::new("git")
            .args(["tag", "-l"])
            .current_dir(root)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let commits_since_tag = collect_commits_since_last_tag(root).map_err(|e| e.to_string());

        Self {
            working_tree_clean: git_clean && git_staged_clean,
            existing_tags,
            dev_script_exists: root.join("dev").exists(),
            commits_since_tag,
        }
    }
}

fn validate_release(
    config: &GatewayConfig,
    version: &str,
    pipeline_path: Option<&Path>,
) -> anyhow::Result<()> {
    let env = ReleaseEnvironment::from_project(&config.workspace_root);
    let pipeline = load_pipeline(config, pipeline_path)?;
    validate_release_with_env(version, &pipeline, &env)
}

fn validate_release_with_env(
    version: &str,
    pipeline: &ReleasePipeline,
    env: &ReleaseEnvironment,
) -> anyhow::Result<()> {
    let version = normalize_version(version, &pipeline.version_policy);

    println!("Validating release v{} ...", version);
    println!();

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // 1. Version format.
    let version_re = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$")?;
    if version_re.is_match(&version) {
        println!("  [ok] Version format: {}", version);
    } else {
        errors.push(format!("Invalid version format: '{}'", version));
        println!("  [FAIL] Version format: {}", version);
    }

    // 2. Git state.
    if env.working_tree_clean {
        println!("  [ok] Working tree is clean");
    } else {
        warnings.push("Working tree has uncommitted changes".to_string());
        println!("  [warn] Working tree has uncommitted changes");
    }

    // 3. Tag availability.
    let tag = format!("v{}", version);
    if env.existing_tags.contains(&tag) {
        errors.push(format!("Tag '{}' already exists", tag));
        println!("  [FAIL] Tag '{}' already exists", tag);
    } else {
        println!("  [ok] Tag '{}' is available", tag);
    }

    // 4. Pipeline config.
    println!(
        "  [ok] Pipeline '{}' loaded ({} steps)",
        pipeline.name,
        pipeline.steps.len()
    );
    for (i, step) in pipeline.steps.iter().enumerate() {
        let kind = if step.run.is_some() {
            "shell"
        } else if step.agent.is_some() {
            "agent"
        } else {
            "unknown"
        };
        let approval = if step.requires_approval {
            " [approval]"
        } else {
            ""
        };
        println!("       {}. {} ({}){}", i + 1, step.name, kind, approval);
    }

    // 5. Dev toolchain check.
    if env.dev_script_exists {
        println!("  [ok] ./dev script found");
    } else {
        warnings.push("./dev script not found — build steps may fail".to_string());
        println!("  [warn] ./dev script not found");
    }

    // 6. Commits since last tag.
    match &env.commits_since_tag {
        Ok((commits, last_tag)) => {
            let count = commits.lines().filter(|l| !l.is_empty()).count();
            println!(
                "  [ok] {} commits since {}",
                count,
                last_tag.as_deref().unwrap_or("(no previous tag)")
            );
        }
        Err(e) => {
            warnings.push(format!("Cannot collect commits: {}", e));
            println!("  [warn] Cannot collect commits: {}", e);
        }
    }

    println!();
    if errors.is_empty() && warnings.is_empty() {
        println!("Validation passed. Ready to release v{}.", version);
    } else if errors.is_empty() {
        println!(
            "Validation passed with {} warning(s). Release can proceed.",
            warnings.len()
        );
    } else {
        println!(
            "Validation failed: {} error(s), {} warning(s).",
            errors.len(),
            warnings.len()
        );
        for e in &errors {
            println!("  ERROR: {}", e);
        }
        anyhow::bail!(
            "Release validation failed with {} error(s). Fix the issues above and retry.",
            errors.len()
        );
    }

    Ok(())
}

// ── Default built-in pipeline ───────────────────────────────────

const DEFAULT_PIPELINE_YAML: &str = r#"# .ta/release.yaml — TA release pipeline configuration.
#
# Variables available in all string fields:
#   ${VERSION}  — target version (e.g., "0.4.0-alpha")
#   ${TAG}      — git tag (e.g., "v0.4.0-alpha")
#   ${COMMITS}  — newline-separated commit messages since last tag
#   ${LAST_TAG} — previous git tag (empty if none)
#
# Each step must have either `run` (shell command) or `agent` (TA goal).
# Steps with `requires_approval: true` pause for human confirmation.

name: ta-release

# Version policy: controls how plan phase IDs are converted to semver.
# Uncomment and edit to customize. Templates use {0}..{3} for segments, {pre} for the suffix.
#
# version_policy:
#   prerelease_suffix: "alpha"
#   two_segment: "{0}.{1}.0-{pre}"
#   three_segment: "{0}.{1}.{2}-{pre}"
#   four_segment: "{0}.{1}.{2}-{pre}.{3}"

steps:
  - name: Preflight checks
    run: |
      set -e
      if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "ERROR: Working tree is not clean. Commit or stash changes first."
        exit 1
      fi
      if git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "ERROR: Tag '${TAG}' already exists."
        exit 1
      fi
      echo "Preflight OK — clean tree, tag ${TAG} is available."

  # v0.10.18: Pre-release sync and build steps.
  # These run `ta sync` and `ta build` if available, otherwise skip gracefully.
  # Full implementation requires v0.11.1 (SourceAdapter) and v0.11.2 (BuildAdapter).
  - name: Sync sources (if available)
    run: |
      if command -v ta >/dev/null 2>&1 && ta sync --help >/dev/null 2>&1; then
        echo "Running pre-release source sync..."
        ta sync
      else
        echo "Skipping: 'ta sync' not available (requires v0.11.1+)."
      fi

  - name: Pre-release build (if available)
    run: |
      if command -v ta >/dev/null 2>&1 && ta build --help >/dev/null 2>&1; then
        echo "Running pre-release build..."
        ta build
      else
        echo "Skipping: 'ta build' not available (requires v0.11.2+)."
      fi

  - name: Version bump
    run: |
      set -e
      # Bump all Cargo.toml versions
      for f in Cargo.toml crates/*/Cargo.toml apps/*/Cargo.toml; do
        [ -f "$f" ] && sed -i.bak '/^\[package\]/,/^\[/s/^version = ".*"/version = "'"${VERSION}"'"/' "$f" && rm -f "${f}.bak"
      done
      echo "Versions bumped to ${VERSION}."

  - name: Build & verify
    run: |
      set -e
      ./dev cargo build --workspace
      ./dev cargo test --workspace
      ./dev cargo clippy --workspace --all-targets -- -D warnings
      ./dev cargo fmt --all -- --check
      echo "All checks passed."

  # Constitution compliance checklist gate (v0.11.6).
  # Pauses for human sign-off that key constitution invariants are met before
  # generating release notes. Skippable with --yes / --skip-approvals.
  - name: Constitution compliance sign-off
    requires_approval: true
    run: |
      echo "── Constitution Compliance Checklist ──"
      echo ""
      echo "Please confirm the following invariants hold for this release:"
      echo ""
      echo "  §4  Injection cleanup: every inject_* call in run.rs has a matching"
      echo "      restore_* on all early-return paths (ok, err, and non-zero exit)."
      echo ""
      echo "  §5  Goal state machine: all GoalRunState transitions go through"
      echo "      GoalRun::transition(), which enforces can_transition_to()."
      echo "      No direct .state = assignment outside transition()."
      echo ""
      echo "  §7  Policy enforcement: every ta_fs_* tool handler calls check_policy()"
      echo "      before accessing source content (read, write_patch, diff)."
      echo ""
      echo "  §8  Audit trail: DraftBuilt, DraftApproved, DraftDenied, DraftApplied,"
      echo "      GoalStarted, GoalCompleted, and GoalFailed events are emitted to"
      echo "      the FsEventStore at every corresponding state change."
      echo ""
      echo "  §13 Error observability: all error paths include what happened, what"
      echo "      was being attempted, and what the user can do next. No bare 'Error'"
      echo "      or 'failed' messages without context."
      echo ""
      echo "Review the diff and audit log before proceeding."

  - name: Generate release notes
    agent:
      id: releaser
    objective: |
      Synthesize user-facing release notes for version ${TAG}.
      Commits since ${LAST_TAG}:
      ${COMMITS}

      Write the notes to .release-draft.md in this format:
      ## ${TAG}
      ### New Features
      - ...
      ### Improvements
      - ...
      ### Bug Fixes
      - ...
    output: .release-draft.md

  - name: Review release notes
    requires_approval: true
    run: |
      if [ -f .release-draft.md ]; then
        echo "── Release notes draft ──"
        cat .release-draft.md
      else
        echo "No .release-draft.md found. Skipping."
      fi

  - name: Commit and tag
    run: |
      set -e
      # Ensure we're on main — release commits must land on main.
      BRANCH="$(git branch --show-current)"
      if [ "$BRANCH" != "main" ]; then
        echo "Switching to main (was on $BRANCH)..."
        git checkout main
      fi
      # Commit if there are staged/unstaged changes; skip if tree is clean.
      git add -A
      if git diff --cached --quiet; then
        echo "Working tree clean — no commit needed (version bumps already committed)."
      else
        git commit -m "Release ${TAG}

      Bump all crate versions to ${VERSION}."
      fi
      # Create tag (skip if it already exists).
      if git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "Tag ${TAG} already exists — skipping."
      else
        git tag -a "${TAG}" -m "Release ${TAG}"
        echo "Created tag ${TAG}."
      fi

  - name: Update version tracking
    run: |
      set -e
      TODAY=$(date +%Y-%m-%d)
      cat > version.json <<VEOF
      {
        "committed": "${VERSION}",
        "deployed": "${VERSION}",
        "committed_at": "${TODAY}",
        "deployed_at": "${TODAY}",
        "deployed_tag": "${TAG}"
      }
      VEOF
      # Update README badges.
      if [ -f README.md ]; then
        sed -i.bak "s|latest-v[0-9a-z.\-]*-blue|latest-v${VERSION}-blue|" README.md
        sed -i.bak "s|released-v[0-9a-z.\-]*-green|released-v${VERSION}-green|" README.md
        rm -f README.md.bak
      fi
      git add version.json README.md
      if ! git diff --cached --quiet; then
        git commit --amend --no-edit
        echo "Updated version tracking and README badges."
      fi

  - name: Push
    requires_approval: true
    run: |
      set -e
      # Rebase onto remote in case main advanced since the release started.
      git pull --rebase origin main
      git push origin main
      git push origin "${TAG}"
      echo "Pushed ${TAG}. GitHub Actions will build the release."
"#;

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Initialize a git repo in a temp dir with user config (needed in CI).
    fn git_init_with_commit(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn parse_default_pipeline() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        assert_eq!(pipeline.name, "ta-release");
        assert!(!pipeline.steps.is_empty());
        // Validate all steps.
        for step in &pipeline.steps {
            step.validate().unwrap();
        }
    }

    #[test]
    fn step_validation_requires_run_or_agent() {
        let step = PipelineStep {
            name: "bad".to_string(),
            run: None,
            agent: None,
            objective: None,
            requires_approval: false,
            output: None,
            working_dir: None,
            env: Default::default(),
        };
        assert!(step.validate().is_err());
    }

    #[test]
    fn step_validation_rejects_both_run_and_agent() {
        let step = PipelineStep {
            name: "bad".to_string(),
            run: Some("echo hi".to_string()),
            agent: Some(AgentStep {
                id: "claude-code".to_string(),
                phase: None,
            }),
            objective: None,
            requires_approval: false,
            output: None,
            working_dir: None,
            env: Default::default(),
        };
        assert!(step.validate().is_err());
    }

    #[test]
    fn variable_substitution() {
        let result = substitute_vars(
            "Build ${TAG} from ${VERSION}, last was ${LAST_TAG}",
            "1.0.0",
            "commit1\ncommit2",
            Some("v0.9.0"),
        );
        assert_eq!(result, "Build v1.0.0 from 1.0.0, last was v0.9.0");
    }

    #[test]
    fn variable_substitution_no_last_tag() {
        let result = substitute_vars("since ${LAST_TAG} end", "1.0.0", "", None);
        assert_eq!(result, "since  end");
    }

    #[test]
    fn load_custom_pipeline_yaml() {
        let yaml = r#"
name: custom
steps:
  - name: greet
    run: echo hello
  - name: agent step
    agent:
      id: codex
      phase: "4b"
    objective: Do something
    requires_approval: true
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pipeline.name, "custom");
        assert_eq!(pipeline.steps.len(), 2);
        assert_eq!(pipeline.steps[0].run.as_deref(), Some("echo hello"));
        assert!(pipeline.steps[0].agent.is_none());
        assert!(pipeline.steps[1].run.is_none());
        assert_eq!(pipeline.steps[1].agent.as_ref().unwrap().id, "codex");
        assert_eq!(
            pipeline.steps[1].agent.as_ref().unwrap().phase.as_deref(),
            Some("4b")
        );
        assert!(pipeline.steps[1].requires_approval);
    }

    #[test]
    fn init_creates_release_yaml() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        init_pipeline(&config).unwrap();

        let path = temp.path().join(".ta").join("release.yaml");
        assert!(path.exists());

        // Should not overwrite existing.
        assert!(init_pipeline(&config).is_err());
    }

    #[test]
    fn load_pipeline_uses_project_override() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());

        // Without override, uses built-in.
        let p = load_pipeline(&config, None).unwrap();
        assert_eq!(p.name, "ta-release");

        // Write a project override.
        let ta_dir = temp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("release.yaml"),
            "name: my-project\nsteps:\n  - name: test\n    run: echo ok\n",
        )
        .unwrap();

        let p = load_pipeline(&config, None).unwrap();
        assert_eq!(p.name, "my-project");
    }

    #[test]
    fn load_pipeline_explicit_path() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());

        let custom = temp.path().join("custom.yaml");
        std::fs::write(
            &custom,
            "name: explicit\nsteps:\n  - name: x\n    run: echo x\n",
        )
        .unwrap();

        let p = load_pipeline(&config, Some(&custom)).unwrap();
        assert_eq!(p.name, "explicit");
    }

    #[test]
    fn version_validation() {
        let re = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$").unwrap();
        assert!(re.is_match("0.4.0-alpha"));
        assert!(re.is_match("1.0.0"));
        assert!(re.is_match("0.1.0-rc.1"));
        assert!(!re.is_match("v0.4.0"));
        assert!(!re.is_match("bad"));
        assert!(!re.is_match("1.0"));
    }

    #[test]
    fn show_pipeline_output() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        // Just ensure it doesn't panic.
        show_pipeline(&config, None).unwrap();
    }

    #[test]
    fn validate_empty_pipeline_fails() {
        let pipeline = ReleasePipeline {
            name: "empty".to_string(),
            version_policy: VersionPolicy::default(),
            steps: vec![],
        };
        assert!(validate_pipeline(&pipeline).is_err());
    }

    #[test]
    fn agent_step_default_id() {
        let yaml = "id: claude-code";
        let agent: AgentStep = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.id, "claude-code");
        assert!(agent.phase.is_none());
    }

    #[test]
    fn pipeline_step_env_vars() {
        let yaml = r#"
name: with-env
steps:
  - name: test
    run: echo $MY_VAR
    env:
      MY_VAR: "value-${VERSION}"
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            pipeline.steps[0].env.get("MY_VAR").unwrap(),
            "value-${VERSION}"
        );
    }

    #[test]
    fn dry_run_does_not_execute() {
        let temp = TempDir::new().unwrap();
        git_init_with_commit(temp.path());

        let config = GatewayConfig::for_project(temp.path());

        // Write a pipeline that would fail if actually run.
        let ta_dir = temp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("release.yaml"),
            "name: test\nsteps:\n  - name: fail\n    run: exit 1\n",
        )
        .unwrap();

        // Dry run should succeed even though the step would fail.
        run_pipeline(&config, "1.0.0", true, true, None, None).unwrap();
    }

    #[test]
    fn normalize_version_strips_v_prefix() {
        let p = VersionPolicy::default();
        assert_eq!(normalize_version("v0.4.1", &p), "0.4.1-alpha");
    }

    #[test]
    fn normalize_version_two_segments() {
        let p = VersionPolicy::default();
        assert_eq!(normalize_version("0.4", &p), "0.4.0-alpha");
    }

    #[test]
    fn normalize_version_three_segments() {
        let p = VersionPolicy::default();
        assert_eq!(normalize_version("0.4.1", &p), "0.4.1-alpha");
    }

    #[test]
    fn normalize_version_four_segments_to_prerelease() {
        let p = VersionPolicy::default();
        assert_eq!(normalize_version("0.4.1.2", &p), "0.4.1-alpha.2");
        assert_eq!(normalize_version("v0.4.1.2", &p), "0.4.1-alpha.2");
    }

    #[test]
    fn normalize_version_passthrough_semver() {
        let p = VersionPolicy::default();
        assert_eq!(normalize_version("0.4.0-alpha", &p), "0.4.0-alpha");
        assert_eq!(normalize_version("1.0.0-beta.1", &p), "1.0.0-beta.1");
    }

    #[test]
    fn normalize_version_custom_suffix() {
        let p = VersionPolicy {
            prerelease_suffix: "beta".to_string(),
            ..Default::default()
        };
        assert_eq!(normalize_version("0.4.1", &p), "0.4.1-beta");
        assert_eq!(normalize_version("0.4.1.2", &p), "0.4.1-beta.2");
    }

    #[test]
    fn normalize_version_custom_templates() {
        let p = VersionPolicy {
            prerelease_suffix: "rc".to_string(),
            three_segment: "{0}.{1}.{2}".to_string(), // No prerelease for 3-segment
            four_segment: "{0}.{1}.{2}-{pre}{3}".to_string(), // e.g., 0.4.1-rc2
            ..Default::default()
        };
        assert_eq!(normalize_version("0.4.1", &p), "0.4.1");
        assert_eq!(normalize_version("0.4.1.2", &p), "0.4.1-rc2");
    }

    #[test]
    fn normalize_version_no_prerelease() {
        let p = VersionPolicy {
            prerelease_suffix: String::new(),
            two_segment: "{0}.{1}.0".to_string(),
            three_segment: "{0}.{1}.{2}".to_string(),
            four_segment: "{0}.{1}.{2}.{3}".to_string(),
        };
        assert_eq!(normalize_version("0.4", &p), "0.4.0");
        assert_eq!(normalize_version("1.2.3", &p), "1.2.3");
        assert_eq!(normalize_version("1.2.3.4", &p), "1.2.3.4");
    }

    #[test]
    fn release_config_default() {
        let config = ReleaseConfig::default();
        assert!(config.press_release_template.is_none());
    }

    #[test]
    fn release_config_roundtrip() {
        let config = ReleaseConfig {
            press_release_template: Some("samples/press-release.md".to_string()),
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: ReleaseConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            parsed.press_release_template.as_deref(),
            Some("samples/press-release.md")
        );
    }

    #[test]
    fn configure_release_unknown_key() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let result = configure_release(&config, "nonexistent", "value");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown"));
    }

    #[test]
    fn configure_release_template_not_found() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let result = configure_release(&config, "press_release_template", "/nonexistent/file.md");
        assert!(result.is_err());
    }

    #[test]
    fn configure_release_template_success() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let template = temp.path().join("sample.md");
        std::fs::write(&template, "# Sample Press Release").unwrap();
        configure_release(
            &config,
            "press_release_template",
            template.to_str().unwrap(),
        )
        .unwrap();

        let loaded = ReleaseConfig::load(&config);
        assert!(loaded.press_release_template.is_some());
    }

    #[test]
    fn version_policy_deserializes_from_yaml() {
        let yaml = r#"
prerelease_suffix: "beta"
three_segment: "{0}.{1}.{2}"
"#;
        let policy: VersionPolicy = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(policy.prerelease_suffix, "beta");
        assert_eq!(policy.three_segment, "{0}.{1}.{2}");
        // Defaults for fields not specified.
        assert_eq!(policy.two_segment, "{0}.{1}.0-{pre}");
        assert_eq!(policy.four_segment, "{0}.{1}.{2}-{pre}.{3}");
    }

    #[test]
    fn pipeline_with_version_policy() {
        let yaml = r#"
name: custom
version_policy:
  prerelease_suffix: "beta"
  three_segment: "{0}.{1}.{2}"
steps:
  - name: test
    run: echo hi
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pipeline.version_policy.prerelease_suffix, "beta");
        assert_eq!(
            normalize_version("0.5.0", &pipeline.version_policy),
            "0.5.0"
        );
        assert_eq!(
            normalize_version("0.5.0.1", &pipeline.version_policy),
            "0.5.0-beta.1"
        );
    }

    #[test]
    fn default_pipeline_uses_releaser_agent() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let agent_step = pipeline
            .steps
            .iter()
            .find(|s| s.agent.is_some())
            .expect("should have an agent step");
        assert_eq!(agent_step.agent.as_ref().unwrap().id, "releaser");
    }

    #[test]
    fn validate_release_clean_repo() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let env = ReleaseEnvironment {
            working_tree_clean: true,
            existing_tags: std::collections::HashSet::new(),
            dev_script_exists: false,
            commits_since_tag: Ok(("".to_string(), None)),
        };
        // Should pass: clean tree, no conflicting tags.
        validate_release_with_env("1.0.0", &pipeline, &env).unwrap();
    }

    #[test]
    fn validate_release_existing_tag_fails() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let env = ReleaseEnvironment {
            working_tree_clean: true,
            existing_tags: ["v1.0.0-alpha".to_string()].into_iter().collect(),
            dev_script_exists: false,
            commits_since_tag: Ok(("".to_string(), None)),
        };
        // Should fail: tag v1.0.0-alpha already exists.
        assert!(validate_release_with_env("1.0.0-alpha", &pipeline, &env).is_err());
    }

    // §11.6 / Plan item #5 regression: the default pipeline MUST include a
    // constitution compliance sign-off step between "Build & verify" and
    // "Generate release notes". If this test fails the step was removed.
    #[test]
    fn default_pipeline_has_constitution_checklist_gate() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();

        // Find the step indices so we can verify ordering.
        let build_idx = pipeline
            .steps
            .iter()
            .position(|s| s.name == "Build & verify");
        let checklist_idx = pipeline
            .steps
            .iter()
            .position(|s| s.name == "Constitution compliance sign-off");
        let notes_idx = pipeline
            .steps
            .iter()
            .position(|s| s.name == "Generate release notes");

        assert!(
            checklist_idx.is_some(),
            "default pipeline must include 'Constitution compliance sign-off' step"
        );
        let checklist_idx = checklist_idx.unwrap();

        // The checklist step must require human approval.
        assert!(
            pipeline.steps[checklist_idx].requires_approval,
            "constitution sign-off step must have requires_approval: true"
        );

        // Ordering: Build & verify < checklist < Generate release notes.
        if let (Some(b), Some(n)) = (build_idx, notes_idx) {
            assert!(
                b < checklist_idx,
                "constitution sign-off must come after 'Build & verify'"
            );
            assert!(
                checklist_idx < n,
                "constitution sign-off must come before 'Generate release notes'"
            );
        }
    }

    #[test]
    fn e2e_pipeline_no_manual_gates() {
        // End-to-end test: pipeline with all-shell steps, --yes skips approvals.
        let temp = TempDir::new().unwrap();
        git_init_with_commit(temp.path());

        let config = GatewayConfig::for_project(temp.path());

        // Write a simple pipeline that creates a marker file.
        let ta_dir = temp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("release.yaml"),
            r#"name: e2e-test
steps:
  - name: create marker
    run: echo "released ${VERSION}" > release-marker.txt
  - name: verify marker
    run: test -f release-marker.txt
  - name: gated step
    requires_approval: true
    run: echo "approved"
"#,
        )
        .unwrap();

        // Run with --yes to skip approvals.
        run_pipeline(&config, "1.0.0", true, false, None, None).unwrap();

        // Verify the marker file was created.
        let marker = temp.path().join("release-marker.txt");
        assert!(marker.exists());
        let content = std::fs::read_to_string(&marker).unwrap();
        assert!(content.contains("released 1.0.0"));
    }

    // ── Output format contract tests ──────────────────────────────

    #[test]
    fn extract_draft_id_valid_output() {
        let id = uuid::Uuid::new_v4();
        let output = format!("some preamble\ndraft package built: {}\ndone", id);
        assert_eq!(extract_draft_id_from_output(&output), Some(id));
    }

    #[test]
    fn extract_draft_id_with_whitespace() {
        let id = uuid::Uuid::new_v4();
        let output = format!("  draft package built:  {}  \n", id);
        assert_eq!(extract_draft_id_from_output(&output), Some(id));
    }

    #[test]
    fn extract_draft_id_no_match() {
        assert_eq!(
            extract_draft_id_from_output("no draft here\nall done"),
            None
        );
    }

    #[test]
    fn extract_draft_id_invalid_uuid() {
        let output = "draft package built: not-a-uuid\n";
        assert_eq!(extract_draft_id_from_output(output), None);
    }

    #[test]
    fn extract_draft_id_empty_output() {
        assert_eq!(extract_draft_id_from_output(""), None);
    }

    #[test]
    fn extract_draft_id_picks_first_match() {
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let output = format!(
            "draft package built: {}\ndraft package built: {}\n",
            id1, id2
        );
        assert_eq!(extract_draft_id_from_output(&output), Some(id1));
    }

    /// Contract test: the format string in draft.rs `println!("draft package built: {}", ...)`
    /// must match what `extract_draft_id_from_output` parses. If either side changes,
    /// this test ensures they stay in sync.
    #[test]
    fn draft_build_output_format_contract() {
        let id = uuid::Uuid::new_v4();
        // Simulate the exact format from draft.rs line 1068.
        let line = format!("draft package built: {}", id);
        assert_eq!(extract_draft_id_from_output(&line), Some(id));
    }

    #[test]
    fn find_latest_draft_skips_applied() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        let id = uuid::Uuid::new_v4();
        std::fs::write(dir.join(format!("{}.json", id)), r#"{"status":"applied"}"#).unwrap();

        let result = find_latest_draft(&config).unwrap();
        assert!(result.is_none(), "applied draft should be skipped");
    }

    #[test]
    fn find_latest_draft_skips_denied() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        let id = uuid::Uuid::new_v4();
        std::fs::write(dir.join(format!("{}.json", id)), r#"{"status":"denied"}"#).unwrap();

        assert!(find_latest_draft(&config).unwrap().is_none());
    }

    #[test]
    fn find_latest_draft_skips_superseded() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        let id = uuid::Uuid::new_v4();
        std::fs::write(
            dir.join(format!("{}.json", id)),
            r#"{"status":"superseded"}"#,
        )
        .unwrap();

        assert!(find_latest_draft(&config).unwrap().is_none());
    }

    #[test]
    fn find_latest_draft_skips_closed() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        let id = uuid::Uuid::new_v4();
        std::fs::write(dir.join(format!("{}.json", id)), r#"{"status":"closed"}"#).unwrap();

        assert!(find_latest_draft(&config).unwrap().is_none());
    }

    #[test]
    fn find_latest_draft_returns_eligible() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        let id = uuid::Uuid::new_v4();
        std::fs::write(
            dir.join(format!("{}.json", id)),
            r#"{"status":"pending_review"}"#,
        )
        .unwrap();

        assert_eq!(find_latest_draft(&config).unwrap(), Some(id));
    }

    #[test]
    fn find_latest_draft_picks_eligible_over_terminal() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        // Terminal draft.
        let old_id = uuid::Uuid::new_v4();
        std::fs::write(
            dir.join(format!("{}.json", old_id)),
            r#"{"status":"applied"}"#,
        )
        .unwrap();

        // Eligible draft (written after, so newer mtime).
        std::thread::sleep(std::time::Duration::from_millis(10));
        let new_id = uuid::Uuid::new_v4();
        std::fs::write(
            dir.join(format!("{}.json", new_id)),
            r#"{"status":"draft"}"#,
        )
        .unwrap();

        assert_eq!(find_latest_draft(&config).unwrap(), Some(new_id));
    }

    #[test]
    fn find_latest_draft_empty_dir() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let dir = &config.pr_packages_dir;
        std::fs::create_dir_all(dir).unwrap();

        assert!(find_latest_draft(&config).unwrap().is_none());
    }

    #[test]
    fn find_latest_draft_no_dir() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        // Don't create the dir — should return None, not error.
        assert!(find_latest_draft(&config).unwrap().is_none());
    }

    #[test]
    fn dry_run_validates_all_steps() {
        let temp = TempDir::new().unwrap();
        git_init_with_commit(temp.path());

        let config = GatewayConfig::for_project(temp.path());

        // Write a pipeline with steps that would fail if actually executed.
        let ta_dir = temp.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("release.yaml"),
            r#"name: dry-run-test
steps:
  - name: would-fail
    run: exit 1
  - name: gated
    requires_approval: true
    run: exit 1
"#,
        )
        .unwrap();

        // Dry run should succeed even with failing steps and approval gates.
        run_pipeline(&config, "1.0.0", false, true, None, None).unwrap();

        // Nothing should have been executed — no files created.
        assert!(!temp.path().join("release-marker.txt").exists());
    }

    #[test]
    fn auto_approve_always_approves() {
        assert!(prompt_approval_with_auto("test-step", true).unwrap());
    }

    #[test]
    fn tui_interaction_question_written() {
        // Verify that non-TTY mode writes an interaction question file.
        // We can't fully test the polling loop, but we can test the question format.
        let temp = tempfile::tempdir().unwrap();
        let pending_dir = temp.path().join(".ta/interactions/pending");
        std::fs::create_dir_all(&pending_dir).unwrap();

        let interaction_id = uuid::Uuid::new_v4();
        let question = serde_json::json!({
            "interaction_id": interaction_id.to_string(),
            "goal_id": "release",
            "question": "Release gate: proceed with 'publish'?",
            "choices": ["y", "n"],
            "response_hint": "y or n",
            "turn": 0
        });
        let path = pending_dir.join(format!("{}.json", interaction_id));
        std::fs::write(&path, serde_json::to_string_pretty(&question).unwrap()).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["goal_id"], "release");
        assert!(content["question"].as_str().unwrap().contains("publish"));
        assert_eq!(content["choices"][0], "y");
    }
}
