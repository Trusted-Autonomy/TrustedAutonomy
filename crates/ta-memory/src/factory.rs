//! Config-driven factory for memory backend selection.
//!
//! `memory_store_from_config()` reads `.ta/memory.toml` and instantiates the
//! appropriate `MemoryStore` backend:
//!
//! | `[memory] backend` | Store returned                          |
//! |--------------------|-----------------------------------------|
//! | `"file"` or absent | `FsMemoryStore` (always available)      |
//! | `"ruvector"`       | `RuVectorStore` (feature-gated)         |
//! | `"plugin"`         | `ExternalMemoryAdapter` (via discovery) |
//!
//! Falls back to `FsMemoryStore` when the preferred backend is unavailable
//! (e.g., ruvector feature is off, or the plugin binary is not found).
//!
//! ## Config (`.ta/memory.toml`)
//!
//! ```toml
//! backend = "plugin"
//! plugin  = "supermemory"      # binary searched as ta-memory-supermemory
//!
//! # Or built-in backends:
//! # backend = "file"            # default — FsMemoryStore
//! # backend = "ruvector"        # HNSW — RuVectorStore (feature-gated)
//! ```

use std::path::Path;

use crate::error::MemoryError;
use crate::fs_store::FsMemoryStore;
use crate::key_schema::load_memory_config;
use crate::plugin_manifest::find_memory_plugin;
use crate::store::MemoryStore;

/// TA version embedded in the binary, used for plugin handshakes.
const TA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create a `MemoryStore` appropriate for the given project root.
///
/// Reads `[memory]` config from `.ta/memory.toml`.  Returns `FsMemoryStore`
/// on any error or when the preferred backend is not available.  Never panics.
///
/// # Example
///
/// ```no_run
/// use ta_memory::memory_store_from_config;
/// use ta_memory::MemoryStore;
///
/// let mut store = memory_store_from_config(std::path::Path::new("."));
/// // store is Box<dyn MemoryStore> — use it like any other backend
/// ```
pub fn memory_store_from_config(project_root: &Path) -> Box<dyn MemoryStore> {
    let config = load_memory_config(project_root);
    let backend = config.backend.as_deref().unwrap_or("file");

    match backend {
        "plugin" => {
            let plugin_name = match &config.plugin {
                Some(n) if !n.is_empty() => n.clone(),
                _ => {
                    tracing::warn!(
                        "memory backend = \"plugin\" but no plugin name configured. \
                         Set `plugin = \"<name>\"` in .ta/memory.toml. \
                         Falling back to FsMemoryStore."
                    );
                    return fs_fallback(project_root);
                }
            };

            match find_memory_plugin(&plugin_name, project_root) {
                Some(discovered) => {
                    match crate::external_adapter::ExternalMemoryAdapter::new(
                        &discovered.manifest,
                        project_root,
                        TA_VERSION,
                    ) {
                        Ok(adapter) => {
                            tracing::info!(
                                plugin = %plugin_name,
                                source = %discovered.source,
                                "Using external memory plugin backend"
                            );
                            Box::new(adapter)
                        }
                        Err(e) => {
                            tracing::warn!(
                                plugin = %plugin_name,
                                error = %e,
                                "Failed to initialize memory plugin — falling back to FsMemoryStore"
                            );
                            fs_fallback(project_root)
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        plugin = %plugin_name,
                        "Memory plugin '{}' not found in .ta/plugins/memory/, \
                         ~/.config/ta/plugins/memory/, or $PATH (as ta-memory-{}). \
                         Install the plugin or check the name. \
                         Falling back to FsMemoryStore.",
                        plugin_name, plugin_name
                    );
                    fs_fallback(project_root)
                }
            }
        }
        "ruvector" => {
            #[cfg(feature = "ruvector")]
            {
                let rvf_path = project_root.join(".ta").join("memory.rvf");
                match crate::ruvector_store::RuVectorStore::open(&rvf_path) {
                    Ok(store) => {
                        tracing::debug!("Using RuVectorStore backend (configured)");
                        return Box::new(store);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %rvf_path.display(),
                            error = %e,
                            "Failed to open RuVectorStore — falling back to FsMemoryStore"
                        );
                    }
                }
            }
            #[cfg(not(feature = "ruvector"))]
            {
                tracing::warn!(
                    "memory backend = \"ruvector\" but ruvector feature is not compiled in. \
                     Rebuild with --features ruvector. Falling back to FsMemoryStore."
                );
            }
            fs_fallback(project_root)
        }
        // "file" or any unknown value → FsMemoryStore.
        _ => fs_fallback(project_root),
    }
}

