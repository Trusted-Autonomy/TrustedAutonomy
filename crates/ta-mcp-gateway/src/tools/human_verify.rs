// tools/human_verify.rs — ta_human_verify MCP tool handler (v0.17.0.12.26).
//
// Two-stage confidence-gated verification, replacing `ta_ask_human`'s
// always-blocking behavior:
//
//   1. Resolve the calling goal's workload_type/security_tier from the most
//      recently logged `ta-brain::RoutingDecision`
//      (`.ta/routing-decisions.jsonl`) — the same "most recent entry"
//      heuristic `ta_ask_human` already uses to resolve goal_id.
//   2. `security_tier != "auto"` skips the synthetic stage entirely and
//      escalates straight to a real blocking human question (an inferred or
//      non-autonomous workload isn't trustworthy enough to auto-confirm).
//   3. Otherwise: an **opinion pass** (a headless agent answers the
//      question the way a human reviewer would) and an independent
//      **validator pass** (a second, separate headless agent that critiques
//      the opinion's reasoning rather than trusting its self-reported
//      confidence) each run as short-lived `ta run --headless` subprocesses,
//      following `ta_session::advisor_agent::spawn_advisor_agent`'s
//      spawn-and-capture pattern. The validator's output becomes a
//      `ta_decision::gate::DecisionInput`, scored via the shared `decide()`
//      gate against per-workload `DecisionThresholds` read from
//      `.ta/workflow.toml`'s `[human_verify.<workload_type>]`.
//   4. `Commit` -> auto-answer using the opinion's answer, with the full
//      opinion + validator reasoning documented in
//      `.ta/human-verify-audit.jsonl` (Observable & Actionable — never a
//      silent auto-confirm). Anything else (`Reject`/`Rework`/`Escalate`)
//      falls through to the existing blocking `ta_ask_human` flow
//      unchanged, with the synthetic reasoning appended as extra context so
//      the human sees what the pipeline already found ambiguous.
//
// The opinion/validator passes are abstracted behind `SyntheticPipeline` so
// the gating/config/audit logic is unit-testable without spawning real
// subprocesses; `HeadlessSyntheticPipeline` is the production implementation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ta_decision::{decide, Decision, DecisionInput, DecisionThresholds, Verdict};
use ta_session::workflow_session::AdvisorSecurity;

use crate::server::GatewayState;
use crate::tools::human::{self, AskHumanParams};

fn default_response_hint() -> String {
    "freeform".to_string()
}

fn default_timeout() -> Option<u64> {
    Some(600)
}

/// Parameters for `ta_human_verify`. Field-identical to the legacy
/// `AskHumanParams` so the deprecated `ta_ask_human` alias can forward
/// unchanged (v0.17.0.12.26 item 1/6).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HumanVerifyParams {
    /// The question to verify/ask.
    pub question: String,
    /// Optional context for the reviewer (what the agent has done so far).
    #[serde(default)]
    pub context: Option<String>,
    /// Expected response shape: "freeform" (default), "yes_no", "choice".
    #[serde(default = "default_response_hint")]
    pub response_hint: String,
    /// Suggested choices when response_hint is "choice".
    #[serde(default)]
    pub choices: Vec<String>,
    /// How long to wait in seconds before timing out, if escalated to a
    /// real human. Default: 600 (10 min).
    #[serde(default = "default_timeout")]
    pub timeout_secs: Option<u64>,
}

impl From<AskHumanParams> for HumanVerifyParams {
    fn from(p: AskHumanParams) -> Self {
        Self {
            question: p.question,
            context: p.context,
            response_hint: p.response_hint,
            choices: p.choices,
            timeout_secs: p.timeout_secs,
        }
    }
}

impl From<HumanVerifyParams> for AskHumanParams {
    fn from(p: HumanVerifyParams) -> Self {
        Self {
            question: p.question,
            context: p.context,
            response_hint: p.response_hint,
            choices: p.choices,
            timeout_secs: p.timeout_secs,
        }
    }
}

/// Output of the opinion pass: an answer plus its own reasoning and
/// self-reported confidence (never trusted blindly — the validator pass
/// critiques this independently).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpinionResult {
    pub answer: String,
    pub reasoning: String,
    pub confidence: f64,
}

