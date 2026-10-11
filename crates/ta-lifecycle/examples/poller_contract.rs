//! Runnable example of the supervised-daemon contract (VT poller).
//!
//!     cargo run -p ta-lifecycle --example poller_contract
//!
//! Simulates a poller with its own in-flight records: it notices a newer
//! installed build, keeps working while busy, and exits for its supervisor
//! once idle. The real poller replaces the simulated pieces (timer, installed
//! binary path, work loop) and calls `std::process::exit` at the end.

use std::path::Path;

use ta_lifecycle::{
    compare_builds, read_installed_identity_with, BuildIdentity, DrainSnapshot, InflightCounter,
    RestartGuard, EXIT_FOR_SUPERVISOR_RESTART,
};

fn main() {
    let running = BuildIdentity::new("2.4.0", Some("aaa1111"));
    let guard_dir = std::env::temp_dir().join("ta-lifecycle-poller-example");
    let guard = RestartGuard::at(guard_dir.join("poller-update.json"));

    // 1. Compare. A real poller omits the closure and runs `<bin> --version`.
    let installed = read_installed_identity_with(Path::new("ta-poller"), &|_| {
        Ok("ta-poller 2.4.0 (bbb2222)\n".to_string())
    })
    .expect("installed binary reports its version");
    if !compare_builds(&running, &installed).is_stale() {
        println!("up to date: {}", running.describe());
        return;
    }
    println!(
        "update available: {} -> {}",
        running.describe(),
        installed.describe()
    );

    // 2. Wait for idle using the poller's own in-flight records.
    let inflight = InflightCounter::new();
    let job = inflight.begin();
    let snap = DrainSnapshot::from_inflight(inflight.count());
    println!("update pending, waiting for {}", snap.blockers().join(", "));
    drop(job);

    // 3. Idle: record the attempt (loop guard), then exit for the supervisor.
    let snap = DrainSnapshot::from_inflight(inflight.count());
    assert!(snap.is_idle());
    guard
        .record_attempt(&installed, &running, 1, 0)
        .expect("loop guard state is writable");
    println!(
        "idle: exit {EXIT_FOR_SUPERVISOR_RESTART} so the supervisor starts {}",
        installed.describe()
    );
    // std::process::exit(EXIT_FOR_SUPERVISOR_RESTART);
    guard.clear();
}
