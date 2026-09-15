//! Pre-launch whiteboard conflict check (v0.17.11.2 item 6) — a live
//! complement to `task-graph`'s static wave planning, called right before
//! spawning a goal's implementation agent. Advisory only: it never blocks a
//! launch, only surfaces who else is currently active on the same
//! `source_dir` so the operator/agent has visibility before an
//! undeclared-overlap incident happens, rather than only after (matching
//! v0.17.10.2's own "prevention below the api_impact-tag granularity is out
//! of scope for v1" framing).
//!
//! No-op unless the project's `.ta/workflow.toml` has `[whiteboard]
//! enabled = true` — see `ta_agent_whiteboard::config`'s module doc for why
//! this stays opt-in.

use std::path::Path;
use std::time::Duration;

use ta_agent_whiteboard::WhiteboardConfig;

use crate::daemon_client::WhiteboardDaemonClient;

/// How long the whole check (connect + query) is allowed to take before
/// being abandoned. A pre-launch check that hangs indefinitely because a
/// configured NATS server is unreachable would be worse than no check at
/// all — this must never meaningfully slow down `ta_goal_start`.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Returns a human-readable list of other agents currently active on
/// `source_dir`, or an empty `Vec` if whiteboard coordination isn't
/// enabled for this project, the check times out, or nothing is found.
/// Never returns an error — every failure mode degrades to "no advisory
/// information available," never blocks the caller.
///
/// Runs its own dedicated single-threaded Tokio runtime on a fresh OS
/// thread rather than assuming an async context: this is called from
/// `launch_goal_agent`, a plain synchronous `fn`, which may itself already
/// be running inside the gateway's own async runtime — starting a nested
/// runtime on the *same* thread would panic.
pub fn other_active_agents_on(source_dir: &str) -> Vec<String> {
    let config = WhiteboardConfig::load(Path::new(source_dir));
    if !config.enabled {
        return Vec::new();
    }

    let source_dir = source_dir.to_string();
    let result = std::thread::spawn(move || -> Vec<String> {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::debug!(error = %e, "whiteboard pre-launch check: failed to start runtime");
                return Vec::new();
            }
        };
        rt.block_on(async move { query(&source_dir).await })
    })
    .join();

    match result {
        Ok(agents) => agents,
        Err(_) => {
            tracing::debug!("whiteboard pre-launch check: worker thread panicked");
            Vec::new()
        }
    }
}

/// Queries the daemon's shared whiteboard transport (Task 2) rather than
/// instantiating a private transport in this process — with `transport =
/// "memory"`, two concurrent `ta serve` processes each selecting their own
/// transport directly would get their own empty map and both report "no
/// conflict" even when one exists. `source_dir` doubles as the project
/// root used to locate the daemon's port (`.ta/daemon.pid`).
async fn query(source_dir: &str) -> Vec<String> {
    let outcome = tokio::time::timeout(CHECK_TIMEOUT, async {
        let client = WhiteboardDaemonClient::new(Path::new(source_dir));
        match client.list_presence_for_source(source_dir).await {
            Ok(records) => records
                .into_iter()
                .map(|r| format!("{} (goal {})", r.agent_id, r.goal_run_id))
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "whiteboard pre-launch check: query failed");
                Vec::new()
            }
        }
    })
    .await;

    match outcome {
        Ok(agents) => agents,
        Err(_) => {
            tracing::debug!("whiteboard pre-launch check: timed out");
            Vec::new()
        }
    }
}
