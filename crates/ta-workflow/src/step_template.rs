// step_template.rs — Template interpolation for step actions and payloads (v0.17.0.4.5).
//
// Resolves `{{context.field}}`, `{{trigger.data.field}}`, and `{{step.elapsed_secs}}`
// in string values within action params, context_patch maps, and payload templates.
//
// Unknown placeholders are left as-is (no error — callers pre-validate required fields).

use serde_json::{Map, Value};

use crate::step_context::{TransitionPayload, WorkflowContext};

// ── resolve_template ──────────────────────────────────────────────────────────

/// Resolve template placeholders in a single string.
///
/// Supported placeholders:
/// - `{{context.FIELD}}` → value of `ctx.fields["FIELD"]` as a string
/// - `{{trigger.data.FIELD}}` → value of `trigger.data["FIELD"]` as a string
/// - `{{step.elapsed_secs}}` → `elapsed_secs` as a decimal string
///
/// Unrecognized placeholders are left unchanged.
pub fn resolve_template(
    template: &str,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> String {
    let mut result = template.to_string();
    let mut output = String::with_capacity(result.len());
    let input = result.as_str();
    let mut remaining = input;

    while let Some(open) = remaining.find("{{") {
        output.push_str(&remaining[..open]);
        let after_open = &remaining[open + 2..];
        if let Some(close) = after_open.find("}}") {
            let placeholder = after_open[..close].trim();
            let resolved = resolve_placeholder(placeholder, trigger, ctx, elapsed_secs);
            output.push_str(&resolved);
            remaining = &after_open[close + 2..];
        } else {
            // No closing braces — emit literally and stop scanning.
            output.push_str("{{");
            remaining = after_open;
        }
    }
    output.push_str(remaining);
    result = output;
    result
}

/// Resolve a single placeholder name (without the `{{ }}`).
fn resolve_placeholder(
    placeholder: &str,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> String {
    if placeholder == "step.elapsed_secs" {
        return elapsed_secs.to_string();
    }

    if let Some(field) = placeholder.strip_prefix("context.") {
        return ctx
            .fields
            .get(field)
            .map(value_to_string)
            .unwrap_or_else(|| format!("{{{{{}}}}}", placeholder));
    }

    if let Some(path) = placeholder.strip_prefix("trigger.data.") {
        if let Some(trig) = trigger {
            if let Some(v) = trig.data.get(path) {
                return value_to_string(v);
            }
        }
        return format!("{{{{{}}}}}", placeholder);
    }

    // Unknown placeholder — leave as-is.
    format!("{{{{{}}}}}", placeholder)
}

/// Convert a JSON value to a human-readable string for template substitution.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ── resolve_params ────────────────────────────────────────────────────────────

/// Resolve templates in all string values within a params map.
///
/// Non-string values are passed through unchanged. String values have their
/// placeholders resolved. Object values are recursed into.
pub fn resolve_params(
    params: &Map<String, Value>,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in params {
        out.insert(k.clone(), resolve_value(v, trigger, ctx, elapsed_secs));
    }
    out
}

// ── resolve_context_patch ─────────────────────────────────────────────────────

/// Resolve templates in a context_patch map.
///
/// The context_patch's values may themselves be template strings (e.g.,
/// `"{{context.rework_count + 1}}"` — note that arithmetic is NOT evaluated;
/// only simple field references are resolved).
pub fn resolve_context_patch(
    patch: &Map<String, Value>,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> Map<String, Value> {
    resolve_params(patch, trigger, ctx, elapsed_secs)
}

/// Recursively resolve templates in a JSON value.
fn resolve_value(
    v: &Value,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> Value {
    match v {
        Value::String(s) => Value::String(resolve_template(s, trigger, ctx, elapsed_secs)),
        Value::Object(map) => Value::Object(resolve_params(map, trigger, ctx, elapsed_secs)),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|elem| resolve_value(elem, trigger, ctx, elapsed_secs))
                .collect(),
        ),
        // Numbers, booleans, null: pass through unchanged.
        other => other.clone(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(pairs: &[(&str, &str)]) -> WorkflowContext {
        let mut ctx = WorkflowContext::new();
        for (k, v) in pairs {
            ctx.fields
                .insert(k.to_string(), Value::String(v.to_string()));
        }
        ctx
    }

    fn trigger_with(pairs: &[(&str, &str)]) -> TransitionPayload {
        let mut data = Map::new();
        for (k, v) in pairs {
            data.insert(k.to_string(), Value::String(v.to_string()));
        }
        TransitionPayload {
            source_step: "prev".to_string(),
            edge: "apply".to_string(),
            data: Value::Object(data),
        }
    }

    // ── resolve_template ─────────────────────────────────────────────────────

    #[test]
    fn context_field_resolved() {
        let ctx = ctx_with(&[("author", "alice")]);
        let result = resolve_template("Hello {{context.author}}", None, &ctx, 0);
        assert_eq!(result, "Hello alice");
    }

    #[test]
    fn trigger_data_field_resolved() {
        let ctx = WorkflowContext::new();
        let trigger = trigger_with(&[("draft_id", "d42")]);
        let result = resolve_template("Draft: {{trigger.data.draft_id}}", Some(&trigger), &ctx, 0);
        assert_eq!(result, "Draft: d42");
    }

    #[test]
    fn elapsed_secs_resolved() {
        let ctx = WorkflowContext::new();
        let result = resolve_template("Elapsed: {{step.elapsed_secs}}s", None, &ctx, 120);
        assert_eq!(result, "Elapsed: 120s");
    }

    #[test]
    fn unknown_placeholder_left_as_is() {
        let ctx = WorkflowContext::new();
        let result = resolve_template("{{unknown.field}}", None, &ctx, 0);
        assert_eq!(result, "{{unknown.field}}");
    }

    #[test]
    fn missing_context_field_left_as_is() {
        let ctx = WorkflowContext::new();
        let result = resolve_template("{{context.missing}}", None, &ctx, 0);
        assert_eq!(result, "{{context.missing}}");
    }

    #[test]
    fn no_trigger_leaves_trigger_placeholder_as_is() {
        let ctx = WorkflowContext::new();
        let result = resolve_template("{{trigger.data.draft_id}}", None, &ctx, 0);
        assert_eq!(result, "{{trigger.data.draft_id}}");
    }

    #[test]
    fn multiple_placeholders_in_one_string() {
        let ctx = ctx_with(&[("author", "alice")]);
        let trigger = trigger_with(&[("draft_id", "d42")]);
        let result = resolve_template(
            "Author: {{context.author}}, Draft: {{trigger.data.draft_id}}",
            Some(&trigger),
            &ctx,
            0,
        );
        assert_eq!(result, "Author: alice, Draft: d42");
    }

    #[test]
    fn numeric_context_field_resolved_as_string() {
        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("count".to_string(), Value::Number(42.into()));
        let result = resolve_template("Count: {{context.count}}", None, &ctx, 0);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn plain_string_unchanged() {
        let ctx = WorkflowContext::new();
        let result = resolve_template("no placeholders here", None, &ctx, 0);
        assert_eq!(result, "no placeholders here");
    }

    // ── resolve_params ────────────────────────────────────────────────────────

    #[test]
    fn resolve_params_resolves_string_values() {
        let ctx = ctx_with(&[("author_email", "alice@example.com")]);
        let mut params = Map::new();
        params.insert(
            "to".to_string(),
            Value::String("{{context.author_email}}".to_string()),
        );
        params.insert("subject".to_string(), Value::String("Hello".to_string()));
        let resolved = resolve_params(&params, None, &ctx, 0);
        assert_eq!(
            resolved["to"],
            Value::String("alice@example.com".to_string())
        );
        assert_eq!(resolved["subject"], Value::String("Hello".to_string()));
    }

    #[test]
    fn resolve_params_passes_through_non_string_values() {
        let ctx = WorkflowContext::new();
        let mut params = Map::new();
        params.insert("count".to_string(), Value::Number(5.into()));
        params.insert("enabled".to_string(), Value::Bool(true));
        let resolved = resolve_params(&params, None, &ctx, 0);
        assert_eq!(resolved["count"], Value::Number(5.into()));
        assert_eq!(resolved["enabled"], Value::Bool(true));
    }

    #[test]
    fn resolve_context_patch_resolves_values() {
        let ctx = ctx_with(&[("current", "old_value")]);
        let mut patch = Map::new();
        patch.insert(
            "last".to_string(),
            Value::String("{{context.current}}".to_string()),
        );
        let resolved = resolve_context_patch(&patch, None, &ctx, 0);
        assert_eq!(resolved["last"], Value::String("old_value".to_string()));
    }
}
