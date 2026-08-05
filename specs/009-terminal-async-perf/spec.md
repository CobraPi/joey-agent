# Feature Specification: Terminal Async Performance & Streaming

**Feature Branch**: `009-terminal-async-perf`

**Created**: 2026-07-30

**Status**: Draft

**Input**: User description: "optimize this agent for performance. I want you to focus on making intensive terminal tasks run faster - right now very heavy jobs freeze the whole program."

## Clarifications

### Session 2026-07-30

- Q: For commands producing very large output, how should the final result delivered to the model be handled? → A: Stream output to a temp file on disk, then read back the full output for the model (bounded memory, but full output available to the model).
- Q: When a background job completes, what should happen? → A: Notify the user visually AND queue the result for the agent so it can act on the next turn (non-interrupting injection into the conversation).
- Q: During a long-running command that produces no output, how should the agent reassure the user it's still alive? → A: Show elapsed time (e.g. "running… 12s") that updates periodically via the existing render tick.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Live Output During Long Commands (Priority: P1)

As a developer running a long build, test suite, or other heavy command, I want
to see the command's output appear in real time as it runs, rather than staring
at a frozen screen that dumps everything at the very end. This lets me judge
whether the job is progressing, stuck, or producing errors I should act on
immediately.

**Why this priority**: This is the single most visible manifestation of the
"freeze" — a command that produces output on stdout/stderr for minutes while
the agent shows nothing. It is the difference between "the program froze" and
"the program is clearly working." Resolving it eliminates the perceived hang
for the vast majority of heavy-terminal scenarios.

**Independent Test**: Run a command that emits a line per second for 30 seconds
(e.g. `for i in $(seq 1 30); do echo "tick $i"; sleep 1; done`). Observe that
each line appears within roughly one second of being emitted, not all at once
after 30 seconds. The agent must remain visibly responsive throughout.

**Acceptance Scenarios**:

1. **Given** a foreground terminal command that streams output over 30+ seconds,
   **When** the command runs, **Then** output lines are surfaced to the user as
   they arrive (no later than a short, bounded latency — not buffered until
   process exit).
2. **Given** a command producing a large volume of output (megabytes), **When**
   it runs, **Then** incremental output is shown without requiring the entire
   output to be held in memory and delivered in one giant block at the end.
3. **Given** a long-running command, **When** output is streaming, **Then** the
   user can see the job is active and progressing (not a silent screen).
4. **Given** a streaming command that exits with a non-zero status partway,
   **When** it terminates, **Then** the final result summary still includes the
   exit code and the complete captured output (incremental display must not lose
   the canonical result the model consumes).

---

### User Story 2 - Agent Stays Responsive While a Command Runs (Priority: P2)

As a developer, when a heavy terminal command is running, I want the rest of the
program — the rendering loop, UI animation, and my ability to interrupt — to
keep working. The program should never appear frozen just because one tool call
is slow.

**Why this priority**: Beyond showing output, the program must remain
interactive during long jobs. If a build hangs the entire UI, the user cannot
even cancel it cleanly. This story covers the structural non-blocking
requirement that underpins a responsive experience.

**Independent Test**: Start a foreground command that sleeps for 60 seconds
(`sleep 60`). During those 60 seconds, confirm that (a) the UI animation/blink
keeps running, (b) pressing Ctrl-C cancels the command within a couple of
seconds, and (c) no "not responding" or dead-screen symptom occurs. The program
must feel alive for the entire duration.

**Acceptance Scenarios**:

1. **Given** a long foreground terminal command is executing, **When** the user
   observes the interface, **Then** the rendering loop continues to update (e.g.
   an active "working" indicator animates) — it does not freeze.
2. **Given** a long foreground terminal command is executing, **When** the user
   triggers an interrupt (Ctrl-C), **Then** the running command is cancelled and
   control returns to the user within a few seconds.
3. **Given** a long command is executing on a multi-turn session, **When** other
   independent work could occur, **Then** the heavy command does not indefinitely
   block unrelated program subsystems (e.g. rendering, background tickers) beyond
   the bounded duration of the command itself.

---

### User Story 3 - Background Jobs Actually Notify on Completion (Priority: P3)

