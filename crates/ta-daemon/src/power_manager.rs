// power_manager.rs — Platform-specific power assertion for active goals (v0.13.1.1).
//
// Holds a power assertion (preventing idle sleep and App Nap) while any goal
// is in the Running state. Released immediately when no goals are running.
//
// Platform implementations:
//   macOS:   `caffeinate -i -s`  — prevents idle sleep and disk sleep, also
//             suppresses App Nap for the caffeinate subprocess's parent group.
//   Linux:   `systemd-inhibit --what=idle:sleep --who=ta-daemon
//              --why="goal in progress" sleep infinity`
//   Windows: No subprocess — uses a state file approach (future: SetThreadExecutionState).
//
// The assertion is held by keeping a child process alive. When released, the
// child is killed. The `PowerManager` is safe to share across threads via Arc.

use std::sync::{Arc, Mutex};

use crate::config::PowerConfig;

/// Shared handle to the power manager. Clone-cheap via `Arc`.
pub type SharedPowerManager = Arc<PowerManager>;

/// Manages the lifecycle of a platform power assertion.
///
/// Create once at daemon startup and share via `Arc`. Call `update(running_goals)`
/// each watchdog cycle to hold or release the assertion based on active goal count.
pub struct PowerManager {
    config: PowerConfig,
    state: Mutex<PowerState>,
}

struct PowerState {
    /// The assertion child process (if held).
    child: Option<std::process::Child>,
    /// Whether the assertion is currently active.
    active: bool,
}

impl PowerManager {
    /// Create a new power manager (assertion not yet held).
    pub fn new(config: PowerConfig) -> Self {
        Self {
            config,
            state: Mutex::new(PowerState {
                child: None,
                active: false,
            }),
        }
    }

    /// Create a no-op power manager (policy disabled).
    pub fn disabled() -> Self {
        Self::new(PowerConfig {
            prevent_sleep_during_active_goals: false,
            ..PowerConfig::default()
        })
    }

    /// Update assertion state based on the current number of running goals.
    ///
    /// - `running_goals > 0` → hold assertion if not already held
    /// - `running_goals == 0` → release assertion if held
    pub fn update(&self, running_goals: usize) {
        if !self.config.prevent_sleep_during_active_goals {
            return;
        }

        let mut state = self.state.lock().unwrap();
        if running_goals > 0 && !state.active {
            match Self::spawn_assertion() {
                Ok(child) => {
                    tracing::info!(
                        goals = running_goals,
                        "Power assertion held — idle sleep prevented during active goal"
                    );
                    state.child = Some(child);
                    state.active = true;
                }
                Err(e) => {
                    // Non-fatal: log and continue without the assertion.
                    tracing::warn!("Failed to hold power assertion (goal will continue): {}", e);
                }
            }
        } else if running_goals == 0 && state.active {
            Self::release_assertion(&mut state);
            tracing::info!("Power assertion released — no active goals");
        }
    }

    /// Returns true if a power assertion is currently held.
    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().active
    }

    /// Spawn the platform-specific assertion process.
    fn spawn_assertion() -> std::io::Result<std::process::Child> {
        #[cfg(target_os = "macos")]
        {
            // caffeinate -i prevents idle sleep; -s prevents display sleep too.
            // We don't use -d (display) to avoid locking the screen.
            // caffeinate exits when its parent exits, or can be killed explicitly.
            std::process::Command::new("caffeinate")
                .args(["-i", "-s"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        }
        #[cfg(target_os = "linux")]
        {
            // systemd-inhibit holds an inhibitor lock. We use `sleep infinity`
            // as the guarded command so it stays alive until we kill it.
            std::process::Command::new("systemd-inhibit")
                .args([
                    "--what=idle:sleep",
                    "--who=ta-daemon",
                    "--why=goal in progress",
                    "--mode=block",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        }
        #[cfg(target_os = "windows")]
        {
            // On Windows we spawn a minimal process that holds ES_SYSTEM_REQUIRED.
            // For now, return a "not supported" error — the goal still runs.
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Power assertion not yet implemented on Windows (goal will continue without it)",
            ))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Power assertion not supported on this platform",
            ))
        }
    }

    /// Kill the assertion child process and reset state.
    fn release_assertion(state: &mut PowerState) {
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait(); // Reap to avoid zombies.
        }
        state.active = false;
    }
}

impl Drop for PowerManager {
    /// Release the assertion when the manager is dropped (daemon shutdown).
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        if state.active {
            Self::release_assertion(&mut state);
            tracing::info!("Power assertion released on daemon shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_manager_disabled_by_config() {
        let manager = PowerManager::disabled();
        manager.update(5); // Would hold assertion if enabled.
        assert!(
            !manager.is_active(),
            "Should not hold assertion when disabled"
        );
    }

    #[test]
    fn power_manager_no_assertion_at_zero_goals() {
        // With policy enabled but no goals, assertion should not be held.
        let config = PowerConfig::default(); // prevent_sleep = true
        let manager = PowerManager::new(config);
        assert!(!manager.is_active());
        // Update with 0 goals — should not start assertion.
        manager.update(0);
        assert!(!manager.is_active());
    }

    #[test]
    fn power_manager_releases_on_drop() {
        // This is a compile-time / logic test — we verify Drop is implemented.
        // Can't fully test without a running assertion, but we verify the path.
        let manager = PowerManager::disabled();
        assert!(!manager.is_active());
        drop(manager); // Should not panic.
    }

    #[test]
    fn power_manager_is_active_reflects_state() {
        let manager = PowerManager::disabled();
        assert!(!manager.is_active());
        // Even with goals, disabled manager stays inactive.
        manager.update(3);
        assert!(!manager.is_active());
    }
}
