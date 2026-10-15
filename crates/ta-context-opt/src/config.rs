use serde::{Deserialize, Serialize};

use crate::{OptError, Registry, Slot};

/// Suffix of the flat override key for a slot, e.g. `optimizers.input` + `.use`.
pub const USE_KEY_SUFFIX: &str = "use";

/// One slot's selection: `[optimizers.input] use = "rtk"`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotSelection {
    /// Optimizer name. `None` means "not chosen": the slot keeps legacy
    /// behaviour (for input, whatever `[compression]` says).
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    pub use_: Option<String>,
    /// Per-optimizer tables, e.g. `[optimizers.input.rtk]`. Kept raw so a malformed table is
    /// reported by [`SlotSelection::hook_override`] instead of silently dropping the whole file.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl SlotSelection {
    /// The `[optimizers.<slot>.<name>]` table as a hook spec override, if present.
    pub fn hook_override(&self, name: &str) -> Result<Option<crate::HookSpec>, OptError> {
        match self.extra.get(name) {
            None => Ok(None),
            Some(v) => serde_json::from_value(v.clone())
                .map(Some)
                .map_err(|e| OptError::BadSpec {
                    name: name.to_string(),
                    detail: e.to_string(),
                }),
        }
    }
}

/// `[optimizers.*]` from daemon.toml.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OptimizersConfig {
    pub input: SlotSelection,
    pub context: SlotSelection,
    pub output: SlotSelection,
}

impl OptimizersConfig {
    /// Apply goal experiment overrides (`optimizers.input.use` -> `"none"`).
    /// Keys are flat, as `ta golden replay --set` stores them. Non-string
    /// values and unrelated keys are ignored.
    pub fn with_overrides(mut self, overrides: Option<&serde_json::Value>) -> Self {
        let Some(map) = overrides.and_then(|v| v.as_object()) else {
            return self;
        };
        for (slot, sel) in [
            ("input", &mut self.input),
            ("context", &mut self.context),
            ("output", &mut self.output),
        ] {
            let key = format!("optimizers.{slot}.{USE_KEY_SUFFIX}");
            if let Some(name) = map.get(&key).and_then(|v| v.as_str()) {
                sel.use_ = Some(name.trim().to_ascii_lowercase());
            }
        }
        self
    }

    /// Check every `optimizers.<slot>.use` override names a registered
    /// optimizer, so a typo in `--set` fails before a replay spends tokens.
    pub fn validate_overrides(
        overrides: &serde_json::Value,
        registry: &Registry,
    ) -> Result<(), OptError> {
        let Some(map) = overrides.as_object() else {
            return Ok(());
        };
        for slot in [Slot::Input, Slot::Context, Slot::Output] {
            let key = format!("optimizers.{}.{USE_KEY_SUFFIX}", slot.name());
            let Some(value) = map.get(&key) else { continue };
            let name = value
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if !registry.names(slot).contains(&name) {
                return Err(OptError::Unknown {
                    slot: slot.name(),
                    name: value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or(value.to_string()),
                    available: registry.names(slot).join(", "),
                });
            }
            registry.ensure_wired(slot, &name)?;
        }
        Ok(())
    }
}
