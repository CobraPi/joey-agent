# Feature Specification: TUI Crush Parity

**Feature Branch**: `001-tui-crush-parity`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "please create a plan to fully update the TUI of this project to make it identical to the crush coding agent"

## Clarifications

### Session 2026-07-23

- Q: Should the sidebar match Crush's full multi-section content (files/LSP/MCP/skills), or just the subset Joey Agent's agent core already has data for? → A: Full Crush parity: sidebar shows all sections (files, LSP, MCP, skills), building any missing backing state (file-change tracking, LSP status surface) as part of this feature.
- Q: How rigorously must degraded-mode (non-truecolor/non-Unicode) rendering be validated for this feature to be considered done? → A: Automated tests for color/glyph downsampling logic only; no manual multi-emulator test pass required.
- Q: Which additional Crush dialogs (beyond model selection, session selection, permission requests, yes/no confirmations, and command/slash completions) must be built for parity? → A: Add dialogs backed by existing Joey Agent functionality — quit confirmation, reasoning-level picker, notifications, and the full question-form family (single/multi/freetext/confirm/editor variants) — beyond the 5 already listed; skip OAuth-in-TUI (Joey's auth is CLI-driven via `joey auth copilot login`) and the file picker (no LSP/file-browse feature exists to back it).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Familiar chat workspace on first launch (Priority: P1)

A developer who already uses Crush opens Joey Agent's terminal UI and sees the same overall workspace shape: a header/status bar, a scrollable message transcript rendered with rich formatting (markdown, code blocks, diffs), a sidebar showing session/session-list context, and a multi-line prompt editor at the bottom with the same keybindings they already know.

**Why this priority**: This is the core visual/interaction surface users spend all their time in. Without this, nothing else in the parity effort matters.

**Independent Test**: Launch the TUI, send a message, and confirm the layout regions (header, transcript, sidebar, editor) render and behave like Crush's `internal/ui/model` layout, independent of any specific dialog or theme feature.

**Acceptance Scenarios**:

1. **Given** the TUI is launched in a terminal at least 80x24, **When** the app starts, **Then** the screen shows a header/status area, a scrollable transcript area, and a bottom prompt editor matching Crush's layout regions.
2. **Given** a running session, **When** the terminal is resized below the compact-mode breakpoints (120 columns / 30 rows), **Then** the layout collapses to a compact single-column mode the same way Crush does.

---

### User Story 2 - Rich transcript rendering matching Crush (Priority: P1)

A user reads assistant responses, tool calls, and tool results in the transcript and sees the same presentation Crush uses: streaming markdown rendering, distinct styling per role (user/assistant/tool/notice/error), collapsible/expandable tool call blocks with status icons (running/done/failed), and diff/code rendering with syntax highlighting.

**Why this priority**: Transcript rendering is the single biggest visible behavior difference between a "similar" TUI and an "identical" one; it's also where most of Crush's UI complexity lives (chat/*.go).

**Independent Test**: Replay a fixed conversation transcript (user message, assistant streaming text, a tool call with a diff result, an error notice) through both Joey Agent's TUI and Crush, and visually/structurally compare the rendered blocks for equivalent styling, icons, and status colors.

**Acceptance Scenarios**:

1. **Given** an assistant message streams token-by-token, **When** it is rendering, **Then** the transcript shows an in-progress state (spinner/reasoning indicator) and finalizes into styled markdown once complete, matching Crush's streaming/finalize behavior.
2. **Given** a tool call transitions from Running to Done or Failed, **When** the transcript is redrawn, **Then** the tool block shows the corresponding status icon/color and a result preview/summary, matching Crush's tool block states.
3. **Given** a tool result contains a unified diff, **When** it is rendered, **Then** the diff is displayed with additions/deletions highlighted the same way as Crush's diff view.

---

### User Story 3 - Matching color theme and visual identity (Priority: P2)

A user familiar with Crush's default color theme (the aurora-synthwave palette: electric cyan, hot orchid-pink, acid lime, jewel-toned high-contrast dark background) sees Joey Agent's TUI using the same semantic color roles (primary, secondary, accent, success, warning, error, info) applied consistently across widgets.

**Why this priority**: Visual identity (colors, gradients, logo styling) is highly noticeable and central to "looks identical," but is lower risk/complexity than transcript/interaction parity, so it's sequenced after the higher-value structural work.

**Independent Test**: Render the same static screen (e.g. a startup/landing view) in Joey Agent and Crush side by side and confirm the background, foreground, and semantic accent colors match within reasonable tolerance.

**Acceptance Scenarios**:

1. **Given** the default theme is active, **When** any screen is rendered, **Then** background/foreground/accent/status colors resolve to the same semantic role values used by Crush's theme/palette.
2. **Given** a gradient-styled element (e.g. logo or startup banner), **When** it renders, **Then** it uses the same multi-stop gradient interpolation style as Crush.

---

### User Story 4 - Dialogs, overlays, and command palette parity (Priority: P2)

A user invokes overlay dialogs (model picker, session picker, permission prompts, command palette / slash-command completions, quit confirmation) and interacts with them using the same navigation and keybindings as Crush.

**Why this priority**: These are frequently used secondary surfaces; without them the "identical" claim fails on real day-to-day interaction, but the app remains usable via the primary chat flow without them.

**Independent Test**: Trigger each dialog type (model list, session list, command completions, a yes/no confirmation, a permission request) and confirm it opens as an overlay, supports keyboard navigation (up/down/enter/esc), and closes correctly, matching Crush's dialog behavior.

**Acceptance Scenarios**:

1. **Given** the user opens the model picker, **When** they navigate the list and press enter, **Then** the selected model is applied and the overlay closes, mirroring Crush's models dialog flow.
2. **Given** a tool requires permission, **When** the permission dialog appears, **Then** the user can approve/deny via the same keybindings Crush uses, and the transcript reflects the outcome.

---

### Edge Cases

- What happens when the terminal is smaller than Crush's minimum supported size (e.g. under 80 columns or under 10 rows)? The UI must degrade gracefully (e.g. further compact layout or a size-warning message) rather than panicking or rendering garbled output.
- How does the system handle extremely long tool output or transcript history that exceeds available memory/scrollback? Older transcript items must be evicted/paged the same way Crush bounds its history, without losing correctness of the currently visible view.
- What happens when a terminal does not support truecolor or Unicode glyphs? The UI must fall back to a reduced-capability rendering (ANSI-16 colors, ASCII-safe glyphs) rather than showing corrupted output, mirroring Crush's capability-detection fallback. Validation of this fallback is via automated unit tests of the color-downsampling and glyph-substitution logic; no manual pass across real terminal emulators is required for this feature to be considered done.
- What happens in the sidebar when a backing data source (file changes, LSP status, MCP status, skills) has nothing to report yet — e.g. no LSP client attached, or no files changed in the session? That section MUST render its Crush-equivalent empty/zero state (e.g. a "0" count or an empty-list message) rather than being omitted, so the sidebar's section layout stays stable.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The TUI MUST present a layout with the same structural regions as Crush's main UI model: header/status bar, message transcript, optional sidebar, and bottom prompt/editor.
- **FR-002**: The TUI MUST collapse to a compact single-column layout when the terminal falls below Crush's compact-mode breakpoints (120 columns wide or 30 rows tall), matching Crush's responsive behavior.
- **FR-003**: The transcript MUST render each item type (user message, assistant message, tool call, notice, error) with visually distinct styling equivalent to Crush's per-role rendering.
- **FR-004**: The transcript MUST render assistant text as streaming markdown that finalizes into fully-styled markdown (headings, lists, code blocks with syntax highlighting, inline formatting) once the message completes.
- **FR-005**: Tool call blocks MUST show a status indicator (running/done/failed) with distinct icon and color per state, plus a collapsed summary and an expandable detail/result view, matching Crush's tool-call presentation.
- **FR-006**: Diff-producing tool results MUST render as a unified diff view with additions and deletions visually distinguished, matching Crush's diff rendering.
- **FR-007**: The color theme MUST define the same semantic roles (background, foreground, primary/accent, secondary, success, warning, error, info, muted/subtle variants) and apply them consistently across all widgets, using Crush's default aurora-synthwave-style palette as the reference values.
- **FR-008**: Gradient-styled elements (e.g. startup banner/logo) MUST use the same multi-stop gradient interpolation approach (perceptual/luminance-aware blending) as Crush's gradient implementation.
- **FR-009**: The TUI MUST provide overlay dialogs equivalent to Crush's for: model selection, session selection, permission requests, yes/no confirmations, command/slash completions, quit confirmation, reasoning-level selection, in-app notifications, and the full question-form family (single-select, multi-select, free-text, confirm, and inline-editor prompts) — each supporting keyboard-only navigation (arrow keys, enter, escape). In-TUI OAuth dialogs and a file picker are explicitly out of scope: Joey Agent's authentication is CLI-driven (`joey auth copilot login`) and no LSP/file-browse feature exists yet to back a file picker.
- **FR-009a**: The TUI sidebar MUST render the same sections as Crush's sidebar — session title/cwd, model+reasoning+token/cost info, changed-files summary, LSP status, MCP status, and skills status — building whatever backing state (file-change tracking, LSP status surface) Joey Agent does not yet expose. Any section with no data to report MUST render its Crush-equivalent empty/zero state rather than being omitted.
- **FR-010**: The prompt/editor area MUST support multi-line input, the same core editing keybindings as Crush's textarea (e.g. submit on Enter, newline on a modified Enter, history navigation), and must reflect current agent state (busy/idle) the same way Crush does.
- **FR-011**: The TUI MUST detect terminal capability (truecolor vs. ANSI-16, Unicode support) and gracefully degrade rendering rather than fail, matching Crush's capability-detection behavior. This requirement is verified via automated unit tests covering the color-downsampling and glyph-substitution logic; manual verification across real terminal emulators is not required for this feature.
- **FR-012**: The TUI's session/turn/tool state model MUST expose enough information (phase, active agents, per-tool status, token stats, changed-file list, LSP client status, MCP server status, skills status) to drive all the above renderings, extending the existing state model where Crush-equivalent data is not yet tracked.

### Key Entities *(include if feature involves data)*

- **TranscriptItem**: A single rendered unit in the conversation view — user text, assistant text, tool call, notice, or error — carrying the state needed to style it (status, timestamps, summaries).
- **Theme/Palette**: The set of semantic color roles (background, foreground, accent, status colors) and gradient definitions used across all widgets.
- **Dialog/Overlay**: A modal-like UI surface (model picker, session picker, permission prompt, confirmation, completions, quit confirmation, reasoning picker, notification, question-form variants) with its own focus and keyboard-navigation state.
- **AppState**: The overall TUI state machine (current phase, active agents, scroll position, streaming buffers, token stats) that drives what is rendered each frame.
- **SidebarSection**: A named block of sidebar content (session info, model info, changed files, LSP status, MCP status, skills status) with a count/summary and an empty/zero-state rendering when its backing data source has nothing to report.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A user who is already familiar with Crush can operate Joey Agent's TUI (send a message, inspect a tool call, open the model picker) without consulting documentation, on their first attempt.
- **SC-002**: Side-by-side screenshots of equivalent screens (startup, mid-conversation with a tool call, an open dialog) in Joey Agent and Crush show matching layout regions and matching semantic colors for at least 95% of visually distinct elements.
- **SC-003**: All primary interaction flows (send message, view streaming response, expand a tool result, open and act on each dialog type, resize the terminal across the compact-mode breakpoint) complete without visual corruption or crashes across at least 3 common terminal emulators.
- **SC-004**: Terminal capability fallback (no truecolor / no Unicode) is verified by an automated test suite covering color-downsampling and glyph-substitution logic, with 100% of defined fallback cases passing; no manual per-terminal-emulator test pass is required.

## Assumptions

- "Identical to the crush coding agent" refers to matching the terminal UI's structure, interaction model, and default visual theme as implemented in the `crush` reference repository at `/Users/jo110366/Development/crush` (Go, Bubble Tea/Lip Gloss/Ultraviolet stack), not literal code reuse — Joey Agent's TUI is implemented in Rust (ratatui/crossterm).
- Exact pixel-for-pixel terminal rendering across different fonts/terminal emulators is not achievable or required; "identical" is scoped to layout structure, interaction behavior, semantic color roles, and default palette values.
- Non-chat/non-TUI surfaces (e.g. CLI subcommands, cron scheduler, MCP client behavior) are out of scope for this feature; only the interactive terminal UI (`crates/joey-tui`) is in scope.
- Crush features that depend on capabilities Joey Agent's agent core does not yet support (e.g. image attachments, Docker MCP) are out of scope for visual/interaction parity until those backing features exist; this spec covers the TUI shell and rendering for functionality Joey Agent already has, plus the sidebar's LSP-status and changed-files sections and their minimal backing state (added per Clarifications).
- In-TUI OAuth dialogs (Copilot/Hyper login flows) and a file picker dialog are explicitly out of scope: Joey Agent's authentication is CLI-driven (`joey auth copilot login`) and no LSP/file-browse feature exists yet to justify a file picker.
- The existing Joey Agent theme module (`crates/joey-tui/src/theme.rs`) will be updated to match Crush's palette values and semantic roles rather than replaced with a different color system.
