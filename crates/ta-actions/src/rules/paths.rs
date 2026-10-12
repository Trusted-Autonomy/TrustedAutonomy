// rules/paths.rs: path normalization for the `apply` rule (v0.17.12.4).
//
// An unattended file apply is allowed only when every changed path matches a
// literal, normalized prefix in the owner's rules. This module decides what
// "normalized" means and which places no rule may ever reach.
//
// Spellings are folded by `ta_workspace::path_safety` (the single source of
// truth for infrastructure paths): case, trailing dots and spaces, 8.3 names,
// NTFS stream syntax, `\` versus `/`, ignorable code points. On top of that
// this module:
//
// * refuses every non-ASCII component instead of normalizing it. Unicode NFC
//   or NFD spellings (macOS) and look-alike folds (the Kelvin sign lowercases
//   to `k`) could otherwise make a path that is not in the allowed prefix match
//   it. A path that is refused can never be auto-applied, so nothing is lost
//   except the convenience of unattended apply into non-ASCII directory names;
// * adds `.claude`, `.mcp.json`, `.github`, `.husky` and `.githooks` to the
//   protected set (the first two are required by the design; the rest are
//   places where a file write turns into code execution on the next commit or
//   CI run);
// * protects the files agent prompts are built from (`CLAUDE.md`, `AGENTS.md`
//   and the like), so reading auto-applied content never raises anyone's
//   permissions (second-order injection).

use std::path::Path;

use ta_workspace::path_safety;

/// Directories and files an `apply` rule can never reach, beyond the
/// infrastructure set in `path_safety` (`.ta`, `.git`, `.svn`, `.hg`, ...).
pub const EXTRA_PROTECTED: &[&str] = &[".claude", ".mcp.json", ".github", ".husky", ".githooks"];

/// File names (folded) that agent prompts are built from.
pub const PROMPT_SOURCE_FILES: &[&str] = &[
    "claude.md",
    "agents.md",
    "gemini.md",
    ".cursorrules",
    "copilot-instructions.md",
];

/// A path that passed normalization: folded components, ASCII only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormPath {
    pub components: Vec<String>,
}

impl NormPath {
    pub fn display(&self) -> String {
        self.components.join("/")
    }
}

/// Why a path is not eligible for an unattended apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    /// Traversal, absolute path, drive prefix, stream syntax or TA/VCS
    /// infrastructure (from `path_safety`). Refused for every caller.
    Unsafe(String),
    /// Contains a non-ASCII character.
    NonAscii,
    /// Names a protected place (`.claude`, `.mcp.json`, ...).
    Protected {
        component: String,
    },
    /// A file that agent prompts are built from.
    PromptSource {
        component: String,
    },
    Empty,
}

impl PathProblem {
    pub fn describe(&self) -> String {
        match self {
            PathProblem::Unsafe(why) => why.clone(),
            PathProblem::NonAscii => {
                "the path has non-ASCII characters; TA does not normalize Unicode spellings, so \
                 such a path is never applied without a person"
                    .into()
            }
            PathProblem::Protected { component } => format!(
                "'{component}' is a protected place (.claude, .mcp.json, .github, .husky, \
                 .githooks); no rule can allow an unattended apply into it"
            ),
            PathProblem::PromptSource { component } => format!(
                "'{component}' is a file agent prompts are built from; auto-applied content is \
                 untrusted and may not land there without a person"
            ),
            PathProblem::Empty => "the path is empty".into(),
        }
    }

    /// Refused for attended applies too (never a legitimate draft target).
    pub fn is_unsafe(&self) -> bool {
        matches!(self, PathProblem::Unsafe(_) | PathProblem::Empty)
    }
}

fn split(raw: &str) -> Vec<&str> {
    raw.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect()
}

