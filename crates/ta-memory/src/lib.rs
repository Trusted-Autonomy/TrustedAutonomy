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
//! - **ExternalMemoryAdapter** (`backend = "plugin"` in `.ta/memory.toml`):
//!   Wraps any external binary plugin over JSON-over-stdio.  Ships with
//!   `ta-memory-supermemory` as the first reference plugin.
//!
//! ## Selecting a backend
//!
//! Use `memory_store_from_config()` to create the appropriate backend from
//! `.ta/memory.toml` without hard-coding a specific implementation.
//!
//! ```no_run
//! use ta_memory::memory_store_from_config;
//! let mut store = memory_store_from_config(std::path::Path::new("."));
//! ```

pub mod auto_capture;
pub mod error;
pub mod external_adapter;
pub mod factory;
pub mod fs_store;
pub mod key_schema;
pub mod plugin_manifest;
pub mod plugin_protocol;
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
pub use external_adapter::ExternalMemoryAdapter;
pub use factory::{memory_store_from_config, memory_store_strict};
pub use fs_store::FsMemoryStore;
pub use key_schema::{KeyDomainMap, KeySchema, MemoryConfig, MemorySharingConfig, ProjectType};
pub use plugin_manifest::{
    discover_all_memory_plugins, find_memory_plugin, DiscoveredMemoryPlugin, MemoryPluginManifest,
    MemoryPluginSource,
};
pub use plugin_protocol::MEMORY_PROTOCOL_VERSION;
#[cfg(feature = "ruvector")]
pub use ruvector_store::RuVectorStore;
pub use solutions::{SolutionContext, SolutionEntry, SolutionStore};
pub use store::{MemoryCategory, MemoryEntry, MemoryQuery, MemoryStats, MemoryStore, StoreParams};
