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
//! - **RuVectorStore** (feature `ruvector`): HNSW-indexed vector database
//!   for semantic search. Sub-millisecond recall at scale.

pub mod auto_capture;
pub mod error;
pub mod fs_store;
pub mod key_schema;
#[cfg(feature = "ruvector")]
pub mod ruvector_store;
pub mod solutions;
pub mod store;

pub use auto_capture::{
    build_memory_context_section_with_manifest_filter, capture_plan_phase_complete,
    index_constitution_rules, slug_from_text_pub, AutoCapture, AutoCaptureConfig, DraftRejectEvent,
    GoalCompleteEvent, HumanGuidanceEvent,
};
pub use error::MemoryError;
pub use fs_store::FsMemoryStore;
pub use key_schema::{KeyDomainMap, KeySchema, ProjectType};
#[cfg(feature = "ruvector")]
pub use ruvector_store::RuVectorStore;
pub use solutions::{SolutionContext, SolutionEntry, SolutionStore};
pub use store::{MemoryCategory, MemoryEntry, MemoryQuery, MemoryStats, MemoryStore, StoreParams};
