// token.rs -- Token management CLI for non-interactive approval.

use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use ta_audit::{AuditAction, AuditEvent, AuditLog};
use ta_credentials::daemon_tokens::{self as dt, TokenRecord, TokenScope};
use ta_events::tokens::TokenStore;
use ta_mcp_gateway::GatewayConfig;

/// Which kind of token a `ta token` command works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenKind {
    /// Single-use approval tokens for non-interactive `ta draft approve`.
    Approval,
    /// Long-lived daemon API bearer tokens for service accounts (pollers, relays).
    Daemon,
}

#[derive(Debug, Subcommand)]
pub enum TokenCommands {
    /// Create a new token. Default kind is an approval token; use
    /// `--kind daemon --scope relay|read|write --label <name>` to mint a
    /// daemon API token (the secret is printed once and only its digest is stored).
    Create {
        /// Token kind: approval (default) or daemon.
        #[arg(long, value_enum, default_value = "approval")]
        kind: TokenKind,
        /// Approval: scope such as "draft:approve" (default) or "draft:deny".
        /// Daemon: relay, read or write (required).
        #[arg(long)]
        scope: Option<String>,
        /// Daemon tokens only: a unique name for this token (for list and revoke).
        #[arg(long)]
        label: Option<String>,
        /// Expiration duration (e.g., "24h", "7d", "90m"). Approval default: 24h.
        /// Daemon default: no expiry.
        #[arg(long)]
        expires: Option<String>,
    },
    /// List tokens of one kind (secrets are never shown).
    List {
        /// Token kind: approval (default) or daemon.
        #[arg(long, value_enum, default_value = "approval")]
        kind: TokenKind,
    },
    /// Revoke a daemon token by id or label (takes effect without a daemon restart).
    Revoke {
        /// Token id (from `ta token list --kind daemon`) or label.
        id_or_label: String,
        /// Only daemon tokens can be revoked.
        #[arg(long, value_enum, default_value = "daemon")]
        kind: TokenKind,
    },
    /// Clean up expired approval tokens.
    Cleanup,
}

pub fn execute(cmd: &TokenCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        TokenCommands::Create {
            kind: TokenKind::Daemon,
            scope,
            label,
            expires,
        } => {
            let path = daemon_tokens_path(&config.workspace_root)?;
            let report = daemon_create(
                &path,
                scope.as_deref(),
                label.as_deref(),
                expires.as_deref(),
                Some(&config.audit_log),
            )?;
            print!("{report}");
            Ok(())
        }
        TokenCommands::List {
            kind: TokenKind::Daemon,
        } => {
            let path = daemon_tokens_path(&config.workspace_root)?;
            print!("{}", daemon_list(&path)?);
            Ok(())
        }
        TokenCommands::Revoke { id_or_label, kind } => {
            if *kind != TokenKind::Daemon {
                anyhow::bail!(
                    "approval tokens cannot be revoked by id: they are single-use and expire on their own. \
                     Run `ta token cleanup` to remove expired ones, or use `ta token revoke <id-or-label> --kind daemon` for a daemon token."
                );
            }
            let path = daemon_tokens_path(&config.workspace_root)?;
            let report = daemon_revoke(&path, id_or_label, Some(&config.audit_log))?;
            print!("{report}");
            Ok(())
        }
        other => {
            let tokens_dir = tokens_dir(&config.workspace_root)?;
            let store = TokenStore::new(&tokens_dir);
            match other {
                TokenCommands::Create {
                    scope,
                    expires,
                    label,
                    ..
                } => {
                    if label.is_some() {
                        anyhow::bail!(
                            "--label applies to daemon tokens only. Use `ta token create --kind daemon --scope relay --label <name>`, or drop --label for an approval token."
                        );
                    }
                    create_token(
                        &store,
                        scope.as_deref().unwrap_or("draft:approve"),
                        expires.as_deref().unwrap_or("24h"),
                    )
                }
                TokenCommands::List { .. } => list_tokens(&store),
                TokenCommands::Cleanup => cleanup_tokens(&store),
                TokenCommands::Revoke { .. } => unreachable!("handled above"),
            }
        }
    }
}

