// advisor_agent.rs — Advisor agent spawner for governed interactive session (v0.15.19).
//
// `spawn_advisor_agent()` builds the context for an advisor run (draft summary,
// available tools, phase summary at milestone) and launches a short-lived
// `ta run --headless --persona advisor` subprocess. The subprocess uses
// `ta_ask_human` to converse with the human, then calls `ta draft approve` +
// `ta draft apply` (or `ta draft deny`). We poll draft status until it reaches
// a terminal state and return the outcome.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ta_changeset::draft_summary::DraftSummary;

use crate::phase_summary::PhaseSummary;
use crate::workflow_session::AdvisorSecurity;

/// Outcome reported by the advisor agent after the session item is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorOutcome {
    /// Human approved; draft was applied.
    Applied,
    /// Human declined; draft was denied.
    Denied,
    /// Advisor timed out before the human responded.
    TimedOut,
    /// Advisor subprocess failed to start or exited with an error.
    SpawnFailed { reason: String },
    /// Another advisor is already reviewing this draft (reviewer lock held).
    ReviewerBusy { active_advisor_goal_id: Uuid },
}

impl std::fmt::Display for AdvisorOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdvisorOutcome::Applied => write!(f, "applied"),
            AdvisorOutcome::Denied => write!(f, "denied"),
            AdvisorOutcome::TimedOut => write!(f, "timed_out"),
            AdvisorOutcome::SpawnFailed { reason } => write!(f, "spawn_failed: {}", reason),
            AdvisorOutcome::ReviewerBusy {
                active_advisor_goal_id,
            } => {
                write!(f, "reviewer_busy: {}", active_advisor_goal_id)
            }
        }
    }
}

/// Configuration for a single advisor agent invocation.
#[derive(Debug, Clone)]
pub struct AdvisorConfig {
    /// Workspace root (project directory, where `.ta/` lives).
    pub workspace_root: PathBuf,
    /// ID of the draft package to review.
    pub draft_id: Uuid,
    /// Session item title (shown in advisor greeting).
    pub item_title: String,
    /// Session ID (used in context file naming to avoid collisions).
    pub session_id: Uuid,
    /// Item ID (used in context file naming).
    pub item_id: Uuid,
    /// Advisor security level (controls available tools in the prompt).
    pub security: AdvisorSecurity,
    /// Optional persona name (references `.ta/personas/<name>.toml`).
    pub persona: Option<String>,
    /// Optional pre-built phase summary for milestone review.
    pub phase_summary: Option<PhaseSummary>,
    /// Optional pre-built draft summary for context injection (v0.17.0.3).
    ///
    /// When present, `build_advisor_context` pre-populates the advisor's CLAUDE.md
    /// with the supervisor verdict, file list, and decision log so the advisor can
    /// present a useful first message without calling `ta_draft_view`.
    pub draft_summary: Option<DraftSummary>,
    /// Timeout for the advisor conversation (default: 30 min).
    pub timeout: Duration,
}

