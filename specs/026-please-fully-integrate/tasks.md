# Tasks: Native Spec-Kit Integration with Copilot Command Parity

**Input**: Design documents from `/specs/026-please-fully-integrate/`

**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/, quickstart.md — all present.

**Tests**: Included as explicit tasks — FR-012 and constitution Principle IV mandate tests alongside implementation.

**Organization**: Tasks grouped by user story (spec.md US1-US7) for independent implementation and testing.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1-US7)
- Exact file paths in every description

## Path Conventions

Rust workspace under `crates/` (see plan.md Project Structure). All paths repo-relative.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Config defaults and vendored assets

- [x] T001 Add `speckit:` defaults block (enabled/lifecycle_context/hooks, all default true) per contracts/config-keys.md and bump CONFIG_VERSION additively in crates/joey-core/src/config.rs
- [x] T002 [P] Vendor the ten upstream workflow bodies (specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues) as crates/joey-cli/src/speckit_bodies/<name>.md

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core modules all stories depend on

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [x] T003 Create crates/joey-cli/src/speckit_bodies.rs: include_str! registry of the ten bodies, WorkflowBodySource enum, resolution chain (.github/skills/speckit-<name>/SKILL.md → .github/agents/speckit.<name>.agent.md + companion prompt → .specify/ → ~/.joey/skills → bundled), frontmatter parser for handoffs (label/agent/prompt/send), scripts (sh/ps/py), tools
- [x] T004 [P] Create crates/joey-cli/src/speckit_lifecycle.rs: LifecycleState (feature_directory + step enum), derivation rules per contracts/lifecycle-state.md, injected context block renderer, FeatureScope extraction (read_files/write_files/acceptance_criteria) via the joey-speckit-ui parser; stale/missing feature pointer → step None with reselect guidance (edge case 5)
- [x] T005 [P] Create crates/joey-cli/src/speckit_hooks.rs: serde_yaml model of .specify/extensions.yml, the 20 hook points (before_/after_ × ten commands), enabled filter, optional/mandatory classification, condition pass-through, silent-skip on invalid YAML
- [x] T006 Foundational unit tests in crates/joey-cli/src/tests/speckit_native.rs: resolution precedence order, frontmatter parse round-trip, lifecycle derivation truth table (incl. stale feature pointer → None), hooks parse/filter/skip (covers T003-T005)
- [x] T007 Wire new modules in crates/joey-cli/src/main.rs and swap body loading in crates/joey-cli/src/speckit_slash.rs to the resolution chain (bundled floor guarantees success with no ~/.joey/skills installed)

**Checkpoint**: Foundation ready — user story implementation can begin

---

## Phase 3: User Story 1 - Full Lifecycle from Native Commands (Priority: P1) 🎯 MVP

**Goal**: All ten lifecycle commands run natively with upstream pre-flight behavior and artifact results

**Independent Test**: Run each lifecycle command in a scratch spec-kit repo (quickstart.md §1, slash form) and confirm expected artifacts

### Implementation for User Story 1

- [x] T008 [P] [US1] Add pre-flight fallback registry (missing-script → internal equivalent + warning, including PowerShell-scaffold variants per FR-002) in crates/joey-cli/src/speckit_slash.rs run_specify_script path
- [x] T009 [US1] Command-surface parity test pinning the ten commands, their slash forms, and pre-flight script bindings against contracts/command-surface.md in crates/joey-cli/src/tests/speckit_native.rs; include script-present-but-failing → hard error naming script + platform variant (contract invariant 4)
- [x] T010 [US1] Validate scratch-repo lifecycle walkthrough (quickstart.md §1) end-to-end; record results in specs/026-please-fully-integrate/quickstart.md checklist comments

**Checkpoint**: User Story 1 fully functional and independently testable

---

## Phase 4: User Story 2 - Dot-Form and Slash-Form Parity (Priority: P1)

**Goal**: Every command invokable as `/speckit-<name>` AND `speckit.<name>` with identical dispatch

