# Feature Specification: Compute Pool for CPU-Bound Terminal Ops

**Feature Branch**: `033-please-reference-plan`

**Created**: 2026-09-16

**Status**: Draft

**Input**: User description: "please reference the plan in /Users/jo110366/Development/joey-agent/.hermes/plans/2026-09-15_214342-computepool-cpu-terminal-ops.md" — feature content derived in full from that plan (Compute Pool for CPU-Bound Terminal Ops — Implementation Plan, 2026-09-15).

## Clarifications

### Session 2026-09-16

- Q: Worker-count auto default floor on low-core machines (cores minus one yields zero on a single-core machine)? → A: Floor at 1 — auto = max(cores - 1, 1); single-core machines run 1 compute worker sharing the core with the async runtime.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Responsive agent during heavy terminal output processing (Priority: P1)

When the agent runs many terminal commands in parallel (for example, a subagent fan-out where 50 commands return large outputs), the CPU-heavy post-processing of those outputs (truncation, ANSI stripping, redaction) today runs on an unbounded first-in-first-out blocking pool that can oversubscribe the machine and starve the async runtime, making the whole agent feel frozen. With this feature, that work runs on a dedicated, size-limited compute pool: at most as many CPU-bound jobs run concurrently as the pool has workers, the async runtime keeps its own capacity, and the agent stays responsive while the burst completes.

**Why this priority**: This is the demonstrated oversubscription hazard the feature exists to remove; every other benefit builds on having the pool at all. Delivering this story alone already yields a viable improvement (bounded CPU concurrency for terminal post-processing).

**Independent Test**: Can be fully tested by issuing a burst of 50 parallel terminal commands with oversized outputs and verifying (a) peak concurrent post-processing jobs never exceed the configured worker count and (b) the burst completes within 2x a serial baseline — while the agent's async interactions remain responsive.

**Acceptance Scenarios**:

1. **Given** an agent executing a burst of 50 concurrent terminal commands with large outputs, **When** their post-processing jobs are dispatched, **Then** the number of post-processing jobs executing at any instant never exceeds the pool's worker count.
2. **Given** the same burst, **When** it completes, **Then** total wall time is no more than 2x the time of running the same commands' post-processing serially (sanity bound against pathological serialization).
3. **Given** CPU-bound post-processing is saturating all pool workers, **When** the user interacts with the agent's async surfaces (streaming, UI events), **Then** those interactions remain responsive because CPU work never runs on the async runtime's workers.

---

### User Story 2 - Fair, starvation-free priority scheduling (Priority: P2)

When critical-path work (a subagent whose result unblocks a long chain of downstream work) competes with background work for the compute pool, the critical-path job should finish materially sooner — but background work must never be starved: every admitted job has a bounded worst-case queue wait. Scheduling is weighted-fair by deadline: an orchestrator-assigned weight (derived from how much downstream work the job gates, never chosen by the LLM itself) shortens a job's effective wait bound proportionally.

**Why this priority**: Fairness and priority are why a plain worker pool is not enough; without bounded starvation, priority scheduling would be unsafe to enable by default.

**Independent Test**: With a single worker and a continuously refilled queue mixing weight-80 and weight-5 jobs, verify every low-weight job waits no longer than the configured worst-case bound (default ~2 seconds) and high-weight jobs' median wait is at least 4x better than low-weight jobs' median wait.

**Acceptance Scenarios**:

1. **Given** a saturated pool with continuous high-weight submissions, **When** a low-weight job is admitted, **Then** its queue wait never exceeds the configured scale bound (default 2000 ms) plus a fixed 500 ms tolerance.
2. **Given** a mixed queue of high-weight and low-weight jobs, **When** measured median queue waits are compared, **Then** high-weight jobs wait materially less (at least 4x lower median) than low-weight jobs.
3. **Given** a job submitted with an invalid weight (non-numeric or out-of-range), **When** it is admitted, **Then** the system clamps it to the supported range and schedules it safely (never crashes, never blocks the queue).

---

### User Story 3 - Reliable jobs: panics, cancellation, and shutdown (Priority: P3)

A misbehaving CPU job must not take the pool down: a job that panics returns a clear error to its submitter while the pool keeps serving at full concurrency; a job whose caller goes away while it is still queued never executes and frees its admission slot; and an explicit shutdown drains all queued jobs to completion before workers stop, rejecting only new submissions.

**Why this priority**: Robustness is required for the pool to be trusted as the substrate under all agent workloads, but the pool is already useful for well-behaved work without it.

**Independent Test**: Submit a panicking job followed immediately by a good job — the submitter receives a panic error, the good job succeeds, and the pool still serves its full worker concurrency; then enqueue several jobs, shut down, and verify all queued jobs complete and later submissions are rejected with a clear error.

**Acceptance Scenarios**:

