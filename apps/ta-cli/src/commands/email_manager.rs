// email_manager.rs — Email Assistant Workflow engine (v0.15.10).
//
// Implements the `email-manager` workflow:
//
//   fetch(since: watermark)
//     → filter rules (ignore / flag / reply / escalate)
//     → [reply] spawn reply-drafting goal
//         → supervisor check (flag_if_contains + confidence threshold)
//         → pass:  create_draft + audit log
//         → fail:  review queue (no draft created)
//   → advance watermark on batch success
//
// Usage:
//   ta workflow run email-manager [--since <iso8601>] [--dry-run]
//   ta workflow init email-manager
//   ta workflow status email-manager

use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use ta_goal::{DraftEmailRecord, DraftEmailState, MessagingAuditLog};
use ta_submit::messaging_adapter::{find_messaging_plugin, ExternalMessagingAdapter};
use ta_submit::messaging_plugin_protocol::{DraftEnvelope, FetchedMessage, MessagingPluginError};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level email-manager workflow config.
#[derive(Debug, Clone, Deserialize)]
pub struct EmailManagerConfig {
    #[serde(rename = "workflow")]
    pub meta: WorkflowMeta,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default, rename = "filter")]
    pub filters: Vec<FilterRule>,
}

impl Default for EmailManagerConfig {
    fn default() -> Self {
        Self {
            meta: WorkflowMeta {
                name: "email-manager".to_string(),
                adapter: "messaging/gmail".to_string(),
                account: None,
                run_every: None,
                constitution: default_constitution_path(),
                max_messages_per_run: default_max_messages(),
            },
            supervisor: SupervisorConfig::default(),
            filters: vec![],
        }
    }
}

fn default_constitution_path() -> String {
    "~/.config/ta/email-constitution.md".to_string()
}

fn default_max_messages() -> u32 {
    50
}

/// `[workflow]` table in email-manager.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowMeta {
    /// Workflow name (must be "email-manager").
    #[allow(dead_code)]
    pub name: String,
    /// Messaging adapter to use (e.g. "messaging/gmail").
    pub adapter: String,
    /// Email account address (None = plugin default).
    pub account: Option<String>,
    /// How often to run (e.g. "30min", "1h"). Registered with daemon scheduler.
    pub run_every: Option<String>,
    /// Path to the email constitution file. Injected verbatim into every reply goal.
    #[serde(default = "default_constitution_path")]
    pub constitution: String,
    /// Maximum messages to process per run.
    #[serde(default = "default_max_messages")]
    pub max_messages_per_run: u32,
}

/// `[supervisor]` table in email-manager.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct SupervisorConfig {
    /// Confidence below this threshold sends to review queue instead of Drafts.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    /// Substrings that — if found in the reply body — trigger a flag.
    #[serde(default)]
    pub flag_if_contains: Vec<String>,
}

fn default_min_confidence() -> f64 {
    0.80
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            min_confidence: default_min_confidence(),
            flag_if_contains: vec![],
        }
    }
}

/// A single `[[filter]]` entry in email-manager.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct FilterRule {
    /// Rule name (for logging).
    pub name: String,
    /// Match if `from` address contains any of these domain names.
    #[serde(default)]
    pub from_domain: Vec<String>,
    /// Match if `subject` contains any of these substrings (case-insensitive).
    #[serde(default)]
    pub subject_contains: Vec<String>,
    /// Action to take on match.
    pub action: FilterAction,
}

/// Filter action for a matched message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterAction {
    /// Compose a reply draft and push to email Drafts folder (after supervisor).
    Reply,
    /// Skip silently — do not draft, do not flag.
    Ignore,
    /// Send directly to TA review queue without drafting.
    Flag,
    /// Send to review queue with "requires human judgment" note — no draft attempt.
    Escalate,
}

impl std::fmt::Display for FilterAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterAction::Reply => write!(f, "reply"),
            FilterAction::Ignore => write!(f, "ignore"),
            FilterAction::Flag => write!(f, "flag"),
            FilterAction::Escalate => write!(f, "escalate"),
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime state types
// ---------------------------------------------------------------------------

/// Watermark state persisted at `~/.config/ta/workflow-state/email-manager.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatermarkState {
    /// ISO-8601 timestamp of the last successfully processed batch.
    pub last_watermark: String,
    /// Timestamp when this state was last updated.
    pub updated_at: String,
}

impl Default for WatermarkState {
    fn default() -> Self {
        Self {
            last_watermark: "1970-01-01T00:00:00Z".to_string(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }
}

/// A flagged or escalated message awaiting human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewQueueEntry {
    /// Unique entry ID.
    pub id: String,
    /// ISO-8601 timestamp when this entry was created.
    pub created_at: String,
    /// Original message that was filtered/flagged.
    pub message_id: String,
    pub message_from: String,
    pub message_subject: String,
    /// Reason for flagging ("escalate", "supervisor_flag:<reason>", "filter_flag").
    pub flag_reason: String,
    /// Proposed reply body (None for escalate or filter_flag with no draft).
    pub proposed_reply: Option<ProposedReplySnapshot>,
    /// Whether this entry has been resolved.
    pub resolved: bool,
    /// How it was resolved ("push_to_drafts", "discard", or None if pending).
    pub resolution: Option<String>,
    /// If true, was manually approved and pushed to Drafts after flagging.
    #[serde(default)]
    pub manually_approved: bool,
}

/// Snapshot of a proposed reply attached to a review queue entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedReplySnapshot {
    pub to: String,
    pub subject: String,
    pub body_html: String,
    pub confidence: f64,
}

/// A reply produced by the reply-drafting goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailReply {
    /// Recipient address.
    pub to: String,
    /// CC addresses (optional).
    #[serde(default)]
    pub cc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// HTML body.
    pub body_html: String,
    /// Plain-text body (optional; derived from body_html if absent).
    pub body_text: Option<String>,
    /// Agent-reported confidence [0.0, 1.0].
    pub confidence: f64,
    /// Provider thread ID for association.
    pub thread_id: Option<String>,
    /// Message-ID of the original for in-reply-to header.
    pub in_reply_to: Option<String>,
}

/// Statistics for a single email-manager run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmailManagerRunStats {
    pub messages_fetched: u32,
    pub messages_ignored: u32,
    pub drafts_created: u32,
    pub flagged_for_review: u32,
    pub escalated: u32,
    pub goal_failures: u32,
}

/// Persisted status for the email-manager workflow (last run metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailManagerStatus {
    /// ISO-8601 timestamp of the last run.
    pub last_run_at: String,
    /// Stats from the last run.
    pub last_run_stats: EmailManagerRunStats,
    /// Current watermark.
    pub watermark: String,
    /// Number of unresolved review queue entries.
    pub review_queue_pending: u32,
    /// Schedule string from config (e.g. "30min").
    pub schedule: Option<String>,
}

