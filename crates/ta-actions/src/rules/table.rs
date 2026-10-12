// rules/table.rs: the compiled table and the single `decide()` (v0.17.12.4).
//
// Precedence, printed by `ta rules show` for every cell:
//   org floor, then legacy deny rules, then a preset that can only tighten,
//   then the owner's rules, then the shipped posture.
//
// How a request is decided:
//   1. Hard invariants no rule can change (a path that is unsafe, or protected
//      for an unattended apply).
//   2. The base answer: the owner's matching rules if any, else the posture's,
//      else the default (unattended: deny, attended: ask). Inside one source
//      the strictest match wins (deny beats ask beats allow), so there are no
//      ordering tricks.
//   3. The org floor, legacy deny rules and the role's preset can only make
//      the answer stricter (`max`), never looser.
//   4. `ask` with nobody present is `deny`.
//
// No model is consulted. The table is data; `cells()` enumerates it.

use std::collections::BTreeMap;

use serde_json::Value;

use super::model::{
    DocKind, Effect, Environment, Layer, Rule, RulesDoc, Target, When, FORMAT_VERSION,
};
use super::paths::{self, NormPath, PathProblem};
use super::postures::PostureData;

/// Where an external action lands, as the caller knows it. Both parts come
/// from the connection the action really uses, never from the goal's own say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetRef {
    /// The name the target goes by in the rules (`main-db`).
    pub label: Option<String>,
    /// The real connection identity (host and database, endpoint, account).
    pub connection: Option<String>,
}

impl TargetRef {
    /// Read `target` and `connection` from an action's parameters. A payload
    /// without them has no known environment, which is production.
    pub fn from_params(params: &Value) -> TargetRef {
        let pick = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        TargetRef {
            label: pick("target"),
            connection: pick("connection"),
        }
    }
}

/// One question for `decide()`.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// `email`, `db_query`, `social_post`, `apply`, or any connector verb.
    pub action: &'a str,
    pub target: TargetRef,
    pub when: When,
    /// For `apply`: the changed path, project relative.
    pub path: Option<&'a str>,
}

impl<'a> Request<'a> {
    pub fn new(action: &'a str, when: When) -> Self {
        Request {
            action,
            target: TargetRef::default(),
            when,
            path: None,
        }
    }
    pub fn with_target(mut self, target: TargetRef) -> Self {
        self.target = target;
        self
    }
    pub fn with_path(mut self, path: &'a str) -> Self {
        self.path = Some(path);
        self
    }
}

/// The answer, with the rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub effect: Effect,
    pub layer: Layer,
    /// Stable name of the deciding rule (`owner rules [[allow]] #2`).
    pub rule: String,
    pub reason: String,
    /// The one-line fix, when there is one.
    pub fix: Option<String>,
    /// The environment the target resolved to (external actions).
    pub environment: Option<Environment>,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        self.effect == Effect::Allow
    }
    pub fn is_deny(&self) -> bool {
        self.effect == Effect::Deny
    }
    pub fn is_ask(&self) -> bool {
        self.effect == Effect::Ask
    }

    /// One line for a refusal: what was refused, by which rule, and the fix.
    pub fn refusal(&self, action: &str, when: When) -> String {
        let mut out = format!(
            "Blocked: {} {} is not allowed by your rules ({}).",
            when.as_str(),
            action,
            self.rule
        );
        if !self.reason.is_empty() {
            out.push(' ');
            out.push_str(&self.reason);
        }
        if let Some(fix) = &self.fix {
            out.push(' ');
            out.push_str(fix);
        }
        out
    }
}

/// Everything `compile` reads.
#[derive(Debug, Clone)]
pub struct Sources {
    pub floor: Option<RulesDoc>,
    pub owner: Option<RulesDoc>,
    /// Presets by role name (only the selected one is consulted).
    pub presets: BTreeMap<String, RulesDoc>,
    /// Deny rules read from the old `.ta/constitution.toml` and `workflow.toml`.
    pub legacy: Vec<Rule>,
    pub posture: PostureData,
}

