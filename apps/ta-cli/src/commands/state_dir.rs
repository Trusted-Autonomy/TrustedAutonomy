//! `ta state`: where TA keeps its per-user state, and how to move it.
//!
//! * `ta state show` prints the resolved directory and which rule decided it
//!   (`TA_STATE_DIR`, the persisted pointer, or the OS default), plus the
//!   state of the old location.
//! * `ta state relocate --to <dir>` points every `ta` and `ta-daemon` process
//!   of this user at `<dir>` and migrates the existing state there, once,
//!   verified, without touching the old copy. Installers call this.
//! * `ta state user-dir` prints the directory that holds the pointer file, so
//!   shell scripts do not re-implement the rule.
//!
//! The decision itself lives in `ta_credentials::state_dir`; this file only
//! renders it.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Subcommand;
use ta_credentials::state_dir::{
    self, AutoMigration, Diagnosis, LegacyStatus, MigrateCtx, MigrationOutcome, MigrationReport,
    StaleTemp,
};

#[derive(Subcommand, Debug)]
pub enum StateCommands {
    /// Show the resolved TA state directory and which rule decided it.
    ///
    /// Precedence: the TA_STATE_DIR environment variable, then the persisted
    /// pointer file (written by `ta state relocate`), then the OS default.
    Show {
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
        /// Print only the resolved directory (for scripts).
        #[arg(long, conflicts_with = "json")]
        path_only: bool,
    },
    /// Print the per-user directory that holds the pointer file.
    ///
    /// That is $TA_USER_STATE_DIR, else ~/.config/ta/human. Scripts use this
    /// instead of re-implementing the rule.
    UserDir,
    /// Move TA's state to a new directory and remember it for every process.
    ///
    /// Copies the current state to a temporary directory beside the target,
    /// verifies file count, permissions and SHA-256 of every file, then renames
    /// it into place and writes the persisted pointer. The old directory is
    /// never modified except for a MIGRATED-TO marker, and never deleted: the
    /// command prints its path, size and the command that removes it.
    /// Refuses while a daemon or NATS server is running against the old
    /// directory, and refuses (changing nothing) when both directories already
    /// hold state. A second run is a no-op.
    ///
    /// Example:
    ///   ta state relocate --to /Volumes/Data/ta-state
    Relocate {
        /// The new state directory (absolute path; created if missing).
        #[arg(long)]
        to: PathBuf,
        /// Adopt the target as it is: set the pointer without copying anything.
        #[arg(long)]
        no_migrate: bool,
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
    },
}

pub fn prints_json(cmd: &StateCommands) -> bool {
    match cmd {
        StateCommands::Show { json, .. } | StateCommands::Relocate { json, .. } => *json,
        StateCommands::UserDir => false,
    }
}

pub fn execute(cmd: &StateCommands) -> anyhow::Result<()> {
    match cmd {
        StateCommands::Show { json, path_only } => show(*json, *path_only),
        StateCommands::UserDir => user_dir(),
        StateCommands::Relocate {
            to,
            no_migrate,
            json,
        } => relocate(to, *no_migrate, *json),
    }
}

fn user_dir() -> anyhow::Result<()> {
    let dir = ta_credentials::human::user_state_dir_with(
        &|k| std::env::var(k).ok(),
        dirs::home_dir().as_deref(),
    )
    .ok_or(state_dir::StateDirError::NoUserDir)?;
    println!("{}", dir.display());
    Ok(())
}

// ── show ───────────────────────────────────────────────────────────────────

fn show(json: bool, path_only: bool) -> anyhow::Result<()> {
    let d = state_dir::diagnose();
    if path_only {
        return match &d.resolved {
            Ok(r) => {
                println!("{}", r.root.display());
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("{e}")),
        };
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&diagnosis_json(&d))?);
        return resolved_or_err(&d);
    }
    print!("{}", render_show(&d));
    resolved_or_err(&d)
}

