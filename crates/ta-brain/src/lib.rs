//! `ta-brain` — tier 2 of the 3-tier request model
//! (`docs/design/ta-concepts-and-architecture.md` §3/§13): the routing
//! brain that turns "a goal request arrived" (either explicit, via `ta run`,
//! or triggered, via a `ta-intake::TriggerEvent`) into a concrete
//! [`RoutingDecision`] — which team role handles it, which persona and
//! agent it runs as, at what security tier, and at what priority.
//!
//! This crate is a library only — no CLI or daemon-specific glue. It owns
//! one pure function, [`route`], plus [`prioritize`] for ordering a batch of
//! pending trigger events. Everything here is deterministic given its inputs
//! and the on-disk config under `workspace_root` (`.ta/workflow.toml`,
//! `.ta/team.toml`, `.ta/personas/*.toml`, `.ta/daemon.toml`) — no network
//! calls, no goal creation, no side effects.
//!
//! `route()` extends the agent-only `Switch` resolution tiers built in
//! v0.17.0.12.13 (`apps/ta-cli/src/commands/run.rs`) to also resolve
//! `team`, `persona`, `security_tier`, and `priority` the same way `agent`
//! already is — most-specific-tier-wins, with an explicit `"auto"` escape
//! hatch at the security tier that reuses the existing `AdvisorSecurity`
//! tri-state (`ta_session::workflow_session::AdvisorSecurity`) rather than a
//! new trust-level concept.

pub mod classify;
pub mod decision;
pub mod input;
pub mod priority;
pub mod queue;
pub mod route;

pub use classify::{classify_workload, WorkloadClassification};
pub use decision::RoutingDecision;
pub use input::{ExplicitGoalRequest, RoutingInput, TriggerRoutingInput};
pub use priority::Priority;
pub use queue::prioritize;
pub use route::route;
