# Feature Specification: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Feature Branch**: `005-expandable-diff-ui`

**Created**: 2026-07-25

**Status**: Draft

**Input**: User description: "add file diff tracking to both the TUI and CLI - reference the Development/crush project for the UI setup. Also implement expandable thinking sections and tool calls - just like the crush UI."

## Clarifications

### Session 2026-07-25

- Q: How should the renderer obtain before/after content to produce diffs? → A: Both — structured file-change tracking for create/edit/delete (records reads as baselines, emits before+after content for changes) AND diff-text detection in any tool output.
- Q: What scope of file content should the baseline ("before") track? → A: Session read baselines — record every file the agent reads during a session as a content baseline (in-session only, no cross-session persistence).
- Q: Which mutating tools must emit File Change Events (before/after content)? → A: Explicit joey file tools (write_file, patch) AND terminal commands that mutate files — anything that changes a file the user might expect to review.
- Q: Should the TUI offer a per-item expand affordance or a single global "expand all / collapse all" control? → A: Per-item only — each reasoning section and tool call expands/collapses independently via a bound key or click on that item, matching the crush reference.
- Q: Should diffs include syntax highlighting of the code lines, or just add/remove/context coloring? → A: Ship syntax highlighting in v1 — each diff line is syntax-highlighted by language in addition to add/remove coloring.

## User Scenarios & Testing *(mandatory)*

<!--
  These stories are ordered by importance. Each is independently shippable
  and independently testable, so any one delivers a viable MVP slice.
  The primary users are developers running the `joey` agent locally who
  need to review what the agent changed and inspect its reasoning/output
  without leaving the session.
-->

### User Story 1 - See File Changes as a Visual Diff (Priority: P1)

As a developer running the agent, when the agent creates, edits, or deletes
files during a turn, I want each file change rendered as an inline,
color-coded diff (additions, removals, context lines) so that I can review
exactly what changed without opening a separate tool or re-reading raw tool
output.

**Why this priority**: Reviewing file mutations is the single highest-stakes
action an agent takes; a clear diff is the difference between trusting the
result and guessing. This is the foundation every other story builds on, and
it must work in both the interactive TUI and the streaming/one-shot CLI so
that no surface is left without it.

**Independent Test**: Run a turn that edits a known file; confirm the diff
appears inline with distinct addition/removal styling and a correct
line-count summary. Delivers immediate, verifiable review capability.

**Acceptance Scenarios**:

1. **Given** a file exists with known content, **When** the agent edits it
   during a turn, **Then** an inline unified diff is rendered showing the
   changed lines with additions and removals visually distinguished and a
   header indicating the file path and total additions/removals.
2. **Given** the agent creates a brand-new file, **When** the change is
   rendered, **Then** the diff shows the entire new content as additions
   (no removals) and labels it as a new file.
3. **Given** the agent deletes a file, **When** the change is rendered,
   **Then** the diff shows the entire prior content as removals and labels
   it as a deletion.
4. **Given** the agent edits multiple files in one turn, **When** the
   changes are rendered, **Then** each file's diff is presented as a
   separate, clearly delimited block.
5. **Given** a non-interactive terminal (piped output or `--quiet`),
   **When** the agent edits a file, **Then** a plain-text unified diff is
   still emitted to standard output (no color/animation), preserving the
   review capability outside the TUI.

---

### User Story 2 - Expandable & Collapsible Thinking Sections (Priority: P2)

As a developer, when the model emits reasoning/thinking content, I want that
content shown in a collapsible section — collapsed to a compact summary by
default and expandable on demand — so that reasoning is available when I
want depth but does not consume screen space when I am focused on the
answer.

**Why this priority**: Reasoning is valuable for trust and debugging but is
verbose; a collapsed default keeps the transcript scannable while preserving
full access. It is second priority because it is purely about transcript
ergonomics, whereas file diffs (P1) gate correctness review.

**Independent Test**: Trigger a turn that produces reasoning; confirm the
section renders collapsed, then expand it and confirm the full reasoning
text is revealed. Delivers a cleaner reading experience on its own.

**Acceptance Scenarios**:

1. **Given** a turn produces reasoning text, **When** the reasoning section
   is rendered, **Then** it appears collapsed by default showing a compact
   header/affordance (e.g. a label and a hint to expand) rather than the
   full text.
2. **Given** a collapsed reasoning section, **When** the user activates the
   expand affordance (keyboard or, in the TUI, click), **Then** the full
   reasoning text is revealed in place.
3. **Given** an expanded reasoning section, **When** the user activates the
   affordance again, **Then** it collapses back to the compact view.
4. **Given** a very long reasoning block, **When** expanded, **Then** the
   view shows a bounded tail window of the most recent lines with an
   affordance indicating how many earlier lines are hidden, and a second
   activation promotes it to a full expansion (three-state cycle:
   collapsed → tail-window → full).

---

### User Story 3 - Expandable & Collapsible Tool Calls (Priority: P3)

As a developer, when the agent invokes a tool, I want the tool call and its
result rendered as a collapsible block — a one-line summary when collapsed
and full details (arguments and result) when expanded — so that the
transcript stays compact while remaining fully inspectable on demand.

