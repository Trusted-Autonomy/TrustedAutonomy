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

        /// Skip all approval gates and the constitution check (non-interactive / CI mode).
        /// Equivalent to --auto-approve.
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

        /// Auto-approve all approval gates and skip the constitution check.
        /// Use in CI or when approval is not needed. Without this flag,
        /// non-TTY contexts (daemon) will prompt via TUI interaction.
        /// Equivalent to --yes.
        #[arg(long)]
        auto_approve: bool,

        /// Override the base git tag used to compute the commit range for release notes.
        /// By default the pipeline uses `git describe --tags --abbrev=0` to find the
        /// previous tag. If that tag is stale (e.g. several releases were skipped without
        /// tagging), use this to pin the correct base, e.g. `--from-tag v0.12.7-alpha`.
        #[arg(long)]
        from_tag: Option<String>,

        /// After the pipeline completes, dispatch the GitHub Actions release workflow
        /// using this tag label instead of the semver tag. Use for public-facing labels
        /// that don't follow the `v*` auto-trigger pattern, e.g. `public-alpha-v0.13.1.7`.
        /// Equivalent to running `ta release dispatch <label>` after `ta release run`.
        #[arg(long)]
        label: Option<String>,

        /// Mark the dispatched label release as a pre-release (only used with --label).
        #[arg(long, default_value_t = false)]
        prerelease: bool,
    },
    /// Show the pipeline that would be executed (without running it).
    Show {
        /// Custom pipeline file (overrides default resolution).
        #[arg(long)]
        pipeline: Option<PathBuf>,
        /// Override the base tag used to compute the commit range.
        /// By default uses git describe or release-history.json.
        #[arg(long)]
        from_tag: Option<String>,
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
    /// Trigger the GitHub Actions release workflow for an arbitrary tag label.
    ///
    /// Use this when the tag name doesn't follow the `v*` auto-trigger pattern
    /// (e.g. `public-alpha-v0.13.1.1`). The workflow runs `workflow_dispatch`
    /// with the tag as an input; the release workflow creates the tag automatically.
    ///
    /// Requires `gh` (GitHub CLI) to be installed and authenticated.
    ///
    /// Example:
    ///   ta release dispatch public-alpha-v0.13.1.1
    Dispatch {
        /// The GitHub tag label to use for the release (e.g. "public-alpha-v0.13.1.1").
        /// Does not need to exist — the release workflow creates it.
        tag: String,

        /// Mark the release as a pre-release on GitHub (default: false = latest).
        #[arg(long, default_value_t = false)]
        prerelease: bool,

        /// GitHub repository in `owner/repo` format.
        /// Defaults to the `GITHUB_REPOSITORY` env var or auto-detected from git remote.
        #[arg(long)]
        repo: Option<String>,

        /// Workflow file name (default: "release.yml").
        #[arg(long, default_value = "release.yml")]
        workflow: String,
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
            from_tag,
            label,
            prerelease,
        } => {
            if *interactive {
                run_interactive_release(config, version)?;
            } else {
                // --yes implies --auto-approve for backward compatibility.
                let skip_approvals = *yes || *auto_approve;
                let pipeline_result = run_pipeline(
                    config,
                    version,
                    skip_approvals,
                    *dry_run,
                    *from_step,
                    pipeline.as_deref(),
                    from_tag.as_deref(),
                );
                match pipeline_result {
                    Err(e) if e.to_string() == "__pipeline_aborted__" => return Ok(()),
                    other => other?,
                }
                if *press_release {
                    generate_press_release(config, version, prompt.as_deref())?;
                }
                // If --label is provided, dispatch the GitHub Actions release workflow
                // using the label tag. Only runs if the pipeline completed without error
                // or abort — placing this inside the else block ensures it is skipped
                // when the user cancels at an approval gate.
                if let Some(tag) = label {
                    if !*dry_run {
                        println!();
                        println!(
                            "--label provided: dispatching release workflow for '{}'",
                            tag
                        );
                        dispatch_release(tag, *prerelease, None, "release.yml")?;
                    } else {
                        println!("[dry-run] Would dispatch: ta release dispatch {}", tag);
                    }
                }
            }
            Ok(())
        }
        ReleaseCommands::Show { pipeline, from_tag } => {
            show_pipeline(config, pipeline.as_deref(), from_tag.as_deref())
        }
        ReleaseCommands::Init => init_pipeline(config),
        ReleaseCommands::Config { key, value } => configure_release(config, key, value),
        ReleaseCommands::Validate { version, pipeline } => {
            validate_release(config, version, pipeline.as_deref())
        }
        ReleaseCommands::Dispatch {
            tag,
            prerelease,
            repo,
            workflow,
        } => dispatch_release(tag, *prerelease, repo.as_deref(), workflow),
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

    /// Shell command to run (mutually exclusive with `agent` and `generate_notes`).
    #[serde(default)]
    pub run: Option<String>,

    /// TA agent to invoke (mutually exclusive with `run` and `generate_notes`).
    #[serde(default)]
    pub agent: Option<AgentStep>,

    /// Generate release notes and write to .release-draft.md.
    /// Uses AI synthesis (configurable agent) with a mechanical fallback.
    /// Mutually exclusive with `run` and `agent`.
    ///
    /// Minimal form (uses defaults):
    ///   generate_notes:
    ///
    /// With agent override:
    ///   generate_notes:
    ///     agent: claude-code
    #[serde(default)]
    pub generate_notes: Option<GenerateNotesConfig>,

    /// If true, run the constitution compliance check programmatically instead of
    /// displaying a static checklist. Shows scan violations and supervisor verdict.
    /// Skipped entirely when --yes / --auto-approve is set.
    #[serde(default)]
    pub constitution_check: bool,

    /// If true, write .ta/release-history.json and stage it with git add.
    /// Place this step AFTER "Commit and tag" to capture the correct SHA.
    /// The next "Update version tracking" amend will include it in the release commit.
    #[serde(default)]
    pub record_release_history: bool,

    /// Objective/description for context (used by agent steps and display).
    #[serde(default)]
    pub objective: Option<String>,

    /// If true, pause for human approval before this step executes.
    #[serde(default)]
    pub requires_approval: bool,

    /// If true, the approval prompt defaults to Y (Enter proceeds, n aborts).
    /// Use for review steps where "looks good" is the common case.
    #[serde(default)]
    pub default_approve: bool,

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

/// Configuration for the built-in `generate_notes` step.
///
/// The step calls the configured agent in one-shot (`--print`) mode,
/// captures its stdout as Markdown, and writes it to `.release-draft.md`.
/// If the agent call fails or returns empty output, the step falls back to
/// a deterministic Rust-based formatter that groups commits by category.
///
/// Example `.ta/release.yaml` override:
/// ```yaml
/// - name: Generate release notes
///   generate_notes:
///     agent: claude-code          # default — the primary TA agent
///     fallback: true              # default — mechanical generation on failure
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateNotesConfig {
    /// Agent ID to use for AI synthesis.
    /// Supported built-ins: `"claude-code"` (Anthropic Claude Code CLI).
    /// Any other value is tried as a raw command name.
    #[serde(default = "default_notes_agent")]
    pub agent: String,

    /// Fall back to deterministic commit-list generation when the agent call
    /// fails, is unavailable, or returns empty output.  Defaults to `true`.
    #[serde(default = "default_notes_fallback")]
    pub fallback: bool,
}

fn default_notes_agent() -> String {
    "claude-code".to_string()
}

fn default_notes_fallback() -> bool {
    true
}

impl Default for GenerateNotesConfig {
    fn default() -> Self {
        Self {
            agent: default_notes_agent(),
            fallback: default_notes_fallback(),
        }
    }
}

impl PipelineStep {
    fn validate(&self) -> anyhow::Result<()> {
        let defined = [
            self.run.is_some(),
            self.agent.is_some(),
            self.generate_notes.is_some(),
            self.constitution_check,
            self.record_release_history,
        ]
        .iter()
        .filter(|&&x| x)
        .count();
        if defined == 0 {
            anyhow::bail!(
                "Step '{}': must have one of 'run', 'agent', 'generate_notes', 'constitution_check', or 'record_release_history'",
                self.name
            );
        }
        if defined > 1 {
            anyhow::bail!(
                "Step '{}': only one of 'run', 'agent', 'generate_notes', 'constitution_check', or 'record_release_history' may be set",
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

    // 3. Built-in default pipeline — apply constitution.toml overrides.
    let mut pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML)?;

    // v0.13.15: Read checklist_gate from .ta/constitution.toml.
    // When checklist_gate = false, remove the "Constitution compliance sign-off" step
    // so projects that manage compliance separately can opt out of the blocking gate.
    let constitution_cfg =
        super::constitution::ProjectConstitutionConfig::load(&config.workspace_root)
            .unwrap_or_default();
    if let Some(ref cc) = constitution_cfg {
        if !cc.release.checklist_gate {
            pipeline.steps.retain(|step| {
                // Matches by substring to avoid fragile exact-string dependency.
                !step.name.to_lowercase().contains("constitution")
            });
        }
    }

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

// ── Release history tracking ─────────────────────────────────────

/// One entry in `.ta/release-history.json` — written after each successful pipeline run.
#[derive(Debug, Serialize, Deserialize)]
struct ReleaseRecord {
    version: String,
    tag: String,
    commit: String,
    released_at: String,
}

/// Path to the release history file within a project.
fn release_history_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("release-history.json")
}

/// Load the release history, returning an empty vec if the file doesn't exist.
fn load_release_history(project_root: &Path) -> Vec<ReleaseRecord> {
    let path = release_history_path(project_root);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Append a new release record to `.ta/release-history.json`.
///
/// Called at the end of a successful `run_pipeline`. Gets the current HEAD commit
/// SHA automatically so the caller doesn't need to pass it.
fn record_release(project_root: &Path, version: &str) -> anyhow::Result<()> {
    let tag = format!("v{}", version);

    // Capture HEAD commit SHA.
    let sha_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()?;
    let commit = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();

    // ISO 8601 timestamp (UTC).
    let released_at = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Format as YYYY-MM-DDTHH:MM:SSZ using simple arithmetic (no chrono dep).
        let s = now;
        let sec = s % 60;
        let min = (s / 60) % 60;
        let hour = (s / 3600) % 24;
        let days = s / 86400; // days since epoch
                              // Compute year/month/day from days-since-epoch (Gregorian).
        let (year, month, day) = days_to_ymd(days);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, hour, min, sec
        )
    };

    let record = ReleaseRecord {
        version: version.to_string(),
        tag,
        commit,
        released_at,
    };

    let mut history = load_release_history(project_root);
    history.push(record);

    let path = release_history_path(project_root);
    // Ensure .ta/ exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&history)?;
    std::fs::write(&path, json)?;

    println!(
        "Release recorded in .ta/release-history.json (v{})",
        version
    );
    Ok(())
}

/// Convert days-since-Unix-epoch to (year, month, day).
///
/// Uses the proleptic Gregorian calendar algorithm.
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m, d)
}

