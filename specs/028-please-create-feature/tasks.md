---
description: "Task list for feature implementation"
---

# Tasks: Context Economy

**Input**: Design documents from `/specs/028-please-create-feature/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/ (3 files)

**Tests**: Per-crate integration tests are included inside each implementation task (constitution IV mandates tests alongside implementation; plan.md Constitution Check VII mandates when-disabled parity coverage). Test-first ordering applies within each task: write the failing test, then implement.

**Organization**: Tasks grouped by user story for independent implementation/testing. Mechanism order within agent.rs is fixed: state block → hygiene → boundary (same file ⇒ those tasks are sequential).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: parallelizable (different files, no dependencies)
- **[Story]**: user story (US1..US6)
- Exact file paths in every description

## Path Conventions

Rust workspace, repo root = crate parent. Paths are relative to repo root: `crates/<crate>/src/...`, `specs/028-please-create-feature/...`, `docs/`, `PORTING.md`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Configuration surface shared by every mechanism (single-file edit, hence sequential).

- [X] T001 Add all 10 context-economy config keys with defaults and clamps to DEFAULT_CONFIG_YAML and getters in crates/joey-core/src/config.rs: scratchpad.enabled=true, scratchpad.max_entry_chars=8000 (clamp 1000..=64000), state_block.enabled=true, state_block.max_chars=1200 (clamp 200..=8000), compression.midturn_tool_hygiene=true, compression.midturn_threshold=0.35 (clamp 0.10..=0.45, must stay < compression.threshold), compression.boundary_trigger=true, compression.boundary_threshold=0.35 (clamp 0.10..=0.45), agent.context_economy_guidance=true, agent.retrieval_verification_nudge=true — per contracts/context-economy-config-keys.md (float thresholds convert to token counts at runtime: threshold × compressor context_length, §Threshold semantics, F3 decision (a)); add config parsing/clamping tests in the same edit

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Cross-cutting plumbing every story reads from; must land before story work.

- [X] T002 [P] Add CONTEXT_ECONOMY_GUIDANCE constant to crates/joey-agent-core/src/guidance.rs using the exact wording pinned in contracts/state-block-injection.md §Guidance (Joey-only string, no upstream text touched) with a unit test asserting the constant matches the contract text
- [X] T003 [P] Memoize serialized tool definitions behind a mutation counter in crates/joey-tools/src/registry.rs (invalidate on register/enable/disable/toolset change) with unit tests: cache hit returns identical bytes, invalidation triggers on every mutation path

**Checkpoint**: Config keys, guidance text, and registry cache in place; user story implementation can begin.

---

## Phase 3: User Story 1 — Details Survive Context Cleanups (Priority: P1) 🎯 MVP

**Goal**: Session scratchpad tool (append/read/clear/stats) with redaction, size bound, sanitized storage under ~/.joey/scratchpads/, post-session persistence, FTS discoverability — FR-001/002/003/012.

**Independent Test**: Start a long task, record exact values via scratchpad, trigger cleanup, then recover them without user re-stating (spec US1 Independent Test).

- [X] T004 [P] [US1] Create scratchpad tool in crates/joey-tools/src/tools/scratchpad_tool.rs: unit struct Scratchpad, actions append(text, label?)/read(tail_entries=20, offset?)/clear()/stats(), storage ~/.joey/scratchpads/<sanitized-key>-<fnv1a-hex8>/scratchpad.md (append-only entries `## <RFC3339> [<label>]\n<text>`, atomic writes, flock sibling lock, [A-Za-z0-9._-] sanitization + FNV-1a-64 hex suffix), redact via joey_core::redact::redact_secrets before persist, reject empty/oversized (>8000 default) entries with guidance, `pub fn stats(session_id)` cross-crate API; include tests: roundtrip, redaction-on-write (fake key → [REDACTED] in file), tail pagination with showing X-Y of Z, oversized rejection, empty-input rejection, sanitization+hash dirname
- [X] T005 [US1] Wire scratchpad into crates/joey-tools/src/tools/mod.rs (module decl + pub use) and register in crates/joey-tools/src/builtins.rs via the register_session_tools pattern, gated by check() returning false when scratchpad.enabled=false; include tests: registered under default config, absent from registry output when disabled, executable append→read roundtrip through the registry

**Checkpoint**: Scratchpad fully functional and independently testable; MVP usable (stories 2+ may proceed in parallel).

---

## Phase 4: User Story 2 — The Assistant Always Knows the Current Plan (Priority: P1)

