//! Wayfinder-backed `PlanStore` for Trusted Autonomy (v0.17.11.3).
//!
//! Sub-project 3 of the TA<->Wayfinder plan integration design
//! (`wayfinder` repo's
//! `docs/superpowers/specs/2026-08-23-ta-wayfinder-plan-integration-design.md`).
//! See `store.rs`'s module doc for the actual architecture — in short,
//! local PLAN.md remains the structural source of truth (phase list,
//! dependency graph, goal-run fidelity), and this crate layers Wayfinder on
//! top as a synced, human-visible, cross-tool status mirror.

mod cache;
mod client;
mod config;
mod mapping;
mod secret;
mod select;
mod store;

#[cfg(test)]
mod test_support;

pub use config::WayfinderPlanConfig;
pub use select::select_plan_store;
pub use store::WayfinderPlanStore;
