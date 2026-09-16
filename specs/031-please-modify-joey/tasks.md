# Tasks: Goal-Directed Task Execution (Feature 031)

**Generated**: 2026-09-15 | **Plan**: [plan.md](plan.md) | **Spec**: [spec.md](spec.md)

## Implementation Strategy

MVP is User Story 1 (Phase 3): after it lands, the agent plans before acting — independently valuable. Stories 2-4 build on the same guidance constant and are sequenced by priority. The baseline bundle (Phases 1-2) MUST be captured before any source edit so the "before" transcripts reflect current behavior. Constitution VII: every guidance-wording task ships with its lockstep test update in the same task; `cargo build --workspace` + `cargo test --workspace` stay green at every phase boundary.

## Dependencies

```text
Phase 1 (Setup) ──> Phase 2 (Foundational: baseline capture) ──> Phase 3 (US1) ──> Phase 4 (US2) ──> Phase 5 (US3) ──> Phase 6 (US4) ──> Phase 7 (Polish)
```

US phases are sequenced because they edit the same constants in crates/joey-agent-core/src/guidance.rs (exclusive write set); US4 (de-brand) touches disjoint files in part and is ordered last so parity tests stabilize first.

## Phase 1 — Setup

- [x] T001 Create baseline task manifest with 6 representative multi-step tasks (2 coding, 2 analysis, 1 writing, 1 multi-file refactor) in specs/031-please-modify-joey/baseline/manifest.md
- [x] T002 Create baseline scoring rubric (action count, on-plan-action ratio, completion-report presence, blocker-statement presence; a "visible action" = one tool invocation or one assistant message, counted separately and summed) in specs/031-please-modify-joey/baseline/rubric.md

## Phase 2 — Foundational (blocking prerequisite)

- [x] T003 Capture current-version transcripts for all 6 manifest tasks against an unmodified build via fresh one-shot CLI sessions exported from the session store to Markdown; store as specs/031-please-modify-joey/baseline/transcripts/task-N.md and score with baseline/rubric.md; also capture the assembled system prompt from pinned deterministic inputs to specs/031-please-modify-joey/baseline/pre-change-prompt.txt (SC-007 pre-change fixture); MUST run before any source edit (SC-001/SC-003/SC-004 baseline)

## Phase 3 — User Story 1: Plan Before Action (P1)

**Goal**: agent states a concrete ordered plan before non-trivial work. **Independent test**: `cargo test -p joey-agent-core --test parity` green (goal-directed on/off pair) + transcript shows plan-first on a manifest task.

- [x] T004 [US1] Add `pub const GOAL_DIRECTED_GUIDANCE` with plan-before-action framing (state concrete ordered plan of verifiable steps before non-trivial work; proportionality: single-step tasks act directly; at most one clarification round when no reasonable default exists) in crates/joey-agent-core/src/guidance.rs, with matches-contract unit test pinning exact wording
- [x] T005 [US1] Add config key `agent.goal_directed_guidance` (boolean, default true) resolution in crates/joey-agent-core/src/prompt.rs following the agent.adaptive_coding_guidance pattern, with default-resolution test
- [x] T006 [US1] Insert GOAL_DIRECTED_GUIDANCE into the stable tier of build_system_prompt immediately after TASK_COMPLETION_GUIDANCE (gated: key true AND tools loaded) in crates/joey-agent-core/src/prompt.rs; update golden/section-order test (prompt.rs:1114-1130) with the new section position
- [x] T007 [US1] Add on/off parity test pair in crates/joey-agent-core/tests/parity.rs: key=true prompt contains GOAL_DIRECTED_GUIDANCE; key=false prompt byte-identical to pre-feature prompt (mirror adaptive-coding pattern at parity.rs:536-539)

## Phase 4 — User Story 2: Execution Without Deviation (P2)

