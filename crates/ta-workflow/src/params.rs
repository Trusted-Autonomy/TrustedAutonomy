// params.rs — Parameterized workflow template support (v0.15.23).
//
// Templates declare typed parameters with defaults. Parameters can reference
// plan context as built-ins. Invocations pass params at runtime via `--param key=value`.
//
// Interpolation syntax: `{{params.name}}` and `{{plan.*}}` in all string fields.
// Plan built-ins: `{{plan.current_version_prefix}}`, `{{plan.next_pending_phase}}`,
//   `{{plan.next_pending_title}}`, `{{plan.pending_count}}`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A typed parameter declaration in a workflow template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDecl {
    /// Human-readable description of the parameter.
    #[serde(default)]
    pub description: String,
    /// Parameter type: "string", "integer", "boolean".
    #[serde(default = "default_param_type")]
    pub r#type: String,
    /// Default value (may itself contain `{{plan.*}}` references).
    #[serde(default)]
    pub default: Option<String>,
    /// Whether the caller must supply this param (no default allowed to be missing).
    #[serde(default)]
    pub required: bool,
}

fn default_param_type() -> String {
    "string".to_string()
}

impl ParamDecl {
    /// Return a one-line summary for `ta workflow list` / `ta workflow show`.
    pub fn summary(&self) -> String {
        let type_tag = &self.r#type;
        let req_tag = if self.required { " [required]" } else { "" };
        let default_tag = match &self.default {
            Some(d) => format!(" (default: {})", d),
            None => String::new(),
        };
        format!(
            "{}{}{} — {}",
            type_tag, req_tag, default_tag, self.description
        )
    }
}

/// Resolved plan context variables extracted from PLAN.md.
#[derive(Debug, Clone, Default)]
pub struct PlanContext {
    /// Version prefix, e.g. "v0.15" (derived from the current running phase).
    pub current_version_prefix: String,
    /// Phase ID of the next `<!-- status: pending -->` phase, e.g. "v0.15.24".
    pub next_pending_phase: String,
    /// Title of the next pending phase.
    pub next_pending_title: String,
    /// Number of pending phases remaining.
    pub pending_count: usize,
}

impl PlanContext {
    /// Parse plan context from PLAN.md content.
    pub fn from_plan_md(content: &str) -> Self {
        let phase_re =
            regex::Regex::new(r"(?m)^###\s+(v[\d.]+[a-z]?)\s+[—\-]\s+(.+)$").expect("static");
        let status_re = regex::Regex::new(r"<!--\s*status:\s*(\w+)\s*-->").expect("static");

        let lines: Vec<&str> = content.lines().collect();
        let n = lines.len();

        // Collect (line_idx, id, title) for every ###-level phase header.
        let mut headers: Vec<(usize, String, String)> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let l = line.trim();
            if let Some(caps) = phase_re.captures(l) {
                let id = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
                let title = caps
                    .get(2)
                    .map(|m| m.as_str())
                    .unwrap_or("")
                    .trim_end_matches(['*', '(', ')'])
                    .trim()
                    .to_string();
                if !id.is_empty() {
                    headers.push((i, id, title));
                }
            }
        }

        // Determine status for each phase by scanning lines right after the header.
        let mut current_id = String::new();
        let mut pending_phases: Vec<(String, String)> = Vec::new();

        for h_idx in 0..headers.len() {
            let (start, ref id, ref title) = headers[h_idx];
            let end = headers.get(h_idx + 1).map(|(i, _, _)| *i).unwrap_or(n);
            let section = &lines[start..end];

            let mut status = "pending";
            for line in section[1..section.len().min(5)].iter() {
                if let Some(caps) = status_re.captures(line.trim()) {
                    let s = caps.get(1).map(|m| m.as_str()).unwrap_or("pending");
                    status = if s == "done" { "done" } else { s };
                    break;
                }
            }

            match status {
                "done" => {
                    // Track the highest done phase for version prefix.
                    current_id = id.clone();
                }
                "in_progress" | "~" => {
                    // Currently in-progress counts as current.
                    current_id = id.clone();
                }
                _ => {
                    pending_phases.push((id.clone(), title.clone()));
                }
            }
        }

        let next_pending_phase = pending_phases
            .first()
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        let next_pending_title = pending_phases
            .first()
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        let pending_count = pending_phases.len();

