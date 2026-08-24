# Phase 0 Research: Subagent Screen Parity

**Feature**: Subagent Screen Parity — full-screen subagent views gain every orchestrator-screen capability (FR-001..FR-013)
**Feature Branch**: `017-please-modify-joey`
**Date**: 2026-08-22
**Purpose**: Consolidate verified codebase findings (reference implementations, current pane gaps, spawn-surface topology, test patterns) and record the architecture decisions that will govern the plan, so planning can proceed from evidence rather than re-exploration.

## Findings Summary

Current state of the focused subagent pane vs. the orchestrator reference:

| FR | Capability | Pane status | Orchestrator reference (file:symbol) |
|---|---|---|---|
| FR-001 | Scrollbar, below-badge, header count/% | missing (pane scrolls but shows no position affordances) | `widgets.rs` `draw_transcript` L964, `draw_scrollbar` L1302, "↓ N lines below" badge L1071-1088, header title "N messages · P% from top / live" L965-982 |
| FR-002 | All scroll nav (line/page/top/bottom/wheel) | partial — j/k/PgUp/PgDn/wheel work; g/G/Home/End misroute to main `App.scroll` (app.rs L1162) | `state.rs` `scroll_up/down/to_top/to_bottom` L2121-2144 (`App.scroll: Option<usize>`, None = follow-tail); app.rs `handle_key` L623, `handle_mouse_scroll` L1597 |
| FR-003 | Expand/collapse all entry types, same keys + mouse, multi-state cycle | partial — click-to-expand works via shared `transcript_hit_test_core`; Space/x hit-tests main only (app.rs L1168) | `state.rs` `ReasoningExpandState` Collapsed→TailWindow→Full L22, `cycle` L41; `toggle_item_expand_by_index` L1428 |
| FR-004 | Dedicated expand actions retarget to focused view | missing — Ctrl+E/Ctrl+G target main transcript | `state.rs` `cycle_focused_reasoning_expand` L1390, `toggle_focused_tool_expand` L1404 |
| FR-005 | File-diff rendering identical | missing — `pane_apply` `_ => {}` drops diffs; FileDiff arm never exercised in panes | `widgets.rs` `item_lines` FileDiff arm L821-923, `parse_diff_lines`/`DiffLineParts`, '+'/'-'/'@' coloring, binary placeholder, MAX_DIFF_LINES caps 50/200/full |
| FR-006 | Selection model verbatim + copy | missing — no y/Y copy from panes | hit-test selection app.rs L1177-1196; y/Y → `TuiAction::CopyItem(idx)` app.rs L1220-1239 → host joey-cli/src/tui.rs L1179 → clipboard.rs (pbcopy/xclip/wl-copy + OSC52) |
| FR-007 | In-transcript search | missing — '/' searches main transcript | `state.rs` `search_open/search_query/search_has_match` L936-941, `run_search` L2659, `search_next` L2686; `widgets.rs` `draw_search_bar` L2753 |
| FR-008 | Maximized surfaces (output viewer, reasoning panel, stats) | partial — per-pane stats page exists; Ctrl+O never retargets (pane branch app.rs L153 preempts); pane `streaming_reasoning` never rendered/flushed | `state.rs` `toggle_output_viewer` L2169, `toggle_stats` L2321, `toggle_context_entry` L2372; `widgets.rs` `draw_output_viewer` L1577, `draw_stats_page` L1933, `draw_reasoning` L1459; `neurocode_viz.rs::draw_explorer` |
| FR-009 | Visual chrome parity | partial — bordered panel + title exist; title lacks message count + scroll %; no scrollbar/badge | `widgets.rs` `draw_pane_transcript` L5385 vs `draw_transcript` L964; `draw_pane_stats_page` L5477 |
| FR-010 | State preservation across switches | partial — scroll + expand persist; `pane_stats_view` is global and resets on every switch | `state.rs` `SubagentPane` L305, `pane_apply` L497, `App.focused_subagent` L824 |
| FR-011 | Universal parity across spawn surfaces | satisfied at pipeline level — all surfaces funnel through one SubagentManager→tap→pane system | delegate_tool.rs L179, call_omo_agent/OMO Atlas, hypercode.rs L845, dispatch_batch manager.rs L373 → manager.rs L273-358, events.rs L210-258 |
| FR-012 | Repeatable acceptance checks | pattern established | crates/joey-tui/tests/: subagent_panes.rs, delegate_expand.rs, unified_inline_expansion.rs, expandable_stats.rs, expanded_view_formatting.rs, smoke.rs |
| FR-013 | Shared help overlay | has — already global/shared | `widgets.rs` `draw_help_overlay` L2674 (F1/'?') |

