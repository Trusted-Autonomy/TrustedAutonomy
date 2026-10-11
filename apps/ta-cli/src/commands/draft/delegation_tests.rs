// Agent-delegation labeling tests (CR-03 item 6): approve, deny and apply accepted from
// a detected agent context on the credential are recorded as `agent_delegated`, in the
// draft record and in the audit log, with the credential holder as the delegating owner.
// A plain terminal records no label.
//
// Every fixture lives in `tempfile::tempdir()`; nothing touches the real Keychain.

use ta_credentials::human;
use tempfile::TempDir;

use super::*;
use crate::commands::human_auth::{
    current_delegation, record_delegation, require_human_for, test_support_set_gate,
};

fn agent() -> Option<human::AgentContext> {
    Some(human::AgentContext {
        signals: vec!["CLAUDECODE is set".into(), "TA_AGENT_ID is set".into()],
    })
}

/// A project with one pending draft; returns (project, config, draft id).
fn project_with_draft() -> (TempDir, GatewayConfig, String) {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
    let config = GatewayConfig::for_project(project.path());
    super::super::goal::execute(
        &super::super::goal::GoalCommands::Start {
            title: "Delegation label test".to_string(),
            source: Some(project.path().to_path_buf()),
            objective: "test".to_string(),
            agent: "test-agent".to_string(),
            phase: None,
            follow_up: None,
            objective_file: None,
        },
        &config,
    )
    .unwrap();
    let goal = GoalRunStore::new(&config.goals_dir)
        .unwrap()
        .list()
        .unwrap()
        .remove(0);
    std::fs::write(goal.workspace_path.join("README.md"), "# Updated\n").unwrap();
    build_package(
        &config,
        &goal.goal_run_id.to_string(),
        "Delegation label test",
        false,
    )
    .unwrap();
    let id = load_all_packages(&config).unwrap()[0]
        .package_id
        .to_string();
    (project, config, id)
}

