# Quickstart: Subagent Screen Parity Validation Guide

**Feature**: `017-please-modify-joey` | **Spec**: `spec.md` (FR-001..FR-013, SC-001..SC-005)

A runnable guide to verify parity — manual end-to-end scenarios plus the
automated gate. It references `contracts/ui-parity-contract.md` and
`data-model.md` rather than duplicating them. No implementation code here.

## 1. Prerequisites

- Rust stable toolchain (pinned by `rust-toolchain.toml`).
- Repo at `/Users/jo110366/Development/joey-agent`.
- Build: `cargo build --workspace`.
- A terminal **≥ 96 columns** wide (the subagent rail needs the width to be
  visible); mouse-enabled terminal recommended (wheel/click scenarios).
- Any configured orchestration surface that spawns subagents: `/hypercode`
  pipeline, `delegate_task` (HyperCode), or `call_omo_agent`/OMO (incl.
  Atlas), `dispatch_batch` — all funnel into the same pane system (FR-011).

## 2. Automated verification (standard workflow gate)

Standard gate (repo acceptance bar, AGENTS.md):

```
cargo test --workspace
```

must be green. Targeted suite for this feature:

```
cargo test -p joey-tui
```

including the new parity suites that land with the implementation phases
(future files, named here so reviewers know what to expect):

- `crates/joey-tui/tests/pane_scroll_parity.rs` — scroll keys/amounts,
  scrollbar/badge/header affordances, follow-tail semantics.
- `crates/joey-tui/tests/pane_expand_parity.rs` — 3-state cycle, dedicated
  Ctrl+E/Ctrl+G retargeting, click hit-test, diff rendering parity.
- `crates/joey-tui/tests/pane_search_copy.rs` — pane-scoped search state,
  n/N navigation, copy action emission (pane-aware `TuiAction` variant).
- `crates/joey-tui/tests/pane_maximized_parity.rs` — output viewer, reasoning
  panel, stats page from panes; state preservation; main-screen
  non-regression.

What the ratatui `TestBackend` assertions prove (research.md D10): the
rendered buffer contains the exact scrollbar/header ("N messages · P%")/
"↓ N lines below" badge glyphs, diff gutters and +/-/@ coloring identical to
the orchestrator screen's, and pure state-logic tests prove key/mouse events
route to the focused pane (and only when one is focused).

## 3. Manual end-to-end scenarios

Each: **setup → action → expected**, with FR/SC references. The keymap and
chrome expectations live in `contracts/ui-parity-contract.md` §2/§4.

**S1 — Scroll affordances (FR-001/002, SC-001)**
Setup: run any `/hypercode` or delegate flow to spawn a subagent with a long
transcript; click its rail tab to focus its view.
Action: scroll with every key — `j/k/↑/↓`, `PgUp/PgDn`, `Ctrl+B/Ctrl+F`,
`g/G`, `Home/End`, mouse wheel.
Expected: a scrollbar reflects position, a "↓ N lines below" badge counts
remaining lines, and the header reads "N messages · P% from top", all
accurate.

**S2 — Follow-tail (FR-001 scenarios 2/3, spec edge case)**
Setup: focused pane, freshly spawned so it streams.
Action: leave it pinned at the bottom while streaming arrives; then scroll up
one page and let more content arrive.
Expected: pinned-at-bottom follows new content; scrolled-up does NOT jump
(scroll state per data-model.md ScrollState).

**S3 — Expansion (FR-003/004)**
Action: `Space`/`x` on a tool entry and click entries to cycle
Collapsed→TailWindow→Full; `Ctrl+E` on a reasoning entry, `Ctrl+G` on a tool
entry.
Expected: cycles and dedicated keys act on the pane's entries — never on the
orchestrator transcript (contract §2).

**S4 — Diff parity (FR-005, SC-003)**
Setup: trigger a subagent file edit (file-diff entry in the pane).
Action: expand the diff to Full; show the same diff on the orchestrator
screen.
Expected: identical gutters, added/removed coloring, hunk headers, binary
placeholders side by side.

**S5 — Copy (FR-006, SC-002)**
Action: hit-test select an entry (viewport-center via scroll, or click),
press `y`/`Y`, paste into any editor.
Expected: pasted content matches the selected pane entry (host clipboard,
OSC52/native — contract §5).

**S6 — Search (FR-007)**
Action: `/` (or `Ctrl+S`) a term unique to one subagent, `Enter`; then `n`
and `N`.
Expected: only that pane's transcript matches; navigation scrolls the pane;
match indicator = the orchestrator's bar (no in-text highlight — parity).

**S7 — Maximized surfaces (FR-008)**
Action: in a focused pane press `Ctrl+O` (output viewer), open the reasoning
panel, `Ctrl+A` (stats page).
Expected: each shows THAT pane's content with full scroll; stats page uses
the pane layout with expandable context stream.

**S8 — Chrome + state preservation (FR-009/010, SC-003/004)**
Action: side-by-side compare borders/header/status line/colors on both
screens. Then expand an entry and scroll mid-transcript in a pane; switch to
the orchestrator screen and back. Finally press `Ctrl+L` while a pane is
focused.
Expected: no styling divergence; pane scroll+expand state intact after the
round trip; after `Ctrl+L` the orchestrator screen returns with its scroll
preserved.

**S9 — Help (FR-013)**
Action: `F1` or `?` inside a focused pane.
Expected: the identical shared help overlay shown on the orchestrator screen.

**S10 — Universal parity (FR-011)**
Action: repeat S1 spot-checks from a different spawn surface (e.g. OMO agent
vs `/hypercode`).
Expected: identical capabilities regardless of spawn surface.

**S11 — Non-regression (SC-005)**
Setup: panes exist but none focused (orchestrator screen showing).
Action: exercise every orchestrator key from contract §2.
Expected: behavior exactly as before this feature.

## 4. Expected outcome

- All scenarios S1–S10 pass ⇒ SC-001..SC-004 satisfied.
- `cargo test --workspace` green (incl. the four joey-tui parity suites) ⇒
  SC-005 satisfied.

## 5. References

- `contracts/ui-parity-contract.md` — parity keymap, chrome, state contract
- `data-model.md` — per-pane state entities and invariants
- `research.md` — findings and decisions D1–D12
- `spec.md` — FR-001..FR-013, SC-001..SC-005
- `../../docs/tui.md` — in-repo TUI documentation