fn resolved_or_err(d: &Diagnosis) -> anyhow::Result<()> {
    match &d.resolved {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

fn diagnosis_json(d: &Diagnosis) -> serde_json::Value {
    let (resolved, source, error) = match &d.resolved {
        Ok(r) => (
            Some(r.root.display().to_string()),
            Some(r.source.short()),
            None,
        ),
        Err(e) => (None, None, Some(e.clone())),
    };
    let (legacy_state, legacy_detail) = legacy_json(&d.legacy);
    serde_json::json!({
        "resolved": resolved,
        "source": source,
        "error": error,
        "env": d.env_value,
        "pointer_file": d.pointer_file.as_ref().map(|p| p.display().to_string()),
        "pointer_value": d.pointer_value.as_ref().map(|p| p.display().to_string()),
        "pointer_error": d.pointer_error,
        "env_and_pointer_diverge": d.diverged,
        "user_dir": d.user_dir.as_ref().map(|p| p.display().to_string()),
        "os_default": d.os_default.as_ref().map(|p| p.display().to_string()),
        "legacy": legacy_state,
        "legacy_detail": legacy_detail,
        "stale_temp_dirs": d.stale_temps.iter().map(|t| t.path.display().to_string()).collect::<Vec<_>>(),
    })
}

fn legacy_json(l: &LegacyStatus) -> (&'static str, serde_json::Value) {
    match l {
        LegacyStatus::NotApplicable => ("not-applicable", serde_json::Value::Null),
        LegacyStatus::Absent => ("absent", serde_json::Value::Null),
        LegacyStatus::Unmigrated(i) => (
            "unmigrated",
            serde_json::json!({ "path": i.path.display().to_string(), "bytes": i.bytes }),
        ),
        LegacyStatus::BothHaveState { legacy, current } => (
            "both-have-state",
            serde_json::json!({
                "old": legacy.path.display().to_string(),
                "old_bytes": legacy.bytes,
                "new": current.path.display().to_string(),
                "new_bytes": current.bytes,
            }),
        ),
        LegacyStatus::MigratedKept { to, info } => (
            "migrated-kept",
            serde_json::json!({
                "path": info.path.display().to_string(),
                "bytes": info.bytes,
                "migrated_to": to.display().to_string(),
            }),
        ),
    }
}

fn render_show(d: &Diagnosis) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "TA state directory");
    match &d.resolved {
        Ok(r) => {
            let _ = writeln!(out, "  Resolved:        {}", r.root.display());
            let _ = writeln!(out, "  Decided by:      {}", r.source.describe());
        }
        Err(e) => {
            let _ = writeln!(out, "  Resolved:        (none) {e}");
        }
    }
    let _ = writeln!(
        out,
        "  TA_STATE_DIR:    {}",
        d.env_value.as_deref().unwrap_or("(unset)")
    );
    let pointer = match (&d.pointer_file, &d.pointer_value, &d.pointer_error) {
        (_, _, Some(e)) => format!("UNUSABLE: {e}"),
        (Some(f), Some(v), None) => format!("{} -> {}", f.display(), v.display()),
        (Some(f), None, None) => format!("{} (not set)", f.display()),
        (None, _, None) => "(no home directory)".to_string(),
    };
    let _ = writeln!(out, "  Pointer file:    {pointer}");
    if d.diverged {
        let _ = writeln!(
            out,
            "  WARNING:         TA_STATE_DIR and the pointer name different directories; \
             processes started with TA_STATE_DIR in their environment use it, others use the \
             pointer. Unset TA_STATE_DIR, or run `ta state relocate --to <dir>` with the one you want."
        );
    }
    if let Some(p) = &d.os_default {
        let _ = writeln!(out, "  OS default:      {}", p.display());
    }
    if let Some(p) = &d.user_dir {
        let _ = writeln!(out, "  Per-user dir:    {}", p.display());
    }
    match &d.legacy {
        LegacyStatus::NotApplicable | LegacyStatus::Absent => {}
        LegacyStatus::Unmigrated(i) => {
            let _ = writeln!(
                out,
                "  Old location:    {}\n                   not migrated yet. Run `ta state relocate --to {}` to move it.",
                i.describe(),
                d.resolved
                    .as_ref()
                    .map(|r| state_dir::shell_quote(&r.root))
                    .unwrap_or_else(|_| "<dir>".into())
            );
        }
        LegacyStatus::BothHaveState { legacy, current } => {
            let _ = writeln!(
                out,
                "  Old location:    {}\n  CONFLICT:        the current directory also holds state: {}\n                   Nothing is merged automatically. See `ta state relocate --help`.",
                legacy.describe(),
                current.describe()
            );
        }
        LegacyStatus::MigratedKept { to, info } => {
            let _ = writeln!(
                out,
                "  Old location:    {}\n                   already migrated to {}; the old copy is kept. Remove it when satisfied: rm -rf {}",
                info.describe(),
                to.display(),
                state_dir::shell_quote(&info.path)
            );
        }
    }
    render_stale(&mut out, &d.stale_temps);
    out
}

fn render_stale(out: &mut String, stale: &[StaleTemp]) {
    use std::fmt::Write as _;
    for t in stale {
        let _ = writeln!(
            out,
            "  Stale temp dir:  {} ({}) from an interrupted migration{}; it is never used as state. Remove it: rm -rf {}",
            t.path.display(),
            state_dir::human_size(t.bytes),
            t.pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
            state_dir::shell_quote(&t.path)
        );
    }
}

// ── relocate ───────────────────────────────────────────────────────────────

fn migrate_ctx() -> MigrateCtx<'static> {
    MigrateCtx {
        is_alive: &ta_lifecycle::process_is_alive,
        tool_version: env!("CARGO_PKG_VERSION"),
    }
}

fn relocate(to: &Path, no_migrate: bool, json: bool) -> anyhow::Result<()> {
    let out = state_dir::relocate(to, !no_migrate, &migrate_ctx()).with_context(|| {
        format!(
            "could not relocate the TA state directory to {}",
            to.display()
        )
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&relocate_json(&out))?);
        return Ok(());
    }
    let mut text = String::new();
    render_relocate(&mut text, &out);
    print!("{text}");
    Ok(())
}

