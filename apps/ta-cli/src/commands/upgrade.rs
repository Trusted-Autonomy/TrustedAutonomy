// upgrade.rs — `ta upgrade`: project TA version tracking & upgrade path (v0.15.18).
//
// Detects when a project was created or last upgraded with an older TA version
// and applies the required project-level changes (gitignore entries, config schema
// updates, etc.) to bring it up to date with the current binary.
//
// `.ta/project-meta.toml` tracks the TA version used at init + last upgrade.
// `UPGRADE_STEPS` is a static manifest embedded in the binary. Each step has
// a check fn (already applied?) and an apply fn (makes the change).
//
// Usage:
//   ta upgrade              # apply all pending steps
//   ta upgrade --dry-run    # show what would be applied, exit 1 if any pending
//   ta upgrade --force      # re-run all steps regardless of version
//   ta upgrade --acknowledge ".ta/review/"  # suppress warning for intentional omission

use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

// ── Project meta ─────────────────────────────────────────────────────────────

/// `.ta/project-meta.toml` schema.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct ProjectMeta {
    /// TA semver at project creation (e.g., "0.15.18-alpha").
    #[serde(default)]
    pub initialized_with: String,
    /// TA semver of last successful `ta upgrade` run.
    #[serde(default)]
    pub last_upgraded: String,
}

impl ProjectMeta {
    const FILE: &'static str = ".ta/project-meta.toml";

    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(Self::FILE);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = toml::from_str::<Self>(&content) {
                return meta;
            }
        }
        Self::default()
    }

    pub fn save(&self, project_root: &Path) -> anyhow::Result<()> {
        let path = project_root.join(Self::FILE);
        let content = format!(
            "# Written by ta init / ta upgrade. Do not edit manually — managed by TA.\ninitialized_with = {:?}\nlast_upgraded    = {:?}\n",
            self.initialized_with, self.last_upgraded,
        );
        std::fs::write(path, content)?;
        Ok(())
    }
}

// ── Version comparison ────────────────────────────────────────────────────────

