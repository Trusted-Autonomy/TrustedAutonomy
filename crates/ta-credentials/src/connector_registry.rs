// connector_registry.rs — symbolic connector-id -> credential/broker mapping
// (v0.17.6.3, PLAN item 3).
//
// `ta_external_action` and similar MCP tool schemas expose only symbolic
// connector ids to the agent (e.g. "github", "slack-ops") — never a
// `Credential.secret` value. This registry, loaded from
// `<project_root>/.ta/connectors.toml`, is what maps a symbolic id back to
// the vault credential that actually backs it, and declares whether that
// connector's secret is resolved server-side by the gateway broker
// (`broker_mediated = true`) or still reaches the agent process directly via
// `bare_process.rs`'s env-injection fallback (`broker_mediated = false`,
// the default — migration is connector-by-connector, not a flag day).
//
// Example `.ta/connectors.toml`:
//
// ```toml
// [connectors.github]
// credential_name = "GITHUB_TOKEN"
// broker_mediated = true
// required_scope = "repo.write"
//
// [connectors.slack-ops]
// credential_name = "SLACK_BOT_TOKEN"
// broker_mediated = false
// ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Registry entry for one symbolic connector id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorEntry {
    /// Name of the vault `Credential` (see `ta_credentials::Credential.name`)
    /// backing this connector.
    pub credential_name: String,

    /// Whether the gateway broker resolves and attaches this connector's
    /// secret itself (never returning it to the agent), or leaves the
    /// credential to reach the agent process the old way, via
    /// `bare_process.rs`'s direct env-injection fallback. Defaults to
    /// `false` so declaring a connector doesn't silently change its
    /// security posture — migration is explicit, connector-by-connector.
    #[serde(default)]
    pub broker_mediated: bool,

    /// Scope string required to invoke this connector through the broker
    /// (checked against the caller's `SessionToken.allowed_scopes`). Only
    /// consulted when `broker_mediated` is `true`.
    #[serde(default)]
    pub required_scope: Option<String>,

    /// Hostnames this connector's credential shim should match (v0.17.6.7),
    /// e.g. `["github.com"]` for git's `host=` credential-protocol field or
    /// the GitHub CLI's API host. Only consulted by the Stage 7 shell/CLI
    /// shims (`ta credential-helper`, the `gh` wrapper binary) — the
    /// MCP-tool-call broker path (v0.17.6.3) never needs a hostname, since
    /// the agent already names the connector explicitly. Empty by default:
    /// a connector with no declared hosts is invisible to the shell shims,
    /// even if `broker_mediated` is `true`.
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// Every connector declared in `.ta/connectors.toml`, keyed by symbolic id.
///
/// Access via [`ConnectorRegistry::load`] — returns an empty registry
/// (never an error) when the file is missing or unparsable, matching
/// `ta_actions::ActionPolicies::load`'s safe-default behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorRegistry {
    #[serde(default)]
    pub connectors: HashMap<String, ConnectorEntry>,
}

