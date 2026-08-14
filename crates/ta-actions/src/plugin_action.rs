// plugin_action.rs — user-authored domain-action adapter plugins (v0.17.5.3).
//
// Today, adding a new domain action (e.g. "execute a stock trade via a
// brokerage API") required a TA core code change: every built-in action type
// (`email`/`social_post`/`api_call`/`db_query`) is a hardcoded stub in
// `action.rs`, and `ActionRegistry::register` had no real-world caller.
//
// This module makes domain actions genuinely user-authorable by reusing the
// exact same external-subprocess-plugin transport already proven twice in
// this codebase — VCS plugins (`ta-submit::ExternalVcsAdapter`) and release
// plugins (`ta-release::PluginReleaseAdapter`) — rather than inventing a
// third protocol. A plugin is discovered the same way any other Plugin-
// category integration is: `.ta/plugins/adapter/<name>/plugin.toml`
// (project-local) or `~/.config/ta/plugins/adapter/<name>/plugin.toml`
// (user-global), found via `ta_plugin::discovery::discover_plugins("adapter", ..)`.
//
// A plugin declares the verb(s) it handles as `capabilities` entries
// prefixed `verb:` (e.g. `capabilities = ["verb:trade.execute"]`) — the same
// prefix-convention idiom `PluginReleaseAdapter` already uses for
// `"channel:<name>"` custom channel declarations, so no new manifest field
// is needed.
//
// Wire protocol (fresh process per call, same `PluginRequest`/`PluginResponse`
// envelope every other Plugin-category integration uses):
//
//   | Method       | Params                          | Result                              |
//   |--------------|----------------------------------|--------------------------------------|
//   | `risk_score` | `{"verb": ..., "payload": ...}` | `{"risk_score": u32, "confidence": f64}` |
//   | `execute`    | `{"verb": ..., "payload": ...}` | arbitrary JSON (the commit/publish outcome) |
//
// A plugin's `risk_score` response is what makes the score real and
// computed, not the hardcoded `0` `ta-submit::social_adapter.rs`'s `publish()`
// uses today — see `action.rs::ExternalAction::risk_score`'s default, which
// this type overrides.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use ta_plugin::discovery::discover_plugins;
use ta_plugin::envelope::{PluginRequest, PluginResponse};
use ta_plugin::manifest::PluginManifest;

use crate::action::{ActionError, ExternalAction, RiskAssessment};

const ADAPTER_PLUGIN_KIND: &str = "adapter";
const VERB_PREFIX: &str = "verb:";

/// Verb(s) an adapter plugin's manifest declares it handles — every
/// `capabilities` entry prefixed `verb:`, in declaration order.
fn declared_verbs(manifest: &PluginManifest) -> Vec<String> {
    manifest
        .capabilities
        .iter()
        .filter_map(|c| c.strip_prefix(VERB_PREFIX))
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect()
}

#[derive(Debug, Serialize)]
struct VerbParams<'a> {
    verb: &'a str,
    payload: &'a Value,
}

/// `ExternalAction` implementation for one verb of a discovered adapter
/// plugin. One instance per (plugin, verb) pair — a plugin declaring
/// multiple verbs gets one `AdapterPluginAction` registered per verb, all
/// sharing the same manifest/subprocess entrypoint.
pub struct AdapterPluginAction {
    verb: String,
    plugin_name: String,
    command: String,
    args: Vec<String>,
    work_dir: PathBuf,
    timeout: Duration,
}

impl AdapterPluginAction {
    pub fn new(verb: impl Into<String>, manifest: &PluginManifest, plugin_dir: &Path) -> Self {
        Self {
            verb: verb.into(),
            plugin_name: manifest.name.clone(),
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            work_dir: plugin_dir.to_path_buf(),
            timeout: manifest.timeout(30),
        }
    }

    fn call(&self, method: &str, payload: &Value) -> Result<Value, ActionError> {
        self.call_with_env(method, payload, &[])
    }

