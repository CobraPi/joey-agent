# Feature Specification: Concurrent Agent Terminal Performance & UI Responsiveness

**Feature Branch**: `018-please-fully-implement`

**Created**: 2026-08-24

**Status**: Draft

**Input**: User description: "Fully implement features that increase tool-calling and terminal performance across multiple concurrently running agents: eliminate intermittent UI freezing caused by blocking/synchronous subprocess calls, ensure terminal calls yield execution back to the runtime rather than blocking the OS thread, cap concurrent terminal processes with queueing, decouple UI from agent execution, and deliver streamed/coalesced output updates."

## Clarifications

### Session 2026-08-24

- Q: Which subprocesses consume capped execution slots? → A: Agent-initiated terminal command executions only (terminal tool calls by the main agent, subagents, and background tasks); MCP servers, browser processes, and auxiliary interface operations do not consume slots.
- Q: How should terminal execution state (active vs queued counts) be surfaced? → A: On-demand inspection plus an automatic, unobtrusive indicator that appears only while requests are queued (under contention).
- Q: What should the default value of the terminal concurrency cap be? → A: Auto-sized from the machine's CPU core count, clamped between 4 and 16; users may override with a fixed value.
- Q: What admission policy should the queue use when requests from different agents are waiting? → A: Per-agent round-robin admission; waiting agents take turns so each agent's next request runs within one admission cycle.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Responsive interface under concurrent agent load (Priority: P1)

As a user running several agents at once (main agent plus delegated subagents and background tasks, each issuing terminal commands and other tool calls), I want the command-line interface and terminal UI to stay responsive at all times, so that I can keep typing, scrolling, reviewing output, and issuing interrupts while the agents work.

**Why this priority**: The intermittent UI freeze is the primary reported pain; an interface that stalls behind process waits blocks every other capability, so responsiveness must be fixed first.

**Independent Test**: With multiple agents executing terminal commands continuously, automated and manual checks confirm input handling and rendering never stall behind a process wait; interrupt keys take effect promptly; auxiliary actions reachable from the interface (clipboard copy/paste, clipboard-image paste, and session-manager control) do not freeze the display.

**Acceptance Scenarios**:

1. **Given** multiple agents executing terminal commands continuously, **When** the user types, scrolls, and reviews output, **Then** input handling and rendering never stall behind a process wait.
2. **Given** agents issuing terminal commands, **When** the user presses an interrupt key or invokes an auxiliary action (clipboard copy/paste, clipboard-image paste, and session-manager control), **Then** the interrupt takes effect promptly and the action completes without freezing the display.

---

### User Story 2 - Bounded, fair terminal fan-out across agents (Priority: P1)

As a user who delegates work to many subagents that each spawn terminal processes, I want the total number of terminal processes running at any moment capped at a configurable limit, with excess requests queued rather than executed all at once, so that my machine remains stable (no lag spikes, process-table or file-descriptor exhaustion) and every agent still makes progress.

**Why this priority**: Unbounded subprocess fan-out during concurrent delegation destabilizes the whole machine; without a cap with fair queueing, running many agents is unsafe, so this is equally critical to the feature's core value.

**Independent Test**: A burst of terminal requests far exceeding the cap shows active process count never above the cap, queued requests start once capacity frees, none are silently dropped, and the user can see how many calls are active versus waiting.

**Acceptance Scenarios**:

1. **Given** a burst of terminal requests far exceeding the configured cap, **When** the requests are submitted, **Then** the active process count never rises above the cap and queued requests begin once capacity frees, with none silently dropped.
2. **Given** terminal calls active and queued across multiple agents, **When** the user inspects terminal execution state, **Then** the counts of active versus waiting calls are visible.

---

### User Story 3 - Live, coalesced progress from busy agents (Priority: P2)

As a user watching many agents produce terminal output at the same time, I want consolidated incremental output and status updates streamed to my display rather than one blocking final dump, so the interface stays smooth under load and I can follow each agent's progress.

**Why this priority**: Streamed, coalesced progress markedly improves usability under heavy multi-agent load, but the feature already delivers core value with bounded execution and complete final results, so this ranks below the two P1 stories.

**Independent Test**: Under simultaneous bursty output from several agents, the display updates incrementally at a bounded rate (no flooding), final results are complete and untruncated, and each agent's final result arrives without requiring the interface to wait on other agents.

**Acceptance Scenarios**:

1. **Given** several agents producing bursty terminal output simultaneously, **When** output flows toward the display, **Then** the display updates incrementally at a bounded rate with no flooding.
2. **Given** concurrent multi-agent terminal output, **When** calls complete, **Then** final results are complete and untruncated and each agent's final result arrives without the interface waiting on other agents.

---

### Edge Cases

