// audit.rs — Audit subcommands: verify, tail, show, export, drift, baseline,
//             verify-attestation (v0.14.1), ledger (v0.14.6).

use clap::Subcommand;
use ta_audit::{
    AttestationBackend, AuditDisposition, AuditEvent, AuditLog, BaselineStore, DraftSummary,
    DriftSeverity, GoalAuditLedger, LedgerFilter, SoftwareAttestationBackend,
};
use ta_goal::{MessagingAuditLog, SocialAuditLog};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum AuditCommands {
    /// Verify the audit log hash chain integrity.
    Verify {
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
    },
    /// Show recent audit events.
    Tail {
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
        /// Number of events to show.
        #[arg(short, default_value = "10")]
        n: usize,
    },
    /// Display the decision trail for a goal with reasoning (v0.3.3).
    Show {
        /// Goal ID to display decision trail for.
        goal_id: String,
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
    },
    /// Export structured audit data for compliance reporting (v0.3.3 / v0.14.8.2).
    ///
    /// Export a goal's audit trail, or a governed workflow run's stage audit trail.
    ///
    /// Examples:
    ///   ta audit export <goal-id>
    ///   ta audit export --workflow-run <run-id>
    Export {
        /// Goal ID to export. Required unless --workflow-run is specified.
        goal_id: Option<String>,
        /// Export the audit trail for a governed workflow run (v0.14.8.2).
        /// Use the full run ID or an 8-char prefix.
        #[arg(long)]
        workflow_run: Option<String>,
        /// Output format.
        #[arg(long, default_value = "json")]
        format: ExportFormat,
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
    },
    /// Show behavioral drift report for an agent (v0.4.2).
    Drift {
        /// Agent ID to check (omit with --all for all agents).
        agent_id: Option<String>,
        /// Show drift summary for all agents with stored baselines.
        #[arg(long)]
        all: bool,
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
        /// Number of recent goals to compare against baseline.
        #[arg(long, default_value = "5")]
        window: usize,
    },
    /// Compute and store a behavioral baseline for an agent (v0.4.2).
    Baseline {
        /// Agent ID to compute baseline for.
        agent_id: String,
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
    },
    /// Verify cryptographic attestation signatures on audit events (v0.14.1).
    ///
    /// For each event that carries an attestation record, re-derives the
    /// canonical payload and verifies the Ed25519 signature.  Events without
    /// attestation are shown as "unsigned".
    VerifyAttestation {
        /// Path to audit log (defaults to .ta/audit.jsonl).
        #[arg(long)]
        log: Option<String>,
        /// Verify only the event with this ID (UUID).
        event_id: Option<String>,
        /// Path to keys directory (defaults to .ta/keys).
        #[arg(long)]
        keys: Option<String>,
    },
    /// Goal-level audit ledger operations (v0.14.6).
    Ledger {
        #[command(subcommand)]
        command: LedgerCommands,
    },
    /// Show the messaging draft audit log (v0.15.9).
    ///
    /// Prints all `DraftEmailRecord` entries from `.ta/messaging-audit.jsonl`.
    /// Each entry shows the draft ID, provider, recipient, subject, creation
    /// time, and current state (drafted / sent / discarded).
    ///
    /// Example:
    ///   ta audit messaging
    Messaging {
        /// Path to messaging audit log (defaults to .ta/messaging-audit.jsonl).
        #[arg(long)]
        log: Option<String>,
        /// Filter by provider (e.g., "gmail", "outlook", "imap").
        #[arg(long)]
        provider: Option<String>,
        /// Filter by draft state (drafted, sent, discarded, unknown).
        #[arg(long)]
        state: Option<String>,
        /// Number of most-recent records to show (0 = all).
        #[arg(short = 'n', long, default_value = "0")]
        limit: usize,
    },
    /// Show the social media draft audit log (v0.15.12).
    ///
    /// Prints all `DraftSocialRecord` entries from `.ta/social-audit.jsonl`.
    /// Each entry shows the post ID, platform, handle, body preview, creation
    /// time, current state (draft / published / deleted), and supervisor score.
    ///
    /// Example:
    ///   ta audit social
    ///   ta audit social --platform linkedin
    ///   ta audit social --state draft -n 10
    Social {
        /// Path to social audit log (defaults to .ta/social-audit.jsonl).
        #[arg(long)]
        log: Option<String>,
        /// Filter by platform (e.g., "linkedin", "x", "buffer").
        #[arg(long)]
        platform: Option<String>,
        /// Filter by state (draft, published, deleted, unknown).
        #[arg(long)]
        state: Option<String>,
        /// Number of most-recent records to show (0 = all).
        #[arg(short = 'n', long, default_value = "0")]
        limit: usize,
    },
}

