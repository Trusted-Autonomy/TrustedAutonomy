//! Advisor-driven goal creation (v0.17.0.12.23): turn a raw natural-language
//! prompt into a routed goal request, asking at most one clarifying question
//! when routing confidence is low.
//!
//! Unifies the two intent-handling systems that didn't previously talk to
//! each other: `ta_brain::route()` (confident once given a structured
//! title/objective, but never parses raw free text itself) and the
//! confidence-gated clarification pattern `ta-workflow::intent::resolve_intent`
//! already modeled for workflow templates (now folded into `route()` itself
//! as one signal — see `ta_brain::route::resolve_workflow_template` —
//! rather than reimplemented here as a second system).
//!
//! A raw prompt needs no separate NLU step to become a structured request:
//! `ta_brain::ExplicitGoalRequest` already accepts a free-text title/objective
//! and classifies workload from it. The only genuinely new behavior this
//! phase adds is *asking a real clarifying question* when that classification
//! is low-confidence, instead of silently guessing or requiring a human to
//! review afterward — reusing the existing `ta_ask_human`-backed
//! headless-agent mechanism (`ta_session::intent_agent`) built for exactly
//! this kind of one-shot conversational round-trip, rather than a new
//! conversational loop.

use std::path::Path;

use ta_brain::{route, ExplicitGoalRequest, RoutingDecision, RoutingInput};

use crate::classify::AMBIGUOUS_CONFIDENCE_THRESHOLD;

/// Ask the human exactly one clarifying question and return their answer
/// (or `None` if they didn't answer / clarification was skipped).
///
/// Implemented in production by spawning the headless clarification agent
/// (`ta_session::intent_agent::spawn_intent_agent` + `poll_intent_answer`,
/// which calls `ta_ask_human` under the hood). Tests inject a fake to avoid
/// spawning a real agent subprocess — see `run_pipeline`'s tests below.
pub trait Clarifier {
    fn ask(&self, question: &str, decision: &RoutingDecision) -> Option<String>;
}

/// A no-op clarifier: never asks, always accepts the low-confidence routing
/// as-is. Used for `--no-input`/non-interactive callers.
pub struct NoClarification;

impl Clarifier for NoClarification {
    fn ask(&self, _question: &str, _decision: &RoutingDecision) -> Option<String> {
        None
    }
}

/// Outcome of `run_pipeline`.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    /// The original raw prompt, used as the goal title.
    pub goal_title: String,
    /// The objective actually routed — equal to `goal_title` unless a
    /// clarifying answer was folded in.
    pub objective: String,
    /// The (possibly re-routed) routing decision.
    pub decision: RoutingDecision,
    /// `true` iff a clarifying question was asked and answered.
    pub clarified: bool,
    /// The human's raw answer, when `clarified` is true.
    pub clarification_answer: Option<String>,
}

impl PipelineResult {
    /// `true` when the final decision is confident enough to have needed no
    /// clarification, or was successfully clarified — i.e. safe to present
    /// as a ready-to-create goal rather than still-ambiguous.
    pub fn is_confident(&self) -> bool {
        self.decision.workload_confidence >= AMBIGUOUS_CONFIDENCE_THRESHOLD
    }
}

/// Run the free-text → routed-goal pipeline end-to-end (item 3):
///
/// 1. Route the raw prompt through `ta_brain::route()` exactly as it
///    consumes any other explicit goal request.
/// 2. If workload-classification confidence is at or above
///    [`AMBIGUOUS_CONFIDENCE_THRESHOLD`], return immediately — zero
///    clarification.
/// 3. Otherwise, ask `clarifier` exactly one question built from the
///    low-confidence decision, fold the answer into the objective, and
///    re-route once. Never asks a second question, even if the re-routed
///    decision is still low-confidence — the caller (Observable & Actionable)
///    sees `decision.rationale` either way.
pub fn run_pipeline(
    raw_prompt: &str,
    workspace_root: &Path,
    clarifier: &dyn Clarifier,
) -> PipelineResult {
    run_pipeline_with_security(raw_prompt, workspace_root, clarifier, None)
}

