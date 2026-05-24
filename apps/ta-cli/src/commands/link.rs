// link.rs — `ta link` CLI commands (v0.16.1.5).
//
// Provides:
//   - `ta link add <path-or-url>` — add to .ta/links.toml
//   - `ta link list`              — table: name, relationship, path/repo, manifest present
//   - `ta link status`            — check reachability of all linked projects
//   - `ta link refresh [<name>]`  — re-read/re-fetch manifests
//   - `ta link remove <name>`     — remove from .ta/links.toml

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::links::{Link, LinkStatus, Relationship};

#[derive(Debug, Subcommand)]
pub enum LinkCommands {
    /// Add a linked project to `.ta/links.toml`.
    ///
    /// Accepts a local path (relative or absolute) or a GitHub URL
    /// (github:org/repo, or https://github.com/org/repo).
    ///
    /// Examples:
    ///   ta link add ../cinepipe-train
    ///   ta link add github:myorg/pragma
    Add {
        /// Local path or GitHub URL of the project to link.
        target: String,
        /// Relationship type: dependency, consumer, workspace-member, sibling, reference.
        #[arg(long, short = 'r')]
        relationship: Option<String>,
        /// Human-readable description of why these projects are linked.
        #[arg(long, short = 'd')]
        description: Option<String>,
        /// Set a custom name for the link (defaults to directory name or repo name).
        #[arg(long, short = 'n')]
        name: Option<String>,
        /// Do not inject manifest into agent context at goal start.
        #[arg(long)]
        no_inject: bool,
    },
    /// Show all linked projects.
    List,
    /// Check reachability and manifest presence for all linked projects.
    Status,
    /// Re-read local manifests and re-fetch remote manifests from GitHub.
    ///
    /// Without a name, refreshes all links with `inject = true`.
    Refresh {
        /// Name of a specific link to refresh.
        name: Option<String>,
    },
    /// Remove a linked project by name.
    Remove {
        /// Name of the link to remove (as shown in `ta link list`).
        name: String,
    },
}

pub fn execute(command: &LinkCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    match command {
        LinkCommands::Add {
            target,
            relationship,
            description,
            name,
            no_inject,
        } => execute_add(
            project_root,
            target,
            relationship.as_deref(),
            description.as_deref(),
            name.as_deref(),
            *no_inject,
        ),
        LinkCommands::List => execute_list(project_root),
        LinkCommands::Status => execute_status(project_root),
        LinkCommands::Refresh { name } => execute_refresh(project_root, name.as_deref()),
        LinkCommands::Remove { name } => execute_remove(project_root, name),
    }
}

// ── add ───────────────────────────────────────────────────────────────────────

fn execute_add(
    project_root: &Path,
    target: &str,
    relationship: Option<&str>,
    description: Option<&str>,
    name_override: Option<&str>,
    no_inject: bool,
) -> anyhow::Result<()> {
    let ta_dir = project_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;

    let (is_remote, resolved_name) = if is_github_url(target) {
        let repo_name = parse_github_repo_name(target).unwrap_or_else(|| target.to_string());
        (true, name_override.unwrap_or(&repo_name).to_string())
    } else {
        let path = std::path::Path::new(target);
        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(target);
        (false, name_override.unwrap_or(dir_name).to_string())
    };

    let rel = parse_relationship(relationship)?;
    let desc = description.unwrap_or("").to_string();

    let link = if is_remote {
        Link {
            name: resolved_name.clone(),
            path: None,
            repo: Some(normalize_github_url(target)),
            relationship: rel,
            description: desc,
            inject: !no_inject,
        }
    } else {
        let stored_path = target.to_string();
        Link {
            name: resolved_name.clone(),
            path: Some(stored_path),
            repo: None,
            relationship: rel,
            description: desc,
            inject: !no_inject,
        }
    };

    ta_workspace::links::add_link(project_root, &link)?;
    println!("Added link '{}' to .ta/links.toml", resolved_name);

    // Offer to fetch manifest for remote links.
    if is_remote {
        if let Some(repo) = &link.repo {
            let cache_dir = project_root.join(".ta").join("link-cache");
            if let Some(_manifest) =
                ta_workspace::link_cache::force_refresh(&resolved_name, repo, &cache_dir)
            {
                println!(
                    "Fetched manifest for '{}' into .ta/link-cache/",
                    resolved_name
                );
            } else {
                println!(
                    "Note: could not fetch manifest for '{}'. Run `ta link refresh {}` when the network is available.",
                    resolved_name, resolved_name
                );
            }
        }
    } else {
        // Check if manifest exists locally.
        let resolved = if std::path::Path::new(target).is_absolute() {
            std::path::PathBuf::from(target)
        } else {
            project_root.join(target)
        };
        let manifest = resolved.join(".ta").join("project-manifest.md");
        if !manifest.exists() {
            println!(
                "Note: '{}' has no .ta/project-manifest.md yet. Run `ta manifest init` there to create one.",
                resolved_name
            );
        }
    }

    Ok(())
}

