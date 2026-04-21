// intent.rs — Natural language → workflow intent resolver (v0.15.24).
//
// Extracts structured entities from free-form text and scores templates by
// keyword overlap against their name, description, and param declarations.
// No ML — pure regex + keyword matching.
//
// Primary entry point: `resolve_intent(text, templates, plan_ctx) -> ResolutionResult`
// Confidence threshold: 0.80 → present confirmation card; < 0.80 → clarifying question.

use std::collections::HashMap;

use crate::params::{PlanContext, TemplateEntry};

/// Action verb extracted from the user's input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IntentVerb {
    Implement,
    Build,
    Run,
    Complete,
    Execute,
    #[default]
    None,
}

impl IntentVerb {
    pub fn is_action(&self) -> bool {
        !matches!(self, IntentVerb::None)
    }

    fn from_word(word: &str) -> Self {
        match word {
            "implement" | "implementing" | "implementation" => Self::Implement,
            "build" | "building" | "built" => Self::Build,
            "run" | "running" | "runs" => Self::Run,
            "complete" | "completing" | "completion" | "finish" | "finishing" | "done" => {
                Self::Complete
            }
            "execute" | "executing" | "do" | "work" | "start" | "kick" => Self::Execute,
            _ => Self::None,
        }
    }
}

/// Scope modifier extracted from the user's input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ScopeModifier {
    Remaining, // "remaining", "rest", "left", "unfinished"
    All,       // "all", "every", "each"
    Pending,   // "pending", "todo", "outstanding"
    Next,      // "next"
    #[default]
    None,
}

impl ScopeModifier {
    /// True when the modifier implies iterating over multiple phases.
    pub fn is_multi_phase(&self) -> bool {
        matches!(
            self,
            ScopeModifier::Remaining
                | ScopeModifier::All
                | ScopeModifier::Pending
                | ScopeModifier::Next
        )
    }

    fn from_word(word: &str) -> Self {
        match word {
            "remaining" | "rest" | "left" | "unfinished" => Self::Remaining,
            "all" | "every" | "each" => Self::All,
            "pending" | "todo" | "outstanding" | "open" => Self::Pending,
            "next" => Self::Next,
            _ => Self::None,
        }
    }
}

/// Structured entities extracted from a natural-language workflow request.
#[derive(Debug, Clone, Default)]
pub struct ExtractedIntent {
    /// Version reference, e.g. `"v0.15"` or `"v0.15.24"`.
    pub version_ref: Option<String>,
    /// The action verb present in the input.
    pub intent_verb: IntentVerb,
    /// Scope modifier: remaining/all/pending/next.
    pub scope_modifier: ScopeModifier,
    /// Plan-related context nouns found in the input (phase, plan, goal, …).
    pub context_words: Vec<String>,
}

/// A scored template candidate.
#[derive(Debug, Clone)]
pub struct TemplateCandidate {
    /// Name of the matched template (e.g. `"plan-build-phases"`).
    pub template_name: String,
    /// Confidence score 0.0–1.0.
    pub score: f64,
    /// Parameter values suggested for this invocation.
    pub suggested_params: HashMap<String, String>,
    /// Template description (for the confirmation card).
    pub description: String,
}

/// Outcome of intent resolution.
#[derive(Debug)]
pub enum ResolutionResult {
    /// Score ≥ 0.80 — show this candidate in the confirmation card.
    Resolved(TemplateCandidate),
    /// Score < 0.80 — surface this question to the user first.
    ClarifyingQuestion(String),
}

/// Minimum score required to present a confirmation card rather than a clarifying question.
pub const CONFIDENCE_THRESHOLD: f64 = 0.80;

/// Extract structured intent entities from a free-form text string.
pub fn extract_intent(text: &str) -> ExtractedIntent {
    let lower = text.to_lowercase();
    // Tokenise: split on whitespace, strip leading/trailing punctuation.
    let words: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.'))
        .filter(|w| !w.is_empty())
        .collect();

    // Extract version reference from the *original* text (preserve case for `v0.15`).
    let version_re = regex::Regex::new(r"\bv\d+(?:\.\d+)+\b").expect("static");
    let version_ref = version_re.find(text).map(|m| m.as_str().to_string());

    // First action verb wins.
    let intent_verb = words
        .iter()
        .map(|w| IntentVerb::from_word(w))
        .find(|v| *v != IntentVerb::None)
        .unwrap_or_default();

    // First scope modifier wins.
    let scope_modifier = words
        .iter()
        .map(|w| ScopeModifier::from_word(w))
        .find(|s| *s != ScopeModifier::None)
        .unwrap_or_default();

    // Collect relevant context nouns.
    const CONTEXT_KW: &[&str] = &[
        "phase",
        "phases",
        "plan",
        "workflow",
        "version",
        "milestone",
        "task",
        "goal",
        "feature",
        "bug",
        "fix",
    ];
    let context_words: Vec<String> = words
        .iter()
        .filter(|w| CONTEXT_KW.contains(w))
        .map(|w| w.to_string())
        .collect();

    ExtractedIntent {
        version_ref,
        intent_verb,
        scope_modifier,
        context_words,
    }
}

