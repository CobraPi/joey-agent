# Data Model: Agentic Orchestration Engine

**Feature**: 002-agentic-orchestration
**Date**: 2026-07-23

## Entities

### DelegationRequest

A request from the parent agent (or user) to dispatch one or more subagents.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| goal | String | Yes (single mode) | The task goal for the subagent |
| context | String | No | Additional context passed to the subagent |
| tasks | Vec<TaskSpec> | No | If present, triggers batch mode (parallel dispatch) |
| model | Option<String> | No | Override model for subagent(s) |
| reasoning | Option<ReasoningEffort> | No | Override reasoning level |
| toolsets | Vec<String> | No | Restrict subagent toolset (default: all enabled) |
| max_turns | Option<usize> | No | Override iteration budget (default: 50) |
| persist | bool | No | Persist subagent trace to session DB (default: false) |
| role | SubagentRole | No | Leaf (default) or Orchestrator |

### TaskSpec (batch mode)

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| goal | String | Yes | Task-specific goal |
| context | String | No | Task-specific context |
| model | Option<String> | No | Per-task model override |
| toolsets | Vec<String> | No | Per-task toolset restriction |

### SubagentRole

```rust
enum SubagentRole {
    Leaf,         // Cannot delegate further (default, max_spawn_depth=1)
    Orchestrator, // Can delegate to leaves (requires max_spawn_depth>1)
}
```

### DelegationResult

The outcome of a completed subagent execution.

| Field | Type | Description |
|-------|------|-------------|
| goal | String | The original goal (for correlation) |
| summary | String | Concise result summary (<500 tokens target) |
| success | bool | Whether the subagent completed without fatal error |
| error | Option<String> | Error detail if success=false |
| token_usage | Usage | Total tokens consumed by this subagent |
| wall_clock | Duration | Total wall-clock execution time |
| model | String | Model that was used |
| iterations | usize | Number of API calls made |
| persisted_session_id | Option<String> | If persist=true, the child session ID |

### SubagentManager

The orchestrator that owns the concurrency limiter and dispatches batches.

| Field | Type | Description |
|-------|------|-------------|
| config | ManagerConfig | Concurrency limits, depth, defaults |
| semaphore | Arc<Semaphore> | Shared in-flight request limiter |
| parent_ctx | ToolContext | Parent's context (for config resolution) |
| event_tx | UnboundedSender<AgentEvent> | Event channel for lifecycle events |
| depth | usize | Current delegation depth (0 = top-level parent) |

### ManagerConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| max_concurrent_children | usize | 3 | Max parallel subagents per batch |
| max_concurrent_requests | usize | max_children + 2 | Semaphore permits (in-flight provider calls) |
| max_spawn_depth | usize | 1 | Max nesting depth (1 = flat) |
| default_max_turns | usize | 50 | Default iteration budget per child |
| default_persist | bool | false | Default trace persistence |

### ProcessSession

A managed background process.

| Field | Type | Description |
|-------|------|-------------|
| session_id | String | Unique handle identifier |
| child | tokio::process::Child | The spawned process |
| stdout_buf | RingBuffer<u8> | Captured stdout (capped) |
| stderr_buf | RingBuffer<u8> | Captured stderr (capped) |
| stdin | Option<ChildStdin> | stdin handle for write actions |
| command | String | Original command string |
| cwd | PathBuf | Working directory |
| started_at | Instant | Spawn timestamp |
| notify_on_complete | bool | Whether to emit Done event |
| watch_patterns | Vec<String> | Patterns to watch in output |

### RingBuffer<T>

A fixed-capacity ring buffer for process output capture.

| Field | Type | Description |
|-------|------|-------------|
| buf | VecDeque<T> | Underlying deque |
| capacity | usize | Max elements before oldest is evicted |
| truncated | bool | Whether data was dropped from the head |

## Existing Entities Reused (No Changes)

### SessionDb (joey-core::state)

Already provides:
- `search(query, limit) -> Vec<SearchHit>` — FTS5 search
- `messages(session_id) -> Vec<StoredMessage>` — full message retrieval
- `async_delegations` table — persists delegation metadata
- `create_session()` — creates new session rows for persistent subagents

### AgentEvent (joey-agent-core::events)

Extended with new variants (additive, non-breaking):

```rust
// New variants added to the existing AgentEvent enum:
SubagentSpawn {
    goal: String,
    model: String,
    toolset_summary: String,
    depth: usize,
},
SubagentComplete {
    goal: String,
    success: bool,
    summary_preview: String,
    token_usage: Usage,
    duration_secs: f64,
},
SubagentFailed {
    goal: String,
    error: String,
    duration_secs: f64,
},
DelegationBatchComplete {
    total: usize,
    succeeded: usize,
    failed: usize,
    total_duration_secs: f64,
},
```

## State Transitions

### Subagent Lifecycle

```
DelegationRequest
    │
    ▼
[SubagentSpawn event emitted]
    │
    ▼
Subagent::new() ──► Agent::run_turn(goal, event_tx)
    │                         │
    │                    ┌────┴────┐
    │                    ▼         ▼
    │              Natural stop   Budget exhausted
    │                    │         │
    │                    └────┬────┘
    │                         ▼
    │              Final summary turn
    │                    (if budget exhausted)
    │                         │
    ▼                         ▼
[SubagentComplete or SubagentFailed event]
    │
    ▼
DelegationResult returned to parent
    │
    ▼ (if persist=true)
Optional: child session persisted to SessionDb
```

### Background Process Lifecycle

```
process(action="start") or terminal(background=true)
    │
    ▼
tokio::process::Command::spawn()
    │
    ▼
ProcessSession registered in ProcessRegistry
    │
    ▼ (handle returned immediately)
    │
    ├──► process(action="poll") ──► returns new stdout/stderr since last poll
    ├──► process(action="wait")  ──► blocks until exit or timeout
    ├──► process(action="write") ──► writes to stdin
    ├──► process(action="kill")  ──► sends SIGTERM, reaps
    └──► process(action="close") ──► closes stdin (EOF)
```

## Validation Rules

- **Concurrency limit**: If `tasks.len() > max_concurrent_children`, the
  excess tasks are queued (JoinSet processes as permits become available,
  not rejected).
- **Depth limit**: If `depth >= max_spawn_depth` and role=Orchestrator,
  the delegate_task tool is not registered for that subagent.
- **Model resolution**: Per-subagent model override > config default
  (`delegation.default_model`) > parent's model.
- **Toolset restriction**: If `toolsets` is empty, the subagent gets all
  enabled tools. If specified, only those toolsets' tools are registered.
  delegate_task is always excluded for Leaf role subagents.
- **Summary token budget**: Target <500 tokens. The summary prompt includes
  an explicit "Keep your summary under 500 tokens" instruction.
