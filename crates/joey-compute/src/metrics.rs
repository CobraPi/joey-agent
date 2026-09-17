//! Pool metrics (feature 033, US5; contracts/api.md §2).
//!
//! Observability ONLY: counters and per-weight-class timing summaries for
//! operators/telemetry. There is deliberately no path from these
//! observations back into scheduling weights (determinism — data-model.md).
//! Summaries are constant-memory (count/total/max), never unbounded
//! sample vectors.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Scheduling weight classes (contracts/api.md §2): low 1.0–24.9, mid
/// 25.0–74.9, high 75.0–100.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightClass {
    Low,
    Mid,
    High,
}

impl WeightClass {
    /// Classify a (clamped) scheduling weight.
    pub fn classify(weight: f64) -> WeightClass {
        if weight < 25.0 {
            WeightClass::Low
        } else if weight < 75.0 {
            WeightClass::Mid
        } else {
            WeightClass::High
        }
    }

    /// Stable lowercase name.
    pub fn name(&self) -> &'static str {
        match self {
            WeightClass::Low => "low",
            WeightClass::Mid => "mid",
            WeightClass::High => "high",
        }
    }
}

/// Running summary of one timing series — constant memory regardless of
/// job volume (count, cumulative total, max).
#[derive(Debug, Default, Clone, Copy)]
pub struct TimingSummary {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

impl TimingSummary {
    fn record(&mut self, d: Duration) {
        let ns = d.as_nanos() as u64;
        self.count += 1;
        self.total_ns = self.total_ns.saturating_add(ns);
        self.max_ns = self.max_ns.max(ns);
    }

    /// Observations recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Mean observation (zero when empty).
    pub fn avg(&self) -> Duration {
        if self.count == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(self.total_ns / self.count)
        }
    }

    /// Worst observation (zero when empty).
    pub fn max(&self) -> Duration {
        Duration::from_nanos(self.max_ns)
    }
}

/// Per-weight-class timing summaries.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClassSummary {
    /// enqueue → pop latency.
    pub queue_wait: TimingSummary,
    /// pop → finish latency.
    pub service_time: TimingSummary,
}

/// A point-in-time view of every pool counter and summary.
#[derive(Debug, Default, Clone, Copy)]
pub struct MetricsSnapshot {
    pub submitted: u64,
    pub completed: u64,
    pub panicked: u64,
    pub cancelled_queued: u64,
    pub deadline_overruns: u64,
    pub in_flight: u64,
    pub low: ClassSummary,
    pub mid: ClassSummary,
    pub high: ClassSummary,
}

impl MetricsSnapshot {
    /// The summary for one weight class.
    pub fn class(&self, class: WeightClass) -> &ClassSummary {
        match class {
            WeightClass::Low => &self.low,
            WeightClass::Mid => &self.mid,
            WeightClass::High => &self.high,
        }
    }
}

#[derive(Debug, Default)]
struct Timings {
    low: ClassSummary,
    mid: ClassSummary,
    high: ClassSummary,
}

/// Atomic counters + per-class timing summaries (feature 033, US5).
#[derive(Debug, Default)]
pub struct Metrics {
    submitted: AtomicU64,
    completed: AtomicU64,
    panicked: AtomicU64,
    cancelled_queued: AtomicU64,
    deadline_overruns: AtomicU64,
    in_flight: AtomicU64,
    timings: Mutex<Timings>,
}

impl Metrics {
    /// Fresh zeroed metrics.
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Job enqueued (admitted and pushed onto the ready heap).
    pub(crate) fn on_submitted(&self) {
        Self::bump(&self.submitted);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    /// Job completed successfully.
    pub(crate) fn on_completed(&self) {
        Self::bump(&self.completed);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    /// Job panicked (isolated).
    pub(crate) fn on_panicked(&self) {
        Self::bump(&self.panicked);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    /// Queued job skipped because its receiver was dropped.
    pub(crate) fn on_cancelled_queued(&self) {
        Self::bump(&self.cancelled_queued);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    /// A popped job's deadline had already passed (waited longer than its
    /// scale/weight lead).
    pub(crate) fn on_deadline_overrun(&self) {
        Self::bump(&self.deadline_overruns);
    }

    /// Record the timing observations for one RUN job (skipped/cancelled
    /// jobs record nothing).
    pub(crate) fn observe(&self, weight: f64, queue_wait: Duration, service_time: Duration) {
        let mut t = self.timings.lock().unwrap();
        let class = match WeightClass::classify(weight) {
            WeightClass::Low => &mut t.low,
            WeightClass::Mid => &mut t.mid,
            WeightClass::High => &mut t.high,
        };
        class.queue_wait.record(queue_wait);
        class.service_time.record(service_time);
    }

    /// Point-in-time snapshot of every counter and summary.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let t = self.timings.lock().unwrap();
        MetricsSnapshot {
            submitted: self.submitted.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            panicked: self.panicked.load(Ordering::Relaxed),
            cancelled_queued: self.cancelled_queued.load(Ordering::Relaxed),
            deadline_overruns: self.deadline_overruns.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            low: t.low,
            mid: t.mid,
            high: t.high,
        }
    }
}
