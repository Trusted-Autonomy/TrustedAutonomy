use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::minimize::Kind;
use crate::{estimate_tokens, HookSpec, OptError, OptMetrics, OptimizersConfig, OriginalStore};

/// The three places an optimizer can sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Input,
    Context,
    Output,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::Input => "input",
            Slot::Context => "context",
            Slot::Output => "output",
        }
    }
}

/// How an optimizer reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizerKind {
    /// A child process TA supervises.
    Process,
    /// An HTTP proxy on `ANTHROPIC_BASE_URL` (the `[compression.plugin]` kind).
    Proxy,
    /// A hook installed into the agent (RTK rewriting shell commands).
    Hook,
    /// Code that runs inside TA or the agent.
    InProcess,
}

/// Identity and provenance, shown by `ta compression status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizerInfo {
    pub name: String,
    pub version: String,
    pub kind: OptimizerKind,
    /// SPDX-style licence of the optimizer itself, recorded so a reviewer can
    /// see what is in use.
    pub licence: String,
}

/// Contract shared by every slot.
pub trait Optimizer: Send + Sync {
    fn info(&self) -> OptimizerInfo;

    /// Cheap probe. Must never start a visible window or a long-running
    /// process: a PATH lookup, not a spawn.
    fn available(&self) -> bool;

    /// For hook-kind optimizers: the default spec TA ships. Config may overlay it.
    fn hook_spec(&self) -> Option<HookSpec> {
        None
    }
}

/// Acts on tool and command output, files and logs before they enter context.
pub trait InputOptimizer: Optimizer {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError>;

    /// Like [`apply`](Self::apply), told which command line produced `input`. The default
    /// ignores the command; a minimizer that understands several commands uses it to pick
    /// the right reduction without guessing from the text.
    fn apply_for_command(
        &self,
        _command: &str,
        input: &str,
        store: &OriginalStore,
    ) -> Result<(String, OptMetrics), OptError> {
        self.apply(input, store)
    }
}

/// Acts on the accumulated conversation.
pub trait ContextOptimizer: Optimizer {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError>;
}

/// Acts on what the model writes.
pub trait OutputOptimizer: Optimizer {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError>;
}

/// Save the original, time `reduce`, and build the metrics record. Every
/// implementation goes through this so the record and the stored original
/// can never disagree.
pub fn run_measured(
    input: &str,
    store: &OriginalStore,
    lossless: bool,
    reduce: impl FnOnce(&str) -> String,
) -> Result<(String, OptMetrics), OptError> {
    let started = Instant::now();
    let original_ref = store.save(input)?;
    let output = reduce(input);
    let metrics = OptMetrics {
        bytes_in: input.len(),
        bytes_out: output.len(),
        est_tokens_in: estimate_tokens(input),
        est_tokens_out: estimate_tokens(&output),
        ms: started.elapsed().as_millis() as u64,
        lossless,
        original_ref,
    };
    Ok((output, metrics))
}

/// A first-party input minimizer (see [`crate::minimize`]). `kind` fixes the command it
/// understands; `None` is the router (`first-party`) that picks per command line, or from the
/// text when no command is known, and passes unrecognised output through unchanged.
struct FirstParty {
    name: &'static str,
    kind: Option<Kind>,
}

impl FirstParty {
    fn reduce(
        &self,
        kind: Option<Kind>,
        input: &str,
        store: &OriginalStore,
    ) -> Result<(String, OptMetrics), OptError> {
        let output = match kind {
            Some(k) => crate::minimize::minimize(k, input),
            None => input.to_string(),
        };
        let lossless = output == input;
        run_measured(input, store, lossless, |_| output)
    }
}

impl Optimizer for FirstParty {
    fn info(&self) -> OptimizerInfo {
        OptimizerInfo {
            name: self.name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: OptimizerKind::InProcess,
            licence: "Apache-2.0".into(),
        }
    }
    fn available(&self) -> bool {
        true
    }
}

impl InputOptimizer for FirstParty {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError> {
        let kind = self.kind.or_else(|| crate::minimize::sniff(input));
        self.reduce(kind, input, store)
    }

