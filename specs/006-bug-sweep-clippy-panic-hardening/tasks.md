# Tasks: Code-Hygiene Bug Sweep & Panic Hardening

**Input**: Design documents from `/specs/006-bug-sweep-clippy-panic-hardening/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/ (all present)

**Tests**: Tests ARE required — the spec explicitly mandates them (FR-002, FR-006, SC-003, SC-005). Test tasks are included.

**Organization**: Tasks grouped by user story. P3 (US3) is sliced into 7 per-crate increments per SC-007, each an independently shippable/testable phase.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Rust Cargo workspace. Crates live under `crates/<crate>/src/`. Tests under `crates/<crate>/tests/` (integration) or inline `#[cfg(test)]` (unit). The committed audit script lives at repo-root `scripts/`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the FR-010 measurement tool before any hardening begins, so before/after counts are objective.

- [x] T001 Create `scripts/audit-external-input-unwraps.sh` per FR-010: a `bash`+`rg` script that enumerates `.unwrap()`/`.expect()` **and** `panic!()`/`unreachable!()` call sites in `src/` (excluding `tests/` and `#[cfg(test)]`) across the 7 in-scope crates (`joey-tools`, `joey-providers`, `joey-core`, `joey-mcp`, `joey-gateway`, `joey-cron`, `joey-agent-core`), classifies each as external-input vs. safe via the curated path/function allowlist (research.md R4), prints per-crate breakdown, and exits non-zero if any external-input site lacks a typed-error conversion or `// SAFETY:` comment. The `panic!`/`unreachable!` enumeration satisfies the spec Edge Cases delegation (spec.md "What about the panic!/unreachable! sites?"). Make it executable (`chmod +x`). Document the classification heuristic in a header comment.

**Checkpoint**: Audit script exists and runs (may exit non-zero — that is the before-state).

---

## Phase 2: Foundational (Baseline Capture)

**Purpose**: Record the objective before-state so every success criterion is measurable. MUST complete before user story work.

- [x] T002 Capture baseline: run `cargo build --workspace && cargo test --workspace` and record the test count (confirm 0 failures — SC-001 before-state). Run `cargo clippy --workspace` and record the exact warning count + list (SC-002 before-state). Run `scripts/audit-external-input-unwraps.sh` and record the per-crate external-input unwrap counts (SC-004 before-state). Write these three numbers into the commit message of the first feature commit.

**Checkpoint**: Baseline recorded. User story implementation can begin.

---

## Phase 3: User Story 1 - Fix Confirmed Logic Bugs (Priority: P1) 🎯 MVP

**Goal**: Kimi `k2.6`/`k2-6` model ids resolve to the k2.6 prompt (not the k2.7 prompt). The missing `kimi_k2_6()` prompt function is authored and wired.

**Independent Test**: `cargo test -p joey-omo kimi` — asserts `k2.6`→`kimi_k2_6()`, `k2.7`→`kimi_k2_7()`, fall-through→`kimi_k3()`. Fails on current `master`, passes after this phase (SC-003).

### Implementation for User Story 1

- [x] T003 [US1] Author `pub fn kimi_k2_6() -> &'static str` in `crates/joey-omo/src/agents/prompts/junior.rs`, placed between `kimi_k3()` (line 366) and `kimi_k2_7()` (line 407). The prompt is original content (no upstream K2.6 prompt exists — research.md R1) calibrated for Kimi K2.6: follow the structural template of the existing Kimi variants (identity line naming "Kimi K2.6", execution-vs-orchestration stance, "keep going", scope discipline, verify-before-done, track multi-step work, recover-from-failure sections). K2.6 predates K2.7 — reduce the steerability claims present in `kimi_k2_7()` and emphasize verification rigor. Return a `r#"..."#` raw string literal.
- [x] T004 [US1] Fix the `for_model()` Kimi branch in `crates/joey-omo/src/agents/prompts/junior.rs:459-460`: change the `k2.6`/`k2-6` arm from `kimi_k2_7()` to `kimi_k2_6()`. Do NOT alter the `k2.7`/`k2-7` arm (line 457-458) or the fall-through `kimi_k3()` (line 462).
- [x] T005 [US1] Add regression tests in `crates/joey-omo/src/agents/prompts/junior.rs` (inline `#[cfg(test)]` module) or `crates/joey-omo/tests/` asserting (FR-002, SC-003): (a) ids containing `k2.6` or `k2-6` resolve to `kimi_k2_6()` not `kimi_k2_7()`; (b) ids containing `k2.7` or `k2-7` still resolve to `kimi_k2_7()` (regression guard); (c) a Kimi id matching neither (e.g. `kimi-k3`) falls through to `kimi_k3()`; (d) a non-Kimi id (e.g. `gpt-5.5`) resolves to its family prompt unchanged. Use `std::ptr::eq(for_model("..."), kimi_k2_6())` since the functions return `&'static str`. The pointer-equality assertions on all families (b–d) implicitly guard the byte-equality of existing prompt bodies (data-model.md E3 Public-Surface Contract) — no separate byte-equality test is needed.
- [x] T006 [US1] Verify the `clippy::if_same_then_else` warning at `junior.rs:457` is resolved: run `cargo clippy -p joey-omo` and confirm the "this `if` has identical blocks" warning is gone (US1 Acceptance Scenario 5).