/// Output of the validator pass: a `ta_decision::gate` verdict over the
/// opinion's reasoning, plus the validator's own (not the opinion's)
/// confidence in that judgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorResult {
    pub verdict: Verdict,
    pub risk_score: u32,
    pub confidence: f64,
    pub reasoning: String,
}

/// The opinion + validator pass, abstracted so gating logic is testable
/// without spawning real subprocesses.
pub trait SyntheticPipeline {
    fn opinion(&self, question: &str, context: Option<&str>) -> Result<OpinionResult, String>;

    fn validate(
        &self,
        question: &str,
        context: Option<&str>,
        opinion: &OpinionResult,
    ) -> Result<ValidatorResult, String>;
}

const OPINION_MARKER: &str = "HUMAN_VERIFY_OPINION_JSON:";
const VALIDATOR_MARKER: &str = "HUMAN_VERIFY_VALIDATOR_JSON:";

/// Production `SyntheticPipeline`: spawns two independent headless
/// `ta run --headless` subprocesses (mirroring
/// `ta_session::advisor_agent::spawn_advisor_agent`'s spawn-and-capture
/// pattern), one for the opinion pass and one for the validator pass. Each
/// gets its own fresh context file and process — no shared prompt/context
/// window, which is the point of independence (item 3).
pub struct HeadlessSyntheticPipeline {
    workspace_root: PathBuf,
    ta_bin: PathBuf,
}

impl HeadlessSyntheticPipeline {
    pub fn new(workspace_root: PathBuf, ta_bin: PathBuf) -> Self {
        Self {
            workspace_root,
            ta_bin,
        }
    }
}

impl SyntheticPipeline for HeadlessSyntheticPipeline {
    fn opinion(&self, question: &str, context: Option<&str>) -> Result<OpinionResult, String> {
        let content = build_opinion_context(question, context);
        let context_path = write_context_file(&self.workspace_root, "opinion-context", &content)
            .map_err(|e| format!("failed to write opinion context file: {e}"))?;

        let result = spawn_headless_and_capture(
            &self.ta_bin,
            &self.workspace_root,
            "ta_human_verify: opinion pass",
            &context_path,
        );
        let _ = std::fs::remove_file(&context_path);
        let (stdout, stderr) = result?;
        extract_marker_json(&stdout, &stderr, OPINION_MARKER)
    }

    fn validate(
        &self,
        question: &str,
        context: Option<&str>,
        opinion: &OpinionResult,
    ) -> Result<ValidatorResult, String> {
        let content = build_validator_context(question, context, opinion);
        let context_path = write_context_file(&self.workspace_root, "validator-context", &content)
            .map_err(|e| format!("failed to write validator context file: {e}"))?;

        let result = spawn_headless_and_capture(
            &self.ta_bin,
            &self.workspace_root,
            "ta_human_verify: validator pass",
            &context_path,
        );
        let _ = std::fs::remove_file(&context_path);
        let (stdout, stderr) = result?;
        extract_marker_json(&stdout, &stderr, VALIDATOR_MARKER)
    }
}

fn build_opinion_context(question: &str, context: Option<&str>) -> String {
    let mut ctx = String::new();
    ctx.push_str("# ta_human_verify — Opinion Pass\n\n");
    ctx.push_str(
        "You are the **opinion model** in TA's two-stage confidence-gated \
         verification pipeline. Answer the question below the way a careful \
         human reviewer would. Do not defer, and do not ask a follow-up \
         question — give your best answer.\n\n",
    );
    ctx.push_str(&format!("## Question\n\n{question}\n\n"));
    if let Some(c) = context {
        ctx.push_str(&format!("## Context\n\n{c}\n\n"));
    }
    ctx.push_str(
        "## Output protocol\n\n\
         Do your reasoning, then print exactly one final line to stdout of \
         the form:\n\n\
         HUMAN_VERIFY_OPINION_JSON: {\"answer\": \"...\", \"reasoning\": \"...\", \"confidence\": 0.0}\n\n\
         `confidence` is your own self-reported confidence (0.0-1.0) in the \
         answer. Print nothing after that line.\n",
    );
    ctx
}

