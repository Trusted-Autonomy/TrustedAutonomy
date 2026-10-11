// human_gate.rs: Who may change anything through the daemon (CR-02).
//
// The daemon's loopback bypass trusts any local caller, which includes an agent
// with a shell. So the gate is FAIL CLOSED: every request that is not a read
// (GET, HEAD, OPTIONS) needs proof of a human, unless the route is listed in
// `AGENT_ALLOWED` with a one-line justification (or in
// `SELF_AUTHENTICATING`, whose handler demands a credential or a signed
// assertion itself). A route added tomorrow is therefore protected until
// someone classifies it, and `every_mutating_route_is_classified` fails the
// build if a route is registered without being classified on purpose.
//
// Proof of a human, for a gated route:
//   1. the credential minted by `ta login` (header `X-TA-Human-Credential`);
//   2. a remote (non-loopback) caller presenting a Write/Admin bearer token;
//   3. `[auth] local_bypass_mutating = true` (explicit, unsafe opt-in).
// A `Relay` service-account token is never enough on its own. A signed
// assertion of a named human is accepted only at `POST /api/attention/{id}/act`
// (see `assertion` and `attention`).
//
// HONEST LIMIT: while agents run as the same OS user as the human, the
// credential file is readable by an agent, so this is a speed bump. The OS
// sandbox phases (v0.17.12.6 onward) make it a wall.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::{HeaderMap, Method};

use crate::assertion::{AssertionVerifier, Ed25519RegistryVerifier};
use crate::config::{AuthConfig, ServerConfig};

/// Header carrying the `ta login` credential.
pub const CREDENTIAL_HEADER: &str = ta_credentials::human::CREDENTIAL_HEADER;

// ── Route classification ────────────────────────────────────────────────────

/// What a gated request is trying to do (used to word refusals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedAction {
    Approve,
    Deny,
    Apply,
    /// Answer an agent question (interaction response, goal/workflow input).
    Answer,
    /// Execute a command (`/api/cmd`, `/api/input`).
    Command,
    Shutdown,
    /// Any other state change: start or stop work, settings, personas, teams,
    /// workflows, plan, projects, memory, uploads.
    Change,
}

impl GatedAction {
    fn describe(self) -> &'static str {
        match self {
            GatedAction::Approve => "approve a draft",
            GatedAction::Deny => "deny a draft",
            GatedAction::Apply => "apply a draft",
            GatedAction::Answer => "answer an agent",
            GatedAction::Command => "run a command",
            GatedAction::Shutdown => "stop the daemon",
            GatedAction::Change => "change project or daemon state",
        }
    }
}

/// A gated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedRoute {
    pub action: GatedAction,
    /// The draft, interaction, goal or other id in the path, if any.
    pub item_id: String,
}

