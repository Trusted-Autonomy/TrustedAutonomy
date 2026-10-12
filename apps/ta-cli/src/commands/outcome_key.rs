//! `ta show outcome-key` and `ta outcome-key rotate|promote|revoke`: the
//! Ed25519 key the daemon signs whiteboard outcome envelopes with (red-team
//! CR-04). The private half stays in the daemon-only state directory; this
//! command only ever prints the public registry path and fingerprints.
//! Contract: docs/superpowers/specs/2026-10-11-outcome-envelope-contract.md.

use std::path::Path;

use clap::Subcommand;
use ta_audit::{AuditAction, AuditEvent, AuditLog};
use ta_credentials::outcome_keys::{KeyStatus, OutcomeKeyStore};
use ta_credentials::state::{ProjectState, StateLocator};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum OutcomeKeyCommands {
    /// Publish a new key id (pending). The old key keeps signing until you
    /// promote, so pollers can load the new key first.
    Rotate,
    /// Make the pending key the signing key and retire the previous one.
    Promote,
    /// Revoke a key id: envelopes it signed stop verifying immediately.
    Revoke {
        /// Key id from `ta show outcome-key`.
        kid: String,
    },
}

fn open(project_root: &Path) -> anyhow::Result<(OutcomeKeyStore, ProjectState)> {
    let ps = ProjectState::open(project_root, &StateLocator::from_env()).map_err(|e| {
        anyhow::anyhow!(
            "cannot open the TA state directory for {} ({e}). Run `ta show state` to see where it is."
            , project_root.display()
        )
    })?;
    Ok((OutcomeKeyStore::open(&ps), ps))
}

fn audit(config: &GatewayConfig, action: &str, kid: &str) {
    let result = AuditLog::open(&config.audit_log).and_then(|mut log| {
        let mut ev = AuditEvent::new("ta-outcome-key", AuditAction::OutcomeKeyChanged)
            .with_target(format!("outcome-key://{kid}"))
            .with_metadata(serde_json::json!({ "action": action, "kid": kid }));
        log.append(&mut ev)
    });
    if let Err(e) = result {
        eprintln!(
            "[warn] The key change succeeded but could not be written to the audit log at {} ({e}). Check the file's permissions.",
            config.audit_log.display()
        );
    }
}

/// `ta show outcome-key`: where the poller reads the public keys, and what
/// each key's fingerprint is.
pub fn show(project_root: &Path) -> anyhow::Result<()> {
    let (store, ps) = open(project_root)?;
    let now = chrono::Utc::now().timestamp();
    let reg = store
        .ensure_active(now)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Outcome signing keys (Ed25519, held by the daemon only)");
    println!(
        "  Public registry:  {}   (the file a poller reads; contains no secrets)",
        OutcomeKeyStore::registry_path_for(&ps).display()
    );
    println!(
        "  Private keys:     {}   (daemon only, never share)",
        ps.secrets_dir().display()
    );
    for k in &reg.keys {
        println!(
            "  {:<8} {}  {}  created {}",
            format!("{:?}", k.status).to_lowercase(),
            k.kid,
            k.fingerprint(),
            k.created_at
        );
    }
    if let Some(active) = reg.active() {
        println!(
            "  Signing with:     {} ({})",
            active.kid,
            active.fingerprint()
        );
    }
    println!("  Rotate: `ta outcome-key rotate`, wait for pollers to reload, then `ta outcome-key promote`.");
    Ok(())
}

pub fn execute(cmd: &OutcomeKeyCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let (store, _ps) = open(&config.workspace_root)?;
    let now = chrono::Utc::now().timestamp();
    match cmd {
        OutcomeKeyCommands::Rotate => {
            let k = store
                .stage_rotation(now)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            audit(config, "stage", &k.kid);
            println!("Staged new outcome key {} ({}).", k.kid, k.fingerprint());
            println!(
                "It is published in {} but not signing yet.",
                store.registry_path().display()
            );
            println!("Once pollers have reloaded the registry, run `ta outcome-key promote`.");
        }
        OutcomeKeyCommands::Promote => {
            let k = store.promote(now).map_err(|e| anyhow::anyhow!("{e}"))?;
            audit(config, "promote", &k.kid);
            println!("Outcome key {} is now signing. The previous key is retired (still verifies older envelopes).", k.kid);
        }
        OutcomeKeyCommands::Revoke { kid } => {
            let reg = store.revoke(kid, now).map_err(|e| anyhow::anyhow!("{e}"))?;
            audit(config, "revoke", kid);
            println!("Revoked outcome key {kid}. Envelopes it signed no longer verify.");
            if let Some(a) = reg.active() {
                println!("Signing now uses {} ({}).", a.kid, a.fingerprint());
            }
            debug_assert!(reg.get(kid).is_some_and(|k| k.status == KeyStatus::Revoked));
        }
    }
    Ok(())
}