**Goal**: actions serve the current plan step; no re-derivation; per-step completion report. **Independent test**: `cargo test -p joey-agent-core --lib guidance` green with reworded pins + manifest-task transcript shows straight-line execution.

- [x] T008 [US2] Extend GOAL_DIRECTED_GUIDANCE in crates/joey-agent-core/src/guidance.rs with execution-discipline clauses (work steps in order; every action serves the current step; reuse established results; re-state plan state at step transitions to survive context compression); update its matches-contract pin in the same edit
- [x] T009 [US2] Reword TASK_COMPLETION_GUIDANCE in crates/joey-agent-core/src/guidance.rs from exploration-permissive to plan-execution-report framing (keep no-silent-partial-completion semantics); update pinned assertions in prompt.rs tests referencing its phrases (prompt.rs:1084-1086)
- [x] T010 [US2] Record the deliberate divergence from verbatim upstream parity for the reworded TASK_COMPLETION_GUIDANCE and new GOAL_DIRECTED_GUIDANCE in PORTING.md (Deliberate-deviation entry with date and feature reference)

## Phase 5 — User Story 3: Explicit, Bounded Plan Revision (P3)

**Goal**: failures/blockers trigger explicit revision or stop-with-status, never silent drift or retry loops. **Independent test**: `cargo test -p joey-agent-core --lib guidance` green with final contract pin + blocked-task transcript ends with explicit blocker statement.

- [x] T011 [US3] Extend GOAL_DIRECTED_GUIDANCE in crates/joey-agent-core/src/guidance.rs with revision clauses (on step failure/blocker: stop, report evidence, state plan impact, present revised plan or stop with completed/blocked/why status; departures from stated plan are explicit revisions — the only sanctioned deviation; user mid-task instructions enter as revisions); finalize its matches-contract pin
- [x] T012 [US3] Add transcript-level guidance check that every completed plan ends with per-step outcome + verification report and every blocked plan ends with an explicit blocker statement (extension of the matches-contract test asserting the reporting clauses exist) in crates/joey-agent-core/src/guidance.rs #[cfg(test)]

## Phase 6 — User Story 4: Consistent Agent Identity (P4)

**Goal**: zero predecessor-brand references in model- and user-facing text. **Independent test**: `cargo test -p joey-agent-core --lib guidance` no-brand test green with zero allowlist + `cargo test -p joey-tui render` green.

- [x] T013 [P] [US4] Remove the "based on Hermes Agent by Nous Research" attribution and hermes-agent.nousresearch.com docs URL from AGENT_HELP_GUIDANCE in crates/joey-agent-core/src/guidance.rs (capability text stays); update prompt.rs:1116 section-order key to the de-branded line
- [x] T014 [P] [US4] De-brand DEFAULT_SOUL_MD persona text in crates/joey-core/src/default_soul.rs; keep legacy-template detection literals functional; update identity_matches_seeded_soul pin (guidance.rs:344-347) and default_soul tests
- [x] T015 [P] [US4] Remove the "· based on Hermes Agent by Nous Research" banner line in crates/joey-tui/src/render.rs; update any banner assertion in joey-tui tests
- [x] T016 [US4] Tighten no_hermes_branding_in_model_visible_text (crates/joey-agent-core/src/guidance.rs:300-325) to zero-allowlist: remove the two attribution replace() exemptions, assert zero case-insensitive 'hermes' across all model-visible guidance constants

## Phase 7 — Polish & Cross-Cutting

- [x] T017 Add token-neutrality test in crates/joey-agent-core/tests/parity.rs: assert estimate_tokens(post-change assembly from pinned fixtures) <= estimate_tokens(committed pre-change fixture at specs/031-please-modify-joey/baseline/pre-change-prompt.txt) via joey_core::utils::estimate_tokens (SC-007)
- [x] T018 Re-run the 6 manifest tasks against the changed build; capture post-change transcripts to specs/031-please-modify-joey/baseline/transcripts-post/; score with rubric.md; verify SC-001 (>=30% fewer actions), SC-003 (>=90% on-plan), SC-004 (no success regression), SC-002/SC-005 (plan-first + completion reports)
- [x] T019 Run full gate: cargo build --workspace && cargo test --workspace (both must be green); record results in specs/031-please-modify-joey/baseline/verification.md
- [x] T020 Execute quickstart.md Scenarios 1-4 and record outcomes in specs/031-please-modify-joey/baseline/verification.md

