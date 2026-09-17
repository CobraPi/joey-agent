# Public API Contract: joey-compute and integration surfaces

**Feature**: specs/033-please-reference-plan | **Date**: 2026-09-16 | **Spec**: [spec.md](../spec.md)

Status: NEW public surface. Under constitution Principle VII this surface is a stable contract from the moment it lands; any later breaking change requires a MAJOR bump, a documented migration, and regression coverage.

## 1. Crate charter
`joey-compute` is the CPU-bound work execution substrate. It depends only on tokio (workspace) and std. Hard rule (stated in the module docs, enforced by tests): CPU work never runs on tokio workers; tokio is used solely for the async interface (completion channel, admission semaphore).

## 2. Core types and semantics
- `AgentId` — a plain numeric identifier for the submitting agent; used for accounting only, never exclusivity.
- `JobError` — exactly these variants: `Cancelled`, `Panicked`, `PoolClosed`. Distinct and exhaustive; callers match on precisely these.
- `CancelToken` — `is_cancelled()` query; idempotent set; cooperative only (never forces termination).
- `JobSpec<T>` — carries the agent identifier, the weight, and the operation (which receives the cancel token and returns the value).
- `ComputePool<T>`:
  - `submit(spec)` (async) — acquires an admission permit (waiting when saturated: backpressure, never unbounded growth); clamps the weight to [1.0, 100.0] with NaN mapped to 1.0; computes `deadline = now + scale / weight`; pushes onto the ready heap; resolves via a one-shot completion channel. If the receiver is dropped while the job is queued, the job never runs (permit released, counted cancelled). After close: returns `PoolClosed` — never hangs.
  - `close()` — drain semantics: queued jobs run to completion, then workers stop; idempotent.
  - `in_flight()` — queued plus running at observation time.
- Ready-heap ordering: earliest deadline first; FIFO among equal deadlines. Internal entry ordering is not part of the public contract.
- `run_chunked(items, cancel_token, chunk_size, f)` — checks cancellation between chunks only; returns partial results plus a completed-all flag.
- `Watchdog { limit }` — `guard(f)`: emits a tracing warning when a job exceeds its limit; the limit is caller-configured per job and defaults to 30 seconds (independent of scale_ms); never kills threads; hang-killing is explicitly out of contract (documented limitation).
- Metrics — `snapshot()` returns counters (submitted, completed, panicked, cancelled-while-queued, deadline overruns, in-flight) plus per-weight-class queue-wait and service-time summaries (weight classes: low 1.0–24.9, mid 25.0–74.9, high 75.0–100.0). Observability only — no scheduling feedback.

## 3. Configuration contract (new, additive)

| Key | Type | Default | Environment override |
|-----|------|---------|----------------------|
| `orchestration.compute.workers` | auto or integer >= 1 | auto = max(cores - 1, 1) | `ORCHESTRATION_COMPUTE_WORKERS` |
| `orchestration.compute.max_inflight` | integer >= 1 | 256 | `ORCHESTRATION_COMPUTE_MAX_INFLIGHT` |
| `orchestration.compute.scale_ms` | integer > 0 | 2000 | `ORCHESTRATION_COMPUTE_SCALE_MS` |
| `orchestration.compute.chain_unit_ms` | integer > 0 | 1000 | `ORCHESTRATION_COMPUTE_CHAIN_UNIT_MS` |

Precedence: environment variable > config file > auto default. A typed accessor resolves `auto`. Invalid values fall back to defaults with a warning — startup never panics.

## 4. Weight-policy contract (orchestrator side)
- `remaining_chain_estimate(graph, id)` — edge count times the chain-unit constant; iterative walk (no recursion-depth limit); cycles treated as zero (graphs are pre-validated upstream anyway).
- `weight_for_task(graph, id, op_est)` — `1.0 + 99.0 * (op_est + remaining_chain) / max_chain`, clamped to [1, 100]. `max_chain` — the largest remaining-chain value across currently known tasks in the graph, floored at one chain-unit constant so a single-task graph divides by one full unit. Weights are orchestrator-assigned only — never accepted from LLM or subagent input (honesty guardrail).

## 5. Consumer integration contract
- `joey-tools`: a process-global, config-built pool for terminal post-processing; error mapping — panicked or pool-closed map to the existing tool-error path; cancelled maps to tool-interrupted. Observable terminal-tool behavior is unchanged: identical outputs, only the execution substrate moves.
- `joey-orchestration`: may instantiate its own typed pools; passes weight-formula weights at subagent CPU-op submission points. Scheduler semantics (waves, retries, governance lanes, provider admission) are unchanged.
- Explicitly not in this contract: rayon batch sites, other blocking-pool sites, subprocess-babysit preemption (documented as a recipe only).

## 6. Compatibility and regression
- Additive only: no existing API, CLI flag, config key, on-disk format, or trait definition changes.
- Regression coverage required at landing: the terminal-tool existing suites (output pipeline unchanged), the orchestration full suite (scheduling semantics unchanged), plus the new compute suites (ordering, basic pool, fairness, chunking, end-to-end burst bound).
