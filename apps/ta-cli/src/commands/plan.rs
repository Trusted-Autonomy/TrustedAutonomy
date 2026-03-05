// plan.rs — Plan tracking: parse PLAN.md, show status, update phases.
//
// The canonical project plan lives in PLAN.md at the project root.
// Each phase has a machine-parseable status marker:
//   ## Phase 4b — Per-Artifact Review Model
//   <!-- status: pending -->
//
// Sub-phases use ### headers with the same status marker pattern:
//   ### v0.3.1 — Plan Lifecycle Automation
//   <!-- status: pending -->
//
// v0.3.1.1: Parsing is now schema-driven via `.ta/plan-schema.yaml`.
// If no schema file is present, a built-in default matching the above format is used.
//
// `ta plan list` shows all phases with their status.
// `ta plan status` shows a summary of progress.
// `ta plan next` shows the next pending phase and optionally creates a goal for it.
// `ta plan history` shows plan change history.
// `ta plan init` extracts a schema from an existing plan document.
// `ta plan create` generates a new plan from a template.
// `ta pr apply` auto-updates PLAN.md when a goal with --phase completes.

use std::fmt;
use std::path::Path;

use clap::Subcommand;
use regex::Regex;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum PlanCommands {
    /// List all plan phases with their status.
    List,
    /// Show a summary of plan progress.
    Status {
        /// Output as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Show the next pending phase and suggest creating a goal for it.
    Next,
    /// Show plan change history (status transitions recorded in .ta/plan_history.jsonl).
    History,
    /// Validate completed work against the plan for a given phase.
    Validate {
        /// Phase ID to validate (e.g., "v0.3.1").
        phase: String,
    },
    /// Extract a plan-schema.yaml from an existing plan document.
    Init {
        /// Plan file to analyze (default: PLAN.md).
        #[arg(long, default_value = "PLAN.md")]
        source: String,
        /// Write the schema without prompting for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Generate a new plan document from a template.
    Create {
        /// Output file path (default: PLAN.md).
        #[arg(long, default_value = "PLAN.md")]
        output: String,
        /// Template: greenfield, feature, or bugfix.
        #[arg(long, default_value = "greenfield")]
        template: String,
        /// Project name for the plan header.
        #[arg(long)]
        name: Option<String>,
    },
}

pub fn execute(cmd: &PlanCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        PlanCommands::List => list_phases(config),
        PlanCommands::Status { json } => show_status(config, *json),
        PlanCommands::Next => show_next(config),
        PlanCommands::History => show_history(config),
        PlanCommands::Validate { phase } => validate_phase(config, phase),
        PlanCommands::Init { source, yes } => plan_init(config, source, *yes),
        PlanCommands::Create {
            output,
            template,
            name,
        } => plan_create(config, output, template, name.as_deref()),
    }
}

// ── Data model ───────────────────────────────────────────────────

/// Status of a plan phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanStatus::Pending => write!(f, "pending"),
            PlanStatus::InProgress => write!(f, "in_progress"),
            PlanStatus::Done => write!(f, "done"),
        }
    }
}

/// A parsed plan phase from PLAN.md.
#[derive(Debug, Clone)]
pub struct PlanPhase {
    /// Phase identifier (e.g., "0", "4b", "4a.1").
    pub id: String,
    /// Human-readable title (e.g., "Per-Artifact Review Model").
    pub title: String,
    /// Current status.
    pub status: PlanStatus,
}

// ── Schema-driven parsing ────────────────────────────────────────

/// A single phase-header pattern in the schema.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PhasePattern {
    /// Regex with capturing groups: group 1 = phase ID, group 2 (optional) = title.
    pub regex: String,
    /// Human-readable label for what this pattern captures (informational only).
    #[serde(default)]
    pub id_capture: String,
}

/// Schema describing how to parse a project's plan document.
/// Loaded from `.ta/plan-schema.yaml`. If absent, the built-in default is used.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanSchema {
    /// Path to the plan file, relative to project root (default: "PLAN.md").
    #[serde(default = "default_source")]
    pub source: String,
    /// One or more header patterns for phase detection (evaluated in order, first match wins).
    pub phase_patterns: Vec<PhasePattern>,
    /// Regex with one capture group that extracts the status value.
    pub status_marker: String,
    /// Recognized status values. Anything not in this list maps to Pending.
    #[serde(default = "default_statuses")]
    pub statuses: Vec<String>,
}

fn default_source() -> String {
    "PLAN.md".to_string()
}

fn default_statuses() -> Vec<String> {
    vec![
        "done".to_string(),
        "in_progress".to_string(),
        "pending".to_string(),
    ]
}

impl PlanSchema {
    /// The built-in default schema — matches the current PLAN.md format.
    /// Used when no `.ta/plan-schema.yaml` is present.
    pub fn default_schema() -> Self {
        PlanSchema {
            source: "PLAN.md".to_string(),
            phase_patterns: vec![
                PhasePattern {
                    // Matches: "## Phase 4b — Title" and "## Phase 4a.1 — Title"
                    regex: r"^##\s+Phase[\s\u{a0}]+([0-9a-z.]+)\s+[—\-]\s+(.+)$".to_string(),
                    id_capture: "phase_number".to_string(),
                },
                PhasePattern {
                    // Matches: "### v0.3.1 — Title" or "### v0.3.1.1 — Title"
                    regex: r"^###\s+(v[\d.]+[a-z]?)\s+[—\-]\s+(.+)$".to_string(),
                    id_capture: "version_number".to_string(),
                },
            ],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
        }
    }

