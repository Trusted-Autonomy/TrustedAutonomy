// draft_action_replay.rs: carry out a draft's approved external actions on
// `ta draft apply`.
//
// Tracked as trustedautonomy-46. When an agent calls `ta_external_action` and
// the action type's policy is `review`, the gateway captures a `PendingAction`
// (tool_name `ta_external_action:<type>`) into the goal's draft package instead
// of executing it. This module is what makes those captures actually happen
// once a human applies the draft.
//
// There is one user-facing verb: `ta draft apply <id>`. It is safe to re-run.
// On a draft that is already applied it does not copy files again; it only
// works through the external actions that are still outstanding.
//
// Safety properties (these are irreversible real-world effects such as email):
//
// 1. Only a genuinely applied draft (`DraftStatus::Applied`) has its actions
//    carried out. `--dry-run` only previews. Not reachable from view/build/deny.
// 2. At-most-once: a durable, append-only ledger at
//    `.ta/action-replay-ledger.jsonl` (keyed by draft_id + action_id) is
//    checked before every execution. An `intent` record is written and fsynced
//    BEFORE calling the executor, and an outcome record after. A crash between
//    the two leaves only the intent: that action's outcome is unknown, and it
//    is never re-sent automatically. The human checks whether it went out and,
//    only if it did not, runs `ta draft apply <id> --resend <action-id>`. The
//    trade-off is deliberate: we would rather hold an action (and say so) than
//    send it twice.
//    Re-running `ta draft apply <id>` retries actions whose last outcome was
//    failed, blocked, or no-executor (re-checking policy first), and never
//    touches actions that already went out.
// 3. Policy is re-evaluated right before sending with the same inputs the
//    capture path uses (`.ta/workflow.toml` `[actions.<type>]`,
//    `EmailDispatchGuard`, `.ta/constitution.toml` rules,
//    `allowed_recipients`, `allowed_domains`, per-draft `rate_limit`,
//    cross-session `max_per_hour`/`max_per_day`). If either policy file exists
//    but does not parse, nothing is sent (the loaders would otherwise silently
//    fall back to permissive defaults).
// 4. Execution goes only through `ActionRegistry` (built-in stubs plus
//    discovered adapter plugins, exactly like the gateway).
// 5. Per-action failure (including a panicking plugin) never blocks the other
//    actions and never rolls back the applied draft. Every outcome is printed
//    in plain words with the single next command, and logged with structured
//    tracing fields.
// 6. Automated applies (workflow-graph auto-approve, governed workflows) go
//    through the named rule `automation_actions` via
//    [`automated_apply_may_run`], which today denies every action.
//
// `ta_propose_*` pending actions are owned by their own dedicated replay
// (`draft_task_replay.rs`) and are always excluded here so the two paths can
// never double-execute.
//
// Concurrency: everything here runs while holding `.ta/apply.lock`
// (`ApplyLock`), so two processes can never interleave ledger writes.

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

/// File name of the actions ledger under `.ta/`.
pub(crate) const LEDGER_FILE: &str = "action-replay-ledger.jsonl";
/// `tool_name` prefix the gateway uses for `ta_external_action` captures.
pub(crate) const EXTERNAL_ACTION_PREFIX: &str = "ta_external_action:";
/// `tool_name` prefix owned by the dedicated `ta_propose_*` replay paths.
pub(crate) const PROPOSE_PREFIX: &str = "ta_propose_";
/// Env equivalent of `ta draft apply --skip-actions`.
pub(crate) const SKIP_ACTIONS_ENV: &str = "TA_SKIP_ACTIONS";
/// Set by automated callers that spawn `ta draft apply` as a subprocess, so
/// the apply is subject to the `automation_actions` rule.
pub(crate) const AUTOMATED_APPLY_ENV: &str = "TA_APPLY_AUTOMATED";

/// Separate rate-limit bucket for real sends, so that limits configured with
/// `max_per_hour`/`max_per_day` govern how many actions actually execute,
/// independently of how many were merely proposed at capture time (which the
/// gateway records under the plain action type).
fn replay_rate_bucket(action_type: &str) -> String {
    format!("replay:{action_type}")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

// ── Automation rule ─────────────────────────────────────────────────────────

/// The `automation_actions` rule's current value. Today automated applies may
/// not carry out external actions at all. A compiled constitution can replace
/// [`automated_apply_may_run`] later without touching any caller.
pub(crate) const AUTOMATION_ACTIONS_RULE: &str = "deny";

/// Outcome of the `automation_actions` rule for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutomationDecision {
    Allow,
    Deny(String),
}

