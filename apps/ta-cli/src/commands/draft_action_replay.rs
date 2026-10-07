// draft_action_replay.rs: replay approved `ta_external_action` captures on apply.
//
// Tracked as trustedautonomy-46. When an agent calls `ta_external_action` and
// the action type's policy is `review`, the gateway captures a `PendingAction`
// (tool_name `ta_external_action:<type>`) into the goal's draft package instead
// of executing it. Before this module, `ta draft apply` never executed any of
// those captures, so approving a draft that contained an email/social/api/db
// action had no real-world effect at all.
//
// This module closes that gap, with safety as the primary design constraint
// (these are irreversible real-world effects such as sending email):
//
// 1. Only a genuinely applied draft (`DraftStatus::Applied`) is replayed, and
//    never from a dry run (a dry run only previews). The hook is called from
//    the tail of `apply_package` and from `ta draft replay-actions`; it is not
//    reachable from view/build/deny.
// 2. At-most-once: a durable, append-only ledger at
//    `.ta/action-replay-ledger.jsonl` (keyed by draft_id + action_id) is
//    checked before every execution. An `intent` record is written and fsynced
//    BEFORE calling the executor, and a terminal record after. A crash between
//    the two leaves only the intent, which is reported as "outcome unknown" and
//    is never re-sent automatically. The trade-off is deliberate: we would
//    rather drop an action (and tell the human) than send it twice.
//    Failed/blocked/no-executor outcomes are recorded and NOT auto-retried; the
//    human retries deliberately with `ta draft replay-actions <id> --retry-failed`.
// 3. Policy is re-evaluated at replay time with the same inputs the capture
//    path uses (`.ta/workflow.toml` `[actions.<type>]`, `EmailDispatchGuard`,
//    `.ta/constitution.toml` rules, `allowed_recipients`, `allowed_domains`,
//    per-draft `rate_limit`, cross-session `max_per_hour`/`max_per_day`). An
//    action that would now be blocked is not executed and is reported. If
//    either policy file exists but does not parse, nothing is replayed (the
//    loaders would otherwise silently fall back to permissive defaults).
// 4. Execution goes only through `ActionRegistry` (built-in stubs plus
//    discovered adapter plugins, exactly like the gateway). Unknown action
//    types and raw intercepted MCP tool calls are skipped with a clear message.
// 5. Per-action failure (including a panicking plugin) never blocks the other
//    actions and never rolls back the already-applied draft. Every outcome is
//    printed and logged with structured tracing fields.
//
// `ta_propose_*` pending actions are owned by their own dedicated replay
// (see `replay_propose_task_update_actions` / `draft_task_replay.rs`) and are
// always excluded here so the two paths can never double-execute.
//
// Concurrency: both entry points run while holding the `.ta/apply.lock`
// (`ApplyLock`), so two processes can never interleave ledger writes for the
// same workspace.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_actions::{
    discover_adapter_actions, ActionError, ActionPolicies, ActionPolicy, ActionRegistry,
    DispatchResult, EmailDispatchGuard, PolicyConstitution, SessionRateLimitResult,
    SessionRateLimiter,
};
use ta_changeset::draft_package::{
    ActionKind, ArtifactDisposition, DraftPackage, DraftStatus, PendingAction,
};
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

/// File name of the replay ledger under `.ta/`.
pub(crate) const LEDGER_FILE: &str = "action-replay-ledger.jsonl";
/// `tool_name` prefix the gateway uses for `ta_external_action` captures.
pub(crate) const EXTERNAL_ACTION_PREFIX: &str = "ta_external_action:";
/// `tool_name` prefix owned by the dedicated `ta_propose_*` replay paths.
pub(crate) const PROPOSE_PREFIX: &str = "ta_propose_";

/// Separate rate-limit bucket for real sends, so that limits configured with
/// `max_per_hour`/`max_per_day` govern how many actions actually execute,
/// independently of how many were merely proposed at capture time (which the
/// gateway records under the plain action type).
fn replay_rate_bucket(action_type: &str) -> String {
    format!("replay:{action_type}")
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// State recorded in the replay ledger for one (draft_id, action_id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LedgerState {
    /// About to call the executor. Written and fsynced before execution.
    Intent,
    /// Executor returned success.
    Executed,
    /// Executor returned an error (or panicked). Side effects are unknown.
    Failed,
    /// Replay-time policy refused the action. Nothing was executed.
    Blocked,
    /// Only a schema stub is registered for this type. Nothing was executed.
    NoExecutor,
}

/// One append-only ledger line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LedgerEntry {
    pub draft_id: Uuid,
    pub action_id: Uuid,
    pub action_type: String,
    pub state: LedgerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Durable at-most-once ledger of replayed actions.
pub(crate) struct ReplayLedger {
    path: PathBuf,
    latest: HashMap<(Uuid, Uuid), LedgerEntry>,
    executed_per_type: HashMap<(Uuid, String), u32>,
}