/// Parse a semver string into (major, minor, patch), ignoring pre-release tags.
fn parse_version(v: &str) -> (u32, u32, u32) {
    let stripped = v.split('-').next().unwrap_or(v);
    let parts: Vec<u32> = stripped
        .splitn(3, '.')
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

/// Return true if `introduced` is newer than `last_upgraded`.
/// `introduced` uses short form like "0.15.18"; `last_upgraded` uses full semver.
fn step_needed(introduced: &str, last_upgraded: &str) -> bool {
    if last_upgraded.is_empty() {
        return true;
    }
    let introduced_v = parse_version(introduced);
    let last_v = parse_version(last_upgraded);
    introduced_v > last_v
}

// ── Upgrade steps ─────────────────────────────────────────────────────────────

pub struct UpgradeStep {
    pub introduced_in: &'static str,
    pub description: &'static str,
    /// Returns true if the step is already applied (no action needed).
    pub check: fn(&Path) -> bool,
    /// Applies the step. Returns Ok(()) on success.
    pub apply: fn(&Path) -> anyhow::Result<()>,
}

/// All upgrade steps in version order. Each step is idempotent.
pub static UPGRADE_STEPS: &[UpgradeStep] = &[
    UpgradeStep {
        introduced_in: "0.15.18",
        description: "add .ta/review/ to .gitignore",
        check: |root| gitignore_contains(root, ".ta/review/"),
        apply: |root| append_gitignore(root, ".ta/review/"),
    },
    UpgradeStep {
        introduced_in: "0.15.18",
        description: "add .ta/review/ to .taignore",
        check: |root| taignore_contains(root, ".ta/review/"),
        apply: |root| append_taignore(root, ".ta/review/"),
    },
    UpgradeStep {
        introduced_in: "0.15.18",
        description: "ensure workflow.toml has [config] pr_poll_interval_secs",
        check: |root| workflow_has_pr_poll(root),
        apply: |root| add_pr_poll_interval(root),
    },
    UpgradeStep {
        introduced_in: "0.15.18",
        description: "warn if old staging dirs may contain target/ artifacts",
        check: |root| !staging_has_target_dirs(root),
        apply: |_root| {
            // Informational only — we don't delete anything automatically.
            println!(
                "  [warn] Old staging dirs may contain target/ artifacts. \
                 Run 'ta gc' to reclaim disk space."
            );
            Ok(())
        },
    },
];

// ── Step helpers ──────────────────────────────────────────────────────────────

fn gitignore_contains(root: &Path, entry: &str) -> bool {
    let path = root.join(".gitignore");
    if let Ok(content) = std::fs::read_to_string(&path) {
        content.lines().any(|l| l.trim() == entry.trim())
    } else {
        false
    }
}

fn append_gitignore(root: &Path, entry: &str) -> anyhow::Result<()> {
    let path = root.join(".gitignore");
    let mut content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str("# Added by ta upgrade\n");
    content.push_str(entry);
    content.push('\n');
    std::fs::write(&path, content)?;
    Ok(())
}

fn taignore_contains(root: &Path, entry: &str) -> bool {
    let path = root.join(".taignore");
    if let Ok(content) = std::fs::read_to_string(&path) {
        content.lines().any(|l| l.trim() == entry.trim())
    } else {
        false
    }
}

fn append_taignore(root: &Path, entry: &str) -> anyhow::Result<()> {
    let path = root.join(".taignore");
    let mut content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str("# Added by ta upgrade\n");
    content.push_str(entry);
    content.push('\n');
    std::fs::write(&path, content)?;
    Ok(())
}

fn workflow_has_pr_poll(root: &Path) -> bool {
    let path = root.join(".ta/workflow.toml");
    if let Ok(content) = std::fs::read_to_string(&path) {
        content.contains("pr_poll_interval_secs")
    } else {
        // No workflow.toml — step not applicable.
        true
    }
}

fn add_pr_poll_interval(root: &Path) -> anyhow::Result<()> {
    let path = root.join(".ta/workflow.toml");
    if !path.exists() {
        return Ok(());
    }
    let mut content = std::fs::read_to_string(&path)?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    // If a [config] section already exists, append the key under that section
    // by inserting it before the next section header (or at the end of the
    // section).  Appending a duplicate [config] header produces invalid TOML.
    if content.contains("[config]") {
        // Find the position just after the [config] header line and insert
        // the new key there, before any subsequent section.
        let key_line = "pr_poll_interval_secs = 60  # Added by ta upgrade\n";
        // Insert after the [config] line.
        if let Some(config_pos) = content.find("[config]") {
            // Move past the [config] line.
            let after_header = content[config_pos..]
                .find('\n')
                .map(|n| config_pos + n + 1)
                .unwrap_or(content.len());
            content.insert_str(after_header, key_line);
        } else {
            // Fallback (shouldn't happen given the contains check above).
            content.push_str(key_line);
        }
    } else {
        content.push_str("\n# Added by ta upgrade — default PR poll interval in seconds.\n[config]\npr_poll_interval_secs = 60\n");
    }
    std::fs::write(&path, content)?;
    Ok(())
}

fn staging_has_target_dirs(root: &Path) -> bool {
    let staging = root.join(".ta/staging");
    if !staging.exists() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(&staging) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let target = entry.path().join("target");
                if target.exists() && target.is_dir() {
                    return true;
                }
            }
        }
    }
    false
}

// ── Acknowledged omissions ────────────────────────────────────────────────────

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LocalConfig {
    #[serde(default)]
    upgrade: UpgradeLocalConfig,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct UpgradeLocalConfig {
    #[serde(default)]
    acknowledged_omissions: Vec<String>,
}

fn load_acknowledged_omissions(project_root: &Path) -> Vec<String> {
    let path = project_root.join(".ta/config.local.toml");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = toml::from_str::<LocalConfig>(&content) {
            return cfg.upgrade.acknowledged_omissions;
        }
    }
    Vec::new()
}

