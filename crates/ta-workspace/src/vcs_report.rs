// vcs_report.rs — Machine-readable result of `ta setup vcs` (v0.17.12.5).
//
// The same document is printed by `ta setup vcs --json` (with or without
// `--dry-run`) and embedded as the `vcs` field of `ta init --json`. It carries
// the detector's evidence list so the VT installer report and the Wayfinder
// wizard can show why a VCS was chosen, not only which one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vcs_detect::{VcsDetection, VcsKind};

/// What happened (or would happen) to the ignore target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreAction {
    /// The ignore file did not exist and was written.
    Created,
    /// The TA block was added or refreshed.
    Updated,
    /// The TA block was already complete; nothing changed.
    Unchanged,
    /// Dry run: the file would be created.
    WouldCreate,
    /// Dry run: the TA block would be added or refreshed.
    WouldUpdate,
    /// Nothing was written; see `note` for why and what to do.
    Skipped,
}

/// Result for the VCS ignore target (`.gitignore`, `.p4ignore`, or the
/// `svn:ignore` property on `.ta`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IgnoreReport {
    /// Where the rules go: `.gitignore`, `.p4ignore`, or `svn:ignore on .ta`.
    pub target: String,
    pub action: IgnoreAction,
    /// Entries the TA block gained (all of them when it is new).
    pub added_entries: Vec<String>,
    /// `.ta/` lines in the existing block that TA does not recognize. They are
    /// preserved, never removed.
    pub preserved_unrecognized: Vec<String>,
    /// Why nothing was written, or what to do next.
    pub note: Option<String>,
}

/// Full report for one `ta setup vcs` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsSetupReport {
    /// JSON shape version (`VCS_DETECTION_SCHEMA_VERSION`).
    pub schema_version: u32,
    pub dry_run: bool,
    /// `--force` was given.
    pub forced: bool,
    /// `detected` or `override` (`--vcs` was given).
    pub source: String,
    /// The VCS setup acted on (equals `detection.kind` unless overridden).
    pub kind: VcsKind,
    /// Plugin name when `kind` is `plugin`, otherwise null.
    pub plugin: Option<String>,
    /// What the detector found and why, regardless of any override.
    pub detection: VcsDetection,
    /// Ignore-file result; null for `none` and `plugin`.
    pub ignore: Option<IgnoreReport>,
    /// Changes (or would-be changes) to `.ta/workflow.toml`.
    pub workflow_changes: Vec<String>,
    /// Warnings and next steps.
    pub notes: Vec<String>,
}

/// JSON document printed by bare `ta init --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InitReport {
    /// JSON shape version (`VCS_DETECTION_SCHEMA_VERSION`).
    pub schema_version: u32,
    pub project: String,
    /// True when `.ta/` was already set up before this run (a re-run).
    pub already_initialized: bool,
    /// Project-root-relative paths created by this run.
    pub created: Vec<String>,
    /// Paths that already existed and were left untouched.
    pub skipped: Vec<String>,
    /// VCS setup result, including the detection evidence. Null only when VCS
    /// setup failed; `vcs_error` then says why.
    pub vcs: Option<VcsSetupReport>,
    pub vcs_error: Option<String>,
}
