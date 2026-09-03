---
description: "Task list for feature 025 OMO-HyperCode Orchestration Integration"
---

# Tasks: OMO-HyperCode Orchestration Integration

**Input**: Design documents from `/specs/025-please-integrate-omo/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Included per constitution principle IV/VII (tests alongside implementation for module changes and public-surface regressions) and spec SC-006.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g. US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- Workspace crates under `crates/` — paths in tasks are repository-relative
- Source tree and structure decisions: see plan.md "Project Structure"

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Establish a verified baseline before any change.

- [x] T001 Verify green baseline: run `cargo test -p joey-omo && cargo test -p joey-orchestration && cargo test -p joey-cli` and record results (crates/joey-omo, crates/joey-orchestration, crates/joey-cli)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The OMO-side persona library every user story depends on.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [x] T002 Create the delegation-first persona module at crates/joey-omo/src/agents/prompts/conductor.rs: atlas-inherited conductor identity, embedded orchestration hard-rules core (no direct writes; single final gate), full-roster delegation briefing, spec-kit lifecycle doctrine (read-only research/review for specify/clarify/plan; parallel implementation for implement; one final acceptance run), with `default`, generic-GPT `gpt`, and `gpt_5_6` variants plus a `for_model(model: &str) -> &'static str` two-level dispatch (ModelFamily prefix match; Gpt arm sub-checks `5.6`/`5-6` per research D3)
- [x] T003 [P] Export the conductor dispatch (unregistered — no AgentRegistry entry, no tab) from crates/joey-omo/src/agents/prompts/mod.rs per research D2
- [x] T004 [P] Add prompt-fidelity tests in crates/joey-omo/tests/verify_prompts.rs (extend existing suite): variant selection for default/GPT/GPT-5.6 model ids (both `5.6` and `5-6` separators), hard-rules presence in every conductor variant, spec-kit doctrine present, and conductor NOT present in the agent registry

**Checkpoint**: Persona library complete and tested; user story wiring can begin.

---

## Phase 3: User Story 1 - Delegation-First Orchestrator Persona (Priority: P1) 🎯 MVP

**Goal**: When integration is active (orchestration enabled + OMO registry populated), the orchestrator's governing instructions become the delegation-first persona (FR-001, FR-002, FR-012).

**Independent Test**: quickstart.md V1 (persona content) and V7 (degradation to existing behavior).

### Implementation for User Story 1

- [x] T005 [P] [US1] Implement `orchestrator_persona_overlay(agent: Option<&str>, model: &str)` in crates/joey-cli/src/hypercode.rs: integration-active → conductor variant (agent None) or named agent persona via existing dispatch (research D1); hard-rules core embedded in every form
- [x] T006 [US1] Implement the activation gate in crates/joey-cli/src/hypercode.rs: persona overlay applies iff orchestration enabled AND OMO registry has ≥1 resolved agent; otherwise return the existing fixed ORCHESTRATOR_PROMPT unchanged (research D6, FR-012)
- [x] T007 [US1] Route the overlay call sites in crates/joey-cli/src/engine.rs (session start, SetOrchestratorMode ON) through the persona-aware function from T005
- [x] T008 [P] [US1] Add tests in crates/joey-cli/src/tests/hypercode_persona.rs: default persona is delegation-first with parallel-dispatch directives and zero hands-on directives (SC-002); inactive integration keeps byte-identical fixed prompt; empty registry degrades with empty-bench notice; the orchestrator toolset remains restricted so no persona can bypass toolset limits (spec edge case)

**Checkpoint**: MVP complete — default orchestrator is delegation-first whenever integration is active; behavior unchanged otherwise.

---

## Phase 4: User Story 2 - Full OMO Roster Callable (Priority: P2)

**Goal**: All 11 registered OMO agents are name-addressable delegation targets with an enriched unknown-name error (FR-003, FR-010, contracts/delegation-roster.md).

