// setup.rs — Agent-guided setup command (v0.7.2, extended v0.11.4).
//
// `ta setup` launches a conversational flow where a TA agent helps configure
// workflows. The resulting config is a TA draft the user reviews.
//
// - `ta setup wizard` — full interactive setup, generates .ta/ config files
// - `ta setup refine <topic>` — refine a specific aspect of config
// - `ta setup show` — display current resolved configuration
// - `ta setup resolve` — resolve plugins from project.toml (v0.11.4)

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::partitioning::{
    update_gitignore_detailed, update_p4ignore_detailed, VcsBackend, LOCAL_TA_PATHS,
    SHARED_TA_PATHS,
};
use ta_workspace::{
    IgnoreAction, IgnoreReport, VcsDetection, VcsKind, VcsSetupReport, VCS_DETECTION_SCHEMA_VERSION,
};

#[derive(Subcommand, Debug)]
pub enum SetupCommands {
    /// Run the interactive setup wizard to generate .ta/ configuration.
    Wizard {
        /// Which config sections to generate: all, workflow, memory, policy, agents.
        #[arg(long, default_value = "all")]
        template: String,
    },
    /// Refine a specific configuration topic (e.g., "policy", "memory", "agents").
    Refine {
        /// Configuration topic to refine.
        topic: String,
    },
    /// Show the current resolved configuration for this project.
    Show {
        /// Which section to show: all, workflow, memory, policy, channels, plugins.
        #[arg(long, default_value = "all")]
        section: String,
    },
    /// Resolve and install plugins declared in .ta/project.toml.
    ///
    /// Reads the project manifest, checks which plugins are installed,
    /// downloads or builds missing ones, verifies integrity, and reports
    /// environment variable requirements.
    Resolve {
        /// CI mode: non-interactive, fail hard on missing plugins or env vars.
        #[arg(long)]
        ci: bool,
    },
    /// Configure VCS ignore rules for the current project (v0.13.13).
    ///
    /// Detects Git, Subversion, Perforce or a VCS adapter plugin, writes the
    /// ignore rules for local .ta/ runtime state, and reports what changed and
    /// which markers led to the detected VCS (the evidence list).
    ///
    /// Examples:
    ///   ta setup vcs                        # auto-detect VCS and update ignore files
    ///   ta setup vcs --force                # rewrite the TA block even if already present
    ///   ta setup vcs --dry-run              # show what would change without writing
    ///   ta setup vcs --dry-run --json       # machine-readable detection + evidence
    ///   ta setup vcs --vcs svn              # set svn:ignore on .ta (appends only)
    ///   ta setup vcs --project-type rust    # pre-populate [commit] auto_stage with Cargo.lock
    ///   ta setup vcs --project-type node    # pre-populate with package-lock.json
    ///   ta setup vcs --project-type python  # pre-populate with poetry.lock / Pipfile.lock
    ///   ta setup vcs --project-type go      # pre-populate with go.sum
    Vcs {
        /// Rewrite the TA ignore block even if it already exists.
        #[arg(long)]
        force: bool,
        /// Show what would change without writing any files.
        #[arg(long)]
        dry_run: bool,
        /// Override VCS backend: git, svn, perforce, none, or the name of an
        /// installed VCS adapter plugin.
        #[arg(long)]
        vcs: Option<String>,
        /// Project type for auto-stage pre-population: rust, node, python, go.
        /// Auto-detected from project files if not specified.
        #[arg(long)]
        project_type: Option<String>,
        /// Print one JSON document (detection, evidence, what changed) instead
        /// of text. Combine with --dry-run to preview.
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(command: &SetupCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        SetupCommands::Wizard { template } => run_wizard(config, template),
        SetupCommands::Refine { topic } => run_refine(config, topic),
        SetupCommands::Show { section } => show_config(config, section),
        SetupCommands::Resolve { ci } => resolve_plugins(config, *ci),
        SetupCommands::Vcs {
            force,
            dry_run,
            vcs,
            project_type,
            json,
        } => {
            if *json {
                run_vcs_setup_json(
                    config,
                    *force,
                    *dry_run,
                    vcs.as_deref(),
                    project_type.as_deref(),
                )
            } else {
                run_vcs_setup(
                    config,
                    *force,
                    *dry_run,
                    vcs.as_deref(),
                    project_type.as_deref(),
                )
            }
        }
    }
}

/// Resolve `--vcs` to a backend, or fall back to the detected one.
fn resolve_backend(
    project_root: &Path,
    detection: &VcsDetection,
    vcs_override: Option<&str>,
) -> anyhow::Result<(VcsBackend, &'static str)> {
    let Some(name) = vcs_override else {
        return Ok((detection.backend(), "detected"));
    };
    let backend = match name {
        "git" => VcsBackend::Git,
        "svn" => VcsBackend::Svn,
        "perforce" => VcsBackend::Perforce,
        "none" => VcsBackend::None,
        other => {
            if ta_submit::find_vcs_plugin(other, project_root).is_some() {
                VcsBackend::Plugin(other.to_string())
            } else {
                anyhow::bail!(
                    "Unknown VCS backend '{}'. Valid values: git, svn, perforce, none, or the \
                     name of an installed VCS adapter plugin (see .ta/plugins/vcs/ and \
                     ~/.config/ta/plugins/vcs/)",
                    other
                )
            }
        }
    };
    Ok((backend, "override"))
}

fn backend_kind(backend: &VcsBackend) -> (VcsKind, Option<String>) {
    match backend {
        VcsBackend::Git => (VcsKind::Git, None),
        VcsBackend::Svn => (VcsKind::Svn, None),
        VcsBackend::Perforce => (VcsKind::Perforce, None),
        VcsBackend::None => (VcsKind::None, None),
        VcsBackend::Plugin(n) => (VcsKind::Plugin, Some(n.clone())),
    }
}

/// Apply (or preview) a marker-block ignore file update and describe it.
fn apply_ignore_file(
    path: &Path,
    file_name: &str,
    dry_run: bool,
    update: impl FnOnce(&str) -> ta_workspace::IgnoreUpdate,
) -> anyhow::Result<IgnoreReport> {
    let existed = path.exists();
    let existing = if existed {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("could not read {}: {}", path.display(), e))?
    } else {
        String::new()
    };
    let u = update(&existing);
    let action = match (u.changed, existed, dry_run) {
        (false, _, _) => IgnoreAction::Unchanged,
        (true, false, true) => IgnoreAction::WouldCreate,
        (true, true, true) => IgnoreAction::WouldUpdate,
        (true, false, false) => IgnoreAction::Created,
        (true, true, false) => IgnoreAction::Updated,
    };
    if u.changed && !dry_run {
        std::fs::write(path, &u.content).map_err(|e| {
            anyhow::anyhow!(
                "could not write {}: {}. Check permissions, then re-run `ta setup vcs`",
                path.display(),
                e
            )
        })?;
    }
    let note = if u.unregistered.is_empty() {
        None
    } else {
        Some(format!(
            "{} unrecognized .ta/ line(s) in {} were preserved, not removed",
            u.unregistered.len(),
            file_name
        ))
    };
    Ok(IgnoreReport {
        target: file_name.to_string(),
        action,
        added_entries: if u.changed { u.added } else { Vec::new() },
        preserved_unrecognized: u.unregistered,
        note,
    })
}

