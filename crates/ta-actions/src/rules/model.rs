// rules/model.rs: the rules vocabulary and the parser/validator (v0.17.12.4).
//
// A rules document (owner file, organization floor, preset or posture) has the
// same shape:
//
// ```toml
// posture = "strict"                 # owner file only
//
// [targets.scratch-db]               # owner file (and floor, production only)
// type = "database"
// environment = "dev"                # production | staging | dev | test
// connection = "postgres://dev.example.com/scratch"
//
// [[allow]]                          # also [[ask]] and [[deny]]
// action = "db_query"
// environment = ["dev", "test"]
// when = "unattended"                # unattended | attended; absent = both
//
// [[allow]]
// action = "apply"
// paths = ["docs/research/**"]       # literal prefixes, never .ta/.git/.claude/.mcp.json
// when = "unattended"
// ```
//
// Every struct rejects unknown fields. A misspelled `enviroment` would
// otherwise silently widen a rule, so a typo is a compile error that names the
// field and the line (the TOML error carries both).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::paths;

/// Version of the rules file format this build understands.
pub const FORMAT_VERSION: u32 = 1;

/// The result of a rule. Ordered from loosest to strictest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Allow,
    Ask,
    Deny,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Allow => "allow",
            Effect::Ask => "ask",
            Effect::Deny => "deny",
        }
    }
    fn table_key(self) -> &'static str {
        self.as_str()
    }
}

/// Whether a person is present. Attendance is proven by a human credential
/// (CR-02) at the call site and is never read from a label the goal controls.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum When {
    /// Unproven means unattended.
    #[default]
    Unattended,
    Attended,
}

impl When {
    pub const ALL: [When; 2] = [When::Unattended, When::Attended];
    pub fn as_str(self) -> &'static str {
        match self {
            When::Unattended => "unattended",
            When::Attended => "attended",
        }
    }
    pub fn parse(s: &str) -> Option<When> {
        match s.trim().to_ascii_lowercase().as_str() {
            "unattended" => Some(When::Unattended),
            "attended" => Some(When::Attended),
            _ => None,
        }
    }
}

/// Where an external action lands. Unknown is production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Production,
    Staging,
    Dev,
    Test,
}

impl Environment {
    pub const ALL: [Environment; 4] = [
        Environment::Production,
        Environment::Staging,
        Environment::Dev,
        Environment::Test,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Production => "production",
            Environment::Staging => "staging",
            Environment::Dev => "dev",
            Environment::Test => "test",
        }
    }
    pub fn parse(s: &str) -> Option<Environment> {
        match s.trim().to_ascii_lowercase().as_str() {
            "production" | "prod" => Some(Environment::Production),
            "staging" => Some(Environment::Staging),
            "dev" | "development" => Some(Environment::Dev),
            "test" => Some(Environment::Test),
            _ => None,
        }
    }
    /// Higher is closer to production. Used to keep the stricter of two
    /// declarations for the same connection.
    pub fn strictness(self) -> u8 {
        match self {
            Environment::Production => 3,
            Environment::Staging => 2,
            Environment::Dev => 1,
            Environment::Test => 0,
        }
    }
}

/// Which source a rule came from. Printed by `ta rules show` for every cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    OrgFloor,
    Legacy,
    Preset,
    Owner,
    Posture,
    BuiltIn,
}

