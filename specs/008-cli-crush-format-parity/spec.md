# Feature Specification: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Feature Branch**: `008-cli-crush-format-parity`

**Created**: 2026-07-30

**Status**: Draft

**Input**: User description: "apply the new design changes to the TUI to the CLI - I want the CLI to look like the TUI except without the expandable sections - make everything fully expanded."

## Clarifications

### Session 2026-07-30

- Q: "Look like the TUI except without the expandable sections" — does this mean adopt the TUI's structural layout (boxed reasoning, `$ command` terminal headers, icon+name+param tool headers) verbatim but force every block into its fully-expanded content view (no `[click or space to expand]` affordances, no "N more lines"/"N lines hidden" truncation), OR does it additionally mean porting the bordered-box drawing characters the TUI uses for reasoning into the CLI? → **Informed guess**: Both. The intent is visual parity with the TUI's crush design (same headers, same box borders, same footers/badges), with the single modification that content is never collapsed or truncated — the CLI always renders the equivalent of the TUI's "full" expand state for every block. The CLI already uses box-drawing characters for its reasoning block (`┌─ Reasoning`), so extending that visual language is consistent. "Without the expandable sections" means: remove the affordance text and the collapsed/tail-window slicing, never hide lines, never show "… N more lines" prompts — always show full content.

- Q: The TUI's three-state reasoning cycle (collapsed → tail-window → full) and per-item tool expand toggle are interactive concepts (Ctrl+E / Ctrl+G / mouse-click). The CLI one-shot renderer is non-interactive. Does this feature change the CLI's reasoning box to match the TUI's "full" state presentation (boxed border + full content + `Thought for Ns` footer)? → **Informed guess**: Yes. The CLI reasoning block should render the same bordered box, the same full content (no tail-windowing), and — where a duration can be derived — the `Thought for Ns` footer. This is the "fully expanded" reasoning state. Since the CLI is non-interactive, there is no cycle; the block is always in its full-content presentation.

- Q: How does the reasoning "Thought for Ns" duration reach the CLI renderer, given the TUI derives it from timestamping the first `ReasoningDelta` to block-close, and the CLI streams `ReasoningDelta` events live via `render_turn`? → A: Derive it the same way the TUI does, but in the CLI's own streaming state: capture a `start: Option<Instant>` on the first `ReasoningDelta` of a reasoning block, and compute `start.elapsed()` when the block closes (on `ContentDelta`, `ToolStart`, or `Done`). No `AgentEvent` surface change is required — the duration is a presentation-layer derivation, consistent with the 007 spec's research decision (which explicitly avoided an `AgentEvent` change for cosmetic display data). The CLI already tracks `reasoning_line_count` as transient streaming state; adding a `reasoning_started: Option<Instant>` alongside it is the same pattern.

- Q: Terminal-command blocks in the TUI derive their `$ command` string from the tool `summary` (which for the terminal tool equals the command), and show output from `full_result`. The CLI's `ToolEnd` arm currently only prints a 120-char trimmed `result_preview` in verbose mode and ignores `full_result`. Should the CLI now print the full output (fully expanded) under a `$ command` header? → A: Yes. This is the core of "fully expanded": the CLI terminal-command block prints the `$ command` header (from `summary`, matching the TUI), the `(exit N)` badge (already implemented in 007 T027 when non-zero), and the FULL output body from the `full_result` field (which 007 T032 already carries on `ToolEnd`), with no line truncation and no "… N more lines" affordance. For the CLI the block is always in the TUI's expanded state.