impl AdvisorConfig {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        draft_id: Uuid,
        item_title: impl Into<String>,
        session_id: Uuid,
        item_id: Uuid,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            draft_id,
            item_title: item_title.into(),
            session_id,
            item_id,
            security: AdvisorSecurity::ReadOnly,
            persona: None,
            phase_summary: None,
            draft_summary: None,
            timeout: Duration::from_secs(30 * 60),
        }
    }

    pub fn with_security(mut self, security: AdvisorSecurity) -> Self {
        self.security = security;
        self
    }

    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.persona = Some(persona.into());
        self
    }

    pub fn with_phase_summary(mut self, summary: PhaseSummary) -> Self {
        self.phase_summary = Some(summary);
        self
    }

    pub fn with_draft_summary(mut self, summary: DraftSummary) -> Self {
        self.draft_summary = Some(summary);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Build the advisor context markdown injected into the advisor's CLAUDE.md.
pub fn build_advisor_context(config: &AdvisorConfig) -> String {
    let mut ctx = String::new();

    ctx.push_str("# Advisor Context\n\n");
    ctx.push_str(&format!(
        "You are the **advisor** for session item: **{}**\n\n",
        config.item_title
    ));
    ctx.push_str(
        "You are explicitly on the human's side. Your job is to look out for their interests:\n\
         - Present what changed clearly in plain English.\n\
         - Proactively flag risks, missing tests, or incomplete work.\n\
         - Advocate against applying a draft that looks wrong.\n\
         - Ask clarifying questions when something is ambiguous.\n\
         - When the human approves, call `ta draft approve` then `ta draft apply`.\n\
         - When the human declines, call `ta draft deny`.\n\n",
    );

    ctx.push_str(&format!("**Draft ID**: `{}`\n\n", config.draft_id));
    ctx.push_str("Use `ta_draft_view` for the full diff, `ta_fs_read` for file contents.\n\n");

    // Security level: list available tools.
    ctx.push_str("## Available Actions\n\n");
    match config.security {
        AdvisorSecurity::ReadOnly => {
            ctx.push_str(
                "Security level: **read_only**\n\
                 - You may answer questions and present diffs.\n\
                 - You may NOT start a goal or apply a draft autonomously.\n\
                 - When suggesting a follow-up, show the exact command for the human to run.\n\n",
            );
        }
        AdvisorSecurity::Suggest => {
            ctx.push_str(
                "Security level: **suggest**\n\
                 - You may present exact `ta run \"...\"` commands for the human to copy-paste.\n\
                 - The human must run any follow-up goals themselves.\n\n",
            );
        }
        AdvisorSecurity::Auto => {
            ctx.push_str(
                "Security level: **auto**\n\
                 - At ≥80% intent confidence, you may fire `ta run` directly.\n\
                 - You MUST call `ta_ask_human` first to confirm before applying.\n\
                 - Use `classify_intent()` to assess confidence before acting autonomously.\n\n",
            );
        }
    }

    // Restrictions — always emitted.
    ctx.push_str(
        "## Restrictions\n\n\
         - You may NOT write to `.ta/personas/` — this directory is governed read-only for agents. \
         Persona files can only be modified by humans via the normal `ta draft` workflow.\n\n",
    );

    // Pre-populated draft summary (v0.17.0.3) — removes the need for an initial ta_draft_view call.
    if let Some(ref ds) = config.draft_summary {
        ctx.push_str(&ds.render_markdown());
        ctx.push('\n');
    }

    // Phase summary if present.
    if let Some(ref ps) = config.phase_summary {
        ctx.push_str("## Phase Run Summary\n\n");
        ctx.push_str(&ps.render_terminal());
        ctx.push('\n');
    }

    // Conversation protocol — varies depending on whether draft summary is pre-loaded.
    if config.draft_summary.is_some() {
        ctx.push_str(
            "## Conversation Protocol\n\n\
             The draft summary is pre-loaded above. You can begin presenting to the human immediately.\n\
             1. Present: what changed, key decisions, any risks flagged, questions for the human.\n\
             2. Call `ta_ask_human(\"Here's what changed: [summary]. Any concerns before I apply?\")` \
                — use `response_hint: freeform`.\n\
             3. Interpret the human's response:\n\
                - \"apply\" / \"looks good\" → call `ta draft approve`, then `ta draft apply`, then exit.\n\
                - \"skip\" / \"don't apply\" → call `ta draft deny`, then exit.\n\
                - A modification request → present the `ta run \"...\"` command (or fire it in auto mode).\n\
                - A question → answer from the decision log and `ta_fs_read`, then loop back to step 1.\n\
             4. Never apply without explicit human approval (unless security = auto and confidence ≥ 80%).\n\
             5. For full file diffs, use `ta_draft_view` or `ta_fs_read` as needed.\n"
        );
    } else {
        ctx.push_str(
            "## Conversation Protocol\n\n\
             1. Call `ta_draft_view` to load the draft summary.\n\
             2. Present: what changed, key decisions, any risks flagged, questions for the human.\n\
             3. Call `ta_ask_human(\"Here's what changed: [summary]. Any concerns before I apply?\")` \
                — use `response_hint: freeform`.\n\
             4. Interpret the human's response:\n\
                - \"apply\" / \"looks good\" → call `ta draft approve`, then `ta draft apply`, then exit.\n\
                - \"skip\" / \"don't apply\" → call `ta draft deny`, then exit.\n\
                - A modification request → present the `ta run \"...\"` command (or fire it in auto mode).\n\
                - A question → answer from the decision log and `ta_fs_read`, then loop back to step 2.\n\
             5. Never apply without explicit human approval (unless security = auto and confidence ≥ 80%).\n"
        );
    }

    ctx
}

// ── Reviewer Lock ─────────────────────────────────────────────────────────────

/// RAII guard that holds `.ta/drafts/<draft_id>.reviewer.lock`.
///
/// Deletes the lock file when dropped. Created by [`acquire_reviewer_lock`].
#[derive(Debug)]
pub struct ReviewerLock {
    path: PathBuf,
}

impl Drop for ReviewerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Attempt to acquire the reviewer lock for `draft_id` using O_CREAT|O_EXCL semantics.
///
/// Creates `.ta/drafts/<draft_id>.reviewer.lock` containing `{"advisor_goal_id":"<uuid>"}`.
/// Returns `Ok(ReviewerLock)` on success.
/// Returns `Err(active_goal_id)` if the lock already exists (another advisor is active).
///
/// The lock is TOCTOU-safe because `create_new(true)` maps to `O_CREAT|O_EXCL` on
/// POSIX systems and `CREATE_NEW` on Windows — both are atomic at the filesystem level.
pub fn acquire_reviewer_lock(
    workspace_root: &Path,
    draft_id: Uuid,
    advisor_goal_id: Uuid,
) -> Result<ReviewerLock, Uuid> {
    let drafts_dir = workspace_root.join(".ta").join("drafts");
    let lock_path = drafts_dir.join(format!("{}.reviewer.lock", draft_id));

    // Ensure the directory exists.
    if let Err(e) = std::fs::create_dir_all(&drafts_dir) {
        tracing::warn!(draft_id = %draft_id, error = %e, "Failed to create drafts dir for reviewer lock");
    }

    let content = serde_json::json!({ "advisor_goal_id": advisor_goal_id }).to_string();

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL — atomic
        .open(&lock_path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            let _ = file.write_all(content.as_bytes());
            tracing::debug!(
                draft_id = %draft_id,
                advisor_goal_id = %advisor_goal_id,
                "Acquired reviewer lock"
            );
            Ok(ReviewerLock { path: lock_path })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Read the existing lock to extract the active goal ID.
            let active_id = std::fs::read_to_string(&lock_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| {
                    v.get("advisor_goal_id")
                        .and_then(|id| id.as_str())
                        .map(String::from)
                })
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or(Uuid::nil());
            tracing::debug!(
                draft_id = %draft_id,
                active_goal_id = %active_id,
                "Reviewer lock busy"
            );
            Err(active_id)
        }
        Err(e) => {
            tracing::warn!(draft_id = %draft_id, error = %e, "Unexpected error acquiring reviewer lock");
            // Treat unexpected errors as busy to avoid racing.
            Err(Uuid::nil())
        }
    }
}