impl Layer {
    pub fn label(self) -> &'static str {
        match self {
            Layer::OrgFloor => "org floor",
            Layer::Legacy => "legacy",
            Layer::Preset => "preset",
            Layer::Owner => "owner rules",
            Layer::Posture => "posture",
            Layer::BuiltIn => "built-in",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    action: String,
    #[serde(default)]
    environment: Option<OneOrMany>,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSpec {
    #[serde(default, rename = "type")]
    _kind: Option<String>,
    environment: String,
    #[serde(default)]
    connection: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PromptSourcesSpec {
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DocSpec {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    revision: Option<u32>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    posture: Option<String>,
    #[serde(default)]
    migrated_from: Vec<String>,
    /// Postures only: run a draft's outside actions one at a time in declared order.
    #[serde(default)]
    ordered_actions: Option<bool>,
    #[serde(default)]
    allow: Vec<RuleSpec>,
    #[serde(default)]
    ask: Vec<RuleSpec>,
    #[serde(default)]
    deny: Vec<RuleSpec>,
    #[serde(default)]
    targets: BTreeMap<String, TargetSpec>,
    #[serde(default)]
    prompt_sources: PromptSourcesSpec,
}

/// A validated rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub effect: Effect,
    /// Lowercase action name, or `*` (deny and ask only).
    pub action: String,
    /// `None` matches every environment.
    pub envs: Option<BTreeSet<Environment>>,
    /// `None` matches both.
    pub when: Option<When>,
    /// Folded literal prefixes (apply rules). Empty matches every path.
    pub paths: Vec<Vec<String>>,
    pub reason: Option<String>,
    pub layer: Layer,
    /// `owner rules [[allow]] #2`: stable, path independent.
    pub label: String,
}

impl Rule {
    pub fn specificity(&self) -> u8 {
        u8::from(self.action != "*")
            + u8::from(self.envs.is_some())
            + u8::from(self.when.is_some())
            + u8::from(!self.paths.is_empty())
    }
}

/// A declared target, bound to its real connection identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub environment: Environment,
    pub connection: Option<String>,
}

/// A parsed, validated rules document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RulesDoc {
    pub revision: u32,
    pub description: Option<String>,
    pub posture: Option<String>,
    pub migrated_from: Vec<String>,
    /// Saga order (postures only): a draft's outside actions run one at a time
    /// in declared order, and a failed step stops the rest.
    pub ordered_actions: bool,
    pub rules: Vec<Rule>,
    pub targets: Vec<Target>,
    /// Folded owner-declared prompt-source prefixes.
    pub prompt_sources: Vec<Vec<String>>,
}

/// What the document is for. Decides which constructs are legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Owner,
    Floor,
    Preset,
    Posture,
}

impl DocKind {
    fn layer(self) -> Layer {
        match self {
            DocKind::Owner => Layer::Owner,
            DocKind::Floor => Layer::OrgFloor,
            DocKind::Preset => Layer::Preset,
            DocKind::Posture => Layer::Posture,
        }
    }
}

