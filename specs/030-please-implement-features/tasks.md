# Tasks: Subagent Resource Governance

**Input**: Design documents from `/specs/030-please-implement-features/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Included — the constitution (Principle IV) and quickstart.md scenarios A1–A6 mandate tests alongside implementation; test tasks are written FIRST and must FAIL before implementation.

**Organization**: Tasks grouped by user story (spec.md US1–US6, priority order) so each story is independently implementable and testable.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (US1–US6)
- Exact file paths in every description

## Path Conventions

Workspace layout per plan.md: implementation lives in `crates/joey-orchestration/` (src + tests) with additive config defaults in `crates/joey-core/src/config.rs`.

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Config surface and shared types every mechanism reads

- [X] T001 Add all 17 governance config keys with defaults and clamps to DEFAULT_CONFIG_YAML in crates/joey-core/src/config.rs exactly as specified in specs/030-please-implement-features/contracts/config-keys.md (resource_governance.enabled=true, max_queue_depth=auto, task_timeout_secs=600, retry_budget=2, backoff_base_secs=2.0, backoff_max_secs=60.0, checkpointing.enabled=true, result_cache.enabled=true, result_cache.max_entries=256, result_cache.ttl_hours=24, single_flight.enabled=true, cpu_ceiling_secs=300, watchdog_interval_secs=1, memory_tracking.enabled=true, priority.enabled=true, degraded_mode.enabled=false, degraded_mode.sample_rate=0.1); add config unit tests asserting every default and clamp
- [X] T002 [P] Add additive governance types to crates/joey-orchestration/src/types.rs: Priority enum (critical|normal|background, Default=normal, serde rename_all lowercase), ResourceRecordOutcome enum for resource records (timeout, aborted_by_resource_limit, busy_refused, cache_hit — record-level vocabulary only; DelegationResult outcome semantics stay untouched, busy refusal remains a dispatch-level ToolResult per contracts/busy-and-outcomes.md), ResourceRecord and ResumeToken structs per specs/030-please-implement-features/data-model.md and contracts/resource-record.md; unit tests for serde round-trip and defaults

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Signature, persistence stores, and config wiring all stories share

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T003 Create crates/joey-orchestration/src/resource_records.rs: ResourceRecordStore appending one JSON object per terminal outcome to ~/.joey/delegation/resource-records.jsonl (append-only, tolerant load skipping a malformed trailing line, exact field set from contracts/resource-record.md); unit tests for append, round-trip, and tolerant load
- [X] T004 Create crates/joey-orchestration/src/result_cache.rs part 1: canonical task_signature(dispatch) function producing deterministic-key-order JSON of { goal, context, toolsets, model_override, role, budgets { timeout_secs, cpu_ceiling_secs } } per data-model.md Task Signature; unit tests pinning signature determinism, stability across restarts (same input → byte-identical string), and difference on every result-affecting field change
- [X] T005 Extend crates/joey-orchestration/src/result_cache.rs part 2: ResultCache envelope { schema_version: 1, entries[] } with LRU eviction (max_entries), TTL (ttl_hours) on read, exact byte-for-byte signature compare, success-only storage, atomic save (temp + fsync + rename, JobStore pattern) at ~/.joey/delegation/result-cache.json; unit tests for eviction, TTL expiry, persistence across store reload, and no cross-serving across differing signatures
- [X] T006 Wire GovernanceConfig into SubagentManager: read all T001 keys via cfg.get_* in crates/joey-orchestration/src/manager.rs, resolve max_queue_depth=auto to 2 × resolved max_concurrent_children (capacity-derived), clamp queue depth and pool to ≥ 1, and make delegation.resource_governance.enabled=false bypass every governance code path; unit tests for resolution, clamps, and master-switch bypass

**Checkpoint**: Foundation ready — user story implementation can now begin in parallel

---

## Phase 3: User Story 1 — Concurrency Is Bounded with Admission Control (Priority: P1) 🎯 MVP

**Goal**: All subagent work flows through the existing shared pool plus a bounded priority-ordered waiting queue; overflow is refused with an explicit busy signal (FR-001/002/003)

**Independent Test**: quickstart.md A1 — `cargo test -p joey-orchestration --test governance_admission`

### Tests for User Story 1

- [X] T007 [P] [US1] Write crates/joey-orchestration/tests/governance_admission.rs using the scripted mock-provider harness convention from tests/concurrency_limiter.rs (TcpListener + delay injection + AtomicUsize in-flight probes): with max_concurrent_children=2 and queue cap 4, 10 concurrent dispatches yield at most 2 running, 4 queued, and ≥ 4 busy refusals carrying the exact text from contracts/busy-and-outcomes.md; defaults test proves capacity-derived safe bounds with no config; governance-off test proves zero refusals/queueing; MUST FAIL before implementation

### Implementation for User Story 1

- [X] T008 [US1] Create crates/joey-orchestration/src/governance.rs: bounded waiting queue (lane-ordered VecDeque, FIFO within lane, cap = resolved max_queue_depth) integrated into SubagentManager so ALL dispatch kinds (blocking singles, batches, background waves) pass through it; when slots and queue are full, return the busy ToolResult per contracts/busy-and-outcomes.md with no side effects
- [X] T009 [US1] Emit additive DelegationBusy AgentEvent through the existing event_tap (manager.rs) carrying queue depth and cap, in crates/joey-orchestration/src/manager.rs

**Checkpoint**: US1 independently functional — bounded admission verifiable via governance_admission.rs

---

## Phase 4: User Story 2 — Retries Are Bounded, Spaced, and Resumable (Priority: P1)

**Goal**: Per-task wall-clock timeout, jittered backoff under a global retry budget, turn-boundary checkpoint/resume (FR-004/005/006)

**Independent Test**: quickstart.md A2 — `cargo test -p joey-orchestration --test governance_retry`

### Tests for User Story 2

- [X] T010 [P] [US2] Write crates/joey-orchestration/tests/governance_retry.rs (same mock harness): always-failing task shows retries within the per-task allowance spaced by jittered exponential backoff and system-wide in-flight retries never exceeding delegation.retry_budget; a task that times out once then succeeds completes after resume without repeating completed turns (turn-count assertion); a timeout with no valid checkpoint counts as a full restart against the budget; MUST FAIL before implementation

### Implementation for User Story 2

- [X] T011 [US2] Implement per-task wall-clock timeout in crates/joey-orchestration/src/governance.rs + manager.rs: tokio::time::timeout (delegation.task_timeout_secs, 0=disabled) around the child future, cancellation via the existing interrupt path, timeout reported as a new outcome kind, consuming one retry attempt
- [X] T012 [US2] Implement the global retry budget in crates/joey-orchestration/src/governance.rs: counting guard (in-flight retries < delegation.retry_budget) with fast-fail reason `retry budget exhausted`; retry delays reuse jittered_backoff_with from joey-providers with backoff_base_secs/backoff_max_secs; existing subagent recovery attempts count against the budget; enforce the per-task retry allowance via the existing delegation.subagent_recovery_attempts key alongside the global budget (SC-002)
- [X] T013 [US2] Implement turn-boundary checkpoint/resume in crates/joey-orchestration/src/subagent.rs + resource_records.rs: record ResumeToken { last_completed_turn, transcript_digest, recorded_at } after each completed turn; a re-dispatched timed-out/failed task presenting a digest-matching token skips completed turns and continues at the next turn boundary (contracts/checkpoint-token.md); stale token → full restart, fully counted

**Checkpoint**: US1 + US2 both independently functional

---

## Phase 5: User Story 3 — The Control Plane Stays Responsive Under Saturation (Priority: P2)

**Goal**: Children execute on a dedicated runtime; runaway CPU is hard-aborted; memory stays advisory (FR-009/010)

**Independent Test**: quickstart.md A4 — `cargo test -p joey-orchestration --test governance_isolation`

### Tests for User Story 3

- [X] T014 [P] [US3] Write crates/joey-orchestration/tests/governance_isolation.rs (same mock harness): with children saturating every slot, parent scheduling decisions (event-tap latency probes) complete within 2× their unsaturated baseline in ≥ 95% of samples; a runaway child (infinite-compute mock) is aborted at delegation.cpu_ceiling_secs with outcome aborted_by_resource_limit while sibling tasks finish unaffected; MUST FAIL before implementation

### Implementation for User Story 3

- [X] T015 [US3] Create the dedicated child runtime in crates/joey-orchestration/src/manager.rs: manager-owned multi-thread tokio Runtime sized to max_concurrent_children worker threads; all child execution moves onto it while the parent turn loop and provider calls stay on the main runtime; parent_reserved_permits behavior preserved; event taps and steering keep working across the runtime boundary
- [X] T016 [US3] Implement the sampling watchdog in crates/joey-orchestration/src/resource_records.rs: portable per-child CPU sampling at watchdog_interval_secs (mechanism: process-level CPU sampling apportioned to the children running during each interval per research.md R5b — cross-platform, no cgroups; land it with a Windows validation note), cumulative CPU over cpu_ceiling_secs triggers abort via the interrupt path with outcome aborted_by_resource_limit; peak RSS sampled advisory-only (memory_tracking.enabled) into memory_peak_kb; sampled fields labeled in records

**Checkpoint**: US1–US3 independently functional

---

## Phase 6: User Story 4 — Identical Work Runs Exactly Once (Priority: P2)

**Goal**: Cache → single-flight → queue lookup order; persistent exact-signature cache (FR-007/008)

**Independent Test**: quickstart.md A3 — `cargo test -p joey-orchestration --test governance_dedup`

### Tests for User Story 4

- [X] T017 [P] [US4] Write crates/joey-orchestration/tests/governance_dedup.rs (same mock harness): 5 identical simultaneous dispatches produce exactly 1 child execution and 5 identical results; after simulating a process restart (store reload), the same signature returns from cache with 0 executions; dispatches differing only in budget fields produce different signatures and never cross-serve; MUST FAIL before implementation

### Implementation for User Story 4

- [X] T018 [US4] Wire dedup into the dispatch path in crates/joey-orchestration/src/manager.rs + result_cache.rs: lookup order cache → single-flight map → queue admission; cache hits return without consuming a slot and record outcome cache_hit; single-flight (single_flight.enabled) makes identical in-flight signatures await the first result; successes stored to the persistent cache; single-flight entries removed on completion

**Checkpoint**: US1–US4 independently functional

---

## Phase 7: User Story 5 — Every Task's Cost Is Accounted and Diagnosable (Priority: P2)

**Goal**: One resource record per terminal outcome, joinable with token telemetry, sufficient to distinguish the three pathologies (FR-011/012)

**Independent Test**: quickstart.md A5 — `cargo test -p joey-orchestration --test governance_records`

### Tests for User Story 5

- [X] T019 [P] [US5] Write crates/joey-orchestration/tests/governance_records.rs: every terminal task (completed, failed, timeout, aborted_by_resource_limit, busy_refused, cache_hit) appends exactly one record with the full contracts/resource-record.md field set; records join with token telemetry via task_signature + token_usage; synthetic pathology fixtures (stuck-task burn vs aggregate load; retry amplification; control-plane starvation via elevated parent_starved_ms) are distinguishable by querying records alone; MUST FAIL before implementation

### Implementation for User Story 5

- [X] T020 [US5] Emit records at every terminal path in crates/joey-orchestration/src/manager.rs + resource_records.rs: queue_wait_ms (submit→admission), compute_ms (admission→terminal, sampled), cpu_ms/memory_peak_kb/parent_starved_ms (advisory, sampled) from the watchdog and event-tap latency probes, retries, checkpoint token, token_usage mirroring joey-providers Usage, priority, degraded flag — for all dispatch kinds including busy refusals and cache hits

**Checkpoint**: US1–US5 independently functional

---

## Phase 8: User Story 6 — Overload Defers and Degrades Gracefully (Priority: P3)

**Goal**: Priority lanes with jump-the-line; explicitly selected degraded mode with marked sampled outputs (FR-013)

**Independent Test**: quickstart.md A6 — `cargo test -p joey-orchestration --test governance_priority`

### Tests for User Story 6

- [X] T021 [P] [US6] Write crates/joey-orchestration/tests/governance_priority.rs (same mock harness): under contention, critical work is admitted before normal in ≥ 95% of admission decisions; normal-lane work still completes within a bound under continuous critical load; with degraded_mode.enabled=true, background/normal work is sampled at sample_rate, every sampled output carries the [degraded] marker and degraded=true record, and critical work is never sampled; with degraded mode off, no degraded output can occur; MUST FAIL before implementation

### Implementation for User Story 6

- [X] T022 [US6] Implement priority-lane admission in crates/joey-orchestration/src/governance.rs (priority.enabled): additive priority field on dispatch requests (default normal; background waves enqueue as background); when a slot frees, admit the highest-priority lane head; critical jumps the queue line only — never preempts running work, never bumps queued work
- [X] T023 [US6] Implement explicitly selected degraded mode in crates/joey-orchestration/src/governance.rs (degraded_mode.enabled, default false, never auto-engaged): sample background+normal work at sample_rate, mark outputs per contracts/busy-and-outcomes.md ([degraded] text marker + degraded=true record), critical never sampled; surface the sustained-overload signal (busy refusals ≥ 3 in 60s) by appending the overload text to busy refusals so the assistant can select degraded mode when re-planning

**Checkpoint**: All six user stories independently functional

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Parity gates, event compatibility, docs, final validation

- [X] T024 [P] Regression/parity suite in crates/joey-orchestration/tests/governance_parity.rs: with delegation.resource_governance.enabled=false, no governance outcome, record, cache file, or governance event can occur, and delegation behavior matches the pre-feature baseline (SC-007, FR-014); iterate every per-mechanism switch (checkpointing.enabled, result_cache.enabled, single_flight.enabled, memory_tracking.enabled, priority.enabled, degraded_mode.enabled, plus 0-means-disabled numeric keys) disabling each in turn and asserting that mechanism reverts to pre-feature behavior while the others stay active (FR-014); config snapshot test asserting all T001 defaults; SC-008 assertions — cache and record files created with user-only permissions (atomic_write_secure) and untouched by any redaction or network layer
- [X] T025 [P] Event compatibility tests in crates/joey-orchestration: serialization snapshots proving the new additive AgentEvent variants (DelegationBusy, DelegationTimeout, DelegationRetryBudgetExhausted, DelegationCacheHit, DelegationDegradedOutput, capacity snapshot) do not alter existing variants (constitution VII)
- [X] T026 [P] Event emission audit across mechanisms in crates/joey-orchestration/src/: verify every governance mechanism emits its event via event_tap (research.md R10) and that CLI/TUI surfaces see them unmodified
- [X] T027 Update PORTING.md at repo root with the feature-030 Joey-only additions section (in-process pool extension, dedicated child runtime, sampled CPU watchdog, JSONL records, persistent result cache — no upstream equivalent), dated, per research.md R11
- [X] T028 Run full validation: cargo test --workspace green; then execute specs/030-please-implement-features/quickstart.md scenarios A1–A6 (automated) and record M1/M2 manual results

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies; T001 and T002 are parallel (different crates)
- **Foundational (Phase 2)**: Depends on Phase 1; T003 starts first, T004 follows T003 (both register modules in lib.rs), T005 depends on T004 (same file), T006 depends on T001; BLOCKS all user stories
- **User Stories (Phases 3–8)**: All depend on Phase 2; stories may proceed sequentially in priority order or in parallel with coordination on shared files
- **Polish (Phase 9)**: Depends on all user stories; T024–T026 parallel; T027–T028 last

### User Story Dependencies

- **US1 (P1)**: After Phase 2 — no story dependencies (MVP)
- **US2 (P1)**: After Phase 2 — builds on the admission path US1 added (busy/timeout interplay); independently testable
- **US3 (P2)**: After Phase 2 — independent of US2 mechanisms; shares manager.rs wiring
- **US4 (P2)**: After Phase 2 — slots in front of the US1 queue in lookup order; independently testable
- **US5 (P2)**: After Phase 2 — consumes terminal outcomes from all kinds; independently testable
- **US6 (P3)**: After Phase 2 — extends the US1 queue with lanes; independently testable

### Within Each User Story

- Test task FIRST and failing, then implementation tasks in listed order
- governance.rs/manager.rs tasks within a story are sequential (same files); test-file tasks are parallel across stories

### Parallel Opportunities

- T001 ∥ T002 (different crates)
- T004 follows T003 (shared lib.rs module registration); T007 ∥ T010 ∥ T014 ∥ T017 ∥ T019 ∥ T021 (six distinct test files, all after Phase 2)
- T024 ∥ T025 ∥ T026 (distinct concerns)
- With multiple implementers: one story per implementer after Phase 2, coordinating on manager.rs/governance.rs merges

---

## Parallel Example: User Story 1 + test-first wave

```bash
# After Phase 2, launch the failing-test wave in parallel (distinct files):
Task: T007 governance_admission.rs
Task: T010 governance_retry.rs
Task: T014 governance_isolation.rs
Task: T017 governance_dedup.rs
Task: T019 governance_records.rs
Task: T021 governance_priority.rs

