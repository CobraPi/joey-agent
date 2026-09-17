//! Process-global compute pool for CPU-bound terminal post-processing
//! (feature 033, US1; specs/033-please-reference-plan).
//!
//! Placement rationale: a process-global lazy singleton mirroring the
//! [`terminal_governor`] / `process_registry()` precedent — one process
//! hosts one session's agents, so process scope == session scope. The
//! pool is typed `ComputePool<Vec<u8>>`: terminal post-processing ops
//! move string bytes through the pool (cheap, Send, no tool-internal
//! types cross the boundary). Config is resolved ONCE at first use from
//! `Config::defaults()` + the typed env-aware accessors — env overrides
//! (`ORCHESTRATION_COMPUTE_*`) are honored because the accessors read
//! them at call time; config-file values are picked up per-process from
//! the default config load (same resolution stage as the terminal
//! governor's auto limit).

use joey_compute::{AgentId, ComputePool, JobSpec};
use once_cell::sync::Lazy;
use std::sync::Arc;

static POOL: Lazy<Arc<ComputePool<Vec<u8>>>> = Lazy::new(|| {
    let cfg = joey_core::Config::defaults();
    Arc::new(build_pool(&cfg))
});

/// Build a pool from a resolved config (workers/max_inflight/scale).
pub fn build_pool(cfg: &joey_core::Config) -> ComputePool<Vec<u8>> {
    ComputePool::new(
        cfg.compute_workers(),
        cfg.compute_max_inflight(),
        cfg.compute_scale_ms(),
    )
}

/// The process-global compute pool (mirrors [`terminal_governor`]).
pub fn compute_pool() -> Arc<ComputePool<Vec<u8>>> {
    POOL.clone()
}

/// Convenience: submit a CPU-bound op to the process-global pool.
/// Weight 5.0 — TODO(weight): replace with rank-derived weight.
pub async fn submit_global<F>(op: F) -> Result<Vec<u8>, joey_compute::JobError>
where
    F: FnOnce(&joey_compute::CancelToken) -> Vec<u8> + Send + 'static,
{
    compute_pool()
        .submit(JobSpec::new(AgentId(0), 5.0, op))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_is_configured_and_shared() {
        let p1 = compute_pool();
        let p2 = compute_pool();
        assert!(Arc::ptr_eq(&p1, &p2), "process-global singleton");
        assert!(p1.workers() >= 1);
    }

    #[tokio::test]
    async fn global_submit_round_trips_bytes() {
        let out = submit_global(|_| b"hello".to_vec()).await.unwrap();
        assert_eq!(out, b"hello".to_vec());
    }
}