/// Rule `automation_actions = "deny"`: may an automated (non-human) apply carry
/// out this external action? Today: never.
pub(crate) fn automated_apply_may_run(
    draft_id: Uuid,
    _action: &PendingAction,
) -> AutomationDecision {
    match AUTOMATION_ACTIONS_RULE {
        "allow" => AutomationDecision::Allow,
        _ => AutomationDecision::Deny(format!(
            "automated apply: external actions need a human; run: ta draft apply {}",
            short(draft_id)
        )),
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// State recorded in the ledger for one (draft_id, action_id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LedgerState {
    /// About to call the executor. Written and fsynced before execution.
    Intent,
    /// Executor returned success.
    Executed,
    /// Executor returned an error (or panicked). Side effects are unknown.
    Failed,
    /// Policy refused the action. Nothing was executed.
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

/// Durable at-most-once ledger of carried-out actions.
pub(crate) struct ReplayLedger {
    path: PathBuf,
    latest: HashMap<(Uuid, Uuid), LedgerEntry>,
    executed_per_type: HashMap<(Uuid, String), u32>,
}

impl ReplayLedger {
    /// Open (or lazily create) the ledger under `ta_dir`.
    ///
    /// Fails closed: an unparseable line means the ledger can no longer be
    /// trusted to prevent double execution, so nothing may be sent. The only
    /// exception is a torn final line with no trailing newline (a crash
    /// mid-append), tolerated with a warning: intents are fsynced before any
    /// execution, so a torn line is either an intent whose action never ran, or
    /// an outcome whose durable intent remains (outcome unknown).
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
                "Could not read the actions ledger at {} ({}), so no external actions were \
                 carried out (the ledger is what prevents sending anything twice). Fix the \
                 file's permissions, then run: ta draft apply <draft-id>",
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
                        "ignoring torn final line in actions ledger (an interrupted \
                         append); it will be truncated on the next write"
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "The actions ledger at {} is corrupted at line {} ({}), so no external \
                         actions were carried out (the ledger is what prevents sending an email \
                         or other action twice). Repair or remove that line, then run: \
                         ta draft apply <draft-id>",
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
        // or an outcome record whose durable intent remains (outcome unknown).
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

/// The parts of a draft package the actions step needs.
#[derive(Debug, Clone)]
pub(crate) struct DraftReplayInput {
    pub draft_id: Uuid,
    pub status: DraftStatus,
    pub pending_actions: Vec<PendingAction>,
    /// True when the draft was applied with partial selective review (some
    /// artifacts rejected/discussed/left pending while others were approved).
    /// In that case only actions explicitly marked `approved` are carried out.
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

/// Knobs for one run over a draft's actions.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReplayOptions {
    /// Preview only: report what would happen, execute nothing, write no ledger.
    pub dry_run: bool,
    /// The apply was triggered by automation, not a human: every action goes
    /// through the `automation_actions` rule first.
    pub automated: bool,
    /// Action ids (full UUID or a prefix of at least 8 characters) the human
    /// has checked and wants sent again. Only valid for actions whose outcome
    /// is unknown (intent recorded, no outcome).
    pub resend: Vec<String>,
}

/// How `ta draft apply` should treat a draft's external actions.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApplyActions {
    /// `--skip-actions` (or `TA_SKIP_ACTIONS=1`): do not carry out any.
    pub skip: bool,
    /// Apply triggered by automation (`TA_APPLY_AUTOMATED=1` or the in-process
    /// workflow-graph auto-approve).
    pub automated: bool,
    /// `--resend <action-id>` values.
    pub resend: Vec<String>,
}

impl ApplyActions {
    /// Build from CLI flags, folding in the env equivalents.
    pub(crate) fn from_cli(skip: bool, automated: bool, resend: &[String]) -> Self {
        Self {
            skip: skip || env_flag(SKIP_ACTIONS_ENV),
            automated: automated || env_flag(AUTOMATED_APPLY_ENV),
            resend: resend.to_vec(),
        }
    }
}

/// Why an action was refused before sending, plus what the human can change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub reason: String,
    /// What to fix before re-running `ta draft apply <id>`. `None` means
    /// re-running will not help.
    pub fix: Option<String>,
}

fn refusal(reason: impl Into<String>, fix: Option<&str>) -> Refusal {
    Refusal {
        reason: reason.into(),
        fix: fix.map(String::from),
    }
}

const FIX_WORKFLOW: &str = "Edit .ta/workflow.toml";
const FIX_CONSTITUTION: &str = "Edit .ta/constitution.toml";

/// What happened to one pending action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplayOutcome {
    /// The executor ran and returned success.
    Executed,
    /// Dry run: this action would be carried out.
    WouldExecute,
    /// The ledger shows this action already went out. Not sent again.
    AlreadyReplayed,
    /// The ledger has an intent but no outcome (an earlier apply stopped while
    /// sending it). Never re-sent without an explicit `--resend`.
    OutcomeUnknown,
    /// The `automation_actions` rule refused it. Nothing executed.
    AutomationDenied { reason: String },
    /// Not attempted (rejected by reviewer, raw MCP call, ...).
    Skipped { reason: String, fix: Option<String> },
    /// Policy refused it. Nothing executed.
    Blocked(Refusal),
    /// Only a schema stub is registered; nothing executed.
    NoExecutor,
    /// The executor returned an error or panicked.
    Failed { error: String },
}

