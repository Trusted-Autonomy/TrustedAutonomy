//! # ta-memory
//!
//! Agent-agnostic persistent memory store for Trusted Autonomy.
//!
//! When a user switches from Claude Code to Codex mid-project, or runs
//! multiple agents in parallel, context doesn't get lost. TA owns the
//! memory — agents consume it through MCP tools or CLI.
//!
//! ## Backends
//!
//! - **FsMemoryStore** (default): JSON files in `.ta/memory/`, one per key.
//!   Zero external dependencies. Exact-match and tag-based lookup.

pub mod error;
pub mod fs_store;
pub mod store;

pub use error::MemoryError;
pub use fs_store::FsMemoryStore;
pub use store::{MemoryEntry, MemoryQuery, MemoryStore};
