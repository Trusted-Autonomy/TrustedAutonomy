//! One cleanup engine for Trusted Autonomy.
//!
//! It scans `.ta/` and the project, gives every finding a verdict from ground
//! truth (goal state, PLAN.md, the VCS, the files themselves), acts on the
//! safe ones, asks a person about the rest and records what happened.
//! The CLI (`ta cleanup`, `ta doctor --fix`) and the daemon's periodic gc call
//! the same [`run::run`].

pub mod act;
pub mod archive;
pub mod model;
pub mod run;
pub mod scan;
pub mod state;

pub use model::*;
pub use run::{explain, render_finding, render_report, run, scan_findings};

#[cfg(test)]
mod tests;