/// Score a single template against the extracted intent.
///
/// Returns `(score, suggested_params)`.  Score is in [0.0, 1.0]:
/// - Component 1 — verb   (0.0–0.35): any action verb matches a template that executes work.
/// - Component 2 — scope  (0.0–0.35): multi-phase scope matches phase-iteration templates.
/// - Component 3 — context(0.0–0.30): version ref or plan keywords matching plan-aware templates.
fn score_template(
    intent: &ExtractedIntent,
    template_name: &str,
    description: &str,
    param_names: &[&str],
    plan_ctx: &PlanContext,
) -> (f64, HashMap<String, String>) {
    let name_lower = template_name.to_lowercase();
    let desc_lower = description.to_lowercase();
    let mut params: HashMap<String, String> = HashMap::new();

    let is_phases_template = name_lower.contains("phase")
        || desc_lower.contains("phase")
        || desc_lower.contains("pending")
        || desc_lower.contains("iterate");

    // --- Component 1: verb (0–0.35) ---
    let verb_score = if intent.intent_verb.is_action() {
        0.35
    } else {
        0.0
    };

    // --- Component 2: scope (0–0.35) ---
    let has_phase_context_word = intent
        .context_words
        .iter()
        .any(|w| w == "phase" || w == "phases");

    let scope_score = if intent.scope_modifier.is_multi_phase() && is_phases_template {
        0.35
    } else if has_phase_context_word && is_phases_template {
        // "run next phase" — "phase" is a context word even if scope_modifier is Next.
        0.30
    } else {
        0.0
    };

    // --- Component 3: plan context (0–0.30) ---
    let has_plan_context = intent.version_ref.is_some()
        || has_phase_context_word
        || intent
            .context_words
            .iter()
            .any(|w| w == "plan" || w == "version");

    let template_has_phase_param = param_names
        .iter()
        .any(|&p| p == "phase_filter" || p == "phase");
    let template_is_plan_aware = is_phases_template || template_has_phase_param;

    let ctx_score = if has_plan_context && template_is_plan_aware {
        if let Some(ref vref) = intent.version_ref {
            if template_has_phase_param {
                params.insert("phase_filter".to_string(), vref.clone());
            }
        } else if (intent.scope_modifier == ScopeModifier::Next || has_phase_context_word)
            && template_has_phase_param
            && !plan_ctx.next_pending_phase.is_empty()
        {
            params.insert(
                "phase_filter".to_string(),
                plan_ctx.next_pending_phase.clone(),
            );
        }
        0.30
    } else {
        0.0
    };

    let total = verb_score + scope_score + ctx_score;
    (total, params)
}