Additional verified facts:
- Panes are never individually removed (they survive Done/Failed); only Ctrl+L `clear_subagent_panes` (state.rs L2565) clears panes, and it already resets focus while leaving orchestrator `App.scroll` untouched.
- Orchestrator search is scroll-to-match + a match indicator in the bar; there is NO in-transcript text highlighting today.
- The TUI is Joey-native: PORTING.md marks TUI features "no upstream equivalent" — this work is NOT upstream-parity-tracked.

## Decisions

**D1 — Core architecture: focused-view action routing.**
Decision: Extend the existing focused-pane indirection so key/mouse handlers target the focused pane when `App.focused_subagent` is set. No generic `TranscriptView` trait refactor of the main screen.
Rationale: Additive-only change (constitution VII); reuses the already-tested pane helpers (`pane_apply`, `transcript_hit_test_core`); keeps main-screen regression risk near zero; honors V (incremental delivery) and VI (narrow interfaces).
Alternatives considered: (a) Unify orchestrator + pane behind a generic `TranscriptView` trait — rejected: large-blast-radius refactor of a green screen for zero added user capability. (b) Fork copies of orchestrator widgets into the pane renderer — rejected: duplicates maintenance, violates VI.

**D2 — Rendering reuse: same widget functions.**
Decision: The pane renderer composes the SAME widget functions the orchestrator uses — `draw_scrollbar`, below-badge logic, header title format (message count + scroll %), `item_lines` incl. the FileDiff arm, and the `draw_output_viewer`/`draw_reasoning`/`draw_stats_page` layouts. Chrome parity by construction (FR-009).
Rationale: Reusing the exact functions makes visual divergence structurally impossible and keeps one code path to maintain.
Alternatives considered: Pane-local re-implementations styled to match — rejected: guaranteed drift, double maintenance.

**D3 — Input routing.**
Decision: In `handle_key`'s pane-focused branches, make g/G/Home/End, Space/x, Ctrl+E, Ctrl+G, y/Y, Ctrl+S/'/' and n/N, Ctrl+O, Ctrl+A, F1/'?' act on the focused pane; mouse handlers likewise via existing pane hit-test helpers. Dedicated actions retarget (FR-004); help stays global (FR-013).
Rationale: One routing point per key in the already-existing pane branch; mouse reuses proven hit-test helpers; no new bindings (spec assumption: reuse orchestrator bindings only).
Alternatives considered: A separate pane keymap table — rejected: duplicates the binding set and risks divergence from the orchestrator's.

**D4 — Selection/copy model.**
Decision: Replicate the orchestrator's hit-test-based selection VERBATIM — no new persistent cursor. Pane copy flows as an additive `TuiAction` variant carrying pane identity (e.g. `CopyPaneItem { pane, idx }` or equivalent), consumed by the existing joey-cli host clipboard path.
Rationale: Identical selection semantics is an explicit spec clarification; `TuiAction` is an internal enum between joey-tui and its host, so extending it is not a public-surface change; clipboard stays host-side where it already works (pbcopy/xclip/wl-copy + OSC52).
Alternatives considered: (a) A persistent cursor/selection bar per pane — rejected: new capability beyond parity scope. (b) Reusing bare `CopyItem(idx)` without pane identity — rejected: ambiguous against the main transcript.

