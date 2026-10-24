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

/// The origin stamped on drafts produced by the virtual-team project manager
/// (the funnel consumer). It is NOT in [`NO_AUTO_APPROVE_ORIGINS`]: such a
/// draft is auto-approvable only when a constitution rule lists it in
/// `origins = [...]` (see [`RULE_GATED_ORIGINS`]).
pub const PROJECT_MANAGER_ORIGIN: &str = "project-manager";

/// The team role name whose launches the gateway stamps with
/// [`PROJECT_MANAGER_ORIGIN`].
pub const PROJECT_MANAGER_ROLE: &str = "project-manager";

/// Environment variable the launcher sets to the launching team role.
pub const TEAM_ROLE_ENV_VAR: &str = "TA_WHITEBOARD_TEAM_ROLE";

/// Origins whose drafts can be auto-approved only by a constitution rule that
/// lists them in `origins`. Never auto-approved by any other path.
pub const RULE_GATED_ORIGINS: &[&str] = &[PROJECT_MANAGER_ORIGIN];

/// Whether a goal with this origin is auto-approvable only through a
/// constitution rule naming the origin. Case-insensitive and trimmed, like
/// [`origin_blocks_auto_approve`].
pub fn origin_is_rule_gated(origin: Option<&str>) -> bool {
    match origin {
        Some(o) => {
            let o = o.trim().to_ascii_lowercase();
            RULE_GATED_ORIGINS.iter().any(|g| *g == o)
        }
        None => false,
    }
}

/// The launching team role from [`TEAM_ROLE_ENV_VAR`]; unset or blank is `None`.
pub fn team_role_from_env() -> Option<String> {
    match std::env::var(TEAM_ROLE_ENV_VAR) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

/// Decide the origin a launch carries, given the launching team role and the
/// origin requested (`--origin` / `TA_GOAL_ORIGIN`). Fails closed.
///
/// - Role `project-manager`, nothing requested: stamped `project-manager`.
/// - Role `project-manager`, `project-manager` requested: same.
/// - Role `project-manager`, `cos`/`chat` requested: that origin is kept
///   (the more restrictive one wins; H9 is unchanged).
/// - Role `project-manager`, any other origin requested: error naming both.
/// - Any other role (or none) requesting `project-manager`: error, the origin
///   is gateway-stamped for the project-manager role only.
/// - Everything else: the requested origin unchanged.
pub fn stamp_launch_origin(
    team_role: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let is_pm_role = team_role
        .map(|r| r.trim().eq_ignore_ascii_case(PROJECT_MANAGER_ROLE))
        .unwrap_or(false);
    let requested_trimmed = requested.map(str::trim).filter(|r| !r.is_empty());
    let requests_pm = origin_is_rule_gated(requested_trimmed);
    if is_pm_role {
        match requested_trimmed {
            None => Ok(Some(PROJECT_MANAGER_ORIGIN.to_string())),
            Some(_) if requests_pm => Ok(Some(PROJECT_MANAGER_ORIGIN.to_string())),
            Some(r) if origin_blocks_auto_approve(Some(r)) => Ok(Some(r.to_ascii_lowercase())),
            Some(r) => Err(format!(
                "team role `{PROJECT_MANAGER_ROLE}` was launched with origin `{r}`, which conflicts \
                 with the `{PROJECT_MANAGER_ORIGIN}` origin the gateway stamps for that role; \
                 remove --origin/{ORIGIN_ENV_VAR} (only `{PROJECT_MANAGER_ORIGIN}`, `cos` or `chat` \
                 are accepted for this role)"
            )),
        }
    } else if requests_pm {
        Err(format!(
            "the {PROJECT_MANAGER_ORIGIN} origin is stamped by the gateway for the \
             {PROJECT_MANAGER_ROLE} role only (launching role: {}); remove \
             --origin/{ORIGIN_ENV_VAR}",
            team_role
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .unwrap_or("none")
        ))
    } else {
        Ok(requested.map(str::to_string))
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

// ── Origin class (accounting only) ───────────────────────────────────────────
//
// `origin_class` records WHY a goal started: a person asked for it, someone
// suggested it, or nobody asked. It exists so spend can be reported and capped
// by that class (`GET /api/spend`, `ta run --max-usd`).
//
// IT IS ACCOUNTING DATA ONLY. It is never read by auto-approve, policy,
// routing, credential scoping, sandbox selection or any other authorization
// decision. Those read `origin` (via `origin_blocks_auto_approve`) and the
// human credential, never this. A test (`origin_class_is_never_read_by_a_decision_path`)
// scans the source tree and fails if a decision module mentions it.
//
// It is set by the LAUNCHER only: `ta run --origin-class`, `$TA_GOAL_ORIGIN_CLASS`
// or the `origin_class` field of `--provenance-file`. No agent-facing surface
// (the `ta_goal_start` MCP tool, chat-mode tools, the daemon API) accepts it,
// it is stamped once at goal creation and never changed, and nested goals
// inherit it through `$TA_GOAL_ORIGIN_CLASS`.

/// Environment variable `ta run` reads as the goal origin class when
/// `--origin-class` is not given. `ta run` exports it so goals started from
/// inside a running goal inherit it.
pub const ORIGIN_CLASS_ENV_VAR: &str = "TA_GOAL_ORIGIN_CLASS";

/// Why a goal started. Accounting data only: see the section comment above.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OriginClass {
    /// A person asked for this work.
    Asked,
    /// Someone or something suggested it and it was accepted.
    Suggested,
    /// Nobody asked. The default, so unclassified spend is never hidden.
    #[default]
    Unasked,
}

impl OriginClass {
    /// Every class, in report order.
    pub const ALL: [OriginClass; 3] = [
        OriginClass::Asked,
        OriginClass::Suggested,
        OriginClass::Unasked,
    ];

    /// The lowercase name used on the command line, in JSON and in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            OriginClass::Asked => "asked",
            OriginClass::Suggested => "suggested",
            OriginClass::Unasked => "unasked",
        }
    }
}

