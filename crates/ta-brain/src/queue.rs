//! [`prioritize`] — route a batch of pending `TriggerEvent`s and order them
//! by [`Priority`], most urgent first. Shared by `ta intake coordinate`'s
//! team-coordinator recommendation path and any future batch consumer of
//! `.ta/intake-queue.jsonl` — one implementation, not reimplemented per
//! caller (§13.1).

use std::path::Path;

use ta_intake::TriggerEvent;

use crate::decision::RoutingDecision;
use crate::input::{RoutingInput, TriggerRoutingInput};
use crate::route::route;

/// Route every event in `events`, then sort by `RoutingDecision.priority`
/// descending (most urgent first), breaking ties by `occurred_at` ascending
/// (older events first — first-in-first-out within the same priority).
pub fn prioritize(
    events: &[TriggerEvent],
    workspace_root: &Path,
) -> Vec<(TriggerEvent, RoutingDecision)> {
    let mut routed: Vec<(TriggerEvent, RoutingDecision)> = events
        .iter()
        .map(|event| {
            let input = RoutingInput::Trigger(TriggerRoutingInput::from_event(event.clone()));
            let decision = route(&input, workspace_root);
            (event.clone(), decision)
        })
        .collect();

    routed.sort_by(|(a_event, a_decision), (b_event, b_decision)| {
        b_decision
            .priority
            .cmp(&a_decision.priority)
            .then_with(|| a_event.occurred_at.cmp(&b_event.occurred_at))
    });

    routed
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn make_event(title: &str, occurred_offset_secs: i64) -> TriggerEvent {
        TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "schedule".to_string(),
            source: "test".to_string(),
            occurred_at: Utc::now() + Duration::seconds(occurred_offset_secs),
            payload: serde_json::json!({}),
            suggested_goal_title: title.to_string(),
            dedupe_key: None,
        }
    }

    #[test]
    fn orders_by_priority_then_occurred_at() {
        let tmp = tempfile::tempdir().unwrap();
        let events = vec![
            make_event("Update README docs", 0),
            make_event("Production down, hotfix needed", 10),
            make_event("Add a new dashboard widget", 5),
            make_event("Fix login bug", 15),
        ];
        let routed = prioritize(&events, tmp.path());
        let titles: Vec<&str> = routed
            .iter()
            .map(|(e, _)| e.suggested_goal_title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec![
                "Production down, hotfix needed",
                "Fix login bug",
                "Add a new dashboard widget",
                "Update README docs",
            ]
        );
    }

    #[test]
    fn ties_break_by_occurred_at_ascending() {
        let tmp = tempfile::tempdir().unwrap();
        let events = vec![
            make_event("Add feature B", 20),
            make_event("Add feature A", 0),
        ];
        let routed = prioritize(&events, tmp.path());
        assert_eq!(routed[0].0.suggested_goal_title, "Add feature A");
        assert_eq!(routed[1].0.suggested_goal_title, "Add feature B");
    }
}
