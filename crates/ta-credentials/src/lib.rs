//! # ta-credentials
//!
//! Credential vault and identity broker for Trusted Autonomy.
//!
//! Agents must never hold raw credentials. TA acts as an identity broker —
//! agents request access, TA provides scoped, short-lived session tokens.
//!
//! ## Backends
//!
//! - **FileVault** (default): JSON file at `.ta/credentials.json` with
//!   restrictive file permissions. Suitable for local development.
//!
//! ## Usage
//!
//! ```ignore
//! let config = CredentialsConfig::for_project(".");
//! let mut vault = FileVault::open(&config)?;
//! let cred = vault.add("gmail", "google", "token...", vec!["gmail.send".into()])?;
//! let token = vault.issue_token(cred.id, "agent-1", vec!["gmail.send".into()], 3600)?;
//! ```

pub mod config;
pub mod error;
pub mod file_vault;
pub mod vault;

pub use config::CredentialsConfig;
pub use error::VaultError;
pub use file_vault::FileVault;
pub use vault::{Credential, CredentialSummary, CredentialVault, SessionToken};