**Goal**: Deterministic, size-bounded state block rendered per turn into the request clone — FR-004/005.

**Independent Test**: Long task with task list; at a late step ask "what's left?" — answer must match the deterministic block without re-reading history.

- [X] T006 [P] [US2] Create renderer in crates/joey-agent-core/src/state_block.rs: `render(&StateBlockInput) -> Option<String>` with StateBlockInput { todos, scratchpad_stats, turn, max_turns }, sections TASKS / SCRATCHPAD pointer (path + entry count + last time, never content) / PROGRESS (turn n of max), first line `[STATE BLOCK — deterministic, auto-maintained]`, hard bound state_block.max_chars with deterministic tail-first truncation keeping headers, None when no todos and no scratchpad entries; tests: render-with-todos, empty→None, bound-never-exceeded, deterministic same-input→same-output, truncation keeps headers
- [X] T007 [US2] Inject in crates/joey-agent-core/src/agent.rs: field `state_block_context: Mutex<Option<(String, String)>>`, render once per turn deduped on last-user-text key (neurocode pattern), append rendered block as Message::user to the request CLONE in build_request (never self.history, never persisted), ordering guard skips the block when history tail is unresolved tool results, tracing::info! on render/skip; tests: block present in request and absent from history under default config, byte-identical build_request output with state_block.enabled=false, retry-identical within turn, guard skip on tool-result tail

**Checkpoint**: State block on by default, cache-safe, never persisted; stories 1+2 form the complete P1 core.

---

## Phase 5: User Story 3 — Long Tool-Heavy Sessions Stay Lean (Priority: P2)

**Goal**: Mid-turn tool-result hygiene: condense stale tool results to pass-2 one-liners/PRUNED_TOOL_PLACEHOLDER with dedup-first, shared attempt budget — FR-006/007.

**Independent Test**: 30+ tool-call session; stale results appear as one-line summaries with markers, tail stays verbatim, details re-fetchable.

- [X] T008 [US3] Implement hygiene sweep as a new branch in the pre-API pressure check in crates/joey-agent-core/src/agent.rs (token pressure ratio request_pressure_tokens/context_length ∈ [midturn_threshold, compression.threshold) — derive midturn_threshold_tokens = midturn_threshold × context_length per contracts/context-economy-config-keys.md §Threshold semantics; enabled gate; branch ordered BEFORE full-compression decision): dedup identical results first, then rewrite in-place ONLY message contents of tool results older than the protected tail, oldest-first, using compressor pass-2 one-line summary or PRUNED_TOOL_PLACEHOLDER ("[Old tool output cleared to save context space]", compression/compressor.rs:173 verbatim); consume one shared turn-local compression_attempts slot, share failure cooldown, never both hygiene and full compression in one turn; no summary message/marker emitted; session-store rows untouched; tracing::info! "hygiene swept N"; tests: tail verbatim + oldest-first rewrite, distinguishable from compaction (no summary marker), shared budget prevents double-fire, dedup collapses identical results, disabled → history byte-identical below threshold

**Checkpoint**: Tool output condensed continuously, store fidelity preserved, backstop untouched.

---

## Phase 6: User Story 4 — Cleanups Happen at Natural Stopping Points (Priority: P2)

**Goal**: Boundary-aligned cleanup at run_turn exits gated on todo completion — FR-008/009.

**Independent Test**: Complete a task with usage above boundary but below emergency threshold → exactly one cleanup; emergency trigger still fires with open todos.

- [X] T009 [US4] Add boundary cleanup at run_turn exit paths in crates/joey-agent-core/src/agent.rs (exit-site candidates verified 2026-09-10: neurocode_auto_reindex call sites agent.rs:2835, 2932, 2947, 3122, 3197, 3264, 3282, 3335, 3350 — re-verify at implementation time, lines drift), gated on compression.boundary_trigger enabled AND todo list all-complete-or-empty (todo_tool::current) AND request_pressure_tokens ≥ boundary_threshold × context_length (§Threshold semantics) AND session failure-cooldown clear AND fresh post-turn attempt budget available: run existing compressor compress() exactly once, counted as one attempt; tracing::info! on fire; tests: fires exactly once at todo-complete above threshold, never fires with open todos, cooldown respected, disabled → exit paths byte-identical, pressure backstop at 0.50 intact and unchanged

**Checkpoint**: Cleanup timing aligns with task boundaries; backstop and cooldowns preserved.

---

## Phase 7: User Story 5 — Verified On-Demand Loading, Concise Output (Priority: P3)

