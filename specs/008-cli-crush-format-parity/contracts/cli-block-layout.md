# Contract: CLI Block Render Layouts (fully-expanded crush parity)

**Feature**: `specs/008-cli-crush-format-parity` | **Crate**: `joey-cli`

This is the presentation contract for the three block types in the CLI
streaming renderer (`render_turn` in `crates/joey-cli/src/render.rs`). It
is the CLI counterpart of the TUI's
`specs/007-tui-crush-format-parity/contracts/block-layout.md`, with the
single modification that all content is always fully expanded (no
expand/collapse, no bounding, no affordances).

All layouts use the existing `Theme::pantera()` palette and existing
`theme::gradient_*` helpers only (FR-010). No new color constants.

## Scope

Three render paths in `render_turn`, all using existing `Theme` fields:

1. Reasoning box → bordered box + full content + `└─ Thought for {:.1}s` footer (P1).
2. Terminal-command (`is_terminal_block == true`) → `$ command` block + full output (P2).
3. Generic tool-call → status icon + emoji + name + param + duration header + full result body (P3).

## §1 — Reasoning box layout (P1)

Composition (ported from TUI widgets.rs:252-344, crush `assistant.go::renderThinking`):

```
┌─ Reasoning ────────────────────────────────────  (border: t.info label + gradient fill, render.rs:487-488)
 │ <reasoning line 1>                              (body: t.fg_more_subtle)
 │ <reasoning line 2>                              (body: t.fg_more_subtle)
 │ ...
└─ Thought for 3.2s                                (footer: t.fg_more_subtle, shown iff duration > 0)
                                                   (border close: gradient_diagonal_field fallback when no duration)
```

| Element | TUI source | CLI `Theme` token | CLI render.rs |
|---|---|---|---|
| `┌─` border open | widgets.rs:300-303 | `t.fg_more_subtle` | render.rs:487 (existing) |
| " Reasoning " label | widgets.rs:295 (title) | `t.info` | render.rs:479-481 (existing) |
| Gradient fill line | — (TUI uses ratatui Block) | `theme::gradient_fg(t.info_most_subtle, t.fg_most_subtle)` | render.rs:482-488 (existing) |
| Body text | widgets.rs:308-317 | `t.fg_more_subtle` | render.rs:491-499 (existing — no change) |
| `└─ Thought for {:.1}s` footer | widgets.rs:333-336 | `t.fg_more_subtle` | NEW — replaces "N lines of reasoning" close (render.rs:383-393) |
| Border close (no duration) | widgets.rs:339-342 | `theme::gradient_diagonal_field(...)` | render.rs:395-397 (existing fallback — retained) |

**Windowing**: NONE. The CLI always shows all reasoning lines (FR-001).
No `MAX_COLLAPSED_LINES`, no `MAX_TAIL_WINDOW_LINES`, no tail-window. The
streaming renderer prints each line as it arrives (render.rs:491-499,
unchanged).

**Affordances**: NONE. No `… (N lines hidden)`, no `[click or space to
expand]`, no state labels (`reasoning (tail)` / `reasoning (full)`) (FR-003,
FR-009).

**Footer gate**: footer shown iff `reasoning_started.elapsed() > 0`
(FR-002). When `reasoning_started` is `None` or elapsed is zero, the box
closes with a plain border line.

**Visibility gate**: when `!opts.show_reasoning || opts.quiet`, no box is
rendered (existing gate, render.rs:473-475 — no regression).

## §2 — Terminal-command block layout (P2)

Rendered when `AgentEvent::ToolEnd { name, .. }` where
`is_terminal_block(&name) == true`.

```
  $ <command>  (exit N)  3.2s                      (header: $ prompt t.accent bold; command t.fg_base; badge t.error; duration t.fg_more_subtle)
    <output line 1>                                (body: t.fg_more_subtle, 4-space indent)
    <output line 2>
    ...
```

| Element | TUI source | CLI `Theme` token |
|---|---|---|
| `$` prompt | widgets.rs:352-355 | `t.accent` + bold |
| Command text | widgets.rs:358-361 (from `summary`) | `t.fg_base` |
| Status icon (`✓`/`✗`/`⟳`) | widgets.rs:363-371 | `t.success` / `t.error` / `t.busy` + bold |
| `(exit N)` badge | widgets.rs:373-379 | `t.error` (shown iff `exit_code` Some and != 0) |
| Duration `{:.1}s` | widgets.rs:382-386 | `t.fg_more_subtle` |
| Output body | widgets.rs:395-424 (from `full_result`) | `t.fg_more_subtle`, 4-space indent |

**Body source**: `full_result` when non-empty; `result_preview` as fallback
(FR-005). No bounding, no `… N more lines` affordance.

**Exit badge**: shown iff `exit_code` is `Some(n)` and `n != 0` (FR-006).
Zero exit or `None` → no badge.

**Empty output**: header line only, no body, no affordance (edge case,
spec US2.5).

**`tool_progress` gate**: applies to WHETHER the block renders, not how
much content shows. Gate logic unchanged (render.rs:638-643).

## §3 — Generic tool-call header layout (P3)

Rendered when `AgentEvent::ToolEnd { name, .. }` where
`is_terminal_block(&name) == false`.

```
  ✓ 🔧 write_file  path/to/file  3.2s               (header: icon t.success/error bold; emoji t.accent; name t.fg_base bold; param t.fg_most_subtle; duration t.fg_more_subtle)
    <result line 1>                                (body: t.fg_more_subtle, 4-space indent)
    <result line 2>
    ...
```

| Element | TUI source | CLI `Theme` token |
|---|---|---|
| Status icon (`✓`/`✗`/`⟳`) | widgets.rs:430-434, 439-443 | `t.success` / `t.error` / `t.busy` + bold |
| Tool emoji | widgets.rs:444-447 | `t.accent` |
| Tool name (bold) | widgets.rs:448-451 | `t.fg_base` + bold |
| Primary param (from `summary`) | widgets.rs:453-459 | `t.fg_most_subtle` |
| Duration `{:.1}s` | widgets.rs:435-437, 460-463 | `t.fg_more_subtle` |
| Result body (indented) | widgets.rs:395-424 (from `full_result`) | `t.fg_more_subtle`, 4-space indent |

**What is OMITTED vs the TUI** (CLI-specific adaptations):
- `▸`/`▾` expand-hint glyph: omitted (FR-009, no expand state).
- `args:` section: omitted (`full_args` never populated; `summary` already
  in header — spec Assumptions).
- State labels: omitted.

**Body source**: same as §2 — `full_result` preferred, `result_preview`
fallback (FR-008). No `MAX_TOOL_OUTPUT_LINES` bounding, no 120-char trim.

**`tool_progress` gate**: same as §2.

## §4 — Capability mode behavior (FR-015)

| Capability | Animations | Block layout | Color | Structural chars |
|---|---|---|---|---|
| Full | On (spinners, caret, tool-line rewrite) | ✅ Full layout | ✅ ANSI truecolor | ✅ Box-drawing, `$`, `✓`/`✗` |
| Reduced | On (reduced fps) | ✅ Full layout | ✅ ANSI (256-color) | ✅ (may degrade glyphs per `supports_unicode`) |
| NonInteractive | Off | ✅ Full layout | ✅ ANSI emitted (existing pattern) | ✅ Box-drawing, `$`, `✓`/`✗` survive as plain text |

In ALL modes, the structural layout (borders, headers, bodies) renders.
Only animations (spinners, carets) are gated by `animations_on`
(render.rs:279). The in-place tool-line rewrite is bypassed when
`!animations_on` (the `else` branch at render.rs:697-703 prints normally).
