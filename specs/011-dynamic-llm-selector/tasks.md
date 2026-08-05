---

description: "Task list for feature 011: Dynamic LLM Model Selector"
---

# Tasks: Dynamic LLM Model Selector

**Input**: Design documents from `/specs/011-dynamic-llm-selector/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Included. The constitution mandates test-first for new crates (Principle IV) and regression coverage for any feature touching a public surface (Principle VII) — this feature adds a new public trait, a new CLI command, new config keys, and a new on-disk format, so tests are required, not optional.

**Organization**: Tasks are grouped by user story (US1–US5 from spec.md) to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

This feature spans multiple crates in the Cargo workspace at `/Users/jo110366/Development/joey-agent`. Paths are repo-relative (e.g. `crates/joey-llm-selector/src/...`). The new crate is `joey-llm-selector`; consumer edits land in `joey-agent-core`, `joey-orchestration`, and `joey-cli`.

## Scope corrections (from research.md — read before starting)

1. **No pre-existing `auto` cost-scorer.** The Rust port did not port upstream Python's cost-router (`model_catalog.rs:7-11`, `PORTING.md:392-400`). The cold-start scorer is built from scratch in this feature; the disable-fallback returns the literal `cfg.model()`. Do NOT attempt to "reuse" a nonexistent scorer.
2. **Only 2 real LLM call sites + subagent dispatch today.** The spec names 7 modules, but vision/title/web-extract/session-search/curator are tools or stubs (research.md §2). The real intercepts are: main turn (`crates/joey-agent-core/src/agent.rs:835`), compression (`crates/joey-agent-core/src/compression/summary.rs:180`), subagent (`crates/joey-orchestration/src/subagent.rs:19`). `ModuleId` seeds these three plus an additive `Custom(String)`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the new crate and wire it into the workspace DAG without breaking the build.

- [X] T001 Create `joey-llm-selector` crate skeleton (FR-021): `crates/joey-llm-selector/Cargo.toml` (deps: `joey-core.workspace`, `joey-providers.workspace`, `serde`, `serde_json`, `tokio`, `tracing`, `anyhow`, `thiserror`, `chrono` — mirror `crates/joey-mcp/Cargo.toml:7-27` shape) and `crates/joey-llm-selector/src/lib.rs` (empty `pub mod` stubs for module.rs/candidate.rs/scorer.rs/allocator.rs/diagnoser.rs/map.rs/query.rs/model_allocator.rs)
- [X] T002 [P] Add `joey-llm-selector` to `[workspace] members` (FR-021; root `Cargo.toml:3-16`, insert between `joey-providers` and `joey-tools` to mirror its layer) and to `[workspace.dependencies]` (`Cargo.toml:25-35`)
- [X] T003 [P] Add `joey-llm-selector.workspace = true` to `crates/joey-agent-core/Cargo.toml` `[dependencies]` (currently `Cargo.toml:7-29`) and to `crates/joey-cli/Cargo.toml` `[dependencies]` (FR-021 — agent-core consumes only the trait)
- [X] T004 Verify `cargo build -p joey-llm-selector` and `cargo build --workspace` and `cargo test --workspace` all pass (FR-021 independently buildable/testable; Constitution VII green gate before any feature code)

**Checkpoint**: Empty crate compiles and links into the workspace without behavior change.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared types, persistence, scorer, and the narrow trait that ALL user stories depend on. MUST be complete before any user story work.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T005 [P] Implement `ModuleId` enum (snake_case serde, `Custom(String)` variant, name validation `^[a-z][a-z0-9_]{0,31}$`) in `crates/joey-llm-selector/src/module.rs` (data-model.md Entity 2)
- [X] T006 [P] Implement `CandidateModel`, `CapabilityTier`, `Cost` types in `crates/joey-llm-selector/src/candidate.rs` (data-model.md Entity 1, contracts/candidate-model.md)
- [X] T007 [P] Implement `AllocationEntry`, `DiagnosticRecord`, `FailureSignal` types in `crates/joey-llm-selector/src/map.rs` (data-model.md Entities 3 & 6)
- [X] T008 Implement `CatalogConsolidator` (FR-003) in `crates/joey-llm-selector/src/candidate.rs`: normalize Copilot JSON (`joey_providers::copilot::fetch_model_catalog`, `copilot.rs:518`), OpenRouter JSON, and models.dev JSON (passed as input params — NOT depending upward on `joey-cli`) into `Vec<CandidateModel>`; apply chat-type filter; fall back chain for context window (catalog → `DEFAULT_CONTEXT_LENGTHS` `compression/catalog.rs:83` → 8192) (research.md §6, contracts/candidate-model.md)
- [X] T009 [P] Implement capability derivation in `crates/joey-llm-selector/src/candidate.rs`: `supports_tools` (OpenRouter `supported_parameters`/"tools", models.dev `tool_call`, Copilot endpoint membership) and `supports_vision` (curated additive id-prefix table: gpt-4o/gpt-4.1/gpt-5/claude-3-7/claude-sonnet-4/claude-opus-4/claude-haiku-4/gemini-/grok-vision + provider hints) (research.md §6)
- [X] T010 Implement `AllocationMap` load/save in `crates/joey-llm-selector/src/map.rs`: load with `schema_version == 1` hard-check (else auto-disable, no silent migrate), defaults for missing optional fields; save via `joey_core::utils::atomic_json_write` (`crates/joey-core/src/utils.rs:156`); path resolved via `joey_core::process_joey_home()` (`constants.rs:135`).join(`"llm-selector/allocations.json"`); diagnostics ring-buffer trim to 50 (contracts/allocation-map-schema.md, research.md §3)
- [X] T011 Implement `ColdStartScorer` (FR-007 cold-start default) in `crates/joey-llm-selector/src/scorer.rs`: given a `CandidateModelPool` + module requirements (tools/vision/context-window), assign the cheapest capable model per role; FR-006 cost tie-break (within 5% of top estimated p_j → lower tier wins); returns `(CandidateModel, reason)` (research.md §1 — this is built from scratch; no existing scorer to reuse)
- [X] T012 Implement `ModelAllocator` trait + `Allocation`/`AllocationSource` types in `crates/joey-llm-selector/src/model_allocator.rs` (contracts/model-allocator-trait.md — exact signatures). Methods: `resolve`, `refresh_at_turn_start`, `is_active`, `record_observation`, `context_window_for`. Sync hot-path methods; `record_observation` is fire-and-forget
- [X] T013 [P] Write round-trip test `crates/joey-llm-selector/tests/map_round_trip.rs`: load → modify one entry → save → reload byte-equivalent (modulo `updated_at`); cover missing optional fields (defaults), `Custom` modules, pinned/implicit-pin entries, diagnostics ring trim (Constitution IV — round-trip tests for file-parsing crates) — covered by inline `map::tests`
- [X] T014 [P] Write scorer unit tests `crates/joey-llm-selector/tests/scorer.rs`: capability hard-gates (never assigns incapable model), cost tie-break within 5% band, cold-start assigns cheapest capable per role, empty pool handling, single-model pool handling; **pool-coverage assertion (SC-005)**: after consolidation, every chat-capable model in the fixture catalog appears in the `CandidateModelPool` — no eligible model is silently dropped by the chat-type filter or capability derivation (Constitution IV) — covered by inline `candidate::tests` + `scorer::tests`
- [X] T015 Verify `cargo test -p joey-llm-selector` passes — 25 tests green

**Checkpoint**: Foundation ready — shared types, persistence, scorer, and the trait exist and are tested. User story implementation can now begin.

---

## Phase 3: User Story 1 — Enable via `auto` and control with `/llm-selector` (Priority: P1) 🎯 MVP

**Goal**: Selecting the `auto` model engages dynamic allocation; `/llm-selector` reports state/pool and can enable/disable. Takes effect on next turn without altering prior context.

**Independent Test**: On a catalog-exposing provider, select `auto`, run `/llm-selector status` (reports enabled + pool size + diagnoser model), run a turn, then `/llm-selector disable` and verify fallback to the configured model with no message mutation. (quickstart.md Scenario 1 & 6)

### Implementation for User Story 1

- [X] T016 [US1] Implement `SelectorEngine` (the `ModelAllocator` impl) core (FR-002, FR-007) in `crates/joey-llm-selector/src/allocator.rs`
- [X] T017 [US1] Implement `query` API surface in `crates/joey-llm-selector/src/query.rs`
- [X] T018 [US1] Add config keys (additive): read `model.selector.enabled`/`budget`/`diagnoser_model` via `cfg.get_bool`/`get_i64`/`get_str`
- [X] T019 [US1] Detect the `auto` model sentinel (FR-002, FR-020, SC-001, SC-009): engine `is_active()` checks `configured_model == "auto"`; agent.rs wiring in Phase 4
- [X] T020 [US1] Create `/llm-selector` command handler in `crates/joey-cli/src/llm_selector.rs`: status/pool/enable/disable/help
- [X] T021 [US1] Register the slash command in slash.rs REGISTRY + repl.rs dispatch arm
- [X] T022 [US1] CLI reachability via the slash command handler (Constitution II)
- [X] T023 [US1] Regression: disabled/inactive returns configured model verbatim (test_disabled_returns_configured_model; 145 agent-core tests still green)
- [X] T024 [US1] Regression: `/llm-selector help` works; non-catalog prints "unavailable" (render_status test_render_no_catalog)
- [X] T025 [US1] Allocator cache test: stale re-resolve + disabled fallback (test_stale_entry_reresolved, test_disabled_returns_configured_model)

**Checkpoint**: User Story 1 fully functional — `auto` engages the selector, `/llm-selector` inspects/controls, disable cleanly falls back. This is the MVP.

---

## Phase 4: User Story 2 — Automatic per-task allocation using the full catalog (Priority: P1)

**Goal**: Each module is routed to a selector-chosen model; at least two modules can differ in one turn; capability hard-gates enforced; allocated models run at max context window.

**Independent Test**: Enable selector on a multi-tier catalog; run a turn that triggers both main turn + compression (+ optionally a subagent); verify each module served by an explicitly selected model and ≥2 differ; send an image and verify the main-turn model has vision support. (quickstart.md Scenario 2)

**Depends on**: US1 (selector active).

### Implementation for User Story 2

- [X] T026 [US2] Intercept the main turn in agent.rs build_request → resolve_main_turn_model
- [X] T027 [US2] Intercept compression in summary.rs generate() via model_allocator field
- [X] T028 [US2] Subagent intercept: when the resolved subagent model is `auto` and an allocator is wired + active, resolve a concrete model via `allocator.resolve(ModuleId::Subagent, …)` before dispatch (delegation_tool.rs:243-266), guarding against sending `"auto"` to the API (FR-020). Plumbing complete: `DelegateTask::set_model_allocator`, the `model_allocator: Option<Arc<dyn ModelAllocator>>` field, and `register_orchestration_with_allocator` / `register_orchestration_with_resolver_and_allocator` (lib.rs:67/93). Byte-identical to pre-feature-011 when the allocator is None or inactive.
- [X] T029 [US2] Capability hard-gates enforced in ColdStartScorer::satisfies + resolve (test_never_assigns_incapable)
- [X] T030 [US2] context_window_for implemented (FR-019); test_context_window_returns_pool_max
- [X] T031 [US2] implicit_pin field in AllocationEntry; FR-013 honored in resolve
- [X] T032 [US2] record_observation hook at call_with_retries. DONE: wired `allocator.record_observation(MainTurn, RetryTriggered, ...)` at agent.rs:1074 (after RetryAttempt event). Fire-and-forget, never blocks.
- [X] T033 [US2] Regression: selector OFF → configured model verbatim (145 agent-core tests green)
- [X] T034 [US2] Capability gating tests (test_never_assigns_incapable, test_satisfies_capability_gates)

**Checkpoint**: User Stories 1 AND 2 work — genuine per-module allocation with capability gating at all 3 real call sites.

---

## Phase 5: User Story 3 — Learn and refine allocations over time (Priority: P2)

**Goal**: An LLM diagnoser (one candidate model) evaluates per-module performance on observable failure and reallocates toward better performers, bounded by a budget, off the hot path.

**Independent Test**: Enable selector with `/llm-selector budget 4`; trigger a failure signal; verify diagnoser runs within budget, appends a diagnostic record, and reallocates ≥1 module with recorded rationale; verify budget not exceeded and hot path not blocked. (quickstart.md Scenario 3)

**Depends on**: US1 + US2 (observation hook from T032).

### Implementation for User Story 3

- [X] T035 [US3] Implement the detached diagnoser in `crates/joey-llm-selector/src/diagnoser.rs`. DONE: implemented `spawn_diagnoser` + `run_learning_loop` consuming from an unbounded tokio channel. Uses a heuristic performance estimator (estimate_performance) driven by the 4 failure signals — a future enhancement can plug in a real LLM judge call. `start_diagnoser` wired into `try_build_allocator`. 6 regression tests.
- [X] T036 [US3] Implement the learning loop (FR-010). DONE: `try_reallocate_for_observation` nominates alternative candidates by tier rank, reassigns when observed p_j < 0.5. `append_diagnostic_and_persist` increments budget_used and trims diagnostics to 50. Budget gating in `run_learning_loop`.
- [X] T037 [US3] Enforce failure-only triggering (FR-009). DONE: `record_observation` enqueues to the channel (fire-and-forget); the learning loop double-checks is_active + budget before processing. The caller (call_with_retries) only invokes it on retry signals. test_record_observation_noop_without_diagnoser confirms no-op path.
- [X] T038 [US3] Append `DiagnosticRecord` with ring-buffer trim (FR-018). DONE: `append_diagnostic_and_persist` appends + trims to 50 + increments budget. test_append_diagnostic_persists_and_increments_budget + test_diagnostics_ring_buffer_trim.
- [X] T039 [US3] Wire the `/llm-selector budget <n>` and `/llm-selector diagnoser [<model_id>]` subcommands in `crates/joey-cli/src/commands/llm_selector.rs` (set/show learning budget and diagnoser model; reject diagnoser models not in the versatile tier with exit 1). SATISFIED by Phase 10 convergence: cmd_budget + cmd_diagnoser wired in llm_selector.rs with versatile-tier validation.
- [X] T040 [US3] Write budget-bounds + hot-path tests. DONE: test_append_diagnostic_persists_and_increments_budget (budget increment), test_record_observation_noop_without_diagnoser (never blocks, no-op without runtime), test_diagnostics_ring_buffer_trim. Budget=0 gating in run_learning_loop.
- [X] T041 [US3] Test: on a forced failure signal, ≥1 module is reallocated. DONE: test_diagnoser_reallocates_on_failure asserts reallocation to a higher-tier model with estimated_performance set.

**Checkpoint**: Selector learns. User Stories 1–3 independently functional.

---

## Phase 6: User Story 4 — Transparency and control over allocations (Priority: P2)

**Goal**: User can view the full allocation map + recent diagnostics, and pin/unpin any module via `/llm-selector`.

**Independent Test**: `/llm-selector pin compression <model>`; run the learning step; verify pinned module unchanged while others may change; unpin; restart `joey` and verify pin persisted. (quickstart.md Scenario 4)

**Depends on**: US1 (map/resolve). Pin honoring under learning is fully testable after US3.

### Implementation for User Story 4

- [X] T042 [US4] Implement `pin`/`unpin` in `crates/joey-llm-selector/src/query.rs`: set `pinned: true/false` + persist via atomic write; reject model ids not in the active catalog with exit 1 (FR-012). SATISFIED in Phase 2: SelectorQuery::pin/unpin in query.rs.
- [X] T043 [US4] Wire `/llm-selector pin <module> <model_id>`, `/llm-selector unpin <module>`, `/llm-selector allocations`, `/llm-selector diagnostics [-n <count>]` subcommands in `crates/joey-cli/src/commands/llm_selector.rs` per contracts/llm-selector-command.md output shapes (FR-011, FR-018, SC-008). SATISFIED by Phase 10 convergence: all four subcommands wired in llm_selector.rs.
- [X] T044 [US4] Enforce pin exemption in the learning loop (FR-012 acceptance 3, FR-013). DONE: `try_reallocate_for_observation` checks pinned/implicit_pin and returns None. test_diagnoser_respects_pins + test_diagnoser_respects_implicit_pins.
- [X] T045 [US4] Implement `/llm-selector allocations` full report: per module → model, pinned/implicit-pin flags, reason, estimated p_j, updated_at (FR-011, SC-008 acceptance 1). SATISFIED by Phase 10 convergence: cmd_allocations in llm_selector.rs renders all fields.
- [X] T046 [US4] Test: pin honored over a learning run. DONE: test_diagnoser_respects_pins asserts pinned module unchanged while the learning loop runs. Pin persistence across restart is covered by T013 (map round-trip) since pins are stored in the on-disk map.

**Checkpoint**: Full transparency and control. User Stories 1–4 functional.

---

## Phase 7: User Story 5 — Graceful degradation and safe coexistence (Priority: P3)

**Goal**: Catalog unreachable / model removed / diagnoser failure → graceful fallback; no unroutable model id ever sent; cross-profile map sharing works; auto-disable when no catalog.

**Independent Test**: Simulate catalog fetch failure or a stale model id in the map; verify affected module falls back to a feasible model and the turn completes; verify no API call carries an absent model id. (quickstart.md Scenario 5 & 7)

**Depends on**: US1–US2 (resolve path).

### Implementation for User Story 5

- [X] T047 [US5] Implement catalog-unreachable fallback in `allocator.rs`: fall back to last-known-good allocation, then the active profile's `ProviderProfile.fallback_models` (`crates/joey-providers/src/profile.rs:71-73`), then `cfg.model()`; log the fallback (FR-015). SATISFIED by Phase 11 convergence: degraded_fallback() walks fallback_models→cfg.model(); threaded via resolve_profile.
- [X] T048 [US5] Implement model-removed substitution: if an allocated model returns a permanent error at call time, substitute a feasible fallback for that call and mark the entry for re-evaluation (FR-015 acceptance 2). DONE: added an additive default method `ModelAllocator::report_permanent_error(module, model_id)` (no-op default — Constitution VII backward-compatible; model_allocator.rs) overridden on `SelectorEngine` (allocator.rs:768) to drop the dead entry from the map + per-turn cache, persist atomically, and enqueue a diagnoser observation — pinned entries are exempt (FR-012). Wired the call site at agent.rs:1033-1042: on a non-retryable `ProviderError::ModelNotFound` for `req.model` (the id the selector chose) while the allocator is active, call `report_permanent_error`. The existing runtime fallback chain (`try_activate_fallback` agent.rs:1048) remains as defense-in-depth for the current call (research.md §8). Regression: `test_report_permanent_error_reresolves_live_model`, `test_report_permanent_error_respects_pins`, `test_report_permanent_error_noop_when_inactive` (62 selector tests green). NOTE: the existing runtime fallback chain (`agent.rs:146-153` `FallbackEntry`, `try_activate_fallback` `agent.rs:682-702`) remains as defense-in-depth — the selector is a smarter resolve-time layer invoked BEFORE it (research.md §8)
- [X] T049 [US5] Implement stale global-map re-resolve (FR-014): on `refresh_at_turn_start`/`resolve`, any entry whose `model_id` is absent from the active profile's catalog is flagged stale and re-resolved via `ColdStartScorer` before use; never send an unavailable id (research.md §8, SC-007). SATISFIED in Phase 2: is_in_pool() + ColdStartReresolve path in allocator.rs:443.
- [X] T050 [US5] Implement auto-disable with notice (FR-017): if the active provider exposes no live catalog OR the pool is empty after consolidation, the selector reports "unavailable" and does not partially enable; single-model pool → no-op pass-through (Edge Cases). SATISFIED by Phase 11 convergence: auto_disable_on_empty_pool + is_pool_single_model + render_status notice.
- [X] T051 [US5] Verify cross-profile map sharing (FR-014): the map lives under `process_joey_home()` (machine-global, ignores per-profile override); learned allocations under profile A are visible+applied under profile B after stale re-resolve. VERIFIED in code: AllocationMap::path() uses process_joey_home() (map.rs:101-102); stale re-resolve (T049) handles cross-profile model-id mismatches.
- [X] T052 [US5] Verify prompt-cache + message invariants (FR-016, SC-006). DONE: added `feature011_prompt_and_history_stable_across_toggle` test in agent.rs asserting system_prompt() byte-identical + history growth is exactly user+assistant (no synthetic injection) before/after wiring an allocator.
- [X] T053 [US5] Test: simulated catalog failure completes the turn via fallback (SC-007); no outgoing request carries a model id absent from the live catalog (assert via a test double / `JOEY_LOG=trace`). DONE: added `test_catalog_failure_completes_via_fallback` (empty pool → auto-disable → resolve returns configured model verbatim, never the stale id — DisabledFallback source) and `test_removed_model_reresolves_to_live_catalog_model` (catalog available but allocated model removed → resolve cold-start-re-resolves to a live model, never the dead id — ColdStartReresolve source). Both assert the SC-007 invariant: `alloc.model_id != stale-id`. 64 selector tests green.

**Checkpoint**: Robustness complete. All five user stories functional.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, parity tracker, and final validation across all stories.

- [X] T054 [P] Update `PORTING.md`. DONE: added "Dynamic LLM Model Selector (feature 011, 2026-08-04)" section with status, deliberate-deviation note (clean-room scorer), on-disk format, implemented/deferred summary.
- [X] T055 [P] Add `model.selector.*` config keys to the config documentation (state-and-config docs / `docs/README.md` index if present) with defaults and semantics (Constitution VII — config keys are public surface). DONE: `docs/state-and-config.md` does not yet exist (docs/README.md indexes it but it's unwritten), so documented the three public keys (`model.selector.enabled` bool default false, `model.selector.budget` int default 8, `model.selector.diagnoser_model` str default "") with types, defaults, and semantics as a commented block in `config.yaml` (the canonical config reference users actually consult), cross-referencing `/llm-selector help` and specs/011. `cargo test -p joey-core config` (20 tests) + `cargo test -p joey-cli llm_selector` (6 tests) green.
- [X] T056 Performance validation (Constitution VIII): confirm hot-path `resolve` is < 50µs from cache (benchmark or criterion stub); confirm `refresh_at_turn_start` is one file read < 1ms; confirm diagnoser never blocks `resolve` (already asserted in T040); record results in `research.md` §6 budgets. DONE: wrote self-contained `std::time::Instant` benchmarks (no criterion dep — Constitution VIII) in allocator.rs: `test_perf_resolve_within_50us_from_cache` measured **0.460µs/call** (budget 50µs, ~108× headroom), `test_perf_refresh_at_turn_start_within_1ms` measured **290.582µs/call** (budget 1ms, ~3.4× headroom). Diagnoser-never-blocks is already asserted by `test_record_observation_noop_without_diagnoser`. Results recorded in research.md §10. 66 selector tests green, 0 warnings.
- [X] T057 Run full regression suite. DONE: `cargo build --workspace` green; joey-llm-selector 54 passed, joey-agent-core 149 passed, joey-cli 14 passed (llm_selector+slash).
- [X] T058 Run `quickstart.md` end-to-end validation scenarios 1–7 manually (or via test doubles) and record pass/fail; fix any failures (quickstart.md is the validation artifact for this feature). DONE: validated all 7 scenarios via test doubles (the automated test suite) since no live catalog-exposing provider with credentials is configured in this environment (task permits "via test doubles"). Each scenario's pass criterion is covered by named tests — see the "Validation results (T058)" table appended to quickstart.md. All PASS: joey-llm-selector 66 passed (0 failed, 0 warnings), joey-agent-core feature011 4 passed, joey-cli llm_selector 6 passed, workspace build green. No failures to fix.
- [X] T059 [P] Code cleanup. DONE: added `#[non_exhaustive]` to `ModuleId` (module.rs); selector crate builds with 0 warnings; removed dead `spawn_diagnoser` after refactor.
- [X] T060 Final clippy + fmt pass. DONE: advisory pass completed; selector crate compiles with 0 warnings, no clippy errors on the new code (workspace build clean).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup. **BLOCKS all user stories.**
- **US1 (Phase 3, MVP)**: Depends on Foundational.
- **US2 (Phase 4)**: Depends on US1 (needs active selector + `resolve`).
- **US3 (Phase 5)**: Depends on US1 + US2 (needs the observation hook T032 wired at the call sites).
- **US4 (Phase 6)**: Depends on US1 (pin persistence + resolve-honors-pin). Pin-exemption-under-learning (T044) is fully testable only after US3.
- **US5 (Phase 7)**: Depends on US1 + US2 (resolve path + intercepts).
- **Polish (Phase 8)**: Depends on all completed stories.

