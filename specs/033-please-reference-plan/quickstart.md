# Quickstart and Validation Guide: Compute Pool for CPU-Bound Terminal Ops

**Feature**: specs/033-please-reference-plan | Run these after implementation to prove the feature end-to-end. Data semantics: [data-model.md](data-model.md); public surface: [contracts/api.md](contracts/api.md).

## Prerequisites
- Repo on branch `033-please-reference-plan`, toolchain per the workspace rust-toolchain file (stable, edition 2021).
- `cargo build --workspace` succeeds.
- Host memory headroom: debug test binaries are large; under heavy memory pressure the OS may kill them at startup before any test runs. Close memory-heavy applications first (observed on this machine 2026-09-16).

## Scenario 1 — Bounded concurrency under burst (SC-001)

    cargo test -p joey-tools --test compute_pool_e2e

Expected: pass — peak overlapping post-processing jobs never exceed the pool worker count across a 50-command burst with oversized outputs; burst wall time within 2x the serial baseline.

## Scenario 2 — Weighted fairness, bounded starvation (SC-002)

    cargo test -p joey-compute --test fairness

Expected: pass — every low-weight job's queue wait stays within the fairness scale (default 2 s) plus tolerance under continuous high-weight refills; high-weight median wait is less than one quarter of low-weight median wait.

## Scenario 3 — Panic isolation and queued cancellation (SC-003)

    cargo test -p joey-compute --test basic_pool

Expected: pass — a panicking job returns the panic error to its submitter; the pool immediately serves good jobs at full worker concurrency; queued jobs with dropped receivers never execute; admission permits are not leaked.

## Scenario 4 — Drain-on-shutdown (SC-004)

    cargo test -p joey-compute --test basic_pool

Expected (same suite as Scenario 3): closing with a populated queue completes all queued jobs; subsequent submissions return the pool-closed error.

## Scenario 5 — Zero-config auto defaults (SC-005)

    cargo test -p joey-core --lib
    cargo test -p joey-compute --test ordering

Expected: pass — configuration defaults resolve (workers auto = max(cores - 1, 1); max in-flight 256; scale 2000 ms; precedence environment > config > auto) and heap ordering with FIFO tiebreak hold. Manual check: run any terminal command through the CLI with no compute configuration set — behaves as before (responsive, correct output).

## Scenario 6 — Cooperative chunking and watchdog (FR-010, FR-011)

    cargo test -p joey-compute --test chunk
    cargo test -p joey-compute

Expected: pass — a 100-item run cancelled after 3 chunks returns 30 items and reports not-completed; an overrunning job logs a warning and still completes.

## Scenario 7 — Non-regression gate (SC-006, FR-013)

    cargo build --workspace
    cargo test --workspace

Expected: both green — terminal-tool, orchestration, and all other suites unchanged in behavior; metric counters match executed workloads exactly (FR-012).

## What is not validated here
Weight honesty (the LLM never assigns weights) and untouched task-graph semantics are enforced by code review plus the orchestration suite, not by a runnable scenario. Deferred items (core pinning, adaptive scheduling input, per-agent caps) have no scenarios by design — see the research decisions in [plan.md](plan.md).