impl ReplayOutcome {
    fn label(&self) -> &'static str {
        match self {
            ReplayOutcome::Executed => "executed",
            ReplayOutcome::WouldExecute => "would-execute",
            ReplayOutcome::AlreadyReplayed => "already-done",
            ReplayOutcome::OutcomeUnknown => "outcome-unknown",
            ReplayOutcome::AutomationDenied { .. } => "automation-denied",
            ReplayOutcome::Skipped { .. } => "skipped",
            ReplayOutcome::Blocked(_) => "blocked",
            ReplayOutcome::NoExecutor => "no-executor",
            ReplayOutcome::Failed { .. } => "failed",
        }
    }

    fn detail(&self) -> String {
        match self {
            ReplayOutcome::AutomationDenied { reason } | ReplayOutcome::Skipped { reason, .. } => {
                reason.clone()
            }
            ReplayOutcome::Blocked(r) => r.reason.clone(),
            ReplayOutcome::Failed { error } => error.clone(),
            _ => String::new(),
        }
    }

    /// Still outstanding: a later `ta draft apply <id>` may carry it out.
    fn outstanding(&self) -> bool {
        matches!(
            self,
            ReplayOutcome::Blocked(Refusal { fix: Some(_), .. })
                | ReplayOutcome::Failed { .. }
                | ReplayOutcome::NoExecutor
                | ReplayOutcome::AutomationDenied { .. }
                | ReplayOutcome::WouldExecute
                | ReplayOutcome::Skipped { fix: Some(_), .. }
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActionReplayResult {
    pub action_id: Uuid,
    pub action_type: String,
    pub summary: String,
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

/// Plain one-phrase description of an action, e.g. "Email to bob@x.com".
fn describe(action_type: &str, payload: &serde_json::Value) -> String {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match action_type {
        "email" => format!("Email to {}", s("to")),
        "social_post" => format!("Post to {}", s("platform")),
        "api_call" => format!("{} request to {}", s("method").to_uppercase(), s("url")),
        "db_query" => "Database query".to_string(),
        other => format!("'{other}' action"),
    }
}

/// Past-tense verb for a successful action of this type.
fn done_verb(action_type: &str) -> &'static str {
    match action_type {
        "email" | "social_post" => "sent",
        _ => "carried out",
    }
}

// ── Policy re-evaluation ─────────────────────────────────────────────────────

/// Re-run the capture-time policy gates for one action. Returns warnings to
/// surface on success, or why the action must not be sent.
fn check_replay_policy(
    action_type: &str,
    payload: &serde_json::Value,
    policies: &ActionPolicies,
    constitution: &PolicyConstitution,
) -> Result<Vec<String>, Refusal> {
    let cfg = policies.policy_for(action_type);
    let mut warnings = Vec::new();

    if cfg.policy == ActionPolicy::Block {
        return Err(refusal(
            format!(
                "'{action_type}' actions are set to policy = \"block\" \
                 ([actions.{action_type}])"
            ),
            Some(FIX_WORKFLOW),
        ));
    }

    // A human approved this capture, so the effective policy is `review`
    // whenever the dispatch guard would have forced it there.
    let effective = match EmailDispatchGuard::new().enforce(action_type, &cfg.policy) {
        DispatchResult::Blocked { message } => return Err(refusal(message, Some(FIX_WORKFLOW))),
        DispatchResult::ForcedReview { .. } => ActionPolicy::Review,
        DispatchResult::Allowed => cfg.policy.clone(),
    };

    match constitution.check_action_policy(action_type, &effective) {
        Ok(()) => {}
        Err(v) if v.is_warn => warnings.push(v.message),
        Err(v) => {
            return Err(refusal(
                format!("constitution rule: {}", v.message),
                Some(FIX_CONSTITUTION),
            ))
        }
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
                return Err(refusal(
                    format!("recipient {r} not in allowed_recipients"),
                    Some(FIX_WORKFLOW),
                ));
            }
        }
    }

    if !cfg.allowed_domains.is_empty() {
        if let Some(url) = payload.get("url").and_then(|v| v.as_str()) {
            let host = url_host(url).unwrap_or_default();
            if !cfg.allowed_domains.iter().any(|d| domain_matches(d, &host)) {
                return Err(refusal(
                    format!("host {host} not in allowed_domains"),
                    Some(FIX_WORKFLOW),
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
            Err(v) => {
                return Err(refusal(
                    format!("constitution rule: {}", v.message),
                    Some(FIX_WORKFLOW),
                ))
            }
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
/// because a capture only queues an action for review. Before sending it would
/// fail open: a typo in `workflow.toml` would silently drop a `policy = "block"`
/// or an `allowed_recipients` list right before an irreversible send.
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
                "Could not read {} ({}), so no external actions were carried out: their \
                 policy must be checked before anything is sent. Fix the file, then run: \
                 ta draft apply <draft-id>",
                path.display(),
                e
            )
        })?;
        toml::from_str::<T>(&content).map_err(|e| {
            anyhow::anyhow!(
                "{} does not parse ({}), so no external actions were carried out: their \
                 policy must be checked before anything is sent. Fix the file, then run: \
                 ta draft apply <draft-id>",
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

// ── Core ─────────────────────────────────────────────────────────────────────

fn matches_resend(action_id: Uuid, wanted: &str) -> bool {
    let wanted = wanted.trim().to_ascii_lowercase();
    wanted.len() >= 8 && action_id.to_string().starts_with(&wanted)
}

/// Carry out the eligible pending actions of one draft through `registry`.
///
/// Returns `Err` only when nothing could safely be attempted at all (draft not
/// applied, ledger or policy file unreadable, invalid `--resend`). Per-action
/// problems are reported in the returned [`ReplayReport`] and never abort the
/// other actions.
pub(crate) fn replay_pending_actions(
    workspace_root: &Path,
    input: &DraftReplayInput,
    registry: &ActionRegistry,
    opts: &ReplayOptions,
) -> anyhow::Result<ReplayReport> {
    let ta_dir = workspace_root.join(".ta");
    let draft = short(input.draft_id);
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
                    "external actions: leaving ta_propose_* action to its dedicated replay"
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
        if let Some(r) = opts.resend.first() {
            anyhow::bail!("--resend {r}: draft {draft} has no external actions. Nothing was sent.");
        }
        return Ok(report);
    }

    if !opts.dry_run && !matches!(input.status, DraftStatus::Applied { .. }) {
        anyhow::bail!(
            "Not carrying out {} external action(s) for draft {}: the draft is '{}', not \
             applied. Run: ta draft apply {}",
            candidates.len(),
            draft,
            input.status,
            draft
        );
    }

    let mut ledger = ReplayLedger::open(&ta_dir)?;

    // --resend is only for actions whose outcome is unknown. Validate every
    // value up front so a typo can never send something unintended.
    for wanted in &opts.resend {
        let Some(action) = candidates
            .iter()
            .find(|a| matches_resend(a.action_id, wanted))
        else {
            anyhow::bail!(
                "--resend {wanted}: no external action in draft {draft} has that id (use the \
                 8-character id shown by ta draft apply). Nothing was sent."
            );
        };
        let state = ledger
            .latest(input.draft_id, action.action_id)
            .map(|e| e.state);
        if state != Some(LedgerState::Intent) {
            anyhow::bail!(
                "--resend {wanted}: that action is not waiting on a check (its last result: \
                 {}). --resend is only for actions whose outcome is unknown. Nothing was \
                 sent. To carry out outstanding actions, run: ta draft apply {draft}",
                match state {
                    None => "not attempted yet".to_string(),
                    Some(LedgerState::Executed) => "already went out".to_string(),
                    Some(s) => format!("{s:?}").to_lowercase(),
                }
            );
        }
    }

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
        let mut ctx = Ctx {
            input,
            registry,
            policies: &policies,
            constitution: &constitution,
            ledger: &mut ledger,
            session_limiter: &mut session_limiter,
            dry_run_counts: &mut dry_run_counts,
            ta_dir: &ta_dir,
            opts,
        };
        let outcome = replay_one(&mut ctx, action, &action_type);
        log_outcome(input.draft_id, action, &action_type, &outcome);
        report.results.push(ActionReplayResult {
            action_id: action.action_id,
            summary: if action.tool_name.starts_with(EXTERNAL_ACTION_PREFIX) {
                describe(&action_type, &action.parameters)
            } else {
                format!("'{}' tool call", action.tool_name)
            },
            action_type,
            outcome,
        });
    }
    Ok(report)
}

struct Ctx<'a> {
    input: &'a DraftReplayInput,
    registry: &'a ActionRegistry,
    policies: &'a ActionPolicies,
    constitution: &'a PolicyConstitution,
    ledger: &'a mut ReplayLedger,
    session_limiter: &'a mut Option<SessionRateLimiter>,
    dry_run_counts: &'a mut HashMap<String, u32>,
    ta_dir: &'a Path,
    opts: &'a ReplayOptions,
}

fn replay_one(ctx: &mut Ctx<'_>, action: &PendingAction, action_type: &str) -> ReplayOutcome {
    let draft_id = ctx.input.draft_id;

    if !action.tool_name.starts_with(EXTERNAL_ACTION_PREFIX) {
        return ReplayOutcome::Skipped {
            reason: "TA cannot re-run intercepted tool calls; run it by hand if you still \
                     need it"
                .into(),
            fix: None,
        };
    }
    match action.disposition {
        ArtifactDisposition::Rejected => {
            return ReplayOutcome::Skipped {
                reason: "the reviewer rejected it".into(),
                fix: None,
            }
        }
        ArtifactDisposition::Discuss => {
            return ReplayOutcome::Skipped {
                reason: "it is marked for discussion, not approved".into(),
                fix: None,
            }
        }
        ArtifactDisposition::Pending if ctx.input.partial_review => {
            return ReplayOutcome::Skipped {
                reason: "the draft was applied with selective review and this action was \
                         not explicitly approved"
                    .into(),
                fix: None,
            }
        }
        _ => {}
    }
    if action.kind == ActionKind::ReadOnly {
        return ReplayOutcome::Skipped {
            reason: "it is read-only and already ran when the agent called it".into(),
            fix: None,
        };
    }

    if let Some(prev) = ctx.ledger.latest(draft_id, action.action_id) {
        match prev.state {
            LedgerState::Executed => return ReplayOutcome::AlreadyReplayed,
            LedgerState::Intent => {
                let explicitly_resent = ctx
                    .opts
                    .resend
                    .iter()
                    .any(|w| matches_resend(action.action_id, w));
                if !explicitly_resent {
                    return ReplayOutcome::OutcomeUnknown;
                }
            }
            // A human re-ran `ta draft apply`: retry, re-checking policy below.
            LedgerState::Failed | LedgerState::Blocked | LedgerState::NoExecutor => {}
        }
    }

    if ctx.opts.automated {
        if let AutomationDecision::Deny(reason) = automated_apply_may_run(draft_id, action) {
            return ReplayOutcome::AutomationDenied { reason };
        }
    }

    let Some(executor) = ctx.registry.get(action_type) else {
        return ReplayOutcome::Skipped {
            reason: format!("no plugin handles '{action_type}' actions"),
            fix: Some(format!(
                "Install an adapter plugin declaring verb:{action_type} under \
                 .ta/plugins/adapter/"
            )),
        };
    };

    let dry_run = ctx.opts.dry_run;
    let blocked = |ledger: &mut ReplayLedger, r: Refusal| -> ReplayOutcome {
        if !dry_run {
            record_or_warn(
                ledger,
                draft_id,
                action,
                action_type,
                LedgerState::Blocked,
                &r.reason,
            );
        }
        ReplayOutcome::Blocked(r)
    };

    if let Err(e) = executor.validate(&action.parameters) {
        return blocked(
            ctx.ledger,
            refusal(format!("the captured request is invalid ({e})"), None),
        );
    }

    match check_replay_policy(
        action_type,
        &action.parameters,
        ctx.policies,
        ctx.constitution,
    ) {
        Ok(warnings) => {
            for w in warnings {
                println!("  Warning for action {}: {}", short(action.action_id), w);
            }
        }
        Err(r) => return blocked(ctx.ledger, r),
    }

    let cfg = ctx.policies.policy_for(action_type);
    if let Some(limit) = cfg.rate_limit {
        let already = ctx.ledger.executed_count(draft_id, action_type)
            + ctx.dry_run_counts.get(action_type).copied().unwrap_or(0);
        if already >= limit {
            return blocked(
                ctx.ledger,
                refusal(
                    format!(
                        "rate_limit reached ({already} of {limit} '{action_type}' actions \
                         already done for this draft)"
                    ),
                    Some(FIX_WORKFLOW),
                ),
            );
        }
    }

    if dry_run {
        *ctx.dry_run_counts
            .entry(action_type.to_string())
            .or_insert(0) += 1;
        return ReplayOutcome::WouldExecute;
    }

    if cfg.max_per_hour.is_some() || cfg.max_per_day.is_some() {
        let ta_dir = ctx.ta_dir;
        let limiter = ctx
            .session_limiter
            .get_or_insert_with(|| SessionRateLimiter::new(ta_dir));
        let wait = "Wait for the limit window to pass (or raise it in .ta/workflow.toml)";
        match limiter.check_and_record(
            &replay_rate_bucket(action_type),
            cfg.max_per_hour,
            cfg.max_per_day,
        ) {
            SessionRateLimitResult::Allowed => {}
            SessionRateLimitResult::HourlyExceeded { limit, count } => {
                return blocked(
                    ctx.ledger,
                    refusal(
                        format!("max_per_hour reached ({count} of {limit} in the last hour)"),
                        Some(wait),
                    ),
                )
            }
            SessionRateLimitResult::DailyExceeded { limit, count } => {
                return blocked(
                    ctx.ledger,
                    refusal(
                        format!("max_per_day reached ({count} of {limit} in the last 24 hours)"),
                        Some(wait),
                    ),
                )
            }
        }
    }

    // At-most-once: the intent must be durable before anything can happen.
    if let Err(e) = ctx.ledger.record(entry(
        draft_id,
        action,
        action_type,
        LedgerState::Intent,
        None,
    )) {
        return ReplayOutcome::Failed {
            error: format!(
                "not attempted: could not write to {} ({e}), and sending without that \
                 record could send it twice",
                ctx.ledger.path().display()
            ),
        };
    }

    let exec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.execute(&action.parameters)
    }));
    let (state, detail, outcome) = match exec {
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
            (LedgerState::Executed, Some(detail), ReplayOutcome::Executed)
        }
        Ok(Err(ActionError::StubOnly(t))) => (
            LedgerState::NoExecutor,
            Some(format!("no executor installed for '{t}'")),
            ReplayOutcome::NoExecutor,
        ),
        Ok(Err(e)) => {
            let error = e.to_string();
            (
                LedgerState::Failed,
                Some(error.clone()),
                ReplayOutcome::Failed { error },
            )
        }
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            let error = format!("the plugin crashed: {msg}");
            (
                LedgerState::Failed,
                Some(error.clone()),
                ReplayOutcome::Failed { error },
            )
        }
    };
    if let Err(e) = ctx
        .ledger
        .record(entry(draft_id, action, action_type, state, detail))
    {
        // The intent is durable, so this action will be reported as
        // outcome-unknown next time and never re-sent automatically.
        tracing::warn!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            error = %e,
            path = %ctx.ledger.path().display(),
            "external action ran but its outcome could not be recorded; it will show as \
             outcome-unknown and will not be re-sent automatically"
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
            "could not record external action outcome in the ledger"
        );
    }
}

