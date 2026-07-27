# Data Model: Claude Code-Style CLI Animations

**Feature**: 002-claude-code-cli-style
**Date**: 2026-07-24
**Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md)

This defines the runtime data entities for the CLI animation system. All entities are ephemeral render-state living in `joey-cli`; none are persisted. Types are Rust structs/enums (Constitution Principle III: plain data, minimal public surface).

---

## Entity 1: `RenderCapability`

**What**: The detected terminal/CLI capability profile, used to select full, reduced, or disabled animation behavior.

**Fields**:
- `is_interactive: bool` — true when stdout is a TTY (`std::io::IsTerminal`). When false, animations are disabled entirely (FR-011).
- `supports_truecolor: bool` — true when `COLORTERM` env var is `truecolor` or `24bit`. When false, colors downsample to ANSI-16 via existing `theme::Rgb::ansi()`.
- `supports_unicode: bool` — true when the terminal likely renders box-drawing and arrow glyphs. Conservative default true; reduced to ASCII-safe glyphs when false.
- `term_width: usize` — columns (from `terminal_size`), used for banner scaling and layout.
- `target_fps: u32` — effective animation frame rate; default 12, lowered for reduced-capability.

**Variants (convenience accessor)**: collapses to a three-level `Capability { Full, Reduced, NonInteractive }` that the profile selector switches on:
- `NonInteractive` ⟺ `!is_interactive` → animations disabled, plain-text output only.
- `Reduced` ⟺ interactive but (no truecolor OR no unicode OR narrow width) → simplified frames + ASCII glyphs + ANSI-16.
- `Full` ⟺ interactive + truecolor + unicode + adequate width → full animations.

**Validation rules**:
- MUST be computed once at REPL startup (cheap, env-var + isatty probe) and passed into the renderer; not recomputed per frame.
- `NonInteractive` MUST short-circuit all animation paths before any cursor-control escape is emitted (FR-011, seam test #3).

**State transitions**: none — immutable after construction.

**Mapped to spec**: FR-007 (graceful degradation), FR-008 (fallback), FR-011 (non-TTY disable), SC-004 (fallback test coverage). Entity `RenderCapability` (spec Key Entities).

---

## Entity 2: `AnimationProfile`

**What**: A named set of parameters defining one animation, keyed by element kind. Pure data; looked up via a registry (Constitution Principle II).

**Fields**:
- `frames: Vec<String>` — the glyph sequence cycled per tick (e.g. spinner frames, or single-element frame for caret blink).
- `interval_ticks: u32` — how many ticks elapse before advancing to the next frame (1 = advance every tick; 2 = every other tick).
- `color: Rgb` — Pantera theme color applied to the animating glyph (from `joey_core::theme::charmtone`).
- `label: Option<String>` — static status label rendered alongside (e.g. "Thinking…" for the spinner; FR-002).
- `reduced: Option<Box<AnimationProfile>>` — the profile variant used under `Capability::Reduced` (ASCII-safe frames, slower interval). `None` means "omit this animation in reduced mode" (e.g. banner entrance collapses to static print).
- `disabled_fallback: String` — the plain-text rendering used under `Capability::NonInteractive` (e.g. spinner → empty, label printed once; streamed text printed raw).

**Kinds (registry keys)**: `Banner`, `ThinkingSpinner`, `StreamingCaret`, `ToolLine`, `PromptCaret`. Each maps to one `AnimationProfile` via a `const`/static registry lookup.

**Validation rules**:
- `frames` MUST be non-empty for any profile used under Full/Reduced capability.
- The registry lookup `profile(kind, capability)` MUST return a profile consistent with capability (Full profile for Full, reduced variant for Reduced, a sentinel/empty for NonInteractive).
- All colors MUST be sourced from the existing Pantera theme (FR-009); no hardcoded non-Pantera colors.

**State transitions**: none — immutable data.

**Mapped to spec**: FR-001/002/003/004/006 (per-element animation), FR-008 (reuse Pantera), FR-009. Entity `AnimationProfile` (spec Key Entities).

---

## Entity 3: `AnimationState`

**What**: The mutable, per-element runtime state of an active animation, advanced each tick by the central tick loop.

**Fields**:
- `kind: AnimationKind` — which profile governs this instance.
- `frame_idx: usize` — current index into `profile.frames`.
- `ticks_to_next_frame: u32` — countdown until `frame_idx` advances (reset to `profile.interval_ticks` after each advance).
- `running: bool` — whether this animation is currently active (e.g. spinner running during `ApiCallStart`→first `ContentDelta`; tool line running during `ToolStart`→`ToolEnd`).
- `started_at: Option<Instant>` — wall-clock start, for duration display (tool line, turn summary).
- `anchor_row: Option<u16>` — captured terminal row where the animating line began, for in-place cursor repaint (R-002/R-007). Cleared on finalize.

**State transitions** (the animation lifecycle):
```
Idle ──(element trigger event)──▶ Running(frame_idx=0)
Running ──(tick, countdown hits 0)──▶ Running(frame_idx=(idx+1)%frames.len())
Running ──(finalize event)──▶ Finalized(running=false, anchor cleared)
Running ──(NonInteractive short-circuit)──▶ Finalized (plain-text only)
```

**Validation rules**:
- `frame_idx` MUST always be a valid index into the active profile's `frames` (mod len) — a seam test asserts wraparound.
- Exactly one tick loop advances all live `AnimationState` instances (FR-010); no per-element timers.
- On finalize, any cursor-control state (hidden cursor, moved column) MUST be restored (cursor shown, newline emitted).

**Mapped to spec**: FR-010 (single interruptible tick loop), FR-007 (no flicker/partial frames). Entity `AnimationState` (spec Key Entities).

---

## Entity 4: `RenderOptions` (extended)

**What**: The existing `render::RenderOptions` struct (render.rs:110), extended with animation gating fields. Carried from REPL config into `render_turn`.

**Existing fields** (unchanged): `show_reasoning: bool`, `tool_progress: String`, `quiet: bool`.

**New fields**:
- `animations_enabled: bool` — master gate; false when `RenderCapability::NonInteractive` or user-disabled. Default true when interactive.
- `animation_fps: u32` — override for tick rate (default 12). Read from config `display.animation_fps` when present.
- `capability: RenderCapability` — the detected capability profile, resolved once at REPL startup.

**Validation rules**:
- When `animations_enabled` is false OR `capability` is `NonInteractive`, `render_turn` MUST take the plain-text code path (no cursor escapes, no spinner) regardless of other fields.
- Cloned per turn (already `#[derive(Clone)]`).

**Mapped to spec**: FR-011, FR-007, edge case "piped stdout". Ties the capability detection to the render path.

---

## Cross-entity relationships

```
RenderCapability ──resolves──▶ AnimationProfile (per kind)
       │                              │
       └─into RenderOptions──────────▶│
                                       ▼
                           AnimationState (per active element)
                                       │
                          advanced by ──▼── (single tick loop in render_turn)
```

- `RenderCapability` is computed once → stored in `RenderOptions.capability`.
- `RenderOptions` + `AnimationKind` → `profile(kind, capability)` selects the active `AnimationProfile`.
- `AnimationProfile` + per-element trigger events → instantiate/advance `AnimationState`.
- The central tick loop (in refactored `render_turn`) advances all live `AnimationState`s on each `interval` tick, interleaved with `AgentEvent` handling via `tokio::select!`.

---

## No persisted entities

All four entities are in-memory, per-session, per-turn. No schema, no migration, no storage. Session/token data continues to flow through `joey-agent-core::AgentEvent` and `joey-providers::Usage` unchanged (R-004).