        // Extract version prefix from current_id (e.g., "v0.15.23" → "v0.15").
        let current_version_prefix = version_prefix_from_id(&current_id);

        PlanContext {
            current_version_prefix,
            next_pending_phase,
            next_pending_title,
            pending_count,
        }
    }

    /// Load PlanContext from the PLAN.md in the given workspace root.
    /// Returns a default (empty) context if PLAN.md is absent.
    pub fn load(workspace_root: &Path) -> Self {
        let plan_path = workspace_root.join("PLAN.md");
        match std::fs::read_to_string(&plan_path) {
            Ok(content) => Self::from_plan_md(&content),
            Err(_) => Self::default(),
        }
    }

    /// Return a mapping of `plan.*` variable names to their resolved values.
    pub fn as_vars(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "plan.current_version_prefix".to_string(),
            self.current_version_prefix.clone(),
        );
        m.insert(
            "plan.next_pending_phase".to_string(),
            self.next_pending_phase.clone(),
        );
        m.insert(
            "plan.next_pending_title".to_string(),
            self.next_pending_title.clone(),
        );
        m.insert(
            "plan.pending_count".to_string(),
            self.pending_count.to_string(),
        );
        m
    }
}

/// Extract a `vMAJOR.MINOR` prefix from a phase ID like `v0.15.23` → `v0.15`.
pub fn version_prefix_from_id(id: &str) -> String {
    if id.is_empty() {
        return String::new();
    }
    // Strip leading 'v', split on '.', take first two numeric parts, re-add 'v'.
    let stripped = id.strip_prefix('v').unwrap_or(id);
    let parts: Vec<&str> = stripped.splitn(3, '.').collect();
    if parts.len() >= 2 {
        format!("v{}.{}", parts[0], parts[1])
    } else {
        id.to_string()
    }
}

/// Resolved parameter values for a workflow invocation.
#[derive(Debug, Clone, Default)]
pub struct ParamValues {
    inner: HashMap<String, String>,
}

impl ParamValues {
    /// Build from CLI `--param key=value` pairs.
    pub fn from_cli_pairs(pairs: &[String]) -> Result<Self, String> {
        let mut inner = HashMap::new();
        for pair in pairs {
            let mut split = pair.splitn(2, '=');
            let key = split.next().unwrap_or("").trim().to_string();
            let value = split.next().unwrap_or("").to_string();
            if key.is_empty() {
                return Err(format!(
                    "invalid --param value '{}': expected key=value",
                    pair
                ));
            }
            inner.insert(key, value);
        }
        Ok(Self { inner })
    }

