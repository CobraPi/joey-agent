# Data Model: Crush-Style Expandable Block Formatting (TUI)

**Feature**: `specs/007-tui-crush-format-parity` | **Phase**: 1

The entity/field changes are minimal and strictly additive. There are three
touch points: the `AgentEvent::ToolEnd` variant (event layer), the
`TranscriptItem::Reasoning` and `TranscriptItem::Tool` variants (TUI state),
and a CLI-render local. No on-disk model changes.

---

## §1 — `AgentEvent::ToolEnd` (event layer, `joey-agent-core/src/events.rs`)

Additive field on an existing struct variant. This is the single source both
surfaces consume (constitution Principle II).

```rust
pub enum AgentEvent {
    // ...
    ToolEnd {
        name: String,
        is_error: bool,
        /// A short preview of the tool result (first line, truncated). (existing)
        result_preview: String,
        /// Execution duration in seconds. (existing)
        duration_secs: f64,
        /// NEW (feature 007): process exit code for terminal/shell tool calls.
        /// `None` for non-terminal tools, errors, and any tool whose result
        /// does not carry an exit code. Sourced by a guarded JSON parse at the
        /// agent-loop boundary (research.md §1). Drives the `(exit N)` badge.
        exit_code: Option<i64>,
    },
    // ...
}
```

**Validation / invariant**: `exit_code` is `Some(n)` only when the emitting
tool is `terminal` AND its result JSON decoded an integer `exit_code`. For all
other tools it is `None`. Non-negative on normal exit; may be negative for
signal kills (`terminal_tool.rs:492`) or `-1` for spawn failure
(`terminal_tool.rs:442`).

**Backward compatibility**: additive field. Every existing `ToolEnd { ... }`
literal must add `exit_code: <expr>` to compile (Rust exhaustive struct init).
The two production sites (`agent.rs:1949`, `:1980`) pass the extracted value;
all test/helper sites pass `None`. See `contracts/agent-event.md`.

---

## §2 — `TranscriptItem::Reasoning` (TUI state, `joey-tui/src/state.rs`)

Additive fields for box footer data. State machine (`ReasoningExpandState`)
is unchanged.

```rust
pub enum TranscriptItem {
    // ...
    Reasoning {
        text: String,
        expand_state: ReasoningExpandState,   // existing (feature 005)
        /// NEW (feature 007): wall-clock duration the model spent producing
        /// this reasoning block, derived in-state (first ReasoningDelta →
        /// flush). `None` while streaming or if undeterminable. Drives the
        /// `Thought for Ns` footer (FR-004). research.md §3.
        thought_duration: Option<std::time::Duration>,
    },
    // ...
}
```

**State transition (unchanged, feature 005)**: `Collapsed → TailWindow →
Full → Collapsed` with the short-text skip rule. The new field does not
participate in the cycle — it is render-only metadata.

**Derivation lifecycle**:
1. First `ReasoningDelta` of a block → `App.reasoning_started = Some(Instant::now())`.
2. `flush_reasoning()` fires (on `ContentDelta`/`ToolStart`/turn end) →
   `thought_duration = reasoning_started.map(|s| s.elapsed())`, stored on the
   pushed `Reasoning` item; `reasoning_started` reset to `None`.

---

## §3 — `TranscriptItem::Tool` (TUI state, `joey-tui/src/state.rs`)

Modeled as a **presentation flag** on the existing variant, NOT a new enum
variant (research.md §2). Two additive fields.

```rust
pub enum TranscriptItem {
    // ...
    Tool {
        // ── existing (feature 005) ──
        name: String,
        emoji: String,
        summary: String,
        status: ToolStatus,
        duration_secs: Option<f64>,
        result_preview: String,
        expanded: bool,
        full_args: Option<String>,      // NOW actually populated (was always None)
        full_result: Option<String>,    // NOW actually populated (was always None)
        // ── NEW (feature 007) ──
        /// True when this tool call is a terminal/shell command and should
        /// render with the `$ command` block layout instead of the generic
        /// tool header. Set once at ToolStart from `name == "terminal"`.
        /// FR-017 / research.md §2.
        is_terminal: bool,
        /// NEW (feature 007): process exit code, for the `(exit N)` badge.
        /// Copied from AgentEvent::ToolEnd.exit_code at ToolEnd time.
        /// `None` for non-terminal tools.
        exit_code: Option<i64>,
    },
    // ...
}
```

**Key correction**: `full_args` and `full_result` already exist on the variant
(feature 005, `state.rs:97-99`) but are initialized to `None` at `ToolStart`
(`state.rs:521-522`) and **never populated** by `ToolEnd` handling
(`state.rs:540-557` does not touch them). Feature 007's event-plumbing change
populates them: at `ToolEnd`, the agent loop additionally emits the full args
string (from `summarize_args`'s input) and full result text (from
`to_content_string`) so the TUI can store them. This is the data that makes
the crush expanded views and the `$ command` body possible.

**Validation**: `is_terminal == true` implies `name == "terminal"`. `exit_code`
is consulted by the renderer only when `is_terminal == true`.

**Lifecycle**:
1. `ToolStart { name, .. }` → push `Tool { is_terminal: is_terminal_block(&name), exit_code: None, full_args: None, full_result: None, .. }`.
   (`full_args` stays `None` — `ToolStart` does not carry args; the terminal
   header derives `$ command` from `summary` via `summarize_args`. See
   `contracts/agent-event.md` Approach A.)
2. `ToolEnd { exit_code, .. }` → set `exit_code`, and set `full_result` from
   the now-available full result text. `full_args` remains `None` (no args
   data is available on `ToolStart` or `ToolEnd` without an event-surface
   extension that was explicitly rejected; the terminal header derives its
   `$ command` from `summary`).

---

## §4 — CLI renderer local (`joey-cli/src/render.rs`)

No new struct. The existing `ToolEnd` match arm (`render.rs:636`) gains a read
of the new `exit_code` field for plain-text parity: when present and non-zero,
append ` (exit N)` to the printed tool line. This keeps the one-shot CLI able
to surface the same exit code the TUI badges (Principle II), without adopting
any interactive affordances.

---

## Entity relationship summary

```
AgentEvent::ToolEnd (events.rs)
   │  + exit_code: Option<i64>   (NEW)
   │  + full result text flows via existing content path
   ▼
App::apply() (state.rs)
   │  populates TranscriptItem::Tool { is_terminal, exit_code, full_result }  (NEW)
   │  derives   TranscriptItem::Reasoning { thought_duration }                (NEW)
   ▼
item_lines() (widgets.rs)
   │  branches on is_terminal → terminal block layout | tool header layout
   │  wraps reasoning in bordered Block + footer
   ▼
render.rs ToolEnd arm (cli)
   │  reads exit_code for plain-text parity
```

No new entities. No new enums (the terminal block is a flag on `Tool`, not a
new `TranscriptItem` variant). No new crates. All changes additive.