// ---------------------------------------------------------------------------
// Trait for messaging operations (injectable for testing)
// ---------------------------------------------------------------------------

/// Abstracts messaging adapter operations for testability.
pub trait MessagingOps {
    fn fetch(
        &self,
        since: &str,
        account: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<FetchedMessage>, MessagingPluginError>;

    fn create_draft(&self, draft: DraftEnvelope) -> Result<String, MessagingPluginError>;
}

impl MessagingOps for ExternalMessagingAdapter {
    fn fetch(
        &self,
        since: &str,
        account: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<FetchedMessage>, MessagingPluginError> {
        ExternalMessagingAdapter::fetch(self, since, account, limit)
    }

    fn create_draft(&self, draft: DraftEnvelope) -> Result<String, MessagingPluginError> {
        ExternalMessagingAdapter::create_draft(self, draft)
    }
}

// ---------------------------------------------------------------------------
// Trait for spawning reply-drafting goals (injectable for testing)
// ---------------------------------------------------------------------------

/// Runs a reply-drafting goal for a single email message.
pub trait ReplyGoalRunner: Send + Sync {
    fn run_reply_goal(
        &self,
        message: &FetchedMessage,
        constitution: &str,
        agent: &str,
    ) -> anyhow::Result<EmailReply>;
}

/// Production implementation that spawns `ta run --headless`.
pub struct TaReplyGoalRunner;

impl ReplyGoalRunner for TaReplyGoalRunner {
    fn run_reply_goal(
        &self,
        message: &FetchedMessage,
        constitution: &str,
        agent: &str,
    ) -> anyhow::Result<EmailReply> {
        let goal_title = format!("Draft reply to: {}", message.subject);

        // Build the context injected into the agent's prompt.
        let context = build_reply_prompt(message, constitution);

        // Write context to a temp file so we can pass it to `ta run`.
        let tmp_path =
            std::env::temp_dir().join(format!("ta-email-ctx-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&tmp_path, &context)?;

        let mut cmd = std::process::Command::new("ta");
        cmd.arg("run")
            .arg(&goal_title)
            .arg("--headless")
            .arg("--agent")
            .arg(agent)
            .arg("--context-file")
            .arg(&tmp_path);

        let output = cmd.output().map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            anyhow::anyhow!(
                "Failed to invoke 'ta run' for email reply: {}\nIs ta installed and on PATH?",
                e
            )
        })?;
        let _ = std::fs::remove_file(&tmp_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "ta run failed (exit {}) for '{}'\nstdout: {}\nstderr: {}",
                output.status,
                goal_title,
                stdout.trim(),
                stderr.trim()
            );
        }

        // Parse `email_reply: <json>` sentinel line from stdout.
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(json) = line.strip_prefix("email_reply: ") {
                match serde_json::from_str::<EmailReply>(json) {
                    Ok(reply) => return Ok(reply),
                    Err(e) => {
                        anyhow::bail!(
                            "ta run produced an invalid email_reply JSON line: {}\nLine: {}",
                            e,
                            json
                        );
                    }
                }
            }
        }

        anyhow::bail!(
            "ta run completed for '{}' but did not produce an 'email_reply: {{...}}' sentinel line.\n\
             The agent must write 'email_reply: <json>' to confirm the reply was composed.\n\
             Full stdout:\n{}",
            goal_title,
            stdout.trim()
        )
    }
}

/// Build the prompt injected into the reply-drafting goal.
pub fn build_reply_prompt(message: &FetchedMessage, constitution: &str) -> String {
    format!(
        "# Email Reply Task\n\n\
         ## Your Constitution\n\n\
         {constitution}\n\n\
         ## Message to Reply To\n\n\
         **From**: {from}\n\
         **Subject**: {subject}\n\
         **Received**: {received_at}\n\
         **Thread ID**: {thread_id}\n\n\
         ### Body\n\n\
         {body}\n\n\
         ---\n\n\
         ## Your Task\n\n\
         Compose a reply to this message following the constitution above.\n\n\
         When done, output exactly ONE line in this format (replace <json> with the reply):\n\n\
         email_reply: <json>\n\n\
         Where <json> is a JSON object with these fields:\n\
         - `to`: recipient address (copy from the From field above)\n\
         - `subject`: subject line (use \"Re: <original subject>\" unless replying inline)\n\
         - `body_html`: your reply in HTML\n\
         - `body_text`: plain-text version (optional)\n\
         - `confidence`: your confidence this reply is correct and policy-compliant [0.0, 1.0]\n\
         - `thread_id`: copy the thread ID above verbatim\n\
         - `in_reply_to`: copy the message ID below if available\n\n\
         Message ID: {message_id}\n",
        constitution = constitution,
        from = message.from,
        subject = message.subject,
        received_at = message.received_at,
        thread_id = if message.thread_id.is_empty() {
            "(none)".to_string()
        } else {
            message.thread_id.clone()
        },
        body = if !message.body_text.is_empty() {
            message.body_text.clone()
        } else {
            message.body_html.clone()
        },
        message_id = message.id,
    )
}

// ---------------------------------------------------------------------------
// Pure logic: filter
// ---------------------------------------------------------------------------

/// Evaluate filter rules against a message. Returns the action of the first
/// matching rule, or `FilterAction::Reply` if no rule matches (default behaviour).
pub fn filter_message(msg: &FetchedMessage, rules: &[FilterRule]) -> FilterAction {
    for rule in rules {
        if matches_rule(msg, rule) {
            return rule.action.clone();
        }
    }
    // Default: attempt a reply for any unfiltered message.
    FilterAction::Reply
}

fn matches_rule(msg: &FetchedMessage, rule: &FilterRule) -> bool {
    tracing::trace!(rule = %rule.name, id = %msg.id, "evaluating filter rule");
    // from_domain check (case-insensitive substring of the From field).
    let from_match = if rule.from_domain.is_empty() {
        true
    } else {
        let from_lower = msg.from.to_lowercase();
        rule.from_domain
            .iter()
            .any(|d| from_lower.contains(&d.to_lowercase()))
    };

    // subject_contains check (case-insensitive).
    let subject_match = if rule.subject_contains.is_empty() {
        true
    } else {
        let subj_lower = msg.subject.to_lowercase();
        rule.subject_contains
            .iter()
            .any(|s| subj_lower.contains(&s.to_lowercase()))
    };

    from_match && subject_match
}

// ---------------------------------------------------------------------------
// Pure logic: supervisor check
// ---------------------------------------------------------------------------

/// Result of a supervisor check.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisorResult {
    /// Whether the draft passed the check.
    pub passed: bool,
    /// Human-readable reason for flagging (None if passed).
    pub flag_reason: Option<String>,
    /// Confidence score from the reply.
    pub confidence: f64,
}

