// team.rs — Virtual team configuration: team.toml schema, parser, and personas path (v0.17.0.3, model_tier/handles_tags added v0.17.11.6).
//
// `.ta/team.toml` declares which agent ID fills each team role and at what security level.
// `model_tier` (optional) overrides `agent_id` via a lookup in the top-level `[model_tiers]`
// table; `handles_tags` (optional) is a local routing hint consulted only by a virtual
// team's own orchestrator persona, never by TA core.
//
// Example:
// ```toml
// [model_tiers]
// highest = "claude-opus-5"
//
// [[members]]
// role = "reviewer"
// agent_id = "claude-sonnet-4-6"
// security = "auto"
// persona = "strict-reviewer"
// model_tier = "highest"
// handles_tags = ["task-delegation"]
// ```

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent_action::{TeamMember, TeamRole};
use crate::workflow_session::AdvisorSecurity;

// ── TeamConfigError ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TeamConfigError {
    #[error("Failed to read .ta/team.toml: {0}")]
    Io(#[from] io::Error),
    #[error("Failed to parse .ta/team.toml: {0}")]
    Parse(#[from] toml::de::Error),
}

// ── TeamConfig ────────────────────────────────────────────────────────────────

/// Parsed contents of `.ta/team.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamConfig {
    #[serde(default)]
    pub members: Vec<TeamMember>,
    /// Named tier -> concrete `agent_id` (e.g. `highest = "claude-opus-5"`).
    /// Looked up by `TeamMember.model_tier`; a member's `agent_id` remains
    /// the fallback when its tier is unset or not present here
    /// (v0.17.11.6).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_tiers: HashMap<String, String>,
}

impl TeamConfig {
    /// Load `.ta/team.toml` from `workspace_root`.
    ///
    /// Returns an empty `TeamConfig` if the file does not exist.
    pub fn load(workspace_root: &Path) -> Result<Self, TeamConfigError> {
        let path = workspace_root.join(".ta").join("team.toml");
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let config: TeamConfig = toml::from_str(&content)?;
                Ok(config)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(TeamConfigError::Io(e)),
        }
    }

    /// Write the current config to `.ta/team.toml` in `workspace_root`.
    pub fn save(&self, workspace_root: &Path) -> io::Result<()> {
        let ta_dir = workspace_root.join(".ta");
        std::fs::create_dir_all(&ta_dir)?;
        let path = ta_dir.join("team.toml");
        let content = toml::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, content)
    }

    /// Find the first team member with the given role.
    pub fn find_by_role(&self, role: &TeamRole) -> Option<&TeamMember> {
        self.members.iter().find(|m| &m.role == role)
    }

    /// All team members whose `handles_tags` includes `tag` — a local
    /// routing lookup for a virtual team's own orchestrator persona
    /// (v0.17.11.6); never consulted by TA core itself.
    pub fn find_by_tag(&self, tag: &str) -> Vec<&TeamMember> {
        self.members
            .iter()
            .filter(|m| m.handles_tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Resolve `member`'s `agent_id` against this config's `model_tiers` —
    /// `member.model_tier` wins when set and present in `model_tiers`;
    /// `member.agent_id` is the fallback otherwise (v0.17.11.6).
    pub fn resolve_agent_id<'a>(&'a self, member: &'a TeamMember) -> &'a str {
        member
            .model_tier
            .as_deref()
            .and_then(|tier| self.model_tiers.get(tier))
            .map(String::as_str)
            .unwrap_or(&member.agent_id)
    }

    /// Upsert a team member for the given role.
    ///
    /// If a member with `role` already exists, updates their fields.
    /// Otherwise appends a new member (with no tier/tags — set those via
    /// direct field access). Returns `&mut Self` for chaining.
    pub fn assign(
        &mut self,
        role: TeamRole,
        agent_id: String,
        security: AdvisorSecurity,
        persona: Option<String>,
    ) -> &mut Self {
        if let Some(existing) = self.members.iter_mut().find(|m| m.role == role) {
            existing.agent_id = agent_id;
            existing.security = security;
            existing.persona = persona;
        } else {
            self.members.push(TeamMember {
                role,
                agent_id,
                security,
                persona,
                model_tier: None,
                handles_tags: Vec::new(),
            });
        }
        self
    }
}

// ── Personas governed path helper ─────────────────────────────────────────────

/// TOML snippet to add to `.ta/workflow.toml` to govern `.ta/personas/` as read-only.
///
/// Append this to `workflow.toml` when initializing team configuration so agent
/// subprocesses cannot modify persona files without human review.
pub fn default_personas_governed_path_toml() -> &'static str {
    r#"
# Advisor persona templates — governed read-only so agents cannot modify them.
[[governed_paths]]
path = ".ta/personas"
mode = "read-only"
purpose = "Advisor persona templates (read-only for agents)"
"#
}