/// How a request is treated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteClass {
    /// A read (GET, HEAD, OPTIONS): normal bearer / loopback authentication.
    Safe,
    /// A mutating route agents legitimately call, with the reason it is open.
    AgentAllowed(&'static str),
    /// A mutating route whose handler demands a credential or an assertion.
    SelfAuthenticating(&'static str),
    /// Everything else that mutates: needs proof of a human.
    Gated(GatedRoute),
}

/// Every mutating route that needs proof of a human, named on purpose. The
/// gate is fail closed, so a mutating route missing here is still gated; the
/// classification test insists it is listed anyway so the choice is explicit.
pub const GATED_ROUTES: &[(&str, &str, GatedAction)] = &[
    ("POST", "/api/cmd", GatedAction::Command),
    ("POST", "/api/input", GatedAction::Command),
    ("POST", "/api/shutdown", GatedAction::Shutdown),
    ("POST", "/api/drafts/{id}/approve", GatedAction::Approve),
    ("POST", "/api/drafts/{id}/deny", GatedAction::Deny),
    ("POST", "/api/drafts/{id}/apply", GatedAction::Apply),
    ("POST", "/api/drafts/{id}/dialog", GatedAction::Change),
    (
        "POST",
        "/api/interactions/{id}/respond",
        GatedAction::Answer,
    ),
    ("POST", "/api/goals/{id}/input", GatedAction::Answer),
    ("POST", "/api/workflow/{id}/input", GatedAction::Answer),
    ("POST", "/api/goals/{id}/message", GatedAction::Change),
    ("POST", "/api/goal/start", GatedAction::Change),
    ("POST", "/api/agent/start", GatedAction::Change),
    ("DELETE", "/api/agent/{id}", GatedAction::Change),
    ("POST", "/api/projects", GatedAction::Change),
    ("DELETE", "/api/projects/{name}", GatedAction::Change),
    ("POST", "/api/office/reload", GatedAction::Change),
    ("POST", "/api/project/new", GatedAction::Change),
    ("POST", "/api/project/open", GatedAction::Change),
    ("POST", "/api/project/browse", GatedAction::Change),
    ("POST", "/api/project/init", GatedAction::Change),
    ("POST", "/api/plan/phase/add", GatedAction::Change),
    ("POST", "/api/plan/generate", GatedAction::Change),
    ("POST", "/api/plan/new", GatedAction::Change),
    ("POST", "/api/persona/save", GatedAction::Change),
    ("POST", "/api/workflow/generate", GatedAction::Change),
    ("POST", "/api/workflow/save", GatedAction::Change),
    ("POST", "/api/workflow/{id}/run", GatedAction::Change),
    ("DELETE", "/api/workflow/{id}", GatedAction::Change),
    ("PUT", "/api/settings/{section}", GatedAction::Change),
    ("PUT", "/api/setup/progress", GatedAction::Change),
    ("POST", "/api/settings/agent/validate", GatedAction::Change),
    (
        "POST",
        "/api/settings/notifications/test",
        GatedAction::Change,
    ),
    ("POST", "/api/settings/vcs/check", GatedAction::Change),
    ("POST", "/api/advisor/message", GatedAction::Change),
    ("POST", "/api/advisor/inject", GatedAction::Change),
    ("POST", "/api/advisor/history", GatedAction::Change),
    ("POST", "/api/advisor/dialog", GatedAction::Change),
    ("POST", "/api/context/upload", GatedAction::Change),
    ("POST", "/api/team/assign", GatedAction::Change),
    ("DELETE", "/api/team/{role}", GatedAction::Change),
    ("POST", "/api/memory", GatedAction::Change),
    ("DELETE", "/api/memory/{key}", GatedAction::Change),
];

/// Mutating routes that stay open to agents, each with the reason. Anything
/// not listed here or in `GATED_ROUTES` is gated anyway.
pub const AGENT_ALLOWED: &[(&str, &str, &str)] = &[
    (
        "POST",
        "/api/agent/ask",
        "agent coordination: an agent asks the project's advisor agent a question; the reply is the only effect",
    ),
    (
        "POST",
        "/api/plan/phase/claim",
        "agent coordination: an agent claims a plan phase so two agents do not build the same one",
    ),
    (
        "POST",
        "/api/plan/phase/release",
        "agent coordination: an agent gives a claimed plan phase back",
    ),
    (
        "POST",
        "/api/whiteboard/presence",
        "agent coordination: an agent registers its presence on the shared whiteboard",
    ),
    (
        "POST",
        "/api/whiteboard/handoff/send",
        "agent coordination: an agent hands work to another role",
    ),
    (
        "POST",
        "/api/whiteboard/handoff/receive",
        "agent coordination: an agent picks up a handoff addressed to it",
    ),
    (
        "POST",
        "/api/whiteboard/tasks/claim",
        "agent coordination: an agent claims a whiteboard task",
    ),
    (
        "POST",
        "/api/whiteboard/tasks/complete",
        "agent coordination: an agent marks its claimed task complete",
    ),
    (
        "POST",
        "/api/whiteboard/outcome/send",
        "agent coordination: the chief-of-staff publishes an outcome for the poller to report back",
    ),
    (
        "POST",
        "/api/webhooks/github",
        "webhook: keeps its own HMAC signature authentication and is served outside the auth middleware",
    ),
    (
        "POST",
        "/api/webhooks/vcs",
        "webhook: keeps its own HMAC signature authentication and is served outside the auth middleware",
    ),
];

/// Mutating routes whose handler checks for proof itself.
pub const SELF_AUTHENTICATING: &[(&str, &str, &str)] = &[(
    "POST",
    "/api/attention/{id}/act",
    "accepts the local human credential or a signed human assertion in the body; a relay token alone is refused by the handler",
)];

/// Does `pattern` (segments, `{x}` matches any one non-empty segment) match `path`?
/// Returns the captured value of the first `{x}` segment.
fn match_pattern(pattern: &str, path: &str) -> Option<String> {
    let want: Vec<&str> = pattern.trim_start_matches('/').split('/').collect();
    let got: Vec<&str> = path
        .trim_end_matches('/')
        .trim_start_matches('/')
        .split('/')
        .collect();
    if want.len() != got.len() {
        return None;
    }
    let mut captured = None;
    for (w, g) in want.iter().zip(&got) {
        if w.starts_with('{') && w.ends_with('}') {
            if g.is_empty() {
                return None;
            }
            captured.get_or_insert_with(|| g.to_string());
        } else if w != g {
            return None;
        }
    }
    Some(captured.unwrap_or_default())
}

fn entry_matches(method: &Method, entry_method: &str, pattern: &str, path: &str) -> Option<String> {
    if method.as_str() != entry_method {
        return None;
    }
    match_pattern(pattern, path)
}

/// Classify a request. Fail closed: a mutating request that matches nothing is
/// gated. Gated entries are checked first, so an overlapping allow-list entry
/// can never open a gated route.
pub fn classify(method: &Method, path: &str) -> RouteClass {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return RouteClass::Safe;
    }
    for (m, pattern, action) in GATED_ROUTES {
        if let Some(item) = entry_matches(method, m, pattern, path) {
            return RouteClass::Gated(GatedRoute {
                action: *action,
                item_id: item,
            });
        }
    }
    for (m, pattern, why) in SELF_AUTHENTICATING {
        if entry_matches(method, m, pattern, path).is_some() {
            return RouteClass::SelfAuthenticating(why);
        }
    }
    for (m, pattern, why) in AGENT_ALLOWED {
        if entry_matches(method, m, pattern, path).is_some() {
            return RouteClass::AgentAllowed(why);
        }
    }
    let item = path
        .trim_matches('/')
        .split('/')
        .nth(2)
        .unwrap_or_default()
        .to_string();
    RouteClass::Gated(GatedRoute {
        action: GatedAction::Change,
        item_id: item,
    })
}

