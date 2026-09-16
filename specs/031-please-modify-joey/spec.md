# Feature Specification: Goal-Directed Task Execution

**Feature Branch**: `031-please-modify-joey`

**Created**: 2026-09-15

**Status**: Draft

**Input**: User description: "please modify joey-agent so that it's optimized to complete a task in the shortest amount of time - right now the agent wanders around a task. I want it to come up with a concrete plan and execute the steps without deviating. Modify the prompts to be less open-ended and more goal-oriented."

## Clarifications

### Session 2026-09-15

- Q: Where does the Task Plan live during a task, and what happens to it after the task ends? → A: Session-transient visible text — the plan lives in the conversation, re-stated at step transitions so it survives context compression; no session-persisted record or on-disk artifact is created.
- Q: Is there a size budget for the new goal-directed guidance? → A: Token-neutral — the new guidance must fit within the token footprint of the guidance text it replaces or removes (including predecessor-brand cleanup savings), verified by comparing assembled model-facing instruction size before and after the change.
- Q: How is SC-006's "sampled" verification performed? → A: Exhaustive automated test — a regression test enumerates every model-facing instruction/guidance text the system assembles and asserts zero predecessor-brand references; no manual audit.
- Q: How are the baseline and the representative task set for SC-001/SC-003/SC-004 defined? → A: Committed baseline bundle — 5-10 representative multi-step tasks defined as feature artifacts, with current-version transcripts captured once and stored alongside the feature for before/after comparison.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Plan Before Action (Priority: P1)

As a user giving the agent a non-trivial task, I want the agent to formulate and show me a concrete, step-by-step plan before it starts working, so I can immediately see the route to completion instead of watching it explore.

**Why this priority**: This is the core of the requested change — every other benefit depends on the agent first committing to a concrete path. Today the agent begins acting without an articulated plan, which is the root cause of wandering.

**Independent Test**: Give the agent a multi-step task (for example, "add configuration validation with tests" in a sample project) and inspect the session transcript: the agent's first substantive response must contain a plan of discrete, ordered steps before any task work begins. Delivers value on its own: predictable, reviewable task starts.

**Acceptance Scenarios**:

1. **Given** a non-trivial task request, **When** the agent begins the task, **Then** it states a plan consisting of discrete, ordered, individually verifiable steps before performing any task work.
2. **Given** a trivial single-step task (for example, "rename this variable"), **When** the agent begins the task, **Then** it skips plan ceremony and completes the task directly — the planning requirement is proportionate to task size.
3. **Given** an ambiguous task with no reasonable default interpretation, **When** the agent begins the task, **Then** it asks at most one round of targeted clarification questions and then plans — it does not explore in place of asking.

---

### User Story 2 - Execution Without Deviation (Priority: P2)

As a user whose task is underway, I want the agent to execute the agreed steps in order without drifting into unrequested exploration, so the task completes in the shortest amount of time.

**Why this priority**: Directly delivers the time-to-completion goal; depends on Story 1's plan existing as the reference for what "on track" means.

**Independent Test**: Give the agent a planned task and audit the session transcript: each action the agent takes maps to the current plan step; actions unrelated to any step (browsing unrelated files, re-deriving already-answered questions) are absent or explicitly justified. Delivers value on its own: visibly shorter, straighter task execution.

**Acceptance Scenarios**:

1. **Given** an active plan, **When** the agent works, **Then** each action it takes serves the current step, and it advances through steps in order.
2. **Given** work already completed in the session, **When** the agent needs that information again, **Then** it reuses the earlier result instead of re-deriving it.
3. **Given** a completed task, **When** the agent reports to the user, **Then** it maps each plan step to its outcome and states the verification performed — no silent partial completion.

---

### User Story 3 - Explicit, Bounded Plan Revision (Priority: P3)

As a user, when reality invalidates the plan (a step fails, a fact turns out wrong), I want the agent to stop, state what changed, and present a revised plan before continuing — so deviation is visible and controlled rather than wandering.

**Why this priority**: Guards Stories 1 and 2 against the failure mode of blind adherence to a dead plan; without it the agent either wanders (current behavior) or stubbornly follows a broken plan.

**Independent Test**: Give the agent a task in which one step is impossible (for example, a required dependency that does not exist) and observe the session transcript: the agent surfaces the blocker with evidence, states the impact on the plan, and either presents a revised plan or stops and reports the blocker — it does not silently retry or explore around it. Delivers value on its own: trustworthy failure handling.

**Acceptance Scenarios**:

1. **Given** a step that fails, **When** failure is confirmed, **Then** the agent reports the blocker with evidence, states the plan impact, and presents a revised plan or a stopping recommendation instead of continuing to act.
2. **Given** a blocked task that cannot proceed, **When** the agent stops, **Then** the user receives a clear statement of what was completed, what is blocked, and why.

---

### User Story 4 - Consistent Agent Identity (Priority: P4)

As a user, I want all text the agent shows me — or includes in its own instructions to the model — to present the agent under its own product identity, so the experience is coherent and free of leftover references to the predecessor project.

**Why this priority**: Explicitly requested alongside the core goal, but it is presentation hygiene rather than the mechanism that reduces task time; it rides along without blocking Stories 1-3.

**Independent Test**: Start a session and inspect every piece of guidance, help, hint, and status text the agent emits to the user or embeds in its model instructions: all of it refers to the agent by its own name and none references the predecessor project (Hermes Agent). Delivers value on its own: a coherent, consistently branded experience.

**Acceptance Scenarios**:

