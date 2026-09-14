//! Feature 030: append-only JSONL resource records plus the sampled
//! CPU/memory watchdog for subagent delegation governance.

use crate::types::{Priority, ResourceRecord, ResourceRecordOutcome, ResumeToken};
use std::io::Write;

/// Append-only JSONL store for resource records
/// (feature 030, contracts/resource-record.md).
#[derive(Debug, Clone)]
pub struct ResourceRecordStore {
    path: std::path::PathBuf,
}

impl ResourceRecordStore {
    /// Opens (lazily creates) the default store at
    /// `<home>/delegation/resource-records.jsonl`, where `<home>` is
    /// `data_dir` when provided or `joey_core::joey_home()` otherwise
    /// (mirrors the joey-cron JobStore `open_default` precedent, jobs.rs:723).
    pub fn open(data_dir: Option<&std::path::Path>) -> Self {
        let base = match data_dir {
            Some(dir) => dir.to_path_buf(),
            None => joey_core::joey_home(),
        };
        Self {
            path: base
                .join("delegation")
                .join("resource-records.jsonl"),
        }
    }

    /// The JSONL file path this store appends to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Appends one JSON object per line. Creates parent dirs. Errors are
    /// returned (the manager logs-and-continues; recording must never fail a task).
    pub fn append(&self, record: &ResourceRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        // SC-008: user-only permissions — idempotent chmod on every append
        // (cheap) so a file created by an older version is also tightened.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    /// Tolerant load: one record per non-empty line; a malformed line is
    /// SKIPPED, not fatal (restart tolerance). A malformed TRAILING line
    /// (torn write) is therefore skipped naturally.
    pub fn load(&self) -> Vec<ResourceRecord> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Count without materializing records.
    pub fn count(&self) -> usize {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(_) => return 0,
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<ResourceRecord>(l).is_ok())
            .count()
    }
}