/// True when (method, pattern) is named in one of the three explicit lists.
pub fn is_explicitly_classified(method: &str, pattern: &str) -> bool {
    GATED_ROUTES
        .iter()
        .any(|(m, p, _)| *m == method && *p == pattern)
        || AGENT_ALLOWED
            .iter()
            .any(|(m, p, _)| *m == method && *p == pattern)
        || SELF_AUTHENTICATING
            .iter()
            .any(|(m, p, _)| *m == method && *p == pattern)
}

// ── Proof of a human ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    /// The `ta login` credential.
    Credential,
    /// A non-loopback caller with a Write/Admin bearer token.
    RemoteToken,
    /// `[auth] local_bypass_mutating = true` (explicit, unsafe opt-in).
    LocalBypass,
    /// A verified signed assertion (`/api/attention/{id}/act` only).
    Assertion,
}

/// Request extension set on every gated request that passed the gate.
#[derive(Debug, Clone)]
pub struct HumanProof {
    pub via: ProofKind,
    /// The named human, when the proof identifies one (assertions).
    pub user_id: Option<String>,
    pub email: Option<String>,
}

impl HumanProof {
    pub fn anonymous(via: ProofKind) -> Self {
        Self {
            via,
            user_id: None,
            email: None,
        }
    }
}

// ── The gate ────────────────────────────────────────────────────────────────

/// Why a gated request was refused, ready to render.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub error: String,
    pub hint: String,
}

/// What bearer authentication learned about a gated request.
#[derive(Clone, Copy)]
pub struct GatedCaller<'a> {
    pub is_local: bool,
    pub scope: crate::config::TokenScope,
    pub method: &'a Method,
    pub path: &'a str,
}

pub struct HumanGate {
    state_dir: PathBuf,
    verifier: Arc<dyn AssertionVerifier>,
}