impl ReplayLedger {
    /// Open (or lazily create) the ledger under `ta_dir`.
    ///
    /// Fails closed: an unparseable line anywhere except the final line means
    /// the ledger can no longer be trusted to prevent double execution, so the
    /// caller must not replay anything. A torn final line (crash mid-append) is
    /// tolerated with a warning: intents are fsynced before any execution, so a
    /// torn line can only be a record whose action had not yet been executed
    /// (torn intent) or one whose intent is already durable (torn terminal).
    pub(crate) fn open(ta_dir: &Path) -> anyhow::Result<Self> {
        let path = ta_dir.join(LEDGER_FILE);
        let mut ledger = Self {
            path: path.clone(),
            latest: HashMap::new(),
            executed_per_type: HashMap::new(),
        };
        if !path.exists() {
            return Ok(ledger);
        }
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "Could not read the action replay ledger at {}: {}. No pending actions were \
                 replayed (the ledger is what prevents double execution). Fix the file's \
                 permissions and run `ta draft replay-actions <draft-id>`.",
                path.display(),
                e
            )
        })?;
        let torn_tail = !content.is_empty() && !content.ends_with('\n');
        let lines: Vec<&str> = content.lines().collect();
        let last_idx = lines.len().saturating_sub(1);
        for (idx, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LedgerEntry>(line) {
                Ok(entry) => ledger.index(entry),
                Err(e) if torn_tail && idx == last_idx => {
                    tracing::warn!(
                        path = %path.display(),
                        line = idx + 1,
                        error = %e,
                        "ignoring torn final line in action replay ledger (an interrupted \
                         append); it will be truncated on the next write"
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "The action replay ledger at {} is corrupted at line {} ({}). \
                         No pending actions were replayed, because the ledger is what \
                         prevents an email or other external action from being sent twice. \
                         Inspect the file, repair or remove the bad line, then run \
                         `ta draft replay-actions <draft-id>`.",
                        path.display(),
                        idx + 1,
                        e
                    );
                }
            }
        }
        Ok(ledger)
    }

    fn index(&mut self, entry: LedgerEntry) {
        if entry.state == LedgerState::Executed {
            *self
                .executed_per_type
                .entry((entry.draft_id, entry.action_type.clone()))
                .or_insert(0) += 1;
        }
        self.latest.insert((entry.draft_id, entry.action_id), entry);
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Latest recorded state for a draft's action, if any.
    pub(crate) fn latest(&self, draft_id: Uuid, action_id: Uuid) -> Option<&LedgerEntry> {
        self.latest.get(&(draft_id, action_id))
    }

    /// Number of actions of `action_type` already executed for this draft.
    pub(crate) fn executed_count(&self, draft_id: Uuid, action_type: &str) -> u32 {
        self.executed_per_type
            .get(&(draft_id, action_type.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Append one entry and fsync it before returning.
    pub(crate) fn record(&mut self, entry: LedgerEntry) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.path)?;
        // A torn tail (an append interrupted before its newline) is dropped
        // before writing. That is always safe: intents are fsynced before any
        // execution, so a torn line is either an intent whose action never ran,
        // or a terminal record whose durable intent remains (outcome-unknown).
        let mut existing = Vec::new();
        file.read_to_end(&mut existing)?;
        if !existing.is_empty() && existing.last() != Some(&b'\n') {
            let keep = existing
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            file.set_len(keep as u64)?;
        }
        file.seek(SeekFrom::End(0))?;
        let mut line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        self.index(entry);
        Ok(())
    }
}

// ── Inputs / outputs ─────────────────────────────────────────────────────────

/// The parts of a draft package the replay needs.
#[derive(Debug, Clone)]
pub(crate) struct DraftReplayInput {
    pub draft_id: Uuid,
    pub status: DraftStatus,
    pub pending_actions: Vec<PendingAction>,
    /// True when the draft was applied with partial selective review (some
    /// artifacts rejected/discussed/left pending while others were approved).
    /// In that case only actions explicitly marked `approved` are replayed.
    pub partial_review: bool,
}

impl DraftReplayInput {
    pub(crate) fn from_package(pkg: &DraftPackage) -> Self {
        let dispositions: Vec<&ArtifactDisposition> = pkg
            .changes
            .artifacts
            .iter()
            .map(|a| &a.disposition)
            .collect();
        let all_pending = dispositions
            .iter()
            .all(|d| **d == ArtifactDisposition::Pending);
        let all_approved = dispositions
            .iter()
            .all(|d| **d == ArtifactDisposition::Approved);
        Self {
            draft_id: pkg.package_id,
            status: pkg.status.clone(),
            pending_actions: pkg.changes.pending_actions.clone(),
            partial_review: !(all_pending || all_approved),
        }
    }
}

/// Knobs for one replay run.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReplayOptions {
    /// Preview only: report what would run, execute nothing, write no ledger.
    pub dry_run: bool,
    /// Deliberately retry actions previously recorded as failed, blocked, or
    /// lacking an executor. Never retries an action whose outcome is unknown.
    pub retry_failed: bool,
}

/// What happened to one pending action during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplayOutcome {
    /// The executor ran and returned success.
    Executed,
    /// Dry run: this action would be executed.
    WouldExecute,
    /// The ledger shows this action already executed. Not re-run.
    AlreadyReplayed,
    /// The ledger has an intent but no outcome (a previous run was interrupted
    /// mid-execution). Never re-run automatically.
    OutcomeUnknown,
    /// A previous attempt was recorded as failed/blocked/no-executor and
    /// `--retry-failed` was not given.
    PreviouslyFailed { state: LedgerState, detail: String },
    /// Not attempted (unknown type, rejected disposition, ...).
    Skipped { reason: String },
    /// Replay-time policy refused it. Nothing executed.
    Blocked { reason: String },
    /// Only a schema stub is registered; nothing executed.
    NoExecutor { reason: String },
    /// The executor returned an error or panicked.
    Failed { error: String },
}