    /// Same as `call`, plus extra environment variables set only on this
    /// one subprocess invocation (v0.17.6.3) — used to attach a
    /// broker-resolved secret without it ever entering `payload` or the
    /// `PluginResponse` relayed back to the agent.
    fn call_with_env(
        &self,
        method: &str,
        payload: &Value,
        extra_env: &[(String, String)],
    ) -> Result<Value, ActionError> {
        let request = PluginRequest::new(
            method,
            json!(VerbParams {
                verb: &self.verb,
                payload,
            }),
        );
        let response: PluginResponse = ta_plugin::transport::call_json_with_env(
            &self.plugin_name,
            method,
            &self.command,
            &self.args,
            &self.work_dir,
            &request,
            self.timeout,
            extra_env,
        )
        .map_err(|e| {
            ActionError::Execution(format!(
                "adapter plugin '{}' verb '{}': {e}",
                self.plugin_name, self.verb
            ))
        })?;

        if !response.ok {
            return Err(ActionError::Execution(format!(
                "adapter plugin '{}' verb '{}' method '{method}' failed: {}",
                self.plugin_name,
                self.verb,
                response.error.as_deref().unwrap_or("unknown error")
            )));
        }
        Ok(response.result)
    }
}

impl ExternalAction for AdapterPluginAction {
    fn action_type(&self) -> &str {
        &self.verb
    }

    fn payload_schema(&self) -> Value {
        // Adapter plugins don't declare a JSON Schema in v1 — the plugin's
        // own `risk_score`/`execute` methods are the validation surface.
        json!({ "type": "object" })
    }

    fn validate(&self, _payload: &Value) -> Result<(), ActionError> {
        Ok(())
    }

    fn risk_score(&self, payload: &Value) -> Result<RiskAssessment, ActionError> {
        let result = self.call("risk_score", payload)?;
        serde_json::from_value(result).map_err(|e| {
            ActionError::Execution(format!(
                "adapter plugin '{}' verb '{}': risk_score response did not match \
                 {{risk_score: u32, confidence: f64}}: {e}",
                self.plugin_name, self.verb
            ))
        })
    }

    fn execute(&self, payload: &Value) -> Result<Value, ActionError> {
        self.call("execute", payload)
    }

    /// Attach a broker-resolved connector secret (v0.17.6.3) to this one
    /// subprocess call as `TA_CONNECTOR_SECRET`, never touching `payload`
    /// or the plugin's `PluginResponse` — the plugin script reads it from
    /// its own environment for the outbound call it makes, and that's the
    /// only place it exists outside the vault.
    fn execute_with_secret(
        &self,
        payload: &Value,
        secret: Option<&str>,
    ) -> Result<Value, ActionError> {
        match secret {
            Some(s) => self.call_with_env(
                "execute",
                payload,
                &[("TA_CONNECTOR_SECRET".to_string(), s.to_string())],
            ),
            None => self.call("execute", payload),
        }
    }
}

