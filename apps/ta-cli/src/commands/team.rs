// team.rs — Virtual team configuration commands (v0.17.0.3).
//
// `ta team list`           — show configured team members from .ta/team.toml
// `ta team assign <role> <agent-id>` — upsert a role assignment in .ta/team.toml

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_session::{AdvisorSecurity, TeamConfig, TeamRole};

#[derive(Subcommand)]
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
}
