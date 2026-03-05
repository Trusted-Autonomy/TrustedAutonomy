// token.rs -- Token management CLI for non-interactive approval.

use clap::Subcommand;
use ta_events::tokens::TokenStore;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum TokenCommands {
    /// Create a new approval token.
    Create {
        /// Token scope (e.g., "draft:approve", "draft:deny").
        #[arg(long, default_value = "draft:approve")]
        scope: String,
        /// Expiration duration (e.g., "24h", "7d", "1h").
        #[arg(long, default_value = "24h")]
        expires: String,
    },
    /// List all tokens.
    List,
    /// Clean up expired tokens.
    Cleanup,
}

pub fn execute(cmd: &TokenCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let tokens_dir = config.workspace_root.join(".ta").join("tokens");
    let store = TokenStore::new(&tokens_dir);

    match cmd {
        TokenCommands::Create { scope, expires } => create_token(&store, scope, expires),
        TokenCommands::List => list_tokens(&store),
        TokenCommands::Cleanup => cleanup_tokens(&store),
    }
}

fn parse_token_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid number in duration '{}'", s))?;
    match unit {
        "d" => Ok(chrono::Duration::days(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "m" => Ok(chrono::Duration::minutes(n)),
        _ => anyhow::bail!(
            "unknown duration unit '{}'. Use d (days), h (hours), or m (minutes)",
            unit
        ),
    }
}

fn create_token(store: &TokenStore, scope: &str, expires: &str) -> anyhow::Result<()> {
    let duration = parse_token_duration(expires)?;
    let token = store.create(scope, duration)?;

    println!("Token created:");
    println!("  ID:      {}", token.id);
    println!("  Scope:   {}", token.scope);
    println!("  Token:   {}", token.token_value);
    println!(
        "  Expires: {}",
        token.expires_at.format("%Y-%m-%d %H:%M UTC")
    );
    println!();
    println!(
        "Use with: ta draft approve <id> --token {}",
        token.token_value
    );

    Ok(())
}

fn list_tokens(store: &TokenStore) -> anyhow::Result<()> {
    let tokens = store.list()?;

    if tokens.is_empty() {
        println!("No tokens found.");
        println!("Create one with: ta token create --scope draft:approve");
        return Ok(());
    }

    println!("Approval tokens ({}):", tokens.len());
    println!();
    for t in &tokens {
        let status = if t.used {
            "USED"
        } else if t.is_expired() {
            "EXPIRED"
        } else {
            "VALID"
        };
        println!(
            "  {} [{}] scope={} expires={}",
            &t.token_value[..std::cmp::min(20, t.token_value.len())],
            status,
            t.scope,
            t.expires_at.format("%Y-%m-%d %H:%M UTC"),
        );
    }

    Ok(())
}

fn cleanup_tokens(store: &TokenStore) -> anyhow::Result<()> {
    let removed = store.cleanup()?;
    println!("Cleaned up {} expired token(s).", removed);
    Ok(())
}
