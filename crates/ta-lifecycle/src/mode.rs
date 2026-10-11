//! The `[daemon] auto_update` setting.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What a daemon does when a newer build is installed next to it.
///
/// * `when_idle`: restart onto the new build once nothing is running.
/// * `ask`: never restart by itself; report "update available" so the CLI's
///   existing prompt (`ta shell`, `ta dev`) or `ta daemon restart` applies it.
/// * `never`: no self-check at all.
///
/// The default is `ask`: it keeps today's behaviour (a person confirms) and
/// the first release that can restart itself cannot surprise anyone. Opt in
/// to `when_idle` with one line in `.ta/daemon.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoUpdateMode {
    WhenIdle,
    #[default]
    Ask,
    Never,
}

/// Every accepted spelling, for error messages and docs.
pub const ACCEPTED_VALUES: &str = "\"when_idle\", \"ask\" or \"never\"";

impl AutoUpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WhenIdle => "when_idle",
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    /// Whether this platform can replace the running daemon safely. Windows
    /// cannot overwrite a running executable and has no detached re-exec, so
    /// `when_idle` degrades to `ask` there (report only, never half-replace).
    pub const fn platform_can_self_restart() -> bool {
        cfg!(unix)
    }

    /// The mode actually applied on this platform.
    pub fn effective(self) -> Self {
        self.effective_with(Self::platform_can_self_restart())
    }

    pub fn effective_with(self, can_self_restart: bool) -> Self {
        if self == Self::WhenIdle && !can_self_restart {
            Self::Ask
        } else {
            self
        }
    }
}

impl fmt::Display for AutoUpdateMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AutoUpdateMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "when_idle" => Ok(Self::WhenIdle),
            "ask" => Ok(Self::Ask),
            "never" => Ok(Self::Never),
            other => Err(format!(
                "invalid auto_update value {other:?}: expected {ACCEPTED_VALUES}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_values_and_rejects_others_naming_the_choices() {
        assert_eq!("when_idle".parse(), Ok(AutoUpdateMode::WhenIdle));
        assert_eq!("ask".parse(), Ok(AutoUpdateMode::Ask));
        assert_eq!(" never ".parse(), Ok(AutoUpdateMode::Never));
        let e = "always".parse::<AutoUpdateMode>().unwrap_err();
        assert!(e.contains("always") && e.contains("when_idle"), "{e}");
    }

    #[test]
    fn default_is_the_conservative_ask() {
        assert_eq!(AutoUpdateMode::default(), AutoUpdateMode::Ask);
    }

    #[test]
    fn serde_uses_snake_case_strings() {
        let v: AutoUpdateMode = serde_json::from_str("\"when_idle\"").unwrap();
        assert_eq!(v, AutoUpdateMode::WhenIdle);
        assert_eq!(
            serde_json::to_string(&AutoUpdateMode::Never).unwrap(),
            "\"never\""
        );
        assert!(serde_json::from_str::<AutoUpdateMode>("\"WhenIdle\"").is_err());
    }

    #[test]
    fn when_idle_degrades_to_ask_where_self_restart_is_unsafe() {
        assert_eq!(
            AutoUpdateMode::WhenIdle.effective_with(false),
            AutoUpdateMode::Ask
        );
        assert_eq!(
            AutoUpdateMode::WhenIdle.effective_with(true),
            AutoUpdateMode::WhenIdle
        );
        assert_eq!(
            AutoUpdateMode::Never.effective_with(false),
            AutoUpdateMode::Never
        );
        assert_eq!(
            AutoUpdateMode::Ask.effective_with(false),
            AutoUpdateMode::Ask
        );
    }
}
