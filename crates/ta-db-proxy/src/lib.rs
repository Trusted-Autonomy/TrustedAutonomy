//! ta-db-proxy — DbProxyPlugin trait for TA database proxy governance.
//!
//! Plugins intercept agent database connections, enforce policies, and capture
//! mutations through DraftOverlay for human review.

pub mod classification;
pub mod error;
pub mod external_plugin;
pub mod plugin;
pub mod registry;

pub use classification::{MutationKind, QueryClass};
pub use error::ProxyError;
pub use external_plugin::ExternalDbProxyPlugin;
pub use plugin::{DbProxyPlugin, ProxyConfig, ProxyHandle};
pub use registry::{DbAdapterEntry, DbAdapterRegistry};