    /// Load schema from `.ta/plan-schema.yaml`, falling back to `default_schema()`.
    pub fn load_or_default(project_root: &Path) -> Self {
        let schema_path = project_root.join(".ta/plan-schema.yaml");
        if schema_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&schema_path) {
                if let Ok(schema) = serde_yaml::from_str::<PlanSchema>(&content) {
                    return schema;
                }
                eprintln!("Warning: failed to parse .ta/plan-schema.yaml — using default schema");
            }
        }
        Self::default_schema()
    }

    /// Serialize to YAML string.
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }
}

// ── Parsing ──────────────────────────────────────────────────────

/// Parse plan content using a provided schema.
///
/// Each `phase_patterns` regex is tested against each line.
/// The first match wins. The regex must have:
///   - Group 1: phase ID (e.g., "4b", "v0.3.1")
///   - Group 2 (optional): phase title
///
/// The status marker regex is tested against the next non-empty line.
pub fn parse_plan_with_schema(content: &str, schema: &PlanSchema) -> Vec<PlanPhase> {
    // Pre-compile all regexes. Silently skip invalid ones.
    let compiled_patterns: Vec<Regex> = schema
        .phase_patterns
        .iter()
        .filter_map(|p| Regex::new(&p.regex).ok())
        .collect();

    let status_re = match Regex::new(&schema.status_marker) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut phases = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        for pattern in &compiled_patterns {
            if let Some(caps) = pattern.captures(line) {
                let id = caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                let title = caps
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();

                if id.is_empty() {
                    break;
                }

                // Strip trailing markup from title (e.g. "*(release)*").
                let title = title.trim_end_matches(['*', '(', ')']).trim().to_string();

                let status = find_status_in_lookahead(&lines, i + 1, &status_re);
                phases.push(PlanPhase { id, title, status });
                break; // First pattern match wins.
            }
        }

        i += 1;
    }

    phases
}

/// Compare phase IDs, normalizing the optional `v` prefix.
/// e.g., "v0.4.0" matches "0.4.0", "4b" matches "4b".
pub fn phase_ids_match(parsed_id: &str, phase_id: &str) -> bool {
    if parsed_id == phase_id {
        return true;
    }
    let norm_parsed = parsed_id.strip_prefix('v').unwrap_or(parsed_id);
    let norm_phase = phase_id.strip_prefix('v').unwrap_or(phase_id);
    norm_parsed == norm_phase
}

/// Look ahead from `start` for a status marker comment.
/// Checks the immediate next line (matching existing behavior).
fn find_status_in_lookahead(lines: &[&str], start: usize, status_re: &Regex) -> PlanStatus {
    if start < lines.len() {
        let line = lines[start].trim();
        if let Some(caps) = status_re.captures(line) {
            let status_str = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            return parse_status_str(status_str);
        }
    }
    PlanStatus::Pending
}

fn parse_status_str(s: &str) -> PlanStatus {
    match s {
        "done" => PlanStatus::Done,
        "in_progress" => PlanStatus::InProgress,
        _ => PlanStatus::Pending,
    }
}

/// Parse PLAN.md content into a list of phases (using the default schema).
///
/// This is the backward-compatible entry point used by existing code.
pub fn parse_plan(content: &str) -> Vec<PlanPhase> {
    parse_plan_with_schema(content, &PlanSchema::default_schema())
}

/// Update a phase's status in PLAN.md content. Returns the new content.
///
/// Finds the phase by ID using the default schema's patterns
/// and replaces its status marker.
pub fn update_phase_status(content: &str, phase_id: &str, new_status: PlanStatus) -> String {
    update_phase_status_with_schema(content, phase_id, new_status, &PlanSchema::default_schema())
}

