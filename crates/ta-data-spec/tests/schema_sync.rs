//! Guards against schema drift: the checked-in `schema/*.schema.json` files
//! must match what the current Rust types actually generate. If this test
//! fails, run `cargo run -p ta-data-spec --bin gen-schema` and commit the
//! result.

use std::path::PathBuf;

fn workspace_schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .canonicalize()
        .expect("workspace schema/ directory must exist")
}

#[test]
fn checked_in_schemas_match_generated_output() {
    let dir = workspace_schema_dir();
    for entry in ta_data_spec::SPECS {
        let generated = ta_data_spec::render(entry);
        let path = dir.join(entry.file);
        let on_disk_text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "could not read {} ({}) — run `cargo run -p ta-data-spec --bin gen-schema`",
                path.display(),
                e
            )
        });
        let on_disk: serde_json::Value =
            serde_json::from_str(&on_disk_text).expect("checked-in schema file must be valid JSON");
        assert_eq!(
            generated, on_disk,
            "schema/{} is out of sync with the current `{}` type — \
             run `cargo run -p ta-data-spec --bin gen-schema` and commit the result",
            entry.file, entry.name
        );
    }
}
