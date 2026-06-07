// scaffold.rs — Tera template rendering for scaffold files (v0.16.5).
//
// Scaffold files use Tera syntax: {{ project_name }}, {% if ... %}, etc.
// Variables come from the TemplateContext: project_name and all vars entries.

/// Render a Tera template string with variables from the given context.
///
/// Template variables available:
/// - `project_name` — the project name string
/// - All entries from `ctx.vars`
///
/// Returns the rendered string.
pub fn render(template: &str, ctx: &crate::feature::TemplateContext) -> anyhow::Result<String> {
    let mut tera_ctx = tera::Context::new();
    tera_ctx.insert("project_name", &ctx.project_name);
    for (k, v) in &ctx.vars {
        tera_ctx.insert(k.as_str(), v);
    }
    tera::Tera::one_off(template, &tera_ctx, false)
        .map_err(|e| anyhow::anyhow!("Scaffold rendering failed: {e}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::TemplateContext;
    use std::collections::HashMap;

    fn ctx(name: &str, vars: Vec<(&str, &str)>) -> TemplateContext {
        TemplateContext {
            project_root: std::path::PathBuf::from("/tmp/test"),
            project_name: name.to_string(),
            vars: vars
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn render_project_name() {
        let result = render("# {{ project_name }}", &ctx("MyProject", vec![])).unwrap();
        assert_eq!(result, "# MyProject");
    }

    #[test]
    fn render_custom_var() {
        let result = render(
            "Engine: {{ engine_version }}",
            &ctx("P", vec![("engine_version", "2026.1.0")]),
        )
        .unwrap();
        assert_eq!(result, "Engine: 2026.1.0");
    }

    #[test]
    fn render_no_vars() {
        let result = render("No template vars here.\n", &ctx("X", vec![])).unwrap();
        assert_eq!(result, "No template vars here.\n");
    }

    #[test]
    fn render_invalid_template_errors() {
        let result = render("{{ unclosed", &ctx("X", vec![]));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Scaffold rendering failed"),);
    }

    #[test]
    fn render_empty_vars_map() {
        let ctx = TemplateContext {
            project_root: std::path::PathBuf::from("/tmp"),
            project_name: "Test".to_string(),
            vars: HashMap::new(),
        };
        let result = render("Hello {{ project_name }}!", &ctx).unwrap();
        assert_eq!(result, "Hello Test!");
    }
}
