// bundled.rs — Compile-time embedded Pragma template (v0.16.5).
//
// All template files are embedded at compile time using include_str!.
// This keeps the binary self-contained — no external files required at runtime.

use std::collections::HashMap;

use crate::{
    engine::{parse_manifest, InitTemplate},
    feature::{
        BmadPlannerFeature, Feature, GitAdapterFeature, KotlinVerifyFeature, PragmaContextFeature,
    },
};

// ── Embedded file contents ────────────────────────────────────────────────────

const PRAGMA_TOML: &str = include_str!("../../../templates/pragma.toml");
const PRAGMA_CLAUDE_MD: &str = include_str!("../../../templates/pragma/CLAUDE.md.tera");
const PRAGMA_WORKFLOW: &str = include_str!("../../../templates/pragma/workflow.toml.tera");
const PRAGMA_MEMORY: &str = include_str!("../../../templates/pragma/memory.toml.tera");
const PRAGMA_POLICY: &str = include_str!("../../../templates/pragma/policy.yaml.tera");
const PRAGMA_TAIGNORE: &str = include_str!("../../../templates/pragma/taignore.tera");
const PRAGMA_PLAN: &str = include_str!("../../../templates/pragma/plan.md.tera");

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a fully resolved `InitTemplate` for the bundled Pragma template.
pub fn pragma_template() -> anyhow::Result<InitTemplate> {
    let manifest = parse_manifest(PRAGMA_TOML)?;

    let mut embedded = HashMap::new();
    embedded.insert("CLAUDE.md.tera".to_string(), PRAGMA_CLAUDE_MD.to_string());
    embedded.insert(
        "workflow.toml.tera".to_string(),
        PRAGMA_WORKFLOW.to_string(),
    );
    embedded.insert("memory.toml.tera".to_string(), PRAGMA_MEMORY.to_string());
    embedded.insert("policy.yaml.tera".to_string(), PRAGMA_POLICY.to_string());
    embedded.insert("taignore.tera".to_string(), PRAGMA_TAIGNORE.to_string());
    embedded.insert("plan.md.tera".to_string(), PRAGMA_PLAN.to_string());

    let features: Vec<Box<dyn Feature>> = vec![
        Box::new(KotlinVerifyFeature),
        Box::new(BmadPlannerFeature),
        Box::new(GitAdapterFeature),
        Box::new(PragmaContextFeature),
    ];

    Ok(InitTemplate {
        manifest,
        features,
        embedded_scaffold: embedded,
        scaffold_dir: None,
    })
}

/// Apply the bundled Pragma template to the given context.
///
/// This is the high-level entry point called by `ta init --template pragma`.
pub fn apply_pragma(ctx: &crate::feature::TemplateContext) -> anyhow::Result<()> {
    let tmpl = pragma_template()?;
    tmpl.apply(ctx)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::TemplateContext;
    use std::collections::HashMap;

    fn make_ctx(dir: &tempfile::TempDir, name: &str) -> TemplateContext {
        TemplateContext {
            project_root: dir.path().to_path_buf(),
            project_name: name.to_string(),
            vars: HashMap::new(),
        }
    }

    #[test]
    fn pragma_template_parses() {
        let tmpl = pragma_template().unwrap();
        assert_eq!(tmpl.manifest.template.name, "pragma");
        // features are defined at root level in pragma.toml (before [template] section).
        assert!(
            !tmpl.manifest.features.is_empty(),
            "pragma.toml must define features at root level (before [template] section)"
        );
        assert!(tmpl.embedded_scaffold.contains_key("CLAUDE.md.tera"));
        assert!(tmpl.embedded_scaffold.contains_key("workflow.toml.tera"));
        assert!(tmpl.embedded_scaffold.contains_key("memory.toml.tera"));
        assert!(tmpl.embedded_scaffold.contains_key("policy.yaml.tera"));
        assert!(tmpl.embedded_scaffold.contains_key("taignore.tera"));
        assert!(tmpl.embedded_scaffold.contains_key("plan.md.tera"));
    }

    #[test]
    fn apply_pragma_creates_expected_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = make_ctx(&dir, "MyPragmaGame");
        apply_pragma(&ctx).unwrap();

        let ta_dir = dir.path().join(".ta");

        // Scaffold files.
        let claude_md = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("# MyPragmaGame"));
        assert!(claude_md.contains("ktlintCheck"));
        assert!(claude_md.contains("ktlintFormat"));

        let workflow = std::fs::read_to_string(ta_dir.join("workflow.toml")).unwrap();
        assert!(workflow.contains("ktlintCheck"));
        assert!(workflow.contains("[analysis.kotlin]"));
        assert!(workflow.contains("on_failure = \"block\""));

        let memory = std::fs::read_to_string(ta_dir.join("memory.toml")).unwrap();
        assert!(memory.contains("pragma-kotlin"));
        assert!(memory.contains("service-map"));

        let policy = std::fs::read_to_string(ta_dir.join("policy.yaml")).unwrap();
        assert!(policy.contains("pragma-core"));
        assert!(policy.contains("settings.gradle.kts"));

        let taignore = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(taignore.contains(".gradle/"));
        assert!(taignore.contains("build/"));

        let plan = std::fs::read_to_string(dir.path().join("PLAN.md")).unwrap();
        assert!(plan.contains("MyPragmaGame"));
        assert!(plan.contains("Player Service Foundation"));
        assert!(plan.contains("Matchmaking Pipeline"));

        // Feature outputs.
        assert!(ta_dir.join("bmad.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-pm.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-architect.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-dev.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-qa.toml").exists());
        assert!(dir.path().join(".mcp.json").exists());
        assert!(ta_dir.join("onboarding-goal.md").exists());
        assert!(ta_dir.join("agents").join("pragma-planner.toml").exists());
        assert!(ta_dir.join("constitutions").join("kotlin.yaml").exists());
    }

    #[test]
    fn apply_pragma_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = make_ctx(&dir, "MyPragmaGame");
        apply_pragma(&ctx).unwrap();
        apply_pragma(&ctx).unwrap(); // Must not error on second call.
    }
}