fn log_outcome(draft_id: Uuid, action: &PendingAction, action_type: &str, outcome: &ReplayOutcome) {
    let label = outcome.label();
    let detail = outcome.detail();
    match outcome {
        ReplayOutcome::Failed { .. }
        | ReplayOutcome::Blocked(_)
        | ReplayOutcome::OutcomeUnknown
        | ReplayOutcome::NoExecutor
        | ReplayOutcome::AutomationDenied { .. } => tracing::warn!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            outcome = label,
            detail = %detail,
            "external action"
        ),
        _ => tracing::info!(
            draft_id = %draft_id,
            action_id = %action.action_id,
            action_type = %action_type,
            outcome = label,
            detail = %detail,
            "external action"
        ),
    }
}

fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

// ── Output ───────────────────────────────────────────────────────────────────

/// One plain sentence per action: what happened, or why not, and the single
/// next command if there is one.
pub(crate) fn describe_result(draft_id: Uuid, r: &ActionReplayResult) -> String {
    let draft = short(draft_id);
    let id = short(r.action_id);
    let what = &r.summary;
    let rerun = format!("ta draft apply {draft}");
    match &r.outcome {
        ReplayOutcome::Executed => format!("{what} was {}.", done_verb(&r.action_type)),
        ReplayOutcome::WouldExecute => format!(
            "{what} would be {} (dry run, nothing done). To do it, run: {rerun}",
            done_verb(&r.action_type)
        ),
        ReplayOutcome::AlreadyReplayed => format!(
            "{what} was already {} earlier; not doing it again.",
            done_verb(&r.action_type)
        ),
        ReplayOutcome::OutcomeUnknown => format!(
            "{what} may or may not have gone out: an earlier apply stopped while doing it \
             (action {id}). Check whether it went out. Only if it did not, run: \
             {rerun} --resend {id}"
        ),
        ReplayOutcome::AutomationDenied { reason } => format!("{what} was not done: {reason}"),
        ReplayOutcome::Skipped { reason, fix } => match fix {
            Some(fix) => format!("{what} was not done: {reason}. {fix}, then run: {rerun}"),
            None => format!("{what} was not done: {reason}."),
        },
        ReplayOutcome::Blocked(Refusal { reason, fix }) => match fix {
            Some(fix) => format!("{what} was blocked: {reason}. {fix}, then run: {rerun}"),
            None => format!("{what} was blocked: {reason}."),
        },
        ReplayOutcome::NoExecutor => format!(
            "{what} was not done: no plugin is installed that can carry out '{}' actions. \
             Install one, then run: {rerun}",
            r.action_type
        ),
        ReplayOutcome::Failed { error } => format!(
            "{what} failed: {error}. It may have partly gone through; check, then run: {rerun}"
        ),
    }
}

