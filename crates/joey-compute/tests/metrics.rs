//! T021 — pool metrics (feature 033, US5). Observability only: counters
//! must match the executed workload exactly; timings are tolerance-checked.

use joey_compute::{AgentId, ComputePool, JobSpec, WeightClass};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

#[tokio::test]
async fn counters_match_mixed_workload_exactly() {
    let pool = Arc::new(ComputePool::<u8>::new(1, 8, 2000));

    // Holder occupies the worker so the next job queues behind it.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(release_rx));
    let p1 = pool.clone();
    let j1 = tokio::spawn(async move {
        p1.submit(JobSpec::new(AgentId(1), 5.0, move |_| {
            let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
            1
        }))
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Queued job whose receiver is dropped while it sits in the queue.
    let p2 = pool.clone();
    let j2 = tokio::spawn(async move {
        p2.submit(JobSpec::new(AgentId(2), 5.0, |_| 2)).await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    j2.abort();
    drop(j2);

    release_tx.send(()).unwrap();
    assert_eq!(j1.await.unwrap().unwrap(), 1);

    // A panicking job.
    let boom = pool.submit(JobSpec::new(AgentId(3), 5.0, |_| panic!("boom")));
    assert_eq!(boom.await, Err(joey_compute::JobError::Panicked));

    // Two completing jobs.
    let a = pool.submit(JobSpec::new(AgentId(4), 5.0, |_| 10));
    let b = pool.submit(JobSpec::new(AgentId(5), 5.0, |_| 20));
    assert_eq!(a.await.unwrap(), 10);
    assert_eq!(b.await.unwrap(), 20);

    let snap = pool.metrics_snapshot();
    assert_eq!(snap.submitted, 5);
    assert_eq!(snap.completed, 3); // holder + a + b
    assert_eq!(snap.panicked, 1);
    assert_eq!(snap.cancelled_queued, 1);
    assert_eq!(snap.in_flight, 0);
    // Weight 5.0 → low class; observations exist only for jobs that RAN
    // (holder, boom, a, b) — the cancelled-queued job records nothing.
    assert_eq!(snap.low.queue_wait.count(), 4);
    assert_eq!(snap.low.service_time.count(), 4);
}

#[tokio::test]
async fn single_job_timings_are_sane() {
    let pool = ComputePool::<u8>::new(1, 8, 2000);
    let out = pool
        .submit(JobSpec::new(AgentId(9), 50.0, |_| {
            std::thread::sleep(Duration::from_millis(100));
            7
        }))
        .await
        .unwrap();
    assert_eq!(out, 7);

    let snap = pool.metrics_snapshot();
    assert_eq!(snap.submitted, 1);
    assert_eq!(snap.completed, 1);
    assert_eq!(WeightClass::classify(50.0), WeightClass::Mid);
    let cs = *snap.class(WeightClass::Mid);
    assert_eq!(cs.queue_wait.count(), 1);
    assert_eq!(cs.service_time.count(), 1);
    assert!(
        cs.queue_wait.avg() < Duration::from_millis(100),
        "queue wait ≈ 0 on an idle pool, got {:?}",
        cs.queue_wait.avg()
    );
    assert!(
        cs.service_time.avg() >= Duration::from_millis(100),
        "service time must cover the op duration, got {:?}",
        cs.service_time.avg()
    );
}
