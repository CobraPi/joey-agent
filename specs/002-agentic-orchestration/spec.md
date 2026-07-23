---
description: "Feature specification for agentic orchestration layer"
---

# Feature Specification: Agentic Orchestration Engine

**Feature Branch**: `002-agentic-orchestration`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "Add all the backend agentic orchestration methods implemented in both hermes-agent and crush into this Rust agent. Goal: most performant, most capable agent — focus on agent performance, not fancy features. Outperform both hermes and crush on agentic benchmarks."

## Clarifications

### Session 2026-07-23

- Q: How should the 6 user stories be delivered — one monolithic increment or phased? → A: Single plan, phased delivery: P1-P3 (delegation core: parallel dispatch, isolation, model selection) as the first shippable increment, then P4-P6 (session search, clarify, background processes) as the second increment. Each increment must build and pass tests independently (Constitution Principle V).
- Q: Should subagent session traces be persisted or ephemeral? → A: Configurable: default ephemeral (trace discarded after summary returned), with an opt-in per-delegation flag that persists the subagent's full session to the store for later session_search recall.
- Q: How should concurrent provider rate limits be handled across a parallel delegation batch? → A: Shared adaptive concurrency limiter (semaphore) capping in-flight provider requests across the parent and all active subagents combined. Existing per-call retry/backoff remains as a second line of defense. Configurable via `delegation.max_concurrent_requests`.
- Q: How deep should the orchestration observability layer go? → A: Structured events via the existing AgentEvent channel — spawn/complete/fail/timing/per-subagent-token-usage emitted as agent events. No new dependency; consumable by TUI, CLI, test harness, and benchmark analysis.
- Q: How should the subagent summary be generated when it completes? → A: The subagent's own model produces the summary as its final turn — when the turn loop ends (naturally or via iteration-budget exhaustion), one final prompt asks it to summarize its work for the parent. Reuses existing provider infrastructure, no separate summarizer call or dependency.

## User Scenarios & Testing

### User Story 1 - Parallel Task Delegation (Priority: P1)

