//! Workflow configuration structures

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level workflow configuration from .ta/workflow.toml
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// Submit adapter configuration
    #[serde(default)]
    pub submit: SubmitConfig,

    /// Diff viewing configuration
    #[serde(default)]
    pub diff: DiffConfig,

    /// Display / output configuration
    #[serde(default)]
    pub display: DisplayConfig,

    /// Build configuration
    #[serde(default)]
    pub build: BuildConfig,

    /// Garbage collection / lifecycle configuration
    #[serde(default)]
    pub gc: GcConfig,

    /// Follow-up goal behavior configuration
    #[serde(default)]
    pub follow_up: FollowUpConfig,

    /// Pre-draft verification gate configuration
    #[serde(default)]
    pub verify: VerifyConfig,

    /// Shell TUI configuration
    #[serde(default)]
    pub shell: ShellConfig,

    /// Desktop notification configuration
    #[serde(default)]
    pub notify: NotifyConfig,
}

/// Submit adapter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitConfig {
    /// Adapter type: "git", "none", or future adapters
    #[serde(default = "default_adapter")]
    pub adapter: String,

    /// Auto-commit on `ta pr apply` (only active when .ta/workflow.toml exists)
    #[serde(default)]
    pub auto_commit: bool,

    /// Auto-push after commit
    #[serde(default)]
    pub auto_push: bool,

    /// Auto-create review (PR) after push
    #[serde(default)]
    pub auto_review: bool,

    /// Co-author trailer appended to every commit made through TA.
    /// Format: "Name <email>". The email should match a GitHub account's
    /// verified email for the contribution to appear in GitHub's graph.
    /// Set to empty string to disable. Default: "Trusted Autonomy <ta@trustedautonomy.dev>"
    #[serde(default = "default_co_author")]
    pub co_author: String,

    /// Git-specific configuration
    #[serde(default)]
    pub git: GitConfig,
}

impl Default for SubmitConfig {
    fn default() -> Self {
        Self {
            adapter: default_adapter(),
            auto_commit: false,
            auto_push: false,
            auto_review: false,
            co_author: default_co_author(),
            git: GitConfig::default(),
        }
    }
}

/// Git adapter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    /// Branch naming prefix (e.g., "ta/", "feature/")
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,

    /// Target branch for PRs (e.g., "main", "develop")
    #[serde(default = "default_target_branch")]
    pub target_branch: String,

    /// Merge strategy: "squash", "merge", "rebase"
    #[serde(default = "default_merge_strategy")]
    pub merge_strategy: String,

    /// Path to PR body template (optional)
    pub pr_template: Option<PathBuf>,

    /// Git remote name
    #[serde(default = "default_remote")]
    pub remote: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            branch_prefix: default_branch_prefix(),
            target_branch: default_target_branch(),
            merge_strategy: default_merge_strategy(),
            pr_template: None,
            remote: default_remote(),
        }
    }
}

// Serde default functions
fn default_adapter() -> String {
    "none".to_string()
}

fn default_co_author() -> String {
    "Trusted Autonomy <266386695+trustedautonomy-agent@users.noreply.github.com>".to_string()
}

fn default_branch_prefix() -> String {
    "ta/".to_string()
}

fn default_target_branch() -> String {
    "main".to_string()
}

fn default_merge_strategy() -> String {
    "squash".to_string()
}

fn default_remote() -> String {
    "origin".to_string()
}

/// Diff viewing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffConfig {
    /// Open files in external handlers by default when using `ta pr view --file`
    #[serde(default = "default_open_external")]
    pub open_external: bool,

    /// Optional path override for diff-handlers.toml (defaults to .ta/diff-handlers.toml)
    pub handlers_file: Option<PathBuf>,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            open_external: default_open_external(),
            handlers_file: None,
        }
    }
}

fn default_open_external() -> bool {
    true
}

