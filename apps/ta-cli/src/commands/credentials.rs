// credentials.rs — Credential vault subcommands.
//
// Manage stored credentials that agents access through scoped session tokens.
// Agents never see raw secrets — TA brokers access via time-limited tokens.

use clap::Subcommand;
use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};
use ta_mcp_gateway::GatewayConfig;

/// Where a credential's secret comes from. Exactly one source is required.
///
/// `--secret-stdin` and `--secret-env` keep the secret out of the process
/// list and the shell history; `--secret` still works but warns, because its
/// value is visible in `ps` to every user on the machine.
#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("secret_source")
        .required(true)
        .multiple(false)
        .args(["secret", "secret_stdin", "secret_env"])
))]
pub struct SecretInput {
    /// The secret on the command line (API key, token, etc.). Visible in
    /// `ps` and shell history; prefer --secret-stdin or --secret-env.
    #[arg(long, value_name = "SECRET")]
    secret: Option<String>,
    /// Read the secret from standard input (to end of input; one trailing
    /// newline is dropped). Example: `printf %s "$TOKEN" | ta credentials add ...`
    #[arg(long)]
    secret_stdin: bool,
    /// Read the secret from the environment variable NAME.
    #[arg(long, value_name = "NAME")]
    secret_env: Option<String>,
}

// A secret must not leak through `{:?}` of a parsed command line.
impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretInput")
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field("secret_stdin", &self.secret_stdin)
            .field("secret_env", &self.secret_env)
            .finish()
    }
}

/// The variable name to show in a message. Only a conventional UPPER_SNAKE
/// name is echoed: a user who mistakenly passes the secret itself as the name
/// (`--secret-env ghp_abc...`) must not see it printed back into logs.
fn env_name_for_message(name: &str) -> String {
    let conventional = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if conventional {
        name.to_string()
    } else {
        "<name not shown: it is not an UPPER_CASE variable name>".to_string()
    }
}

impl SecretInput {
    /// Resolve the secret from the chosen source, warning on stderr when it
    /// came from argv. Errors name the source, never the value.
    pub fn resolve(&self) -> anyhow::Result<String> {
        use std::io::IsTerminal;
        let stdin = std::io::stdin();
        if self.secret_stdin && stdin.is_terminal() {
            eprintln!("Reading the secret from standard input; finish with Ctrl-D.");
        }
        self.resolve_with(
            &mut stdin.lock(),
            &|name| std::env::var(name).ok(),
            &mut std::io::stderr(),
        )
    }

