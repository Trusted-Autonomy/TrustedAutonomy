//! Proves the Plugin-category unification (v0.17.0.12.14 item 6): one
//! synthetic, community-authored-style plugin script is discovered and
//! invoked identically through two independent Plugin-category integrations
//! (VCS and agent-runtime) via the shared ta-plugin transport.

#![cfg(unix)]

use std::path::Path;

fn write_synthetic_plugin(dir: &Path) -> String {
    let path = dir.join("synthetic-plugin.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"9.9.9","protocol_version":1,"adapter_name":"synthetic","capabilities":["handshake"]}}'
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn synthetic_plugin_works_identically_via_vcs_and_runtime_integrations() {
    let dir = tempfile::tempdir().unwrap();
    let command = write_synthetic_plugin(dir.path());

    // Integration 1: VCS — per-call spawn via ta_plugin::transport::call_json.
    let vcs_manifest = ta_submit::vcs_plugin_manifest::VcsPluginManifest {
        name: "synthetic".to_string(),
        version: "0.1.0".to_string(),
        plugin_type: "vcs".to_string(),
        command: command.clone(),
        args: vec![],
        capabilities: vec![],
        description: None,
        timeout_secs: 5,
        min_daemon_version: None,
        source_url: None,
        staging_env: Default::default(),
    };
    let vcs_adapter = ta_submit::external_vcs_adapter::ExternalVcsAdapter::new(
        &vcs_manifest,
        dir.path(),
        "0.17.0-test",
    )
    .expect("VCS handshake via synthetic plugin should succeed");
    assert_eq!(vcs_adapter.plugin_version(), "9.9.9");

    // Integration 2: agent-runtime — long-lived spawn, line framing via
    // ta_plugin::transport::write_line/read_line.
    let runtime_adapter =
        ta_runtime::plugin::ExternalRuntimeAdapter::new(Path::new(&command), "synthetic")
            .expect("runtime handshake via the same synthetic plugin should succeed");
    assert_eq!(runtime_adapter.plugin_version(), "9.9.9");
}