/// Path segment for the personas directory, relative to the workspace root.
pub const PERSONAS_GOVERNED_PATH: &str = ".ta/personas";

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_member(role: TeamRole) -> TeamMember {
        TeamMember {
            role,
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::ReadOnly,
            persona: None,
            model_tier: None,
            handles_tags: Vec::new(),
        }
    }

    #[test]
    fn team_load_missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let config = TeamConfig::load(tmp.path()).unwrap();
        assert!(config.members.is_empty());
    }

    #[test]
    fn team_toml_parse_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mut config = TeamConfig::default();
        config.members.push(TeamMember {
            role: TeamRole::reviewer(),
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::Auto,
            persona: Some("strict-reviewer".to_string()),
            model_tier: None,
            handles_tags: Vec::new(),
        });
        config.save(tmp.path()).unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].role, TeamRole::reviewer());
        assert_eq!(loaded.members[0].agent_id, "claude-sonnet-4-6");
        assert_eq!(loaded.members[0].security, AdvisorSecurity::Auto);
        assert_eq!(
            loaded.members[0].persona,
            Some("strict-reviewer".to_string())
        );
    }

    #[test]
    fn team_toml_multiple_members_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mut config = TeamConfig::default();
        config.members.push(TeamMember {
            role: TeamRole::implementer(),
            agent_id: "claude-opus-4-8".to_string(),
            security: AdvisorSecurity::ReadOnly,
            persona: None,
            model_tier: None,
            handles_tags: Vec::new(),
        });
        config.members.push(TeamMember {
            role: TeamRole::reviewer(),
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::Auto,
            persona: Some("strict".to_string()),
            model_tier: None,
            handles_tags: Vec::new(),
        });
        config.save(tmp.path()).unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 2);
    }

    #[test]
    fn team_toml_custom_role_round_trip() {
        // Data-defined roles (TA-CONSTITUTION.md §1.6): a custom role name not
        // among the well-known constants must round-trip through team.toml
        // identically, anticipating §8's community-review workflow.
        let tmp = TempDir::new().unwrap();
        let mut config = TeamConfig::default();
        config.members.push(TeamMember {
            role: TeamRole::new("security-team"),
            agent_id: "claude-opus-4-8".to_string(),
            security: AdvisorSecurity::ReadOnly,
            persona: None,
            model_tier: None,
            handles_tags: Vec::new(),
        });
        config.save(tmp.path()).unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].role, TeamRole::new("security-team"));
        assert_eq!(loaded.members[0].role.as_str(), "security-team");
    }

    #[test]
    fn team_toml_well_known_roles_parse_as_plain_strings() {
        // Regression guard: existing team.toml fixtures using well-known role
        // names as plain strings (e.g. `role = "reviewer"`) must keep parsing
        // identically after TeamRole became a data-defined newtype.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta")).unwrap();
        std::fs::write(
            tmp.path().join(".ta").join("team.toml"),
            r#"
[[members]]
role = "reviewer"
agent_id = "claude-sonnet-4-6"
security = "auto"
persona = "strict-reviewer"
"#,
        )
        .unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].role, TeamRole::reviewer());
        assert_eq!(loaded.members[0].role.as_str(), "reviewer");
    }

    #[test]
    fn team_assign_new_role() {
        let mut config = TeamConfig::default();
        config.assign(
            TeamRole::implementer(),
            "claude-opus".to_string(),
            AdvisorSecurity::ReadOnly,
            None,
        );
        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].agent_id, "claude-opus");
    }

    #[test]
    fn team_assign_upserts_existing_role() {
        let mut config = TeamConfig::default();
        config.assign(
            TeamRole::implementer(),
            "claude-opus".to_string(),
            AdvisorSecurity::ReadOnly,
            None,
        );
        // Assign again — should update, not duplicate.
        config.assign(
            TeamRole::implementer(),
            "claude-sonnet".to_string(),
            AdvisorSecurity::Auto,
            Some("fast".to_string()),
        );
        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].agent_id, "claude-sonnet");
        assert_eq!(config.members[0].security, AdvisorSecurity::Auto);
        assert_eq!(config.members[0].persona, Some("fast".to_string()));
    }

    #[test]
    fn team_find_by_role() {
        let mut config = TeamConfig::default();
        config.members.push(make_member(TeamRole::reviewer()));
        config.members.push(make_member(TeamRole::qa()));

        assert!(config.find_by_role(&TeamRole::reviewer()).is_some());
        assert!(config.find_by_role(&TeamRole::qa()).is_some());
        assert!(config.find_by_role(&TeamRole::architect()).is_none());
    }

    #[test]
    fn team_toml_model_tier_and_handles_tags_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mut config = TeamConfig::default();
        config
            .model_tiers
            .insert("highest".to_string(), "claude-opus-5".to_string());
        config.members.push(TeamMember {
            role: TeamRole::new("chief-of-staff"),
            agent_id: "claude-sonnet-5".to_string(),
            security: AdvisorSecurity::Auto,
            persona: None,
            model_tier: Some("highest".to_string()),
            handles_tags: vec![
                "task-delegation".to_string(),
                "research-request".to_string(),
            ],
        });
        config.save(tmp.path()).unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].model_tier, Some("highest".to_string()));
        assert_eq!(
            loaded.members[0].handles_tags,
            vec![
                "task-delegation".to_string(),
                "research-request".to_string()
            ]
        );
        assert_eq!(
            loaded.model_tiers.get("highest"),
            Some(&"claude-opus-5".to_string())
        );
    }

    #[test]
    fn team_toml_missing_model_tier_and_handles_tags_default_empty() {
        // Backward compatibility: existing team.toml files with neither
        // field must keep parsing identically (matches the well-known-roles
        // regression guard above).
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta")).unwrap();
        std::fs::write(
            tmp.path().join(".ta").join("team.toml"),
            r#"
[[members]]
role = "reviewer"
agent_id = "claude-sonnet-4-6"
security = "auto"
"#,
        )
        .unwrap();

        let loaded = TeamConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].model_tier, None);
        assert!(loaded.members[0].handles_tags.is_empty());
        assert!(loaded.model_tiers.is_empty());
    }

    #[test]
    fn resolve_agent_id_uses_tier_when_set_and_present() {
        let mut config = TeamConfig::default();
        config
            .model_tiers
            .insert("highest".to_string(), "claude-opus-5".to_string());
        let mut member = make_member(TeamRole::reviewer());
        member.model_tier = Some("highest".to_string());

        assert_eq!(config.resolve_agent_id(&member), "claude-opus-5");
    }

    #[test]
    fn resolve_agent_id_falls_back_to_agent_id_when_tier_unset() {
        let config = TeamConfig::default();
        let member = make_member(TeamRole::reviewer());

        assert_eq!(config.resolve_agent_id(&member), "claude-sonnet-4-6");
    }

    #[test]
    fn resolve_agent_id_falls_back_to_agent_id_when_tier_not_found() {
        let config = TeamConfig::default();
        let mut member = make_member(TeamRole::reviewer());
        member.model_tier = Some("nonexistent-tier".to_string());

        // The tier is set but not declared in `model_tiers` — fall back
        // rather than error, since a stale/typo'd tier shouldn't break
        // launching the role entirely.
        assert_eq!(config.resolve_agent_id(&member), "claude-sonnet-4-6");
    }

    #[test]
    fn find_by_tag_returns_matching_members_only() {
        let mut config = TeamConfig::default();
        let mut chief = make_member(TeamRole::new("chief-of-staff"));
        chief.handles_tags = vec!["task-delegation".to_string()];
        let mut worker = make_member(TeamRole::implementer());
        worker.handles_tags = vec!["docs".to_string()];
        config.members.push(chief);
        config.members.push(worker);

        let matches = config.find_by_tag("task-delegation");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].role, TeamRole::new("chief-of-staff"));
        assert!(config.find_by_tag("nonexistent-tag").is_empty());
    }

    #[test]
    fn assign_new_member_defaults_tier_and_tags_empty() {
        let mut config = TeamConfig::default();
        config.assign(
            TeamRole::implementer(),
            "claude-opus".to_string(),
            AdvisorSecurity::ReadOnly,
            None,
        );
        assert_eq!(config.members[0].model_tier, None);
        assert!(config.members[0].handles_tags.is_empty());
    }

    #[test]
    fn personas_governed_path_toml_contains_path() {
        let toml = default_personas_governed_path_toml();
        assert!(toml.contains(".ta/personas"));
        assert!(toml.contains("read-only"));
        assert!(toml.contains("governed_paths"));
    }
}
