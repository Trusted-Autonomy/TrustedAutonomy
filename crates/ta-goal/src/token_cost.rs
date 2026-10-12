// token_cost.rs: Model-keyed rate table for computing LLM API cost from token usage.
//
// All built-in prices live in the `BUILTIN_RATES` data table below. Nothing else
// in this file holds a price, so changing or adding a model is a one-row edit.
// A project can add or replace rows without a rebuild with `.ta/pricing.toml`
// (see `PriceTable::load`).
//
// Cost precedence, highest first (see `GoalSpend::resolve_cost`):
//   1. the cost the agent runtime itself reported (`total_cost_usd`): exact;
//   2. a row of the override file or the built-in table: an ESTIMATE
//      (`cost_estimated = true`);
//   3. no row (an unknown or non-Claude model, e.g. Ollama or Codex): the
//      tokens are recorded and the cost stays 0 with `cost_estimated = false`.
//      A price is never invented for a model the table does not know.
//
// The older Claude rows carry the list prices TA has always used. The Claude 5
// rows (`claude-sonnet-5-5`, `claude-opus-5-5`, `claude-fable-5-1`) are
// PROVISIONAL: they use the price of the family tier they belong to and have
// not been checked against a published price sheet. Measured runtime costs of
// `claude-sonnet-5-5` goals came out lower than this table predicts, which is
// why the runtime-reported cost is preferred whenever it exists. Put the real
// numbers in `.ta/pricing.toml` to override them.
//
// Cache rates follow Anthropic's published multipliers: a cache read costs
// 0.1x the input price and a cache write (5 minute TTL) costs 1.25x. A write
// with the 1 hour TTL costs 2x; the runtime does not split the two in the
// field TA reads, so one write rate is used.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Per-million-token prices in USD for one model row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRate {
    /// USD per 1M uncached input tokens.
    pub input_per_m: f64,
    /// USD per 1M output tokens.
    pub output_per_m: f64,
    /// USD per 1M cache-read input tokens.
    pub cache_read_per_m: f64,
    /// USD per 1M cache-creation (cache-write) input tokens.
    pub cache_write_per_m: f64,
}

/// Token counts for one goal, split the way the agent runtime reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens served from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_creation: u64,
}

impl TokenUsage {
    /// Every token of every kind (what `--max-tokens` is compared against).
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

/// One row of the built-in table.
struct RateRow {
    /// Lowercase substrings; the row matches when the model name contains any.
    names: &'static [&'static str],
    rate: ModelRate,
}

const fn rate(input: f64, output: f64, cache_read: f64, cache_write: f64) -> ModelRate {
    ModelRate {
        input_per_m: input,
        output_per_m: output,
        cache_read_per_m: cache_read,
        cache_write_per_m: cache_write,
    }
}

/// The built-in price table, USD per million tokens:
/// `rate(input, output, cache_read, cache_write)`.
///
/// Order matters: the first row that matches wins, so the most specific names
/// come first. There is deliberately no catch-all `claude` row.
const BUILTIN_RATES: &[RateRow] = &[
    // Claude 5 family (PROVISIONAL, see the header).
    RateRow {
        names: &["claude-opus-5-5", "claude-opus-5.5"],
        rate: rate(15.0, 75.0, 1.5, 18.75),
    },
    RateRow {
        names: &["claude-fable-5-1", "claude-fable-5.1"],
        rate: rate(15.0, 75.0, 1.5, 18.75),
    },
    RateRow {
        names: &["claude-sonnet-5-5", "claude-sonnet-5.5"],
        rate: rate(3.0, 15.0, 0.30, 3.75),
    },
    // Claude 4 family.
    RateRow {
        names: &["claude-opus-4"],
        rate: rate(15.0, 75.0, 1.5, 18.75),
    },
    RateRow {
        names: &["claude-sonnet-4"],
        rate: rate(3.0, 15.0, 0.30, 3.75),
    },
    RateRow {
        names: &["claude-haiku-4"],
        rate: rate(0.80, 4.0, 0.08, 1.0),
    },
    // Claude 3.x legacy family.
    RateRow {
        names: &["claude-opus-3"],
        rate: rate(15.0, 75.0, 1.5, 18.75),
    },
    RateRow {
        names: &["claude-sonnet-3"],
        rate: rate(3.0, 15.0, 0.30, 3.75),
    },
    RateRow {
        names: &["claude-haiku-3-5", "claude-haiku-3.5"],
        rate: rate(0.80, 4.0, 0.08, 1.0),
    },
    RateRow {
        names: &["claude-haiku-3"],
        rate: rate(0.25, 1.25, 0.03, 0.30),
    },
];

