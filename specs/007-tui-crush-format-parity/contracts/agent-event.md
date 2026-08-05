# Contract: `AgentEvent::ToolEnd` extension + result plumbing

**Feature**: `specs/007-tui-crush-format-parity` | **Crates**: `joey-agent-core` (producer), `joey-tui` + `joey-cli` (consumers)

This is the wire/event contract for the additive presentation-data plumbing
that backs all three block layouts. It is the single source consumed by both
the TUI state machine and the CLI renderer (constitution Principle II: CLI/TUI
parity). It extends the feature-005 `ToolStart`/`ToolEnd` events without
altering their existing fields.

## Scope

Two additive changes, both backward-compatible:

1. A typed `exit_code: Option<i64>` field on `AgentEvent::ToolEnd`.
2. Population of the already-declared `full_args` / `full_result` on the TUI
   `TranscriptItem::Tool` (declared in feature 005, never populated).

This contract pins the field shapes, the producer extraction rule, and the
consumer obligations. It does NOT introduce a new event variant.

## Contract

### `ToolEnd` — additive field (events.rs)

```rust
ToolEnd {
    name: String,
    is_error: bool,
    result_preview: String,     // existing
    duration_secs: f64,         // existing
    /// NEW. Process exit code for `terminal` tool calls; None otherwise.
    exit_code: Option<i64>,
}
```

### Producer rule (agent.rs, both ToolEnd sites at :1949 and :1980)

The agent loop extracts `exit_code` via a single guarded helper at the
emission site:

```rust
fn extract_exit_code(tool_name: &str, content: &str) -> Option<i64> {
    if tool_name != "terminal" { return None; }            // FR-017 classification
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("exit_code")?.as_i64())
}
```

- Runs **only** for `terminal` tools (O(1) guard; non-terminal tools skip the
  parse — Principle VIII).
- On parse failure / missing field → `None` (graceful; never panics).
- The terminal tool already serializes `exit_code` into its JSON result
  (`terminal_tool.rs:328`), so no tool-layer change is required.

### `full_args` / `full_result` population

Two viable approaches; the plan selects **Approach A** (minimal surface):

**Approach A (chosen) — populate at ToolEnd only, derive command from summary.**
The `ToolEnd` event additionally carries the full result text (it already has
`content` in hand at the emission site). The TUI stores it in `full_result` at
`ToolEnd` time. The `$ command` string is derived from the existing `summary`
field (which for the terminal tool is the command — see `summarize_args`). No
`ToolStart` change. `full_args` remains `None` (the generic tool header uses
`summary` as the primary param, not `full_args`).

**Approach B (rejected) — add `args_json: Option<String>` to `ToolStart`.**
Would extend a second event variant's surface for data derivable from
`summary`. Rejected on Principle VIII (lean surface).

**Chosen consequence**: the terminal block's header command and the generic
tool header's primary param both come from the existing `summary` field;
`full_result` (the full output / full tool result) is the only newly-flowing
data, carried via the existing result-content path at `ToolEnd`. `full_args`
remains `None` for all tools — no args data is available at `ToolStart`, and
the generic tool header's expanded view uses `summary` for its param display.

## Consumer obligations

### `joey-tui` (state.rs `App::apply`)

- `ToolEnd { exit_code, result_preview, .. }`: in addition to the existing
  status/duration/preview updates (`state.rs:540-557`), set the matched item's
  `exit_code` and `full_result` (from the result text the event now exposes).
- `Tool { is_terminal, .. }`: set `is_terminal` at `ToolStart` from
  `is_terminal_block(&name)`.

### `joey-cli` (render.rs ToolEnd arm, :636)

- Read `exit_code`; when `Some(n)` and `n != 0`, append ` (exit N)` to the
  printed line. No interactive affordances (non-interactive parity, FR-016).

## Backward compatibility (constitution Principle VII)

- `exit_code` is additive with `None` default semantics. Existing `ToolEnd`
  struct literals fail to compile until they add the field — migration is
  forced and explicit (see research.md §1 for the full site list).
- No existing field is removed or retyped.
- Feature-005 tests constructing `ToolEnd` add `exit_code: None`; behavior
  unchanged.
- `cargo build --workspace` and `cargo test --workspace` MUST stay green.

## Regression coverage (mandated by Principle VII)

`tasks.md` MUST include:
- A test asserting a non-terminal `ToolEnd` yields `exit_code: None`.
- A test asserting a `terminal` `ToolEnd` with `{"exit_code": 0, ...}`
  yields `Some(0)`, and `{"exit_code": 2, ...}` yields `Some(2)`.
- A test asserting malformed terminal result JSON yields `None` (no panic).
- The existing feature-005 `ToolEnd` event tests still pass unchanged.
