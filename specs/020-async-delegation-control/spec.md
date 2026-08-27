# Feature Specification: Async Delegation & Subagent Control

**Feature Branch**: `020-async-delegation-control`

**Created**: 2026-08-26

**Status**: Draft

**Input**: User description: "please turn this whole plan into specs" — this feature converts Phases 1–2 of the approved HyperCode improvement plan: keeping the orchestrator productive while subagents run, and giving the orchestrator direct authority over running subagents.

## Clarifications

### Session 2026-08-26

- Q: Does per-subagent control belong to the orchestrator only, or should the operator also get per-subagent stop and steer? → A: Full operator parity — the operator also gets per-subagent stop and steer from the live interface.
- Q: How should the orchestrator's own work be prioritized against its running background subagents when shared capacity is saturated? → A: Reserved share — the orchestrator keeps a guaranteed minimum of capacity; children share the rest; the reservation releases when the orchestrator is idle.
- Q: When background work completes while the orchestrator is idle, should the orchestrator be woken to act on it autonomously? → A: Proactive wake — pending notices start a new orchestrator turn autonomously when idle; mid-turn, delivery still waits for turn boundaries.
- Q: How long should completed background work remain visible in the delegation overview? → A: Session-lifetime — records persist for the rest of the orchestrator session, then are discarded; durable copies only via existing opt-in session persistence.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Background Delegation (Priority: P1)

As an orchestrator agent, I want to start subagent work in the background and receive a work handle immediately, so that I can keep doing useful work (research, file inspection, planning, further delegation) while subagents run instead of blocking idle until they finish.

**Why this priority**: The orchestrator currently waits idle for the full duration of every delegation. Removing that idle time is the core value of this feature, and every other story builds on the handle it returns.

**Independent Test**: Start one background subagent task designed to take at least 30 seconds; immediately perform an unrelated action (a file read or a second delegation) and confirm it completes before the subagent finishes.

**Acceptance Scenarios**:

1. **Given** the orchestrator requests background delegation, **When** the request is accepted, **Then** a handle containing a unique identifier and the task goal is returned within 2 seconds, long before subagent completion.
2. **Given** a background subagent is running, **When** the orchestrator performs other work, **Then** that work completes without waiting for the subagent.
3. **Given** background requests exceed the configured concurrency limit, **When** they are started, **Then** excess work queues under the same limits that govern blocking delegation today rather than being rejected.

---

### User Story 2 - Completion Notifications (Priority: P1)

As an orchestrator agent, I want to be notified when background subagent work completes — successfully or not — so that I can incorporate results (or react to failures) at my next decision point without polling.

**Why this priority**: Background delegation is only useful if results reliably come back; without notifications the orchestrator would have to poll or silently lose work.

**Independent Test**: Start a background subagent that finishes in roughly 30 seconds; continue with other work; confirm a completion notice (identifier, goal, outcome, summary, resource usage) is delivered by the orchestrator's next interaction after completion.

**Acceptance Scenarios**:

1. **Given** a background subagent completes successfully, **When** the orchestrator is next active, **Then** it receives a completion notice containing identifier, goal, outcome, distilled summary, and resource usage.
2. **Given** a background subagent fails, **When** the orchestrator is next active, **Then** it receives a failure notice with the reason; failures are never silently dropped.
3. **Given** multiple background subagents complete while the orchestrator is mid-turn, **When** the turn reaches its next boundary, **Then** all pending notices are delivered together without corrupting in-flight work.
4. **Given** the orchestrator is idle (no turn in progress), **When** a background subagent completes, **Then** a new orchestrator turn starts autonomously to process the completion notice without waiting for user input.

---

### User Story 3 - Steer & Stop (Priority: P1)

As an orchestrator agent, I want to redirect (steer) or stop an individual running subagent, so that off-track work is corrected early or ended cheaply instead of running to completion wasting time and resources.

**Why this priority**: Course-correcting a running subagent is the highest-leverage authority improvement: today a mis-aimed subagent must run its full course before the orchestrator can react at all.

**Independent Test**: Start a long-running subagent with the wrong framing; steer it with corrected instructions and confirm its subsequent behavior reflects the correction. In a separate run, stop it mid-run and confirm a partial result is returned.

**Acceptance Scenarios**:

1. **Given** a running background subagent, **When** the orchestrator sends it a steering message, **Then** the message is delivered to the subagent before its next action and the delivery is acknowledged to the orchestrator.
2. **Given** a steering message targets a subagent that already finished, **When** the orchestrator sends it, **Then** the system responds gracefully ("already finished") without error.
3. **Given** multiple background subagents are running, **When** the orchestrator stops one, **Then** only that subagent stops; the others are unaffected.
4. **Given** a stopped subagent, **When** its stop is recorded, **Then** a partial result including the stop reason is available to the orchestrator.

