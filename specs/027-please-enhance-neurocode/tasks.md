---
description: "Task list for feature 027: NeuroCode Adaptive Memory"
---

# Tasks: NeuroCode Adaptive Memory

**Input**: Design documents from `/specs/027-please-enhance-neurocode/`

**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included throughout — constitution v1.1.0 Principles IV and VII mandate tests alongside implementation and regression coverage for public-surface changes (schema, config keys, command surface).

**Organization**: Tasks grouped by user story (US1 adaptive code, US2 episodic memory, US3 hypercode integration, US4 transparency/control) for independent implementation and testing.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1–US4)
- Exact file paths in every task

## Path Conventions

Rust workspace: crates under `crates/`, integration tests under `crates/<crate>/tests/`. All paths below are repo-relative.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Config contract for the whole feature (all later tasks read these keys)

- [x] T001 [P] Add the five `neurocode.memory.*` key specs (enabled=false, top_k=5, injection_char_limit=2048, max_episodes=500, distill_model="") and `MemoryConfig::load(&Config)` with documented clamps to crates/joey-neurocode-rag/src/config.rs, following the `RAG_CONFIG_KEYS` / `RagConfig::load` pattern (contract: contracts/neurocode-memory-config-keys.md)
- [x] T002 [P] Config contract tests (defaults, clamps, absent-key behavior, additivity) in crates/joey-neurocode-rag/tests/memory_config.rs

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Storage, retrieval leg, and distillation core that EVERY user story depends on

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [x] T003 [P] Bump `NEUROCODE_SCHEMA_VERSION` 3 → 4 in crates/joey-neurocode/src/lib.rs and append the additive-idempotent v4 batch (memory_episodes, memory_preferences, memory_vectors + 3 indexes) to `apply_schema` in crates/joey-neurocode/src/graph/store.rs, exactly per contracts/neurocode-memory-storage.md DDL
- [x] T004 [P] `MemoryEpisode` + `EpisodeStore` (insert enforcing the store-level sanitization choke point — secret redaction + 4 KB field caps on every insert so all writers inherit it [analyze U1], get, list_recent, delete with vector cascade in one transaction, FIFO eviction beyond max_episodes) in crates/joey-neurocode/src/memory/episodes.rs + register `pub mod episodes;` in crates/joey-neurocode/src/memory/mod.rs (fields per data-model.md)
- [x] T005 `MemoryPreference` + `PreferenceStore` (upsert/strengthen enforcing the same sanitization choke point — redaction + 1 KB statement cap [analyze U1], deterministic supersede rank explicit>inferred then recency, resolve_active, delete with vector cascade) in crates/joey-neurocode/src/memory/preferences.rs + register `pub mod preferences;` in crates/joey-neurocode/src/memory/mod.rs (depends T004: same mod.rs)
- [x] T006 [P] Memory retrieval leg in crates/joey-neurocode-rag/src/memory_search.rs: reuse `EmbeddingBackend` resolve, quantize encode (f32/int8, byte-identical BLOB to rag_vectors), `dense_scan`, `rrf_fuse` (dense + keyword legs); synthetic ids `memory://episode/<id>`, `memory://preference/<id>`; zero coupling to rag_* tables (contract: contracts/neurocode-memory-injection.md)
- [x] T007 `MemoryDistiller` trait + heuristic explicit-statement detector (I prefer/always/never patterns) + recurrence strengthening (same category + cosine ≥ 0.92 → append evidence, bump confidence) in crates/joey-neurocode/src/memory/distill.rs + mod.rs registration (depends T004, T005)
- [x] T008 Schema migration + store tests in crates/joey-neurocode/tests/memory_schema_migration.rs and crates/joey-neurocode/tests/memory_stores.rs: hand-crafted v3 DB opens with data intact + gains empty memory tables, version reads 4 exactly once, double-open idempotent, vector BLOB round-trip byte-exact, delete cascades, FIFO eviction, insert-path sanitization verified at the store choke point (redaction + caps, analyze U1) (mirrors tests/rag_schema_migration.rs) (depends T003–T005)
- [x] T009 Retrieval-leg tests in crates/joey-neurocode-rag/tests/memory_search.rs: dense+keyword fusion ranking, namespace isolation (memory deletes never touch rag_* and vice versa), two-project-root isolation — distinct roots resolve to distinct stores with zero cross-project memory reads (FR-012, analyze C1), degraded-backend fallback to keyword-only (depends T006)

**Checkpoint**: Foundation ready — stores, retrieval, and distillation compile and test green (`cargo test -p joey-neurocode -p joey-neurocode-rag`)

---

