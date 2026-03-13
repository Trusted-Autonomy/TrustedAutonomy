// scorer.rs — Feedback scoring with optional agent integration (v0.10.18).
//
// The scorer aggregates multiple review verdicts into a single assessment
// with routing recommendations. Uses either the built-in aggregate algorithm
// or delegates to an external scoring agent via command invocation.
//
// When a `ScorerConfig` is provided, the scorer spawns the configured agent
// command with verdict data on stdin and reads a ScoringResult from stdout.
// This allows LLM-powered scoring that reasons about verdict context rather
// than just averaging numeric scores.

use crate::definition::{ScorerConfig, VerdictConfig};
use crate::verdict::{aggregate_score, required_roles_pass, Finding, Severity, Verdict};

use serde::{Deserialize, Serialize};

/// Result of scoring a set of verdicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringResult {
    /// Aggregate score (0.0-1.0).
    pub score: f64,
    /// Overall severity classification.
    pub severity: Severity,
    /// Whether the stage passes based on threshold and required roles.
    pub passes: bool,
    /// Routing recommendation (stage to route back to, if any).
    pub route_to: Option<String>,
    /// Synthesized feedback for the next iteration.
    pub feedback: String,
    /// All findings collected from verdicts.
    pub findings: Vec<Finding>,
    /// Whether an agent scorer was used (vs built-in algorithm).
    #[serde(default)]
    pub agent_scored: bool,
}

/// Score a set of verdicts against a verdict configuration.
///
/// If `config.scorer` is set and an agent is configured, attempts to delegate
/// scoring to the external agent. Falls back to built-in scoring on failure.
///
/// Built-in algorithm:
/// - Aggregate score = average of verdict scores (Pass=1, Conditional=0.5, Fail=0)
/// - Passes if score >= threshold AND all required roles pass
/// - Severity = worst finding severity, or Minor if no findings
pub fn score_verdicts(
    verdicts: &[Verdict],
    config: &VerdictConfig,
    failure_route: Option<&str>,
) -> ScoringResult {
    // v0.10.18: Try agent scoring if configured.
    if let Some(ref scorer_config) = config.scorer {
        match try_agent_score(verdicts, config, scorer_config, failure_route) {
            Ok(result) => return result,
            Err(e) => {
                tracing::warn!(
                    agent = %scorer_config.agent,
                    error = %e,
                    "Agent scoring failed, falling back to built-in algorithm"
                );
            }
        }
    }

    builtin_score(verdicts, config, failure_route)
}

/// Built-in scoring algorithm (always available).
fn builtin_score(
    verdicts: &[Verdict],
    config: &VerdictConfig,
    failure_route: Option<&str>,
) -> ScoringResult {
    let score = aggregate_score(verdicts);
    let required_ok = required_roles_pass(verdicts, &config.required_pass);
    let passes = score >= config.pass_threshold && required_ok;

    // Collect all findings.
    let findings: Vec<Finding> = verdicts
        .iter()
        .flat_map(|v| v.findings.iter().cloned())
        .collect();

    // Determine worst severity.
    let severity = findings
        .iter()
        .map(|f| f.severity.clone())
        .max()
        .unwrap_or(Severity::Minor);

    // Build feedback summary.
    let feedback = if passes {
        format!(
            "All checks passed (score: {:.2}, threshold: {:.2}).",
            score, config.pass_threshold
        )
    } else {
        let mut parts = Vec::new();
        if score < config.pass_threshold {
            parts.push(format!(
                "Aggregate score {:.2} below threshold {:.2}",
                score, config.pass_threshold
            ));
        }
        if !required_ok {
            let failed: Vec<&str> = config
                .required_pass
                .iter()
                .filter(|r| !verdicts.iter().any(|v| v.role == **r && v.is_pass()))
                .map(|s| s.as_str())
                .collect();
            parts.push(format!(
                "Required roles did not pass: {}",
                failed.join(", ")
            ));
        }
        let finding_summary: Vec<String> = findings
            .iter()
            .map(|f| format!("[{}] {}: {}", f.severity, f.title, f.description))
            .collect();
        if !finding_summary.is_empty() {
            parts.push(format!("Findings:\n{}", finding_summary.join("\n")));
        }
        parts.join("\n")
    };

    let route_to = if !passes {
        failure_route.map(|s| s.to_string())
    } else {
        None
    };

    ScoringResult {
        score,
        severity,
        passes,
        route_to,
        feedback,
        findings,
        agent_scored: false,
    }
}

/// Input payload sent to the scoring agent on stdin.
#[derive(Debug, Serialize)]
struct AgentScoringInput {
    verdicts: Vec<Verdict>,
    pass_threshold: f64,
    required_pass: Vec<String>,
    failure_route: Option<String>,
    prompt: String,
}

