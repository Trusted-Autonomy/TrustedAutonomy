// vcs_detect.rs — The one VCS detector (v0.17.12.5).
//
// Shared by `ta init`, `ta setup vcs`, `ta doctor`, and (later) the VT
// installer and the add-project wizard. It answers two questions at once:
// which VCS is this project under, and WHY (the evidence list), so the
// installer report and the wizard can show the reasoning instead of only the
// verdict.
//
// Kinds: git, svn, perforce, none, plugin. `plugin` means a VCS adapter plugin
// declared detection markers (`[detect] markers = [...]` in its `plugin.toml`)
// and one of them is present. Plugin markers are passed in by the caller
// (ta-submit owns manifest discovery and cannot be a dependency of this crate).
//
// Selection rule: every marker found is recorded. The chosen kind is the best
// by (locality, kind order). Locality: tier 0 = marker in the project root,
// tier 1 = marker in an enclosing directory, tier 2 = environment variable.
// Kind order inside a tier: git, svn, perforce, plugin. Evidence that
// supports the chosen kind goes in `evidence`; anything found that lost goes
// in `ignored_evidence`, so a mixed tree is visible rather than silently
// resolved.
//
// All paths in the output use forward slashes on every platform and are
// relative to the project root when they sit inside it, so the JSON is stable
// and golden-testable.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::partitioning::VcsBackend;

/// Version of the `VcsDetection` / `VcsSetupReport` JSON shape. Bump on any
/// non-additive change (see `schema/vcs_detection.schema.json`).
pub const VCS_DETECTION_SCHEMA_VERSION: u32 = 1;

/// Kind of version control the project is under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VcsKind {
    Git,
    Svn,
    Perforce,
    /// No VCS marker found.
    None,
    /// A VCS adapter plugin claimed the project through its declared markers.
    Plugin,
}

impl VcsKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Svn => "svn",
            Self::Perforce => "perforce",
            Self::None => "none",
            Self::Plugin => "plugin",
        }
    }
}

/// One marker or file that supports a VCS kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsEvidence {
    /// The kind this marker points to.
    pub kind: VcsKind,
    /// Plugin name when `kind` is `plugin`, otherwise null.
    pub plugin: Option<String>,
    /// What was looked for, e.g. `.git`, `.p4config`, `P4PORT`.
    pub marker: String,
    /// Where it was found: project-relative with forward slashes, an absolute
    /// forward-slash path for an enclosing directory, or `env:<NAME>`.
    pub path: String,
    /// Human-readable reason this marker counts.
    pub reason: String,
}

/// Result of VCS detection: the chosen kind and the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDetection {
    /// JSON shape version (`VCS_DETECTION_SCHEMA_VERSION`).
    pub schema_version: u32,
    /// The chosen kind.
    pub kind: VcsKind,
    /// Plugin name when `kind` is `plugin`, otherwise null.
    pub plugin: Option<String>,
    /// Markers and files that led to the chosen kind (empty for `none`).
    pub evidence: Vec<VcsEvidence>,
    /// Markers found for other kinds that lost to the chosen one.
    pub ignored_evidence: Vec<VcsEvidence>,
    /// Every marker that was probed, so `none` can say what was checked.
    pub checked: Vec<String>,
}

impl VcsDetection {
    /// Map to the backend enum used by ignore-file generation.
    pub fn backend(&self) -> VcsBackend {
        match self.kind {
            VcsKind::Git => VcsBackend::Git,
            VcsKind::Svn => VcsBackend::Svn,
            VcsKind::Perforce => VcsBackend::Perforce,
            VcsKind::None => VcsBackend::None,
            VcsKind::Plugin => VcsBackend::Plugin(self.plugin.clone().unwrap_or_default()),
        }
    }

    /// One-line summary for CLI output, e.g. `git (.git)`.
    pub fn summary(&self) -> String {
        match self.evidence.first() {
            Some(e) => {
                let name = match &self.plugin {
                    Some(p) => format!("plugin:{p}"),
                    None => self.kind.as_str().to_string(),
                };
                format!("{name} ({})", e.marker)
            }
            None => "none (no VCS marker found)".to_string(),
        }
    }
}