impl ConnectorRegistry {
    /// Load the registry from `<ta_dir>/connectors.toml`.
    ///
    /// A missing file or a parse failure both fall back to an empty
    /// registry (logged at `warn` for the parse-failure case) rather than
    /// erroring — the safe default when a connector isn't declared is "not
    /// broker-mediated, no known credential", which every lookup call site
    /// already handles explicitly.
    pub fn load(ta_dir: &Path) -> Self {
        let path = ta_dir.join("connectors.toml");
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => Self::parse(&content),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read connectors.toml; using empty connector registry"
                );
                Self::default()
            }
        }
    }

    fn parse(content: &str) -> Self {
        match toml::from_str::<Self>(content) {
            Ok(registry) => registry,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to parse connectors.toml; using empty connector registry"
                );
                Self::default()
            }
        }
    }

    /// Look up a connector by its symbolic id.
    pub fn get(&self, connector_id: &str) -> Option<&ConnectorEntry> {
        self.connectors.get(connector_id)
    }

    /// Whether `credential_name` is declared as `broker_mediated = true` by
    /// any connector entry. Used by `bare_process.rs`'s env-injection path
    /// to decide whether a credential must be withheld from the agent's
    /// direct environment (v0.17.6.3 item 5).
    pub fn is_broker_mediated_credential(&self, credential_name: &str) -> bool {
        self.connectors
            .values()
            .any(|c| c.credential_name == credential_name && c.broker_mediated)
    }

    /// Find the broker-mediated connector entry declaring `host` (v0.17.6.7)
    /// — used by the shell/CLI credential shims (`ta credential-helper`,
    /// the `gh` wrapper) to map a request's hostname back to a vault
    /// credential. A connector that isn't `broker_mediated`, or that
    /// declares no `hosts` at all, is never returned here — those stay on
    /// the reduced-security env-injection fallback, exactly as before this
    /// connector existed.
    pub fn find_by_host(&self, host: &str) -> Option<(&str, &ConnectorEntry)> {
        self.connectors
            .iter()
            .find(|(_, entry)| {
                entry.broker_mediated && entry.hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
            })
            .map(|(id, entry)| (id.as_str(), entry))
    }

    /// Whether any declared connector is both `broker_mediated` and has at
    /// least one `hosts` entry — the precondition for installing the Stage 7
    /// shell/CLI shims at all (v0.17.6.7). A project with no such connector
    /// gets no git-config mutation and no `PATH` change, keeping the shims
    /// entirely opt-in per `.ta/connectors.toml`.
    pub fn has_shell_shim_connector(&self) -> bool {
        self.connectors
            .values()
            .any(|c| c.broker_mediated && !c.hosts.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ConnectorRegistry::load(dir.path());
        assert!(registry.connectors.is_empty());
    }

    #[test]
    fn parses_broker_mediated_and_fallback_connectors() {
        let toml = r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = true
required_scope = "repo.write"

[connectors.slack-ops]
credential_name = "SLACK_BOT_TOKEN"
"#;
        let registry = ConnectorRegistry::parse(toml);

        let github = registry.get("github").unwrap();
        assert!(github.broker_mediated);
        assert_eq!(github.required_scope.as_deref(), Some("repo.write"));

        let slack = registry.get("slack-ops").unwrap();
        assert!(
            !slack.broker_mediated,
            "broker_mediated must default to false"
        );
        assert_eq!(slack.required_scope, None);

        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connectors.toml"),
            b"[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\nbroker_mediated = true\n",
        )
        .unwrap();
        let registry = ConnectorRegistry::load(dir.path());
        assert!(registry.get("github").unwrap().broker_mediated);
    }

    #[test]
    fn malformed_toml_falls_back_to_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("connectors.toml"), b"not valid toml [[[").unwrap();
        let registry = ConnectorRegistry::load(dir.path());
        assert!(registry.connectors.is_empty());
    }

    #[test]
    fn is_broker_mediated_credential_checks_across_all_connectors() {
        let toml = r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = true

[connectors.slack-ops]
credential_name = "SLACK_BOT_TOKEN"
broker_mediated = false
"#;
        let registry = ConnectorRegistry::parse(toml);
        assert!(registry.is_broker_mediated_credential("GITHUB_TOKEN"));
        assert!(!registry.is_broker_mediated_credential("SLACK_BOT_TOKEN"));
        assert!(!registry.is_broker_mediated_credential("UNDECLARED_TOKEN"));
    }

    #[test]
    fn find_by_host_matches_broker_mediated_connector_with_declared_hosts() {
        let toml = r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = true
hosts = ["github.com", "gist.github.com"]
"#;
        let registry = ConnectorRegistry::parse(toml);

        let (id, entry) = registry.find_by_host("github.com").unwrap();
        assert_eq!(id, "github");
        assert_eq!(entry.credential_name, "GITHUB_TOKEN");

        // Case-insensitive, and a second declared host resolves too.
        let (id2, _) = registry.find_by_host("GIST.GITHUB.COM").unwrap();
        assert_eq!(id2, "github");

        assert!(registry.find_by_host("gitlab.com").is_none());
    }

    #[test]
    fn find_by_host_ignores_non_broker_mediated_and_hostless_connectors() {
        let toml = r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = false
hosts = ["github.com"]

[connectors.slack-ops]
credential_name = "SLACK_BOT_TOKEN"
broker_mediated = true
"#;
        let registry = ConnectorRegistry::parse(toml);

        // Not broker_mediated -> never matched by the shell shims, even
        // though it declares the host.
        assert!(registry.find_by_host("github.com").is_none());
        // broker_mediated but no `hosts` declared -> never matched either.
        assert!(registry.find_by_host("slack.com").is_none());
    }

    #[test]
    fn has_shell_shim_connector_requires_broker_mediated_and_hosts() {
        assert!(!ConnectorRegistry::default().has_shell_shim_connector());

        let none_qualify = ConnectorRegistry::parse(
            r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = true

[connectors.slack-ops]
credential_name = "SLACK_BOT_TOKEN"
broker_mediated = false
hosts = ["slack.com"]
"#,
        );
        assert!(!none_qualify.has_shell_shim_connector());

        let qualifies = ConnectorRegistry::parse(
            r#"
[connectors.github]
credential_name = "GITHUB_TOKEN"
broker_mediated = true
hosts = ["github.com"]
"#,
        );
        assert!(qualifies.has_shell_shim_connector());
    }
}
