//! ta-governed-paths — SHA filesystem and URI journal for managed paths (v0.17.0).
//!
//! ## Design
//!
//! Governed paths are directories or files declared in `.ta/workflow.toml` under
//! `[[governed_paths]]`. Two facilities protect them:
//!
//! **SHA store** (`.ta/sha-fs/<sha256>`): A content-addressed blob store.  Writing a
//! file to a governed path computes its SHA-256 and saves a copy of the content at
//! `.ta/sha-fs/<sha256>`. Blobs are immutable and de-duplicated automatically.
//!
//! **URI journal** (`.ta/governed/journal.jsonl`): An append-only log of every
//! governed-path event. Each line is a JSON object (`JournalEntry`).  Events:
//! - `snapshot` — pre-goal baseline recorded before an agent runs
//! - `write` — a write to the governed path, referencing the new SHA blob
//! - `denied` — a draft-deny event preventing further replay of a write
//! - `applied` — a draft-apply event confirming the write reached the real path
//! - `rolled_back` — a rollback that restored the pre-goal snapshot
//!
//! `GovernedPathManager` ties these together and is the only public API callers
//! need for apply, deny, snapshot, and GC operations.

pub mod config;
pub mod error;
pub mod journal;
pub mod manager;
pub mod sha_store;

pub use config::{GovernedPathConfig, PathMode};
pub use error::GovernedPathError;
pub use journal::{JournalAction, JournalEntry, UriJournal};
pub use manager::GovernedPathManager;
pub use sha_store::ShaStore;
