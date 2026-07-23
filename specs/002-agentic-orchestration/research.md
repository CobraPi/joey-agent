# Research: Agentic Orchestration Engine

**Feature**: 002-agentic-orchestration
**Date**: 2026-07-23

## Phase 0 Research Summary

Research was conducted by analyzing the existing joey-agent codebase
(the port of Hermes Agent), the Hermes Agent documentation and source, and
the Crush (charmbracelet/crush) architecture as described in its public
GitHub issues and documentation.

---

## R1: Subagent Isolation Model

**Decision**: Each subagent is a full `Agent` instance constructed with a
fresh `ToolContext` (new session_id, new SessionState), a restricted
`ToolRegistry` (filtered by the requested toolset), and a separate
`history: Vec<Message>` that starts empty. The subagent runs its own turn
loop via the existing `Agent::run_turn` method. The parent never sees the
child's history.

**Rationale**:
- The existing `Agent::new()` already accepts `AgentConfig`, `ToolRegistry`,
  and `ToolContext` as parameters — constructing a subagent is literally
  `Agent::new(child_config, child_registry, child_ctx)`. No architectural
  change to the Agent struct is needed.
- `ToolContext::new()` creates independent `SessionState` (read trackers,
  dedup maps, terminal cwd) per call. Cloning the context would share state;
  constructing fresh isolates it.
- The `Transport` trait abstraction in agent.rs means subagents can be tested
  with mocked providers without live API calls.

**Alternatives considered**:
1. *Thread-based isolation (separate OS processes)*: Rejected — unnecessary
   complexity. Rust's ownership model + tokio tasks provide sufficient
   isolation. Each Agent owns its history Vec; there's no shared mutable
   state between parent and child.
2. *Clone parent context with filtered history*: Rejected — risks leaking
   parent state. A fresh context is simpler and provably isolated.

---

## R2: Parallel Batch Dispatch

**Decision**: Use `tokio::task::JoinSet` to spawn N subagent tasks
concurrently. Each task is `async move { agent.run_turn(goal, tx).await }`.
The `JoinSet` collects results as they complete; one failure does not cancel
others. Results are collected into `Vec<DelegationResult>` in completion
order, then returned to the parent.

**Rationale**:
- `JoinSet` is the idiomatic tokio primitive for concurrent task management.
  It's already a transitive dependency (tokio "full" features).
- JoinSet does not cancel sibling tasks on individual failure — exactly the
  batch-resilience semantics required by FR-005/SC-003.
- The existing `Agent::run_turn` takes `mpsc::UnboundedSender<AgentEvent>`,
  so each subagent can emit lifecycle events through the parent's event
  channel (FR-019).

**Alternatives considered**:
1. *Sequential execution*: Rejected — defeats the purpose. SC-001 requires
   parallel wall-clock advantage.
2. *tokio::select! with manual task spawning*: More error-prone than
   JoinSet; JoinSet handles the join-all-collect pattern cleanly.
3. *rayon (CPU parallelism)*: Rejected — subagents are I/O-bound (provider
  API calls), not CPU-bound. tokio is the correct runtime.

---

## R3: Shared Concurrency Limiter

**Decision**: A `tokio::sync::Semaphore` shared across the parent agent and
all active subagents via `Arc<Semaphore>`. Before any provider call, the
caller acquires a permit (`.acquire_owned().await`), holds it through the
call, and drops it on completion. Configurable via
`delegation.max_concurrent_requests` (default: derived from
`delegation.max_concurrent_children` + 2 to allow parent overlap).

**Rationale**:
- tokio::sync::Semaphore is the standard async semaphore — permit-based,
  fair, zero-allocation when uncontended. It's in the tokio "sync" feature
  already enabled by "full".
- Permit-based throttling is strictly better than retry-based: it prevents
  the 429 from happening in the first place, rather than reacting to it.
- The existing per-call retry/backoff in `call_with_retries` stays as-is
  underneath — it handles residual 429s from other sources (shared API keys,
  rate limits from concurrent users).

**Alternatives considered**:
1. *Token-bucket rate limiter (governor crate)*: Rejected — adds a new
   dependency (Constitution Principle VIII). The semaphore is sufficient for
   the "cap in-flight requests" use case; a per-minute token bucket adds
   complexity without proportional benefit for the typical 3-concurrent-child
   workload.
2. *Per-subagent independent retry*: Rejected — thundering herd risk.
   N children hitting the provider simultaneously multiply the request rate
   with no coordination.

**Integration point**: The semaphore is injected into the
`SubagentManager`, which passes `Semaphore::clone()` to each subagent's
execution context. The subagent's `call_with_retries` path acquires a permit
before `transport_call`. This requires a minimal hook in the Agent: either
a `provider_permit: Option<Arc<Semaphore>>` field checked before
`transport_call`, or a wrapper around `Transport` that acquires the permit.
The wrapper approach is preferred (no change to Agent internals).

---

## R4: Subagent Summary Generation

**Decision**: When the subagent's turn loop ends (either naturally via
`finish_reason: stop` or via iteration-budget exhaustion), one final
provider call is made with the existing `MAX_ITERATIONS_SUMMARY_REQUEST`
prompt (already defined in agent.rs:57-60) and tools stripped. The response
text is the summary returned to the parent.

**Rationale**:
- The iteration-budget summary mechanism already exists in agent.rs
  (`MAX_ITERATIONS_SUMMARY_REQUEST`). For natural completion, the agent's
  final assistant message IS the summary. For budget exhaustion, the existing
  finalizer prompt fires. Both reuse the existing provider call path.