/// Detect the VCS, write (or preview) the ignore rules, and return what
/// happened. Prints nothing: callers render the report as text or JSON.
pub fn compute_vcs_setup(
    config: &GatewayConfig,
    force: bool,
    dry_run: bool,
    vcs_override: Option<&str>,
    project_type_override: Option<&str>,
) -> anyhow::Result<VcsSetupReport> {
    let project_root = &config.workspace_root;
    let detection = ta_submit::detect_vcs(project_root);
    let (backend, source) = resolve_backend(project_root, &detection, vcs_override)?;
    let (kind, plugin) = backend_kind(&backend);
    let mut notes = Vec::new();
    let mut workflow_changes = Vec::new();

    let ignore = match &backend {
        VcsBackend::Git => {
            let r = apply_ignore_file(
                &project_root.join(".gitignore"),
                ".gitignore",
                dry_run,
                |e| update_gitignore_detailed(e, force),
            )?;
            workflow_changes =
                update_workflow_vcs(project_root, &backend, dry_run, project_type_override)?;
            Some(r)
        }
        VcsBackend::Perforce => {
            let r =
                apply_ignore_file(&project_root.join(".p4ignore"), ".p4ignore", dry_run, |e| {
                    update_p4ignore_detailed(e, force)
                })?;
            if std::env::var("P4IGNORE").is_err() {
                notes.push(
                    "P4IGNORE is not set, so Perforce will not use .p4ignore until you run \
                     `export P4IGNORE=.p4ignore` (add it to your shell profile). Without it, \
                     .ta/staging/, .ta/goals/ and similar may be submitted accidentally."
                        .to_string(),
                );
            }
            workflow_changes =
                update_workflow_vcs(project_root, &backend, dry_run, project_type_override)?;
            Some(r)
        }
        VcsBackend::Svn => {
            let target = "svn:ignore on .ta".to_string();
            let r = match ta_submit::ensure_ta_ignore(project_root, dry_run) {
                Ok(out) => {
                    let action = match (out.added.is_empty(), dry_run) {
                        (true, _) => IgnoreAction::Unchanged,
                        (false, true) => IgnoreAction::WouldUpdate,
                        (false, false) => IgnoreAction::Updated,
                    };
                    let note = out.applied.then(|| {
                        "svn:ignore changed in the working copy; commit .ta with `svn commit` \
                         to share it"
                            .to_string()
                    });
                    IgnoreReport {
                        target,
                        action,
                        added_entries: out.added,
                        preserved_unrecognized: Vec::new(),
                        note,
                    }
                }
                Err(e) => IgnoreReport {
                    target,
                    action: IgnoreAction::Skipped,
                    added_entries: Vec::new(),
                    preserved_unrecognized: Vec::new(),
                    note: Some(e.to_string()),
                },
            };
            workflow_changes =
                update_workflow_vcs(project_root, &backend, dry_run, project_type_override)?;
            Some(r)
        }
        VcsBackend::Plugin(name) => {
            notes.push(format!(
                "VCS adapter plugin '{name}' owns this project. TA does not know its ignore \
                 format, so no ignore file was written. Add the local .ta/ paths listed by \
                 `ta plan shared` to that VCS's ignore mechanism by hand."
            ));
            workflow_changes =
                update_workflow_vcs(project_root, &backend, dry_run, project_type_override)?;
            None
        }
        VcsBackend::None => {
            notes.push(
                "No VCS detected, so no ignore file was generated. Run `ta setup vcs --vcs git`, \
                 `--vcs svn` or `--vcs perforce` to override."
                    .to_string(),
            );
            None
        }
    };

    Ok(VcsSetupReport {
        schema_version: VCS_DETECTION_SCHEMA_VERSION,
        dry_run,
        forced: force,
        source: source.to_string(),
        kind,
        plugin,
        detection,
        ignore,
        workflow_changes,
        notes,
    })
}

fn action_label(a: IgnoreAction) -> &'static str {
    match a {
        IgnoreAction::Created => "[created]",
        IgnoreAction::Updated => "[updated]",
        IgnoreAction::Unchanged => "[ok]",
        IgnoreAction::WouldCreate => "[dry-run] Would create",
        IgnoreAction::WouldUpdate => "[dry-run] Would update",
        IgnoreAction::Skipped => "[skipped]",
    }
}

