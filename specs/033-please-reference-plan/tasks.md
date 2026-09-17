# Tasks: Compute Pool for CPU-Bound Terminal Ops

**Input**: Design documents from `/specs/033-please-reference-plan/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/api.md, quickstart.md — all present.

**Tests**: Included. Constitution Principle IV (test-first for new crates) and the spec's FR/SC acceptance map require test-first delivery; the referenced implementation plan (`.hermes/plans/2026-09-15_214342-computepool-cpu-terminal-ops.md`) is explicitly TDD-ordered.

**Organization**: Tasks grouped by user story (US1–US5 map to spec.md stories P1–P5) so each story is independently implementable and testable.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (US1–US5)
- Exact file paths in every description
- Story-phase tasks carry the story label; Setup/Foundational/Polish tasks do not

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Scaffold the new leaf crate; register it in the workspace.

- [x] T001 Create `crates/joey-compute/Cargo.toml` (deps: `tokio.workspace = true` only) and `crates/joey-compute/src/lib.rs` module skeleton with the crate-level doc comment stating the hard rule (CPU work never runs on tokio workers); register the crate in root `Cargo.toml` workspace members and `[workspace.dependencies]`. Verify: `cargo build -p joey-compute`.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The pool core and its configuration — every user story builds on these.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [x] T002 Implement core types in `crates/joey-compute/src/lib.rs` per `specs/033-please-reference-plan/contracts/api.md` §2: `AgentId`, `JobError` (Cancelled | Panicked | PoolClosed), `CancelToken` (is_cancelled, idempotent set), `JobSpec<T>` (agent, weight, op receiving &CancelToken). Verify: `cargo build -p joey-compute`.
- [x] T003 TDD deadline-heap entry: write failing test `crates/joey-compute/tests/ordering.rs` (earliest-deadline-first; FIFO among equal deadlines via sequence tiebreak; max-heap inversion — "greatest" Entry is most urgent; cases now+2s/w1, now+400ms/w5 at scale 2s, now+40ms/w80), then implement `Entry<T>` with inverted `Ord` in `crates/joey-compute/src/lib.rs`. Verify red→green: `cargo test -p joey-compute --test ordering`.
- [x] T004 TDD worker loop: write failing tests in `crates/joey-compute/tests/basic_pool.rs` (2 workers, 4 short jobs all Ok; `in_flight()` returns to empty within 5s using 25ms polling), then implement `PoolState`, `Inner`, and `worker_loop` in `crates/joey-compute/src/lib.rs` (state lock never held across op; the two internal locks never nested; pop → skip-if-receiver-closed → running-accounting inc → run → dec). Verify red→green: `cargo test -p joey-compute --test basic_pool`.
- [x] T005 TDD submit with admission: extend `crates/joey-compute/tests/basic_pool.rs` ((a) max_inflight=1: job2 does not start until job1's permit releases at worker finish; (b) close() → subsequent submit returns PoolClosed), then implement `submit()` in `crates/joey-compute/src/lib.rs` (acquire permit → oneshot → clamp weight to [1.0,100.0] with NaN→1.0 → deadline = now + scale/weight → push Entry carrying the permit → notify worker). Verify red→green: `cargo test -p joey-compute --test basic_pool`.
- [x] T006 [P] Add config keys in `crates/joey-core/src/config.rs`: `orchestration.compute.workers` (auto = max(cores - 1, 1), floored at one), `.max_inflight` (256), `.scale_ms` (2000), `.chain_unit_ms` (1000) after the `hypercode.execution_graph` block in DEFAULT_CONFIG_YAML; environment overrides (`ORCHESTRATION_COMPUTE_WORKERS` etc.) with precedence env > config > auto; typed accessor `compute_workers()` resolving auto per the terminal.max_concurrent pattern; update the config-defaults snapshot test in the same commit if one exists. Verify: `cargo build -p joey-core && cargo test -p joey-core`.

**Checkpoint**: Pool core + config ready — user story implementation can begin.

---

## Phase 3: User Story 1 - Responsive agent during heavy terminal output processing (Priority: P1) 🎯 MVP

**Goal**: CPU-bound terminal post-processing runs on the bounded pool; peak concurrency ≤ workers under a 50-command burst; async runtime keeps its capacity.

**Independent Test**: `cargo test -p joey-tools --test compute_pool_e2e` plus the existing terminal suites — SC-001 satisfied in isolation.

### Implementation for User Story 1

- [x] T007 [US1] Create `crates/joey-tools/src/tools/compute_pool.rs`: process-global lazily-built `ComputePool<Vec<u8>>` constructed from config (workers = compute_workers(), max_inflight, scale) mirroring the terminal_governor Lazy singleton pattern, with public accessors; add `joey-compute.workspace = true` to `crates/joey-tools/Cargo.toml` and `pub mod compute_pool;` to `crates/joey-tools/src/tools/mod.rs`. Verify: `cargo build -p joey-tools`.
- [x] T008 [US1] Replace the `tokio::task::spawn_blocking` call sites in `crates/joey-tools/src/tools/terminal_tool.rs` — the tracked-files pre-snapshot (~line 701) and the truncate→ANSI-strip→redact post-processing pipeline (~lines 769-786) — with pool submissions (the CPU-bound work of each call site moves to a pool op unchanged; weight constant 5.0 with a `// TODO(weight): replace with rank-derived weight` comment; governor slot released before submit; error mapping per contracts/api.md §5: Panicked/PoolClosed → tool-error path, Cancelled → tool-interrupted). Verify: `cargo build -p joey-tools && cargo test -p joey-tools --test terminal_streaming`.
- [x] T009 [US1] [P] Create `crates/joey-tools/tests/compute_pool_e2e.rs`: 50-burst `sleep 0.25` commands with oversized outputs; assert peak overlapping post-processing jobs ≤ workers via an Arc<AtomicUsize> fetch_max probe inside a wrapped op using a test-constructed pool instance (not the singleton); assert burst wall time ≤ 2x serial baseline. Verify: `cargo test -p joey-tools --test compute_pool_e2e`.
- [x] T010 [US1] Regression gate (Principle VII / FR-013 / SC-006): run the full `cargo test -p joey-tools` suite and confirm terminal-tool behavior is unchanged (identical outputs; only the execution substrate moved). Fix only regressions introduced by T007–T009.

