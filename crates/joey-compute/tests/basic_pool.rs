//! T004 — pool basics: dedicated workers, deadline pop, in-flight drain
//! (feature 033, specs/033 contracts §2).

use joey_compute::{AgentId, ComputePool, JobSpec};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

async fn eventually<F: FnMut() -> bool>(mut f: F) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

#[tokio::test]
async fn two_workers_four_jobs_all_ok() {
    let pool = ComputePool::<u32>::new(2, 16, 2000);
    let futs: Vec<_> = (0..4u32)
        .map(|i| pool.submit(JobSpec::new(AgentId(1), 5.0, move |_| i * 10)))
        .collect();
    for (i, f) in futs.into_iter().enumerate() {
        assert_eq!(f.await.unwrap(), (i as u32) * 10);
    }
    assert!(eventually(|| pool.in_flight() == 0).await, "in_flight drains to 0");
}

#[tokio::test]
async fn in_flight_returns_to_empty() {
    let pool = ComputePool::<u8>::new(2, 16, 2000);
    let f1 = pool.submit(JobSpec::new(AgentId(1), 5.0, |_| {
        std::thread::sleep(Duration::from_millis(100));
        1
    }));
    let f2 = pool.submit(JobSpec::new(AgentId(1), 5.0, |_| {
        std::thread::sleep(Duration::from_millis(100));
        2
    }));
    let (a, b) = tokio::join!(f1, f2);
    assert_eq!(a.unwrap(), 1);
    assert_eq!(b.unwrap(), 2);
    assert!(eventually(|| pool.in_flight() == 0).await, "in_flight should drain");
}

#[tokio::test]
async fn peak_overlap_never_exceeds_workers() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let pool = Arc::new(ComputePool::<u8>::new(2, 64, 2000));
    let futs: Vec<_> = (0..8)
        .map(|_| {
            let live2 = live.clone();
            let peak2 = peak.clone();
            let p = pool.clone();
            tokio::spawn(async move {
                p.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
                    let n = live2.fetch_add(1, Ordering::SeqCst) + 1;
                    peak2.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(60));
                    live2.fetch_sub(1, Ordering::SeqCst);
                    0
                }))
                .await
            })
        })
        .collect();
    for f in futs {
        f.await.unwrap().unwrap();
    }
    let p = peak.load(Ordering::SeqCst);
    assert!(p <= 2, "peak overlap {} exceeded workers 2", p);
    assert!(p >= 2, "peak overlap {} never reached workers 2 — probe too weak", p);
    assert_eq!(pool.in_flight(), 0);
}

// ── T005: admission backpressure + close semantics ───────────────────

#[tokio::test]
async fn max_inflight_one_gates_second_job_until_first_finishes() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    let started1 = Arc::new(AtomicBool::new(false));
    let started2 = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(release_rx));

    let pool = Arc::new(ComputePool::<u8>::new(1, 1, 2000));

    // Submit futures are lazy: spawn them so they progress concurrently.
    let p1 = pool.clone();
    let s1 = started1.clone();
    let r = rx.clone();
    let j1 = tokio::spawn(async move {
        p1.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
            s1.store(true, Ordering::SeqCst);
            // Hold the single admission permit until released.
            let _ = r.lock().unwrap().recv_timeout(Duration::from_secs(5));
            1
        }))
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(started1.load(Ordering::SeqCst), "job1 should be running");

    let p2 = pool.clone();
    let s2 = started2.clone();
    let j2 = tokio::spawn(async move {
        p2.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
            s2.store(true, Ordering::SeqCst);
            2
        }))
        .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !started2.load(Ordering::SeqCst),
        "job2 must not start while job1 holds the only admission permit"
    );

    release_tx.send(()).unwrap();
    assert_eq!(j1.await.unwrap().unwrap(), 1);
    assert_eq!(j2.await.unwrap().unwrap(), 2);
    assert!(started2.load(Ordering::SeqCst));
}

#[tokio::test]
async fn close_rejects_subsequent_submits() {
    let pool = ComputePool::<u8>::new(1, 16, 2000);
    let j = pool.submit(JobSpec::new(AgentId(1), 5.0, |_| 7));
    assert_eq!(j.await.unwrap(), 7);
    pool.close();
    let late = pool.submit(JobSpec::new(AgentId(1), 5.0, |_| 9));
    assert_eq!(late.await, Err(joey_compute::JobError::PoolClosed));
    let late2 = pool.submit(JobSpec::new(AgentId(2), 5.0, |_| 11));
    assert_eq!(late2.await, Err(joey_compute::JobError::PoolClosed));
}

