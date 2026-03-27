// constitution.rs — Project behavioral constitution commands (v0.12.0).
//
// `ta constitution init` asks the QA agent to draft a `.ta/constitution.md`
// from the project's PLAN.md, CLAUDE.md, and stated objectives. The result
// is a behavioral contract for AI agents working on this project — defining
// rules, invariants, coding standards, and autonomy boundaries.
//
// This is the "simple" constitution init from §16.6 pull-forward. A single
// agent prompt produces the first draft for human review. The full v0.14.1
// constitution framework (guided UI, incremental sections, versioning) is
// deferred.
//
// v0.14.6.1: `ta constitution review` — deduplication via agent review.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use chrono::Utc;
use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

#[derive(Subcommand, Debug)]
pub enum ConstitutionCommands {
    /// Draft a behavioral constitution for this project.
    ///
    /// Reads PLAN.md, CLAUDE.md (if present), and asks the agent to produce
    /// a `.ta/constitution.md` — a behavioral contract defining rules,
    /// invariants, and autonomy policies for AI agents. The output is a
    /// TA draft for human review before applying.
    Init {
        /// Agent to use for drafting (default: claude-code).
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Path to additional context document (PRD, spec, guidelines).
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// Skip interactive session — let the agent draft from available docs only.
        #[arg(long)]
        non_interactive: bool,
    },
    /// Show the current .ta/constitution.md if it exists.
    Show,
    /// Check changed files against constitution rules (basic static check).
    ///
    /// Currently checks §4 (inject/restore balance) when s4_scan is enabled
    /// in .ta/workflow.toml. Additional checks will be added in v0.14.1.
    Check {
        /// Draft ID to check (defaults to latest build draft).
        #[arg(long)]
        draft_id: Option<String>,
    },
    /// Scaffold .ta/constitution.toml from the ta-default template (v0.13.9).
    ///
    /// Writes a starter `.ta/constitution.toml` with TA's default injection/cleanup
    /// rules, scan config, and validation steps. Edit it to match your project's
    /// patterns, then run `ta constitution check-toml` to validate.
    ///
    /// Use `--template` to get language-specific verify commands pre-populated.
    /// Available templates: rust (default), python, typescript, nodejs, go, generic.
    /// Auto-detects language if `--template` is omitted.
    InitToml {
        /// Language template: rust, python, typescript, nodejs, go, generic.
        /// Auto-detected from Cargo.toml, pyproject.toml, package.json, go.mod if omitted.
        #[arg(long)]
        template: Option<String>,
    },
    /// Run the constitution scanner against the project source (v0.13.9).
    ///
    /// Reads `.ta/constitution.toml` (or uses ta-default rules if not present).
    /// Checks each declared inject/restore function pair: if a file calls an
    /// inject function but not the paired restore function, it is flagged.
    /// Exit code 0 = clean, exit code 1 = violations when on_violation = "block".
    CheckToml {
        /// Output violations as JSON (machine-readable for CI).
        #[arg(long)]
        json: bool,
    },
    /// Deduplicate the project constitution via agent review (v0.14.6.1).
    ///
    /// Loads the effective rule set from `.ta/constitution.toml` (after `extends`
    /// inheritance), identifies duplicate and conflicting rules, proposes a
    /// deduplicated version, and packages it as a draft for human review via
    /// the standard `ta draft view / approve / apply` workflow.
    ///
    /// The review runs in two passes:
    ///   1. Rust-side exact dedup (hash-based, fast, no model needed).
    ///   2. Agent semantic pass (`claude --print`) for near-duplicates and
    ///      conflicting rules that differ in phrasing but enforce the same
    ///      or contradictory constraints. Skip with `--no-agent`.
    Review {
        /// Print the proposed changes without creating a draft.
        #[arg(long)]
        dry_run: bool,
        /// Override the model used for the semantic review pass.
        #[arg(long)]
        model: Option<String>,
        /// Skip the agent semantic review pass (exact dedup only).
        #[arg(long)]
        no_agent: bool,
    },
}

pub fn execute(command: &ConstitutionCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        ConstitutionCommands::Init {
            agent,
            from,
            non_interactive,
        } => run_init(config, agent, from.as_deref(), *non_interactive),
        ConstitutionCommands::Show => show_constitution(config),
        ConstitutionCommands::Check { draft_id } => check_constitution(config, draft_id.as_deref()),
        ConstitutionCommands::InitToml { template } => {
            init_toml(&config.workspace_root, template.as_deref())
        }
        ConstitutionCommands::CheckToml { json } => check_toml(&config.workspace_root, *json),
        ConstitutionCommands::Review {
            dry_run,
            model,
            no_agent,
        } => review_constitution(config, *dry_run, model.as_deref(), *no_agent),
    }
}

fn run_init(
    config: &GatewayConfig,
    agent: &str,
    from: Option<&Path>,
    non_interactive: bool,
) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    // Warn if constitution already exists.
    let constitution_path = ta_dir.join("constitution.md");
    if constitution_path.exists() {
        println!("Note: .ta/constitution.md already exists.");
        println!("      The agent will be asked to update it rather than create from scratch.");
        println!();
    }

    // Gather project context.
    let plan_content = read_file_if_exists(&project_root.join("PLAN.md"));
    let claude_md_content = read_file_if_exists(&project_root.join("CLAUDE.md"));
    let existing_constitution = read_file_if_exists(&constitution_path);

    let extra_content = if let Some(from_path) = from {
        let resolved = if from_path.is_absolute() {
            from_path.to_path_buf()
        } else {
            project_root.join(from_path)
        };
        if !resolved.exists() {
            anyhow::bail!(
                "Context document not found: {}\n\
                 Provide the full path to your guidelines, spec, or PRD.",
                resolved.display()
            );
        }
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", resolved.display(), e))?;
        Some((resolved.display().to_string(), content))
    } else {
        None
    };

    // Build the agent prompt.
    let objective = build_constitution_prompt(
        project_root,
        plan_content.as_deref(),
        claude_md_content.as_deref(),
        existing_constitution.as_deref(),
        extra_content
            .as_ref()
            .map(|(name, content)| (name.as_str(), content.as_str())),
        non_interactive,
    );

    let title = "Draft project behavioral constitution";

    println!("Launching constitution drafting session...");
    println!("  Target: .ta/constitution.md");
    if plan_content.is_some() {
        println!("  Context: PLAN.md found");
    }
    if claude_md_content.is_some() {
        println!("  Context: CLAUDE.md found");
    }
    if let Some((ref name, _)) = extra_content {
        println!("  Context: {}", name);
    }
    println!();

    super::run::execute(
        config,
        Some(title),
        agent,
        None, // source = project root (from config)
        &objective,
        None,             // no phase
        None,             // no follow_up
        None,             // follow_up_draft
        None,             // follow_up_goal
        None,             // no objective file
        false,            // no_launch = false
        !non_interactive, // interactive when not --non-interactive
        false,            // macro_goal = false
        None,             // resume
        false,            // headless = false
        false,            // skip_verify = false
        false,            // quiet = false
        None,             // existing_goal_id
        None,             // workflow = default (single-agent)
    )?;

    println!();
    if constitution_path.exists() {
        println!("Constitution drafted: .ta/constitution.md");
        println!("Review it, then commit it to make it part of your project.");
    } else {
        println!("The agent did not create .ta/constitution.md.");
        println!("Check the draft for details — the agent may have asked a follow-up question.");
    }

    Ok(())
}