- Q: Generic tool-call blocks in the TUI show icon + emoji + bold name + primary param + duration + expand hint, then an expanded body with `args:`/`result:` sections. Should the CLI adopt the same header composition and always render the full result body (since there's no expand toggle)? → A: Yes, with the CLI-appropriate adaptation: adopt the crush header composition (status icon, tool name, primary param, duration) matching the TUI, and always show the full result body from `full_result` (fully expanded, no bounding, no affordance). The `args:` line is omitted in the CLI because `full_args` is never populated (007 contracts/agent-event.md Approach A) and the primary param already comes from `summary` — printing a redundant `args:` summary line that duplicates the header adds noise without information. The `result:` body is shown indented beneath the header when the result is non-empty. The `▸`/`▾` expand-hint glyph is omitted (there is nothing to toggle). The `emoji` glyph: the CLI currently shows the tool `emoji` as the header icon (`⚡` fallback); the TUI uses a status-driven icon (`✓`/`✗`/`⟳`). The CLI should adopt the TUI's status-driven icon on the header line for parity, since the "look like the TUI" intent favors the status-icon composition.
- Q: The TUI renders execution duration (`{:.1}s`) on BOTH the terminal-command header (widgets.rs:382-386) and the generic tool-call header (widgets.rs:435-437), and the CLI's current tool line already shows duration via `fmt_duration` (`(12.3s)` / `failed (12.3s)`). The spec's FR-007 enumerated the generic header as only "icon + tool name + primary parameter" and FR-004/005/006 (terminal) said nothing about duration. For full "look like the TUI" parity, should the new crush-style headers retain the duration display? → A: Yes — show `{:.1}s` duration on BOTH terminal-command and generic tool-call headers. This matches the TUI reference (full parity) and retains the CLI's existing behavior (avoiding an FR-011 regression).
- Q: FR-007 said the generic tool header uses a "status-driven icon (`✓` done / `✗` failed)" and noted the `emoji` "MAY be retained as a secondary element." The TUI renders BOTH: the status icon AND the emoji, then the bold name (widgets.rs:440-447). The current CLI shows only the `emoji` as the icon (with `⚡` fallback), no status icon. For the new CLI header, should the `emoji` be retained alongside the status icon (full TUI parity), or replaced by it? → A: Render BOTH the status icon and the emoji, matching the TUI composition exactly (status icon + emoji + bold name + param + duration). This is what "look like the TUI" means, and retaining the emoji preserves existing per-tool visual identity at zero data cost (the `emoji` field is already present on `ToolStart`).
- Q: The spec said the CLI reasoning footer should match the TUI's "`Thought for Ns`" footer, but the TUI code (widgets.rs:333-336) actually renders the footer as `└─ Thought for {:.1}s` — it closes the box border (`└─`) AND uses a one-decimal float format (`Thought for 3.2s`), not the integer-second wording (`Thought for Ns`) used throughout the spec. Which footer format should the CLI use? → A: Use `└─ Thought for {:.1}s` (one decimal place, border-close prefix), matching the TUI exactly. The CLI already renders the reasoning box with `┌─` border characters, so the `└─` border-closing prefix is consistent with the established visual language and makes the box read as a closed region. The `{:.1}s` format also matches how the CLI/TUI format tool durations (`{:.1}s`), so reasoning and tool durations display consistently.
- Q: The spec's Edge Cases establish a deliberate divergence from 007's FR-016: the crush layout applies in ALL capability modes (Full / Reduced / NonInteractive). But 007's block-layout contract §5 says the crush layouts do NOT apply when non-interactive (plain text only). Should the bordered `┌─ reasoning` box and the `$ command` / icon+name headers render with full structural styling even when output is piped/redirected, or should NonInteractive strip the visual structure to plain text (no borders, no box chars)? → A: Render the full structural layout (borders, headers, box chars) in ALL modes including NonInteractive. ANSI color codes are emitted in all modes via the existing `.ansi().paint()` pattern (no new application-level stripping layer); the box-drawing characters (`┌─`, `└─`, `$`) are plain UTF-8 text that survive piping/redirecting and remain meaningful structure in logs. Stripping the structural layout would make NonInteractive output diverge from interactive output, defeating the parity goal. The existing `RenderCapability` already gates animations (spinners/carets) separately via `animations_on`.

## User Scenarios & Testing *(mandatory)*

These stories are ordered by importance and are independently shippable. Each
maps to one of the three block types in the CLI transcript renderer. The
primary users are developers running `joey` one-shot (`joey -z "..."`) or in
the REPL who want the streaming CLI output to read like the TUI's crush
design — but always fully expanded, since the CLI has no expand/collapse
interaction.

### User Story 1 - Fully-Expanded Reasoning Box with TUI Layout (Priority: P1)

As a developer running the agent in the CLI with reasoning visible
(`display.show_reasoning`), when the model emits reasoning/thinking content,
I want that content rendered inside the same bordered box the TUI uses — the
`┌─ Reasoning` header, the full reasoning text with no truncation, and a
`└─ Thought for {:.1}s` footer — so that reasoning in the CLI reads like the TUI's
fully-expanded reasoning state, with no hidden lines and no
"click or space to expand" affordance.

**Why this priority**: Reasoning is the block where the CLI diverges most
visually from the TUI today: it has a box, but the box closes with a
"N lines of reasoning" count line (a compact summary) and no footer, whereas
the TUI shows a bordered box with a `└─ Thought for {:.1}s` footer. Aligning it
first establishes the "fully-expanded TUI layout in the CLI" pattern that the
other two stories follow, and it reuses the CLI's existing box-drawing code
so the structural change is low-risk.

**Independent Test**: Trigger a CLI turn that produces a long reasoning
block (multi-line); confirm it renders inside a bordered `┌─ Reasoning`
region showing ALL reasoning lines (no tail-window, no "N lines hidden"),
closes with the TUI-style footer (`└─ Thought for {:.1}s`) when a duration is
derivable, and never shows a "click or space to expand" affordance.

**Acceptance Scenarios**:

1. **Given** a CLI turn produces reasoning text of any length, **When** the
   reasoning block renders, **Then** it appears inside a bordered region with
   the `┌─ Reasoning` header line, showing the FULL content (no tail-window
   slicing, no `MAX_COLLAPSED_LINES`/`MAX_TAIL_WINDOW_LINES` bounding).
2. **Given** a reasoning block that has finished streaming, **When** the block
   closes (a non-reasoning event arrives), **Then** a `└─ Thought for {:.1}s`
   footer is shown beneath the content when a thinking duration greater than
   zero was derived, matching the TUI's footer placement and wording.
3. **Given** any reasoning block, **When** it renders, **Then** no
   "… (N lines hidden)" or "[click or space to expand]" affordance text
   appears anywhere — the CLI always renders the equivalent of the TUI's
   full expand state.
4. **Given** reasoning content that is empty or whitespace-only, **When** the
   block would render, **Then** no box is drawn at all (matching the existing
   reasoning-visibility gate and the TUI's empty-reasoning behavior).
5. **Given** `display.show_reasoning` is false or `--quiet` is set, **When**
   reasoning deltas arrive, **Then** no reasoning box is rendered (existing
   gate preserved — no regression).

---

### User Story 2 - Fully-Expanded Terminal-Command Block (Priority: P2)

As a developer, when the agent runs a terminal/shell command (a `terminal`
tool call) in the CLI, I want that command rendered as its own distinct block
— a `$ command` prompt header (matching the TUI), an `(exit N)` badge, and
the FULL command output beneath it with no truncation — so that terminal
commands are visually distinguishable from ordinary tool calls and read like
the TUI's fully-expanded terminal block, without any "… N more lines"
collapsed affordance.

**Why this priority**: Terminal commands are the block type the CLI renders
least like the TUI today: the CLI lumps them into the generic tool line
(icon + name + 120-char trimmed preview in verbose mode) and ignores the
`full_result` that 007 T032 already carries. They are high-frequency and
high-information, so distinct + fully-expanded rendering delivers immediate,
visible value. They are second priority because they do not gate the
reasoning-block layout established in P1 and reuse the full-output plumbing
(`full_result`) already wired by 007.

**Independent Test**: Run a CLI turn that executes a shell command producing
multi-line output (e.g. `ls -la crates`); confirm it renders with a
`$ ls -la crates` header (distinct from a generic tool's icon+name header)
and the FULL output body beneath it, with no "… N more lines" affordance.
Run a failing command (`false`); confirm the header shows `(exit 1)` in the
error color.

**Acceptance Scenarios**:

1. **Given** the agent invokes a terminal command in the CLI, **When** the
   command block renders on `ToolEnd`, **Then** it appears with a `$ `
   prompt followed by the command string (sourced from the tool `summary`)
   as the header line, visually distinct from a generic tool call's
   icon+name header, and displays the execution duration (`{:.1}s`).
2. **Given** a terminal command that produced multi-line output, **When** the
   block renders, **Then** the FULL output is shown beneath the header
   (sourced from `full_result`), with no line bounding and no
   "… N more lines" affordance.
3. **Given** a terminal command that exited with a non-zero code, **When**
   the block renders, **Then** an `(exit N)` badge is shown on the header
   line (in the error color), sourced from the `exit_code` field (already
   wired by 007 T027; this story ensures it renders on the terminal-block
   header, not the generic tool line).
4. **Given** a terminal command that exited zero, **When** the block renders,
   **Then** no `(exit N)` badge is shown (zero exit is the implicit success).
5. **Given** a terminal command that produced no output, **When** the block
   renders, **Then** only the `$ command` header line appears, with no
   output body and no affordance (edge case).
6. **Given** the same command is run multiple times, **When** each completes,
   **Then** each invocation renders as a distinct block (not folded into
   one), matching the TUI's per-call identity.

---

### User Story 3 - Fully-Expanded Tool-Call Block with TUI Header Layout (Priority: P3)

As a developer, when the agent invokes a non-terminal tool in the CLI, I want
the tool call rendered with the TUI's crush header composition — a
status-driven icon, the bold tool name, and the primary parameter on a single
header line, with the full result body indented beneath — so that the compact
header reads like the TUI while the result is always fully shown (no
collapsed bounding, no "N lines hidden" affordance).

**Why this priority**: The CLI already renders tool calls (icon + gradient
name + 120-char preview in verbose mode), so this story is a layout
refinement (adopt the TUI's icon+bold-name+param header and full-result body)
rather than net-new functionality. It is lowest priority because the existing
tool line is functional and the most impactful gaps (boxed reasoning,
distinct terminal blocks) are covered by P1 and P2; this story completes the
full CLI↔TUI layout parity.

**Independent Test**: Run a CLI turn that calls a non-terminal tool with a
multi-line result; confirm the header renders as status icon + tool name +
primary parameter (matching the TUI), and the full result body is indented
beneath with no "… (N lines hidden)" affordance and no 120-char trim.

**Acceptance Scenarios**:

1. **Given** the agent invokes a non-terminal tool in the CLI, **When** the
   tool call renders on `ToolEnd`, **Then** the header line is composed of a
   status-driven icon (`✓` done / `✗` failed), the tool `emoji`, the tool
   name, the primary parameter value (from `summary`), and the execution
   duration (`{:.1}s`), matching the TUI's full header composition.
2. **Given** a completed tool call with a multi-line result, **When** it
   renders, **Then** the FULL result body is shown indented beneath the
   header (sourced from `full_result`), with no `MAX_TOOL_OUTPUT_LINES`
   bounding and no "… (N lines hidden)" affordance.
3. **Given** a tool call whose result is empty, **When** it renders, **Then**
   only the header line appears, with no result body (edge case).
4. **Given** a tool call that failed, **When** it renders, **Then** the error
   status is reflected in the header icon (`✗`) and the result body shows
   the error content in full.
5. **Given** the `tool_progress` config is `off`, **When** tool events
   arrive, **Then** no tool block is rendered (existing gate preserved — no
   regression); the `new`/`all`/`verbose` gating remains but the body is
   always full (no 120-char trim, no affordance) whenever a block is shown.

---

### Edge Cases

- What happens when reasoning content is empty or whitespace-only? The
  reasoning box MUST NOT be rendered at all, matching the existing gate and
  the TUI's `ShouldRenderAssistantMessage` behavior (no regression from
  007).
- What happens when a terminal command produces no output? The block renders
  the `$ command` header line only, with no output body and no affordance
  (mirrors the TUI empty-output edge case from 007 spec.md:171-172).
- What happens when terminal/tool output exceeds the screen height? The CLI
  is a scrolling stream (no bounded viewport like the TUI), so the full
  output simply scrolls — there is no truncation and no affordance. This is
  the correct "fully expanded" behavior; the TUI's bounding exists only
  because the TUI has a fixed viewport.
- What happens when the same command/tool is run multiple times? Each
  invocation renders as a distinct block, matching crush's and the TUI's
  per-call identity (no collapsing).
- What happens when `full_result` is empty but `result_preview` is non-empty
  (or vice versa)? The renderer MUST prefer `full_result` for the body when
  non-empty and fall back to `result_preview` otherwise, so a block is never
  left visually empty if any result text is available. (This guards against
  any producer path that populates one but not the other.)
- What happens in a non-interactive / piped-stdout context (NonInteractive
  capability)? The structural layout (headers, badges, box borders) still
  applies — it is plain text with ANSI styling that degrades to unstyled
  text when the terminal does not support color. The existing capability
  detection gates animations (spinners, caret, markdown reflow) but NOT the
  block layout; the block layout is static text and renders in all modes.
  (Formalized in FR-015; this is a DELIBERATE divergence from 007's FR-016,
  which gated the crush layout to interactive-TUI only — this feature
  explicitly brings the layout to the CLI in ALL modes, per the user's
  request.)
- What happens when the terminal width is very narrow? The box borders and
  header lines MUST wrap rather than overflow and MUST NOT produce
  misaligned borders. The CLI already uses `box_width()` clamped to
  [20, 80]; this is preserved.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The CLI reasoning renderer MUST render reasoning/thinking
  content inside a bordered box (the existing `┌─ Reasoning` visual) showing
  the FULL content — no tail-window slicing, no `MAX_COLLAPSED_LINES` /
  `MAX_TAIL_WINDOW_LINES` bounding — so that the CLI always renders the
  equivalent of the TUI's fully-expanded reasoning state.
- **FR-002**: When reasoning has finished streaming and a thinking duration
  greater than zero is available, the CLI reasoning box MUST display a
  `└─ Thought for {:.1}s` footer (border-close prefix + one-decimal seconds),
  matching the TUI's footer placement and wording exactly
  (widgets.rs:333-336). The duration MUST be derived in the CLI streaming
  state (timestamp the first `ReasoningDelta` of a block; compute the
  elapsed on block close) — no `AgentEvent` surface change is required
  (consistent with 007 research).
- **FR-003**: The CLI reasoning renderer MUST NOT emit any affordance text
  (`… (N lines hidden)`, `[click or space to expand]`, `… N earlier lines
  hidden`) — the CLI is always fully expanded.
- **FR-004**: The CLI MUST render terminal/shell command tool calls
  (`terminal` tool) as a distinct block type, separate from generic
  tool-call blocks, using a `$ command` prompt header layout (the command
  sourced from the tool `summary`, matching the TUI's terminal-block header
  source). The header MUST also display the execution duration (`{:.1}s`,
  sourced from `duration_secs` on `ToolEnd`), matching the TUI's
  terminal-block header (widgets.rs:382-386).
- **FR-005**: A terminal-command block in the CLI MUST display the command's
  FULL output as plain text beneath the prompt header, sourced from the
  `full_result` field on `AgentEvent::ToolEnd` (already carried per 007
  T032), with no line bounding and no `… N more lines` affordance. When
  `full_result` is empty but `result_preview` is non-empty, the renderer
  MUST fall back to `result_preview`.
- **FR-006**: A terminal-command block whose command exited non-zero MUST
  display an `(exit N)` badge on the header line, sourced from the
  `exit_code` field on `AgentEvent::ToolEnd` (already wired by 007 T027). A
  zero exit (or `None`) MUST NOT display a badge.
- **FR-007**: The CLI MUST render non-terminal tool-call headers in the
  TUI's crush composition: a status-driven icon (`✓` done / `✗` failed) in
  the success/error theme color, FOLLOWED BY the tool `emoji` (preserving
  per-tool visual identity, as the TUI does at widgets.rs:440-447), the
  tool name (bold), the primary parameter value (from `summary`), and the
  execution duration (`{:.1}s`, sourced from `duration_secs` on `ToolEnd`),
  on a single header line — matching the TUI's full header composition.
  (The CLI's current `emoji`-as-icon and gradient-name styling is replaced
  by the status-driven icon + emoji + themed-bold-name composition to match
  the TUI; the `emoji` is retained as a secondary element after the status
  icon, exactly as the TUI renders it.)
- **FR-008**: A non-terminal tool-call block in the CLI MUST display the
  FULL result body indented beneath the header, sourced from `full_result`
  (falling back to `result_preview` when `full_result` is empty), with no
  `MAX_TOOL_OUTPUT_LINES` bounding, no 120-character trim, and no
  `… (N lines hidden)` affordance. The body is shown whenever the result is
  non-empty and the `tool_progress` gate allows the block to render.
- **FR-009**: The feature MUST NOT introduce the TUI's expand/collapse
  affordances, state labels (`reasoning (tail)` / `reasoning (full)`), or
  `▸`/`▾` expand-hint glyphs into the CLI. The CLI has no expand state;
  every block is always in its fully-expanded content presentation.
- **FR-010**: All structural and layout changes MUST use the CLI's existing
  theme (`Theme::pantera()` via the existing `theme()` accessor in
  `render.rs`) and the existing `theme::gradient_*` helpers; no new color
  constants or theme struct fields are introduced. The CLI already imports
  `joey_core::theme::{self, Theme}` and renders via `Theme::pantera()`.
- **FR-011**: The feature MUST NOT introduce regressions: existing CLI
  streaming, the thinking-spinner animation, the streaming-caret animation,
  the per-tool spinner animation, the markdown reflow on `Done`, the banner,
  the turn-summary, the file-change diff rendering, and all other existing
  `AgentEvent` arms MUST remain intact. The existing `--quiet` /
  `display.show_reasoning` / `tool_progress` gates MUST continue to work.
- **FR-012**: The feature MUST NOT change any `AgentEvent` variant, any
  `TranscriptItem` variant, or any public surface in `joey-agent-core`,
  `joey-tui`, or `joey-tools`. All data required by the CLI layouts
  (`full_result`, `exit_code`, `summary`) is already present on the
  existing events (per 007 T027/T032). This feature is a `joey-cli`
  `render.rs` presentation change only.
- **FR-013**: The feature MUST classify a tool call as a terminal-command
  block when the tool name is `terminal`, and as a generic tool-call block
  otherwise — using the SAME classification logic as the TUI's
  `is_terminal_block` (007 T016), so the two surfaces agree on which calls
  are terminal. The classification is data-driven (tool name), not a
  hardcoded command-string allow-list.
- **FR-014**: The feature MUST NOT change the CLI's TUI counterpart
  (`joey-tui`): the TUI retains its expand/collapse interaction and
  affordances. This feature is CLI-only parity-in-reverse — bringing the
  TUI's *layout* to the CLI in an always-fully-expanded form.
- **FR-015**: The crush block layout (borders, `$ command` headers,
  icon+emoji+name headers, badges, footers) MUST render in ALL capability
  modes (`Full` / `Reduc` / `NonInteractive`), including when output is
  piped or redirected. ANSI color codes are emitted via `.ansi().paint()`
  in all modes (matching the existing reasoning-box and tool-line
  behavior); the structural characters (box-drawing `┌─`/`└─`, `$`,
  indentation, badges) are plain UTF-8 text that remain meaningful in
  logs and piped output. This feature does NOT introduce an
  application-level ANSI-stripping layer — it follows the existing
  codebase pattern (research.md §6), where the terminal or downstream
  consumer handles any color degradation. This is a DELIBERATE divergence
  from 007's FR-016 (which gated the crush layout to the interactive TUI
  only); this feature explicitly brings the layout to the CLI in ALL
  modes per the user's request. Animation gating (spinners, carets)
  remains unchanged via `animations_on = is_interactive
  && animations_enabled` — animations degrade in NonInteractive, never
  the block structure.

### Key Entities *(include if feature involves data)*

- **CLI Reasoning Box (fully-expanded)**: The streaming representation of a
  reasoning/thinking block in the CLI: a bordered `┌─ Reasoning` region
  containing the full reasoning text (no windowing), and a
  `└─ Thought for {:.1}s` footer whose duration is derived in the CLI
  streaming state from the first-`ReasoningDelta`-to-block-close interval. Backed by the existing
  `AgentEvent::ReasoningDelta` stream and the existing CLI transient state
  (`reasoning_open`, `reasoning_buf`, `reasoning_line_count`), extended with
  a `reasoning_started: Option<Instant>` only. No new event.
- **CLI Terminal-Command Block (fully-expanded)**: The streaming
  representation of a `terminal` tool call in the CLI: a `$ command` prompt
  header, an optional `(exit N)` badge, the execution duration (`{:.1}s`),
  and the full output body (sourced from `full_result`). Distinct from the
  generic tool block via `is_terminal_block`. Reuses the tool-call event
  data (`name`, `summary`, `full_result`, `exit_code`, `duration_secs`)
  already present on `AgentEvent::ToolStart`/`ToolEnd`.
- **CLI Tool-Call Header (crush composition)**: The compact summary of a
  non-terminal tool call in the CLI: status-driven icon + tool name +
  primary parameter + execution duration on one line, followed by the full
  indented result body. Reuses the existing `ToolEnd` fields (`name`,
  `summary`, `full_result`, `duration_secs`, `is_error`). This entity
  describes the new header *layout* and full-body rendering, not
  fundamentally new data.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer can visually distinguish a reasoning block, a
  terminal-command block, and a generic tool-call block at a glance in the
  CLI transcript, by layout alone — matching the TUI's visual
  distinguishability (07 SC-001) but in the always-fully-expanded CLI form.
- **SC-002**: A CLI transcript containing a long reasoning block, a terminal
  command with multi-line output, and several tool calls shows the FULL
  content of every block (no hidden lines, no "… N more lines", no 120-char
  trims, no affordances), confirming the "fully expanded" intent.
- **SC-003**: A developer running the same turn in the CLI and the TUI sees
  the same block headers (`┌─ Reasoning`, `$ command`, icon+name+param),
  badges (`(exit N)`), and footer (`└─ Thought for {:.1}s`), differing only in
  that the CLI never collapses and the TUI starts collapsed — the layouts
  are otherwise visually consistent.
- **SC-004**: 100% of the CLI's new structural elements (boxes, headers,
  badges, footers) use the existing `Theme::pantera()` palette and existing
  `theme::gradient_*` helpers — verified by the absence of any new color
  constants or theme fields introduced by this feature.
- **SC-005**: The full workspace test suite (`cargo test --workspace`)
  remains green, and no `AgentEvent` / `TranscriptItem` / public-surface
  change is introduced — the feature is a presentation-only change confined
  to `crates/joey-cli/src/render.rs`.

## Assumptions

- The primary reference for the *layout, header composition, badges, and
  footer wording* of the three block types is the joey-agent TUI as
  implemented under spec 007 (`crates/joey-tui/src/widgets.rs::item_lines()`
  and `specs/007-tui-crush-format-parity/contracts/block-layout.md`). This
  feature is the CLI↔TUI parity-in-reverse: it ports the TUI's crush layout
  to the CLI in an always-fully-expanded form. The three layouts are:
  §1 reasoning box (`┌─ Reasoning` border, full content, `└─ Thought for {:.1}s`
  footer); §2 terminal-command block (`$ command` header, `(exit N)` badge,
  full output body); §3 generic tool-call header (status icon + name +
  primary param, full indented result body).
- "Without the expandable sections / make everything fully expanded" means:
  remove all affordance text (`… N lines hidden`, `[click or space to
  expand]`, `… N more lines`), remove all collapsed/tail-window content
  slicing, remove the `▸`/`▾` expand-hint glyphs and the state labels
  (`reasoning (tail)` / `reasoning (full)`), and always render the full
  content of every block. It does NOT mean removing the bordered-box drawing
  or the headers — those ARE the "look like the TUI" part.
- All data the CLI layouts require (`full_result`, `exit_code`, `summary`)
  is already present on the existing `AgentEvent::ToolStart` / `ToolEnd`
  variants, as established by spec 007 (T027 added `exit_code`; T032 added
  `full_result`; `summary` has always been on `ToolStart`). This feature
  requires NO `AgentEvent` surface change and NO `TranscriptItem` change —
  it is confined to `crates/joey-cli/src/render.rs`. This respects
  constitution Principle VII (NON-NEGOTIABLE public-surface stability) and
  Principle II (the CLI and TUI consume the same event stream).
- The CLI is a scrolling, non-interactive stream (one-shot `joey -z` or the
  REPL's streaming renderer). Unlike the TUI it has no bounded viewport, so
  "fully expanded" naturally means "print all the lines" — there is no need
  for the TUI's bounding constants (`MAX_COLLAPSED_LINES`,
  `MAX_TAIL_WINDOW_LINES`, `MAX_TOOL_OUTPUT_LINES`). The CLI's existing
  `box_width()` clamp ([20, 80]) for the reasoning border is preserved.
- The reasoning `└─ Thought for {:.1}s` duration is derived in the CLI streaming
  state, the same approach the TUI uses (007 research §3): timestamp the
  first `ReasoningDelta` of a block and compute the elapsed when the block
  closes. The CLI already maintains transient reasoning state
  (`reasoning_open`, `reasoning_buf`, `reasoning_line_count`) inside
  `render_turn`; adding a `reasoning_started: Option<Instant>` is the same
  local-state pattern and requires no event change.
- The existing CLI animation machinery (thinking spinner on `ApiCallStart`,
  streaming caret between deltas, per-tool spinner on `ToolStart`, markdown
  reflow on `Done`) and the existing capability detection
  (`RenderCapability::{Full, Reduced, NonInteractive}`) remain intact. This
  feature changes only the *static text layout* of the reasoning/tool/terminal
  blocks, not the animation loop. When animations are off (`--quiet` /
  NonInteractive), the block layouts still render as plain (possibly
  unstyled) text — the layout is not animation-dependent.
- The `tool_progress` config gate (`off` / `new` / `all` / `verbose`) and
  the `--quiet` / `display.show_reasoning` gates are preserved. Where a
  block IS rendered, it is fully expanded; the gates only decide WHETHER a
  block renders, not how much of it shows. The `verbose`-only 120-char trim
  on `result_preview` is removed (FR-008) in favor of the full `full_result`
  body whenever a tool/terminal block is shown.
- The generic tool-call header's `emoji` field: the TUI composes the header
  as status-icon + emoji + bold-name + param. The CLI currently uses the
  `emoji` as the header icon (with `⚡` fallback) and applies a gradient to
  the name. To match the TUI's status-driven icon composition, the CLI
  adopts the `✓`/`✗` status icon as the primary header glyph; the `emoji`
  may be retained as a secondary glyph (as the TUI does) so long as the
  single-line composition and the existing `emoji` data are not lost.
- The `args:` section the TUI shows in its expanded tool view is OMITTED in
  the CLI: `full_args` is never populated (007 contracts/agent-event.md
  Approach A), and the TUI's expanded view falls back to `summary` for the
  param display — which is already shown in the CLI header. Printing a
  redundant `args:` line duplicating the header would add noise, so the CLI
  shows only the header + full result body. This is a documented,
  justified CLI-specific adaptation, not a parity gap.
- The crush layout applies to the CLI in ALL capability modes (Full /
  Reduced / NonInteractive), unlike 007's FR-016 which gated the crush
  layout to the interactive TUI only. This is the explicit intent of the
  user's request ("apply the design changes to the CLI"), and it is safe
  because the layout is static ANSI-styled text that degrades gracefully to
  unstyled text when color is unsupported. This is a deliberate,
  documented divergence from 007 FR-016, scoped to the CLI surface only
  (the TUI's FR-016 behavior is unchanged).
