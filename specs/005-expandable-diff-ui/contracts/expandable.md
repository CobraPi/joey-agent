# Contract: Expandable Section state

**Feature**: `specs/005-expandable-diff-ui` | **Crates**: `joey-tui`, `joey-cli`

This is the interaction contract for the per-item expand/collapse behavior
on reasoning sections and tool calls (FR-006…011, FR-018; Clarification Q4:
per-item only). It pins the state machines, the activation surface, and the
non-interactive resolution.

## Scope: per-item only

Expand/collapse operates **per item**. Each reasoning section and each tool
call in the transcript carries its own independent state. There is no
global "expand all / collapse all" control (FR-018). This matches the
crush reference (`Expandable.ToggleExpanded` per item; click or key on the
focused item).

## Reasoning section: three-state machine

States (ported from crush `thinkingViewMode`, `internal/ui/chat/assistant.go`):

```
   Collapsed ──activate──► TailWindow ──activate──► FullExpanded
        ▲                                                   │
        └───────────────── activate ────────────────────────┘
```

| State | Rendered content | Affordance |
|-------|------------------|------------|
| `Collapsed` | Last `MAX_COLLAPSED_HEIGHT` lines (default 10, ported from `maxCollapsedThinkingHeight`). | "… (N lines hidden) [activate to expand]" |
| `TailWindow` | Last `MAX_TAIL_WINDOW_LINES` lines (default 200, ported from `maxExpandedThinkingTailLines`). Only entered if rendered line count > collapsed cap. | "… N earlier lines hidden [activate for full view]" |
| `FullExpanded` | Full reasoning text. | (collapse affordance) |

**Skip rule**: if the rendered line count ≤ `MAX_COLLAPSED_HEIGHT`, the
`TailWindow` state is skipped and activation toggles `Collapsed ↔
FullExpanded` directly.

## Tool call: two-state machine

```
   Collapsed ──activate──► Expanded ──activate──► Collapsed
```

| State | Rendered content |
|-------|------------------|
| `Collapsed` | One-line summary: `{emoji} {tool_name} — {short_summary} ({status})`. |
| `Expanded` | Full tool arguments + full result. If the tool produced `AgentEvent::FileChange`(s), the Rendered Diff Block(s) are shown inside the expanded block (FR-010). |

For long tool results, the collapsed state truncates with an affordance
(FR-011): "… (N lines hidden) [activate to expand]".

## Activation surface (interactive TUI only)

Activation is triggered by:

- **Keyboard**: a bound key on the currently-focused transcript item
  (exact keybinding finalized in `tasks.md`; crush uses Enter/Space on the
  focused item).
- **Mouse**: click on the item's hit region (TUI only; port crush's
  `HandleMouseDown` hit-testing on the item bounds).

Both map to the same state-machine transition. No other input affects
expand state.

## State ownership

State lives **on the transcript item**, not in a global registry:

- In `joey-tui`: `TranscriptItem::Reasoning` gains an `expand_state:
  ReasoningExpandState` field; `TranscriptItem::Tool` gains an `expanded:
  bool` field. The `App::apply` handler flips these on activation.
- In `joey-cli` REPL: the transcript record carries the same field,
  toggled by the REPL's key handler.

This keeps render a pure function of `(item, width, interaction_mode)`
(Principle VI: no global state threaded through shared paths).

## Non-interactive resolution (constitution Principle II)

When the CLI is non-interactive (`RenderCapability::NonInteractive`,
`--quiet`, or piped stdout), **both** state machines resolve to "fully
shown" on first render:

- Reasoning: emitted in full (`FullExpanded` semantics), no affordance.
- Tool calls: full arguments + result emitted, no truncation.

No content is hidden behind an interaction in a non-interactive context
(FR-012). This guarantees the same information reaches a piped/quiet
consumer as the interactive TUI user.

## Reasoning-visibility preference (FR-013)

The existing `show_reasoning` toggle (in the CLI `RenderOptions` and the
TUI `App.show_reasoning`) takes precedence: when reasoning is disabled, no
reasoning section is rendered at all, regardless of expand state. The
expand state is simply never consulted.

## Constants (ported from crush; final values in `tasks.md`)

| Constant | Default | Crush source |
|----------|---------|--------------|
| `MAX_COLLAPSED_HEIGHT` | 10 | `maxCollapsedThinkingHeight` |
| `MAX_TAIL_WINDOW_LINES` | 200 | `maxExpandedThinkingTailLines` |
| `RESPONSE_CONTEXT_HEIGHT` (tool result truncation) | (crush value, carried) | `responseContextHeight` |
