# Contract: render / animation seam

**Feature**: 004-claude-code-cli-style
**Date**: 2026-07-24
**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md)

This documents the interface contracts the `joey-cli` animation system exposes. These are intra-crate seams (Constitution Principle IV: test the seam). Each contract lists signature, caller(s), behavior, and the seam test that guards it.

---

## Contract 1: `render::render_turn` (refactored)

**Signature** (unchanged externally):
```rust
pub async fn render_turn(
    rx: mpsc::UnboundedReceiver<AgentEvent>,
    opts: RenderOptions,
) -> String
```

**Callers**: `repl.rs::run_turn_interactive` (repl.rs:602 — `tokio::spawn(render::render_turn(rx, st.ropts.clone()))`).

**Behavior contract**:
- Consumes the `AgentEvent` stream to completion (until `Done`/`Failed`), same as today.
- When `opts.animations_enabled && opts.capability.is_interactive`: runs a `tokio::select!` loop multiplexing `rx.recv()` with a `tokio::time::interval` (tick = 1000/`opts.animation_fps` ms). On each tick, advances and repaints all live `AnimationState`s using crossterm cursor control. Emits ANSI escapes only on an interactive TTY.
- When `!opts.animations_enabled || !opts.capability.is_interactive`: takes the plain-text path — prints banner text, thinking label (once), streamed text raw, tool summaries, and turn-complete summary as plain lines with NO cursor-control escapes and NO `\r`. Functionally equivalent to today's behavior plus the new persistent-info lines.
- Returns the final assistant text (same as today).

**Non-regression guarantees**:
- Return value identical to pre-refactor for the same event stream.
- Plain-text path output is a superset of today's output (adds token/duration summary; does not remove existing lines).

**Seam test**: Contract-3 (plain-text fallback) asserts no ANSI cursor escapes when capability is NonInteractive.

---

## Contract 2: `render::markdown_to_ansi`

**Signature**:
```rust
pub(crate) fn markdown_to_ansi(input: &str, theme: &Theme) -> String
```

**Callers**: `render_turn` finalize step (on `AssistantMessage`/`Done`), per R-002.

**Behavior contract**:
- Parses `input` as CommonMark via `pulldown-cmark`.
- Emits ANSI-styled text using `theme` (Pantera) colors:
  - Headings (H1–H6) → bold + gradient color per level (theme `primary`/`secondary`/`accent`/`info`).
  - Bold/italic → corresponding ANSI attributes.
  - Inline code and fenced code blocks → `theme.accent` background/foreground; fenced blocks get a language label line and preserved newlines.
  - Bullet/ordered lists → indented with Pantera-colored markers.
  - Blockquotes → indented with a `│`-style marker in `fg_more_subtle`.
  - Horizontal rules → `gradient_diagonal_field`.
  - Links → text shown with URL in `fg_more_subtle`.
- Output is a single `String` of pre-wrapped ANSI text, safe to `println!` (caller handles cursor positioning).
- Pure function: no I/O, no globals, deterministic given (input, theme).

**Seam test** (Principle IV): a unit test feeding representative markdown (heading, code block, list, bold) asserts the output contains the expected ANSI color sequences for each role. Fails if the event→ANSI mapping regresses.

---

## Contract 3: `capability::RenderCapability`

**Signature**:
```rust
pub(crate) struct RenderCapability { /* fields per data-model Entity 1 */ }

impl RenderCapability {
    pub(crate) fn detect() -> Self;          // probe stdout IsTerminal + COLORTERM + terminal_size
    pub(crate) fn level(&self) -> Capability; // Full | Reduced | NonInteractive
}

pub(crate) enum Capability { Full, Reduced, NonInteractive }
```

**Callers**: REPL startup (constructs `RenderOptions`), `render_turn` (branches on level).

**Behavior contract**:
- `detect()` reads `std::io::stdout().is_terminal()`, `std::env::var("COLORTERM")`, and `terminal_size::terminal_size()` exactly once; cheap, deterministic per process environment.
- `level()` returns `NonInteractive` iff `!is_interactive`; else `Reduced` iff no truecolor OR no unicode OR `term_width < 60`; else `Full`.
- Immutable after construction.