/// The daemon token store in the per-user state directory (the file the
/// daemon reads). An old-location `.ta/daemon-tokens.json` is used, with a
/// warning, in the default posture until `ta doctor --fix` moves it.
fn daemon_tokens_path(project_root: &Path) -> anyhow::Result<PathBuf> {
    use ta_credentials::state::{
        resolve_moved_path, MovedItem, Posture, ProjectState, StateLocator,
    };
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(project_root, &locator)?;
    Ok(resolve_moved_path(
        &ps,
        MovedItem::DaemonTokens,
        Posture::resolve(&locator),
    )?)
}

fn audit_daemon_token(audit_log: Option<&Path>, action: AuditAction, rec: &TokenRecord, op: &str) {
    let Some(path) = audit_log else { return };
    let result = AuditLog::open(path).and_then(|mut log| {
        let mut event = AuditEvent::new("ta-token", action)
            .with_target(format!("daemon-token://{}", rec.display_id()))
            .with_metadata(serde_json::json!({
                "operation": op,
                "id": rec.display_id(),
                "label": rec.label,
                "scope": rec.scope.as_str(),
                "expires_at": rec.expires_at.map(|e| e.to_rfc3339()),
            }));
        log.append(&mut event)
    });
    if let Err(e) = result {
        tracing::warn!(path = %path.display(), error = %e, "could not record the daemon token {op} in the audit log");
        eprintln!(
            "[warn] The token was {op}d but the audit log at {} could not be written ({e}). \
             Check the file's permissions.",
            path.display()
        );
    }
}

/// Mint a daemon token and return the report to print. The secret appears in
/// this report only; the store gets the digest.
fn daemon_create(
    path: &Path,
    scope: Option<&str>,
    label: Option<&str>,
    expires: Option<&str>,
    audit_log: Option<&Path>,
) -> anyhow::Result<String> {
    let scope_str = scope.ok_or_else(|| {
        anyhow::anyhow!(
            "--scope is required for daemon tokens. Use `ta token create --kind daemon --scope relay --label <name>` (scopes: relay, read, write)."
        )
    })?;
    let scope = TokenScope::parse_mintable(scope_str).map_err(|e| anyhow::anyhow!(e))?;
    let label = label.map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "--label is required for daemon tokens so the token can be listed and revoked by name. Example: --label wayfinder-poller."
        )
    })?;
    let duration = expires.map(parse_token_duration).transpose()?;

    let mut records = dt::load_file(path).map_err(|e| anyhow::anyhow!(e))?;
    if records
        .iter()
        .any(|r| r.is_active(chrono::Utc::now()) && r.label.as_deref() == Some(label))
    {
        anyhow::bail!(
            "an active daemon token labelled '{label}' already exists in {}. Pick another --label, or revoke it first with `ta token revoke {label} --kind daemon`.",
            path.display()
        );
    }
    let (rec, secret) = dt::mint(scope, Some(label.to_string()), duration);
    records.push(rec.clone());
    dt::save_file(path, &records).map_err(|e| {
        anyhow::anyhow!(
            "could not write the daemon token store {} ({e}). Check that the directory is writable, then retry `ta token create --kind daemon`.",
            path.display()
        )
    })?;
    audit_daemon_token(audit_log, AuditAction::TokenIssued, &rec, "create");

    Ok(format!(
        "Daemon token created:\n  ID:      {}\n  Label:   {}\n  Scope:   {}\n  Expires: {}\n  Store:   {}\n\n  Token:   {}\n\n\
         Copy the token now. It is shown only once; only its SHA-256 digest is stored.\n\
         Use it as: Authorization: Bearer <token>\n",
        rec.display_id(),
        label,
        rec.scope.as_str(),
        rec.expires_at
            .map(|e| e.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "never".into()),
        path.display(),
        secret,
    ))
}

fn daemon_status(r: &TokenRecord, now: chrono::DateTime<chrono::Utc>) -> &'static str {
    if r.is_revoked() {
        "revoked"
    } else if r.is_expired(now) {
        "expired"
    } else {
        "active"
    }
}

