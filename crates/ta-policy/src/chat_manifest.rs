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

/// Biscuit scope carrying the right to read the real project root.
pub const CHAT_SCOPE_PROJECT_READ: &str = "fs:read:project";
const CHAT_SCOPE_READABLE_PREFIX: &str = "fs:read:readable:";
const CHAT_SCOPE_TOOL_PREFIX: &str = "tool:read:";

/// Resource pattern the real project root's files are checked against.
pub const CHAT_PROJECT_RESOURCE_SCOPE: &str = "fs://project/**";

/// Resource URI prefix of one registry-listed readable project.
pub fn chat_readable_resource_prefix(label: &str) -> String {
    format!("fs://readable/{label}/")
}

/// Resource URI a read-only chat tool (a Wayfinder or wiki read, plan
/// status, community lookup) is checked against.
pub fn chat_tool_resource(tool: &str) -> String {
    format!("tool://{tool}")
}

/// A label naming one `[chat] readable_projects` entry. Labels become part
/// of a policy resource URI and of a comma-joined biscuit scope list, so
/// they are restricted to characters that can never be glob syntax, path
/// separators or list separators.
pub fn is_valid_readable_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label != "."
        && label != ".."
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_valid_tool_name(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What a read-only chat session (the Chief of Staff) may read beyond its
/// own session directory: the real project root, other registry-listed
/// projects, and the named read tools that load project configuration
/// (Wayfinder reads, wiki, plan status, community lookup).
///
/// This is a grant, not a mode: it is minted by the launcher as a biscuit
/// (scopes from [`ChatReadAccess::to_scopes`]), verified by the gateway, and
/// then compiled into the session's capability manifest by
/// [`compile_chat_manifest_with_read_access`], where the same
/// `PolicyEngine` that already refuses writes enforces it. It never carries
/// a write, commit, email or network capability: there is no field for one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatReadAccess {
    /// Read files of the real project root (`fs://project/**`).
    pub project: bool,
    /// Labels of registry-listed projects readable the same way.
    pub readable_projects: Vec<String>,
    /// Bare MCP tool names (`ta_wayfinder_task_list`, ...) allowed to read
    /// project configuration from the real project root.
    pub tools: Vec<String>,
}

impl ChatReadAccess {
    /// True when nothing is granted (the pre-grant chat session).
    pub fn is_empty(&self) -> bool {
        !self.project && self.readable_projects.is_empty() && self.tools.is_empty()
    }

    /// The biscuit scopes expressing this grant. Order: project, readable
    /// projects, tools.
    pub fn to_scopes(&self) -> Vec<String> {
        let mut scopes = Vec::new();
        if self.project {
            scopes.push(CHAT_SCOPE_PROJECT_READ.to_string());
        }
        scopes.extend(
            self.readable_projects
                .iter()
                .map(|l| format!("{CHAT_SCOPE_READABLE_PREFIX}{l}")),
        );
        scopes.extend(
            self.tools
                .iter()
                .map(|t| format!("{CHAT_SCOPE_TOOL_PREFIX}{t}")),
        );
        scopes
    }

    /// Recover a grant from verified biscuit scopes. Scopes this module does
    /// not define, and labels or tool names that are not well-formed, are
    /// dropped (a grant can only ever be narrower than what was minted).
    pub fn from_scopes<S: AsRef<str>>(scopes: &[S]) -> Self {
        let mut access = Self::default();
        for scope in scopes {
            let scope = scope.as_ref();
            if scope == CHAT_SCOPE_PROJECT_READ {
                access.project = true;
            } else if let Some(label) = scope.strip_prefix(CHAT_SCOPE_READABLE_PREFIX) {
                if is_valid_readable_label(label) {
                    access.readable_projects.push(label.to_string());
                }
            } else if let Some(tool) = scope.strip_prefix(CHAT_SCOPE_TOOL_PREFIX) {
                if is_valid_tool_name(tool) {
                    access.tools.push(tool.to_string());
                }
            }
        }
        access
    }
}