    fn apply_for_command(
        &self,
        command: &str,
        input: &str,
        store: &OriginalStore,
    ) -> Result<(String, OptMetrics), OptError> {
        // A single-kind minimizer only reduces its own command's output; anything else it
        // was handed passes through, never mangled by the wrong rules.
        let kind = match self.kind {
            Some(k) => (crate::minimize::kind_for_command(command) == Some(k)).then_some(k),
            None => {
                crate::minimize::kind_for_command(command).or_else(|| crate::minimize::sniff(input))
            }
        };
        self.reduce(kind, input, store)
    }
}

/// The baseline: changes nothing, valid in every slot.
struct NoneOptimizer;

impl Optimizer for NoneOptimizer {
    fn info(&self) -> OptimizerInfo {
        OptimizerInfo {
            name: "none".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: OptimizerKind::InProcess,
            licence: "Apache-2.0".into(),
        }
    }
    fn available(&self) -> bool {
        true
    }
}

macro_rules! impl_none {
    ($($tr:ident),*) => {$(
        impl $tr for NoneOptimizer {
            fn apply(&self, input: &str, store: &OriginalStore)
                -> Result<(String, OptMetrics), OptError> {
                run_measured(input, store, true, |s| s.to_string())
            }
        }
    )*};
}
impl_none!(InputOptimizer, ContextOptimizer, OutputOptimizer);

/// An optimizer that works out of band (a proxy or a hook), so TA never hands
/// it text. `apply` is an identity that still records the original, which keeps
/// it comparable in the golden set; the real reduction shows in `ta cost report`.
struct OutOfBand {
    name: &'static str,
    kind: OptimizerKind,
    licence: &'static str,
    /// Binary probed on PATH; `None` for a user-supplied proxy.
    binary: Option<&'static str>,
    /// Default spec for a hook-kind entry. The hook lifecycle is generic over this data.
    hook: Option<fn() -> HookSpec>,
}

impl Optimizer for OutOfBand {
    fn info(&self) -> OptimizerInfo {
        OptimizerInfo {
            name: self.name.into(),
            version: "external".into(),
            kind: self.kind,
            licence: self.licence.into(),
        }
    }
    fn available(&self) -> bool {
        self.binary.is_none_or(|b| which::which(b).is_ok())
    }
    fn hook_spec(&self) -> Option<HookSpec> {
        self.hook.map(|f| f())
    }
}

impl OutOfBand {
    /// A hook-kind optimizer works inside the agent, so TA never hands it text. `apply` refuses
    /// instead of returning the input unchanged with a `lossless` metrics row for work that never
    /// happened. Hook results are recorded by the goal's install record and `measure_pair`.
    fn refuse_if_hook(&self, slot: &'static str) -> Result<(), OptError> {
        if self.kind == OptimizerKind::Hook {
            return Err(OptError::NotWired {
                slot,
                name: self.name.to_string(),
                reason: "a hook optimizer acts inside the agent and does not transform text passed to apply".into(),
                usable: String::new(),
            });
        }
        Ok(())
    }
}

impl InputOptimizer for OutOfBand {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError> {
        self.refuse_if_hook("input")?;
        run_measured(input, store, true, |s| s.to_string())
    }
}

impl ContextOptimizer for OutOfBand {
    fn apply(&self, input: &str, store: &OriginalStore) -> Result<(String, OptMetrics), OptError> {
        self.refuse_if_hook("context")?;
        run_measured(input, store, true, |s| s.to_string())
    }
}

/// Name -> implementation for each slot.
pub struct Registry {
    input: Vec<Box<dyn InputOptimizer>>,
    context: Vec<Box<dyn ContextOptimizer>>,
    output: Vec<Box<dyn OutputOptimizer>>,
    specs: std::collections::BTreeMap<(Slot, String), HookSpec>,
}

