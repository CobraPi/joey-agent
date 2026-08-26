# Implementation Plan: Subagent Screen Parity

**Branch**: `017-please-modify-joey` | **Date**: 2026-08-22 | **Spec**: `specs/017-please-modify-joey/spec.md`

**Input**: Feature specification from `specs/017-please-modify-joey/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

The primary requirement is that full-screen subagent views (the `SubagentPane` surface that takes over the main area when a subagent is focused) reach functional and visual parity with the orchestrator transcript screen: scroll affordances (scrollbar, entries-below indicator, header message-count and scroll-percentage title), expand/collapse for every entry type including the 3-state cycle (collapsed → tail → full) and identical file-diff rendering, copy-entry, in-transcript search, maximized output viewer / reasoning panel / stats page, identical chrome, per-view state preservation across switches, and the shared help overlay. Parity must hold across ALL spawn surfaces — and it does so by construction, because every orchestration surface (delegate_task, call_omo_agent/OMO Atlas, /hypercode run, dispatch_batch) funnels through the single SubagentManager → tap → pane pipeline (research.md D8, FR-011).

The technical approach is focused-view action routing: extend the existing pane indirection (`App.focused_subagent`, `pane_apply`, `transcript_hit_test_core`) so key/mouse handlers target the focused pane when one is focused — no generic `TranscriptView` trait refactor of the main screen (research.md D1 rejected that as over-abstraction). The pane renderer composes the SAME widget functions the orchestrator screen already uses (`draw_scrollbar`, below-badge logic, header title format, `item_lines` incl. the FileDiff arm, `draw_output_viewer`/`draw_reasoning`/`draw_stats_page`), making visual divergence structurally impossible (D2). Data enrichment is one event-mapping fix: `pane_apply` maps FileChange events to FileDiff items the same way the main transcript does (D7). All changes are additive-only per constitution VII — orchestrator-screen code paths are untouched, and dedicated keys retarget to the focused pane only when a pane is focused (FR-004).

Per-view state additions mirror `App`'s existing patterns: per-pane search fields with a generalized `run_search`/`search_next` (D5), a `TuiAction` variant carrying pane identity for host-side clipboard (D4, clipboard stays in joey-cli with pbcopy/xclip/wl-copy + OSC52), per-pane stats-view state moved onto `SubagentPane` for state preservation (D6, FR-010), and flushing the pane's `streaming_reasoning` like the main loop does so the reasoning panel has content (D6). Testing follows the established joey-tui pattern — ratatui TestBackend buffer assertions plus pure state-logic tests with every increment, plus regression tests pinning main-screen behavior (D10). No new dependencies (D12); the perf budget is widget-reuse parity with the orchestrator screen (D11).

## Technical Context

**Language/Version**: Rust stable channel (pinned by `rust-toolchain.toml`), edition 2021.

**Primary Dependencies**: ratatui 0.30 (crossterm feature), crossterm 0.28, unicode-width 0.2, textwrap 0.16 (crates/joey-tui); clipboard handled host-side in joey-cli (pbcopy/xclip/wl-copy + OSC52, `crates/joey-cli/src/clipboard.rs`). NO new dependencies (research.md D12).

**Storage**: N/A — in-memory TUI state only; no on-disk format changes.

**Testing**: `cargo test` (workspace ~green); joey-tui uses ratatui TestBackend buffer assertions + pure state-logic tests. Existing suites: `crates/joey-tui/tests/{subagent_panes,delegate_expand,unified_inline_expansion,expandable_stats,expanded_view_formatting,smoke}.rs`.

**Target Platform**: cross-platform terminal (macOS/Linux/Windows terminals via crossterm).

**Project Type**: TUI subsystem of a CLI workspace app (crates/joey-tui + host wiring in crates/joey-cli).

**Performance Goals**: pane frame rendering cost must remain in the same complexity class as the orchestrator screen — widget reuse over visible entries; no new per-frame allocations beyond `draw_transcript`'s; no new polling/timers. Budget note per research.md D11: identical widget functions over the same entry counts give parity by construction, verified via existing TestBackend tests (no timing harness needed).

**Constraints**: additive-only (constitution VII non-regression; no public CLI/config/on-disk surface changes); parity only — no new capabilities beyond the orchestrator screen; upstream PORTING.md unaffected (TUI is Joey-native, not upstream-tracked).

**Scale/Scope**: single crate focus (joey-tui) + minimal host wiring (joey-cli) + one read-only verification pass over crates/joey-orchestration/src/manager.rs (D8 single-funnel check); ~1 renderer family, key/mouse routing, per-pane state fields, `pane_apply` event mapping.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **0 Cross-platform**: PASS — only crossterm/ratatui primitives are used; no platform-specific code added (clipboard remains the existing host-side path).
- **I Workspace-first**: PASS — all work lands in the existing `crates/joey-tui` and `crates/joey-cli` members; no new crates or standalone projects.
- **II CLI/TUI parity**: PASS — no capability is hidden from text surfaces; the change is the TUI itself and data flows are unchanged.
- **III FS source of truth**: N/A — no `.specify` artifact UI changes are involved.
- **IV Test-first**: PASS by plan — every increment ships with TestBackend buffer assertions plus state-logic tests, and regression tests pin main-screen behavior (research.md D10).
- **V Incremental delivery**: PASS — increments are (1) scroll affordances, (2) expand routing, (3) diff data mapping, (4) copy, (5) search, (6) maximized viewers, (7) per-pane stats state; each is independently shippable.
- **VI Modularity**: PASS — extends the existing pane indirection with no new cross-crate coupling; D1 explicitly rejected the `TranscriptView` trait refactor as over-abstraction.
- **VII Backward compat/non-regression**: PASS — additive key retargeting applies only when a pane is focused; regression tests pin main-screen behavior; the `TuiAction` extension is internal, not a public surface.
- **VIII Performance discipline**: PASS — widget reuse over visible entries with an explicit budget (see Technical Context); zero new dependencies (research.md D11/D12).

## Project Structure

### Documentation (this feature)

```text
specs/017-please-modify-joey/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
├── checklists/
│   └── requirements.md  # Requirements coverage checklist
└── tasks.md             # Phase 2 — not created by /speckit-plan
```

### Source Code (repository root)

```text
crates/
├── joey-tui/
│   ├── src/
│   │   ├── app.rs            # Key/mouse routing; pane-focused action retargeting
│   │   ├── state.rs          # SubagentPane, pane_apply, per-pane scroll/search/stats state
│   │   ├── widgets.rs        # draw_pane_transcript composing orchestrator widget functions
│   │   ├── theme.rs          # Shared styling (unchanged; parity by reuse)
│   │   └── neurocode_viz.rs  # Mode-specific explorer (reachability rule, FR-008)
│   └── tests/
│       ├── subagent_panes.rs
│       ├── delegate_expand.rs
│       ├── unified_inline_expansion.rs
│       ├── expandable_stats.rs
│       ├── expanded_view_formatting.rs
│       ├── smoke.rs
│       └── [new parity test files to be added]
├── joey-cli/
│   └── src/
│       ├── tui.rs            # Host TuiAction consumption incl. pane copy variant
│       └── clipboard.rs      # Host clipboard (pbcopy/xclip/wl-copy + OSC52; unchanged)
└── joey-orchestration/
    └── src/
        └── manager.rs       # Single-funnel verification target (read-only; D8/FR-011 — T026 verifies, no edits)
```

**Structure Decision**: This is a single-crate-focused change inside the existing workspace (no new crates). The pane system — `SubagentPane` / `pane_apply` / `draw_pane_transcript` in crates/joey-tui — is the single integration point: all spawn surfaces (delegate_task, call_omo_agent/OMO Atlas, /hypercode run, dispatch_batch) funnel through the one SubagentManager→tap→pane pipeline, so one code path gains parity for every surface (research.md D8). joey-cli is touched only for host-side wiring (TuiAction consumption for pane copy).

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| None — no gate violations. | | |
