// origin.rs: goal origin, which component asked for a goal (H9).
//
// A goal's `origin` names the component that originated it (e.g. `cos` when
// the Chief-of-Staff classified and dispatched it, `chat` for a chat-mode
// session). It is set with `ta run --origin <name>` (or `TA_GOAL_ORIGIN`),
// and by `GatewayState::start_chat_session` for chat sessions.
//
// Security hypothesis H9 (docs/superpowers/specs/security-hypotheses.md):
// no goal receives an auto-approve shortcut solely because CoS originated
// it. CoS is the component most exposed to untrusted external input (chat,
// forum posts, meeting notes), so work it dispatches must never get a
// "trusted because CoS asked" fast lane. Every auto-approve path in TA core
// (policy auto-approve, the workflow-graph decision, advisor `auto`
// security, `ta draft apply`'s apply-implies-approval) consults
// `origin_blocks_auto_approve` deterministically, in code, not in a prompt,
// and falls through to normal human review.

/// Origins whose goals are never auto-approved by any path. Add new
/// untrusted-ingress origins here; every auto-approve path picks them up.
pub const NO_AUTO_APPROVE_ORIGINS: &[&str] = &["cos", "chat"];

/// The origin stamped on chat-mode sessions by the MCP gateway.
pub const CHAT_ORIGIN: &str = "chat";

/// Environment variable `ta run` reads as the goal origin when `--origin`
/// is not given. `ta run --origin <name>` also exports it, so nested
/// invocations (paired shadow goals, sub-goals the agent starts) inherit
/// the origin rather than silently dropping it.
pub const ORIGIN_ENV_VAR: &str = "TA_GOAL_ORIGIN";

/// Read and validate the goal origin from [`ORIGIN_ENV_VAR`]. Unset or
/// blank means no origin. An invalid value is an error (fail closed: a
/// typo must not silently drop a `cos` origin).
pub fn origin_from_env() -> Result<Option<String>, String> {
    match std::env::var(ORIGIN_ENV_VAR) {
        Ok(v) if v.trim().is_empty() => Ok(None),
        Ok(v) => validate_origin(v.trim())
            .map(Some)
            .map_err(|e| format!("{} (from ${})", e, ORIGIN_ENV_VAR)),
        Err(_) => Ok(None),
    }
}

/// Maximum length of an origin name.
pub const MAX_ORIGIN_LEN: usize = 32;

/// Validate an origin name supplied on the command line or via
/// `TA_GOAL_ORIGIN`: 1..=32 characters from `[a-z0-9_-]`, starting with a
/// letter. Returns the accepted value unchanged, or an actionable error.
pub fn validate_origin(origin: &str) -> Result<String, String> {
    let valid_chars = origin
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    let starts_with_letter = origin
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase());
    if origin.is_empty() || origin.len() > MAX_ORIGIN_LEN || !valid_chars || !starts_with_letter {
        return Err(format!(
            "invalid goal origin {:?}: must be 1-{} characters of lowercase letters, digits, \
             '-' or '_', starting with a letter (e.g. --origin cos)",
            origin, MAX_ORIGIN_LEN
        ));
    }
    Ok(origin.to_string())
}

/// Whether a goal with this origin must never be auto-approved.
///
/// Compared case-insensitively and after trimming, so a record written by
/// some other tool with `"COS"` or `" cos"` cannot slip past the check.
pub fn origin_blocks_auto_approve(origin: Option<&str>) -> bool {
    match origin {
        Some(o) => {
            let o = o.trim().to_ascii_lowercase();
            NO_AUTO_APPROVE_ORIGINS.iter().any(|blocked| *blocked == o)
        }
        None => false,
    }
}

/// Whether a goal must stay out of the plan entirely: no auto-linked phase,
/// no ad-hoc stub in `PLAN.md`, no `in_progress` marker, no claim.
///
/// True for any chat-mode goal and any goal whose origin is in
/// [`NO_AUTO_APPROVE_ORIGINS`]. Such a goal is a conversation, not a unit of
/// planned work: it usually ends with no draft, so a phase it claimed would
/// stay `in_progress` forever and fail every later launch that linked it.
pub fn goal_never_claims_plan_phase(chat_mode: bool, origin: Option<&str>) -> bool {
    chat_mode || origin_blocks_auto_approve(origin)
}