impl ReplayOutcome {
    fn label(&self) -> &'static str {
        match self {
            ReplayOutcome::Executed => "executed",
            ReplayOutcome::WouldExecute => "would-execute",
            ReplayOutcome::AlreadyReplayed => "already-replayed",
            ReplayOutcome::OutcomeUnknown => "outcome-unknown",
            ReplayOutcome::PreviouslyFailed { .. } => "previously-failed",
            ReplayOutcome::Skipped { .. } => "skipped",
            ReplayOutcome::Blocked { .. } => "blocked",
            ReplayOutcome::NoExecutor { .. } => "no-executor",
            ReplayOutcome::Failed { .. } => "failed",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            ReplayOutcome::PreviouslyFailed { detail, .. } => Some(detail),
            ReplayOutcome::Skipped { reason }
            | ReplayOutcome::Blocked { reason }
            | ReplayOutcome::NoExecutor { reason } => Some(reason),
            ReplayOutcome::Failed { error } => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActionReplayResult {
    pub action_id: Uuid,
    pub action_type: String,
    pub description: String,
    pub outcome: ReplayOutcome,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayReport {
    pub draft_id: Uuid,
    pub dry_run: bool,
    pub ledger_path: PathBuf,
    pub results: Vec<ActionReplayResult>,
}

impl ReplayReport {
    pub(crate) fn count(&self, pred: impl Fn(&ReplayOutcome) -> bool) -> usize {
        self.results.iter().filter(|r| pred(&r.outcome)).count()
    }
}

// ── Policy re-evaluation ─────────────────────────────────────────────────────

/// Re-run the capture-time policy gates for one action. Returns warnings to
/// surface on success, or the reason the action must not execute.
fn check_replay_policy(
    action_type: &str,
    payload: &serde_json::Value,
    policies: &ActionPolicies,
    constitution: &PolicyConstitution,
) -> Result<Vec<String>, String> {
    let cfg = policies.policy_for(action_type);
    let mut warnings = Vec::new();

    if cfg.policy == ActionPolicy::Block {
        return Err(format!(
            "action type '{action_type}' is now blocked by policy \
             ([actions.{action_type}] policy = \"block\" in .ta/workflow.toml)"
        ));
    }

    // A human approved this capture, so the effective policy is `review`
    // whenever the dispatch guard would have forced it there.
    let effective = match EmailDispatchGuard::new().enforce(action_type, &cfg.policy) {
        DispatchResult::Blocked { message } => return Err(message),
        DispatchResult::ForcedReview { .. } => ActionPolicy::Review,
        DispatchResult::Allowed => cfg.policy.clone(),
    };

    match constitution.check_action_policy(action_type, &effective) {
        Ok(()) => {}
        Err(v) if v.is_warn => warnings.push(v.message),
        Err(v) => return Err(format!("constitution rule: {}", v.message)),
    }

    if action_type == "email" && !cfg.allowed_recipients.is_empty() {
        let mut recipients: Vec<String> = Vec::new();
        if let Some(to) = payload.get("to").and_then(|v| v.as_str()) {
            recipients.push(to.to_string());
        }
        if let Some(cc) = payload.get("cc").and_then(|v| v.as_array()) {
            recipients.extend(cc.iter().filter_map(|v| v.as_str()).map(String::from));
        }
        for r in &recipients {
            if !cfg.allowed_recipients.iter().any(|a| a == r) {
                return Err(format!(
                    "recipient '{r}' is not in allowed_recipients \
                     ([actions.email].allowed_recipients in .ta/workflow.toml)"
                ));
            }
        }
    }

    if !cfg.allowed_domains.is_empty() {
        if let Some(url) = payload.get("url").and_then(|v| v.as_str()) {
            let host = url_host(url).unwrap_or_default();
            if !cfg.allowed_domains.iter().any(|d| domain_matches(d, &host)) {
                return Err(format!(
                    "host '{host}' is not in allowed_domains \
                     ([actions.{action_type}].allowed_domains in .ta/workflow.toml)"
                ));
            }
        }
    }

    if action_type == "db_query" {
        let schema_altering = payload
            .get("query")
            .and_then(|v| v.as_str())
            .is_some_and(is_schema_altering_statement);
        match constitution.check_db_mutation(0, schema_altering, cfg.allow_schema_drops) {
            Ok(()) => {}
            Err(v) if v.is_warn => warnings.push(v.message),
            Err(v) => return Err(format!("constitution rule: {}", v.message)),
        }
    }

    Ok(warnings)
}

/// Same heuristic `ta-db-proxy` applies to staged DDL.
fn is_schema_altering_statement(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    upper.contains("DROP TABLE")
        || upper.contains("TRUNCATE")
        || (upper.contains("ALTER TABLE") && upper.contains("DROP COLUMN"))
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or(host_port);
    Some(host.to_ascii_lowercase())
}

fn domain_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.ends_with(&format!(".{suffix}")),
        None => pattern == host,
    }
}

/// Fail closed on unreadable policy files.
///
/// `ActionPolicies::load` and `PolicyConstitution::load` fall back to defaults
/// (with a warning) when their file cannot be parsed. For capture that is fine,
/// because a capture only queues an action for review. For replay it would fail
/// open: a typo in `workflow.toml` would silently drop a `policy = "block"` or
/// an `allowed_recipients` list right before an irreversible send. So replay
/// refuses to run until both files parse.
fn ensure_policy_files_parse(ta_dir: &Path) -> anyhow::Result<()> {
    use std::collections::HashMap as Map;
    use ta_actions::constitution_rules::ConstitutionRule;
    use ta_actions::ActionPolicyConfig;

    #[derive(Deserialize)]
    struct WorkflowActions {
        #[serde(default)]
        #[allow(dead_code)]
        actions: Map<String, ActionPolicyConfig>,
    }
    #[derive(Deserialize, Default)]
    struct RuleSets {
        #[serde(default)]
        #[allow(dead_code)]
        block: Vec<ConstitutionRule>,
        #[serde(default)]
        #[allow(dead_code)]
        warn: Vec<ConstitutionRule>,
    }
    #[derive(Deserialize)]
    struct Constitution {
        #[serde(default)]
        #[allow(dead_code)]
        rules: RuleSets,
    }

    fn check<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "Could not read {} ({}). No pending actions were replayed, because the \
                 policy in this file must be re-checked before any external action runs. \
                 Fix the file, then run `ta draft replay-actions <draft-id>`.",
                path.display(),
                e
            )
        })?;
        toml::from_str::<T>(&content).map_err(|e| {
            anyhow::anyhow!(
                "{} does not parse ({}). No pending actions were replayed, because the \
                 policy in this file must be re-checked before any external action runs. \
                 Fix the file, then run `ta draft replay-actions <draft-id>`.",
                path.display(),
                e
            )
        })?;
        Ok(())
    }

    check::<WorkflowActions>(&ta_dir.join("workflow.toml"))?;
    check::<Constitution>(&ta_dir.join("constitution.toml"))?;
    Ok(())
}

// ── Core replay ──────────────────────────────────────────────────────────────