#[derive(Debug, Clone)]
pub struct CompiledRules {
    pub posture: String,
    pub posture_revision: u32,
    pub posture_digest: String,
    pub role: Option<String>,
    pub preset_found: bool,
    pub owner_present: bool,
    pub org_floor_present: bool,
    /// Saga order for a draft's outside actions (the posture's setting).
    pub ordered_actions: bool,
    floor: Vec<Rule>,
    legacy: Vec<Rule>,
    preset: Vec<Rule>,
    owner: Vec<Rule>,
    posture_rules: Vec<Rule>,
    by_connection: BTreeMap<String, Environment>,
    by_label: BTreeMap<String, Option<String>>,
    prompt_sources: Vec<Vec<String>>,
    pub targets: Vec<Target>,
}

/// Does `preset_allow` grant nothing the owner has not already granted?
fn covered_by(owner: &[Rule], p: &Rule) -> bool {
    owner.iter().any(|o| {
        o.effect == Effect::Allow
            && o.action == p.action
            && match (&o.envs, &p.envs) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(oe), Some(pe)) => pe.is_subset(oe),
            }
            && match (o.when, p.when) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            }
            && if o.paths.is_empty() {
                true
            } else {
                !p.paths.is_empty()
                    && p.paths
                        .iter()
                        .all(|pp| o.paths.iter().any(|op| pp.starts_with(op)))
            }
    })
}

/// Build the table. Every problem is returned at once; any problem means no
/// table (the caller fails closed).
pub fn compile(src: &Sources, role: Option<&str>) -> Result<CompiledRules, Vec<String>> {
    let mut errors = Vec::new();
    let mut targets: Vec<Target> = Vec::new();
    let mut by_connection: BTreeMap<String, Environment> = BTreeMap::new();
    let mut by_label: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut owner_seen: BTreeMap<String, (Environment, String)> = BTreeMap::new();

    for (kind, doc) in [(DocKind::Floor, &src.floor), (DocKind::Owner, &src.owner)] {
        let Some(doc) = doc else { continue };
        for t in &doc.targets {
            targets.push(t.clone());
            if let Some(conn) = &t.connection {
                if kind == DocKind::Owner {
                    if let Some((prev, prev_name)) = owner_seen.get(conn) {
                        if *prev != t.environment {
                            errors.push(format!(
                                "owner rules: connection \"{conn}\" is declared as {} by \
                                 [targets.{prev_name}] and as {} by [targets.{}]. One connection \
                                 is one environment. Fix: delete one of them.",
                                prev.as_str(),
                                t.environment.as_str(),
                                t.name
                            ));
                        }
                    } else {
                        owner_seen.insert(conn.clone(), (t.environment, t.name.clone()));
                    }
                }
                // The stricter declaration wins when floor and owner disagree.
                let entry = by_connection.entry(conn.clone()).or_insert(t.environment);
                if t.environment.strictness() > entry.strictness() {
                    *entry = t.environment;
                }
            }
            if kind == DocKind::Owner || !by_label.contains_key(&t.name) {
                by_label.insert(t.name.clone(), t.connection.clone());
            }
        }
    }

    let owner_rules = src
        .owner
        .as_ref()
        .map(|d| d.rules.clone())
        .unwrap_or_default();
    let mut preset_rules = Vec::new();
    let mut preset_found = false;
    if let Some(role) = role {
        if let Some(doc) = src.presets.get(role) {
            preset_found = true;
            for r in &doc.rules {
                if r.effect == Effect::Allow && !covered_by(&owner_rules, r) {
                    errors.push(format!(
                        "{}: allows something your own rules do not allow. A preset can only \
                         tighten. Fix: change it to [[ask]] or [[deny]], or adopt it into your \
                         rules file with `ta rules sign` after adding the allow there.",
                        r.label
                    ));
                }
            }
            preset_rules = doc.rules.clone();
        }
    }

    let mut prompt_sources = Vec::new();
    for doc in [&src.floor, &src.owner].into_iter().flatten() {
        prompt_sources.extend(doc.prompt_sources.iter().cloned());
    }

    if errors.is_empty() {
        Ok(CompiledRules {
            posture: src.posture.name.clone(),
            posture_revision: src.posture.revision,
            posture_digest: src.posture.digest.clone(),
            ordered_actions: src.posture.doc.ordered_actions,
            role: role.map(str::to_string),
            preset_found,
            owner_present: src.owner.is_some(),
            org_floor_present: src.floor.is_some(),
            floor: src
                .floor
                .as_ref()
                .map(|d| d.rules.clone())
                .unwrap_or_default(),
            legacy: src.legacy.clone(),
            preset: preset_rules,
            owner: owner_rules,
            posture_rules: src.posture.doc.rules.clone(),
            by_connection,
            by_label,
            prompt_sources,
            targets,
        })
    } else {
        Err(errors)
    }
}

