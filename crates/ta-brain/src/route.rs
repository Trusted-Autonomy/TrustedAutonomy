//! `route()` — the single entry point every goal-creation path calls to turn
//! a [`RoutingInput`] into a [`RoutingDecision`]: workload classification,
//! then team/persona/agent/security_tier/priority resolution, most-specific
//! tier first, extending the `agent`-only tiers built in v0.17.0.12.13
//! (`apps/ta-cli/src/commands/run.rs::resolve_effective_agent_full`) to the
//! other four fields the same way.
//!
//! **Config surface** (all additive to what v0.17.0.12.13 already reads):
//! ```toml
//! # .ta/workflow.toml
//! [team]
//! default = "implementer"          # workflow-level team-role default
//!
//! [security]
//! default = "suggest"              # workflow-level security-tier default
//!
//! [priority]
//! default = "normal"               # workflow-level priority default
//!
//! [workload_types.bugfix]          # per-workload-type bindings (new table,
//! team = "implementer"             # sibling to the existing [workload_agents]
//! persona = "careful-reviewer"     # table used for the agent tier)
//! security = "suggest"
//! priority = "high"
//! ```
//!
//! `[workload_agents]` (agent-only, v0.17.0.12.13) is untouched — a
//! `[workload_types.<type>].agent` key is not read, to avoid two competing
//! sources of truth for the same field.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;

use ta_goal::PersonaConfig;
use ta_session::agent_action::TeamRole;
use ta_session::team::TeamConfig;
use ta_session::workflow_session::AdvisorSecurity;

use crate::classify::{self, WorkloadClassification};
use crate::decision::RoutingDecision;
use crate::input::{ExplicitGoalRequest, RoutingInput, TriggerRoutingInput};
use crate::priority::Priority;

/// Below this workload-classification confidence, a resolved
/// `security_tier = "auto"` is downgraded to `Suggest` — an inferred
/// workload type isn't trustworthy enough on its own to hand full autonomy
/// to the advisor (§3: "gated by workload classification, not just role").
pub const AUTO_SECURITY_CONFIDENCE_THRESHOLD: f32 = 0.65;

// ── Normalized context ──────────────────────────────────────────────────

struct Context<'a> {
    classification_text: String,
    cli_agent: Option<&'a str>,
    cli_persona: Option<&'a str>,
    cli_team: Option<&'a str>,
    cli_security: Option<&'a str>,
    cli_priority: Option<&'a str>,
    workflow_name_or_path: Option<&'a str>,
    workload_override: Option<&'a str>,
}

impl<'a> From<&'a ExplicitGoalRequest> for Context<'a> {
    fn from(req: &'a ExplicitGoalRequest) -> Self {
        Self {
            classification_text: req.classification_text(),
            cli_agent: req.cli_agent.as_deref(),
            cli_persona: req.cli_persona.as_deref(),
            cli_team: req.cli_team.as_deref(),
            cli_security: req.cli_security.as_deref(),
            cli_priority: req.cli_priority.as_deref(),
            workflow_name_or_path: req.workflow_name_or_path.as_deref(),
            workload_override: req.workload_type_override.as_deref(),
        }
    }
}

impl<'a> From<&'a TriggerRoutingInput> for Context<'a> {
    fn from(trig: &'a TriggerRoutingInput) -> Self {
        Self {
            classification_text: trig.classification_text(),
            cli_agent: None,
            cli_persona: trig.persona_hint.as_deref(),
            cli_team: trig.team_hint.as_deref(),
            cli_security: trig.security_hint.as_deref(),
            cli_priority: trig.priority_hint.as_deref(),
            workflow_name_or_path: None,
            workload_override: trig.workload_hint.as_deref(),
        }
    }
}

impl<'a> Context<'a> {
    fn from_input(input: &'a RoutingInput) -> Self {
        match input {
            RoutingInput::ExplicitGoal(req) => Context::from(req),
            RoutingInput::Trigger(trig) => Context::from(trig),
        }
    }
}