**Independent Test**: quickstart.md V2.

### Implementation for User Story 2

- [x] T009 [P] [US2] Widen the `subagent_type` enum in the `call_omo_agent` schema to all 11 roster names (sisyphus, hephaestus, prometheus, atlas, oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior) in crates/joey-orchestration/src/delegation_tool.rs (research D4)
- [x] T010 [US2] Enrich the unknown-subagent-type error in crates/joey-orchestration/src/delegation_tool.rs to list the valid agent names (User Story 2 scenario 2)
- [x] T011 [US2] Honor `load_skills` on the named-agent delegation path in crates/joey-orchestration/src/delegation_tool.rs: when routing by subagent type, construct the skill overlay (prompt_append) from load_skills entries exactly as the category path does, so skill loading works without category routing (FR-004)
- [x] T012 [P] [US2] Add tests in crates/joey-orchestration/tests/roster_delegation.rs: each of the 11 names resolves; unknown name error lists valid names; load_skills produces the skill overlay on the named-agent path (FR-004); category/subagent_type mutual exclusivity unchanged (BC-011)

**Checkpoint**: Full roster callable; additive-only surface change verified.

---

## Phase 5: User Story 3 - Role-to-Agent Model Mapping (Priority: P3)

**Goal**: Explorer/implementor/orchestrator role model defaults derive from OMO chains with warn-and-inherit fallback (FR-005, FR-006, contracts/role-defaults.md).

**Independent Test**: quickstart.md V3.

### Implementation for User Story 3

- [x] T013 [P] [US3] Implement OMO-chain default derivation for explorer (explore→librarian) and implementor (momus) roles in crates/joey-cli/src/hypercode.rs: applies only when configured model is empty; resolves against available providers in order; unresolvable chain → inherit parent/role default + warning emitted through the existing agent-notice channel (same mechanism as other delegation notices)
- [x] T014 [P] [US3] Mirror the identical derivation in the role gap-fill path of crates/joey-orchestration/src/delegation_tool.rs (HyperRoleSettings; config keys are the contract — both sides derive identically, including the agent-notice warning on unresolvable chains)
- [x] T015 [US3] Apply the orchestrator-role chain (sisyphus→hephaestus→metis) at session model resolution in crates/joey-cli/src/hypercode.rs: used only when the user has not pinned or configured a session model; no new hypercode configuration key (same resolution and warning semantics as T013)
- [x] T016 [P] [US3] Add tests in crates/joey-cli/src/tests/hypercode_persona.rs and crates/joey-orchestration/tests/roster_delegation.rs: chain resolution precedence (override wins > chain default > inherit+warning), first-resolvable-member selection, warning emitted on unresolvable chain via the agent-notice channel

**Checkpoint**: Zero-config roles resolve from OMO chains; explicit overrides unchanged.

---

## Phase 6: User Story 4 - Agent Switch Swaps Only the Persona (Priority: P4)

**Goal**: The existing OMO agent switch replaces only the persona overlay mid-session; orchestration invariants remain active (FR-007, research D1).

**Independent Test**: quickstart.md V4.

### Implementation for User Story 4

- [x] T017 [US4] Extend `reapply_orchestrator_overlay` in crates/joey-cli/src/engine.rs to carry the current agent name and resolved model, and have `engine_switch_agent` supply both so the swapped persona (named agent) replaces the default persona while toolset, roles, and final-gate rule stay active
- [x] T018 [US4] Ensure model switches re-select the persona variant for the new family without losing the persona: route `engine_switch_model` reapply through the persona-aware function in crates/joey-cli/src/engine.rs
- [x] T019 [P] [US4] Add tests in crates/joey-cli/src/tests/hypercode_persona.rs: switching across all four primaries swaps persona text with orchestration invariants intact and no session restart (SC-003); model-family change keeps persona and reselects variant; two agents sharing the same resolved model still swap distinct persona text (spec edge case)

