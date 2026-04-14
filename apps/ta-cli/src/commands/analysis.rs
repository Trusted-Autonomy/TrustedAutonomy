// analysis.rs — `ta analysis` subcommand (v0.15.14.3).
//
// Runs the configured static analysis tool for the current workspace outside of
// a goal/workflow. Prints a structured findings table. With `--fix`, triggers
// the agent correction loop as a standalone goal that produces a draft for review.
//
// Usage:
//   ta analysis run [--lang <lang>]
//   ta analysis run --fix [--lang <lang>]

use std::path::Path;
use std::str::FromStr as _;

use clap::Subcommand;
use ta_goal::analysis::{
    detect_language, run_analyzer, AnalysisFinding, FindingSeverity, Language, OnFailure,
};
use ta_mcp_gateway::GatewayConfig;

// ── CLI surface ───────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum AnalysisCommands {
    /// Run the configured static analysis tool for the current workspace.
    ///
    /// Reads `[analysis.<lang>]` from `.ta/workflow.toml`. Language is
    /// auto-detected from workspace marker files (Cargo.toml, go.mod, etc.)
    /// unless overridden with `--lang`.
    ///
    /// Examples:
    ///   ta analysis run
    ///   ta analysis run --lang python
    ///   ta analysis run --fix
    Run {
        /// Override the language to analyse (python, typescript, rust, go).
        #[arg(long)]
        lang: Option<String>,

        /// Trigger the agent correction loop. Spawns a fix goal that produces
        /// a draft for human review instead of auto-applying.
        #[arg(long)]
        fix: bool,

        /// Agent to use for the correction loop (default: claude-code).
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
}

