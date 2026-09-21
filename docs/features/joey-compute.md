# joey-compute — CPU-bound work execution substrate

`joey-compute` is the workspace's dedicated pool for CPU-bound work
(feature 033, spec `specs/033-please-reference-plan/`). It exists so heavy
post-processing — terminal-output truncation, re-encoding, formatting —
never runs on (or starves) the tokio async runtime that drives the agent
turn loop. It has no workspace dependencies at all: `std` threads, one
tokio semaphore, and nothing else.

> See also: [../compute-pool.md](../compute-pool.md) — the full design
> page (scheduling math, fairness proofs, config, testing).

## The hard rule

CPU work submitted to this crate **never runs on tokio worker threads**.
Tokio is used solely for the async interface — the admission semaphore and
the per-job one-shot completion channel. The work itself runs on dedicated
OS threads named `compute-{i}`.

## Consumers

- `joey-tools` — routes terminal-tool output post-processing through a
  process-global `ComputePool<Vec<u8>>` singleton
  (`tools/compute_pool.rs`); a burst of 50 parallel terminal commands with
  large outputs can no longer oversubscribe the machine or freeze the
  agent.
- `joey-orchestration` — assigns scheduling weights to pool jobs (see
  §Weight policy below); weights are orchestrator-assigned only, never
  taken from LLM or subagent input.

## Public API (src/lib.rs, chunk.rs, watchdog.rs, metrics.rs)

| Item | Kind | Purpose |
|---|---|---|
| `AgentId(u64)` | type | submitter identity for metrics/attribution |
| `JobError` | enum | `Panicked`, `Cancelled`, `PoolClosed`, … |
| `CancelToken` | struct | `Arc<AtomicBool>`-backed cooperative cancel flag |
| `JobSpec<T>` | struct | `{ agent, weight, op }` — one unit of CPU work |
| `clamp_weight(w)` | fn | clamp to `[1.0, 100.0]`; NaN → `1.0` |
| `ComputePool<T>` | struct | `new(workers, max_inflight, scale_ms)`, `submit`, `close`, `in_flight`, `metrics_snapshot` |
| `run_chunked` (chunk.rs) | fn | drive an `FnMut` op in chunks, checking the cancel token between chunks |
| `Watchdog` (watchdog.rs) | struct | `new(limit)` + guard; warn-only |
| `Metrics` / `MetricsSnapshot` (metrics.rs) | struct | counters, in-flight gauge, per-weight-class timing summaries |

## Scheduling policy

Each admitted job gets `deadline = now + scale / weight` (`scale` =
`orchestration.compute.scale_ms`, default 2000 ms). The queue is a
max-heap of `Entry` with an **inverted `Ord`** so the greatest entry is
the earliest deadline; equal deadlines break FIFO by a monotonic
submission sequence. Net effect: **earliest-deadline-first with FIFO
tiebreak** and bounded starvation — every admitted job waits at most
`scale` (plus tolerance) even under sustained weight-100 pressure,
verified by the fairness suite.

Two synchronization domains, deliberately never nested:

1. the **state mutex** (queue + counters) — short critical sections only,
   never held across an op;
2. the **tokio semaphore** bounding queued+running jobs — a permit is
   acquired at submit (the backpressure point) and held until the worker
   finishes, so the count is an honest queued+running bound.

## Lifecycle semantics

- **Admission/backpressure:** semaphore caps queued+running at
  `max_inflight` (default 256); submitters await a permit.
- **Panic isolation:** ops run inside `catch_unwind`; a panicking job
  returns `JobError::Panicked` and the pool is unaffected.
- **Dropped receiver while queued:** job skipped, permit released,
  counted as cancelled-while-queued.
- **`close()`:** drains already-admitted jobs, then stops workers;
  post-close submits fail with `PoolClosed`.
- **Cancellation:** cooperative via `CancelToken`, checked between chunks
  by `run_chunked` — never inside hot loops.

## Configuration

| Key | Default | Env override |
|---|---|---|
| `orchestration.compute.workers` | `"auto"` = max(cores − 1, 1) | `ORCHESTRATION_COMPUTE_WORKERS` |
| `orchestration.compute.max_inflight` | 256 | `ORCHESTRATION_COMPUTE_MAX_INFLIGHT` |
| `orchestration.compute.scale_ms` | 2000 | `ORCHESTRATION_COMPUTE_SCALE_MS` |
| `orchestration.compute.chain_unit_ms` | 1000 | `ORCHESTRATION_COMPUTE_CHAIN_UNIT_MS` |

Precedence: env > config file > auto default; invalid values fall back
with a warning — startup never panics. Single-core machines floor at 1
worker.

## Weight policy (orchestrator side)

`joey-orchestration` (`chain_est.rs`, `compute_weight.rs`):

- `remaining_chain_estimate` — iterative Kahn walk over the task graph;
  edge count × `chain_unit_ms`; cycles contribute 0; safe to 10k depth.
- `weight_for_task = 1 + 99 · (op_est + remaining) / max_chain`.

## Testing map

`tests/basic_pool.rs`, `chunk.rs`, `fairness.rs` (starvation bound),
`metrics.rs`, `ordering.rs` (EDF/FIFO), `watchdog.rs`; weight math in
`joey-orchestration/tests/chain_est.rs`.