/// Print the human-readable form of a setup report. `with_paths` adds the
/// shared/local `.ta/` path status listing.
pub fn print_vcs_setup(config: &GatewayConfig, report: &VcsSetupReport, with_paths: bool) {
    let project_root = &config.workspace_root;
    let name = match &report.plugin {
        Some(p) => format!("plugin:{p}"),
        None => report.kind.as_str().to_string(),
    };
    println!("VCS: {} ({})", name, report.source);
    if report.source == "override" {
        println!("  detected: {}", report.detection.summary());
    }
    for e in &report.detection.evidence {
        println!("  evidence: {} at {}: {}", e.marker, e.path, e.reason);
    }
    for e in &report.detection.ignored_evidence {
        println!(
            "  also found (not used): {} at {} ({})",
            e.marker,
            e.path,
            e.kind.as_str()
        );
    }
    if report.detection.evidence.is_empty() && report.detection.ignored_evidence.is_empty() {
        println!(
            "  no VCS marker found; checked: {}",
            report.detection.checked.join(", ")
        );
    }

    if let Some(ig) = &report.ignore {
        match ig.action {
            IgnoreAction::Unchanged => println!(
                "  [ok] {} already contains the TA block. No change needed. Use --force to rewrite it.",
                ig.target
            ),
            IgnoreAction::Skipped => println!(
                "  [skipped] {}: {}",
                ig.target,
                ig.note.as_deref().unwrap_or("see notes")
            ),
            a => {
                println!(
                    "  {} {} ({} entr{} added)",
                    action_label(a),
                    ig.target,
                    ig.added_entries.len(),
                    if ig.added_entries.len() == 1 { "y" } else { "ies" }
                );
                for entry in &ig.added_entries {
                    println!("      + {entry}");
                }
                if let Some(n) = &ig.note {
                    println!("  {n}");
                }
            }
        }
        warn_unregistered_ta_lines(&ig.preserved_unrecognized, &ig.target);
    }
    for c in &report.workflow_changes {
        if report.dry_run {
            println!("  [dry-run] Would update .ta/workflow.toml: {c}");
        } else {
            println!("  [updated] .ta/workflow.toml: {c}");
        }
    }
    for n in &report.notes {
        println!();
        println!("  Note: {n}");
    }

    if !with_paths {
        return;
    }
    let backend = match (report.kind, &report.plugin) {
        (VcsKind::Git, _) => VcsBackend::Git,
        (VcsKind::Svn, _) => VcsBackend::Svn,
        (VcsKind::Perforce, _) => VcsBackend::Perforce,
        (VcsKind::Plugin, Some(p)) => VcsBackend::Plugin(p.clone()),
        _ => VcsBackend::None,
    };
    println!();
    println!("Shared (commit to VCS):");
    for path in SHARED_TA_PATHS {
        let full = project_root.join(".ta").join(path.trim_end_matches('/'));
        let status = if full.exists() {
            "[present]"
        } else {
            "[missing]"
        };
        println!("  .ta/{}  {}", path, status);
    }
    println!();
    println!("Local (should be ignored):");
    for path in LOCAL_TA_PATHS {
        let full = project_root.join(".ta").join(path.trim_end_matches('/'));
        let status = if !full.exists() {
            "[absent]"
        } else {
            match backend.is_path_ignored(project_root, path) {
                Ok(true) => "[ignored ✓]",
                Ok(false) => "[NOT IGNORED ⚠ — run `ta doctor` to fix]",
                Err(_) => "[ignore status unknown]",
            }
        };
        println!("  .ta/{}  {}", path, status);
    }
}

/// Fail (after the report is printed) when a real run could not apply the
/// ignore rules, so scripts see a non-zero exit instead of a quiet no-op.
fn fail_if_skipped(report: &VcsSetupReport) -> anyhow::Result<()> {
    if let Some(ig) = &report.ignore {
        if ig.action == IgnoreAction::Skipped && !report.dry_run {
            anyhow::bail!(
                "could not apply ignore rules to {}: {}",
                ig.target,
                ig.note.as_deref().unwrap_or("unknown error")
            );
        }
    }
    Ok(())
}

/// VCS setup: detect VCS backend, write ignore files, update workflow.toml.
pub fn run_vcs_setup(
    config: &GatewayConfig,
    force: bool,
    dry_run: bool,
    vcs_override: Option<&str>,
    project_type_override: Option<&str>,
) -> anyhow::Result<()> {
    let report = compute_vcs_setup(config, force, dry_run, vcs_override, project_type_override)?;
    print_vcs_setup(config, &report, true);
    fail_if_skipped(&report)
}

/// Like [`run_vcs_setup`] but prints only the JSON report on stdout.
pub fn run_vcs_setup_json(
    config: &GatewayConfig,
    force: bool,
    dry_run: bool,
    vcs_override: Option<&str>,
    project_type_override: Option<&str>,
) -> anyhow::Result<()> {
    let report = compute_vcs_setup(config, force, dry_run, vcs_override, project_type_override)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    fail_if_skipped(&report)
}

/// Print a warning for any `.ta/`-prefixed line preserved from the existing
/// ignore file that TA doesn't recognize (not in `LOCAL_TA_PATHS` or
/// `SHARED_TA_PATHS`). These lines are never dropped — see
/// `update_gitignore`/`update_p4ignore` in `ta_workspace::partitioning` — but
/// an unrecognized entry usually means either a real TA runtime path that
/// was never registered (the v0.17.0.12.32 incident) or stale content that
/// can be cleaned up by hand.
fn warn_unregistered_ta_lines(unregistered: &[String], ignore_file_name: &str) {
    if unregistered.is_empty() {
        return;
    }
    println!();
    println!(
        "  ⚠ Found {} unrecognized .ta/-prefixed line(s) in {} that are not in \
         LOCAL_TA_PATHS/SHARED_TA_PATHS (crates/ta-workspace/src/partitioning.rs):",
        unregistered.len(),
        ignore_file_name
    );
    for line in unregistered {
        println!("      {}", line);
    }
    println!(
        "    Preserved as-is — not removed. If this protects a real TA runtime file, \
         register it in LOCAL_TA_PATHS so future `--force` runs regenerate it consistently."
    );
}