**Checkpoint**: US1 fully functional and independently testable — this is the MVP.

---

## Phase 4: User Story 2 - Fair, starvation-free priority scheduling (Priority: P2)

**Goal**: Deadline-form weighted fair queuing with a bounded starvation guarantee; orchestrator-derived weights from remaining dependency-chain depth.

**Independent Test**: `cargo test -p joey-compute --test fairness` and `cargo test -p joey-orchestration --test chain_est` — SC-002 satisfied in isolation.

### Implementation for User Story 2

- [x] T011 [US2] [P] Create `crates/joey-compute/tests/fairness.rs` (the headline test): 1 worker; queue continuously refilled with alternating weight-80 (agent A) and weight-5 (agent B) jobs; assert every B job's measured queue wait ≤ scale (2s) + 500 ms tolerance under continuous A refills, and A median wait < B median / 4; use tolerance-based assertions (fixed 500 ms over scale) with 25ms eventually()-style polling, never exact-scheduler assertions; mark `#[ignore]` only if demonstrably flaky, keeping it in the default local suite. Verify: `cargo test -p joey-compute --test fairness`.
- [x] T012 [US2] [P] Create `crates/joey-orchestration/src/chain_est.rs` (`remaining_chain_estimate(graph, id)`: iterative topological walk, edge count × `orchestration.compute.chain_unit_ms`, cycles → 0, no recursion — 10k-deep graphs must not overflow) and `crates/joey-orchestration/tests/chain_est.rs` (diamond graph; deep chain 10k; cyclic input); register `pub mod chain_est;` in `crates/joey-orchestration/src/lib.rs`. Verify: `cargo test -p joey-orchestration --test chain_est`.
- [x] T013 [US2] Create `crates/joey-orchestration/src/compute_weight.rs` (`weight_for_task(graph, id, op_est)` = `1.0 + 99.0 * (op_est + remaining_chain) / max_chain`, clamped [1,100]; max_chain = largest remaining-chain across known tasks floored at one chain-unit constant, per contracts/api.md §4; op_est defaults to the chain-unit constant — EWMA explicitly rejected for determinism), add `joey-compute.workspace = true` to `crates/joey-orchestration/Cargo.toml`, and pass `weight: weight_for_task(...)` at the subagent CPU-op submission points in the dispatch path of `crates/joey-orchestration/src/manager.rs`. Weights are orchestrator-assigned only — never LLM-supplied. Verify: `cargo test -p joey-orchestration` (all existing suites green; scheduler semantics unchanged).

**Checkpoint**: US1 and US2 both independently functional.

