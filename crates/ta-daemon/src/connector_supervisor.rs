// connector_supervisor.rs — Fault-isolated plugin process supervisor (v0.17.0.6).
//
// Manages user-defined "supervised" connectors declared in `.ta/workflow.toml`
// under `[[connectors.managed]]`.  Each connector runs as its own subprocess;
// a crash or hang cannot starve the daemon's HTTP handlers or accumulate
// unbounded event backlogs.
//
// ## Protocol (connector side)
//   - Write/touch `.ta/connectors/<name>/heartbeat` every ≤30s while healthy.
//   - On clean exit write `{"status":"stopped","reason":"..."}` to `status.json`.
//   - On error write `{"status":"error","reason":"..."}` before crashing.
//
// ## State files written by the supervisor (readable by `ta connector *` CLI)
//   - `.ta/connectors/<name>/supervisor-status.json` — current handle state
//   - `.ta/connectors/<name>/restart-signal`         — CLI writes this to clear Suspended
//
// ## Backoff model
//   1s → 2s → 4s → 8s → … → 60s cap, reset on successful heartbeat.
//   After 5 failures within a 5-minute window the connector is **Suspended**:
//   no further restarts until the operator runs `ta connector restart <name>`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

// ─── Connector config from workflow.toml ─────────────────────────────────────

/// A single connector entry parsed from `[[connectors.managed]]`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ManagedConnectorEntry {
    /// Connector name (used for file paths and CLI display).
    pub name: String,
    /// Binary to spawn.
    pub command: String,
    /// Additional arguments passed to the binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether this connector is currently enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Extra environment variables (merged into the daemon's environment).
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

/// Parse `[[connectors.managed]]` from a `workflow.toml` string.
///
/// Returns an empty list if the section is absent or unparseable.
pub fn load_managed_connectors(workflow_toml: &str) -> Vec<ManagedConnectorEntry> {
    #[derive(Deserialize, Default)]
    struct Root {
        #[serde(default)]
        connectors: Connectors,
    }
    #[derive(Deserialize, Default)]
    struct Connectors {
        #[serde(default)]
        managed: Vec<ManagedConnectorEntry>,
    }

    toml::from_str::<Root>(workflow_toml)
        .map(|r| r.connectors.managed)
        .unwrap_or_default()
}

// ─── Backoff state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackoffState {
    /// Active: next restart will use `delay`.
    Active,
    /// Suspended: requires `ta connector restart <name>` to resume.
    Suspended,
}

// ─── Per-connector status (written to disk) ───────────────────────────────────

/// Status written to `.ta/connectors/<name>/supervisor-status.json`.
/// Read by `ta connector list` and `ta connector status <name>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorSupervisorStatus {
    pub name: String,
    /// "running", "stopped", "suspended", "starting"
    pub status: String,
    pub pid: Option<u32>,
    pub restart_count: u32,
    /// Seconds since the last heartbeat file was touched. None = never seen.
    pub last_heartbeat_secs_ago: Option<u64>,
    pub last_started_at: Option<String>,
    pub updated_at: String,
}

// ─── Event queue ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorEvent {
    pub connector: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub message: String,
}

/// Bounded, TTL-evicted in-memory event queue for a single connector.
pub struct ConnectorEventQueue {
    events: VecDeque<ConnectorEvent>,
    capacity: usize,
}

