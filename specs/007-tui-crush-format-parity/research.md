# Research: Crush-Style Expandable Block Formatting (TUI)

**Feature**: `specs/007-tui-crush-format-parity` | **Phase**: 0

This resolves the design decisions deferred during specify/clarify. Each
section records Decision / Rationale / Alternatives. All are grounded in the
actual joey-agent source (`crates/`) and the crush reference
(`~/Development/crush/internal/ui/chat/`).

---

## §1 — Additive `ToolEnd` field: exact shape & migration

**Decision**: Add a single typed field to the existing `AgentEvent::ToolEnd`
struct variant:

```rust
ToolEnd {
    name: String,
    is_error: bool,
    result_preview: String,
    duration_secs: f64,
    exit_code: Option<i64>,   // NEW; None for non-terminal tools & errors
},
```

The two existing construction sites (`crates/joey-agent-core/src/agent.rs:1949`
parallel path and `:1980` sequential path) set `exit_code` by parsing the tool
result content **only inside the tool layer's dispatch**, not in the agent
loop. Concretely: `Terminal::execute` already builds a JSON object containing
`exit_code`; the agent loop extracts it via a small helper
(`extract_exit_code(tool_name, &content) -> Option<i64>`) that JSON-parses the
content and reads `.exit_code` **only when `tool_name == "terminal"`** and the
parse succeeds. For all other tools it returns `None`. No free-text heuristic.

**Why the parse lives at the agent-loop boundary, not in the event enum**:
`ToolResult` is `Text(String) | Multimodal(Vec<Value>) | Error(String)`
(`crates/joey-tools/src/registry.rs:27`). The terminal tool's `exit_code` is
already serialized INTO that `Text` as JSON (`terminal_tool.rs:326-340`:
`result.insert("exit_code", json!(returncode))`). Widening `ToolResult` to
carry typed metadata would touch the tool trait (a public surface) for every
tool, violating Principle VIII lean-code and Principle VII surface-stability.
A single guarded parse at the known terminal boundary is narrower and
backward-compatible.

**Rationale**: Additive struct field with a `None` default is the minimal
backward-compatible extension (Principle VII). Every existing `ToolEnd { ... }`
construction site that does not name `exit_code` fails to compile under Rust's
exhaustive-struct-init — so the migration is *forced and explicit*, not silent.
Feature-005 tests that build `ToolEnd` literals
(`crates/joey-agent-core/src/agent.rs:1584` CLI test helper;
`crates/joey-tui/src/state.rs:1261`-area tests) add `exit_code: None` in the
same PR. No runtime behavior changes for non-terminal tools.

**Alternatives considered**:
- *(rejected)* Parse exit code from `result_preview` in the renderer — the
  preview is already one-line-truncated (`preview_result`,
  `agent.rs:2404`), so the JSON is destroyed before it reaches the renderer.
- *(rejected)* Add a brand-new `AgentEvent::ToolResultDetails` variant — adds
  event-surface churn and a second stream to reconcile per iteration; the
  agent loop already has the result in hand at the `ToolEnd` emission site.
- *(rejected)* Widen `ToolResult` with typed metadata — touches the `Tool`
  trait for every tool; violates surface stability (Principle VII).

---

## §2 — Terminal-block classification

**Decision**: A pure function classifies a `TranscriptItem::Tool` as a terminal
block iff its `name == "terminal"` (the exact `Terminal::name()` value,
`terminal_tool.rs:171`):

```rust
fn is_terminal_block(name: &str) -> bool { name == "terminal" }
```