// ── T014: panic isolation ─────────────────────────────────────────────

#[tokio::test]
async fn panicking_job_is_isolated_and_pool_recovers() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let pool = Arc::new(ComputePool::<u8>::new(2, 16, 2000));

    let boom = pool.submit(JobSpec::new(AgentId(9), 5.0, |_| panic!("job blew up")));
    assert_eq!(boom.await, Err(joey_compute::JobError::Panicked));

    // An immediately-submitted good job still succeeds.
    let good = pool.submit(JobSpec::new(AgentId(9), 5.0, |_| 42));
    assert_eq!(good.await.unwrap(), 42);

    // Full worker concurrency still served after the panic (SC-003).
    let futs: Vec<_> = (0..8)
        .map(|_| {
            let live2 = live.clone();
            let peak2 = peak.clone();
            let p = pool.clone();
            tokio::spawn(async move {
                p.submit(JobSpec::new(AgentId(9), 5.0, move |_| {
                    let n = live2.fetch_add(1, Ordering::SeqCst) + 1;
                    peak2.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    live2.fetch_sub(1, Ordering::SeqCst);
                    0
                }))
                .await
            })
        })
        .collect();
    for f in futs {
        f.await.unwrap().unwrap();
    }
    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "pool should serve full worker concurrency after a panic, peak = {}",
        peak.load(Ordering::SeqCst)
    );
}

// ── T015: queued cancellation (dropped receiver) ─────────────────────

#[tokio::test]
async fn dropped_receiver_skips_queued_job_and_releases_permit() {
    use std::sync::atomic::AtomicBool;

    let ran = Arc::new(AtomicBool::new(false));
    let pool = Arc::new(ComputePool::<u8>::new(1, 1, 2000));

    // Occupy the single permit + the single worker with a holding job.
    let holder_done = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(release_rx));
    let p1 = pool.clone();
    let hd = holder_done.clone();
    let j1 = tokio::spawn(async move {
        p1.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
            let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
            hd.store(true, Ordering::SeqCst);
            1
        }))
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Submit job2, then immediately drop the receiving future — the job is
    // admitted (permit taken) and queued behind the holder, but its receiver
    // is gone before it ever runs.
    let p2 = pool.clone();
    let r2 = ran.clone();
    let j2 = tokio::spawn(async move {
        p2.submit(JobSpec::new(AgentId(2), 5.0, move |_| {
            r2.store(true, Ordering::SeqCst);
            2
        }))
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await; // admitted + queued
    j2.abort(); // drop the receiver while the job sits in the queue
    drop(j2);

    // Release the holder; job2 must be skipped (never runs) and its permit
    // freed, so a subsequent submit is NOT deadlocked.
    release_tx.send(()).unwrap();
    assert_eq!(j1.await.unwrap().unwrap(), 1);
    assert!(holder_done.load(Ordering::SeqCst));
    assert!(
        !ran.load(Ordering::SeqCst),
        "queued job with dropped receiver must never run"
    );

    // Permit hygiene: a fresh submit completes promptly (no deadlock).
    let j3 = pool.submit(JobSpec::new(AgentId(3), 5.0, |_| 3));
    let out = tokio::time::timeout(Duration::from_secs(5), j3)
        .await
        .expect("subsequent submit must not deadlock");
    assert_eq!(out.unwrap(), 3);
}

// ── T016: close-drain semantics ────────────────────────────────────

#[tokio::test]
async fn close_drains_queue_then_rejects_new_submits() {
    let pool = Arc::new(ComputePool::<u8>::new(1, 64, 2000));

    // Enqueue several jobs (single worker ⇒ most sit queued).
    let futs: Vec<_> = (0..4u8)
        .map(|i| {
            let p = pool.clone();
            tokio::spawn(async move {
                p.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
                    std::thread::sleep(Duration::from_millis(50));
                    i
                }))
                .await
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(100)).await; // let them enqueue

    pool.close();

    // Drain semantics: every already-queued job completes.
    for (i, f) in futs.into_iter().enumerate() {
        assert_eq!(f.await.unwrap().unwrap(), i as u8, "queued job {i} must drain");
    }

    // Post-close submits are rejected — never hang.
    let late = pool.submit(JobSpec::new(AgentId(2), 5.0, |_| 99));
    assert_eq!(late.await, Err(joey_compute::JobError::PoolClosed));
}