---

## Phase 5: User Story 3 - Reliable jobs: panics, cancellation, and shutdown (Priority: P3)

**Goal**: Panic isolation, queued-cancellation cleanup, and drain-on-shutdown.

**Independent Test**: `cargo test -p joey-compute` — SC-003 and SC-004 satisfied in isolation.

### Implementation for User Story 3

- [x] T014 [US3] Add panic-isolation test to `crates/joey-compute/tests/basic_pool.rs`: submit a panicking op → submitter receives Err(JobError::Panicked); immediately submit a good job → Ok; pool still serves full worker concurrency (peak-probe). If it fails, fix the catch_unwind boundary in `crates/joey-compute/src/lib.rs`. Verify: `cargo test -p joey-compute`.
- [x] T015 [US3] Add queued-cancellation test to `crates/joey-compute/tests/basic_pool.rs`: drop the receiver while a job is queued → the job never runs (atomic flag stays false) and its admission permit is released (a subsequent submit is not deadlocked); pool healthy. Verify: `cargo test -p joey-compute --test basic_pool`.
- [x] T016 [US3] Add close-drain test to `crates/joey-compute/tests/basic_pool.rs`: enqueue several jobs, call close() → all queued jobs complete (drain semantics), post-close submits return PoolClosed, workers exit. Verify: `cargo test -p joey-compute`.

**Checkpoint**: US1–US3 independently functional.

---

## Phase 6: User Story 4 - Operator configuration with sane auto defaults (Priority: P4)

**Goal**: Documented, layered configuration with correct auto defaults and precedence.

**Independent Test**: `cargo test -p joey-core` config tests — SC-005 satisfied in isolation.

### Implementation for User Story 4

- [x] T017 [US4] [P] Add configuration tests in `crates/joey-core` (extend the existing config test module): auto defaults resolve (workers = max(cores - 1, 1) including the single-core floor; max_inflight 256; scale_ms 2000; chain_unit_ms 1000); precedence env > config file > auto; invalid values fall back to defaults with a warning, never panic. Verify: `cargo test -p joey-core`.
- [x] T018 [US4] [P] Document the `orchestration.compute.*` keys in `docs/state-and-config.md` §2 (new group after the terminal/delegation entries): each key, type, default, environment override, and the precedence rule, matching contracts/api.md §3.

**Checkpoint**: US1–US4 independently functional.

---

## Phase 7: User Story 5 - Cooperative long-job controls and observability (Priority: P5)

**Goal**: Chunked cancellation, watchdog warnings, and pool metrics.

**Independent Test**: `cargo test -p joey-compute --test chunk` plus metrics tests — FR-010/FR-011/FR-012 satisfied in isolation.

### Implementation for User Story 5

- [x] T019 [US5] [P] Create `crates/joey-compute/src/chunk.rs` (`run_chunked(items, cancel_token, chunk_size, f)`: cancellation checked between chunks only; returns (partial results, completed-all flag)) re-exported from lib.rs, and `crates/joey-compute/tests/chunk.rs` (100 items, chunk 10, cancel after 3 chunks → 30 items, completed=false; no cancel → full, completed=true). Verify: `cargo test -p joey-compute --test chunk`.
- [x] T020 [US5] [P] Create `crates/joey-compute/src/watchdog.rs` (`Watchdog { limit }` with a guard that emits a `tracing::warn!` when a job exceeds its limit (limit is caller-configured per job, default 30 seconds, independent of scale_ms); never kills threads; documents that true hang-killing requires process-level handling and is out of scope v1) with a test proving: op sleeps 500ms ignoring cancel, limit 100ms → warning captured via a tracing test subscriber AND the job still completes. Verify: `cargo test -p joey-compute`.
- [x] T021 [US5] Create `crates/joey-compute/src/metrics.rs` (AtomicU64 counters: submitted, completed, panicked, cancelled_queued, deadline_overruns; in_flight gauge already exists; per-job queue-wait (enqueue→pop) and service-time (pop→finish) summarized per weight class (low 1.0–24.9, mid 25.0–74.9, high 75.0–100.0) — observability only, never fed back into weights; `snapshot()` accessor) and instrument submit/worker_loop; tests: mixed workload → snapshot counts match exactly; single deterministic job → queue_wait ≈ 0 and service_time ≥ op duration. Verify: `cargo test -p joey-compute`.

**Checkpoint**: All user stories independently functional.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, parity tracking, and the end-to-end validation gate.

