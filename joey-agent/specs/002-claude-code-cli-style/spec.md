# Feature Specification: Claude Code-Style CLI Animations

**Feature Branch**: `002-claude-code-cli-style`

**Created**: 2026-07-24

**Status**: Draft

**Input**: User description: "make the CLI of this agent similar to claude-code - use similar but not identical animations." Refined 2026-07-24: "Keep the current style of the TUI - only change the CLI and make it a hybrid between crush and claude code - claude code with crush colors."

## Clarifications

### Session 2026-07-24

- Q: Is the feature scope animation-only, or does it also include claude-code's persistent info elements (token/cost line, turn-complete summary)? → A: B — animation elements PLUS a claude-code-style token/cost line and a turn-complete summary line (tokens used, duration), in Crush/Pantera colors. A full context-window usage indicator (option C) is out of scope.
- Q: What does the thinking/processing indicator show — pure spinner, spinner+label, or spinner+live reasoning stream? → A: B — spinner + short static status label (e.g. "Thinking…"), no live reasoning text streaming.
- Q: How is streaming assistant text rendered — raw progressive reveal only, progressive reveal + markdown finalize on completion, or live markdown re-render per token? → A: B — progressive raw reveal token-by-token with caret, then on completion the block is re-rendered once as formatted markdown (headings, code blocks, lists, inline formatting) in Crush/Pantera colors (single controlled reflow).
- Q: What granularity for tool feedback — minimal/aggregated, per-tool animated lines with one-line summary, or per-tool lines with expandable detail? → A: B — each tool call gets its own entry line with entry animation + running spinner, resolving to a one-line done/failed summary with icon. No expandable detail block (CLI line-based).

## Scope Boundary (Clarification Resolution)

- **In scope**: the interactive line-based CLI REPL in the `joey-cli` crate (`src/repl.rs`, `src/render.rs`, and adjacent rendering/animation code) — the reedline-driven, ANSI-to-stdout session started by plain `joey`.
- **Out of scope (explicitly unchanged)**: the ratatui TUI in the `joey-tui` crate (the full-screen app launched via `--tui`). Its current style stays as-is. Feature `001-tui-crush-parity` continues to own that surface; the two features do not conflict because they live in different crates.
- **Visual identity decision**: claude-code provides the *animation and interaction model* (banner, thinking indicator, streaming reveal, tool feedback); the existing Crush-inspired Pantera color palette (already in `render.rs`) provides the *colors*. Animations are similar to claude-code but use Joey's own glyphs/timing (not identical copies of claude-code's frames), and no claude-code source or assets are used.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Recognizable claude-code-style startup banner (Priority: P1)

A developer familiar with the claude-code CLI starts Joey Agent's interactive CLI (`joey`) and sees a polished welcome banner with a distinctive entrance animation, rendered in the existing Crush/Pantera color palette. The motion/feel evokes claude-code's startup; the colors and branding are Joey's own (Crush-inspired palette, Joey logo/name).

**Why this priority**: The startup banner is the first thing every user sees and the most identifiable "feel" of claude-code. Without it the similarity is not felt on launch.

**Independent Test**: Launch the CLI cold and confirm a branded banner renders with an entrance animation in Crush/Pantera colors and resolves to a ready `❯` prompt within ~1 second.

**Acceptance Scenarios**:

1. **Given** the interactive CLI is started in a terminal at least 80x24, **When** the app launches, **Then** a Joey-branded banner appears with an entrance animation (distinct frame set from claude-code) in Crush/Pantera colors, followed by the ready prompt.
2. **Given** the CLI is launched in a terminal narrower than the banner width, **When** the banner renders, **Then** it scales or wraps gracefully rather than overflowing the line or breaking the layout.

---

### User Story 2 - Thinking/processing spinner while awaiting first token (Priority: P1)

While the agent processes a submitted message, the CLI shows a claude-code-style "thinking" indicator: an animated spinner or pulsing glyph paired with a concise status label, animating while the model works and resolving cleanly when the first token arrives. The glyph set and timing are Joey's own; the indicator is rendered in the Crush/Pantera palette.