As a developer who launches a job in the background (so I can keep working), I
want to be reliably notified when that background job finishes — without having
to manually poll for it. The documented "notify on complete" capability should
work as advertised.

**Why this priority**: The background + notify path is the intended escape hatch
for very heavy jobs (start them in the background, get notified, keep going).
Today the notify flag is set but never acted upon, so this path is inert.
Fixing it makes heavy jobs genuinely non-blocking from the user's workflow
perspective, complementing the foreground streaming and responsiveness fixes.

**Independent Test**: Launch a background job with completion notification
requested that runs for ~15 seconds, then immediately continue a separate
conversation turn. Confirm that when the job finishes, a completion notification
is delivered to the session automatically — without the model or user having to
issue a manual poll/wait call.

**Acceptance Scenarios**:

1. **Given** a background job launched with "notify on complete" requested,
   **When** the job finishes (success or failure), **Then** the user is visually
   notified AND the completion result is injected into the conversation so the
   agent can act on it on its next turn — without requiring a manual poll/wait
   call and without interrupting any turn currently in progress.
2. **Given** a background job that has already finished, **When** the user or
   model inspects process state, **Then** the finished status (exit code,
   truncated tail of output) is accurately reflected.
3. **Given** multiple background jobs running concurrently with notify requested,
   **When** they complete at different times, **Then** each delivers its own
   distinct completion notification (visual + conversation-injected) in the right
   order.

---

### Edge Cases

- What happens when a streaming command produces no output at all for a long
  time (e.g. a silent compile)? The UI MUST display an elapsed-time indicator
  (e.g. "running… 12s") that updates periodically, showing the job is active so
  it is not mistaken for a hang.
- What happens when output is produced faster than it can be displayed (flood)?
  Output must be coalesced/throttled so display stays responsive and memory
  stays bounded; no unbounded buffering of partial deltas.