**Goal**: Standing economy guidance injection + retrieval-verification nudge extension — FR-010/011.

**Independent Test**: Retrieval returns stale fact → assistant re-verifies before finishing; guidance present/absent by switch; response length shrinks without information loss.

- [X] T010 [P] [US5] Inject CONTEXT_ECONOMY_GUIDANCE via the existing gated pattern in crates/joey-agent-core/src/prompt.rs (~L835 MEMORY/SESSION_SEARCH/SKILLS pattern), gated on scratchpad tool present AND agent.context_economy_guidance=true (default), mentioning the scratchpad affordance explicitly; tests: guidance present in system prompt under default config, absent when disabled, prompt still rendered exactly once per session
- [X] T011 [P] [US5] Extend verify-on-stop in crates/joey-agent-core/src/verification.rs (build_verify_on_stop_nudge path, verification.rs:522, cap max_verify_nudges at :608) with a one-line retrieval reminder when the turn's dedupe keys show rag-prefetch or neurocode cold-mode usage, respecting existing max_verify_nudges caps and agent.retrieval_verification_nudge switch; tests: nudge line present only when retrieval used, absent when disabled, caps respected, when-disabled nudge text identical to pre-feature

**Checkpoint**: Guidance + verification nudge on by default, individually disableable.

---

## Phase 8: User Story 6 — Everything On by Default, Individually Switchable (Priority: P3)

**Goal**: Default-on verification and when-disabled parity suite — FR-013/014, SC-004.

**Independent Test**: Flip each switch off one at a time → behavior matches pre-feature system; fresh install needs no setup.

- [X] T012 [P] [US6] Add default-on integration tests in crates/joey-agent-core/tests/parity.rs asserting the full default config activates every mechanism (scratchpad registered, state block rendered, hygiene + boundary armed, guidance + nudge present) and per-mechanism when-disabled parity: build_request output, history contents, tool-registry behavior, and verify-nudge text byte-identical to pre-feature golden snapshots for each of the 10 config keys flipped off individually
- [X] T013 [P] [US6] Add scratchpad-side parity tests in crates/joey-tools/tests/parity.rs: registry output byte-identical to pre-feature when scratchpad.enabled=false, stats() None on missing store, and a fresh JOEY_HOME integration smoke (mktemp -d) proving zero-setup availability

**Checkpoint**: Default-on contractually safe — every mechanism proven additive and reversible.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Living-audit duty, docs, final gate.

- [X] T014 [P] Add the Joey-only additions ledger entry to PORTING.md covering guidance constant, state-block marker wording, PRUNED_TOOL_PLACEHOLDER reuse, 10 config keys, scratchpad tool, registry memoization
- [X] T015 [P] Create docs/context-economy.md documenting all five mechanisms, the 10 config keys with defaults/clamps, and the parity guarantees; add it to the index in docs/README.md
- [X] T016 Run quickstart.md validation scenarios V1–V4 with scoped cargo test commands (JOEY_HOME=$(mktemp -d) cargo test -p joey-tools scratchpad; -p joey-agent-core state_block; -p joey-agent-core compression; -p joey-agent-core prompt / verification; -p joey-agent-core parity + -p joey-tools parity) and record results
- [X] T017 Run cargo build --workspace && cargo test --workspace exactly once as the final acceptance gate; triage failures to a single fix round if needed
- [X] T018 Verify specs/028-please-create-feature/quickstart.md validation instructions match reality (branch refs, mktemp flag, V2b FR heading — typos remediated 2026-09-10 during /speckit-analyze; confirm no regression)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately. Single task, single file.
- **Foundational (Phase 2)**: Depends on T001 (config getters referenced by later gates). T002/T003 are parallel (different files).
- **User Stories (Phases 3–8)**: All depend on Phase 1 + Phase 2. Story phases 3–8 may run in parallel EXCEPT tasks touching crates/joey-agent-core/src/agent.rs, which are strictly sequential in ID order: T007 → T008 → T009 (same file; also logical order state block → hygiene → boundary).
- **Polish (Phase 9)**: Depends on all story phases. T016/T017 sequential (validation then gate); T014/T015/T018 parallel to each other.

### User Story Dependencies