**Checkpoint**: P1 fix complete and independently testable. `cargo test -p joey-omo` green.

---

## Phase 4: User Story 2 - Make the Workspace Clippy-Clean (Priority: P2)

**Goal**: `cargo clippy --workspace -- -D warnings` exits zero. All warnings resolved per the per-class policy (research.md R5). Deviations recorded in plan Complexity Tracking.

**Independent Test**: `cargo clippy --workspace -- -D warnings; echo "exit: $?"` → exit 0 (SC-002).

### Implementation for User Story 2

- [ ] T007 [P] [US2] Resolve 6 clippy warnings in `crates/joey-tools/src/`: `file_tracker.rs:528` (`mem::take`), `highlight.rs:138` (needless borrow `&ref`), `safe_commands.rs:109-110` (manual prefix strip → `str::strip_prefix`), `tools/file_tools.rs:405` (needless borrow), `tools/session_search_tool.rs:108` (needless borrow), and the `i64`→`i64` unnecessary cast. Apply per research.md R5 policy. Run `cargo clippy -p joey-tools` to confirm zero warnings.
- [ ] T008 [P] [US2] Resolve 7 clippy warnings in `crates/joey-agent-core/src/`: `agent.rs:1947,1978,2005` (borrowed expression implements traits — drop redundant `&`), `hooks.rs:146,366,377` (needless borrow / format), `verification.rs:390` (needless borrow or closure). Run `cargo clippy -p joey-agent-core` to confirm zero warnings.
- [ ] T009 [P] [US2] Resolve 6 clippy warnings in `crates/joey-orchestration/src/`: `delegation_tool.rs:319,322` (needless borrow), `manager.rs:184,280`, `subagent.rs:141`, `types.rs:17` (`impl can be derived` — evaluate per-site per research.md R5; if the manual impl matches derived semantics, replace with `#[derive]`; if semantics differ, record as deviation in plan Complexity Tracking and suppress with a justified `#[allow]`). The `too_many_arguments` warnings (9/7, 8/7, 10/7) are deviations — do NOT refactor (record in Complexity Tracking). Run `cargo clippy -p joey-orchestration` to confirm.
- [ ] T010 [P] [US2] Resolve 17 clippy warnings in `crates/joey-omo/src/`: `junior.rs:457` (already fixed by T004 — verify gone), `registry.rs:369` (`too_many_arguments` 11/7 — deviation, record), `categories.rs:300,308,313,318,321` (`std::io::Error::other`, needless borrow), `models.rs:137`, `boulder.rs:20,95`, `goal.rs:18,53` (`impl can be derived` — evaluate per-site), `orchestrator.rs:118,141,418,479,627` (`io::Error::other`, `to_vec`, `push_str`→`push`, `Iterator::last`, `sort_by_key`, needless borrow). Run `cargo clippy -p joey-omo` to confirm zero warnings (excluding recorded deviations).
- [ ] T011 [P] [US2] Resolve 18 clippy warnings in `crates/joey-cli/src/`: `render.rs:247` (`manual_div_ceil` → `.div_ceil()`), `render.rs:31,443,581,802,845,950,973,996,1143` (needless borrow), `repl.rs:146` (`let_and_return`), `repl.rs:156,195,206,1083,1479` (needless borrow, `to_string` in format args, `Iterator::last`), `project_trust.rs:109`, `tui.rs:833` (needless borrow / format). Run `cargo clippy -p joey-cli` to confirm zero warnings.
- [ ] T012 [US2] Final clippy gate: run `cargo clippy --workspace -- -D warnings` and confirm exit 0 (SC-002). Then run `cargo build --workspace && cargo test --workspace` and confirm 0 failures (SC-001 — no public surface changed per FR-004). Record any deviations applied (too_many_arguments, non-derivable impls) in `specs/006-bug-sweep-clippy-panic-hardening/plan.md` Complexity Tracking if not already present.