/// Detection markers declared by one VCS adapter plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMarkers {
    pub name: String,
    /// Files or directories, relative to the project root.
    pub markers: Vec<String>,
}

/// Environment and process seams so detection is testable without touching
/// the real environment or spawning `git`.
pub struct DetectContext<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Runs `git rev-parse --git-dir` in the directory; `Some(output)` on success.
    pub git_dir: &'a dyn Fn(&Path) -> Option<String>,
}

/// Names a plugin cannot take: they are built-in kinds.
const BUILTIN_NAMES: &[&str] = &["git", "svn", "perforce", "none"];

/// Detect the VCS for `project_root` using the real environment and `git`.
pub fn detect_vcs(project_root: &Path, plugins: &[PluginMarkers]) -> VcsDetection {
    let env = |name: &str| std::env::var(name).ok();
    let git_dir = real_git_dir;
    detect_vcs_with(
        project_root,
        plugins,
        &DetectContext {
            env: &env,
            git_dir: &git_dir,
        },
    )
}

fn real_git_dir(dir: &Path) -> Option<String> {
    // Clear TA agent VCS isolation env vars (v0.13.17.3) so detection reflects
    // the project's own repository, not the staging directory.
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Forward-slash path, relative to `root` when inside it.
fn display_path(root: &Path, p: &Path) -> String {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        ".".to_string()
    } else {
        s
    }
}

struct Candidate {
    tier: u8,
    order: u8,
    evidence: VcsEvidence,
}

