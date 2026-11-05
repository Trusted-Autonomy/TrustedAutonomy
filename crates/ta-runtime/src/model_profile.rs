// model_profile.rs — TA's framework-neutral model + reasoning-effort abstraction.
//
// Two independent axes plus named profiles:
//
//   tier    abstract capability class   local | light | standard | max
//   effort  abstract reasoning level    default | low | medium | high | xhigh | max
//   profile a named (tier, effort) pair light | standard | deep | max | local
//
// Every agent framework maps the abstraction onto its own concrete model ids and
// its own way of passing them (CLI args, config-override args, or env vars).
// The mapping is DATA (`model_map` / `effort_map` in an agent manifest, in a
// project/user `model-profiles.toml`, or in the shipped catalog
// `model-profiles.default.toml`); nothing here branches on a framework name.
//
// Resolution precedence, highest first:
//
//   1. explicit CLI flags (`--model`, `--profile`, `--tier`, `--effort`)
//   2. goal/plan setting (PLAN.md phase marker `<!-- profile: deep -->`)
//   3. project override  (`.ta/model-profiles.toml`)
//   4. user override     (`~/.config/ta/model-profiles.toml`)
//   5. manifest default  (`model_map`/`effort_map` in the agent manifest), then
//      the shipped catalog default
//
// Layers 1-4 decide WHAT is requested (per axis, the highest layer that names it
// wins; inside one layer an explicit `tier`/`effort` beats that layer's
// `profile`). The mapping layers decide HOW it is delivered, per tier / per
// level: project, then user, then manifest, then shipped catalog.
//
// Nothing is dropped silently: a requested tier or effort the framework cannot
// express is reported in `ResolvedModelEffort::warnings` (the launcher prints
// them) and `effort_applied` is false.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::sandbox_launch::is_denied_env;

/// Shipped catalog of per-framework mappings, keyed by command name.
const SHIPPED_CATALOG: &str = include_str!("model-profiles.default.toml");

/// File name of the project/user override file (`.ta/model-profiles.toml`,
/// `~/.config/ta/model-profiles.toml`).
pub const OVERRIDE_FILE_NAME: &str = "model-profiles.toml";

// ── Errors ───────────────────────────────────────────────────────────────────

/// A model/effort selection or mapping that could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelProfileError {
    #[error("unknown tier '{given}' (from {origin}). Valid tiers: {valid}.")]
    UnknownTier {
        given: String,
        origin: String,
        valid: String,
    },
    #[error("unknown effort '{given}' (from {origin}). Valid efforts: {valid}.")]
    UnknownEffort {
        given: String,
        origin: String,
        valid: String,
    },
    #[error(
        "unknown profile '{given}' (from {origin}). Valid profiles: {valid}. \
         Define your own under [profiles.<name>] in .ta/model-profiles.toml."
    )]
    UnknownProfile {
        given: String,
        origin: String,
        valid: String,
    },
    #[error("invalid model/effort mapping in {origin}: {detail}")]
    InvalidMapping { origin: String, detail: String },
    #[error("cannot read {path}: {detail}. Fix or delete the file, then re-run.")]
    Unreadable { path: String, detail: String },
}

// ── Tier / Effort ────────────────────────────────────────────────────────────

/// Abstract capability class of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A model running on the user's own machine (Ollama and similar).
    Local,
    /// Cheapest/fastest hosted model.
    Light,
    /// The everyday coding model.
    Standard,
    /// The most capable hosted model.
    Max,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Local, Tier::Light, Tier::Standard, Tier::Max];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Local => "local",
            Tier::Light => "light",
            Tier::Standard => "standard",
            Tier::Max => "max",
        }
    }

    pub fn valid_names() -> String {
        Self::ALL.map(Tier::as_str).join(", ")
    }

    /// Parse `s`; `origin` names where the value came from (for the error).
    pub fn parse(s: &str, origin: &str) -> Result<Tier, ModelProfileError> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| ModelProfileError::UnknownTier {
                given: s.to_string(),
                origin: origin.to_string(),
                valid: Self::valid_names(),
            })
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = ModelProfileError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Tier::parse(s, "value")
    }
}

/// Abstract reasoning level.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Pass nothing: the agent/model uses its own default.
    #[default]
    Default,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 6] = [
        Effort::Default,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn valid_names() -> String {
        Self::ALL.map(Effort::as_str).join(", ")
    }

    pub fn parse(s: &str, origin: &str) -> Result<Effort, ModelProfileError> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == s)
            .ok_or_else(|| ModelProfileError::UnknownEffort {
                given: s.to_string(),
                origin: origin.to_string(),
                valid: Self::valid_names(),
            })
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Effort {
    type Err = ModelProfileError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Effort::parse(s, "value")
    }
}

/// A named (tier, effort) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub tier: Tier,
    #[serde(default)]
    pub effort: Effort,
}

/// The profiles TA ships. `deep` is the `standard` model with more reasoning;
/// `max` is the top model at maximum effort. Override or extend them under
/// `[profiles.<name>]` in `.ta/model-profiles.toml`.
pub fn builtin_profiles() -> BTreeMap<String, Profile> {
    let p = |tier, effort| Profile { tier, effort };
    BTreeMap::from([
        ("local".to_string(), p(Tier::Local, Effort::Default)),
        ("light".to_string(), p(Tier::Light, Effort::Default)),
        ("standard".to_string(), p(Tier::Standard, Effort::Default)),
        ("deep".to_string(), p(Tier::Standard, Effort::Xhigh)),
        ("max".to_string(), p(Tier::Max, Effort::Max)),
    ])
}

// ── Mapping data ─────────────────────────────────────────────────────────────

/// Where one resolved value goes on the agent's command line / environment.
/// `{value}` in any template is replaced by the concrete value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Arg,
    Config,
    Env,
}

impl Via {
    fn label(self) -> &'static str {
        match self {
            Via::Arg => "arg",
            Via::Config => "config",
            Via::Env => "env",
        }
    }
}