impl HumanGate {
    /// The default gate: credential and key registry under `state_dir`.
    pub fn new(state_dir: PathBuf) -> Self {
        let verifier = Arc::new(Ed25519RegistryVerifier::new(state_dir.clone()));
        Self {
            state_dir,
            verifier,
        }
    }

    pub fn with_verifier(state_dir: PathBuf, verifier: Arc<dyn AssertionVerifier>) -> Self {
        Self {
            state_dir,
            verifier,
        }
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn verifier(&self) -> &dyn AssertionVerifier {
        self.verifier.as_ref()
    }

    /// Does the request carry the `ta login` credential?
    pub fn credential_proof(&self, headers: &HeaderMap) -> Option<HumanProof> {
        let secret = headers
            .get(CREDENTIAL_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)?;
        ta_credentials::human::verify_secret(&self.state_dir, secret)
            .then(|| HumanProof::anonymous(ProofKind::Credential))
    }

    /// Proof of a human without an assertion: the credential, a remote
    /// Write/Admin token, or the explicit local opt-in. `what` words the
    /// refusal (for example "POST /api/cmd").
    pub fn decide_without_assertion(
        &self,
        what: &str,
        action: GatedAction,
        headers: &HeaderMap,
        auth: &AuthConfig,
        caller: &GatedCaller<'_>,
    ) -> Result<HumanProof, Refusal> {
        if let Some(proof) = self.credential_proof(headers) {
            return Ok(proof);
        }
        if caller.is_local && auth.local_bypass_mutating {
            return Ok(HumanProof::anonymous(ProofKind::LocalBypass));
        }
        if !caller.is_local && caller.scope.counts_as_human() {
            return Ok(HumanProof::anonymous(ProofKind::RemoteToken));
        }
        let relay = caller.scope == crate::config::TokenScope::Relay;
        let verb = action.describe();
        let error = if relay {
            format!(
                "{what} refused: a relay (service-account) token can read the attention feed and                  relay requests but cannot {verb}. The acting human's signed assertion is required."
            )
        } else {
            format!(
                "{what} refused: trying to {verb} needs proof of a human and the request carries                  none. Loopback callers are no longer trusted for anything that changes state,                  so an agent with a shell cannot do this."
            )
        };
        let hint = if relay {
            "Relay the human's signed assertion with POST /api/attention/{id}/act              (docs/USAGE.md, 'Human-asserted actions')."
                .to_string()
        } else {
            format!(
                "Run `ta login` once in your own terminal, then send the credential in the                  {CREDENTIAL_HEADER} header (the ta CLI and shell do this automatically).                  To restore the old behaviour on a throwaway machine set                  `[auth] local_bypass_mutating = true` in daemon.toml (unsafe: any local process,                  including an agent, could then change state)."
            )
        };
        Err(Refusal { error, hint })
    }

    /// Final decision for a gated request that has already been through bearer
    /// authentication (`caller` carries what that step learned).
    pub fn decide(
        &self,
        route: &GatedRoute,
        headers: &HeaderMap,
        auth: &AuthConfig,
        caller: &GatedCaller<'_>,
    ) -> Result<HumanProof, Refusal> {
        let what = format!("{} {}", caller.method, caller.path);
        self.decide_without_assertion(&what, route.action, headers, auth, caller)
    }
}

// ── Host header allowlist (CR-36) ───────────────────────────────────────────

/// Which `Host` headers the daemon accepts. Defends the loopback daemon against
/// DNS-rebinding: a hostile web page whose name resolves to 127.0.0.1 sends its
/// own name in `Host`, which is refused here.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    enforce: bool,
    extra: Vec<String>,
}