/// Detect with injected seams. See the module comment for the selection rule.
pub fn detect_vcs_with(
    project_root: &Path,
    plugins: &[PluginMarkers],
    ctx: &DetectContext<'_>,
) -> VcsDetection {
    let mut found: Vec<Candidate> = Vec::new();
    let mut checked: Vec<String> = Vec::new();
    let ev = |kind: VcsKind, plugin: Option<&str>, marker: &str, path: String, reason: String| {
        VcsEvidence {
            kind,
            plugin: plugin.map(str::to_string),
            marker: marker.to_string(),
            path,
            reason,
        }
    };

    // Tier 0: markers in the project root.
    checked.push(".git".into());
    let dot_git = project_root.join(".git");
    if dot_git.exists() {
        let reason = if dot_git.is_dir() {
            ".git directory present in the project root"
        } else {
            ".git file present in the project root (git worktree or submodule pointer)"
        };
        found.push(Candidate {
            tier: 0,
            order: 0,
            evidence: ev(
                VcsKind::Git,
                None,
                ".git",
                display_path(project_root, &dot_git),
                reason.into(),
            ),
        });
    }

    checked.push(".svn".into());
    let dot_svn = project_root.join(".svn");
    if dot_svn.exists() {
        found.push(Candidate {
            tier: 0,
            order: 1,
            evidence: ev(
                VcsKind::Svn,
                None,
                ".svn",
                display_path(project_root, &dot_svn),
                ".svn directory present in the project root (Subversion working copy)".into(),
            ),
        });
    }

    checked.push(".p4config".into());
    let p4config = project_root.join(".p4config");
    if p4config.exists() {
        found.push(Candidate {
            tier: 0,
            order: 2,
            evidence: ev(
                VcsKind::Perforce,
                None,
                ".p4config",
                display_path(project_root, &p4config),
                ".p4config present in the project root".into(),
            ),
        });
    }

    for plugin in plugins {
        if BUILTIN_NAMES.contains(&plugin.name.as_str()) {
            continue;
        }
        for marker in &plugin.markers {
            checked.push(format!("{marker} (plugin {})", plugin.name));
            let p = project_root.join(marker);
            if p.exists() {
                found.push(Candidate {
                    tier: 0,
                    order: 3,
                    evidence: ev(
                        VcsKind::Plugin,
                        Some(&plugin.name),
                        marker,
                        display_path(project_root, &p),
                        format!(
                            "marker declared by VCS adapter plugin '{}' is present",
                            plugin.name
                        ),
                    ),
                });
            }
        }
    }

    // Tier 1: markers in enclosing directories.
    let have_root_git = found
        .iter()
        .any(|c| c.evidence.kind == VcsKind::Git && c.tier == 0);
    if !have_root_git {
        checked.push("git rev-parse --git-dir".into());
        if let Some(out) = (ctx.git_dir)(project_root) {
            found.push(Candidate {
                tier: 1,
                order: 0,
                evidence: ev(
                    VcsKind::Git,
                    None,
                    "git rev-parse --git-dir",
                    out.replace('\\', "/"),
                    "the project sits inside a git work tree (git reports a git dir)".into(),
                ),
            });
        }
    }

    checked.push(".svn in an enclosing directory".into());
    let mut dir: Option<PathBuf> = project_root.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        let candidate = d.join(".svn");
        if candidate.exists() {
            found.push(Candidate {
                tier: 1,
                order: 1,
                evidence: ev(
                    VcsKind::Svn,
                    None,
                    ".svn",
                    display_path(project_root, &candidate),
                    ".svn directory found in an enclosing directory (Subversion working copy)"
                        .into(),
                ),
            });
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    checked.push(".p4config in an enclosing directory".into());
    let mut dir: Option<PathBuf> = project_root.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        let candidate = d.join(".p4config");
        if candidate.exists() {
            found.push(Candidate {
                tier: 1,
                order: 2,
                evidence: ev(
                    VcsKind::Perforce,
                    None,
                    ".p4config",
                    display_path(project_root, &candidate),
                    ".p4config found in an enclosing directory".into(),
                ),
            });
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    // Tier 2: environment. Values are never recorded, only that they are set.
    for var in ["P4PORT", "P4CLIENT"] {
        checked.push(format!("env {var}"));
        if (ctx.env)(var).is_some() {
            found.push(Candidate {
                tier: 2,
                order: 2,
                evidence: ev(
                    VcsKind::Perforce,
                    None,
                    var,
                    format!("env:{var}"),
                    format!("environment variable {var} is set (value not recorded)"),
                ),
            });
        }
    }

    // Choose: lowest (tier, order); first in discovery order breaks plugin ties.
    found.sort_by_key(|c| (c.tier, c.order));
    let (kind, plugin) = match found.first() {
        Some(c) => (c.evidence.kind, c.evidence.plugin.clone()),
        None => (VcsKind::None, None),
    };
    let (evidence, ignored_evidence): (Vec<_>, Vec<_>) = found
        .into_iter()
        .map(|c| c.evidence)
        .partition(|e| e.kind == kind && e.plugin == plugin);

    VcsDetection {
        schema_version: VCS_DETECTION_SCHEMA_VERSION,
        kind,
        plugin,
        evidence,
        ignored_evidence,
        checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn no_env(_: &str) -> Option<String> {
        None
    }
    fn no_git(_: &Path) -> Option<String> {
        None
    }

    fn detect_plain(root: &Path, plugins: &[PluginMarkers]) -> VcsDetection {
        detect_vcs_with(
            root,
            plugins,
            &DetectContext {
                env: &no_env,
                git_dir: &no_git,
            },
        )
    }

    #[test]
    fn empty_directory_is_none_and_says_what_was_checked() {
        let dir = tempfile::tempdir().unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::None);
        assert!(d.evidence.is_empty());
        assert!(d.checked.iter().any(|c| c == ".git"));
        assert!(d.summary().starts_with("none"));
    }

    #[test]
    fn git_dir_in_root_is_git_with_evidence() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::Git);
        assert_eq!(d.evidence.len(), 1);
        assert_eq!(d.evidence[0].path, ".git");
        assert!(d.evidence[0].reason.contains("directory"));
        assert_eq!(d.backend(), VcsBackend::Git);
    }

    #[test]
    fn git_file_worktree_pointer_is_git() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".git"), "gitdir: ../elsewhere\n").unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::Git);
        assert!(d.evidence[0].reason.contains("worktree"));
    }

    #[test]
    fn enclosing_git_repo_found_by_rev_parse_seam() {
        let dir = tempfile::tempdir().unwrap();
        let git = |_: &Path| Some("..\\.git".to_string());
        let d = detect_vcs_with(
            dir.path(),
            &[],
            &DetectContext {
                env: &no_env,
                git_dir: &git,
            },
        );
        assert_eq!(d.kind, VcsKind::Git);
        assert_eq!(d.evidence[0].marker, "git rev-parse --git-dir");
        assert_eq!(d.evidence[0].path, "../.git", "backslashes are normalized");
    }

    #[test]
    fn svn_in_root_and_in_enclosing_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".svn")).unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::Svn);
        assert_eq!(d.evidence[0].path, ".svn");

        // Subversion 1.7+ keeps .svn only at the working copy root.
        let sub = dir.path().join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        let d = detect_plain(&sub, &[]);
        assert_eq!(d.kind, VcsKind::Svn);
        assert!(d.evidence[0].reason.contains("enclosing"));
    }

    #[test]
    fn perforce_by_p4config_and_by_env() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".p4config"), "P4PORT=x:1666\r\n").unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::Perforce);
        assert_eq!(d.evidence[0].marker, ".p4config");

        let empty = tempfile::tempdir().unwrap();
        let env = |n: &str| (n == "P4PORT").then(|| "secret-host:1666".to_string());
        let d = detect_vcs_with(
            empty.path(),
            &[],
            &DetectContext {
                env: &env,
                git_dir: &no_git,
            },
        );
        assert_eq!(d.kind, VcsKind::Perforce);
        assert_eq!(d.evidence[0].path, "env:P4PORT");
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("secret-host"),
            "env values are never recorded"
        );
    }

    #[test]
    fn plugin_marker_selects_plugin_kind() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".hg")).unwrap();
        let plugins = [PluginMarkers {
            name: "mercurial".into(),
            markers: vec![".hg".into()],
        }];
        let d = detect_plain(dir.path(), &plugins);
        assert_eq!(d.kind, VcsKind::Plugin);
        assert_eq!(d.plugin.as_deref(), Some("mercurial"));
        assert_eq!(d.evidence[0].plugin.as_deref(), Some("mercurial"));
        assert_eq!(d.backend(), VcsBackend::Plugin("mercurial".into()));
        assert_eq!(d.summary(), "plugin:mercurial (.hg)");
    }

    #[test]
    fn plugin_cannot_claim_a_builtin_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".hg")).unwrap();
        let plugins = [PluginMarkers {
            name: "perforce".into(),
            markers: vec![".hg".into()],
        }];
        assert_eq!(detect_plain(dir.path(), &plugins).kind, VcsKind::None);
    }

    #[test]
    fn root_marker_beats_enclosing_git_and_loser_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".svn")).unwrap();
        let git = |_: &Path| Some("/outer/.git".to_string());
        let d = detect_vcs_with(
            dir.path(),
            &[],
            &DetectContext {
                env: &no_env,
                git_dir: &git,
            },
        );
        assert_eq!(d.kind, VcsKind::Svn);
        assert_eq!(d.ignored_evidence.len(), 1);
        assert_eq!(d.ignored_evidence[0].kind, VcsKind::Git);
    }

    #[test]
    fn git_beats_svn_beats_perforce_in_the_same_tier() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::create_dir(dir.path().join(".svn")).unwrap();
        fs::write(dir.path().join(".p4config"), "").unwrap();
        let d = detect_plain(dir.path(), &[]);
        assert_eq!(d.kind, VcsKind::Git);
        assert_eq!(d.ignored_evidence.len(), 2);
    }

    /// Golden test: the JSON shape is a documented contract (installer report,
    /// Wayfinder wizard). Changing it requires bumping the schema version.
    #[test]
    fn detection_json_golden() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let d = detect_plain(dir.path(), &[]);
        let mut v = serde_json::to_value(&d).unwrap();
        // `checked` is informational; pin only its presence and first entry.
        let checked = v.as_object_mut().unwrap().remove("checked").unwrap();
        assert_eq!(checked[0], ".git");
        let expected = serde_json::json!({
            "schema_version": 1,
            "kind": "git",
            "plugin": null,
            "evidence": [{
                "kind": "git",
                "plugin": null,
                "marker": ".git",
                "path": ".git",
                "reason": ".git directory present in the project root"
            }],
            "ignored_evidence": []
        });
        assert_eq!(v, expected);
    }
}
