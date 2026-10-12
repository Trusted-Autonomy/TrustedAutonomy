// startup.rs -- daemon readiness, keychain approval at start, and the reasons
// `/health` and `/api/status` report (v0.17.12.16).
//
// An unattended daemon start must not look healthy and then hang. Reading a
// secret from the OS keychain can block on a system dialog that needs a
// person's click. So the daemon:
//
//   1. binds its port first and answers `/health` with `status: "starting"`
//      and a reason while it works through startup;
//   2. reads EVERY keychain item it will need on a blocking thread, with a
//      timeout (`[daemon] keychain_approval_timeout_secs`, default 60), and
//      logs a clear line when it starts waiting and when it resumes;
//   3. on timeout starts DEGRADED (default posture) or refuses to start
//      (strict posture);
//   4. switches the process into keychain "request mode" so that no secret is
//      ever read from the keychain while serving a request.
//
// A read that finishes after the timeout still fills the in-memory cache and
// clears the degraded state, so approving the dialog late still helps.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use ta_credentials::keychain::{self, Cached, KeychainItem};
use ta_credentials::state::Posture;
use tokio::sync::{watch, Notify};

/// Default seconds startup waits for one keychain item.
pub const DEFAULT_KEYCHAIN_TIMEOUT_SECS: u64 = 60;

/// How long a keychain read may take before the daemon says it is waiting.
/// An approved item answers in milliseconds; this keeps routine starts quiet.
pub const WAIT_GRACE: Duration = Duration::from_millis(750);

/// One reason the daemon is `starting` or `degraded`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StatusReason {
    /// Which part of the daemon: `keychain` or `whiteboard`.
    pub component: String,
    pub reason: String,
    /// What a person can do about it.
    pub next_step: String,
}

/// Overall daemon state as `/health` reports it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Starting,
    Degraded,
    Ok,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Degraded => "degraded",
            Phase::Ok => "ok",
        }
    }
}

#[derive(Debug, Clone)]
enum KeychainState {
    Loading {
        waiting_for: Option<String>,
        waited_since: Option<Instant>,
        timeout: Duration,
    },
    Ready,
    Degraded(String),
}

/// Link state of the whiteboard transport, shown in `/api/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhiteboardLink {
    /// `[whiteboard] enabled = false`, or a transport that needs no link.
    Disabled,
    Connecting {
        server: String,
        attempt: u32,
    },
    Connected {
        server: String,
    },
    Disconnected {
        server: String,
        attempt: u32,
        error: String,
        retry_in: Duration,
    },
}

struct Inner {
    keychain: KeychainState,
    whiteboard: WhiteboardLink,
    refusal: Option<String>,
}

/// Shared readiness state: one per daemon process.
pub struct DaemonHealth {
    inner: Mutex<Inner>,
    resolved: watch::Sender<bool>,
    refused: Notify,
}

impl DaemonHealth {
    /// Everything ready: what tests and a daemon with nothing to load use.
    pub fn ready() -> Arc<Self> {
        Arc::new(Self::build(KeychainState::Ready, true))
    }

    /// Keychain items are still being read.
    pub fn starting(timeout: Duration) -> Arc<Self> {
        Arc::new(Self::build(
            KeychainState::Loading {
                waiting_for: None,
                waited_since: None,
                timeout,
            },
            false,
        ))
    }

