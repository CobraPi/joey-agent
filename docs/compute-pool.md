# Compute Pool

`joey-compute` is the CPU-bound work execution substrate (feature 033,
spec `specs/033-please-reference-plan/`). It exists so that heavy
CPU-bound post-processing (output truncation, re-encoding, formatting)
never blocks the async runtime that drives the agent turn loop.

> **THE HARD RULE:** CPU work submitted to this crate never runs on
> tokio worker threads. Tokio is used solely for the async interface —
> the admission semaphore and the one-shot completion channel. The work
> itself runs on dedicated OS threads named `compute-{i}`.

Consumers today: `joey-tools` routes terminal-tool output post-processing
through a process-global `ComputePool<Vec<u8>>` singleton
(`tools/compute_pool.rs`); `joey-orchestration` assigns scheduling
weights (see §Weight policy below).

## Architecture

Four modules:

- `lib.rs` — pool core: `Entry` (inverted-`Ord` heap node),
  `PoolState`, `worker_loop`, semaphore admission, `catch_unwind`
  isolation, close-drain, `in_flight()` gauge.
- `chunk.rs` — `run_chunked`: drive an `FnMut` op in chunks, checking
  the cancel token between chunks.
- `watchdog.rs` — `Watchdog::new(limit)` / guard; warn-only.
- `metrics.rs` — counters, in-flight gauge, per-weight-class timing
  summaries, `snapshot()`.

Two synchronization domains, deliberately never nested:

1. The **state mutex** guarding `PoolState` (queue + counters). It is
   acquired only for short, non-blocking critical sections — the state
   lock is never held across an op.
2. The **tokio semaphore** bounding queued+running jobs. A permit is
   acquired at submit time (backpressure point) and held until the
   worker finishes the job, so the semaphore count is an honest
   queued+running bound.

Workers are plain `std::thread`s; the async side only ever touches the
pool through the semaphore, the state mutex, and the per-job one-shot
channel.

## Scheduling policy

Each admitted job gets `deadline = now + scale / weight`:

- `scale` is `orchestration.compute.scale_ms` (default 2000 ms).
- `weight` is clamped to `[1.0, 100.0]`; NaN falls back to `1.0`.

The queue is a max-heap of `Entry` with an **inverted `Ord`** so the
greatest entry is the earliest deadline; equal deadlines break FIFO by
a monotonic submission sequence number. Net effect: earliest-deadline-
first with FIFO tiebreak.

Starvation is bounded: because a weight-100 job's deadline is
`now + scale/100` and a weight-1 job's is `now + scale`, every admitted
job waits at most `scale` (plus scheduling tolerance) even under
sustained high-weight pressure — verified by the fairness suite.

## Admission, cancellation, and lifecycle

- **Backpressure:** the tokio semaphore caps queued+running jobs at
  `max_inflight` (default 256). Growth is never unbounded; submitters
  await a permit.
- **Panic isolation:** ops run inside `catch_unwind`; a panicking job
  returns `JobError::Panicked` to its caller and the pool is unaffected.
- **Dropped receiver while queued:** the job is skipped, its permit is
  released, and it is counted as cancelled-while-queued.
- **`close()`:** drains the queue (already-admitted jobs finish), then
  stops workers. Submits after close fail with `PoolClosed`.
- **`in_flight()`:** live gauge of queued+running jobs.

## Configuration

Keys live in `joey-core` config (see
[state-and-config.md](state-and-config.md) §2):

| Key | Default | Env override |
|---|---|---|
| `orchestration.compute.workers` | `"auto"` = max(cores − 1, 1) | `ORCHESTRATION_COMPUTE_WORKERS` |
| `orchestration.compute.max_inflight` | 256 | `ORCHESTRATION_COMPUTE_MAX_INFLIGHT` |
| `orchestration.compute.scale_ms` | 2000 | `ORCHESTRATION_COMPUTE_SCALE_MS` |
| `orchestration.compute.chain_unit_ms` | 1000 | `ORCHESTRATION_COMPUTE_CHAIN_UNIT_MS` |

Precedence: env > config file > auto default. Invalid values fall back
to defaults with a warning — startup never panics.

## Metrics

`metrics.rs` tracks:

- Counters: `submitted`, `completed`, `panicked`, `cancelled_queued`,
  `deadline_overruns`.
- Gauge: `in_flight`.
- Per-weight-class `TimingSummary` (queue-wait, service-time) — low
  (1.0–24.9), mid (25.0–74.9), high (75.0–100.0) — via `snapshot()`.

Observability only: metrics deliberately never feed back into weights.
Scheduling stays deterministic.

## Weight policy (orchestrator side)

`joey-orchestration` computes weights (`chain_est.rs`,
`compute_weight.rs`), and the formula is:

- `remaining_chain_estimate`: iterative Kahn walk over the task graph —
  edge count × `chain_unit_ms`; cycles contribute 0; safe to 10k depth.
- `weight_for_task` = `1 + 99 · (op_est + remaining) / max_chain`.

Weights are orchestrator-assigned only, never taken from LLM or
subagent input — an honesty guardrail.

## Usage guidance

- **Chunking:** for long CPU work, use `run_chunked` and check the
  cancel token between chunks only — never inside hot loops.
- **~100 µs threshold:** ops cheaper than roughly 100 µs are not worth
  a pool round-trip; run them inline.
- **Subprocess babysitting (documented recipe only — not wired):**
  spawn the subprocess, poll `waitpid` checking the cancel token
  between polls, kill on cancel. True hang-killing needs process-level
  supervision and is out of scope for v1.
- **Singleton vs per-crate:** `joey-tools` holds a process-global
  `ComputePool<Vec<u8>>` singleton for terminal post-processing; typed
  per-instance pools are permitted for other consumers.

## Testing map

`crates/joey-compute` (7 suites):

- `tests/ordering.rs` (3) — heap discipline: earliest deadline pops
  first, FIFO tiebreak, inverted `Ord` makes greatest most urgent.
- `tests/basic_pool.rs` (8) — all-jobs-complete, in-flight drains to
  zero, overlap ≤ workers, `max_inflight` gating, close semantics,
  panic isolation, dropped-receiver skip, close-drains-then-rejects.
- `tests/fairness.rs` (1) — low-weight jobs starvation-free while
  high-weight jobs win (median-based).
- `tests/chunk.rs` (3) — cancel mid-run yields partial + incomplete,
  full run completes, pre-cancelled token processes nothing.
- `tests/watchdog.rs` (1) — overrun warns and the job still completes
  (warn-only).
- `tests/metrics.rs` (2) — counters match a mixed workload exactly;
  single-job timings are sane.
- `src/lib.rs` inline unit tests — `CancelToken` set/clone semantics
  and `JobSpec` op invocation.

Elsewhere:

- `crates/joey-tools/tests/compute_pool_e2e.rs` (1) — 50-job burst
  through the real singleton: peak bounded, wall-clock within 2×
  serial.
- `crates/joey-core` config tests (4) — the four
  `orchestration.compute.*` keys: defaults, env overrides, precedence,
  invalid-value fallback.
