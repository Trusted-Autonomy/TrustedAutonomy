// resolve.rs — Pure, read-only phase matching by title and `next` selection.
//
// `ta run "<title>"` with no `--phase` used to fall through to an ad-hoc
// `in_progress` stub when the title carried no version token. This module is
// the scoring half of the fix: given free text and the parsed plan, say which
// phase it names, and whether that answer is safe to act on without asking.
//
// Nothing here touches the filesystem, PLAN.md or the claim registry. The
// caller (`ta run`, `ta plan resolve`) supplies the live claims it knows about
// and decides what to do with an unconfident answer.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::parse::phase_ids_match;
use crate::schema::{PlanPhase, PlanStatus};

/// A title match is confident only if the best eligible candidate scores at
/// least this much (inclusive) and no other eligible candidate also does.
pub const MIN_CONFIDENT_SCORE: f64 = 0.6;

/// ...and it beats the runner-up (best other eligible candidate) by at least
/// this much (inclusive).
pub const MIN_CONFIDENT_MARGIN: f64 = 0.15;

/// Candidates scoring below this are noise and never listed.
pub const MIN_LISTED_SCORE: f64 = 0.2;

/// How many candidates a block message or JSON outcome lists.
pub const MAX_LISTED_CANDIDATES: usize = 5;

/// Tolerance so `0.75 - 0.6 >= 0.15` is not lost to float rounding.
const EPSILON: f64 = 1e-9;

/// Weight of "how much of what the user typed is covered" versus "how much of
/// the phase title is covered". Typed titles are short and phase titles long,
/// so query coverage dominates.
const QUERY_COVERAGE_WEIGHT: f64 = 0.75;

/// Words that carry no identity. `ta` and `phase` appear in most titles in this
/// repository, so matching on them would make everything a candidate.
const STOP_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "of",
    "for",
    "to",
    "in",
    "on",
    "at",
    "by",
    "with",
    "from",
    "into",
    "is",
    "are",
    "be",
    "as",
    "it",
    "its",
    "this",
    "that",
    "these",
    "those",
    "via",
    "vs",
    "per",
    "ta",
    "phase",
    "implement",
    "add",
];

/// A phase a live goal currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedPhase {
    pub phase_id: String,
    pub goal_id: String,
}

/// One scored phase. `excluded` is set when the phase matched but may not be
/// chosen (done, deferred, held by a live goal, ...) and says why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub status: String,
    pub score: f64,
    pub excluded: Option<String>,
}

impl Candidate {
    pub fn is_eligible(&self) -> bool {
        self.excluded.is_none()
    }
}

/// Result of matching free text against phase titles.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleMatch {
    /// Scored phases at or above [`MIN_LISTED_SCORE`], best first, eligible and
    /// excluded together. Capped at [`MAX_LISTED_CANDIDATES`].
    pub candidates: Vec<Candidate>,
    /// Index into `candidates` of the confident answer, if there is one.
    pub confident: Option<usize>,
}

impl TitleMatch {
    pub fn chosen(&self) -> Option<&Candidate> {
        self.confident.and_then(|i| self.candidates.get(i))
    }
}

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bv?\d+(?:\.\d+)+[a-z]?\b|\bv\d+\b").expect("static regex"))
}

fn stem(word: &str) -> String {
    if word.len() > 4 && word.ends_with("ies") {
        return format!("{}y", &word[..word.len() - 3]);
    }
    if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

/// Lower-cased content words of `text`: version tokens, stop words and
/// one-letter words removed, plurals folded.
fn content_words(text: &str) -> BTreeSet<String> {
    let without_versions = version_re().replace_all(text, " ");
    without_versions
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !STOP_WORDS.contains(w))
        .map(stem)
        .collect()
}

