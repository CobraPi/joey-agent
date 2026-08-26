# Data Model: Subagent Screen Parity

**Feature**: Subagent Screen Parity (spec: `spec.md`, Phase 0: `research.md` decisions D1–D12)
**Feature Branch**: `017-please-modify-joey`
**Date**: 2026-08-22
**Scope note**: This models **in-memory TUI state only** — no database, no on-disk format, no serialization changes. Affected code: `crates/joey-tui/src/state.rs` (entities), `app.rs` (routing), `widgets.rs` (rendering), plus the joey-cli host's `TuiAction` consumption.

## Entity Overview

| Entity | Owning type | Lives in | Lifetime |
|---|---|---|---|
| SubagentPane | `SubagentPane` struct | `App.subagent_panes` map (state.rs L305) | Session; survives Done/Failed; cleared only by Ctrl+L `clear_subagent_panes` |
| FocusedView routing state | `App.focused_subagent: Option<PaneId>` | `App` (state.rs L824) | Session; `None` ⇔ orchestrator screen focused |
| TranscriptItem | shared enum `TranscriptItem` | main transcript and each pane's `transcript: VecDeque<TranscriptItem>` | Per view; append-only during pane life |
| ExpansionState | `ReasoningExpandState` per item (`expand_state` field) | Each expandable `TranscriptItem` (state.rs L22, `cycle` L41) | Per entry, stable for entry lifetime |
| ScrollState | `scroll: Option<usize>` + `last_max_scroll` | `App.scroll` (orchestrator) and per-pane scroll (state.rs L2121–2144) | Per view; survives focus switches (FR-010) |
| SearchState | open/query/has_match fields | `App.search_*` (orchestrator, state.rs L936–941) and NEW per-pane mirror (D5) | Per view; isolated per transcript (FR-007) |
| StatsViewState | stats-view + context-entry toggles | NEW: moved onto `SubagentPane` from global `pane_stats_view` (D6) | Per pane; survives focus switches (FR-010) |
| NavigationRail | tab list derived from panes | rendered `draw_subagent_rail` (widgets.rs L5023) | Session; tracks pane set |
| TuiAction | enum `TuiAction` | joey-tui → joey-cli host (tui.rs L1179) | Per action, immediately consumed |

## SubagentPane

Per-subagent full-screen view entity. One per spawned subagent regardless of spawn surface (D8).

Fields (existing):
- `transcript: VecDeque<TranscriptItem>` — the pane's own append-only entry log.
- `scroll: Option<usize>` + per-pane `last_max_scroll` — see ScrollState.
- `streaming_reasoning: String` — in-flight reasoning text; **currently never rendered/flushed**; plan renders it in the pane's reasoning panel and `pane_apply` flushes it to a `Reasoning` TranscriptItem on completion, mirroring the main loop (D6).

Fields (NEW, this feature):
- `search_open: bool`, `search_query: String`, `search_has_match: bool` — mirror of `App.search_*` (D5).
- `stats_view: StatsViewState` — per-pane stats state moved from the global `pane_stats_view` that today resets on every switch (D6, FR-010).

Relationships: keyed by `PaneId` in `App.subagent_panes`; referenced by `App.focused_subagent`; fed by `pane_apply` (state.rs L497) from the SubagentManager tap.

Validation:
- Per-pane scroll/expand/search/stats state MUST survive focus switches (FR-010).
- Panes are never individually removed; only `clear_subagent_panes` (Ctrl+L) drops all panes, resets focus to orchestrator, and leaves orchestrator `App.scroll` untouched (D9).

State transitions (pane lifecycle): `Spawned → Running(Done|Failed)` — no removal on completion; `Any → Cleared` only via Ctrl+L, which forces `focused_subagent = None`.

## FocusedView routing state

`App.focused_subagent: Option<PaneId>` — `None` means the orchestrator screen is focused.

Action targeting rule (FR-004, D1/D3): when `Some(pane)`, transcript-targeted actions (scroll g/G/Home/End, expand Space/x, dedicated Ctrl+E/Ctrl+G, copy y/Y, search '/'/n/N, viewers Ctrl+O/Ctrl+A) act on that pane; when `None`, they act on the orchestrator transcript exactly as today. The help overlay (F1/'?') stays global regardless (FR-013).

Transitions: `None → Some(p)` on rail/tab focus; `Some(p) → None` on unfocus or pane disappearance/Ctrl+L. No `Some(p) → Some(q)` without passing through the rail selection.

## TranscriptItem

Shared enum used by BOTH the main transcript and panes (D2 — no pane-specific item type). Variants include `Message`, `Tool`, `Reasoning`, `FileDiff`, and the event-mapping source `FileChange`.

Change (D7): `pane_apply` currently drops FileChange events (`_ => {}`). It MUST instead map `FileChange → FileDiff` TranscriptItem using the same construction as the main transcript, so the shared `item_lines` FileDiff arm (widgets.rs L821–923) renders identical gutters/coloring/hunk headers/binary placeholders/caps (FR-005).

Validation: content richness identical to the main transcript — a pane must be able to hold every entry type the main transcript holds.

## ExpansionState (per item)

`ReasoningExpandState` enum on each expandable item's `expand_state` field.

Cycle (state.rs L22/L41):

```
Collapsed → TailWindow → Full → Collapsed → …
```

Validation:
- The cycle applies to Tool, Reasoning, and FileDiff items only (FR-003).
- Per-entry state is stored on the entry itself, so rapid arrival of new entries cannot perturb an entry's expansion state (spec edge case; FR-010).
- Toggling is by index into the OWNING view's transcript (`toggle_item_expand_by_index` semantics).

