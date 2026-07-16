//! Round-trip / backward-compatibility guard for the five published spec
//! types (v0.17.0.12.21 item 5): a frozen, hand-written JSON example per
//! type, captured against today's field set. If a future change to the
//! struct breaks deserialization of these examples — a required field
//! renamed or removed without a compatible default — this test fails CI,
//! which is exactly the "a schema change that breaks an existing serialized
//! example fails CI" guarantee the phase asks for.
//!
//! Mirrors the existing inline-JSON-literal test convention used elsewhere
//! in this codebase (e.g. `ta-changeset::draft_package`'s
//! `artifact_without_new_fields_deserializes_with_defaults`) rather than
//! introducing separate fixture files.

use ta_brain::RoutingDecision;
use ta_changeset::draft_package::{Artifact, DraftPackage};
use ta_goal::{GoalRun, PersonaConfig};
use ta_intake::TriggerEvent;

#[test]
fn goal_run_example_deserializes() {
    let json = r#"{
        "goal_run_id": "5b1b1b1b-1b1b-4b1b-8b1b-1b1b1b1b1b1b",
        "title": "Fix authentication bug",
        "objective": "Resolve the session timeout issue",
        "agent_id": "claude-code",
        "state": { "state": "created" },
        "manifest_id": "6c2c2c2c-2c2c-4c2c-8c2c-2c2c2c2c2c2c",
        "workspace_path": "/tmp/staging",
        "store_path": "/tmp/store",
        "pr_package_id": null,
        "created_at": "2026-07-01T00:00:00Z",
        "updated_at": "2026-07-01T00:00:00Z"
    }"#;
    let restored: GoalRun = serde_json::from_str(json).expect("GoalRun example must deserialize");
    assert_eq!(restored.title, "Fix authentication bug");
    assert_eq!(restored.state, ta_goal::GoalRunState::Created);

    // Round trip: re-serializing and re-deserializing must be lossless.
    let reserialized = serde_json::to_string(&restored).unwrap();
    let twice: GoalRun = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(twice.goal_run_id, restored.goal_run_id);
}

#[test]
fn artifact_example_deserializes() {
    let json = r#"{
        "resource_uri": "fs://workspace/src/main.rs",
        "change_type": "modify",
        "diff_ref": "changeset:0"
    }"#;
    let restored: Artifact = serde_json::from_str(json).expect("Artifact example must deserialize");
    assert_eq!(restored.resource_uri, "fs://workspace/src/main.rs");
}

#[test]
fn draft_example_deserializes() {
    let json = r#"{
        "package_version": "1.0.0",
        "package_id": "7d3d3d3d-3d3d-4d3d-8d3d-3d3d3d3d3d3d",
        "created_at": "2026-07-01T00:00:00Z",
        "goal": {
            "goal_id": "goal-1",
            "title": "Fix authentication bug",
            "objective": "Resolve the session timeout issue",
            "success_criteria": ["Tests pass"]
        },
        "iteration": {
            "iteration_id": "iter-1",
            "sequence": 1,
            "workspace_ref": { "type": "git", "ref": "main", "base_ref": null }
        },
        "agent_identity": {
            "agent_id": "claude-code",
            "agent_type": "claude-code",
            "constitution_id": "default",
            "capability_manifest_hash": "hash123",
            "orchestrator_run_id": null
        },
        "summary": {
            "what_changed": "Fixed the session timeout",
            "why": "Sessions were expiring early",
            "impact": "Users stay logged in correctly",
            "rollback_plan": "Revert this commit"
        },
        "plan": { "completed_steps": [], "next_steps": [] },
        "changes": { "artifacts": [], "patch_sets": [] },
        "risk": { "risk_score": 0, "findings": [], "policy_decisions": [] },
        "provenance": { "inputs": [], "tool_trace_hash": "trace-hash" },
        "review_requests": {
            "requested_actions": [],
            "reviewers": ["human-reviewer"],
            "notes_to_reviewer": null
        },
        "signatures": {
            "package_hash": "pkg-hash",
            "agent_signature": "sig",
            "gateway_attestation": null
        }
    }"#;
    let restored: DraftPackage =
        serde_json::from_str(json).expect("DraftPackage example must deserialize");
    assert_eq!(restored.package_version, "1.0.0");
    assert_eq!(
        restored.status,
        ta_changeset::draft_package::DraftStatus::Draft
    );
}

#[test]
fn trigger_event_example_deserializes() {
    let json = r#"{
        "id": "8e4e4e4e-4e4e-4e4e-8e4e-4e4e4e4e4e4e",
        "trigger_type": "schedule",
        "source": "nightly-report",
        "occurred_at": "2026-07-01T00:00:00Z",
        "payload": {"cron": "0 0 * * *"},
        "suggested_goal_title": "Run nightly report",
        "dedupe_key": null
    }"#;
    let restored: TriggerEvent =
        serde_json::from_str(json).expect("TriggerEvent example must deserialize");
    assert_eq!(restored.trigger_type, "schedule");
}

#[test]
fn routing_decision_example_deserializes() {
    let json = r#"{
        "team": "implementer",
        "agent": "claude-code",
        "security_tier": "suggest",
        "priority": "normal",
        "workload_type": "bugfix",
        "workload_confidence": 1.0,
        "rationale": ["agent: tier=explicit value=claude-code"]
    }"#;
    let restored: RoutingDecision =
        serde_json::from_str(json).expect("RoutingDecision example must deserialize");
    assert_eq!(restored.agent, "claude-code");
}

#[test]
fn persona_example_deserializes() {
    let json = r#"{
        "persona": { "name": "financial-analyst" }
    }"#;
    let restored: PersonaConfig =
        serde_json::from_str(json).expect("PersonaConfig example must deserialize");
    assert_eq!(restored.persona.name, "financial-analyst");
}