/// Subcommands for the goal audit ledger.
#[derive(Subcommand)]
pub enum LedgerCommands {
    /// Export the goal audit ledger as JSONL or CSV.
    Export {
        /// Output format: jsonl or csv.
        #[arg(long, default_value = "jsonl")]
        format: LedgerExportFormat,
        /// Filter by disposition (applied, denied, cancelled, abandoned, gc, timeout, crashed, closed).
        #[arg(long)]
        disposition: Option<String>,
        /// Filter by plan phase.
        #[arg(long)]
        phase: Option<String>,
        /// Filter by agent.
        #[arg(long)]
        agent: Option<String>,
        /// Filter entries recorded after this date (ISO 8601, e.g. 2026-01-01).
        #[arg(long)]
        since: Option<String>,
        /// Filter entries recorded before this date (ISO 8601).
        #[arg(long)]
        until: Option<String>,
        /// Path to ledger (defaults to .ta/goal-audit.jsonl).
        #[arg(long)]
        ledger: Option<String>,
    },
    /// Verify the goal audit ledger hash chain integrity.
    Verify {
        /// Path to ledger (defaults to .ta/goal-audit.jsonl).
        #[arg(long)]
        ledger: Option<String>,
    },
    /// Remove entries older than a retention period while preserving chain integrity.
    Gc {
        /// Remove entries older than this duration (e.g. "1y", "6m", "90d").
        #[arg(long, default_value = "1y")]
        older_than: String,
        /// Show what would be removed without making changes.
        #[arg(long)]
        dry_run: bool,
        /// Path to ledger (defaults to .ta/goal-audit.jsonl).
        #[arg(long)]
        ledger: Option<String>,
    },
    /// Migrate .ta/goal-history.jsonl entries to the goal audit ledger.
    Migrate {
        /// Path to source history file (defaults to .ta/goal-history.jsonl).
        #[arg(long)]
        history: Option<String>,
        /// Path to ledger (defaults to .ta/goal-audit.jsonl).
        #[arg(long)]
        ledger: Option<String>,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum LedgerExportFormat {
    Jsonl,
    Csv,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ExportFormat {
    Json,
}

pub fn execute(cmd: &AuditCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        AuditCommands::Verify { log } => {
            let path = log
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| config.audit_log.clone());

            if !path.exists() {
                println!("No audit log found at {}", path.display());
                return Ok(());
            }

            // Verify using the real hash-chain verification (recomputes hashes).
            match AuditLog::verify_chain(&path) {
                Ok(_) => {
                    let events = AuditLog::read_all(&path)?;
                    println!(
                        "Audit log verified: {} event(s), hash chain intact.",
                        events.len()
                    );
                }
                Err(ta_audit::AuditError::IntegrityViolation {
                    line,
                    expected,
                    actual,
                }) => {
                    println!("INTEGRITY VIOLATION at line {}:", line);
                    println!("  Expected previous_hash: {}", expected);
                    println!("  Actual previous_hash:   {}", actual);
                    println!();
                    println!("The audit log may have been tampered with.");
                    anyhow::bail!("Audit log integrity check failed");
                }
                Err(e) => return Err(e.into()),
            }
        }

        AuditCommands::Tail { log, n } => {
            let path = log
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| config.audit_log.clone());

            if !path.exists() {
                println!("No audit log found at {}", path.display());
                return Ok(());
            }

            let events = AuditLog::read_all(&path)?;
            let start = events.len().saturating_sub(*n);
            let recent = &events[start..];

            if recent.is_empty() {
                println!("No audit events.");
                return Ok(());
            }

            println!(
                "{:<26} {:<12} {:<14} TARGET",
                "TIMESTAMP", "AGENT", "ACTION"
            );
            println!("{}", "-".repeat(80));

            for event in recent {
                println!(
                    "{:<26} {:<12} {:<14} {}",
                    event.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    event.agent_id,
                    format!("{:?}", event.action),
                    event.target_uri.as_deref().unwrap_or("-"),
                );
            }
        }

        AuditCommands::Show { goal_id, log } => {
            show_decision_trail(config, goal_id, log.as_deref())?;
        }

        AuditCommands::Export {
            goal_id,
            workflow_run,
            format,
            log,
        } => {
            if let Some(run_id) = workflow_run {
                let runs_dir = config.workspace_root.join(".ta").join("workflow-runs");
                super::governed_workflow::export_run_audit(&runs_dir, run_id)?;
            } else {
                let gid = goal_id.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Provide a goal ID or use --workflow-run <run-id>\n\
                         Examples:\n  \
                           ta audit export <goal-id>\n  \
                           ta audit export --workflow-run <run-id>"
                    )
                })?;
                export_audit(config, gid, format, log.as_deref())?;
            }
        }

        AuditCommands::Drift {
            agent_id,
            all,
            log,
            window,
        } => {
            execute_drift(config, agent_id.as_deref(), *all, log.as_deref(), *window)?;
        }

        AuditCommands::Baseline { agent_id, log } => {
            execute_baseline(config, agent_id, log.as_deref())?;
        }

        AuditCommands::VerifyAttestation {
            log,
            event_id,
            keys,
        } => {
            execute_verify_attestation(
                config,
                log.as_deref(),
                event_id.as_deref(),
                keys.as_deref(),
            )?;
        }

        AuditCommands::Ledger { command } => {
            execute_ledger(command, config)?;
        }

        AuditCommands::Messaging {
            log,
            provider,
            state,
            limit,
        } => {
            messaging_audit(
                config,
                log.as_deref(),
                provider.as_deref(),
                state.as_deref(),
                *limit,
            )?;
        }

        AuditCommands::Social {
            log,
            platform,
            state,
            limit,
        } => {
            social_audit(
                config,
                log.as_deref(),
                platform.as_deref(),
                state.as_deref(),
                *limit,
            )?;
        }
    }

    Ok(())
}

