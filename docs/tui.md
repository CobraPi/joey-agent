# joey-tui — Animated Terminal Dashboard

The "busy yet elegant" synthwave-aurora terminal UI, enabled with `--tui`
(or `JOEY_TUI=1`); falls back to the line REPL when stdio isn't a terminal.

## Architecture

Elm-like: `state::App` is the model (fed agent events via `App::apply`),
`Tui` owns the terminal and animation timers, and joey-cli's `tui` module
hosts the event/render loop. Key sources: `src/{app,state,theme,widgets,
input,anim}.rs`.

### GUI/compute decoupling (engine-actor model)

All compute — agent turns, heavy slash jobs (`/neurocode index`), tool
calls — runs on a dedicated **engine task** (`joey-cli/src/engine.rs`)
that owns the `Agent`. The UI task never awaits engine work; the two
communicate over channels:

- UI → engine: `EngineCommand` (Submit / Interrupt / ForceKill /
  SwitchAgent / HeavyJob)
- engine → UI: `EngineEvent` (raw `AgentEvent`s, TurnFinished,
  HeavyJobFinished, AgentSwitched, Notice, EngineGone)

The UI loop is one `select!` over engine events, terminal input, and the
frame timer — a hung tool or stuck turn can only block the engine task;
the GUI keeps rendering at full frame rate.

**Kill & restart**: Ctrl-C escalation — while busy, the 1st press
cooperatively interrupts (the agent's interrupt flag); a 2nd press within
2s **force-kills**: the UI abandons the engine task (deliberately leaked
until process exit — even a hard-stuck blocking tool can't take the GUI
down), sets the interrupt flag, and spawns a fresh engine around a
rebuilt agent with history restored from the session DB. A notice
("engine killed & restarted … GUI stayed live") confirms the restart,
after which prompts and slash commands work immediately. Heavy jobs
(`neurocode index` etc.) run on the engine's blocking pool under the same
kill/restart regime. At idle, Ctrl-C/Esc quits as before.

Pre-turn preprocessing (intent gate, `@plan`, agent switching) happens
engine-side, where the agent lives; the UI applies resulting notices and
model-label updates via events.

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

## Expandable tool, terminal, and diff blocks

Every tool call, terminal command block, and inline file diff is
expandable in place:

- **Collapsed** — the first 10 lines of output (tools/terminal) or last 50
  diff lines, with an affordance line ("… N more lines").
- **Expanded** — the FULL result: generic tools show args + full result
  (tail-anchored, 200-line window); terminal blocks show the full command
  output tail (the END of long output is what matters) with exit code;
  file diffs show the entire diff (the 50-line bound lifts).
- **Toggle**: mouse click on the block (hit-tested), or **Space / x** in
  transcript focus (Ctrl+T / Shift+Up / PgUp to focus) — the key resolves
  the item at the viewport center via the same hit-test machinery clicks
  use, falling back to the first expandable visible item when the center
  lands on a non-expandable line. Reasoning blocks keep their three-state
  cycle (collapsed → tail-window → full).

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

## Mid-turn messaging (Hermes parity)

Three distinct behaviors when a turn is running (upstream `busy_input_mode`
semantics, default `interrupt`):

- **Plain message + Enter** — interrupts the running turn; your message
  runs as the next turn ("⚡ interrupting — your message runs next").
- **`/steer <message>`** — does NOT interrupt: the text is injected into
  the current turn's last tool result, wrapped in the upstream
  `[OUT-OF-BAND USER MESSAGE]` marker so the model treats it as a genuine
  user instruction (not tool output). It lands after the current tool
  batch / before the next model call; multiple steers concatenate. A NEW
  user message drops pending steers (they were meant for the aborted turn).
  The system prompt carries the upstream STEER_CHANNEL_NOTE so the model
  trusts the marker and ignores lookalikes in tool output.
- **`/queue <prompt>`** — queues for the NEXT turn; never interrupts.

The steer slot is Arc-shared (`Agent::steer_handle` /
`steer_via_handle`), so the engine task can receive steers mid-turn while
the turn future holds the agent borrow.

## Slash commands

Full slash menu including `/neurocode` (alias `/nc`) — run asynchronously
off the UI task (e.g. `/neurocode index` parses the whole tree) so the GUI
keeps rendering; Ctrl-C twice force-exits.

`/neurocode ingest` accepts TWO forms: the strict
`ingest <category> <path> [--version <v>] [--provenance <p>]`, and
**natural language** — describe what to ingest ("ingest the framework docs
in docs/spring as version 3.2", "ingest this postmortem: <pasted text>")
and an agent turn takes over: it locates (or writes, for pasted knowledge,
under `.neurocode/sources/`) the source, picks the category, and calls
`neurocode_ingest` itself.
