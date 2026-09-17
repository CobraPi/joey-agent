//! Overrun watchdog for pool jobs (feature 033, US5; contracts/api.md §2).
//!
//! `Watchdog::guard(f)` runs `f` to completion, timing it. If `f` runs
//! longer than the configured limit, a single `tracing::warn!` is emitted
//! — and NOTHING else: the watchdog never kills threads, never aborts the
//! op, never intervenes. Truly hung non-cooperative jobs require
//! process-level handling (e.g. a supervisor process); that is explicitly
//! out of scope for v1 and documented as a recipe in docs/compute-pool.md
//! instead.

use std::time::{Duration, Instant};

/// A per-job overrun timer. The limit is caller-configured per job and is
/// independent of the pool's `scale_ms` (default 30 s).
pub struct Watchdog {
    limit: Duration,
}

impl Watchdog {
    /// A watchdog with the default 30-second limit.
    pub fn new(limit: Duration) -> Self {
        Self { limit }
    }

    /// Run `f` under watchdog observation. `f` ALWAYS runs to completion;
    /// an overrun emits one `tracing::warn!` (once per guard invocation,
    /// not per tick) with the elapsed time. Returns `f`'s value.
    pub fn guard<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = Instant::now();
        let result = f();
        let elapsed = start.elapsed();
        if elapsed > self.limit {
            tracing::warn!(
                limit_ms = self.limit.as_millis() as u64,
                elapsed_ms = elapsed.as_millis() as u64,
                "compute job overran watchdog limit (job completed; hang-killing is out of scope v1)"
            );
        }
        result
    }

    /// The configured limit.
    pub fn limit(&self) -> Duration {
        self.limit
    }
}