/// How the concrete model id is delivered, and which id each tier maps to.
///
/// ```toml
/// [model_map]
/// args = ["--model", "{value}"]        # and/or `config = [...]`, `env = {...}`
/// [model_map.tiers]
/// light = "claude-haiku-5-5"           # a tier with no entry is unsupported
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMap {
    /// Argument template appended to the agent command line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Config-override argument template (for example `["-c", "model=\"{value}\""]`).
    /// Delivered like `args`; reported as `via=config`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<String>,
    /// Environment variables to set (`NAME = "template"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Tier name -> concrete model id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiers: BTreeMap<String, String>,
    /// Shown when a requested tier has no mapping: how to configure it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// How a reasoning level is delivered, and the concrete value for each level.
///
/// ```toml
/// [effort_map]
/// env = { CLAUDE_CODE_EFFORT_LEVEL = "{value}" }
/// [effort_map.levels]
/// low = "low"                          # a level with no entry is unsupported
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortMap {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Effort level name -> concrete value for this framework.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub levels: BTreeMap<String, String>,
    /// Shown when a requested level has no mapping: why, and how to configure it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The `model_map` + `effort_map` pair for one framework.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkMapping {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_map: Option<ModelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_map: Option<EffortMap>,
}

impl ModelMap {
    fn has_delivery(&self) -> bool {
        !(self.args.is_empty() && self.config.is_empty() && self.env.is_empty())
    }

    /// `higher` wins: its delivery replaces ours when it declares one, tiers merge keywise.
    fn overlay(mut self, higher: &ModelMap) -> ModelMap {
        if higher.has_delivery() {
            self.args = higher.args.clone();
            self.config = higher.config.clone();
            self.env = higher.env.clone();
        }
        self.tiers
            .extend(higher.tiers.iter().map(|(k, v)| (k.clone(), v.clone())));
        if higher.hint.is_some() {
            self.hint = higher.hint.clone();
        }
        self
    }

    fn validate(&self, origin: &str) -> Result<(), ModelProfileError> {
        for tier in self.tiers.keys() {
            Tier::parse(tier, &format!("[model_map.tiers] in {origin}"))?;
        }
        Ok(())
    }
}

impl EffortMap {
    fn has_delivery(&self) -> bool {
        !(self.args.is_empty() && self.config.is_empty() && self.env.is_empty())
    }

    fn overlay(mut self, higher: &EffortMap) -> EffortMap {
        if higher.has_delivery() {
            self.args = higher.args.clone();
            self.config = higher.config.clone();
            self.env = higher.env.clone();
        }
        self.levels
            .extend(higher.levels.iter().map(|(k, v)| (k.clone(), v.clone())));
        if higher.hint.is_some() {
            self.hint = higher.hint.clone();
        }
        self
    }

    fn validate(&self, origin: &str) -> Result<(), ModelProfileError> {
        for level in self.levels.keys() {
            let e = Effort::parse(level, &format!("[effort_map.levels] in {origin}"))?;
            if e == Effort::Default {
                return Err(ModelProfileError::InvalidMapping {
                    origin: origin.to_string(),
                    detail: "effort 'default' means \"pass nothing\" and cannot be mapped; \
                             remove it from [effort_map.levels]"
                        .to_string(),
                });
            }
        }
        Ok(())
    }
}

impl FrameworkMapping {
    fn validate(&self, origin: &str) -> Result<(), ModelProfileError> {
        if let Some(m) = &self.model_map {
            m.validate(origin)?;
        }
        if let Some(e) = &self.effort_map {
            e.validate(origin)?;
        }
        Ok(())
    }
}

// ── Selection ────────────────────────────────────────────────────────────────

/// A request for a model/effort, as typed by the user or read from a config
/// layer. Strings are validated at resolve time so errors can name their origin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Named (tier, effort) pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// A concrete model id. Beats the tier mapping; only meaningful on the CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.profile.is_none()
            && self.tier.is_none()
            && self.effort.is_none()
            && self.model.is_none()
    }
}

/// One precedence layer: where it came from, and what it asks for.
#[derive(Debug, Clone)]
pub struct SelectionLayer {
    /// `cli`, `plan`, `project` or `user`.
    pub source: &'static str,
    /// Human-readable origin for error messages (`--profile`, a file path, ...).
    pub origin: String,
    pub selection: Selection,
}

// ── Override / catalog file ──────────────────────────────────────────────────

/// Contents of a `model-profiles.toml` (project, user, or the shipped catalog).
///
/// ```toml
/// [defaults]                     # what to use when nothing higher asks
/// profile = "standard"
///
/// [profiles.review]              # your own named profile
/// tier = "standard"
/// effort = "high"
///
/// [frameworks.claude-code.model_map.tiers]   # key = framework name or command
/// standard = "claude-sonnet-5-5"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfilesFile {
    #[serde(default)]
    pub defaults: Selection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frameworks: BTreeMap<String, FrameworkMapping>,
}

impl ModelProfilesFile {
    /// Parse and validate TOML text; `origin` names the file for error messages.
    pub fn parse(text: &str, origin: &str) -> Result<Self, ModelProfileError> {
        let file: ModelProfilesFile =
            toml::from_str(text).map_err(|e| ModelProfileError::InvalidMapping {
                origin: origin.to_string(),
                detail: format!("{e}. See docs/USAGE.md, \"Model and effort profiles\""),
            })?;
        for (name, m) in &file.frameworks {
            m.validate(&format!("{origin} [frameworks.{name}]"))?;
        }
        Ok(file)
    }

    /// Load `path`; a missing file is `Ok(None)`, an unreadable or invalid one an error.
    pub fn load(path: &Path) -> Result<Option<Self>, ModelProfileError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ModelProfileError::Unreadable {
                path: path.display().to_string(),
                detail: e.to_string(),
            }),
        }
    }

    /// The catalog compiled into the binary.
    pub fn shipped() -> Self {
        Self::parse(SHIPPED_CATALOG, "shipped model-profiles.default.toml")
            .expect("shipped model-profiles.default.toml must parse (covered by tests)")
    }

    /// Mapping for a framework: keyed by its name first, then its command basename.
    fn mapping_for(&self, name: &str, command: &str) -> Option<&FrameworkMapping> {
        self.frameworks
            .get(name)
            .or_else(|| self.frameworks.get(command_basename(command)))
    }
}