/// Convenience constructor filling record_id (uuid v4 hyphenated),
/// created_at (RFC 3339 seconds precision — the evidence.rs:134 precedent),
/// from the given payload fields.
#[allow(clippy::too_many_arguments)]
pub fn new_record(
    task_signature: String,
    priority: Priority,
    outcome: ResourceRecordOutcome,
    queue_wait_ms: u64,
    compute_ms: u64,
    cpu_ms: u64,
    memory_peak_kb: u64,
    parent_starved_ms: u64,
    retries: u16,
    checkpoint: Option<ResumeToken>,
    token_usage: joey_providers::Usage,
    degraded: bool,
) -> ResourceRecord {
    ResourceRecord {
        record_id: uuid::Uuid::new_v4().hyphenated().to_string(),
        task_signature,
        priority,
        outcome,
        queue_wait_ms,
        compute_ms,
        cpu_ms,
        memory_peak_kb,
        parent_starved_ms,
        retries,
        checkpoint,
        token_usage,
        degraded,
        created_at: chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

// ---------------------------------------------------------------------------
// T016 (US3, research.md R5b/R6): the sampling CPU watchdog.
// ---------------------------------------------------------------------------

/// Shared per-child resource accounting (feature 030, research.md R5b/R6).
/// Process-level CPU is sampled every watchdog tick and apportioned evenly
/// to the children running during that interval; peak RSS is advisory.
/// All fields are labeled sampled in resource records.
///
/// Windows note (R5b): sysinfo covers Windows; validation on Windows is
/// pending (research.md R12, carried to tasks).
pub struct WatchdogState {
    /// child id -> cumulative attributed CPU milliseconds.
    pub cpu_ms: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
    /// child id -> peak attributed RSS in KB (advisory).
    pub memory_peak_kb: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
    /// child ids executing at the last tick.
    pub running: std::sync::Mutex<std::collections::HashSet<u64>>,
    /// child id -> ceiling exceeded marker (set once, read by the manager
    /// after the run to override the outcome text).
    pub cpu_exceeded: std::sync::Mutex<std::collections::HashSet<u64>>,
}

impl WatchdogState {
    pub fn new() -> Self {
        Self {
            cpu_ms: std::sync::Mutex::new(std::collections::HashMap::new()),
            memory_peak_kb: std::sync::Mutex::new(std::collections::HashMap::new()),
            running: std::sync::Mutex::new(std::collections::HashSet::new()),
            cpu_exceeded: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub fn snapshot_cpu_ms(&self, id: u64) -> u64 {
        self.cpu_ms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .copied()
            .unwrap_or(0)
    }

    pub fn snapshot_memory_peak_kb(&self, id: u64) -> u64 {
        self.memory_peak_kb
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .copied()
            .unwrap_or(0)
    }

    pub fn mark_running(&self, id: u64) {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id);
    }

    pub fn mark_finished(&self, id: u64) {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
    }

    pub fn is_cpu_exceeded(&self, id: u64) -> bool {
        self.cpu_exceeded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&id)
    }
}

/// Testable per-tick sampling math (one tick-equivalent): apportion
/// `delta_cpu_ms` and `rss_kb` evenly among `running`, updating cumulative
/// CPU and the advisory peak-RSS maps. Skip when none running.
fn apportion(state: &WatchdogState, delta_cpu_ms: u64, rss_kb: u64, running: &[u64]) {
    if running.is_empty() {
        return;
    }
    let n = running.len() as u64;
    let per_cpu = delta_cpu_ms / n;
    let per_rss = rss_kb / n;
    let mut cpu = state.cpu_ms.lock().unwrap_or_else(|p| p.into_inner());
    for id in running {
        *cpu.entry(*id).or_insert(0) += per_cpu;
    }
    drop(cpu);
    let mut peak = state
        .memory_peak_kb
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for id in running {
        let e = peak.entry(*id).or_insert(0);
        *e = (*e).max(per_rss);
    }
}

/// Spawn the sampling watchdog loop (FR-010/R5b). Returns nothing; the task
/// runs until the shutdown flag flips. Each tick (interval_secs, clamped
/// >= 1):
///   1. minimal sysinfo refresh: refresh_process(current pid) +
///      refresh_memory (see capacity.rs for the sysinfo import style)
///   2. process cpu_usage() (percent of one core since last refresh)
///      -> delta_cpu_ms = (usage/100.0 * interval_ms)
///   3. apportion delta evenly among `running` ids (skip when none running)
///   4. RSS: sysinfo process memory() bytes -> kb; each running id's
///      peak = max(peak, rss_kb / running.len())  [advisory]
///   5. for each running id with cpu_ceiling_secs > 0 and cumulative
///      cpu_ms >= ceiling*1000: insert into cpu_exceeded AND trigger abort
///      (caller-provided abort fn — the manager passes a closure that sets
///      that child's interrupt flag + pending_stop, mirroring stop_child's
///      writes via the registry)
///
/// Implementation notes: the sysinfo System instance is kept alive across
/// ticks (created once before the loop — cpu_usage() is since-last-refresh
/// so the first tick reads ~0; acceptable sampling warmup). Interval 0 →
/// clamped to 1 inside. Ceiling 0 → step 5 skipped (disabled).
/// getrusage-free; NO cgroups.
pub fn spawn_watchdog(
    state: std::sync::Arc<WatchdogState>,
    interval_secs: u64,
    cpu_ceiling_secs: u64,
    memory_tracking: bool,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    abort: impl Fn(u64) + Send + 'static,
) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let interval = interval_secs.max(1);
    let fut = async move {
        // One System instance alive across ticks: cpu_usage() is measured
        // since the last refresh, so the first tick reads ~0 (sampling
        // warmup — acceptable).
        let mut sys = System::new();
        let pid = match sysinfo::get_current_pid() {
            Ok(pid) => pid,
            Err(_) => return, // no sampling possible on this platform
        };
        // Prime the cpu_usage() baseline: usage is measured since the
        // last refresh, so refresh once here (same call the tick uses,
        // apportioning nothing) so tick 1 measures a real interval
        // instead of reading ~0 and missing the startup window.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::everything(),
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            // Snapshot the running set FIRST (cheap, synchronous) so the
            // apportionment below acts on a stable view.
            let running: Vec<u64> = {
                let r = state.running.lock().unwrap_or_else(|p| p.into_inner());
                r.iter().copied().collect()
            };
            // Minimal refresh: current process (CPU + memory) and system
            // memory (the capacity.rs sysinfo import style).
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                false,
                ProcessRefreshKind::everything(),
            );
            sys.refresh_memory();
            if running.is_empty() {
                continue; // nothing to apportion — cheap tick
            }
            let interval_ms = interval as f64 * 1000.0;
            let (delta_cpu_ms, rss_kb) = match sys.process(pid) {
                Some(proc) => {
                    let delta_cpu_ms = (proc.cpu_usage() as f64 / 100.0 * interval_ms) as u64;
                    let rss_kb = if memory_tracking {
                        proc.memory() / 1024
                    } else {
                        0
                    };
                    (delta_cpu_ms, rss_kb)
                }
                None => (0, 0),
            };
            apportion(&state, delta_cpu_ms, rss_kb, &running);
            // Step 5: hard ceiling check + abort (ceiling 0 = disabled).
            if cpu_ceiling_secs > 0 {
                let ceiling_ms = cpu_ceiling_secs.saturating_mul(1000);
                let to_abort: Vec<u64> = {
                    let cpu = state.cpu_ms.lock().unwrap_or_else(|p| p.into_inner());
                    running
                        .iter()
                        .copied()
                        .filter(|id| cpu.get(id).copied().unwrap_or(0) >= ceiling_ms)
                        .collect()
                };
                for id in &to_abort {
                    state
                        .cpu_exceeded
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(*id);
                }
                for id in to_abort {
                    abort(id);
                }
            }
        }
    };
    // Reactor-safe spawn: prefer the ambient runtime; when constructed
    // outside any Tokio context (e.g. joey-cli engine tests building
    // managers synchronously), drive the sampling loop on a detached
    // std thread with its own current-thread runtime so watchdog
    // enforcement never depends on the caller's async context.
    // Fixes the T034 regression where joey-cli actor_tests hit the bare
    // tokio::spawn with no reactor running (governance now inherited).
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(fut);
        }
        Err(_) => {
            let _ = std::thread::Builder::new()
                .name("joey-gov-watchdog".to_string())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("watchdog runtime");
                    rt.block_on(fut);
                });
        }
    }
}