- **US1 (P1)**: after Phase 2 — independent (MVP). T004 → T005 (mod.rs/builtins.rs wiring needs the tool).
- **US2 (P1)**: after Phase 2 — independent of US1 code-wise (state_block.rs standalone), but T007 consumes scratchpad stats — implement after T004 lands.
- **US3 (P2)**: after T007 (same file, ordering); benefits from US1+US2 safety nets per spec.
- **US4 (P2)**: after T008 (same file, ordering; shares budget with hygiene).
- **US5 (P3)**: after Phase 2 — T010/T011 parallel (different files).
- **US6 (P3)**: after ALL story phases (asserts their behavior end-to-end).

### Within Each User Story

- Tests written first within each task (failing before implementation, per task's test list)
- Renderer/module before agent.rs integration (US2: T006 → T007)
- Tool before registration wiring (US1: T004 → T005)
- Story complete before moving to next priority

### Parallel Opportunities

- T002 ∥ T003 (Phase 2, different files)
- T004 ∥ T006 ∥ T010 ∥ T011 (different files, after Phase 2)
- T005 ∥ T007 (after T004/T006 respectively)
- T012 ∥ T013 (US6 test files, different crates)
- T014 ∥ T015 ∥ T018 (Polish, different files)
- NEVER parallel: any two tasks touching crates/joey-agent-core/src/agent.rs (T007/T008/T009)

---

## Parallel Example: User Story 1 + 2

```bash
# After Phase 2, launch in parallel (different files):
Task: T004 "Create scratchpad tool in crates/joey-tools/src/tools/scratchpad_tool.rs"
Task: T006 "Create state-block renderer in crates/joey-agent-core/src/state_block.rs"
Task: T010 "Inject guidance in crates/joey-agent-core/src/prompt.rs"
Task: T011 "Extend verify nudge in crates/joey-agent-core/src/verification.rs"

# Then sequential wiring on agent.rs (same file):
Task: T005 "Wire scratchpad into builtins"
Task: T007 → T008 → T009 "agent.rs state block → hygiene → boundary (strict order)"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (T001) + Phase 2 (T002, T003)
2. Complete Phase 3 (T004, T005)
3. STOP and VALIDATE: JOEY_HOME=$(mktemp -d) cargo test -p joey-tools scratchpad — US1 independently green
4. Scratchpad is demoable/deployable on its own

### Incremental Delivery

1. Setup + Foundational → foundation ready
2. +US1 (scratchpad) → validate V1 → MVP 🎯
3. +US2 (state block) → validate V2 → P1 core complete
4. +US3 (hygiene) → validate V2b part 1 → lean tool sessions
5. +US4 (boundary) → validate V2b part 2 → boundary timing
6. +US5 (guidance + nudge) → validate V2c → steady-state economy
7. +US6 (parity suite) → validate V3 → default-on contractually safe
8. Polish (T014–T018) → V4 docs/ledger → final gate T017

### Parallel Team Strategy

1. Team completes Setup + Foundational together
2. Dev A: US1 (joey-tools); Dev B: US2 renderer (state_block.rs) then queues for agent.rs; Dev C: US5 (prompt.rs + verification.rs); Dev D: US6 parity tests after stories land
3. agent.rs work serialized: T007 → T008 → T009 (single owner at a time)
4. Polish parallel: docs/ledger/quickstart-fix across different files

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks
- [Story] label maps tasks to spec.md user stories for traceability
- Every task includes its tests (constitution IV); parity tasks (T012/T013) discharge constitution VII's regression mandate for the touched public surfaces (config keys, registry output, request wire format)
- agent.rs task order T007 → T008 → T009 is a hard constraint (same file + logical layering)
- Verify current agent.rs exit-site list at implementation time (research R5 risk: line numbers drift)
- Commit after each task or logical group; stop at any checkpoint to validate a story independently

---

## Phase 10: Convergence

- [X] T019 Wire the retrieval-verification nudge into run_turn's completion path: populate RetrievalUsage from the turn's rag-prefetch/neurocode assembly state, pass the turn's changed file paths, gate on verify_on_stop_enabled + max_verify_nudges caps + agent.retrieval_verification_nudge; add tests (nudge delivered in-session when retrieval used, absent when disabled/capped) per FR-010, US5/AC1 (partial)
- [X] T020 Include the tool-schema token component in the boundary-cleanup pressure estimate at run_turn exits (mirror the pre-API gate's estimate_request_tokens_rough + estimate_tools_tokens_rough sum; thread the turn's tools or their token count into boundary_cleanup_if_appropriate) per FR-008, contracts/context-economy-config-keys.md §Threshold semantics (partial)
- [X] T021 Renumber the docs/README.md doc index sequentially (duplicate 5s and 7s) per plan.md docs touchpoint (partial)