**Independent Test**: Invoke all ten commands via both forms; identical help text and results (quickstart.md §1)

### Implementation for User Story 2

- [x] T011 [US2] Dotted `speckit.` prefix intercept in crates/joey-cli/src/repl.rs process_input normalizing to the shared speckit_step_slash dispatch (never spawns external binaries)
- [x] T012 [US2] Same dotted intercept in crates/joey-cli/src/tui.rs handle_slash
- [x] T013 [P] [US2] Completion candidates for both forms in crates/joey-cli/src/slash_menu.rs SmartCompleter
- [x] T014 [US2] Unknown-command error listing available commands with closest suggestion (both forms; never shadows user commands) in crates/joey-cli/src/speckit_slash.rs
- [x] T015 [US2] Parity tests: 12 commands (10 lifecycle + status/help) × 2 forms dispatch identically, collision disambiguation, and speckit.enabled=false short-circuits all new paths (FR-013) in crates/joey-cli/src/tests/speckit_native.rs

**Checkpoint**: Stories 1 AND 2 both independently functional

---

## Phase 5: User Story 3 - Bundled Bodies and Project-Local Overrides (Priority: P2)

**Goal**: Bodies ship in-binary; project-local overrides from upstream layouts win

**Independent Test**: Hide ~/.joey/skills and run commands (bundled floor); add an override and see it preferred (quickstart.md §2-3)

### Implementation for User Story 3

- [x] T016 [US3] US3 acceptance tests in crates/joey-cli/src/tests/speckit_native.rs: fresh-machine (skills dir absent) uses bundled body; .github/skills override preferred; .github/agents layout discovered; precedence order pinned
- [x] T017 [US3] Record vendored-body provenance (upstream spec-kit version, refresh procedure script) in crates/joey-cli/src/speckit_bodies/PROVENANCE.md

**Checkpoint**: Body delivery independent of installed skills

---

## Phase 6: User Story 4 - Extension Hooks Execution (Priority: P2)

**Goal**: All 20 hook points discovered and executed per upstream semantics

**Independent Test**: Register a mandatory before-specify hook creating a marker; run specify; marker exists before spec creation (quickstart.md §4)

### Implementation for User Story 4

- [x] T018 [US4] Wire before_/after_ hook execution into step dispatch (mandatory: execute as native command turn and await, failure stops step; optional: surface block) in crates/joey-cli/src/speckit_slash.rs
- [x] T019 [US4] speckit.hooks config gating and silent-skip semantics in crates/joey-cli/src/speckit_hooks.rs discovery entry point
- [x] T020 [US4] Hook parity tests per contracts/hooks.md in crates/joey-cli/src/tests/speckit_native.rs: 20 points enumerated, enabled=false excluded, mandatory blocks, optional surfaces, invalid YAML skips silently; speckit.enabled=false and speckit.hooks=false skip discovery entirely (FR-013)

**Checkpoint**: Hooks behavior matches contracts/hooks.md

---

## Phase 7: User Story 5 - Command Handoff Chaining (Priority: P2)

**Goal**: Upstream handoff semantics — offer next step (or auto-invoke when send=true) with carried context

**Independent Test**: Complete specify; plan/clarify handoff offered per workflow frontmatter; send-flag behavior correct

### Implementation for User Story 5

- [x] T021 [US5] Post-step handoff resolution and offer/auto-send (send flag semantics, prior outputs carried) in crates/joey-cli/src/speckit_slash.rs and crates/joey-cli/src/repl.rs submit path
- [x] T022 [US5] Handoff tests (offer-without-start, auto-send invokes, context carried) in crates/joey-cli/src/tests/speckit_native.rs

**Checkpoint**: Lifecycle chaining works per FR-006

---

## Phase 8: User Story 6 - Spec-Kit-Aware Orchestration (Priority: P2)

**Goal**: Conductor detects feature/step automatically, adapts dispatch, fans out implement work safely