/// Attempt to score verdicts using an external agent process.
///
/// Spawns the agent command, sends verdict data as JSON on stdin,
/// and reads a ScoringResult from stdout. The agent can use LLM
/// reasoning to produce more nuanced assessments than the numeric average.
fn try_agent_score(
    verdicts: &[Verdict],
    config: &VerdictConfig,
    scorer_config: &ScorerConfig,
    failure_route: Option<&str>,
) -> Result<ScoringResult, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let input = AgentScoringInput {
        verdicts: verdicts.to_vec(),
        pass_threshold: config.pass_threshold,
        required_pass: config.required_pass.clone(),
        failure_route: failure_route.map(|s| s.to_string()),
        prompt: scorer_config.prompt.clone(),
    };

    let input_json = serde_json::to_string(&input)
        .map_err(|e| format!("Failed to serialize scoring input: {}", e))?;

    tracing::info!(
        agent = %scorer_config.agent,
        verdict_count = verdicts.len(),
        "Invoking scoring agent"
    );

    let mut child = Command::new(&scorer_config.agent)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn scoring agent '{}': {}. \
                 Ensure the scorer binary is installed and in PATH.",
                scorer_config.agent, e
            )
        })?;

    // Write input to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input_json.as_bytes())
            .map_err(|e| format!("Failed to write to scorer stdin: {}", e))?;
        // Drop stdin to signal EOF.
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for scorer agent: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Scoring agent '{}' exited with status {}. stderr: {}",
            scorer_config.agent,
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result: ScoringResult = serde_json::from_str(stdout.trim()).map_err(|e| {
        format!(
            "Failed to parse scorer output as ScoringResult: {}. Raw output: '{}'",
            e,
            stdout.trim()
        )
    })?;

    result.agent_scored = true;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::VerdictDecision;

    fn make_config() -> VerdictConfig {
        VerdictConfig {
            scorer: None,
            pass_threshold: 0.7,
            required_pass: vec!["security".to_string()],
        }
    }

    #[test]
    fn scoring_all_pass() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(result.passes);
        assert_eq!(result.score, 1.0);
        assert!(result.route_to.is_none());
        assert!(!result.agent_scored);
    }

    #[test]
    fn scoring_below_threshold() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Fail,
                severity: Some(Severity::Major),
                findings: vec![Finding {
                    title: "Bad formatting".to_string(),
                    description: "Inconsistent style".to_string(),
                    severity: Severity::Major,
                    category: Some("style".to_string()),
                }],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(!result.passes);
        assert_eq!(result.score, 0.5);
        assert_eq!(result.route_to, Some("build".to_string()));
    }

    #[test]
    fn scoring_required_role_fails() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Fail,
                severity: Some(Severity::Critical),
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(!result.passes); // security is required
    }

    #[test]
    fn scoring_feedback_includes_findings() {
        let verdicts = vec![Verdict {
            role: "security".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Critical),
            findings: vec![Finding {
                title: "SQL injection".to_string(),
                description: "Unescaped input".to_string(),
                severity: Severity::Critical,
                category: Some("security".to_string()),
            }],
        }];
        let result = score_verdicts(&verdicts, &make_config(), None);
        assert!(result.feedback.contains("SQL injection"));
    }

    #[test]
    fn agent_scorer_fallback_on_missing_binary() {
        let config = VerdictConfig {
            scorer: Some(ScorerConfig {
                agent: "nonexistent-scorer-binary-12345".to_string(),
                prompt: "Score these verdicts".to_string(),
            }),
            pass_threshold: 0.7,
            required_pass: vec![],
        };
        let verdicts = vec![Verdict {
            role: "test".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        // Should fall back to built-in scoring when agent fails.
        let result = score_verdicts(&verdicts, &config, None);
        assert!(result.passes);
        assert!(!result.agent_scored);
    }

    #[test]
    fn builtin_score_no_verdicts() {
        let config = VerdictConfig {
            scorer: None,
            pass_threshold: 0.0,
            required_pass: vec![],
        };
        let result = score_verdicts(&[], &config, None);
        // NaN guard: aggregate_score of empty = 0.0, threshold 0.0 → passes.
        assert!(result.passes);
    }

    #[test]
    fn scoring_result_serialization() {
        let result = ScoringResult {
            score: 0.85,
            severity: Severity::Minor,
            passes: true,
            route_to: None,
            feedback: "All good".to_string(),
            findings: vec![],
            agent_scored: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"agent_scored\":true"));
        let restored: ScoringResult = serde_json::from_str(&json).unwrap();
        assert!(restored.agent_scored);
    }
}