    fn resolve_with(
        &self,
        stdin: &mut dyn std::io::Read,
        env: &dyn Fn(&str) -> Option<String>,
        warn: &mut dyn std::io::Write,
    ) -> anyhow::Result<String> {
        let secret = if let Some(value) = &self.secret {
            let _ = writeln!(
                warn,
                "warning: --secret puts the secret in the process list (visible in `ps`) and \
                 your shell history. Use --secret-stdin (printf %s \"$TOKEN\" | ta credentials \
                 ...) or --secret-env NAME instead."
            );
            value.clone()
        } else if self.secret_stdin {
            let mut raw = String::new();
            stdin.read_to_string(&mut raw).map_err(|e| {
                anyhow::anyhow!(
                    "Could not read the secret from standard input ({}). The input must be \
                     valid UTF-8 text; pipe it in, e.g. `printf %s \"$TOKEN\" | ta credentials \
                     add ... --secret-stdin`.",
                    e.kind()
                )
            })?;
            if raw.ends_with('\n') {
                raw.pop();
                if raw.ends_with('\r') {
                    raw.pop();
                }
            }
            raw
        } else if let Some(name) = &self.secret_env {
            let shown = env_name_for_message(name);
            env(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "--secret-env {shown}: the environment variable is not set (or is not valid \
                     UTF-8). Export it in the shell that runs this command, e.g. \
                     `export NAME=...`, then retry. (Pass the variable's NAME, not the secret.)"
                )
            })?
        } else {
            anyhow::bail!(
                "No secret source given. Use --secret-stdin or --secret-env NAME (or, less \
                 safely, --secret VALUE)."
            );
        };
        if secret.is_empty() {
            let source = if self.secret_stdin {
                "standard input".to_string()
            } else if let Some(name) = &self.secret_env {
                env_name_for_message(name)
            } else {
                "--secret".to_string()
            };
            anyhow::bail!(
                "The secret from {source} is empty, so nothing was stored. Supply the secret \
                 value, e.g. `printf %s \"$TOKEN\" | ta credentials add ... --secret-stdin`."
            );
        }
        Ok(secret)
    }
}

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
        #[command(flatten)]
        secret: SecretInput,
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
        #[command(flatten)]
        secret: SecretInput,
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
        } => {
            let secret = secret.resolve()?;
            add_credential(config, name, service, &secret, scope)
        }
        CredentialsCommands::List => list_credentials(config),
        CredentialsCommands::Update { id, secret } => {
            let secret = secret.resolve()?;
            update_credential(config, id, &secret)
        }
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
        println!(
            "Add one with: printf %s \"$TOKEN\" | ta credentials add --name <name> --service <svc> --secret-stdin"
        );
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

    let broker = ta_credential_broker::CredentialBroker::open_for_project(&config.workspace_root)?;
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
        let broker =
            ta_credential_broker::CredentialBroker::open_for_project(&config.workspace_root)
                .unwrap();
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
        let broker =
            ta_credential_broker::CredentialBroker::open_for_project(&config.workspace_root)
                .unwrap();
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

    // ── secret input (stdin / env / argv) ───────────────────────────────────

    #[derive(clap::Parser, Debug)]
    struct Wrap {
        #[command(subcommand)]
        cmd: CredentialsCommands,
    }

    fn parse(args: &[&str]) -> Result<CredentialsCommands, clap::Error> {
        use clap::Parser;
        let argv: Vec<&str> = ["ta"].iter().chain(args).copied().collect();
        Wrap::try_parse_from(argv).map(|w| w.cmd)
    }

    fn secret_of(cmd: CredentialsCommands) -> SecretInput {
        match cmd {
            CredentialsCommands::Add { secret, .. }
            | CredentialsCommands::Update { secret, .. } => secret,
            other => panic!("not add/update: {other:?}"),
        }
    }

    const ADD: [&str; 5] = ["add", "--name", "n", "--service", "s"];

    fn add_with(extra: &[&str]) -> Result<SecretInput, clap::Error> {
        let args: Vec<&str> = ADD.iter().chain(extra).copied().collect();
        parse(&args).map(secret_of)
    }

    fn resolve(
        input: &SecretInput,
        stdin: &str,
        env: &[(&str, &str)],
    ) -> (anyhow::Result<String>, String) {
        let mut warn = Vec::new();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let result = input.resolve_with(
            &mut stdin.as_bytes(),
            &|name| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            &mut warn,
        );
        (result, String::from_utf8(warn).unwrap())
    }

    #[test]
    fn exactly_one_secret_source_is_required() {
        assert!(add_with(&[]).is_err(), "no source must be rejected");
        assert!(add_with(&["--secret-stdin"]).is_ok());
        assert!(add_with(&["--secret-env", "TOKEN"]).is_ok());
        assert!(add_with(&["--secret", "x"]).is_ok());
        assert!(add_with(&["--secret-stdin", "--secret-env", "TOKEN"]).is_err());
        let err = add_with(&["--secret", "hunter2-do-not-print", "--secret-stdin"])
            .expect_err("conflicting sources must be rejected");
        assert!(
            !err.to_string().contains("hunter2-do-not-print"),
            "clap's conflict error must not echo the value: {err}"
        );
        // `update` takes the same sources.
        assert!(parse(&["update", "abcd1234", "--secret-stdin"]).is_ok());
        assert!(parse(&["update", "abcd1234"]).is_err());
    }

    #[test]
    fn stdin_secret_is_read_to_the_end_with_one_trailing_newline_dropped() {
        let input = add_with(&["--secret-stdin"]).unwrap();
        for (fed, want) in [
            ("tok-123", "tok-123"),
            ("tok-123\n", "tok-123"),
            ("tok-123\r\n", "tok-123"),
            ("tok-123\n\n", "tok-123\n"),
            ("a b\nc", "a b\nc"),
        ] {
            let (got, warn) = resolve(&input, fed, &[]);
            assert_eq!(got.unwrap(), want, "fed {fed:?}");
            assert!(
                warn.is_empty(),
                "stdin must not trigger the argv warning: {warn}"
            );
        }
    }

    #[test]
    fn a_stdin_secret_never_reaches_stdout_stderr_logs_or_error_text() {
        const SECRET: &str = "s3cr3t-from-stdin-7f3a";
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let input = add_with(&["--secret-stdin"]).unwrap();
        let (secret, warn) = resolve(&input, &format!("{SECRET}\n"), &[]);
        let secret = secret.unwrap();
        assert_eq!(secret, SECRET);

        add_credential(&config, "svc", "svc", &secret, &[]).unwrap();
        // It is stored...
        let vault = FileVault::open(&cred_config(&config)).unwrap();
        let id = vault.list().unwrap()[0].id;
        assert_eq!(vault.get(id).unwrap().secret, SECRET);
        // ...and appears nowhere a human or a log would see it.
        assert!(!warn.contains(SECRET), "warning leaked it: {warn}");
        let parsed_debug = format!("{input:?}");
        assert!(
            !parsed_debug.contains(SECRET),
            "Debug leaked it: {parsed_debug}"
        );
        let on_disk = std::fs::read_to_string(dir.path().join(".ta").join("credentials.json"))
            .unwrap_or_default();
        assert!(
            !on_disk.contains(SECRET),
            "the vault file holds it in clear text"
        );
        // The error for a failed resolve names the source, not the value.
        let (empty, _) = resolve(&input, "\n", &[]);
        let msg = format!("{:#}", empty.unwrap_err());
        assert!(
            msg.contains("standard input") && msg.contains("empty"),
            "{msg}"
        );
    }

    #[test]
    fn an_argv_secret_warns_without_echoing_the_value() {
        const SECRET: &str = "argv-secret-9c1d";
        let input = add_with(&["--secret", SECRET]).unwrap();
        let (got, warn) = resolve(&input, "", &[]);
        assert_eq!(got.unwrap(), SECRET);
        assert!(warn.contains("warning") && warn.contains("`ps`"), "{warn}");
        assert!(
            warn.contains("--secret-stdin") && warn.contains("--secret-env"),
            "{warn}"
        );
        assert!(
            !warn.contains(SECRET),
            "the warning must not repeat the secret: {warn}"
        );
        let debug = format!("{input:?}");
        assert!(
            !debug.contains(SECRET) && debug.contains("redacted"),
            "{debug}"
        );
    }

    #[test]
    fn env_secret_is_read_from_the_named_variable_only() {
        let input = add_with(&["--secret-env", "MY_TOKEN"]).unwrap();
        let (got, warn) = resolve(
            &input,
            "ignored",
            &[("MY_TOKEN", "from-env"), ("OTHER", "x")],
        );
        assert_eq!(got.unwrap(), "from-env");
        assert!(warn.is_empty());

        let (missing, _) = resolve(&input, "", &[("OTHER", "x")]);
        let msg = format!("{:#}", missing.unwrap_err());
        assert!(msg.contains("MY_TOKEN") && msg.contains("not set"), "{msg}");

        let (empty, _) = resolve(&input, "", &[("MY_TOKEN", "")]);
        let msg = format!("{:#}", empty.unwrap_err());
        assert!(msg.contains("MY_TOKEN") && msg.contains("empty"), "{msg}");
    }

    /// `--secret-env ghp_realtoken...` is a plausible slip; the error must not
    /// print the token back.
    #[test]
    fn a_secret_passed_as_the_variable_name_is_not_echoed() {
        const SLIP: &str = "ghp_RealLookingToken123";
        let input = add_with(&["--secret-env", SLIP]).unwrap();
        let (got, _) = resolve(&input, "", &[]);
        let msg = format!("{:#}", got.unwrap_err());
        assert!(!msg.contains(SLIP), "{msg}");
        assert!(
            msg.contains("NAME"),
            "the fix must still be actionable: {msg}"
        );
    }

    #[test]
    fn an_empty_argv_secret_is_refused() {
        let input = add_with(&["--secret", ""]).unwrap();
        let (got, _) = resolve(&input, "", &[]);
        assert!(format!("{:#}", got.unwrap_err()).contains("empty"));
    }

    #[test]
    fn invalid_utf8_on_stdin_is_a_clear_error() {
        let input = add_with(&["--secret-stdin"]).unwrap();
        let mut warn = Vec::new();
        let bytes: &[u8] = &[0xff, 0xfe, 0x41];
        let err = input
            .resolve_with(&mut &*bytes, &|_| None, &mut warn)
            .unwrap_err();
        assert!(format!("{err:#}").contains("UTF-8"), "{err:#}");
    }
}
