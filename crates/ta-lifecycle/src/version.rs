//! Build identity and comparison.
//!
//! A TA binary is identified by its semver and the short VCS revision stamped
//! at compile time (`TA_GIT_HASH`). The daemon reports both in `GET /api/status`
//! (`version` / `daemon_version` and `build_sha`) and both binaries print them
//! on `--version`. Comparing the hash catches rebuilds within one semver.

use serde::{Deserialize, Serialize};

/// Version plus optional build hash of one binary.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    /// Short VCS revision. `None` when the binary does not report one or it
    /// reports a placeholder (`""`, `"?"`, `"unknown"`).
    pub build_hash: Option<String>,
}

fn normalize_hash(raw: &str) -> Option<String> {
    let h = raw.trim();
    if h.is_empty() || h == "?" || h.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(h.to_string())
    }
}

impl BuildIdentity {
    pub fn new(version: impl Into<String>, build_hash: Option<&str>) -> Self {
        Self {
            version: version.into(),
            build_hash: build_hash.and_then(normalize_hash),
        }
    }

    /// Build from the daemon's `/api/status` JSON, reusing the fields the
    /// version guard already reads (`daemon_version` falling back to `version`,
    /// and `build_sha`). `None` when the version is missing or `"?"`.
    pub fn from_status_json(status: &serde_json::Value) -> Option<Self> {
        let version = status["daemon_version"]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| status["version"].as_str())?
            .trim();
        if version.is_empty() || version == "?" {
            return None;
        }
        Some(Self::new(version, status["build_sha"].as_str()))
    }

    /// Parse `--version` output such as `ta-daemon 0.17.11-alpha.26 (abc1234)`
    /// or `ta 0.17.11-alpha.26 (abc1234 2026-02-11)`. Output with no hash
    /// yields an identity whose `build_hash` is `None`.
    pub fn parse_version_output(output: &str) -> Option<Self> {
        let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
        let mut tokens = line.split_whitespace();
        let version = tokens.find(|t| t.starts_with(|c: char| c.is_ascii_digit()))?;
        let hash = tokens
            .next()
            .and_then(|t| t.strip_prefix('('))
            .map(|t| t.trim_end_matches(')'));
        Some(Self::new(version, hash))
    }

    /// `0.17.11-alpha.26 (abc1234)` or just the version when there is no hash.
    pub fn describe(&self) -> String {
        match &self.build_hash {
            Some(h) => format!("{} ({})", self.version, h),
            None => self.version.clone(),
        }
    }
}

/// Result of comparing the running build with an installed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildComparison {
    Same,
    /// Same semver, different build hash (a rebuild).
    DiffersByHash,
    /// Different semver (or one side has no hash to compare).
    DiffersByVersion,
}

impl BuildComparison {
    pub fn is_stale(&self) -> bool {
        !matches!(self, BuildComparison::Same)
    }
}

/// Compare `running` with `installed`. Mirrors the CLI version guard: equal
/// hashes mean the same build; when either side has no hash the versions
/// decide; otherwise a hash mismatch is a different build.
pub fn compare_builds(running: &BuildIdentity, installed: &BuildIdentity) -> BuildComparison {
    match (&running.build_hash, &installed.build_hash) {
        (Some(a), Some(b)) if a == b => BuildComparison::Same,
        (Some(_), Some(_)) if running.version == installed.version => {
            BuildComparison::DiffersByHash
        }
        (Some(_), Some(_)) => BuildComparison::DiffersByVersion,
        _ if running.version == installed.version => BuildComparison::Same,
        _ => BuildComparison::DiffersByVersion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daemon_and_cli_version_output() {
        let d =
            BuildIdentity::parse_version_output("ta-daemon 0.17.11-alpha.26 (abc1234)\n").unwrap();
        assert_eq!(d, BuildIdentity::new("0.17.11-alpha.26", Some("abc1234")));
        let c = BuildIdentity::parse_version_output("ta 0.17.11-alpha.26 (abc1234 2026-02-11)")
            .unwrap();
        assert_eq!(c.build_hash.as_deref(), Some("abc1234"));
    }

    #[test]
    fn old_binary_without_hash_parses_to_version_only() {
        let d = BuildIdentity::parse_version_output("ta-daemon 0.17.11-alpha.20").unwrap();
        assert_eq!(d.build_hash, None);
        assert!(BuildIdentity::parse_version_output("garbage without digits").is_none());
        assert!(BuildIdentity::parse_version_output("").is_none());
    }

    #[test]
    fn placeholder_hashes_are_none() {
        assert_eq!(
            BuildIdentity::new("1.0.0", Some("unknown")).build_hash,
            None
        );
        assert_eq!(BuildIdentity::new("1.0.0", Some("?")).build_hash, None);
        assert_eq!(BuildIdentity::new("1.0.0", Some("")).build_hash, None);
    }

    #[test]
    fn status_json_uses_the_reported_fields() {
        let j =
            serde_json::json!({"version":"1.2.3","daemon_version":"1.2.3","build_sha":"deadbee"});
        assert_eq!(
            BuildIdentity::from_status_json(&j),
            Some(BuildIdentity::new("1.2.3", Some("deadbee")))
        );
        assert!(BuildIdentity::from_status_json(&serde_json::json!({"version":"?"})).is_none());
        let old = serde_json::json!({"version":"1.2.3"});
        assert_eq!(
            BuildIdentity::from_status_json(&old).unwrap().build_hash,
            None
        );
    }

    #[test]
    fn comparison_rules() {
        let a = BuildIdentity::new("1.0.0", Some("aaa"));
        assert_eq!(compare_builds(&a, &a.clone()), BuildComparison::Same);
        assert_eq!(
            compare_builds(&a, &BuildIdentity::new("1.0.0", Some("bbb"))),
            BuildComparison::DiffersByHash
        );
        assert_eq!(
            compare_builds(&a, &BuildIdentity::new("1.0.1", Some("bbb"))),
            BuildComparison::DiffersByVersion
        );
        // No hash on one side: versions decide.
        assert_eq!(
            compare_builds(&a, &BuildIdentity::new("1.0.0", None)),
            BuildComparison::Same
        );
        assert_eq!(
            compare_builds(&a, &BuildIdentity::new("0.9.0", None)),
            BuildComparison::DiffersByVersion
        );
    }
}
