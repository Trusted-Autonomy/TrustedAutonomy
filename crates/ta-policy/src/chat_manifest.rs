// chat_manifest.rs — Chat-scoped capability manifest construction.
//
// Builds the narrow "chat mode" CapabilityManifest described in
// wayfinder/docs/superpowers/specs/2026-10-03-multi-project-vt-agnostic-coordination-design.md
// §8: read-only. A chat session has NO write grant of any kind: no scratch
// directory, no project write, no git/email/Drive/social tool. The session's
// own session directory is readable (`fs://workspace/**`); everything outside
// it that the Chief of Staff may read is decided by the read allow/deny
// policy ([`crate::read_policy::ReadPolicy`]) that the launcher mints into the
// biscuit read grant ([`ChatReadAccess`]).
//
// Whiteboard delivery: NOT separately gated here, by design, not
// oversight. Confirmed by source inspection
// (`crates/ta-mcp-gateway/src/tools/whiteboard.rs`,
// `crates/ta-daemon/src/api/whiteboard.rs`) that whiteboard read/write
// operations have zero references to `PolicyEngine`/`CapabilityManifest`/
// `ta_policy` anywhere. They carry messages between agents, not file or
// project content.
//
// No grants for git/email/Drive/social tools: this falls out of
// default-deny with zero extra code (`PolicyEngine::evaluate` denies any
// `(tool, verb)` pair with no matching grant).

use std::path::{Path, PathBuf};

use crate::alignment::{AlignmentProfile, AutonomyEnvelope, CoordinationConfig};
use crate::capability::CapabilityManifest;
use crate::compiler::{CompilerError, CompilerOptions, PolicyCompiler};
use crate::read_policy::{
    is_valid_label, scope_decode, scope_encode, ReadPolicy, ReadRule, READ_RESOURCE_SCOPE,
};

/// The chat-mode read `AlignmentProfile`: broad `fs_read`, no write of any kind.
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

/// Compile the chat-mode `CapabilityManifest`: `fs_read` across
/// `workspace_resource_scope` (the session directory) and nothing else. There is
/// no write grant; git, email, Drive and social tools are simply never granted,
/// so default-deny handles them.
pub fn compile_chat_manifest(
    agent_id: &str,
    workspace_resource_scope: &str,
    validity_hours: i64,
) -> Result<CapabilityManifest, CompilerError> {
    let read_options = CompilerOptions {
        resource_scope: vec![workspace_resource_scope.to_string()],
        validity_hours,
    };
    PolicyCompiler::compile(agent_id, &chat_read_profile(), &read_options)
}

/// Biscuit scope prefix carrying the canonical project root (readable always).
pub const CHAT_SCOPE_PROJECT_READ_PREFIX: &str = "fs:read:project:";
/// Biscuit scope prefix of one allow rule (`<kind>:<registered>:<label>:<pattern>:<source>`).
pub const CHAT_SCOPE_ALLOW_PREFIX: &str = "fs:read:allow:";
/// Biscuit scope prefix of one deny rule.
pub const CHAT_SCOPE_DENY_PREFIX: &str = "fs:read:deny:";
const CHAT_SCOPE_TOOL_PREFIX: &str = "tool:read:";

/// Resource URI a read-only chat tool (a Wayfinder or wiki read, plan
/// status, community lookup) is checked against.
pub fn chat_tool_resource(tool: &str) -> String {
    format!("tool://{tool}")
}

fn is_valid_tool_name(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What a read-only chat session (the Chief of Staff) may read beyond its own
/// session directory: the compiled read allow/deny policy and the named read
/// tools that load project configuration (Wayfinder reads, wiki, plan status,
/// community lookup).
///
/// This is a grant, not a mode: it is minted by the launcher as a biscuit
/// (scopes from [`ChatReadAccess::to_scopes`]), verified by the gateway, and the
/// gateway rebuilds its read policy from the verified scopes ONLY, never from live
/// config, so editing `.ta/workflow.toml` mid-session cannot widen (or narrow) a
/// running session. It never carries a write, commit, email or network capability:
/// there is no field for one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatReadAccess {
    /// Canonical root of the real project (always readable). `None` grants no file
    /// reads at all.
    pub project_root: Option<PathBuf>,
    /// Resolved allow rules beyond the project root.
    pub allow: Vec<ReadRule>,
    /// Resolved deny rules (they beat every allow).
    pub deny: Vec<ReadRule>,
    /// Bare MCP tool names (`ta_wayfinder_task_list`, ...) allowed to read
    /// project configuration from the real project root.
    pub tools: Vec<String>,
}