pub fn execute(command: &AnalysisCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        AnalysisCommands::Run { lang, fix, agent } => {
            run_analysis(config, lang.as_deref(), *fix, agent)
        }
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

fn run_analysis(
    config: &GatewayConfig,
    lang_override: Option<&str>,
    fix_mode: bool,
    agent: &str,
) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;

    // Load [analysis.*] from .ta/workflow.toml.
    let workflow_config = load_workflow_config(project_root);

    // Resolve language.
    let language = resolve_language(lang_override, project_root, &workflow_config)?;
    let lang_key = language.as_key();

    // Find the analysis config for this language.
    let analysis_cfg = workflow_config
        .analysis
        .get(&lang_key)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No [analysis.{}] section found in .ta/workflow.toml.\n\
                 Add a configuration block, for example:\n\n\
                 [analysis.{}]\n\
                 tool = \"{}\"\n\
                 on_failure = \"agent\"",
                lang_key,
                lang_key,
                default_tool_for_language(&language)
            )
        })?;

    println!(
        "Running {} static analysis (tool: {})...",
        language, analysis_cfg.tool
    );

    // Run the analyzer.
    let (success, _raw, findings) = run_analyzer(&analysis_cfg, project_root)?;

    if success && findings.is_empty() {
        println!("  No findings — analysis passed.");
        return Ok(());
    }

    // Partition by severity.
    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Error)
        .collect();
    let warnings: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Warning)
        .collect();
    let other: Vec<_> = findings
        .iter()
        .filter(|f| f.severity != FindingSeverity::Error && f.severity != FindingSeverity::Warning)
        .collect();

    println!(
        "\nFindings: {} error(s), {} warning(s), {} note(s)",
        errors.len(),
        warnings.len(),
        other.len()
    );
    println!("\n{}", AnalysisFinding::format_table(&findings));

    if success && errors.is_empty() {
        // Tool exited 0 but emitted warnings — still print them, not a failure.
        return Ok(());
    }

    // Failures below.
    if !fix_mode {
        match analysis_cfg.on_failure {
            OnFailure::Fail => {
                anyhow::bail!(
                    "{} found {} error(s). Run `ta analysis run --fix` to invoke the agent \
                     correction loop.",
                    analysis_cfg.tool,
                    errors.len()
                )
            }
            OnFailure::Warn => {
                println!(
                    "[warn] {} errors found. on_failure=warn — continuing.",
                    errors.len()
                );
                return Ok(());
            }
            OnFailure::Agent => {
                println!(
                    "on_failure=agent. Run `ta analysis run --fix` to trigger the correction loop."
                );
                return Ok(());
            }
        }
    }

    // --fix mode: spawn one targeted fix goal and produce a draft for review.
    println!("\n[fix] Spawning correction goal ({})...", agent);
    let objective = AnalysisFinding::build_fix_prompt(&analysis_cfg.tool, &language, &findings);
    let title = format!("Fix {} analysis findings ({})", analysis_cfg.tool, language);

    let mut cmd = std::process::Command::new("ta");
    cmd.args([
        "--project-root",
        &project_root.to_string_lossy(),
        "run",
        &title,
        "--agent",
        agent,
        "--headless",
        "--no-version-check",
        "--objective",
        &objective,
    ]);

    println!("  Running: ta run {:?} --agent {} --headless", title, agent);

    let output = cmd.output().map_err(|e| {
        anyhow::anyhow!(
            "Failed to invoke 'ta run': {}\nIs ta installed and on PATH?",
            e
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Echo output for observability.
    if !stdout.trim().is_empty() {
        print!("{}", stdout);
    }
    if !stderr.trim().is_empty() {
        eprint!("{}", stderr);
    }

    if !output.status.success() {
        anyhow::bail!(
            "Fix goal failed (exit {}).",
            output.status.code().unwrap_or(-1)
        );
    }

    // Extract draft ID from headless sentinel.
    let mut draft_id: Option<String> = None;
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(json_str) = line.strip_prefix("__TA_HEADLESS_RESULT__:") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(id) = v["draft_id"].as_str() {
                    if id != "null" && !id.is_empty() {
                        draft_id = Some(id.to_string());
                    }
                }
            }
            break;
        }
    }

    match draft_id {
        Some(id) => {
            println!(
                "\n[fix] Correction draft created: {}\nReview with: ta draft view {}",
                &id[..8.min(id.len())],
                &id[..8.min(id.len())]
            );
        }
        None => {
            println!(
                "\n[fix] Fix goal completed. Run `ta draft list` to find the draft for review."
            );
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Load WorkflowConfig from `.ta/workflow.toml`, returning a default on any error.
fn load_workflow_config(project_root: &Path) -> ta_submit::config::WorkflowConfig {
    let path = project_root.join(".ta").join("workflow.toml");
    if !path.exists() {
        return ta_submit::config::WorkflowConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => ta_submit::config::WorkflowConfig::default(),
    }
}

/// Resolve the target language from: CLI override > per-language config keys > auto-detect.
fn resolve_language(
    lang_override: Option<&str>,
    project_root: &Path,
    workflow_config: &ta_submit::config::WorkflowConfig,
) -> anyhow::Result<Language> {
    // 1. Explicit CLI override.
    if let Some(lang_str) = lang_override {
        return Ok(Language::from_str(lang_str).unwrap());
    }
    // 2. Auto-detect from workspace, but then confirm there's config for it.
    if let Some(detected) = detect_language(project_root) {
        if workflow_config.analysis.contains_key(&detected.as_key()) {
            return Ok(detected);
        }
    }
    // 3. If only one language is configured, use it.
    if workflow_config.analysis.len() == 1 {
        let key = workflow_config.analysis.keys().next().unwrap();
        return Ok(Language::from_str(key).unwrap());
    }
    // 4. Auto-detect result even if no config (will be caught when we look up config).
    if let Some(detected) = detect_language(project_root) {
        return Ok(detected);
    }
    anyhow::bail!(
        "Could not auto-detect the project language. Use --lang <lang> to specify it explicitly.\n\
         Supported: python, typescript, rust, go"
    )
}

/// Default tool suggestion when creating an [analysis.*] example for a given language.
fn default_tool_for_language(lang: &Language) -> &'static str {
    match lang {
        Language::Python => "mypy",
        Language::TypeScript => "pyright",
        Language::Rust => "cargo-clippy",
        Language::Go => "golangci-lint",
        Language::Other(_) => "your-linter",
    }
}
