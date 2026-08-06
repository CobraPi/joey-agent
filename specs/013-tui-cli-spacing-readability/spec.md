# Feature Specification: TUI & CLI Spacing / Vertical Rhythm (Crush-Style Readability)

**Feature Branch**: `013-tui-cli-spacing-readability`

**Created**: 2026-08-05

**Status**: Implemented

**Input**: User description: "optimize the formatting of the TUI so that it's more readable - follow the crush style layout. I also want you to modify the CLI and fix the formatting so that there is ample spacing between all elements."

## Clarifications

### Session 2026-08-05

- Q: What magnitude is "ample spacing" — one blank line between adjacent blocks (crush-parity), or each block wrapped before-and-after (more generous, ~2x line cost)? → A: One blank line between adjacent blocks (crush-parity). "Ample" means exactly one uniform separator between adjacent distinct blocks, deduplicated at boundaries so no double-blank gaps accumulate. This is formalized in FR-001 / FR-015 and applied to both the TUI and CLI.
- Q: Does the TUI's ~120-col width cap apply to body text only, or to the entire block (headers + borders + body)? → A: Body text only. Block headers, reasoning-box borders, `$ command` headers, and icon+emoji+name tool headers stay at full panel width; only assistant/reasoning body text is capped. This matches crush's `cappedMessageWidth` (content-only cap) and satisfies FR-008 (border alignment) by construction. Formalized in FR-005.
- Q: Does the CLI token-usage line (`↪ N in · M out`) get full before-and-after spacing, or tighter treatment as a trailing-metadata fragment? → A: Trailing-metadata treatment. The usage line attaches tightly to the block it summarizes (no blank line before it) and is followed by one blank line before the next distinct block. This avoids stacking blank lines around a one-line incidental metric across multi-step turns while keeping FR-009/FR-015 consistent (no two distinct elements on adjacent lines without a gap). Formalized in FR-012.

## User Scenarios & Testing *(mandatory)*

These stories are ordered by importance and are independently shippable. Each
targets one surface (TUI transcript panel, CLI streaming transcript) and the
shared concept of **vertical rhythm** — the consistent breathing room between
blocks (user messages, assistant messages, reasoning, tool calls, file diffs,
notices) so the transcript reads like crush's chat view rather than a dense,
edge-to-edge log. The primary users are developers who run `joey` in the TUI
or as a one-shot / REPL CLI turn and want to visually scan the transcript
without blocks blurring together.

The reference for "crush style layout" spacing is the upstream Crush
(`crush/internal/ui/chat/*`) transcript, which (a) separates every top-level
message item with vertical whitespace, (b) keeps thinking and content apart
with a blank line, (c) wraps body text at a readable cap (~120 cols) with a
small left gutter, and (d) uses a uniform indent so tool/terminal bodies read
as nested under their headers. This feature applies that rhythm to joey's TUI
and CLI without altering the block structures already delivered by specs 007
(TUI crush layout) and 008 (CLI crush layout, fully-expanded).

### User Story 1 - TUI Transcript Vertical Rhythm (Priority: P1)

As a developer watching the agent work in the TUI conversation panel, I want
clear vertical separation between every distinct transcript block (user
message, assistant message, reasoning box, tool call, file diff, notice) so
that I can tell at a glance where one block ends and the next begins — like
crush's chat layout, where each top-level message sits in its own visual band
with whitespace around it.

**Why this priority**: This is the heart of "optimize the formatting of the
TUI so that it's more readable." Today the TUI renders most transcript items
with a single trailing blank line (or none for some items), and several block
types (notices, errors, file-diff headers, consecutive tool calls) sit
directly adjacent to the blocks before them, producing a cramped wall of text.
Establishing a consistent per-block spacing rule in the TUI is the single
highest-leverage readability change and is self-contained within
`joey-tui`'s `item_lines` renderer — it does not gate the CLI story.

**Independent Test**: Open the TUI and run a turn that produces (a) a user
message, (b) a reasoning block, (c) the assistant's text answer, and (d) two
tool calls in a row; confirm every block is visually separated from its
neighbors by consistent whitespace so no two block headers or bodies touch,
matching crush's per-message banding.

