# Data Model: Compute Pool for CPU-Bound Terminal Ops

**Feature**: specs/033-please-reference-plan | **Date**: 2026-09-16 | **Spec**: [spec.md](spec.md) | **API contract**: [contracts/api.md](contracts/api.md)

Conceptual model — field semantics independent of any language-level signature (those live in the API contract).

## Entities

### ComputeJob
A unit of CPU-bound work submitted by a caller (terminal post-processing or subagent fan-out).
- Fields: agent identifier (accounting only — never exclusivity; two jobs may share one identifier), scheduling weight, the operation to run (receives a cancel token), and internally: the admission permit, the enqueue instant, and a monotonic sequence number for FIFO tiebreaking.
- Validation: the operation must be safe to run on a pool worker; jobs are one-shot (submitted exactly once, produce exactly one outcome or a skipped-drop).
- Relationships: submitted to exactly one pool; yields exactly one job outcome.

### SchedulingWeight
Orchestrator-computed importance.
- Range [1.0, 100.0]; out-of-range values clamped to the nearest bound; non-numeric (NaN) mapped to the minimum 1.0.
- Derived only by the orchestrator's policy (see Data flows); never accepted from the LLM or subagent.

### CancelToken
Cooperative cancellation flag shared between the submitter side and the running job.
- Semantics: once set, a well-behaved job stops at its next chunk boundary; setting is idempotent; never forces termination.

### JobOutcome
Terminal result delivered to the submitter.
- Variants: success (with value) | cancelled | panicked | pool closed.
- Rules: exactly one outcome per submission; a submission whose receiver was abandoned produces no delivery and the job is skipped at pop.

### ComputePool
The execution substrate.
- Fields: fixed-size worker set (named `compute-{i}`), deadline-ordered ready heap (FIFO among equal deadlines via sequence numbers), admission bound (max queued plus running), fairness scale, running-jobs accounting keyed by agent identifier, metrics counters, shutdown flag.
- Invariants: state locks are never held across job execution and the two internal locks are never nested; workers never run on the async runtime; concurrently running jobs never exceed the worker count; queued plus running never exceeds the admission bound; shutdown drains the queue before stopping workers.

### PoolConfiguration
- Fields: `workers` (auto = max(cores minus 1, 1), or an explicit integer of at least 1), `max_inflight` (default 256, at least 1), `scale_ms` (default 2000, greater than 0), `chain_unit_ms` (default 1000, greater than 0; feeds weight estimation).
- Resolution precedence: environment variable > config file > auto default.
- Validation: workers floor of 1 enforced; non-positive scale or inflight values fall back to defaults rather than failing startup.

### PoolMetrics
- Counters: submitted, completed, panicked, cancelled-while-queued, deadline overruns; gauge: in-flight.
- Observations: per-job queue wait (enqueue to pop) and service time (pop to finish), summarized per weight class — observability only, never fed back into scheduling weights.
- Consistency: a snapshot reflects exactly the workload executed (no drift between counters and reality).

## Job lifecycle

```text
Submitted --await capacity--> Admitted --push--> Queued --pop--> Running --> Terminal
    |                            |                 |              |            |
    +-- pool closed --> PoolClosed|                 +-- receiver    +-- panic --> Panicked
                                 |                    dropped:     +-- normal --> Success(value)
                                 +-- deadline =         skip (Cancelled)
                                     now + scale / weight
```

State transition rules:
- Submitted to Admitted: the admission permit is acquired; a saturated submitter waits (backpressure, never unbounded queue growth).
- Admitted to Queued: heap push with a deadline computed from the clamped weight; FIFO tiebreak by sequence number.
- Queued to Running: a worker pops the earliest deadline and increments the running accounting.
- Queued to skipped: the receiver is already dropped — the job never executes, its permit is released, and it is counted cancelled-while-queued.
- Running to Success or Panicked: the operation completes or unwinds (isolated); the permit is released at finish and the running accounting is decremented.
- Any submission after close returns pool closed; jobs already queued at close still drain to a terminal state.

## Data flows

Weight estimation (orchestrator): task graph -> remaining-chain estimate (edge count times chain-unit constant; cycles treated as zero; deep graphs walked iteratively) -> weight formula (see API contract) -> job weight -> deadline.

Metrics flow: submit and worker events -> counters and timing observations -> snapshot for operators/telemetry. There is deliberately no path from observations back into weights (determinism).

## Volume and scale assumptions
- Queued plus running is bounded by the admission bound (default 256); ready-heap depth is O(admission bound).
- The running-accounting map is sized by distinct concurrent agents; entries are transient.
- Metrics are fixed-size counters and summaries — constant memory regardless of job volume.