impl Registry {
    /// The shipped set: `none` everywhere, RTK / headroom / the generic
    /// `[compression.plugin]` proxy for input, and the agent's own compaction
    /// for context.
    pub fn builtin() -> Self {
        Self {
            input: vec![
                Box::new(NoneOptimizer),
                Box::new(OutOfBand {
                    name: "rtk",
                    kind: OptimizerKind::Hook,
                    licence: "Apache-2.0 (rtk-oss core; full rtk and rtk-proxy add BUSL-1.1 parts)",
                    binary: Some("rtk"),
                    hook: Some(HookSpec::rtk_oss),
                }),
                Box::new(OutOfBand {
                    name: "headroom",
                    kind: OptimizerKind::Proxy,
                    licence: "Apache-2.0",
                    binary: Some("headroom"),
                    hook: None,
                }),
                Box::new(OutOfBand {
                    name: "proxy",
                    kind: OptimizerKind::Proxy,
                    licence: "see the configured [compression.plugin] command",
                    binary: None,
                    hook: None,
                }),
                Box::new(FirstParty {
                    name: "first-party",
                    kind: None,
                }),
                Box::new(FirstParty {
                    name: "cargo",
                    kind: Some(Kind::Cargo),
                }),
                Box::new(FirstParty {
                    name: "git",
                    kind: Some(Kind::Git),
                }),
                Box::new(FirstParty {
                    name: "ta",
                    kind: Some(Kind::Ta),
                }),
            ],
            context: vec![
                Box::new(NoneOptimizer),
                Box::new(OutOfBand {
                    name: "agent-native",
                    kind: OptimizerKind::InProcess,
                    licence: "n/a (built into the agent)",
                    binary: None,
                    hook: None,
                }),
            ],
            output: vec![Box::new(NoneOptimizer)],
            specs: Default::default(),
        }
        .seed_specs()
    }

    fn seed_specs(mut self) -> Self {
        for slot in [Slot::Input, Slot::Context, Slot::Output] {
            let found: Vec<(String, HookSpec)> = match slot {
                Slot::Input => self
                    .input
                    .iter()
                    .map(|o| (o.info().name, o.hook_spec()))
                    .collect::<Vec<_>>(),
                Slot::Context => self
                    .context
                    .iter()
                    .map(|o| (o.info().name, o.hook_spec()))
                    .collect(),
                Slot::Output => self
                    .output
                    .iter()
                    .map(|o| (o.info().name, o.hook_spec()))
                    .collect(),
            }
            .into_iter()
            .filter_map(|(n, s)| s.map(|s| (n, s)))
            .collect();
            for (n, s) in found {
                self.specs.insert((slot, n), s);
            }
        }
        self
    }

    /// Overlay `[optimizers.<slot>.<name>]` hook tables from config onto the shipped specs.
    /// A table naming an optimizer that is not a registered hook kind is ignored (it may be
    /// another slot's setting); a malformed table is an error naming the table.
    pub fn with_config(mut self, cfg: &OptimizersConfig) -> Result<Self, OptError> {
        for (slot, sel) in [
            (Slot::Input, &cfg.input),
            (Slot::Context, &cfg.context),
            (Slot::Output, &cfg.output),
        ] {
            let names: Vec<String> = sel.extra.keys().cloned().collect();
            for name in names {
                let is_hook = self
                    .infos(slot)
                    .iter()
                    .any(|i| i.name == name && i.kind == OptimizerKind::Hook);
                if !is_hook {
                    continue;
                }
                if let Some(over) = sel.hook_override(&name)? {
                    self.specs.entry((slot, name)).or_default().overlay(&over);
                }
            }
        }
        Ok(self)
    }

    /// The effective spec (shipped default plus config) for a hook optimizer.
    pub fn hook_spec(&self, slot: Slot, name: &str) -> Option<&HookSpec> {
        self.specs.get(&(slot, name.to_string()))
    }

    pub fn names(&self, slot: Slot) -> Vec<String> {
        self.infos(slot).into_iter().map(|i| i.name).collect()
    }

    pub fn infos(&self, slot: Slot) -> Vec<OptimizerInfo> {
        match slot {
            Slot::Input => self.input.iter().map(|o| o.info()).collect(),
            Slot::Context => self.context.iter().map(|o| o.info()).collect(),
            Slot::Output => self.output.iter().map(|o| o.info()).collect(),
        }
    }