fn daemon_list(path: &Path) -> anyhow::Result<String> {
    let records = dt::load_file(path).map_err(|e| anyhow::anyhow!(e))?;
    if records.is_empty() {
        return Ok(format!(
            "No daemon tokens in {}.\nCreate one with: ta token create --kind daemon --scope relay --label <name>\n",
            path.display()
        ));
    }
    let now = chrono::Utc::now();
    let mut out = format!(
        "Daemon tokens ({}) in {}:\n\n",
        records.len(),
        path.display()
    );
    for r in &records {
        out.push_str(&format!(
            "  {:<16} label={:<20} scope={:<6} status={:<8} created={} expires={} digest={}{}\n",
            r.display_id(),
            r.label.as_deref().unwrap_or("-"),
            r.scope.as_str(),
            daemon_status(r, now),
            r.created_at.format("%Y-%m-%d %H:%M UTC"),
            r.expires_at
                .map(|e| e.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "never".into()),
            r.short_digest(),
            if r.is_legacy_plaintext() {
                "  (legacy plaintext entry; replace with a minted token)"
            } else {
                ""
            },
        ));
    }
    Ok(out)
}

fn daemon_revoke(path: &Path, key: &str, audit_log: Option<&Path>) -> anyhow::Result<String> {
    let mut records = dt::load_file(path).map_err(|e| anyhow::anyhow!(e))?;
    let matches: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.display_id() == key || r.label.as_deref() == Some(key))
        .map(|(i, _)| i)
        .collect();
    let open: Vec<usize> = matches
        .iter()
        .copied()
        .filter(|&i| !records[i].is_revoked())
        .collect();
    if matches.is_empty() {
        anyhow::bail!(
            "no daemon token with id or label '{key}' in {}. Run `ta token list --kind daemon` to see the available tokens.",
            path.display()
        );
    }
    if open.is_empty() {
        return Ok(format!(
            "Daemon token '{key}' was already revoked. Nothing changed.\n"
        ));
    }
    if open.len() > 1 {
        anyhow::bail!(
            "'{key}' matches {} unrevoked daemon tokens. Revoke by id instead (see `ta token list --kind daemon`).",
            open.len()
        );
    }
    let i = open[0];
    records[i].revoked_at = Some(chrono::Utc::now());
    dt::save_file(path, &records).map_err(|e| {
        anyhow::anyhow!(
            "could not write the daemon token store {} ({e}). Check that the directory is writable, then retry the revoke.",
            path.display()
        )
    })?;
    let rec = records[i].clone();
    audit_daemon_token(audit_log, AuditAction::TokenRevoked, &rec, "revoke");
    Ok(format!(
        "Revoked daemon token {} (label {}, scope {}). The daemon rejects it from its next request; no restart needed.\n",
        rec.display_id(),
        rec.label.as_deref().unwrap_or("-"),
        rec.scope.as_str()
    ))
}

