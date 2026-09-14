// credentials.rs — Credential vault subcommands.
//
// Manage stored credentials that agents access through scoped session tokens.
// Agents never see raw secrets — TA brokers access via time-limited tokens.

use clap::Subcommand;
use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};
use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
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
    /// Rotate a credential's secret in place, keeping its name, service,
    /// scopes, and id unchanged. Use this instead of `revoke` + `add` when
    /// swapping in a new key for the same credential (e.g. after rotating a
    /// model-provider API key) — anything that already refers to this
    /// credential by id (an issued grant, a `.ta/team.toml` role binding)
    /// keeps working, it just resolves to the new secret on next use.
    Update {
        /// Credential ID (UUID) or prefix.
        id: String,
        /// The new secret value.
        #[arg(long)]
        secret: String,
    },
    /// Revoke (delete) a credential by ID.
    Revoke {
        /// Credential ID (UUID) or prefix.
        id: String,
    },
    /// Issue a scoped, time-limited session grant for an agent (v0.17.6.2;
    /// migrated to biscuit tokens in v0.17.6.4).
    ///
    /// This is the real credential-delivery path: the grant records who it
    /// was issued to, which scopes it authorizes, and when it expires,
    /// cryptographically signed by `ta-credential-broker` so any process
    /// holding the broker's public key (e.g. the MCP gateway) can verify it
    /// offline. It does not itself hand back the underlying secret — `ta
    /// run` uses the same `CredentialBroker::grant` path internally to gate
    /// secret delivery into an agent's environment.
    Grant {
        /// Credential ID (UUID) or prefix.
        id: String,
        /// Agent identifier the token is issued to (e.g. a goal ID).
        #[arg(long)]
        agent: String,
        /// Scopes to grant (repeatable). Must be a subset of the
        /// credential's own declared scopes; an unscoped credential grants
        /// whatever scopes are requested here.
        #[arg(long)]
        scope: Vec<String>,
        /// Time-to-live in seconds before the token expires.
        #[arg(long)]
        ttl: u64,
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
        CredentialsCommands::Update { id, secret } => update_credential(config, id, secret),
        CredentialsCommands::Revoke { id } => revoke_credential(config, id),
        CredentialsCommands::Grant {
            id,
            agent,
            scope,
            ttl,
        } => grant_token(config, id, agent, scope, *ttl),
    }
}

fn cred_config(config: &GatewayConfig) -> CredentialsConfig {
    let mut cred_config = CredentialsConfig::for_project(&config.workspace_root);
    cred_config.use_keychain = config.credential_vault_use_keychain;
    cred_config
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

/// Resolve `id_str` (a full credential UUID or an unambiguous prefix of one)
/// to a single credential summary. Shared by `revoke`, `grant`, and `update`
/// — all three accept a prefix rather than requiring the full UUID.
fn resolve_credential_prefix<'a>(
    creds: &'a [ta_credentials::CredentialSummary],
    id_str: &str,
) -> anyhow::Result<&'a ta_credentials::CredentialSummary> {
    let matches: Vec<_> = creds
        .iter()
        .filter(|c| c.id.to_string().starts_with(id_str))
        .collect();
    match matches.len() {
        0 => anyhow::bail!("No credential found matching '{}'", id_str),
        1 => Ok(matches[0]),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} credentials. Use a longer prefix.",
            id_str,
            n
        ),
    }
}

fn update_credential(config: &GatewayConfig, id_str: &str, secret: &str) -> anyhow::Result<()> {
    let mut vault = FileVault::open(&cred_config(config))?;
    let creds = vault.list()?;
    let id = resolve_credential_prefix(&creds, id_str)?.id;

    let updated = vault.update(id, secret)?;
    println!("Credential rotated:");
    println!("  ID:      {}", updated.id);
    println!("  Name:    {}", updated.name);
    println!("  Service: {}", updated.service);
    if !updated.scopes.is_empty() {
        println!("  Scopes:  {}", updated.scopes.join(", "));
    }
    println!(
        "Existing grants for this credential remain valid; the next agent launch \
         picks up the new secret."
    );
    Ok(())
}

/// Where the broker's root key and revocation denylist live — alongside
/// `credentials.json`, inside the project's `.ta` dir.
fn broker_dir(config: &GatewayConfig) -> std::path::PathBuf {
    cred_config(config)
        .vault_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config.workspace_root.join(".ta"))
}

/// Resolve `id_str` (a credential id or prefix) and mint a biscuit-backed
/// grant for it via [`ta_credential_broker::CredentialBroker`]. Split out
/// from `grant_token` so the minted token itself (not just "did this print
/// without erroring") is assertable in tests.
fn mint_grant(
    config: &GatewayConfig,
    id_str: &str,
    agent: &str,
    scopes: &[String],
    ttl_secs: u64,
) -> anyhow::Result<(
    ta_credentials::CredentialSummary,
    ta_credential_broker::GrantedToken,
)> {
    let vault = FileVault::open(&cred_config(config))?;
    let creds = vault.list()?;
    let cred = resolve_credential_prefix(&creds, id_str)?.clone();

    let broker = ta_credential_broker::CredentialBroker::open(&broker_dir(config))?;
    let granted = broker.grant(cred.id, agent, scopes.to_vec(), ttl_secs)?;
    Ok((cred, granted))
}

