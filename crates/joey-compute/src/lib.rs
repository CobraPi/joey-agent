//! CPU-bound work execution substrate (feature 033, spec
//! `specs/033-please-reference-plan`).
//!
//! # The hard rule
//!
//! CPU work submitted to this crate NEVER runs on tokio worker threads.
//! Tokio is used solely for the async interface surface — the admission
//! semaphore and the one-shot completion channel. Every submitted
//! operation executes on this pool's own dedicated OS threads
//! (`compute-{i}`), so saturating the pool never starves the async
//! runtime.

use std::collections::BinaryHeap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub mod chunk;
pub use chunk::run_chunked;

pub mod watchdog;

pub mod metrics;
pub use metrics::{ClassSummary, Metrics, MetricsSnapshot, TimingSummary, WeightClass};

/// A plain numeric identifier for the submitting agent (contracts/api.md §2).
/// Used for accounting only — never exclusivity: two jobs may share one
/// identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AgentId(pub u64);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent-{}", self.0)
    }
}

/// Error outcomes for a submitted job (contracts/api.md §2). Exactly these
/// three variants — distinct and exhaustive; callers match on precisely
/// these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    /// The job's receiver was dropped while the job was still queued (or the
    /// job observed cancellation); the job never ran or stopped early.
    Cancelled,
    /// The operation panicked; the panic was isolated to this job.
    Panicked,
    /// The pool is closed; the job was never accepted.
    PoolClosed,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Cancelled => write!(f, "job cancelled"),
            JobError::Panicked => write!(f, "job panicked"),
            JobError::PoolClosed => write!(f, "compute pool closed"),
        }
    }
}

impl std::error::Error for JobError {}

/// Cooperative cancellation token (contracts/api.md §2). `is_cancelled()`
/// query; idempotent `set()`; cooperative only — setting never forces
/// termination, well-behaved jobs stop at their next chunk boundary.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cancelled flag. Idempotent: setting an already-cancelled
    /// token is a no-op.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A job submission: the submitting agent, the scheduling weight, and the
/// operation to run (contracts/api.md §2). The operation receives a
/// reference to the job's [`CancelToken`] and returns the job's value.
pub struct JobSpec<T> {
    /// Submitting agent (accounting only, never exclusivity).
    pub agent: AgentId,
    /// Scheduling weight; the pool clamps to [1.0, 100.0] (NaN → 1.0) at
    /// admission (research.md D2).
    pub weight: f64,
    /// The CPU-bound operation. Runs on a dedicated pool thread, NEVER on a
    /// tokio worker (crate hard rule).
    pub op: Box<dyn FnOnce(&CancelToken) -> T + Send + 'static>,
}

impl<T> fmt::Debug for JobSpec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobSpec")
            .field("agent", &self.agent)
            .field("weight", &self.weight)
            .field("op", &"<closure>")
            .finish()
    }
}

impl<T: Send + 'static> JobSpec<T> {
    /// Build a job spec from an agent id, a weight, and an operation.
    pub fn new<F>(agent: AgentId, weight: f64, op: F) -> Self
    where
        F: FnOnce(&CancelToken) -> T + Send + 'static,
    {
        Self { agent, weight, op: Box::new(op) }
    }
}

/// Minimum scheduling weight (contracts/api.md §2).
pub const MIN_WEIGHT: f64 = 1.0;

/// Maximum scheduling weight (contracts/api.md §2).
pub const MAX_WEIGHT: f64 = 100.0;

/// Clamp a scheduling weight into [`MIN_WEIGHT`, `MAX_WEIGHT`]; NaN maps
/// to the minimum 1.0 (research.md D2, contracts/api.md §2). Shared by
/// the pool's submit path and the orchestrator's weight policy
/// (joey-orchestration `compute_weight`), so the clamp bounds live in
/// exactly one place.
pub fn clamp_weight(w: f64) -> f64 {
    if w.is_nan() {
        1.0
    } else {
        w.clamp(MIN_WEIGHT, MAX_WEIGHT)
    }
}

/// Ready-heap entry (contracts/api.md §2: internal entry ordering is NOT
/// part of the public contract — exposed for the T003 ordering tests
/// only).
///
/// `Ord` is deliberately INVERTED for [`std::collections::BinaryHeap`]'s
/// max-heap: the *greatest* entry is the most urgent — earliest `deadline`;
/// among equal deadlines, the smaller `seq` wins (FIFO).
///
/// Equality and ordering consider ONLY `(deadline, seq)`; the carried
/// `value` never participates, so `Entry<T>: Ord` holds for every `T`.
#[derive(Debug, Clone, Copy)]
pub struct Entry<T> {
    /// Absolute deadline = enqueue instant + scale / clamped weight.
    pub deadline: std::time::Instant,
    /// Monotonic enqueue sequence for FIFO tiebreaking among equal
    /// deadlines.
    pub seq: u64,
    /// The carried payload (the queued job internals).
    pub value: T,
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.seq == other.seq
    }
}

