// rules/load.rs: find, verify and compile the rules for a project (v0.17.12.4).
//
// Sources, strictest first:
//   1. the org floor    `<user state>/org/floor.toml` (+ `.sig`, `floor.pub`)
//   2. legacy denies    `.ta/constitution.toml` and `[actions.*] policy = "block"`
//                       in `.ta/workflow.toml` (until migrated into the owner file)
//   3. a role preset    `.ta/rules-presets/<role>.toml` (tighten only)
//   4. the owner file   `.ta/rules.toml` + `.ta/rules.toml.sig`
//   5. the posture      shipped data
//
// Everything is re-read and re-verified on every call. The files are small and
// an Ed25519 verify is microseconds, so there is no cache to go stale or to
// poison.
//
// Fail closed. A file that does not parse, a signature that does not verify, a
// missing trust anchor or a rule that does not validate yields NO table. The
// caller then gets a deny with the exact problems (`Loaded::decide`). The one
// exception is an attended file apply, which proceeds with a loud warning: a
// person at the keyboard running `ta draft apply` is not the threat this guards
// against, and refusing would lock them out of repairing the rules themselves.
// Protected paths stay refused there too.
//
// A project with NO owner file is not an error. It runs on the shipped strict
// posture, which needs no signature because there is nothing to forge.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use ta_credentials::state::{ControlSecret, CustodyOpts, ProjectState, StateLocator};

use super::model::{DocKind, Effect, Layer, Rule, RulesDoc, When};
use super::paths;
use super::postures::{self, PostureData};
use super::signing::{self, Domain, SIG_SUFFIX};
use super::table::{compile, CompiledRules, Decision, Request, Sources};

/// Owner rules file, relative to the project root.
pub const OWNER_FILE: &str = ".ta/rules.toml";
/// Directory of role presets, relative to the project root.
pub const PRESET_DIR: &str = ".ta/rules-presets";
/// Environment override for the org floor directory.
pub const ORG_DIR_ENV: &str = "TA_ORG_RULES_DIR";

const ANCHOR_FILE: &str = "owner.pub";
const LAST_SIGNED: &str = "last-signed.toml";
const LAST_SIGNED_POSTURE: &str = "last-signed-posture.toml";

/// Whether the rules are in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No owner file: the shipped posture applies.
    Default,
    /// The owner file verified against the trusted key.
    Verified { key_id: String },
    /// Something is wrong and nothing runs unattended. See `problems`.
    Invalid,
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub compiled: Option<CompiledRules>,
    pub status: Status,
    /// Why the table is unusable, one fix-bearing line each.
    pub problems: Vec<String>,
    /// Things worth saying that do not stop the rules.
    pub warnings: Vec<String>,
    pub owner_file_present: bool,
}

impl Loaded {
    pub fn is_usable(&self) -> bool {
        self.compiled.is_some()
    }

    /// Decide, failing closed when there is no usable table.
    pub fn decide(&self, req: &Request<'_>) -> Decision {
        if let Some(c) = &self.compiled {
            return c.decide(req);
        }
        let action = req.action.trim().to_ascii_lowercase();
        let problems = self.problems.join(" ");
        if action == "apply" && req.when == When::Attended {
            // See the module note: a person running apply is not blocked from
            // repairing the rules, but unsafe paths are still refused.
            if let Some(path) = req.path {
                if let Err(p) = paths::normalize(path, &[]) {
                    if p.is_unsafe() {
                        return Decision {
                            effect: Effect::Deny,
                            layer: Layer::BuiltIn,
                            rule: "built-in: protected path".into(),
                            reason: format!("'{path}': {}.", p.describe()),
                            fix: None,
                            environment: None,
                        };
                    }
                }
            }
            return Decision {
                effect: Effect::Ask,
                layer: Layer::BuiltIn,
                rule: "built-in: rules are invalid (failing closed for everything but an attended apply)".into(),
                reason: format!("WARNING: your rules are not in effect. {problems}"),
                fix: None,
                environment: None,
            };
        }
        Decision {
            effect: Effect::Deny,
            layer: Layer::BuiltIn,
            rule: "built-in: rules are invalid (failing closed)".into(),
            reason: format!("Your rules could not be verified, so nothing runs. {problems}"),
            fix: Some(
                "Fix the problem above, then retry. `ta rules status` shows the same list.".into(),
            ),
            environment: None,
        }
    }
}

