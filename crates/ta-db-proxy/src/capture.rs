//! Capture lifecycle for a database proxy plugin (v0.17.1).
//!
//! `start_capture` begins change-capture at goal start (a logical replication
//! slot for Postgres, a recorded binlog position for MySQL, a shadow-copy
//! snapshot for SQLite — engine-specific, opaque to TA core). `stop_capture`
//! ends it at goal apply/deny time: `Apply` drains captured changes into the
//! goal's `db-overlay.jsonl` (the same file `ta_db_overlay::DraftOverlay`
//! reads), `Discard` throws them away. Either way the engine-native capture
//! resource (replication slot, etc.) is released so it never leaks past the
//! goal's lifetime.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Parameters passed to `start_capture`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureParams {
    /// The goal this capture is scoped to — used to derive a stable,
    /// collision-resistant capture resource name (e.g. replication slot).
    pub goal_id: String,
    /// Staging directory for this goal. Captured mutations are written to
    /// `<staging_dir>/db-overlay.jsonl` on `stop_capture(Apply)`.
    pub staging_dir: PathBuf,
    /// The real database connection string. Resolved exclusively from the
    /// credential vault by the caller — never handed to the agent directly.
    pub upstream_dsn: String,
}

/// Opaque handle returned by `start_capture`. TA core persists this
/// (typically alongside goal state) and passes it back to `stop_capture`
/// unmodified — it never inspects `cursor`, which is entirely engine-defined
/// (e.g. `{"slot_name": "..."}` for Postgres, `{"file": "...", "pos": ...}`
/// for MySQL, `{"shadow_db": "..."}` for SQLite).
///
/// **`cursor` must never contain `upstream_dsn` or any other value with
/// embedded credentials.** Because TA core persists this handle to disk, a
/// raw DSN (`postgres://user:pass@host/db`) round-tripped through it would
/// leak plaintext DB credentials into on-disk goal state. If a plugin needs
/// the DSN again at `stop_capture` time, it must be re-resolved by the
/// caller from the credential vault and passed as a fresh `upstream_dsn`
/// argument to that call — the same pattern `apply_mutation` already uses —
/// never sourced from `cursor`. A plugin may still put a *sanitized*
/// connection identifier (host/port/dbname, no credentials) in `cursor` for
/// its own use in building display strings or captured URIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureHandle {
    pub engine: String,
    pub cursor: serde_json::Value,
}

/// What to do with captured mutations when `stop_capture` is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureAction {
    /// Drain captured changes into `db-overlay.jsonl` for review/replay.
    Apply,
    /// Discard captured changes without writing them anywhere.
    Discard,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_action_round_trips_json() {
        let apply = serde_json::to_string(&CaptureAction::Apply).unwrap();
        assert_eq!(apply, "\"apply\"");
        let discard: CaptureAction = serde_json::from_str("\"discard\"").unwrap();
        assert_eq!(discard, CaptureAction::Discard);
    }

    #[test]
    fn capture_handle_round_trips_opaque_cursor() {
        let handle = CaptureHandle {
            engine: "postgres".to_string(),
            cursor: serde_json::json!({"slot_name": "ta_goal_123"}),
        };
        let text = serde_json::to_string(&handle).unwrap();
        let back: CaptureHandle = serde_json::from_str(&text).unwrap();
        assert_eq!(back.engine, "postgres");
        assert_eq!(back.cursor["slot_name"], "ta_goal_123");
    }
}