/// Discover every registered adapter plugin under `project_root` and return
/// one boxed `ExternalAction` per declared verb, ready to hand to
/// `ActionRegistry::register`.
///
/// Discovery itself never spawns a subprocess — it only reads
/// `plugin.toml` files — so building the registry stays cheap even when
/// most `ta_external_action` calls target an unrelated built-in type. A
/// plugin's subprocess is only spawned when one of its verbs is actually
/// dispatched via `risk_score`/`execute`.
pub fn discover_adapter_actions(project_root: &Path) -> Vec<Box<dyn ExternalAction>> {
    let mut actions: Vec<Box<dyn ExternalAction>> = Vec::new();
    for discovered in discover_plugins(ADAPTER_PLUGIN_KIND, project_root) {
        if let Err(e) = discovered.manifest.validate(ADAPTER_PLUGIN_KIND) {
            tracing::warn!(
                plugin = %discovered.manifest.name,
                error = %e,
                "skipping adapter plugin with invalid manifest"
            );
            continue;
        }
        let verbs = declared_verbs(&discovered.manifest);
        if verbs.is_empty() {
            tracing::warn!(
                plugin = %discovered.manifest.name,
                "adapter plugin declares no 'verb:<name>' capabilities — it handles no \
                 action types and will never be dispatched to"
            );
            continue;
        }
        for verb in verbs {
            actions.push(Box::new(AdapterPluginAction::new(
                verb,
                &discovered.manifest,
                &discovered.plugin_dir,
            )));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Writes a mock adapter plugin under `.ta/plugins/adapter/<name>/` whose
    /// `risk_score`/`execute` methods are driven by a small Python script —
    /// no live external API, matching item 5's "no live external API
    /// dependency in TA's own test suite" requirement.
    fn write_mock_plugin(project_root: &Path, name: &str, verbs: &[&str], script: &str) -> PathBuf {
        let plugin_dir = project_root
            .join(".ta")
            .join("plugins")
            .join("adapter")
            .join(name);
        fs::create_dir_all(&plugin_dir).unwrap();
        let script_path = plugin_dir.join("mock_plugin.py");
        fs::write(&script_path, script).unwrap();

        let capabilities: Vec<String> = verbs.iter().map(|v| format!("verb:{v}")).collect();
        // Serialize via serde/toml rather than hand-formatting the TOML
        // string: `script_path.display()` on Windows renders backslashes
        // (`C:\Users\...`), and TOML string literals treat `\` as an
        // escape-sequence introducer — `format!`-ing it in raw produced
        // "invalid unicode 8-digit hex code" parse errors in Windows CI.
        // `toml::to_string` escapes the path correctly for every platform.
        let manifest = PluginManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            kind: "adapter".to_string(),
            command: "python3".to_string(),
            args: vec![script_path.to_string_lossy().to_string()],
            capabilities,
            description: None,
            timeout_secs: None,
            protocol_version: None,
            min_daemon_version: None,
            source_url: None,
            staging_env: Default::default(),
        };
        fs::write(
            plugin_dir.join("plugin.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        plugin_dir
    }

    const ECHO_RISK_PLUGIN: &str = r#"
import json
import sys

line = sys.stdin.readline()
req = json.loads(line)
method = req["method"]
params = req["params"]
payload = params["payload"]

if method == "risk_score":
    amount = payload.get("amount", 0)
    result = {"risk_score": min(100, int(amount / 10)), "confidence": 0.95}
    print(json.dumps({"ok": True, "result": result}))
elif method == "execute":
    result = {"status": "filled", "amount": payload.get("amount", 0)}
    print(json.dumps({"ok": True, "result": result}))
else:
    print(json.dumps({"ok": False, "error": f"unknown method {method}"}))
"#;

    #[test]
    fn discovers_verbs_declared_via_verb_prefixed_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        write_mock_plugin(
            dir.path(),
            "paper-trading",
            &["trade.execute"],
            ECHO_RISK_PLUGIN,
        );

        let actions = discover_adapter_actions(dir.path());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_type(), "trade.execute");
    }

    #[test]
    fn plugin_with_no_verb_capabilities_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_mock_plugin(dir.path(), "no-verbs", &[], ECHO_RISK_PLUGIN);

        let actions = discover_adapter_actions(dir.path());
        assert!(actions.is_empty());
    }

    #[test]
    fn risk_score_calls_the_subprocess_and_returns_a_real_computed_score() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = write_mock_plugin(
            dir.path(),
            "paper-trading",
            &["trade.execute"],
            ECHO_RISK_PLUGIN,
        );
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml")).unwrap();
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let assessment = action.risk_score(&json!({"amount": 500})).unwrap();
        assert_eq!(assessment.risk_score, 50);
        assert_eq!(assessment.confidence, 0.95);

        // Not the hardcoded 0 the social adapter uses -- a different amount
        // produces a different score.
        let assessment_hi = action.risk_score(&json!({"amount": 2000})).unwrap();
        assert_eq!(assessment_hi.risk_score, 100);
    }

    #[test]
    fn execute_calls_the_subprocess_and_returns_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = write_mock_plugin(
            dir.path(),
            "paper-trading",
            &["trade.execute"],
            ECHO_RISK_PLUGIN,
        );
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml")).unwrap();
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let result = action.execute(&json!({"amount": 250})).unwrap();
        assert_eq!(result["status"], "filled");
        assert_eq!(result["amount"], 250);
    }

    /// Exercises the actual shipped reference implementation
    /// (`plugins/adapter/paper-trading/`, v0.17.5.3 item 5) rather than an
    /// inline fixture -- proves the example in the repo really works, not
    /// just prose describing it.
    #[test]
    fn shipped_paper_trading_reference_plugin_scores_risk_and_executes() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let plugin_dir = repo_root
            .join("plugins")
            .join("adapter")
            .join("paper-trading");
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml"))
            .expect("plugins/adapter/paper-trading/plugin.toml must load");
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let low_risk = action
            .risk_score(&json!({"symbol": "ACME", "amount": 200}))
            .unwrap();
        assert!(
            low_risk.risk_score < 40,
            "expected a low risk score, got {low_risk:?}"
        );

        let high_risk = action
            .risk_score(&json!({"symbol": "X", "amount": 5000}))
            .unwrap();
        assert_eq!(high_risk.risk_score, 100);

        let fill = action
            .execute(&json!({"symbol": "ACME", "side": "buy", "amount": 200}))
            .unwrap();
        assert_eq!(fill["status"], "filled");
        assert_eq!(fill["venue"], "paper-trading-mock");
    }

    /// v0.17.6.3: `execute_with_secret` must attach the secret only as an
    /// env var on the plugin's own subprocess call — never as part of the
    /// request payload, and the plugin's response (echoed back verbatim by
    /// this fixture) must never be assumed safe by the caller either, but
    /// this test focuses on proving the *transport* mechanism actually
    /// delivers `TA_CONNECTOR_SECRET` into the subprocess environment.
    const ENV_ECHO_PLUGIN: &str = r#"