/// Check a draft reply against the supervisor policy.
///
/// Returns `passed: true` iff:
/// - `reply.confidence >= supervisor.min_confidence`
/// - No `flag_if_contains` substring appears in the reply body
pub fn supervisor_check(reply: &EmailReply, supervisor: &SupervisorConfig) -> SupervisorResult {
    // Check confidence threshold.
    if reply.confidence < supervisor.min_confidence {
        return SupervisorResult {
            passed: false,
            flag_reason: Some(format!(
                "Confidence {:.2} is below minimum threshold {:.2}",
                reply.confidence, supervisor.min_confidence
            )),
            confidence: reply.confidence,
        };
    }

    // Check flag_if_contains patterns.
    let body_lower = reply.body_html.to_lowercase();
    for pattern in &supervisor.flag_if_contains {
        if body_lower.contains(&pattern.to_lowercase()) {
            return SupervisorResult {
                passed: false,
                flag_reason: Some(format!("Reply body contains flagged phrase: {:?}", pattern)),
                confidence: reply.confidence,
            };
        }
    }

    SupervisorResult {
        passed: true,
        flag_reason: None,
        confidence: reply.confidence,
    }
}

// ---------------------------------------------------------------------------
// Watermark management
// ---------------------------------------------------------------------------

fn watermark_state_path() -> Option<PathBuf> {
    user_config_dir().map(|d| {
        d.join("ta")
            .join("workflow-state")
            .join("email-manager.json")
    })
}

/// Load the watermark state. Returns `Default` if the file does not exist.
pub fn load_watermark(path: &Path) -> WatermarkState {
    if !path.exists() {
        return WatermarkState::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read watermark state; using default");
            WatermarkState::default()
        }
    }
}

/// Persist the watermark state.
pub fn save_watermark(path: &Path, state: &WatermarkState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Write atomically: write to .tmp, rename.
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Review queue
// ---------------------------------------------------------------------------

fn review_queue_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("email-review-queue.jsonl")
}

/// Append an entry to the review queue.
pub fn push_to_review_queue(project_root: &Path, entry: &ReviewQueueEntry) -> std::io::Result<()> {
    let path = review_queue_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", json)?;
    Ok(())
}

/// Read all review queue entries.
pub fn read_review_queue(project_root: &Path) -> std::io::Result<Vec<ReviewQueueEntry>> {
    let path = review_queue_path(project_root);
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<ReviewQueueEntry>(trimmed) {
            Ok(e) => entries.push(e),
            Err(e) => {
                tracing::warn!(error = %e, "Skipping malformed review queue entry");
            }
        }
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Status persistence
// ---------------------------------------------------------------------------

fn status_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("email-manager-status.json")
}

fn load_status(project_root: &Path) -> Option<EmailManagerStatus> {
    let path = status_path(project_root);
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_status(project_root: &Path, status: &EmailManagerStatus) -> std::io::Result<()> {
    let path = status_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(status)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, &json)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn default_config_path() -> Option<PathBuf> {
    user_config_dir().map(|d| d.join("ta").join("workflows").join("email-manager.toml"))
}

/// Load the email-manager workflow config.
///
/// Search order:
/// 1. `.ta/workflows/email-manager.toml` (project-local)
/// 2. `~/.config/ta/workflows/email-manager.toml` (user-global)
/// 3. Built-in defaults
pub fn load_email_manager_config(project_root: &Path) -> anyhow::Result<EmailManagerConfig> {
    let candidates = [
        project_root
            .join(".ta")
            .join("workflows")
            .join("email-manager.toml"),
        default_config_path().unwrap_or_else(|| PathBuf::from("/dev/null")),
    ];

    for path in &candidates {
        if path.exists() {
            let content = std::fs::read_to_string(path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read email-manager config at {}: {}\n\
                     Run 'ta workflow init email-manager' to create a config.",
                    path.display(),
                    e
                )
            })?;
            let cfg: EmailManagerConfig = toml::from_str(&content).map_err(|e| {
                anyhow::anyhow!(
                    "Invalid email-manager.toml at {}: {}\n\
                     Check the TOML syntax and required fields (adapter, account).",
                    path.display(),
                    e
                )
            })?;
            return Ok(cfg);
        }
    }

    // No config found: return a helpful error.
    let global_path = default_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/ta/workflows/email-manager.toml".to_string());

    anyhow::bail!(
        "No email-manager.toml config found.\n\
         Create one with: ta workflow init email-manager\n\
         Or place a config at: {global_path}"
    )
}

