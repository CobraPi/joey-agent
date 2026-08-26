# Contract: UI Interaction Parity (FR-001..FR-013)

**Feature**: Subagent Screen Parity (`017-please-modify-joey`)
**Contract type**: UI interaction contract — the user-facing surface of this
feature is the interactive terminal UI (key/mouse input → behavior), per the
plan workflow's application-UI-surface contract category. This is an internal
feature: no public CLI flags, no config keys, no on-disk formats, no external
APIs. The authoritative behavioral reference is the orchestrator screen's
existing behavior, reproduced verbatim in `docs/tui.md` and the research
sources (research.md D1–D12).

---

## 1. Scope & non-goals

**In scope**: additive retargeting of the interactions below to the focused
subagent view (`App.focused_subagent == Some(pane)`), plus rendering that view
with the same widget functions as the orchestrator screen.

**Non-goals**: no CLI/config/on-disk/API changes; no new key bindings; no new
capabilities beyond what the orchestrator screen has (parity only); no changes
to orchestrator-screen behavior when no pane is focused (byte-identical
non-regression, constitution VII).

## 2. Parity keymap

"Today" = behavior before this feature. "Retargeted" = newly routed by this
feature. Reference semantics: orchestrator screen (`app.rs handle_key`,
`handle_mouse_scroll`; see research.md Sources).

| Input | Orchestrator screen (reference) | Focused subagent view (after) | FR | Status |
|---|---|---|---|---|
| `j`/`k`/`↑`/`↓` | Scroll one line | Same, on pane transcript | FR-002 | works today |
| `PgUp`/`PgDn` | Page scroll (10 lines) | Same | FR-002 | works today |
| `Ctrl+B`/`Ctrl+F` | Half-page scroll (15 lines) | Same | FR-002 | works today |
| `g`/`G`, `Home`/`End` | Jump to top / bottom | Same (was misrouted to main) | FR-002 | retargeted |
| Mouse wheel | 3-line scroll | Same | FR-002 | works today |
| `Space`/`x` | Expand viewport-center entry, 3-state cycle Collapsed→TailWindow→Full | Same (was main-only) | FR-003 | retargeted |
| Click entry | Toggle that entry's expansion (hit-test) | Same (shared `transcript_hit_test_core`) | FR-003 | works today |
| `Ctrl+E` | Cycle focused reasoning expansion | Acts on pane's entry (was main-only) | FR-004 | retargeted |
| `Ctrl+G` | Toggle focused tool expansion | Acts on pane's entry (was main-only) | FR-004 | retargeted |
| `y`/`Y` | Copy selected entry (hit-test selected) | Copies pane's selected entry via host clipboard | FR-006 | retargeted |
| `Ctrl+S` / `/` | Open search; `Enter` runs | Searches only the pane's transcript | FR-007 | retargeted |
| `n` / `N` | Next / previous match, scrolls to it | Same, within pane matches | FR-007 | retargeted |
| `Ctrl+O` | Maximized output viewer | Shows the pane's tool output | FR-008 | retargeted |
| `Ctrl+A` | Stats page | Pane stats page (state now per-pane) | FR-008/010 | retargeted |
| `F1` / `?` | Help overlay | Identical overlay (global, unchanged) | FR-013 | works today |
| `Ctrl+N` | Expand/collapse subagent rail | Same | — | works today |
| `Ctrl+P` / `Esc` | (with pane focused) return focus to orchestrator | Same | FR-010 | works today |
| `Ctrl+L` | Clear all subagent panes, focus → orchestrator, orchestrator scroll preserved | Same | FR-010 | works today |
| Click rail tab | Focus that pane's view | — (entry into pane focus) | FR-010 | works today |
| Click focused rail tab | Unfocus → orchestrator screen | — | FR-010 | works today |

Selection model (FR-006): the "selected entry" is resolved by hit-test at
viewport center for keyboard actions and by click position for mouse — no
persistent cursor; identical to the orchestrator screen (data-model.md,
Selection model).

## 3. Maximized-surface reachability matrix

| Surface | From orchestrator | From any focused pane | FR |
|---|---|---|---|
| Output viewer (`Ctrl+O`) | yes | yes — displays that pane's selected/last tool output, full scroll | FR-008 |
| Reasoning panel | yes | yes — renders that pane's reasoning (incl. flushed `streaming_reasoning`) | FR-008 |
| Stats page (`Ctrl+A`) | yes | yes — pane stats in the same layout, expandable context stream | FR-008/009 |
| Help overlay (`F1`/`?`) | yes | yes — one shared overlay, identical content | FR-013 |
| Mode-specific explorers (e.g. NeuroCode) | per current mode rules | reachable ONLY when that mode spawned the focused subagent | FR-008 |

## 4. Visual chrome contract

All chrome is defined once by shared widget functions and rendered by both
screens through those same functions — parity by construction (research.md D2,
FR-009; data-model.md Invariant 1):

- **Header/title**: `"N messages · P% from top"`, or `"· live"` in place of
  the percentage while following the tail. `N` = that view's entry count.
- **Scrollbar**: same glyphs/track, same proportional thumb.
- **Below-badge**: `"↓ N lines below"` when content extends past the viewport.
- **Borders, status line, colors**: same conventions on both screens.
- **File diffs**: identical old/new gutters, added/removed coloring, hunk
  headers, binary placeholder, and line caps (FR-005), via the shared
  `item_lines` FileDiff arm.
- **Empty state**: same styling as the orchestrator screen.

## 5. Internal interface note

`TuiAction` (joey-tui → joey-cli host) gains ONE additive pane-aware copy
variant (e.g. `CopyPaneItem { pane, idx }`) consumed by the existing host
clipboard path (pbcopy/xclip/wl-copy + OSC52). `TuiAction` is an internal
enum at the joey-tui ↔ joey-cli boundary, not a public contract surface; the
existing `TuiAction::CopyItem(idx)` is unchanged and remains
main-transcript-only (research.md D4; data-model.md TuiAction).

## 6. State-preservation contract

- Each view (orchestrator + every pane) independently owns its scroll,
  expansion, search, and stats state; all survive focus switches unchanged
  (FR-010).
- No action may mutate a non-focused view's state (data-model.md Invariant 2).
- Follow-tail applies only when a view is pinned at the bottom; scrolled-up
  views are never yanked by new content (spec edge case).
- `Ctrl+L` clears all panes, returns focus to the orchestrator screen, and
  preserves the orchestrator's scroll state (research.md D9).
- When no pane is focused, every orchestrator-screen interaction behaves
  exactly as before this feature (non-regression, constitution VII).

## 7. Acceptance mapping

| Contract section | FR | Success criteria |
|---|---|---|
| §2 scroll rows | FR-001, FR-002 | SC-001, SC-002, SC-005 |
| §2 expand/click rows | FR-003, FR-004 | SC-001, SC-002 |
| §4 diff rendering | FR-005 | SC-003 |
| §2 copy row + §5 | FR-006 | SC-002 |
| §2 search rows | FR-007 | SC-002 |
| §3 reachability matrix | FR-008 | SC-001 |
| §4 chrome | FR-009 | SC-003 |
| §6 state preservation | FR-010 | SC-004 |
| §2 applied per spawn surface | FR-011 | SC-001, SC-005 |
| quickstart.md checks | FR-012 | SC-005 |
| §3 help row | FR-013 | SC-001 |
