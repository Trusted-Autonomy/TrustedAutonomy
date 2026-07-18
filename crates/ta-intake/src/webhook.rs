//! `webhook` trigger type — fires when a GitHub `pull_request` merge event
//! that matches this project's build-milestone automation config lands in
//! the event store. Added for v0.17.0.12.31 (Build-Milestone Automation:
//! VCS-Triggered Phase Continuation).
//!
//! This is deliberately *not* an HTTP listener: `ta-daemon`'s existing
//! `/api/webhooks/github` and `/api/webhooks/vcs` endpoints
//! (`crates/ta-daemon/src/api/webhooks.rs`, v0.14.8.3) already validate,
//! parse, and persist inbound GitHub/VCS events to `.ta/events/<date>.jsonl`
//! as `SessionEvent::VcsPrMerged`. This trigger type polls that same event
//! store (via `ta_events::store::FsEventStore`) the same way `schedule`
//! polls a clock and `inbound-email` polls a mailbox — one normalized
//! `TriggerSource::poll()` seam, no second event-ingestion mechanism.
//!
//! **Opt-in by design (PLAN.md v0.17.0.12.31 item 5)**: this trigger only
//! exists (and therefore only fires) when a project explicitly creates
//! `.ta/triggers/webhook.toml` — nothing here runs by default. On top of
//! that, every fired event is *also* required to carry a specific PR label
//! (`require_label`, default `"ta-automation"`) before it matches, so a
//! human's unrelated merged PR on the same repo/branch can never
//! accidentally trigger an autonomous phase launch — the label has to be
//! applied by the automation itself (see `ta workflow build-milestone`).

use crate::event::{TriggerError, TriggerEvent, TriggerSource};
use crate::manifest::TriggerManifest;
use std::path::{Path, PathBuf};
use ta_events::schema::SessionEvent;
use ta_events::store::{EventQueryFilter, EventStore, FsEventStore};
use uuid::Uuid;

const DEFAULT_BASE_BRANCH: &str = "main";
const DEFAULT_HEAD_BRANCH_PREFIX: &str = "feature/";
const DEFAULT_REQUIRE_LABEL: &str = "ta-automation";

pub struct WebhookTriggerSource {
    manifest: TriggerManifest,
    events_dir: PathBuf,
}

impl WebhookTriggerSource {
    /// `project_root` is used to locate the event store (`.ta/events/`)
    /// unless overridden by the manifest's `events_dir` setting (used by
    /// tests to point at a temp directory without a real `.ta/`).
    pub fn new(manifest: TriggerManifest, project_root: &Path) -> Self {
        let events_dir = manifest
            .get_str("events_dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| project_root.join(".ta").join("events"));
        Self {
            manifest,
            events_dir,
        }
    }

    fn repo(&self) -> Option<&str> {
        self.manifest.get_str("repo")
    }

    fn base_branch(&self) -> &str {
        self.manifest
            .get_str("base_branch")
            .unwrap_or(DEFAULT_BASE_BRANCH)
    }

    fn head_branch_prefix(&self) -> &str {
        self.manifest
            .get_str("head_branch_prefix")
            .unwrap_or(DEFAULT_HEAD_BRANCH_PREFIX)
    }

    /// The PR label that must be present for a merged PR to match. An empty
    /// string in config explicitly disables the label requirement — still
    /// gated by the manifest file itself having to exist (opt-in per
    /// project), but a project can choose to trust repo/branch matching
    /// alone if it accepts the reduced blast-radius protection.
    fn require_label(&self) -> Option<&str> {
        match self.manifest.get_str("require_label") {
            Some("") => None,
            Some(label) => Some(label),
            None => Some(DEFAULT_REQUIRE_LABEL),
        }
    }
}

/// Extract a plan phase ID (e.g. `"v0.17.0.12.31"`) from a PR title.
///
/// Mirrors `apps/ta-cli/src/commands/plan.rs::extract_semver_from_title` —
/// duplicated rather than shared because `ta-intake` doesn't (and shouldn't)
/// depend on the `ta-cli` binary crate. Both match the same PR title
/// convention this codebase's own automation produces: `"Implement
/// v0.17.0.12.31 — Build-Milestone Automation: ..."`, optionally behind a
/// `[<shortref>]` prefix added by `ta-submit`'s PR-open step.
pub fn extract_phase_id_from_title(title: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?:^|\s)(v\d+(?:\.\d+)*)").ok()?;
    re.captures(title)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

impl TriggerSource for WebhookTriggerSource {
    fn trigger_type(&self) -> &str {
        "webhook"
    }