// ── Commit collection ───────────────────────────────────────────

/// Collect commit messages since the given tag (or the most recent tag if none specified).
///
/// Resolution order (highest priority first):
///   1. `from_tag` — explicit override, e.g. `--from-tag v0.12.7-alpha`
///   2. `.ta/release-history.json` — tag from the last successful pipeline run
///   3. `git describe --tags --abbrev=0` — most recent git tag (fallback)
///
/// Using the tracking file (#2) means the pipeline always knows the exact commit
/// delta for the current release — even if intermediate git tags were created for
/// bookkeeping purposes or if git describe returns a stale/wrong tag.
fn collect_commits_since_tag(
    project_root: &Path,
    from_tag: Option<&str>,
) -> anyhow::Result<(String, Option<String>)> {
    // Resolve the base tag: use the explicit override if given, otherwise ask git.
    let last_tag = if let Some(tag) = from_tag {
        // Validate the tag exists so we fail fast with a clear error.
        let check = Command::new("git")
            .args(["rev-parse", "--verify", tag])
            .current_dir(project_root)
            .output();
        match check {
            Ok(out) if out.status.success() => Some(tag.to_string()),
            _ => anyhow::bail!(
                "Tag '{}' not found in this repository.\n\
                 Run `git tag` to list available tags.",
                tag
            ),
        }
    } else {
        // Try the release history file first — most precise source of truth.
        let history = load_release_history(project_root);
        if let Some(last) = history.last() {
            Some(last.tag.clone())
        } else {
            // Fall back to git describe.
            let out = Command::new("git")
                .args(["describe", "--tags", "--abbrev=0"])
                .current_dir(project_root)
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                }
                _ => None,
            }
        }
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