/// Resolve the built-in rate for a model identifier. Case-insensitive
/// substring match, so `claude-sonnet-4-6-20250514` resolves like
/// `claude-sonnet-4-6`.
///
/// Returns `None` for unknown or non-Claude models (Ollama, Codex, and any
/// Claude name the table does not list); use `.ta/pricing.toml` to add one.
pub fn rate_for_model(model: &str) -> Option<ModelRate> {
    let lower = model.to_lowercase();
    BUILTIN_RATES
        .iter()
        .find(|row| row.names.iter().any(|n| lower.contains(n)))
        .map(|row| row.rate)
}

/// Relative path of the per-project price override file.
pub const PRICING_FILE: &str = ".ta/pricing.toml";

/// One `[[model]]` entry of `.ta/pricing.toml`. All four rates are required so
/// a partial row can never silently price cache tokens at a guessed rate.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideRow {
    /// Model name to match (case-insensitive substring, or the whole name when
    /// `exact = true`).
    name: String,
    #[serde(default)]
    exact: bool,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideFile {
    #[serde(default)]
    model: Vec<OverrideRow>,
}

/// The price table in force for one project: override rows (first match wins)
/// ahead of the built-in table.
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    overrides: Vec<(String, bool, ModelRate)>,
    source: Option<PathBuf>,
}

impl PriceTable {
    /// The built-in table only.
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Parse the text of a `pricing.toml`. `origin` names the file in errors.
    pub fn from_toml(text: &str, origin: &Path) -> Result<Self, String> {
        let file: OverrideFile = toml::from_str(text).map_err(|e| {
            format!(
                "{} is not a valid price table: {e}. Expected one or more [[model]] tables with \
                 name, input, output, cache_read and cache_write (USD per million tokens) and an \
                 optional exact = true. Fix or delete the file; until then built-in prices are used.",
                origin.display()
            )
        })?;
        let mut overrides = Vec::new();
        for row in file.model {
            let name = row.name.trim().to_lowercase();
            if name.is_empty() {
                return Err(format!(
                    "{}: a [[model]] entry has an empty name. Give the model name or a unique \
                     part of it.",
                    origin.display()
                ));
            }
            for (field, v) in [
                ("input", row.input),
                ("output", row.output),
                ("cache_read", row.cache_read),
                ("cache_write", row.cache_write),
            ] {
                if !v.is_finite() || v < 0.0 {
                    return Err(format!(
                        "{}: model {:?} has {} = {}; rates are USD per million tokens and must \
                         be zero or more.",
                        origin.display(),
                        row.name,
                        field,
                        v
                    ));
                }
            }
            overrides.push((
                name,
                row.exact,
                rate(row.input, row.output, row.cache_read, row.cache_write),
            ));
        }
        Ok(Self {
            overrides,
            source: Some(origin.to_path_buf()),
        })
    }

    /// Load `<project_root>/.ta/pricing.toml`. A missing file is the built-in
    /// table. A malformed file is reported (log and stderr-free: the message
    /// is returned to the caller) and the built-in table is used, so one typo
    /// cannot stop a goal from finishing.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(PRICING_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::builtin(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not read the price override file; using built-in prices. Check that \
                     the file is readable"
                );
                return Self::builtin();
            }
        };
        match Self::from_toml(&text, &path) {
            Ok(t) => t,
            Err(msg) => {
                tracing::warn!(path = %path.display(), "{msg}");
                Self::builtin()
            }
        }
    }

    /// Where the override rows came from, if any file was read.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Number of override rows in force.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// The rate for a model: override rows first, then the built-in table.
    pub fn rate_for(&self, model: &str) -> Option<ModelRate> {
        let lower = model.trim().to_lowercase();
        if lower.is_empty() {
            return None;
        }
        for (name, exact, r) in &self.overrides {
            let hit = if *exact {
                lower == *name
            } else {
                lower.contains(name.as_str())
            };
            if hit {
                return Some(*r);
            }
        }
        rate_for_model(&lower)
    }

    /// Price `usage` for `model`. `None` when no row matches.
    pub fn price(&self, model: &str, usage: &TokenUsage) -> Option<f64> {
        self.rate_for(model).map(|r| price_with(&r, usage))
    }
}

