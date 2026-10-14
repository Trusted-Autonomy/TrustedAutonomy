//! # ta-credential-broker
//!
//! Biscuit-backed replacement for `ta_credentials::FileVault`'s UUID-keyed
//! `SessionToken`. A [`CredentialBroker`] mints self-contained, offline
//! verifiable grants: any process holding the broker's public key can verify
//! a presented token without a round trip to whichever process minted it,
//! unlike a UUID that only resolves against the vault process that issued it.

pub mod broker;
pub mod error;
pub mod shim;

pub use broker::{CredentialBroker, GrantedToken, VerifiedGrant};
pub use error::BrokerError;
#[cfg(unix)]
pub use shim::resolve_via_endpoint;
pub use shim::{
    encode_endpoint_reply, parse_endpoint_reply, resolve_for_host, ShimError, ShimResolution,
    CREDENTIAL_PURPOSE,
};
