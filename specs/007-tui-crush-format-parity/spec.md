# Feature Specification: Crush-Style Expandable Block Formatting (TUI)

**Feature Branch**: `007-tui-crush-format-parity`

**Created**: 2026-07-29

**Status**: Draft

**Input**: User description: "modify joey-agent and format the TUI exactly like the crush coding agent - copy the formatting exactly (expandable thinking blocks, expandable tool call blocks, and expandable terminal command blocks) - copy the UI formatting but keep the current styling."

## Clarifications

### Session 2026-07-29

- Q: "Copy the formatting but keep the current styling" — does this mean keep joey-agent's aurora-synthwave theme colors and only adopt crush's layout/structure, OR adopt crush's exact colors too? → **Informed guess**: Keep joey-agent's existing `theme.rs` palette and semantic styling (colors, icons, gradient). Copy crush's *structural layout, component composition, and expand/collapse affordance model* verbatim. The word "styling" in the request refers to the visual color theme, while "formatting" refers to the structural arrangement of the blocks. This is the interpretation that delivers a recognizable crush layout without discarding joey-agent's established visual identity.
- Q: The existing feature 005 already implemented expandable thinking and expandable tool calls — is this a net-new feature or a refinement of 005? → **Informed guess**: This is a refinement/parity upgrade of the 005 implementation. joey-agent already has a three-state reasoning cycle and a tool expand toggle, but their *visual presentation* does not match crush: thinking blocks lack crush's bordered-box treatment and `Thought for Ns` footer; tool blocks lack crush's icon+name+param header layout; and there is NO distinct terminal-command block type at all (terminal tool calls render identically to every other tool). This feature brings the 005 primitives to crush's exact layout, and adds the missing terminal-command block as a first-class expandable type.
- Q: How does presentation data the crush layout needs (exit code, full tool args, full result text, thinking duration) reach the renderer, given the current `AgentEvent::ToolEnd` carries only `name`, `is_error`, a one-line `result_preview`, and `duration_secs`, and `TranscriptItem::Tool.full_args`/`full_result` are never populated? → A: Extend `AgentEvent` additively — add a typed `exit_code: Option<i64>` to `ToolEnd` and carry full result text + args via the existing result path. This is explicit, typed, backward-compatible (additive field with default), and benefits both the TUI and CLI surfaces. No parsing of free-text results, no brand-new event type.
- Q: Where does the reasoning/thinking duration for the `Thought for Ns` footer come from, given the event stream has only `ReasoningDelta(String)` chunks and no reasoning-end/duration marker? → A: Derive the duration entirely in the TUI state — timestamp the first `ReasoningDelta` of a block and subtract from when the block closes (when content starts or reasoning flushes). No `AgentEvent` surface changes, avoiding a NON-NEGOTIABLE public-surface extension (constitution Principle VII) for cosmetic display data.
- Q: What input modality toggles expand/collapse — keyboard-only (today's Ctrl+E/Ctrl+G on the focused item), mouse-click, or both — given crush's affordance text says "click or space" but joey-tui only has scroll-wheel mouse support today? → A: Both — keep the existing keyboard bindings (Ctrl+E reasoning / Ctrl+G tool) as the primary path AND add mouse-click-to-toggle on a transcript item (a click focuses the item and toggles its expand state). Additive; no regression to existing keys (constitution Principle VII). This makes the "click to expand" affordance text honest.

## User Scenarios & Testing *(mandatory)*

<!--
  These stories are ordered by importance and are independently shippable.
  Each one can be implemented, tested, and delivered on its own, since they
  touch three distinct block types in the transcript renderer. The primary
  users are developers running `joey` interactively who want a transcript
  that reads like crush's: compact collapsed summaries that expand in place
  to reveal full content, with terminal commands rendered distinctly from
  ordinary tool calls.
-->

### User Story 1 - Expandable Thinking Blocks with Crush Layout (Priority: P1)

As a developer running the agent, when the model emits reasoning/thinking
content, I want that content rendered in a visually distinct bordered block
— collapsed to a compact summary by default with a clear "expand" affordance,
and expandable through the three-state cycle — that matches crush's thinking
layout, so that reasoning is recognizable, bounded, and consistent with the
reference UI while keeping joey-agent's colors.

**Why this priority**: Reasoning is the most visually prominent collapsible
content and is the block where joey-agent's current presentation diverges
most from crush (a bare `┄ reasoning` label with no box). Aligning it first
establishes the boxed, affordance-laden layout pattern that the other two
stories follow, and it builds on the three-state state machine already
shipped in feature 005 so the structural change is low-risk.

**Independent Test**: Trigger a turn that produces a reasoning block; confirm
it renders inside a bordered region, collapsed by default with a `… (N lines
hidden) [click or space to expand]`-style affordance, then cycle through the
three states and confirm each shows the expected content window and label.

**Acceptance Scenarios**:

1. **Given** a turn produces reasoning text longer than the collapsed cap,
   **When** the reasoning block renders, **Then** it appears inside a
   bordered region showing only the collapsed cap of lines plus an
   affordance line stating how many lines are hidden and how to expand.
2. **Given** a collapsed reasoning block, **When** the user activates the
   expand affordance, **Then** the block expands to show a tail window of
   the most recent reasoning with the same affordance text style as crush
   (`… N earlier lines hidden [click or space for full view]`).
3. **Given** a tail-windowed reasoning block, **When** the user activates
   the affordance again, **Then** the full reasoning content is shown with
   no truncation.
4. **Given** reasoning has finished streaming, **When** the block renders
   in any state, **Then** a `Thought for Ns` footer is shown (using
   joey-agent's theme colors) when a thinking duration is available,
   matching crush's footer placement and wording.
5. **Given** a reasoning block short enough to fit the collapsed cap,
   **When** it renders, **Then** the three-state cycle skips redundant
   states exactly as it does today (no behavior regression), but now
   inside the boxed layout.

---

### User Story 2 - Expandable Terminal-Command Blocks (Priority: P2)

As a developer, when the agent runs a terminal/shell command (a `terminal`
tool call), I want that command rendered as its own distinct expandable
block — a `$ command` prompt header, a plain-text output body, an exit-code
badge, and a collapsed-to-N-lines view that expands to the full output — so
that terminal commands are visually distinguishable from ordinary tool calls
and read exactly like crush's shell/bash rendering.

**Why this priority**: Terminal commands are the block type that is entirely
*missing* from joey-agent's current renderer (they fold into the generic
tool-call block) and are explicitly called out in the request. They are
second priority because they are high-frequency and high-information but do
not gate the reasoning-block layout pattern established in P1; the terminal
block reuses the expand machinery already proven by the tool block.

**Independent Test**: Run a turn that executes a shell command producing
multi-line output; confirm it renders with a `$ ` prompt header and a
collapsed output body, then expand it and confirm the full output and exit
code are shown, visually distinct from a non-terminal tool call.

**Acceptance Scenarios**:

1. **Given** the agent invokes a terminal command, **When** the command
   block renders, **Then** it appears with a `$ ` prompt followed by the
   command string as the header line, distinct from a generic tool call's
   icon+name header.
2. **Given** a terminal command that produced multi-line output, **When**
   the block renders collapsed, **Then** only a bounded number of output
   lines are shown with an affordance indicating how many lines are hidden
   (`… N more lines` for finished commands).
3. **Given** a collapsed terminal-command block, **When** the user expands
   it, **Then** the full output is revealed in place.
4. **Given** a terminal command that exited with a non-zero code, **When**
   the block renders, **Then** an `(exit N)` badge is shown on the header
   line.
5. **Given** a terminal command that is still running, **When** the block
   renders, **Then** a running indicator (`busy`-colored spinner) is shown
   on the header while the command executes. (Note: the terminal tool is a
   blocking call that returns full output at completion — it does not stream
   interim output — so the tail-biased streaming window from crush is not
   implemented in this feature; the running indicator is the scoped
   deliverable.)

---

### User Story 3 - Expandable Tool-Call Blocks with Crush Header Layout (Priority: P3)

As a developer, when the agent invokes a non-terminal tool, I want the tool
call rendered with crush's header layout — a status icon, the bold tool name,
and the primary parameter value on a single header line, with the result body
indented beneath and collapsed to a bounded height with a hidden-line
affordance — so that the compact summary reads exactly like crush while
remaining expandable to full arguments and result.

**Why this priority**: joey-agent already renders expandable tool calls from
feature 005, so this story is purely a layout refinement (adopt crush's
icon+name+param header and indented-body-with-affordance composition) rather
than new functionality. It is the lowest-priority slice because the existing
tool block is functional and the most impactful visual gaps (boxed thinking,
distinct terminal blocks) are covered by P1 and P2; this story completes the
full layout parity.

**Independent Test**: Run a turn that calls a non-terminal tool with
arguments and a result; confirm the header renders as icon + bold name +
primary parameter, the result is collapsed to a bounded height with a
hidden-line affordance, and expanding reveals the full arguments and result.

**Acceptance Scenarios**:

1. **Given** the agent invokes a tool, **When** the tool call renders, **Then**
   the header line is composed of a status icon, the tool name (bold), and
   the primary parameter value, all on a single line.
2. **Given** a completed tool call with a multi-line result, **When** it
   renders collapsed, **Then** the result body is indented and bounded to a
   fixed number of lines with an affordance stating how many lines are hidden
   (`… (N lines hidden) [click or space to expand]`).
3. **Given** a collapsed tool-call block, **When** the user expands it, **Then**
   the full arguments and full result are revealed.
4. **Given** a tool call that is still running, **When** it renders, **Then**
   a running status is indicated on the header with no result body yet.
5. **Given** a tool call that failed, **When** it renders, **Then** the error
   status is reflected in the header icon and the result body shows the
   error content.

---

### Edge Cases

- What happens when reasoning content is empty or whitespace-only? The
  thinking block must not be rendered at all (matching the existing
  reasoning-visibility gate and crush's `ShouldRenderAssistantMessage`).
- What happens when a terminal command produces no output? The block must
  render the header line only (`$ command`), with no output body and no
  truncation affordance.
- What happens when a terminal command output exceeds the screen height even
  when expanded? The expanded block must not push later transcript items off
  an unbounded area; the transcript scroll region already handles this, but
  the affordance text must remain correct.
- What happens when the same command is run multiple times? Each invocation
  must render as a distinct block (not collapse into one), matching crush's
  per-call identity.
- What happens in a non-interactive / one-shot CLI context? All blocks
  resolve to fully-shown plain text (no borders, no affordances, no hidden
  lines), preserving the existing CLI parity guarantee from feature 005
  FR-012. The crush layout applies to the interactive TUI only.
- What happens when a tool call has a very long primary parameter that does
  not fit the header line? It must be truncated with an ellipsis when
  collapsed and wrap when expanded, matching crush's `toolParamList`
  behavior.
- What happens when the terminal width is very narrow? The boxed thinking
  region and the header layouts must degrade gracefully (wrap rather than
  overflow) and must not produce misaligned borders.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The TUI MUST render reasoning/thinking content inside a
  visually distinct bordered region (a box) rather than a bare label, while
  preserving joey-agent's existing theme colors for the border and text.
- **FR-002**: A collapsed reasoning box MUST show only a bounded cap of
  lines plus an affordance line that states the number of hidden lines and
  how to expand, worded in the style of crush's thinking affordance
  (`… (N lines hidden) [click or space to expand]`).
- **FR-003**: The reasoning expand cycle MUST remain the existing
  three-state machine (collapsed → tail-window → full → collapsed) with the
  existing skip rule for short content; this feature changes only the
  *visual presentation* of each state, not the state logic.
- **FR-004**: When reasoning has finished and a thinking duration is
  available, the reasoning box MUST display a `Thought for Ns` footer using
  joey-agent's theme colors, matching crush's footer placement and wording.
  The duration is DERIVED in the TUI state (timestamp of the first
  `ReasoningDelta` of the block minus the moment the block closes) — no
  `AgentEvent` surface change is required for this data.
- **FR-005**: The TUI MUST render terminal/shell command tool calls
  (`terminal` tool) as a distinct transcript block type, separate from
  generic tool-call blocks, using a `$ command` prompt header layout.
- **FR-006**: A terminal-command block MUST display the command's output as
  plain text beneath the prompt header, collapsed to a bounded number of
  lines, with an affordance indicating hidden lines.
- **FR-007**: A terminal-command block MUST expand to reveal its full output
  in place on user activation.
- **FR-008**: A terminal-command block whose command exited non-zero MUST
  display an `(exit N)` badge on the header line, sourced from the
  additively-added `exit_code` field on `ToolEnd` (see FR-018).
- **FR-009**: A running terminal-command block MUST show a running indicator
  (a `busy`-colored spinner) while the command executes, before `ToolEnd`
  arrives. (Note: the terminal tool is a blocking `await` that returns full
  output at completion — it does not stream interim `ToolProgress` events —
  so the "tail-biased streaming window" from crush is not architecturally
  possible in this feature. The running indicator is the scoped deliverable;
  true live streaming would require a separate feature to emit stdout lines
  as `ToolProgress` from the terminal tool's subprocess pipe and is out of
  scope here.)
- **FR-010**: The TUI MUST render non-terminal tool-call headers in crush's
  composition: status icon, bold tool name, and the primary parameter value,
  on a single header line, followed by an indented result body.
- **FR-011**: A collapsed tool-call result body MUST be bounded to a fixed
  number of lines with an affordance stating the number of hidden lines,
  worded in crush's style.
- **FR-012**: Tool-call blocks MUST remain expandable to reveal the full
  arguments and full result, preserving the existing expand behavior from
  feature 005. (Today `full_args`/`full_result` are uninitialized; the
  additive `ToolEnd` extension in FR-018 is what populates them.)
- **FR-013**: All three block types MUST be independently expandable and
  collapsible per-item via TWO additive input paths: (a) the existing
  keyboard bindings on the focused item (Ctrl+E for reasoning, Ctrl+G for
  tool/terminal blocks), unchanged, and (b) a mouse click on the rendered
  block, which focuses the item and toggles its expand state. A global
  "expand all / collapse all" control remains out of scope.
- **FR-014**: All structural and layout changes MUST use joey-agent's
  existing `theme.rs` palette and semantic tokens; no crush-specific RGB
  values, palette constants, or theme struct fields are introduced. The
  border color, text colors, icon colors, and badges all draw from the
  existing `Theme` fields (e.g. `fg_subtle`, `fg_more_subtle`, `success`,
  `error`, `busy`).
- **FR-015**: The feature MUST NOT introduce regressions: existing
  streaming, transcript scrolling, banner, usage, search, subagent, and
  activity-panel rendering MUST remain intact, and all existing expand/collapse
  keybindings and focus behavior MUST continue to work.
- **FR-016**: In non-interactive / one-shot CLI contexts, all three block
  types MUST resolve to fully-shown plain text (no borders, no affordances,
  no hidden lines), preserving the feature 005 FR-012 parity guarantee.
  The crush layout is an interactive-TUI presentation only.
- **FR-017**: The feature MUST classify a tool call as a terminal-command
  block when the tool name corresponds to the shell/terminal execution tool,
  and as a generic tool-call block otherwise. The classification MUST be
  data-driven (based on the tool name/event), not a hardcoded allow-list of
  command strings.
- **FR-018**: To supply the presentation data the crush layout requires
  (exit codes, full arguments, full result text), the `AgentEvent::ToolEnd`
  variant MUST be extended additively with a typed `exit_code: Option<i64>`
  field, and the tool-execution path MUST populate the tool item's full
  arguments and full result text so they flow to both the TUI and the
  one-shot CLI. The extension MUST be backward-compatible: the new field
  defaults to `None`/empty so existing producers and consumers (including
  feature-005 tests) are unaffected without modification. No free-text
  parsing of tool results is used to recover structured data.

### Key Entities *(include if feature involves data)*

- **Reasoning Box**: The visual representation of a completed or in-progress
  reasoning/thinking block within the transcript: a bordered region
  containing the rendered reasoning text (windowed to the current expand
  state), a state label, a hidden-line affordance, and (when finished) a
  `Thought for Ns` footer whose duration is derived in the TUI state from
  the first-`ReasoningDelta`-to-block-close interval. Backed by the existing
  `TranscriptItem::Reasoning` variant and `ReasoningExpandState`, extended
  with box-render data (and a first-delta timestamp) only.
- **Terminal-Command Block**: A new transcript presentation of a
  `terminal`/shell tool call: a `$ command` prompt header, an optional
  `(exit N)` badge, a plain-text output body bounded to a collapsed line
  count with a hidden-line affordance, an expand toggle, and a running
  indicator while the command executes. Reuses the tool-call event data
  (name, args, result, status) but is rendered as a distinct block type.
- **Tool-Call Header**: The compact summary of a non-terminal tool call:
  status icon, bold tool name, and the primary parameter value on a single
  line, plus the indented result body beneath. Reuses the existing
  `TranscriptItem::Tool` variant's fields, now including the additively-
  populated `full_args`/`full_result` (see FR-018); this entity describes
  the new header *layout*, not fundamentally new data.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer can visually distinguish a reasoning block, a
  terminal-command block, and a generic tool-call block at a glance in the
  TUI transcript, by layout alone, without reading the content.
- **SC-002**: A transcript containing a long reasoning block, a terminal
  command with multi-line output, and several tool calls fits within a
  single screen at default collapsed state, with each block showing only a
  compact summary and an expand affordance.
- **SC-003**: A developer can move any of the three block types from its
  collapsed summary to its full content in a single activation, and return
  to the collapsed state in a single activation.
- **SC-004**: 100% of the TUI's new structural elements (boxes, headers,
  badges, affordances) use the existing joey-agent theme palette — verified
  by the absence of any new color constants in `theme.rs`.
- **SC-005**: The non-interactive CLI output for the same turn remains
  unchanged in content (full reasoning, full command output, full tool
  results as plain text), confirming the crush layout did not leak into the
  one-shot surface.

## Assumptions

- The primary reference for the *layout, component composition, and
  expand/collapse affordance model* of the three block types is the `crush`
  project at `~/Development/crush` (Charm's "crush" CLI). Specifically:
  `internal/ui/chat/assistant.go` (the bordered `ThinkingBox`, the
  three-state `thinkingViewMode`, the `Thought for Ns` footer, and the
  `… (N lines hidden) [click or space to expand]` /
  `… N earlier lines hidden [click or space for full view]` affordance
  strings); `internal/ui/chat/tools.go` (the `toolHeader` icon + bold name
  + primary-parameter layout, the `toolOutputPlainContent` indented body
  with `responseContextHeight` bounding and hidden-line affordance); and
  `internal/ui/chat/shell.go` + `internal/ui/chat/bash.go` (the `ShellItem`
  `$ ` prompt header, the `shellMaxCollapsedLines` bounding, the
  `(exit N)` badge, the running spinner, and the collapsed-output window
  (`shellMaxCollapsedLines`). The design ports these structural patterns to joey-agent's
  ratatui-based `widgets.rs`; exact widget/crate choices are deferred to the
  plan per constitution Principle VIII.
- "Keep the current styling" means the existing `joey-agent/crates/joey-tui/src/theme.rs`
  aurora-synthwave palette and semantic `Theme` struct are the source of all
  colors, icons, and modifiers used by the new layout. No crush color values,
  no new palette constants, and no new `Theme` fields are introduced by this
  feature. Where crush uses a semantic token (e.g. `ThinkingTruncationHint`,
  `Tool.Body`, `ShellExitCode`), joey-agent maps it onto the closest existing
  `Theme` field.
- Feature 005 already shipped the three-state reasoning cycle
  (`ReasoningExpandState`), the per-item tool expand toggle, and the
  `FileDiff` block, along with their keybindings and focus handling. This
  feature reuses all of that machinery and changes only the *rendering* of
  reasoning and tool blocks, plus adds the new terminal-command block. No
  state-machine or event-model changes are in scope.
- The terminal-command block is sourced from the existing tool-call event
  stream: a `terminal` tool invocation already carries the command string
  (in its args), the output (in its result), the status, and (when
  available) the exit code. The feature classifies by tool name and renders
  accordingly; it does not require new events or new result metadata beyond
  what the `terminal` tool already provides. The exit code reaches the
  renderer via the additively-added `exit_code: Option<i64>` field on
  `ToolEnd` (FR-018); full args/result reach it via the now-populated
  `full_args`/`full_result` item fields. Both the TUI and the one-shot CLI
  receive the same extended data (constitution Principle II).
- The crush layout is an interactive-TUI presentation. The non-interactive /
  one-shot CLI renderer continues to emit fully-expanded plain text for all
  three block types (feature 005 FR-012 parity), so no affordances, borders,
  or hidden lines appear when there is no interaction layer.
- Both the TUI and CLI consume the same underlying agent-event stream, so
  the new block classification and any new presentation data must be
  derivable from events/data already available or added additively — the two
  surfaces must not diverge into separate data sources (constitution
  Principle II).
- The existing transcript scroll region, focus model, and per-item keybinding
  dispatch (Tab/Shift-Tab to focus an item, Ctrl+E / Ctrl+G to expand a
  focused item) remain the primary interaction model. This feature ADDS
  mouse-click-to-toggle on a rendered block (a click focuses the item and
  flips its expand state) as an additive input path alongside the existing
  keys; it does not replace them. Mouse capture is already enabled in
  `joey-tui/src/app.rs` (`EnableMouseCapture`) and a mouse-event handler
  exists for scroll, so click-to-toggle is an extension of the existing
  crossterm mouse routing, not a new subsystem.
