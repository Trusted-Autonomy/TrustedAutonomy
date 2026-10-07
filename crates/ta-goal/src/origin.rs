// origin.rs — Goal origin: which component asked for a goal (H9).
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
    fn refusal_text_names_the_origin() {
        assert!(auto_approve_refusal("cos").starts_with("auto-approve refused: origin=cos"));
    }
}