**Seam test** (Principle IV / SC-004): parameterized tests construct synthetic `RenderCapability` values and assert `level()` classification. A separate test asserts `detect()` on a piped stdout returns `NonInteractive`.

---

## Contract 4: `profile::AnimationProfile` registry

**Signature**:
```rust
pub(crate) struct AnimationProfile { /* fields per data-model Entity 2 */ }

pub(crate) enum AnimationKind { Banner, ThinkingSpinner, StreamingCaret, ToolLine, PromptCaret }

impl AnimationProfile {
    pub(crate) fn for_kind(kind: AnimationKind, cap: Capability) -> &'static AnimationProfile;
}
```

**Callers**: `render_turn` (selects profiles when instantiating `AnimationState`), banner animation entry.

**Behavior contract**:
- `for_kind` is a data-registry lookup (static table), NOT a central `match` with per-variant business logic — it returns a pre-built `&'static AnimationProfile`. Adding a new animation = adding one table entry + one enum variant, without editing render-loop conditionals (Constitution Principle II).
- For `Capability::Reduced`, returns the profile's `reduced` variant (ASCII frames, slower). For `NonInteractive`, returns a profile whose `disabled_fallback` is used and whose frames are never rendered.

**Seam test** (Principle IV): asserts `for_kind` returns a profile whose `frames` is non-empty for every kind under `Full`/`Reduced`, and that Reduced profiles use only ASCII-safe glyphs.

---

## Contract 5: `animation::AnimationState` + tick advancement

**Signature**:
```rust
pub(crate) struct AnimationState { /* fields per data-model Entity 3 */ }

impl AnimationState {
    pub(crate) fn new(kind: AnimationKind, cap: Capability, now: Instant) -> Self;
    pub(crate) fn advance(&mut self, profile: &AnimationProfile);  // called per tick
    pub(crate) fn current_frame(&self, profile: &AnimationProfile) -> &str;
    pub(crate) fn finalize(&mut self);                              // stop animating, clear anchor
}
```

**Callers**: `render_turn` tick-loop arm (advances all live states each tick).

**Behavior contract**:
- `advance` decrements `ticks_to_next_frame`; at zero, increments `frame_idx` mod `frames.len()` and resets the countdown.
- `current_frame` returns `profile.frames[self.frame_idx]`.
- `finalize` sets `running = false`, clears `anchor_row`.
- All live states are advanced by the single tick loop (FR-010); no element spawns its own timer.

**Seam test** (Principle IV): asserts `advance` wraps `frame_idx` correctly (after N advances ≥ frames.len(), index returns to 0) and never indexes out of bounds.

---

## Contract 6: `render::banner_animated`

**Signature**:
```rust
pub fn banner_animated(info: &BannerInfo, opts: &RenderOptions)
```

**Callers**: `repl.rs` REPL startup (replaces the current direct `render::banner` call at repl.rs:412).

**Behavior contract**:
- Full capability: runs a bounded entrance animation (gradient wipe-in of the logo, ~600–900ms via the tick timer) then prints the full banner content (delegating the static layout to the existing `render::banner` internals).
- Reduced capability: prints the static banner with no animation.
- NonInteractive: prints banner text only (plain text, no escapes).
- Always completes in bounded time (≤ ~1.5s worst case) and never blocks the prompt indefinitely.

**Seam test**: asserts that under NonInteractive, `banner_animated` writes only plain text (no cursor escapes); under a mocked Full capability with a fake clock, it emits a sequence of partial frames.

---

## Summary: what crosses the seams

| Seam | Input → Output | Guarded by test |
|---|---|---|
| `render_turn` | `AgentEvent` stream → final text + terminal output | Contract 3 fallback test |
| `markdown_to_ansi` | markdown `&str` → ANSI `String` | unit test per role |
| `RenderCapability` | env probes → `Capability` level | parameterized classification test |
| `AnimationProfile::for_kind` | `(kind, capability)` → `&'static profile` | non-empty + ASCII-reduced test |
| `AnimationState::advance` | tick → frame index wrap | wraparound unit test |
| `banner_animated` | `(BannerInfo, opts)` → terminal frames | NonInteractive plain-text test |

All seams are `pub(crate)` — no cross-crate surface added (Constitution Principle III).
