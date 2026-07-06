//! Git-specific helpers for the release pipeline.
//!
//! All direct `Command::new("git")` calls for release operations live here.
//! This is the only location in ta-cli (outside ta-submit) where direct git
//! calls are permitted for release workflows. Routing through this module
//! satisfies the VCS adapter enforcement rule (v0.15.29).
//!
//! # Release Adapter Interface (v0.15.30.2)
//!
//! The `ReleaseAdapter` trait abstracts VCS-specific release operations so the
//! pipeline can run against non-git repositories. The `GitAdapter` provides the
//! default implementation. Perforce and SVN adapters return `Err(Unsupported)`
//! with actionable messages. Custom adapters can be configured via
//! `.ta/release.yaml` `adapter:` key.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

// ── Release adapter interface ───────────────────────────────────

/// VCS-agnostic interface for release operations.
///
/// Implement this trait to support non-git version control systems.
/// The `GitAdapter` is the default; `PerforceAdapter` and `SvnAdapter`
/// return `Err` for all operations (override via `.ta/release.yaml`).
#[allow(dead_code)]
pub trait ReleaseAdapter: Send + Sync {
    /// Bump the version in VCS-tracked files (Cargo.toml, CLAUDE.md, etc.).
    fn bump_version(&self, root: &Path, new_version: &str) -> anyhow::Result<()>;

    /// Commit staged changes and create a release tag.
    fn commit_and_tag(&self, root: &Path, message: &str, tag: &str) -> anyhow::Result<()>;

