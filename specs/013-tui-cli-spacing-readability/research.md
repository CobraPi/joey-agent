# Research: TUI & CLI Spacing / Vertical Rhythm (Crush-Style Readability)

**Feature**: `013-tui-cli-spacing-readability` | **Phase**: 0 | **Date**: 2026-08-05

This document resolves every design decision for porting crush's vertical
rhythm to the joey TUI (`crates/joey-tui/src/widgets.rs`) and CLI
(`crates/joey-cli/src/render.rs`). All decisions are grounded in actual
codebase analysis with file:line references, the three spec clarifications
(2026-08-05), and the upstream Crush source at
`/Users/jo110366/Development/crush`. No NEEDS CLARIFICATION markers remain.

---

## §1. TUI separator placement: per-item trailing blank vs. draw-loop separator

**Decision**: Emit exactly one trailing blank line (`Span::raw("")`) at the
end of `item_lines` for EVERY `TranscriptItem` variant — making the
separator uniform and per-item — and drop the inconsistent ad-hoc blanks
that exist today.

**Rationale (codebase grounding)**: `item_lines` (widgets.rs:222) currently
returns a `Vec<Line>` per item, and the separator behavior is inconsistent:
- `TranscriptItem::User` → no trailing blank (widgets.rs:225-235).
- `TranscriptItem::Assistant` → one trailing blank (widgets.rs:250).
- `TranscriptItem::Reasoning` → one trailing blank (widgets.rs:344).
- `TranscriptItem::Tool` (terminal) → one trailing blank then early-return
  (widgets.rs:426-427).
- `TranscriptItem::Tool` (generic) → NO trailing blank after the expanded
  view (widgets.rs:496-545 — falls through to `lines` return at 620 with no
  blank).
- `TranscriptItem::FileDiff` → NO trailing blank (widgets.rs:547-594).
- `TranscriptItem::Notice` → NO trailing blank (widgets.rs:596-609).
- `TranscriptItem::Error` → NO trailing blank (widgets.rs:611-617).

The gap problem is exactly the FileDiff/Notice/Error/generic-tool cases.
The clean fix is a single rule: every variant appends one blank line before
returning (the terminal-tool early-return at 427 must also append before
returning). Because items are concatenated with no inter-item logic in
`draw_transcript` (widgets.rs:703-708 just `blocks_rev.push(item_lines(...))`
and flattens), a per-item trailing blank produces exactly one blank between
adjacent items — no double-blank, because there is no leading blank on the
next item. This is the cheapest implementation that satisfies FR-001 and
the Clarification Q1 "deduplicated at boundaries" requirement by
construction (one trailing blank per item ⇒ one blank between items, never
two).

**Edge case — streaming tail**: the live `streaming_assistant` tail
(widgets.rs:682-695) builds its own block with a leading `◆ agent` line and
no trailing blank; it sits as the bottommost block. Under the new rule, the
*last committed item* before the tail already carries a trailing blank, so
the tail's header is separated from it. The tail itself needs no trailing
blank (it is live; the next committed item will provide separation). No
change to the tail block is required.

**Verification of FR-004 (viewport)**: `draw_transcript` builds items
newest→oldest lazily and stops once `built >= needed` (widgets.rs:698-706),
proportional to the viewport. Adding one line per item does not change the
algorithmic complexity; it only means each item contributes one extra row.
The lazy build still bounds work to the viewport. The bottom-anchored scroll
(`scroll=None` ⇒ live at bottom) keeps the tail visible. FR-004 holds.

**Alternatives considered**:
- Interpose a separator in `draw_transcript`'s flatten loop instead of
  per-item. Rejected: the streaming tail is special (no trailing blank), so
  the loop would need per-block-type exceptions anyway; per-item is simpler
  and self-contained in `item_lines`.
- Two blanks between items. Rejected by Clarification Q1.

---

## §2. TUI width cap mechanics: `capped_content_width` helper

