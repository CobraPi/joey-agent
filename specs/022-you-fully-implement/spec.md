# Feature Specification: HyperCode Agent Teams

**Feature Branch**: `022-you-fully-implement`

**Created**: 2026-09-02

**Status**: Draft

**Input**: User description: "I want you to fully implement a feature in /hypercode called team - I want you to follow the agent teams implementation as described by this document: https://code.claude.com/docs/en/agent-teams#when-to-use-agent-teams --- I want the orchestrator to be able to delegate between both the existing subagent mode and the new agent team mode based on the task provided - I want it to decide and use the optimum solution for the current task. Please follow the spec exactly like the documentation states."

## Clarifications

### Session 2026-09-02

- Q: What permission model should teammates have? → A: The lead assigns each teammate an existing role profile at spawn (read-only investigator or write-capable implementer), reusing the current subagent role toolsets.
- Q: While a team is active, how are new unrelated tasks handled? → A: The orchestrator keeps routing new, unrelated tasks to subagent mode in parallel with the running team (mixed modes concurrently; still at most one team).
- Q: Should the team lead's model be configurable? → A: Yes — the lead runs with a model configurable through configuration; when no lead model is set, the lead inherits the orchestrator's effective model.

## User Scenarios & Testing *(mandatory)*

<!--
  IMPORTANT: User stories should be PRIORITIZED as user journeys ordered by importance.
  Each user story/journey must be INDEPENDENTLY TESTABLE - meaning if you implement just ONE of them,
  you should still have a viable MVP (Minimum Viable Product) that delivers value.

  Assign priorities (P1, P2, P3, etc.) to each user story, where P1 is the most critical.
  Think of each user story as a standalone slice of functionality that can be:
  - Developed independently
  - Tested independently
  - Deployed independently
  - Demonstrated to users independently
-->

### User Story 1 - Orchestrator Chooses the Optimum Execution Mode (Priority: P1)

As a user of the HyperCode orchestrator, when I give it a task, it decides — per task — whether to execute it with the existing single-agent/subagent mode or with the new agent team mode, applies the documented decision guidance, tells me which mode it chose and why in one or two sentences, and completes the task correctly in the chosen mode.

**Why this priority**: Mode selection is the core of the request and the entry point for every other story; without it the team mode is unreachable from normal orchestrator use and no optimum delegation occurs. Shipped alone, it already lets every task run in the cheaper mode with an explicit rationale.

**Independent Test**: Give the orchestrator one clearly self-contained task (expected: subagent/single-agent mode chosen and the task completed) and one clearly parallelizable multi-part task with independent parts (expected: team mode chosen and the task completed). Both outcomes are observable from the orchestrator's stated decision plus the completed work.

**Acceptance Scenarios**:

1. **Given** an orchestrator session and a self-contained task with a single focus area, **When** the task is delegated, **Then** the orchestrator selects single-agent or subagent mode, states that choice and a one-to-two-sentence rationale, and completes the task without creating a team.
2. **Given** an orchestrator session and a large task whose parts are independent and parallelizable (for example: research one area while implementing a module in another), **When** the task is delegated, **Then** the orchestrator selects team mode, states that choice and rationale, and creates the team; end-to-end execution by that team is validated in User Story 2's scenarios.
3. **Given** a task whose parts would all edit the same files or must run in a strict sequence, **When** the routing decision is made, **Then** team mode is NOT selected, because the documented guidance reserves teams for independent, parallelizable work.
4. **Given** a task for which the optimum mode is ambiguous, **When** the orchestrator decides, **Then** it picks the lower-overhead mode and says it did so.
5. **Given** team mode is disabled in configuration, **When** a team-suited task is delegated, **Then** the work still completes via subagents and the user is informed that team mode is disabled.
6. **Given** any task delegated by the orchestrator, **When** the mode decision is made, **Then** the decision and its rationale are recorded as part of the run's report so the user can review them afterwards.

---

### User Story 2 - Lead Coordinates a Team with a Shared Task List and Direct Messaging (Priority: P2)

As a team lead running in the orchestrator's session, I can break the objective into tasks with statuses and dependencies in a shared task list, assign them to named teammates, and exchange messages with teammates through per-teammate inboxes; teammates work independently in their own contexts, claim unblocked work, message me and each other directly, and notify me whenever they go idle or finish.