    fn build(keychain: KeychainState, resolved: bool) -> Self {
        Self {
            inner: Mutex::new(Inner {
                keychain,
                whiteboard: WhiteboardLink::Disabled,
                refusal: None,
            }),
            resolved: watch::channel(resolved).0,
            refused: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── keychain ───────────────────────────────────────────────────────

    fn set_waiting(&self, label: &str, timeout: Duration) {
        let mut g = self.lock();
        if let KeychainState::Loading {
            waiting_for,
            waited_since,
            timeout: t,
        } = &mut g.keychain
        {
            *waiting_for = Some(label.to_string());
            *waited_since = Some(Instant::now());
            *t = timeout;
        }
    }

    fn set_not_waiting(&self) {
        let mut g = self.lock();
        if let KeychainState::Loading {
            waiting_for,
            waited_since,
            ..
        } = &mut g.keychain
        {
            *waiting_for = None;
            *waited_since = None;
        }
    }

    pub fn set_keychain_ready(&self) {
        self.lock().keychain = KeychainState::Ready;
        self.resolved.send_replace(true);
    }

    fn set_keychain_degraded(&self, reason: String) {
        self.lock().keychain = KeychainState::Degraded(reason);
        self.resolved.send_replace(true);
    }

    fn set_refused(&self, reason: String) {
        {
            let mut g = self.lock();
            g.keychain = KeychainState::Degraded(reason.clone());
            g.refusal = Some(reason);
        }
        self.resolved.send_replace(true);
        self.refused.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn refuse_for_test(&self, reason: &str) {
        self.set_refused(reason.to_string());
    }

    /// Why the strict posture refused to start, if it did.
    pub fn refusal(&self) -> Option<String> {
        self.lock().refusal.clone()
    }

    /// Completes when the daemon decided to refuse to start.
    pub async fn refused(&self) -> String {
        loop {
            if let Some(r) = self.refusal() {
                return r;
            }
            self.refused.notified().await;
        }
    }

    /// Completes once startup keychain loading has a verdict (loaded, degraded
    /// or refused). Things that read secrets wait for this before they start.
    pub async fn wait_resolved(&self) {
        let mut rx = self.resolved.subscribe();
        let _ = rx.wait_for(|done| *done).await;
    }

    // ── whiteboard ─────────────────────────────────────────────────────

    pub fn set_whiteboard(&self, link: WhiteboardLink) {
        self.lock().whiteboard = link;
    }

    pub fn whiteboard(&self) -> WhiteboardLink {
        self.lock().whiteboard.clone()
    }

    // ── reporting ──────────────────────────────────────────────────────

    pub fn reasons(&self) -> Vec<StatusReason> {
        let g = self.lock();
        let mut out = Vec::new();
        match &g.keychain {
            KeychainState::Ready => {}
            KeychainState::Loading {
                waiting_for: Some(label),
                waited_since,
                timeout,
            } => out.push(StatusReason {
                component: "keychain".into(),
                reason: format!(
                    "waiting for keychain approval for {label} ({}s of {}s)",
                    waited_since.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                    timeout.as_secs()
                ),
                next_step: "Click Always Allow in the macOS Keychain dialog (it may be behind \
                            other windows). If you are not at this machine, the daemon starts \
                            degraded when the wait times out"
                    .into(),
            }),
            KeychainState::Loading { .. } => out.push(StatusReason {
                component: "keychain".into(),
                reason: "reading keychain items".into(),
                next_step: "No action needed; this normally takes under a second".into(),
            }),
            KeychainState::Degraded(reason) => out.push(StatusReason {
                component: "keychain".into(),
                reason: reason.clone(),
                next_step: "Unlock the keychain and approve the TA access prompt (choose Always \
                            Allow), then run `ta daemon restart`. Until then features that need \
                            the key (capability tokens, audit signing, the credential vault) \
                            refuse with an error. To use file custody instead, set \
                            TA_NO_KEYCHAIN=1 and restart"
                    .into(),
            }),
        }
        match &g.whiteboard {
            WhiteboardLink::Disconnected {
                server,
                attempt,
                error,
                retry_in,
            } => out.push(StatusReason {
                component: "whiteboard".into(),
                reason: format!(
                    "whiteboard disconnected: cannot reach {server} (attempt {attempt}, last \
                     error: {error}); retrying in {}s",
                    retry_in.as_secs()
                ),
                next_step: "Start the NATS server (for example `nats-server -js`) or fix \
                            nats_url under [whiteboard] in .ta/workflow.toml. The daemon \
                            reconnects by itself; no restart is needed"
                    .into(),
            }),
            WhiteboardLink::Connecting { .. }
            | WhiteboardLink::Connected { .. }
            | WhiteboardLink::Disabled => {}
        }
        out
    }

    pub fn phase(&self) -> Phase {
        let loading = matches!(self.lock().keychain, KeychainState::Loading { .. });
        if loading {
            Phase::Starting
        } else if self.reasons().is_empty() {
            Phase::Ok
        } else {
            Phase::Degraded
        }
    }

    /// The whiteboard link as the JSON object `/api/status` shows.
    pub fn whiteboard_json(&self) -> serde_json::Value {
        match self.whiteboard() {
            WhiteboardLink::Disabled => serde_json::json!({ "state": "disabled" }),
            WhiteboardLink::Connecting { server, attempt } => {
                serde_json::json!({ "state": "connecting", "server": server, "attempt": attempt })
            }
            WhiteboardLink::Connected { server } => {
                serde_json::json!({ "state": "connected", "server": server })
            }
            WhiteboardLink::Disconnected {
                server,
                attempt,
                error,
                retry_in,
            } => serde_json::json!({
                "state": "disconnected",
                "server": server,
                "attempt": attempt,
                "error": error,
                "retry_in_secs": retry_in.as_secs(),
            }),
        }
    }

    /// Add `status` (`ok` | `starting` | `degraded`), `reasons` and
    /// `whiteboard` to an existing JSON object, keeping every other field.
    pub fn apply_to(&self, value: &mut serde_json::Value) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("status".into(), self.phase().as_str().into());
            obj.insert(
                "reasons".into(),
                serde_json::to_value(self.reasons()).unwrap_or_default(),
            );
            obj.insert("whiteboard".into(), self.whiteboard_json());
        }
    }
}

// ── keychain preload ───────────────────────────────────────────────────────

/// The line logged when startup begins waiting on an approval dialog.
pub fn waiting_line(label: &str, timeout: Duration) -> String {
    format!(
        "waiting for keychain approval for {label} ... click Always Allow in the macOS dialog; \
         timeout {}s",
        timeout.as_secs()
    )
}

#[derive(Debug, Clone, Copy)]
pub struct PreloadSettings {
    /// Longest startup waits for one item.
    pub timeout: Duration,
    /// How long a read may take before the "waiting" line is logged.
    pub grace: Duration,
    pub posture: Posture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreloadOutcome {
    Loaded,
    Degraded(String),
    Refused(String),
}

#[derive(Clone, Default)]
struct Progress {
    done: usize,
    errors: Vec<String>,
}

fn timeout_reason(label: &str, timeout: Duration) -> String {
    format!(
        "timed out after {}s waiting for keychain approval for {label}",
        timeout.as_secs()
    )
}

fn failure_reason(errors: &[String]) -> String {
    format!("could not read the keychain: {}", errors.join("; "))
}

/// Read every item on a blocking thread, time-boxed per item, and record the
/// verdict in `health`. Always switches the process into keychain request mode
/// before it returns, so nothing reads the keychain while serving requests.
pub async fn run_preload(
    items: Vec<KeychainItem>,
    settings: PreloadSettings,
    health: &Arc<DaemonHealth>,
) -> PreloadOutcome {
    let total = items.len();
    if total == 0 {
        health.set_keychain_ready();
        keychain::enter_request_mode();
        return PreloadOutcome::Loaded;
    }
    tracing::info!(
        items = total,
        timeout_secs = settings.timeout.as_secs(),
        "startup: reading {total} keychain item(s) before reporting ready"
    );
    let (tx, mut rx) = watch::channel(Progress::default());
    let worker_items = items.clone();
    tokio::task::spawn_blocking(move || {
        let mut errors = Vec::new();
        for (i, item) in worker_items.iter().enumerate() {
            if let Cached::Unavailable(e) = keychain::preload(&item.service, &item.user) {
                errors.push(format!("{}: {e}", item.label()));
            }
            let snapshot = errors.clone();
            tx.send_modify(|p| {
                p.done = i + 1;
                p.errors = snapshot;
            });
        }
    });

    let started = Instant::now();
    let mut item_started = Instant::now();
    let mut announced: Option<String> = None;
    let mut seen_done = 0usize;

    let verdict = loop {
        let progress = rx.borrow().clone();
        if progress.done != seen_done {
            seen_done = progress.done;
            item_started = Instant::now();
            if let Some(label) = announced.take() {
                health.set_not_waiting();
                tracing::info!(
                    item = %label,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "keychain access approved for {label}; startup resumed"
                );
            }
        }
        if progress.done >= total {
            break if progress.errors.is_empty() {
                PreloadOutcome::Loaded
            } else {
                let reason = failure_reason(&progress.errors);
                match settings.posture {
                    Posture::Strict => PreloadOutcome::Refused(reason),
                    Posture::Default => PreloadOutcome::Degraded(reason),
                }
            };
        }
        let label = items[progress.done].label();
        let waited = item_started.elapsed();
        if waited >= settings.timeout {
            let reason = timeout_reason(&label, settings.timeout);
            break match settings.posture {
                Posture::Strict => PreloadOutcome::Refused(reason),
                Posture::Default => PreloadOutcome::Degraded(reason),
            };
        }
        let wake_in = if announced.is_none() {
            settings.grace.saturating_sub(waited)
        } else {
            settings.timeout - waited
        };
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    // The worker finished and dropped its sender.
                    if rx.borrow().done < total {
                        break PreloadOutcome::Degraded(
                            "the keychain reader stopped before finishing".into(),
                        );
                    }
                }
            }
            _ = tokio::time::sleep(wake_in) => {
                if announced.is_none() && item_started.elapsed() >= settings.grace {
                    health.set_waiting(&label, settings.timeout);
                    tracing::warn!(
                        item = %label,
                        timeout_secs = settings.timeout.as_secs(),
                        "{}",
                        waiting_line(&label, settings.timeout)
                    );
                    announced = Some(label);
                }
            }
        }
    };

    // From here on no request may read the keychain.
    keychain::enter_request_mode();
    match &verdict {
        PreloadOutcome::Loaded => {
            health.set_keychain_ready();
            tracing::info!(
                items = total,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "startup: all keychain items loaded"
            );
        }
        PreloadOutcome::Degraded(reason) => {
            tracing::error!(
                reason = %reason,
                "startup: keychain unavailable, starting DEGRADED. Features that need the \
                 key refuse until it is loaded. Approve the keychain prompt (Always Allow) \
                 and run `ta daemon restart`, or set TA_NO_KEYCHAIN=1 for file custody. \
                 Strict posture (TA_POSTURE=strict) refuses to start instead"
            );
            health.set_keychain_degraded(reason.clone());
            spawn_late_watcher(rx, total, announced, health.clone());
        }
        PreloadOutcome::Refused(reason) => {
            tracing::error!(
                reason = %reason,
                "startup: keychain unavailable and the strict posture is active, refusing to \
                 start. Approve the keychain prompt (Always Allow) and start again, or use \
                 the default posture"
            );
            health.set_refused(reason.clone());
        }
    }
    verdict
}

