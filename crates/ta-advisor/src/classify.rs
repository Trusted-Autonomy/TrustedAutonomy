// classify.rs — Smart Advisor intent classification (v0.17.0.12.6).
//
// Wraps `ta_session::classify_intent` (the existing 5-way GoalRun/Question/
// Clarify/Apply/Deny heuristic classifier) with the 4-way taxonomy the
// Studio Smart Advisor needs: queue_goal, info_request, draft_action
// (amend/follow-up/add-to-plan), or ambiguous. Draft-action phrasing isn't
// covered by the underlying classifier at all, so it's detected here first.

use serde::{Deserialize, Serialize};
use ta_session::{classify_intent, Intent as SessionIntent};

/// Message wasn't confident enough to auto-route; requires clarification.
pub const AMBIGUOUS_CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Maximum number of clarification round-trips before giving up and telling
/// the human "I need more info" (item 15).
pub const MAX_CLARIFY_ROUNDS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorIntent {
    /// Human wants to queue a new goal (e.g. "also add X", "fix Y").
    QueueGoal,
    /// Human is asking a question answerable from daemon state, no goal needed.
    InfoRequest,
    /// Human wants to act on a specific draft: amend it, follow it up, or
    /// fold it into the plan.
    DraftAction,
    /// Not enough signal to route confidently.
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftActionKind {
    /// "add item X to the plan"
    AddToPlan,
    /// "create a --follow-up goal to fix Y"
    FollowUp,
    /// "amend this draft to also include Z"
    Amend,
    /// "apply" / "deny" style direct action on the draft itself.
    ApplyOrDeny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvisorClassification {
    pub intent: AdvisorIntent,
    /// Confidence in [0.0, 1.0].
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_action_kind: Option<DraftActionKind>,
}

impl AdvisorClassification {
    fn new(intent: AdvisorIntent, confidence: f32) -> Self {
        Self {
            intent,
            confidence,
            extracted_goal: None,
            draft_action_kind: None,
        }
    }

    fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.extracted_goal = Some(goal.into());
        self
    }

    fn with_draft_action(mut self, kind: DraftActionKind) -> Self {
        self.draft_action_kind = Some(kind);
        self
    }
}

/// Classify free-text advisor input into the Studio's 4-way taxonomy.
///
/// `in_draft_context` should be `true` when the message originates from a
/// per-draft Q&A dialog (item 11/12) — in that context "apply"/"deny"-style
/// replies and amend/follow-up phrasing are draft actions rather than
/// dashboard-level goal queueing.
pub fn classify(message: &str, in_draft_context: bool) -> AdvisorClassification {
    let lower = message.to_ascii_lowercase();
    let trimmed = lower.trim();

    if let Some(kind) = detect_draft_action_kind(trimmed) {
        return AdvisorClassification::new(AdvisorIntent::DraftAction, 0.9).with_draft_action(kind);
    }

    let result = classify_intent(message);
    let mut classification = match result.intent {
        SessionIntent::GoalRun => {
            let mut c = AdvisorClassification::new(AdvisorIntent::QueueGoal, result.confidence);
            if let Some(goal) = result.extracted_goal {
                c = c.with_goal(goal);
            }
            c
        }
        SessionIntent::Question => {
            AdvisorClassification::new(AdvisorIntent::InfoRequest, result.confidence)
        }
        SessionIntent::Apply | SessionIntent::Deny if in_draft_context => {
            AdvisorClassification::new(AdvisorIntent::DraftAction, result.confidence)
                .with_draft_action(DraftActionKind::ApplyOrDeny)
        }
        // Outside a draft context, "apply"/"yes"/"skip" carry no goal to run
        // and aren't an info request either — treat as ambiguous rather than
        // silently doing nothing.
        SessionIntent::Apply | SessionIntent::Deny => {
            AdvisorClassification::new(AdvisorIntent::Ambiguous, 0.5)
        }
        SessionIntent::Clarify => {
            AdvisorClassification::new(AdvisorIntent::Ambiguous, result.confidence)
        }
    };

    if classification.confidence < AMBIGUOUS_CONFIDENCE_THRESHOLD {
        classification.intent = AdvisorIntent::Ambiguous;
    }
    classification
}

fn detect_draft_action_kind(trimmed: &str) -> Option<DraftActionKind> {
    let add_to_plan_phrases = ["add item", "add this to the plan", "to the plan"];
    let follow_up_phrases = ["follow-up", "follow up", "create a followup"];
    let amend_phrases = [
        "amend this draft",
        "amend the draft",
        "amend this",
        "amend draft",
    ];

    if amend_phrases.iter().any(|p| trimmed.contains(p)) {
        return Some(DraftActionKind::Amend);
    }
    if follow_up_phrases.iter().any(|p| trimmed.contains(p)) {
        return Some(DraftActionKind::FollowUp);
    }
    if add_to_plan_phrases.iter().any(|p| trimmed.contains(p)) {
        return Some(DraftActionKind::AddToPlan);
    }
    None
}

/// Confirmation card shown for the unambiguous `queue_goal` path (item 14):
/// title, phase, and a rough estimated duration, with Approve / Edit / Cancel
/// left to the caller (Studio renders the buttons; approving calls
/// `ta goal start`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmationCard {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub estimated_duration_mins: u32,
}

