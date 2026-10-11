//! # ta-lifecycle
//!
//! Daemon self-update when idle, as a small reusable library. Used by the
//! `ta` CLI, by `ta-daemon`, and by the VT poller daemon (a separate
//! repository that depends on this crate by git revision).
//!
//! Dependencies are std plus serde on purpose. HTTP is not in here: callers
//! supply it behind [`DrainSource`] and [`UpdateEnv`].
//!
//! | Need | API |
//! |------|-----|
//! | Compare running vs installed build | [`BuildIdentity`], [`compare_builds`] |
//! | Is it idle? | [`DrainSnapshot`], [`apply_lock_state`], [`InflightCounter`] |
//! | Drain-aware restart | [`wait_for_idle`], [`drain_then_restart`] |
//! | Find the sibling binary and its build | [`locate_sibling_binary`], [`read_installed_identity`] |
//! | macOS signing (never ad-hoc) | [`ensure_stable_codesign`] |
//! | One full self-update check | [`run_update_check`], [`UpdateEnv`], [`RestartGuard`] |
//! | The `[daemon] auto_update` setting | [`AutoUpdateMode`] |
//!
//! ## Contract for a supervised daemon (the VT poller)
//!
//! 1. On a timer, read the installed binary's identity and compare it with
//!    the running one ([`compare_builds`]). Same build: do nothing.
//! 2. When stale, wait for idle using the daemon's *own* in-flight records
//!    ([`DrainSnapshot::from_inflight`] or a counter). Never cut work short.
//! 3. When idle, exit with [`EXIT_FOR_SUPERVISOR_RESTART`] (or re-exec) so
//!    the supervisor (launchd, systemd, a service wrapper) starts the new
//!    binary. Guard against loops with [`RestartGuard`].
//!
//! ```
//! use ta_lifecycle::{compare_builds, BuildIdentity, DrainSnapshot, InflightCounter};
//!
//! // What the poller is running, and what `ta-poller --version` prints now.
//! let running = BuildIdentity::new("2.4.0", Some("aaa1111"));
//! let installed = BuildIdentity::parse_version_output("ta-poller 2.4.0 (bbb2222)").unwrap();
//!
//! // The poller's own in-flight records: one message is being processed.
//! let inflight = InflightCounter::new();
//! let working = inflight.begin();
//!
//! let stale = compare_builds(&running, &installed).is_stale();
//! assert!(stale);
//! // Busy: keep working, report why the update is pending.
//! let snap = DrainSnapshot::from_inflight(inflight.count());
//! assert!(!snap.is_idle());
//! assert_eq!(snap.blockers(), vec!["1 in-flight wake launch".to_string()]);
//!
//! // Work finishes; now idle, so exit and let the supervisor restart us.
//! drop(working);
//! assert!(DrainSnapshot::from_inflight(inflight.count()).is_idle());
//! // std::process::exit(ta_lifecycle::EXIT_FOR_SUPERVISOR_RESTART);
//! ```

pub mod binary;
pub mod clock;
pub mod codesign;
pub mod drain;
pub mod guard;
pub mod mode;
pub mod update;
pub mod version;

pub use binary::{
    binary_file_name, find_on_path, locate_binary_from, locate_sibling_binary,
    read_installed_identity, read_installed_identity_with, run_version_command, sibling_of,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use codesign::{
    ensure_stable_codesign, SignOutcome, SignRequest, Signer, SystemCodesign, CLI_IDENTIFIER,
    DAEMON_IDENTIFIER,
};
pub use drain::{
    apply_lock_state, drain_then_restart, process_is_alive, wait_for_idle, ApplyLockState,
    DrainSnapshot, DrainSource, DrainTimeout, DrainWaitOptions, InflightCounter, InflightGuard,
    RestartError,
};
pub use guard::{GuardDecision, GuardState, RestartGuard};
pub use mode::{AutoUpdateMode, ACCEPTED_VALUES};
pub use update::{run_update_check, Level, UpdateEnv, UpdateOutcome, UpdateReport};
pub use version::{compare_builds, BuildComparison, BuildIdentity};

/// Exit status a supervised daemon uses to say "restart me onto the new
/// build" (`EX_TEMPFAIL`). Configure the supervisor to restart on any exit.
pub const EXIT_FOR_SUPERVISOR_RESTART: i32 = 75;

/// Replace the current process with a fresh copy of itself (Unix `exec`).
/// Only returns on failure, with the error. On other platforms it returns an
/// error and the caller should exit for its supervisor instead.
pub fn reexec_current_exe() -> std::io::Error {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        match std::env::current_exe() {
            Ok(exe) => std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .exec(),
            Err(e) => e,
        }
    }
    #[cfg(not(unix))]
    {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "re-exec is not supported on this platform; exit with EXIT_FOR_SUPERVISOR_RESTART so the supervisor restarts the process",
        )
    }
}
