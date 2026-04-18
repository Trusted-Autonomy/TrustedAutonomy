// intent.rs — Intent classifier for the advisor agent (v0.15.19).
//
// Classifies human input during an advisor session into one of three intents.
// Used by the advisor in `auto` security mode to decide whether to fire a goal
// directly (threshold: ≥0.80 confidence).

use serde::{Deserialize, Serialize};

/// The classified intent of a human message during advisor conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Human wants to run a follow-up goal (e.g. "also add X", "fix Y").
    GoalRun,
    /// Human is asking a question about the draft or codebase.
    Question,
    /// Message is ambiguous; advisor should ask for clarification.
    Clarify,
    /// Human approved the draft (e.g. "apply", "looks good", "yes").
    Apply,
    /// Human declined the draft (e.g. "skip", "no", "don't apply").
    Deny,
}

/// Result of intent classification with confidence score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentResult {
    pub intent: Intent,
    /// Confidence in [0.0, 1.0]. In `auto` mode, ≥0.80 fires the intent directly.
    pub confidence: f32,
    /// Optional extracted goal prompt (set when intent is GoalRun).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_goal: Option<String>,
}

impl IntentResult {
    pub fn new(intent: Intent, confidence: f32) -> Self {
        Self {
            intent,
            confidence,
            extracted_goal: None,
        }
    }

    pub fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.extracted_goal = Some(goal.into());
        self
    }

    /// Returns true if this result meets the auto-fire threshold (≥80% confidence).
    pub fn is_auto_actionable(&self) -> bool {
        self.confidence >= 0.80
    }
}

/// Classify a human message into an intent.
///
/// This is a heuristic classifier based on keyword matching. In production the
/// advisor agent calls this as a structured tool, so the LLM provides the
/// confidence score directly. This implementation provides a deterministic
/// fallback and is used in tests.
pub fn classify_intent(message: &str) -> IntentResult {
    let lower = message.to_ascii_lowercase();
    let trimmed = lower.trim();

    // Deny / skip patterns — checked first to prevent "don't apply" matching apply.
    let deny_exact = [
        "skip", "no", "nope", "reject", "deny", "cancel", "abort", "stop",
    ];
    let deny_phrases = [
        "don't apply",
        "do not apply",
        "never mind",
        "no thanks",
        "don't do it",
        "please don't",
        "skip this",
    ];
    for kw in &deny_exact {
        if trimmed == *kw {
            return IntentResult::new(Intent::Deny, 0.95);
        }
    }
    for phrase in &deny_phrases {
        if trimmed.starts_with(phrase) || trimmed == *phrase {
            return IntentResult::new(Intent::Deny, 0.95);
        }
    }

    // Apply / approve patterns — checked after deny to avoid false positives.
    let apply_exact = ["apply", "approve", "yes", "lgtm", "ok", "okay", "merge"];
    let apply_phrases = [
        "looks good",
        "ship it",
        "go ahead",
        "proceed",
        "do it",
        "apply it",
        "yes please",
        "approved",
        "go for it",
    ];
    for kw in &apply_exact {
        if trimmed == *kw {
            return IntentResult::new(Intent::Apply, 0.95);
        }
    }
    for phrase in &apply_phrases {
        if trimmed.starts_with(phrase) || trimmed == *phrase {
            return IntentResult::new(Intent::Apply, 0.95);
        }
    }

    // Goal-run patterns: imperative modification requests.
    let goal_prefixes = [
        "also ",
        "additionally ",
        "please also ",
        "can you also ",
        "add ",
        "fix ",
        "change ",
        "update ",
        "remove ",
        "refactor ",
        "make it ",
        "could you ",
        "while you're at it",
    ];
    for prefix in &goal_prefixes {
        if trimmed.starts_with(prefix) {
            let goal = trimmed.trim_start_matches(prefix).trim().to_string();
            return IntentResult::new(Intent::GoalRun, 0.85).with_goal(goal);
        }
    }

    // Question patterns.
    let question_keywords = [
        "why",
        "what",
        "how",
        "explain",
        "show me",
        "tell me",
        "what does",
        "what is",
        "can you explain",
        "what changed",
    ];
    for kw in &question_keywords {
        if trimmed.starts_with(kw) {
            return IntentResult::new(Intent::Question, 0.85);
        }
    }
    if trimmed.ends_with('?') {
        return IntentResult::new(Intent::Question, 0.75);
    }

    // Default: needs clarification.
    IntentResult::new(Intent::Clarify, 0.50)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_apply_variants() {
        assert_eq!(classify_intent("apply").intent, Intent::Apply);
        assert_eq!(classify_intent("yes").intent, Intent::Apply);
        assert_eq!(classify_intent("looks good").intent, Intent::Apply);
        assert_eq!(classify_intent("LGTM").intent, Intent::Apply);
        assert!(classify_intent("apply").confidence >= 0.80);
    }

    #[test]
    fn classify_deny_variants() {
        assert_eq!(classify_intent("skip").intent, Intent::Deny);
        assert_eq!(classify_intent("no").intent, Intent::Deny);
        assert_eq!(classify_intent("don't apply").intent, Intent::Deny);
        assert!(classify_intent("no").confidence >= 0.80);
    }

    #[test]
    fn classify_goal_run() {
        let result = classify_intent("also add a test for the new endpoint");
        assert_eq!(result.intent, Intent::GoalRun);
        assert!(result.confidence >= 0.80);
        assert!(result.extracted_goal.is_some());
        assert!(result.is_auto_actionable());
    }

    #[test]
    fn classify_question() {
        assert_eq!(
            classify_intent("why did you use async here?").intent,
            Intent::Question
        );
        assert_eq!(
            classify_intent("what changed in auth.rs?").intent,
            Intent::Question
        );
        assert_eq!(
            classify_intent("what does this do?").intent,
            Intent::Question
        );
    }

    #[test]
    fn classify_clarify_fallback() {
        let result = classify_intent("hmm interesting");
        assert_eq!(result.intent, Intent::Clarify);
        assert!(!result.is_auto_actionable());
    }

    #[test]
    fn intent_result_auto_actionable_threshold() {
        assert!(IntentResult::new(Intent::Apply, 0.95).is_auto_actionable());
        assert!(IntentResult::new(Intent::GoalRun, 0.80).is_auto_actionable());
        assert!(!IntentResult::new(Intent::Clarify, 0.79).is_auto_actionable());
    }

    #[test]
    fn intent_serialization() {
        let result =
            IntentResult::new(Intent::GoalRun, 0.85).with_goal("add tests for the auth module");
        let json = serde_json::to_string(&result).unwrap();
        let restored: IntentResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.intent, Intent::GoalRun);
        assert_eq!(
            restored.extracted_goal.as_deref(),
            Some("add tests for the auth module")
        );
    }
}