/// Print the per-action lines and a one-line summary.
pub(crate) fn print_report(report: &ReplayReport) {
    if report.results.is_empty() {
        return;
    }
    let draft = short(report.draft_id);
    println!();
    if report.dry_run {
        println!(
            "External actions in draft {} ({}; dry run, nothing done):",
            draft,
            report.results.len()
        );
    } else {
        println!(
            "External actions in draft {} ({}):",
            draft,
            report.results.len()
        );
    }
    for r in &report.results {
        println!("  {}", describe_result(report.draft_id, r));
    }
    let done = report.count(|o| matches!(o, ReplayOutcome::Executed));
    let already = report.count(|o| matches!(o, ReplayOutcome::AlreadyReplayed));
    let unknown = report.count(|o| matches!(o, ReplayOutcome::OutcomeUnknown));
    let outstanding = report.count(|o| o.outstanding());
    if report.dry_run {
        println!(
            "  {} would be done, {} already done, {} need a check.",
            report.count(|o| matches!(o, ReplayOutcome::WouldExecute)),
            already,
            unknown
        );
    } else {
        println!(
            "  {} done now, {} already done, {} still outstanding, {} need a check. \
             Record: {}",
            done,
            already,
            outstanding,
            unknown,
            report.ledger_path.display()
        );
    }
}

// ── Entry points used by draft.rs ────────────────────────────────────────────

/// Build the same registry the gateway uses: built-in stubs plus every
/// discovered adapter-plugin verb.
fn build_registry(workspace_root: &Path) -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    for plugin_action in discover_adapter_actions(workspace_root) {
        registry.register(plugin_action);
    }
    registry
}

/// Number of pending actions this module is responsible for.
pub(crate) fn external_action_count(pkg: &DraftPackage) -> usize {
    pkg.changes
        .pending_actions
        .iter()
        .filter(|a| !a.tool_name.starts_with(PROPOSE_PREFIX))
        .count()
}

