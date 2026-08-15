# joey-tui — Animated Terminal Dashboard

The "busy yet elegant" synthwave-aurora terminal UI, enabled with `--tui`
(or `JOEY_TUI=1`); falls back to the line REPL when stdio isn't a terminal.

## Architecture

Elm-like: `state::App` is the model (fed agent events via `App::apply`),
`Tui` owns the terminal and animation timers, and joey-cli's `tui` module
hosts the event/render loop. Key sources: `src/{app,state,theme,widgets,
input,anim}.rs`.

- `state.rs` — `AppState`: the whole TUI state machine (phase, active
  agents, transcript items, token stats, scroll).
- `theme.rs` — the synthwave color palette + gradient helpers.
- `widgets.rs` — the ratatui render functions.
- `app.rs` — the event loop wiring state → widgets.

## Views and panels (widgets.rs)

Particle background, header (spinner + pulse), transcript with scrollbar
and streaming assistant/reasoning text ("Thought for Ns" footer), reasoning
panel, OMO agent panel, multi-line input editor, status bar (token
accounting, elapsed), help overlay, search bar, agent picker, and a
slash-command popup.

## Animations (anim.rs)

Particle field, spinners, equalizer, pulse, activity signal — all paced by
one global `Activity` signal: **animation speed scales live with the number
of active agents**, easing to a calm shimmer when idle.

## RunMode

Input / Busy (turn in progress, busy input styling) / Quitting.

## Input history recall (CLI parity)

Plain ↑ / ↓ in the input box walks the shared input history
(`~/.joey/.joey_history`) — the exact same file, format (reedline
newline-escaped), and recall semantics as the line REPL:

- ↑ on a single-line draft (or with the cursor on the first line of a
  multi-line draft) recalls the previous (older) entry; the in-progress
  draft is saved first.
- ↓ on the last line recalls the next (newer) entry; moving past the
  newest entry restores the saved draft.
- Submitting records the input (consecutive-duplicate dedup, 10 000-entry
  cap) and resets recall state — entries made in either surface (CLI or
  TUI) are recallable in both.
- Inside a multi-line draft, ↑ / ↓ move the cursor between lines (the
  boundary rule above still triggers history at the first/last line).
- Transcript scrolling that plain ↑ used to trigger moved to
  Shift+Up / Ctrl+T / PgUp (j/k also work in transcript focus).

## Smart completions (Hermes parity)

Both surfaces share one completion engine (`joey-tools::completion`, ported
from upstream `hermes_cli/commands.py::SlashCommandCompleter`):

- **Slash commands** — Tab popup (CLI) / auto-popup (TUI) with names,
  aliases, descriptions, arg hints, implemented status.
- **Subcommands** — after `/cmd ` the first argument word completes against
  the command's pipe-encoded args_hint (`/timestamps <Tab>` → on/off/status;
  `/llm-selector st` → status). Offered for implemented commands only.
- **@-context refs** — Claude-Code-style `@diff`, `@staged`, `@file:`,
  `@folder:`, `@git:`, `@url:` static references; bare `@query` runs a
  fuzzy project-wide file search (rg-listed, 5s cache, scoring: exact 100 /
  prefix 80 / substring 60 / path 40 / boundary-initials 35).
- **Path completions** — words starting `./`, `../`, `~/`, `/` or containing
  `/` (URLs excluded) list directory entries, dirs first, with size labels.
- **Ghost text (CLI)** — fish-style inline hints as you type: unique
  slash-name remainder (`/hel`→`p`), subcommand remainder, or the newest
  matching history entry's tail.

CLI (reedline): Tab opens the description menu (fixed to
`only_buffer_difference(false)` — the upstream default left the menu
perpetually empty); ↑/↓ navigate, Enter accepts. TUI: popups render above
the input box; ↑/↓/Tab navigate, Enter accepts, Esc closes; accepting a
completion dismisses it until the next edit; @-search file listings refresh
on a background thread (never stalls a frame).

## NeuroCode live context panel (feature 015)

When the NeuroCode engine is active (`neurocode.enabled: true` in config):

- **Status badge** — `⚡NEUROCODE` appears in the status bar whenever the
  engine is wired, so context-graph injection is never silent. A transcript
  notice on startup announces it too.
- **Live context feed** — the right sidebar splits: the OMO panel keeps the
  top, and a `neurocode · context feed` panel anchors the bottom-right. On
  every model dispatch it shows exactly what NeuroCode fed the agent — the
  same string prepended to the system prompt (`AgentEvent::NeuroCodeContext`):
  the serving tier, estimated tokens, graph-expanded node count, a `COLD`
  badge in degraded (un-indexed) mode, and the full context text
  (hard-wrapped, tail-anchored). **Alt+↑ / Alt+↓** scroll the feed without
  leaving the input box.
- The panel yields entirely on narrow/short terminals or when NeuroCode is
  off — the layout is byte-identical to pre-feature when inactive.

## Slash commands

Full slash menu including `/neurocode` (alias `/nc`) — run asynchronously
off the UI task (e.g. `/neurocode index` parses the whole tree) so the GUI
keeps rendering; Ctrl-C twice force-exits.