    /// Push the current branch and tags to `remote`.
    fn push(&self, root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()>;

    /// Create a release draft on the hosting platform.
    /// `notes` is the Markdown body from `.release-draft.md`.
    fn create_release_draft(&self, root: &Path, tag: &str, notes: &str) -> anyhow::Result<()>;

    /// Publish the draft release, making it publicly visible.
    fn publish_release(&self, root: &Path, tag: &str) -> anyhow::Result<()>;

    /// Dispatch a CI workflow (e.g., GitHub Actions release.yml) for `tag`.
    fn dispatch_workflow(&self, root: &Path, tag: &str, prerelease: bool) -> anyhow::Result<()>;
}

/// Default release adapter backed by standard git + gh CLI.
#[allow(dead_code)]
pub struct GitAdapter;

impl ReleaseAdapter for GitAdapter {
    fn bump_version(&self, root: &Path, new_version: &str) -> anyhow::Result<()> {
        // Delegates to the inline Rust version bumper used by release.rs.
        // Using a direct file-edit approach so no subprocess is needed here.
        let cargo_path = root.join("Cargo.toml");
        if cargo_path.exists() {
            let content = std::fs::read_to_string(&cargo_path)?;
            // Very simple bump: replace version in [workspace.package].
            let updated = content
                .lines()
                .scan(false, |in_ws, line| {
                    let t = line.trim();
                    if t == "[workspace.package]" {
                        *in_ws = true;
                    } else if t.starts_with('[') {
                        *in_ws = false;
                    }
                    if *in_ws && t.starts_with("version") && t.contains('=') {
                        Some(format!("version = \"{}\"", new_version))
                    } else {
                        Some(line.to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&cargo_path, updated)?;
        }
        Ok(())
    }

    fn commit_and_tag(&self, root: &Path, message: &str, tag: &str) -> anyhow::Result<()> {
        git_add(root, "-A")?;
        git_commit(root, message)?;
        let status = Command::new("git")
            .args(["tag", "-a", tag, "-m", &format!("Release {}", tag)])
            .current_dir(root)
            .status()?;
        if !status.success() {
            anyhow::bail!(
                "git tag {} failed — tag may already exist. Check with: git tag -l",
                tag
            );
        }
        Ok(())
    }

    fn push(&self, root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()> {
        git_push(root, remote, args)
    }

    fn create_release_draft(&self, root: &Path, tag: &str, notes: &str) -> anyhow::Result<()> {
        let notes_file = root.join(".release-draft.md");
        std::fs::write(&notes_file, notes)?;
        let status = Command::new("gh")
            .args([
                "release",
                "create",
                tag,
                "--draft",
                "--notes-file",
                ".release-draft.md",
            ])
            .current_dir(root)
            .status()
            .map_err(|e| anyhow::anyhow!("gh not found: {}. Install: https://cli.github.com", e))?;
        if !status.success() {
            anyhow::bail!(
                "gh release create --draft {} failed. Check: gh auth status",
                tag
            );
        }
        Ok(())
    }

    fn publish_release(&self, root: &Path, tag: &str) -> anyhow::Result<()> {
        let status = Command::new("gh")
            .args(["release", "edit", tag, "--draft=false"])
            .current_dir(root)
            .status()
            .map_err(|e| anyhow::anyhow!("gh not found: {}", e))?;
        if !status.success() {
            anyhow::bail!(
                "gh release edit {} --draft=false failed. \
                 The release may not exist yet — run create_release_draft first.",
                tag
            );
        }
        Ok(())
    }

    fn dispatch_workflow(&self, root: &Path, tag: &str, prerelease: bool) -> anyhow::Result<()> {
        let status = Command::new("gh")
            .args([
                "workflow",
                "run",
                "release.yml",
                "--field",
                &format!("tag={}", tag),
                "--field",
                &format!("prerelease={}", prerelease),
            ])
            .current_dir(root)
            .status()
            .map_err(|e| anyhow::anyhow!("gh not found: {}", e))?;
        if !status.success() {
            anyhow::bail!(
                "gh workflow run release.yml failed for tag {}. \
                 Ensure the workflow file exists and: gh auth status",
                tag
            );
        }
        Ok(())
    }
}

/// Perforce (P4) adapter stub — returns Err(Unsupported) for all operations.
///
/// Override via `.ta/release.yaml` `adapter: perforce` to use this adapter.
/// Actionable next steps are included in each error message.
#[allow(dead_code)]
pub struct PerforceAdapter;

impl ReleaseAdapter for PerforceAdapter {
    fn bump_version(&self, _root: &Path, _new_version: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: bump_version is not implemented.\n\
             Configure an alternative version bump mechanism or use a shell step:\n\
             run: p4 edit Cargo.toml && sed -i 's/version = .*/version = \"${{VERSION}}\"/' Cargo.toml"
        )
    }

    fn commit_and_tag(&self, _root: &Path, _message: &str, _tag: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: commit_and_tag is not implemented.\n\
             Use a shell step with p4 submit and p4 tag:\n\
             run: p4 submit -d \"Release ${{TAG}}\" && p4 tag -l ${{TAG}} //depot/..."
        )
    }

    fn push(&self, _root: &Path, _remote: &str, _args: &[&str]) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: push is not applicable — Perforce uses submit, not push.\n\
             Changes committed via commit_and_tag are already in the depot."
        )
    }

    fn create_release_draft(&self, _root: &Path, _tag: &str, _notes: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: create_release_draft is not implemented.\n\
             Consider using a shell step to create a release in your issue tracker."
        )
    }

    fn publish_release(&self, _root: &Path, _tag: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: publish_release is not implemented.\n\
             Publish the release manually in your issue tracker or CI system."
        )
    }

    fn dispatch_workflow(&self, _root: &Path, _tag: &str, _prerelease: bool) -> anyhow::Result<()> {
        anyhow::bail!(
            "Perforce adapter: dispatch_workflow is not implemented.\n\
             Trigger your CI system manually or use a shell step:\n\
             run: curl -X POST <your-ci-trigger-url>"
        )
    }
}

/// SVN adapter stub — returns Err(Unsupported) for all operations.
///
/// Override via `.ta/release.yaml` `adapter: svn` to use this adapter.
#[allow(dead_code)]
pub struct SvnAdapter;