**Why this priority**: The shared task list and mailbox messaging are the collaboration backbone that makes a team more than parallel subagents; every team behavior in the reference depends on them.

**Independent Test**: Start a team on a multi-part objective and observe, in the shared task list and mailbox records, tasks being created, assigned or claimed, moved to in-progress and completed (dependencies respected), and messages flowing between lead and teammates — culminating in a lead-side synthesis of all completed work.

**Acceptance Scenarios**:

1. **Given** an active team, **When** the lead decomposes the objective, **Then** tasks are created in a shared task list with a status (pending, in-progress, or completed) and optional dependencies on other tasks.
2. **Given** a pending task whose dependencies are all completed, **When** a teammate becomes available, **Then** that teammate can claim the task (first-come-first-served, without two teammates ever holding the same task) and move it to in-progress.
3. **Given** a pending task with an unfinished dependency, **When** teammates look for work, **Then** that task cannot be claimed until its dependency completes.
4. **Given** two teammates and one unassigned unblocked task, **When** both try to claim it, **Then** exactly one claim succeeds.
5. **Given** active teammates, **When** one needs information or a decision owned by another, **Then** it can send a message to that teammate's inbox, and messages are delivered to their recipient without the lead relaying them.
6. **Given** a teammate that finishes its current work, **When** it has no further runnable task, **Then** it notifies the lead and includes its final answer (or error text) so the lead can assign more work or start wind-down.
7. **Given** all tasks completed, **When** the lead synthesizes, **Then** it combines the teammates' results into a single coherent answer for the user.

---

### User Story 3 - Teams Start, Run, and Shut Down as Documented (Priority: P3)

As a user, I can ask for a team in natural language and one is created and started for my objective without further ceremony; while it runs I keep working in my session; and when the work is done the lead shuts the team down — or the session ends and team resources are cleaned up automatically — without leaving stray processes, state, or notifications behind.

**Why this priority**: Lifecycle management turns the collaboration model into something the user can start and forget; it completes the feature but delivers no value without stories 1 and 2.

**Independent Test**: Request a team for a small parallelizable objective from a fresh session, let it finish, confirm the final synthesized result is delivered, confirm every teammate is stopped, and confirm that ending the session leaves no active team processes or unreadable leftover state while the task list remains available for resumption.

**Acceptance Scenarios**:

1. **Given** a session with team mode enabled, **When** the user describes a team-worthy objective in natural language, **Then** the lead creates and starts the team without asking for confirmation.
2. **Given** a running team, **When** teammates work, **Then** the user's session stays interactive (the team coordinates through the lead, teammates, shared task list, and mailboxes).
3. **Given** a running team, **When** the lead asks a teammate to shut down, **Then** the teammate acknowledges and stops, or reports why it cannot yet stop.
4. **Given** the objective is complete, **When** the lead winds the team down, **Then** every teammate is stopped, mailboxes are closed, and the final result is delivered before the team ceases to exist.
5. **Given** a running team, **When** the session ends, **Then** team resources are cleaned up automatically without user intervention.
6. **Given** a team whose session ended, **When** the user resumes and asks about its progress, **Then** the persisted task list shows which tasks were completed and which were not.

---

### User Story 4 - Users Stay Informed and in Control of Team Activity (Priority: P4)

As a user watching a team work, I can see which teammates exist and what they are doing, I am notified when a teammate finishes or needs my input, and I can stop a teammate or the whole team early if I change my mind.

**Why this priority**: Visibility and control polish the feature for real use; a user losing track of a running team is a usability failure but not a functional one.

**Independent Test**: While a team runs, check the visible team status, let one teammate finish (notification arrives), then stop the team early and confirm all teammates halt and the session returns to normal.

**Acceptance Scenarios**:

1. **Given** a running team, **When** the user views team status, **Then** the teammates, their names, and the task list with current statuses are visible.
2. **Given** a running team, **When** a teammate goes idle or completes its work, **Then** the user is notified, and a teammate needing the user's input can reach the user through the lead.
3. **Given** a running team, **When** the user requests a specific teammate to stop, **Then** only that teammate stops and the rest of the team continues.
4. **Given** a running team, **When** the user requests the whole team to stop, **Then** all teammates stop promptly and the session returns to its pre-team state.