fn matches(
    rule: &Rule,
    action: &str,
    env: Environment,
    when: When,
    path: Option<&NormPath>,
) -> bool {
    (rule.action == "*" || rule.action == action)
        && rule.envs.as_ref().is_none_or(|s| s.contains(&env))
        && rule.when.is_none_or(|w| w == when)
        && (rule.paths.is_empty()
            || path.is_some_and(|p| rule.paths.iter().any(|pre| p.components.starts_with(pre))))
}

/// The strictest matching rule; ties go to the most specific, then the first.
fn pick<'a>(
    rules: &'a [Rule],
    action: &str,
    env: Environment,
    when: When,
    path: Option<&NormPath>,
) -> Option<&'a Rule> {
    let mut best: Option<&Rule> = None;
    for r in rules.iter().filter(|r| matches(r, action, env, when, path)) {
        best = match best {
            None => Some(r),
            Some(b) => {
                if (r.effect, r.specificity()) > (b.effect, b.specificity()) {
                    Some(r)
                } else {
                    Some(b)
                }
            }
        };
    }
    best
}

/// Fold a path for matching without the safety checks (attended applies of
/// places that are protected only for unattended runs).
fn fold_only(raw: &str) -> NormPath {
    NormPath {
        components: raw
            .split(['/', '\\'])
            .filter(|c| !c.is_empty() && *c != ".")
            .map(ta_workspace::path_safety::fold_component)
            .collect(),
    }
}

