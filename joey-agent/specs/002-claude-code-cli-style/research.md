# Phase 0 Research: Claude Code-Style CLI Animations

**Feature**: 002-claude-code-cli-style
**Date**: 2026-07-24
**Spec**: [spec.md](spec.md)

This document resolves all technical unknowns needed for Phase 1 design. Every decision is grounded in evidence read from the current codebase (paths cited) and evaluated against alternatives.

---

## R-001: Animation tick-loop architecture for `render_turn`

**Decision**: Convert `render_turn` from a blocking `while let Some(ev) = rx.recv().await` loop into a `tokio::select!` loop that multiplexes agent events with a fixed-interval animation tick, using crossterm cursor-control escapes (`cursor::MoveToColumn`, `terminal::Clear`, `cursor::Hide`/`Show`) to repaint in-place animating elements (thinking spinner, streaming caret, running tool line).

**Rationale**:
- The current `render_turn` (crates/joey-cli/src/render.rs:136) is a single `recv().await` loop with `print!`/`println!` only — it has no timer and cannot repaint a spinner frame while waiting for the next event. A spinner that only advances on event arrival would freeze during multi-second model queries (the exact scenario FR-002 and edge-case "very slow connections" target).
- `tokio::select!` over `rx.recv()` vs `tokio::time::interval` is the idiomatic async pattern already used in this codebase: `run_turn_interactive` (repl.rs:609) already uses `tokio::select!` over the turn future and `ctrl_c()`. Adding a timer branch is the same mechanism.
- crossterm 0.28 is already a `joey-cli` workspace dependency (crates/joey-cli/Cargo.toml) and provides `cursor::MoveToColumn(0)`, `terminal::Clear(ClearType::CurrentLine)`, `cursor::Hide`/`Show`. These are the minimal primitives needed for in-place repaint without a full alternate-screen TUI.
- The spec's FR-010 ("single, interruptible timer/tick loop") and FR-007 ("steady frame rate") mandate exactly one timer driving all animating elements — a single `interval` arm in the select loop satisfies this.

**Alternatives considered**:
- *indicatif crate*: provides ready-made spinners/progress bars. Rejected: it owns its own draw thread and rendering, which would conflict with the line-based `print!` streaming model and the requirement (FR-009) to reuse the existing Pantera theme colors. Adds a dependency for behavior that is a small select-loop arm.
- *ratatui alternate-screen*: already used by `joey-tui`. Rejected: this feature is explicitly the line-based CLI (spec scope boundary), and ratatui takes over the whole screen, breaking pipe/scrollback behavior (FR-011, edge case "piped stdout").
- *Separate OS thread for animation*: rejected — introduces cross-thread synchronization for what is cleanly expressible as one async select arm, and would fight the single-threaded tokio runtime's stdout writes.

**Frame rate default**: 12 fps tick interval (~83ms) for spinner frames; the streaming caret blinks on the same tick. Configurable via `display.animation_fps` (Phase 2 concern; default chosen here). This is well under claude-code's cadence and avoids CPU overhead while remaining visibly smooth.

---

## R-002: Two-phase streaming reveal (raw stream → single markdown finalize)

**Decision**: During `ContentDelta`, continue printing raw text progressively with an appended caret glyph (styled in Pantera `accent`), and do NOT reflow per token. On `AssistantMessage` (the complete-message event) OR `Done { final_text, .. }`, perform exactly one controlled reflow: clear the streamed region (move cursor up to the first streamed line, clear lines down), then re-render the full text through a markdown renderer in Pantera colors.

**Rationale**:
- Clarification Q3 (spec Clarifications, Session 2026-07-24) chose "progressive raw reveal + markdown finalize on completion (single controlled reflow)". This decision implements it.
- `AgentEvent::AssistantMessage(text)` (events.rs:59) fires with the complete assistant message; `AgentEvent::Done { final_text, usage, iterations }` (events.rs:153) is the turn-ender carrying the final text. Either can trigger the finalize. The current code (render.rs:233) already treats `AssistantMessage` as the finalize signal but only `println!`s it if nothing streamed. The new behavior: track the number of lines streamed since the first `ContentDelta`, then on finalize move the cursor back up and clear them, then print the rendered markdown.
- crossterm `cursor::MoveUp(n)` + `terminal::Clear(ClearType::CurrentLine)` repeated n times, or `cursor::MoveTo(x,y)` if the start position is captured, handles the clear. Capturing the cursor row at first delta is most robust against intervening tool/reasoning lines.
- Markdown rendering requires a markdown→ANSI library (see R-003).