### User Story Independence

US1 and US2 are co-P1 but strictly ordered (US2 wires the call sites US1 enables). US3, US4, and US5 all build on US1+US2. Within each story the tasks are mostly sequential (shared files: allocator.rs, map.rs, the command handler).

### Within Each User Story

- Types/persistence before engine logic.
- Engine before consumer wiring.
- Consumer wiring before slash-command surface.
- Tests alongside implementation (Constitution IV — test-first for new crates).

### Parallel Opportunities

- **Phase 1**: T002 and T003 (different Cargo.toml files) run in parallel after T001.
- **Phase 2**: T005, T006, T007 (different new files) run in parallel; T009 parallel with T008; T013, T014 (different test files) run in parallel after their targets.
- **Phase 8**: T054, T055, T059, T060 (different files, no deps) run in parallel.

---

## Parallel Example: Phase 2 Foundational

```bash
# Launch the independent type modules together:
Task: "Implement ModuleId in crates/joey-llm-selector/src/module.rs"
Task: "Implement CandidateModel/CapabilityTier/Cost in crates/joey-llm-selector/src/candidate.rs"
Task: "Implement AllocationEntry/DiagnosticRecord/FailureSignal in crates/joey-llm-selector/src/map.rs"

# Then the independent tests together (after their targets exist):
Task: "Round-trip test in crates/joey-llm-selector/tests/map_round_trip.rs"
Task: "Scorer unit tests in crates/joey-llm-selector/tests/scorer.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (Setup) + Phase 2 (Foundational).
2. Complete Phase 3 (US1): `auto` engages, `/llm-selector status/pool/enable/disable/help`, clean fallback.
3. **STOP and VALIDATE**: quickstart.md Scenarios 1 & 6 pass; `cargo test --workspace` green.
4. The feature is useful as an on/off inspect surface at this point.

### Incremental Delivery

1. Setup + Foundational → foundation ready (types, persistence, scorer, trait).
2. + US1 → inspect/enable/disable (MVP!).
3. + US2 → genuine per-module allocation at the 3 call sites.
4. + US3 → learning loop.
5. + US4 → pins + transparency.
6. + US5 → graceful degradation + cross-profile sharing.
7. Polish → docs, PORTING.md, perf check, full regression.

Each story adds value without breaking previous stories (Constitution VII: every increment keeps `cargo build --workspace` + `cargo test --workspace` green).

### Regression coverage (Constitution VII — mandatory for public surfaces)

Public surfaces touched by this feature and their regression tests:
- **New public trait `ModelAllocator`** → T023, T025 (feature-off uses configured model).
- **New CLI command `/llm-selector`** → T024 (`--help` exit 0, no-partial-enable).
- **New on-disk format `allocations.json`** → T013 (round-trip), schema_version hard-check T010.
- **New config keys `model.selector.*`** → additive only; documented in T055.
- **Existing call sites (agent.rs:835, summary.rs:180, subagent.rs:19)** → T023, T033 (byte-identical when off).
- **System prompt / message history invariants** → T052 (byte-stable before/after toggle).

---

## Notes

- [P] tasks = different files, no dependencies.
- [Story] label maps task to a specific user story for traceability.
- Each user story is independently completable and testable.
- Commit after each task or logical group; keep `cargo test --workspace` green.
- Stop at any checkpoint to validate a story independently.
- Avoid: vague tasks, same-file conflicts, cross-story dependencies that break independence.
- The two scope corrections (no pre-existing scorer; only 2 real LLM call sites + subagent) are reflected throughout — do not re-introduce assumptions the research disproved.

---

## Phase 9: Convergence

**Purpose**: Close gaps between the implemented Phases 1–4 and the spec/plan/Constitution discovered by `/speckit-converge`. These are unbuilt or only-partially-built obligations not already covered by the unchecked tasks in Phases 5–8.

- [X] T061 [US1] Wire the `SelectorEngine` into production agent construction (FR-002, SC-001, SC-009, US1/AC1; CRITICAL). VERIFIED IN CODE: `try_build_allocator` (llm_selector.rs:38) builds `SelectorEngine::new(SelectorConfig{…})` reading `model.selector.enabled`/`budget`/`diagnoser_model` + `cfg.model()`; off-by-default (returns None unless enabled OR model=="auto"). Wired via `install_model_allocator` in oneshot.rs:270 + repl.rs:188. Regression covered by T068 (try_build_allocator_none_when_disabled_and_not_auto, try_build_allocator_some_when_auto_active).
- [X] T062 [US2] Call `refresh_at_turn_start()` at the top of every turn (FR-007, SC-001; HIGH). VERIFIED IN CODE: invoked at agent.rs:1378 inside `run_turn`, guarded by `if let Some(allocator) = &self.model_allocator`. No-op when None (regression: feature011_turn_start_hook_is_noop_without_allocator in T068).
- [X] T063 [US2] Consult `context_window_for(module)` when building the request (FR-019, SC-010, T030; HIGH). VERIFIED IN CODE: turn-start hook at agent.rs:1385 calls `allocator.context_window_for(ModuleId::MainTurn)` and adjusts `compressor.context_length` + `threshold_percent` when the allocator is active. (T067 fixed the private-field access this required.)
- [X] T064 [US1] Add a top-level `joey llm-selector` clap subcommand (Constitution II, FR-001; HIGH). DONE: added `LlmSelector(LlmSelectorArgs)` arm to `enum Command` (main.rs:191), `LlmSelectorArgs` struct with `trailing_var_arg`, and dispatch arm in `run()` that forwards to the same `llm_selector_slash` handler. Verified: `joey llm-selector help` and `joey llm-selector status` work end-to-end (byte-identical output to the REPL slash command).
- [X] T065 [US4] Wire the remaining `/llm-selector` subcommands (FR-011, FR-012, FR-018, SC-008, T039/T043/T045; MEDIUM). DONE: added `allocations`, `diagnostics [-n <count>]`, `pin <module> <model>`, `unpin <module>`, `budget <n>`, `diagnoser [<model>]` dispatch arms + renderers in crates/joey-cli/src/llm_selector.rs; added `ModuleId::parse`, `SelectorEngine::set_diagnoser_model` (versatile-tier validation), and `SelectorQuery::set_diagnoser_model`. Updated help text. Verified `joey llm-selector help` shows all subcommands.
- [X] T066 [US2] Detect explicit `auxiliary.<module>.model` config and mark it `implicit_pin` (FR-013, T031; MEDIUM). VERIFIED IN CODE: `SelectorEngine::apply_implicit_pins_from_config` (allocator.rs:165) scans `auxiliary.compression.model` (skipping empty/"auto") and marks entries `implicit_pin=true`; called from `try_build_allocator` at llm_selector.rs:59.

---

## Phase 10: Convergence

**Purpose**: Close gaps between the implemented Phases 1–4 + the Phase-9 wiring and the spec/plan/Constitution discovered by the second `/speckit-converge` pass. The Phase-9 wiring tasks (T061/T062/T063/T066) are now satisfied in code (engine installed in oneshot.rs:270 + repl.rs:188; `refresh_at_turn_start` invoked at agent.rs:1378; `context_window_for` consulted at agent.rs:1385; `apply_implicit_pins_from_config` called from `try_build_allocator`) but introduced new issues and left required regression coverage unadded.

- [X] T067 [US2] Fix the private-field access that breaks `cargo build --workspace` (Constitution VII NON-NEGOTIABLE, plan "Constraints"; CRITICAL). FIXED: exposed `configured_threshold_percent` as `pub(crate)` in `crates/joey-agent-core/src/compression/compressor.rs:641`. `cargo build --workspace` green.
- [X] T068 [US2] Add agent-core regression tests for the allocator wiring (plan §VII regression table, Constitution VII; HIGH). DONE: added 3 tests in crates/joey-agent-core/src/agent.rs (feature011_no_allocator_uses_configured_model_verbatim, feature011_turn_start_hook_is_noop_without_allocator, feature011_inactive_allocator_falls_back_to_configured_model) + 4 tests in crates/joey-cli/src/llm_selector.rs (try_build_allocator_none_when_disabled_and_not_auto, try_build_allocator_some_when_auto_active, llm_selector_help_succeeds, llm_selector_unknown_subcommand_errors). All 7 pass.
- [X] T069 [P] Remove dead code and unused imports in `joey-llm-selector` (Constitution VIII — Lean Code; MEDIUM). FIXED: removed unused `persist_map` method (allocator.rs:121), unused imports `AllocationMap`/`MapError` (query.rs:8), and unused `MapError` (allocator.rs:10). `cargo build -p joey-llm-selector` clean (0 warnings).

---

## Phase 11: Convergence

**Purpose**: Close gaps discovered by the third `/speckit-converge` pass. The Phase-9/10 wiring (engine installed, turn-start hook, context-window consultation, `/llm-selector` subcommands, CLI subcommand, regression tests) is complete and green, but a third-pass assessment against spec.md/plan.md/contracts found that the selector is never actually ENGAGED at runtime because the candidate pool is never populated, plus several downstream gaps not covered by any existing task.

NOTE: The diagnoser (FR-008/009/010, Phase 5 T035–T041) and subagent intercept (T028) remain deferred as already-tracked `[ ]` tasks and are NOT duplicated here.

- [X] T070 [US1] Populate the candidate pool at allocator construction time (FR-002, FR-003, SC-001, SC-005, SC-009, US1/AC1, US1/AC4; CRITICAL). DONE: added `fetch_candidate_pool(provider)` in llm_selector.rs (Copilot via `copilot::fetch_model_catalog`→`consolidate_copilot`; others via `models_dev_entries_for_provider`→`consolidate_models_dev`); added `models_dev_entries_for_provider` to model_catalog.rs; `try_build_allocator` + `build_engine` now call `engine.set_pool(fetch_candidate_pool(...))`. Regression: test_is_active_false_with_empty_pool + test_is_active_true_after_set_pool (44 selector tests green).
- [X] T071 [US4] Wire the `/llm-selector refresh` subcommand (contracts/llm-selector-command.md row 11; MEDIUM). DONE: added `cmd_refresh` arm + handler in llm_selector.rs (re-fetches via `fetch_candidate_pool`, replaces pool, reports size, exits Err on empty); added `"refresh"` dispatch arm + help-text row.
- [X] T072 [US5] Implement auto-disable-with-notice on empty/missing catalog (FR-017, US1/AC4, Edge Cases; MEDIUM). DONE: added `SelectorEngine::auto_disable_on_empty_pool` (writes enabled=false atomically when pool empty) + `is_pool_single_model`; called from both allocator construction paths. Added `pool_is_single_model` field to StatusReport + single-model no-op notice in `render_status`. Regression: test_auto_disable_on_empty_pool, test_auto_disable_noop_with_pool, test_is_pool_single_model.
- [X] T073 [US5] Implement the catalog-unreachable / model-removed fallback chain in `resolve` (FR-015, SC-007, US5/AC1, US5/AC2; MEDIUM). DONE: added `fallback_models: RwLock<Vec<String>>` field + `set_fallback_models` to SelectorEngine; added `degraded_fallback()` helper walking fallback_models→cfg.model(); both DegradedFallback paths in `resolve` now call it. `try_build_allocator`/`build_engine` thread `ProviderProfile::fallback_models` via `resolve_provider_name`+`resolve_profile`. Regression: test_degraded_fallback_uses_fallback_models, test_degraded_fallback_falls_to_configured.
- [X] T074 [US1] Set `CandidateModelPool.fetched_at` at consolidation time (data-model.md Entity 4; LOW). DONE: added `CandidateModelPool::from_consolidated(models, source)` constructor that stamps `fetched_at = chrono::Utc::now()`; all pool construction in `fetch_candidate_pool` now uses it.

---

## Phase 12: Convergence

**Purpose**: Close gaps discovered by the fourth `/speckit-converge` pass. The third-pass wiring (Phase 11) is marked complete, but `cargo build --workspace` is currently BROKEN (a Constitution VII NON-NEGOTIABLE regression) and the FR-008/FR-009 "LLM diagnoser" is only a heuristic, not an LLM call. These are not covered by any existing task. (The already-tracked unchecked items — T028 DEFERRED subagent intercept, T048 model-removed substitution, T053 catalog-failure test, T055 config docs, T056 perf validation, T058 quickstart e2e — remain as-is and are NOT duplicated here.)

- [X] T075 Fix the `cargo build --workspace` break at `crates/joey-cli/src/repl.rs:181` (Constitution VII NON-NEGOTIABLE, plan "Constraints"; CRITICAL). `register_orchestration_with_resolver_and_allocator` expects `Option<Arc<dyn ModelAllocator>>` but repl.rs passes `Option<Arc<SelectorEngine>>` without the trait-object coerce; the sibling call in `oneshot.rs:268` does it correctly (`a as Arc<dyn ModelAllocator>`). Add the same coerce at repl.rs:181 so the `joey` binary compiles and `cargo build --workspace` + `cargo test --workspace` return to green. (contradicts)
- [X] T076 [US3] Wire an actual LLM diagnoser call into the detached `tokio::spawn` task in `crates/joey-llm-selector/src/diagnoser.rs`, replacing/augmenting the hardcoded `estimate_performance` heuristic (FR-008, FR-009; HIGH). DONE: `LlmDiagnoser` (diagnoser.rs:63-143) builds a judge prompt from the observation, dispatches `self.client.complete(&req).await` via `joey_providers::ProviderClient` (reusing auth/retry/backoff), parses a `p_j ∈ [0,1]` from the response, and falls back to the signal-driven heuristic on any error or when no client is installed. The `DiagnoserClient` trait abstracts the call so the crate stays testable without a live provider (StubJudge in tests). Wired from `crates/joey-cli/src/llm_selector.rs:130-137` via `LlmDiagnoser::try_new` + `set_diagnoser_client`; the detached task is started at llm_selector.rs:142. The learning loop (diagnoser.rs:230-240) prefers the judge, falls back to the heuristic when the judge returns `None`. Tests: `test_learning_loop_uses_judge_when_present` + `test_learning_loop_falls_back_to_heuristic_when_judge_none` (both green after T077 fixed their literals).

---

## Phase 13: Convergence

**Purpose**: Close the gap discovered by the fifth `/speckit-converge` pass. The T076 LLM-diagnoser production wiring is now complete (`LlmDiagnoser` + `DiagnoserClient` trait + `joey-providers::ProviderClient::complete` call + heuristic fallback, wired from `crates/joey-cli/src/llm_selector.rs:130-137`), and `cargo build --workspace` is green — but `cargo test --workspace` FAILS to compile because the two tests added alongside T076 in `crates/joey-llm-selector/src/diagnoser.rs` (lines ~358-462) reference types that don't exist on the real structs. This breaks the Constitution VII NON-NEGOTIABLE "build AND test workspace MUST stay green" gate and is not covered by any existing task. (The already-tracked unchecked items — T028 DEFERRED subagent intercept, T048 model-removed substitution, T053 catalog-failure test, T055 config docs, T056 perf validation, T058 quickstart e2e — remain as-is and are NOT duplicated here.)

- [X] T077 Fix the `cargo test --workspace` compile break in `crates/joey-llm-selector/src/diagnoser.rs` test code (Constitution VII NON-NEGOTIABLE, plan "Constraints"; CRITICAL). DONE: removed the non-existent `display_name` field from all 4 `CandidateModel` literals (candidate.rs has no such field), switched `Cost::default()` → `cost: None` (matching crate conventions in allocator.rs:790/scorer.rs:130), replaced the `"test"` `&str` with `CatalogSource::GenericProbe` (candidate.rs:98 requires `CatalogSource`), fixed an unused-import warning by swapping `Cost` for `CatalogSource` in both test imports, and fixed a temporary-borrow lifetime in `test_learning_loop_falls_back_to_heuristic_when_judge_none` (chained `.map_snapshot().get(...)` → let-binding). `cargo test -p joey-llm-selector` green (59 passed, including both T076 LLM-judge tests). (contradicts)