    fn unknown(&self, slot: Slot, name: &str) -> OptError {
        OptError::Unknown {
            slot: slot.name(),
            name: name.to_string(),
            available: self.names(slot).join(", "),
        }
    }

    pub fn input(&self, name: &str) -> Result<&dyn InputOptimizer, OptError> {
        self.input
            .iter()
            .find(|o| o.info().name == name)
            .map(|o| o.as_ref())
            .ok_or_else(|| self.unknown(Slot::Input, name))
    }

    pub fn context(&self, name: &str) -> Result<&dyn ContextOptimizer, OptError> {
        self.context
            .iter()
            .find(|o| o.info().name == name)
            .map(|o| o.as_ref())
            .ok_or_else(|| self.unknown(Slot::Context, name))
    }

    pub fn output(&self, name: &str) -> Result<&dyn OutputOptimizer, OptError> {
        self.output
            .iter()
            .find(|o| o.info().name == name)
            .map(|o| o.as_ref())
            .ok_or_else(|| self.unknown(Slot::Output, name))
    }

    /// Refuse a name that cannot do its job. For a hook kind that means: its spec is present, the
    /// binary is on PATH, the version meets the minimum and the edition is acceptable (see
    /// [`crate::probe`]). Without this, `use = "rtk"` would run unoptimised and report the same
    /// numbers as `none`, so an A/B replay arm would conclude the optimizer saves nothing.
    /// Unknown names are left to the plain lookup so the two failures keep distinct messages.
    pub fn ensure_wired(&self, slot: Slot, name: &str) -> Result<(), OptError> {
        let infos = self.infos(slot);
        let Some(info) = infos.iter().find(|i| i.name == name) else {
            return Ok(());
        };
        if info.kind != OptimizerKind::Hook {
            return Ok(());
        }
        let refuse = |reason: String| OptError::NotWired {
            slot: slot.name(),
            name: name.to_string(),
            reason,
            usable: infos
                .iter()
                .filter(|i| i.kind != OptimizerKind::Hook)
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        };
        let Some(spec) = self.hook_spec(slot, name) else {
            return Err(refuse(crate::HookRefusal::NoSpec(name.into()).to_string()));
        };
        crate::probe(spec, name)
            .map(|_| ())
            .map_err(|e| refuse(e.to_string()))
    }

    /// The optimizer a config selects for the input slot (`none` when unset).
    pub fn select_input(&self, cfg: &OptimizersConfig) -> Result<&dyn InputOptimizer, OptError> {
        let name = cfg.input.use_.as_deref().unwrap_or("none");
        self.ensure_wired(Slot::Input, name)?;
        self.input(name)
    }

    pub fn select_context(
        &self,
        cfg: &OptimizersConfig,
    ) -> Result<&dyn ContextOptimizer, OptError> {
        let name = cfg.context.use_.as_deref().unwrap_or("none");
        self.ensure_wired(Slot::Context, name)?;
        self.context(name)
    }

    pub fn select_output(&self, cfg: &OptimizersConfig) -> Result<&dyn OutputOptimizer, OptError> {
        let name = cfg.output.use_.as_deref().unwrap_or("none");
        self.ensure_wired(Slot::Output, name)?;
        self.output(name)
    }
}

