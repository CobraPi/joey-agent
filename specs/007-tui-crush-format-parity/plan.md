# Implementation Plan: Crush-Style Expandable Block Formatting (TUI)

**Branch**: `007-tui-crush-format-parity` | **Date**: 2026-07-29 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/007-tui-crush-format-parity/spec.md`

## Summary

Bring the three expandable transcript block types in joey-agent's interactive TUI
to crush's exact layout/composition, while keeping joey-agent's aurora-synthwave
`theme.rs` palette. Concretely: (1) render reasoning in a bordered box with a
three-state windowed view and a derived `Thought for Ns` footer; (2) add a
distinct terminal-command block type (`$ command` prompt, output body,
`(exit N)` badge, running spinner) that today does not exist; (3) upgrade
the generic tool-call header to crush's icon + bold name + primary-parameter
layout with an indented, bounded result body and hidden-line affordances.

A single additive data-plumbing change backs all three: `AgentEvent::ToolEnd`
gains a typed `exit_code: Option<i64>` field and the tool-execution path begins
populating the tool item's `full_args`/`full_result` (today both are always
`None`). Both the TUI and the one-shot CLI consume the same extended event
(constitution Principle II). Mouse-click-to-toggle is added alongside the
existing Ctrl+E / Ctrl+G keyboard bindings.

## Technical Context

**Language/Version**: Rust 2021 edition (stable, matching `rust-toolchain.toml`).

**Primary Dependencies**: `ratatui` + `crossterm` (existing `joey-tui`); `serde_json`
(existing, for the typed exit-code parse inside the tool layer only — no new
crate). No new runtime dependencies are introduced (see `research.md` §4 for the
markdown-in-thinking decision, resolved as "ship plain text in v1").

**Storage**: N/A — pure in-memory TUI state. No on-disk format changes.

**Testing**: `cargo test -p joey-tui`, `cargo test -p joey-agent-core`,
`cargo test -p joey-cli`. New unit tests for the three-state footer-duration
derivation, the terminal-block classification, and the affordance string
formatting. Regression tests asserting feature-005 `AgentEvent::ToolEnd`
construction sites still compile and pass (Principle VII).

**Target Platform**: Native terminal (macOS/Linux), existing surfaces.

**Project Type**: CLI/TUI (extension of existing `joey-tui` + `joey-cli`).

**Performance Goals**: Steady-state frame budget unchanged (the TUI already
frame-batches). Per-item render of a collapsed block ≤ the cost of the current
tool/reasoning render. Expanded terminal-command block with very long output
must not regress the transcript scroll region — the bounded-window cap
(reusing the feature-005 `MAX_*` constants) keeps rendered height O(1) in the
collapsed state and O(visible) when expanded, never O(total output).

**Constraints**: Strictly additive (constitution Principle VII, NON-NEGOTIABLE).
No change to `theme.rs` palette or `Theme` struct fields (FR-014). No new
crate. No new runtime dependency. `cargo build --workspace` and
`cargo test --workspace` must stay green on every increment.

**Scale/Scope**: Three block render paths in `joey-tui/src/widgets.rs`; one
enum-variant field addition in `joey-agent-core/src/events.rs`; two
`ToolEnd` construction sites in `joey-agent-core/src/agent.rs`; state-model
additions in `joey-tui/src/state.rs`; one CLI render branch update in
`joey-cli/src/render.rs`; mouse-click routing in `joey-tui/src/app.rs`.
~6 source files touched, all narrow.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Principle | Status | Evidence |
|---|-----------|--------|----------|
| I | Workspace-First Rust | ✅ PASS | All work is inside existing crates (`joey-tui`, `joey-agent-core`, `joey-cli`). No new crate; no code at workspace root. |
| II | CLI/TUI Parity | ✅ PASS | The additive `exit_code` field and the `full_args`/`full_result` population flow to BOTH the TUI state machine and the CLI `render.rs` via the same `AgentEvent::ToolEnd` variant. The crush layout (boxes, affordances, click) is explicitly scoped to the interactive TUI; the non-interactive CLI continues to emit fully-expanded plain text (FR-016). See `contracts/agent-event.md`. |
| III | FS Is Source of Truth (NON-NEGOTIABLE) | ✅ N/A | No spec-kit file visualization/editing involved. |
| IV | Test-First for New Crates | ✅ N/A | No new crate. New logic in existing crates ships with unit tests (state derivation, classification, affordance formatting). |
| V | Incremental, Reviewable Delivery | ✅ PASS | Decomposed into three independently-shippable user-story slices (P1 boxed thinking → P2 terminal block → P3 tool header). Each builds and tests on its own. |
| VI | Modularity and Decoupling | ✅ PASS | No new cross-crate coupling. The additive `ToolEnd` field is consumed through the existing event stream; rendering stays a pure function of `(item, width, interaction_mode)` — no global state threaded through shared paths. The terminal-block classification is a pure function on the tool name. |
| VII | Backward Compatibility / Non-Regression (NON-NEGOTIABLE) | ✅ PASS | The `ToolEnd` extension is an additive struct field with a `None`/empty default; existing construction sites and all feature-005 tests compile and pass unchanged except where they explicitly opt into the new field (research.md §1 documents the exact migration). Existing keybindings (Ctrl+E/Ctrl+G) are preserved; click is additive. Regression-coverage tasks mandated in `tasks.md`. |
| VIII | Performance Discipline / Lean Code | ✅ PASS | No new dependency. The only new per-frame work is bordered-box drawing for reasoning (a ratatui `Block`, O(visible lines)). Click hit-testing is O(transcript items) per click (amortized O(1) — clicks are human-paced, not per-frame; reuses scroll's line accounting). Hot-path budget noted in `research.md` §6. Plain-text thinking body avoids a glamour/markdown dependency in v1 (research.md §4). |

**Additional Constraints**: Rust 2021 ✅; UI rendering reuses `joey-tui` (no new stack) ✅;
no new runtime dependency (markdown-in-thinking deferred, research.md §4) ✅;
`ToolEnd` is a public trait/enum surface — change is additive with a default,
gated by regression coverage (research.md §1) ✅.

**Gate result**: PASS — no violations. Complexity Tracking section intentionally empty.

## Project Structure

### Documentation (this feature)

```text
specs/007-tui-crush-format-parity/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── agent-event.md   # Additive ToolEnd.exit_code + full_args/full_result plumbing
│   └── block-layout.md  # The three block render contracts (reasoning box, terminal, tool)
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-agent-core/
│   └── src/
│       ├── events.rs    # ADD: exit_code: Option<i64> to AgentEvent::ToolEnd
│       └── agent.rs     # UPDATE: 2 ToolEnd construction sites to populate exit_code
├── joey-tui/
│   └── src/
│       ├── state.rs     # ADD: TerminalBlock fields on TranscriptItem::Tool;
│       │                #      reasoning first-delta timestamp + duration; click toggle
│       ├── widgets.rs   # UPDATE: item_lines() reasoning box + terminal block + tool header
│       └── app.rs       # UPDATE: mouse-click → focus + toggle routing
└── joey-cli/
    └── src/
        └── render.rs    # UPDATE: ToolEnd branch consumes exit_code for plain-text parity
```

**Structure Decision**: Single-workspace extension across three existing crates,
following the dependency DAG (`joey-agent-core` → `joey-tui`/`joey-cli`). The
event-variant change lives in the lowest crate (`joey-agent-core::events`); the
renderers consume it downstream. No new crate is warranted — the change is
narrow, fits existing module boundaries, and adding a crate would violate
constitution Principle VIII (YAGNI). The terminal-command block is modeled as
a *presentation mode* of the existing `TranscriptItem::Tool` variant (a
classification flag + render branch), not a new enum variant, to avoid
duplicating the shared expand/status/duration fields — see `data-model.md` §3.

## Complexity Tracking

> Constitution Check has no violations. This section is intentionally empty —
> no deviations require justification.
