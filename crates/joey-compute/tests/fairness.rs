//! T011 v2 — deadline-form weighted fairness (the headline test, feature
//! 033 SC-002). One worker, scale 2 s: a stream of alternating weight-80
//! (agent A) and weight-5 (agent B) jobs. Every B job's queue wait must
//! stay within scale + 500 ms tolerance (bounded starvation), and the A
//! median wait must be under a quarter of the B median (weight
//! proportionality). Tolerance-based assertions only — never exact
//! scheduler timings.

use joey_compute::{AgentId, ComputePool, JobSpec};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SCALE_MS: u64 = 2000;
const TOLERANCE_MS: u64 = 500;
const OP_MS: u64 = 15;
const ROUND_SPACING_MS: u64 = 25;
const ROUNDS: usize = 20;

fn median(xs: &mut Vec<u64>) -> u64 {
    xs.sort_unstable();
    let n = xs.len();
    assert!(n > 0);
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2
    }
}

#[tokio::test]
async fn low_weight_jobs_are_starvation_free_and_high_weight_jobs_win() {
    let pool = Arc::new(ComputePool::<Duration>::new(1, 128, SCALE_MS));
    let mut handles = Vec::new();

    for _round in 0..ROUNDS {
        for (agent, weight) in [(AgentId(1), 80.0f64), (AgentId(2), 5.0f64)] {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                let enqueued = Instant::now();
                let wait = pool
                    .submit(JobSpec::new(agent, weight, move |_| {
                        let waited = enqueued.elapsed();
                        std::thread::sleep(Duration::from_millis(OP_MS));
                        waited
                    }))
                    .await
                    .expect("job should complete");
                (weight, wait.as_millis() as u64)
            }));
        }
        tokio::time::sleep(Duration::from_millis(ROUND_SPACING_MS)).await;
    }

    let mut a_waits: Vec<u64> = Vec::new();
    let mut b_waits: Vec<u64> = Vec::new();
    for h in handles {
        let (w, wait) = h.await.expect("task panicked");
        if w >= 75.0 {
            a_waits.push(wait);
        } else {
            b_waits.push(wait);
        }
    }

    // Bounded starvation: EVERY low-weight job waited ≤ scale + tolerance.
    let bound = SCALE_MS + TOLERANCE_MS;
    for (i, w) in b_waits.iter().enumerate() {
        assert!(
            *w <= bound,
            "B job {} waited {} ms > bound {} ms — starvation",
            i,
            w,
            bound
        );
    }
    // Weight proportionality: A median < B median / 4.
    let a_med = median(&mut a_waits);
    let b_med = median(&mut b_waits);
    assert!(
        a_med * 4 < b_med,
        "A median {} ms not < B median {} ms / 4",
        a_med,
        b_med
    );
}
