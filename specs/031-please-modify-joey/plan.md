# Implementation Plan: Goal-Directed Task Execution

**Branch**: `031-please-modify-joey` | **Date**: 2026-09-15 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/031-please-modify-joey/spec.md`

## Summary

Reshape the primary interactive agent from open-ended exploration into goal-directed execution through model-facing guidance changes in `joey-agent-core`: (1) new goal-directed guidance (plan before action; execute steps in order without deviation; explicit plan revision on blocker/failure; per-step completion reporting), replacing exploration-flavored wording; (2) removal of predecessor-brand references from model- and user-facing text (identity constant, guidance help line, default persona, TUI banner); (3) config key `agent.goal_directed_guidance` (default true) gating the new guidance, following the existing guidance-gating pattern; (4) a token-neutral prompt-size budget (SC-007) verified via `joey_core::utils::estimate_tokens` in a deterministic test; (5) regression coverage strengthening the existing no-predecessor-brand test and the system-prompt golden test, plus a committed baseline bundle (5-10 representative tasks with captured current-version transcripts) for before/after comparison.

## Technical Context

**Language/Version**: Rust 2021 edition, stable toolchain (per `rust-toolchain.toml`)

**Primary Dependencies**: existing workspace crates only — `joey-agent-core` (guidance constants, `build_system_prompt`), `joey-core` (config, `utils::estimate_tokens`, default persona), `joey-tui` (banner), `joey-cli` (integration). No new dependencies.

**Storage**: none new — the Task Plan is session-transient visible text by spec clarification Q1; no on-disk plan state, no schema changes.

**Testing**: `cargo test` per crate; prompt-text parity tests in `crates/joey-agent-core/tests/parity.rs` and `#[cfg(test)]` modules in `guidance.rs`/`prompt.rs`/`agent.rs`; TUI banner string assertions in `joey-tui`.

**Target Platform**: existing cross-platform targets (macOS/Linux/Windows); no platform-specific code introduced.

**Project Type**: library + CLI/TUI agent (existing crates; feature is additive within `joey-agent-core`, `joey-core`, `joey-tui`)

**Performance Goals**: SC-007 token-neutrality — assembled model-facing instruction text after the change is no larger (estimated tokens) than before; verified in a deterministic test using `joey_core::utils::estimate_tokens`.

**Constraints**: verbatim-parity constraint on ported guidance (AGENTS.md): rewording `TASK_COMPLETION_GUIDANCE` and removing the two attribution strings is a deliberate divergence to be recorded in `PORTING.md`; system prompt built once per session (cache-warmth) — the design adds one stable-tier section, no per-turn re-render; sanitization/threat-scan layers untouched.

**Scale/Scope**: ~4 source files edited (`guidance.rs`, `prompt.rs`, `default_soul.rs`, `render.rs`) + tests; ~2-3 tests added, ~3 existing tests updated; baseline bundle artifacts under `specs/031-please-modify-joey/baseline/`.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Principle | Status | Notes |
|---|-----------|--------|-------|
| 1 | 0. Cross-Platform Compatibility | PASS | No platform-specific code; text-only edits in cross-platform files. |
| 2 | I. Workspace-First Rust | PASS | All edits inside existing crates; no workspace-root code. |
| 3 | II. CLI/TUI Parity | PASS | TUI banner de-brand applies to the shared string; CLI text unchanged except banner constant consumer. |
| 4 | III. Filesystem Source of Truth | N/A | No spec-kit UI work. |
| 5 | IV. Test-First for New Crates | PASS | Tests updated alongside; contract tests pin exact new wording; regression coverage mandated (Principle VII). |
| 6 | V. Incremental Delivery | PASS | Three independently shippable increments: (a) de-brand, (b) goal-directed guidance + config gate, (c) baseline bundle + docs. |
| 7 | VI. Modularity and Decoupling | PASS | Guidance constants keep the existing pub-const interface; no new inter-crate coupling. |
| 8 | VII. Backward Compatibility & Non-Regression | PASS w/ note | Config key is additive (default true restores old behavior when off); existing behavior preserved when guidance disabled. Prompt text changes are the feature itself, sanctioned by spec FR-008/FR-011; regression tests updated to pin NEW wording while asserting capability preservation (tools/skills/modes untouched). |
| 9 | VIII. Performance Discipline | PASS | SC-007 token-neutrality budget; zero new dependencies; zero per-turn overhead. |

No gate failures. Complexity Tracking section intentionally empty.

## Project Structure

### Documentation (this feature)

```text
specs/031-please-modify-joey/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/
│   ├── config-key.md    # agent.goal_directed_guidance contract
│   └── system-prompt.md # assembled-prompt surface changes
├── baseline/            # committed baseline bundle (implementation phase)
└── tasks.md             # Phase 2 output (/speckit-tasks - not created here)
```

### Source Code (repository root)

```text
crates/joey-agent-core/src/
├── guidance.rs          # reword/replace TASK_COMPLETION_GUIDANCE; add GOAL_DIRECTED_GUIDANCE; de-brand AGENT_HELP_GUIDANCE
├── prompt.rs            # insert GOAL_DIRECTED_GUIDANCE into stable tier; update golden/section-order tests
└── tests/parity.rs      # goal-directed parity + token-neutrality tests
crates/joey-core/src/
├── default_soul.rs      # de-brand DEFAULT_SOUL_MD
└── (utils.rs            # unchanged; estimate_tokens reused)
crates/joey-tui/src/
└── render.rs            # de-brand TUI banner line
```

**Structure Decision**: single-workspace additive edits across `joey-core`, `joey-agent-core`, `joey-tui`, per the structure tree above; no new crates, no new top-level directories.

## Complexity Tracking

> No Constitution Check violations — section intentionally empty.
