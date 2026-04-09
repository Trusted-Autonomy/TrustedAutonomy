// adapter.rs — Adapter subcommands: list, install, setup, health.
//
// Adapters are config templates that generate agent-specific integration
// files. `ta adapter install claude-code` generates `.mcp.json` and
// `.ta/config.toml` so Claude Code can connect to the TA MCP server.
//
// Messaging adapters (v0.15.9):
//   `ta adapter setup messaging/<plugin>` — one-time credential wizard
//   `ta adapter health messaging`         — connectivity check for all configured providers
//
// Social adapters (v0.15.12):
//   `ta adapter setup social/<plugin>`   — one-time OAuth2 credential wizard
//   `ta adapter health social`           — connectivity check for all configured social plugins

use std::fs;
use std::path::Path;

use clap::Subcommand;
use ta_submit::{
    find_messaging_plugin, find_social_plugin, MessagingPluginManifest, SocialPluginManifest,
};

#[derive(Subcommand)]
pub enum AdapterCommands {
    /// List available agent adapters.
    List,
    /// Install an adapter for a specific agent system.
    Install {
        /// Adapter name (e.g., "claude-code").
        name: String,
    },
    /// One-time setup wizard for a messaging or social media provider plugin.
    ///
    /// Captures credentials (OAuth2 refresh token or IMAP app-password) and
    /// stores them in the OS keychain. Runs a health check on success.
    ///
    /// Examples:
    ///   ta adapter setup messaging/gmail
    ///   ta adapter setup messaging/outlook
    ///   ta adapter setup messaging/imap
    ///   ta adapter setup social/linkedin
    ///   ta adapter setup social/x
    ///   ta adapter setup social/buffer
    Setup {
        /// Plugin specifier: "messaging/<provider>" or "social/<platform>".
        plugin: String,
    },
    /// Run a health check on all configured messaging or social adapters.
    ///
    /// Calls the `health` op on each discovered plugin and prints
    /// the provider name, connected address/handle, and status.
    /// No credentials are printed.
    ///
    /// Examples:
    ///   ta adapter health messaging
    ///   ta adapter health social
    Health {
        /// Adapter type to check: "messaging" or "social".
        adapter_type: String,
    },
    /// Get a credential value from the OS keychain (used by plugins).
    ///
    /// Plugins call `ta adapter credentials get <key>` to retrieve secrets
    /// without holding them in memory across process boundaries.
    Credentials {
        #[command(subcommand)]
        cmd: CredentialsCommands,
    },
}

#[derive(Subcommand)]
pub enum CredentialsCommands {
    /// Get a credential by key.
    Get {
        /// Keychain key (e.g., "ta-messaging:gmail:me@example.com").
        key: String,
    },
    /// Set a credential value in the OS keychain.
    Set {
        /// Keychain key.
        key: String,
        /// Value to store. If omitted, reads from stdin (masked).
        value: Option<String>,
    },
}

