// actor_context.rs: Label approvals that reached the daemon from an agent (CR-03 item 6).
//
// The daemon cannot see a client's environment, so clients that detect an agent
// context send the advisory header `X-TA-Actor-Context: agent_delegated;
// reasons=...`. The daemon records `actor_context` (`agent_delegated` or
// `direct`) next to the credential holder in the audit log and the draft
// record for approve, deny and apply, and in the attention resolution record
// for `/api/attention/{id}/act`.
//
// ADVISORY ONLY. An agent can omit the header, so this is for the audit trail,
// not a security control; the CR-02 gate is unchanged and is what refuses
// requests without proof of a human. A malformed or oversize header is ignored
// with a debug log, never an error: it must not be a way to make an approval fail.

use std::path::Path;

use axum::http::HeaderMap;
use chrono::Utc;
use ta_credentials::human;

use crate::human_gate::{HumanProof, ProofKind};

/// `actor_context` for an action with no agent header.
pub const ACTOR_DIRECT: &str = "direct";

/// See [`human::DAEMON_RECORDED_ENV`].
pub use ta_credentials::human::DAEMON_RECORDED_ENV;

/// What the client claimed about who is acting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorClaim {
    /// `agent_delegated` or `direct`.
    pub actor_context: &'static str,
    /// Detection reasons (empty for `direct`).
    pub signals: Vec<String>,
    /// The delegating owner: the credential holder (or the asserted human).
    pub owner: String,
}

impl ActorClaim {
    pub fn is_delegated(&self) -> bool {
        self.actor_context == human::ACTOR_AGENT_DELEGATED
    }
}

/// The reasons carried by a valid `X-TA-Actor-Context` header, if any.
/// Malformed or oversize values are ignored with a debug log.
pub fn signals_from_headers(headers: &HeaderMap) -> Option<Vec<String>> {
    let raw = headers.get(human::ACTOR_CONTEXT_HEADER)?;
    let Ok(text) = raw.to_str() else {
        tracing::debug!(
            header = human::ACTOR_CONTEXT_HEADER,
            "ignoring actor-context header: value is not visible ASCII; treating the action as direct"
        );
        return None;
    };
    match human::parse_actor_context_header(text) {
        Ok(signals) => Some(signals),
        Err(why) => {
            tracing::debug!(
                header = human::ACTOR_CONTEXT_HEADER,
                reason = %why,
                "ignoring malformed actor-context header; treating the action as direct"
            );
            None
        }
    }
}

/// Build the claim for an accepted request. `proof` names who vouched for it.
pub fn claim_for(headers: &HeaderMap, proof: Option<&HumanProof>) -> ActorClaim {
    let owner = proof
        .and_then(|p| p.user_id.clone())
        .or_else(|| {
            proof
                .filter(|p| p.via == ProofKind::Credential)
                .map(|_| human::credential_holder())
        })
        .unwrap_or_else(|| "web-ui".to_string());
    match signals_from_headers(headers) {
        Some(signals) => ActorClaim {
            actor_context: human::ACTOR_AGENT_DELEGATED,
            signals,
            owner,
        },
        None => ActorClaim {
            actor_context: ACTOR_DIRECT,
            signals: Vec::new(),
            owner,
        },
    }
}

/// The name stored as `approved_by` / `denied_by`: the delegating owner when the
/// action was delegated, otherwise the historical default for the route.
pub fn actor_name(claim: &ActorClaim, direct_default: Option<String>, fallback: &str) -> String {
    if claim.is_delegated() {
        claim.owner.clone()
    } else {
        direct_default.unwrap_or_else(|| fallback.to_string())
    }
}

/// The record stored on the draft.
pub fn draft_record(claim: &ActorClaim, action: &str) -> ta_changeset::ActionContextRecord {
    ta_changeset::ActionContextRecord {
        action: action.to_string(),
        actor_context: claim.actor_context.to_string(),
        delegating_owner: claim.owner.clone(),
        delegated_signals: claim.signals.clone(),
        at: Utc::now(),
    }
}

/// Audit metadata, next to the credential holder.
pub fn audit_metadata(claim: &ActorClaim, action: &str, route: &str) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "actor_context": claim.actor_context,
        "delegating_owner": claim.owner,
        "delegated_signals": claim.signals,
        "source": "daemon",
        "route": route,
    })
}

/// Append the audit entry for an approve, deny or apply accepted by the daemon.
/// Failures are logged and never undo the action.
pub fn write_audit(
    project_root: &Path,
    claim: &ActorClaim,
    action: &str,
    draft_id: &str,
    route: &str,
) {
    let audit_path = project_root.join(".ta").join("audit.jsonl");
    let kind = if action == "apply" {
        ta_audit::AuditAction::Apply
    } else {
        ta_audit::AuditAction::Approval
    };
    match ta_audit::AuditLog::open(&audit_path) {
        Ok(mut log) => {
            let mut event = ta_audit::AuditEvent::new(claim.owner.clone(), kind)
                .with_target(format!("draft://{draft_id}"))
                .with_metadata(audit_metadata(claim, action, route));
            if let Err(e) = log.append(&mut event) {
                tracing::warn!(
                    path = %audit_path.display(), route, draft = draft_id, error = %e,
                    "could not append the actor_context audit event; the action itself succeeded"
                );
            }
        }
        Err(e) => tracing::warn!(
            path = %audit_path.display(), route, draft = draft_id, error = %e,
            "could not open the audit log to record actor_context; the action itself succeeded"
        ),
    }
}