// ── list ──────────────────────────────────────────────────────────────────────

fn execute_list(project_root: &Path) -> anyhow::Result<()> {
    let links = ta_workspace::links::load(project_root);
    if links.is_empty() {
        println!("No linked projects. Use `ta link add <path-or-github-url>` to add one.");
        return Ok(());
    }

    let cache_dir = project_root.join(".ta").join("link-cache");

    println!(
        "{:<20} {:<18} {:<30} {:<10} INJECT",
        "NAME", "RELATIONSHIP", "PATH/REPO", "MANIFEST"
    );
    println!("{}", "─".repeat(90));
    for link in &links {
        let location = link.path.as_deref().or(link.repo.as_deref()).unwrap_or("—");
        let status = link.status(project_root, &cache_dir);
        let manifest_indicator = status.indicator();
        let inject = if link.inject { "yes" } else { "no" };
        println!(
            "{:<20} {:<18} {:<30} {:<10} {}",
            truncate(link.name.as_str(), 19),
            link.relationship.badge(),
            truncate(location, 29),
            manifest_indicator,
            inject,
        );
    }
    println!();
    println!("✓ = manifest found  ✗ = manifest missing  ~ = cached  — = unreachable");
    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────────

fn execute_status(project_root: &Path) -> anyhow::Result<()> {
    let links = ta_workspace::links::load(project_root);
    if links.is_empty() {
        println!("No linked projects configured.");
        return Ok(());
    }

    let cache_dir = project_root.join(".ta").join("link-cache");
    let mut any_issues = false;

    for link in &links {
        let status = link.status(project_root, &cache_dir);
        let refreshed = link
            .last_refreshed(&cache_dir)
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "never".to_string());

        match &status {
            LinkStatus::Ok { cached } => {
                let source = if *cached { "cached" } else { "local" };
                println!(
                    "[ok]  {} — manifest found ({}), last refreshed: {}",
                    link.name, source, refreshed
                );
            }
            LinkStatus::MissingManifest => {
                any_issues = true;
                println!(
                    "[warn] {} — manifest missing. Run `ta manifest init` in that project.",
                    link.name
                );
            }
            LinkStatus::Unreachable { reason } => {
                println!("[info] {} — unreachable ({}). Skipping.", link.name, reason);
            }
        }
    }

    if any_issues {
        println!();
        println!(
            "Some manifests are missing. Create them with `ta manifest init` in each project."
        );
    }

    Ok(())
}

// ── refresh ───────────────────────────────────────────────────────────────────