fn build_constitution_prompt(
    project_root: &Path,
    plan: Option<&str>,
    claude_md: Option<&str>,
    existing: Option<&str>,
    extra: Option<(&str, &str)>,
    non_interactive: bool,
) -> String {
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("this project");

    let plan_section = plan
        .map(|p| format!("\n## Project Plan (PLAN.md)\n\n{}\n", truncate(p, 40_000)))
        .unwrap_or_default();

    let claude_section = claude_md
        .map(|c| {
            format!(
                "\n## Project Instructions (CLAUDE.md)\n\n{}\n",
                truncate(c, 20_000)
            )
        })
        .unwrap_or_default();

    let existing_section = existing.map(|e| format!(
        "\n## Existing Constitution (.ta/constitution.md)\n\nUpdate this rather than replace:\n\n{}\n",
        truncate(e, 20_000)
    )).unwrap_or_default();

    let extra_section = extra
        .map(|(name, content)| {
            format!(
                "\n## Additional Context ({})\n\n{}\n",
                name,
                truncate(content, 20_000)
            )
        })
        .unwrap_or_default();

    let interaction_style = if non_interactive {
        "Non-interactive mode: draft the constitution from the provided documents without \
         asking questions. Use reasonable defaults for anything not specified."
    } else {
        "Ask 2-3 focused questions using `ta_ask_human` to understand the project's key \
         behavioral requirements before drafting. Keep questions concise."
    };

    format!(
        r#"You are drafting a behavioral constitution for the project **{name}**.

A behavioral constitution is a Markdown document saved at `.ta/constitution.md` that defines:
- **Rules and invariants** the AI agent must never violate (e.g., "never commit directly to main")
- **Coding standards** specific to this project
- **Autonomy boundaries** — what the agent can decide alone vs. what needs human approval
- **Key patterns** the agent should follow (naming conventions, error handling, etc.)
- **Anti-patterns** to avoid based on past incidents or design decisions

## Your Task

{interaction_style}

Then write `.ta/constitution.md` with clear, actionable rules organized into sections:
1. **Core Invariants** — hard rules that must never be broken
2. **Development Standards** — coding, testing, commit, and PR standards
3. **Autonomy Policy** — when to ask vs. when to proceed
4. **Project-Specific Patterns** — naming, structure, tooling conventions
5. **Known Anti-Patterns** — things that have caused problems before

## Format

Use this structure:
```markdown
# {{Project Name}} — Agent Behavioral Constitution

## Core Invariants
<!-- Rules that must NEVER be violated -->
- **Rule**: Description of what must always/never happen and why.

## Development Standards
<!-- How to write code, tests, commits, PRs -->
- **Standard**: Specific requirement.

## Autonomy Policy
<!-- When to ask vs. proceed -->
- **Proceed without asking**: Description of low-risk actions.
- **Always ask first**: Description of high-risk or irreversible actions.

## Project-Specific Patterns
<!-- Conventions, tooling, naming -->
- **Pattern**: How and why.

## Known Anti-Patterns
<!-- Things to avoid, with brief incident note if applicable -->
- **Anti-pattern**: What not to do and why.
```

Write the constitution based on the project context below. Be specific — generic rules
("write good code") are not useful. Rules should be concrete enough that an AI agent
can check itself against them.
{plan_section}{claude_section}{existing_section}{extra_section}
## Instructions

1. Review the project context above.
2. {interaction_style}
3. Write `.ta/constitution.md` with the completed constitution.
4. Confirm what you wrote using `ta_ask_human` and ask if the human wants any changes.

The project name is: **{name}**"#,
        name = project_name,
        interaction_style = interaction_style,
        plan_section = plan_section,
        claude_section = claude_section,
        existing_section = existing_section,
        extra_section = extra_section,
    )
}

fn show_constitution(config: &GatewayConfig) -> anyhow::Result<()> {
    let path = config.workspace_root.join(".ta/constitution.md");
    if !path.exists() {
        println!("No .ta/constitution.md found.");
        println!();
        println!("Run `ta constitution init` to draft one.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Failed to read .ta/constitution.md: {}", e))?;

    println!("=== .ta/constitution.md ===");
    println!();
    println!("{}", content);
    Ok(())
}

fn check_constitution(config: &GatewayConfig, draft_id: Option<&str>) -> anyhow::Result<()> {
    use ta_submit::WorkflowConfig;

    let wf_path = config.workspace_root.join(".ta/workflow.toml");
    let wf_config = WorkflowConfig::load_or_default(&wf_path);

    if !wf_config.constitution.s4_scan && draft_id.is_none() {
        println!("Constitution checks:");
        println!("  s4_scan: disabled (set [constitution] s4_scan = true in .ta/workflow.toml to enable)");
        println!();
        println!("No checks configured for this project.");
        println!("More checks will be available in v0.14.1.");
        return Ok(());
    }

    println!("Constitution checks:");
    if wf_config.constitution.s4_scan {
        println!("  s4_scan: enabled — §4 inject/restore balance checked during `ta draft build`");
    }

    if let Some(id) = draft_id {
        // Run the check against a specific draft.
        let packages = super::draft::load_all_packages(config).unwrap_or_default();
        let pkg = packages.iter().find(|p| {
            p.package_id.to_string().starts_with(id)
                || p.display_id.as_deref().unwrap_or("").starts_with(id)
        });
        match pkg {
            Some(p) => {
                let goal_id_parsed = p.goal.goal_id.parse::<uuid::Uuid>().ok();
                let goal_store_res = ta_goal::GoalRunStore::new(&config.goals_dir);
                let goal_opt = goal_id_parsed
                    .zip(goal_store_res.ok())
                    .and_then(|(id, gs)| gs.get(id).ok().flatten());
                if let Some(goal) = goal_opt {
                    let warnings = super::draft::scan_s4_violations(
                        &p.changes.artifacts,
                        &goal.workspace_path,
                    );
                    if warnings.is_empty() {
                        println!("  §4 check: clean — no inject/restore imbalances found");
                    } else {
                        println!("  §4 check: {} warning(s):", warnings.len());
                        for w in &warnings {
                            println!("    {}", w.output);
                        }
                    }
                } else {
                    println!("  §4 check: could not load goal workspace — skipped");
                }
            }
            None => {
                anyhow::bail!(
                    "Draft '{}' not found. Run `ta draft list` to see available drafts.",
                    id
                );
            }
        }
    }

    Ok(())
}

// ── v0.13.9 — Project Constitution Framework ────────────────────────────────

/// A validation step that runs at a specific draft stage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ValidationStep {
    /// When this runs: "pre_draft_build" | "pre_draft_apply"
    pub stage: String,
    /// Commands to run in the staging directory.
    pub commands: Vec<String>,
    /// What to do on failure: "block" | "warn" | "ask_follow_up" | "auto_follow_up"
    #[serde(default = "default_on_failure")]
    pub on_failure: String,
}

fn default_on_failure() -> String {
    "warn".to_string()
}

/// A constitution rule (injection/cleanup pairs or error-path patterns).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConstitutionRule {
    /// Functions that inject context (must have corresponding restore calls).
    #[serde(default)]
    pub inject_fns: Vec<String>,
    /// Functions that restore context.
    #[serde(default)]
    pub restore_fns: Vec<String>,
    /// Code patterns that flag a violation when present without cleanup.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Severity of violations from this rule: "high" | "medium" | "low"
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_severity() -> String {
    "medium".to_string()
}

/// Scan configuration for constitution checks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConstitutionScan {
    /// File/dir patterns to include (relative to project root).
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    /// File/dir patterns to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// What to do on violation: "warn" | "block" | "off"
    #[serde(default = "default_on_violation")]
    pub on_violation: String,
}

fn default_include() -> Vec<String> {
    vec!["src/".to_string()]
}

fn default_on_violation() -> String {
    "warn".to_string()
}

impl Default for ConstitutionScan {
    fn default() -> Self {
        ConstitutionScan {
            include: default_include(),
            exclude: vec![],
            on_violation: default_on_violation(),
        }
    }
}

/// Release-time constitution settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConstitutionRelease {
    /// Whether to include a constitution compliance gate in the release pipeline.
    #[serde(default = "default_checklist_gate")]
    pub checklist_gate: bool,
    /// Whether to run a parallel agent constitution review during release.
    #[serde(default)]
    pub agent_review: bool,
}

fn default_checklist_gate() -> bool {
    true
}

/// Top-level `.ta/constitution.toml` configuration (v0.13.9).
///
/// Load with [`ProjectConstitutionConfig::load`] or use
/// [`ProjectConstitutionConfig::ta_default`] to get TA's built-in rule set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectConstitutionConfig {
    /// Optional parent profile to inherit from ("ta-default" is built-in).
    #[serde(default)]
    pub extends: Option<String>,
    /// Named rules keyed by rule name.
    #[serde(default)]
    pub rules: std::collections::HashMap<String, ConstitutionRule>,
    /// Scan configuration.
    #[serde(default)]
    pub scan: ConstitutionScan,
    /// Release configuration.
    #[serde(default)]
    pub release: ConstitutionRelease,
    /// Validation steps at draft stages.
    #[serde(default)]
    pub validate: Vec<ValidationStep>,
}

impl Default for ProjectConstitutionConfig {
    fn default() -> Self {
        ProjectConstitutionConfig {
            extends: None,
            rules: std::collections::HashMap::new(),
            scan: ConstitutionScan::default(),
            release: ConstitutionRelease {
                checklist_gate: true,
                agent_review: false,
            },
            validate: vec![],
        }
    }
}