// ── .ta/workflow.toml shape ─────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WorkflowToml {
    agent: DefaultSection,
    team: DefaultSection,
    security: DefaultSection,
    priority: DefaultSection,
    workload_agents: HashMap<String, String>,
    workload_types: HashMap<String, WorkloadTypeBinding>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DefaultSection {
    default: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WorkloadTypeBinding {
    team: Option<String>,
    persona: Option<String>,
    security: Option<String>,
    priority: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WorkflowYamlAgent {
    agent_framework: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DaemonToml {
    agent: DaemonAgentSection,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct DaemonAgentSection {
    default: String,
    default_framework: String,
    trusted_binaries: HashMap<String, String>,
}

impl Default for DaemonAgentSection {
    fn default() -> Self {
        Self {
            default: String::new(),
            default_framework: "claude-code".to_string(),
            trusted_binaries: HashMap::new(),
        }
    }
}

fn load_workflow_toml(workspace_root: &Path) -> WorkflowToml {
    std::fs::read_to_string(workspace_root.join(".ta").join("workflow.toml"))
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

fn load_daemon_toml(workspace_root: &Path) -> DaemonToml {
    std::fs::read_to_string(workspace_root.join(".ta").join("daemon.toml"))
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

// ── route() ──────────────────────────────────────────────────────────────

/// Route a request to a full [`RoutingDecision`]. Deterministic given
/// `input` and the on-disk config under `workspace_root` — no network
/// calls, no goal creation, no logging side effects (callers own
/// persisting/printing `RoutingDecision.rationale`, same as the existing
/// `agent = "auto"` recommendation logging in `apps/ta-cli`).
pub fn route(input: &RoutingInput, workspace_root: &Path) -> RoutingDecision {
    let ctx = Context::from_input(input);
    let mut rationale = Vec::new();

    let workflow_toml = load_workflow_toml(workspace_root);

    let workload = resolve_workload(&ctx, &mut rationale);
    let team = resolve_team(&ctx, &workload, &workflow_toml, &mut rationale);
    let persona = resolve_persona(
        &ctx,
        &workload,
        &team,
        &workflow_toml,
        workspace_root,
        &mut rationale,
    );
    let agent = resolve_agent(
        &ctx,
        &persona,
        &workload,
        &workflow_toml,
        workspace_root,
        &mut rationale,
    );
    let security_tier = resolve_security(
        &ctx,
        &team,
        &workload,
        &workflow_toml,
        workspace_root,
        &mut rationale,
    );
    let priority = resolve_priority(&ctx, &workload, &workflow_toml, &mut rationale);

    RoutingDecision {
        team,
        persona,
        agent,
        security_tier,
        priority,
        workload_type: workload.workload_type,
        workload_confidence: workload.confidence,
        rationale,
    }
}

// ── Workload classification ─────────────────────────────────────────────

fn resolve_workload(ctx: &Context, rationale: &mut Vec<String>) -> WorkloadClassification {
    let result = match ctx.workload_override {
        Some(w) if !w.is_empty() => classify::explicit(w),
        _ => classify::classify_workload(&ctx.classification_text),
    };
    rationale.push(format!(
        "workload: type={} confidence={:.2}{}",
        result.workload_type,
        result.confidence,
        if ctx.workload_override.is_some() {
            " (explicit)"
        } else {
            " (classified)"
        }
    ));
    result
}

// ── Team-role tier ───────────────────────────────────────────────────────

/// Built-in workload-type → team-role heuristic, used only when no
/// explicit/config-driven binding exists at any higher tier.
fn heuristic_team_for_workload(workload_type: &str) -> TeamRole {
    match workload_type {
        "security" => TeamRole::reviewer(),
        "release" => TeamRole::release_manager(),
        "test" => TeamRole::qa(),
        _ => TeamRole::implementer(),
    }
}

fn resolve_team(
    ctx: &Context,
    workload: &WorkloadClassification,
    workflow_toml: &WorkflowToml,
    rationale: &mut Vec<String>,
) -> TeamRole {
    if let Some(t) = ctx.cli_team {
        if !t.is_empty() {
            rationale.push(format!("team: tier=explicit value={t}"));
            return TeamRole::new(t);
        }
    }
    if let Some(binding) = workflow_toml.workload_types.get(&workload.workload_type) {
        if let Some(t) = &binding.team {
            if !t.is_empty() {
                rationale.push(format!(
                    "team: tier=workload-type({}) value={t}",
                    workload.workload_type
                ));
                return TeamRole::new(t);
            }
        }
    }
    if !workflow_toml.team.default.is_empty() {
        rationale.push(format!(
            "team: tier=workflow.toml-default value={}",
            workflow_toml.team.default
        ));
        return TeamRole::new(&workflow_toml.team.default);
    }
    let heuristic = heuristic_team_for_workload(&workload.workload_type);
    rationale.push(format!(
        "team: tier=heuristic(workload={}) value={heuristic}",
        workload.workload_type
    ));
    heuristic
}

// ── Persona tier ─────────────────────────────────────────────────────────

fn resolve_persona(
    ctx: &Context,
    workload: &WorkloadClassification,
    team: &TeamRole,
    workflow_toml: &WorkflowToml,
    workspace_root: &Path,
    rationale: &mut Vec<String>,
) -> Option<String> {
    if let Some(p) = ctx.cli_persona {
        if !p.is_empty() {
            rationale.push(format!("persona: tier=explicit value={p}"));
            return Some(p.to_string());
        }
    }
    if let Some(binding) = workflow_toml.workload_types.get(&workload.workload_type) {
        if let Some(p) = &binding.persona {
            if !p.is_empty() {
                rationale.push(format!(
                    "persona: tier=workload-type({}) value={p}",
                    workload.workload_type
                ));
                return Some(p.clone());
            }
        }
    }
    let team_config = TeamConfig::load(workspace_root).unwrap_or_default();
    if let Some(member) = team_config.find_by_role(team) {
        if let Some(p) = &member.persona {
            if !p.is_empty() {
                rationale.push(format!("persona: tier=team.toml(role={team}) value={p}"));
                return Some(p.clone());
            }
        }
    }
    rationale.push("persona: tier=none value=<unset>".to_string());
    None
}

// ── Agent tier (extends v0.17.0.12.13's Switch resolution) ──────────────

fn resolve_agent(
    ctx: &Context,
    persona: &Option<String>,
    workload: &WorkloadClassification,
    workflow_toml: &WorkflowToml,
    workspace_root: &Path,
    rationale: &mut Vec<String>,
) -> String {
    // Tier 1: explicit --agent.
    if let Some(a) = ctx.cli_agent {
        if !a.is_empty() {
            rationale.push(format!("agent: tier=explicit value={a}"));
            return resolve_or_recommend(a, "explicit", workspace_root, rationale);
        }
    }

    // Tier 2: persona-level agent binding.
    if let Some(name) = persona {
        if let Ok(cfg) = PersonaConfig::load(workspace_root, name) {
            if let Some(a) = cfg.persona.agent {
                if !a.is_empty() {
                    rationale.push(format!("agent: tier=persona({name}) value={a}"));
                    return resolve_or_recommend(&a, "persona", workspace_root, rationale);
                }
            }
        }
    }

    // Tier 3a: workflow YAML agent_framework.
    if let Some(wf) = ctx.workflow_name_or_path {
        let wf_path = Path::new(wf);
        let is_yaml_path = wf.ends_with(".yaml")
            || wf.ends_with(".yml")
            || wf_path.is_absolute()
            || wf_path.exists();
        if is_yaml_path {
            if let Ok(content) = std::fs::read_to_string(wf_path) {
                if let Ok(parsed) = serde_yaml::from_str::<WorkflowYamlAgent>(&content) {
                    if let Some(a) = parsed.agent_framework {
                        if !a.is_empty() {
                            rationale.push(format!("agent: tier=workflow-yaml value={a}"));
                            return resolve_or_recommend(
                                &a,
                                "workflow-yaml",
                                workspace_root,
                                rationale,
                            );
                        }
                    }
                }
            }
        }
    }

    // Tier 3b: [agent].default in workflow.toml.
    if !workflow_toml.agent.default.is_empty() {
        rationale.push(format!(
            "agent: tier=workflow.toml-default value={}",
            workflow_toml.agent.default
        ));
        return resolve_or_recommend(
            &workflow_toml.agent.default,
            "workflow.toml",
            workspace_root,
            rationale,
        );
    }

    // Tier 4: [workload_agents].<type>.
    if let Some(a) = workflow_toml.workload_agents.get(&workload.workload_type) {
        if !a.is_empty() {
            rationale.push(format!(
                "agent: tier=workload-type({}) value={a}",
                workload.workload_type
            ));
            return resolve_or_recommend(a, "workload-type", workspace_root, rationale);
        }
    }

    // Tier 5: daemon.toml default / legacy default_framework.
    let daemon = load_daemon_toml(workspace_root);
    if !daemon.agent.default.is_empty() {
        rationale.push(format!(
            "agent: tier=daemon.toml-default value={}",
            daemon.agent.default
        ));
        return resolve_or_recommend(
            &daemon.agent.default,
            "daemon.toml-default",
            workspace_root,
            rationale,
        );
    }
    if !daemon.agent.default_framework.is_empty() {
        rationale.push(format!(
            "agent: tier=daemon.toml-default_framework value={}",
            daemon.agent.default_framework
        ));
        return resolve_or_recommend(
            &daemon.agent.default_framework,
            "daemon.toml-default_framework",
            workspace_root,
            rationale,
        );
    }

    // Tier 6: built-in fallback.
    rationale.push("agent: tier=fallback value=claude-code".to_string());
    "claude-code".to_string()
}

fn resolve_or_recommend(
    candidate: &str,
    tier: &str,
    workspace_root: &Path,
    rationale: &mut Vec<String>,
) -> String {
    if candidate.trim().eq_ignore_ascii_case("auto") {
        let daemon = load_daemon_toml(workspace_root);
        let (agent, why) = if !daemon.agent.trusted_binaries.is_empty() {
            let mut names: Vec<&String> = daemon.agent.trusted_binaries.keys().collect();
            names.sort();
            (
                names[0].clone(),
                "alphabetically-first entry in daemon.toml's [agent].trusted_binaries".to_string(),
            )
        } else if !daemon.agent.default_framework.is_empty()
            && daemon.agent.default_framework != "claude-code"
        {
            (
                daemon.agent.default_framework.clone(),
                "daemon.toml's legacy [agent].default_framework".to_string(),
            )
        } else {
            ("claude-code".to_string(), "built-in fallback".to_string())
        };
        rationale.push(format!(
            "agent: tier={tier} was \"auto\" — resolved to {agent} ({why})"
        ));
        agent
    } else {
        candidate.to_string()
    }
}

// ── Security tier (new, reuses AdvisorSecurity) ──────────────────────────

fn resolve_security(
    ctx: &Context,
    team: &TeamRole,
    workload: &WorkloadClassification,
    workflow_toml: &WorkflowToml,
    workspace_root: &Path,
    rationale: &mut Vec<String>,
) -> AdvisorSecurity {
    let (raw, tier) = if let Some(s) = ctx.cli_security.filter(|s| !s.is_empty()) {
        (s.to_string(), "explicit")
    } else if let Some(binding) = workflow_toml.workload_types.get(&workload.workload_type) {
        if let Some(s) = binding.security.as_deref().filter(|s| !s.is_empty()) {
            (s.to_string(), "workload-type")
        } else {
            resolve_security_from_team_or_default(team, workflow_toml, workspace_root)
        }
    } else {
        resolve_security_from_team_or_default(team, workflow_toml, workspace_root)
    };

    let resolved = AdvisorSecurity::from_str(&raw).unwrap_or_default();
    rationale.push(format!("security_tier: tier={tier} value={raw}"));

    if resolved == AdvisorSecurity::Auto && workload.confidence < AUTO_SECURITY_CONFIDENCE_THRESHOLD
    {
        rationale.push(format!(
            "security_tier: downgraded auto→suggest — workload classification confidence {:.2} \
             is below the {:.2} threshold required to hand full autonomy to a low-confidence \
             workload guess",
            workload.confidence, AUTO_SECURITY_CONFIDENCE_THRESHOLD
        ));
        return AdvisorSecurity::Suggest;
    }
    resolved
}

fn resolve_security_from_team_or_default(
    team: &TeamRole,
    workflow_toml: &WorkflowToml,
    workspace_root: &Path,
) -> (String, &'static str) {
    let team_config = TeamConfig::load(workspace_root).unwrap_or_default();
    if let Some(member) = team_config.find_by_role(team) {
        return (member.security.to_string(), "team.toml");
    }
    if !workflow_toml.security.default.is_empty() {
        return (
            workflow_toml.security.default.clone(),
            "workflow.toml-default",
        );
    }
    ("read_only".to_string(), "fallback")
}

// ── Priority tier (new) ───────────────────────────────────────────────────

fn resolve_priority(
    ctx: &Context,
    workload: &WorkloadClassification,
    workflow_toml: &WorkflowToml,
    rationale: &mut Vec<String>,
) -> Priority {
    if let Some(p) = ctx.cli_priority.filter(|p| !p.is_empty()) {
        if let Ok(parsed) = Priority::from_str(p) {
            rationale.push(format!("priority: tier=explicit value={parsed}"));
            return parsed;
        }
    }
    if let Some(binding) = workflow_toml.workload_types.get(&workload.workload_type) {
        if let Some(p) = binding.priority.as_deref().filter(|p| !p.is_empty()) {
            if let Ok(parsed) = Priority::from_str(p) {
                rationale.push(format!(
                    "priority: tier=workload-type({}) value={parsed}",
                    workload.workload_type
                ));
                return parsed;
            }
        }
    }
    if !workflow_toml.priority.default.is_empty() {
        if let Ok(parsed) = Priority::from_str(&workflow_toml.priority.default) {
            rationale.push(format!(
                "priority: tier=workflow.toml-default value={parsed}"
            ));
            return parsed;
        }
    }
    let detected = crate::priority::detect_urgency(&ctx.classification_text);
    rationale.push(format!("priority: tier=keyword-detection value={detected}"));
    detected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ExplicitGoalRequest, TriggerRoutingInput};
    use chrono::Utc;
    use ta_goal::persona::{PersonaConfig, PersonaInner};
    use ta_session::agent_action::TeamMember;
    use ta_session::team::TeamConfig;
    use uuid::Uuid;

    fn explicit(title: &str) -> RoutingInput {
        RoutingInput::ExplicitGoal(ExplicitGoalRequest::new(title))
    }

    fn write_workflow_toml(workspace_root: &Path, contents: &str) {
        let dir = workspace_root.join(".ta");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workflow.toml"), contents).unwrap();
    }

    // ── agent tier (extends v0.17.0.12.13) ──────────────────────────────

    #[test]
    fn agent_explicit_beats_everything() {
        let tmp = tempfile::tempdir().unwrap();
        write_workflow_toml(tmp.path(), "[agent]\ndefault = \"codex\"\n");
        let mut req = ExplicitGoalRequest::new("fix the bug");
        req.cli_agent = Some("claude-opus-4-8".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.agent, "claude-opus-4-8");
    }

    #[test]
    fn agent_workflow_toml_default_used_when_no_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        write_workflow_toml(tmp.path(), "[agent]\ndefault = \"codex\"\n");
        let decision = route(&explicit("fix the bug"), tmp.path());
        assert_eq!(decision.agent, "codex");
    }

    #[test]
    fn agent_falls_back_to_claude_code() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("fix the bug"), tmp.path());
        assert_eq!(decision.agent, "claude-code");
    }

    #[test]
    fn agent_auto_resolves_via_trusted_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta")).unwrap();
        std::fs::write(
            tmp.path().join(".ta").join("daemon.toml"),
            "[agent]\ntrusted_binaries = { zeta = \"path\", alpha = \"path\" }\n",
        )
        .unwrap();
        write_workflow_toml(tmp.path(), "[agent]\ndefault = \"auto\"\n");
        let decision = route(&explicit("fix the bug"), tmp.path());
        assert_eq!(decision.agent, "alpha");
        assert!(decision
            .rationale
            .iter()
            .any(|l| l.contains("was \"auto\"")));
    }

    // ── team tier ────────────────────────────────────────────────────────

    #[test]
    fn team_explicit_flag_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = ExplicitGoalRequest::new("fix the bug");
        req.cli_team = Some("qa".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.team, TeamRole::qa());
    }

    #[test]
    fn team_heuristic_maps_security_workload_to_reviewer() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("Fix the auth bypass vulnerability"), tmp.path());
        assert_eq!(decision.workload_type, "security");
        assert_eq!(decision.team, TeamRole::reviewer());
    }

    #[test]
    fn team_heuristic_defaults_to_implementer() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("Add a new dashboard widget"), tmp.path());
        assert_eq!(decision.team, TeamRole::implementer());
    }

    #[test]
    fn team_workload_type_binding_beats_heuristic() {
        let tmp = tempfile::tempdir().unwrap();
        write_workflow_toml(
            tmp.path(),
            "[workload_types.bugfix]\nteam = \"architect\"\n",
        );
        let decision = route(&explicit("Fix the login bug"), tmp.path());
        assert_eq!(decision.workload_type, "bugfix");
        assert_eq!(decision.team, TeamRole::architect());
    }

    // ── persona tier ─────────────────────────────────────────────────────

    #[test]
    fn persona_none_when_unset_anywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("Add a new dashboard widget"), tmp.path());
        assert_eq!(decision.persona, None);
    }

    #[test]
    fn persona_from_team_toml_member_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let mut team_config = TeamConfig::default();
        team_config.members.push(TeamMember {
            role: TeamRole::implementer(),
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::ReadOnly,
            persona: Some("careful-implementer".to_string()),
        });
        team_config.save(tmp.path()).unwrap();
        let decision = route(&explicit("Add a new dashboard widget"), tmp.path());
        assert_eq!(decision.persona, Some("careful-implementer".to_string()));
    }

    #[test]
    fn persona_explicit_beats_team_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let mut team_config = TeamConfig::default();
        team_config.members.push(TeamMember {
            role: TeamRole::implementer(),
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::ReadOnly,
            persona: Some("careful-implementer".to_string()),
        });
        team_config.save(tmp.path()).unwrap();
        let mut req = ExplicitGoalRequest::new("Add a new dashboard widget");
        req.cli_persona = Some("fast-mode".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.persona, Some("fast-mode".to_string()));
    }

    #[test]
    fn agent_uses_resolved_persona_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let persona = PersonaConfig {
            persona: PersonaInner {
                name: "fast-mode".to_string(),
                description: String::new(),
                system_prompt: String::new(),
                constitution: None,
                agent: Some("claude-opus-4-8".to_string()),
            },
            capabilities: Default::default(),
            style: Default::default(),
        };
        persona.save(tmp.path()).unwrap();
        let mut req = ExplicitGoalRequest::new("Add a new dashboard widget");
        req.cli_persona = Some("fast-mode".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.agent, "claude-opus-4-8");
    }

    // ── security tier ────────────────────────────────────────────────────

    #[test]
    fn security_defaults_to_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("Add a new dashboard widget"), tmp.path());
        assert_eq!(decision.security_tier, AdvisorSecurity::ReadOnly);
    }

    #[test]
    fn security_explicit_flag_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = ExplicitGoalRequest::new("Add a new dashboard widget");
        req.cli_security = Some("suggest".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.security_tier, AdvisorSecurity::Suggest);
    }

    #[test]
    fn security_auto_downgraded_to_suggest_for_low_confidence_workload() {
        let tmp = tempfile::tempdir().unwrap();
        // "xyzzy plugh" classifies as "other" at 0.3 confidence, below the
        // 0.65 auto-eligibility threshold.
        let mut req = ExplicitGoalRequest::new("xyzzy plugh");
        req.cli_security = Some("auto".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.security_tier, AdvisorSecurity::Suggest);
        assert!(decision
            .rationale
            .iter()
            .any(|l| l.contains("downgraded auto")));
    }

    #[test]
    fn security_auto_kept_for_high_confidence_workload() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = ExplicitGoalRequest::new("Fix the login bug");
        req.cli_security = Some("auto".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert!(decision.workload_confidence >= AUTO_SECURITY_CONFIDENCE_THRESHOLD);
        assert_eq!(decision.security_tier, AdvisorSecurity::Auto);
    }

    #[test]
    fn security_from_team_toml_role_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let mut team_config = TeamConfig::default();
        team_config.members.push(TeamMember {
            role: TeamRole::implementer(),
            agent_id: "claude-sonnet-4-6".to_string(),
            security: AdvisorSecurity::Suggest,
            persona: None,
        });
        team_config.save(tmp.path()).unwrap();
        let decision = route(&explicit("Add a new dashboard widget"), tmp.path());
        assert_eq!(decision.security_tier, AdvisorSecurity::Suggest);
    }

    // ── priority tier ────────────────────────────────────────────────────

    #[test]
    fn priority_explicit_flag_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = ExplicitGoalRequest::new("Update the README docs");
        req.cli_priority = Some("urgent".to_string());
        let decision = route(&RoutingInput::ExplicitGoal(req), tmp.path());
        assert_eq!(decision.priority, Priority::Urgent);
    }

    #[test]
    fn priority_keyword_detected_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let decision = route(&explicit("Production down, need a hotfix"), tmp.path());
        assert_eq!(decision.priority, Priority::Urgent);
    }

    #[test]
    fn priority_workload_type_binding() {
        let tmp = tempfile::tempdir().unwrap();
        write_workflow_toml(tmp.path(), "[workload_types.docs]\npriority = \"low\"\n");
        let decision = route(&explicit("Update the README docs"), tmp.path());
        assert_eq!(decision.workload_type, "docs");
        assert_eq!(decision.priority, Priority::Low);
    }

    // ── explicit vs. triggered equivalence ──────────────────────────────

    #[test]
    fn explicit_and_triggered_inputs_produce_identical_decisions() {
        let tmp = tempfile::tempdir().unwrap();
        write_workflow_toml(
            tmp.path(),
            "[workload_types.bugfix]\nteam = \"architect\"\npersona = \"careful-reviewer\"\nsecurity = \"suggest\"\npriority = \"high\"\n",
        );

        let explicit_decision = route(&explicit("Fix the login bug"), tmp.path());

        let event = ta_intake::TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "inbound-email".to_string(),
            source: "test".to_string(),
            occurred_at: Utc::now(),
            payload: serde_json::json!({}),
            suggested_goal_title: "Fix the login bug".to_string(),
            dedupe_key: None,
        };
        let triggered_decision = route(
            &RoutingInput::Trigger(TriggerRoutingInput::from_event(event)),
            tmp.path(),
        );

        assert_eq!(explicit_decision.team, triggered_decision.team);
        assert_eq!(explicit_decision.persona, triggered_decision.persona);
        assert_eq!(explicit_decision.agent, triggered_decision.agent);
        assert_eq!(
            explicit_decision.security_tier,
            triggered_decision.security_tier
        );
        assert_eq!(explicit_decision.priority, triggered_decision.priority);
        assert_eq!(
            explicit_decision.workload_type,
            triggered_decision.workload_type
        );
    }

    #[test]
    fn trigger_manifest_settings_hint_override_workload_and_priority() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = ta_intake::TriggerManifest::load(&{
            let dir = tmp.path().join(".ta").join("triggers");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("webhook.toml");
            std::fs::write(
                &path,
                "type = \"webhook\"\n\n[settings]\nteam = \"qa\"\npriority = \"urgent\"\n",
            )
            .unwrap();
            path
        })
        .unwrap();

        let event = ta_intake::TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "webhook".to_string(),
            source: "test".to_string(),
            occurred_at: Utc::now(),
            payload: serde_json::json!({}),
            suggested_goal_title: "Add a new dashboard widget".to_string(),
            dedupe_key: None,
        };
        let decision = route(
            &RoutingInput::Trigger(TriggerRoutingInput::from_event_and_manifest(
                event, &manifest,
            )),
            tmp.path(),
        );
        assert_eq!(decision.team, TeamRole::qa());
        assert_eq!(decision.priority, Priority::Urgent);
    }
}