**Checkpoint**: Workspace is clippy-clean under `-D warnings`. P2 complete and independently testable.

---

## Phase 5: User Story 3 - Harden joey-mcp (Priority: P3, Increment 1/7)

**Goal**: All external-input `.unwrap()`/`.expect()` in `joey-mcp` converted to typed errors or logged fallbacks. This is the **first** P3 increment — it establishes the canonical FR-009 `tracing::warn!` event shape and the FR-006 per-call-site regression-test pattern that later increments replicate (SC-007).

**Independent Test**: `cargo build -p joey-mcp && cargo test -p joey-mcp` green. Malformed MCP JSON-RPC input returns `RequestError::Rpc`, not a panic (SC-004, SC-005).

### Implementation

- [ ] T013 [US3] Harden external-input unwraps in `crates/joey-mcp/src/`: `security.rs:210` (`serde_yaml::from_str(text).unwrap()` — parses MCP server config, external input → propagate `anyhow::Result` with `.context()`), `config.rs:471` (`serde_yaml::from_str(text).unwrap()` — parses config YAML → propagate), `schema.rs:74` (`value.as_object().expect("checked is_object")` — MCP tool schema from server → propagate or fallback). Leave the `regex::Regex::new(...).expect("static regex")` sites (`security.rs:47,55,66`, `config.rs:72`, `result.rs:32`) as tier **safe** — add `// SAFETY:` comments per FR-007. Leave `result.rs:87,88` (serializing a `serde_json::Value`) as safe with SAFETY comment. Leave mutex `.expect("poisoned")` (`lib.rs:394,405`) as internal-but-recoverable — convert to `.unwrap_or_else(|e| tracing::warn!(...))` or propagate per the FR-009 contract in `contracts/error-handling-contract.md`.
- [ ] T014 [US3] Add the canonical FR-009 `tracing::warn!` event to each recovered fallback in `crates/joey-mcp/src/` using the exact shape from `contracts/error-handling-contract.md` §2.1 with `input_kind = "mcp_jsonrpc"`. This establishes the template later increments copy.
- [ ] T015 [US3] Add per-call-site malformed-input regression tests (FR-006, one per hardened site) in `crates/joey-mcp/tests/` or inline `#[cfg(test)]`: feed malformed YAML (missing fields, wrong types) to the config parsers and assert a typed error; feed non-object JSON to the schema parser and assert `RequestError::Rpc` or equivalent. Name each test `<function>_<condition>_returns_error_not_panic`.

**Checkpoint**: `joey-mcp` increment complete. Canonical patterns established.

---

## Phase 6: User Story 3 - Harden joey-gateway (Priority: P3, Increment 2/7)

**Goal**: External-input unwraps in `joey-gateway` (platform messaging message-decode paths) hardened. Follows the patterns established in T014.

**Independent Test**: `cargo build -p joey-gateway && cargo test -p joey-gateway` green.

### Implementation