/// Remember that an agent context was seen, for the 'agent delegation active'
/// line and `GET /api/status`. Only delegated claims count as a sighting.
pub fn note_sighting(state_dir: &Path, claim: &ActorClaim) {
    if !claim.is_delegated() {
        return;
    }
    let ctx = human::AgentContext {
        signals: claim.signals.clone(),
    };
    if let Err(e) = human::record_agent_context_seen(state_dir, &ctx) {
        tracing::warn!(
            path = %state_dir.display(), error = %e,
            "could not record the agent-context sighting; `agent_delegation` in /api/status may lag"
        );
    }
}

/// Store the record on the draft file. Returns `Ok(false)` when it is gone.
pub fn stamp_draft(
    pr_packages_dir: &Path,
    id: uuid::Uuid,
    record: ta_changeset::ActionContextRecord,
) -> std::io::Result<bool> {
    let path = pr_packages_dir.join(format!("{id}.json"));
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path)?;
    let mut draft: ta_changeset::DraftPackage = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    draft.action_contexts.push(record);
    let out =
        serde_json::to_string_pretty(&draft).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(&path, out)?;
    Ok(true)
}

/// Record everything for one accepted draft action: draft record, audit entry,
/// sighting. `state_dir` is the per-user state directory.
pub fn record_draft_action(
    pr_packages_dir: &Path,
    project_root: &Path,
    state_dir: &Path,
    claim: &ActorClaim,
    action: &str,
    id: uuid::Uuid,
    route: &str,
) {
    if let Err(e) = stamp_draft(pr_packages_dir, id, draft_record(claim, action)) {
        tracing::warn!(
            draft = %id, route, error = %e,
            "{action} succeeded but the actor_context could not be saved on the draft; check write access to {}",
            pr_packages_dir.display()
        );
    }
    write_audit(project_root, claim, action, &id.to_string(), route);
    note_sighting(state_dir, claim);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            human::ACTOR_CONTEXT_HEADER,
            HeaderValue::from_str(v).unwrap(),
        );
        h
    }

    fn proof() -> HumanProof {
        HumanProof::anonymous(ProofKind::Credential)
    }

    #[test]
    fn absent_header_is_direct() {
        let c = claim_for(&HeaderMap::new(), Some(&proof()));
        assert_eq!(c.actor_context, ACTOR_DIRECT);
        assert!(c.signals.is_empty());
        assert!(!c.is_delegated());
    }

    #[test]
    fn valid_header_is_delegated_to_the_credential_holder() {
        let c = claim_for(
            &headers("agent_delegated; reasons=CLAUDECODE%20is%20set"),
            Some(&proof()),
        );
        assert!(c.is_delegated());
        assert_eq!(c.signals, vec!["CLAUDECODE is set"]);
        assert_eq!(c.owner, human::credential_holder());
        assert_eq!(actor_name(&c, None, "web-ui"), human::credential_holder());
    }

    #[test]
    fn malformed_or_oversize_header_is_ignored_not_an_error() {
        for bad in [
            "garbage",
            "agent_delegated; reasons=%ZZ",
            &format!("agent_delegated; reasons={}", "a".repeat(2000)),
        ] {
            let c = claim_for(&headers(bad), Some(&proof()));
            assert_eq!(c.actor_context, ACTOR_DIRECT, "{bad}");
        }
        // Not visible ASCII: HeaderValue allows opaque bytes, to_str refuses them.
        let mut h = HeaderMap::new();
        h.insert(
            human::ACTOR_CONTEXT_HEADER,
            HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap(),
        );
        assert_eq!(claim_for(&h, None).actor_context, ACTOR_DIRECT);
    }

    #[test]
    fn direct_action_keeps_the_historical_name() {
        let c = claim_for(&HeaderMap::new(), Some(&proof()));
        assert_eq!(actor_name(&c, None, "web-ui"), "web-ui");
        assert_eq!(actor_name(&c, Some("ana".into()), "web-ui"), "ana");
    }

    #[test]
    fn delegated_record_lands_on_the_draft_audit_log_and_sighting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pr = root.join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr).unwrap();
        let state = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let draft = crate::web::test_draft_value(id);
        std::fs::write(pr.join(format!("{id}.json")), draft.to_string()).unwrap();

        let claim = claim_for(
            &headers("agent_delegated; reasons=CLAUDECODE%20is%20set"),
            Some(&proof()),
        );
        record_draft_action(&pr, root, state.path(), &claim, "approve", id, "POST /x");

        let saved: ta_changeset::DraftPackage =
            serde_json::from_str(&std::fs::read_to_string(pr.join(format!("{id}.json"))).unwrap())
                .unwrap();
        assert_eq!(saved.action_contexts.len(), 1);
        assert_eq!(saved.action_contexts[0].actor_context, "agent_delegated");
        assert_eq!(saved.action_contexts[0].action, "approve");
        assert_eq!(
            saved.action_contexts[0].delegating_owner,
            human::credential_holder()
        );
        let audit = std::fs::read_to_string(root.join(".ta/audit.jsonl")).unwrap();
        assert!(audit.contains("agent_delegated"), "{audit}");
        assert!(audit.contains("CLAUDECODE is set"), "{audit}");
        assert!(audit.contains(&human::credential_holder()), "{audit}");
        assert!(human::last_agent_context_seen(state.path()).is_some());
    }

    #[test]
    fn direct_record_has_no_sighting() {
        let state = tempfile::tempdir().unwrap();
        let claim = claim_for(&HeaderMap::new(), Some(&proof()));
        note_sighting(state.path(), &claim);
        assert!(human::last_agent_context_seen(state.path()).is_none());
    }
}