/// Approval tokens live in the per-user TA state directory, not the project
/// tree, so an agent cannot read them with `../../`. An old-location
/// `.ta/tokens` is still used (with a warning) in the default posture until
/// `ta doctor --fix` moves it.
fn tokens_dir(project_root: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use ta_credentials::state::{
        resolve_moved_path, MovedItem, Posture, ProjectState, StateLocator,
    };
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(project_root, &locator)?;
    Ok(resolve_moved_path(
        &ps,
        MovedItem::Tokens,
        Posture::resolve(&locator),
    )?)
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
    if n <= 0 {
        anyhow::bail!(
            "duration '{}' must be positive (for example 30d, 12h, 90m)",
            s
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn store_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("state").join("daemon-tokens.json")
    }

    fn secret_in(report: &str) -> String {
        report
            .lines()
            .find_map(|l| l.trim().strip_prefix("Token:"))
            .map(|v| v.trim().to_string())
            .expect("report shows the token")
    }

    #[test]
    fn create_each_scope_stores_digest_not_secret() {
        for scope in ["relay", "read", "write"] {
            let dir = tempfile::tempdir().unwrap();
            let path = store_path(&dir);
            let report =
                daemon_create(&path, Some(scope), Some("poller"), Some("30d"), None).unwrap();
            let secret = secret_in(&report);
            assert!(secret.starts_with("ta_"));
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(
                !raw.contains(&secret),
                "secret leaked to the store ({scope})"
            );
            assert!(raw.contains(&dt::digest_of(&secret)));
            let recs = dt::load_file(&path).unwrap();
            assert_eq!(recs[0].scope.as_str(), scope);
            assert!(recs[0].expires_at.is_some());
            assert!(dt::verify(&recs, &secret, chrono::Utc::now()).is_some());
        }
    }

    #[test]
    fn secret_is_shown_once_and_never_by_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(&dir);
        let report = daemon_create(&path, Some("relay"), Some("p"), None, None).unwrap();
        let secret = secret_in(&report);
        for _ in 0..2 {
            let list = daemon_list(&path).unwrap();
            assert!(!list.contains(&secret));
            assert!(list.contains("label=p"));
            assert!(list.contains("scope=relay"));
            assert!(list.contains("status=active"));
            assert!(list.contains(&dt::digest_of(&secret)[..12]));
        }
    }

    #[test]
    fn create_validates_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(&dir);
        assert!(daemon_create(&path, None, Some("a"), None, None).is_err());
        assert!(daemon_create(&path, Some("admin"), Some("a"), None, None).is_err());
        assert!(daemon_create(&path, Some("read"), None, None, None).is_err());
        assert!(daemon_create(&path, Some("read"), Some("a"), Some("0d"), None).is_err());
        assert!(daemon_create(&path, Some("read"), Some("a"), Some("5x"), None).is_err());
        daemon_create(&path, Some("read"), Some("a"), Some("90m"), None).unwrap();
        daemon_create(&path, Some("read"), Some("b"), Some("12h"), None).unwrap();
        assert!(daemon_create(&path, Some("read"), Some("a"), None, None).is_err());
    }

    #[test]
    fn revoke_by_label_and_id_rejects_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(&dir);
        let secret =
            secret_in(&daemon_create(&path, Some("write"), Some("w"), None, None).unwrap());
        let id = dt::load_file(&path).unwrap()[0].display_id();
        assert!(daemon_revoke(&path, "nope", None).is_err());
        daemon_revoke(&path, "w", None).unwrap();
        let recs = dt::load_file(&path).unwrap();
        assert!(dt::verify(&recs, &secret, chrono::Utc::now()).is_none());
        assert!(daemon_list(&path).unwrap().contains("status=revoked"));
        assert!(daemon_revoke(&path, &id, None)
            .unwrap()
            .contains("already revoked"));
    }

    #[test]
    fn create_and_revoke_are_audited_without_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(&dir);
        let audit = dir.path().join("audit.jsonl");
        let secret =
            secret_in(&daemon_create(&path, Some("relay"), Some("p"), None, Some(&audit)).unwrap());
        daemon_revoke(&path, "p", Some(&audit)).unwrap();
        let log = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("\"label\":\"p\""));
        assert!(log.contains("\"scope\":\"relay\""));
        assert!(!log.contains(&secret));
    }

    #[test]
    fn legacy_plaintext_entry_is_preserved_and_listed() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"[{"token":"ta_old_plain","scope":"read","created_at":"2026-01-01T00:00:00Z","label":"old"}]"#,
        )
        .unwrap();
        daemon_create(&path, Some("relay"), Some("new"), None, None).unwrap();
        let list = daemon_list(&path).unwrap();
        assert!(list.contains("legacy plaintext"));
        assert!(!list.contains("ta_old_plain"));
        let recs = dt::load_file(&path).unwrap();
        assert!(dt::verify(&recs, "ta_old_plain", chrono::Utc::now()).is_some());
    }

    #[test]
    fn approval_create_and_list_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("tokens"));
        create_token(&store, "draft:approve", "24h").unwrap();
        let toks = store.list().unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].scope, "draft:approve");
        assert!(parse_token_duration("7d").is_ok());
    }

    #[test]
    fn cli_parses_default_kind_as_approval() {
        use clap::Parser;
        #[derive(Parser)]
        struct W {
            #[command(subcommand)]
            c: TokenCommands,
        }
        match W::try_parse_from(["w", "create"]).unwrap().c {
            TokenCommands::Create { kind, scope, .. } => {
                assert_eq!(kind, TokenKind::Approval);
                assert!(scope.is_none());
            }
            _ => panic!("expected create"),
        }
        match W::try_parse_from([
            "w", "create", "--kind", "daemon", "--scope", "relay", "--label", "x",
        ])
        .unwrap()
        .c
        {
            TokenCommands::Create { kind, .. } => assert_eq!(kind, TokenKind::Daemon),
            _ => panic!("expected create"),
        }
    }
}
