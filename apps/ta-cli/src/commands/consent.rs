// consent.rs — Per-agent terms consent tracking (v0.10.18.4).
//
// Tracks which agent terms the user has explicitly accepted, stored in
// `.ta/consent.json`. The daemon checks this before spawning agents,
// replacing the old silent `--accept-terms` injection.
//
// Consent is per-agent and per-version: when an agent's terms version
// changes, the user must re-accept.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-agent consent record.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AgentConsent {
    /// Agent terms version that was accepted (e.g., "2025-03-01").
    pub version: String,
    /// When the user accepted.
    pub accepted_at: String,
}

/// Consent store: maps agent IDs to their consent records.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
pub struct ConsentStore {
    #[serde(flatten)]
    pub agents: HashMap<String, AgentConsent>,
}

/// Resolve the consent.json path for a project.
pub fn consent_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("consent.json")
}

/// Load the consent store from disk. Returns an empty store if the file doesn't exist.
pub fn load(project_root: &Path) -> ConsentStore {
    let path = consent_path(project_root);
    if !path.exists() {
        return ConsentStore::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ConsentStore::default(),
    }
}

/// Save the consent store to disk.
pub fn save(project_root: &Path, store: &ConsentStore) -> anyhow::Result<()> {
    let path = consent_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Check if an agent has current consent.
/// Returns Ok(()) if consent is current, Err with message if not.
pub fn check_agent_consent(
    project_root: &Path,
    agent_id: &str,
    current_version: &str,
) -> Result<(), String> {
    let store = load(project_root);
    match store.agents.get(agent_id) {
        Some(consent) if consent.version == current_version => Ok(()),
        Some(consent) => Err(format!(
            "Agent '{}' terms have been updated ({} -> {}). \
             Please run `ta terms accept {}` or restart `ta shell` to review and accept.",
            agent_id, consent.version, current_version, agent_id
        )),
        None => Err(format!(
            "Agent '{}' terms have not been accepted. \
             Please run `ta terms accept {}` or restart `ta shell` to review and accept.",
            agent_id, agent_id
        )),
    }
}

/// Record consent for an agent at the given version.
pub fn accept_agent(project_root: &Path, agent_id: &str, version: &str) -> anyhow::Result<()> {
    let mut store = load(project_root);
    store.agents.insert(
        agent_id.to_string(),
        AgentConsent {
            version: version.to_string(),
            accepted_at: chrono::Utc::now().to_rfc3339(),
        },
    );
    save(project_root, &store)?;
    Ok(())
}

/// Get the agent's terms version. Tries running `<command> --version` and
/// falling back to the TA CLI version if the agent binary is not available.
pub fn detect_agent_version(agent_id: &str) -> String {
    let command = match agent_id {
        "claude-code" => "claude",
        "codex" => "codex",
        other => other,
    };

    // Try to get the agent's version.
    if let Ok(output) = std::process::Command::new(command)
        .arg("--version")
        .output()
    {
        if output.status.success() {
            let version_str = String::from_utf8_lossy(&output.stdout);
            let trimmed = version_str.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    // Fallback: use a placeholder that will match until the agent is available.
    format!("{}-unknown", agent_id)
}

/// Show consent status for all tracked agents.
pub fn show_status(project_root: &Path) {
    let store = load(project_root);
    if store.agents.is_empty() {
        println!("No agent terms have been accepted yet.");
        println!("Run `ta terms accept <agent>` to accept agent terms.");
        return;
    }

    println!("Agent consent status:");
    for (agent_id, consent) in &store.agents {
        let current_version = detect_agent_version(agent_id);
        let status = if consent.version == current_version {
            "current"
        } else {
            "outdated"
        };
        println!(
            "  {}: accepted v{} at {} ({})",
            agent_id, consent.version, consent.accepted_at, status
        );
    }
}

/// Display the terms summary for an agent.
/// Since agents don't expose terms text programmatically, we show a generic
/// message directing the user to the agent's documentation.
pub fn show_agent_terms(agent_id: &str) {
    let version = detect_agent_version(agent_id);
    println!("Agent: {}", agent_id);
    println!("Detected version: {}", version);
    println!();
    match agent_id {
        "claude-code" => {
            println!("Claude Code is governed by Anthropic's Terms of Service.");
            println!("Review at: https://www.anthropic.com/terms");
            println!();
            println!("By accepting, you acknowledge that TA will pass `--accept-terms`");
            println!("to Claude Code on your behalf when running goals.");
        }
        "codex" => {
            println!("Codex is governed by OpenAI's Terms of Use.");
            println!("Review at: https://openai.com/terms");
            println!();
            println!("By accepting, you acknowledge that TA will run Codex");
            println!("with `--approval-mode full-auto` on your behalf.");
        }
        _ => {
            println!(
                "No specific terms information available for '{}'.",
                agent_id
            );
            println!("Review the agent's documentation before accepting.");
        }
    }
}

/// Interactive prompt for accepting agent terms.
pub fn prompt_and_accept(project_root: &Path, agent_id: &str) -> anyhow::Result<()> {
    use std::io::Write;

    let version = detect_agent_version(agent_id);
    show_agent_terms(agent_id);
    println!();
    println!("Terms version: {}", version);
    println!("─────────────────────────────────────────────────────");
    print!("Do you accept these terms for {}? [y/N] ", agent_id);
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(anyhow::anyhow!(
            "Terms not accepted for '{}'. Goals using this agent cannot be dispatched.",
            agent_id
        ));
    }

    accept_agent(project_root, agent_id, &version)?;
    println!(
        "\nTerms accepted for {} (v{}). Goals can now use this agent.",
        agent_id, version
    );
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();
        std::fs::create_dir_all(project_root.join(".ta")).unwrap();

        // Initially no consent.
        let store = load(project_root);
        assert!(store.agents.is_empty());

        // Accept agent terms.
        accept_agent(project_root, "claude-code", "1.0.0").unwrap();

        // Verify consent exists.
        let store = load(project_root);
        assert!(store.agents.contains_key("claude-code"));
        assert_eq!(store.agents["claude-code"].version, "1.0.0");

        // Check consent with matching version.
        assert!(check_agent_consent(project_root, "claude-code", "1.0.0").is_ok());

        // Check consent with different version — should fail.
        assert!(check_agent_consent(project_root, "claude-code", "2.0.0").is_err());

        // Check consent for unknown agent — should fail.
        assert!(check_agent_consent(project_root, "unknown-agent", "1.0.0").is_err());
    }

    #[test]
    fn consent_gate_blocks_without_consent() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();

        // No consent file exists — should fail with helpful message.
        let result = check_agent_consent(project_root, "claude-code", "1.0.0");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("ta terms accept"),
            "Error should mention ta terms accept: {}",
            msg
        );
    }

    #[test]
    fn consent_path_resolves_correctly() {
        let root = Path::new("/tmp/my-project");
        let path = consent_path(root);
        assert_eq!(path, PathBuf::from("/tmp/my-project/.ta/consent.json"));
    }
}