impl ConnectorEventQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push an event.  Returns `false` (and logs a warn) when the queue is full.
    pub fn push(&mut self, event: ConnectorEvent) -> bool {
        if self.events.len() >= self.capacity {
            tracing::warn!(
                connector = %event.connector,
                capacity = self.capacity,
                "Connector event queue full — dropping event"
            );
            return false;
        }
        self.events.push_back(event);
        true
    }

    /// Evict events older than `max_age_secs`.
    pub fn evict_stale(&mut self, max_age_secs: u64) {
        let threshold = chrono::Utc::now() - chrono::Duration::seconds(max_age_secs as i64);
        self.events.retain(|e| e.at > threshold);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// All per-connector queues, keyed by connector name.
pub type AllQueues = Arc<Mutex<HashMap<String, ConnectorEventQueue>>>;

/// Create a new shared queue registry.
pub fn new_queue_registry() -> AllQueues {
    Arc::new(Mutex::new(HashMap::new()))
}

// ─── Supervisor entry point ───────────────────────────────────────────────────

const EVENT_QUEUE_CAPACITY: usize = 1000;
const EVENT_TTL_SECS: u64 = 600; // 10 minutes
const TTL_EVICTION_INTERVAL_SECS: u64 = 60;
const HEARTBEAT_STALE_SECS: u64 = 90;
const HEARTBEAT_POLL_INTERVAL_SECS: u64 = 10;
const SUSPEND_FAILURE_COUNT: u32 = 5;
const SUSPEND_WINDOW_SECS: u64 = 300; // 5 minutes
const MAX_BACKOFF_SECS: u64 = 60;
const RESTART_SIGNAL_POLL_SECS: u64 = 5;

/// Start the connector supervisor.
///
/// Reads `[[connectors.managed]]` from `.ta/workflow.toml`, spawns a monitoring
/// task per enabled connector, and a single TTL-eviction task for all queues.
/// Returns the shared queue registry for introspection.
pub fn start(project_root: PathBuf, shutdown: Arc<Notify>) -> AllQueues {
    let queues = new_queue_registry();

    // Load connectors.
    let workflow_path = project_root.join(".ta").join("workflow.toml");
    let connectors = if let Ok(raw) = std::fs::read_to_string(&workflow_path) {
        load_managed_connectors(&raw)
    } else {
        vec![]
    };

    let enabled: Vec<_> = connectors.into_iter().filter(|c| c.enabled).collect();

    if enabled.is_empty() {
        return queues;
    }

    tracing::info!(
        count = enabled.len(),
        "ConnectorSupervisor starting {} managed connector(s)",
        enabled.len()
    );

    // Spawn a monitoring task per connector.
    for entry in enabled {
        let name = entry.name.clone();
        let root = project_root.clone();
        let sd = shutdown.clone();
        let q = queues.clone();

        // Pre-create the queue slot.
        {
            let mut lock = q.lock().unwrap();
            lock.insert(name.clone(), ConnectorEventQueue::new(EVENT_QUEUE_CAPACITY));
        }

        tokio::spawn(async move {
            run_connector(root, entry, q, sd).await;
        });
    }

    // TTL-eviction background task — scans all queues every 60s.
    {
        let q = queues.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            run_ttl_eviction(q, sd).await;
        });
    }

    queues
}

/// Background TTL eviction: scans all queues every minute and drops stale events.
/// Holds the lock only during the compact phase (no I/O while locked).
async fn run_ttl_eviction(queues: AllQueues, shutdown: Arc<Notify>) {
    let interval = Duration::from_secs(TTL_EVICTION_INTERVAL_SECS);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.notified() => return,
        }

        let names: Vec<String> = {
            let lock = queues.lock().unwrap();
            lock.keys().cloned().collect()
        };

        for name in names {
            let mut lock = queues.lock().unwrap();
            if let Some(q) = lock.get_mut(&name) {
                let before = q.len();
                q.evict_stale(EVENT_TTL_SECS);
                let evicted = before - q.len();
                if evicted > 0 {
                    tracing::debug!(
                        connector = %name,
                        evicted,
                        "TTL eviction: removed {} stale connector event(s)",
                        evicted
                    );
                }
            }
        }
    }
}

// ─── Per-connector supervisor loop ────────────────────────────────────────────