/// Build pipeline configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Summary enforcement level at `ta draft build` time.
    /// - "ignore": No check — artifacts without descriptions are silently accepted.
    /// - "warning" (default): Print a warning listing artifacts missing descriptions.
    /// - "error": Fail the build if any non-exempt artifact lacks a description.
    ///
    /// Exempt files (lockfiles, config manifests, docs) always get auto-summaries.
    #[serde(default = "default_summary_enforcement")]
    pub summary_enforcement: String,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            summary_enforcement: default_summary_enforcement(),
        }
    }
}

fn default_summary_enforcement() -> String {
    "warning".to_string()
}

/// Display / output configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Enable ANSI color output in terminal adapter. Default: false.
    /// Override per-command with `--color`.
    #[serde(default)]
    pub color: bool,
}

/// Garbage collection / draft lifecycle configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcConfig {
    /// Number of days after which drafts in terminal states (Applied, Denied, Closed)
    /// become eligible for staging directory cleanup. Default: 7.
    #[serde(default = "default_stale_threshold_days")]
    pub stale_threshold_days: u64,

    /// Emit a one-line warning on `ta` startup if stale drafts exist. Default: true.
    #[serde(default = "default_health_check")]
    pub health_check: bool,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            stale_threshold_days: default_stale_threshold_days(),
            health_check: default_health_check(),
        }
    }
}

fn default_stale_threshold_days() -> u64 {
    7
}

/// Follow-up goal behavior configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowUpConfig {
    /// Default mode for --follow-up: "extend" reuses parent staging, "standalone" creates fresh copy.
    #[serde(default = "default_follow_up_mode")]
    pub default_mode: String,

    /// Auto-supersede parent draft when building from same staging directory.
    #[serde(default = "default_auto_supersede")]
    pub auto_supersede: bool,

    /// Re-snapshot source before applying when source has changed since goal start.
    #[serde(default = "default_rebase_on_apply")]
    pub rebase_on_apply: bool,
}

impl Default for FollowUpConfig {
    fn default() -> Self {
        Self {
            default_mode: default_follow_up_mode(),
            auto_supersede: default_auto_supersede(),
            rebase_on_apply: default_rebase_on_apply(),
        }
    }
}

fn default_follow_up_mode() -> String {
    "extend".to_string()
}

fn default_auto_supersede() -> bool {
    true
}

fn default_rebase_on_apply() -> bool {
    true
}

fn default_health_check() -> bool {
    true
}

/// Failure handling strategy for verification commands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyOnFailure {
    /// Do not create a draft. Print which command failed with output.
    #[default]
    Block,
    /// Create the draft but attach verification warnings visible in `ta draft view`.
    Warn,
    /// Re-launch the agent with the failure output injected as context.
    Agent,
}

impl std::fmt::Display for VerifyOnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Block => write!(f, "block"),
            Self::Warn => write!(f, "warn"),
            Self::Agent => write!(f, "agent"),
        }
    }
}

/// A single verification command with optional per-command timeout.
///
/// Used in `[[verify.commands]]` TOML arrays for per-command configuration.
/// When only a string is needed, the flat `commands` list (backward compat) works too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyCommand {
    /// The shell command to run.
    pub run: String,

    /// Per-command timeout in seconds. If omitted, `default_timeout_secs` is used.
    pub timeout_secs: Option<u64>,
}

/// Pre-draft verification gate configuration.
///
/// Commands run in the staging directory after the agent exits but before
/// the draft is created. If any command fails, behavior depends on `on_failure`.
///
/// Supports two command formats (backward compatible):
/// - Flat string list: `commands = ["cmd1", "cmd2"]` (legacy)
/// - Structured commands: `[[verify.commands]]` with `run` and optional `timeout_secs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyConfig {
    /// Commands to run sequentially. All must exit 0 for verification to pass.
    /// Accepts either plain strings or `VerifyCommand` objects.
    #[serde(default, deserialize_with = "deserialize_verify_commands")]
    pub commands: Vec<VerifyCommand>,

    /// Behavior when a command fails: "block" (default), "warn", or "agent".
    #[serde(default)]
    pub on_failure: VerifyOnFailure,

    /// Legacy: global timeout per command in seconds. Default: 300 (5 minutes).
    /// Superseded by `default_timeout_secs`; kept for backward compat.
    #[serde(default = "default_verify_timeout")]
    pub timeout: u64,

    /// Default timeout per command in seconds when not specified per-command.
    /// If set, takes priority over `timeout`. Default: 300.
    pub default_timeout_secs: Option<u64>,

    /// Heartbeat interval in seconds for long-running verification commands.
    /// A progress message is emitted every N seconds. Default: 30.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,
}