fn execute_refresh(project_root: &Path, name: Option<&str>) -> anyhow::Result<()> {
    let links = ta_workspace::links::load(project_root);
    let cache_dir = project_root.join(".ta").join("link-cache");
    std::fs::create_dir_all(&cache_dir)?;

    let to_refresh: Vec<&Link> = if let Some(n) = name {
        let link = links.iter().find(|l| l.name == n).ok_or_else(|| {
            anyhow::anyhow!(
                "No link named '{}'. Run `ta link list` to see all links.",
                n
            )
        })?;
        vec![link]
    } else {
        links.iter().collect()
    };

    let mut refreshed = 0usize;
    let mut skipped = 0usize;

    for link in to_refresh {
        if let Some(repo) = &link.repo {
            match ta_workspace::link_cache::force_refresh(&link.name, repo, &cache_dir) {
                Some(_) => {
                    println!("  refreshed '{}' from {}", link.name, repo);
                    refreshed += 1;
                }
                None => {
                    println!(
                        "  [warn] could not fetch manifest for '{}' ({})",
                        link.name, repo
                    );
                    skipped += 1;
                }
            }
        } else {
            // Local link — just check if manifest is readable.
            let manifest = link.read_manifest(project_root, &cache_dir);
            if manifest.is_some() {
                println!("  '{}' — local manifest found", link.name);
                refreshed += 1;
            } else {
                println!(
                    "  [warn] '{}' — local manifest not found (expected .ta/project-manifest.md)",
                    link.name
                );
                skipped += 1;
            }
        }
    }

    println!();
    println!("{} refreshed, {} skipped.", refreshed, skipped);
    Ok(())
}

// ── remove ────────────────────────────────────────────────────────────────────

fn execute_remove(project_root: &Path, name: &str) -> anyhow::Result<()> {
    match ta_workspace::links::remove_link(project_root, name)? {
        true => {
            println!("Removed link '{}' from .ta/links.toml.", name);
            Ok(())
        }
        false => Err(anyhow::anyhow!(
            "No link named '{}'. Run `ta link list` to see all links.",
            name
        )),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_github_url(s: &str) -> bool {
    s.starts_with("github:") || s.starts_with("https://github.com/") || s.starts_with("github.com/")
}

fn normalize_github_url(s: &str) -> String {
    if s.starts_with("github:") {
        s.to_string()
    } else if let Some(slug) = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("github.com/"))
    {
        format!("github:{}", slug.trim_end_matches(".git"))
    } else {
        s.to_string()
    }
}

fn parse_github_repo_name(s: &str) -> Option<String> {
    let slug = s
        .strip_prefix("github:")
        .or_else(|| s.strip_prefix("https://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))?;
    slug.split_once('/')
        .map(|x| x.1.trim_end_matches(".git").to_string())
}

fn parse_relationship(s: Option<&str>) -> anyhow::Result<Relationship> {
    match s.unwrap_or("reference") {
        "dependency" => Ok(Relationship::Dependency),
        "consumer" => Ok(Relationship::Consumer),
        "workspace-member" => Ok(Relationship::WorkspaceMember),
        "sibling" => Ok(Relationship::Sibling),
        "reference" => Ok(Relationship::Reference),
        other => Err(anyhow::anyhow!(
            "Unknown relationship type '{}'. Valid types: dependency, consumer, workspace-member, sibling, reference.",
            other
        )),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_github_url_variants() {
        assert_eq!(normalize_github_url("github:org/repo"), "github:org/repo");
        assert_eq!(
            normalize_github_url("https://github.com/org/repo"),
            "github:org/repo"
        );
        assert_eq!(
            normalize_github_url("https://github.com/org/repo.git"),
            "github:org/repo"
        );
    }

    #[test]
    fn parse_relationship_valid() {
        assert_eq!(
            parse_relationship(Some("dependency")).unwrap(),
            Relationship::Dependency
        );
        assert_eq!(
            parse_relationship(Some("workspace-member")).unwrap(),
            Relationship::WorkspaceMember
        );
        assert_eq!(parse_relationship(None).unwrap(), Relationship::Reference);
    }

    #[test]
    fn parse_relationship_invalid() {
        assert!(parse_relationship(Some("bogus")).is_err());
    }

    #[test]
    fn link_list_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = execute(&LinkCommands::List, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn link_status_reports_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // Create links.toml pointing to a local path without a manifest.
        let linked = dir.path().join("other");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            ta_dir.join("links.toml"),
            format!(
                "[[link]]\nname = \"other\"\npath = {:?}\nrelationship = \"sibling\"\n",
                linked.to_str().unwrap()
            ),
        )
        .unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = execute(&LinkCommands::Status, &config);
        assert!(result.is_ok());
    }
}