**Checkpoint**: Persona switching fully live on the existing surface.

---

## Phase 7: User Story 5 - Model-Optimized Prompt Selection (Priority: P5)

**Goal**: GPT-5.6 variants exist for the persona (done in T002) and all four switchable primaries; family-aware selection everywhere (FR-008, FR-009).

**Independent Test**: quickstart.md V5.

### Implementation for User Story 5

- [x] T020 [P] [US5] Add `gpt_5_6()` variant and Gpt-arm version sub-check (`5.6`/`5-6` → gpt_5_6, else existing generic) to crates/joey-omo/src/agents/prompts/sisyphus.rs
- [x] T021 [P] [US5] Add `gpt_5_6()` variant and Gpt-arm version sub-check to crates/joey-omo/src/agents/prompts/atlas.rs
- [x] T022 [P] [US5] Add a Gpt arm with `gpt_5_6()` (and generic `gpt()` fallback) to crates/joey-omo/src/agents/prompts/prometheus.rs (currently model-agnostic)
- [x] T023 [P] [US5] Extend variant-selection tests in crates/joey-omo/tests/verify_prompts.rs: GPT-5.6 ids select gpt_5_6 for sisyphus/atlas/prometheus/conductor; hephaestus unchanged; non-GPT families keep existing variants; delegation-only agents fall back without error

**Checkpoint**: GPT-5.6 selection complete across the persona and all switchable primaries.

---

## Phase 8: User Story 6 - Spec-Kit Workflow Synergy (Priority: P6)

**Goal**: Orchestrator dispatch patterns follow the active spec-kit lifecycle step (FR-011, SC-007, research D7).

**Independent Test**: quickstart.md V6.

### Implementation for User Story 6

- [x] T024 [US6] Verify and refine the spec-kit doctrine in crates/joey-omo/src/agents/prompts/conductor.rs: lifecycle dispatch patterns plus the concrete step-detection procedure (read `.specify/feature.json`, infer step from spec.md/plan.md/tasks.md presence)
- [x] T025 [US6] Run the end-to-end scenario from specs/025-please-integrate-omo/quickstart.md V6: drive a small spec-kit feature under orchestration; verify read-only researchers/reviewers during specify/clarify/plan, parallel implementors during implement, exactly one final acceptance run

**Checkpoint**: Orchestration reinforces the spec-kit lifecycle end-to-end.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, parity tracking, and the final gate.

- [x] T026 [P] Update PORTING.md with the OMO↔HyperCode integration parity entries (Complete status + date) per repo convention
- [x] T027 [P] Update docs/orchestration.md (and docs/README.md index if applicable) with the persona-aware orchestrator, roster surface, and role defaults
- [x] T028 Run the full quickstart validation V1–V7 from specs/025-please-integrate-omo/quickstart.md
- [x] T029 Final gate: `cargo test --workspace` green with zero regressions vs the T001 baseline (SC-006, FR-012)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately.
- **Foundational (Phase 2)**: Depends on Phase 1 — BLOCKS all user stories.
- **User Stories (Phases 3–8)**: All depend on Phase 2. Stories may run in parallel (different files) or sequentially in priority order (P1 → P6).
- **Polish (Phase 9)**: Depends on all user stories being complete.

### User Story Dependencies

- **US1 (P1)**: After Phase 2 — no story dependencies (MVP).
- **US2 (P2)**: After Phase 2 — independent of US1 (touches joey-orchestration only).
- **US3 (P3)**: After Phase 2 — derivation logic is orthogonal to US1 but shares crates/joey-cli/src/hypercode.rs with it; sequence after US1 (or coordinate serially on that file).
- **US4 (P4)**: After Phase 2 — builds on US1's persona-aware overlay function (T005); sequence after US1.
- **US5 (P5)**: After Phase 2 — independent of US1–US4 (pure joey-omo prompt work).
- **US6 (P6)**: After Phase 2 — T024 refines conductor text (independent); T025 validates end-to-end (needs US1 wired).

