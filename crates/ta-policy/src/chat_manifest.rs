// chat_manifest.rs — Chat-scoped capability manifest construction.
//
// Builds the narrow "chat mode" CapabilityManifest described in
// wayfinder/docs/superpowers/specs/2026-10-03-multi-project-vt-agnostic-coordination-design.md
// §8: read anywhere in project context, write only to an ephemeral
// scratch path, no grants for git/email/Drive/social tools. Composed
// from two `PolicyCompiler::compile()` calls (broad-read +
// narrow-scratch-write) rather than extending the compiler itself,
// since `CompilerOptions` applies one `resource_scope` uniformly to
// every grant produced by a single `compile()` call — composition, not
// modification, of an already-tested security-critical path.
//
// Ephemeral scratch path convention (newly established by this module;
// nothing matching this purpose existed anywhere in this codebase
// before it): `.ta/chat-scratch/` relative to the workspace root. Any
// future caller wiring this into a real goal/session launch path must
// create this directory and treat its contents as disposable.
//
// Whiteboard delivery: NOT separately gated here, by design, not
// oversight. Confirmed by source inspection
// (`crates/ta-mcp-gateway/src/tools/whiteboard.rs`,
// `crates/ta-daemon/src/api/whiteboard.rs`) that whiteboard read/write
// operations have zero references to `PolicyEngine`/`CapabilityManifest`/
// `ta_policy` anywhere — they are not currently checked against any
// capability manifest at all. "Whiteboard delivery allowed" in chat
// mode therefore requires no new grant type: nothing today would check
// for one. If whiteboard access is ever brought under capability-manifest
// gating, this module will need a corresponding grant added then.
//
// No grants for git/email/Drive/social tools: this falls out of
// default-deny with zero extra code — `chat_read_profile`/
// `chat_scratch_write_profile` below simply never list any
// `git_*`/`email_*`/`drive_*`/social-tool action in `bounded_actions`,
// and `PolicyEngine::evaluate` denies any `(tool, verb)` pair with no
// matching grant (confirmed directly: `crates/ta-policy/src/engine.rs`'s
// `evaluate` denies when `self.manifests.get(&request.agent_id)` is
// `None`, and `has_matching_grant` is a positive match against the
// grants list with no separate allow-by-default path for anything).

use crate::alignment::{AlignmentProfile, AutonomyEnvelope, CoordinationConfig};
use crate::capability::CapabilityManifest;
use crate::compiler::{CompilerError, CompilerOptions, PolicyCompiler};

/// Relative path (from the workspace root) of the ephemeral scratch
/// directory chat-mode write access is scoped to. Not an existing
/// convention — established by this module.
pub const CHAT_SCRATCH_DIR: &str = ".ta/chat-scratch";

/// The chat-mode read `AlignmentProfile`: broad `fs_read`, no write of
/// any kind. Write access is granted separately (see
/// `chat_scratch_write_profile`), scoped by the caller to
/// `CHAT_SCRATCH_DIR`, never to the rest of the workspace.
fn chat_read_profile() -> AlignmentProfile {
    AlignmentProfile {
        principal: "chat-session".to_string(),
        autonomy_envelope: AutonomyEnvelope {
            bounded_actions: vec!["fs_read".to_string()],
            escalation_triggers: vec![],
            forbidden_actions: vec![
                "network_external".to_string(),
                "credential_access".to_string(),
            ],
        },
        constitution: "default-v1".to_string(),
        coordination: CoordinationConfig::default(),
    }
}

/// The chat-mode scratch-write `AlignmentProfile`: one bounded action
/// (`fs_write_patch`) — the caller must scope this with a
/// `CompilerOptions.resource_scope` limited to `CHAT_SCRATCH_DIR`, never
/// the broad workspace pattern used for `chat_read_profile`.
fn chat_scratch_write_profile() -> AlignmentProfile {
    AlignmentProfile {
        principal: "chat-session".to_string(),
        autonomy_envelope: AutonomyEnvelope {
            bounded_actions: vec!["fs_write_patch".to_string()],
            escalation_triggers: vec![],
            forbidden_actions: vec![
                "network_external".to_string(),
                "credential_access".to_string(),
            ],
        },
        constitution: "default-v1".to_string(),
        coordination: CoordinationConfig::default(),
    }
}