/// Action names are short identifiers. Anything else is a typo or an attempt
/// to smuggle a pattern in.
fn valid_action(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 64
        && a.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

/// Normalize CRLF and strip a BOM, so a file edited on Windows hashes, signs
/// and parses the same as on Unix.
pub fn normalize_text(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.replace("\r\n", "\n")
}

impl RulesDoc {
    /// Parse and validate. `name` labels errors and rules (`owner rules`,
    /// `preset researcher`, `posture strict`). Every problem is collected so
    /// the person fixes the file in one pass; each says what to change.
    pub fn parse(text: &str, kind: DocKind, name: &str) -> Result<RulesDoc, Vec<String>> {
        let text = normalize_text(text);
        let spec: DocSpec = match toml::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                return Err(vec![format!(
                    "{name} does not parse: {e}. Fix the line named above (unknown or misspelled \
                     fields are refused on purpose: a typo must never widen a rule)."
                )])
            }
        };
        let mut errors = Vec::new();
        if let Some(v) = spec.version {
            if v != FORMAT_VERSION {
                errors.push(format!(
                    "{name}: version = {v} is not supported by this TA (expected {FORMAT_VERSION}). \
                     Upgrade TA, or change the file back to version = {FORMAT_VERSION}."
                ));
            }
        }
        if kind != DocKind::Owner && kind != DocKind::Posture && spec.posture.is_some() {
            errors.push(format!(
                "{name}: only the owner rules file chooses a posture. Remove the posture line."
            ));
        }
        if kind != DocKind::Posture && spec.ordered_actions.is_some() {
            errors.push(format!(
                "{name}: ordered_actions is set by a shipped posture only; a rules file cannot \
                 loosen or change it. Remove the line."
            ));
        }
        let layer = kind.layer();
        let mut rules = Vec::new();
        for (effect, specs) in [
            (Effect::Allow, &spec.allow),
            (Effect::Ask, &spec.ask),
            (Effect::Deny, &spec.deny),
        ] {
            for (i, r) in specs.iter().enumerate() {
                let label = format!("{name} [[{}]] #{}", effect.table_key(), i + 1);
                match build_rule(effect, r, kind, layer, &label) {
                    Ok(rule) => rules.push(rule),
                    Err(mut es) => errors.append(&mut es),
                }
            }
        }
        let mut targets = Vec::new();
        for (tname, t) in &spec.targets {
            if kind == DocKind::Posture || kind == DocKind::Preset {
                errors.push(format!(
                    "{name}: [targets.{tname}] is only allowed in the owner rules file or the \
                     org floor. Move it there."
                ));
                continue;
            }
            let Some(env) = Environment::parse(&t.environment) else {
                errors.push(format!(
                    "{name}: [targets.{tname}] environment = \"{}\" is not one of production, \
                     staging, dev, test. Fix the value.",
                    t.environment
                ));
                continue;
            };
            let connection = t
                .connection
                .as_ref()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty());
            if env != Environment::Production && connection.is_none() {
                errors.push(format!(
                    "{name}: [targets.{tname}] says environment = \"{}\" but names no connection. \
                     A label alone proves nothing: a target called '{tname}' could point at \
                     production. Add connection = \"<the real connection identity>\", or set \
                     environment = \"production\".",
                    env.as_str()
                ));
                continue;
            }
            if kind == DocKind::Floor && env != Environment::Production {
                errors.push(format!(
                    "{name}: the org floor may only declare production targets, because a floor \
                     can only tighten. Change [targets.{tname}] to environment = \"production\"."
                ));
                continue;
            }
            targets.push(Target {
                name: tname.clone(),
                environment: env,
                connection,
            });
        }
        let mut prompt_sources = Vec::new();
        for p in &spec.prompt_sources.paths {
            match paths::parse_rule_prefix(p) {
                Ok(c) => prompt_sources.push(c),
                Err(_) => {
                    // A prompt-source prefix may be any literal directory, even
                    // one that would be refused as an apply allow. Reuse the
                    // path folding but accept protected places.
                    let comps: Vec<String> = p
                        .trim()
                        .trim_end_matches("/**")
                        .split(['/', '\\'])
                        .filter(|c| !c.is_empty() && *c != ".")
                        .map(ta_workspace::path_safety::fold_component)
                        .collect();
                    if comps.is_empty() || comps.iter().any(|c| c == ".." || c.contains('*')) {
                        errors.push(format!(
                            "{name}: [prompt_sources] path '{p}' is not a literal directory. \
                             Fix: name the directory, for example docs/wiki/**"
                        ));
                    } else {
                        prompt_sources.push(comps);
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(RulesDoc {
                revision: spec.revision.unwrap_or(1),
                description: spec.description,
                posture: spec.posture,
                migrated_from: spec.migrated_from,
                ordered_actions: spec.ordered_actions.unwrap_or(false),
                rules,
                targets,
                prompt_sources,
            })
        } else {
            Err(errors)
        }
    }
}

fn build_rule(
    effect: Effect,
    r: &RuleSpec,
    kind: DocKind,
    layer: Layer,
    label: &str,
) -> Result<Rule, Vec<String>> {
    let mut errors = Vec::new();
    let action = r.action.trim().to_ascii_lowercase();
    if action != "*" && !valid_action(&action) {
        errors.push(format!(
            "{label}: action = \"{}\" is not a valid action name (lowercase letters, digits, _ - .). \
             Fix the action.",
            r.action
        ));
    }
    if action == "*" && effect == Effect::Allow {
        errors.push(format!(
            "{label}: an allow rule cannot use action = \"*\". Name the action you want to allow."
        ));
    }
    if kind == DocKind::Floor && effect == Effect::Allow {
        errors.push(format!(
            "{label}: the org floor can only deny or ask, never allow, because nothing below a \
             floor may be loosened by it and a floor grants nothing. Change it to [[deny]] or \
             [[ask]], or remove it."
        ));
    }
    let when = match r.when.as_deref() {
        None => None,
        Some(w) => match When::parse(w) {
            Some(w) => Some(w),
            None => {
                errors.push(format!(
                    "{label}: when = \"{w}\" must be \"unattended\" or \"attended\". Fix the value."
                ));
                None
            }
        },
    };
    let envs = match &r.environment {
        None => None,
        Some(v) => {
            let mut set = BTreeSet::new();
            for e in v.clone().into_vec() {
                match Environment::parse(&e) {
                    Some(env) => {
                        set.insert(env);
                    }
                    None => errors.push(format!(
                        "{label}: environment \"{e}\" is not one of production, staging, dev, \
                         test. Fix the value."
                    )),
                }
            }
            if set.is_empty() && errors.is_empty() {
                errors.push(format!(
                    "{label}: environment = [] matches nothing. Remove the line to match every \
                     environment."
                ));
            }
            Some(set)
        }
    };
    let mut path_prefixes = Vec::new();
    if !r.paths.is_empty() && action != "apply" {
        errors.push(format!(
            "{label}: paths only apply to action = \"apply\". Remove the paths line."
        ));
    }
    if action == "apply" {
        if effect == Effect::Allow && r.paths.is_empty() && when != Some(When::Attended) {
            errors.push(format!(
                "{label}: an allow rule for apply needs paths = [\"docs/notes/**\"], so an \
                 unattended apply can only reach the directories you name. (Only an attended \
                 allow may omit paths.)"
            ));
        }
        for p in &r.paths {
            match paths::parse_rule_prefix(p) {
                Ok(c) => path_prefixes.push(c),
                Err(e) => errors.push(format!("{label}: {e}")),
            }
        }
        if envs.is_some() {
            errors.push(format!(
                "{label}: environment does not apply to action = \"apply\". Remove the line."
            ));
        }
    }
    if errors.is_empty() {
        Ok(Rule {
            effect,
            action,
            envs,
            when,
            paths: path_prefixes,
            reason: r.reason.clone(),
            layer,
            label: label.to_string(),
        })
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<RulesDoc, Vec<String>> {
        RulesDoc::parse(text, DocKind::Owner, "owner rules")
    }

    #[test]
    fn parses_the_documented_example() {
        let doc = parse(
            r#"
posture = "strict"
[targets.scratch-db]
type = "database"
environment = "dev"
connection = "postgres://dev.example.com/scratch"
[[allow]]
action = "db_query"
environment = ["dev", "test"]
when = "unattended"
[[allow]]
action = "apply"
paths = ["docs/research/**"]
when = "unattended"
[[deny]]
action = "email"
reason = "Email always needs a person."
"#,
        )
        .unwrap();
        assert_eq!(doc.rules.len(), 3);
        assert_eq!(doc.targets[0].environment, Environment::Dev);
        assert_eq!(doc.rules[1].paths, vec![vec!["docs", "research"]]);
        assert_eq!(doc.rules[1].label, "owner rules [[allow]] #2");
    }

    #[test]
    fn crlf_and_bom_parse_the_same() {
        let unix = "[[deny]]\naction = \"email\"\n";
        let win = "\u{feff}[[deny]]\r\naction = \"email\"\r\n";
        assert_eq!(parse(unix).unwrap(), parse(win).unwrap());
        assert_eq!(normalize_text(win), unix);
    }

    #[test]
    fn unknown_and_misspelled_fields_fail_closed() {
        let e = parse("[[allow]]\naction = \"db_query\"\nenviroment = [\"dev\"]\n").unwrap_err();
        assert!(e[0].contains("enviroment") && e[0].contains("Fix"), "{e:?}");
        let e = parse("postur = \"open\"\n").unwrap_err();
        assert!(e[0].contains("postur"), "{e:?}");
    }

    #[test]
    fn bad_values_say_what_to_change() {
        let e = parse(
            r#"
[[allow]]
action = "*"
[[allow]]
action = "db_query"
environment = ["prodd"]
when = "sometimes"
[[allow]]
action = "email"
paths = ["docs"]
[[allow]]
action = "apply"
[[allow]]
action = "apply"
paths = [".git/hooks", "docs/*", ".mcp.json"]
"#,
        )
        .unwrap_err();
        let all = e.join("\n");
        for needle in [
            "cannot use action",
            "prodd",
            "sometimes",
            "only apply to action",
            "needs paths",
            ".git/hooks",
            "glob",
            ".mcp.json",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
    }

    #[test]
    fn target_environment_must_be_bound_to_a_connection() {
        let e = parse("[targets.scratch]\nenvironment = \"dev\"\n").unwrap_err();
        assert!(e[0].contains("names no connection"), "{e:?}");
        // Production needs no connection: it is the strictest reading anyway.
        assert!(parse("[targets.main]\nenvironment = \"production\"\n").is_ok());
    }

    #[test]
    fn floor_cannot_allow_or_declare_non_production_targets() {
        let e = RulesDoc::parse(
            "[[allow]]\naction = \"email\"\n",
            DocKind::Floor,
            "org floor",
        )
        .unwrap_err();
        assert!(e[0].contains("can only deny or ask"), "{e:?}");
        let e = RulesDoc::parse(
            "[targets.x]\nenvironment = \"dev\"\nconnection = \"c\"\n",
            DocKind::Floor,
            "org floor",
        )
        .unwrap_err();
        assert!(e[0].contains("only declare production"), "{e:?}");
    }

    #[test]
    fn apply_allow_without_paths_is_only_legal_when_attended() {
        assert!(parse("[[allow]]\naction = \"apply\"\nwhen = \"attended\"\n").is_ok());
        assert!(parse("[[allow]]\naction = \"apply\"\nwhen = \"unattended\"\n").is_err());
    }

    #[test]
    fn unsupported_version_is_refused() {
        let e = parse("version = 2\n").unwrap_err();
        assert!(e[0].contains("version = 2"), "{e:?}");
    }
}