/// [`compile_chat_manifest`] plus the read grants of `access`:
///
/// - `fs.read` on `fs://project/**` (the real project root);
/// - `fs.read` on `fs://readable/<label>/**` for each readable project;
/// - `tool.read` on `tool://<name>` for each named read tool.
///
/// Nothing here is a write. The scratch-write grant of the base manifest is
/// unchanged, so `fs.write_patch` on `fs://project/**` stays denied, as do
/// git, email and external network (never granted: default-deny).
pub fn compile_chat_manifest_with_read_access(
    agent_id: &str,
    workspace_resource_scope: &str,
    validity_hours: i64,
    access: &ChatReadAccess,
) -> Result<CapabilityManifest, CompilerError> {
    let mut manifest = compile_chat_manifest(agent_id, workspace_resource_scope, validity_hours)?;

    for label in &access.readable_projects {
        if !is_valid_readable_label(label) {
            return Err(CompilerError::InvalidAction {
                action: format!("fs_read on readable project '{label}'"),
                message: "a readable project label may only contain letters, digits, '.', '_' \
                          and '-'"
                    .to_string(),
            });
        }
    }
    for tool in &access.tools {
        if !is_valid_tool_name(tool) {
            return Err(CompilerError::InvalidAction {
                action: format!("tool_read on '{tool}'"),
                message: "a read tool name may only contain lowercase letters, digits and '_'"
                    .to_string(),
            });
        }
    }

    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    let mut fs_scopes = Vec::new();
    if access.project {
        fs_scopes.push(CHAT_PROJECT_RESOURCE_SCOPE.to_string());
    }
    fs_scopes.extend(
        access
            .readable_projects
            .iter()
            .map(|l| format!("{}**", chat_readable_resource_prefix(l))),
    );
    groups.push(("fs_read", fs_scopes));
    groups.push((
        "tool_read",
        access.tools.iter().map(|t| chat_tool_resource(t)).collect(),
    ));

    for (action, resource_scope) in groups {
        if resource_scope.is_empty() {
            continue;
        }
        let profile = AlignmentProfile {
            principal: "chat-session".to_string(),
            autonomy_envelope: AutonomyEnvelope {
                bounded_actions: vec![action.to_string()],
                escalation_triggers: vec![],
                forbidden_actions: vec![
                    "network_external".to_string(),
                    "credential_access".to_string(),
                ],
            },
            constitution: "default-v1".to_string(),
            coordination: CoordinationConfig::default(),
        };
        let options = CompilerOptions {
            resource_scope,
            validity_hours,
        };
        manifest
            .grants
            .extend(PolicyCompiler::compile(agent_id, &profile, &options)?.grants);
    }
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

    fn cos_access() -> ChatReadAccess {
        ChatReadAccess {
            project: true,
            readable_projects: vec!["sibling".to_string()],
            tools: vec!["ta_wayfinder_task_list".to_string()],
        }
    }

    fn decide(engine: &PolicyEngine, tool: &str, verb: &str, target: &str) -> PolicyDecision {
        engine.evaluate(&PolicyRequest {
            agent_id: "cos".to_string(),
            tool: tool.to_string(),
            verb: verb.to_string(),
            target_uri: target.to_string(),
        })
    }

    #[test]
    fn read_access_grants_project_read_and_named_tool_reads_only() {
        let manifest =
            compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &cos_access()).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);

        assert_eq!(
            decide(&engine, "fs", "read", "fs://project/src/main.rs"),
            PolicyDecision::Allow
        );
        assert_eq!(
            decide(&engine, "fs", "read", "fs://readable/sibling/README.md"),
            PolicyDecision::Allow
        );
        assert_eq!(
            decide(&engine, "tool", "read", "tool://ta_wayfinder_task_list"),
            PolicyDecision::Allow
        );
        // No grant for a tool that was not named, nor for an unlisted project.
        for (tool, verb, target) in [
            ("tool", "read", "tool://ta_wiki_search"),
            ("fs", "read", "fs://readable/other/README.md"),
            // Writes, commits, email, network: never granted.
            ("fs", "write_patch", "fs://project/src/main.rs"),
            ("fs", "write_patch", "fs://readable/sibling/x"),
            ("fs", "apply", "fs://project/src/main.rs"),
            ("git", "commit", "fs://project/**"),
            ("email", "send", "mailto:a@example.com"),
            ("network", "external", "https://example.com"),
        ] {
            assert!(
                matches!(
                    decide(&engine, tool, verb, target),
                    PolicyDecision::Deny { .. }
                ),
                "{tool}.{verb} on {target} must be denied"
            );
        }
    }

    #[test]
    fn secrets_backstop_still_wins_over_the_project_read_grant() {
        let manifest =
            compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &cos_access()).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);
        for target in [
            "fs://project/.env",
            "fs://project/deploy/server.pem",
            "fs://project/.ssh/id_ed25519",
            "fs://readable/sibling/.env",
        ] {
            assert!(
                matches!(
                    decide(&engine, "fs", "read", target),
                    PolicyDecision::Deny { .. }
                ),
                "{target} must stay unreadable"
            );
        }
    }

    #[test]
    fn without_read_access_the_manifest_has_no_project_or_tool_grants() {
        let manifest =
            compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &ChatReadAccess::default())
                .unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);
        assert!(matches!(
            decide(&engine, "fs", "read", "fs://project/src/main.rs"),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            decide(&engine, "tool", "read", "tool://ta_wayfinder_task_list"),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn scopes_round_trip_and_malformed_scopes_are_dropped() {
        let access = cos_access();
        assert_eq!(ChatReadAccess::from_scopes(&access.to_scopes()), access);

        let hostile = ChatReadAccess::from_scopes(&[
            "fs:write:project",
            "fs:read:readable:../etc",
            "fs:read:readable:a/b",
            "fs:read:readable:a*",
            "tool:read:Ta Evil",
            "email:send:all",
        ]);
        assert!(hostile.is_empty(), "{hostile:?}");
    }

    #[test]
    fn malformed_labels_and_tool_names_are_refused_at_compile_time() {
        for bad in ["", "a/b", "a*", "..", "x,y"] {
            let access = ChatReadAccess {
                project: false,
                readable_projects: vec![bad.to_string()],
                tools: vec![],
            };
            assert!(
                compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &access).is_err(),
                "label {bad:?} must be refused"
            );
        }
        let access = ChatReadAccess {
            project: false,
            readable_projects: vec![],
            tools: vec!["Bad Tool".to_string()],
        };
        assert!(compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &access).is_err());
    }
}
