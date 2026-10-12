// send.rs: send-time policy and the registry-backed executor (v0.17.12.3).
//
// `ProjectSendPolicy` is what the queue worker consults before EVERY attempt.
// It fails closed: a missing connector, a policy file that cannot be parsed,
// or a rule that now blocks the action all stop the send and put the action in
// `needs attention` with the reason. Nothing is guessed.
//
// `RegistryExecutor` runs an action through the same `ExternalAction` plugins
// that `ta_external_action` uses, and classifies the outcome conservatively:
// only an error the connector explicitly marks `retryable:` or `rejected:` is
// treated as a definite non-send. Everything else is an UNKNOWN outcome,
// because a timeout or crash may have happened after the remote accepted it.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::action::{ActionError, ActionRegistry};
use crate::constitution_rules::PolicyConstitution;
use crate::policy::{ActionPolicy, ActionPolicyConfig};
use crate::queue::{ActionExecutor, ExecOutcome, PolicyVerdict, QueueEntry, SendPolicy};
use crate::reversibility::Idempotency;

/// Tool names that carry a connector verb look like `ta_external_action:<verb>`.
pub const EXTERNAL_ACTION_PREFIX: &str = "ta_external_action:";

fn verb_of(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix(EXTERNAL_ACTION_PREFIX)
        .filter(|v| !v.is_empty())
}

/// Re-reads the project's policy files on every call and fails closed.
pub struct ProjectSendPolicy {
    workspace_root: PathBuf,
}

impl ProjectSendPolicy {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    fn policy_config(&self, verb: &str) -> Result<ActionPolicyConfig, String> {
        #[derive(Deserialize)]
        struct Root {
            #[serde(default)]
            actions: HashMap<String, ActionPolicyConfig>,
        }
        let path = self.workspace_root.join(".ta").join("workflow.toml");
        if !path.exists() {
            return Ok(ActionPolicyConfig::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let root: Root = toml::from_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", path.display()))?;
        Ok(root.actions.get(verb).cloned().unwrap_or_default())
    }
}

impl SendPolicy for ProjectSendPolicy {
    fn check(&self, entry: &QueueEntry) -> PolicyVerdict {
        let block = |why: String| PolicyVerdict::Block {
            reason: format!(
                "{why}. Fix the policy or configuration, then run `ta show actions --resolve {} \
                 --resend`, or drop the action with `--drop`.",
                entry.key
            ),
        };
        let Some(verb) = verb_of(&entry.action.tool_name) else {
            return block(format!(
                "'{}' is not a connector action TA can send",
                entry.action.tool_name
            ));
        };
        let cfg = match self.policy_config(verb) {
            Ok(c) => c,
            Err(e) => {
                return block(format!(
                    "configuration is unreadable, so TA fails closed: {e}"
                ))
            }
        };
        if cfg.policy == ActionPolicy::Block {
            return block(format!(
                "workflow.toml now sets [actions.{verb}] policy = \"block\""
            ));
        }
        let constitution = match PolicyConstitution::try_load(&self.workspace_root) {
            Ok(c) => c,
            Err(e) => {
                return block(format!(
                    "constitution is unreadable, so TA fails closed: {e}"
                ))
            }
        };
        match constitution.check_action_policy(verb, &cfg.policy) {
            Err(v) if !v.is_warn => block(format!(
                "the constitution blocks this action: {}",
                v.message
            )),
            _ => PolicyVerdict::Allow,
        }
    }
}

/// Executes through the registry's `ExternalAction` plugins.
pub struct RegistryExecutor<'a> {
    pub registry: &'a ActionRegistry,
}

impl ActionExecutor for RegistryExecutor<'_> {
    fn execute(&self, entry: &QueueEntry, idempotency_key: &str) -> ExecOutcome {
        let Some(verb) = verb_of(&entry.action.tool_name) else {
            return ExecOutcome::Rejected {
                message: format!(
                    "'{}' is not a connector action TA can send",
                    entry.action.tool_name
                ),
            };
        };
        let Some(action) = self.registry.get(verb) else {
            return ExecOutcome::Rejected {
                message: format!(
                    "no connector is installed for '{verb}'. Install or enable the connector, \
                     then resend"
                ),
            };
        };
        let mut payload = entry.action.parameters.clone();
        if entry.action.idempotency == Idempotency::Key {
            if let Value::Object(map) = &mut payload {
                map.insert(
                    "idempotency_key".into(),
                    Value::String(idempotency_key.to_string()),
                );
            }
        }
        if let Err(e) = action.validate(&payload) {
            return ExecOutcome::Rejected {
                message: format!("the payload is not valid for '{verb}': {e}"),
            };
        }
        match action.execute(&payload) {
            Ok(result) => ExecOutcome::Done { result },
            Err(ActionError::StubOnly(t)) => ExecOutcome::Rejected {
                message: format!(
                    "'{t}' has only a built-in stub and no real connector. Install a connector \
                     that declares verb:{t}, then resend"
                ),
            },
            Err(ActionError::Validation(m)) | Err(ActionError::MissingField(m)) => {
                ExecOutcome::Rejected { message: m }
            }
            Err(ActionError::Execution(m)) => classify(&m),
        }
    }
}