/// The cost arithmetic: each token kind times its per-million rate.
pub fn price_with(rate: &ModelRate, usage: &TokenUsage) -> f64 {
    (usage.input as f64 / 1_000_000.0) * rate.input_per_m
        + (usage.output as f64 / 1_000_000.0) * rate.output_per_m
        + (usage.cache_read as f64 / 1_000_000.0) * rate.cache_read_per_m
        + (usage.cache_creation as f64 / 1_000_000.0) * rate.cache_write_per_m
}

/// Where a recorded cost came from.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// No price: unknown or non-Claude model and no runtime-reported cost.
    #[default]
    None,
    /// The agent runtime reported the cost (`total_cost_usd`): exact.
    Runtime,
    /// Computed from a price-table row: an estimate.
    Table,
}

/// A resolved cost: the USD amount, whether it is an estimate, and its source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedCost {
    pub usd: f64,
    /// `true` only when the price came from a table row.
    pub estimated: bool,
    pub source: CostSource,
}

/// Decide the cost of a goal: the runtime-reported cost wins and is exact;
/// otherwise a table row gives an estimate; otherwise there is no price.
pub fn resolve_cost(
    table: &PriceTable,
    model: &str,
    usage: &TokenUsage,
    runtime_reported_usd: Option<f64>,
) -> ResolvedCost {
    if let Some(usd) = runtime_reported_usd.filter(|v| v.is_finite() && *v >= 0.0) {
        return ResolvedCost {
            usd,
            estimated: false,
            source: CostSource::Runtime,
        };
    }
    match table.price(model, usage) {
        Some(usd) => ResolvedCost {
            usd,
            estimated: true,
            source: CostSource::Table,
        },
        None => ResolvedCost {
            usd: 0.0,
            estimated: false,
            source: CostSource::None,
        },
    }
}