/// Replay the eligible pending actions of one draft through `registry`.
///
/// Returns `Err` only when nothing could safely be attempted at all (draft not
/// applied, ledger unreadable/corrupted). Per-action problems are reported in
/// the returned [`ReplayReport`] and never abort the other actions.
pub(crate) fn replay_pending_actions(
    workspace_root: &Path,
    input: &DraftReplayInput,
    registry: &ActionRegistry,
    opts: ReplayOptions,
) -> anyhow::Result<ReplayReport> {
    let ta_dir = workspace_root.join(".ta");
    let candidates: Vec<&PendingAction> = input
        .pending_actions
        .iter()
        .filter(|a| {
            let owned_elsewhere = a.tool_name.starts_with(PROPOSE_PREFIX);
            if owned_elsewhere {
                tracing::debug!(
                    draft_id = %input.draft_id,
                    action_id = %a.action_id,
                    tool_name = %a.tool_name,
                    "action replay: leaving ta_propose_* action to its dedicated replay"
                );
            }
            !owned_elsewhere
        })
        .collect();

    let mut report = ReplayReport {
        draft_id: input.draft_id,
        dry_run: opts.dry_run,
        ledger_path: ta_dir.join(LEDGER_FILE),
        results: Vec::new(),
    };
    if candidates.is_empty() {
        return Ok(report);
    }

    if !opts.dry_run && !matches!(input.status, DraftStatus::Applied { .. }) {
        anyhow::bail!(
            "Refusing to replay {} pending action(s) for draft {}: the draft is '{}', not \
             applied. Pending actions only execute after a real (non dry-run) `ta draft apply`.",
            candidates.len(),
            input.draft_id,
            input.status
        );
    }

    let mut ledger = ReplayLedger::open(&ta_dir)?;
    ensure_policy_files_parse(&ta_dir)?;
    let policies = ActionPolicies::load(&ta_dir.join("workflow.toml"));
    let constitution = PolicyConstitution::load(workspace_root);
    let mut session_limiter: Option<SessionRateLimiter> = None;
    // Executions counted during a dry run (no ledger writes happen then).
    let mut dry_run_counts: HashMap<String, u32> = HashMap::new();

    for action in candidates {
        let action_type = action
            .tool_name
            .strip_prefix(EXTERNAL_ACTION_PREFIX)
            .unwrap_or(&action.tool_name)
            .to_string();
        let outcome = replay_one(
            input,
            action,
            &action_type,
            registry,
            &policies,
            &constitution,
            &mut ledger,
            &mut session_limiter,
            &mut dry_run_counts,
            &ta_dir,
            opts,
        );
        log_outcome(input.draft_id, action, &action_type, &outcome);
        report.results.push(ActionReplayResult {
            action_id: action.action_id,
            action_type,
            description: action.description.clone(),
            outcome,
        });
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn replay_one(
    input: &DraftReplayInput,
    action: &PendingAction,
    action_type: &str,
    registry: &ActionRegistry,
    policies: &ActionPolicies,
    constitution: &PolicyConstitution,
    ledger: &mut ReplayLedger,
    session_limiter: &mut Option<SessionRateLimiter>,
    dry_run_counts: &mut HashMap<String, u32>,
    ta_dir: &Path,
    opts: ReplayOptions,
) -> ReplayOutcome {
    let draft_id = input.draft_id;

    if !action.tool_name.starts_with(EXTERNAL_ACTION_PREFIX) {
        return ReplayOutcome::Skipped {
            reason: format!(
                "'{}' is an intercepted MCP tool call, not a ta_external_action capture; TA \
                 has no executor to replay it. Re-run it by hand if it is still needed.",
                action.tool_name
            ),
        };
    }
    match action.disposition {
        ArtifactDisposition::Rejected => {
            return ReplayOutcome::Skipped {
                reason: "rejected by the reviewer".into(),
            }
        }
        ArtifactDisposition::Discuss => {
            return ReplayOutcome::Skipped {
                reason: "marked for discussion, not approved".into(),
            }
        }
        ArtifactDisposition::Pending if input.partial_review => {
            return ReplayOutcome::Skipped {
                reason: "draft was applied with partial selective review and this action \
                         was not explicitly approved"
                    .into(),
            }
        }
        _ => {}
    }
    if action.kind == ActionKind::ReadOnly {
        return ReplayOutcome::Skipped {
            reason: "read-only call; it already passed through at capture time".into(),
        };
    }

    if let Some(prev) = ledger.latest(draft_id, action.action_id) {
        match prev.state {
            LedgerState::Executed => return ReplayOutcome::AlreadyReplayed,
            LedgerState::Intent => return ReplayOutcome::OutcomeUnknown,
            state @ (LedgerState::Failed | LedgerState::Blocked | LedgerState::NoExecutor) => {
                if !opts.retry_failed {
                    return ReplayOutcome::PreviouslyFailed {
                        state,
                        detail: prev.detail.clone().unwrap_or_default(),
                    };
                }
            }
        }
    }

    let Some(executor) = registry.get(action_type) else {
        let registered: Vec<String> = registry.list().into_iter().map(|t| t.action_type).collect();
        return ReplayOutcome::Skipped {
            reason: format!(
                "unknown action type '{action_type}' (registered: {}). Install an adapter \
                 plugin declaring `verb:{action_type}` under .ta/plugins/adapter/, then run \
                 `ta draft replay-actions <draft-id>`.",
                registered.join(", ")
            ),
        };
    };

    let blocked = |ledger: &mut ReplayLedger, reason: String| -> ReplayOutcome {
        if !opts.dry_run {
            record_or_warn(
                ledger,
                draft_id,
                action,
                action_type,
                LedgerState::Blocked,
                &reason,
            );
        }
        ReplayOutcome::Blocked { reason }
    };

    if let Err(e) = executor.validate(&action.parameters) {
        return blocked(ledger, format!("payload no longer validates: {e}"));
    }

    match check_replay_policy(action_type, &action.parameters, policies, constitution) {
        Ok(warnings) => {
            for w in warnings {
                println!(
                    "  [warn] {} ({}): {}",
                    action_type,
                    short(action.action_id),
                    w
                );
            }
        }
        Err(reason) => return blocked(ledger, reason),
    }

    let cfg = policies.policy_for(action_type);
    if let Some(limit) = cfg.rate_limit {
        let already = ledger.executed_count(draft_id, action_type)
            + dry_run_counts.get(action_type).copied().unwrap_or(0);
        if already >= limit {
            return blocked(
                ledger,
                format!(
                    "rate_limit reached: {already} of {limit} '{action_type}' action(s) already \
                     executed for this draft ([actions.{action_type}].rate_limit)"
                ),
            );
        }
    }

    if opts.dry_run {
        *dry_run_counts.entry(action_type.to_string()).or_insert(0) += 1;
        return ReplayOutcome::WouldExecute;
    }

    if cfg.max_per_hour.is_some() || cfg.max_per_day.is_some() {
        let limiter = session_limiter.get_or_insert_with(|| SessionRateLimiter::new(ta_dir));
        match limiter.check_and_record(
            &replay_rate_bucket(action_type),
            cfg.max_per_hour,
            cfg.max_per_day,
        ) {
            SessionRateLimitResult::Allowed => {}
            SessionRateLimitResult::HourlyExceeded { limit, count } => {
                return blocked(
                    ledger,
                    format!(
                        "max_per_hour reached ({count} of {limit} '{action_type}' sends in the \
                         last hour)"
                    ),
                )
            }
            SessionRateLimitResult::DailyExceeded { limit, count } => {
                return blocked(
                    ledger,
                    format!(
                        "max_per_day reached ({count} of {limit} '{action_type}' sends in the \
                         last 24 hours)"
                    ),
                )
            }
        }
    }

    // At-most-once: the intent must be durable before anything can happen.
    if let Err(e) = ledger.record(entry(
        draft_id,
        action,
        action_type,
        LedgerState::Intent,
        None,
    )) {
        return ReplayOutcome::Failed {
            error: format!(
                "not executed: could not record intent in {} ({e}); executing without a \
                 durable record could cause a double send",
                ledger.path().display()
            ),
        };
    }

    let exec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.execute(&action.parameters)
    }));
    let (state, outcome) = match exec {
        Ok(Ok(result)) => {
            let mut detail = result.to_string();
            if detail.len() > 500 {
                let cut = (0..=500)
                    .rev()
                    .find(|i| detail.is_char_boundary(*i))
                    .unwrap_or(0);
                detail.truncate(cut);
                detail.push_str("...");
            }
            (
                (LedgerState::Executed, Some(detail)),
                ReplayOutcome::Executed,
            )
        }
        Ok(Err(ActionError::StubOnly(t))) => {
            let reason = format!(
                "no executor installed for '{t}' (only the built-in schema stub is \
                 registered), so nothing was sent. Install a plugin that implements it, then \
                 run `ta draft replay-actions <draft-id> --retry-failed`."
            );
            (
                (LedgerState::NoExecutor, Some(reason.clone())),
                ReplayOutcome::NoExecutor { reason },
            )
        }
        Ok(Err(e)) => {
            let error = e.to_string();
            (
                (LedgerState::Failed, Some(error.clone())),
                ReplayOutcome::Failed { error },
            )
        }
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            let error = format!("executor panicked: {msg}");
            (
                (LedgerState::Failed, Some(error.clone())),
                ReplayOutcome::Failed { error },
            )
        }
    };
    let (state, detail) = state;
    if let Err(e) = ledger.record(entry(draft_id, action, action_type, state, detail)) {
        // The intent is durable, so this action will be reported as
        // outcome-unknown next time and never re-sent automatically.
        tracing::warn!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            error = %e,
            path = %ledger.path().display(),
            "action replay: executed but could not record the outcome in the ledger; it \
             will show as outcome-unknown and will not be re-sent"
        );
    }
    outcome
}