---

### User Story 4 - Progress Inspection (Priority: P2)

As an orchestrator agent, I want to inspect the progress and recent activity of running subagents on demand, so that I can decide whether to keep waiting, steer, or stop — based on evidence rather than guesses.

**Why this priority**: Inspection turns control (Story 3) into an informed feedback loop; it is essential but secondary to being able to act at all.

**Independent Test**: While two subagents run, list all background work (identifier, goal, elapsed time, resource usage, live status) and retrieve the last few activity items of one subagent; confirm both answers arrive promptly.

**Acceptance Scenarios**:

1. **Given** one or more background subagents running, **When** the orchestrator lists them, **Then** it receives identifier, goal, elapsed time, resource usage, and status for each.
2. **Given** a running subagent, **When** the orchestrator requests its recent activity, **Then** it receives the most recent bounded slice of activity, not the full transcript.
3. **Given** the orchestrator has nothing else to do, **When** it explicitly waits for a specific subagent, **Then** it blocks for that subagent only and receives the result.

---

### User Story 5 - Resource Budgets (Priority: P2)

As an operator, I want per-subagent resource budgets (turns, tokens, wall-clock time) enforced automatically, so that a runaway subagent cannot consume unbounded resources.

**Why this priority**: Budget enforcement protects cost and time once delegation becomes long-running and background; it matters most once Stories 1–3 exist.

**Independent Test**: Start a subagent with a deliberately small budget; confirm it is stopped when the budget is exceeded, a breach report is produced, and sibling subagents continue unaffected.

**Acceptance Scenarios**:

1. **Given** a delegation request includes budgets, **When** any budget is exceeded, **Then** the subagent is stopped after at most one additional action and a budget-exceeded outcome is reported.
2. **Given** invalid budget values (zero, negative, or malformed), **When** the request is made, **Then** it is rejected immediately with a clear reason.
3. **Given** ongoing delegation activity, **When** the orchestrator inspects its delegation overview, **Then** cumulative resource usage across all its subagents is visible.

---

### User Story 6 - Operator Subagent Control (Priority: P2)

As an operator, I want to stop or steer an individual running subagent from the live interface, so that I can intervene on a single off-track subagent without aborting my whole session.

**Why this priority**: It extends the orchestrator's per-subagent controls (Story 3) to the operator once those controls exist; valuable for supervision but dependent on Story 3's machinery.

**Independent Test**: While at least two subagents run, from the live interface stop one and steer another; confirm only the targeted subagent stops, the steered one changes course, and all others continue.

**Acceptance Scenarios**:

1. **Given** a running background subagent, **When** the operator selects it in the live interface and stops it, **Then** only that subagent stops and its partial result includes the operator-requested stop reason.
2. **Given** a running background subagent, **When** the operator sends it a steering message from the live interface, **Then** the message is delivered before the subagent's next action and the delivery is visible to the orchestrator.
3. **Given** a subagent that already finished, **When** the operator attempts to stop or steer it, **Then** the action responds gracefully ("already finished") without error.

---

### Edge Cases