/// Write or update the `[submit] adapter = "..."` field in `.ta/workflow.toml`.
///
/// If `workflow.toml` doesn't exist, nothing is written — the user may not have
/// a submit workflow configured yet.  If it does exist, we ensure the `[submit]`
/// section has `adapter = "<vcs>"`, creating the section if absent.
///
/// Also populates `[commit] auto_stage` with the lock file(s) for the detected
/// or specified project type (v0.14.3.7).
fn update_workflow_vcs(
    project_root: &std::path::Path,
    backend: &VcsBackend,
    dry_run: bool,
    project_type_override: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut changes: Vec<String> = Vec::new();
    let ta_dir = project_root.join(".ta");
    let workflow_path = ta_dir.join("workflow.toml");

    if !workflow_path.exists() {
        return Ok(changes); // Don't create workflow.toml; just update if it exists.
    }

    let existing = std::fs::read_to_string(&workflow_path)?;
    let vcs_str = backend.as_str();

    // ── [submit] adapter ────────────────────────────────────────────
    let mut updated = existing.clone();

    // Check if [submit] section already has adapter = "<vcs>" — nothing to do.
    if !updated.contains(&format!("adapter = \"{}\"", vcs_str)) {
        // Simple text manipulation: insert/update adapter = "..." in [submit] section.
        updated = if updated.contains("[submit]") {
            // Replace whatever adapter = "..." was set to.
            let re = regex::Regex::new(r#"adapter\s*=\s*"[^"]*""#).unwrap();
            let replaced = re.replace(&updated, &format!("adapter = \"{}\"", vcs_str));
            if replaced == updated {
                // adapter key not yet in [submit] — append after [submit] header.
                updated.replace("[submit]", &format!("[submit]\nadapter = \"{}\"", vcs_str))
            } else {
                replaced.to_string()
            }
        } else {
            // Append [submit] section at end.
            format!(
                "{}\n[submit]\nadapter = \"{}\"\n",
                updated.trim_end(),
                vcs_str
            )
        };

        changes.push(format!("set [submit] adapter = \"{}\"", vcs_str));
    }

    // ── [commit] auto_stage ─────────────────────────────────────────
    // Determine which lock file(s) to add based on project type.
    let lock_files = lock_files_for_project_type(project_root, project_type_override);
    if !lock_files.is_empty() {
        updated = add_auto_stage_entries(&updated, &lock_files);
        if updated != existing {
            changes.push(format!(
                "add [commit] auto_stage: {}",
                lock_files.join(", ")
            ));
        }
    }

    if !dry_run && updated != existing {
        std::fs::write(&workflow_path, &updated)?;
    }
    Ok(changes)
}

/// Return the lock file name(s) appropriate for the given project type.
///
/// When `project_type_override` is provided, it takes precedence. Otherwise,
/// the project root is scanned for known lock files to auto-detect.
fn lock_files_for_project_type(
    project_root: &std::path::Path,
    project_type_override: Option<&str>,
) -> Vec<String> {
    let pt = project_type_override.unwrap_or("");
    match pt {
        "rust" => vec!["Cargo.lock".to_string()],
        "node" => vec!["package-lock.json".to_string()],
        "python" => {
            // Prefer poetry.lock if both exist; also accept Pipfile.lock.
            let mut result = Vec::new();
            if project_root.join("poetry.lock").exists() {
                result.push("poetry.lock".to_string());
            } else if project_root.join("Pipfile.lock").exists() {
                result.push("Pipfile.lock".to_string());
            } else {
                result.push("poetry.lock".to_string());
            }
            result
        }
        "go" => vec!["go.sum".to_string()],
        _ => {
            // Auto-detect from project root.
            ta_submit::GitAdapter::BUILTIN_LOCK_FILES
                .iter()
                .filter(|f| project_root.join(f).exists())
                .map(|f| f.to_string())
                .collect()
        }
    }
}

/// Add entries to `[commit] auto_stage` in workflow.toml content (TOML text).
///
/// Skips entries that are already present. Appends a `[commit]` section if absent.
fn add_auto_stage_entries(toml_content: &str, entries: &[String]) -> String {
    // Find which entries are not already present.
    let missing: Vec<&str> = entries
        .iter()
        .filter(|e| !toml_content.contains(e.as_str()))
        .map(|e| e.as_str())
        .collect();
    if missing.is_empty() {
        return toml_content.to_string();
    }

    let new_lines: String = missing
        .iter()
        .map(|e| format!("    \"{}\",\n", e))
        .collect();

    if toml_content.contains("[commit]") {
        // Find `auto_stage = [` and append before the closing `]`.
        if let Some(pos) = toml_content.find("auto_stage = [") {
            // Find the closing `]` after the opening `[`.
            let after_bracket = pos + "auto_stage = [".len();
            if let Some(close) = toml_content[after_bracket..].find(']') {
                let close_abs = after_bracket + close;
                let mut result = toml_content[..close_abs].to_string();
                // If the existing content before `]` has entries, ensure newline.
                if !result.trim_end_matches([' ', '\t']).ends_with('[') && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(&new_lines);
                result.push_str(&toml_content[close_abs..]);
                return result;
            }
        }
        // [commit] exists but no auto_stage key — append after [commit] header.
        let commit_pos = toml_content.find("[commit]").unwrap();
        let after_header = commit_pos + "[commit]".len();
        let auto_stage_block = format!(
            "\nauto_stage = [\n{}]",
            missing
                .iter()
                .map(|e| format!("    \"{}\",\n", e))
                .collect::<String>()
        );
        let mut result = toml_content[..after_header].to_string();
        result.push_str(&auto_stage_block);
        result.push_str(&toml_content[after_header..]);
        result
    } else {
        // Append a new [commit] section.
        let block = format!(
            "\n[commit]\nauto_stage = [\n{}]\n",
            missing
                .iter()
                .map(|e| format!("    \"{}\",\n", e))
                .collect::<String>()
        );
        format!("{}{}", toml_content.trim_end(), block)
    }
}

/// Run the interactive setup wizard.
///
/// Detects project type, generates sensible defaults, and writes them
/// as proposed config files. In full TA mode, this would launch an agent
/// goal that produces a draft. For now, generates config directly.
fn run_wizard(config: &GatewayConfig, template: &str) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    // Detect project type.
    let schema = ta_memory::key_schema::KeySchema::resolve(project_root);
    println!("Detected project type: {}", schema.project_type);
    println!("Backend: {}", schema.backend);
    println!();

    let sections: Vec<&str> = if template == "all" {
        vec!["workflow", "memory", "policy", "agents"]
    } else {
        vec![template]
    };

    std::fs::create_dir_all(&ta_dir)?;

    for section in &sections {
        match *section {
            "workflow" => generate_workflow_config(&ta_dir, &schema)?,
            "memory" => generate_memory_config(&ta_dir, &schema)?,
            "policy" => generate_policy_config(&ta_dir)?,
            "agents" => generate_agent_config(&ta_dir)?,
            other => {
                eprintln!("Unknown template section: {}", other);
            }
        }
    }

    println!();
    println!("Setup complete. Review the generated files in .ta/");
    println!("Run `ta setup show` to inspect the resolved configuration.");

    Ok(())
}

/// Refine a specific configuration topic.
fn run_refine(config: &GatewayConfig, topic: &str) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let schema = ta_memory::key_schema::KeySchema::resolve(&config.workspace_root);

    match topic {
        "workflow" => {
            println!("Refining workflow configuration...");
            generate_workflow_config(&ta_dir, &schema)?;
        }
        "memory" => {
            println!("Refining memory configuration...");
            generate_memory_config(&ta_dir, &schema)?;
        }
        "policy" => {
            println!("Refining policy configuration...");
            generate_policy_config(&ta_dir)?;
        }
        "agents" => {
            println!("Refining agent configuration...");
            generate_agent_config(&ta_dir)?;
        }
        "channels" => {
            println!("Refining channel configuration...");
            generate_channel_config(&ta_dir)?;
        }
        _ => {
            anyhow::bail!(
                "Unknown topic: '{}'. Available: workflow, memory, policy, agents, channels",
                topic
            );
        }
    }

    Ok(())
}