async fn run_connector(
    project_root: PathBuf,
    entry: ManagedConnectorEntry,
    queues: AllQueues,
    shutdown: Arc<Notify>,
) {
    let name = &entry.name;
    let connector_dir = project_root.join(".ta").join("connectors").join(name);
    ensure_connector_dir(&connector_dir);

    let mut restart_count: u32 = 0;
    let mut backoff_state = BackoffState::Active;
    let mut recent_failure_times: Vec<Instant> = Vec::new();

    tracing::info!(
        connector = %name,
        command = %entry.command,
        "ConnectorSupervisor: starting managed connector"
    );

    loop {
        // ── Check for a CLI restart-signal (clears Suspended) ─────────────
        if backoff_state == BackoffState::Suspended {
            let signal_path = connector_dir.join("restart-signal");
            if signal_path.exists() {
                tracing::info!(
                    connector = %name,
                    "Restart signal received — clearing Suspended state"
                );
                let _ = std::fs::remove_file(&signal_path);
                backoff_state = BackoffState::Active;
                recent_failure_times.clear();
                push_event(&queues, name, "resumed", "Connector resumed by operator");
                write_status_file(&connector_dir, name, None, "starting", restart_count, None);
            } else {
                // Poll until a signal arrives or shutdown.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(RESTART_SIGNAL_POLL_SECS)) => {}
                    _ = shutdown.notified() => {
                        write_status_file(&connector_dir, name, None, "stopped", restart_count, None);
                        return;
                    }
                }
                continue;
            }
        }

        // ── Adopt-orphan check ────────────────────────────────────────────
        if let Some(orphan_pid) = try_adopt_orphan_connector(&connector_dir) {
            tracing::info!(
                connector = %name,
                pid = orphan_pid,
                "ConnectorSupervisor: adopted orphan process — skipping spawn"
            );
            push_event(
                &queues,
                name,
                "adopted",
                &format!("Adopted orphan PID {}", orphan_pid),
            );
            write_status_file(
                &connector_dir,
                name,
                Some(orphan_pid),
                "running",
                restart_count,
                None,
            );

            // Watch until the adopted process dies or shutdown arrives.
            tokio::select! {
                _ = wait_until_dead(orphan_pid) => {
                    tracing::warn!(
                        connector = %name,
                        pid = orphan_pid,
                        "ConnectorSupervisor: adopted process exited"
                    );
                }
                _ = shutdown.notified() => {
                    write_status_file(&connector_dir, name, None, "stopped", restart_count, None);
                    return;
                }
            }
            // Fall through to restart logic.
        } else {
            // ── Spawn ─────────────────────────────────────────────────────
            let started_at = chrono::Utc::now().to_rfc3339();
            write_status_file(
                &connector_dir,
                name,
                None,
                "starting",
                restart_count,
                Some(&started_at),
            );
            push_event(&queues, name, "starting", "Spawning connector process");

            let mut child = match spawn_connector(&entry) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        connector = %name,
                        error = %e,
                        "ConnectorSupervisor: failed to spawn connector — will retry"
                    );
                    write_status_file(&connector_dir, name, None, "stopped", restart_count, None);
                    push_event(
                        &queues,
                        name,
                        "spawn_failed",
                        &format!("Spawn error: {}", e),
                    );
                    handle_failure(
                        &connector_dir,
                        name,
                        &queues,
                        &mut restart_count,
                        &mut backoff_state,
                        &mut recent_failure_times,
                        &shutdown,
                    )
                    .await;
                    continue;
                }
            };

            let pid = child.id().unwrap_or(0);
            tracing::info!(
                connector = %name,
                pid,
                "ConnectorSupervisor: connector running"
            );
            write_supervisor_pid(&connector_dir, pid);
            write_status_file(
                &connector_dir,
                name,
                Some(pid),
                "running",
                restart_count,
                Some(&started_at),
            );
            push_event(&queues, name, "started", &format!("PID {}", pid));

            // ── Monitor: heartbeat + exit + shutdown ──────────────────────
            let exit_reason = monitor_connector(&mut child, &connector_dir, &shutdown).await;

            let _ = child.kill().await; // ensure no zombie

            tracing::warn!(
                connector = %name,
                reason = ?exit_reason,
                "ConnectorSupervisor: connector exited"
            );
            write_status_file(&connector_dir, name, None, "stopped", restart_count, None);
            remove_supervisor_pid(&connector_dir);

            match exit_reason {
                ExitReason::DaemonShutdown => {
                    push_event(&queues, name, "stopped", "Daemon shutdown");
                    return;
                }
                ExitReason::Clean => {
                    push_event(&queues, name, "stopped", "Clean exit");
                }
                ExitReason::Crash(code) => {
                    push_event(&queues, name, "crashed", &format!("Exit code {:?}", code));
                }
                ExitReason::HeartbeatMissed => {
                    push_event(
                        &queues,
                        name,
                        "heartbeat_missed",
                        "No heartbeat for 90s — restarting",
                    );
                }
            }
        }

        // ── Backoff + restart ─────────────────────────────────────────────
        handle_failure(
            &connector_dir,
            name,
            &queues,
            &mut restart_count,
            &mut backoff_state,
            &mut recent_failure_times,
            &shutdown,
        )
        .await;
    }
}