- Steering or stopping a subagent that has just completed must return a clear "already finished" response, never an error.
- Completion notices arriving while the orchestrator is mid-turn must wait for a turn boundary; they must never interleave with or truncate in-flight output.
- A wave of subagents that all fail must still produce one failure notice each.
- Unknown or reused identifiers in any control request must produce a clear error.
- Background work still running when the orchestrator session ends must be wound down gracefully and its final status recorded; nothing keeps running invisibly after session end.
- Starting background work and then immediately waiting on it must behave identically to today's blocking delegation.
- Budgets left unspecified must preserve today's behavior (existing default turn limits only).
- Operator and orchestrator control actions on the same subagent must be honored in the order received; after a stop, further steering messages for that subagent return "already finished".

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The delegation mechanism MUST support a background mode that accepts work and returns a work handle (unique identifier + goal) immediately, without waiting for completion.
- **FR-002**: The default behavior of delegation MUST remain blocking and unchanged; background mode is opt-in per request.
- **FR-003**: On background subagent completion (success or failure), the system SHALL deliver a completion notice to the orchestrator containing identifier, goal, outcome, distilled summary, and resource usage; delivery occurs at the next turn boundary if the orchestrator is mid-turn, and SHALL start a new orchestrator turn autonomously if the orchestrator is idle.
- **FR-004**: Completion notices MUST be distilled summaries; raw subagent activity transcripts MUST NOT be pushed into orchestrator context (transcripts remain available through existing session-persistence and search capabilities).
- **FR-005**: The orchestrator MUST be able to list running background subagents with identifier, goal, elapsed time, status, and resource usage.
- **FR-006**: The orchestrator MUST be able to retrieve a bounded, most-recent slice of a specific subagent's activity on demand.
- **FR-007**: The orchestrator MUST be able to explicitly wait for one or more specific background subagents and receive their results.
- **FR-008**: The orchestrator MUST be able to send a steering message to a specific running subagent, delivered before the subagent's next action.
- **FR-009**: The orchestrator MUST be able to stop a specific running subagent; stopping one subagent MUST NOT affect any other running subagent.
- **FR-010**: A stopped subagent MUST yield a partial result that includes the stop reason (orchestrator-requested, operator-requested, budget-exceeded, or session-end).
- **FR-011**: Delegation requests MUST accept optional per-subagent budgets on turns, token usage, and wall-clock time; exceeding any budget stops the subagent and reports a budget-exceeded outcome.
- **FR-012**: The orchestrator MUST be able to view cumulative resource usage of its delegation activity (running and completed).
- **FR-013**: Background delegation MUST operate under the same configured concurrency limits as blocking delegation; it MUST NOT bypass them.
- **FR-014**: Background subagent activity MUST remain visible in the operator's live interface, exactly as blocking subagent activity is today.
- **FR-015**: When the orchestrator session ends while background subagents run, the system MUST wind them down gracefully and record their final status.
- **FR-016**: All background work states (running, completed, failed, stopped, budget-exceeded) MUST be distinguishable in notices and listings.
- **FR-017**: The operator SHALL be able to stop, and to send a steering message to, a specific running background subagent through the live interface; these actions SHALL target only the selected subagent and SHALL be reflected in its recorded stop reason or activity.
- **FR-018**: When the orchestrator and its running subagents share concurrency capacity, a guaranteed minimum of capacity SHALL be reserved for the orchestrator's own actions; running subagents SHALL share the remaining capacity, and the reservation SHALL be released while the orchestrator is idle.
- **FR-019**: Records of completed background work SHALL remain listed in the delegation overview for the remainder of the orchestrator session and SHALL be discarded at session end; durable copies SHALL exist only through existing opt-in session persistence.

### Key Entities *(include if feature involves data)*

- **Work Handle**: unique identifier, goal, and start time, returned when background work is accepted.
- **Completion Notice**: identifier, goal, outcome, distilled summary, resource usage.
- **Steering Message**: target identifier plus instruction text, delivered at the subagent's next action boundary.
- **Budget**: optional per-subagent caps: maximum turns, maximum token usage, maximum wall-clock time.
- **Stop Reason**: orchestrator-requested, operator-requested, budget-exceeded, session-end.
- **Delegation Overview**: live listing of background work with status, elapsed time, and cumulative resource usage (records retained for the session lifetime only).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Starting background delegation returns control to the orchestrator within 2 seconds, regardless of how long the subagent runs.
- **SC-002**: 100% of background subagents that finish (success, failure, or stop) produce a corresponding notice or partial result visible to the orchestrator at its next interaction.
- **SC-003**: Stopping one running subagent never prevents any other running subagent from completing normally.
- **SC-004**: A subagent exceeding its budget performs no further actions after breach detection, and the breach is reported in its result.
- **SC-005**: All existing delegation behaviors (single, batch, blocking) continue to pass their existing acceptance checks unchanged.
- **SC-006**: Orchestrator context consumed by background features (handles plus notices) grows proportionally to the number of subagents — not to the volume of subagent activity.
- **SC-007**: While all delegation capacity is consumed by running subagents, the orchestrator remains able to complete inspection and control actions within 5 seconds.

## Assumptions

- Background mode is opt-in; existing blocking delegation remains the default and unchanged (per the project's additive-compatibility principle).
- Steering operates at the next-action boundary of a subagent — mid-action interruption is out of scope.
- Distilled notices are a deliberate token-economy decision: subagents act as compressors; full transcripts stay in persisted sessions.
- Existing configured concurrency limits continue to govern background work; background mode adds no extra capacity.
- Budgets left unspecified fall back to existing default turn limits (current behavior).
- **Out of scope (candidates for future specs)**: resuming or branching persisted subagent sessions; priority scheduling and preemption of queued work; team/shared task boards; multi-level delegation depth changes; speculative dual-dispatch as a built-in capability.
