# Implementation Plan: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Branch**: `008-cli-crush-format-parity` | **Date**: 2026-07-30 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/008-cli-crush-format-parity/spec.md`

## Summary

Port the TUI's crush-style block layout (spec 007) to the CLI streaming
renderer in `crates/joey-cli/src/render.rs`, always in fully-expanded form:
the CLI has no expand/collapse interaction, so every block renders the
equivalent of the TUI's full-expand state. Three block types: (1) reasoning
box — bordered `┌─ reasoning` region with full content and `└─ Thought for
{:.1}s` footer; (2) terminal-command block — `$ command` header with
`(exit N)` badge, duration, and full output; (3) generic tool-call header —
status icon + emoji + bold name + param + duration, with full indented
result body. No `AgentEvent`, `TranscriptItem`, or public-surface change;
no new dependencies; presentation-only change confined to `render.rs`.

## Technical Context

**Language/Version**: Rust (2021 edition, stable channel per `rust-toolchain.toml`)

**Primary Dependencies**: Existing only — `joey_core::theme::{self, Theme}`
(Theme::pantera, gradient helpers), `joey_agent_core::events::AgentEvent`,
`crossterm` (cursor control for in-place tool-line rewrite), `terminal_size`
(term width detection). No new dependencies introduced (Principle VIII).

**Storage**: N/A — pure presentation layer, no persistence.

**Testing**: `cargo test -p joey-cli` (inline `#[cfg(test)] mod tests` in
`render.rs`). Tests construct synthetic `AgentEvent` streams, push them
through `render_turn`, and assert on output. Existing test helper
`opts_for(Capability)` builds `RenderOptions` for Full/Reduced/NonInteractive
profiles; `run_synthetic_turn` pushes a minimal event stream.

**Target Platform**: Same as existing CLI — Linux/macOS terminal, piped
stdout (NonInteractive), one-shot `joey -z` and REPL streaming modes.

**Project Type**: CLI presentation layer (crate `joey-cli`).

**Performance Goals**: The renderer is a streaming `println!`-based loop.
New work per block-close is O(content_lines) for printing — same order as
existing. The reasoning footer duration derivation is O(1) (two `Instant`
comparisons). No new allocation beyond formatting header strings. No
measurable impact on steady-state streaming latency.

**Constraints**: Strictly additive to presentation only (Principle VII).
No `AgentEvent` variant, field, or semantics change (FR-012). No new color
constants or theme fields (FR-010, SC-004). No new dependencies (Principle
VIII). `cargo build --workspace` and `cargo test --workspace` MUST stay
green. All existing gates (`--quiet`, `display.show_reasoning`,
`tool_progress`) MUST remain functional (FR-011).

**Scale/Scope**: Single file (`render.rs`, ~1900 LOC). Three event arms
modified (`ReasoningDelta`, `ToolStart`, `ToolEnd`), one closure modified
(`close_reasoning`), one new local variable (`reasoning_started:
Option<Instant>`), one new helper function (`is_terminal_block` — or import
from `joey_tui::state`). Estimated ~200-300 LOC of changes.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|---|---|---|
| I. Workspace-First Rust | ✅ PASS | Change confined to existing crate `joey-cli`. No new crate. |
| II. CLI/TUI Parity | ✅ PASS | This feature IS the parity-in-reverse: bringing TUI layout to CLI. CLI and TUI continue to consume the same `AgentEvent` stream (FR-012). The TUI's expand/collapse interaction is unchanged (FR-014). |
| III. Filesystem is Source of Truth | ✅ PASS | N/A — no spec-kit UI artifacts involved. No on-disk format changes. |
| IV. Test-First for New Crates | ✅ PASS | No new crate. Tests are added alongside implementation in `render.rs` `#[cfg(test)] mod tests`. The `is_terminal_block` classification logic is tested identically to the TUI's test (007 T020). |
| V. Incremental, Reviewable Delivery | ✅ PASS | Three user stories (P1/P2/P3) are independently shippable. Each maps to one block type and can be verified in isolation. |
| VI. Modularity and Decoupling | ✅ PASS | All changes are within `render.rs`. The `is_terminal_block` classification may either be defined locally (matching the TUI's `joey_tui::state::is_terminal_block`) or imported — but importing from `joey_tui` would create an undesirable downward dependency from `joey-cli` on `joey-tui`. A local function is the correct choice (the TUI's version is a one-liner: `name == "terminal"`). No shared module threading. |
| VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE) | ✅ PASS | No `AgentEvent` variant/field change (FR-012). No `TranscriptItem` change. No public API, CLI flag, config key, or on-disk format change. All existing gates preserved (FR-011). Existing animation machinery (spinner, caret, tool-line rewrite, markdown reflow, banner) untouched. Regression tests MUST be added (tasks.md). |
| VIII. Performance Discipline and Lean Code | ✅ PASS | No new dependency. No new allocation pattern. Footer duration is O(1). No speculative abstraction — changes are inline modifications to existing event arms, following the existing local-state pattern (`reasoning_line_count`, `active_tool`). |

**Gate Result**: PASS — no violations. No Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
specs/008-cli-crush-format-parity/
├── plan.md              # This file
├── research.md          # Phase 0 output — design decisions
├── data-model.md        # Phase 1 output — streaming state + event data
├── quickstart.md        # Phase 1 output — validation guide
├── contracts/
│   └── cli-block-layout.md  # Phase 1 output — CLI render layout contract
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source Code (repository root)

```text
crates/joey-cli/
└── src/
    └── render.rs        # ALL changes confined to this single file
```

**Structure Decision**: Single-file scope. The entire feature is a
presentation-layer change in `crates/joey-cli/src/render.rs`. No other
crate is touched. No new modules, no new files, no new dependencies. The
`is_terminal_block` classification function is added as a private function
within `render.rs` (1 line, matching `joey_tui::state::is_terminal_block`
exactly — duplicated rather than imported to avoid a `joey-cli → joey-tui`
dependency edge, which would violate the workspace DAG).

## Complexity Tracking

> No Constitution Check violations. This section is intentionally empty.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| (none) | — | — |