**Why this priority**: Tool calls are already summarized today; making them
expandable is an ergonomics refinement layered on the existing inline
rendering. It is the lowest-priority slice because the current summary is
functional, but it completes parity with the reference UI and pairs
naturally with the new diff rendering (file-edit tools show a diff when
expanded).

**Independent Test**: Run a turn that calls a tool; confirm the tool renders
as a one-line summary, then expand it and confirm the full arguments and
result are shown. Delivers a more compact, inspectable transcript.

**Acceptance Scenarios**:

1. **Given** the agent invokes a tool, **When** the tool call is rendered,
   **Then** it appears as a compact one-line summary (tool name, status,
   short description).
2. **Given** a collapsed tool call that has completed, **When** the user
   expands it, **Then** the full tool arguments and result are revealed.
3. **Given** the expanded tool is a file-editing tool that produced a diff,
   **When** expanded, **Then** the file diff (from User Story 1) is shown
   inside the expanded tool block.
4. **Given** a tool result too long to show fully, **When** collapsed,
   **Then** a truncation affordance indicates how many lines are hidden and
   how to reveal them.

---

### Edge Cases

- What happens when the changed file is binary or non-UTF-8? The diff must
  not crash or emit garbage; it should render a clear "binary file changed"
  placeholder instead of a textual diff.
- What happens when a diff is extremely large (hundreds of changed lines)?
  Collapsed/truncated views must bound the rendered height and advertise the
  hidden line count, exactly like long reasoning/tool output.
- What happens when diff content arrives incrementally while a tool is still
  streaming? The rendered block must update without tearing or duplicating
  content; a partially-applied patch should render the portion resolved so
  far. **Scope decision (analyze 2026-07-25, E1):** inline diffs are emitted
  and rendered once per file change, at the moment the write completes (the
  `ToolStart` → (`FileChange`)* → `ToolEnd` ordering in
  `contracts/agent-event.md`). Partial/streaming patch rendering during an
  in-flight `patch` call is **deferred** from v1: the `patch` tool resolves
  the full edit before returning, so there is no observable "partially
  applied" state to render under the current sequential tool-execution
  model. If a future tool streams partial edits, this edge case is
  revisited by extending `FileChange` to carry a partial flag. This
  decision is recorded here so it is not silently dropped.
- What happens when the agent edits the same file more than once in a turn?
  Each edit should be reflected; the final diff should represent the net
  change against the file's state when the agent first read it (so repeated
  edits don't show intermediate noise).
- What happens in a non-interactive/quiet CLI context? All expand/collapse
  affordances collapse to "show everything" (plain text) so no information
  is hidden when there is no interaction layer — diffs, reasoning, and tool
  output are emitted in full.
- What happens when reasoning is disabled by the user (e.g. via the existing
  reasoning-visibility toggle)? The thinking section must be omitted
  entirely, honoring the existing preference.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST render file changes (create, edit, delete)
  produced during a turn as an inline, visually distinguished unified diff
  in both the interactive TUI and the non-interactive/one-shot CLI.
- **FR-002**: Each rendered diff MUST display the file path and a count of
  added and removed lines.
- **FR-003**: The system MUST visually distinguish added lines, removed
  lines, and unchanged context lines within a diff (e.g. by color and/or a
  leading `+`/`-` marker), AND MUST apply per-language syntax highlighting
  to the code content of each diff line in addition to the
  add/remove/context coloring.
- **FR-004**: The system MUST label brand-new files and deleted files as
  such, rather than rendering them as ordinary modifications.
- **FR-005**: The system MUST detect whether a tool's textual output is
  itself a unified diff and render that output using the same diff
  presentation (so diffs the agent pastes or returns are also visualized).
- **FR-006**: The system MUST render reasoning/thinking content inside a
  collapsible section that is collapsed by default, with an affordance to
  expand it.
- **FR-007**: A collapsed reasoning section MUST expand to reveal its full
  content on user activation, and MUST collapse again on a repeat
  activation.
- **FR-008**: For reasoning that exceeds a bounded height when expanded, the
  system MUST show a tail window of the most recent content with an
  affordance stating how many earlier lines are hidden, and MUST allow a
  further activation to reveal the full content (three-state cycle).
- **FR-009**: The system MUST render each tool call as a collapsible block:
  a compact one-line summary when collapsed and the full arguments plus
  result when expanded.
- **FR-010**: When a file-editing tool call is expanded and it produced a
  diff, the system MUST render that diff inside the expanded block.
- **FR-011**: For tool results exceeding a bounded height, the system MUST
  truncate the collapsed view and advertise the number of hidden lines with
  an affordance to expand.
- **FR-012**: The system MUST NOT hide any content in non-interactive
  contexts (piped output, `--quiet`): diffs, reasoning, and tool output are
  emitted in full plain text, and all expand/collapse states resolve to
  "fully shown."
- **FR-013**: The system MUST honor the existing reasoning-visibility
  preference: when reasoning is disabled, no thinking section is rendered.
- **FR-014**: The system MUST NOT introduce regressions: existing streaming,
  tool-line, banner, and usage rendering behavior in both the TUI and CLI
  MUST remain intact.