# Then implement story by story (same-file tasks stay sequential within a story):
Task: T008 → T009 (US1)
Task: T011 → T012 → T013 (US2)
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (Setup) and Phase 2 (Foundational)
2. Complete Phase 3 (US1: bounded admission + busy refusal)
3. **STOP and VALIDATE**: cargo test -p joey-orchestration --test governance_admission green
4. Ship/demo — the single biggest lever per the spec (bounded pool + admission)

Note: MVP is a delivery milestone, not feature completion — clarification Q2 requires all six stories and SC-001–SC-008 passing before the feature is complete.

### Incremental Delivery

1. Setup + Foundational → foundation ready
2. +US1 → validate A1 → increment shippable (MVP)
3. +US2 → validate A2; +US3 → validate A4; +US4 → validate A3; +US5 → validate A5; +US6 → validate A6
4. Polish: parity gates, event snapshots, PORTING.md, full workspace green

### Parallel Team Strategy

1. Team completes Phases 1–2 together
2. Then: implementer A → US1 then US2; B → US3; C → US4 then US5; D → US6 (after A lands the queue)
3. Polish together; T028 runs the full gate

---

## Notes

- [P] = different files, no dependency on incomplete tasks
- Same-file tasks (governance.rs, manager.rs, result_cache.rs, resource_records.rs) are sequential within their story
- Verify each story's test fails before its implementation lands
- Commit after each task or logical group; keep cargo test -p joey-orchestration green at every checkpoint
- Constitution VII: T024/T025 parity and event-snapshot tasks are release gates, not optional polish