/// Compute USD cost for a model and uncached input/output counts, using the
/// built-in table only (kept for callers that have no cache counts).
///
/// Returns `(cost_usd, cost_estimated)`: `(price, true)` when a row matched,
/// `(0.0, false)` for an unknown model.
pub fn compute_cost(model: &str, input_tokens: u64, output_tokens: u64) -> (f64, bool) {
    let usage = TokenUsage {
        input: input_tokens,
        output: output_tokens,
        ..TokenUsage::default()
    };
    let r = resolve_cost(&PriceTable::builtin(), model, &usage, None);
    (r.usd, r.estimated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(i: u64, o: u64, r: u64, w: u64) -> TokenUsage {
        TokenUsage {
            input: i,
            output: o,
            cache_read: r,
            cache_creation: w,
        }
    }

    #[test]
    fn sonnet_46_rate_resolves() {
        let rate = rate_for_model("claude-sonnet-4-6").unwrap();
        assert_eq!(rate.input_per_m, 3.0);
        assert_eq!(rate.output_per_m, 15.0);
    }

    #[test]
    fn sonnet_46_with_date_suffix() {
        let rate = rate_for_model("claude-sonnet-4-6-20250514").unwrap();
        assert_eq!(rate.input_per_m, 3.0);
    }

    #[test]
    fn opus_46_rate_resolves() {
        let rate = rate_for_model("claude-opus-4-6").unwrap();
        assert_eq!(rate.input_per_m, 15.0);
        assert_eq!(rate.output_per_m, 75.0);
    }

    #[test]
    fn haiku_45_rate_resolves() {
        let rate = rate_for_model("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(rate.input_per_m, 0.80);
        assert_eq!(rate.output_per_m, 4.0);
    }

    #[test]
    fn every_current_model_name_resolves_with_cache_rates() {
        for model in [
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-haiku-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-haiku-3-5",
        ] {
            let r = rate_for_model(model).unwrap_or_else(|| panic!("{model} has no rate"));
            assert!(
                r.cache_read_per_m > 0.0 && r.cache_read_per_m < r.input_per_m,
                "{model}"
            );
            assert!(r.cache_write_per_m > r.input_per_m, "{model}");
        }
    }

    #[test]
    fn cache_rates_follow_the_published_multipliers_for_every_row() {
        for row in BUILTIN_RATES {
            let r = row.rate;
            assert!(
                (r.cache_read_per_m - 0.1 * r.input_per_m).abs() < 0.0051,
                "{:?} cache read is not ~0.1x input",
                row.names
            );
            assert!(
                (r.cache_write_per_m - 1.25 * r.input_per_m).abs() < 0.0501,
                "{:?} cache write is not ~1.25x input",
                row.names
            );
        }
    }

    #[test]
    fn ollama_and_unknown_claude_names_have_no_rate() {
        assert!(rate_for_model("qwen3.5:9b").is_none());
        assert!(rate_for_model("llama3.3:70b").is_none());
        assert!(rate_for_model("gpt-5-codex").is_none());
        assert!(
            rate_for_model("claude-mystery-9-9").is_none(),
            "an unknown Claude name must not inherit another model's price"
        );
        assert!(rate_for_model("").is_none());
    }

    #[test]
    fn compute_cost_sonnet_1m_tokens() {
        let (cost, estimated) = compute_cost("claude-sonnet-4-6", 1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);
        assert!(estimated);
    }

    #[test]
    fn compute_cost_ollama_is_zero_and_not_estimated() {
        let (cost, estimated) = compute_cost("qwen3.5:9b", 50_000, 10_000);
        assert_eq!(cost, 0.0);
        assert!(!estimated);
    }

    #[test]
    fn compute_cost_zero_tokens() {
        let (cost, _) = compute_cost("claude-sonnet-4-6", 0, 0);
        assert_eq!(cost, 0.0);
    }

    /// Golden arithmetic. Sonnet rates are 3 / 15 / 0.30 / 3.75 per million, so
    /// 1,000 input + 2,000 output + 4,000,000 cache read + 100,000 cache write is
    ///   0.003 + 0.030 + 1.200 + 0.375 = 1.608 USD
    /// and the numbers below are written out by hand, not computed by the code.
    #[test]
    fn golden_cost_arithmetic_for_each_model_family() {
        let u = usage(1_000, 2_000, 4_000_000, 100_000);
        let t = PriceTable::builtin();
        let sonnet = t.price("claude-sonnet-5-5", &u).unwrap();
        assert!((sonnet - 1.608).abs() < 1e-9, "sonnet {sonnet}");
        // Opus: 15 / 75 / 1.5 / 18.75 -> 0.015 + 0.150 + 6.0 + 1.875 = 8.04
        let opus = t.price("claude-opus-5-5", &u).unwrap();
        assert!((opus - 8.04).abs() < 1e-9, "opus {opus}");
        // Haiku 4.5: 0.80 / 4 / 0.08 / 1.0 -> 0.0008 + 0.008 + 0.32 + 0.1 = 0.4288
        let haiku = t.price("claude-haiku-4-5", &u).unwrap();
        assert!((haiku - 0.4288).abs() < 1e-9, "haiku {haiku}");
        // The cache kinds are priced: dropping them would understate by far.
        let uncached_only = t
            .price("claude-sonnet-5-5", &usage(1_000, 2_000, 0, 0))
            .unwrap();
        assert!(sonnet > 30.0 * uncached_only);
    }

    #[test]
    fn runtime_reported_cost_wins_and_is_exact() {
        let u = usage(86, 44_216, 4_423_499, 138_618);
        let r = resolve_cost(
            &PriceTable::builtin(),
            "claude-sonnet-5-5",
            &u,
            Some(1.4391539),
        );
        assert_eq!(r.source, CostSource::Runtime);
        assert!(
            !r.estimated,
            "a runtime-reported cost is exact, not estimated"
        );
        assert_eq!(r.usd, 1.4391539);
    }

    #[test]
    fn table_price_is_marked_estimated() {
        let r = resolve_cost(
            &PriceTable::builtin(),
            "claude-sonnet-5-5",
            &usage(0, 1_000_000, 0, 0),
            None,
        );
        assert_eq!(r.source, CostSource::Table);
        assert!(r.estimated);
        assert!((r.usd - 15.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_records_no_price_and_is_not_estimated() {
        let r = resolve_cost(
            &PriceTable::builtin(),
            "qwen3.5:9b",
            &usage(10, 20, 30, 40),
            None,
        );
        assert_eq!(r.usd, 0.0);
        assert!(!r.estimated);
        assert_eq!(r.source, CostSource::None);
    }

    #[test]
    fn invalid_runtime_cost_is_ignored() {
        for bad in [f64::NAN, -1.0, f64::INFINITY] {
            let r = resolve_cost(
                &PriceTable::builtin(),
                "qwen3.5:9b",
                &usage(1, 1, 0, 0),
                Some(bad),
            );
            assert_eq!(r.source, CostSource::None, "{bad}");
        }
    }

    const OVERRIDE: &str = r#"
[[model]]
name = "claude-sonnet-5-5"
input = 1.0
output = 5.0
cache_read = 0.1
cache_write = 1.25

[[model]]
name = "my-local-model"
exact = true
input = 0.5
output = 0.5
cache_read = 0.05
cache_write = 0.5
"#;

    #[test]
    fn override_file_replaces_builtin_rows_and_adds_unknown_models() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(PRICING_FILE), OVERRIDE).unwrap();
        let t = PriceTable::load(dir.path());
        assert_eq!(t.override_count(), 2);
        assert!(t.source().is_some());
        // Overridden row.
        let r = t.rate_for("claude-sonnet-5-5-20260101").unwrap();
        assert_eq!(r.input_per_m, 1.0);
        assert_eq!(r.cache_read_per_m, 0.1);
        // Exact match only for the exact row.
        assert!(t.rate_for("my-local-model").is_some());
        assert!(t.rate_for("my-local-model-v2").is_none());
        // Built-in rows the file does not mention still apply.
        assert_eq!(t.rate_for("claude-opus-5-5").unwrap().input_per_m, 15.0);
        // And pricing through the override is an estimate from a table row.
        let c = resolve_cost(&t, "my-local-model", &usage(2_000_000, 0, 0, 0), None);
        assert!(c.estimated);
        assert!((c.usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn malformed_override_file_falls_back_to_builtin_with_a_clear_error() {
        let p = Path::new("/p/.ta/pricing.toml");
        for bad in [
            "[[model]]\nname = \"x\"\ninput = 1.0\n",
            "[[model]]\nname = \"x\"\ninput = -1.0\noutput = 1.0\ncache_read = 0.0\ncache_write = 0.0\n",
            "[[model]]\nname = \"\"\ninput = 1.0\noutput = 1.0\ncache_read = 0.0\ncache_write = 0.0\n",
            "[[model]]\nname = \"x\"\ninput = 1.0\noutput = 1.0\ncache_read = 0.0\ncache_write = 0.0\nbogus = 1\n",
            "this is not toml [",
        ] {
            let err = PriceTable::from_toml(bad, p).unwrap_err();
            assert!(err.contains("pricing.toml"), "{err}");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(PRICING_FILE), "not toml [").unwrap();
        let t = PriceTable::load(dir.path());
        assert_eq!(t.override_count(), 0);
        assert!(t.rate_for("claude-sonnet-4-6").is_some());
    }

    #[test]
    fn missing_override_file_is_the_builtin_table() {
        let dir = tempfile::tempdir().unwrap();
        let t = PriceTable::load(dir.path());
        assert_eq!(t.override_count(), 0);
        assert!(t.source().is_none());
    }

    #[test]
    fn token_usage_total_counts_every_kind() {
        assert_eq!(usage(1, 2, 3, 4).total(), 10);
        assert_eq!(usage(u64::MAX, 1, 0, 0).total(), u64::MAX);
    }
}
