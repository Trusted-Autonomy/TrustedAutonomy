// token_refresh.rs — Periodic whiteboard-token refresh (v0.17.11.12).
//
// A team session's whiteboard scope token (`TeamSessionConfig.
// whiteboard_token`) is minted once at `ta team-session start` time with a
// fixed TTL (`WHITEBOARD_TOKEN_TTL_SECS` in `apps/ta-cli/src/commands/
// team_session.rs`) and no refresh path anywhere in the session's
// lifecycle -- a session still active past 24h got a clear 403 from every
// `ta_whiteboard_*` call (observable, per the Observability Mandate, but
// not graceful: whiteboard coordination just stopped working for the rest
// of that run). This loop closes that gap: it re-mints the token well
// before expiry and writes both `whiteboard_token`/
// `whiteboard_token_expires_at` back into `state.json` together.
//
// No other code needs to change for a refreshed token to take effect: every
// `ta run` launch reads the token fresh from `state.json`
// (`write_whiteboard_session_file` in `apps/ta-cli/src/commands/run.rs`),
// not from any in-memory/cached copy — so the very next role launched after
// a refresh already has the new token.
//
// Independent of `wake_listener.rs` and `team_session.rs`'s own rotation
// loop deliberately: a session with wake-on-demand listeners and zero
// rotation stages never runs `run_team_session`'s loop body past its first
// iteration (`state.stages.is_empty()` stops it immediately), so anchoring
// refresh to that loop would silently stop refreshing exactly the sessions
// most likely to run for a long time. This is its own daemon-startup task,
// same shape as `watchdog::run_watchdog`'s periodic-scan pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::team_session::TeamSessionState;

/// How often this loop wakes to check every session's token. Deliberately
/// much shorter than `REFRESH_THRESHOLD_SECS`, so a single check is never
/// meaningfully late relative to the threshold's own safety margin.
const CHECK_INTERVAL: Duration = Duration::from_secs(1800); // 30 min

/// Refresh a token once less than this much time remains before it
/// expires. Generous relative to `CHECK_INTERVAL` (24x) so a delayed or
/// even fully-missed single check cycle still can't let a token actually
/// expire before the next one catches it.
const REFRESH_THRESHOLD_SECS: i64 = 3600; // 1h

/// Must match `apps/ta-cli/src/commands/team_session.rs`'s
/// `WHITEBOARD_TOKEN_TTL_SECS` — the TTL a refreshed token gets, same as
/// the original mint.
const WHITEBOARD_TOKEN_TTL_SECS: u64 = 86400;

/// Spawns the daemon-lifetime refresh loop. Doesn't need `AppState`/
/// `whiteboard_transport` — token minting goes through `CredentialBroker`
/// directly (already scoped per-project via `.ta/`), the same mechanism
/// the CLI's own mint code uses.
pub fn start(
    project_root: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            for id in TeamSessionState::list_ids(&project_root) {
                if let Err(e) = refresh_one_session(&project_root, &id) {
                    tracing::warn!(
                        session = %id,
                        error = %e,
                        "token_refresh: failed to check/refresh whiteboard token -- will retry \
                         next check cycle"
                    );
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(CHECK_INTERVAL) => {}
                _ = shutdown.notified() => return,
            }
        }
    })
}

