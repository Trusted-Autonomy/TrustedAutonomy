//! The `.ta/triggers/<type>.toml` manifest schema (§13.1), mirroring the
//! `plugin.toml` data-defined-entity pattern from v0.17.0.12.14
//! (`ta-plugin::manifest`): per-type trigger configs are data, not code, so
//! the community can create/improve trigger types the same way personas and
//! plugins are data-defined.

use crate::event::TriggerError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Whether a fired trigger should result in a goal being created directly,
/// or be queued for later batch/regular processing.
///
/// **Design decision (v0.17.0.12.19)**: this is intentionally *data*, part
/// of the per-type config, not a hardcoded Rust behavior per trigger type.
/// `ta-intake` itself never makes this choice — it only produces
/// `TriggerEvent`s and reports the configured mode for a consumer (the `ta
/// intake fire` CLI glue today, `ta-brain` from v0.17.0.12.20 on) to act on.
/// Rationale for the two shipped defaults: `schedule` fires one event per
/// interval tick, which maps naturally 1:1 onto "create one goal now"
/// (`Direct`); `inbound-email` mirrors the existing email-manager workflow's
/// batch-fetch-then-process model, which maps naturally onto `Queue`. A
/// community-authored trigger type is free to pick either regardless of
/// type — nothing in `ta-intake` special-cases the built-in two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dispatch {
    /// Every fired `TriggerEvent` should result in a goal being created
    /// right away.
    #[default]
    Direct,
    /// Fired `TriggerEvent`s are appended to a queue for later
    /// batch/regular processing instead of creating a goal immediately.
    Queue,
}

/// Parsed `.ta/triggers/<type>.toml` contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerManifest {
    /// Trigger type identifier (e.g. `"schedule"`, `"inbound-email"`).
    #[serde(rename = "type")]
    pub trigger_type: String,

    /// Whether this trigger is active. Discovery still returns disabled
    /// triggers (so `ta intake list` can show them); callers that poll
    /// should skip a manifest with `enabled = false`.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Direct-goal vs queued-batch dispatch — see `Dispatch` docs above.
    #[serde(default)]
    pub dispatch: Dispatch,

    #[serde(default)]
    pub description: Option<String>,

    /// Trigger-type-specific settings (e.g. `interval_secs` for `schedule`,
    /// `provider`/`account` for `inbound-email`). Deliberately untyped here
    /// — each `TriggerSource` implementation interprets its own keys, which
    /// is what lets a community-authored trigger type round-trip through
    /// discovery without any change to `ta-intake` itself.
    #[serde(default)]
    pub settings: HashMap<String, toml::Value>,
}

fn default_true() -> bool {
    true
}

impl TriggerManifest {
    pub fn load(path: &Path) -> Result<Self, TriggerError> {
        if !path.exists() {
            return Err(TriggerError::ConfigNotFound {
                path: path.display().to_string(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|e| TriggerError::InvalidConfig {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        toml::from_str(&text).map_err(|e| TriggerError::InvalidConfig {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// Look up a string setting, e.g. `settings.get_str("provider")`.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.settings.get(key).and_then(|v| v.as_str())
    }

    /// Look up an integer setting, e.g. `settings.get_u64("interval_secs")`.
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.settings
            .get(key)
            .and_then(|v| v.as_integer())
            .and_then(|i| u64::try_from(i).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_minimal_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.toml");
        std::fs::write(&path, "type = \"schedule\"\n").unwrap();
        let manifest = TriggerManifest::load(&path).unwrap();
        assert_eq!(manifest.trigger_type, "schedule");
        assert!(manifest.enabled);
        assert_eq!(manifest.dispatch, Dispatch::Direct);
    }

    #[test]
    fn loads_full_manifest_with_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbound-email.toml");
        std::fs::write(
            &path,
            r#"
type = "inbound-email"
enabled = true
dispatch = "queue"
description = "Polls inbound email"

[settings]
provider = "gmail"
account = "intake@example.com"
max_messages_per_poll = 25
"#,
        )
        .unwrap();
        let manifest = TriggerManifest::load(&path).unwrap();
        assert_eq!(manifest.trigger_type, "inbound-email");
        assert_eq!(manifest.dispatch, Dispatch::Queue);
        assert_eq!(manifest.get_str("provider"), Some("gmail"));
        assert_eq!(manifest.get_u64("max_messages_per_poll"), Some(25));
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        assert!(matches!(
            TriggerManifest::load(&path),
            Err(TriggerError::ConfigNotFound { .. })
        ));
    }

    #[test]
    fn invalid_toml_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();
        assert!(matches!(
            TriggerManifest::load(&path),
            Err(TriggerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn custom_community_trigger_type_round_trips() {
        // A trigger type ta-intake has never heard of, with arbitrary
        // settings, must parse and discover identically to the two shipped
        // types — proving discovery is genuinely data-defined.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webhook.toml");
        std::fs::write(
            &path,
            r#"
type = "webhook"
dispatch = "direct"
description = "Community-authored webhook trigger"

[settings]
listen_path = "/hooks/custom"
secret_env = "CUSTOM_WEBHOOK_SECRET"
"#,
        )
        .unwrap();
        let manifest = TriggerManifest::load(&path).unwrap();
        assert_eq!(manifest.trigger_type, "webhook");
        assert_eq!(manifest.get_str("listen_path"), Some("/hooks/custom"));

        let serialized = toml::to_string(&manifest).unwrap();
        let round_tripped: TriggerManifest = toml::from_str(&serialized).unwrap();
        assert_eq!(round_tripped.trigger_type, manifest.trigger_type);
        assert_eq!(
            round_tripped.get_str("listen_path"),
            manifest.get_str("listen_path")
        );
    }
}
