# Contract: Tab Agent Picker

**Feature**: 003-omo-orchestration

## State Machine

```
                  ┌──────────────────────────────────────────┐
                  ▼                                          │
  Input ──Tab──► Picker Open ──Tab──► (cycle highlight)     │
                  │               ↑↓ (move selection)        │
                  │               Enter (select)             │
                  │               Esc (cancel)               │
                  └──Esc──► Input (unchanged)                │
                  └──Enter──► Input (agent switched)         │
```

## TUI Behavior

- **BC-013**: Pressing Tab in Input mode opens an overlay picker listing
  available primary agents in canonical order: Default, Sisyphus,
  Hephaestus, Prometheus, Atlas. Skipped agents (unavailable models) are
  hidden.
- **BC-014**: Within the picker, Tab or ↓ cycles forward; Shift+Tab or ↑
  cycles backward. Enter selects. Esc cancels (no change).
- **BC-015**: Selecting an agent emits `AgentModeChanged` event, rebuilds
  the system prompt, and updates the status bar with the agent name + color.
- **BC-016**: If a turn is in progress (Busy mode), the switch is deferred
  to the next user turn (queued, not applied mid-execution).
- **BC-017**: The Default agent (existing joey-agent) is always the 0th
  entry and is always available.

## CLI Behavior (Parity)

- **BC-018**: In the CLI REPL, pressing Tab (reedline completion) or typing
  `/agent` prints a numbered list. User types a number to select.
- **BC-019**: The CLI prompt indicator updates to show the active agent.

## Tab Picker Overlay Layout

```text
┌─ Agent Mode ─────────────────────┐
│ ► Default                        │
│   Sisyphus      [claude-opus-4.8]│
│   Hephaestus    [gpt-5.6-sol]    │
│   Prometheus    [claude-opus-4.8]│
│   Atlas         [claude-sonnet-5]│
└──────────────────────────────────┘
```

Each row: agent display_name + resolved model. The currently active agent is
marked (►). Arrow keys move the cursor; Enter confirms.

## Keybinding Changes

| Key | Before | After |
|-----|--------|-------|
| Tab | Toggle focus Input ↔ Transcript | Open/cycle agent picker |
| Shift+Tab | (unused) | Cycle agent picker backwards |
| Up arrow (input) | (unused) | Focus transcript (replaces lost Tab-focus) |
| j/k (any) | Scroll transcript | Scroll transcript (unchanged) |

The help overlay (`?`) is updated to document these changes.
