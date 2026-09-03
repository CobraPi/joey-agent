# joey-tui — terminal UI: rendering, panes, and the NeuroCode explorer

`joey-tui` is the animated ratatui frontend of the joey-agent workspace: a "busy yet elegant" synthwave-aurora dashboard (deep indigo-charcoal panels, a four-stop cyan→violet→magenta→lime gradient, a live particle backdrop) whose animation intensity scales live with the number of active agents. The crate is deliberately split Elm-style: `App` (`state.rs`) is the pure model that consumes `AgentEvent`s, `Tui` (`app.rs`) owns the terminal and maps crossterm keys to `TuiAction`s, and `widgets.rs` renders panels from borrowed state — the event/render loop itself lives in the host (joey-cli's `tui` module), which polls input, drains agent events into the model, and calls `Tui::tick_animations` + `Tui::draw` each frame at `Tui::frame_budget` cadence.

> See also: [../tui.md](../tui.md)

## Overview

The visual identity is a unique synthwave-aurora palette (see `theme.rs` doc comment): jewel-toned, high-saturation accents on a deep near-black canvas. `Tui` draws, but never owns agent state: joey-cli hosts the loop and is the single source of truth for wiring (it owns the agent, queues `Submit` while busy, performs clipboard copies, and routes `StopSubagent`/`SteerSubagent` to the delegation manager). Everything that moves lives in `anim.rs`; the pacing contract is that animation speed scales with the active-agent count — more agents mean faster spinners, denser particles, and more energetic equalizer bars, while idle motion decays to a calm shimmer.

Animation pacing in numbers (all from `anim.rs`):

| Signal | Formula / value |
|---|---|
| `Activity::target_fps()` | `12.0 + intensity * 12.0` → 12–24 fps |
| `Tui::frame_budget()` | `1000 / fps` ms, fps clamped to 10–60 |
| `Activity::speed()` | `0.8 + intensity * 0.7` (≈0.8× idle → ≈1.5× busy) |
| Idle intensity baseline | `0.12` (motion never fully stops) |
| Spinner base rate | ~10 fps × speed multiplier |
| Intensity target when busy | `0.35 + 0.65 * (agents/4).min(1)` |

The terminal is entered via `Tui::enter` (raw mode + alternate screen + bracketed paste + mouse capture), and a panic hook restores the terminal even mid-frame; while the TUI owns the TTY, tracing's console layer is redirected to `logs/tui-console.log` (`joey_core::logging::set_console_suppressed(true)`).

## Module map

27 files total (9 source, 15 under `tests/`, 2 examples, 1 manifest):

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; module declarations + re-exports (`Tui`, `TuiAction`, `App as AppState`, `RunMode`, `SlashCommandInfo`, `TranscriptItem`, `Theme`, `gradient_spans`) |
| `src/theme.rs` | `Rgb` color type, raw `palette` constants, `Theme` (24 `Rgb` fields), gradient samplers (`sample_stops`, `gradient_spans`) |
| `src/anim.rs` | `Activity`, `Clock`, `Spinner` (dots/orbit), `ParticleField`, `Equalizer`, `Pulse`, `HeaderFlow` |
| `src/state.rs` | The application model: `App`, `TranscriptItem`, `SubagentPane`, run modes, slash/completion/search/history state, `App::apply(AgentEvent)` |
| `src/input.rs` | Multi-line text editor for the input box (char-index cursor, ANSI-escape-stripping paste) |
| `src/widgets.rs` | All rendered panels (`draw_*` functions), hit-testing helpers, truncation/wrapping constants |
| `src/app.rs` | `Tui` controller: terminal lifecycle, frame composition (`render_body`), key → `TuiAction` mapping, mouse routing, text selection |
| `src/neurocode_viz.rs` | Fullscreen NeuroCode explorer: `VizTab`, `VizState`, `layout_nodes`, `line_cells`, `explorer_key/click/scroll`, `draw_explorer` |
| `src/neurocode_search.rs` | TUI result view over `/neurocode search` payloads: badge/result/context/relation renderers, `styled_notice_spans`, `outcome_lines` |
| `tests/smoke.rs` | Rendering-pipeline + event-contract smoke (idle/busy frames, token accounting, dedupe, scroll clamping) |
| `tests/chrome_overflow.rs` | Main-view chrome overflow regressions: popups inside frame, status-bar yield, header logo protection |
| `tests/subagent_panes.rs` | Per-subagent pane state + rendering via synthetic orchestration events on a `TestBackend` |
| `tests/subagent_overflow.rs` | Display-width-aware truncation of double-cell glyphs in the rail/OMO roster |
| `tests/pane_scroll_parity.rs` | T006/US1 (feature 017): scroll-affordance parity pane ↔ orchestrator (FR-001/002) |
| `tests/pane_expand_parity.rs` | T010/US2 (feature 017): expand/collapse parity, Ctrl+E/Ctrl+G retargeting (FR-003..005) |
| `tests/pane_search_copy.rs` | T014/US3 (feature 017): search & copy parity, `CopyPaneItem` (FR-006/007, design D5) |
| `tests/pane_maximized_parity.rs` | T019/US4 (feature 017): maximized-viewer parity — Ctrl+O viewer, reasoning, Ctrl+A stats (FR-006..008) |
| `tests/pane_fixture_smoke.rs` | Smoke for the shared pane-parity fixture (`tests/common/mod.rs`, feature 017 T002) |
| `tests/common/mod.rs` | Shared fixture builders for the parity suites (not itself a test file) |
| `tests/delegate_expand.rs` | `delegate_task` expandability from `ToolStart` (Feature 2) |
| `tests/expandable_stats.rs` | Expandable context-window entries on both stats pages (click/Space toggles) |
| `tests/expanded_view_formatting.rs` | Expanded views render embedded newlines as real line breaks in the numbered gutter |
| `tests/unified_inline_expansion.rs` | Tool/terminal/diff kinds follow the reasoning-history three-state inline cycle |
| `tests/neurocode_search.rs` | Spec 021 FR-014 (badges) + FR-008 (mode banner) rendering over `SearchOutcome` payloads |
| `examples/stress.rs` | 2000-turn `App::apply` state-machine stress (see Performance & tests) |
| `examples/anim_stress.rs` | ~500,000-frame animation tick stress across all animators |
| `Cargo.toml` | Manifest (depends on `joey-agent-core`, `joey-core`, `joey-omo`, …) |

## Public API

### `TuiAction` — requests emitted to the host

| Variant | Meaning |
|---|---|
| `Submit(String)` | Submit this prompt (host queues it if a turn is running) |
| `Interrupt` | User wants to interrupt the current turn |
| `Quit` | User wants to quit the session |
| `SwitchAgent(String)` | Switch agent (T033, BC-015; from the Tab picker) |
| `CopyItem(usize)` | Copy main-transcript item `usize` to the clipboard (`y`/`Y`) |
| `CopyPaneItem { pane, idx }` | Copy pane `pane`'s pane-relative item `idx` (T017/D4 — `y`/`Y` with a pane focused must never copy from the main transcript) |
| `StopSubagent { id }` | Stop the focused pane's child (T023, US6, FR-017, spec 020; plain `x` on a pane) |
| `SteerSubagent { id, text }` | Steer the focused pane's child with `text` (T023; the steer overlay's Enter) |