impl HostPolicy {
    pub fn from_server(server: &ServerConfig) -> Self {
        let bind_is_loopback = server
            .bind
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(server.bind == "localhost");
        Self {
            // A daemon deliberately bound to a LAN address with no allowlist
            // keeps working; rebinding is a loopback problem. Setting
            // `allowed_hosts` turns enforcement on everywhere.
            enforce: bind_is_loopback || !server.allowed_hosts.is_empty(),
            extra: server
                .allowed_hosts
                .iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn loopback_default() -> Self {
        Self {
            enforce: true,
            extra: Vec::new(),
        }
    }

    /// `Ok` when `host_header` (value of `Host`, if any) is acceptable.
    pub fn check(&self, host_header: Option<&str>) -> Result<(), Refusal> {
        if !self.enforce {
            return Ok(());
        }
        // No Host header: not a browser, nothing to rebind.
        let Some(raw) = host_header else {
            return Ok(());
        };
        let host = host_only(raw);
        let ok = host == "localhost"
            || host.ends_with(".localhost")
            || host.parse::<std::net::IpAddr>().is_ok()
            || self.extra.contains(&host);
        if ok {
            Ok(())
        } else {
            Err(Refusal {
                error: format!(
                    "request refused: Host header '{raw}' is not an allowed daemon host."
                ),
                hint: format!(
                    "Reach the daemon as localhost or an IP address, or add '{host}' to \
                     `[server] allowed_hosts` in daemon.toml if you reach it through that name. \
                     (This blocks DNS-rebinding attacks from web pages.)"
                ),
            })
        }
    }
}

/// Lower-cased host part of a `Host` header value (port and IPv6 brackets removed).
fn host_only(raw: &str) -> String {
    let raw = raw.trim().to_ascii_lowercase();
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("").to_string();
    }
    raw.rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
        .map(|(h, _)| h.to_string())
        .unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_gated(m: Method, p: &str) -> Option<GatedAction> {
        match classify(&m, p) {
            RouteClass::Gated(g) => Some(g.action),
            _ => None,
        }
    }

    #[test]
    fn reads_are_safe_and_everything_else_is_gated_unless_listed() {
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(classify(&m, "/api/drafts/abc/approve"), RouteClass::Safe);
        }
        assert_eq!(
            is_gated(Method::POST, "/api/cmd"),
            Some(GatedAction::Command)
        );
        let a = match classify(&Method::POST, "/api/drafts/abc/approve") {
            RouteClass::Gated(g) => g,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            (a.action, a.item_id.as_str()),
            (GatedAction::Approve, "abc")
        );
        assert_eq!(
            is_gated(Method::POST, "/api/drafts/abc/apply/"),
            Some(GatedAction::Apply)
        );
        assert_eq!(
            is_gated(Method::POST, "/api/interactions/i1/respond"),
            Some(GatedAction::Answer)
        );
        // The previously unprotected mutating routes.
        for (m, p) in [
            (Method::POST, "/api/persona/save"),
            (Method::POST, "/api/team/assign"),
            (Method::POST, "/api/workflow/save"),
            (Method::POST, "/api/workflow/w1/run"),
            (Method::POST, "/api/plan/phase/add"),
            (Method::POST, "/api/advisor/inject"),
            (Method::POST, "/api/goal/start"),
            (Method::POST, "/api/agent/start"),
            (Method::PUT, "/api/settings/auth"),
            (Method::PUT, "/api/setup/progress"),
            (Method::DELETE, "/api/agent/a1"),
            (Method::DELETE, "/api/team/reviewer"),
            (Method::DELETE, "/api/memory/k"),
            (Method::POST, "/api/project/open"),
            (Method::POST, "/api/office/reload"),
        ] {
            assert!(is_gated(m.clone(), p).is_some(), "{m} {p}");
        }
    }

    #[test]
    fn an_unlisted_or_future_mutating_route_is_gated_by_default() {
        for (m, p) in [
            (Method::POST, "/api/brand-new-thing"),
            (Method::PUT, "/api/brand-new-thing/1"),
            (Method::PATCH, "/api/anything"),
            (Method::DELETE, "/api/whatever/x"),
            (Method::POST, "/"),
            (Method::POST, "//api//cmd"),
        ] {
            assert!(is_gated(m.clone(), p).is_some(), "{m} {p}");
        }
    }