impl ChatReadAccess {
    /// Compile `policy` and `tools` into a grant.
    pub fn from_policy(policy: &ReadPolicy, tools: Vec<String>) -> Self {
        Self {
            project_root: Some(policy.project_root.clone()),
            allow: policy.allow.clone(),
            deny: policy.deny.clone(),
            tools,
        }
    }

    /// True when nothing is granted (the pre-grant chat session).
    pub fn is_empty(&self) -> bool {
        self.project_root.is_none()
            && self.allow.is_empty()
            && self.deny.is_empty()
            && self.tools.is_empty()
    }

    /// The biscuit scopes expressing this grant. Order: project root, allow
    /// rules, deny rules, tools. Free-text fields are percent-encoded because the
    /// broker joins scopes with `,`.
    pub fn to_scopes(&self) -> Vec<String> {
        let mut scopes = Vec::new();
        if let Some(root) = &self.project_root {
            scopes.push(format!(
                "{CHAT_SCOPE_PROJECT_READ_PREFIX}{}",
                scope_encode(&root.to_string_lossy())
            ));
        }
        scopes.extend(
            self.allow
                .iter()
                .map(|r| format!("{CHAT_SCOPE_ALLOW_PREFIX}{}", r.to_scope_fields())),
        );
        scopes.extend(
            self.deny
                .iter()
                .map(|r| format!("{CHAT_SCOPE_DENY_PREFIX}{}", r.to_scope_fields())),
        );
        scopes.extend(
            self.tools
                .iter()
                .map(|t| format!("{CHAT_SCOPE_TOOL_PREFIX}{t}")),
        );
        scopes
    }

    /// Recover a grant from verified biscuit scopes. Scopes outside this module's
    /// namespaces are ignored. A scope INSIDE its namespaces that is malformed is an
    /// error: silently dropping a deny rule would widen access, so the grant is
    /// refused as a whole instead.
    pub fn from_scopes<S: AsRef<str>>(scopes: &[S]) -> Result<Self, String> {
        let mut access = Self::default();
        for scope in scopes {
            let scope = scope.as_ref();
            if let Some(root) = scope.strip_prefix(CHAT_SCOPE_PROJECT_READ_PREFIX) {
                let root = scope_decode(root)
                    .filter(|r| Path::new(r).is_absolute())
                    .ok_or_else(|| format!("malformed project-root read scope '{scope}'"))?;
                access.project_root = Some(PathBuf::from(root));
            } else if let Some(fields) = scope.strip_prefix(CHAT_SCOPE_ALLOW_PREFIX) {
                access.allow.push(
                    ReadRule::from_scope_fields(fields)
                        .ok_or_else(|| format!("malformed allow scope '{scope}'"))?,
                );
            } else if let Some(fields) = scope.strip_prefix(CHAT_SCOPE_DENY_PREFIX) {
                access.deny.push(
                    ReadRule::from_scope_fields(fields)
                        .ok_or_else(|| format!("malformed deny scope '{scope}'"))?,
                );
            } else if let Some(tool) = scope.strip_prefix(CHAT_SCOPE_TOOL_PREFIX) {
                if is_valid_tool_name(tool) {
                    access.tools.push(tool.to_string());
                }
            }
        }
        Ok(access)
    }

    /// The read policy this grant encodes; `None` when it grants no file reads.
    /// `home` and `state_dir` only feed the built-in hard deny list.
    pub fn read_policy(&self, home: Option<&Path>, state_dir: Option<&Path>) -> Option<ReadPolicy> {
        let root = self.project_root.as_ref()?;
        Some(ReadPolicy::new(
            root,
            self.allow.clone(),
            self.deny.clone(),
            home,
            state_dir,
        ))
    }
}

