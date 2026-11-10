//! Publishing `launch_failed`: tell the Wayfinder poller that the agent for a
//! candidate could not start, so the task does not sit "in progress" with nobody
//! working on it.
//!
//! The daemon signs and publishes the outcome on the same report-back stream the
//! Chief of Staff uses (`external-outcome`), keyed to the candidate:
//!
//! ```json
//! {"candidate_id":"wayfinder-task:T#nonce","outcome":"launch_failed","kind":"launch_failed",
//!  "reason_code":"no_login","detail":"...","fix":"...","v":1,"stamp":{"role":"ta-daemon",...},"sig":{...}}
//! ```
//!
//! Only the daemon may do this (role `ta-daemon`); an agent token is refused at
//! `POST /api/whiteboard/outcome/send`. When the stream cannot be reached the
//! outcome is queued under `.ta/launch-failed-outbox/` and re-sent by
//! [`flush_outbox`] on the listener's next pass, so a transport hiccup does not
//! lose the report.
//!
//! Two sources feed this module:
//! - the wake path (`wake_retry`): a Chief-of-Staff launch for an intake message
//!   failed permanently or was dead-lettered; the candidate id is the intake
//!   message's, with its `#nonce`;
//! - [`crate::goal_watch`]: a worker goal launched by the poller ended `failed`
//!   with a recorded launch failure; the candidate id is the goal's bare id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ta_agent_whiteboard::outcome_envelope::{OutcomeBody, DAEMON_ROLE, OUTCOME_LAUNCH_FAILED};
use ta_agent_whiteboard::WhiteboardTransport;
use ta_goal::launch_failure::{classify_and_describe, LaunchEvidence, LaunchFailure};

use crate::api::whiteboard::EXTERNAL_OUTCOME_STREAM;
use crate::outcome_stamp::{stamp_and_sign, StampSubject};

/// Prefix of the first line of a launch error the daemon built itself. The rest
/// of the line is the JSON of a [`LaunchFailure`]. Only the FIRST line of an
/// error is ever read, so text an agent printed to stderr (which follows) cannot
/// forge it.
pub const ERROR_PREFIX: &str = "launch-failure-json: ";

/// Longest evidence line quoted in a `detail`, in characters.
const MAX_EVIDENCE_CHARS: usize = 160;

/// Directory (under the project's `.ta/`) holding outcomes awaiting re-send.
pub fn outbox_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("launch-failed-outbox")
}

/// An error string whose first line carries `failure` and whose remaining lines
/// are `human` (what the daemon logs).
pub fn encode_error(failure: &LaunchFailure, human: &str) -> String {
    let json = serde_json::to_string(failure).unwrap_or_default();
    format!("{ERROR_PREFIX}{json}\n{human}")
}

/// The failure a daemon-built error carries in its first line.
pub fn decode_error(err: &str) -> Option<LaunchFailure> {
    let first = err.lines().next()?;
    serde_json::from_str(first.strip_prefix(ERROR_PREFIX)?).ok()
}

/// The failure for a launch error: the one the daemon encoded, else classified
/// from the text (a plain error from a launcher that did not classify).
/// `project_root` decides whether a login failure's fix leads with Wayfinder Settings
/// (a Wayfinder-managed project) or with the CLI line.
pub fn failure_from_error(err: &str, project_root: &Path) -> LaunchFailure {
    decode_error(err).unwrap_or_else(|| {
        classify_and_describe(&LaunchEvidence {
            stderr: err,
            wayfinder_managed: ta_submit::WorkflowConfig::is_wayfinder_managed(project_root),
            ..Default::default()
        })
    })
}

/// Remove the value of every secret-looking environment variable (name ending
/// `_KEY`, `_TOKEN`, `_SECRET` or `_PASSWORD`; value at least 8 characters)
/// from `text`, from both the process environment and `extra_env`. Run after
/// `secret_redact` on any stderr line quoted in a `detail`.
pub fn scrub_secrets(text: &str, extra_env: &[(String, String)]) -> String {
    let secret_name = |k: &str| {
        let k = k.to_ascii_uppercase();
        ["_KEY", "_TOKEN", "_SECRET", "_PASSWORD"]
            .iter()
            .any(|s| k.ends_with(s))
    };
    let mut out = text.to_string();
    let process: Vec<(String, String)> = std::env::vars().collect();
    for (k, v) in process.iter().chain(extra_env.iter()) {
        if secret_name(k) && v.len() >= 8 {
            out = out.replace(v.as_str(), "[redacted]");
        }
    }
    out
}