// ─── Failure handling + backoff ───────────────────────────────────────────────

async fn handle_failure(
    connector_dir: &Path,
    name: &str,
    queues: &AllQueues,
    restart_count: &mut u32,
    backoff_state: &mut BackoffState,
    recent_failure_times: &mut Vec<Instant>,
    shutdown: &Arc<Notify>,
) {
    *restart_count = restart_count.saturating_add(1);

    // Track failures in the 5-minute window.
    let now = Instant::now();
    recent_failure_times.push(now);
    let window = Duration::from_secs(SUSPEND_WINDOW_SECS);
    recent_failure_times.retain(|t| now.duration_since(*t) < window);

    if recent_failure_times.len() as u32 >= SUSPEND_FAILURE_COUNT {
        tracing::error!(
            connector = %name,
            failures_in_window = recent_failure_times.len(),
            "ConnectorSupervisor: connector suspended after {} failures in {}s \
             — run `ta connector restart {}` to resume",
            SUSPEND_FAILURE_COUNT,
            SUSPEND_WINDOW_SECS,
            name,
        );
        *backoff_state = BackoffState::Suspended;
        push_event(
            queues,
            name,
            "suspended",
            &format!(
                "Suspended after {} failures in {}s — run `ta connector restart {}`",
                SUSPEND_FAILURE_COUNT, SUSPEND_WINDOW_SECS, name
            ),
        );
        write_status_file(connector_dir, name, None, "suspended", *restart_count, None);
        return;
    }

    // Exponential backoff: min(2^restart_count, 60) seconds.
    let backoff_secs = MAX_BACKOFF_SECS.min(2u64.saturating_pow(*restart_count));
    let backoff_secs = backoff_secs.max(1);

    tracing::info!(
        connector = %name,
        restart_count = *restart_count,
        backoff_secs,
        "ConnectorSupervisor: connector will restart in {}s",
        backoff_secs
    );

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
        _ = shutdown.notified() => {
            write_status_file(connector_dir, name, None, "stopped", *restart_count, None);
        }
    }
}

// ─── Process monitoring ───────────────────────────────────────────────────────

#[derive(Debug)]
enum ExitReason {
    DaemonShutdown,
    Clean,
    Crash(Option<i32>),
    HeartbeatMissed,
}

async fn monitor_connector(
    child: &mut tokio::process::Child,
    connector_dir: &Path,
    shutdown: &Arc<Notify>,
) -> ExitReason {
    let heartbeat_path = connector_dir.join("heartbeat");
    let poll_interval = Duration::from_secs(HEARTBEAT_POLL_INTERVAL_SECS);

    loop {
        tokio::select! {
            status = child.wait() => {
                let code = status.ok().and_then(|s| s.code());
                return if code == Some(0) {
                    ExitReason::Clean
                } else {
                    ExitReason::Crash(code)
                };
            }
            _ = tokio::time::sleep(poll_interval) => {
                // Check heartbeat staleness.
                if heartbeat_path.exists() {
                    if let Ok(mtime) = heartbeat_path.metadata().and_then(|m| m.modified()) {
                        let age = SystemTime::now()
                            .duration_since(mtime)
                            .unwrap_or(Duration::ZERO);
                        if age.as_secs() > HEARTBEAT_STALE_SECS {
                            tracing::warn!(
                                path = %heartbeat_path.display(),
                                age_secs = age.as_secs(),
                                "ConnectorSupervisor: heartbeat stale — restarting"
                            );
                            return ExitReason::HeartbeatMissed;
                        }
                    }
                }
                // Check for restart signal during running phase too (to handle
                // operator requests that arrive while the connector is healthy).
                // We don't clear suspended here — that's handled in the main loop.
            }
            _ = shutdown.notified() => return ExitReason::DaemonShutdown,
        }
    }
}