    /// Validate param values against the template's param declarations.
    ///
    /// - Unknown params → error.
    /// - Required params with no value and no default → error.
    /// - Optional params with defaults are filled in (after plan-var expansion).
    pub fn validate_and_fill(
        &mut self,
        decls: &HashMap<String, ParamDecl>,
        plan_ctx: &PlanContext,
    ) -> Result<(), String> {
        // Check for unknown params.
        for key in self.inner.keys() {
            if !decls.contains_key(key.as_str()) {
                let known: Vec<&str> = decls.keys().map(|k| k.as_str()).collect();
                return Err(format!(
                    "unknown parameter '{}'; known parameters: {}",
                    key,
                    if known.is_empty() {
                        "(none declared)".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
            }
        }

        // Fill in defaults for missing params.
        for (name, decl) in decls {
            if !self.inner.contains_key(name.as_str()) {
                match &decl.default {
                    Some(default_val) => {
                        // Defaults may contain {{plan.*}} references — expand them.
                        let expanded = interpolate_plan_vars(default_val, plan_ctx);
                        self.inner.insert(name.clone(), expanded);
                    }
                    None if decl.required => {
                        return Err(format!(
                            "required parameter '{}' not provided; {}",
                            name, decl.description
                        ));
                    }
                    None => {
                        // Optional with no default — insert empty string.
                        self.inner.insert(name.clone(), String::new());
                    }
                }
            }
        }

        Ok(())
    }

    /// Get a resolved parameter value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(|s| s.as_str())
    }

    /// Return all key→value pairs.
    pub fn all(&self) -> &HashMap<String, String> {
        &self.inner
    }
}

/// Interpolate `{{params.name}}` and `{{plan.*}}` references in a string.
///
/// Returns the interpolated string. Unknown references are left as-is (no
/// error — the caller is responsible for pre-validating required params).
pub fn interpolate(text: &str, params: &ParamValues, plan_ctx: &PlanContext) -> String {
    let plan_vars = plan_ctx.as_vars();
    let mut result = text.to_string();

    // Replace {{params.name}} references.
    for (key, value) in params.all() {
        let placeholder = format!("{{{{params.{}}}}}", key);
        result = result.replace(&placeholder, value);
    }

    // Replace {{plan.*}} references.
    for (key, value) in &plan_vars {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }

    result
}

/// Expand only `{{plan.*}}` references (used for default-value expansion).
fn interpolate_plan_vars(text: &str, plan_ctx: &PlanContext) -> String {
    let plan_vars = plan_ctx.as_vars();
    let mut result = text.to_string();
    for (key, value) in &plan_vars {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

/// Template library loader: discovers templates from disk and built-ins.
///
/// Search order (highest priority first):
/// 1. `.ta/workflow-templates/` in the workspace root (project-committed)
/// 2. `~/.config/ta/workflow-templates/` (user global)
/// 3. Built-in templates embedded in the binary
pub struct TemplateLibrary {
    workspace_root: std::path::PathBuf,
}

impl TemplateLibrary {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    /// List all available templates (name, description, source).
    pub fn list(&self) -> Vec<TemplateEntry> {
        let mut entries: HashMap<String, TemplateEntry> = HashMap::new();

        // 3. Built-ins first (lowest priority — may be overridden).
        for (name, content) in BUILTIN_TEMPLATES {
            let (description, params, tags) = extract_template_meta(content);
            entries.insert(
                name.to_string(),
                TemplateEntry {
                    name: name.to_string(),
                    description,
                    source: TemplateSource::Builtin,
                    params,
                    tags,
                },
            );
        }

        // 2. User global templates.
        if let Some(user_dir) = user_templates_dir() {
            self.load_from_dir(&user_dir, TemplateSource::User, &mut entries);
        }

        // 1. Project templates (highest priority).
        let project_dir = self.workspace_root.join(".ta").join("workflow-templates");
        self.load_from_dir(&project_dir, TemplateSource::Project, &mut entries);

        let mut result: Vec<TemplateEntry> = entries.into_values().collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Load a template by name, searching project → user → built-in.
    pub fn load(&self, name: &str) -> Option<String> {
        // Project templates.
        let project_dir = self.workspace_root.join(".ta").join("workflow-templates");
        let path = project_dir.join(format!("{}.yaml", name));
        if path.exists() {
            return std::fs::read_to_string(&path).ok();
        }

        // User global templates.
        if let Some(user_dir) = user_templates_dir() {
            let path = user_dir.join(format!("{}.yaml", name));
            if path.exists() {
                return std::fs::read_to_string(&path).ok();
            }
        }

        // Built-in templates.
        BUILTIN_TEMPLATES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| c.to_string())
    }

    fn load_from_dir(
        &self,
        dir: &Path,
        source: TemplateSource,
        entries: &mut HashMap<String, TemplateEntry>,
    ) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (description, params, tags) = extract_template_meta(&content);
            entries.insert(
                stem.to_string(),
                TemplateEntry {
                    name: stem.to_string(),
                    description,
                    source: source.clone(),
                    params,
                    tags,
                },
            );
        }
    }
}

/// An entry in the template library.
#[derive(Debug, Clone)]
pub struct TemplateEntry {
    pub name: String,
    pub description: String,
    pub source: TemplateSource,
    /// Parameter names with their summaries.
    pub params: Vec<(String, String)>,
    /// Tags for discovery (parsed from `# tags:` comment or `tags:` YAML field).
    pub tags: Vec<String>,
}

/// Where a template comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    Project,
    User,
    Builtin,
}

impl std::fmt::Display for TemplateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateSource::Project => write!(f, "project"),
            TemplateSource::User => write!(f, "user"),
            TemplateSource::Builtin => write!(f, "built-in"),
        }
    }
}

/// Platform-appropriate user-global workflow templates directory.
fn user_templates_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("ta")
            .join("workflow-templates"),
    )
}