// ── Ledger subcommand (v0.14.6) ──

fn execute_ledger(cmd: &LedgerCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        LedgerCommands::Export {
            format,
            disposition,
            phase,
            agent,
            since,
            until,
            ledger,
        } => {
            ledger_export(
                config,
                format,
                LedgerExportParams {
                    disposition: disposition.as_deref(),
                    phase: phase.as_deref(),
                    agent: agent.as_deref(),
                    since: since.as_deref(),
                    until: until.as_deref(),
                    override_path: ledger.as_deref(),
                },
            )?;
        }

        LedgerCommands::Verify { ledger } => {
            ledger_verify(config, ledger.as_deref())?;
        }

        LedgerCommands::Gc {
            older_than,
            dry_run,
            ledger,
        } => {
            ledger_gc(config, older_than, *dry_run, ledger.as_deref())?;
        }

        LedgerCommands::Migrate { history, ledger } => {
            ledger_migrate(config, history.as_deref(), ledger.as_deref())?;
        }
    }
    Ok(())
}

fn ledger_path(config: &GatewayConfig, override_path: Option<&str>) -> std::path::PathBuf {
    override_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| GoalAuditLedger::path_for(&config.workspace_root))
}

struct LedgerExportParams<'a> {
    disposition: Option<&'a str>,
    phase: Option<&'a str>,
    agent: Option<&'a str>,
    since: Option<&'a str>,
    until: Option<&'a str>,
    override_path: Option<&'a str>,
}