**Independent Test**: Session in a repo with unchecked tasks.md → implement fan-out with write_set exclusivity; no spec → specify step proposed (quickstart.md §5)

### Implementation for User Story 6

- [x] T023 [US6] Session-start lifecycle detection + one-time context injection through the extra_instructions slot (gated by speckit.lifecycle_context; cache-friendly, never per-turn) in crates/joey-cli/src/repl.rs and crates/joey-cli/src/oneshot.rs, with speckit.enabled=false / speckit.lifecycle_context=false short-circuit asserted (FR-013)
- [x] T024 [P] [US6] Dynamic CURRENT LIFECYCLE STATE block appended after the static SPEC_KIT_DOCTRINE in crates/joey-omo/src/agents/prompts/conductor.rs render path (static doctrine text unchanged)
- [x] T025 [P] [US6] tasks.md → orchestration TaskNode adapter (id/objective/dependencies/read_set/write_set) in crates/joey-cli/src/speckit_lifecycle.rs, enforcing no-two-specialists-same-file via existing write_set overlap checks in crates/joey-cli/src/hypercode.rs
- [x] T026 [US6] Orchestration tests: conductor prompt contains dynamic block (crates/joey-omo/tests/verify_prompts.rs) and adapter produces valid TaskNodes with exclusive write sets (crates/joey-cli/src/tests/speckit_native.rs), incl. shared-file parallel tasks demoted to sequential with surfaced collision (edge case 4)

**Checkpoint**: Orchestration lifecycle-aware per FR-008/FR-009

---

## Phase 9: User Story 7 - Spec-Kit-Aware Code Intelligence (Priority: P3)

**Goal**: Neurocode context/indexing/verification scoped to the active feature

**Independent Test**: With a feature active, code questions over task-listed files include those files' entities; empty scope behaves exactly as before (quickstart.md §5)

### Implementation for User Story 7

- [x] T027 [P] [US7] Add `scope_files: Vec<String>` (default empty) to CodingRequest in crates/joey-neurocode/src/engine.rs and seed find_primary_nodes from scope_files in crates/joey-neurocode/src/context/discovery.rs
- [x] T028 [P] [US7] Scope-prioritized reindex ordering (thresholds unchanged) in crates/joey-neurocode/src/auto_index.rs
- [x] T029 [P] [US7] Additive acceptance_criteria input referencing spec scenarios in verification plans in crates/joey-neurocode/src/verification_plan.rs
- [x] T030 [US7] Wire FeatureScope → scope_files from crates/joey-cli/src/speckit_lifecycle.rs into the engine in crates/joey-cli/src/neurocode_wiring.rs
- [x] T031 [US7] Scoped enrichment + acceptance-criteria verification + empty-scope no-regression tests in crates/joey-neurocode/tests/scope_enrichment.rs

**Checkpoint**: Code intelligence feature-scoped per FR-010; no active feature → unchanged

---

## Phase 10: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, validation, final gate