impl CompiledRules {
    /// Resolve a target's environment. Production unless the real connection
    /// identity is declared (and, if a label is given, declared for that same
    /// connection). A label alone never lowers anything.
    pub fn environment_of(&self, target: &TargetRef) -> (Environment, String) {
        let Some(conn) = target
            .connection
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return (
                Environment::Production,
                "no connection identity was given, so the target counts as production".into(),
            );
        };
        let Some(env) = self.by_connection.get(conn) else {
            return (
                Environment::Production,
                format!("connection \"{conn}\" is not declared in the rules, so it counts as production"),
            );
        };
        if let Some(label) = target.label.as_deref() {
            if let Some(Some(declared)) = self.by_label.get(label) {
                if declared != conn {
                    return (
                        Environment::Production,
                        format!(
                            "target \"{label}\" is declared for a different connection than the \
                             one this action uses, so it counts as production"
                        ),
                    );
                }
            }
        }
        (
            *env,
            format!("connection \"{conn}\" is declared {}", env.as_str()),
        )
    }

    fn hard_deny(&self, problem: &PathProblem, path: &str, when: When) -> Decision {
        Decision {
            effect: Effect::Deny,
            layer: Layer::BuiltIn,
            rule: "built-in: protected path".into(),
            reason: format!("'{path}': {}.", problem.describe()),
            fix: Some(match when {
                When::Unattended => "No rule can change this. A person can apply the change with \
                     `ta draft apply` from their own terminal."
                    .into(),
                When::Attended => {
                    "Rule, posture, target and persona files are changed with `ta rules`, never \
                     through a draft."
                        .into()
                }
            }),
            environment: None,
        }
    }

    /// The single decision function. Every path that can cause an effect asks
    /// this; none keeps a rule of its own.
    pub fn decide(&self, req: &Request<'_>) -> Decision {
        let resolved = (!req.action.trim().eq_ignore_ascii_case("apply"))
            .then(|| self.environment_of(&req.target));
        self.decide_resolved(req, resolved)
    }

    /// Decide as if the target had already resolved to `env`. `ta rules show`
    /// and `effective` use it to print the cell for each environment.
    pub fn decide_for_environment(&self, req: &Request<'_>, env: Environment) -> Decision {
        self.decide_resolved(req, Some((env, format!("{} environment", env.as_str()))))
    }

    fn decide_resolved(
        &self,
        req: &Request<'_>,
        resolved: Option<(Environment, String)>,
    ) -> Decision {
        let action = req.action.trim().to_ascii_lowercase();
        let is_apply = action == "apply";

        let mut norm: Option<NormPath> = None;
        if is_apply {
            if let Some(raw) = req.path {
                match paths::normalize(raw, &self.prompt_sources) {
                    Ok(n) => norm = Some(n),
                    Err(problem) => {
                        if problem.is_unsafe() || req.when == When::Unattended {
                            return self.hard_deny(&problem, raw, req.when);
                        }
                        norm = Some(fold_only(raw));
                    }
                }
            }
        }

        let (env, env_why) = resolved.unwrap_or((Environment::Production, String::new()));
        let env_known = (!is_apply).then_some(env);

        let when = req.when;
        let p = norm.as_ref();
        let owner = pick(&self.owner, &action, env, when, p);
        let base_rule = owner.or_else(|| pick(&self.posture_rules, &action, env, when, p));
        let default_effect = match when {
            When::Unattended => Effect::Deny,
            When::Attended => Effect::Ask,
        };

        // Candidates: (effect, precedence, rule). Lower precedence number wins ties.
        let mut best: (Effect, u8, Option<&Rule>) = match base_rule {
            Some(r) => (r.effect, 4, Some(r)),
            None => (default_effect, 5, None),
        };
        let tighteners = [
            (0u8, pick(&self.floor, &action, env, when, p)),
            (1u8, pick(&self.legacy, &action, env, when, p)),
            (2u8, pick(&self.preset, &action, env, when, p)),
        ];
        for (prec, rule) in tighteners {
            if let Some(r) = rule {
                if (r.effect, std::cmp::Reverse(prec)) > (best.0, std::cmp::Reverse(best.1)) {
                    best = (r.effect, prec, Some(r));
                }
            }
        }

        let (mut effect, _, rule) = best;
        let (layer, rule_name, rule_reason) = match rule {
            Some(r) => (r.layer, r.label.clone(), r.reason.clone()),
            None => (
                Layer::BuiltIn,
                "built-in default (no rule matches)".to_string(),
                None,
            ),
        };

        let mut reason = rule_reason.unwrap_or_default();
        if rule.is_none() {
            reason = match when {
                When::Unattended => format!("Nothing in your rules allows unattended {action}."),
                When::Attended => {
                    format!("Your rules do not mention {action}; a person must confirm it.")
                }
            };
        }
        let target_matters =
            req.target != TargetRef::default() || rule.is_some_and(|r| r.envs.is_some());
        if !is_apply && target_matters {
            if !reason.is_empty() {
                reason.push(' ');
            }
            reason.push_str(&format!(
                "(target environment: {}; {}.)",
                env.as_str(),
                env_why
            ));
        }
        if when == When::Unattended && effect == Effect::Ask {
            effect = Effect::Deny;
            if !reason.is_empty() {
                reason.push(' ');
            }
            reason.push_str("Your rules say a person must confirm this, and nobody is present.");
        }
        let fix = (effect == Effect::Deny).then(|| self.fix_hint(&action, env, when, is_apply));
        Decision {
            effect,
            layer,
            rule: rule_name,
            reason,
            fix,
            environment: env_known,
        }
    }

    fn fix_hint(&self, action: &str, env: Environment, when: When, is_apply: bool) -> String {
        let floor_note = if self.org_floor_present {
            " If the organization floor is the rule that blocks it, only your administrator can change that."
        } else {
            ""
        };
        if is_apply {
            return format!(
                "To allow it for specific folders, add [[allow]] action = \"apply\" paths = \
                 [\"docs/notes/**\"] when = \"{}\" to .ta/rules.toml, then run `ta rules sign`.{floor_note}",
                when.as_str()
            );
        }
        let env_part = if env == Environment::Production {
            String::new()
        } else {
            format!(" environment = [\"{}\"]", env.as_str())
        };
        format!(
            "To allow it, add [[allow]] action = \"{action}\"{env_part} when = \"{}\" to \
             .ta/rules.toml, then run `ta rules sign`.{floor_note}",
            when.as_str()
        )
    }

    /// Actions the table talks about: the built-in set plus every named action.
    pub fn actions(&self) -> Vec<String> {
        let mut set: std::collections::BTreeSet<String> = ["email", "social_post", "db_query"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for r in self.all_rules() {
            if r.action != "*" && r.action != "apply" {
                set.insert(r.action.clone());
            }
        }
        set.into_iter().collect()
    }

    pub fn all_rules(&self) -> impl Iterator<Item = &Rule> {
        self.floor
            .iter()
            .chain(&self.legacy)
            .chain(&self.preset)
            .chain(&self.owner)
            .chain(&self.posture_rules)
    }

    /// Rules that mention file paths, for display.
    pub fn apply_rules(&self) -> Vec<&Rule> {
        self.all_rules()
            .filter(|r| r.action == "apply" && !r.paths.is_empty())
            .collect()
    }

    pub fn format_version(&self) -> u32 {
        FORMAT_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::postures;

    pub(crate) fn doc(text: &str, kind: DocKind, name: &str) -> RulesDoc {
        RulesDoc::parse(text, kind, name).unwrap_or_else(|e| panic!("{e:?}"))
    }

    pub(crate) fn sources(owner: Option<&str>, posture: &str) -> Sources {
        Sources {
            floor: None,
            owner: owner.map(|t| doc(t, DocKind::Owner, "owner rules")),
            presets: BTreeMap::new(),
            legacy: vec![],
            posture: postures::load(posture).unwrap(),
        }
    }

    pub(crate) fn table(owner: Option<&str>, posture: &str) -> CompiledRules {
        compile(&sources(owner, posture), None).unwrap()
    }

    fn effect(t: &CompiledRules, action: &str, when: When) -> Effect {
        t.decide(&Request::new(action, when)).effect
    }

    #[test]
    fn default_is_deny_unattended_ask_attended_and_unknown_is_production() {
        let t = table(None, "strict");
        assert_eq!(effect(&t, "mystery", When::Unattended), Effect::Deny);
        assert_eq!(effect(&t, "mystery", When::Attended), Effect::Ask);
        let d = t.decide(&Request::new("db_query", When::Unattended));
        assert_eq!(d.environment, Some(Environment::Production));
        assert!(d.is_deny());
    }

    #[test]
    fn strict_posture_matches_the_design_table() {
        let t = table(None, "strict");
        assert!(t.decide(&Request::new("email", When::Unattended)).is_deny());
        assert!(t.decide(&Request::new("email", When::Attended)).is_ask());
        assert!(t
            .decide(&Request::new("social_post", When::Unattended))
            .is_deny());
        assert!(t.decide(&Request::new("apply", When::Attended)).is_allow());
        // Unattended apply without an owner path rule is denied.
        let d = t.decide(&Request::new("apply", When::Unattended).with_path("docs/a.md"));
        assert!(d.is_deny(), "{d:?}");
    }

    #[test]
    fn balanced_allows_dev_and_test_databases_but_not_production() {
        let owner = "[targets.scratch]\nenvironment = \"dev\"\nconnection = \"pg://dev/scratch\"\n";
        let t = table(Some(owner), "balanced");
        let dev = TargetRef {
            label: Some("scratch".into()),
            connection: Some("pg://dev/scratch".into()),
        };
        assert!(t
            .decide(&Request::new("db_query", When::Unattended).with_target(dev))
            .is_allow());
        assert!(t
            .decide(&Request::new("db_query", When::Unattended))
            .is_deny());
    }

    #[test]
    fn deny_beats_allow_inside_one_source_in_any_order() {
        for owner in [
            "[[allow]]\naction=\"db_query\"\n[[deny]]\naction=\"db_query\"\n",
            "[[deny]]\naction=\"db_query\"\n[[allow]]\naction=\"db_query\"\n",
            "[[allow]]\naction=\"db_query\"\n[[ask]]\naction=\"db_query\"\n",
        ] {
            let t = table(Some(owner), "open");
            assert_ne!(
                effect(&t, "db_query", When::Attended),
                Effect::Allow,
                "{owner}"
            );
        }
    }

    #[test]
    fn owner_overrides_the_posture_in_both_directions() {
        let owner = "[[allow]]\naction=\"email\"\nwhen=\"unattended\"\n";
        let t = table(Some(owner), "strict");
        assert!(t
            .decide(&Request::new("email", When::Unattended))
            .is_allow());
        let owner = "[[deny]]\naction=\"apply\"\nwhen=\"attended\"\n";
        let t = table(Some(owner), "open");
        assert!(t.decide(&Request::new("apply", When::Attended)).is_deny());
    }

    #[test]
    fn ask_with_nobody_present_becomes_deny() {
        let owner = "[[ask]]\naction=\"email\"\n";
        let t = table(Some(owner), "open");
        let d = t.decide(&Request::new("email", When::Unattended));
        assert!(d.is_deny());
        assert!(d.reason.contains("nobody is present"), "{}", d.reason);
        assert!(t.decide(&Request::new("email", When::Attended)).is_ask());
    }

    #[test]
    fn org_floor_cannot_be_loosened_by_the_owner_file() {
        let mut src = sources(
            Some("[[allow]]\naction=\"email\"\n[[allow]]\naction=\"db_query\"\n"),
            "open",
        );
        src.floor = Some(doc(
            "[[deny]]\naction=\"email\"\nreason=\"Company policy.\"\n",
            DocKind::Floor,
            "org floor",
        ));
        let t = compile(&src, None).unwrap();
        let d = t.decide(&Request::new("email", When::Attended));
        assert!(d.is_deny());
        assert_eq!(d.layer, Layer::OrgFloor);
        assert!(d.reason.contains("Company policy."));
        assert!(d.fix.as_ref().unwrap().contains("administrator"));
    }

    #[test]
    fn preset_tightens_but_cannot_loosen() {
        let mut src = sources(Some("[[allow]]\naction=\"db_query\"\n"), "strict");
        src.presets.insert(
            "researcher".into(),
            doc(
                "[[deny]]\naction=\"db_query\"\n",
                DocKind::Preset,
                "preset researcher",
            ),
        );
        let base = compile(&src, None).unwrap();
        assert!(base
            .decide(&Request::new("db_query", When::Unattended))
            .is_allow());
        let role = compile(&src, Some("researcher")).unwrap();
        assert!(role.preset_found);
        let d = role.decide(&Request::new("db_query", When::Unattended));
        assert!(d.is_deny());
        assert_eq!(d.layer, Layer::Preset);

        // An allow the owner does not already grant is a compile error.
        src.presets.insert(
            "bad".into(),
            doc(
                "[[allow]]\naction=\"email\"\n",
                DocKind::Preset,
                "preset bad",
            ),
        );
        let e = compile(&src, Some("bad")).unwrap_err();
        assert!(e[0].contains("can only tighten"), "{e:?}");

        // An allow the owner already grants (here: a narrower path) is fine and loosens nothing.
        let mut src = sources(
            Some("[[allow]]\naction=\"apply\"\nwhen=\"unattended\"\npaths=[\"docs/**\"]\n"),
            "strict",
        );
        src.presets.insert(
            "r".into(),
            doc(
                "[[allow]]\naction=\"apply\"\nwhen=\"unattended\"\npaths=[\"docs/research/**\"]\n",
                DocKind::Preset,
                "preset r",
            ),
        );
        assert!(compile(&src, Some("r")).is_ok());
        src.presets.insert(
            "wide".into(),
            doc(
                "[[allow]]\naction=\"apply\"\nwhen=\"unattended\"\npaths=[\"src/**\"]\n",
                DocKind::Preset,
                "preset wide",
            ),
        );
        assert!(compile(&src, Some("wide")).is_err());
    }

    #[test]
    fn environment_is_bound_to_the_connection_not_the_label() {
        let owner = "[targets.scratch-db]\nenvironment=\"dev\"\nconnection=\"pg://dev/scratch\"\n\
                     [[allow]]\naction=\"db_query\"\nenvironment=[\"dev\"]\nwhen=\"unattended\"\n";
        let t = table(Some(owner), "strict");
        let ask = |label: Option<&str>, conn: Option<&str>| {
            t.decide(
                &Request::new("db_query", When::Unattended).with_target(TargetRef {
                    label: label.map(Into::into),
                    connection: conn.map(Into::into),
                }),
            )
        };
        assert!(ask(Some("scratch-db"), Some("pg://dev/scratch")).is_allow());
        // Same label, production connection string: production, denied.
        let d = ask(Some("scratch-db"), Some("pg://prod/main"));
        assert!(d.is_deny());
        assert_eq!(d.environment, Some(Environment::Production));
        // The label alone proves nothing.
        assert!(ask(Some("scratch-db"), None).is_deny());
        // A dev connection under a label declared for another connection is production.
        let owner2 = format!(
            "{owner}[targets.other]\nenvironment=\"production\"\nconnection=\"pg://prod/main\"\n"
        );
        let t2 = table(Some(&owner2), "strict");
        let d = t2.decide(
            &Request::new("db_query", When::Unattended).with_target(TargetRef {
                label: Some("scratch-db".into()),
                connection: Some("pg://prod/main".into()),
            }),
        );
        assert!(d.is_deny());
        // A declared label with a different declared connection is a mismatch.
        let owner3 = "[targets.a]\nenvironment=\"dev\"\nconnection=\"c1\"\n\
                      [targets.b]\nenvironment=\"dev\"\nconnection=\"c2\"\n\
                      [[allow]]\naction=\"db_query\"\nenvironment=[\"dev\"]\nwhen=\"unattended\"\n";
        let t3 = table(Some(owner3), "strict");
        let d = t3.decide(
            &Request::new("db_query", When::Unattended).with_target(TargetRef {
                label: Some("a".into()),
                connection: Some("c2".into()),
            }),
        );
        assert!(d.is_deny(), "{d:?}");
    }

    #[test]
    fn one_connection_cannot_be_two_environments() {
        let src = sources(
            Some(
                "[targets.a]\nenvironment=\"dev\"\nconnection=\"c\"\n\
                 [targets.b]\nenvironment=\"test\"\nconnection=\"c\"\n",
            ),
            "strict",
        );
        let e = compile(&src, None).unwrap_err();
        assert!(e[0].contains("One connection is one environment"), "{e:?}");
    }

    #[test]
    fn floor_production_declaration_beats_an_owner_dev_declaration() {
        let mut src = sources(
            Some(
                "[targets.db]\nenvironment=\"dev\"\nconnection=\"c\"\n\
                 [[allow]]\naction=\"db_query\"\nenvironment=[\"dev\"]\nwhen=\"unattended\"\n",
            ),
            "strict",
        );
        src.floor = Some(doc(
            "[targets.db]\nenvironment=\"production\"\nconnection=\"c\"\n",
            DocKind::Floor,
            "org floor",
        ));
        let t = compile(&src, None).unwrap();
        let d = t.decide(
            &Request::new("db_query", When::Unattended).with_target(TargetRef {
                label: None,
                connection: Some("c".into()),
            }),
        );
        assert_eq!(d.environment, Some(Environment::Production));
        assert!(d.is_deny());
    }

    fn apply_owner() -> CompiledRules {
        table(
            Some(
                "[[allow]]\naction=\"apply\"\nwhen=\"unattended\"\npaths=[\"docs/research/**\"]\n",
            ),
            "strict",
        )
    }

    fn apply(t: &CompiledRules, when: When, path: &str) -> Decision {
        t.decide(&Request::new("apply", when).with_path(path))
    }

    #[test]
    fn unattended_apply_is_limited_to_the_named_prefix() {
        let t = apply_owner();
        assert!(apply(&t, When::Unattended, "docs/research/a.md").is_allow());
        assert!(apply(&t, When::Unattended, "Docs\\Research\\sub\\a.md").is_allow());
        assert!(apply(&t, When::Unattended, "docs/other/a.md").is_deny());
        assert!(apply(&t, When::Unattended, "docs/researchers/a.md").is_deny());
        assert!(apply(&t, When::Unattended, "docs/research").is_allow());
    }

    #[test]
    fn apply_bypass_attempts_are_denied() {
        let t = apply_owner();
        for path in [
            "docs/research/../../.git/hooks/pre-commit",
            "docs/research/../../src/main.rs",
            "../docs/research/a.md",
            "/etc/passwd",
            "C:\\docs\\research\\a.md",
            "docs/research/.git/config",
            "docs/research/.GIT/config",
            "docs/research/.git./config",
            "docs/research/GIT~1/config",
            "docs/research/.ta/rules.toml",
            ".ta/rules.toml",
            ".TA/rules.toml.sig",
            "docs/research/.claude/settings.json",
            "docs/research/.CLAUDE/x",
            "docs/research/.mcp.json",
            "docs/research/.MCP.JSON",
            "docs/research/.github/workflows/ci.yml",
            "docs/research/CLAUDE.md",
            "docs/research/caf\u{e9}/a.md",
            "docs/research/cafe\u{301}/a.md",
            "docs/research/.git::$INDEX_ALLOCATION/x",
        ] {
            let d = apply(&t, When::Unattended, path);
            assert!(d.is_deny(), "{path} must be denied, got {d:?}");
            assert_eq!(d.layer, Layer::BuiltIn, "{path}");
        }
    }

    #[test]
    fn rules_files_are_never_reachable_through_apply_even_attended() {
        let t = table(None, "open");
        for path in [
            ".ta/rules.toml",
            ".ta/rules.toml.sig",
            ".TA\\rules.toml",
            ".ta/personas/cos.toml",
            "x/../.ta/rules.toml",
        ] {
            assert!(apply(&t, When::Attended, path).is_deny(), "{path}");
        }
        // Attended edits of other protected places go through the table.
        assert!(apply(&t, When::Attended, ".claude/settings.json").is_allow());
    }

    #[test]
    fn owner_declared_prompt_sources_block_unattended_apply() {
        let t = table(
            Some(
                "[prompt_sources]\npaths=[\"docs/wiki/**\"]\n\
                 [[allow]]\naction=\"apply\"\nwhen=\"unattended\"\npaths=[\"docs/**\"]\n",
            ),
            "strict",
        );
        assert!(apply(&t, When::Unattended, "docs/notes/a.md").is_allow());
        assert!(apply(&t, When::Unattended, "docs/wiki/home.md").is_deny());
        assert!(apply(&t, When::Unattended, "DOCS/WIKI/home.md").is_deny());
    }

    #[test]
    fn refusal_names_the_rule_and_the_fix() {
        let t = table(None, "strict");
        let d = t.decide(&Request::new("email", When::Unattended));
        let msg = d.refusal("email", When::Unattended);
        assert!(
            msg.starts_with("Blocked: unattended email is not allowed by your rules"),
            "{msg}"
        );
        assert!(msg.contains("posture strict [[deny]] #1"), "{msg}");
        assert!(msg.contains("Email always needs a person."), "{msg}");
        assert!(msg.contains("ta rules sign"), "{msg}");
    }

    #[test]
    fn decide_is_deterministic_and_total_over_the_cells() {
        let t = table(None, "balanced");
        let a = t.decide(&Request::new("db_query", When::Attended));
        let b = t.decide(&Request::new("db_query", When::Attended));
        assert_eq!(a, b);
    }
}