/// Similarity in `0.0..=1.0` between what the user typed and a phase title.
pub fn score_title(query: &str, phase_title: &str) -> f64 {
    let q = content_words(query);
    let p = content_words(phase_title);
    if q.is_empty() || p.is_empty() {
        return 0.0;
    }
    let shared = q.intersection(&p).count() as f64;
    let score = QUERY_COVERAGE_WEIGHT * (shared / q.len() as f64)
        + (1.0 - QUERY_COVERAGE_WEIGHT) * (shared / p.len() as f64);
    score.min(1.0)
}

/// The confidence rule on the scores of the *eligible* candidates (any order):
/// exactly one reaches [`MIN_CONFIDENT_SCORE`] and it leads the runner-up by at
/// least [`MIN_CONFIDENT_MARGIN`].
pub fn is_confident(eligible_scores: &[f64]) -> bool {
    let mut sorted: Vec<f64> = eligible_scores.to_vec();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let Some(&top) = sorted.first() else {
        return false;
    };
    if top + EPSILON < MIN_CONFIDENT_SCORE {
        return false;
    }
    let above = sorted
        .iter()
        .filter(|s| **s + EPSILON >= MIN_CONFIDENT_SCORE)
        .count();
    if above != 1 {
        return false;
    }
    let runner_up = sorted.get(1).copied().unwrap_or(0.0);
    top - runner_up + EPSILON >= MIN_CONFIDENT_MARGIN
}

/// Why `phase` may not be chosen, or `None` when it can be.
pub fn exclusion_reason(phase: &PlanPhase, claims: &[ClaimedPhase]) -> Option<String> {
    if let Some(claim) = claims
        .iter()
        .find(|c| phase_ids_match(&phase.id, &c.phase_id))
    {
        return Some(format!("held by live goal {}", claim.goal_id));
    }
    match phase.status {
        PlanStatus::Done => Some("already done".to_string()),
        PlanStatus::Deferred => Some("deferred".to_string()),
        PlanStatus::Blocked => Some("blocked".to_string()),
        PlanStatus::Pending | PlanStatus::InProgress => None,
    }
}

/// Build a [`Candidate`] for `phase` with an explicit score.
pub fn candidate_for(phase: &PlanPhase, score: f64, claims: &[ClaimedPhase]) -> Candidate {
    Candidate {
        id: phase.id.clone(),
        title: phase.title.clone(),
        status: phase.status.to_string(),
        score,
        excluded: exclusion_reason(phase, claims),
    }
}

/// Match free text against every phase title.
///
/// Done, deferred and live-claimed phases are scored too so the caller can say
/// "your best match was X but it is done". They never count toward confidence,
/// and a better-scoring excluded phase blocks a weaker eligible one: choosing
/// the runner-up would silently ignore what the user most likely meant.
pub fn match_title(title: &str, phases: &[PlanPhase], claims: &[ClaimedPhase]) -> TitleMatch {
    let mut scored: Vec<Candidate> = phases
        .iter()
        .map(|p| candidate_for(p, score_title(title, &p.title), claims))
        .filter(|c| c.score >= MIN_LISTED_SCORE)
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let eligible_scores: Vec<f64> = scored
        .iter()
        .filter(|c| c.is_eligible())
        .map(|c| c.score)
        .collect();
    let best_excluded = scored
        .iter()
        .filter(|c| !c.is_eligible())
        .map(|c| c.score)
        .fold(0.0_f64, f64::max);

    let mut confident = None;
    if is_confident(&eligible_scores) {
        let idx = scored.iter().position(|c| c.is_eligible());
        let top = idx.map(|i| scored[i].score).unwrap_or(0.0);
        if best_excluded + EPSILON < top {
            confident = idx;
        }
    }

    scored.truncate(MAX_LISTED_CANDIDATES);
    let confident = confident.filter(|i| *i < scored.len());
    TitleMatch {
        candidates: scored,
        confident,
    }
}

/// One phase that `next` skipped because a dependency is not done.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockedPhase {
    pub id: String,
    pub title: String,
    /// `"<dep id> (<status>)"` or `"<dep id> (not in PLAN.md)"`.
    pub blocked_by: Vec<String>,
}