fn build_validator_context(
    question: &str,
    context: Option<&str>,
    opinion: &OpinionResult,
) -> String {
    let mut ctx = String::new();
    ctx.push_str("# ta_human_verify — Validator Pass\n\n");
    ctx.push_str(
        "You are the **independent validator** in TA's two-stage \
         confidence-gated verification pipeline. You do not share context or \
         a conversation with the opinion model — critique its reasoning \
         against the original question below; you are not being asked to \
         re-answer the question yourself.\n\n",
    );
    ctx.push_str(&format!("## Original question\n\n{question}\n\n"));
    if let Some(c) = context {
        ctx.push_str(&format!("## Context given to the opinion model\n\n{c}\n\n"));
    }
    ctx.push_str(&format!(
        "## Opinion model's answer\n\n**Answer**: {}\n\n**Reasoning**: {}\n\n**Self-reported confidence**: {:.2}\n\n",
        opinion.answer, opinion.reasoning, opinion.confidence
    ));
    ctx.push_str(
        "## Your job\n\n\
         Critique the reasoning above — is it sound, does it actually answer \
         the question, is anything glossed over or assumed without support? \
         Then produce:\n\n\
         - `verdict`: \"pass\" (reasoning is sound, answer is trustworthy), \
         \"warn\" (gaps, but not clearly wrong), or \"block\" (reasoning is \
         wrong or the answer is unsafe to trust)\n\
         - `risk_score`: 0-100, higher = riskier to auto-trust\n\
         - `confidence`: 0.0-1.0, YOUR OWN confidence in this validation \
         judgment — not the opinion model's self-reported confidence\n\n\
         ## Output protocol\n\n\
         Print exactly one final line to stdout of the form:\n\n\
         HUMAN_VERIFY_VALIDATOR_JSON: {\"verdict\": \"pass\", \"risk_score\": 0, \"confidence\": 0.0, \"reasoning\": \"...\"}\n\n\
         Print nothing after that line.\n",
    );
    ctx
}

/// Write a fresh context file under `.ta/human-verify/<uuid>/<label>.md`,
/// mirroring `advisor_agent::write_advisor_context`'s per-invocation
/// directory scheme.
fn write_context_file(
    workspace_root: &Path,
    label: &str,
    content: &str,
) -> std::io::Result<PathBuf> {
    let dir = workspace_root
        .join(".ta")
        .join("human-verify")
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{label}.md"));
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Spawn `ta run <goal_title> --objective-file <context_path> --headless`
/// and capture its stdout/stderr.
///
/// Delivers the question/context content via `ta run`'s existing
/// `--objective-file` flag (main.rs's `Run.objective_file`, read directly
/// into the spawned agent's real objective) rather than an environment
/// variable — nothing in the codebase reads a `TA_*_CONTEXT_FILE`-style env
/// var to inject content into a spawned agent's prompt (the same gap exists
/// in the pre-existing `advisor_agent::spawn_advisor_agent`, which sets
/// `TA_ADVISOR_CONTEXT_FILE` but nothing reads it either). `--objective-file`
/// is the real, working mechanism for this.
fn spawn_headless_and_capture(
    ta_bin: &Path,
    workspace_root: &Path,
    goal_title: &str,
    context_path: &Path,
) -> Result<(String, String), String> {
    let mut cmd = std::process::Command::new(ta_bin);
    cmd.args([
        "--project-root",
        &workspace_root.to_string_lossy(),
        "run",
        goal_title,
        "--objective-file",
        &context_path.to_string_lossy(),
        "--headless",
        "--no-version-check",
    ]);
    // Also set the env var for any future consumer that reads it directly
    // (e.g. a persona-driven prompt instructing the agent to re-read its
    // own context file) -- harmless alongside --objective-file, not relied
    // on as the sole delivery mechanism.
    cmd.env("TA_HUMAN_VERIFY_CONTEXT_FILE", context_path);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ta run for '{goal_title}': {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!(
            "ta run --headless exited {} for '{}'.\nstdout: {}\nstderr: {}",
            output.status.code().unwrap_or(-1),
            goal_title,
            stdout.trim(),
            stderr.trim()
        ));
    }

    Ok((stdout, stderr))
}