### Within Each User Story

- Persona library (models) before consumers (engine wiring) before tests where tests exercise integration.
- Same-file tasks are sequential by design (T005→T006→T007; T009→T010→T011; T013→T015; T017→T018).

### Parallel Opportunities

- T003 ∥ T004 after T002; T012 within US2 once the same-file sequence T009→T010→T011 lands; only T014 within US3 is parallel-safe (joey-orchestration) — T013/T015/T016 share hypercode.rs and hypercode_persona.rs; T020 ∥ T021 ∥ T022 within US5, then T023; T026 ∥ T027 in Phase 9; whole stories US1 ∥ US2 ∥ US5 after Phase 2 (disjoint files: joey-cli vs joey-orchestration vs joey-omo prompts); sequence US3 and US4 after US1 (shared hypercode.rs / engine wiring).

---

## Parallel Example: User Story 5

```bash
# After Phase 2, launch the three variant tasks together (different files):
Task: "T020 Add gpt_5_6 to crates/joey-omo/src/agents/prompts/sisyphus.rs"
Task: "T021 Add gpt_5_6 to crates/joey-omo/src/agents/prompts/atlas.rs"
Task: "T022 Add Gpt arm to crates/joey-omo/src/agents/prompts/prometheus.rs"
# Then T023 test extension once all three land.
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (baseline)
2. Complete Phase 2: Foundational (persona library + tests)
3. Complete Phase 3: User Story 1 (CLI wiring + activation gate + tests)
4. **STOP and VALIDATE**: quickstart V1 + V7 independently
5. Ship/demo if ready — default orchestrator is now delegation-first

### Incremental Delivery

1. Setup + Foundational → persona library ready
2. +US1 → MVP (delegation-first default persona)
3. +US2 → full roster callable
4. +US3 → OMO-chain role defaults
5. +US4 → mid-session persona switching
6. +US5 → GPT-5.6 variant coverage
7. +US6 → spec-kit lifecycle synergy validated
8. Phase 9 → docs, parity tracker, quickstart, final gate

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to spec.md user stories US1–US6
- Commit after each task or logical group; stop at any checkpoint to validate independently
- Guidance text authored fresh in conductor.rs is new (not upstream-ported); existing verbatim-ported strings elsewhere must not be reworded
- All changes additive per constitution VII; any deviation found during implementation escalates to plan.md revision, not silent scope change

---

## Phase 10: Convergence

- [x] T030 Make named OMO delegations run under the agent's identity: resolve the named agent's identity prompt (joey_omo dispatch_system_prompt) on the subagent_type path (crates/joey-cli/src/omo_resolver.rs prompt_append) and verify role enrichment composes with named routing (explicit request values win over role gap-fills) per US2/AC1 + FR-004 + contracts/delegation-roster.md (partial)
- [x] T031 Emit a user-visible empty-bench notice (EngineEvent::Notice at session start and SetOrchestratorMode ON) when orchestration is active but the OMO registry resolves zero agents, instead of silently degrading to the fixed prompt per spec edge case (empty registry) + T008 (partial)
- [x] T032 Surface the FR-006 unresolvable-chain warning user-visibly on the joey-cli /hypercode run path and the session-model path (agent-notice channel at the run's call sites; keep tracing as log mirror) per FR-006 + T013 (partial)
- [x] T033 Complete one observable end-to-end spec-kit drive under orchestration (interactive or config-enabled session on a toy feature) recording read-only dispatch during specify/clarify/plan, parallel implementors during implement, and exactly one final acceptance run per SC-007 + quickstart V6 (partial)
- [x] T034 Review the unused conductor_prompt() export in crates/joey-omo/src/agents/prompts/mod.rs: either wire joey-cli's persona overlay through it as the single dispatch surface or remove the dead export per plan modularity decision (unrequested)
