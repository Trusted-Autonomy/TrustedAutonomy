//! `ta build` — Run project build/test through the BuildAdapter.
//!
//! Calls the configured `BuildAdapter::build()` (and optionally `test()`),
//! emits `build_completed` or `build_failed` events through the TA event system,
//! and exits with the build tool's exit code.

use ta_build::{select_build_adapter, BuildAdapterConfig};
use ta_mcp_gateway::GatewayConfig;
use ta_submit::WorkflowConfig;

/// Execute `ta build`.
///
/// Resolves the build adapter from `.ta/workflow.toml`, calls `build()` and
/// optionally `test()`, emits events, and exits with the build result.
pub fn execute(config: &GatewayConfig, run_test: bool) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;

    // Load workflow config.
    let workflow_config_path = project_root.join(".ta/workflow.toml");
    let workflow_config = WorkflowConfig::load_or_default(&workflow_config_path);

    // Build adapter config from workflow.toml [build] section.
    let adapter_config = BuildAdapterConfig {
        adapter: workflow_config.build.adapter.clone(),
        command: workflow_config.build.command.clone(),
        test_command: workflow_config.build.test_command.clone(),
        webhook_url: workflow_config.build.webhook_url.clone(),
    };

    // Select adapter.
    let adapter = match select_build_adapter(project_root, &adapter_config) {
        Some(a) => a,
        None => {
            eprintln!(
                "No build adapter detected or configured.\n\n\
                 TA looks for Cargo.toml (Rust), package.json (Node.js), or Makefile.\n\
                 You can also configure a custom build command in .ta/workflow.toml:\n\n\
                 [build]\n\
                 adapter = \"script\"\n\
                 command = \"your-build-command\"\n\
                 test_command = \"your-test-command\"\n\n\
                 Known adapters: cargo, npm, script, webhook, auto, none"
            );
            return Err(anyhow::anyhow!(
                "No build adapter available. \
                 Configure [build].adapter in .ta/workflow.toml."
            ));
        }
    };

    println!("Building with {} adapter...", adapter.name());

    // Run build.
    match adapter.build() {
        Ok(result) => {
            if result.success {
                println!("\n[ok] Build succeeded ({:.1}s)", result.duration_secs);
                emit_build_event(config, adapter.name(), "build", &result);
            } else {
                eprintln!(
                    "\n[fail] Build failed (exit code {}, {:.1}s)",
                    result.exit_code, result.duration_secs
                );
                if !result.stderr.is_empty() {
                    let lines: Vec<&str> = result.stderr.lines().collect();
                    let show = if lines.len() > 40 {
                        let head: Vec<&str> = lines[..20].to_vec();
                        let tail: Vec<&str> = lines[lines.len() - 20..].to_vec();
                        format!(
                            "{}\n  ... ({} lines omitted) ...\n{}",
                            head.join("\n"),
                            lines.len() - 40,
                            tail.join("\n")
                        )
                    } else {
                        result.stderr.clone()
                    };
                    eprintln!("\n{}", show);
                }
                emit_build_failed_event(config, adapter.name(), "build", &result);

                if !run_test {
                    return Err(anyhow::anyhow!(
                        "Build failed with exit code {}. \
                         Fix the errors above and run `ta build` again.",
                        result.exit_code
                    ));
                }
            }
        }
        Err(e) => {
            eprintln!("\nBuild error: {}", e);
            return Err(anyhow::anyhow!("Build error: {}", e));
        }
    }

    // Optionally run tests.
    if run_test {
        println!("\nRunning tests with {} adapter...", adapter.name());

        match adapter.test() {
            Ok(result) => {
                if result.success {
                    println!("\n[ok] Tests passed ({:.1}s)", result.duration_secs);
                    emit_build_event(config, adapter.name(), "test", &result);
                } else {
                    eprintln!(
                        "\n[fail] Tests failed (exit code {}, {:.1}s)",
                        result.exit_code, result.duration_secs
                    );
                    if !result.stderr.is_empty() {
                        let lines: Vec<&str> = result.stderr.lines().collect();
                        let show = if lines.len() > 40 {
                            let head: Vec<&str> = lines[..20].to_vec();
                            let tail: Vec<&str> = lines[lines.len() - 20..].to_vec();
                            format!(
                                "{}\n  ... ({} lines omitted) ...\n{}",
                                head.join("\n"),
                                lines.len() - 40,
                                tail.join("\n")
                            )
                        } else {
                            result.stderr.clone()
                        };
                        eprintln!("\n{}", show);
                    }
                    if !result.stdout.is_empty() {
                        let lines: Vec<&str> = result.stdout.lines().collect();
                        let show = if lines.len() > 40 {
                            let head: Vec<&str> = lines[..20].to_vec();
                            let tail: Vec<&str> = lines[lines.len() - 20..].to_vec();
                            format!(
                                "{}\n  ... ({} lines omitted) ...\n{}",
                                head.join("\n"),
                                lines.len() - 40,
                                tail.join("\n")
                            )
                        } else {
                            result.stdout.clone()
                        };
                        eprintln!("\n{}", show);
                    }
                    emit_build_failed_event(config, adapter.name(), "test", &result);
                    return Err(anyhow::anyhow!(
                        "Tests failed with exit code {}. \
                         Fix the failures above and run `ta build --test` again.",
                        result.exit_code
                    ));
                }
            }
            Err(e) => {
                eprintln!("\nTest error: {}", e);
                return Err(anyhow::anyhow!("Test error: {}", e));
            }
        }
    }

    Ok(())
}