impl VerifyConfig {
    /// Effective default timeout: `default_timeout_secs` if set, else legacy `timeout`.
    pub fn effective_default_timeout(&self) -> u64 {
        self.default_timeout_secs.unwrap_or(self.timeout)
    }

    /// Resolve the timeout for a specific command.
    pub fn command_timeout(&self, cmd: &VerifyCommand) -> u64 {
        cmd.timeout_secs
            .unwrap_or_else(|| self.effective_default_timeout())
    }
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            on_failure: VerifyOnFailure::default(),
            timeout: default_verify_timeout(),
            default_timeout_secs: None,
            heartbeat_interval_secs: default_heartbeat_interval(),
        }
    }
}

fn default_verify_timeout() -> u64 {
    300
}

fn default_heartbeat_interval() -> u64 {
    30
}

/// Deserialize commands from either a list of strings or a list of VerifyCommand objects.
fn deserialize_verify_commands<'de, D>(deserializer: D) -> Result<Vec<VerifyCommand>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CommandItem {
        Simple(String),
        Structured(VerifyCommand),
    }

    let items: Vec<CommandItem> = Vec::deserialize(deserializer)?;
    Ok(items
        .into_iter()
        .map(|item| match item {
            CommandItem::Simple(s) => VerifyCommand {
                run: s,
                timeout_secs: None,
            },
            CommandItem::Structured(c) => c,
        })
        .collect())
}

/// Shell TUI configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Number of lines to backfill when attaching to a tail stream. Default: 5.
    #[serde(default = "default_tail_backfill_lines")]
    pub tail_backfill_lines: usize,

    /// Maximum number of lines retained in the TUI output buffer. Default: 50000.
    /// Older lines are dropped when this limit is exceeded.
    #[serde(default = "default_output_buffer_lines")]
    pub output_buffer_lines: usize,

    /// Alias for `output_buffer_lines` — configurable as `scrollback_lines` (v0.10.18.2).
    /// If set, overrides `output_buffer_lines`. Minimum enforced: 10,000.
    #[serde(default)]
    pub scrollback_lines: Option<usize>,

    /// Automatically tail agent output when a goal starts. Default: true.
    #[serde(default = "default_auto_tail")]
    pub auto_tail: bool,
}

impl ShellConfig {
    /// Effective scrollback buffer size: `scrollback_lines` if set, else `output_buffer_lines`.
    /// Enforces a minimum of 10,000 lines.
    pub fn effective_scrollback(&self) -> usize {
        let raw = self.scrollback_lines.unwrap_or(self.output_buffer_lines);
        raw.max(10_000)
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            tail_backfill_lines: default_tail_backfill_lines(),
            output_buffer_lines: default_output_buffer_lines(),
            scrollback_lines: None,
            auto_tail: default_auto_tail(),
        }
    }
}

fn default_tail_backfill_lines() -> usize {
    5
}

fn default_output_buffer_lines() -> usize {
    50000
}

fn default_auto_tail() -> bool {
    true
}

/// Desktop notification configuration.
///
/// When enabled, TA sends a system notification (macOS/Linux) when a draft
/// is ready for review, so users don't have to watch the terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// Enable desktop notifications. Default: true.
    #[serde(default = "default_notify_enabled")]
    pub enabled: bool,

    /// Title prefix for notifications. Default: "TA".
    #[serde(default = "default_notify_title")]
    pub title: String,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: default_notify_enabled(),
            title: default_notify_title(),
        }
    }
}