/// The origin `ta run` should write onto a goal record, given the origin
/// already on the record and the one requested for this run (`--origin` /
/// `TA_GOAL_ORIGIN`). `None` means "leave the record alone".
///
/// A goal's origin is fixed once it is on the record: a later run against
/// the same goal (`ta run --goal-id`) can never change or clear it, whatever
/// its environment says. The environment only fills in a record that has no
/// origin yet, which in practice is goal creation. Every auto-approve
/// decision then reads the record, never the environment.
pub fn origin_to_stamp(existing: Option<&str>, requested: Option<&str>) -> Option<String> {
    match (existing, requested) {
        (None, Some(r)) => Some(r.to_string()),
        _ => None,
    }
}

/// Origin settings a daemon-built `ta run` for one team role must carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOrigin {
    /// Pass `--chat-mode` (read-only chat session).
    pub chat_mode: bool,
    /// Pass `--origin <value>`.
    pub origin: Option<String>,
}

/// Work out the origin and chat-mode flags for a role launch from its
/// persona (`[capabilities] chat_mode` / `origin`) and its team role
/// (`team.toml` member `origin`).
///
/// Rules (fail closed):
/// - Each declared origin must pass [`validate_origin`].
/// - If both the persona and the role declare an origin they must agree.
/// - A chat-mode persona with no declared origin gets [`CHAT_ORIGIN`].
/// - An origin that is never auto-approved (`cos`, `chat`) forces chat mode,
///   so a role marked as the CoS can never launch with a full tool surface.
/// - A chat-mode launch must carry an origin that is never auto-approved.
pub fn resolve_launch_origin(
    persona_chat_mode: bool,
    persona_origin: Option<&str>,
    role_origin: Option<&str>,
) -> Result<LaunchOrigin, String> {
    let persona_origin = persona_origin
        .map(|o| validate_origin(o.trim()).map_err(|e| format!("persona origin: {}", e)))
        .transpose()?;
    let role_origin = role_origin
        .map(|o| validate_origin(o.trim()).map_err(|e| format!("team role origin: {}", e)))
        .transpose()?;
    if let (Some(p), Some(r)) = (&persona_origin, &role_origin) {
        if p != r {
            return Err(format!(
                "the persona declares origin {:?} but the team role declares origin {:?}; \
                 set the same origin in both places (or remove one)",
                p, r
            ));
        }
    }
    let mut origin = role_origin.or(persona_origin);
    if persona_chat_mode && origin.is_none() {
        origin = Some(CHAT_ORIGIN.to_string());
    }
    let chat_mode = persona_chat_mode || origin_blocks_auto_approve(origin.as_deref());
    if chat_mode && !origin_blocks_auto_approve(origin.as_deref()) {
        return Err(format!(
            "a chat-mode launch must carry an origin that is never auto-approved ({}), \
             but origin {:?} was declared; use origin = \"cos\" or remove the origin",
            NO_AUTO_APPROVE_ORIGINS.join(", "),
            origin.as_deref().unwrap_or_default()
        ));
    }
    Ok(LaunchOrigin { chat_mode, origin })
}