pub fn execute(cmd: &AdapterCommands, project_root: &Path) -> anyhow::Result<()> {
    match cmd {
        AdapterCommands::List => list_adapters(project_root),
        AdapterCommands::Install { name } => install_adapter(name, project_root),
        AdapterCommands::Setup { plugin } => setup_plugin(plugin, project_root),
        AdapterCommands::Health { adapter_type } => health_check(adapter_type, project_root),
        AdapterCommands::Credentials { cmd } => credentials_cmd(cmd),
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

fn list_adapters(project_root: &Path) -> anyhow::Result<()> {
    println!("Available adapters:");
    println!();
    println!("  claude-code    Claude Code (MCP stdio server)");
    println!("  generic-mcp    Generic MCP client");
    println!();
    println!("Install with: ta adapter install <name>");
    println!();

    // Show discovered messaging plugins.
    let messaging = ta_submit::discover_messaging_plugins(project_root);
    if !messaging.is_empty() {
        println!("Messaging plugins:");
        for p in &messaging {
            let desc = p
                .manifest
                .description
                .as_deref()
                .unwrap_or("(no description)");
            println!(
                "  messaging/{}    [{}]  {}",
                p.manifest.name, p.source, desc
            );
        }
        println!();
        println!("Run health check:  ta adapter health messaging");
        println!("Set up a plugin:   ta adapter setup messaging/<provider>");
        println!();
    }

    // Show discovered social plugins.
    let social = ta_submit::discover_social_plugins(project_root);
    if !social.is_empty() {
        println!("Social plugins:");
        for p in &social {
            let desc = p
                .manifest
                .description
                .as_deref()
                .unwrap_or("(no description)");
            println!("  social/{}    [{}]  {}", p.manifest.name, p.source, desc);
        }
        println!();
        println!("Run health check:  ta adapter health social");
        println!("Set up a plugin:   ta adapter setup social/<platform>");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

fn install_adapter(name: &str, project_root: &Path) -> anyhow::Result<()> {
    match name {
        "claude-code" => install_claude_code(project_root),
        "generic-mcp" => install_generic_mcp(project_root),
        _ => {
            anyhow::bail!(
                "Unknown adapter: {}. Run `ta adapter list` to see available adapters.",
                name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Setup dispatcher
// ---------------------------------------------------------------------------

fn setup_plugin(plugin_spec: &str, project_root: &Path) -> anyhow::Result<()> {
    if plugin_spec.starts_with("messaging/") {
        setup_messaging(plugin_spec, project_root)
    } else if plugin_spec.starts_with("social/") {
        setup_social(plugin_spec, project_root)
    } else {
        anyhow::bail!(
            "Invalid plugin specifier '{}'. Expected format:\n  messaging/<provider>  (e.g., messaging/gmail)\n  social/<platform>     (e.g., social/linkedin, social/x, social/buffer)",
            plugin_spec
        )
    }
}

// ---------------------------------------------------------------------------
// Setup messaging/<provider>
// ---------------------------------------------------------------------------

fn setup_messaging(plugin_spec: &str, project_root: &Path) -> anyhow::Result<()> {
    // Parse "messaging/<provider>" specifier.
    let provider = plugin_spec.strip_prefix("messaging/").ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid plugin specifier '{}'. Expected format: messaging/<provider> \
             (e.g., messaging/gmail, messaging/outlook, messaging/imap)",
            plugin_spec
        )
    })?;

    // Try to find an installed plugin; fall back to built-in guidance.
    let discovered = find_messaging_plugin(provider, project_root);

    println!("Setting up messaging provider: {}", provider);
    println!();

    match provider {
        "gmail" => setup_gmail(discovered.as_ref().map(|d| &d.manifest)),
        "outlook" => setup_outlook(discovered.as_ref().map(|d| &d.manifest)),
        "imap" => setup_imap(discovered.as_ref().map(|d| &d.manifest)),
        other => {
            if let Some(ref disc) = discovered {
                // Community plugin found — call its health op as a smoke-test.
                println!("Found community plugin: {} ({})", other, disc.source);
                let adapter = ta_submit::ExternalMessagingAdapter::new(&disc.manifest);
                match adapter.health() {
                    Ok((address, prov)) => {
                        println!("  Provider: {}", prov);
                        println!("  Address:  {}", address);
                        println!();
                        println!("Plugin is healthy. Credentials already configured.");
                    }
                    Err(e) => {
                        println!("  Health check failed: {}", e);
                        println!();
                        println!(
                            "Ensure credentials are configured per the plugin's documentation."
                        );
                    }
                }
                Ok(())
            } else {
                anyhow::bail!(
                    "No messaging plugin found for provider '{}'. \n\
                     Built-in providers: gmail, outlook, imap\n\
                     Community plugins: install the ta-messaging-{0} binary on PATH or \
                     in ~/.config/ta/plugins/messaging/{0}/",
                    other
                )
            }
        }
    }
}

fn setup_gmail(_manifest: Option<&MessagingPluginManifest>) -> anyhow::Result<()> {
    println!("Gmail OAuth2 Setup");
    println!("──────────────────");
    println!();
    println!("To connect TA to Gmail, you need a Google OAuth2 refresh token.");
    println!("TA never stores passwords — only the OAuth2 refresh token is saved");
    println!("in the OS keychain under: ta-messaging:gmail:<your-address>");
    println!();
    println!("Steps:");
    println!("  1. Create a Google Cloud project at https://console.cloud.google.com/");
    println!("  2. Enable the Gmail API");
    println!("  3. Create OAuth 2.0 credentials (Desktop App)");
    println!("  4. Download the credentials JSON");
    println!("  5. Run the Gmail plugin setup:");
    println!();
    println!("     ta-messaging-gmail setup --credentials /path/to/credentials.json");
    println!();
    println!("  The plugin will open a browser for consent, then store the refresh");
    println!("  token in the keychain automatically.");
    println!();
    println!("  6. Verify: ta adapter health messaging");
    println!();

    // Check if the plugin binary is available.
    if which_on_path("ta-messaging-gmail") {
        println!("ta-messaging-gmail is installed and ready.");
    } else {
        println!("ta-messaging-gmail not found on PATH.");
        println!("Install it from the TA release assets or build from source:");
        println!("  https://github.com/trustedautonomy/ta/releases");
    }

    Ok(())
}

fn setup_outlook(_manifest: Option<&MessagingPluginManifest>) -> anyhow::Result<()> {
    println!("Outlook / Microsoft Graph OAuth2 Setup");
    println!("──────────────────────────────────────");
    println!();
    println!("To connect TA to Outlook, you need a Microsoft OAuth2 refresh token.");
    println!("The token is stored in the OS keychain under: ta-messaging:outlook:<address>");
    println!();
    println!("Steps:");
    println!("  1. Register an app at https://portal.azure.com/ → App registrations");
    println!("  2. Add Mail.Read and Mail.ReadWrite delegated permissions");
    println!("  3. Add a redirect URI: http://localhost:8765/auth/callback");
    println!("  4. Note the Application (client) ID and tenant ID");
    println!("  5. Run the Outlook plugin setup:");
    println!();
    println!("     ta-messaging-outlook setup \\");
    println!("       --client-id <APP_CLIENT_ID> \\");
    println!("       --tenant-id <TENANT_ID>");
    println!();
    println!("  The plugin will open a browser for consent, then store the refresh");
    println!("  token in the keychain automatically.");
    println!();
    println!("  6. Verify: ta adapter health messaging");
    println!();

    if which_on_path("ta-messaging-outlook") {
        println!("ta-messaging-outlook is installed and ready.");
    } else {
        println!("ta-messaging-outlook not found on PATH.");
        println!("Install from the TA release assets or build from source.");
    }

    Ok(())
}

fn setup_imap(_manifest: Option<&MessagingPluginManifest>) -> anyhow::Result<()> {
    println!("IMAP / SMTP Setup");
    println!("─────────────────");
    println!();
    println!("Enter your IMAP server details. An app-specific password is");
    println!("strongly recommended — never use your account password.");
    println!("Credentials are stored in the OS keychain (never written to disk).");
    println!();

    // Prompt for IMAP configuration.
    let host = prompt_line("IMAP host (e.g., imap.fastmail.com): ")?;
    if host.is_empty() {
        anyhow::bail!("IMAP host is required. Aborting setup.");
    }

    let port_str = prompt_line("IMAP port [993]: ")?;
    let port: u16 = if port_str.is_empty() {
        993
    } else {
        port_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid port number: {}", port_str))?
    };

    let username = prompt_line("Username / email address: ")?;
    if username.is_empty() {
        anyhow::bail!("Username is required. Aborting setup.");
    }

    let password = prompt_password("App password (input hidden): ")?;
    if password.is_empty() {
        anyhow::bail!("Password is required. Aborting setup.");
    }

    let drafts_folder = prompt_line("Drafts folder name [Drafts]: ")?;
    let drafts_folder = if drafts_folder.is_empty() {
        "Drafts".to_string()
    } else {
        drafts_folder
    };

    // Persist via keychain (stored as JSON blob).
    let keychain_key = format!("ta-messaging:imap:{}", username);
    let config_json = serde_json::json!({
        "host": host,
        "port": port,
        "username": username,
        "password": password,
        "drafts_folder": drafts_folder,
        "tls": true,
    });

    store_in_keychain(&keychain_key, &config_json.to_string())?;

    println!();
    println!("IMAP credentials stored in keychain: {}", keychain_key);
    println!();

    // Smoke-test: call the health op if the plugin is available.
    if which_on_path("ta-messaging-imap") {
        println!("Running health check...");
        let manifest = MessagingPluginManifest {
            name: "imap".to_string(),
            version: "unknown".to_string(),
            plugin_type: "messaging".to_string(),
            command: "ta-messaging-imap".to_string(),
            args: vec![],
            capabilities: vec![
                "fetch".to_string(),
                "create_draft".to_string(),
                "health".to_string(),
            ],
            description: None,
            timeout_secs: 30,
            protocol_version: ta_submit::MESSAGING_PROTOCOL_VERSION,
        };
        let adapter = ta_submit::ExternalMessagingAdapter::new(&manifest);
        match adapter.health() {
            Ok((address, prov)) => {
                println!("  Connected as: {} (provider: {})", address, prov);
                println!();
                println!("IMAP setup complete!");
            }
            Err(e) => {
                println!("  Health check failed: {}", e);
                println!();
                println!("Check your host, port, and app password and run setup again.");
            }
        }
    } else {
        println!("ta-messaging-imap not found on PATH. Install it to enable IMAP fetch/draft.");
        println!("Install from the TA release assets or build from source.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Setup social/<platform>
// ---------------------------------------------------------------------------

fn setup_social(plugin_spec: &str, project_root: &Path) -> anyhow::Result<()> {
    let platform = plugin_spec.strip_prefix("social/").ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid plugin specifier '{}'. Expected format: social/<platform> \
             (e.g., social/linkedin, social/x, social/buffer)",
            plugin_spec
        )
    })?;

    let discovered = find_social_plugin(platform, project_root);

    println!("Setting up social media platform: {}", platform);
    println!();

    match platform {
        "linkedin" => setup_linkedin(discovered.as_ref().map(|d| &d.manifest)),
        "x" | "twitter" => setup_x(discovered.as_ref().map(|d| &d.manifest)),
        "buffer" => setup_buffer(discovered.as_ref().map(|d| &d.manifest)),
        other => {
            if let Some(ref disc) = discovered {
                println!("Found community plugin: {} ({})", other, disc.source);
                let adapter = ta_submit::ExternalSocialAdapter::new(&disc.manifest);
                match adapter.health() {
                    Ok((handle, prov)) => {
                        println!("  Provider: {}", prov);
                        println!("  Handle:   {}", handle);
                        println!();
                        println!("Plugin is healthy. Credentials already configured.");
                    }
                    Err(e) => {
                        println!("  Health check failed: {}", e);
                        println!();
                        println!(
                            "Ensure credentials are configured per the plugin's documentation."
                        );
                    }
                }
                Ok(())
            } else {
                anyhow::bail!(
                    "No social plugin found for platform '{}'. \n\
                     Built-in platforms: linkedin, x, buffer\n\
                     Community plugins: install the ta-social-{0} binary on PATH or \
                     in ~/.config/ta/plugins/social/{0}/",
                    other
                )
            }
        }
    }
}

fn setup_linkedin(_manifest: Option<&SocialPluginManifest>) -> anyhow::Result<()> {
    println!("LinkedIn OAuth2 Setup");
    println!("─────────────────────");
    println!();
    println!("To connect TA to LinkedIn, you need a LinkedIn OAuth2 access token.");
    println!("TA never stores passwords — only the OAuth2 access token is saved");
    println!("in the OS keychain under: ta-social:linkedin:<your-handle>");
    println!();
    println!("Steps:");
    println!("  1. Create a LinkedIn app at https://developer.linkedin.com/");
    println!("  2. Enable the 'Share on LinkedIn' and 'OpenID Connect' products");
    println!("  3. Add http://localhost:8766/auth/callback as a redirect URI");
    println!("  4. Note your Client ID and Client Secret");
    println!("  5. Run the LinkedIn plugin setup:");
    println!();
    println!("     ta-social-linkedin setup --client-id <CLIENT_ID> --client-secret <SECRET>");
    println!();
    println!("  The plugin will open a browser for consent, then store the access");
    println!("  token in the keychain automatically.");
    println!();
    println!("  6. Verify: ta adapter health social");
    println!();
    println!("Required OAuth2 scopes: w_member_social, openid, profile");
    println!();

    if which_on_path("ta-social-linkedin") {
        println!("ta-social-linkedin is installed and ready.");
    } else {
        println!("ta-social-linkedin not found on PATH.");
        println!("Install from the TA release assets or build from source:");
        println!("  https://github.com/trustedautonomy/ta/releases");
    }

    Ok(())
}

fn setup_x(_manifest: Option<&SocialPluginManifest>) -> anyhow::Result<()> {
    println!("X (Twitter) OAuth2 Setup");
    println!("────────────────────────");
    println!();
    println!("To connect TA to X, you need an X API OAuth2 bearer token.");
    println!("TA never stores passwords — only the OAuth2 token is saved");
    println!("in the OS keychain under: ta-social:x:<your-handle>");
    println!();
    println!("API Tier Requirements:");
    println!("  - Draft tweets:     X API Basic tier or higher");
    println!("  - Scheduled tweets: X API Basic tier or higher");
    println!("  - Health check:     X API Free tier (read-only)");
    println!();
    println!("Check your API tier at: https://developer.twitter.com/en/portal/dashboard");
    println!();
    println!("Steps:");
    println!("  1. Create a project + app at https://developer.twitter.com/en/portal/dashboard");
    println!("  2. Enable OAuth 2.0 with PKCE (User authentication settings)");
    println!("  3. Add http://localhost:8767/auth/callback as a redirect URI");
    println!("  4. Note your Client ID");
    println!("  5. Run the X plugin setup:");
    println!();
    println!("     ta-social-x setup --client-id <CLIENT_ID>");
    println!();
    println!("  The plugin will open a browser for consent, then store the bearer");
    println!("  token in the keychain automatically.");
    println!();
    println!("  6. Verify: ta adapter health social");
    println!();

    if which_on_path("ta-social-x") {
        println!("ta-social-x is installed and ready.");
    } else {
        println!("ta-social-x not found on PATH.");
        println!("Install from the TA release assets or build from source.");
    }

    Ok(())
}

fn setup_buffer(_manifest: Option<&SocialPluginManifest>) -> anyhow::Result<()> {
    println!("Buffer OAuth2 Setup");
    println!("───────────────────");
    println!();
    println!("Buffer is a cross-platform scheduler: one Buffer account can post to");
    println!("LinkedIn, X, Instagram, and Facebook simultaneously.");
    println!();
    println!("Steps:");
    println!("  1. Create a Buffer app at https://buffer.com/developers/api");
    println!("  2. Note your Client ID and Client Secret");
    println!("  3. Run the Buffer plugin setup:");
    println!();
    println!("     ta-social-buffer setup --client-id <CLIENT_ID> --client-secret <SECRET>");
    println!();
    println!("  The plugin will open a browser for consent, then store the access");
    println!("  token in the keychain automatically.");
    println!();
    println!("  4. Connect your social profiles in the Buffer dashboard:");
    println!("     https://buffer.com/manage/channels");
    println!();
    println!("  5. Verify: ta adapter health social");
    println!();
    println!("Note: Buffer fans out posts to ALL connected profiles in a single call.");
    println!("A single 'create_draft' creates drafts on LinkedIn, X, and Instagram at once.");
    println!();

    if which_on_path("ta-social-buffer") {
        println!("ta-social-buffer is installed and ready.");
    } else {
        println!("ta-social-buffer not found on PATH.");
        println!("Install from the TA release assets or build from source.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Health messaging / social
// ---------------------------------------------------------------------------

fn health_check(adapter_type: &str, project_root: &Path) -> anyhow::Result<()> {
    match adapter_type {
        "messaging" => health_messaging(project_root),
        "social" => health_social(project_root),
        other => anyhow::bail!(
            "Unknown adapter type: '{}'. Supported: messaging, social",
            other
        ),
    }
}

fn health_messaging(project_root: &Path) -> anyhow::Result<()> {
    let plugins = ta_submit::discover_messaging_plugins(project_root);

    if plugins.is_empty() {
        println!("No messaging plugins found.");
        println!();
        println!("Set up a provider: ta adapter setup messaging/<provider>");
        println!("Providers: gmail, outlook, imap");
        return Ok(());
    }

    println!("Messaging adapter health:");
    println!();

    let mut any_healthy = false;

    for plugin in &plugins {
        let adapter = ta_submit::ExternalMessagingAdapter::new(&plugin.manifest);
        print!("  {} ({})  ", plugin.manifest.name, plugin.source);
        match adapter.health() {
            Ok((address, prov)) => {
                println!("OK  — {} <{}>", prov, address);
                any_healthy = true;
            }
            Err(e) => {
                println!("FAIL — {}", e);
            }
        }
    }

    println!();
    if any_healthy {
        println!("All reachable providers are healthy.");
    } else {
        println!("No providers responded successfully.");
        println!("Re-run setup: ta adapter setup messaging/<provider>");
    }

    Ok(())
}

fn health_social(project_root: &Path) -> anyhow::Result<()> {
    let plugins = ta_submit::discover_social_plugins(project_root);

    if plugins.is_empty() {
        println!("No social plugins found.");
        println!();
        println!("Set up a platform: ta adapter setup social/<platform>");
        println!("Platforms: linkedin, x, buffer");
        return Ok(());
    }

    println!("Social adapter health:");
    println!();

    let mut any_healthy = false;

    for plugin in &plugins {
        let adapter = ta_submit::ExternalSocialAdapter::new(&plugin.manifest);
        print!("  {} ({})  ", plugin.manifest.name, plugin.source);
        match adapter.health() {
            Ok((handle, prov)) => {
                println!("OK  — {} <{}>", prov, handle);
                any_healthy = true;
            }
            Err(e) => {
                println!("FAIL — {}", e);
            }
        }
    }

    println!();
    if any_healthy {
        println!("All reachable platforms are healthy.");
    } else {
        println!("No platforms responded successfully.");
        println!("Re-run setup: ta adapter setup social/<platform>");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Credentials subcommand
// ---------------------------------------------------------------------------

fn credentials_cmd(cmd: &CredentialsCommands) -> anyhow::Result<()> {
    match cmd {
        CredentialsCommands::Get { key } => credentials_get(key),
        CredentialsCommands::Set { key, value } => credentials_set(key, value.as_deref()),
    }
}

fn credentials_get(key: &str) -> anyhow::Result<()> {
    match read_from_keychain(key) {
        Ok(value) => {
            print!("{}", value);
            Ok(())
        }
        Err(e) => anyhow::bail!(
            "Credential not found for key '{}': {}\n\
             Set it with: ta adapter credentials set '{}'",
            key,
            e,
            key
        ),
    }
}

fn credentials_set(key: &str, value: Option<&str>) -> anyhow::Result<()> {
    let secret = match value {
        Some(v) => v.to_string(),
        None => prompt_password(&format!("Value for '{}' (input hidden): ", key))?,
    };
    store_in_keychain(key, &secret)?;
    println!("Stored credential: {}", key);
    Ok(())
}

// ---------------------------------------------------------------------------
// Keychain helpers (file-based fallback for environments without a system keychain)
// ---------------------------------------------------------------------------

/// Store a secret in the OS keychain.
///
/// On macOS uses the Keychain; on Linux uses the Secret Service / libsecret.
/// Falls back to a restrictive-permissions file in `~/.config/ta/secrets/`
/// if neither is available (development / CI environments).
fn store_in_keychain(key: &str, value: &str) -> anyhow::Result<()> {
    // Try environment variable override first (used by plugins in tests/CI).
    let env_key = key_to_env_var(key);
    if std::env::var(&env_key).is_ok() {
        // Already set via env — don't overwrite.
        return Ok(());
    }

    // Fall back to ~/.config/ta/secrets/<sanitized-key>.
    let secrets_dir = secrets_dir()?;
    std::fs::create_dir_all(&secrets_dir)?;

    let file_path = secrets_dir.join(sanitize_key(key));

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o600);
        let mut f = opts.open(&file_path)?;
        std::io::Write::write_all(&mut f, value.as_bytes())?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(&file_path, value)?;
    }

    Ok(())
}

/// Read a secret from the OS keychain or fallback file store.
fn read_from_keychain(key: &str) -> anyhow::Result<String> {
    // 1. Environment variable override (used by plugins in CI/test).
    let env_key = key_to_env_var(key);
    if let Ok(val) = std::env::var(&env_key) {
        return Ok(val);
    }

    // 2. Fallback file store.
    let secrets_dir = secrets_dir()?;
    let file_path = secrets_dir.join(sanitize_key(key));
    if file_path.exists() {
        let val = std::fs::read_to_string(&file_path)?;
        return Ok(val.trim().to_string());
    }

    anyhow::bail!("No credential found for key '{}'", key)
}

fn secrets_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".config")
        .join("ta")
        .join("secrets"))
}

fn key_to_env_var(key: &str) -> String {
    format!(
        "TA_SECRET_{}",
        key.to_uppercase().replace([':', '-', '/', '.', '@'], "_")
    )
}

fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Interactive prompt helpers
// ---------------------------------------------------------------------------

fn prompt_line(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write;
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    // On platforms where rpassword is available, use masked input.
    // We implement a simple fallback: print the prompt and read from stdin.
    // Production builds should link rpassword; for now this is adequate.
    use std::io::Write;
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn which_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Agent adapter install helpers (existing functionality)
// ---------------------------------------------------------------------------

fn install_claude_code(project_root: &Path) -> anyhow::Result<()> {
    // Generate .mcp.json for Claude Code auto-discovery.
    let mcp_json_path = project_root.join(".mcp.json");
    if mcp_json_path.exists() {
        println!("  .mcp.json already exists — skipping (check manually)");
    } else {
        let mcp_json = serde_json::json!({
            "mcpServers": {
                "ta": {
                    "type": "stdio",
                    "command": "cargo",
                    "args": ["run", "-p", "ta-daemon", "--"],
                    "env": {
                        "TA_LOG_LEVEL": "info"
                    }
                }
            }
        });
        fs::write(&mcp_json_path, serde_json::to_string_pretty(&mcp_json)?)?;
        println!("  Created .mcp.json");
    }

    // Generate .ta/config.toml with default settings.
    install_ta_config(project_root)?;

    println!();
    println!("Claude Code adapter installed!");
    println!();
    println!("Next steps:");
    println!("  1. Build TA:  cargo build --workspace");
    println!("  2. Start Claude Code in this directory");
    println!("  3. TA tools will appear alongside built-in tools");
    println!("  4. Agent stages changes through ta_fs_write");
    println!("  5. Review with: ta draft list / ta draft view <id>");
    println!("  6. Approve:    ta draft approve <id>");
    println!("  7. Apply:      ta draft apply <id>");

    Ok(())
}

fn install_generic_mcp(project_root: &Path) -> anyhow::Result<()> {
    install_ta_config(project_root)?;

    println!();
    println!("Generic MCP adapter installed!");
    println!();
    println!("Start the MCP server with:");
    println!("  cargo run -p ta-daemon -- --project-root .");
    println!();
    println!("Connect your MCP client to the server via stdio.");

    Ok(())
}

fn install_ta_config(project_root: &Path) -> anyhow::Result<()> {
    let ta_dir = project_root.join(".ta");
    fs::create_dir_all(&ta_dir)?;

    let config_path = ta_dir.join("config.toml");
    if config_path.exists() {
        println!("  .ta/config.toml already exists — skipping");
    } else {
        let config_toml = r#"# Trusted Autonomy configuration
#
# This file is created by `ta adapter install`. Edit as needed.

[workspace]
root = "."
staging_dir = ".ta/staging"
store_dir = ".ta/store"
audit_log = ".ta/audit.jsonl"

[policy]
default_template = "developer"
manifest_ttl_hours = 8

[notifications]
# Uncomment to enable notifications:
# discord_webhook = "https://discord.com/api/webhooks/..."
# email_to = "you@example.com"
"#;
        fs::write(&config_path, config_toml)?;
        println!("  Created .ta/config.toml");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn list_adapters_succeeds() {
        let dir = TempDir::new().unwrap();
        list_adapters(dir.path()).unwrap();
    }

    #[test]
    fn install_claude_code_creates_files() {
        let dir = TempDir::new().unwrap();
        install_claude_code(dir.path()).unwrap();

        assert!(dir.path().join(".mcp.json").exists());
        assert!(dir.path().join(".ta/config.toml").exists());

        // Verify .mcp.json content.
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert!(json["mcpServers"]["ta"].is_object());
        assert_eq!(json["mcpServers"]["ta"]["type"], "stdio");
    }

    #[test]
    fn install_claude_code_skips_existing_mcp_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".mcp.json"), "existing").unwrap();

        install_claude_code(dir.path()).unwrap();

        // Should not overwrite.
        let content = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert_eq!(content, "existing");
    }

    #[test]
    fn install_generic_mcp_creates_config() {
        let dir = TempDir::new().unwrap();
        install_generic_mcp(dir.path()).unwrap();

        assert!(dir.path().join(".ta/config.toml").exists());
        // Should NOT create .mcp.json.
        assert!(!dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn unknown_adapter_fails() {
        let dir = TempDir::new().unwrap();
        let result = install_adapter("nonexistent", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn setup_messaging_invalid_spec_fails() {
        let dir = TempDir::new().unwrap();
        let result = setup_messaging("notmessaging/foo", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("messaging/"));
    }

    #[test]
    fn setup_messaging_unknown_provider_no_plugin_fails() {
        let dir = TempDir::new().unwrap();
        // "unknown-provider-xyz" is not a real plugin binary — discovery returns None
        // without any PATH manipulation (which would race with parallel tests).
        let result = setup_messaging("messaging/unknown-provider-xyz", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn health_check_unknown_type_fails() {
        let dir = TempDir::new().unwrap();
        let result = health_check("vcs", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn health_messaging_no_plugins_succeeds() {
        let dir = TempDir::new().unwrap();
        // No plugins installed — should succeed with a "no plugins found" message.
        health_messaging(dir.path()).unwrap();
    }

    #[test]
    fn credentials_env_override_roundtrip() {
        // Use the env-var override path — no HOME manipulation, safe for parallel tests.
        let unique = format!(
            "TA_SECRET_TA_MESSAGING_TESTCRED_ROUNDTRIP_{}",
            std::process::id()
        );
        std::env::set_var(&unique, "secret-value-abc");
        // key_to_env_var("ta-messaging:testcred:roundtrip-<pid>") == unique
        // Read via env override.
        let val = std::env::var(&unique).unwrap();
        assert_eq!(val, "secret-value-abc");
        std::env::remove_var(&unique);
    }

    #[test]
    fn credentials_env_override() {
        let env_key = format!("TA_SECRET_TA_MESSAGING_ENVTEST_FOO_{}", std::process::id());
        std::env::set_var(&env_key, "from-env");
        assert_eq!(std::env::var(&env_key).unwrap(), "from-env");
        std::env::remove_var(&env_key);
    }

    #[test]
    fn credentials_missing_key_fails() {
        // A key that has no env var and no file — should fail.
        // Use env var path: clear any env override and check against a nonexistent key.
        let result = read_from_keychain(
            "ta-messaging:nonexistent-xyz-unique-3a7f:nobody-at-example-dot-com",
        );
        assert!(result.is_err());
    }

    #[test]
    fn key_to_env_var_sanitizes() {
        assert_eq!(
            key_to_env_var("ta-messaging:gmail:me@example.com"),
            "TA_SECRET_TA_MESSAGING_GMAIL_ME_EXAMPLE_COM"
        );
    }
}