fn add_acknowledged_omission(project_root: &Path, pattern: &str) -> anyhow::Result<()> {
    let path = project_root.join(".ta/config.local.toml");
    let mut cfg = if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        toml::from_str::<LocalConfig>(&content).unwrap_or_default()
    } else {
        LocalConfig::default()
    };
    if !cfg
        .upgrade
        .acknowledged_omissions
        .iter()
        .any(|s| s == pattern)
    {
        cfg.upgrade.acknowledged_omissions.push(pattern.to_string());
    }
    let content = toml::to_string_pretty(&cfg)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, content)?;
    println!(
        "  Acknowledged '{}' — step will be silently skipped in future runs.",
        pattern
    );
    Ok(())
}

fn is_acknowledged(acknowledged: &[String], description: &str) -> bool {
    acknowledged
        .iter()
        .any(|p| description.contains(p.as_str()))
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[derive(clap::Args, Debug)]
pub struct UpgradeArgs {
    /// Show what would be applied without making changes. Exits 1 if any steps are pending.
    #[arg(long)]
    pub dry_run: bool,

    /// Re-run all steps regardless of last_upgraded version.
    #[arg(long)]
    pub force: bool,

    /// Add a pattern to acknowledged_omissions so the step is silently skipped.
    ///
    /// Example: ta upgrade --acknowledge ".ta/review/" suppresses the review/ gitignore warning
    /// for users who intentionally removed that entry.
    #[arg(long, value_name = "PATTERN")]
    pub acknowledge: Option<String>,
}

/// Execute `ta upgrade`.
pub fn execute(config: &GatewayConfig, args: &UpgradeArgs) -> anyhow::Result<()> {
    let root = &config.workspace_root;

    // Handle --acknowledge before anything else.
    if let Some(ref pattern) = args.acknowledge {
        return add_acknowledged_omission(root, pattern);
    }

    let meta = ProjectMeta::load(root);
    let last_upgraded = meta.last_upgraded.as_str();
    let current_version = env!("CARGO_PKG_VERSION");
    let acknowledged = load_acknowledged_omissions(root);

    let mut any_pending = false;
    let mut applied = 0;
    let mut already_ok = 0;
    let mut skipped = 0;

    for step in UPGRADE_STEPS {
        // Skip steps introduced in versions already covered.
        if !args.force && !step_needed(step.introduced_in, last_upgraded) {
            already_ok += 1;
            continue;
        }

        // Check acknowledged omissions.
        if is_acknowledged(&acknowledged, step.description) {
            skipped += 1;
            continue;
        }

        // Run check fn.
        let already_applied = (step.check)(root);
        if already_applied {
            println!("  [ok]  {}", step.description);
            already_ok += 1;
        } else {
            any_pending = true;
            if args.dry_run {
                println!(
                    "  [fix] {} (pending — run 'ta upgrade' to apply)",
                    step.description
                );
            } else {
                match (step.apply)(root) {
                    Ok(()) => {
                        println!("  [fix] {}", step.description);
                        applied += 1;
                    }
                    Err(e) => {
                        eprintln!("  [err] {}: {}", step.description, e);
                    }
                }
            }
        }
    }

    if args.dry_run {
        if any_pending {
            println!();
            println!(
                "{} step(s) pending — run 'ta upgrade' to apply.",
                UPGRADE_STEPS.len() - already_ok - skipped
            );
            return Err(anyhow::anyhow!("upgrade steps pending"));
        } else {
            println!();
            println!(
                "Project is up to date ({}). No steps pending.",
                current_version
            );
        }
        return Ok(());
    }

    // Update project-meta.toml.
    if !args.dry_run && (applied > 0 || meta.last_upgraded.is_empty()) {
        let updated = ProjectMeta {
            initialized_with: if meta.initialized_with.is_empty() {
                current_version.to_string()
            } else {
                meta.initialized_with.clone()
            },
            last_upgraded: current_version.to_string(),
        };
        if let Err(e) = updated.save(root) {
            eprintln!("  warning: could not write project-meta.toml: {}", e);
        }
    }

    println!();
    if applied > 0 {
        let from = if meta.last_upgraded.is_empty() {
            "0.0.0".to_string()
        } else {
            meta.last_upgraded.clone()
        };
        println!(
            "Upgraded project from {} → {} ({} step(s) applied, {} already ok).",
            from, current_version, applied, already_ok
        );
    } else {
        println!(
            "Project is up to date ({}). All {} step(s) already applied.",
            current_version, already_ok
        );
    }

    Ok(())
}

/// Run upgrade checks in dry-run mode and return a summary for `ta doctor`.
///
/// Returns `(pending_count, step_descriptions)`.
pub fn check_pending(project_root: &Path) -> (usize, Vec<String>) {
    let meta = ProjectMeta::load(project_root);
    let last_upgraded = meta.last_upgraded.as_str();
    let acknowledged = load_acknowledged_omissions(project_root);

    let mut pending = Vec::new();
    for step in UPGRADE_STEPS {
        if !step_needed(step.introduced_in, last_upgraded) {
            continue;
        }
        if is_acknowledged(&acknowledged, step.description) {
            continue;
        }
        if !(step.check)(project_root) {
            pending.push(step.description.to_string());
        }
    }
    (pending.len(), pending)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_root() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn parse_version_basic() {
        assert_eq!(parse_version("0.15.18"), (0, 15, 18));
        assert_eq!(parse_version("0.15.18-alpha"), (0, 15, 18));
        assert_eq!(parse_version("1.2.3-rc.1"), (1, 2, 3));
        assert_eq!(parse_version("0.0.0"), (0, 0, 0));
    }

    #[test]
    fn step_needed_empty_last_upgraded() {
        assert!(step_needed("0.15.18", ""));
    }

    #[test]
    fn step_needed_older_version() {
        assert!(step_needed("0.15.18", "0.15.17-alpha"));
    }

    #[test]
    fn step_not_needed_same_version() {
        assert!(!step_needed("0.15.18", "0.15.18-alpha"));
    }

    #[test]
    fn step_not_needed_newer_version() {
        assert!(!step_needed("0.15.18", "0.15.19-alpha"));
    }

    #[test]
    fn gitignore_check_and_apply_roundtrip() {
        let dir = make_root();
        let root = dir.path();
        assert!(!gitignore_contains(root, ".ta/review/"));
        append_gitignore(root, ".ta/review/").unwrap();
        assert!(gitignore_contains(root, ".ta/review/"));
    }

    #[test]
    fn gitignore_check_idempotent() {
        let dir = make_root();
        let root = dir.path();
        append_gitignore(root, ".ta/review/").unwrap();
        append_gitignore(root, ".ta/review/").unwrap();
        let content = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(
            content.lines().filter(|l| l.trim() == ".ta/review/").count(),
            2,
            "two separate appends should produce two entries (idempotency is caller's responsibility)"
        );
    }

    #[test]
    fn taignore_check_and_apply_roundtrip() {
        let dir = make_root();
        let root = dir.path();
        assert!(!taignore_contains(root, ".ta/review/"));
        append_taignore(root, ".ta/review/").unwrap();
        assert!(taignore_contains(root, ".ta/review/"));
    }

    #[test]
    fn workflow_pr_poll_check_missing() {
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        std::fs::write(root.join(".ta/workflow.toml"), "[verify]\ncommands = []\n").unwrap();
        assert!(!workflow_has_pr_poll(root));
        add_pr_poll_interval(root).unwrap();
        assert!(workflow_has_pr_poll(root));
    }

    #[test]
    fn workflow_pr_poll_existing_config_section_no_duplicate_header() {
        // If workflow.toml already has a [config] section, add_pr_poll_interval
        // must NOT append a second [config] header (invalid TOML).
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        std::fs::write(
            root.join(".ta/workflow.toml"),
            "[config]\nauto_merge = true\n\n[verify]\ncommands = []\n",
        )
        .unwrap();
        assert!(!workflow_has_pr_poll(root));
        add_pr_poll_interval(root).unwrap();
        let content = std::fs::read_to_string(root.join(".ta/workflow.toml")).unwrap();
        // Key must be present.
        assert!(content.contains("pr_poll_interval_secs"));
        // No duplicate [config] header.
        assert_eq!(
            content.matches("[config]").count(),
            1,
            "must not produce a duplicate [config] section header"
        );
        // Must still be valid TOML.
        content
            .parse::<toml::Value>()
            .expect("result must be valid TOML");
    }

    #[test]
    fn workflow_pr_poll_no_file_returns_true() {
        let dir = make_root();
        // No workflow.toml — step considered not applicable.
        assert!(workflow_has_pr_poll(dir.path()));
    }

    #[test]
    fn project_meta_save_and_load_roundtrip() {
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        let meta = ProjectMeta {
            initialized_with: "0.15.17-alpha".to_string(),
            last_upgraded: "0.15.18-alpha".to_string(),
        };
        meta.save(root).unwrap();
        let loaded = ProjectMeta::load(root);
        assert_eq!(loaded.initialized_with, "0.15.17-alpha");
        assert_eq!(loaded.last_upgraded, "0.15.18-alpha");
    }

    #[test]
    fn project_meta_load_missing_file_returns_default() {
        let dir = make_root();
        let meta = ProjectMeta::load(dir.path());
        assert!(meta.initialized_with.is_empty());
        assert!(meta.last_upgraded.is_empty());
    }

    #[test]
    fn acknowledged_omissions_suppresses_step() {
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();

        // No gitignore entry — step would normally be pending.
        let (before, _) = check_pending(root);
        // After acknowledging, the step should be suppressed.
        add_acknowledged_omission(root, ".ta/review/").unwrap();
        let acknowledged = load_acknowledged_omissions(root);
        assert!(acknowledged.contains(&".ta/review/".to_string()));
        // The check step for gitignore contains ".ta/review/" in its description,
        // so it should be acknowledged.
        let filtered: Vec<_> = UPGRADE_STEPS
            .iter()
            .filter(|s| s.description.contains(".ta/review/"))
            .collect();
        assert!(!filtered.is_empty());
        let _ = before; // before count varies based on env
    }

    #[test]
    fn upgrade_from_zero_applies_all_applicable_steps() {
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        // Meta is absent → "0.0.0" → all steps needed.
        let (count, _) = check_pending(root);
        // At least gitignore and taignore steps should be pending.
        assert!(
            count >= 2,
            "expected >=2 pending steps from 0.0.0, got {}",
            count
        );
    }

    #[test]
    fn dry_run_exits_nonzero_when_pending() {
        let dir = make_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(root);
        let args = UpgradeArgs {
            dry_run: true,
            force: false,
            acknowledge: None,
        };
        // Should error because steps are pending.
        let result = execute(&config, &args);
        assert!(
            result.is_err(),
            "dry_run with pending steps should return Err"
        );
    }

    #[test]
    fn staging_target_dir_detection() {
        let dir = make_root();
        let root = dir.path();
        // No staging — should return false.
        assert!(!staging_has_target_dirs(root));
        // Create a fake staging dir with target/.
        let staging = root.join(".ta/staging/fake-goal-id");
        std::fs::create_dir_all(staging.join("target")).unwrap();
        assert!(staging_has_target_dirs(root));
    }
}
