# Contract: process Tool

**Feature**: 002-agentic-orchestration

## Tool Identity

- **Name**: `process`
- **Toolset**: `terminal`

## Description

Manage background processes started with `terminal(background=true)`. Actions:
list active processes, poll for new output, wait for completion, write to
stdin, submit input (write + Enter), and kill processes.

## Parameters Schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": ["list", "poll", "log", "wait", "kill", "write", "submit", "close"],
      "description": "The action to perform on a background process."
    },
    "session_id": {
      "type": "string",
      "description": "Process session ID (required for all actions except 'list')."
    },
    "data": {
      "type": "string",
      "description": "Data to send to stdin (for 'write' and 'submit' actions)."
    },
    "timeout": {
      "type": "integer",
      "description": "Max seconds to block for 'wait' action. Default: none (returns partial on timeout)."
    },
    "limit": {
      "type": "integer",
      "description": "Max lines to return for 'log' action. Default: 200."
    },
    "offset": {
      "type": "integer",
      "description": "Line offset for 'log' action (for pagination)."
    }
  },
  "required": ["action"]
}
```

## Action Contracts

### list

Returns all active background processes:

```
Active background processes:

[1] session_id: proc-abc | command: cargo test | running: 12s
[2] session_id: proc-def | command: npm run dev | running: 45s
```

No `session_id` required.

### poll

Returns new stdout/stderr output since the last poll:

```
[proc-abc] new output (stdout):
    Running 42 tests...
    test result: ok. 40 passed; 2 failed; 0 ignored
```

Returns empty if no new output.

### log

Returns full output (paginated). Use `offset` and `limit` for scrolling.

### wait

Blocks until the process exits or `timeout` is reached. Returns exit code
and final output.

### kill

Sends SIGTERM to the process. Returns confirmation. Cleans up the
ProcessSession from the registry.

### write

Sends raw data to stdin (no newline appended).

### submit

Sends data + Enter to stdin (for answering prompts).

### close

Closes stdin (sends EOF). Useful for triggering processing of buffered input.

## Integration with terminal Tool

When `terminal(background=true)` is used:
1. The terminal tool spawns the process via `tokio::process::Command`
2. Output is captured to a `RingBuffer` (fixed capacity)
3. A `ProcessSession` is registered in the global `ProcessRegistry`
4. The terminal tool returns the session_id immediately (non-blocking)
5. The `process` tool manages the lifecycle via the session_id

## Constraints

- Non-blocking: `list`, `poll`, `write`, `submit`, `close`, `kill` return
  in <100ms (SC-006)
- `wait` is the only potentially-blocking action; it respects the timeout
- Ring buffer capacity: 256KB stdout + 256KB stderr (configurable)
- Process cleanup: on kill, the process is reaped and the session is removed