/// Build a confirmation card for a `QueueGoal` classification. Returns `None`
/// for any other intent — callers should only invoke this after confirming
/// the classification is unambiguous and actionable.
pub fn build_confirmation_card(
    classification: &AdvisorClassification,
    current_phase: Option<&str>,
) -> Option<ConfirmationCard> {
    if classification.intent != AdvisorIntent::QueueGoal {
        return None;
    }
    let title = classification
        .extracted_goal
        .clone()
        .unwrap_or_else(|| "Untitled goal".to_string());
    let estimated_duration_mins = estimate_duration_mins(&title);
    Some(ConfirmationCard {
        title,
        phase: current_phase.map(|s| s.to_string()),
        estimated_duration_mins,
    })
}

/// Rough duration estimate driven by request length — longer, more
/// multi-part requests tend to take longer. Purely informational; never
/// blocks anything.
fn estimate_duration_mins(title: &str) -> u32 {
    let words = title.split_whitespace().count() as u32;
    (15 + words * 2).min(120)
}

/// Tracks how many clarification round-trips have occurred for a single
/// ambiguous conversation thread (item 15: max 2 rounds).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ClarifyState {
    pub round: u32,
}

impl ClarifyState {
    pub fn rounds_exhausted(&self) -> bool {
        self.round >= MAX_CLARIFY_ROUNDS
    }

    pub fn next(self) -> Self {
        Self {
            round: self.round + 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClarifyOutcome {
    /// Numbered clarification options to present to the human.
    Options { options: Vec<String> },
    /// Clarification rounds exhausted; ask the human to rephrase with more detail.
    NeedMoreInfo,
}

/// Given the current clarify round state, decide whether to present another
/// round of numbered clarification options or give up (item 15).
pub fn next_clarify_step(state: ClarifyState) -> ClarifyOutcome {
    if state.rounds_exhausted() {
        return ClarifyOutcome::NeedMoreInfo;
    }
    ClarifyOutcome::Options {
        options: vec![
            "Queue this as a new goal".to_string(),
            "Ask a question about the project or a running goal".to_string(),
            "Take an action on a draft (amend, follow-up, or add to plan)".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_queue_goal() {
        let c = classify("also add a test for the new endpoint", false);
        assert_eq!(c.intent, AdvisorIntent::QueueGoal);
        assert!(c.extracted_goal.is_some());
    }

    #[test]
    fn classifies_info_request() {
        let c = classify("what changed in auth.rs?", false);
        assert_eq!(c.intent, AdvisorIntent::InfoRequest);
    }

    #[test]
    fn classifies_amend_as_draft_action() {
        let c = classify("amend this draft to also include the migration", false);
        assert_eq!(c.intent, AdvisorIntent::DraftAction);
        assert_eq!(c.draft_action_kind, Some(DraftActionKind::Amend));
    }

    #[test]
    fn classifies_follow_up_as_draft_action() {
        let c = classify("create a follow-up goal to fix the flaky test", false);
        assert_eq!(c.intent, AdvisorIntent::DraftAction);
        assert_eq!(c.draft_action_kind, Some(DraftActionKind::FollowUp));
    }

    #[test]
    fn classifies_add_to_plan_as_draft_action() {
        let c = classify("add item X to the plan", false);
        assert_eq!(c.intent, AdvisorIntent::DraftAction);
        assert_eq!(c.draft_action_kind, Some(DraftActionKind::AddToPlan));
    }

    #[test]
    fn apply_outside_draft_context_is_ambiguous() {
        let c = classify("apply", false);
        assert_eq!(c.intent, AdvisorIntent::Ambiguous);
    }

    #[test]
    fn apply_inside_draft_context_is_draft_action() {
        let c = classify("apply", true);
        assert_eq!(c.intent, AdvisorIntent::DraftAction);
        assert_eq!(c.draft_action_kind, Some(DraftActionKind::ApplyOrDeny));
    }

    #[test]
    fn low_confidence_falls_back_to_ambiguous() {
        let c = classify("hmm interesting", false);
        assert_eq!(c.intent, AdvisorIntent::Ambiguous);
    }

    #[test]
    fn confirmation_card_only_for_queue_goal() {
        let queue = classify("fix the flaky login test", false);
        assert!(build_confirmation_card(&queue, Some("v0.17.0.12.6")).is_some());

        let info = classify("what is the current phase?", false);
        assert!(build_confirmation_card(&info, None).is_none());
    }

    #[test]
    fn clarify_state_exhausts_after_max_rounds() {
        let mut state = ClarifyState::default();
        assert!(matches!(
            next_clarify_step(state),
            ClarifyOutcome::Options { .. }
        ));
        state = state.next();
        assert!(matches!(
            next_clarify_step(state),
            ClarifyOutcome::Options { .. }
        ));
        state = state.next();
        assert!(matches!(
            next_clarify_step(state),
            ClarifyOutcome::NeedMoreInfo
        ));
    }
}
