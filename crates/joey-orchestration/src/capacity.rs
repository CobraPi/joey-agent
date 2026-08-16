//! System-capacity detection for parallel subagent spawning.
//!
//! The goal (parallel-subagent feature): use everything the host can
//! reasonably offer — parallelize inference across as many subagents as the
//! system supports — without melting the machine. The limiter that actually
//! matters for subagents is NOT CPU count (children are network-bound on the
//! provider API almost all of the time) but:
//!
//! 1. **Provider-side concurrency** — in-flight API requests (`Semaphore`
//!    permits shared across parent + children, FR-018).
//! 2. **Memory** — each child is a full `Agent` (history + registry clone +
//!    reqwest client reuse) and each terminal tool can spawn subprocesses.
//!    We reserve a healthy per-child headroom and never let the projected
//!    footprint exceed a fraction of physical RAM.
//! 3. **Tokio worker threads** — the async runtime size, because every
//!    child's event stream + any subprocess reaping competes for workers.
//!
//! Everything is cheap to compute (one `/proc` or `sysctl` read cached for
//! the process lifetime) and overridable via config:
//!
//! - `delegation.max_concurrent_children` — hard cap (set to `auto` or 0 to
//!   use the detected capacity).
//! - `delegation.auto_mem_reserve_mb_per_child` — memory headroom per child
//!   (default 256 MB).
//! - `delegation.auto_mem_max_fraction` — fraction of physical RAM the
//!   projected footprint may reach (default 0.6).

use std::sync::OnceLock;

/// Detected host capacity, cached for the process lifetime.
#[derive(Debug, Clone, Copy)]
pub struct SystemCapacity {
    /// Logical CPU count (0 = unknown).
    pub cpus: usize,
    /// Physical RAM in MB (0 = unknown).
    pub total_mem_mb: u64,
    /// Available RAM in MB right now (0 = unknown).
    pub available_mem_mb: u64,
}

impl SystemCapacity {
    /// Detect the host's capacity. Never panics; unknown values read as 0
    /// and the sizing formula degrades to conservative defaults.
    pub fn detect() -> Self {
        static CACHE: OnceLock<SystemCapacity> = OnceLock::new();
        *CACHE.get_or_init(detect_uncached)
    }
}

fn detect_uncached() -> SystemCapacity {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let (total_mem_mb, available_mem_mb) = read_mem_mb();
    SystemCapacity {
        cpus,
        total_mem_mb,
        available_mem_mb,
    }
}

/// Read total + available RAM (MB) via sysinfo (already a workspace dep of
/// joey-tools; added here for the same purpose). Returns (0, 0) on failure.
fn read_mem_mb() -> (u64, u64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory() / (1024 * 1024);
    let available = sys.available_memory() / (1024 * 1024);
    (total, available)
}

/// Default per-child memory headroom (MB).
pub const DEFAULT_MEM_RESERVE_MB_PER_CHILD: u64 = 256;
/// Default fraction of physical RAM the projected footprint may reach.
pub const DEFAULT_MEM_MAX_FRACTION: f64 = 0.6;
/// Hard ceiling on children regardless of what detection says — beyond this
/// the provider queueing + context-switch overhead dominates and wall-clock
/// gains invert. Also protects against absurd hosts (e.g. 512-core boxes).
pub const HARD_CHILD_CEILING: usize = 32;
/// Floor when nothing else is known.
pub const FLOOR_CHILDREN: usize = 4;

/// Compute the capacity-driven parallel-children limit.
///
/// Formula (each term is a limiter; the minimum wins):
/// - `mem_limit`: (available_mem × fraction) / reserve_per_child — how many
///   children fit in the memory we're willing to spend.
/// - `cpu_limit`: cpus (children are network-bound; one slot per CPU is a
///   sane ceiling, not a tight one — the provider request semaphore is the
///   real throttle).
///
/// Everything clamps into `[FLOOR_CHILDREN, HARD_CHILD_CEILING]`.
pub fn capacity_children(
    cap: &SystemCapacity,
    mem_reserve_mb_per_child: u64,
    mem_max_fraction: f64,
) -> usize {
    let mem_limit = if cap.available_mem_mb > 0 && mem_reserve_mb_per_child > 0 {
        let budget = (cap.available_mem_mb as f64 * mem_max_fraction) as u64;
        (budget / mem_reserve_mb_per_child).max(1) as usize
    } else {
        0
    };
    let cpu_limit = if cap.cpus > 0 { cap.cpus } else { 0 };

    let mut limits: Vec<usize> = Vec::with_capacity(2);
    if mem_limit > 0 {
        limits.push(mem_limit);
    }
    if cpu_limit > 0 {
        limits.push(cpu_limit);
    }
    let chosen = limits.into_iter().min().unwrap_or(FLOOR_CHILDREN);
    chosen.clamp(FLOOR_CHILDREN, HARD_CHILD_CEILING)
}

/// Capacity-driven semaphore permits for in-flight provider requests.
///
/// Children are network-bound; the request semaphore should be at least the
/// children limit (one request in flight per child) plus a couple for the
/// parent/turn loop. Capped at a sane maximum to avoid smashing provider
/// rate limits.
pub fn capacity_requests(children: usize) -> usize {
    (children + 2).min(64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(cpus: usize, total_mb: u64, avail_mb: u64) -> SystemCapacity {
        SystemCapacity {
            cpus,
            total_mem_mb: total_mb,
            available_mem_mb: avail_mb,
        }
    }

    #[test]
    fn memory_limited_host() {
        // 8 GB total, 2 GB available, 256 MB/child, 60% → ~4 children.
        let c = capacity_children(
            &cap(16, 8192, 2048),
            DEFAULT_MEM_RESERVE_MB_PER_CHILD,
            DEFAULT_MEM_MAX_FRACTION,
        );
        assert_eq!(c, 4, "2 GB × 0.6 / 256 MB = 4");
    }

    #[test]
    fn cpu_limited_host() {
        // Plenty of RAM, 4 CPUs → 4 children.
        let c = capacity_children(
            &cap(4, 65_536, 32_768),
            DEFAULT_MEM_RESERVE_MB_PER_CHILD,
            DEFAULT_MEM_MAX_FRACTION,
        );
        assert_eq!(c, 4);
    }

    #[test]
    fn floor_applies_when_nothing_known() {
        let c = capacity_children(&cap(0, 0, 0), 256, 0.6);
        assert_eq!(c, FLOOR_CHILDREN);
    }

    #[test]
    fn hard_ceiling_applies() {
        let c = capacity_children(
            &cap(512, 1_048_576, 1_048_576),
            DEFAULT_MEM_RESERVE_MB_PER_CHILD,
            DEFAULT_MEM_MAX_FRACTION,
        );
        assert_eq!(c, HARD_CHILD_CEILING);
    }

    #[test]
    fn requests_at_least_children_plus_two() {
        assert_eq!(capacity_requests(8), 10);
        assert_eq!(capacity_requests(100), 64, "capped at 64");
    }

    #[test]
    fn detect_returns_something() {
        let c = SystemCapacity::detect();
        // On any real test host we expect CPUs; memory may read 0 only if
        // sysinfo fails catastrophically — don't assert on it.
        assert!(c.cpus >= 1, "available_parallelism should work");
    }
}