impl ProjectConstitutionConfig {
    /// Load from `.ta/constitution.toml` in the project root.
    /// Returns `Ok(None)` if the file does not exist.
    ///
    /// If the loaded config declares `extends = "ta-default"`, the ta-default
    /// profile is merged in as a base: project rules are layered on top so that
    /// project-specific settings win while the base provides the standard rule set.
    pub fn load(project_root: &Path) -> anyhow::Result<Option<ProjectConstitutionConfig>> {
        let path = project_root.join(".ta/constitution.toml");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
        let mut config: ProjectConstitutionConfig = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("constitution.toml parse error: {}", e))?;

        // v0.13.15: Apply `extends` inheritance at load time.
        // Currently only "ta-default" is supported as a base profile.
        if config.extends.as_deref() == Some("ta-default") {
            config = apply_extends_ta_default(config);
        }

        Ok(Some(config))
    }

    /// Returns TA's built-in "ta-default" constitution config.
    ///
    /// This matches TA's own inject/restore conventions and serves as the
    /// starter template for `ta constitution init-toml`.
    pub fn ta_default() -> ProjectConstitutionConfig {
        let mut rules = std::collections::HashMap::new();
        rules.insert(
            "injection_cleanup".to_string(),
            ConstitutionRule {
                inject_fns: vec![
                    "inject_claude_md".to_string(),
                    "inject_credentials".to_string(),
                ],
                restore_fns: vec![
                    "restore_claude_md".to_string(),
                    "restore_credentials".to_string(),
                ],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        rules.insert(
            "error_paths".to_string(),
            ConstitutionRule {
                inject_fns: vec![],
                restore_fns: vec![],
                patterns: vec!["return Err(".to_string()],
                severity: "medium".to_string(),
            },
        );
        ProjectConstitutionConfig {
            extends: None,
            rules,
            scan: ConstitutionScan {
                include: vec!["src/".to_string()],
                exclude: vec!["src/tests/".to_string(), "target/".to_string()],
                on_violation: "warn".to_string(),
            },
            release: ConstitutionRelease {
                checklist_gate: true,
                agent_review: false,
            },
            validate: vec![
                ValidationStep {
                    stage: "pre_draft_build".to_string(),
                    commands: vec![
                        "cargo clippy --workspace --all-targets -- -D warnings".to_string()
                    ],
                    on_failure: "block".to_string(),
                },
                ValidationStep {
                    stage: "pre_draft_apply".to_string(),
                    commands: vec![
                        "cargo test --workspace".to_string(),
                        "cargo fmt --all -- --check".to_string(),
                    ],
                    on_failure: "warn".to_string(),
                },
            ],
        }
    }

    /// Returns all validate steps for a given stage name.
    #[allow(dead_code)]
    pub fn validate_steps_for_stage(&self, stage: &str) -> Vec<&ValidationStep> {
        self.validate.iter().filter(|v| v.stage == stage).collect()
    }
}

// ── v0.13.15: extends inheritance ────────────────────────────────────────────

/// Merge a project config on top of `ta-default`.
///
/// Merge strategy:
/// - `rules`: base rules included by default; project rules override/extend them.
/// - `scan.include/exclude`: project values win if non-empty; else base values.
/// - `scan.on_violation`: project value wins.
/// - `release`: project values win.
/// - `validate`: project steps replace base steps entirely (project knows best).
/// - `extends`: cleared after merging (no double-inheritance).
fn apply_extends_ta_default(project: ProjectConstitutionConfig) -> ProjectConstitutionConfig {
    let base = ProjectConstitutionConfig::ta_default();

    // Merge rules: base first, project rules override by key.
    let mut merged_rules = base.rules;
    for (k, v) in project.rules {
        merged_rules.insert(k, v);
    }

    // scan: project wins if its include is non-empty; otherwise inherit base.
    let merged_scan = if !project.scan.include.is_empty() {
        project.scan
    } else {
        let mut scan = base.scan;
        scan.on_violation = project.scan.on_violation;
        scan
    };

    // validate: project steps replace base entirely when non-empty.
    let merged_validate = if !project.validate.is_empty() {
        project.validate
    } else {
        base.validate
    };

    ProjectConstitutionConfig {
        extends: None, // consumed
        rules: merged_rules,
        scan: merged_scan,
        release: project.release,
        validate: merged_validate,
    }
}

// ── Scanner ──────────────────────────────────────────────────────────────────

/// A single constitution violation found during a scan.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConstitutionViolation {
    /// Project-relative file path.
    pub file: String,
    /// Line number of the inject call (1-based).
    pub line: usize,
    /// Name of the rule that was violated.
    pub rule: String,
    /// Human-readable description of the violation.
    pub message: String,
    /// Severity: "high" | "medium" | "low"
    pub severity: String,
}

/// Scan `scan_root` for constitution violations declared in `config`.
///
/// For each rule with `inject_fns`/`restore_fns` pairs, any file that calls
/// an inject function but does not call the corresponding restore function is
/// flagged as a violation. The check is file-scoped (not function-scoped) to
/// keep the scanner fast and dependency-free.
pub fn scan_for_violations(
    scan_root: &Path,
    config: &ProjectConstitutionConfig,
) -> anyhow::Result<Vec<ConstitutionViolation>> {
    let mut violations = Vec::new();

    if config.scan.on_violation == "off" {
        return Ok(violations);
    }

    let files = collect_scan_files(scan_root, &config.scan)?;

    for file_path in &files {
        let relative = file_path
            .strip_prefix(scan_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            // Skip unreadable files (binary, permission-denied, etc.)
            Err(_) => continue,
        };

        for (rule_name, rule) in &config.rules {
            // Check each inject/restore pair.
            for (inject_fn, restore_fn) in rule.inject_fns.iter().zip(rule.restore_fns.iter()) {
                if content.contains(inject_fn.as_str()) && !content.contains(restore_fn.as_str()) {
                    let line_num = content
                        .lines()
                        .enumerate()
                        .find(|(_, l)| l.contains(inject_fn.as_str()))
                        .map(|(i, _)| i + 1)
                        .unwrap_or(0);
                    violations.push(ConstitutionViolation {
                        file: relative.clone(),
                        line: line_num,
                        rule: rule_name.clone(),
                        message: format!(
                            "'{}' is called but '{}' is not present in this file",
                            inject_fn, restore_fn
                        ),
                        severity: rule.severity.clone(),
                    });
                }
            }
        }
    }

    Ok(violations)
}

/// Collect all `.rs` files under `root` matching the include/exclude patterns.
fn collect_scan_files(
    root: &Path,
    scan: &ConstitutionScan,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for pattern in &scan.include {
        // Build a glob: <root>/<include_dir>/**/*.rs
        let base = root.join(pattern);
        let pattern_str = format!("{}/**/*.rs", base.to_string_lossy());
        match glob::glob(&pattern_str) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if entry.is_file() {
                        let rel = entry.strip_prefix(root).unwrap_or(&entry);
                        // Normalize to forward slashes so exclude patterns work on all platforms.
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let excluded = scan.exclude.iter().any(|ex| rel_str.contains(ex.as_str()));
                        if !excluded {
                            files.push(entry);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(pattern = %pattern_str, error = %e, "constitution scan: glob error");
            }
        }
    }
    Ok(files)
}

// ── CLI handlers for v0.13.9 commands ────────────────────────────────────────

fn init_toml(project_root: &Path, template: Option<&str>) -> anyhow::Result<()> {
    let path = project_root.join(".ta/constitution.toml");
    if path.exists() {
        println!("constitution.toml already exists at {}.", path.display());
        println!("  Delete it first if you want to regenerate.");
        return Ok(());
    }

    // Resolve template: explicit > auto-detected > rust (default).
    let lang = template
        .map(|s| s.to_string())
        .unwrap_or_else(|| detect_constitution_language(project_root));

    let config = constitution_template_for_language(&lang);
    let toml_str = toml::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize constitution config: {}", e))?;
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("constitution.toml has no parent directory"))?,
    )?;
    std::fs::write(&path, toml_str)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    println!("Created {} (template: {}).", path.display(), lang);
    println!("  Edit to define your project's invariants.");
    println!("  Run `ta constitution check-toml` to validate.");
    Ok(())
}

/// Auto-detect the project language from filesystem signals.
fn detect_constitution_language(project_root: &Path) -> String {
    if project_root.join("Cargo.toml").exists() {
        return "rust".to_string();
    }
    if project_root.join("pyproject.toml").exists()
        || project_root.join("setup.py").exists()
        || project_root.join("requirements.txt").exists()
    {
        return "python".to_string();
    }
    if project_root.join("package.json").exists() {
        // Distinguish TypeScript vs plain Node.js by tsconfig presence.
        if project_root.join("tsconfig.json").exists() {
            return "typescript".to_string();
        }
        return "nodejs".to_string();
    }
    if project_root.join("go.mod").exists() {
        return "go".to_string();
    }
    "generic".to_string()
}