/// Show resolved configuration.
fn show_config(config: &GatewayConfig, section: &str) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    let sections: Vec<&str> = if section == "all" {
        vec![
            "project", "workflow", "memory", "policy", "channels", "plugins",
        ]
    } else {
        vec![section]
    };

    for s in &sections {
        match *s {
            "project" => {
                let schema = ta_memory::key_schema::KeySchema::resolve(project_root);
                println!("=== Project ===");
                println!("  Type: {}", schema.project_type);
                println!("  Backend: {}", schema.backend);
                println!("  Key domains:");
                println!("    module_map: {}", schema.domains.module_map);
                println!("    module: {}", schema.domains.module);
                println!("    type_system: {}", schema.domains.type_system);
                println!("    build_tool: {}", schema.domains.build_tool);
                println!();
            }
            "workflow" => {
                show_file_if_exists(&ta_dir.join("workflow.toml"), "Workflow")?;
            }
            "memory" => {
                show_file_if_exists(&ta_dir.join("memory.toml"), "Memory")?;
            }
            "policy" => {
                show_file_if_exists(&ta_dir.join("policy.yaml"), "Policy")?;
            }
            "channels" => {
                show_file_if_exists(&ta_dir.join("config.yaml"), "Channels")?;
            }
            "plugins" => {
                show_plugins(project_root)?;
            }
            other => {
                eprintln!("Unknown section: {}", other);
            }
        }
    }

    Ok(())
}

fn show_file_if_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    println!("=== {} ===", label);
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        println!("{}", content);
    } else {
        println!("  (not configured — run `ta setup wizard` to generate)");
    }
    println!();
    Ok(())
}

fn generate_workflow_config(
    ta_dir: &Path,
    _schema: &ta_memory::key_schema::KeySchema,
) -> anyhow::Result<()> {
    let path = ta_dir.join("workflow.toml");
    if path.exists() {
        println!(
            "  workflow.toml already exists — skipping (use `ta setup refine workflow` to update)"
        );
        return Ok(());
    }

    let content = r#"# TA Workflow Configuration
# Generated by `ta setup wizard`

[memory.auto_capture]
on_goal_complete = true
on_draft_reject = true
on_human_guidance = true
on_repeated_correction = true
correction_threshold = 3
max_context_entries = 10

# VCS submit adapter: controls branch naming and PR creation.
# `ta draft apply` (without --no-submit) uses this to create the branch and PR.
[submit.git]
# Branch prefix for agent-created branches. Default: "feature/"
# Change to "ta/" if your team prefers a TA-namespaced convention.
branch_prefix = "feature/"
# Auto-create a pull request when the branch is pushed.
auto_review = true
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/workflow.toml");
    Ok(())
}