/// [`compile_chat_manifest`] plus the read grants of `access`:
///
/// - `fs.read` on `fs://read/**` when the grant carries a project root. WHICH
///   paths are readable is decided by [`ReadPolicy`], rebuilt from the same token;
///   the engine's secrets backstop still applies on top;
/// - `tool.read` on `tool://<name>` for each named read tool.
///
/// Nothing here is a write: `fs.write_patch` on any resource stays denied, as do
/// git, email and external network (never granted: default-deny).
pub fn compile_chat_manifest_with_read_access(
    agent_id: &str,
    workspace_resource_scope: &str,
    validity_hours: i64,
    access: &ChatReadAccess,
) -> Result<CapabilityManifest, CompilerError> {
    let mut manifest = compile_chat_manifest(agent_id, workspace_resource_scope, validity_hours)?;

    for rule in access.allow.iter().chain(&access.deny) {
        if let Some(label) = &rule.label {
            if !is_valid_label(label) {
                return Err(CompilerError::InvalidAction {
                    action: format!("fs_read on readable project '{label}'"),
                    message: "a readable project label may only contain letters, digits, '.', \
                              '_' and '-'"
                        .to_string(),
                });
            }
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
    if access.project_root.is_some() {
        groups.push(("fs_read", vec![READ_RESOURCE_SCOPE.to_string()]));
    }
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
    use crate::read_policy::RuleKind;

    const WORKSPACE: &str = "fs://workspace/**";

    fn engine_for(access: &ChatReadAccess) -> PolicyEngine {
        let manifest = compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, access).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);
        engine
    }

    fn decide(engine: &PolicyEngine, tool: &str, verb: &str, target: &str) -> PolicyDecision {
        engine.evaluate(&PolicyRequest {
            agent_id: "cos".to_string(),
            tool: tool.to_string(),
            verb: verb.to_string(),
            target_uri: target.to_string(),
        })
    }

    fn rule(pattern: &str, source: &str, label: Option<&str>) -> ReadRule {
        ReadRule {
            kind: RuleKind::Path,
            pattern: pattern.to_string(),
            source: source.to_string(),
            label: label.map(str::to_string),
            registered: false,
        }
    }

    fn cos_access() -> ChatReadAccess {
        ChatReadAccess {
            project_root: Some(PathBuf::from("/work/proj")),
            allow: vec![rule("/work/notes", "~/notes", Some("notes"))],
            deny: vec![rule("/work/notes/private", "~/notes/private", None)],
            tools: vec!["ta_wayfinder_task_list".to_string()],
        }
    }

    #[test]
    fn manifest_has_session_read_and_no_write_grant_at_all() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let read_grant = manifest
            .grants
            .iter()
            .find(|g| g.tool == "fs" && g.verb == "read")
            .expect("expected an fs read grant");
        assert_eq!(read_grant.resource_pattern, "fs://workspace/**");
        assert!(
            manifest.grants.iter().all(|g| g.verb == "read"),
            "a chat manifest grants reads only: {:?}",
            manifest.grants
        );
    }

    #[test]
    fn policy_engine_denies_every_write_verb_everywhere() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);
        for target in [
            "fs://workspace/src/main.rs",
            "fs://workspace/.ta/chat-scratch/notes.md",
            "fs://project/src/main.rs",
        ] {
            for verb in ["write_patch", "write", "apply", "delete"] {
                let decision = engine.evaluate(&PolicyRequest {
                    agent_id: "chat-agent".to_string(),
                    tool: "fs".to_string(),
                    verb: verb.to_string(),
                    target_uri: target.to_string(),
                });
                assert!(
                    matches!(decision, PolicyDecision::Deny { .. }),
                    "{verb} on {target} must be denied, got {decision:?}"
                );
            }
        }
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
    fn policy_engine_denies_git_push_and_email_send_with_no_grant() {
        let manifest = compile_chat_manifest("chat-agent", WORKSPACE, 1).unwrap();
        let mut engine = PolicyEngine::new();
        engine.load_manifest(manifest);
        for (tool, verb, target) in [
            ("git", "push", "fs://workspace/**"),
            ("email", "send", "mailto:someone@example.com"),
        ] {
            let decision = engine.evaluate(&PolicyRequest {
                agent_id: "chat-agent".to_string(),
                tool: tool.to_string(),
                verb: verb.to_string(),
                target_uri: target.to_string(),
            });
            assert!(
                matches!(decision, PolicyDecision::Deny { .. }),
                "expected Deny for {tool}.{verb}, got {decision:?}"
            );
        }
    }

    #[test]
    fn read_access_grants_reads_and_named_tool_reads_only() {
        let engine = engine_for(&cos_access());
        assert_eq!(
            decide(&engine, "fs", "read", "fs://read/work/proj/src/main.rs"),
            PolicyDecision::Allow
        );
        assert_eq!(
            decide(&engine, "tool", "read", "tool://ta_wayfinder_task_list"),
            PolicyDecision::Allow
        );
        for (tool, verb, target) in [
            ("tool", "read", "tool://ta_wiki_search"),
            // Writes, commits, email, network: never granted.
            ("fs", "write_patch", "fs://read/work/proj/src/main.rs"),
            ("fs", "write_patch", "fs://project/src/main.rs"),
            ("fs", "apply", "fs://read/work/proj/src/main.rs"),
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
    fn secrets_backstop_still_wins_over_the_read_grant() {
        let engine = engine_for(&cos_access());
        for target in [
            "fs://read/work/proj/.env",
            "fs://read/work/notes/deploy/server.pem",
            "fs://read/work/proj/.ssh/id_ed25519",
            "fs://read/Users/me/.aws/credentials",
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
    fn without_read_access_the_manifest_has_no_file_or_tool_grants() {
        let engine = engine_for(&ChatReadAccess::default());
        assert!(matches!(
            decide(&engine, "fs", "read", "fs://read/work/proj/src/main.rs"),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            decide(&engine, "tool", "read", "tool://ta_wayfinder_task_list"),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn scopes_round_trip_and_are_comma_free() {
        let access = cos_access();
        let scopes = access.to_scopes();
        assert!(scopes.iter().all(|s| !s.contains(',')), "{scopes:?}");
        assert_eq!(ChatReadAccess::from_scopes(&scopes).unwrap(), access);
        // A token without a rule yields a policy without it.
        let narrow: Vec<&String> = scopes
            .iter()
            .filter(|s| !s.starts_with(CHAT_SCOPE_ALLOW_PREFIX))
            .collect();
        let narrowed = ChatReadAccess::from_scopes(&narrow).unwrap();
        assert!(narrowed.allow.is_empty());
        assert_eq!(narrowed.deny.len(), 1);
    }

    #[test]
    fn foreign_scopes_are_ignored_but_malformed_read_scopes_refuse_the_grant() {
        let ok = ChatReadAccess::from_scopes(&[
            "fs:write:project",
            "tool:read:Ta Evil",
            "email:send:all",
        ])
        .unwrap();
        assert!(ok.is_empty(), "{ok:?}");
        for bad in [
            "fs:read:project:relative/path",
            "fs:read:allow:p:1:x",
            "fs:read:deny:z:0:::",
            "fs:read:deny:p:0::%zz:x",
            "fs:read:allow:p:0:a*b:/x:x",
        ] {
            assert!(
                ChatReadAccess::from_scopes(&[bad]).is_err(),
                "{bad} must refuse the grant"
            );
        }
    }

    #[test]
    fn read_policy_is_rebuilt_from_the_token_alone() {
        let access = cos_access();
        let rebuilt = ChatReadAccess::from_scopes(&access.to_scopes()).unwrap();
        let policy = rebuilt.read_policy(None, None).unwrap();
        assert_eq!(policy.allow.len(), 1);
        assert_eq!(policy.deny.len(), 1);
        assert!(ChatReadAccess::default().read_policy(None, None).is_none());
    }

    #[test]
    fn malformed_labels_and_tool_names_are_refused_at_compile_time() {
        for bad in ["a/b", "a*", "..", "x,y"] {
            let access = ChatReadAccess {
                project_root: Some(PathBuf::from("/work/proj")),
                allow: vec![rule("/work/x", "x", Some(bad))],
                ..Default::default()
            };
            assert!(
                compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &access).is_err(),
                "label {bad:?} must be refused"
            );
        }
        let access = ChatReadAccess {
            tools: vec!["Bad Tool".to_string()],
            ..Default::default()
        };
        assert!(compile_chat_manifest_with_read_access("cos", WORKSPACE, 1, &access).is_err());
    }
}