/// Build a language-specific ProjectConstitutionConfig.
///
/// Each template inherits TA's default injection/cleanup rules and adds
/// language-specific validate steps appropriate for the ecosystem.
fn constitution_template_for_language(lang: &str) -> ProjectConstitutionConfig {
    let mut config = ProjectConstitutionConfig::ta_default();
    // All templates extend ta-default (inheritance stub — field stored, applied at load time).
    config.extends = Some("ta-default".to_string());

    match lang {
        "python" => {
            config.scan.include = vec!["src/".to_string()];
            config.validate = vec![
                ValidationStep {
                    stage: "pre_draft_build".to_string(),
                    commands: vec!["ruff check .".to_string(), "mypy src/".to_string()],
                    on_failure: "block".to_string(),
                },
                ValidationStep {
                    stage: "pre_draft_apply".to_string(),
                    commands: vec!["pytest".to_string()],
                    on_failure: "warn".to_string(),
                },
            ];
        }
        "typescript" | "nodejs" => {
            config.scan.include = vec!["src/".to_string()];
            config.scan.exclude = vec!["node_modules/".to_string(), "dist/".to_string()];
            let check_cmd = if lang == "typescript" {
                "npm run typecheck"
            } else {
                "node --check src/index.js"
            };
            config.validate = vec![
                ValidationStep {
                    stage: "pre_draft_build".to_string(),
                    commands: vec![check_cmd.to_string(), "npm run lint".to_string()],
                    on_failure: "block".to_string(),
                },
                ValidationStep {
                    stage: "pre_draft_apply".to_string(),
                    commands: vec!["npm test".to_string()],
                    on_failure: "warn".to_string(),
                },
            ];
        }
        "go" => {
            config.scan.include = vec![".".to_string()];
            config.validate = vec![
                ValidationStep {
                    stage: "pre_draft_build".to_string(),
                    commands: vec!["go vet ./...".to_string()],
                    on_failure: "block".to_string(),
                },
                ValidationStep {
                    stage: "pre_draft_apply".to_string(),
                    commands: vec!["go test ./...".to_string()],
                    on_failure: "warn".to_string(),
                },
            ];
        }
        "generic" => {
            config.validate = vec![];
        }
        _ => {} // "rust" — ta_default() already has the right validate steps
    }
    config
}

fn check_toml(project_root: &Path, json: bool) -> anyhow::Result<()> {
    let config = ProjectConstitutionConfig::load(project_root)?.unwrap_or_else(|| {
        println!("No .ta/constitution.toml found — using ta-default rules.");
        ProjectConstitutionConfig::ta_default()
    });

    let violations = scan_for_violations(project_root, &config)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&violations)?);
    } else if violations.is_empty() {
        println!("Constitution check passed — no violations found.");
    } else {
        for v in &violations {
            println!(
                "[{}] {}:{} — {} (rule: {})",
                v.severity.to_uppercase(),
                v.file,
                v.line,
                v.message,
                v.rule
            );
        }
        println!("\n{} violation(s) found.", violations.len());
        if config.scan.on_violation == "block" {
            std::process::exit(1);
        }
    }
    Ok(())
}

// ── v0.14.6.1 — Constitution Deduplication via Agent Review ─────────────────

/// Agent-returned semantic review response.
#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct AgentReviewResponse {
    /// Pairs of rules that are semantically equivalent or near-duplicate.
    #[serde(default)]
    pub duplicates: Vec<SemanticDuplicate>,
    /// Pairs of rules that conflict with each other.
    #[serde(default)]
    pub conflicts: Vec<SemanticConflict>,
}

/// A pair of rules identified as semantic duplicates by the agent.
#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SemanticDuplicate {
    /// Name of the first rule.
    pub rule_a: String,
    /// Name of the second rule.
    pub rule_b: String,
    /// The canonical name to keep (should be one of rule_a or rule_b).
    pub canonical: String,
}

/// A pair of rules identified as conflicting by the agent.
#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SemanticConflict {
    /// Name of the first rule.
    pub rule_a: String,
    /// Name of the second rule.
    pub rule_b: String,
    /// Agent's recommendation for resolving the conflict.
    pub recommendation: String,
}

/// Statistics from the deduplication pass.
#[derive(Debug)]
struct DeduplicationStats {
    rules_before: usize,
    rules_after: usize,
    exact_removed: usize,
    semantic_removed: usize,
    conflicts: usize,
}

/// Main handler for `ta constitution review`.
fn review_constitution(
    config: &GatewayConfig,
    dry_run: bool,
    model: Option<&str>,
    no_agent: bool,
) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let toml_path = project_root.join(".ta/constitution.toml");

    // 1. Load the effective rule set (with extends inheritance).
    let (effective_config, original_toml) = if toml_path.exists() {
        let raw = ProjectConstitutionConfig::load(project_root)?
            .expect("load returned None but file exists");
        let original = std::fs::read_to_string(&toml_path)
            .map_err(|e| anyhow::anyhow!("Failed to read .ta/constitution.toml: {}", e))?;
        (raw, original)
    } else {
        println!("No .ta/constitution.toml found — using ta-default as the baseline.");
        println!("Run `ta constitution init-toml` to create a project constitution first.");
        println!();
        // Use ta-default as the effective config; original is empty (new file).
        (ProjectConstitutionConfig::ta_default(), String::new())
    };

    let rules_before = effective_config.rules.len();
    println!("Constitution review:");
    println!("  Rules loaded: {}", rules_before);

    // 2. Exact duplicate detection (Rust-side, no model needed).
    let exact_dups = detect_exact_duplicates(&effective_config.rules);
    if exact_dups.is_empty() {
        println!("  Exact duplicates: none");
    } else {
        println!("  Exact duplicates: {} pair(s)", exact_dups.len());
        for (a, b) in &exact_dups {
            println!("    • \"{}\" ≡ \"{}\"", a, b);
        }
    }

    // 3. Agent semantic review pass (optional).
    let agent_response = if no_agent {
        println!("  Semantic review: skipped (--no-agent)");
        None
    } else {
        print!("  Semantic review: ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        match try_agent_review(&effective_config.rules, model) {
            Some(resp) => {
                let sem_dups = resp.duplicates.len();
                let conflicts = resp.conflicts.len();
                println!(
                    "done ({} semantic duplicate(s), {} conflict(s))",
                    sem_dups, conflicts
                );
                Some(resp)
            }
            None => {
                println!("skipped (claude not available or returned invalid JSON)");
                None
            }
        }
    };

    // 4. Generate merged TOML.
    let (merged_toml, stats) =
        generate_merged_toml(&effective_config, &exact_dups, agent_response.as_ref());

    println!();
    println!("Deduplication summary:");
    println!("  Rules before:  {}", stats.rules_before);
    println!("  Rules after:   {}", stats.rules_after);
    if stats.exact_removed > 0 {
        println!("  Exact removed: {}", stats.exact_removed);
    }
    if stats.semantic_removed > 0 {
        println!("  Semantic removed: {}", stats.semantic_removed);
    }
    if stats.conflicts > 0 {
        println!("  Conflicts flagged: {}", stats.conflicts);
    }

    if stats.rules_before == stats.rules_after && stats.conflicts == 0 {
        println!();
        println!("Constitution is already clean — no duplicates or conflicts found.");
        if dry_run {
            return Ok(());
        }
        // Still create a draft so the user can confirm the review ran.
    }

    if dry_run {
        println!();
        println!("--- Proposed .ta/constitution.toml ---");
        println!();
        println!("{}", merged_toml);
        return Ok(());
    }

    // 5. Create draft artifact.
    println!();
    print!("Creating draft... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let package_id = create_review_draft(config, &original_toml, &merged_toml, &stats)?;
    println!("done");
    println!();
    println!("Draft created: {}", &package_id.to_string()[..8]);
    println!(
        "  ta draft view {} — review the proposed constitution diff",
        &package_id.to_string()[..8]
    );
    println!(
        "  ta draft approve {} — approve the deduplication",
        &package_id.to_string()[..8]
    );
    println!(
        "  ta draft apply {} — write the deduplicated .ta/constitution.toml",
        &package_id.to_string()[..8]
    );

    Ok(())
}