fn ledger_export(
    config: &GatewayConfig,
    format: &LedgerExportFormat,
    params: LedgerExportParams<'_>,
) -> anyhow::Result<()> {
    let LedgerExportParams {
        disposition,
        phase,
        agent,
        since,
        until,
        override_path,
    } = params;
    let path = ledger_path(config, override_path);

    if !path.exists() {
        println!("No goal audit ledger found at {}", path.display());
        println!("Ledger is populated as goals reach terminal states.");
        return Ok(());
    }

    let all = GoalAuditLedger::read_all(&path)?;

    let parsed_disp: Option<AuditDisposition> = disposition
        .map(|s| {
            s.parse::<AuditDisposition>()
                .map_err(|e| anyhow::anyhow!("Invalid disposition '{}': {}", s, e))
        })
        .transpose()?;

    let parsed_since = since
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
                .or_else(|_| s.parse::<chrono::DateTime<chrono::Utc>>())
                .map_err(|e| anyhow::anyhow!("Invalid --since date '{}': {}", s, e))
        })
        .transpose()?;

    let parsed_until = until
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(23, 59, 59).unwrap().and_utc())
                .or_else(|_| s.parse::<chrono::DateTime<chrono::Utc>>())
                .map_err(|e| anyhow::anyhow!("Invalid --until date '{}': {}", s, e))
        })
        .transpose()?;

    let filter = LedgerFilter {
        since: parsed_since,
        until: parsed_until,
        phase: phase.map(|s| s.to_string()),
        agent: agent.map(|s| s.to_string()),
        disposition: parsed_disp,
    };

    let entries: Vec<_> = all.iter().filter(|e| filter.matches(e)).collect();

    match format {
        LedgerExportFormat::Jsonl => {
            for entry in &entries {
                println!("{}", serde_json::to_string(entry)?);
            }
        }
        LedgerExportFormat::Csv => {
            println!(
                "goal_id,title,disposition,phase,agent,created_at,recorded_at,build_seconds,review_seconds,total_seconds,draft_id,artifact_count,lines_changed,denial_reason"
            );
            for entry in &entries {
                println!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                    entry.goal_id,
                    csv_escape(&entry.title),
                    entry.disposition,
                    entry.phase.as_deref().unwrap_or(""),
                    entry.agent,
                    entry.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
                    entry.recorded_at.format("%Y-%m-%dT%H:%M:%SZ"),
                    entry.build_seconds,
                    entry.review_seconds,
                    entry.total_seconds,
                    entry.draft_id.map(|id| id.to_string()).unwrap_or_default(),
                    entry.artifact_count,
                    entry.lines_changed,
                    csv_escape(entry.denial_reason.as_deref().unwrap_or("")),
                );
            }
        }
    }

    eprintln!("{} entries exported.", entries.len());
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn ledger_verify(config: &GatewayConfig, override_path: Option<&str>) -> anyhow::Result<()> {
    use ta_audit::{verify_hmac_chain, AuditHmacKey};

    let path = ledger_path(config, override_path);

    if !path.exists() {
        println!("No goal audit ledger found at {}", path.display());
        return Ok(());
    }

    // Load HMAC key if present (high-security projects).
    let hmac_key_path = AuditHmacKey::path_for(&config.workspace_root);
    let hmac_key = if hmac_key_path.exists() {
        match AuditHmacKey::load(&hmac_key_path) {
            Ok(k) => {
                println!("HMAC key found — verifying signed hash chain.");
                Some(k)
            }
            Err(e) => {
                eprintln!(
                    "[warn] Failed to load audit.key: {} — skipping HMAC verification.",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Use the extended chain verifier which also checks HMAC sigs.
    let results = verify_hmac_chain(&path, hmac_key.as_ref())?;

    let mut hmac_failures = 0usize;
    let mut hash_failures = 0usize;

    for r in &results {
        if !r.hash_ok {
            hash_failures += 1;
            println!(
                "  HASH CHAIN BROKEN at line {} (goal {}): previous_hash mismatch",
                r.line, r.goal_id
            );
        }
        if let Some(false) = r.hmac_ok {
            hmac_failures += 1;
            println!(
                "  HMAC INVALID at line {} (goal {}): signature verification failed",
                r.line, r.goal_id
            );
        }
        if let Some(ref err) = r.error {
            println!("  PARSE ERROR at line {}: {}", r.line, err);
        }
    }

    if hash_failures > 0 || hmac_failures > 0 {
        println!();
        println!(
            "INTEGRITY VIOLATION: {} hash failure(s), {} HMAC failure(s).",
            hash_failures, hmac_failures
        );
        println!("The goal audit ledger may have been tampered with.");
        anyhow::bail!("Goal audit ledger integrity check failed");
    }

    let hmac_str = if hmac_key.is_some() {
        format!(
            " + {} HMAC signature(s) verified",
            results.iter().filter(|r| r.hmac_ok == Some(true)).count()
        )
    } else {
        String::new()
    };

    println!(
        "Goal audit ledger verified: {} entries, hash chain intact{}.",
        results.len(),
        hmac_str,
    );

    Ok(())
}

fn parse_retention_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    if let Some(years) = s.strip_suffix('y') {
        let n: i64 = years
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(chrono::Duration::days(n * 365))
    } else if let Some(months) = s.strip_suffix('m') {
        let n: i64 = months
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(chrono::Duration::days(n * 30))
    } else if let Some(days) = s.strip_suffix('d') {
        let n: i64 = days
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(chrono::Duration::days(n))
    } else {
        anyhow::bail!("Duration must end with 'y' (years), 'm' (months), or 'd' (days). Examples: 1y, 6m, 90d")
    }
}

fn ledger_gc(
    config: &GatewayConfig,
    older_than: &str,
    dry_run: bool,
    override_path: Option<&str>,
) -> anyhow::Result<()> {
    let path = ledger_path(config, override_path);

    if !path.exists() {
        println!("No goal audit ledger found at {}", path.display());
        return Ok(());
    }

    let retention = parse_retention_duration(older_than)?;
    let cutoff = chrono::Utc::now() - retention;

    let all = GoalAuditLedger::read_all(&path)?;
    let (retain, remove): (Vec<_>, Vec<_>) = all.into_iter().partition(|e| e.recorded_at >= cutoff);

    if remove.is_empty() {
        println!("No entries older than {}. Nothing to remove.", older_than);
        return Ok(());
    }

    if dry_run {
        println!(
            "[dry-run] Would remove {} entries (recorded before {}).",
            remove.len(),
            cutoff.format("%Y-%m-%d")
        );
        println!("[dry-run] Would retain {} entries.", retain.len());
        return Ok(());
    }

    // Rewrite ledger with only retained entries (re-chains hashes).
    // Write to a temp file first, then rename atomically.
    let tmp_path = path.with_extension("jsonl.tmp");
    {
        let mut ledger = GoalAuditLedger::open(&tmp_path)?;
        for mut entry in retain.clone() {
            entry.previous_hash = None; // will be set by append
            ledger.append(&mut entry)?;
        }
    }
    std::fs::rename(&tmp_path, &path)?;

    println!(
        "Removed {} entries older than {} (before {}).",
        remove.len(),
        older_than,
        cutoff.format("%Y-%m-%d")
    );
    println!("Retained {} entries. Chain re-anchored.", retain.len());
    Ok(())
}

fn ledger_migrate(
    config: &GatewayConfig,
    history_override: Option<&str>,
    ledger_override: Option<&str>,
) -> anyhow::Result<()> {
    let history_path = history_override
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.workspace_root.join(".ta").join("goal-history.jsonl"));

    let ledger_path = ledger_path(config, ledger_override);

    if !history_path.exists() {
        println!(
            "No goal history file found at {}. Nothing to migrate.",
            history_path.display()
        );
        return Ok(());
    }

    // Load existing ledger IDs to skip already-migrated entries.
    let existing_ids: std::collections::HashSet<uuid::Uuid> = if ledger_path.exists() {
        GoalAuditLedger::read_all(&ledger_path)?
            .into_iter()
            .map(|e| e.goal_id)
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut ledger = GoalAuditLedger::open(&ledger_path)?;
    let count = ta_audit::migrate_from_history(&history_path, &mut ledger, &existing_ids)?;

    if count == 0 {
        println!("All entries already migrated (or history file is empty).");
    } else {
        println!(
            "Migrated {} entries from {} to {}.",
            count,
            history_path.display(),
            ledger_path.display()
        );
    }

    Ok(())
}

// ── Drift subcommand (v0.4.2) ──

fn baselines_dir(config: &GatewayConfig) -> std::path::PathBuf {
    config.workspace_root.join(".ta").join("baselines")
}

/// Load draft package summaries from the pr_packages directory.
fn load_draft_summaries(config: &GatewayConfig) -> anyhow::Result<Vec<DraftSummary>> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut summaries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(pkg) = serde_json::from_str::<ta_changeset::DraftPackage>(&data) {
                    let rejected = matches!(pkg.status, ta_changeset::DraftStatus::Denied { .. });
                    let dep_count = pkg
                        .changes
                        .artifacts
                        .iter()
                        .filter(|a| ta_audit::drift::is_dependency_file(&a.resource_uri))
                        .count();
                    summaries.push(DraftSummary {
                        agent_id: pkg.agent_identity.agent_id,
                        artifact_count: pkg.changes.artifacts.len(),
                        risk_score: pkg.risk.risk_score,
                        rejected,
                        dependency_artifact_count: dep_count,
                    });
                }
            }
        }
    }
    Ok(summaries)
}

