# Contract: Terminal capability detection API

**Crate/module**: `crates/joey-tui/src/capabilities.rs`

**Purpose**: Satisfy FR-011/SC-004 — detect truecolor/Unicode support and expose pure,
unit-testable downsampling so degraded terminals still render usably, without requiring manual
verification across real terminal emulators (per Clarifications).

## API

```rust
pub struct Capabilities {
    pub truecolor: bool,
    pub unicode: bool,
}

/// Pure decision function — no env access. Exposed so tests can drive it directly
/// with synthetic inputs instead of mutating process environment variables.
pub fn detect(colorterm: Option<&str>, term: Option<&str>, lang: Option<&str>) -> Capabilities;

/// Impure wrapper reading real process environment variables, called once at
/// Tui::enter() time and stored on App/Tui.
pub fn detect_capabilities() -> Capabilities;

/// Downsample a 24-bit color to the nearest ANSI-16 color when `caps.truecolor`
/// is false. Returns the input unchanged when truecolor is supported.
pub fn downsample_color(caps: &Capabilities, rgb: Rgb) -> ratatui::style::Color;

/// Substitute a glyph with an ASCII-safe fallback when `caps.unicode` is false
/// (e.g. spinner frames, box-drawing characters, status icons). Returns the
/// input unchanged when unicode is supported.
pub fn downsample_glyph(caps: &Capabilities, glyph: char) -> char;
```

## Behavioral contract

1. `detect` MUST return `truecolor: true` when `colorterm` is `Some("truecolor")` or
   `Some("24bit")` (case-insensitive), else `false`.
2. `detect` MUST return `unicode: true` when `lang` contains `"UTF-8"` or `"utf8"`
   (case-insensitive), else `false`. Absence of `lang` MUST default to `false` (conservative
   fallback matching Crush's own conservative default).
3. `downsample_color` MUST be a pure function: same `(caps, rgb)` input always yields the same
   output, with no I/O — this is what makes it testable without a real terminal.
4. `downsample_glyph` MUST have an explicit fallback table (e.g. spinner braille dots → `.`/`o`/
   `O` cycle, box-drawing corners → `+`/`-`/`|`) covering every non-ASCII glyph the TUI currently
   emits (survey via `search_files` over `anim.rs`/`widgets.rs` during implementation) — no
   glyph may fall through to an unmapped Unicode codepoint when `unicode: false`.
5. All widget draw functions that emit color/glyphs MUST route through `downsample_color`/
   `downsample_glyph` when `caps.truecolor`/`caps.unicode` is false — this is a per-widget
   integration requirement enforced by the smoke tests, not a capabilities.rs-only concern.

## Seam-level test (Constitution Principle IV)

`crates/joey-tui/tests/capabilities.rs` MUST be a table-driven test (per research.md R6) with at
minimum these rows, asserting exact `Capabilities` output:

| colorterm | term | lang | expected truecolor | expected unicode |
|---|---|---|---|---|
| `Some("truecolor")` | any | `Some("en_US.UTF-8")` | true | true |
| `None` | `Some("xterm")` | `Some("en_US.UTF-8")` | false | true |
| `Some("truecolor")` | any | `Some("C")` | true | false |
| `None` | `Some("dumb")` | `None` | false | false |

Plus at least one `downsample_color` test asserting a known RGB value maps to the expected
ANSI-16 color, and one `downsample_glyph` test asserting a known spinner glyph maps to its ASCII
fallback.