**Decision**: Add a private helper `fn capped_content_width(content_w: usize) -> usize`
returning `content_w.min(MAX_CONTENT_WIDTH)` where `MAX_CONTENT_WIDTH = 120`.
Apply it ONLY to assistant-message and reasoning BODY wrapping (the `wrap(...)`
calls that produce body lines), NOT to headers, borders, or tool/terminal
blocks. Headers/borders keep using the full `content_w`.

**Rationale (codebase grounding)**: Today every `wrap(text, content_w.saturating_sub(N))`
call (widgets.rs:230, 244, 309, 398, 411, 480, 512, 536, 612, 687) passes the
full panel `content_w` minus an indent. On a 200-col panel, body text wraps at
~198 cols — edge-to-edge, hard to scan. Clarification Q2 decided the cap is
body-text-only, matching crush's `cappedMessageWidth` (messages.go:357-360:
`min(availableWidth - MessageLeftPaddingTotal, maxTextWidth)` with
`maxTextWidth = 120`).

The body-wrap calls to cap are precisely:
- `Assistant` body (widgets.rs:244): `wrap(text, content_w.saturating_sub(2))`
  → `wrap(text, capped_content_width(content_w).saturating_sub(2))`.
- `Reasoning` body (widgets.rs:309): `wrap(wl, content_w.saturating_sub(4))`
  → `wrap(wl, capped_content_width(content_w).saturating_sub(4))`.
- `User` body (widgets.rs:230): also a message body → cap it for consistency
  (crush caps all message content).

The calls that MUST NOT be capped (to satisfy FR-008 border alignment and
Clarification Q2):
- Tool/terminal headers (`one_line(summary, content_w.saturating_sub(20))`
  at 359, `content_w.saturating_sub(name.len()+12)` at 454) — these are
  single-line headers, not wrapped bodies; they already use `one_line`
  (truncate, not wrap). No change.
- Tool/terminal output bodies (widgets.rs:398, 411, 480, 512, 536) — these
  are command/code output where edge-to-edge is correct (matches crush's
  `toolOutputCodeContent`/`toolOutputPlainContent`, which render at `width`,
  not `cappedMessageWidth`). FR-006 codifies the *indent* (already 4 spaces),
  not a width cap, for these. No change.
- FileDiff lines (no wrap call — they emit raw `dl` at 589-592) — diffs are
  width-sensitive; capping would break diff alignment. No change.
- Error/Notice (`wrap(text, content_w.saturating_sub(4))` at 612,
  `one_line` at 606) — these are short status lines; leave as-is (capping a
  one-line notice adds no readability and risks surprising truncation).

**Graceful degradation (FR-007)**: `content_w.min(120)` naturally degrades —
when `content_w < 120`, `.min(120)` returns `content_w`, so narrow terminals
keep full width (no premature wrap, no overflow). No special-casing needed.

**No border misalignment (FR-008)**: the reasoning box border
(`┌─ Reasoning ──...` at widgets.rs:300-303, `└─` at 333-342) is drawn from
`content_w`, NOT from the capped width. The border stays full-width while
only the body lines inside wrap earlier. The `│` left border char prefixes
each body line (widgets.rs:311 `format!(" │ {}", w)`), so the left border
column is unaffected by how early the text wraps. Alignment holds by
construction. FR-008 satisfied.

**Constant choice**: `MAX_CONTENT_WIDTH = 120` matches crush's
`maxTextWidth` (messages.go:26) exactly. A module-level `const` is
appropriate (mirrors the existing `MAX_COLLAPSED_LINES`, `MAX_DIFF_LINES`
consts at widgets.rs:26-35).

**Alternatives considered**:
- Cap tool output bodies too. Rejected: crush doesn't (tool bodies render at
  `width`); capping diffs/code breaks alignment; FR-006 is about indent, not
  width.
- Make the cap configurable. Rejected (Principle VIII: avoid speculative
  generality; crush hardcodes 120).
- Apply cap in `wrap()` itself. Rejected: `wrap` is also used for tool bodies
  and errors where capping is wrong; capping at the call site is precise.

---