/// Detect exact duplicate rules: two rules with identical inject_fns, restore_fns,
/// and patterns (order-independent). Returns pairs of (rule_name_a, rule_name_b)
/// where rule_a < rule_b lexicographically.
pub fn detect_exact_duplicates(rules: &HashMap<String, ConstitutionRule>) -> Vec<(String, String)> {
    // Build a fingerprint for each rule: sorted lists joined into a canonical string.
    let mut fingerprint_to_names: HashMap<String, Vec<String>> = HashMap::new();

    for (name, rule) in rules {
        let fp = rule_fingerprint(rule);
        fingerprint_to_names
            .entry(fp)
            .or_default()
            .push(name.clone());
    }

    let mut pairs = Vec::new();
    for names in fingerprint_to_names.values() {
        if names.len() >= 2 {
            let mut sorted = names.clone();
            sorted.sort();
            // Emit all (a, b) pairs where a < b.
            for i in 0..sorted.len() {
                for j in (i + 1)..sorted.len() {
                    pairs.push((sorted[i].clone(), sorted[j].clone()));
                }
            }
        }
    }

    pairs.sort();
    pairs
}

/// Compute a canonical fingerprint for a ConstitutionRule.
/// Two rules with the same fingerprint are content-identical.
fn rule_fingerprint(rule: &ConstitutionRule) -> String {
    let mut inject = rule.inject_fns.clone();
    inject.sort();
    let mut restore = rule.restore_fns.clone();
    restore.sort();
    let mut patterns = rule.patterns.clone();
    patterns.sort();
    format!(
        "inject:{:?}|restore:{:?}|patterns:{:?}|severity:{}",
        inject, restore, patterns, rule.severity
    )
}

/// Call `claude --print` for a short semantic review of the rules.
///
/// Sends all rule names and their content to the model, asks for JSON output
/// identifying semantic duplicates and conflicts. Returns `None` if the model
/// is not available, the call fails, or the response is not valid JSON.
pub fn try_agent_review(
    rules: &HashMap<String, ConstitutionRule>,
    model: Option<&str>,
) -> Option<AgentReviewResponse> {
    if rules.is_empty() {
        return Some(AgentReviewResponse {
            duplicates: vec![],
            conflicts: vec![],
        });
    }

    // Serialize rules to JSON for the prompt.
    let rules_json = serde_json::to_string_pretty(rules).ok()?;

    let prompt = format!(
        "You are reviewing a project constitution — a set of named rules that govern AI agent \
         behavior. Each rule has inject_fns, restore_fns, patterns, and a severity.\n\
         \n\
         Identify:\n\
         1. **Semantic near-duplicates**: two rules that enforce the same constraint with \
            different names or slightly different content. These can be merged.\n\
         2. **Conflicts**: two rules that cannot both be satisfied simultaneously.\n\
         \n\
         Rules (JSON):\n\
         {rules_json}\n\
         \n\
         Respond ONLY with valid JSON in EXACTLY this format — no prose, no markdown fencing:\n\
         {{\"duplicates\":[{{\"rule_a\":\"name1\",\"rule_b\":\"name2\",\"canonical\":\"preferred_name\"}}],\
         \"conflicts\":[{{\"rule_a\":\"name1\",\"rule_b\":\"name2\",\"recommendation\":\"how to resolve\"}}]}}\n\
         \n\
         Use the EXACT rule names from the input. If there are no duplicates or conflicts, \
         return empty arrays. Only flag rules that are clearly redundant or contradictory.",
        rules_json = rules_json,
    );

    let mut args = vec!["--print".to_string()];
    if let Some(m) = model {
        args.push(format!("--model={}", m));
    }
    args.push(prompt);

    let output = Command::new("claude")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();

    // Strip markdown code fences if the model wrapped the JSON.
    let json_str = if trimmed.starts_with("```") {
        trimmed
            .lines()
            .skip(1)
            .take_while(|l| !l.starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        trimmed.to_string()
    };

    // Find the JSON object within the response (model may add prose).
    let json_start = json_str.find('{');
    let json_end = json_str.rfind('}');
    if let (Some(start), Some(end)) = (json_start, json_end) {
        let candidate = &json_str[start..=end];
        serde_json::from_str(candidate).ok()
    } else {
        None
    }
}

/// Generate the merged constitution TOML content with dedup annotations.
///
/// Returns (toml_string, stats). The merged config removes exact-duplicate rules
/// (keeping the lexicographically first name) and semantic duplicates flagged by
/// the agent (keeping the canonical). Conflicts are annotated with a comment.
///
/// The `extends` field is preserved from the original effective config.
fn generate_merged_toml(
    config: &ProjectConstitutionConfig,
    exact_dups: &[(String, String)],
    agent_response: Option<&AgentReviewResponse>,
) -> (String, DeduplicationStats) {
    let rules_before = config.rules.len();

    // Build the set of rules to remove (keep lexicographically first of each pair).
    let mut remove: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merge_comments: HashMap<String, String> = HashMap::new(); // canonical → comment

    // Exact duplicates: keep the lex-first name, remove the rest.
    for (a, b) in exact_dups {
        // a < b by construction from detect_exact_duplicates.
        remove.insert(b.clone());
        merge_comments
            .entry(a.clone())
            .or_insert_with(|| format!("# merged from: {}, {}", a, b));
    }

    // Semantic duplicates from agent: keep `canonical`, remove the other.
    let mut semantic_removed = 0usize;
    let mut conflict_comments: HashMap<String, String> = HashMap::new();

    if let Some(resp) = agent_response {
        for dup in &resp.duplicates {
            // Only remove if both rules actually exist and canonical is one of them.
            if config.rules.contains_key(&dup.rule_a)
                && config.rules.contains_key(&dup.rule_b)
                && (dup.canonical == dup.rule_a || dup.canonical == dup.rule_b)
            {
                let to_remove = if dup.canonical == dup.rule_a {
                    &dup.rule_b
                } else {
                    &dup.rule_a
                };
                if !remove.contains(to_remove) {
                    remove.insert(to_remove.clone());
                    semantic_removed += 1;
                    merge_comments
                        .entry(dup.canonical.clone())
                        .or_insert_with(|| {
                            format!("# merged from: {}, {}", dup.rule_a, dup.rule_b)
                        });
                }
            }
        }

        for conflict in &resp.conflicts {
            if config.rules.contains_key(&conflict.rule_a)
                && config.rules.contains_key(&conflict.rule_b)
            {
                let comment = format!(
                    "# CONFLICT: {} vs {} — {}",
                    conflict.rule_a, conflict.rule_b, conflict.recommendation
                );
                conflict_comments
                    .entry(conflict.rule_a.clone())
                    .or_insert(comment.clone());
                conflict_comments
                    .entry(conflict.rule_b.clone())
                    .or_insert(comment);
            }
        }
    }

    let exact_removed = exact_dups.len();
    let _num_conflicts = conflict_comments.len() / 2; // each conflict annotates 2 rules (unused here)

    // Build deduplicated rule set.
    let mut merged_rules: HashMap<String, ConstitutionRule> = HashMap::new();
    for (name, rule) in &config.rules {
        if !remove.contains(name) {
            merged_rules.insert(name.clone(), rule.clone());
        }
    }

    let rules_after = merged_rules.len();

    // Build the merged config.
    let merged_config = ProjectConstitutionConfig {
        extends: config.extends.clone(),
        rules: merged_rules.clone(),
        scan: config.scan.clone(),
        release: config.release.clone(),
        validate: config.validate.clone(),
    };

    // Serialize base TOML.
    let base_toml = toml::to_string_pretty(&merged_config)
        .unwrap_or_else(|e| format!("# ERROR: failed to serialize merged constitution: {}\n", e));

    // Insert per-rule comments into the TOML string.
    // Look for `[rules.<name>]` sections and prepend the comment.
    let mut annotated = base_toml.clone();
    let mut rule_names: Vec<&str> = merged_rules.keys().map(|s| s.as_str()).collect();
    rule_names.sort();
    for name in rule_names {
        let section_header = format!("[rules.{}]", name);
        let mut comment = String::new();
        if let Some(merge_cmt) = merge_comments.get(name) {
            comment.push_str(merge_cmt);
            comment.push('\n');
        }
        if let Some(conflict_cmt) = conflict_comments.get(name) {
            comment.push_str(conflict_cmt);
            comment.push('\n');
        }
        if !comment.is_empty() {
            annotated =
                annotated.replace(&section_header, &format!("{}{}", comment, section_header));
        }
    }

    // Prepend header comment.
    let conflicts_total = if let Some(resp) = agent_response {
        resp.conflicts.len()
    } else {
        0
    };
    let header = format!(
        "# Generated by `ta constitution review`\n\
         # Rules before: {rules_before}  Rules after: {rules_after}\n\
         # Exact duplicates removed: {exact_removed}  Semantic: {semantic_removed}\n\
         # Conflicts flagged: {conflicts_total}\n\
         # Apply via: ta draft approve <id> && ta draft apply <id>\n\n",
        rules_before = rules_before,
        rules_after = rules_after,
        exact_removed = exact_removed,
        semantic_removed = semantic_removed,
        conflicts_total = conflicts_total,
    );

    let final_toml = format!("{}{}", header, annotated);

    let stats = DeduplicationStats {
        rules_before,
        rules_after,
        exact_removed,
        semantic_removed,
        conflicts: conflicts_total,
    };

    (final_toml, stats)
}

/// Compute a simple unified diff between two strings.
///
/// Returns a diff string in unified diff format. If the strings are equal,
/// returns an empty string.
fn constitution_unified_diff(path: &str, original: &str, modified: &str) -> String {
    if original == modified {
        return String::new();
    }
    let mut output = String::new();
    output.push_str(&format!("--- a/{}\n", path));
    output.push_str(&format!("+++ b/{}\n", path));

    let orig_lines: Vec<&str> = original.lines().collect();
    let mod_lines: Vec<&str> = modified.lines().collect();

    output.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        1,
        orig_lines.len(),
        1,
        mod_lines.len()
    ));
    for line in &orig_lines {
        output.push_str(&format!("-{}\n", line));
    }
    for line in &mod_lines {
        output.push_str(&format!("+{}\n", line));
    }

    output
}