**Alternatives considered**:
- *Live markdown re-render per token (option C)*: rejected by clarification Q3 (reflow on every token = flicker/CPU risk).
- *No markdown finalize, raw text only (option A)*: rejected by clarification Q3 (not polished enough for claude-code feel).
- *Capture streamed text in a buffer and diff/patch on finalize*: over-engineering; a full clear+re-render of one message block is cheap and avoids diff bugs. YAGNI (Constitution Principle V).

---

## R-003: Markdown rendering library for finalize step

**Decision**: Add `pulldown-cmark = "0.12"` (already a workspace dependency, used by `joey-speckit-ui`) to `joey-cli`'s dependencies, and write a small Pantera-colored markdown→ANSI renderer module (`render::markdown`) that walks the pulldown-cmark `Event` stream and emits styled ANSI spans for headings, bold/italic, inline code, fenced code blocks, bullet/ordered lists, blockquotes, and horizontal rules.

**Rationale**:
- pulldown-cmark 0.12 is already in the workspace `Cargo.toml` and used by `joey-speckit-ui` (crates/joey-speckit-ui/Cargo.toml), so no new external dependency is introduced — it is promoted from workspace-level to a `joey-cli` workspace dep.
- It is a pull-based parser (events), which maps cleanly to a stateful ANSI emitter without owning the terminal.
- The Pantera theme (joey-core::theme::Theme::pantera()) already exposes semantic colors and `gradient_fg`; the renderer maps heading levels to gradient colors, code to `accent`, etc. This satisfies FR-009 (reuse Pantera, no new palette).
- The renderer is a pure function `markdown_to_ansi(&str, &Theme) -> String`, trivially unit-testable (Constitution Principle IV: test the seam).

**Alternatives considered**:
- *comrak*: heavier (HTML/render pipeline), more than needed for terminal ANSI. pulldown-cmark's event stream is sufficient.
- *syntect (syntax highlighting for code blocks)*: adds a large dependency (grammar loading) for marginal gain in a line-based CLI. Out of scope; code blocks get a single accent color and a language label. Noted as a future enhancement, not this feature.
- *bat / prettydiff*: reject for the same reason — too much machinery for inline ANSI markdown.
- *Hand-rolled regex markdown*: fragile (nested emphasis, code spans). pulldown-cmark is already available.

---

## R-004: Token/cost persistent line and turn-complete summary wiring

**Decision**: No changes to `joey-agent-core` are required. The existing `AgentEvent` stream already carries all needed data:
- `ApiCallEnd { usage: Usage }` (events.rs:36) fires after each model call with prompt/completion token counts → drives the in-flight usage indicator (FR-005).
- `Done { final_text, usage, iterations }` (events.rs:153) fires at turn end with cumulative usage → drives the turn-complete summary (FR-005).

The current `render_turn` already accumulates `total_prompt_tokens`/`total_completion_tokens` (render.rs:142-143) from `ApiCallEnd` and prints a turn summary on `Done` (render.rs:365-376). The enhancement is: (a) show a persistent in-flight usage line during the turn (updated via the tick loop), and (b) restyle the existing turn summary to a claude-code-style line with duration (sourced from `session_start` already on `ReplState`).

**Rationale**:
- The data path already exists end-to-end. Adding fields to `AgentEvent` would violate Constitution Principle III (minimal public surface) and I (touching a crate whose responsibility is the turn loop, not rendering) for no functional gain.
- `Usage` (from joey-providers) is a plain data struct crossing the crate boundary, exactly as Principle III prescribes.

**Cost computation**: per-token cost requires a price table per model. This is out of scope for the in-flight indicator (which shows token counts), but the turn-complete summary MAY show an estimated cost if a price table is available in config. Default: show token counts + duration; cost is a Phase 2 enhancement if a price table is added. This keeps the feature self-contained.

---

## R-005: Terminal capability detection and animation fallback

**Decision**: Reuse the existing `std::io::IsTerminal` idiom (already used at repl.rs:450, commands.rs:51, tui.rs:29) for the TTY/non-TTY gate. For truecolor/Unicode detection, add a small `render::capability::RenderCapability` that probes `std::env::var("COLORTERM")` (truecolor) and crossterm's terminal size, selecting an `AnimationProfile` variant.

