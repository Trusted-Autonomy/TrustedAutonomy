// token_refresh.rs: scrub bearer tokens out of persisted team-session state.
//
// Until the control-plane secrets work (red-team CR-03) a team session's
// whiteboard scope token was minted once at `ta team-session start`, stored in
// plaintext in `.ta/team-sessions/<id>/state.json`, and re-minted into that
// file by this module before it expired. Any agent that could read the project
// tree could copy it.
//
// Tokens are now minted per role launch by the daemon (see
// `team_session::role_launch_env`) and handed to the launched `ta run` in its
// environment; nothing token-shaped is persisted. This loop remains as a
// safety net for sessions started by an older version: it rewrites any
// `state.json` that still carries a token, keeping the session's
// whiteboard-enabled setting, so the secret does not sit in the project tree.
//
// Independent of `wake_listener.rs` and the rotation loop on purpose: a
// session with wake-on-demand listeners and no rotation stages never runs the
// rotation loop body, so anchoring the scrub there would miss exactly those
// long-lived sessions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::team_session::TeamSessionState;

/// How often this loop wakes to look for tokens left in persisted state.
const CHECK_INTERVAL: Duration = Duration::from_secs(1800); // 30 min

/// Lifetime of a per-launch whiteboard token. Each role launch gets a fresh
/// one, so this only needs to outlast a single role goal run.
pub const WHITEBOARD_TOKEN_TTL_SECS: u64 = 86400;

/// Spawns the daemon-lifetime scrub loop.
pub fn start(
    project_root: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            for id in TeamSessionState::list_ids(&project_root) {
                if let Err(e) = scrub_one_session(&project_root, &id) {
                    tracing::warn!(
                        session = %id,
                        error = %e,
                        "token_refresh: could not remove a persisted bearer token from the \
                         team-session state; run `ta doctor --fix` to clean it up"
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

/// Rewrites one session's state without any persisted token. Returns whether
/// a token was found and removed. Synchronous so it is directly testable.
fn scrub_one_session(project_root: &Path, id: &str) -> anyhow::Result<bool> {
    let Some(mut state) = TeamSessionState::load(project_root, id)? else {
        return Ok(false);
    };
    if state.config.whiteboard_token.is_none() {
        return Ok(false);
    }
    state.config.whiteboard_enabled = true;
    state.config.whiteboard_token = None;
    state.config.whiteboard_token_expires_at = None;
    state.save(project_root)?;
    tracing::info!(
        session = %id,
        "token_refresh: removed a persisted whiteboard token from the team-session state; \
         tokens are now minted per role launch"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_session::{TeamSessionConfig, TeamSessionStatus};

    fn sample_config(token: Option<&str>) -> TeamSessionConfig {
        TeamSessionConfig {
            name: "sess-1".to_string(),
            workflow_path: "wf.yaml".to_string(),
            team_toml_path: "team.toml".to_string(),
            objective: "test".to_string(),
            budget: None,
            role_prompts: std::collections::HashMap::new(),
            whiteboard_enabled: false,
            whiteboard_token: token.map(str::to_string),
            whiteboard_token_expires_at: None,
        }
    }

    fn state_json(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(".ta/team-sessions/sess-1/state.json")).unwrap()
    }

    #[test]
    fn a_session_with_no_token_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        TeamSessionState::new("sess-1".into(), sample_config(None), Vec::new())
            .save(dir.path())
            .unwrap();
        assert!(!scrub_one_session(dir.path(), "sess-1").unwrap());
    }

    #[test]
    fn saving_a_state_never_writes_the_token() {
        let dir = tempfile::tempdir().unwrap();
        TeamSessionState::new(
            "sess-1".into(),
            sample_config(Some("TOKEN-SENTINEL-1")),
            Vec::new(),
        )
        .save(dir.path())
        .unwrap();
        assert!(!state_json(dir.path()).contains("TOKEN-SENTINEL-1"));
    }

    #[test]
    fn a_legacy_state_file_with_a_token_is_scrubbed_but_keeps_the_session_working() {
        let dir = tempfile::tempdir().unwrap();
        TeamSessionState::new("sess-1".into(), sample_config(None), Vec::new())
            .save(dir.path())
            .unwrap();
        // Simulate a file written by the older version: token on disk.
        let path = dir.path().join(".ta/team-sessions/sess-1/state.json");
        let mut v: serde_json::Value = serde_json::from_str(&state_json(dir.path())).unwrap();
        v["config"]["whiteboard_token"] = "TOKEN-SENTINEL-2".into();
        v["config"]["whiteboard_token_expires_at"] = "2030-01-01T00:00:00Z".into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(state_json(dir.path()).contains("TOKEN-SENTINEL-2"));

        assert!(scrub_one_session(dir.path(), "sess-1").unwrap());

        assert!(!state_json(dir.path()).contains("TOKEN-SENTINEL-2"));
        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert!(
            reloaded.config.whiteboard_enabled,
            "the session must still launch roles with a whiteboard token"
        );
        assert!(reloaded.config.whiteboard_token.is_none());
        assert_eq!(reloaded.status, TeamSessionStatus::Active);
        // Idempotent.
        assert!(!scrub_one_session(dir.path(), "sess-1").unwrap());
    }

    #[test]
    fn scrubbing_a_nonexistent_session_is_a_harmless_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!scrub_one_session(dir.path(), "no-such-session").unwrap());
    }
}