**D5 — Search.**
Decision: Add per-pane search state fields mirroring `App`'s (open/query/has_match); generalize `run_search`/`search_next` to operate on a target transcript.
Rationale: Minimal generalization of existing pure functions; per-pane state keeps searches isolated to that subagent's transcript (FR-007).
Alternatives considered: Routing pane search through `App.search_*` with a target pointer — rejected: entangles main/pane state, harder to preserve independently.
Parity note: Orchestrator search = scroll-to-match + match indicator bar, NO in-transcript text highlighting. Pane search replicates exactly that. FR-007's "highlighting" is satisfied by the same match indicator the orchestrator shows; adding in-text highlighting would exceed parity scope (spec assumption: no new capabilities beyond the orchestrator screen).

**D6 — Maximized surfaces.**
Decision: Ctrl+O retargets to the focused pane's hit-test-selected/last tool output using the same viewer. The reasoning panel renders the pane's `streaming_reasoning` (currently never rendered) and `pane_apply` flushes it to a Reasoning item like the main loop does. Stats: keep `draw_pane_stats_page` but move `pane_stats_view` state onto `SubagentPane` (per-pane, survives switches) for FR-010. Mode-specific explorers (e.g. NeuroCode) remain reachable only from surfaces that spawned them (per spec clarification).
Rationale: Same layouts (D2), per-pane state fixes the reset-on-switch bug without new machinery, and the flush mirrors the main loop's existing reasoning lifecycle.
Alternatives considered: Rendering pane reasoning inline only (no panel) — rejected: FR-008 requires the maximized surface. Keeping `pane_stats_view` global — rejected: violates FR-010 state preservation.

**D7 — Data enrichment: FileChange → FileDiff.**
Decision: `pane_apply` maps FileChange events to FileDiff `TranscriptItem`s using the same construction as the main transcript (today dropped by `_ => {}`).
Rationale: The renderer (D2) already handles FileDiff; only event mapping is missing, so this unlocks FR-005 with no new rendering code.
Alternatives considered: A pane-specific diff item type — rejected: diverges from the shared `item_lines` path and its tested caps/coloring.

**D8 — Universal parity across spawn surfaces.**
Decision: Fix the pane system once. All spawn surfaces (delegate_task, call_omo_agent/OMO Atlas, /hypercode run, dispatch_batch) funnel through the single SubagentManager→tap→pane pipeline, so pane-level parity satisfies FR-011 everywhere; verify per-surface via the quickstart checklist (FR-012).
Rationale: One funnel means one fix; per-surface verification is a test/checklist concern, not a code concern.
Alternatives considered: Per-surface adapters — rejected: duplicates routing for no benefit; violates VI.

**D9 — Disappearance edge case.**
Decision: Rely on existing behavior — panes persist after Done/Failed; Ctrl+L `clear_subagent_panes` already resets focus and preserves orchestrator scroll. Add a regression test to pin it.
Rationale: The spec's graceful-return edge case is already satisfied; pinning it prevents future regression.
Alternatives considered: Auto-removing panes on completion — rejected: destroys user review state and changes existing behavior (constitution VII).

**D10 — Testing strategy.**
Decision: Every increment lands with (a) TestBackend buffer assertions for rendered affordances (scrollbar glyphs, header text, badge, diff gutters/colors) and (b) pure state-logic tests for routing/expand/search transitions; plus regression tests asserting main-screen handlers still act on the main transcript when no pane is focused.
Rationale: Matches the established joey-tui test pattern (buffer assertions + pure state tests) and constitution IV (tests alongside implementation) and VII (non-regression).
Alternatives considered: Screenshot/golden-file tests — rejected: heavier than the repo's existing pattern, no added confidence here.

**D11 — Performance budget.**
Decision: Pane rendering must not exceed the orchestrator screen's per-frame cost profile: reuse existing widget functions and hit-test helpers (same complexity class over visible entries), no new per-frame allocations beyond what `draw_transcript` performs, no new polling/timers. Target: indistinguishable frame latency vs. the orchestrator screen on a 200-entry transcript, measured via existing TestBackend tests (no timing harness needed — algorithmic-parity argument + widget reuse).
Rationale: Constitution VIII requires budgets on perf-sensitive paths; identical widget functions over the same entry counts give parity by construction, so a timing harness adds cost without signal.
Alternatives considered: Adding a frame-time benchmark harness — rejected: unjustified given the reuse argument and the repo's lack of any TUI timing infrastructure.