**Acceptance Scenarios**:

1. **Given** two consecutive transcript blocks of any type (user→assistant,
   assistant→reasoning, reasoning→assistant, tool→tool, tool→file-diff,
   notice→notice, error→notice, …), **When** they render in the TUI
   transcript, **Then** there is consistent vertical whitespace between them
   — neither block's body runs directly into the next block's header.
2. **Given** a reasoning block immediately followed by an assistant content
   block in the TUI, **When** both render, **Then** the reasoning box close
   (`└─`) and the assistant `◆ agent` header are separated by a blank line,
   matching crush's thinking→content separation.
3. **Given** a sequence of several tool/terminal calls in the TUI, **When**
   they render, **Then** each tool block is separated from the next by
   uniform whitespace (not packed together), so the user can count distinct
   calls by the gaps.
4. **Given** any block in the TUI, **When** it renders, **Then** it does not
   start on the very same line as the previous block's last content line —
   there is always at least one line of separation between distinct blocks.
5. **Given** the TUI transcript panel at a normal height, **When** blocks
   accumulate, **Then** the extra spacing does not push the live/streaming
   tail off-screen mid-turn (the bottom-anchored scroll and lazy-build
   behavior is preserved — no regression from spec 007).

---

### User Story 2 - TUI Body Text Readability (Width Cap & Indent) (Priority: P2)

As a developer reading long assistant answers and tool results in the TUI, I
want the text wrapped at a comfortable reading width and indented under a
small left gutter, rather than stretching edge-to-edge across a wide terminal
— like crush's layout, which caps message width (~120 columns) and applies a
consistent left inset so paragraphs and output bodies read as nested,
scannable text.

**Why this priority**: This is the second half of TUI readability. Even with
good block separation, lines that run the full panel width on a large monitor
are hard to scan. Crush caps content width and indents bodies so the eye has
a consistent left margin. This story is P2 because it builds on the spacing
foundation of P1 and is the polish that makes long-form content comfortable;
it does not gate the CLI work.

**Independent Test**: Resize the TUI to a very wide terminal (e.g. 200
columns) and run a turn producing a long assistant answer plus a multi-line
tool result; confirm the body text wraps well before the right border
(~120-col cap) and that tool/terminal output is indented under its header
gutter, matching crush's readability.

**Acceptance Scenarios**:

1. **Given** a wide TUI transcript panel (more than ~120 content columns),
   **When** an assistant message or reasoning body renders, **Then** the
   wrapped lines do not exceed a readable width cap (around 120 columns), so
   paragraphs stay scannable instead of stretching edge-to-edge.
2. **Given** a terminal-command or generic tool output body in the TUI,
   **When** it renders, **Then** the body lines are indented consistently
   under the header (a small, uniform left gutter) — not flush with the panel
   edge — matching crush's nested-body indentation.
3. **Given** a narrow TUI (fewer columns than the cap), **When** text wraps,
   **Then** wrapping still uses the available width gracefully (the cap does
   not cause premature wrapping or overflow on small terminals) — no
   regression for small viewports.
4. **Given** the existing block structures from spec 007 (reasoning box,
   `$ command` headers, icon+name+param tool headers), **When** the width
   cap and indent apply, **Then** those headers and borders remain intact
   and correctly aligned (no border misalignment introduced by the new
   wrapping/indent).

---

### User Story 3 - CLI Ample Spacing Between Elements (Priority: P3)

As a developer running the agent one-shot (`joey -z "…"`) or in the REPL, I
want ample, consistent spacing between every distinct element in the
streaming CLI transcript — reasoning box, assistant text, token-usage line,
tool-call blocks, terminal-command blocks, file diffs, subagent events,
notices — so that the CLI output reads like crush's layout with clear
breathing room, rather than a dense log where blocks run together.