fn audit_events(config: &GatewayConfig) -> Vec<serde_json::Value> {
    std::fs::read_to_string(&config.audit_log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn only_context(config: &GatewayConfig, id: &str) -> Vec<ta_changeset::ActionContextRecord> {
    let pid = uuid::Uuid::parse_str(id).unwrap();
    load_package(config, pid).unwrap().action_contexts
}

#[test]
fn approve_from_an_agent_context_is_labeled_and_names_the_holder() {
    let (_p, config, id) = project_with_draft();
    test_support_set_gate(agent(), true);
    execute(
        &DraftCommands::Approve {
            id: Some(id.clone()),
            reviewer: "human-reviewer".into(),
            reviewer_as: Some("someone-the-agent-made-up".into()),
            force_override: false,
        },
        &config,
    )
    .unwrap();
    let holder = human::credential_holder();
    let pkg = load_package(&config, uuid::Uuid::parse_str(&id).unwrap()).unwrap();
    match &pkg.status {
        DraftStatus::Approved { approved_by, .. } => assert_eq!(approved_by, &holder),
        other => panic!("expected Approved, got {other:?}"),
    }
    let ctx = only_context(&config, &id);
    assert_eq!(ctx.len(), 1);
    assert_eq!(ctx[0].action, "approve");
    assert_eq!(ctx[0].actor_context, "agent_delegated");
    assert_eq!(ctx[0].delegating_owner, holder);
    assert_eq!(ctx[0].delegated_signals, agent().unwrap().signals);
    let ev = audit_events(&config);
    let ev = ev
        .iter()
        .find(|e| e["metadata"]["action"] == "approve")
        .expect("approve audit event");
    assert_eq!(ev["metadata"]["actor_context"], "agent_delegated");
    assert_eq!(ev["metadata"]["delegating_owner"], holder.as_str());
    assert_eq!(ev["metadata"]["delegated_signals"][0], "CLAUDECODE is set");
    assert_eq!(ev["agent_id"], holder.as_str());
    test_support_set_gate(None, false);
}

#[test]
fn deny_from_an_agent_context_is_labeled() {
    let (_p, config, id) = project_with_draft();
    test_support_set_gate(agent(), true);
    execute(
        &DraftCommands::Deny {
            id: Some(id.clone()),
            reason: "not good enough".into(),
            reviewer: "human-reviewer".into(),
            file: None,
        },
        &config,
    )
    .unwrap();
    let holder = human::credential_holder();
    let pkg = load_package(&config, uuid::Uuid::parse_str(&id).unwrap()).unwrap();
    match &pkg.status {
        DraftStatus::Denied { denied_by, .. } => assert_eq!(denied_by, &holder),
        other => panic!("expected Denied, got {other:?}"),
    }
    let ctx = only_context(&config, &id);
    assert_eq!(ctx.len(), 1);
    assert_eq!(ctx[0].action, "deny");
    assert_eq!(ctx[0].actor_context, "agent_delegated");
    assert!(audit_events(&config)
        .iter()
        .any(|e| e["metadata"]["action"] == "deny"
            && e["metadata"]["actor_context"] == "agent_delegated"));
    test_support_set_gate(None, false);
}

#[test]
fn apply_from_an_agent_context_is_labeled_in_record_and_audit_log() {
    // The full apply pipeline needs a VCS; the dispatch calls exactly this step
    // after a successful apply, so it is exercised directly.
    let (_p, config, id) = project_with_draft();
    test_support_set_gate(agent(), true);
    require_human_for("apply").unwrap();
    assert!(current_delegation("apply").is_some());
    record_delegation(&config, &id, "apply");
    let ctx = only_context(&config, &id);
    assert_eq!(ctx.len(), 1);
    assert_eq!(ctx[0].action, "apply");
    assert_eq!(ctx[0].actor_context, "agent_delegated");
    assert_eq!(ctx[0].delegating_owner, human::credential_holder());
    let events = audit_events(&config);
    let ev = events
        .iter()
        .find(|e| e["metadata"]["action"] == "apply")
        .expect("apply audit event");
    assert_eq!(ev["action"], "apply");
    assert_eq!(ev["metadata"]["actor_context"], "agent_delegated");
    test_support_set_gate(None, false);
}

#[test]
fn plain_terminal_actions_carry_no_label() {
    let (_p, config, id) = project_with_draft();
    test_support_set_gate(None, false);
    execute(
        &DraftCommands::Approve {
            id: Some(id.clone()),
            reviewer: "human-reviewer".into(),
            reviewer_as: None,
            force_override: false,
        },
        &config,
    )
    .unwrap();
    require_human_for("apply").unwrap();
    assert!(current_delegation("apply").is_none());
    record_delegation(&config, &id, "apply");
    assert!(only_context(&config, &id).is_empty());
    assert!(audit_events(&config)
        .iter()
        .all(|e| e["metadata"]["actor_context"].is_null()));
    let pkg = load_package(&config, uuid::Uuid::parse_str(&id).unwrap()).unwrap();
    match &pkg.status {
        DraftStatus::Approved { approved_by, .. } => assert_eq!(approved_by, "human-reviewer"),
        other => panic!("expected Approved, got {other:?}"),
    }
}

#[test]
fn a_stale_delegation_does_not_leak_into_a_later_direct_action() {
    test_support_set_gate(agent(), true);
    require_human_for("approve").unwrap();
    assert!(current_delegation("approve").is_some());
    test_support_set_gate(None, false);
    require_human_for("approve").unwrap();
    assert!(current_delegation("approve").is_none());
}

#[test]
fn drafts_and_labels_written_before_the_field_existed_still_load() {
    let (_p, config, id) = project_with_draft();
    let path = config.pr_packages_dir.join(format!("{id}.json"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        json.get("action_contexts").is_none(),
        "empty labels must not be serialized"
    );
    assert!(load_package(&config, uuid::Uuid::parse_str(&id).unwrap())
        .unwrap()
        .action_contexts
        .is_empty());
    // A label from a build that recorded no reasons still loads.
    let rec: ta_changeset::ActionContextRecord = serde_json::from_value(serde_json::json!({
        "action": "approve",
        "actor_context": "agent_delegated",
        "delegating_owner": "alice",
        "at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    assert!(rec.delegated_signals.is_empty());
    // An audit line without any of the new metadata still parses.
    let old = r#"{"event_id":"11111111-1111-1111-1111-111111111111","timestamp":"2026-01-01T00:00:00Z","agent_id":"a","action":"approval","target_uri":null,"input_hash":null,"output_hash":null,"parent_event_id":null,"previous_hash":null}"#;
    let ev: ta_audit::AuditEvent = serde_json::from_str(old).unwrap();
    assert!(ev.metadata.is_null());
}