## §3. TUI `transcript_hit_test` line-accounting sync (SC-006 coupling)

**Decision**: The hit-test function `transcript_hit_test`
(widgets.rs:758-837) replicates `item_lines`'s line counts to map screen
rows to item indices. Because §1 adds exactly one trailing blank to every
item, the per-item line count used in hit-testing must include that blank.
Concretely, the hit-test computes `count = item_lines(...).len()` per item
(widgets.rs:791-793) — since it CALLS `item_lines`, it automatically picks
up the new trailing-blank line count with NO code change to the hit-test
itself.

**Rationale**: This is the key insight that de-risks SC-006. The hit-test
does not hardcode line counts; it invokes `item_lines(item, content_w, theme)`
(widgets.rs:791) and uses `.len()`. Therefore any change to `item_lines`'s
output length is automatically reflected in click-target accounting. The
only requirement is that the hit-test uses the SAME `item_lines` (it does —
same function, same module). No drift is possible as long as we do not
introduce a separate line-count estimator.

**Verification**: the streaming-tail line-count estimate in hit-testing
(widgets.rs:782, 803: `1 + wrap(...).len()`) does NOT include a trailing
blank (matching §1's tail decision). This is consistent: the tail block in
`draw_transcript` also has no trailing blank. Click targets remain accurate.

**Risk**: none, BECAUSE the hit-test delegates to `item_lines`. This is a
test-design note (the regression test in quickstart.md will verify click
expansion still works post-change), not an implementation change.

**Alternatives considered**:
- Maintain a separate line-count table. Rejected: precisely the drift risk
  SC-006 warns against; the current delegation design already avoids it.

---

## §4. CLI separator strategy: a `pending_separator` flag, not scattered blanks

**Decision**: Introduce a single `pending_separator: bool` (or equivalently
a small helper `fn emit_separator()`) in `render_turn`, set to `true` after
any element renders, and drained (print one blank line, set back to `false`)
before the NEXT element renders. This gives uniform, deduplicated spacing
across all `AgentEvent` arms without per-arm ad-hoc `println!()` calls.

**Rationale (codebase grounding)**: Today the CLI spacing is incidental and
inconsistent:
- `ToolStart` prints `println!()` only `if streamed_any` (render.rs:660-663).
- `Done` prints `println!()` after reflow (render.rs:972-974) and another
  before the turn summary (render.rs:985).
- `Failed` prints `println!()` `if streamed_any` (render.rs:1008-1010).
- Most other arms (Notice, Retry, Compression, Fallback, Subagent*,
  FileChange, OMO events) print their line with NO surrounding blanks.

The result: consecutive lifecycle events, a tool body followed by a notice,
or a diff followed by a subagent event all sit on adjacent lines — the exact
"dense log" problem the user reported.

A `pending_separator` drain-before-next-element pattern is the standard way
to implement "exactly one blank between adjacent elements, none at start,
none duplicated":
1. Before printing any element's first line, if `pending_separator` is true,
   print one blank line. (This is the "drain".)
2. After the element finishes printing, set `pending_separator = true`.
3. At turn boundaries (TurnStart), do NOT drain — the first element of a
   turn starts fresh (Edge Case: "no leading blank line at the top").
4. Suppressed elements (quiet/gate hidden, FR-015) do NOT set
   `pending_separator`, so a hidden block contributes no spacing and no
   dangling blank.

This satisfies FR-009 (uniform), FR-015 (no dangling blanks for suppressed
blocks — they simply don't set the flag), and Clarification Q1 (deduplicated
— draining resets the flag, so two consecutive renderable elements produce
exactly one blank, never two).

**Trailing-metadata exception (Clarification Q3, FR-012)**: the token-usage
line (`ApiCallEnd`, render.rs:567-578) must attach tightly to its block (no
blank before). Implement by having `ApiCallEnd` NOT drain the separator
before printing (it prints immediately after whatever preceded it — usually
the `ApiCallStart` spinner or a tool block), but SET `pending_separator =
true` after printing, so the NEXT element is preceded by one blank. This
precisely matches "tight before, one blank after."

**Where drain hooks go**: a single drain call at the top of each arm that
begins a new distinct element (ContentDelta start, AssistantMessage,
ToolStart, ToolEnd, Notice, Retry, Compression*, Fallback, Subagent*,
DelegationBatch, FileChange, AgentModeChanged, CategoryDelegation,
BoulderWork*, GoalSet/Cleared, WisdomAccumulated). Arms that are
tightly-coupled continuations (ToolProgress streaming line, the tick/spinner
repaint, ApiCallEnd usage line) do NOT drain. This is mechanical and
auditable per-arm.

**Alternatives considered**:
- Per-arm `println!()` before/after each element. Rejected: duplicates the
  dedup logic in every arm; high risk of double-blanks or missing blanks —
  the exact inconsistency we're fixing.
- Print blank AFTER each element only. Rejected: produces a trailing blank
  at turn end (Edge Case violation) unless special-cased; the drain-before
  pattern naturally avoids it because nothing drains after the last element.

---

## §5. CLI in-place tool-line rewrite interaction (FR-014)

**Decision**: The `pending_separator` drain (§4) MUST happen BEFORE the
`ToolStart` arm captures `cursor::position()` into `tool_row`
(render.rs:691-709), and the `ToolEnd` rewrite (render.rs:772-790,
803-821) MUST NOT drain. Body lines and the post-block separator append
naturally below the rewritten header row.

**Rationale (codebase grounding)**: The rewrite workflow (spec 008 T016/T022):
1. `ToolStart` (animations on): captures `tool_row = cursor::position().1`
   (render.rs:709), prints the spinner+name line, then `println!()` to move
   to the next row (render.rs:723).
2. `ToolEnd`: `cursor::MoveTo(0, tool_row)` +
   `Clear(CurrentLine)` (render.rs:776-780), prints the resolved header
   (render.rs:782), then body `println!`s append below (render.rs:793-795).

The `tool_row` is an absolute screen row captured at ToolStart. If a
separator were printed AFTER `tool_row` capture, it would not shift
`tool_row` (already captured), but it WOULD put a blank between the spinner
line and... nothing — the spinner line IS the tool line. The correct
sequence for ToolStart is: **drain separator (print blank if pending) →
capture tool_row → print spinner line → println**. The blank lands ABOVE the
spinner line, separating it from the previous block. `tool_row` is captured
after the drain, so it points at the spinner row. Correct.

For ToolEnd: the rewrite targets `tool_row` (the spinner row, unchanged by
the drain). Body lines append below via `println!` at render.rs:793-795.
After the body, set `pending_separator = true`. The NEXT element's drain
prints the blank below the body. No cursor-row math is touched by the
separator. FR-014 holds.

**Non-animated path** (animations off / NonInteractive): ToolStart prints
the header via `println!` directly (render.rs:733-741) — no rewrite, no
`tool_row`. The drain-before + set-after pattern applies identically;
`tool_row` capture is simply skipped (it's inside `if animations_on`).

**Verification**: the regression test (quickstart.md) runs a multi-tool turn
with animations ON and confirms the rewritten header is not corrupted (no
stray blanks on the header row, body lands below). This is the FR-014
acceptance test.

**Alternatives considered**:
- Drain after ToolStart spinner print. Rejected: would push the blank BELOW
  the spinner, between spinner and body, corrupting the visual block.
- Skip the separator entirely for tool blocks. Rejected: violates FR-011
  (consecutive tools must be separated).

---

## §6. Reasoning→content separation in both surfaces (FR-002, FR-010)

**Decision (TUI)**: §1's per-item trailing blank already separates a
`Reasoning` item from a following `Assistant` item (the Reasoning block's
trailing blank at the new uniform position sits between the `└─` footer and
the next item's `◆ agent` header). FR-002 satisfied by construction.

**Decision (CLI)**: The reasoning box is closed by `close_reasoning`
(render.rs:486-510), which prints the `└─ Thought for {:.1}s` footer. After
closing, set `pending_separator = true`. The next element (ContentDelta or
AssistantMessage) drains the separator, printing one blank between the
footer and the assistant text. FR-010 satisfied.

**Edge — reasoning immediately followed by a tool (no assistant text)**:
`ToolStart` calls `close_reasoning` (render.rs:666) then drains the
separator before capturing `tool_row`. The blank lands between the reasoning
footer and the tool header. Matches the spec Edge Case ("the rule applies
regardless of block-type pairing").

**Alternatives considered**:
- Hardcode a blank inside `close_reasoning`. Rejected: would double-blank
  when the pending_separator also drains; the flag pattern keeps it single.

---

## §7. All-modes behavior (FR-013) and quiet/gates (FR-015, FR-016)

**Decision**: The `pending_separator` drain is plain `println!()` — pure
text, no cursor control, no ANSI. It renders identically in Full, Reduced,
and NonInteractive modes (matching spec 008 FR-015's "structural layout
renders in all modes"). The drain is gated by the SAME `!opts.quiet` and
`tool_progress`/`show_reasoning` checks that gate the elements themselves:
if an arm skips printing (quiet/gate), it does NOT drain and does NOT set
the flag, so suppressed blocks contribute no spacing (FR-015) and the gates
are preserved unchanged (FR-016).

**Rationale**: The existing arms already early-return/`continue` on
`opts.quiet` (e.g. render.rs:581-583 reasoning, 751-753 tool, 886-889 diff).
The drain/set calls are placed AFTER those guards, inside the printing path,
so a suppressed arm never touches the flag. This is the minimal,
least-invasive integration.

**Alternatives considered**:
- A separate "strip blanks in NonInteractive" pass. Rejected (spec 008
  research §6 already decided against an ANSI/stripping layer; blanks are
  harmless plain text).

---

## §8. TUI body indent consistency (FR-006)

**Decision**: No code change needed for FR-006. The indent is ALREADY
consistent at 4 spaces for tool/terminal bodies (widgets.rs:399-401, 412-414,
480-482, 536-538 all use `format!("    {}", w)`) and 2 spaces for
assistant/user bodies (widgets.rs:231-233, 244-247). FR-006 is a
*codification* requirement (the spec makes the existing behavior normative),
not an implementation change. The task list will include a verification test
asserting the indent is present for every tool/terminal body, but no
production code change is required for FR-006.

**Alternatives considered**:
- Re-indent to crush's 2-space `toolBodyLeftPaddingTotal`. Rejected: would
  be a gratuitous change from the established 4-space indent (specs 005/007)
  not required by any FR; risks regression for no gain.

---

## §9. Summary of all design decisions

| # | Decision | Spec FR | Risk |
|---|---|---|---|
| §1 | TUI: one trailing blank per `item_lines` variant (uniform) | FR-001/2/3/4 | Low — adds one `Span::raw("")` per variant |
| §2 | TUI: `capped_content_width(content_w)` = `min(content_w, 120)` on body wraps only | FR-005/7/8 | Low — one helper, 3 call-site changes |
| §3 | TUI hit-test auto-syncs (delegates to `item_lines`) | SC-006 | None — no code change; test-only verification |
| §4 | CLI: `pending_separator` drain-before/set-after flag | FR-009/11/12/15 | Medium — touches every event arm, mechanical |
| §5 | CLI: drain before `tool_row` capture; no drain in ToolEnd rewrite | FR-014 | Medium — ordering-sensitive; regression-tested |
| §6 | Reasoning→content separation via §1 (TUI) / flag (CLI) | FR-002/010 | Low — follows from §1/§4 |
| §7 | CLI spacing is plain `println!`, gated with its arm | FR-013/15/16 | None — follows existing gate pattern |
| §8 | TUI body indent already consistent (FR-006 codified, no change) | FR-006 | None — verification test only |

No new dependencies. No public-surface changes (FR-017). No new config keys
or on-disk formats (FR-018). All changes confined to
`crates/joey-tui/src/widgets.rs` and `crates/joey-cli/src/render.rs`.