## ScrollState (per view)

`scroll: Option<usize>` — `None` = FollowTail (live, sticks to newest content); `Some(n)` = Pinned at line n. `last_max_scroll` tracks the clamp bound.

Transitions:
- `FollowTail → Pinned(n)` on `scroll_up` / wheel-up / PgUp.
- `Pinned(n) → FollowTail` on `to_bottom` (G/End), or when new streaming content arrives while pinned at the bottom.
- `Pinned(n) → Pinned(n±k)` on line/page/wheel steps.

Validation:
- Clamp: any pinned value stays in `[0, max_scroll]` (re-clamp against `last_max_scroll` as content grows/shrinks).
- Follow-tail applies only when the view was pinned at the bottom — being scrolled up must NOT yank the view on new content (spec edge case).
- Identical key/mouse semantics on both screens (FR-002); each view's scroll is independent and preserved across switches (FR-010).

## SearchState (per view)

Fields: `search_open: bool`, `search_query: String`, `search_has_match: bool` — mirrored per pane (D5); `run_search`/`search_next` generalize to operate on a target transcript.

Transitions: `Closed → Open (empty query) → Query (typing) → Running → (HasMatch | NoMatch)`; `Open → Closed` on Escape/accept.

Validation:
- Search operates ONLY on the focused pane's transcript when one is focused (FR-007); the orchestrator's `App.search_*` state is untouched by pane search and vice versa.
- Match semantics replicate the orchestrator verbatim: scroll-to-match + match indicator in the search bar; NO in-transcript text highlighting (parity decision D5).
- Navigation (n/N) wraps/advances matches and scrolls the OWNING view only.

## StatsViewState (per pane)

Moved from the global `pane_stats_view` (which resets on every pane switch) onto `SubagentPane` (D6).

Fields: the active stats view plus per-context-stream-entry expand toggles (`toggle_context_entry`-style, L2372).

Validation:
- MUST survive focus switches (FR-010) — this is the bug the move fixes.
- Rendered via the same stats-page layout conventions as the orchestrator stats page (FR-008/FR-009), retaining the expandable context stream.
- Reachable maximized surfaces from a pane: output viewer, reasoning panel, stats page; mode-specific explorers only when that mode spawned the subagent (FR-008 rule).

## NavigationRail

Tab list = orchestrator screen + one tab per `SubagentPane`, rendered by `draw_subagent_rail`.

Transitions: focus/unfocus moves `App.focused_subagent` between `None` and `Some(pane_id)`; each switch preserves both screens' scroll/expand/search/stats state (FR-010).

Validation:
- On pane disappearance or Ctrl+L clearing, focus returns to the orchestrator screen and the orchestrator scroll state is untouched (spec edge case, D9).
- The rail reflects the pane set exactly (tabs appear on spawn; all vanish on Ctrl+L).

## TuiAction

Internal enum emitted by the TUI and consumed by the joey-cli host (tui.rs L1179; clipboard.rs: pbcopy/xclip/wl-copy + OSC52).

- Existing: `CopyItem(idx)` — copies main-transcript entry `idx`.
- NEW (additive, D4): a pane-aware variant carrying pane identity + index, e.g. `CopyPaneItem { pane: PaneId, idx: usize }` — disambiguated from `CopyItem`, which stays main-transcript-only.

Validation:
- Additive variant only; `TuiAction` is internal to the joey-tui ↔ joey-cli boundary, NOT a public surface (no MAJOR bump).
- Actions never touch on-disk state; clipboard stays host-side.

## Selection model (non-entity)

There is NO persistent cursor entity. The "current entry" for keyboard actions is resolved by hit-test at viewport center; mouse actions resolve by click position — identical to the orchestrator screen, per spec FR-006 "same selection model verbatim" (D4). This is routing behavior, not stored state, and is documented here to prevent reintroducing a cursor entity.

## Invariants

1. **Single rendering source**: panes compose the SAME widget functions as the orchestrator screen (`draw_scrollbar`, badge, header title, `item_lines` incl. FileDiff, viewer/reasoning/stats layouts) — chrome parity by construction (D2, FR-009).
2. **Focused-view isolation**: no action may mutate a non-focused view's scroll/expand/search/stats state.
3. **Main-screen preservation**: when `focused_subagent == None`, main-screen behavior is byte-identical to today; pinned by regression tests (D10, constitution VII).
4. **One pane pipeline**: every spawn surface (delegate_task, call_omo_agent/OMO Atlas, /hypercode, dispatch_batch) produces identical `SubagentPane` entities via the single SubagentManager→tap→`pane_apply` funnel (D8, FR-011).

## Spec Key Entities → Model Entities

| Spec Key Entity | Model entity |
|---|---|
| Orchestrator Screen | `App` main transcript + `App.scroll`/`search_*` + `None` value of `App.focused_subagent` |
| Subagent View | `SubagentPane` (focused via `App.focused_subagent`) |
| Transcript Entry | `TranscriptItem` (shared enum; + FileChange→FileDiff mapping in `pane_apply`) |
| Expansion State | `ReasoningExpandState` per-item `expand_state` |
| Scroll State | `scroll: Option<usize>` + `last_max_scroll` per view |
| Search State | per-view open/query/has_match (`App.search_*` and pane mirror) |
| Stats Page | `StatsViewState` on `SubagentPane` (moved from global) |
| Navigation Rail | tab list over panes; `App.focused_subagent` transitions |