/// Find the `marker: {json}` line in captured stdout/stderr and parse the
/// JSON payload, mirroring `spawn_advisor_agent`'s `"goal_id: "` line
/// extraction from subprocess output.
fn extract_marker_json<T: serde::de::DeserializeOwned>(
    stdout: &str,
    stderr: &str,
    marker: &str,
) -> Result<T, String> {
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(rest) = line.trim().strip_prefix(marker) {
            return serde_json::from_str(rest.trim())
                .map_err(|e| format!("malformed {marker} payload ({e}). line was: {line}"));
        }
    }
    Err(format!(
        "no '{marker}' line found in headless agent output.\nstdout: {stdout}\nstderr: {stderr}"
    ))
}

// ── Per-workload threshold config (.ta/workflow.toml) ────────────────────

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct PartialThresholds {
    min_confidence: Option<f64>,
    max_risk_score: Option<u32>,
    escalate_risk_score: Option<u32>,
}

impl PartialThresholds {
    fn apply_over(&self, base: DecisionThresholds) -> DecisionThresholds {
        DecisionThresholds {
            min_confidence: self.min_confidence.unwrap_or(base.min_confidence),
            max_risk_score: self.max_risk_score.unwrap_or(base.max_risk_score),
            escalate_risk_score: self.escalate_risk_score.unwrap_or(base.escalate_risk_score),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HumanVerifyWorkflowToml {
    human_verify: std::collections::HashMap<String, PartialThresholds>,
}

fn load_workflow_toml(workspace_root: &Path) -> HumanVerifyWorkflowToml {
    std::fs::read_to_string(workspace_root.join(".ta").join("workflow.toml"))
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

/// Resolve the `DecisionThresholds` for `workload_type`: a `[human_verify.default]`
/// table layers over the built-in default, then `[human_verify.<workload_type>]`
/// layers over that — each table only needs to override the fields it cares
/// about (v0.17.0.12.26 item 4).
fn load_thresholds(workspace_root: &Path, workload_type: &str) -> DecisionThresholds {
    let toml = load_workflow_toml(workspace_root);
    let base = toml
        .human_verify
        .get("default")
        .map(|p| p.apply_over(DecisionThresholds::default()))
        .unwrap_or_default();
    toml.human_verify
        .get(workload_type)
        .map(|p| p.apply_over(base))
        .unwrap_or(base)
}

// ── Workload/security-tier resolution ─────────────────────────────────────

/// Resolve the current goal's `workload_type`/`security_tier` from the most
/// recently logged `ta-brain::RoutingDecision`
/// (`.ta/routing-decisions.jsonl`) — the same "most recent entry" heuristic
/// `ta_ask_human` already uses to resolve `goal_id`. Falls back to
/// `("general", Suggest)` when no routing decision has ever been logged —
/// deliberately *not* `Auto`, so an unclassified workload never silently
/// gets the auto-confirm fast path.
fn resolve_workload_context(workspace_root: &Path) -> (String, AdvisorSecurity) {
    #[derive(Deserialize)]
    struct LoggedDecision {
        workload_type: String,
        security_tier: AdvisorSecurity,
    }
    #[derive(Deserialize)]
    struct RoutingLogEntry {
        decision: LoggedDecision,
    }

    let log_path = workspace_root.join(".ta").join("routing-decisions.jsonl");
    let last = std::fs::read_to_string(&log_path).ok().and_then(|content| {
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<RoutingLogEntry>(l).ok())
            .next_back()
    });

    match last {
        Some(entry) => (entry.decision.workload_type, entry.decision.security_tier),
        None => ("general".to_string(), AdvisorSecurity::Suggest),
    }
}

// ── Audit trail (.ta/human-verify-audit.jsonl) ────────────────────────────

#[derive(Debug, Serialize)]
struct HumanVerifyAuditEntry<'a> {
    timestamp: String,
    question: &'a str,
    context: Option<&'a str>,
    workload_type: &'a str,
    opinion: &'a OpinionResult,
    validator: &'a ValidatorResult,
    decision: Decision,
}

/// Append one entry to `.ta/human-verify-audit.jsonl` — gitignored,
/// per-run operational log, same category as `routing-decisions.jsonl`.
/// Only called on the `Commit` path (item 5) — an auto-confirmed decision
/// must never be a silent black box.
fn write_audit_entry(workspace_root: &Path, entry: &HumanVerifyAuditEntry) {
    let log_path = workspace_root.join(".ta").join("human-verify-audit.jsonl");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(entry) else {
        tracing::warn!("ta_human_verify: failed to serialize audit entry");
        return;
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(mut f) => {
            use std::io::Write as _;
            if let Err(e) = writeln!(f, "{}", line) {
                tracing::warn!(path = %log_path.display(), error = %e, "ta_human_verify: failed to write audit log");
            }
        }
        Err(e) => {
            tracing::warn!(path = %log_path.display(), error = %e, "ta_human_verify: failed to open audit log");
        }
    }
}

// ── Tool entry points ─────────────────────────────────────────────────────

/// Handle `ta_human_verify` using the real, subprocess-backed synthetic
/// pipeline.
pub fn handle_human_verify(
    state: &Arc<Mutex<GatewayState>>,
    params: HumanVerifyParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = {
        let locked = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        locked.config.workspace_root.clone()
    };
    let ta_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ta"));
    let pipeline = HeadlessSyntheticPipeline::new(workspace_root, ta_bin);
    handle_human_verify_with_pipeline(state, params, &pipeline)
}

/// Deprecated alias: `ta_ask_human` now forwards unchanged to
/// `ta_human_verify`'s two-stage confidence-gated pipeline. Kept registered
/// (matching v0.17.0.12.16's alias + one-time deprecation-notice pattern) so
/// existing agent prompts/docs that already reference the old tool name
/// don't break (item 1).
pub fn handle_ask_human_deprecated(
    state: &Arc<Mutex<GatewayState>>,
    params: AskHumanParams,
) -> Result<CallToolResult, McpError> {
    tracing::warn!(
        "[deprecated-tool] `ta_ask_human` is deprecated -> use `ta_human_verify` instead \
         (identical parameters/behavior, now confidence-gated). See docs/USAGE.md."
    );
    handle_human_verify(state, params.into())
}

fn handle_human_verify_with_pipeline(
    state: &Arc<Mutex<GatewayState>>,
    params: HumanVerifyParams,
    pipeline: &dyn SyntheticPipeline,
) -> Result<CallToolResult, McpError> {
    let workspace_root = {
        let locked = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        locked.config.workspace_root.clone()
    };

    let (workload_type, security_tier) = resolve_workload_context(&workspace_root);

    if security_tier != AdvisorSecurity::Auto {
        tracing::info!(
            workload_type = %workload_type,
            security_tier = %security_tier,
            "ta_human_verify: security_tier != auto, skipping synthetic stage, escalating directly"
        );
        return escalate_to_human(state, params, None);
    }

    let opinion = match pipeline.opinion(&params.question, params.context.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "ta_human_verify: opinion pass failed, escalating to human");
            return escalate_to_human(
                state,
                params,
                Some(format!(
                    "[ta_human_verify] Synthetic opinion pass failed ({e}) — escalated directly."
                )),
            );
        }
    };