impl<T> Eq for Entry<T> {}

impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Inverted: earlier deadline ⇒ Greater; among equal deadlines,
        // smaller seq ⇒ Greater, so equal deadlines pop FIFO.
        other
            .deadline
            .cmp(&self.deadline)
            .then(other.seq.cmp(&self.seq))
    }
}

// PartialOrd delegates to the inverted `Ord` so the two agree (a derived
// PartialOrd would use natural field order and contradict `Ord`).
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── Pool core ──────────────────────────────────────────────────────────

/// Internal queued-job payload carried inside [`Entry`].
struct QueuedJob<T> {
    /// Submitting agent — accounting only.
    #[allow(dead_code)]
    agent: AgentId,
    cancel: CancelToken,
    op: Box<dyn FnOnce(&CancelToken) -> T + Send + 'static>,
    tx: Option<tokio::sync::oneshot::Sender<Result<T, JobError>>>,
    /// Admission permit — held while queued AND running; released when the
    /// worker finishes the job (the permit drops with this struct).
    _permit: tokio::sync::OwnedSemaphorePermit,
    /// Enqueue instant (metrics: queue wait = pop − enqueue).
    enqueued_at: Instant,
    /// Clamped scheduling weight (metrics: weight-class summaries).
    weight: f64,
}

/// State guarded by ONE mutex. The mutex is NEVER held across a job's
/// operation, and never acquired while holding the tokio admission
/// semaphore's internal state (the two synchronization domains never
/// nest).
struct PoolState<T> {
    heap: BinaryHeap<Entry<QueuedJob<T>>>,
    closed: bool,
    running: usize,
    /// queued + running at observation time (contracts §2 `in_flight`).
    in_flight: usize,
}

struct Inner<T> {
    state: Mutex<PoolState<T>>,
    notify: Condvar,
    semaphore: Arc<tokio::sync::Semaphore>,
    scale_ms: u64,
    seq: AtomicU64,
    workers: usize,
    metrics: metrics::Metrics,
}

/// A fixed-size pool of dedicated OS threads (`compute-{i}`) executing
/// CPU-bound jobs under deadline-form weighted fair scheduling with
/// bounded admission (feature 033). CPU work NEVER runs on tokio workers
/// (crate hard rule): tokio is used only for the admission semaphore and
/// the one-shot completion channel.
pub struct ComputePool<T> {
    inner: Arc<Inner<T>>,
}

impl<T: Send + 'static> ComputePool<T> {
    /// Build a pool of `workers` dedicated threads (>= 1), an admission
    /// bound of `max_inflight` queued+running jobs (>= 1), and a fairness
    /// scale of `scale_ms` (> 0).
    pub fn new(workers: usize, max_inflight: usize, scale_ms: u64) -> Self {
        let workers = workers.max(1);
        let inner = Arc::new(Inner {
            state: Mutex::new(PoolState {
                heap: BinaryHeap::new(),
                closed: false,
                running: 0,
                in_flight: 0,
            }),
            notify: Condvar::new(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_inflight.max(1))),
            scale_ms: scale_ms.max(1),
            seq: AtomicU64::new(0),
            workers,
            metrics: metrics::Metrics::new(),
        });
        for i in 0..workers {
            let worker_inner = inner.clone();
            std::thread::Builder::new()
                .name(format!("compute-{i}"))
                .spawn(move || worker_loop(worker_inner))
                .expect("failed to spawn compute worker thread");
        }
        Self { inner }
    }

    /// Configured worker-thread count.
    pub fn workers(&self) -> usize {
        self.inner.workers
    }

    /// Queued + running jobs at observation time (contracts §2).
    pub fn in_flight(&self) -> usize {
        self.inner.state.lock().unwrap().in_flight
    }

    /// Raw metrics handle (observability only — never scheduling input).
    pub fn metrics(&self) -> &metrics::Metrics {
        &self.inner.metrics
    }

    /// Point-in-time metrics snapshot (counters + per-class summaries +
    /// in-flight gauge).
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.inner.metrics.snapshot()
    }

    /// Submit a job (contracts §2): acquire an admission permit
    /// (backpressure when saturated — submitters wait, memory stays
    /// bounded), clamp the weight to [1.0, 100.0] with NaN → 1.0, compute
    /// `deadline = now + scale / weight`, push onto the ready heap, and
    /// resolve via a one-shot completion channel. After close: returns
    /// `PoolClosed` — never hangs.
    pub async fn submit(&self, spec: JobSpec<T>) -> Result<T, JobError> {
        let permit = self
            .inner
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| JobError::PoolClosed)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let weight = clamp_weight(spec.weight);
        let job = QueuedJob {
            agent: spec.agent,
            cancel: CancelToken::new(),
            op: spec.op,
            tx: Some(tx),
            _permit: permit,
            enqueued_at: Instant::now(),
            weight,
        };
        let lead_ms = (self.inner.scale_ms as f64 / weight).ceil().max(1.0);
        let deadline = job.enqueued_at + Duration::from_millis(lead_ms as u64);
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = self.inner.state.lock().unwrap();
            if st.closed {
                return Err(JobError::PoolClosed); // permit drops → released
            }
            st.in_flight += 1;
            st.heap.push(Entry {
                deadline,
                seq,
                value: job,
            });
        }
        self.inner.metrics.on_submitted();
        self.inner.notify.notify_one();
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(JobError::Cancelled),
        }
    }

    /// Drain-on-close (contracts §2): queued jobs run to completion, then
    /// workers stop. Idempotent.
    pub fn close(&self) {
        self.inner.state.lock().unwrap().closed = true;
        self.inner.notify.notify_all();
    }
}