/// Why `next` found nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct NextBlocked {
    pub blocked: Vec<BlockedPhase>,
    /// Pending phases skipped because a live goal holds them.
    pub claimed: Vec<String>,
    pub pending_total: usize,
}

/// The first pending phase in plan (document) order whose `Depends on` phases
/// are all done. `in_progress` phases and phases a live goal holds are skipped.
pub fn resolve_next(
    phases: &[PlanPhase],
    claims: &[ClaimedPhase],
) -> Result<Candidate, NextBlocked> {
    let mut blocked = Vec::new();
    let mut claimed = Vec::new();
    let mut pending_total = 0;

    for phase in phases.iter().filter(|p| p.status == PlanStatus::Pending) {
        pending_total += 1;
        let unmet: Vec<String> = phase
            .depends_on
            .iter()
            .filter_map(
                |dep| match phases.iter().find(|d| phase_ids_match(&d.id, dep)) {
                    Some(d) if d.status == PlanStatus::Done => None,
                    Some(d) => Some(format!("{} ({})", d.id, d.status)),
                    None => Some(format!("{dep} (not in PLAN.md)")),
                },
            )
            .collect();
        if !unmet.is_empty() {
            blocked.push(BlockedPhase {
                id: phase.id.clone(),
                title: phase.title.clone(),
                blocked_by: unmet,
            });
            continue;
        }
        if exclusion_reason(phase, claims).is_some() {
            claimed.push(phase.id.clone());
            continue;
        }
        return Ok(candidate_for(phase, 1.0, claims));
    }

    Err(NextBlocked {
        blocked,
        claimed,
        pending_total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(id: &str, title: &str, status: PlanStatus, deps: &[&str]) -> PlanPhase {
        PlanPhase {
            id: id.to_string(),
            title: title.to_string(),
            status,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            human_review_items: vec![],
            api_impact: vec![],
        }
    }

    fn claim(phase_id: &str) -> ClaimedPhase {
        ClaimedPhase {
            phase_id: phase_id.to_string(),
            goal_id: "goal-1".to_string(),
        }
    }

    #[test]
    fn identical_titles_score_one_and_unrelated_score_zero() {
        assert!((score_title("Golden set capture", "Golden Set: Capture") - 1.0).abs() < 1e-9);
        assert_eq!(score_title("signed releases", "Golden Set Capture"), 0.0);
    }

    #[test]
    fn version_tokens_stop_words_and_plurals_are_ignored() {
        let a = score_title("v0.17.12.25 the optimizers", "Optimizer Slots");
        let b = score_title("optimizer", "Optimizer Slots");
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn empty_or_all_stop_word_queries_score_zero() {
        assert_eq!(score_title("", "Anything"), 0.0);
        assert_eq!(score_title("the ta phase", "Anything"), 0.0);
    }

    // Boundaries of the confidence rule, expressed with the named constants.

    #[test]
    fn score_exactly_at_threshold_is_confident() {
        assert!(is_confident(&[MIN_CONFIDENT_SCORE]));
    }

    #[test]
    fn score_just_below_threshold_is_not_confident() {
        assert!(!is_confident(&[MIN_CONFIDENT_SCORE - 0.01]));
    }

    #[test]
    fn margin_exactly_at_limit_is_confident() {
        // Runner-up below the threshold, margin exactly the minimum.
        let top = MIN_CONFIDENT_SCORE + 0.1;
        assert!(is_confident(&[top, top - MIN_CONFIDENT_MARGIN]));
    }

    #[test]
    fn margin_just_under_limit_is_not_confident() {
        let top = MIN_CONFIDENT_SCORE + 0.1;
        assert!(!is_confident(&[top, top - MIN_CONFIDENT_MARGIN + 0.01]));
    }

    #[test]
    fn two_candidates_above_threshold_is_never_confident() {
        assert!(!is_confident(&[1.0, MIN_CONFIDENT_SCORE]));
    }

    #[test]
    fn no_candidates_is_not_confident() {
        assert!(!is_confident(&[]));
    }

    #[test]
    fn unique_close_title_is_confident() {
        let phases = vec![
            phase(
                "v1.0.1",
                "Golden Set: Capture and Replay Goals",
                PlanStatus::Pending,
                &[],
            ),
            phase("v1.0.2", "Signed Public Releases", PlanStatus::Pending, &[]),
        ];
        let m = match_title("golden set capture replay", &phases, &[]);
        assert_eq!(m.chosen().map(|c| c.id.as_str()), Some("v1.0.1"));
    }

    #[test]
    fn two_close_titles_are_not_confident() {
        let phases = vec![
            phase("v1.0.1", "Cost Report Export", PlanStatus::Pending, &[]),
            phase("v1.0.2", "Cost Report Meridian", PlanStatus::Pending, &[]),
        ];
        let m = match_title("cost report", &phases, &[]);
        assert!(m.chosen().is_none());
        assert_eq!(m.candidates.len(), 2);
    }

    #[test]
    fn done_phase_is_scored_but_excluded_and_blocks_a_weaker_eligible_match() {
        let phases = vec![
            phase("v1.0.1", "Golden Set Capture", PlanStatus::Done, &[]),
            phase(
                "v1.0.2",
                "Golden Set Dashboard Widgets Extra Words",
                PlanStatus::Pending,
                &[],
            ),
        ];
        let m = match_title("golden set capture", &phases, &[]);
        assert!(m.chosen().is_none(), "{m:?}");
        let best = &m.candidates[0];
        assert_eq!(best.id, "v1.0.1");
        assert_eq!(best.excluded.as_deref(), Some("already done"));
    }

    #[test]
    fn live_claimed_phase_is_excluded_with_goal_id() {
        let phases = vec![phase(
            "v1.0.1",
            "Golden Set Capture",
            PlanStatus::Pending,
            &[],
        )];
        let m = match_title("golden set capture", &phases, &[claim("v1.0.1")]);
        assert!(m.chosen().is_none());
        assert_eq!(
            m.candidates[0].excluded.as_deref(),
            Some("held by live goal goal-1")
        );
    }

    #[test]
    fn next_takes_first_pending_in_plan_order_with_deps_done() {
        let phases = vec![
            phase("v1.0.0", "Base", PlanStatus::Done, &[]),
            phase("v1.0.3", "Later id first in file", PlanStatus::Pending, &[]),
            phase("v1.0.1", "Earlier id second", PlanStatus::Pending, &[]),
        ];
        assert_eq!(resolve_next(&phases, &[]).unwrap().id, "v1.0.3");
    }

    #[test]
    fn next_skips_unmet_dependencies_in_progress_and_claimed() {
        let phases = vec![
            phase("v1.0.0", "Base", PlanStatus::Done, &[]),
            phase("v1.0.1", "Running", PlanStatus::InProgress, &[]),
            phase("v1.0.2", "Waits", PlanStatus::Pending, &["v1.0.1"]),
            phase("v1.0.3", "Held", PlanStatus::Pending, &["v1.0.0"]),
            phase("v1.0.4", "Free", PlanStatus::Pending, &["v1.0.0"]),
        ];
        let chosen = resolve_next(&phases, &[claim("v1.0.3")]).unwrap();
        assert_eq!(chosen.id, "v1.0.4");
    }

    #[test]
    fn next_names_blocking_dependencies_when_nothing_qualifies() {
        let phases = vec![
            phase("v1.0.1", "Running", PlanStatus::InProgress, &[]),
            phase(
                "v1.0.2",
                "Waits",
                PlanStatus::Pending,
                &["v1.0.1", "v9.9.9"],
            ),
        ];
        let blocked = resolve_next(&phases, &[]).unwrap_err();
        assert_eq!(blocked.pending_total, 1);
        assert_eq!(
            blocked.blocked[0].blocked_by,
            vec![
                "v1.0.1 (in_progress)".to_string(),
                "v9.9.9 (not in PLAN.md)".to_string()
            ]
        );
    }
}