    let validator = match pipeline.validate(&params.question, params.context.as_deref(), &opinion) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "ta_human_verify: validator pass failed, escalating to human");
            return escalate_to_human(
                state,
                params,
                Some(format!(
                    "[ta_human_verify] Synthetic validator pass failed ({e}) after opinion \
                     answered \"{}\" — escalated directly.",
                    opinion.answer
                )),
            );
        }
    };

    let thresholds = load_thresholds(&workspace_root, &workload_type);
    let input = DecisionInput {
        verdict: validator.verdict,
        risk_score: validator.risk_score,
        confidence: validator.confidence,
    };
    let decision = decide(&input, &thresholds);

    tracing::info!(
        workload_type = %workload_type,
        decision = ?decision,
        opinion_confidence = opinion.confidence,
        validator_risk_score = validator.risk_score,
        validator_confidence = validator.confidence,
        "ta_human_verify: synthetic pipeline decision"
    );

    if decision.is_auto_approvable() {
        let entry = HumanVerifyAuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            question: &params.question,
            context: params.context.as_deref(),
            workload_type: &workload_type,
            opinion: &opinion,
            validator: &validator,
            decision,
        };
        write_audit_entry(&workspace_root, &entry);

        let response = serde_json::json!({
            "answer": opinion.answer,
            "auto_confirmed": true,
            "decision": decision,
            "opinion_reasoning": opinion.reasoning,
            "opinion_confidence": opinion.confidence,
            "validator_reasoning": validator.reasoning,
            "validator_confidence": validator.confidence,
            "validator_risk_score": validator.risk_score,
            "audit_log": ".ta/human-verify-audit.jsonl",
        });
        return Ok(CallToolResult::success(vec![
            Content::json(response).map_err(|e| McpError::internal_error(e.to_string(), None))?
        ]));
    }

    let extra_context = format!(
        "[ta_human_verify synthetic pre-check: {decision:?}] Opinion answer: \"{}\" \
         (confidence {:.0}%). Validator verdict: {:?} (risk {}, confidence {:.0}%) — {}",
        opinion.answer,
        opinion.confidence * 100.0,
        validator.verdict,
        validator.risk_score,
        validator.confidence * 100.0,
        validator.reasoning,
    );
    escalate_to_human(state, params, Some(extra_context))
}

