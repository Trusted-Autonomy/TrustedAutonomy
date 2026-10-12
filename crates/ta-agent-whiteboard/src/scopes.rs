//! Whiteboard capability scope names (v0.17.12.2).
//!
//! A role launch gets one Biscuit token whose scopes name its session, its
//! role, optionally the right to post outcomes, and the execution target it
//! runs on. The daemon reads all of these from the verified token; nothing
//! about the sender's identity is taken from a request body.
//!
//! - `whiteboard:team_session:<s>`: member of session `s` (presence, handoff,
//!   tasks). Role-less tokens minted before this phase carry only this.
//! - `whiteboard:team_session:<s>:role:<r>`: the launch is role `r` in `s`.
//! - `whiteboard:outcome:send`: may post outcomes. Granted only to the roles
//!   listed in `[whiteboard] outcome_roles` (default `chief-of-staff`).
//! - `whiteboard:target:<t>`: the execution target identity (default `local`).

/// The distinct right to post an outcome.
pub const OUTCOME_SEND_SCOPE: &str = "whiteboard:outcome:send";

/// Execution target stamped when a token names none.
pub const DEFAULT_TARGET: &str = "local";

pub fn session_scope(team_session: &str) -> String {
    format!("whiteboard:team_session:{team_session}")
}

pub fn role_scope(team_session: &str, role: &str) -> String {
    format!("whiteboard:team_session:{team_session}:role:{role}")
}

pub fn target_scope(target: &str) -> String {
    format!("whiteboard:target:{target}")
}

/// The role a token names for `team_session`, or `None` when it names none or
/// more than one (ambiguous is treated as no role).
pub fn role_from_scopes(scopes: &[String], team_session: &str) -> Option<String> {
    let prefix = format!("whiteboard:team_session:{team_session}:role:");
    let mut roles = scopes.iter().filter_map(|s| s.strip_prefix(&prefix));
    let first = roles.next()?;
    if roles.next().is_some() || first.is_empty() {
        return None;
    }
    Some(first.to_string())
}

/// The execution target a token names, defaulting to [`DEFAULT_TARGET`].
pub fn target_from_scopes(scopes: &[String]) -> String {
    let mut targets = scopes
        .iter()
        .filter_map(|s| s.strip_prefix("whiteboard:target:"));
    match (targets.next(), targets.next()) {
        (Some(t), None) if !t.is_empty() => t.to_string(),
        _ => DEFAULT_TARGET.to_string(),
    }
}

/// Whether the scopes include the right to post outcomes.
pub fn may_send_outcome(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s == OUTCOME_SEND_SCOPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_is_read_only_for_the_named_session() {
        let scopes = vec![
            session_scope("s1"),
            role_scope("s1", "chief-of-staff"),
            OUTCOME_SEND_SCOPE.to_string(),
        ];
        assert_eq!(
            role_from_scopes(&scopes, "s1").as_deref(),
            Some("chief-of-staff")
        );
        assert_eq!(role_from_scopes(&scopes, "s2"), None);
        assert!(may_send_outcome(&scopes));
    }

    #[test]
    fn two_roles_in_one_token_are_ambiguous() {
        let scopes = vec![role_scope("s1", "a"), role_scope("s1", "b")];
        assert_eq!(role_from_scopes(&scopes, "s1"), None);
    }

    #[test]
    fn target_defaults_to_local() {
        assert_eq!(target_from_scopes(&[]), "local");
        assert_eq!(
            target_from_scopes(&[target_scope("runner:build-7")]),
            "runner:build-7"
        );
    }
}
