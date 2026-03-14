//! Response strategy definitions for event routing.
//!
//! Strategy modules define the parameters and context builders for each
//! response type. The actual execution happens in the daemon or CLI —
//! these modules define the data shapes and helper functions for building
//! strategy context from routing decisions and event envelopes.

pub mod agent;
pub mod workflow;
