//! ta-db-proxy — DbProxyPlugin trait for TA database proxy governance.
//!
//! Plugins intercept agent database connections, enforce policies, and capture
//! mutations through DraftOverlay for human review.

pub mod capture;
pub mod classification;
pub mod constitution;
pub mod error;
pub mod external_plugin;
pub mod plugin;
pub mod registry;
pub mod review;

pub use capture::{CaptureAction, CaptureHandle, CaptureParams};
pub use classification::{MutationKind, QueryClass};
pub use constitution::{
    check_draft as check_constitution, has_schema_altering_statement, rows_modified,
};
pub use error::ProxyError;
pub use external_plugin::ExternalDbProxyPlugin;
pub use plugin::{DbProxyPlugin, ProxyConfig, ProxyHandle};
pub use registry::{DbAdapterEntry, DbAdapterRegistry};
pub use review::{classify_for_review, review_mutation};