/// Fall through to the existing, unchanged blocking human-ask flow
/// (`tools::human::handle_ask_human`), optionally appending synthetic
/// pre-check reasoning as extra context for the human (item 5).
fn escalate_to_human(
    state: &Arc<Mutex<GatewayState>>,
    mut params: HumanVerifyParams,
    extra_context: Option<String>,
) -> Result<CallToolResult, McpError> {
    if let Some(extra) = extra_context {
        params.context = Some(match params.context.take() {
            Some(existing) => format!("{existing}\n\n{extra}"),
            None => extra,
        });
    }
    human::handle_ask_human(state, params.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    use crate::config::GatewayConfig;
    use crate::server::GatewayState;

    fn make_state(dir: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let config = GatewayConfig::for_project(dir);
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    fn write_routing_decision(workspace_root: &Path, workload_type: &str, security_tier: &str) {
        let log_path = workspace_root.join(".ta").join("routing-decisions.jsonl");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        let line = serde_json::json!({
            "timestamp": "2026-07-10T00:00:00Z",
            "goal_title": "test goal",
            "decision": {
                "team": "implementer",
                "agent": "claude-code",
                "security_tier": security_tier,
                "priority": "normal",
                "workload_type": workload_type,
                "workload_confidence": 0.9,
                "rationale": ["test"],
            }
        });
        std::fs::write(&log_path, format!("{}\n", line)).unwrap();
    }

    /// A fake pipeline with fixed opinion/validator outputs, used to test
    /// gating logic without spawning real subprocesses.
    struct FakePipeline {
        opinion: OpinionResult,
        validator: ValidatorResult,
    }

    impl SyntheticPipeline for FakePipeline {
        fn opinion(
            &self,
            _question: &str,
            _context: Option<&str>,
        ) -> Result<OpinionResult, String> {
            Ok(self.opinion.clone())
        }
        fn validate(
            &self,
            _question: &str,
            _context: Option<&str>,
            _opinion: &OpinionResult,
        ) -> Result<ValidatorResult, String> {
            Ok(self.validator.clone())
        }
    }

    /// A pipeline that panics if invoked — used to prove the synthetic
    /// stage was skipped entirely (e.g. non-"auto" security tier).
    struct MustNotBeCalledPipeline;

    impl SyntheticPipeline for MustNotBeCalledPipeline {
        fn opinion(
            &self,
            _question: &str,
            _context: Option<&str>,
        ) -> Result<OpinionResult, String> {
            panic!("synthetic opinion pass must not run for this security tier");
        }
        fn validate(
            &self,
            _question: &str,
            _context: Option<&str>,
            _opinion: &OpinionResult,
        ) -> Result<ValidatorResult, String> {
            panic!("synthetic validator pass must not run for this security tier");
        }
    }

    fn spawn_answer_thread(dir: &std::path::Path, answer_text: &str) {
        let pending_dir = dir.join(".ta").join("interactions").join("pending");
        let answers_dir = dir.join(".ta").join("interactions").join("answers");
        let answer_text = answer_text.to_string();
        std::thread::spawn(move || {
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(entries) = std::fs::read_dir(&pending_dir) {
                    let files: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                        .collect();
                    if let Some(entry) = files.first() {
                        let stem = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        std::fs::create_dir_all(&answers_dir).unwrap();
                        let answer_path = answers_dir.join(format!("{}.json", stem));
                        let answer = serde_json::json!({
                            "text": answer_text,
                            "responder_id": "test-human",
                            "answered_at": Utc::now().to_rfc3339(),
                        });
                        std::fs::write(&answer_path, serde_json::to_string(&answer).unwrap())
                            .unwrap();
                        return;
                    }
                }
            }
        });
    }

    fn content_text(result: &CallToolResult) -> String {
        match &result.content[0].raw {
            RawContent::Text(t) => t.text.clone(),
            other => panic!("expected text content, got: {:?}", other),
        }
    }

    fn base_params() -> HumanVerifyParams {
        HumanVerifyParams {
            question: "Should we ship this?".to_string(),
            context: Some("All tests pass.".to_string()),
            response_hint: "yes_no".to_string(),
            choices: vec![],
            timeout_secs: Some(5),
        }
    }

    #[test]
    fn high_confidence_low_risk_auto_confirms_with_audit_entry() {
        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "docs", "auto");
        let state = make_state(dir.path());
        let pipeline = FakePipeline {
            opinion: OpinionResult {
                answer: "Yes, ship it.".to_string(),
                reasoning: "All checks green.".to_string(),
                confidence: 0.95,
            },
            validator: ValidatorResult {
                verdict: Verdict::Pass,
                risk_score: 5,
                confidence: 0.95,
                reasoning: "Sound reasoning, low stakes.".to_string(),
            },
        };

        let result = handle_human_verify_with_pipeline(&state, base_params(), &pipeline)
            .expect("should succeed");
        let text = content_text(&result);
        assert!(text.contains("\"auto_confirmed\":true"), "got: {}", text);
        assert!(text.contains("Yes, ship it."), "got: {}", text);

        let audit_path = dir.path().join(".ta").join("human-verify-audit.jsonl");
        let audit = std::fs::read_to_string(&audit_path).expect("audit log should exist");
        assert!(audit.contains("\"decision\":\"commit\""), "got: {}", audit);
        assert!(audit.contains("docs"), "got: {}", audit);
    }

    #[test]
    fn low_confidence_escalates_to_real_human_and_no_audit_entry() {
        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "bugfix", "auto");
        let state = make_state(dir.path());
        let pipeline = FakePipeline {
            opinion: OpinionResult {
                answer: "Maybe.".to_string(),
                reasoning: "Unclear signal.".to_string(),
                confidence: 0.4,
            },
            validator: ValidatorResult {
                verdict: Verdict::Pass,
                risk_score: 10,
                confidence: 0.3,
                reasoning: "Not confident in this call.".to_string(),
            },
        };

        spawn_answer_thread(dir.path(), "human said yes");

        let result = handle_human_verify_with_pipeline(&state, base_params(), &pipeline)
            .expect("should succeed");
        let text = content_text(&result);
        assert!(text.contains("human said yes"), "got: {}", text);

        let audit_path = dir.path().join(".ta").join("human-verify-audit.jsonl");
        assert!(
            !audit_path.exists(),
            "escalated decisions must not write an audit entry"
        );
    }

    #[test]
    fn non_auto_security_tier_always_escalates_skipping_synthetic_stage() {
        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "security", "suggest");
        let state = make_state(dir.path());

        spawn_answer_thread(dir.path(), "escalated answer");

        // MustNotBeCalledPipeline panics if the synthetic stage runs at all.
        let result =
            handle_human_verify_with_pipeline(&state, base_params(), &MustNotBeCalledPipeline)
                .expect("should succeed without touching the pipeline");
        let text = content_text(&result);
        assert!(text.contains("escalated answer"), "got: {}", text);
    }

    #[test]
    fn alias_ta_ask_human_behaves_identically_to_direct_call() {
        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "docs", "auto");
        let state = make_state(dir.path());
        let pipeline = FakePipeline {
            opinion: OpinionResult {
                answer: "Consistent answer".to_string(),
                reasoning: "Straightforward.".to_string(),
                confidence: 0.9,
            },
            validator: ValidatorResult {
                verdict: Verdict::Pass,
                risk_score: 5,
                confidence: 0.9,
                reasoning: "Sound.".to_string(),
            },
        };

        let direct = handle_human_verify_with_pipeline(&state, base_params(), &pipeline).unwrap();
        // Round-trip through AskHumanParams, exactly as the deprecated
        // `ta_ask_human` alias does before forwarding.
        let via_alias: AskHumanParams = base_params().into();
        let roundtripped: HumanVerifyParams = via_alias.into();
        let aliased = handle_human_verify_with_pipeline(&state, roundtripped, &pipeline).unwrap();

        let direct_text = content_text(&direct);
        let aliased_text = content_text(&aliased);
        assert_eq!(direct_text, aliased_text);
    }

    #[test]
    fn resolve_workload_context_defaults_when_no_log_present() {
        let dir = tempdir().unwrap();
        let (workload_type, security_tier) = resolve_workload_context(dir.path());
        assert_eq!(workload_type, "general");
        assert_eq!(security_tier, AdvisorSecurity::Suggest);
    }

    #[test]
    fn resolve_workload_context_reads_most_recent_entry() {
        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "bugfix", "suggest");
        write_routing_decision(dir.path(), "security", "auto");
        let (workload_type, security_tier) = resolve_workload_context(dir.path());
        assert_eq!(workload_type, "security");
        assert_eq!(security_tier, AdvisorSecurity::Auto);
    }

    #[test]
    fn load_thresholds_falls_back_to_default_when_unconfigured() {
        let dir = tempdir().unwrap();
        let thresholds = load_thresholds(dir.path(), "docs");
        let default = DecisionThresholds::default();
        assert_eq!(thresholds.min_confidence, default.min_confidence);
        assert_eq!(thresholds.max_risk_score, default.max_risk_score);
        assert_eq!(thresholds.escalate_risk_score, default.escalate_risk_score);
    }

    #[test]
    fn load_thresholds_applies_per_workload_override() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[human_verify.docs]\nmin_confidence = 0.5\n",
        )
        .unwrap();

        let docs_thresholds = load_thresholds(dir.path(), "docs");
        assert_eq!(docs_thresholds.min_confidence, 0.5);

        // Unrelated workload types stay at the built-in default.
        let other_thresholds = load_thresholds(dir.path(), "security");
        assert_eq!(
            other_thresholds.min_confidence,
            DecisionThresholds::default().min_confidence
        );
    }

    #[test]
    fn load_thresholds_default_table_layers_under_workload_override() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[human_verify.default]\nmin_confidence = 0.8\nmax_risk_score = 20\n\n\
             [human_verify.docs]\nmax_risk_score = 60\n",
        )
        .unwrap();

        let docs_thresholds = load_thresholds(dir.path(), "docs");
        // docs overrides max_risk_score but inherits min_confidence from [default].
        assert_eq!(docs_thresholds.min_confidence, 0.8);
        assert_eq!(docs_thresholds.max_risk_score, 60);
    }

    #[test]
    fn opinion_pass_failure_escalates_with_reason_in_context() {
        struct FailingOpinionPipeline;
        impl SyntheticPipeline for FailingOpinionPipeline {
            fn opinion(&self, _q: &str, _c: Option<&str>) -> Result<OpinionResult, String> {
                Err("subprocess spawn failed".to_string())
            }
            fn validate(
                &self,
                _q: &str,
                _c: Option<&str>,
                _o: &OpinionResult,
            ) -> Result<ValidatorResult, String> {
                panic!("validator must not run when opinion pass fails");
            }
        }

        let dir = tempdir().unwrap();
        write_routing_decision(dir.path(), "docs", "auto");
        let state = make_state(dir.path());
        spawn_answer_thread(dir.path(), "human answered after synthetic failure");

        let result =
            handle_human_verify_with_pipeline(&state, base_params(), &FailingOpinionPipeline)
                .expect("should escalate rather than error out");
        let text = content_text(&result);
        assert!(
            text.contains("human answered after synthetic failure"),
            "got: {}",
            text
        );
    }
}