- What happens when a foreground command is interrupted mid-stream? Partial
  output captured so far must be preserved and surfaced (to both the user and
  the model's result), and the child process must be cleaned up reliably.
- What happens when output contains very long lines or binary/control
  characters? Display must degrade safely (truncation/sanitization) without
  crashing or blocking.
- What happens when many background jobs finish simultaneously? Notifications
  must not flood or deadlock the session event channel.
- What happens when a background job's owning session/turn has already ended?
  Completion must be handled gracefully — the result must be queued and injected
  into the conversation on the next turn (non-interrupting), and the user-facing
  notification must still fire so the completion is not lost.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The agent MUST surface incremental terminal output (stdout/stderr)
  to the user while a foreground command is still running, rather than buffering
  all output until the process exits.
- **FR-002**: Incremental output MUST be delivered with bounded latency (target:
  within ~1 second of being produced under normal load), so the user sees live
  progress.
- **FR-003**: The agent MUST remain visibly responsive (rendering loop updates,
  interrupt remains functional) for the entire duration of any running terminal
  command.
- **FR-004**: The user MUST be able to interrupt (cancel) a running foreground
  command, and the cancellation MUST take effect within a few seconds, including
  reliable cleanup of the child process.
- **FR-005**: When a command finishes, the agent MUST still deliver a complete
  final result (exit code + full captured output) to the model, even though
  output was shown incrementally — incremental display MUST NOT replace or
  truncate the canonical result. Output MUST be streamed to a temporary file on
  disk during execution and read back for the final result, so the complete
  output is available to the model regardless of size while memory stays bounded.
- **FR-006**: For commands producing large output volumes, the agent MUST bound
  its in-memory usage by writing output to disk (temp file) as it streams, rather
  than holding the full output in memory (it MUST NOT hold unbounded duplicate
  buffers of streaming output indefinitely). The temp file MUST be cleaned up
  after the final result has been read back.
- **FR-007**: Background jobs launched with completion-notification requested
  MUST deliver an automatic completion event when they finish, without requiring
  a manual poll or wait call. The notification MUST both (a) visually notify the
  user and (b) non-interruptingly inject the result into the conversation so the
  agent can act on it on its next turn (the completion MUST NOT preempt or
  interrupt the turn currently in progress).
- **FR-008**: The completion-notification mechanism MUST correctly report the
  job outcome (success/failure, exit code, and a bounded tail of output) in both
  the user-facing visual notification and the conversation-injected result.
- **FR-009**: The behavior of existing terminal tool invocations (foreground
  command result schema, exit codes, timeout handling, background session
  handles) MUST remain backward-compatible — no breaking change to the tool's
  documented inputs/outputs or result format.
- **FR-010**: The performance improvement MUST NOT degrade the steady-state
  latency or memory footprint of short, fast commands (sub-second commands must
  remain effectively instantaneous to the user).
- **FR-011**: During a running command that produces no output, the agent MUST
  display an elapsed-time indicator (e.g. "running… 12s") that updates
  periodically via the existing render tick, so a silent command is not mistaken
  for a hang.

### Key Entities *(include if feature involves data)*

- **Output Delta**: A bounded chunk of incremental stdout/stderr produced by a
  running command, surfaced to the user as progress. Has an associated session
  and command identity. Deltas are read from the stream, displayed live, and
  simultaneously appended to a temp-file capture for the final result.
- **Terminal Command Execution**: A single invocation of the terminal tool —
  foreground or background — carrying its stream of output deltas (backed by a
  temp-file capture), a final outcome (exit code, full output read back from the
  temp file), and lifecycle (running → completed/interrupted/failed).
- **Background Job**: A terminal command launched to run independently of the
  turn, carrying a session handle, completion state, and an optional
  notification intent (notify-on-complete) that, when set, guarantees an
  automatic completion event on finish.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: During any foreground terminal command lasting more than a few
  seconds, the user sees incremental output within ~1 second of it being
  produced (eliminating the "silent freeze" symptom).
- **SC-002**: For the entire duration of any running terminal command, the
  program's UI continues to update at its normal animation cadence with no
  perceptible freeze or "not responding" state.
- **SC-003**: A user interrupting a running foreground command sees control
  return within 3 seconds, with the child process reliably terminated.
- **SC-004**: Background jobs with completion notification requested deliver a
  completion event automatically on finish in 100% of observed cases (no manual
  polling required).
- **SC-005**: Commands producing large output (tens of MB) complete without
  runaway memory growth — peak memory for output handling stays bounded and
  proportional to a configured cap, not to total output size.
- **SC-006**: Existing fast (sub-second) terminal commands show no perceptible
  latency regression versus current behavior.
- **SC-007**: All existing terminal-tool result formats, exit codes, and
  behaviors continue to work unchanged (no behavioral regressions), verified by
  the full test suite staying green.
- **SC-008**: During a running command producing no output, the user sees an
  elapsed-time indicator that updates at least every few seconds, confirming the
  job is still active (not a frozen screen).

## Assumptions

- The runtime already provides a multi-threaded async executor (confirmed:
  `joey-cli` uses a multi-threaded runtime), so adding concurrency does not
  require a new runtime — only correct use of it.
- "Heavy jobs" primarily means long-running foreground shell commands (builds,
  test suites, installs, batch scripts) and long-running background processes,
  not network-bound LLM streaming (that is a separate concern).
- Bounded-latency streaming targets (~1s) assume the host is not under extreme
  CPU starvation; under severe load, best-effort delivery with eventual
  completeness is acceptable.
- Output displayed incrementally to the user and output delivered in the final
  tool result to the model are backed by the same temp-file capture: deltas are
  surfaced to the user live, and the complete output is read back from the temp
  file for the model's final result. The temp file is cleaned up after readback.
  This guarantees the model's canonical result always contains the complete output
  regardless of size, while in-memory usage stays bounded.
- Existing terminal tool timeout behavior (default 180s, hard max 600s) remains
  in force; this feature changes how output/timing is surfaced, not the timeout
  policy itself.
- Backward compatibility of the terminal tool's public result schema and the
  background-process session-handle contract is a hard constraint (Constitution
  Principle VII); any internal restructuring must preserve these surfaces.
- The "notify_on_complete" capability is already a documented, user-facing
  feature of the process/terminal tools — this work makes it functional, which
  is a bug-fix completion of an existing contract rather than a new surface.