## Phase 3: User Story 1 - Code That Adapts to the User (Priority: P1) 🎯 MVP

**Goal**: Stated/inferred preferences shape code output without restating them (FR-002, FR-003, FR-004; SC-001)

**Independent Test**: State "always use constructor injection", complete a task, ask for new code in a fresh session — it conforms; injected block visible via memory status

### Implementation for User Story 1

- [x] T010 [P] [US1] Memory injection block in crates/joey-agent-core/src/agent.rs mirroring the `apply_rag_prefetch` precedent: top_k active preferences (explicit first, then recency) + relevant episodes, hard char cap with whole-entry truncation, silent omission on retrieval error, no-op when disabled (contract: contracts/neurocode-memory-injection.md)
- [x] T011 [US1] Post-turn capture at the three `run_turn` exit paths (the `neurocode_auto_reindex` call-sites) in crates/joey-agent-core/src/agent.rs: synchronous explicit-preference detection (origin=explicit, no gate — clarified Q1) + immediate per-episode distillation spawn (continuous — clarified Q3), off the response critical path (depends T010: same file)
- [x] T012 [US1] Provider-backed `MemoryDistiller` implementation wired in crates/joey-cli/src/neurocode_wiring.rs behind the default-off gate (economical tier, `distill_model` override), passed to the agent via the memory API introduced in T010 (sequenced after T010, not parallel — analyze I2)
- [x] T013 [P] [US1] Injection + capture tests in crates/joey-agent-core/tests/memory_injection.rs: default-off byte-identical no-op regression, block content/ordering, char-cap truncation, error-omission, explicit capture without approval gate, injected-block size/formatting assertion as the latency proxy for the ≤ 2s budget (analyze A2)

**Checkpoint**: US1 independently functional — preferences learned and applied

---

## Phase 4: User Story 2 - Remembering Past Work Episodes (Priority: P1)

**Goal**: Completed tasks become episodes; related work recalls them (FR-001; SC-002, SC-005)

**Independent Test**: Complete a task whose approach failed; start a related task — the failed approach is recalled and avoided

### Implementation for User Story 2