- [x] T032 [P] Document native command surface, dotted form, config keys, and hooks in docs/speckit-workflow.md, normalizing terminology (workflow body ≡ vendored body; dotted form ≡ Dot-Form; hypercode ≡ orchestration/conductor layer)
- [x] T033 [P] Add spec-kit integration status entry (Complete scope, deliberate-divergence notes) to PORTING.md
- [x] T034 Run all specs/026-please-fully-integrate/quickstart.md validation scenarios (§1-§7) end-to-end; fix fallout and record results
- [x] T035 Pin FR-007 status/help parity: assert /speckit-status and /speckit-help surfaces match or exceed upstream capabilities (lifecycle block content, per-command guidance) in crates/joey-cli/src/tests/speckit_native.rs
- [x] T036 Honor frontmatter tools references at dispatch time (step-turn toolset restricted per tools refs, e.g. taskstoissues github issue tools) per FR-004a in crates/joey-cli/src/speckit_slash.rs with tests in crates/joey-cli/src/tests/speckit_native.rs
- [x] T037 Validate performance budgets: SC-003 lifecycle dispatch <2s (pre-flight + body load + submission, excluding inference) and SC-005a feature-scoped indexing <3s vs unscoped baseline, with timing tests in crates/joey-cli/src/tests/speckit_native.rs and crates/joey-neurocode/tests/scope_enrichment.rs; record measured numbers in specs/026-please-fully-integrate/
- [x] T038 Regression: all 12 pre-existing /speckit-* commands behave identically to pre-feature when no project overrides exist and ~/.joey/skills bodies load (prior body source, output shape) per constitution VII / SC-006a in crates/joey-cli/src/tests/speckit_native.rs
- [x] T039 Full workspace gate: `cargo build --workspace` and `cargo test --workspace` green; record summary in specs/026-please-fully-integrate/

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: Depends on Phase 1 — BLOCKS all user stories
- **User Stories (Phases 3-9)**: Depend on Phase 2; can run in parallel (if staffed) or sequentially US1 → US7
- **Polish (Phase 10)**: Depends on all completed stories it validates

### User Story Dependencies

- **US1 (P1)**: After Phase 2 — no story dependencies
- **US2 (P1)**: After Phase 2 — shares dispatch files with US1; implement after US1 to avoid same-file conflicts in crates/joey-cli/src/speckit_slash.rs
- **US3 (P2)**: After Phase 2 (T016 depends on T006/T007 resolution chain)
- **US4 (P2)**: After Phase 2 — touches crates/joey-cli/src/speckit_slash.rs; sequence after US1/US2
- **US5 (P2)**: After US1 (handoffs attach to completed steps); touches crates/joey-cli/src/speckit_slash.rs — sequence after US4
- **US6 (P2)**: After Phase 2; independent files (repl.rs/oneshot.rs/conductor.rs) except T025 extends T004's module
- **US7 (P3)**: After Phase 2; independent (joey-neurocode files) except T030 depends on T004 FeatureScope

### Within Each User Story

- Models/modules before wiring; wiring before tests-that-exercise-wiring
- Unit tests land with their implementation task, not after (constitution IV)

### Parallel Opportunities

- T002 parallel with T001; T004/T005 parallel with T003 (different files)
- T013 parallel with T011/T012; T024 parallel with T023/T025; T027/T028/T029 fully parallel (different joey-neurocode files); T032/T033 parallel
- Different stories on disjoint files can proceed simultaneously (US6 conductor work ∥ US7 neurocode work ∥ US1)

---

## Parallel Example: User Story 7

```bash
# Launch independent joey-neurocode changes together:
Task: "T027 Add scope_files to CodingRequest in crates/joey-neurocode/src/engine.rs + discovery seeding"
Task: "T028 Scope-prioritized reindex ordering in crates/joey-neurocode/src/auto_index.rs"
Task: "T029 acceptance_criteria input in crates/joey-neurocode/src/verification_plan.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: ten lifecycle commands native via slash form (quickstart §1)
5. Deploy/demo if ready

### Incremental Delivery

1. Setup + Foundational → foundation ready
2. +US1 → native lifecycle (MVP!) → validate
3. +US2 → dual-form parity → validate
4. +US3 → body independence → validate
5. +US4 → hooks → +US5 → handoffs → +US6 → orchestration → +US7 → neurocode scoping
6. Polish: docs, quickstart validation, workspace gate

### Parallel Team Strategy

1. Team completes Phases 1-2 together
2. Then: Dev A → US1→US2→US4→US5 (speckit_slash.rs chain); Dev B → US6; Dev C → US7
3. Stories integrate independently; final phase runs the full gate

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps each task to its spec.md user story
- Every story independently completable and testable at its checkpoint
- speckit.enabled=false must short-circuit ALL new paths (T001 gating verified in T015/T020/T023 tests)
- Commit after each task or logical group; stop at any checkpoint to validate