- **FR-015**: Any change to file state the agent relies on to compute a
  diff (the "before" content) MUST be tracked per session and per file, so
  that a diff reflects the change since the agent's last known view of that
  file.
- **FR-016**: The system MUST handle binary or non-text files gracefully,
  rendering a "binary file changed" placeholder instead of a textual diff.
- **FR-017**: File Change Events MUST be emitted for every file-mutating
  operation the agent performs, including at minimum the explicit joey
  file tools (`write_file`, `patch`) and terminal commands that change file
  contents, so that no write path silently lacks a diff.
- **FR-018**: Expand/collapse in the TUI MUST operate per-item (each
  reasoning section and tool call toggles independently via a bound key or
  click on that item). A global "expand all / collapse all" control is out
  of scope.

### Key Entities *(include if feature involves data)*

- **Tracked File Read**: A record that the agent read a specific file at a
  specific point in a session, establishing the baseline ("before") content
  used to compute the diff when that file is later edited. Scoped per
  session and per file path. This is one of two inputs to diff rendering.
- **File Change Event**: A structured change notification emitted when the
  agent creates, edits, or deletes a file, carrying the file path, the
  before content (derived from the last Tracked File Read, empty for new
  files, full prior content for deletions) and the after content. This is
  the second input to diff rendering, complementing diff-text detection.
- **Rendered Diff Block**: The visual representation of one file's net
  change within a turn: file path, additions count, removals count, and the
  ordered set of added/removed/context lines. One block per changed file.
  Produced either from a File Change Event or from diff-text detected in a
  tool result.
- **Expandable Section**: A transcript element (reasoning block or tool
  call) carrying a discrete expand/collapse state — collapsed, expanded, or
  (for long content) tail-windowed — plus the full content and a bounded
  view of it.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Developers can identify every line the agent added, removed,
  or left untouched in a changed file directly from the in-session
  transcript, without running a separate diff command, in both the TUI and
  the CLI.
- **SC-002**: A transcript containing a long reasoning block and several
  tool calls fits within a single screen at default collapsed state, so the
  most recent answer and tool summaries remain visible without scrolling.
- **SC-003**: A developer can move from the collapsed summary of any
  reasoning section or tool call to its full content in a single activation,
  and return to the collapsed state in a single activation.
- **SC-004**: 100% of file edits produced during a sample turn are
  represented as correctly-styled diffs with accurate addition/removal
  counts, verified against the actual on-disk change.
- **SC-005**: The non-interactive CLI output for the same turn contains the
  same diff information (as plain text) and the full reasoning and tool
  output, with zero content hidden behind an interaction.

## Assumptions

- The primary reference for the look, feel, and interaction model of the
  diffs, expandable thinking sections, and expandable tool calls is the
  `crush` project at `~/Development/crush` (the Charm "crush" CLI), whose
  `internal/ui` provides a proven model: a `diffdetect` module that
  recognizes unified diffs, a `diffview` module that renders
  unified/split diffs with syntax highlighting, an `Expandable` interface
  for items that collapse/expand, and a three-state thinking view
  (collapsed → tail-window → full). The design will follow these patterns
  adapted to the existing Rust TUI/CLI architecture; exact crate/dependency
  choices are deferred to the plan per constitution Principle VIII.
- The existing interactive TUI (`joey-tui`) already streams reasoning and
  tool events and has a reasoning-visibility toggle; this feature extends
  that transcript rather than replacing it.
- The existing streaming CLI renderer (`joey-cli`) already renders a
  reasoning box and tool-completion lines; this feature adds diff
  rendering and an expand/collapse model appropriate to a non-interactive
  surface (i.e. fully-shown plain text).
- Both surfaces consume the same underlying agent-event stream, so the diff
  and expansion data must be derivable from events/data already available
  or added additively to that stream — the two surfaces must not diverge
  into separate data sources (constitution Principle II: CLI/TUI parity).
- File "before" content is captured per session by recording every file the
  agent reads during that session as a content baseline (in-session only,
  not persisted across sessions). This matches the reference project's
  per-session file-read tracking. Baselines are bounded to files actually
  read in-session, keeping the memory/IO cost proportional to read volume
  rather than to the working tree.
- Diffs MUST apply per-language syntax highlighting to code lines in
  addition to add/remove/context coloring (Clarification 2026-07-25). The
  specific syntax-highlighting engine is a plan-phase decision that MUST
  be justified against constitution Principle VIII: the chosen dependency's
  binary-size, compile-time, and transitive surface MUST be recorded
  against the alternatives in the feature's `research.md`, and the
  per-line highlighting path MUST carry an explicit performance budget
  (target latency for rendering a diff of N lines) since it is a hot path
  in streaming output. Syntax highlighting MUST degrade gracefully for
  unrecognized languages (fall back to plain add/remove/context coloring
  with no error).
- Diff rendering must remain correct when the agent uses the existing
  fuzzy/patch-based editing tools; the "before" baseline is the file
  content as last seen by the agent, not a git working-tree snapshot, so
  this works identically inside and outside git repositories.
