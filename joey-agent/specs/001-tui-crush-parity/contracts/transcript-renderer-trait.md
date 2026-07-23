# Contract: `TranscriptRenderer` trait

**Crate/module**: `crates/joey-tui/src/widgets.rs` (trait) with per-variant impls colocated or in
a `widgets/transcript/` submodule if the file grows too large during implementation.

**Purpose**: Extension point for rendering each `TranscriptItem` variant (FR-003–FR-006),
satisfying Constitution Principle II by replacing today's single `draw_transcript` match (see
`crates/joey-tui/src/widgets.rs` current `TranscriptItem` handling) with per-kind registration.

## Trait

```rust
pub trait TranscriptRenderer {
    /// Compute the wrapped line count this item will occupy at the given width,
    /// so the transcript widget can do virtual scrolling without rendering
    /// off-screen items (mirrors the existing `item_lines` sizing pass).
    fn measure(&self, width: usize, theme: &Theme) -> usize;

    /// Render this item's lines starting at the given y-offset within area.
    fn render(&self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect,
              y_offset: u16, theme: &Theme, is_streaming: bool);
}
```

Each existing variant (`User`, `Assistant`, `Tool`, `Notice`, `Error`) gets a small wrapper struct
implementing this trait (e.g. `struct AssistantRender<'a>(&'a str)`), constructed by a single
dispatch function:

```rust
fn renderer_for(item: &TranscriptItem) -> &dyn TranscriptRenderer;
```

This dispatch function is the one necessarily-central piece (mapping an existing closed enum to
its renderer) — it contains no rendering logic itself, only construction, matching the same
"construction match is fine, logic match is not" rule as the Dialog contract.

## Behavioral contract

1. `Assistant` items' renderer MUST call the markdown pipeline (research.md R1:
   `pulldown-cmark` + `syntect`) and MUST support a streaming mode (`is_streaming: true`) that
   renders partially-parsed text without crashing on an incomplete code fence or list (FR-004).
2. `Tool` items' renderer MUST map `ToolStatus::{Running, Done, Failed}` to distinct
   icon+color pairs sourced from `Theme` (busy/success/error roles respectively) plus render
   `ToolResultPreview::Diff` via a unified-diff layout with additions/deletions colored from
   `Theme::success`/`Theme::error` (FR-005/FR-006).
3. Adding a 6th `TranscriptItem` variant in the future MUST require only: a new struct
   implementing `TranscriptRenderer`, a new enum variant, and one new arm in `renderer_for` —
   no changes to the transcript widget's scroll/virtualization logic in `widgets::draw_transcript`.

## Seam-level test (Constitution Principle IV)

`crates/joey-tui/tests/transcript_renderer_seam.rs` MUST:
- Define a fake `TranscriptItem`-like item and a fake `TranscriptRenderer` impl for it, local to
  the test.
- Drive it through the same virtualized-scroll measurement path used by
  `widgets::draw_transcript` (extracted as a small testable helper if not already) and assert
  the fake item's `measure`/`render` are invoked and its output appears in the resulting
  `TestBackend` buffer — proving new item kinds don't require editing the transcript widget's
  core loop.