fn execute_drift(
    config: &GatewayConfig,
    agent_id: Option<&str>,
    all: bool,
    log_path: Option<&str>,
    window: usize,
) -> anyhow::Result<()> {
    let store = BaselineStore::new(baselines_dir(config));

    // Determine which agents to report on.
    let agents: Vec<String> = if all {
        store.list_agents()?
    } else if let Some(id) = agent_id {
        vec![id.to_string()]
    } else {
        anyhow::bail!("Provide an agent ID or use --all");
    };

    if agents.is_empty() {
        println!("No baselines found. Run `ta audit baseline <agent-id>` first.");
        return Ok(());
    }

    // Load audit events.
    let audit_path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.audit_log.clone());
    let events = if audit_path.exists() {
        AuditLog::read_all(&audit_path)?
    } else {
        Vec::new()
    };

    // Load draft summaries.
    let drafts = load_draft_summaries(config)?;

    for agent in &agents {
        let baseline = match store.load(agent)? {
            Some(b) => b,
            None => {
                println!(
                    "{}: no baseline found (run `ta audit baseline {}`)",
                    agent, agent
                );
                continue;
            }
        };

        let report = ta_audit::drift::compute_drift(&baseline, &events, &drafts, window);

        println!("Drift report for {}", agent);
        println!("{}", "=".repeat(50));
        println!(
            "Baseline: {} goals, computed {}",
            baseline.goal_count,
            baseline.computed_at.format("%Y-%m-%d %H:%M"),
        );
        println!("Window: {} recent goals", window);
        println!(
            "Overall: {}",
            match report.overall_severity {
                DriftSeverity::Normal => "NORMAL — no significant drift",
                DriftSeverity::Warning => "WARNING — notable behavioral changes",
                DriftSeverity::Alert => "ALERT — significant behavioral divergence",
            }
        );

        if report.findings.is_empty() {
            println!("  No drift signals detected.");
        } else {
            println!();
            for finding in &report.findings {
                let icon = match finding.severity {
                    DriftSeverity::Normal => " ",
                    DriftSeverity::Warning => "!",
                    DriftSeverity::Alert => "X",
                };
                println!("[{}] {:?}: {}", icon, finding.signal, finding.description);
                if let (Some(bv), Some(cv)) = (finding.baseline_value, finding.current_value) {
                    println!("    baseline={:.2}  current={:.2}", bv, cv);
                }
            }
        }
        println!();
    }

    Ok(())
}

