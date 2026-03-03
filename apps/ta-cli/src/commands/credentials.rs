// credentials.rs — Credential vault subcommands.
//
// Manage stored credentials that agents access through scoped session tokens.
// Agents never see raw secrets — TA brokers access via time-limited tokens.

use clap::Subcommand;
use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum CredentialsCommands {
    /// Add a credential to the vault.
    Add {
        /// Human-readable name (e.g., "gmail-personal").
        #[arg(long)]
        name: String,
        /// Service identifier (e.g., "gmail", "slack").
        #[arg(long)]
        service: String,
        /// The secret value (API key, token, etc.).
        #[arg(long)]
        secret: String,
        /// Scopes this credential grants (repeatable).
        #[arg(long)]
        scope: Vec<String>,
    },
    /// List all stored credentials (secrets are hidden).
    List,
    /// Revoke (delete) a credential by ID.
    Revoke {
        /// Credential ID (UUID) or prefix.
        id: String,
    },
}

pub fn execute(cmd: &CredentialsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        CredentialsCommands::Add {
            name,
            service,
            secret,
            scope,
        } => add_credential(config, name, service, secret, scope),
        CredentialsCommands::List => list_credentials(config),
        CredentialsCommands::Revoke { id } => revoke_credential(config, id),
    }
}

fn cred_config(config: &GatewayConfig) -> CredentialsConfig {
    CredentialsConfig::for_project(&config.workspace_root)
}

fn add_credential(
    config: &GatewayConfig,
    name: &str,
    service: &str,
    secret: &str,
    scopes: &[String],
) -> anyhow::Result<()> {
    let mut vault = FileVault::open(&cred_config(config))?;
    let cred = vault.add(name, service, secret, scopes.to_vec())?;
    println!("Credential added:");
    println!("  ID:      {}", cred.id);
    println!("  Name:    {}", cred.name);
    println!("  Service: {}", cred.service);
    if !cred.scopes.is_empty() {
        println!("  Scopes:  {}", cred.scopes.join(", "));
    }
    Ok(())
}

fn list_credentials(config: &GatewayConfig) -> anyhow::Result<()> {
    let vault = FileVault::open(&cred_config(config))?;
    let creds = vault.list()?;

    if creds.is_empty() {
        println!("No credentials stored.");
        println!();
        println!("Add one with: ta credentials add --name <name> --service <svc> --secret <token>");
        return Ok(());
    }

    println!("Stored credentials:");
    println!();
    for c in &creds {
        println!("  {} ({})", c.name, c.id);
        println!("    Service: {}", c.service);
        if !c.scopes.is_empty() {
            println!("    Scopes:  {}", c.scopes.join(", "));
        }
        println!("    Created: {}", c.created_at.format("%Y-%m-%d %H:%M UTC"));
        println!();
    }
    Ok(())
}

fn revoke_credential(config: &GatewayConfig, id_str: &str) -> anyhow::Result<()> {
    let mut vault = FileVault::open(&cred_config(config))?;

    // Support prefix matching.
    let creds = vault.list()?;
    let matches: Vec<_> = creds
        .iter()
        .filter(|c| c.id.to_string().starts_with(id_str))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No credential found matching '{}'", id_str),
        1 => {
            let id = matches[0].id;
            let name = &matches[0].name;
            vault.revoke(id)?;
            println!("Revoked credential '{}' ({})", name, id);
            Ok(())
        }
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} credentials. Use a longer prefix.",
            id_str,
            n
        ),
    }
}