    #[test]
    fn agent_allowed_routes_stay_open_each_with_a_reason() {
        for (m, p, why) in AGENT_ALLOWED {
            assert!(!why.trim().is_empty(), "{m} {p} needs a justification");
            assert!(why.len() > 20, "{m} {p}: justification too thin: {why}");
            let concrete = p.replace("{id}", "x1");
            assert!(
                matches!(
                    classify(&m.parse().unwrap(), &concrete),
                    RouteClass::AgentAllowed(_)
                ),
                "{m} {p}"
            );
        }
        assert!(matches!(
            classify(&Method::POST, "/api/whiteboard/handoff/send"),
            RouteClass::AgentAllowed(_)
        ));
        assert!(matches!(
            classify(&Method::POST, "/api/plan/phase/claim"),
            RouteClass::AgentAllowed(_)
        ));
    }

    #[test]
    fn gated_entries_win_over_allow_entries_and_lists_do_not_overlap() {
        for (m, p, _) in AGENT_ALLOWED.iter().chain(SELF_AUTHENTICATING.iter()) {
            let concrete = p.replace("{id}", "x1");
            let hit = GATED_ROUTES
                .iter()
                .any(|(gm, gp, _)| gm == m && match_pattern(gp, &concrete).is_some());
            assert!(!hit, "{m} {p} is both gated and allowed");
        }
    }

    #[test]
    fn the_act_endpoint_is_self_authenticating() {
        assert!(matches!(
            classify(&Method::POST, "/api/attention/att_draft_1/act"),
            RouteClass::SelfAuthenticating(_)
        ));
        // The feed itself is a read.
        assert_eq!(classify(&Method::GET, "/api/attention"), RouteClass::Safe);
    }

    #[test]
    fn every_gated_route_classifies_as_gated() {
        for (m, p, action) in GATED_ROUTES {
            let concrete = p
                .replace("{id}", "x1")
                .replace("{name}", "n1")
                .replace("{section}", "s1");
            let concrete = concrete.replace("{role}", "r1").replace("{key}", "k1");
            match classify(&m.parse().unwrap(), &concrete) {
                RouteClass::Gated(g) => assert_eq!(g.action, *action, "{m} {p}"),
                other => panic!("{m} {p} classified as {other:?}"),
            }
        }
    }

    #[test]
    fn host_policy_blocks_rebinding_names_and_allows_local_forms() {
        let p = HostPolicy::loopback_default();
        for ok in [
            "localhost:7700",
            "127.0.0.1:7700",
            "[::1]:7700",
            "LOCALHOST",
            "ta.localhost:7700",
            "192.168.1.5:7700",
        ] {
            assert!(p.check(Some(ok)).is_ok(), "{ok}");
        }
        assert!(p.check(None).is_ok());
        let err = p.check(Some("evil.example.com:7700")).unwrap_err();
        assert!(err.hint.contains("allowed_hosts"));
        let server = ServerConfig {
            allowed_hosts: vec!["ta.example.com".into()],
            ..Default::default()
        };
        let p = HostPolicy::from_server(&server);
        assert!(p.check(Some("ta.example.com:443")).is_ok());
        assert!(p.check(Some("evil.example.com")).is_err());
    }

    #[test]
    fn host_policy_is_off_for_a_lan_bind_without_an_allowlist() {
        let server = ServerConfig {
            bind: "0.0.0.0".into(),
            ..Default::default()
        };
        let p = HostPolicy::from_server(&server);
        assert!(p.check(Some("ta.example.com")).is_ok());
    }

    // ── Source-scanning classification test ────────────────────────────────