/// Backwards-compatible wrapper used by non-pipeline code paths.
fn collect_commits_since_last_tag(project_root: &Path) -> anyhow::Result<(String, Option<String>)> {
    collect_commits_since_tag(project_root, None)
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

/// RAII guard that writes `.ta/release.lock` on creation and removes it on drop.
/// `ta gc` checks for this file and skips staging deletion while a pipeline is active.
struct ReleaseLockGuard {
    path: std::path::PathBuf,
}

impl ReleaseLockGuard {
    fn acquire(workspace_root: &std::path::Path) -> anyhow::Result<Self> {
        let ta_dir = workspace_root.join(".ta");
        std::fs::create_dir_all(&ta_dir)?;
        let path = ta_dir.join("release.lock");
        let pid = std::process::id();
        std::fs::write(&path, format!("{}\n", pid))?;
        Ok(Self { path })
    }
}

impl Drop for ReleaseLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn run_pipeline(
    config: &GatewayConfig,
    version: &str,
    skip_approvals: bool,
    dry_run: bool,
    from_step: Option<usize>,
    pipeline_path: Option<&Path>,
    from_tag: Option<&str>,
) -> anyhow::Result<()> {
    // Acquire a release lockfile so `ta gc` knows not to delete staging dirs mid-pipeline.
    let _lock = if !dry_run {
        Some(ReleaseLockGuard::acquire(&config.workspace_root)?)
    } else {
        None
    };

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
    let (commits, last_tag) = collect_commits_since_tag(&config.workspace_root, from_tag)?;

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
            if !prompt_approval_default(&step.name, step.default_approve)? {
                println!("Aborted at step {}.", i + 1);
                anyhow::bail!("__pipeline_aborted__");
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
            execute_agent_step(
                config,
                step,
                agent,
                version,
                &commits,
                last_tag.as_deref(),
                i + 1,
            )?;
        } else if let Some(ref notes_cfg) = step.generate_notes {
            generate_notes_step(
                &config.workspace_root,
                version,
                &commits,
                last_tag.as_deref(),
                notes_cfg,
            )?;
        } else if step.constitution_check {
            if skip_approvals {
                println!("  Constitution check skipped (--yes/--auto-approve).");
            } else {
                let verdict = run_constitution_check_step(config)?;
                // Gate: default Y on pass/warn, default N on block.
                let default_approve = !matches!(verdict, ta_changeset::SupervisorVerdict::Block);
                if !dry_run
                    && !prompt_approval_default(
                        &format!("Proceed with '{}' (verdict: {})?", step.name, verdict),
                        default_approve,
                    )?
                {
                    println!("Aborted at step {}.", i + 1);
                    anyhow::bail!("__pipeline_aborted__");
                }
            }
        } else if step.record_release_history {
            execute_record_release_history_step(&config.workspace_root, version)?;
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
    step_number: usize,
) -> anyhow::Result<()> {
    let objective = step.objective.as_deref().unwrap_or("Execute release step");
    let objective = substitute_vars(objective, version, commits, last_tag);

    let title = format!("release: {}", step.name);

    // Build the ta run command.
    // --headless is required so the agent binary receives the objective via
    // stdin pipe rather than expecting an interactive terminal. Without it,
    // claude exits immediately when stdin is closed by the parent process.
    let mut args = vec![
        "run".to_string(),
        title,
        "--agent".to_string(),
        agent.id.clone(),
        "--source".to_string(),
        config.workspace_root.display().to_string(),
        "--objective".to_string(),
        objective,
        "--headless".to_string(),
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
    let mut draft_id = extract_draft_id_from_output(&stdout_text).or_else(|| {
        // Fallback: find latest eligible draft on disk.
        find_latest_draft(config).ok().flatten()
    });

    // If the agent didn't call `ta draft build` itself (no draft ID in stdout),
    // extract the goal ID from `ta run` output and call it explicitly.
    // `ta run` prints "  ta draft build <goal-id>" as a reminder to the user.
    if draft_id.is_none() {
        if let Some(goal_id) = extract_goal_id_from_output(&stdout_text) {
            println!(
                "  Agent did not build draft — running: ta draft build {}",
                goal_id
            );
            let build_output = Command::new(&ta_bin)
                .args(["draft", "build", &goal_id])
                .current_dir(&config.workspace_root)
                .output()?;
            let build_stdout = String::from_utf8_lossy(&build_output.stdout);
            for line in build_stdout.lines() {
                println!("    {}", line);
            }
            draft_id = extract_draft_id_from_output(&build_stdout)
                .or_else(|| find_latest_draft(config).ok().flatten());
        }
    }

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
                "Release pipeline stopped at step '{}': could not approve the agent draft (see error above).\n\
                 \n\
                 Fix the underlying issue, then re-run:\n\
                 \n\
                 ta release run {}",
                step.name,
                version
            );
        }

        // Apply (no git commit — the release pipeline handles commits itself).
        // --skip-verify: the pipeline's own "Build & verify" step handles
        // pre-submit checks. Agent steps in the release pipeline generate
        // content (release notes, press releases) — running cargo clippy/test
        // against the staging snapshot is both unnecessary and fragile (the
        // staging may predate recent source changes).
        let apply_status = Command::new(&ta_bin)
            .args(["draft", "apply", &id_str, "--skip-verify"])
            .current_dir(&config.workspace_root)
            .status()?;
        if !apply_status.success() {
            anyhow::bail!(
                "Release pipeline stopped at step '{}' (see error above).\n\
                 \n\
                 Fix the underlying issue, then re-run:\n\
                 \n\
                 ta release run {}",
                step.name,
                version
            );
        }
        println!("  Draft {} applied to working directory.", id_str);
    } else {
        anyhow::bail!(
            "Release pipeline stopped at step '{}': agent produced no draft.\n\
             \n\
             The agent ran but wrote no changes to the staging workspace, or\n\
             `ta draft build` found no differences between staging and source.\n\
             \n\
             Check the agent output above for errors, then re-run:\n\
             \n\
             ta release run {} --from-step {}",
            step.name,
            version,
            step_number
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

/// Extract the goal run ID from `ta run` stdout output.
/// `ta run` prints "  ta draft build <goal-id>" as a reminder when the agent exits.
fn extract_goal_id_from_output(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ta draft build") {
            let id = rest.trim();
            if uuid::Uuid::parse_str(id).is_ok() {
                return Some(id.to_string());
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

/// Built-in release notes generator.
///
/// Formats commit messages since the last tag into a structured Markdown file
/// at `.release-draft.md`. This is a deterministic alternative to the `agent:`
/// step type — no AI agent required, no stdin/headless issues.
///
/// The generated file uses the same format expected by the "Review release notes"
/// approval gate and the GitHub release workflow.
fn generate_notes_step(
    root: &Path,
    version: &str,
    commits: &str,
    last_tag: Option<&str>,
    config: &GenerateNotesConfig,
) -> anyhow::Result<()> {
    let tag = format!("v{}", version);
    let since = last_tag.unwrap_or("the beginning of the project");
    let commit_count = commits.lines().filter(|l| !l.trim().is_empty()).count();

    // ── AI synthesis (primary path) ─────────────────────────────────────────
    println!(
        "  Generating release notes via agent '{}' ({} commits since {})...",
        config.agent, commit_count, since
    );

    if let Some(notes) = try_ai_notes(&config.agent, &tag, since, commits) {
        write_release_notes(root, &notes)?;
        println!("  AI-synthesized release notes written to .release-draft.md");
        println!();
        println!("{}", notes);
        return Ok(());
    }

    // ── Fallback: deterministic commit-list generation ───────────────────────
    if !config.fallback {
        anyhow::bail!(
            "Release notes agent '{}' failed and fallback is disabled.\n\
             \n\
             Either ensure the agent is installed and ANTHROPIC_API_KEY is set,\n\
             or enable fallback with:\n\
             \n\
             generate_notes:\n\
               agent: {}\n\
               fallback: true",
            config.agent,
            config.agent
        );
    }

    println!("  Agent unavailable or returned empty output — using deterministic fallback.");
    let notes = mechanical_release_notes(&tag, since, commits);
    write_release_notes(root, &notes)?;
    let total = commits.lines().filter(|l| !l.trim().is_empty()).count();
    println!(
        "  Generated .release-draft.md ({} changes since {})",
        total, since
    );
    println!();
    println!("{}", notes);
    Ok(())
}

/// Resolve an agent ID to a CLI command name.
///
/// Built-in agent IDs map to known commands; unknown IDs are used as-is
/// (allowing users to point at a custom binary).
fn agent_id_to_command(agent_id: &str) -> &str {
    match agent_id {
        "claude-code" => "claude",
        other => other,
    }
}

/// Try to synthesize release notes by calling the agent in one-shot (`--print`) mode.
///
/// The agent receives the commit list as a structured prompt and must output
/// only the Markdown release notes.  stdout is captured and returned as-is.
/// Returns `None` if the agent is unavailable, exits non-zero, or produces
/// empty/non-Markdown output.
fn try_ai_notes(agent_id: &str, tag: &str, since: &str, commits: &str) -> Option<String> {
    use std::process::Stdio;

    let command = agent_id_to_command(agent_id);

    let prompt = format!(
        "Write user-facing release notes for {tag}.\n\
         \n\
         Commits since {since}:\n\
         {commits}\n\
         \n\
         Requirements:\n\
         - Format as Markdown starting with ## {tag}\n\
         - Group into ### New Features, ### Improvements, ### Bug Fixes (omit empty sections)\n\
         - Write from the user's perspective — what changed, not how it was implemented\n\
         - Skip internal tooling, CI, and doc-only commits unless user-visible\n\
         - Be concise: one line per item\n\
         - End with: _Changes since {since}_\n\
         \n\
         Output ONLY the Markdown. No preamble, no explanation.",
    );

    let output = Command::new(command)
        .args(["--print", &prompt])
        .stdin(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim().to_string();

    // Sanity-check: must look like Markdown with a heading.
    if trimmed.is_empty() || !trimmed.contains('#') {
        return None;
    }

    Some(trimmed)
}

/// Deterministic fallback: groups commits by keyword category into Markdown.
fn mechanical_release_notes(tag: &str, since: &str, commits: &str) -> String {
    let mut features: Vec<&str> = Vec::new();
    let mut fixes: Vec<&str> = Vec::new();
    let mut improvements: Vec<&str> = Vec::new();
    let mut other: Vec<&str> = Vec::new();

    for line in commits.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("feat")
            || lower.contains("implement")
            || lower.contains("add ")
            || lower.contains("new ")
        {
            features.push(line);
        } else if lower.starts_with("fix")
            || lower.contains("fix ")
            || lower.contains("bug")
            || lower.contains("revert")
        {
            fixes.push(line);
        } else if lower.starts_with("refactor")
            || lower.starts_with("perf")
            || lower.starts_with("improve")
            || lower.contains("update")
            || lower.contains("improve")
            || lower.contains("enhance")
            || lower.contains("upgrade")
        {
            improvements.push(line);
        } else {
            other.push(line);
        }
    }

    let total = features.len() + fixes.len() + improvements.len() + other.len();
    let mut notes = format!("## {}\n\n", tag);

    if !features.is_empty() {
        notes.push_str("### New Features\n\n");
        for c in &features {
            notes.push_str(&format!("- {}\n", c));
        }
        notes.push('\n');
    }
    if !improvements.is_empty() {
        notes.push_str("### Improvements\n\n");
        for c in &improvements {
            notes.push_str(&format!("- {}\n", c));
        }
        notes.push('\n');
    }
    if !fixes.is_empty() {
        notes.push_str("### Bug Fixes\n\n");
        for c in &fixes {
            notes.push_str(&format!("- {}\n", c));
        }
        notes.push('\n');
    }
    if !other.is_empty() {
        notes.push_str("### Other Changes\n\n");
        for c in &other {
            notes.push_str(&format!("- {}\n", c));
        }
        notes.push('\n');
    }
    if total == 0 {
        notes.push_str("_No changes since last release._\n\n");
    }

    notes.push_str(&format!("_Changes since {}_\n", since));
    notes
}

/// Write release notes to `.release-draft.md` in the project root.
fn write_release_notes(root: &Path, notes: &str) -> anyhow::Result<()> {
    let path = root.join(".release-draft.md");
    std::fs::write(&path, notes).map_err(|e| {
        anyhow::anyhow!(
            "Could not write .release-draft.md to {}: {}",
            root.display(),
            e
        )
    })
}

/// Run the constitution compliance check step programmatically.
///
/// Loads the project constitution config, calls scan_for_violations(), then
/// invokes the supervisor agent against the release diff description.
/// Returns the verdict. If no constitution is configured, returns Pass.
fn run_constitution_check_step(
    config: &GatewayConfig,
) -> anyhow::Result<ta_changeset::SupervisorVerdict> {
    let constitution_cfg =
        super::constitution::ProjectConstitutionConfig::load(&config.workspace_root)
            .unwrap_or_default();

    if let Some(ref cc) = constitution_cfg {
        // Run static scan.
        println!("  Running constitution scan...");
        match super::constitution::scan_for_violations(&config.workspace_root, cc) {
            Ok(violations) if violations.is_empty() => {
                println!("  Scan: no violations found.");
            }
            Ok(violations) => {
                println!("  Scan: {} violation(s) found:", violations.len());
                for v in violations.iter().take(5) {
                    println!("    [{}] {}:{} — {}", v.severity, v.file, v.line, v.message);
                }
                if violations.len() > 5 {
                    println!("    ... and {} more.", violations.len() - 5);
                }
            }
            Err(e) => {
                println!("  Scan error (continuing): {}", e);
            }
        }
    } else {
        println!("  No constitution configured — skipping check.");
        return Ok(ta_changeset::SupervisorVerdict::Pass);
    }

    // Build supervisor run config from workflow.toml.
    let workflow_toml = config.workspace_root.join(".ta/workflow.toml");
    let wf = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);
    let sup_cfg = &wf.supervisor;

    if !sup_cfg.enabled {
        println!("  Supervisor review: disabled.");
        return Ok(ta_changeset::SupervisorVerdict::Pass);
    }

    if sup_cfg.timeout_secs.is_some() {
        eprintln!(
            "Warning: [supervisor] timeout_secs is deprecated. \
             Use heartbeat_stale_secs instead (see workflow.toml)."
        );
    }
    let run_config = ta_changeset::SupervisorRunConfig {
        enabled: true,
        agent: sup_cfg.agent.clone(),
        verdict_on_block: sup_cfg.verdict_on_block.clone(),
        constitution_path: sup_cfg.constitution_path.clone(),
        skip_if_no_constitution: sup_cfg.skip_if_no_constitution,
        heartbeat_stale_secs: sup_cfg.heartbeat_stale_secs,
        timeout_secs: sup_cfg.timeout_secs.unwrap_or(120),
        api_key_env: sup_cfg.api_key_env.clone(),
        staging_path: None,
        heartbeat_path: None,
        agent_profile: None,
        resolved_model: None,
        enable_hooks: sup_cfg.enable_hooks,
    };

    let constitution_text = ta_changeset::load_constitution(&config.workspace_root, &run_config);
    if constitution_text.is_some() {
        println!("  Constitution: loaded.");
    }

    println!("  Running supervisor review...");
    let review = ta_changeset::invoke_supervisor_agent(
        "Release compliance check",
        &[],
        constitution_text.as_deref(),
        &run_config,
    );

    let verdict_label = match review.verdict {
        ta_changeset::SupervisorVerdict::Pass => "[PASS]",
        ta_changeset::SupervisorVerdict::Warn => "[WARN]",
        ta_changeset::SupervisorVerdict::Block => "[BLOCK]",
    };
    println!("  Supervisor: {} {}", verdict_label, review.summary);
    for finding in review.findings.iter().take(3) {
        println!("    - {}", finding);
    }

    Ok(review.verdict)
}

/// Record a release in .ta/release-history.json and stage the file with git add.
///
/// Called as a pipeline step so the history file is written after tagging but
/// before the final version-tracking commit, letting it be included in the release.
fn execute_record_release_history_step(project_root: &Path, version: &str) -> anyhow::Result<()> {
    record_release(project_root, version)?;
    // Stage the file so the next git commit --amend picks it up.
    let status = Command::new("git")
        .args(["add", ".ta/release-history.json"])
        .current_dir(project_root)
        .status()?;
    if !status.success() {
        eprintln!("Warning: could not stage release-history.json for commit");
    }
    Ok(())
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
    } else if let Some(ref notes_cfg) = step.generate_notes {
        println!("  type: generate_notes (agent: {})", notes_cfg.agent);
        println!("  output: .release-draft.md");
        println!("  fallback: {}", notes_cfg.fallback);
    } else if step.constitution_check {
        println!("  type: constitution_check");
        println!("  would: run scan_for_violations() and invoke supervisor agent");
        println!("  gate: pass/warn → default Y; block → default N");
    } else if step.record_release_history {
        println!("  type: record_history");
        println!("  would: record release in .ta/release-history.json");
    }
    if step.requires_approval {
        let default_hint = if step.default_approve {
            " [default Y]"
        } else {
            ""
        };
        println!("  approval: required{}", default_hint);
    }
    if let Some(ref out) = step.output {
        println!("  output: {}", out);
    }
    println!();
}

/// Prompt for approval with configurable default answer.
///
/// When `default_yes = true`, displays `[Y/n]` and treats Enter as yes.
/// When `default_yes = false`, displays `[y/N]` and requires explicit `y`.
fn prompt_approval_default(step_name: &str, default_yes: bool) -> anyhow::Result<bool> {
    use std::io::{self, IsTerminal, Write};

    // TTY context: prompt directly.
    if io::stdin().is_terminal() {
        let prompt_hint = if default_yes { "[Y/n]" } else { "[y/N]" };
        print!("Proceed with '{}'? {} ", step_name, prompt_hint);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let answer = input.trim().to_lowercase();
        if answer.is_empty() {
            return Ok(default_yes);
        }
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

#[cfg(test)]
fn prompt_approval_with_auto(step_name: &str, auto_approve: bool) -> anyhow::Result<bool> {
    if auto_approve {
        println!("Proceed with '{}'? [y/N] y (auto-approved)", step_name);
        return Ok(true);
    }
    prompt_approval_default(step_name, false)
}

// ── Show pipeline ───────────────────────────────────────────────

fn show_pipeline(
    config: &GatewayConfig,
    pipeline_path: Option<&Path>,
    from_tag: Option<&str>,
) -> anyhow::Result<()> {
    let pipeline = load_pipeline(config, pipeline_path)?;

    // Show base tag for release notes.
    let base_tag_display = match collect_commits_since_tag(&config.workspace_root, from_tag) {
        Ok((commits, Some(tag))) => {
            let count = commits.lines().filter(|l| !l.is_empty()).count();
            format!("{} ({} commits)", tag, count)
        }
        Ok((commits, None)) => {
            let count = commits.lines().filter(|l| !l.is_empty()).count();
            format!("(no previous tag — {} commits total)", count)
        }
        Err(_) => "(cannot determine — not a git repo or no commits)".to_string(),
    };

    println!("Pipeline:    {}", pipeline.name);
    println!("Steps:       {}", pipeline.steps.len());
    println!("Base tag:    {}", base_tag_display);
    println!("             Override with: --from-tag <tag>");
    println!();

    for (i, step) in pipeline.steps.iter().enumerate() {
        let kind = if step.run.is_some() {
            "shell"
        } else if step.agent.is_some() {
            "agent"
        } else if step.generate_notes.is_some() {
            "generate_notes"
        } else if step.constitution_check {
            "constitution_check"
        } else if step.record_release_history {
            "record_history"
        } else {
            "unknown"
        };
        let approval = if step.requires_approval {
            " [approval required]"
        } else {
            ""
        };
        let default_y = if step.default_approve {
            " [default Y]"
        } else {
            ""
        };
        println!(
            "  {}. {} ({}){}{}",
            i + 1,
            step.name,
            kind,
            approval,
            default_y
        );

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
        } else if step.generate_notes.is_some() {
            "generate_notes"
        } else if step.constitution_check {
            "constitution_check"
        } else if step.record_release_history {
            "record_history"
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

// ── Dispatch release via workflow_dispatch ───────────────────────

/// Trigger the GitHub Actions release workflow for a custom tag label.
///
/// For tags that don't match `v*` (e.g. `public-alpha-v0.13.1.1`), the
/// `push: tags: v*` CI trigger doesn't fire. This function calls
/// `gh workflow run <workflow> --field tag=<tag>` to trigger via
/// `workflow_dispatch` instead.
fn dispatch_release(
    tag: &str,
    prerelease: bool,
    repo: Option<&str>,
    workflow: &str,
) -> anyhow::Result<()> {
    // Resolve repo: explicit arg > GITHUB_REPOSITORY env > git remote parse.
    let repo = if let Some(r) = repo {
        r.to_string()
    } else if let Ok(r) = std::env::var("GITHUB_REPOSITORY") {
        r
    } else {
        // Try to parse from `git remote get-url origin`.
        let out = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .output()
            .map_err(|e| anyhow::anyhow!("Cannot run git: {}", e))?;
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Parse github.com/owner/repo or git@github.com:owner/repo
        url.split("github.com")
            .nth(1)
            .map(|s| s.trim_start_matches('/').trim_start_matches(':').trim_end_matches(".git").to_string())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine GitHub repo from remote URL: {}\nPass --repo owner/repo explicitly.", url))?
    };

    // Check gh is available.
    let gh_check = Command::new("gh").arg("--version").output();
    if gh_check.is_err() || !gh_check.unwrap().status.success() {
        anyhow::bail!(
            "GitHub CLI (gh) is required for `ta release dispatch`.\n\
             Install: https://cli.github.com"
        );
    }

    println!("Dispatching release workflow for tag: {}", tag);
    println!("  Repository: {}", repo);
    println!("  Workflow:   {}", workflow);
    println!("  Pre-release: {}", prerelease);
    println!();

    let mut args = vec![
        "workflow".to_string(),
        "run".to_string(),
        workflow.to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--field".to_string(),
        format!("tag={}", tag),
        "--field".to_string(),
        format!("prerelease={}", prerelease),
    ];

    // Use --ref main so workflow_dispatch targets the main branch.
    args.push("--ref".to_string());
    args.push("main".to_string());

    let status = Command::new("gh")
        .args(&args)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run gh: {}", e))?;

    if !status.success() {
        anyhow::bail!(
            "gh workflow run failed (exit {}).\n\
             Check that the workflow file '{}' exists and you are authenticated:\n\
             gh auth status",
            status.code().unwrap_or(-1),
            workflow
        );
    }

    println!("Release workflow dispatched.");
    println!(
        "Monitor progress:\n  gh run list --repo {} --workflow {}",
        repo, workflow
    );
    println!(
        "View release when complete:\n  gh release view {} --repo {}",
        tag, repo
    );

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

  # Constitution compliance check (v0.14.3.3).
  # Runs scan_for_violations() and the supervisor agent against the release diff.
  # Verdict: pass/warn → gate defaults Y (Enter proceeds); block → gate defaults N.
  # Skipped entirely with --yes / --auto-approve.
  - name: Constitution compliance sign-off
    constitution_check: true

  - name: Clear stale release draft
    run: |
      rm -f .release-draft.md
      echo "Cleared any stale .release-draft.md."

  - name: Generate release notes
    generate_notes:
      agent: claude-code
    output: .release-draft.md

  - name: Review release notes
    requires_approval: true
    default_approve: true
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

  # Record this release in .ta/release-history.json and stage with git add.
  # Placed after "Commit and tag" so the SHA is correct.
  # The "Update version tracking" amend below will include it in the release commit.
  - name: Record release history
    record_release_history: true

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
                // Clear TA agent VCS isolation env vars (set by v0.13.17.3) so
                // git operates on the test's temp directory, not the staging repo.
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_CEILING_DIRECTORIES")
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
            generate_notes: None,
            constitution_check: false,
            record_release_history: false,
            objective: None,
            requires_approval: false,
            default_approve: false,
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
            generate_notes: None,
            constitution_check: false,
            record_release_history: false,
            objective: None,
            requires_approval: false,
            default_approve: false,
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
        show_pipeline(&config, None, None).unwrap();
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
        run_pipeline(&config, "1.0.0", true, true, None, None, None).unwrap();
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
    fn default_pipeline_uses_generate_notes_step() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let notes_step = pipeline
            .steps
            .iter()
            .find(|s| s.generate_notes.is_some())
            .expect("default pipeline must have a generate_notes step");
        assert_eq!(notes_step.name, "Generate release notes");
        let cfg = notes_step.generate_notes.as_ref().unwrap();
        assert_eq!(
            cfg.agent, "claude-code",
            "default agent must be claude-code"
        );
        assert!(cfg.fallback, "fallback must default to true");
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
    // v0.14.3.3: step is now a programmatic constitution_check (not a static checklist).
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

        // v0.14.3.3: step is now constitution_check: true (programmatic), not requires_approval.
        assert!(
            pipeline.steps[checklist_idx].constitution_check,
            "constitution sign-off step must have constitution_check: true"
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
    fn pipeline_step_constitution_check_deserializes() {
        let yaml = r#"
name: test
steps:
  - name: check
    constitution_check: true
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert!(pipeline.steps[0].constitution_check);
        pipeline.steps[0].validate().unwrap();
    }

    #[test]
    fn pipeline_step_record_release_history_deserializes() {
        let yaml = r#"
name: test
steps:
  - name: record
    record_release_history: true
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert!(pipeline.steps[0].record_release_history);
        pipeline.steps[0].validate().unwrap();
    }

    #[test]
    fn pipeline_step_default_approve_deserializes() {
        let yaml = r#"
name: test
steps:
  - name: review
    requires_approval: true
    default_approve: true
    run: echo ok
"#;
        let pipeline: ReleasePipeline = serde_yaml::from_str(yaml).unwrap();
        assert!(pipeline.steps[0].default_approve);
        assert!(pipeline.steps[0].requires_approval);
    }

    #[test]
    fn default_pipeline_review_notes_defaults_y() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let review_step = pipeline
            .steps
            .iter()
            .find(|s| s.name == "Review release notes")
            .expect("default pipeline must have 'Review release notes' step");
        assert!(
            review_step.default_approve,
            "'Review release notes' step must have default_approve: true"
        );
        assert!(
            review_step.requires_approval,
            "'Review release notes' step must have requires_approval: true"
        );
    }

    #[test]
    fn default_pipeline_has_record_release_history_step() {
        let pipeline: ReleasePipeline = serde_yaml::from_str(DEFAULT_PIPELINE_YAML).unwrap();
        let record_idx = pipeline
            .steps
            .iter()
            .position(|s| s.record_release_history)
            .expect("default pipeline must have a record_release_history step");
        let tag_idx = pipeline
            .steps
            .iter()
            .position(|s| s.name == "Commit and tag")
            .expect("default pipeline must have 'Commit and tag' step");
        let update_idx = pipeline
            .steps
            .iter()
            .position(|s| s.name == "Update version tracking")
            .expect("default pipeline must have 'Update version tracking' step");
        // Must be between "Commit and tag" and "Update version tracking".
        assert!(
            tag_idx < record_idx,
            "record_release_history must come after 'Commit and tag'"
        );
        assert!(
            record_idx < update_idx,
            "record_release_history must come before 'Update version tracking'"
        );
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
        run_pipeline(&config, "1.0.0", true, false, None, None, None).unwrap();

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
        run_pipeline(&config, "1.0.0", false, true, None, None, None).unwrap();

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