## Parallel Execution Examples

- Phase 6: T013, T014, T015 touch disjoint files (guidance.rs vs default_soul.rs vs render.rs) and can run in parallel; T016 depends on T013 (same file) and follows it.
- Phase 1: T001 and T002 are independent file creations (parallelizable).
- All other phases are sequential due to the shared guidance.rs write set.

## Phase 8: Convergence

- [ ] T021 Re-run the full acceptance gate `cargo build --workspace && cargo test --workspace` to green on a quiet host — all four 2026-09-16 attempts were SIGKILLed by host memory pressure before any test executed (build was green; every targeted run that executed passed), and `system_prompt_token_neutrality_sc007` has never executed — then record the green gate result (including the SC-007 test) in specs/031-please-modify-joey/baseline/verification.md per Constitution VII (contradicts)
- [x] T022 Re-capture the six baseline manifest tasks post-change with the model pinned to the pre-change capture's model and provider (`./target/debug/joey -z "<prompt>" -m gpt-5.6-sol --provider ai-usage-hud`) to remove the documented gpt-5.6-sol→glm-5.3 config-default drift between captures, re-score with baseline/rubric.md, update baseline/scores-post.md, and record honest SC-001/SC-002/SC-003 verdicts (current confounded capture: SC-001/SC-002/SC-003 FAIL, SC-004/SC-005 PASS) per SC-001, SC-002, SC-003 (partial)
- [x] T023 Review the working-tree modifications outside feature-031's artifact scope (AGENTS.md, README.md, docs/* — modified concurrently with implementation by no T001–T020 task) and either attribute them to a separate change set or exclude them from the feature-031 branch before opening a PR per plan.md file-change map (unrequested)

## Phase 9: Convergence

- [x] T024 Eliminate redundant post-report verification re-fires that inflate SC-001/SC-003 counts: both pinned and unpinned post-change captures show the verify-on-stop nudge (`agent.retrieval_verification_nudge`, `build_verify_on_stop_nudge` in crates/joey-agent-core) re-firing AFTER a compliant per-step completion report (two extra verification rounds on tasks 1/2/6, plus todo bookkeeping calls, all off-plan per rubric); make the nudge not re-fire once a session has delivered a completion report satisfying SC-005 (per-step outcomes + verification stated), with lockstep regression tests, then re-run the six manifest tasks pinned (`-m gpt-5.6-sol --provider ai-usage-hud`), re-score with baseline/rubric.md, and append the re-capture verdicts to baseline/scores-post.md per SC-001, SC-003 (partial)

## Phase 10: Convergence

- [x] T025 Update the feature-031 PR inclusion list in specs/031-please-modify-joey/baseline/verification.md (T023 section) to include the eighth changed file crates/joey-agent-core/src/agent.rs (T024 nudge-suppression fix) alongside the existing seven, so PR staging is not misled (partial)

## Phase 11: Convergence

- [x] T026 Update specs/031-please-modify-joey/baseline/verification.md T021 record with the direct-execution evidence: SC-007 parity test executed GREEN (parity 9/9 incl. system_prompt_token_neutrality_sc007), lib suite 276/276 green (incl. T024 nudge-suppression tests), memory_injection 7/7, rag_config_key_parity 3/3 — all in paced direct binary runs on 2026-09-16; the "never-run SC-007" phrasing in the Phase-10 closeout is stale and must be corrected so the record reflects that every feature-031 code artifact is test-verified in real runs and only the single-command workspace monolith remains env-blocked (stale-doc)