impl OptimizersConfig {
    /// Whether the input slot, as configured, wants the proxy-kind optimizer
    /// (`[compression.plugin]` / headroom) running. `None` = not chosen, so the
    /// caller keeps its legacy `[compression] enabled` behaviour.
    pub fn input_wants_proxy(&self) -> Option<bool> {
        let name = self.input.use_.as_deref()?;
        Some(matches!(name, "headroom" | "proxy"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(input: Option<&str>) -> OptimizersConfig {
        let mut c = OptimizersConfig::default();
        c.input.use_ = input.map(str::to_string);
        c
    }

    #[test]
    fn unset_slot_selects_none() {
        let r = Registry::builtin();
        assert_eq!(r.select_input(&cfg(None)).unwrap().info().name, "none");
        assert_eq!(
            r.select_context(&OptimizersConfig::default())
                .unwrap()
                .info()
                .name,
            "none"
        );
        assert_eq!(
            r.select_output(&OptimizersConfig::default())
                .unwrap()
                .info()
                .name,
            "none"
        );
    }

    #[test]
    fn slot_selection_by_name() {
        let r = Registry::builtin();
        for name in ["none", "headroom", "proxy"] {
            assert_eq!(r.select_input(&cfg(Some(name))).unwrap().info().name, name);
        }
        assert_eq!(
            r.input("headroom").unwrap().info().kind,
            OptimizerKind::Proxy
        );
        assert_eq!(r.input("rtk").unwrap().info().kind, OptimizerKind::Hook);
    }

    #[test]
    fn unknown_name_lists_choices_and_the_fix() {
        let r = Registry::builtin();
        let err = r.input("caveman").err().unwrap().to_string();
        assert!(err.contains("'caveman'"), "{err}");
        assert!(
            err.contains("none, rtk, headroom, proxy, first-party, cargo, git, ta"),
            "{err}"
        );
        assert!(err.contains("--set optimizers.input.use=none"), "{err}");
    }

    #[test]
    fn licences_are_recorded_for_every_optimizer() {
        let r = Registry::builtin();
        for slot in [Slot::Input, Slot::Context, Slot::Output] {
            for info in r.infos(slot) {
                assert!(!info.licence.is_empty(), "{} has no licence", info.name);
            }
        }
        assert!(r.input("rtk").unwrap().info().licence.contains("BUSL-1.1"));
    }

    #[test]
    fn none_is_an_identity_baseline_with_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        let text = "warning: unused\nwarning: unused\nerror: boom\n";
        let (out, m) = Registry::builtin()
            .input("none")
            .unwrap()
            .apply(text, &store)
            .unwrap();
        assert_eq!(out, text);
        assert_eq!(m.bytes_in, text.len());
        assert_eq!(m.bytes_out, text.len());
        assert_eq!(m.est_tokens_in, m.est_tokens_out);
        assert!(m.lossless);
        assert_eq!(m.reduction(), 0.0);
    }

    const CARGO_OUT: &str = "   Compiling serde v1.0.200\n   Compiling tokio v1.38.0\n   Compiling hyper v1.3.1\n   Compiling reqwest v0.12.4\n   Compiling ta-core v0.1.0\n   Compiling b v1.0.0\nerror[E0308]: mismatched types\n --> src/lib.rs:1:1\n\nerror: could not compile `b` (lib) due to 1 previous error\n";

    #[test]
    fn first_party_minimizers_are_selectable_and_apply_records_real_metrics() {
        let r = Registry::builtin();
        for name in ["first-party", "cargo", "git", "ta"] {
            let chosen = r.select_input(&cfg(Some(name))).unwrap();
            assert_eq!(chosen.info().name, name);
            assert_eq!(chosen.info().kind, OptimizerKind::InProcess);
            assert!(chosen.available());
            assert!(!chosen.info().licence.is_empty());
        }
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        let opt = r.select_input(&cfg(Some("cargo"))).unwrap();
        let (out, m) = opt
            .apply_for_command("cargo build", CARGO_OUT, &store)
            .unwrap();
        assert!(out.contains("error[E0308]") && !out.contains("Compiling serde"));
        assert_eq!(m.bytes_in, CARGO_OUT.len());
        assert_eq!(m.bytes_out, out.len());
        assert!(m.bytes_out < m.bytes_in && m.reduction() > 0.0);
        assert!(!m.lossless, "text was removed, so it is not lossless");
        assert_eq!(store.recover(&m.original_ref).unwrap(), CARGO_OUT);
    }

    #[test]
    fn a_single_kind_minimizer_leaves_other_commands_output_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        let r = Registry::builtin();
        let (out, m) = r
            .input("git")
            .unwrap()
            .apply_for_command("cargo build", CARGO_OUT, &store)
            .unwrap();
        assert_eq!(out, CARGO_OUT);
        assert!(m.lossless && m.reduction() == 0.0);
        // The original is still stored, so the record stays auditable.
        assert_eq!(store.recover(&m.original_ref).unwrap(), CARGO_OUT);
    }

    #[test]
    fn the_router_picks_by_command_then_by_text_and_passes_the_rest_through() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        let router = Registry::builtin();
        let router = router.input("first-party").unwrap();
        let (by_cmd, _) = router
            .apply_for_command("./dev cargo test", CARGO_OUT, &store)
            .unwrap();
        assert!(!by_cmd.contains("Compiling serde"));
        let (by_text, _) = router.apply(CARGO_OUT, &store).unwrap();
        assert_eq!(by_text, by_cmd);
        let (plain, m) = router
            .apply_for_command("make deploy", "just some words\n", &store)
            .unwrap();
        assert_eq!(plain, "just some words\n");
        assert!(m.lossless);
    }

