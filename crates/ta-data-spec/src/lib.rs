//! Versioned JSON Schema data-format specs for TA's core wire types
//! (`docs/design/ta-concepts-and-architecture.md` §13.1, `docs/design/ta-data-format-spec.md`).
//!
//! Publishes `schemars`-derived JSON Schema for the five types identified as
//! the real interface boundary between TA's core, Studio, and
//! community-authored trigger-configs/plugins: [`Goal`](spec_goal_run),
//! [`Draft`/`Artifact`](spec_draft_package), [`TriggerEvent`](spec_trigger_event),
//! [`RoutingDecision`](spec_routing_decision), and [`Persona`](spec_persona_config).
//!
//! This crate has exactly one job: generate these schemas from the real,
//! already-serde types (`ta_goal::GoalRun`, `ta_changeset::draft_package::{DraftPackage, Artifact}`,
//! `ta_intake::TriggerEvent`, `ta_brain::RoutingDecision`, `ta_goal::PersonaConfig`) —
//! not a parallel mirror DTO layer that could drift from the real wire format.
//!
//! Regenerate the checked-in files under `schema/` with:
//! ```text
//! cargo run -p ta-data-spec --bin gen-schema
//! ```

use schemars::Schema;
use serde_json::Value;

/// One published spec: a name, an explicit version, and the checked-in file
/// it's regenerated into. Bumping `version` is a deliberate, reviewable act —
/// it is not derived from the crate/workspace version, since a spec schema
/// and the binary that happens to ship it change on different cadences.
pub struct SpecEntry {
    /// Stable name used in `$id` and the schema file name.
    pub name: &'static str,
    /// Explicit schema version. Bump when the shape changes in a way that
    /// isn't purely additive/backward-compatible.
    pub version: u32,
    /// Path relative to the workspace root `schema/` directory.
    pub file: &'static str,
    /// Generates the current schema for this spec.
    pub generate: fn() -> Schema,
}

/// The five published data-format specs (item 1 of v0.17.0.12.21).
pub const SPECS: &[SpecEntry] = &[
    SpecEntry {
        name: "goal",
        version: 1,
        file: "goal.schema.json",
        generate: goal_schema,
    },
    SpecEntry {
        name: "draft",
        version: 1,
        file: "draft.schema.json",
        generate: draft_schema,
    },
    SpecEntry {
        name: "artifact",
        version: 1,
        file: "artifact.schema.json",
        generate: artifact_schema,
    },
    SpecEntry {
        name: "trigger_event",
        version: 1,
        file: "trigger_event.schema.json",
        generate: trigger_event_schema,
    },
    SpecEntry {
        name: "routing_decision",
        version: 1,
        file: "routing_decision.schema.json",
        generate: routing_decision_schema,
    },
    SpecEntry {
        name: "persona",
        version: 1,
        file: "persona.schema.json",
        generate: persona_schema,
    },
];

/// The `Goal` spec — `ta_goal::GoalRun`, the execution-lifecycle unit.
pub fn goal_schema() -> Schema {
    schemars::schema_for!(ta_goal::GoalRun)
}

/// The `Draft` spec — `ta_changeset::draft_package::DraftPackage`, the
/// top-level human-review deliverable.
pub fn draft_schema() -> Schema {
    schemars::schema_for!(ta_changeset::draft_package::DraftPackage)
}

/// The `Artifact` spec — `ta_changeset::draft_package::Artifact`, a single
/// changed file/resource within a `Draft`.
pub fn artifact_schema() -> Schema {
    schemars::schema_for!(ta_changeset::draft_package::Artifact)
}

/// The `TriggerEvent` spec — `ta_intake::TriggerEvent`, the normalized
/// payload every trigger type produces.
pub fn trigger_event_schema() -> Schema {
    schemars::schema_for!(ta_intake::TriggerEvent)
}

/// The `RoutingDecision` spec — `ta_brain::RoutingDecision`, the output of
/// `ta_brain::route()`.
pub fn routing_decision_schema() -> Schema {
    schemars::schema_for!(ta_brain::RoutingDecision)
}

/// The `Persona` spec — `ta_goal::PersonaConfig`, the full `.ta/personas/<name>.toml` shape.
pub fn persona_schema() -> Schema {
    schemars::schema_for!(ta_goal::PersonaConfig)
}

/// Looks up a published spec's current version by name (e.g. `"draft"`,
/// `"goal"`). Lets API responses that intentionally expose a spec type
/// verbatim (see `ta-daemon`'s `DraftDetailResponse`) stamp a `schema_version`
/// field from the single source of truth instead of a hand-copied literal.
///
/// # Panics
/// Panics if `name` isn't a published spec — this is a programmer error
/// (typo'd spec name), not a runtime condition to recover from.
pub fn version_of(name: &str) -> u32 {
    SPECS
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no published spec named '{name}'"))
        .version
}

/// Renders a [`SpecEntry`]'s schema to a JSON `Value` with its `$id` and
/// explicit version stamped in, ready to write to `schema/<file>`.
pub fn render(entry: &SpecEntry) -> Value {
    let mut schema = (entry.generate)();
    schema.insert(
        "$id".to_string(),
        Value::String(format!(
            "https://trustedautonomy.dev/schema/{}.v{}.schema.json",
            entry.name, entry.version
        )),
    );
    schema.insert(
        "x-ta-schema-version".to_string(),
        Value::Number(entry.version.into()),
    );
    schema.to_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_generates_an_object_schema() {
        for entry in SPECS {
            let value = render(entry);
            assert!(
                value.is_object(),
                "spec '{}' did not render to a JSON object",
                entry.name
            );
            assert_eq!(
                value.get("x-ta-schema-version").and_then(Value::as_u64),
                Some(entry.version as u64),
                "spec '{}' missing/incorrect x-ta-schema-version",
                entry.name
            );
        }
    }

    #[test]
    fn spec_names_and_files_are_unique() {
        let mut names: Vec<&str> = SPECS.iter().map(|e| e.name).collect();
        let mut files: Vec<&str> = SPECS.iter().map(|e| e.file).collect();
        let names_len = names.len();
        let files_len = files.len();
        names.sort_unstable();
        names.dedup();
        files.sort_unstable();
        files.dedup();
        assert_eq!(names.len(), names_len, "duplicate spec name in SPECS");
        assert_eq!(files.len(), files_len, "duplicate spec file in SPECS");
    }
}