#[cfg(test)]
mod resource_record_store_tests {
    use super::*;
    use crate::types::ResourceRecordOutcome::*;
    use std::io::Write as _;

    fn usage() -> joey_providers::Usage {
        joey_providers::Usage {
            prompt_tokens: 11,
            completion_tokens: 22,
            total_tokens: 33,
            cache_read_tokens: 4,
            cache_write_tokens: 5,
            reasoning_tokens: 6,
        }
    }

    fn token(last_completed_turn: usize) -> Option<ResumeToken> {
        Some(ResumeToken {
            last_completed_turn,
            transcript_digest: format!("digest-{last_completed_turn}"),
            recorded_at: "2026-09-12T00:00:00+00:00".to_string(),
        })
    }

    #[test]
    fn append_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResourceRecordStore::open(Some(dir.path()));
        let records = vec![
            new_record("sig-a".into(), Priority::Critical, Completed, 1, 2, 3, 4, 5, 0, token(3), usage(), false),
            new_record("sig-b".into(), Priority::Normal, Failed, 10, 20, 30, 40, 50, 1, None, usage(), true),
            new_record("sig-c".into(), Priority::Background, Timeout, 100, 200, 300, 400, 500, 2, token(7), usage(), false),
        ];
        for r in &records {
            store.append(r).unwrap();
        }
        let loaded = store.load();
        assert_eq!(loaded.len(), 3);
        assert_eq!(store.count(), 3);
        for (l, r) in loaded.iter().zip(&records) {
            assert_eq!(l, r); // PartialEq
        }
        let content = std::fs::read_to_string(store.path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            for key in [
                "record_id", "task_signature", "priority", "outcome",
                "queue_wait_ms", "compute_ms", "cpu_ms", "memory_peak_kb",
                "parent_starved_ms", "retries", "checkpoint", "token_usage",
                "degraded", "created_at",
            ] {
                assert!(v.get(key).is_some(), "missing key {key}");
            }
        }
    }

    #[test]
    fn tolerant_load_skips_malformed_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResourceRecordStore::open(Some(dir.path()));
        let a = new_record("sig-1".into(), Priority::Normal, Completed, 1, 1, 1, 1, 1, 0, None, usage(), false);
        let b = new_record("sig-2".into(), Priority::Normal, Failed, 2, 2, 2, 2, 2, 0, None, usage(), false);
        store.append(&a).unwrap();
        store.append(&b).unwrap();
        // Full rewrite for determinism: valid + valid + torn trailing line.
        let l1 = serde_json::to_string(&a).unwrap();
        let l2 = serde_json::to_string(&b).unwrap();
        std::fs::write(store.path(), format!("{l1}\n{l2}\n{{torn json")).unwrap();
        assert_eq!(store.load().len(), 2);
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn append_creates_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push("level1");
        path.push("level2");
        let store = ResourceRecordStore { path: path.join("resource-records.jsonl") };
        let r = new_record("sig-dirs".into(), Priority::Normal, Completed, 0, 0, 0, 0, 0, 0, None, usage(), false);
        store.append(&r).unwrap();
        assert_eq!(store.load().len(), 1);
    }

    #[test]
    fn one_record_per_terminal_outcome_all_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResourceRecordStore::open(Some(dir.path()));
        let all = [BusyRefused, CacheHit, AbortedByResourceLimit, Timeout, Failed, Completed];
        for (i, outcome) in all.iter().enumerate() {
            let r = new_record(format!("sig-{i}"), Priority::Normal, *outcome, 0, 0, 0, 0, 0, 0, None, usage(), false);
            store.append(&r).unwrap();
        }
        let loaded = store.load();
        assert_eq!(loaded.len(), 6);
        let outcomes: Vec<ResourceRecordOutcome> = loaded.iter().map(|r| r.outcome).collect();
        assert_eq!(outcomes, all.to_vec());
    }

    #[test]
    fn new_record_fills_ids_and_timestamp() {
        let r = new_record("sig-x".into(), Priority::Normal, Completed, 1, 2, 3, 4, 5, 6, token(9), usage(), true);
        assert!(!r.record_id.is_empty());
        assert!(r.record_id.contains('-'));
        assert!(!r.created_at.is_empty());
        assert!(r.created_at.starts_with(|c: char| c.is_ascii_digit() && c != '0'));
        assert_eq!(r.token_usage, usage());
        let round: ResourceRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(round.token_usage, usage());
        assert_eq!(round.checkpoint, token(9));
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    #[test]
    fn apportion_evenly_two_children() {
        let state = WatchdogState::new();
        let running = [7u64, 9u64];
        // Two tick-equivalents of 1000ms delta over 2 ids → each 1000ms.
        apportion(&state, 1000, 0, &running);
        apportion(&state, 1000, 0, &running);
        assert_eq!(state.snapshot_cpu_ms(7), 1000);
        assert_eq!(state.snapshot_cpu_ms(9), 1000);
        // Peak rss recorded (advisory): 4096kb / 2 = 2048 per child.
        apportion(&state, 0, 4096, &running);
        assert_eq!(state.snapshot_memory_peak_kb(7), 2048);
        assert_eq!(state.snapshot_memory_peak_kb(9), 2048);
        // Peak is a max: a smaller sample never lowers it.
        apportion(&state, 0, 1024, &running);
        assert_eq!(state.snapshot_memory_peak_kb(7), 2048);
    }

    #[test]
    fn apportion_skips_when_none_running() {
        let state = WatchdogState::new();
        apportion(&state, 1000, 4096, &[]);
        assert_eq!(state.snapshot_cpu_ms(1), 0);
        assert_eq!(state.snapshot_memory_peak_kb(1), 0);
    }

    #[test]
    fn ceiling_marks_exceeded() {
        let state = WatchdogState::new();
        state.mark_running(3);
        // Apportion 2500ms to one id with ceiling 2s: cumulative 2500 >=
        // 2000 → the same helper boundary the watchdog loop uses.
        apportion(&state, 2500, 0, &[3]);
        assert!(state.snapshot_cpu_ms(3) >= 2_000);
        // The exceeded marker is the loop's step 5; simulate the insert
        // exactly as the loop does and read it back.
        state
            .cpu_exceeded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(3);
        assert!(state.is_cpu_exceeded(3));
        assert!(!state.is_cpu_exceeded(4));
        // mark_finished removes from running.
        state.mark_finished(3);
        assert!(!state
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&3));
    }
}