    #[test]
    fn original_is_recoverable_from_the_metrics_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path().join("originals"));
        let text = "the full unmodified content";
        let (_, m) = Registry::builtin()
            .input("none")
            .unwrap()
            .apply(text, &store)
            .unwrap();
        assert_eq!(store.recover(&m.original_ref).unwrap(), text);
    }

    #[test]
    fn recover_rejects_path_escape_and_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        assert!(matches!(
            store.recover("../../etc/passwd"),
            Err(OptError::BadReference(_))
        ));
        assert!(matches!(
            store.recover(&"a".repeat(64)),
            Err(OptError::MissingOriginal(_))
        ));
    }

    #[test]
    fn run_measured_records_reduction_and_lossiness() {
        let dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(dir.path());
        let (out, m) =
            run_measured("aaaa bbbb cccc", &store, false, |s| s[..4].to_string()).unwrap();
        assert_eq!(out, "aaaa");
        assert!(!m.lossless);
        assert!(m.reduction() > 0.7);
        assert_eq!(store.recover(&m.original_ref).unwrap(), "aaaa bbbb cccc");
    }

    #[test]
    fn overrides_replace_config_selection() {
        let base = cfg(Some("headroom"));
        let ov = serde_json::json!({"optimizers.input.use": "None", "other": 1});
        let got = base.with_overrides(Some(&ov));
        assert_eq!(got.input.use_.as_deref(), Some("none"));
        assert_eq!(got.input_wants_proxy(), Some(false));
        let same = cfg(Some("headroom")).with_overrides(None);
        assert_eq!(same.input_wants_proxy(), Some(true));
        assert_eq!(cfg(None).input_wants_proxy(), None);
    }

    #[test]
    fn config_parses_from_toml_shape() {
        let c: OptimizersConfig = serde_json::from_value(serde_json::json!({
            "input": {"use": "rtk"}
        }))
        .unwrap();
        assert_eq!(c.input.use_.as_deref(), Some("rtk"));
        assert_eq!(c.output.use_, None);
    }

    #[test]
    fn override_validation_accepts_known_and_rejects_typos() {
        let r = Registry::builtin();
        for name in ["none", "headroom"] {
            let ov = serde_json::json!({"optimizers.input.use": name});
            OptimizersConfig::validate_overrides(&ov, &r).unwrap();
            let sel = OptimizersConfig::default().with_overrides(Some(&ov));
            assert_eq!(r.select_input(&sel).unwrap().info().name, name);
        }
        let bad = serde_json::json!({"optimizers.input.use": "rkt"});
        let err = OptimizersConfig::validate_overrides(&bad, &r)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'rkt'") && err.contains("none, rtk, headroom, proxy"),
            "{err}"
        );
        let non_string = serde_json::json!({"optimizers.input.use": 3});
        assert!(OptimizersConfig::validate_overrides(&non_string, &r).is_err());
        OptimizersConfig::validate_overrides(&serde_json::json!({"wiki.disabled": true}), &r)
            .unwrap();
    }

    fn rtk_cfg(binary: &std::path::Path) -> OptimizersConfig {
        serde_json::from_value(serde_json::json!({
            "input": {"rtk": {"binary": binary.display().to_string()}}
        }))
        .unwrap()
    }

    #[test]
    fn a_hook_optimizer_without_a_usable_binary_is_refused_not_silently_ignored() {
        // Point the spec at a path that does not exist so the result never depends on whether
        // this machine has a real rtk.
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::builtin()
            .with_config(&rtk_cfg(&dir.path().join("no-such-rtk")))
            .unwrap();
        let ov = serde_json::json!({"optimizers.input.use": "rtk"});
        // Replay validation fails before a replay spends tokens on an arm that would run
        // unoptimised and be reported identically to `none`.
        let err = OptimizersConfig::validate_overrides(&ov, &r)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("rtk")
                && err.contains("not found on PATH")
                && err.contains("brew install rtk-ai/tap/rtk-oss"),
            "{err}"
        );
        assert!(
            err.contains("none, headroom, proxy"),
            "the message must name what can be used: {err}"
        );
        let sel = OptimizersConfig::default().with_overrides(Some(&ov));
        assert!(matches!(
            r.select_input(&sel),
            Err(crate::OptError::NotWired { .. })
        ));
        // `apply` cannot be used to fabricate a metrics row for work that never happened.
        let store_dir = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(store_dir.path());
        assert!(matches!(
            r.input("rtk").unwrap().apply("some command output", &store),
            Err(crate::OptError::NotWired { .. })
        ));
        // Its licence stays on record for `ta compression status`.
        assert!(r.input("rtk").unwrap().info().licence.contains("BUSL-1.1"));
        assert!(r.ensure_wired(Slot::Input, "headroom").is_ok());
        assert!(r.ensure_wired(Slot::Input, "nonesuch").is_ok());
    }

    #[test]
    fn a_hook_with_no_spec_is_refused() {
        let mut r = Registry::builtin();
        r.specs.clear();
        let err = r.ensure_wired(Slot::Input, "rtk").unwrap_err().to_string();
        assert!(
            err.contains("no spec") && err.contains("[optimizers.input.rtk]"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_wired_rtk_flips_to_accepting_only_when_spec_binary_and_version_are_satisfied() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("rtk");
        let script = |version: &str, help: &str| {
            std::fs::write(
                &bin,
                format!("#!/bin/sh\ncase \"$1\" in --version) echo 'rtk {version}';; --help) echo '{help}';; esac\n"),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let reg = || Registry::builtin().with_config(&rtk_cfg(&bin)).unwrap();
        // binary missing
        assert!(reg().ensure_wired(Slot::Input, "rtk").is_err());
        // too old
        script("0.22.0", "init");
        assert!(reg().ensure_wired(Slot::Input, "rtk").is_err());
        // full edition
        script("0.23.0", "init pipeline");
        assert!(reg().ensure_wired(Slot::Input, "rtk").is_err());
        // unknown edition
        script("0.23.0", "nothing useful");
        assert!(reg().ensure_wired(Slot::Input, "rtk").is_err());
        // all satisfied
        script("0.23.0", "init gain");
        reg().ensure_wired(Slot::Input, "rtk").unwrap();
        let sel = OptimizersConfig::default()
            .with_overrides(Some(&serde_json::json!({"optimizers.input.use": "rtk"})));
        assert_eq!(reg().select_input(&sel).unwrap().info().name, "rtk");
    }

    #[test]
    fn config_overlay_replaces_only_the_fields_it_names_and_bad_tables_are_reported() {
        let cfg: OptimizersConfig = serde_json::from_value(serde_json::json!({
            "input": {"use": "rtk", "rtk": {"min_version": "9.9.9", "install": ["x"]}}
        }))
        .unwrap();
        let r = Registry::builtin().with_config(&cfg).unwrap();
        let spec = r.hook_spec(Slot::Input, "rtk").unwrap();
        assert_eq!(spec.min_version, "9.9.9");
        assert_eq!(spec.install, vec!["x".to_string()]);
        assert_eq!(spec.binary, "rtk");
        assert_eq!(spec.edition, "oss");
        assert!(!spec.allow_busl);
        let bad: OptimizersConfig = serde_json::from_value(serde_json::json!({
            "input": {"rtk": {"install": "not-a-list"}}
        }))
        .unwrap();
        let e = Registry::builtin()
            .with_config(&bad)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("[optimizers.input.rtk]"), "{e}");
    }
}
