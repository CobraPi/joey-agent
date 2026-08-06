# Contract: TUI `item_lines` Spacing & Width-Cap

**Feature**: `013-tui-cli-spacing-readability` | **Crate**: `joey-tui`
**Function**: `item_lines(item, content_w, theme) -> Vec<Line>` (widgets.rs:222)

This is the presentation contract for vertical rhythm and body-text width
capping in the TUI transcript. It is additive to the spec 007 block-layout
contract; the block structures (reasoning box, `$ command` headers,
icon+emoji+name tool headers, badges, footers) are unchanged.

## §1 — Inter-block separator (FR-001/002/003/015, Clarification Q1)

Every `TranscriptItem` variant's `item_lines` output MUST end with exactly
one blank line (`Span::raw("")`), appended as the final element of the
returned `Vec<Line>`, BEFORE the function returns.

| Variant | Current trailing blank | Required |
|---|---|---|
| `User` | none (widgets.rs:225-235) | **add one** |
| `Assistant` | one (widgets.rs:250) | keep (already one) |
| `Reasoning` | one (widgets.rs:344) | keep (already one) |
| `Tool` (terminal) | one then early-return (widgets.rs:426-427) | keep — blank MUST be before the `return lines` |
| `Tool` (generic) | none (falls through to widgets.rs:620) | **add one** before return |
| `FileDiff` | none (widgets.rs:547-594) | **add one** before return |
| `Notice` | none (widgets.rs:596-609) | **add one** before return |
| `Error` | none (widgets.rs:611-617) | **add one** before return |

Because `draw_transcript` (widgets.rs:703-708) concatenates items with no
inter-item logic and no item emits a LEADING blank, one trailing blank per
item yields exactly one blank between adjacent items — never two (INV-1).

**Streaming tail exception**: the live `streaming_assistant` block
(widgets.rs:682-695) emits NO trailing blank (it is live; the next committed
item provides separation). Unchanged.

**Empty-block suppression**: variants that render nothing (e.g. an empty
`Reasoning` that should not draw per spec 008 US1.4 — currently the TUI
always draws the box, so this is theoretical) MUST NOT emit a trailing blank
alone. In practice every variant emits ≥1 content line today, so a trailing
blank always pairs with content. (FR-015.)

## §2 — Body-text width cap (FR-005/007/008, Clarification Q2)

A module constant and helper:

```rust
/// Maximum column width at which body text wraps, regardless of panel width.
/// Matches crush's `maxTextWidth` (messages.go:26). Body-text-only (Q2).
const MAX_CONTENT_WIDTH: usize = 120;

/// Cap applied to BODY wrapping only. Degrades gracefully: when `content_w`
/// is below the cap, returns `content_w` unchanged (FR-007).
fn capped_content_width(content_w: usize) -> usize {
    content_w.min(MAX_CONTENT_WIDTH)
}
```

Applied at exactly these body-wrap call sites (the BODY of a message):

| Call site | Current | Required |
|---|---|---|
| `User` body (widgets.rs:230) | `wrap(text, content_w.saturating_sub(2))` | `wrap(text, capped_content_width(content_w).saturating_sub(2))` |
| `Assistant` body (widgets.rs:244) | `wrap(text, content_w.saturating_sub(2))` | `wrap(text, capped_content_width(content_w).saturating_sub(2))` |
| `Reasoning` body (widgets.rs:309) | `wrap(wl, content_w.saturating_sub(4))` | `wrap(wl, capped_content_width(content_w).saturating_sub(4))` |

**NOT capped (must remain at full `content_w`)** — headers, borders, tool/
terminal output, diffs, notices, errors:

| Call site / element | Why not capped |
|---|---|
| Tool/terminal headers (`one_line` at 359, 454) | single-line header (truncate, not wrap); Clarification Q2 |
| Tool/terminal output bodies (398, 411, 480, 512, 536) | command/code output renders at `width` (crush parity); FR-006 is indent, not width |
| FileDiff lines (589-592) | raw diff lines, width-sensitive |
| `Notice` (606), `Error` (612) | short status lines; capping adds no readability |

**Border alignment (FR-008)**: the reasoning box border
(`┌─ Reasoning ──...` widgets.rs:300-303; `└─` 333-342) is drawn from
`content_w`, NOT `capped_content_width`. The `│` prefix on body lines
(widgets.rs:311) is a fixed 1-col indent unaffected by wrap width. Borders
stay aligned by construction.

## §3 — Body indent (FR-006, no change)

Tool/terminal output bodies already indent at 4 spaces
(`format!("    {}", w)`); assistant/user bodies at 2 spaces. FR-006
codifies this as the contract. **No code change.** A regression test
asserts the indent is present.

## §4 — Viewport & hit-test preservation (FR-004, SC-006)

- `draw_transcript` lazy-builds items newest→oldest, stopping at
  `built >= needed` (widgets.rs:698-706). Adding one line per item does not
  change complexity; the bottom-anchored live tail stays visible (FR-004).
- `transcript_hit_test` (widgets.rs:758-837) calls `item_lines(...).len()`
  to map rows→items (widgets.rs:791). Because it delegates to `item_lines`,
  the new trailing-blank line count is picked up automatically — no drift
  (SC-006, research.md §3). **No code change to hit-test.**

## §5 — Acceptance examples (rendered)

TUI, two consecutive tool calls (P1 acceptance scenario US1.3):

```
  ✓ 🔧 write_file  path/to/file  3.2s
    result line 1
    result line 2
                                              ← exactly one blank (§1)
  $ ls -la crates  3.2s
    drwxr-xr-x  ...
```

TUI, wide panel body cap (P2 acceptance scenario US2.1), 200-col panel:

```
◆ agent
  <body wraps at 120 cols, not 198> ........... ← right edge of body
  ............................................. ← (blank space to panel border)
                                              ← exactly one blank (§1)
```