- No separate summarizer model or dependency (Constitution Principle VIII).
  The subagent's own model — which has full context of what it did — produces
  the highest-quality summary.
- Hermes uses the same approach: the child agent's final response is the
  summary returned to the parent.
- Crush's SessionAgent returns its last assistant message.

**Alternatives considered**:
1. *Dedicated cheap-model summarizer call*: Adds latency (extra round-trip)
   and cost. The child's own model already has the context; no need for a
   second model to read the trace.
2. *Heuristic extraction (last N messages concatenated)*: Loses quality —
   the last messages may be tool results, not a coherent summary.

---

## R5: Session History Search Implementation

**Decision**: Wrap the existing `SessionDb::search()` method in a tool.
`SessionDb::search()` already implements FTS5 full-text search with snippet
extraction and ranking. The tool exposes `query` (string), `limit` (int),
and optionally `session_id` + `around_message_id` for message-window
scrolling. Results are formatted as structured text with session_id,
message_id, role, and snippet.

**Rationale**:
- The FTS5 infrastructure is already built and tested. The `messages_fts`
  virtual table, `sanitize_fts5_query`, and `SearchHit` struct all exist.
  The tool is a thin wrapper — no new storage or indexing logic.
- Hermes's `session_search` tool follows the same pattern: it's a surface
  over the existing session DB FTS5.
- For message-window scrolling (FR-010), `SessionDb::messages()` already
  returns all messages for a session; the tool paginates a window around a
  given message_id.

**Alternatives considered**:
1. *Embedding-based vector search*: Massive overkill for the initial
   implementation. Requires a new embedding model dependency and a vector
   store. FTS5 is already fast (SC-005: <1s for 100+ sessions) and requires
   zero new dependencies.
2. *External search index (Meilisearch, etc.)*: Rejected — violates
   Constitution Principle VIII (new dependency, external service).

---

## R6: Background Process Management

**Decision**: The existing terminal tool already defines the full parameter
schema for background processes (`background`, `notify_on_complete`,
`watch_patterns`, `pty`) but returns "not supported" stubs. The implementation
activates these stubs by adding a process session registry
(`HashMap<SessionId, ProcessSession>`) that manages spawned child processes.
On `background=true`, the command is spawned via `tokio::process::Command`
(asynchronous child process), output is captured to a ring buffer, and a
session handle is returned. The `process` tool (separate tool, already
referenced in toolsets.rs) provides `poll`/`wait`/`kill`/`write`/`submit`/
`close` actions on the handle.

**Rationale**:
- The terminal tool already parses and validates `background`, `pty`,
  `notify_on_complete`, and `watch_patterns` parameters — it just returns
  an error stub. Activating the feature means implementing the actual process
  management, not redesigning the tool interface.
- `tokio::process::Command` is the async-native process spawner, already
  available via tokio "full" features. It integrates with the tokio runtime
  for non-blocking I/O (SC-006).
- A ring buffer (fixed-capacity `VecDeque<u8>`) per process prevents
  unbounded memory growth while preserving recent output for polling. The
  edge case for buffer overflow is already documented in the spec (truncation
  notice + paginated reads).

**Alternatives considered**:
1. *portable-pty (already a workspace dep) for PTY-based process management*:
   Used only for `pty=true` mode (interactive CLI tools). For non-PTY
   background processes, `tokio::process::Command` is simpler and sufficient.
   The PTY path uses portable-pty for pseudo-terminal allocation.
2. *Separate process crate*: Rejected — the process tool is tightly coupled
   to the terminal tool (shared command parsing, cwd resolution, secret
   stripping). Keeping it in joey-tools alongside the terminal tool avoids
   circular dependencies.

---

## R7: Structured Orchestration Events

**Decision**: Extend the existing `AgentEvent` enum with new variants for
subagent lifecycle events. The variants are additive (new enum arms —
non-breaking for existing match arms that use `_`). Events carry goal,
model, toolset, token usage, and wall-clock duration. They are emitted
through the same `mpsc::UnboundedSender<AgentEvent>` channel the turn loop
already uses.

**Rationale**:
- `AgentEvent` is already an enum with 20+ variants. Adding new arms is
  backward-compatible in Rust (existing wildcard matches still compile).
- The mpsc channel is already wired to the CLI/TUI rendering layer — new
  events flow through automatically with zero plumbing changes.
- No new dependency (Constitution Principle VIII). No OpenTelemetry or
  distributed tracing crate.

**Alternatives considered**:
1. *tracing::info! structured logs*: Insufficient for benchmark analysis —
   logs require parsing, lack structured typing, and aren't consumable by
   the TUI in real-time.
2. *OpenTelemetry spans with trace propagation*: Adds otel/trace dependencies.
   Overkill for the current scope. The structured event channel gives
   exactly the data needed for SC-009.

---

## R8: Delegation Tool Schema (delegate_task)

**Decision**: The `delegate_task` tool accepts `goal` (string, required),
`context` (string, optional), `tasks` (array of goal objects, optional —
triggers batch mode), `model` (string, optional), `toolsets` (array,
optional), `persist` (bool, optional, default false). It returns a summary
(for single) or an array of summaries (for batch).

**Rationale**:
- Modeled directly on Hermes's `delegate_task` tool schema, which is
  battle-tested and well-understood by models. The model-facing API is:
  single task → `goal` + `context`; batch → `tasks` array.
- The `persist` flag implements FR-017 (configurable ephemeral/durable).
- `model` and `toolsets` implement FR-006/FR-007 (per-subagent selection).

**Alternatives considered**:
1. *Separate `delegate_batch` tool*: Rejected — increases surface area. The
  `tasks` array parameter cleanly handles both single and batch within one
  tool schema.
