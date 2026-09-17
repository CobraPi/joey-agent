# Research: Compute Pool for CPU-Bound Terminal Ops

**Feature**: specs/033-please-reference-plan (branch `033-please-reference-plan`) | **Date**: 2026-09-16
**Sources**: spec.md; repo-grounded implementation plan `.hermes/plans/2026-09-15_214342-computepool-cpu-terminal-ops.md` (facts verified against the tree 2026-09-15).

Phase 0 output. Each entry records Decision / Rationale / Alternatives considered.

## D1 — Execution substrate for CPU-bound terminal post-processing
**Decision**: dedicated OS-thread compute pool in a new leaf crate `joey-compute`; async callers submit and await a oneshot; CPU work never runs on tokio workers.
**Rationale**: today's blocking call sites run the truncate/strip/redact pipeline on tokio's default 512-thread FIFO blocking pool — unbounded oversubscription that starves the async runtime under subagent fan-out bursts.
**Alternatives**: (a) keep blocking-pool dispatch and tune its size — still FIFO, still shared with every other blocking user, no fairness or admission semantics; (b) rayon — right for batch data-parallelism (already used for result post-processing), wrong for prioritized, cancellable, individually-awaited jobs; (c) async tasks with cooperative yielding — CPU-bound loops starve the executor, which is the exact hazard being removed.

## D2 — Scheduling policy
**Decision**: deadline-form weighted fair queuing: each admitted job gets `deadline = now + scale / weight` (scale default 2 s; weight clamped to [1.0, 100.0], NaN mapped to 1.0); workers pop earliest deadline first (heap with inverted ordering; FIFO sequence tiebreak among equal deadlines).
**Rationale**: a raw priority heap starves low-priority work; the deadline form gives weight-80 work roughly a 16:1 service advantage over weight-5 work while guaranteeing every admitted job a bounded worst-case wait of about one scale period. One structure replaces a priority ladder plus aging plus a separate fairness queue.
**Alternatives**: raw max-heap on weight (starves background work); strict FIFO (no priority at all); lottery/stochastic fair share (harder to test deterministically, weaker worst-case bounds).

## D3 — Admission control
**Decision**: bounded queued+running via a semaphore; the permit is acquired at submit and moves into the job (held until worker finish), so saturation backpressures submitters instead of growing memory with queued closures.
**Rationale**: matches the project's stated admission-control requirement — bounded queue plus running work; saturated submitters await a semaphore rather than ballooning memory.
**Alternatives**: unbounded queue with per-job spawn (memory hazard under bursts); try-acquire-and-fail (breaks the wait-for-capacity contract); bounded channel shared with the heap (couples admission to the state lock and would hold it across an await — worse).

## D4 — New dependency `core_affinity` (Principle VIII recording)
**Decision**: NOT added in v1; workers are plain named threads (`compute-{i}`); the OS scheduler places them.
**Rationale**: no new runtime dependency without a concrete, measurable benefit; pinning's benefit is unmeasured here while its cost is real (new dependency: binary size, compile time, transitive/platform-specific surface — recorded here as the Principle VIII justification). Trigger to revisit: measured async-side latency instability attributable to compute threads.
**Alternatives**: `core_affinity` with disjoint core-sets (rejected: cost unjustified by current evidence); hand-rolled affinity via libc (rejected: platform-specific unsafe code, worse than the dependency it avoids).

## D5 — Weight source (honesty + determinism)
**Decision**: weights computed ONLY by the orchestrator: `1.0 + 99.0 * (op_est + remaining_chain) / max_chain`, clamped to [1, 100]; remaining chain = edge count from an iterative topological walk times `chain_unit_ms` (default 1000 ms); op-level estimate = the same constant until a real timing signal exists. The LLM/subagent never assigns its own weight.
**Rationale**: no per-operation timing history exists in the repo, and spec 023 explicitly rejected cost-model escalation for determinism; edge-count times constant is deterministic and testable; orchestrator-only assignment is the anti-gaming guardrail.
**Alternatives**: EWMA service-time feedback into weights (rejected: determinism stance plus no existing signal — revisit only with explicit direction); caller- or LLM-supplied weights (rejected: honesty hazard); flat weights (rejected: removes the reason D2 exists).

