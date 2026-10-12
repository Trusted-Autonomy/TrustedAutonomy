// rules/audit.rs: every decision goes to the audit log (v0.17.12.4).
//
// "Why did this send?" and "why did this not send?" must always be answerable,
// so each `decide()` that causes or blocks an effect is appended here with the
// rule that decided it. The write is best effort in the sense that a failure
// never turns a deny into an allow, but it is never silent: it is logged and
// returned so the caller can tell the person.

use std::path::Path;

use ta_audit::{AuditAction, AuditEvent, AuditLog};

use super::model::When;
use super::table::Decision;

/// One decision to record.
pub struct DecisionRecord<'a> {
    /// Who asked: `ta-draft-apply`, `ta-daemon-queue`, `ta-mcp-gateway`.
    pub caller: &'a str,
    pub action: &'a str,
    pub when: When,
    pub decision: &'a Decision,
    /// The draft or goal this belongs to, when known.
    pub subject: Option<&'a str>,
    /// The path, for apply decisions.
    pub path: Option<&'a str>,
}

/// Append the decision. `Err` carries a sentence for the person.
pub fn record(audit_log: &Path, rec: &DecisionRecord<'_>) -> Result<(), String> {
    tracing::info!(
        caller = rec.caller,
        action = rec.action,
        attendance = rec.when.as_str(),
        effect = rec.decision.effect.as_str(),
        rule = %rec.decision.rule,
        subject = rec.subject.unwrap_or(""),
        "rules decision"
    );
    let mut log = AuditLog::open(audit_log).map_err(|e| {
        format!(
            "could not open the audit log at {} to record a rules decision ({e}). Check the \
             file's permissions.",
            audit_log.display()
        )
    })?;
    let mut event = AuditEvent::new(rec.caller, AuditAction::PolicyDecision)
        .with_target(format!("rules://{}", rec.action))
        .with_metadata(serde_json::json!({
            "action": rec.action,
            "attendance": rec.when.as_str(),
            "effect": rec.decision.effect.as_str(),
            "rule": rec.decision.rule,
            "layer": rec.decision.layer.label(),
            "reason": rec.decision.reason,
            "environment": rec.decision.environment.map(|e| e.as_str()),
            "subject": rec.subject,
            "path": rec.path,
        }));
    log.append(&mut event).map_err(|e| {
        format!(
            "could not append to the audit log at {}: {e}",
            audit_log.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::model::{Effect, Layer};

    #[test]
    fn decisions_are_appended_with_their_rule() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("audit.jsonl");
        let decision = Decision {
            effect: Effect::Deny,
            layer: Layer::Posture,
            rule: "posture strict [[deny]] #1".into(),
            reason: "Email always needs a person.".into(),
            fix: None,
            environment: None,
        };
        record(
            &log,
            &DecisionRecord {
                caller: "test",
                action: "email",
                when: When::Unattended,
                decision: &decision,
                subject: Some("draft-1"),
                path: None,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("posture strict [[deny]] #1") && text.contains("rules://email"),
            "{text}"
        );
    }

    #[test]
    fn unwritable_log_is_reported_not_swallowed() {
        let d = tempfile::tempdir().unwrap();
        let decision = Decision {
            effect: Effect::Allow,
            layer: Layer::Owner,
            rule: "r".into(),
            reason: String::new(),
            fix: None,
            environment: None,
        };
        let err = record(
            &d.path().join("no/such/dir/audit.jsonl"),
            &DecisionRecord {
                caller: "t",
                action: "a",
                when: When::Attended,
                decision: &decision,
                subject: None,
                path: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("audit log"), "{err}");
    }
}