impl ReleaseAdapter for SvnAdapter {
    fn bump_version(&self, _root: &Path, _new_version: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: bump_version is not implemented.\n\
             Use a shell step: svn propset svn:externals ... or edit Cargo.toml manually."
        )
    }

    fn commit_and_tag(&self, _root: &Path, _message: &str, _tag: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: commit_and_tag is not implemented.\n\
             Use a shell step with svn commit and svn copy for branching/tagging:\n\
             run: svn commit -m \"Release ${{TAG}}\" && svn copy . ^/tags/${{TAG}} -m \"Tag ${{TAG}}\""
        )
    }

    fn push(&self, _root: &Path, _remote: &str, _args: &[&str]) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: push is not applicable — SVN commits are immediate.\n\
             Changes committed via commit_and_tag are already in the repository."
        )
    }

    fn create_release_draft(&self, _root: &Path, _tag: &str, _notes: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: create_release_draft is not implemented.\n\
             Create the release draft manually in your hosting platform."
        )
    }

    fn publish_release(&self, _root: &Path, _tag: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: publish_release is not implemented.\n\
             Publish the release manually in your hosting platform."
        )
    }

    fn dispatch_workflow(&self, _root: &Path, _tag: &str, _prerelease: bool) -> anyhow::Result<()> {
        anyhow::bail!(
            "SVN adapter: dispatch_workflow is not implemented.\n\
             Trigger your CI system manually or use a shell step."
        )
    }
}

/// External-process `ReleaseAdapter` (§2.2 Plugin category), discovered via
/// `.ta/plugins/release/<name>/plugin.toml`. Lets a community member ship a
/// custom release adapter (e.g. `AppStoreReleaseAdapter`, `ItchIoReleaseAdapter`)
/// without a TA core PR, per v0.17.0.12.14 item 5.
#[allow(dead_code)]
pub struct ExternalReleaseAdapter {
    name: String,
    command: String,
    args: Vec<String>,
    timeout: std::time::Duration,
}

#[derive(serde::Serialize)]
struct BumpVersionParams<'a> {
    root: &'a str,
    new_version: &'a str,
}
#[derive(serde::Serialize)]
struct CommitAndTagParams<'a> {
    root: &'a str,
    message: &'a str,
    tag: &'a str,
}
#[derive(serde::Serialize)]
struct PushParams<'a> {
    root: &'a str,
    remote: &'a str,
    args: &'a [&'a str],
}
#[derive(serde::Serialize)]
struct CreateReleaseDraftParams<'a> {
    root: &'a str,
    tag: &'a str,
    notes: &'a str,
}
#[derive(serde::Serialize)]
struct TagOnlyParams<'a> {
    root: &'a str,
    tag: &'a str,
}
#[derive(serde::Serialize)]
struct DispatchWorkflowParams<'a> {
    root: &'a str,
    tag: &'a str,
    prerelease: bool,
}

#[allow(dead_code)]
impl ExternalReleaseAdapter {
    /// Resolve `name` via `.ta/plugins/release/<name>/plugin.toml` discovery.
    pub fn discover(name: &str, project_root: &Path) -> Option<Self> {
        let found = ta_plugin::find_plugin("release", name, project_root)?;
        Some(Self {
            name: found.manifest.name.clone(),
            command: found
                .plugin_dir
                .join(&found.manifest.command)
                .to_string_lossy()
                .to_string(),
            args: found.manifest.args.clone(),
            timeout: found.manifest.timeout(60),
        })
    }

    /// The adapter name this instance was discovered as (for tests/diagnostics).
    pub fn adapter_name(&self) -> &str {
        &self.name
    }

    fn call<Req: serde::Serialize>(
        &self,
        method: &str,
        root: &Path,
        params: &Req,
    ) -> anyhow::Result<()> {
        let request = ta_plugin::PluginRequest::new(method, serde_json::to_value(params)?);
        let response: ta_plugin::PluginResponse = ta_plugin::transport::call_json(
            &self.name,
            method,
            &self.command,
            &self.args,
            root,
            &request,
            self.timeout,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "release plugin '{}' method '{method}' failed: {e}",
                self.name
            )
        })?;
        if !response.ok {
            anyhow::bail!(
                "release plugin '{}' method '{method}' failed: {}",
                self.name,
                response
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
        Ok(())
    }
}

