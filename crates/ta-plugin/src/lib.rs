//! Shared JSON-over-stdio plugin transport, manifest schema, and discovery
//! convention for every Trusted Autonomy Plugin-category integration
//! (docs/design/ta-concepts-and-architecture.md §2.2): VCS, messaging,
//! social, agent-runtime, tool, db, and release plugins all use this crate.

pub mod discovery;
pub mod envelope;
pub mod error;
pub mod manifest;
pub mod transport;

pub use discovery::{
    discover_plugins, find_plugin, user_config_dir, DiscoveredPlugin, PluginSource,
};
pub use envelope::{
    HandshakeParams, HandshakeResult, PluginRequest, PluginResponse, PROTOCOL_VERSION,
};
pub use error::PluginError;
pub use manifest::PluginManifest;