**Why this priority**: This is the explicit CLI half of the request ("modify
the CLI and fix the formatting so that there is ample spacing between all
elements"). Spec 008 already ported crush's *block structure* to the CLI
(fully-expanded); this story adds the *vertical rhythm* on top: blank lines
before and/or after each block so consecutive elements never touch. It is P3
because it depends conceptually on the same rhythm rule as P1 (so the two
surfaces stay consistent) and because the CLI already has some inter-block
spacing today — this story makes it uniform and ample, not net-new.

**Independent Test**: Run a one-shot CLI turn that triggers reasoning, an
assistant answer, a couple of tool/terminal calls (one with multi-line
output), and a file diff; confirm every major element is separated from its
neighbors by a blank line and that there is no place where two block headers
or a header and a body sit on adjacent lines without breathing room.

**Acceptance Scenarios**:

1. **Given** a CLI turn that streams reasoning then assistant content,
   **When** the reasoning box closes and content begins, **Then** there is
   a blank line between the reasoning footer (`└─ Thought for Ns`) and the
   start of the assistant text.
2. **Given** consecutive tool/terminal blocks in the CLI, **When** each
   completes, **Then** each block is separated from the next by a blank
   line (ample spacing) — no two `$ command` headers or tool headers sit on
   adjacent lines.
3. **Given** a token-usage line (`↪ N in · M out`) followed by more output,
   **When** the next element renders, **Then** the usage line is separated
   from the following block by a blank line.
4. **Given** a file-change diff block in the CLI, **When** it renders
   alongside other blocks, **Then** the diff header (`◆ path …`) and its
   last diff line are each separated from neighboring blocks by a blank
   line.
5. **Given** subagent spawn/complete events, notices, retries, compression,
   or fallback notices, **When** they render, **Then** each is separated
   from surrounding elements by consistent spacing (no event line butts
   directly against a tool body or assistant text).
6. **Given** `--quiet` mode, **When** a turn runs, **Then** only the final
   response prints (no inter-block spacing noise) — the existing quiet
   behavior is preserved (no regression).
7. **Given** non-interactive / piped-stdout output, **When** the transcript
   renders, **Then** the blank-line spacing still applies (it is plain
   text that survives piping) and the structural elements from spec 008
   (borders, `$ command`, badges) remain intact — no regression to the
   all-modes crush layout from spec 008 FR-015.

---

### Edge Cases

- What happens when a block is empty (empty reasoning, tool with no output,
  empty diff)? Spacing applies around the block only if the block actually
  renders; an empty/suppressed block (e.g. reasoning box not drawn because
  content is empty, per spec 008 US1.4) MUST NOT introduce a dangling blank
  line — no double-blank gaps where a block was skipped.
- What happens when many consecutive single-line notices/events fire (e.g. a
  burst of retries or subagent events)? Spacing between them MUST stay
  consistent (ample) but MUST NOT balloon into excessive whitespace — each
  event gets one consistent separator, not a growing stack of blanks.
- What happens at the very start of a turn (before the first block)? No
  leading blank line should be added beyond what the banner/turn delimiter
  already provides — spacing is between blocks, not a blank line at the top
  of the transcript for its own sake.
- What happens at the very end of a turn (after the last block / before the
  turn summary or usage)? A single trailing separator is acceptable; a
  double-blank must not accumulate across turns (the turn delimiter handles
  turn-to-turn separation).
- What happens when the terminal is very short (TUI)? The TUI is
  bottom-anchored and lazy-builds only visible lines; extra spacing consumes
  rows, so the rhythm MUST be modest (ample, not lavish) so the live tail
  stays visible — the lazy viewport build from spec 007 must keep working.
- What happens when the terminal is very narrow? The width cap (P2) MUST
  degrade gracefully — it only kicks in when the panel is wider than the cap;
  narrow terminals keep using full width (no premature wrap, no overflow).
- What happens when reasoning is immediately followed by a tool call (no
  assistant text in between)? The reasoning footer and the tool/terminal
  header MUST still be separated by a blank line (ample spacing) — the rule
  applies regardless of block-type pairing.
- What happens with the CLI's in-place tool-line rewrite (animations on)?
  The blank-line spacing MUST NOT corrupt the cursor-row rewrite logic from
  spec 008 (T016/T022) — the rewrite clears one header row; body lines and
  inter-block blanks are appended below it naturally. The spacing change must
  be verified not to shift the stored `tool_row` accounting.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The TUI transcript renderer MUST apply a consistent vertical
  separator of exactly one blank line between every pair of adjacent distinct
  transcript blocks (user, assistant, reasoning, tool, terminal, file-diff,
  notice, error) so that no block's body renders directly against the next
  block's header. The separator MUST be uniform across block-type pairings
  (the same one-blank-line rule for tool→tool as for assistant→reasoning),
  establishing a single vertical rhythm rather than per-type ad-hoc gaps.
  Boundaries are deduplicated so adjacent blocks never produce a double-blank
  gap (Clarification Q1, 2026-08-05; FR-015).
- **FR-002**: The TUI MUST separate a reasoning block from a following
  assistant content block with at least one blank line (between the
  reasoning box close and the `◆ agent` header), matching crush's
  thinking→content separation.
- **FR-003**: The TUI MUST separate consecutive tool/terminal blocks from
  each other with uniform whitespace so distinct calls are visually
  countable by the gaps (not packed together).
- **FR-004**: The TUI MUST NOT suppress the live/streaming tail off-screen
  mid-turn as a result of the added spacing — the existing bottom-anchored,
  lazy, viewport-proportional scroll/build behavior (spec 007) MUST remain
  intact. The added rhythm is modest (ample, not lavish) by design.
- **FR-005**: The TUI MUST wrap assistant-message and reasoning BODY text at
  a readable width cap (approximately 120 columns) when the transcript panel
  is wider than the cap, so long-form text stays scannable on wide terminals
  instead of stretching edge-to-edge. The cap applies to BODY TEXT ONLY —
  block headers, borders, `$ command` headers, and icon+emoji+name tool
  headers remain at full panel width (Clarification Q2, 2026-08-05; matches
  crush's `cappedMessageWidth`, which caps message content while borders and
  headers render at item width). This satisfies FR-008 (border alignment)
  by construction.
- **FR-006**: The TUI MUST indent terminal-command and generic-tool output
  bodies consistently under their headers using a uniform small left gutter,
  matching crush's nested-body indentation. (Spec 007 already indents bodies
  by 4 spaces; this requirement codifies that the indent is consistent and
  present for every tool/terminal body.)