## D6 — Panic isolation and failure containment
**Decision**: catch-unwind around each job operation; a distinct panic error to the submitter; pool state locks never held across job execution and the two internal locks never nested; queued jobs whose receiver is already dropped are skipped at pop (permit released) and never executed.
**Rationale**: a misbehaving CPU job must not take the shared substrate down; the lock discipline makes panic safety structurally impossible to violate rather than merely tested.
**Alternatives**: process-per-job isolation (rejected for v1: process model plus IPC cost out of proportion; revisit only with a concrete hang-kill requirement — see D8); letting panics unwind into workers (kills the worker and corrupts accounting).

## D7 — Cancellation and shutdown semantics
**Decision**: cooperative cancel token checked between chunks (chunked runs return partial results plus a completed flag); close() drains queued jobs then stops workers; post-close submissions get a distinct pool-closed error.
**Rationale**: true in-process preemption is impossible safely; drain-on-close matches the contract that submitted work is never silently dropped.
**Alternatives**: thread-kill on cancel (unsafe — corrupts locks); abort-on-close (drops queued work); async-task abort semantics (do not apply to owned OS threads).

## D8 — Hang handling (watchdog)
**Decision**: minimal watchdog: warn (tracing) when a job exceeds its limit; never kill threads; the limitation that truly hung non-cooperative jobs require process-level handling is documented, with the subprocess pattern recorded as a recipe in the feature docs, not wired in.
**Rationale**: killing a thread that holds arbitrary user locks corrupts the process; warning plus isolation is the safe subset; subprocess operations already have kill-based preemption through the existing process reaper where applicable.
**Alternatives**: kill-based preemption for in-process ops (rejected: unsafe); speculative re-execution (rejected: determinism stance plus cost).

## D9 — Worker-count auto default (clarified 2026-09-16)
**Decision**: `workers: auto` resolves to `max(available_parallelism() - 1, 1)` — floored at one; single-core machines run one compute worker sharing the core with the async runtime.
**Rationale**: reserves one core for the async runtime on multi-core machines but never yields a zero-worker (non-functional) pool; mirrors the existing terminal-concurrency clamp-style auto precedent.
**Alternatives**: floor at 2 (deliberate oversubscription on tiny machines — no evidence it helps); zero disables the pool on single-core (two code paths, regression surface).

## D10 — Configuration surface
**Decision**: new additive dotted keys `orchestration.compute.workers` (auto or int), `.max_inflight` (default 256), `.scale_ms` (default 2000), plus `.chain_unit_ms` (default 1000) feeding D5; environment overrides with documented precedence: environment variable > config file > auto default.
**Rationale**: follows the existing terminal-concurrency pattern verbatim (typed accessor resolving auto, environment override, documented precedence); all keys are NEW public surface, so no existing contract changes (additive-only per Principle VII).
**Alternatives**: CLI flags (wrong layer — runtime tuning, not per-invocation options); a separate pool configuration file (diverges from the layered YAML-plus-env model).

## D11 — Pool instantiation topology
**Decision**: `joey-tools` holds a process-global lazily-built pool constructed from config (mirroring the existing terminal-governor singleton); `joey-orchestration` may instantiate its own typed pools; both share the configuration keys; tests needing non-default sizes construct their own pool through the public constructor; tests touching the singleton serialize on a static mutex (existing convention).
**Rationale**: a single substrate per process for the hot terminal path (cheap, config-consistent); the public constructor is the test seam; avoids a global registry abstraction nobody needs.
**Alternatives**: one global shared across all crates (forces a monomorphic type or object-safety contortions); per-call pools (lose admission bounds across bursts).

## D12 — Migration scope (what moves, what stays)
**Decision**: ONLY the terminal-tool CPU post-processing call sites (the truncate/strip/redact pipeline and the tracked-files pre-snapshot) move onto the pool in v1; existing rayon batch sites and the other blocking-pool sites (search reindex, cron scheduler, CLI engine) are explicitly out of scope; subprocess streaming stays on the async runtime (I/O-bound, correct as-is).
**Rationale**: the migrated sites are the demonstrated oversubscription hazard; rayon is the right tool for the batch sites; broad migration inflates regression surface without evidence of hazard.
**Alternatives**: migrate every blocking/rayon site (rejected: regression surface without measured hazard); migrate nothing (the hazard remains).

## Deferred / open (recorded, not blocking)
- Core pinning (D4) — revisit on measured async-side latency instability.
- EWMA as scheduling input (D5) — revisit only with explicit direction; determinism stance stands.
- Per-agent hard caps — in-flight gauge plus metrics expose the data; build only on evidence.
- Subprocess-babysit operation pattern — documented as a recipe, not wired (existing reaper already covers it).