impl std::fmt::Display for OriginClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OriginClass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        parse_origin_class(s)
    }
}

/// Parse an origin class (case-insensitive, trimmed). An unknown value is an
/// error that lists the accepted ones, so a typo never silently becomes
/// `unasked`.
pub fn parse_origin_class(s: &str) -> Result<OriginClass, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "asked" => Ok(OriginClass::Asked),
        "suggested" => Ok(OriginClass::Suggested),
        "unasked" => Ok(OriginClass::Unasked),
        other => Err(format!(
            "invalid origin class {:?}: expected one of asked, suggested, unasked \
             (e.g. --origin-class asked)",
            other
        )),
    }
}

/// Read and validate the origin class from [`ORIGIN_CLASS_ENV_VAR`]. Unset or
/// blank means "not given". An invalid value is an error (fail loudly).
pub fn origin_class_from_env() -> Result<Option<OriginClass>, String> {
    match std::env::var(ORIGIN_CLASS_ENV_VAR) {
        Ok(v) if v.trim().is_empty() => Ok(None),
        Ok(v) => parse_origin_class(&v)
            .map(Some)
            .map_err(|e| format!("{} (from ${})", e, ORIGIN_CLASS_ENV_VAR)),
        Err(_) => Ok(None),
    }
}

/// The origin class a goal started by an agent-facing surface (the MCP
/// `ta_goal_start` tool, a nested `ta` call) receives: only what the launcher
/// exported, else `unasked`. Never taken from a tool argument.
pub fn inherited_origin_class() -> OriginClass {
    match origin_class_from_env() {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "ignoring an invalid inherited origin class; the goal is recorded as unasked. \
                 Set TA_GOAL_ORIGIN_CLASS to asked, suggested or unasked"
            );
            OriginClass::default()
        }
    }
}

