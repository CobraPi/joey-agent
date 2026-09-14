# Quickstart: Subagent Resource Governance

Runnable validation scenarios keyed to FR/SC ids. Prerequisites: repo checkout; cargo (stable); no provider keys needed for harness tests (mock providers); manual scenarios use a working provider config. Config keys: [contracts/config-keys.md](contracts/config-keys.md); entities: [data-model.md](data-model.md).

## Prerequisites
- Repo checkout on branch `030-please-implement-features`; stable Rust toolchain.
- For automated scenarios (A1–A6): no API keys required — tests use the scripted mock-provider harness (tests/concurrency_limiter.rs convention).
- For manual scenarios (M1–M2): a configured provider (any) and a multi-core machine.
- Baseline for parity: `cargo test -p joey-orchestration` green BEFORE feature work as the pre-feature reference.

## A1 — Bounded concurrency + busy refusal (FR-001/002/003, SC-001)
`cargo test -p joey-orchestration --test governance_admission` — expect: with max_concurrent_children=2 and queue cap 4, 10 concurrent dispatches → at most 2 running, 4 queued, ≥4 busy refusals with exact busy text; machine and parent responsive throughout.

## A2 — Timeout + retry budget + resume (FR-004/005/006, SC-002)
`cargo test -p joey-orchestration --test governance_retry` — expect: always-failing task → bounded retries ≤ configured allowance, spaced with jittered backoff, budget never exceeded; timeout-then-succeed task → completes after resume from checkpoint without repeating turns (verifiable via transcript/turn counts); zero-checkpoint timeout → full-restart accounting.

## A3 — Single-flight + persistent cache (FR-007/008, SC-003)
`cargo test -p joey-orchestration --test governance_dedup` — expect: 5 identical simultaneous dispatches → exactly 1 child executes, 5 results; after process-restart simulation (cache reload), same signature → cache hit, 0 executions. Differing budgets → different signatures, no cross-serving.

cargo test -p joey-orchestration --test governance_retry -- --nocapture gov_concurrent_failures_respect_global_budget — expect: budget invariants under concurrent failures (system-wide in-flight retries never exceed delegation.retry_budget).

## A4 — Control-plane isolation + CPU ceiling (FR-009/010, SC-004)
`cargo test -p joey-orchestration --test governance_isolation` — expect: children saturating all slots; parent scheduling decisions (sampled via event-tap latency) complete within 2× unsaturated baseline in ≥95% of samples; runaway child (infinite-compute mock) aborted at cpu_ceiling_secs with failed-by-resource-limit outcome; siblings unaffected.

## A5 — Resource records (FR-011/012, SC-005)
`cargo test -p joey-orchestration --test governance_records` — expect: every terminal task appends exactly one record; records joinable with token telemetry via task_signature; seeded pathologies (stuck-task burn, retry amplification, control-plane starvation) distinguishable from records alone (synthetic-record fixtures assert the distinguishing queries).

cargo test -p joey-orchestration --test governance_records — expect: synthetic pathology fixtures assert distinguishing queries (A5 diagnostic half; see gov_pathologies_distinguishable_from_records_alone).

## A6 — Priority + degraded mode (FR-013, SC-006)
`cargo test -p joey-orchestration --test governance_priority` — expect: mixed-priority queue admits critical before normal in ≥95% of admission decisions under contention; degraded mode (explicitly enabled) samples background/normal at configured rate, marks outputs degraded, never samples critical; with degraded off, no degraded outputs possible.

## M1 — End-to-end feel (manual)
Run any multi-child delegation workload (e.g. a batch delegate_task). Expect: no machine exhaustion; busy notices under overload; completion notices carry tokens/duration; `~/.joey/delegation/resource-records.jsonl` grows by one line per task; `result-cache.json` present after first success.

## M2 — Parity (manual, SC-007)
Set `delegation.resource_governance.enabled=false` (all mechanisms off), run the same workload and test suite as baseline; expect behavior identical to pre-feature reference (same outputs, formats, perf profile). Also verify no records/cache files are written with governance disabled.