### `Tui` controller

| Method | Role |
|---|---|
| `enter(app, theme)` | Enter the alternate screen and create the terminal (installs the panic-restore hook) |
| `enter_from_leave()` | Re-enter after a `leave()` (e.g. returning from `$EDITOR` via `/prompt`); forces a full repaint |
| `leave()` | Restore the terminal; idempotent, also runs on `Drop` |
| `draw()` | Render one frame (background → particles → layout → overlays → selection pass) |
| `resize(w, h)` | Resize the terminal buffers and the particle field |
| `frame_budget()` | How long the host should sleep/poll between frames |
| `tick_animations()` / `tick_animations_with_dt(dt)` | Advance all animation state by elapsed (or fixed) dt |
| `handle_key(key)` | Map one crossterm key press to an optional `TuiAction` |
| `handle_mouse_scroll/click/down/drag/up` | Mouse routing: wheel scrolling, click hit-testing, drag-select |
| `app()` / `app_mut()` | Borrow the application state |
| `toggle_help()`, `has_selection()`, `clear_selection()` | Help overlay + selection helpers |

`Tui` is generic over the ratatui backend (`Tui<B: Backend = CrosstermBackend<Stdout>>`) so tests drive `handle_key` against a `TestBackend` without a real TTY. `Focus` is `Input` or `Transcript`; `TranscriptTarget` (private) is the single routing point (`resolve_transcript_target`) that resolves `Main` vs `Pane(usize)` for every transcript-targeted key.

### `App` state model (`state.rs`)

