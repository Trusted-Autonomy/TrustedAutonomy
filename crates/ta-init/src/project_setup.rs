// project_setup.rs — Shared baseline project onboarding logic (v0.17.9).
//
// This is the single source of truth for "make an empty directory into a
// TA-managed project": `.ta/` scaffolding, a starter PLAN.md, a minimal
// `.ta/workflow.toml`, a stack-agnostic starter CLAUDE.md, and a correct
// project-scoped `.mcp.json`. Both the CLI (`ta init`) and the daemon's
// `POST /api/project/init` (Studio's "New Project" form) call this function
// so the two entry points can never diverge again.
//
// Idempotent by construction: every write here first checks whether the
// target already exists and skips it if so. Re-running `init_project` on an
// already-initialized project is always a safe no-op — in particular it
// never touches a `CLAUDE.md` the user has since edited.

use std::path::Path;

/// Input to `init_project`.
pub struct ProjectInitOptions<'a> {
    /// Absolute path to the project root.
    pub project_root: &'a Path,
    /// Human-readable project name (usually the directory name).
    pub name: &'a str,
}

/// What `init_project` actually did, for CLI/API output and observability.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Project-root-relative paths created by this call.
    pub created: Vec<String>,
    /// Project-root-relative paths that already existed and were left untouched.
    pub skipped: Vec<String>,
    /// True if the project was already fully initialized before this call
    /// (i.e. `.ta/workflow.toml` already existed).
    pub already_initialized: bool,
}

/// Returns true if `project_root` already has baseline TA configuration.
///
/// This is the single check both `ta status` and `init_project`'s idempotency
/// path use to decide "has this project been onboarded?".
pub fn is_initialized(project_root: &Path) -> bool {
    project_root.join(".ta").join("workflow.toml").exists()
}

const TA_SUBDIRS: &[&str] = &[
    "goals",
    "pr_packages",
    "memory",
    "events",
    "personas",
    "workflows",
];

/// Onboard `opts.project_root` as a TA-managed project.
///
/// Creates (skipping anything already present):
///   `.ta/{goals,pr_packages,memory,events,personas,workflows}/`
///   `.ta/workflow.toml`  — minimal generic workflow config
///   `.ta/project-meta.toml` — records the TA version that initialized the project
///   `PLAN.md`            — starter development plan
///   `CLAUDE.md`           — stack-agnostic starter rules (build/test left as TODO)
///   `.mcp.json`           — project-scoped TA MCP server entry
///
/// Safe to call repeatedly: every file write is skip-if-exists, and directory
/// creation is idempotent by nature. Never overwrites an existing `CLAUDE.md`.
pub fn init_project(opts: &ProjectInitOptions) -> anyhow::Result<InitOutcome> {
    let project_root = opts.project_root;
    let ta_dir = project_root.join(".ta");
    let already_initialized = is_initialized(project_root);

    let mut outcome = InitOutcome {
        already_initialized,
        ..Default::default()
    };

    for sub in TA_SUBDIRS {
        let dir = ta_dir.join(sub);
        let existed = dir.exists();
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("Could not create .ta/{sub}: {e}", sub = sub, e = e))?;
        record(&mut outcome, project_root, &dir, existed);
    }

    write_if_absent(
        &mut outcome,
        project_root,
        &project_root.join("PLAN.md"),
        || starter_plan_md(opts.name),
    )?;

    write_if_absent(
        &mut outcome,
        project_root,
        &ta_dir.join("workflow.toml"),
        || starter_workflow_toml(opts.name),
    )?;

    write_if_absent(
        &mut outcome,
        project_root,
        &project_root.join("CLAUDE.md"),
        || starter_claude_md(opts.name),
    )?;

    write_if_absent(
        &mut outcome,
        project_root,
        &project_root.join(".mcp.json"),
        || starter_mcp_json(project_root),
    )?;

    write_if_absent(
        &mut outcome,
        project_root,
        &ta_dir.join("project-meta.toml"),
        || {
            let ta_version = env!("CARGO_PKG_VERSION");
            format!(
                "# Written by `ta init`. Do not edit manually — managed by TA.\n\
                 initialized_with = {ta_version:?}\n\
                 last_upgraded    = {ta_version:?}\n"
            )
        },
    )?;

    Ok(outcome)
}

/// Write `path` with the content produced by `content_fn`, unless it already exists.
/// Records the outcome (created vs. skipped) using paths relative to `project_root`.
fn write_if_absent(
    outcome: &mut InitOutcome,
    project_root: &Path,
    path: &Path,
    content_fn: impl FnOnce() -> String,
) -> anyhow::Result<()> {
    let existed = path.exists();
    if !existed {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content_fn())
            .map_err(|e| anyhow::anyhow!("Could not write {}: {}", path.display(), e))?;
    }
    record(outcome, project_root, path, existed);
    Ok(())
}

fn record(outcome: &mut InitOutcome, project_root: &Path, path: &Path, existed: bool) {
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .display()
        .to_string();
    if existed {
        outcome.skipped.push(rel);
    } else {
        outcome.created.push(rel);
    }
}

fn starter_plan_md(name: &str) -> String {
    format!(
        "# {name} — Development Plan\n\n\
         ## Versioning\n\n\
         Version format: `MAJOR.MINOR.PATCH-alpha`. Phases map directly to semver.\n\n\
         ---\n\
         <!-- Add phases below using `ta plan add` or the Plan tab in Studio. -->\n",
        name = name
    )
}