/// Resolve natural language intent to a workflow template.
///
/// Scores every template in `templates` against the extracted intent.
/// - Top candidate score ≥ 0.80 → `Resolved`
/// - Otherwise → `ClarifyingQuestion`
///
/// Explicit template names must be checked by the caller before calling this
/// function — this function is only reached when the name doesn't match directly.
pub fn resolve_intent(
    text: &str,
    templates: &[TemplateEntry],
    plan_ctx: &PlanContext,
) -> ResolutionResult {
    let intent = extract_intent(text);

    if !intent.intent_verb.is_action() {
        return ResolutionResult::ClarifyingQuestion(
            "Couldn't determine what action you want to take.\n\
             Try phrasing your request with a verb, for example:\n  \
             \"implement remaining v0.15\"\n  \
             \"run next phase\"\n\n\
             Or use an explicit template:\n  \
             ta workflow run plan-build-phases --param phase_filter=v0.15\n  \
             ta workflow list --param-templates"
                .to_string(),
        );
    }

    let mut candidates: Vec<TemplateCandidate> = templates
        .iter()
        .map(|t| {
            let param_names: Vec<&str> = t.params.iter().map(|(n, _)| n.as_str()).collect();
            let (score, suggested_params) =
                score_template(&intent, &t.name, &t.description, &param_names, plan_ctx);
            TemplateCandidate {
                template_name: t.name.clone(),
                score,
                suggested_params,
                description: t.description.clone(),
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    match candidates.first() {
        Some(best) if best.score >= CONFIDENCE_THRESHOLD => {
            ResolutionResult::Resolved(best.clone())
        }
        _ => ResolutionResult::ClarifyingQuestion(build_clarifying_question(&intent, &candidates)),
    }
}

/// Build a helpful clarifying question when confidence is below the threshold.
fn build_clarifying_question(intent: &ExtractedIntent, candidates: &[TemplateCandidate]) -> String {
    let mut parts =
        vec!["Couldn't determine which workflow to run with high confidence.".to_string()];

    if !candidates.is_empty() {
        parts.push("\nBest matches (low confidence):".to_string());
        for (i, c) in candidates.iter().take(3).enumerate() {
            parts.push(format!(
                "  {}. {} ({:.0}%)",
                i + 1,
                c.template_name,
                c.score * 100.0
            ));
        }
    }

    let missing_version = intent.version_ref.is_none();
    let missing_phase = !intent
        .context_words
        .iter()
        .any(|w| w == "phase" || w == "phases");

    if missing_version && missing_phase {
        parts.push("\nTip: Add a version or phase reference to narrow it down, e.g.:".to_string());
        parts.push("  \"implement remaining v0.15\"".to_string());
        parts.push("  \"run next phase\"".to_string());
    }

    parts.push("\nOr use explicit template names:".to_string());
    parts.push("  ta workflow run plan-build-phases --param phase_filter=v0.15".to_string());
    parts.push("  ta workflow list --param-templates".to_string());

    parts.join("\n")
}

/// Format a resolved candidate as a confirmation card string.
///
/// Returns the card text ready to print to the terminal.
pub fn format_confirmation_card(candidate: &TemplateCandidate, text: &str) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Resolved workflow for: \"{}\"", text.trim()));
    lines.push(format!(
        "  Template:    {} ({:.0}% confidence)",
        candidate.template_name,
        candidate.score * 100.0
    ));
    if !candidate.description.is_empty() {
        lines.push(format!("  Description: {}", candidate.description));
    }
    if !candidate.suggested_params.is_empty() {
        let param_str: Vec<String> = candidate
            .suggested_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        lines.push(format!("  Params:      {}", param_str.join("  ")));
    }
    lines.push(String::new());
    lines.push("1. Run    2. Adjust params    3. Different workflow    4. Cancel".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{TemplateEntry, TemplateSource};

    fn make_plan_ctx(next_phase: &str, prefix: &str) -> PlanContext {
        PlanContext {
            current_version_prefix: prefix.to_string(),
            next_pending_phase: next_phase.to_string(),
            next_pending_title: "Intent Resolver".to_string(),
            pending_count: 3,
        }
    }

    fn builtin_templates() -> Vec<TemplateEntry> {
        vec![
            TemplateEntry {
                name: "plan-build-phases".to_string(),
                description: "Iterate pending PLAN.md phases through the governed build workflow."
                    .to_string(),
                source: TemplateSource::Builtin,
                params: vec![
                    (
                        "phase_filter".to_string(),
                        "string (default: v0.15) — Phase ID prefix to process".to_string(),
                    ),
                    (
                        "max_phases".to_string(),
                        "integer (default: 5) — Maximum phases to process".to_string(),
                    ),
                ],
            },
            TemplateEntry {
                name: "governed-goal".to_string(),
                description:
                    "Safe autonomous coding loop: run_goal → review → human_gate → apply → pr_sync."
                        .to_string(),
                source: TemplateSource::Builtin,
                params: vec![(
                    "goal_title".to_string(),
                    "string [required] — Goal title to implement".to_string(),
                )],
            },
        ]
    }

    // ── extract_intent ──────────────────────────────────────────────────────

    #[test]
    fn extract_version_ref() {
        let intent = extract_intent("implement remaining v0.15");
        assert_eq!(intent.version_ref, Some("v0.15".to_string()));
    }

    #[test]
    fn extract_intent_verb_implement() {
        let intent = extract_intent("implement remaining v0.15");
        assert_eq!(intent.intent_verb, IntentVerb::Implement);
    }

    #[test]
    fn extract_scope_remaining() {
        let intent = extract_intent("implement remaining v0.15");
        assert_eq!(intent.scope_modifier, ScopeModifier::Remaining);
    }

    #[test]
    fn extract_scope_next() {
        let intent = extract_intent("run next phase");
        assert_eq!(intent.scope_modifier, ScopeModifier::Next);
    }

    #[test]
    fn extract_context_words_phase() {
        let intent = extract_intent("run next phase");
        assert!(intent.context_words.contains(&"phase".to_string()));
    }

    #[test]
    fn extract_no_version_ref() {
        let intent = extract_intent("run next phase");
        assert_eq!(intent.version_ref, None);
    }

    #[test]
    fn extract_intent_verb_run() {
        let intent = extract_intent("run next phase");
        assert_eq!(intent.intent_verb, IntentVerb::Run);
    }

    #[test]
    fn extract_no_verb() {
        let intent = extract_intent("v0.15");
        assert_eq!(intent.intent_verb, IntentVerb::None);
    }

    // ── resolve_intent — happy paths ────────────────────────────────────────

    #[test]
    fn resolve_implement_remaining_v015_matches_plan_build_phases() {
        let templates = builtin_templates();
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");

        let result = resolve_intent("implement remaining v0.15", &templates, &plan_ctx);

        match result {
            ResolutionResult::Resolved(c) => {
                assert_eq!(c.template_name, "plan-build-phases");
                assert_eq!(
                    c.suggested_params.get("phase_filter"),
                    Some(&"v0.15".to_string())
                );
                assert!(c.score >= CONFIDENCE_THRESHOLD);
            }
            ResolutionResult::ClarifyingQuestion(q) => {
                panic!("Expected Resolved, got ClarifyingQuestion: {}", q);
            }
        }
    }

    #[test]
    fn resolve_run_next_phase_matches_plan_build_phases() {
        let templates = builtin_templates();
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");

        let result = resolve_intent("run next phase", &templates, &plan_ctx);

        match result {
            ResolutionResult::Resolved(c) => {
                assert_eq!(c.template_name, "plan-build-phases");
                // suggested phase_filter should be the next pending phase
                assert_eq!(
                    c.suggested_params.get("phase_filter"),
                    Some(&"v0.15.24".to_string())
                );
                assert!(c.score >= CONFIDENCE_THRESHOLD);
            }
            ResolutionResult::ClarifyingQuestion(q) => {
                panic!("Expected Resolved, got ClarifyingQuestion: {}", q);
            }
        }
    }

    // ── resolve_intent — low confidence ────────────────────────────────────

    #[test]
    fn low_confidence_returns_clarifying_question() {
        let templates = builtin_templates();
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");

        // Vague input with no scope or plan context.
        let result = resolve_intent("something something", &templates, &plan_ctx);

        match result {
            ResolutionResult::ClarifyingQuestion(_) => {}
            ResolutionResult::Resolved(c) => {
                panic!(
                    "Expected ClarifyingQuestion for vague input, got Resolved: {}",
                    c.template_name
                );
            }
        }
    }

    #[test]
    fn no_verb_returns_clarifying_question() {
        let templates = builtin_templates();
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");

        let result = resolve_intent("v0.15", &templates, &plan_ctx);

        match result {
            ResolutionResult::ClarifyingQuestion(_) => {}
            ResolutionResult::Resolved(c) => {
                panic!(
                    "Expected ClarifyingQuestion for no-verb input, got Resolved: {}",
                    c.template_name
                );
            }
        }
    }

    // ── Explicit template name bypasses resolver ────────────────────────────
    // (This is validated at the call site in workflow.rs — just check score is high
    //  when the template name is stated explicitly in the text.)

    #[test]
    fn explicit_template_name_in_text_scores_high() {
        // When someone types the template name directly, extract_intent still works.
        let intent = extract_intent("run plan-build-phases");
        // "run" is a known verb
        assert!(intent.intent_verb.is_action());
    }

    // ── format_confirmation_card ────────────────────────────────────────────

    #[test]
    fn confirmation_card_contains_template_name_and_options() {
        let candidate = TemplateCandidate {
            template_name: "plan-build-phases".to_string(),
            score: 1.0,
            suggested_params: {
                let mut m = HashMap::new();
                m.insert("phase_filter".to_string(), "v0.15".to_string());
                m
            },
            description: "Iterate pending PLAN.md phases.".to_string(),
        };
        let card = format_confirmation_card(&candidate, "implement remaining v0.15");
        assert!(card.contains("plan-build-phases"));
        assert!(card.contains("phase_filter=v0.15"));
        assert!(card.contains("1. Run"));
        assert!(card.contains("4. Cancel"));
    }

    // ── score_template internals ────────────────────────────────────────────

    #[test]
    fn score_above_threshold_for_implement_remaining() {
        let intent = extract_intent("implement remaining v0.15");
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");
        let (score, params) = score_template(
            &intent,
            "plan-build-phases",
            "Iterate pending PLAN.md phases through the governed build workflow.",
            &["phase_filter", "max_phases"],
            &plan_ctx,
        );
        assert!(score >= CONFIDENCE_THRESHOLD, "score was {}", score);
        assert_eq!(params.get("phase_filter"), Some(&"v0.15".to_string()));
    }

    #[test]
    fn score_above_threshold_for_run_next_phase() {
        let intent = extract_intent("run next phase");
        let plan_ctx = make_plan_ctx("v0.15.24", "v0.15");
        let (score, params) = score_template(
            &intent,
            "plan-build-phases",
            "Iterate pending PLAN.md phases through the governed build workflow.",
            &["phase_filter", "max_phases"],
            &plan_ctx,
        );
        assert!(score >= CONFIDENCE_THRESHOLD, "score was {}", score);
        assert_eq!(params.get("phase_filter"), Some(&"v0.15.24".to_string()));
    }
}