/// Try to open the configured backend, returning `Err` (with an actionable message) if it fails.
///
/// Unlike `memory_store_from_config`, this does NOT fall back to FsMemoryStore — useful for
/// commands that want to explicitly report misconfiguration rather than silently degrading.
pub fn memory_store_strict(project_root: &Path) -> Result<Box<dyn MemoryStore>, MemoryError> {
    let config = load_memory_config(project_root);
    let backend = config.backend.as_deref().unwrap_or("file");

    match backend {
        "plugin" => {
            let plugin_name = config
                .plugin
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MemoryError::Plugin(
                        "memory backend = \"plugin\" but no plugin name configured. \
                     Set `plugin = \"<name>\"` in .ta/memory.toml."
                            .to_string(),
                    )
                })?;

            let discovered = find_memory_plugin(plugin_name, project_root).ok_or_else(|| {
                MemoryError::Plugin(format!(
                    "memory plugin '{}' not found. Search paths: \
                     .ta/plugins/memory/{name}/, ~/.config/ta/plugins/memory/{name}/, \
                     ta-memory-{name} on $PATH.",
                    plugin_name,
                    name = plugin_name
                ))
            })?;

            let adapter = crate::external_adapter::ExternalMemoryAdapter::new(
                &discovered.manifest,
                project_root,
                TA_VERSION,
            )?;
            Ok(Box::new(adapter))
        }
        "ruvector" => {
            #[cfg(feature = "ruvector")]
            {
                let rvf_path = project_root.join(".ta").join("memory.rvf");
                let store = crate::ruvector_store::RuVectorStore::open(&rvf_path)?;
                Ok(Box::new(store))
            }
            #[cfg(not(feature = "ruvector"))]
            {
                Err(MemoryError::Plugin(
                    "memory backend = \"ruvector\" but ruvector feature is not compiled in. \
                     Rebuild with --features ruvector."
                        .to_string(),
                ))
            }
        }
        _ => Ok(fs_fallback(project_root)),
    }
}

fn fs_fallback(project_root: &Path) -> Box<dyn MemoryStore> {
    let memory_dir = project_root.join(".ta").join("memory");
    Box::new(FsMemoryStore::new(&memory_dir))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_config_returns_fs_store() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        // No memory.toml → should return FsMemoryStore without error.
        let store = memory_store_from_config(dir.path());
        drop(store); // Should not panic.
    }

    #[test]
    fn explicit_file_backend_returns_fs_store() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("memory.toml"),
            "backend = \"file\"\n",
        )
        .unwrap();
        let store = memory_store_from_config(dir.path());
        drop(store);
    }

    #[test]
    fn plugin_backend_without_plugin_field_falls_back() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("memory.toml"),
            "backend = \"plugin\"\n",
        )
        .unwrap();
        // No plugin name → should fall back to FsMemoryStore without panicking.
        let store = memory_store_from_config(dir.path());
        drop(store);
    }

    #[test]
    fn plugin_backend_missing_binary_falls_back() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("memory.toml"),
            "backend = \"plugin\"\nplugin = \"nonexistent-xyz-abc\"\n",
        )
        .unwrap();
        // Plugin not found → should fall back without panicking.
        let store = memory_store_from_config(dir.path());
        drop(store);
    }

    #[test]
    fn strict_missing_plugin_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("memory.toml"),
            "backend = \"plugin\"\nplugin = \"nonexistent-xyz-abc\"\n",
        )
        .unwrap();
        let result = memory_store_strict(dir.path());
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("plugin"),
            "Expected plugin error, got: {}",
            msg
        );
    }

    #[test]
    fn strict_no_plugin_name_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("memory.toml"),
            "backend = \"plugin\"\n",
        )
        .unwrap();
        let result = memory_store_strict(dir.path());
        assert!(result.is_err(), "expected error for missing plugin name");
    }
}