/// Emit a `build_completed` event through the TA event store.
fn emit_build_event(
    config: &GatewayConfig,
    adapter_name: &str,
    operation: &str,
    result: &ta_build::BuildResult,
) {
    use ta_events::schema::{EventEnvelope, SessionEvent};
    use ta_events::store::{EventStore, FsEventStore};

    let event = SessionEvent::BuildCompleted {
        adapter: adapter_name.to_string(),
        operation: operation.to_string(),
        duration_secs: result.duration_secs,
        message: format!(
            "{} {} succeeded in {:.1}s",
            adapter_name, operation, result.duration_secs
        ),
    };

    let events_dir = config.workspace_root.join(".ta").join("events");
    let store = FsEventStore::new(&events_dir);
    if let Err(e) = store.append(&EventEnvelope::new(event)) {
        tracing::warn!(error = %e, "Failed to store build_completed event");
    }
}

/// Emit a `build_failed` event through the TA event store.
fn emit_build_failed_event(
    config: &GatewayConfig,
    adapter_name: &str,
    operation: &str,
    result: &ta_build::BuildResult,
) {
    use ta_events::schema::{EventEnvelope, SessionEvent};
    use ta_events::store::{EventStore, FsEventStore};

    let event = SessionEvent::BuildFailed {
        adapter: adapter_name.to_string(),
        operation: operation.to_string(),
        exit_code: result.exit_code,
        duration_secs: result.duration_secs,
        message: format!(
            "{} {} failed (exit code {}) in {:.1}s",
            adapter_name, operation, result.exit_code, result.duration_secs
        ),
    };

    let events_dir = config.workspace_root.join(".ta").join("events");
    let store = FsEventStore::new(&events_dir);
    if let Err(e) = store.append(&EventEnvelope::new(event)) {
        tracing::warn!(error = %e, "Failed to store build_failed event");
    }
}

#[cfg(test)]
mod tests {
    use ta_build::{select_build_adapter, BuildAdapterConfig};
    use tempfile::tempdir;

    #[test]
    fn select_cargo_adapter_for_rust_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"t\"").unwrap();
        let config = BuildAdapterConfig::default();
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn select_npm_adapter_for_node_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let config = BuildAdapterConfig::default();
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "npm");
    }

    #[test]
    fn no_adapter_for_empty_project() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig::default();
        assert!(select_build_adapter(dir.path(), &config).is_none());
    }

    #[test]
    fn script_adapter_runs_build() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "script".to_string(),
            command: Some("echo built".to_string()),
            test_command: Some("echo tested".to_string()),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        let result = adapter.build().unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("built"));
    }

    #[test]
    fn script_adapter_runs_test() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "script".to_string(),
            command: Some("echo built".to_string()),
            test_command: Some("echo tested".to_string()),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        let result = adapter.test().unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("tested"));
    }
}
