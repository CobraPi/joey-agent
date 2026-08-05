# Research: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Feature**: `008-cli-crush-format-parity` | **Phase**: 0 | **Date**: 2026-07-30

This document resolves the design decisions for porting the TUI's crush
block layout to the CLI renderer. All decisions are grounded in actual
codebase analysis (file:line references) against the TUI reference
implementation in `crates/joey-tui/src/widgets.rs` and the existing CLI
renderer in `crates/joey-cli/src/render.rs`.

---

## §1. Reasoning header casing: "Reasoning" vs "reasoning"

**Decision**: Keep the CLI's existing " Reasoning " (title-case) header.

**Rationale**: The CLI reasoning box opens with `┌─ Reasoning` today
(render.rs:479, `let label = " Reasoning "`). The TUI uses lowercase
`reasoning` (widgets.rs:295, `ReasoningExpandState::Collapsed →
"reasoning"`). The CLI's header also includes a gradient fill line
(render.rs:482-488) that the TUI doesn't use (the TUI uses a ratatui
`Block::default().borders(ALL)`). The casing difference is a pre-existing
CLI-specific choice, not a regression introduced by this feature. Since the
spec's goal is "look like the TUI" at the *layout* level (boxed reasoning,
full content, footer), and the CLI already has its own header style, we
preserve the existing title-case header to avoid a gratuitous visual change
that isn't called for by any FR. The FRs reference `┌─ reasoning` as the
conceptual layout, not as a byte-level mandate.

**Alternatives considered**:
- Change to lowercase "reasoning" to match the TUI exactly. Rejected: would
  be a gratuitous change not required by any FR, and the CLI header has
  different styling (gradient fill) that already diverges from the TUI's
  ratatui Block border.

---

## §2. Reasoning footer format: `└─ Thought for {:.1}s`

**Decision**: Render the footer as `└─ Thought for {:.1}s` (one decimal
place, with the `└─` border-close prefix), matching the TUI exactly
(widgets.rs:333-336).

**Rationale**: The TUI code renders:
```rust
format!(" └─ Thought for {:.1}s", secs)  // widgets.rs:334
```
The CLI's existing `close_reasoning` closure (render.rs:375-402) currently
closes the box with a gradient fill line containing "N lines of reasoning".
The new footer REPLACES that close line. The `└─` prefix is consistent with
the `┌─` opening border (render.rs:487), creating a visually closed region.

The duration format `{:.1}s` matches:
- The TUI's reasoning footer (widgets.rs:334)
- The TUI's tool/terminal duration display (widgets.rs:384, 436)
- The CLI's existing `fmt_duration` for sub-10s values (render.rs:1057)

**Implementation detail**: The `close_reasoning` closure currently takes
`(&mut bool, &mut String, &mut usize)`. It will be extended to accept an
optional `Instant` for the reasoning-start timestamp, from which the
duration is computed as `start.elapsed()`. The "N lines of reasoning"
summary line is REPLACED by the `└─ Thought for {:.1}s` footer when a
duration > 0 is available; when no duration is available (e.g.
`reasoning_started` is `None`), the box closes with a plain border line
(the existing gradient-diagonal-field close, render.rs:396-397, is
retained as the no-duration fallback).

**Alternatives considered**:
- `Thought for Ns` (integer seconds, no border prefix). Rejected: does not
  match the TUI; decided in clarification Q3.
- Separate `└─` border line + `Thought for {:.1}s` on next line. Rejected:
  the TUI puts them on the same line; separating them adds a line of
  vertical noise without information value.

---

## §3. Terminal-block classification: local `is_terminal_block`

**Decision**: Add a private `fn is_terminal_block(name: &str) -> bool` in
`render.rs` that returns `name == "terminal"`, duplicating
`joey_tui::state::is_terminal_block` (state.rs:133-135).

