# Contract: Block render layouts (reasoning box, terminal-command, tool-call)

**Feature**: `specs/007-tui-crush-format-parity` | **Crate**: `joey-tui`

This is the presentation contract for the three expandable transcript block
types. It pins the visual composition (ported from crush) and the
theme-token mapping (joey-agent's existing `Theme`, per FR-014). It extends
the feature-005 `contracts/expandable.md` (state machines unchanged) with the
crush-matching *layouts*. The state machines themselves are NOT redefined
here — see feature-005 `expandable.md` for `ReasoningExpandState` and the
tool `expanded: bool` toggle.

## Scope

Three render branches in `joey-tui/src/widgets.rs::item_lines()`, all using
existing `Theme` fields only:

1. Reasoning → bordered box (P1).
2. Terminal-command (`is_terminal == true`) → `$ command` block (P2).
3. Generic tool-call → icon + name + param header + indented body (P3).

## §1 — Reasoning box layout (P1)

Composition (ported from crush `assistant.go::renderThinking`):

```
┌─ reasoning ────────────────────────────────────────────  (border: theme.fg_more_subtle)
… (N lines hidden) [click or space to expand]              (affordance: theme.fg_most_subtle)
  <collapsed cap: last MAX_COLLAPSED_HEIGHT lines>          (body: theme.fg_more_subtle, DIM)
└── (Thought for Ns)                                       (footer: theme.fg_more_subtle)
```

| Element | Crush source | joey-agent `Theme` token |
|---------|--------------|--------------------------|
| Bordered region | `ThinkingBox` style | `Block::default().borders(ALL).border_style(fg_more_subtle)` |
| State label (border title) | `thinkingViewMode` label | `fg_more_subtle` ("reasoning" / "reasoning (tail)" / "reasoning (full)") |
| Collapsed affordance | `assistantMessageTruncateFormat` | `fg_most_subtle` — text: `… (N lines hidden) [click or space to expand]` |
| Tail-window affordance | `assistantMessageTailWindowFormat` | `fg_most_subtle` — text: `… N earlier lines hidden [click or space for full view]` |
| Body text | glamour markdown | plain wrapped text (research.md §4 — deferred) `fg_more_subtle` + `DIM` |
| `Thought for Ns` footer | `ThinkingFooterTitle` + `ThinkingFooterDuration` | `fg_more_subtle` — shown iff `thought_duration` is `Some` and > 0 (FR-004) |

**Windowing (unchanged from feature 005)**: `Collapsed` → last
`MAX_COLLAPSED_HEIGHT` (10) lines; `TailWindow` → last
`MAX_TAIL_WINDOW_LINES` (200); `Full` → all. Skip rule for short text
unchanged.

**Reasoning-visibility gate (FR-013, feature 005)**: when `show_reasoning` is
false, the box is not rendered at all. The footer/box are additive only when
reasoning is shown.

## §2 — Terminal-command block layout (P2)

Rendered when `TranscriptItem::Tool { is_terminal: true, .. }`. Composition
(ported from crush `shell.go::ShellItem::RawRender` + `bash.go`):

```
$ <command>  (exit N)            (header: prompt theme.accent; command theme.fg_base; badge theme.error iff N != 0)
  <output line 1>                 (body: theme.fg_more_subtle, plain)
  …
… N more lines                    (collapsed affordance: theme.fg_most_subtle)
```

| Element | Crush source | joey-agent `Theme` token |
|---------|--------------|--------------------------|
| `$` prompt | `ShellPrompt` | `theme.accent` (bold) |
| Command text | `ShellCommand` | `theme.fg_base` |
| `(exit N)` badge | `ShellExitCode` | `theme.error` (shown iff `exit_code` `Some` and `!= 0`) |
| Output body | `ShellOutput` | `theme.fg_more_subtle` (plain) |
| Collapsed affordance | `ShellTruncation` `… N more lines` | `theme.fg_most_subtle` |
| Running indicator | `anim` spinner | `theme.busy` (status `Running`) |

**Windowing**:
- Collapsed: first `shellMaxCollapsedLines` (10, crush) lines of output +
  `… N more lines` (finished) — note crush shows *head* for finished, *tail*
  for streaming.
- Running (`status == Running`): the terminal tool is a blocking `await` that
  returns full output at completion — it emits no interim `ToolProgress`
  events, so there is no live output stream to window. A running block shows
  a `theme.busy` spinner on the header only (FR-009 scoped deliverable; true
  live streaming is out of scope for this feature).
- Expanded (`expanded == true`): full output.

**Command source**: the command string comes from the tool `summary` (for the
terminal tool, `summarize_args` yields the command). Output + exit code come
from `full_result` / `exit_code` populated per `contracts/agent-event.md`.

**Empty output**: header line only, no body, no affordance (edge case, spec).

## §3 — Generic tool-call header layout (P3)

Rendered when `TranscriptItem::Tool { is_terminal: false, .. }`. Composition
(ported from crush `tools.go::toolHeader` + `toolOutputPlainContent`):

```
<icon> <name (bold)> <primary param>  <duration>  ▸/▾    (header)
    └ <result preview>                            (collapsed preview, existing)
    … (N lines hidden) [click or space to expand]  (affordance when bounded)
  args:                                            (expanded only)
    <full args>
  result:
    <full result, tail-bounded>
```

| Element | Crush source | joey-agent `Theme` token |
|---------|--------------|--------------------------|
| Status icon (`✓`/`✗`/`⟳`) | `toolIcon` | `theme.success` / `theme.error` / `theme.busy` (bold) |
| Tool name (bold) | `Tool.NameNormal` | `theme.fg_base` + `BOLD` |
| Primary param | `toolParamList` | `theme.fg_most_subtle` |
| Indented result body | `Tool.Body` + `ContentLine` | `theme.fg_more_subtle`, 2-space indent |
| Hidden-line affordance | `assistantMessageTruncateFormat` | `theme.fg_most_subtle` — `… (N lines hidden) [click or space to expand]` |
| Expand hint `▸`/`▾` | (feature 005) | `theme.fg_most_subtle` |

**Windowing**: collapsed result body bounded to `MAX_TOOL_OUTPUT_LINES` (10,
crush) lines; expanded reveals full content tail-bounded at `MAX_*` per
feature 005 for very long results.

**Change from feature 005**: the header already shows icon+name+summary; this
contract formalizes the crush *composition order* (icon, bold name, primary
param on one line) and adds the indented-body-with-affordance for the result
preview beyond the one-line `result_preview`. The expand behavior is
unchanged.

## §4 — Interaction (shared across all three)

Activation (constitution Principle VI — single transition path):

| Input | Effect | Source |
|-------|--------|--------|
| `Ctrl+E` (focused reasoning) | cycle `ReasoningExpandState` | existing (`app.rs:434`) — unchanged |
| `Ctrl+G` (focused tool/terminal) | toggle `expanded` | existing (`app.rs:440`) — unchanged |
| Mouse left-click on a block | focus the item + toggle its expand state | NEW (research.md §5) — additive |

Click and keyboard hit the SAME state methods
(`cycle_focused_reasoning_expand` / `toggle_focused_tool_expand`); no parallel
logic.

## §5 — Non-interactive resolution (constitution Principle II)

When the CLI is non-interactive (`--quiet`, piped stdout), NONE of these
layouts apply. The one-shot renderer emits plain text: full reasoning, full
command output + `(exit N)`, full tool results — no borders, no affordances,
no hidden lines (FR-016, feature-005 FR-012 parity). The crush layouts are an
interactive-TUI presentation only.

## §6 — Constants

| Constant | Default | Source | Status |
|----------|---------|--------|--------|
| `MAX_COLLAPSED_HEIGHT` (reasoning) | 10 | crush `maxCollapsedThinkingHeight` | existing (`state.rs:32`) — reused |
| `MAX_TAIL_WINDOW_LINES` (reasoning) | 200 | crush `maxExpandedThinkingTailLines` | existing (`state.rs:34`) — reused |
| Collapsed terminal/tool output lines | 10 | crush `shellMaxCollapsedLines` / `responseContextHeight` | new constant in `widgets.rs` |
| Click hit-test | reuses scroll line accounting | n/a | new, O(items) per click |

No existing constant values change.