The classification is stored on the item at `ToolStart` time as a bool flag
(`is_terminal`) rather than re-derived each render, so the render branch is a
single match arm. The `$ command` prompt string comes from parsing the tool's
`full_args` for the `command` field (already populated by §1's plumbing); the
output body and `(exit N)` badge come from `full_result`/`exit_code`.

**Rationale**: FR-017 mandates data-driven classification, not a command-string
allow-list. Tool name is the stable identity the event stream already carries
(`AgentEvent::ToolStart { name, .. }`). Storing the flag once avoids
re-parsing on every frame (Principle VIII). The `command` field name matches
the terminal tool's schema (`terminal_tool.rs:190`).

**Alternatives considered**:
- *(rejected)* A new `TranscriptItem::TerminalCommand { .. }` enum variant —
  duplicates the `expanded`/`status`/`duration_secs`/`full_args`/`full_result`
  fields already on `Tool`, and forces every `match TranscriptItem` arm
  (`widgets.rs`, `state.rs:922 transcript_item_text`, tests) to gain a branch.
  A presentation flag on the existing variant is leaner (Principle VIII) and
  keeps the shared expand machinery in one place.
- *(rejected)* Re-derive `is_terminal` from the name each render — redundant
  string compare per frame; the flag is set once at event time.

---

## §3 — Reasoning footer duration derivation (in-state, no event change)

**Decision**: The TUI `App` state records an `Instant` the first time a
`ReasoningDelta` arrives for a block (`reasoning_started: Option<Instant>`),
and computes the duration when the block closes — i.e. when `flush_reasoning()`
fires (on `ContentDelta` or `ToolStart`, `state.rs:353`). The `TranscriptItem::Reasoning`
variant gains `thought_duration: Option<Duration>`; the box footer renders
`Thought for {secs}s` when `Some` and > 0.

**Rationale**: Clarification Q2 chose in-state derivation to avoid extending
the NON-NEGOTIABLE `AgentEvent` public surface for cosmetic data (Principle
VII). The first-delta-to-flush interval is a faithful proxy for "how long the
model thought" because `flush_reasoning` is exactly the boundary between the
reasoning phase and the content/tool phase (`state.rs:353-371`). The CLI
surface already has its own reasoning-open/close tracking (`render.rs:375`
`close_reasoning`), so it can derive the same value independently if/when it
adopts the footer — no event coupling required.

**Edge case**: if a turn ends with reasoning still open (model produced only
thinking, no content), `flush_reasoning` still fires on turn cleanup
(`state.rs:785` resolves still-Running tools; reasoning flush happens via the
same done path), so the duration is always recorded.

**Alternatives considered**:
- *(rejected)* Add `duration_secs: f64` to a `ReasoningEnd` event — extends
  the public event surface for display-only data; rejected in clarification Q2.
- *(rejected)* Skip the footer — drops an explicit FR-004 requirement and a
  piece of crush parity.

---

## §4 — Thinking body: plain text vs markdown (deferred to v2)

**Decision**: In v1, the reasoning box body renders the thinking text as
**plain wrapped text** (the current behavior), NOT glamour/markdown-rendered.
Crush glamour-renders thinking (`assistant.go:460`
`common.QuietMarkdownRenderer`); joey-agent does not today, and adopting a
markdown renderer is a dependency-weight decision gated by Principle VIII.

**Rationale**: Introducing `comrak`/`pulldown-cmark`+a styler into `joey-tui`
(a hot per-frame path) has a real binary-size and compile-time cost for a
cosmetic gain (thinking text is rarely dense markdown). The spec's scope is
*layout/formatting parity*, and the boxed + windowed + footered layout is
deliverable without markdown. `joey-cli` already has a markdown path
(`crates/joey-cli/src/markdown.rs`, pulldown-cmark) for *assistant content*
finalization, but `joey-tui` deliberately does not. Deferring keeps v1 lean.

**Alternatives considered**:
- *(rejected for v1)* Port `joey-cli`'s `markdown_to_ansi` into `joey-tui` —
  would pull `pulldown-cmark` + `syntect` into the TUI crate's dependency
  graph and add a per-frame render cost on every reasoning line. Recorded as
  a follow-up: if a concrete need emerges (e.g. reasoning contains fenced
  code that renders poorly), revisit with a measured budget in a new feature.
- *(rejected)* Add a `comrak` dependency fresh — heavier than pulldown-cmark,
  no incremental benefit over the existing CLI dependency were it to be shared.

---

## §5 — Click-to-toggle mouse routing

**Decision**: Extend the existing `Tui::handle_mouse_scroll`
(`crates/joey-tui/src/app.rs:756`) to also handle
`MouseEventKind::Down(MouseButton::Left)`. On a left-click inside the
transcript area, the handler: (a) computes which transcript item row was
clicked (using the same per-item line accounting `draw_transcript` already
performs, `widgets.rs:466`), (b) sets `app.focus = Transcript` and focuses
that item index, (c) toggles that item's expand state via the existing
`cycle_focused_reasoning_expand()` / `toggle_focused_tool_expand()` paths.

**Rationale**: Clarification Q3 chose keyboard + click. Mouse capture is
already enabled (`EnableMouseCapture`, `app.rs:110`) and a mouse handler
already exists for scroll — click is an additive `match` arm in the same
function, not a new subsystem (Principle VIII). Reusing the existing expand
methods means click and keyboard hit the *same* state transition (Principle
VI: no parallel logic). Click→focus+toggle matches crush's
`HandleMouseClick`/`HandleMouseDown` model.

**Hit-testing approach**: `draw_transcript` already iterates items to compute
rendered line counts for scroll; the click handler reuses that accounting
(mapped to the current scroll offset) to resolve the clicked item index. No
new geometry bookkeeping beyond what scroll already needs.

**Alternatives considered**:
- *(rejected)* Keyboard-only — drops the "click to expand" affordance the spec
  advertises (FR-013); rejected in clarification Q3.
- *(rejected)* A separate mouse-state machine — duplicates the expand logic;
  violates Principle VI.

---

## §6 — Performance budget (hot paths)

**Identified hot paths and budgets**:

| Path | Budget | Justification |
|------|--------|---------------|
| Collapsed block render (any of 3 types) | ≤ current tool/reasoning render cost | Bounded window (≤10 lines reasoning, ≤10 lines terminal/tool output via reused `MAX_*` constants). Bordered-box draw is a ratatui `Block` — O(visible). |
| Expanded terminal-command block | O(visible output), never O(total) | Reuses the feature-005 tail-window cap (`MAX_TAIL_WINDOW_LINES`/`MAX_DIFF_LINES` pattern); very long output is bounded + advertises hidden count. |
| Click hit-testing | O(transcript items) per click, amortized O(1) | Clicks are infrequent (human-paced); iteration over items reuses scroll's line accounting. Not per-frame. |
| `exit_code` extraction | O(1) JSON parse, terminal tools only | Guarded by `name == "terminal"`; non-terminal tools skip the parse entirely (returns `None`). Runs once per tool call, not per frame. |

No per-frame work is added beyond a bordered `Block` wrap on reasoning items.
The steady-state frame budget (host frame-batched draw) is unchanged.

**Alternatives considered**: none needed — no path exceeds budget. The only
watch-item (markdown in thinking) was deferred in §4 specifically to avoid a
hot-path cost.

---

## §7 — Reference mapping (crush → joey-agent)

Pinpoints the crush source each layout decision is ported from, so the
implementation is verifiable against the reference:

| Crush source | joey-agent target | What's ported |
|---|---|---|
| `internal/ui/chat/assistant.go` `renderThinking` (lines ~459-500) | `joey-tui/widgets.rs` reasoning arm | Bordered box + windowed slice + `Thought for Ns` footer |
| `assistant.go` constants `maxCollapsedThinkingHeight`=10, `maxExpandedThinkingTailLines`=200 | already present in `joey-tui/state.rs:32-34` | Reuse existing `MAX_COLLAPSED_HEIGHT`/`MAX_TAIL_WINDOW_LINES` — no change |
| `assistant.go` affordance strings `assistantMessageTruncateFormat`, `assistantMessageTailWindowFormat` | `joey-tui/widgets.rs` reasoning affordance lines | Wording parity (`… (N lines hidden) [click or space to expand]`) |
| `internal/ui/chat/shell.go` `ShellItem.RawRender` (lines 191-305) | `joey-tui/widgets.rs` new terminal arm | `$ command` prompt, output body, `(exit N)` badge (`ShellExitCode`), tail-biased streaming window (`shellMaxCollapsedLines`=10) |
| `internal/ui/chat/tools.go` `toolHeader` (lines 624-648) + `toolOutputPlainContent` (651-681) | `joey-tui/widgets.rs` tool arm | icon + bold name + primary-param header; indented bounded body + `… (N lines hidden)` affordance (`responseContextHeight`=10) |
| `shell.go`/`tools.go` `ToggleExpanded` per item | already present (`state.rs:386 toggle_focused_tool_expand`) | Reuse existing per-item toggle; add click path (§5) |

All semantic crush tokens map onto existing `joey-tui` `Theme` fields:
`ThinkingBox` border → `theme.fg_more_subtle`; `ShellExitCode`/error →
`theme.error`; success icon → `theme.success`; running/busy → `theme.busy`;
affordance text → `theme.fg_most_subtle`. No new `Theme` fields (FR-014).