`App::apply(AgentEvent)` is the only ingress for agent activity; the host calls it as events stream in. Key mutators/queries: `record_user`, `push_item`, `scroll_up/down/to_top/to_bottom` and their `pane_*` counterparts, `run_search` / `search_next` (focus-following: they target the focused pane's per-view `SearchState`), `focus_subagent`, `toggle_stats`, `toggle_output_viewer`, `toggle_subagent_rail`, `subagent_rail_scroll_up/down`, and the rail hit-tests `subagent_tab_hit` / `orchestrator_tab_hit` / `subagent_rail_title_hit`. History is shared with the CLI: `history_prev/history_next/history_record` walk the same `~/.joey/.joey_history` the line REPL reads. Slash/completion menus live here too: `update_slash_menu` (two stages: command names, then pipe-encoded subcommands from `args_hint`), `slash_selected`, `set_completion_items`, `completion_menu_move`.

Supporting types:

| Type | Fields / variants |
|---|---|
| `TranscriptItem` | `User`, `Assistant`, `Reasoning { text, expand_state, thought_duration }`, `Tool { name, emoji, summary, status, duration_secs, result_preview, expand_state, full_args, full_result, is_terminal, exit_code, live_output, live_output_capacity }`, `FileDiff { path, stat, lines, is_binary, expand_state }`, `Notice { text, kind }`, `Error { text }` |
| `RunMode` | `Input`, `Busy`, `Quitting` |
| `TokenStats` | `prompt`, `completion`, `iterations`; `total()` = prompt+completion |
| `SubagentPane` | `child_id`, `goal`, `model`, `toolset_summary`, `depth`, `status`, `transcript` (ring, `transcript_capacity` = 256), `streaming_assistant`, `streaming_reasoning`, `reasoning_expanded/view/started`, `scroll`, per-child stats (`context_entries`, token/context fields, `stats_view`, `expanded_context`), per-view search (`search_open/query/has_match`), `tokens`, `started`, `summary_preview`, `tap_attached`, `spawned_by_neurocode` |
| `SubagentStatus` | `Pending`, `Running`, `Done`, `Failed`, `Stopped` (spec 020 T030 — terminal; never overwritten by a later Complete/Failed) |
| `ActiveAgent` / `AgentPhase` | `id`, `label`, `phase`, `started`, `iterations`, `max_iterations`; phases `Idle`/`QueryingModel`/`RunningTool(String)`/`Reasoning`/`Done` |
| `ActiveSubagentEntry` | `id`, `child_id`, `agent_type`, `category`, `status`, `phase`, `model`, `iterations`, `started`, `task_title` (T155), `tool_call_count`, `last_tool` |
| `DisplayAgent` | `name`, `display_name`, `color`, `mode`, `resolved_model`, `description` (Tab picker / OMO roster) |
| `SlashCommandInfo` | `name`, `aliases`, `description`, `args_hint`, `implemented` — injected by the host from joey-cli's shared slash registry |

### `Theme`, `Input`, and anim widgets

`Theme` carries 24 `Rgb` fields: brand (`primary`, `secondary`, `accent`, `keyword`, `gold`), foreground scale (`fg_base` … `fg_most_subtle`), background scale (`bg_void` … `bg_highest`), status (`success`, `info`, `warning`, `error`, `busy`, `success_subtle`, `separator`), and the signature gradient stops `grad_0..grad_3` (cyan → violet → orchid → lime). Gradient helpers: `Theme::gradient(t)` (sample the 4-stop ramp), `Theme::ramp2(a, b, n)`, `sample_stops`, and `gradient_spans` / `gradient_spans_stops` for gradient-styled text. `Theme::aurora()` is the sole built-in palette, built from the raw `palette` constants:

| Group | Constants (sRGB) |
|---|---|
| Backgrounds (deep → raised) | `BG_VOID` `#0B0B12`, `BG_BASE` `#10101B`, `BG_PANEL` `#161626`, `BG_ELEVATED` `#1D1D31`, `BG_HIGHEST` `#282840` |
| Foregrounds | `FG_BASE` `#EAEAF6`, `FG_SUBTLE` `#AEAECE`, `FG_MORE_SUBTLE` `#7878A2`, `FG_MOST_SUBTLE` `#4C4C6A` |
| Brand triad + bridges | `CYAN` `#22E4E8` (primary), `ORCHID` `#FF3D9A` (secondary), `LIME` `#B6FF3D` (accent), `VIOLET` `#B94FFF` (keyword), `GOLD` `#FFC93D` (highlight) |
| Status | `MINT` `#2EFFA8` (success), `SKY` `#3DBFFF` (info), `AMBER` `#FFB13D` (warning), `CORAL` `#FF4D6D` (error), `YELLOW` `#FFDD3D` (busy), `TEAL` `#18D6C9` (success subtle) |

`Rgb` itself provides `lerp` (linear interpolation), `to_color` (ratatui conversion), and `luma` (perceived luminance for contrast-aware dimming).

`input.rs` is a lightweight multi-line editor: one `String` per logical line, cursor column always a **character** index (byte offsets derived at the edit site — multibyte input can never split a UTF-8 boundary), with insert/cursor movement (arrows, Home/End, word jumps), backspace/delete, kill-line ops, newline insertion, and multi-line paste filtered through an ANSI-escape stripper (`StripState` recognizes CSI/OSC/simple ESC forms so terminal garbage never materializes as text).

Input-editor key arms (`handle_input_key`, once no popup owns the keys): `Alt+Enter`/`Ctrl+J` newline; `Enter` submit; `Ctrl+H`/`Backspace` backspace; `Ctrl+A`/`Ctrl+E` line start/end; `Ctrl+B`/`Ctrl+F` and plain `←`/`→` cursor left/right; `Ctrl+K`/`Ctrl+U` kill to end/start; `Ctrl+W`/`Alt+Backspace` delete word back; `Alt+B`/`Alt+F`/`Ctrl+←`/`Ctrl+→` word jumps; `↑`/`↓` move within a multi-line draft or recall history at its edges; `Ctrl+S` opens search; `?` on an empty input opens help; `/` on an empty input starts the slash popup; any other printable inserts (control-modified keys never insert).

## Layout & panels

One frame is composed in `Tui::draw` (terminals below 24×9 show only "⚠ terminal too small"):

1. Background fill (`bg_void`) and the particle backdrop (`draw_particles`).
2. Vertical layout: header (2 rows) / body (`Min(5)`) / input (`line_count + 2`, clamped 3–7) / status bar (1 row).
3. Overlays (see next section), then the buffer-level text-selection pass.

Maximized takeovers of the main area all share `split_expanded_feed(total)` (caller guarantees `total >= 12`): the transcript strip gets `(total * 0.3).round()` clamped to 4–10 rows (bottom-anchored, so the streaming tail stays visible); the takeover widget takes the rest. Precedence: stats page > output viewer > NeuroCode explorer > expanded reasoning — and `Esc` closes surfaces in that same one-Esc-one-surface order: pane reasoning expansion → pane focus → stats → output viewer → explorer dock → reasoning dock → transcript focus → interrupt/quit.

`render_body` (extracted from `draw` so tests can drive the real layout) splits the body: the subagent rail on the left (when panes exist and width ≥ 96), then transcript (left, `Min(40)`) + sidebar (right, 34 cols — yields entirely below 72 cols or while a pane's output viewer is maximized). When NeuroCode is active and not expanded, the sidebar splits vertically: OMO panel on top, context feed anchored at the bottom (up to 40% of the sidebar, min 6 rows, yields below 16 rows).

Widget inventory (`widgets.rs`):

| Widget | Role |
|---|---|
| `draw_header` | "joey" wordmark as an inverted brand chip (breathing pulse ≤20% lift), gold ✦ spark, HyperCode badge, right-aligned model + short session id + `⚡N active`/`◌ idle` + orbit spinner; the `HeaderFlow` gradient bar is the busy indicator |
| `draw_transcript` / `draw_pane_transcript` | Main and pane conversation views (scrollbar + "↓ N lines below" gold badge) |
| `draw_reasoning` | Live reasoning panel (docked 8-row strip or expanded takeover), shared by main and pane views |
| `draw_output_viewer` | Maximized live terminal output (replays finished tools), auto-follows the tail |
| `draw_stats_page` / `draw_pane_stats_page` | Maximized agent-stats/context window (live context stream; shared section builders) |
| `draw_omo_panel` | OMO sidebar: active agent, concurrency, subagent roster, equalizer |
| `draw_neurocode_panel` | Docked NeuroCode live context feed (click to open the explorer) |
| `draw_subagent_rail` | Left rail of subagent tabs (see next section) |
| `draw_input` | Input box (grows with content, focused/unfocused styling) |
| `draw_status` | Status bar: mode badge (` INPUT `/` BUSY `/` QUIT `), active agent, `⚡NEUROCODE` badge, cwd, tokens, elapsed |
| `draw_help_overlay`, `draw_search_bar`, `draw_steer_bar`, `draw_agent_picker`, `draw_slash_popup`, `draw_completion_popup` | Overlays (each popup shows 8 visible rows) |

Panels are framed by `gradient_block` (gradient title, separator border) or `gradient_block_focused` (border lerped 75%+ toward primary with a subtle pulse contribution — a steady focus indicator, not a strobe). Animations: the particle backdrop (`ParticleField`), spinners (`Spinner::dots()` — 10 braille frames — and `Spinner::orbit()` — 4 crescent phases), the 28-bar `Equalizer`, `Pulse`, and `HeaderFlow` (eased busy envelope; wave pace rides the shared activity speed).

## Subagent rail & panes

Whenever subagent panes exist and the body is ≥96 cols, the left edge hosts a vertical tab rail (each spawned child stacks a tab; the orchestrator is the implicit topmost tab = focus `None`):

- Collapsed (default): a fixed 19-col tab strip (18 inner + right border), 2 rows per tab.
- Expanded (`Ctrl+N` or clicking the rail's ` subagents (N) ▸/▾` title row): a 48-col detail rail with 4-row cards — status glyph + task title/goal, `model · depth · iterations`, live phase, last invoked tool. The rail yields (clamps back to 19) whenever the remaining main area would drop below 60 cols.
- The tab window scrolls when tabs overflow (capacity computed from the real inner height; 2 rows reserved for the pinned orchestrator tab at the bottom; a scroll indicator claims the inner right column). `Alt+Up`/`Alt+Down` scroll the rail by one pane; the wheel over the rail does the same.
- Clicking a pane tab focuses it (clicking the focused tab returns to the orchestrator); the pinned bottom tab always returns to the orchestrator view — keyboard equivalent `Ctrl+P`.

A focused pane takes over the main transcript area with `draw_pane_transcript`, and the maximized surfaces retarget to the child with full parity with the orchestrator's tabs: per-pane scroll, expand, search (`Ctrl+S` focus-follows the pane's own query/match state), copy (`y`/`Y` emit `CopyPaneItem`), stats page (per-pane `stats_view` anchor — switching focus never resets a sibling's scroll, FR-010), output viewer, docked/expanded reasoning, and — when the pane was spawned by the NeuroCode mode — the explorer takeover. Precedence in the pane branch: stats > viewer > reasoning > explorer.

## Overlays

| Overlay | Open | Close/interact |
|---|---|---|
| Help | `?` (empty input or transcript focus) or `F1` | `?`/`Esc`/`F1`/`q`/`Enter` dismiss; swallows all keys while open |
| Search bar | `Ctrl+S` (or `/` in transcript focus) | Live query; `Enter` jumps, `n`/`N` cycle, `Esc` closes; bottom 3-row bar whose match indicator mirrors the *target* view (pane vs main) |
| Agent picker | `Tab` (deferred while busy, BC-016) | `↑/↓/Tab/Shift+Tab` cycle, `Enter` switches (`SwitchAgent`), `Esc` cancels |
| Slash popup | Typing `/` in the input box | `↑/↓` navigate, `Enter` accepts (an exact command submits), `Esc` closes; subcommand stage for `/cmd arg` |
| Completion popup | `@`-context / path words | `↑/↓/Tab` navigate, `Enter` accepts, `Esc` closes; suppressed until the next edit after acceptance |
| Output viewer | `Ctrl+O` (most recent terminal item; clicking a terminal item's live region) | `Esc`/`Ctrl+O` restores; arrows/PgUp/PgDn/Home/End (+ hjkl/g/G in transcript focus) scroll; auto-follows tail |
| Reasoning panel | `Ctrl+R` toggles visibility | `Ctrl+E` cycles the newest block (tail ↔ full); clicking the docked strip expands the live stream over the main screen; `Esc` docks back |
| Stats page | `Ctrl+A` or clicking the header's right section | `Esc`/`Ctrl+A` closes; arrows/PgUp/PgDn scroll the context stream; `Space`/`x` expand entries |
| Steer overlay | `s` with a subagent pane focused (transcript focus) | `Enter` commits `SteerSubagent` (empty draft is a safe no-op), `Esc` cancels; child id frozen at open |
| NeuroCode explorer | Clicking the docked context feed panel | `Esc` (or clicking the title) docks back; see next section |

Mouse routing, in hit-test order (`handle_mouse_click`): the rail's title row (toggle expansion) → the orchestrator's pinned tab → pane tabs → the header's right section (stats toggle) → the HyperCode badge → the stats page's context entries (expand toggle) → the expanded NeuroCode explorer (own hit-testing; clicks inside never leak out) → the docked NeuroCode feed panel (dock ↔ expand) → the reasoning strips (docked ↔ expanded, target follows focus) → transcript items (inline expand toggle, via `transcript_hit_test`; pane transcript when a pane is focused). The wheel (`handle_mouse_scroll`) routes by hover target: rail, NeuroCode feed, reasoning panel, or the transcript/pane under the pointer.

## Keybindings

Complete table from `draw_help_overlay` (`widgets.rs`):

| Key | Action |
|---|---|
| `Enter` | Send — queues next prompt while busy |
| `Alt+Enter` / `Ctrl+J` | Insert newline |
| `Tab` | Agent picker; slash-menu next when popup open |
| `Shift+Tab` | Reverse cycle picker / slash menu |
| `/` | Open slash-command popup (type to filter) |
| `/cmd arg` | Subcommand suggestions (↑/↓ · ⏎ select) |
| `@` / path word | Context refs — file & folder completions |
| `↑` / `↓` (input) | Input history recall (shared with the CLI) |
| `↑` / `↓` (popup) | Navigate slash commands · ⏎ select · Esc close |
| `Ctrl+C` ×1 / ×2 (busy) | Interrupt turn / KILL & restart engine (2nd press within 2s, host-side); when idle, Ctrl+C quits |
| `Ctrl+D` | Quit (on empty input); otherwise delete-forward |
| `Shift+Up` / `Ctrl+T` / `PgUp`·`PgDn` | Scroll transcript (enters scroll mode; PgDown returns focus to input at the bottom) |
| `Ctrl+B` / `Ctrl+F` | Half-page scroll up/down (15 lines) |
| `j` / `k` / `↑` / `↓` | Scroll one line (transcript focus) |
| `Space` / `x` (transcript) | Inline-expand the item at the top/center of the view |
| `g` / `G` | Top / bottom (transcript focus) |
| `y` / `Y` | Copy last agent / user message to clipboard (`CopyItem` main, `CopyPaneItem` on a pane) |
| `/copy [n]` | Copy nth assistant message (−n counts from last) — host-side slash command |
| `Ctrl+S` | Search transcript · `n`/`N` cycle matches |
| `Ctrl+R` | Toggle reasoning panel |
| `Ctrl+E` | Cycle the newest reasoning block (tail ↔ full) |
| `Ctrl+G` | Cycle the newest tool block (inline expand) |
| `Ctrl+O` | Maximize live terminal output · `Esc` restores |
| viewer: `↑↓`/`PgUp·PgDn` | Scroll output (`g`/`G` top/bottom) · auto-follows tail |
| `Ctrl+A` / click header ▸ | Agent stats page · live context-window stream |
| stats: `↑↓`/`PgUp·PgDn` | Scroll context (`g`/`G` top/bottom) · auto-follows tail |
| `Alt+↑` / `Alt+↓` | Scroll NeuroCode context feed (when active); with panes present they scroll the rail instead |
| click feed panel | Open the fullscreen NeuroCode graph explorer |
| explorer: `←→↑↓`/`hjkl` | Select nodes on the graph canvas |
| explorer: `Shift+←→↑↓` | Pan the graph canvas |
| explorer: `+`/`−`/wheel/`0` | Zoom in/out · reset view |
| explorer: `Tab` / `⏎` | Cycle graph · nodes · feed panes |
| explorer: `Esc` / click title | Dock the explorer back |
| `Ctrl+L` | Clear transcript view (also clears panes + rail — full reset to the orchestrator view) |
| `Ctrl+P` | Back to the orchestrator tab (from a subagent pane) |
| `Ctrl+N` | Expand / collapse the subagent rail (or click its title) |
| click rail tabs | Focus a subagent · bottom tab = orchestrator |
| `Ctrl+A`/`E` · `Ctrl+U`/`K`/`W` | Line start/end · kill line/word (input editor) |
| `?` / `F1` | Toggle this help |
| `x` / `s` (pane focused) | Stop the focused child / open the steer overlay (spec 020; see Subagent rail & panes) |

Esc, when nothing is open, releases one surface at a time: a pane's expanded reasoning → pane focus → the stats page → the output viewer → the NeuroCode explorer → the expanded reasoning panel → transcript focus back to input — and only then interrupts a busy turn or quits.

Pane-operator keys (not in the overlay): plain `x` with a pane focused stops that child (`StopSubagent` — this arm precedes every legacy `x` binding), and plain `s` opens the steer overlay. Mouse drag selects text (highlight persists until the next press or `Esc`); a drag-release extracts the selected text from the rendered buffer and hands it to the host for clipboard copy — all on-screen text is selectable.

**Honest note on duplicated `Ctrl` bindings (resolved from `app.rs` source):** global arms in `handle_key` run *before* the input-editor arms, so where the help overlay lists a key twice the global binding wins. In practice: `Ctrl+A` is the stats-page toggle (the line-start arm in `handle_input_key` is unreachable — use `Home`), and `Ctrl+B`/`Ctrl+F` are half-page scroll (cursor movement is plain `←`/`→`, word jumps `Alt+B`/`Alt+F`/`Ctrl+←`/`Ctrl+→`). `Ctrl+P` has exactly one binding — return to the orchestrator tab (the comment in source notes `Ctrl+W` stays delete-word-back, and `Ctrl+N` was chosen for the rail because `Ctrl+B` was already bound). `Ctrl+C` when idle quits (line-REPL parity).

## NeuroCode explorer

`neurocode_viz.rs` implements the fullscreen takeover of the main area (opened by clicking the docked bottom-right context feed; drawn by `draw_explorer` for the orchestrator view *and* — mode-attributed — for panes whose `spawned_by_neurocode` flag was snapshotted at `SubagentSpawn`; a plain delegation pane never shows it, FR-008). Key ownership mirrors the draw gate exactly (`neurocode_explorer_owns_keys`): the explorer is fed only when it owns the screen.

Four surfaces, cycled by `Tab` (`VizTab::Graph` default, `Nodes`, `Feed`):

- **Graph canvas** — the primary target at the center, expanded nodes on concentric depth rings (6 cells per depth step × zoom, rings twisted so children sit between parents angularly; primaries fan around the center), typed edges drawn via `line_cells` (Bresenham-ish interior cells). `layout_nodes(snapshot, cx, cy, pan, zoom)` is pure: (snapshot, camera) → cell positions, which the renderer paints and the mouse hit-tests against (`node_cells` recorded at render time).
- **Node browser** — the inclusion list (reason, depth, fan-in), synced with the canvas selection.
- **Detail pane** — the selected node's full snapshot record.
- **Raw-feed tab** — the exact text NeuroCode fed the model.

`VizState` (on `App::neurocode_viz`, reset on new snapshots/open): `tab`, `pan` (cells), `zoom` (0.4–3.0, ×1.25 steps), `selected`, `list_cursor`, `feed_scroll`, `detail_scroll`, `show_neighbors` (toggled by `Space`). Input: `explorer_key` (shift-arrows pan ±4/±2 cells; hjkl/arrows select directionally on the graph or move the list/feed; `+`/`-`/`0` zoom/reset; `Enter` graph↔nodes with re-centering), `explorer_click` (node selection + tab-title dock), `explorer_scroll` (zoom on the canvas, feed scroll elsewhere).

`neurocode_search.rs` is the pure rendering surface over the RAG crate's search payload — the same payload the CLI renders as text and the `neurocode_search` tool returns as JSON:

| Renderer | Output |
|---|---|
| `badge_label` / `badge_style` / `badge_span` | FR-014 badge: `symbol-aligned` (primary, bold) vs `fallback-chunk` (warning) |
| `result_line` | `<rank>. <file> [<badge>] <symbol> (<kind>):<L12|L12-40> — score 0.1234`; missing symbols render as `(top-level code)` |
| `context_lines` | Indented ≤3-line context preview |
| `relation_lines` | `↳ relates via <kind> → <file> <symbol>` |
| `banner` (via `outcome_lines`) | FR-008 mode banner: keyword-only degradation carries the reason (warning color); hybrid shows the plain mode (info) |
| `closer` | No-match is a CLEAR response (`Nothing matched …`, distinct from failure); the empty-index variant appends the `/neurocode index` hint |
| `styled_notice_spans` | Transcript wiring (T043): notice lines that byte-identically round-trip through the CLI renderer's format strings are re-rendered styled; every other notice keeps its plain rendering |

## Rendering states

Expandable transcript kinds (tool calls, terminal blocks, file diffs, reasoning blocks) share one three-state inline cycle — `ReasoningExpandState::Collapsed → TailWindow → Full → Collapsed` (`cycle` skips `TailWindow` when it would equal `Full`). Collapsed sizes: `MAX_COLLAPSED_LINES` = 10 (reasoning), `MAX_TOOL_OUTPUT_LINES` = 10 (terminal/tool bodies, tail-anchored with a "… N earlier lines hidden" affordance), `MAX_DIFF_LINES` = 50 (collapsed diff tail); `TailWindow` = 200 lines. Expanded tool/terminal views render args and results as text-editor-like views with a numbered gutter where embedded newlines appear as **real** line breaks — never literal `\n` runs (pinned by `expanded_view_formatting.rs`).

The stats page's context-stream entries are all expandable (click or `Space`/`x` toggles the entry at the center of the visible window): collapsed entries show a one-line preview; expanded entries render the full content inline with a gutter (`MAX_EXPANDED_ENTRY_LINES` = 40 per entry).

Text hygiene: the cursor and all truncation are display-width aware (`unicode_width`) — double-cell glyphs (CJK, emoji) never blow a fixed column budget (rail tabs, OMO roster; pinned by `subagent_overflow.rs` on the real rendered buffer). Body text (assistant/user/reasoning) wraps at `MAX_CONTENT_WIDTH` = 120 columns regardless of panel width; headers, borders, and tool/terminal output are not capped. Chrome-overflow regressions are pinned by `chrome_overflow.rs`: popups stay inside the frame, the status bar's left content yields to the right-aligned keymap hint instead of colliding, and the header's right status never overwrites the wordmark.

## Performance & tests

`examples/stress.rs` drives 2000 synthetic turns through `App::apply` (each: `TurnStart`, 5 iterations of reasoning/content deltas + a terminal tool round-trip + usage, `Done`) and prints elapsed time and transcript/agent counts every 200 turns — the state machine's scaling guardrail. `examples/anim_stress.rs` ticks every animator for ~500,000 frames (≈4.5 hours at 30 fps) alternating busy/idle targets. Runtime bounds keep the model affordable: transcript rings (`transcript_capacity` = 1024 main / 256 per pane) and per-item live output capped at `LIVE_OUTPUT_CAPACITY` = 128 KB (tail window; the definitive output arrives via `ToolEnd.full_result`). Idle CPU stays low because `Tui::frame_budget` scales the poll interval with `Activity::target_fps` — an idle dashboard doesn't spin at 60 fps.

The 14 integration test files (plus the shared `tests/common/mod.rs` fixture), one line each:

- `smoke.rs` — rendering + event-contract smoke: valid frames for idle/busy states without a real TTY (spec 013 SC-001-adjacent block-rendering checks: token accounting, message dedupe, tool lifecycle resolution, scroll clamping).
- `chrome_overflow.rs` — popups in-frame, status-bar yield, header logo protection (spec 013 chrome regressions).
- `subagent_panes.rs` — pane model + rail/pane rendering from synthetic orchestration events.
- `subagent_overflow.rs` — double-cell glyph truncation on the real buffer.
- `pane_scroll_parity.rs` — T006 scroll parity pane ↔ orchestrator (FR-001/002).
- `pane_expand_parity.rs` — T010 expand parity + Ctrl+E/Ctrl+G retarget (FR-003..005).
- `pane_search_copy.rs` — T014 search/copy parity, `CopyPaneItem` (FR-006/007).
- `pane_maximized_parity.rs` — T019 maximized-viewer parity (FR-006..008).
- `pane_fixture_smoke.rs` — shared parity fixture builders compile and produce expected state.
- `delegate_expand.rs` — `delegate_task` expandable from `ToolStart`.
- `expandable_stats.rs` — context entries expandable on both stats pages.
- `expanded_view_formatting.rs` — real newlines in the expanded gutter view.
- `unified_inline_expansion.rs` — one three-state inline cycle for every expandable kind.
- `neurocode_search.rs` — spec 021 FR-014 badges + FR-008 banners over `SearchOutcome`.

## See also

- [../tui.md](../tui.md) — user-facing TUI guide
- [joey-cli.md](joey-cli.md) — the host: event/render loop, clipboard, slash registry
- [joey-agent-core.md](joey-agent-core.md) — `AgentEvent` stream consumed by `App::apply`
- [joey-omo.md](joey-omo.md) — the OMO sidebar's agents/goals model
- [joey-neurocode.md](joey-neurocode.md) and [joey-neurocode-rag.md](joey-neurocode-rag.md) — the context graph and search payloads the explorer/renderers project
- [joey-orchestration.md](joey-orchestration.md) — subagent spawn/event/complete contracts behind the rail
- [joey-tools.md](joey-tools.md) — the shared completion engine feeding the popups
- [README.md](README.md) — the docs/features index