/// Compile the chat-mode `CapabilityManifest`: broad `fs_read` across
/// `workspace_resource_scope`, plus narrow `fs_write_patch` scoped only
/// to `{workspace_resource_scope}/.ta/chat-scratch/**` — never
/// git/email/Drive/social tools (simply never granted, so default-deny
/// handles them with no extra code; see this module's own header
/// comment).
///
/// `workspace_resource_scope` is expected in the same shape
/// `PolicyCompiler`'s own default uses elsewhere (e.g.
/// `"fs://workspace/**"`) — a glob ending in `/**`, since the scratch
/// pattern is derived by trimming that suffix and appending the scratch
/// subdirectory.
pub fn compile_chat_manifest(
    agent_id: &str,
    workspace_resource_scope: &str,
    validity_hours: i64,
) -> Result<CapabilityManifest, CompilerError> {
    let read_options = CompilerOptions {
        resource_scope: vec![workspace_resource_scope.to_string()],
        validity_hours,
    };
    let mut manifest = PolicyCompiler::compile(agent_id, &chat_read_profile(), &read_options)?;

    let scratch_pattern = format!(
        "{}/{}/**",
        workspace_resource_scope.trim_end_matches("/**"),
        CHAT_SCRATCH_DIR
    );
    let write_options = CompilerOptions {
        resource_scope: vec![scratch_pattern],
        validity_hours,
    };
    let write_manifest =
        PolicyCompiler::compile(agent_id, &chat_scratch_write_profile(), &write_options)?;

    manifest.grants.extend(write_manifest.grants);
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PolicyDecision, PolicyEngine, PolicyRequest};

    const WORKSPACE: &str = "fs://workspace/**";

    #[test]
    fn manifest_has_broad_read_and_narrow_scratch_write_grants() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();

        let read_grant = manifest
            .grants
            .iter()
            .find(|g| g.tool == "fs" && g.verb == "read")
            .expect("expected an fs read grant");
        assert_eq!(read_grant.resource_pattern, "fs://workspace/**");

        let write_grant = manifest
            .grants
            .iter()
            .find(|g| g.tool == "fs" && g.verb == "write_patch")
            .expect("expected an fs write_patch grant");
        assert_eq!(
            write_grant.resource_pattern,
            "fs://workspace/.ta/chat-scratch/**"
        );
    }

    #[test]
    fn policy_engine_allows_read_anywhere_in_workspace() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "chat-agent".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/src/main.rs".to_string(),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn policy_engine_allows_write_inside_scratch() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "chat-agent".to_string(),
            tool: "fs".to_string(),
            verb: "write_patch".to_string(),
            target_uri: "fs://workspace/.ta/chat-scratch/notes.md".to_string(),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    /// This is the test that actually proves the security boundary:
    /// a write OUTSIDE the scratch path must be denied by the real
    /// `PolicyEngine`, not merely absent from a hand-inspected grants
    /// list.
    #[test]
    fn policy_engine_denies_write_outside_scratch() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "chat-agent".to_string(),
            tool: "fs".to_string(),
            verb: "write_patch".to_string(),
            target_uri: "fs://workspace/src/main.rs".to_string(),
        });
        assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "expected Deny for a write outside the scratch path, got {:?}",
            decision
        );
    }

    #[test]
    fn policy_engine_denies_git_push_with_no_grant() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "chat-agent".to_string(),
            tool: "git".to_string(),
            verb: "push".to_string(),
            target_uri: "fs://workspace/**".to_string(),
        });
        assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "expected Deny for git push with no grant, got {:?}",
            decision
        );
    }

    #[test]
    fn policy_engine_denies_email_send_with_no_grant() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        let decision = engine.evaluate(&PolicyRequest {
            agent_id: "chat-agent".to_string(),
            tool: "email".to_string(),
            verb: "send".to_string(),
            target_uri: "mailto:someone@example.com".to_string(),
        });
        assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "expected Deny for email send with no grant, got {:?}",
            decision
        );
    }
}