/// Write the advisor context to `.ta/advisor/<item_id>/context.md` in the workspace.
///
/// Returns the path to the written context file.
pub fn write_advisor_context(config: &AdvisorConfig) -> std::io::Result<PathBuf> {
    let advisor_dir = config
        .workspace_root
        .join(".ta")
        .join("advisor")
        .join(config.item_id.to_string());
    std::fs::create_dir_all(&advisor_dir)?;

    let context_path = advisor_dir.join("context.md");
    let content = build_advisor_context(config);
    std::fs::write(&context_path, content)?;
    Ok(context_path)
}

/// Spawn an advisor agent for the given session item.
///
/// Launches `ta run --headless` as a subprocess with:
/// - `--objective-file <path>` pointing at the context markdown (the real
///   delivery mechanism -- read directly into the spawned agent's objective)
/// - `TA_ADVISOR_DRAFT_ID=<id>` / `TA_ADVISOR_CONTEXT_FILE=<path>` /
///   `TA_ADVISOR_SECURITY`/`TA_ADVISOR_SESSION_ID`/`TA_ADVISOR_ITEM_ID`
///   environment variables, for any future consumer that reads them directly
/// - `--persona advisor` (or the configured persona)
///
/// Returns the advisor goal run ID extracted from stdout.
pub fn spawn_advisor_agent(config: &AdvisorConfig, ta_bin: &Path) -> Result<Uuid, String> {
    let context_path = write_advisor_context(config)
        .map_err(|e| format!("Failed to write advisor context: {}", e))?;

    let persona = config.persona.as_deref().unwrap_or("advisor");
    let goal_title = format!("Advisor: review session item '{}'", config.item_title);

    let mut cmd = std::process::Command::new(ta_bin);
    cmd.args([
        "--project-root",
        &config.workspace_root.to_string_lossy(),
        "run",
        &goal_title,
        "--objective-file",
        &context_path.to_string_lossy(),
        "--headless",
        "--no-version-check",
        "--persona",
        persona,
    ]);
    cmd.env("TA_ADVISOR_DRAFT_ID", config.draft_id.to_string());
    // Also set the env var for any future consumer that reads it directly --
    // --objective-file above is the real, working delivery mechanism.
    // Previously this env var was the *only* attempt to deliver context and
    // nothing in the codebase reads it, so the advisor agent never actually
    // saw `context_path`'s content (found 2026-07-11 while building
    // v0.17.0.12.26's ta_human_verify, which mirrored this same gap).
    cmd.env("TA_ADVISOR_CONTEXT_FILE", &context_path);
    cmd.env("TA_ADVISOR_SECURITY", config.security.to_string());
    cmd.env("TA_ADVISOR_SESSION_ID", config.session_id.to_string());
    cmd.env("TA_ADVISOR_ITEM_ID", config.item_id.to_string());

    tracing::info!(
        draft_id = %config.draft_id,
        item = %config.item_title,
        security = %config.security,
        "Spawning advisor agent"
    );

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ta run: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(format!(
            "ta run --headless exited {} for advisor goal.\nstdout: {}\nstderr: {}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        ));
    }

    // Extract goal_id from stdout (emitted by ta run on spawn).
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(id_str) = line.strip_prefix("goal_id: ") {
            let id_str = id_str.trim();
            return Uuid::parse_str(id_str)
                .map_err(|e| format!("Failed to parse advisor goal_id '{}': {}", id_str, e));
        }
    }

    Err(format!(
        "Advisor subprocess exited successfully but did not emit goal_id.\n\
         stdout: {}\nstderr: {}",
        stdout.trim(),
        stderr.trim()
    ))
}

