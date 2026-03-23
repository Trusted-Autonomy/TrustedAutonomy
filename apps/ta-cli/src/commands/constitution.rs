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

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

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