enum StateSel {
    /// Find the project's state directory from the environment when needed.
    Lazy,
    /// Use this directory (tests, and callers that already opened the state).
    Dir(Option<PathBuf>),
}

pub struct RulesLoader {
    project_root: PathBuf,
    state: StateSel,
    org_dir: Option<PathBuf>,
}

/// Role names are file names. Refuse anything that could leave the directory.
pub fn valid_role(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 64
        && role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn org_dir_default() -> PathBuf {
    if let Some(d) = std::env::var_os(ORG_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    ta_credentials::human::user_state_dir().join("org")
}

impl RulesLoader {
    /// The normal loader: state directory and org floor from the environment.
    pub fn for_project(project_root: impl Into<PathBuf>) -> Self {
        RulesLoader {
            project_root: project_root.into(),
            state: StateSel::Lazy,
            org_dir: Some(org_dir_default()),
        }
    }

    /// Explicit directories (tests).
    pub fn with_dirs(
        project_root: impl Into<PathBuf>,
        rules_state_dir: Option<PathBuf>,
        org_dir: Option<PathBuf>,
    ) -> Self {
        RulesLoader {
            project_root: project_root.into(),
            state: StateSel::Dir(rules_state_dir),
            org_dir,
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn owner_path(&self) -> PathBuf {
        self.project_root.join(OWNER_FILE)
    }

    pub fn sig_path(&self) -> PathBuf {
        PathBuf::from(format!("{}{}", self.owner_path().display(), SIG_SUFFIX))
    }

    /// The state-directory `rules/` folder, when one exists for this project.
    pub fn rules_state_dir(&self) -> Result<Option<PathBuf>, String> {
        match &self.state {
            StateSel::Dir(d) => Ok(d.clone()),
            StateSel::Lazy => {
                match ProjectState::locate(&self.project_root, &StateLocator::from_env()) {
                    Ok(Some(ps)) => Ok(Some(ps.rules_dir())),
                    Ok(None) => Ok(None),
                    Err(e) => Err(format!(
                        "could not locate the TA state directory for {}: {e}",
                        self.project_root.display()
                    )),
                }
            }
        }
    }

    fn read_anchor(&self, problems: &mut Vec<String>) -> Option<String> {
        match self.rules_state_dir() {
            Ok(Some(d)) => std::fs::read_to_string(d.join(ANCHOR_FILE))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            Ok(None) => None,
            Err(e) => {
                problems.push(e);
                None
            }
        }
    }

    /// Snapshot of what was last signed: (rules text, posture name, posture text).
    pub fn last_signed(&self) -> Option<(String, String, String)> {
        let dir = self.rules_state_dir().ok()??;
        let text = std::fs::read_to_string(dir.join(LAST_SIGNED)).ok()?;
        let posture_text = std::fs::read_to_string(dir.join(LAST_SIGNED_POSTURE)).ok()?;
        let name = RulesDoc::parse(&text, DocKind::Owner, "last signed rules")
            .ok()
            .and_then(|d| d.posture)
            .unwrap_or_else(|| postures::DEFAULT.to_string());
        Some((text, name, posture_text))
    }

    /// Record what was just signed, for `ta rules diff` and drift notices.
    pub fn save_last_signed(&self, text: &str, posture_text: &str) -> Result<(), String> {
        let Some(dir) = self.rules_state_dir()? else {
            return Err("there is no TA state directory for this project yet".into());
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        for (name, body) in [(LAST_SIGNED, text), (LAST_SIGNED_POSTURE, posture_text)] {
            ta_credentials::state::write_state_file(&dir.join(name), body)
                .map_err(|e| format!("could not write {}: {e}", dir.join(name).display()))?;
        }
        Ok(())
    }

    /// Load and compile for `role` (a preset name) or the base table.
    pub fn load(&self, role: Option<&str>) -> Loaded {
        let mut problems: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        if let Some(r) = role {
            if !valid_role(r) {
                problems.push(format!(
                    "role \"{r}\" is not a valid role name (letters, digits, - and _). Fix the \
                     --role value."
                ));
            }
        }

        // 1. Org floor.
        let floor = self.load_floor(&mut problems);

        // 4. Owner file.
        let owner_path = self.owner_path();
        let owner_file_present = owner_path.exists();
        let mut owner_doc: Option<RulesDoc> = None;
        let mut verified: Option<String> = None;
        if owner_file_present {
            match std::fs::read_to_string(&owner_path) {
                Err(e) => problems.push(format!(
                    "could not read {}: {e}. Check the file's permissions.",
                    owner_path.display()
                )),
                Ok(text) => {
                    let anchor = self.read_anchor(&mut problems);
                    let sig = std::fs::read_to_string(self.sig_path()).ok();
                    match signing::verify(Domain::Owner, &text, sig.as_deref(), anchor.as_deref()) {
                        Err(p) => problems.push(p.describe(OWNER_FILE)),
                        Ok(v) => {
                            verified = Some(v.key_id.clone());
                            match RulesDoc::parse(&text, DocKind::Owner, "owner rules") {
                                Ok(d) => {
                                    if let Some(w) =
                                        posture_drift(&v.sig.posture, &v.sig.posture_digest)
                                    {
                                        warnings.push(w);
                                    }
                                    owner_doc = Some(d)
                                }
                                Err(mut es) => problems.append(&mut es),
                            }
                        }
                    }
                }
            }
        }

        // Posture.
        let posture_name = owner_doc
            .as_ref()
            .and_then(|d| d.posture.clone())
            .unwrap_or_else(|| postures::DEFAULT.to_string());
        let posture = match postures::load(&posture_name) {
            Ok(p) => Some(p),
            Err(e) => {
                problems.push(e);
                None
            }
        };

        // 2. Legacy denies.
        let migrated = owner_doc
            .as_ref()
            .map(|d| d.migrated_from.clone())
            .unwrap_or_default();
        let legacy = match legacy_rules(&self.project_root, &migrated) {
            Ok(r) => r,
            Err(e) => {
                problems.push(e);
                Vec::new()
            }
        };

        // 3. Preset for the role.
        let mut presets = BTreeMap::new();
        if let Some(r) = role.filter(|r| valid_role(r)) {
            let p = self.project_root.join(PRESET_DIR).join(format!("{r}.toml"));
            if p.exists() {
                match std::fs::read_to_string(&p) {
                    Err(e) => problems.push(format!("could not read {}: {e}", p.display())),
                    Ok(text) => {
                        match RulesDoc::parse(&text, DocKind::Preset, &format!("preset {r}")) {
                            Ok(d) => {
                                presets.insert(r.to_string(), d);
                            }
                            Err(mut es) => problems.append(&mut es),
                        }
                    }
                }
            }
        }

        let (Some(posture), true) = (posture, problems.is_empty()) else {
            return Loaded {
                compiled: None,
                status: Status::Invalid,
                problems,
                warnings,
                owner_file_present,
            };
        };
        let sources = Sources {
            floor,
            owner: owner_doc,
            presets,
            legacy,
            posture,
        };
        match compile(&sources, role) {
            Ok(c) => Loaded {
                compiled: Some(c),
                status: match verified {
                    Some(key_id) => Status::Verified { key_id },
                    None => Status::Default,
                },
                problems,
                warnings,
                owner_file_present,
            },
            Err(mut es) => {
                problems.append(&mut es);
                Loaded {
                    compiled: None,
                    status: Status::Invalid,
                    problems,
                    warnings,
                    owner_file_present,
                }
            }
        }
    }

    fn load_floor(&self, problems: &mut Vec<String>) -> Option<RulesDoc> {
        let dir = self.org_dir.as_ref()?;
        let floor = dir.join("floor.toml");
        if !floor.exists() {
            return None;
        }
        let text = match std::fs::read_to_string(&floor) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!(
                    "could not read the organization floor {}: {e}. Ask your administrator.",
                    floor.display()
                ));
                return None;
            }
        };
        let sig = std::fs::read_to_string(dir.join(format!("floor.toml{SIG_SUFFIX}"))).ok();
        let anchor = std::fs::read_to_string(dir.join("floor.pub"))
            .ok()
            .map(|s| s.trim().to_string());
        if let Err(p) = signing::verify(Domain::Floor, &text, sig.as_deref(), anchor.as_deref()) {
            problems.push(p.describe("the organization floor"));
            return None;
        }
        match RulesDoc::parse(&text, DocKind::Floor, "org floor") {
            Ok(d) => Some(d),
            Err(mut es) => {
                problems.append(&mut es);
                None
            }
        }
    }

    /// Roles that have a preset file, and any that do not validate.
    pub fn presets(&self) -> Vec<(String, Result<(), Vec<String>>)> {
        let dir = self.project_root.join(PRESET_DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            let Some(role) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let result = if !valid_role(role) {
                Err(vec![format!(
                    "{} is not a valid role file name (letters, digits, - and _).",
                    path.display()
                )])
            } else {
                match std::fs::read_to_string(&path) {
                    Err(e) => Err(vec![format!("could not read {}: {e}", path.display())]),
                    Ok(t) => {
                        RulesDoc::parse(&t, DocKind::Preset, &format!("preset {role}")).map(|_| ())
                    }
                }
            };
            out.push((role.to_string(), result));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Compile an arbitrary owner text (not necessarily signed): used by
    /// `ta rules sign` and `diff` to validate and compare candidate files.
    /// Floor and legacy sources are the same as `load`.
    pub fn compile_text(
        &self,
        owner_text: &str,
        posture_override: Option<PostureData>,
    ) -> Result<CompiledRules, Vec<String>> {
        let mut problems = Vec::new();
        let floor = self.load_floor(&mut problems);
        let owner = RulesDoc::parse(owner_text, DocKind::Owner, "owner rules")?;
        let posture = match posture_override {
            Some(p) => p,
            None => postures::load(owner.posture.as_deref().unwrap_or(postures::DEFAULT))
                .map_err(|e| vec![e])?,
        };
        let legacy = legacy_rules(&self.project_root, &owner.migrated_from).map_err(|e| vec![e])?;
        if !problems.is_empty() {
            return Err(problems);
        }
        compile(
            &Sources {
                floor,
                owner: Some(owner),
                presets: BTreeMap::new(),
                legacy,
                posture,
            },
            None,
        )
    }
}

fn posture_drift(name: &str, signed_digest: &str) -> Option<String> {
    let now = postures::load(name).ok()?;
    (now.digest != signed_digest).then(|| {
        format!(
            "the shipped {name} posture changed since you last signed your rules (this TA ships \
             revision {}). The new table is in effect. Run `ta rules diff` to see exactly what \
             changed, then `ta rules sign` to accept it.",
            now.revision
        )
    })
}

// ── Legacy sources ──────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct LegacyConstitution {
    #[serde(default)]
    rules: LegacyRuleSets,
}

#[derive(Deserialize, Default)]
struct LegacyRuleSets {
    #[serde(default)]
    block: Vec<LegacyBlock>,
}

#[derive(Deserialize)]
struct LegacyBlock {
    action_type: String,
    condition: String,
    #[serde(default)]
    message: String,
}

fn legacy_deny(action: &str, when: Option<When>, reason: String, label: String) -> Rule {
    Rule {
        effect: Effect::Deny,
        action: action.trim().to_ascii_lowercase(),
        envs: None,
        when,
        paths: vec![],
        reason: Some(reason),
        layer: Layer::Legacy,
        label,
    }
}

/// Deny rules from the old files. They can only tighten. A file already
/// folded into the owner file (`migrated_from`) is not read again.
pub fn legacy_rules(project_root: &Path, migrated_from: &[String]) -> Result<Vec<Rule>, String> {
    let mut out = Vec::new();
    let ta = project_root.join(".ta");
    let constitution = ta.join("constitution.toml");
    if constitution.exists() && !migrated_from.iter().any(|m| m == "constitution.toml") {
        let text = std::fs::read_to_string(&constitution)
            .map_err(|e| format!("could not read {}: {e}", constitution.display()))?;
        let parsed: LegacyConstitution = toml::from_str(&text).map_err(|e| {
            format!(
                "constitution is unreadable, so TA fails closed: {} does not parse: {e}. Fix the \
                 line named, or run `ta rules migrate`.",
                constitution.display()
            )
        })?;
        for (i, b) in parsed.rules.block.iter().enumerate() {
            let label = format!("legacy constitution.toml [[rules.block]] #{}", i + 1);
            match b.condition.as_str() {
                "always" => out.push(legacy_deny(&b.action_type, None, b.message.clone(), label)),
                "policy_is_not_review" => out.push(legacy_deny(
                    &b.action_type,
                    Some(When::Unattended),
                    b.message.clone(),
                    label,
                )),
                _ => {}
            }
        }
    }
    let workflow = ta.join("workflow.toml");
    if workflow.exists() && !migrated_from.iter().any(|m| m == "workflow.toml") {
        let text = std::fs::read_to_string(&workflow)
            .map_err(|e| format!("could not read {}: {e}", workflow.display()))?;
        let value: toml::Value = toml::from_str(&text).map_err(|e| {
            format!(
                "configuration is unreadable, so TA fails closed: {} does not parse: {e}. Fix \
                 the line named.",
                workflow.display()
            )
        })?;
        if let Some(actions) = value.get("actions").and_then(|a| a.as_table()) {
            for (verb, cfg) in actions {
                if cfg.get("policy").and_then(|p| p.as_str()) == Some("block") {
                    out.push(legacy_deny(
                        verb,
                        None,
                        format!("workflow.toml sets [actions.{verb}] policy = \"block\"."),
                        format!("legacy workflow.toml [actions.{verb}]"),
                    ));
                }
            }
        }
    }
    Ok(out)
}

// ── Owner key custody ───────────────────────────────────────────────────────

/// Load the owner's signing seed from the keychain or the owner-only file.
pub fn load_owner_seed(ps: &ProjectState, opts: CustodyOpts) -> Result<Option<[u8; 32]>, String> {
    match ta_credentials::state::load_secret_new(ps, ControlSecret::RulesOwnerKey, opts) {
        None => Ok(None),
        Some(s) => signing::seed_from_b64(&s.value).map(Some),
    }
}

/// Create the owner key, store the private half outside the tree and write the
/// public half as this machine's trust anchor. Refuses to replace an existing key.
pub fn create_owner_key(ps: &ProjectState, opts: CustodyOpts) -> Result<(String, String), String> {
    if load_owner_seed(ps, opts)?.is_some() {
        return Err(
            "an owner key already exists for this project. Use `ta rules status` to see it; \
                    to rotate it, remove it deliberately first with `ta rules rotate-key`."
                .into(),
        );
    }
    let seed = signing::generate_seed();
    let outcome = ta_credentials::state::store_secret(
        ps,
        ControlSecret::RulesOwnerKey,
        &signing::seed_to_b64(&seed),
        opts,
    )
    .map_err(|e| e.to_string())?;
    let public = signing::public_to_b64(&signing::public_of(&seed));
    write_anchor(ps, &public)?;
    Ok((public, outcome.custody.describe()))
}

/// Trust a public key on this machine (the owner's key, from another machine).
pub fn write_anchor(ps: &ProjectState, public_b64: &str) -> Result<(), String> {
    signing::public_from_b64(public_b64)?;
    let dir = ps.rules_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    ta_credentials::state::write_state_file(&dir.join(ANCHOR_FILE), public_b64.trim())
        .map_err(|e| format!("could not write the trust anchor in {}: {e}", dir.display()))
}

/// The key this machine trusts, if any.
pub fn read_anchor(ps: &ProjectState) -> Option<String> {
    std::fs::read_to_string(ps.rules_dir().join(ANCHOR_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Remove the owner key (private half and anchor). Deliberate rotation only.
pub fn remove_owner_key(ps: &ProjectState, opts: CustodyOpts) -> bool {
    let removed = ta_credentials::state::remove_secret_new(ps, ControlSecret::RulesOwnerKey, opts);
    let _ = std::fs::remove_file(ps.rules_dir().join(ANCHOR_FILE));
    removed
}

/// Sign the owner file in the tree: validate, write the signature file and
/// snapshot what was signed. Returns the key id.
pub fn sign_owner_file(
    loader: &RulesLoader,
    seed: &[u8; 32],
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, Vec<String>> {
    let doc = RulesDoc::parse(text, DocKind::Owner, "owner rules")?;
    let posture_name = doc
        .posture
        .clone()
        .unwrap_or_else(|| postures::DEFAULT.to_string());
    let posture = postures::load(&posture_name).map_err(|e| vec![e])?;
    // A file that does not compile (a preset-independent check) is never signed.
    loader.compile_text(text, Some(posture.clone()))?;
    let sig = signing::sign(
        seed,
        Domain::Owner,
        text,
        &posture.name,
        &posture.digest,
        now,
    );
    let json = serde_json::to_string_pretty(&sig).map_err(|e| vec![e.to_string()])?;
    std::fs::write(loader.sig_path(), format!("{json}\n")).map_err(|e| {
        vec![format!(
            "could not write {}: {e}. Check that the .ta directory is writable.",
            loader.sig_path().display()
        )]
    })?;
    let shipped = postures::shipped(&posture.name).unwrap_or_default();
    if let Err(e) = loader.save_last_signed(text, shipped) {
        tracing::warn!(error = %e, "rules: could not snapshot the signed file; `ta rules diff` will have no baseline");
    }
    Ok(sig.key_id)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub struct Fixture {
        pub root: tempfile::TempDir,
        pub state: tempfile::TempDir,
        pub org: tempfile::TempDir,
        pub seed: [u8; 32],
    }

    impl Fixture {
        pub fn new() -> Self {
            let f = Fixture {
                root: tempfile::tempdir().unwrap(),
                state: tempfile::tempdir().unwrap(),
                org: tempfile::tempdir().unwrap(),
                seed: [3u8; 32],
            };
            std::fs::create_dir_all(f.root.path().join(".ta")).unwrap();
            std::fs::write(
                f.state.path().join(ANCHOR_FILE),
                signing::public_to_b64(&signing::public_of(&f.seed)),
            )
            .unwrap();
            f
        }
        pub fn loader(&self) -> RulesLoader {
            RulesLoader::with_dirs(
                self.root.path(),
                Some(self.state.path().to_path_buf()),
                Some(self.org.path().to_path_buf()),
            )
        }
        pub fn write_owner(&self, text: &str) {
            std::fs::write(self.root.path().join(OWNER_FILE), text).unwrap();
        }
        pub fn sign_owner(&self) {
            let text = std::fs::read_to_string(self.root.path().join(OWNER_FILE)).unwrap();
            let now = chrono::Utc::now();
            sign_owner_file(&self.loader(), &self.seed, &text, now).unwrap();
        }
        pub fn write_floor(&self, text: &str, sign: bool) {
            std::fs::write(self.org.path().join("floor.toml"), text).unwrap();
            let seed = [5u8; 32];
            std::fs::write(
                self.org.path().join("floor.pub"),
                signing::public_to_b64(&signing::public_of(&seed)),
            )
            .unwrap();
            if sign {
                let sig = signing::sign(
                    &seed,
                    Domain::Floor,
                    text,
                    "strict",
                    "d",
                    chrono::Utc::now(),
                );
                std::fs::write(
                    self.org.path().join("floor.toml.sig"),
                    serde_json::to_string(&sig).unwrap(),
                )
                .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::Fixture;
    use super::*;
    use crate::rules::model::When;

    fn email_unattended(l: &Loaded) -> Decision {
        l.decide(&Request::new("email", When::Unattended))
    }

    #[test]
    fn no_owner_file_runs_the_strict_posture_without_a_signature() {
        let f = Fixture::new();
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Default);
        assert!(l.is_usable() && !l.owner_file_present);
        assert!(email_unattended(&l).is_deny());
    }

    #[test]
    fn signed_owner_file_takes_effect() {
        let f = Fixture::new();
        f.write_owner("[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n");
        f.sign_owner();
        let l = f.loader().load(None);
        assert!(
            matches!(l.status, Status::Verified { .. }),
            "{:?}",
            l.problems
        );
        assert!(email_unattended(&l).is_allow());
    }

    #[test]
    fn unsigned_edited_missing_key_and_wrong_key_all_fail_closed() {
        let f = Fixture::new();
        f.write_owner("[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n");
        // Unsigned.
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(l.problems[0].contains("ta rules sign"), "{:?}", l.problems);
        let d = email_unattended(&l);
        assert!(d.is_deny() && d.reason.contains("nothing runs"), "{d:?}");

        // Signed, then edited by an agent to also allow production db access.
        f.sign_owner();
        f.write_owner("[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n[[allow]]\naction = \"db_query\"\n");
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(
            l.problems[0].contains("changed after it was signed"),
            "{:?}",
            l.problems
        );
        assert!(l
            .decide(&Request::new("db_query", When::Unattended))
            .is_deny());

        // No trust anchor on this machine.
        f.sign_owner();
        std::fs::remove_file(f.state.path().join(ANCHOR_FILE)).unwrap();
        let l = f.loader().load(None);
        assert!(l.problems[0].contains("ta rules trust"), "{:?}", l.problems);

        // The agent replaces the anchor too: it still cannot sign for the owner.
        std::fs::write(
            f.state.path().join(ANCHOR_FILE),
            signing::public_to_b64(&signing::public_of(&[99u8; 32])),
        )
        .unwrap();
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(l.problems[0].contains("signed by key"), "{:?}", l.problems);
    }

    #[test]
    fn bad_rules_after_a_valid_signature_are_refused_with_the_line() {
        let f = Fixture::new();
        // Sign a file with a typo directly (bypassing the validator in sign_owner_file).
        let text = "[[allow]]\naction = \"email\"\nenviroment = [\"dev\"]\n";
        f.write_owner(text);
        let sig = signing::sign(
            &f.seed,
            Domain::Owner,
            text,
            "strict",
            "d",
            chrono::Utc::now(),
        );
        std::fs::write(
            f.root.path().join(".ta/rules.toml.sig"),
            serde_json::to_string(&sig).unwrap(),
        )
        .unwrap();
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(
            l.problems.join(" ").contains("enviroment"),
            "{:?}",
            l.problems
        );
    }

    #[test]
    fn sign_refuses_an_invalid_file() {
        let f = Fixture::new();
        let err = sign_owner_file(
            &f.loader(),
            &f.seed,
            "[[allow]]\naction = \"*\"\n",
            chrono::Utc::now(),
        )
        .unwrap_err();
        assert!(err[0].contains("cannot use action"), "{err:?}");
        assert!(!f.root.path().join(".ta/rules.toml.sig").exists());
    }

    #[test]
    fn attended_apply_survives_broken_rules_but_protected_paths_do_not() {
        let f = Fixture::new();
        f.write_owner("not toml [");
        let l = f.loader().load(None);
        assert!(!l.is_usable());
        let ok = l.decide(&Request::new("apply", When::Attended).with_path("src/a.rs"));
        assert!(ok.is_ask() && ok.reason.contains("WARNING"), "{ok:?}");
        let bad = l.decide(&Request::new("apply", When::Attended).with_path(".ta/rules.toml"));
        assert!(bad.is_deny());
        // Everything else, and any unattended apply, is denied.
        assert!(l
            .decide(&Request::new("apply", When::Unattended).with_path("src/a.rs"))
            .is_deny());
        assert!(l.decide(&Request::new("email", When::Attended)).is_deny());
    }

    #[test]
    fn floor_must_verify_and_cannot_be_loosened() {
        let f = Fixture::new();
        f.write_owner("[[allow]]\naction = \"email\"\n");
        f.sign_owner();
        f.write_floor(
            "[[deny]]\naction = \"email\"\nreason = \"Company policy.\"\n",
            true,
        );
        let l = f.loader().load(None);
        assert!(l.is_usable(), "{:?}", l.problems);
        let d = l.decide(&Request::new("email", When::Attended));
        assert!(d.is_deny() && d.layer == Layer::OrgFloor, "{d:?}");

        // An unsigned floor is not ignored: it fails the whole table closed.
        f.write_floor("[[deny]]\naction = \"email\"\n", false);
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(
            l.problems[0].contains("organization floor"),
            "{:?}",
            l.problems
        );
    }

    #[test]
    fn legacy_denies_apply_until_migrated_and_unreadable_legacy_fails_closed() {
        let f = Fixture::new();
        std::fs::write(
            f.root.path().join(".ta/constitution.toml"),
            "[[rules.block]]\naction_type = \"social_post\"\ncondition = \"always\"\nmessage = \"No posting.\"\n",
        )
        .unwrap();
        std::fs::write(
            f.root.path().join(".ta/workflow.toml"),
            "[actions.webhook]\npolicy = \"block\"\n[submit]\nadapter = \"git\"\n",
        )
        .unwrap();
        f.write_owner("[[allow]]\naction = \"social_post\"\n[[allow]]\naction = \"webhook\"\n");
        f.sign_owner();
        let l = f.loader().load(None);
        assert!(l
            .decide(&Request::new("social_post", When::Attended))
            .is_deny());
        assert!(l.decide(&Request::new("webhook", When::Attended)).is_deny());

        std::fs::write(f.root.path().join(".ta/constitution.toml"), "[[rules.block").unwrap();
        let l = f.loader().load(None);
        assert_eq!(l.status, Status::Invalid);
        assert!(
            l.problems.join(" ").contains("fails closed"),
            "{:?}",
            l.problems
        );
    }

    #[test]
    fn role_names_cannot_escape_the_preset_directory() {
        let f = Fixture::new();
        for role in ["../../etc/passwd", "a/b", "", "x.toml", "a b"] {
            let l = f.loader().load(Some(role));
            assert_eq!(l.status, Status::Invalid, "{role:?}");
        }
        std::fs::create_dir_all(f.root.path().join(PRESET_DIR)).unwrap();
        std::fs::write(
            f.root.path().join(PRESET_DIR).join("researcher.toml"),
            "[[deny]]\naction = \"db_query\"\n",
        )
        .unwrap();
        let l = f.loader().load(Some("researcher"));
        assert!(l.compiled.as_ref().unwrap().preset_found);
        let l = f.loader().load(Some("nobody"));
        assert!(!l.compiled.as_ref().unwrap().preset_found);
    }

    #[test]
    fn posture_drift_is_reported_not_silent() {
        let f = Fixture::new();
        f.write_owner("posture = \"balanced\"\n");
        f.sign_owner();
        // Pretend the signature recorded an older posture digest.
        let sigp = f.root.path().join(".ta/rules.toml.sig");
        let mut sig: signing::SigFile =
            serde_json::from_str(&std::fs::read_to_string(&sigp).unwrap()).unwrap();
        assert_eq!(sig.posture, "balanced");
        sig.posture_digest = "0".repeat(64);
        std::fs::write(&sigp, serde_json::to_string(&sig).unwrap()).unwrap();
        let l = f.loader().load(None);
        // The signature covers the file text, not the digest field, so it still verifies.
        assert!(l.is_usable(), "{:?}", l.problems);
        assert!(
            l.warnings
                .iter()
                .any(|w| w.contains("changed since you last signed")),
            "{:?}",
            l.warnings
        );
    }
}