/// Dedicated worker-thread body: pop the most urgent entry (earliest
/// deadline; FIFO among equal deadlines) → skip-if-receiver-closed →
/// running accounting → run (panic-isolated, state lock NOT held) →
/// accounting decrement → deliver result. The job's admission permit is
/// released when the job value drops at the end of an iteration — i.e. at
/// worker finish, not at submit. Metrics: queue wait = pop − enqueue,
/// service time = run duration; observations recorded only for jobs that
/// actually ran; deadline overruns counted at pop.
fn worker_loop<T: Send + 'static>(inner: Arc<Inner<T>>) {
    loop {
        let Entry {
            deadline,
            seq: _,
            value: mut job,
        } = {
            let mut st = inner.state.lock().unwrap();
            loop {
                match st.heap.pop() {
                    Some(entry) => break entry,
                    None => {
                        if st.closed {
                            return; // drained: queue empty and closed
                        }
                        st = inner.notify.wait(st).unwrap();
                    }
                }
            }
        };
        let popped_at = Instant::now();
        let queue_wait = popped_at - job.enqueued_at;
        // Receiver already dropped → the job never runs (contracts §2);
        // its permit releases when `job` drops at the `continue`.
        if job.tx.as_ref().map_or(true, |tx| tx.is_closed()) {
            inner.state.lock().unwrap().in_flight -= 1;
            inner.metrics.on_cancelled_queued();
            continue;
        }
        if popped_at > deadline {
            inner.metrics.on_deadline_overrun();
        }
        inner.state.lock().unwrap().running += 1;
        let cancel = job.cancel.clone();
        let started = Instant::now();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.op)(&cancel)));
        let service_time = started.elapsed();
        {
            let mut st = inner.state.lock().unwrap();
            st.running -= 1;
            st.in_flight -= 1;
        }
        inner
            .metrics
            .observe(job.weight, queue_wait, service_time);
        match result {
            Ok(value) => {
                inner.metrics.on_completed();
                if let Some(tx) = job.tx.take() {
                    let _ = tx.send(Ok(value));
                }
            }
            Err(_) => {
                inner.metrics.on_panicked();
                if let Some(tx) = job.tx.take() {
                    let _ = tx.send(Err(JobError::Panicked));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_set_is_idempotent_and_queryable() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        t.set();
        assert!(t.is_cancelled());
        t.set(); // idempotent
        assert!(t.is_cancelled());
    }

    #[test]
    fn cancel_token_clone_shares_state() {
        let t = CancelToken::new();
        let c = t.clone();
        c.set();
        assert!(t.is_cancelled());
    }

    #[test]
    fn job_spec_new_invokes_op_with_token() {
        let spec = JobSpec::new(AgentId(7), 5.0, |t| {
            assert!(!t.is_cancelled());
            42u32
        });
        assert_eq!(spec.agent, AgentId(7));
        let cancel = CancelToken::new();
        assert_eq!((spec.op)(&cancel), 42);
    }
}