**Why this priority**: The thinking animation is the most-watched dynamic element during a turn and is central to "feels like claude-code" during real use.

**Independent Test**: Submit a prompt that takes a few seconds to respond, and confirm the thinking indicator animates during processing (in Crush/Pantera colors) and transitions smoothly into streaming the reply.

**Acceptance Scenarios**:

1. **Given** a submitted prompt is awaiting the first response token, **When** the agent is processing, **Then** a spinner/pulsing glyph with a status label animates at a steady frame rate, using its own frames (not claude-code's literal frames) and Crush/Pantera colors.
2. **Given** the first response token arrives, **When** streaming begins, **Then** the thinking indicator clears or morphs into the streaming output without flicker or stray line artifacts on the line-based terminal.

---

### User Story 3 - Streaming assistant text reveal (Priority: P1)

As the assistant's reply streams token-by-token, the CLI reveals text progressively with a claude-code-like cadence and a subtle caret/cursor styling at the current position. The reveal is similar to claude-code's streaming feel but uses Joey's own caret style and wrapping behavior, rendered in Crush/Pantera colors.

**Why this priority**: Streaming text reveal is the primary content surface and the clearest "alive" animation during a response.

**Independent Test**: Send a prompt that produces a multi-paragraph response and confirm the text streams in progressively with a visible caret, then settles into final formatting when complete.

**Acceptance Scenarios**:

1. **Given** assistant tokens are streaming, **When** new tokens arrive, **Then** text appears progressively (raw, unformatted) with an animated caret/cursor at the current position, distinct from claude-code's exact caret, in Crush/Pantera colors.
2. **Given** streaming completes, **When** the message finalizes, **Then** the block is re-rendered exactly once as formatted markdown (headings, code blocks, lists, inline formatting) in Crush/Pantera colors, the caret is removed, and no further reflow occurs.

---

### User Story 4 - Tool-call progress with animated status transitions (Priority: P2)

When the agent invokes a tool, the CLI shows a claude-code-style tool activity indicator: an animated entry line for the tool call, a running state with a spinner/progress cue while the tool executes, and a resolved state (done/failed) with a distinct icon and a one-line summary. Transitions are similar to claude-code's tool feedback but use Joey's own glyph/color choices from the Crush/Pantera palette.

**Why this priority**: Tool feedback animation is highly visible during agentic work but is secondary to the core text/streaming feel.

**Independent Test**: Trigger a turn that calls one or more tools and confirm each tool line appears with an entry animation, animates while running, and resolves to a done/failed icon with a summary.

**Acceptance Scenarios**:

1. **Given** the agent dispatches a tool call, **When** the tool line appears, **Then** it enters with an animation (e.g. a brief reveal/slide) and shows a running spinner/progress cue.
2. **Given** a tool completes (success or failure), **When** the result is rendered, **Then** the line transitions to a resolved icon/color with a one-line summary, animated distinctly from claude-code's literal transitions.

---

### User Story 5 - Persistent token/cost line and turn-complete summary (Priority: P2)

During and after a turn, the CLI shows claude-code-style persistent status: a token/cost line (or equivalent usage indicator) that reflects in-flight usage while the agent works, and a turn-complete summary line after each response showing tokens used and turn duration, rendered in Crush/Pantera colors.

**Why this priority**: Persistent usage info is a signature part of claude-code's "feel" alongside the animations; it makes the similarity real and is sourced from token stats the agent core already emits, so it is low-cost to include.

**Independent Test**: Run a turn to completion and confirm a turn-complete summary line appears (tokens used + duration) in Crush/Pantera colors, and that usage updates are visible during the turn.

**Acceptance Scenarios**:

1. **Given** a turn is in progress, **When** tokens are consumed, **Then** a persistent usage indicator (token/cost) reflects current usage in Crush/Pantera colors.
2. **Given** a turn completes, **When** the response finalizes, **Then** a summary line appears showing tokens used and turn duration, in Crush/Pantera colors, and does not interfere with the streaming text above it.

---

### User Story 6 - Polished prompt with subtle idle animation (Priority: P3)

The prompt area (reedline `❯` prompt) has a claude-code-style feel: a clean input with a subtle idle caret blink and gentle focus indication, in Crush/Pantera colors. Multiline expansion remains smooth.

**Why this priority**: The prompt is always visible but its animation is subtle; it reinforces the overall feel at lower impact than the processing/streaming animations.

**Independent Test**: Focus the prompt and leave it idle, and confirm a blinking caret and a clean, non-flickering input; type a multiline message and confirm smooth expansion.

**Acceptance Scenarios**:

1. **Given** the prompt is focused and idle, **When** no input is being typed, **Then** the caret blinks at a steady interval without screen flicker.
2. **Given** the user types a multiline message, **When** the input grows, **Then** the editor expands smoothly and surrounding lines adjust without a visible jump.

---

### Edge Cases

- What happens when the terminal does not support the frame rate or Unicode glyphs the animations assume? The animations MUST degrade to a reduced-capability mode (slower/simpler frames, ASCII-safe glyphs) rather than producing artifacts or garbled output.
- What happens on very slow connections where the first token takes many seconds? The thinking/processing animation MUST keep animating smoothly without freezing and MUST NOT spawn multiple overlapping indicators.
- What happens when many tool calls run in quick succession? Each tool line's entry/exit animation MUST remain coherent (no overlapping/corrupted frames or line tearing) even when lines appear and resolve rapidly.
- What happens when the CLI output is piped (non-interactive stdout) or animation is explicitly disabled? The dynamic content (banner text, thinking status, streaming text, tool summaries, and the turn-complete usage summary) MUST remain fully readable as plain text with animations disabled — no frames, no carriage-return tricks.
- What happens when the terminal is resized during an animation? The animation MUST adapt to the new width without leaving partial frames or forcing a redraw flash of unrelated content.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The CLI MUST display a Joey-branded startup banner with an entrance animation on launch that is visually distinct from claude-code's banner while evoking a similar polished startup feel, rendered in the existing Crush/Pantera color palette.
- **FR-002**: The CLI MUST show an animated "thinking/processing" indicator — a spinner or pulsing glyph paired with a short static status label (e.g. "Thinking…") — while awaiting the first response token, using its own glyph set and frame timing (not claude-code's exact frames), in Crush/Pantera colors. The indicator MUST NOT stream live reasoning/thinking text; the label is static until streaming response begins.
- **FR-003**: The CLI MUST reveal streaming assistant text in two phases: (a) progressive raw reveal — text streams token-by-token with an animated caret/cursor at the current position, distinct from claude-code's exact caret; then (b) on completion, a single controlled reflow that re-renders the whole block as formatted markdown (headings, code blocks, lists, inline formatting) in Crush/Pantera colors. The finalize reflow MUST NOT repeat on subsequent tokens and MUST NOT cause visible flicker beyond the one expected format reflow.
- **FR-004**: The CLI MUST render each tool call as its own line with entry/running/resolved animations: an animated entry line, a running spinner/progress cue while executing, and a resolved done/failed icon with a one-line summary — using its own animation style (not claude-code's literal transitions) and Crush/Pantera colors. Tool feedback MUST be per-tool (not aggregated); there MUST be no expandable/collapsible detail block (the CLI is line-based).
- **FR-005**: The CLI MUST show a persistent usage indicator (token/cost line or equivalent) that reflects in-flight usage during a turn, and a turn-complete summary line (tokens used, turn duration) after each response, rendered in Crush/Pantera colors. The summary MUST NOT overwrite or interfere with the streamed response text.
- **FR-006**: The CLI MUST provide a polished prompt area with an idle caret-blink and smooth multiline expansion, evoking claude-code's prompt feel without copying its exact styling, in Crush/Pantera colors.
- **FR-007**: All animations MUST run at a configurable, steady frame rate (with a sensible default) and MUST NOT cause screen flicker, partial-frame artifacts, carriage-return tearing, or layout shift unrelated to the animating element on the line-based terminal.
- **FR-008**: All animations MUST degrade gracefully on terminals lacking the assumed capabilities (high frame rate, Unicode glyphs, truecolor): fall back to simpler/slower frames and ASCII-safe glyphs, validated by automated tests.
- **FR-009**: The CLI MUST reuse the existing Crush-inspired Pantera theme (already in `joey-cli/src/render.rs`) for all animated elements and persistent info lines; this feature MUST NOT introduce a competing color palette, and MUST NOT alter the `joey-tui` crate's theme or style.
- **FR-010**: The animation frame scheduling MUST be driven by a single, interruptible timer/tick loop within the CLI REPL so that resize, capability changes, or rapid state transitions do not produce overlapping or corrupted frames.
- **FR-011**: The animations and persistent info MUST be automatically disabled when stdout is detected as non-interactive (piped/non-TTY) or when animation is explicitly disabled by the user, falling back to plain-text rendering of the same content (banner text, status label, streamed text, tool summaries, and the turn-complete usage summary).

### Key Entities *(include if feature involves data)*

- **AnimationProfile**: A named set of parameters defining an animation — glyph frames, interval/timing, color/style (drawn from the Pantera theme), and a reduced-capability fallback — applied to a specific CLI element (banner, thinking indicator, streaming caret, tool line, prompt caret).
- **AnimationState**: The per-element runtime state of an animation — current frame index, elapsed time, running/idle flag, last tick timestamp — advanced by the central tick loop.
- **RenderCapability**: The detected terminal/CLI capability profile (is a TTY, supports truecolor, supports Unicode glyphs, target frame rate) used to select an AnimationProfile's full, reduced-capability, or disabled (plain-text) variant.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer familiar with claude-code who launches Joey Agent's CLI for the first time recognizes the claude-code-style feel (startup banner, thinking indicator, streaming reveal) within the first 30 seconds of use.
- **SC-002**: Side-by-side observation of equivalent interactions (cold launch, a multi-second think, a streaming multi-paragraph reply, a tool call) shows animation concepts and cadence similar to claude-code while using visibly distinct glyphs/frames and the existing Crush/Pantera colors.
- **SC-003**: All animations run flicker-free at the configured target frame rate during a representative 2-minute interactive session across at least 3 common terminal emulators, with no partial-frame artifacts or line tearing on resize.
- **SC-004**: The reduced-capability fallback (non-TTY / no truecolor / no Unicode / limited frame rate) is verified by an automated test suite covering profile-selection and frame-substitution logic, with 100% of defined fallback cases passing.
- **SC-005**: The `joey-tui` crate (the `--tui` full-screen app) is unchanged by this feature: its existing style, theme, and behavior remain identical before and after.

## Assumptions

- "Similar to claude-code but not identical" means matching the *concepts and feel* of claude-code's CLI animations (polished startup banner, thinking/processing spinner, progressive streaming text reveal, animated tool-call feedback) while using Joey Agent's own glyphs, timing, and branding — not copying claude-code's literal frames or proprietary assets.
- "Claude code with crush colors" means the animation/interaction model comes from claude-code and the color palette is the existing Crush-inspired Pantera theme already present in `joey-cli/src/render.rs`; no new palette is introduced.
- "Keep the current style of the TUI" means the `joey-tui` crate (ratatui full-screen app via `--tui`) is explicitly out of scope and must remain unchanged; this feature touches only the line-based CLI REPL in `joey-cli`.
- This feature is independent of `001-tui-crush-parity` because the two live in different crates (`joey-cli` vs `joey-tui`); no reconciliation or sequencing between them is required.
- The reference for "claude-code-style" is the externally observable behavior of Anthropic's `claude-code` CLI; Joey Agent's implementation uses its existing Rust CLI stack (reedline + ANSI/`nu-ansi-term` + crossterm in `joey-cli`), not claude-code's code.
- A reasonable default frame rate (e.g. a ~16-20 fps tick loop with per-element intervals) will be chosen and made configurable; exact frame timing is an implementation detail.
- Terminal/CLI capability detection for animation fallback reuses the existing TTY/truecolor detection already present in the CLI rather than introducing a new subsystem.