fn generate_memory_config(
    ta_dir: &Path,
    schema: &ta_memory::key_schema::KeySchema,
) -> anyhow::Result<()> {
    let path = ta_dir.join("memory.toml");
    if path.exists() {
        println!("  memory.toml already exists — skipping");
        return Ok(());
    }

    let content = format!(
        r#"# TA Memory Configuration
# Generated by `ta setup wizard`
# Project type: {}

backend = "{}"

[project]
type = "{}"

[key_domains]
module_map = "{}"
module = "{}"
type_system = "{}"
build_tool = "{}"
"#,
        schema.project_type,
        schema.backend,
        schema.project_type,
        schema.domains.module_map,
        schema.domains.module,
        schema.domains.type_system,
        schema.domains.build_tool,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/memory.toml");
    Ok(())
}

fn generate_policy_config(ta_dir: &Path) -> anyhow::Result<()> {
    let path = ta_dir.join("policy.yaml");
    if path.exists() {
        println!("  policy.yaml already exists — skipping");
        return Ok(());
    }

    let content = r#"# TA Policy Configuration
# Generated by `ta setup wizard`
# See docs/USAGE.md for full policy reference.

security_level: checkpoint

defaults:
  enforcement: error
  auto_approve:
    verbs:
      - read
      - list
      - search

schemes:
  fs:
    auto_approve_verbs:
      - read
      - list

escalation:
  drift_threshold: 0.7
  action_limit: 100
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/policy.yaml");
    Ok(())
}

fn generate_agent_config(ta_dir: &Path) -> anyhow::Result<()> {
    let agents_dir = ta_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    // Use framework registry to detect installed frameworks.
    let project_root = ta_dir.parent();
    let registry = crate::framework_registry::FrameworkRegistry::load(project_root);
    let installed = registry.detect_installed();

    println!("  Framework detection:");
    if installed.is_empty() {
        println!("    No frameworks detected on PATH.");
        println!("    Generating default claude-code config.");

        let path = agents_dir.join("claude-code.yaml");
        if !path.exists() {
            let content = r#"# Agent Configuration: Claude Code
# Generated by `ta setup wizard`

name: claude-code
command: claude
args_template:
  - "{prompt}"
injects_context_file: true
injects_settings: true
"#;
            std::fs::write(&path, content)?;
            println!("    Created .ta/agents/claude-code.yaml");
        } else {
            println!("    agents/claude-code.yaml already exists — skipping");
        }
    } else {
        for (id, entry) in &installed {
            let path = agents_dir.join(format!("{}.yaml", id));
            if path.exists() {
                println!(
                    "    [installed] {} — agents/{}.yaml already exists",
                    entry.name, id
                );
                continue;
            }
            let content = format!(
                "# Agent Configuration: {}\n# Generated by `ta setup wizard`\n# Detected on PATH\n\nname: {}\ncommand: {}\nargs_template:\n  - \"{{prompt}}\"\ninjects_context_file: {}\ninjects_settings: {}\n",
                entry.name,
                id,
                entry.detect.first().unwrap_or(&id.to_string()),
                *id == "claude-code" || *id == "bmad",
                *id == "claude-code" || *id == "bmad",
            );
            std::fs::write(&path, content)?;
            println!(
                "    [installed] {} — created agents/{}.yaml",
                entry.name, id
            );
        }
    }

    // Show available-but-not-installed frameworks.
    let available: Vec<_> = registry
        .detect_available()
        .into_iter()
        .filter(|(_, e)| !e.detect.is_empty())
        .collect();
    if !available.is_empty() {
        println!();
        println!("  Available frameworks (not yet installed):");
        for (id, entry) in &available {
            let install = entry
                .install
                .as_ref()
                .map(|i| i.for_current_platform().to_string())
                .unwrap_or_else(|| "See homepage".to_string());
            println!("    {} — install: {}", id, install);
            if let Some(ref hp) = entry.homepage {
                println!("      {}", hp);
            }
        }
        println!("    Run `ta setup refine agents` after installing to generate configs.");
    }

    Ok(())
}

fn generate_channel_config(ta_dir: &Path) -> anyhow::Result<()> {
    let path = ta_dir.join("config.yaml");
    if path.exists() {
        println!("  config.yaml already exists — skipping");
        return Ok(());
    }

    let content = r#"# TA Channel Configuration
# Generated by `ta setup refine channels`

channels:
  review:
    type: terminal
  session:
    type: terminal
  notify: []
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/config.yaml");
    Ok(())
}

/// Show plugin status: manifest requirements vs installed plugins.
fn show_plugins(project_root: &Path) -> anyhow::Result<()> {
    use ta_changeset::project_manifest::ProjectManifest;
    use ta_changeset::registry_client::detect_platform;

    println!("=== Plugins ===");
    println!("  Platform: {}", detect_platform());

    if !ProjectManifest::exists(project_root) {
        println!("  (no .ta/project.toml — run `ta setup resolve` after creating one)");
        println!();
        return Ok(());
    }

    match ProjectManifest::load(project_root) {
        Ok(manifest) => {
            println!("  Project: {}", manifest.project.name);
            if manifest.plugins.is_empty() {
                println!("  No plugins declared.");
            } else {
                let installed = ta_changeset::plugin::discover_plugins(project_root);
                println!("  Declared plugins:");
                for (name, req) in &manifest.plugins {
                    let status = match installed.iter().find(|p| p.manifest.name == *name) {
                        Some(p) => {
                            if ta_changeset::project_manifest::version_satisfies(
                                &p.manifest.version,
                                &req.version,
                            ) {
                                format!("installed (v{})", p.manifest.version)
                            } else {
                                format!("outdated (v{}, needs {})", p.manifest.version, req.version)
                            }
                        }
                        None => "missing".to_string(),
                    };
                    let req_label = if req.required { "" } else { " (optional)" };
                    println!(
                        "    {} [{}] {} — {}{}",
                        name, req.plugin_type, req.version, status, req_label
                    );
                }
            }
        }
        Err(e) => {
            println!("  Error loading project.toml: {}", e);
        }
    }

    println!();
    Ok(())
}

/// Resolve and install plugins from .ta/project.toml.
fn resolve_plugins(config: &GatewayConfig, ci_mode: bool) -> anyhow::Result<()> {
    use ta_changeset::plugin_resolver::{resolve_all, PluginResolveResult};
    use ta_changeset::project_manifest::ProjectManifest;
    use ta_changeset::registry_client::detect_platform;

    let project_root = &config.workspace_root;

    println!("Platform: {}", detect_platform());

    let manifest = match ProjectManifest::load(project_root) {
        Ok(m) => m,
        Err(ta_changeset::project_manifest::ManifestError::NotFound { path }) => {
            if ci_mode {
                anyhow::bail!(
                    "No project manifest found at {}. \
                     Create .ta/project.toml to declare plugin requirements.",
                    path.display()
                );
            }
            println!("No .ta/project.toml found.");
            println!();
            println!("Create one to declare plugin requirements:");
            println!();
            println!("  [project]");
            println!("  name = \"my-project\"");
            println!();
            println!("  [plugins.discord]");
            println!("  type = \"channel\"");
            println!("  version = \">=0.1.0\"");
            println!("  source = \"registry:ta-channel-discord\"");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    println!("Project: {}", manifest.project.name);
    if manifest.plugins.is_empty() {
        println!("No plugins declared in project.toml.");
        return Ok(());
    }

    println!("Resolving {} plugin(s)...", manifest.plugins.len());
    println!();

    let report = resolve_all(&manifest, project_root, ci_mode);

    // Print results.
    for result in &report.results {
        match result {
            PluginResolveResult::AlreadyInstalled {
                name,
                installed_version,
            } => {
                println!(
                    "  [ok]      {} v{} — already installed",
                    name, installed_version
                );
            }
            PluginResolveResult::Installed {
                name,
                version,
                source,
            } => {
                println!("  [install] {} v{} — from {}", name, version, source);
            }
            PluginResolveResult::BuiltFromSource { name, source_path } => {
                println!(
                    "  [build]   {} — built from {}",
                    name,
                    source_path.display()
                );
            }
            PluginResolveResult::Failed { name, reason } => {
                println!("  [FAIL]    {} — {}", name, reason);
            }
            PluginResolveResult::Skipped { name, reason } => {
                println!("  [skip]    {} — {} (optional)", name, reason);
            }
        }
    }

    // Print environment variable warnings.
    if !report.missing_env_vars.is_empty() {
        println!();
        println!("Missing environment variables:");
        for (plugin, vars) in &report.missing_env_vars {
            for var in vars {
                println!("  {} needs ${}", plugin, var);
            }
        }
        if ci_mode {
            anyhow::bail!(
                "Missing required environment variables. Set them and re-run `ta setup resolve --ci`."
            );
        } else {
            println!();
            println!("Set these variables before starting the daemon.");
            println!("Plugins may still work partially without them.");
        }
    }

    // Summary.
    println!();
    println!(
        "Resolved: {} ok, {} failed, {} skipped",
        report.success_count(),
        report.failure_count(),
        report
            .results
            .iter()
            .filter(|r| matches!(r, PluginResolveResult::Skipped { .. }))
            .count()
    );

    if !report.all_ok() {
        if ci_mode {
            anyhow::bail!(
                "Plugin resolution failed. {} plugin(s) could not be installed.",
                report.failure_count()
            );
        } else {
            println!();
            println!("Some plugins failed to install. Check the errors above.");
            println!("You can re-run `ta setup resolve` after fixing the issues.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(dir.path())
    }

    #[test]
    fn wizard_generates_all_configs() {
        let dir = TempDir::new().unwrap();
        // Create Cargo.toml to detect Rust project.
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let config = test_config(&dir);
        run_wizard(&config, "all").unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("workflow.toml").exists());
        assert!(ta_dir.join("memory.toml").exists());
        assert!(ta_dir.join("policy.yaml").exists());
        assert!(ta_dir.join("agents").join("claude-code.yaml").exists());

        // Verify memory.toml has Rust-specific content.
        let memory_content = std::fs::read_to_string(ta_dir.join("memory.toml")).unwrap();
        assert!(memory_content.contains("rust-workspace"));
        assert!(memory_content.contains("crate-map"));
    }

    #[test]
    fn wizard_skips_existing_files() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), "# existing\n").unwrap();

        let config = test_config(&dir);
        run_wizard(&config, "workflow").unwrap();

        // Should not overwrite.
        let content = std::fs::read_to_string(ta_dir.join("workflow.toml")).unwrap();
        assert_eq!(content, "# existing\n");
    }

    #[test]
    fn show_config_no_files() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Should not panic when no .ta/ files exist.
        show_config(&config, "all").unwrap();
    }

    #[test]
    fn refine_single_section() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_refine(&config, "policy").unwrap();
        assert!(dir.path().join(".ta").join("policy.yaml").exists());
    }

    #[test]
    fn refine_unknown_topic_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = run_refine(&config, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_no_manifest_interactive() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Should not error in interactive mode when no manifest exists.
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_no_manifest_ci_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // CI mode should error when no manifest exists.
        let result = resolve_plugins(&config, true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_empty_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("project.toml"), "[project]\nname = \"test\"\n").unwrap();

        let config = test_config(&dir);
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_with_already_installed_plugin() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Create project.toml.
        std::fs::write(
            ta_dir.join("project.toml"),
            r#"[project]
name = "test"

[plugins.test-plug]
type = "channel"
version = ">=0.1.0"
source = "path:./nonexistent"
"#,
        )
        .unwrap();

        // Install the plugin manually.
        let plugin_dir = ta_dir.join("plugins").join("channels").join("test-plug");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "test-plug"
version = "0.2.0"
command = "test"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let config = test_config(&dir);
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn show_plugins_no_manifest() {
        let dir = TempDir::new().unwrap();
        // Should not panic.
        show_plugins(dir.path()).unwrap();
    }

    #[test]
    fn show_plugins_with_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("project.toml"),
            r#"[project]
name = "test"

[plugins.slack]
type = "channel"
version = ">=0.1.0"
source = "registry:ta-channel-slack"
"#,
        )
        .unwrap();

        show_plugins(dir.path()).unwrap();
    }

    // ── VCS setup tests ──────────────────────────────────────────

    #[test]
    fn vcs_setup_git_creates_gitignore() {
        let dir = TempDir::new().unwrap();
        // Create .git/ so Git is detected.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, false, false, None, None).unwrap();
        let gitignore = dir.path().join(".gitignore");
        assert!(gitignore.exists(), ".gitignore should be created");
        let content = std::fs::read_to_string(&gitignore).unwrap();
        assert!(content.contains("# Trusted Autonomy"));
        assert!(content.contains(".ta/staging/"));
        assert!(content.contains(".ta/goals/"));
    }

    #[test]
    fn vcs_setup_git_idempotent() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let config = test_config(&dir);
        // Run twice — should not duplicate the block.
        run_vcs_setup(&config, false, false, None, None).unwrap();
        run_vcs_setup(&config, false, false, None, None).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let count = content.matches("# Trusted Autonomy").count();
        assert_eq!(count, 1, "should only have one TA block after two runs");
    }

    #[test]
    fn vcs_setup_git_force_rewrites() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "# Trusted Autonomy — local runtime state (do not commit)\n.ta/staging/\n",
        )
        .unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, true, false, None, None).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let count = content.matches("# Trusted Autonomy").count();
        assert_eq!(count, 1, "force should rewrite, not duplicate");
    }

    /// Regression test for v0.17.0.12.32: a `.ta/`-prefixed line manually
    /// added to `.gitignore` that TA doesn't recognize (not in
    /// LOCAL_TA_PATHS/SHARED_TA_PATHS) must survive `ta setup vcs --force`
    /// end-to-end, not just at the `update_gitignore` unit level.
    #[test]
    fn vcs_setup_git_force_preserves_unregistered_entry() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "# Trusted Autonomy — local runtime state (do not commit)\n\
             .ta/staging/\n.ta/some-real-file.jsonl\n.ta/goals/\n",
        )
        .unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, true, false, None, None).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(
            content.contains(".ta/some-real-file.jsonl"),
            "unrecognized entry must survive --force regeneration, not be silently dropped:\n{}",
            content
        );
    }

    #[test]
    fn vcs_setup_dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, false, true, None, None).unwrap();
        // dry-run should NOT create .gitignore.
        assert!(
            !dir.path().join(".gitignore").exists(),
            ".gitignore should not be written in dry-run mode"
        );
    }

    #[test]
    fn vcs_setup_explicit_none() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Should not fail even when VCS is forced to none.
        run_vcs_setup(&config, false, false, Some("none"), None).unwrap();
        assert!(!dir.path().join(".gitignore").exists());
    }

    #[test]
    fn vcs_setup_unknown_backend_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = run_vcs_setup(&config, false, false, Some("bogus"), None);
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Valid values"), "{msg}");
    }

    // ── v0.14.3.7: [commit] auto_stage helpers ───────────────────

    #[test]
    fn add_auto_stage_entries_appends_new_commit_section() {
        let toml = "[submit]\nadapter = \"git\"\n";
        let result = add_auto_stage_entries(toml, &["Cargo.lock".to_string()]);
        assert!(result.contains("[commit]"), "should add [commit] section");
        assert!(
            result.contains("\"Cargo.lock\""),
            "should add Cargo.lock entry"
        );
    }

    #[test]
    fn add_auto_stage_entries_appends_to_existing_commit_section() {
        let toml =
            "[submit]\nadapter = \"git\"\n\n[commit]\nauto_stage = [\n    \"existing.lock\",\n]\n";
        let result = add_auto_stage_entries(toml, &["Cargo.lock".to_string()]);
        assert!(
            result.contains("\"existing.lock\""),
            "should keep existing entry"
        );
        assert!(result.contains("\"Cargo.lock\""), "should add new entry");
    }

    #[test]
    fn add_auto_stage_entries_skips_already_present() {
        let toml = "[commit]\nauto_stage = [\n    \"Cargo.lock\",\n]\n";
        let result = add_auto_stage_entries(toml, &["Cargo.lock".to_string()]);
        let count = result.matches("\"Cargo.lock\"").count();
        assert_eq!(count, 1, "Cargo.lock should appear exactly once");
    }

    #[test]
    fn lock_files_for_project_type_rust_override() {
        let dir = TempDir::new().unwrap();
        let files = lock_files_for_project_type(dir.path(), Some("rust"));
        assert_eq!(files, vec!["Cargo.lock"]);
    }

    #[test]
    fn lock_files_for_project_type_node_override() {
        let dir = TempDir::new().unwrap();
        let files = lock_files_for_project_type(dir.path(), Some("node"));
        assert_eq!(files, vec!["package-lock.json"]);
    }

    #[test]
    fn lock_files_for_project_type_go_override() {
        let dir = TempDir::new().unwrap();
        let files = lock_files_for_project_type(dir.path(), Some("go"));
        assert_eq!(files, vec!["go.sum"]);
    }

    #[test]
    fn lock_files_for_project_type_python_prefers_poetry() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("poetry.lock"), "").unwrap();
        let files = lock_files_for_project_type(dir.path(), Some("python"));
        assert_eq!(files, vec!["poetry.lock"]);
    }

    #[test]
    fn lock_files_for_project_type_auto_detect_from_disk() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "").unwrap();
        let files = lock_files_for_project_type(dir.path(), None);
        assert!(files.contains(&"Cargo.lock".to_string()));
        // Non-existent lock files should not appear.
        assert!(!files.contains(&"go.sum".to_string()));
    }

    #[test]
    fn update_workflow_vcs_adds_commit_auto_stage() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[submit]\nadapter = \"git\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "").unwrap();
        update_workflow_vcs(
            dir.path(),
            &ta_workspace::partitioning::VcsBackend::Git,
            false,
            None,
        )
        .unwrap();
        let content = std::fs::read_to_string(dir.path().join(".ta/workflow.toml")).unwrap();
        assert!(
            content.contains("\"Cargo.lock\""),
            "workflow.toml should have Cargo.lock in auto_stage"
        );
    }

    // ── v0.17.12.5: shared detector, evidence, svn, re-run ───────

    #[test]
    fn setup_report_carries_evidence_and_dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, true, None, None).unwrap();
        assert_eq!(report.kind, VcsKind::Git);
        assert_eq!(report.source, "detected");
        assert!(report.dry_run);
        assert_eq!(report.detection.evidence[0].path, ".git");
        let ig = report.ignore.as_ref().unwrap();
        assert_eq!(ig.action, IgnoreAction::WouldCreate);
        assert!(ig
            .added_entries
            .contains(&".ta/setup-progress.json".to_string()));
        assert!(!dir.path().join(".gitignore").exists());
    }

    /// Golden test for the documented `ta setup vcs --dry-run --json` shape.
    #[test]
    fn setup_report_json_golden() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, true, None, None).unwrap();
        let mut v = serde_json::to_value(&report).unwrap();
        // The entry list tracks LOCAL_TA_PATHS; pin its length separately.
        let ig = v["ignore"].as_object_mut().unwrap();
        let added = ig.remove("added_entries").unwrap();
        assert_eq!(added.as_array().unwrap().len(), LOCAL_TA_PATHS.len());
        v.as_object_mut()
            .unwrap()
            .get_mut("detection")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("checked");
        let expected = serde_json::json!({
            "schema_version": 1,
            "dry_run": true,
            "forced": false,
            "source": "detected",
            "kind": "git",
            "plugin": null,
            "detection": {
                "schema_version": 1,
                "kind": "git",
                "plugin": null,
                "evidence": [{
                    "kind": "git",
                    "plugin": null,
                    "marker": ".git",
                    "path": ".git",
                    "reason": ".git directory present in the project root"
                }],
                "ignored_evidence": []
            },
            "ignore": {
                "target": ".gitignore",
                "action": "would_update",
                "preserved_unrecognized": [],
                "note": null
            },
            "workflow_changes": [],
            "notes": []
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn setup_refreshes_stale_block_without_force_and_reports_added() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "target/\n\n# Trusted Autonomy — local runtime state (do not commit)\n.ta/staging/\n\nkeep/\n",
        )
        .unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, false, None, None).unwrap();
        let ig = report.ignore.unwrap();
        assert_eq!(ig.action, IgnoreAction::Updated);
        assert!(ig
            .added_entries
            .contains(&".ta/setup-progress.json".to_string()));
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.starts_with("target/\n\n"));
        assert!(content.ends_with("\nkeep/\n"));
        // Second run: nothing to do.
        let again = compute_vcs_setup(&config, false, false, None, None).unwrap();
        assert_eq!(again.ignore.unwrap().action, IgnoreAction::Unchanged);
    }

    #[test]
    fn setup_svn_override_reports_actionable_skip_when_svn_cannot_apply() {
        // A plain tempdir is not an svn working copy (and svn may be absent),
        // so applying fails; the report must say so and the run must error.
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, false, Some("svn"), None).unwrap();
        assert_eq!(report.kind, VcsKind::Svn);
        assert_eq!(report.source, "override");
        let ig = report.ignore.as_ref().unwrap();
        assert_eq!(ig.action, IgnoreAction::Skipped);
        let note = ig.note.as_deref().unwrap();
        assert!(
            note.contains("svn add --depth=empty .ta") || note.contains("Install Subversion"),
            "{note}"
        );
        assert!(run_vcs_setup(&config, false, false, Some("svn"), None).is_err());
        // A dry run only reports.
        assert!(run_vcs_setup(&config, false, true, Some("svn"), None).is_ok());
    }

    #[test]
    fn setup_override_keeps_detection_evidence() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, true, Some("perforce"), None).unwrap();
        assert_eq!(report.kind, VcsKind::Perforce);
        assert_eq!(report.detection.kind, VcsKind::Git);
        assert_eq!(report.ignore.unwrap().target, ".p4ignore");
    }

    #[test]
    fn setup_plugin_detected_writes_no_ignore_file() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/vcs/hgish");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "name = \"hgish\"\ncommand = \"ta-submit-hgish\"\n[detect]\nmarkers = [\".hgish\"]\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join(".hgish")).unwrap();
        let config = test_config(&dir);
        let report = compute_vcs_setup(&config, false, false, None, None).unwrap();
        assert_eq!(report.kind, VcsKind::Plugin);
        assert_eq!(report.plugin.as_deref(), Some("hgish"));
        assert!(report.ignore.is_none());
        assert!(report.notes[0].contains("hgish"));
        assert!(!dir.path().join(".gitignore").exists());
    }

    #[test]
    fn setup_preserves_crlf_gitignore() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "target/\r\nnode_modules/\r\n",
        )
        .unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, false, false, None, None).unwrap();
        let bytes = std::fs::read(dir.path().join(".gitignore")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("target/\r\nnode_modules/\r\n"));
        assert!(
            !text.replace("\r\n", "").contains('\n'),
            "mixed line endings"
        );
    }

    #[test]
    fn setup_never_touches_content_outside_the_block() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let before = "# mine\nsecret.txt\n*.bak\n";
        std::fs::write(dir.path().join(".gitignore"), before).unwrap();
        let config = test_config(&dir);
        run_vcs_setup(&config, true, false, None, None).unwrap();
        let after = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(after.starts_with(before));
    }
}