/// Extract description, param list, and tags from a template YAML.
///
/// Tags are parsed from a `# tags: tag1, tag2` comment or a top-level `tags:` YAML field.
fn extract_template_meta(content: &str) -> (String, Vec<(String, String)>, Vec<String>) {
    let mut description = String::new();
    let mut comment_tags: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# description:") {
            if description.is_empty() {
                description = rest.trim().to_string();
            }
        } else if let Some(rest) = trimmed.strip_prefix("# tags:") {
            comment_tags = rest
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        } else if let Some(rest) = trimmed.strip_prefix('#') {
            let comment = rest.trim();
            if !comment.is_empty()
                && !comment.starts_with('!')
                && description.is_empty()
                && !comment.starts_with("Usage")
            {
                description = comment.to_string();
            }
        }
    }

    let mut params: Vec<(String, String)> = Vec::new();
    let mut yaml_tags: Vec<String> = Vec::new();

    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(content) {
        // Parse params section.
        if let Some(serde_yaml::Value::Mapping(param_map)) = value.get("params") {
            for (key, val) in param_map {
                let name = match key {
                    serde_yaml::Value::String(s) => s.clone(),
                    _ => continue,
                };
                let summary = if let Ok(decl) = serde_yaml::from_value::<ParamDecl>(val.clone()) {
                    decl.summary()
                } else {
                    String::new()
                };
                params.push((name, summary));
            }
        }

        // Parse tags from YAML field (overrides comment-based tags if both present).
        if let Some(serde_yaml::Value::Sequence(seq)) = value.get("tags") {
            yaml_tags = seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
    }

    let tags = if !yaml_tags.is_empty() {
        yaml_tags
    } else {
        comment_tags
    };

    (description, params, tags)
}

/// Built-in templates embedded in the binary.
///
/// Each entry is (name, yaml_content). These are the reusable, parameterized
/// templates shipped with TA.
const BUILTIN_TEMPLATES: &[(&str, &str)] = &[
    ("plan-build-phases", TEMPLATE_PLAN_BUILD_PHASES),
    ("governed-goal", TEMPLATE_GOVERNED_GOAL),
];

const TEMPLATE_PLAN_BUILD_PHASES: &str = r#"# description: Iterate pending PLAN.md phases through the governed build workflow.
# tags: plan, phases, automation
#
# Parameters:
#   phase_filter — only process phases matching this prefix (e.g., v0.15)
#   max_phases   — stop after this many phases (guard against runaway loops)
#
# Built-in plan vars used as defaults:
#   {{plan.current_version_prefix}} → current major.minor version prefix
#   {{plan.next_pending_phase}}     → next pending phase ID
#   {{plan.pending_count}}          → number of pending phases

name: plan-build-phases

params:
  phase_filter:
    type: string
    description: "Phase ID prefix to process (e.g., v0.15). Empty = all pending."
    default: "{{plan.current_version_prefix}}"
    required: false
  max_phases:
    type: integer
    description: "Maximum number of phases to process in one run."
    default: "5"
    required: false

stages:
  - name: build
    roles: [implementor]
    await_human: never

  - name: review
    depends_on: [build]
    roles: [reviewer]
    await_human: on_fail
    on_fail:
      route_to: build
      max_retries: 2

roles:
  implementor:
    agent: claude-code
    prompt: |
      You are implementing plan phase {{params.phase_filter}}.
      Process up to {{params.max_phases}} pending phases from PLAN.md.
      Start with: {{plan.next_pending_phase}} — {{plan.next_pending_title}}.
      There are {{plan.pending_count}} phases remaining total.

  reviewer:
    agent: claude-code
    prompt: |
      You are reviewing the implementation of {{params.phase_filter}} phases.
      Verify correctness, test coverage, and adherence to project standards.

verdict:
  pass_threshold: 0.7
"#;

const TEMPLATE_GOVERNED_GOAL: &str = r#"# description: Safe autonomous coding loop: run_goal → review → human_gate → apply → pr_sync.
# tags: governance, safe, coding
#
# Parameters:
#   goal_title — the goal to implement (required)
#   phase      — PLAN.md phase to link the goal to (optional)

name: governed-goal

params:
  goal_title:
    type: string
    description: "Goal title to implement through the governed workflow."
    required: true
  phase:
    type: string
    description: "PLAN.md phase ID to link to (e.g., v0.15.23)."
    default: "{{plan.next_pending_phase}}"
    required: false

