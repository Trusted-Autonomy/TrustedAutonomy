// send.rs: send-time policy and the registry-backed executor (v0.17.12.3).
//
// `ProjectSendPolicy` is what the queue worker consults before EVERY attempt.
// It fails closed: a missing connector, a policy file that cannot be parsed,
// a rules file that does not verify, or a rule that now blocks the action all
// stop the send and put the action in `needs attention` with the reason.
// Nothing is guessed.
//
// Since v0.17.12.4 the answer to "may this be sent?" comes from the compiled
// rules table (`rules::CompiledRules::decide`), called with the attendance
// that was proven when the draft was applied and stored with the queued
// action. The old per-file checks below it still run and can only tighten.
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
use crate::rules::audit::{self, DecisionRecord};
use crate::rules::{Request, RulesLoader, TargetRef};

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
    loader: RulesLoader,
}

impl ProjectSendPolicy {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            loader: RulesLoader::for_project(&workspace_root),
            workspace_root,
        }
    }

    /// Use an explicit rules loader (tests, and callers that already know
    /// where the state directory and org floor are).
    pub fn with_loader(mut self, loader: RulesLoader) -> Self {
        self.loader = loader;
        self
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
        // The rules table decides first. It is re-read and its signature
        // re-verified on every attempt, so an edit or a revoked allow takes
        // effect on the next retry.
        let loaded = self.loader.load(None);
        let when = entry.action.attendance;
        let decision = loaded.decide(
            &Request::new(verb, when).with_target(TargetRef::from_params(&entry.action.parameters)),
        );
        let audit_path = self.workspace_root.join(".ta").join("audit.jsonl");
        if let Err(e) = audit::record(
            &audit_path,
            &DecisionRecord {
                caller: "ta-send-policy",
                action: verb,
                when,
                decision: &decision,
                subject: Some(&entry.key),
                path: None,
            },
        ) {
            tracing::warn!(key = %entry.key, error = %e, "rules decision could not be audited");
        }
        if decision.is_deny() {
            return block(decision.refusal(verb, when));
        }
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
                attendance: crate::rules::When::Attended,
                ordered: false,
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

    /// A policy that reads no real state directory or org floor.
    fn policy(root: &std::path::Path) -> ProjectSendPolicy {
        ProjectSendPolicy::new(root).with_loader(RulesLoader::with_dirs(root, None, None))
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
        let p = policy(&project(tmp.path()));
        assert_eq!(p.check(&e), PolicyVerdict::Allow);
    }

    #[test]
    fn policy_block_and_unparsable_config_both_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project(tmp.path());
        let e = entry_in(tmp.path(), "ta_external_action:email");
        let p = policy(&root);
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
        let p = policy(&root);
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
    fn unattended_send_is_decided_by_the_rules_table_and_audited() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project(tmp.path());
        let mut e = entry_in(tmp.path(), "ta_external_action:email");
        e.action.attendance = crate::rules::When::Unattended;
        let p = policy(&root);
        let why = blocked(p.check(&e));
        assert!(why.contains("Blocked: unattended email"), "{why}");
        assert!(
            why.contains("ta rules sign"),
            "the refusal names the fix: {why}"
        );
        let audit = std::fs::read_to_string(root.join(".ta").join("audit.jsonl")).unwrap();
        assert!(audit.contains("ta-send-policy"), "{audit}");

        // A rules file an agent wrote into the tree is unsigned, so it allows nothing.
        std::fs::write(
            root.join(".ta").join("rules.toml"),
            "version = 1\n[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n",
        )
        .unwrap();
        let why = blocked(p.check(&e));
        assert!(
            why.contains("could not be verified") || why.contains("Blocked"),
            "{why}"
        );

        // The same action queued by a person present is not stopped by the table.
        e.action.attendance = crate::rules::When::Attended;
        let fresh = policy(&project(tmp.path().join("other").as_path()));
        assert_eq!(fresh.check(&e), PolicyVerdict::Allow);
    }

    #[test]
    fn non_connector_tool_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let e = entry_in(tmp.path(), "gmail_send");
        let p = policy(&project(tmp.path()));
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