// ─── Process spawning ─────────────────────────────────────────────────────────

fn spawn_connector(entry: &ManagedConnectorEntry) -> std::io::Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(&entry.command);
    cmd.args(&entry.args)
        .envs(std::env::vars())
        .envs(entry.env.iter())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    cmd.spawn()
}

// ─── Adopt-orphan ─────────────────────────────────────────────────────────────

/// Check if a previous instance is still alive by reading the supervisor PID file.
///
/// If alive: returns `Some(pid)`.
/// If dead (stale): removes the PID file and returns `None`.
fn try_adopt_orphan_connector(connector_dir: &Path) -> Option<u32> {
    let pid_path = connector_dir.join("supervisor.pid");
    let raw = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;

    if is_pid_alive(pid) {
        Some(pid)
    } else {
        tracing::info!(
            pid,
            path = %pid_path.display(),
            "ConnectorSupervisor: removing stale supervisor PID file"
        );
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

// ─── Platform PID liveness check ─────────────────────────────────────────────

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Poll until a non-child process is no longer alive.
async fn wait_until_dead(pid: u32) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if !is_pid_alive(pid) {
            return;
        }
    }
}

// ─── File helpers ─────────────────────────────────────────────────────────────

fn ensure_connector_dir(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "ConnectorSupervisor: cannot create connector directory"
        );
    }
}

fn write_supervisor_pid(connector_dir: &Path, pid: u32) {
    let path = connector_dir.join("supervisor.pid");
    let _ = std::fs::write(&path, pid.to_string());
}

fn remove_supervisor_pid(connector_dir: &Path) {
    let _ = std::fs::remove_file(connector_dir.join("supervisor.pid"));
}

fn write_status_file(
    connector_dir: &Path,
    name: &str,
    pid: Option<u32>,
    status: &str,
    restart_count: u32,
    last_started_at: Option<&str>,
) {
    let heartbeat_secs_ago = heartbeat_age_secs(connector_dir);
    let record = ConnectorSupervisorStatus {
        name: name.to_string(),
        status: status.to_string(),
        pid,
        restart_count,
        last_heartbeat_secs_ago: heartbeat_secs_ago,
        last_started_at: last_started_at.map(|s| s.to_string()),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Ok(json) = serde_json::to_string_pretty(&record) {
        let path = connector_dir.join("supervisor-status.json");
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "ConnectorSupervisor: failed to write status file"
            );
        }
    }
}

fn heartbeat_age_secs(connector_dir: &Path) -> Option<u64> {
    let path = connector_dir.join("heartbeat");
    let mtime = path.metadata().and_then(|m| m.modified()).ok()?;
    SystemTime::now()
        .duration_since(mtime)
        .ok()
        .map(|d| d.as_secs())
}

// ─── Event push helper ────────────────────────────────────────────────────────

fn push_event(queues: &AllQueues, connector: &str, kind: &str, message: &str) {
    let event = ConnectorEvent {
        connector: connector.to_string(),
        at: chrono::Utc::now(),
        kind: kind.to_string(),
        message: message.to_string(),
    };
    if let Ok(mut lock) = queues.lock() {
        if let Some(q) = lock.get_mut(connector) {
            q.push(event);
        }
    }
}

// ─── CLI helpers (called by `ta connector` commands) ─────────────────────────

