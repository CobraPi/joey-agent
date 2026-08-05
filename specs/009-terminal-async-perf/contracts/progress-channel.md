# Contract: Progress Channel

**Feature**: 009-terminal-async-perf
**Date**: 2026-07-30

## Purpose

Defines the additive API added to `joey-tools::ToolContext` that allows tools
to emit `ToolProgress` events during execution, without changing the `Tool`
trait.

## API

### `ToolContext::with_progress_sender`

```rust
impl ToolContext {
    /// Set an optional progress sender. When set, tools that support
    /// streaming (e.g. `terminal`) will emit incremental progress events
    /// through this sender during execution. When `None` (the default),
    /// tools run as before with no progress events.
    ///
    /// This is an additive, backward-compatible method — existing callers
    /// that don't call it see no behavior change.
    pub fn with_progress_sender(
        self,
        sender: Option<tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self;
}
```

### `ToolContext::progress_sender`

```rust
impl ToolContext {
    /// Returns the progress sender, if one was set.
    /// Tools call this to check whether streaming is available:
    ///
    ///   if let Some(tx) = ctx.progress_sender() {
    ///       let _ = tx.send(progress_text);
    ///   }
    pub fn progress_sender(&self) -> Option<&tokio::sync::mpsc::UnboundedSender<String>>;
}
```

## Backward Compatibility

- **Existing callers**: `ToolContext::new(...)` still works without calling
  `with_progress_sender`. The sender defaults to `None`. No existing code
  changes required.
- **Tool trait**: `async fn execute(&self, args: Value, ctx: &ToolContext)`
  is unchanged. Tools access the channel through `ctx`, not through a new
  parameter.
- **Clone semantics**: `ToolContext` is `Clone` (via `Arc<ContextInner>`).
  `UnboundedSender<String>` is `Clone`, so it fits in the `Arc` with no issue.

## Agent-Side Wiring

The agent loop (`joey-agent-core::agent.rs::execute_tool_calls`) is
responsible for:

1. Creating a progress sub-channel (or reusing the main event channel's
   sender, wrapping `String` → `AgentEvent::ToolProgress`).
2. Setting it on the `ToolContext` before dispatching a tool call.
3. The sender carries the tool name alongside the progress text (the agent
   knows which tool it dispatched; it attaches the name when forwarding to
   the main event channel).

## Failure Semantics

- If the receiver is dropped (turn ended, channel closed), `tx.send(...)`
  returns `Err`. Tools MUST ignore this silently (the `let _ =` idiom) —
  a failed progress send must never fail the tool execution.
- If the sender is `None` (no channel set), tools skip progress emission
  entirely. This is the backward-compatible default.