/// Normalize a project-relative path for rule matching.
///
/// `extra_prompt_sources` are owner-declared literal prefixes (already
/// normalized) that count as prompt sources, for example `docs/wiki`.
pub fn normalize(raw: &str, extra_prompt_sources: &[Vec<String>]) -> Result<NormPath, PathProblem> {
    if let Err(issue) = path_safety::check_relative_path(raw) {
        return Err(match issue {
            path_safety::PathSafetyIssue::Empty => PathProblem::Empty,
            other => PathProblem::Unsafe(other.describe()),
        });
    }
    if !raw.is_ascii() {
        return Err(PathProblem::NonAscii);
    }
    let components: Vec<String> = split(raw)
        .into_iter()
        .map(path_safety::fold_component)
        .collect();
    for (i, folded) in components.iter().enumerate() {
        for protected in EXTRA_PROTECTED {
            if path_safety::component_aliases(folded, protected) {
                return Err(PathProblem::Protected {
                    component: folded.clone(),
                });
            }
        }
        if i + 1 == components.len() && PROMPT_SOURCE_FILES.contains(&folded.as_str()) {
            return Err(PathProblem::PromptSource {
                component: folded.clone(),
            });
        }
    }
    for prefix in extra_prompt_sources {
        if components.starts_with(prefix) {
            return Err(PathProblem::PromptSource {
                component: prefix.join("/"),
            });
        }
    }
    Ok(NormPath { components })
}

/// Parse a rule's literal prefix (`docs/research`, `docs/research/`,
/// `docs/research/**`) into folded components. Anything that could widen or
/// confuse the match is an error with a fix.
pub fn parse_rule_prefix(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    let literal = trimmed
        .strip_suffix("/**")
        .or_else(|| trimmed.strip_suffix("\\**"))
        .unwrap_or(trimmed);
    if literal.contains(['*', '?', '[', ']', '{', '}']) {
        return Err(format!(
            "'{raw}' is a glob. Paths are literal directory prefixes, optionally ending in /**. \
             Fix: name the directory, for example docs/research/**"
        ));
    }
    if literal.trim_matches(['/', '\\', '.']).is_empty() {
        return Err(format!(
            "'{raw}' names the whole project. Fix: name a directory, for example docs/research/**"
        ));
    }
    match normalize(literal, &[]) {
        Ok(n) => Ok(n.components),
        Err(PathProblem::PromptSource { component }) => Err(format!(
            "'{raw}' is a file agent prompts are built from ('{component}'). Fix: remove it; \
             auto-applied content is untrusted and may not land in prompt sources"
        )),
        Err(p) => Err(format!(
            "'{raw}' can never be auto-applied: {}. Fix: remove it from the rule",
            p.describe()
        )),
    }
}

/// A link (symlink, junction or hard link) or an escape found on a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkProblem {
    pub path: String,
    pub why: String,
}