- [x] T022 [P] Create `docs/compute-pool.md` (architecture and hard rule; scheduling policy; config keys; metrics; chunking guidance incl. the ~100µs small-op threshold; the subprocess-babysit op pattern as a documented recipe only; the singleton-vs-per-crate instantiation note) and add its index entry to `docs/README.md`.
- [x] T023 [P] Add a Compute Pool section to `PORTING.md`: additive subsystem, no upstream-parity surface touched, "Deliberate divergence: none".
- [x] T024 Run every validation scenario in `specs/033-please-reference-plan/quickstart.md` (Scenarios 1–7) and record results; any failure triggers a scoped fix in the owning task's files, not a spec change.
- [ ] T025 Final gate: `cargo build --workspace && cargo test --workspace` exactly once; on failure, one fix round using targeted `-p <crate>` tests only, then re-run the gate once. Note: debug test binaries are large — run only with host memory headroom (see quickstart.md Prerequisites).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately.
- **Foundational (Phase 2)**: Depends on T001 — BLOCKS all user stories.
- **User Stories (Phases 3–7)**: Each depends on Phase 2 completion; stories may proceed sequentially in priority order (US1 → US2 → US3 → US4 → US5) or in parallel where files differ (see Parallel Opportunities).
- **Polish (Phase 8)**: Depends on all user stories being complete.

### User Story Dependencies

- **US1 (P1)**: After Phase 2. No story dependencies (MVP).
- **US2 (P2)**: After Phase 2; T013 depends on T012 and on the pool submit (T005).
- **US3 (P3)**: After Phase 2; extends `crates/joey-compute/tests/basic_pool.rs` created in Phase 2 (run after T005).
- **US4 (P4)**: After Phase 2 (T006 specifically for T017).
- **US5 (P5)**: After Phase 2; independent of US1–US4 files.

### Within Each User Story

- Tests written first and failing before implementation (constitution Principle IV).
- Core implementation before integration/wiring.
- Story checkpoint validated before moving to the next priority.

### Parallel Opportunities

- T006 runs parallel to T002–T005 (different crate).
- T009 parallel to nothing inside US1 (T007→T008 sequential; T009 after T007).
- T011 ∥ T012 (different crates), then T013.
- T017 ∥ T018 (test vs docs).
- T019 ∥ T020 (different new files), then T021.
- T022 ∥ T023 ∥ T024-prep (different files).

---

## Parallel Example: User Story 2

```bash
# After Phase 2 completes, launch in parallel (different crates/files):
Task: "T011 [US2] fairness headline test in crates/joey-compute/tests/fairness.rs"
Task: "T012 [US2] chain estimator + tests in crates/joey-orchestration/src/chain_est.rs"
# Then sequentially:
Task: "T013 [US2] weight function + manager.rs wiring in crates/joey-orchestration"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (Setup) and Phase 2 (Foundational).
2. Complete Phase 3 (US1: pool accessor, terminal wiring, e2e bound, regression gate).
3. **STOP and VALIDATE**: `cargo test -p joey-tools --test compute_pool_e2e` green and terminal suites unchanged.
4. This alone removes the demonstrated oversubscription hazard (spec SC-001).

### Incremental Delivery

1. Setup + Foundational → foundation ready.
2. + US1 → validate → MVP delivered.
3. + US2 → validate fairness/weights → deliver.
4. + US3 → validate reliability → deliver.
5. + US4 → validate configuration → deliver.
6. + US5 → validate chunking/watchdog/metrics → deliver.
7. Phase 8 polish → final workspace gate.

### Parallel Team Strategy

1. Team completes Phases 1–2 together.
2. Then: Developer A → US1; Developer B → US2 (T011/T012 first); Developer C → US5 (T019/T020 first).
3. US3 and US4 pick up after their file dependencies free (basic_pool.rs; config tests).

---

## Notes

Note: [P] tasks = different files, no dependencies on incomplete tasks.
Note: [Story] labels map tasks to spec.md user stories for traceability.
- Every verification command is a scoped `-p <crate>` (or named `--test`) run — never the full workspace suite until T025.
- Weight honesty: weights come only from the orchestrator's `weight_for_task`; never accept LLM/subagent-supplied weights.
- Deferred by design (see research.md): core pinning, EWMA scheduling input, per-agent caps, non-terminal call-site migration, subprocess-kill preemption wiring.
- Commit after each task or logical group; stop at any checkpoint to validate the story independently.