fn execute_baseline(
    config: &GatewayConfig,
    agent_id: &str,
    log_path: Option<&str>,
) -> anyhow::Result<()> {
    let audit_path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.audit_log.clone());

    let events = if audit_path.exists() {
        AuditLog::read_all(&audit_path)?
    } else {
        Vec::new()
    };
    let drafts = load_draft_summaries(config)?;

    let baseline = ta_audit::drift::compute_baseline(agent_id, &events, &drafts);

    let store = BaselineStore::new(baselines_dir(config));
    store.save(&baseline)?;

    println!("Baseline stored for {}", agent_id);
    println!("  Goals in sample: {}", baseline.goal_count);
    println!("  Resource patterns: {}", baseline.resource_patterns.len());
    println!("  Avg artifact count: {:.1}", baseline.avg_artifact_count);
    println!("  Avg risk score: {:.1}", baseline.avg_risk_score);
    println!(
        "  Escalation rate: {:.1}%",
        baseline.escalation_rate * 100.0
    );
    println!("  Rejection rate: {:.1}%", baseline.rejection_rate * 100.0);
    println!(
        "  Stored at: {}",
        baselines_dir(config)
            .join(format!("{}.json", agent_id))
            .display()
    );

    Ok(())
}

// ── Existing subcommands ──

/// Filter events related to a specific goal.
fn events_for_goal(events: &[AuditEvent], goal_id: &str) -> Vec<AuditEvent> {
    events
        .iter()
        .filter(|e| {
            // Check agent_id or metadata for goal reference.
            e.agent_id.contains(goal_id)
                || e.target_uri
                    .as_deref()
                    .map(|u| u.contains(goal_id))
                    .unwrap_or(false)
                || {
                    // Check metadata for goal_id reference.
                    let meta = e.metadata.to_string();
                    meta.contains(goal_id)
                }
        })
        .cloned()
        .collect()
}

/// Display the decision trail for a goal with reasoning (v0.3.3).
fn show_decision_trail(
    config: &GatewayConfig,
    goal_id: &str,
    log_path: Option<&str>,
) -> anyhow::Result<()> {
    let path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.audit_log.clone());

    if !path.exists() {
        println!("No audit log found at {}", path.display());
        return Ok(());
    }

    let all_events = AuditLog::read_all(&path)?;
    let events = events_for_goal(&all_events, goal_id);

    if events.is_empty() {
        // Show all events if no goal-specific filter matches — the goal_id
        // might be a prefix or might match the draft package instead.
        println!("No events found for goal {}.", goal_id);
        println!();
        println!(
            "Showing all {} events with decision reasoning:",
            all_events.len()
        );

        let reasoned: Vec<_> = all_events
            .iter()
            .filter(|e| e.reasoning.is_some())
            .collect();
        if reasoned.is_empty() {
            println!("  No events have decision reasoning attached.");
            println!("  Reasoning is captured during policy evaluation (v0.3.3).");
        } else {
            print_events_with_reasoning(&reasoned);
        }
        return Ok(());
    }

    println!("Decision trail for goal {}", goal_id);
    println!("{}", "=".repeat(60));
    println!("{} event(s) found", events.len());
    println!();

    let event_refs: Vec<&AuditEvent> = events.iter().collect();
    print_events_with_reasoning(&event_refs);

    // Count events with reasoning.
    let reasoning_count = events.iter().filter(|e| e.reasoning.is_some()).count();
    println!();
    println!(
        "Summary: {} total events, {} with decision reasoning",
        events.len(),
        reasoning_count
    );

    Ok(())
}