impl ReleaseAdapter for ExternalReleaseAdapter {
    fn bump_version(&self, root: &Path, new_version: &str) -> anyhow::Result<()> {
        self.call(
            "bump_version",
            root,
            &BumpVersionParams {
                root: &root.to_string_lossy(),
                new_version,
            },
        )
    }

    fn commit_and_tag(&self, root: &Path, message: &str, tag: &str) -> anyhow::Result<()> {
        self.call(
            "commit_and_tag",
            root,
            &CommitAndTagParams {
                root: &root.to_string_lossy(),
                message,
                tag,
            },
        )
    }

    fn push(&self, root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()> {
        self.call(
            "push",
            root,
            &PushParams {
                root: &root.to_string_lossy(),
                remote,
                args,
            },
        )
    }

    fn create_release_draft(&self, root: &Path, tag: &str, notes: &str) -> anyhow::Result<()> {
        self.call(
            "create_release_draft",
            root,
            &CreateReleaseDraftParams {
                root: &root.to_string_lossy(),
                tag,
                notes,
            },
        )
    }

    fn publish_release(&self, root: &Path, tag: &str) -> anyhow::Result<()> {
        self.call(
            "publish_release",
            root,
            &TagOnlyParams {
                root: &root.to_string_lossy(),
                tag,
            },
        )
    }

    fn dispatch_workflow(&self, root: &Path, tag: &str, prerelease: bool) -> anyhow::Result<()> {
        self.call(
            "dispatch_workflow",
            root,
            &DispatchWorkflowParams {
                root: &root.to_string_lossy(),
                tag,
                prerelease,
            },
        )
    }
}

/// Resolve a `ReleaseAdapter` from an adapter name string.
///
/// Used by the pipeline to select the right adapter based on `.ta/release.yaml`
/// `adapter:` key. Checks `.ta/plugins/release/<name>/plugin.toml` (Plugin
/// category, community-contributable) before falling back to the built-in
/// git/perforce/svn adapters. The git adapter is the default.
#[allow(dead_code)]
pub fn resolve_adapter(adapter_name: Option<&str>, project_root: &Path) -> Box<dyn ReleaseAdapter> {
    if let Some(name) = adapter_name {
        if let Some(external) = ExternalReleaseAdapter::discover(name, project_root) {
            return Box::new(external);
        }
        match name {
            "perforce" | "p4" => return Box::new(PerforceAdapter),
            "svn" | "subversion" => return Box::new(SvnAdapter),
            _ => {}
        }
    }
    Box::new(GitAdapter)
}

/// Check whether the working tree has any uncommitted changes (staged or unstaged).
pub fn git_is_dirty(root: &Path) -> bool {
    let unstaged = Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(root)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(root)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    unstaged || staged
}

/// Return all existing tag names in the repository.
pub fn git_tags(root: &Path) -> HashSet<String> {
    Command::new("git")
        .args(["tag", "-l"])
        .current_dir(root)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Return the current HEAD commit SHA, or None if unavailable.
pub fn git_head_sha(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Validate that a tag exists in the repository.
/// Returns the tag name on success, or an error with a user-actionable message.
pub fn git_verify_tag(root: &Path, tag: &str) -> anyhow::Result<String> {
    let check = Command::new("git")
        .args(["rev-parse", "--verify", tag])
        .current_dir(root)
        .output();
    match check {
        Ok(out) if out.status.success() => Ok(tag.to_string()),
        _ => anyhow::bail!(
            "Tag '{}' not found in this repository.\nRun `git tag` to list available tags.",
            tag
        ),
    }
}

/// Collect commit subjects since the given tag (or all commits if tag is None).
/// Returns (commit_subjects_joined, last_tag_used).
pub fn git_log_since_tag(
    root: &Path,
    from_tag: Option<&str>,
) -> anyhow::Result<(String, Option<String>)> {
    let last_tag = if let Some(tag) = from_tag {
        git_verify_tag(root, tag)?;
        Some(tag.to_string())
    } else {
        // Try git describe for the most recent tag.
        let out = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .current_dir(root)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            _ => None,
        }
    };

    let log_args: Vec<String> = match &last_tag {
        Some(tag) => vec![
            "log".to_string(),
            format!("{}..HEAD", tag),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
        None => vec![
            "log".to_string(),
            "--pretty=format:%s".to_string(),
            "--no-merges".to_string(),
        ],
    };

    let output = Command::new("git")
        .args(&log_args)
        .current_dir(root)
        .output()?;

    let commits = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((commits, last_tag))
}

/// Stage a path with `git add`.
pub fn git_add(root: &Path, path: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["add", path])
        .current_dir(root)
        .status()?;
    if !status.success() {
        tracing::warn!("git add {} returned non-zero exit code", path);
    }
    Ok(())
}

/// Commit with the given message.
pub fn git_commit(root: &Path, message: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!("git commit failed — check `git status` for details");
    }
    Ok(())
}

/// Amend the last commit.
#[allow(dead_code)]
pub fn git_commit_amend(root: &Path, message: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["commit", "--amend", "--no-edit", "-m", message])
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!("git commit --amend failed — check `git status` for details");
    }
    Ok(())
}

