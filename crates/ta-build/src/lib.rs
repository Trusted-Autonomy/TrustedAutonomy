//! Build adapters for project build/test integration.
//!
//! This crate provides pluggable adapters for project build and test
//! operations. The core abstraction is the `BuildAdapter` trait, with
//! built-in implementations for Cargo, npm, script (arbitrary commands),
//! and webhook (external CI — stub).

pub mod adapter;
pub mod cargo;
pub mod npm;
pub mod registry;
pub mod script;
pub mod webhook;

// Primary exports
pub use adapter::{BuildAdapter, BuildError, BuildResult};
pub use cargo::CargoAdapter;
pub use npm::NpmAdapter;
pub use registry::{
    detect_build_adapter, known_build_adapters, select_build_adapter, BuildAdapterConfig,
};
pub use script::ScriptAdapter;
pub use webhook::WebhookAdapter;