/// The user-visible refusal text every auto-approve path uses, so log
/// lines, `ta draft view` and `ta draft apply` output all say the same
/// thing: `auto-approve refused: origin=cos`.
pub fn auto_approve_refusal(origin: &str) -> String {
    format!(
        "auto-approve refused: origin={} (goals from this origin always require human review; \
         run `ta draft approve <id>` after reviewing)",
        origin.trim().to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_mode_and_untrusted_origins_never_claim_a_plan_phase() {
        assert!(goal_never_claims_plan_phase(true, None));
        assert!(goal_never_claims_plan_phase(false, Some("cos")));
        assert!(goal_never_claims_plan_phase(false, Some(" Chat ")));
        assert!(goal_never_claims_plan_phase(true, Some("cli")));
        assert!(!goal_never_claims_plan_phase(false, None));
        assert!(!goal_never_claims_plan_phase(false, Some("cli")));
    }

    #[test]
    fn cos_and_chat_block_auto_approve_case_insensitively() {
        for o in ["cos", "chat", "COS", " Chat "] {
            assert!(origin_blocks_auto_approve(Some(o)), "{:?}", o);
        }
    }

    #[test]
    fn none_and_other_origins_do_not_block() {
        assert!(!origin_blocks_auto_approve(None));
        for o in ["cli", "poller", "cosmic", "chatbot", ""] {
            assert!(!origin_blocks_auto_approve(Some(o)), "{:?}", o);
        }
    }

    #[test]
    fn validate_origin_accepts_simple_names() {
        for o in ["cos", "chat", "poller-2", "ci_bot"] {
            assert_eq!(validate_origin(o).unwrap(), o);
        }
    }

    #[test]
    fn validate_origin_rejects_bad_names() {
        let long = "a".repeat(MAX_ORIGIN_LEN + 1);
        for o in [
            "",
            "COS",
            "1cos",
            "-cos",
            "c o s",
            "cos;rm",
            "c\u{e9}s",
            long.as_str(),
        ] {
            assert!(validate_origin(o).is_err(), "{:?}", o);
        }
    }

    /// Single test touching the env var, so parallel tests never race on it
    /// (no other test in this crate reads or writes TA_GOAL_ORIGIN).
    #[test]
    fn origin_from_env_reads_validates_and_fails_closed() {
        std::env::remove_var(ORIGIN_ENV_VAR);
        assert_eq!(origin_from_env().unwrap(), None);
        std::env::set_var(ORIGIN_ENV_VAR, "  ");
        assert_eq!(origin_from_env().unwrap(), None);
        std::env::set_var(ORIGIN_ENV_VAR, "cos");
        assert_eq!(origin_from_env().unwrap().as_deref(), Some("cos"));
        std::env::set_var(ORIGIN_ENV_VAR, "COS!");
        assert!(origin_from_env().is_err());
        std::env::remove_var(ORIGIN_ENV_VAR);
    }

    #[test]
    fn origin_on_record_is_never_changed_by_a_later_request() {
        assert_eq!(origin_to_stamp(None, Some("cos")).as_deref(), Some("cos"));
        assert_eq!(origin_to_stamp(None, None), None);
        assert_eq!(origin_to_stamp(Some("cos"), Some("cli")), None);
        assert_eq!(origin_to_stamp(Some("cos"), None), None);
        assert_eq!(origin_to_stamp(Some("cli"), Some("cos")), None);
    }

    #[test]
    fn chat_mode_persona_defaults_to_chat_origin() {
        let l = resolve_launch_origin(true, None, None).unwrap();
        assert!(l.chat_mode);
        assert_eq!(l.origin.as_deref(), Some("chat"));
    }

    #[test]
    fn chat_mode_persona_with_cos_origin() {
        let l = resolve_launch_origin(true, Some("cos"), None).unwrap();
        assert_eq!(
            l,
            LaunchOrigin {
                chat_mode: true,
                origin: Some("cos".into())
            }
        );
    }

    #[test]
    fn cos_role_forces_chat_mode_even_without_a_chat_persona() {
        let l = resolve_launch_origin(false, None, Some("cos")).unwrap();
        assert!(l.chat_mode);
        assert_eq!(l.origin.as_deref(), Some("cos"));
    }

    #[test]
    fn plain_persona_has_no_origin_and_no_chat_mode() {
        assert_eq!(
            resolve_launch_origin(false, None, None).unwrap(),
            LaunchOrigin::default()
        );
        let l = resolve_launch_origin(false, Some("poller"), None).unwrap();
        assert!(!l.chat_mode);
        assert_eq!(l.origin.as_deref(), Some("poller"));
    }

    #[test]
    fn launch_origin_fails_closed() {
        // Invalid names.
        assert!(resolve_launch_origin(true, Some("COS"), None).is_err());
        assert!(resolve_launch_origin(false, None, Some("c o s")).is_err());
        // Persona and role disagree.
        assert!(resolve_launch_origin(true, Some("cos"), Some("chat")).is_err());
        // Chat mode with an origin that would be auto-approvable.
        assert!(resolve_launch_origin(true, Some("poller"), None).is_err());
        assert!(resolve_launch_origin(true, None, Some("poller")).is_err());
    }

    #[test]
    fn refusal_text_names_the_origin() {
        assert!(auto_approve_refusal("cos").starts_with("auto-approve refused: origin=cos"));
    }
}