**Rationale**: The workspace dependency graph is a strict DAG.
`joey-cli` does NOT depend on `joey-tui` (the TUI depends on agent-core
and tools, not vice versa). Adding a `joey-cli → joey-tui` dependency edge
to import a one-liner would violate Principle VI (Modularity and
Decoupling) and create an inappropriate coupling where the CLI depends on
the TUI crate. The TUI's function is trivially simple (`name ==
"terminal"`), tested in 007 T020, and the same test will be replicated in
the CLI. This is data-driven classification, not a hardcoded command-string
allow-list (FR-013).

**Alternatives considered**:
- Import `joey_tui::state::is_terminal_block`. Rejected: creates a
  `joey-cli → joey-tui` dependency that violates the workspace DAG.
- Move `is_terminal_block` to a shared crate (`joey-core` or
  `joey-tools`). Rejected: over-engineering for a one-liner; the function
  is presentation-layer classification, not core/tool logic. Principle
  VIII: avoid speculative abstractions.

---

## §4. Tool-call header composition: status icon + emoji + name + param + duration

**Decision**: The CLI generic tool-call header renders: status icon (`✓`
done / `✗` failed, themed), followed by the tool `emoji`, followed by the
bold tool name, followed by the primary parameter (from `summary`), and
the duration (`{:.1}s`).

**Rationale**: This matches the TUI's exact composition (widgets.rs:439-468):
```
icon → emoji → name(bold) → summary → duration → [expand hint]
```
The CLI omits only the `▸`/`▾` expand-hint glyph (FR-009: no expand state).
The status icon (`✓`/`✗`) replaces the CLI's current emoji-as-icon approach.
The CLI's current gradient-name styling (render.rs:569,
`theme::gradient_fg(&name, t.info, t.accent)`) is replaced by plain bold
themed color (`t.fg_base`) to match the TUI's
`theme.fg_base + BOLD` (widgets.rs:449-450).

**Interaction with `active_tool` in-place rewrite**: The existing
`ToolEnd` arm (render.rs:681-703) rewrites the tool-entry line in place
when animations are on and the tool name matches. This in-place rewrite
prints the RESOLVED header line (replacing the spinner frame). The full
result body is then printed on subsequent lines AFTER the rewrite. The
in-place rewrite covers exactly ONE terminal row (the header); the body
lines are appended below. This works because:
1. The rewrite uses `cursor::MoveTo(0, tool_row)` +
   `terminal::Clear(ClearType::CurrentLine)` — it clears only the header
   row.
2. The body `println!` calls advance the cursor naturally to new rows
   below.

The key change: after the in-place rewrite of the header, the code now
prints the full body (for terminal blocks: `$ command` + output; for
generic tools: indented result). This body printing happens AFTER the
rewrite, using normal `println!`, which appends to the stream below the
rewritten header row. The `active_tool` row tracking is unaffected.

**For NonInteractive / animations-off mode**: there is no in-place rewrite
(the `else` branch at render.rs:697-703 prints the line normally). The
header + body are printed sequentially via `println!`. This is the simpler
path and requires no special handling.

**Alternatives considered**:
- Drop the emoji entirely (use status icon only). Rejected: clarification
  Q2 decided to retain both; the emoji preserves per-tool visual identity.
- Keep the gradient name. Rejected: the TUI uses flat `fg_base + BOLD`, and
  the spec's goal is TUI parity.

---

## §5. `full_result` vs `result_preview` fallback

**Decision**: Prefer `full_result` for the body when non-empty; fall back
to `result_preview` when `full_result` is empty. This matches the spec's
edge-case requirement (spec.md:200-204).

**Rationale**: The `AgentEvent::ToolEnd` variant carries both
`result_preview` (first-line trimmed) and `full_result` (complete text,
added by 007 T032). Today the CLI ignores `full_result` entirely
(render.rs:636: `full_result: _`). The new code binds `full_result` and
uses it as the body source:
```rust
let body = if !full_result.is_empty() { &full_result } else { &result_preview };
```

**Edge case**: both empty → no body printed (header-only block, matching
spec acceptance scenario US2.5/US3.3).

**Alternatives considered**:
- Always use `full_result` only. Rejected: some producer paths might not
  populate `full_result` yet, leaving the block visually empty when
  `result_preview` has content.
- Concatenate both. Rejected: `result_preview` is a subset of
  `full_result`; concatenating would duplicate content.

---

## §6. NonInteractive / piped-stdout color handling

**Decision**: Follow the existing codebase pattern — emit ANSI color codes
via `.ansi().paint()` in all modes, including NonInteractive. The
structural characters (box-drawing `┌─`/`└─`, `$`, `✓`/`✗`, indentation)
are plain UTF-8 text that always renders. Do NOT add a new ANSI-stripping
layer for NonInteractive mode.

**Rationale**: The existing codebase has two patterns for NonInteractive:
1. The reasoning box (render.rs:487-501) and tool line (render.rs:646-677)
   ALWAYS use `.ansi().paint()` regardless of capability — ANSI codes are
   emitted even when piped.
2. Specific features (result preview, diff rendering) have explicit
   `if !opts.capability.is_interactive` branches that use plain `println!`
   without color (render.rs:709-710, 794-798).

Pattern (1) is the dominant pattern — the main block rendering already
emits ANSI in all modes. The new block layouts follow pattern (1):
structural layout + ANSI color always emitted, matching the existing
reasoning box and tool line behavior. This is the lowest-risk approach
(Principle VII: no regression to existing rendering behavior).

FR-015's "ANSI color codes are emitted via `.ansi().paint()` in all modes"
is satisfied by the terminal's own ANSI processing: when stdout is piped to
a file or a non-color terminal, the ANSI escape sequences are harmless
(most `cat`,
`less`, and log viewers strip or ignore them). The structural characters
survive as readable text in all cases. Adding an application-level ANSI
stripper would be a new code path with its own regression risk, and it is
not how the existing code works.

**What changes in NonInteractive mode**: the `animations_on` flag is
already `false` in NonInteractive (render.rs:279:
`opts.animations_enabled && opts.capability.is_interactive`). This means:
- No in-place tool-line rewrite (the `else` branch at render.rs:697-703
  prints normally).
- No spinner, no caret animation.
- The block layouts (headers, borders, bodies) render as static text via
  `println!` — this is the existing behavior and this feature preserves it.

**Alternatives considered**:
- Add an ANSI-stripping wrapper that checks `is_interactive` and emits raw
  text vs styled text. Rejected: high regression risk; the existing
  reasoning box already emits ANSI in all modes without issue; introduces
  a new code path for marginal benefit. Principle VIII: avoid speculative
  complexity.
- Gate the entire block layout to interactive-only (007 FR-016 approach).
  Rejected: explicitly contradicted by the user's request and FR-015.

---

## §7. `tool_progress` gate interaction with full-body rendering

**Decision**: The `tool_progress` gate (`off` / `new` / `all` / `verbose`)
controls WHETHER a block renders, not HOW MUCH content shows. When a block
IS rendered (i.e., the gate allows it), the full body is always shown.

**Rationale**: Today the `verbose` mode shows a 120-char trimmed
`result_preview` (render.rs:706-714). This feature REPLACES that trimmed
preview with the full body from `full_result`. The gate still applies:
- `off` → no block at all (render.rs:638-639)
- `new` → skip if same-name consecutive success (render.rs:641-643)
- `all` / `verbose` → render the block with full body

The distinction between `all` and `verbose` becomes: both show the full
body; `verbose` additionally shows `ToolProgress` events (render.rs:632)
and was previously the only mode that showed any preview. After this
feature, `all` and `verbose` both show the full body — `verbose` retains
its `ToolProgress` streaming advantage. The 120-char trim is removed.

**Alternatives considered**:
- Keep the 120-char trim in `all` mode, full body only in `verbose`.
  Rejected: contradicts FR-008 ("no 120-character trim"); the spec's
  intent is "fully expanded" in all modes.
- Remove the `new`/`all`/`verbose` distinction. Rejected: would break
  existing user expectations (FR-011); the gates are preserved per spec.

---

## §8. Reasoning duration derivation: `reasoning_started: Option<Instant>`

**Decision**: Add a `reasoning_started: Option<Instant>` local variable in
`render_turn`, set to `Some(Instant::now())` on the first
`ReasoningDelta` of a block, and read as `start.elapsed()` when the block
closes.

**Rationale**: The TUI derives `thought_duration` from the first
`ReasoningDelta` timestamp to block-close (007 research §3). The CLI
streams `ReasoningDelta` events live via `render_turn`, so the same
derivation works: set the timestamp when `reasoning_open` transitions
`false → true` (render.rs:476-477), read it when `close_reasoning` is
called (which happens on `ContentDelta`, `ToolStart`, `AssistantMessage`,
and `Done`).

The `close_reasoning` closure signature changes from
`|open, buf, line_count|` to `|open, buf, line_count, started|`. When
`started` is `Some(t)` and `t.elapsed() > 0`, the footer shows
`└─ Thought for {:.1}s`. When `started` is `None` (shouldn't happen if
the box is open, but defensive), or `elapsed == 0`, the box closes with a
plain border line (no footer).

**No `AgentEvent` surface change** — the duration is derived entirely in
the presentation layer from wall-clock timestamps, consistent with 007's
research decision (spec.md:19).

**Alternatives considered**:
- Add a `thinking_duration` field to `AgentEvent`. Rejected: explicitly
  avoided by 007 research; FR-012 forbids event surface changes.
- Derive from `ApiCallStart`/`ApiCallEnd` timing. Rejected: those bracket
  the entire API call, not the reasoning portion specifically.

---

## §9. Summary of all design decisions

| # | Decision | Spec FR | Risk |
|---|---|---|---|
| §1 | Keep CLI's "Reasoning" (title-case) header | FR-001 | None — existing behavior preserved |
| §2 | `└─ Thought for {:.1}s` footer replaces "N lines" summary | FR-002, FR-003 | Low — replaces one closure branch |
| §3 | Local `is_terminal_block` fn (1-liner, duplicated from TUI) | FR-013 | None — trivial logic |
| §4 | Status icon + emoji + bold name + param + duration header | FR-007 | Medium — header composition change |
| §5 | Prefer `full_result`, fall back to `result_preview` | FR-005, FR-008 | Low — bind existing field |
| §6 | Emit ANSI in all modes (existing pattern), no new stripping layer | FR-015 | None — follows existing behavior |
| §7 | Full body in all modes; remove 120-char trim | FR-008 | Low — removes a trim branch |
| §8 | `reasoning_started: Option<Instant>` for footer duration | FR-002 | Low — same pattern as existing state |

No new dependencies. No public-surface changes. No new files. All changes
confined to `crates/joey-cli/src/render.rs`.
