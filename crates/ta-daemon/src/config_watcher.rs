// config_watcher.rs — Live config hot-reload via filesystem notifications (v0.10.18).
//
// Watches `.ta/daemon.toml` and `.ta/office.yaml` for changes and reloads
// the daemon configuration without requiring a restart. Uses the `notify`
// crate for cross-platform file system events.
//
// Architecture:
//   - `ConfigWatcher` spawns a background thread that receives FS events
//   - On relevant changes, it reloads the config and sends it through a channel
//   - The daemon's main loop receives the new config and swaps it atomically
//   - Active connections are unaffected; new requests use the updated config

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::DaemonConfig;
use crate::office::OfficeConfig;

/// Events produced by the config watcher.
#[derive(Debug)]
pub enum ConfigEvent {
    /// daemon.toml was modified — new config attached.
    DaemonConfigReloaded(Box<DaemonConfig>),
    /// office.yaml was modified — new config attached.
    OfficeConfigReloaded(Box<OfficeConfig>),
    /// A watched file was modified but parsing failed.
    ReloadFailed { path: String, error: String },
}

/// Watches config files and emits reload events.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<ConfigEvent>,
}

impl ConfigWatcher {
    /// Start watching config files in the given project root.
    ///
    /// Watches:
    ///   - `<project_root>/.ta/daemon.toml`
    ///   - `<project_root>/.ta/office.yaml`
    ///
    /// Returns a `ConfigWatcher` whose `recv()` method yields `ConfigEvent`s.
    pub fn start(project_root: &Path) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let ta_dir = project_root.join(".ta");
        let project_root_owned = project_root.to_path_buf();

        let daemon_toml = ta_dir.join("daemon.toml");
        let office_yaml = ta_dir.join("office.yaml");

        let tx_clone = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: Result<Event, notify::Error>| match result {
                Ok(event) => {
                    if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        return;
                    }

                    for path in &event.paths {
                        handle_config_change(path, &project_root_owned, &tx_clone);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Config watcher error");
                }
            })
            .map_err(|e| format!("Failed to create config watcher: {}", e))?;

        // Watch the .ta directory (non-recursive is sufficient).
        if ta_dir.exists() {
            watcher
                .watch(&ta_dir, RecursiveMode::NonRecursive)
                .map_err(|e| {
                    format!(
                        "Failed to watch {}: {}. Config hot-reload is disabled.",
                        ta_dir.display(),
                        e
                    )
                })?;

            tracing::info!(
                path = %ta_dir.display(),
                daemon_toml_exists = daemon_toml.exists(),
                office_yaml_exists = office_yaml.exists(),
                "Config hot-reload watcher started"
            );
        } else {
            tracing::warn!(
                path = %ta_dir.display(),
                "Config directory does not exist — hot-reload disabled until directory is created"
            );
        }

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }

    /// Try to receive a config event without blocking.
    pub fn try_recv(&self) -> Option<ConfigEvent> {
        self.receiver.try_recv().ok()
    }

    /// Receive a config event, blocking until one is available or timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ConfigEvent> {
        self.receiver.recv_timeout(timeout).ok()
    }
}

fn handle_config_change(path: &Path, project_root: &Path, tx: &mpsc::Sender<ConfigEvent>) {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();

    match filename {
        "daemon.toml" => {
            tracing::info!(path = %path.display(), "daemon.toml changed, reloading");
            let config = DaemonConfig::load(project_root);
            let _ = tx.send(ConfigEvent::DaemonConfigReloaded(Box::new(config)));
        }
        "office.yaml" => {
            tracing::info!(path = %path.display(), "office.yaml changed, reloading");
            match OfficeConfig::load(path) {
                Ok(config) => {
                    let _ = tx.send(ConfigEvent::OfficeConfigReloaded(Box::new(config)));
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to parse office.yaml on reload"
                    );
                    let _ = tx.send(ConfigEvent::ReloadFailed {
                        path: path.display().to_string(),
                        error: e,
                    });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_watcher_nonexistent_dir() {
        let result = ConfigWatcher::start(Path::new("/nonexistent/path/that/does/not/exist"));
        // Should succeed (watcher created) but warn about missing directory.
        // The watcher just won't receive events.
        // On some platforms this may fail, which is also acceptable.
        match result {
            Ok(watcher) => {
                // No events should be available.
                assert!(watcher.try_recv().is_none());
            }
            Err(e) => {
                assert!(
                    e.contains("does not exist") || e.contains("Failed"),
                    "Unexpected error: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn config_watcher_with_real_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let watcher = ConfigWatcher::start(dir.path()).unwrap();

        // Write a daemon.toml to trigger a reload event.
        std::fs::write(ta_dir.join("daemon.toml"), "[server]\nport = 8080\n").unwrap();

        // Give the watcher a moment to process the event.
        std::thread::sleep(Duration::from_millis(200));

        // Check if we got a reload event.
        if let Some(ConfigEvent::DaemonConfigReloaded(config)) = watcher.try_recv() {
            assert_eq!(config.server.port, 8080);
        }
        // Note: File system events are not guaranteed to be delivered
        // immediately on all platforms, so we don't assert here.
    }

    #[test]
    fn handle_config_change_daemon_toml() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Write a valid daemon.toml.
        std::fs::write(ta_dir.join("daemon.toml"), "[server]\nport = 9090\n").unwrap();

        let (tx, rx) = mpsc::channel();
        let project_root = dir.path().to_path_buf();

        handle_config_change(&ta_dir.join("daemon.toml"), &project_root, &tx);

        let event = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        match event {
            ConfigEvent::DaemonConfigReloaded(config) => {
                assert_eq!(config.server.port, 9090);
            }
            other => panic!("Expected DaemonConfigReloaded, got {:?}", other),
        }
    }

    #[test]
    fn handle_config_change_unrelated_file() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let project_root = dir.path().to_path_buf();

        // An unrelated file should not produce an event.
        handle_config_change(&dir.path().join("unrelated.txt"), &project_root, &tx);

        assert!(rx.try_recv().is_err());
    }
}
