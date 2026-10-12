// rules: the rules engine (v0.17.12.4).
//
// One deterministic table, compiled from the owner's signed rules file, an
// optional organization floor, tighten-only role presets and a shipped
// posture. `CompiledRules::decide` is the only place an answer to "may this
// happen?" comes from. Design:
// `docs/superpowers/specs/2026-10-07-compiled-constitution-automation-rules-design.md`
// (Revision 2).

pub mod audit;
pub mod load;
pub mod migrate;
pub mod model;
pub mod paths;
pub mod postures;
pub mod report;
pub mod signing;
pub mod table;
pub mod untrusted;

pub use load::{Loaded, RulesLoader, Status, OWNER_FILE, PRESET_DIR};
pub use model::{DocKind, Effect, Environment, Layer, Rule, RulesDoc, When};
pub use table::{CompiledRules, Decision, Request, TargetRef};