---

### Edge Cases

<!--
  ACTION REQUIRED: The content in this section represents placeholders.
  Fill them out with the right edge cases.
-->

- What happens when a task looks parallelizable but its parts would edit the same files? The routing decision must favor single-session or subagent execution, because concurrent same-file edits overwrite each other.
- What happens when a teammate fails mid-task? The lead must be notified with the error, mark the affected task appropriately (for example returning it to pending), and re-plan — reassigning it or splitting it — without losing the rest of the team's progress.
- What happens when all remaining tasks are blocked? The lead must detect the deadlock, inform the user, and either re-plan the dependencies or wind the team down cleanly.
- What happens when a user asks for a second team while one is active? The request must be refused with a clear explanation (one team per session, no nested teams), and subagent mode offered for the new work.
- What happens when the session ends abruptly while a team is running? Team resources must still be cleaned up automatically on exit, while the task list persists for a later resumed session within the configured retention window.
- What happens when team mode is disabled? FR-013 applies unchanged: all work continues through the existing subagent mode exactly as before, with no errors or behavioral regressions.
- What happens when the orchestrator is genuinely uncertain which mode fits? It must choose the lower-overhead mode (subagents) and state that reasoning, since teams cost significantly more.

## Requirements *(mandatory)*

<!--
  ACTION REQUIRED: The content in this section represents placeholders.
  Fill them out with the right functional requirements.
-->

### Functional Requirements

- **FR-001**: The system MUST provide an agent team mode in which one lead coordinates multiple teammates on a shared objective, with each teammate working independently in its own context window.
- **FR-002**: Teammates MUST be able to communicate directly with the lead AND with each other through per-teammate message inboxes; message delivery MUST NOT require the lead to relay teammate-to-teammate messages.
- **FR-003**: The system MUST maintain one shared task list per team containing tasks with a lifecycle of pending, in-progress, and completed, plus optional dependencies between tasks.
- **FR-004**: Task claiming MUST be safe under concurrency: a task can be held by at most one teammate at a time, and an unclaimed task whose dependencies have not completed MUST NOT be claimable.
- **FR-005**: A teammate that completes its current task MUST claim the next unassigned, unblocked task if one exists; otherwise it MUST notify the lead and include its final answer or error text.
- **FR-006**: The lead MUST be notified when a teammate finishes work, goes idle, or encounters an error, without polling by the user.
- **FR-007**: The orchestrator MUST decide, per delegated task, whether to use the existing subagent mode or the team mode, following the documented guidance: teams for independent, parallelizable work (for example research plus implementation across separate areas, or debugging with competing hypotheses); subagents or a single session for sequential work, same-file edits, or heavily interdependent steps.
- **FR-008**: The orchestrator MUST prefer the lower-overhead subagent mode when a task does not clearly warrant a team, and MUST always report the chosen mode and a brief rationale to the user.
- **FR-009**: The system MUST start a team from a natural-language description of the objective, create the shared task list, and spawn teammates without asking the user for confirmation.
- **FR-010**: A session MUST support at most one active team, and teams MUST NOT spawn nested teams; teammates MUST NOT create background subagents of their own.
- **FR-011**: The lead MUST be a fixed member for the team's lifetime, and teammates MUST be stoppable individually or together on request, each stopping gracefully (finishing or abandoning the current step and releasing claimed tasks).
- **FR-012**: When the session ends, the system MUST clean up team resources automatically; the task list MUST persist so a resumed session can reconstruct progress.
- **FR-013**: Team mode MUST be disabled by default and enable through configuration; when disabled, all delegation MUST behave exactly as the existing subagent mode does today.
- **FR-014**: The system MUST surface team activity to the user: teammates and task statuses on demand, and notifications when teammates finish or need input.
- **FR-015**: A teammate stopping MUST release its claimed tasks so those tasks become claimable by remaining teammates or recoverable by the lead.
- **FR-016**: The mode-selection decision MUST be recorded with each run's report so it can be reviewed after completion (the recorded counterpart of the live statement required by FR-008).
- **FR-017**: The lead MUST assign each teammate a role profile at spawn — reusing the existing subagent role toolsets (read-only investigator or write-capable implementer) — and teammates MUST NOT inherit the lead's full toolset or permissions.
- **FR-018**: While a team is active, the orchestrator MUST continue routing new, unrelated tasks to the existing subagent mode in parallel with the running team; the one-team-per-session limit MUST NOT cause new work to queue or block behind the team.
- **FR-019**: The team lead MUST run with a model configurable through configuration; when no lead model is set, the lead MUST inherit the orchestrator's effective model.

