# Contract: Terminal Streaming Behavior

**Feature**: 009-terminal-async-perf
**Date**: 2026-07-30

## Purpose

Defines the observable behavior contract of the `terminal` tool after this
feature: what events are emitted during execution, the output-capture
strategy, and the backward-compatible result format.

## Event Sequence (Foreground)

```
AgentEvent::ToolStart { name: "terminal", ... }
  AgentEvent::ToolProgress { name: "terminal", progress: "<chunk>" }    ← repeated
  AgentEvent::ToolProgress { name: "terminal", progress: "running… 12s" }  ← if silent > 2s
AgentEvent::ToolEnd { name: "terminal", full_result: "{...JSON...}", exit_code: N }
```

### ToolProgress content

- **Output delta**: decoded UTF-8 text (lossy) from the merged stdout/stderr
  pipe. Raw bytes that don't decode as UTF-8 are replaced with U+FFFD. Each
  delta is a coalesced chunk (up to 64 KB of text per event, though typically
  much smaller — line-buffered where practical).
- **Elapsed-time indicator**: the literal string `"running… Ns"` where N is
  the elapsed seconds since `ToolStart`. Emitted only when no output chunk
  has arrived for ≥ 2 seconds. Not emitted for commands that complete in
  under 2 seconds.

### Throttling / coalescing

- If output arrives faster than events can be processed (flood), consecutive
  chunks within a 50 ms window are coalesced into a single `ToolProgress`
  event to avoid flooding the event channel.
- The event channel is unbounded (no backpressure stall), but coalescing
  keeps the event count reasonable.

## Output Capture Strategy

| Output size | Strategy | Memory bound |
|-------------|----------|--------------|
| < 4 KB | In-memory `String` (no temp file) | ≤ 4 KB |
| ≥ 4 KB | Temp file on disk (`tempfile::NamedTempFile`) | ≤ 64 KB chunk buffer |

On completion, the full output is read from the source (in-memory string or
temp file). The existing post-processing pipeline runs on the full output:
CWD marker extraction → truncation → ANSI stripping → secret redaction →
exit-code interpretation.

## Result Format (Unchanged — Backward Compatible)

The final `ToolResult` JSON is identical to today:

```json
{
    "output": "<full processed output>",
    "exit_code": 0,
    "error": null,
    "exit_code_meaning": "No matches found (not an error)"
}
```

Fields:
- `output`: full processed output (same as pre-feature — temp-file readback
  is transparent to the result format).
- `exit_code`: integer (0 = success, non-zero = command exit code, negative
  = signal, 124 = timeout, -1 = spawn failure).
- `error`: `null` on success, error string on failure/timeout.
- `exit_code_meaning`: optional human-readable note for known non-zero codes.

No new fields are added to the result. No existing fields change semantics.
This guarantees FR-009 (backward compatibility).

## Background Process Completion Event

When a background job with `notify_on_complete=true` finishes, the reaper
emits an event through the session's event channel. The event carries:

- Session ID
- Exit code
- Bounded tail of output (from ring buffer, truncated for display)
- Elapsed duration

This is surfaced as an `AgentEvent` variant (e.g. `Notice` or a new
`BackgroundComplete` variant — decision deferred to implementation, but the
observable contract is: the user sees a visual notification and the agent
receives the result for its next turn).

## Timeout Behavior (Unchanged)

- Default: 180s (config: `terminal.timeout`, env: `TERMINAL_TIMEOUT`).
- Max: 600s (env: `TERMINAL_MAX_FOREGROUND_TIMEOUT`).
- On timeout: partial output captured so far is preserved (read from temp
  file / in-memory string), `[Command timed out after Ns]` appended, exit
  code 124, child killed.