import json
import os
import sys

line = sys.stdin.readline()
req = json.loads(line)
method = req["method"]

if method == "execute":
    secret = os.environ.get("TA_CONNECTOR_SECRET", "")
    result = {"saw_secret": secret == "s3cr3t-value"}
    print(json.dumps({"ok": True, "result": result}))
else:
    print(json.dumps({"ok": False, "error": f"unknown method {method}"}))
"#;

    #[test]
    fn execute_with_secret_passes_secret_via_subprocess_env_not_payload() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = write_mock_plugin(
            dir.path(),
            "broker-test",
            &["trade.execute"],
            ENV_ECHO_PLUGIN,
        );
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml")).unwrap();
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let payload = json!({"symbol": "ACME"});
        let result = action
            .execute_with_secret(&payload, Some("s3cr3t-value"))
            .unwrap();
        assert_eq!(result["saw_secret"], json!(true));

        // The payload sent to the plugin never carried the secret itself --
        // only the env var did (asserted above via the plugin's own check).
        assert!(!payload.to_string().contains("s3cr3t-value"));
    }

    #[test]
    fn execute_with_secret_none_behaves_like_plain_execute() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = write_mock_plugin(
            dir.path(),
            "broker-test",
            &["trade.execute"],
            ENV_ECHO_PLUGIN,
        );
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml")).unwrap();
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let result = action.execute_with_secret(&json!({}), None).unwrap();
        assert_eq!(result["saw_secret"], json!(false));
    }

    #[test]
    fn a_failing_plugin_call_surfaces_a_clear_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        let broken_script = "import sys\nsys.exit(1)\n";
        let plugin_dir = write_mock_plugin(dir.path(), "broken", &["trade.execute"], broken_script);
        let manifest = PluginManifest::load(&plugin_dir.join("plugin.toml")).unwrap();
        let action = AdapterPluginAction::new("trade.execute", &manifest, &plugin_dir);

        let err = action.execute(&json!({"amount": 1})).unwrap_err();
        assert!(matches!(err, ActionError::Execution(_)));
    }
}