/// `/usr/local/bin/claude.exe` -> `claude`.
pub fn command_basename(command: &str) -> &str {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    base.strip_suffix(".exe").unwrap_or(base)
}

// ── Config: all layers loaded ────────────────────────────────────────────────

/// Project + user override files and the shipped catalog.
#[derive(Debug, Clone)]
pub struct ModelProfileConfig {
    pub project: Option<(PathBuf, ModelProfilesFile)>,
    pub user: Option<(PathBuf, ModelProfilesFile)>,
    pub shipped: ModelProfilesFile,
}

/// The agent being launched, as far as mapping lookup is concerned.
#[derive(Debug, Clone, Copy)]
pub struct AgentTarget<'a> {
    /// Framework name (`claude-code`, `codex`, a custom manifest's name).
    pub name: &'a str,
    /// The command it launches (`claude`, `codex`, `ta-agent-ollama`).
    pub command: &'a str,
    /// `model_map`/`effort_map` declared inline in the agent manifest, if any.
    pub manifest: Option<&'a FrameworkMapping>,
}

impl ModelProfileConfig {
    /// Load `<project_root>/.ta/model-profiles.toml` and
    /// `~/.config/ta/model-profiles.toml`; either may be absent.
    pub fn load(project_root: &Path) -> Result<Self, ModelProfileError> {
        let project_path = project_root.join(".ta").join(OVERRIDE_FILE_NAME);
        let user_path =
            dirs::home_dir().map(|h| h.join(".config").join("ta").join(OVERRIDE_FILE_NAME));
        Self::load_from(Some(project_path), user_path)
    }

    /// Same as [`load`](Self::load) with explicit paths (tests, embedding).
    pub fn load_from(
        project_path: Option<PathBuf>,
        user_path: Option<PathBuf>,
    ) -> Result<Self, ModelProfileError> {
        let load_one = |p: Option<PathBuf>| -> Result<Option<(PathBuf, ModelProfilesFile)>, _> {
            match p {
                Some(path) => Ok(ModelProfilesFile::load(&path)?.map(|f| (path, f))),
                None => Ok(None),
            }
        };
        Ok(ModelProfileConfig {
            project: load_one(project_path)?,
            user: load_one(user_path)?,
            shipped: ModelProfilesFile::shipped(),
        })
    }

    /// Only the shipped catalog (no override files).
    pub fn shipped_only() -> Self {
        ModelProfileConfig {
            project: None,
            user: None,
            shipped: ModelProfilesFile::shipped(),
        }
    }

    /// Every profile visible to a launch: shipped built-ins, overridden by user, then project.
    pub fn profiles(&self) -> BTreeMap<String, Profile> {
        let mut all = builtin_profiles();
        for file in [self.user.as_ref(), self.project.as_ref()]
            .into_iter()
            .flatten()
        {
            all.extend(file.1.profiles.iter().map(|(k, v)| (k.clone(), *v)));
        }
        all
    }

    /// The `[defaults]` layers from the override files: project, then user.
    fn default_layers(&self) -> Vec<SelectionLayer> {
        let mut layers = Vec::new();
        for (source, file) in [("project", &self.project), ("user", &self.user)] {
            if let Some((path, f)) = file {
                if !f.defaults.is_empty() {
                    layers.push(SelectionLayer {
                        source,
                        origin: format!("[defaults] in {}", path.display()),
                        selection: f.defaults.clone(),
                    });
                }
            }
        }
        layers
    }

    /// Resolve a launch.
    ///
    /// `cli` and `plan` are the two caller-supplied layers (either may be
    /// empty); the project and user `[defaults]` follow them automatically.
    pub fn resolve(
        &self,
        target: AgentTarget<'_>,
        cli: &Selection,
        plan: Option<(&str, &Selection)>,
    ) -> Result<ResolvedModelEffort, ModelProfileError> {
        let mut layers = vec![SelectionLayer {
            source: "cli",
            origin: "the command line".to_string(),
            selection: cli.clone(),
        }];
        if let Some((origin, sel)) = plan {
            layers.push(SelectionLayer {
                source: "plan",
                origin: origin.to_string(),
                selection: sel.clone(),
            });
        }
        layers.extend(self.default_layers());
        self.resolve_layers(target, &layers)
    }

    fn resolve_layers(
        &self,
        target: AgentTarget<'_>,
        layers: &[SelectionLayer],
    ) -> Result<ResolvedModelEffort, ModelProfileError> {
        let profiles = self.profiles();
        let profile_names = || profiles.keys().cloned().collect::<Vec<_>>().join(", ");

        // Validate EVERY layer, even ones a higher layer shadows: a typo in the
        // project file must not stay hidden until someone removes the CLI flag.
        struct Parsed<'a> {
            layer: &'a SelectionLayer,
            profile: Option<(String, Profile)>,
            tier: Option<Tier>,
            effort: Option<Effort>,
        }
        let mut parsed = Vec::new();
        for layer in layers {
            let sel = &layer.selection;
            let profile = match &sel.profile {
                Some(name) => Some((
                    name.clone(),
                    *profiles
                        .get(name)
                        .ok_or_else(|| ModelProfileError::UnknownProfile {
                            given: name.clone(),
                            origin: layer.origin.clone(),
                            valid: profile_names(),
                        })?,
                )),
                None => None,
            };
            let tier = sel
                .tier
                .as_deref()
                .map(|t| Tier::parse(t, &layer.origin))
                .transpose()?;
            let effort = sel
                .effort
                .as_deref()
                .map(|e| Effort::parse(e, &layer.origin))
                .transpose()?;
            parsed.push(Parsed {
                layer,
                profile,
                tier,
                effort,
            });
        }

