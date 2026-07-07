// team.rs — Virtual team configuration commands (v0.17.0.3).
//
// `ta team list`           — show configured team members from .ta/team.toml
// `ta team assign <role> <agent-id>` — upsert a role assignment in .ta/team.toml

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_session::{AdvisorSecurity, TeamConfig, TeamRole};

#[derive(Debug, Subcommand)]
pub enum TeamCommands {
    /// Show configured team members from .ta/team.toml.
    List,
    /// Assign an agent to a team role in .ta/team.toml.
    ///
    /// Roles are data-defined — any name is valid. Well-known roles:
    /// implementer, reviewer, qa, architect, release_manager. Custom roles
    /// (e.g. security-team) work identically.
    /// Security levels: read_only (default), suggest, auto.
    ///
    /// Examples:
    ///   ta team assign reviewer claude-sonnet-4-6 --security auto --persona strict-reviewer
    ///   ta team assign implementer claude-opus-4-8
    ///   ta team assign security-team claude-opus-4-8
    Assign {
        /// Role to assign — any name (e.g. implementer, reviewer, qa, architect,
        /// release_manager, or a custom role like security-team).
        role: String,
        /// Agent ID (e.g., claude-sonnet-4-6).
        agent_id: String,
        /// Security level for this role: read_only, suggest, or auto (default: read_only).
        #[arg(long, default_value = "read_only")]
        security: String,
        /// Persona name from .ta/personas/ (optional).
        #[arg(long)]
        persona: Option<String>,
    },
}

pub fn execute(command: &TeamCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        TeamCommands::List => cmd_list(config),
        TeamCommands::Assign {
            role,
            agent_id,
            security,
            persona,
        } => cmd_assign(config, role, agent_id, security, persona.as_deref()),
    }
}

