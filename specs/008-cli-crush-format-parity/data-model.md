# Data Model: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Feature**: `008-cli-crush-format-parity` | **Phase**: 1 | **Date**: 2026-07-30

This feature introduces NO new data structures, NO new event types, and NO
new persistent state. All data it consumes already exists on the
`AgentEvent` stream (per spec 007). The only additions are transient
streaming-state variables local to `render_turn`. This document catalogs
those entities and the event data they consume.

---

## §1. Consumed Event Data (no changes — already present)

These fields are already on the existing `AgentEvent` variants, populated
by `joey-agent-core` (per 007 T027/T032). This feature READS them; it does
not modify, add, or re-order any field.

### AgentEvent::ReasoningDelta(String)

| Field | Type | Source | Used For |
|---|---|---|---|
| `0` (delta text) | `String` | Provider streaming | Reasoning box body content (existing) |

### AgentEvent::ToolStart

| Field | Type | Source | Used For |
|---|---|---|---|
| `name` | `String` | Tool layer | Terminal-block classification (`is_terminal_block`); header display |
| `emoji` | `String` | Tool registry | Generic tool header secondary glyph (NEW usage — existing data) |
| `summary` | `String` | Tool `summarize_args` | Terminal header `$ command`; generic header primary param |

### AgentEvent::ToolEnd

| Field | Type | Source | Used For |
|---|---|---|---|
| `name` | `String` | Tool layer | Terminal-block classification; header display |
| `is_error` | `bool` | Tool layer | Status icon selection (`✓` / `✗`); error color theming |
| `result_preview` | `String` | Tool layer | Body fallback (when `full_result` is empty) |
| `duration_secs` | `f64` | Agent loop timing | Header duration display (`{:.1}s`) — NEW usage |
| `exit_code` | `Option<i64>` | 007 T027 guarded JSON parse | Terminal header `(exit N)` badge |
| `full_result` | `String` | 007 T032 tool layer | Body content (terminal output / generic result) — NEW usage |

---

## §2. Transient Streaming State (local to `render_turn`)

These are local variables inside the `render_turn` async function. They
are NOT persisted, NOT part of any public API, and NOT visible outside
`render.rs`. They exist only for the lifetime of one turn's event stream.

### Existing state (preserved, no change)

| Variable | Type | Purpose |
|---|---|---|
| `reasoning_open` | `bool` | Whether a reasoning box is currently open |
| `reasoning_buf` | `String` | Partial line buffer for streaming reasoning text |
| `reasoning_line_count` | `usize` | Count of reasoning lines printed (legacy — retained but no longer used for the close summary; see §3) |
| `active_tool` | `Option<(u16, AnimationState, String, String)>` | In-flight tool animation (row, state, name, summary) |
| `last_tool_line` | `Option<String>` | Previous tool name for `tool_progress: "new"` dedup |
| `streamed_any` | `bool` | Whether content has been streamed this turn |
| `caret_active` / `caret_visible` | `bool` | Streaming caret state |
| `turn_in_progress` | `bool` | Whether a turn is actively streaming |

### NEW state (added by this feature)

| Variable | Type | Initial | Purpose |
|---|---|---|---|
| `reasoning_started` | `Option<Instant>` | `None` | Timestamp of the first `ReasoningDelta` of the current reasoning block; used to derive the `Thought for {:.1}s` footer duration on block close |

**Lifecycle of `reasoning_started`:**

```
[turn start]         reasoning_started = None
                     reasoning_open = false
                         │
[ReasoningDelta]  ──▶  reasoning_open: false → true
                         reasoning_started = Some(Instant::now())   ← SET
                         │
[more deltas]         reasoning lines printed live
                         │
[block close]      ──▶  close_reasoning() called:
    (ContentDelta /     duration = reasoning_started.unwrap().elapsed()
     ToolStart /        if duration > 0: print "└─ Thought for {:.1}s"
     AssistantMessage/  reasoning_started = None                   ← RESET
     Done)              reasoning_open = false
```

---

## §3. `close_reasoning` closure — signature change

**Current signature** (render.rs:375):
```rust
let close_reasoning = |open: &mut bool, buf: &mut String, line_count: &mut usize| { ... }
```

**New signature**:
```rust
let close_reasoning = |open: &mut bool,
                       buf: &mut String,
                       line_count: &mut usize,
                       started: Option<Instant>| { ... }
```

**Behavioral change**: The "N lines of reasoning" gradient summary line
(render.rs:383-393) is REPLACED by:
- If `started` is `Some(t)` and `t.elapsed().as_secs_f64() > 0.0`:
  print `└─ Thought for {:.1}s` (in `t.fg_more_subtle` color, matching TUI).
- Otherwise (no duration / zero duration): print a plain border close line
  (the existing gradient-diagonal-field fallback, render.rs:395-397).

**Call sites updated** (all existing callers of `close_reasoning`):
- `ContentDelta` arm (render.rs:529)
- `ToolStart` arm (render.rs:553)
- `AssistantMessage` arm (render.rs:542)
- `Done` arm (if present)

---

## §4. Classification: `is_terminal_block`

```rust
/// Classify whether a tool name is a terminal-command block (renders with
/// the crush `$ command` layout). Matches `joey_tui::state::is_terminal_block`
/// (007 T016, FR-013). Data-driven: tool name only.
fn is_terminal_block(name: &str) -> bool {
    name == "terminal"
}
```

This is a new private function in `render.rs`. It has no state — it is a
pure function of the tool name. It is called in the `ToolEnd` arm to
select between the terminal-block render path and the generic-tool render
path.

---

## §5. No New Entities

This feature creates no new structs, enums, traits, or type aliases. The
"Key Entities" described in the spec (CLI Reasoning Box, CLI
Terminal-Command Block, CLI Tool-Call Header) are *render concepts* — they
describe how existing event data is laid out on screen, not new data
structures. They are realized as code branches within the `ToolEnd` and
`close_reasoning` code paths, not as standalone types.