- [x] T014 [US2] Episode assembly + capture in crates/joey-agent-core/src/agent.rs at the T011 call-sites: one episode per completed task (Q2), fields per data-model.md (title/task/context/approach/outcome/lessons), joey-core secret redaction before persist (FR-011 — skip if redacted text empty), 4 KB field caps (depends T011: same file)
- [x] T015 [US2] Episode capture/recall tests in crates/joey-agent-core/tests/memory_injection.rs (extends T013's file): episode recorded per completed turn, failed-outcome episodes captured, "what did we try" recall from injection, secrets never persisted (quickstart Scenario 7) (depends T013, T014)

**Checkpoint**: US1 + US2 both independently functional — full interactive memory loop

---

## Phase 5: User Story 3 - Memory Across Orchestrated (/hypercode) Runs (Priority: P2)

**Goal**: Orchestrated runs write and read the same memory (FR-006)

**Independent Test**: Run an orchestrated goal; `kind=workstream, source=hypercode` episodes appear; a second run's children receive the memory prefix

### Implementation for User Story 3

- [x] T016 [P] [US3] Write side: one episode per completed unit in `finalize_graph_run` beside `record_verified_outcomes` in crates/joey-cli/src/hypercode.rs — node in graph runs, workstream in legacy runs (clarified Q2 "workstream or subtask", analyze I1) — (task=unit focus/objective, outcome from success flag, approach+lessons from build summary/node result, evidence ids → run node artifacts; gated exactly like outcome recording; redaction inherited from the T004 store choke point, analyze U1) (contract: contracts/neurocode-memory-injection.md)
- [x] T017 [US3] Read side: extend the `lesson_prefix` goal injection in `HypercodeDispatcher::dispatch` in crates/joey-cli/src/hypercode.rs with bounded preferences + relevant-episodes sections, same top_k/char budget as interactive (depends T016: same file)
- [x] T018 [P] [US3] Tests in crates/joey-cli/tests/hypercode_memory_episodes.rs: per-workstream capture on completion, no capture for incomplete workstreams, goal-prefix contains memory sections, disabled = byte-identical hypercode behavior regression

**Checkpoint**: Orchestrated runs feed from and into the same memory

---

## Phase 6: User Story 4 - Transparency and Control Over Memory (Priority: P3)

**Goal**: Users see, correct, delete, and toggle memory via /neurocode (FR-007; SC-004)

**Independent Test**: List memories, correct one, delete another — subsequent behavior changes and deleted items never reappear

### Implementation for User Story 4

- [x] T019 [P] [US4] `memory_command_text` handler + `"memory"` dispatch arm in crates/joey-cli/src/commands/neurocode.rs per contracts/neurocode-memory-command.md (status/list/show/search/correct/delete/enable/disable; consent-style confirm with --yes/-y; episodes immutable — correct only on preferences)
- [x] T020 [P] [US4] Additive `/neurocode` registration update (description + grammar gain `memory`) in crates/joey-cli/src/slash.rs
- [x] T021 [P] [US4] Command tests in crates/joey-cli/tests/neurocode_memory_command.rs: full grammar, usage on malformed args, delete confirmation flow, hard-delete never-reappears (SC-004), status works while disabled

**Checkpoint**: All four stories independently functional

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, budgets, and the feature-wide regression gate

- [x] T022 [P] Documentation: memory subsystem in docs/features/joey-neurocode.md and docs/features/joey-neurocode-rag.md; `/neurocode memory` in docs/cli.md and the README /neurocode line; config keys in docs/state-and-config.md
- [x] T023 [P] PORTING.md: add feature-027 entry (joey-native addition, no upstream equivalent — same note style as feature 015)
- [x] T024 Final gate: `cargo build --workspace && cargo test --workspace` green, then execute all seven quickstart.md scenarios and record results
- [x] T025 [P] Budget/eviction hardening tests in crates/joey-neurocode/tests/memory_stores.rs (extends T008's file): FIFO eviction respects max_episodes, preference strengthen refreshes updated_at without row duplication, supersede links are consistent both directions

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies — start immediately
- **Phase 2 (Foundational)**: After Phase 1 — BLOCKS all user stories
- **Phases 3–6 (Stories)**: After Phase 2; independently testable in priority order (US1 → US2 → US3 → US4) or in parallel by different workers, EXCEPT the noted sequences (T005 after T004; T011 after T010; T012 after T010 [API dependency, analyze I2]; T014 after T011; T015 after T013/T014; T017 after T016)
- **Phase 7 (Polish)**: After all implemented stories

### User Story Dependencies

- **US1 (P1)**: After Phase 2 — no story dependencies (MVP)
- **US2 (P1)**: Extends the US1 capture call-sites (T014 depends T011) but is independently testable once complete
- **US3 (P2)**: Consumes Phase 2 stores only; testable independently of US1/US2 outcomes
- **US4 (P3)**: Consumes Phase 2 stores only; independent of US1–US3

### Within Each User Story

- Tests written alongside implementation (constitution Principle IV); stores/services before wiring; wiring before command surface
- Same-file tasks are explicitly sequenced (see Phase Dependencies) — never run them in parallel

### Parallel Opportunities

- Phase 1: T001 ∥ T002
- Phase 2: T003 ∥ T004 ∥ T006; then T005, T007, T008, T009 as their deps clear
- US1: T010 ∥ T013 (test authoring); then T011; T012 after T010
- US3: T016 ∥ T018(test authoring); then T017
- US4: T019 ∥ T020 ∥ T021
- Polish: T022 ∥ T023 ∥ T025; T024 last, alone

---

## Implementation Strategy

### MVP First (User Story 1)

1. Phase 1 + Phase 2 → foundation green
2. Phase 3 (US1) → validate adaptive preferences end-to-end (quickstart Scenario 3)
3. Stop-and-validate is safe at every checkpoint

### Incremental Delivery

1. Foundation → 2. US1 (adaptive code) → 3. US2 (episodes; quickstart Scenarios 2, 7) → 4. US3 (hypercode; Scenario 5) → 5. US4 (control; Scenario 4) → 6. Polish (Scenarios 1, 6 + full suite)

### Parallel Team Strategy

- Worker A: Phase 2 stores (T003–T005, T007–T008)
- Worker B: retrieval leg (T001–T002, T006, T009)
- After Phase 2: one worker per story (US3 and US4 are fully parallel-safe; US1→US2 sequenced on agent.rs)

---

## Notes

- [P] = different files, no incomplete dependencies
- Same-file sequences are hard ordering constraints — respect them
- Commit after each task or logical group; every phase must leave `cargo build --workspace` green (constitution)
- Regression coverage is not optional: T008, T009, T013, T018, T021 pin prior behavior per Principle VII

---

## Phase 8: Convergence

- [x] T026 Record the `tracing` direct-dependency justification in specs/027-please-enhance-neurocode/research.md per Constitution VIII (partial): added to crates/joey-neurocode-rag/Cargo.toml during the final gate for the memory-search degradation warning; document weight (already in the crate's build graph via joey-neurocode — zero added compile-time/binary cost) against the rejected alternative (eprintln!, inconsistent with workspace observability)