- **FR-007**: The TUI width cap MUST degrade gracefully on narrow terminals:
  when the panel has fewer columns than the cap, wrapping MUST use the
  available width (no premature wrap, no overflow) — the cap only applies
  when width exceeds it.
- **FR-008**: The TUI spacing/width-cap changes MUST NOT misalign the
  existing block borders, headers, or badges delivered by spec 007 (the
  `┌─`/`└─` reasoning border, `$ command` headers, icon+emoji+name tool
  headers, `(exit N)` badges). Borders MUST stay aligned under the new
  wrapping/indent.
- **FR-009**: The CLI streaming renderer MUST emit consistent spacing of
  exactly one blank line between every distinct element in the transcript —
  reasoning box, assistant text, token-usage line, tool-call blocks,
  terminal-command blocks, file-diff blocks, subagent events, notices,
  retries, compression/fallback notices — so that no two distinct elements
  render on directly adjacent lines without breathing room. Boundaries are
  deduplicated so no double-blank gaps accumulate (Clarification Q1,
  2026-08-05).
- **FR-010**: In the CLI, a reasoning block MUST be separated from following
  assistant content by a blank line (between the `└─ Thought for Ns` footer
  and the start of assistant text).
- **FR-011**: In the CLI, consecutive tool/terminal blocks MUST each be
  separated from the next by a blank line — no two `$ command` or tool
  headers sit on adjacent lines.
- **FR-012**: In the CLI, each file-diff block and each lifecycle event
  (subagent spawn/complete/failed, batch-complete, retry, compression
  start/end, fallback, notice) MUST be separated from surrounding elements
  by consistent spacing. The token-usage line (`↪ N in · M out`) is a
  TRAILING-METADATA element: it attaches tightly to the block it summarizes
  (no blank line before it) and is followed by exactly one blank line before
  the next distinct block (Clarification Q3, 2026-08-05). This avoids
  stacking blanks around a one-line incidental metric across multi-step
  turns while preserving the FR-009 rule that no two distinct elements sit
  on adjacent lines without a gap.