/// Make the failed goal's recorded `launch_failure` say what the outcome says.
/// `ta run` records what it could see (exit status, whether the agent wrote
/// anything); the launcher also saw the agent's stderr and may know better
/// (`login_invalid` rather than `crashed`). Done before publishing, so
/// `ta goal status` and the outcome the poller reads never disagree. Best
/// effort: a goal that is not there or not failed is left alone.
pub fn refine_goal_record(project_root: &Path, goal_id: &str, failure: &LaunchFailure) {
    let Ok(id) = uuid::Uuid::parse_str(goal_id) else {
        return;
    };
    let goals_dir = ta_mcp_gateway::GatewayConfig::for_project(project_root).goals_dir;
    let Ok(store) = ta_goal::GoalRunStore::new(&goals_dir) else {
        return;
    };
    match store.get(id) {
        Ok(Some(mut goal))
            if matches!(goal.state, ta_goal::GoalRunState::Failed { .. })
                && goal.launch_failure.as_ref() != Some(failure) =>
        {
            goal.launch_failure = Some(failure.clone());
            if let Err(e) = store.save(&goal) {
                tracing::warn!(goal_id = %goal_id, error = %e, "could not update the goal's launch_failure; `ta goal status` may name a different reason than the launch_failed outcome");
            }
        }
        // The goal was left `completed` (an empty diff) or still `running` /
        // `finalizing`, but the launcher saw evidence the agent never got to work
        // (a sandbox denial). The outcome says failed, so the goal must too.
        Ok(Some(mut goal))
            if matches!(
                goal.state,
                ta_goal::GoalRunState::Completed
                    | ta_goal::GoalRunState::Running
                    | ta_goal::GoalRunState::Finalizing { .. }
            ) && goal.pr_package_id.is_none() =>
        {
            goal.state = ta_goal::GoalRunState::Failed {
                reason: format!("{} {}", failure.detail, failure.fix),
            };
            goal.launch_failure = Some(failure.clone());
            goal.updated_at = chrono::Utc::now();
            if let Err(e) = store.save(&goal) {
                tracing::warn!(goal_id = %goal_id, error = %e, "could not mark the goal failed; `ta goal status` may disagree with the launch_failed outcome");
            }
        }
        _ => {}
    }
}

/// One already-redacted, scrubbed, single-line quote of the last non-empty line
/// of `stderr`, for the `detail` of a failure.
pub fn evidence_line(stderr: &str, extra_env: &[(String, String)]) -> Option<String> {
    let last = stderr.lines().rev().map(str::trim).find(|l| {
        !l.is_empty() && !l.starts_with(ta_goal::launch_failure::LAUNCH_FAILURE_MARKER)
    })?;
    let redacted = ta_mcp_gateway::secret_redact::redact_lines(last);
    let scrubbed = scrub_secrets(&redacted, extra_env);
    Some(scrubbed.chars().take(MAX_EVIDENCE_CHARS).collect())
}

/// The candidate id of an intake message: its payload's top-level `candidate_id`.
pub fn candidate_id_from_payload(payload: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()?
        .get("candidate_id")?
        .as_str()
        .map(str::to_string)
        .filter(|id| !id.is_empty())
}

/// The stamp's `goal_id` when no goal was ever created for the candidate (the
/// launch died before `ta run` made one).
pub const NO_GOAL_ID: &str = "none-created";

/// The goal TA created for `candidate_id` (the newest one whose bare candidate id
/// matches), so the outcome's stamp names the real goal. `None` when there is no
/// such goal.
pub fn resolve_goal_id(project_root: &Path, candidate_id: &str) -> Option<String> {
    let bare = candidate_id.split('#').next().unwrap_or(candidate_id);
    let goals_dir = ta_mcp_gateway::GatewayConfig::for_project(project_root).goals_dir;
    let store = ta_goal::GoalRunStore::new(&goals_dir).ok()?;
    store
        .list()
        .ok()?
        .into_iter()
        .filter(|g| g.bare_candidate_id().as_deref() == Some(bare))
        .max_by_key(|g| g.created_at)
        .map(|g| g.goal_run_id.to_string())
}