1. **Given** a job that panics mid-execution, **When** it runs, **Then** its submitter receives an error (not a crashed pool) and immediately-submitted good jobs complete successfully at the pool's full concurrency.
2. **Given** a queued job whose receiver has been abandoned, **When** its turn comes, **Then** it is skipped without executing and the pool stays healthy for subsequent submissions.
3. **Given** jobs still queued, **When** shutdown is requested, **Then** all queued jobs complete (drain semantics) and any submission after shutdown receives a distinct "pool closed" error.

---

### User Story 4 - Operator configuration with sane auto defaults (Priority: P4)

An operator can size and tune the compute pool through the existing layered configuration (config file with environment-variable override), with sensible automatic defaults so that no configuration is required: worker count auto-sizes to the machine (all cores minus one reserved for the async runtime, never fewer than one worker), the in-flight bound defaults to 256, and the fairness scale defaults to 2000 ms.

**Why this priority**: Zero-config correctness matters for default installs, but the pool already works with defaults; explicit tuning is a convenience layer.

**Independent Test**: With no configuration set, verify the pool sizes itself to the machine's core count minus one; then set each key via config file and via environment variable and verify the values take effect with documented precedence (environment > config file > auto).

**Acceptance Scenarios**:

1. **Given** no explicit configuration, **When** the pool starts, **Then** it uses auto defaults (workers = max(cores - 1, 1), max in-flight = 256, fairness scale = 2000 ms) and requires no user action.
2. **Given** an operator-set value for any pool key, **When** the pool starts, **Then** the operator value wins over the auto default, and an environment-variable value wins over the config-file value.
3. **Given** the documented configuration reference, **When** an operator reads it, **Then** all pool keys, their defaults, and the precedence rules are listed.

---

### User Story 5 - Cooperative long-job controls and observability (Priority: P5)

Long CPU operations can be split into chunks with cancellation checks between chunks, so abandoned work stops early and returns its partial results; a watchdog warns (without killing threads) when a job runs unreasonably long; and the pool exposes counters and timing (submitted, completed, panicked, cancelled-while-queued, deadline overruns, queue-wait and service-time) so operators can see pool health.

**Why this priority**: Cancellation granularity and metrics make the pool operable at scale, but the pool is functional without them.

**Independent Test**: Run a 100-item chunked operation and cancel after three chunks — verify it stops with 30 items processed and reports incomplete; run a job that ignores cancellation past its limit and verify a warning is logged while the job still completes; run a mixed workload and verify the metric counters exactly reflect the executed work.

**Acceptance Scenarios**:

1. **Given** a chunked operation cancelled partway, **When** cancellation is checked between chunks, **Then** the operation stops, returns its partial results, and reports that it did not complete all work.
2. **Given** a job that overruns its time limit without honoring cancellation, **When** the limit is exceeded, **Then** a warning is recorded and the thread is never forcibly killed (documented limitation: truly hung jobs require process-level handling, out of scope).
3. **Given** any workload, **When** the operator reads pool metrics, **Then** submitted/completed/panicked/cancelled/overrun counts match the workload exactly, and queue-wait/service-time observations are available per weight class (counters are per-event, not disjoint: a job that overruns and still completes counts in both completed and overrun).

### Edge Cases

- A job panics while holding no pool state: pool state must remain consistent (state locks are never held across job execution; the two internal locks are never nested).
- A job's caller is dropped while the job is queued: the job must never execute, and its admission permit must be released so capacity is not leaked.
- Shutdown requested while jobs are mid-flight and queued: queued jobs drain; concurrent jobs finish; only new submissions are rejected.
- Weight is non-numeric (NaN) or outside the supported range: clamped to the minimum/maximum; never propagates into scheduling arithmetic.
- The machine exposes a single core (the auto formula would compute zero workers): the worker-count auto default floors at one; the pool stays functional with one worker sharing the core with the async runtime.
- The dependency graph used for weight estimation contains a cycle or is extremely deep (e.g. 10,000 nodes): estimation must terminate safely (cycles treated as zero remaining chain; deep graphs must not overflow).
- A job ignores cancellation checks entirely (true hang): the pool warns and continues serving other jobs; thread-killing is explicitly rejected as unsafe.
- Two jobs share the same agent identifier: both must be schedulable (the identifier is used for accounting, not exclusivity).
- Zero CPU-bound workload: the pool idles without consuming CPU; workers sleep until notified.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST execute CPU-bound post-processing of terminal-command output on a dedicated compute pool whose concurrent job execution is capped at its configured worker count, separate from the async runtime's workers.
- **FR-002**: The compute pool MUST enforce admission control with a bounded number of queued-plus-running jobs; submitters beyond the bound MUST wait for capacity rather than cause unbounded queue growth, and an admission permit MUST be held by the job for its entire execution (released at worker finish, not at submission).
- **FR-003**: The pool's scheduler MUST order jobs by deadline computed from each job's weight, such that (a) higher-weight jobs complete materially sooner than lower-weight jobs under contention and (b) every admitted job has a bounded worst-case queue wait controlled by the configured fairness scale.
- **FR-004**: Job weights MUST be clamped to a fixed supported range, and non-numeric weights MUST be mapped to the minimum weight.
- **FR-005**: The orchestrator MUST derive a job's weight from the amount of downstream work the job gates (remaining dependency-chain depth), never from the LLM or subagent itself.
- **FR-006**: A job that panics MUST be isolated: its submitter receives a distinct error and the pool MUST continue serving subsequent jobs at full worker concurrency.
- **FR-007**: A job whose receiver is abandoned while queued MUST never execute and MUST release its admission capacity; the pool MUST remain healthy for subsequent submissions.
- **FR-008**: Pool shutdown MUST drain queued jobs to completion before stopping workers; submissions after shutdown MUST receive a distinct "pool closed" error.
- **FR-009**: The system MUST expose configuration keys for worker count, maximum in-flight jobs, and fairness scale, with automatic defaults (workers auto = machine parallelism minus one, floored at one so single-core machines still get one worker), honoring the documented precedence environment variable > config file > auto default.
- **FR-010**: Long CPU operations MUST be chunkable with cancellation checked between chunks, returning partial results plus a completed/not-completed indication when cancelled early.
- **FR-011**: The system MUST warn (without killing threads) when a job exceeds its warn limit (caller-configured per job; defaults to 30 seconds), and MUST document that truly hung jobs are not forcibly terminated in v1.
- **FR-012**: The pool MUST expose counters for submitted, completed, panicked, cancelled-while-queued, and deadline-overrun jobs, plus queue-wait and service-time observations per weight class, without feeding timing observations back into scheduling weights.
- **FR-013**: All existing terminal-tool, orchestration, and workspace behaviors MUST remain unchanged apart from moving CPU-bound terminal post-processing onto the pool (strict non-regression; existing test suites stay green).