## Phase 10: Convergence

_Appended by /speckit-converge on 2026-09-13 — gap closure between spec/plan/tasks intent and the implemented code. Ordered CRITICAL → LOW (no CRITICAL findings)._

- [X] T029 Expose additive `priority` parameter (enum critical|normal|background, default normal) in the delegate tool input schema (crates/joey-orchestration/src/delegation_tool.rs `parameters()`, lines ~373-462) and map it onto `DelegationRequest.priority` for single and batch dispatches (background waves keep forcing Background), with a contract test proving a critical dispatch from the tool surface reaches admission as the Critical lane per FR-013 (partial)
- [X] T030 Reconcile quickstart.md A3/A5 validation references: either add crates/joey-orchestration/tests/governance_retry_budget.rs (system-wide budget invariants under concurrent failures) and tests/governance_records_diag.rs (synthetic pathology-fixture distinguishing queries), or update specs/030-please-implement-features/quickstart.md A3/A5 to reference the covering tests in governance_retry.rs / governance_records.rs per quickstart A3/A5 (partial)
- [X] T031 Execute quickstart.md M1 (end-to-end feel) and M2 (manual governance-off parity) manual scenarios on a configured provider and record results in specs/030-please-implement-features/manual-validation.md per quickstart M1/M2 (missing)
- [X] T032 Move the uncommitted feature-030 working tree (13 modified + 6 untracked governance files) onto branch `030-please-implement-features` per plan.md branch decision and commit per tasks.md Notes commit cadence (contradicts)
- [ ] T033 Run workspace doc-tests (cargo test --workspace --doc), currently unexecuted because the host EDR SIGKILLs cargo-spawned test processes, once an environment permitting them is available per T028 (partial)

## Phase 11: Convergence

_Appended by /speckit-converge on 2026-09-13 (round 2) — one root-caused gap surfaced by T029/T031 verification work._

- [X] T034 Govern batch-wave and background-wave children: both transient SubagentManager constructors (crates/joey-orchestration/src/manager.rs `shared_child_manager` ~:887-916 and the dispatch_requests batch-wave literal ~:2611-2630) set `config: ManagerConfig::default()` (governance disabled) and `gov_records: None`, so batch/background children get no task timeout (FR-004), no admission-queue busy refusal (FR-002), no result-cache/single-flight (FR-007/008), and no resource records (FR-011/SC-005); fix by inheriting the parent's governance config into both transients (keep all shared pools/queue/runtime/watchdog Arcs unchanged) and sharing the records store (wrap `gov_records` in an Arc so every dispatch kind emits), with tests: batch wave appends exactly one record per child, a batch child exceeding task_timeout_secs yields a Timeout record, and a background wave appends records per FR-011 (partial)