fn run_for_package(
    config: &GatewayConfig,
    pkg: &DraftPackage,
    dry_run: bool,
    actions: &ApplyActions,
) -> anyhow::Result<()> {
    let n = external_action_count(pkg);
    if n == 0 {
        if let Some(r) = actions.resend.first() {
            anyhow::bail!(
                "--resend {r}: draft {} has no external actions. Nothing was sent.",
                short(pkg.package_id)
            );
        }
        return Ok(());
    }
    if actions.skip && !dry_run {
        println!();
        println!(
            "External actions: {} not done (--skip-actions). To do them, run: ta draft apply {}",
            n,
            short(pkg.package_id)
        );
        return Ok(());
    }
    let registry = build_registry(&config.workspace_root);
    let input = DraftReplayInput::from_package(pkg);
    let opts = ReplayOptions {
        dry_run,
        automated: actions.automated,
        resend: actions.resend.clone(),
    };
    let report = replay_pending_actions(&config.workspace_root, &input, &registry, &opts)?;
    print_report(&report);
    Ok(())
}

/// Called at the tail of a first `ta draft apply` (the apply lock is still
/// held). Never returns an error: the files are already applied, so a problem
/// with the actions is reported, not propagated.
pub(crate) fn post_apply_hook(
    config: &GatewayConfig,
    package_id: Uuid,
    dry_run: bool,
    actions: &ApplyActions,
) {
    let pkg = match super::load_package(config, package_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "Could not reload draft {} to carry out its external actions ({}). Run: \
                 ta draft apply {}",
                short(package_id),
                e,
                short(package_id)
            );
            return;
        }
    };
    if let Err(e) = run_for_package(config, &pkg, dry_run, actions) {
        tracing::error!(draft_id = %package_id, error = %e, "external actions not carried out");
        eprintln!("{e}");
    }
}