fn cmd_list(config: &GatewayConfig) -> anyhow::Result<()> {
    let team = TeamConfig::load(&config.workspace_root)
        .map_err(|e| anyhow::anyhow!("Failed to load .ta/team.toml: {}", e))?;

    if team.members.is_empty() {
        println!("No team members configured.");
        println!("Use `ta team assign <role> <agent-id>` to add members.");
        return Ok(());
    }

    println!(
        "{:<20} {:<30} {:<12} PERSONA",
        "ROLE", "AGENT ID", "SECURITY"
    );
    println!("{}", "-".repeat(80));
    for m in &team.members {
        println!(
            "{:<20} {:<30} {:<12} {}",
            m.role.to_string(),
            m.agent_id,
            m.security.to_string(),
            m.persona.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn cmd_assign(
    config: &GatewayConfig,
    role_str: &str,
    agent_id: &str,
    security_str: &str,
    persona: Option<&str>,
) -> anyhow::Result<()> {
    let role = parse_role(role_str)?;
    let security = parse_security(security_str)?;
    validate_agent_id(agent_id)?;

    let mut team = TeamConfig::load(&config.workspace_root)
        .map_err(|e| anyhow::anyhow!("Failed to load .ta/team.toml: {}", e))?;

    team.assign(
        role.clone(),
        agent_id.to_string(),
        security.clone(),
        persona.map(str::to_string),
    );

    team.save(&config.workspace_root)
        .map_err(|e| anyhow::anyhow!("Failed to save .ta/team.toml: {}", e))?;

    println!(
        "Assigned {} to role '{}' with security '{}'{}.",
        agent_id,
        role,
        security,
        persona
            .map(|p| format!(" and persona '{}'", p))
            .unwrap_or_default()
    );
    Ok(())
}

/// Parse a role name into a `TeamRole`.
///
/// Roles are data-defined (`TA-CONSTITUTION.md` §1.6): any non-empty name is
/// accepted, not just the well-known ones, so teams can declare custom roles
/// (e.g. `security-team`) without a TA core change. `releasemanager` is kept
/// as an alias for `release_manager` for backward compatibility.
fn parse_role(s: &str) -> anyhow::Result<TeamRole> {
    let normalized = s.to_lowercase();
    match normalized.as_str() {
        "" => anyhow::bail!("Role name cannot be empty."),
        "releasemanager" => Ok(TeamRole::release_manager()),
        other if other.starts_with("human:") => {
            Ok(TeamRole::human(other.trim_start_matches("human:")))
        }
        other => Ok(TeamRole::new(other)),
    }
}

fn parse_security(s: &str) -> anyhow::Result<AdvisorSecurity> {
    match s.to_lowercase().as_str() {
        "read_only" | "readonly" => Ok(AdvisorSecurity::ReadOnly),
        "suggest" => Ok(AdvisorSecurity::Suggest),
        "auto" => Ok(AdvisorSecurity::Auto),
        other => anyhow::bail!(
            "Unknown security level '{}'. Valid levels: read_only, suggest, auto.",
            other
        ),
    }
}

/// Validate an agent-id value used at any `Switch` action tier (`ta team assign`,
/// `ta persona set-agent`, workflow.toml's `[agent]`/`[workload_agents]` sections).
///
/// Accepts the literal `"auto"` (case-insensitive) — the v0.17.0.12.13 declaration
/// that hands agent selection to the supervisor's recommendation — alongside any
/// non-empty identifier-like agent ID (letters, digits, `-`, `_`, `.`, `:`; the
/// `:` allows `human:<id>`-style references and manifest-qualified names).
/// Rejects empty strings and anything containing whitespace, which are almost
/// always a copy-paste mistake rather than a real agent ID.
pub fn validate_agent_id(s: &str) -> anyhow::Result<()> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Agent ID cannot be empty. Use 'auto' to let the supervisor pick, or a real agent ID (e.g. claude-sonnet-4-6).");
    }
    if trimmed.eq_ignore_ascii_case("auto") {
        return Ok(());
    }
    if trimmed != s || trimmed.contains(char::is_whitespace) {
        anyhow::bail!(
            "Agent ID '{}' contains whitespace. Use a plain identifier (e.g. claude-sonnet-4-6) or 'auto'.",
            s
        );
    }
    let valid = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if !valid {
        anyhow::bail!(
            "Agent ID '{}' contains invalid characters. Use letters, digits, '-', '_', '.', ':', or 'auto'.",
            s
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_role_well_known_names() {
        assert_eq!(parse_role("implementer").unwrap(), TeamRole::implementer());
        assert_eq!(parse_role("reviewer").unwrap(), TeamRole::reviewer());
        assert_eq!(parse_role("qa").unwrap(), TeamRole::qa());
        assert_eq!(parse_role("architect").unwrap(), TeamRole::architect());
        assert_eq!(
            parse_role("release_manager").unwrap(),
            TeamRole::release_manager()
        );
        assert_eq!(
            parse_role("releasemanager").unwrap(),
            TeamRole::release_manager()
        );
    }

    #[test]
    fn parse_role_human_reference() {
        assert_eq!(parse_role("human:alice").unwrap(), TeamRole::human("alice"));
    }

    #[test]
    fn parse_role_accepts_custom_data_defined_role() {
        // Data-defined roles (TA-CONSTITUTION.md §1.6): any non-empty name is
        // valid, not just the well-known ones.
        assert_eq!(
            parse_role("security-team").unwrap(),
            TeamRole::new("security-team")
        );
    }

    #[test]
    fn parse_role_rejects_empty() {
        assert!(parse_role("").is_err());
    }

    // ── v0.17.0.12.13: agent-id validation (accepts "auto") ──────────

    #[test]
    fn validate_agent_id_accepts_auto_case_insensitive() {
        assert!(validate_agent_id("auto").is_ok());
        assert!(validate_agent_id("Auto").is_ok());
        assert!(validate_agent_id("AUTO").is_ok());
    }

    #[test]
    fn validate_agent_id_accepts_real_agent_ids() {
        assert!(validate_agent_id("claude-sonnet-4-6").is_ok());
        assert!(validate_agent_id("claude-opus-4-8").is_ok());
        assert!(validate_agent_id("human:alice").is_ok());
        assert!(validate_agent_id("manifest.custom_agent").is_ok());
    }

    #[test]
    fn validate_agent_id_rejects_empty_and_whitespace() {
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("   ").is_err());
        assert!(validate_agent_id("claude sonnet").is_err());
        assert!(validate_agent_id(" claude-sonnet").is_err());
    }

    #[test]
    fn validate_agent_id_rejects_invalid_characters() {
        assert!(validate_agent_id("claude/../sonnet").is_err());
        assert!(validate_agent_id("claude;rm -rf").is_err());
    }
}
