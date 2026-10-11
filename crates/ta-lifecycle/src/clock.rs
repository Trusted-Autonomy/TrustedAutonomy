//! Injectable clock so waits and backoff are testable without real sleeping.

use std::time::Duration;

pub trait Clock {
    /// Seconds since the Unix epoch.
    fn now_unix(&self) -> u64;
    fn sleep(&self, d: Duration);
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// A simulated clock: `sleep` advances time instantly.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: std::sync::atomic::AtomicU64,
}

impl ManualClock {
    pub fn new(start_unix: u64) -> Self {
        Self {
            now: std::sync::atomic::AtomicU64::new(start_unix),
        }
    }
    pub fn advance(&self, secs: u64) {
        self.now
            .fetch_add(secs, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> u64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn sleep(&self, d: Duration) {
        self.advance(d.as_secs().max(1));
    }
}