A user asks the agent to solve a complex task that naturally decomposes
into independent subtasks (e.g., "fix the failing tests in crates A, B,
and C, then update the docs"). The agent MUST be able to spawn multiple
isolated subagents in parallel, each working on one subtask with its own
context window, toolset, and execution budget, and then collect their
results back into the parent conversation. No subagent sees another's
history; the parent receives only the final summary from each child.

**Why this priority**: Parallel delegation is the single highest-leverage
capability for agentic benchmark performance. It directly reduces wall-clock
time on decomposable tasks, prevents context-window pollution from
intermediate tool results, and enables the agent to attack independent
sub-problems simultaneously instead of serially. Both Hermes and Crush
implement this as their core orchestration primitive — without it, the
agent is strictly weaker than both on any multi-part task.

**Independent Test**: Given three independent code-modification tasks in
separate files, the agent spawns three parallel subagents, each completes
its task, and the parent receives three summaries and reports completion.
Wall-clock time is closer to the slowest single subtask than the sum of
all three.

**Acceptance Scenarios**:

1. **Given** a task decomposable into N independent subtasks, **When** the
   agent decides to delegate, **Then** it spawns up to M concurrent
   subagents (M configurable, default 3) and each subagent receives only
   its specific goal and context — no sibling history, no parent history
   beyond what is explicitly passed.
2. **Given** a subagent completes its task, **When** it returns, **Then**
   the parent receives a concise summary (not raw tool output) and can
   incorporate it into its own reasoning.
3. **Given** one subagent in a parallel batch fails or errors out,
   **When** the batch resolves, **Then** the remaining subagents' results
   are still delivered and the parent is informed of the failure — one
   child's failure does not abort the batch.
4. **Given** the parent is mid-turn and spawns background subagents,
   **When** the subagents finish, **Then** their results re-enter the
   parent conversation as new messages — the parent does not block-wait
   if the session supports deferred delivery.

---

### User Story 2 - Isolated Subagent Execution Context (Priority: P2)

Each subagent MUST operate in a fully isolated execution context: its own
conversation history (starting fresh), its own working directory (when
specified), its own tool registry (restricted to a configurable subset),
and its own iteration budget. The parent agent's context window is never
polluted by a subagent's intermediate tool calls, reasoning chains, or
exploratory dead-ends. Only the final result summary crosses the boundary.

**Why this priority**: Context pollution is the dominant failure mode for
long-running agentic tasks — intermediate results, failed approaches, and
noisy tool output consume context budget that the parent needs for its
own reasoning. Isolation is the architectural guarantee that makes
delegation safe to use aggressively, which is essential for benchmark
performance.

**Independent Test**: A subagent is spawned to explore a codebase (reading
20 files, running searches). The parent's context window is unaffected —
it receives only a paragraph-length summary. The parent can then continue
its own reasoning with full context budget intact.

**Acceptance Scenarios**:

1. **Given** a subagent is spawned, **When** it executes, **Then** its
   tool calls, results, and reasoning are invisible to the parent and to
   sibling subagents — only the final summary is returned.
2. **Given** a subagent is spawned with a restricted toolset, **When** it
   attempts to use a disallowed tool, **Then** the tool is not available
   in its schema and the call is rejected.
3. **Given** a subagent has an iteration budget of K turns, **When** it
   exhausts the budget, **Then** it produces a best-effort summary and
   returns, rather than running indefinitely.

---

### User Story 3 - Per-Subagent Model and Capability Selection (Priority: P3)

The agent (or the user via configuration) MUST be able to assign different
model tiers and tool capabilities to different subagents within the same
delegation batch. A computationally expensive subagent (deep code analysis)
can use a high-reasoning model, while a routine subagent (mechanical file
edits) uses a faster, cheaper model. The parent agent chooses
automatically based on task complexity, and the user can override via
configuration.

**Why this priority**: Cost-performance optimization across a delegation
batch is what allows the agent to be aggressive with parallelism without
tripling the token budget. Crush's community has specifically requested
per-subagent model selection (issue #2501), and Hermes supports model
pinning per child. For benchmark performance under token/time budgets,
this is the difference between 3 parallel subagents being affordable
versus prohibitive.

**Independent Test**: A delegation batch of 3 tasks runs with a mixed
model assignment (1 heavy, 2 light). The heavy subagent uses a
high-reasoning model, the light ones use a fast model. Total token cost
is lower than running all three on the heavy model, with equivalent task
completion quality.

**Acceptance Scenarios**:

1. **Given** a delegation batch with heterogeneous task complexity,
   **When** the agent assigns models, **Then** each subagent's model
   selection is recorded and visible in the execution trace.
2. **Given** a user-configured default model for subagents, **When** the
   agent does not explicitly override, **Then** the configured default is
   used.
3. **Given** a subagent requires tools the parent has but the subagent
   should not, **When** the subagent is spawned, **Then** only the
   specified tool subset is registered for that child.

---

### User Story 4 - Session History Search (Priority: P4)

The agent MUST be able to search its own past conversation history across
sessions to recall decisions, prior solutions, and context established in
earlier interactions. This enables the agent to avoid re-asking the user
for information already discussed and to build on prior work without
re-discovering it from scratch.

**Why this priority**: Agentic benchmarks increasingly include multi-session
or multi-turn tasks where context from earlier work is essential. Without
session search, the agent either re-explores (wasting iterations) or asks
the user to repeat themselves (failing autonomy). This is a known
capability gap — the tool is referenced in the codebase but not
implemented.

**Independent Test**: In a prior session, the agent and user established
that the project uses a specific test framework. In a new session, the
agent searches history, finds the prior decision, and applies it without
re-asking.

**Acceptance Scenarios**:

1. **Given** past sessions exist in the session store, **When** the agent
   searches with a keyword query, **Then** matching conversations are
   returned ranked by relevance, with snippets and timestamps.
2. **Given** a search returns results, **When** the agent selects one,
   **Then** it can read a window of messages around the match to recover
   full context.

---

### User Story 5 - User Clarification Protocol (Priority: P5)

The agent MUST be able to ask the user a structured question when it
encounters genuine ambiguity that blocks progress, presenting clear
options (multiple-choice or open-ended) rather than guessing silently or
halting. This is a deliberate, scoped interaction — not a
clarification-every-turn pattern — reserved for decisions where the wrong
choice has significant downstream cost.

**Why this priority**: On agentic benchmarks with autonomous scoring, the
clarification tool is primarily relevant for interactive workflows, but
its availability prevents the agent from making unrecoverable wrong
assumptions on ambiguous tasks. It also completes the toolset parity with
Hermes, which lists `clarify` as a core orchestration-adjacent tool.

**Independent Test**: Given an ambiguous task ("add authentication" with
no method specified), the agent presents a structured question with
options (OAuth2, session-based, API key) and waits for the user's choice
before proceeding.

**Acceptance Scenarios**:

1. **Given** a task with a scope-affecting ambiguity and no reasonable
   default, **When** the agent invokes clarification, **Then** the user
   receives a structured prompt with up to 4 options plus a free-text
   fallback.
2. **Given** the user responds to a clarification, **When** the agent
   receives the answer, **Then** it incorporates the answer and continues
   the task — the clarification is not re-asked.

---

### User Story 6 - Background Process Lifecycle Management (Priority: P6)

The agent MUST be able to start, monitor, and control long-running
background processes (builds, test suites, dev servers, watchers) without
blocking its own turn loop. The agent can poll process output, wait for
completion with a timeout, write to stdin, and kill processes it started.
This is distinct from the existing terminal tool's foreground execution.

**Why this priority**: Long-running processes are ubiquitous in real
agentic coding tasks — running test suites, starting dev servers,
monitoring builds. Without background process management, the agent either
blocks its entire turn waiting (wasting iteration budget) or cannot use
long-running processes at all. Both Hermes and Crush implement background
process control as a core capability.

**Independent Test**: The agent starts a dev server in the background,
receives a session handle, polls its output to confirm startup, then runs
tests against it in a separate foreground command, and finally kills the
server.

**Acceptance Scenarios**:

1. **Given** the agent starts a background process, **When** it is
   launched, **Then** a session handle is returned immediately and the
   agent's turn loop is not blocked.
2. **Given** a background process is running, **When** the agent polls
   it, **Then** new stdout/stderr output since the last poll is returned.
3. **Given** a background process, **When** the agent kills it, **Then**
   the process is terminated and its resources are cleaned up.

---

### Edge Cases

- What happens when the maximum concurrent subagent limit is reached and
  the agent attempts to spawn more? (Queue or reject with a clear error.)
- What happens when a subagent's provider call fails irrecoverably after
  all retries and fallbacks? (The subagent returns an error summary; the
  parent is informed but other children continue.)
- What happens when a subagent attempts to spawn its own subagents
  (nested delegation)? (Controlled by a configurable depth limit; default
  depth is 1 — flat, no nesting — to prevent runaway recursion.)
- What happens when a subagent exhausts its iteration budget before
  completing? (The turn loop sends a final summary prompt — the same
  mechanism used on natural completion — so the parent always receives
  a best-effort summary, never a bare truncation.)
- What happens when the session store is unavailable (corrupt, locked)?
  (Session search degrades gracefully to a "no results" response rather
  than crashing the turn.)
- What happens when a background process produces output exceeding buffer
  limits? (Output is capped with a truncation notice; full output is
  available via paginated reads.)
- What happens when the user interrupts during a parallel delegation
  batch? (All running subagents receive the interrupt signal and wind
  down cooperatively.)
- What happens when the shared concurrency limiter is saturated and a
  subagent needs a provider call? (The request waits for a semaphore
  permit before dispatching — it is queued, not rejected. The
  existing per-call timeout still applies once the permit is
  acquired.)

## Requirements

### Functional Requirements

- **FR-001**: The agent MUST be able to spawn one or more subagents,
  each with an isolated conversation history, toolset, and execution
  budget, to work on a specified goal.
- **FR-002**: The agent MUST support spawning multiple subagents in
  parallel (a "batch"), up to a configurable concurrency limit
  (default: 3), with each subagent executing concurrently and
  independently.
- **FR-003**: Each subagent MUST receive only the goal and explicit
  context passed to it — never the parent's conversation history or
  sibling subagents' state.
- **FR-004**: When a subagent completes, its final result MUST be a
  concise summary produced by the subagent's own model as its final
  turn — the turn loop sends one final prompt requesting a summary of
  its work for the parent, reusing the existing provider call
  infrastructure (no separate summarizer call or dependency). The
  summary re-enters the parent conversation, not raw tool output or
  intermediate reasoning.
- **FR-005**: If a subagent fails, the parent MUST be informed of the
  failure, but other subagents in the same batch MUST continue and their
  results MUST still be delivered.
- **FR-006**: The agent MUST be able to assign a specific model and
  reasoning level per subagent, falling back to a configurable default
  when not explicitly specified.
- **FR-007**: The agent MUST be able to restrict a subagent's toolset to
  a specified subset of available tools.
- **FR-008**: Nested delegation (a subagent spawning its own subagents)
  MUST be controlled by a configurable maximum depth (default: 1,
  meaning flat — no nesting).
- **FR-009**: The agent MUST be able to search past session history by
  keyword query and receive ranked results with snippets and timestamps.
- **FR-010**: The agent MUST be able to retrieve a window of messages
  around a specific past-session match to recover full context.
- **FR-011**: The agent MUST be able to ask the user a structured
  clarification question with up to 4 multiple-choice options plus a
  free-text fallback, and receive the user's response before continuing.
- **FR-012**: The agent MUST be able to start a process in the
  background and receive a handle without blocking its turn loop.
- **FR-013**: The agent MUST be able to poll background process output,
  wait for completion with a timeout, write to stdin, and kill a
  process by handle.
- **FR-014**: Background processes MUST support concurrent execution —
  multiple processes can run simultaneously without interfering.
- **FR-015**: A cooperative interrupt signal MUST propagate to all
  running subagents and background processes when the user requests
  interruption.
- **FR-016**: The feature MUST be delivered in two independently
  shippable increments: Increment 1 covers P1-P3 (delegation core:
  parallel dispatch, isolation, per-subagent model/capability
  selection); Increment 2 covers P4-P6 (session search, clarification,
  background process management). Each increment MUST build and pass
  `cargo test --workspace` on its own before the next begins.
- **FR-017**: Subagent traces are ephemeral by default — discarded
  after the summary is returned. A per-delegation opt-in flag MUST be
  available to persist a subagent's full session to the session store,
  making it searchable via session history search (P4). When the flag
  is not set, no child session rows are written.
- **FR-018**: A shared adaptive concurrency limiter (semaphore) MUST
  cap the number of in-flight provider requests across the parent
  agent and all active subagents combined, configurable via
  `delegation.max_concurrent_requests`. The existing per-call
  retry/backoff mechanism remains underneath as a second line of
  defense against residual 429 responses.
- **FR-019**: The orchestration layer MUST emit structured events
  through the existing AgentEvent channel for every subagent lifecycle
  transition: spawn (with goal, model, toolset), completion (with
  summary, token usage, wall-clock duration), and failure (with error
  detail). These events MUST be consumable by the TUI, CLI, test
  harness, and any benchmark analysis tooling without requiring a new
  dependency.

### Key Entities

- **Subagent**: An isolated agent instance with its own conversation
  history, tool registry, model assignment, working directory, and
  iteration budget. Created by a parent agent, returns a summary on
  completion. Traces are ephemeral by default; an opt-in flag persists
  the full session for later search recall.
- **Delegation Batch**: A collection of one or more subagent tasks
  dispatched concurrently. The batch resolves when all members complete
  (successfully or with failure).
- **Session History Record**: A stored conversation from a prior session,
  searchable by keyword, containing timestamped messages with role and
  content.
- **Background Process**: A long-running OS process started by the agent,
  identified by a session handle, producing streamable output and
  accepting stdin input.

## Success Criteria

### Measurable Outcomes

- **SC-001**: On a task decomposable into 3 independent subtasks, the
  agent completes the task via parallel delegation in wall-clock time no
  greater than 1.5x the slowest single subtask (vs. 3x for serial
  execution).
- **SC-002**: A subagent's intermediate tool calls and reasoning consume
  zero tokens from the parent agent's context window — only the final
  summary (target: under 500 tokens) crosses the boundary.
- **SC-003**: When one subagent in a 3-member batch fails, the remaining
  two subagents' results are delivered to the parent without delay or
  loss.
- **SC-004**: A delegation batch of 3 mixed-complexity subtasks uses at
  least 30% fewer total tokens than running all 3 on the heaviest model,
  with equivalent task-completion quality.
- **SC-005**: Session history search returns relevant results for a
  keyword query in under 1 second for a store of 100+ sessions.
- **SC-006**: A background process can be started, polled, and killed
  without the agent's turn loop blocking for more than 100ms on any
  lifecycle operation.
- **SC-007**: The agent's overall performance on multi-part agentic
  tasks (measured by task completion rate and wall-clock time) exceeds
  the same agent's single-agent-only baseline — delegation is a net
  positive, not overhead.
- **SC-008**: Under a parallel delegation batch of 3 subagents against
  a rate-limited provider, the shared concurrency limiter prevents
  retry storms: total provider attempts do not exceed
  `max_concurrent_requests` + the retry budget of those in-flight
  calls, and no subagent sees a hard 429 failure that the limiter
  could have avoided.
- **SC-009**: Every subagent lifecycle event (spawn, completion,
  failure) is emitted as a structured AgentEvent containing goal,
  model, toolset, token usage, and wall-clock duration — sufficient
  for a benchmark harness to compute per-subagent cost and timing
  breakdowns without inspecting logs.

## Assumptions

- Subagents use the same provider infrastructure and configuration
  resolution as the parent agent (same config file, same environment),
  differing only in model, tools, and budget as explicitly specified.
- The existing context compression engine is available to subagents and
  activates under the same thresholds — a subagent that runs long enough
  to fill its context window compresses just like the parent would.
- The existing session store (SQLite-backed) is the data source for
  session history search, using its existing schema; no separate search
  index is required for the initial implementation.
- The existing terminal tool's PTY infrastructure can be extended to
  support background process management rather than requiring a separate
  process-spawning mechanism.
- Default delegation parameters (concurrency limit, max depth, iteration
  budget) are conservative and prioritized for stability — users who
  want aggressive parallelism can raise them via configuration.
- Agentic benchmarks referenced for "outperform" comparisons include
  multi-step coding tasks (SWE-bench-style), tool-use benchmarks, and
  tasks with decomposable sub-problems where parallel delegation
  provides measurable advantage.
