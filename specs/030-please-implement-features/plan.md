# Implementation Plan: Subagent Resource Governance

**Branch**: `030-please-implement-features` | **Date**: 2026-09-11 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/030-please-implement-features/spec.md`

## Summary

Govern subagent delegation resource use end to end. Today `joey-orchestration` already has a shared child-slot semaphore (`manager.rs`), capacity-derived sizing (`capacity.rs`), parent-reserved provider permits, and per-child token telemetry — but nothing bounds the waiting queue, retries, runaway CPU, duplicate work, or persistence of per-task costs. This feature keeps the existing pool as the single admission point and adds, in priority order: (1) a priority-lane bounded waiting queue with explicit busy refusal; (2) per-task wall-clock timeout plus a global retry budget reusing `jittered_backoff_with` from `joey-providers`; (3) turn-boundary checkpoint/resume for timed-out tasks; (4) single-flight dedup plus a persistent result cache (exact-signature compare, atomic-write pattern from the `joey-cron` JobStore); (5) control-plane isolation via a dedicated multi-thread child runtime, a CPU ceiling via a sampled watchdog with hard abort, and memory kept advisory-only; (6) priority classes with an explicitly selected degraded mode; (7) persistent resource records (JSONL) joinable with token telemetry. All mechanisms are additive, default-on, individually switchable, with zero new dependencies.

## Technical Context

**Language/Version**: Rust 2021 edition, stable toolchain (`rust-toolchain.toml`), Cargo workspace under `crates/`.

**Primary Dependencies**: existing only — `joey-orchestration` (SubagentManager, JoinSet dispatch, `capacity.rs`), `joey-providers` (`Usage`, `jittered_backoff_with`, `ProviderError`), `joey-agent-core` (`AgentEvent`), `joey-core` (layered config, `atomic_write_secure`), tokio (JoinSet, Semaphore, `time::timeout`, multi-thread Runtime). Zero new dependencies (constitution VIII; research.md R1).

**Storage**: `~/.joey/delegation/result-cache.json` (versioned envelope, LRU + TTL eviction, exact-signature compare) and `~/.joey/delegation/resource-records.jsonl` (append-only). Both follow the joey-cron JobStore atomic-write pattern (temp file + fsync + rename) with restart-tolerant loading.

**Testing**: `cargo test -p joey-orchestration` (integration tests under `tests/` plus inline `#[cfg(test)]`); full gate `cargo test --workspace`. Saturation, timeout, and retry tests reuse the scripted mock-provider harness convention from `tests/concurrency_limiter.rs` (TcpListener + delay injection + AtomicUsize in-flight probes).

**Target Platform**: cross-platform (macOS, Linux, Windows) — the CPU ceiling uses portable sampling, not cgroups; the wall-clock timeout is tokio-based.

**Project Type**: library crate extension (`joey-orchestration` plus additive defaults in `joey-core`) consumed by `joey-cli`/`joey-tui`.

**Performance Goals**: no measurable delegation-throughput regression with governance disabled (SC-007 parity); admission decisions O(log n) in queued work; cache lookup is one hash plus one exact-string compare; record append is O(1) JSONL.

**Constraints**: strict additivity — existing `delegation.*` config keys, on-disk formats, CLI behavior, and the `SubagentManager` public API stay backward-compatible (constitution VII; FR-015); disabling every mechanism must yield exact pre-feature behavior (FR-014); no new crates, no new dependencies.

**Scale/Scope**: single machine, single user; pool sized from `capacity_children` (typically ≤ 8 concurrent children); cache default 256 entries / 24h TTL; records retained under existing retention policies.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Principle | Verdict | Reasoning |
|---|-----------|---------|----------|
| 0 | Cross-platform compatibility | PASS | Sampling watchdog + tokio timeout are portable; cgroups rejected as Linux-only; no platform-specific API outside std/tokio. |
| I | Workspace-first Rust | PASS | All code in existing `crates/joey-orchestration` (plus config defaults in `joey-core` DEFAULT_CONFIG_YAML); nothing at workspace root. |
| II | CLI/TUI parity | PASS | All surfaces exposed via the existing delegation toolset and config; no new UI. |
| III | Filesystem source of truth | PASS | Governance state lives in on-disk files (result-cache.json, resource-records.jsonl) plus config; no UI-only state. |
| IV | Test-first for new modules | PASS | Every mechanism ships with tests alongside implementation (tests/ + inline), reusing the mock-provider harness. |
| V | Incremental delivery | PASS | Six stories decompose into independently shippable increments (queue/busy → timeouts/retry → cache/dedup → isolation/ceiling → accounting → priority/degrade). |
| VI | Modularity and decoupling | PASS | New logic sits behind the SubagentManager boundary in new modules (governance.rs, result_cache.rs, resource_records.rs); no sibling-crate coupling beyond additive config defaults. |
| VII | Backward compat (NON-NEGOTIABLE) | PASS | No existing public surface changes: config keys additive, new state files are new paths, existing outcome vocabulary unchanged (new outcome kinds are additive); regression coverage mandated (SC-007 parity tests). |
| VIII | Performance discipline | PASS | Zero new dependencies; O(log n) admission; one hash + exact compare per cache lookup; O(1) appends; budgets recorded in research.md. |

Post-Phase-1 re-check: unchanged — all eight PASS. The design stayed behind the SubagentManager boundary, config keys are additive, and no new dependencies were introduced. No violations; the Complexity Tracking table remains empty.

## Project Structure

### Documentation (this feature)

```text
specs/030-please-implement-features/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-orchestration/
│   ├── src/
│   │   ├── manager.rs            # extended: queue admission, priority lanes, busy refusal, retry budget, child runtime
│   │   ├── governance.rs         # NEW: bounded priority queue + busy refusal + retry-budget admission
│   │   ├── result_cache.rs       # NEW: task signature, persistent cache, single-flight map
│   │   ├── resource_records.rs   # NEW: JSONL records + sampled CPU/memory watchdog
│   │   └── types.rs              # additive types: Priority, outcome kinds, resume token
│   └── tests/
│       ├── governance_admission.rs   # quickstart A1
│       ├── governance_retry.rs       # quickstart A2
│       ├── governance_dedup.rs       # quickstart A3
│       ├── governance_isolation.rs   # quickstart A4
│       ├── governance_records.rs     # quickstart A5
│       └── governance_priority.rs    # quickstart A6
└── joey-core/
    └── src/
        └── config.rs            # DEFAULT_CONFIG_YAML: additive delegation.* governance defaults
```

**Structure Decision**: extend the existing `joey-orchestration` crate behind the `SubagentManager` boundary rather than adding a new crate (constitution I, VI); config defaults go in `joey-core`'s `DEFAULT_CONFIG_YAML`, the established home. New logic is isolated in three new modules so each mechanism is independently testable and removable.

## Complexity Tracking

> No Constitution Check violations — this section is intentionally empty.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|--------------------------------------|
| — | — | — |
