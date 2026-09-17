# Implementation Plan: Compute Pool for CPU-Bound Terminal Ops

**Branch**: `033-please-reference-plan` | **Date**: 2026-09-16 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/033-please-reference-plan/spec.md`

## Summary

Replace the unbounded FIFO blocking pool currently used as the substrate for CPU-bound terminal-op post-processing (truncate, ANSI strip, redaction) with a dedicated, core-count-sized compute pool featuring deadline-fair weighted scheduling, semaphore admission control, and panic isolation, delivered as a new leaf crate `joey-compute` shared by terminal tooling and orchestration. The repo-grounded task/milestone breakdown (file:line anchors, TDD test ordering, commit granularity) is maintained in the referenced implementation plan at `.hermes/plans/2026-09-15_214342-computepool-cpu-terminal-ops.md` and will be materialized into `tasks.md` by `/speckit-tasks`.

## Technical Context

**Language/Version**: Rust, stable channel, edition 2021 (matches workspace `rust-toolchain.toml`).

**Primary Dependencies**: Existing workspace dependencies only: `tokio` (workspace, features `["full"]`; oneshot + Semaphore on the async side), std `thread`/`sync` primitives (Mutex, Condvar, BinaryHeap, atomics) for the pool core. Existing `rayon` stays where already used. **No new external dependencies.**

**Storage**: N/A — in-memory pool state only; no persistence and no on-disk format changes (SQLite schema untouched).

**Testing**: `cargo test` — TDD for the new crate (failing test first for heap ordering, pool basics, submit semantics, fairness), `#[tokio::test]` with `Arc<AtomicUsize>` peak-concurrency probes and `eventually()`-style tolerance polling per repo convention; all existing suites must stay green (SC-006).

**Target Platform**: Cross-platform (macOS, Linux, Windows) — std threads + tokio only; no OS-specific scheduling APIs (core pinning explicitly deferred, see research.md D4).

**Project Type**: Library — new leaf crate `crates/joey-compute` inside the existing Cargo workspace, consumed by `joey-tools` and `joey-orchestration`.

**Performance Goals** (budget-bearing, Principle VIII):
- SC-001: 50-command burst — peak concurrent post-processing jobs never exceed the worker count; burst wall time within 2x the serial baseline.
- SC-002: bounded starvation — every admitted job's queue wait stays within the fairness scale (default 2 s) plus tolerance; high-weight median wait at most one quarter of low-weight median wait.
- SC-003: after a panic or queued cancellation, the pool immediately serves new jobs at full worker concurrency.
- Submit path: amortized O(log n) heap operations; no spinning when idle (workers park on the condvar).
- Hard rule: zero CPU-bound job executions on tokio workers (async runtime capacity preserved).

**Constraints**: Strict non-regression (FR-013/SC-006, Principle VII); no new runtime dependency (Principle VIII); strict crate DAG (the pool must not make `joey-tools` depend on `joey-orchestration`); deterministic scheduling inputs only (no adaptive EWMA feedback).

**Scale/Scope**: One new crate (about 4 source modules, 4 test files); surgical wiring in `joey-core` (config keys), `joey-tools` (terminal post-processing call sites), `joey-orchestration` (weight policy); documentation additions. Deferred: core pinning, EWMA scheduling input, per-agent caps, non-terminal call-site migration, subprocess-kill preemption (see research.md).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Principle | Status | Basis |
|---|-----------|--------|-------|
| 0 | Cross-platform compatibility | PASS | std threads + tokio only; no platform-specific affinity APIs; the AsyncFd streaming path is untouched |
| I | Workspace-first Rust | PASS | all code lives in the new crate `crates/joey-compute`; nothing added at workspace root |
| II | CLI/TUI parity | PASS | no UI surface; the feature is invisible plumbing behind existing surfaces |
| III | Filesystem is the source of truth | PASS | no UI state; only spec-kit artifacts on disk |
| IV | Test-first for new crates | PASS | failing-test-first ordering mandated for ordering/basic-pool/submit/fairness; tests land with each module, not after |
| V | Incremental, reviewable delivery | PASS | 8 milestones, each independently green under scoped `cargo test -p` |
| VI | Modularity & decoupling | PASS | new leaf crate with a narrow public surface (contracts/api.md); consumers depend only on the abstraction |
| VII | Backward compat & non-regression | PASS | strictly additive: no existing config key, CLI flag, on-disk format, or trait changes; new keys are new surface; regression coverage mandated (FR-013, SC-006) |
| VIII | Performance discipline & lean code | PASS | zero new dependencies (core_affinity rejected with recorded alternatives in research.md D4); performance budget recorded in Technical Context |