/// The outcome body for `failure`, keyed to `candidate_id`.
pub fn outcome_body(candidate_id: &str, failure: &LaunchFailure) -> OutcomeBody {
    OutcomeBody {
        candidate_id: candidate_id.to_string(),
        outcome: OUTCOME_LAUNCH_FAILED.to_string(),
        detail: failure.detail.clone(),
        new_task_title: None,
        reason_code: Some(failure.code.as_str().to_string()),
        fix: Some(failure.fix.clone()),
    }
}

/// An outcome that has not reached the stream yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Queued {
    team_session: String,
    goal_id: Option<String>,
    candidate_id: String,
    failure: LaunchFailure,
}

/// Sign and append one `launch_failed` outcome. Returns the stamped `msg_id`.
pub async fn publish(
    transport: &dyn WhiteboardTransport,
    project_root: &Path,
    team_session: &str,
    goal_id: Option<&str>,
    candidate_id: &str,
    failure: &LaunchFailure,
) -> Result<String, String> {
    let subject = StampSubject {
        team_session: team_session.to_string(),
        role: DAEMON_ROLE.to_string(),
        goal_id: goal_id.unwrap_or(NO_GOAL_ID).to_string(),
        target: "local".to_string(),
    };
    let envelope = stamp_and_sign(project_root, &subject, outcome_body(candidate_id, failure))?;
    let msg_id = envelope.stamp.msg_id.clone();
    transport
        .stream_append(EXTERNAL_OUTCOME_STREAM, envelope.to_bytes())
        .await
        .map_err(|e| {
            format!(
                "could not append to the {EXTERNAL_OUTCOME_STREAM} stream ({e}). Check that the \
                 whiteboard transport (NATS) is reachable; the outcome is queued and re-sent"
            )
        })?;
    Ok(msg_id)
}

/// Publish, or queue under `.ta/launch-failed-outbox/` when that fails. Logs the
/// result either way. Never panics and never blocks the caller on a retry.
pub async fn publish_or_queue(
    transport: &dyn WhiteboardTransport,
    project_root: &Path,
    team_session: &str,
    goal_id: Option<&str>,
    candidate_id: &str,
    failure: &LaunchFailure,
) {
    match publish(
        transport,
        project_root,
        team_session,
        goal_id,
        candidate_id,
        failure,
    )
    .await
    {
        Ok(msg_id) => tracing::info!(
            candidate_id = %candidate_id,
            reason_code = %failure.code,
            goal_id = ?goal_id,
            msg_id = %msg_id,
            "launch_failed outcome published: {}",
            failure.headline()
        ),
        Err(e) => {
            tracing::error!(
                candidate_id = %candidate_id,
                reason_code = %failure.code,
                error = %e,
                "launch_failed outcome NOT published yet; queueing it in {} to re-send. The \
                 task stays in progress until it is delivered.",
                outbox_dir(project_root).display()
            );
            if let Err(qe) = queue(
                project_root,
                &Queued {
                    team_session: team_session.to_string(),
                    goal_id: goal_id.map(str::to_string),
                    candidate_id: candidate_id.to_string(),
                    failure: failure.clone(),
                },
            ) {
                tracing::error!(
                    error = %qe,
                    "could not queue the launch_failed outcome; it is lost. Check that .ta/ is writable"
                );
            }
        }
    }
}

fn queue(project_root: &Path, q: &Queued) -> std::io::Result<()> {
    let dir = outbox_dir(project_root);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}.json", uuid::Uuid::new_v4());
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(q)?)?;
    std::fs::rename(tmp, dir.join(name))
}