/// Read connector status from the supervisor-status.json file.
pub fn read_connector_status(project_root: &Path, name: &str) -> Option<ConnectorSupervisorStatus> {
    let path = project_root
        .join(".ta")
        .join("connectors")
        .join(name)
        .join("supervisor-status.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// List all connectors the supervisor knows about (by scanning `.ta/connectors/`).
pub fn list_known_connectors(project_root: &Path) -> Vec<ConnectorSupervisorStatus> {
    let connectors_dir = project_root.join(".ta").join("connectors");
    if !connectors_dir.exists() {
        return vec![];
    }

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&connectors_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(status) = read_connector_status(project_root, &name) {
                    results.push(status);
                }
            }
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

/// Write a restart signal file so the supervisor clears Suspended state.
pub fn signal_connector_restart(project_root: &Path, name: &str) -> std::io::Result<()> {
    let signal_path = project_root
        .join(".ta")
        .join("connectors")
        .join(name)
        .join("restart-signal");
    std::fs::write(&signal_path, "restart")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_managed_connectors_from_toml() {
        let toml = r#"
[[connectors.managed]]
name = "discord"
command = "ta-channel-discord"
args = ["--listen"]
enabled = true

[[connectors.managed]]
name = "slack"
command = "ta-channel-slack"
enabled = false
"#;
        let entries = load_managed_connectors(toml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "discord");
        assert_eq!(entries[0].command, "ta-channel-discord");
        assert_eq!(entries[0].args, vec!["--listen"]);
        assert!(entries[0].enabled);
        assert_eq!(entries[1].name, "slack");
        assert!(!entries[1].enabled);
    }

    #[test]
    fn parse_missing_connectors_section_returns_empty() {
        let toml = "[workflow]\nname = \"my-project\"\n";
        let entries = load_managed_connectors(toml);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_managed_connectors_default_enabled() {
        let toml = r#"
[[connectors.managed]]
name = "teams"
command = "ta-channel-teams"
"#;
        let entries = load_managed_connectors(toml);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].enabled, "enabled should default to true");
    }

    #[test]
    fn event_queue_bounded() {
        let mut q = ConnectorEventQueue::new(3);
        let ev = |k: &str| ConnectorEvent {
            connector: "test".to_string(),
            at: chrono::Utc::now(),
            kind: k.to_string(),
            message: k.to_string(),
        };

        assert!(q.push(ev("a")));
        assert!(q.push(ev("b")));
        assert!(q.push(ev("c")));
        // Queue is full — new event is rejected.
        assert!(!q.push(ev("d")));
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn event_queue_ttl_eviction() {
        let mut q = ConnectorEventQueue::new(10);
        // Add a very old event.
        let old = ConnectorEvent {
            connector: "test".to_string(),
            at: chrono::Utc::now() - chrono::Duration::seconds(700),
            kind: "old".to_string(),
            message: "old".to_string(),
        };
        let fresh = ConnectorEvent {
            connector: "test".to_string(),
            at: chrono::Utc::now(),
            kind: "fresh".to_string(),
            message: "fresh".to_string(),
        };
        q.push(old);
        q.push(fresh);
        assert_eq!(q.len(), 2);
        q.evict_stale(600); // 600s TTL
        assert_eq!(q.len(), 1, "old event should have been evicted");
    }

    #[test]
    fn write_and_read_status_file() {
        let dir = tempfile::tempdir().unwrap();
        let connector_dir = dir.path().join(".ta").join("connectors").join("test");
        std::fs::create_dir_all(&connector_dir).unwrap();

        write_status_file(&connector_dir, "test", Some(12345), "running", 2, None);

        let status = read_connector_status(dir.path(), "test").unwrap();
        assert_eq!(status.name, "test");
        assert_eq!(status.pid, Some(12345));
        assert_eq!(status.status, "running");
        assert_eq!(status.restart_count, 2);
    }

    #[test]
    fn is_pid_alive_self() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn is_pid_alive_dead() {
        assert!(!is_pid_alive(999_999_999));
    }

    #[test]
    fn signal_connector_restart_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let connector_dir = dir.path().join(".ta").join("connectors").join("discord");
        std::fs::create_dir_all(&connector_dir).unwrap();

        signal_connector_restart(dir.path(), "discord").unwrap();

        let signal_path = connector_dir.join("restart-signal");
        assert!(
            signal_path.exists(),
            "restart signal file should be created"
        );
    }

    #[test]
    fn list_known_connectors_empty_when_no_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = list_known_connectors(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn list_known_connectors_returns_known_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let conn_dir = dir.path().join(".ta").join("connectors").join("alpha");
        std::fs::create_dir_all(&conn_dir).unwrap();
        write_status_file(&conn_dir, "alpha", None, "stopped", 1, None);

        let results = list_known_connectors(dir.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "alpha");
    }
}