    /// Every `.route(` registration in `source` as (path, methods).
    fn scan_routes(source: &str) -> Vec<(String, Vec<String>)> {
        let src = source.replace("\r\n", "\n");
        let bytes = src.as_bytes();
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(rel) = src[from..].find(".route(") {
            let start = from + rel + ".route(".len();
            from = start;
            // Take the balanced argument list.
            let mut depth = 1usize;
            let mut i = start;
            let mut in_str = false;
            while i < bytes.len() && depth > 0 {
                let c = bytes[i] as char;
                if in_str {
                    if c == '\\' {
                        i += 1;
                    } else if c == '"' {
                        in_str = false;
                    }
                } else {
                    match c {
                        '"' => in_str = true,
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                }
                i += 1;
            }
            let args = &src[start..i.saturating_sub(1)];
            let Some(q1) = args.find('"') else { continue };
            let Some(q2) = args[q1 + 1..].find('"') else {
                continue;
            };
            let path = args[q1 + 1..q1 + 1 + q2].to_string();
            if !path.starts_with('/') {
                continue;
            }
            let rest = &args[q1 + q2 + 2..];
            let mut methods = Vec::new();
            for (name, label) in [
                ("post", "POST"),
                ("put", "PUT"),
                ("delete", "DELETE"),
                ("patch", "PATCH"),
            ] {
                let needle = format!("{name}(");
                let mut at = 0;
                while let Some(pos) = rest[at..].find(&needle) {
                    let abs = at + pos;
                    let prev = rest[..abs].chars().last();
                    if prev.is_none_or(|p| !(p.is_alphanumeric() || p == '_')) {
                        methods.push(label.to_string());
                        break;
                    }
                    at = abs + needle.len();
                }
            }
            out.push((path, methods));
        }
        out
    }

    fn mutating_routes_in(file: &str) -> Vec<(String, String)> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        // Only the non-test part of the file registers real routes.
        let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
        scan_routes(production)
            .into_iter()
            .flat_map(|(p, ms)| ms.into_iter().map(move |m| (m, p.clone())))
            .collect()
    }

    #[test]
    fn the_route_scanner_finds_chained_and_multiline_registrations() {
        let src = r#"
            .route("/a", get(x))
            .route("/b", post(y))
            .route(
                "/c/{id}",
                get(z).put(w).delete(v),
            )
            .route("/d", get(delete_thing))
            .route("/e", get(x).post(y))
        "#;
        let got = scan_routes(src);
        assert_eq!(got[0], ("/a".into(), vec![]));
        assert_eq!(got[1], ("/b".into(), vec!["POST".into()]));
        assert_eq!(
            got[2],
            ("/c/{id}".into(), vec!["PUT".into(), "DELETE".into()])
        );
        assert_eq!(got[3], ("/d".into(), vec![]), "delete_thing is not delete(");
        assert_eq!(got[4], ("/e".into(), vec!["POST".into()]));
    }

    #[test]
    fn every_mutating_route_is_classified() {
        let mut found = Vec::new();
        for file in ["api/mod.rs", "web.rs"] {
            found.extend(mutating_routes_in(file));
        }
        assert!(
            found.len() >= 40,
            "the scanner found only {} mutating routes; it has probably stopped working",
            found.len()
        );
        let unclassified: Vec<String> = found
            .iter()
            .filter(|(m, p)| !is_explicitly_classified(m, p))
            .map(|(m, p)| format!("{m} {p}"))
            .collect();
        assert!(
            unclassified.is_empty(),
            "these mutating routes are neither in GATED_ROUTES nor AGENT_ALLOWED \
             (nor SELF_AUTHENTICATING) in crates/ta-daemon/src/human_gate.rs: {unclassified:?}. \
             Add each to GATED_ROUTES, or to AGENT_ALLOWED with a one-line justification."
        );
        // And nothing listed is stale: every listed route is really registered.
        for (m, p, _) in GATED_ROUTES {
            assert!(
                found.iter().any(|(fm, fp)| fm == m && fp == p),
                "GATED_ROUTES lists {m} {p} but no such route is registered"
            );
        }
        for (m, p, _) in AGENT_ALLOWED {
            // Webhooks are registered in api/mod.rs too.
            assert!(
                found.iter().any(|(fm, fp)| fm == m && fp == p),
                "AGENT_ALLOWED lists {m} {p} but no such route is registered"
            );
        }
    }

    fn usage_text() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/USAGE.md");
        // Normalize CRLF so the check is the same on Windows checkouts.
        std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
    }

    #[test]
    fn the_usage_guide_lists_every_route_from_the_consts() {
        let text = usage_text();
        for (method, route, _) in GATED_ROUTES {
            let needle = format!("`{method} {route}`");
            assert!(
                text.contains(&needle),
                "docs/USAGE.md must list the gated route {needle} (Human Credential section)"
            );
        }
        for (method, route, _) in AGENT_ALLOWED.iter().chain(SELF_AUTHENTICATING.iter()) {
            let needle = format!("`{method} {route}`");
            assert!(
                text.contains(&needle),
                "docs/USAGE.md must list the open route {needle} (Human Credential section)"
            );
        }
    }
}