/// Checks one session's whiteboard token and re-mints it if needed.
/// Synchronous and side-effect-explicit (loads/saves state itself) so it's
/// directly unit-testable, same shape as `team_session::run_one_cycle`.
fn refresh_one_session(project_root: &Path, id: &str) -> anyhow::Result<()> {
    let Some(mut state) = TeamSessionState::load(project_root, id)? else {
        return Ok(());
    };
    if state.config.whiteboard_token.is_none() {
        return Ok(()); // whiteboard was never enabled for this session
    }

    let needs_refresh = match state.config.whiteboard_token_expires_at {
        Some(expires_at) => {
            (expires_at - chrono::Utc::now()).num_seconds() < REFRESH_THRESHOLD_SECS
        }
        // A token exists but no expiry was recorded -- a session started
        // before this field existed. Treat as due now rather than "never
        // expires", so an old session self-heals into having a real expiry
        // on its very next check instead of being permanently unrefreshed.
        None => true,
    };
    if !needs_refresh {
        return Ok(());
    }

    let broker = ta_credential_broker::CredentialBroker::open(&project_root.join(".ta"))?;
    let scope = format!("whiteboard:team_session:{id}");
    let granted = broker.grant(
        uuid::Uuid::new_v4(),
        id,
        vec![scope],
        WHITEBOARD_TOKEN_TTL_SECS,
    )?;

    state.config.whiteboard_token = Some(granted.token);
    state.config.whiteboard_token_expires_at =
        Some(chrono::Utc::now() + chrono::Duration::seconds(WHITEBOARD_TOKEN_TTL_SECS as i64));
    state.save(project_root)?;

    tracing::info!(session = %id, "token_refresh: refreshed whiteboard scope token");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_session::{TeamSessionConfig, TeamSessionStatus};

    fn sample_config(
        whiteboard_token: Option<String>,
        whiteboard_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> TeamSessionConfig {
        TeamSessionConfig {
            name: "sess-1".to_string(),
            workflow_path: "wf.yaml".to_string(),
            team_toml_path: "team.toml".to_string(),
            objective: "test".to_string(),
            budget: None,
            role_prompts: std::collections::HashMap::new(),
            whiteboard_token,
            whiteboard_token_expires_at,
        }
    }

    #[test]
    fn refresh_does_nothing_for_a_session_with_no_whiteboard_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(None, None), Vec::new());
        state.save(dir.path()).unwrap();

        refresh_one_session(dir.path(), "sess-1").unwrap();

        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert!(reloaded.config.whiteboard_token.is_none());
        assert_eq!(reloaded.status, TeamSessionStatus::Active); // untouched
    }

    #[test]
    fn refresh_does_nothing_when_expiry_is_far_in_the_future() {
        let dir = tempfile::tempdir().unwrap();
        let far_future = chrono::Utc::now() + chrono::Duration::hours(20);
        let mut state = TeamSessionState::new(
            "sess-1".to_string(),
            sample_config(Some("old-token".to_string()), Some(far_future)),
            Vec::new(),
        );
        state.save(dir.path()).unwrap();

        refresh_one_session(dir.path(), "sess-1").unwrap();

        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            reloaded.config.whiteboard_token.as_deref(),
            Some("old-token"),
            "a token with plenty of remaining lifetime must not be touched"
        );
        assert_eq!(
            reloaded.config.whiteboard_token_expires_at,
            Some(far_future)
        );
    }

    #[test]
    fn refresh_mints_a_new_token_when_close_to_expiry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let soon = chrono::Utc::now() + chrono::Duration::minutes(30);
        let mut state = TeamSessionState::new(
            "sess-1".to_string(),
            sample_config(Some("old-token".to_string()), Some(soon)),
            Vec::new(),
        );
        state.save(dir.path()).unwrap();

        refresh_one_session(dir.path(), "sess-1").unwrap();

        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_ne!(
            reloaded.config.whiteboard_token.as_deref(),
            Some("old-token"),
            "a token within the refresh threshold must be re-minted"
        );
        assert!(reloaded.config.whiteboard_token.is_some());
        let new_expiry = reloaded.config.whiteboard_token_expires_at.unwrap();
        assert!(
            new_expiry > soon,
            "the refreshed token's expiry must be pushed back out, not left at the old value"
        );
    }

    #[test]
    fn refresh_self_heals_a_token_with_no_recorded_expiry() {
        // A session started before whiteboard_token_expires_at existed:
        // token present, expiry None. Must be treated as due now.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let mut state = TeamSessionState::new(
            "sess-1".to_string(),
            sample_config(Some("old-token".to_string()), None),
            Vec::new(),
        );
        state.save(dir.path()).unwrap();

        refresh_one_session(dir.path(), "sess-1").unwrap();

        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert!(reloaded.config.whiteboard_token_expires_at.is_some());
        assert_ne!(
            reloaded.config.whiteboard_token.as_deref(),
            Some("old-token")
        );
    }

    #[test]
    fn refresh_of_a_nonexistent_session_is_a_harmless_no_op() {
        let dir = tempfile::tempdir().unwrap();
        refresh_one_session(dir.path(), "no-such-session").unwrap();
    }
}
