//! Homebrew tap auto-update — PLAN.md v0.17.4 item 2.
//!
//! Not a `ReleaseAdapter`: per `docs/release-design.md` §8, this is a post-publish side
//! effect of `GitHubReleaseAdapter` reaching the `stable` channel (opening a PR in a
//! separate tap repo), not a distinct publish target. It reuses `GitHubReleaseAdapter`'s
//! `GhRunner` abstraction so it stays subprocess-mockable in tests, and drives the tap
//! repo entirely through `gh api`/`gh pr create` — no local clone of the tap repo needed.

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::adapters::github::GhRunner;
use crate::config::HomebrewTapConfig;
use crate::error::{ReleaseError, Result};

/// Result of a successful tap update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomebrewTapPr {
    /// PR URL, when `gh pr create` reported one (it prints the URL to stdout on success).
    pub url: Option<String>,
    pub branch: String,
}

pub struct HomebrewTapUpdater {
    runner: Box<dyn GhRunner>,
}

impl Default for HomebrewTapUpdater {
    fn default() -> Self {
        Self {
            runner: crate::adapters::github::default_gh_runner(),
        }
    }
}

impl HomebrewTapUpdater {
    pub fn with_runner(runner: Box<dyn GhRunner>) -> Self {
        Self { runner }
    }

    /// Open a PR in `config.tap_repo` bumping `config.formula_path`'s version string
    /// (every literal occurrence of `old_version` is replaced with `new_version`, which
    /// covers both a `version "..."` line and a download URL embedding the version) and
    /// its `sha256 "..."` line to `new_sha256`.
    ///
    /// Sequence (no local clone — everything via `gh api`):
    /// 1. `GET contents/<formula_path>` — current text + blob sha.
    /// 2. `GET git/refs/heads/<base_branch>` — base commit sha for the new branch.
    /// 3. `POST git/refs` — create `ta-release-<new_version>` off that base sha.
    /// 4. `PUT contents/<formula_path>` — commit updated text to the new branch.
    /// 5. `gh pr create` — open the PR.
    pub fn update_formula(
        &self,
        config: &HomebrewTapConfig,
        old_version: &str,
        new_version: &str,
        new_sha256: &str,
    ) -> Result<HomebrewTapPr> {
        let contents_path = format!("repos/{}/contents/{}", config.tap_repo, config.formula_path);

        let raw = self
            .runner
            .run(&["api", &contents_path])
            .map_err(|reason| {
                homebrew_err(format!(
                    "failed to read '{}': {reason}",
                    config.formula_path
                ))
            })?;
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            ReleaseError::Config(format!("invalid `gh api {contents_path}` response: {e}"))
        })?;
        let blob_sha = parsed["sha"]
            .as_str()
            .ok_or_else(|| homebrew_err("gh api contents response missing 'sha'".to_string()))?
            .to_string();
        let encoded_content = parsed["content"].as_str().ok_or_else(|| {
            homebrew_err("gh api contents response missing 'content'".to_string())
        })?;
        let decoded = STANDARD
            .decode(encoded_content.replace('\n', ""))
            .map_err(|e| {
                ReleaseError::Config(format!("formula content is not valid base64: {e}"))
            })?;
        let formula_text = String::from_utf8(decoded).map_err(|e| {
            ReleaseError::Config(format!("formula content is not valid UTF-8: {e}"))
        })?;

        let updated_text = substitute_formula(&formula_text, old_version, new_version, new_sha256);

        let ref_path = format!(
            "repos/{}/git/refs/heads/{}",
            config.tap_repo, config.base_branch
        );
        let ref_raw = self.runner.run(&["api", &ref_path]).map_err(|reason| {
            homebrew_err(format!(
                "failed to read base branch '{}': {reason}",
                config.base_branch
            ))
        })?;
        let ref_parsed: serde_json::Value = serde_json::from_str(&ref_raw).map_err(|e| {
            ReleaseError::Config(format!("invalid `gh api {ref_path}` response: {e}"))
        })?;
        let base_sha = ref_parsed["object"]["sha"]
            .as_str()
            .ok_or_else(|| homebrew_err("gh api refs response missing 'object.sha'".to_string()))?
            .to_string();

        let branch_name = format!("ta-release-{}", sanitize_branch_suffix(new_version));
        let refs_create_path = format!("repos/{}/git/refs", config.tap_repo);
        self.runner
            .run(&[
                "api",
                &refs_create_path,
                "-f",
                &format!("ref=refs/heads/{branch_name}"),
                "-f",
                &format!("sha={base_sha}"),
            ])
            .map_err(|reason| {
                homebrew_err(format!("failed to create branch '{branch_name}': {reason}"))
            })?;

        let new_content_b64 = STANDARD.encode(updated_text.as_bytes());
        let message = format!("ta: bump {} to {new_version}", config.formula_path);
        self.runner
            .run(&[
                "api",
                "--method",
                "PUT",
                &contents_path,
                "-f",
                &format!("message={message}"),
                "-f",
                &format!("content={new_content_b64}"),
                "-f",
                &format!("sha={blob_sha}"),
                "-f",
                &format!("branch={branch_name}"),
            ])
            .map_err(|reason| {
                homebrew_err(format!(
                    "failed to update '{}': {reason}",
                    config.formula_path
                ))
            })?;

        let title = format!("Update {} to {new_version}", config.formula_path);
        let body = format!(
            "Automated Homebrew formula bump to {new_version} (sha256: {new_sha256}), opened by `ta release`."
        );
        let pr_output = self
            .runner
            .run(&[
                "pr",
                "create",
                "--repo",
                &config.tap_repo,
                "--base",
                &config.base_branch,
                "--head",
                &branch_name,
                "--title",
                &title,
                "--body",
                &body,
            ])
            .map_err(|reason| homebrew_err(format!("failed to open PR: {reason}")))?;

        Ok(HomebrewTapPr {
            url: (!pr_output.trim().is_empty()).then(|| pr_output.trim().to_string()),
            branch: branch_name,
        })
    }
}