### Key Entities *(include if feature involves data)*

- **Team**: A named collaboration unit for one session and one objective, consisting of a lead, teammates, a shared task list, and per-teammate mailboxes. At most one team may be active per session; teams may not nest.
- **Team Lead**: The member that receives the user's objective, decomposes it into tasks, assigns work or lets teammates self-claim, tracks progress, synthesizes results, and initiates wind-down. Fixed for the team's lifetime, and runs with a configurable model that defaults to the orchestrator's effective model.
- **Teammate**: A named worker with its own context window that executes tasks, exchanges messages with the lead and other teammates, notifies the lead when idle or finished, and runs under a role profile (read-only investigator or write-capable implementer) assigned by the lead at spawn.
- **Task**: A unit of work with a status (pending, in-progress, completed) and optional dependencies on other tasks. Exactly one teammate holds a task at a time.
- **Mailbox / Team Message**: A per-teammate inbox holding messages (questions, results, assignments, notifications) that the system delivers automatically to their recipient.
- **Mode-Selection Decision**: The orchestrator's per-task choice of subagent mode or team mode, together with its rationale, recorded with the run's report.

## Success Criteria *(mandatory)*

<!--
  ACTION REQUIRED: Define measurable success criteria.
  These must be technology-agnostic and measurable.
-->

### Measurable Outcomes

- **SC-001**: For a fixed evaluation set of 10 canonical tasks (5 team-suited, 5 subagent-suited), the orchestrator selects the documented-appropriate mode for at least 9 of the 10, and every selection includes a mode statement plus rationale.
- **SC-002**: A team executing a parallelizable objective with at least 3 independent tasks completes all of them and delivers one synthesized result, with every task passing through pending → in-progress → completed exactly once and dependency ordering never violated in 10 consecutive runs.
- **SC-003**: The user receives a notification within 30 seconds of any teammate going idle, finishing, or erroring, 100% of the time.
- **SC-004**: Ending a session with a running team leaves zero active team processes or orphaned resources in 100% of tested cases, and the task list remains readable for resumption afterwards.
- **SC-005**: With team mode disabled, 100% of existing delegation scenarios behave identically to the current subagent mode (no regressions across the existing delegation test suite).
- **SC-006**: On objectives with 4 or more independent parts, completing the work with a team of 3-5 teammates finishes in at most 90% of the wall-clock time of completing the same work sequentially through one session.

## Assumptions

<!--
  ACTION REQUIRED: The content in this section represents placeholders.
  Fill them out with the right assumptions based on reasonable defaults
  chosen when the feature description did not specify certain details.
-->

- Existing subagent delegation behavior, configuration keys, and reports remain unchanged; the team feature is strictly additive, and disabled-by-default rollout guarantees non-regression (per the project constitution's backward-compatibility principle).
- The reference document's v2.1.178 behavior is the target; capabilities it explicitly removed before that release (declarative team-creation tools) and platform-specific terminal multiplexer integrations are out of scope.
- Reasonable defaults from the reference apply where the user did not specify: the lead is fixed for the team's lifetime; there is no hard cap on teammates but roughly 3-5 with 5-6 tasks each is the expected operating range; one team per session; no nested teams; headless one-shot invocations do not start teams.
- The mailbox, shared task list, and team configuration concepts already prototyped in the codebase are the starting point; persisting task lists (and their retention window) uses a project-appropriate on-disk location decided at planning time.
- Teammates do NOT share the lead's conversation history — each starts fresh from its spawn instructions plus whatever the team's shared artifacts give it.
- The orchestrator's mode choice is a decision made with its normal reasoning (not a separate user-facing configuration), and it can mix modes within one session as tasks require.
- Teammates may run in the background relative to the user's session, reusing the existing background-delegation and child-supervision machinery where possible.
- UI for team activity reuses the existing session's surfaces; no separate graphical interface is required.