fn entry(
    draft_id: Uuid,
    action: &PendingAction,
    action_type: &str,
    state: LedgerState,
    detail: Option<String>,
) -> LedgerEntry {
    LedgerEntry {
        draft_id,
        action_id: action.action_id,
        action_type: action_type.to_string(),
        state,
        detail,
        timestamp: Utc::now(),
    }
}

fn record_or_warn(
    ledger: &mut ReplayLedger,
    draft_id: Uuid,
    action: &PendingAction,
    action_type: &str,
    state: LedgerState,
    detail: &str,
) {
    if let Err(e) = ledger.record(entry(
        draft_id,
        action,
        action_type,
        state,
        Some(detail.to_string()),
    )) {
        tracing::warn!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            error = %e,
            path = %ledger.path().display(),
            "action replay: could not record outcome in the ledger"
        );
    }
}

fn log_outcome(draft_id: Uuid, action: &PendingAction, action_type: &str, outcome: &ReplayOutcome) {
    let label = outcome.label();
    let detail = outcome.detail().unwrap_or("");
    match outcome {
        ReplayOutcome::Failed { .. }
        | ReplayOutcome::Blocked { .. }
        | ReplayOutcome::OutcomeUnknown
        | ReplayOutcome::NoExecutor { .. } => tracing::warn!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            outcome = label,
            detail = %detail,
            "pending action replay"
        ),
        _ => tracing::info!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            outcome = label,
            detail = %detail,
            "pending action replay"
        ),
    }
}

fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

// ── Output ───────────────────────────────────────────────────────────────────

/// Print the per-action lines, a summary, and next steps.
pub(crate) fn print_report(report: &ReplayReport) {
    if report.results.is_empty() {
        return;
    }
    let draft = short(report.draft_id);
    println!();
    if report.dry_run {
        println!(
            "[actions] Dry run: {} pending action(s) in draft {} (nothing executed):",
            report.results.len(),
            draft
        );
    } else {
        println!(
            "[actions] Replaying {} approved pending action(s) from draft {}:",
            report.results.len(),
            draft
        );
    }
    for r in &report.results {
        let mut line = format!(
            "  [{}] {} ({}) {}",
            r.outcome.label(),
            r.action_type,
            short(r.action_id),
            r.description
        );
        if let Some(d) = r.outcome.detail() {
            if !d.is_empty() {
                line.push_str(&format!(" :: {d}"));
            }
        }
        println!("{line}");
    }

    let c = |p: fn(&ReplayOutcome) -> bool| report.count(p);
    let executed = c(|o| matches!(o, ReplayOutcome::Executed | ReplayOutcome::WouldExecute));
    let blocked = c(|o| matches!(o, ReplayOutcome::Blocked { .. }));
    let failed = c(|o| matches!(o, ReplayOutcome::Failed { .. }));
    let no_exec = c(|o| matches!(o, ReplayOutcome::NoExecutor { .. }));
    let skipped = c(|o| matches!(o, ReplayOutcome::Skipped { .. }));
    let already = c(|o| matches!(o, ReplayOutcome::AlreadyReplayed));
    let unknown = c(|o| matches!(o, ReplayOutcome::OutcomeUnknown));
    let prev_failed = c(|o| matches!(o, ReplayOutcome::PreviouslyFailed { .. }));
    println!(
        "[actions] {} {}, {} blocked, {} failed, {} no-executor, {} skipped, {} already \
         replayed, {} previously failed, {} outcome unknown.",
        executed,
        if report.dry_run {
            "would execute"
        } else {
            "executed"
        },
        blocked,
        failed,
        no_exec,
        skipped,
        already,
        prev_failed,
        unknown
    );
    if report.dry_run {
        println!(
            "  Next: run `ta draft replay-actions {}` to execute them for real.",
            draft
        );
    } else {
        println!("  Ledger: {}", report.ledger_path.display());
    }
    if failed + blocked + no_exec + prev_failed > 0 {
        println!(
            "  Next: fix the cause above, then retry deliberately with \
             `ta draft replay-actions {} --retry-failed` (failed actions may have partially \
             taken effect; check the destination first).",
            draft
        );
    }
    if unknown > 0 {
        println!(
            "  Next: {} action(s) were interrupted mid-execution in an earlier run. Check the \
             destination to see whether they took effect. TA will not re-send them \
             automatically (at-most-once).",
            unknown
        );
    }
}

// ── Entry points used by draft.rs ────────────────────────────────────────────

/// Environment variable automated callers set so that an apply they trigger
/// never executes external actions (equivalent to `--no-replay-actions`).
pub(crate) const NO_REPLAY_ENV: &str = "TA_NO_REPLAY_ACTIONS";

/// True when `TA_NO_REPLAY_ACTIONS` is set to anything other than empty/"0".
pub(crate) fn replay_disabled_by_env() -> bool {
    std::env::var(NO_REPLAY_ENV)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Build the same registry the gateway uses: built-in stubs plus every
/// discovered adapter-plugin verb.
fn build_registry(workspace_root: &Path) -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    for plugin_action in discover_adapter_actions(workspace_root) {
        registry.register(plugin_action);
    }
    registry
}

fn replayable_count(pkg: &DraftPackage) -> usize {
    pkg.changes
        .pending_actions
        .iter()
        .filter(|a| !a.tool_name.starts_with(PROPOSE_PREFIX))
        .count()
}

/// Called at the tail of `apply_package` (the apply lock is still held).
///
/// Never returns an error: the draft is already applied, so a replay problem
/// is reported, not propagated.
pub(crate) fn post_apply_hook(
    config: &GatewayConfig,
    package_id: Uuid,
    dry_run: bool,
    replay_enabled: bool,
) {
    let pkg = match super::load_package(config, package_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "  [actions] Could not reload draft {} to replay its pending actions: {}. \
                 Run `ta draft replay-actions {}` once the draft is readable.",
                short(package_id),
                e,
                short(package_id)
            );
            return;
        }
    };
    let n = replayable_count(&pkg);
    if n == 0 {
        return;
    }
    if !replay_enabled && !dry_run {
        println!();
        println!(
            "[actions] {} pending action(s) were NOT executed (--no-replay-actions). Run \
             `ta draft replay-actions {}` when you are ready to execute them.",
            n,
            short(package_id)
        );
        return;
    }
    let registry = build_registry(&config.workspace_root);
    let input = DraftReplayInput::from_package(&pkg);
    let opts = ReplayOptions {
        dry_run,
        retry_failed: false,
    };
    match replay_pending_actions(&config.workspace_root, &input, &registry, opts) {
        Ok(report) => print_report(&report),
        Err(e) => {
            tracing::error!(draft_id = %package_id, error = %e, "pending action replay aborted");
            eprintln!("  [actions] {e}");
        }
    }
}