/// Pick the class to stamp on a goal at creation.
///
/// `existing` is what the record already carries (`Some` means it was stamped
/// at creation and is fixed). `requested` is what the launcher asked for (flag,
/// provenance file or environment, already merged by the caller);
/// `inherited` is `$TA_GOAL_ORIGIN_CLASS`; `launched_by_agent` says the
/// process is running inside an agent.
///
/// Returns `None` to leave the record alone, else the class to write.
/// Rules: an existing class is never changed; when launched by an agent the
/// inherited class wins over any request (an agent cannot raise or lower the
/// class of work it starts); otherwise the request, then the inherited class,
/// then `unasked`.
pub fn origin_class_to_stamp(
    existing: Option<OriginClass>,
    requested: Option<OriginClass>,
    inherited: Option<OriginClass>,
    launched_by_agent: bool,
) -> Option<OriginClass> {
    if existing.is_some() {
        return None;
    }
    let chosen = if launched_by_agent {
        inherited
    } else {
        requested.or(inherited)
    };
    Some(chosen.unwrap_or_default())
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

    #[test]
    fn origin_class_default_is_unasked_and_parse_validates() {
        assert_eq!(OriginClass::default(), OriginClass::Unasked);
        assert_eq!(parse_origin_class("asked"), Ok(OriginClass::Asked));
        assert_eq!(
            parse_origin_class(" Suggested "),
            Ok(OriginClass::Suggested)
        );
        assert_eq!(parse_origin_class("UNASKED"), Ok(OriginClass::Unasked));
        for bad in ["", "ask", "requested", "cos", "asked,unasked"] {
            let e = parse_origin_class(bad).unwrap_err();
            assert!(e.contains("asked, suggested, unasked"), "{e}");
        }
        assert_eq!("asked".parse::<OriginClass>(), Ok(OriginClass::Asked));
    }

    #[test]
    fn origin_class_serde_uses_snake_case_and_rejects_unknown() {
        assert_eq!(
            serde_json::to_string(&OriginClass::Suggested).unwrap(),
            "\"suggested\""
        );
        let c: OriginClass = serde_json::from_str("\"asked\"").unwrap();
        assert_eq!(c, OriginClass::Asked);
        assert!(serde_json::from_str::<OriginClass>("\"Asked\"").is_err());
        assert!(serde_json::from_str::<OriginClass>("\"bogus\"").is_err());
    }

    #[test]
    fn stamping_follows_request_then_inherited_then_default() {
        use OriginClass::*;
        assert_eq!(
            origin_class_to_stamp(None, None, None, false),
            Some(Unasked)
        );
        assert_eq!(
            origin_class_to_stamp(None, Some(Asked), None, false),
            Some(Asked)
        );
        assert_eq!(
            origin_class_to_stamp(None, None, Some(Suggested), false),
            Some(Suggested)
        );
        assert_eq!(
            origin_class_to_stamp(None, Some(Asked), Some(Suggested), false),
            Some(Asked)
        );
    }

    #[test]
    fn a_recorded_class_is_never_changed() {
        for existing in OriginClass::ALL {
            for requested in OriginClass::ALL {
                assert_eq!(
                    origin_class_to_stamp(Some(existing), Some(requested), None, false),
                    None
                );
            }
        }
    }

    #[test]
    fn inside_an_agent_the_inherited_class_wins_over_any_request() {
        use OriginClass::*;
        // An agent that runs `ta run --origin-class asked` inside an
        // `unasked` goal gets `unasked`; inside no class at all, also `unasked`.
        assert_eq!(
            origin_class_to_stamp(None, Some(Asked), Some(Unasked), true),
            Some(Unasked)
        );
        assert_eq!(
            origin_class_to_stamp(None, Some(Asked), None, true),
            Some(Unasked)
        );
        assert_eq!(
            origin_class_to_stamp(None, Some(Unasked), Some(Suggested), true),
            Some(Suggested)
        );
    }

    /// `origin_class` is accounting data. No module that makes an
    /// authorization, policy, routing or approval decision may read it. This
    /// scans their sources (CRLF-normalized for Windows checkouts).
    #[test]
    fn origin_class_is_never_read_by_a_decision_path() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        // (crate directory, file or directory relative to crates/)
        let decision_paths = [
            "ta-policy/src",
            "ta-sandbox/src",
            "ta-brain/src",
            "ta-credential-broker/src",
            "ta-credentials/src",
            "ta-mediation/src",
            "ta-daemon/src/human_gate.rs",
            "ta-daemon/src/router.rs",
            "ta-daemon/src/assertion.rs",
            "ta-goal/src/security.rs",
            "ta-goal/src/chat_mode.rs",
            "ta-goal/src/tool_surface.rs",
            "ta-mcp-gateway/src/interceptor.rs",
            "ta-mcp-gateway/src/tools/draft.rs",
        ];
        let mut hits = Vec::new();
        for rel in decision_paths {
            let path = root.join(rel);
            assert!(
                path.exists(),
                "decision path {rel} no longer exists; update this list so the scan keeps \
                 covering the module that replaced it"
            );
            scan_for_origin_class(&path, &mut hits);
        }
        assert!(
            hits.is_empty(),
            "origin_class is accounting data only and must not be read by a decision path, \
             but these files mention it: {hits:?}"
        );
    }

    fn scan_for_origin_class(path: &std::path::Path, hits: &mut Vec<String>) {
        if path.is_dir() {
            for e in std::fs::read_dir(path).unwrap().flatten() {
                scan_for_origin_class(&e.path(), hits);
            }
        } else if path.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
            let lower = text.to_ascii_lowercase();
            if lower.contains("origin_class") || lower.contains("originclass") {
                hits.push(path.display().to_string());
            }
        }
    }

    /// Auto-approve refusal is decided by `origin` alone: every origin class
    /// leaves `cos` and `chat` refused and other origins unaffected.
    #[test]
    fn auto_approve_refusal_does_not_depend_on_origin_class() {
        for _class in OriginClass::ALL {
            for blocked in NO_AUTO_APPROVE_ORIGINS {
                assert!(origin_blocks_auto_approve(Some(blocked)));
            }
            assert!(!origin_blocks_auto_approve(Some("cli")));
            assert!(!origin_blocks_auto_approve(None));
        }
    }

    #[test]
    fn project_manager_origin_is_rule_gated_not_blocked() {
        assert!(!origin_blocks_auto_approve(Some(PROJECT_MANAGER_ORIGIN)));
        assert!(!NO_AUTO_APPROVE_ORIGINS.contains(&PROJECT_MANAGER_ORIGIN));
        assert!(origin_is_rule_gated(Some("project-manager")));
        assert!(origin_is_rule_gated(Some("  Project-Manager ")));
        assert!(!origin_is_rule_gated(Some("cos")));
        assert!(!origin_is_rule_gated(Some("chat")));
        assert!(!origin_is_rule_gated(Some("cli")));
        assert!(!origin_is_rule_gated(None));
    }

    fn stamp(role: Option<&str>, req: Option<&str>) -> Result<Option<String>, String> {
        stamp_launch_origin(role, req)
    }

    #[test]
    fn stamp_launch_origin_truth_table() {
        let pm = Some("project-manager".to_string());
        // PM role: stamped, with case/whitespace tolerated on the role.
        for role in ["project-manager", " Project-Manager ", "PROJECT-MANAGER"] {
            assert_eq!(stamp(Some(role), None).unwrap(), pm);
            assert_eq!(stamp(Some(role), Some("project-manager")).unwrap(), pm);
            assert_eq!(stamp(Some(role), Some(" Project-Manager")).unwrap(), pm);
            // More restrictive origin wins.
            assert_eq!(
                stamp(Some(role), Some("cos")).unwrap().as_deref(),
                Some("cos")
            );
            assert_eq!(
                stamp(Some(role), Some("chat")).unwrap().as_deref(),
                Some("chat")
            );
            assert_eq!(
                stamp(Some(role), Some("COS")).unwrap().as_deref(),
                Some("cos")
            );
            // Any other origin conflicts, and the error names both.
            let err = stamp(Some(role), Some("cli")).unwrap_err();
            assert!(
                err.contains("project-manager") && err.contains("`cli`"),
                "{err}"
            );
        }
        // Non-PM roles and no role: forgery refused, everything else unchanged.
        for role in [
            None,
            Some("engineer"),
            Some("cos"),
            Some("  "),
            Some("project-manager-2"),
        ] {
            let err = stamp(role, Some("project-manager")).unwrap_err();
            assert!(err.contains("project-manager origin is stamped by the gateway"));
            assert!(err.contains("remove --origin/TA_GOAL_ORIGIN"));
            assert!(stamp(role, Some("PROJECT-MANAGER")).is_err());
            assert_eq!(stamp(role, None).unwrap(), None);
            for o in ["cos", "chat", "cli"] {
                assert_eq!(stamp(role, Some(o)).unwrap().as_deref(), Some(o));
            }
        }
    }

    #[test]
    fn team_role_from_env_reads_and_trims() {
        std::env::remove_var(TEAM_ROLE_ENV_VAR);
        assert_eq!(team_role_from_env(), None);
        std::env::set_var(TEAM_ROLE_ENV_VAR, "   ");
        assert_eq!(team_role_from_env(), None);
        std::env::set_var(TEAM_ROLE_ENV_VAR, " project-manager ");
        assert_eq!(team_role_from_env().as_deref(), Some("project-manager"));
        std::env::remove_var(TEAM_ROLE_ENV_VAR);
    }
}
