//! `[release]` section of `.release.toml` — adapter selection and versioning config.
//!
//! Additive to the existing flat `.release.toml` fields (`prerelease`, `title_suffix`,
//! `last_release_tag`, `stable_release_tag`, `changes_since`, `nightly_tag`,
//! `nightly_history_limit`), which stay exactly as-is per `docs/release-design.md` §7 —
//! nothing is removed. This is a new, optional `[release]` table alongside them.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{ReleaseError, Result};

/// The `[release]` table in `.release.toml`. Every field is optional so a project with
/// no `[release]` section at all falls back to the zero-config GitHub path (§7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseAdapterConfig {
    /// Adapter type inferred from the scheme (`s3://`, `sftp://`, `file://`, ...).
    /// Absent → `GitHubReleaseAdapter` if a git remote is configured.
    #[serde(default)]
    pub publish_url: Option<String>,

    /// Default channel for `ta release run` when `--channel` is omitted.
    #[serde(default)]
    pub default_channel: Option<String>,

    /// Paths to bump when running a release (Cargo.toml, package.json, ...).
    /// Empty/omitted for non-semver adapters.
    #[serde(default)]
    pub version_files: Vec<String>,

    /// Optional shell command to generate a changelog before publish.
    #[serde(default)]
    pub changelog_cmd: Option<String>,

    /// Homebrew tap auto-update (v0.17.4 item 2) — not a `ReleaseAdapter`, a post-publish
    /// side effect of `GitHubReleaseAdapter` reaching the `stable` channel. Absent means
    /// no tap PR is opened. See `docs/release-design.md` §8.
    #[serde(default)]
    pub homebrew: Option<HomebrewTapConfig>,
}

/// `[release.homebrew]` — where to open the formula-bump PR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomebrewTapConfig {
    /// `owner/repo` of the tap, e.g. `"trustedautonomy/homebrew-tap"`.
    pub tap_repo: String,
    /// Path to the formula file within the tap repo, e.g. `"Formula/ta.rb"`.
    pub formula_path: String,
    /// Base branch to open the PR against.
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
}

fn default_base_branch() -> String {
    "main".to_string()
}

impl ReleaseAdapterConfig {
    /// Load the `[release]` table from a `.release.toml` file. Missing file or missing
    /// `[release]` table both fall back to `Default` (all-`None`/empty) rather than an
    /// error — this section is entirely optional.
    pub fn load(release_toml_path: &Path) -> Result<Self> {
        if !release_toml_path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(release_toml_path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            release: ReleaseAdapterConfig,
        }
        let wrapper: Wrapper = toml::from_str(text)
            .map_err(|e| ReleaseError::Config(format!("invalid .release.toml [release]: {e}")))?;
        Ok(wrapper.release)
    }

    /// Effective default channel, falling back to "stable" when unset — matches the
    /// plan's "`--channel` defaults to `nightly` for pre-release labels, `stable`
    /// otherwise" rule at the CLI layer; this is the config-level override point.
    pub fn default_channel(&self) -> &str {
        self.default_channel.as_deref().unwrap_or("stable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ReleaseAdapterConfig::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg, ReleaseAdapterConfig::default());
        assert_eq!(cfg.default_channel(), "stable");
    }

    #[test]
    fn parses_release_table() {
        let text = r#"
prerelease = true

[release]
publish_url = "s3://my-bucket/releases"
default_channel = "rc"
version_files = ["Cargo.toml"]
changelog_cmd = "git log --oneline"
"#;
        let cfg = ReleaseAdapterConfig::parse(text).unwrap();
        assert_eq!(cfg.publish_url.as_deref(), Some("s3://my-bucket/releases"));
        assert_eq!(cfg.default_channel(), "rc");
        assert_eq!(cfg.version_files, vec!["Cargo.toml"]);
        assert_eq!(cfg.changelog_cmd.as_deref(), Some("git log --oneline"));
    }

    #[test]
    fn missing_release_table_is_empty_default() {
        let text = "prerelease = true\ntitle_suffix = \"x\"\n";
        let cfg = ReleaseAdapterConfig::parse(text).unwrap();
        assert_eq!(cfg, ReleaseAdapterConfig::default());
    }

    #[test]
    fn coexists_with_legacy_flat_fields() {
        // The legacy flat fields (prerelease, last_release_tag, ...) are simply
        // ignored by this parser — they're not part of ReleaseAdapterConfig — proving
        // the two schemas can coexist in the same file without conflict.
        let text = r#"
prerelease = true
last_release_tag = "v0.16.6-alpha.1"
stable_release_tag = "public-alpha-v0.13.17.3"

[release]
default_channel = "nightly"
"#;
        let cfg = ReleaseAdapterConfig::parse(text).unwrap();
        assert_eq!(cfg.default_channel(), "nightly");
    }

    #[test]
    fn invalid_toml_is_reported() {
        let err = ReleaseAdapterConfig::parse("{{not valid toml}}").unwrap_err();
        assert!(matches!(err, ReleaseError::Config(_)));
    }

    #[test]
    fn parses_homebrew_table() {
        let text = r#"
[release]
default_channel = "stable"

[release.homebrew]
tap_repo = "trustedautonomy/homebrew-tap"
formula_path = "Formula/ta.rb"
"#;
        let cfg = ReleaseAdapterConfig::parse(text).unwrap();
        let homebrew = cfg.homebrew.expect("homebrew config present");
        assert_eq!(homebrew.tap_repo, "trustedautonomy/homebrew-tap");
        assert_eq!(homebrew.formula_path, "Formula/ta.rb");
        assert_eq!(homebrew.base_branch, "main");
    }

    #[test]
    fn homebrew_table_absent_by_default() {
        let cfg = ReleaseAdapterConfig::parse("[release]\ndefault_channel = \"stable\"\n").unwrap();
        assert!(cfg.homebrew.is_none());
    }

    #[test]
    fn homebrew_base_branch_override() {
        let text = r#"
[release.homebrew]
tap_repo = "org/tap"
formula_path = "Formula/x.rb"
base_branch = "develop"
"#;
        let cfg = ReleaseAdapterConfig::parse(text).unwrap();
        assert_eq!(cfg.homebrew.unwrap().base_branch, "develop");
    }
}