/// `retryable:` and `rejected:` are the only markers that assert "not sent".
fn classify(message: &str) -> ExecOutcome {
    let lower = message.to_ascii_lowercase();
    if lower.contains("retryable:") {
        ExecOutcome::Retryable {
            message: message.to_string(),
        }
    } else if lower.contains("rejected:") {
        ExecOutcome::Rejected {
            message: message.to_string(),
        }
    } else {
        ExecOutcome::Unknown {
            message: format!(
                "{message} (the connector did not say whether the action was sent, so TA does \
                 not retry)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{ActionQueue, ManualClock, NewAction};
    use crate::reversibility::Reversibility;

    fn entry_in(dir: &std::path::Path, tool: &str) -> QueueEntry {
        let q = ActionQueue::open(dir.join("q")).unwrap();
        let clock = ManualClock::new(chrono::Utc::now());
        q.enqueue_batch(
            &[NewAction {
                draft_id: "d".into(),
                action_id: "a".into(),
                tool_name: tool.into(),
                parameters: serde_json::json!({"to": "x@example.com", "subject": "s", "body": "b"}),
                description: String::new(),
                class: Reversibility::Irrecoverable,
                undo: None,
                idempotency: Idempotency::None,
                undo_of: None,
            }],
            &clock,
        )
        .unwrap();
        q.snapshot().unwrap().entries.remove(0)
    }

    fn project(dir: &std::path::Path) -> PathBuf {
        let root = dir.join("project");
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        root
    }

    fn blocked(v: PolicyVerdict) -> String {
        match v {
            PolicyVerdict::Block { reason } => reason,
            PolicyVerdict::Allow => panic!("expected the policy to block"),
        }
    }

    #[test]
    fn no_config_allows_a_reviewed_action() {
        let tmp = tempfile::tempdir().unwrap();
        let e = entry_in(tmp.path(), "ta_external_action:email");
        let p = ProjectSendPolicy::new(project(tmp.path()));
        assert_eq!(p.check(&e), PolicyVerdict::Allow);
    }

    #[test]
    fn policy_block_and_unparsable_config_both_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project(tmp.path());
        let e = entry_in(tmp.path(), "ta_external_action:email");
        let p = ProjectSendPolicy::new(&root);
        let wf = root.join(".ta").join("workflow.toml");

        std::fs::write(&wf, "[actions.email]\npolicy = \"block\"\n").unwrap();
        let reason = blocked(p.check(&e));
        assert!(
            reason.contains("policy = \"block\"") && reason.contains("--resend"),
            "{reason}"
        );

        std::fs::write(&wf, "[actions.email\npolicy = ").unwrap();
        assert!(blocked(p.check(&e)).contains("fails closed"));

        // Fixing the file is picked up on the next attempt (re-read every time).
        std::fs::write(&wf, "[actions.email]\npolicy = \"review\"\n").unwrap();
        assert_eq!(p.check(&e), PolicyVerdict::Allow);
    }

    #[test]
    fn broken_constitution_fails_closed_and_email_auto_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project(tmp.path());
        let e = entry_in(tmp.path(), "ta_external_action:email");
        let p = ProjectSendPolicy::new(&root);
        let cons = root.join(".ta").join("constitution.toml");
        std::fs::write(&cons, "[[rules.block").unwrap();
        assert!(blocked(p.check(&e)).contains("constitution is unreadable"));
        std::fs::remove_file(&cons).unwrap();
        // Built-in rule: email with policy auto is never allowed.
        std::fs::write(
            root.join(".ta").join("workflow.toml"),
            "[actions.email]\npolicy = \"auto\"\n",
        )
        .unwrap();
        assert!(blocked(p.check(&e)).contains("constitution blocks"));
    }

    #[test]
    fn non_connector_tool_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let e = entry_in(tmp.path(), "gmail_send");
        let p = ProjectSendPolicy::new(project(tmp.path()));
        assert!(matches!(p.check(&e), PolicyVerdict::Block { .. }));
    }

    #[test]
    fn executor_classifies_conservatively() {
        assert!(matches!(
            classify("plugin failed: RETRYABLE: 503"),
            ExecOutcome::Retryable { .. }
        ));
        assert!(matches!(
            classify("rejected: bad address"),
            ExecOutcome::Rejected { .. }
        ));
        let ExecOutcome::Unknown { message } = classify("timed out after 30s") else {
            panic!("a bare timeout must be unknown");
        };
        assert!(message.contains("does not retry"));
    }

    #[test]
    fn stub_and_missing_connector_are_rejections_not_sends() {
        let reg = ActionRegistry::new();
        let ex = RegistryExecutor { registry: &reg };
        let tmp = tempfile::tempdir().unwrap();
        let e = entry_in(tmp.path(), "ta_external_action:email");
        let ExecOutcome::Rejected { message } = ex.execute(&e, "d:a") else {
            panic!("a stub must not report a send");
        };
        assert!(message.contains("no real connector"), "{message}");
        let tmp2 = tempfile::tempdir().unwrap();
        let e = entry_in(tmp2.path(), "ta_external_action:nonesuch");
        let ExecOutcome::Rejected { message } = ex.execute(&e, "d:a") else {
            panic!("a missing connector must not report a send");
        };
        assert!(message.contains("no connector is installed"), "{message}");
    }
}