/// Same as [`run_pipeline`], but with an explicit `--security` override fed
/// into `route()` exactly as `ta run --security` already is (tier 1,
/// "explicit beats everything" — see `ta_brain::route::resolve_security`).
pub fn run_pipeline_with_security(
    raw_prompt: &str,
    workspace_root: &Path,
    clarifier: &dyn Clarifier,
    security_override: Option<&str>,
) -> PipelineResult {
    let prompt = raw_prompt.trim().to_string();
    let decision = route_text(&prompt, workspace_root, security_override);

    if decision.workload_confidence >= AMBIGUOUS_CONFIDENCE_THRESHOLD {
        return PipelineResult {
            goal_title: prompt.clone(),
            objective: prompt,
            decision,
            clarified: false,
            clarification_answer: None,
        };
    }

    let question = clarifying_question(&decision);
    match clarifier.ask(&question, &decision) {
        Some(answer) if !answer.trim().is_empty() => {
            let combined = format!("{} {}", prompt, answer.trim());
            let re_decision = route_text(&combined, workspace_root, security_override);
            PipelineResult {
                goal_title: prompt,
                objective: combined,
                decision: re_decision,
                clarified: true,
                clarification_answer: Some(answer),
            }
        }
        _ => PipelineResult {
            goal_title: prompt.clone(),
            objective: prompt,
            decision,
            clarified: false,
            clarification_answer: None,
        },
    }
}

/// Build the single clarifying question presented for a low-confidence
/// decision — Observable & Actionable: names the guess and its confidence
/// rather than a bare "please clarify."
pub fn clarifying_question(decision: &RoutingDecision) -> String {
    format!(
        "I'm not fully confident how to route this (best guess: \"{}\" work at {:.0}% \
         confidence). Can you say more about what you'd like done?",
        decision.workload_type,
        decision.workload_confidence * 100.0
    )
}

fn route_text(
    text: &str,
    workspace_root: &Path,
    security_override: Option<&str>,
) -> RoutingDecision {
    let request = ExplicitGoalRequest {
        goal_title: text.to_string(),
        objective: text.to_string(),
        cli_security: security_override.map(str::to_string),
        ..ExplicitGoalRequest::new(text.to_string())
    };
    route(&RoutingInput::ExplicitGoal(request), workspace_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClarifier(Option<String>);
    impl Clarifier for FakeClarifier {
        fn ask(&self, _question: &str, _decision: &RoutingDecision) -> Option<String> {
            self.0.clone()
        }
    }

    struct PanicClarifier;
    impl Clarifier for PanicClarifier {
        fn ask(&self, _question: &str, _decision: &RoutingDecision) -> Option<String> {
            panic!("clarifier must not be invoked for a high-confidence prompt");
        }
    }

    #[test]
    fn high_confidence_prompt_routes_with_zero_clarification() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_pipeline("Fix the login bug", tmp.path(), &PanicClarifier);
        assert!(!result.clarified);
        assert!(result.clarification_answer.is_none());
        assert!(result.is_confident());
        assert_eq!(result.decision.workload_type, "bugfix");
        assert_eq!(result.objective, "Fix the login bug");
    }

    #[test]
    fn low_confidence_prompt_asks_exactly_one_question_and_reroutes() {
        let tmp = tempfile::tempdir().unwrap();
        // "xyzzy plugh" classifies as "other" at 0.3 confidence (below the
        // 0.65 threshold), so the pipeline must ask before routing.
        let result = run_pipeline(
            "xyzzy plugh",
            tmp.path(),
            &FakeClarifier(Some("It's a security fix for the login bypass".to_string())),
        );
        assert!(result.clarified);
        assert_eq!(
            result.clarification_answer.as_deref(),
            Some("It's a security fix for the login bypass")
        );
        assert_eq!(result.decision.workload_type, "security");
        assert!(result.objective.starts_with("xyzzy plugh"));
        assert!(result.objective.contains("security fix"));
    }

    #[test]
    fn low_confidence_prompt_with_no_answer_keeps_original_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_pipeline("xyzzy plugh", tmp.path(), &NoClarification);
        assert!(!result.clarified);
        assert_eq!(result.decision.workload_type, "other");
        assert_eq!(result.objective, "xyzzy plugh");
    }

    #[test]
    fn low_confidence_prompt_with_blank_answer_keeps_original_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_pipeline(
            "xyzzy plugh",
            tmp.path(),
            &FakeClarifier(Some("   ".to_string())),
        );
        assert!(!result.clarified);
        assert_eq!(result.decision.workload_type, "other");
    }

    #[test]
    fn security_override_forces_explicit_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_pipeline_with_security(
            "Fix the login bug",
            tmp.path(),
            &PanicClarifier,
            Some("auto"),
        );
        assert_eq!(
            result.decision.security_tier,
            ta_session::workflow_session::AdvisorSecurity::Auto
        );
    }

    #[test]
    fn clarifying_question_names_the_guess_and_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route_text("xyzzy plugh", tmp.path(), None);
        let question = clarifying_question(&decision);
        assert!(question.contains("other"));
        assert!(question.contains('%'));
    }
}