- **FR-013**: The CLI spacing MUST apply in ALL capability modes (Full /
  Reduced / NonInteractive), consistent with spec 008 FR-015 — blank lines
  are plain text that survive piping/redirect, and the structural crush
  layout from spec 008 remains intact.
- **FR-014**: The CLI spacing MUST NOT corrupt the in-place tool-line
  rewrite logic (spec 008 T016/T022): the cursor-row rewrite clears exactly
  one header row, and body lines plus inter-block blanks append below it
  naturally. The stored `tool_row` accounting MUST remain correct.
- **FR-015**: The feature MUST NOT introduce dangling or double blank lines
  where a block was suppressed (empty reasoning not drawn, empty tool
  output, `tool_progress` gate hiding a block, `--quiet` hiding blocks). The
  separator attaches to blocks that actually render; suppressed blocks
  contribute no spacing.
- **FR-016**: The feature MUST preserve the existing `--quiet`,
  `display.show_reasoning`, and `tool_progress` gates unchanged. Where a
  block IS rendered, the new spacing applies; the gates only decide whether
  a block renders, as in spec 008.
- **FR-017**: The feature MUST NOT change any `AgentEvent` variant, any
  `TranscriptItem` variant, or any public surface in `joey-agent-core`,
  `joey-tui`, `joey-tools`, or `joey-cli`. This is a presentation-only
  change (TUI `widgets.rs::item_lines` and related; CLI `render.rs` print
  paths), consistent with constitution Principle VII (public-surface
  stability) and spec 008's no-surface-change scope.
- **FR-018**: The feature MUST NOT introduce a new runtime dependency,
  change the on-disk formats, or alter config keys — consistent with
  constitution Principle VIII (lean code) and the spec-kit public-surface
  contract.
- **FR-019**: The TUI and CLI spacing rules MUST be visually consistent with
  each other (the same notion of "ample separation" between block types) so
  that a developer moving between the two surfaces sees the same rhythm —
  matching crush's cross-surface consistency.

### Key Entities *(include if feature involves data)*

- **Vertical Rhythm (inter-block separator)**: The presentation concept of
  consistent whitespace between distinct transcript blocks. It is a property
  of the renderer (TUI `item_lines` trailing separators; CLI print-path
  blank-line emission), not new data — every block that renders carries a
  uniform separator before/after, and suppressed blocks carry none. This is
  the core entity the feature formalizes.
- **Readable Width Cap (TUI)**: The presentation concept of a maximum
  content width (~120 columns) at which body text wraps in the TUI,
  regardless of panel width. It is a render-time wrapping parameter in
  `item_lines`, not stored data. Mirrors crush's `maxTextWidth`.
- **Left Gutter / Body Indent**: The uniform left inset under which
  tool/terminal output bodies and assistant paragraphs render in the TUI,
  giving the eye a consistent margin (matches crush's
  `MessageLeftPaddingTotal` / `toolBodyLeftPaddingTotal`).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer viewing the TUI transcript can visually
  distinguish every distinct block (user, assistant, reasoning, tool,
  terminal, file-diff, notice) by consistent whitespace alone — no two
  blocks' content touches — matching crush's per-message banding without
  reading the headers.
- **SC-002**: A developer reading a long assistant answer or multi-line tool
  result in the TUI on a wide terminal sees text wrapped at a comfortable
  reading width (~120 cols) and indented under a consistent gutter, rather
  than stretching edge-to-edge — measurably more scannable than the current
  full-width wrapping.
- **SC-003**: A developer running the same turn in the CLI and the TUI sees
  the same vertical rhythm (ample, consistent separation between the same
  block types), differing only in that the CLI is a scrolling stream and the
  TUI is a viewport — the spacing intent is consistent across surfaces.
- **SC-004**: A CLI transcript containing reasoning + assistant text +
  multiple tool/terminal calls + a file diff + a subagent event shows a
  blank line between every pair of adjacent distinct elements, with zero
  instances of two block headers or a header and a body sitting on directly
  adjacent lines.
