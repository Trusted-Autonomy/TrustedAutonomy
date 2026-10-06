//! Shared test-only utilities for `apps/ta-cli/src/commands/`.
//!
//! Rust's test runner executes tests in parallel threads within one process
//! by default. Any test that temporarily swaps `$HOME` (via
//! `std::env::set_var("HOME", ...)` / `remove_var`) races with every other
//! test in the same binary that reads or mutates `$HOME` concurrently,
//! regardless of which module each test lives in -- a `Mutex` scoped to a
//! single module only serializes against tests in that same module, not
//! against a separate, independently-declared lock in another module.
//!
//! `HOME_ENV_LOCK` is the single shared lock for the whole `ta-cli` test
//! binary: every test (in any `commands::*` module) that reads or mutates
//! `$HOME` must acquire this lock for the duration of that read/mutation.
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