/// Refuse links of any kind under `base` along `rel`.
///
/// Every existing ancestor is checked with `symlink_metadata` (which does not
/// follow links), the deepest existing ancestor must canonicalize to a place
/// inside `base`, and an existing file with more than one hard link is refused
/// on Unix. Windows has no stable hard-link count in `std`; there junctions
/// and symlinks are caught by the canonicalization check and a hard link to a
/// file outside the tree is covered by the allowed-prefix rule plus the
/// protected-place list. Call it for the project root and for the staging
/// directory the content is copied from.
pub fn check_no_links(base: &Path, rel: &str) -> Result<(), LinkProblem> {
    let canonical_base = base.canonicalize().map_err(|e| LinkProblem {
        path: base.display().to_string(),
        why: format!("could not resolve the base directory: {e}"),
    })?;
    let mut current = base.to_path_buf();
    let mut deepest_existing = base.to_path_buf();
    for component in split(rel) {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(LinkProblem {
                        path: current.display().to_string(),
                        why: "is a symbolic link; links are never auto-applied".into(),
                    });
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if meta.is_file() && meta.nlink() > 1 {
                        return Err(LinkProblem {
                            path: current.display().to_string(),
                            why: format!(
                                "has {} hard links; links are never auto-applied",
                                meta.nlink()
                            ),
                        });
                    }
                }
                deepest_existing = current.clone();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                return Err(LinkProblem {
                    path: current.display().to_string(),
                    why: format!("could not be inspected ({e}); refusing rather than guessing"),
                })
            }
        }
    }
    let resolved = deepest_existing.canonicalize().map_err(|e| LinkProblem {
        path: deepest_existing.display().to_string(),
        why: format!("could not be resolved: {e}"),
    })?;
    if !resolved.starts_with(&canonical_base) {
        return Err(LinkProblem {
            path: deepest_existing.display().to_string(),
            why: format!(
                "resolves to {}, outside {}; links are never auto-applied",
                resolved.display(),
                canonical_base.display()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> Vec<String> {
        normalize(raw, &[]).unwrap().components
    }

    #[test]
    fn folds_case_separators_and_dots() {
        assert_eq!(
            ok("Docs\\Research/./a.md"),
            vec!["docs", "research", "a.md"]
        );
        assert_eq!(ok("docs./research /x"), vec!["docs", "research", "x"]);
    }

    #[test]
    fn traversal_absolute_and_infrastructure_are_unsafe() {
        for raw in [
            "../x",
            "docs/../../x",
            "docs/.. /x",
            "/etc/passwd",
            "C:\\x",
            ".ta/rules.toml",
            ".TA/rules.toml",
            ".ta./rules.toml",
            "docs/.git/hooks/pre-commit",
            "GIT~1/config",
            ".git::$INDEX_ALLOCATION/x",
        ] {
            assert!(
                matches!(normalize(raw, &[]), Err(PathProblem::Unsafe(_))),
                "{raw} must be unsafe, got {:?}",
                normalize(raw, &[])
            );
        }
    }

    #[test]
    fn claude_mcp_and_hook_places_are_protected_in_any_spelling() {
        for raw in [
            ".claude/settings.json",
            ".CLAUDE/x",
            ".claude./x",
            "a/.claude/x",
            ".mcp.json",
            ".MCP.JSON",
            ".mcp.json./",
            ".github/workflows/ci.yml",
            ".husky/pre-commit",
        ] {
            assert!(
                matches!(normalize(raw, &[]), Err(PathProblem::Protected { .. })),
                "{raw}: {:?}",
                normalize(raw, &[])
            );
        }
    }

    #[test]
    fn prompt_sources_are_protected_including_owner_declared_ones() {
        assert!(matches!(
            normalize("docs/CLAUDE.md", &[]),
            Err(PathProblem::PromptSource { .. })
        ));
        assert!(matches!(
            normalize("AGENTS.MD", &[]),
            Err(PathProblem::PromptSource { .. })
        ));
        let wiki = vec![vec!["docs".to_string(), "wiki".to_string()]];
        assert!(matches!(
            normalize("Docs/Wiki/page.md", &wiki),
            Err(PathProblem::PromptSource { .. })
        ));
        assert!(normalize("docs/notes/page.md", &wiki).is_ok());
    }

    #[test]
    fn non_ascii_is_refused_not_normalized() {
        // NFC and NFD spellings of the same name, and a Kelvin-sign look-alike.
        for raw in ["docs/caf\u{e9}/x", "docs/cafe\u{301}/x", "\u{212A}b/x"] {
            assert_eq!(normalize(raw, &[]), Err(PathProblem::NonAscii), "{raw}");
        }
    }

    #[test]
    fn rule_prefixes_must_be_literal_and_reachable() {
        assert_eq!(
            parse_rule_prefix("docs/research/**").unwrap(),
            vec!["docs", "research"]
        );
        assert_eq!(
            parse_rule_prefix("Docs\\Research").unwrap(),
            vec!["docs", "research"]
        );
        for bad in [
            "docs/*/x",
            "**",
            "/",
            ".",
            "",
            "docs/[ab]",
            "../x",
            ".ta/x",
            ".git/hooks",
            ".claude/**",
            ".mcp.json",
            "CLAUDE.md",
            "caf\u{e9}",
        ] {
            assert!(parse_rule_prefix(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn regular_paths_have_no_link_problem() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs/research")).unwrap();
        std::fs::write(tmp.path().join("docs/research/a.md"), "x").unwrap();
        assert!(check_no_links(tmp.path(), "docs/research/a.md").is_ok());
        assert!(check_no_links(tmp.path(), "docs/research/new.md").is_ok());
        assert!(check_no_links(tmp.path(), "brand/new/dir/x.md").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_directory_and_escape_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("docs/link.md"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("docs/dir")).unwrap();
        std::os::unix::fs::symlink(root.join(".git"), root.join("docs/gitlink")).unwrap();
        for rel in ["docs/link.md", "docs/dir/new.md", "docs/gitlink/hooks/x"] {
            let err = check_no_links(root, rel).unwrap_err();
            assert!(err.why.contains("symbolic link"), "{rel}: {err:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/a.md"), "x").unwrap();
        std::fs::hard_link(root.join("docs/a.md"), root.join("docs/b.md")).unwrap();
        let err = check_no_links(root, "docs/b.md").unwrap_err();
        assert!(err.why.contains("hard links"), "{err:?}");
    }
}