- [ ] T016 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-gateway/src/` (35 unwraps, 0 expects per audit): focus on message-event decode functions and any JSON/protocol parsing. Classify each via the audit script; convert external-input sites to propagated `anyhow::Result` with `.context()` or logged fallback + FR-009 `warn!` (`input_kind = "gateway_message"` per the vocabulary in `contracts/error-handling-contract.md` §2.2). Add `// SAFETY:` comments to safe-tier retained sites.
- [ ] T017 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-gateway/tests/` for each hardened site.

**Checkpoint**: `joey-gateway` increment complete.

---

## Phase 7: User Story 3 - Harden joey-cron (Priority: P3, Increment 3/7)

**Goal**: External-input unwraps in `joey-cron` (`jobs.json` parsing) hardened.

**Independent Test**: `cargo build -p joey-cron && cargo test -p joey-cron` green. Malformed `jobs.json` returns an error, not a panic.

### Implementation

- [ ] T018 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-cron/src/` (217 unwraps, 2 expects): focus on `jobs.json` load/parse functions and any schedule-expression parsing. Convert to propagated `anyhow::Result` with `.context("jobs.json: ...")` or logged fallback + FR-009 `warn!` (`input_kind = "jobs_json"`). Add `// SAFETY:` comments to safe-tier sites (e.g. cron-expression constants parsed at compile time).
- [ ] T019 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-cron/tests/`: feed corrupt `jobs.json` (missing fields, bad cron expressions, non-UTF-8) and assert typed errors.

**Checkpoint**: `joey-cron` increment complete.

---

## Phase 8: User Story 3 - Harden joey-core (Priority: P3, Increment 4/7)

**Goal**: External-input unwraps in `joey-core` (config, SQLite, auth decode) hardened.

**Independent Test**: `cargo build -p joey-core && cargo test -p joey-core` green. Malformed config/auth/SQLite input returns an error, not a panic.

### Implementation

- [ ] T020 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-core/src/` (167 unwraps, 18 expects): focus on `config.rs` (YAML/env parsing), `auth_store.rs` (credential decode), session-store SQLite row decode, and `lib.rs` path/home operations. Convert to propagated `anyhow::Result` with `.context()` or logged fallback + FR-009 `warn!` (`input_kind` = `"config_file"`, `"auth_store"`, or `"sqlite_row"`). Route user-facing strings through `joey_core::redact::redact_sensitive_text` (FR-008). Add `// SAFETY:` comments to safe-tier sites.
- [ ] T021 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-core/tests/`: feed corrupt config YAML, malformed `.env`, bad auth-store entries, and assert typed errors with no secret leakage.

**Checkpoint**: `joey-core` increment complete.

---

## Phase 9: User Story 3 - Harden joey-providers (Priority: P3, Increment 5/7)

**Goal**: External-input unwraps in `joey-providers` (provider SSE/JSON) hardened.

**Independent Test**: `cargo build -p joey-providers && cargo test -p joey-providers` green. Malformed provider JSON/SSE returns `ProviderError::Parse`, not a panic.

### Implementation

- [ ] T022 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-providers/src/` (93 unwraps, 1 expect): focus on SSE stream-chunk parsing, chat-completion JSON body decoding, and response field extraction. Convert to `ProviderError::Parse(sanitized)` (include field name, not raw JSON — `contracts/error-handling-contract.md` §1.2) or logged fallback + FR-009 `warn!` (`input_kind` = `"provider_json"` or `"provider_sse"`). Add `// SAFETY:` comments to safe-tier sites.
- [ ] T023 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-providers/tests/`: feed truncated SSE chunks, JSON missing required fields (`content`, `choices`, `message`), wrong-type values, and assert `ProviderError::Parse` (US3 Acceptance Scenario 1).

**Checkpoint**: `joey-providers` increment complete.

---

## Phase 10: User Story 3 - Harden joey-tools (Priority: P3, Increment 6/7)

**Goal**: External-input unwraps in `joey-tools` (tool input, sanitize, lsp, file-read results) hardened. Largest surface (244 unwraps).

**Independent Test**: `cargo build -p joey-tools && cargo test -p joey-tools` green.

### Implementation

- [ ] T024 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-tools/src/` (244 unwraps, 4 expects): focus on `sanitize*.rs`, `lsp.rs` (LSP message decode → `LspError`), tool-result JSON parsing, and `safe_commands.rs`. Convert to propagated errors or logged fallback + FR-009 `warn!` (`input_kind` = `"context_file"` or tool-input value). Add `// SAFETY:` comments to safe-tier sites. This is the largest increment — work methodically by module.
- [ ] T025 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-tools/tests/`: feed malformed tool arguments, corrupt LSP messages, non-UTF-8 file content, and assert typed errors (US3 Acceptance Scenario 2).

**Checkpoint**: `joey-tools` increment complete.

---

## Phase 11: User Story 3 - Harden joey-agent-core (Priority: P3, Increment 7/7)

**Goal**: External-input unwraps in `joey-agent-core` (turn-loop provider/model JSON decode) hardened — external-input paths only.

**Independent Test**: `cargo build -p joey-agent-core && cargo test -p joey-agent-core` green.

### Implementation

- [ ] T026 [US3] Enumerate and harden external-input `.unwrap()`/`.expect()` sites in `crates/joey-agent-core/src/` (136 unwraps, 17 expects): focus ONLY on the turn-loop paths that decode provider/model JSON responses and tool-call arguments (the curated function allowlist from research.md R4). Convert to propagated `anyhow::Result` with `.context("provider/model decode: ...")` or logged fallback + FR-009 `warn!` (`input_kind` = `"provider_json"`). Leave internal control-flow unwraps as safe-tier with `// SAFETY:` comments. This is the final increment — the FR-009/FR-006 patterns from T014 are well-established by now.
- [ ] T027 [US3] Add per-call-site malformed-input regression tests (FR-006) in `crates/joey-agent-core/tests/`: feed malformed provider responses into the turn loop and assert graceful error handling, not panic.

**Checkpoint**: All 7 P3 crate increments complete.

---

## Phase 12: Polish & Cross-Cutting Concerns

**Purpose**: Final verification gates and documentation.