**D12 — Dependencies.**
Decision: NONE added. All capabilities already exist as in-repo code; clipboard stays host-side via `TuiAction` (OSC52 + native tools already implemented). Binary-size/compile-time impact: zero new deps, minor code growth in joey-tui only.
Rationale: Constitution VIII + Additional Constraints require justification for any new dependency; none is needed.
Alternatives considered: A TUI-side clipboard crate — rejected: duplicates the working host-side path and adds a dependency for zero capability.

### Constitution compliance notes

- **IV (tests alongside implementation)**: D10 mandates buffer + state-logic tests with every increment.
- **V (incremental delivery)**: D1/D3 route actions through existing branches; each capability (scroll, expand, copy, search, viewers) lands as an independent increment.
- **VI (modularity/narrow interfaces)**: D1 extends one indirection point; D4 uses an internal enum variant; D8 fixes one pipeline.
- **VII (additive-only / non-regression)**: D1-D4 change no orchestrator-screen code paths; D9/D10 explicitly pin main-screen behavior; `TuiAction` extension is internal, not a public surface (no MAJOR bump, no on-disk format change).
- **VIII (performance / dependency justification)**: D11 records the perf budget and its measurement rationale; D12 records zero new dependencies.

No gate violations are introduced (no new dependency, no public-surface change, no on-disk format change), so no Complexity Tracking entries are required.

## Sources

- crates/joey-tui/src/widgets.rs — `draw_transcript` (L964), `draw_scrollbar` (L1302), below-badge (L1071-1088), header title (L965-982), `item_lines` FileDiff arm (L821-923), `parse_diff_lines`/`DiffLineParts`, `transcript_hit_test` (L1099), `draw_output_viewer` (L1577), `draw_reasoning` (L1459), `draw_stats_page` (L1933), `draw_help_overlay` (L2674), `draw_search_bar` (L2753), `draw_subagent_rail` (L5023), `draw_pane_transcript` (L5385), `draw_pane_stats_page` (L5477)
- crates/joey-tui/src/state.rs — `ReasoningExpandState` (L22, `cycle` L41), `SubagentPane` (L305), `pane_apply` (L497), `App.focused_subagent` (L824), `search_open/search_query/search_has_match` (L936-941), `cycle_focused_reasoning_expand` (L1390), `toggle_focused_tool_expand` (L1404), `toggle_item_expand_by_index` (L1428), scroll methods (L2121-2144), `toggle_output_viewer` (L2169), `toggle_stats` (L2321), `toggle_context_entry` (L2372), `clear_subagent_panes` (L2565), `run_search` (L2659), `search_next` (L2686)
- crates/joey-tui/src/app.rs — pane branch (L153), `handle_key` (L623), pane keys (L977-1044), g/G misroute (L1162), Space/x hit-test (L1168), selection hit-test (L1177-1196), y/Y copy (L1220-1239), `handle_mouse_scroll` (L1597), pane clicks (L1760-1891)
- crates/joey-tui/src/neurocode_viz.rs — `draw_explorer`
- joey-cli/src/tui.rs — host `TuiAction` consumption (L1179)
- joey-cli/src/clipboard.rs — pbcopy/xclip/wl-copy + OSC52
- crates/joey-orchestration/src/manager.rs — SubagentManager/tap (L273-358), `dispatch_batch` (L373)
- crates/joey-agent-core/src/events.rs — subagent tap events (L210-258)
- delegate_tool.rs — delegate_task spawn (L179)
- joey-cli/src/hypercode.rs — /hypercode run spawn (L845)
- specs/017-please-modify-joey/spec.md — FR-001..FR-013, clarifications, assumptions
- .specify/memory/constitution.md — v1.1.0, principles IV-VIII
- crates/joey-tui/tests/ — subagent_panes.rs, delegate_expand.rs, unified_inline_expansion.rs, expandable_stats.rs, expanded_view_formatting.rs, smoke.rs