/// Check a draft file for a terminal status.
///
/// Returns `Some(outcome)` when the file exists and contains `applied` or `denied`.
fn check_draft_terminal(draft_file: &Path, draft_id: Uuid) -> Option<AdvisorOutcome> {
    match std::fs::read_to_string(draft_file) {
        Ok(content) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                let status_key = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
                match status_key {
                    "applied" => return Some(AdvisorOutcome::Applied),
                    "denied" => return Some(AdvisorOutcome::Denied),
                    _ => {}
                }
                if v.get("applied").is_some() {
                    return Some(AdvisorOutcome::Applied);
                }
                if v.get("denied").is_some() {
                    return Some(AdvisorOutcome::Denied);
                }
                // Heuristic fallback for bare string values.
                if content.contains("\"applied\"") {
                    return Some(AdvisorOutcome::Applied);
                }
                if content.contains("\"denied\"") {
                    return Some(AdvisorOutcome::Denied);
                }
            }
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(draft_id = %draft_id, "Draft file not found yet, waiting...");
            None
        }
        Err(e) => {
            tracing::warn!(draft_id = %draft_id, error = %e, "Error reading draft file");
            None
        }
    }
}

/// Poll the draft status until it reaches a terminal state (Applied or Denied).
///
/// Preferred path: watches `.ta/drafts/<id>.json` with `notify` for modify/create
/// events — no spin-sleep. Falls back to `poll_interval` (min 500 ms) sleep-polling
/// when a file watcher cannot be established.
pub fn poll_draft_outcome(
    workspace_root: &Path,
    draft_id: Uuid,
    timeout: Duration,
    poll_interval: Duration,
) -> AdvisorOutcome {
    let drafts_dir = workspace_root.join(".ta").join("drafts");
    let draft_file = drafts_dir.join(format!("{}.json", draft_id));
    let deadline = Instant::now() + timeout;

    // Check immediately before setting up any watcher.
    if let Some(outcome) = check_draft_terminal(&draft_file, draft_id) {
        return outcome;
    }

    if Instant::now() >= deadline {
        tracing::warn!(
            draft_id = %draft_id,
            timeout_secs = timeout.as_secs(),
            "Advisor timed out waiting for draft outcome"
        );
        return AdvisorOutcome::TimedOut;
    }

    // Attempt event-driven watching on the drafts directory.
    match try_watch_draft(&drafts_dir) {
        Ok((mut _watcher, rx)) => {
            tracing::debug!(draft_id = %draft_id, "Using file-watcher for draft polling");
            loop {
                if Instant::now() >= deadline {
                    tracing::warn!(
                        draft_id = %draft_id,
                        timeout_secs = timeout.as_secs(),
                        "Advisor timed out waiting for draft outcome"
                    );
                    return AdvisorOutcome::TimedOut;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(remaining.min(Duration::from_secs(5))) {
                    Ok(_event) => {
                        if let Some(outcome) = check_draft_terminal(&draft_file, draft_id) {
                            return outcome;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // Periodic check to catch any events the watcher may have missed.
                        if let Some(outcome) = check_draft_terminal(&draft_file, draft_id) {
                            return outcome;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        tracing::warn!(draft_id = %draft_id, "File watcher channel disconnected, falling back to poll");
                        break;
                    }
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                draft_id = %draft_id,
                error = %e,
                "File watcher unavailable, falling back to 500 ms poll"
            );
        }
    }

    // Fallback: sleep-based polling (at least 500 ms between checks).
    let effective_interval = poll_interval.max(Duration::from_millis(500));
    loop {
        if Instant::now() >= deadline {
            tracing::warn!(
                draft_id = %draft_id,
                timeout_secs = timeout.as_secs(),
                "Advisor timed out waiting for draft outcome"
            );
            return AdvisorOutcome::TimedOut;
        }

        std::thread::sleep(effective_interval);

        if let Some(outcome) = check_draft_terminal(&draft_file, draft_id) {
            return outcome;
        }
    }
}

/// Attempt to set up a file-system watcher on `dir`.
///
/// Returns the watcher (must be kept alive) and the event receiver.
/// Returns `Err` if the watcher cannot be established (platform unsupported, etc.).
fn try_watch_draft(
    dir: &Path,
) -> Result<
    (
        notify::RecommendedWatcher,
        std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    ),
    notify::Error,
> {
    use notify::Watcher;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )?;

    if dir.exists() {
        watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
    }

    Ok((watcher, rx))
}

/// Minimum intent confidence (0-100) required for `advisor_security = "auto"` to
/// fire `ta draft apply` without explicit human approval. Matches the contract
/// advertised to the advisor in `build_advisor_context` ("At ≥80% intent
/// confidence, you may fire `ta run` directly").
const AUTO_APPLY_MIN_CONFIDENCE_PCT: u8 = 80;

/// Constitution guard: verify that auto-apply is permitted by the project constitution.
///
/// Blocks `ta draft apply` unless either:
/// 1. The human sent an explicit approval message, or
/// 2. `advisor_security = "auto"` is configured AND the shared Decision gate
///    (`ta-decision::decide`) judges `confidence_pct` sufficient — `Auto` is no
///    longer an unconditional bypass (v0.17.0.12.15). Missing confidence is
///    treated as 0% (withhold), not as an implicit pass.
pub fn check_advisor_auto_approve(
    security: &AdvisorSecurity,
    human_approved_explicitly: bool,
    confidence_pct: Option<u8>,
) -> Result<(), String> {
    if human_approved_explicitly {
        return Ok(());
    }
    match security {
        AdvisorSecurity::Auto => {
            let confidence = f64::from(confidence_pct.unwrap_or(0)) / 100.0;
            let thresholds = ta_decision::DecisionThresholds {
                min_confidence: f64::from(AUTO_APPLY_MIN_CONFIDENCE_PCT) / 100.0,
                // This guard only judges intent confidence — draft risk is
                // already enforced separately at the `ta draft apply` gate
                // (v0.17.0.12.15 item 1) — so risk never overrides here.
                max_risk_score: 100,
                escalate_risk_score: u32::MAX,
            };
            let decision = ta_decision::decide(
                &ta_decision::DecisionInput {
                    verdict: ta_decision::Verdict::Pass,
                    risk_score: 0,
                    confidence,
                },
                &thresholds,
            );
            if decision.is_auto_approvable() {
                Ok(())
            } else {
                Err(format!(
                    "Constitution guard: auto-apply withheld. advisor_security = \"auto\" \
                     requires \u{2265}{}% intent confidence to fire without explicit human \
                     approval; this action reported {:.0}%. Ask the human to confirm \
                     explicitly, or have the advisor re-assess confidence before retrying.",
                    AUTO_APPLY_MIN_CONFIDENCE_PCT,
                    confidence * 100.0
                ))
            }
        }
        AdvisorSecurity::ReadOnly | AdvisorSecurity::Suggest => {
            Err("Constitution guard: auto-apply blocked. \
             The advisor may not call 'ta draft apply' without explicit human approval \
             unless advisor_security = \"auto\" is configured. \
             Set `advisor_security = \"auto\"` in .ta/workflow.toml to enable autonomous apply."
                .to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase_summary::{PhaseRecord, PhaseSummary};
    use ta_changeset::draft_summary::{ConstitutionSignal, DraftSummary, FileDiff};
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> AdvisorConfig {
        AdvisorConfig::new(
            tmp.path(),
            Uuid::new_v4(),
            "Implement feature X",
            Uuid::new_v4(),
            Uuid::new_v4(),
        )
    }

    #[test]
    fn build_advisor_context_read_only() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("read_only"));
        assert!(ctx.contains("Implement feature X"));
        assert!(ctx.contains("ta draft approve"));
        assert!(ctx.contains("ta draft deny"));
        assert!(ctx.contains("ta_ask_human"));
    }

    #[test]
    fn build_advisor_context_auto_security() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp).with_security(AdvisorSecurity::Auto);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("≥80% intent confidence"));
        assert!(ctx.contains("auto"));
    }

    #[test]
    fn build_advisor_context_includes_phase_summary() {
        let tmp = TempDir::new().unwrap();
        let mut ps = PhaseSummary::new();
        ps.add_phase(PhaseRecord::new("v0.15.14").with_decision("tokio spawn"));
        let config = make_config(&tmp).with_phase_summary(ps);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("Phase Run Summary"));
        assert!(ctx.contains("v0.15.14"));
    }

    #[test]
    fn write_advisor_context_creates_file() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let path = write_advisor_context(&config).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Advisor Context"));
    }

    #[test]
    fn constitution_guard_allows_explicit_approval() {
        assert!(check_advisor_auto_approve(&AdvisorSecurity::ReadOnly, true, None).is_ok());
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Suggest, true, None).is_ok());
    }

    #[test]
    fn constitution_guard_blocks_without_approval_in_read_only() {
        let result = check_advisor_auto_approve(&AdvisorSecurity::ReadOnly, false, Some(100));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Constitution guard"));
    }

    #[test]
    fn constitution_guard_allows_auto_security_with_sufficient_confidence() {
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Auto, false, Some(80)).is_ok());
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Auto, false, Some(95)).is_ok());
    }

    /// v0.17.0.12.15: `Auto` must no longer be an unconditional bypass — below
    /// the 80% intent-confidence contract, it must withhold approval.
    #[test]
    fn constitution_guard_withholds_auto_security_with_low_confidence() {
        let result = check_advisor_auto_approve(&AdvisorSecurity::Auto, false, Some(50));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("80"));
    }

    #[test]
    fn constitution_guard_withholds_auto_security_with_missing_confidence() {
        // No confidence reported — must not be treated as an implicit pass.
        let result = check_advisor_auto_approve(&AdvisorSecurity::Auto, false, None);
        assert!(result.is_err());
    }

    #[test]
    fn constitution_guard_explicit_approval_overrides_low_confidence() {
        assert!(check_advisor_auto_approve(&AdvisorSecurity::Auto, true, Some(0)).is_ok());
    }

    #[test]
    fn advisor_outcome_display() {
        assert_eq!(AdvisorOutcome::Applied.to_string(), "applied");
        assert_eq!(AdvisorOutcome::Denied.to_string(), "denied");
        assert_eq!(AdvisorOutcome::TimedOut.to_string(), "timed_out");
        assert_eq!(
            AdvisorOutcome::SpawnFailed {
                reason: "no binary".to_string()
            }
            .to_string(),
            "spawn_failed: no binary"
        );
        let busy_id = Uuid::nil();
        assert_eq!(
            AdvisorOutcome::ReviewerBusy {
                active_advisor_goal_id: busy_id
            }
            .to_string(),
            format!("reviewer_busy: {}", busy_id)
        );
    }

    // ── Reviewer lock tests ────────────────────────────────────────────────────

    #[test]
    fn reviewer_lock_is_exclusive() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta/drafts")).unwrap();
        let draft_id = Uuid::new_v4();
        let goal_id_1 = Uuid::new_v4();
        let goal_id_2 = Uuid::new_v4();

        // First acquire succeeds.
        let lock1 = acquire_reviewer_lock(tmp.path(), draft_id, goal_id_1).unwrap();

        // Second acquire fails (lock is held).
        let result = acquire_reviewer_lock(tmp.path(), draft_id, goal_id_2);
        assert!(
            result.is_err(),
            "Second acquire should fail while lock is held"
        );

        // After the first lock is released, a new acquire succeeds.
        drop(lock1);
        let lock2 = acquire_reviewer_lock(tmp.path(), draft_id, goal_id_2);
        assert!(lock2.is_ok(), "Should acquire after previous lock dropped");
    }

    #[test]
    fn reviewer_lock_records_goal_id() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let goal_id = Uuid::new_v4();
        let _lock = acquire_reviewer_lock(tmp.path(), draft_id, goal_id).unwrap();

        let lock_path = tmp
            .path()
            .join(".ta/drafts")
            .join(format!("{}.reviewer.lock", draft_id));
        let content = std::fs::read_to_string(&lock_path).unwrap();
        assert!(content.contains(&goal_id.to_string()));
    }

    #[test]
    fn reviewer_lock_returns_active_goal_on_busy() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let active_goal = Uuid::new_v4();

        let _lock = acquire_reviewer_lock(tmp.path(), draft_id, active_goal).unwrap();
        let err = acquire_reviewer_lock(tmp.path(), draft_id, Uuid::new_v4()).unwrap_err();
        assert_eq!(
            err, active_goal,
            "Busy error should contain the active goal ID"
        );
    }

    #[test]
    fn reviewer_lock_cleans_up_on_drop() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let lock_path = tmp
            .path()
            .join(".ta/drafts")
            .join(format!("{}.reviewer.lock", draft_id));

        {
            let _lock = acquire_reviewer_lock(tmp.path(), draft_id, Uuid::new_v4()).unwrap();
            assert!(lock_path.exists());
        }
        assert!(!lock_path.exists(), "Lock file should be removed on drop");
    }

    // ── Personas restriction tests ─────────────────────────────────────────────

    #[test]
    fn build_advisor_context_prohibits_personas_writes() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let ctx = build_advisor_context(&config);
        assert!(
            ctx.contains(".ta/personas/"),
            "Context should mention .ta/personas/"
        );
        assert!(
            ctx.contains("NOT write") || ctx.contains("read-only"),
            "Context should prohibit writing to personas dir"
        );
    }

    // ── Draft summary injection tests ──────────────────────────────────────────

    #[test]
    fn build_advisor_context_with_draft_summary() {
        let tmp = TempDir::new().unwrap();
        let mut summary = DraftSummary::new();
        summary.artifact_count = 3;
        summary.supervisor_verdict = Some("no blocking issues".to_string());
        summary.file_list.push(FileDiff {
            path: "src/main.rs".to_string(),
            action: "modified".to_string(),
            what: Some("Added team commands".to_string()),
        });
        summary.constitution_signals.push(ConstitutionSignal {
            signal: "no tests added".to_string(),
            severity: "warn".to_string(),
        });
        let config = make_config(&tmp).with_draft_summary(summary);
        let ctx = build_advisor_context(&config);
        assert!(ctx.contains("Draft Summary"));
        assert!(ctx.contains("no blocking issues"));
        assert!(ctx.contains("src/main.rs"));
        assert!(ctx.contains("Added team commands"));
        assert!(ctx.contains("[WARN]"));
        // Without a pre-loaded summary the protocol says "1. Call `ta_draft_view`"
        // With a summary it should say the summary is pre-loaded.
        assert!(
            !ctx.contains("1. Call `ta_draft_view` to load the draft summary"),
            "Protocol should not ask to load draft view when summary is pre-populated"
        );
    }

    #[test]
    fn build_advisor_context_without_draft_summary_keeps_protocol() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let ctx = build_advisor_context(&config);
        assert!(
            ctx.contains("Call `ta_draft_view` to load the draft summary"),
            "Without summary, protocol should still instruct ta_draft_view"
        );
    }

    #[test]
    fn poll_draft_outcome_not_found_returns_timeout() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        // File doesn't exist; should time out quickly with a very short timeout.
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::TimedOut);
    }

    #[test]
    fn poll_draft_outcome_applied_status() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let drafts_dir = tmp.path().join(".ta/drafts");
        std::fs::create_dir_all(&drafts_dir).unwrap();
        let draft_file = drafts_dir.join(format!("{}.json", draft_id));
        // Write a draft JSON with "applied" status.
        std::fs::write(
            &draft_file,
            r#"{"draft_package_id": "00000000-0000-0000-0000-000000000000", "status": "applied"}"#,
        )
        .unwrap();
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::Applied);
    }

    #[test]
    fn poll_draft_outcome_denied_status() {
        let tmp = TempDir::new().unwrap();
        let draft_id = Uuid::new_v4();
        let drafts_dir = tmp.path().join(".ta/drafts");
        std::fs::create_dir_all(&drafts_dir).unwrap();
        let draft_file = drafts_dir.join(format!("{}.json", draft_id));
        std::fs::write(
            &draft_file,
            r#"{"draft_package_id": "00000000-0000-0000-0000-000000000000", "status": "denied"}"#,
        )
        .unwrap();
        let outcome = poll_draft_outcome(
            tmp.path(),
            draft_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        assert_eq!(outcome, AdvisorOutcome::Denied);
    }
}