- **SC-005**: The full workspace test suite (`cargo test --workspace`)
  remains green, and no `AgentEvent` / `TranscriptItem` / public-surface
  change and no new dependency is introduced — the feature is a
  presentation-only change confined to `crates/joey-tui/src/widgets.rs`
  (TUI) and `crates/joey-cli/src/render.rs` (CLI).
- **SC-006**: The existing TUI bottom-anchored scroll, lazy viewport build,
  click hit-testing (`transcript_hit_test`), and CLI in-place tool-line
  rewrite all continue to work correctly under the new spacing (no
  regression to spec 007 / spec 008 interactive behavior).

## Assumptions

- The reference for "crush style layout" spacing is the upstream Crush
  transcript renderer at `crush/internal/ui/chat/*.go` (verified on this
  machine at `/Users/jo110366/Development/crush`). Its spacing conventions
  are: (a) top-level message items are separated by vertical whitespace; (b)
  thinking and content within an assistant message are joined with a blank
  line (`renderMessageContent` inserts `""` between thinking and content);
  (c) body text is capped at `maxTextWidth = 120` columns for readability
  (`cappedMessageWidth`); (d) every message has a small left padding
  (`MessageLeftPaddingTotal = 2`) and tool bodies a body padding
  (`toolBodyLeftPaddingTotal = 2`). This feature ports that *rhythm*, not
  crush's caching/animation machinery.
- This feature is strictly additive on top of specs 007 (TUI crush block
  layout) and 008 (CLI crush block layout, fully-expanded). It changes only
  the *vertical spacing* between blocks and (TUI only) the *width cap / body
  indent* of content; the block structures themselves (reasoning box,
  `$ command` headers, icon+emoji+name tool headers, `(exit N)` badges,
  `└─ Thought for Ns` footer) are unchanged.
- "Ample spacing between all elements" means consistent blank-line
  separation between distinct blocks — one uniform separator between
  adjacent blocks, not a growing stack. It does NOT mean large gaps or
  borders around every line; the rhythm is modest so the TUI live tail
  stays visible (FR-004) and the CLI doesn't balloon (Edge Cases).
- The TUI already inserts a trailing blank line after some items (e.g.
  `TranscriptItem::Assistant` and `Reasoning` push a final empty `Span`;
  tool/terminal blocks push one too). The gaps this feature addresses are
  the *inconsistent* ones — notices, errors, file-diff headers, and
  consecutive tool calls where separation is missing or uneven. The
  assumption is that a single uniform rule replaces the per-type ad-hoc
  behavior.
- The CLI today inserts a blank line in a few spots (e.g. `ToolStart` prints
  `println!()` if `streamed_any`, and there are isolated `println!()` calls
  around the turn summary). This feature generalizes that to a consistent
  inter-element separator across all `AgentEvent` arms, so spacing is
  uniform rather than incidental.
- All changes are presentation-only and confined to `crates/joey-tui/src/
  widgets.rs` (and the TUI width/indent path) and `crates/joey-cli/src/
  render.rs`. No `AgentEvent` or `TranscriptItem` variant changes (respects
  constitution Principle VII). No new dependencies (Principle VIII).
- The CLI's in-place tool-line rewrite (spec 008 T016/T022) stores a
  `tool_row` and rewrites exactly one row on `ToolEnd`. Added inter-block
  blank lines append below the rewritten header via normal `println!`, so
  they do not shift the stored row; this must be verified, not assumed, in
  the plan (FR-014).
- The TUI `transcript_hit_test` (spec 007 T026) replicates `item_lines` line
  accounting for click detection. Any change to the line count per item
  (which the spacing rule affects) MUST be reflected in the hit-test
  accounting so click targets stay accurate — this is a known coupling the
  plan must address (SC-006).
- `--quiet`, `display.show_reasoning`, and `tool_progress` gates are
  preserved exactly as in spec 008: they decide whether a block renders, and
  the new spacing only attaches to blocks that actually render (FR-015,
  FR-016).