**Rationale**:
- `IsTerminal` is the established pattern in this crate — no new detection subsystem (Constitution Principle V: reuse, don't add).
- `COLORTERM=truecolor` (or `24bit`) is the de facto env-var signal for truecolor support; absence downsampled to ANSI-16 via the existing `theme::Rgb::ansi()` which already produces ANSI-16 fallbacks (theme.rs:45).
- Non-TTY (piped) stdout disables animations entirely and falls back to plain-text printing of the same content (FR-011, edge case "piped stdout"). This is a single `if !stdout.is_terminal() { return plain; }` gate at the top of the animation path.

**Reduced-capability profiles**: each `AnimationProfile` carries a `reduced` variant — spinner becomes a static `*`/`-` prefix, glyph frames collapse to ASCII (`|/-\`), caret becomes `_`. Validated by unit tests (FR-008, SC-004).

**Alternatives considered**:
- *arseedline/terminal detection crate*: over-dependency for `var("COLORTERM")` + `IsTerminal`.
- *Manual terminfo parsing*: far beyond what's needed; crossterm already abstracts this.

---

## R-006: Startup banner entrance animation

**Decision**: Add a `render::banner_animated(&BannerInfo, &AnimationProfile)` that wraps the existing `render::banner` content with a short entrance sequence: a left-to-right gradient "wipe-in" of the logo line (printing successive prefixes with a small inter-frame sleep using the tick timer), then fade-in of the info lines. The animation runs once at REPL startup before the first prompt.

**Rationale**:
- The existing `render::banner` (render.rs:518) already composes the full static banner with gradient logo and diagonal fields. The animation layers a timed reveal over the same content — no redesign of the banner itself, only a presentation wrapper.
- A single-shot entrance animation at startup is the claude-code signature (FR-001) and the highest-impact "feel" element.
- Implemented via the same tick-loop/timer mechanism as the spinner (R-001), reused for a bounded ~600-900ms sequence, then handed off to the static banner.

**Alternatives considered**:
- *ASCII-art morphing logo*: visually heavy, font-dependent, and risks the narrow-terminal edge case. A gradient wipe-in is terminal-safe and degrades to "print banner" in reduced-capability mode.
- *No animation, just the static banner*: rejected by FR-001 (entrance animation required).

---

## R-007: Per-tool animated lines (entry/running/resolved)

**Decision**: On `ToolStart`, print an entry line with a brief reveal (the tool name/emoji fades or slides in via a 2-3 frame gradient build over the tick loop), then keep the line "live" with a running spinner glyph that advances each tick. On `ToolEnd`, rewrite the same line (cursor-up + clear-line) to the resolved state with done/failed icon + one-line summary + duration. No expandable detail (clarification Q4).

**Rationale**:
- Clarification Q4 chose "per-tool animated lines with one-line summary, no expandable detail". This implements it directly.
- The current `ToolStart`/`ToolEnd` handling (render.rs:240-300) prints two separate lines (entry on start, result on end). The enhancement: make them the SAME logical line, updated in place via cursor control, so the user sees one tool block transition from running→resolved, matching claude-code.
- Per-tool granularity (not aggregated) is required by FR-004. The existing code already renders per-tool (one `ToolStart`/`ToolEnd` pair per call), so this is an in-place-update enhancement, not a structural change.
- Tracking "which row was this tool printed on" uses the cursor-capture approach from R-002.

**Alternatives considered**:
- *Collapsible detail block (option C)*: rejected by clarification Q4 (CLI line-based, no expand/collapse interaction).
- *Keep current two-line approach*: rejected by FR-004 (single line transitioning states is the claude-code signature).

---

## Summary of codebase facts grounding this plan

| Fact | Location | Used for |
|---|---|---|
| `render_turn(rx, opts)` is a recv loop, no timer | render.rs:136 | R-001 (add select+timer) |
| `run_turn_interactive` spawns render task + ctrl_c select | repl.rs:600-620 | R-001 (same async select idiom) |
| `AgentEvent::{ContentDelta, AssistantMessage, Done, ToolStart/End, ApiCallEnd{usage}}` | events.rs:18-161 | R-002, R-004, R-007 (no core changes) |
| `total_prompt_tokens`/`completion_tokens` accumulated in render | render.rs:142-143 | R-004 (data already present) |
| Turn summary already printed on `Done` | render.rs:365-376 | R-004 (enhance, not build new) |
| `render::banner(&BannerInfo)` static banner | render.rs:518 | R-006 (wrap with animation) |
| `Theme::pantera()`, `gradient_fg`, `gradient_diagonal_field`, `charmtone` | theme.rs | All (reuse, FR-009) |
| crossterm 0.28 workspace dep | joey-cli/Cargo.toml | R-001 (cursor control) |
| pulldown-cmark 0.12 workspace dep (in speckit-ui) | workspace Cargo.toml | R-003 (promote to joey-cli) |
| `IsTerminal` idiom | repl.rs:450, tui.rs:29 | R-005 (reuse for TTY gate) |
| `Rgb::ansi()` produces ANSI-16 fallback | theme.rs:45 | R-005 (color downsampling) |
| `joey-tui` crate untouched | — | SC-005 (verified: no edits) |