- [ ] T028 Run `scripts/audit-external-input-unwraps.sh` and confirm exit 0 (SC-004 — zero external-input unwraps remaining, or every remaining site has a `// SAFETY:` comment). Record the before→after count comparison (e.g. "joey-mcp: 3 → 0, joey-tools: N → 0, ...").
- [ ] T029 Run `cargo clippy --workspace -- -D warnings` (SC-002) and `cargo build --workspace && cargo test --workspace` (SC-001) — confirm both green. Record final test count.
- [ ] T030 Run the quickstart.md validation scenarios (A–F) and confirm all pass.
- [ ] T031 Update `PORTING.md` with the bug-sweep status (the k2.6 prompt-variant correction and the hardening pass), per the AGENTS.md convention that PORTING.md is a living audit document updated when upstream-parity work changes.
- [ ] T032 [P] Verify no public surface changed: confirm `SCHEMA_VERSION` is still 22, no new CLI flags, no config keys renamed, no trait signatures changed (FR-004, SC-006). Cross-check against the Public-Surface Contract in `data-model.md` §E3.

**Checkpoint**: Feature complete.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies. Creates the audit script all later measurement depends on.
- **Phase 2 (Foundational)**: Depends on Phase 1 (uses the audit script for baseline).
- **Phase 3 (US1/P1)**: Depends on Phase 2. Independent of P2/P3.
- **Phase 4 (US2/P2)**: Depends on Phase 2. Independent of P1/P3. Can run in parallel with Phase 3 (different files — P1 touches `junior.rs`, P2 touches the other clippy sites; the one overlap is `junior.rs:457` which P1 resolves, so P2's joey-omo task T010 just verifies it's gone).
- **Phase 5 (US3/joey-mcp)**: Depends on Phase 2. Establishes canonical FR-009/FR-006 patterns — MUST complete before Phases 6–11.
- **Phase 6–11 (US3/other crates)**: Each depends on Phase 5 (for the established patterns). They are otherwise independent of each other and could run in parallel if staffed, but the spec's SC-007 ordering (ascending risk) is the recommended sequence.
- **Phase 12 (Polish)**: Depends on all prior phases.

### User Story Dependencies

- **US1 (P1)**: No dependencies on other stories. Can start after baseline.
- **US2 (P2)**: No dependencies on other stories. The `if_same_then_else` warning at `junior.rs:457` is resolved by US1 (T004), but P2's joey-omo task (T010) only needs to verify it's gone — no hard dependency.
- **US3 (P3)**: No dependencies on US1/US2 for correctness, but SC-007 mandates the `joey-mcp` increment first to establish patterns.

### Parallel Opportunities

- T007–T011 (P2 per-crate clippy cleanup) are all marked `[P]` — different crates, no file conflicts.
- T016–T027 (P3 crate increments) are independent once T014 establishes the FR-009 pattern — a team could run them in parallel if desired (SC-007 ordering is a recommendation, not a hard dependency chain).
- T031 (PORTING.md) and T032 (public-surface audit) in the polish phase are marked `[P]`.

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: create the audit script.
2. Complete Phase 2: capture baseline.
3. Complete Phase 3: the Kimi k2.6 fix (correctness).
4. **STOP and VALIDATE**: `cargo test -p joey-omo kimi` passes. The user-visible correctness bug is fixed. Ship if only the P1 fix is wanted.

### Incremental Delivery

1. Setup + baseline → measurement tooling ready.
2. US1 (P1) → correctness fix shipped, independently testable.
3. US2 (P2) → clippy-clean baseline, independently testable.
4. US3 (P3) → 7 per-crate hardening increments, each independently shippable/testable per SC-007.
5. Polish → final gates green, docs updated.

### Parallel Team Strategy

With multiple developers:
1. Team completes Setup + Foundational together.
2. Developer A: US1 (P1, small surface).
3. Developer B: US2 (P2, mechanical clippy cleanup across 5 crates).
4. Once `joey-mcp` increment lands (canonical patterns): Developers C/D/E take subsequent P3 crate increments in parallel.

---

## Notes

- [P] tasks = different files/crates, no dependencies.
- [Story] label maps task to its user story for traceability.
- Each user story (and each P3 crate increment) is independently completable and testable.
- P1 task T003 (author `kimi_k2_6()`) is the only task requiring original content authoring — all others are mechanical edits following established patterns.
- The clippy warning count (~59–77) and unwrap counts in task descriptions are from the audit at plan time; the implementer re-measures at execution time via the audit script (FR-010) and `cargo clippy`.
- Commit after each task or logical group. Each P3 crate increment is a natural commit boundary.