/// After a timeout the blocking reader keeps going. If the dialog is approved
/// late and every item loads, clear the degraded state.
fn spawn_late_watcher(
    mut rx: watch::Receiver<Progress>,
    total: usize,
    _announced: Option<String>,
    health: Arc<DaemonHealth>,
) {
    tokio::spawn(async move {
        let finished = rx.wait_for(|p| p.done >= total).await;
        let Ok(progress) = finished.map(|p| p.clone()) else {
            return;
        };
        if progress.errors.is_empty() {
            health.set_keychain_ready();
            tracing::info!(
                "keychain approval arrived after the startup timeout; all keychain items are \
                 now loaded and the daemon is no longer degraded"
            );
        }
    });
}

/// Items this daemon must load for `project_root` (empty when the keychain is
/// not in use). A failure to even list them is reported as a reason string.
pub fn items_for(project_root: &std::path::Path) -> Result<Vec<KeychainItem>, String> {
    keychain::startup_items(project_root)
}

/// Start loading keychain items in the background and return the shared
/// health handle immediately, so the caller can bind its port and answer
/// `/health` while a person approves the dialog.
pub fn begin(
    project_root: &std::path::Path,
    timeout: Duration,
    posture: Posture,
) -> Arc<DaemonHealth> {
    let items = match items_for(project_root) {
        Ok(items) => items,
        Err(e) => {
            tracing::error!(
                project = %project_root.display(),
                error = %e,
                "startup: could not work out which keychain items to load; secrets are \
                 not preloaded. Check the TA state directory (`ta show state`)"
            );
            let health = DaemonHealth::ready();
            health.set_keychain_degraded(format!("could not list keychain items: {e}"));
            keychain::enter_request_mode();
            return health;
        }
    };
    if items.is_empty() {
        keychain::enter_request_mode();
        return DaemonHealth::ready();
    }
    let health = DaemonHealth::starting(timeout);
    let settings = PreloadSettings {
        timeout,
        grace: WAIT_GRACE,
        posture,
    };
    let h = health.clone();
    tokio::spawn(async move {
        run_preload(items, settings, &h).await;
    });
    health
}