1. **Given** a newly started session, **When** the agent's instruction text is assembled, **Then** every identity reference uses the agent's own product name and none references the predecessor project's name.
2. **Given** help, error, or guidance text shown to the user during a session, **When** it is displayed, **Then** it is branded consistently with the agent's own identity and contains no predecessor-project references.

### Edge Cases

- What happens when the task is trivial (a single obvious action)? — The agent skips plan ceremony and acts; proportionality prevents new overhead (User Story 1, Scenario 2).
- How does the system handle mid-task discovery that invalidates the plan? — Explicit revision with a stated reason; never silent drift (User Story 3).
- How does the system handle long tasks where earlier conversation is compacted or summarized? — The agent keeps the plan (goal, steps, step statuses) present and authoritative throughout the task; execution never loses sight of the plan.
- How does the system handle user instructions that conflict with the plan mid-task? — The user's new instruction wins; the agent incorporates it as an explicit plan revision.
- How does the system handle a task whose scope grows during execution? — The agent re-plans explicitly before taking on the enlarged scope.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Before starting work on a non-trivial task, the agent MUST formulate a concrete plan of discrete, ordered, individually verifiable steps and state it to the user.
- **FR-002**: Plans MUST be proportionate to task size: single-step tasks MUST be executed directly without plan ceremony.
- **FR-003**: During execution, the agent MUST work the plan steps in order, and every action it takes MUST serve the current step; unrequested exploration MUST NOT occur.
- **FR-004**: The agent MUST reuse results it has already established in the session instead of re-deriving them.
- **FR-005**: Departures from the stated plan MUST be explicit: the agent MUST state what changed and why, and present the revised plan, before continuing on the changed path.
- **FR-006**: When a step is blocked or fails, the agent MUST surface the blocker with evidence and either present a revised plan or stop with a clear status report; it MUST NOT keep retrying or exploring around the blocker.
- **FR-007**: On completion, the agent MUST report the outcome of every plan step and the verification performed, and MUST NOT present a task as complete when steps are unverified or incomplete.
- **FR-008**: The agent's behavioral guidance MUST frame every task in terms of the goal, the concrete steps, and the definition of done — replacing open-ended exploration framing with goal-oriented framing.
- **FR-009**: Ambiguous task requests MUST trigger at most one round of targeted clarification before planning when no reasonable default interpretation exists; when a reasonable default exists, the agent MUST proceed on it and record the assumption.
- **FR-010**: The changes MUST NOT remove or break existing user-facing capabilities (tools, skills, session management, modes); behavior changes are confined to how the agent approaches and executes tasks.
- **FR-011**: All model-facing instruction text and user-facing guidance text MUST present the agent exclusively under its own product identity; references to the predecessor project or its branding (for example, "Hermes") MUST NOT appear in any text shown to the user or sent to the model.

### Key Entities

- **Task Plan**: the agent's commitment for one task — the goal, the ordered steps, and a definition of done; created before work begins and visible to the user; session-transient (it lives in the conversation, not in session-persisted or on-disk state) and re-stated at step transitions so it survives context compression.
- **Plan Step**: one discrete, verifiable unit of a Task Plan; carries a status (pending / in progress / done / blocked) and its verification outcome when done.
- **Plan Revision**: an explicit change to a Task Plan — what changed, why, and the resulting plan; the only sanctioned form of deviation.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a fixed set of representative multi-step tasks, the agent completes each task with at least 30% fewer visible actions in the session transcript than the current version does on the same tasks.
- **SC-002**: 100% of non-trivial task sessions show a stated plan before the first task action.
- **SC-003**: On audited sample sessions, at least 90% of the agent's actions map to a declared plan step.
- **SC-004**: Task success rate on the representative set is at least as high as the current version's baseline (no regression in exchange for speed).
- **SC-005**: Every completed task session ends with a step-by-step completion report; every blocked session ends with an explicit blocker statement.
- **SC-006**: 100% of assembled model-facing instruction text and user-facing guidance contains zero references to the predecessor project's branding (for example, "Hermes"), verified by an exhaustive automated test that enumerates every such text and asserts zero matches.
- **SC-007**: The assembled model-facing instruction text after the change is no larger (in tokens) than before the change — the added goal-directed guidance fits within the footprint of the guidance it replaces or removes.

## Assumptions

- The change is delivered primarily through revised agent behavioral guidance and, where needed, lightweight supporting behavior; no new user-facing tools are required for v1.
- Scope covers the primary interactive agent's task execution; orchestration personas (the delegation-based orchestration flows) and scheduled-job prompts are out of scope for v1, though they may adopt the same framing later.
- Existing guidance that does not conflict with goal-directed execution remains in effect; the feature changes framing, not capability.
- Baseline measurements for SC-001, SC-003, and SC-004 come from a committed baseline bundle: 5-10 representative multi-step tasks defined as feature artifacts, with current-version transcripts captured once and stored alongside the feature; no formal benchmark harness is required for v1.
- A "visible action" (SC-001's counting unit) is one tool invocation or one assistant message recorded in the session transcript; the baseline rubric counts both and sums them.
- Baseline transcripts are captured by running each manifest task as a fresh one-shot CLI session and exporting the stored session to Markdown, one file per task; the same mechanism is used for post-change capture.
- "Non-trivial task" is judged by the agent using a proportionality rule (more than one meaningful step, or more than one file or artifact affected).
- The identity cleanup covers text shown to the user or sent to the model (guidance, help, hints, status text); internal engineering artifacts that document upstream lineage (the porting/parity audit and source comments citing upstream files) deliberately retain their references, as they are audit records rather than prompts.
- Removing predecessor-brand references from guidance originally ported verbatim from the predecessor is a deliberate, documented divergence from verbatim parity; the divergence MUST be recorded in the project's porting/parity audit during implementation.
