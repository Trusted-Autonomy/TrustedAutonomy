//! `ta-context-opt`: three swappable optimizer slots behind one contract.
//!
//! | Slot | Acts on | Trait |
//! |---|---|---|
//! | Input | tool and command output, files, logs before they enter context | [`InputOptimizer`] |
//! | Context | the accumulated conversation | [`ContextOptimizer`] |
//! | Output | what the model writes | [`OutputOptimizer`] |
//!
//! Every implementation shares [`Optimizer`] (identity, kind, licence, a cheap
//! `available()` probe) and returns an [`OptMetrics`] record from `apply`. The
//! full unmodified content is written to an [`OriginalStore`] first, so any
//! reduction is reversible and auditable via `original_ref`. `none` is always a
//! valid choice and is the baseline for every comparison.
//!
//! Slots are selected in config (`[optimizers.input] use = "rtk"`) and can be
//! overridden per golden-set replay (`--set optimizers.input.use=none`); see
//! [`OptimizersConfig`].

mod config;
mod metrics;
mod registry;
mod store;

pub use config::{OptimizersConfig, SlotSelection, USE_KEY_SUFFIX};
pub use metrics::{estimate_tokens, OptMetrics};
pub use registry::{
    ContextOptimizer, InputOptimizer, Optimizer, OptimizerInfo, OptimizerKind, OutputOptimizer,
    Registry, Slot,
};
pub use store::OriginalStore;

/// Errors from selecting or running an optimizer.
#[derive(Debug, thiserror::Error)]
pub enum OptError {
    #[error(
        "unknown {slot} optimizer '{name}'. Available: {available}. \
         Fix `use` under [optimizers.{slot}] in .ta/daemon.toml, or pass \
         --set optimizers.{slot}.use=none"
    )]
    Unknown {
        slot: &'static str,
        name: String,
        available: String,
    },
    #[error("cannot store the original at {path}: {source}. Check disk space and permissions on the optimizer store directory")]
    Store {
        path: String,
        source: std::io::Error,
    },
    #[error("no stored original for reference '{0}'. It may have been pruned or belong to a different project")]
    MissingOriginal(String),
    #[error("invalid original reference '{0}': expected 64 lowercase hex characters")]
    BadReference(String),
    #[error(
        "{slot} optimizer '{name}' is registered so its licence is on record, but TA has no \
         integration for it yet, so selecting it would run with no optimization and report \
         the same numbers as 'none'. Use one of: {usable}. See the optimizer slots section of \
         docs/USAGE.md for what is wired"
    )]
    NotWired {
        slot: &'static str,
        name: String,
        usable: String,
    },
}