/// Print events with their reasoning details.
fn print_events_with_reasoning(events: &[&AuditEvent]) {
    for event in events {
        println!(
            "[{}] {:?} by {}",
            event.timestamp.format("%Y-%m-%d %H:%M:%S"),
            event.action,
            event.agent_id,
        );
        if let Some(target) = &event.target_uri {
            println!("  Target: {}", target);
        }
        if let Some(reasoning) = &event.reasoning {
            println!("  Rationale: {}", reasoning.rationale);
            if !reasoning.applied_principles.is_empty() {
                println!("  Principles: {}", reasoning.applied_principles.join(", "));
            }
            for (i, alt) in reasoning.alternatives.iter().enumerate() {
                println!(
                    "  Alternative {}: {} (rejected: {}{})",
                    i + 1,
                    alt.description,
                    alt.rejected_reason,
                    alt.score
                        .map(|s| format!(", score: {:.2}", s))
                        .unwrap_or_default(),
                );
            }
        }
        println!();
    }
}

// ── VerifyAttestation subcommand (v0.14.1) ──

fn execute_verify_attestation(
    config: &GatewayConfig,
    log_path: Option<&str>,
    event_id: Option<&str>,
    keys_path: Option<&str>,
) -> anyhow::Result<()> {
    let path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.audit_log.clone());

    if !path.exists() {
        println!("No audit log found at {}", path.display());
        return Ok(());
    }

    let keys_dir = keys_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.workspace_root.join(".ta").join("keys"));

    // Load or generate the software backend.
    let backend = SoftwareAttestationBackend::load_or_generate(&keys_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to load attestation key from {}: {}",
            keys_dir.display(),
            e
        )
    })?;

    let events = AuditLog::read_all(&path)?;
    let events_to_check: Vec<&AuditEvent> = if let Some(id) = event_id {
        events
            .iter()
            .filter(|e| e.event_id.to_string().starts_with(id))
            .collect()
    } else {
        events.iter().collect()
    };

    if events_to_check.is_empty() {
        println!("No events found.");
        return Ok(());
    }

    let mut signed = 0usize;
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut unsigned = 0usize;

    println!("{:<36}  {:<16}  STATUS", "EVENT ID", "BACKEND");
    println!("{}", "-".repeat(70));

    for event in &events_to_check {
        if let Some(record) = &event.attestation {
            signed += 1;
            // Reconstruct canonical payload: event JSON with attestation = None.
            let mut canonical_event = (*event).clone();
            canonical_event.attestation = None;
            let canonical = serde_json::to_string(&canonical_event)?;

            match backend.verify(canonical.as_bytes(), record) {
                Ok(true) => {
                    valid += 1;
                    println!(
                        "{:<36}  {:<16}  OK (key: {})",
                        event.event_id, record.backend, record.key_fingerprint
                    );
                }
                Ok(false) => {
                    invalid += 1;
                    println!(
                        "{:<36}  {:<16}  INVALID SIGNATURE",
                        event.event_id, record.backend
                    );
                }
                Err(e) => {
                    invalid += 1;
                    println!(
                        "{:<36}  {:<16}  ERROR: {}",
                        event.event_id, record.backend, e
                    );
                }
            }
        } else {
            unsigned += 1;
            if event_id.is_some() {
                // Only show unsigned events if explicitly requested by ID.
                println!("{:<36}  {:<16}  unsigned", event.event_id, "-");
            }
        }
    }

    println!();
    println!(
        "Summary: {} signed ({} valid, {} invalid), {} unsigned",
        signed, valid, invalid, unsigned
    );

    if invalid > 0 {
        anyhow::bail!(
            "{} event(s) failed attestation verification — audit log may have been tampered with",
            invalid
        );
    }

    Ok(())
}

/// Export audit data for compliance reporting (v0.3.3).
fn export_audit(
    config: &GatewayConfig,
    goal_id: &str,
    _format: &ExportFormat,
    log_path: Option<&str>,
) -> anyhow::Result<()> {
    let path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.audit_log.clone());

    if !path.exists() {
        anyhow::bail!("No audit log found at {}", path.display());
    }

    let all_events = AuditLog::read_all(&path)?;
    let events = events_for_goal(&all_events, goal_id);

    // Build structured export.
    let export = serde_json::json!({
        "export_version": "1.0.0",
        "goal_id": goal_id,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "total_events": events.len(),
        "events_with_reasoning": events.iter().filter(|e| e.reasoning.is_some()).count(),
        "standards_alignment": {
            "iso_42001": "Decision processes documented with rationale (A.6.2.3)",
            "ieee_7001": "Autonomous system decisions explainable to stakeholders",
            "nist_ai_rmf": "MAP 1.1 (purpose documentation), GOVERN 1.3 (decision documentation)"
        },
        "events": events,
    });

    println!("{}", serde_json::to_string_pretty(&export)?);

    Ok(())
}

// ── Messaging audit (v0.15.9) ──────────────────────────────────────────────