fn default_notify_enabled() -> bool {
    true
}

fn default_notify_title() -> String {
    "TA".to_string()
}

impl WorkflowConfig {
    /// Load workflow config from .ta/workflow.toml
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Try to load config, returning default if file doesn't exist
    pub fn load_or_default(path: &std::path::Path) -> Self {
        Self::load(path).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_config_defaults_to_warning() {
        let config = BuildConfig::default();
        assert_eq!(config.summary_enforcement, "warning");
    }

    #[test]
    fn workflow_config_default_has_build_section() {
        let config = WorkflowConfig::default();
        assert_eq!(config.build.summary_enforcement, "warning");
    }

    #[test]
    fn parse_toml_with_build_section() {
        let toml = r#"
[build]
summary_enforcement = "error"
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.build.summary_enforcement, "error");
    }

    #[test]
    fn parse_toml_without_build_section_uses_default() {
        let toml = r#"
[submit]
adapter = "git"
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.build.summary_enforcement, "warning");
    }

    #[test]
    fn gc_config_defaults() {
        let config = GcConfig::default();
        assert_eq!(config.stale_threshold_days, 7);
        assert!(config.health_check);
    }

    #[test]
    fn workflow_config_default_has_gc_section() {
        let config = WorkflowConfig::default();
        assert_eq!(config.gc.stale_threshold_days, 7);
        assert!(config.gc.health_check);
    }

    #[test]
    fn parse_toml_with_gc_section() {
        let toml = r#"
[gc]
stale_threshold_days = 14
health_check = false
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.gc.stale_threshold_days, 14);
        assert!(!config.gc.health_check);
    }

    #[test]
    fn load_or_default_returns_default_for_missing_file() {
        let config = WorkflowConfig::load_or_default(std::path::Path::new("/nonexistent/path"));
        assert_eq!(config.build.summary_enforcement, "warning");
        assert_eq!(config.submit.adapter, "none");
    }

    #[test]
    fn follow_up_config_defaults() {
        let config = FollowUpConfig::default();
        assert_eq!(config.default_mode, "extend");
        assert!(config.auto_supersede);
        assert!(config.rebase_on_apply);
    }

    #[test]
    fn workflow_config_default_has_follow_up_section() {
        let config = WorkflowConfig::default();
        assert_eq!(config.follow_up.default_mode, "extend");
        assert!(config.follow_up.auto_supersede);
        assert!(config.follow_up.rebase_on_apply);
    }

    #[test]
    fn parse_toml_with_follow_up_section() {
        let toml = r#"
[follow_up]
default_mode = "standalone"
auto_supersede = false
rebase_on_apply = false
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.follow_up.default_mode, "standalone");
        assert!(!config.follow_up.auto_supersede);
        assert!(!config.follow_up.rebase_on_apply);
    }

    #[test]
    fn verify_config_defaults() {
        let config = VerifyConfig::default();
        assert!(config.commands.is_empty());
        assert_eq!(config.on_failure, VerifyOnFailure::Block);
        assert_eq!(config.timeout, 300);
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert!(config.default_timeout_secs.is_none());
        assert_eq!(config.effective_default_timeout(), 300);
    }

    #[test]
    fn workflow_config_default_has_verify_section() {
        let config = WorkflowConfig::default();
        assert!(config.verify.commands.is_empty());
        assert_eq!(config.verify.on_failure, VerifyOnFailure::Block);
        assert_eq!(config.verify.timeout, 300);
    }

    #[test]
    fn parse_toml_with_verify_section() {
        let toml = r#"
[verify]
commands = [
    "cargo build --workspace",
    "cargo test --workspace",
]
on_failure = "warn"
timeout = 600
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.verify.commands.len(), 2);
        assert_eq!(config.verify.commands[0].run, "cargo build --workspace");
        assert_eq!(config.verify.on_failure, VerifyOnFailure::Warn);
        assert_eq!(config.verify.timeout, 600);
    }

    #[test]
    fn parse_toml_with_verify_agent_mode() {
        let toml = r#"
[verify]
commands = ["make test"]
on_failure = "agent"
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.verify.on_failure, VerifyOnFailure::Agent);
        assert_eq!(config.verify.timeout, 300); // default
    }

    #[test]
    fn parse_toml_without_verify_section_uses_default() {
        let toml = r#"
[submit]
adapter = "git"
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert!(config.verify.commands.is_empty());
        assert_eq!(config.verify.on_failure, VerifyOnFailure::Block);
    }

    #[test]
    fn parse_toml_with_per_command_timeout() {
        let toml = r#"
[verify]
default_timeout_secs = 300
heartbeat_interval_secs = 15

[[verify.commands]]
run = "cargo fmt --all -- --check"
timeout_secs = 60

[[verify.commands]]
run = "cargo test --workspace"
timeout_secs = 900
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.verify.commands.len(), 2);
        assert_eq!(config.verify.commands[0].run, "cargo fmt --all -- --check");
        assert_eq!(config.verify.commands[0].timeout_secs, Some(60));
        assert_eq!(config.verify.commands[1].run, "cargo test --workspace");
        assert_eq!(config.verify.commands[1].timeout_secs, Some(900));
        assert_eq!(config.verify.default_timeout_secs, Some(300));
        assert_eq!(config.verify.heartbeat_interval_secs, 15);
        assert_eq!(config.verify.effective_default_timeout(), 300);
        assert_eq!(
            config.verify.command_timeout(&config.verify.commands[0]),
            60
        );
        assert_eq!(
            config.verify.command_timeout(&config.verify.commands[1]),
            900
        );
    }

    #[test]
    fn per_command_timeout_falls_back_to_default() {
        let config = VerifyConfig {
            commands: vec![VerifyCommand {
                run: "test".to_string(),
                timeout_secs: None,
            }],
            default_timeout_secs: Some(600),
            ..Default::default()
        };
        assert_eq!(config.command_timeout(&config.commands[0]), 600);
    }

    #[test]
    fn effective_timeout_falls_back_to_legacy() {
        let config = VerifyConfig {
            timeout: 900,
            default_timeout_secs: None,
            ..Default::default()
        };
        assert_eq!(config.effective_default_timeout(), 900);
    }

    #[test]
    fn verify_on_failure_display() {
        assert_eq!(VerifyOnFailure::Block.to_string(), "block");
        assert_eq!(VerifyOnFailure::Warn.to_string(), "warn");
        assert_eq!(VerifyOnFailure::Agent.to_string(), "agent");
    }

    #[test]
    fn shell_config_defaults() {
        let config = ShellConfig::default();
        assert_eq!(config.tail_backfill_lines, 5);
        assert_eq!(config.output_buffer_lines, 50000);
        assert!(config.scrollback_lines.is_none());
        assert!(config.auto_tail);
        assert_eq!(config.effective_scrollback(), 50000);
    }

    #[test]
    fn workflow_config_default_has_shell_section() {
        let config = WorkflowConfig::default();
        assert_eq!(config.shell.tail_backfill_lines, 5);
        assert_eq!(config.shell.output_buffer_lines, 50000);
        assert!(config.shell.auto_tail);
    }

    #[test]
    fn parse_toml_with_shell_section() {
        let toml = r#"
[shell]
tail_backfill_lines = 20
output_buffer_lines = 5000
auto_tail = false
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.shell.tail_backfill_lines, 20);
        assert_eq!(config.shell.output_buffer_lines, 5000);
        assert!(!config.shell.auto_tail);
    }

    #[test]
    fn parse_toml_without_shell_section_uses_default() {
        let toml = r#"
[submit]
adapter = "git"
"#;
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.shell.tail_backfill_lines, 5);
        assert_eq!(config.shell.output_buffer_lines, 50000);
        assert!(config.shell.auto_tail);
    }
}
