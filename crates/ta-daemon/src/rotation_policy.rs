// rotation_policy.rs -- minimum delay between round-robin rotation cycles.
//
// A successful rotation cycle used to sleep zero seconds, so a one-stage
// workflow launched `ta run` for that stage's role back to back forever,
// burning real agent runs (found live, 2026-10-08). This module owns the one
// setting that rate-limits rotation: `[team_session] rotation_min_delay_secs`
// in `.ta/workflow.toml`.
//
// It only paces rotation. Wake-on-demand launches (`wake_listener.rs`) and
// their retry, backoff and idempotency behavior (`wake_retry.rs`) never read
// it.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// Default minimum delay between two rotation cycles of one session.
pub const DEFAULT_ROTATION_MIN_DELAY_SECS: u64 = 30;
/// Smallest accepted value. Zero would re-allow the tight launch loop.
pub const MIN_ROTATION_MIN_DELAY_SECS: u64 = 1;
/// Largest accepted value (one day); beyond this a typo is far likelier than intent.
pub const MAX_ROTATION_MIN_DELAY_SECS: u64 = 86_400;

/// Where the effective value came from, for the startup log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelaySource {
    Default,
    Configured,
    /// The file held an unusable value; the default was used instead.
    InvalidFellBackToDefault,
}

impl DelaySource {
    pub fn as_str(self) -> &'static str {
        match self {
            DelaySource::Default => "default",
            DelaySource::Configured => "workflow.toml",
            DelaySource::InvalidFellBackToDefault => "default (configured value rejected)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationPolicy {
    pub min_delay: Duration,
    pub source: DelaySource,
    /// Set when the configured value was rejected; says what to fix.
    pub problem: Option<String>,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            min_delay: Duration::from_secs(DEFAULT_ROTATION_MIN_DELAY_SECS),
            source: DelaySource::Default,
            problem: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct TeamSessionSection {
    // i64 so a negative value is reported as out of range, not as a parse error.
    #[serde(default)]
    rotation_min_delay_secs: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkflowToml {
    #[serde(default)]
    team_session: TeamSessionSection,
}

impl RotationPolicy {
    /// Validates a raw configured value. `Err` carries an actionable message.
    pub fn validate(secs: i64) -> Result<Duration, String> {
        let min = MIN_ROTATION_MIN_DELAY_SECS as i64;
        let max = MAX_ROTATION_MIN_DELAY_SECS as i64;
        if secs < min || secs > max {
            return Err(format!(
                "[team_session] rotation_min_delay_secs = {secs} is outside the accepted range \
                 {min}..={max} seconds (0 is refused because it allows a tight loop of agent \
                 launches). Set a value in that range in .ta/workflow.toml, or remove the \
                 setting to use the default of {DEFAULT_ROTATION_MIN_DELAY_SECS} seconds."
            ));
        }
        Ok(Duration::from_secs(secs as u64))
    }

    /// Loads the policy from `<project_root>/.ta/workflow.toml`. A missing
    /// file or setting gives the default; an unusable value or unreadable TOML
    /// also gives the default, with `problem` set so the caller can log it.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".ta").join("workflow.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let parsed = match toml::from_str::<WorkflowToml>(&content) {
            Ok(p) => p,
            Err(e) => {
                return Self {
                    source: DelaySource::InvalidFellBackToDefault,
                    problem: Some(format!(
                        "could not parse [team_session] in {}: {e}. Fix the TOML, or remove \
                         rotation_min_delay_secs to use the default of \
                         {DEFAULT_ROTATION_MIN_DELAY_SECS} seconds.",
                        path.display()
                    )),
                    ..Self::default()
                };
            }
        };
        match parsed.team_session.rotation_min_delay_secs {
            None => Self::default(),
            Some(secs) => match Self::validate(secs) {
                Ok(min_delay) => Self {
                    min_delay,
                    source: DelaySource::Configured,
                    problem: None,
                },
                Err(problem) => Self {
                    source: DelaySource::InvalidFellBackToDefault,
                    problem: Some(problem),
                    ..Self::default()
                },
            },
        }
    }

    /// Logs the effective value for one session: the startup line.
    pub fn log_effective(&self, session_id: &str) {
        if let Some(problem) = &self.problem {
            tracing::error!(session = %session_id, problem = %problem, "team_session: invalid rotation delay setting, using the default");
        }
        tracing::info!(
            session = %session_id,
            rotation_min_delay_secs = self.min_delay.as_secs(),
            source = self.source.as_str(),
            "team_session: rotation runs at most one cycle per {}s for this session \
             (set [team_session] rotation_min_delay_secs in .ta/workflow.toml to change it; \
             wake-on-demand launches are not delayed)",
            self.min_delay.as_secs()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_toml(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".ta")).unwrap();
        std::fs::write(dir.join(".ta/workflow.toml"), body).unwrap();
    }

    #[test]
    fn default_is_thirty_seconds_when_file_or_setting_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RotationPolicy::load(dir.path()).min_delay.as_secs(), 30);
        write_toml(dir.path(), "[whiteboard]\nenabled = true\n");
        let p = RotationPolicy::load(dir.path());
        assert_eq!(p.min_delay.as_secs(), 30);
        assert_eq!(p.source, DelaySource::Default);
        assert!(p.problem.is_none());
    }

    #[test]
    fn configured_value_is_used() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "[team_session]\nrotation_min_delay_secs = 120\n",
        );
        let p = RotationPolicy::load(dir.path());
        assert_eq!(p.min_delay.as_secs(), 120);
        assert_eq!(p.source, DelaySource::Configured);
    }

    #[test]
    fn zero_negative_and_huge_values_are_rejected_with_an_actionable_message() {
        for bad in ["0", "-5", "86401"] {
            let dir = tempfile::tempdir().unwrap();
            write_toml(
                dir.path(),
                &format!("[team_session]\nrotation_min_delay_secs = {bad}\n"),
            );
            let p = RotationPolicy::load(dir.path());
            assert_eq!(p.min_delay.as_secs(), DEFAULT_ROTATION_MIN_DELAY_SECS);
            assert_eq!(p.source, DelaySource::InvalidFellBackToDefault);
            let msg = p.problem.expect("a rejected value must carry a message");
            assert!(msg.contains(bad), "{msg}");
            assert!(msg.contains("workflow.toml"), "{msg}");
            assert!(msg.contains("default"), "{msg}");
        }
    }

    #[test]
    fn malformed_toml_falls_back_with_a_message() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[team_session\nrotation_min_delay_secs = 5\n");
        let p = RotationPolicy::load(dir.path());
        assert_eq!(p.min_delay.as_secs(), DEFAULT_ROTATION_MIN_DELAY_SECS);
        assert!(p.problem.unwrap().contains("could not parse"));
    }

    #[test]
    fn boundary_values_are_accepted() {
        assert!(RotationPolicy::validate(1).is_ok());
        assert!(RotationPolicy::validate(86_400).is_ok());
    }
}
