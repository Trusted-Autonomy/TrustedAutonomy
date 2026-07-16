// policy.rs — Policy management CLI commands (v0.9.8.1).

use clap::Subcommand;
use ta_changeset::draft_package::DraftPackage;
use ta_mcp_gateway::GatewayConfig;
use ta_policy::auto_approve::{self, DraftInfo};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum PolicyCommands {
    /// Dry-run auto-approval evaluation for a draft.
    Check {
        /// Draft ID (full UUID or prefix).
        draft_id: String,
    },
    /// Show the resolved policy document for a project.
    Show,
}

pub fn execute(cmd: &PolicyCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        PolicyCommands::Check { draft_id } => check_draft(config, draft_id),
        PolicyCommands::Show => show_policy(config),
    }
}

fn check_draft(config: &GatewayConfig, draft_id_prefix: &str) -> anyhow::Result<()> {
    // Load the policy document.
    let policy_path = config.workspace_root.join(".ta/policy.yaml");
    let doc = if policy_path.exists() {
        let content = std::fs::read_to_string(&policy_path)?;
        serde_yaml::from_str::<ta_policy::PolicyDocument>(&content)?
    } else {
        println!("No .ta/policy.yaml found — using defaults.");
        ta_policy::PolicyDocument::default()
    };

    // Find the draft package.
    let pkg = find_draft_package(config, draft_id_prefix)?;
    let pkg_id = pkg.package_id;

    println!(
        "Draft: {} — \"{}\"",
        &pkg_id.to_string()[..8],
        pkg.summary.what_changed
    );
    println!();
    println!("Auto-approval evaluation:");

    let changed_paths: Vec<String> = pkg
        .changes
        .artifacts
        .iter()
        .map(|a| a.resource_uri.clone())
        .collect();

    let draft_info = DraftInfo {
        changed_paths: changed_paths.clone(),
        lines_changed: 0,
        plan_phase: None, // We don't have phase info from the draft
        agent_id: pkg.agent_identity.agent_id.clone(),
    };

    // Show each condition evaluation.
    let drafts_cfg = &doc.defaults.auto_approve.drafts;

    // Enabled check.
    if drafts_cfg.enabled {
        println!("  ✅ enabled: true");
    } else {
        println!("  ❌ enabled: false");
        println!();
        println!("Result: WOULD NOT AUTO-APPROVE (disabled)");
        return Ok(());
    }

    // File count.
    let file_count = changed_paths.len();
    if let Some(max) = drafts_cfg.conditions.max_files {
        if file_count <= max {
            println!("  ✅ max_files: {} ≤ {}", file_count, max);
        } else {
            println!("  ❌ max_files: {} > {}", file_count, max);
        }
    } else {
        println!("  ⏭️  max_files: not configured");
    }

    // Lines changed (approximate).
    if let Some(max) = drafts_cfg.conditions.max_lines_changed {
        println!(
            "  ⏭️  max_lines_changed: limit {} (line count not available from draft)",
            max
        );
    }

    // Path checks.
    if !drafts_cfg.conditions.blocked_paths.is_empty() {
        let mut any_blocked = false;
        for path in &changed_paths {
            let bare = path
                .strip_prefix("fs://workspace/")
                .unwrap_or(path.as_str());
            for pattern in &drafts_cfg.conditions.blocked_paths {
                if glob::Pattern::new(pattern)
                    .map(|p| p.matches(bare))
                    .unwrap_or(false)
                {
                    println!("  ❌ blocked: {} → {}", bare, pattern);
                    any_blocked = true;
                }
            }
        }
        if !any_blocked {
            println!("  ✅ no blocked paths matched");
        }
    }

    if !drafts_cfg.conditions.allowed_paths.is_empty() {
        let mut all_match = true;
        for path in &changed_paths {
            let bare = path
                .strip_prefix("fs://workspace/")
                .unwrap_or(path.as_str());
            let matched = drafts_cfg.conditions.allowed_paths.iter().find(|pattern| {
                glob::Pattern::new(pattern)
                    .map(|p| p.matches(bare))
                    .unwrap_or(false)
            });
            if let Some(pat) = matched {
                println!("  ✅ {} → {}", bare, pat);
            } else {
                println!("  ❌ {} does not match any allowed_paths", bare);
                all_match = false;
            }
        }
        if all_match {
            println!("  ✅ all paths match allowed_paths");
        }
    }

    // Verification.
    if drafts_cfg.conditions.require_tests_pass {
        println!(
            "  ⏭️  require_tests_pass: would run '{}'",
            drafts_cfg.conditions.test_command
        );
    } else {
        println!("  ⏭️  require_tests_pass: skipped (not enabled)");
    }
    if drafts_cfg.conditions.require_clean_clippy {
        println!(
            "  ⏭️  require_clean_clippy: would run '{}'",
            drafts_cfg.conditions.lint_command
        );
    } else {
        println!("  ⏭️  require_clean_clippy: skipped (not enabled)");
    }

    // Phase check.
    if !drafts_cfg.conditions.allowed_phases.is_empty() {
        println!(
            "  ⏭️  allowed_phases: {:?} (draft phase not available)",
            drafts_cfg.conditions.allowed_phases
        );
    }

    // Final decision.
    let decision = auto_approve::should_auto_approve_draft(&draft_info, &doc);
    println!();
    match decision {
        auto_approve::AutoApproveDecision::Approved { .. } => {
            println!("Result: WOULD AUTO-APPROVE");
        }
        auto_approve::AutoApproveDecision::Denied { blockers } => {
            println!("Result: WOULD NOT AUTO-APPROVE");
            for b in &blockers {
                println!("  Reason: {}", b);
            }
        }
    }

    Ok(())
}

fn show_policy(config: &GatewayConfig) -> anyhow::Result<()> {
    let policy_path = config.workspace_root.join(".ta/policy.yaml");
    if policy_path.exists() {
        let content = std::fs::read_to_string(&policy_path)?;
        let doc: ta_policy::PolicyDocument = serde_yaml::from_str(&content)?;
        println!("{}", serde_yaml::to_string(&doc)?);
    } else {
        println!("No .ta/policy.yaml found — showing defaults:");
        let doc = ta_policy::PolicyDocument::default();
        println!("{}", serde_yaml::to_string(&doc)?);
    }
    Ok(())
}

fn find_draft_package(config: &GatewayConfig, prefix: &str) -> anyhow::Result<DraftPackage> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        anyhow::bail!("No drafts directory found at {}", dir.display());
    }

    let entries = std::fs::read_dir(dir)?;
    let mut matches = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let filename = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if filename.starts_with(prefix) || prefix.len() >= 8 && filename.contains(prefix) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(pkg) = serde_json::from_str::<DraftPackage>(&content) {
                        matches.push(pkg);
                    }
                }
            }
        }
    }

    // Also try parsing as UUID and looking for exact match.
    if matches.is_empty() {
        if let Ok(uuid) = Uuid::parse_str(prefix) {
            let path = dir.join(format!("{}.json", uuid));
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                matches.push(serde_json::from_str::<DraftPackage>(&content)?);
            }
        }
    }

    match matches.len() {
        0 => anyhow::bail!("No draft found matching '{}'", prefix),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} drafts. Use a longer prefix.",
            prefix,
            n
        ),
    }
}