- Single agent, single terminal call: latency and behavior unchanged from today (no new artificial delays or serialization for a lone agent).
- Burst far above the cap: requests queue; a queued request begins within a bounded interval after capacity frees; no starvation of any single agent.
- Interrupt/cancel of an agent or turn: its queued and running terminal calls are cancelled or terminated promptly; capacity is released; no orphaned child processes remain.
- Long-running foreground command at the per-call timeout boundary: existing configured timeout and hard maximum still apply; timeout of a call counts from the start of execution, not from time spent waiting in queue.
- One agent's terminal call fails or times out: other agents' calls are unaffected (fault isolation).
- Background tasks and interactive agents share the same global cap and queueing rules.
- Behavior is consistent across supported platforms (macOS, Linux, Windows), using platform-appropriate equivalents.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: All terminal/process execution initiated by any agent (main agent, subagent, or background task) must execute asynchronously with respect to the interface event loop; no interface rendering or input-handling path may wait synchronously on process completion.
- **FR-002**: Auxiliary operations reachable from interface paths that currently perform synchronous process execution or blocking waits (for example clipboard access, clipboard-image paste, and session-manager control commands such as tmux) must complete without blocking the interface.
- **FR-003**: The system must enforce a configurable global cap on the number of terminal processes executing concurrently across all agents in a session; requests beyond the cap must queue until capacity is available, admitted per-agent round-robin so waiting agents take turns and each agent's next request runs within one admission cycle (no agent starves). The cap applies to agent-initiated terminal command executions (terminal tool calls by the main agent, subagents, and background tasks); long-lived infrastructure processes such as MCP servers and browser processes, and the auxiliary operations of FR-002, do not consume capped slots and remain governed by FR-001/FR-002.
- **FR-004**: Queued terminal requests must never be dropped or fail solely because of the cap; they must either execute when capacity frees or be explicitly cancellable by the requesting agent or the user.
- **FR-005**: The cap must be configurable through the product's existing configuration mechanism as a new additive setting; when not explicitly set it defaults to a value auto-derived from the machine's CPU capacity, clamped between 4 and 16 concurrent terminal processes; existing configuration keys, their names, and defaults must remain valid (backward compatible). The default must not throttle a single agent working alone.
- **FR-006**: Cancelling an agent, turn, or session must promptly cancel its queued and running terminal requests, terminate the underlying processes, and release capacity without leaking child processes.
- **FR-007**: Terminal output and status for concurrent calls must be delivered incrementally as discrete events, and updates destined for a single interface must be coalesced so bursts from many agents do not flood the display; final results must remain complete.
- **FR-008**: Existing per-call terminal timeout semantics are preserved, applying to execution time of each call once admitted from the queue.
- **FR-009**: A terminal call's failure, timeout, or cancellation must not block, unfairly delay, or cancel other agents' calls.
- **FR-010**: Existing concurrent dispatch of independent read-only tool calls is preserved; concurrency governance for terminal processes must not regress tool-calling throughput for tools that consume no terminal capacity.
- **FR-011**: The user must be able to inspect current terminal execution state (active count versus queued count) on demand, and the interface must show an unobtrusive automatic indicator whenever terminal requests are queued (i.e., under contention), without adding persistent indicators to the default single-agent experience.
- **FR-012**: All requirements hold on every supported platform via platform-appropriate equivalents, and outcomes are reachable identically from both the command-line interface and the terminal UI.

### Key Entities *(include if feature involves data)*

- **Terminal Execution Request**: a single command execution asked for by an agent, carrying its timeout and a lifecycle state of queued, running, completed, failed, or cancelled.
- **Execution Slot**: a counted unit of global terminal-execution capacity consumed by an agent-initiated terminal command execution; admission to running state requires an available slot, released on completion, failure, or cancellation.
- **Execution Event**: an incremental output or status chunk emitted during execution and destined for coalesced delivery to the interface.
- **Terminal Concurrency Setting**: the additive configuration key defining the global cap; when unset, the default is auto-derived from machine CPU capacity (clamped between 4 and 16); an explicit setting overrides the auto-derived default.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: While 8 or more agents continuously issue terminal calls, user input echo and scroll response remain under 150 ms and no interface interaction stalls longer than 1 second.
- **SC-002**: During a burst of at least 50 requested terminal calls, sampled system process counts never exceed the configured cap, and every call either completes, fails with its own error, or is explicitly cancelled — none are dropped.
- **SC-003**: After a cancel request, affected running terminal processes are gone and their capacity released within 2 seconds, with zero orphaned child processes attributable to the cancelled work.
- **SC-004**: A single agent performing sequential tool calls sees no more than 5% added latency versus the current build (no regression for the lone-agent case).
- **SC-005**: Under bursty multi-agent output, interface update frequency stays within a bounded coalescing budget while final results arrive complete and untruncated.
- **SC-006**: Automated tests cover cap enforcement, queue admission and drain, cancellation cleanup, fault isolation, and an interface-responsiveness probe; the full workspace test suite remains green.

## Assumptions

- Feature 009 (per-call terminal streaming, timeouts, and single-path responsiveness) is the delivered baseline; this feature governs the multi-agent concurrent case, residual blocking paths, and aggregate output flow, and does not re-specify 009 behavior.
- The reported intermittent freezing is caused by blocking execution on runtime or interface threads (residual synchronous process calls and unbounded subprocess fan-out during concurrent delegation); the remedy is that terminal calls yield the thread rather than block it.
- Implementation guidance (non-binding): prefer native asynchronous process execution; confine any unavoidable synchronous execution to a dedicated blocking-capacity pool; keep the interface loop decoupled from agent execution via message passing; implement the cap as a counting-permit mechanism with queueing; coalesce output at the display layer. Any approach achieving the outcomes is acceptable.
- The default global cap auto-sizes from the machine's CPU capacity, clamped between 4 and 16 concurrent terminal processes, and is user-tunable to a fixed value.
- Existing provider-request and delegation concurrency limits remain unchanged and separate; the new cap governs terminal subprocess execution specifically.
- No on-disk session or state format changes; new configuration is additive with defaults, per project backward-compatibility rules.