fn migration_json(m: &Option<MigrationOutcome>) -> serde_json::Value {
    match m {
        None => serde_json::json!({ "result": "skipped" }),
        Some(MigrationOutcome::NothingToMigrate { reason }) => {
            serde_json::json!({ "result": "nothing-to-migrate", "reason": reason })
        }
        Some(MigrationOutcome::AlreadyMigrated { from, to, .. }) => serde_json::json!({
            "result": "already-migrated",
            "from": from.display().to_string(),
            "to": to.display().to_string(),
        }),
        Some(MigrationOutcome::Migrated(r)) => serde_json::json!({
            "result": "migrated",
            "from": r.from.display().to_string(),
            "to": r.to.display().to_string(),
            "files": r.files,
            "dirs": r.dirs,
            "bytes": r.bytes,
            "old_directory_kept": r.from.display().to_string(),
            "old_directory_bytes": r.old_info.bytes,
            "remove_old_with": format!("rm -rf {}", state_dir::shell_quote(&r.from)),
        }),
    }
}

fn relocate_json(o: &state_dir::RelocateOutcome) -> serde_json::Value {
    serde_json::json!({
        "to": o.to.display().to_string(),
        "pointer_file": o.pointer_file.display().to_string(),
        "pointer_changed": o.pointer_changed,
        "migration": migration_json(&o.migration),
        "env_override": o.env_override,
    })
}

fn render_report(out: &mut String, r: &MigrationReport) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "Migrated TA state\n  from:  {}\n  to:    {}\n  copied {} file(s), {} dir(s), {} ({} verified by SHA-256, permissions kept)",
        r.from.display(),
        r.to.display(),
        r.files,
        r.dirs,
        state_dir::human_size(r.bytes),
        r.files
    );
    if !r.tightened.is_empty() {
        let _ = writeln!(
            out,
            "  tightened {} file(s) that were readable beyond the owner",
            r.tightened.len()
        );
    }
    for s in &r.skipped {
        let _ = writeln!(out, "  skipped (not a file or directory): {}", s.display());
    }
    let _ = writeln!(
        out,
        "  markers: {} in the new directory, {} in the old one",
        state_dir::MIGRATED_FROM_MARKER,
        state_dir::MIGRATED_TO_MARKER
    );
    if let Some(e) = &r.old_marker_error {
        let _ = writeln!(
            out,
            "  WARNING: could not write {} in the old directory: {e}. The migration itself is complete.",
            state_dir::MIGRATED_TO_MARKER
        );
    }
    let _ = writeln!(
        out,
        "\nThe old directory was NOT deleted or changed (apart from the marker).\n  old:    {} ({})\n  remove it once you have checked the new one:\n    rm -rf {}",
        r.from.display(),
        state_dir::human_size(r.old_info.bytes),
        state_dir::shell_quote(&r.from)
    );
    render_stale(out, &r.stale_temps);
}

fn render_relocate(out: &mut String, o: &state_dir::RelocateOutcome) {
    use std::fmt::Write as _;
    match &o.migration {
        Some(MigrationOutcome::Migrated(r)) => render_report(out, r),
        Some(MigrationOutcome::AlreadyMigrated {
            from,
            to,
            stale_temps,
        }) => {
            let _ = writeln!(
                out,
                "Already migrated: {} was copied to {} earlier. Nothing to do.",
                from.display(),
                to.display()
            );
            render_stale(out, stale_temps);
        }
        Some(MigrationOutcome::NothingToMigrate { reason }) => {
            let _ = writeln!(out, "Nothing to migrate: {reason}.");
        }
        None => {
            let _ = writeln!(
                out,
                "No data was copied (--no-migrate, or this directory is already in use)."
            );
        }
    }
    let _ = writeln!(
        out,
        "{} {} -> {}",
        if o.pointer_changed {
            "Pointer set:"
        } else {
            "Pointer already set:"
        },
        o.pointer_file.display(),
        o.to.display()
    );
    if let Some(e) = &o.env_override {
        let _ = writeln!(
            out,
            "WARNING: TA_STATE_DIR={e} is set in this shell and overrides the pointer for processes \
             that inherit it. Unset it (or point it at {}) so every process agrees.",
            o.to.display()
        );
    }
    let _ = writeln!(
        out,
        "Every ta and ta-daemon process of this user now resolves to {} (check with `ta state show`).",
        o.to.display()
    );
}

// ── automatic migration at startup ─────────────────────────────────────────

/// Called by `ta` before any project command: when the resolver picked a
/// directory with no state while the OS default holds an unmigrated one, copy
/// it over once (verified, old data untouched). Failure is fatal for the
/// command: carrying on against an empty directory would mint a second vault
/// key and orphan the encrypted credentials.
pub fn auto_migrate_at_startup() -> anyhow::Result<()> {
    match state_dir::ensure_migrated(&migrate_ctx())
        .context("could not migrate the TA state directory to its configured location")?
    {
        AutoMigration::NotNeeded => Ok(()),
        AutoMigration::Migrated(r) => {
            let mut text = String::new();
            render_report(&mut text, &r);
            eprint!("{text}");
            Ok(())
        }
    }
}