**Gate evaluation**: PASS — no violations; Phase 0 and Phase 1 proceed.

**Post-Phase-1 re-check**: PASS — research.md records the dependency decision and alternatives (Principle VIII), data-model.md bounds the entities and invariants, contracts/api.md declares the new public surface as a stable contract with required regression coverage (Principle VII), quickstart.md defines the runnable validation gates. No Complexity Tracking entries required.

## Project Structure

### Documentation (this feature)

```text
specs/033-please-reference-plan/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   └── api.md
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-compute/                 # NEW leaf crate (depends only on tokio + std)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs                # pool core: Entry ordering, PoolState, Inner, worker_loop,
│   │   │                         #   submit with admission, close/drain, in_flight
│   │   ├── chunk.rs              # run_chunked cooperative-cancellation helper
│   │   ├── watchdog.rs           # overrun warning guard (no thread-kill)
│   │   └── metrics.rs            # counters + queue-wait/service-time observation
│   └── tests/
│       ├── ordering.rs           # deadline-heap ordering + FIFO tiebreak (TDD)
│       ├── basic_pool.rs         # submit/backpressure/queued-cancel/panic/close-drain (TDD)
│       ├── fairness.rs           # starvation bound + weight proportionality (headline)
│       └── chunk.rs              # chunked cancellation partial results
├── joey-core/
│   └── src/config.rs             # MODIFY: orchestration.compute.* defaults + typed accessors
                                 #   + env overrides (after hypercode.execution_graph block)
├── joey-tools/
│   ├── Cargo.toml                # MODIFY: add joey-compute dependency
│   └── src/tools/
│       ├── compute_pool.rs       # NEW: config-built Lazy pool singleton + accessors
│       ├── mod.rs                # MODIFY: pub mod compute_pool
│       └── terminal_tool.rs      # MODIFY: replace spawn_blocking call sites
                                 #   (tracked-files pre-snapshot ~:701, post-processing
                                 #   pipeline ~:769-786)
│   └── tests/
│       └── compute_pool_e2e.rs   # NEW: 50-burst peak-concurrency bound
├── joey-orchestration/
│   ├── Cargo.toml                # MODIFY: add joey-compute dependency
│   └── src/
│       ├── chain_est.rs          # NEW: remaining_chain_estimate (iterative topo walk)
│       ├── compute_weight.rs     # NEW: weight_for_task
│       └── lib.rs                # MODIFY: pub mod chain_est; pub mod compute_weight
│   └── tests/
│       └── chain_est.rs          # NEW: diamond / deep-10k / cycle safety
docs/
├── compute-pool.md               # NEW: architecture, policy, config, metrics, recipes
└── README.md                     # MODIFY: index entry
PORTING.md                        # MODIFY: additive-subsystem note only
```

**Structure Decision**: a new leaf crate `crates/joey-compute` — the pool is needed by both `joey-tools` (terminal post-processing) and `joey-orchestration` (subagent fan-out), and `joey-tools` must not depend on `joey-orchestration` under the strict workspace DAG; a shared leaf crate is the only placement that satisfies the DAG. All other changes are surgical modifications inside existing crates at the anchors above; rayon batch sites and non-terminal spawn_blocking sites are deliberately untouched (research.md D12).

## Complexity Tracking

No Constitution Check violations — nothing to justify. (Section retained to record that explicitly.)