/// Print the messaging draft audit log.
fn messaging_audit(
    config: &GatewayConfig,
    log_path: Option<&str>,
    provider_filter: Option<&str>,
    state_filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let path = log_path.map(std::path::PathBuf::from).unwrap_or_else(|| {
        config
            .workspace_root
            .join(".ta")
            .join("messaging-audit.jsonl")
    });

    let log = MessagingAuditLog::at(&path);
    let mut records = log.read_all().map_err(|e| {
        anyhow::anyhow!(
            "Failed to read messaging audit log at {}: {}",
            path.display(),
            e
        )
    })?;

    if records.is_empty() {
        println!("No messaging audit records found.");
        if !path.exists() {
            println!("(Log file does not exist yet: {})", path.display());
            println!("Records are written when a MessagingAdapter creates a draft.");
        }
        return Ok(());
    }

    // Apply filters.
    if let Some(prov) = provider_filter {
        records.retain(|r| r.provider.eq_ignore_ascii_case(prov));
    }
    if let Some(st) = state_filter {
        records.retain(|r| r.state.to_string().eq_ignore_ascii_case(st));
    }

    // Apply limit (most recent first).
    if limit > 0 && records.len() > limit {
        records = records.into_iter().rev().take(limit).rev().collect();
    }

    if records.is_empty() {
        println!("No records match the specified filters.");
        return Ok(());
    }

    println!(
        "{:<30}  {:<10}  {:<30}  {:<40}  {:<10}  created_at",
        "draft_id", "provider", "to", "subject", "state"
    );
    println!("{}", "─".repeat(140usize));

    for r in &records {
        let draft_id = if r.draft_id.len() > 28 {
            format!("{}…", &r.draft_id[..27])
        } else {
            r.draft_id.clone()
        };
        let to = if r.to.len() > 28 {
            format!("{}…", &r.to[..27])
        } else {
            r.to.clone()
        };
        let subject = if r.subject.len() > 38 {
            format!("{}…", &r.subject[..37])
        } else {
            r.subject.clone()
        };
        println!(
            "{:<30}  {:<10}  {:<30}  {:<40}  {:<10}  {}",
            draft_id, r.provider, to, subject, r.state, r.created_at
        );
    }

    println!();
    println!("{} draft record(s)", records.len());

    Ok(())
}

// ── Social audit (v0.15.12) ────────────────────────────────────────────────

/// Print the social media draft audit log.
fn social_audit(
    config: &GatewayConfig,
    log_path: Option<&str>,
    platform_filter: Option<&str>,
    state_filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let path = log_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.workspace_root.join(".ta").join("social-audit.jsonl"));

    let log = SocialAuditLog::at(&path);
    let mut records = log.read_all().map_err(|e| {
        anyhow::anyhow!(
            "Failed to read social audit log at {}: {}",
            path.display(),
            e
        )
    })?;

    if records.is_empty() {
        println!("No social audit records found.");
        if !path.exists() {
            println!("(Log file does not exist yet: {})", path.display());
            println!("Records are written when a SocialAdapter creates a draft or scheduled post.");
        }
        return Ok(());
    }

    // Apply filters.
    if let Some(plat) = platform_filter {
        records.retain(|r| r.platform.eq_ignore_ascii_case(plat));
    }
    if let Some(st) = state_filter {
        records.retain(|r| r.state.to_string().eq_ignore_ascii_case(st));
    }

    // Apply limit (most recent first).
    if limit > 0 && records.len() > limit {
        records = records.into_iter().rev().take(limit).rev().collect();
    }

    if records.is_empty() {
        println!("No records match the specified filters.");
        return Ok(());
    }

    println!(
        "{:<32}  {:<10}  {:<16}  {:<35}  {:<11}  created_at",
        "post_id", "platform", "handle", "body_preview", "state"
    );
    println!("{}", "─".repeat(130usize));

    for r in &records {
        let post_id = if r.post_id.len() > 30 {
            format!("{}…", &r.post_id[..29])
        } else {
            r.post_id.clone()
        };
        let handle = if r.handle.len() > 14 {
            format!("{}…", &r.handle[..13])
        } else {
            r.handle.clone()
        };
        let preview = if r.body_preview.len() > 33 {
            format!("{}…", &r.body_preview[..32])
        } else {
            r.body_preview.clone()
        };
        let approved_marker = if r.manually_approved { " ✓" } else { "" };
        println!(
            "{:<32}  {:<10}  {:<16}  {:<35}  {:<11}  {}{}",
            post_id, r.platform, handle, preview, r.state, r.created_at, approved_marker
        );
    }

    println!();
    println!("{} social post record(s)", records.len());

    Ok(())
}