/// Update a phase's status using a provided schema.
pub fn update_phase_status_with_schema(
    content: &str,
    phase_id: &str,
    new_status: PlanStatus,
    schema: &PlanSchema,
) -> String {
    let compiled_patterns: Vec<Regex> = schema
        .phase_patterns
        .iter()
        .filter_map(|p| Regex::new(&p.regex).ok())
        .collect();

    let status_re = match Regex::new(&schema.status_marker) {
        Ok(r) => r,
        Err(_) => return content.to_string(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Check if this line is the target phase header.
        // Normalize comparison: "v0.4.0" matches "0.4.0" and vice versa.
        let mut is_target = false;
        for pattern in &compiled_patterns {
            if let Some(caps) = pattern.captures(trimmed) {
                if let Some(id_match) = caps.get(1) {
                    let parsed_id = id_match.as_str().trim();
                    if phase_ids_match(parsed_id, phase_id) {
                        is_target = true;
                        break;
                    }
                }
            }
        }

        result.push(line.to_string());

        // If this is the target phase, replace the next line's status marker.
        if is_target && i + 1 < lines.len() {
            let next_line = lines[i + 1].trim();
            if status_re.is_match(next_line) {
                result.push(format!("<!-- status: {} -->", new_status));
                i += 2;
                continue;
            }
        }

        i += 1;
    }

    result.join("\n")
}

/// Read and parse PLAN.md from a project directory.
///
/// Loads `.ta/plan-schema.yaml` if present, otherwise uses the default schema.
pub fn load_plan(project_root: &Path) -> anyhow::Result<Vec<PlanPhase>> {
    let schema = PlanSchema::load_or_default(project_root);
    let plan_path = project_root.join(&schema.source);
    if !plan_path.exists() {
        anyhow::bail!("No {} found in {}", schema.source, project_root.display());
    }
    let content = std::fs::read_to_string(&plan_path)?;
    Ok(parse_plan_with_schema(&content, &schema))
}

/// Format a plan phase list as a checklist for CLAUDE.md injection.
pub fn format_plan_checklist(phases: &[PlanPhase], current_phase: Option<&str>) -> String {
    let mut lines = Vec::new();
    for phase in phases {
        let checkbox = if phase.status == PlanStatus::Done {
            "[x]"
        } else {
            "[ ]"
        };
        let current_marker = if current_phase == Some(phase.id.as_str()) {
            " <-- current"
        } else {
            ""
        };
        let bold = if current_phase == Some(phase.id.as_str()) {
            format!("**Phase {} — {}**", phase.id, phase.title)
        } else {
            format!("Phase {} — {}", phase.id, phase.title)
        };
        lines.push(format!("- {} {}{}", checkbox, bold, current_marker));
    }
    lines.join("\n")
}

/// Find the next pending phase after the given phase ID.
/// If `after_phase` is None, returns the first pending phase.
pub fn find_next_pending<'a>(
    phases: &'a [PlanPhase],
    after_phase: Option<&str>,
) -> Option<&'a PlanPhase> {
    if let Some(after) = after_phase {
        // Find the current phase's position and search forward from there.
        if let Some(idx) = phases.iter().position(|p| p.id == after) {
            // Search forward from the phase after the current one.
            if let Some(next) = phases[idx + 1..]
                .iter()
                .find(|p| p.status == PlanStatus::Pending)
            {
                return Some(next);
            }
        }
        // Phase not found or no pending phases after it — don't fall back to
        // the beginning (which would suggest unrelated earlier phases like v0.1).
        None
    } else {
        phases.iter().find(|p| p.status == PlanStatus::Pending)
    }
}

/// Record a plan phase status change to the history log.
pub fn record_history(
    project_root: &Path,
    phase_id: &str,
    old_status: &PlanStatus,
    new_status: &PlanStatus,
) -> anyhow::Result<()> {
    let ta_dir = project_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let history_path = ta_dir.join("plan_history.jsonl");

    let entry = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "phase_id": phase_id,
        "old_status": old_status.to_string(),
        "new_status": new_status.to_string(),
    });

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)?;
    writeln!(file, "{}", entry)?;
    Ok(())
}

/// Load plan history entries from the JSONL file.
pub fn load_history(project_root: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    let history_path = project_root.join(".ta/plan_history.jsonl");
    if !history_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&history_path)?;
    let entries: Vec<serde_json::Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(entries)
}

/// Build a suggested `ta run` command for the next pending phase.
pub fn suggest_next_goal_command(phase: &PlanPhase) -> String {
    format!(
        "ta run \"implement {}\" --source . --phase {}",
        phase.title, phase.id
    )
}

// ── Schema detection ─────────────────────────────────────────────

/// Heuristic schema detection from plan content.
///
/// Tries the default schema first — if it finds phases, uses it.
/// Otherwise falls back to a loose heading-based schema.
fn detect_schema_from_content(content: &str, source: &str) -> PlanSchema {
    let default = PlanSchema::default_schema();
    let phases_with_default = parse_plan_with_schema(content, &default);
    if !phases_with_default.is_empty() {
        let mut schema = default;
        schema.source = source.to_string();
        return schema;
    }

    // Fallback: generic ## heading pattern.
    PlanSchema {
        source: source.to_string(),
        phase_patterns: vec![PhasePattern {
            regex: r"^##\s+(.+)$".to_string(),
            id_capture: "heading_text".to_string(),
        }],
        status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
        statuses: default_statuses(),
    }
}

// ── Plan templates ───────────────────────────────────────────────

fn greenfield_plan_template(name: &str) -> String {
    format!(
        r#"# {name} — Development Plan

## Phase 0 — Project Setup
<!-- status: pending -->
Repository layout, tooling, CI/CD.

## Phase 1 — Core Feature
<!-- status: pending -->
Implement the primary feature or MVP.

## Phase 2 — Testing & Polish
<!-- status: pending -->
Test coverage, documentation, release prep.
"#,
        name = name
    )
}

fn feature_plan_template(name: &str) -> String {
    format!(
        r#"# {name} — Feature Plan

## Phase 1 — Design
<!-- status: pending -->
Requirements, API design, interface contracts.

## Phase 2 — Implementation
<!-- status: pending -->
Core implementation with unit tests.

## Phase 3 — Integration & Review
<!-- status: pending -->
Integration tests, code review, merge.
"#,
        name = name
    )
}

