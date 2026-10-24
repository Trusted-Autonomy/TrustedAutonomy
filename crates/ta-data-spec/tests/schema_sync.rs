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

/// Every installer-facing JSON document declares its `schema_version` as a
/// required field, so a consumer can refuse a shape it does not know.
#[test]
fn installer_documents_require_a_schema_version() {
    for name in [
        "init_report",
        "vcs_setup_report",
        "capabilities",
        "terms_status",
        "daemon_report",
    ] {
        let entry = ta_data_spec::SPECS
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no spec named {name}"));
        let schema = ta_data_spec::render(entry);
        let required = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} has no `required` list"));
        assert!(
            required.iter().any(|r| r == "schema_version"),
            "{name} must require schema_version: {required:?}"
        );
        assert!(
            schema["properties"]["schema_version"].is_object(),
            "{name} must declare schema_version"
        );
    }
}

/// The probe document the daemon prints is versioned by `probe_schema`.
#[test]
fn the_daemon_probe_requires_its_probe_schema() {
    let entry = ta_data_spec::SPECS
        .iter()
        .find(|e| e.name == "daemon_probe")
        .unwrap();
    let schema = ta_data_spec::render(entry);
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "probe_schema"));
}