        // Per axis: the highest layer that names it. Inside a layer, an
        // explicit tier/effort beats that layer's profile.
        let mut tier: Option<(Tier, &'static str, Option<String>)> = None;
        let mut effort: Option<(Effort, &'static str, Option<String>)> = None;
        for p in &parsed {
            if tier.is_none() {
                if let Some(t) = p.tier {
                    tier = Some((t, p.layer.source, None));
                } else if let Some((name, prof)) = &p.profile {
                    tier = Some((prof.tier, p.layer.source, Some(name.clone())));
                }
            }
            if effort.is_none() {
                if let Some(e) = p.effort {
                    effort = Some((e, p.layer.source, None));
                } else if let Some((name, prof)) = &p.profile {
                    effort = Some((prof.effort, p.layer.source, Some(name.clone())));
                }
            }
        }
        let explicit_model = parsed
            .iter()
            .find_map(|p| p.layer.selection.model.clone().map(|m| (m, p.layer.source)));
        let profile_name = tier
            .as_ref()
            .and_then(|t| t.2.clone())
            .or_else(|| effort.as_ref().and_then(|e| e.2.clone()));

        // Mapping: shipped < manifest < user < project, per axis.
        let (model_map, effort_map) = self.effective_mapping(target)?;

        let mut out = ResolvedModelEffort {
            agent: target.name.to_string(),
            profile: profile_name,
            tier: tier.as_ref().map(|t| t.0),
            tier_source: tier.as_ref().map(|t| t.1),
            effort: effort.as_ref().map(|e| e.0).unwrap_or(Effort::Default),
            effort_source: effort.as_ref().map(|e| e.1),
            model: None,
            model_source: None,
            model_via: "none".to_string(),
            effort_applied: false,
            effort_via: "none".to_string(),
            arg_groups: Vec::new(),
            env: BTreeMap::new(),
            warnings: Vec::new(),
        };

        // Model axis: an explicit --model beats the tier mapping.
        let model_choice: Option<(String, &'static str)> = match (&explicit_model, out.tier) {
            (Some((m, src)), _) => Some((m.clone(), *src)),
            (None, Some(t)) => match model_map.as_ref().and_then(|m| m.tiers.get(t.as_str())) {
                Some(id) => Some((id.clone(), out.tier_source.unwrap_or("default"))),
                None => {
                    out.warnings.push(unmapped_tier_warning(
                        target.name,
                        t,
                        model_map.as_ref().and_then(|m| m.hint.as_deref()),
                    ));
                    None
                }
            },
            (None, None) => None,
        };
        if let Some((model, source)) = model_choice {
            match &model_map {
                Some(m) if m.has_delivery() => {
                    let rendered = render(&m.args, &m.config, &m.env, &model);
                    out.absorb_model(&target, rendered, model, source);
                }
                _ => out.warnings.push(format!(
                    "model '{model}' was requested but agent '{}' has no model_map delivery \
                     (how to pass a model), so it was NOT applied and the agent's default model \
                     will run. Declare it in the agent manifest ([model_map] args = [\"--model\", \
                     \"{{value}}\"]) or under [frameworks.{}.model_map] in .ta/model-profiles.toml.",
                    target.name, target.name
                )),
            }
        }

        // Effort axis.
        if out.effort != Effort::Default {
            let level = out.effort;
            let value = effort_map
                .as_ref()
                .and_then(|m| m.levels.get(level.as_str()).map(|v| (m, v.clone())));
            match value {
                Some((m, v)) if m.has_delivery() => {
                    let rendered = render(&m.args, &m.config, &m.env, &v);
                    out.absorb_effort(&target, rendered);
                }
                Some(_) => {
                    out.effort_via = "unsupported".to_string();
                    out.warnings.push(format!(
                        "effort '{level}' maps to a value for agent '{}' but no delivery \
                         (args/config/env) is declared, so it was NOT applied. Add e.g. \
                         env = {{ NAME = \"{{value}}\" }} to its effort_map.",
                        target.name
                    ));
                }
                None => {
                    out.effort_via = "unsupported".to_string();
                    out.warnings.push(unsupported_effort_warning(
                        target.name,
                        level,
                        effort_map.as_ref().and_then(|m| m.hint.as_deref()),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Effective `(model_map, effort_map)` for a target: shipped < manifest < user < project.
    fn effective_mapping(
        &self,
        target: AgentTarget<'_>,
    ) -> Result<(Option<ModelMap>, Option<EffortMap>), ModelProfileError> {
        let mut sources: Vec<(String, &FrameworkMapping)> = Vec::new();
        if let Some(m) = self.shipped.mapping_for(target.name, target.command) {
            sources.push(("the shipped catalog".to_string(), m));
        }
        if let Some(m) = target.manifest {
            sources.push((format!("the '{}' agent manifest", target.name), m));
        }
        if let Some((path, f)) = &self.user {
            if let Some(m) = f.mapping_for(target.name, target.command) {
                sources.push((path.display().to_string(), m));
            }
        }
        if let Some((path, f)) = &self.project {
            if let Some(m) = f.mapping_for(target.name, target.command) {
                sources.push((path.display().to_string(), m));
            }
        }
        let mut model_map: Option<ModelMap> = None;
        let mut effort_map: Option<EffortMap> = None;
        for (origin, m) in sources {
            m.validate(&origin)?;
            if let Some(higher) = &m.model_map {
                model_map = Some(match model_map.take() {
                    Some(lower) => lower.overlay(higher),
                    None => higher.clone(),
                });
            }
            if let Some(higher) = &m.effort_map {
                effort_map = Some(match effort_map.take() {
                    Some(lower) => lower.overlay(higher),
                    None => higher.clone(),
                });
            }
        }
        Ok((model_map, effort_map))
    }
}

fn unmapped_tier_warning(agent: &str, tier: Tier, hint: Option<&str>) -> String {
    let how = hint.map(|h| format!(" {h}")).unwrap_or_default();
    format!(
        "tier '{tier}' has no model mapped for agent '{agent}', so it was NOT applied and the \
         agent's default model will run. Map it in .ta/model-profiles.toml:\n  \
         [frameworks.{agent}.model_map.tiers]\n  {tier} = \"<model-id>\"{how}"
    )
}

fn unsupported_effort_warning(agent: &str, level: Effort, hint: Option<&str>) -> String {
    let how = hint.map(|h| format!(" {h}")).unwrap_or_else(|| {
        format!(
            " If this agent can take a reasoning setting, declare it under \
             [frameworks.{agent}.effort_map] in .ta/model-profiles.toml."
        )
    });
    format!(
        "effort '{level}' is not supported by agent '{agent}' (no mapping for that level), so it \
         was NOT applied and the agent's default reasoning effort will run.{how}"
    )
}

// ── Rendering and applying ───────────────────────────────────────────────────

struct Rendered {
    groups: Vec<(Via, Vec<String>)>,
    env: Vec<(String, String)>,
    warnings: Vec<String>,
}

fn render(
    args: &[String],
    config: &[String],
    env: &BTreeMap<String, String>,
    value: &str,
) -> Rendered {
    let sub = |s: &str| s.replace("{value}", value);
    let mut groups = Vec::new();
    if !args.is_empty() {
        groups.push((Via::Arg, args.iter().map(|a| sub(a)).collect()));
    }
    if !config.is_empty() {
        groups.push((Via::Config, config.iter().map(|a| sub(a)).collect()));
    }
    let mut kept = Vec::new();
    let mut warnings = Vec::new();
    for (k, v) in env {
        if is_denied_env(k) {
            warnings.push(format!(
                "env var '{k}' from a model/effort mapping is on the sandbox deny list and would \
                 be removed before the agent starts; it was NOT set. Use a different variable \
                 or an args/config delivery."
            ));
        } else {
            kept.push((k.clone(), sub(v)));
        }
    }
    Rendered {
        groups,
        env: kept,
        warnings,
    }
}

/// Outcome of resolving a launch: what was requested, what concrete values it
/// became for this agent, how they are delivered, and everything that was NOT
/// applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelEffort {
    pub agent: String,
    /// Profile that supplied the tier or effort, when one did.
    pub profile: Option<String>,
    pub tier: Option<Tier>,
    /// Which layer chose the tier: `cli`, `plan`, `project`, `user`.
    pub tier_source: Option<&'static str>,
    pub effort: Effort,
    pub effort_source: Option<&'static str>,
    /// Concrete model id passed to the agent (`None`: the agent's default runs).
    pub model: Option<String>,
    /// `cli` for `--model`, else the layer that chose the tier.
    pub model_source: Option<&'static str>,
    /// `arg`, `config`, `env` (joined with `+`), or `none`.
    pub model_via: String,
    pub effort_applied: bool,
    /// `arg`, `config`, `env` (joined with `+`), `unsupported`, or `none` (default effort).
    pub effort_via: String,
    arg_groups: Vec<(Via, Vec<String>)>,
    /// Environment variables to set on the agent process.
    pub env: BTreeMap<String, String>,
    /// Loud, actionable messages for everything requested but not applied.
    pub warnings: Vec<String>,
}

impl ResolvedModelEffort {
    fn absorb_model(
        &mut self,
        _target: &AgentTarget<'_>,
        r: Rendered,
        model: String,
        source: &'static str,
    ) {
        self.warnings.extend(r.warnings);
        if r.groups.is_empty() && r.env.is_empty() {
            // Every delivery was refused above; the warning says why.
            return;
        }
        self.model_via = via_label(&r.groups, !r.env.is_empty());
        self.model = Some(model);
        self.model_source = Some(source);
        self.arg_groups.extend(r.groups);
        self.env.extend(r.env);
    }

    fn absorb_effort(&mut self, _target: &AgentTarget<'_>, r: Rendered) {
        self.warnings.extend(r.warnings);
        if r.groups.is_empty() && r.env.is_empty() {
            self.effort_via = "unsupported".to_string();
            return;
        }
        self.effort_via = via_label(&r.groups, !r.env.is_empty());
        self.effort_applied = true;
        self.arg_groups.extend(r.groups);
        self.env.extend(r.env);
    }

    /// Whether anything is passed to the agent.
    pub fn is_noop(&self) -> bool {
        self.arg_groups.is_empty() && self.env.is_empty()
    }

    /// Add the resolved arguments to an agent argument template, and the
    /// resolved env vars to its environment. A flag the template already
    /// carries (for example a manifest's own `--model x`) is replaced in
    /// place instead of duplicated; otherwise the arguments go immediately
    /// before the first `{prompt}` entry (or at the end when there is none).
    /// Explicit manifest env of the same name is overridden: the resolved
    /// value is what the user asked for on this run.
    pub fn apply(&self, args_template: &mut Vec<String>, env: &mut HashMap<String, String>) {
        for (_, group) in &self.arg_groups {
            inject_args(args_template, group);
        }
        for (k, v) in &self.env {
            env.insert(k.clone(), v.clone());
        }
    }

    /// The one-line launch record.
    pub fn launch_line(&self) -> String {
        format!(
            "agent={} tier={} effort={} model={} effort_applied={} via={} model_via={}",
            self.agent,
            self.tier.map(Tier::as_str).unwrap_or("none"),
            self.effort,
            self.model.as_deref().unwrap_or("default"),
            self.effort_applied,
            self.effort_via,
            self.model_via,
        )
    }
}

fn via_label(groups: &[(Via, Vec<String>)], has_env: bool) -> String {
    let mut parts: Vec<&str> = groups.iter().map(|(v, _)| v.label()).collect();
    if has_env {
        parts.push(Via::Env.label());
    }
    parts.dedup();
    parts.join("+")
}

/// Insert `group` (a rendered flag template such as `["--model", "claude-x"]`
/// or `["-c", "key=\"v\""]`) into `args`: replace the value of the same flag if
/// `args` already has it, else insert before the first `{prompt}` entry.
fn inject_args(args: &mut Vec<String>, group: &[String]) {
    let Some(flag) = group.first().filter(|f| f.starts_with('-')) else {
        // Not flag-shaped (a bare subcommand/positional): append before the prompt.
        insert_before_prompt(args, group);
        return;
    };
    if group.len() == 1 {
        if !args.contains(flag) {
            insert_before_prompt(args, group);
        }
        return;
    }
    // `-c key=value` style: the pair is identified by flag + key, not flag alone.
    let key = group[1].split_once('=').map(|(k, _)| format!("{k}="));
    let existing = (0..args.len().saturating_sub(1)).find(|&i| {
        args[i] == *flag
            && match &key {
                Some(k) => args[i + 1].starts_with(k.as_str()),
                None => true,
            }
    });
    match existing {
        Some(i) => {
            for (offset, v) in group[1..].iter().enumerate() {
                if let Some(slot) = args.get_mut(i + 1 + offset) {
                    *slot = v.clone();
                }
            }
        }
        None => insert_before_prompt(args, group),
    }
}

fn insert_before_prompt(args: &mut Vec<String>, group: &[String]) {
    let pos = args
        .iter()
        .position(|a| a.contains("{prompt}"))
        .unwrap_or(args.len());
    for (offset, v) in group.iter().enumerate() {
        args.insert(pos + offset, v.clone());
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(profile: Option<&str>, tier: Option<&str>, effort: Option<&str>) -> Selection {
        Selection {
            profile: profile.map(str::to_string),
            tier: tier.map(str::to_string),
            effort: effort.map(str::to_string),
            model: None,
        }
    }

    fn cfg() -> ModelProfileConfig {
        ModelProfileConfig::shipped_only()
    }

    fn target<'a>(name: &'a str, command: &'a str) -> AgentTarget<'a> {
        AgentTarget {
            name,
            command,
            manifest: None,
        }
    }

    fn resolve(name: &str, command: &str, s: &Selection) -> ResolvedModelEffort {
        cfg().resolve(target(name, command), s, None).unwrap()
    }

    // ── catalog and vocabulary ───────────────────────────────────────

    #[test]
    fn shipped_catalog_parses_and_is_valid() {
        let c = ModelProfilesFile::shipped();
        for key in ["claude", "codex", "gemini", "agy", "ta-agent-ollama"] {
            assert!(c.frameworks.contains_key(key), "catalog lacks {key}");
        }
    }

    #[test]
    fn builtin_profiles_match_the_documented_set() {
        let p = builtin_profiles();
        assert_eq!(
            p.keys().cloned().collect::<Vec<_>>(),
            ["deep", "light", "local", "max", "standard"]
        );
        assert_eq!(
            p["deep"],
            Profile {
                tier: Tier::Standard,
                effort: Effort::Xhigh
            }
        );
        assert_eq!(
            p["max"],
            Profile {
                tier: Tier::Max,
                effort: Effort::Max
            }
        );
    }

    #[test]
    fn unknown_names_error_with_the_valid_values() {
        let e = cfg()
            .resolve(
                target("claude-code", "claude"),
                &sel(Some("turbo"), None, None),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown profile 'turbo'"), "{e}");
        assert!(e.contains("deep, light, local, max, standard"), "{e}");
        assert!(e.contains("--profile") || e.contains("command line"), "{e}");

        let e = cfg()
            .resolve(
                target("claude-code", "claude"),
                &sel(None, Some("huge"), None),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown tier 'huge'"), "{e}");
        assert!(e.contains("local, light, standard, max"), "{e}");

        let e = cfg()
            .resolve(
                target("claude-code", "claude"),
                &sel(None, None, Some("ludicrous")),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown effort 'ludicrous'"), "{e}");
        assert!(e.contains("default, low, medium, high, xhigh, max"), "{e}");
    }

    // ── per-framework resolution ─────────────────────────────────────

    #[test]
    fn claude_deep_is_sonnet_with_xhigh_via_env() {
        let r = resolve("claude-code", "claude", &sel(Some("deep"), None, None));
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(r.effort, Effort::Xhigh);
        assert!(r.effort_applied);
        assert_eq!(r.effort_via, "env");
        assert_eq!(r.model_via, "arg");
        assert_eq!(r.env["CLAUDE_CODE_EFFORT_LEVEL"], "xhigh");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let mut args = vec!["{prompt}".to_string()];
        let mut env = HashMap::new();
        r.apply(&mut args, &mut env);
        assert_eq!(args, ["--model", "claude-sonnet-5-5", "{prompt}"]);
        assert_eq!(env["CLAUDE_CODE_EFFORT_LEVEL"], "xhigh");
    }

    #[test]
    fn claude_light_and_max_profiles() {
        let light = resolve("claude-code", "claude", &sel(Some("light"), None, None));
        assert_eq!(light.model.as_deref(), Some("claude-haiku-5-5"));
        assert!(!light.effort_applied);
        assert_eq!(light.effort_via, "none");
        assert!(light.warnings.is_empty());
        let max = resolve("claude-code", "claude", &sel(Some("max"), None, None));
        assert_eq!(max.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(max.env["CLAUDE_CODE_EFFORT_LEVEL"], "max");
    }

    #[test]
    fn codex_maps_models_by_arg_and_effort_by_config_override() {
        let r = resolve("codex", "codex", &sel(Some("deep"), None, None));
        assert_eq!(r.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(r.effort_via, "config");
        assert!(r.env.is_empty());
        let mut args = vec![
            "--approval-mode".to_string(),
            "full-auto".to_string(),
            "{prompt}".to_string(),
        ];
        r.apply(&mut args, &mut HashMap::new());
        assert_eq!(
            args,
            [
                "--approval-mode",
                "full-auto",
                "--model",
                "gpt-6.1-sol",
                "-c",
                "model_reasoning_effort=\"xhigh\"",
                "{prompt}"
            ]
        );
        let light = resolve("codex", "codex", &sel(Some("light"), None, None));
        assert_eq!(light.model.as_deref(), Some("gpt-6-luna"));
        let max = resolve("codex", "codex", &sel(Some("max"), None, None));
        assert_eq!(max.model.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn gemini_has_model_aliases_but_no_effort() {
        let r = resolve("gemini", "gemini", &sel(Some("deep"), None, None));
        assert_eq!(r.model.as_deref(), Some("flash"));
        assert!(!r.effort_applied);
        assert_eq!(r.effort_via, "unsupported");
        // Unsupported effort is reported, never silently dropped.
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(
            r.warnings[0].contains("effort 'xhigh' is not supported by agent 'gemini'"),
            "{}",
            r.warnings[0]
        );
        assert!(r.warnings[0].contains("NOT applied"));
        assert!(r.warnings[0].contains("settings.json"), "{}", r.warnings[0]);
    }

    #[test]
    fn antigravity_passes_effort_by_flag_and_leaves_models_unmapped() {
        let r = resolve("antigravity", "agy", &sel(None, None, Some("high")));
        assert_eq!(r.effort_via, "arg");
        let mut args = vec!["-p".to_string(), "{prompt}".to_string()];
        r.apply(&mut args, &mut HashMap::new());
        assert_eq!(args, ["-p", "--effort", "high", "{prompt}"]);
        // No tier -> model ids are verified only by `agy models`: warn, with the hint.
        let r = resolve("antigravity", "agy", &sel(None, Some("max"), None));
        assert_eq!(r.model, None);
        assert!(r.warnings[0].contains("tier 'max' has no model mapped"));
        assert!(r.warnings[0].contains("agy models"), "{}", r.warnings[0]);
    }

    #[test]
    fn ollama_effort_is_thinking_mode_and_tiers_are_user_local() {
        let low = resolve(
            "qwen3.5-9b",
            "ta-agent-ollama",
            &sel(None, None, Some("low")),
        );
        assert_eq!(low.effort_via, "arg");
        let mut args = vec![
            "--model".to_string(),
            "qwen3.5:9b".to_string(),
            "--thinking-mode".to_string(),
            "true".to_string(),
        ];
        low.apply(&mut args, &mut HashMap::new());
        // The manifest's own `--thinking-mode true` is replaced, not duplicated.
        assert_eq!(args, ["--model", "qwen3.5:9b", "--thinking-mode", "false"]);
        let high = resolve("ollama", "ta-agent-ollama", &sel(None, None, Some("max")));
        let mut args = Vec::new();
        high.apply(&mut args, &mut HashMap::new());
        assert_eq!(args, ["--thinking-mode", "true"]);
        // A local tier has no shipped mapping: the warning says how to set it.
        let r = resolve("ollama", "ta-agent-ollama", &sel(Some("local"), None, None));
        assert_eq!(r.model, None);
        assert!(r.warnings[0].contains("[frameworks.ollama.model_map.tiers]"));
    }

    #[test]
    fn framework_with_no_mapping_warns_for_tier_and_effort() {
        let r = resolve("mystery", "mystery-agent", &sel(Some("deep"), None, None));
        assert!(r.is_noop());
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("tier 'standard' has no model mapped"));
        assert!(r.warnings[1].contains("effort 'xhigh' is not supported"));
        assert!(!r.effort_applied);
    }

    #[test]
    fn default_effort_and_no_selection_pass_nothing_and_do_not_warn() {
        let r = resolve("mystery", "mystery-agent", &Selection::default());
        assert!(r.is_noop());
        assert!(r.warnings.is_empty());
        let r = resolve(
            "mystery",
            "mystery-agent",
            &sel(None, None, Some("default")),
        );
        assert!(r.warnings.is_empty());
        assert!(!r.effort_applied);
        assert_eq!(r.effort_via, "none");
    }

    // ── precedence ───────────────────────────────────────────────────

    #[test]
    fn explicit_model_beats_the_tier_mapping() {
        let mut s = sel(Some("deep"), None, None);
        s.model = Some("claude-opus-5-5".to_string());
        let r = resolve("claude-code", "claude", &s);
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.model_source, Some("cli"));
        // The profile's effort still applies.
        assert_eq!(r.env["CLAUDE_CODE_EFFORT_LEVEL"], "xhigh");
    }

    #[test]
    fn explicit_model_on_a_framework_without_a_model_map_warns() {
        let s = Selection {
            model: Some("whatever".to_string()),
            ..Selection::default()
        };
        let r = resolve("mystery", "mystery-agent", &s);
        assert_eq!(r.model, None);
        assert!(r.warnings[0].contains("model 'whatever' was requested"));
    }

    #[test]
    fn explicit_effort_beats_the_profile_in_the_same_layer() {
        let r = resolve(
            "claude-code",
            "claude",
            &sel(Some("deep"), None, Some("low")),
        );
        assert_eq!(r.effort, Effort::Low);
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5"));
        let r = resolve(
            "claude-code",
            "claude",
            &sel(Some("deep"), Some("max"), None),
        );
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.effort, Effort::Xhigh);
    }

    fn with_project(toml: &str) -> ModelProfileConfig {
        let mut c = ModelProfileConfig::shipped_only();
        c.project = Some((
            PathBuf::from(".ta/model-profiles.toml"),
            ModelProfilesFile::parse(toml, ".ta/model-profiles.toml").unwrap(),
        ));
        c
    }

    #[test]
    fn precedence_cli_then_plan_then_project_then_shipped() {
        let c = with_project(
            "[defaults]\nprofile = \"light\"\n\n[profiles.review]\ntier = \"standard\"\neffort = \"high\"\n",
        );
        let t = target("claude-code", "claude");
        // Project default only.
        let r = c.resolve(t, &Selection::default(), None).unwrap();
        assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(r.tier_source, Some("project"));
        // Plan beats project.
        let plan = sel(Some("deep"), None, None);
        let r = c
            .resolve(t, &Selection::default(), Some(("PLAN.md phase 1", &plan)))
            .unwrap();
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(r.tier_source, Some("plan"));
        assert_eq!(r.effort, Effort::Xhigh);
        // CLI beats plan, per axis: --effort low keeps the plan's tier.
        let r = c
            .resolve(
                t,
                &sel(None, None, Some("low")),
                Some(("PLAN.md phase 1", &plan)),
            )
            .unwrap();
        assert_eq!(r.effort, Effort::Low);
        assert_eq!(r.effort_source, Some("cli"));
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(r.tier_source, Some("plan"));
        // A project-defined profile is selectable from the CLI.
        let r = c
            .resolve(t, &sel(Some("review"), None, None), None)
            .unwrap();
        assert_eq!(r.profile.as_deref(), Some("review"));
        assert_eq!(r.env["CLAUDE_CODE_EFFORT_LEVEL"], "high");
    }

    #[test]
    fn project_mapping_overrides_manifest_which_overrides_shipped() {
        let manifest = FrameworkMapping {
            model_map: Some(ModelMap {
                tiers: BTreeMap::from([("standard".to_string(), "from-manifest".to_string())]),
                ..Default::default()
            }),
            effort_map: None,
        };
        let t = AgentTarget {
            name: "claude-code",
            command: "claude",
            manifest: Some(&manifest),
        };
        let s = sel(None, Some("standard"), None);
        // Manifest beats shipped; the shipped delivery (`--model`) is inherited.
        let r = cfg().resolve(t, &s, None).unwrap();
        assert_eq!(r.model.as_deref(), Some("from-manifest"));
        assert_eq!(r.model_via, "arg");
        // Project beats manifest.
        let c =
            with_project("[frameworks.claude-code.model_map.tiers]\nstandard = \"from-project\"\n");
        let r = c.resolve(t, &s, None).unwrap();
        assert_eq!(r.model.as_deref(), Some("from-project"));
        // Other tiers still come from the shipped catalog.
        let r = c.resolve(t, &sel(None, Some("max"), None), None).unwrap();
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn project_file_can_add_an_ollama_local_tier() {
        let c =
            with_project("[frameworks.ta-agent-ollama.model_map.tiers]\nlocal = \"qwen3.5:9b\"\n");
        let r = c
            .resolve(
                target("ollama", "ta-agent-ollama"),
                &sel(Some("local"), None, None),
                None,
            )
            .unwrap();
        assert_eq!(r.model.as_deref(), Some("qwen3.5:9b"));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn invalid_override_files_are_loud() {
        let e = ModelProfilesFile::parse("[defaults]\nprofile = [", "x.toml")
            .unwrap_err()
            .to_string();
        assert!(e.contains("x.toml"), "{e}");
        let e = ModelProfilesFile::parse(
            "[frameworks.codex.model_map.tiers]\nhuge = \"m\"\n",
            "x.toml",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("unknown tier 'huge'"), "{e}");
        let e = ModelProfilesFile::parse(
            "[frameworks.codex.effort_map.levels]\ndefault = \"m\"\n",
            "x.toml",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("cannot be mapped"), "{e}");
        // A bad name in a lower layer is reported even when the CLI shadows it.
        let c = with_project("[defaults]\neffort = \"warp\"\n");
        let e = c
            .resolve(
                target("claude-code", "claude"),
                &sel(None, None, Some("low")),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown effort 'warp'"), "{e}");
        assert!(e.contains(".ta/model-profiles.toml"), "{e}");
    }

    #[test]
    fn denied_env_names_are_refused_with_a_warning() {
        let manifest = FrameworkMapping {
            model_map: None,
            effort_map: Some(EffortMap {
                env: BTreeMap::from([("TA_SECRET_KNOB".to_string(), "{value}".to_string())]),
                levels: BTreeMap::from([("high".to_string(), "1".to_string())]),
                ..Default::default()
            }),
        };
        let t = AgentTarget {
            name: "custom",
            command: "custom-agent",
            manifest: Some(&manifest),
        };
        let r = cfg()
            .resolve(t, &sel(None, None, Some("high")), None)
            .unwrap();
        assert!(!r.effort_applied);
        assert_eq!(r.effort_via, "unsupported");
        assert!(r.env.is_empty());
        assert!(r.warnings[0].contains("TA_SECRET_KNOB"), "{:?}", r.warnings);
        assert!(r.warnings[0].contains("deny list"));
    }

    // ── launch line, load, arg injection ─────────────────────────────

    #[test]
    fn launch_line_has_the_documented_fields() {
        let r = resolve("claude-code", "claude", &sel(Some("deep"), None, None));
        assert_eq!(
            r.launch_line(),
            "agent=claude-code tier=standard effort=xhigh model=claude-sonnet-5-5 \
             effort_applied=true via=env model_via=arg"
        );
        let r = resolve("gemini", "gemini", &sel(Some("deep"), None, None));
        assert_eq!(
            r.launch_line(),
            "agent=gemini tier=standard effort=xhigh model=flash \
             effort_applied=false via=unsupported model_via=arg"
        );
    }

    #[test]
    fn missing_override_file_is_none_and_unreadable_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ModelProfilesFile::load(&dir.path().join("nope.toml"))
            .unwrap()
            .is_none());
        // A directory where the file should be: unreadable, not silently skipped.
        let as_dir = dir.path().join("model-profiles.toml");
        std::fs::create_dir(&as_dir).unwrap();
        let e = ModelProfilesFile::load(&as_dir).unwrap_err().to_string();
        assert!(e.contains("cannot read"), "{e}");
    }

    #[test]
    fn load_reads_project_then_user_files() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p.toml");
        let user = dir.path().join("u.toml");
        std::fs::write(&proj, "[defaults]\neffort = \"low\"\n").unwrap();
        std::fs::write(&user, "[profiles.mine]\ntier = \"max\"\n").unwrap();
        let c = ModelProfileConfig::load_from(Some(proj), Some(user)).unwrap();
        assert!(c.profiles().contains_key("mine"));
        let r = c
            .resolve(target("claude-code", "claude"), &Selection::default(), None)
            .unwrap();
        assert_eq!(r.effort, Effort::Low);
        assert_eq!(r.tier, None);
    }

    #[test]
    fn inject_args_replaces_existing_flag_values_and_inserts_before_prompt() {
        let mut a = vec!["--model".into(), "old".into(), "{prompt}".into()];
        inject_args(&mut a, &["--model".to_string(), "new".to_string()]);
        assert_eq!(a, ["--model", "new", "{prompt}"]);
        let mut a = vec!["x".to_string()];
        inject_args(&mut a, &["--flag".to_string()]);
        inject_args(&mut a, &["--flag".to_string()]);
        assert_eq!(a, ["x", "--flag"]);
        // `-c key=value` pairs are keyed by flag + key.
        let mut a: Vec<String> = ["-c", "a=1", "-c", "b=2", "{prompt}"]
            .into_iter()
            .map(String::from)
            .collect();
        inject_args(&mut a, &["-c".to_string(), "b=9".to_string()]);
        assert_eq!(a, ["-c", "a=1", "-c", "b=9", "{prompt}"]);
        inject_args(&mut a, &["-c".to_string(), "c=3".to_string()]);
        assert_eq!(a, ["-c", "a=1", "-c", "b=9", "-c", "c=3", "{prompt}"]);
    }

    #[test]
    fn command_basename_strips_dirs_and_exe() {
        assert_eq!(command_basename("/opt/bin/claude"), "claude");
        assert_eq!(command_basename("C:\\tools\\codex.exe"), "codex");
        assert_eq!(command_basename("agy"), "agy");
    }
}