/// Create a draft package for the constitution review.
///
/// Uses the legacy MCP-based apply path (GoalRun.source_dir = None) so that
/// `.ta/constitution.toml` is copied directly from staging without going through
/// the overlay diff that excludes `.ta/`.
///
/// Returns the package UUID.
fn create_review_draft(
    config: &GatewayConfig,
    original_toml: &str,
    merged_toml: &str,
    stats: &DeduplicationStats,
) -> anyhow::Result<Uuid> {
    use ta_changeset::changeset::{ChangeKind, ChangeSet, CommitIntent};
    use ta_changeset::diff::DiffContent;
    use ta_changeset::draft_package::{
        AgentIdentity, Artifact, ChangeType, Changes, DraftPackage, DraftStatus, Goal, Iteration,
        Plan, Provenance, ReviewRequests, Risk, Signatures, Summary, WorkspaceRef,
    };
    use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
    use ta_workspace::ChangeStore;
    use ta_workspace::JsonFileStore;

    let review_id = Uuid::new_v4();
    let review_id_str = review_id.to_string();
    let now = Utc::now();

    // Staging workspace: write the merged constitution.toml.
    // The legacy MCP apply path uses:
    //   StagingWorkspace::new(goal_run_id, &config.staging_dir)
    // which creates config.staging_dir/<goal_run_id>/ as the staging dir.
    // FsConnector::apply copies all files from that dir to target_dir.
    let staging_dir = config.staging_dir.join(&review_id_str);
    let ta_staging = staging_dir.join(".ta");
    std::fs::create_dir_all(&ta_staging).map_err(|e| {
        anyhow::anyhow!(
            "Failed to create staging directory {}: {}",
            ta_staging.display(),
            e
        )
    })?;
    std::fs::write(ta_staging.join("constitution.toml"), merged_toml)
        .map_err(|e| anyhow::anyhow!("Failed to write merged constitution to staging: {}", e))?;

    // Changeset: record the diff for `ta draft view`.
    // Store path: config.store_dir/<review_id>/
    let store_path = config.store_dir.join(&review_id_str);
    std::fs::create_dir_all(&store_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to create store directory {}: {}",
            store_path.display(),
            e
        )
    })?;

    let diff_text = constitution_unified_diff(".ta/constitution.toml", original_toml, merged_toml);
    let change_type = if original_toml.is_empty() {
        ChangeType::Add
    } else {
        ChangeType::Modify
    };
    let diff_content = if original_toml.is_empty() {
        DiffContent::CreateFile {
            content: merged_toml.to_string(),
        }
    } else {
        DiffContent::UnifiedDiff { content: diff_text }
    };

    let changeset = ChangeSet::new(
        "fs://workspace/.ta/constitution.toml".to_string(),
        ChangeKind::FsPatch,
        diff_content,
    )
    .with_commit_intent(CommitIntent::RequestCommit);

    let mut cs_store = JsonFileStore::new(&store_path)
        .map_err(|e| anyhow::anyhow!("Failed to open changeset store: {}", e))?;
    cs_store
        .save(&review_id_str, &changeset)
        .map_err(|e| anyhow::anyhow!("Failed to save changeset: {}", e))?;

    // GoalRun record (legacy/MCP-based: source_dir = None).
    let goal_run = GoalRun {
        goal_run_id: review_id,
        tag: Some(format!("constitution-review-{}", &review_id_str[..8])),
        title: "Constitution Deduplication Review".to_string(),
        objective: format!(
            "Deduplicate .ta/constitution.toml: {} rule(s) → {} rule(s) \
             ({} exact duplicate(s) removed, {} conflict(s) flagged)",
            stats.rules_before, stats.rules_after, stats.exact_removed, stats.conflicts,
        ),
        agent_id: "ta-constitution-review".to_string(),
        state: GoalRunState::Running,
        manifest_id: Uuid::new_v4(),
        workspace_path: staging_dir.clone(),
        store_path: store_path.clone(),
        source_dir: None, // legacy path — no overlay diff
        plan_phase: None,
        parent_goal_id: None,
        source_snapshot: None,
        is_macro: false,
        parent_macro_id: None,
        sub_goal_ids: vec![],
        workflow_id: None,
        stage: None,
        role: None,
        context_from: vec![],
        thread_id: None,
        project_name: config
            .workspace_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string()),
        agent_pid: None,
        heartbeat_required: false,
        pr_url: None,
        pr_package_id: None,
        progress_note: None,
        vcs_isolation: None,
        initiated_by: None,
        created_at: now,
        updated_at: now,
    };

    let goal_store = GoalRunStore::new(&config.goals_dir)
        .map_err(|e| anyhow::anyhow!("Failed to open goal store: {}", e))?;
    goal_store
        .save(&goal_run)
        .map_err(|e| anyhow::anyhow!("Failed to save goal run: {}", e))?;

    // Build the DraftPackage.
    let package_id = Uuid::new_v4();

    let pkg = DraftPackage {
        package_version: "1.0.0".to_string(),
        package_id,
        created_at: now,
        goal: Goal {
            goal_id: review_id_str.clone(),
            title: "Constitution Deduplication Review".to_string(),
            objective: goal_run.objective.clone(),
            success_criteria: vec![format!(
                "Deduplicated .ta/constitution.toml to {} rule(s)",
                stats.rules_after
            )],
            constraints: vec![],
            parent_goal_title: None,
        },
        iteration: Iteration {
            iteration_id: Uuid::new_v4().to_string(),
            sequence: 1,
            workspace_ref: WorkspaceRef {
                ref_type: "constitution_review".to_string(),
                ref_name: staging_dir.to_string_lossy().to_string(),
                base_ref: None,
            },
        },
        agent_identity: AgentIdentity {
            agent_id: "ta-constitution-review".to_string(),
            agent_type: "constitution-review".to_string(),
            constitution_id: "ta-default".to_string(),
            capability_manifest_hash: "constitution-review".to_string(),
            orchestrator_run_id: None,
        },
        summary: Summary {
            what_changed: format!(
                "Deduplicated .ta/constitution.toml: {} → {} rule(s)",
                stats.rules_before, stats.rules_after
            ),
            why: format!(
                "Removed {} exact duplicate(s) and {} semantic duplicate(s). \
                 {} conflict(s) flagged for human review.",
                stats.exact_removed, stats.semantic_removed, stats.conflicts
            ),
            impact: "Constitution rule set is smaller and consistent. \
                     No behavioral change unless conflicting rules are resolved."
                .to_string(),
            rollback_plan: "Deny this draft — no changes are applied until approved.".to_string(),
            open_questions: vec![],
            alternatives_considered: vec![],
        },
        plan: Plan {
            completed_steps: vec![
                format!("Exact deduplication: {} pair(s) found", stats.exact_removed),
                if stats.semantic_removed > 0 {
                    format!(
                        "Semantic review: {} near-duplicate(s) found",
                        stats.semantic_removed
                    )
                } else {
                    "Semantic review: no near-duplicates found".to_string()
                },
            ],
            next_steps: if stats.conflicts > 0 {
                vec![format!(
                    "Resolve {} conflict(s) flagged with # CONFLICT: comments in the diff",
                    stats.conflicts
                )]
            } else {
                vec![]
            },
            decision_log: vec![],
        },
        changes: Changes {
            artifacts: vec![Artifact {
                resource_uri: "fs://workspace/.ta/constitution.toml".to_string(),
                change_type,
                diff_ref: "changeset:0".to_string(),
                tests_run: vec![],
                disposition: Default::default(),
                rationale: Some(format!(
                    "Deduplicated from {} to {} rule(s). \
                     Generated by `ta constitution review`.",
                    stats.rules_before, stats.rules_after
                )),
                dependencies: vec![],
                explanation_tiers: None,
                comments: None,
                amendment: None,
            }],
            patch_sets: vec![],
            pending_actions: vec![],
        },
        risk: Risk {
            risk_score: 5,
            findings: vec![],
            policy_decisions: vec![],
        },
        provenance: Provenance {
            inputs: vec![],
            tool_trace_hash: "constitution-review".to_string(),
        },
        review_requests: ReviewRequests {
            requested_actions: vec![],
            reviewers: vec![],
            required_approvals: 1,
            notes_to_reviewer: if stats.conflicts > 0 {
                Some(format!(
                    "{} conflict(s) flagged with # CONFLICT: comments — \
                     resolve these before applying.",
                    stats.conflicts
                ))
            } else {
                None
            },
        },
        signatures: Signatures {
            package_hash: "constitution-review".to_string(),
            agent_signature: "constitution-review".to_string(),
            gateway_attestation: None,
        },
        status: DraftStatus::PendingReview,
        verification_warnings: vec![],
        validation_log: vec![],
        display_id: Some(format!("{}-01", &review_id_str[..8])),
        tag: Some(format!("constitution-review-{}", &review_id_str[..8])),
        vcs_status: None,
        parent_draft_id: None,
        pending_approvals: vec![],
        supervisor_review: None,
        ignored_artifacts: vec![],
        baseline_artifacts: vec![],
        agent_decision_log: vec![],
    };

    super::draft::save_package(config, &pkg)
        .map_err(|e| anyhow::anyhow!("Failed to save draft package: {}", e))?;

    // Update GoalRun to PrReady with the package ID.
    let mut updated_goal = goal_run;
    updated_goal.state = GoalRunState::PrReady;
    updated_goal.pr_package_id = Some(package_id);
    updated_goal.updated_at = Utc::now();
    goal_store
        .save(&updated_goal)
        .map_err(|e| anyhow::anyhow!("Failed to update goal run with package ID: {}", e))?;

    Ok(package_id)
}