### Key Entities

- **Compute Job**: a unit of CPU-bound work submitted by a caller (terminal post-processing or subagent fan-out), carrying the submitting agent's identifier, a scheduling weight, and the operation to run.
- **Scheduling Weight**: an orchestrator-computed importance value in a fixed range, derived from the remaining dependency chain the job gates; controls deadline position, never assigned by the LLM.
- **Cancel Token**: a cooperative cancellation flag a job checks between chunks.
- **Job Outcome**: the result delivered to the submitter — success with a value, or one of the distinct failures: cancelled, panicked, pool closed.
- **Compute Pool**: the dedicated worker set with admission control, deadline-ordered fair scheduling, panic isolation, and drain-on-shutdown semantics.
- **Pool Configuration**: the operator-tunable settings (worker count, maximum in-flight jobs, fairness scale) with auto defaults and environment/file precedence.
- **Pool Metrics**: the counters and timing observations exposed for pool health monitoring.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Under a burst of 50 concurrent terminal commands with oversized outputs, peak simultaneous post-processing executions never exceed the configured worker count, and the burst finishes within 2x the serial-baseline time.
- **SC-002**: In a continuously contended pool, no admitted job waits longer than the configured fairness bound (default ~2 seconds, tested with a 500 ms tolerance) even when higher-weight work keeps arriving, and high-weight jobs' median wait is at least 4x shorter than low-weight jobs' median wait.
- **SC-003**: Immediately after a job panics or is cancelled while queued, the pool serves a newly submitted job successfully at full worker concurrency in 100% of test repetitions.
- **SC-004**: On shutdown with a populated queue, 100% of already-queued jobs complete and 100% of post-shutdown submissions receive a clear rejection.
- **SC-005**: With zero operator configuration, the pool activates with correct auto defaults on first use (no setup steps, no user action).
- **SC-006**: All pre-existing workspace test suites pass unchanged after the feature lands (non-regression).

## Assumptions

- v1 scope (from the referenced plan) intentionally excludes: core pinning, timing-history-driven (adaptive) scheduling input, per-agent hard concurrency caps, migration of non-terminal CPU call sites (search reindex, cron scheduling, other batch-parallelism), and subprocess kill-based preemption. These are recorded as deferred, not omitted by oversight.
- Weights are honesty-critical: assigned only by the orchestrator's policy; the design explicitly guards against LLM- or self-assigned weights.
- Remaining-chain estimation starts as edge-count times a configurable constant (default 1000 ms per edge) because no per-operation timing history exists; determinism is preferred over adaptive cost models (consistent with the enterprise-orchestration spec's stance).
- The pool lives as a new self-contained leaf module so both terminal tooling and orchestration can share it without either depending on the other; batch-parallelism that already uses a work-stealing pool stays where it is.
- The subprocess streaming part of terminal operations is I/O-bound and stays on the async runtime; only CPU-bound post-processing moves to the compute pool.
- Existing scheduling semantics of the task graph (waves, retries, governance lanes, provider admission) are untouched; the pool is a new substrate underneath them.
- Success-criteria tolerances (2x serial baseline, 4x median ratio, ~2 second bound) follow the referenced plan's verification strategy; timing-based assertions use tolerance-based checks, never exact-scheduler assertions.
