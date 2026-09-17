//! T009 — 50-burst peak-concurrency bound (feature 033, US1; SC-001).
//!
//! Simulates the terminal post-processing workload (oversized command
//! outputs run through ANSI-strip on the compute pool) as a 50-command
//! burst against a TEST-CONSTRUCTED pool (not the process singleton) and
//! asserts: (a) peak overlapping post-processing jobs never exceed the
//! worker count; (b) burst wall time stays within 2x the serial baseline
//! (pool adds overhead, never catastrophic slowdown).

use joey_compute::{AgentId, ComputePool, JobSpec};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Build an oversized fake command output (~256 KB) dense with ANSI
/// escapes — the shape terminal post-processing actually chews on.
fn oversized_output() -> String {
    let chunk = "\u{1b}[32mOK\u{1b}[0m some line of command output\n".repeat(100);
    chunk.repeat(80) // ~2.7 MB worth of escapes+text
}

#[tokio::test]
async fn fifty_burst_peak_bounded_and_wall_within_2x_serial() {
    const WORKERS: usize = 2;
    const BURST: usize = 50;

    let payload = oversized_output();
    let pool = Arc::new(ComputePool::<usize>::new(WORKERS, 256, 2000));
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));

    // Serial baseline: one op alone (enqueue → run → complete).
    let p0 = pool.clone();
    let pay0 = payload.clone();
    let t0 = Instant::now();
    p0.submit(JobSpec::new(AgentId(0), 5.0, move |_| {
        joey_tools::guards::strip_ansi(&pay0).len()
    }))
    .await
    .expect("baseline op");
    let per_op = t0.elapsed();
    let serial_baseline = per_op * BURST as u32;

    // The 50-command burst: every op wraps the same oversized payload.
    let start = Instant::now();
    let futs: Vec<_> = (0..BURST)
        .map(|_| {
            let p = pool.clone();
            let live2 = live.clone();
            let peak2 = peak.clone();
            let pay = payload.clone();
            tokio::spawn(async move {
                p.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
                    let n = live2.fetch_add(1, Ordering::SeqCst) + 1;
                    peak2.fetch_max(n, Ordering::SeqCst);
                    let stripped = joey_tools::guards::strip_ansi(&pay).len();
                    live2.fetch_sub(1, Ordering::SeqCst);
                    stripped
                }))
                .await
            })
        })
        .collect();
    let mut total = 0usize;
    for f in futs {
        total += f.await.expect("spawn").expect("job");
    }
    let burst_wall = start.elapsed();

    assert!(total > 0, "work actually happened");
    let p = peak.load(Ordering::SeqCst);
    assert!(
        p <= WORKERS,
        "peak overlapping post-processing jobs {} exceeded workers {}",
        p,
        WORKERS
    );
    assert!(
        burst_wall <= serial_baseline * 2,
        "burst wall {:?} exceeded 2x serial baseline {:?}",
        burst_wall,
        serial_baseline
    );
}