fn homebrew_err(reason: String) -> ReleaseError {
    ReleaseError::PublishFailed {
        adapter: "homebrew".to_string(),
        reason,
    }
}

fn sanitize_branch_suffix(version: &str) -> String {
    version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Replace every occurrence of `old_version` with `new_version` (covers a `version "..."`
/// line and a download URL embedding the version), then replace the quoted 64-hex-char
/// value on any line mentioning `sha256` with `new_sha256`. No Ruby parsing — a plain
/// text substitution, matching what the plan item actually asks for.
fn substitute_formula(
    text: &str,
    old_version: &str,
    new_version: &str,
    new_sha256: &str,
) -> String {
    let with_version = if old_version.is_empty() {
        text.to_string()
    } else {
        text.replace(old_version, new_version)
    };
    with_version
        .lines()
        .map(|line| {
            if line.contains("sha256") {
                replace_quoted_hex(line, new_sha256)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_quoted_hex(line: &str, new_hex: &str) -> String {
    if let Some(start) = line.find('"') {
        if let Some(rel_end) = line[start + 1..].find('"') {
            let end = start + 1 + rel_end;
            let candidate = &line[start + 1..end];
            if candidate.len() == 64 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
                return format!("{}\"{new_hex}\"{}", &line[..start], &line[end + 1..]);
            }
        }
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockGh {
        calls: Mutex<Vec<Vec<String>>>,
        responses: std::collections::HashMap<String, std::result::Result<String, String>>,
    }

    impl MockGh {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: std::collections::HashMap::new(),
            }
        }
        fn with_response(mut self, args: &[&str], response: &str) -> Self {
            self.responses
                .insert(args.join(" "), Ok(response.to_string()));
            self
        }
    }

    impl GhRunner for MockGh {
        fn run(&self, args: &[&str]) -> std::result::Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            let key = args.join(" ");
            self.responses
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Ok(String::new()))
        }
    }

    fn config() -> HomebrewTapConfig {
        HomebrewTapConfig {
            tap_repo: "trustedautonomy/homebrew-tap".to_string(),
            formula_path: "Formula/ta.rb".to_string(),
            base_branch: "main".to_string(),
        }
    }

    fn formula_b64(text: &str) -> String {
        STANDARD.encode(text.as_bytes())
    }

    #[test]
    fn substitute_formula_replaces_version_and_sha256() {
        let formula = "class Ta < Formula\n  version \"0.17.3\"\n  url \"https://example.com/v0.17.3/ta.tar.gz\"\n  sha256 \"0000000000000000000000000000000000000000000000000000000000000000\"\nend\n";
        // trim the sha to a valid 64-char hex first
        let old_sha = "a".repeat(64);
        let formula = formula.replace(&"0".repeat(68), &old_sha);
        let new_sha = "b".repeat(64);
        let updated = substitute_formula(&formula, "0.17.3", "0.17.4", &new_sha);
        assert!(updated.contains("version \"0.17.4\""));
        assert!(updated.contains("url \"https://example.com/v0.17.4/ta.tar.gz\""));
        assert!(updated.contains(&format!("sha256 \"{new_sha}\"")));
        assert!(!updated.contains(&old_sha));
    }

    #[test]
    fn update_formula_opens_pr_with_expected_sequence() {
        let old_sha = "a".repeat(64);
        let new_sha = "b".repeat(64);
        let formula_text =
            format!("class Ta < Formula\n  version \"0.17.3\"\n  sha256 \"{old_sha}\"\nend\n");
        let mock = MockGh::new()
            .with_response(
                &["api", "repos/trustedautonomy/homebrew-tap/contents/Formula/ta.rb"],
                &serde_json::json!({
                    "sha": "blobsha123",
                    "content": formula_b64(&formula_text),
                })
                .to_string(),
            )
            .with_response(
                &["api", "repos/trustedautonomy/homebrew-tap/git/refs/heads/main"],
                &serde_json::json!({"object": {"sha": "basesha456"}}).to_string(),
            )
            .with_response(
                &[
                    "pr",
                    "create",
                    "--repo",
                    "trustedautonomy/homebrew-tap",
                    "--base",
                    "main",
                    "--head",
                    "ta-release-0-17-4",
                    "--title",
                    "Update Formula/ta.rb to 0.17.4",
                    "--body",
                    &format!(
                        "Automated Homebrew formula bump to 0.17.4 (sha256: {new_sha}), opened by `ta release`."
                    ),
                ],
                "https://github.com/trustedautonomy/homebrew-tap/pull/7",
            );

        let updater = HomebrewTapUpdater::with_runner(Box::new(mock));
        let result = updater
            .update_formula(&config(), "0.17.3", "0.17.4", &new_sha)
            .unwrap();
        assert_eq!(result.branch, "ta-release-0-17-4");
        assert_eq!(
            result.url.as_deref(),
            Some("https://github.com/trustedautonomy/homebrew-tap/pull/7")
        );
    }

    #[test]
    fn update_formula_fails_when_contents_fetch_fails() {
        struct FailingGh;
        impl GhRunner for FailingGh {
            fn run(&self, _args: &[&str]) -> std::result::Result<String, String> {
                Err("404 Not Found".to_string())
            }
        }
        let updater = HomebrewTapUpdater::with_runner(Box::new(FailingGh));
        let err = updater
            .update_formula(&config(), "0.17.3", "0.17.4", "abc")
            .unwrap_err();
        assert!(matches!(err, ReleaseError::PublishFailed { .. }));
    }

    #[test]
    fn sanitize_branch_suffix_replaces_dots() {
        assert_eq!(sanitize_branch_suffix("0.17.4"), "0-17-4");
    }
}