fn bugfix_plan_template(name: &str) -> String {
    format!(
        r#"# {name} — Bug Fix Plan

## Phase 1 — Reproduce
<!-- status: pending -->
Reproduce the bug with a failing test.

## Phase 2 — Fix
<!-- status: pending -->
Implement the fix, verify the test passes.

## Phase 3 — Regression Tests
<!-- status: pending -->
Add regression tests, deploy.
"#,
        name = name
    )
}

// ── CLI implementations ──────────────────────────────────────────

fn list_phases(config: &GatewayConfig) -> anyhow::Result<()> {
    let phases = load_plan(&config.workspace_root)?;

    if phases.is_empty() {
        println!("No phases found in PLAN.md.");
        return Ok(());
    }

    println!("{:<12} {:<40} {:<14}", "PHASE", "TITLE", "STATUS");
    println!("{}", "-".repeat(66));

    for phase in &phases {
        let status_display = match phase.status {
            PlanStatus::Done => "done",
            PlanStatus::InProgress => "in_progress",
            PlanStatus::Pending => "pending",
        };
        println!(
            "{:<12} {:<40} {:<14}",
            phase.id,
            truncate(&phase.title, 38),
            status_display,
        );
    }

    Ok(())
}

fn show_status(config: &GatewayConfig, json_output: bool) -> anyhow::Result<()> {
    let phases = load_plan(&config.workspace_root)?;

    let done = phases
        .iter()
        .filter(|p| p.status == PlanStatus::Done)
        .count();
    let in_progress = phases
        .iter()
        .filter(|p| p.status == PlanStatus::InProgress)
        .count();
    let pending = phases
        .iter()
        .filter(|p| p.status == PlanStatus::Pending)
        .count();
    let total = phases.len();

    if json_output {
        let data = serde_json::json!({
            "total": total,
            "done": done,
            "in_progress": in_progress,
            "pending": pending,
            "phases": phases.iter().map(|p| serde_json::json!({
                "id": p.id,
                "title": p.title,
                "status": format!("{}", p.status),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    println!("Plan Progress: {}/{} phases complete", done, total);
    println!("  Done:        {}", done);
    println!("  In Progress: {}", in_progress);
    println!("  Pending:     {}", pending);

    if let Some(current) = phases.iter().find(|p| p.status == PlanStatus::InProgress) {
        println!("\nCurrent: Phase {} — {}", current.id, current.title);
    }

    if let Some(next) = phases.iter().find(|p| p.status == PlanStatus::Pending) {
        println!("Next:    Phase {} — {}", next.id, next.title);
    }

    Ok(())
}

fn show_next(config: &GatewayConfig) -> anyhow::Result<()> {
    let phases = load_plan(&config.workspace_root)?;

    // Find next pending (prefer after in_progress, fallback to first pending).
    let after_current = phases
        .iter()
        .rev()
        .find(|p| p.status == PlanStatus::InProgress)
        .map(|p| p.id.as_str());

    let next = find_next_pending(&phases, after_current);

    match next {
        Some(phase) => {
            println!("Next pending phase:");
            println!("  Phase {} — {}", phase.id, phase.title);
            println!();
            println!("To start working on it:");
            println!("  {}", suggest_next_goal_command(phase));
        }
        None => {
            println!("All plan phases are complete or in progress.");
        }
    }

    Ok(())
}

fn show_history(config: &GatewayConfig) -> anyhow::Result<()> {
    let entries = load_history(&config.workspace_root)?;

    if entries.is_empty() {
        println!("No plan history recorded yet.");
        println!("History is recorded when phases change status via `ta draft apply`.");
        return Ok(());
    }

    println!(
        "{:<24} {:<14} {:<14} {:<14}",
        "TIMESTAMP", "PHASE", "FROM", "TO"
    );
    println!("{}", "-".repeat(66));

    for entry in &entries {
        let ts = entry["timestamp"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(19)
            .collect::<String>();
        let phase = entry["phase_id"].as_str().unwrap_or("?");
        let from = entry["old_status"].as_str().unwrap_or("?");
        let to = entry["new_status"].as_str().unwrap_or("?");
        println!("{:<24} {:<14} {:<14} {:<14}", ts, phase, from, to);
    }

    Ok(())
}

fn validate_phase(config: &GatewayConfig, phase_id: &str) -> anyhow::Result<()> {
    let phases = load_plan(&config.workspace_root)?;

    let phase = phases.iter().find(|p| p.id == phase_id);
    match phase {
        None => {
            anyhow::bail!("Phase '{}' not found in PLAN.md", phase_id);
        }
        Some(p) => {
            println!("Phase {} — {}", p.id, p.title);
            println!("Status: {}", p.status);

            // Look for the most recent goal linked to this phase.
            let goal_store = ta_goal::GoalRunStore::new(&config.goals_dir)?;
            let goals = goal_store.list()?;
            let phase_goals: Vec<_> = goals
                .iter()
                .filter(|g| g.plan_phase.as_deref() == Some(phase_id))
                .collect();

            if phase_goals.is_empty() {
                println!("\nNo goals found linked to this phase.");
                if p.status != PlanStatus::Done {
                    println!("To start: {}", suggest_next_goal_command(p));
                }
                return Ok(());
            }

            println!("\nLinked goals ({}):", phase_goals.len());
            for g in &phase_goals {
                println!(
                    "  {} — {} [{}]",
                    &g.goal_run_id.to_string()[..8],
                    g.title,
                    g.state,
                );
            }

            // Check if the most recent goal has a draft with change_summary.
            if let Some(latest) = phase_goals.first() {
                if let Some(pkg_id) = latest.pr_package_id {
                    let pkg_path = config.pr_packages_dir.join(format!("{}.json", pkg_id));
                    if pkg_path.exists() {
                        let content = std::fs::read_to_string(&pkg_path)?;
                        if let Ok(pkg) =
                            serde_json::from_str::<ta_changeset::DraftPackage>(&content)
                        {
                            println!("\nLatest draft summary: {}", pkg.summary.what_changed);
                            println!("  Artifacts: {}", pkg.changes.artifacts.len());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn plan_init(config: &GatewayConfig, source: &str, yes: bool) -> anyhow::Result<()> {
    let plan_path = config.workspace_root.join(source);
    if !plan_path.exists() {
        anyhow::bail!("Plan file not found: {}", plan_path.display());
    }

    let content = std::fs::read_to_string(&plan_path)?;
    let schema = detect_schema_from_content(&content, source);

    let schema_path = config.workspace_root.join(".ta/plan-schema.yaml");

    let yaml = schema.to_yaml()?;
    println!("Proposed .ta/plan-schema.yaml:");
    println!("---");
    print!("{}", yaml);
    println!("---");

    // Show how many phases this schema detects.
    let phases = parse_plan_with_schema(&content, &schema);
    println!("This schema detects {} phases.", phases.len());
    if !phases.is_empty() {
        println!("First detected:");
        for p in phases.iter().take(3) {
            println!("  {} — {} [{}]", p.id, p.title, p.status);
        }
    }

    if schema_path.exists() && !yes {
        println!("\n.ta/plan-schema.yaml already exists. Use --yes to overwrite.");
        return Ok(());
    }

    if !yes {
        print!("\nWrite this schema? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    std::fs::create_dir_all(config.workspace_root.join(".ta"))?;
    std::fs::write(&schema_path, yaml)?;
    println!("Written: {}", schema_path.display());
    Ok(())
}

fn plan_create(
    config: &GatewayConfig,
    output: &str,
    template: &str,
    name: Option<&str>,
) -> anyhow::Result<()> {
    let output_path = config.workspace_root.join(output);
    if output_path.exists() {
        anyhow::bail!(
            "{} already exists. Delete it or specify a different --output path.",
            output
        );
    }

    let project_name = name.unwrap_or("My Project");
    let content = match template {
        "feature" => feature_plan_template(project_name),
        "bugfix" => bugfix_plan_template(project_name),
        _ => greenfield_plan_template(project_name),
    };

    std::fs::write(&output_path, &content)?;
    println!("Created: {}", output_path.display());

    // Also write a schema file that matches the template format.
    let schema_path = config.workspace_root.join(".ta/plan-schema.yaml");
    if !schema_path.exists() {
        std::fs::create_dir_all(config.workspace_root.join(".ta"))?;
        let schema = PlanSchema::default_schema();
        let yaml = schema.to_yaml()?;
        std::fs::write(&schema_path, yaml)?;
        println!("Created: {}", schema_path.display());
    }

    println!("\nRun 'ta plan list' to see your phases.");
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PLAN: &str = r#"# Trusted Autonomy — Development Plan

## Phase 0 — Repo Layout
<!-- status: done -->
Basic repo structure.

## Phase 1 — Kernel
<!-- status: done -->
Core crates.

## Phase 4a — Agent Prompt Enhancement
<!-- status: done -->
CLAUDE.md injection.

## Phase 4a.1 — Plan Tracking
<!-- status: in_progress -->
This very feature.

## Phase 4b — Per-Artifact Review Model
<!-- status: pending -->
Selective approval.

## Phase 4c — Selective Review CLI
<!-- status: pending -->
Wildcards in approve/reject/discuss.
"#;

    const SAMPLE_PLAN_WITH_SUBPHASES: &str = r#"# Plan

## Phase 0 — Repo Layout
<!-- status: done -->

## v0.3 — Review & Plan Automation *(release)*

### v0.3.0 — Review Sessions
<!-- status: done -->
Review sessions.

### v0.3.1 — Plan Lifecycle Automation
<!-- status: pending -->
Automation features.

### v0.3.2 — Configurable Release Pipeline
<!-- status: pending -->
Release automation.
"#;

    #[test]
    fn parse_plan_extracts_all_phases() {
        let phases = parse_plan(SAMPLE_PLAN);
        assert_eq!(phases.len(), 6);
        assert_eq!(phases[0].id, "0");
        assert_eq!(phases[0].title, "Repo Layout");
        assert_eq!(phases[0].status, PlanStatus::Done);
    }

    #[test]
    fn parse_plan_handles_dotted_ids() {
        let phases = parse_plan(SAMPLE_PLAN);
        let phase_4a1 = &phases[3];
        assert_eq!(phase_4a1.id, "4a.1");
        assert_eq!(phase_4a1.title, "Plan Tracking");
        assert_eq!(phase_4a1.status, PlanStatus::InProgress);
    }

    #[test]
    fn parse_plan_handles_all_statuses() {
        let phases = parse_plan(SAMPLE_PLAN);
        let statuses: Vec<&PlanStatus> = phases.iter().map(|p| &p.status).collect();
        assert_eq!(
            statuses,
            vec![
                &PlanStatus::Done,
                &PlanStatus::Done,
                &PlanStatus::Done,
                &PlanStatus::InProgress,
                &PlanStatus::Pending,
                &PlanStatus::Pending,
            ]
        );
    }

    #[test]
    fn update_phase_status_changes_target() {
        let updated = update_phase_status(SAMPLE_PLAN, "4b", PlanStatus::Done);
        let phases = parse_plan(&updated);
        let phase_4b = phases.iter().find(|p| p.id == "4b").unwrap();
        assert_eq!(phase_4b.status, PlanStatus::Done);
    }

    #[test]
    fn update_phase_status_preserves_others() {
        let updated = update_phase_status(SAMPLE_PLAN, "4b", PlanStatus::Done);
        let phases = parse_plan(&updated);
        // Phase 0 still done.
        assert_eq!(phases[0].status, PlanStatus::Done);
        // Phase 4c still pending.
        let phase_4c = phases.iter().find(|p| p.id == "4c").unwrap();
        assert_eq!(phase_4c.status, PlanStatus::Pending);
    }

    #[test]
    fn update_nonexistent_phase_is_noop() {
        let updated = update_phase_status(SAMPLE_PLAN, "99", PlanStatus::Done);
        // Content should be unchanged (no crash, no corruption).
        let phases = parse_plan(&updated);
        assert_eq!(phases.len(), 6);
    }

    #[test]
    fn format_plan_checklist_marks_current() {
        let phases = parse_plan(SAMPLE_PLAN);
        let checklist = format_plan_checklist(&phases, Some("4a.1"));
        assert!(checklist.contains("[x] Phase 0"));
        assert!(checklist.contains("[ ] **Phase 4a.1 — Plan Tracking** <-- current"));
        assert!(checklist.contains("[ ] Phase 4b"));
    }

    #[test]
    fn plan_status_display() {
        assert_eq!(PlanStatus::Done.to_string(), "done");
        assert_eq!(PlanStatus::InProgress.to_string(), "in_progress");
        assert_eq!(PlanStatus::Pending.to_string(), "pending");
    }

    // ── Sub-phase tests ──

    #[test]
    fn parse_plan_handles_sub_phases() {
        let phases = parse_plan(SAMPLE_PLAN_WITH_SUBPHASES);
        // Should find: Phase 0, v0.3.0, v0.3.1, v0.3.2
        assert_eq!(phases.len(), 4);
        assert_eq!(phases[0].id, "0");
        assert_eq!(phases[1].id, "v0.3.0");
        assert_eq!(phases[1].title, "Review Sessions");
        assert_eq!(phases[1].status, PlanStatus::Done);
        assert_eq!(phases[2].id, "v0.3.1");
        assert_eq!(phases[2].title, "Plan Lifecycle Automation");
        assert_eq!(phases[2].status, PlanStatus::Pending);
    }

    #[test]
    fn update_sub_phase_status() {
        let updated = update_phase_status(SAMPLE_PLAN_WITH_SUBPHASES, "v0.3.1", PlanStatus::Done);
        let phases = parse_plan(&updated);
        let phase = phases.iter().find(|p| p.id == "v0.3.1").unwrap();
        assert_eq!(phase.status, PlanStatus::Done);
        // v0.3.0 should still be done.
        let v030 = phases.iter().find(|p| p.id == "v0.3.0").unwrap();
        assert_eq!(v030.status, PlanStatus::Done);
        // v0.3.2 should still be pending.
        let v032 = phases.iter().find(|p| p.id == "v0.3.2").unwrap();
        assert_eq!(v032.status, PlanStatus::Pending);
    }

    #[test]
    fn find_next_pending_returns_first() {
        let phases = parse_plan(SAMPLE_PLAN);
        let next = find_next_pending(&phases, None);
        assert!(next.is_some());
        assert_eq!(next.unwrap().id, "4b");
    }

    #[test]
    fn find_next_pending_after_phase() {
        let phases = parse_plan(SAMPLE_PLAN);
        let next = find_next_pending(&phases, Some("4b"));
        assert!(next.is_some());
        assert_eq!(next.unwrap().id, "4c");
    }

    #[test]
    fn find_next_pending_returns_none_when_all_done() {
        let plan = r#"
## Phase 0 — Done
<!-- status: done -->
"#;
        let phases = parse_plan(plan);
        let next = find_next_pending(&phases, None);
        assert!(next.is_none());
    }

    #[test]
    fn find_next_pending_sub_phases() {
        let phases = parse_plan(SAMPLE_PLAN_WITH_SUBPHASES);
        let next = find_next_pending(&phases, Some("v0.3.0"));
        assert!(next.is_some());
        assert_eq!(next.unwrap().id, "v0.3.1");
    }

    #[test]
    fn suggest_next_goal_command_format() {
        let phase = PlanPhase {
            id: "v0.3.2".to_string(),
            title: "Release Pipeline".to_string(),
            status: PlanStatus::Pending,
        };
        let cmd = suggest_next_goal_command(&phase);
        assert_eq!(
            cmd,
            "ta run \"implement Release Pipeline\" --source . --phase v0.3.2"
        );
    }

    #[test]
    fn record_and_load_history() {
        let dir = tempfile::tempdir().unwrap();
        record_history(
            dir.path(),
            "v0.3.1",
            &PlanStatus::Pending,
            &PlanStatus::Done,
        )
        .unwrap();
        record_history(
            dir.path(),
            "v0.3.2",
            &PlanStatus::Pending,
            &PlanStatus::InProgress,
        )
        .unwrap();

        let entries = load_history(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["phase_id"], "v0.3.1");
        assert_eq!(entries[0]["new_status"], "done");
        assert_eq!(entries[1]["phase_id"], "v0.3.2");
        assert_eq!(entries[1]["new_status"], "in_progress");
    }

    #[test]
    fn load_history_empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let entries = load_history(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    // ── v0.3.1.1: Schema-driven parsing tests ──

    #[test]
    fn default_schema_matches_hardcoded_behavior() {
        // parse_plan() using the default schema must produce identical output
        // for both the top-level and sub-phase plan formats.
        let phases = parse_plan(SAMPLE_PLAN);
        let schema = PlanSchema::default_schema();
        let phases_schema = parse_plan_with_schema(SAMPLE_PLAN, &schema);
        assert_eq!(phases.len(), phases_schema.len());
        for (old, new) in phases.iter().zip(phases_schema.iter()) {
            assert_eq!(old.id, new.id, "IDs differ for phase {}", old.id);
            assert_eq!(old.title, new.title, "Titles differ for phase {}", old.id);
            assert_eq!(
                old.status, new.status,
                "Statuses differ for phase {}",
                old.id
            );
        }
    }

    #[test]
    fn default_schema_matches_sub_phases() {
        let phases = parse_plan(SAMPLE_PLAN_WITH_SUBPHASES);
        let schema = PlanSchema::default_schema();
        let phases_schema = parse_plan_with_schema(SAMPLE_PLAN_WITH_SUBPHASES, &schema);
        assert_eq!(phases.len(), phases_schema.len());
        for (old, new) in phases.iter().zip(phases_schema.iter()) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.status, new.status);
        }
    }

    #[test]
    fn plan_schema_serializes_roundtrip() {
        let schema = PlanSchema::default_schema();
        let yaml = schema.to_yaml().unwrap();
        assert!(yaml.contains("phase_patterns"));
        assert!(yaml.contains("status_marker"));
        let roundtripped: PlanSchema = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(roundtripped.source, schema.source);
        assert_eq!(
            roundtripped.phase_patterns.len(),
            schema.phase_patterns.len()
        );
    }

    #[test]
    fn load_or_default_returns_default_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let schema = PlanSchema::load_or_default(dir.path());
        assert_eq!(schema.source, "PLAN.md");
        assert_eq!(schema.phase_patterns.len(), 2);
    }

    #[test]
    fn load_or_default_loads_custom_schema() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let custom = PlanSchema {
            source: "ROADMAP.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"^##\s+(.+)$".to_string(),
                id_capture: "heading".to_string(),
            }],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: vec!["done".to_string(), "pending".to_string()],
        };
        std::fs::write(
            dir.path().join(".ta/plan-schema.yaml"),
            serde_yaml::to_string(&custom).unwrap(),
        )
        .unwrap();
        let loaded = PlanSchema::load_or_default(dir.path());
        assert_eq!(loaded.source, "ROADMAP.md");
    }

    #[test]
    fn parse_plan_with_custom_schema() {
        let content = r#"# My Roadmap

## Setup
<!-- status: done -->
Get the project started.

## Feature Alpha
<!-- status: in_progress -->
First big feature.

## Release
<!-- status: pending -->
Ship it.
"#;
        let schema = PlanSchema {
            source: "ROADMAP.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"^##\s+(.+)$".to_string(),
                id_capture: "heading".to_string(),
            }],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
        };
        let phases = parse_plan_with_schema(content, &schema);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].id, "Setup");
        assert_eq!(phases[0].status, PlanStatus::Done);
        assert_eq!(phases[1].id, "Feature Alpha");
        assert_eq!(phases[1].status, PlanStatus::InProgress);
        assert_eq!(phases[2].id, "Release");
        assert_eq!(phases[2].status, PlanStatus::Pending);
    }

    #[test]
    fn detect_schema_uses_default_for_standard_plan() {
        let schema = detect_schema_from_content(SAMPLE_PLAN, "PLAN.md");
        assert_eq!(schema.source, "PLAN.md");
        let phases = parse_plan_with_schema(SAMPLE_PLAN, &schema);
        assert!(!phases.is_empty());
    }

    #[test]
    fn detect_schema_falls_back_for_unknown_format() {
        let content = r#"# Random Doc

## Introduction
No status markers here.

## Methods
Also no markers.
"#;
        let schema = detect_schema_from_content(content, "README.md");
        // Should have fallen back to the generic heading pattern.
        assert_eq!(schema.source, "README.md");
        assert_eq!(schema.phase_patterns.len(), 1);
        assert!(schema.phase_patterns[0].regex.contains("##"));
    }

    #[test]
    fn plan_create_templates_are_parseable() {
        for (template_fn, expected_phases) in &[
            (greenfield_plan_template as fn(&str) -> String, 3usize),
            (feature_plan_template as fn(&str) -> String, 3),
            (bugfix_plan_template as fn(&str) -> String, 3),
        ] {
            let content = template_fn("Test Project");
            let phases = parse_plan(&content);
            assert_eq!(
                phases.len(),
                *expected_phases,
                "Template produced wrong phase count"
            );
            assert!(phases.iter().all(|p| p.status == PlanStatus::Pending));
        }
    }

    #[test]
    fn update_phase_status_with_custom_schema() {
        let content = r#"# Roadmap

## Setup
<!-- status: pending -->
Get started.

## Build
<!-- status: pending -->
Build it.
"#;
        let schema = PlanSchema {
            source: "ROADMAP.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"^##\s+(.+)$".to_string(),
                id_capture: "heading".to_string(),
            }],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
        };
        let updated = update_phase_status_with_schema(content, "Setup", PlanStatus::Done, &schema);
        let phases = parse_plan_with_schema(&updated, &schema);
        assert_eq!(phases[0].id, "Setup");
        assert_eq!(phases[0].status, PlanStatus::Done);
        assert_eq!(phases[1].id, "Build");
        assert_eq!(phases[1].status, PlanStatus::Pending);
    }

    #[test]
    fn load_plan_with_custom_schema_and_source() {
        let dir = tempfile::tempdir().unwrap();

        // Write a ROADMAP.md
        std::fs::write(
            dir.path().join("ROADMAP.md"),
            r#"# My Roadmap

## Alpha
<!-- status: done -->

## Beta
<!-- status: pending -->
"#,
        )
        .unwrap();

        // Write a custom schema pointing to ROADMAP.md
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let schema = PlanSchema {
            source: "ROADMAP.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"^##\s+(.+)$".to_string(),
                id_capture: "heading".to_string(),
            }],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
        };
        std::fs::write(
            dir.path().join(".ta/plan-schema.yaml"),
            serde_yaml::to_string(&schema).unwrap(),
        )
        .unwrap();

        // load_plan should use the custom schema and find ROADMAP.md
        let phases = load_plan(dir.path()).unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].id, "Alpha");
        assert_eq!(phases[0].status, PlanStatus::Done);
        assert_eq!(phases[1].id, "Beta");
        assert_eq!(phases[1].status, PlanStatus::Pending);
    }

    #[test]
    fn parse_plan_with_invalid_regex_returns_empty() {
        let schema = PlanSchema {
            source: "PLAN.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"[invalid".to_string(),
                id_capture: "bad".to_string(),
            }],
            status_marker: r"<!--\s*status:\s*(\w+)\s*-->".to_string(),
            statuses: default_statuses(),
        };
        let phases = parse_plan_with_schema(SAMPLE_PLAN, &schema);
        assert!(phases.is_empty());
    }

    #[test]
    fn parse_plan_with_invalid_status_regex_returns_empty() {
        let schema = PlanSchema {
            source: "PLAN.md".to_string(),
            phase_patterns: vec![PhasePattern {
                regex: r"^##\s+Phase\s+(\S+)\s+[—\-]\s+(.+)$".to_string(),
                id_capture: "phase".to_string(),
            }],
            status_marker: r"[invalid".to_string(),
            statuses: default_statuses(),
        };
        let phases = parse_plan_with_schema(SAMPLE_PLAN, &schema);
        assert!(phases.is_empty());
    }

    #[test]
    fn phase_ids_match_normalizes_v_prefix() {
        assert!(phase_ids_match("v0.4.0", "0.4.0"));
        assert!(phase_ids_match("0.4.0", "v0.4.0"));
        assert!(phase_ids_match("v0.4.0", "v0.4.0"));
        assert!(phase_ids_match("0.4.0", "0.4.0"));
        assert!(phase_ids_match("4b", "4b"));
        assert!(!phase_ids_match("v0.4.0", "0.3.0"));
        assert!(!phase_ids_match("4b", "4c"));
    }

    #[test]
    fn update_phase_status_matches_without_v_prefix() {
        // Simulate: PLAN.md has "### v0.4.0 — Title" but goal stores "0.4.0"
        let plan = "### v0.4.0 — Test Phase\n<!-- status: pending -->\n- item\n";
        let updated = update_phase_status(plan, "0.4.0", PlanStatus::Done);
        assert!(
            updated.contains("<!-- status: done -->"),
            "Should match v0.4.0 header when given 0.4.0: {}",
            updated
        );
    }
}