/// Re-send every queued outcome; remove the ones that went out. Returns how many
/// were delivered. A cheap directory read when nothing is queued.
pub async fn flush_outbox(transport: &dyn WhiteboardTransport, project_root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(outbox_dir(project_root)) else {
        return 0;
    };
    let mut sent = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let q: Queued = match serde_json::from_str(&raw) {
            Ok(q) => q,
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "unreadable queued launch_failed outcome; delete it");
                continue;
            }
        };
        match publish(
            transport,
            project_root,
            &q.team_session,
            q.goal_id.as_deref(),
            &q.candidate_id,
            &q.failure,
        )
        .await
        {
            Ok(msg_id) => {
                let _ = std::fs::remove_file(&path);
                sent += 1;
                tracing::info!(candidate_id = %q.candidate_id, msg_id = %msg_id, "queued launch_failed outcome delivered");
            }
            Err(e) => {
                tracing::warn!(candidate_id = %q.candidate_id, error = %e, "queued launch_failed outcome still undeliverable; will retry");
                break;
            }
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::launch_failure::LaunchFailureCode;

    #[test]
    fn whiteboard_and_goal_code_lists_are_the_same() {
        let goal: Vec<&str> = LaunchFailureCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            goal,
            ta_agent_whiteboard::outcome_envelope::LAUNCH_FAILURE_CODES.to_vec()
        );
    }

    #[test]
    fn only_the_first_line_of_an_error_is_trusted() {
        let f = LaunchFailure {
            code: LaunchFailureCode::NoLogin,
            detail: "d".into(),
            fix: "f".into(),
        };
        let err = encode_error(&f, "human text");
        assert_eq!(decode_error(&err), Some(f));
        let forged = format!(
            "human\n{ERROR_PREFIX}{{\"code\":\"no_login\",\"detail\":\"x\",\"fix\":\"y\"}}"
        );
        assert_eq!(decode_error(&forged), None);
        // A plain error is classified from its text instead.
        assert_eq!(
            failure_from_error("'claude' command not found", Path::new("/nonexistent")).code,
            LaunchFailureCode::AgentMissing
        );
        assert_eq!(
            failure_from_error("boom", Path::new("/nonexistent")).code,
            LaunchFailureCode::Crashed
        );
    }

    #[test]
    fn candidate_id_comes_from_the_payload() {
        assert_eq!(
            candidate_id_from_payload(br##"{"candidate_id":"wayfinder-task:T1#n"}"##).as_deref(),
            Some("wayfinder-task:T1#n")
        );
        assert_eq!(candidate_id_from_payload(b"not json"), None);
        assert_eq!(candidate_id_from_payload(br#"{"candidate_id":""}"#), None);
        assert_eq!(candidate_id_from_payload(br#"{"candidate_id":7}"#), None);
    }

    #[test]
    fn secrets_never_survive_into_an_evidence_line() {
        let env = vec![(
            "MY_SERVICE_TOKEN".to_string(),
            "tok-1234567890abc".to_string(),
        )];
        let stderr = format!(
            "starting\nrequest failed with tok-1234567890abc and {}{}\n",
            "sk-ant-api", "03-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        );
        let line = evidence_line(&stderr, &env).unwrap();
        assert!(!line.contains("tok-1234567890abc"), "{line}");
        assert!(!line.contains("sk-ant-api03"), "{line}");
        assert_eq!(evidence_line("\n  \n", &env), None);
    }

    #[test]
    fn the_stamp_goal_id_is_the_real_goal_of_the_candidate_or_a_clear_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(resolve_goal_id(tmp.path(), "wayfinder-task:T-7#n1"), None);
        let config = ta_mcp_gateway::GatewayConfig::for_project(tmp.path());
        let store = ta_goal::GoalRunStore::new(&config.goals_dir).unwrap();
        let mut goal = ta_goal::GoalRun::new(
            "t",
            "o",
            "claude-code",
            std::path::PathBuf::from("/w"),
            std::path::PathBuf::from("/s"),
        );
        goal.candidate_id = Some("wayfinder-task:T-7".to_string());
        store.save(&goal).unwrap();
        let mut other = goal.clone();
        other.goal_run_id = uuid::Uuid::new_v4();
        other.candidate_id = Some("wayfinder-task:T-8".to_string());
        store.save(&other).unwrap();
        assert_eq!(
            resolve_goal_id(tmp.path(), "wayfinder-task:T-7#n1"),
            Some(goal.goal_run_id.to_string()),
            "the wake path's full id (with #nonce) finds the goal"
        );
        assert_ne!(NO_GOAL_ID, "unresolved");
    }
}