stages:
  - name: run_goal
    roles: [implementor]
    await_human: never

  - name: review_draft
    depends_on: [run_goal]
    roles: [reviewer]
    await_human: on_fail
    on_fail:
      route_to: run_goal
      max_retries: 2

  - name: human_gate
    depends_on: [review_draft]
    roles: []
    await_human: always

  - name: apply_draft
    depends_on: [human_gate]
    roles: [deployer]
    await_human: never

  - name: pr_sync
    depends_on: [apply_draft]
    roles: [pr_author]
    await_human: never

roles:
  implementor:
    agent: claude-code
    prompt: |
      Implement: {{params.goal_title}}
      Phase: {{params.phase}}

  reviewer:
    agent: claude-code
    prompt: |
      Review the implementation of: {{params.goal_title}}
      Check correctness, tests, and code quality.

  deployer:
    agent: claude-code
    prompt: |
      Apply the draft for: {{params.goal_title}}

  pr_author:
    agent: claude-code
    prompt: |
      Open a pull request for: {{params.goal_title}}

verdict:
  pass_threshold: 0.7
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // ── version_prefix_from_id ──────────────────────────────────────────────

    #[test]
    fn version_prefix_three_parts() {
        assert_eq!(version_prefix_from_id("v0.15.23"), "v0.15");
    }

    #[test]
    fn version_prefix_two_parts() {
        assert_eq!(version_prefix_from_id("v0.15"), "v0.15");
    }

    #[test]
    fn version_prefix_empty() {
        assert_eq!(version_prefix_from_id(""), "");
    }

    // ── PlanContext ─────────────────────────────────────────────────────────

    #[test]
    fn plan_context_extracts_pending_phases() {
        let plan_md = r#"
### v0.15.22 — Secret Scan
<!-- status: done -->

### v0.15.23 — Parameterized Workflow Templates
<!-- status: in_progress -->

### v0.15.24 — Intent Resolver
<!-- status: pending -->

### v0.15.25 — Auto-Approve Constitution
<!-- status: pending -->
"#;
        let ctx = PlanContext::from_plan_md(plan_md);
        assert_eq!(ctx.current_version_prefix, "v0.15");
        assert_eq!(ctx.next_pending_phase, "v0.15.24");
        assert_eq!(ctx.next_pending_title, "Intent Resolver");
        assert_eq!(ctx.pending_count, 2);
    }

    #[test]
    fn plan_context_empty_when_no_pending() {
        let plan_md = r#"
### v0.15.22 — Done Phase
<!-- status: done -->
"#;
        let ctx = PlanContext::from_plan_md(plan_md);
        assert_eq!(ctx.next_pending_phase, "");
        assert_eq!(ctx.pending_count, 0);
        assert_eq!(ctx.current_version_prefix, "v0.15");
    }

    #[test]
    fn plan_context_as_vars_contains_all_keys() {
        let ctx = PlanContext {
            current_version_prefix: "v0.15".to_string(),
            next_pending_phase: "v0.15.24".to_string(),
            next_pending_title: "Intent Resolver".to_string(),
            pending_count: 3,
        };
        let vars = ctx.as_vars();
        assert_eq!(vars["plan.current_version_prefix"], "v0.15");
        assert_eq!(vars["plan.next_pending_phase"], "v0.15.24");
        assert_eq!(vars["plan.next_pending_title"], "Intent Resolver");
        assert_eq!(vars["plan.pending_count"], "3");
    }

    // ── ParamValues ─────────────────────────────────────────────────────────

    #[test]
    fn param_values_from_cli_pairs_ok() {
        let pairs = vec!["phase_filter=v0.15".to_string(), "max_phases=3".to_string()];
        let pv = ParamValues::from_cli_pairs(&pairs).unwrap();
        assert_eq!(pv.get("phase_filter"), Some("v0.15"));
        assert_eq!(pv.get("max_phases"), Some("3"));
    }

    #[test]
    fn param_values_invalid_pair_error() {
        let pairs = vec!["=bad".to_string()];
        assert!(ParamValues::from_cli_pairs(&pairs).is_err());
    }

    #[test]
    fn param_values_unknown_param_error() {
        let pairs = vec!["unknown_key=val".to_string()];
        let mut pv = ParamValues::from_cli_pairs(&pairs).unwrap();
        let decls: HashMap<String, ParamDecl> = HashMap::new();
        let plan_ctx = PlanContext::default();
        let err = pv.validate_and_fill(&decls, &plan_ctx).unwrap_err();
        assert!(err.contains("unknown parameter"), "got: {}", err);
    }

    #[test]
    fn param_values_required_missing_error() {
        let mut pv = ParamValues::default();
        let mut decls: HashMap<String, ParamDecl> = HashMap::new();
        decls.insert(
            "goal_title".to_string(),
            ParamDecl {
                description: "The goal to implement".to_string(),
                r#type: "string".to_string(),
                default: None,
                required: true,
            },
        );
        let plan_ctx = PlanContext::default();
        let err = pv.validate_and_fill(&decls, &plan_ctx).unwrap_err();
        assert!(err.contains("required parameter"), "got: {}", err);
    }

    #[test]
    fn param_values_default_filled_from_plan_var() {
        let mut pv = ParamValues::default();
        let mut decls: HashMap<String, ParamDecl> = HashMap::new();
        decls.insert(
            "phase_filter".to_string(),
            ParamDecl {
                description: "Phase prefix".to_string(),
                r#type: "string".to_string(),
                default: Some("{{plan.current_version_prefix}}".to_string()),
                required: false,
            },
        );
        let plan_ctx = PlanContext {
            current_version_prefix: "v0.15".to_string(),
            ..Default::default()
        };
        pv.validate_and_fill(&decls, &plan_ctx).unwrap();
        assert_eq!(pv.get("phase_filter"), Some("v0.15"));
    }

    // ── interpolate ─────────────────────────────────────────────────────────

    #[test]
    fn interpolate_params() {
        let plan_ctx = PlanContext::default();
        let mut pv = ParamValues::default();
        pv.inner
            .insert("goal_title".to_string(), "Fix the auth bug".to_string());
        let result = interpolate("Implement: {{params.goal_title}}", &pv, &plan_ctx);
        assert_eq!(result, "Implement: Fix the auth bug");
    }

    #[test]
    fn interpolate_plan_vars_in_text() {
        let plan_ctx = PlanContext {
            current_version_prefix: "v0.15".to_string(),
            next_pending_phase: "v0.15.24".to_string(),
            next_pending_title: "Intent Resolver".to_string(),
            pending_count: 2,
        };
        let pv = ParamValues::default();
        let result = interpolate(
            "Working on {{plan.current_version_prefix}}, next: {{plan.next_pending_phase}}",
            &pv,
            &plan_ctx,
        );
        assert_eq!(result, "Working on v0.15, next: v0.15.24");
    }

    #[test]
    fn interpolate_unknown_placeholder_left_as_is() {
        let plan_ctx = PlanContext::default();
        let pv = ParamValues::default();
        let result = interpolate("Hello {{params.unknown}}", &pv, &plan_ctx);
        assert_eq!(result, "Hello {{params.unknown}}");
    }

    // ── TemplateLibrary ─────────────────────────────────────────────────────

    #[test]
    fn template_library_lists_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let lib = TemplateLibrary::new(dir.path());
        let entries = lib.list();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"plan-build-phases"), "got: {:?}", names);
        assert!(names.contains(&"governed-goal"), "got: {:?}", names);
    }

    #[test]
    fn template_library_project_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let templates_dir = dir.path().join(".ta").join("workflow-templates");
        std::fs::create_dir_all(&templates_dir).unwrap();
        std::fs::write(
            templates_dir.join("plan-build-phases.yaml"),
            "# description: custom override\nname: plan-build-phases\nstages: []\nroles: {}\n",
        )
        .unwrap();

        let lib = TemplateLibrary::new(dir.path());
        let entry = lib
            .list()
            .into_iter()
            .find(|e| e.name == "plan-build-phases")
            .unwrap();
        assert_eq!(entry.source, TemplateSource::Project);
        assert_eq!(entry.description, "custom override");
    }

    #[test]
    fn template_library_load_builtin_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let lib = TemplateLibrary::new(dir.path());
        let content = lib.load("plan-build-phases").unwrap();
        assert!(content.contains("plan-build-phases"));
    }

    #[test]
    fn template_library_load_unknown_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let lib = TemplateLibrary::new(dir.path());
        assert!(lib.load("nonexistent-template").is_none());
    }
}