    fn poll(&self, since: Option<&str>) -> Result<Vec<TriggerEvent>, TriggerError> {
        let repo = self.repo().ok_or_else(|| TriggerError::PollFailed {
            trigger_type: "webhook".into(),
            reason: "trigger config is missing required `repo` setting (e.g. \
                      settings.repo = \"org/repo\") — see docs/USAGE.md \"Trigger Layer\""
                .into(),
        })?;

        let since_dt = match since {
            None => None,
            Some(watermark) => Some(
                chrono::DateTime::parse_from_rfc3339(watermark)
                    .map_err(|e| TriggerError::PollFailed {
                        trigger_type: "webhook".into(),
                        reason: format!("watermark '{watermark}' is not RFC 3339: {e}"),
                    })?
                    .with_timezone(&chrono::Utc),
            ),
        };

        let store = FsEventStore::new(&self.events_dir);
        let envelopes = store
            .query(&EventQueryFilter {
                event_types: vec!["vcs.pr_merged".to_string()],
                since: since_dt,
                ..Default::default()
            })
            .map_err(|e| TriggerError::PollFailed {
                trigger_type: "webhook".into(),
                reason: format!(
                    "failed to read event store at {}: {e}",
                    self.events_dir.display()
                ),
            })?;

        let base_branch = self.base_branch();
        let head_branch_prefix = self.head_branch_prefix();
        let require_label = self.require_label();

        let mut events = Vec::new();
        for envelope in envelopes {
            let SessionEvent::VcsPrMerged {
                repo: event_repo,
                branch,
                pr_number,
                pr_title,
                merged_by,
                head_branch,
                labels,
                ..
            } = envelope.payload
            else {
                continue;
            };

            if event_repo != repo {
                continue;
            }
            if branch != base_branch {
                continue;
            }
            if !head_branch.starts_with(head_branch_prefix) {
                continue;
            }
            if let Some(label) = require_label {
                if !labels.iter().any(|l| l == label) {
                    continue;
                }
            }

            let Some(source_phase) = extract_phase_id_from_title(&pr_title) else {
                tracing::warn!(
                    pr_number,
                    pr_title = %pr_title,
                    "webhook trigger: merged PR matched repo/branch/label but its title has \
                     no vX.Y.Z phase prefix — skipping, cannot resolve which phase this closes"
                );
                continue;
            };

            events.push(TriggerEvent {
                id: Uuid::new_v4(),
                trigger_type: "webhook".into(),
                source: format!("github:{repo}#{pr_number}"),
                occurred_at: envelope.timestamp,
                payload: serde_json::json!({
                    "repo": event_repo,
                    "base_branch": branch,
                    "head_branch": head_branch,
                    "pr_number": pr_number,
                    "pr_title": pr_title,
                    "merged_by": merged_by,
                    "source_phase": source_phase,
                }),
                suggested_goal_title: format!("build-milestone: continue after {source_phase}"),
                dedupe_key: Some(format!("webhook:{repo}:pr{pr_number}")),
            });
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Dispatch;
    use std::collections::HashMap;
    use ta_events::schema::EventEnvelope;

    fn manifest(settings: HashMap<String, toml::Value>) -> TriggerManifest {
        TriggerManifest {
            trigger_type: "webhook".into(),
            enabled: true,
            dispatch: Dispatch::Direct,
            description: None,
            settings,
        }
    }

    fn settings_with(pairs: &[(&str, &str)]) -> HashMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), toml::Value::String(v.to_string())))
            .collect()
    }

    fn write_pr_merged_event(
        events_dir: &Path,
        repo: &str,
        base_branch: &str,
        head_branch: &str,
        pr_title: &str,
        labels: Vec<&str>,
    ) {
        let store = FsEventStore::new(events_dir);
        let event = SessionEvent::VcsPrMerged {
            repo: repo.to_string(),
            branch: base_branch.to_string(),
            pr_number: 42,
            pr_title: pr_title.to_string(),
            merged_by: "alice".to_string(),
            commit_sha: "abc123".to_string(),
            provider: "github".to_string(),
            head_branch: head_branch.to_string(),
            labels: labels.into_iter().map(String::from).collect(),
        };
        store.append(&EventEnvelope::new(event)).unwrap();
    }

    fn source_for(dir: &Path, extra: &[(&str, &str)]) -> WebhookTriggerSource {
        let events_dir = dir.join(".ta").join("events");
        let mut pairs = vec![
            ("repo", "org/repo"),
            ("events_dir", events_dir.to_str().unwrap()),
        ];
        pairs.extend_from_slice(extra);
        WebhookTriggerSource::new(manifest(settings_with(&pairs)), dir)
    }

    #[test]
    fn extracts_phase_id_from_pr_title() {
        assert_eq!(
            extract_phase_id_from_title(
                "[abc1234] Implement v0.17.0.12.31 — Build-Milestone Automation"
            ),
            Some("v0.17.0.12.31".to_string())
        );
        assert_eq!(extract_phase_id_from_title("No phase here"), None);
    }

    #[test]
    fn fires_for_matching_labeled_pr_merged_event() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["source_phase"], "v0.17.0.12.30");
        assert_eq!(
            events[0].dedupe_key.as_deref(),
            Some("webhook:org/repo:pr42")
        );
    }

    #[test]
    fn does_not_fire_without_required_label() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["some-other-label"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert!(
            events.is_empty(),
            "a merged PR without the automation's label must never fire — \
             this is the guard against an unrelated human PR accidentally \
             triggering an autonomous phase launch"
        );
    }

    #[test]
    fn does_not_fire_for_wrong_repo() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "someone-else/unrelated-repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn does_not_fire_for_wrong_base_branch() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "release",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn does_not_fire_for_head_branch_outside_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "hotfix/urgent-thing",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn does_not_fire_for_title_without_phase_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/some-change",
            "Fix a typo in the README",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let events = source.poll(None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn custom_require_label_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["custom-marker"],
        );

        let source = source_for(dir.path(), &[("require_label", "custom-marker")]);
        let events = source.poll(None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn empty_require_label_disables_label_check() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec![],
        );

        let source = source_for(dir.path(), &[("require_label", "")]);
        let events = source.poll(None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn missing_repo_setting_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = WebhookTriggerSource::new(manifest(HashMap::new()), dir.path());
        let result = source.poll(None);
        assert!(matches!(result, Err(TriggerError::PollFailed { .. })));
    }

    #[test]
    fn watermark_filters_out_already_seen_events() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        write_pr_merged_event(
            &events_dir,
            "org/repo",
            "main",
            "feature/phase-30",
            "Implement v0.17.0.12.30 — Draft Count Fixes",
            vec!["ta-automation"],
        );

        let source = source_for(dir.path(), &[]);
        let future_watermark = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let events = source.poll(Some(&future_watermark)).unwrap();
        assert!(events.is_empty());
    }
}
