//! Regenerates the checked-in JSON Schema files under `schema/` from the
//! current Rust types. Run after changing any of the five spec types
//! (`GoalRun`, `DraftPackage`/`Artifact`, `TriggerEvent`, `RoutingDecision`,
//! `PersonaConfig`) — `ta-data-spec`'s `tests/round_trip.rs` fails CI if the
//! checked-in files drift from what this binary would produce.
//!
//! Usage: `cargo run -p ta-data-spec --bin gen-schema`

use std::path::PathBuf;

fn workspace_schema_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ta-data-spec; the workspace root's
    // schema/ dir is two levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .canonicalize()
        .expect("workspace schema/ directory must exist")
}

fn main() {
    let dir = workspace_schema_dir();
    for entry in ta_data_spec::SPECS {
        let value = ta_data_spec::render(entry);
        let json = serde_json::to_string_pretty(&value).expect("schema serializes to JSON");
        let path = dir.join(entry.file);
        std::fs::write(&path, format!("{json}\n")).unwrap_or_else(|e| {
            panic!("failed to write {}: {}", path.display(), e);
        });
        println!("wrote {} (v{})", path.display(), entry.version);
    }
}