/// `ta draft replay-actions <id> [--retry-failed]`: replay (or deliberately
/// retry) the pending actions of an already-applied draft.
pub(crate) fn replay_actions_command(
    config: &GatewayConfig,
    id: &str,
    retry_failed: bool,
) -> anyhow::Result<()> {
    let package_id = super::resolve_draft_id(id, config)?;
    let _lock = super::ApplyLock::acquire(&config.workspace_root, &package_id.to_string())?;
    let pkg = super::load_package(config, package_id)?;
    if !matches!(pkg.status, DraftStatus::Applied { .. }) {
        anyhow::bail!(
            "Draft {} is '{}', not applied. Pending actions only execute for applied drafts: \
             run `ta draft apply {}` first.",
            short(package_id),
            pkg.status,
            short(package_id)
        );
    }
    if replayable_count(&pkg) == 0 {
        println!(
            "[actions] Draft {} has no ta_external_action pending actions to replay.",
            short(package_id)
        );
        return Ok(());
    }
    let registry = build_registry(&config.workspace_root);
    let input = DraftReplayInput::from_package(&pkg);
    let report = replay_pending_actions(
        &config.workspace_root,
        &input,
        &registry,
        ReplayOptions {
            dry_run: false,
            retry_failed,
        },
    )?;
    print_report(&report);
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use ta_actions::ExternalAction;

    /// Test-only executor: counts executions, never does any I/O.
    struct FakeAction {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        mode: FakeMode,
    }

    #[derive(Clone, Copy)]
    enum FakeMode {
        Ok,
        Err,
        Panic,
    }

    impl ExternalAction for FakeAction {
        fn action_type(&self) -> &str {
            self.name
        }
        fn payload_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn validate(&self, payload: &Value) -> Result<(), ActionError> {
            if payload.get("invalid").is_some() {
                return Err(ActionError::Validation("invalid marker".into()));
            }
            Ok(())
        }
        fn execute(&self, _payload: &Value) -> Result<Value, ActionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                FakeMode::Ok => Ok(json!({"sent": true})),
                FakeMode::Err => Err(ActionError::Execution("smtp 550".into())),
                FakeMode::Panic => panic!("plugin exploded"),
            }
        }
    }

    fn registry_with(actions: Vec<(&'static str, FakeMode, Arc<AtomicUsize>)>) -> ActionRegistry {
        let mut r = ActionRegistry::new();
        for (name, mode, calls) in actions {
            r.register(Box::new(FakeAction { name, calls, mode }));
        }
        r
    }

    fn pending(tool_name: &str, params: Value) -> PendingAction {
        PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: tool_name.to_string(),
            parameters: params,
            kind: ActionKind::StateChanging,
            intercepted_at: Utc::now(),
            description: format!("test {tool_name}"),
            target_uri: None,
            disposition: ArtifactDisposition::Pending,
        }
    }

    fn applied_input(actions: Vec<PendingAction>) -> DraftReplayInput {
        DraftReplayInput {
            draft_id: Uuid::new_v4(),
            status: DraftStatus::Applied {
                applied_at: Utc::now(),
                applied_via: Default::default(),
            },
            pending_actions: actions,
            partial_review: false,
        }
    }

    fn email(to: &str) -> Value {
        json!({"to": to, "subject": "s", "body": "b"})
    }

    fn write_workflow(root: &Path, toml: &str) {
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        std::fs::write(root.join(".ta/workflow.toml"), toml).unwrap();
    }

    const LIVE: ReplayOptions = ReplayOptions {
        dry_run: false,
        retry_failed: false,
    };

    #[test]
    fn executes_once_on_apply_and_records_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);

        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        let ledger = ReplayLedger::open(&dir.path().join(".ta")).unwrap();
        let e = ledger
            .latest(input.draft_id, input.pending_actions[0].action_id)
            .unwrap();
        assert_eq!(e.state, LedgerState::Executed);
    }

    #[test]
    fn second_apply_does_not_re_execute() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);

        replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        let second = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        // Even a deliberate --retry-failed never re-sends an executed action.
        let third = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            ReplayOptions {
                dry_run: false,
                retry_failed: true,
            },
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.results[0].outcome, ReplayOutcome::AlreadyReplayed);
        assert_eq!(third.results[0].outcome, ReplayOutcome::AlreadyReplayed);
    }

    #[test]
    fn ledger_survives_process_restart() {
        let dir = tempfile::tempdir().unwrap();
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        {
            let calls = Arc::new(AtomicUsize::new(0));
            let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
            replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        // Fresh registry + fresh ledger read from disk, as a new process would.
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.results[0].outcome, ReplayOutcome::AlreadyReplayed);
    }

    #[test]
    fn dry_run_executes_nothing_and_writes_no_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let mut input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        // A dry-run apply never transitions the draft to Applied.
        input.status = DraftStatus::PendingReview;

        let report = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            ReplayOptions {
                dry_run: true,
                retry_failed: false,
            },
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.results[0].outcome, ReplayOutcome::WouldExecute);
        assert!(!dir.path().join(".ta").join(LEDGER_FILE).exists());
    }

    #[test]
    fn refuses_to_replay_a_draft_that_is_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        for status in [
            DraftStatus::PendingReview,
            DraftStatus::Denied {
                reason: "no".into(),
                denied_by: "me".into(),
            },
            DraftStatus::Approved {
                approved_by: "me".into(),
                approved_at: Utc::now(),
            },
        ] {
            let mut input =
                applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
            input.status = status;
            assert!(replay_pending_actions(dir.path(), &input, &reg, LIVE).is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn blocked_by_policy_at_replay_does_not_execute() {
        let dir = tempfile::tempdir().unwrap();
        write_workflow(
            dir.path(),
            "[actions.email]\npolicy = \"review\"\nallowed_recipients = [\"ok@x.com\"]\n\n\
             [actions.social_post]\npolicy = \"block\"\n",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![
            ("email", FakeMode::Ok, calls.clone()),
            ("social_post", FakeMode::Ok, calls.clone()),
        ]);
        let input = applied_input(vec![
            pending("ta_external_action:email", email("evil@x.com")),
            pending(
                "ta_external_action:social_post",
                json!({"platform": "x", "content": "hi"}),
            ),
            pending("ta_external_action:email", email("ok@x.com")),
        ]);

        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();

        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("allowed_recipients")
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("block")
        ));
        assert_eq!(report.results[2].outcome, ReplayOutcome::Executed);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the allowed email ran"
        );

        // Blocked is recorded and not auto-retried.
        let again = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            again.results[0].outcome,
            ReplayOutcome::PreviouslyFailed {
                state: LedgerState::Blocked,
                ..
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn constitution_block_rule_and_schema_drop_block_at_replay() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/constitution.toml"),
            "[[rules.block]]\naction_type = \"api_call\"\ncondition = \"always\"\n\
             message = \"no api calls\"\n",
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![
            ("api_call", FakeMode::Ok, calls.clone()),
            ("db_query", FakeMode::Ok, calls.clone()),
        ]);
        let input = applied_input(vec![
            pending(
                "ta_external_action:api_call",
                json!({"method": "POST", "url": "https://api.example.com"}),
            ),
            pending(
                "ta_external_action:db_query",
                json!({"query": "DROP TABLE users"}),
            ),
        ]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("no api calls")
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("Schema-altering")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn allowed_domains_and_rate_limit_enforced_at_replay() {
        let dir = tempfile::tempdir().unwrap();
        write_workflow(
            dir.path(),
            "[actions.api_call]\npolicy = \"review\"\nrate_limit = 1\n\
             allowed_domains = [\"*.example.com\"]\n",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("api_call", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![
            pending(
                "ta_external_action:api_call",
                json!({"method": "GET", "url": "https://evil.test/x"}),
            ),
            pending(
                "ta_external_action:api_call",
                json!({"method": "GET", "url": "https://api.example.com/a"}),
            ),
            pending(
                "ta_external_action:api_call",
                json!({"method": "GET", "url": "https://api.example.com/b"}),
            ),
        ]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("allowed_domains")
        ));
        assert_eq!(report.results[1].outcome, ReplayOutcome::Executed);
        assert!(matches!(
            &report.results[2].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("rate_limit")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn session_rate_limit_blocks_excess_sends() {
        let dir = tempfile::tempdir().unwrap();
        write_workflow(
            dir.path(),
            "[actions.email]\npolicy = \"review\"\nmax_per_hour = 1\n",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![
            pending("ta_external_action:email", email("a@x.com")),
            pending("ta_external_action:email", email("b@x.com")),
        ]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked { reason } if reason.contains("max_per_hour")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failure_is_recorded_and_others_continue() {
        let dir = tempfile::tempdir().unwrap();
        let ok_calls = Arc::new(AtomicUsize::new(0));
        let bad_calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![
            ("email", FakeMode::Err, bad_calls.clone()),
            ("social_post", FakeMode::Panic, bad_calls.clone()),
            ("api_call", FakeMode::Ok, ok_calls.clone()),
        ]);
        let input = applied_input(vec![
            pending("ta_external_action:email", email("a@x.com")),
            pending(
                "ta_external_action:social_post",
                json!({"platform": "x", "content": "hi"}),
            ),
            pending(
                "ta_external_action:api_call",
                json!({"method": "GET", "url": "https://a.test"}),
            ),
        ]);

        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();

        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Failed { error } if error.contains("smtp 550")
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Failed { error } if error.contains("panicked")
        ));
        assert_eq!(report.results[2].outcome, ReplayOutcome::Executed);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bad_calls.load(Ordering::SeqCst), 2);

        // Not auto-retried...
        let again = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            again.results[0].outcome,
            ReplayOutcome::PreviouslyFailed {
                state: LedgerState::Failed,
                ..
            }
        ));
        assert_eq!(bad_calls.load(Ordering::SeqCst), 2);

        // ...but a deliberate --retry-failed does retry (and only the failed ones).
        let retry = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            ReplayOptions {
                dry_run: false,
                retry_failed: true,
            },
        )
        .unwrap();
        assert_eq!(bad_calls.load(Ordering::SeqCst), 4);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 1);
        assert_eq!(retry.results[2].outcome, ReplayOutcome::AlreadyReplayed);
    }

    #[test]
    fn interrupted_intent_is_never_resent() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        // Simulate a crash after the intent was made durable but before the
        // outcome was recorded.
        let mut ledger = ReplayLedger::open(&dir.path().join(".ta")).unwrap();
        ledger
            .record(entry(
                input.draft_id,
                &input.pending_actions[0],
                "email",
                LedgerState::Intent,
                None,
            ))
            .unwrap();

        for retry_failed in [false, true] {
            let report = replay_pending_actions(
                dir.path(),
                &input,
                &reg,
                ReplayOptions {
                    dry_run: false,
                    retry_failed,
                },
            )
            .unwrap();
            assert_eq!(report.results[0].outcome, ReplayOutcome::OutcomeUnknown);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn corrupted_ledger_fails_closed_but_torn_final_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let ta = dir.path().join(".ta");
        std::fs::create_dir_all(&ta).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);

        std::fs::write(ta.join(LEDGER_FILE), "garbage\n{\"also\":\"bad\"}\n").unwrap();
        assert!(replay_pending_actions(dir.path(), &input, &reg, LIVE).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // A complete-but-unparseable final line (ends in a newline) is
        // corruption, not a torn append: fail closed.
        std::fs::write(ta.join(LEDGER_FILE), "{\"draft_id\":\"bad\"}\n").unwrap();
        assert!(replay_pending_actions(dir.path(), &input, &reg, LIVE).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // A torn final line (no newline) is tolerated, and truncated on the
        // next write so the ledger stays fully parseable afterwards.
        std::fs::write(ta.join(LEDGER_FILE), "{\"draft_id\":\"torn").unwrap();
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        let reopened = ReplayLedger::open(&ta).unwrap();
        assert_eq!(
            reopened
                .latest(input.draft_id, input.pending_actions[0].action_id)
                .unwrap()
                .state,
            LedgerState::Executed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ta_propose_actions_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        // Even if something registered an executor under the same name.
        let reg = registry_with(vec![
            ("ta_propose_task_update", FakeMode::Ok, calls.clone()),
            ("ta_propose_task_create", FakeMode::Ok, calls.clone()),
        ]);
        let input = applied_input(vec![
            pending("ta_propose_task_update", json!({"task_id": "t1"})),
            pending("ta_propose_task_create", json!({"title": "x"})),
        ]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(report.results.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!dir.path().join(".ta").join(LEDGER_FILE).exists());
    }

    #[test]
    fn unknown_action_type_and_raw_mcp_calls_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![
            pending("ta_external_action:teleport", json!({})),
            pending("gmail_send", json!({"to": "a@x.com"})),
        ]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Skipped { reason } if reason.contains("unknown action type 'teleport'")
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Skipped { reason } if reason.contains("intercepted MCP tool call")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stub_only_executor_is_reported_and_not_marked_executed() {
        let dir = tempfile::tempdir().unwrap();
        // Plain registry: email is the built-in schema stub.
        let reg = ActionRegistry::new();
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            ReplayOutcome::NoExecutor { .. }
        ));
        let ledger = ReplayLedger::open(&dir.path().join(".ta")).unwrap();
        assert_eq!(
            ledger
                .latest(input.draft_id, input.pending_actions[0].action_id)
                .unwrap()
                .state,
            LedgerState::NoExecutor
        );
    }

    #[test]
    fn rejected_discussed_and_partial_review_actions_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let mut rejected = pending("ta_external_action:email", email("a@x.com"));
        rejected.disposition = ArtifactDisposition::Rejected;
        let mut discuss = pending("ta_external_action:email", email("a@x.com"));
        discuss.disposition = ArtifactDisposition::Discuss;
        let input = applied_input(vec![rejected, discuss]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(report
            .results
            .iter()
            .all(|r| matches!(r.outcome, ReplayOutcome::Skipped { .. })));

        let mut approved = pending("ta_external_action:email", email("a@x.com"));
        approved.disposition = ArtifactDisposition::Approved;
        let mut partial = applied_input(vec![
            pending("ta_external_action:email", email("a@x.com")),
            approved,
        ]);
        partial.partial_review = true;
        let report = replay_pending_actions(dir.path(), &partial, &reg, LIVE).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            ReplayOutcome::Skipped { .. }
        ));
        assert_eq!(report.results[1].outcome, ReplayOutcome::Executed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unparseable_policy_files_fail_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);

        let dir = tempfile::tempdir().unwrap();
        write_workflow(dir.path(), "[actions.email]\npolicy = \"sometimes\"\n");
        let err = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap_err();
        assert!(err.to_string().contains("workflow.toml"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/constitution.toml"), "[[rules.block]\n").unwrap();
        let err = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap_err();
        assert!(err.to_string().contains("constitution.toml"), "{err}");

        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn invalid_payload_is_blocked_not_executed() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending(
            "ta_external_action:email",
            json!({"invalid": true}),
        )]);
        let report = replay_pending_actions(dir.path(), &input, &reg, LIVE).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            ReplayOutcome::Blocked { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// End-to-end fixture through the real `apply_package` hook and the real
    /// registry/plugin discovery path. The "external action" is a local
    /// adapter plugin that only appends a line to a file in the temp dir, so
    /// no email/HTTP is ever sent. Unix-only because it shells out to python3.
    #[cfg(unix)]
    fn e2e_fixture() -> E2e {
        use super::super::{
            approve_package, build_package, load_all_packages, load_package, save_package,
        };
        use ta_goal::GoalRunStore;

        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("README.md"), "# Original\n").unwrap();
        let mut config = GatewayConfig::for_project(project.path());
        config.credential_vault_use_keychain = false;

        crate::commands::goal::execute(
            &crate::commands::goal::GoalCommands::Start {
                title: "Replay e2e".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Replay approved pending actions".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goal = &goal_store.list().unwrap()[0];
        std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
        build_package(&config, &goal.goal_run_id.to_string(), "Test", false).unwrap();

        // Adapter plugin implementing verb `notify.send`: appends to a marker file.
        let marker = project.path().join("sent.log");
        let plugin_dir = project.path().join(".ta/plugins/adapter/notify");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let script = plugin_dir.join("notify.py");
        std::fs::write(
            &script,
            r#"
import json, sys
req = json.loads(sys.stdin.readline())
payload = req["params"]["payload"]
if req["method"] == "execute":
    with open(payload["marker"], "a") as f:
        f.write("sent\n")
    print(json.dumps({"ok": True, "result": {"status": "sent"}}))
elif req["method"] == "risk_score":
    print(json.dumps({"ok": True, "result": {"risk_score": 0, "confidence": 1.0}}))
else:
    print(json.dumps({"ok": False, "error": "unknown method"}))
"#,
        )
        .unwrap();
        let mut manifest = toml::Table::new();
        manifest.insert("name".into(), "notify".into());
        manifest.insert("type".into(), "adapter".into());
        manifest.insert("command".into(), "python3".into());
        manifest.insert(
            "args".into(),
            toml::Value::Array(vec![script.to_string_lossy().to_string().into()]),
        );
        manifest.insert(
            "capabilities".into(),
            toml::Value::Array(vec!["verb:notify.send".into()]),
        );
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let packages = load_all_packages(&config).unwrap();
        let mut pkg = load_package(&config, packages[0].package_id).unwrap();
        pkg.changes.pending_actions.push(pending(
            "ta_external_action:notify.send",
            json!({"marker": marker.to_string_lossy()}),
        ));
        pkg.changes
            .pending_actions
            .push(pending("ta_propose_task_update", json!({"task_id": "t1"})));
        save_package(&config, &pkg).unwrap();
        approve_package(&config, &pkg.package_id.to_string(), "tester", false).unwrap();
        E2e {
            _project: project,
            config,
            pkg,
            marker,
        }
    }

    #[cfg(unix)]
    struct E2e {
        _project: tempfile::TempDir,
        config: GatewayConfig,
        pkg: DraftPackage,
        marker: PathBuf,
    }

    #[cfg(unix)]
    impl E2e {
        fn id(&self) -> String {
            self.pkg.package_id.to_string()
        }

        fn apply(&self, dry_run: bool, replay: bool) {
            super::super::apply_package_with_replay(
                &self.config,
                &self.id(),
                None,
                false,
                false,
                false,
                true, // skip_verify
                dry_run,
                ta_workspace::ConflictResolution::Abort,
                super::super::SelectiveReviewPatterns::default(),
                None,
                false,
                false,
                false,
                false,
                replay,
            )
            .unwrap();
        }

        fn sends(&self) -> usize {
            std::fs::read_to_string(&self.marker)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }

        fn ledger(&self) -> ReplayLedger {
            ReplayLedger::open(&self.config.workspace_root.join(".ta")).unwrap()
        }
    }

    #[cfg(unix)]
    #[test]
    fn apply_package_replays_once_and_replay_command_never_resends() {
        let e = e2e_fixture();
        e.apply(false, true);
        assert_eq!(e.sends(), 1, "real apply executes the approved action once");

        replay_actions_command(&e.config, &e.id(), false).unwrap();
        replay_actions_command(&e.config, &e.id(), true).unwrap();
        assert_eq!(
            e.sends(),
            1,
            "replay-actions must never re-send an executed action"
        );

        let ledger = e.ledger();
        assert_eq!(
            ledger
                .latest(e.pkg.package_id, e.pkg.changes.pending_actions[0].action_id)
                .unwrap()
                .state,
            LedgerState::Executed
        );
        assert!(
            ledger
                .latest(e.pkg.package_id, e.pkg.changes.pending_actions[1].action_id)
                .is_none(),
            "ta_propose_* actions belong to their own replay"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_apply_executes_nothing_and_replay_command_runs_it_later() {
        let e = e2e_fixture();
        // `apply --dry-run` still copies files and marks the draft applied
        // (only VCS operations are simulated), so it must never execute.
        e.apply(true, true);
        assert_eq!(e.sends(), 0, "dry-run apply must not execute anything");
        assert!(e
            .ledger()
            .latest(e.pkg.package_id, e.pkg.changes.pending_actions[0].action_id)
            .is_none());

        replay_actions_command(&e.config, &e.id(), false).unwrap();
        assert_eq!(e.sends(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn no_replay_actions_flag_defers_execution() {
        let e = e2e_fixture();
        e.apply(false, false);
        assert_eq!(
            e.sends(),
            0,
            "--no-replay-actions must not execute anything"
        );
        replay_actions_command(&e.config, &e.id(), false).unwrap();
        replay_actions_command(&e.config, &e.id(), false).unwrap();
        assert_eq!(e.sends(), 1);
    }

    #[test]
    fn url_host_and_domain_matching() {
        assert_eq!(
            url_host("https://user@API.Example.com:8443/p?q").as_deref(),
            Some("api.example.com")
        );
        assert!(domain_matches("*.example.com", "api.example.com"));
        assert!(!domain_matches("*.example.com", "example.com.evil.test"));
        assert!(domain_matches("api.example.com", "api.example.com"));
        assert!(url_host("not a url").is_none());
    }
}