fn grant_token(
    config: &GatewayConfig,
    id_str: &str,
    agent: &str,
    scopes: &[String],
    ttl_secs: u64,
) -> anyhow::Result<()> {
    let (cred, granted) = mint_grant(config, id_str, agent, scopes, ttl_secs)?;
    println!("Session token issued:");
    println!("  Token:      {}", granted.token);
    println!("  Token ID:   {}", granted.token_id);
    println!("  Credential: {} ({})", cred.name, cred.id);
    println!("  Agent:      {}", granted.agent_id);
    if !granted.allowed_scopes.is_empty() {
        println!("  Scopes:     {}", granted.allowed_scopes.join(", "));
    }
    println!(
        "  Expires:    {}",
        granted.expires_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    Ok(())
}

fn revoke_credential(config: &GatewayConfig, id_str: &str) -> anyhow::Result<()> {
    let mut vault = FileVault::open(&cred_config(config))?;
    let creds = vault.list()?;
    let resolved = resolve_credential_prefix(&creds, id_str)?;
    let id = resolved.id;
    let name = resolved.name.clone();

    vault.revoke(id)?;
    println!("Revoked credential '{}' ({})", name, id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        config
    }

    #[test]
    fn grant_mints_a_broker_verifiable_token_not_a_vault_only_uuid() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        add_credential(&config, "svc", "svc", "secret", &["read".into()]).unwrap();
        let cred_id = FileVault::open(&cred_config(&config))
            .unwrap()
            .list()
            .unwrap()[0]
            .id;

        let (cred, granted) = mint_grant(
            &config,
            &cred_id.to_string(),
            "agent-1",
            &["read".into()],
            3600,
        )
        .unwrap();
        assert_eq!(cred.id, cred_id);

        // The whole point of the migration: a *different* CredentialBroker
        // instance, opened fresh on the same `.ta` dir (standing in for the
        // gateway process, which never shares memory with this CLI process),
        // can verify the token purely from what it was handed — no lookup
        // into `vault.tokens` required, unlike the old UUID SessionToken.
        let broker = ta_credential_broker::CredentialBroker::open(&broker_dir(&config)).unwrap();
        let verified = broker.verify(&granted.token).unwrap();
        assert_eq!(verified.credential_id, cred_id);
        assert_eq!(verified.agent_id, "agent-1");
        assert_eq!(verified.allowed_scopes, vec!["read".to_string()]);
    }

    #[test]
    fn grant_for_unknown_prefix_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        let result = mint_grant(&config, "deadbeef", "agent-1", &[], 3600);
        assert!(result.is_err());
    }

    #[test]
    fn update_rotates_secret_and_is_resolvable_by_prefix() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        add_credential(&config, "api-key", "anthropic", "old-secret", &[]).unwrap();
        let cred_id = FileVault::open(&cred_config(&config))
            .unwrap()
            .list()
            .unwrap()[0]
            .id;
        let prefix = &cred_id.to_string()[..8];

        update_credential(&config, prefix, "new-secret").unwrap();

        let vault = FileVault::open(&cred_config(&config)).unwrap();
        assert_eq!(vault.get(cred_id).unwrap().secret, "new-secret");
        // Name/service/scopes unchanged by rotation.
        let summary = &vault.list().unwrap()[0];
        assert_eq!(summary.name, "api-key");
        assert_eq!(summary.service, "anthropic");
    }

    #[test]
    fn update_unknown_prefix_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        let result = update_credential(&config, "deadbeef", "new-secret");
        assert!(result.is_err());
    }

    #[test]
    fn update_ambiguous_prefix_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        add_credential(&config, "a", "svc", "secret1", &[]).unwrap();
        add_credential(&config, "b", "svc", "secret2", &[]).unwrap();

        // The empty string is a prefix of every id.
        let result = update_credential(&config, "", "new-secret");
        assert!(result.is_err());
    }

    #[test]
    fn existing_grant_survives_a_rotation() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        add_credential(&config, "svc", "svc", "old-secret", &["read".into()]).unwrap();
        let cred_id = FileVault::open(&cred_config(&config))
            .unwrap()
            .list()
            .unwrap()[0]
            .id;

        let (_, granted) = mint_grant(
            &config,
            &cred_id.to_string(),
            "agent-1",
            &["read".into()],
            3600,
        )
        .unwrap();

        update_credential(&config, &cred_id.to_string(), "new-secret").unwrap();

        // The grant minted before rotation still verifies — rotation only
        // changes the secret a subsequent `vault.get` returns, not the
        // credential's identity or any already-issued grant.
        let broker = ta_credential_broker::CredentialBroker::open(&broker_dir(&config)).unwrap();
        let verified = broker.verify(&granted.token).unwrap();
        assert_eq!(verified.credential_id, cred_id);

        let vault = FileVault::open(&cred_config(&config)).unwrap();
        assert_eq!(vault.get(cred_id).unwrap().secret, "new-secret");
    }

    #[test]
    fn revoke_by_prefix_still_works_after_extracting_shared_helper() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        add_credential(&config, "svc", "svc", "secret", &[]).unwrap();
        let cred_id = FileVault::open(&cred_config(&config))
            .unwrap()
            .list()
            .unwrap()[0]
            .id;
        let prefix = &cred_id.to_string()[..8];

        revoke_credential(&config, prefix).unwrap();

        let vault = FileVault::open(&cred_config(&config)).unwrap();
        assert!(vault.list().unwrap().is_empty());
    }
}
