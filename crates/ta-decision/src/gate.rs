use serde::{Deserialize, Serialize};

/// Outcome of an automated review step, prior to the Decision gate.
///
/// This mirrors `ta-changeset::SupervisorVerdict` but lives here so any
/// reviewer (AI supervisor, social content check, email reply check, DB
/// mutation classifier) can produce one without depending on ta-changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Warn,
    Block,
}

/// The three signals every Write/Review step in TA produces, regardless of
/// which subsystem (draft apply, social publish, email send, DB mutation)
/// is doing the reviewing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DecisionInput {
    pub verdict: Verdict,
    /// 0-100, higher is riskier.
    pub risk_score: u32,
    /// 0.0-1.0, confidence the review itself had in its verdict.
    pub confidence: f64,
}

/// Thresholds an application configures to control where the Decision gate
/// draws its lines. Same shape everywhere; each call site supplies its own
/// values (e.g. social content can be stricter than an internal DB mutation).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DecisionThresholds {
    /// Below this confidence, a `Pass`/`Warn` verdict is escalated to a human
    /// rather than trusted.
    pub min_confidence: f64,
    /// Above this risk score (but below `escalate_risk_score`), a `Pass`
    /// verdict is downgraded to `Rework` instead of auto-committing.
    pub max_risk_score: u32,
    /// At or above this risk score, the action is always escalated to a
    /// human regardless of verdict or confidence (except `Block`, which is
    /// always a `Reject` — nothing outranks an explicit block).
    pub escalate_risk_score: u32,
}

impl Default for DecisionThresholds {
    fn default() -> Self {
        Self {
            min_confidence: 0.7,
            max_risk_score: 40,
            escalate_risk_score: 75,
        }
    }
}

/// The four outcomes of the Decision gate. `Commit` and `Reject` are
/// terminal; `Rework` sends the action back to Write; `Escalate` hands the
/// decision to a human (auto-approval is withheld).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Commit,
    Reject,
    Rework,
    Escalate,
}

impl Decision {
    /// True for the only outcome that is allowed to proceed without a human.
    pub fn is_auto_approvable(self) -> bool {
        matches!(self, Decision::Commit)
    }
}

/// The one generic Decision gate every Write -> Review -> Decision ->
/// Commit/Reject instantiation in TA should call, instead of re-implementing
/// its own pass/fail threshold check.
///
/// `Block` always rejects, irrespective of risk or confidence — nothing
/// overrides an explicit block. Otherwise, risk at or above
/// `escalate_risk_score` always escalates. A `Warn` verdict reworks unless
/// confidence is too low to trust reworking it automatically, in which case
/// it escalates. A `Pass` verdict commits unless risk is elevated (reworked)
/// or confidence is too low (escalated).
pub fn decide(input: &DecisionInput, thresholds: &DecisionThresholds) -> Decision {
    if input.verdict == Verdict::Block {
        return Decision::Reject;
    }
    if input.risk_score >= thresholds.escalate_risk_score {
        return Decision::Escalate;
    }
    match input.verdict {
        Verdict::Block => unreachable!("handled above"),
        Verdict::Warn => {
            if input.confidence < thresholds.min_confidence {
                Decision::Escalate
            } else {
                Decision::Rework
            }
        }
        Verdict::Pass => {
            if input.risk_score > thresholds.max_risk_score {
                Decision::Rework
            } else if input.confidence < thresholds.min_confidence {
                Decision::Escalate
            } else {
                Decision::Commit
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(verdict: Verdict, risk_score: u32, confidence: f64) -> DecisionInput {
        DecisionInput {
            verdict,
            risk_score,
            confidence,
        }
    }

    #[test]
    fn block_always_rejects() {
        let t = DecisionThresholds::default();
        assert_eq!(decide(&input(Verdict::Block, 0, 1.0), &t), Decision::Reject);
        assert_eq!(
            decide(&input(Verdict::Block, 100, 0.0), &t),
            Decision::Reject
        );
    }

    #[test]
    fn pass_with_low_risk_and_high_confidence_commits() {
        let t = DecisionThresholds::default();
        assert_eq!(
            decide(&input(Verdict::Pass, 10, 0.95), &t),
            Decision::Commit
        );
    }

    #[test]
    fn pass_with_elevated_risk_reworks() {
        let t = DecisionThresholds::default();
        assert_eq!(
            decide(&input(Verdict::Pass, 50, 0.95), &t),
            Decision::Rework
        );
    }

    #[test]
    fn pass_with_low_confidence_escalates() {
        let t = DecisionThresholds::default();
        assert_eq!(
            decide(&input(Verdict::Pass, 10, 0.4), &t),
            Decision::Escalate
        );
    }

    #[test]
    fn very_high_risk_always_escalates_even_if_pass() {
        let t = DecisionThresholds::default();
        assert_eq!(
            decide(&input(Verdict::Pass, 90, 0.99), &t),
            Decision::Escalate
        );
    }

    #[test]
    fn warn_with_sufficient_confidence_reworks() {
        let t = DecisionThresholds::default();
        assert_eq!(decide(&input(Verdict::Warn, 10, 0.9), &t), Decision::Rework);
    }

    #[test]
    fn warn_with_low_confidence_escalates() {
        let t = DecisionThresholds::default();
        assert_eq!(
            decide(&input(Verdict::Warn, 10, 0.3), &t),
            Decision::Escalate
        );
    }

    #[test]
    fn identical_inputs_produce_identical_decisions_regardless_of_call_site() {
        // Simulates three different call sites (draft apply, social publish,
        // email send) constructing the same logical input independently.
        let t = DecisionThresholds::default();
        let from_draft = input(Verdict::Pass, 20, 0.85);
        let from_social = DecisionInput {
            verdict: Verdict::Pass,
            risk_score: 20,
            confidence: 0.85,
        };
        let from_email = input(Verdict::Pass, 20, 0.85);
        let d1 = decide(&from_draft, &t);
        let d2 = decide(&from_social, &t);
        let d3 = decide(&from_email, &t);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
        assert_eq!(d1, Decision::Commit);
    }

    #[test]
    fn only_commit_is_auto_approvable() {
        assert!(Decision::Commit.is_auto_approvable());
        assert!(!Decision::Reject.is_auto_approvable());
        assert!(!Decision::Rework.is_auto_approvable());
        assert!(!Decision::Escalate.is_auto_approvable());
    }
}