/// Load one more project's keychain items (a project opened after start),
/// time-boxed so a pending dialog cannot hang the caller.
pub async fn preload_project(project_root: &std::path::Path, timeout: Duration) {
    let items = match items_for(project_root) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(project = %project_root.display(), error = %e,
                "could not list keychain items for the newly opened project");
            return;
        }
    };
    if items.is_empty() {
        return;
    }
    let root = project_root.display().to_string();
    let job = tokio::task::spawn_blocking(move || {
        for item in &items {
            keychain::preload(&item.service, &item.user);
        }
    });
    if tokio::time::timeout(timeout, job).await.is_err() {
        tracing::warn!(
            project = %root,
            timeout_secs = timeout.as_secs(),
            "keychain approval for the newly opened project did not arrive in time; its \
             secrets stay unavailable until you approve the dialog and run `ta daemon restart`"
        );
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Test doubles for the keychain, shared with the HTTP startup tests.
    use std::collections::HashMap;
    use std::sync::{Arc, Condvar, Mutex as StdMutex};

    use ta_credentials::keychain::KeychainBackend;

    /// Serializes tests that use the process-wide keychain state.
    pub static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A keychain whose reads block until released.
    pub struct Blocked {
        pub released: (StdMutex<bool>, Condvar),
        pub items: StdMutex<HashMap<(String, String), String>>,
        pub reads: std::sync::atomic::AtomicUsize,
    }

    impl Blocked {
        pub fn new(blocked: bool) -> Arc<Self> {
            Arc::new(Self {
                released: (StdMutex::new(!blocked), Condvar::new()),
                items: StdMutex::new(HashMap::new()),
                reads: Default::default(),
            })
        }
        pub fn release(&self) {
            *self.released.0.lock().unwrap() = true;
            self.released.1.notify_all();
        }
    }

    impl KeychainBackend for Blocked {
        fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut open = self.released.0.lock().unwrap();
            while !*open {
                open = self.released.1.wait(open).unwrap();
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), user.into()))
                .cloned())
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Blocked, LOCK};
    use super::*;
    use std::sync::Mutex as StdMutex;
    use ta_credentials::keychain::KeychainBackend;

    fn item(name: &str) -> KeychainItem {
        KeychainItem {
            service: "svc".into(),
            user: name.into(),
            purpose: "test key",
        }
    }

    fn settings(posture: Posture) -> PreloadSettings {
        PreloadSettings {
            timeout: Duration::from_millis(300),
            grace: Duration::from_millis(30),
            posture,
        }
    }

    /// Captures tracing output of the current thread.
    #[derive(Clone, Default)]
    struct LogBuf(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl LogBuf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }
    fn capture_logs(buf: &LogBuf) -> tracing::subscriber::DefaultGuard {
        let b = buf.clone();
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(move || b.clone())
                .with_ansi(false)
                .finish(),
        )
    }

    #[test]
    fn waiting_line_names_the_item_the_dialog_and_the_timeout() {
        let line = waiting_line(
            "audit-log signing key (svc/p/audit-key)",
            Duration::from_secs(60),
        );
        assert!(line.starts_with("waiting for keychain approval for audit-log signing key"));
        assert!(line.contains("click Always Allow in the macOS dialog"));
        assert!(line.contains("timeout 60s"));
    }

    #[tokio::test]
    async fn a_prompt_nobody_clicks_leaves_health_answering_then_degrades() {
        let _g = LOCK.lock().await;
        keychain::reset();
        let backend = Blocked::new(true);
        keychain::install_backend(backend.clone());
        let logs = LogBuf::default();
        let _log_guard = capture_logs(&logs);

        let health = DaemonHealth::starting(Duration::from_millis(300));
        let h = health.clone();
        let task = tokio::spawn(async move {
            run_preload(vec![item("a"), item("b")], settings(Posture::Default), &h).await
        });

        // While the dialog is pending the status API is live and says why.
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(health.phase(), Phase::Starting);
        let reasons = health.reasons();
        assert_eq!(reasons[0].component, "keychain");
        assert!(reasons[0].reason.contains("waiting for keychain approval"));
        assert!(reasons[0].next_step.contains("Always Allow"));
        let mut json = serde_json::json!({ "version": "x" });
        health.apply_to(&mut json);
        assert_eq!(json["status"], "starting");
        assert_eq!(json["version"], "x", "existing fields are kept");

        // Default posture: timeout falls back to degraded, not a refusal.
        let outcome = task.await.unwrap();
        assert!(matches!(outcome, PreloadOutcome::Degraded(ref r) if r.contains("timed out")));
        assert_eq!(health.phase(), Phase::Degraded);
        assert!(health.refusal().is_none());
        assert!(health.reasons()[0].reason.contains("timed out"));
        assert!(logs.text().contains("waiting for keychain approval for"));
        assert!(logs
            .text()
            .contains("click Always Allow in the macOS dialog"));

        // The process is in request mode: the pending read can never be repeated.
        assert!(keychain::request_mode());
        let reads_at_timeout = backend.reads.load(std::sync::atomic::Ordering::SeqCst);
        assert!(keychain::get("svc", "a").is_err());
        assert_eq!(
            backend.reads.load(std::sync::atomic::Ordering::SeqCst),
            reads_at_timeout
        );

        // A late click clears the degraded state.
        backend.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            while health.phase() != Phase::Ok {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("late approval should clear the degraded state");
        keychain::reset();
    }

    #[tokio::test]
    async fn the_strict_posture_refuses_to_start_on_timeout() {
        let _g = LOCK.lock().await;
        keychain::reset();
        let backend = Blocked::new(true);
        keychain::install_backend(backend.clone());
        let health = DaemonHealth::starting(Duration::from_millis(300));
        let outcome = run_preload(vec![item("a")], settings(Posture::Strict), &health).await;
        assert!(matches!(outcome, PreloadOutcome::Refused(ref r) if r.contains("timed out")));
        assert!(health.refusal().unwrap().contains("keychain approval"));
        tokio::time::timeout(Duration::from_secs(1), health.refused())
            .await
            .expect("refusal is signalled");
        backend.release();
        keychain::reset();
    }

    #[tokio::test]
    async fn an_approved_start_logs_resume_and_reports_ok() {
        let _g = LOCK.lock().await;
        keychain::reset();
        let backend = Blocked::new(true);
        backend
            .items
            .lock()
            .unwrap()
            .insert(("svc".into(), "a".into()), "secret".into());
        keychain::install_backend(backend.clone());
        let logs = LogBuf::default();
        let _log_guard = capture_logs(&logs);

        let health = DaemonHealth::starting(Duration::from_millis(300));
        let h = health.clone();
        let task = tokio::spawn(async move {
            run_preload(vec![item("a")], settings(Posture::Default), &h).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(health.phase(), Phase::Starting);
        backend.release();
        assert_eq!(task.await.unwrap(), PreloadOutcome::Loaded);
        assert_eq!(health.phase(), Phase::Ok);
        let text = logs.text();
        assert!(text.contains("waiting for keychain approval for"), "{text}");
        assert!(text.contains("startup resumed"), "{text}");
        assert_eq!(
            keychain::get("svc", "a").unwrap().as_deref(),
            Some("secret")
        );
        keychain::reset();
    }

    #[tokio::test]
    async fn an_unreadable_item_degrades_or_refuses_by_posture() {
        struct Denied;
        impl KeychainBackend for Denied {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                Err("user denied access".into())
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn delete(&self, _: &str, _: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let _g = LOCK.lock().await;
        for (posture, refused) in [(Posture::Default, false), (Posture::Strict, true)] {
            keychain::reset();
            keychain::install_backend(Arc::new(Denied));
            let health = DaemonHealth::starting(Duration::from_secs(1));
            let outcome = run_preload(vec![item("a")], settings(posture), &health).await;
            assert_eq!(matches!(outcome, PreloadOutcome::Refused(_)), refused);
            assert_eq!(health.refusal().is_some(), refused);
            assert!(health.reasons()[0].reason.contains("user denied access"));
        }
        keychain::reset();
    }

    #[tokio::test]
    async fn no_items_means_ready_at_once() {
        let _g = LOCK.lock().await;
        keychain::reset();
        let health = DaemonHealth::starting(Duration::from_secs(1));
        let outcome = run_preload(Vec::new(), settings(Posture::Default), &health).await;
        assert_eq!(outcome, PreloadOutcome::Loaded);
        assert_eq!(health.phase(), Phase::Ok);
        health.wait_resolved().await;
        keychain::reset();
    }

    #[test]
    fn a_disconnected_whiteboard_is_degraded_with_a_reason() {
        let health = DaemonHealth::ready();
        assert_eq!(health.phase(), Phase::Ok);
        health.set_whiteboard(WhiteboardLink::Disconnected {
            server: "nats://127.0.0.1:4222".into(),
            attempt: 3,
            error: "connection refused".into(),
            retry_in: Duration::from_secs(8),
        });
        assert_eq!(health.phase(), Phase::Degraded);
        let r = &health.reasons()[0];
        assert_eq!(r.component, "whiteboard");
        assert!(r.reason.contains("whiteboard disconnected"));
        assert!(r.reason.contains("attempt 3") && r.reason.contains("8s"));
        let mut json = serde_json::json!({});
        health.apply_to(&mut json);
        assert_eq!(json["whiteboard"]["state"], "disconnected");
        health.set_whiteboard(WhiteboardLink::Connected {
            server: "nats://127.0.0.1:4222".into(),
        });
        assert_eq!(health.phase(), Phase::Ok);
    }
}
