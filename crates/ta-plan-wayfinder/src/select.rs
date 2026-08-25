// select.rs — runtime `PlanStore` backend selection, mirroring
// `ta-submit`'s own `select_adapter` pattern (`.ta/workflow.toml`'s
// `[submit] adapter = "git"`, here `[plan] backend = "wayfinder"`).
//
// Self-contained on purpose: a call site only needs `project_root` and
// `goals_dir` (the same two arguments every existing `FilePlanStore::new`
// call site already has in hand) — this function loads
// `.ta/workflow.toml` itself rather than asking the caller to thread a
// `WorkflowConfig` through, so swapping a call site over is a one-line
// change.

use std::path::Path;

use anyhow::Context;
use ta_plan::{FilePlanStore, PlanStore};
use ta_submit::WorkflowConfig;

use crate::config::WayfinderPlanConfig;
use crate::store::WayfinderPlanStore;

/// Returns the configured `PlanStore` backend for this project. Defaults to
/// `FilePlanStore` when `.ta/workflow.toml` is absent or `[plan] backend`
/// is absent/`"file"` — selecting `"wayfinder"` is opt-in and changes no
/// existing deployment's behavior.
pub fn select_plan_store(
    project_root: impl AsRef<Path>,
    goals_dir: impl AsRef<Path>,
) -> anyhow::Result<Box<dyn PlanStore>> {
    let project_root = project_root.as_ref();
    let workflow_toml = project_root.join(".ta").join("workflow.toml");
    let workflow_config = WorkflowConfig::load_or_default(&workflow_toml);

    match workflow_config.plan.backend.as_str() {
        "wayfinder" => {
            let raw = workflow_config.plan.wayfinder.as_ref().with_context(|| {
                "[plan] backend = \"wayfinder\" but no [plan.wayfinder] table was found in \
                 .ta/workflow.toml — add base_url, org_id, project_id, and credential_name"
            })?;
            let config = WayfinderPlanConfig::load(project_root, raw)?;
            let store = WayfinderPlanStore::new(project_root, goals_dir, &config)?;
            Ok(Box::new(store))
        }
        "file" | "" => {
            let store = FilePlanStore::new(project_root, goals_dir)?;
            Ok(Box::new(store))
        }
        other => anyhow::bail!(
            "[plan] backend = \"{other}\" is not a recognized PlanStore backend — expected \
             \"file\" (default) or \"wayfinder\""
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_plan_md(dir: &TempDir) {
        std::fs::write(
            dir.path().join("PLAN.md"),
            "### v0.1.0 — First phase\n<!-- status: pending -->\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".ta").join("goals")).unwrap();
    }

    #[test]
    fn missing_workflow_toml_defaults_to_file_backend() {
        let dir = TempDir::new().unwrap();
        setup_plan_md(&dir);
        let store = select_plan_store(dir.path(), dir.path().join(".ta/goals")).unwrap();
        assert_eq!(store.backend_name(), "file");
    }

    #[test]
    fn explicit_file_backend_is_honored() {
        let dir = TempDir::new().unwrap();
        setup_plan_md(&dir);
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[plan]\nbackend = \"file\"\n",
        )
        .unwrap();
        let store = select_plan_store(dir.path(), dir.path().join(".ta/goals")).unwrap();
        assert_eq!(store.backend_name(), "file");
    }

    #[test]
    fn wayfinder_backend_without_a_wayfinder_table_is_a_config_error() {
        let dir = TempDir::new().unwrap();
        setup_plan_md(&dir);
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[plan]\nbackend = \"wayfinder\"\n",
        )
        .unwrap();
        // Not `.unwrap_err()`: that requires the `Ok` type (`Box<dyn
        // PlanStore>`) to implement `Debug` for its panic-message
        // formatting, which trait objects don't. `.err().unwrap()` only
        // needs the error type to be `Debug`.
        let err = select_plan_store(dir.path(), dir.path().join(".ta/goals"))
            .err()
            .unwrap();
        assert!(err.to_string().contains("[plan.wayfinder]"));
    }

    #[test]
    fn unknown_backend_is_a_config_error() {
        let dir = TempDir::new().unwrap();
        setup_plan_md(&dir);
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[plan]\nbackend = \"carrier-pigeon\"\n",
        )
        .unwrap();
        // Not `.unwrap_err()`: that requires the `Ok` type (`Box<dyn
        // PlanStore>`) to implement `Debug` for its panic-message
        // formatting, which trait objects don't. `.err().unwrap()` only
        // needs the error type to be `Debug`.
        let err = select_plan_store(dir.path(), dir.path().join(".ta/goals"))
            .err()
            .unwrap();
        assert!(err.to_string().contains("carrier-pigeon"));
    }
}