/// Push the current branch to the given remote.
pub fn git_push(root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd_args = vec!["push", remote];
    cmd_args.extend_from_slice(args);
    let status = Command::new("git")
        .args(&cmd_args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "git push {} failed — check your remote access and try again",
            remote
        );
    }
    Ok(())
}

/// Get the URL of a remote (e.g., "origin").
pub fn git_remote_url(root: &Path, remote: &str) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("Cannot run git remote get-url {}: {}", remote, e))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Get the output of `git log` with a custom format string.
#[allow(dead_code)]
pub fn git_log_format(root: &Path, format: &str, range: Option<&str>) -> anyhow::Result<String> {
    let format_arg = format!("--pretty=format:{}", format);
    let mut args = vec!["log", &format_arg];
    if let Some(r) = range {
        args.push(r);
    }
    let output = Command::new("git").args(&args).current_dir(root).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git log failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_adapter_falls_back_to_git_without_plugin() {
        let dir = tempfile::tempdir().unwrap();
        // No adapter name, and no .ta/plugins/release/ manifests — must fall
        // back to the default GitAdapter rather than panicking or erroring.
        let _adapter = resolve_adapter(None, dir.path());
    }

    #[test]
    fn resolve_adapter_falls_back_to_perforce_without_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let _adapter = resolve_adapter(Some("perforce"), dir.path());
    }

    #[test]
    fn resolve_adapter_discovers_external_release_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/release/custom");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "name = \"custom\"\ntype = \"release\"\ncommand = \"custom-release-bin\"\n",
        )
        .unwrap();

        let adapter = ExternalReleaseAdapter::discover("custom", dir.path()).unwrap();
        assert_eq!(adapter.adapter_name(), "custom");

        // resolve_adapter must pick the external plugin path over perforce/svn/git.
        let _adapter = resolve_adapter(Some("custom"), dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn external_release_adapter_bump_version_via_mock_plugin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/release/mockrelease");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let script_path = plugin_dir.join("mockrelease-plugin.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nread -r line\necho '{\"ok\":true,\"result\":{}}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                "name = \"mockrelease\"\ntype = \"release\"\ncommand = \"{}\"\n",
                script_path.display()
            ),
        )
        .unwrap();

        let adapter = ExternalReleaseAdapter::discover("mockrelease", dir.path()).unwrap();
        adapter.bump_version(dir.path(), "1.2.3").unwrap();
    }
}