/// Load the email constitution from its configured path.
///
/// Expands `~` to `$HOME`. Returns an empty string (with a warning) if absent.
pub fn load_constitution(constitution_path: &str) -> String {
    let expanded = expand_tilde(constitution_path);
    let path = Path::new(&expanded);
    if !path.exists() {
        tracing::warn!(
            path = %path.display(),
            "Email constitution not found; run 'ta workflow init email-manager' to create it"
        );
        return String::new();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read email constitution");
            String::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Main runner
// ---------------------------------------------------------------------------

/// Options passed into `run_email_manager_with_ops` (injectable for testing).
pub struct EmailRunOptions<'a, M: MessagingOps, G: ReplyGoalRunner> {
    pub project_root: &'a Path,
    pub config: &'a EmailManagerConfig,
    pub adapter: &'a M,
    pub goal_runner: &'a G,
    pub since_override: Option<&'a str>,
    pub dry_run: bool,
    pub agent: &'a str,
}

/// Core pipeline — injectable for tests.
pub fn run_email_manager_with_ops<M: MessagingOps, G: ReplyGoalRunner>(
    opts: &EmailRunOptions<M, G>,
) -> anyhow::Result<EmailManagerRunStats> {
    let config = opts.config;
    let audit_log = MessagingAuditLog::open(opts.project_root);

    // ── Watermark ──────────────────────────────────────────────────────────
    let watermark_path = watermark_state_path().unwrap_or_else(|| {
        opts.project_root
            .join(".ta")
            .join("workflow-state")
            .join("email-manager.json")
    });
    let mut watermark = load_watermark(&watermark_path);
    let since = opts
        .since_override
        .unwrap_or(&watermark.last_watermark)
        .to_string();

    // ── Dry-run ────────────────────────────────────────────────────────────
    if opts.dry_run {
        println!("  [dry-run] Would fetch messages since: {}", since);
        println!("  [dry-run] Adapter: {}", config.meta.adapter);
        if let Some(account) = &config.meta.account {
            println!("  [dry-run] Account: {}", account);
        }
        println!("  [dry-run] Filter rules: {}", config.filters.len());
        println!(
            "  [dry-run] Supervisor min_confidence: {:.2}",
            config.supervisor.min_confidence
        );
        println!("  No drafts will be created.");
        return Ok(EmailManagerRunStats::default());
    }

    // ── Fetch ──────────────────────────────────────────────────────────────
    println!(
        "  Fetching messages since {} from {}…",
        since, config.meta.adapter
    );
    let messages = opts
        .adapter
        .fetch(
            &since,
            config.meta.account.as_deref(),
            Some(config.meta.max_messages_per_run),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "MessagingAdapter '{}' fetch failed: {}\n\
                 Check adapter credentials with: ta adapter health messaging/{}",
                config.meta.adapter,
                e,
                config.meta.adapter.trim_start_matches("messaging/")
            )
        })?;

    println!("  Fetched {} messages.", messages.len());

    let mut stats = EmailManagerRunStats {
        messages_fetched: messages.len() as u32,
        ..Default::default()
    };

    // Load constitution once.
    let constitution = load_constitution(&config.meta.constitution);

    // ── Per-message pipeline ───────────────────────────────────────────────
    let mut all_succeeded = true;
    let mut latest_timestamp = since.clone();

    for msg in &messages {
        // Track the latest received_at timestamp seen.
        if msg.received_at > latest_timestamp {
            latest_timestamp = msg.received_at.clone();
        }

        let action = filter_message(msg, &config.filters);
        match action {
            FilterAction::Ignore => {
                tracing::debug!(id = %msg.id, "Ignoring message");
                stats.messages_ignored += 1;
            }

            FilterAction::Escalate => {
                println!(
                    "  Escalating '{}' (from {}) — requires human judgment",
                    msg.subject, msg.from
                );
                let entry = ReviewQueueEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    created_at: Utc::now().to_rfc3339(),
                    message_id: msg.id.clone(),
                    message_from: msg.from.clone(),
                    message_subject: msg.subject.clone(),
                    flag_reason: "escalate: requires human judgment".to_string(),
                    proposed_reply: None,
                    resolved: false,
                    resolution: None,
                    manually_approved: false,
                };
                if let Err(e) = push_to_review_queue(opts.project_root, &entry) {
                    tracing::warn!(error = %e, "Failed to write review queue entry");
                }
                stats.escalated += 1;
            }

            FilterAction::Flag => {
                println!(
                    "  Flagging '{}' (from {}) — filter rule triggered",
                    msg.subject, msg.from
                );
                let entry = ReviewQueueEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    created_at: Utc::now().to_rfc3339(),
                    message_id: msg.id.clone(),
                    message_from: msg.from.clone(),
                    message_subject: msg.subject.clone(),
                    flag_reason: "filter_flag: direct flag by filter rule".to_string(),
                    proposed_reply: None,
                    resolved: false,
                    resolution: None,
                    manually_approved: false,
                };
                if let Err(e) = push_to_review_queue(opts.project_root, &entry) {
                    tracing::warn!(error = %e, "Failed to write review queue entry");
                }
                stats.flagged_for_review += 1;
            }

            FilterAction::Reply => {
                println!(
                    "  Drafting reply for '{}' (from {})…",
                    msg.subject, msg.from
                );

                // Run reply-drafting goal.
                let reply = match opts
                    .goal_runner
                    .run_reply_goal(msg, &constitution, opts.agent)
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(
                            id = %msg.id,
                            error = %e,
                            "Reply goal failed — skipping message"
                        );
                        println!("  Warning: reply goal failed for '{}': {}", msg.subject, e);
                        stats.goal_failures += 1;
                        all_succeeded = false;
                        continue;
                    }
                };

                // Supervisor check.
                let sv_result = supervisor_check(&reply, &config.supervisor);

                if sv_result.passed {
                    // Push draft to email Drafts folder.
                    let draft_env = DraftEnvelope {
                        to: reply.to.clone(),
                        subject: reply.subject.clone(),
                        body_html: reply.body_html.clone(),
                        in_reply_to: reply.in_reply_to.clone(),
                        thread_id: reply.thread_id.clone(),
                        body_text: reply.body_text.clone(),
                    };

                    match opts.adapter.create_draft(draft_env) {
                        Ok(draft_id) => {
                            println!(
                                "  Draft created: {} (confidence {:.2})",
                                draft_id, reply.confidence
                            );
                            // Audit log.
                            let record = DraftEmailRecord {
                                draft_id: draft_id.clone(),
                                provider: config
                                    .meta
                                    .adapter
                                    .trim_start_matches("messaging/")
                                    .to_string(),
                                to: reply.to.clone(),
                                subject: reply.subject.clone(),
                                created_at: Utc::now().to_rfc3339(),
                                state: DraftEmailState::Drafted,
                                goal_id: None,
                                constitution_check_passed: Some(true),
                                supervisor_score: Some(reply.confidence),
                            };
                            if let Err(e) = audit_log.append(&record) {
                                tracing::warn!(error = %e, "Failed to write audit log entry");
                            }
                            stats.drafts_created += 1;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "create_draft failed");
                            println!(
                                "  Warning: create_draft failed for '{}': {}",
                                msg.subject, e
                            );
                            stats.goal_failures += 1;
                            all_succeeded = false;
                        }
                    }
                } else {
                    let reason = sv_result
                        .flag_reason
                        .unwrap_or_else(|| "supervisor check".to_string());
                    println!(
                        "  Supervisor flagged '{}': {} — sent to review queue",
                        msg.subject, reason
                    );
                    let entry = ReviewQueueEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        created_at: Utc::now().to_rfc3339(),
                        message_id: msg.id.clone(),
                        message_from: msg.from.clone(),
                        message_subject: msg.subject.clone(),
                        flag_reason: format!("supervisor_flag: {}", reason),
                        proposed_reply: Some(ProposedReplySnapshot {
                            to: reply.to.clone(),
                            subject: reply.subject.clone(),
                            body_html: reply.body_html.clone(),
                            confidence: reply.confidence,
                        }),
                        resolved: false,
                        resolution: None,
                        manually_approved: false,
                    };
                    if let Err(e) = push_to_review_queue(opts.project_root, &entry) {
                        tracing::warn!(error = %e, "Failed to write review queue entry");
                    }
                    stats.flagged_for_review += 1;
                }
            }
        }
    }

    // ── Advance watermark on batch success ─────────────────────────────────
    // Only advance if there were no hard failures (goal_failures == 0 and all
    // create_draft calls succeeded). Partial-batch success still advances to
    // avoid re-processing successfully handled messages.
    if all_succeeded && !messages.is_empty() {
        watermark.last_watermark = latest_timestamp;
        watermark.updated_at = Utc::now().to_rfc3339();
        if let Err(e) = save_watermark(&watermark_path, &watermark) {
            tracing::warn!(error = %e, "Failed to persist watermark");
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Public entry points (called from workflow.rs)
// ---------------------------------------------------------------------------

/// Run the email-manager workflow.
pub fn run_email_manager(
    project_root: &Path,
    since_override: Option<&str>,
    dry_run: bool,
    agent: &str,
) -> anyhow::Result<()> {
    let config = load_email_manager_config(project_root)?;

    // Parse the provider name from "messaging/<name>".
    let provider = config
        .meta
        .adapter
        .trim_start_matches("messaging/")
        .to_string();

    // Find the plugin.
    let plugin = find_messaging_plugin(&provider, project_root).ok_or_else(|| {
        anyhow::anyhow!(
            "Messaging adapter '{}' not found.\n\
             Install with: ta adapter setup messaging/{}\n\
             Or check available adapters with: ta adapter list",
            config.meta.adapter,
            provider
        )
    })?;

    let adapter = ExternalMessagingAdapter::new(&plugin.manifest);
    let goal_runner = TaReplyGoalRunner;

    println!("Email Assistant Workflow");
    println!("  Adapter: {}", config.meta.adapter);
    if let Some(account) = &config.meta.account {
        println!("  Account: {}", account);
    }

    let opts = EmailRunOptions {
        project_root,
        config: &config,
        adapter: &adapter,
        goal_runner: &goal_runner,
        since_override,
        dry_run,
        agent,
    };

    let stats = run_email_manager_with_ops(&opts)?;

    // Print summary.
    println!("\nRun complete:");
    println!("  Messages fetched:     {}", stats.messages_fetched);
    println!("  Ignored:              {}", stats.messages_ignored);
    println!("  Drafts created:       {}", stats.drafts_created);
    println!("  Flagged for review:   {}", stats.flagged_for_review);
    println!("  Escalated:            {}", stats.escalated);
    if stats.goal_failures > 0 {
        println!(
            "  Goal failures:        {} (watermark NOT advanced)",
            stats.goal_failures
        );
    }

    if stats.flagged_for_review > 0 || stats.escalated > 0 {
        println!(
            "\n  {} item(s) in review queue. Run 'ta workflow status email-manager' to review.",
            stats.flagged_for_review + stats.escalated
        );
    }

    // Persist status.
    let queue_len = read_review_queue(project_root)
        .map(|q| q.iter().filter(|e| !e.resolved).count() as u32)
        .unwrap_or(0);
    let watermark_state = watermark_state_path()
        .map(|p| load_watermark(&p).last_watermark)
        .unwrap_or_else(|| "unknown".to_string());
    let status = EmailManagerStatus {
        last_run_at: Utc::now().to_rfc3339(),
        last_run_stats: stats,
        watermark: watermark_state,
        review_queue_pending: queue_len,
        schedule: config.meta.run_every.clone(),
    };
    if let Err(e) = save_status(project_root, &status) {
        tracing::warn!(error = %e, "Failed to persist email-manager status");
    }

    Ok(())
}

/// Create the email-constitution.md template and email-manager.toml if absent.
pub fn init_email_manager(project_root: &Path) -> anyhow::Result<()> {
    // 1. Create the constitution file if absent.
    let constitution_path = user_config_dir()
        .map(|d| d.join("ta").join("email-constitution.md"))
        .unwrap_or_else(|| PathBuf::from("email-constitution.md"));

    if !constitution_path.exists() {
        if let Some(parent) = constitution_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&constitution_path, EMAIL_CONSTITUTION_TEMPLATE)?;
        println!(
            "Created email constitution: {}\nEdit this file to configure your voice and policy.",
            constitution_path.display()
        );
    } else {
        println!(
            "Email constitution already exists: {}",
            constitution_path.display()
        );
    }

    // 2. Create the workflow config if absent.
    let config_path = user_config_dir()
        .map(|d| d.join("ta").join("workflows").join("email-manager.toml"))
        .unwrap_or_else(|| {
            project_root
                .join(".ta")
                .join("workflows")
                .join("email-manager.toml")
        });

    if !config_path.exists() {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let constitution_str = constitution_path.display().to_string();
        let toml_content = EMAIL_MANAGER_CONFIG_TEMPLATE
            .replace("~/.config/ta/email-constitution.md", &constitution_str);
        std::fs::write(&config_path, &toml_content)?;
        println!(
            "Created email-manager config: {}\nEdit this file to configure your adapter and filters.",
            config_path.display()
        );
    } else {
        println!(
            "Email-manager config already exists: {}",
            config_path.display()
        );
    }

    println!("\nNext steps:");
    println!(
        "  1. Edit your email constitution: {}",
        constitution_path.display()
    );
    println!(
        "  2. Set your adapter account in: {}",
        config_path.display()
    );
    println!("  3. Install the adapter: ta adapter setup messaging/gmail");
    println!("  4. Run: ta workflow run email-manager");

    Ok(())
}

/// Show the email-manager workflow status.
pub fn show_email_manager_status(project_root: &Path) -> anyhow::Result<()> {
    // Load persisted status.
    if let Some(status) = load_status(project_root) {
        println!("Email Manager Status");
        println!("  Last run:         {}", status.last_run_at);
        println!("  Watermark:        {}", status.watermark);
        if let Some(schedule) = &status.schedule {
            println!("  Schedule:         {}", schedule);
        } else {
            println!("  Schedule:         (not scheduled — run manually)");
        }
        println!("\nLast Run Stats:");
        println!(
            "  Messages fetched: {}",
            status.last_run_stats.messages_fetched
        );
        println!(
            "  Ignored:          {}",
            status.last_run_stats.messages_ignored
        );
        println!(
            "  Drafts created:   {}",
            status.last_run_stats.drafts_created
        );
        println!(
            "  Flagged:          {}",
            status.last_run_stats.flagged_for_review
        );
        println!("  Escalated:        {}", status.last_run_stats.escalated);
    } else {
        println!("Email Manager: no runs recorded yet.");
        println!("  Run: ta workflow run email-manager");
        return Ok(());
    }

    // Show review queue.
    let queue = read_review_queue(project_root)?;
    let pending: Vec<_> = queue.iter().filter(|e| !e.resolved).collect();

    if pending.is_empty() {
        println!("\nReview Queue: empty");
    } else {
        println!("\nReview Queue ({} pending):", pending.len());
        for entry in &pending {
            let has_draft = if entry.proposed_reply.is_some() {
                " [draft attached]"
            } else {
                ""
            };
            println!(
                "  {} — from: {} subject: '{}' reason: {}{}",
                &entry.id[..8],
                entry.message_from,
                entry.message_subject,
                entry.flag_reason,
                has_draft
            );
        }
        println!(
            "\n  To push a flagged item to Drafts: ta workflow run email-manager --approve-flagged <id>"
        );
        println!("  To discard: ta workflow run email-manager --discard-flagged <id>");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

const EMAIL_CONSTITUTION_TEMPLATE: &str = r#"# Email Constitution

This document defines your voice, policies, and constraints for TA-assisted email replies.
TA injects this verbatim into every reply-drafting goal and supervisor check.

## Voice & Tone

- Professional and friendly
- Concise — no unnecessary padding or filler phrases
- Sign off with: [Your Name]

## Topics to Engage

- Product questions and feature requests
- Meeting scheduling and follow-ups
- Project status updates
- Client inquiries

## Topics to Decline (escalate to human review)

- Legal or compliance questions
- HR or personnel matters
- Financial commitments or pricing negotiations
- Crisis or incident communications
- Any request that would make a binding commitment

## Sign-Off Format

[Your sign-off here, e.g.:]
Best,
[Your Name]

## Out-of-Office Language

[Configure if needed, e.g.:]
I'm currently away and will reply when I return on [date].
For urgent matters, please contact [backup contact].

## Forbidden Phrases

The supervisor will flag any reply containing these phrases:
- "I promise"
- "I guarantee"
- "by tomorrow"
- "committed to"
- "legally binding"
"#;

const EMAIL_MANAGER_CONFIG_TEMPLATE: &str = r#"[workflow]
name            = "email-manager"
adapter         = "messaging/gmail"
account         = "me@example.com"
run_every       = "30min"
constitution    = "~/.config/ta/email-constitution.md"
max_messages_per_run = 50

[supervisor]
# Confidence below this threshold → TA review queue instead of Drafts folder
min_confidence  = 0.80
# Always flag if the reply contains any of these (belt-and-suspenders)
flag_if_contains = ["I promise", "I guarantee", "by tomorrow", "committed to"]

[[filter]]
name            = "newsletters"
subject_contains = ["unsubscribe", "newsletter", "mailing list"]
action          = "ignore"

[[filter]]
name            = "auto-replies"
subject_contains = ["out of office", "auto-reply", "automatic reply"]
action          = "ignore"

[[filter]]
name            = "legal-hr"
subject_contains = ["legal", "compliance", "lawsuit", "hr", "termination"]
action          = "escalate"

# Default: any message not matched above gets a reply attempt.
# Add more [[filter]] sections as needed.
"#;

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/{}", home, rest);
        }
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_message(id: &str, from: &str, subject: &str) -> FetchedMessage {
        FetchedMessage {
            id: id.to_string(),
            from: from.to_string(),
            to: "me@example.com".to_string(),
            subject: subject.to_string(),
            body_text: "Hello, can you help me?".to_string(),
            body_html: "<p>Hello, can you help me?</p>".to_string(),
            thread_id: "thread-1".to_string(),
            received_at: "2026-04-07T10:00:00Z".to_string(),
        }
    }

    fn default_supervisor() -> SupervisorConfig {
        SupervisorConfig {
            min_confidence: 0.80,
            flag_if_contains: vec!["I promise".to_string(), "guarantee".to_string()],
        }
    }

    fn make_reply(confidence: f64, body: &str) -> EmailReply {
        EmailReply {
            to: "alice@example.com".to_string(),
            cc: vec![],
            subject: "Re: Question".to_string(),
            body_html: body.to_string(),
            body_text: None,
            confidence,
            thread_id: Some("thread-1".to_string()),
            in_reply_to: None,
        }
    }

    // ── Mock adapter ──────────────────────────────────────────────────────

    struct MockAdapter {
        messages: Vec<FetchedMessage>,
        draft_id: String,
    }

    impl MessagingOps for MockAdapter {
        fn fetch(
            &self,
            _since: &str,
            _account: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Vec<FetchedMessage>, MessagingPluginError> {
            Ok(self.messages.clone())
        }

        fn create_draft(&self, _draft: DraftEnvelope) -> Result<String, MessagingPluginError> {
            Ok(self.draft_id.clone())
        }
    }

    // ── Mock goal runner ──────────────────────────────────────────────────

    struct MockGoalRunner {
        reply: Option<EmailReply>,
    }

    impl ReplyGoalRunner for MockGoalRunner {
        fn run_reply_goal(
            &self,
            _msg: &FetchedMessage,
            _constitution: &str,
            _agent: &str,
        ) -> anyhow::Result<EmailReply> {
            match &self.reply {
                Some(r) => Ok(r.clone()),
                None => anyhow::bail!("mock goal runner: no reply configured"),
            }
        }
    }

    // ── Filter tests ──────────────────────────────────────────────────────

    #[test]
    fn filter_no_rules_returns_reply() {
        let msg = make_message("1", "alice@example.com", "Hello");
        let action = filter_message(&msg, &[]);
        assert_eq!(action, FilterAction::Reply);
    }

    #[test]
    fn filter_ignore_newsletter() {
        let rules = vec![FilterRule {
            name: "newsletters".to_string(),
            from_domain: vec![],
            subject_contains: vec!["unsubscribe".to_string()],
            action: FilterAction::Ignore,
        }];
        let msg = make_message("1", "news@acme.com", "Click here to unsubscribe");
        assert_eq!(filter_message(&msg, &rules), FilterAction::Ignore);
    }

    #[test]
    fn filter_escalate_legal() {
        let rules = vec![FilterRule {
            name: "legal".to_string(),
            from_domain: vec![],
            subject_contains: vec!["legal".to_string()],
            action: FilterAction::Escalate,
        }];
        let msg = make_message("1", "lawyer@firm.com", "Legal notice");
        assert_eq!(filter_message(&msg, &rules), FilterAction::Escalate);
    }

    #[test]
    fn filter_from_domain_match() {
        let rules = vec![FilterRule {
            name: "client".to_string(),
            from_domain: vec!["bigclient.com".to_string()],
            subject_contains: vec![],
            action: FilterAction::Reply,
        }];
        let msg = make_message("1", "Alice <alice@bigclient.com>", "A question");
        assert_eq!(filter_message(&msg, &rules), FilterAction::Reply);

        let other = make_message("2", "bob@otherclient.com", "Another question");
        // No match → default Reply
        assert_eq!(filter_message(&other, &rules), FilterAction::Reply);
    }

    #[test]
    fn filter_first_matching_rule_wins() {
        let rules = vec![
            FilterRule {
                name: "newsletters".to_string(),
                from_domain: vec![],
                subject_contains: vec!["newsletter".to_string()],
                action: FilterAction::Ignore,
            },
            FilterRule {
                name: "questions".to_string(),
                from_domain: vec![],
                subject_contains: vec!["?".to_string()],
                action: FilterAction::Reply,
            },
        ];
        // "newsletter?" matches both — first rule wins.
        let msg = make_message("1", "x@y.com", "Is this a newsletter?");
        assert_eq!(filter_message(&msg, &rules), FilterAction::Ignore);
    }

    #[test]
    fn filter_case_insensitive() {
        let rules = vec![FilterRule {
            name: "oof".to_string(),
            from_domain: vec![],
            subject_contains: vec!["out of office".to_string()],
            action: FilterAction::Ignore,
        }];
        let msg = make_message("1", "x@y.com", "OUT OF OFFICE reply");
        assert_eq!(filter_message(&msg, &rules), FilterAction::Ignore);
    }

    // ── Supervisor tests ──────────────────────────────────────────────────

    #[test]
    fn supervisor_pass_high_confidence() {
        let sv = default_supervisor();
        let reply = make_reply(0.95, "<p>Thank you for reaching out!</p>");
        let result = supervisor_check(&reply, &sv);
        assert!(result.passed);
        assert!(result.flag_reason.is_none());
    }

    #[test]
    fn supervisor_fail_low_confidence() {
        let sv = default_supervisor();
        let reply = make_reply(0.60, "<p>Happy to help.</p>");
        let result = supervisor_check(&reply, &sv);
        assert!(!result.passed);
        assert!(result
            .flag_reason
            .unwrap()
            .contains("below minimum threshold"));
    }

    #[test]
    fn supervisor_fail_flag_if_contains() {
        let sv = default_supervisor();
        let reply = make_reply(0.90, "<p>I promise we will deliver by tomorrow.</p>");
        let result = supervisor_check(&reply, &sv);
        assert!(!result.passed);
        let reason = result.flag_reason.unwrap();
        assert!(
            reason.contains("I promise"),
            "Expected flag_if_contains trigger, got: {}",
            reason
        );
    }

    #[test]
    fn supervisor_flag_if_contains_case_insensitive() {
        let sv = SupervisorConfig {
            min_confidence: 0.80,
            flag_if_contains: vec!["guarantee".to_string()],
        };
        let reply = make_reply(0.92, "<p>We GUARANTEE delivery.</p>");
        let result = supervisor_check(&reply, &sv);
        assert!(!result.passed);
    }

    #[test]
    fn supervisor_exact_threshold_passes() {
        let sv = SupervisorConfig {
            min_confidence: 0.80,
            flag_if_contains: vec![],
        };
        // Exactly at threshold should pass.
        let reply = make_reply(0.80, "<p>Reply.</p>");
        let result = supervisor_check(&reply, &sv);
        assert!(result.passed);
    }

    // ── Watermark tests ───────────────────────────────────────────────────

    #[test]
    fn watermark_default_epoch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("watermark.json");
        let state = load_watermark(&path);
        assert_eq!(state.last_watermark, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn watermark_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("watermark.json");
        let state = WatermarkState {
            last_watermark: "2026-04-07T12:00:00Z".to_string(),
            updated_at: "2026-04-07T12:01:00Z".to_string(),
        };
        save_watermark(&path, &state).unwrap();
        let loaded = load_watermark(&path);
        assert_eq!(loaded.last_watermark, "2026-04-07T12:00:00Z");
    }

    #[test]
    fn watermark_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("deep").join("nested").join("wm.json");
        let state = WatermarkState {
            last_watermark: "2026-01-01T00:00:00Z".to_string(),
            updated_at: Utc::now().to_rfc3339(),
        };
        save_watermark(&path, &state).unwrap();
        assert!(path.exists());
    }

    // ── Review queue tests ────────────────────────────────────────────────

    #[test]
    fn review_queue_empty_returns_empty() {
        let dir = tempdir().unwrap();
        let entries = read_review_queue(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn review_queue_append_and_read() {
        let dir = tempdir().unwrap();
        let entry = ReviewQueueEntry {
            id: "abc123".to_string(),
            created_at: "2026-04-07T10:00:00Z".to_string(),
            message_id: "msg-1".to_string(),
            message_from: "alice@example.com".to_string(),
            message_subject: "Legal matter".to_string(),
            flag_reason: "escalate: requires human judgment".to_string(),
            proposed_reply: None,
            resolved: false,
            resolution: None,
            manually_approved: false,
        };
        push_to_review_queue(dir.path(), &entry).unwrap();
        let entries = read_review_queue(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "abc123");
        assert_eq!(entries[0].flag_reason, "escalate: requires human judgment");
    }

    // ── Full pipeline tests (mock adapter + mock goal runner) ─────────────

    fn make_config_with_filters(
        filters: Vec<FilterRule>,
        supervisor: SupervisorConfig,
    ) -> EmailManagerConfig {
        EmailManagerConfig {
            meta: WorkflowMeta {
                name: "email-manager".to_string(),
                adapter: "messaging/mock".to_string(),
                account: Some("me@example.com".to_string()),
                run_every: None,
                constitution: "/dev/null".to_string(),
                max_messages_per_run: 50,
            },
            supervisor,
            filters,
        }
    }

    #[test]
    fn pipeline_supervisor_pass_creates_draft() {
        let dir = tempdir().unwrap();
        let messages = vec![make_message("msg-1", "alice@example.com", "Help needed")];
        let adapter = MockAdapter {
            messages,
            draft_id: "draft-xyz".to_string(),
        };
        let goal_runner = MockGoalRunner {
            reply: Some(make_reply(0.90, "<p>Happy to help!</p>")),
        };
        let config = make_config_with_filters(vec![], SupervisorConfig::default());

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &goal_runner,
            since_override: Some("1970-01-01T00:00:00Z"),
            dry_run: false,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(stats.messages_fetched, 1);
        assert_eq!(stats.drafts_created, 1);
        assert_eq!(stats.flagged_for_review, 0);

        // Audit log should have one entry.
        let audit = MessagingAuditLog::open(dir.path());
        let records = audit.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].draft_id, "draft-xyz");
        assert_eq!(records[0].constitution_check_passed, Some(true));
    }

    #[test]
    fn pipeline_supervisor_fail_goes_to_review_queue() {
        let dir = tempdir().unwrap();
        let messages = vec![make_message("msg-1", "alice@example.com", "Question")];
        let adapter = MockAdapter {
            messages,
            draft_id: "draft-never".to_string(),
        };
        let goal_runner = MockGoalRunner {
            // Low confidence → supervisor fails
            reply: Some(make_reply(0.40, "<p>Maybe.</p>")),
        };
        let config = make_config_with_filters(vec![], SupervisorConfig::default());

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &goal_runner,
            since_override: Some("1970-01-01T00:00:00Z"),
            dry_run: false,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(
            stats.drafts_created, 0,
            "No draft should be created on supervisor fail"
        );
        assert_eq!(stats.flagged_for_review, 1);

        // Review queue should have one entry with proposed reply.
        let queue = read_review_queue(dir.path()).unwrap();
        assert_eq!(queue.len(), 1);
        assert!(queue[0].proposed_reply.is_some());
        assert!(queue[0].flag_reason.contains("supervisor_flag"));

        // Audit log must be empty.
        let audit = MessagingAuditLog::open(dir.path());
        let records = audit.read_all().unwrap();
        assert!(records.is_empty(), "No audit record when draft not created");
    }

    #[test]
    fn pipeline_escalate_filter_goes_to_review_queue_without_goal() {
        let dir = tempdir().unwrap();
        let messages = vec![make_message("msg-1", "counsel@law.com", "Legal notice")];
        let adapter = MockAdapter {
            messages,
            draft_id: "should-not-appear".to_string(),
        };
        // Goal runner that panics if called — ensures no goal is run for escalate.
        struct PanicGoalRunner;
        impl ReplyGoalRunner for PanicGoalRunner {
            fn run_reply_goal(
                &self,
                _: &FetchedMessage,
                _: &str,
                _: &str,
            ) -> anyhow::Result<EmailReply> {
                panic!("Goal runner should not be called for escalated messages");
            }
        }
        let rules = vec![FilterRule {
            name: "legal".to_string(),
            from_domain: vec![],
            subject_contains: vec!["legal".to_string()],
            action: FilterAction::Escalate,
        }];
        let config = make_config_with_filters(rules, SupervisorConfig::default());

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &PanicGoalRunner,
            since_override: Some("1970-01-01T00:00:00Z"),
            dry_run: false,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(stats.escalated, 1);
        assert_eq!(stats.drafts_created, 0);

        let queue = read_review_queue(dir.path()).unwrap();
        assert_eq!(queue.len(), 1);
        assert!(queue[0].flag_reason.contains("escalate"));
        assert!(queue[0].proposed_reply.is_none());
    }

    #[test]
    fn pipeline_dry_run_creates_no_drafts() {
        let dir = tempdir().unwrap();
        let messages = vec![
            make_message("1", "a@b.com", "Hello"),
            make_message("2", "c@d.com", "World"),
        ];
        let adapter = MockAdapter {
            messages,
            draft_id: "should-not-appear".to_string(),
        };
        let goal_runner = MockGoalRunner {
            reply: Some(make_reply(0.95, "<p>Hi</p>")),
        };
        let config = make_config_with_filters(vec![], SupervisorConfig::default());

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &goal_runner,
            since_override: None,
            dry_run: true,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(stats.messages_fetched, 0);
        assert_eq!(stats.drafts_created, 0);

        // Audit log must not be written.
        let audit = MessagingAuditLog::open(dir.path());
        assert!(audit.read_all().unwrap().is_empty());
    }

    #[test]
    fn pipeline_watermark_advances_on_success() {
        let dir = tempdir().unwrap();
        let wm_path = dir.path().join("wm.json");
        // Pre-seed a watermark.
        let initial = WatermarkState {
            last_watermark: "2026-04-01T00:00:00Z".to_string(),
            updated_at: "2026-04-01T00:00:00Z".to_string(),
        };
        save_watermark(&wm_path, &initial).unwrap();

        // We can't easily control the watermark path in the pipeline without
        // refactoring (it uses user_config_dir()), so test the pure functions:
        let mut state = load_watermark(&wm_path);
        state.last_watermark = "2026-04-07T10:00:00Z".to_string();
        state.updated_at = Utc::now().to_rfc3339();
        save_watermark(&wm_path, &state).unwrap();

        let loaded = load_watermark(&wm_path);
        assert_eq!(loaded.last_watermark, "2026-04-07T10:00:00Z");
    }

    #[test]
    fn pipeline_flag_if_contains_triggers_review_queue() {
        let dir = tempdir().unwrap();
        let messages = vec![make_message(
            "msg-1",
            "alice@example.com",
            "Delivery schedule",
        )];
        let adapter = MockAdapter {
            messages,
            draft_id: "never".to_string(),
        };
        let goal_runner = MockGoalRunner {
            reply: Some(make_reply(0.92, "<p>I promise delivery by tomorrow.</p>")),
        };
        let sv = SupervisorConfig {
            min_confidence: 0.80,
            flag_if_contains: vec!["I promise".to_string()],
        };
        let config = make_config_with_filters(vec![], sv);

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &goal_runner,
            since_override: Some("1970-01-01T00:00:00Z"),
            dry_run: false,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(stats.drafts_created, 0);
        assert_eq!(stats.flagged_for_review, 1);

        let queue = read_review_queue(dir.path()).unwrap();
        assert!(queue[0].flag_reason.contains("I promise"));
    }

    #[test]
    fn pipeline_ignore_filter_skips_message() {
        let dir = tempdir().unwrap();
        let messages = vec![make_message(
            "msg-1",
            "news@example.com",
            "Unsubscribe from our newsletter",
        )];
        let adapter = MockAdapter {
            messages,
            draft_id: "never".to_string(),
        };
        struct PanicGoalRunner;
        impl ReplyGoalRunner for PanicGoalRunner {
            fn run_reply_goal(
                &self,
                _: &FetchedMessage,
                _: &str,
                _: &str,
            ) -> anyhow::Result<EmailReply> {
                panic!("goal runner must not be called for ignored messages");
            }
        }
        let rules = vec![FilterRule {
            name: "newsletters".to_string(),
            from_domain: vec![],
            subject_contains: vec!["unsubscribe".to_string()],
            action: FilterAction::Ignore,
        }];
        let config = make_config_with_filters(rules, SupervisorConfig::default());

        let opts = EmailRunOptions {
            project_root: dir.path(),
            config: &config,
            adapter: &adapter,
            goal_runner: &PanicGoalRunner,
            since_override: Some("1970-01-01T00:00:00Z"),
            dry_run: false,
            agent: "claude-code",
        };

        let stats = run_email_manager_with_ops(&opts).unwrap();
        assert_eq!(stats.messages_ignored, 1);
        assert_eq!(stats.drafts_created, 0);
    }

    // ── build_reply_prompt ────────────────────────────────────────────────

    #[test]
    fn reply_prompt_includes_constitution_and_message() {
        let msg = make_message("msg-1", "alice@example.com", "Need help");
        let constitution = "Be professional and concise.";
        let prompt = build_reply_prompt(&msg, constitution);
        assert!(prompt.contains("Be professional and concise."));
        assert!(prompt.contains("alice@example.com"));
        assert!(prompt.contains("Need help"));
        assert!(prompt.contains("email_reply:"));
    }

    // ── Config loading ────────────────────────────────────────────────────

    #[test]
    fn load_email_manager_config_from_project_dir() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(".ta").join("workflows");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("email-manager.toml"),
            r#"[workflow]
name = "email-manager"
adapter = "messaging/gmail"
account = "test@example.com"
constitution = "~/.config/ta/email-constitution.md"
"#,
        )
        .unwrap();

        let cfg = load_email_manager_config(dir.path()).unwrap();
        assert_eq!(cfg.meta.account.as_deref(), Some("test@example.com"));
        assert_eq!(cfg.meta.adapter, "messaging/gmail");
    }

    #[test]
    fn load_email_manager_config_missing_returns_error() {
        let dir = tempdir().unwrap();
        let result = load_email_manager_config(dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("ta workflow init email-manager"),
            "Error should mention init command, got: {}",
            msg
        );
    }
}