/// `ta draft apply <id>` on a draft that is already applied and has external
/// actions: files are not copied again; only outstanding actions are worked
/// through. Called with the apply lock held.
pub(crate) fn apply_actions_for_applied_draft(
    config: &GatewayConfig,
    pkg: &DraftPackage,
    dry_run: bool,
    actions: &ApplyActions,
) -> anyhow::Result<()> {
    println!(
        "Draft {} (\"{}\") is already applied; its files are not copied again. Checking its \
         external actions.",
        short(pkg.package_id),
        pkg.goal.title
    );
    run_for_package(config, pkg, dry_run, actions)
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

    fn live() -> ReplayOptions {
        ReplayOptions::default()
    }

    fn resend(id: Uuid) -> ReplayOptions {
        ReplayOptions {
            resend: vec![short(id)],
            ..Default::default()
        }
    }

    #[test]
    fn executes_once_on_apply_and_records_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);

        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        assert_eq!(
            describe_result(input.draft_id, &report.results[0]),
            "Email to a@x.com was sent."
        );
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

        replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        let second = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        let third = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();

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
            replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        // Fresh registry + fresh ledger read from disk, as a new process would.
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.results[0].outcome, ReplayOutcome::AlreadyReplayed);
    }

    #[test]
    fn dry_run_executes_nothing_and_writes_no_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let mut input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        input.status = DraftStatus::PendingReview;

        let report = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            &ReplayOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.results[0].outcome, ReplayOutcome::WouldExecute);
        assert!(!dir.path().join(".ta").join(LEDGER_FILE).exists());
    }

    #[test]
    fn refuses_a_draft_that_is_not_applied() {
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
            assert!(replay_pending_actions(dir.path(), &input, &reg, &live()).is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn blocked_by_policy_does_not_execute_and_is_retried_after_fix() {
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
            pending("ta_external_action:email", email("bob@x.com")),
            pending(
                "ta_external_action:social_post",
                json!({"platform": "x", "content": "hi"}),
            ),
            pending("ta_external_action:email", email("ok@x.com")),
        ]);

        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();

        let line = describe_result(input.draft_id, &report.results[0]);
        assert_eq!(
            line,
            format!(
                "Email to bob@x.com was blocked: recipient bob@x.com not in allowed_recipients. \
                 Edit .ta/workflow.toml, then run: ta draft apply {}",
                short(input.draft_id)
            )
        );
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("block")
        ));
        assert_eq!(report.results[2].outcome, ReplayOutcome::Executed);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the allowed email ran"
        );

        // Re-run with the same policy: re-checked, still blocked, nothing sent.
        let again = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert!(matches!(
            again.results[0].outcome,
            ReplayOutcome::Blocked(_)
        ));
        assert_eq!(again.results[2].outcome, ReplayOutcome::AlreadyReplayed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // After the human fixes the policy, re-running sends only the blocked ones.
        write_workflow(dir.path(), "[actions.email]\npolicy = \"review\"\n");
        let fixed = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(fixed.results[0].outcome, ReplayOutcome::Executed);
        assert_eq!(fixed.results[1].outcome, ReplayOutcome::Executed);
        assert_eq!(fixed.results[2].outcome, ReplayOutcome::AlreadyReplayed);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn constitution_block_rule_and_schema_drop_block() {
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("no api calls")
                && r.fix.as_deref() == Some(FIX_CONSTITUTION)
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("Schema-altering")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn allowed_domains_and_rate_limit_enforced() {
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("allowed_domains")
        ));
        assert_eq!(report.results[1].outcome, ReplayOutcome::Executed);
        assert!(matches!(
            &report.results[2].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("rate_limit")
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Blocked(r) if r.reason.contains("max_per_hour")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failure_is_recorded_others_continue_and_rerun_retries_only_failed() {
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

        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();

        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Failed { error } if error.contains("smtp 550")
        ));
        assert!(
            describe_result(input.draft_id, &report.results[0]).ends_with(&format!(
                "check, then run: ta draft apply {}",
                short(input.draft_id)
            ))
        );
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Failed { error } if error.contains("crashed")
        ));
        assert_eq!(report.results[2].outcome, ReplayOutcome::Executed);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bad_calls.load(Ordering::SeqCst), 2);

        // A human re-running `ta draft apply` retries only the failed ones.
        let retry = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(bad_calls.load(Ordering::SeqCst), 4);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 1);
        assert_eq!(retry.results[2].outcome, ReplayOutcome::AlreadyReplayed);
    }

    fn record_interrupted_intent(dir: &Path, input: &DraftReplayInput, idx: usize) {
        let mut ledger = ReplayLedger::open(&dir.join(".ta")).unwrap();
        ledger
            .record(entry(
                input.draft_id,
                &input.pending_actions[idx],
                "email",
                LedgerState::Intent,
                None,
            ))
            .unwrap();
    }

    #[test]
    fn outcome_unknown_is_never_auto_resent() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        // Simulate a crash after the intent was made durable but before the
        // outcome was recorded.
        record_interrupted_intent(dir.path(), &input, 0);

        for _ in 0..3 {
            let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
            assert_eq!(report.results[0].outcome, ReplayOutcome::OutcomeUnknown);
            let line = describe_result(input.draft_id, &report.results[0]);
            assert!(
                line.contains(&format!(
                    "run: ta draft apply {} --resend {}",
                    short(input.draft_id),
                    short(input.pending_actions[0].action_id)
                )),
                "{line}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resend_works_only_for_outcome_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![
            pending("ta_external_action:email", email("a@x.com")),
            pending("ta_external_action:email", email("b@x.com")),
        ]);
        let unknown_id = input.pending_actions[0].action_id;
        let fresh_id = input.pending_actions[1].action_id;
        record_interrupted_intent(dir.path(), &input, 0);

        // --resend on an action that was never attempted: refused, nothing sent.
        assert!(replay_pending_actions(dir.path(), &input, &reg, &resend(fresh_id)).is_err());
        // --resend with an id that matches nothing: refused, nothing sent.
        assert!(replay_pending_actions(dir.path(), &input, &reg, &resend(Uuid::new_v4())).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // --resend for the outcome-unknown one: sent once (other one runs normally).
        let report = replay_pending_actions(dir.path(), &input, &reg, &resend(unknown_id)).unwrap();
        assert_eq!(report.results[0].outcome, ReplayOutcome::Executed);
        assert_eq!(report.results[1].outcome, ReplayOutcome::Executed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Now it went out: --resend for it is refused, and plain re-runs skip it.
        assert!(replay_pending_actions(dir.path(), &input, &reg, &resend(unknown_id)).is_err());
        let again = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(again.results[0].outcome, ReplayOutcome::AlreadyReplayed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn resend_still_rechecks_policy() {
        let dir = tempfile::tempdir().unwrap();
        write_workflow(dir.path(), "[actions.email]\npolicy = \"block\"\n");
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        record_interrupted_intent(dir.path(), &input, 0);
        let report = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            &resend(input.pending_actions[0].action_id),
        )
        .unwrap();
        assert!(matches!(
            report.results[0].outcome,
            ReplayOutcome::Blocked(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn automated_apply_is_denied_by_the_automation_rule() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = registry_with(vec![("email", FakeMode::Ok, calls.clone())]);
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        assert_eq!(AUTOMATION_ACTIONS_RULE, "deny");

        let report = replay_pending_actions(
            dir.path(),
            &input,
            &reg,
            &ReplayOptions {
                automated: true,
                ..Default::default()
            },
        )
        .unwrap();
        let expected = format!(
            "automated apply: external actions need a human; run: ta draft apply {}",
            short(input.draft_id)
        );
        assert_eq!(
            report.results[0].outcome,
            ReplayOutcome::AutomationDenied {
                reason: expected.clone()
            }
        );
        assert!(describe_result(input.draft_id, &report.results[0]).ends_with(&expected));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // Nothing recorded, so the human's own apply carries it out.
        assert!(!dir.path().join(".ta").join(LEDGER_FILE).exists());
        replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
        assert!(replay_pending_actions(dir.path(), &input, &reg, &live()).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // A complete-but-unparseable final line (ends in a newline) is
        // corruption, not a torn append: fail closed.
        std::fs::write(ta.join(LEDGER_FILE), "{\"draft_id\":\"bad\"}\n").unwrap();
        assert!(replay_pending_actions(dir.path(), &input, &reg, &live()).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // A torn final line (no newline) is tolerated, and truncated on the
        // next write so the ledger stays fully parseable afterwards.
        std::fs::write(ta.join(LEDGER_FILE), "{\"draft_id\":\"torn").unwrap();
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert!(matches!(
            &report.results[0].outcome,
            ReplayOutcome::Skipped { reason, fix: Some(_) } if reason.contains("'teleport'")
        ));
        assert!(matches!(
            &report.results[1].outcome,
            ReplayOutcome::Skipped { reason, fix: None } if reason.contains("intercepted")
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stub_only_executor_is_reported_and_not_marked_executed() {
        let dir = tempfile::tempdir().unwrap();
        // Plain registry: email is the built-in schema stub.
        let reg = ActionRegistry::new();
        let input = applied_input(vec![pending("ta_external_action:email", email("a@x.com"))]);
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert_eq!(report.results[0].outcome, ReplayOutcome::NoExecutor);
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
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
        let report = replay_pending_actions(dir.path(), &partial, &reg, &live()).unwrap();
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
        let err = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap_err();
        assert!(err.to_string().contains("workflow.toml"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/constitution.toml"), "[[rules.block]\n").unwrap();
        let err = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap_err();
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
        let report = replay_pending_actions(dir.path(), &input, &reg, &live()).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            ReplayOutcome::Blocked(Refusal { fix: None, .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// End-to-end fixture through the real `ta draft apply` code path and the
    /// real registry/plugin discovery path. The "external actions" are a local
    /// adapter plugin that only appends a line to a file in the temp dir (or
    /// fails while a "fail" flag file exists), so no email/HTTP is ever sent.
    /// Unix-only because it shells out to python3.
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
                title: "Actions e2e".to_string(),
                source: Some(project.path().to_path_buf()),
                objective: "Carry out approved external actions".to_string(),
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

        // Adapter plugin implementing verb `notify.send`.
        let plugin_dir = project.path().join(".ta/plugins/adapter/notify");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let script = plugin_dir.join("notify.py");
        std::fs::write(
            &script,
            r#"
import json, os, sys
req = json.loads(sys.stdin.readline())
payload = req["params"]["payload"]
if req["method"] == "execute":
    if os.path.exists(payload["fail_flag"]):
        print(json.dumps({"ok": False, "error": "destination unavailable"}))
    else:
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

        let ok_marker = project.path().join("ok.log");
        let flaky_marker = project.path().join("flaky.log");
        let fail_flag = project.path().join("fail.flag");
        let packages = load_all_packages(&config).unwrap();
        let mut pkg = load_package(&config, packages[0].package_id).unwrap();
        pkg.changes.pending_actions.push(pending(
            "ta_external_action:notify.send",
            json!({"marker": ok_marker.to_string_lossy(), "fail_flag": "/nonexistent/flag"}),
        ));
        pkg.changes.pending_actions.push(pending(
            "ta_external_action:notify.send",
            json!({"marker": flaky_marker.to_string_lossy(), "fail_flag": fail_flag.to_string_lossy()}),
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
            ok_marker,
            flaky_marker,
            fail_flag,
        }
    }

    #[cfg(unix)]
    struct E2e {
        _project: tempfile::TempDir,
        config: GatewayConfig,
        pkg: DraftPackage,
        ok_marker: PathBuf,
        flaky_marker: PathBuf,
        fail_flag: PathBuf,
    }

    #[cfg(unix)]
    impl E2e {
        fn id(&self) -> String {
            self.pkg.package_id.to_string()
        }

        fn apply_with(&self, dry_run: bool, actions: ApplyActions) -> anyhow::Result<()> {
            super::super::apply_package_with_actions(
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
                &actions,
            )
        }

        fn apply(&self, dry_run: bool) {
            self.apply_with(dry_run, ApplyActions::default()).unwrap();
        }

        fn count(path: &Path) -> usize {
            std::fs::read_to_string(path)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }

        fn ok_sends(&self) -> usize {
            Self::count(&self.ok_marker)
        }

        fn flaky_sends(&self) -> usize {
            Self::count(&self.flaky_marker)
        }

        fn readme(&self) -> String {
            std::fs::read_to_string(self.config.workspace_root.join("README.md")).unwrap()
        }

        fn ledger(&self) -> ReplayLedger {
            ReplayLedger::open(&self.config.workspace_root.join(".ta")).unwrap()
        }
    }

    #[cfg(unix)]
    #[test]
    fn apply_executes_once_and_reapply_never_resends() {
        let e = e2e_fixture();
        e.apply(false);
        assert_eq!(
            e.ok_sends(),
            1,
            "real apply carries out the approved action"
        );
        assert_eq!(e.flaky_sends(), 1);

        e.apply(false);
        e.apply(false);
        assert_eq!(e.ok_sends(), 1, "re-applying never re-sends");
        assert_eq!(e.flaky_sends(), 1);

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
                .latest(e.pkg.package_id, e.pkg.changes.pending_actions[2].action_id)
                .is_none(),
            "ta_propose_* actions belong to their own replay"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reapply_retries_only_failed_actions_and_does_not_recopy_files() {
        let e = e2e_fixture();
        std::fs::write(&e.fail_flag, "x").unwrap();
        e.apply(false);
        assert_eq!(e.readme(), "# Updated\n");
        assert_eq!(e.ok_sends(), 1);
        assert_eq!(e.flaky_sends(), 0, "the flaky destination failed");
        assert_eq!(
            e.ledger()
                .latest(e.pkg.package_id, e.pkg.changes.pending_actions[1].action_id)
                .unwrap()
                .state,
            LedgerState::Failed
        );

        // The human edits the applied file afterwards; re-apply must not
        // overwrite it (files are not copied again).
        std::fs::write(e.config.workspace_root.join("README.md"), "# Human edit\n").unwrap();
        std::fs::remove_file(&e.fail_flag).unwrap();
        e.apply(false);
        assert_eq!(e.readme(), "# Human edit\n", "files must not be re-copied");
        assert_eq!(e.ok_sends(), 1, "already-sent action not re-sent");
        assert_eq!(e.flaky_sends(), 1, "failed action retried once");
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_apply_previews_only_and_a_later_apply_does_it() {
        let e = e2e_fixture();
        // `apply --dry-run` still copies files and marks the draft applied
        // (only VCS operations are simulated), but never carries out actions.
        e.apply(true);
        assert_eq!(
            e.ok_sends() + e.flaky_sends(),
            0,
            "dry run must not execute"
        );
        assert!(!e
            .config
            .workspace_root
            .join(".ta")
            .join(LEDGER_FILE)
            .exists());

        // A dry-run re-apply of the now-applied draft still only previews.
        e.apply(true);
        assert_eq!(e.ok_sends() + e.flaky_sends(), 0);

        e.apply(false);
        assert_eq!(e.ok_sends(), 1);
        assert_eq!(e.flaky_sends(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn skip_actions_and_automated_apply_defer_to_a_human_apply() {
        let e = e2e_fixture();
        e.apply_with(
            false,
            ApplyActions {
                skip: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(e.ok_sends(), 0, "--skip-actions must not execute anything");

        e.apply_with(
            false,
            ApplyActions {
                automated: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(e.ok_sends(), 0, "automated apply must not execute anything");

        e.apply(false);
        e.apply(false);
        assert_eq!(e.ok_sends(), 1);
        assert_eq!(e.flaky_sends(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn reapply_with_resend_only_resends_outcome_unknown() {
        let e = e2e_fixture();
        e.apply(false);
        assert_eq!(e.ok_sends(), 1);
        let sent_id = short(e.pkg.changes.pending_actions[0].action_id);
        // --resend for an action that already went out is refused.
        assert!(e
            .apply_with(
                false,
                ApplyActions {
                    resend: vec![sent_id],
                    ..Default::default()
                },
            )
            .is_err());
        assert_eq!(e.ok_sends(), 1);
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