fn read_file_if_exists(path: &Path) -> Option<String> {
    if path.exists() {
        std::fs::read_to_string(path).ok()
    } else {
        None
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_constitution_prompt_basic() {
        let dir = TempDir::new().unwrap();
        let prompt = build_constitution_prompt(dir.path(), None, None, None, None, false);
        assert!(prompt.contains("constitution.md"));
        assert!(prompt.contains("ta_ask_human"));
        assert!(prompt.contains("Core Invariants"));
    }

    #[test]
    fn build_constitution_prompt_with_plan() {
        let dir = TempDir::new().unwrap();
        let plan = "# My Project\n## Phase v0.1.0\nBootstrap.";
        let prompt = build_constitution_prompt(dir.path(), Some(plan), None, None, None, false);
        assert!(prompt.contains("PLAN.md"));
        assert!(prompt.contains("Bootstrap"));
    }

    #[test]
    fn build_constitution_prompt_non_interactive() {
        let dir = TempDir::new().unwrap();
        let prompt = build_constitution_prompt(dir.path(), None, None, None, None, true);
        assert!(prompt.contains("Non-interactive mode"));
        assert!(!prompt.contains("Ask 2-3"));
    }

    #[test]
    fn show_constitution_no_file() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic — just prints "not found" message.
        let result = show_constitution(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn show_constitution_with_file() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("constitution.md"),
            "# My Project Constitution\n## Core Invariants\n- Never delete main.\n",
        )
        .unwrap();

        let config = GatewayConfig::for_project(dir.path());
        let result = show_constitution(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn check_constitution_no_config() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // No workflow.toml → defaults (s4_scan = false) → prints info and returns Ok.
        let result = check_constitution(&config, None);
        assert!(result.is_ok());
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let s = "a".repeat(200);
        assert_eq!(truncate(&s, 100).len(), 100);
    }

    // ── v0.13.9 tests ────────────────────────────────────────────────────────

    #[test]
    fn project_constitution_config_ta_default_has_expected_rules() {
        let config = ProjectConstitutionConfig::ta_default();
        assert!(config.rules.contains_key("injection_cleanup"));
        assert!(config.rules.contains_key("error_paths"));
        let rule = config.rules.get("injection_cleanup").unwrap();
        assert_eq!(rule.severity, "high");
        assert!(rule.inject_fns.contains(&"inject_claude_md".to_string()));
        assert!(rule.restore_fns.contains(&"restore_claude_md".to_string()));
    }

    #[test]
    fn project_constitution_config_load_missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = ProjectConstitutionConfig::load(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn project_constitution_config_load_parses_toml() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let toml_content = r#"
[scan]
include = ["src/"]
on_violation = "warn"

[rules.my_rule]
inject_fns = ["setup_ctx"]
restore_fns = ["teardown_ctx"]
severity = "high"
"#;
        std::fs::write(ta_dir.join("constitution.toml"), toml_content).unwrap();
        let config = ProjectConstitutionConfig::load(dir.path())
            .unwrap()
            .unwrap();
        assert!(config.rules.contains_key("my_rule"));
        assert_eq!(config.scan.on_violation, "warn");
    }

    #[test]
    fn project_constitution_config_roundtrip_toml() {
        let config = ProjectConstitutionConfig::ta_default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let parsed: ProjectConstitutionConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(
            parsed.rules.len(),
            config.rules.len(),
            "rule count should round-trip"
        );
    }

    #[test]
    fn scan_for_violations_detects_missing_restore() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // File that calls inject but not restore.
        std::fs::write(src_dir.join("run.rs"), "fn foo() { inject_claude_md(); }\n").unwrap();

        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert(
            "test_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["inject_claude_md".to_string()],
                restore_fns: vec!["restore_claude_md".to_string()],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        config.scan = ConstitutionScan {
            include: vec!["src/".to_string()],
            exclude: vec![],
            on_violation: "warn".to_string(),
        };

        let violations = scan_for_violations(dir.path(), &config).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "test_rule");
        assert_eq!(violations[0].severity, "high");
        assert!(violations[0].message.contains("restore_claude_md"));
    }

    #[test]
    fn scan_for_violations_clean_when_restore_present() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // File that calls both inject and restore.
        std::fs::write(
            src_dir.join("run.rs"),
            "fn foo() { inject_claude_md(); restore_claude_md(); }\n",
        )
        .unwrap();

        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert(
            "test_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["inject_claude_md".to_string()],
                restore_fns: vec!["restore_claude_md".to_string()],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        config.scan = ConstitutionScan {
            include: vec!["src/".to_string()],
            exclude: vec![],
            on_violation: "warn".to_string(),
        };

        let violations = scan_for_violations(dir.path(), &config).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn scan_for_violations_off_returns_empty() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("run.rs"), "fn foo() { inject_claude_md(); }\n").unwrap();

        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert(
            "test_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["inject_claude_md".to_string()],
                restore_fns: vec!["restore_claude_md".to_string()],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        config.scan = ConstitutionScan {
            include: vec!["src/".to_string()],
            exclude: vec![],
            on_violation: "off".to_string(),
        };

        let violations = scan_for_violations(dir.path(), &config).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn scan_for_violations_exclusion_works() {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        let tests_dir = src_dir.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        // Put the violating file in the excluded directory.
        std::fs::write(
            tests_dir.join("run.rs"),
            "fn foo() { inject_claude_md(); }\n",
        )
        .unwrap();

        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert(
            "test_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["inject_claude_md".to_string()],
                restore_fns: vec!["restore_claude_md".to_string()],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        config.scan = ConstitutionScan {
            include: vec!["src/".to_string()],
            exclude: vec!["src/tests/".to_string()],
            on_violation: "warn".to_string(),
        };

        let violations = scan_for_violations(dir.path(), &config).unwrap();
        assert!(
            violations.is_empty(),
            "excluded directory should not be scanned"
        );
    }

    #[test]
    fn init_toml_creates_file() {
        let dir = TempDir::new().unwrap();
        let result = init_toml(dir.path(), None);
        assert!(result.is_ok());
        let path = dir.path().join(".ta/constitution.toml");
        assert!(path.exists(), "constitution.toml should be created");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("injection_cleanup") || content.contains("inject_fns"));
    }

    #[test]
    fn init_toml_does_not_overwrite() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let existing = "# existing content";
        std::fs::write(ta_dir.join("constitution.toml"), existing).unwrap();
        init_toml(dir.path(), None).unwrap();
        let content = std::fs::read_to_string(ta_dir.join("constitution.toml")).unwrap();
        assert_eq!(content, existing, "existing file should not be overwritten");
    }

    #[test]
    fn validate_steps_for_stage_filters_correctly() {
        let config = ProjectConstitutionConfig::ta_default();
        let build_steps = config.validate_steps_for_stage("pre_draft_build");
        let apply_steps = config.validate_steps_for_stage("pre_draft_apply");
        assert!(!build_steps.is_empty());
        assert!(!apply_steps.is_empty());
        assert!(build_steps.iter().all(|s| s.stage == "pre_draft_build"));
        assert!(apply_steps.iter().all(|s| s.stage == "pre_draft_apply"));
    }

    // ── v0.13.15: extends inheritance + template tests ────────────

    #[test]
    fn init_toml_python_template_has_ruff() {
        let dir = TempDir::new().unwrap();
        init_toml(dir.path(), Some("python")).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".ta/constitution.toml")).unwrap();
        assert!(
            content.contains("ruff"),
            "python template should include ruff"
        );
        assert!(content.contains("ta-default"), "should extend ta-default");
    }

    #[test]
    fn init_toml_typescript_template_has_typecheck() {
        let dir = TempDir::new().unwrap();
        init_toml(dir.path(), Some("typescript")).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".ta/constitution.toml")).unwrap();
        assert!(
            content.contains("typecheck"),
            "typescript template should include typecheck"
        );
    }

    #[test]
    fn init_toml_auto_detects_rust_from_cargo_toml() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        // No explicit template — should auto-detect rust.
        let lang = detect_constitution_language(dir.path());
        assert_eq!(lang, "rust");
    }

    #[test]
    fn init_toml_auto_detects_python_from_pyproject() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]\n").unwrap();
        let lang = detect_constitution_language(dir.path());
        assert_eq!(lang, "python");
    }

    // ── v0.14.6.1 tests ────────────────────────────────────────────────────

    #[test]
    fn exact_duplicates_none_when_all_distinct() {
        let mut rules = HashMap::new();
        rules.insert(
            "rule_a".to_string(),
            ConstitutionRule {
                inject_fns: vec!["fn_a".to_string()],
                restore_fns: vec!["fn_a_restore".to_string()],
                patterns: vec![],
                severity: "high".to_string(),
            },
        );
        rules.insert(
            "rule_b".to_string(),
            ConstitutionRule {
                inject_fns: vec!["fn_b".to_string()],
                restore_fns: vec!["fn_b_restore".to_string()],
                patterns: vec![],
                severity: "medium".to_string(),
            },
        );
        let dups = detect_exact_duplicates(&rules);
        assert!(
            dups.is_empty(),
            "distinct rules should produce no exact duplicates"
        );
    }

    #[test]
    fn exact_duplicates_found_when_content_identical() {
        let rule = ConstitutionRule {
            inject_fns: vec!["fn_x".to_string()],
            restore_fns: vec!["fn_x_restore".to_string()],
            patterns: vec!["PATTERN".to_string()],
            severity: "high".to_string(),
        };
        let mut rules = HashMap::new();
        rules.insert("alpha".to_string(), rule.clone());
        rules.insert("beta".to_string(), rule.clone());
        let dups = detect_exact_duplicates(&rules);
        assert_eq!(dups.len(), 1, "one duplicate pair should be found");
        // alpha < beta lexicographically
        assert_eq!(dups[0], ("alpha".to_string(), "beta".to_string()));
    }

    #[test]
    fn exact_duplicates_order_independent() {
        // inject_fns in different order should still be detected as duplicate.
        let mut rule_a = ConstitutionRule {
            inject_fns: vec!["fn2".to_string(), "fn1".to_string()],
            restore_fns: vec!["fn1_r".to_string()],
            patterns: vec![],
            severity: "low".to_string(),
        };
        let rule_b = ConstitutionRule {
            inject_fns: vec!["fn1".to_string(), "fn2".to_string()],
            restore_fns: vec!["fn1_r".to_string()],
            patterns: vec![],
            severity: "low".to_string(),
        };
        rule_a.inject_fns.sort(); // fingerprint sorts them
        let mut rules = HashMap::new();
        rules.insert("x".to_string(), rule_a);
        rules.insert("y".to_string(), rule_b);
        // Both have the same sorted inject_fns so fingerprints should match.
        let dups = detect_exact_duplicates(&rules);
        assert_eq!(
            dups.len(),
            1,
            "order-independent duplicate should be detected"
        );
    }

    #[test]
    fn agent_review_response_roundtrip_json() {
        let resp = AgentReviewResponse {
            duplicates: vec![SemanticDuplicate {
                rule_a: "a".to_string(),
                rule_b: "b".to_string(),
                canonical: "a".to_string(),
            }],
            conflicts: vec![SemanticConflict {
                rule_a: "c".to_string(),
                rule_b: "d".to_string(),
                recommendation: "drop d".to_string(),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: AgentReviewResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.duplicates.len(), 1);
        assert_eq!(parsed.conflicts.len(), 1);
        assert_eq!(parsed.duplicates[0].canonical, "a");
        assert_eq!(parsed.conflicts[0].recommendation, "drop d");
    }

    #[test]
    fn generate_merged_toml_removes_exact_dups() {
        let rule = ConstitutionRule {
            inject_fns: vec!["fn_inject".to_string()],
            restore_fns: vec!["fn_restore".to_string()],
            patterns: vec![],
            severity: "medium".to_string(),
        };
        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert("rule_dup_a".to_string(), rule.clone());
        config.rules.insert("rule_dup_b".to_string(), rule.clone());
        config.rules.insert(
            "rule_unique".to_string(),
            ConstitutionRule {
                inject_fns: vec!["other".to_string()],
                restore_fns: vec![],
                patterns: vec![],
                severity: "low".to_string(),
            },
        );

        let exact_dups = detect_exact_duplicates(&config.rules);
        let (toml_str, stats) = generate_merged_toml(&config, &exact_dups, None);

        assert_eq!(stats.rules_before, 3);
        assert_eq!(stats.rules_after, 2, "one duplicate should be removed");
        assert_eq!(stats.exact_removed, 1);
        // The merged TOML should contain the canonical name but not the duplicate.
        assert!(
            toml_str.contains("[rules.rule_dup_a]"),
            "canonical section should be present"
        );
        // rule_dup_b should not appear as a rules section (may appear in comment text)
        assert!(
            !toml_str.contains("[rules.rule_dup_b]"),
            "duplicate section should be absent"
        );
        assert!(
            toml_str.contains("# merged from:"),
            "merge comment should be present"
        );
    }

    #[test]
    fn generate_merged_toml_no_changes_when_clean() {
        let mut config = ProjectConstitutionConfig::default();
        config.rules.insert(
            "only_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["fn".to_string()],
                restore_fns: vec![],
                patterns: vec![],
                severity: "medium".to_string(),
            },
        );
        let (_, stats) = generate_merged_toml(&config, &[], None);
        assert_eq!(stats.rules_before, stats.rules_after);
        assert_eq!(stats.exact_removed, 0);
        assert_eq!(stats.conflicts, 0);
    }

    #[test]
    fn constitution_unified_diff_empty_when_equal() {
        let diff = constitution_unified_diff(".ta/constitution.toml", "same", "same");
        assert!(diff.is_empty(), "no diff when content is identical");
    }

    #[test]
    fn constitution_unified_diff_non_empty_when_changed() {
        let diff = constitution_unified_diff(".ta/constitution.toml", "old\n", "new\n");
        assert!(diff.contains("--- a/.ta/constitution.toml"));
        assert!(diff.contains("+++ b/.ta/constitution.toml"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
    }

    #[test]
    fn extends_ta_default_merges_rules() {
        let mut project = ProjectConstitutionConfig {
            extends: Some("ta-default".to_string()),
            ..Default::default()
        };
        project.rules.insert(
            "my_rule".to_string(),
            ConstitutionRule {
                inject_fns: vec!["my_inject".to_string()],
                restore_fns: vec!["my_restore".to_string()],
                patterns: vec![],
                severity: "low".to_string(),
            },
        );

        let merged = apply_extends_ta_default(project);
        // Base rules from ta-default should be present.
        assert!(
            merged.rules.contains_key("injection_cleanup"),
            "ta-default rule should be inherited"
        );
        // Project rule should also be present.
        assert!(
            merged.rules.contains_key("my_rule"),
            "project rule should be preserved"
        );
        // extends should be consumed.
        assert!(
            merged.extends.is_none(),
            "extends should be None after merging"
        );
    }
}
