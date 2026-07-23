# Contract: delegate_task Tool

**Feature**: 002-agentic-orchestration

## Tool Identity

- **Name**: `delegate_task`
- **Toolset**: `delegation`
- **Emoji**: `🤖`

## Description

Spawn one or more subagents to work on tasks in isolated contexts. Each
subagent gets its own conversation history, toolset, and execution budget.
The parent receives only a concise summary from each child. By default,
subagent traces are ephemeral (discarded after summary); set `persist=true`
to store the child session for later session_search recall.

## Parameters Schema

```json
{
  "type": "object",
  "properties": {
    "goal": {
      "type": "string",
      "description": "The task goal for the subagent. Required for single-task mode."
    },
    "context": {
      "type": "string",
      "description": "Additional context to pass to the subagent. Include file paths, error messages, project structure, constraints. The subagent knows nothing about the parent conversation."
    },
    "tasks": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "goal": { "type": "string" },
          "context": { "type": "string" },
          "model": { "type": "string" },
          "toolsets": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["goal"]
      },
      "description": "Batch mode: array of task specs for parallel dispatch. Each runs concurrently and independently. If provided, 'goal' is ignored."
    },
    "model": {
      "type": "string",
      "description": "Override model for the subagent(s). If omitted, uses delegation.default_model from config or the parent's model."
    },
    "toolsets": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Restrict the subagent's available tools to these toolsets. If omitted, all enabled tools are available (minus delegate_task for leaf role)."
    },
    "persist": {
      "type": "boolean",
      "description": "If true, persist the subagent's full session trace to the session store for later session_search recall. Default: false (ephemeral).",
      "default": false
    }
  }
}
```

## Return Contract

### Single-task mode (goal provided)

Returns the subagent's summary as text. On failure, returns an error
envelope: `{"error": "Subagent failed: <detail>"}`.

### Batch mode (tasks provided)

Returns a structured text block:

```
[1/3] goal: "Fix tests in crate A"
      status: success
      summary: <summary text>
      tokens: 4521 | duration: 12.3s

[2/3] goal: "Fix tests in crate B"
      status: success
      summary: <summary text>
      tokens: 3210 | duration: 8.1s

[3/3] goal: "Update docs"
      status: failed
      error: <error detail>
      tokens: 890 | duration: 3.2s
```

## Events Emitted

- `SubagentSpawn` — when each subagent starts (goal, model, toolset)
- `SubagentComplete` — when each subagent finishes (summary, tokens, duration)
- `SubagentFailed` — when a subagent errors out (error, duration)
- `DelegationBatchComplete` — when all batch members resolve (counts, duration)

## Configuration Keys

| Key | Default | Description |
|-----|---------|-------------|
| `delegation.max_concurrent_children` | 3 | Max parallel subagents per batch |
| `delegation.max_concurrent_requests` | 5 | Semaphore permits across parent + children |
| `delegation.max_spawn_depth` | 1 | Nesting depth (1 = flat) |
| `delegation.default_max_turns` | 50 | Default iteration budget per child |
| `delegation.default_model` | (none) | Default model for subagents |

## Constraints

- Leaf subagents (default) cannot call delegate_task.
- Orchestrator subagents require `max_spawn_depth > 1`.
- Subagent summary target: <500 tokens.
- The concurrency limiter queues excess tasks (does not reject).