fn starter_workflow_toml(name: &str) -> String {
    format!(
        "[workflow]\n\
         name = \"{name}\"\n\
         enforce_phase_order = \"warn\"\n\
         context_budget_chars = 0\n\n\
         [build]\n\
         # commands = [\"<add your build command>\"]\n\n\
         [verify]\n\
         # commands = [\"<add your test command>\", \"<add your lint command>\"]\n\
         # on_failure = \"block\"\n",
        name = name
    )
}

/// Stack-agnostic starter `CLAUDE.md`. Deliberately leaves build/test commands
/// as a TODO — the stack isn't known at init time, so we don't guess.
fn starter_claude_md(name: &str) -> String {
    format!(
        r#"# {name}

## Build

<!-- TODO: add your build command, e.g. `npm run build` or `cargo build` -->

## Verify (all must pass before committing)

<!-- TODO: add your test/lint commands, e.g. `npm test` or `cargo test` -->

## Git Workflow

Always work on a feature branch. Never commit directly to `main`.
Branch prefixes: `feature/`, `fix/`, `refactor/`, `docs/`

## Rules

- Run verify after every code change, before committing
- Commit in logical working units
- Never disable or skip tests to make a build pass

## Observability

Error messages must state what happened, what was being attempted, and what
to do next. Never return a bare "Error" or "failed" without context.
"#,
        name = name
    )
}

/// Project-scoped `.mcp.json` with the correct `TA_CALLER_MODE=orchestrator`
/// env entry — mirrors the format `ta dev` writes via
/// `inject_mcp_server_config_with_session`, so a project initialized by
/// `ta init` never falls back to a global/user-scope MCP config that might
/// carry a stale `TA_IS_STAGING=1` (see PLAN.md v0.17.9 item 1a).
fn starter_mcp_json(project_root: &Path) -> String {
    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());

    let config = serde_json::json!({
        "mcpServers": {
            "ta": {
                "command": ta_binary,
                "args": ["serve"],
                "env": {
                    "TA_PROJECT_ROOT": project_root.display().to_string(),
                    "TA_CALLER_MODE": "orchestrator"
                }
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default() + "\n"
}

/// Convenience: derive a project name from the directory name when the
/// caller has no better name (e.g. no `--name` flag, no form field).
pub fn default_project_name(project_root: &Path) -> String {
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn opts<'a>(dir: &'a TempDir, name: &'a str) -> ProjectInitOptions<'a> {
        ProjectInitOptions {
            project_root: dir.path(),
            name,
        }
    }

    #[test]
    fn not_initialized_before_init() {
        let dir = TempDir::new().unwrap();
        assert!(!is_initialized(dir.path()));
    }

    #[test]
    fn init_project_creates_baseline_files() {
        let dir = TempDir::new().unwrap();
        let outcome = init_project(&opts(&dir, "MyApp")).unwrap();

        assert!(!outcome.already_initialized);
        assert!(is_initialized(dir.path()));

        for sub in TA_SUBDIRS {
            assert!(
                dir.path().join(".ta").join(sub).is_dir(),
                "missing .ta/{sub}"
            );
        }
        assert!(dir.path().join("PLAN.md").exists());
        assert!(dir.path().join(".ta/workflow.toml").exists());
        assert!(dir.path().join("CLAUDE.md").exists());
        assert!(dir.path().join(".mcp.json").exists());
        assert!(dir.path().join(".ta/project-meta.toml").exists());

        let claude_md = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("MyApp"));
        assert!(claude_md.contains("Never commit directly to `main`"));

        let plan = std::fs::read_to_string(dir.path().join("PLAN.md")).unwrap();
        assert!(plan.contains("MyApp"));
    }

    #[test]
    fn mcp_json_has_correct_caller_mode() {
        let dir = TempDir::new().unwrap();
        init_project(&opts(&dir, "MyApp")).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            json["mcpServers"]["ta"]["env"]["TA_CALLER_MODE"],
            "orchestrator"
        );
        assert!(json["mcpServers"]["ta"]["env"]["TA_PROJECT_ROOT"]
            .as_str()
            .unwrap()
            .contains(dir.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn init_project_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let first = init_project(&opts(&dir, "MyApp")).unwrap();
        assert!(!first.created.is_empty());

        let second = init_project(&opts(&dir, "MyApp")).unwrap();
        assert!(second.already_initialized);
        assert!(
            second.created.is_empty(),
            "second run should not create anything new, created: {:?}",
            second.created
        );
    }

    #[test]
    fn init_project_never_overwrites_edited_claude_md() {
        let dir = TempDir::new().unwrap();
        init_project(&opts(&dir, "MyApp")).unwrap();

        let claude_md_path = dir.path().join("CLAUDE.md");
        std::fs::write(&claude_md_path, "# Hand-edited by the user\n").unwrap();

        init_project(&opts(&dir, "MyApp")).unwrap();

        let content = std::fs::read_to_string(&claude_md_path).unwrap();
        assert_eq!(content, "# Hand-edited by the user\n");
    }

    #[test]
    fn init_project_reports_already_initialized_on_rerun() {
        let dir = TempDir::new().unwrap();
        init_project(&opts(&dir, "MyApp")).unwrap();
        let second = init_project(&opts(&dir, "MyApp")).unwrap();
        assert!(second.already_initialized);
    }

    #[test]
    fn default_project_name_uses_dirname() {
        let dir = TempDir::new().unwrap();
        let name = default_project_name(dir.path());
        assert!(!name.is_empty());
        assert_ne!(name, "project"); // tempdir names are never literally "project"
    }
}
